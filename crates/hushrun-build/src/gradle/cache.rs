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

use std::net::TcpListener;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use serde_json::json;

use super::entry::Checked;
use super::{ENTRY, Local};
use crate::cache::http::{self, Body};
use crate::cache::shared::Shared;
use crate::model::{Class, Kind, NotStored, Outcome};
use crate::origin::Origin;
use crate::relay::Request;
use crate::session::{Current, Entry, GradleEntry};
use crate::store::{NewFile, Store};

/// The most bytes a `PUT`'s body may hold: a larger entry stays in Gradle's own cache.
pub const MOST_BODY: u64 = crate::cache::service::MOST_BODY;

/// The cache for one check's session.
pub struct Cache {
    /// The session, the relay and the log, as every tool's cache reaches them.
    shared: Shared,
    /// The `Authorization` header Gradle must send.
    expected: String,
}

pub use crate::cache::service::Running;

/// Serves `cache` on `listener` until the returned handle is dropped.
pub fn start(listener: TcpListener, cache: Cache) -> std::io::Result<Running> {
    crate::cache::service::start(listener, cache)
}

impl crate::cache::service::Answers for Cache {
    fn name(&self) -> &'static str {
        "gradle"
    }

    fn answer(&self, request: &http::Request) -> (u16, Vec<u8>) {
        Cache::answer(self, request)
    }

    fn headers(&self, status: u16, body: &[u8]) -> Vec<(&'static str, &'static str)> {
        headers(status, body)
    }
}

impl Cache {
    /// A cache for the session `current`, which Gradle reaches with `token`.
    pub fn new(current: Current, token: &str) -> Cache {
        Cache {
            shared: Shared::new(current, Kind::Gradle),
            expected: http::basic(super::USER, token),
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
        let store = Store::new(&self.shared.current.store);
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
        let store = Store::new(&self.shared.current.store);
        let at = SystemTime::now();
        if store.suspended_at(&key, at).is_some() {
            return None;
        }
        if let Ok(Some(_)) = store.block(&key) {
            return self.serve(&store, &key, gradle_key, false, at);
        }
        if self.shared.fetch(&store, &key) {
            return self.serve(&store, &key, gradle_key, true, at);
        }
        None
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
        let cost = block.description["rustc_ms"].as_u64().unwrap_or(0);
        let outputs = super::outputs_of(file.size, &file.sha256);
        if !self.shared.vouched(key, origin.as_ref(), outputs, at) {
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
            let entry = gradle_entry(Outcome::BuiltNotStored(NotStored::Compare), key, "");
            self.shared
                .refuse(store, key, downloaded, "Gradle entry", entry, at);
            return None;
        };
        self.shared.reused(store, key, origin.as_ref(), at);
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
        let store = Store::new(&self.shared.current.store);
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
        let floor = u64::from(self.shared.current.min_free_gb).saturating_mul(1 << 30);
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
                self.tell(&Request::ToolBuilt {
                    kind: Kind::Gradle,
                    key,
                    cost_ms,
                });
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
            self.tell(&Request::ToolBuilt {
                kind: Kind::Gradle,
                key,
                cost_ms,
            });
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
        let run = self.shared.current.gradle.clone().unwrap_or_default();
        let store = self.shared.current.store.to_string_lossy().into_owned();
        let mut folders: Vec<(Local, String)> = run
            .roots
            .iter()
            .map(|root| (Local::Checkout, root.clone()))
            .collect();
        if let Some(user_home) = &run.user_home {
            folders.push((Local::GradleHome, user_home.clone()));
        }
        folders.push((Local::Store, store));
        if let Some(home) = &self.shared.current.home {
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
        let share = self.shared.current.share.relay();
        let description = json!({
            "schema": crate::key::BLOCK_SCHEMA,
            "kind": Kind::Gradle.word(),
            "step": key,
            "class": Class::Library.as_str(),
            "gradle_key": gradle_key,
            "task": checked.identity,
            "gradle": checked.gradle_version,
            "repository": self.shared.current.repository,
            "rustc_ms": cost_ms,
            "built_ms": epoch_ms(at),
        });
        let run = share
            .map(|share| share.run.clone())
            .or_else(|| {
                self.shared
                    .current
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
        let dir = self.shared.current.folder.join("gradle-entries");
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

    fn tell(&self, request: &Request) {
        self.shared.tell(request);
    }

    fn log(&self, entry: Entry) {
        self.shared.log(entry);
    }

    /// The session's folder.
    pub fn folder(&self) -> &Path {
        self.shared.folder()
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

/// The headers of Gradle's answer of `status` with `body`: an entry's type, and the realm of a
/// refused credential.
fn headers(status: u16, body: &[u8]) -> Vec<(&'static str, &'static str)> {
    match status {
        200 if !body.is_empty() => vec![(
            "Content-Type",
            "application/vnd.gradle.build-cache-artifact.v2",
        )],
        401 => vec![("WWW-Authenticate", "Basic realm=\"hushrun\"")],
        _ => Vec::new(),
    }
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests;
