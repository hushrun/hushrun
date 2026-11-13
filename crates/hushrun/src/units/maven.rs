//! \[ADR 0085\] The Maven adapter (C-47): a unit is a test class of a reactor's module, run by
//! Surefire or Failsafe, for Java, Kotlin, Groovy and Scala, and what it read is every source file
//! below the source roots of its module and of each module of the reactor it depends on, as
//! Maven's own model says.
//!
//! - Loading: `MAVEN_OPTS` gains two system properties, after what it and `.mvn/jvm.config` say,
//!   joined to their values when they set them: `maven.ext.class.path` loads hushrun's Maven core
//!   extension, and `maven.test.additionalClasspath` puts the test-JVM hook ([`super::jvm`]) on
//!   every test JVM's classpath. Both jars are embedded in this binary and written to the cache.
//!   No file of the repository changes. The `mvn` scripts split `MAVEN_OPTS` at spaces, so a
//!   cache path with a space can't be carried, and hushrun observes nothing.
//! - The extension, before any plugin runs, writes the reactor as Maven built its model, in a
//!   facts file whose `settings.maven` holds it, and turns `hushrun.json` into the hook's list,
//!   one section a module.
//! - Outcomes: from the hook's events where it loaded (JUnit 5 on Surefire 3.0.0-M4 and later;
//!   JUnit 4 and TestNG on Surefire 3.6 and later, through the JUnit Platform), else from the XML
//!   reports Surefire and Failsafe wrote during this run ([`reports`]). Where the hook doesn't
//!   load, nothing is left out, and every test runs.
//! - Names: a test class is named by its source file under its module's test source roots,
//!   `pkg/Outer.java` (or `.kt`, `.groovy`, `.scala`), when exactly one file is that, else by its
//!   module's directory and its binary name, `app::pkg.Outer`. A nested class belongs to its outer
//!   class. A test's id is its method's name, after its nested class's path and `#` when it has
//!   one: `passes`, `Inner$Deeper#deepPasses`. Overloads and every invocation of a parametrized
//!   method are one test, since the hook leaves them out by name.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use hushrun_core::units::{self, Facts, NotFollowed, Sets, Sources, Tree};

use super::adapter::{Adapter, Hints, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras};
use super::adapter::{StartAt, WhyAt};
use super::command::program_name;
use super::facts::{FactsFile, MergedTest};
use super::jvm::{self, ClassEvents};

mod reports;

/// The adapter's name in its facts files.
pub const ADAPTER: &str = "maven";
/// The kinds of the files below a module's source roots that are sources (C-30).
pub const SOURCE_KINDS: &[&str] = &[".java", ".kt", ".groovy", ".scala"];
/// hushrun's Maven core extension, as `cargo xtask jars` builds it.
const EXTENSION: &[u8] = include_bytes!("../../jvm/hushrun-maven-extension.jar");
const EXTENSION_NAME: &str = "hushrun-maven-extension.jar";
/// The system property that names Maven's core extensions.
const EXT_CLASS_PATH: &str = "maven.ext.class.path";
/// Surefire's and Failsafe's user property for their additional classpath elements.
const ADDITIONAL_CLASSPATH: &str = "maven.test.additionalClasspath";
/// Why a unit of a module whose reactor dependencies this build doesn't build isn't followed.
pub const NOT_BUILT: &str = "upstream-not-built";

/// COMMAND, when it starts Maven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// `mvn` or `mvnw`, as COMMAND starts it.
    pub launcher: String,
}

/// The `-D` properties `args` set, by name, the last of each: `-Dname=value`, `-Dname` (`true`),
/// and `-D name=value`.
fn properties(args: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let set = if word == "-D" || word == "--define" {
            words.next().map(String::as_str)
        } else if let Some(rest) = word.strip_prefix("--define=") {
            Some(rest)
        } else {
            word.strip_prefix("-D")
        };
        if let Some(set) = set.filter(|s| !s.is_empty()) {
            let (name, value) = set.split_once('=').unwrap_or((set, "true"));
            out.insert(name.to_string(), value.to_string());
        }
    }
    out
}

/// What `command` is for the Maven adapter: `None` when it doesn't start Maven; `Some(Err(why))`
/// when it does in a way hushrun can't observe, which the run says.
pub fn recognize(command: &[String]) -> Option<Result<Recognized, String>> {
    let launcher = program_name(command.first()?);
    let launcher = &*launcher;
    match launcher {
        "mvn" | "mvnw" => {}
        "mvnd" | "mvnd.sh" => {
            return Some(Err(
                "mvnd runs the build in a daemon that keeps its own JVM and options, which hushrun can't load into".into(),
            ));
        }
        _ => return None,
    }
    let set = properties(&command[1..]);
    let on = |name: &str| set.get(name).is_some_and(|v| v != "false");
    for skip in ["skipTests", "maven.test.skip"] {
        if on(skip) {
            return Some(Err(format!("COMMAND skips the tests (-D{skip})")));
        }
    }
    if set.contains_key(EXT_CLASS_PATH) {
        return Some(Err(format!(
            "COMMAND sets -D{EXT_CLASS_PATH}, which would leave out hushrun's Maven extension"
        )));
    }
    Some(Ok(Recognized {
        launcher: launcher.to_string(),
    }))
}

/// \[ADR 0085\] The Maven adapter for `mvn …` and `./mvnw …` (C-47); `None` for any other COMMAND.
pub fn recognized(command: &[String], _: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    Some(recognize(command)?.map(|r| Box::new(r) as Box<dyn Adapter>))
}

/// The value the last `-D<name>=` of `words` gives, if one does.
fn last_property<'a>(words: impl Iterator<Item = &'a str>, name: &str) -> Option<String> {
    let prefix = format!("-D{name}=");
    words
        .filter_map(|w| w.strip_prefix(&prefix))
        .last()
        .map(str::to_string)
}

/// `MAVEN_OPTS` with hushrun's extension and hook last, so that they win as a property given twice
/// does, each joined to the value `.mvn/jvm.config` (read first by the `mvn` scripts) or
/// `MAVEN_OPTS` gave it. `Err` when a path has a space, which the scripts would split.
pub fn maven_opts(
    opts: Option<&str>,
    jvm_config: Option<&str>,
    extension: &Path,
    hook: &Path,
) -> Result<String, String> {
    for path in [extension, hook] {
        let text = path.to_string_lossy();
        if text.chars().any(char::is_whitespace) {
            return Err(format!(
                "the path of hushrun's jar for Maven, {text}, has a space, and the mvn script splits MAVEN_OPTS at spaces"
            ));
        }
    }
    let words = || {
        jvm_config
            .unwrap_or("")
            .split_whitespace()
            .chain(opts.unwrap_or("").split_whitespace())
    };
    let separator = if cfg!(windows) { ";" } else { ":" };
    let join = |had: Option<String>, ours: &Path, sep: &str| match had.filter(|h| !h.is_empty()) {
        Some(had) => format!("{had}{sep}{}", ours.display()),
        None => ours.display().to_string(),
    };
    let ext = join(last_property(words(), EXT_CLASS_PATH), extension, separator);
    let classpath = join(last_property(words(), ADDITIONAL_CLASSPATH), hook, ",");
    let mut out = opts.unwrap_or("").trim().to_string();
    if !out.is_empty() {
        out.push(' ');
    }
    out.push_str(&format!(
        "-D{EXT_CLASS_PATH}={ext} -D{ADDITIONAL_CLASSPATH}={classpath}"
    ));
    Ok(out)
}

/// `.mvn/jvm.config` of the project Maven finds from `cwd`: the nearest directory up from it with
/// a `.mvn` directory.
fn jvm_config(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join(".mvn").is_dir() {
            return std::fs::read_to_string(d.join(".mvn").join("jvm.config")).ok();
        }
        dir = d.parent();
    }
    None
}

// What the extension writes.

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    pub version: u32,
    pub started_ms: i64,
    pub units_dir: String,
    #[serde(default)]
    pub leave_out: Option<LeaveOutSaid>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    pub projects: Vec<Project>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaveOutSaid {
    pub written: bool,
    #[serde(default)]
    pub listed: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub built: bool,
    pub basedir: String,
    #[serde(default)]
    pub source_roots: Vec<String>,
    #[serde(default)]
    pub test_source_roots: Vec<String>,
    #[serde(default)]
    pub upstream: Vec<String>,
    #[serde(default)]
    pub upstream_not_built: Vec<String>,
    #[serde(default)]
    pub test_jars: Vec<String>,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginSaid>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSaid {
    #[serde(default)]
    pub reports_directory: Option<String>,
    #[serde(default)]
    pub configured: BTreeMap<String, String>,
    #[serde(default)]
    pub reports: Vec<Snapshot>,
}

/// A report that was in a reports directory before the build began.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub name: String,
    pub mtime_ms: i64,
    pub size: u64,
}

/// The one graph the extension wrote in this run: `Ok(None)` when it wrote none (it didn't load);
/// `Err` when it wrote more than one, or one that doesn't read.
pub fn graph(files: &[FactsFile]) -> Result<Option<Graph>, String> {
    let mut graphs = Vec::new();
    for file in files.iter().filter(|f| f.adapter == ADAPTER) {
        let Some(value) = file.settings.get("maven") else {
            continue;
        };
        let graph: Graph = serde_json::from_value(value.clone())
            .map_err(|err| format!("hushrun's Maven extension wrote what doesn't read ({err})"))?;
        if graph.version != 1 {
            return Err(format!(
                "hushrun's Maven extension wrote version {} of its file",
                graph.version
            ));
        }
        graphs.push(graph);
    }
    match graphs.len() {
        0 => Ok(None),
        1 => Ok(graphs.pop()),
        n => Err(format!(
            "Maven started {n} times under COMMAND, so which tests ran in which can't be told"
        )),
    }
}

/// A module of the reactor inside the repository, its paths from the repository root.
#[derive(Debug, Clone, Default)]
struct Module {
    /// Its directory, `.` for the root.
    dir: String,
    abs: PathBuf,
    built: bool,
    test_roots: Vec<String>,
    /// \[C-47\] The directories its test classes read every file below: its own roots, each
    /// upstream module's main roots, and its test roots for a test-jar dependency.
    dirs: BTreeSet<String>,
    not_built: Vec<String>,
    project: Option<Project>,
}

fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` from `root`, `/`-separated, `.` for the root itself; `None` outside it.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let inside = real(path).strip_prefix(root).ok()?.to_path_buf();
    let parts: Vec<String> = inside
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// The modules of `graph` whose directories are inside `root` (canonical), by directory.
fn modules(graph: &Graph, root: &Path) -> BTreeMap<String, Module> {
    let rel_all = |paths: &[String]| -> Vec<String> {
        paths
            .iter()
            .filter_map(|p| relative(root, Path::new(p)))
            .collect()
    };
    let by_id: BTreeMap<&str, &Project> =
        graph.projects.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut out = BTreeMap::new();
    for p in &graph.projects {
        let Some(dir) = relative(root, Path::new(&p.basedir)) else {
            continue;
        };
        let main_roots = rel_all(&p.source_roots);
        let test_roots = rel_all(&p.test_source_roots);
        let mut dirs: BTreeSet<String> = main_roots.iter().chain(&test_roots).cloned().collect();
        for up in &p.upstream {
            let Some(u) = by_id.get(up.as_str()) else {
                continue;
            };
            dirs.extend(rel_all(&u.source_roots));
            if p.test_jars.contains(up) {
                dirs.extend(rel_all(&u.test_source_roots));
            }
        }
        out.insert(
            dir.clone(),
            Module {
                dir,
                abs: real(Path::new(&p.basedir)),
                built: p.built,
                test_roots,
                dirs,
                not_built: p.upstream_not_built.clone(),
                project: Some(p.clone()),
            },
        );
    }
    out
}

/// Whether `path` is below `dir` (`.` is the root, below which every path is).
fn below(path: &str, dir: &str) -> bool {
    dir == "." || dir.is_empty() || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

fn of_kind(path: &str) -> bool {
    SOURCE_KINDS.iter().any(|k| path.ends_with(k))
}

/// \[C-47\] The one rule for the sources: the files of a JVM kind below a directory some unit
/// read, `dirs`. Every other file, POMs and resources among them, is in the rest set.
pub fn sources(tree: &Tree, dirs: &BTreeSet<String>) -> BTreeSet<String> {
    super::gotest::under(tree, dirs)
        .into_iter()
        .filter(|p| of_kind(p))
        .collect()
}

/// \[C-30, C-47\] The four sets: `unit_files` the units, [`sources`] the sources.
pub fn classify(
    tree: &Tree,
    unit_files: &BTreeSet<String>,
    dirs: &BTreeSet<String>,
    ignore: &[hushrun_core::glob::Pattern],
) -> Sets {
    let named = sources(tree, dirs);
    units::classify(
        tree.paths(),
        &|p| unit_files.contains(p),
        Sources::Named(&named, SOURCE_KINDS),
        ignore,
    )
}

/// \[C-47\] A test class's unit: its source file below its module's test roots when exactly one
/// file there is `pkg/Outer.<kind>` and no longer root holds it; else `<module dir>::<class>`,
/// made of its module's test sources (`own`).
fn unit_of(module: &Module, class: &str, tree: &Tree) -> (String, Option<Vec<String>>) {
    let rel = class.replace('.', "/");
    let mut found = Vec::new();
    for root in &module.test_roots {
        for kind in SOURCE_KINDS {
            let path = if root == "." {
                format!("{rel}{kind}")
            } else {
                format!("{root}/{rel}{kind}")
            };
            if tree.contains(&path) {
                found.push((root, path));
            }
        }
    }
    if let [(root, path)] = found.as_slice() {
        let longest = module
            .test_roots
            .iter()
            .filter(|r| below(path, r))
            .max_by_key(|r| r.len());
        if longest == Some(*root) {
            return (path.clone(), None);
        }
    }
    let own: Vec<String> = tree
        .paths()
        .filter(|p| of_kind(p) && module.test_roots.iter().any(|r| below(p, r)))
        .map(str::to_string)
        .collect();
    (format!("{}::{class}", module.dir), Some(own))
}

/// What the run's events and reports say of one test class of one module.
#[derive(Debug, Clone, Default)]
struct ClassSeen {
    events: Option<ClassEvents>,
    /// From reports: each test's invocations' outcomes and time, and a failure of the class.
    cases: BTreeMap<String, (Vec<reports::Outcome>, u64)>,
    case_class_failed: bool,
    /// From reports: the class was skipped whole (JUnit 4's `@Ignore` on it).
    case_class_skipped: bool,
    from_reports: bool,
    /// \[C-39\] Its reports hold cases no test file owns, charged to it.
    generated: bool,
}

/// \[C-39\] The id of a case no test file owns, charged to the class that ran it: the top-level
/// class it names, a `.`, and its id there (`testlib.Checks.testOne`), which no method of the
/// class itself has; its id alone when it names no class.
fn generated_id(named: &str, id: &str) -> String {
    if named.is_empty() {
        id.to_string()
    } else {
        format!("{named}.{id}")
    }
}

impl ClassSeen {
    /// One test case of a report of this class: a test's invocation, or the class's own (a
    /// failure outside its tests, or the whole class skipped).
    fn add_case(&mut self, case: &reports::Case) {
        self.from_reports = true;
        let class = case.classname.as_str();
        let top = class.split('$').next().unwrap_or(class);
        let simple = top.rsplit('.').next().unwrap_or(top);
        let class_level = case.name.is_empty()
            || case.name == "initializationError"
            || case.name == class
            || case.name == simple;
        if class_level {
            self.case_class_failed |= case.outcome == reports::Outcome::Failed;
            self.case_class_skipped |= case.outcome == reports::Outcome::Skipped;
            return;
        }
        let (_, id) = jvm::test_id(class, method_of_case(&case.name), true);
        let slot = self.cases.entry(id).or_default();
        slot.0.push(case.outcome);
        slot.1 = slot.1.saturating_add(case.time_ms);
    }

    /// \[C-39\] A case of a report of this class that no test file owns (its `classname` names
    /// another class, or none): a test of this class by that class's name and its method.
    fn add_generated_case(&mut self, case: &reports::Case) {
        self.from_reports = true;
        self.generated = true;
        let (top, id) = jvm::test_id(&case.classname, method_of_case(&case.name), true);
        let id = if case.classname.is_empty() {
            method_of_case(&case.name).to_string()
        } else {
            generated_id(&top, &id)
        };
        let slot = self.cases.entry(id).or_default();
        slot.0.push(case.outcome);
        slot.1 = slot.1.saturating_add(case.time_ms);
    }

    /// Its events or its reports say something of it failed.
    fn any_failed(&self) -> bool {
        self.events
            .as_ref()
            .is_some_and(|e| e.failed || e.tests.values().any(|t| t.failed))
            || self.case_class_failed
            || self
                .cases
                .values()
                .any(|(o, _)| o.contains(&reports::Outcome::Failed))
    }
}

/// A report's test name up to its parameters or invocation: `param(int)[1]` is `param`.
fn method_of_case(name: &str) -> &str {
    name.split(['[', '(', '{']).next().unwrap_or(name).trim()
}

/// The reports of `dir` this run wrote: `TEST-*.xml` directly in it, not as they were before the
/// build began, and not older than its start. `Err` when one of them doesn't read: then none of
/// them counts.
fn fresh_reports(
    dir: &Path,
    before: &[Snapshot],
    started_ms: i64,
) -> Result<Vec<reports::Case>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(n, p)| n.starts_with("TEST-") && n.ends_with(".xml") && p.is_file())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for (name, path) in names {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);
        let unchanged = before
            .iter()
            .any(|s| s.name == name && s.mtime_ms == mtime_ms && s.size == meta.len());
        if unchanged || mtime_ms < started_ms {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|err| format!("{name}: {err}"))?;
        out.extend(reports::cases(&bytes).map_err(|why| format!("{name}: {why}"))?);
    }
    Ok(out)
}

/// What the Maven adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub seen: BTreeMap<String, super::Seen>,
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    pub own: BTreeMap<String, Vec<String>>,
    pub unit_files: BTreeSet<String>,
    pub failures_by_unit: bool,
    pub refused: Option<String>,
    /// \[C-32\] Why the hook left out what the plan didn't list.
    pub left_out: Option<String>,
    /// \[C-37\] Why a run that failed keeps no pass: tests or failures no class could be told for.
    pub not_kept: Option<String>,
    /// \[C-47\] The directory of every module of the reactor inside the repository, and of those
    /// of which this run saw a class: the build reached them.
    pub modules: BTreeSet<String>,
    pub reached: BTreeSet<String>,
}

/// \[C-47\] What the last run that reached a module learned, for the modules this run's build
/// didn't reach (it stopped at a failure before them, or COMMAND didn't select them): the test
/// files `map` lists below them, and the directories its units of them read. A file of theirs is
/// then where the plan from files puts it, never the rest, and no file there is gone that isn't.
fn unreached(map: &super::store::Map, observed: &Observed) -> (BTreeSet<String>, BTreeSet<String>) {
    let module_of = |path: &str| -> Option<String> {
        if let Some((dir, _)) = path.split_once("::") {
            return Some(dir.to_string());
        }
        observed
            .modules
            .iter()
            .filter(|dir| below(path, dir))
            .max_by_key(|dir| if *dir == "." { 0 } else { dir.len() })
            .cloned()
    };
    let kept = |path: &str| !module_of(path).is_some_and(|dir| observed.reached.contains(&dir));
    let files = map
        .unit_files
        .iter()
        .filter(|path| kept(path))
        .cloned()
        .collect();
    let dirs = map
        .units
        .iter()
        .filter(|(unit, _)| !observed.seen.contains_key(*unit) && kept(unit))
        .flat_map(|(_, mapped)| mapped.dirs.iter().cloned())
        .collect();
    (files, dirs)
}

/// Reads what the extension, the hook and the reports say of a run in the repository at `root`
/// whose files are `tree`: `Ok(None)` when the extension didn't load.
pub fn observe(files: &[FactsFile], root: &Path, tree: &Tree) -> Result<Option<Observed>, String> {
    let Some(graph) = graph(files)? else {
        return Ok(None);
    };
    for error in &graph.errors {
        tracing::debug!("hushrun's Maven extension: {error}");
    }
    let root = real(root);
    let modules = modules(&graph, &root);
    let mut unattributed = 0u64;
    let mut seen: BTreeMap<(String, String), ClassSeen> = BTreeMap::new();

    // The hook's events, each JVM's by the module whose directory it ran in.
    let events = jvm::read_events_dir(&Path::new(&graph.units_dir).join(jvm::EVENTS_DIR))?;
    // The hook loaded in a JVM that listened, or left something out.
    let listened = events
        .values()
        .any(|e| e.listened || e.classes.values().any(|c| c.left_out));
    // \[C-39\] A top-level class a test file of `module` is: else what a suite's case names (a
    // tester of Guava's testlib), which the suite that ran it owns.
    let owned =
        |module: &Module, top: &str| !top.is_empty() && unit_of(module, top, tree).1.is_none();
    let add_events = |seen: &mut BTreeMap<(String, String), ClassSeen>, key, ce: &ClassEvents| {
        let entry: &mut ClassSeen = seen.entry(key).or_default();
        entry.events = Some(match entry.events.take() {
            None => ce.clone(),
            Some(had) => merge_events(had, ce),
        });
    };
    for jvm_events in events.values() {
        let dir = jvm_events.basedir.as_ref().or(jvm_events.dir.as_ref());
        let module = dir
            .map(|d| real(Path::new(d)))
            .and_then(|d| modules.values().find(|m| m.abs == d));
        unattributed += jvm_events.unattributed;
        let Some(module) = module else {
            if jvm_events
                .classes
                .values()
                .chain(jvm_events.generated.values())
                .any(|c| !c.tests.is_empty() || c.failed)
            {
                unattributed += 1;
            }
            continue;
        };
        for (class, ce) in &jvm_events.classes {
            add_events(&mut seen, (module.dir.clone(), class.clone()), ce);
        }
        for ((runner, named), ce) in &jvm_events.generated {
            if owned(module, named) {
                // A test class of the repository that a suite ran (`@SuiteClasses`): its own.
                add_events(&mut seen, (module.dir.clone(), named.clone()), ce);
                continue;
            }
            let charged = ClassEvents {
                tests: ce
                    .tests
                    .iter()
                    .map(|(id, t)| (generated_id(named, id), t.clone()))
                    .collect(),
                failed: ce.failed,
                open: ce.open,
                unnamed: ce.unnamed,
                generated: true,
                ..ClassEvents::default()
            };
            add_events(&mut seen, (module.dir.clone(), runner.clone()), &charged);
        }
    }

    // The reports this run wrote, for the classes the events don't speak of.
    for module in modules.values().filter(|m| m.built) {
        let Some(project) = &module.project else {
            continue;
        };
        for (kind, plugin) in &project.plugins {
            let suffixed = plugin.configured.contains_key("reportNameSuffix")
                || graph
                    .properties
                    .contains_key(&format!("{kind}.reportNameSuffix"));
            let Some(dir) = plugin.reports_directory.as_ref().filter(|_| !suffixed) else {
                continue;
            };
            let Ok(cases) = fresh_reports(Path::new(dir), &plugin.reports, graph.started_ms) else {
                // A report cut short: nothing of this directory counts, and its classes are
                // unknown, never passed.
                continue;
            };
            for case in cases {
                let suite = case.suite.split('$').next().unwrap_or(&case.suite);
                let class = case.classname.as_str();
                let top = class.split('$').next().unwrap_or(class).to_string();
                // \[C-39\] A case no test file owns, in the report of a class one does (a
                // suite's): that class's.
                if top != suite && !owned(module, &top) && owned(module, suite) {
                    seen.entry((module.dir.clone(), suite.to_string()))
                        .or_default()
                        .add_generated_case(&case);
                    continue;
                }
                if case.classname.is_empty() {
                    unattributed += 1;
                    continue;
                }
                seen.entry((module.dir.clone(), top))
                    .or_default()
                    .add_case(&case);
            }
        }
    }

    let mut out = Observed {
        failures_by_unit: unattributed == 0,
        not_kept: (unattributed > 0).then(|| {
            format!(
                "hushrun couldn't tell which test class {unattributed} of its tests or failures belong to, so no pass of a run that failed is kept"
            )
        }),
        modules: modules.keys().cloned().collect(),
        reached: seen.keys().map(|(dir, _)| dir.clone()).collect(),
        ..Observed::default()
    };
    // \[C-32\] Something of this run failed: a failure no class owns, or any class's.
    let run_failed = unattributed > 0 || seen.values().any(ClassSeen::any_failed);
    // Each class that ran, by module and top-level class, with the ids its reports name.
    let seen_ran: BTreeMap<(String, String), BTreeSet<String>> = seen
        .iter()
        .filter(|(_, c)| {
            !c.cases.is_empty()
                || c.events
                    .as_ref()
                    .is_some_and(|e| e.started || e.tests.values().any(|t| t.started))
        })
        .map(|(k, c)| (k.clone(), c.cases.keys().cloned().collect()))
        .collect();
    for ((module_dir, class), class_seen) in seen {
        let Some(module) = modules.get(&module_dir) else {
            continue;
        };
        let Some(mut seen_unit) = decide_class(&class_seen, run_failed) else {
            continue;
        };
        if !module.not_built.is_empty() {
            seen_unit.facts.not_followed.insert(NotFollowed {
                why: NOT_BUILT.into(),
                detail: module.not_built.join(", "),
            });
        }
        let (unit, own) = unit_of(module, &class, tree);
        seen_unit.reads = super::gotest::under(tree, &module.dirs);
        match &own {
            Some(own) => out.unit_files.extend(own.iter().cloned()),
            None => {
                out.unit_files.insert(unit.clone());
            }
        }
        if let Some(own) = own {
            out.own.insert(unit.clone(), own);
        }
        out.dirs.insert(unit.clone(), module.dirs.clone());
        match out.seen.get_mut(&unit) {
            // Two classes of one unit can't be: a unit is one class. Keep what both say.
            Some(had) => {
                had.facts.failed |= seen_unit.facts.failed;
                had.facts.complete &= seen_unit.facts.complete;
                had.facts.deselected &= seen_unit.facts.deselected;
                had.tests = None;
            }
            None => {
                out.seen.insert(unit, seen_unit);
            }
        }
    }
    out.left_out = left_out_unlisted(&graph, &modules, &events, &seen_ran);
    let listed = graph
        .leave_out
        .as_ref()
        .is_some_and(|l| !l.listed.is_empty());
    out.refused = match &graph.leave_out {
        Some(l) if !l.written => Some("hushrun's Maven extension couldn't write the list".into()),
        Some(_) if listed && !listened => Some(
            "the test-JVM hook loaded in no test JVM: Surefire before 3.0.0-M4, JUnit 4 or TestNG before Surefire 3.6, or a POM that sets additionalClasspathElements".into(),
        ),
        _ => None,
    };
    Ok(Some(out))
}

/// \[C-32\] Why the hook left out, in some module's JVM, what the extension's list for that module
/// didn't list; or found no test a method entry named in a class that ran, which it neither left
/// out nor saw start (`ran`: the classes that ran, with the ids their reports name).
fn left_out_unlisted(
    graph: &Graph,
    modules: &BTreeMap<String, Module>,
    events: &BTreeMap<String, jvm::JvmEvents>,
    ran: &BTreeMap<(String, String), BTreeSet<String>>,
) -> Option<String> {
    let listed = graph.leave_out.as_ref().map(|l| &l.listed);
    for module in modules.values() {
        let abs = module.abs.to_string_lossy().into_owned();
        let lines: BTreeSet<String> = listed
            .and_then(|l| l.get(&abs))
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let mine: Vec<&jvm::JvmEvents> = events
            .values()
            .filter(|e| {
                e.basedir
                    .as_ref()
                    .or(e.dir.as_ref())
                    .is_some_and(|d| real(Path::new(d)) == module.abs)
            })
            .collect();
        if let Some(why) = jvm::left_out_unlisted(mine.iter().copied(), &lines) {
            return Some(why);
        }
        let left_out: BTreeSet<&str> = mine
            .iter()
            .flat_map(|e| &e.left_out)
            .filter_map(|o| o.entry.as_deref())
            .collect();
        for line in &lines {
            let Some((class, method)) = line.split_once('#') else {
                continue;
            };
            let (top, id) = jvm::test_id(class, method, true);
            let Some(ids) = ran.get(&(module.dir.clone(), top)) else {
                continue;
            };
            let started = mine.iter().any(|e| {
                e.seen.iter().any(|s| {
                    s == line
                        || s.strip_prefix(line.as_str())
                            .is_some_and(|rest| rest.starts_with('['))
                })
            });
            if !left_out.contains(line.as_str()) && !started && !ids.contains(&id) {
                return Some(format!(
                    "the test-JVM hook found no test {line} that its list named, in a class that ran"
                ));
            }
        }
    }
    None
}

fn merge_events(mut a: ClassEvents, b: &ClassEvents) -> ClassEvents {
    a.started |= b.started;
    a.finished |= b.finished;
    a.skipped |= b.skipped;
    a.failed |= b.failed;
    a.open |= b.open;
    a.unnamed |= b.unnamed;
    a.left_out |= b.left_out;
    a.generated |= b.generated;
    a.kept.extend(b.kept.iter().cloned());
    for (id, t) in &b.tests {
        let into = a.tests.entry(id.clone()).or_default();
        into.started |= t.started;
        into.open |= t.open;
        into.finished |= t.finished;
        into.failed |= t.failed;
        into.skipped |= t.skipped;
        into.left_out |= t.left_out;
        into.duration_ms = into.duration_ms.saturating_add(t.duration_ms);
    }
    a
}

/// \[C-47\] What one test class's events, or else its reports, come to as facts: hushrun's facts,
/// never a state. A test is finished only when every invocation of it finished; a failure is a
/// failure; a class that failed outside its tests, or something of which never finished,
/// finishes none of its tests but those that failed.
///
/// \[C-32\] A skipped test is told `skipped`, and so is its class. It counts as finished only
/// when nothing of the run failed (`run_failed`): TestNG skips every test of a suite whose
/// configuration failed, in any class, and the tests that depend on a method that failed; then
/// its class didn't run whole either. `None` when neither says anything of it.
fn decide_class(c: &ClassSeen, run_failed: bool) -> Option<super::Seen> {
    let mut tests: BTreeMap<String, MergedTest> = BTreeMap::new();
    let (class_failed, open, mut complete, deselected, unnamed);
    let mut class_skipped = false;
    // \[C-39\] It ran cases no test file owns: the hook leaves nothing of it out.
    let generated = c.generated || c.events.as_ref().is_some_and(|e| e.generated);
    if let Some(e) = c.events.as_ref().filter(|e| {
        e.started
            || e.finished
            || e.skipped
            || e.failed
            || e.tests.values().any(|t| t.started || t.finished)
    }) {
        let any_failed = e.failed || e.tests.values().any(|t| t.failed);
        let any_open = e.open || e.tests.values().any(|t| t.open);
        for (id, t) in &e.tests {
            let mut finished = t.finished && !t.open;
            if (t.skipped && (any_failed || run_failed)) || ((e.failed || any_open) && !t.failed) {
                finished = false;
            }
            tests.insert(
                id.clone(),
                MergedTest {
                    finished: finished || t.failed,
                    failed: t.failed,
                    deselected: t.left_out && !t.started,
                    skipped: t.skipped && !t.failed,
                    never_left_out: generated,
                    duration_ms: t.duration_ms,
                    ..MergedTest::default()
                },
            );
        }
        class_skipped = e.skipped;
        class_failed = any_failed;
        open = any_open;
        let all_done = !e.tests.is_empty()
            && e.tests
                .values()
                .all(|t| (t.finished && !t.open) || (t.left_out && !t.started));
        complete = !open && (e.finished || e.skipped || (!e.started && all_done));
        deselected = false;
        unnamed = e.unnamed;
    } else if let Some(e) = c.events.as_ref().filter(|e| e.left_out && !c.from_reports) {
        // Only left out: nothing of it started.
        for (id, t) in &e.tests {
            tests.insert(
                id.clone(),
                MergedTest {
                    deselected: t.left_out,
                    ..MergedTest::default()
                },
            );
        }
        class_failed = false;
        open = false;
        complete = false;
        deselected = true;
        unnamed = e.unnamed;
    } else if c.from_reports {
        let any_failed = c.case_class_failed
            || c.cases
                .values()
                .any(|(o, _)| o.contains(&reports::Outcome::Failed));
        for (id, (outcomes, ms)) in &c.cases {
            let failed = outcomes.contains(&reports::Outcome::Failed);
            let skipped = outcomes.contains(&reports::Outcome::Skipped);
            let finished =
                failed || !((skipped && (any_failed || run_failed)) || c.case_class_failed);
            tests.insert(
                id.clone(),
                MergedTest {
                    finished,
                    failed,
                    skipped: skipped && !failed,
                    never_left_out: generated,
                    duration_ms: *ms,
                    ..MergedTest::default()
                },
            );
        }
        // TestNG's interceptor leaves tests out where no listener writes their events (TestNG's
        // own provider): what it left out is in the events, what ran in the reports.
        for (id, t) in c.events.iter().flat_map(|e| &e.tests) {
            if t.left_out && !t.started && !tests.contains_key(id) {
                tests.insert(
                    id.clone(),
                    MergedTest {
                        deselected: true,
                        ..MergedTest::default()
                    },
                );
            }
        }
        class_failed = any_failed;
        class_skipped = c.case_class_skipped;
        open = false;
        complete = true;
        deselected = false;
        unnamed = false;
    } else {
        return None;
    }
    let duration_ms = tests
        .values()
        .map(|t| t.duration_ms)
        .fold(0, u64::saturating_add);
    // \[C-32\] A skip that didn't count as finished: the class didn't run whole.
    if tests.values().any(|t| t.skipped && !t.finished) {
        complete = false;
    }
    let skipped = class_skipped || tests.values().any(|t| t.skipped && !t.deselected);
    Some(super::Seen {
        facts: Facts {
            complete: complete && !open,
            failed: class_failed,
            deselected,
            skipped,
            never_left_out: generated,
            ..Facts::default()
        },
        reads: BTreeSet::new(),
        duration_ms,
        tests: (!unnamed).then_some(tests),
    })
}

/// The directories every unit of `map` and of the hints read: what the plan's sources are below.
fn known_dirs(map: &super::store::Map, shared: &super::shared::Shared) -> BTreeSet<String> {
    let mut dirs: BTreeSet<String> = map
        .units
        .values()
        .flat_map(|m| m.dirs.iter().cloned())
        .collect();
    for (_, hint) in &shared.hints {
        dirs.extend(hint.units.values().flat_map(|u| u.dirs.iter().cloned()));
    }
    dirs
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "Maven"
    }

    fn test_file(&self) -> &'static str {
        "test class"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            reads: false,
        }
    }

    fn hints(&self) -> Hints {
        Hints::Kinds(SOURCE_KINDS)
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let extension = jvm::install(at.cache, EXTENSION_NAME, EXTENSION)
            .map_err(|err| format!("can't write hushrun's Maven extension: {err}"))?;
        let hook = jvm::install(at.cache, jvm::TEST_JVM_HOOK_NAME, jvm::TEST_JVM_HOOK)
            .map_err(|err| format!("can't write hushrun's test-JVM hook: {err}"))?;
        let opts = (at.env)("MAVEN_OPTS").map(|o| o.to_string_lossy().into_owned());
        let opts = maven_opts(
            opts.as_deref(),
            jvm_config(at.cwd).as_deref(),
            &extension,
            &hook,
        )?;
        // The hook finds its list and events below `HUSHRUN_UNITS`, which a second run changes:
        // a variable of the environment naming others would win, so it is set empty.
        Ok(vec![
            ("MAVEN_OPTS".to_string(), OsString::from(opts)),
            (jvm::LEAVE_OUT_VARIABLE.to_string(), OsString::new()),
            (jvm::EVENTS_VARIABLE.to_string(), OsString::new()),
        ])
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let shared = super::fetch(at.asker, self.hints(), &Sets::default(), at.tree, at.ignore);
        let mut listed: BTreeSet<String> = at
            .map
            .unit_files
            .iter()
            .filter(|f| at.tree.contains(f))
            .cloned()
            .collect();
        if listed.is_empty() {
            listed = shared.unit_files();
        }
        let mut names: BTreeSet<String> = at.map.units.keys().cloned().collect();
        for (_, hint) in &shared.hints {
            names.extend(hint.units.keys().cloned());
        }
        if names.is_empty() {
            return Err("no run here has read Maven's reactor yet".into());
        }
        let dirs = known_dirs(at.map, &shared);
        let sets = classify(at.tree, &listed, &dirs, at.ignore);
        let known = names
            .into_iter()
            .filter(|unit| unit.contains("::") || at.tree.contains(unit))
            .map(|unit| {
                let lists =
                    super::shared::lists(&unit, at.map.units.get(&unit), &shared, at.tree, None);
                (unit, lists)
            })
            .collect();
        Ok(Some(Planning {
            sets,
            known,
            received: shared.received,
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let Some(mut observed) = observe(at.files, at.root, at.tree)? else {
            return Ok(None);
        };
        let (files, mut dirs) = unreached(at.map, &observed);
        observed
            .unit_files
            .extend(files.into_iter().filter(|f| at.tree.contains(f)));
        dirs.extend(observed.dirs.values().flatten().cloned());
        let sets = classify(at.tree, &observed.unit_files, &dirs, at.ignore);
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = observed
            .seen
            .keys()
            .filter_map(|unit| {
                let lists =
                    super::shared::lists(unit, at.map.units.get(unit), &shared, at.tree, None)?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen: observed.seen,
            failures_by_unit: observed.failures_by_unit,
            sets,
            planned,
            own: observed.own,
            dirs: observed.dirs,
            unit_files: Some(observed.unit_files),
            refused: observed.refused,
            left_out: observed.left_out,
            not_kept: observed.not_kept,
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        let dirs = known_dirs(at.map, at.shared);
        let listed: BTreeSet<String> = if at.map.unit_files.is_empty() {
            at.listed.clone()
        } else {
            at.map.unit_files.clone()
        };
        classify(at.tree, &listed, &dirs, at.ignore)
    }

    fn why_is_unit(&self, at: &WhyAt<'_>, _: &Sets, unit: &str) -> bool {
        at.map.units.contains_key(unit) || at.shared.knows(unit)
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a test class {check} ran as far as this machine knows: no run of Maven here reported it"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some(
            "its test classes, and the source roots each read, are those Maven's model named in its last run here",
        )
    }
}

#[cfg(test)]
mod tests;
