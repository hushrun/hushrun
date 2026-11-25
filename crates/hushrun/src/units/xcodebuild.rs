//! \[ADR 0121\] The `xcodebuild test` adapter (C-110 to C-113), for the XCTest and Swift Testing
//! tests of an Xcode project, a workspace, or a Swift package tested through Xcode, on macOS. A
//! unit is a **test target** (a unit-test or UI-test bundle) and a test is its id in the result
//! bundle, `Class/testMethod()` or `Suite/test()`.
//!
//! - **Loading.** `xcodebuild` takes nothing from the environment that would let hushrun inside
//!   it, so hushrun puts an `xcodebuild` of its own first on COMMAND's `PATH`
//!   ([`super::program`], [`XcodebuildProgram`]), as for `swift test` (ADR 0089). It catches
//!   `xcodebuild` typed, or started by a script, a Makefile or fastlane by name; `xcrun
//!   xcodebuild` and a path to it go round it.
//! - **The plan** reads each project's `project.pbxproj` ([`graph`]), or for a Swift package
//!   `swift package describe` as the `swift test` adapter does; nothing is built or listed
//!   first. It writes which unit is which target, for which project, to a file of the run's own
//!   ([`PLAN_FILE`]): hushrun's `xcodebuild` leaves out only units of the projects it is asked to
//!   test.
//! - **In COMMAND,** hushrun's `xcodebuild` adds `-skip-testing:` for each target and test the
//!   plan reuses, and a result bundle of its own unless COMMAND names one; after the run it reads
//!   the bundle with `xcresulttool` ([`results`]) and writes what each test did.

mod args;
pub mod graph;
mod pbxproj;
mod results;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use hushrun_core::units::{self, Facts, Sets, Sources, Tree};

use super::Seen;
use super::adapter::{
    Adapter, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::command::{self, Runner, program_name};
use super::facts::{self, FactsFile, RunFile, TestFacts, UnitFacts};
use super::program::{self, Call, Program};

pub use args::{Parsed, parse};
pub use graph::Graph;

/// The adapter's word, in its facts files, this machine's store and a hint.
pub const ADAPTER: &str = "xcodebuild";
/// The plan's file in the run's directory.
const PLAN_FILE: &str = "xcodebuild-plan.v1";
const PLAN_SCHEMA: &str = "hushrun.xcodebuild-plan/v1";
/// The longest hushrun waits for `xcodebuild -version`, `xcresulttool` or `swift package`.
const BUDGET: Duration = Duration::from_secs(120);
/// The oldest Xcode whose `xcresulttool get test-results` hushrun reads.
const OLDEST_XCODE: u32 = 16;
/// Why nothing was observed when hushrun's `xcodebuild` wrote nothing.
pub const NOT_CAUGHT: &str = "xcodebuild wrote nothing for hushrun: no `xcodebuild test` reached hushrun's `xcodebuild` on PATH; start it as `xcodebuild test`, not with `xcrun xcodebuild` or a path to xcodebuild";
const NOT_MACOS: &str = "xcodebuild runs on macOS only";

/// \[C-110\] The adapter for one COMMAND.
#[derive(Debug)]
pub struct Recognized {
    /// COMMAND's arguments when COMMAND is `xcodebuild` itself: the plan reads the projects they
    /// name. A wrapper's plan reads the project of the folder COMMAND runs in.
    direct: Option<Vec<String>>,
    /// The run's directory, from [`Adapter::variables`], where the plan writes its file.
    dir: OnceLock<PathBuf>,
}

/// What a plan says of each unit, for hushrun's `xcodebuild`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PlanFile {
    schema: String,
    /// Each unit's target name and the project or package it is of, from the root.
    units: BTreeMap<String, PlannedUnit>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PlannedUnit {
    target: String,
    container: String,
}

/// \[C-110\] The adapter for `xcodebuild … test …`, or a wrapper whose own text starts
/// `xcodebuild` with `test` and names no other runner ([`command::wrapper_starts`]). `Some(Err)`
/// for `xcrun xcodebuild test`, a path to `xcodebuild`, a COMMAND hushrun can't observe, and any
/// system but macOS. `None` for any other COMMAND.
pub fn recognized(
    command: &[String],
    cwd: &Path,
) -> Option<Result<Box<dyn super::adapter::Adapter>, String>> {
    let found = recognize(command, cwd)?;
    Some(found.map(|adapter| Box::new(adapter) as Box<dyn Adapter>))
}

/// [`recognized`], as the adapter's own type: what a test of it calls, with no trait object.
pub fn recognize(command: &[String], cwd: &Path) -> Option<Result<Recognized, String>> {
    let found = recognize_command(command, cwd)?;
    Some(found.map(|direct| Recognized {
        direct,
        dir: OnceLock::new(),
    }))
}

/// What `command` in `cwd` is for the adapter: `Some(Ok(Some(args)))` for `xcodebuild` itself,
/// `Some(Ok(None))` for a wrapper, `Some(Err(why))` for one it can't observe.
pub fn recognize_command(
    command: &[String],
    cwd: &Path,
) -> Option<Result<Option<Vec<String>>, String>> {
    let first = command.first()?;
    let name = program_name(first);
    let tests_in = |words: &[String]| parse(words).tests();
    let found = match &*name {
        "xcodebuild" if tests_in(&command[1..]) => {
            if first.contains(['/', '\\']) {
                return Some(Err(format!(
                    "{first} is started by its path, which goes round hushrun's xcodebuild on PATH; start it as `xcodebuild test`"
                )));
            }
            let parsed = parse(&command[1..]);
            if let Some(why) = parsed.refused {
                return Some(Err(why));
            }
            Some(command[1..].to_vec())
        }
        "xcrun" if command.iter().any(|w| program_name(w) == "xcodebuild") && tests_in(command) => {
            return Some(Err(
                "xcrun xcodebuild goes round hushrun's xcodebuild on PATH; start it as `xcodebuild test`".into(),
            ));
        }
        _ if command::wrapper_starts(command, cwd, Runner::Xcodebuild) => None,
        _ => return None,
    };
    if !cfg!(target_os = "macos") {
        return Some(Err(NOT_MACOS.into()));
    }
    Some(Ok(found))
}

/// What a start of `xcodebuild` in `cwd` with `parsed` tests: its projects' folders, from `cwd`,
/// or the package it is in. `Err` why that can't be told.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Container {
    /// Projects, each its `.xcodeproj` folder, and the Swift packages a workspace holds.
    Projects {
        projects: Vec<PathBuf>,
        packages: Vec<PathBuf>,
    },
    /// The Swift package at this folder, tested through its scheme.
    Package(PathBuf),
}

fn containers(parsed: &Parsed, cwd: &Path) -> Result<Container, String> {
    if let Some(workspace) = &parsed.workspace {
        let ws = cwd.join(workspace);
        let data = std::fs::read_to_string(ws.join("contents.xcworkspacedata"))
            .map_err(|err| format!("{workspace}: {err}"))?;
        let (projects, packages) = workspace_refs(&data, &ws);
        return Ok(Container::Projects { projects, packages });
    }
    if let Some(project) = &parsed.project {
        return Ok(Container::Projects {
            projects: vec![cwd.join(project)],
            packages: Vec::new(),
        });
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(cwd)
        .map_err(|err| format!("{}: {err}", cwd.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "xcodeproj"))
        .collect();
    found.sort();
    match found.len() {
        1 => Ok(Container::Projects {
            projects: found,
            packages: Vec::new(),
        }),
        0 if cwd.join("Package.swift").is_file() => Ok(Container::Package(cwd.to_path_buf())),
        0 => Err(format!("{} holds no Xcode project", cwd.display())),
        _ => Err(format!(
            "{} holds more than one Xcode project and COMMAND names none",
            cwd.display()
        )),
    }
}

/// The projects and packages a workspace's `contents.xcworkspacedata` names, by their folders.
fn workspace_refs(data: &str, workspace: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let base = workspace.parent().unwrap_or(workspace).to_path_buf();
    let mut groups: Vec<PathBuf> = vec![base.clone()];
    let mut projects = Vec::new();
    let mut packages = Vec::new();
    let mut rest = data;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        rest = &rest[end..];
        if tag.starts_with("/Group") {
            if groups.len() > 1 {
                groups.pop();
            }
            continue;
        }
        let is_group = tag.starts_with("Group");
        if !is_group && !tag.starts_with("FileRef") {
            continue;
        }
        let Some(location) = attribute(tag, "location") else {
            if is_group && !tag.ends_with('/') {
                groups.push(groups.last().cloned().unwrap_or_else(|| base.clone()));
            }
            continue;
        };
        let here = groups.last().cloned().unwrap_or_else(|| base.clone());
        let path = match location.split_once(':') {
            Some(("group", p)) => here.join(p),
            Some(("container", p)) => base.join(p),
            Some(("absolute", p)) => PathBuf::from(p),
            Some(("self", _)) => workspace.parent().unwrap_or(workspace).to_path_buf(),
            _ => here.join(&location),
        };
        if is_group {
            if !tag.ends_with('/') {
                groups.push(path);
            }
            continue;
        }
        if path.extension().is_some_and(|e| e == "xcodeproj") {
            projects.push(path);
        } else if path.join("Package.swift").is_file() {
            packages.push(path);
        }
    }
    (projects, packages)
}

/// The value of `name = "…"` in a tag's text, with XML's entities read.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(name)?;
    let rest = tag[at + name.len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    Some(
        rest[..end]
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// `path` from `root`, `/`-separated, when it is below it.
fn relative(path: &Path, root: &Path) -> Option<String> {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let rel = real(path).strip_prefix(real(root)).ok()?.to_path_buf();
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// The graph of what `container` tests, for the scope `tree` at `root`, and which unit is which
/// target of which container.
fn graph_for(
    container: &Container,
    root: &Path,
    tree: &Tree,
    cwd: &Path,
    assigned: &[(String, String)],
) -> Result<(Graph, BTreeMap<String, PlannedUnit>), String> {
    let mut out = Graph::default();
    let mut planned = BTreeMap::new();
    match container {
        Container::Projects { projects, packages } => {
            let local: Vec<String> = packages.iter().filter_map(|p| relative(p, root)).collect();
            for project in projects {
                let rel = relative(project, root)
                    .ok_or_else(|| format!("{} is outside the checkout", project.display()))?;
                let text = std::fs::read_to_string(project.join("project.pbxproj"))
                    .map_err(|err| format!("{rel}/project.pbxproj: {err}"))?;
                let g = graph::graph_of_with(&rel, &text, tree, root, &local)?;
                for (unit, t) in &g.tests {
                    planned.insert(
                        unit.clone(),
                        PlannedUnit {
                            target: t.name.clone(),
                            container: rel.clone(),
                        },
                    );
                }
                out.merge(g);
            }
        }
        Container::Package(package) => {
            let rel = relative(package, root)
                .ok_or_else(|| format!("{} is outside the checkout", package.display()))?;
            let scratch =
                tempfile::tempdir().map_err(|err| format!("no temporary directory: {err}"))?;
            let swift = || command::tool("swift", cwd, assigned);
            let described = super::swift::describe_all(&swift, package, scratch.path())?;
            let g = super::swift::graph_of(&described, root, tree);
            for (unit, t) in g.tests {
                planned.insert(
                    unit.clone(),
                    PlannedUnit {
                        target: t.c99.clone(),
                        container: rel.clone(),
                    },
                );
                out.by_name.insert(t.c99.clone(), unit.clone());
                out.tests.insert(
                    unit,
                    graph::TestTarget {
                        name: t.c99,
                        reads: t.reads,
                        own: t.own,
                        not_followed: t.not_followed,
                    },
                );
            }
            out.named.extend(g.named);
            out.edges.extend(g.edges);
        }
    }
    Ok((out, planned))
}

impl Recognized {
    /// The graph the plan and the read go by: of what COMMAND names, or for a wrapper of the
    /// folder it runs in.
    fn graph(
        &self,
        root: &Path,
        tree: &Tree,
        cwd: &Path,
        assigned: &[(String, String)],
        files_only: bool,
    ) -> Result<(Graph, BTreeMap<String, PlannedUnit>), String> {
        let parsed = self.direct.as_deref().map(parse).unwrap_or_default();
        let container = containers(&parsed, cwd)?;
        if files_only && matches!(container, Container::Package(_)) {
            return Err(super::adapter::BUILT.into());
        }
        graph_for(&container, root, tree, cwd, assigned)
    }

    fn planning(&self, at: &PlanAt<'_>, files_only: bool) -> Result<Option<Planning>, String> {
        let (graph, planned) = self.graph(at.root, at.tree, at.cwd, at.assigned, files_only)?;
        let sets = sets_of(at.tree, &graph, at.ignore);
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let known = graph
            .tests
            .iter()
            .map(|(unit, t)| {
                let whole = t.not_followed.is_empty()
                    && !t.own.is_empty()
                    && t.own.iter().all(|f| at.tree.contains(f));
                let lists = whole
                    .then(|| {
                        super::shared::lists(
                            unit,
                            at.map.units.get(unit),
                            &shared,
                            at.tree,
                            Some(&t.reads),
                            None,
                        )
                    })
                    .flatten();
                (unit.clone(), lists)
            })
            .collect();
        if !files_only && let Some(dir) = self.dir.get() {
            let file = PlanFile {
                schema: PLAN_SCHEMA.into(),
                units: planned,
            };
            if let Ok(bytes) = serde_json::to_vec(&file) {
                let _ = facts::write_whole(&dir.join(PLAN_FILE), &bytes);
            }
        }
        Ok(Some(Planning {
            sets,
            known,
            received: shared.received,
            ..Planning::default()
        }))
    }
}

/// \[C-30\] The four sets: a test target's own sources are units, a file of a compiled kind some
/// test target reads is a source, everything else is the rest.
fn sets_of(tree: &Tree, graph: &Graph, ignore: &[hushrun_core::glob::Pattern]) -> Sets {
    let own: BTreeSet<&str> = graph
        .tests
        .values()
        .flat_map(|t| t.own.iter().map(String::as_str))
        .collect();
    units::classify(
        tree.paths(),
        &|p| own.contains(p),
        Sources::Named(&graph.named, super::swift::KINDS),
        ignore,
    )
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "xcodebuild test"
    }

    fn test_file(&self) -> &'static str {
        "test target"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            reads: false,
        }
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let path = (at.env)("PATH");
        if program::real("xcodebuild", &path.clone().unwrap_or_default()).is_none() {
            return Err("there is no xcodebuild on COMMAND's PATH".into());
        }
        let dir = program::make(at.dir, &at.exe()?, &["xcodebuild"])
            .map_err(|err| format!("can't put hushrun's xcodebuild on PATH: {err}"))?;
        let _ = self.dir.set(at.dir.to_path_buf());
        program::variables(&dir, path)
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        self.planning(at, true)
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        self.planning(at, false)
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let mine: Vec<&FactsFile> = at.files.iter().filter(|f| f.adapter == ADAPTER).collect();
        if mine.is_empty() {
            return Err(NOT_CAUGHT.into());
        }
        if let Some(why) = mine.iter().find_map(|f| f.errors.first()) {
            return Err(why.clone());
        }
        let (graph, _) = self.graph(at.root, at.tree, at.cwd, at.assigned, false)?;
        let refused = mine.iter().find_map(|f| {
            f.settings
                .get("refused")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let merged = facts::merged(at.files, ADAPTER);
        let failures_by_unit = mine.iter().all(|f| f.failures_by_unit);
        let mut seen = BTreeMap::new();
        let mut own = BTreeMap::new();
        for (unit, m) in merged {
            let Some(target) = graph.tests.get(&unit) else {
                continue;
            };
            let complete = match &m.tests {
                Some(tests) => tests.values().filter(|t| !t.deselected).all(|t| t.finished),
                None => m.complete,
            };
            let mut not_followed = m.not_followed;
            not_followed.extend(target.not_followed.iter().cloned());
            own.insert(unit.clone(), target.own.clone());
            seen.insert(
                unit,
                Seen {
                    outside: Default::default(),
                    declared: None,
                    getenv: Default::default(),
                    facts: Facts {
                        complete,
                        failed: m.failed,
                        not_followed,
                        deselected: m.deselected,
                        ..Facts::default()
                    },
                    reads: target.reads.clone(),
                    duration_ms: m.duration_ms,
                    tests: m.tests,
                },
            );
        }
        let sets = sets_of(at.tree, &graph, at.ignore);
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = seen
            .keys()
            .filter_map(|unit| {
                let t = graph.tests.get(unit)?;
                if !t.not_followed.is_empty() {
                    return None;
                }
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    Some(&t.reads),
                    None,
                )?;
                Some((unit.clone(), lists))
            })
            .collect();
        let unit_files: BTreeSet<String> = own.values().flatten().cloned().collect();
        Ok(Some(Read {
            seen,
            failures_by_unit,
            sets,
            planned,
            own,
            unit_files: Some(unit_files),
            edges: graph.edges.clone(),
            refused,
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        units::classify(
            at.tree.paths(),
            &|p| at.map.unit_files.contains(p),
            Sources::Named(&at.map.sources, super::swift::KINDS),
            at.ignore,
        )
    }

    fn why_is_unit(&self, at: &WhyAt<'_>, _: &Sets, unit: &str) -> bool {
        at.map.units.contains_key(unit) || at.shared.knows(unit)
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a test target {check} ran as far as this machine knows: its project didn't name it in its last run here"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some(
            "its test targets and what each reads are those its Xcode project named in its last run here",
        )
    }

    fn why_asks_hints(&self, map: &super::store::Map) -> bool {
        !map.sources.is_empty()
    }
}

/// \[ADR 0121\] hushrun's `xcodebuild` on COMMAND's `PATH`.
#[derive(Debug)]
pub struct XcodebuildProgram;

impl Program for XcodebuildProgram {
    fn name(&self) -> &'static str {
        "xcodebuild"
    }

    fn args(&self, call: &Call<'_>) -> Vec<OsString> {
        call.args.to_vec()
    }

    fn run(&self, call: &Call<'_>) -> Option<ExitCode> {
        let units = call.units?;
        if !cfg!(target_os = "macos") {
            return None;
        }
        let args: Vec<String> = call
            .args
            .iter()
            .map(|a| a.to_str().map(str::to_string))
            .collect::<Option<_>>()?;
        let parsed = parse(&args);
        if parsed.passthrough || !parsed.tests() {
            return None;
        }
        if let Some(why) = parsed.refused.clone().or_else(|| too_old(call.real)) {
            write_facts(units, &refusal(why));
            return None;
        }
        Some(observe(call.real, &args, &parsed, units))
    }
}

/// Why the real `xcodebuild` is one hushrun doesn't read: older than Xcode 16, or of no version it
/// can tell.
fn too_old(real: &Path) -> Option<String> {
    let mut command = program::real_command(real);
    command.arg("-version");
    let text = match output_within(command) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(why) => return Some(why),
    };
    match xcode_major(&text) {
        Some(major) if major >= OLDEST_XCODE => None,
        Some(major) => Some(format!(
            "xcodebuild of Xcode {major} is older than Xcode {OLDEST_XCODE}, whose result bundles hushrun reads"
        )),
        None => Some("xcodebuild -version named no Xcode version hushrun can read".into()),
    }
}

/// The major version in `xcodebuild -version`'s first line, `Xcode 26.1`.
pub fn xcode_major(text: &str) -> Option<u32> {
    let rest = text.lines().next()?.trim().strip_prefix("Xcode ")?;
    rest.split('.').next()?.trim().parse().ok()
}

fn output_within(mut command: Command) -> Result<Vec<u8>, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|err| format!("can't start {:?}: {err}", command.get_program()))?;
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        out
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().unwrap_or_default();
                return if status.success() {
                    Ok(out)
                } else {
                    Err(format!("{:?} failed: {status}", command.get_program()))
                };
            }
            Ok(None) if start.elapsed() > BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{:?} took longer than {} s",
                    command.get_program(),
                    BUDGET.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => return Err(format!("{:?}: {err}", command.get_program())),
        }
    }
}

fn refusal(why: String) -> FactsFile {
    FactsFile {
        schema: facts::FACTS_SCHEMA_V1.into(),
        adapter: ADAPTER.into(),
        errors: vec![why],
        ..FactsFile::default()
    }
}

fn write_facts(units: &Path, file: &FactsFile) {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let start = format!("xcodebuild-{}-{nanos}", std::process::id());
    let file = FactsFile {
        start: Some(start.clone()),
        ..file.clone()
    };
    if let Ok(bytes) = serde_json::to_vec(&file) {
        let _ = facts::write_whole(&units.join(format!("{start}.json")), &bytes);
    }
}

/// What one start leaves out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LeftOut {
    /// Units left out whole.
    units: BTreeSet<String>,
    /// Tests left out one by one, by unit.
    tests: BTreeMap<String, BTreeSet<String>>,
    /// The `-skip-testing:` values.
    args: Vec<String>,
}

/// \[C-112\] What `run` lists that this start may leave out: only units the plan named of the
/// containers this start tests, by target name, and none when a target name is the plan's for two
/// units (two projects of a workspace with a target of one name).
fn leave_out(run: &RunFile, plan: &PlanFile, containers: &BTreeSet<String>) -> LeftOut {
    let ours = ours(plan, containers);
    let mut out = LeftOut::default();
    let mut targets = Vec::new();
    for unit in &run.reuse {
        if let Some(p) = ours.get(unit.as_str()) {
            out.units.insert(unit.clone());
            targets.push(p.target.clone());
        }
    }
    let mut tests = Vec::new();
    for (unit, ids) in &run.reuse_tests {
        if out.units.contains(unit) {
            continue;
        }
        if let Some(p) = ours.get(unit.as_str()) {
            for id in ids {
                out.tests
                    .entry(unit.clone())
                    .or_default()
                    .insert(id.clone());
                tests.push((p.target.clone(), id.clone()));
            }
        }
    }
    out.args = args::skip_testing(&targets, &tests);
    out
}

/// The plan's units of the containers a start tests (`containers`), by unit, but those whose
/// target name the plan gives two units: `-skip-testing` and the result bundle name a target by
/// its name alone.
fn ours<'p>(
    plan: &'p PlanFile,
    containers: &BTreeSet<String>,
) -> BTreeMap<&'p str, &'p PlannedUnit> {
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for p in plan.units.values() {
        *names.entry(p.target.as_str()).or_default() += 1;
    }
    plan.units
        .iter()
        .filter(|(_, p)| containers.contains(&p.container))
        .filter(|(_, p)| names.get(p.target.as_str()) == Some(&1))
        .map(|(u, p)| (u.as_str(), p))
        .collect()
}

/// The containers a start tests, from the root, as the plan names them.
fn containers_from_root(parsed: &Parsed, cwd: &Path, root: &Path) -> BTreeSet<String> {
    match containers(parsed, cwd) {
        Ok(Container::Projects { projects, .. }) => {
            projects.iter().filter_map(|p| relative(p, root)).collect()
        }
        Ok(Container::Package(p)) => relative(&p, root).into_iter().collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// The run hushrun's `xcodebuild` observes. Its exit is the real one's.
fn observe(real: &Path, args: &[String], parsed: &Parsed, units: &Path) -> ExitCode {
    let work = units.join(format!("xcodebuild-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&work);
    let cwd = std::env::current_dir().unwrap_or_default();
    let run_file: Option<RunFile> = std::fs::read(units.join(facts::RUN_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let plan = plan_file(units);
    let report_tests = run_file.as_ref().and_then(|r| r.unit.as_deref()) == Some("test");
    let tested = run_file
        .as_ref()
        .map(|run| containers_from_root(parsed, &cwd, &run.root))
        .unwrap_or_default();
    let left = match (&run_file, &plan) {
        (Some(run), Some(plan)) => leave_out(run, plan, &tested),
        _ => LeftOut::default(),
    };
    let bundle = match &parsed.result_bundle {
        Some(theirs) => cwd.join(theirs),
        None => work.join("tests.xcresult"),
    };
    let before = std::fs::metadata(&bundle).and_then(|m| m.modified()).ok();
    let mut command = program::real_command(real);
    command.args(args).args(&left.args);
    if parsed.result_bundle.is_none() {
        command.arg("-resultBundlePath").arg(&bundle);
    }
    command.env_remove(super::runner::UNITS_VAR);
    let started = SystemTime::now();
    let status = match command.status() {
        Ok(status) => status,
        Err(err) => {
            eprintln!("error: can't start {}: {err}", real.display());
            let _ = std::fs::remove_dir_all(&work);
            return ExitCode::from(if err.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            });
        }
    };
    let exit = status.code();
    // A bundle COMMAND named is read only when this run wrote it.
    let modified = std::fs::metadata(&bundle).and_then(|m| m.modified()).ok();
    let written = modified.is_some_and(|m| {
        Some(m) != before
            && m.duration_since(started)
                .map_or_else(|e| e.duration() < Duration::from_secs(1), |_| true)
    });
    let results = written.then(|| read_bundle(real, &bundle).ok()).flatten();
    let file = match &results {
        Some(results) => facts_of(results, &left, plan.as_ref(), &tested, exit, report_tests),
        None => {
            let mut file = refusal("xcodebuild wrote no result bundle hushrun could read".into());
            if !left.args.is_empty() {
                file.left_out_unlisted = Some(
                    "the result bundle couldn't be read, so what xcodebuild ran of those left out can't be checked".into(),
                );
            }
            file
        }
    };
    write_facts(units, &file);
    let _ = std::fs::remove_dir_all(&work);
    super::runner::end_with(status.code(), signal_of(&status))
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

#[cfg(not(unix))]
fn signal_of(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// The plan's file, in the run's directory, or for an audit's second run in the first run's.
fn plan_file(units: &Path) -> Option<PlanFile> {
    let mut places = vec![units.join(PLAN_FILE)];
    if units.file_name().is_some_and(|n| n == "again")
        && let Some(parent) = units.parent()
    {
        places.push(parent.join(PLAN_FILE));
    }
    places.into_iter().find_map(|path| {
        let plan: PlanFile = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        (plan.schema == PLAN_SCHEMA).then_some(plan)
    })
}

/// The bundle's tests, from `xcrun xcresulttool get test-results tests`, beside the real
/// `xcodebuild`'s developer folder.
fn read_bundle(real: &Path, bundle: &Path) -> Result<results::Results, String> {
    let xcrun = real
        .parent()
        .map(|dir| dir.join("xcrun"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("/usr/bin/xcrun"));
    let mut command = program::real_command(&xcrun);
    command
        .args(["xcresulttool", "get", "test-results", "tests", "--path"])
        .arg(bundle);
    let bytes = output_within(command)?;
    results::read(&bytes).ok_or_else(|| "xcresulttool's output wasn't a result tree".into())
}

/// \[C-112\] The facts file of a run (pure, for tests): each test the bundle names of a unit the
/// plan knows, and those left out.
fn facts_of(
    results: &results::Results,
    left: &LeftOut,
    plan: Option<&PlanFile>,
    containers: &BTreeSet<String>,
    exit: Option<i32>,
    report_tests: bool,
) -> FactsFile {
    let by_target: BTreeMap<&str, &str> = plan
        .map(|p| {
            ours(p, containers)
                .into_iter()
                .map(|(u, p)| (p.target.as_str(), u))
                .collect()
        })
        .unwrap_or_default();
    let mut units: BTreeMap<String, UnitFacts> = BTreeMap::new();
    for unit in &left.units {
        units.insert(
            unit.clone(),
            UnitFacts {
                unit: unit.clone(),
                deselected: true,
                tests: report_tests.then(Vec::new),
                ..UnitFacts::default()
            },
        );
    }
    let mut any_failed = false;
    for (target, tests) in results {
        let Some(unit) = by_target.get(target.as_str()) else {
            continue;
        };
        let facts = units
            .entry((*unit).to_string())
            .or_insert_with(|| UnitFacts {
                unit: (*unit).to_string(),
                tests: Some(Vec::new()),
                ..UnitFacts::default()
            });
        facts.deselected = false;
        let list = facts.tests.get_or_insert_with(Vec::new);
        for (id, outcome) in tests {
            let (failed, skipped, ms) = match outcome {
                results::Outcome::Passed(ms) => (false, false, *ms),
                results::Outcome::Skipped(ms) => (false, true, *ms),
                results::Outcome::Failed(ms) => (true, false, *ms),
            };
            any_failed |= failed;
            list.push(TestFacts {
                id: id.clone(),
                finished: true,
                failed,
                skipped,
                duration_ms: ms,
                ..TestFacts::default()
            });
        }
        if let Some(gone) = left.tests.get(*unit) {
            for id in gone {
                if !tests.contains_key(id) {
                    list.push(TestFacts {
                        id: id.clone(),
                        deselected: true,
                        ..TestFacts::default()
                    });
                }
            }
        }
    }
    for facts in units.values_mut() {
        if let Some(tests) = &facts.tests {
            facts.selected = tests.iter().filter(|t| !t.deselected).count() as u64;
            facts.finished = tests.iter().filter(|t| t.finished).count() as u64;
            facts.failed = tests.iter().any(|t| t.failed);
            facts.duration_ms = tests.iter().map(|t| t.duration_ms).sum();
        }
        if !report_tests && !facts.deselected {
            facts.tests = None;
        }
    }
    // A run that failed with no test failing (a build that failed, a crash outside any test)
    // keeps no pass.
    let failures_by_unit = exit == Some(0) || any_failed;
    FactsFile {
        schema: facts::FACTS_SCHEMA_V1.into(),
        adapter: ADAPTER.into(),
        failures_by_unit,
        units: units.into_values().collect(),
        ..FactsFile::default()
    }
}

#[cfg(test)]
mod tests;
