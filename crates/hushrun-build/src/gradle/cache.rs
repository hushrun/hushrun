//! B-80, B-81: hushrun's Gradle cache, the remote Gradle's `HttpBuildCache` asks: a listener on
//! the loopback, in the `hushrun run` process, for one check's session. It answers a `GET` from
//! this machine's store, else, where the run shares, from what the relay downloads (B-80); it
//! keeps what a `PUT` sends as a block and tells the relay, which uploads it where the run may
//! write (B-81). It reaches the relay as every adapter does, over the session's socket, so it
//! holds no network code of its own.
//!
//! Every failure is a miss: a `GET` Gradle can't be answered is a 404, after which Gradle runs
//! the task; a `PUT` is answered 200 whatever happens to the entry, since Gradle keeps it in its
//! own cache either way, and turns a remote that fails off for the rest of the build.

use std::collections::HashSet;
use std::io::{BufReader, BufWriter};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use serde_json::json;

use super::entry::Checked;
use super::http::{self, Body, Unread};
use super::{ENTRY, Local};
use crate::model::{Class, Kind, Mode, NotStored, Outcome};
use crate::origin::Origin;
use crate::relay::{self, Answer, Problem, Request};
use crate::session::{Current, Entry, GradleEntry};
use crate::store::{NewFile, Store};

/// The longest a question waits on a relay that says nothing (ADR 0144): a relay working on an
/// answer says so every heartbeat, so this bounds a frozen or gone relay only, and every question
/// waits the same, whatever its task costs.
const RELAY_WAIT: Duration = relay::WAIT;
/// The most bytes a `PUT`'s body may hold: a larger entry stays in Gradle's own cache.
pub const MOST_BODY: u64 = 512 << 20;
/// How long a connection may sit between requests.
const IDLE: Duration = Duration::from_secs(120);
/// Connections served at once: Gradle opens one per worker that asks.
const MOST_CONNECTIONS: usize = 64;

/// The cache for one check's session.
pub struct Cache {
    current: Current,
    /// The `Authorization` header Gradle must send.
    expected: String,
    /// The relay failed or timed out once: nothing more is asked of it this run.
    relay_down: AtomicBool,
    /// Keys whose first reuse of a download here was counted, so it is counted once.
    first_reuses: Mutex<HashSet<Digest>>,
}

/// The listener while it serves: dropping it stops it.
#[derive(Debug)]
pub struct Running {
    stop: Arc<AtomicBool>,
    address: Option<SocketAddr>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accepting thread, which then sees the flag.
        if let Some(address) = self.address {
            let _ = TcpStream::connect_timeout(&address, Duration::from_millis(200));
        }
    }
}

/// Serves `cache` on `listener` until the returned handle is dropped.
pub fn start(listener: TcpListener, cache: Cache) -> std::io::Result<Running> {
    let stop = Arc::new(AtomicBool::new(false));
    let address = listener.local_addr().ok();
    let cache = Arc::new(cache);
    let serving = stop.clone();
    std::thread::Builder::new()
        .name("hushrun-gradle-cache".into())
        .spawn(move || accept(listener, cache, serving))?;
    Ok(Running { stop, address })
}

fn accept(listener: TcpListener, cache: Arc<Cache>, stop: Arc<AtomicBool>) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else {
            continue;
        };
        // Only this machine: the listener is on the loopback, and a peer elsewhere is refused.
        if !stream.peer_addr().is_ok_and(|peer| peer.ip().is_loopback()) {
            continue;
        }
        if open.fetch_add(1, Ordering::SeqCst) >= MOST_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let mut out = &stream;
            let _ = http::write(&mut out, 503, b"", false, true);
            continue;
        }
        let cache = cache.clone();
        let done = open.clone();
        let stop = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("hushrun-gradle-request".into())
            .spawn(move || {
                cache.connection(stream, &stop);
                done.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            open.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Cache {
    /// A cache for the session `current`, which Gradle reaches with `token`.
    pub fn new(current: Current, token: &str) -> Cache {
        Cache {
            current,
            expected: http::basic(super::USER, token),
            relay_down: AtomicBool::new(false),
            first_reuses: Mutex::new(HashSet::new()),
        }
    }

    fn connection(&self, stream: TcpStream, stop: &AtomicBool) {
        let _ = stream.set_read_timeout(Some(IDLE));
        let _ = stream.set_write_timeout(Some(IDLE));
        let _ = stream.set_nodelay(true);
        let Ok(reading) = stream.try_clone() else {
            return;
        };
        let mut input = BufReader::new(reading);
        let mut output = BufWriter::new(&stream);
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let request = match http::read(&mut input, &mut output, MOST_BODY) {
                Ok(request) => request,
                Err(Unread::Gone) => return,
                Err(Unread::Bad(status)) => {
                    let _ = http::write(&mut output, status, b"", false, true);
                    return;
                }
            };
            // ADR 0144: `102 Processing` each heartbeat while a `GET` is worked on.
            let (status, body) = http::answer_alive(&request, &stream, || self.answer(&request));
            let head = request.method == "HEAD";
            if http::write(&mut output, status, &body, head, request.close).is_err()
                || request.close
            {
                return;
            }
        }
    }

    /// One request's status and body.
    pub fn answer(&self, request: &http::Request) -> (u16, Vec<u8>) {
        let Some(gradle_key) = request.path.strip_prefix(super::PATH) else {
            return (404, Vec::new());
        };
        if !http::same_credential(request.authorization.as_deref(), &self.expected) {
            return (401, Vec::new());
        }
        if !super::is_gradle_key(gradle_key) {
            return (404, Vec::new());
        }
        match request.method.as_str() {
            "GET" => match self.get(gradle_key) {
                Some(body) => (200, body),
                None => (404, Vec::new()),
            },
            // Whether this machine's store holds the key: no download, nothing logged, no body.
            "HEAD" if self.holds(gradle_key) => (200, Vec::new()),
            "HEAD" => (404, Vec::new()),
            "PUT" => {
                if let Body::Bytes(bytes) = &request.body {
                    self.put(gradle_key, bytes);
                }
                (200, Vec::new())
            }
            _ => (405, Vec::new()),
        }
    }

    // --- get ---------------------------------------------------------------------------

    /// Whether this machine's store holds a Gradle block for `gradle_key` that isn't suspended.
    fn holds(&self, gradle_key: &str) -> bool {
        let Some(key) = super::key(gradle_key) else {
            return false;
        };
        let store = Store::new(&self.current.store);
        if store.suspended_at(&key, SystemTime::now()).is_some() {
            return false;
        }
        store.block(&key).ok().flatten().is_some_and(|block| {
            Kind::of_description(&block.description) == Kind::Gradle
                && block.description["gradle_key"].as_str() == Some(gradle_key)
        })
    }

    /// B-80: the entry for `gradle_key` as Gradle takes it, or `None` (a miss).
    pub fn get(&self, gradle_key: &str) -> Option<Vec<u8>> {
        let key = super::key(gradle_key)?;
        let store = Store::new(&self.current.store);
        let at = SystemTime::now();
        if store.suspended_at(&key, at).is_some() {
            return None;
        }
        if let Ok(Some(_)) = store.block(&key) {
            return self.serve(&store, &key, gradle_key, false, at);
        }
        // A download, only where the run shares with sharing on (the relay says) and the
        // check's `gradle/1` manifest names it.
        if self.current.mode != Mode::On {
            return None;
        }
        let asked = Request::GradleFetch {
            key,
            local_ms: store.step_cost(&key),
        };
        match self.ask(&asked, RELAY_WAIT) {
            Some(Answer::Ready { key: got }) if got == key => {
                self.serve(&store, &key, gradle_key, true, at)
            }
            _ => None,
        }
    }

    /// Serves the block under `key` for `gradle_key`, or `None` when it can't be: not a Gradle
    /// block of that key, not vouched for (B-34), or bytes that fail their checks (B-78).
    fn serve(
        &self,
        store: &Store,
        key: &Digest,
        gradle_key: &str,
        downloaded_now: bool,
        at: SystemTime,
    ) -> Option<Vec<u8>> {
        let block = store.block(key).ok().flatten()?;
        if Kind::of_description(&block.description) != Kind::Gradle
            || block.description["gradle_key"].as_str() != Some(gradle_key)
            || block.files.len() != 1
            || block.files[0].name != ENTRY
        {
            return None;
        }
        let file = &block.files[0];
        let origin = store.origin(key);
        let vouch = Request::GradleVouch {
            key: *key,
            outputs: super::outputs_of(file.size, &file.sha256),
        };
        let cost = block.description["rustc_ms"].as_u64().unwrap_or(0);
        let reusable =
            self.current
                .may_reuse(origin.as_ref(), epoch_ms(at), vouch, &mut |_, request| {
                    self.ask(request, RELAY_WAIT)
                });
        if !reusable {
            return None;
        }
        let downloaded = origin.as_ref().is_some_and(Origin::downloaded_only);
        let bytes = std::fs::read(&file.path).ok()?;
        if bytes.len() as u64 != file.size || Digest::of(&bytes) != file.sha256 {
            // Changed on disk since it was stored: gone, and the task runs.
            let _ = store.discard(key);
            return None;
        }
        let canonical = Checked::from_tar(&bytes, gradle_key)
            .ok()
            .filter(|checked| checked.canonical() == bytes);
        let Some(checked) = canonical else {
            // B-78: a block that isn't a canonical entry of its key is never served. One that
            // was downloaded is reported to the scope its record names, and its key suspended
            // for the kept days, as every adapter's refused downloads are; then it goes.
            if downloaded {
                let _ = self.ask(
                    &Request::GradleReport {
                        key: *key,
                        problem: Problem::Rejected,
                    },
                    RELAY_WAIT,
                );
                let until = at + Duration::from_secs(u64::from(self.current.keep_days) * 86_400);
                let _ = store.suspend_until(
                    key,
                    "a downloaded Gradle entry failed its checks",
                    until,
                    at,
                );
            }
            let _ = store.discard(key);
            self.log(Entry {
                note: Some("a Gradle entry in the store was refused: it failed its checks".into()),
                ..gradle_entry(Outcome::BuiltNotStored(NotStored::Compare), key, "")
            });
            return None;
        };
        let _ = store.used(key, at);
        let first_download = downloaded
            && origin.as_ref().is_some_and(|o| !o.reused)
            && self.first_reuses.lock().is_ok_and(|mut s| s.insert(*key));
        if first_download {
            let _ = store.update_origin(key, |origin| {
                let mut origin = origin?;
                (!origin.reused).then(|| {
                    origin.reused = true;
                    origin
                })
            });
        }
        self.log(Entry {
            saved_ms: cost,
            found: true,
            downloaded: downloaded_now,
            ..gradle_entry(Outcome::Reused, key, &checked.identity)
        });
        Some(super::entry::to_gradle(&bytes))
    }

    // --- put ---------------------------------------------------------------------------

    /// B-81: an entry Gradle stored after it ran a task: a block, when it may be one, and the
    /// relay told of it when it may leave.
    pub fn put(&self, gradle_key: &str, body: &[u8]) {
        let Some(key) = super::key(gradle_key) else {
            return;
        };
        let checked = match Checked::from_gradle(body, gradle_key) {
            Ok(checked) => checked,
            Err(why) => {
                return self.log(Entry {
                    note: Some(format!("a Gradle entry wasn't kept: {why}")),
                    ..gradle_entry(Outcome::BuiltNotStored(NotStored::StoreSkipped), &key, "")
                });
            }
        };
        let cost_ms = checked.execution_ms;
        let task = checked.identity.clone();
        let canonical = checked.canonical();
        let sha = Digest::of(&canonical);
        let store = Store::new(&self.current.store);
        let at = SystemTime::now();
        // B-79: an entry whose outputs hold a folder of this machine stays with Gradle's own
        // cache.
        if let Some(local) = self.stays_here(&checked) {
            let mut entry = Entry {
                rustc_ms: cost_ms,
                ..gradle_entry(
                    Outcome::BuiltNotStored(NotStored::StoreSkipped),
                    &key,
                    &task,
                )
            };
            entry.gradle = Some(GradleEntry { local: Some(local) });
            return self.log(entry);
        }
        if store.suspended_at(&key, at).is_some() {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..gradle_entry(Outcome::BuiltNotStored(NotStored::Suspended), &key, &task)
            });
        }
        let floor = u64::from(self.current.min_free_gb).saturating_mul(1 << 30);
        if floor > 0 && store.free_bytes().is_ok_and(|free| free < floor) {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..gradle_entry(Outcome::BuiltNotStored(NotStored::LowSpace), &key, &task)
            });
        }
        // A block there already: one only downloaded gives way to this execution, which is
        // from then on built here (B-32). Other bytes are no sign against it: Gradle's outputs
        // needn't be the same from one execution to the next (B-81).
        if let Ok(Some(block)) = store.block(&key)
            && let [file] = block.files.as_slice()
        {
            let differed = file.sha256 != sha;
            let downloaded = store.origin(&key).is_some_and(|o| o.downloaded_only());
            let took = downloaded
                && self.publish(&store, &key, gradle_key, &checked, &canonical, cost_ms, at);
            if took {
                self.tell(&Request::GradleBuilt { key, cost_ms });
            }
            return self.log(Entry {
                found: true,
                differed,
                rustc_ms: cost_ms,
                ..gradle_entry(Outcome::BuiltAndStored, &key, &task)
            });
        }
        if self.publish(&store, &key, gradle_key, &checked, &canonical, cost_ms, at) {
            let _ = store.set_step_cost(&key, cost_ms, at);
            self.tell(&Request::GradleBuilt { key, cost_ms });
            self.log(Entry {
                rustc_ms: cost_ms,
                stored_bytes: canonical.len() as u64,
                ..gradle_entry(Outcome::BuiltAndStored, &key, &task)
            });
        } else {
            self.log(Entry {
                rustc_ms: cost_ms,
                ..gradle_entry(
                    Outcome::BuiltNotStored(NotStored::StoreRefused),
                    &key,
                    &task,
                )
            });
        }
    }

    /// B-79: why an entry's outputs keep it on this machine: they hold the checkout's folder,
    /// Gradle's user home, the store's folder or the home folder, as bytes anywhere in a
    /// member's name or contents.
    pub fn stays_here(&self, checked: &Checked) -> Option<Local> {
        let run = self.current.gradle.clone().unwrap_or_default();
        let store = self.current.store.to_string_lossy().into_owned();
        let mut folders: Vec<(Local, String)> = run
            .roots
            .iter()
            .map(|root| (Local::Checkout, root.clone()))
            .collect();
        if let Some(user_home) = &run.user_home {
            folders.push((Local::GradleHome, user_home.clone()));
        }
        folders.push((Local::Store, store));
        if let Some(home) = &self.current.home {
            folders.push((Local::Home, home.clone()));
        }
        folders
            .into_iter()
            .filter(|(_, folder)| folder.len() > 1)
            .find(|(_, folder)| checked.outputs_hold(folder.trim_end_matches('/').as_bytes()))
            .map(|(local, _)| local)
    }

    /// Publishes the canonical entry as the block of `key`, built here by this run (B-32), in
    /// place of one only downloaded. Whether it is stored.
    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        store: &Store,
        key: &Digest,
        gradle_key: &str,
        checked: &Checked,
        canonical: &[u8],
        cost_ms: u64,
        at: SystemTime,
    ) -> bool {
        let share = self.current.share.relay();
        let description = json!({
            "schema": crate::key::BLOCK_SCHEMA,
            "kind": Kind::Gradle.word(),
            "step": key,
            "class": Class::Library.as_str(),
            "gradle_key": gradle_key,
            "task": checked.identity,
            "gradle": checked.gradle_version,
            "repository": self.current.repository,
            "rustc_ms": cost_ms,
            "built_ms": epoch_ms(at),
        });
        let run = share
            .map(|share| share.run.clone())
            .or_else(|| {
                self.current
                    .folder
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let origin = Origin::built_here(&run, share.map(|share| share.by.clone()));
        let Some(temp) = self.write_temp(key, canonical) else {
            return false;
        };
        let files = [NewFile {
            name: ENTRY.into(),
            source: temp.clone(),
        }];
        let stored = store
            .replace(key, &description, &files, &origin, at, |there, _| {
                there.is_some_and(Origin::downloaded_only)
            })
            .is_ok_and(|replaced| replaced == crate::store::Replaced::Stored);
        let _ = std::fs::remove_file(&temp);
        stored
    }

    fn write_temp(&self, key: &Digest, bytes: &[u8]) -> Option<std::path::PathBuf> {
        let dir = self.current.folder.join("gradle-entries");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!(
            "{}.{}.{:?}.tmp",
            key.hex(),
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, bytes).ok()?;
        Some(path)
    }

    // --- the relay and the log ---------------------------------------------------------

    /// Asks the run's relay within `wait`: no relay, or one call that failed or timed out this
    /// run, and nothing more is asked (B-26).
    fn ask(&self, request: &Request, wait: Duration) -> Option<Answer> {
        if self.relay_down.load(Ordering::SeqCst) {
            return None;
        }
        let share = self.current.share.relay()?;
        let answer = relay::ask(&share.socket, request, wait);
        if answer.is_none() {
            self.relay_down.store(true, Ordering::SeqCst);
        }
        answer
    }

    fn tell(&self, request: &Request) {
        if self.relay_down.load(Ordering::SeqCst) {
            return;
        }
        if let Some(share) = self.current.share.relay()
            && !relay::tell(&share.socket, request, RELAY_WAIT)
        {
            self.relay_down.store(true, Ordering::SeqCst);
        }
    }

    fn log(&self, entry: Entry) {
        self.current.log(&entry);
    }

    /// The session's folder.
    pub fn folder(&self) -> &Path {
        &self.current.folder
    }
}

/// A Gradle entry's line in the run's log: its task by Gradle's name.
fn gradle_entry(outcome: Outcome, key: &Digest, task: &str) -> Entry {
    Entry {
        xcode: None,
        outcome,
        class: Some(Class::Library),
        crate_name: task.to_string(),
        package: String::new(),
        rustc_ms: 0,
        saved_ms: 0,
        stored_bytes: 0,
        found: false,
        differed: false,
        note: None,
        portable: None,
        step: Some(*key),
        key: Some(*key),
        downloaded: false,
        baked: false,
        go: None,
        cc: None,
        gradle: Some(GradleEntry::default()),
        maven: None,
        tsc: None,
    }
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests;
