//! The wrapper itself (B-2, B-19): one compiler call from start to exit code.
//!
//! [`call`] is the whole flow over [`Ports`]: the process's variables, the clock, running rustc.
//! [`main`] wires the real ones; tests drive [`call`] with a fake compiler. Every error of this
//! machinery is a value that ends in rustc running as asked (rule 1).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;

use crate::decide::{self, Before, Compared, Facts, Keep, Lookup, Plan, Rustc, Stored};
use crate::depinfo;
use crate::description::{Description, Output, OutputKind, SCHEMA, StoredDepInfo};
use crate::inputs::{Common, Context};
use crate::invocation::{self, Emit, Env, Invocation, Parsed};
use crate::key::{Asked, Link, LinkLibrary, Observed, Place, PlaceKind, ReadList};
use crate::messages::{Line, Recorded, Recorder};
use crate::model::{Class, Folder, Located, NotStored, Outcome, PassedThrough, Programs};
use crate::portable::{self, Portability, Reason};
use crate::restore::Restore;
use crate::roots::{Machine, RootPaths, Roots, resolve_on_disk};
use crate::scan::Needles;
use crate::script::{self, Script};
use crate::session::{self, Current, Entry};
use crate::store::{Block, NewFile, Published, Store};
use crate::unit::{self, Seen};
use crate::{audit, compiler, inputs, link, metadata, sidecar};

/// Whether the binary was called by the wrapper's name (B-2). `main` asks this first, before it
/// reads any setting.
pub fn called_as_wrapper(argv0: &OsStr) -> bool {
    Path::new(argv0)
        .file_stem()
        .is_some_and(|stem| stem == crate::WRAPPER_NAME)
}

/// One run of rustc the wrapper starts and watches.
pub struct Run<'a> {
    /// The compiler.
    pub rustc: &'a OsStr,
    /// Its arguments.
    pub args: &'a [OsString],
    /// Variables set on top of the wrapper's own.
    pub env: &'a [(OsString, OsString)],
    /// Keep rustc's stdout instead of passing it on (`--print=link-args`).
    pub capture_stdout: bool,
}

/// How a run of rustc ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// The exit code; 1 when rustc was killed by a signal.
    pub code: i32,
    /// Whether it succeeded.
    pub success: bool,
    /// Its stdout, when captured.
    pub stdout: Vec<u8>,
}

/// What the wrapper needs from the world, so tests can stand in for it.
pub trait Ports {
    /// One variable of the process.
    fn var(&self, name: &str) -> Option<OsString>;
    /// Every variable of the process.
    fn vars(&self) -> Vec<(OsString, OsString)>;
    /// The working directory: rustc's.
    fn cwd(&self) -> io::Result<PathBuf>;
    /// The clock.
    fn now(&self) -> SystemTime;
    /// The date the file system gives a file created in `dir` now, where file dates come from
    /// another clock than [`Ports::now`] (Linux's moves once a tick and lags it), so that B-11
    /// compares dates with dates. `None` where dates are exact, or the file can't be made.
    fn file_clock(&self, dir: &Path) -> Option<SystemTime> {
        let _ = dir;
        None
    }
    /// This process's id, for temporary names.
    fn pid(&self) -> u32;
    /// What `<rustc> -vV` prints (B-17).
    fn rustc_version(&self, rustc: &Path) -> io::Result<String>;
    /// What `<rustc> --print target-list` prints: the compiler's built-in targets (B-4).
    fn rustc_targets(&self, rustc: &Path) -> io::Result<String>;
    /// Writes to the wrapper's stdout what rustc printed there (B-18).
    fn stdout(&mut self, bytes: &[u8]);
    /// Runs rustc exactly as asked and returns its exit code. The real one replaces the process
    /// on unix, so a pass-through costs one process.
    fn exec(&mut self, rustc: &OsStr, args: &[OsString]) -> i32;
    /// Runs rustc with stderr piped: each line goes to `line` as it comes.
    fn compile(&mut self, run: &Run<'_>, line: &mut dyn FnMut(&str)) -> io::Result<Ran>;
    /// Whether the audit picks the compile of block `key` in the run started at `started_ms`
    /// (B-16). Tests choose.
    fn audited(&self, key: &Digest, started_ms: u64) -> bool {
        audit::picked(key, started_ms)
    }
}

/// One compiler call. The process's own arguments are `[<this name>, <rustc>, <args>…]`, as
/// cargo passes them to a `RUSTC_WRAPPER`. Returns rustc's exit code, or 0 for a reuse.
pub fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let code = call(&mut Real, &mut io::stderr(), &argv);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// The whole flow of one call (B-3): `argv` is the compiler's path and its arguments, `err` the
/// wrapper's stderr. Returns the exit code.
pub fn call(ports: &mut dyn Ports, err: &mut dyn Write, argv: &[OsString]) -> i32 {
    let start = ports.now();
    let Some((rustc, args)) = argv.split_first() else {
        let _ = writeln!(
            err,
            "error: {} stands in for rustc and is run by cargo: it needs the compiler's path as its first argument",
            crate::WRAPPER_NAME
        );
        return 2;
    };
    // No session: the cheapest pass-through, before anything is read (B-2). A record of the
    // unit it names would no longer describe what rustc writes now (B-23).
    if ports.var(session::SESSION).is_none_or(|v| v.is_empty()) {
        unit::forget_for_args(args, None);
        return ports.exec(rustc, args);
    }
    let Ok(cwd) = ports.cwd() else {
        unit::forget_for_args(args, None);
        return ports.exec(rustc, args);
    };
    let env = Env::from_vars(ports.vars());
    let invocation = match invocation::parse(argv, &env, &cwd) {
        Parsed::Handled(invocation) => invocation,
        // Cargo's probes run for every build: nothing else happens for them.
        Parsed::PassedThrough(PassedThrough::Probe | PassedThrough::NoCrateName) => {
            return ports.exec(rustc, args);
        }
        Parsed::PassedThrough(why) => {
            unit::forget_for_args(args, Some(&cwd));
            if let Some(current) = session::current(&|name| ports.var(name)) {
                let var = |name: &str| env.get(name).and_then(OsStr::to_str).map(str::to_string);
                current.log(&Entry {
                    crate_name: crate_name_of(args).unwrap_or_default(),
                    package: package_id(var("CARGO_PKG_NAME"), var("CARGO_PKG_VERSION")),
                    ..entry(decide::passed_through(why), None)
                });
            }
            return ports.exec(rustc, args);
        }
    };
    let Some(current) = session::current(&|name| ports.var(name)) else {
        unit::forget_for_args(args, Some(&cwd));
        return ports.exec(rustc, args);
    };
    let floor = inputs::floor(
        ports.file_clock(&Path::new(invocation.cwd()).join(invocation.out_dir())),
        start,
    );
    Call {
        ports,
        err,
        current: &current,
        invocation: &invocation,
        env: &env,
        start,
        floor,
        link_extra: Vec::new(),
        sysroot: None,
        hide_sources: None,
        view: None,
        bundle: false,
        clear_bundle: false,
        keys: std::cell::RefCell::new(KeyLine::default()),
        asked: None,
    }
    .run()
}

/// An entry with nothing measured yet.
fn entry(outcome: Outcome, class: Option<Class>) -> Entry {
    Entry {
        outcome,
        class,
        crate_name: String::new(),
        package: String::new(),
        rustc_ms: 0,
        saved_ms: 0,
        stored_bytes: 0,
        found: false,
        differed: false,
        note: None,
        portable: None,
    }
}

/// The package as the log names it: `name-version`, the name alone without a version, empty
/// when unknown. It tells apart two build scripts, whose crates are all `build_script_build`.
fn package_id(name: Option<String>, version: Option<String>) -> String {
    match (name, version) {
        (Some(name), Some(version)) => format!("{name}-{version}"),
        (Some(name), None) => name,
        (None, _) => String::new(),
    }
}

/// The `--crate-name` of arguments the parser refused, for the log.
fn crate_name_of(args: &[OsString]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let arg = arg.to_str()?;
        if arg == "--crate-name" {
            return iter.next()?.to_str().map(str::to_string);
        }
        if let Some(name) = arg.strip_prefix("--crate-name=") {
            return Some(name.to_string());
        }
    }
    None
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, millis)
}

/// One call the wrapper handles, after the probes and the session.
struct Call<'a> {
    ports: &'a mut dyn Ports,
    err: &'a mut dyn Write,
    current: &'a Current,
    invocation: &'a Invocation,
    env: &'a Env,
    start: SystemTime,
    /// The call's start as file dates count it (B-11), for the reuse's unit record.
    floor: SystemTime,
    /// What a linked output's compile gets besides the remaps (B-9, B-18).
    link_extra: Vec<OsString>,
    /// The compiler's sysroot (B-17), where a link's `-B` folders must lie (B-18).
    sysroot: Option<PathBuf>,
    /// `--sysroot=<the toolchain without its sources>`, when it has sources (B-9, B-17).
    hide_sources: Option<OsString>,
    /// That view and the toolchain it leads into: a path under the view is judged by what it
    /// points at, the toolchain's own file (B-11).
    view: Option<(PathBuf, PathBuf)>,
    /// A program that gets a debug bundle and may move (B-9): its compile is changed so, and
    /// its block carries the bundle.
    bundle: bool,
    /// A program whose flags allow a bundle (B-9): a bundle an earlier compile left beside it
    /// is removed first, whether this compile makes one (`movable`) or not (`tied`, B-21).
    clear_bundle: bool,
    /// What the key log says of this call, filled in as the call goes ([`Current::key_log`]).
    keys: std::cell::RefCell<KeyLine>,
    /// The first key's parts, once computed: what decides whether the block may leave the
    /// machine reads them.
    asked: Option<Asked>,
}

/// One line of the key log ([`Current::key_log`]): the parts of a compile's keys, so that two
/// machines' lines for one crate tell which part differs. Written only when the log is asked
/// for.
#[derive(Debug, Default, serde::Serialize)]
struct KeyLine {
    /// The first key.
    step: Option<Digest>,
    /// The first key's body as it is hashed: the compiler's digest, the arguments and the
    /// variables with roots and bases as placeholders.
    first: Option<serde_json::Value>,
    /// The first key's parts, each by digest.
    first_parts: Option<serde_json::Value>,
    /// The compiler identity's parts, each by digest (B-17).
    compiler: Option<compiler::Parts>,
    /// The second key: of the block stored, or the one a lookup found.
    block: Option<Digest>,
    /// The second key's parts, each by digest, list entries by what they name.
    second_parts: Option<serde_json::Value>,
    /// The outputs, by name and SHA-256.
    outputs: Vec<(String, Digest)>,
    /// The folders that name the machine found in an output.
    in_outputs: Vec<Folder>,
    /// Each `--extern` as the second key holds it: by the SHA-256 of its file, or a proc macro
    /// by what it was built from. With `outputs`, what joins a crate to its dependencies.
    externs: Vec<crate::key::ExternInput>,
}

/// What the lookup found, beyond the plan's facts.
struct Found {
    block: Block,
    description: Description,
    /// What the hint that found it hashed, as the key holds it: the unit record's digests.
    observed: Observed,
    /// The files the hint that found it hashed, at the stats they were hashed at: a compile
    /// that follows must find them unchanged (B-11).
    watched: Vec<inputs::Watched>,
}

/// What a linked output was linked from (B-18), with the facts B-8 and B-11 need.
struct Linked {
    link: Link,
    /// A file the link read lies outside every root: the block is tied (B-8).
    outside: bool,
    /// A file the link read changed while compiling (B-11).
    changed: bool,
}

/// What a compile produced, for the store.
struct Produced {
    observed: Observed,
    /// What dependents name a linked library by when the block may move (B-6).
    known_by: Digest,
    outputs: Vec<OutputFile>,
    dep_info: depinfo::DepInfo,
    /// Whether the block may leave the machine.
    portability: Portability,
}

/// An output searched (B-10): whether it holds a root, and which folders that name the machine.
struct Searched {
    file: OutputFile,
    tied: bool,
    machine: BTreeSet<Folder>,
}

/// What outputs are searched for: the roots (B-10) and the folders that name the machine, each
/// text once with every kind it stands for.
struct Search {
    needles: Needles,
    kinds: Vec<Vec<Option<Folder>>>,
}

impl Search {
    fn new(roots: &Roots) -> Search {
        let mut texts: Vec<(Vec<u8>, Vec<Option<Folder>>)> = Vec::new();
        let mut add = |bytes: Vec<u8>, kind: Option<Folder>| {
            if bytes.is_empty() {
                return;
            }
            match texts.iter_mut().find(|(text, _)| *text == bytes) {
                Some((_, kinds)) => kinds.push(kind),
                None => texts.push((bytes, vec![kind])),
            }
        };
        for needle in roots.needles() {
            add(needle, None);
        }
        for (folder, text) in roots.machine_needles() {
            add(text.as_bytes().to_vec(), Some(*folder));
        }
        let kinds = texts.iter().map(|(_, kinds)| kinds.clone()).collect();
        Search {
            needles: Needles::new(texts.into_iter().map(|(text, _)| text).collect()),
            kinds,
        }
    }

    /// One output, hashed and searched in one pass.
    fn file(&self, path: PathBuf, name: String, kind: OutputKind) -> Result<Searched, NotStored> {
        let (found, sha256, size) = self
            .needles
            .all_in_file_hashed(&path)
            .map_err(|_| NotStored::Scan)?;
        let mut tied = false;
        let mut machine = BTreeSet::new();
        for (hit, kinds) in found.iter().zip(&self.kinds) {
            if !hit {
                continue;
            }
            for kind in kinds {
                match kind {
                    None => tied = true,
                    Some(folder) => {
                        machine.insert(*folder);
                    }
                }
            }
        }
        Ok(Searched {
            file: OutputFile {
                path,
                name,
                kind,
                sha256,
                size,
            },
            tied,
            machine,
        })
    }
}

/// One output file of a compile.
struct OutputFile {
    path: PathBuf,
    name: String,
    kind: OutputKind,
    sha256: Digest,
    size: u64,
}

impl Call<'_> {
    fn log(&self, entry: Entry) {
        let package = self.invocation.package();
        let entry = Entry {
            crate_name: self.invocation.crate_name().to_string(),
            package: package_id(package.name.clone(), package.version.clone()),
            ..entry
        };
        self.current.log(&entry);
        self.log_keys(&entry);
    }

    /// Appends this call's line to the key log, when the run asked for one. A line that can't
    /// be written never fails a compile.
    fn log_keys(&self, entry: &Entry) {
        use std::io::Write as _;
        let Some(path) = &self.current.key_log else {
            return;
        };
        let keys = self.keys.borrow();
        let line = serde_json::json!({
            "crate_name": entry.crate_name,
            "package": entry.package,
            "class": entry.class,
            "outcome": entry.outcome,
            "found": entry.found,
            "differed": entry.differed,
            "audit": self.current.audit,
            "portable": entry.portable,
            "keys": &*keys,
        });
        let Ok(mut bytes) = serde_json::to_vec(&line) else {
            return;
        };
        bytes.push(b'\n');
        // One write of one whole line, to a file opened for appending.
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(&bytes);
        }
    }

    /// Runs rustc exactly as asked, logging `outcome`: the one place that decides how, for every
    /// call passed through or built as asked after its arguments were read (never-store, an
    /// unknown compiler identity, a target that is not the compiler's, unusable roots, a keyed
    /// variable that is not UTF-8, mode `off`, an incremental rebuild, a key that can't be
    /// computed (a not-a-file input among them), a store that can't be read, a block that
    /// can't be put in place). When a reuse put at least one of the call's externs in place,
    /// rustc runs as a child ([`Call::watched_as_asked`]) so that B-15 can heal a block it
    /// can't load; otherwise the process is replaced, as cheap as a pass-through gets.
    ///
    /// Calls whose arguments could not be read (the parser's own pass-through reasons) and
    /// calls with no session or working directory never get here: [`call`] replaces the
    /// process for them, since there are no externs to look at and no store to suspend in.
    fn as_asked(&mut self, outcome: Outcome) -> i32 {
        // A crate with something local to watch runs as a child too, so that its unit record
        // is written once rustc is done (B-23).
        let record = self.local();
        if record || self.any_extern_reused() {
            return self.watched_as_asked(outcome, record);
        }
        self.forget_unit();
        self.log(entry(outcome, Some(self.invocation.class())));
        let rustc = OsString::from(self.invocation.rustc());
        let args: Vec<OsString> = self.invocation.args().iter().map(OsString::from).collect();
        self.ports.exec(&rustc, &args)
    }

    /// Whether the crate may have something local to watch (B-23): its package folder (or, with
    /// none, its source) lies outside the shared folders. A registry or git crate has nothing.
    fn local(&self) -> bool {
        let inv = self.invocation;
        let place = inv
            .package()
            .manifest_dir
            .as_deref()
            .unwrap_or(inv.source());
        let path = Path::new(inv.cwd()).join(place);
        !unit::Shared::new(&self.current.shared).holds(&path)
    }

    /// Removes this unit's record, before rustc runs or anything is put in place (B-23).
    fn forget_unit(&self) {
        unit::settle(&self.out_dir(), &self.invocation.file_stem(), None);
    }

    /// The outputs rustc announced: what `link` and `metadata` produced.
    fn announced(&self, recorded: &Recorded) -> Vec<PathBuf> {
        recorded
            .lines()
            .iter()
            .filter_map(|line| match line {
                Line::Notice { artifact, emit } if emit == "link" || emit == "metadata" => {
                    Some(Path::new(self.invocation.cwd()).join(artifact))
                }
                _ => None,
            })
            .collect()
    }

    /// Writes this unit's record after a compile or a reuse ended well (B-23), from the
    /// dep-info now in the output folder, and remembers the output folder for the check. Any
    /// failure leaves no record.
    fn record_unit(
        &self,
        since: SystemTime,
        outputs: Vec<PathBuf>,
        digests: &BTreeMap<PathBuf, Digest>,
    ) {
        let inv = self.invocation;
        // A registry or git crate: its sources never change, and what its build script wrote is
        // cargo's to watch. No record, and its output folder isn't registered by it.
        let info = self
            .local()
            .then(|| std::fs::read_to_string(inv.dep_info_path()).ok())
            .flatten()
            .and_then(|text| depinfo::parse(&text).ok());
        let seen = info.map(|info| Seen {
            crate_name: inv.crate_name().to_string(),
            package: inv.package().name.clone(),
            cwd: PathBuf::from(inv.cwd()),
            outputs,
            items: unit::items(
                inv,
                &info,
                &unit::Shared::new(&self.current.shared),
                digests,
            ),
            since,
        });
        let out_dir = self.out_dir();
        if unit::settle(&out_dir, &inv.file_stem(), seen.as_ref()) {
            self.register_out_dir(&out_dir);
        }
    }

    /// Remembers the output folder in the store's registry, once per run (B-23): a marker in
    /// the session's folder says it was done. Cost: one failed `open` after the first time.
    fn register_out_dir(&self, out_dir: &Path) {
        let name = Digest::of(out_dir.as_os_str().as_encoded_bytes()).hex();
        let marks = self.current.folder.join("out-dirs");
        let _ = std::fs::create_dir_all(&marks);
        let first = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marks.join(&name[..32]))
            .is_ok();
        if first {
            let _ = Store::new(&self.current.store).register_out_dir(
                out_dir,
                Path::new(self.invocation.cwd()),
                self.ports.now(),
            );
        }
    }

    fn pass(&mut self, why: PassedThrough) -> i32 {
        self.as_asked(decide::passed_through(why))
    }

    /// Whether a reuse put any of this call's externs in place (B-15). Cost, for a call run as
    /// asked: one read of each extern's small record (a failed open where there is none), and a
    /// stat only for a record that says reused. No file is hashed.
    fn any_extern_reused(&self) -> bool {
        self.invocation.externs().iter().any(|ext| {
            ext.path
                .as_ref()
                .is_some_and(|path| sidecar::reused(&Path::new(self.invocation.cwd()).join(path)))
        })
    }

    /// Runs rustc exactly as asked, as a child whose stderr goes on line by line and whose
    /// stdout is left alone, so that a failure to load a reused extern suspends and removes it
    /// (B-15) as it does for a compile the wrapper handles. The outcome stays the one asked.
    fn watched_as_asked(&mut self, outcome: Outcome, record: bool) -> i32 {
        let store = Store::new(&self.current.store);
        self.forget_unit();
        let since = self.ports.now();
        let rustc = OsString::from(self.invocation.rustc());
        let args: Vec<OsString> = self.invocation.args().iter().map(OsString::from).collect();
        let mut recorder = Recorder::new(&mut *self.err, self.invocation.form().clone());
        let ran = self.ports.compile(
            &Run {
                rustc: &rustc,
                args: &args,
                env: &[],
                capture_stdout: false,
            },
            &mut |line| {
                let _ = recorder.line(line);
            },
        );
        let recorded = recorder.finish();
        let mut log = entry(outcome, Some(self.invocation.class()));
        let code = match ran {
            Ok(ran) => {
                if !ran.success {
                    log.note = self.reject_reused(&store, &recorded);
                } else if record {
                    let outputs = self.announced(&recorded);
                    self.record_unit(since, outputs, &BTreeMap::new());
                }
                ran.code
            }
            Err(err) => {
                let _ = writeln!(
                    self.err,
                    "error: hushrun could not run {}: {err}",
                    self.invocation.rustc()
                );
                1
            }
        };
        self.log(log);
        code
    }

    fn run(mut self) -> i32 {
        let inv = self.invocation;
        let class = inv.class();
        if inv
            .package()
            .name
            .as_ref()
            .is_some_and(|name| self.current.never_store.contains(name))
        {
            return self.pass(PassedThrough::NeverStore);
        }
        let store = Store::new(&self.current.store);
        let identity = {
            let ports = &*self.ports;
            compiler::identity(
                Path::new(inv.rustc()),
                inv.target(),
                &|name| ports.var(name),
                &store.root().join(compiler::MEMOS),
                &|path| ports.rustc_version(path),
                &|path| ports.rustc_targets(path),
            )
        };
        let Ok(identity) = identity else {
            return self.pass(PassedThrough::Compiler);
        };
        self.sysroot = identity.sysroot.clone();
        // Every compile sees the toolchain as one without sources does (B-9, B-17). `hushrun
        // run` made the view as it began; a toolchain it couldn't foresee gets one here.
        let used =
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(self.current.started_ms);
        let view = match &identity.sysroot {
            Some(sysroot) => match compiler::hidden_sources(
                sysroot,
                &store.root().join(compiler::MEMOS).join(compiler::SYSROOTS),
                Some(used),
            ) {
                Ok(view) => view,
                Err(_) => return self.pass(PassedThrough::Compiler),
            },
            None => None,
        };
        self.hide_sources = view.as_ref().map(|view| {
            let mut arg = OsString::from("--sysroot=");
            arg.push(view);
            arg
        });
        self.view = view.clone().zip(identity.sysroot.clone());
        // A target that is not one of the compiler's own is a file rustc loads (B-4).
        if inv
            .target()
            .is_some_and(|target| !identity.builds_for(target))
        {
            return self.pass(PassedThrough::TargetFile);
        }
        // The bases (B-8) and what outputs are searched for besides the roots (B-10).
        let machine = Machine {
            cargo_home: self.current.cargo_home.clone(),
            toolchain: identity
                .sysroot
                .as_ref()
                .and_then(|path| path.to_str())
                .map(str::to_string),
            toolchain_view: view
                .as_ref()
                .and_then(|path| path.to_str())
                .map(str::to_string),
            rustup_home: self.current.rustup_home.clone(),
            home: self.current.home.clone(),
        };
        let Ok(roots) = Roots::new(
            &RootPaths::of(inv),
            &self.current.shared,
            &machine,
            resolve_on_disk,
        ) else {
            return self.pass(PassedThrough::Roots);
        };

        // The build script's saved output: its `rustc-env` names belong in the first key.
        let script: Option<Script> = inv.package().out_dir.as_ref().and_then(|out| {
            let out = Path::new(inv.cwd()).join(out);
            let saved = script::saved_output(&out)?;
            Some(script::parse_bytes(&crate::regular::read(&saved).ok()?))
        });
        let mut unstorable = (inv.package().out_dir.is_some() && script.is_none())
            .then_some(NotStored::ScriptOutputMissing);
        // A `-l` whose file can't be found with certainty can't be keyed (B-6).
        if !inv.native_certain() {
            unstorable = unstorable.or(Some(NotStored::NativeOutside));
        }
        if class != Class::Library {
            let apple = inv.target().unwrap_or(&identity.host).contains("-apple-");
            self.clear_bundle = bundled(inv, apple);
            self.bundle = self.clear_bundle && self.current.programs == Programs::Movable;
            self.link_extra = linked_args(inv, apple, self.bundle);
            // Debug info packed beside a proc macro or a build script, or beside a program the
            // wrapper leaves as it is, is not something a block carries.
            if inv.debug().split.as_deref() == Some("packed") && !self.bundle {
                unstorable = unstorable.or(Some(NotStored::DebugFiles));
            }
        }

        let mut extra: Vec<String> = self.current.env.clone();
        if let Some(script) = &script {
            extra.extend(script.rustc_env.iter().map(|(name, _)| name.clone()));
        }
        extra.sort();
        extra.dedup();
        let Ok(key_env) = inv.key_env_with(self.env, &extra) else {
            return self.pass(PassedThrough::NotUtf8);
        };
        // What the wrapper adds besides the remaps changes the outputs: a program set to
        // `movable` and one set to `tied` are different compiles (B-5, B-9, B-21).
        let Some(added) = self
            .link_extra
            .iter()
            .map(|arg| arg.to_str().map(|arg| roots.to_placeholders(arg)))
            .collect::<Option<Vec<_>>>()
        else {
            return self.pass(PassedThrough::NotUtf8);
        };
        let asked = Asked {
            compiler: identity.digest,
            args: inv
                .key_args()
                .iter()
                .map(|arg| roots.to_placeholders(arg))
                .collect(),
            env: key_env
                .into_iter()
                .map(|(name, value)| (name, roots.to_placeholders(&value)))
                .collect(),
            added,
        };
        let step = asked.key().ok();
        if self.current.key_log.is_some() {
            let mut keys = self.keys.borrow_mut();
            keys.step = step;
            keys.first = serde_json::to_value(&asked).ok();
            keys.first_parts = asked.parts().ok();
            keys.compiler = Some(identity.parts.clone());
        }
        self.asked = Some(asked.clone());
        let cx = step.map(|step| Context {
            invocation: inv,
            roots: &roots,
            env: self.env,
            step,
            script: script.as_ref(),
            run_memo: Some(&self.current.folder),
        });

        let mut common: Option<Result<Common, NotStored>> = None;
        let mut found: Option<Found> = None;
        let lookup = match (&cx, unstorable) {
            (None, _) => Lookup::KeyFailed,
            (Some(_), Some(why)) => Lookup::Unstorable(why),
            (Some(cx), None) => {
                if store.suspended(&cx.step).is_some() {
                    Lookup::Suspended
                } else {
                    self.look_up(&store, cx, &mut common, &mut found)
                }
            }
        };
        let incremental_rebuild = inv.incremental().is_some() && inv.dep_info_path().exists();
        let plan = decide::before(&Before {
            mode: self.current.mode,
            class,
            lookup,
            incremental_rebuild,
        });

        match plan {
            Plan::PassThrough(why) => self.pass(why),
            Plan::AsAsked(why) => self.as_asked(Outcome::BuiltNotStored(why)),
            Plan::Reuse => {
                let (Some(found), Some(_)) = (&found, &cx) else {
                    return self.as_asked(
                        decide::after(decide::restore_failed(), &Facts::ran(Rustc::NotRun)).outcome,
                    );
                };
                self.reuse(&store, &roots, found, cx.as_ref())
            }
            Plan::Compile { keep, compare } => {
                let Some(cx) = &cx else {
                    return self.as_asked(Outcome::BuiltNotStored(NotStored::Key));
                };
                let common = match common {
                    Some(common) => common,
                    None => cx.common(),
                };
                self.compile(
                    &store,
                    &roots,
                    cx,
                    common,
                    plan,
                    keep,
                    compare,
                    found.as_ref(),
                )
            }
        }
    }

    /// B-7: each hint, followed with the files as they are now, until one finds a block.
    fn look_up(
        &self,
        store: &Store,
        cx: &Context<'_>,
        common: &mut Option<Result<Common, NotStored>>,
        found: &mut Option<Found>,
    ) -> Lookup {
        let hints = store.hints(&cx.step);
        if hints.is_empty() {
            return Lookup::Missing;
        }
        let shared = match common.get_or_insert_with(|| cx.common()) {
            Ok(shared) => shared.clone(),
            Err(_) => return Lookup::KeyFailed,
        };
        for bytes in hints {
            let Ok(hint) = serde_json::from_slice::<ReadList>(&bytes) else {
                continue;
            };
            let Some((observed, watched)) = cx.from_hint(&hint, &shared) else {
                continue;
            };
            let Ok(key) = observed.key() else {
                continue;
            };
            match store.block(&key) {
                Err(_) => return Lookup::StoreFailed,
                Ok(None) => continue,
                Ok(Some(block)) => {
                    let Some(description) = Description::from_value(&block.description)
                        .filter(|d| d.step == cx.step && d.class == self.invocation.class())
                    else {
                        continue;
                    };
                    let serves_form = description.messages.form() == self.invocation.form()
                        || description.messages.only_notices();
                    let audit = self
                        .current
                        .audit
                        .picks(|| self.ports.audited(&key, self.current.started_ms));
                    if self.current.key_log.is_some() {
                        let mut keys = self.keys.borrow_mut();
                        keys.block = Some(key);
                        keys.second_parts = observed.parts().ok();
                        keys.externs = observed.externs.clone();
                        keys.outputs = block
                            .files
                            .iter()
                            .map(|f| (f.name.clone(), f.sha256))
                            .collect();
                    }
                    *found = Some(Found {
                        block,
                        description,
                        observed,
                        watched,
                    });
                    return Lookup::Found { audit, serves_form };
                }
            }
        }
        Lookup::Missing
    }

    /// B-12: the block's files in place, then its messages; rustc doesn't start. Any failure
    /// before the first message runs rustc as asked.
    fn reuse(
        &mut self,
        store: &Store,
        roots: &Roots,
        found: &Found,
        cx: Option<&Context<'_>>,
    ) -> i32 {
        self.forget_unit();
        let restored = Restore {
            block: &found.block,
            description: &found.description,
            invocation: self.invocation,
            roots,
            start: self.start,
            pid: self.ports.pid(),
        }
        .run();
        let restored = match restored {
            Ok(restored) => restored,
            Err(_) => {
                let outcome =
                    decide::after(decide::restore_failed(), &Facts::ran(Rustc::NotRun)).outcome;
                return self.as_asked(outcome);
            }
        };
        if let Some(cx) = cx {
            let out_dir = self.out_dir();
            let outputs = found
                .description
                .outputs
                .iter()
                .filter(|o| matches!(o.kind, OutputKind::Link | OutputKind::Metadata))
                .map(|o| out_dir.join(&o.name))
                .collect();
            let digests = unit::digests(cx, &found.observed);
            self.record_unit(self.floor, outputs, &digests);
        }
        let _ = self.err.write_all(&restored.messages);
        let _ = self.err.flush();
        let now = self.ports.now();
        let _ = store.used(&found.block.key, now);
        let took = millis(now.duration_since(self.start).unwrap_or_default());
        let verdict = decide::after(Plan::Reuse, &Facts::ran(Rustc::NotRun));
        self.log(Entry {
            saved_ms: found.description.rustc_ms.saturating_sub(took),
            found: true,
            portable: Some(found.description.portability.clone()),
            ..entry(verdict.outcome, Some(self.invocation.class()))
        });
        0
    }

    /// Steps 8 to 16: compile with the wrapper's changes, gather what it read, store, compare.
    #[allow(clippy::too_many_arguments)]
    fn compile(
        &mut self,
        store: &Store,
        roots: &Roots,
        cx: &Context<'_>,
        common: Result<Common, NotStored>,
        plan: Plan,
        keep: Keep,
        compare: bool,
        found: Option<&Found>,
    ) -> i32 {
        let inv = self.invocation;
        let linked = inv.class() != Class::Library;
        let args = inv.command(&self.added(roots));
        let rustc = OsString::from(inv.rustc());
        let before = if inv.class() == Class::Program && !self.bundle {
            debug_objects(&self.out_dir(), &inv.file_stem())
        } else {
            BTreeMap::new()
        };
        // dsymutil adds to a bundle that is there and leaves what it doesn't write: the one an
        // earlier compile left goes first, so the bundle after this compile is all its own.
        let stale_bundle = self.clear_bundle
            && crate::restore::remove_aside(&self.bundle_dir(), &self.out_dir(), self.ports.pid())
                .is_err();
        self.forget_unit();
        // What the compile reads is dated against this, a date of the file system's (B-11).
        let floor = inputs::floor(self.ports.file_clock(&self.out_dir()), self.ports.now());
        let compile_start = self.ports.now();
        let mut recorder = Recorder::new(&mut *self.err, inv.form().clone());
        let ran = self.ports.compile(
            &Run {
                rustc: &rustc,
                args: &args,
                env: &[],
                capture_stdout: linked,
            },
            &mut |line| {
                let _ = recorder.line(line);
            },
        );
        let recorded = recorder.finish();
        let rustc_ms = millis(
            self.ports
                .now()
                .duration_since(compile_start)
                .unwrap_or_default(),
        );
        let mut log = Entry {
            rustc_ms,
            found: found.is_some(),
            ..entry(Outcome::BuiltAndStored, Some(inv.class()))
        };
        let ran = match ran {
            Ok(ran) => ran,
            Err(err) => {
                let _ = writeln!(
                    self.err,
                    "error: hushrun could not run {}: {err}",
                    inv.rustc()
                );
                Ran {
                    code: 1,
                    success: false,
                    stdout: Vec::new(),
                }
            }
        };
        // What rustc printed goes on, but the linker command the wrapper asked for (B-18).
        let printed = link::without_command(&ran.stdout);
        if !printed.is_empty() {
            self.ports.stdout(&printed);
        }
        if !ran.success {
            log.note = self.reject_reused(store, &recorded);
            log.outcome = decide::after(plan, &Facts::ran(Rustc::Failed)).outcome;
            self.log(log);
            return ran.code;
        }

        let mut facts = Facts::ran(Rustc::Succeeded);
        let common = match common {
            Ok(_) if stale_bundle => Err(NotStored::DebugFiles),
            other => other,
        };
        let produced = common.and_then(|common| {
            self.produced(
                cx,
                roots,
                &common,
                &recorded,
                floor,
                store,
                &mut facts,
                (&ran.stdout, &before),
                found,
            )
        });
        match produced {
            Err(why) => facts.error = Some(why),
            Ok(produced) => {
                if compare && let Some(found) = found {
                    facts.compared = compare_outputs(&produced.outputs, &found.block);
                }
                let trusted = !facts.changed && facts.compared != Compared::Differed;
                log.portable = Some(produced.portability.clone());
                if self.current.key_log.is_some() {
                    let mut keys = self.keys.borrow_mut();
                    keys.block = produced.observed.key().ok();
                    keys.second_parts = produced.observed.parts().ok();
                    keys.externs = produced.observed.externs.clone();
                    keys.outputs = produced
                        .outputs
                        .iter()
                        .map(|o| (o.name.clone(), o.sha256))
                        .collect();
                }
                if trusted {
                    // Dependents may know a block by what it was built from only if it may move:
                    // a tied one is another file in another checkout, under the same name (B-6).
                    let produced_by = matches!(produced.observed.place, Place::Anywhere)
                        .then_some(produced.known_by);
                    let portable = produced.portability.named_portably(inv.class());
                    // A bundle's files are a program's debug info: no dependent loads them, and
                    // no record is written inside the bundle.
                    for output in produced.outputs.iter().filter(|o| !o.kind.in_bundle()) {
                        let _ = sidecar::record_block(
                            &output.path,
                            output.sha256,
                            produced_by
                                .and_then(|known| metadata::produced_by(known, &output.path)),
                            Some(cx.step),
                            false,
                            portable,
                        );
                    }
                }
                // What it read, for the check before a later run (B-23): only when nothing
                // changed while compiling, or the record would describe other bytes.
                if !facts.changed {
                    let outputs = produced
                        .outputs
                        .iter()
                        .filter(|o| matches!(o.kind, OutputKind::Link | OutputKind::Metadata))
                        .map(|o| o.path.clone())
                        .collect();
                    let digests = unit::digests(cx, &produced.observed);
                    self.record_unit(floor, outputs, &digests);
                }
                if trusted && keep == Keep::Store {
                    let min_free = u64::from(self.current.min_free_gb).saturating_mul(1 << 30);
                    facts.low_space = store.free_bytes().is_ok_and(|free| free < min_free);
                    if !facts.low_space {
                        let (stored, bytes) =
                            self.store(store, cx, roots, &produced, &recorded, rustc_ms);
                        facts.stored = stored;
                        if stored == Stored::Took {
                            log.stored_bytes = bytes;
                        }
                    }
                }
            }
        }
        let verdict = decide::after(plan, &facts);
        if verdict.suspend {
            let _ = store.suspend(
                &cx.step,
                "a compile's outputs differed from the block's",
                self.ports.now(),
            );
            log.note = Some(format!(
                "{} built differently from its block; its blocks are not reused until `hushrun build clean`",
                inv.crate_name()
            ));
        }
        log.differed = facts.compared == Compared::Differed;
        log.outcome = verdict.outcome;
        self.log(log);
        ran.code
    }

    /// Steps 9 to 12: what the compile read, its outputs, and where the block may be reused.
    #[allow(clippy::too_many_arguments)]
    fn produced(
        &mut self,
        cx: &Context<'_>,
        roots: &Roots,
        common: &Common,
        recorded: &Recorded,
        floor: SystemTime,
        store: &Store,
        facts: &mut Facts,
        (stdout, before): (&[u8], &BTreeMap<String, Stat>),
        found: Option<&Found>,
    ) -> Result<Produced, NotStored> {
        let inv = self.invocation;
        let dep_path = inv.dep_info_path();
        let text = std::fs::read_to_string(&dep_path).map_err(|_| NotStored::DepInfo)?;
        let info = depinfo::parse(&text).map_err(|_| NotStored::DepInfo)?;
        info.check().map_err(|_| NotStored::DepInfo)?;
        let gathered = cx.after(&info, common, floor)?;
        facts.changed = gathered.changed;
        // What the lookup hashed must still be there as it was (B-11): otherwise the block
        // found describes other inputs and no comparison with it is fair.
        if let Some(found) = found {
            facts.changed |= inputs::moved(
                &found.watched,
                floor,
                &mut std::collections::BTreeSet::new(),
            );
        }
        let search = Search::new(roots);
        let mut outputs = self.outputs(recorded, &search)?;
        let mut outside = gathered.outside;
        let mut observed = gathered.observed;
        if inv.class() != Class::Library {
            let linked = self.link(cx, stdout, floor)?;
            observed.link = Some(linked.link);
            outside |= linked.outside;
            facts.changed |= linked.changed;
            outputs.extend(self.debug_outputs(&search, before)?);
        }

        let tied_by_bytes = outputs.iter().any(|o| o.tied);
        let in_outputs: BTreeSet<Folder> = outputs
            .iter()
            .flat_map(|o| o.machine.iter().copied())
            .collect();
        let outputs: Vec<OutputFile> = outputs.into_iter().map(|o| o.file).collect();
        let origin = observed.key().map_err(|_| NotStored::Key)?;
        let known_by = observed.known_by().map_err(|_| NotStored::Key)?;
        let mut tied = !inv.searchable() || tied_by_bytes || outside;
        // The variables the compile read whose value names a root, or only bases (B-10).
        let mut through_roots: Vec<(String, String)> = Vec::new();
        let mut through_bases: Vec<(String, String)> = Vec::new();
        for dep in &info.env {
            let Some(value) = dep.value.as_ref() else {
                continue;
            };
            let named = roots.named(value);
            if !named.is_empty() && named.iter().all(|name| name.is_base()) {
                through_bases.push((dep.name.clone(), value.clone()));
            } else if roots.to_placeholders(value) != *value {
                through_roots.push((dep.name.clone(), value.clone()));
            }
        }
        // Proved before, by whoever stored this very block (B-10).
        let stored_before = || match store.block(&origin) {
            Ok(Some(block)) => Some(Description::from_value(&block.description)),
            _ => None,
        };
        if !tied && !facts.changed && !through_roots.is_empty() && stored_before().is_none() {
            tied = !self.prove(roots, &through_roots, &outputs)?;
        }
        // A variable that names only a base is the same in every checkout here: it never ties
        // the block. Whether its value left a trace of this machine in the outputs (a path
        // hashed, say) is proved the same way, once: otherwise the block stays here.
        let variable_names_base = match () {
            _ if through_bases.is_empty() => false,
            _ if tied || facts.changed => true,
            _ => match stored_before() {
                Some(description) => description
                    .is_none_or(|d| d.portability.reasons.contains(&Reason::VariableNamesBase)),
                None => !self.prove(roots, &through_bases, &outputs).unwrap_or(false),
            },
        };
        if tied {
            observed.place = Place::Tied(roots.tied());
        }
        let placeholders = |text: &str| roots.to_placeholders(text);
        let records: Vec<String> = [
            serde_json::to_string(&StoredDepInfo::of(&info, placeholders)),
            serde_json::to_string(&recorded.store(placeholders)),
        ]
        .into_iter()
        .collect::<Result<_, _>>()
        .map_err(|_| NotStored::Messages)?;
        let asked = self.asked.as_ref().ok_or(NotStored::Key)?;
        let portability = portable::portable(&portable::Facts {
            class: inv.class(),
            asked,
            observed: &observed,
            records: &records,
            in_outputs: &in_outputs,
            machine: roots.machine_needles(),
            variable_names_base,
            externs: &common.externs_portable,
        });
        if self.current.key_log.is_some() {
            self.keys.borrow_mut().in_outputs = in_outputs.into_iter().collect();
        }
        Ok(Produced {
            observed,
            known_by,
            outputs,
            dep_info: info,
            portability,
        })
    }

    /// Step 11: the files rustc announced, each hashed and searched for the roots and for the
    /// folders that name the machine in one pass. The dep-info is left out: it holds real paths
    /// by design.
    fn outputs(&self, recorded: &Recorded, search: &Search) -> Result<Vec<Searched>, NotStored> {
        let inv = self.invocation;
        let out_dir = Path::new(inv.cwd()).join(inv.out_dir());
        let mut emits: Vec<Emit> = Vec::new();
        let mut out = Vec::new();
        for line in recorded.lines() {
            let Line::Notice { artifact, emit } = line else {
                continue;
            };
            let (emit, kind) = match emit.as_str() {
                "dep-info" => (Emit::DepInfo, None),
                "metadata" => (Emit::Metadata, Some(OutputKind::Metadata)),
                "link" => (Emit::Link, Some(OutputKind::Link)),
                _ => return Err(NotStored::Outputs),
            };
            if emits.contains(&emit) {
                return Err(NotStored::Outputs);
            }
            emits.push(emit);
            let path = Path::new(inv.cwd()).join(artifact);
            if path.parent() != Some(out_dir.as_path()) {
                return Err(NotStored::Outputs);
            }
            let Some(kind) = kind else {
                if path != inv.dep_info_path() {
                    return Err(NotStored::Outputs);
                }
                continue;
            };
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or(NotStored::Outputs)?
                .to_string();
            out.push(search.file(path, name, kind)?);
        }
        let mut asked = inv.emits().to_vec();
        asked.sort();
        emits.sort();
        if asked != emits {
            return Err(NotStored::Outputs);
        }
        Ok(out)
    }

    /// B-10's proof: the compile once more into a scratch folder, with each variable that named
    /// a root pointing at a link to the same place. Same bytes: the block may move.
    fn prove(
        &mut self,
        roots: &Roots,
        vars: &[(String, String)],
        outputs: &[OutputFile],
    ) -> Result<bool, NotStored> {
        let inv = self.invocation;
        let out_dir = Path::new(inv.cwd()).join(inv.out_dir());
        let scratch = out_dir.join(sidecar::DIR).join(format!(
            "proof-{}-{}",
            self.ports.pid(),
            epoch_ms(self.ports.now())
        ));
        let result = self.prove_in(&scratch, roots, vars, outputs);
        let _ = std::fs::remove_dir_all(&scratch);
        result
    }

    fn prove_in(
        &mut self,
        scratch: &Path,
        roots: &Roots,
        vars: &[(String, String)],
        outputs: &[OutputFile],
    ) -> Result<bool, NotStored> {
        let inv = self.invocation;
        let scratch_out = scratch.join("out");
        std::fs::create_dir_all(&scratch_out).map_err(|_| NotStored::PlaceCheck)?;
        let mut extra = self.added(roots);
        let mut env = Vec::with_capacity(vars.len());
        for (i, (name, value)) in vars.iter().enumerate() {
            let target = Path::new(inv.cwd()).join(value);
            // Only a variable that names one place can be pointed elsewhere.
            if !target.exists() {
                return Ok(false);
            }
            let alias = scratch.join(format!("alias-{i}"));
            symlink(&target, &alias).map_err(|_| NotStored::PlaceCheck)?;
            let alias_text = alias.to_str().ok_or(NotStored::PlaceCheck)?.to_string();
            extra.push(OsString::from(format!(
                "--remap-path-prefix={alias_text}={}",
                roots.remapped(value)
            )));
            env.push((OsString::from(name), OsString::from(alias_text)));
        }
        let args = inv.command_in(&extra, &scratch_out);
        let rustc = OsString::from(inv.rustc());
        let ran = self
            .ports
            .compile(
                &Run {
                    rustc: &rustc,
                    args: &args,
                    env: &env,
                    capture_stdout: true,
                },
                &mut |_| {},
            )
            .map_err(|_| NotStored::PlaceCheck)?;
        if !ran.success {
            return Err(NotStored::PlaceCheck);
        }
        for output in outputs {
            let again = scratch_out.join(&output.name);
            let Ok((sha, _)) = hushrun_digest::hash::hash_file(&again) else {
                return Ok(false);
            };
            if Digest::from_bytes(sha) != output.sha256 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// What the wrapper adds to a compile it doesn't pass through (B-9): the remaps, the
    /// sysroot without sources when the toolchain has sources, and a linked output's additions.
    fn added(&self, roots: &Roots) -> Vec<OsString> {
        let mut extra = roots.remap_args();
        extra.extend(self.hide_sources.iter().cloned());
        extra.extend(self.link_extra.iter().cloned());
        extra
    }

    /// Whether `dir` lies under the compiler's sysroot, in either form.
    fn in_sysroot(&self, dir: &Path) -> bool {
        let Some(sysroot) = &self.sysroot else {
            return false;
        };
        if dir.starts_with(sysroot) {
            return true;
        }
        match (std::fs::canonicalize(dir), std::fs::canonicalize(sysroot)) {
            (Ok(dir), Ok(sysroot)) => dir.starts_with(sysroot),
            _ => false,
        }
    }

    /// The output folder as an absolute path.
    fn out_dir(&self) -> PathBuf {
        Path::new(self.invocation.cwd()).join(self.invocation.out_dir())
    }

    /// Where rustc writes a program's debug bundle: `<output folder>/<file stem>.dSYM`.
    fn bundle_dir(&self) -> PathBuf {
        self.out_dir()
            .join(bundle_name(&self.invocation.file_stem()))
    }

    /// B-18: what the linker was handed, from rustc's `--print=link-args` line, with whether a
    /// file it names lies outside every root (that ties the block, B-8) and whether one changed
    /// while compiling (B-11): a date at or after `start`, or a link on the way to it that is.
    fn link(
        &self,
        cx: &Context<'_>,
        stdout: &[u8],
        start: SystemTime,
    ) -> Result<Linked, NotStored> {
        let text = std::str::from_utf8(stdout).map_err(|_| NotStored::LinkInputs)?;
        let args = link::parse(text).map_err(|_| NotStored::LinkInputs)?;
        let cwd = args
            .cwd
            .as_ref()
            .map_or_else(|| PathBuf::from(self.invocation.cwd()), PathBuf::from);
        let mut inputs = args.inputs(&cwd);
        // rustc's own temporary search folder (Linux's `-L <out>/rustcXXXXXX/raw-dylibs`): its
        // stubs come from the compile itself and its name is new each time (B-18).
        let out_dir = self.out_dir();
        inputs
            .search
            .retain(|dir| !link::under_rustc_temp(dir, &out_dir));
        // Arguments hidden in a file can't be keyed.
        if !inputs.response_files.is_empty() {
            return Err(NotStored::LinkInputs);
        }
        // A driver folder (`-B`) is searched for programs the driver runs: only the compiler's
        // own (rustc's `gcc-ld`) is covered by its identity (B-17, B-18).
        for prefix in &inputs.prefixes {
            if !self.in_sysroot(prefix) {
                return Err(NotStored::LinkInputs);
            }
        }
        let path_var = args
            .set_var("PATH")
            .map(OsString::from)
            .or_else(|| self.env.get("PATH").map(OsStr::to_os_string));
        let linker =
            link::linker(&args.linker, path_var.as_deref(), &cwd).ok_or(NotStored::LinkInputs)?;
        // The linker the driver runs instead of its own (B-18).
        let ld = match &inputs.fuse_ld {
            None => None,
            Some(name) => Some(
                link::fuse_linker(name, &inputs.prefixes, path_var.as_deref(), &cwd)
                    .ok_or(NotStored::LinkInputs)?
                    .path,
            ),
        };
        let mut outside = false;
        let mut changed = false;
        let mut links = std::collections::BTreeSet::new();
        let view = self.view.clone();
        let mut moved = |path: &Path, stat: Option<&Stat>| {
            // A path under the view is the toolchain's file it leads to: the view's links are
            // new when the run made them, and the compiler's identity holds the toolchain by
            // content (B-11, B-17).
            let path = match &view {
                Some((view, sysroot)) => match path.strip_prefix(view) {
                    Ok(rest) => sysroot.join(rest),
                    Err(_) => path.to_path_buf(),
                },
                None => path.to_path_buf(),
            };
            stat.is_some_and(|stat| inputs::dated_after(stat, start))
                || inputs::links_changed(&path, start, &mut links)
        };
        // The linker, the one `-fuse-ld` names and the SDK, located like every other path, so a
        // path under another checkout is never recorded as it is (B-8).
        let linker_at = cx.located_path(&linker.path)?;
        let ld_at = match &ld {
            Some(path) => Some(cx.located_path(path)?),
            None => None,
        };
        let sdk_at = match &inputs.sdk {
            Some(path) => Some(cx.located_path(path)?),
            None => None,
        };
        // The `dsymutil` rustc runs for a program's bundle: the first on its own `PATH` (B-9).
        let dsymutil = match self.bundle {
            false => None,
            true => Some(
                link::linker("dsymutil", self.env.get("PATH"), &cwd)
                    .ok_or(NotStored::LinkInputs)?
                    .path,
            ),
        };
        let dsymutil_at = match &dsymutil {
            Some(path) => Some(cx.located_path(path)?),
            None => None,
        };
        for (at, path) in [
            (Some(&linker_at), Some(&linker.path)),
            (ld_at.as_ref(), ld.as_ref()),
            (dsymutil_at.as_ref(), dsymutil.as_ref()),
        ] {
            if let (Some(at), Some(path)) = (at, path) {
                outside |= matches!(at, Located::Outside(_));
                let stat = std::fs::metadata(path).ok().map(|m| Stat::of(&m));
                changed |= moved(path, stat.as_ref());
            }
        }
        outside |= matches!(sdk_at, Some(Located::Outside(_)));
        // Libraries named with `-l` found through `-L` and `-F`, and the files options name
        // (B-6, B-18). Cost: one stat per candidate, only for linked outputs.
        let mut files = Vec::new();
        let mut keyed: std::collections::BTreeSet<PathBuf> =
            inputs.libraries.iter().cloned().collect();
        for place in link::library_places(&inputs) {
            let (read, stat, is_outside) = cx.place(&place)?;
            outside |= is_outside;
            changed |= stat.is_some() && moved(&place, stat.as_ref());
            files.push(read);
            keyed.insert(place);
        }
        for (places, script) in link::file_places(&inputs, &cwd) {
            // rustc's own symbol list, gone after the link: it comes from the compile itself.
            if places.len() == 1 && link::in_rustc_temp(&places[0]) {
                continue;
            }
            let mut found = false;
            for place in &places {
                let (read, stat, is_outside) = cx.place(place)?;
                if read.state != crate::key::ReadState::Absent {
                    found = true;
                    changed |= moved(place, stat.as_ref());
                    if script {
                        let text =
                            crate::regular::read(place).map_err(|_| NotStored::LinkInputs)?;
                        if link::script_names_files(&text) {
                            return Err(NotStored::LinkInputs);
                        }
                    }
                }
                outside |= is_outside;
                files.push(read);
                keyed.insert(place.clone());
            }
            // A file the linker read that is gone can't be keyed.
            if !found {
                return Err(NotStored::LinkInputs);
            }
        }
        // Any other argument that is the path of an existing regular file (an object a
        // `link-arg` names, a file an option this list doesn't know names) is an input too, but
        // the compile's own outputs and rustc's own temporary files (B-18). Cost: one stat per
        // argument that is not an option.
        let stem = self.invocation.file_stem();
        for path in &inputs.paths {
            let own_output = path.parent() == Some(out_dir.as_path())
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with(&stem));
            if keyed.contains(path) || link::in_rustc_temp(path) || own_output {
                continue;
            }
            let meta = match std::fs::metadata(path) {
                Ok(meta) => meta,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(NotStored::LinkInputs),
            };
            if meta.is_dir() {
                continue;
            }
            if !crate::regular::is_regular(&meta) {
                return Err(NotStored::NotAFile);
            }
            let (read, stat, is_outside) = cx.place(path)?;
            outside |= is_outside;
            changed |= moved(path, stat.as_ref());
            files.push(read);
            keyed.insert(path.clone());
        }
        let mut libraries = Vec::with_capacity(inputs.libraries.len());
        for library in &inputs.libraries {
            let at = cx.located_path(library)?;
            outside |= matches!(at, Located::Outside(_));
            let sha256 = cx.digest(library).map_err(|_| NotStored::LinkInputs)?;
            let stat = std::fs::metadata(library)
                .map(|m| Stat::of(&m))
                .map_err(|_| NotStored::LinkInputs)?;
            changed |= moved(library, Some(&stat));
            libraries.push(LinkLibrary { at, sha256 });
        }
        let link = Link {
            libraries,
            files,
            linker: inputs::linkers_text(
                (&linker_at, &linker.path),
                ld_at.as_ref().zip(ld.as_deref()),
                dsymutil_at.as_ref().zip(dsymutil.as_deref()),
            )
            .ok_or(NotStored::LinkInputs)?,
            sdk: inputs::sdk_text(sdk_at.as_ref().zip(inputs.sdk.as_deref())),
            linker_path: linker_at,
            ld_path: ld_at,
            sdk_path: sdk_at,
            dsymutil_path: dsymutil_at,
        };
        Ok(Linked {
            link,
            outside,
            changed,
        })
    }

    /// A program's debug files (B-18): its debug bundle when it gets one (B-9), else the
    /// `<crate><extra>.*.o` files in the output folder the compile wrote. Debug files a block
    /// doesn't carry (a `.dSYM` the wrapper didn't ask for, `.dwp`) refuse the store.
    fn debug_outputs(
        &self,
        search: &Search,
        before: &BTreeMap<String, Stat>,
    ) -> Result<Vec<Searched>, NotStored> {
        if self.bundle {
            return self.bundle_outputs(search);
        }
        let inv = self.invocation;
        let stem = inv.file_stem();
        let out_dir = self.out_dir();
        let entries = std::fs::read_dir(&out_dir).map_err(|_| NotStored::Outputs)?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&stem) && (name.ends_with(".dSYM") || name.ends_with(".dwp")) {
                return Err(NotStored::DebugFiles);
            }
        }
        if inv.class() != Class::Program {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (name, stat) in debug_objects(&out_dir, &stem) {
            if before
                .get(&name)
                .is_some_and(|old| inputs::same(old, &stat))
            {
                continue;
            }
            let path = out_dir.join(&name);
            out.push(search.file(path, name, OutputKind::DebugObject)?);
        }
        Ok(out)
    }

    /// A program's debug bundle as outputs (B-9, B-10): every file under `<stem>.dSYM`, named
    /// by its path from the output folder, hashed and searched for the roots like every other
    /// output. The relocations dsymutil writes for tools that rewrite a program later
    /// (`Contents/Resources/Relocations`) name the program by its absolute path; no debugger
    /// reads them, and they are removed first. A bundle with no DWARF file, or holding anything
    /// but folders and regular files, refuses the store.
    fn bundle_outputs(&self, search: &Search) -> Result<Vec<Searched>, NotStored> {
        let dir = self.bundle_dir();
        let top = bundle_name(&self.invocation.file_stem());
        match std::fs::remove_dir_all(dir.join(RELOCATIONS)) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(NotStored::DebugFiles),
        }
        let mut files = Vec::new();
        bundle_files(&dir, &top, &mut files)?;
        let dwarf = format!("{top}/{DWARF}/");
        if !files.iter().any(|(_, name)| name.starts_with(&dwarf)) {
            return Err(NotStored::DebugFiles);
        }
        let mut out = Vec::with_capacity(files.len());
        for (path, name) in files {
            out.push(search.file(path, name, OutputKind::DebugBundle)?);
        }
        Ok(out)
    }

    /// Step 13: the block, its hint and its last use.
    fn store(
        &self,
        store: &Store,
        cx: &Context<'_>,
        roots: &Roots,
        produced: &Produced,
        recorded: &Recorded,
        rustc_ms: u64,
    ) -> (Stored, u64) {
        let Ok(key) = produced.observed.key() else {
            return (Stored::Refused, 0);
        };
        let placeholders = |text: &str| roots.to_placeholders(text);
        let description = Description {
            schema: SCHEMA.to_string(),
            repository: self.current.repository.clone(),
            class: self.invocation.class(),
            crate_name: self.invocation.crate_name().to_string(),
            package: self.invocation.package().name.clone(),
            step: cx.step,
            origin: produced.known_by,
            place: match produced.observed.place {
                Place::Anywhere => PlaceKind::Anywhere,
                Place::Tied(_) => PlaceKind::Tied,
            },
            portability: produced.portability.clone(),
            outputs: produced
                .outputs
                .iter()
                .map(|o| Output {
                    name: o.name.clone(),
                    kind: o.kind,
                })
                .collect(),
            dep_info: StoredDepInfo::of(&produced.dep_info, placeholders),
            messages: recorded.store(placeholders),
            rustc_ms,
            built_ms: epoch_ms(self.start),
        };
        let Ok(value) = description.to_value() else {
            return (Stored::Refused, 0);
        };
        let files: Vec<NewFile> = produced
            .outputs
            .iter()
            .map(|o| NewFile {
                name: o.name.clone(),
                source: o.path.clone(),
            })
            .collect();
        let now = self.ports.now();
        let stored = match store.publish(&key, &value, &files, now) {
            Ok(Published::Stored) => Stored::Took,
            Ok(Published::AlreadyThere) => Stored::AlreadyHad,
            Err(_) => return (Stored::Refused, 0),
        };
        if let Ok(hint) = serde_json::to_vec(&ReadList::of(&produced.observed)) {
            let _ = store.add_hint(&cx.step, &hint, now);
        }
        let _ = store.used(&key, now);
        let bytes = produced.outputs.iter().map(|o| o.size).sum();
        (stored, bytes)
    }

    /// B-15: a dependent that could not load a reused crate suspends that crate's first key and
    /// removes its reused files, so cargo builds it. Any other error changes nothing.
    fn reject_reused(&self, store: &Store, recorded: &Recorded) -> Option<String> {
        let diagnostics = recorded.lines().iter().filter_map(|line| match line {
            Line::Diagnostic(value) => Some(value),
            _ => None,
        });
        let named = decide::not_loaded(diagnostics)?;
        let now = self.ports.now();
        let mut names = Vec::new();
        for ext in self.invocation.externs() {
            let Some(given) = &ext.path else { continue };
            let path = Path::new(self.invocation.cwd()).join(given);
            // Only an extern the errors name: by its name, its crate's name or its file. A
            // crate that is no extern of this call (a typo in this crate's own code) is none.
            let file_crate = crate_of_file(&path);
            let is_named = named.names_crate(&ext.name)
                || file_crate.is_some_and(|name| named.names_crate(name))
                || named.names_file(given)
                || path.to_str().is_some_and(|text| named.names_file(text));
            if !is_named {
                continue;
            }
            let Ok(known) = sidecar::lookup(&path) else {
                continue;
            };
            let (true, Some(step)) = (known.reused, known.step) else {
                continue;
            };
            let _ = store.suspend(&step, "a dependent could not load a reused block", now);
            if let Some(dir) = path.parent() {
                for file in sidecar::reused_in(dir).unwrap_or_default() {
                    if sidecar::lookup(&file).is_ok_and(|k| k.step == Some(step)) {
                        let _ = std::fs::remove_file(&file);
                        let _ = sidecar::forget(&file);
                    }
                }
            }
            names.push(ext.name.clone());
        }
        if names.is_empty() {
            return None;
        }
        names.sort();
        names.dedup();
        Some(format!(
            "{} could not load reused {}; suspended and removed so cargo builds them again",
            self.invocation.crate_name(),
            names.join(", ")
        ))
    }
}

/// The crate a dependency's file holds, from its name: `lib<crate>-<extra>.<ext>`.
fn crate_of_file(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    let name = stem.strip_prefix("lib").unwrap_or(stem);
    Some(name.rsplit_once('-').map_or(name, |(name, _)| name))
}

/// Where dsymutil writes the relocations a later rewrite of the program needs, in a bundle.
const RELOCATIONS: &str = "Contents/Resources/Relocations";
/// Where the DWARF files of a bundle lie.
const DWARF: &str = "Contents/Resources/DWARF";

/// The name of a program's debug bundle: `<file stem>.dSYM`.
fn bundle_name(stem: &str) -> String {
    format!("{stem}.dSYM")
}

/// Whether a compile is a program that gets a debug bundle and may move (B-9): a program that
/// links, for an Apple target, with debug info, whose own flags don't rule a bundle out. A
/// `-C split-debuginfo=off` (no bundle wanted) or a `-C strip` other than `none` (the person
/// strips the program themselves) leaves the program as it is, tied to its folder like before.
/// An explicit `unpacked` can't be told from cargo's default for its dev profile on macOS, and
/// is replaced.
fn bundled(inv: &Invocation, apple: bool) -> bool {
    let debug = inv.debug();
    inv.class() == Class::Program
        && apple
        && inv.emits().contains(&Emit::Link)
        && debug.any()
        && matches!(debug.split.as_deref(), None | Some("packed" | "unpacked"))
        && matches!(debug.strip.as_deref(), None | Some("none"))
}

/// What a linked output's compile gets besides the remaps: `--print=link-args` for its link
/// inputs (B-18) and, on Apple targets (B-9), an install name and debug-map prefix that don't
/// hold the folder it was linked in for a proc macro and a build script, and for a program that
/// gets a debug bundle ([`bundled`]): `-C split-debuginfo=packed`, so rustc runs dsymutil and
/// writes `<program>.dSYM` with the DWARF of every object; `-C strip=debuginfo`, so the program
/// keeps no debug map naming those objects by path (the bundle has what it pointed at); and
/// `-oso_prefix` for the workspace, so the program's UUID, which the linker computes before the
/// strip, holds no path when the target folder is inside it.
fn linked_args(inv: &Invocation, apple: bool, bundle: bool) -> Vec<OsString> {
    let mut out = vec![OsString::from("--print=link-args")];
    if !apple {
        return out;
    }
    let class = inv.class();
    if bundle {
        for arg in ["split-debuginfo=packed", "strip=debuginfo"] {
            out.push("-C".into());
            out.push(arg.into());
        }
        // The linker names objects by their real path. A comma would split `-Wl,`.
        let workspace = crate::roots::resolve_on_disk(inv.cwd()).unwrap_or(inv.cwd().to_string());
        if !workspace.contains(',') {
            out.push("-C".into());
            out.push(
                format!(
                    "link-arg=-Wl,-oso_prefix,{}/",
                    workspace.trim_end_matches('/')
                )
                .into(),
            );
        }
    }
    if class == Class::ProcMacro {
        out.push("-C".into());
        out.push(
            format!(
                "link-arg=-Wl,-install_name,@rpath/lib{}.dylib",
                inv.file_stem()
            )
            .into(),
        );
    }
    if matches!(class, Class::ProcMacro | Class::BuildScript)
        && let Some(deps) = inv.dependency_dirs().next()
    {
        out.push("-C".into());
        out.push(format!("link-arg=-Wl,-oso_prefix,{}/", deps.trim_end_matches('/')).into());
    }
    out
}

/// Collects the regular files under the bundle folder `dir`, with their names from the output
/// folder (`top/...`), in a stable order. Anything else in it (a link, a pipe) or a name that
/// is not UTF-8 refuses the store.
fn bundle_files(dir: &Path, top: &str, out: &mut Vec<(PathBuf, String)>) -> Result<(), NotStored> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|_| NotStored::DebugFiles)?
        .collect::<Result<_, _>>()
        .map_err(|_| NotStored::DebugFiles)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| NotStored::DebugFiles)?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|_| NotStored::DebugFiles)?;
        let named = format!("{top}/{name}");
        if kind.is_dir() {
            bundle_files(&path, &named, out)?;
        } else if kind.is_file() {
            out.push((path, named));
        } else {
            return Err(NotStored::DebugFiles);
        }
    }
    Ok(())
}

/// The loose objects named `<stem>.*.o` in a folder, by name, with their stats.
fn debug_objects(dir: &Path, stem: &str) -> BTreeMap<String, Stat> {
    let prefix = format!("{stem}.");
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with(&prefix)
            && name.ends_with(".o")
            && let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            out.insert(name, Stat::of(&meta));
        }
    }
    out
}

/// Step 14: the fresh outputs' bytes against the block's, by name (B-1, B-16).
fn compare_outputs(outputs: &[OutputFile], block: &Block) -> Compared {
    if outputs.len() != block.files.len() {
        return Compared::Differed;
    }
    for output in outputs {
        let same = block
            .files
            .iter()
            .any(|f| f.name == output.name && f.sha256 == output.sha256);
        if !same {
            return Compared::Differed;
        }
    }
    Compared::Same
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::other("no symlinks here"))
}

/// What `<rustc> <args>` prints on stdout, when it succeeds.
fn run_for_output(rustc: &Path, args: &[&str]) -> io::Result<String> {
    let out = std::process::Command::new(rustc).args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{} {} failed",
            rustc.display(),
            args.join(" ")
        )));
    }
    String::from_utf8(out.stdout).map_err(io::Error::other)
}

/// The real world.
struct Real;

impl Ports for Real {
    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn vars(&self) -> Vec<(OsString, OsString)> {
        std::env::vars_os().collect()
    }

    fn cwd(&self) -> io::Result<PathBuf> {
        std::env::current_dir()
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    #[cfg(target_os = "linux")]
    fn file_clock(&self, dir: &Path) -> Option<SystemTime> {
        file_clock(dir, std::process::id())
    }

    fn pid(&self) -> u32 {
        std::process::id()
    }

    fn rustc_version(&self, rustc: &Path) -> io::Result<String> {
        run_for_output(rustc, &["-vV"])
    }

    fn rustc_targets(&self, rustc: &Path) -> io::Result<String> {
        run_for_output(rustc, &["--print", "target-list"])
    }

    fn exec(&mut self, rustc: &OsStr, args: &[OsString]) -> i32 {
        let mut command = std::process::Command::new(rustc);
        command.args(args);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = command.exec();
            eprintln!(
                "error: hushrun could not run {}: {err}",
                Path::new(rustc).display()
            );
            1
        }
        #[cfg(not(unix))]
        match command.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(err) => {
                eprintln!(
                    "error: hushrun could not run {}: {err}",
                    Path::new(rustc).display()
                );
                1
            }
        }
    }

    fn compile(&mut self, run: &Run<'_>, line: &mut dyn FnMut(&str)) -> io::Result<Ran> {
        use std::io::{BufRead, Read};
        use std::process::{Command, Stdio};
        let mut command = Command::new(run.rustc);
        command.args(run.args).stderr(Stdio::piped());
        for (name, value) in run.env {
            command.env(name, value);
        }
        if run.capture_stdout {
            command.stdout(Stdio::piped());
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().map(|mut out| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = out.read_to_end(&mut bytes);
                bytes
            })
        });
        if let Some(stderr) = child.stderr.take() {
            let mut reader = io::BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(_) => line(&String::from_utf8_lossy(&buf)),
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        let status = child.wait()?;
        let stdout = stdout.and_then(|t| t.join().ok()).unwrap_or_default();
        Ok(Ran {
            code: status.code().unwrap_or(1),
            success: status.success(),
            stdout,
        })
    }

    fn stdout(&mut self, bytes: &[u8]) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }
}

/// Creates an empty file in `dir` under a name of this process's, reads the date the file system
/// gave it and removes it. Cost: a create, a stat and an unlink.
#[cfg(any(target_os = "linux", test))]
fn file_clock(dir: &Path, pid: u32) -> Option<SystemTime> {
    let probe = dir.join(format!(".hushrun-clock-{pid}"));
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|file| file.metadata())
        .and_then(|meta| meta.modified());
    // Removed whether or not its date could be read; a name this process made, nobody else's.
    if !matches!(&made, Err(err) if err.kind() == io::ErrorKind::AlreadyExists) {
        let _ = std::fs::remove_file(&probe);
    }
    made.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-11
    #[test]
    fn a_file_written_just_before_the_compile_is_not_taken_for_one_written_during_it() {
        let dir = tempfile::tempdir().unwrap();
        let stat = |p: &Path| Stat::of(&std::fs::metadata(p).unwrap());
        let before = dir.path().join("generated.rs");
        std::fs::write(&before, b"").unwrap();
        // A build script's output, a few milliseconds before cargo starts the compile that
        // reads it: more than a tick of Linux's file clock, less than its fixed slack.
        std::thread::sleep(Duration::from_millis(5));
        let floor = inputs::floor(file_clock(dir.path(), 1), SystemTime::now());
        assert!(!inputs::dated_after(&stat(&before), floor));
        // Anything written after it is dated at or after it.
        let during = dir.path().join("during.rs");
        std::fs::write(&during, b"").unwrap();
        assert!(inputs::dated_after(&stat(&during), floor));
        // The probe leaves nothing behind.
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 2);
    }

    // covers: B-2
    #[test]
    fn the_wrapper_is_known_by_its_own_name() {
        assert!(called_as_wrapper(OsStr::new("hushrun-rustc")));
        assert!(called_as_wrapper(OsStr::new(
            "/store/bin/ab12/hushrun-rustc"
        )));
        assert!(called_as_wrapper(OsStr::new("hushrun-rustc.exe")));
        assert!(!called_as_wrapper(OsStr::new("hushrun")));
        assert!(!called_as_wrapper(OsStr::new("/usr/local/bin/hushrun")));
        assert!(!called_as_wrapper(OsStr::new("rustc")));
        assert!(!called_as_wrapper(OsStr::new("")));
    }

    #[test]
    fn the_crate_name_of_a_refused_call_is_logged() {
        let args = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            crate_name_of(&args(&["--crate-name", "x", "@file"])).as_deref(),
            Some("x")
        );
        assert_eq!(
            crate_name_of(&args(&["--crate-name=y"])).as_deref(),
            Some("y")
        );
        assert_eq!(crate_name_of(&args(&["-vV"])), None);
    }
}
