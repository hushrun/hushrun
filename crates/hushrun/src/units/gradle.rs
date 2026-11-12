//! \[ADR 0084\] The Gradle adapter, observed (C-30, C-32, C-46): for Java, Kotlin and Android's
//! local unit tests, a unit is a test class, named by its source file, and a test is one of its
//! methods. What a test class read is every source file below the directory of its project and of
//! each project its test classpath is built from.
//!
//! - Loading: a Groovy script, embedded in this binary and written to `<cache dir>/adapters/`,
//!   that the build's settings file applies from `HUSHRUN_GRADLE` with one line `hushrun init`
//!   proposes ([`GROOVY_LINE`], [`KOTLIN_LINE`]). No route from the environment alone loads code
//!   into a Gradle build that a daemon already runs, so without the line hushrun observes nothing,
//!   and says so. The test-JVM hook ([`super::jvm`]) goes on each Test task's classpath, named in
//!   `HUSHRUN_GRADLE_HOOK`.
//! - The script reports facts and decides nothing: one file per Test task in `HUSHRUN_UNITS`, a
//!   `hushrun.unit-facts/v1` file of no unit whose `settings.gradle` is the task's report
//!   ([`Report`]): Gradle's own test events as they came, how the task ended, the names of its test
//!   classes, what it left out and its classpath mapped to the build's projects. Everything else
//!   is decided here, in [`observe`].
//! - Selection: a class reused whole by an exclusion Gradle evaluates while it looks at the task's
//!   class files; a test reused one by one by the hook for the JUnit Platform and TestNG, and by
//!   Gradle's own filter patterns for JUnit 4, up to 250 tests a task.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use hushrun_core::units::{self, Facts, NotFollowed, Sets, Sources, Tree};

use super::adapter::{
    Adapter, Hints, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::facts::{FactsFile, MergedTest, Unfollowed};
use super::shared::Shared;
use super::store::Map;

/// The adapter's name in its facts files.
pub const ADAPTER: &str = "gradle";
/// What a test class reads of the files below its projects' directories: its sources (C-30).
/// Build scripts (`.gradle`, `.gradle.kts`), resources, manifests and Android's `res/` are rest.
pub const SOURCE_KINDS: &[&str] = &[".java", ".kt", ".groovy", ".scala"];
/// The variable the settings line applies the script from.
pub const VARIABLE: &str = "HUSHRUN_GRADLE";
/// The variable that names the test-JVM hook's jar for the script.
pub const HOOK_VARIABLE: &str = "HUSHRUN_GRADLE_HOOK";
/// The line a Groovy settings file needs, after any `pluginManagement {}` and `plugins {}`.
pub const GROOVY_LINE: &str =
    r#"if (System.getenv("HUSHRUN_GRADLE")) apply from: System.getenv("HUSHRUN_GRADLE")"#;
/// The line a Kotlin settings file needs.
pub const KOTLIN_LINE: &str = r#"System.getenv("HUSHRUN_GRADLE")?.let { apply(from = it) }"#;
/// The schema of a task's report, inside its facts file.
pub const REPORT_SCHEMA: &str = "hushrun.gradle-report/v1";
/// The script, applied to the build's settings.
const SCRIPT: &str = include_str!("hushrun_gradle.gradle");
/// The settings files Gradle reads, in the order it looks for them.
const SETTINGS_NAMES: &[&str] = &["settings.gradle.kts", "settings.gradle"];
/// Failures Gradle reports as a test that are its class's: JUnit's `initializationError` (a
/// `@BeforeAll` or a Spock `setupSpec` that failed, a class JUnit 4 refuses), Gradle's JUnit 4
/// runner's `classMethod` (`@BeforeClass`, `@AfterClass`), Kotest's `executionError`.
const CLASS_LEVEL: &[&str] = &["initializationError", "classMethod", "executionError"];

/// COMMAND, when it is Gradle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// `-p`/`--project-dir`, as COMMAND names it.
    pub project_dir: Option<String>,
    /// `-c`/`--settings-file` (Gradle 8), as COMMAND names it.
    pub settings: Option<String>,
}

/// What `command` is for the Gradle adapter: `None` when it doesn't start Gradle (`gradle`, a
/// wrapper `gradlew`, or either's `.bat`, by name or path); `Some(Err(why))` when it does, in a way
/// hushrun doesn't observe.
pub fn recognize(command: &[String]) -> Option<Result<Recognized, String>> {
    let file = super::command::program_name(command.first()?);
    if file != "gradle" && file != "gradlew" {
        return None;
    }
    let mut out = Recognized {
        project_dir: None,
        settings: None,
    };
    let mut words = command[1..].iter();
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (word.as_str(), None),
        };
        match flag {
            "-t" | "--continuous" => {
                return Some(Err("gradle --continuous runs again as files change".into()));
            }
            "-p" | "--project-dir" => {
                out.project_dir = inline.or_else(|| words.next().cloned());
            }
            "-c" | "--settings-file" => {
                out.settings = inline.or_else(|| words.next().cloned());
            }
            _ => {}
        }
    }
    Some(Ok(out))
}

/// The settings file Gradle reads for `recognized` in `cwd`: COMMAND's, else the first in the
/// build's directory or a directory above it.
pub fn settings_file(recognized: &Recognized, cwd: &Path) -> Option<PathBuf> {
    if let Some(file) = &recognized.settings {
        return Some(cwd.join(file));
    }
    let start = recognized
        .project_dir
        .as_ref()
        .map_or_else(|| cwd.to_path_buf(), |dir| cwd.join(dir));
    let mut dir = Some(start.as_path());
    while let Some(here) = dir {
        if let Some(found) = SETTINGS_NAMES
            .iter()
            .map(|name| here.join(name))
            .find(|path| path.is_file())
        {
            return Some(found);
        }
        dir = here.parent();
    }
    None
}

/// The line `file` needs: Kotlin's for a `.kts` file, else Groovy's.
fn line_for(file: &Path) -> &'static str {
    if file.extension().is_some_and(|e| e == "kts") {
        KOTLIN_LINE
    } else {
        GROOVY_LINE
    }
}

/// Why the adapter can't load for `recognized` in `cwd`: no settings file, or one without the line.
pub fn unloadable(recognized: &Recognized, cwd: &Path) -> Option<String> {
    let Some(settings) = settings_file(recognized, cwd) else {
        return Some(format!(
            "Gradle loads hushrun's adapter from one line in its settings file, and there is none here: `hushrun init` proposes settings.gradle with `{GROOVY_LINE}`"
        ));
    };
    let text = std::fs::read_to_string(&settings).unwrap_or_default();
    if text.contains(VARIABLE) {
        return None;
    }
    let shown = settings
        .strip_prefix(cwd)
        .unwrap_or(&settings)
        .display()
        .to_string();
    Some(format!(
        "Gradle loads hushrun's adapter from one line in {shown}, which `hushrun init` proposes: `{}`",
        line_for(&settings)
    ))
}

/// \[ADR 0084\] What `hushrun init` proposes, in its "Change by hand" list, for a repository at
/// `root` with a Gradle build, when `checks` look at their test files: the one line in the
/// settings file that loads hushrun's adapter. `None` when there is no Gradle build at the root,
/// or the line is there.
pub fn proposal(root: &Path, checks: &[String]) -> Option<String> {
    if checks.is_empty() {
        return None;
    }
    let names = checks.join(", ");
    let settings = SETTINGS_NAMES
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file());
    match settings {
        Some(settings) => {
            let text = std::fs::read_to_string(&settings).unwrap_or_default();
            if text.contains(VARIABLE) {
                return None;
            }
            let name = settings.file_name()?.to_string_lossy().into_owned();
            Some(format!(
                "{name}: add `{}` after any pluginManagement {{}} and plugins {{}} block, so that hushrun can look at Gradle's test classes for {names} (tests in hushrun.yaml); without HUSHRUN_GRADLE, which hushrun sets, it does nothing",
                line_for(&settings)
            ))
        }
        None => {
            let built = ["build.gradle", "build.gradle.kts", "gradlew"]
                .iter()
                .any(|name| root.join(name).is_file());
            built.then(|| {
                format!(
                    "settings.gradle: write `{GROOVY_LINE}`, so that hushrun can look at Gradle's test classes for {names} (tests in hushrun.yaml)"
                )
            })
        }
    }
}

/// The script for this binary, written whole when it isn't there yet:
/// `<cache dir>/adapters/gradle-<first 16 hex of its SHA-256>/hushrun.gradle`.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(SCRIPT.as_bytes()).hex();
    let path = cache
        .join("adapters")
        .join(format!("gradle-{}", &digest[..16]))
        .join("hushrun.gradle");
    if std::fs::read(&path).ok().as_deref() != Some(SCRIPT.as_bytes()) {
        super::facts::write_whole(&path, SCRIPT.as_bytes())?;
    }
    Ok(path)
}

/// One Test task's report, as the script writes it in its facts file's `settings.gradle`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Report {
    pub schema: String,
    pub task: String,
    /// The task's project's directory, from the repository root.
    pub project_dir: String,
    /// `executed`, `up-to-date`, `from-cache`, `no-source`, `skipped`, or `not-followed` for a test
    /// task the script can't follow.
    pub outcome: String,
    pub task_failed: bool,
    /// Gradle ended the task's root test suite: every test it started ended.
    pub root_done: bool,
    /// `junit-platform`, `junit4` or `testng`; none for a framework the script doesn't know.
    pub framework: Option<String>,
    /// Each test class the report speaks of, by its outer class, with its name for people.
    pub names: BTreeMap<String, String>,
    pub left_out: LeftOut,
    /// The task's own includes and `--tests`: a class missing from the report hasn't gone away.
    pub narrowed_by: Vec<String>,
    /// Every test Gradle reported, each invocation as it came.
    pub invocations: Vec<Invocation>,
    /// Suites that failed with no failed test under them.
    pub suite_failures: Vec<SuiteFailure>,
    pub reads: ReportReads,
    /// What the script couldn't follow of the task: every test class of it.
    pub not_followed: Vec<Unfollowed>,
}

/// What a task left out.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LeftOut {
    /// Outer classes left out whole.
    pub classes: Vec<String>,
    /// Tests, by id, it asked the hook or Gradle's filter to leave out.
    pub tests: Vec<String>,
    /// Tests `reuseTests` listed that it couldn't leave out: they run.
    pub not_attempted: Vec<String>,
}

/// One test invocation, as Gradle's `afterTest` gave it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Invocation {
    /// Its class, as Gradle names it (`pkg.Outer$Inner`).
    pub c: Option<String>,
    /// Its name.
    pub n: String,
    /// Its parents with a class, nearest first: `[name, class]`.
    pub p: Vec<(String, String)>,
    /// `SUCCESS`, `FAILURE` or `SKIPPED`.
    pub r: String,
    pub ms: u64,
}

/// A suite that failed by itself.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SuiteFailure {
    pub n: String,
    pub c: Option<String>,
}

/// What a task's classpath was built from.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReportReads {
    /// Directories of the repository: its project's and those of each project its classpath is
    /// built from, and their source directories outside them.
    pub dirs: Vec<String>,
    /// Other entries of its classpath inside the repository, from its root.
    pub paths: Vec<String>,
}

/// A class's outer class: its binary name up to the first `$`.
fn outer_of(class: &str) -> &str {
    match class.find('$') {
        Some(at) if at > 0 => &class[..at],
        _ => class,
    }
}

/// Whether a parent suite is a class's own suite rather than a method's.
fn is_class_suite(name: &str, class: &str) -> bool {
    let simple = class.rsplit('.').next().unwrap_or(class);
    let inner = simple.rsplit('$').next().unwrap_or(simple);
    name == class || name == simple || name == inner
}

/// \[ADR 0084\] A test's id: `<its class as Gradle names it>.<its method>`, every invocation of a
/// method one test. The method is Gradle's name, or for an invocation under a method's own suite of
/// the same class (a parametrized, repeated or factory method, a Spock unrolled feature), the
/// topmost such suite's; then `(args)`, `[n]` and `[n](x)` go. `None` without a class.
pub fn test_id(inv: &Invocation) -> Option<String> {
    let class = inv.c.as_deref()?;
    let mut top: Option<&str> = None;
    for (name, of) in &inv.p {
        if of != class || is_class_suite(name, class) {
            break;
        }
        top = Some(name);
    }
    let name = match top {
        // JUnit 4's Parameterized suites are named `[0: x=1]`: the test keeps its own name.
        Some(top) if !top.starts_with('[') => top,
        _ => inv.n.as_str(),
    };
    Some(format!("{class}.{}", method_of(name)))
}

/// A method's name without `(args)`, `[n]` or `[n](x)`.
fn method_of(name: &str) -> &str {
    let ident_start = |c: char| c.is_alphabetic() || c == '_' || c == '$';
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut chars = name.char_indices();
    if let Some((_, first)) = chars.next()
        && ident_start(first)
    {
        for (at, c) in chars {
            if c == '[' || c == '(' {
                return &name[..at];
            }
            if !ident(c) {
                break;
            }
        }
    }
    // A Kotlin name in backticks, `name with spaces()`: the JUnit Platform appends its parameter
    // types with no space before the parenthesis.
    if let Some(inner) = name.strip_suffix(')')
        && let Some(open) = inner.rfind('(')
        && open > 0
        && !name[..open].ends_with(char::is_whitespace)
    {
        let types = &inner[open + 1..];
        let is_type = |t: &str| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '$' | '[' | ']'))
        };
        if types.is_empty() || types.split(',').all(|t| is_type(t.trim_start())) {
            return &name[..open];
        }
    }
    name
}

/// What one task saw of one test, before the tasks are merged.
#[derive(Debug, Clone, Default)]
struct TestSaw {
    finished: bool,
    failed: bool,
    deselected: bool,
    duration_ms: u64,
}

/// What one or more tasks saw of one unit, merged.
#[derive(Debug, Clone, Default)]
struct UnitSaw {
    /// Each task that saw it ran it whole, or left it out.
    complete: bool,
    failed: bool,
    deselected: bool,
    duration_ms: u64,
    not_followed: BTreeSet<NotFollowed>,
    dirs: BTreeSet<String>,
    paths: BTreeSet<String>,
    tests: BTreeMap<String, TestSaw>,
    /// A task reported tests for it: its tests are known.
    told: bool,
}

impl UnitSaw {
    /// What no task has said anything of yet: complete and left out until one says otherwise,
    /// which `add` takes as AND.
    fn unseen() -> UnitSaw {
        UnitSaw {
            complete: true,
            deselected: true,
            ..UnitSaw::default()
        }
    }

    /// Adds one task's word on the unit (or one class of it): complete, failed and deselected as
    /// that task saw it, its tests, what wasn't followed, and what the task read.
    fn add(&mut self, one: UnitSaw) {
        self.complete &= one.complete || one.deselected;
        self.failed |= one.failed;
        self.deselected &= one.deselected;
        self.duration_ms = self.duration_ms.saturating_add(one.duration_ms);
        self.not_followed.extend(one.not_followed);
        self.dirs.extend(one.dirs);
        self.paths.extend(one.paths);
        self.told |= one.told;
        for (id, saw) in one.tests {
            match self.tests.get_mut(&id) {
                None => {
                    self.tests.insert(id, saw);
                }
                // A test finished only when every task that ran it finished it, and was left out
                // only when every task that saw it left it out.
                Some(had) => {
                    if !saw.deselected {
                        had.finished = if had.deselected {
                            saw.finished
                        } else {
                            had.finished && saw.finished
                        };
                    }
                    had.failed |= saw.failed;
                    had.deselected &= saw.deselected;
                    had.duration_ms = had.duration_ms.saturating_add(saw.duration_ms);
                }
            }
        }
    }
}

/// What the Gradle adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub units: BTreeMap<String, super::Seen>,
    /// The test classes named by their files: the unit rule (C-30).
    pub unit_files: BTreeSet<String>,
    /// The directories each unit read every file below.
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    /// Units named by no file (`<project dir>::<class>`), each made of no file of the scope.
    pub own: BTreeMap<String, Vec<String>>,
    /// The directories some unit read: where the sources are (C-30).
    pub known_dirs: BTreeSet<String>,
}

/// A task's report, from one facts file, with its errors.
fn report_of(file: &FactsFile) -> Result<Report, String> {
    let value = file
        .settings
        .get(ADAPTER)
        .ok_or("a Gradle task's facts hold no report")?;
    let report: Report = serde_json::from_value(value.clone())
        .map_err(|err| format!("a Gradle task's report can't be read: {err}"))?;
    if report.schema != REPORT_SCHEMA {
        return Err(format!(
            "a Gradle task's report is {}, not {REPORT_SCHEMA}",
            report.schema
        ));
    }
    Ok(report)
}

/// Each unit one task's report speaks of, as that task saw it, with its tests.
fn units_of(report: &Report, errors: &[String]) -> Vec<(String, UnitSaw)> {
    let mut out = Vec::new();
    if report.outcome == "not-followed" {
        return out;
    }
    let executed = report.outcome == "executed";
    let no_source = report.outcome == "no-source";
    // The task ran its tests whole: its root suite ended, or it sent no event and succeeded.
    let whole = no_source
        || (executed
            && (report.root_done || (!report.task_failed && report.invocations.is_empty())));
    // A failure outside any class: no test of the task is known to have run whole.
    let outside = report.suite_failures.iter().any(|s| s.c.is_none());
    let whole = whole && !outside;
    let mut task_not_followed: BTreeSet<NotFollowed> =
        report.not_followed.iter().map(NotFollowed::from).collect();
    if let Some(error) = errors.first() {
        task_not_followed.insert(NotFollowed {
            why: "adapter-error".into(),
            detail: error.chars().take(200).collect(),
        });
    }
    let class_failed: BTreeSet<&str> = report
        .invocations
        .iter()
        .filter(|inv| CLASS_LEVEL.contains(&inv.n.as_str()) && inv.r == "FAILURE")
        .filter_map(|inv| inv.c.as_deref().map(outer_of))
        .chain(
            report
                .suite_failures
                .iter()
                .filter_map(|s| s.c.as_deref().map(outer_of)),
        )
        .collect();
    // Each test's invocations, by outer class and id.
    let mut by_class: BTreeMap<&str, BTreeMap<String, Vec<&Invocation>>> = BTreeMap::new();
    for inv in &report.invocations {
        let Some(class) = inv.c.as_deref() else {
            continue;
        };
        if CLASS_LEVEL.contains(&inv.n.as_str()) {
            continue;
        }
        let Some(id) = test_id(inv) else { continue };
        by_class
            .entry(outer_of(class))
            .or_default()
            .entry(id)
            .or_default()
            .push(inv);
    }
    let left_out_classes: BTreeSet<&str> =
        report.left_out.classes.iter().map(String::as_str).collect();
    let up_to_date = matches!(report.outcome.as_str(), "up-to-date" | "from-cache");
    for (outer, name) in &report.names {
        let events = by_class.get(outer.as_str());
        let mut one = UnitSaw::unseen();
        one.not_followed = task_not_followed.clone();
        one.dirs = report.reads.dirs.iter().cloned().collect();
        one.paths = report.reads.paths.iter().cloned().collect();
        if up_to_date || !(executed || no_source) {
            // Gradle didn't execute the task: its tests record nothing.
            one.complete = false;
            one.deselected = false;
            out.push((name.clone(), one));
            continue;
        }
        one.deselected = left_out_classes.contains(outer.as_str()) && events.is_none();
        one.complete = whole;
        one.failed = class_failed.contains(outer.as_str());
        // A class left out whole ran nothing, and Gradle never named its tests.
        one.told = !one.deselected;
        let testng = report.framework.as_deref() == Some("testng");
        let any_failed =
            events.is_some_and(|tests| tests.values().flatten().any(|inv| inv.r == "FAILURE"));
        for (id, invs) in events.into_iter().flatten() {
            let failed = invs.iter().any(|inv| inv.r == "FAILURE");
            let skipped = invs.iter().all(|inv| inv.r == "SKIPPED");
            // A test skipped in a class whose setup failed didn't earn a pass: TestNG reports a
            // failing configuration method as a failed test and skips the rest of its class.
            let cascade = skipped && (one.failed || (testng && any_failed));
            let saw = TestSaw {
                finished: whole && !cascade,
                failed,
                deselected: false,
                duration_ms: invs.iter().map(|inv| inv.ms).fold(0, u64::saturating_add),
            };
            one.failed |= failed;
            one.duration_ms = one.duration_ms.saturating_add(saw.duration_ms);
            one.tests.insert(id.clone(), saw);
        }
        // What the task asked to leave out of this class, and doesn't appear: left out when the
        // task ran whole, else not known to have run.
        let prefix_ok = |id: &str| {
            id.strip_prefix(outer.as_str())
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('$'))
        };
        for id in report.left_out.tests.iter().filter(|id| prefix_ok(id)) {
            one.tests.entry(id.clone()).or_insert(TestSaw {
                finished: false,
                failed: false,
                deselected: whole,
                duration_ms: 0,
            });
        }
        out.push((name.clone(), one));
    }
    out
}

/// Reads what the Gradle adapter wrote in a run's directory, for the repository whose files are
/// `tree`. `Ok(None)` when it wrote nothing; `Err` says why nothing can be told.
pub fn observe(files: &[FactsFile], tree: &Tree) -> Result<Option<Observed>, String> {
    let mine: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == ADAPTER).collect();
    if mine.is_empty() {
        return Ok(None);
    }
    let mut merged: BTreeMap<String, UnitSaw> = BTreeMap::new();
    for file in mine {
        let report = report_of(file)?;
        for (name, one) in units_of(&report, &file.errors) {
            merged.entry(name).or_insert_with(UnitSaw::unseen).add(one);
        }
    }
    let mut out = Observed::default();
    for (unit, saw) in merged {
        let mut not_followed = saw.not_followed.clone();
        let mut dirs = saw.dirs.clone();
        let mut reads: BTreeSet<String> = BTreeSet::new();
        // Other entries of the classpath inside the repository: a file of the scope is read, a
        // directory with files of the scope is read whole, and anything else (built output
        // outside every project's build directory) can't be told.
        for path in &saw.paths {
            if tree.contains(path) {
                reads.insert(path.clone());
            } else if super::gotest::under(tree, &BTreeSet::from([path.clone()])).is_empty() {
                not_followed.insert(NotFollowed {
                    why: "classpath".into(),
                    detail: path.clone(),
                });
            } else {
                dirs.insert(path.clone());
            }
        }
        reads.extend(super::gotest::under(tree, &dirs));
        if unit.contains("::") {
            out.own.insert(unit.clone(), Vec::new());
        } else {
            reads.insert(unit.clone());
            out.unit_files.insert(unit.clone());
        }
        out.known_dirs.extend(dirs.iter().cloned());
        out.dirs.insert(unit.clone(), dirs);
        out.units.insert(
            unit,
            super::Seen {
                facts: Facts {
                    complete: saw.complete,
                    failed: saw.failed,
                    not_followed,
                    deselected: saw.deselected,
                    ..Facts::default()
                },
                reads,
                duration_ms: saw.duration_ms,
                tests: saw.told.then(|| {
                    saw.tests
                        .iter()
                        .map(|(id, t)| {
                            let merged = MergedTest {
                                finished: t.finished,
                                failed: t.failed,
                                deselected: t.deselected,
                                duration_ms: t.duration_ms,
                                ..MergedTest::default()
                            };
                            (id.clone(), merged)
                        })
                        .collect()
                }),
            },
        );
    }
    Ok(Some(out))
}

/// \[C-30, ADR 0084\] The sources: every file of a JVM kind below one of `dirs`, the directories
/// test classes read, but below build logic: a `buildSrc`, or a directory with a settings file of
/// its own (a build it includes for its plugins), between that directory and the file.
pub fn sources_below(tree: &Tree, dirs: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for path in tree.paths() {
        if !SOURCE_KINDS.iter().any(|kind| path.ends_with(kind)) {
            continue;
        }
        let deepest = dirs
            .iter()
            .filter(|dir| {
                dir.is_empty()
                    || *dir == "."
                    || path
                        .strip_prefix(dir.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|dir| if *dir == "." { 0 } else { dir.len() });
        let Some(deepest) = deepest else { continue };
        let base = if deepest.is_empty() || deepest == "." {
            ""
        } else {
            deepest.as_str()
        };
        let rest = if base.is_empty() {
            path
        } else {
            &path[base.len() + 1..]
        };
        let parts: Vec<&str> = rest.split('/').collect();
        let mut dir = base.to_string();
        let mut logic = false;
        for part in &parts[..parts.len() - 1] {
            if !dir.is_empty() {
                dir.push('/');
            }
            dir.push_str(part);
            if *part == "buildSrc"
                || SETTINGS_NAMES
                    .iter()
                    .any(|name| tree.contains(&format!("{dir}/{name}")))
            {
                logic = true;
                break;
            }
        }
        if !logic {
            out.insert(path.to_string());
        }
    }
    out
}

/// \[C-30\] The four sets: `units`, the test classes' files; the sources below `dirs`; the rest.
pub fn classify(
    tree: &Tree,
    units: &BTreeSet<String>,
    dirs: &BTreeSet<String>,
    ignore: &[hushrun_core::glob::Pattern],
) -> Sets {
    let named = sources_below(tree, dirs);
    units::classify(
        tree.paths(),
        &|path| units.contains(path),
        Sources::Named(&named, SOURCE_KINDS),
        ignore,
    )
}

/// The units a plan knows of, and the directories they read, from this machine's map, else from
/// the newest hint that names test classes: the test classes' files of the last run here, and the
/// units named by no file it kept.
fn known(
    map: &Map,
    shared: &Shared,
    tree: &Tree,
) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let mut files: BTreeSet<String> = map
        .unit_files
        .iter()
        .filter(|f| tree.contains(f))
        .cloned()
        .collect();
    let unnamed: BTreeSet<String> = map
        .units
        .keys()
        .filter(|unit| unit.contains("::"))
        .cloned()
        .collect();
    let mut dirs: BTreeSet<String> = files
        .iter()
        .chain(&unnamed)
        .filter_map(|unit| map.units.get(unit))
        .flat_map(|mapped| mapped.dirs.iter().cloned())
        .collect();
    if files.is_empty() && unnamed.is_empty() {
        files = shared.unit_files();
        for (_, hint) in &shared.hints {
            for unit in &files {
                if let Some(seen) = hint.units.get(unit) {
                    dirs.extend(seen.dirs.iter().cloned());
                }
            }
        }
    }
    (files, unnamed, dirs)
}

/// \[ADR 0084\] The Gradle adapter for a COMMAND that starts Gradle, whose settings file loads it
/// (C-46); `None` for any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    let recognized = match recognize(command)? {
        Ok(recognized) => recognized,
        Err(why) => return Some(Err(why)),
    };
    if let Some(why) = unloadable(&recognized, cwd) {
        return Some(Err(why));
    }
    Some(Ok(Box::new(recognized)))
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "Gradle"
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
        let script =
            install(at.cache).map_err(|err| format!("can't write its Gradle script: {err}"))?;
        let hook = super::jvm::install(
            at.cache,
            super::jvm::TEST_JVM_HOOK_NAME,
            super::jvm::TEST_JVM_HOOK,
        )
        .map_err(|err| format!("can't write its test-JVM hook: {err}"))?;
        Ok(vec![
            (VARIABLE.to_string(), script.into_os_string()),
            (HOOK_VARIABLE.to_string(), hook.into_os_string()),
        ])
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let shared = super::fetch(at.asker, self.hints(), &Sets::default(), at.tree, at.ignore);
        let (files, unnamed, dirs) = known(at.map, &shared, at.tree);
        if files.is_empty() && unnamed.is_empty() {
            return Err("no run here has listed Gradle's test classes yet".into());
        }
        let sets = classify(at.tree, &files, &dirs, at.ignore);
        let known = files
            .iter()
            .chain(&unnamed)
            .map(|unit| {
                let lists =
                    super::shared::lists(unit, at.map.units.get(unit), &shared, at.tree, None);
                (unit.clone(), lists)
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
        let Some(observed) = observe(at.files, at.tree)? else {
            return Ok(None);
        };
        let sets = classify(
            at.tree,
            &observed.unit_files,
            &observed.known_dirs,
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = observed
            .units
            .keys()
            .filter_map(|unit| {
                let lists =
                    super::shared::lists(unit, at.map.units.get(unit), &shared, at.tree, None)?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen: observed.units,
            failures_by_unit: true,
            sets,
            planned,
            own: observed.own,
            dirs: observed.dirs,
            unit_files: Some(observed.unit_files),
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        let (_, _, dirs) = known(at.map, at.shared, at.tree);
        classify(at.tree, at.listed, &dirs, at.ignore)
    }

    fn why_is_unit(&self, at: &WhyAt<'_>, _: &Sets, unit: &str) -> bool {
        at.map.units.contains_key(unit) || at.listed.contains(unit) || at.shared.knows(unit)
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a test class of {check} as far as this machine knows: Gradle didn't run it in its last run here"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("Gradle's test classes and what each read are those of its last run here")
    }
}

#[cfg(test)]
mod tests;
