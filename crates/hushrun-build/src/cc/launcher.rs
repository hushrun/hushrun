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
use super::{CcRun, KIND, depfile, graph};
use crate::model::{Class, Mode, NotStored, Outcome, PassedThrough};
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
    /// (B-59). Tests choose.
    fn audited(&self, key: &Digest, started_ms: u64) -> bool {
        crate::audit::sample(key, started_ms, false)
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
        go: None,
        cc: None,
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
}

struct Found {
    key: Digest,
    object: PathBuf,
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
                &store,
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
    if !suspended {
        let looked = lookup(&store, &step, &places, &identity, &cwd, &mut digests);
        digests.flush();
        match looked {
            Ok(Some(hit)) => {
                let audited = current
                    .audit
                    .picks(|| ports.audited(&hit.key, current.started_ms));
                if current.mode == Mode::On && !audited {
                    // A restore that fails before any message compiles as asked (B-57).
                    return match restore(ports, current, &compile, &places, &cwd, &hit, start) {
                        Ok(reused) => Decided::Reused(Box::new(reused)),
                        Err(_) => Decided::Pass {
                            why: "restore",
                            probe: false,
                        },
                    };
                }
                found = Some(Found {
                    key: hit.key,
                    object: hit.object,
                });
            }
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

/// A block a hint led to.
struct Hit {
    key: Digest,
    object: PathBuf,
    stdout: Option<PathBuf>,
    stderr: Option<PathBuf>,
    hint: key::Hint,
    compile_ms: u64,
}

/// Follows the first key's hints, the newest first (B-54): each read hashed as it is now, each
/// place and probe checked. The first that names a stored block of this kind wins.
fn lookup(
    store: &Store,
    step: &Digest,
    places: &Places,
    identity: &Identity,
    cwd: &Path,
    digests: &mut Digests,
) -> Result<Option<Hit>, crate::store::StoreError> {
    'hints: for bytes in store.hints(step) {
        let Some(hint) = key::Hint::read(&bytes) else {
            continue;
        };
        let reads = hint.reads();
        let mut found = Vec::with_capacity(reads.len());
        for read in &reads {
            let path = key::read_path(read, places, cwd);
            let base = is_base(&path, places, identity);
            match digests.digest(&path, base) {
                Ok(digest) => found.push(digest),
                Err(_) => continue 'hints,
            }
        }
        if !key::places_hold(&hint, places) {
            continue;
        }
        let block_key = key::block(step, &hint, &found);
        for key in [block_key, key::tied(&block_key, places)] {
            let Some(block) = store.block(&key)? else {
                continue;
            };
            if block.description.get("kind").and_then(|k| k.as_str()) != Some(KIND) {
                continue;
            }
            // B-34's seam: a block with a record of where it came from is a download, which this
            // phase never reuses (sharing is a later phase).
            if store.origin(&key).is_some() {
                continue;
            }
            let file = |name: &str| {
                block
                    .files
                    .iter()
                    .find(|f| f.name == name)
                    .map(|f| f.path.clone())
            };
            let Some(object) = file("object") else {
                continue;
            };
            let compile_ms = block
                .description
                .get("compile_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            return Ok(Some(Hit {
                key,
                object,
                stdout: file("stdout"),
                stderr: file("stderr"),
                hint,
                compile_ms,
            }));
        }
    }
    Ok(None)
}

/// Whether a read lies under the SDK, the toolchain or a system folder: a file the run memo may
/// answer for by its stat (B-52).
fn is_base(path: &Path, places: &Places, identity: &Identity) -> bool {
    let text = path.to_string_lossy();
    (places.under_any(&text) && !places.under_root(&text)) || identity.in_system(&text)
}

/// Puts a block in place (B-57): the object under a temporary name in its folder, checked
/// against its digest, made writable, dated now and renamed into place; then the depfile from
/// the hint with this checkout's paths, written whole. The messages are read now and written by
/// the caller.
fn restore(
    ports: &mut dyn Ports,
    current: &Current,
    compile: &Compile,
    places: &Places,
    cwd: &Path,
    hit: &Hit,
    start: SystemTime,
) -> io::Result<Reused> {
    let output = cwd.join(&compile.output);
    let expected = crate::regular::hash_file(&hit.object)?.0;
    let pid = ports.pid();
    put_whole(&output, pid, start, |temp| {
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
        put_whole(&path, pid, start, |temp| {
            std::fs::write(temp, text.as_bytes())
        })?;
    }
    let read = |p: &Option<PathBuf>| -> io::Result<Vec<u8>> {
        match p {
            Some(p) => Ok(places.show_bytes(&std::fs::read(p)?)),
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
            saved_ms: hit.compile_ms.saturating_sub(took),
            found: true,
            step: None,
            key: Some(hit.key),
            cc: Some(CcEntry {
                source: places.hide(&compile.source),
                took_ms: took,
                ..CcEntry::default()
            }),
            ..entry(Outcome::Reused)
        },
    })
}

/// Writes `dest` whole: `fill` writes a temporary file beside it, which is made writable, dated
/// `when` and renamed into place. A failure removes the temporary file and leaves `dest` as it
/// was.
fn put_whole(
    dest: &Path,
    pid: u32,
    when: SystemTime,
    fill: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dest.with_file_name(format!(".{name}.hushrun-{pid}.tmp"));
    let done = (|| {
        fill(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644))?;
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

/// The second half: the launcher's own compile, the person's depfile, and the store.
fn compile(ports: &mut dyn Ports, current: &Current, mut plan: Plan, start: SystemTime) -> i32 {
    let pid = ports.pid();
    let deps = work_file(current, pid, start, "d");
    let graph_file = work_file(current, pid, start, "json");
    if std::fs::create_dir_all(current.folder.join(WORK)).is_err() {
        log_pass(current, "session", false, &[]);
        return ports.exec(&plan.compile.compiler, &os(&plan.compile.args));
    }
    let args = compile_args(&plan.compile, &deps, &plan.added);
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    if plan.identity.clang {
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
                    let system = |p: &str| {
                        let abs = places::absolute(&plan.cwd, p)
                            .to_string_lossy()
                            .into_owned();
                        plan.identity.in_system(&abs)
                            || dirs
                                .iter()
                                .any(|d| d.system && places::under(&abs, &d.path))
                    };
                    depfile::without_system(text, &plan.compile.dep, &plan.compile.output, &system)?
                }
                _ => text.to_string(),
            };
            put_whole(&plan.cwd.join(&path), pid, ports.now(), |temp| {
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
            });
            current.log(&Entry {
                rustc_ms: compile_ms,
                cc: Some(CcEntry {
                    source,
                    why: Some("depfile".into()),
                    ..CcEntry::default()
                }),
                ..entry(Outcome::BuiltNotStored(NotStored::DepInfo))
            });
            return match again {
                Ok(again) => {
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
        cc: Some(cc_entry),
        ..entry(outcome)
    });
    0
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
    if plan.suspended {
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
        let system = plan.identity.in_system(&abs_text)
            || dirs
                .iter()
                .any(|d| d.system && places::under(&abs_text, &d.path));
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
    let source_dir = places::absolute(&cwd, &plan.compile.source)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (found_places, probes) =
        key::places_and_probes(&key_reads, &dirs, &source_dir, &plan.places);
    let hint = key::Hint::new(reads, found_places, probes);
    let block_key = key::block(&plan.step, &hint, &digests);
    // B-56: where the block may go, from what the object holds.
    let output = cwd.join(&plan.compile.output);
    let Ok(object) = std::fs::read(&output) else {
        return not_stored(NotStored::Outputs, "no-object");
    };
    let holds = |needle: &str| {
        !needle.is_empty() && memchr::memmem::find(&object, needle.as_bytes()).is_some()
    };
    // B-56: a root in compressed debug sections isn't in the bytes; such a block is tied.
    let tied = plan.compile.compressed_debug || plan.places.roots().iter().any(|root| holds(root));
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
    let key = if tied {
        key::tied(&block_key, &plan.places)
    } else {
        block_key
    };
    let now = ports.now();
    // An audit or observe: the fresh object against the block found (B-59).
    if let Some(found) = &plan.found {
        let same = found.key == key && std::fs::read(&found.object).is_ok_and(|b| b == object);
        if !same {
            let _ = store.suspend(&plan.step, "a fresh compile differed from the block", now);
            let mut result = not_stored(NotStored::Differed, "differed");
            result.4 = true;
            return result;
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
        if std::fs::write(&temp, plan.places.hide_bytes(bytes)).is_err() {
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
    let published = store.publish(&key, &value, &files, now);
    for temp in &temps {
        let _ = std::fs::remove_file(temp);
    }
    let size = object.len() as u64;
    let outcome = match published {
        Ok(Published::Stored) => (Outcome::BuiltAndStored, size),
        Ok(Published::AlreadyThere) => (Outcome::BuiltAndStored, 0),
        Err(_) => return not_stored(NotStored::StoreRefused, "store"),
    };
    let _ = store.add_hint(&plan.step, &hint.bytes(), now);
    plan.digests.flush();
    key_log(current, &plan.step, None, Some(&key), &plan.compile.source);
    (
        outcome.0,
        CcEntry {
            local,
            ..CcEntry::default()
        },
        Some(key),
        outcome.1,
        false,
    )
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
        if let Some(stderr) = child.stderr.take() {
            let mut reader = io::BufReader::new(stderr);
            let mut buf = Vec::new();
            let mut err = io::stderr();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        let _ = err.write_all(&buf);
                        let _ = err.flush();
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
