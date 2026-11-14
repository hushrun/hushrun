//! \[ADR 0087\] The Gradle adapter, observed (C-30, C-32, C-46): for Java, Kotlin and Android's
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
//!   classes, what it left out and its classpath mapped to the build's projects, and where its test
//!   JVMs wrote the hook's events, which are read here too. Each start of Gradle keeps its own
//!   files. Everything else is decided here, in [`observe`].
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
use super::jvm::{self, JvmEvents};
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

/// The exception classes of a Test task's failure that say its test process ended badly: what
/// Gradle's test executer throws, and the process's own.
const DIED_TYPES: &[&str] = &[
    "TestSuiteExecutionException",
    "ProcessExecutionException",
    "ExecException",
];

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
    let file = &*file;
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

/// \[ADR 0087\] What `hushrun init` proposes, in its "Change by hand" list, for a repository at
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
    /// The directory this run's test JVMs of the task wrote the hook's events to.
    pub events_dir: Option<String>,
    /// The messages of the task's failure and its causes, when it failed.
    pub task_failures: Vec<String>,
    /// The exception class of the task's failure and of each of its causes, when it failed.
    pub task_failure_types: Vec<String>,
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
    /// How it left tests out: `hook` or `patterns`.
    pub how: Option<String>,
    /// The test-JVM hook's list, `class#method` a line, when it went through the hook.
    pub hook: Vec<String>,
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
    /// File-based Maven repositories inside the repository the build resolves from, from its root.
    pub repositories: Vec<String>,
}

/// \[C-46\] Of the tests a plan lists one by one (`listed`, each by name with its unit and id),
/// those a JUnit 4 task of `groups` would not leave out, which run: as the script's filter takes
/// them, per task, in the order of their ids as strings, the first `most`, and of those none whose
/// class is in the default package or a package whose name starts with a capital letter (Gradle
/// reads such a pattern as a simple class name). The class is the id up to its last `.`: a JUnit 4
/// method's name has none. The script mirrors this; the plan lists only what it leaves out.
pub fn left_in<'a>(
    groups: &[super::store::LeaveOutAtMost],
    listed: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
) -> BTreeSet<String> {
    let listed: Vec<(&str, &str, &str)> = listed.collect();
    let mut out = BTreeSet::new();
    for group in groups {
        let mut of_group: Vec<(&str, &str)> = listed
            .iter()
            .filter(|(_, unit, _)| group.units.contains(*unit))
            .map(|(name, _, id)| (*id, *name))
            .collect();
        of_group.sort_unstable();
        for (at, (id, name)) in of_group.into_iter().enumerate() {
            let class = id.rsplit_once('.').map_or("", |(class, _)| class);
            let named_alone =
                !class.contains('.') || class.chars().next().is_some_and(char::is_uppercase);
            if at >= group.most || named_alone {
                out.insert(name.to_string());
            }
        }
    }
    out
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

/// \[ADR 0087\] A test's id: `<its class as Gradle names it>.<its method>`, every invocation of a
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
    /// \[C-32\] An invocation of it was skipped: its body didn't run.
    skipped: bool,
    /// \[C-39\] The hook can't leave it out: its runner gives it no method (Kotest).
    never_left_out: bool,
    duration_ms: u64,
}

/// What one or more tasks saw of one unit, merged.
#[derive(Debug, Clone, Default)]
struct UnitSaw {
    /// Each task that saw it ran it whole, or left it out.
    complete: bool,
    failed: bool,
    deselected: bool,
    /// \[C-32\] A test of it was skipped.
    skipped: bool,
    duration_ms: u64,
    not_followed: BTreeSet<NotFollowed>,
    dirs: BTreeSet<String>,
    paths: BTreeSet<String>,
    /// File-based Maven repositories inside the repository the build resolves from.
    repositories: BTreeSet<String>,
    tests: BTreeMap<String, TestSaw>,
    /// A task reported tests for it: its tests are known.
    told: bool,
    /// \[C-39\] It ran cases no test class of the task is (a JUnit 4 `suite()`'s), charged to it:
    /// nothing of it can be left out.
    generated: bool,
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
        self.skipped |= one.skipped;
        self.duration_ms = self.duration_ms.saturating_add(one.duration_ms);
        self.not_followed.extend(one.not_followed);
        self.dirs.extend(one.dirs);
        self.paths.extend(one.paths);
        self.repositories.extend(one.repositories);
        self.told |= one.told;
        self.generated |= one.generated;
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
                    had.skipped |= saw.skipped;
                    had.never_left_out |= saw.never_left_out;
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
    /// \[C-32\] Why a task left out what the plan didn't list.
    pub left_out: Option<String>,
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

/// \[C-32\] Why a task's hook left out what its list didn't list, or found no test its list named
/// in a class that ran (it neither left it out nor saw it run): then what ran can't be told.
fn left_out_unlisted(report: &Report, events: &BTreeMap<String, JvmEvents>) -> Option<String> {
    let listed: BTreeSet<String> = report.left_out.hook.iter().cloned().collect();
    if let Some(why) = jvm::left_out_unlisted(events.values(), &listed) {
        return Some(why);
    }
    if report.left_out.how.as_deref() != Some("hook") {
        return None;
    }
    let left_out: BTreeSet<&str> = events
        .values()
        .flat_map(|jvm| &jvm.left_out)
        .filter_map(|out| out.entry.as_deref())
        .collect();
    let ran: BTreeSet<String> = report.invocations.iter().filter_map(test_id).collect();
    let classes_ran: BTreeSet<&str> = report
        .invocations
        .iter()
        .filter_map(|inv| inv.c.as_deref().map(outer_of))
        .collect();
    for line in &report.left_out.hook {
        let Some((class, method)) = line.split_once('#') else {
            continue;
        };
        if classes_ran.contains(outer_of(class))
            && !left_out.contains(line.as_str())
            && !ran.contains(&format!("{class}.{method}"))
        {
            return Some(format!(
                "the test-JVM hook found no test {line} that its list named, in a class that ran"
            ));
        }
    }
    None
}

/// Whether something a task's report speaks of failed: the task, a test, or a suite.
fn report_failed(report: &Report) -> bool {
    report.task_failed
        || !report.suite_failures.is_empty()
        || report.invocations.iter().any(|inv| inv.r == "FAILURE")
}

/// Each unit one task's report speaks of, as that task saw it, with its tests. \[C-32\] A skipped
/// test is told `skipped`; it counts as finished only when nothing of the run failed
/// (`run_failed`): TestNG skips every test of a suite whose configuration failed, in any class,
/// and a JVM that died leaves the test in flight skipped. Then its class didn't run whole either.
///
/// A test JVM that died ran nothing of its task whole: the task failed and no test or suite of it
/// did, or Gradle said its test process ended badly (it calls the test in flight skipped, and
/// says nothing of a class whose `@AfterAll` never ended). A class the hook's events (`events`,
/// this task's JVMs') started and never ended didn't run whole either.
fn units_of(
    report: &Report,
    errors: &[String],
    run_failed: bool,
    events: &Result<BTreeMap<String, JvmEvents>, String>,
) -> Vec<(String, UnitSaw)> {
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
    // \[C-32\] The hook's agent: a test JVM of the task stopped before its runner was done, which
    // Gradle may call a success (a test's `System.exit(0)`). The classes whose frames were below
    // the exit didn't run whole; `None` when which can't be told (a halt, a kill, an exit from
    // elsewhere): then nothing of the task did.
    let stopped: Option<BTreeSet<String>> = events
        .iter()
        .flat_map(BTreeMap::values)
        .filter(|jvm| jvm.stopped_early())
        .try_fold(BTreeSet::new(), |mut classes, jvm| {
            let end = jvm.ended.as_ref().filter(|end| end.by_test)?;
            let found: Vec<String> = end
                .stack
                .split(' ')
                .filter_map(|frame| frame.rsplit_once('.').map(|(class, _)| outer_of(class)))
                .filter(|class| report.names.contains_key(*class))
                .map(str::to_string)
                .collect();
            if found.is_empty() {
                return None;
            }
            classes.extend(found);
            Some(classes)
        });
    // A test JVM that died: the type of the task's failure says so (Gradle's test executer
    // wraps a test process that ended badly in a `TestSuiteExecutionException`, caused by the
    // process's `ProcessExecutionException`, an `ExecException` before Gradle 9); or a failure no
    // test or suite explains; or, last, the words of Gradle's message, for a failure whose
    // description names no type.
    let died = report
        .task_failure_types
        .iter()
        .any(|t| DIED_TYPES.contains(&t.rsplit('.').next().unwrap_or(t)))
        || (report.task_failed
            && report.suite_failures.is_empty()
            && !report.invocations.iter().any(|inv| inv.r == "FAILURE"))
        || report
            .task_failures
            .iter()
            .any(|m| m.contains("non-zero exit value") || m.contains("unexpected problem"))
        || stopped.is_none();
    let whole = whole && !outside && !died && events.is_ok();
    // The classes a test JVM started and never ended, by their outer class, and those a test of
    // which ended its JVM.
    let mut open: BTreeSet<&str> = events
        .iter()
        .flat_map(BTreeMap::values)
        .flat_map(|jvm| jvm.classes.iter())
        .filter(|(_, class)| class.open)
        .map(|(outer, _)| outer.as_str())
        .collect();
    // What a class ran that names another (a suite's cases): open for both.
    for jvm in events.iter().flat_map(BTreeMap::values) {
        for ((runner, named), class) in &jvm.generated {
            if class.open {
                open.insert(runner.as_str());
                if !named.is_empty() {
                    open.insert(named.as_str());
                }
            }
        }
    }
    open.extend(stopped.iter().flatten().map(String::as_str));
    // \[C-39\] Classes whose tests the hook saw with no method to leave them out by (Kotest's):
    // their tests are never left out, so none is ever counted as reused.
    let unnamed: BTreeSet<&str> = events
        .iter()
        .flat_map(BTreeMap::values)
        .flat_map(|jvm| jvm.classes.iter())
        .filter(|(_, class)| class.unnamed)
        .map(|(outer, _)| outer.as_str())
        .collect();
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
    // Each test's invocations, by outer class and id. \[C-39\] A case whose class is no test class
    // of the task (a JUnit 4 suite's, as Guava's testlib builds) is the nearest parent's that is,
    // the class that ran it, which can leave nothing of it out (`generated`).
    let mut by_class: BTreeMap<&str, BTreeMap<String, Vec<&Invocation>>> = BTreeMap::new();
    let mut generated: BTreeSet<&str> = BTreeSet::new();
    for inv in &report.invocations {
        let Some(class) = inv.c.as_deref() else {
            continue;
        };
        if CLASS_LEVEL.contains(&inv.n.as_str()) {
            continue;
        }
        let Some(id) = test_id(inv) else { continue };
        let mut owner = outer_of(class);
        if !report.names.contains_key(owner)
            && let Some(parent) = inv
                .p
                .iter()
                .map(|(_, of)| outer_of(of))
                .find(|of| report.names.contains_key(*of))
        {
            owner = parent;
            generated.insert(parent);
        }
        by_class
            .entry(owner)
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
        one.repositories = report.reads.repositories.iter().cloned().collect();
        if up_to_date || !(executed || no_source) {
            // Gradle didn't execute the task: its tests record nothing.
            one.complete = false;
            one.deselected = false;
            out.push((name.clone(), one));
            continue;
        }
        one.deselected = left_out_classes.contains(outer.as_str()) && events.is_none();
        let class_whole = whole && !open.contains(outer.as_str());
        one.complete = class_whole;
        one.generated = generated.contains(outer.as_str());
        one.failed = class_failed.contains(outer.as_str());
        // A class left out whole ran nothing, and Gradle never named its tests.
        one.told = !one.deselected;
        let any_failed =
            events.is_some_and(|tests| tests.values().flatten().any(|inv| inv.r == "FAILURE"));
        for (id, invs) in events.into_iter().flatten() {
            let failed = invs.iter().any(|inv| inv.r == "FAILURE");
            let skipped = !failed && invs.iter().any(|inv| inv.r == "SKIPPED");
            // A test skipped in a class or a run where something failed didn't earn a pass:
            // TestNG reports a failing configuration method as a failed test and skips what it
            // guards, in its class or in every class of its suite.
            let cascade = skipped && (one.failed || any_failed || run_failed);
            let saw = TestSaw {
                finished: class_whole && !cascade,
                failed,
                deselected: false,
                skipped,
                never_left_out: unnamed.contains(outer.as_str()) || one.generated,
                duration_ms: invs.iter().map(|inv| inv.ms).fold(0, u64::saturating_add),
            };
            one.failed |= failed;
            one.skipped |= skipped;
            if cascade {
                one.complete = false;
            }
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
                skipped: false,
                never_left_out: false,
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
    // \[C-32\] A file of no task but the build's note that Gradle ran no test (`ranNothing`),
    // which the run says as it says any note: the tasks' own files say what they were.
    let mine: Vec<&FactsFile> = files
        .iter()
        .filter(|f| {
            f.adapter == ADAPTER
                && (f.settings.contains_key(ADAPTER) || !f.settings.contains_key("ranNothing"))
        })
        .collect();
    if mine.is_empty() {
        return Ok(None);
    }
    let reports = mine
        .iter()
        .map(|file| Ok((report_of(file)?, &file.errors)))
        .collect::<Result<Vec<_>, String>>()?;
    // \[C-32\] Each task's hook events; none for a task whose JVMs wrote none (JUnit 4, TestNG).
    let events: Vec<Result<BTreeMap<String, JvmEvents>, String>> = reports
        .iter()
        .map(|(report, _)| match &report.events_dir {
            Some(dir) => jvm::read_events_dir(Path::new(dir)),
            None => Ok(BTreeMap::new()),
        })
        .collect();
    // \[C-32\] Something of this run's tasks failed.
    let run_failed = reports.iter().any(|(report, _)| report_failed(report));
    let mut merged: BTreeMap<String, UnitSaw> = BTreeMap::new();
    for ((report, errors), events) in reports.iter().zip(&events) {
        for (name, one) in units_of(report, errors, run_failed, events) {
            merged.entry(name).or_insert_with(UnitSaw::unseen).add(one);
        }
    }
    let mut out = Observed {
        left_out: reports
            .iter()
            .zip(&events)
            .find_map(|((report, _), events)| {
                left_out_unlisted(report, events.as_ref().ok()?)
                    .map(|why| format!("{}: {why}", report.task))
            }),
        ..Observed::default()
    };
    for (unit, saw) in merged {
        let mut not_followed = saw.not_followed.clone();
        let mut dirs = saw.dirs.clone();
        let mut reads: BTreeSet<String> = BTreeSet::new();
        // Other entries of the classpath inside the repository: a file of the scope is read, a
        // directory with files of the scope is read whole, and anything else (built output
        // outside every project's build directory) can't be told.
        for path in &saw.paths {
            let in_repository = saw.repositories.iter().find_map(|repo| {
                path.strip_prefix(repo.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
            });
            if tree.contains(path) {
                reads.insert(path.clone());
            } else if let Some(rest) = in_repository {
                // A jar of a file repository in the checkout, which git doesn't track and no
                // project of the build has the coordinates of.
                not_followed.insert(NotFollowed {
                    why: LOCAL_REPOSITORY.into(),
                    detail: coordinates_of(rest).unwrap_or_else(|| path.clone()),
                });
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
        let not_followed = one_local_repository_reason(not_followed);
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
                    skipped: saw.skipped,
                    never_left_out: saw.generated,
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
                                skipped: t.skipped,
                                never_left_out: t.never_left_out,
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

/// Why a class whose classpath holds a jar of a local Maven repository that no project of this
/// build made, and no remote repository supplied, isn't followed.
const LOCAL_REPOSITORY: &str = "local-maven-repository";

/// `group:artifact:version` from a jar's path below a Maven repository
/// (`com/acme/thing/1.0/thing-1.0.jar`), or `None`.
fn coordinates_of(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let n = parts.len();
    (n >= 4).then(|| {
        format!(
            "{}:{}:{}",
            parts[..n - 3].join("."),
            parts[n - 3],
            parts[n - 2]
        )
    })
}

/// \[ADR 0087\] One sentence, for `hushrun why`, in place of every `local-maven-repository` entry:
/// the jars, at most three named and how many more, and what a person can do.
fn one_local_repository_reason(not_followed: BTreeSet<NotFollowed>) -> BTreeSet<NotFollowed> {
    let (local, mut out): (BTreeSet<NotFollowed>, BTreeSet<NotFollowed>) = not_followed
        .into_iter()
        .partition(|n| n.why == LOCAL_REPOSITORY);
    if local.is_empty() {
        return out;
    }
    let jars: Vec<&str> = local.iter().map(|n| n.detail.as_str()).collect();
    let mut named = jars.iter().take(3).copied().collect::<Vec<_>>().join(", ");
    if jars.len() > 3 {
        named.push_str(&format!(" and {} more", jars.len() - 3));
    }
    out.insert(NotFollowed {
        why: LOCAL_REPOSITORY.into(),
        detail: format!(
            "hushrun doesn't know what built {named}, which the tests take from a local Maven repository: filter mavenLocal() to this build's own group, or, where a remote repository has the same coordinates (check it), put mavenLocal() after that repository"
        ),
    });
    out
}

/// \[C-30, ADR 0087\] The sources: every file of a JVM kind below one of `dirs`, the directories
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

/// \[ADR 0087\] The Gradle adapter for a COMMAND that starts Gradle, whose settings file loads it
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
            left_out: observed.left_out,
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
