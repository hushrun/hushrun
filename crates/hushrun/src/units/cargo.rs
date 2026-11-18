//! \[ADR 0068\] The Rust adapter, compiler-given (C-30, C-32): a unit is a test binary, whose root
//! file names it, and what it read is what rustc's dep-info files say the build read for it.
//!
//! - Before COMMAND: when COMMAND is `cargo nextest run …` or `cargo test …`, hushrun sets
//!   `CARGO_TARGET_<TRIPLE>_RUNNER` to its own target runner ([`super::runner`]), chained to a
//!   runner the environment already sets. When Cargo's configuration sets one, which this version
//!   can't chain to safely, it sets nothing, and the check runs as it always did.
//! - After it: hushrun reads which binaries the runner started and how each ended, then asks
//!   Cargo for the build COMMAND made (`cargo test --no-run --message-format=json` with COMMAND's
//!   own build options), which must be fresh: anything Cargo would build means it isn't COMMAND's
//!   build, and no unit is observed. From that build's artifacts it reads each test binary's
//!   dep-info, the dep-info of every workspace library it may link, and, for an integration test,
//!   of every target of its package, since it may run them (the package rule); \[ADR 0096\] and
//!   of the build scripts of their packages, with what each build script's run gave the build.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use hushrun_core::digest::Digest;
use hushrun_core::units::{self, Facts, NotFollowed, Sets, Sources};

use super::adapter::{
    Adapter, Hints, KeysOutside, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::facts::{FactsFile, Invocation};
use super::runner;

/// The adapter's name in a facts file.
pub const ADAPTER: &str = "cargo";
/// What a compiler-given adapter's sources are, of the files a build names (C-30).
pub const SOURCE_KINDS: &[&str] = &[".rs"];
/// The longest hushrun waits for Cargo to say what COMMAND built.
const BUILD_BUDGET: Duration = Duration::from_secs(120);

/// COMMAND, when it runs a Cargo workspace's tests in a way hushrun can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// `cargo`, or what COMMAND starts it as.
    pub cargo: String,
    /// `+toolchain`, when COMMAND names one.
    pub toolchain: Option<String>,
    /// `cargo nextest run`, not `cargo test`.
    pub nextest: bool,
    /// The options that choose what Cargo builds, as `cargo test` takes them.
    pub build: Vec<String>,
    /// `--target` triples.
    pub targets: Vec<String>,
    /// COMMAND says something of ignored tests, which runs some that a list doesn't count.
    pub ignored: bool,
}

/// Cargo's build options that take no value.
const FLAGS: &[&str] = &[
    "--workspace",
    "--all",
    "--lib",
    "--bins",
    "--examples",
    "--tests",
    "--benches",
    "--all-targets",
    "--all-features",
    "--no-default-features",
    "--release",
    "-r",
    "--frozen",
    "--locked",
    "--offline",
    "--ignore-rust-version",
];
/// Cargo's build options that take one.
const VALUED: &[&str] = &[
    "-p",
    "--package",
    "--exclude",
    "--bin",
    "--example",
    "--test",
    "--bench",
    "-F",
    "--features",
    "--target",
    "--target-dir",
    "--manifest-path",
    "--config",
    "-Z",
];
/// The options of `cargo nextest run` that take a value and aren't Cargo's: their values are
/// never read as one of Cargo's.
const NEXTEST_VALUED: &[&str] = &[
    "-E",
    "--filterset",
    "--filter-expr",
    "--partition",
    "--retries",
    "--profile",
    "-P",
    "-j",
    "--test-threads",
    "--build-jobs",
    "--run-ignored",
    "--message-format",
    "--message-format-version",
    "--status-level",
    "--final-status-level",
    "--failure-output",
    "--success-output",
    "--max-fail",
    "--tool-config-file",
    "--color",
    "--archive-file",
    "--extract-to",
    "--workspace-remap",
    "--binaries-metadata",
    "--cargo-metadata",
    "--target-dir-remap",
    "--show-progress",
    "--hide-progress-bar",
];

/// What `command` is, when hushrun can read the build it makes: `cargo [+toolchain] nextest run
/// …` or `cargo [+toolchain] test …`.
pub fn recognize(command: &[String]) -> Option<Recognized> {
    let program = command.first()?;
    let file = super::command::program_name(program);
    let file = &*file;
    let mut rest = &command[1..];
    let mut toolchain = None;
    let nextest = match file {
        "cargo" => {
            if let Some(tc) = rest.first().and_then(|w| w.strip_prefix('+')) {
                toolchain = Some(tc.to_string());
                rest = &rest[1..];
            }
            match rest {
                [sub, run, ..] if sub == "nextest" && (run == "run" || run == "r") => {
                    rest = &rest[2..];
                    true
                }
                [sub, ..] if sub == "test" || sub == "t" => {
                    rest = &rest[1..];
                    false
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    let mut build = Vec::new();
    let mut targets = Vec::new();
    let mut ignored = false;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            ignored |= words.any(|w| w.contains("ignored"));
            break;
        }
        ignored |= word.contains("ignored");
        if word == "--doc" || word == "--no-run" {
            // Doctests and builds run no test binary through a runner.
            return None;
        }
        let (name, inline) = match word.split_once('=') {
            Some((name, value)) if name.starts_with('-') => (name, Some(value.to_string())),
            _ => (word.as_str(), None),
        };
        // nextest's `--cargo-profile` is Cargo's `--profile`; its `--profile` is its own.
        let name = match (nextest, name) {
            (true, "--cargo-profile") => "--profile",
            (true, "--profile") => "--nextest-profile",
            (false, "--profile") => "--profile",
            (_, other) => other,
        };
        if FLAGS.contains(&name) {
            build.push(name.to_string());
        } else if VALUED.contains(&name) || name == "--profile" {
            let value = match inline {
                Some(value) => Some(value),
                None => words.next().cloned(),
            };
            let Some(value) = value else { break };
            if name == "--target" {
                targets.push(value.clone());
            }
            build.push(name.to_string());
            build.push(value);
        } else if inline.is_none()
            && (NEXTEST_VALUED.contains(&name) || name == "--nextest-profile")
            && name != "--hide-progress-bar"
        {
            words.next();
        }
    }
    Some(Recognized {
        cargo: program.clone(),
        toolchain,
        nextest,
        build,
        targets,
        ignored,
    })
}

/// The variables that put hushrun's runner in front of every test binary of `recognized`, for
/// each triple it builds for; or why it can't. `exe` is this hushrun, `cwd` where COMMAND runs
/// and `env` a variable's value in COMMAND's environment.
pub fn runner_variables(
    recognized: &Recognized,
    exe: &Path,
    cwd: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    assigned: &[(String, String)],
) -> Result<Vec<(String, String)>, String> {
    let exe_text = exe
        .to_str()
        .filter(|text| !text.chars().any(char::is_whitespace))
        .ok_or("this hushrun's path holds a space, and Cargo splits a runner at spaces")?;
    if let Some(file) = config_with_runner(cwd, env) {
        return Err(format!(
            "{} sets a target runner, which this version doesn't chain to",
            file.display()
        ));
    }
    if recognized.build.iter().any(|word| word.contains("runner")) {
        return Err("COMMAND sets a target runner".into());
    }
    let triples = triples(recognized, cwd, env, assigned)?;
    let mut out = Vec::new();
    for triple in triples {
        let key: String = triple
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        let variable = format!("CARGO_TARGET_{key}_RUNNER");
        let chain: Vec<String> = env(&variable)
            .map(|value| value.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        let mut value = format!("{exe_text} {} {key} {}", runner::SUBCOMMAND, chain.len());
        for word in &chain {
            value.push(' ');
            value.push_str(word);
        }
        out.push((variable, value));
    }
    Ok(out)
}

/// The triples COMMAND builds for: each `--target`, `CARGO_BUILD_TARGET`, and the host's.
fn triples(
    recognized: &Recognized,
    cwd: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    assigned: &[(String, String)],
) -> Result<BTreeSet<String>, String> {
    let mut triples: BTreeSet<String> = recognized.targets.iter().cloned().collect();
    if let Some(target) = env("CARGO_BUILD_TARGET").filter(|t| !t.is_empty()) {
        triples.insert(target);
    }
    triples.insert(host(recognized, cwd, env, assigned)?);
    Ok(triples)
}

/// The `rustc` Cargo runs for COMMAND, as a command and its name: `RUSTC` when set, as it is;
/// else rustup's `rustc` with COMMAND's `+toolchain`. A `+toolchain` goes only to rustup's proxy,
/// never to a `RUSTC` that is a compiler itself.
fn rustc(
    recognized: &Recognized,
    cwd: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    assigned: &[(String, String)],
) -> (Command, String) {
    match env("RUSTC").filter(|r| !r.is_empty()) {
        Some(rustc) => (super::command::tool(&rustc, cwd, assigned), rustc),
        None => {
            let mut command = super::command::tool("rustc", cwd, assigned);
            if let Some(tc) = &recognized.toolchain {
                command.arg(format!("+{tc}"));
            }
            (command, "rustc".into())
        }
    }
}

/// The host triple, as the `rustc` COMMAND's toolchain runs says.
fn host(
    recognized: &Recognized,
    cwd: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    assigned: &[(String, String)],
) -> Result<String, String> {
    let (mut command, rustc) = rustc(recognized, cwd, env, assigned);
    let output = command
        .arg("-vV")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| format!("can't start {rustc}: {err}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|host| host.trim().to_string())
        .ok_or_else(|| format!("{rustc} -vV names no host"))
}

/// A Cargo configuration file that sets a runner, from `cwd` up and in Cargo's home.
fn config_with_runner(cwd: &Path, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = cwd.ancestors().map(|d| d.join(".cargo")).collect();
    let home = env("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")));
    dirs.extend(home);
    for dir in dirs {
        for name in ["config.toml", "config"] {
            let path = dir.join(name);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let sets = text.lines().any(|line| {
                let line = line.trim_start();
                line.strip_prefix("runner")
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
                    || (line.starts_with("target.") && line.contains(".runner"))
            });
            if sets {
                return Some(path);
            }
        }
    }
    None
}

/// What the Rust adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    /// Each test binary of COMMAND's build, by its root file: its facts, what it read and how
    /// long it ran.
    pub units: BTreeMap<String, super::Seen>,
    /// The unit rule: every root file of a test target of the workspace (C-30).
    pub unit_files: BTreeSet<String>,
    /// The files any dep-info of the build names: of these, `.rs` files are sources (C-30).
    pub named: BTreeSet<String>,
    /// \[ADR 0069\] Each test binary's executable, by its unit.
    pub executables: BTreeMap<String, PathBuf>,
}

/// \[ADR 0069\] What a build says of its test binaries before any runs: the plan's.
#[derive(Debug, Default)]
pub struct Built {
    /// Each test binary, by its root file: its executable, what it read, and what can't be told.
    pub units: BTreeMap<String, BuiltUnit>,
    pub unit_files: BTreeSet<String>,
    pub named: BTreeSet<String>,
}

/// One test binary of a build.
#[derive(Debug, Clone)]
pub struct BuiltUnit {
    pub executable: PathBuf,
    pub reads: BTreeSet<String>,
    /// \[ADR 0095\] What the same dep-info says its build read of the environment, as its key
    /// holds it (C-31's `env`).
    pub env: Digest,
    /// \[ADR 0096\] What the build scripts of its closure gave its build, as its key holds it
    /// (C-31's `scripts`).
    pub scripts: Digest,
    pub not_followed: BTreeSet<NotFollowed>,
}

impl BuiltUnit {
    /// What its key holds of what isn't a file of the scope.
    pub fn outside(&self) -> units::Outside {
        units::Outside {
            env: Some(self.env),
            scripts: Some(self.scripts),
        }
    }
}

/// \[ADR 0069\] The plan's build (RFC 0003 §3.9): the build COMMAND makes, made first, so that
/// COMMAND's own finds it done and the binaries' dep-info is there before any test runs. Cargo's
/// progress goes to stderr as COMMAND's would. `Err` when it fails: then nothing is reused, and
/// COMMAND says why. `env` is what COMMAND's compiles get beside hushrun's own environment: the
/// build blocks' wrapper and session (B-2), so that this build, which is COMMAND's, compiles
/// through it too.
pub fn plan_build(
    recognized: &Recognized,
    root: &Path,
    cwd: &Path,
    env: &[(OsString, OsString)],
    assigned: &[(String, String)],
) -> Result<Built, String> {
    // \[ADR 0095\] The build first: the graph `cargo metadata` gives is asked offline, from what
    // the build fetched.
    let compiled = build(recognized, cwd, true, env, assigned)?;
    let metadata = metadata(recognized, cwd, assigned)?;
    let roots = checkout_roots(recognized, root, cwd, &metadata, assigned);
    Ok(read_build(&metadata, compiled, root, roots.as_ref()))
}

#[derive(Debug, Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    /// \[ADR 0095\] Cargo's target folder, absolute.
    #[serde(default)]
    target_directory: Option<PathBuf>,
    /// \[ADR 0095\] Cargo's build folder (`build.build-dir`), absolute; the target folder when
    /// none is set, and absent before Cargo 1.91.
    #[serde(default)]
    build_directory: Option<PathBuf>,
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    /// \[ADR 0095\] The dependency graph, which `--no-deps` leaves out: `None` then.
    #[serde(default)]
    resolve: Option<Resolve>,
}

/// \[ADR 0095\] `cargo metadata`'s dependency graph, by package id.
#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    #[serde(default)]
    deps: Vec<NodeDep>,
}

#[derive(Debug, Deserialize)]
struct NodeDep {
    pkg: String,
    #[serde(default)]
    dep_kinds: Vec<DepKind>,
}

#[derive(Debug, Deserialize)]
struct DepKind {
    #[serde(default)]
    kind: Option<String>,
}

impl Resolve {
    /// \[ADR 0095\] Every package a test binary of `root` may be built against: its own
    /// dependencies of any kind, then theirs other than dev-dependencies, closed, build
    /// dependencies included (more than it links reads more, never less). `None` when the graph
    /// doesn't hold `root`.
    fn closure(&self, root: &str) -> Option<BTreeSet<String>> {
        let nodes: BTreeMap<&str, &Node> = self.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let first = nodes.get(root)?;
        let mut seen = BTreeSet::new();
        let mut pending: Vec<&str> = first.deps.iter().map(|d| d.pkg.as_str()).collect();
        while let Some(next) = pending.pop() {
            if !seen.insert(next.to_string()) {
                continue;
            }
            if let Some(node) = nodes.get(next) {
                pending.extend(
                    node.deps
                        .iter()
                        .filter(|d| {
                            d.dep_kinds.is_empty()
                                || d.dep_kinds.iter().any(|k| k.kind.as_deref() != Some("dev"))
                        })
                        .map(|d| d.pkg.as_str()),
                );
            }
        }
        Some(seen)
    }
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
    targets: Vec<Target>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Target {
    kind: Vec<String>,
    src_path: PathBuf,
    #[serde(default)]
    test: bool,
}

#[derive(Debug, Deserialize)]
struct Message {
    reason: String,
    #[serde(default)]
    package_id: Option<String>,
    #[serde(default)]
    manifest_path: Option<PathBuf>,
    #[serde(default)]
    target: Option<Target>,
    #[serde(default)]
    profile: Option<Profile>,
    #[serde(default)]
    filenames: Vec<PathBuf>,
    #[serde(default)]
    executable: Option<PathBuf>,
    #[serde(default)]
    fresh: bool,
    /// \[ADR 0096\] A build script's run (`build-script-executed`): its `OUT_DIR`, beside which
    /// its `output` is.
    #[serde(default)]
    out_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct Profile {
    test: bool,
}

/// One compiler artifact of a workspace member, as the build named it.
#[derive(Debug)]
struct Artifact {
    manifest: PathBuf,
    /// \[ADR 0095\] Its package's id, as `cargo metadata` names it.
    package_id: Option<String>,
    target: Target,
    test: bool,
    executable: Option<PathBuf>,
    depinfo: Option<PathBuf>,
}

impl Artifact {
    /// A library a binary may link: a `lib`, `rlib`, `proc-macro`, ….
    fn is_lib(&self) -> bool {
        self.target
            .kind
            .iter()
            .any(|k| k.contains("lib") || k == "proc-macro")
    }

    /// \[ADR 0096\] A build script's compile.
    fn is_build_script(&self) -> bool {
        self.target.kind.iter().any(|k| k == "custom-build")
    }
}

/// \[ADR 0096\] One run of a build script, as the build named it.
#[derive(Debug, Clone)]
struct ScriptRun {
    /// Its package's id, as `cargo metadata` names it.
    package_id: String,
    /// Its `OUT_DIR`, as Cargo gave it.
    out_dir: PathBuf,
}

/// What a build named: its compiler artifacts and \[ADR 0096\] its build scripts' runs.
#[derive(Debug, Default)]
struct Compiled {
    artifacts: Vec<Artifact>,
    scripts: Vec<ScriptRun>,
}

/// Reads what the Rust adapter saw: `files`' invocations, and the build `recognized` made, asked
/// of Cargo in `cwd` with hushrun's own environment, which is COMMAND's but for what hushrun
/// added. `root` is the repository's. `command_passed`: COMMAND exited 0. `Err` says why nothing
/// could be read, and then no unit is observed.
pub fn observe(
    recognized: &Recognized,
    files: &[FactsFile],
    root: &Path,
    cwd: &Path,
    command_passed: bool,
    assigned: &[(String, String)],
) -> Result<Observed, String> {
    let invocations: Vec<&Invocation> = files
        .iter()
        .filter(|f| f.adapter == ADAPTER)
        .filter_map(|f| f.invocation.as_ref())
        .collect();
    let metadata = metadata(recognized, cwd, assigned)?;
    let compiled = build(recognized, cwd, false, &[], assigned)?;
    let roots = checkout_roots(recognized, root, cwd, &metadata, assigned);
    let built = read_build(&metadata, compiled, root, roots.as_ref());
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // How each binary the runner started ended, by its real path.
    let mut started: BTreeMap<PathBuf, Vec<&Invocation>> = BTreeMap::new();
    for invocation in &invocations {
        started
            .entry(real(&invocation.binary))
            .or_default()
            .push(invocation);
    }
    let mut units = BTreeMap::new();
    let mut executables = BTreeMap::new();
    for (unit, b) in built.units {
        let facts_of = started.get(&real(&b.executable)).map(Vec::as_slice);
        let starts = facts_of.unwrap_or(&[]);
        let (complete, failed, duration_ms) = ended(starts, recognized, command_passed);
        let outside = b.outside();
        units.insert(
            unit.clone(),
            super::Seen {
                facts: Facts {
                    complete,
                    failed,
                    not_followed: b.not_followed,
                    deselected: deselected(starts),
                    ..Facts::default()
                },
                outside,
                reads: b.reads,
                getenv: BTreeSet::new(),
                duration_ms,
                tests: None,
            },
        );
        executables.insert(unit, b.executable);
    }
    Ok(Observed {
        units,
        unit_files: built.unit_files,
        named: built.named,
        executables,
    })
}

/// \[ADR 0069\] Whether the target runner reused a binary every time it was asked to start it:
/// then none of its tests ran, as the plan said.
fn deselected(starts: &[&Invocation]) -> bool {
    !starts.is_empty() && starts.iter().all(|s| s.reused)
}

/// The test binaries of a build, what each read and what can't be told of it, from its artifacts:
/// a test binary read what its own dep-info names, what the dep-info of every workspace library
/// it may link names, and for an integration test, a bench or an example, what every target of
/// its package was built from (C-31); \[ADR 0096\] and what the build scripts of its package and
/// of those libraries were built from, with what their output says to watch (`rerun-if-changed`).
/// \[ADR 0095\] The same dep-info files, and those of the libraries outside the workspace its
/// package's dependency graph holds (every such library when `metadata` has no graph), say what
/// it read of the environment, its `env`, with `roots` ([`checkout_roots`]) replaced. \[ADR 0096\]
/// The runs of the build scripts of the same packages, and the files of the target or build folder
/// those dep-info files name (what the scripts generated), are what the build scripts gave it, its
/// `scripts` ([`script_entry`], [`generated_entry`]). A dep-info, an output or a generated file
/// that can't be read, or a library without dep-info, makes it `not observed`. The plan and the
/// reading after COMMAND both call it, so both key a binary alike.
fn read_build(
    metadata: &Metadata,
    compiled: Compiled,
    root: &Path,
    roots: Option<&hushrun_build::roots::Roots>,
) -> Built {
    let Compiled { artifacts, scripts } = compiled;
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = real(root);
    let relative = |path: &Path| -> Option<String> {
        let path = real(path);
        let inside = path.strip_prefix(&root).ok()?;
        let parts: Vec<String> = inside
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        (!parts.is_empty()).then(|| parts.join("/"))
    };

    // Workspace members, by manifest, and the members each may link: its path dependencies of
    // any kind, then theirs that aren't dev-dependencies, closed. More than it links reads more,
    // never less.
    let members: BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let packages: BTreeMap<PathBuf, &Package> = metadata
        .packages
        .iter()
        .filter(|p| members.contains(p.id.as_str()))
        .map(|p| (real(&p.manifest_path), p))
        .collect();
    let by_dir: BTreeMap<PathBuf, PathBuf> = packages
        .keys()
        .filter_map(|m| Some((m.parent()?.to_path_buf(), m.clone())))
        .collect();
    let deps_of = |manifest: &Path, with_dev: bool| -> BTreeSet<PathBuf> {
        let Some(package) = packages.get(manifest) else {
            return BTreeSet::new();
        };
        package
            .dependencies
            .iter()
            .filter(|d| with_dev || d.kind.as_deref() != Some("dev"))
            .filter_map(|d| by_dir.get(&real(d.path.as_ref()?)).cloned())
            .collect()
    };
    let linked = |manifest: &Path| -> BTreeSet<PathBuf> {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<PathBuf> = deps_of(manifest, true).into_iter().collect();
        while let Some(next) = pending.pop() {
            if seen.insert(next.clone()) {
                pending.extend(deps_of(&next, false));
            }
        }
        seen
    };

    let mut unit_files = BTreeSet::new();
    for package in packages.values() {
        for target in package.targets.iter().filter(|t| t.test) {
            unit_files.extend(relative(&target.src_path));
        }
    }

    // \[ADR 0095\] The crates outside the workspace a build compiled (registry, git, and path
    // crates outside it), whose libraries a binary may link: what each read of the environment
    // is in the keys of the binaries whose dependency graph holds it, or of every binary when
    // `cargo metadata` gave no graph.
    let (artifacts, outside): (Vec<Artifact>, Vec<Artifact>) = artifacts
        .into_iter()
        .partition(|a| packages.contains_key(&a.manifest));
    let outside: Vec<Artifact> = outside.into_iter().filter(Artifact::is_lib).collect();
    // A graph that doesn't hold every crate the build compiled (one for a platform it was filtered
    // out for, as a Cargo configuration's `build.target` names) can't say what a binary is built
    // against: then every such crate counts for every binary.
    let graph_holds_the_build = metadata.resolve.as_ref().is_some_and(|resolve| {
        let nodes: BTreeSet<&str> = resolve.nodes.iter().map(|n| n.id.as_str()).collect();
        outside
            .iter()
            .all(|a| a.package_id.as_deref().is_some_and(|id| nodes.contains(id)))
    });
    // \[ADR 0096\] The folders Cargo builds in: a file of a dep-info under one is one it made.
    let made_in: Vec<PathBuf> = [&metadata.target_directory, &metadata.build_directory]
        .into_iter()
        .flatten()
        .map(|dir| real(dir))
        .collect();
    let member_ids: BTreeMap<&str, &Package> =
        packages.values().map(|p| (p.id.as_str(), *p)).collect();
    let mut cache: BTreeMap<PathBuf, Option<DepInfo>> = BTreeMap::new();
    let mut outputs: BTreeMap<PathBuf, Option<hushrun_build::script::Script>> = BTreeMap::new();
    // \[ADR 0096\] Each dep-info's files of the build folder with their entries (`None`: unreadable),
    // and each build script run's watched files of the repository, read once a build.
    let mut made: BTreeMap<PathBuf, Vec<(PathBuf, Option<String>)>> = BTreeMap::new();
    let mut watching: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    let mut named = BTreeSet::new();
    let mut read_depinfo = |path: &Path| -> Option<DepInfo> {
        cache
            .entry(path.to_path_buf())
            .or_insert_with(|| parse_depinfo(path, &metadata.workspace_root))
            .clone()
    };
    for artifact in &artifacts {
        if let Some(info) = artifact.depinfo.as_deref().and_then(&mut read_depinfo) {
            named.extend(info.files.iter().filter_map(|p| relative(p)));
        }
    }

    let mut units = BTreeMap::new();
    for artifact in artifacts.iter().filter(|a| a.test) {
        let Some(executable) = &artifact.executable else {
            continue;
        };
        let Some(unit) = relative(&artifact.target.src_path) else {
            continue;
        };
        let mut not_followed = BTreeSet::new();
        let mut depinfos: Vec<PathBuf> = Vec::new();
        match &artifact.depinfo {
            Some(d) => depinfos.push(d.clone()),
            None => {
                not_followed.insert(NotFollowed {
                    why: "no-dep-info".into(),
                    detail: unit.clone(),
                });
            }
        }
        let needed: BTreeSet<PathBuf> = linked(&artifact.manifest);
        let integration = artifact
            .target
            .kind
            .iter()
            .any(|k| matches!(k.as_str(), "test" | "bench" | "example"));
        for other in artifacts.iter().filter(|a| !a.test) {
            let lib = other.is_lib();
            // \[ADR 0096\] The build scripts of its package and of the libraries it may link.
            let script = other.is_build_script();
            let wanted = (needed.contains(&other.manifest) && (lib || script))
                || (other.manifest == artifact.manifest && (lib || script || integration));
            if !wanted {
                continue;
            }
            match &other.depinfo {
                Some(d) => depinfos.push(d.clone()),
                None => {
                    not_followed.insert(NotFollowed {
                        why: "target-without-dep-info".into(),
                        detail: relative(&other.target.src_path).unwrap_or_default(),
                    });
                }
            }
        }
        // \[ADR 0095\] The crates outside the workspace it may be built against: only what they
        // read of the environment counts, since their files are no file of the scope.
        let graph = graph_holds_the_build
            .then_some(metadata.resolve.as_ref())
            .flatten()
            .and_then(|resolve| {
                let id = &packages.get(&artifact.manifest)?.id;
                resolve.closure(id)
            });
        let mut env_only: Vec<PathBuf> = Vec::new();
        for other in &outside {
            let linked = match (&graph, &other.package_id) {
                (Some(graph), Some(id)) => graph.contains(id),
                (Some(_), None) => true,
                (None, _) => true,
            };
            if !linked {
                continue;
            }
            match &other.depinfo {
                Some(d) => env_only.push(d.clone()),
                None => {
                    not_followed.insert(NotFollowed {
                        why: "target-without-dep-info".into(),
                        detail: other
                            .package_id
                            .as_deref()
                            .and_then(|id| id.rsplit('#').next())
                            .unwrap_or_default()
                            .to_string(),
                    });
                }
            }
        }
        let mut reads: BTreeSet<String> = BTreeSet::new();
        let mut env: BTreeSet<(String, Option<String>)> = BTreeSet::new();
        let mut given: BTreeSet<String> = BTreeSet::new();
        for (d, files) in depinfos
            .iter()
            .map(|d| (d, true))
            .chain(env_only.iter().map(|d| (d, false)))
        {
            // \[ADR 0095\] A dep-info that can't be read says nothing of what was read, never
            // that nothing was.
            let Some(info) = read_depinfo(d) else {
                not_followed.insert(NotFollowed {
                    why: "unreadable-dep-info".into(),
                    detail: d
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                });
                continue;
            };
            if files {
                reads.extend(info.files.iter().filter_map(|p| relative(p)));
            }
            env.extend(keyed_env(info.env, roots));
            // \[ADR 0096\] What the build made that the compile read: a file a build script
            // generated, by its content.
            let files = made.entry(d.clone()).or_insert_with(|| {
                info.files
                    .iter()
                    .filter(|f| {
                        let f = real(f);
                        made_in.iter().any(|dir| f.starts_with(dir))
                    })
                    .map(|file| (file.clone(), generated_entry(file, roots)))
                    .collect()
            });
            for (file, entry) in files.iter() {
                match entry {
                    Some(entry) => {
                        given.insert(entry.clone());
                    }
                    None => {
                        not_followed.insert(NotFollowed {
                            why: "unreadable-generated-file".into(),
                            detail: placeholders(file, roots),
                        });
                    }
                }
            }
        }
        // \[ADR 0096\] The runs of the build scripts of its package, of the workspace libraries
        // it may link, and of the crates outside the workspace it may be built against.
        let mine: BTreeSet<&str> = std::iter::once(&artifact.manifest)
            .chain(needed.iter())
            .filter_map(|m| Some(packages.get(m)?.id.as_str()))
            .collect();
        for run in &scripts {
            let member = member_ids.get(run.package_id.as_str());
            let counts = match member {
                Some(_) => mine.contains(run.package_id.as_str()),
                None => graph.as_ref().is_none_or(|g| g.contains(&run.package_id)),
            };
            if !counts {
                continue;
            }
            let output = outputs
                .entry(run.out_dir.clone())
                .or_insert_with(|| script_output(&run.out_dir));
            let Some(output) = output else {
                // Named by its run's folder, `<package>-<hash>`.
                not_followed.insert(NotFollowed {
                    why: "unreadable-build-output".into(),
                    detail: run
                        .out_dir
                        .parent()
                        .and_then(Path::file_name)
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                });
                continue;
            };
            given.insert(script_entry(&run.out_dir, output, roots));
            // What a build script of the workspace says to watch, from its package's folder.
            if let Some(dir) = member.and_then(|p| p.manifest_path.parent()) {
                let watched = watching.entry(run.out_dir.clone()).or_insert_with(|| {
                    output
                        .rerun_if_changed
                        .iter()
                        .flat_map(|watched| watched_files(&dir.join(watched)))
                        .filter_map(|file| relative(&file))
                        .collect()
                });
                reads.extend(watched.iter().cloned());
            }
        }
        units.insert(
            unit,
            BuiltUnit {
                executable: executable.clone(),
                reads,
                env: units::env(env.iter().map(|(n, v)| (n.as_str(), v.as_deref()))),
                scripts: units::scripts(given),
                not_followed,
            },
        );
    }
    Built {
        units,
        unit_files,
        named,
    }
}

/// \[ADR 0096\] One build script's run as C-31's `scripts` holds it: `output `, its `OUT_DIR`, and
/// each line of its output that reaches the compile after a line feed, `roots` replaced in each.
/// Its output is read by build blocks' one reader of it ([`hushrun_build::script`]).
fn script_entry(
    out_dir: &Path,
    output: &hushrun_build::script::Script,
    roots: Option<&hushrun_build::roots::Roots>,
) -> String {
    let mut entry = format!("output {}", placeholders(out_dir, roots));
    for directive in &output.to_the_compile {
        entry.push('\n');
        entry.push_str(&text_placeholders(directive, roots));
    }
    entry
}

/// \[ADR 0096\] A file the build made that a compile read, as C-31's `scripts` holds it: `file `,
/// the SHA-256 of its bytes, a space and its path, `roots` replaced; `None` when it can't be read.
fn generated_entry(file: &Path, roots: Option<&hushrun_build::roots::Roots>) -> Option<String> {
    let bytes = std::fs::read(file).ok()?;
    Some(made_entry(&bytes, file, roots))
}

/// \[ADR 0096\] [`generated_entry`] of a file whose bytes are `bytes`.
fn made_entry(bytes: &[u8], file: &Path, roots: Option<&hushrun_build::roots::Roots>) -> String {
    format!("file {} {}", Digest::of(bytes), placeholders(file, roots))
}

/// \[ADR 0096\] What the build script whose `OUT_DIR` is `out_dir` printed, from the output Cargo
/// saved beside it; `None` when it can't be read.
fn script_output(out_dir: &Path) -> Option<hushrun_build::script::Script> {
    let saved = hushrun_build::script::saved_output(out_dir)?;
    Some(hushrun_build::script::parse_bytes(
        &std::fs::read(saved).ok()?,
    ))
}

/// `path` as text, `roots` replaced.
fn placeholders(path: &Path, roots: Option<&hushrun_build::roots::Roots>) -> String {
    text_placeholders(&path.to_string_lossy(), roots)
}

/// `text`, `roots` replaced: as [`keyed_env`] replaces them in a value.
fn text_placeholders(text: &str, roots: Option<&hushrun_build::roots::Roots>) -> String {
    match roots {
        Some(roots) => roots.to_placeholders(text),
        None => text.to_string(),
    }
}

/// \[ADR 0096\] The files a `rerun-if-changed` path names: the file, or every file below the
/// folder, as Cargo watches them; none when it is gone.
fn watched_files(path: &Path) -> Vec<PathBuf> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    if !meta.is_dir() {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// Whether a test binary ran whole, whether a test of it failed, and how long its tests took,
/// from the runner's starts of it.
///
/// - nextest lists a binary's tests first, then starts it once per test: it ran whole when COMMAND
///   passed, or when every test it listed (ignored ones aside) finished and passed and COMMAND
///   says nothing of ignored tests, which can run some the list doesn't count. Otherwise a failure
///   that stopped the run (nextest stops at the first by default) may have kept some of its tests
///   from running, and it didn't run whole.
/// - `cargo test` starts a binary once, for all its tests: it ran whole when that start ended.
fn ended(
    starts: &[&Invocation],
    recognized: &Recognized,
    command_passed: bool,
) -> (bool, bool, u64) {
    // \[ADR 0069\] What the runner reused ran nothing: only real starts say how it ended.
    let starts: Vec<&Invocation> = starts.iter().copied().filter(|s| !s.reused).collect();
    let starts = starts.as_slice();
    let duration: u64 = starts
        .iter()
        .filter(|s| s.listed.is_none())
        .map(|s| s.duration_ms)
        .fold(0, u64::saturating_add);
    let ok = |s: &Invocation| s.exit == Some(0) && s.signal.is_none();
    let failed = starts.iter().any(|s| !ok(s));
    let lists: Vec<&&Invocation> = starts.iter().filter(|s| s.listed.is_some()).collect();
    if lists.is_empty() {
        let ran = starts.iter().any(|s| s.listed.is_none());
        return (ran, failed, duration);
    }
    if command_passed {
        return (true, failed, duration);
    }
    let listed: BTreeSet<&str> = lists
        .iter()
        .filter(|s| !s.args.iter().any(|a| a == "--ignored"))
        .flat_map(|s| s.listed.iter().flatten().map(String::as_str))
        .collect();
    let passed: BTreeSet<&str> = starts
        .iter()
        .filter(|s| s.listed.is_none() && ok(s))
        .filter_map(|s| {
            let at = s.args.iter().position(|a| a == "--exact")?;
            s.args.get(at + 1).map(String::as_str)
        })
        .collect();
    let whole = !recognized.ignored && listed.iter().all(|test| passed.contains(test));
    (whole, failed, duration)
}

/// `cargo metadata`: the workspace's members, their targets and path dependencies, and \[ADR 0095\]
/// the dependency graph, after COMMAND's build. The graph is asked `--frozen`, so
/// it writes no lockfile and reads no network, and for COMMAND's triples alone
/// (`--filter-platform`), so it needs no package only another platform does: what the build
/// fetched is all it reads. When that fails, at once, `--no-deps` resolves nothing and reads no
/// network either, and every library outside the workspace counts for every binary.
fn metadata(
    recognized: &Recognized,
    cwd: &Path,
    assigned: &[(String, String)],
) -> Result<Metadata, String> {
    // \[ADR 0095\] With the dependency graph, resolved with COMMAND's features, so that each
    // binary's crates outside the workspace are known; without it when that fails, and then every
    // such crate of the build counts for every binary.
    let var = |name: &str| -> Option<String> {
        assigned
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(name).ok())
    };
    triples(recognized, cwd, &var, assigned)
        .and_then(|triples| metadata_of(recognized, cwd, assigned, Some(&triples)))
        .or_else(|_| metadata_of(recognized, cwd, assigned, None))
}

/// `cargo metadata` with the graph for `platforms`, or with `--no-deps` for `None`.
fn metadata_of(
    recognized: &Recognized,
    cwd: &Path,
    assigned: &[(String, String)],
    platforms: Option<&BTreeSet<String>>,
) -> Result<Metadata, String> {
    let resolve = platforms.is_some();
    let mut command = cargo(recognized, cwd, assigned);
    command.args(["metadata", "--format-version", "1"]);
    match platforms {
        None => {
            command.arg("--no-deps");
        }
        Some(platforms) => {
            // Locked and offline, whatever COMMAND says.
            command.arg("--frozen");
            for triple in platforms {
                command.args(["--filter-platform", triple]);
            }
        }
    }
    let mut words = recognized.build.iter();
    let mut given: BTreeSet<&str> = BTreeSet::new();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--manifest-path" | "--config" | "-Z" => {
                command.arg(word);
                command.args(words.next());
            }
            "-F" | "--features" if resolve => {
                command.arg(word);
                command.args(words.next());
            }
            "--frozen" | "--locked" | "--offline" if !resolve => {
                if given.insert(word) {
                    command.arg(word);
                }
            }
            "--all-features" | "--no-default-features" if resolve => {
                command.arg(word);
            }
            _ => {}
        }
    }
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| format!("can't run cargo metadata: {err}"))?;
    if !output.status.success() {
        return Err("cargo metadata failed".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|err| format!("cargo metadata: {err}"))
}

/// \[C-32\] A `cargo` command hushrun asks, as COMMAND's `cargo` will run: in `cwd`, with COMMAND's
/// own `env NAME=value` assignments (`assigned`), and none of hushrun's own variables.
fn cargo(recognized: &Recognized, cwd: &Path, assigned: &[(String, String)]) -> Command {
    let mut command = super::command::tool(&recognized.cargo, cwd, assigned);
    if let Some(tc) = &recognized.toolchain {
        command.arg(format!("+{tc}"));
    }
    command
}

/// The build COMMAND made, as Cargo names it: `cargo test --no-run --message-format=json` with
/// COMMAND's build options and `--locked`. Every artifact must be fresh, or this isn't COMMAND's
/// build: then Cargo is stopped at once, and nothing is read. \[ADR 0069\] For the plan (`plan`),
/// the build COMMAND will make, made now: whatever it builds, with Cargo's progress on stderr.
fn build(
    recognized: &Recognized,
    cwd: &Path,
    plan: bool,
    env: &[(OsString, OsString)],
    assigned: &[(String, String)],
) -> Result<Compiled, String> {
    let mut command = cargo(recognized, cwd, assigned);
    command.envs(env.iter().map(|(name, value)| (name, value)));
    command.args(["test", "--no-run", "--message-format=json"]);
    if !recognized
        .build
        .iter()
        .any(|w| w == "--locked" || w == "--frozen")
    {
        command.arg("--locked");
    }
    command.args(&recognized.build);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if plan {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .spawn()
        .map_err(|err| format!("can't run cargo test --no-run: {err}"))?;
    let started = Instant::now();
    let stdout = child.stdout.take().expect("piped");
    let mut compiled = Compiled::default();
    let mut refused = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if !plan && started.elapsed() > BUILD_BUDGET {
            refused = Some("Cargo took too long to say what it built".to_string());
            break;
        }
        let Ok(message) = serde_json::from_str::<Message>(&line) else {
            continue;
        };
        // \[ADR 0096\] A build script's run, fresh or not: where its output is.
        if message.reason == "build-script-executed" {
            if let (Some(package_id), Some(out_dir)) = (message.package_id, message.out_dir) {
                compiled.scripts.push(ScriptRun {
                    package_id,
                    out_dir,
                });
            }
            continue;
        }
        if message.reason != "compiler-artifact" {
            continue;
        }
        if !plan && !message.fresh {
            refused = Some(
                "Cargo would build something COMMAND didn't, so hushrun can't tell what it built"
                    .to_string(),
            );
            break;
        }
        let (Some(manifest), Some(target), Some(profile)) =
            (message.manifest_path, message.target, message.profile)
        else {
            continue;
        };
        let depinfo = depinfo_of(&message.filenames, message.executable.as_deref(), &target);
        compiled.artifacts.push(Artifact {
            manifest: std::fs::canonicalize(&manifest).unwrap_or(manifest),
            package_id: message.package_id,
            target,
            test: profile.test,
            executable: message.executable,
            depinfo,
        });
    }
    if refused.is_some() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|err| format!("cargo: {err}"))?;
    if let Some(why) = refused {
        return Err(why);
    }
    if !status.success() {
        return Err(if plan {
            "the build failed".into()
        } else {
            "cargo test --no-run failed".into()
        });
    }
    Ok(compiled)
}

/// The rustc dep-info file of one artifact: beside a hashed file in `deps/`; for one Cargo copied
/// out of `deps/` (a binary, an example), the hashed build with the same bytes; \[ADR 0096\] for a
/// build script, `build/<package>-<hash>/build-script-build`, which Cargo copied from the compile
/// beside it, `<crate>-<hash>.d` there (`build_script_build-<hash>.d`). `None` when there is none,
/// and then what it read can't be told.
fn depinfo_of(
    filenames: &[PathBuf],
    executable: Option<&Path>,
    target: &Target,
) -> Option<PathBuf> {
    if target.kind.iter().any(|k| k == "custom-build") {
        return filenames.iter().find_map(|file| {
            let dir = file.parent()?;
            let (_, hash) = dir.file_name()?.to_str()?.rsplit_once('-')?;
            let name = file.file_name()?.to_str()?;
            let name = name.strip_suffix(".exe").unwrap_or(name).replace('-', "_");
            let candidate = dir.join(format!("{name}-{hash}.d"));
            candidate.is_file().then_some(candidate)
        });
    }
    for file in filenames {
        let dir = file.parent()?;
        let name = file.file_name()?.to_str()?;
        if dir.file_name().and_then(|n| n.to_str()) == Some("deps") {
            let mut stem = name;
            for suffix in [
                ".rlib", ".rmeta", ".dylib", ".so", ".dll", ".a", ".exe", ".lib",
            ] {
                if let Some(s) = stem.strip_suffix(suffix) {
                    stem = s.strip_prefix("lib").unwrap_or(s);
                    break;
                }
            }
            let candidate = dir.join(format!("{stem}.d"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let executable = executable?;
    let file_name = executable.file_name()?.to_str()?;
    let name = file_name.strip_suffix(".exe").unwrap_or(file_name);
    let names: BTreeSet<String> = [name.to_string(), name.replace('-', "_")].into();
    let parent = executable.parent()?;
    let bytes = std::fs::read(executable).ok()?;
    let _ = target;
    for dir in [parent.join("deps"), parent.to_path_buf()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|path| {
                let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
                    return false;
                };
                let file = file.strip_suffix(".exe").unwrap_or(file);
                let Some((stem, hash)) = file.rsplit_once('-') else {
                    return false;
                };
                !file.contains('.')
                    && names.contains(stem)
                    && hash.len() == 16
                    && path.with_extension("d").is_file()
            })
            .collect();
        candidates.sort();
        for candidate in candidates {
            if std::fs::read(&candidate).ok().as_deref() == Some(bytes.as_slice()) {
                let mut d = candidate.into_os_string();
                d.push(".d");
                return Some(PathBuf::from(d));
            }
        }
    }
    None
}

/// \[ADR 0095\] The roots a test binary's `env` replaces (C-31), of [`checkout_folders`], each
/// resolved on disk too; the toolchain is the sysroot of the `rustc` COMMAND's toolchain runs.
/// `None` where they can't be roots (a root that isn't a `/` path, as on Windows): values are
/// keyed as they are. The plan and the reading after COMMAND both call it.
fn checkout_roots(
    recognized: &Recognized,
    root: &Path,
    cwd: &Path,
    metadata: &Metadata,
    assigned: &[(String, String)],
) -> Option<hushrun_build::roots::Roots> {
    let var = |name: &str| -> Option<String> {
        assigned
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(name).ok())
    };
    let toolchain = sysroot(recognized, cwd, &var, assigned);
    let folders = checkout_folders(root, cwd, metadata, &var, toolchain)?;
    hushrun_build::roots::Roots::of_checkout(&folders, hushrun_build::roots::resolve_on_disk).ok()
}

/// \[ADR 0095\] The folders of a checkout (C-31), as COMMAND's `cargo` has them (`var`, its
/// assignments over hushrun's environment): the repository's root `root`; Cargo's target and
/// build folders, from `metadata`; the cargo home, `CARGO_HOME` (one that isn't absolute from
/// `cwd`, as Cargo reads it), else `$HOME/.cargo`; the rustup home, `RUSTUP_HOME` likewise, else
/// `$HOME/.rustup`; the toolchain, `toolchain`; and the home folder, `HOME`. An empty variable is
/// unset; a `HOME` that is empty, not absolute or `/` is no folder, and gives no cargo or rustup
/// home of its own.
fn checkout_folders(
    root: &Path,
    cwd: &Path,
    metadata: &Metadata,
    var: &dyn Fn(&str) -> Option<String>,
    toolchain: Option<String>,
) -> Option<hushrun_build::roots::CheckoutFolders> {
    let var = |name: &str| var(name).filter(|v| !v.is_empty());
    let home =
        var("HOME").filter(|home| home.starts_with('/') && !home.trim_matches('/').is_empty());
    let home_of = |name: &str, under: &str| -> Option<String> {
        match var(name) {
            Some(dir) if Path::new(&dir).is_absolute() => Some(dir),
            Some(dir) => cwd.join(dir).to_str().map(str::to_string),
            None => home
                .as_ref()
                .map(|home| format!("{}/{under}", home.trim_end_matches('/'))),
        }
    };
    let text = |dir: &Option<PathBuf>| dir.as_ref().and_then(|d| d.to_str()).map(str::to_string);
    Some(hushrun_build::roots::CheckoutFolders {
        checkout: root.to_str()?.to_string(),
        target_dir: text(&metadata.target_directory),
        build_dir: text(&metadata.build_directory),
        cargo_home: home_of("CARGO_HOME", ".cargo"),
        rustup_home: home_of("RUSTUP_HOME", ".rustup"),
        toolchain,
        home: home.clone(),
    })
}

/// \[ADR 0095\] The sysroot of the `rustc` Cargo runs for COMMAND ([`rustc`]): the toolchain's
/// folder. `None` when it can't be asked, and then there is no toolchain root, silently.
fn sysroot(
    recognized: &Recognized,
    cwd: &Path,
    var: &dyn Fn(&str) -> Option<String>,
    assigned: &[(String, String)],
) -> Option<String> {
    let (mut command, _) = rustc(recognized, cwd, var, assigned);
    let output = command
        .args(["--print", "sysroot"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let line = String::from_utf8(output.stdout).ok()?;
    let line = line.trim();
    (output.status.success() && Path::new(line).is_absolute()).then(|| line.to_string())
}

/// \[ADR 0095\] One dep-info's variables as a key holds them (C-31's `env`): each value with
/// `roots`' placeholders in place of the folders they name; as they are without roots.
fn keyed_env(
    env: BTreeSet<(String, Option<String>)>,
    roots: Option<&hushrun_build::roots::Roots>,
) -> impl Iterator<Item = (String, Option<String>)> + '_ {
    env.into_iter().map(move |(name, value)| {
        let value = match (value, roots) {
            (Some(value), Some(roots)) => Some(roots.to_placeholders(&value)),
            (value, _) => value,
        };
        (name, value)
    })
}

/// What a dep-info file says a compile read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DepInfo {
    /// Every file it names, absolute.
    files: BTreeSet<PathBuf>,
    /// \[ADR 0095\] Every variable it names: each `# env-dep:` line's name and value, `None` for
    /// one that was unset, as rustc wrote it, unescaped.
    env: BTreeSet<(String, Option<String>)>,
}

/// What a Makefile-style dep-info names: every file, absolute, relative ones against `base`; and
/// \[ADR 0095\] every variable of its `# env-dep:` lines, read by the one reader of them
/// ([`hushrun_build::depinfo::env_dep`]). `None` for a file that can't be read, which says nothing
/// of what was read (the unit is then `not observed`); an `env-dep`
/// line with an escape rustc never writes is kept as it is written, as a name alone, so that it
/// still keys the binary.
fn parse_depinfo(path: &Path, base: &Path) -> Option<DepInfo> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut env = BTreeSet::new();
    for line in text.lines() {
        match hushrun_build::depinfo::env_dep(line) {
            Some(Some(dep)) => {
                env.insert((dep.name, dep.value));
            }
            Some(None) => {
                env.insert((line.to_string(), None));
            }
            None => {}
        }
    }
    let text = text.replace("\\\n", " ");
    let mut files = BTreeSet::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = rule_colon(line) else {
            continue;
        };
        for item in split_escaped(&line[colon + 1..]) {
            let item = PathBuf::from(item);
            files.insert(if item.is_absolute() {
                item
            } else {
                base.join(item)
            });
        }
    }
    Some(DepInfo {
        files: files.into_iter().map(|p| normalize(&p)).collect(),
        env,
    })
}

/// `a/b/../c` as `a/c`, without asking the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Where a rule's targets end: the first `:` followed by a space, a tab or the line's end that
/// isn't escaped.
fn rule_colon(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b':' if i + 1 == bytes.len() || matches!(bytes[i + 1], b' ' | b'\t') => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// A rule's prerequisites, split at unescaped spaces.
fn split_escaped(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek().is_some() => current.push(chars.next().unwrap_or('\\')),
            ' ' | '\t' => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// \[ADR 0068\] The Cargo adapter for `cargo test` and `cargo nextest run` (C-32); `None` for any
/// other COMMAND.
pub fn recognized(command: &[String], _: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    Some(Ok(Box::new(recognize(command)?)))
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "cargo"
    }

    fn test_file(&self) -> &'static str {
        "test binary"
    }

    fn hints(&self) -> Hints {
        Hints::None
    }

    fn binds(&self) -> bool {
        true
    }

    fn keys(&self) -> KeysOutside {
        KeysOutside::Build
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let exe = at.exe()?;
        let env = |name: &str| (at.env)(name).and_then(|v| v.into_string().ok());
        runner_variables(self, &exe, at.cwd, &env, at.assigned).map(|vars| {
            vars.into_iter()
                .map(|(name, value)| (name, OsString::from(value)))
                .collect()
        })
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let b = plan_build(self, at.root, at.cwd, at.compile_env, at.assigned)?;
        let sets = units::classify(
            at.tree.paths(),
            &|path| b.unit_files.contains(path),
            Sources::Named(&b.named, SOURCE_KINDS),
            at.ignore,
        );
        let known = b
            .units
            .iter()
            .map(|(unit, b)| {
                let lists = (b.not_followed.is_empty() && at.tree.contains(unit))
                    .then(|| vec![super::built_list(&b.reads, b.outside())]);
                (unit.clone(), lists)
            })
            .collect();
        let bound = b
            .units
            .iter()
            .map(|(unit, b)| (unit.clone(), b.executable.clone()))
            .collect();
        Ok(Some(Planning {
            sets,
            known,
            bound,
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        if !at.files.iter().any(|f| f.adapter == ADAPTER) {
            return Ok(None);
        }
        let observed = observe(
            self,
            at.files,
            at.root,
            at.cwd,
            at.command_passed,
            at.assigned,
        )?;
        let sets = units::classify(
            at.tree.paths(),
            &|path| observed.unit_files.contains(path),
            Sources::Named(&observed.named, SOURCE_KINDS),
            at.ignore,
        );
        let planned = observed
            .units
            .iter()
            .map(|(unit, seen)| {
                (
                    unit.clone(),
                    vec![super::built_list(&seen.reads, seen.outside)],
                )
            })
            .collect();
        Ok(Some(Read {
            seen: observed.units,
            failures_by_unit: true,
            sets,
            planned,
            executables: observed.executables,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        units::classify(
            at.tree.paths(),
            &|p| at.map.units.contains_key(p),
            Sources::Named(&at.map.sources, SOURCE_KINDS),
            at.ignore,
        )
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a test file of {check} as far as this machine knows: no test binary of its last build here has it as its root file"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("its test binaries and what each is built from are those of its last build here")
    }

    fn why_no_path(&self) -> Option<&'static str> {
        Some("its test binary is built from it: rustc's dep-info for the build names it")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-32
    #[test]
    fn commands_that_run_a_workspaces_tests_are_recognized() {
        let r = recognize(&words(
            "cargo nextest run --workspace --locked -E test(foo) --profile ci --cargo-profile dev2 -p a --features=x -j 4 name",
        ))
        .unwrap();
        assert!(r.nextest);
        assert_eq!(
            r.build,
            words("--workspace --locked --profile dev2 -p a --features x")
        );
        let r = recognize(&words(
            "cargo +nightly test --release --target x86_64-unknown-linux-gnu -- --ignored",
        ))
        .unwrap();
        assert!(!r.nextest);
        assert_eq!(r.toolchain.as_deref(), Some("nightly"));
        assert_eq!(r.targets, ["x86_64-unknown-linux-gnu"]);
        assert!(r.ignored);
        assert!(recognize(&words("cargo test --doc")).is_none());
        assert!(recognize(&words("cargo build")).is_none());
        assert!(recognize(&words("make test")).is_none());
        assert!(recognize(&words("cargo nextest list")).is_none());
    }

    // covers: C-32
    #[test]
    fn dep_info_names_every_file_a_build_read() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("a-1.d");
        std::fs::write(
            &d,
            "/t/deps/a-1.d: crates/a/src/lib.rs crates/a/src/../data\\ file.txt /abs/b.rs\n\n/t/deps/a-1: crates/a/src/lib.rs\n\ncrates/a/src/lib.rs:\n\n# env-dep:X=1\n",
        )
        .unwrap();
        let files = parse_depinfo(&d, Path::new("/ws")).unwrap().files;
        let names: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            names,
            [
                "/abs/b.rs",
                "/ws/crates/a/data file.txt",
                "/ws/crates/a/src/lib.rs"
            ]
        );
    }

    fn env_of(text: &str) -> Vec<(String, Option<String>)> {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("a-1.d");
        std::fs::write(&d, text).unwrap();
        parse_depinfo(&d, Path::new("/ws"))
            .unwrap()
            .env
            .into_iter()
            .collect()
    }

    fn entry(name: &str, value: Option<&str>) -> (String, Option<String>) {
        (name.to_string(), value.map(str::to_string))
    }

    // covers: C-31
    #[test]
    fn dep_info_names_every_variable_a_build_read_set_unset_or_escaped() {
        // As rustc 1.94 writes them: a space and `=` as they are, `\`, a line feed and a carriage
        // return escaped as `\\`, `\n` and `\r`.
        let text = "/t/deps/a-1.d: src/lib.rs\n\nsrc/lib.rs:\n\n\
# env-dep:CARGO_MANIFEST_DIR=/ws/crates/a\n\
# env-dep:EMPTY=\n\
# env-dep:UNSET\n\
# env-dep:WEIRD=a b\\\\c\\nd\\re=f\n";
        assert_eq!(
            env_of(text),
            [
                entry("CARGO_MANIFEST_DIR", Some("/ws/crates/a")),
                entry("EMPTY", Some("")),
                entry("UNSET", None),
                entry("WEIRD", Some("a b\\c\nd\re=f")),
            ]
        );
        // Lines that aren't `# env-dep:` lines name no variable: another comment, one without its
        // space or its `#`, one indented, a rule or a path holding the word.
        let others = "/t/deps/a-1.d: src/env-dep:X=1.rs\n\nsrc/env-dep:X=1.rs:\n\n\
# checksum:abc\n#env-dep:Y=1\n env-dep:Z=1\n # env-dep:Z=2\nenv-dep:Q=3:\n";
        assert_eq!(env_of(others), []);
        // An escape rustc never writes keys the line as it is, so it still tells builds apart.
        assert_eq!(
            env_of("/t/a.d: src/lib.rs\n\n# env-dep:BAD=a\\qb\n"),
            [entry("# env-dep:BAD=a\\qb", None)]
        );
        // A dep-info that can't be read says nothing, not that nothing was read.
        assert!(parse_depinfo(Path::new("/nowhere/a.d"), Path::new("/ws")).is_none());
    }

    // covers: C-31
    // \[ADR 0095\] The Cargo vectors' `env-dep` lines, read and replaced as a build's are, give the
    // entries the vectors list, which `tools/refdigest.py` and hushrun-core's test key.
    #[test]
    fn the_cargo_vectors_lines_give_the_entries_they_list() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/digest/unit-keys.json");
        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let (mut seen, mut scripted) = (0, 0);
        for vector in data["vectors"].as_array().unwrap() {
            let Some(deps) = vector.get("envDeps") else {
                continue;
            };
            seen += 1;
            let lines: Vec<&str> = deps["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap())
                .collect();
            let folder = |name: &str| deps["folders"][name].as_str().map(str::to_string);
            // The vectors' folders resolve to themselves, as on a machine without symlinks there.
            let roots = hushrun_build::roots::Roots::of_checkout(
                &hushrun_build::roots::CheckoutFolders {
                    checkout: folder("root").unwrap(),
                    target_dir: folder("targetDir"),
                    build_dir: folder("buildDir"),
                    cargo_home: folder("cargoHome"),
                    toolchain: folder("toolchain"),
                    rustup_home: folder("rustupHome"),
                    home: folder("home"),
                },
                |_: &str| None,
            )
            .unwrap();
            let entries: Vec<(String, Option<String>)> = keyed_env(
                env_of(&format!("/t/a.d: src/lib.rs\n\n{}\n", lines.join("\n")))
                    .into_iter()
                    .collect(),
                Some(&roots),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
            let listed: Vec<(String, Option<String>)> = vector["env"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    (
                        e[0].as_str().unwrap().to_string(),
                        e[1].as_str().map(str::to_string),
                    )
                })
                .collect();
            assert_eq!(entries, listed, "{}", vector["name"]);
            assert_eq!(
                units::env(entries.iter().map(|(n, v)| (n.as_str(), v.as_deref()))).to_string(),
                vector["expect"]["env"],
                "{}",
                vector["name"]
            );
            // \[ADR 0096\] Its build scripts' runs, and the files of its build folder its dep-info
            // names, give the entries of `scripts` it lists.
            let Some(build) = vector.get("buildScripts") else {
                continue;
            };
            assert_eq!(build["folders"], deps["folders"], "{}", vector["name"]);
            let mut given: BTreeSet<String> = BTreeSet::new();
            for run in build["runs"].as_array().unwrap() {
                let output = hushrun_build::script::parse(run["output"].as_str().unwrap());
                let out_dir = Path::new(run["outDir"].as_str().unwrap());
                given.insert(script_entry(out_dir, &output, Some(&roots)));
            }
            let made_in: Vec<String> = ["targetDir", "buildDir"]
                .into_iter()
                .filter_map(folder)
                .collect();
            for named in build["depInfo"].as_array().unwrap() {
                let path = Path::new(named["path"].as_str().unwrap());
                if made_in.iter().any(|dir| path.starts_with(dir)) {
                    let text = named["text"].as_str().unwrap();
                    given.insert(made_entry(text.as_bytes(), path, Some(&roots)));
                }
            }
            let listed: BTreeSet<String> = vector["scripts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e.as_str().unwrap().to_string())
                .collect();
            assert_eq!(given, listed, "{}", vector["name"]);
            scripted += 1;
        }
        assert_eq!(seen, 6);
        assert_eq!(scripted, 2);
    }

    // covers: C-31
    #[test]
    fn a_binarys_env_is_its_closures_with_the_checkouts_folders_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("ws");
        let ws = root.to_str().unwrap().to_string();
        let target = format!("{ws}/target");
        let deps = root.join("target/debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        let depinfo = |name: &str, env: &[&str]| -> PathBuf {
            let path = deps.join(format!("{name}.d"));
            let mut text = format!("{}: x.rs\n\nx.rs:\n\n", path.display());
            for line in env {
                text.push_str(&format!("# env-dep:{line}\n"));
            }
            std::fs::write(&path, text).unwrap();
            path
        };
        let graph = serde_json::json!({"nodes": [
            {"id": "a", "deps": [{"pkg": "ext", "dep_kinds": [{"kind": null}]}]},
            {"id": "b", "deps": [{"pkg": "a", "dep_kinds": [{"kind": null}]}]},
            {"id": "ext", "deps": [
                {"pkg": "far", "dep_kinds": [{"kind": "build"}]},
                {"pkg": "dev-only", "dep_kinds": [{"kind": "dev"}]}
            ]},
            {"id": "far", "deps": []},
            {"id": "dev-only", "deps": []},
            {"id": "lone", "deps": []}
        ]});
        let metadata_with = |resolve: serde_json::Value| -> Metadata {
            serde_json::from_value(serde_json::json!({
                "workspace_root": ws,
                "target_directory": target,
                "workspace_members": ["a", "b"],
                "packages": [
                    {"id": "a", "manifest_path": format!("{ws}/a/Cargo.toml"), "dependencies": [],
                     "targets": [{"kind": ["lib"], "src_path": format!("{ws}/a/src/lib.rs"), "test": true}]},
                    {"id": "b", "manifest_path": format!("{ws}/b/Cargo.toml"),
                     "dependencies": [{"path": format!("{ws}/a"), "kind": null}],
                     "targets": [
                         {"kind": ["lib"], "src_path": format!("{ws}/b/src/lib.rs"), "test": true},
                         {"kind": ["test"], "src_path": format!("{ws}/b/tests/it.rs"), "test": true}
                     ]}
                ],
                "resolve": resolve,
            }))
            .unwrap()
        };
        let target_of = |package: &str, kind: &str, src: &str| Target {
            kind: vec![kind.to_string()],
            src_path: root.join(package).join(src),
            test: true,
        };
        let artifact =
            |package: &str, kind: &str, src: &str, test: bool, d: Option<PathBuf>| Artifact {
                manifest: root.join(package).join("Cargo.toml"),
                package_id: Some(package.to_string()),
                target: target_of(package, kind, src),
                test,
                executable: test.then(|| deps.join(format!("{package}-{kind}"))),
                depinfo: d,
            };
        // A crate outside the workspace: its manifest is no member's.
        let outside = |package: &str, kind: &str, d: Option<PathBuf>| Artifact {
            manifest: root.join("..").join(package).join("Cargo.toml"),
            ..artifact(package, kind, "src/lib.rs", false, d)
        };
        let a_dir = format!("CARGO_MANIFEST_DIR={ws}/a");
        let b_dir = format!("CARGO_MANIFEST_DIR={ws}/b");
        let out = format!("OUT_DIR={target}/debug/build/b-1/out");
        let artifacts = || {
            vec![
                artifact(
                    "a",
                    "lib",
                    "src/lib.rs",
                    false,
                    Some(depinfo("a", &[&a_dir, "A_ONLY"])),
                ),
                artifact(
                    "a",
                    "lib",
                    "src/lib.rs",
                    true,
                    Some(depinfo("a-test", &[&a_dir])),
                ),
                artifact(
                    "b",
                    "lib",
                    "src/lib.rs",
                    false,
                    Some(depinfo("b", &[&b_dir, &out])),
                ),
                artifact(
                    "b",
                    "lib",
                    "src/lib.rs",
                    true,
                    Some(depinfo("b-test", &[&b_dir])),
                ),
                artifact(
                    "b",
                    "test",
                    "tests/it.rs",
                    true,
                    Some(depinfo("it", &["IT=1"])),
                ),
                outside("ext", "lib", Some(depinfo("ext", &["EXT=1"]))),
                // A build script's own compile isn't linked, and has no dep-info found.
                outside("ext", "custom-build", None),
                outside("far", "proc-macro", Some(depinfo("far", &["FAR"]))),
                outside("dev-only", "lib", Some(depinfo("dev-only", &["DEV=1"]))),
                outside("lone", "lib", Some(depinfo("lone", &["LONE=1"]))),
            ]
        };
        let roots = hushrun_build::roots::Roots::of_checkout(
            &hushrun_build::roots::CheckoutFolders {
                checkout: ws.clone(),
                target_dir: Some(target.clone()),
                ..Default::default()
            },
            |_: &str| None,
        )
        .unwrap();
        let compiled = |artifacts: Vec<Artifact>| Compiled {
            artifacts,
            scripts: Vec::new(),
        };
        let built = read_build(
            &metadata_with(graph.clone()),
            compiled(artifacts()),
            &root,
            Some(&roots),
        );
        let env = |entries: &[(&str, Option<&str>)]| units::env(entries.iter().copied());
        let a = ("CARGO_MANIFEST_DIR", Some("{hushrun:workspace}/a"));
        let b = ("CARGO_MANIFEST_DIR", Some("{hushrun:workspace}/b"));
        let out = ("OUT_DIR", Some("{hushrun:target-dir}/debug/build/b-1/out"));
        // The crates outside the workspace its graph holds: a dependency's own dependencies of
        // any kind but dev.
        let outer = [("EXT", Some("1")), ("FAR", None)];
        let with = |entries: &[(&'static str, Option<&'static str>)]| {
            let mut all = entries.to_vec();
            all.extend(outer);
            env(&all)
        };
        // A unit test reads its own dep-info's, its package's library's and each workspace
        // library's it may link.
        assert_eq!(
            built.units["a/src/lib.rs"].env,
            with(&[a, ("A_ONLY", None)])
        );
        assert_eq!(
            built.units["b/src/lib.rs"].env,
            with(&[b, out, a, ("A_ONLY", None)])
        );
        assert_ne!(
            built.units["b/src/lib.rs"].env,
            with(&[b, a, ("A_ONLY", None)])
        );
        // An integration test, every target's of its package too.
        assert_eq!(
            built.units["b/tests/it.rs"].env,
            with(&[("IT", Some("1")), b, out, a, ("A_ONLY", None)])
        );
        assert!(built.units.values().all(|u| u.not_followed.is_empty()));
        // Without the graph, every crate outside the workspace counts for every binary.
        let built = read_build(
            &metadata_with(serde_json::Value::Null),
            compiled(artifacts()),
            &root,
            Some(&roots),
        );
        let mut all = vec![a, ("A_ONLY", None), ("DEV", Some("1")), ("LONE", Some("1"))];
        all.extend(outer);
        assert_eq!(built.units["a/src/lib.rs"].env, env(&all));
        // A dep-info that can't be read keeps the binaries it is in from being followed; a
        // library outside the workspace without one too.
        let mut broken = artifacts();
        std::fs::write(deps.join("ext.d"), [0xff, 0xfe, b'\n']).unwrap();
        broken.push(outside("far", "lib", None));
        let built = read_build(&metadata_with(graph), compiled(broken), &root, Some(&roots));
        let whys = |unit: &str| -> BTreeSet<String> {
            built.units[unit]
                .not_followed
                .iter()
                .map(|n| format!("{} {}", n.why, n.detail))
                .collect()
        };
        assert_eq!(
            whys("a/src/lib.rs"),
            BTreeSet::from([
                "target-without-dep-info far".to_string(),
                "unreadable-dep-info ext.d".to_string()
            ])
        );
        assert_eq!(whys("b/tests/it.rs"), whys("a/src/lib.rs"));
    }

    // covers: C-31
    // \[ADR 0096\] A build script's run: its `OUT_DIR`, then the lines of its output that reach
    // the compile, in the order written, roots replaced.
    #[test]
    fn a_build_scripts_run_is_its_out_dir_and_what_reaches_the_compile_in_order() {
        let output = hushrun_build::script::parse(
            "cargo:rustc-link-lib=static=z\ncargo::rustc-cfg=loud\ncargo:warning=hello\ncargo:rerun-if-changed=build.rs\ncargo:rustc-link-search=native=/w/target/debug/build/a-1/out\n",
        );
        let roots = hushrun_build::roots::Roots::of_checkout(
            &hushrun_build::roots::CheckoutFolders {
                checkout: "/w".into(),
                target_dir: Some("/w/target".into()),
                ..Default::default()
            },
            |_: &str| None,
        )
        .unwrap();
        assert_eq!(
            script_entry(
                Path::new("/w/target/debug/build/a-1/out"),
                &output,
                Some(&roots)
            ),
            "output {hushrun:target-dir}/debug/build/a-1/out\nrustc-link-lib=static=z\nrustc-cfg=loud\nrustc-link-search=native={hushrun:target-dir}/debug/build/a-1/out"
        );
        // Lines in another order link another way: another entry.
        let swapped = hushrun_build::script::Script {
            to_the_compile: output.to_the_compile.iter().rev().cloned().collect(),
            ..output.clone()
        };
        assert_ne!(
            script_entry(Path::new("/o"), &swapped, None),
            script_entry(Path::new("/o"), &output, None)
        );
        assert_eq!(
            script_entry(Path::new("/o"), &output, None).lines().count(),
            4
        );
    }

    // covers: C-30, C-31
    // \[ADR 0096\] A build script's dep-info is found beside the copy Cargo names, in its own
    // folder of `build/`.
    #[test]
    fn a_build_scripts_dep_info_is_beside_its_compile() {
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("target/debug/build/gen-0123456789abcdef");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("build-script-build"), "x").unwrap();
        std::fs::write(build.join("build_script_build-0123456789abcdef"), "x").unwrap();
        let target = |kind: &str| Target {
            kind: vec![kind.to_string()],
            src_path: dir.path().join("build.rs"),
            test: false,
        };
        let filenames = [build.join("build-script-build")];
        assert_eq!(depinfo_of(&filenames, None, &target("custom-build")), None);
        std::fs::write(
            build.join("build_script_build-0123456789abcdef.d"),
            "x: build.rs\n",
        )
        .unwrap();
        assert_eq!(
            depinfo_of(&filenames, None, &target("custom-build")),
            Some(build.join("build_script_build-0123456789abcdef.d"))
        );
        assert_eq!(depinfo_of(&filenames, None, &target("lib")), None);
    }

    // covers: C-30, C-31
    // \[ADR 0096\] A binary's `scripts` are the runs of the build scripts of its package, of the
    // workspace libraries it may link and of the crates outside the workspace its graph holds,
    // and the files of the build folder its closure's dep-info names, by content; its reads hold
    // its closure's build scripts and what they watch. What can't be read leaves it unfollowed.
    #[test]
    fn a_binarys_scripts_are_its_closures_build_script_runs_and_the_files_they_made() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("ws");
        let ws = root.to_str().unwrap().to_string();
        let target = format!("{ws}/target");
        let deps = root.join("target/debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::create_dir_all(root.join("a/data/deep")).unwrap();
        std::fs::write(root.join("a/data/deep/x.txt"), "x").unwrap();
        // Each package's build script's run: its output, and what it made in OUT_DIR.
        let run = |package: &str, output: &str| -> ScriptRun {
            let out = root.join(format!("target/debug/build/{package}-1/out"));
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.parent().unwrap().join("output"), output).unwrap();
            ScriptRun {
                package_id: package.to_string(),
                out_dir: out,
            }
        };
        let made = root.join("target/debug/build/b-1/out/gen.rs");
        let depinfo = |name: &str, files: &[String]| -> PathBuf {
            let path = deps.join(format!("{name}.d"));
            let list = files.join(" ");
            std::fs::write(&path, format!("{}: {list}\n", path.display())).unwrap();
            path
        };
        let metadata: Metadata = serde_json::from_value(serde_json::json!({
            "workspace_root": ws,
            "target_directory": target,
            "workspace_members": ["a", "b", "c"],
            "packages": [
                {"id": "a", "manifest_path": format!("{ws}/a/Cargo.toml"), "dependencies": [],
                 "targets": [{"kind": ["lib"], "src_path": format!("{ws}/a/src/lib.rs"), "test": true}]},
                {"id": "b", "manifest_path": format!("{ws}/b/Cargo.toml"),
                 "dependencies": [{"path": format!("{ws}/a"), "kind": null}],
                 "targets": [{"kind": ["lib"], "src_path": format!("{ws}/b/src/lib.rs"), "test": true}]},
                {"id": "c", "manifest_path": format!("{ws}/c/Cargo.toml"), "dependencies": [],
                 "targets": [{"kind": ["lib"], "src_path": format!("{ws}/c/src/lib.rs"), "test": true}]}
            ],
            "resolve": {"nodes": [
                {"id": "a", "deps": []},
                {"id": "b", "deps": [
                    {"pkg": "a", "dep_kinds": [{"kind": null}]},
                    {"pkg": "ext", "dep_kinds": [{"kind": null}]}
                ]},
                {"id": "c", "deps": []},
                {"id": "ext", "deps": []}
            ]},
        }))
        .unwrap();
        let artifact = |package: &str, kind: &str, src: &str, test: bool, d: PathBuf| Artifact {
            manifest: root.join(package).join("Cargo.toml"),
            package_id: Some(package.to_string()),
            target: Target {
                kind: vec![kind.to_string()],
                src_path: root.join(package).join(src),
                test: true,
            },
            test,
            executable: test.then(|| deps.join(format!("{package}-test"))),
            depinfo: Some(d),
        };
        let compiled = || Compiled {
            artifacts: vec![
                artifact(
                    "a",
                    "lib",
                    "src/lib.rs",
                    false,
                    depinfo("a", &[format!("{ws}/a/src/lib.rs")]),
                ),
                artifact(
                    "a",
                    "lib",
                    "src/lib.rs",
                    true,
                    depinfo("a-test", &[format!("{ws}/a/src/lib.rs")]),
                ),
                artifact(
                    "a",
                    "custom-build",
                    "build.rs",
                    false,
                    depinfo("a-build", &[format!("{ws}/a/build.rs")]),
                ),
                artifact(
                    "b",
                    "lib",
                    "src/lib.rs",
                    false,
                    depinfo(
                        "b",
                        &[format!("{ws}/b/src/lib.rs"), made.display().to_string()],
                    ),
                ),
                artifact(
                    "b",
                    "lib",
                    "src/lib.rs",
                    true,
                    depinfo("b-test", &[format!("{ws}/b/src/lib.rs")]),
                ),
                artifact(
                    "b",
                    "custom-build",
                    "build.rs",
                    false,
                    depinfo("b-build", &[format!("{ws}/b/build.rs")]),
                ),
                artifact(
                    "c",
                    "lib",
                    "src/lib.rs",
                    false,
                    depinfo("c", &[format!("{ws}/c/src/lib.rs")]),
                ),
                artifact(
                    "c",
                    "lib",
                    "src/lib.rs",
                    true,
                    depinfo("c-test", &[format!("{ws}/c/src/lib.rs")]),
                ),
                Artifact {
                    manifest: root.join("../ext/Cargo.toml"),
                    ..artifact("ext", "lib", "src/lib.rs", false, depinfo("ext", &[]))
                },
            ],
            scripts: vec![
                run(
                    "a",
                    &format!(
                        "cargo::rustc-cfg=loud\ncargo:warning=w\ncargo:rerun-if-changed=data\ncargo:rustc-link-search=native={target}/debug/build/a-1/out\n"
                    ),
                ),
                run("b", "cargo:rerun-if-changed=build.rs\n"),
                run("ext", "cargo:rustc-cfg=ext\n"),
            ],
        };
        std::fs::create_dir_all(made.parent().unwrap()).unwrap();
        std::fs::write(&made, "pub const G: &str = \"plain\";\n").unwrap();
        let roots = hushrun_build::roots::Roots::of_checkout(
            &hushrun_build::roots::CheckoutFolders {
                checkout: ws.clone(),
                target_dir: Some(target.clone()),
                ..Default::default()
            },
            |_: &str| None,
        )
        .unwrap();
        let built = read_build(&metadata, compiled(), &root, Some(&roots));
        let out =
            |package: &str| format!("output {{hushrun:target-dir}}/debug/build/{package}-1/out");
        let a_run = format!(
            "{}\nrustc-cfg=loud\nrustc-link-search=native={{hushrun:target-dir}}/debug/build/a-1/out",
            out("a")
        );
        let gen_entry = |text: &str| {
            format!(
                "file {} {{hushrun:target-dir}}/debug/build/b-1/out/gen.rs",
                Digest::of(text.as_bytes())
            )
        };
        assert_eq!(
            built.units["a/src/lib.rs"].scripts,
            units::scripts([a_run.clone()])
        );
        assert_eq!(
            built.units["b/src/lib.rs"].scripts,
            units::scripts([
                a_run.clone(),
                out("b"),
                out("ext") + "\nrustc-cfg=ext",
                gen_entry("pub const G: &str = \"plain\";\n"),
            ])
        );
        // No build script in its closure: none, and so not the crate outside its graph.
        assert_eq!(built.units["c/src/lib.rs"].scripts, units::scripts([]));
        // Its closure's build scripts, and what they watch, are files it read.
        let reads =
            |unit: &str| -> Vec<String> { built.units[unit].reads.iter().cloned().collect() };
        assert_eq!(
            reads("a/src/lib.rs"),
            ["a/build.rs", "a/data/deep/x.txt", "a/src/lib.rs"]
        );
        assert_eq!(
            reads("b/src/lib.rs"),
            [
                "a/build.rs",
                "a/data/deep/x.txt",
                "a/src/lib.rs",
                "b/build.rs",
                "b/src/lib.rs",
                "target/debug/build/b-1/out/gen.rs"
            ]
        );
        assert_eq!(reads("c/src/lib.rs"), ["c/src/lib.rs"]);
        assert!(built.named.contains("a/build.rs"));
        assert!(built.units.values().all(|u| u.not_followed.is_empty()));

        // Another generated file moves the binaries that read it alone.
        std::fs::write(&made, "pub const G: &str = \"spicy\";\n").unwrap();
        let again = read_build(&metadata, compiled(), &root, Some(&roots));
        assert_ne!(
            again.units["b/src/lib.rs"].scripts,
            built.units["b/src/lib.rs"].scripts
        );
        assert_eq!(
            again.units["a/src/lib.rs"].scripts,
            built.units["a/src/lib.rs"].scripts
        );

        // An output or a generated file that can't be read: not followed.
        let broken = compiled();
        std::fs::remove_file(root.join("target/debug/build/b-1/output")).unwrap();
        std::fs::remove_file(&made).unwrap();
        let built = read_build(&metadata, broken, &root, Some(&roots));
        let whys: BTreeSet<String> = built.units["b/src/lib.rs"]
            .not_followed
            .iter()
            .map(|n| format!("{} {}", n.why, n.detail))
            .collect();
        assert_eq!(
            whys,
            BTreeSet::from([
                "unreadable-build-output b-1".to_string(),
                "unreadable-generated-file {hushrun:target-dir}/debug/build/b-1/out/gen.rs"
                    .to_string(),
            ])
        );
        assert!(built.units["a/src/lib.rs"].not_followed.is_empty());
    }

    // covers: C-31, C-32
    // \[ADR 0095\] The `rustc` Cargo runs for COMMAND: `RUSTC` as it is, never given COMMAND's
    // `+toolchain`, which only rustup's proxy reads.
    #[test]
    fn rustc_is_cargos_and_a_toolchain_goes_only_to_rustups_proxy() {
        let recognized = recognize(&words("cargo +1.94.0 nextest run")).unwrap();
        let args = |command: &Command| -> Vec<String> {
            std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let set = |name: &str| (name == "RUSTC").then(|| "/opt/rust/bin/rustc".to_string());
        let (command, name) = rustc(&recognized, Path::new("/"), &set, &[]);
        assert_eq!(name, "/opt/rust/bin/rustc");
        assert_eq!(args(&command), ["/opt/rust/bin/rustc"]);
        let unset = |_: &str| None;
        let (command, name) = rustc(&recognized, Path::new("/"), &unset, &[]);
        assert_eq!(name, "rustc");
        assert_eq!(args(&command), ["rustc", "+1.94.0"]);
        let empty = |name: &str| (name == "RUSTC").then(String::new);
        assert_eq!(
            args(&rustc(&recognized, Path::new("/"), &empty, &[]).0),
            ["rustc", "+1.94.0"]
        );
    }

    // covers: C-31
    #[test]
    fn a_checkouts_folders_are_cargos_own() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        let metadata: Metadata = serde_json::from_value(serde_json::json!({
            "workspace_root": root, "target_directory": root.join("target"),
            "build_directory": "/mnt/build", "workspace_members": [], "packages": [],
        }))
        .unwrap();
        let folders = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect();
            let var =
                move |name: &str| vars.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
            checkout_folders(
                &root,
                Path::new("/cwd"),
                &metadata,
                &var,
                Some("/tc".into()),
            )
            .unwrap()
        };
        let set = folders(&[
            ("HOME", "/home/dev"),
            ("CARGO_HOME", "/opt/cargo"),
            ("RUSTUP_HOME", "rustup"),
        ]);
        assert_eq!(set.checkout, root.to_str().unwrap());
        assert_eq!(set.target_dir.as_deref(), root.join("target").to_str());
        assert_eq!(set.build_dir.as_deref(), Some("/mnt/build"));
        assert_eq!(set.cargo_home.as_deref(), Some("/opt/cargo"));
        // One that isn't absolute is from where COMMAND's cargo runs, as Cargo reads it.
        assert_eq!(set.rustup_home.as_deref(), Some("/cwd/rustup"));
        assert_eq!(set.toolchain.as_deref(), Some("/tc"));
        assert_eq!(set.home.as_deref(), Some("/home/dev"));
        // Unset: under the home folder.
        let unset = folders(&[("HOME", "/home/dev/"), ("CARGO_HOME", "")]);
        assert_eq!(unset.cargo_home.as_deref(), Some("/home/dev/.cargo"));
        assert_eq!(unset.rustup_home.as_deref(), Some("/home/dev/.rustup"));
        // A home folder that is empty, not absolute or `/` is none, and gives no other folder.
        for home in ["", "home/dev", "/", "//"] {
            let none = folders(&[("HOME", home)]);
            assert_eq!(
                (none.home, none.cargo_home, none.rustup_home),
                (None, None, None),
                "{home:?}"
            );
        }
    }

    fn start(args: &str, listed: Option<&[&str]>, exit: i32) -> Invocation {
        Invocation {
            binary: "/t/deps/a-1".into(),
            args: words(args),
            listed: listed.map(|l| l.iter().map(|s| s.to_string()).collect()),
            exit: Some(exit),
            signal: None,
            duration_ms: 5,
            reused: false,
            dir: None,
            testlog: None,
            testlog_truncated: false,
        }
    }

    // covers: C-32
    #[test]
    fn a_binary_ran_whole_when_every_test_it_listed_passed() {
        let nextest = recognize(&words("cargo nextest run")).unwrap();
        let list = start("--list --format terse", Some(&["x", "y"]), 0);
        let ignored = start("--list --format terse --ignored", Some(&["slow"]), 0);
        let x = start("--exact x --nocapture", None, 0);
        let y = start("--exact y --nocapture", None, 0);
        let y_failed = start("--exact y --nocapture", None, 101);
        // COMMAND passed: whatever nextest chose ran.
        assert_eq!(
            ended(&[&list, &ignored, &x], &nextest, true),
            (true, false, 5)
        );
        // COMMAND failed elsewhere: all it listed passed, the ignored one aside.
        assert_eq!(
            ended(&[&list, &ignored, &x, &y], &nextest, false),
            (true, false, 10)
        );
        // A failure that stopped the run before y started: not whole.
        assert_eq!(
            ended(&[&list, &ignored, &x], &nextest, false),
            (false, false, 5)
        );
        // A failed test: failed, whatever else (C-32 decides it so).
        assert_eq!(
            ended(&[&list, &x, &y_failed], &nextest, false),
            (false, true, 10)
        );
        // Listed and never started, as fail-fast leaves it.
        assert!(!ended(&[&list], &nextest, false).0);
        // COMMAND names ignored tests: a list doesn't say what runs.
        let with_ignored = recognize(&words("cargo nextest run --run-ignored all")).unwrap();
        assert!(!ended(&[&list, &x, &y], &with_ignored, false).0);
        // cargo test: one start for all its tests.
        let plain = recognize(&words("cargo test")).unwrap();
        let whole = start("", None, 0);
        assert_eq!(ended(&[&whole], &plain, false), (true, false, 5));
        assert_eq!(ended(&[], &plain, false), (false, false, 0));
        let failed = start("", None, 101);
        assert_eq!(ended(&[&failed], &plain, false), (true, true, 5));
        // \[ADR 0069\] What the runner reused ran nothing: a binary every start of which it
        // reused is deselected, and a reused start says nothing of how one ran.
        let reused_list = Invocation {
            reused: true,
            ..start("--list --format terse", Some(&[]), 0)
        };
        assert!(deselected(&[&reused_list, &reused_list]));
        assert!(!deselected(&[&reused_list, &x]));
        assert!(!deselected(&[]));
        assert_eq!(ended(&[&reused_list], &plain, false), (false, false, 0));
    }
}
