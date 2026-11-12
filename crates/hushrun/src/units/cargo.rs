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
//!   of every target of its package, since it may run them (the package rule).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use hushrun_core::units::{self, Facts, NotFollowed, Sets, Sources};

use super::adapter::{Adapter, Hints, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt};
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
    let file = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let file = file.strip_suffix(".exe").unwrap_or(file);
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
    let mut triples: BTreeSet<String> = recognized.targets.iter().cloned().collect();
    if let Some(target) = env("CARGO_BUILD_TARGET").filter(|t| !t.is_empty()) {
        triples.insert(target);
    }
    triples.insert(host(recognized, env)?);
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

/// The host triple, as the `rustc` COMMAND's toolchain runs says.
fn host(recognized: &Recognized, env: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let rustc = env("RUSTC")
        .filter(|r| !r.is_empty())
        .unwrap_or("rustc".into());
    let mut command = Command::new(&rustc);
    if let Some(tc) = &recognized.toolchain {
        command.arg(format!("+{tc}"));
    }
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
    pub not_followed: BTreeSet<NotFollowed>,
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
) -> Result<Built, String> {
    let metadata = metadata(recognized, cwd)?;
    let artifacts = build(recognized, cwd, true, env)?;
    Ok(read_build(&metadata, artifacts, root))
}

#[derive(Debug, Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    packages: Vec<Package>,
    workspace_members: Vec<String>,
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
}

#[derive(Debug, Deserialize)]
struct Profile {
    test: bool,
}

/// One compiler artifact of a workspace member, as the build named it.
#[derive(Debug)]
struct Artifact {
    manifest: PathBuf,
    target: Target,
    test: bool,
    executable: Option<PathBuf>,
    depinfo: Option<PathBuf>,
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
) -> Result<Observed, String> {
    let invocations: Vec<&Invocation> = files
        .iter()
        .filter(|f| f.adapter == ADAPTER)
        .filter_map(|f| f.invocation.as_ref())
        .collect();
    let metadata = metadata(recognized, cwd)?;
    let artifacts = build(recognized, cwd, false, &[])?;
    let built = read_build(&metadata, artifacts, root);
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
                reads: b.reads,
                duration_ms,
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
/// its package was built from (C-31).
fn read_build(metadata: &Metadata, artifacts: Vec<Artifact>, root: &Path) -> Built {
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

    let artifacts: Vec<Artifact> = artifacts
        .into_iter()
        .filter(|a| packages.contains_key(&a.manifest))
        .collect();
    let mut cache: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    let mut named = BTreeSet::new();
    let mut read_depinfo = |path: &Path| -> BTreeSet<PathBuf> {
        cache
            .entry(path.to_path_buf())
            .or_insert_with(|| parse_depinfo(path, &metadata.workspace_root))
            .clone()
    };
    for artifact in &artifacts {
        if let Some(d) = &artifact.depinfo {
            named.extend(read_depinfo(d).iter().filter_map(|p| relative(p)));
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
            let lib = other
                .target
                .kind
                .iter()
                .any(|k| k.contains("lib") || k == "proc-macro");
            let wanted = (needed.contains(&other.manifest) && lib)
                || (other.manifest == artifact.manifest && (lib || integration));
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
        let mut reads: BTreeSet<String> = BTreeSet::new();
        for d in &depinfos {
            reads.extend(read_depinfo(d).iter().filter_map(|p| relative(p)));
        }
        units.insert(
            unit,
            BuiltUnit {
                executable: executable.clone(),
                reads,
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

/// `cargo metadata --no-deps`: the workspace's members, their targets and path dependencies.
/// It resolves nothing, so it reads no network and writes no lockfile.
fn metadata(recognized: &Recognized, cwd: &Path) -> Result<Metadata, String> {
    let mut command = cargo(recognized, cwd);
    command.args(["metadata", "--format-version", "1", "--no-deps"]);
    let mut words = recognized.build.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--manifest-path" | "--config" | "-Z" => {
                command.arg(word);
                command.args(words.next());
            }
            "--frozen" | "--locked" | "--offline" => {
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

fn cargo(recognized: &Recognized, cwd: &Path) -> Command {
    let mut command = Command::new(&recognized.cargo);
    if let Some(tc) = &recognized.toolchain {
        command.arg(format!("+{tc}"));
    }
    command.current_dir(cwd);
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
) -> Result<Vec<Artifact>, String> {
    let mut command = cargo(recognized, cwd);
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
    let mut artifacts = Vec::new();
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
        artifacts.push(Artifact {
            manifest: std::fs::canonicalize(&manifest).unwrap_or(manifest),
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
    Ok(artifacts)
}

/// The rustc dep-info file of one artifact: beside a hashed file in `deps/`; for one Cargo copied
/// out of `deps/` (a binary, an example), the hashed build with the same bytes. `None` when there
/// is none, and then what it read can't be told.
fn depinfo_of(
    filenames: &[PathBuf],
    executable: Option<&Path>,
    target: &Target,
) -> Option<PathBuf> {
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

/// Every file a Makefile-style dep-info names, absolute: relative ones against `base`.
fn parse_depinfo(path: &Path, base: &Path) -> BTreeSet<PathBuf> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
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
    files.into_iter().map(|p| normalize(&p)).collect()
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

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let exe = at.exe()?;
        let env = |name: &str| (at.env)(name).and_then(|v| v.into_string().ok());
        runner_variables(self, &exe, at.cwd, &env).map(|vars| {
            vars.into_iter()
                .map(|(name, value)| (name, OsString::from(value)))
                .collect()
        })
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let b = plan_build(self, at.root, at.cwd, at.compile_env)?;
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
                    .then(|| vec![super::built_list(&b.reads)]);
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
        let observed = observe(self, at.files, at.root, at.cwd, at.command_passed)?;
        let sets = units::classify(
            at.tree.paths(),
            &|path| observed.unit_files.contains(path),
            Sources::Named(&observed.named, SOURCE_KINDS),
            at.ignore,
        );
        let planned = observed
            .units
            .iter()
            .map(|(unit, seen)| (unit.clone(), vec![super::built_list(&seen.reads)]))
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
        let files = parse_depinfo(&d, Path::new("/ws"));
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
