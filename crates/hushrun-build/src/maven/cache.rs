//! B-170, B-174, B-175: hushrun's cache for Maven, the remote Apache's build cache extension
//! asks: a listener on the loopback, in the `hushrun run` process, for one check's session.
//!
//! A module's entry comes in pieces. On a `GET` of its `buildinfo.xml` the cache answers from
//! this machine's store, else, where the run shares, from what the relay downloads (B-174); the
//! files that `buildinfo.xml` names are then served from the same block, and only after it. After
//! Maven built a module it `PUT`s each file, then `buildinfo.xml`: the cache holds them in memory
//! until the `buildinfo.xml` and every file it names are there, then keeps the whole as one block
//! and tells the relay, which uploads it where the run may write (B-175). It reaches the relay as
//! every adapter does, over the session's socket, so it holds no network code of its own.
//!
//! Every failure is a miss: a `GET` that can't be answered is a 404, after which Maven builds the
//! module; a `PUT` is answered 200 whatever becomes of the file, since the extension keeps its own
//! local copy either way.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use serde_json::json;

use super::buildinfo::{self, Checked};
use super::{BUILDINFO, Local, Module, REPORT};
use crate::cache::http::{self, Body};
use crate::model::{Class, Kind, Mode, NotStored, Outcome};
use crate::origin::Origin;
use crate::relay::{self, Answer, Problem, Request};
use crate::session::{Current, Entry, MavenEntry};
use crate::store::{Block, NewFile, Store};

/// The longest a question waits on a relay that says nothing (ADR 0144): a relay working on an
/// answer says so every heartbeat, so this bounds a frozen or gone relay only, and every question
/// waits the same, whatever its module costs.
const RELAY_WAIT: Duration = relay::WAIT;
/// The most bytes one `PUT`'s body may hold.
pub const MOST_BODY: u64 = crate::cache::service::MOST_BODY;
/// The most bytes held for modules whose entry isn't whole yet: what comes beyond it stays in the
/// extension's own cache.
pub const MOST_PENDING: u64 = 512 << 20;

/// The cache for one check's session.
pub struct Cache {
    current: Current,
    /// The first part of every path Maven asks: the run's token.
    token: String,
    /// The relay failed or timed out once: nothing more is asked of it this run.
    relay_down: AtomicBool,
    /// Keys whose first reuse of a download here was counted, so it is counted once.
    first_reuses: Mutex<HashSet<Digest>>,
    /// The modules whose `buildinfo.xml` this run served: only their files are served (B-174).
    served: Mutex<HashSet<Digest>>,
    /// When this run's `GET` of a module's `buildinfo.xml` missed: Maven built the module from
    /// then until it stored it, which is its cost (B-175).
    missed: Mutex<HashMap<Digest, Instant>>,
    /// The files of modules whose entry isn't whole yet (B-175).
    pending: Mutex<Pending>,
    /// The property by which this run's Maven skips tests, once the run knows (B-175): no
    /// entry it makes is kept.
    skips_tests: Arc<Mutex<Option<String>>>,
}

#[derive(Default)]
struct Pending {
    modules: HashMap<Module, Parts>,
    bytes: u64,
}

#[derive(Default)]
struct Parts {
    buildinfo: Option<Vec<u8>>,
    files: BTreeMap<String, Vec<u8>>,
}

impl Parts {
    fn bytes(&self) -> u64 {
        let files: usize = self.files.values().map(Vec::len).sum();
        (files + self.buildinfo.as_ref().map_or(0, Vec::len)) as u64
    }
}

pub use crate::cache::service::Running;

/// Serves `cache` on `listener` until the returned handle is dropped.
pub fn start(listener: TcpListener, cache: Cache) -> std::io::Result<Running> {
    crate::cache::service::start(listener, cache)
}

/// The headers of Maven's answer of `status` with `body`: a file's type.
fn headers(status: u16, body: &[u8]) -> &'static [(&'static str, &'static str)] {
    if status == 200 && !body.is_empty() {
        &[("Content-Type", "application/octet-stream")]
    } else {
        &[]
    }
}

impl crate::cache::service::Answers for Cache {
    fn name(&self) -> &'static str {
        "maven"
    }

    fn answer(&self, request: &http::Request) -> (u16, Vec<u8>) {
        Cache::answer(self, request)
    }

    fn headers(&self, status: u16, body: &[u8]) -> Vec<(&'static str, &'static str)> {
        headers(status, body).to_vec()
    }
}

impl Cache {
    /// A cache for the session `current`, which Maven reaches under `token`.
    pub fn new(current: Current, token: &str) -> Cache {
        Cache {
            current,
            token: token.to_string(),
            relay_down: AtomicBool::new(false),
            first_reuses: Mutex::new(HashSet::new()),
            served: Mutex::new(HashSet::new()),
            missed: Mutex::new(HashMap::new()),
            pending: Mutex::new(Pending::default()),
            skips_tests: Arc::new(Mutex::new(None)),
        }
    }

    /// What the session sets once it knows how COMMAND runs Maven (B-175): the property by which
    /// it skips tests.
    pub fn skips_tests(&self) -> Arc<Mutex<Option<String>>> {
        self.skips_tests.clone()
    }

    /// One request's status and body (B-170, B-171).
    pub fn answer(&self, request: &http::Request) -> (u16, Vec<u8>) {
        let mut parts = request.path.split('/').filter(|p| !p.is_empty());
        let token = parts.next().unwrap_or_default();
        if !http::same_credential(Some(token), &self.token) {
            return (401, Vec::new());
        }
        // The report the extension puts under a build's id at the end of a build: taken and
        // dropped, as a remote that keeps nothing of it.
        if request.method == "PUT" && parts.next_back() == Some(REPORT) {
            return (200, Vec::new());
        }
        let Some(asked) = super::read_path(&request.path) else {
            return (404, Vec::new());
        };
        match request.method.as_str() {
            "GET" if asked.file == BUILDINFO => match self.get(&asked.module) {
                Some(body) => (200, body),
                None => (404, Vec::new()),
            },
            "GET" => match self.file(&asked.module, asked.file) {
                Some(body) => (200, body),
                None => (404, Vec::new()),
            },
            // Whether this machine's store holds the file: no download, nothing logged.
            "HEAD" if self.holds(&asked.module, asked.file) => (200, Vec::new()),
            "HEAD" => (404, Vec::new()),
            "PUT" => {
                match &request.body {
                    Body::Bytes(bytes) => self.put(&asked.module, asked.file, bytes),
                    // Too large to hold: the module's entry can't be whole, and goes.
                    Body::TooLarge => self.drop_pending(&asked.module),
                }
                (200, Vec::new())
            }
            _ => (405, Vec::new()),
        }
    }

    // --- get ---------------------------------------------------------------------------

    /// The block of `module` in this machine's store, when it is a Maven block of that module
    /// that isn't suspended.
    fn block_of(&self, store: &Store, module: &Module) -> Option<Block> {
        let key = module.key();
        if store.suspended_at(&key, SystemTime::now()).is_some() {
            return None;
        }
        let block = store.block(&key).ok().flatten()?;
        (Kind::of_description(&block.description) == Kind::Maven
            && block.description["group"].as_str() == Some(module.group.as_str())
            && block.description["artifact"].as_str() == Some(module.artifact.as_str())
            && block.description["checksum"].as_str() == Some(module.checksum.as_str())
            && block.files.first().is_some_and(|f| f.name == BUILDINFO))
        .then_some(block)
    }

    fn holds(&self, module: &Module, file: &str) -> bool {
        let store = Store::new(&self.current.store);
        self.block_of(&store, module)
            .is_some_and(|block| block.files.iter().any(|f| f.name == file))
    }

    /// B-174: the `buildinfo.xml` of `module`, or `None` (a miss).
    pub fn get(&self, module: &Module) -> Option<Vec<u8>> {
        let key = module.key();
        let store = Store::new(&self.current.store);
        let at = SystemTime::now();
        if store.suspended_at(&key, at).is_some() {
            return None;
        }
        let served = if let Ok(Some(_)) = store.block(&key) {
            self.serve(&store, &key, module, false, at)
        } else if self.current.mode == Mode::On {
            // A download, only where the run shares with sharing on (the relay says) and the
            // check's `maven/1` manifest names it.
            let asked = Request::ToolFetch {
                kind: Kind::Maven,
                key,
                local_ms: store.step_cost(&key),
            };
            match self.ask(&asked, RELAY_WAIT) {
                Some(Answer::Ready { key: got }) if got == key => {
                    self.serve(&store, &key, module, true, at)
                }
                _ => None,
            }
        } else {
            None
        };
        if served.is_none()
            && let Ok(mut missed) = self.missed.lock()
        {
            missed.entry(key).or_insert_with(Instant::now);
        }
        served
    }

    /// Serves the `buildinfo.xml` of the block under `key`, or `None` when it can't be: not a
    /// Maven block of `module`, not vouched for (B-34), or files that fail their checks (B-172).
    fn serve(
        &self,
        store: &Store,
        key: &Digest,
        module: &Module,
        downloaded_now: bool,
        at: SystemTime,
    ) -> Option<Vec<u8>> {
        let block = self.block_of(store, module)?;
        let origin = store.origin(key);
        let vouch = Request::ToolVouch {
            kind: Kind::Maven,
            key: *key,
            outputs: super::outputs_of(&block),
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
        let Some(buildinfo) = whole(&block) else {
            // B-172: a block that isn't a canonical entry of its module is never served. One
            // that was downloaded is reported to the scope its record names, and its key
            // suspended for the kept days, as every adapter's refused downloads are; then it goes.
            if downloaded {
                let _ = self.ask(
                    &Request::ToolReport {
                        kind: Kind::Maven,
                        key: *key,
                        problem: Problem::Rejected,
                    },
                    RELAY_WAIT,
                );
                let until = at + Duration::from_secs(u64::from(self.current.keep_days) * 86_400);
                let _ = store.suspend_until(
                    key,
                    "a downloaded Maven entry failed its checks",
                    until,
                    at,
                );
            }
            let _ = store.discard(key);
            self.log(Entry {
                note: Some("a Maven entry in the store was refused: it failed its checks".into()),
                ..maven_entry(
                    Outcome::BuiltNotStored(NotStored::Compare),
                    key,
                    &module.name(),
                )
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
        if let Ok(mut served) = self.served.lock() {
            served.insert(*key);
        }
        self.log(Entry {
            saved_ms: cost,
            found: true,
            downloaded: downloaded_now,
            ..maven_entry(Outcome::Reused, key, &module.name())
        });
        Some(buildinfo)
    }

    /// B-174: a file of `module`'s entry, once this run served its `buildinfo.xml`.
    pub fn file(&self, module: &Module, name: &str) -> Option<Vec<u8>> {
        let key = module.key();
        if !self.served.lock().is_ok_and(|s| s.contains(&key)) {
            return None;
        }
        let store = Store::new(&self.current.store);
        let block = self.block_of(&store, module)?;
        let file = block.files.iter().find(|f| f.name == name)?;
        let bytes = std::fs::read(&file.path).ok()?;
        if bytes.len() as u64 != file.size || Digest::of(&bytes) != file.sha256 {
            // Changed on disk since it was stored: gone, and Maven builds the module.
            let _ = store.discard(&key);
            return None;
        }
        Some(bytes)
    }

    // --- put ---------------------------------------------------------------------------

    /// B-175: a file Maven stored after it built a module, held until the module's entry is
    /// whole, which is then kept as a block.
    pub fn put(&self, module: &Module, name: &str, body: &[u8]) {
        let whole = {
            let Ok(mut pending) = self.pending.lock() else {
                return;
            };
            if pending.bytes.saturating_add(body.len() as u64) > MOST_PENDING {
                let dropped = pending.modules.remove(module).map_or(0, |p| p.bytes());
                pending.bytes = pending.bytes.saturating_sub(dropped);
                drop(pending);
                return self.log(Entry {
                    note: Some(format!(
                        "a Maven entry wasn't kept: {} holds more than 512 MiB at once",
                        module.name()
                    )),
                    ..maven_entry(
                        Outcome::BuiltNotStored(NotStored::StoreSkipped),
                        &module.key(),
                        &module.name(),
                    )
                });
            }
            let parts = pending.modules.entry(module.clone()).or_default();
            let before = parts.bytes();
            if name == BUILDINFO {
                parts.buildinfo = Some(body.to_vec());
            } else {
                parts.files.insert(name.to_string(), body.to_vec());
            }
            let after = parts.bytes();
            pending.bytes = pending.bytes.saturating_add(after).saturating_sub(before);
            take_whole(&mut pending, module)
        };
        match whole {
            None => {}
            Some(Err(why)) => self.log(Entry {
                note: Some(format!("a Maven entry wasn't kept: {why}")),
                ..maven_entry(
                    Outcome::BuiltNotStored(NotStored::StoreSkipped),
                    &module.key(),
                    &module.name(),
                )
            }),
            Some(Ok((checked, files))) => self.keep(module, &checked, &files),
        }
    }

    fn drop_pending(&self, module: &Module) {
        if let Ok(mut pending) = self.pending.lock() {
            let dropped = pending.modules.remove(module).map_or(0, |p| p.bytes());
            pending.bytes = pending.bytes.saturating_sub(dropped);
        }
    }

    /// Keeps a whole entry as the block of `module` (B-175).
    fn keep(&self, module: &Module, checked: &Checked, files: &[(String, Vec<u8>)]) {
        let key = module.key();
        let name = module.name();
        let cost_ms = self
            .missed
            .lock()
            .ok()
            .and_then(|mut missed| missed.remove(&key))
            .map_or(0, |since| since.elapsed().as_millis() as u64);
        // B-175: a build that skipped tests makes an entry that another build would restore,
        // skipping tests that never ran: it stays with the extension's own cache.
        if let Some(property) = self.skips_tests.lock().ok().and_then(|s| s.clone()) {
            return self.log(Entry {
                rustc_ms: cost_ms,
                note: Some(format!(
                    "not kept: Maven skips tests in this run ({property})"
                )),
                ..maven_entry(
                    Outcome::BuiltNotStored(NotStored::StoreSkipped),
                    &key,
                    &name,
                )
            });
        }
        let store = Store::new(&self.current.store);
        let at = SystemTime::now();
        // B-173: an entry whose files hold a folder of this machine stays with the extension's
        // own cache.
        if let Some(local) = self.stays_here(checked, files) {
            let mut entry = Entry {
                rustc_ms: cost_ms,
                ..maven_entry(
                    Outcome::BuiltNotStored(NotStored::StoreSkipped),
                    &key,
                    &name,
                )
            };
            entry.maven = Some(MavenEntry { local: Some(local) });
            return self.log(entry);
        }
        if store.suspended_at(&key, at).is_some() {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..maven_entry(Outcome::BuiltNotStored(NotStored::Suspended), &key, &name)
            });
        }
        let floor = u64::from(self.current.min_free_gb).saturating_mul(1 << 30);
        if floor > 0 && store.free_bytes().is_ok_and(|free| free < floor) {
            return self.log(Entry {
                rustc_ms: cost_ms,
                ..maven_entry(Outcome::BuiltNotStored(NotStored::LowSpace), &key, &name)
            });
        }
        let stored_bytes =
            (checked.canonical.len() + files.iter().map(|(_, b)| b.len()).sum::<usize>()) as u64;
        // A block there already: one only downloaded gives way to this build, which is from
        // then on built here (B-32). Other bytes are no sign against it: a jar keeps its files'
        // dates unless the build makes it reproducible (B-175).
        if let Ok(Some(block)) = store.block(&key) {
            let differed = !same_files(&block, checked, files);
            let downloaded = store.origin(&key).is_some_and(|o| o.downloaded_only());
            let took =
                downloaded && self.publish(&store, &key, module, checked, files, cost_ms, at);
            if took {
                self.tell(&Request::ToolBuilt {
                    kind: Kind::Maven,
                    key,
                    cost_ms,
                });
            }
            return self.log(Entry {
                found: true,
                differed,
                rustc_ms: cost_ms,
                ..maven_entry(Outcome::BuiltAndStored, &key, &name)
            });
        }
        if self.publish(&store, &key, module, checked, files, cost_ms, at) {
            let _ = store.set_step_cost(&key, cost_ms, at);
            self.tell(&Request::ToolBuilt {
                kind: Kind::Maven,
                key,
                cost_ms,
            });
            self.log(Entry {
                rustc_ms: cost_ms,
                stored_bytes,
                ..maven_entry(Outcome::BuiltAndStored, &key, &name)
            });
        } else {
            self.log(Entry {
                rustc_ms: cost_ms,
                ..maven_entry(
                    Outcome::BuiltNotStored(NotStored::StoreRefused),
                    &key,
                    &name,
                )
            });
        }
    }

    /// B-173: why an entry keeps to this machine: its `buildinfo.xml` or a file it names holds
    /// the checkout's folder, Maven's local repository, the store's folder or the home folder, as
    /// bytes anywhere.
    pub fn stays_here(&self, checked: &Checked, files: &[(String, Vec<u8>)]) -> Option<Local> {
        let run = self.current.maven.clone().unwrap_or_default();
        let mut folders: Vec<(Local, String)> = run
            .roots
            .iter()
            .map(|root| (Local::Checkout, root.clone()))
            .collect();
        if let Some(repository) = &run.repository {
            folders.push((Local::Repository, repository.clone()));
        }
        folders.push((
            Local::Store,
            self.current.store.to_string_lossy().into_owned(),
        ));
        if let Some(home) = &self.current.home {
            folders.push((Local::Home, home.clone()));
        }
        folders
            .into_iter()
            .filter(|(_, folder)| folder.trim_end_matches('/').len() > 1)
            .find(|(_, folder)| {
                let needle = folder.trim_end_matches('/').as_bytes();
                std::iter::once(checked.canonical.as_slice())
                    .chain(files.iter().map(|(_, bytes)| bytes.as_slice()))
                    .any(|bytes| memchr::memmem::find(bytes, needle).is_some())
            })
            .map(|(local, _)| local)
    }

    /// Publishes the entry as the block of `key`, built here by this run (B-32), in place of one
    /// only downloaded. Whether it is stored.
    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        store: &Store,
        key: &Digest,
        module: &Module,
        checked: &Checked,
        files: &[(String, Vec<u8>)],
        cost_ms: u64,
        at: SystemTime,
    ) -> bool {
        let share = self.current.share.relay();
        let description = json!({
            "schema": crate::key::BLOCK_SCHEMA,
            "kind": Kind::Maven.word(),
            "step": key,
            "class": Class::Library.as_str(),
            "group": module.group,
            "artifact": module.artifact,
            "checksum": module.checksum,
            "module": module.name(),
            "hash": checked.hash_function,
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
        let Some(temp) = self.write_temp(key, checked, files) else {
            return false;
        };
        let new: Vec<NewFile> = std::iter::once(BUILDINFO.to_string())
            .chain(files.iter().map(|(name, _)| name.clone()))
            .map(|name| NewFile {
                source: temp.join(&name),
                name,
            })
            .collect();
        let stored = store
            .replace(key, &description, &new, &origin, at, |there, _| {
                there.is_some_and(Origin::downloaded_only)
            })
            .is_ok_and(|replaced| replaced == crate::store::Replaced::Stored);
        let _ = std::fs::remove_dir_all(&temp);
        stored
    }

    /// The entry's files in a folder of their own in the session's, to publish from.
    fn write_temp(
        &self,
        key: &Digest,
        checked: &Checked,
        files: &[(String, Vec<u8>)],
    ) -> Option<PathBuf> {
        let dir = self.current.folder.join("maven-entries").join(format!(
            "{}.{}.{:?}",
            key.hex(),
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).ok()?;
        let written = std::fs::write(dir.join(BUILDINFO), &checked.canonical).is_ok()
            && files
                .iter()
                .all(|(name, bytes)| std::fs::write(dir.join(name), bytes).is_ok());
        if !written {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        Some(dir)
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

/// The module's entry, once its `buildinfo.xml` and every file it names have come: taken out of
/// `pending`, checked (B-172). `None` while a piece is missing.
#[allow(clippy::type_complexity)]
fn take_whole(
    pending: &mut Pending,
    module: &Module,
) -> Option<Result<(Checked, Vec<(String, Vec<u8>)>), String>> {
    let parts = pending.modules.get(module)?;
    let buildinfo = parts.buildinfo.as_ref()?;
    let checked = match buildinfo::check(buildinfo, module) {
        Ok(checked) => checked,
        Err(why) => {
            let dropped = pending.modules.remove(module).map_or(0, |p| p.bytes());
            pending.bytes = pending.bytes.saturating_sub(dropped);
            return Some(Err(why));
        }
    };
    if !checked
        .files
        .iter()
        .all(|named| parts.files.contains_key(&named.name))
    {
        return None;
    }
    let mut parts = pending.modules.remove(module)?;
    pending.bytes = pending.bytes.saturating_sub(parts.bytes());
    let mut files = Vec::with_capacity(checked.files.len());
    for named in &checked.files {
        let bytes = parts.files.remove(&named.name)?;
        if bytes.len() as u64 != named.size {
            return Some(Err(format!(
                "{} isn't the size its buildinfo.xml gives",
                named.name
            )));
        }
        files.push((named.name.clone(), bytes));
    }
    Some(Ok((checked, files)))
}

/// B-172: the block's canonical `buildinfo.xml`, when the block is whole: its first file a
/// canonical `buildinfo.xml` of its module, then exactly the files that names, each of the size
/// it gives, and every file of the size and digest its record holds.
fn whole(block: &Block) -> Option<Vec<u8>> {
    let module = Module {
        group: block.description["group"].as_str()?.to_string(),
        artifact: block.description["artifact"].as_str()?.to_string(),
        checksum: block.description["checksum"].as_str()?.to_string(),
    };
    let (first, rest) = block.files.split_first()?;
    let buildinfo = std::fs::read(&first.path).ok()?;
    let checked = buildinfo::check(&buildinfo, &module).ok()?;
    if first.name != BUILDINFO
        || !checked.is_canonical(&buildinfo)
        || rest.len() != checked.files.len()
        || !rest
            .iter()
            .zip(&checked.files)
            .all(|(file, named)| file.name == named.name && file.size == named.size)
    {
        return None;
    }
    let intact = |file: &crate::store::BlockFile, bytes: &[u8]| {
        bytes.len() as u64 == file.size && Digest::of(bytes) == file.sha256
    };
    if !intact(first, &buildinfo) {
        return None;
    }
    for file in rest {
        let bytes = std::fs::read(&file.path).ok()?;
        if !intact(file, &bytes) {
            return None;
        }
    }
    Some(buildinfo)
}

/// Whether the block holds the same files as the entry.
fn same_files(block: &Block, checked: &Checked, files: &[(String, Vec<u8>)]) -> bool {
    let ours = std::iter::once((BUILDINFO, checked.canonical.as_slice()))
        .chain(files.iter().map(|(n, b)| (n.as_str(), b.as_slice())));
    block.files.len() == files.len() + 1
        && block
            .files
            .iter()
            .zip(ours)
            .all(|(file, (name, bytes))| file.name == name && file.sha256 == Digest::of(bytes))
}

/// A Maven module's line in the run's log: the module by its `group:artifact`.
fn maven_entry(outcome: Outcome, key: &Digest, module: &str) -> Entry {
    Entry {
        xcode: None,
        outcome,
        class: Some(Class::Library),
        crate_name: module.to_string(),
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
        gradle: None,
        maven: Some(MavenEntry::default()),
        tsc: None,
    }
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests;
