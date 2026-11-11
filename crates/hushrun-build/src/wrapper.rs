//! The wrapper itself (B-2, B-19): one compiler call from start to exit code.
//!
//! [`call`] is the whole flow over [`Ports`]: the process's variables, the clock, running rustc.
//! [`main`] wires the real ones; tests drive [`call`] with a fake compiler. Every error of this
//! machinery is a value that ends in rustc running as asked (rule 1).

use std::collections::BTreeMap;
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
use crate::model::{Class, NotStored, Outcome, PassedThrough};
use crate::restore::Restore;
use crate::roots::{RootPaths, Roots, resolve_on_disk};
use crate::scan::Needles;
use crate::script::{self, Script};
use crate::session::{self, Current, Entry};
use crate::store::{Block, NewFile, Published, Store};
use crate::{audit, compiler, inputs, link, sidecar};

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
    // No session: the cheapest pass-through, before anything is read (B-2).
    if ports.var(session::SESSION).is_none_or(|v| v.is_empty()) {
        return ports.exec(rustc, args);
    }
    let Ok(cwd) = ports.cwd() else {
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
        return ports.exec(rustc, args);
    };
    Call {
        ports,
        err,
        current: &current,
        invocation: &invocation,
        env: &env,
        start,
        link_extra: Vec::new(),
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
    /// What a linked output's compile gets besides the remaps (B-9, B-18).
    link_extra: Vec<OsString>,
}

/// What the lookup found, beyond the plan's facts.
struct Found {
    block: Block,
    description: Description,
}

/// What a compile produced, for the store.
struct Produced {
    observed: Observed,
    origin: Digest,
    outputs: Vec<OutputFile>,
    dep_info: depinfo::DepInfo,
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
        self.current.log(&Entry {
            crate_name: self.invocation.crate_name().to_string(),
            package: package_id(package.name.clone(), package.version.clone()),
            ..entry
        });
    }

    /// Runs rustc as asked after logging `outcome` (the process may be replaced).
    fn as_asked(&mut self, outcome: Outcome) -> i32 {
        self.log(entry(outcome, Some(self.invocation.class())));
        let rustc = OsString::from(self.invocation.rustc());
        let args: Vec<OsString> = self.invocation.args().iter().map(OsString::from).collect();
        self.ports.exec(&rustc, &args)
    }

    fn pass(&mut self, why: PassedThrough) -> i32 {
        self.as_asked(decide::passed_through(why))
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
                &store.root().join("compilers"),
                &|path| ports.rustc_version(path),
                &|path| ports.rustc_targets(path),
            )
        };
        let Ok(identity) = identity else {
            return self.pass(PassedThrough::Compiler);
        };
        // A target that is not one of the compiler's own is a file rustc loads (B-4).
        if inv
            .target()
            .is_some_and(|target| !identity.builds_for(target))
        {
            return self.pass(PassedThrough::TargetFile);
        }
        let Ok(mut roots) = Roots::new(&RootPaths::of(inv), &self.current.shared, resolve_on_disk)
        else {
            return self.pass(PassedThrough::Roots);
        };
        roots.add_needles(&inv.package().target_dirs, resolve_on_disk);

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
            self.link_extra = linked_args(inv, apple);
            // Debug info packed beside the output is not something a block carries.
            let packed = inv
                .args()
                .iter()
                .any(|arg| arg.ends_with("split-debuginfo=packed"));
            if packed {
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
        };
        let step = asked.key().ok();
        let cx = step.map(|step| Context {
            invocation: inv,
            roots: &roots,
            env: self.env,
            step,
            script: script.as_ref(),
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
                self.reuse(&store, &roots, found)
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
            let Some(observed) = cx.from_hint(&hint, &shared) else {
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
                    let audit = self.ports.audited(&key, self.current.started_ms);
                    *found = Some(Found { block, description });
                    return Lookup::Found { audit, serves_form };
                }
            }
        }
        Lookup::Missing
    }

    /// B-12: the block's files in place, then its messages; rustc doesn't start. Any failure
    /// before the first message runs rustc as asked.
    fn reuse(&mut self, store: &Store, roots: &Roots, found: &Found) -> i32 {
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
        let _ = self.err.write_all(&restored.messages);
        let _ = self.err.flush();
        let now = self.ports.now();
        let _ = store.used(&found.block.key, now);
        let took = millis(now.duration_since(self.start).unwrap_or_default());
        let verdict = decide::after(Plan::Reuse, &Facts::ran(Rustc::NotRun));
        self.log(Entry {
            saved_ms: found.description.rustc_ms.saturating_sub(took),
            found: true,
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
        let mut extra = roots.remap_args();
        extra.extend(self.link_extra.iter().cloned());
        let args = inv.command(&extra);
        let rustc = OsString::from(inv.rustc());
        let before = if inv.class() == Class::Program {
            debug_objects(&self.out_dir(), &inv.file_stem())
        } else {
            BTreeMap::new()
        };
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
        let produced = common.and_then(|common| {
            self.produced(
                cx,
                roots,
                &common,
                &recorded,
                compile_start,
                store,
                &mut facts,
                (&ran.stdout, &before),
            )
        });
        match produced {
            Err(why) => facts.error = Some(why),
            Ok(produced) => {
                if compare && let Some(found) = found {
                    facts.compared = compare_outputs(&produced.outputs, &found.block);
                }
                let trusted = !facts.changed && facts.compared != Compared::Differed;
                if trusted {
                    // Dependents may know a block by its origin only if it may move: a tied one
                    // is another file in another checkout, under the same origin (B-6).
                    let produced_by = matches!(produced.observed.place, Place::Anywhere)
                        .then_some(produced.origin);
                    for output in &produced.outputs {
                        let _ = sidecar::record_block(
                            &output.path,
                            output.sha256,
                            produced_by,
                            Some(cx.step),
                            false,
                        );
                    }
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
        compile_start: SystemTime,
        store: &Store,
        facts: &mut Facts,
        (stdout, before): (&[u8], &BTreeMap<String, Stat>),
    ) -> Result<Produced, NotStored> {
        let inv = self.invocation;
        let dep_path = inv.dep_info_path();
        let text = std::fs::read_to_string(&dep_path).map_err(|_| NotStored::DepInfo)?;
        let info = depinfo::parse(&text).map_err(|_| NotStored::DepInfo)?;
        info.check().map_err(|_| NotStored::DepInfo)?;
        let gathered = cx.after(&info, common, compile_start)?;
        facts.changed = gathered.changed;
        let mut outputs = self.outputs(recorded, roots)?;
        let mut outside = gathered.outside;
        let mut observed = gathered.observed;
        if inv.class() != Class::Library {
            let (link, link_outside) = self.link(cx, stdout)?;
            observed.link = Some(link);
            outside |= link_outside;
            outputs.extend(self.debug_outputs(roots, before)?);
        }

        let tied_by_bytes = outputs.iter().any(|o| o.1);
        let outputs: Vec<OutputFile> = outputs.into_iter().map(|o| o.0).collect();
        let origin = observed.key().map_err(|_| NotStored::Key)?;
        let mut tied = !inv.searchable() || tied_by_bytes || outside;
        if !tied && !gathered.changed {
            let through_vars: Vec<(String, String)> = info
                .env
                .iter()
                .filter_map(|dep| {
                    let value = dep.value.as_ref()?;
                    (roots.to_placeholders(value) != *value)
                        .then(|| (dep.name.clone(), value.clone()))
                })
                .collect();
            // Proved before, by whoever stored this very block (B-10).
            if !through_vars.is_empty() && !matches!(store.block(&origin), Ok(Some(_))) {
                tied = !self.prove(roots, &through_vars, &outputs)?;
            }
        }
        if tied {
            observed.place = Place::Tied(roots.tied());
        }
        Ok(Produced {
            observed,
            origin,
            outputs,
            dep_info: info,
        })
    }

    /// Step 11: the files rustc announced, each hashed and searched for the roots in one pass.
    /// The dep-info is left out: it holds real paths by design.
    fn outputs(
        &self,
        recorded: &Recorded,
        roots: &Roots,
    ) -> Result<Vec<(OutputFile, bool)>, NotStored> {
        let inv = self.invocation;
        let out_dir = Path::new(inv.cwd()).join(inv.out_dir());
        let needles = Needles::new(roots.needles());
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
            let (hit, sha256, size) = needles.in_file_hashed(&path).map_err(|_| NotStored::Scan)?;
            out.push((
                OutputFile {
                    path,
                    name,
                    kind,
                    sha256,
                    size,
                },
                hit.is_some(),
            ));
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
        let mut extra = roots.remap_args();
        extra.extend(self.link_extra.iter().cloned());
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

    /// The output folder as an absolute path.
    fn out_dir(&self) -> PathBuf {
        Path::new(self.invocation.cwd()).join(self.invocation.out_dir())
    }

    /// B-18: what the linker was handed, from rustc's `--print=link-args` line. Whether a
    /// library lies outside every root comes back too: that ties the block (B-8).
    fn link(&self, cx: &Context<'_>, stdout: &[u8]) -> Result<(Link, bool), NotStored> {
        let text = std::str::from_utf8(stdout).map_err(|_| NotStored::LinkInputs)?;
        let args = link::parse(text).map_err(|_| NotStored::LinkInputs)?;
        let cwd = args
            .cwd
            .as_ref()
            .map_or_else(|| PathBuf::from(self.invocation.cwd()), PathBuf::from);
        let inputs = args.inputs(&cwd);
        // Arguments hidden in a file can't be keyed.
        if !inputs.response_files.is_empty() {
            return Err(NotStored::LinkInputs);
        }
        let path_var = args
            .set_var("PATH")
            .map(OsString::from)
            .or_else(|| self.env.get("PATH").map(OsStr::to_os_string));
        let linker =
            link::linker(&args.linker, path_var.as_deref(), &cwd).ok_or(NotStored::LinkInputs)?;
        let linker_path = linker
            .path
            .to_str()
            .ok_or(NotStored::LinkInputs)?
            .to_string();
        // The linker the driver runs instead of its own (B-18).
        let ld = match &inputs.fuse_ld {
            None => None,
            Some(name) => {
                let found = link::fuse_linker(name, &inputs.prefixes, path_var.as_deref(), &cwd)
                    .ok_or(NotStored::LinkInputs)?;
                Some(
                    found
                        .path
                        .to_str()
                        .ok_or(NotStored::LinkInputs)?
                        .to_string(),
                )
            }
        };
        let mut outside = false;
        // Libraries named with `-l` found through `-L` and `-F`, and the files options name
        // (B-6, B-18). Cost: one stat per candidate, only for linked outputs.
        let mut files = Vec::new();
        for place in link::library_places(&inputs) {
            let (read, _, is_outside) = cx.place(&place)?;
            outside |= is_outside;
            files.push(read);
        }
        for (places, script) in link::file_places(&inputs, &cwd) {
            // rustc's own symbol list, gone after the link: it comes from the compile itself.
            if places.len() == 1 && link::in_rustc_temp(&places[0]) {
                continue;
            }
            let mut found = false;
            for place in &places {
                let (read, _, is_outside) = cx.place(place)?;
                if read.state != crate::key::ReadState::Absent {
                    found = true;
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
            }
            // A file the linker read that is gone can't be keyed.
            if !found {
                return Err(NotStored::LinkInputs);
            }
        }
        let mut libraries = Vec::with_capacity(inputs.libraries.len());
        for library in &inputs.libraries {
            let at = cx.located_path(library)?;
            outside |= matches!(at, crate::model::Located::Outside(_));
            let sha256 = inputs::library_digest(library).map_err(|_| NotStored::LinkInputs)?;
            libraries.push(LinkLibrary { at, sha256 });
        }
        let sdk_path = match &inputs.sdk {
            Some(sdk) => Some(sdk.to_str().ok_or(NotStored::LinkInputs)?.to_string()),
            None => None,
        };
        Ok((
            Link {
                libraries,
                files,
                linker: inputs::linkers_text(&linker.path, ld.as_deref().map(Path::new))
                    .ok_or(NotStored::LinkInputs)?,
                sdk: inputs::sdk_text(sdk_path.as_deref()),
                linker_path,
                ld_path: ld,
                sdk_path,
            },
            outside,
        ))
    }

    /// A program's debug objects (B-18): the `<crate><extra>.*.o` files in the output folder
    /// the compile wrote. Debug files a block doesn't carry (`.dSYM`, `.dwp`) refuse the store.
    fn debug_outputs(
        &self,
        roots: &Roots,
        before: &BTreeMap<String, Stat>,
    ) -> Result<Vec<(OutputFile, bool)>, NotStored> {
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
        let needles = Needles::new(roots.needles());
        let mut out = Vec::new();
        for (name, stat) in debug_objects(&out_dir, &stem) {
            if before
                .get(&name)
                .is_some_and(|old| inputs::same(old, &stat))
            {
                continue;
            }
            let path = out_dir.join(&name);
            let (hit, sha256, size) = needles.in_file_hashed(&path).map_err(|_| NotStored::Scan)?;
            out.push((
                OutputFile {
                    path,
                    name,
                    kind: OutputKind::DebugObject,
                    sha256,
                    size,
                },
                hit.is_some(),
            ));
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
            origin: produced.origin,
            place: match produced.observed.place {
                Place::Anywhere => PlaceKind::Anywhere,
                Place::Tied(_) => PlaceKind::Tied,
            },
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
        let codes = recorded.lines().iter().filter_map(|line| match line {
            Line::Diagnostic(value) => value.get("code")?.get("code")?.as_str(),
            _ => None,
        });
        if !decide::could_not_load(codes) {
            return None;
        }
        let now = self.ports.now();
        let mut names = Vec::new();
        for ext in self.invocation.externs() {
            let Some(path) = &ext.path else { continue };
            let path = Path::new(self.invocation.cwd()).join(path);
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

/// What a linked output's compile gets besides the remaps: `--print=link-args` for its link
/// inputs (B-18) and, on Apple targets, an install name and debug-map prefix that don't hold
/// the folder it was linked in (B-9). Programs get neither: their debug info must work.
fn linked_args(inv: &Invocation, apple: bool) -> Vec<OsString> {
    let mut out = vec![OsString::from("--print=link-args")];
    if !apple {
        return out;
    }
    let class = inv.class();
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

#[cfg(test)]
mod tests {
    use super::*;

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
