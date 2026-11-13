//! B-26 to B-30, B-32 to B-34: the relay, the part of the `hushrun run` process that talks to the
//! org's shared store of build blocks for the wrappers. One per process, started by the first
//! check that shares; it sends `open` and `close` once for each check, since a manifest belongs
//! to a check. The wrappers reach it over a Unix socket in their session's folder and stay free of
//! network code.
//!
//! What it holds, each in one place:
//!
//! - **Its phase** ([`Phase`]): opening, answering, resting (three failures in a row: every
//!   waiting wrapper is answered "none" at once, and every new one for 30 seconds), stopped (the
//!   org doesn't share, the server can't be reached, or it refused the caller mid-run). Moved by
//!   [`State::phase_now`], `open`'s answer, [`State::failed`] and [`State::stop`] only.
//! - **What the run shares** ([`Sharing`]): `hushrun_build::share::what_a_run_does`, asked again
//!   once `open` answers.
//! - **Each download** ([`Download`]): queued, downloading, arrived (a sealed pack in
//!   `incoming/<run>/`), published, dropped. Published only to answer a wrapper still waiting for
//!   it, never in shared `observe`.
//! - **Each upload**: its marker in the store's outbox (`hushrun_build::outbox`), moved by
//!   `upload.rs`.
//!
//! Every failure ends in rustc running as asked (B-19): an answer of "none" is a compile.

mod credential;
mod http;
mod pack;
mod socket;
mod upload;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use hushrun_build::model::{Class as LocalClass, Mode};
use hushrun_build::origin::{Identity, Origin, Vouch};
use hushrun_build::outbox::{self, Marker, Outbox};
use hushrun_build::relay::{Answer, Problem, Request};
use hushrun_build::session::{Entry, Share};
use hushrun_build::share::{Caller, Org, Sharing, what_a_run_does};
use hushrun_build::store::Store;
use hushrun_core::Digest;
use hushrun_core::blocks::{
    self, CloseRequest, Counts, Found, Hex32, HintsLookupRequest, HintsLookupResponse,
    LookupRequest, LookupResponse, OpenRequest, Pair, Policy, ReportRequest,
};

use self::http::{Client, Failure};

/// The kind of the blocks this relay shares: the rustc adapter, its keys' version.
pub(crate) const KIND: &str = "rustc/1";

/// How long the relay's numbers are: the defaults of B-26, which tests shorten.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tuning {
    /// `open` gets this long, and one more try.
    pub(crate) open: Duration,
    /// A lookup a wrapper waits on gets at most this long.
    pub(crate) lookup: Duration,
    /// The most a fetch's cost is taken to be.
    pub(crate) most_cost: Duration,
    /// Three failures in a row rest the relay this long.
    pub(crate) rest: Duration,
    /// How long uploads in flight get when the command ends, on a person's machine.
    pub(crate) end_wait: Duration,
    /// How long `close` gets.
    pub(crate) close: Duration,
    /// Downloads at once.
    pub(crate) downloads: usize,
    /// How often a waiting fetch looks at its progress.
    pub(crate) tick: Duration,
    /// Bytes of a pack held in memory while it is made; more go to a file.
    pub(crate) spill: usize,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            open: Duration::from_secs(10),
            lookup: Duration::from_secs(10),
            most_cost: Duration::from_secs(60),
            rest: Duration::from_secs(30),
            end_wait: Duration::from_secs(5),
            close: Duration::from_secs(5),
            downloads: 8,
            tick: Duration::from_millis(20),
            spill: pack::IN_MEMORY,
        }
    }
}

/// How many failures in a row rest the relay.
const FAILURES_TO_REST: u32 = 3;
/// How many of this machine's compiles, with a cost the server also knew, it takes to learn
/// the machine's speed against the uploaders'.
const SPEED_SAMPLES: usize = 3;
/// A fetch's expected finish adds one round trip of this much, until one is measured.
const ROUND_TRIP: Duration = Duration::from_millis(50);
/// The rate is known once this many bytes were measured over at least [`MEASURED_FOR`], or a
/// download finished.
const MEASURED_BYTES: u64 = 256 << 10;
const MEASURED_FOR: Duration = Duration::from_millis(100);
/// A download's unpacked size is taken as this many times its pack, for the free-space floor.
const UNPACKED_PER_PACKED: u64 = 4;

/// What starts a relay (B-26).
pub(crate) struct Config {
    pub(crate) store: Store,
    pub(crate) access: crate::results::BuildAccess,
    /// The run's repository, as a result names it.
    pub(crate) repository: Option<String>,
    /// The workspace's commit.
    pub(crate) commit: Option<String>,
    /// The target the run builds for.
    pub(crate) platform: String,
    /// The compiler cargo will call and the folder it runs in: its host, as `rustc -vV` says
    /// it, is the run's platform, asked in the background before `open`. `None`: `platform`.
    pub(crate) compiler: Option<(std::ffi::OsString, PathBuf)>,
    /// B-1's mode and the person's share setting, for this command (B-33).
    pub(crate) local: Mode,
    pub(crate) own: Mode,
    pub(crate) min_free_gb: u32,
    pub(crate) keep_days: u32,
    pub(crate) tuning: Tuning,
}

/// Why sharing stopped for the rest of the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// `open` said no, or what the run does says off.
    Off(String),
    /// The server couldn't be reached: downloaded blocks count whatever their date (B-34).
    Offline(String),
    /// The server refused the caller (401, 403, 404): markers stay, nothing more is asked.
    Refused(String),
}

/// The relay's phase in a run (B-26).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    /// `open` is under way: every request is answered "none".
    Opening,
    Answering,
    /// Three failures in a row: "none" to everyone until then.
    Resting(Instant),
    Stopped(Stop),
}

/// A download's state (B-30).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Download {
    Queued,
    Downloading,
    /// The sealed pack, checked against its row's size and SHA-256.
    Arrived(PathBuf),
    Published,
    Dropped,
}

/// One download: what the lookup said, its state, how far it got.
struct Fetch {
    found: Found,
    /// When the lookup that gave its link answered.
    looked_up: Instant,
    state: Download,
    bytes: u64,
    started: Option<Instant>,
    cancel: Arc<AtomicBool>,
    /// A wrapper is waiting for it.
    waited: bool,
}

/// The rate of the link: bytes over the time any download was running, partial and abandoned
/// downloads included (B-26).
#[derive(Default)]
struct Meter {
    bytes: u64,
    busy: Duration,
    running: usize,
    since: Option<Instant>,
    finished: bool,
}

impl Meter {
    fn start(&mut self, now: Instant) {
        if self.running == 0 {
            self.since = Some(now);
        }
        self.running += 1;
    }

    fn stop(&mut self, now: Instant, finished: bool) {
        self.running = self.running.saturating_sub(1);
        if self.running == 0
            && let Some(since) = self.since.take()
        {
            self.busy += now.saturating_duration_since(since);
        }
        self.finished |= finished;
    }

    /// Bytes a second, once measured.
    fn rate(&self, now: Instant) -> Option<f64> {
        let busy = self.busy
            + self
                .since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        let measured = self.finished || (self.bytes >= MEASURED_BYTES && busy >= MEASURED_FOR);
        (measured && !busy.is_zero()).then(|| self.bytes as f64 / busy.as_secs_f64())
    }
}

/// What `close` counts beyond the wrappers' log.
#[derive(Default, Clone)]
struct Tally {
    uploaded: u64,
    bytes_down: u64,
    bytes_up: u64,
    unused_downloads: u64,
    compared: u64,
    compared_others: u64,
    compared_differed: u64,
}

/// A check the relay serves.
struct Check {
    run: Hex32,
    check: Option<Hex32>,
    socket: socket::Bound,
    stop_serving: Arc<AtomicBool>,
}

struct State {
    phase: Phase,
    policy: Option<Policy>,
    sharing: Sharing,
    /// Lines for the person, each once.
    lines: Vec<String>,
    /// Lookups this run: found, with when, and addresses known missing.
    found: HashMap<Hex32, (Found, Instant)>,
    missing: HashSet<Hex32>,
    suspended: HashSet<Hex32>,
    /// What the server said this run of a downloaded block's outputs: served or not.
    vouched: HashMap<Hex32, Option<Hex32>>,
    /// Sealed hint lists, by hint address, from the prefetch.
    sealed_hints: HashMap<Hex32, Vec<Vec<u8>>>,
    hints_asked: HashSet<Hex32>,
    fetches: HashMap<Hex32, Fetch>,
    queue: VecDeque<Hex32>,
    meter: Meter,
    failures: u32,
    measuring: Option<Hex32>,
    /// This machine's compile time over the uploader's, per sample.
    speed: Vec<f64>,
    tally: Tally,
    /// Blocks built before `open` answered, decided once it has.
    pending: Vec<Request>,
    check: Option<Check>,
    /// The server's date in its last lookup's answer: what a block's entry is dated by (B-34).
    server_date_ms: Option<u64>,
    /// The PUTs in flight, which the end of the command may cut off (B-29).
    puts: HashMap<Digest, tokio::task::AbortHandle>,
    /// The upload loop of the check is running.
    uploading: bool,
    /// The run's platform, once asked.
    platform: Option<String>,
}

impl State {
    /// The phase now: a rest whose time is up is over.
    fn phase_now(&mut self, now: Instant) -> &Phase {
        if let Phase::Resting(until) = self.phase
            && now >= until
        {
            self.phase = Phase::Answering;
            self.failures = 0;
        }
        &self.phase
    }

    fn answering(&mut self, now: Instant) -> bool {
        matches!(self.phase_now(now), Phase::Answering)
    }

    /// One more failure; the third in a row rests the relay.
    fn failed(&mut self, now: Instant, rest: Duration) {
        self.failures += 1;
        if self.failures >= FAILURES_TO_REST && self.phase == Phase::Answering {
            self.phase = Phase::Resting(now + rest);
        }
    }

    fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// Stops sharing for the rest of the run, saying why once.
    fn stop(&mut self, stop: Stop) {
        if matches!(self.phase, Phase::Stopped(_)) {
            return;
        }
        let line = match &stop {
            Stop::Off(why) => format!("build: sharing is off for this run: {why}"),
            Stop::Offline(why) => {
                format!(
                    "build: sharing is off for this run: the shared store can't be reached ({why})"
                )
            }
            Stop::Refused(why) => {
                format!(
                    "build: sharing stopped for this run: the backend {why}; uploads wait for the next run"
                )
            }
        };
        self.say(line);
        self.phase = Phase::Stopped(stop);
    }

    fn say(&mut self, line: String) {
        if !self.lines.contains(&line) {
            self.lines.push(line);
        }
    }

    /// The machine's speed against the uploaders': their cost times this is the cost here.
    fn speed(&self) -> f64 {
        if self.speed.len() < SPEED_SAMPLES {
            return 1.0;
        }
        let mut sorted = self.speed.clone();
        sorted.sort_by(f64::total_cmp);
        sorted[sorted.len() / 2]
    }

    /// Bytes still to come before a fetch put at the front: the downloads running now.
    fn ahead(&self) -> u64 {
        self.fetches
            .values()
            .filter(|f| f.state == Download::Downloading)
            .map(|f| f.found.bytes.saturating_sub(f.bytes))
            .sum()
    }
}

/// The relay.
pub(crate) struct Relay {
    config: Config,
    by: Identity,
    client: Client,
    runtime: tokio::runtime::Runtime,
    state: Mutex<State>,
    /// The relay's own run: the name of its folders in the store.
    run: String,
    machine: Option<crate::machine::Machine>,
    /// The command ended: no new PUT starts.
    ending: AtomicBool,
    /// A marker was written: the upload loop looks now rather than at its next turn.
    wake: tokio::sync::Notify,
}

static RELAY: OnceLock<Arc<Relay>> = OnceLock::new();

impl Relay {
    /// The process's relay, started on first use (B-26: one per `hushrun run` process).
    pub(crate) fn get_or_start(make: impl FnOnce() -> Option<Config>) -> Option<Arc<Relay>> {
        if let Some(relay) = RELAY.get() {
            return Some(relay.clone());
        }
        let relay = Arc::new(Relay::start(make()?).ok()?);
        Some(RELAY.get_or_init(|| relay).clone())
    }

    /// The process's relay, when one started.
    pub(crate) fn running() -> Option<Arc<Relay>> {
        RELAY.get().cloned()
    }

    /// Starts a relay: its runtime, its folder in the store, and the outbox's reconciliation
    /// (claims of dead runs back to waiting, dead runs' folders gone).
    pub(crate) fn start(mut config: Config) -> Result<Relay, String> {
        let credential = config
            .access
            .credential
            .as_ref()
            .ok_or_else(|| "no credential to send".to_string())?;
        let client = Client::new(
            &config.access.base,
            credential::Credential::of(credential, &config.access.base),
        )?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|err| format!("starting the relay's runtime: {err}"))?;
        let run = outbox::run_name(SystemTime::now(), std::process::id());
        std::fs::create_dir_all(outbox::run_folder(&config.store, outbox::RELAYS, &run))
            .map_err(|err| format!("{}: {err}", config.store.root().display()))?;
        outbox::remove_dead_runs(&config.store);
        let by = Identity {
            host: config.access.host.clone(),
            org: config.access.org.clone(),
            credential: client.fingerprint(),
        };
        let machine = config.access.machine.take();
        let caller = Caller::Person { signed_in: true };
        let sharing = what_a_run_does(caller, config.local, config.own, Org::Shares(None)).sharing;
        Ok(Relay {
            by,
            client,
            runtime,
            run,
            machine,
            ending: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
            state: Mutex::new(State {
                phase: Phase::Opening,
                policy: None,
                sharing,
                lines: Vec::new(),
                found: HashMap::new(),
                missing: HashSet::new(),
                suspended: HashSet::new(),
                vouched: HashMap::new(),
                sealed_hints: HashMap::new(),
                hints_asked: HashSet::new(),
                fetches: HashMap::new(),
                queue: VecDeque::new(),
                meter: Meter::default(),
                failures: 0,
                measuring: None,
                speed: Vec::new(),
                tally: Tally::default(),
                pending: Vec::new(),
                check: None,
                server_date_ms: None,
                puts: HashMap::new(),
                uploading: false,
                platform: None,
            }),
            config,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The caller's kind for B-25's table.
    fn caller(&self) -> Caller {
        if self.config.access.agent {
            Caller::Agent { sandbox: false }
        } else {
            Caller::Person { signed_in: true }
        }
    }

    /// Begins a check (B-26): binds a socket in the session's folder, starts answering it, and
    /// sends `open` in the background, so cargo never waits for the network. The `Share` to
    /// write into the session; an error is the line to say, and the check shares nothing.
    pub(crate) fn begin_check(
        self: &Arc<Relay>,
        session_folder: &Path,
        check: Option<Hex32>,
        env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Share, String> {
        let bound = socket::bind(session_folder, env)
            .map_err(|why| format!("build: sharing is off for this run: {why}"))?;
        let path = bound.path.clone();
        let stop_serving = Arc::new(AtomicBool::new(false));
        #[cfg(unix)]
        {
            let listener = bound
                .listener
                .try_clone()
                .map_err(|err| format!("build: sharing is off for this run: {err}"))?;
            let relay = self.clone();
            let stop = stop_serving.clone();
            std::thread::spawn(move || relay.serve(listener, stop));
        }
        let mut run = [0u8; 32];
        random(&mut run);
        {
            let mut state = self.state();
            state.check = Some(Check {
                run: Hex32(run),
                check,
                socket: bound,
                stop_serving,
            });
            if !matches!(state.phase, Phase::Stopped(_)) {
                state.phase = Phase::Opening;
            }
        }
        let relay = self.clone();
        self.runtime.spawn(async move { relay.open(check).await });
        let relay = self.clone();
        self.runtime.spawn(async move { relay.uploads().await });
        Ok(Share {
            socket: path,
            run: self.run.clone(),
            by: self.by.clone(),
            repository: self.config.repository.clone().unwrap_or_default(),
        })
    }

    /// Accepts the wrappers' connections: one request each, each in its own thread, answered
    /// within the wrapper's wait.
    #[cfg(unix)]
    fn serve(self: Arc<Relay>, listener: std::os::unix::net::UnixListener, stop: Arc<AtomicBool>) {
        for stream in listener.incoming() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(stream) = stream else { continue };
            let relay = self.clone();
            std::thread::spawn(move || relay.connection(stream));
        }
    }

    #[cfg(unix)]
    fn connection(self: Arc<Relay>, stream: std::os::unix::net::UnixStream) {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        if BufReader::new(&stream)
            .take(1 << 20)
            .read_line(&mut line)
            .is_err()
        {
            return;
        }
        let answer = match serde_json::from_str::<Request>(line.trim_end()) {
            Ok(request) => {
                let relay = self.clone();
                self.runtime
                    .block_on(async move { relay.answer(request).await })
            }
            Err(_) => Answer::None,
        };
        let mut bytes = serde_json::to_vec(&answer).unwrap_or_default();
        bytes.push(b'\n');
        let _ = (&stream).write_all(&bytes);
    }

    /// One wrapper's request.
    pub(crate) async fn answer(self: Arc<Relay>, request: Request) -> Answer {
        match request {
            Request::Hints { step } => self.hints(step).await,
            Request::Fetch {
                step,
                keys,
                class,
                local_ms,
            } => self.fetch(step, keys, class, local_ms).await,
            Request::Vouch { key, outputs, .. } => self.vouch(key, outputs).await,
            Request::Built { .. } => {
                self.built(request);
                Answer::Ok
            }
            Request::Report { key, problem } => {
                self.report_problem(key, problem).await;
                Answer::Ok
            }
        }
    }

    fn address(key: &Digest) -> Hex32 {
        blocks::address(KIND, key.as_bytes())
    }

    // --- open and the prefetch ----------------------------------------------------------

    /// The run's platform: the host of the compiler cargo will call (`rustc -vV`'s `host:`), so
    /// that a hushrun built for one target and run under emulation names the compiler's own;
    /// asked once, off cargo's path. When it can't be asked, the target hushrun was built for.
    async fn platform(&self) -> String {
        if let Some(known) = self.state().platform.clone() {
            return known;
        }
        let asked = match self.config.compiler.clone() {
            Some((rustc, folder)) => self
                .runtime
                .spawn_blocking(move || compiler_host(&rustc, &folder))
                .await
                .ok()
                .flatten(),
            None => None,
        };
        let platform = asked
            .filter(|p| blocks::is_platform(p))
            .unwrap_or_else(|| self.config.platform.clone());
        self.state().platform = Some(platform.clone());
        platform
    }

    async fn open(self: Arc<Relay>, check: Option<Hex32>) {
        let platform = self.platform().await;
        let body = OpenRequest {
            kind: KIND.to_string(),
            repository: self.config.repository.clone(),
            commit: self.config.commit.clone(),
            check,
            platform,
        };
        let mut answer = Err(Failure::Unreachable(String::new()));
        for _ in 0..2 {
            answer = self
                .client
                .post::<_, Policy>(blocks::OPEN_PATH, &body, self.config.tuning.open, None)
                .await;
            if !matches!(answer, Err(Failure::Unreachable(_) | Failure::Failed(_))) {
                break;
            }
        }
        let manifest = {
            let mut state = self.state();
            match answer {
                Err(Failure::Refused(404 | 405, _)) => {
                    state.stop(Stop::Off("the backend doesn't share build blocks".into()));
                    return;
                }
                Err(Failure::Refused(status, code)) => {
                    let why = Failure::Refused(status, code).line();
                    state.stop(Stop::Refused(why));
                    return;
                }
                Err(failure) => {
                    state.stop(Stop::Offline(failure.line()));
                    return;
                }
                Ok(answered) => {
                    let policy = answered.body;
                    let org = if policy.sharing {
                        Org::Shares(Some(match policy.mode {
                            Some(blocks::Mode::On) => Mode::On,
                            _ => Mode::Observe,
                        }))
                    } else {
                        Org::NotSharing
                    };
                    let does =
                        what_a_run_does(self.caller(), self.config.local, self.config.own, org);
                    state.sharing = does.sharing;
                    let manifest = policy.manifest.clone();
                    // A CI job on a branch its repository lists that isn't trusted: why, once.
                    if let Some(untrusted) = policy.untrusted {
                        state.say(format!(
                            "build: this job reads the repository's trusted blocks and writes none: {}",
                            untrusted_words(untrusted)
                        ));
                    }
                    state.policy = Some(policy);
                    match does.sharing {
                        Sharing::Off(off) => {
                            state.stop(Stop::Off(off.reason().to_string()));
                            return;
                        }
                        Sharing::Observe | Sharing::On => {
                            if !matches!(state.phase, Phase::Stopped(_)) {
                                state.phase = Phase::Answering;
                            }
                        }
                    }
                    let pending = std::mem::take(&mut state.pending);
                    drop(state);
                    for built in pending {
                        self.built(built);
                    }
                    manifest
                }
            }
        };
        self.clone().prefetch(manifest).await;
    }

    /// B-30: from the manifest `open` returned, drop every pair the store already holds the
    /// block or the hint of, ask for the rest's hints and look up the rest's blocks, one request
    /// each, and queue the libraries' downloads in order. Programs wait to be asked for.
    async fn prefetch(self: Arc<Relay>, manifest: Vec<Pair>) {
        if manifest.is_empty() {
            return;
        }
        let store = self.config.store.clone();
        let (held_blocks, held_hints) = self
            .runtime
            .spawn_blocking(move || {
                let blocks: HashSet<Hex32> = store.keys().iter().map(Relay::address).collect();
                let hints: HashSet<Hex32> = store.steps().iter().map(Relay::address).collect();
                (blocks, hints)
            })
            .await
            .unwrap_or_default();
        let limit = self
            .state()
            .policy
            .as_ref()
            .map_or(blocks::MOST_LOOKUP, |p| p.limits.lookup as usize)
            .max(1);
        let hints: Vec<Hex32> = dedup(
            manifest
                .iter()
                .map(Pair::hint)
                .filter(|a| !held_hints.contains(a)),
        );
        let wanted: Vec<Hex32> = dedup(
            manifest
                .iter()
                .map(Pair::block)
                .filter(|a| !held_blocks.contains(a)),
        );
        for batch in hints.chunks(limit) {
            self.lookup_hints(batch).await;
        }
        for batch in wanted.chunks(limit) {
            let found = self.lookup(batch, true).await;
            let mut state = self.state();
            for found in found.into_iter().flatten() {
                if found.class == blocks::Class::Library
                    && !state.fetches.contains_key(&found.address)
                {
                    state.queue.push_back(found.address);
                    let looked = state
                        .found
                        .get(&found.address)
                        .map_or_else(Instant::now, |f| f.1);
                    state.fetches.insert(
                        found.address,
                        Fetch {
                            found,
                            looked_up: looked,
                            state: Download::Queued,
                            bytes: 0,
                            started: None,
                            cancel: Arc::new(AtomicBool::new(false)),
                            waited: false,
                        },
                    );
                }
            }
        }
        self.pump();
    }

    async fn lookup_hints(&self, addresses: &[Hex32]) {
        let body = HintsLookupRequest {
            kind: KIND.to_string(),
            addresses: addresses.to_vec(),
            repository: self.config.repository.clone(),
            commit: self.config.commit.clone(),
        };
        let answer = self
            .client
            .post::<_, HintsLookupResponse>(
                blocks::HINTS_LOOKUP_PATH,
                &body,
                self.config.tuning.lookup,
                None,
            )
            .await;
        let mut state = self.state();
        match answer {
            Ok(answered) => {
                for address in addresses {
                    state.hints_asked.insert(*address);
                }
                for found in answered.body.hints {
                    state.sealed_hints.insert(
                        found.address,
                        found.lists.into_iter().map(|l| l.0).collect(),
                    );
                }
            }
            Err(failure) => self.request_failed(&mut state, failure),
        }
    }

    /// A lookup of `addresses` (B-26: "this is the use"); what it found, or `None` when it
    /// failed.
    async fn lookup(&self, addresses: &[Hex32], fetch: bool) -> Option<Vec<Found>> {
        let body = LookupRequest {
            kind: KIND.to_string(),
            addresses: addresses.to_vec(),
            fetch,
            repository: self.config.repository.clone(),
            machine: self.machine.as_ref().map(crate::machine::Machine::keyid),
            commit: self.config.commit.clone(),
        };
        let answer = self
            .client
            .post::<_, LookupResponse>(blocks::LOOKUP_PATH, &body, self.config.tuning.lookup, None)
            .await;
        let mut state = self.state();
        match answer {
            Ok(answered) => {
                let now = Instant::now();
                let found_now: HashSet<Hex32> =
                    answered.body.found.iter().map(|f| f.address).collect();
                for address in addresses {
                    if !found_now.contains(address) {
                        state.missing.insert(*address);
                    }
                }
                for address in &answered.body.suspended {
                    state.suspended.insert(*address);
                }
                for found in &answered.body.found {
                    state.vouched.insert(found.address, Some(found.outputs));
                    if fetch {
                        state.found.insert(found.address, (found.clone(), now));
                    }
                }
                if answered.date_ms.is_some() {
                    state.server_date_ms = answered.date_ms;
                }
                Some(answered.body.found)
            }
            Err(failure) => {
                self.request_failed(&mut state, failure);
                None
            }
        }
    }

    /// What a failed request does to the run (B-26): a refusal stops sharing; a server that
    /// can't be reached, or fails, counts as a failure.
    fn request_failed(&self, state: &mut State, failure: Failure) {
        match failure {
            Failure::Refused(_, _) => state.stop(Stop::Refused(failure.line())),
            Failure::Unreachable(_) | Failure::Failed(_) => {
                state.failed(Instant::now(), self.config.tuning.rest);
            }
        }
    }

    // --- hints ---------------------------------------------------------------------------

    /// B-30: the shared store's hints of a first key, opened with it and written where
    /// downloaded hints live.
    async fn hints(self: Arc<Relay>, step: Digest) -> Answer {
        let address = Relay::address(&step);
        let known = {
            let mut state = self.state();
            if !state.answering(Instant::now()) {
                return Answer::None;
            }
            if state.hints_asked.contains(&address) {
                Some(state.sealed_hints.remove(&address).unwrap_or_default())
            } else {
                None
            }
        };
        let sealed = match known {
            Some(sealed) => sealed,
            None => {
                self.lookup_hints(&[address]).await;
                self.state()
                    .sealed_hints
                    .remove(&address)
                    .unwrap_or_default()
            }
        };
        let mut count = 0u32;
        let now = SystemTime::now();
        for list in sealed {
            if let Some(hint) = blocks::pack::open_hint(step.as_bytes(), &list)
                && self.config.store.add_shared_hint(&step, &hint, now).is_ok()
            {
                count += 1;
            }
        }
        if count == 0 {
            Answer::None
        } else {
            Answer::Hints { count }
        }
    }

    // --- fetches -------------------------------------------------------------------------

    /// B-26's "never slower than building", for a wrapper waiting on one of `keys`.
    async fn fetch(
        self: Arc<Relay>,
        step: Digest,
        keys: Vec<Digest>,
        class: LocalClass,
        local_ms: Option<u64>,
    ) -> Answer {
        let _ = class;
        {
            let mut state = self.state();
            if !state.answering(Instant::now()) || state.sharing != Sharing::On {
                return Answer::None;
            }
        }
        let addresses: Vec<(Digest, Hex32)> =
            keys.iter().map(|key| (*key, Relay::address(key))).collect();
        // What the run knows already, else one lookup for all of them.
        let unknown: Vec<Hex32> = {
            let state = self.state();
            addresses
                .iter()
                .map(|(_, a)| *a)
                .filter(|a| {
                    !state.missing.contains(a)
                        && !state.fetches.contains_key(a)
                        && !state.found.contains_key(a)
                })
                .collect()
        };
        if !unknown.is_empty() {
            self.lookup(&unknown, true).await;
        }
        let chosen = {
            let state = self.state();
            addresses.iter().find(|(_, a)| {
                !state.suspended.contains(a)
                    && (state.fetches.contains_key(a) || state.found.contains_key(a))
            })
        }
        .copied();
        let Some((key, address)) = chosen else {
            return Answer::None;
        };
        self.wait_for(key, step, address, local_ms).await
    }

    /// Starts or joins the download of `address` and waits for it within half its cost.
    async fn wait_for(
        self: Arc<Relay>,
        key: Digest,
        step: Digest,
        address: Hex32,
        local_ms: Option<u64>,
    ) -> Answer {
        let tuning = self.config.tuning;
        let started = Instant::now();
        // A pack that arrived before anyone asked: open it now.
        let arrived = matches!(
            self.state().fetches.get(&address),
            Some(Fetch {
                state: Download::Arrived(_),
                ..
            })
        );
        if arrived {
            return self.publish(key, step, address).await;
        }
        let budget = {
            let mut state = self.state();
            let now = Instant::now();
            if !state.fetches.contains_key(&address) {
                let Some((found, looked)) = state.found.get(&address).cloned() else {
                    return Answer::None;
                };
                state.fetches.insert(
                    address,
                    Fetch {
                        found,
                        looked_up: looked,
                        state: Download::Queued,
                        bytes: 0,
                        started: None,
                        cancel: Arc::new(AtomicBool::new(false)),
                        waited: false,
                    },
                );
            }
            let speed = state.speed();
            let Some(fetch) = state.fetches.get(&address) else {
                return Answer::None;
            };
            if matches!(fetch.state, Download::Dropped | Download::Published) {
                return Answer::None;
            }
            // The cost: the smallest of the uploader's (scaled by this machine's speed), this
            // machine's own last compile of the first key, and the ceiling.
            let theirs = Duration::from_millis((f64::from(fetch.found.cost_ms) * speed) as u64);
            let mut cost = theirs.min(tuning.most_cost);
            if let Some(ms) = local_ms {
                cost = cost.min(Duration::from_millis(ms));
            }
            let budget = cost / 2;
            let size = fetch.found.bytes.saturating_sub(fetch.bytes);
            match state.meter.rate(now) {
                None => {
                    // No rate yet: the first fetch asked for runs to its end as the
                    // measurement; every other one is "none" until a rate exists.
                    if state.measuring.is_some_and(|m| m != address) {
                        return Answer::None;
                    }
                    state.measuring = Some(address);
                }
                Some(rate) => {
                    let ahead = state.ahead();
                    let expected =
                        Duration::from_secs_f64((ahead + size) as f64 / rate.max(1.0)) + ROUND_TRIP;
                    if expected >= budget {
                        return Answer::None;
                    }
                }
            }
            if let Some(fetch) = state.fetches.get_mut(&address) {
                fetch.waited = true;
            }
            if !state.queue.contains(&address)
                && state
                    .fetches
                    .get(&address)
                    .is_some_and(|f| f.state == Download::Queued)
            {
                state.queue.push_front(address);
            } else if let Some(at) = state.queue.iter().position(|a| *a == address) {
                state.queue.remove(at);
                state.queue.push_front(address);
            }
            budget
        };
        self.pump();
        loop {
            tokio::time::sleep(tuning.tick).await;
            let arrived = matches!(
                self.state().fetches.get(&address),
                Some(Fetch {
                    state: Download::Arrived(_),
                    ..
                })
            );
            if arrived {
                return self.publish(key, step, address).await;
            }
            let now = Instant::now();
            let mut state = self.state();
            if !state.answering(now) {
                return Answer::None;
            }
            let measuring = state.measuring == Some(address);
            let rate = state.meter.rate(now);
            let ahead = state.ahead();
            let Some(fetch) = state.fetches.get(&address) else {
                return Answer::None;
            };
            match &fetch.state {
                Download::Arrived(_) => continue,
                Download::Dropped | Download::Published => return Answer::None,
                Download::Queued | Download::Downloading => {}
            }
            let elapsed = now.saturating_duration_since(started);
            let remaining = fetch.found.bytes.saturating_sub(fetch.bytes);
            // Given up as soon as the projected finish passes half the cost.
            let projected = match rate {
                Some(rate) => {
                    let behind = if fetch.state == Download::Downloading {
                        remaining
                    } else {
                        ahead + remaining
                    };
                    elapsed + Duration::from_secs_f64(behind as f64 / rate.max(1.0))
                }
                None => elapsed,
            };
            if projected > budget || elapsed >= budget {
                if let Some(fetch) = state.fetches.get_mut(&address) {
                    fetch.waited = false;
                }
                if !measuring {
                    if let Some(fetch) = state.fetches.get(&address) {
                        fetch.cancel.store(true, Ordering::Relaxed);
                    }
                    state.failed(now, tuning.rest);
                }
                return Answer::None;
            }
        }
    }

    /// Starts queued downloads while fewer than the most are running and the disk has room.
    fn pump(self: &Arc<Relay>) {
        let mut starts = Vec::new();
        {
            let mut state = self.state();
            if matches!(state.phase, Phase::Stopped(_)) {
                return;
            }
            let free = self.config.store.free_bytes().unwrap_or(u64::MAX);
            let floor = u64::from(self.config.min_free_gb).saturating_mul(1 << 30);
            let mut running = state
                .fetches
                .values()
                .filter(|f| f.state == Download::Downloading)
                .count();
            let mut planned = 0u64;
            while running < self.config.tuning.downloads {
                let Some(address) = state.queue.pop_front() else {
                    break;
                };
                let Some(fetch) = state.fetches.get_mut(&address) else {
                    continue;
                };
                if fetch.state != Download::Queued {
                    continue;
                }
                // B-22: no download that would take the disk below the floor.
                let needs = fetch.found.bytes.saturating_mul(1 + UNPACKED_PER_PACKED);
                if free.saturating_sub(planned).saturating_sub(needs) < floor {
                    fetch.state = Download::Dropped;
                    continue;
                }
                planned = planned.saturating_add(needs);
                fetch.state = Download::Downloading;
                fetch.started = Some(Instant::now());
                starts.push((
                    address,
                    fetch.found.clone(),
                    fetch.looked_up,
                    fetch.cancel.clone(),
                ));
                running += 1;
            }
            let now = Instant::now();
            for _ in &starts {
                state.meter.start(now);
            }
        }
        for (address, found, looked_up, cancel) in starts {
            let relay = self.clone();
            self.runtime
                .spawn(async move { relay.download(address, found, looked_up, cancel).await });
        }
    }

    /// One download into `incoming/<run>/`, checked against its row's size and SHA-256.
    async fn download(
        self: Arc<Relay>,
        address: Hex32,
        mut found: Found,
        looked_up: Instant,
        cancel: Arc<AtomicBool>,
    ) {
        use sha2::Digest as _;
        // A link older than its life gets a new one.
        let expires = found.get.as_ref().map_or(Duration::ZERO, |l| {
            Duration::from_secs(u64::from(l.expires_in_seconds))
        });
        if looked_up.elapsed() + Duration::from_secs(30) >= expires
            && let Some(again) = self
                .lookup(&[address], true)
                .await
                .and_then(|f| f.into_iter().next())
        {
            found = again;
        }
        let dir = outbox::run_folder(&self.config.store, outbox::INCOMING, &self.run);
        let part = dir.join(format!("{address}.part"));
        let done = dir.join(format!("{address}.pack"));
        let outcome: Result<(), Failure> = async {
            let link = found
                .get
                .as_ref()
                .ok_or_else(|| Failure::Failed("no link".into()))?;
            let request = self
                .client
                .link(link)
                .ok_or_else(|| Failure::Failed("a link this version can't follow".into()))?;
            let mut response = request
                .send()
                .await
                .map_err(|err| Failure::Unreachable(http::chain(&err)))?;
            if !response.status().is_success() {
                return Err(Failure::Failed(format!(
                    "the store answered {}",
                    response.status()
                )));
            }
            std::fs::create_dir_all(&dir).map_err(|err| Failure::Failed(err.to_string()))?;
            let mut file =
                std::fs::File::create(&part).map_err(|err| Failure::Failed(err.to_string()))?;
            let mut hash = sha2::Sha256::new();
            let mut got = 0u64;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Failure::Failed("given up".into()));
                }
                let chunk = response
                    .chunk()
                    .await
                    .map_err(|err| Failure::Unreachable(http::chain(&err)))?;
                let Some(chunk) = chunk else { break };
                got += chunk.len() as u64;
                if got > found.bytes {
                    return Err(Failure::Failed("longer than its row says".into()));
                }
                hash.update(&chunk);
                std::io::Write::write_all(&mut file, &chunk)
                    .map_err(|err| Failure::Failed(err.to_string()))?;
                let mut state = self.state();
                state.meter.bytes += chunk.len() as u64;
                state.tally.bytes_down += chunk.len() as u64;
                if let Some(fetch) = state.fetches.get_mut(&address) {
                    fetch.bytes = got;
                }
            }
            let sha = Hex32(hash.finalize().into());
            if got != found.bytes || sha != found.pack_sha256 {
                return Err(Failure::Failed("unreadable".into()));
            }
            std::fs::rename(&part, &done).map_err(|err| Failure::Failed(err.to_string()))?;
            Ok(())
        }
        .await;
        let now = Instant::now();
        let unreadable = matches!(&outcome, Err(Failure::Failed(why)) if why == "unreadable" || why.starts_with("longer"));
        {
            let mut state = self.state();
            state.meter.stop(now, outcome.is_ok());
            if state.measuring == Some(address) {
                state.measuring = None;
            }
            match &outcome {
                Ok(()) => {
                    state.succeeded();
                    if let Some(fetch) = state.fetches.get_mut(&address) {
                        fetch.state = Download::Arrived(done.clone());
                    }
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&part);
                    let cancelled = cancel.load(Ordering::Relaxed);
                    if let Some(fetch) = state.fetches.get_mut(&address) {
                        fetch.state = Download::Dropped;
                    }
                    if !cancelled {
                        state.failed(now, self.config.tuning.rest);
                    }
                }
            }
        }
        if unreadable {
            self.report(address, &found.scope, blocks::Problem::Unreadable)
                .await;
        }
        self.pump();
    }

    /// B-32: opens an arrived pack with the key the wrapper brought and publishes it into the
    /// store, recorded as downloaded with the server's word for it. Only for a wrapper that is
    /// waiting, and never in shared `observe`.
    async fn publish(self: Arc<Relay>, key: Digest, step: Digest, address: Hex32) -> Answer {
        let (path, found) = {
            let state = self.state();
            match state.fetches.get(&address) {
                Some(Fetch {
                    state: Download::Arrived(path),
                    found,
                    ..
                }) => (path.clone(), found.clone()),
                _ => return Answer::None,
            }
        };
        let relay = self.clone();
        let opened = self
            .runtime
            .spawn_blocking(move || relay.open_and_publish(&key, &step, &path, &found))
            .await
            .unwrap_or(Err(false));
        let scope = {
            let mut state = self.state();
            let fetch = state.fetches.get_mut(&address);
            fetch.map(|fetch| {
                fetch.state = if opened.is_ok() {
                    Download::Published
                } else {
                    Download::Dropped
                };
                fetch.found.scope.clone()
            })
        };
        match opened {
            Ok(()) => Answer::Ready { key },
            Err(unreadable) => {
                if unreadable && let Some(scope) = scope {
                    self.report(address, &scope, blocks::Problem::Unreadable)
                        .await;
                }
                Answer::None
            }
        }
    }

    /// The blocking part of [`Relay::publish`]. `Err(true)`: the pack failed its checks.
    fn open_and_publish(
        &self,
        key: &Digest,
        step: &Digest,
        path: &Path,
        found: &Found,
    ) -> Result<(), bool> {
        let expect = blocks::pack::Expect {
            key: *key.as_bytes(),
            step: Some(Hex32(*step.as_bytes())),
            pack_sha256: Some(found.pack_sha256),
            bytes: Some(found.bytes),
            outputs: Some(found.outputs),
        };
        let dir = path.with_extension("open");
        let opened = pack::open_pack(path, &expect, &dir).map_err(|err| err.unreadable())?;
        let date = self.state().server_date_ms.unwrap_or_else(now_ms);
        let origin = Origin::downloaded(self.vouch_entry(found, date));
        let published = pack::publish(
            &self.config.store,
            key,
            &opened,
            self.config.repository.as_deref(),
            u64::from(found.cost_ms),
            &origin,
            SystemTime::now(),
        );
        opened.remove();
        let _ = std::fs::remove_file(path);
        published.map(|_| ()).map_err(|_| false)
    }

    fn vouch_entry(&self, found: &Found, date_ms: u64) -> Vouch {
        Vouch {
            host: self.by.host.clone(),
            org: self.by.org.clone(),
            repository: self.config.repository.clone().unwrap_or_default(),
            scope: found.scope.clone(),
            outputs: found.outputs,
            date_ms,
        }
    }

    // --- vouching ------------------------------------------------------------------------

    /// B-34: whether a server the run belongs to still serves the downloaded block `key` with
    /// `outputs`. Served: its entry is renewed. No server reachable in this run: an entry of any
    /// age counts.
    async fn vouch(self: Arc<Relay>, key: Digest, outputs: Hex32) -> Answer {
        let address = Relay::address(&key);
        // Before `open` answers, wait for it, within the wrapper's wait.
        let deadline = Instant::now() + self.config.tuning.open * 2;
        loop {
            let phase = self.state().phase_now(Instant::now()).clone();
            match phase {
                Phase::Opening if Instant::now() < deadline => {
                    tokio::time::sleep(self.config.tuning.tick).await;
                }
                Phase::Opening => return Answer::None,
                Phase::Stopped(Stop::Offline(_)) => return Answer::Offline,
                Phase::Stopped(_) => return Answer::None,
                Phase::Resting(_) | Phase::Answering => break,
            }
        }
        let known = self.state().vouched.get(&address).copied();
        let served = match known {
            Some(served) => served,
            None => match self.lookup(&[address], false).await {
                Some(found) => found
                    .iter()
                    .find(|f| f.address == address)
                    .map(|f| f.outputs),
                None => {
                    return match self.state().phase.clone() {
                        Phase::Stopped(Stop::Refused(_)) => Answer::None,
                        _ => Answer::Offline,
                    };
                }
            },
        };
        if served != Some(outputs) {
            return Answer::None;
        }
        let found = self.state().found.get(&address).map(|f| f.0.clone());
        let scope = found.map(|f| f.scope);
        let entry = Vouch {
            host: self.by.host.clone(),
            org: self.by.org.clone(),
            repository: self.config.repository.clone().unwrap_or_default(),
            scope: scope.unwrap_or_default(),
            outputs,
            date_ms: self.state().server_date_ms.unwrap_or_else(now_ms),
        };
        let renewed = self.config.store.update_origin(&key, |origin| {
            let mut origin = origin?;
            let mut entry = entry;
            if entry.scope.is_empty() {
                entry.scope = origin
                    .report_scope(&self.by.host, &self.by.org, &entry.repository)
                    .unwrap_or_default()
                    .to_string();
            }
            origin.renew(entry);
            Some(origin)
        });
        if renewed.is_ok() {
            Answer::Served
        } else {
            Answer::None
        }
    }

    // --- what was built ------------------------------------------------------------------

    /// B-29: a block this run built. It gets a marker when it may leave the machine, the policy
    /// says this caller writes, the run shares, and the store's record says this run's host,
    /// org and credential built it. In shared `observe` it is also compared with the shared
    /// store's copy.
    fn built(self: &Arc<Relay>, request: Request) {
        let Request::Built {
            key,
            step,
            class,
            portable,
            cost_ms,
        } = request
        else {
            return;
        };
        let address = Relay::address(&key);
        let (writes, sharing) = {
            let mut state = self.state();
            if state.phase == Phase::Opening {
                state.pending.push(Request::Built {
                    key,
                    step,
                    class,
                    portable,
                    cost_ms,
                });
                return;
            }
            // The machine's speed: its compile against the uploader's, where the server knew one.
            if let Some((found, _)) = state.found.get(&address)
                && found.cost_ms > 0
            {
                let sample = cost_ms as f64 / f64::from(found.cost_ms);
                state.speed.push(sample);
            }
            let writes = state.policy.as_ref().is_some_and(|p| p.writes);
            (writes, state.sharing)
        };
        if matches!(sharing, Sharing::Off(_)) {
            return;
        }
        let shares = matches!(sharing, Sharing::Observe | Sharing::On);
        if portable && writes && shares {
            let built_by_us = self
                .config
                .store
                .origin(&key)
                .is_some_and(|origin| origin.may_go_up_as(&self.by));
            if built_by_us {
                let marker = Marker::new(
                    key,
                    step,
                    class,
                    cost_ms,
                    self.by.clone(),
                    self.config.repository.clone(),
                    SystemTime::now(),
                );
                if Outbox::new(&self.config.store).add(&marker).is_ok() {
                    self.wake.notify_one();
                }
            }
        }
        if sharing == Sharing::Observe && class == LocalClass::Library {
            let relay = self.clone();
            self.runtime
                .spawn(async move { relay.compare(key, address).await });
        }
    }

    /// B-33: shared `observe`'s comparison of a block built here with the shared store's copy:
    /// downloaded and opened in the background, never published. A difference suspends the
    /// first key here for the store's kept days and is reported to the scope it came from.
    async fn compare(self: Arc<Relay>, key: Digest, address: Hex32) {
        let Some(found) = self
            .lookup(&[address], true)
            .await
            .and_then(|f| f.into_iter().find(|f| f.address == address))
        else {
            return;
        };
        let Some(link) = found.get.clone() else {
            return;
        };
        let Some(request) = self.client.link(&link) else {
            return;
        };
        let Ok(response) = request.send().await else {
            return;
        };
        if !response.status().is_success() {
            return;
        }
        let Ok(bytes) = response.bytes().await else {
            return;
        };
        self.state().tally.bytes_down += bytes.len() as u64;
        let store = self.config.store.clone();
        let expect = blocks::pack::Expect {
            key: *key.as_bytes(),
            step: None,
            pack_sha256: Some(found.pack_sha256),
            bytes: Some(found.bytes),
            outputs: Some(found.outputs),
        };
        let verdict = self
            .runtime
            .spawn_blocking(move || {
                struct Discard;
                impl blocks::pack::Sink for Discard {
                    fn file(&mut self, _: usize, _: &str, _: u64) -> std::io::Result<()> {
                        Ok(())
                    }
                    fn write(&mut self, _: &[u8]) -> std::io::Result<()> {
                        Ok(())
                    }
                }
                let record = blocks::pack::open(&mut bytes.as_ref(), &expect, &mut Discard)?;
                let local = store.block(&key).ok().flatten();
                Ok::<_, blocks::pack::PackError>((record, local))
            })
            .await;
        let (record, local) = match verdict {
            Ok(Ok((record, Some(local)))) => (record, local),
            Ok(Err(err)) if err.unreadable() => {
                self.report(address, &found.scope, blocks::Problem::Unreadable)
                    .await;
                return;
            }
            _ => return,
        };
        let same = pack::same_outputs(&record, &local);
        {
            let mut state = self.state();
            state.tally.compared += 1;
            if !found.mine {
                state.tally.compared_others += 1;
            }
            if !same {
                state.tally.compared_differed += 1;
            }
        }
        if !same {
            let step: Option<Digest> = local
                .description
                .get("step")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            if let Some(step) = step {
                let now = SystemTime::now();
                let until = now + Duration::from_secs(u64::from(self.config.keep_days) * 86_400);
                let _ = self.config.store.suspend_until(
                    &step,
                    "the shared store's copy differed from a compile here",
                    until,
                    now,
                );
            }
            self.report(address, &found.scope, blocks::Problem::Differs)
                .await;
        }
    }

    // --- reports -------------------------------------------------------------------------

    /// A wrapper found a downloaded block wrong: reported to the scope its record names.
    async fn report_problem(self: Arc<Relay>, key: Digest, problem: Problem) {
        let Some(origin) = self.config.store.origin(&key) else {
            return;
        };
        let repository = self.config.repository.clone().unwrap_or_default();
        let Some(scope) = origin
            .report_scope(&self.by.host, &self.by.org, &repository)
            .map(str::to_string)
        else {
            return;
        };
        let problem = match problem {
            Problem::Differs => blocks::Problem::Differs,
            Problem::Rejected => blocks::Problem::Rejected,
        };
        self.report(Relay::address(&key), &scope, problem).await;
    }

    async fn report(&self, address: Hex32, scope: &str, problem: blocks::Problem) {
        if matches!(self.state().phase, Phase::Stopped(_)) {
            return;
        }
        let body = ReportRequest {
            kind: KIND.to_string(),
            address,
            scope: scope.to_string(),
            problem,
            repository: self.config.repository.clone(),
            commit: self.config.commit.clone(),
        };
        let answer = self
            .client
            .post::<_, serde_json::Value>(
                blocks::REPORT_PATH,
                &body,
                self.config.tuning.lookup,
                None,
            )
            .await;
        if let Err(Failure::Refused(status, code)) = answer {
            let mut state = self.state();
            state.stop(Stop::Refused(Failure::Refused(status, code).line()));
        }
    }

    // --- the end of a check --------------------------------------------------------------

    /// Ends a check (B-26, B-29): no new PUT starts, those in flight get the end's wait, the
    /// rest stays in the outbox; `close` goes once with the manifest and the counts; the socket
    /// goes. The lines for the person.
    pub(crate) fn end_check(self: &Arc<Relay>, entries: &[Entry]) -> Vec<String> {
        self.ending.store(true, Ordering::Relaxed);
        let wait = upload::wait_at_end(&self.config.tuning, false);
        let relay = self.clone();
        self.runtime.block_on(async move {
            upload::settle(&relay, wait).await;
        });
        let check = self.state().check.take();
        if let Some(check) = &check {
            check.stop_serving.store(true, Ordering::Relaxed);
            #[cfg(unix)]
            {
                let _ = std::os::unix::net::UnixStream::connect(&check.socket.path);
            }
            check.socket.remove();
        }
        let shares = {
            let state = self.state();
            !matches!(state.phase, Phase::Stopped(_) | Phase::Opening) && state.policy.is_some()
        };
        if let (true, Some(check)) = (shares, &check) {
            let close = self.close_request(check, entries);
            let relay = self.clone();
            self.runtime.block_on(async move {
                let _ = relay
                    .client
                    .post::<_, serde_json::Value>(
                        blocks::CLOSE_PATH,
                        &close,
                        relay.config.tuning.close,
                        None,
                    )
                    .await;
            });
        }
        // Downloads nobody opened are dropped and counted; the run's folder goes.
        let mut state = self.state();
        for fetch in state.fetches.values() {
            fetch.cancel.store(true, Ordering::Relaxed);
        }
        let unused = state
            .fetches
            .values()
            .filter(|f| matches!(f.state, Download::Arrived(_)))
            .count() as u64;
        state.tally.unused_downloads += unused;
        state.fetches.clear();
        state.queue.clear();
        let _ = std::fs::remove_dir_all(outbox::run_folder(
            &self.config.store,
            outbox::INCOMING,
            &self.run,
        ));
        let waiting = Outbox::new(&self.config.store)
            .waiting()
            .iter()
            .filter(|m| m.by == self.by)
            .count();
        let mut lines = std::mem::take(&mut state.lines);
        if waiting > 0 && !matches!(state.phase, Phase::Stopped(Stop::Off(_))) {
            lines.push(format!(
                "build: {waiting} {} to upload {} for the next run",
                if waiting == 1 { "block" } else { "blocks" },
                if waiting == 1 { "waits" } else { "wait" }
            ));
        }
        state.tally = Tally::default();
        drop(state);
        self.ending.store(false, Ordering::Relaxed);
        lines
    }

    fn close_request(&self, check: &Check, entries: &[Entry]) -> CloseRequest {
        let mut manifest = Vec::new();
        let mut seen = HashSet::new();
        let mut counts = Counts::default();
        for entry in entries {
            if let (Some(step), Some(key)) = (entry.step, entry.key) {
                let pair = Pair(Relay::address(&step), Relay::address(&key));
                if seen.insert(pair) && manifest.len() < blocks::MOST_MANIFEST {
                    manifest.push(pair);
                }
            }
            match entry.outcome {
                hushrun_build::model::Outcome::Reused if entry.downloaded => {
                    counts.reused_shared += 1;
                }
                hushrun_build::model::Outcome::Reused => counts.reused_local += 1,
                hushrun_build::model::Outcome::BuiltAndStored
                | hushrun_build::model::Outcome::BuiltNotStored(_) => counts.built += 1,
                hushrun_build::model::Outcome::PassedThrough(_) => {}
            }
            counts.saved_ms = counts.saved_ms.saturating_add(entry.saved_ms);
        }
        let state = self.state();
        let tally = &state.tally;
        counts.uploaded = tally.uploaded;
        counts.bytes_down = tally.bytes_down;
        counts.bytes_up = tally.bytes_up;
        counts.unused_downloads = tally.unused_downloads
            + state
                .fetches
                .values()
                .filter(|f| matches!(f.state, Download::Arrived(_)))
                .count() as u64;
        counts.compared = tally.compared;
        counts.compared_others = tally.compared_others;
        counts.compared_differed = tally.compared_differed;
        CloseRequest {
            kind: KIND.to_string(),
            run: check.run,
            repository: self.config.repository.clone(),
            check: check.check,
            platform: state
                .platform
                .clone()
                .unwrap_or_else(|| self.config.platform.clone()),
            commit: self.config.commit.clone(),
            manifest,
            counts,
        }
    }
}

/// Why a CI job on a listed branch isn't trusted, in plain words (the policy's reason word).
pub(crate) fn untrusted_words(why: blocks::Untrusted) -> &'static str {
    match why {
        blocks::Untrusted::Repository => {
            "the repository's owner isn't the one an admin confirmed (transferred, or made again)"
        }
        blocks::Untrusted::Event => {
            "its event isn't a push or a schedule, nor a dispatch the repository allows"
        }
        blocks::Untrusted::Environment => "it names a deployment environment",
        blocks::Untrusted::Commit => {
            "the commit it builds isn't the one its token names (or couldn't be read)"
        }
        blocks::Untrusted::Runner => {
            "it runs on a self-hosted runner the repository hasn't opted in"
        }
        blocks::Untrusted::Run => {
            "its run started before the branch was listed, or an admin withdrew it"
        }
        blocks::Untrusted::Unknown => "the backend says it isn't trusted",
    }
}

/// The `host:` line of `<rustc> -vV`, run in `folder` (where the rustup proxy picks the
/// folder's toolchain).
fn compiler_host(rustc: &std::ffi::OsStr, folder: &Path) -> Option<String> {
    let out = std::process::Command::new(rustc)
        .arg("-vV")
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(|h| h.trim().to_string()))
}

fn dedup(items: impl Iterator<Item = Hex32>) -> Vec<Hex32> {
    let mut seen = HashSet::new();
    items.filter(|item| seen.insert(*item)).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Random bytes: the time, the process and a counter, hashed. A run id needs to differ between
/// runs, not to be secret.
fn random(out: &mut [u8; 32]) {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seed = format!(
        "{:?}\n{}\n{}",
        SystemTime::now(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    *out = *Digest::of(seed.as_bytes()).as_bytes();
}

#[cfg(test)]
mod tests;
