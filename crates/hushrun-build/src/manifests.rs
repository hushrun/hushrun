//! B-63: the manifests a run of a check leaves in this machine's store, so that `hushrun build
//! bake` can bake a store from this machine alone, with no backend.
//!
//! A run that shares sends its manifests with `close` (B-30, B-45, B-60), one per kind and
//! platform, and keeps nothing; a backend then answers the newest one per repository, check,
//! kind and platform. A machine's store keeps the same thing for itself: one file per
//! repository, check and platform (in a tag's words, so a GNU and a musl hushrun of one machine
//! share it), `manifests/<digest>.json`, holding one part per kind and platform (`rustc/1` for
//! the run's target, `go/1` for Go's `GOOS_GOARCH`, `cc/1` per target triple the launcher
//! compiled for), written whole under the slot's lock when the run ends. So the newest manifest
//! of a check here is the one file there, and no clock compares two. A bake reads every check's
//! file of the repository and platform, or those it names.
//!
//! Cargo, Go and Ninja call the compiler only for what changed, so a run names only the compiles
//! it saw. Each part is the last one with each first key the run compiled given its new block, in
//! place, and the run's new first keys after; a pair of the last one whose block the store no
//! longer holds, or holds unused for longer than the store keeps blocks, goes. So a laptop's
//! manifest names its whole build, whatever was found fresh, and nothing the sweep would remove.
//!
//! Unlike a backend's, it holds the keys themselves, not their addresses: a bake opens the
//! store's blocks by key. The store already holds every key as a folder's name; the file adds no
//! secret, and it never leaves the machine. A file that can't be read is set aside under a name
//! that says so, never silently replaced, and the run says so.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use hushrun_core::Digest;
use hushrun_core::blocks::{self, Hex32, Pair};
use serde::{Deserialize, Serialize};

use crate::model::Kind;
use crate::session::Entry;
use crate::store::{Store, Tally};

/// The folder, under the store's root.
pub const FOLDER: &str = "manifests";
/// Each file's schema.
pub const SCHEMA: &str = "hushrun.build-manifest/v2";
/// How long a save waits for another run of the same check to finish saving.
pub const LOCK_WAIT: Duration = Duration::from_secs(30);
/// What a slot's file that couldn't be read is renamed to end with, before the stamp.
const UNREADABLE: &str = ".unreadable";

/// Which manifest a run leaves: its check and target, the checkout it ran in, and Go's platform
/// when a Go command of the run had the cache program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// The check's id (B-26), or none.
    pub check: Option<Hex32>,
    /// The target triple of the run's `rustc/1` part.
    pub target: String,
    /// The checkout the run's compiles ran in.
    pub checkout: Option<String>,
    /// Go's `GOOS_GOARCH`, when the run is a Go module's.
    pub go: Option<String>,
}

/// One kind's manifest for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPart {
    /// `rustc/1`, `go/1` or `cc/1`.
    pub kind: String,
    /// Its platform.
    pub platform: String,
    /// Each compile's first key and block key, in the order they ended, each pair once.
    pub pairs: Vec<(Digest, Digest)>,
}

impl SavedPart {
    /// The pairs as a backend keeps them, by address.
    pub fn addresses(&self) -> Vec<Pair> {
        self.pairs
            .iter()
            .map(|(step, key)| {
                Pair(
                    blocks::address(&self.kind, step.as_bytes()),
                    blocks::address(&self.kind, key.as_bytes()),
                )
            })
            .collect()
    }

    /// The part's identity ([`crate::baked::manifest_id`]): what a backend's manifest of the same
    /// run would have.
    pub fn id(&self) -> Hex32 {
        crate::baked::manifest_id(&self.kind, &self.addresses())
    }
}

/// One saved manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    /// [`SCHEMA`].
    pub schema: String,
    /// The repository, as a run names it.
    pub repository: Option<String>,
    /// The check's id.
    pub check: Option<Hex32>,
    /// The target triple of its `rustc/1` part.
    pub target: String,
    /// When it was saved, by this machine's clock: shown, never compared.
    pub saved_ms: u64,
    /// For a repository with no name, the SHA-256 of its checkout's folder, which tells it apart.
    pub checkout: Option<Hex32>,
    /// One part per kind and platform.
    pub parts: Vec<SavedPart>,
}

/// What a repository with no name is told apart by: the SHA-256 of its checkout's folder,
/// resolved, which never leaves the machine.
fn checkout_digest(checkout: &str) -> Hex32 {
    let resolved = fs::canonicalize(checkout)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| checkout.to_string());
    Hex32::sha256(format!("hushrun.build-manifest-checkout/v1\n{resolved}").as_bytes())
}

/// The slot's file name: the repository (or, with no name, the checkout), the check, and the
/// platform in a tag's words, so a GNU and a musl build of hushrun, or of the compiler, on one
/// machine name one slot.
fn slot_name(
    repository: Option<&str>,
    checkout: Option<&str>,
    check: Option<&Hex32>,
    target: &str,
) -> String {
    let whose = repository.map_or_else(
        || {
            format!(
                "checkout:{}",
                checkout.map(checkout_digest).unwrap_or(Hex32([0; 32]))
            )
        },
        |r| format!("repository:{r}"),
    );
    Hex32::sha256(
        format!(
            "hushrun.build-manifest-slot/v3\n{whose}\n{}\n{}",
            check.map(ToString::to_string).unwrap_or_default(),
            crate::baked::platform_of(target)
        )
        .as_bytes(),
    )
    .to_string()
}

/// The parts of a run's compiles, as its `close`s would send them (B-30, B-45, B-60): each
/// compile with a first key and a block key, in the order logged, each pair once per part. A Go
/// archive kept here (B-42) and a probe of CMake's are in none.
pub fn parts_of(entries: &[Entry], slot: &Slot) -> Vec<SavedPart> {
    let mut parts: Vec<SavedPart> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        let (Some(step), Some(key)) = (entry.step, entry.key) else {
            continue;
        };
        let (kind, platform) = match entry.kind() {
            Kind::Rustc => (Kind::Rustc, slot.target.clone()),
            Kind::Go => {
                if entry.go.as_ref().is_some_and(|go| go.local.is_some()) {
                    continue;
                }
                let Some(platform) = slot.go.clone() else {
                    continue;
                };
                (Kind::Go, platform)
            }
            Kind::Cc => {
                let Some(cc) = entry
                    .cc
                    .as_ref()
                    .filter(|cc| !cc.probe && !cc.platform.is_empty())
                else {
                    continue;
                };
                (Kind::Cc, cc.platform.clone())
            }
            Kind::Xcode => {
                let Some(xcode) = entry
                    .xcode
                    .as_ref()
                    .filter(|xcode| xcode.local.is_none() && !xcode.platform.is_empty())
                else {
                    continue;
                };
                (Kind::Xcode, xcode.platform.clone())
            }
            // Gradle's entries aren't baked yet (B-82): their manifest goes with `close` alone.
            Kind::Gradle => continue,
        };
        if !seen.insert((kind, platform.clone(), step, key)) {
            continue;
        }
        let at = match parts
            .iter()
            .position(|p| p.kind == kind.word() && p.platform == platform)
        {
            Some(at) => at,
            None => {
                parts.push(SavedPart {
                    kind: kind.word().to_string(),
                    platform,
                    pairs: Vec::new(),
                });
                parts.len() - 1
            }
        };
        if parts[at].pairs.len() < blocks::MOST_MANIFEST {
            parts[at].pairs.push((step, key));
        }
    }
    parts
}

/// The pairs of a part's next manifest: `last`'s, each first key this run compiled given this
/// run's block in place, and this run's other pairs after; a pair of `last` whose block `held`
/// says is gone, or stale, is left out.
fn merged(
    last: &[(Digest, Digest)],
    run: Vec<(Digest, Digest)>,
    held: impl Fn(&Digest) -> bool,
) -> Vec<(Digest, Digest)> {
    let by_step: std::collections::HashMap<Digest, Digest> = run.iter().copied().collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (step, key) in last {
        let pair = match by_step.get(step) {
            Some(new) => (*step, *new),
            None if held(key) => (*step, *key),
            None => continue,
        };
        if seen.insert(pair) {
            out.push(pair);
        }
    }
    for pair in run {
        if seen.insert(pair) {
            out.push(pair);
        }
    }
    out.truncate(blocks::MOST_MANIFEST);
    out
}

/// What a slot's last file was, as a save found it.
enum Last {
    /// None: the slot's first save.
    None,
    /// Read.
    Read(Saved),
    /// There, and couldn't be read: set aside at this path.
    SetAside(PathBuf),
}

/// Saves a run's manifest whole, under the slot's lock (two runs of one check on one store merge
/// one after the other; a save waits at most [`LOCK_WAIT`]): a temporary file of a name no other
/// process uses, synced, then a rename over the slot's last. A run with no pair saves nothing,
/// and leaves the last as it was. A pair of the last whose block isn't used within `keep_days`
/// of `now` goes, as the sweep would remove the block. A last file that can't be read is set
/// aside, never silently replaced: `Ok(Some(line))` says so.
pub fn save(
    store: &Store,
    repository: Option<&str>,
    slot: &Slot,
    entries: &[Entry],
    now: SystemTime,
    keep_days: u32,
) -> io::Result<Option<String>> {
    let run = parts_of(entries, slot);
    if run.is_empty() {
        return Ok(None);
    }
    let name = slot_name(
        repository,
        slot.checkout.as_deref(),
        slot.check.as_ref(),
        &slot.target,
    );
    let dir = store.root().join(FOLDER);
    fs::create_dir_all(dir.as_path())?;
    let lock_path = dir.join(format!("{name}.lock"));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    lock_within(&lock, &lock_path, LOCK_WAIT)?;
    let path = dir.join(format!("{name}.json"));
    let last = match read(&path) {
        Ok(Some(saved)) => Last::Read(saved),
        Ok(None) => Last::None,
        Err(_) => {
            let aside = dir.join(format!(
                ".{name}.json{UNREADABLE}.{}",
                crate::baked::unique("set-aside")
            ));
            fs::rename(&path, &aside)?;
            Last::SetAside(aside)
        }
    };
    let keep = Duration::from_secs(u64::from(keep_days) * 24 * 60 * 60);
    let fresh_enough = |key: &Digest| {
        store
            .last_used(key)
            .is_some_and(|used| now.duration_since(used).map_or(true, |age| age <= keep))
    };
    let last_parts = match &last {
        Last::Read(saved) => saved.parts.clone(),
        _ => Vec::new(),
    };
    let mut parts: Vec<SavedPart> = Vec::new();
    for part in &last_parts {
        let fresh = run
            .iter()
            .find(|p| p.kind == part.kind && p.platform == part.platform);
        let pairs = merged(
            &part.pairs,
            fresh.map(|p| p.pairs.clone()).unwrap_or_default(),
            fresh_enough,
        );
        if !pairs.is_empty() {
            parts.push(SavedPart {
                pairs,
                ..part.clone()
            });
        }
    }
    for part in run {
        if !parts
            .iter()
            .any(|p| p.kind == part.kind && p.platform == part.platform)
        {
            parts.push(part);
        }
    }
    let saved = Saved {
        schema: SCHEMA.to_string(),
        repository: repository.map(str::to_string),
        check: slot.check,
        target: slot.target.clone(),
        saved_ms: now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64),
        checkout: match repository {
            Some(_) => None,
            None => slot.checkout.as_deref().map(checkout_digest),
        },
        parts,
    };
    let temp = dir.join(format!(".{}.tmp", crate::baked::unique(&name)));
    let bytes = serde_json::to_vec(&saved).map_err(io::Error::other)?;
    let written = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()
    })()
    .and_then(|()| fs::rename(&temp, &path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written?;
    Ok(match last {
        Last::SetAside(aside) => Some(format!(
            "build: this check's saved manifest couldn't be read, so it was set aside as {} and a new one begun",
            aside.display()
        )),
        _ => None,
    })
}

/// Takes `lock` on `file`, waiting at most `wait`: past it, an error that names the lock.
fn lock_within(file: &fs::File, path: &Path, wait: Duration) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(fs::TryLockError::WouldBlock) if started.elapsed() < wait => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "another run of this check held {} for {} seconds, so this run's manifest wasn't saved",
                        path.display(),
                        wait.as_secs()
                    ),
                ));
            }
            Err(fs::TryLockError::Error(err)) => return Err(err),
        }
    }
}

/// A slot's file: `Ok(None)` when there is none, `Err` when it is there and can't be read as
/// this version's.
fn read(path: &Path) -> io::Result<Option<Saved>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let saved: Saved = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if saved.schema != SCHEMA {
        return Err(io::Error::other(format!(
            "{} is {}",
            path.display(),
            saved.schema
        )));
    }
    Ok(Some(saved))
}

/// The slots' files: `<digest>.json`, never a temporary file, a lock or one set aside.
fn slot_files(store: &Store) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(store.root().join(FOLDER)) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.starts_with('.') && n.ends_with(".json"))
        })
        .collect();
    out.sort();
    out
}

/// B-63: the manifests this machine saved for `repository` (or, with none, `checkout`) on the
/// platform of `target`, one per check: of each of `checks` when some are named, else of every
/// check, in the order of their ids. A file that can't be read is left out.
pub fn of_checks(
    store: &Store,
    repository: Option<&str>,
    checkout: Option<&str>,
    checks: &[Hex32],
    target: &str,
) -> Vec<Saved> {
    let mut out: Vec<Saved> = if checks.is_empty() {
        let platform = crate::baked::platform_of(target);
        let whose = match repository {
            Some(_) => None,
            None => checkout.map(checkout_digest),
        };
        slot_files(store)
            .iter()
            .filter_map(|path| read(path).ok().flatten())
            .filter(|saved| {
                saved.repository.as_deref() == repository
                    && crate::baked::platform_of(&saved.target) == platform
                    && (repository.is_some() || saved.checkout == whose)
            })
            .collect()
    } else {
        checks
            .iter()
            .filter_map(|check| {
                let name = slot_name(repository, checkout, Some(check), target);
                read(&store.root().join(FOLDER).join(format!("{name}.json")))
                    .ok()
                    .flatten()
            })
            .collect()
    };
    out.sort_by_key(|saved| saved.check);
    out.dedup_by_key(|saved| saved.check);
    out
}

/// The sweep's step (B-14): a manifest saved longer ago than `keep_days`, one set aside, and a
/// temporary file a dead save left, go.
pub fn sweep(store: &Store, now: SystemTime, keep_days: u32) -> Tally {
    let mut tally = Tally::default();
    let Ok(entries) = fs::read_dir(store.root().join(FOLDER)) else {
        return tally;
    };
    let keep = Duration::from_secs(u64::from(keep_days) * 24 * 60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // A slot's lock stays while its manifest does.
        if name.ends_with(".lock") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > keep);
        if old {
            match fs::remove_file(entry.path()) {
                Ok(()) => tally.removed += 1,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => tally.errors += 1,
            }
        }
    }
    tally
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Outcome;

    fn entry(step: u8, key: u8) -> Entry {
        Entry {
            outcome: Outcome::Reused,
            class: None,
            crate_name: "a".into(),
            package: String::new(),
            rustc_ms: 0,
            saved_ms: 0,
            stored_bytes: 0,
            found: true,
            differed: false,
            note: None,
            portable: None,
            step: Some(Digest::of(&[step])),
            key: Some(Digest::of(&[key])),
            downloaded: false,
            baked: false,
            go: None,
            xcode: None,
            cc: None,
            gradle: None,
        }
    }

    fn go(key: u8) -> Entry {
        Entry {
            step: Some(Digest::of(&[key])),
            key: Some(Digest::of(&[key])),
            go: Some(crate::session::GoEntry::default()),
            ..entry(0, 0)
        }
    }

    fn cc(step: u8, key: u8, platform: &str) -> Entry {
        Entry {
            xcode: None,
            cc: Some(crate::cc::launcher::CcEntry {
                platform: platform.into(),
                ..Default::default()
            }),
            ..entry(step, key)
        }
    }

    const TARGET: &str = "aarch64-unknown-linux-gnu";

    fn slot(check: Hex32) -> Slot {
        Slot {
            check: Some(check),
            target: TARGET.into(),
            checkout: Some("/work/shop".into()),
            go: Some("linux_arm64".into()),
        }
    }

    /// The saved manifest of `check`, as a bake that names it reads it.
    fn newest(
        store: &Store,
        repository: Option<&str>,
        checkout: Option<&str>,
        check: Option<&Hex32>,
        target: &str,
    ) -> Option<Saved> {
        of_checks(
            store,
            repository,
            checkout,
            check.map(std::slice::from_ref).unwrap_or(&[]),
            target,
        )
        .into_iter()
        .next()
    }

    const KEEP: u32 = 15;

    fn pairs(saved: &Saved, kind: &str) -> Vec<(Digest, Digest)> {
        saved
            .parts
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| p.pairs.clone())
            .unwrap_or_default()
    }

    // covers: B-63
    #[test]
    fn a_run_s_manifest_replaces_its_slot_s_last_and_the_newest_is_found_without_a_clock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let repo = Some("github.com/acme/shop");
        let build = Hex32([1; 32]);
        let test = Hex32([2; 32]);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // Nothing compiled: nothing saved.
        save(&store, repo, &slot(build), &[], at, KEEP).unwrap();
        assert!(newest(&store, repo, None, Some(&build), TARGET).is_none());

        let run = [
            entry(1, 1),
            entry(2, 2),
            entry(1, 1),
            go(5),
            cc(6, 7, "aarch64-linux-gnu"),
            cc(8, 9, "x86_64-linux-gnu"),
        ];
        save(&store, repo, &slot(build), &run, at, KEEP).unwrap();
        let first = newest(&store, repo, None, Some(&build), TARGET).unwrap();
        assert_eq!(pairs(&first, "rustc/1").len(), 2);
        assert_eq!(
            pairs(&first, "go/1"),
            vec![(Digest::of(&[5]), Digest::of(&[5]))]
        );
        assert_eq!(first.parts.len(), 4, "{:?}", first.parts);
        // A repository with a name keeps no folder of the run's, not even as a digest.
        let text = fs::read_to_string(slot_files(&store)[0].clone()).unwrap();
        assert!(!text.contains("/work/shop"));
        assert_eq!(first.checkout, None);
        // The next run of the same check replaces it, whatever the clock says; the store holds
        // none of the first run's blocks, so their pairs go.
        save(
            &store,
            repo,
            &slot(build),
            &[entry(3, 3)],
            at - Duration::from_secs(60),
            KEEP,
        )
        .unwrap();
        let second = newest(&store, repo, None, Some(&build), TARGET).unwrap();
        assert_eq!(
            pairs(&second, "rustc/1"),
            vec![(Digest::of(&[3]), Digest::of(&[3]))]
        );
        assert_ne!(first.parts[0].id(), second.parts[0].id());

        // A run that compiled only some crates keeps the last manifest's pairs whose blocks the
        // store holds, its own block in place for a first key it compiled again.
        let held: Vec<Digest> = vec![Digest::of(&[7])];
        let last = vec![
            (Digest::of(&[5]), Digest::of(&[6])),
            (Digest::of(&[8]), Digest::of(&[7])),
            (Digest::of(&[9]), Digest::of(&[9])),
        ];
        let next = merged(
            &last,
            vec![
                (Digest::of(&[5]), Digest::of(&[50])),
                (Digest::of(&[1]), Digest::of(&[1])),
            ],
            |key| held.contains(key),
        );
        assert_eq!(
            next,
            vec![
                (Digest::of(&[5]), Digest::of(&[50])),
                (Digest::of(&[8]), Digest::of(&[7])),
                (Digest::of(&[1]), Digest::of(&[1])),
            ]
        );

        // Another check keeps its own; with no check named, the one saved last.
        save(
            &store,
            repo,
            &slot(test),
            &[entry(4, 4)],
            at + Duration::from_secs(60),
            KEEP,
        )
        .unwrap();
        assert_eq!(
            newest(&store, repo, None, Some(&build), TARGET),
            Some(second)
        );
        // With no check named, every check's, in the order of their ids.
        let all = of_checks(&store, repo, None, &[], TARGET);
        assert_eq!(
            all.iter().map(|s| s.check).collect::<Vec<_>>(),
            [Some(build), Some(test)]
        );
        assert!(of_checks(&store, Some("github.com/acme/other"), None, &[], TARGET).is_empty());
        assert!(of_checks(&store, repo, None, &[], "x86_64-unknown-linux-gnu").is_empty());
        // A musl build of hushrun or of the compiler finds what a GNU one saved.
        assert_eq!(
            of_checks(&store, repo, None, &[], "aarch64-unknown-linux-musl").len(),
            2
        );
        let any = all[1].clone();
        // A temporary file a save left is never a manifest.
        fs::write(
            store.root().join(FOLDER).join(".x.tmp"),
            serde_json::to_vec(&any).unwrap(),
        )
        .unwrap();
        assert_eq!(slot_files(&store).len(), 2);
    }

    // covers: B-63
    #[test]
    fn two_runs_of_one_check_saving_at_once_lose_no_pair() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let check = Hex32([1; 32]);
        let at = SystemTime::now();
        // Blocks the store holds, so that a merge keeps them.
        let src = dir.path().join("src");
        fs::write(&src, b"x").unwrap();
        for n in 0..16u8 {
            store
                .publish(
                    &Digest::of(&[n]),
                    &serde_json::json!({}),
                    &[crate::store::NewFile {
                        name: "f".into(),
                        source: src.clone(),
                    }],
                    at,
                )
                .unwrap();
        }
        std::thread::scope(|scope| {
            for n in 0..16u8 {
                let store = &store;
                scope.spawn(move || {
                    save(store, Some("r"), &slot(check), &[entry(n, n)], at, KEEP).unwrap();
                });
            }
        });
        let saved = newest(&store, Some("r"), None, Some(&check), TARGET).unwrap();
        assert_eq!(pairs(&saved, "rustc/1").len(), 16, "{saved:?}");
    }

    #[test]
    fn a_repository_with_no_name_is_told_apart_by_its_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let check = Hex32([1; 32]);
        let at = SystemTime::now();
        let mut one = slot(check);
        one.checkout = Some("/work/one".into());
        let mut two = slot(check);
        two.checkout = Some("/work/two".into());
        save(&store, None, &one, &[entry(1, 1)], at, KEEP).unwrap();
        save(&store, None, &two, &[entry(2, 2)], at, KEEP).unwrap();
        let found = newest(&store, None, Some("/work/one"), Some(&check), TARGET).unwrap();
        assert_eq!(
            pairs(&found, "rustc/1"),
            vec![(Digest::of(&[1]), Digest::of(&[1]))]
        );
    }
}

#[cfg(test)]
mod leftovers {
    use super::*;
    use crate::model::Outcome;

    fn entry(n: u8) -> Entry {
        Entry {
            outcome: Outcome::Reused,
            class: None,
            crate_name: "a".into(),
            package: String::new(),
            rustc_ms: 0,
            saved_ms: 0,
            stored_bytes: 0,
            found: true,
            differed: false,
            note: None,
            portable: None,
            step: Some(Digest::of(&[n])),
            key: Some(Digest::of(&[n])),
            downloaded: false,
            baked: false,
            go: None,
            xcode: None,
            cc: None,
            gradle: None,
        }
    }

    fn slot(target: &str) -> Slot {
        Slot {
            check: Some(Hex32([1; 32])),
            target: target.into(),
            checkout: Some("/work/shop".into()),
            go: None,
        }
    }

    fn publish(store: &Store, dir: &Path, n: u8, at: SystemTime) {
        let src = dir.join("src");
        fs::write(&src, b"x").unwrap();
        store
            .publish(
                &Digest::of(&[n]),
                &serde_json::json!({}),
                &[crate::store::NewFile {
                    name: "f".into(),
                    source: src,
                }],
                at,
            )
            .unwrap();
    }

    // covers: B-63
    #[test]
    fn a_saved_manifest_that_can_t_be_read_is_set_aside_and_said() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let at = SystemTime::now();
        publish(&store, dir.path(), 1, at);
        save(
            &store,
            Some("r"),
            &slot("aarch64-unknown-linux-gnu"),
            &[entry(1)],
            at,
            15,
        )
        .unwrap();
        let file = slot_files(&store)[0].clone();
        fs::write(&file, b"{ half a file").unwrap();
        let said = save(
            &store,
            Some("r"),
            &slot("aarch64-unknown-linux-gnu"),
            &[entry(2)],
            at,
            15,
        )
        .unwrap()
        .expect("a line");
        assert!(said.contains("set aside"), "{said}");
        // The bytes are kept beside it, under a name no slot has.
        let aside: Vec<PathBuf> = fs::read_dir(store.root().join(FOLDER))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(UNREADABLE))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(fs::read(&aside[0]).unwrap(), b"{ half a file");
        assert_eq!(slot_files(&store).len(), 1);
        // Set aside, it goes with the sweep once older than the kept days, as a manifest does
        // (a rename keeps its date); a slot's lock stays, one per slot.
        let later = SystemTime::now() + Duration::from_secs(16 * 24 * 60 * 60);
        assert!(sweep(&store, later, 15).removed >= 1);
        assert!(!aside[0].exists());
        let left: Vec<String> = fs::read_dir(store.root().join(FOLDER))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.iter().all(|n| n.ends_with(".lock")), "{left:?}");
    }

    // covers: B-63
    #[test]
    fn a_gnu_and_a_musl_hushrun_share_one_slot() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let at = SystemTime::now();
        save(
            &store,
            Some("r"),
            &slot("x86_64-unknown-linux-gnu"),
            &[entry(1)],
            at,
            15,
        )
        .unwrap();
        save(
            &store,
            Some("r"),
            &slot("x86_64-unknown-linux-musl"),
            &[entry(2)],
            at,
            15,
        )
        .unwrap();
        assert_eq!(slot_files(&store).len(), 1);
        assert_eq!(
            of_checks(&store, Some("r"), None, &[], "x86_64-unknown-linux-musl").len(),
            1
        );
    }

    // covers: B-63
    #[test]
    fn a_pair_whose_block_is_unused_for_the_kept_days_leaves_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let long_ago = SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60);
        let now = SystemTime::now();
        publish(&store, dir.path(), 1, long_ago);
        publish(&store, dir.path(), 2, now);
        let target = "aarch64-unknown-linux-gnu";
        save(
            &store,
            Some("r"),
            &slot(target),
            &[entry(1), entry(2)],
            now,
            15,
        )
        .unwrap();
        // A later run that compiled something else: the pair of the block untouched for 40
        // days goes, the fresh one stays.
        publish(&store, dir.path(), 3, now);
        save(&store, Some("r"), &slot(target), &[entry(3)], now, 15).unwrap();
        let saved = &of_checks(&store, Some("r"), None, &[], target)[0];
        let keys: Vec<Digest> = saved.parts[0].pairs.iter().map(|(_, k)| *k).collect();
        assert_eq!(keys, [Digest::of(&[2]), Digest::of(&[3])]);
    }

    // covers: B-63
    #[test]
    fn a_save_waits_for_the_slot_s_lock_only_so_long() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let held = fs::File::create(&path).unwrap();
        held.lock().unwrap();
        let other = fs::OpenOptions::new().write(true).open(&path).unwrap();
        let err = lock_within(&other, &path, Duration::from_millis(100)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(err.to_string().contains("x.lock"), "{err}");
        drop(held);
        lock_within(&other, &path, Duration::from_millis(100)).unwrap();
    }
}
