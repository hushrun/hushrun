//! What the service answers (B-78 to B-84), without the transport: the gRPC server in the
//! hushrun binary reads each request, hands it to [`Service::answer`], writes the reply, and then
//! runs what the reply left to do ([`After`]) off Xcode's path.
//!
//! **A `GetValue`** is answered, in this order, from: the service's own cache on this machine
//! ([`super::cas`]), once every object the value reaches is there; a block of hushrun's store
//! under [`super::key`], checked object by object and only as
//! [`crate::session::Shared::may_reuse`] lets a downloaded one be reused; the run's baked store
//! (B-64); a download the relay brings, only where the run shares and the check's `xcode/1`
//! manifest names it. Anything else is a miss, and Xcode compiles. One time in fifty, one in ten
//! on a downloaded block's first reuse here, a block is answered as a miss on purpose, never
//! twice for one key in a run, and the value Xcode then puts is compared with it (B-84).
//!
//! **A `PutValue`** is kept in the cache at once. Then, where the run has a session, the value
//! and what it reaches become a block when they may leave the machine (B-82), for the relay to
//! upload, and the audit's comparison is made.
//!
//! **Objects** (`Get`, `Load`, `Put`, `Save`) are the cache's alone: named by their content, so
//! any object a block brought is the same object Xcode would have put.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use hushrun_core::blocks::Hex32;
use serde_json::json;

use super::block::{self, Local};
use super::cas::{self, Cache, Id, Object};
use super::proto::{self, Bytes, Reply, Request, Value};
use crate::model::{Class, Kind, Mode, NotStored, Outcome};
use crate::origin::Origin;
use crate::relay::{self, Answer, Problem};
use crate::session::{Current, Entry, XcodeEntry};
use crate::store::{NewFile, Store};

/// The longest a `GetValue` waits for the relay: Xcode waits on it before it compiles.
const RELAY_WAIT: Duration = Duration::from_secs(10);
/// How long a question the relay answers from what it holds may take.
const QUICK_WAIT: Duration = Duration::from_secs(2);
/// Bytes from this many on go to a file when the plugin asks for one (`write_to_disk`).
pub const EXPORT_AT: usize = 1 << 20;
/// The most bytes of a file a `Put` or `Save` names that the service reads.
const MOST_FILE: u64 = cas::MOST_OBJECT;
/// The reason a value served from the cache stays here: it has no block in the store.
const LOCAL_ONLY: &str = "cache";
/// The most misses the service remembers the time of, for a compile's cost.
const MOST_MISSES: usize = 100_000;
/// A file in the session's folder: the relay failed once in this run, so nothing more is asked.
const RELAY_DOWN: &str = "xcode-relay-down";
/// The folder in the session's folder that marks keys audited in this run.
const AUDITED: &str = "xcode-audited";

/// What the service was started with.
pub struct Config {
    /// The store's folder, whose `xcode/` is the cache.
    pub store: PathBuf,
    /// The run's session, when there is one: what hushrun's store, the baked store and the relay
    /// are read through. `None`: the cache alone.
    pub current: Option<Current>,
    /// The Xcode build the values are compiled with (B-83): the platform of the manifest a fetch
    /// is looked up in, and of the run's `close`.
    pub platform: Option<String>,
    /// Folders of this machine an output must not hold to leave it (B-82): the home folder, the
    /// checkout's, DerivedData's.
    pub folders: Vec<String>,
    /// Where block files are staged before the store copies them.
    pub staging: PathBuf,
}

/// What a reply left to do, once Xcode has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum After {
    Nothing,
    /// The value Xcode put under `key`: the audit's comparison and the block.
    Keep {
        key: Vec<u8>,
        value: Value,
    },
    /// A value served from a block: its use recorded, its line logged.
    Served(Served),
}

/// A block served, recorded once Xcode has the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    key: Digest,
    first_download: bool,
    downloaded_now: bool,
    baked_now: bool,
    cost: u64,
    objects: u64,
}

struct Audit {
    key: Digest,
    value: Digest,
    downloaded: bool,
}

/// What the service did, for the line the run says and `hushrun build xcode status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Values asked for.
    pub asked: u64,
    /// Answered from this machine's cache.
    pub local: u64,
    /// Answered from a block of hushrun's store, a baked store or a download.
    pub reused: u64,
    /// Of those, downloaded in this run.
    pub downloaded: u64,
    /// Values Xcode put.
    pub put: u64,
    /// Of those, kept as blocks that may leave the machine.
    pub stored: u64,
    /// Audited values that came out different.
    pub differed: u64,
}

/// The service.
pub struct Service {
    cache: Cache,
    current: Option<Current>,
    platform: Option<String>,
    folders: Vec<String>,
    staging: PathBuf,
    misses: Mutex<HashMap<Vec<u8>, Instant>>,
    audits: Mutex<HashMap<Vec<u8>, Audit>>,
    relay_down: AtomicBool,
    counts: Mutex<Counts>,
    waits: Waits,
}

#[derive(Debug, Clone, Copy)]
struct Waits {
    relay: Duration,
    quick: Duration,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

impl Service {
    pub fn new(config: Config) -> Service {
        Service {
            cache: Cache::in_store(&config.store),
            current: config.current.filter(|current| current.mode != Mode::Off),
            platform: config.platform,
            folders: config.folders,
            staging: config.staging,
            misses: Mutex::new(HashMap::new()),
            audits: Mutex::new(HashMap::new()),
            relay_down: AtomicBool::new(false),
            counts: Mutex::new(Counts::default()),
            waits: Waits {
                relay: RELAY_WAIT,
                quick: QUICK_WAIT,
            },
        }
    }

    /// What it did so far.
    pub fn counts(&self) -> Counts {
        *lock(&self.counts)
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// The reply to one request, and what is left to do once Xcode has it.
    pub fn answer(&self, request: Request, now: SystemTime) -> (Reply, After) {
        match request {
            Request::CasGet { id, write_to_disk } => {
                let got = cas::id_of(&id).and_then(|id| self.cache.object(&id, now));
                let reply = got.map(|object| proto::Object {
                    references: object.references.iter().map(|id| id.to_vec()).collect(),
                    blob: self.handed(object.data, write_to_disk),
                });
                (Reply::CasGot(Ok(reply)), After::Nothing)
            }
            Request::CasLoad { id, write_to_disk } => {
                let got = cas::id_of(&id).and_then(|id| self.cache.object(&id, now));
                (
                    Reply::CasLoaded(Ok(got.map(|object| self.handed(object.data, write_to_disk)))),
                    After::Nothing,
                )
            }
            Request::CasPut(object) => (
                Reply::CasStored(self.put_object(object.blob, &object.references, now)),
                After::Nothing,
            ),
            Request::CasSave(blob) => (
                Reply::CasStored(self.put_object(blob, &[], now)),
                After::Nothing,
            ),
            Request::KvGet { key } => self.get_value(&key, now),
            Request::KvPut { key, value } => {
                lock(&self.counts).put += 1;
                match self.cache.put_value(&key, &value) {
                    Ok(()) => (Reply::KvPut(Ok(())), After::Keep { key, value }),
                    Err(err) => (Reply::KvPut(Err(err.to_string())), After::Nothing),
                }
            }
        }
    }

    /// An object's bytes as the reply carries them: in a file when the plugin asked for one and
    /// they are large, else themselves.
    fn handed(&self, data: Vec<u8>, write_to_disk: bool) -> Bytes {
        if write_to_disk
            && data.len() >= EXPORT_AT
            && let Ok(path) = self.cache.export(&data)
        {
            return Bytes::FilePath(path.to_string_lossy().into_owned());
        }
        Bytes::Data(data)
    }

    fn put_object(
        &self,
        blob: Bytes,
        references: &[Vec<u8>],
        now: SystemTime,
    ) -> Result<Vec<u8>, String> {
        let data = match blob {
            Bytes::Data(data) => data,
            Bytes::FilePath(path) => read_bounded(Path::new(&path))?,
        };
        if data.len() as u64 > cas::MOST_OBJECT {
            return Err("an object larger than the service keeps".into());
        }
        if references.len() > cas::MOST_REFERENCES {
            return Err("an object with more references than the service keeps".into());
        }
        let mut ids: Vec<Id> = Vec::with_capacity(references.len());
        for reference in references {
            let Some(id) = cas::id_of(reference) else {
                return Err("a reference that is no id this service gave".into());
            };
            if !self.cache.has(&id) {
                return Err("a reference to an object the service doesn't hold".into());
            }
            ids.push(id);
        }
        let object = Object {
            data,
            references: ids,
        };
        self.cache
            .put_object(&object, now)
            .map(|id| id.to_vec())
            .map_err(|err| err.to_string())
    }

    // --- GetValue ----------------------------------------------------------------------------

    fn get_value(&self, xcode_key: &[u8], now: SystemTime) -> (Reply, After) {
        lock(&self.counts).asked += 1;
        if let Some(value) = self.cache.value(xcode_key, now) {
            if block::present(&value, &self.cache) {
                lock(&self.counts).local += 1;
                if let Some(current) = &self.current {
                    // B-83: in the check's manifest only when its block is in the store, and so
                    // went up or will: a value that stays here would name a block no server has.
                    let key = super::key(xcode_key);
                    let shared = Store::new(&current.store)
                        .block(&key)
                        .is_ok_and(|block| block.is_some());
                    self.log(Entry {
                        xcode: Some(XcodeEntry {
                            local_cache: true,
                            local: (!shared).then(|| LOCAL_ONLY.to_string()),
                            platform: self.platform.clone().unwrap_or_default(),
                            ..XcodeEntry::default()
                        }),
                        found: true,
                        ..self.entry(Outcome::Reused, &key)
                    });
                }
                return (Reply::KvGot(Ok(Some(value))), After::Nothing);
            }
            // An object it reaches is gone (trimmed, removed by hand): as if never put.
            self.cache.forget_value(xcode_key);
        }
        {
            let mut misses = lock(&self.misses);
            // A long-lived service (the Xcode app's) forgets misses Xcode never put.
            if misses.len() >= MOST_MISSES {
                misses.clear();
            }
            misses.insert(xcode_key.to_vec(), Instant::now());
        }
        match self.served_from_store(xcode_key, now) {
            Some((value, served)) => (Reply::KvGot(Ok(Some(value))), After::Served(served)),
            None => (Reply::KvGot(Ok(None)), After::Nothing),
        }
    }

    fn served_from_store(&self, xcode_key: &[u8], now: SystemTime) -> Option<(Value, Served)> {
        let current = self.current.as_ref()?;
        let store = Store::new(&current.store);
        let key = super::key(xcode_key);
        if store.suspended_at(&key, now).is_some() {
            return None;
        }
        if let Ok(Some(_)) = store.block(&key) {
            return self.serve(current, &store, &key, xcode_key, false, now);
        }
        // B-64: the run's baked store, before the relay.
        if let (Some(lower), Some(unseal)) =
            (current.lower_for(Kind::Xcode), crate::baked::unseal())
            && crate::baked::Reader::new(lower, unseal)
                .open_into(
                    &store,
                    Kind::Xcode,
                    &key,
                    &key,
                    u64::from(current.min_free_gb).saturating_mul(1 << 30),
                    current.repository.as_deref(),
                    now,
                )
                .unwrap_or(false)
        {
            return self
                .serve(current, &store, &key, xcode_key, false, now)
                .map(|(value, served)| {
                    (
                        value,
                        Served {
                            baked_now: true,
                            ..served
                        },
                    )
                });
        }
        // B-83: a download, only where the run shares and its manifest names it.
        if current.mode != Mode::On {
            return None;
        }
        let platform = self.platform.clone()?;
        let local_ms = store.step_cost(&key);
        let asked = relay::Request::XcodeFetch {
            key,
            platform,
            local_ms,
        };
        let wait = local_ms.map_or(self.waits.relay, |ms| {
            (Duration::from_millis(ms) / 2 + self.waits.quick).min(self.waits.relay)
        });
        match self.ask(&asked, wait) {
            Some(Answer::Ready { key: got }) if got == key => {
                self.serve(current, &store, &key, xcode_key, true, now)
            }
            _ => None,
        }
    }

    /// Serves the block under `key` for Xcode's `xcode_key`, or `None` (a miss): not an Xcode
    /// block of this key, not vouched for (B-34), objects that fail their checks, or picked by
    /// the audit.
    fn serve(
        &self,
        current: &Current,
        store: &Store,
        key: &Digest,
        xcode_key: &[u8],
        downloaded_now: bool,
        now: SystemTime,
    ) -> Option<(Value, Served)> {
        let found = store.block(key).ok().flatten()?;
        let key_hex = hushrun_core::digest::to_hex(xcode_key);
        if Kind::of_description(&found.description) != Kind::Xcode
            || found.description["key"].as_str() != Some(key_hex.as_str())
        {
            return None;
        }
        let origin = store.origin(key);
        let outputs = outputs_of(&found);
        let vouch = relay::Request::XcodeVouch { key: *key, outputs };
        let cost = found.description["rustc_ms"].as_u64().unwrap_or(0);
        let vouch_wait = (Duration::from_millis(cost) / 2 + self.waits.quick).min(self.waits.relay);
        let reusable =
            current.may_reuse(origin.as_ref(), epoch_ms(now), vouch, &mut |_, request| {
                let wait = if matches!(request, relay::Request::Sharing) {
                    self.waits.quick
                } else {
                    vouch_wait
                };
                self.ask(request, wait)
            });
        if !reusable {
            return None;
        }
        let downloaded = origin.as_ref().is_some_and(Origin::downloaded_only);
        let mut files = Vec::with_capacity(found.files.len());
        for file in &found.files {
            let Ok(bytes) = std::fs::read(&file.path) else {
                let _ = store.discard(key);
                return None;
            };
            if bytes.len() as u64 != file.size || Digest::of(&bytes) != file.sha256 {
                // Changed on disk since it was stored: gone, and compiled.
                let _ = store.discard(key);
                return None;
            }
            files.push((file.name.clone(), bytes));
        }
        let expected = found.description["value"]
            .as_str()
            .and_then(|hex| Digest::from_hex(hex).ok());
        let value = match block::open(files, expected.as_ref(), &self.cache, now) {
            Ok(value) => value,
            Err(why) => {
                if downloaded {
                    let _ = self.ask(
                        &relay::Request::XcodeReport {
                            key: *key,
                            problem: Problem::Rejected,
                        },
                        self.waits.quick,
                    );
                    let until = now + Duration::from_secs(u64::from(current.keep_days) * 86_400);
                    let _ = store.suspend_until(
                        key,
                        "a downloaded Xcode block failed its checks",
                        until,
                        now,
                    );
                }
                let _ = store.discard(key);
                self.log(Entry {
                    note: Some(format!(
                        "an Xcode block in the store was refused: {}",
                        why.words()
                    )),
                    ..self.entry(Outcome::BuiltNotStored(NotStored::Compare), key)
                });
                return None;
            }
        };
        let first_download = origin
            .as_ref()
            .is_some_and(|o| o.downloaded_only() && !o.reused);
        // B-84: one in fifty, one in ten on a downloaded block's first reuse here, answered as a
        // miss and compared with what Xcode then puts; every one in local `observe`.
        let picked = current.mode == Mode::Observe
            || current.audits_reuse(origin.as_ref(), |reuse| {
                crate::audit::sample(key, current.started_ms, reuse)
            });
        if picked && self.claim_audit(current, key) {
            lock(&self.audits).insert(
                xcode_key.to_vec(),
                Audit {
                    key: *key,
                    value: cas::value_digest(&value),
                    downloaded,
                },
            );
            return None;
        }
        // From now on this machine's cache answers it.
        let _ = self.cache.put_value(xcode_key, &value);
        let objects = found.files.len().saturating_sub(1) as u64;
        Some((
            value,
            Served {
                key: *key,
                first_download,
                downloaded_now,
                baked_now: false,
                cost,
                objects,
            },
        ))
    }

    /// Claims the audit of `key` for the run: whether no service of the run audited it yet.
    fn claim_audit(&self, current: &Current, key: &Digest) -> bool {
        let dir = current.folder.join(AUDITED);
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        std::fs::File::create_new(dir.join(key.hex())).is_ok()
    }

    // --- after the reply ---------------------------------------------------------------------

    /// Runs what a reply left to do.
    pub fn after(&self, after: After, now: SystemTime) {
        match after {
            After::Nothing => {}
            After::Served(served) => self.served(served, now),
            After::Keep { key, value } => self.keep(&key, &value, now),
        }
    }

    fn served(&self, served: Served, now: SystemTime) {
        let Some(current) = &self.current else {
            return;
        };
        {
            let mut counts = lock(&self.counts);
            counts.reused += 1;
            if served.downloaded_now {
                counts.downloaded += 1;
            }
        }
        let store = Store::new(&current.store);
        let _ = store.used(&served.key, now);
        if served.first_download {
            let _ = store.update_origin(&served.key, |origin| {
                let mut origin = origin?;
                (!origin.reused).then(|| {
                    origin.reused = true;
                    origin
                })
            });
        }
        self.log(Entry {
            saved_ms: served.cost,
            found: true,
            downloaded: served.downloaded_now,
            baked: served.baked_now,
            xcode: Some(XcodeEntry {
                objects: served.objects,
                platform: self.platform.clone().unwrap_or_default(),
                ..XcodeEntry::default()
            }),
            ..self.entry(Outcome::Reused, &served.key)
        });
    }

    /// A value Xcode put: the audit's comparison, and the block, when it may leave.
    fn keep(&self, xcode_key: &[u8], value: &Value, now: SystemTime) {
        let cost_ms = lock(&self.misses)
            .remove(xcode_key)
            .map_or(0, |since| since.elapsed().as_millis() as u64);
        let Some(current) = &self.current else {
            return;
        };
        let store = Store::new(&current.store);
        let key = super::key(xcode_key);
        let digest = cas::value_digest(value);
        if let Some(audit) = lock(&self.audits).remove(xcode_key) {
            let differed = audit.value != digest;
            if differed {
                lock(&self.counts).differed += 1;
                self.differs(current, &store, &audit.key, now);
            }
            if differed && audit.downloaded {
                // The compile here takes the downloaded block's place (B-32).
                self.store_block(current, &store, &key, xcode_key, value, cost_ms, now, false);
            }
            self.log(Entry {
                found: true,
                differed,
                rustc_ms: cost_ms,
                note: differed.then(|| {
                    format!(
                        "an Xcode compile came out differently from its block in the store; its key is not reused for {} days",
                        current.keep_days
                    )
                }),
                ..self.entry(Outcome::BuiltAndStored, &key)
            });
            return;
        }
        if store.suspended_at(&key, now).is_some() {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..self.entry(Outcome::BuiltNotStored(NotStored::Suspended), &key)
            });
        }
        // A block there already: compared, as a second opinion (B-32). One only downloaded gives
        // way to this compile, which is from then on built here.
        if let Ok(Some(found)) = store.block(&key) {
            let theirs = found.description["value"]
                .as_str()
                .and_then(|hex| Digest::from_hex(hex).ok());
            let differed = theirs.is_some_and(|theirs| theirs != digest);
            if differed {
                lock(&self.counts).differed += 1;
                self.differs(current, &store, &key, now);
            }
            let downloaded = store.origin(&key).is_some_and(|o| o.downloaded_only());
            if downloaded {
                self.store_block(
                    current, &store, &key, xcode_key, value, cost_ms, now, !differed,
                );
            }
            return self.log(Entry {
                found: true,
                differed,
                rustc_ms: cost_ms,
                ..self.entry(Outcome::BuiltAndStored, &key)
            });
        }
        self.store_block(current, &store, &key, xcode_key, value, cost_ms, now, true);
    }

    /// Stores the block of `value` under `key` when it may leave the machine (B-82), built here
    /// by this run (B-32), in place of one only downloaded; tells the relay when `tell`.
    #[allow(clippy::too_many_arguments)]
    fn store_block(
        &self,
        current: &Current,
        store: &Store,
        key: &Digest,
        xcode_key: &[u8],
        value: &Value,
        cost_ms: u64,
        now: SystemTime,
        tell: bool,
    ) {
        let objects = match block::closure(value, &self.cache, now) {
            Ok(objects) => objects,
            Err(local) => return self.log_local(local, key, cost_ms),
        };
        if block::holds_a_folder(objects.values(), &self.folders) {
            return self.log_local(Local::HoldsPath, key, cost_ms);
        }
        let floor = u64::from(current.min_free_gb).saturating_mul(1 << 30);
        if floor > 0 && store.free_bytes().is_ok_and(|free| free < floor) {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..self.entry(Outcome::BuiltNotStored(NotStored::LowSpace), key)
            });
        }
        static STAGED: AtomicU64 = AtomicU64::new(0);
        let dir = self.staging.join(format!(
            "{}-{}-{}",
            std::process::id(),
            epoch_ms(now),
            STAGED.fetch_add(1, Ordering::Relaxed)
        ));
        let written = block::write_files(&dir, value, &objects);
        let stored = written.is_ok_and(|files| {
            let files: Vec<NewFile> = files
                .into_iter()
                .map(|(name, source)| NewFile { name, source })
                .collect();
            let share = current.share.relay();
            let description = json!({
                "schema": block::BLOCK_SCHEMA,
                "kind": Kind::Xcode.word(),
                "step": key,
                "class": Class::Library.as_str(),
                "key": hushrun_core::digest::to_hex(xcode_key),
                "value": cas::value_digest(value).hex(),
                "objects": objects.len(),
                "platform": self.platform,
                "repository": current.repository,
                "rustc_ms": cost_ms,
                "built_ms": epoch_ms(now),
            });
            let run = share
                .map(|share| share.run.clone())
                .or_else(|| {
                    current
                        .folder
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            let origin = Origin::built_here(&run, share.map(|share| share.by.clone()));
            store
                .replace(key, &description, &files, &origin, now, |there, _| {
                    there.is_some_and(Origin::downloaded_only)
                })
                .is_ok_and(|replaced| replaced == crate::store::Replaced::Stored)
        });
        let _ = std::fs::remove_dir_all(&dir);
        if !stored {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..self.entry(Outcome::BuiltNotStored(NotStored::StoreRefused), key)
            });
        }
        lock(&self.counts).stored += 1;
        let _ = store.set_step_cost(key, cost_ms, now);
        if tell {
            self.tell(&relay::Request::XcodeBuilt { key: *key, cost_ms });
        }
        let bytes: u64 = objects.values().map(|o| o.data.len() as u64).sum();
        self.log(Entry {
            rustc_ms: cost_ms,
            stored_bytes: bytes,
            xcode: Some(XcodeEntry {
                objects: objects.len() as u64,
                platform: self.platform.clone().unwrap_or_default(),
                ..XcodeEntry::default()
            }),
            ..self.entry(Outcome::BuiltAndStored, key)
        });
    }

    fn differs(&self, current: &Current, store: &Store, key: &Digest, now: SystemTime) {
        let downloaded = store.origin(key).is_some_and(|o| o.downloaded_only());
        if downloaded {
            let _ = self.ask(
                &relay::Request::XcodeReport {
                    key: *key,
                    problem: Problem::Differs,
                },
                self.waits.quick,
            );
        }
        let until = now + Duration::from_secs(u64::from(current.keep_days) * 86_400);
        let _ = store.suspend_until(
            key,
            "an Xcode compile came out differently from its block",
            until,
            now,
        );
    }

    fn log_local(&self, local: Local, key: &Digest, cost_ms: u64) {
        self.log(Entry {
            rustc_ms: cost_ms,
            xcode: Some(XcodeEntry {
                local: Some(local.as_str().to_string()),
                platform: self.platform.clone().unwrap_or_default(),
                ..XcodeEntry::default()
            }),
            ..self.entry(Outcome::BuiltNotStored(NotStored::StoreSkipped), key)
        });
    }

    fn log(&self, entry: Entry) {
        if let Some(current) = &self.current {
            current.log(&entry);
        }
    }

    /// Removes what nobody used from the cache: what the end of a service does.
    pub fn trim(&self, now: SystemTime, keep_days: u32) -> cas::Trimmed {
        self.cache.trim(now, keep_days)
    }

    // --- the relay ---------------------------------------------------------------------------

    /// Asks the run's relay within `wait`: no relay, or one call that failed or timed out in
    /// this run, and nothing more is asked (as Go's program, F10).
    fn ask(&self, request: &relay::Request, wait: Duration) -> Option<Answer> {
        if self.relay_is_down() {
            return None;
        }
        let share = self.current.as_ref()?.share.relay()?;
        let answer = relay::ask(&share.socket, request, wait);
        if answer.is_none() {
            let slow_fetch = matches!(request, relay::Request::XcodeFetch { .. })
                && relay::ask(&share.socket, &relay::Request::Sharing, self.waits.quick).is_some();
            if !slow_fetch {
                self.set_relay_down();
            }
        }
        answer
    }

    fn relay_is_down(&self) -> bool {
        if self.relay_down.load(Ordering::Acquire) {
            return true;
        }
        let down = self
            .current
            .as_ref()
            .is_some_and(|c| c.folder.join(RELAY_DOWN).exists());
        if down {
            self.relay_down.store(true, Ordering::Release);
        }
        down
    }

    fn set_relay_down(&self) {
        self.relay_down.store(true, Ordering::Release);
        if let Some(current) = &self.current {
            let _ = std::fs::write(current.folder.join(RELAY_DOWN), b"down\n");
        }
    }

    fn tell(&self, request: &relay::Request) {
        if self.relay_is_down() {
            return;
        }
        let Some(share) = self.current.as_ref().and_then(|c| c.share.relay()) else {
            return;
        };
        if !relay::tell(&share.socket, request, self.waits.quick) {
            self.set_relay_down();
        }
    }
}

/// The digest of a stored block's outputs, as the shared store's rows hold it (B-28).
pub fn outputs_of(block: &crate::store::Block) -> Hex32 {
    let files: Vec<hushrun_core::blocks::OutputFile<'_>> = block
        .files
        .iter()
        .map(|f| hushrun_core::blocks::OutputFile {
            name: &f.name,
            size: f.size,
            sha256: Hex32(*f.sha256.as_bytes()),
        })
        .collect();
    hushrun_core::blocks::outputs_digest(&files)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|err| format!("{}: {err}", path.display()))?;
    if !meta.is_file() || meta.len() > MOST_FILE {
        return Err(format!("{}: not a file the service reads", path.display()));
    }
    std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))
}

impl Service {
    /// An Xcode value's line in the run's log.
    fn entry(&self, outcome: Outcome, key: &Digest) -> Entry {
        Entry {
            outcome,
            class: Some(Class::Library),
            crate_name: String::new(),
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
            xcode: Some(XcodeEntry {
                platform: self.platform.clone().unwrap_or_default(),
                ..XcodeEntry::default()
            }),
            cc: None,
        }
    }
}

#[cfg(test)]
mod tests;
