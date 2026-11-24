//! The launcher itself (B-50 to B-62): one C or C++ compiler call from start to exit status.
//!
//! [`call`] is the whole flow over [`Ports`]: the process's variables, the clock, running the
//! compiler. [`main`] wires the real ones; tests drive [`call`] with stand-ins. The flow has two
//! halves. Until the launcher writes a byte or starts a compile of its own, every failure, a
//! panic among them, replaces the process with the compiler exactly as asked (B-58). After, the
//! compile's status is the call's, and a failure only means nothing was stored.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

use super::args::{self, Classified, Compile, DepMode};
use super::compiler::{self, Identity, Said};
use super::key::{self, HintRead};
use super::memo::Digests;
use super::places::{self, Places};
use super::share::{self as relay_side, Waits};
use super::{CcRun, KIND, checks, depfile, graph};
use crate::model::{Class, Mode, NotStored, Outcome, PassedThrough};
use crate::origin::Origin;
use crate::relay::{Answer, Problem, Request};
use crate::session::{self, Current, Entry};
use crate::store::{NewFile, Published, Store};

/// The schema of a C or C++ block's description.
const DESCRIPTION_SCHEMA: &str = "hushrun.cc-block/v1";
/// The folder of the session the launcher's own files go in: its depfiles and include graphs.
const WORK: &str = "cc";

/// One compile the launcher starts and watches.
pub struct Run<'a> {
    /// The compiler.
    pub compiler: &'a OsStr,
    /// Its arguments.
    pub args: &'a [OsString],
    /// Variables set on top of the launcher's own.
    pub env: &'a [(OsString, OsString)],
    /// Keep its stderr without passing it on: the caller passes on what the person hasn't seen.
    pub quiet: bool,
}

/// How a compile ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    /// The exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    /// The signal that ended it.
    pub signal: Option<i32>,
    /// Its stdout, whole.
    pub stdout: Vec<u8>,
    /// Its stderr, as it was passed on.
    pub stderr: Vec<u8>,
}

impl Ran {
    fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// What the launcher needs from the world, so tests can stand in for it.
pub trait Ports {
    /// One variable of the process.
    fn var(&self, name: &str) -> Option<OsString>;
    /// The names of the process's variables.
    fn var_names(&self) -> Vec<OsString>;
    /// The working directory: the compiler's.
    fn cwd(&self) -> io::Result<PathBuf>;
    /// The clock.
    fn now(&self) -> SystemTime;
    /// The date the file system gives a file created in `dir` now (B-11); `None` where dates
    /// are exact or the file can't be made.
    fn file_clock(&self, dir: &Path) -> Option<SystemTime>;
    /// This process's id, for temporary names.
    fn pid(&self) -> u32;
    /// Replaces the process with the compiler exactly as asked; returns only when it can't.
    fn exec(&mut self, compiler: &OsStr, args: &[OsString]) -> i32;
    /// Runs a compile: its stderr is passed on line by line as it comes and kept, its stdout
    /// kept whole and not passed on.
    fn compile(&mut self, run: &Run<'_>) -> io::Result<Ran>;
    /// Runs a program for the compiler's identity (B-52).
    fn ask(&self, program: &Path, args: &[String]) -> Option<Said>;
    /// Writes to the launcher's stdout.
    fn stdout(&mut self, bytes: &[u8]);
    /// Writes to the launcher's stderr.
    fn stderr(&mut self, bytes: &[u8]);
    /// Whether the audit picks the reuse of block `key` in the run started at `started_ms`
    /// (B-59): as `reuse` says (`audit::sample`). Tests choose.
    fn audited(&self, key: &Digest, started_ms: u64, reuse: crate::audit::Reuse) -> bool {
        crate::audit::sample(key, started_ms, reuse)
    }
    /// Asks the run's relay at `socket` within `wait` (B-60); `None` is a miss.
    fn relay(&self, socket: &Path, request: &Request, wait: Duration) -> Option<Answer> {
        crate::relay::ask(socket, request, wait)
    }
    /// What opens a baked store's packs (B-64): the binary's, when it installed one. Tests stand
    /// in for it.
    fn unseal(&self) -> Option<&'static dyn crate::baked::Unseal> {
        crate::baked::unseal()
    }
    /// Hands the run's relay a notice; whether it was written.
    fn tell(&self, socket: &Path, request: &Request, wait: Duration) -> bool {
        crate::relay::tell(socket, request, wait)
    }
    /// Ends the process as a compile ended: its code, or its signal raised again.
    fn end_as(&mut self, ran: &Ran) -> i32 {
        ran.code.unwrap_or(1)
    }
}

/// What a C or C++ line of the run's log says beside the rest (B-61).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CcEntry {
    /// The source, from the root when under it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Why it passed through or wasn't stored, in the launcher's words (B-51, B-54).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// A probe of CMake's own configure (`try_compile`): counted nowhere.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub probe: bool,
    /// Why the block stays on this machine, when it was stored (B-56): `tied`, `home-in-output`,
    /// `base-in-output`, `native-cpu`, `cwd-outside-roots`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local: Vec<String>,
    /// The target triple the driver names (B-60).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub platform: String,
    /// How long the launcher took from its start to its line, compile included, in
    /// milliseconds: for a reuse, what a hit costs.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub took_ms: u64,
    /// The run had a relay (B-60): a block stored with reasons in `local` stays on this machine
    /// though the run shares, which the summary says.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The block's description, as the store keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Description {
    schema: String,
    kind: String,
    /// The first key.
    step: Digest,
    /// The source, with roots as placeholders.
    source: String,
    /// What the compile took, in milliseconds: what a reuse saves.
    compile_ms: u64,
    /// `anywhere` or `tied` (B-56).
    place: String,
    /// Why it may not leave the machine; empty when it may (B-56).
    local: Vec<String>,
    /// The target triple (B-60).
    platform: String,
    /// The repository that built it (B-13).
    repository: Option<String>,
}

/// One compiler call. The process's arguments are `[hushrun-cc, <compiler>, <args>…]`, as the
/// indirection passes them. Returns the compiler's status, or 0 for a reuse.
pub fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    // A panic must not print: the stderr the build sees is the compiler's (B-58).
    std::panic::set_hook(Box::new(|_| {}));
    let code = call(&mut Real, &argv);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// The whole flow of one call: `argv` is the compiler and its arguments. Returns the exit code.
pub fn call(ports: &mut dyn Ports, argv: &[OsString]) -> i32 {
    let Some((compiler, args)) = argv.split_first() else {
        ports.stderr(
            format!(
                "error: {} stands in front of a C or C++ compiler and is run by a build: it needs the compiler as its first argument\n",
                super::LAUNCHER_NAME
            )
            .as_bytes(),
        );
        return 2;
    };
    // B-62: no usable session, nothing else is read.
    let Some(current) = session::current(&|name| ports.var(name)) else {
        return ports.exec(compiler, args);
    };
    let Some(cc) = current.cc.clone() else {
        return ports.exec(compiler, args);
    };
    let start = ports.now();
    // B-58: the first half; a panic in it is a pass-through.
    let decided = catch_unwind(AssertUnwindSafe(|| {
        decide(ports, &current, &cc, compiler, args, start)
    }));
    match decided {
        Ok(Decided::Pass { why, probe }) => {
            log_pass(&current, why, probe, args);
            ports.exec(compiler, args)
        }
        Ok(Decided::Reused(reused)) => {
            let reused = *reused;
            ports.stdout(&reused.stdout);
            ports.stderr(&reused.stderr);
            current.log(&reused.entry);
            0
        }
        Ok(Decided::Compile(plan)) => compile(ports, &current, *plan, start),
        Err(_) => {
            log_pass(&current, "panic", false, args);
            ports.exec(compiler, args)
        }
    }
}

fn log_pass(current: &Current, why: &'static str, probe: bool, args: &[OsString]) {
    let source = args
        .iter()
        .rev()
        .filter_map(|a| a.to_str())
        .find(|a| {
            !a.starts_with('-')
                && (a.ends_with(".c")
                    || a.ends_with(".cc")
                    || a.ends_with(".cpp")
                    || a.ends_with(".cxx"))
        })
        .unwrap_or_default()
        .to_string();
    let reason = match why {
        "not-utf8" => PassedThrough::NotUtf8,
        "response-file" => PassedThrough::ArgFile,
        "compiler" => PassedThrough::Compiler,
        "store-read" | "store" => PassedThrough::Store,
        _ => PassedThrough::UnknownArgument,
    };
    current.log(&Entry {
        xcode: None,
        cc: Some(CcEntry {
            source,
            why: Some(why.to_string()),
            probe,
            ..CcEntry::default()
        }),
        ..entry(Outcome::PassedThrough(reason))
    });
}

/// A log line with nothing measured yet.
fn entry(outcome: Outcome) -> Entry {
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
        step: None,
        key: None,
        downloaded: false,
        baked: false,
        go: None,
        xcode: None,
        cc: None,
        gradle: None,
        maven: None,
        tsc: None,
    }
}

/// What the first half decided.
enum Decided {
    /// Run the compiler exactly as asked.
    Pass { why: &'static str, probe: bool },
    /// A block was put in place: write its messages.
    Reused(Box<Reused>),
    /// Compile, then store.
    Compile(Box<Plan>),
}

struct Reused {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    entry: Entry,
}

/// Everything a compile of the launcher's own needs.
struct Plan {
    compile: Compile,
    identity: Identity,
    places: Places,
    cwd: PathBuf,
    step: Digest,
    added: Vec<String>,
    /// A block found that this compile is compared with (observe, an audit).
    found: Option<Found>,
    /// The first key is suspended: compile, store nothing.
    suspended: bool,
    native: bool,
    digests: Digests,
    /// Why a block found was refused before this compile (B-59), for the run's log.
    note: Option<String>,
}

struct Found {
    key: Digest,
    object: PathBuf,
    /// The block was only downloaded: a difference is reported to the scope it came from.
    downloaded: bool,
}

macro_rules! pass {
    ($e:expr, $why:expr) => {
        match $e {
            Some(v) => v,
            None => {
                return Decided::Pass {
                    why: $why,
                    probe: false,
                }
            }
        }
    };
}

/// Whether a compile is one of CMake's own configure checks, run in a scratch folder with a
/// random name: storing it would only fill the store.
fn cmake_probe(cwd: &Path, output: &str) -> bool {
    let markers = [
        "/CMakeFiles/CMakeScratch/",
        "/CMakeFiles/CMakeTmp",
        "/CMakeFiles/CMakeTmp/",
    ];
    let cwd = format!("{}/", cwd.to_string_lossy());
    markers
        .iter()
        .any(|m| cwd.contains(m) || output.contains(m.trim_start_matches('/')))
}

fn decide(
    ports: &mut dyn Ports,
    current: &Current,
    cc: &CcRun,
    compiler: &OsString,
    args: &[OsString],
    start: SystemTime,
) -> Decided {
    let compile = match args::classify(compiler, args, &ports.var_names()) {
        Classified::Compile(compile) => *compile,
        Classified::PassThrough(why) => return Decided::Pass { why, probe: false },
    };
    let cwd = pass!(ports.cwd().ok(), "cwd");
    if cmake_probe(&cwd, &compile.output) {
        return Decided::Pass {
            why: "cmake-probe",
            probe: true,
        };
    }
    let store = Store::new(&current.store);
    let var = |name: &str| ports.var(name);
    let ask = |program: &Path, words: &[String]| ports.ask(program, words);
    let identity = match compiler::identity(
        compiler,
        compile.lang,
        &compile.driver_flags,
        &var,
        &store,
        &ask,
    ) {
        Ok(identity) => identity,
        Err(_) => {
            return Decided::Pass {
                why: "compiler",
                probe: false,
            };
        }
    };
    // GCC's link-time objects differ between two compiles of one file without a seed (B-51).
    if compile.lto && !compile.random_seed && !identity.clang {
        return Decided::Pass {
            why: "lto-seed",
            probe: false,
        };
    }
    let native = if compile.native.is_empty() {
        None
    } else {
        Some(pass!(
            compiler::native(
                compiler,
                &identity,
                compile.lang,
                &compile.native,
                &var,
                &current.folder,
                &ask
            ),
            "native"
        ))
    };
    let places = identity.places(&cc.roots);
    let cwd_text = pass!(cwd.to_str(), "not-utf8").to_string();
    let added = added_flags(cc, &places);
    let (step, body) = key::step(&key::Asked {
        compile: &compile,
        identity: &identity,
        places: &places,
        cwd: &cwd_text,
        native: native.as_deref(),
        added: &added,
        var: &var,
        extra: &[current.env.clone(), current.repository_env.clone()].concat(),
    });
    key_log(current, &step, Some(&body), None, &compile.source);
    let mut digests = Digests::new(Some(&current.folder));
    let suspended = store.suspended_at(&step, start).is_some();
    let mut found = None;
    let mut note = None;
    if !suspended {
        let looked = find(
            ports,
            current,
            &store,
            &step,
            &Seen {
                places: &places,
                identity: &identity,
                cwd: &cwd,
            },
            &mut digests,
            start,
        );
        digests.flush();
        match looked {
            Ok(Some(hit)) => match vet(
                ports, current, &store, &step, &hit, &compile, &identity, start,
            ) {
                Vetted::Serve { first_download } => {
                    // B-63: one in ten only where nothing verified where the block came from;
                    // never at a verified baked block's first reuse here.
                    let origin = hit.origin.as_ref().filter(|_| first_download);
                    let audited = current.audits_reuse(origin, |reuse| {
                        ports.audited(&hit.key, current.started_ms, reuse)
                    });
                    if current.mode == Mode::On && !audited {
                        // A restore that fails before any message compiles as asked (B-57).
                        let at = Restore {
                            compile: &compile,
                            places: &places,
                            cwd: &cwd,
                            step: &step,
                            platform: &identity.target,
                        };
                        return match restore(ports, current, &at, &hit, start) {
                            Ok(reused) => {
                                if first_download {
                                    first_reuse(&store, &hit.key);
                                }
                                Decided::Reused(Box::new(reused))
                            }
                            Err(_) => Decided::Pass {
                                why: "restore",
                                probe: false,
                            },
                        };
                    }
                    found = Some(Found {
                        key: hit.key,
                        object: hit.object.clone(),
                        downloaded: hit.downloaded(),
                    });
                }
                Vetted::Refused(why) => note = why,
            },
            Ok(None) => {}
            Err(_) => {
                return Decided::Pass {
                    why: "store-read",
                    probe: false,
                };
            }
        }
    }
    Decided::Compile(Box::new(Plan {
        native: native.is_some(),
        compile,
        identity,
        places,
        cwd,
        step,
        added,
        found,
        suspended,
        digests,
        note,
    }))
}

/// What the launcher adds to a compile it handles (B-55): a prefix map per form of the root, the
/// shortest first, so that the inner one wins where the compiler takes the last match.
fn added_flags(cc: &CcRun, places: &Places) -> Vec<String> {
    if !cc.maps {
        return Vec::new();
    }
    let mut roots: Vec<&String> = places.roots().iter().collect();
    roots.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
    roots
        .into_iter()
        .map(|root| format!("-ffile-prefix-map={root}=."))
        .collect()
}

/// A block a hint led to, before its checks (B-59).
struct Hit {
    key: Digest,
    object: PathBuf,
    /// The object's size and SHA-256, as the block's record says.
    size: u64,
    sha256: Digest,
    stdout: Option<PathBuf>,
    stderr: Option<PathBuf>,
    hint: key::Hint,
    compile_ms: u64,
    /// Why the block stays on the machine it was built on, as its description says (B-56).
    local: Vec<String>,
    /// Where it came from (B-32).
    origin: Option<Origin>,
    /// The digest of its outputs, as the shared store's rows hold it (B-28).
    outputs: hushrun_core::blocks::Hex32,
    /// The relay downloaded it for this compile.
    downloaded_now: bool,
    /// It was opened from the run's baked store for this compile (B-64).
    baked_now: bool,
}

impl Hit {
    /// Whether the block was only downloaded: no compile here made it.
    fn downloaded(&self) -> bool {
        self.origin.as_ref().is_some_and(Origin::downloaded_only)
    }
}

/// What a lookup reads its hints against: this call's places, compiler and folder.
struct Seen<'a> {
    places: &'a Places,
    identity: &'a Identity,
    cwd: &'a Path,
}

/// The most second keys one fetch names (the `rustc` wrapper's limit).
const MOST_FETCH_KEYS: usize = 8;
/// The most reads, places and probes a downloaded hint may make a lookup hash or look at: a hint
/// is untrusted bytes, and what it names sets a lookup's cost. A unit of abseil's tests names
/// about 1,100 reads and 13,000 places.
const MOST_DOWNLOADED_READS: usize = 20_000;
const MOST_DOWNLOADED_PLACES: usize = 500_000;
const MOST_DOWNLOADED_PROBES: usize = 100_000;

/// Whether a downloaded hint is within what a lookup follows.
fn within_budget(hint: &key::Hint) -> bool {
    let places: usize = hint
        .places
        .iter()
        .map(|(dirs, names)| dirs.len().saturating_mul(names.len()))
        .fold(0, usize::saturating_add);
    hint.reads.len() <= MOST_DOWNLOADED_READS
        && places <= MOST_DOWNLOADED_PLACES
        && hint.probes.len() <= MOST_DOWNLOADED_PROBES
}

/// Whether every path a downloaded hint names lies under this call's roots, bases or system
/// folders once made absolute (a `..` taken out by its text): a hint is a member's bytes, and one naming `/etc/…` or
/// `{root}/../..` would make this machine hash a file it never reads, or say by a lookup's
/// outcome whether a file exists there.
fn names_only_known_folders(hint: &key::Hint, seen: &Seen<'_>) -> bool {
    let known = |text: &str, folder: bool| {
        let shown = seen.places.show(text);
        let path = if folder {
            places::absolute(seen.cwd, &shown)
        } else {
            key::read_path(
                &key::HintRead {
                    p: text.to_string(),
                    s: false,
                },
                seen.places,
                seen.cwd,
            )
        };
        let text = path.to_string_lossy();
        seen.places.under_root(&text) || is_base(&path, seen.places, seen.identity)
    };
    hint.reads().iter().all(|r| known(&r.p, false))
        && hint
            .places
            .iter()
            .flat_map(|(dirs, _)| dirs)
            .chain(hint.probes.iter().map(|(d, _, _)| d))
            .all(|d| known(hint.dirs.get(*d as usize).map_or("", String::as_str), true))
}

/// The block for this compile, by its first key's hints (B-54, B-60): this machine's own first,
/// then those downloaded from the shared store, asked of the run's relay when the store has
/// none; and when one leads to a second key the store lacks, a fetch of it. The relay looks a
/// first key up only when the check's `cc/1` manifest names it, so a cold build asks the
/// network nothing per compile.
fn find(
    ports: &mut dyn Ports,
    current: &Current,
    store: &Store,
    step: &Digest,
    seen: &Seen<'_>,
    digests: &mut Digests,
    at: SystemTime,
) -> Result<Option<Hit>, crate::store::StoreError> {
    let mut candidates: Vec<(Digest, key::Hint)> = Vec::new();
    let own = store.hints(step);
    if let Some(hit) = follow(store, step, &own, false, seen, digests, &mut candidates)? {
        return Ok(Some(hit));
    }
    let target = &seen.identity.target;
    // A first key kept on this machine asks the shared store nothing (B-34), nor the baked one.
    if store.kept_local_at(step, at).is_some() {
        return Ok(None);
    }
    // B-64: the run's baked store, for a target whose objects can be checked (B-59).
    let unseal = ports.unseal();
    let lower = current
        .lower_for(crate::model::Kind::Cc)
        .filter(|lower| lower.reads_for(crate::model::Kind::Cc, target) && checks::knows(target))
        .zip(unseal)
        .map(|(lower, unseal)| crate::baked::Reader::new(lower, unseal));
    if !current.shares() && lower.is_none() {
        return Ok(None);
    }
    // Sharing is never above the local mode, and only `on` reuses a download (B-33); a target
    // whose objects can't be checked is never served one (B-59).
    let asks = current.mode == Mode::On
        && current.share.relay().is_some()
        && checks::knows(target)
        && hushrun_core::blocks::is_platform(target);
    let waits = Waits::of(&|name| ports.var(name));
    let local_ms = store.step_cost(step);
    let mut downloaded = store.shared_hints(step);
    if downloaded.is_empty()
        && let Some(lower) = &lower
        && lower.hints_into(store, crate::model::Kind::Cc, step, at) > 0
    {
        downloaded = store.shared_hints(step);
    }
    if downloaded.is_empty() && asks {
        let asked = Request::CcHints {
            step: *step,
            platform: target.clone(),
            local_ms,
        };
        let answer = relay_side::ask(
            current,
            &asked,
            waits.within(local_ms),
            &waits,
            &mut |socket, request, wait| ports.relay(socket, request, wait),
        );
        if matches!(answer, Some(Answer::Hints { count }) if count > 0) {
            downloaded = store.shared_hints(step);
        }
    }
    if let Some(hit) = follow(
        store,
        step,
        &downloaded,
        true,
        seen,
        digests,
        &mut candidates,
    )? {
        return Ok(Some(hit));
    }
    candidates.truncate(MOST_FETCH_KEYS);
    if let Some(lower) = &lower {
        let floor = u64::from(current.min_free_gb).saturating_mul(1 << 30);
        for (key, hint) in &candidates {
            let opened = lower
                .open_into(
                    store,
                    crate::model::Kind::Cc,
                    key,
                    step,
                    floor,
                    current.repository.as_deref(),
                    at,
                )
                .unwrap_or(false);
            if opened && let Some(hit) = hit_of(store, *key, hint)? {
                return Ok(Some(Hit {
                    baked_now: true,
                    ..hit
                }));
            }
        }
    }
    if !asks || candidates.is_empty() {
        return Ok(None);
    }
    let asked = Request::CcFetch {
        step: *step,
        keys: candidates.iter().map(|(key, _)| *key).collect(),
        platform: target.clone(),
        local_ms,
    };
    let answer = relay_side::ask(
        current,
        &asked,
        waits.within(local_ms),
        &waits,
        &mut |socket, request, wait| ports.relay(socket, request, wait),
    );
    let Some(Answer::Ready { key }) = answer else {
        return Ok(None);
    };
    let Some((_, hint)) = candidates.into_iter().find(|(k, _)| *k == key) else {
        return Ok(None);
    };
    Ok(hit_of(store, key, &hint)?.map(|hit| Hit {
        downloaded_now: true,
        ..hit
    }))
}

/// Follows `hints`, the newest first (B-54): each read hashed as it is now, each place and probe
/// checked. The first that names a stored block of this kind wins; a second key the store lacks
/// is kept in `candidates`, for the relay to fetch. `downloaded`: the hints came from the shared
/// store, and one past [`within_budget`] is passed over.
fn follow(
    store: &Store,
    step: &Digest,
    hints: &[Vec<u8>],
    downloaded: bool,
    seen: &Seen<'_>,
    digests: &mut Digests,
    candidates: &mut Vec<(Digest, key::Hint)>,
) -> Result<Option<Hit>, crate::store::StoreError> {
    'hints: for bytes in hints {
        let Some(hint) = key::Hint::read(bytes) else {
            continue;
        };
        if downloaded && !(within_budget(&hint) && names_only_known_folders(&hint, seen)) {
            continue;
        }
        let reads = hint.reads();
        let mut found = Vec::with_capacity(reads.len());
        for read in &reads {
            let path = key::read_path(read, seen.places, seen.cwd);
            let base = is_base(&path, seen.places, seen.identity);
            match digests.digest(&path, base) {
                Ok(digest) => found.push(digest),
                Err(_) => continue 'hints,
            }
        }
        if !key::places_hold(&hint, seen.places) {
            continue;
        }
        let block_key = key::block(step, &hint, &found);
        let keys = [block_key, key::tied(&block_key, seen.places)];
        for key in keys {
            if let Some(hit) = hit_of(store, key, &hint)? {
                return Ok(Some(hit));
            }
        }
        for key in keys {
            if !candidates.iter().any(|(k, _)| *k == key) {
                candidates.push((key, hint.clone()));
            }
        }
    }
    Ok(None)
}

/// The stored block under `key` as a hit for `hint`, when it is a C or C++ block with an object.
fn hit_of(
    store: &Store,
    key: Digest,
    hint: &key::Hint,
) -> Result<Option<Hit>, crate::store::StoreError> {
    let Some(block) = store.block(&key)? else {
        return Ok(None);
    };
    if block.description.get("kind").and_then(|k| k.as_str()) != Some(KIND) {
        return Ok(None);
    }
    let file = |name: &str| block.files.iter().find(|f| f.name == name);
    let Some(object) = file("object") else {
        return Ok(None);
    };
    // What the compile took: the relay's word for a download (the uploader's, as the server
    // states it), else the launcher's own; a download's pack carries no `compile_ms`.
    let compile_ms = ["rustc_ms", "compile_ms"]
        .iter()
        .find_map(|name| block.description.get(*name).and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let local = block
        .description
        .get("local")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default();
    let outputs = hushrun_core::blocks::outputs_digest(
        &block
            .files
            .iter()
            .map(|f| hushrun_core::blocks::OutputFile {
                name: &f.name,
                size: f.size,
                sha256: hushrun_core::blocks::Hex32(*f.sha256.as_bytes()),
            })
            .collect::<Vec<_>>(),
    );
    Ok(Some(Hit {
        key,
        object: object.path.clone(),
        size: object.size,
        sha256: object.sha256,
        stdout: file("stdout").map(|f| f.path.clone()),
        stderr: file("stderr").map(|f| f.path.clone()),
        hint: hint.clone(),
        compile_ms,
        local,
        origin: store.origin(&key),
        outputs,
        downloaded_now: false,
        baked_now: false,
    }))
}

/// What the checks of a block found (B-59).
enum Vetted {
    /// It may be served; `first_download`: a downloaded block's first reuse here.
    Serve { first_download: bool },
    /// It was not: compile, with the reason for the log when there is one to say.
    Refused(Option<String>),
}

/// The checks of a block before it is served (B-59): who vouches for it ([`Shared::may_reuse`]),
/// its object's size and SHA-256 against its record, and for a block only downloaded, its bytes
/// an object of the compile's kind. A block whose bytes fail is removed; one only downloaded is
/// first reported `rejected` to the scope its record names, and its first key suspended for the
/// store's kept days.
///
/// [`Shared::may_reuse`]: crate::session::Shared::may_reuse
#[allow(clippy::too_many_arguments)]
fn vet(
    ports: &mut dyn Ports,
    current: &Current,
    store: &Store,
    step: &Digest,
    hit: &Hit,
    compile: &Compile,
    identity: &Identity,
    at: SystemTime,
) -> Vetted {
    let downloaded = hit.downloaded();
    let waits = Waits::of(&|name| ports.var(name));
    let vouch_wait = waits.within(Some(hit.compile_ms));
    let vouch = Request::CcVouch {
        key: hit.key,
        outputs: hit.outputs,
    };
    let reusable = current.may_reuse(
        hit.origin.as_ref(),
        epoch_ms(at),
        vouch,
        &mut |_, request| {
            let wait = if matches!(request, Request::Sharing) {
                waits.quick
            } else {
                vouch_wait
            };
            relay_side::ask(
                current,
                request,
                wait,
                &waits,
                &mut |socket, request, wait| ports.relay(socket, request, wait),
            )
        },
    );
    // A download for a target whose objects can't be checked is never served, and is no
    // sign of anything wrong.
    if !reusable || (downloaded && !checks::knows(&identity.target)) {
        return Vetted::Refused(None);
    }
    let bytes = match std::fs::read(&hit.object) {
        Ok(bytes) => bytes,
        Err(_) => return Vetted::Refused(None),
    };
    let wrong = if bytes.len() as u64 != hit.size || Digest::of(&bytes) != hit.sha256 {
        Some("wrong-sha256")
    } else if downloaded {
        checks::object(&bytes, &identity.target, compile.lto && identity.clang)
            .err()
            .map(checks::Wrong::word)
    } else {
        None
    };
    let Some(wrong) = wrong else {
        let first_download = downloaded && hit.origin.as_ref().is_some_and(|o| !o.reused);
        return Vetted::Serve { first_download };
    };
    if downloaded {
        // Reported before the block goes: the relay reads the scope from its record.
        let _ = relay_side::ask(
            current,
            &Request::CcReport {
                key: hit.key,
                problem: Problem::Rejected,
            },
            waits.quick,
            &waits,
            &mut |socket, request, wait| ports.relay(socket, request, wait),
        );
        let until = at + Duration::from_secs(u64::from(current.keep_days) * 86_400);
        let _ = store.suspend_until(
            step,
            "a downloaded C or C++ object failed its checks",
            until,
            at,
        );
    }
    let _ = store.discard(&hit.key);
    Vetted::Refused(Some(format!(
        "a {} C or C++ object in the store was refused: {wrong}",
        if downloaded { "downloaded" } else { "stored" }
    )))
}

/// Notes a downloaded block's first reuse here in its record (B-34): the audit picks it one time
/// in ten until then.
fn first_reuse(store: &Store, key: &Digest) {
    let _ = store.update_origin(key, |origin| {
        let mut origin = origin?;
        (!origin.reused).then(|| {
            origin.reused = true;
            origin
        })
    });
}

fn epoch_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Whether a read lies under the SDK, the toolchain or a system folder: a file the run memo may
/// answer for by its stat (B-52).
fn is_base(path: &Path, places: &Places, identity: &Identity) -> bool {
    let text = path.to_string_lossy();
    (places.under_any(&text) && !places.under_root(&text)) || identity.in_system(&text)
}

/// What a reuse puts in place, and for which compile.
#[derive(Clone, Copy)]
struct Restore<'a> {
    compile: &'a Compile,
    places: &'a Places,
    cwd: &'a Path,
    /// The first key, for the run's log and the check's manifest (B-60).
    step: &'a Digest,
    /// The target triple: the manifest the reuse goes in.
    platform: &'a str,
}

/// Puts a block in place (B-57): the object under a temporary name in its folder, checked
/// against the digest its record holds, made writable, dated now and renamed into place; then
/// the depfile from the hint with this checkout's paths, written whole: the files this machine
/// hashed for the key, never anything of a download's. The messages are read now and written by
/// the caller; a downloaded block's keep only their colour (B-59).
fn restore(
    ports: &mut dyn Ports,
    current: &Current,
    at: &Restore<'_>,
    hit: &Hit,
    start: SystemTime,
) -> io::Result<Reused> {
    let Restore {
        compile,
        places,
        cwd,
        step,
        platform,
    } = *at;
    let output = cwd.join(&compile.output);
    let expected = hit.sha256;
    put_whole(&output, start, |temp| {
        std::fs::copy(&hit.object, temp)?;
        let (copied, _) = crate::regular::hash_file(temp)?;
        if copied != expected {
            return Err(io::Error::other("the copy is not the block's bytes"));
        }
        Ok(())
    })?;
    if let Some(path) = compile.dep.path(&compile.output) {
        let reads: Vec<depfile::Read> = hit
            .hint
            .reads()
            .iter()
            .map(|r| depfile::Read {
                path: places.show(&r.p),
                system: r.s,
            })
            .collect();
        let text = depfile::render(&compile.dep, &compile.output, &reads);
        let path = cwd.join(path);
        put_whole(&path, start, |temp| std::fs::write(temp, text.as_bytes()))?;
    }
    let downloaded = hit.downloaded();
    let read = |p: &Option<PathBuf>| -> io::Result<Vec<u8>> {
        match p {
            Some(p) => {
                let shown = places.show_bytes(&std::fs::read(p)?);
                Ok(if downloaded {
                    checks::sgr_only(&shown)
                } else {
                    shown
                })
            }
            None => Ok(Vec::new()),
        }
    };
    let stdout = read(&hit.stdout)?;
    let stderr = read(&hit.stderr)?;
    let store = Store::new(&current.store);
    let _ = store.used(&hit.key, start);
    let took = ports
        .now()
        .duration_since(start)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    Ok(Reused {
        stdout,
        stderr,
        entry: Entry {
            baked: hit.baked_now,
            ..reused_entry(
                *step,
                hit.key,
                CcEntry {
                    source: places.hide(&compile.source),
                    platform: platform.to_string(),
                    took_ms: took,
                    local: hit.local.clone(),
                    shared: current.share.relay().is_some(),
                    ..CcEntry::default()
                },
                hit.compile_ms.saturating_sub(took),
                hit.downloaded_now,
            )
        },
    })
}

/// The run's log line of a reuse (B-61), as the relay's close reads it (B-60): with its first
/// key and its block's key, so that the check's manifest pairs them, and its `cc` part naming
/// the target triple, whose manifest it goes in.
pub fn reused_entry(
    step: Digest,
    key: Digest,
    cc: CcEntry,
    saved_ms: u64,
    downloaded: bool,
) -> Entry {
    Entry {
        saved_ms,
        found: true,
        downloaded,
        step: Some(step),
        key: Some(key),
        xcode: None,
        cc: Some(cc),
        ..entry(Outcome::Reused)
    }
}

/// Writes `dest` whole: `fill` writes a temporary file beside it, which is given the mode a file
/// the compiler made would have (`0666` less the umask), dated `when` and renamed into place. A
/// failure removes the temporary file and leaves `dest` as it was. The temporary file's name is
/// the same for every launcher writing `dest` (Ninja and Make never run two for one output), so
/// one a killed launcher left is replaced by the next write, not left behind.
fn put_whole(
    dest: &Path,
    when: SystemTime,
    fill: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dest.with_file_name(format!(".{name}.hushrun.tmp"));
    let done = (|| {
        match std::fs::remove_file(&temp) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        fill(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o666 & !umask()))?;
        }
        std::fs::File::options()
            .write(true)
            .open(&temp)?
            .set_modified(when)?;
        std::fs::rename(&temp, dest)
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    done
}

/// The process's umask. Reading it sets it, so it is set back at once; the launcher has no other
/// thread then.
#[cfg(unix)]
#[allow(
    clippy::useless_conversion,
    reason = "a mode is a u16 on macOS and a u32 on Linux"
)]
fn umask() -> u32 {
    use rustix::fs::Mode;
    let old = rustix::process::umask(Mode::from_raw_mode(0o022));
    rustix::process::umask(old);
    old.as_raw_mode().into()
}

/// Whether the session's folder for the launcher's own files is there, made in `session` when it
/// isn't; `false` when `session` itself is gone.
fn work_folder(session: &Path) -> bool {
    match std::fs::create_dir(session.join(WORK)) {
        Ok(()) => true,
        Err(err) => err.kind() == io::ErrorKind::AlreadyExists,
    }
}

/// A file of the session's for this compile: its depfile or its include graph.
fn work_file(current: &Current, pid: u32, start: SystemTime, suffix: &str) -> PathBuf {
    let nanos = start.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    current
        .folder
        .join(WORK)
        .join(format!("{pid}-{nanos}.{suffix}"))
}

/// The compile's arguments (B-55): the call's without its depfile flags, then the launcher's own
/// depfile, which names the call's targets, then the prefix maps.
fn compile_args(compile: &Compile, depfile: &Path, added: &[String]) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::with_capacity(compile.args.len() + 8);
    let mut skip = false;
    for arg in &compile.args {
        if skip {
            skip = false;
            continue;
        }
        match arg.as_str() {
            "-MD" | "-MMD" | "-MP" => continue,
            "-MF" | "-MT" | "-MQ" => {
                skip = true;
                continue;
            }
            a if a.starts_with("-MF") || a.starts_with("-MT") || a.starts_with("-MQ") => continue,
            _ => out.push(OsString::from(arg)),
        }
    }
    out.push("-MD".into());
    out.push("-MF".into());
    out.push(depfile.as_os_str().to_owned());
    for target in &compile.dep.targets {
        match target {
            args::DepTarget::Plain(t) => {
                out.push("-MT".into());
                out.push(t.into());
            }
            args::DepTarget::Quoted(t) => {
                out.push("-MQ".into());
                out.push(t.into());
            }
        }
    }
    if compile.dep.phony {
        out.push("-MP".into());
    }
    out.extend(added.iter().map(OsString::from));
    out
}

/// The folder of the compile's source, absolute: it stands for an including file's where the
/// include graph names none (B-54).
fn source_dir(cwd: &Path, source: &str) -> String {
    places::absolute(cwd, source)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether `object` holds `text` as its bytes or as a string literal of wider characters would
/// spell it: UTF-16 and UTF-32, each byte order (B-56). A root in `L"…"`, `u"…"` or `U"…"` is
/// one the narrow search can't see.
pub(crate) fn holds_text(object: &[u8], text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let found = |needle: &[u8]| memchr::memmem::find(object, needle).is_some();
    if found(text.as_bytes()) {
        return true;
    }
    let units16: Vec<u16> = text.encode_utf16().collect();
    let le16: Vec<u8> = units16.iter().flat_map(|u| u.to_le_bytes()).collect();
    let be16: Vec<u8> = units16.iter().flat_map(|u| u.to_be_bytes()).collect();
    let le32: Vec<u8> = text
        .chars()
        .flat_map(|c| u32::from(c).to_le_bytes())
        .collect();
    let be32: Vec<u8> = text
        .chars()
        .flat_map(|c| u32::from(c).to_be_bytes())
        .collect();
    [le16, be16, le32, be32].iter().any(|needle| found(needle))
}

/// Whether a macro the arguments define (`-DNAME=value`, `-D NAME=value`) holds a root: the code
/// may turn it into bytes no search finds (B-56).
fn defines_a_root(keyed: &[String], roots: &[String]) -> bool {
    let mut defines = Vec::new();
    let mut next = false;
    for arg in keyed {
        if next {
            defines.push(arg.as_str());
            next = false;
        } else if arg == "-D" {
            next = true;
        } else if let Some(value) = arg.strip_prefix("-D") {
            defines.push(value);
        }
    }
    defines.iter().any(|d| {
        roots
            .iter()
            .any(|root| !root.is_empty() && d.contains(root.as_str()))
    })
}

/// The second half: the launcher's own compile, the person's depfile, and the store.
fn compile(ports: &mut dyn Ports, current: &Current, mut plan: Plan, start: SystemTime) -> i32 {
    let pid = ports.pid();
    let deps = work_file(current, pid, start, "d");
    let graph_file = work_file(current, pid, start, "json");
    // The session's folder is the run's: one gone (the run ended) is not made again here.
    if !work_folder(&current.folder) {
        log_pass(current, "session", false, &[]);
        return ports.exec(&plan.compile.compiler, &os(&plan.compile.args));
    }
    let args = compile_args(&plan.compile, &deps, &plan.added);
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    // `-H` prints the include tree to stderr, which the graph's variables would turn into the
    // graph's JSON there: such a compile has no graph, as GCC's hasn't (B-55).
    if plan.identity.clang && !plan.compile.prints_headers {
        env.push((graph::FORMAT.0.into(), graph::FORMAT.1.into()));
        env.push((graph::FILTERING.0.into(), graph::FILTERING.1.into()));
        env.push((graph::FILE.into(), graph_file.clone().into_os_string()));
    }
    // What the compile reads is dated against this, a date of the file system's (B-11).
    let floor = crate::inputs::floor(ports.file_clock(&current.folder), ports.now());
    let compile_start = ports.now();
    let ran = ports.compile(&Run {
        compiler: &plan.compile.compiler,
        args: &args,
        env: &env,
        quiet: false,
    });
    let ran = match ran {
        Ok(ran) => ran,
        Err(_) => {
            // Nothing started: the compiler as asked, as a pass-through.
            cleanup(&[&deps, &graph_file]);
            log_pass(current, "spawn", false, &[]);
            return ports.exec(&plan.compile.compiler, &os(&plan.compile.args));
        }
    };
    let compile_ms = ports
        .now()
        .duration_since(compile_start)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    ports.stdout(&ran.stdout);
    let source = plan.places.hide(&plan.compile.source);
    if !ran.success() {
        cleanup(&[&deps, &graph_file]);
        current.log(&Entry {
            rustc_ms: compile_ms,
            xcode: None,
            cc: Some(CcEntry {
                source,
                why: Some("compile-failed".into()),
                ..CcEntry::default()
            }),
            ..entry(Outcome::BuiltNotStored(NotStored::RustcFailed))
        });
        return ports.end_as(&ran);
    }
    let dep_text = std::fs::read_to_string(&deps).ok();
    // The person's depfile, as the compiler would have written it (B-57).
    if let Some(path) = plan.compile.dep.path(&plan.compile.output) {
        let written = dep_text.as_deref().and_then(|text| {
            let text = match plan.compile.dep.mode {
                DepMode::User => {
                    let dirs =
                        key::search(&plan.compile, &plan.identity, &plan.cwd, &|n| ports.var(n));
                    let source_dir = source_dir(&plan.cwd, &plan.compile.source);
                    let system = |p: &str| {
                        let abs = places::absolute(&plan.cwd, p)
                            .to_string_lossy()
                            .into_owned();
                        key::system_read(&abs, &plan.identity, &dirs, &source_dir)
                    };
                    depfile::without_system(text, &plan.compile.dep, &plan.compile.output, &system)?
                }
                _ => text.to_string(),
            };
            put_whole(&plan.cwd.join(&path), ports.now(), |temp| {
                std::fs::write(temp, text.as_bytes())
            })
            .ok()
        });
        if written.is_none() {
            // Without it the build can't tell what the object depends on: the compiler as asked
            // writes its own (B-58).
            cleanup(&[&deps, &graph_file]);
            let again = ports.compile(&Run {
                compiler: &plan.compile.compiler,
                args: &os(&plan.compile.args),
                env: &[],
                quiet: true,
            });
            current.log(&Entry {
                rustc_ms: compile_ms,
                xcode: None,
                cc: Some(CcEntry {
                    source,
                    why: Some("depfile".into()),
                    ..CcEntry::default()
                }),
                ..entry(Outcome::BuiltNotStored(NotStored::DepInfo))
            });
            return match again {
                Ok(again) => {
                    // The first compile's messages were passed on as they came: only what this
                    // one says beyond them is new (the depfile's error, at the end).
                    ports.stderr(unseen(&ran.stderr, &again.stderr));
                    ports.stdout(&again.stdout);
                    ports.end_as(&again)
                }
                Err(_) => 1,
            };
        }
    }
    let graph_text = std::fs::read_to_string(&graph_file).ok();
    cleanup(&[&deps, &graph_file]);
    let stored = catch_unwind(AssertUnwindSafe(|| {
        store_block(
            ports,
            current,
            &mut plan,
            dep_text.as_deref(),
            graph_text.as_deref(),
            &ran,
            floor,
            compile_ms,
        )
    }));
    let (outcome, mut cc_entry, key, stored_bytes, differed) = match stored {
        Ok(stored) => stored,
        Err(_) => (
            Outcome::BuiltNotStored(NotStored::StoreRefused),
            CcEntry {
                why: Some("panic".into()),
                ..CcEntry::default()
            },
            None,
            0,
            false,
        ),
    };
    cc_entry.source = source;
    cc_entry.platform = plan.identity.target.clone();
    cc_entry.shared = current.share.relay().is_some();
    cc_entry.took_ms = ports
        .now()
        .duration_since(start)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    current.log(&Entry {
        rustc_ms: compile_ms,
        found: plan.found.is_some(),
        differed,
        step: Some(plan.step),
        key,
        stored_bytes,
        note: plan.note.take(),
        xcode: None,
        cc: Some(cc_entry),
        ..entry(outcome)
    });
    0
}

/// What `again` says beyond the lines `seen` already passed on: the part after the longest run
/// of whole lines both start with.
fn unseen<'a>(seen: &[u8], again: &'a [u8]) -> &'a [u8] {
    let mut common = 0;
    for (i, (a, b)) in seen.iter().zip(again).enumerate() {
        if a != b {
            break;
        }
        if *a == b'\n' {
            common = i + 1;
        }
    }
    &again[common..]
}

fn os(args: &[String]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn cleanup(files: &[&Path]) {
    for file in files {
        let _ = std::fs::remove_file(file);
    }
}

type StoredResult = (Outcome, CcEntry, Option<Digest>, u64, bool);

fn not_stored(why: NotStored, words: &str) -> StoredResult {
    (
        Outcome::BuiltNotStored(why),
        CcEntry {
            why: Some(words.to_string()),
            ..CcEntry::default()
        },
        None,
        0,
        false,
    )
}

/// After a compile that succeeded: hashes what it read, decides where the block may go, and
/// stores it under its second key with the hint under its first (B-54, B-56).
#[allow(clippy::too_many_arguments)]
fn store_block(
    ports: &mut dyn Ports,
    current: &Current,
    plan: &mut Plan,
    dep_text: Option<&str>,
    graph_text: Option<&str>,
    ran: &Ran,
    floor: SystemTime,
    compile_ms: u64,
) -> StoredResult {
    let store = Store::new(&current.store);
    // Suspended before the compile, or by a refused download's checks since (B-59).
    if plan.suspended || store.suspended_at(&plan.step, ports.now()).is_some() {
        return not_stored(NotStored::Suspended, "suspended");
    }
    let Some(spellings) = dep_text.and_then(depfile::reads) else {
        return not_stored(NotStored::DepInfo, "depfile");
    };
    let cwd = plan.cwd.clone();
    let includers: HashMap<PathBuf, Vec<String>> = graph_text
        .and_then(graph::read)
        .map(|graph| {
            graph
                .into_iter()
                .map(|(file, by)| {
                    (
                        places::absolute(&cwd, &file),
                        by.iter()
                            .map(|b| places::absolute(&cwd, b).to_string_lossy().into_owned())
                            .collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let dirs = key::search(&plan.compile, &plan.identity, &cwd, &|n| ports.var(n));
    let source_epoch = ports.var("SOURCE_DATE_EPOCH").is_some();
    let source_dir = source_dir(&cwd, &plan.compile.source);
    // A time macro in the arguments (`-DBUILD=__DATE__`) is one as much as in a read (B-54).
    let asked = super::text::scan(plan.compile.keyed.join("\n").as_bytes());
    if asked.timestamp || (asked.time && !source_epoch) {
        return not_stored(NotStored::Unkeyable, "time-macro");
    }
    let mut reads = Vec::with_capacity(spellings.len());
    let mut digests = Vec::with_capacity(spellings.len());
    let mut abs_paths = Vec::with_capacity(spellings.len());
    let mut seens = Vec::with_capacity(spellings.len());
    for spelling in &spellings {
        if spelling.ends_with(".gch") || spelling.ends_with(".pch") || spelling.ends_with(".pcm") {
            return not_stored(NotStored::Unkeyable, "pch");
        }
        let abs = places::absolute(&cwd, spelling);
        let Some(abs_text) = abs.to_str().map(str::to_string) else {
            return not_stored(NotStored::Unkeyable, "not-utf8");
        };
        let base = is_base(&abs, &plan.places, &plan.identity);
        if !(plan.places.under_root(&abs_text) || base) {
            return not_stored(NotStored::Unkeyable, "outside-roots");
        }
        let hashed = match plan.digests.read(&abs, base) {
            Ok(hashed) => hashed,
            Err(err) if crate::regular::is_not_regular(&err) => {
                return not_stored(NotStored::NotAFile, "not-a-file");
            }
            Err(_) => return not_stored(NotStored::Changed, "changed"),
        };
        if crate::inputs::dated_after(&hashed.stat, floor) {
            return not_stored(NotStored::Changed, "changed");
        }
        let seen = hashed.seen.clone().unwrap_or_default();
        if seen.computed {
            return not_stored(NotStored::Unkeyable, "computed-probe");
        }
        if seen.timestamp || (seen.time && !source_epoch) {
            return not_stored(NotStored::Unkeyable, "time-macro");
        }
        if seen.incbin {
            return not_stored(NotStored::Unkeyable, "incbin");
        }
        if seen.has_embed {
            return not_stored(NotStored::Unkeyable, "has-embed");
        }
        let system = key::system_read(&abs_text, &plan.identity, &dirs, &source_dir);
        reads.push(HintRead {
            p: plan.places.hide(spelling),
            s: system,
        });
        digests.push(hashed.digest);
        abs_paths.push(abs_text);
        seens.push(seen);
    }
    let key_reads: Vec<key::Read<'_>> = abs_paths
        .iter()
        .zip(&seens)
        .map(|(abs, seen)| key::Read {
            abs,
            seen,
            includers: includers.get(Path::new(abs)).cloned().unwrap_or_default(),
        })
        .collect();
    let output = cwd.join(&plan.compile.output);
    let written: Vec<String> = [
        Some(output.clone()),
        plan.compile
            .dep
            .path(&plan.compile.output)
            .map(|d| cwd.join(d)),
    ]
    .into_iter()
    .flatten()
    .filter_map(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
    .collect();
    let searched = key::Searched {
        dirs: &dirs,
        source_dir: &source_dir,
        cwd: &cwd.to_string_lossy(),
        forced: &plan.compile.includes,
        written: &written,
        floor,
    };
    let Ok((found_places, probes)) = key::places_and_probes(&key_reads, &searched, &plan.places)
    else {
        return not_stored(NotStored::Changed, "changed");
    };
    let hint = key::Hint::new(reads, found_places, probes);
    let block_key = key::block(&plan.step, &hint, &digests);
    // B-56: where the block may go, from what the object holds.
    let Ok(object) = std::fs::read(&output) else {
        return not_stored(NotStored::Outputs, "no-object");
    };
    let holds = |needle: &str| holds_text(&object, needle);
    // B-56: a root in compressed debug sections isn't in the bytes, nor one a macro's value
    // holds once the code has turned it into anything else (a wide string, a hash): such a
    // block is tied.
    let tied = plan.compile.compressed_debug
        || defines_a_root(&plan.compile.keyed, plan.places.roots())
        || plan.places.roots().iter().any(|root| holds(root));
    let mut local = Vec::new();
    if tied {
        local.push("tied".to_string());
    }
    if current.home.as_deref().is_some_and(holds) {
        local.push("home-in-output".to_string());
    }
    if plan.places.bases().any(holds) {
        local.push("base-in-output".to_string());
    }
    if plan.native {
        local.push("native-cpu".to_string());
    }
    if !plan.places.under_root(&cwd.to_string_lossy()) {
        local.push("cwd-outside-roots".to_string());
    }
    // B-59: what leaves the machine is an object of the compile's kind, as another machine
    // checks it; anything else (bitcode without `-flto`, a target this version can't check)
    // is reused here only.
    let target = &plan.identity.target;
    if checks::object(&object, target, plan.compile.lto && plan.identity.clang).is_err() {
        local.push("not-an-object".to_string());
    }
    let key = if tied {
        key::tied(&block_key, &plan.places)
    } else {
        block_key
    };
    let now = ports.now();
    let waits = Waits::of(&|name| ports.var(name));
    // An audit or observe: the fresh object against the block found (B-59).
    let mut replace_downloaded = false;
    let mut differed = false;
    if let Some(found) = &plan.found {
        // The bytes alone: a compile here may be tied by this checkout's root where the block
        // found wasn't (B-56), and its key then differs from an honest block's.
        let same = std::fs::read(&found.object).is_ok_and(|b| b == object);
        if !same && !found.downloaded {
            let _ = store.suspend(&plan.step, "a fresh compile differed from the block", now);
            let mut result = not_stored(NotStored::Differed, "differed");
            result.4 = true;
            return result;
        }
        if !same {
            // A downloaded block: its key is suspended for the kept days and the scope it came
            // from told; this compile takes its place (B-32).
            differs(ports, current, &store, &plan.step, &found.key, now, &waits);
            replace_downloaded = found.key == key;
            differed = true;
        }
    }
    if let Some(min) = u64::from(current.min_free_gb).checked_mul(1 << 30)
        && min > 0
        && store.free_bytes().is_ok_and(|free| free < min)
    {
        return not_stored(NotStored::LowSpace, "low-space");
    }
    // The messages, with roots as placeholders, as files of the block.
    let work = current.folder.join(WORK);
    let pid = ports.pid();
    let mut files = vec![NewFile {
        name: "object".into(),
        source: output.clone(),
    }];
    let mut temps = Vec::new();
    for (name, bytes) in [("stdout", &ran.stdout), ("stderr", &ran.stderr)] {
        if bytes.is_empty() {
            continue;
        }
        let temp = work.join(format!("{pid}-{name}"));
        // The assembler's temporary input, named in its warnings, is another name in every
        // compile: as `{temp}`, two machines' blocks of one compile are the same outputs.
        let kept = super::compiler::without_temp_names_in(&plan.places.hide_bytes(bytes));
        if std::fs::write(&temp, kept).is_err() {
            return not_stored(NotStored::Messages, "messages");
        }
        files.push(NewFile {
            name: name.into(),
            source: temp.clone(),
        });
        temps.push(temp);
    }
    let description = Description {
        schema: DESCRIPTION_SCHEMA.into(),
        kind: KIND.into(),
        step: plan.step,
        source: plan.places.hide(&plan.compile.source),
        compile_ms,
        place: if tied { "tied" } else { "anywhere" }.into(),
        local: local.clone(),
        platform: plan.identity.target.clone(),
        repository: current.repository.clone(),
    };
    let value = serde_json::to_value(&description).unwrap_or_default();
    // B-32: built here, by this run, and by whom when the run shares.
    let relay = current.share.relay();
    let run = relay
        .map(|share| share.run.clone())
        .or_else(|| {
            current
                .folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let origin = Origin::built_here(&run, relay.map(|share| share.by.clone()));
    let only_downloaded = |there: Option<&Origin>, _: Option<&crate::store::Block>| {
        there.is_some_and(Origin::downloaded_only)
    };
    let published = if replace_downloaded {
        store
            .replace(&key, &value, &files, &origin, now, only_downloaded)
            .map(|replaced| match replaced {
                crate::store::Replaced::Stored => Published::Stored,
                crate::store::Replaced::Kept => Published::AlreadyThere,
            })
    } else {
        store.publish_with_origin(&key, &value, &files, &origin, now)
    };
    let size = object.len() as u64;
    let stored = match published {
        Ok(Published::Stored) => size,
        Ok(Published::AlreadyThere) => {
            if already_there(
                ports, current, &store, &plan.step, &key, &object, &value, &files, &origin, now,
                &waits,
            ) {
                differed = true;
            }
            0
        }
        Err(_) => {
            for temp in &temps {
                let _ = std::fs::remove_file(temp);
            }
            return not_stored(NotStored::StoreRefused, "store");
        }
    };
    for temp in &temps {
        let _ = std::fs::remove_file(temp);
    }
    let _ = store.add_hint(&plan.step, &hint.bytes(), now);
    let _ = store.set_step_cost(&plan.step, compile_ms, now);
    plan.digests.flush();
    key_log(current, &plan.step, None, Some(&key), &plan.compile.source);
    // B-29, B-60: the relay decides whether it goes up, from its record and the policy; a block
    // tied to the checkout's folder alone goes up only from a CI job (B-24).
    let portable = local.is_empty();
    let bound = local == ["tied"];
    if !differed && (portable || bound) {
        relay_side::tell(
            current,
            &Request::CcBuilt {
                key,
                step: plan.step,
                portable,
                bound,
                cost_ms: compile_ms,
            },
            &waits,
            &mut |socket, request, wait| ports.tell(socket, request, wait),
        );
    }
    (
        Outcome::BuiltAndStored,
        CcEntry {
            local,
            ..CcEntry::default()
        },
        Some(key),
        stored,
        differed,
    )
}

/// A block of first key `step` disagrees with a compile here (B-59): the key is suspended for
/// the store's kept days, and a block only downloaded (`key`) is reported to the scope it came
/// from.
#[allow(clippy::too_many_arguments)]
fn differs(
    ports: &mut dyn Ports,
    current: &Current,
    store: &Store,
    step: &Digest,
    key: &Digest,
    now: SystemTime,
    waits: &Waits,
) {
    let until = now + Duration::from_secs(u64::from(current.keep_days) * 86_400);
    let _ = store.suspend_until(step, "a C or C++ compile here made other bytes", until, now);
    if store.origin(key).is_some_and(|o| o.downloaded_only()) {
        let _ = relay_side::ask(
            current,
            &Request::CcReport {
                key: *key,
                problem: Problem::Differs,
            },
            waits.quick,
            waits,
            &mut |socket, request, wait| ports.relay(socket, request, wait),
        );
    }
}

/// B-32: a compile whose own publish found a block already there. Built here, nothing changes
/// (a reuse may be copying from it). Only downloaded, the compile's object takes its place where
/// the two differ, which suspends the key and is reported; where they are the same, the block is
/// from then on also built here. Whether they differed.
#[allow(clippy::too_many_arguments)]
fn already_there(
    ports: &mut dyn Ports,
    current: &Current,
    store: &Store,
    step: &Digest,
    key: &Digest,
    object: &[u8],
    description: &serde_json::Value,
    files: &[NewFile],
    origin: &Origin,
    now: SystemTime,
    waits: &Waits,
) -> bool {
    if !store.origin(key).is_some_and(|o| o.downloaded_only()) {
        return false;
    }
    let Ok(Some(block)) = store.block(key) else {
        return false;
    };
    let same = block
        .files
        .iter()
        .find(|f| f.name == "object")
        .is_some_and(|f| f.size == object.len() as u64 && f.sha256 == Digest::of(object));
    let only_downloaded = |there: Option<&Origin>, _: Option<&crate::store::Block>| {
        there.is_some_and(Origin::downloaded_only)
    };
    if !same {
        differs(ports, current, store, step, key, now, waits);
        let _ = store.replace(key, description, files, origin, now, only_downloaded);
        return true;
    }
    // The same object: the block becomes built here, which serves its messages as they are
    // (B-59). Only when they are this compile's too; else this compile's take their place, so
    // that no downloaded message is ever shown unfiltered.
    let ours: Vec<(String, Option<Digest>)> = files
        .iter()
        .map(|f| {
            let digest = std::fs::read(&f.source).ok().map(|b| Digest::of(&b));
            (f.name.clone(), digest)
        })
        .collect();
    let theirs: Vec<(String, Option<Digest>)> = block
        .files
        .iter()
        .map(|f| (f.name.clone(), Some(f.sha256)))
        .collect();
    let mut ours_sorted = ours.clone();
    ours_sorted.sort();
    let mut theirs_sorted = theirs;
    theirs_sorted.sort();
    if ours_sorted != theirs_sorted {
        let _ = store.replace(key, description, files, origin, now, only_downloaded);
        return false;
    }
    let built = origin.built.clone();
    let _ = store.update_origin(key, |o| {
        let mut o = o?;
        o.built.is_none().then(|| {
            o.built = built;
            o
        })
    });
    false
}

/// Appends the compile's keys to the run's key log, when one was asked for.
fn key_log(
    current: &Current,
    step: &Digest,
    first: Option<&serde_json::Value>,
    block: Option<&Digest>,
    source: &str,
) {
    let Some(path) = &current.key_log else {
        return;
    };
    let line = serde_json::json!({
        "kind": KIND,
        "source": source,
        "step": step,
        "first": first,
        "block": block,
    });
    let Ok(mut bytes) = serde_json::to_vec(&line) else {
        return;
    };
    bytes.push(b'\n');
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(&bytes);
    }
}

/// The real world.
struct Real;

impl Ports for Real {
    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn var_names(&self) -> Vec<OsString> {
        std::env::vars_os().map(|(name, _)| name).collect()
    }

    fn cwd(&self) -> io::Result<PathBuf> {
        std::env::current_dir()
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn file_clock(&self, dir: &Path) -> Option<SystemTime> {
        let probe = dir.join(format!(".hushrun-cc-clock-{}", std::process::id()));
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .and_then(|file| file.metadata())
            .and_then(|meta| meta.modified());
        if !matches!(&made, Err(err) if err.kind() == io::ErrorKind::AlreadyExists) {
            let _ = std::fs::remove_file(&probe);
        }
        made.ok()
    }

    fn pid(&self) -> u32 {
        std::process::id()
    }

    fn exec(&mut self, compiler: &OsStr, args: &[OsString]) -> i32 {
        let mut command = std::process::Command::new(compiler);
        command.args(args);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = command.exec();
            eprintln!(
                "error: hushrun could not run {}: {err}",
                Path::new(compiler).display()
            );
            127
        }
        #[cfg(not(unix))]
        match command.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(err) => {
                eprintln!(
                    "error: hushrun could not run {}: {err}",
                    Path::new(compiler).display()
                );
                127
            }
        }
    }

    fn compile(&mut self, run: &Run<'_>) -> io::Result<Ran> {
        use std::io::{BufRead, Read};
        use std::process::{Command, Stdio};
        let mut command = Command::new(run.compiler);
        command
            .args(run.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in run.env {
            command.env(name, value);
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().map(|mut out| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = out.read_to_end(&mut bytes);
                bytes
            })
        });
        let mut kept = Vec::new();
        let quiet = run.quiet;
        if let Some(stderr) = child.stderr.take() {
            let mut reader = io::BufReader::new(stderr);
            let mut buf = Vec::new();
            let mut err = io::stderr();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if !quiet {
                            let _ = err.write_all(&buf);
                            let _ = err.flush();
                        }
                        kept.extend_from_slice(&buf);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        let status = child.wait()?;
        let stdout = stdout.and_then(|t| t.join().ok()).unwrap_or_default();
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let signal = None;
        Ok(Ran {
            code: status.code(),
            signal,
            stdout,
            stderr: kept,
        })
    }

    fn ask(&self, program: &Path, args: &[String]) -> Option<Said> {
        let out = std::process::Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .env_remove("CPATH")
            .env_remove("C_INCLUDE_PATH")
            .env_remove("CPLUS_INCLUDE_PATH")
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        Some(Said {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn stdout(&mut self, bytes: &[u8]) {
        let mut out = io::stdout();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }

    fn stderr(&mut self, bytes: &[u8]) {
        let mut err = io::stderr();
        let _ = err.write_all(bytes);
        let _ = err.flush();
    }

    fn end_as(&mut self, ran: &Ran) -> i32 {
        #[cfg(unix)]
        if let Some(signal) = ran.signal.and_then(rustix::process::Signal::from_named_raw) {
            let _ = rustix::process::kill_process(rustix::process::getpid(), signal);
            // Still here: the signal is ignored or caught; the shell's convention.
            std::thread::sleep(Duration::from_millis(10));
            return 128 + ran.signal.unwrap_or(0);
        }
        ran.code.unwrap_or(1)
    }
}

#[cfg(test)]
mod tests;
