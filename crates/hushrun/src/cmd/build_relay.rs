//! B-26 to B-30, B-32 to B-34: the relay, the part of the `hushrun run` process that talks to the
//! org's shared store of build blocks for the wrappers. One per process, started by the first
//! check that shares; it sends `open` and `close` once for each check, since a manifest belongs
//! to a check. The wrappers reach it over a Unix socket in their session's folder and stay free of
//! network code.
//!
//! What it holds, each in one place:
//!
//! - **Its phase** ([`Phase`]): opening, answering, resting (three requests in a row that errored
//!   or timed out: for 30 seconds no new request goes to the network, and wrappers are answered
//!   from what the relay holds), stopped (the org doesn't share, the server can't be reached, or
//!   it refused the caller mid-run). Moved by [`State::phase_now`], `open`'s answer,
//!   [`State::failed`] and [`State::stop`] only.
//! - **What the run shares** ([`Sharing`]): `hushrun_build::share::what_a_run_does`, asked again
//!   once `open` answers.
//! - **Each download** ([`Download`]): queued, downloading, arrived (a sealed pack in
//!   `incoming/<run>/`), published, dropped. Published only to answer a wrapper still waiting for
//!   it, never in shared `observe`.
//! - **Each upload**: its marker in the store's outbox (`hushrun_build::outbox`), moved by
//!   `upload.rs`.
//!
//! Every failure ends in rustc running as asked (B-19): an answer of "none" is a compile.

pub(crate) mod bake_archive;
pub(crate) mod bake_image;
pub mod baked;
mod credential;
pub(crate) mod hosted;
mod http;
mod pack;
pub(crate) mod saved;
pub(crate) mod socket;
mod upload;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use hushrun_build::model::{Class as LocalClass, Kind, Mode};
use hushrun_build::origin::{Identity, Origin, Standing, Vouch, Word};
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

/// How long the relay's numbers are: the defaults of B-26, which tests shorten. ADR 0144: none
/// of them is a budget for work. The three silences bound only a server that stopped sending
/// (each byte that comes starts one again), and the rest follows failures, never slowness: how
/// long an answer takes never decides what the run does.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tuning {
    /// `open` waits at most this on a silent server, and one more try.
    pub(crate) open: Duration,
    /// A lookup, `begin` or `commit` waits at most this on a silent server.
    pub(crate) lookup: Duration,
    /// Three failures in a row rest the relay this long.
    pub(crate) rest: Duration,
    /// `close` waits at most this on a silent server.
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
            open: http::SILENCE,
            lookup: http::SILENCE,
            rest: Duration::from_secs(30),
            close: http::SILENCE,
            downloads: 8,
            tick: Duration::from_millis(20),
            spill: pack::IN_MEMORY,
        }
    }
}

/// The most target triples a check asks `cc/1` manifests for: each is one more `open`.
const MOST_CC_PLATFORMS: usize = 8;
/// How many failures in a row rest the relay.
const FAILURES_TO_REST: u32 = 3;
/// A download's unpacked size is taken as this many times its pack, for the free-space floor.
const UNPACKED_PER_PACKED: u64 = 4;

/// Why sharing is off when `open` answers the reason `contract` (B-25): the org's plan sets its
/// shared storage by contract alone, and none is set yet. Never "upgrade": no plan gives it.
pub(crate) const BY_CONTRACT: &str = "the org's plan sets its shared storage by contract, and none is set yet: get in touch with hushrun to set it";

/// The host of an object store's link, and nothing else of it: no scheme, port, user, path or
/// query, which may hold a signature.
pub(crate) fn store_host(link: &str) -> Option<String> {
    reqwest::Url::parse(link)
        .ok()?
        .host_str()
        .filter(|host| !host.is_empty())
        .map(str::to_string)
}

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
    /// Who the run is, for B-25's table: [`crate::cmd::build_session::caller`].
    pub(crate) caller: Caller,
    /// B-1's mode and the person's share setting, for this command (B-33).
    pub(crate) local: Mode,
    pub(crate) own: Mode,
    pub(crate) min_free_gb: u32,
    pub(crate) keep_days: u32,
    pub(crate) tuning: Tuning,
    /// B-36: a fresh fetch and its folder, for a run that reads only it and sends nothing.
    pub(crate) saved: Option<(saved::Saved, PathBuf)>,
    /// B-25: no results lookup said the org shares (a run that can't tell which code it is):
    /// `open` is what asks, and its "doesn't share" is silent, as an org that doesn't share is
    /// to every run.
    pub(crate) open_decides: bool,
}

/// How a check's `open` stood after [`Relay::wait_open`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Opened {
    Answering,
    /// The org doesn't share, or what the run does says off.
    Off,
    /// The server couldn't be reached or refused: why.
    Stopped(String),
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
    /// Three failures in a row: until then no new request goes to the network, and wrappers are
    /// answered from what the relay holds.
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
    /// Whose block it is: a `rustc` one or a Go one.
    kind: Kind,
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
    /// Blocks built here this check discarded (B-29): said in one line.
    discarded: u64,
    /// Linked outputs built here that the trusted copy disagrees with, kept to this machine
    /// (B-29, B-34): said in one line.
    kept_local: u64,
}

/// A check the relay serves.
struct Check {
    run: Hex32,
    check: Option<Hex32>,
    socket: socket::Bound,
    stop_serving: Arc<AtomicBool>,
    /// B-45: the platform of the check's Go command (`GOOS_GOARCH`), when COMMAND is a Go
    /// module's and gets the cache program: its Go manifest is asked for and closed apart.
    go: Option<String>,
    /// B-82, B-176: the kinds a build tool keys itself whose cache COMMAND's tool is served
    /// (Gradle's, Maven's), each with the one platform its manifest is asked and closed under,
    /// apart from the check's own.
    tools: Vec<(Kind, String)>,
}

/// The check's Go manifest (B-45): asked once `open` answered, kept until the first Go fetch
/// takes it for the prefetch. A Go fetch that comes before it has answered waits for it, within
/// the fetch's own bound (ADR 0111), and is a miss past it; a Go fetch for an archive the manifest
/// doesn't name never asks the server.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GoManifest {
    /// No Go command in the check, or `open` hasn't answered yet.
    NotAsked,
    Asking,
    Got(Vec<Pair>),
    /// The prefetch has it: what it found is in the fetches.
    Taken,
}

/// The check's `cc/1` manifest for one target triple (B-60): asked when the first launcher of a
/// compile for that platform asks the relay (the triple is the compiler driver's, which only a
/// compile knows), kept for the check. Until it has answered, and for a first key it doesn't
/// name, a launcher's question is answered without the network.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CcManifest {
    Asking,
    Got {
        /// The first keys' addresses it names, each with the blocks' it pairs it with.
        named: HashMap<Hex32, HashSet<Hex32>>,
        /// Its pairs, until the first question about a first key it names takes them for the
        /// prefetch.
        pairs: Option<Vec<Pair>>,
    },
}

struct State {
    phase: Phase,
    policy: Option<Policy>,
    sharing: Sharing,
    /// The manifest `open` returned, until the first wrapper that misses starts its prefetch.
    manifest: Option<Vec<Pair>>,
    go_manifest: GoManifest,
    /// The prefetch of the Go manifest is looking up what it names.
    go_prefetching: bool,
    /// B-82, B-176: the check's manifest of each kind a build tool keys itself, as the Go one is
    /// kept; none is not asked.
    tool_manifests: HashMap<Kind, GoManifest>,
    /// The kinds whose manifest's prefetch is looking up what it names.
    tool_prefetching: HashSet<Kind>,
    /// The check's `cc/1` manifests, by target triple (B-60), and its `xcode/1` manifests, by
    /// Xcode build (B-90): by kind and platform.
    cc_manifests: HashMap<(Kind, String), CcManifest>,
    /// The prefetch of a `cc/1` manifest is looking up what it names.
    cc_prefetching: usize,
    /// The C and C++ objects a launcher told of (B-60): the run's log names the others at the
    /// check's end.
    cc_told: HashSet<Digest>,
    /// Whether the run may upload, as [`what_a_run_does`] says; the policy decides the rest.
    uploads: bool,
    /// Lines for the person, each once.
    lines: Vec<String>,
    /// Lookups this run: found, with when, and addresses known missing.
    found: HashMap<Hex32, (Found, Instant)>,
    missing: HashSet<Hex32>,
    suspended: HashSet<Hex32>,
    /// What the server said this run of a downloaded block's outputs: served or not.
    vouched: HashMap<Hex32, Option<Hex32>>,
    /// C1, B-34: the run's lookup of stale vouches is in flight (one at a time), and the
    /// addresses it asks about.
    vouching: bool,
    vouch_asked: HashSet<Hex32>,
    /// Sealed hint lists, by hint address, from the prefetch.
    sealed_hints: HashMap<Hex32, Vec<Vec<u8>>>,
    hints_asked: HashSet<Hex32>,
    fetches: HashMap<Hex32, Fetch>,
    queue: VecDeque<Hex32>,
    failures: u32,
    tally: Tally,
    /// Blocks built before `open` answered, decided once it has.
    pending: Vec<Request>,
    check: Option<Check>,
    /// The server's date in its last lookup's answer: what a block's entry is dated by (B-34).
    server_date_ms: Option<u64>,
    /// The PUTs in flight, which the end of the command may cut off (B-29).
    puts: HashMap<Digest, tokio::task::AbortHandle>,
    /// B-75: the blocks `begin` refused for good this check (`too_large`, `unsupported`,
    /// `no_scope`, `suspended`), by first key: never on the backend, so a bake's build that made
    /// one isn't whole. Taken at the end of each check.
    refused: Vec<(Digest, blocks::Refusal)>,
    /// The upload loop of the check is running.
    uploading: bool,
    /// The relay rested once this run: said once (B-26).
    rested: bool,
    /// The run is a CI job's (B-31): its store goes with it.
    ci: bool,
    /// The object store's host couldn't be reached once this run: said once (B-26).
    store_unreachable: bool,
    /// The run's platform, once asked.
    platform: Option<String>,
}

impl State {
    /// The check's manifest of `kind` that a fetch must name (B-45, B-82, B-176): a build
    /// tool's, by its kind, else Go's.
    fn named_manifest(&mut self, kind: Kind) -> &mut GoManifest {
        if kind.tool_keyed() {
            return self
                .tool_manifests
                .entry(kind)
                .or_insert(GoManifest::NotAsked);
        }
        &mut self.go_manifest
    }

    /// Whether the prefetch of that manifest is looking up what it names.
    fn named_prefetching(&self, kind: Kind) -> bool {
        if kind.tool_keyed() {
            return self.tool_prefetching.contains(&kind);
        }
        self.go_prefetching
    }

    /// Marks the prefetch of that manifest as under way, or over.
    fn set_named_prefetching(&mut self, kind: Kind, under_way: bool) {
        if !kind.tool_keyed() {
            self.go_prefetching = under_way;
        } else if under_way {
            self.tool_prefetching.insert(kind);
        } else {
            self.tool_prefetching.remove(&kind);
        }
    }

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

    /// Whether a new request may go to the network now: only while answering (B-26).
    fn answering(&mut self, now: Instant) -> bool {
        matches!(self.phase_now(now), Phase::Answering)
    }

    /// Whether a wrapper is answered from what the relay holds now (hints already downloaded,
    /// packs already in `incoming/`, vouches already known): while answering, and while resting,
    /// which stops new requests to the network and nothing else (B-26).
    fn serving(&mut self, now: Instant) -> bool {
        matches!(self.phase_now(now), Phase::Answering | Phase::Resting(_))
    }

    /// One more failure (a request that errored or timed out, never a give-up), `why` it failed;
    /// the third in a row rests the relay, and the first rest of the run says so in one line
    /// (B-26).
    fn failed(&mut self, now: Instant, rest: Duration, why: &str) {
        self.failures += 1;
        if self.failures >= FAILURES_TO_REST && self.phase == Phase::Answering {
            self.phase = Phase::Resting(now + rest);
            if !self.rested {
                self.rested = true;
                self.say(format!(
                    "build: the backend stopped answering mid-run ({why}); the run goes on without it, asking again every {}s",
                    rest.as_secs()
                ));
            }
        }
    }

    fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// Stops sharing for the rest of the run, saying nothing: the org doesn't share, and no
    /// results lookup had said otherwise (B-25).
    fn stop_silently(&mut self, stop: Stop) {
        if !matches!(self.phase, Phase::Stopped(_)) {
            self.phase = Phase::Stopped(stop);
        }
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
            // Markers wait for a later run on a machine whose store stays; in CI the store goes
            // with the run, and a withdrawn job's later runs are refused too.
            Stop::Refused(why) if self.ci || why.contains("job_withdrawn") => {
                format!("build: sharing stopped for this run: the backend {why}")
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

    /// B-26: a request to the object store couldn't connect to its host (a sandbox behind an
    /// allow-list reaches the backend and not the store): the run's first says which host to
    /// allow, in one line, by its name alone, never the link, its path or its query.
    fn store_unreachable(&mut self, link: &str) {
        let Some(host) = store_host(link) else {
            return;
        };
        if !self.store_unreachable {
            self.store_unreachable = true;
            self.say(format!(
                "build: the shared store's host, {host}, can't be reached from here: where this network allows only some hosts, allow {host} to reuse what other machines built"
            ));
        }
    }

    fn say(&mut self, line: String) {
        if !self.lines.contains(&line) {
            self.lines.push(line);
        }
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
    /// B-49: this run alone was asked to stop (`Relay::ask_to_stop`, a test's).
    asked_to_stop: AtomicBool,
    /// B-36: it reads only a fetch, and sends nothing.
    saved: bool,
    /// B-63: a bake's relay: its prefetch downloads every block a manifest names, programs too,
    /// since nothing will ask for them later and the store holds what the manifest names.
    whole: AtomicBool,
    /// A marker was written: the upload loop looks now rather than at its next turn.
    wake: tokio::sync::Notify,
    /// The end of the command cut uploads off: the upload loop's requests in flight are given up
    /// (B-29). Set at the end's cut, cleared when the check is over.
    cut: tokio::sync::watch::Sender<bool>,
}

static RELAY: OnceLock<Arc<Relay>> = OnceLock::new();

/// B-49: a stop signal came while COMMAND ran: the end of the run is short.
static STOPPING: AtomicBool = AtomicBool::new(false);

/// How a check's end goes ([`Relay::end_check_as`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// A run's: uploads waited for as [`upload::wait_at_end`] says, then `close` names what
    /// went up.
    Usual,
    /// B-75: a bake's build of what's missing. Every upload is waited for, as at every end
    /// (ADR 0144); `close` goes only when the command `succeeded` and nothing it built is left
    /// unsent.
    Bake { succeeded: bool },
}

/// B-49: the run was asked to stop (a GitLab job cancelled or timed out): its end waits for no
/// upload, and `close` gets at most [`upload::STOPPED_CLOSE`].
pub(crate) fn stopping() {
    STOPPING.store(true, Ordering::SeqCst);
}

impl Relay {
    /// The process's relay, started on first use (B-26: one per `hushrun run` process). `None`
    /// when `make` gives no config; `Err` is why it couldn't start.
    pub(crate) fn get_or_start(
        make: impl FnOnce() -> Option<Config>,
    ) -> Option<Result<Arc<Relay>, String>> {
        if let Some(relay) = RELAY.get() {
            return Some(Ok(relay.clone()));
        }
        Some(Relay::start(make()?).map(|relay| {
            let relay = Arc::new(relay);
            RELAY.get_or_init(|| relay).clone()
        }))
    }

    /// The process's relay, when one started.
    pub(crate) fn running() -> Option<Arc<Relay>> {
        RELAY.get().cloned()
    }

    /// Whether its credential is a GitLab job's (B-49).
    pub(crate) fn is_gitlab_job(&self) -> bool {
        self.client.is_gitlab_job()
    }

    /// Starts a relay: its runtime, its folder in the store, and the outbox's reconciliation
    /// (claims of dead runs back to waiting, dead runs' folders gone).
    pub(crate) fn start(mut config: Config) -> Result<Relay, String> {
        let credential = match (&config.access.credential, &config.saved) {
            (Some(credential), _) => {
                credential::Credential::of(credential, &config.access.base, &config.access.org)?
            }
            (None, Some(_)) => credential::Credential::Nothing,
            (None, None) => return Err("no credential to send".to_string()),
        };
        // B-31: a person's or an agent's token in CI says on every request that it runs there.
        let in_ci = matches!(config.caller, Caller::Ci { .. });
        let client = Client::new(&config.access.base, credential, in_ci)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|err| format!("starting the relay's runtime: {err}"))?;
        let run = outbox::run_name(SystemTime::now(), std::process::id());
        std::fs::create_dir_all(outbox::run_folder(&config.store, outbox::RELAYS, &run))
            .map_err(|err| format!("{}: {err}", config.store.root().display()))?;
        outbox::remove_dead_runs(&config.store);
        // B-31: in CI, who built a block is this job and its own store, whatever its
        // credential: a marker or a block another run left is never this run's to send.
        let credential = match config.caller {
            Caller::Ci { .. } => hushrun_build::origin::fingerprint(&format!(
                "ci\n{}\n{}",
                client.fingerprint(),
                config.store.root().display()
            )),
            _ => client.fingerprint(),
        };
        let by = Identity {
            host: config.access.host.clone(),
            org: config.access.org.clone(),
            credential,
        };
        let machine = config.access.machine.take();
        let does = what_a_run_does(config.caller, config.local, config.own, Org::Shares(None));
        Ok(Relay {
            by,
            client,
            runtime,
            run,
            machine,
            ending: AtomicBool::new(false),
            asked_to_stop: AtomicBool::new(false),
            saved: config.saved.is_some(),
            whole: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
            cut: tokio::sync::watch::Sender::new(false),
            state: Mutex::new(State {
                phase: Phase::Opening,
                policy: None,
                sharing: does.sharing,
                manifest: None,
                go_manifest: GoManifest::NotAsked,
                go_prefetching: false,
                tool_manifests: HashMap::new(),
                tool_prefetching: HashSet::new(),
                cc_manifests: HashMap::new(),
                cc_prefetching: 0,
                cc_told: HashSet::new(),
                uploads: does.uploads,
                lines: Vec::new(),
                found: HashMap::new(),
                missing: HashSet::new(),
                suspended: HashSet::new(),
                vouched: HashMap::new(),
                vouching: false,
                vouch_asked: HashSet::new(),
                sealed_hints: HashMap::new(),
                hints_asked: HashSet::new(),
                fetches: HashMap::new(),
                queue: VecDeque::new(),
                failures: 0,
                tally: Tally::default(),
                pending: Vec::new(),
                check: None,
                server_date_ms: None,
                puts: HashMap::new(),
                refused: Vec::new(),
                uploading: false,
                rested: false,
                ci: in_ci,
                store_unreachable: false,
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
        self.config.caller
    }

    /// Begins a check (B-26): binds a socket in the session's folder, starts answering it, and
    /// sends `open` in the background, so cargo never waits for the network. The `Share` to
    /// write into the session; an error is the line to say, and the check shares nothing.
    pub(crate) fn begin_check(
        self: &Arc<Relay>,
        session_folder: &Path,
        check: Option<Hex32>,
        go: Option<String>,
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
                go,
                tools: Vec::new(),
            });
            state.go_manifest = GoManifest::NotAsked;
            state.tool_manifests.clear();
            state.cc_manifests.clear();
            if !matches!(state.phase, Phase::Stopped(_)) {
                state.phase = Phase::Opening;
            }
        }
        if let Some((saved, folder)) = &self.config.saved {
            self.take_in_saved(saved, folder);
        } else {
            let relay = self.clone();
            self.runtime.spawn(async move { relay.open(check).await });
            // B-45: the check's Go manifest, asked beside the check's own `open`, not after it:
            // the standard library's leaves, which Go asks for first, would miss meanwhile. A Go
            // fetch serves nothing before the check's `open` says the run shares.
            let go_platform = {
                let mut state = self.state();
                let platform = state.check.as_ref().and_then(|c| c.go.clone());
                if platform.is_some() {
                    state.go_manifest = GoManifest::Asking;
                }
                platform
            };
            if let Some(platform) = go_platform {
                let relay = self.clone();
                self.runtime.spawn(async move {
                    relay.manifest_open(Kind::Go, check, platform).await;
                });
            }
            self.start_uploads();
        }
        Ok(Share {
            socket: path,
            run: self.run.clone(),
            by: self.by.clone(),
            repository: self.config.repository.clone().unwrap_or_default(),
        })
    }

    /// The lines the relay has said and no check's end has taken: for `hushrun build fetch`
    /// (B-36), which ends no check.
    /// What the relay's requests reach and carry: the org's base URL, the backend's origin, the
    /// org (B-70 names the backend's registry from them).
    pub(crate) fn access(&self) -> &crate::results::BuildAccess {
        &self.config.access
    }

    /// B-70: the bearer token the relay's requests carry now (a CI job's ID token minted for the
    /// org's URL), once its mint ends: what the backend's registry takes as a password for the
    /// same caller.
    pub(crate) fn bearer_now(&self) -> Result<String, String> {
        self.runtime
            .block_on(self.client.bearer_now())
            .map_err(|f| f.line())
    }

    pub(crate) fn take_lines(&self) -> Vec<String> {
        std::mem::take(&mut self.state().lines)
    }

    /// B-2, B-31: waits for the check's `open` to answer, for a CI job, whose wrapper is set only
    /// once the org said it shares. ADR 0144: until it answers or fails (a server silent for
    /// [`Tuning::open`] is one), however long that takes: whether a job shares never depends on
    /// how fast its machine got to it. What it found: answering, the org's no, or a stop with its
    /// line.
    pub(crate) fn wait_open(&self) -> Opened {
        loop {
            // The phase alone: the state's lock is not held while this waits.
            let phase = self.state().phase_now(Instant::now()).clone();
            match phase {
                Phase::Answering | Phase::Resting(_) => return Opened::Answering,
                Phase::Stopped(Stop::Off(_)) => return Opened::Off,
                Phase::Stopped(Stop::Offline(why) | Stop::Refused(why)) => {
                    return Opened::Stopped(why);
                }
                Phase::Opening => std::thread::sleep(self.config.tuning.tick),
            }
        }
    }

    /// Accepts the wrappers' connections: one request each, each in its own thread, answered
    /// when its answer is known, with heartbeats until then (ADR 0144).
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
        let Ok(request) = serde_json::from_str::<Request>(line.trim_end()) else {
            let _ = (&stream).write_all(b"{\"answer\":\"none\"}\n");
            return;
        };
        // ADR 0144: while the answer is worked on, one space every heartbeat, so that the
        // wrapper's wait bounds a relay that is frozen or gone and never one that is slow. The
        // answer and the heartbeats are written under one lock: no space lands inside the line.
        let written = std::sync::Mutex::new(false);
        std::thread::scope(|scope| {
            let heartbeats = scope.spawn(|| {
                loop {
                    std::thread::park_timeout(hushrun_build::relay::HEARTBEAT);
                    let done = written.lock().unwrap_or_else(|p| p.into_inner());
                    if *done || (&stream).write_all(b" ").is_err() {
                        return;
                    }
                }
            });
            // However the answer ends, a panic included, the heartbeats stop: a wrapper whose
            // answer panicked gets the connection's end, a miss, and never waits on a relay that
            // only says it is working.
            struct Over<'a>(&'a std::sync::Mutex<bool>, &'a std::thread::Thread);
            impl Drop for Over<'_> {
                fn drop(&mut self) {
                    *self.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
                    self.1.unpark();
                }
            }
            let _over = Over(&written, heartbeats.thread());
            let relay = self.clone();
            let answer = self
                .runtime
                .block_on(async move { relay.answer(request).await });
            let mut bytes = serde_json::to_vec(&answer).unwrap_or_default();
            bytes.push(b'\n');
            let mut done = written.lock().unwrap_or_else(|p| p.into_inner());
            let _ = (&stream).write_all(&bytes);
            *done = true;
        });
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
            } => {
                #[cfg(test)]
                fault::fetch();
                self.fetch(step, keys, class, local_ms).await
            }
            Request::Vouch { key, outputs, .. } => self.vouch(Kind::Rustc, key, outputs).await,
            Request::Sharing => self.still_sharing(),
            Request::Built { .. } => {
                self.built(request);
                Answer::Ok
            }
            Request::Report { key, problem } => {
                self.report_problem(Kind::Rustc, key, problem).await;
                Answer::Ok
            }
            Request::GoFetch { key, local_ms } => self.named_fetch(Kind::Go, key, local_ms).await,
            Request::GoVouch { key, outputs } => self.vouch(Kind::Go, key, outputs).await,
            Request::GoBuilt { .. } => {
                self.built(request);
                Answer::Ok
            }
            Request::GoReport { key, problem } => {
                // Answered once the block's record is read: the cache program then suspends the
                // key and discards the block, and the report goes on behind.
                self.report_problem_soon(Kind::Go, key, problem);
                Answer::Ok
            }
            Request::CcHints {
                step,
                platform,
                local_ms,
            } => self.cc_hints(Kind::Cc, step, platform, local_ms).await,
            Request::CcFetch {
                step,
                keys,
                platform,
                local_ms,
            } => {
                self.cc_fetch(Kind::Cc, step, keys, platform, local_ms)
                    .await
            }
            Request::CcVouch { key, outputs } => self.vouch(Kind::Cc, key, outputs).await,
            Request::CcBuilt { .. } => {
                self.built(request);
                Answer::Ok
            }
            Request::CcReport { key, problem } => {
                // As Go's: answered once the record is read, so the launcher may discard it.
                self.report_problem_soon(Kind::Cc, key, problem);
                Answer::Ok
            }
            Request::XcodeFetch {
                key,
                platform,
                local_ms,
            } => self.xcode_fetch(key, platform, local_ms).await,
            Request::XcodeVouch { key, outputs } => self.vouch(Kind::Xcode, key, outputs).await,
            Request::XcodeBuilt { .. } => {
                self.built(request);
                Answer::Ok
            }
            Request::XcodeReport { key, problem } => {
                self.report_problem_soon(Kind::Xcode, key, problem);
                Answer::Ok
            }
            // B-80, B-174: a build tool's cache's, by its kind; a kind the tool doesn't key
            // itself is no such request.
            Request::ToolFetch {
                kind,
                key,
                local_ms,
            } if kind.tool_keyed() => self.named_fetch(kind, key, local_ms).await,
            Request::ToolVouch { kind, key, outputs } if kind.tool_keyed() => {
                self.vouch(kind, key, outputs).await
            }
            Request::ToolBuilt { .. } => {
                self.built(request);
                Answer::Ok
            }
            Request::ToolReport { kind, key, problem } => {
                // As Go's: answered once the record is read, so the cache may discard it.
                if kind.tool_keyed() {
                    self.report_problem_soon(kind, key, problem);
                }
                Answer::Ok
            }
            Request::ToolFetch { .. } | Request::ToolVouch { .. } => Answer::None,
            // B-108: the TypeScript helper's, as the C and C++ launcher's, by its own kind and
            // manifest.
            Request::TscHints {
                step,
                platform,
                local_ms,
            } => self.cc_hints(Kind::Tsc, step, platform, local_ms).await,
            Request::TscFetch {
                step,
                keys,
                platform,
                local_ms,
            } => {
                self.cc_fetch(Kind::Tsc, step, keys, platform, local_ms)
                    .await
            }
            Request::TscVouch { key, outputs } => self.vouch(Kind::Tsc, key, outputs).await,
            Request::TscBuilt { .. } => {
                self.built(request);
                Answer::Ok
            }
            Request::TscReport { key, problem } => {
                self.report_problem_soon(Kind::Tsc, key, problem);
                Answer::Ok
            }
        }
    }

    /// The address of a key of `kind` (B-27): the kind is part of it, so a Go block and a
    /// `rustc` one never share one.
    fn address_in(kind: Kind, key: &Digest) -> Hex32 {
        blocks::address(kind.word(), key.as_bytes())
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
                    let off = Stop::Off("the backend doesn't share build blocks".into());
                    if self.config.open_decides {
                        state.stop_silently(off);
                    } else {
                        state.stop(off);
                    }
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
                    let policy_shares = policy.sharing;
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
                    state.uploads = does.uploads;
                    let manifest = policy.manifest.clone();
                    // A CI job on a branch its repository lists that isn't trusted: why, once.
                    if let Some(untrusted) = policy.untrusted {
                        state.say(format!(
                            "build: this job reads the repository's trusted blocks and writes none: {}",
                            untrusted_words(untrusted, self.is_gitlab_job())
                        ));
                    }
                    // B-25: the org shares, and its plan sets its storage by contract alone,
                    // which none set yet: said, also where `open` decides, since the org did
                    // turn sharing on.
                    let by_contract = policy.reason == Some(blocks::OffReason::Contract);
                    state.policy = Some(policy);
                    match does.sharing {
                        Sharing::Off(hushrun_build::share::Off::OrgDoesntShare) if by_contract => {
                            state.stop(Stop::Off(BY_CONTRACT.to_string()));
                            return;
                        }
                        Sharing::Off(off) => {
                            let off = Stop::Off(off.reason().to_string());
                            if self.config.open_decides && !policy_shares {
                                state.stop_silently(off);
                            } else {
                                state.stop(off);
                            }
                            return;
                        }
                        Sharing::Observe | Sharing::On => {
                            if !matches!(state.phase, Phase::Stopped(_)) {
                                state.phase = Phase::Answering;
                            }
                        }
                    }
                    let pending = std::mem::take(&mut state.pending);
                    // B-34: the run's lookup of stale vouches goes as soon as `open` answers.
                    let vouches = !state.vouching;
                    state.vouching = true;
                    drop(state);
                    if vouches {
                        let relay = self.clone();
                        self.runtime
                            .spawn(async move { relay.ask_vouches(None).await });
                    }
                    for built in pending {
                        self.built(built);
                    }
                    manifest
                }
            }
        };
        // C4: kept for the first wrapper that misses; a run that compiles nothing fetches
        // nothing.
        self.state().manifest = Some(manifest);
    }

    /// B-45, B-82: `open` for the check's Go command (kind `go/1`) or its Gradle (`gradle/1`),
    /// for its manifest alone: the policy is the one the check's own `open` answers, sent at the
    /// same time. A failure leaves no manifest, and so no download of the kind, and changes
    /// nothing else.
    async fn manifest_open(&self, kind: Kind, check: Option<Hex32>, platform: String) {
        let body = OpenRequest {
            kind: kind.word().to_string(),
            repository: self.config.repository.clone(),
            commit: self.config.commit.clone(),
            check,
            platform,
        };
        let manifest = match self
            .client
            .post::<_, Policy>(blocks::OPEN_PATH, &body, self.config.tuning.open, None)
            .await
        {
            Ok(answered) => answered.body.manifest,
            Err(_) => Vec::new(),
        };
        let mut state = self.state();
        let kept = state.named_manifest(kind);
        if *kept == GoManifest::Asking {
            *kept = GoManifest::Got(manifest);
        }
    }

    /// B-45: a Go package archive the cache program asks for. Downloaded only when the check's
    /// Go manifest names it: the program asks about every entry Go's cache misses, and most of
    /// them are local ones no server holds. The first Go fetch starts the prefetch of what the
    /// manifest names (a warm machine, whose `go` finds everything in its own cache, asks
    /// nothing and downloads nothing); one that comes while it looks up waits for it, however
    /// long that takes (ADR 0144).
    ///
    /// B-80: Gradle's cache asks the same way, for an entry of kind `gradle/1` the check's Gradle
    /// manifest names: Gradle asks about every task its own cache misses.
    async fn named_fetch(
        self: Arc<Relay>,
        kind: Kind,
        key: Digest,
        local_ms: Option<u64>,
    ) -> Answer {
        // ADR 0144: no bound. Each loop below ends on what it waits for, whatever the time.
        // \[ADR 0111\] The cache program starts with COMMAND, as the check's `open`s go out: a
        // fetch that comes while one is under way (the policy, or this kind's manifest) waits for
        // it within the same bound as for the lookup below, else Go's first packages compiled
        // on a slow link whatever the manifest named.
        loop {
            {
                let mut state = self.state();
                let now = Instant::now();
                let opening = matches!(state.phase_now(now), Phase::Opening);
                let asking = *state.named_manifest(kind) == GoManifest::Asking;
                if !(opening || asking) {
                    break;
                }
            }
            tokio::time::sleep(self.config.tuning.tick).await;
        }
        // The manifest is taken, and its prefetch marked as under way, under one lock: a fetch
        // that came between the two found it taken and no prefetch looking up, and missed what
        // the manifest named.
        let manifest = {
            let mut state = self.state();
            let now = Instant::now();
            if !state.serving(now) || state.sharing != Sharing::On {
                return Answer::None;
            }
            let got = matches!(state.named_manifest(kind), GoManifest::Got(_));
            if got && state.answering(now) {
                match std::mem::replace(state.named_manifest(kind), GoManifest::Taken) {
                    GoManifest::Got(manifest) => {
                        state.set_named_prefetching(kind, true);
                        Some(manifest)
                    }
                    _ => None,
                }
            } else {
                None
            }
        };
        if let Some(manifest) = manifest {
            // However the prefetch ends, a panic included, it is no longer under way: a fetch
            // waiting for it stops waiting.
            struct Prefetched<'a>(&'a Relay, Kind);
            impl Drop for Prefetched<'_> {
                fn drop(&mut self) {
                    self.0.state().set_named_prefetching(self.1, false);
                }
            }
            let _prefetched = Prefetched(&self, kind);
            self.clone().prefetch_in(kind, manifest).await;
        }
        let address = Relay::address_in(kind, &key);
        loop {
            {
                let state = self.state();
                if state.suspended.contains(&address) {
                    return Answer::None;
                }
                if state.fetches.contains_key(&address) || state.found.contains_key(&address) {
                    break;
                }
                if !state.named_prefetching(kind) {
                    return Answer::None;
                }
            }
            tokio::time::sleep(self.config.tuning.tick).await;
        }
        self.wait_for(kind, key, key, address, local_ms).await
    }

    /// B-60: `open` for the check's C and C++ compiles for the target triple `platform`, kind
    /// `cc/1`, for its manifest alone. A failure leaves an empty manifest, and so no download.
    async fn platform_open(&self, kind: Kind, platform: String) {
        // The check this `open` is for: a relay that runs several checks keeps an answer only
        // for the check still running when it comes.
        let (run, check) = {
            let state = self.state();
            let at = state.check.as_ref();
            (at.map(|c| c.run), at.and_then(|c| c.check))
        };
        let body = OpenRequest {
            kind: kind.word().to_string(),
            repository: self.config.repository.clone(),
            commit: self.config.commit.clone(),
            check,
            platform: platform.clone(),
        };
        let manifest = match self
            .client
            .post::<_, Policy>(blocks::OPEN_PATH, &body, self.config.tuning.open, None)
            .await
        {
            Ok(answered) => answered.body.manifest,
            Err(_) => Vec::new(),
        };
        let mut state = self.state();
        if state.check.as_ref().map(|c| c.run) == run
            && state.cc_manifests.get(&(kind, platform.clone())) == Some(&CcManifest::Asking)
        {
            state.cc_manifests.insert(
                (kind, platform),
                CcManifest::Got {
                    named: manifest.iter().fold(HashMap::new(), |mut named, pair| {
                        named
                            .entry(pair.hint())
                            .or_insert_with(HashSet::new)
                            .insert(pair.block());
                        named
                    }),
                    pairs: Some(manifest),
                },
            );
        }
    }

    /// B-60: whether the check's `cc/1` manifest for `platform` names the first key `step`,
    /// waiting for the check's `open` and the manifest to answer, however long that takes (ADR
    /// 0144); the first question of a platform asks for its manifest. The first question about a
    /// first key it names starts the prefetch of what it names, and waits for its lookups.
    /// `false`, at once, for a run that doesn't share `on`, or a manifest that doesn't name it
    /// or failed.
    async fn platform_names(
        self: &Arc<Relay>,
        kind: Kind,
        step: &Digest,
        platform: &str,
        _local_ms: Option<u64>,
    ) -> bool {
        if !blocks::is_platform(platform) {
            return false;
        }
        let address = Relay::address_in(kind, step);
        let slot = (kind, platform.to_string());
        // ADR 0144: no bound. Each loop below ends on what it waits for, whatever the time.
        let pairs = loop {
            {
                let mut state = self.state();
                let now = Instant::now();
                let opening = state.phase_now(now) == &Phase::Opening;
                if !opening && (!state.serving(now) || state.sharing != Sharing::On) {
                    return false;
                }
                if !state.cc_manifests.contains_key(&slot) {
                    // Resting, nothing new is asked (B-26).
                    let may_ask = opening || state.answering(now);
                    let of_kind = state
                        .cc_manifests
                        .keys()
                        .filter(|(k, _)| *k == kind)
                        .count();
                    if of_kind >= MOST_CC_PLATFORMS || self.saved || !may_ask {
                        return false;
                    }
                    state.cc_manifests.insert(slot.clone(), CcManifest::Asking);
                    let relay = self.clone();
                    let platform = platform.to_string();
                    self.runtime
                        .spawn(async move { relay.platform_open(kind, platform).await });
                }
                if !opening
                    && let Some(CcManifest::Got { named, pairs }) =
                        state.cc_manifests.get_mut(&slot)
                {
                    if !named.contains_key(&address) {
                        return false;
                    }
                    let taken = pairs.take();
                    if taken.is_some() {
                        state.cc_prefetching += 1;
                    }
                    break taken;
                }
            }
            tokio::time::sleep(self.config.tuning.tick).await;
        };
        match pairs {
            Some(pairs) => {
                // Answering: the prefetch's lookups are this question's. However it ends, a
                // panic included, it is no longer under way: the questions waiting for it stop.
                struct Prefetched<'a>(&'a Relay);
                impl Drop for Prefetched<'_> {
                    fn drop(&mut self) {
                        let mut state = self.0.state();
                        state.cc_prefetching = state.cc_prefetching.saturating_sub(1);
                    }
                }
                let _prefetched = Prefetched(self);
                self.clone().prefetch_in(kind, pairs).await;
            }
            None => {
                // Another question started the prefetch: what it looks up answers this one.
                while self.state().cc_prefetching > 0 {
                    tokio::time::sleep(self.config.tuning.tick).await;
                }
            }
        }
        true
    }

    /// B-60: the shared store's hints of a C or C++ first key, only when the check's `cc/1`
    /// manifest for the compile's platform names it.
    async fn cc_hints(
        self: Arc<Relay>,
        kind: Kind,
        step: Digest,
        platform: String,
        local_ms: Option<u64>,
    ) -> Answer {
        if !self.platform_names(kind, &step, &platform, local_ms).await {
            return Answer::None;
        }
        self.hints_in(kind, step).await
    }

    /// B-60: one of `keys`, the second keys a launcher's downloaded hints led to, as a `rustc`
    /// wrapper's fetch, of those the check's `cc/1` manifest names with the first key: a key a
    /// hint leads to that no compile of the check stored (the key an object tied to its folder
    /// would have, beside every portable one's) asks the server nothing.
    async fn cc_fetch(
        self: Arc<Relay>,
        kind: Kind,
        step: Digest,
        keys: Vec<Digest>,
        platform: String,
        local_ms: Option<u64>,
    ) -> Answer {
        if !self.platform_names(kind, &step, &platform, local_ms).await {
            return Answer::None;
        }
        let keys: Vec<Digest> = {
            let state = self.state();
            let Some(CcManifest::Got { named, .. }) = state.cc_manifests.get(&(kind, platform))
            else {
                return Answer::None;
            };
            let blocks = named.get(&Relay::address_in(kind, &step));
            keys.into_iter()
                .filter(|key| blocks.is_some_and(|b| b.contains(&Relay::address_in(kind, key))))
                .collect()
        };
        if keys.is_empty() {
            return Answer::None;
        }
        self.fetch_in(kind, step, keys, local_ms).await
    }

    /// B-90: the block of an Xcode value the service asks for, only when the check's `xcode/1`
    /// manifest for the Xcode build `platform` names it: the service asks about every compile
    /// its cache misses, and most are ones no server holds. The first question about a key it
    /// names starts the prefetch of what it names, as a C or C++ first key's does.
    async fn xcode_fetch(
        self: Arc<Relay>,
        key: Digest,
        platform: String,
        local_ms: Option<u64>,
    ) -> Answer {
        if !self
            .platform_names(Kind::Xcode, &key, &platform, local_ms)
            .await
        {
            return Answer::None;
        }
        self.fetch_in(Kind::Xcode, key, vec![key], local_ms).await
    }

    /// C4, B-30: the prefetch starts at the first wrapper that asks the relay, once: a run whose
    /// compiles all find their blocks here asks nothing and downloads nothing.
    async fn prefetch_at_first_miss(self: Arc<Relay>) {
        // Shared `observe` never reuses a download (B-33): its comparisons download what they
        // compare, once each, and a prefetch would only download those blocks a second time.
        let manifest = {
            let mut state = self.state();
            // Resting, it waits for a wrapper after the rest.
            if state.sharing != Sharing::On || !state.answering(Instant::now()) {
                return;
            }
            state.manifest.take()
        };
        if let Some(manifest) = manifest {
            self.prefetch_in(Kind::Rustc, manifest).await;
        }
    }

    /// B-30: from the manifest `open` returned, drop every pair the store already holds the
    /// block or the hint of, ask for the rest's hints and look up the rest's blocks, one request
    /// each, and queue the downloads [`prefetched`] names: proc macros and build-script binaries
    /// first, then libraries in order. Programs wait to be asked for.
    /// A Go manifest (`kind` Go) has no hints: each pair's first key is its block's key.
    async fn prefetch_in(self: Arc<Relay>, kind: Kind, manifest: Vec<Pair>) {
        if manifest.is_empty() {
            return;
        }
        let store = self.config.store.clone();
        // B-63, B-75: a bake's relay takes every block and hint from the backend, held here or
        // not: what it bakes is what the backend serves (a block its own build just compiled and
        // uploaded included), and it bakes from what arrives.
        let whole = self.whole.load(Ordering::SeqCst);
        let (held_blocks, held_hints, kept_local) = self
            .runtime
            .spawn_blocking(move || {
                if whole {
                    return (HashSet::new(), HashSet::new(), HashSet::new());
                }
                let blocks: HashSet<Hex32> = store
                    .keys()
                    .iter()
                    .map(|key| Relay::address_in(kind, key))
                    .collect();
                if kind.self_keyed() {
                    return (blocks, HashSet::new(), HashSet::new());
                }
                let steps = store.steps();
                let hints: HashSet<Hex32> = steps
                    .iter()
                    .map(|step| Relay::address_in(kind, step))
                    .collect();
                // B-34: a first key this machine keeps to itself asks the shared store nothing.
                let kept: HashSet<Hex32> = steps
                    .iter()
                    .filter(|step| store.kept_local(step).is_some())
                    .map(|step| Relay::address_in(kind, step))
                    .collect();
                (blocks, hints, kept)
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
                .filter(|pair| !kept_local.contains(&pair.hint()))
                .map(Pair::block)
                .filter(|a| !held_blocks.contains(a)),
        );
        if !kind.self_keyed() {
            for batch in hints.chunks(limit) {
                self.lookup_hints(kind, batch).await;
            }
        }
        // What is fetched without being asked for, by class (B-30): proc macros and
        // build-script binaries first, since cargo can start no dependent of a macro and no
        // build script's run before they are there, and they are small; then libraries, in the
        // manifest's order. A program is the last thing a build needs and the largest: it
        // waits to be asked for.
        let mut looked_up = Vec::new();
        for batch in wanted.chunks(limit) {
            looked_up.extend(
                self.lookup_in(kind, batch, true)
                    .await
                    .into_iter()
                    .flatten(),
            );
        }
        {
            let mut state = self.state();
            for found in prefetch_order(kind, looked_up, |found| found.class, whole) {
                if !state.fetches.contains_key(&found.address) {
                    state.queue.push_back(found.address);
                    let looked = state
                        .found
                        .get(&found.address)
                        .map_or_else(Instant::now, |f| f.1);
                    state.fetches.insert(
                        found.address,
                        Fetch {
                            kind,
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

    async fn lookup_hints(&self, kind: Kind, addresses: &[Hex32]) {
        // Resting, nothing new is asked (B-26).
        if self.saved || !self.state().answering(Instant::now()) {
            return;
        }
        let body = HintsLookupRequest {
            kind: kind.word().to_string(),
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

    /// A lookup of `addresses`, blocks of `kind` (B-26: "this is the use"); what it found, or
    /// `None` when it failed.
    async fn lookup_in(&self, kind: Kind, addresses: &[Hex32], fetch: bool) -> Option<Vec<Found>> {
        // Resting, nothing new is asked (B-26).
        if self.saved || !self.state().answering(Instant::now()) {
            return None;
        }
        let body = LookupRequest {
            kind: kind.word().to_string(),
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

    /// What a failed request does to the run (B-26), a download's, a lookup's or an upload's: a
    /// refusal stops sharing; a server that can't be reached, or fails, counts as a failure.
    fn request_failed(&self, state: &mut State, failure: Failure) {
        match failure {
            Failure::Refused(_, _) => state.stop(Stop::Refused(failure.line())),
            Failure::Unreachable(why) | Failure::Failed(why) => {
                state.failed(Instant::now(), self.config.tuning.rest, &why);
            }
        }
    }

    /// ADR 0144: waits while the check's `open` is on the wire, until it answers or fails (its
    /// server silent for [`Tuning::open`] is a failure): a question that comes before is
    /// answered as one that comes after, so what a build reuses never depends on how fast its
    /// first compiles started.
    async fn opened(&self) {
        while matches!(self.state().phase_now(Instant::now()), Phase::Opening) {
            tokio::time::sleep(self.config.tuning.tick).await;
        }
    }

    // --- hints ---------------------------------------------------------------------------

    /// B-30: the shared store's hints of a first key, opened with it and written where
    /// downloaded hints live.
    async fn hints(self: Arc<Relay>, step: Digest) -> Answer {
        self.opened().await;
        self.clone().prefetch_at_first_miss().await;
        self.hints_in(Kind::Rustc, step).await
    }

    /// [`Relay::hints`], of a first key of `kind`, once the caller decided it may be asked.
    async fn hints_in(self: Arc<Relay>, kind: Kind, step: Digest) -> Answer {
        let address = Relay::address_in(kind, &step);
        let known = {
            let mut state = self.state();
            let now = Instant::now();
            // A downloaded hint only leads to a fetch, which shared `observe` never makes.
            if !state.serving(now) || state.sharing != Sharing::On {
                return Answer::None;
            }
            if state.hints_asked.contains(&address) {
                Some(state.sealed_hints.remove(&address).unwrap_or_default())
            } else if !state.answering(now) {
                // Resting: hints already downloaded are answered, nothing new is asked.
                return Answer::None;
            } else {
                None
            }
        };
        let sealed = match known {
            Some(sealed) => sealed,
            None => {
                self.lookup_hints(kind, &[address]).await;
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
        self.opened().await;
        {
            let mut state = self.state();
            if !state.serving(Instant::now()) || state.sharing != Sharing::On {
                return Answer::None;
            }
        }
        self.clone().prefetch_at_first_miss().await;
        self.fetch_in(Kind::Rustc, step, keys, local_ms).await
    }

    /// [`Relay::fetch`], of blocks of `kind`, once the caller decided they may be asked for.
    async fn fetch_in(
        self: Arc<Relay>,
        kind: Kind,
        step: Digest,
        keys: Vec<Digest>,
        local_ms: Option<u64>,
    ) -> Answer {
        {
            let mut state = self.state();
            if !state.serving(Instant::now()) || state.sharing != Sharing::On {
                return Answer::None;
            }
        }
        let addresses: Vec<(Digest, Hex32)> = keys
            .iter()
            .map(|key| (*key, Relay::address_in(kind, key)))
            .collect();
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
        // Resting, nothing new is asked: only what the run knows answers.
        if !unknown.is_empty() && self.state().answering(Instant::now()) {
            self.lookup_in(kind, &unknown, true).await;
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
        self.wait_for(kind, key, step, address, local_ms).await
    }

    /// Starts or joins the download of `address`, a block of `kind`, and waits for it within
    /// half its cost.
    async fn wait_for(
        self: Arc<Relay>,
        kind: Kind,
        key: Digest,
        step: Digest,
        address: Hex32,
        _local_ms: Option<u64>,
    ) -> Answer {
        let tuning = self.config.tuning;
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
        {
            let mut state = self.state();
            let now = Instant::now();
            if !state.fetches.contains_key(&address) {
                let Some((found, looked)) = state.found.get(&address).cloned() else {
                    return Answer::None;
                };
                state.fetches.insert(
                    address,
                    Fetch {
                        kind,
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
            let resting = !state.answering(now);
            let Some(fetch) = state.fetches.get(&address) else {
                return Answer::None;
            };
            if matches!(fetch.state, Download::Dropped | Download::Published) {
                return Answer::None;
            }
            // Resting after failures (B-26): a download not under way would be a new request.
            if resting && fetch.state == Download::Queued {
                return Answer::None;
            }
            // ADR 0144: waited for to its end, however long it takes; what the shared store
            // holds is reused, whatever this machine's load or link. Nothing about how fast it
            // goes decides whether it is waited for.
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
        }
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
            if !state.serving(now) {
                return Answer::None;
            }
            let resting = !state.answering(now);
            let Some(fetch) = state.fetches.get(&address) else {
                return Answer::None;
            };
            match &fetch.state {
                Download::Arrived(_) => continue,
                Download::Dropped | Download::Published => return Answer::None,
                Download::Queued | Download::Downloading => {}
            }
            // Resting after failures, while it hasn't started: no new request goes until the
            // rest is over (B-26). A download under way goes on, and is waited for.
            if resting && fetch.state == Download::Queued {
                if let Some(fetch) = state.fetches.get_mut(&address) {
                    fetch.waited = false;
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
            // Resting or stopped: no new download starts; those under way go on (B-26).
            if !state.answering(Instant::now()) {
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
                    fetch.kind,
                    fetch.found.clone(),
                    fetch.looked_up,
                    fetch.cancel.clone(),
                ));
                running += 1;
            }
        }
        for (address, kind, found, looked_up, cancel) in starts {
            let relay = self.clone();
            self.runtime.spawn(async move {
                relay
                    .download(address, kind, found, looked_up, cancel)
                    .await
            });
        }
    }

    /// One download into `incoming/<run>/`, checked against its row's size and SHA-256.
    async fn download(
        self: Arc<Relay>,
        address: Hex32,
        kind: Kind,
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
                .lookup_in(kind, &[address], true)
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
            // A link's URL is signed: no error that names it reaches a line.
            let mut response = match request.send().await {
                Ok(response) => response,
                Err(err) => {
                    if err.is_connect() {
                        self.state().store_unreachable(&link.url);
                    }
                    return Err(Failure::Unreachable(http::chain(&err.without_url())));
                }
            };
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
                    .map_err(|err| Failure::Unreachable(http::chain(&err.without_url())))?;
                let Some(chunk) = chunk else { break };
                got += chunk.len() as u64;
                if got > found.bytes {
                    return Err(Failure::Failed("longer than its row says".into()));
                }
                hash.update(&chunk);
                std::io::Write::write_all(&mut file, &chunk)
                    .map_err(|err| Failure::Failed(err.to_string()))?;
                let mut state = self.state();
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
                    if !cancelled && let Err(failure) = &outcome {
                        state.failed(now, self.config.tuning.rest, &failure.line());
                    }
                }
            }
        }
        if unreadable {
            self.report(kind, address, &found.scope, blocks::Problem::Unreadable)
                .await;
        }
        self.pump();
    }

    /// B-32: opens an arrived pack with the key the wrapper brought and publishes it into the
    /// store, recorded as downloaded with the server's word for it. Only for a wrapper that is
    /// waiting, and never in shared `observe`.
    async fn publish(self: Arc<Relay>, key: Digest, step: Digest, address: Hex32) -> Answer {
        let (kind, path, found) = {
            let state = self.state();
            match state.fetches.get(&address) {
                Some(Fetch {
                    kind,
                    state: Download::Arrived(path),
                    found,
                    ..
                }) => (*kind, path.clone(), found.clone()),
                _ => return Answer::None,
            }
        };
        let relay = self.clone();
        let opened = self
            .runtime
            .spawn_blocking(move || relay.open_and_publish(kind, &key, &step, &path, &found))
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
                    self.report(kind, address, &scope, blocks::Problem::Unreadable)
                        .await;
                }
                Answer::None
            }
        }
    }

    /// The blocking part of [`Relay::publish`]. `Err(true)`: the pack failed its checks.
    fn open_and_publish(
        &self,
        kind: Kind,
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
        let room = self.room_for(found.bytes);
        let opened =
            pack::open_pack(path, &expect, Some(room), &dir).map_err(|err| err.unreadable())?;
        let date = self.state().server_date_ms.unwrap_or_else(now_ms);
        let origin = Origin::downloaded(self.vouch_entry(found, date));
        let published = pack::publish(
            &self.config.store,
            kind,
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
            word: Word::Served,
        }
    }

    // --- vouching ------------------------------------------------------------------------

    /// B-34: whether a block that was only downloaded may be reused on a fresh entry: not once
    /// sharing stopped for this run by a decision, `open`'s "the org doesn't share" (or what
    /// the run does saying off) or a refusal of the caller. While `open` is under way, while the
    /// relay answers or rests, and when the server couldn't be reached, it may. Answered from
    /// the phase alone, without the network.
    fn still_sharing(&self) -> Answer {
        match self.state().phase_now(Instant::now()) {
            Phase::Stopped(Stop::Off(_) | Stop::Refused(_)) => Answer::None,
            Phase::Opening
            | Phase::Answering
            | Phase::Resting(_)
            | Phase::Stopped(Stop::Offline(_)) => Answer::Ok,
        }
    }

    /// B-34: whether a server the run belongs to still serves the downloaded block `key` with
    /// `outputs`, for a wrapper whose entry is stale: once `open` has answered, the run's lookup
    /// of stale vouches answers, or it is a miss; the server can't be reached (an entry of any
    /// age counts) only when `open` failed. ADR 0144: no budget; the answer waits for `open` and
    /// the lookup, however long they take, so that it never depends on the machine's speed.
    /// Resting after failures (B-26), a block no lookup named is a miss at once: no new request
    /// goes until the rest is over.
    async fn vouch(self: Arc<Relay>, kind: Kind, key: Digest, outputs: Hex32) -> Answer {
        // A run from a saved fetch reaches no server (B-36).
        if self.saved {
            return Answer::Offline;
        }
        let address = Relay::address_in(kind, &key);
        // A lookup naming it was under way since this question came.
        let mut named = false;
        loop {
            let now = Instant::now();
            {
                let mut state = self.state();
                match state.phase_now(now).clone() {
                    Phase::Stopped(Stop::Offline(_)) => return Answer::Offline,
                    Phase::Stopped(_) => return Answer::None,
                    Phase::Opening => {}
                    Phase::Resting(_) | Phase::Answering => {
                        if let Some(served) = state.vouched.get(&address) {
                            return if *served == Some(outputs) {
                                Answer::Served
                            } else {
                                Answer::None
                            };
                        }
                        if state.vouch_asked.contains(&address) {
                            named = true;
                        } else if named {
                            // The lookup that named it got no answer: a miss.
                            return Answer::None;
                        } else if !state.answering(now) {
                            return Answer::None;
                        } else if !state.vouching {
                            // A block no lookup of the run named: one more, once none is under
                            // way.
                            state.vouching = true;
                            state.vouch_asked.insert(address);
                            named = true;
                            let relay = self.clone();
                            self.runtime
                                .spawn(async move { relay.ask_vouches(Some((kind, key))).await });
                        }
                    }
                }
            }
            tokio::time::sleep(self.config.tuning.tick).await;
        }
    }

    /// B-34: the run's lookup of stale vouches, as soon as `open` answers (`first` none), and
    /// again for a block a wrapper asks about that no lookup named: every block in the store only
    /// downloaded whose entry for this run's host, org and repository is stale, as the store's
    /// records say, and `first`. One runs at a time (`vouching`, set by the caller).
    async fn ask_vouches(self: Arc<Relay>, first: Option<(Kind, Digest)>) {
        // However the lookup ends, a panic included, it is over: the end of the command waits
        // for it no longer, the next question about a block no lookup named starts one, and a
        // question about `first` is a miss at once (B-29, B-34).
        struct Over<'a>(&'a Relay, Option<(Kind, Digest)>);
        impl Drop for Over<'_> {
            fn drop(&mut self) {
                let mut state = self.0.state();
                if let Some((kind, first)) = &self.1 {
                    state.vouch_asked.remove(&Relay::address_in(*kind, first));
                }
                state.vouching = false;
            }
        }
        let _over = Over(&self, first);
        #[cfg(test)]
        fault::vouches();
        let relay = self.clone();
        let stale = self
            .runtime
            .spawn_blocking(move || relay.stale_downloads())
            .await
            .unwrap_or_default();
        let asked: Vec<(Kind, Digest)> = {
            let state = self.state();
            let mut seen = HashSet::new();
            first
                .into_iter()
                .chain(stale)
                .filter(|(kind, key)| {
                    let address = Relay::address_in(*kind, key);
                    seen.insert(address) && !state.vouched.contains_key(&address)
                })
                .collect()
        };
        self.ask_about(&asked).await;
    }

    /// B-34: asks the server, without fetching, whether it still serves `keys`, downloaded blocks
    /// in the store, in as few lookups as its limit allows, and records each answer: served with
    /// the block's outputs renews its entry; anything else is a "no", which the block's record
    /// keeps (`Word::Withdrawn`), so that it is never reused as a download again. A lookup that
    /// fails records nothing. What the run learnt answers its wrappers (`vouched`).
    async fn ask_about(self: &Arc<Relay>, keys: &[(Kind, Digest)]) {
        let limit = self
            .state()
            .policy
            .as_ref()
            .map_or(blocks::MOST_LOOKUP, |p| p.limits.lookup as usize)
            .max(1);
        for kind in [
            Kind::Rustc,
            Kind::Go,
            Kind::Cc,
            Kind::Gradle,
            Kind::Xcode,
            Kind::Maven,
            Kind::Tsc,
        ] {
            let keys: Vec<Digest> = keys
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, key)| *key)
                .collect();
            self.ask_about_in(kind, &keys, limit).await;
        }
    }

    /// [`Relay::ask_about`], for blocks of one kind.
    async fn ask_about_in(self: &Arc<Relay>, kind: Kind, keys: &[Digest], limit: usize) {
        for batch in keys.chunks(limit) {
            let addresses: Vec<Hex32> = batch
                .iter()
                .map(|key| Relay::address_in(kind, key))
                .collect();
            self.state().vouch_asked.extend(addresses.iter().copied());
            let found = self.lookup_in(kind, &addresses, false).await;
            let date_ms = self.state().server_date_ms.unwrap_or_else(now_ms);
            let answered: Option<HashMap<Hex32, Found>> =
                found.map(|found| found.into_iter().map(|f| (f.address, f)).collect());
            {
                let mut state = self.state();
                for address in &addresses {
                    state.vouch_asked.remove(address);
                    if let Some(answered) = &answered {
                        let served = answered.get(address).map(|f| f.outputs);
                        state.vouched.insert(*address, served);
                    }
                }
            }
            let Some(answered) = answered else { continue };
            let relay = self.clone();
            let batch = batch.to_vec();
            let _ = self
                .runtime
                .spawn_blocking(move || relay.record_words(kind, &batch, &answered, date_ms))
                .await;
        }
    }

    /// Writes what the server said of each of `keys` into its record (B-32, B-34), dated
    /// `date_ms` by the server's clock.
    fn record_words(
        &self,
        kind: Kind,
        keys: &[Digest],
        answered: &HashMap<Hex32, Found>,
        date_ms: u64,
    ) {
        let repository = self.config.repository.clone().unwrap_or_default();
        for key in keys {
            let found = answered.get(&Relay::address_in(kind, key));
            let _ = self.config.store.update_origin(key, |origin| {
                let mut origin = origin.filter(Origin::downloaded_only)?;
                let held = origin
                    .vouch(&self.by.host, &self.by.org, &repository)
                    .or_else(|| origin.vouched.first())
                    .cloned()?;
                let served = found.filter(|f| f.outputs == held.outputs);
                origin.renew(Vouch {
                    host: self.by.host.clone(),
                    org: self.by.org.clone(),
                    repository: repository.clone(),
                    scope: served.map_or(held.scope.clone(), |f| f.scope.clone()),
                    outputs: held.outputs,
                    date_ms,
                    word: if served.is_some() {
                        Word::Served
                    } else {
                        Word::Withdrawn
                    },
                });
                Some(origin)
            });
        }
    }

    /// The blocks in the store only downloaded whose entry for this run's host, org and
    /// repository is a day old or more ([`Origin::standing`] says stale).
    fn stale_downloads(&self) -> Vec<(Kind, Digest)> {
        let repository = self.config.repository.clone().unwrap_or_default();
        let run = Some((
            self.by.host.as_str(),
            self.by.org.as_str(),
            repository.as_str(),
        ));
        let now = now_ms();
        self.config
            .store
            .keys()
            .into_iter()
            .filter(|key| {
                self.config.store.origin(key).is_some_and(|origin| {
                    origin
                        .vouch(&self.by.host, &self.by.org, &repository)
                        .is_some()
                        && origin.standing(run, now) == Standing::Stale
                })
            })
            .map(|key| (self.kind_of(&key), key))
            .collect()
    }

    /// The kind of the block in the store under `key`, as its description says.
    fn kind_of(&self, key: &Digest) -> Kind {
        self.config
            .store
            .block(key)
            .ok()
            .flatten()
            .map_or(Kind::Rustc, |block| {
                Kind::of_description(&block.description)
            })
    }

    /// B-34: at the end of a check, the downloaded blocks its compiles reused that no lookup of
    /// the run answered for, asked about once more, so that a machine in daily use never goes
    /// stale.
    fn reused_downloads(&self, entries: &[Entry]) -> Vec<(Kind, Digest)> {
        let state = self.state();
        let mut seen = HashSet::new();
        entries
            .iter()
            .filter(|e| e.outcome == hushrun_build::model::Outcome::Reused)
            .filter(|e| !e.go.as_ref().is_some_and(|go| go.go_cache))
            .filter_map(|e| e.key.map(|key| (e.kind(), key)))
            .filter(|(kind, key)| {
                let address = Relay::address_in(*kind, key);
                seen.insert(address) && !state.vouched.contains_key(&address)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .filter(|(_, key)| {
                self.config.store.origin(key).is_some_and(|origin| {
                    matches!(
                        origin.standing(None, now_ms()),
                        Standing::Stale | Standing::Fresh
                    )
                })
            })
            .collect()
    }

    // --- what was built ------------------------------------------------------------------

    /// B-60: the C and C++ objects the run's log says were stored with nothing keeping them on
    /// the machine (or tied alone), whose notice never reached the relay: a launcher that found
    /// it slow once marks it down for the run and tells it nothing after. Each is told now, as
    /// its launcher would have, before the uploads end.
    fn sweep_cc_built(self: &Arc<Relay>, entries: &[Entry]) {
        let told = self.state().cc_told.clone();
        for entry in entries {
            let (Some(cc), Some(key), Some(step)) = (entry.cc.as_ref(), entry.key, entry.step)
            else {
                continue;
            };
            if entry.outcome != hushrun_build::model::Outcome::BuiltAndStored
                || entry.differed
                || cc.probe
                || told.contains(&key)
            {
                continue;
            }
            let portable = cc.local.is_empty();
            let bound = cc.local == ["tied"];
            if portable || bound {
                self.built(Request::CcBuilt {
                    key,
                    step,
                    portable,
                    bound,
                    cost_ms: entry.rustc_ms,
                });
            }
        }
    }

    /// B-29: a block this run built. It gets a marker when it may leave the machine, the policy
    /// says this caller writes, the run shares, and the store's record says this run's host,
    /// org and credential built it. In shared `observe` it is also compared with the shared
    /// store's copy.
    fn built(self: &Arc<Relay>, request: Request) {
        let (kind, key, step, class, portable, bound, cost_ms) = match &request {
            Request::Built {
                key,
                step,
                class,
                portable,
                bound,
                cost_ms,
            } => (
                Kind::Rustc,
                *key,
                *step,
                *class,
                *portable,
                *bound,
                *cost_ms,
            ),
            // B-45: a Go package archive is a library whose first key is its key, and the cache
            // program tells only of portable ones.
            Request::GoBuilt { key, cost_ms } => (
                Kind::Go,
                *key,
                *key,
                LocalClass::Library,
                true,
                false,
                *cost_ms,
            ),
            // B-81, B-175: a Gradle entry or a Maven module's is a library whose first key is
            // its key, and the cache tells only of ones that may leave (B-79, B-173).
            Request::ToolBuilt { kind, key, cost_ms } if kind.tool_keyed() => (
                *kind,
                *key,
                *key,
                LocalClass::Library,
                true,
                false,
                *cost_ms,
            ),
            // B-60: a C or C++ object is a library.
            Request::CcBuilt {
                key,
                step,
                portable,
                bound,
                cost_ms,
            } => {
                self.state().cc_told.insert(*key);
                (
                    Kind::Cc,
                    *key,
                    *step,
                    LocalClass::Library,
                    *portable,
                    *bound,
                    *cost_ms,
                )
            }
            // B-89: an Xcode block is a library whose first key is its key, and the service tells
            // only of those that may leave the machine.
            Request::XcodeBuilt { key, cost_ms } => (
                Kind::Xcode,
                *key,
                *key,
                LocalClass::Library,
                true,
                false,
                *cost_ms,
            ),
            // B-108: a TypeScript block is a library, and the helper tells only of those that
            // may leave the machine.
            Request::TscBuilt { key, step, cost_ms } => (
                Kind::Tsc,
                *key,
                *step,
                LocalClass::Library,
                true,
                false,
                *cost_ms,
            ),
            _ => return,
        };
        let address = Relay::address_in(kind, &key);
        let (writes, sharing) = {
            let mut state = self.state();
            if state.phase == Phase::Opening {
                state.pending.push(request);
                return;
            }
            // A run that uploads nothing (a fetch's, B-36; an agent token's in CI) leaves no
            // marker nobody could send.
            let writes = state.uploads && state.policy.as_ref().is_some_and(|p| p.writes);
            (writes, state.sharing)
        };
        if matches!(sharing, Sharing::Off(_)) {
            return;
        }
        let shares = matches!(sharing, Sharing::Observe | Sharing::On);
        // B-24, B-29: a portable block may go up from any run that writes. One bound to
        // folders its keys hold the values of goes up from a CI job only: every job of a
        // repository has the same paths, which is where such a block is of use, and a CI
        // path names nobody, where a person's folders hold their user name and would be
        // readable, in the block's bytes, by everyone their scope serves. What makes a run a
        // CI job here is its credential, the job's own ID token, which is what the server
        // trusts it by: never the environment, which says CI wherever a variable is set.
        let from_a_job = self.client.is_a_jobs_id_token();
        if (portable || (bound && from_a_job)) && writes && shares {
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
                )
                .of_kind(kind);
                if Outbox::new(&self.config.store).add(&marker).is_ok() {
                    self.wake.notify_one();
                }
            }
        }
        // B-33: every class is compared, a linked output among them (B-16); a Gradle entry
        // never is, since Gradle's outputs needn't be the same from one execution to the next
        // (B-81).
        if sharing == Sharing::Observe && !kind.tool_keyed() {
            let relay = self.clone();
            let linked = class != LocalClass::Library;
            self.runtime
                .spawn(async move { relay.compare(kind, key, address, linked).await });
        }
    }

    /// C5: what a pack of `pack_bytes` may unpack to here, the one rule a download and shared
    /// `observe`'s comparison open by: what the disk can take above its floor, and at most 64
    /// times the pack.
    fn room_for(&self, pack_bytes: u64) -> blocks::pack::Room {
        let free = self.config.store.free_bytes().unwrap_or(u64::MAX);
        let floor = u64::from(self.config.min_free_gb).saturating_mul(1 << 30);
        blocks::pack::Room::for_pack(pack_bytes, free.saturating_sub(floor))
    }

    /// B-33: shared `observe`'s comparison of a block built here with the shared store's copy:
    /// downloaded and opened in the background, never published. A difference suspends the
    /// first key here for the store's kept days and is reported to the scope it came from. For
    /// a linked output (`linked`) it keeps the first key to this machine instead, and the
    /// report says the link differs (B-34): this machine's linker makes these bytes.
    async fn compare(self: Arc<Relay>, kind: Kind, key: Digest, address: Hex32, linked: bool) {
        let Some(found) = self
            .lookup_in(kind, &[address], true)
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
        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                if err.is_connect() {
                    self.state().store_unreachable(&link.url);
                }
                return;
            }
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
        // C5: the room a download gets, though nothing is written: a pack made to expand costs
        // no more CPU here than it could disk there.
        let room = self.room_for(found.bytes);
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
                let record = blocks::pack::open_within(
                    &mut bytes.as_ref(),
                    &expect,
                    Some(room),
                    &mut Discard,
                )?;
                let local = store.block(&key).ok().flatten();
                Ok::<_, blocks::pack::PackError>((record, local))
            })
            .await;
        let (record, local) = match verdict {
            Ok(Ok((record, Some(local)))) => (record, local),
            Ok(Err(err)) if err.unreadable() => {
                self.report(kind, address, &found.scope, blocks::Problem::Unreadable)
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
                let _ = if linked {
                    self.config.store.keep_local(
                        &step,
                        "the shared store's copy was linked otherwise",
                        until,
                        now,
                    )
                } else {
                    self.config.store.suspend_until(
                        &step,
                        "the shared store's copy differed from a compile here",
                        until,
                        now,
                    )
                };
            }
            let problem = if linked {
                blocks::Problem::LinkDiffers
            } else {
                blocks::Problem::Differs
            };
            self.report(kind, address, &found.scope, problem).await;
        }
    }

    // --- reports -------------------------------------------------------------------------

    /// A wrapper found a downloaded block wrong: reported to the scope its record names.
    /// [`Relay::report_problem`], with the scope read now and the report sent in the
    /// background.
    fn report_problem_soon(self: &Arc<Relay>, kind: Kind, key: Digest, problem: Problem) {
        let Some(scope) = self.report_scope(&key) else {
            return;
        };
        let relay = self.clone();
        self.runtime.spawn(async move {
            relay
                .report(
                    kind,
                    Relay::address_in(kind, &key),
                    &scope,
                    wire_problem(problem),
                )
                .await;
        });
    }

    /// The scope a report about the block `key` goes to, from its record.
    fn report_scope(&self, key: &Digest) -> Option<String> {
        let origin = self.config.store.origin(key)?;
        let repository = self.config.repository.clone().unwrap_or_default();
        origin
            .report_scope(&self.by.host, &self.by.org, &repository)
            .map(str::to_string)
    }

    async fn report_problem(self: Arc<Relay>, kind: Kind, key: Digest, problem: Problem) {
        let Some(scope) = self.report_scope(&key) else {
            return;
        };
        self.report(
            kind,
            Relay::address_in(kind, &key),
            &scope,
            wire_problem(problem),
        )
        .await;
    }

    async fn report(&self, kind: Kind, address: Hex32, scope: &str, problem: blocks::Problem) {
        if self.saved || !self.state().answering(Instant::now()) || !reported(kind, problem) {
            return;
        }
        let body = ReportRequest {
            kind: kind.word().to_string(),
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

    /// B-82, B-176: the check that began serves a build tool's cache too (Gradle's, Maven's):
    /// its manifest of `kind` is asked for beside the check's own `open`, under `platform`, one
    /// for every machine, and closed apart. Nothing is asked by a relay that reads a saved
    /// policy, whose runs ask no manifest.
    pub(crate) fn also_tool(self: &Arc<Relay>, kind: Kind, platform: &str) {
        if !kind.tool_keyed() {
            return;
        }
        let check = {
            let mut state = self.state();
            let Some(at) = state.check.as_mut() else {
                return;
            };
            if !at.tools.iter().any(|(k, _)| *k == kind) {
                at.tools.push((kind, platform.to_string()));
            }
            let check = at.check;
            if self.config.saved.is_some() {
                return;
            }
            *state.named_manifest(kind) = GoManifest::Asking;
            check
        };
        let relay = self.clone();
        let platform = platform.to_string();
        self.runtime.spawn(async move {
            relay.manifest_open(kind, check, platform).await;
        });
    }

    /// Ends a check (B-26, B-29): no new PUT starts, those in flight get the end's wait, the
    /// rest stays in the outbox; `close` goes once with the manifest and the counts; the socket
    /// goes. The lines for the person.
    pub(crate) fn end_check(self: &Arc<Relay>, entries: &[Entry]) -> Vec<String> {
        self.end_check_as(entries, Ending::Usual)
    }

    /// [`Relay::end_check`], ended as `ending` says: a bake's build of what's missing (B-75)
    /// waits for every upload and closes only a whole build.
    pub(crate) fn end_check_as(
        self: &Arc<Relay>,
        entries: &[Entry],
        ending: Ending,
    ) -> Vec<String> {
        self.sweep_cc_built(entries);
        // B-31: no ID token is minted once the end has begun.
        self.client.ending(true);
        let end = self.end();
        // ADR 0144 (B-29, B-31): unless a stop signal came, nothing after the command has a
        // deadline. Every upload this run built goes on, on any machine, until it is done or an
        // answer (or a server's silence) puts it off; then `close` names what went up. What the
        // manifest leaves out depends on the backend's answers alone, never on the machine's
        // speed. Asked to stop (B-49), uploads are cut off at once and `close` gets its short
        // grace.
        let wait = upload::wait_at_end(&self.config.tuning, &end);
        let started = Instant::now();
        let cut = wait.map(|wait| started + wait.uploads);
        let deadline = wait.map(|wait| started + wait.total);
        if cut.is_none() {
            self.say_uploading();
        }
        let relay = self.clone();
        self.runtime.block_on(async move {
            // B-34: the lookup of stale vouches still in flight ends, so that what it learnt is
            // written; then the downloaded blocks this check reused that no lookup answered for
            // are asked about once more, beside the last uploads (cut off with them on a stop).
            let renewals = async {
                let ask = async {
                    while relay.state().vouching {
                        tokio::time::sleep(relay.config.tuning.tick).await;
                    }
                    let reused = relay.reused_downloads(entries);
                    if !reused.is_empty() {
                        relay.ask_about(&reused).await;
                    }
                };
                match cut {
                    Some(cut) => {
                        let _ = tokio::time::timeout_at(cut.into(), ask).await;
                    }
                    None => ask.await,
                }
            };
            let uploads = async {
                // The loop goes on claiming what this run built until none is left to go, on
                // any machine: a CI job's store goes with the job, and every other run's would
                // leave its manifest short.
                if cut.is_none() {
                    upload::drain(&relay, None).await;
                }
                relay.ending.store(true, Ordering::Relaxed);
                upload::settle(&relay, cut, deadline).await;
            };
            futures_util::future::join(renewals, uploads).await;
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
        let left = deadline
            .map_or(self.config.tuning.close, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            })
            .min(self.config.tuning.close);
        // B-75: a bake's build closes only a whole build: one that failed, was stopped, or left
        // blocks it built unsent would name the commit with a part of its blocks, and every later
        // bake would take that part for the commit's build.
        let mut closes = true;
        // What `begin` refused for good this check, of the blocks it built: taken whatever the
        // ending, so none is counted against a later check.
        let refused: Vec<(Digest, blocks::Refusal)> = std::mem::take(&mut self.state().refused)
            .into_iter()
            .filter(|(key, _)| entries.iter().any(|e| e.key == Some(*key)))
            .collect();
        if let Ending::Bake { succeeded, .. } = ending {
            let unsent = {
                let outbox = Outbox::new(&self.config.store);
                outbox.waiting().iter().filter(|m| m.by == self.by).count()
                    + outbox.claimed(&self.run).len()
            };
            if !succeeded || unsent > 0 || !refused.is_empty() {
                closes = false;
                if shares && check.is_some() {
                    let why = if !succeeded {
                        "its command didn't succeed".to_string()
                    } else if unsent > 0 {
                        format!(
                            "{unsent} {} it built {} uploaded",
                            if unsent == 1 { "block" } else { "blocks" },
                            if unsent == 1 { "wasn't" } else { "weren't" }
                        )
                    } else {
                        format!(
                            "the backend refused {} {} it built ({})",
                            refused.len(),
                            if refused.len() == 1 {
                                "block"
                            } else {
                                "blocks"
                            },
                            refused
                                .iter()
                                .map(|(key, reason)| format!(
                                    "{}: {}",
                                    key.to_string().chars().take(12).collect::<String>(),
                                    upload::refusal_words(*reason)
                                ))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    self.state().lines.push(format!(
                        "build: no manifest was closed for this commit: {why}; the backend's newest build stays the last whole one, and the next bake builds again"
                    ));
                }
            }
        }
        if let (true, false, Some(check), false, true) =
            (shares, self.saved, &check, left.is_zero(), closes)
        {
            let close = self.close_request(Kind::Rustc, check, entries);
            // B-45: the check's Go command closes apart, with its own manifest and run.
            let go = (check.go.is_some() && entries.iter().any(|e| e.kind() == Kind::Go))
                .then(|| self.close_request(Kind::Go, check, entries));
            // B-82, B-176: and each build tool's, with its own manifest and run.
            let tools: Vec<CloseRequest> = check
                .tools
                .iter()
                .filter(|(kind, _)| entries.iter().any(|e| e.kind() == *kind))
                .map(|(kind, _)| self.close_request(*kind, check, entries))
                .collect();
            // B-60: and its C and C++ compiles, once for each target triple they were for.
            let mut cc: Vec<CloseRequest> = cc_platforms(entries)
                .into_iter()
                .map(|platform| self.close_platform(Kind::Cc, check, entries, &platform))
                .collect();
            // B-108: and its TypeScript compiles, once for each Node platform.
            cc.extend(
                tsc_platforms(entries)
                    .into_iter()
                    .map(|platform| self.close_platform(Kind::Tsc, check, entries, &platform)),
            );
            // B-90: and its Xcode values, once for each Xcode build they were compiled with.
            cc.extend(
                xcode_platforms(entries)
                    .into_iter()
                    .map(|platform| self.close_platform(Kind::Xcode, check, entries, &platform)),
            );
            let relay = self.clone();
            self.runtime.block_on(async move {
                let rust = relay.client.post::<_, serde_json::Value>(
                    blocks::CLOSE_PATH,
                    &close,
                    left,
                    None,
                );
                let go = async {
                    for close in go.iter().chain(tools.iter()) {
                        let _ = relay
                            .client
                            .post::<_, serde_json::Value>(blocks::CLOSE_PATH, close, left, None)
                            .await;
                    }
                };
                let cc = futures_util::future::join_all(cc.iter().map(|close| {
                    relay
                        .client
                        .post::<_, serde_json::Value>(blocks::CLOSE_PATH, close, left, None)
                }));
                let _ = futures_util::future::join3(rust, go, cc).await;
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
        // B-29: a block built here that was discarded is never silent.
        if state.tally.discarded > 0 {
            let n = state.tally.discarded;
            lines.push(format!(
                "build: {n} {} built here {} discarded: the repository's trusted copy has other bytes, and is served in its place",
                if n == 1 { "block" } else { "blocks" },
                if n == 1 { "was" } else { "were" },
            ));
        }
        // B-29, B-34: nor is a linked output this machine keeps to itself.
        if state.tally.kept_local > 0 {
            let n = state.tally.kept_local;
            lines.push(format!(
                "build: {n} {} linked here {} from the repository's trusted copy: this machine keeps its own and doesn't share {} for {} days (the two link environments differ in something hushrun doesn't see)",
                if n == 1 { "program, proc macro or build script" } else { "programs, proc macros or build scripts" },
                if n == 1 { "differs" } else { "differ" },
                if n == 1 { "it" } else { "them" },
                self.config.keep_days,
            ));
        }
        // B-20, B-33: shared `observe` says what it compared and how many differed.
        if state.sharing == Sharing::Observe && state.tally.compared > 0 {
            lines.push(observed_line(
                state.tally.compared,
                state.tally.compared_differed,
                self.config.keep_days,
            ));
        }
        let in_ci = matches!(self.config.caller, Caller::Ci { .. });
        if waiting > 0 && in_ci && state.uploads {
            // B-31: the job's store goes with the job.
            lines.push(format!(
                "build: {waiting} {} uploaded before the job's command ended",
                if waiting == 1 {
                    "block wasn't"
                } else {
                    "blocks weren't"
                },
            ));
        } else if waiting > 0
            && !in_ci
            && state.uploads
            && !matches!(state.phase, Phase::Stopped(Stop::Off(_)))
        {
            lines.push(format!(
                "build: {waiting} {} to upload {} for the next run",
                if waiting == 1 { "block" } else { "blocks" },
                if waiting == 1 { "waits" } else { "wait" }
            ));
        }
        state.tally = Tally::default();
        drop(state);
        self.ending.store(false, Ordering::Relaxed);
        self.cut.send_replace(false);
        self.client.ending(false);
        lines
    }

    /// What the end of this check's command waits by (B-31).
    fn end(&self) -> upload::End {
        upload::End {
            stopping: STOPPING.load(Ordering::SeqCst) || self.asked_to_stop.load(Ordering::SeqCst),
        }
    }

    /// B-49: this relay's run was asked to stop, as a stop signal asks every run of the process:
    /// its end then cuts its uploads off. For a test, which can't send this process a signal
    /// without stopping every other test's run.
    #[cfg(test)]
    pub(crate) fn ask_to_stop(&self) {
        self.asked_to_stop.store(true, Ordering::SeqCst);
    }

    /// ADR 0144: the line a person watching sees while the end of the command waits for this
    /// run's uploads, when any is left: on a terminal only, since the run's own lines come after.
    fn say_uploading(&self) {
        use std::io::IsTerminal as _;
        if !std::io::stderr().is_terminal() {
            return;
        }
        let outbox = Outbox::new(&self.config.store);
        let left = outbox.waiting().iter().filter(|m| m.by == self.by).count()
            + outbox.claimed(&self.run).len();
        if left > 0 && self.state().uploads {
            eprintln!(
                "hushrun: build: uploading {left} {} before the run ends (Ctrl-C leaves the rest for the next run)",
                if left == 1 { "block" } else { "blocks" }
            );
        }
    }

    /// B-60, B-90: the check's `close` for its C and C++ objects compiled for `platform`, or its
    /// Xcode values compiled with the Xcode build `platform`, under a run of its own made from
    /// the check's, the kind and the platform, with a manifest of their blocks in the order they
    /// were logged.
    fn close_platform(
        &self,
        kind: Kind,
        check: &Check,
        entries: &[Entry],
        platform: &str,
    ) -> CloseRequest {
        let entries: Vec<Entry> = entries
            .iter()
            .filter(|e| match kind {
                Kind::Xcode => e.xcode.as_ref().is_some_and(|x| x.platform == platform),
                Kind::Tsc => e.tsc.as_ref().is_some_and(|t| t.platform == platform),
                _ => e.cc.as_ref().is_some_and(|cc| cc.platform == platform),
            })
            .cloned()
            .collect();
        let mut close = self.close_request(kind, check, &entries);
        let mut bytes = check.run.0.to_vec();
        bytes.extend_from_slice(format!("{}\n{platform}\n", kind.word()).as_bytes());
        close.run = Hex32::sha256(&bytes);
        close.platform = platform.to_string();
        close
    }

    /// The check's `close` for its blocks of `kind`: a Go command's goes under a run of its own,
    /// made from the check's, with the Go command's platform, and the relay's tallies go with the
    /// `rustc` one alone.
    fn close_request(&self, kind: Kind, check: &Check, entries: &[Entry]) -> CloseRequest {
        let mut manifest = Vec::new();
        let mut seen = HashSet::new();
        let mut counts = Counts::default();
        // A Go archive kept on this machine (B-42) never goes up, nor does a C or C++ object that
        // stays here (B-56): no later prefetch looks it up. A C object reused here names why its
        // block stays from the block's own record. One bound to the checkout's folders alone
        // (`tied`) goes up from a CI job's own ID token (B-24), and that job's close names it:
        // a later job at the same paths fetches only a key the manifest pairs (B-60).
        let from_a_job = self.client.is_a_jobs_id_token();
        let goes_up = |local: &[String]| local.is_empty() || (from_a_job && local == ["tied"]);
        // B-30, B-31: a block this run meant to upload and didn't (its marker still waits in the
        // outbox, or was claimed and cut off at the end of the command) isn't on the server: the
        // manifest leaves it out, so no bake or fetch counts on it. The marker stays, and the
        // next run in this store uploads it.
        let outbox = hushrun_build::outbox::Outbox::new(&self.config.store);
        let unsent: HashSet<Digest> = outbox
            .waiting()
            .into_iter()
            .chain(outbox.claimed(&self.run))
            .filter(|m| m.by == self.by && m.kind == kind)
            .map(|m| m.key)
            .collect();
        // B-24, B-29: nor does a compiled crate that may not leave the machine: one neither
        // portable nor (from a CI job's own ID token) bound to folders alone. Named, it would be a
        // block no bake or fetch can have (the bake of nushell missed nu-command, whose `extern`
        // keeps it here, and every image of it was partial).
        let rust_stays = |e: &Entry| {
            e.go.is_none()
                && e.cc.is_none()
                && e.xcode.is_none()
                && e.gradle.is_none()
                && e.maven.is_none()
                && e.tsc.is_none()
                && e.portable
                    .as_ref()
                    .is_some_and(|p| !(p.portable || (from_a_job && p.bound_only())))
        };
        let kept_here = |e: &Entry| {
            e.go.as_ref().is_some_and(|go| go.local.is_some())
                || e.xcode.as_ref().is_some_and(|x| x.local.is_some())
                || e.gradle.as_ref().is_some_and(|g| g.local.is_some())
                || e.maven.as_ref().is_some_and(|m| m.local.is_some())
                || e.cc.as_ref().is_some_and(|cc| !goes_up(&cc.local))
                || e.tsc.as_ref().is_some_and(|t| !t.local.is_empty())
                || rust_stays(e)
        };
        for entry in entries.iter().filter(|e| e.kind() == kind) {
            if let (Some(step), Some(key), false) = (entry.step, entry.key, kept_here(entry))
                && !unsent.contains(&key)
            {
                let pair = Pair(
                    Relay::address_in(kind, &step),
                    Relay::address_in(kind, &key),
                );
                if seen.insert(pair) && manifest.len() < blocks::MOST_MANIFEST {
                    manifest.push(pair);
                }
            }
            // What Go's own cache served is plain Go's: in the manifest, and counted nowhere.
            if entry.go.as_ref().is_some_and(|go| go.go_cache) {
                continue;
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
        if matches!(kind, Kind::Cc | Kind::Xcode | Kind::Tsc) {
            return CloseRequest {
                kind: kind.word().to_string(),
                run: check.run,
                repository: self.config.repository.clone(),
                check: check.check,
                platform: String::new(),
                commit: self.config.commit.clone(),
                manifest,
                counts,
            };
        }
        if kind == Kind::Go || kind.tool_keyed() {
            let mut bytes = check.run.0.to_vec();
            bytes.extend_from_slice(format!("{}\n", kind.word()).as_bytes());
            let platform = if kind.tool_keyed() {
                check
                    .tools
                    .iter()
                    .find(|(k, _)| *k == kind)
                    .map(|(_, platform)| platform.clone())
                    .unwrap_or_default()
            } else {
                check.go.clone().unwrap_or_default()
            };
            return CloseRequest {
                kind: kind.word().to_string(),
                run: Hex32::sha256(&bytes),
                repository: self.config.repository.clone(),
                check: check.check,
                platform,
                commit: self.config.commit.clone(),
                manifest,
                counts,
            };
        }
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
            kind: kind.word().to_string(),
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

/// The target triples of a check's C and C++ objects (B-60), in the order first logged: one
/// `close` each. A triple the protocol can't carry, and CMake's own configure checks, are none.
fn cc_platforms(entries: &[Entry]) -> Vec<String> {
    let mut platforms: Vec<String> = Vec::new();
    for cc in entries.iter().filter_map(|e| e.cc.as_ref()) {
        if !cc.probe
            && blocks::is_platform(&cc.platform)
            && !platforms.contains(&cc.platform)
            && platforms.len() < MOST_CC_PLATFORMS
        {
            platforms.push(cc.platform.clone());
        }
    }
    platforms
}

/// The Node platforms of a check's TypeScript compiles (B-108), in the order first logged: one
/// `close` each.
fn tsc_platforms(entries: &[Entry]) -> Vec<String> {
    let mut platforms: Vec<String> = Vec::new();
    for tsc in entries.iter().filter_map(|e| e.tsc.as_ref()) {
        if blocks::is_platform(&tsc.platform)
            && !platforms.contains(&tsc.platform)
            && platforms.len() < MOST_CC_PLATFORMS
        {
            platforms.push(tsc.platform.clone());
        }
    }
    platforms
}

/// The Xcode builds of a check's values (B-90), in the order first logged: one `close` each.
fn xcode_platforms(entries: &[Entry]) -> Vec<String> {
    let mut platforms: Vec<String> = Vec::new();
    for xcode in entries.iter().filter_map(|e| e.xcode.as_ref()) {
        if blocks::is_platform(&xcode.platform)
            && !platforms.contains(&xcode.platform)
            && platforms.len() < MOST_CC_PLATFORMS
        {
            platforms.push(xcode.platform.clone());
        }
    }
    platforms
}

/// A wrapper's word for what was wrong with a block, as the protocol writes it.
fn wire_problem(problem: Problem) -> blocks::Problem {
    match problem {
        Problem::Differs => blocks::Problem::Differs,
        Problem::Rejected => blocks::Problem::Rejected,
        Problem::LinkDiffers => blocks::Problem::LinkDiffers,
    }
}

/// The order the prefetch queues what it found of kind `kind` in (B-30): what [`prefetched`]
/// names, proc macros and build-script binaries first, each kind keeping the order it came in
/// (the manifest's), then libraries, in that order too. With `whole` (a bake's, B-63) every
/// block, the rest (programs) last. A kind a build tool keys itself (Gradle's, Maven's) takes
/// every block its manifest names, in the manifest's order: they go up as class `other`, and
/// the tool asks for each only as it reaches its task.
fn prefetch_order<T>(
    kind: Kind,
    found: Vec<T>,
    class: impl Fn(&T) -> blocks::Class,
    whole: bool,
) -> Vec<T> {
    if kind.tool_keyed() {
        return found;
    }
    let (asked, rest): (Vec<T>, Vec<T>) = found
        .into_iter()
        .partition(|found| prefetched(class(found)));
    let (mut first, libraries): (Vec<T>, Vec<T>) = asked
        .into_iter()
        .partition(|found| class(found) != blocks::Class::Library);
    first.extend(libraries);
    if whole {
        first.extend(rest);
    }
    first
}

/// Whether a report of `problem` about a block of `kind` is sent at all. A block a build tool
/// keyed itself (Gradle's, Maven's) is never reported as differing: its bytes needn't repeat,
/// and a trusted job's `differs` report suspends a key and deletes the copy whatever its class
/// (B-81, B-175). That it couldn't be read, was missing or was rejected is reported as for
/// every kind.
fn reported(kind: Kind, problem: blocks::Problem) -> bool {
    !(kind.tool_keyed()
        && matches!(
            problem,
            blocks::Problem::Differs | blocks::Problem::LinkDiffers
        ))
}

/// Whether a block of `class` is fetched from a manifest without being asked for (B-30):
/// libraries, proc macros and build-script binaries. A program is fetched only when a compile
/// asks for it; a class this version doesn't know never is.
fn prefetched(class: blocks::Class) -> bool {
    matches!(
        class,
        blocks::Class::Library | blocks::Class::ProcMacro | blocks::Class::BuildScript
    )
}

/// The summary's line for shared `observe` (B-20, B-33): how many blocks built here were compared
/// with the shared store's copy, and how many differed, which are reported and not reused here
/// for `keep_days`.
fn observed_line(compared: u64, differed: u64, keep_days: u32) -> String {
    let blocks = if compared == 1 { "block" } else { "blocks" };
    if differed == 0 {
        return format!(
            "build: sharing observed: {compared} {blocks} compared with the shared store, none differed"
        );
    }
    format!(
        "build: sharing observed: {compared} {blocks} compared with the shared store, {differed} differed; {} reported and not reused here for {keep_days} days",
        if differed == 1 { "it is" } else { "they are" }
    )
}

/// Why a CI job on a listed branch isn't trusted, in plain words: the one place that says each
/// reason. `gitlab_job`: the run's credential is a GitLab job's ID token, which is what makes the
/// backend judge it by gitlab.com's rule (`build_trust`'s `gitlab`); only `event` differs, since a
/// listed branch on gitlab.com trusts a push, and a schedule, web or API pipeline only where the
/// project's admin chose the dashboard's Trust pipelines started by a person.
pub(crate) fn untrusted_words(why: blocks::Untrusted, gitlab_job: bool) -> &'static str {
    match why {
        blocks::Untrusted::Repository => {
            "the repository's owner isn't the one an admin confirmed (transferred, or made again)"
        }
        blocks::Untrusted::Event if gitlab_job => {
            "its pipeline isn't a push, nor a schedule, web or API run the project trusts (Trust pipelines started by a person)"
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
        blocks::Untrusted::Credential => {
            "it uses a person's or an agent's token in CI, not the job's own ID token"
        }
        // B-48, B-49: gitlab.com's, as the build page says them.
        blocks::Untrusted::Job => {
            "another request showed this job's ID token with another key, so the backend withdrew the job"
        }
        blocks::Untrusted::Token => "its ID token is older than a job can be",
        blocks::Untrusted::Fork => "it builds a fork's code",
        blocks::Untrusted::Source => "a policy or another source added this job",
        blocks::Untrusted::Unprotected => "its branch isn't protected in GitLab",
        blocks::Untrusted::Config => "its pipeline's configuration isn't the project's own file",
        blocks::Untrusted::User => {
            "whoever started, retried or played it isn't a Maintainer or an Owner"
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

/// Test only, never compiled into hushrun itself: where the relay panics, so that what a panic
/// leaves is tested. Armed only by a test that runs in a process of its own.
#[cfg(test)]
pub(crate) mod fault {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The thread answering a wrapper's fetch panics.
    pub(crate) static FETCH: AtomicBool = AtomicBool::new(false);
    /// The upload loop panics once `begin` answered, its claims held.
    pub(crate) static UPLOAD: AtomicBool = AtomicBool::new(false);
    /// Set just before the upload loop's panic.
    pub(crate) static UPLOAD_PANICKED: AtomicBool = AtomicBool::new(false);
    /// The run's lookup of stale vouches panics as it starts, once.
    pub(crate) static VOUCHES: AtomicBool = AtomicBool::new(false);
    /// Set just before that panic.
    pub(crate) static VOUCHES_PANICKED: AtomicBool = AtomicBool::new(false);

    pub(crate) fn fetch() {
        if FETCH.load(Ordering::Relaxed) {
            panic!("the test's panic: a fetch");
        }
    }

    pub(crate) fn vouches() {
        if VOUCHES.swap(false, Ordering::Relaxed) {
            VOUCHES_PANICKED.store(true, Ordering::Relaxed);
            panic!("the test's panic: a lookup of stale vouches");
        }
    }

    pub(crate) fn upload() {
        if UPLOAD.load(Ordering::Relaxed) {
            UPLOAD_PANICKED.store(true, Ordering::Relaxed);
            panic!("the test's panic: an upload");
        }
    }
}

#[cfg(test)]
mod tests;
