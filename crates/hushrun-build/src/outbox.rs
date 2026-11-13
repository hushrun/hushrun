//! A block's upload on a machine (B-29), and the folders a run of the relay owns (B-26, B-30).
//!
//! An upload's state is where its marker is, and nothing else:
//!
//! | State | On disk |
//! |---|---|
//! | waiting | `outbox/<key hex>.json` |
//! | claimed | `outbox/claimed/<run>/<key hex>.json` |
//! | sent | the claimed marker, while the relay that claimed it lives: its pack was put |
//! | done | no marker |
//!
//! "Sent" is known only to the relay that holds the claim: if that relay dies, its claims go back
//! to waiting, and the next try's `begin` finds the upload under way (the same pack, since a pack
//! is the same bytes every time) and its PUT finds the object there. Every move is a rename or a
//! file written whole, so two relays on one store never both hold one marker: a claim is a rename
//! out of `outbox/`, and only one rename of a file succeeds.
//!
//! A run's folders, each removed when its run ends and swept when its run is dead: `relays/<run>/`
//! (its being there, with the run's process alive, is what makes the run alive),
//! `incoming/<run>/` (downloads waiting for a compile that brings their key),
//! `outbox/tmp/<run>/` (packs larger than memory holds) and `outbox/claimed/<run>/`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

use crate::model::Class;
use crate::origin::Identity;
use crate::store::{DAY, Store, Tally};

/// The outbox's folder in the store.
pub const OUTBOX: &str = "outbox";
/// Claims, one folder per run.
pub const CLAIMED: &str = "claimed";
/// Packs being written, one folder per run.
pub const PACKS: &str = "tmp";
/// Downloads, one folder per run.
pub const INCOMING: &str = "incoming";
/// One folder per live run of the relay.
pub const RELAYS: &str = "relays";
/// The schema name of every marker.
pub const MARKER_SCHEMA: &str = "hushrun.build-outbox/v1";
/// A marker the end of a command cut off this many times is dropped: the block doesn't fit this
/// machine's runs.
pub const MOST_CUTOFFS: u32 = 3;

/// A marker: a block built here that waits to go up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    /// [`MARKER_SCHEMA`].
    pub schema: String,
    /// The block's key.
    pub key: Digest,
    /// Its first key, whose address hints and suspensions go by.
    pub step: Digest,
    /// What it is.
    pub class: Class,
    /// What its compile took here, in milliseconds.
    pub cost_ms: u64,
    /// Who built it: a marker goes up only from a run with the same host, org and credential.
    pub by: Identity,
    /// The repository the run that built it was in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// When it was made, in milliseconds since the epoch.
    pub created_ms: u64,
    /// Not tried again before this, in milliseconds since the epoch.
    #[serde(default)]
    pub not_before_ms: u64,
    /// How many times the end of a command cut its upload off.
    #[serde(default)]
    pub cutoffs: u32,
}

impl Marker {
    /// A new marker for a block built at `now`.
    pub fn new(
        key: Digest,
        step: Digest,
        class: Class,
        cost_ms: u64,
        by: Identity,
        repository: Option<String>,
        now: SystemTime,
    ) -> Marker {
        Marker {
            schema: MARKER_SCHEMA.to_string(),
            key,
            step,
            class,
            cost_ms,
            by,
            repository,
            created_ms: epoch_ms(now),
            not_before_ms: 0,
            cutoffs: 0,
        }
    }
}

/// What happens to a claimed marker that didn't reach done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Back {
    /// Waiting again, not tried before this many milliseconds since the epoch: a network error,
    /// a server's failure, a refusal that may pass, a word this version doesn't know.
    Later(u64),
    /// Waiting again with one more cut-off: the command ended while its PUT was in flight. The
    /// third drops it.
    CutOff,
    /// Waiting again as it was: the command ended before its turn came.
    Untouched,
}

/// The outbox of a store.
#[derive(Debug, Clone)]
pub struct Outbox {
    store: Store,
}

impl Outbox {
    /// The outbox of `store`.
    pub fn new(store: &Store) -> Outbox {
        Outbox {
            store: store.clone(),
        }
    }

    fn dir(&self) -> PathBuf {
        self.store.root().join(OUTBOX)
    }

    fn waiting_path(&self, key: &Digest) -> PathBuf {
        self.dir().join(format!("{}.json", key.hex()))
    }

    fn claimed_path(&self, run: &str, key: &Digest) -> PathBuf {
        self.dir()
            .join(CLAIMED)
            .join(run)
            .join(format!("{}.json", key.hex()))
    }

    /// none → waiting: the marker is written whole. Not when a relay holds a claim of the same
    /// block: it is on its way. Whether it was written.
    pub fn add(&self, marker: &Marker) -> io::Result<bool> {
        let name = format!("{}.json", marker.key.hex());
        for run in subfolders(&self.dir().join(CLAIMED)) {
            if run.join(&name).exists() {
                return Ok(false);
            }
        }
        let bytes = serde_json::to_vec(marker).map_err(io::Error::other)?;
        self.store
            .write_whole(&self.waiting_path(&marker.key), &bytes, None, true)?;
        Ok(true)
    }

    /// The waiting markers, the oldest first. One that can't be read is left out.
    pub fn waiting(&self) -> Vec<Marker> {
        let Ok(entries) = fs::read_dir(self.dir()) else {
            return Vec::new();
        };
        let mut out: Vec<Marker> = entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .filter_map(|entry| read_marker(&entry.path()))
            .collect();
        out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.key.cmp(&b.key)));
        out
    }

    /// The markers `run` holds claimed. One that can't be read is left out.
    pub fn claimed(&self, run: &str) -> Vec<Marker> {
        let Ok(entries) = fs::read_dir(self.dir().join(CLAIMED).join(run)) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .filter_map(|entry| read_marker(&entry.path()))
            .collect()
    }

    /// waiting → claimed, by `run`: a rename into the run's claims. `None` when another relay
    /// took it first, or it is gone.
    pub fn claim(&self, run: &str, key: &Digest) -> io::Result<Option<Marker>> {
        let to = self.claimed_path(run, key);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::rename(self.waiting_path(key), &to) {
            Ok(()) => Ok(read_marker(&to)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// claimed or sent → waiting, as `back` says: the marker is rewritten whole in the claim,
    /// then renamed back. A third cut-off drops it instead (waiting → done).
    pub fn release(&self, run: &str, marker: &Marker, back: Back) -> io::Result<()> {
        let mut marker = marker.clone();
        match back {
            Back::Later(not_before_ms) => marker.not_before_ms = not_before_ms,
            Back::CutOff => marker.cutoffs += 1,
            Back::Untouched => {}
        }
        let claimed = self.claimed_path(run, &marker.key);
        if marker.cutoffs >= MOST_CUTOFFS {
            return remove_file(&claimed);
        }
        let bytes = serde_json::to_vec(&marker).map_err(io::Error::other)?;
        self.store.write_whole(&claimed, &bytes, None, true)?;
        fs::rename(&claimed, self.waiting_path(&marker.key))
    }

    /// claimed or sent → done.
    pub fn done(&self, run: &str, key: &Digest) -> io::Result<()> {
        remove_file(&self.claimed_path(run, key))
    }

    /// claimed → waiting, for every run that `alive` says is dead: at any relay's start and in
    /// the sweep. A waiting marker of the same block (written since) stays, and the claim goes.
    pub fn return_claims(&self, alive: &dyn Fn(&str) -> bool) -> Tally {
        let mut tally = Tally::default();
        for run_dir in subfolders(&self.dir().join(CLAIMED)) {
            let run = run_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if alive(&run) {
                continue;
            }
            for entry in fs::read_dir(&run_dir).into_iter().flatten().flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let waiting = self.dir().join(&name);
                let moved = if waiting.exists() {
                    remove_file(&path)
                } else {
                    fs::rename(&path, &waiting)
                };
                count(&mut tally, moved);
            }
            let _ = fs::remove_dir(&run_dir);
        }
        tally
    }

    /// any → done (B-14's sweep): a waiting marker whose block is gone from the store, or that
    /// is older than the store's kept days.
    pub fn sweep(&self, now: SystemTime, keep_days: u32) -> Tally {
        let mut tally = Tally::default();
        let kept_ms = u64::from(keep_days) * DAY.as_millis() as u64;
        let now_ms = epoch_ms(now);
        let Ok(entries) = fs::read_dir(self.dir()) else {
            return tally;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_name().to_string_lossy().ends_with(".json") {
                continue;
            }
            let gone = match read_marker(&path) {
                None => true,
                Some(marker) => {
                    now_ms.saturating_sub(marker.created_ms) > kept_ms
                        || !matches!(self.store.block(&marker.key), Ok(Some(_)))
                }
            };
            if gone {
                count(&mut tally, remove_file(&path));
            }
        }
        tally
    }
}

/// The folder of `run` under `top` of the store: `incoming/<run>`, `outbox/tmp/<run>`,
/// `relays/<run>`.
pub fn run_folder(store: &Store, top: &str, run: &str) -> PathBuf {
    let base = match top {
        PACKS => store.root().join(OUTBOX).join(PACKS),
        other => store.root().join(other),
    };
    base.join(run)
}

/// A name for a run of the relay: `<milliseconds since the epoch>-<process id>`.
pub fn run_name(now: SystemTime, pid: u32) -> String {
    format!("{}-{pid}", epoch_ms(now))
}

/// Whether the run `run` of the store is alive: its folder under `relays/` is there and its
/// process is running. A run whose process is gone is dead, whatever it left.
pub fn alive(store: &Store, run: &str) -> bool {
    if !run_folder(store, RELAYS, run).is_dir() {
        return false;
    }
    let Some(pid) = run
        .rsplit('-')
        .next()
        .and_then(|pid| pid.parse::<i32>().ok())
    else {
        return false;
    };
    process_alive(pid)
}

/// Whether the process `pid` is running.
#[cfg(unix)]
pub(crate) fn process_alive(pid: i32) -> bool {
    match rustix::process::Pid::from_raw(pid) {
        Some(pid) => match rustix::process::test_kill_process(pid) {
            Ok(()) => true,
            Err(err) => err == rustix::io::Errno::PERM,
        },
        None => false,
    }
}

#[cfg(not(unix))]
pub(crate) fn process_alive(_pid: i32) -> bool {
    false
}

/// Removes the folders of dead runs (B-14, B-26): their claims go back to waiting first, then
/// `incoming/<run>`, `outbox/tmp/<run>` and `relays/<run>` go. At any relay's start and in the
/// sweep.
pub fn remove_dead_runs(store: &Store) -> Tally {
    let is_alive = |run: &str| alive(store, run);
    let mut tally = Outbox::new(store).return_claims(&is_alive);
    for top in [INCOMING, PACKS, RELAYS] {
        let base = run_folder(store, top, "");
        for dir in subfolders(&base) {
            let run = dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if top == RELAYS {
                if run
                    .rsplit('-')
                    .next()
                    .and_then(|p| p.parse::<i32>().ok())
                    .is_some_and(process_alive)
                {
                    continue;
                }
            } else if is_alive(&run) {
                continue;
            }
            count(&mut tally, fs::remove_dir_all(&dir));
        }
    }
    tally
}

fn read_marker(path: &Path) -> Option<Marker> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice::<Marker>(&bytes)
        .ok()
        .filter(|marker| marker.schema == MARKER_SCHEMA)
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

fn count(tally: &mut Tally, result: io::Result<()>) {
    match result {
        Ok(()) => tally.removed += 1,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_) => tally.errors += 1,
    }
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::store::NewFile;
    use std::time::Duration;

    fn world() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().join("build"));
        (dir, store)
    }

    fn by(org: &str) -> Identity {
        Identity {
            host: "https://api.example.com".into(),
            org: org.into(),
            credential: "c".into(),
        }
    }

    fn marker(n: u8, now: SystemTime) -> Marker {
        Marker::new(
            Digest::of(&[n]),
            Digest::of(&[n, n]),
            Class::Library,
            1000,
            by("acme"),
            None,
            now,
        )
    }

    fn publish(dir: &Path, store: &Store, key: &Digest) {
        let source = dir.join(key.hex());
        fs::write(&source, b"bytes").unwrap();
        store
            .publish(
                key,
                &serde_json::json!({}),
                &[NewFile {
                    name: "liba.rlib".into(),
                    source,
                }],
                SystemTime::now(),
            )
            .unwrap();
    }

    // covers: B-29
    #[test]
    fn a_claim_is_won_by_one_relay_and_every_way_back_is_a_state_of_the_table() {
        let (_dir, store) = world();
        let outbox = Outbox::new(&store);
        let now = SystemTime::now();
        let m = marker(1, now);
        // none → waiting
        assert!(outbox.add(&m).unwrap());
        assert_eq!(outbox.waiting(), vec![m.clone()]);
        // waiting → claimed: one relay wins.
        assert_eq!(outbox.claim("1-1", &m.key).unwrap(), Some(m.clone()));
        assert_eq!(outbox.claim("2-2", &m.key).unwrap(), None);
        assert!(outbox.waiting().is_empty());
        // A compile of the same block meanwhile doesn't add a second marker.
        assert!(!outbox.add(&m).unwrap());
        // claimed → waiting, with a date.
        outbox.release("1-1", &m, Back::Later(42)).unwrap();
        assert_eq!(outbox.waiting()[0].not_before_ms, 42);
        // claimed → waiting with a cut-off, three times → done.
        for n in 1..=MOST_CUTOFFS {
            let claimed = outbox.claim("1-1", &m.key).unwrap().unwrap();
            outbox.release("1-1", &claimed, Back::CutOff).unwrap();
            let left = outbox.waiting();
            if n < MOST_CUTOFFS {
                assert_eq!(left[0].cutoffs, n);
            } else {
                assert!(left.is_empty(), "the third cut-off drops it");
            }
        }
        // claimed → done.
        outbox.add(&m).unwrap();
        outbox.claim("1-1", &m.key).unwrap().unwrap();
        outbox.done("1-1", &m.key).unwrap();
        assert!(outbox.waiting().is_empty());
        assert_eq!(outbox.claim("1-1", &m.key).unwrap(), None);
    }

    // covers: B-14, B-29
    #[test]
    fn a_dead_run_s_claims_go_back_and_its_folders_go() {
        let (dir, store) = world();
        let outbox = Outbox::new(&store);
        let m = marker(1, SystemTime::now());
        outbox.add(&m).unwrap();
        // A run whose process is long gone (no such pid), with a claim and folders.
        let dead = "1-999999999";
        outbox.claim(dead, &m.key).unwrap().unwrap();
        for top in [INCOMING, PACKS, RELAYS] {
            let folder = run_folder(&store, top, dead);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("x"), b"x").unwrap();
        }
        // A live one: this process, with its folder.
        let live = run_name(SystemTime::now(), std::process::id());
        fs::create_dir_all(run_folder(&store, RELAYS, &live)).unwrap();
        fs::create_dir_all(run_folder(&store, INCOMING, &live)).unwrap();
        assert!(alive(&store, &live));
        assert!(!alive(&store, dead));
        remove_dead_runs(&store);
        assert_eq!(outbox.waiting(), vec![m.clone()]);
        for top in [INCOMING, PACKS, RELAYS] {
            assert!(!run_folder(&store, top, dead).exists(), "{top}");
        }
        assert!(run_folder(&store, INCOMING, &live).exists());
        // any → done: a block gone, or a marker past the kept days.
        assert_eq!(outbox.sweep(SystemTime::now(), 15).removed, 1);
        publish(dir.path(), &store, &m.key);
        outbox.add(&m).unwrap();
        assert_eq!(outbox.sweep(SystemTime::now(), 15).removed, 0);
        assert_eq!(
            outbox
                .sweep(SystemTime::now() + Duration::from_secs(16 * 86_400), 15)
                .removed,
            1
        );
    }
}
