//! \[ADR 0120\] A C or C++ test program COMMAND starts itself, by its path, not through CTest
//! (C-106, C-107, C-108): GoogleTest's and Catch2 3's. A unit is the program, named as CTest's
//! adapter names it (ADR 0090); a test is one of its tests, by its `--gtest_filter` or its Catch2
//! name. What the program read is what its build directory says it is built from, by CTest's
//! adapter's walk.
//!
//! - **Loading, with COMMAND byte for byte as written.** Neither framework is reached through
//!   `PATH`: COMMAND is the program. Both take what hushrun needs from the environment, which
//!   [`Adapter::start_variables`] sets for each start of COMMAND: GoogleTest `GTEST_OUTPUT` and
//!   `GTEST_FILTER`; Catch2 3 Bazel's test variables (`BAZEL_TEST`, `XML_OUTPUT_FILE`,
//!   `TESTBRIDGE_TEST_ONLY`, one shard of one). doctest takes nothing from the environment, and
//!   Catch2 2 nothing of this: their programs are said once not to be observed.
//! - **Leaving out, checked twice.** Just before COMMAND starts, hushrun lists the program's tests
//!   with COMMAND's own arguments, checks the program against the plan's walk (CTest's check,
//!   ADR 0090 decision 8), and lists again with the variable that leaves tests out: unless that
//!   listing names exactly the tests expected, nothing is left out. What it found is written as
//!   a facts file of the run's directory, which [`Adapter::read`] completes from the program's
//!   own XML after COMMAND.

mod catch2;
mod gtest;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use hushrun_core::units::{self, Sets, Sources};

use super::adapter::{
    Adapter, Hints, KeysOutside, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::ctest::{self, build, run, select};
use super::facts::{self, FactsFile, TestFacts, Unfollowed, UnitFacts};

/// The adapter's word, in its facts files, this machine's store and a hint.
pub const ADAPTER: &str = "test-program";
/// The facts file the start writes, for the read after COMMAND to complete.
const PENDING: &str = "pending";
/// The most bytes a variable that leaves tests out may hold: a larger one leaves nothing out.
const MOST_FILTER: usize = 100_000;

/// The frameworks a test program is observed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Framework {
    Gtest,
    Catch2,
}

impl Framework {
    fn name(self) -> &'static str {
        match self {
            Framework::Gtest => "GoogleTest",
            Framework::Catch2 => "Catch2",
        }
    }

    fn args(self, args: &[String]) -> Result<Option<String>, String> {
        match self {
            Framework::Gtest => gtest::args(args),
            Framework::Catch2 => catch2::args(args),
        }
    }

    fn env(self, get: &dyn Fn(&str) -> Option<OsString>) -> Result<Option<String>, String> {
        match self {
            Framework::Gtest => gtest::env(get),
            Framework::Catch2 => catch2::env(get),
        }
    }
}

/// What one test came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    /// Its body didn't run, as it asked (`GTEST_SKIP`, Catch2's `SKIP`): a pass, as elsewhere.
    Skipped,
    /// A disabled GoogleTest test: listed, never run.
    Disabled,
}

/// One case of a framework's XML file.
#[derive(Debug, Clone, PartialEq)]
struct Case {
    id: String,
    outcome: Outcome,
    duration_ms: u64,
}

/// An attribute's value, unescaped.
fn attribute(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Result<Option<String>, String> {
    for attr in e.attributes() {
        let attr = attr.map_err(|err| format!("an attribute doesn't parse: {err}"))?;
        if attr.key.local_name().as_ref() == name {
            let value = quick_xml::escape::unescape(&attr.value)
                .map_err(|err| format!("an attribute doesn't parse: {err}"))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

/// Seconds, as the files write them, in whole milliseconds.
fn millis(seconds: Option<&str>) -> u64 {
    seconds
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map_or(0, |s| (s * 1000.0).round() as u64)
}

/// Why a program of a framework hushrun can't observe isn't, or `None` when it is no test program
/// hushrun knows of. Each is found by words its own code holds.
fn detect(bytes: &[u8]) -> Option<Result<Framework, String>> {
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    if has(b"--gtest_list_tests") || has(b"_ZN7testing14InitGoogleTest") {
        return Some(Ok(Framework::Gtest));
    }
    if has(b"--allow-running-no-tests") || has(b"_ZN5Catch7Session") {
        if has(b"BAZEL_TEST") && has(b"TESTBRIDGE_TEST_ONLY") && has(b"XML_OUTPUT_FILE") {
            return Some(Ok(Framework::Catch2));
        }
        return Some(Err(
            "this Catch2 reads no variable that leaves tests out or writes what each came to (Catch2 3.3 and later read BAZEL_TEST); run it through ctest with catch_discover_tests".into(),
        ));
    }
    if has(b"--list-test-names-only") && has(b"Catch") {
        return Some(Err(
            "Catch2 2 takes what to run and where to report only from its command line; run it through ctest with catch_discover_tests".into(),
        ));
    }
    if has(b"dt-no-run") || has(b"doctest_filter") {
        return Some(Err(
            "doctest takes what to run and where to report only from its command line; run it through ctest with doctest_discover_tests".into(),
        ));
    }
    None
}

/// The program COMMAND starts, when it is a file of a CMake build directory: its real path.
fn program_of(first: &str, cwd: &Path) -> Option<PathBuf> {
    if !first.contains(['/', '\\']) {
        return None;
    }
    let real = std::fs::canonicalize(cwd.join(first)).ok()?;
    if !real.is_file() {
        return None;
    }
    select::build_root(&real)?;
    Some(real)
}

/// \[ADR 0120\] The adapter for a COMMAND that is a GoogleTest or Catch2 3 program of a CMake
/// build directory, started by its path (C-106): `Some(Err)` for one hushrun can't observe
/// (doctest's, Catch2 2's, or COMMAND's arguments), `None` for any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    let found = recognize(command, cwd)?;
    Some(found.map(|adapter| Box::new(adapter) as Box<dyn Adapter>))
}

/// [`recognized`], as the adapter's own type: what a test of it calls, with no trait object.
pub fn recognize(command: &[String], cwd: &Path) -> Option<Result<Recognized, String>> {
    let first = command.first()?;
    let program = program_of(first, cwd)?;
    let bytes = std::fs::read(&program).ok()?;
    let framework = match detect(&bytes)? {
        Ok(framework) => framework,
        Err(why) => return Some(Err(why)),
    };
    let args = command[1..].to_vec();
    Some(framework.args(&args).map(|observe| Recognized {
        program,
        framework,
        args,
        observe,
        started: OnceLock::new(),
    }))
}

/// The adapter for one COMMAND.
#[derive(Debug)]
pub struct Recognized {
    /// The program's real path.
    program: PathBuf,
    framework: Framework,
    /// COMMAND's arguments after the program.
    args: Vec<String>,
    /// Why COMMAND's arguments let nothing be left out, when they don't.
    observe: Option<String>,
    started: OnceLock<Started>,
}

/// What the start of the run said, which every start of COMMAND goes by.
#[derive(Debug)]
struct Started {
    cwd: PathBuf,
    /// \[C-32\] COMMAND's own `env NAME=value` assignments.
    assigned: Vec<(String, String)>,
    /// Why COMMAND's environment lets nothing be left out, when it doesn't.
    observe: Option<String>,
    /// The run's directory, where the plan writes its file.
    dir: PathBuf,
}

/// What a start found before COMMAND, kept in its facts file for the read after it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Pending {
    unit: String,
    /// Where the framework writes what each test came to.
    xml: PathBuf,
    /// When the start was, in milliseconds since the epoch.
    started_ms: u64,
    /// The tests the listing named, by id, in order: `None` when it couldn't be listed.
    listed: Option<Vec<String>>,
    /// Those left out.
    left: Vec<String>,
    /// The unit was left out whole, as `reuse` listed it.
    whole: bool,
    /// `hushrun.json` asked for each test (`unit: test`).
    report_tests: bool,
    /// How tests were left out, or why none were, for `hushrun explain`.
    selection: String,
    reads: Vec<String>,
    own: Vec<String>,
    every_source: bool,
    /// Every C and C++ file the build compiles into any of its programs.
    sources: Vec<String>,
    /// Why the program isn't up to date: `old` when it ran old code.
    stale: Option<(bool, String)>,
    refused: Option<String>,
}

/// What the walk says of the program now.
struct Walked {
    tests: Vec<select::Test>,
    built: BTreeMap<String, build::Built>,
    /// Every C and C++ file the build compiles into any of its programs.
    sources: BTreeSet<String>,
}

impl Recognized {
    /// The program as CTest's adapter names and walks a test's program: its arguments that name
    /// a program or a file count as a test's command's do.
    fn walk(&self, root: &Path, assigned: &[(String, String)]) -> Walked {
        let mut command = vec![self.program.to_string_lossy().into_owned()];
        command.extend(self.args.iter().cloned());
        let listed = select::Listed {
            name: self.program.to_string_lossy().into_owned(),
            command: Some(command),
            properties: Vec::new(),
        };
        let tests = select::identify(vec![listed], root, None);
        let built = run::walk(&tests, root, path_of(assigned), assigned);
        let sources = match tests.first().map(|t| &t.program) {
            Some(select::Program::Built { build, .. }) => {
                build::Walker::new(root, path_of(assigned), assigned).sources(build)
            }
            _ => BTreeSet::new(),
        };
        Walked {
            tests,
            built,
            sources,
        }
    }

    fn unit(walked: &Walked) -> String {
        walked
            .tests
            .first()
            .map(|t| t.unit.clone())
            .unwrap_or_default()
    }

    /// The program's tests, by id, listed with COMMAND's own arguments and `env`.
    fn list(&self, started: &Started, env: &[(&str, &str)]) -> Result<Vec<String>, String> {
        let args = match self.framework {
            Framework::Gtest => {
                let mut args = self.args.clone();
                args.push("--gtest_list_tests".into());
                args
            }
            Framework::Catch2 => catch2::listing_args(&self.args),
        };
        let mut command = super::command::tool(&self.program, &started.cwd, &started.assigned);
        command
            .args(&args)
            .envs(env.iter().copied())
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let out = command
            .output()
            .map_err(|err| format!("the program couldn't be started to list its tests: {err}"))?;
        if !out.status.success() {
            return Err(format!("listing its tests ended with {}", out.status));
        }
        match self.framework {
            Framework::Gtest => Ok(gtest::listing(&String::from_utf8_lossy(&out.stdout))),
            Framework::Catch2 => catch2::listing(&out.stdout),
        }
    }

    /// The variables every start gets, the xml file's path, and the one that leaves tests out.
    fn variables_for(&self, dir: &Path, xml: &Path) -> Vec<(String, Option<OsString>)> {
        match self.framework {
            Framework::Gtest => {
                let mut output = OsString::from("xml:");
                output.push(xml);
                vec![("GTEST_OUTPUT".into(), Some(output))]
            }
            Framework::Catch2 => vec![
                ("BAZEL_TEST".into(), Some("1".into())),
                ("XML_OUTPUT_FILE".into(), Some(xml.into())),
                ("TEST_SHARD_INDEX".into(), Some("0".into())),
                ("TEST_TOTAL_SHARDS".into(), Some("1".into())),
                (
                    "TEST_SHARD_STATUS_FILE".into(),
                    Some(dir.join(format!("{ADAPTER}.shard")).into()),
                ),
            ],
        }
    }

    /// The variable that leaves tests out, its value for `left`, or `None` when none can be.
    fn leave_out(&self, left: &[&str], whole: bool) -> Option<(&'static str, String)> {
        match self.framework {
            Framework::Gtest => {
                if !left.iter().all(|id| gtest::nameable(id)) {
                    return None;
                }
                Some(("GTEST_FILTER", gtest::filter(left, whole)))
            }
            Framework::Catch2 => {
                if !left.iter().all(|n| catch2::nameable(n)) {
                    return None;
                }
                Some(("TESTBRIDGE_TEST_ONLY", catch2::spec(left)))
            }
        }
    }

    /// \[ADR 0120\] What this start leaves out, and how, or why nothing.
    fn choose(
        &self,
        started: &Started,
        run_dir: &Path,
        run: &facts::RunFile,
        walked: &Walked,
        listed: &[String],
        unit: &str,
    ) -> (Vec<String>, bool, Option<(&'static str, String)>, String) {
        let nothing = |why: &str| (Vec::new(), false, None, why.to_string());
        if let Some(why) = self.observe.as_ref().or(started.observe.as_ref()) {
            return nothing(&format!("observed only: {why}"));
        }
        let whole = run.reuse.iter().any(|u| u == unit);
        let asked: BTreeSet<&str> = run
            .reuse_tests
            .get(unit)
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        if !whole && asked.is_empty() {
            return nothing("nothing listed");
        }
        let plan = run::plan_file(run_dir);
        if !run::verify(unit, &walked.tests, &walked.built, plan.as_ref()) {
            return nothing(
                "the program isn't the one the plan keyed: not built here, not up to date, built after the plan, or built from other files",
            );
        }
        let left: Vec<&str> = listed
            .iter()
            .map(String::as_str)
            .filter(|id| whole || asked.contains(id))
            .collect();
        if left.is_empty() {
            return nothing("nothing listed is a test of the program");
        }
        // Catch2 ends in failure when it runs no test: a program left out whole runs whole.
        if self.framework == Framework::Catch2 && left.len() == listed.len() {
            return nothing("Catch2 fails when it runs no test, so a program left out whole runs");
        }
        let whole = whole && left.len() == listed.len();
        let Some((name, value)) = self.leave_out(&left, whole) else {
            return nothing("a test's name can't be named exactly");
        };
        if value.len() > MOST_FILTER {
            return nothing("the variable that would leave the tests out is too long");
        }
        // Listed again with the variable: exactly the tests expected, or nothing is left out.
        let mut env: Vec<(&str, &str)> = vec![(name, value.as_str())];
        if self.framework == Framework::Catch2 {
            env.extend([
                ("BAZEL_TEST", "1"),
                ("TEST_SHARD_INDEX", "0"),
                ("TEST_TOTAL_SHARDS", "1"),
            ]);
        }
        let shard = run_dir.join(format!("{ADAPTER}.shard"));
        let shard = shard.to_string_lossy().into_owned();
        if self.framework == Framework::Catch2 {
            env.push(("TEST_SHARD_STATUS_FILE", shard.as_str()));
        }
        let expected: BTreeSet<&str> = listed
            .iter()
            .map(String::as_str)
            .filter(|id| !left.contains(id))
            .collect();
        match self.list(started, &env) {
            Ok(again) if again.iter().map(String::as_str).collect::<BTreeSet<_>>() == expected => {
                let left = left.iter().map(|s| s.to_string()).collect();
                (left, whole, Some((name, value)), name.to_string())
            }
            Ok(_) => nothing(&format!(
                "listed with {name}, the program didn't name exactly the tests expected"
            )),
            Err(why) => nothing(&why),
        }
    }
}

/// `PATH` as COMMAND will have it: its own assignment, else hushrun's.
fn path_of(assigned: &[(String, String)]) -> Option<OsString> {
    assigned
        .iter()
        .rev()
        .find(|(n, _)| n == "PATH")
        .map(|(_, v)| OsString::from(v))
        .or_else(|| std::env::var_os("PATH"))
}

fn now_ms(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        self.framework.name()
    }

    fn test_file(&self) -> &'static str {
        "test program"
    }

    fn tests(&self) -> super::adapter::KnowsTests {
        super::adapter::KnowsTests {
            unit: true,
            reads: false,
        }
    }

    // `hints`, `binds` and `keys` are the trait's defaults, written out: a call on this type then
    // names this impl's own method, which the static test map follows to it alone, where the
    // trait's would count every adapter that overrides it (CONTRIBUTING.md, "Which tests CI runs
    // again").
    fn hints(&self) -> Hints {
        Hints::Sources
    }

    fn binds(&self) -> bool {
        false
    }

    fn keys(&self) -> KeysOutside {
        KeysOutside::None
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let observe = self.framework.env(at.env)?;
        let _ = self.started.set(Started {
            cwd: at.cwd.to_path_buf(),
            assigned: at.assigned.to_vec(),
            observe,
            dir: at.dir.to_path_buf(),
        });
        Ok(Vec::new())
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        Ok(Some(ctest::planning(at, None)))
    }

    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        // \[C-106\] A program that can only be observed: every test runs, and each is recorded.
        let observe = self.observe.as_ref();
        if let Some(why) = observe.or(self.started.get().and_then(|s| s.observe.as_ref())) {
            return Err(why.clone());
        }
        let root =
            std::fs::canonicalize(at.root).map_err(|err| format!("the repository: {err}"))?;
        let walked = self.walk(&root, at.assigned);
        let programs: BTreeMap<&str, bool> = walked
            .tests
            .iter()
            .map(|t| {
                (
                    t.unit.as_str(),
                    matches!(t.program, select::Program::Built { .. }),
                )
            })
            .collect();
        let mut compiled = ctest::compiled_from(&programs, &walked.built, at.tree);
        compiled.name(
            walked
                .sources
                .iter()
                .filter(|f| at.tree.contains(f))
                .cloned(),
        );
        let planning = ctest::planning(at, Some(&compiled));
        if let Some(started) = self.started.get() {
            ctest::write_plan(&started.dir, &planning, at.tree);
        }
        Ok(Some(planning))
    }

    fn start_variables(&self, run_dir: &Path) -> Vec<(String, Option<OsString>)> {
        let Some(started) = self.started.get() else {
            return Vec::new();
        };
        let Some(run) = std::fs::read(run_dir.join(facts::RUN_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<facts::RunFile>(&bytes).ok())
            .filter(|run| run.schema == facts::RUN_SCHEMA)
        else {
            return Vec::new();
        };
        let Ok(root) = std::fs::canonicalize(&run.root) else {
            return Vec::new();
        };
        let xml = run_dir.join(format!("{ADAPTER}.xml"));
        let _ = std::fs::remove_file(&xml);
        let mut vars = self.variables_for(run_dir, &xml);
        let walked = self.walk(&root, &started.assigned);
        let unit = Self::unit(&walked);
        let built = walked.built.get(&unit).cloned().unwrap_or_default();
        let listed = self.list(started, &[]);
        let (left, whole, filter, selection) = match &listed {
            Ok(listed) => self.choose(started, run_dir, &run, &walked, listed, &unit),
            Err(why) => (Vec::new(), false, None, why.clone()),
        };
        // A variable of hushrun's: set when it leaves tests out, else removed. The user's own
        // (an observed GTEST_FILTER) stays as it was.
        let owned = match self.framework {
            Framework::Gtest => started.observe.is_none().then_some("GTEST_FILTER"),
            Framework::Catch2 => Some("TESTBRIDGE_TEST_ONLY"),
        };
        match filter {
            Some((name, value)) => vars.push((name.to_string(), Some(value.into()))),
            None => vars.extend(owned.map(|name| (name.to_string(), None))),
        }
        let pending = Pending {
            unit,
            xml,
            started_ms: now_ms(SystemTime::now()),
            listed: listed.ok(),
            left,
            whole,
            report_tests: run.unit.as_deref() == Some("test"),
            selection,
            reads: built.reads.iter().cloned().collect(),
            own: built.own.iter().cloned().collect(),
            every_source: built.every_source,
            sources: walked.sources.iter().cloned().collect(),
            stale: built.stale.as_ref().map(|s| {
                let why = s
                    .why()
                    .replace("build before ctest", "build before it runs");
                (matches!(s, build::Stale::Old(_)), why)
            }),
            refused: built.refused.clone(),
        };
        let mut settings = BTreeMap::new();
        settings.insert(PENDING.to_string(), serde_json::json!(pending));
        let file = FactsFile {
            schema: facts::FACTS_SCHEMA.into(),
            adapter: ADAPTER.into(),
            settings,
            ..FactsFile::default()
        };
        if let Ok(bytes) = serde_json::to_vec(&file) {
            let _ = facts::write_whole(&run_dir.join(format!("{ADAPTER}-start.json")), &bytes);
        }
        vars
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let pending: Pending = at
            .files
            .iter()
            .filter(|f| f.adapter == ADAPTER)
            .find_map(|f| f.settings.get(PENDING))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .ok_or_else(|| {
                format!(
                    "hushrun couldn't ready {} before COMMAND started",
                    self.framework.name()
                )
            })?;
        let file = complete(&pending, self.framework, at.command_passed);
        let left_out = file
            .settings
            .get("leftOut")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut read = ctest::read_built(at, std::slice::from_ref(&file), ADAPTER)?;
        if let Some(read) = &mut read {
            read.left_out = left_out;
            read.stale_build = file.stale_build.clone();
        }
        Ok(read)
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras::default()
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        units::classify(
            at.tree.paths(),
            &|p| at.map.unit_files.contains(p),
            Sources::Named(&at.map.sources, ctest::SOURCE_KINDS),
            at.ignore,
        )
    }

    fn why_is_unit(&self, at: &WhyAt<'_>, _: &Sets, unit: &str) -> bool {
        at.map.units.contains_key(unit) || at.shared.knows(unit)
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't the test program {check} runs as far as this machine knows: its last run here didn't start it"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("its test program and what it is built from are those of its last run here")
    }

    fn why_asks_hints(&self, map: &super::store::Map) -> bool {
        !map.sources.is_empty()
    }

    fn why_no_path(&self) -> Option<&'static str> {
        Some("its test program is built from it: its build directory's records name it")
    }
}

/// \[ADR 0120\] The facts of a start, from what it found before COMMAND and the XML file the
/// framework wrote: each listed test's outcome, nothing read as a pass from absence.
fn complete(p: &Pending, framework: Framework, command_passed: bool) -> FactsFile {
    let cases = std::fs::metadata(&p.xml)
        .and_then(|m| m.modified())
        .map_err(|_| "the program wrote no XML file".to_string())
        .and_then(|modified| {
            let started = SystemTime::UNIX_EPOCH + Duration::from_millis(p.started_ms);
            let fresh = modified
                .checked_add(Duration::from_secs(2))
                .is_some_and(|m| m >= started);
            if !fresh {
                return Err("the XML file is older than this run".into());
            }
            std::fs::read(&p.xml).map_err(|err| err.to_string())
        })
        .and_then(|bytes| match framework {
            Framework::Gtest => gtest::cases(&bytes),
            Framework::Catch2 => catch2::cases(&bytes),
        });
    let listed: Vec<String> = p.listed.clone().unwrap_or_default();
    // Each listed test's outcome and time, merged over its cases (Catch2's sections).
    let mut outcome_of: BTreeMap<&str, (Outcome, u64)> = BTreeMap::new();
    let mut errors = Vec::new();
    if let Ok(cases) = &cases {
        for case in cases {
            let test = match framework {
                Framework::Gtest => listed.iter().find(|l| **l == case.id).map(String::as_str),
                Framework::Catch2 => catch2::test_of(&case.id, &listed),
            };
            let Some(test) = test else {
                if case.outcome == Outcome::Failed {
                    errors.push(if case.id.is_empty() {
                        format!(
                            "{} failed outside every test (an environment's or a suite's setup)",
                            framework.name()
                        )
                    } else {
                        format!("{}, which the listing didn't name, failed", case.id)
                    });
                }
                continue;
            };
            let entry = outcome_of.entry(test).or_insert((case.outcome, 0));
            entry.0 = match (entry.0, case.outcome) {
                (Outcome::Failed, _) | (_, Outcome::Failed) => Outcome::Failed,
                (Outcome::Passed, _) | (_, Outcome::Passed) => Outcome::Passed,
                (a, _) => a,
            };
            entry.1 = entry.1.saturating_add(case.duration_ms);
        }
    }
    let left: BTreeSet<&str> = p.left.iter().map(String::as_str).collect();
    let mut unit = UnitFacts {
        unit: p.unit.clone(),
        deselected: p.whole,
        reads: p.reads.clone(),
        tests: p.report_tests.then(Vec::new),
        ..UnitFacts::default()
    };
    if let Some(why) = &p.refused {
        unit.not_followed.push(Unfollowed {
            why: "in-source build".into(),
            detail: why.clone(),
        });
    }
    if let Some((_, why)) = &p.stale {
        unit.not_followed.push(Unfollowed {
            why: "stale program".into(),
            detail: why.clone(),
        });
    }
    if p.listed.is_none() {
        unit.not_followed.push(Unfollowed {
            why: "not listed".into(),
            detail: p.selection.clone(),
        });
    }
    let mut kept_absent: Option<&str> = None;
    let mut any_failed = false;
    for id in &listed {
        let is_left = left.contains(id.as_str());
        let (outcome, duration_ms) = match outcome_of.get(id.as_str()) {
            Some(&(outcome, ms)) if !is_left => (Some(outcome), ms),
            _ => (None, 0),
        };
        let disabled = outcome == Some(Outcome::Disabled);
        let finished = matches!(
            outcome,
            Some(Outcome::Passed | Outcome::Failed | Outcome::Skipped)
        );
        let failed = outcome == Some(Outcome::Failed);
        any_failed |= failed;
        let skipped =
            !is_left && matches!(outcome, Some(Outcome::Skipped) | Some(Outcome::Disabled));
        if !is_left && !disabled {
            unit.selected += 1;
            unit.finished += u64::from(finished);
            if outcome.is_none() && kept_absent.is_none() {
                kept_absent = Some(id);
            }
        }
        if unit.tests.is_none() {
            unit.skipped += u64::from(skipped);
        }
        unit.failed |= failed;
        unit.duration_ms = unit.duration_ms.saturating_add(duration_ms);
        if let Some(tests) = &mut unit.tests {
            tests.push(TestFacts {
                id: id.clone(),
                finished,
                failed,
                deselected: is_left,
                duration_ms,
                skipped,
                ..TestFacts::default()
            });
        }
    }
    let mut settings = BTreeMap::new();
    settings.insert("own".to_string(), serde_json::json!({ &p.unit: p.own }));
    let every: Vec<&str> = p
        .every_source
        .then_some(p.unit.as_str())
        .into_iter()
        .collect();
    settings.insert("everySource".to_string(), serde_json::json!(every));
    settings.insert("selection".to_string(), serde_json::json!(p.selection));
    settings.insert("sources".to_string(), serde_json::json!(p.sources));
    if let Err(why) = &cases {
        settings.insert(
            "xml".to_string(),
            serde_json::json!(format!(
                "{}'s XML file wasn't read: {why}",
                framework.name()
            )),
        );
    }
    // \[C-32\] Something was left out, and the run says it didn't go as expected: a kept test
    // GoogleTest didn't run, or a COMMAND that failed with no test failing (Catch2 fails when
    // every test it ran was skipped). The run starts COMMAND again with nothing left out.
    if !left.is_empty() && cases.is_ok() {
        let why = match (framework, kept_absent) {
            (Framework::Gtest, Some(id)) => Some(format!(
                "GoogleTest didn't run {id}, which hushrun didn't leave out"
            )),
            _ if !command_passed && !any_failed && errors.is_empty() => Some(format!(
                "{} failed with tests left out and no test failing",
                framework.name()
            )),
            _ => None,
        };
        if let Some(why) = why {
            settings.insert("leftOut".to_string(), serde_json::json!(why));
        }
    }
    let stale_build = p
        .stale
        .as_ref()
        .filter(|(old, _)| *old && unit.selected > 0)
        .map(|(_, why)| format!("{} ran old code: {why}", p.unit));
    FactsFile {
        schema: if p.report_tests {
            facts::FACTS_SCHEMA_V1
        } else {
            facts::FACTS_SCHEMA
        }
        .into(),
        adapter: ADAPTER.into(),
        failures_by_unit: true,
        settings,
        units: vec![unit],
        start: Some(format!("{ADAPTER}-{}", p.started_ms)),
        stale_build,
        errors,
        ..FactsFile::default()
    }
}

#[cfg(test)]
mod tests;
