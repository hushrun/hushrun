//! \[ADR 0087\] hushrun's `ctest`: the program first on COMMAND's `PATH` ([`super::super::program`])
//! that every `ctest` started by name reaches. For a test run it lists the tests with the real
//! `ctest`, leaves out those `hushrun.json` lets it (each checked again here), adds a JUnit file
//! of its own unless one is named, starts the real `ctest` with stdio, signals and the exit status
//! passed through, and writes one file of facts. Anything in doubt starts the real `ctest` with
//! its arguments unchanged.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::SystemTime;

use super::super::facts::{self, FactsFile, RunFile, TestFacts, Unfollowed, UnitFacts};
use super::super::program::{self, Call};
use super::args::{self, Mode};
use super::build::{self, Built, Walker};
use super::junit::{self, Outcome};
use super::select::{self, Program, SelectAt, Test};
use super::{ADAPTER, PLAN_FILE, PlanFile};

/// The program hushrun puts on `PATH` for the CTest adapter.
pub struct Ctest;

/// What [`Ctest`]'s [`args`](program::Program::args) leaves for its
/// [`ended`](program::Program::ended): one start per process.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// What a start of the real `ctest` was given, for reading what it did.
struct Pending {
    units_dir: PathBuf,
    tests: Vec<Test>,
    leave: BTreeSet<usize>,
    built: BTreeMap<String, Built>,
    junit: PathBuf,
    /// The JUnit file as it was before, when it is the user's: it must have changed.
    before: Option<Option<(SystemTime, u64)>>,
    started: SystemTime,
    retries: bool,
    report_tests: bool,
    reuse: BTreeSet<String>,
    /// How the tests were left out, for what a run's facts say: `-E`, `-R`,
    /// `--exclude-from-file`, or why none.
    selection: String,
}

impl program::Program for Ctest {
    fn name(&self) -> &'static str {
        "ctest"
    }

    fn args(&self, call: &Call<'_>) -> Vec<OsString> {
        debug_assert_eq!(call.name, "ctest");
        let Some(units) = call.units else {
            return call.args.to_vec();
        };
        match prepare(call, units) {
            Ok((args, pending)) => {
                if let Ok(mut slot) = PENDING.lock() {
                    *slot = Some(pending);
                }
                args
            }
            Err(Skip::Passthrough) => call.args.to_vec(),
            Err(Skip::Unobserved(why)) => {
                write_facts(units, Vec::new(), settings_why(&why), false);
                call.args.to_vec()
            }
        }
    }

    fn ended(&self, _: &Call<'_>, _: &[OsString], exit: Option<i32>) {
        let Some(pending) = PENDING.lock().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
        if exit.is_none() {
            write_facts(
                &pending.units_dir,
                Vec::new(),
                settings_why("ctest was stopped by a signal, so nothing it ran is known"),
                false,
            );
            return;
        }
        record(&pending);
    }
}

/// Why hushrun's `ctest` starts the real one unchanged.
enum Skip {
    /// Not a test run, or not hushrun's to read: nothing written.
    Passthrough,
    /// A test run hushrun can't observe, and why, which the run says once.
    Unobserved(String),
}

impl From<String> for Skip {
    fn from(why: String) -> Skip {
        Skip::Unobserved(why)
    }
}

/// The real `ctest`, or another tool, with the environment COMMAND had: `PATH` as it was.
fn tool(program: &Path) -> Command {
    let mut command = program::real_command(program);
    command.stdin(Stdio::null()).stderr(Stdio::null());
    command
}

/// `PATH` as COMMAND had it.
fn path_was() -> Option<OsString> {
    match program::path_was(std::env::var_os(program::PATH_WAS)) {
        Some(path) => path,
        None => std::env::var_os("PATH"),
    }
}

/// The real `ctest`'s listing with `args`: its tests, and the lines it printed before them.
pub fn list(real: &Path, args: &[String], cwd: &Path) -> Result<Vec<select::Listed>, String> {
    let output = tool(real)
        .args(args)
        .arg("--show-only=json-v1")
        .current_dir(cwd)
        .output()
        .map_err(|err| format!("can't start {}: {err}", real.display()))?;
    if !output.status.success() {
        return Err("ctest couldn't list its tests".into());
    }
    select::parse_listing(&output.stdout).map(|(listing, _)| listing.tests)
}

/// The real `ctest`'s version.
pub fn version(real: &Path) -> Result<(u32, u32, u32), String> {
    let output = tool(real)
        .arg("--version")
        .output()
        .map_err(|err| format!("can't start {}: {err}", real.display()))?;
    select::version(&output.stdout).ok_or_else(|| "ctest --version named no version".into())
}

/// The oldest CTest that writes a JUnit file.
pub const JUNIT_SINCE: (u32, u32, u32) = (3, 21, 0);

/// What a ctest command line in `cwd` lists, each test with its unit and id; `Err` why it can't
/// be told. The plan asks it too, before COMMAND.
pub fn listed(
    real: &Path,
    items: &[args::Item],
    cwd: &Path,
    root: &Path,
) -> Result<(Vec<Test>, Option<args::Preset>, PathBuf), String> {
    let version = version(real)?;
    if version < JUNIT_SINCE {
        return Err(format!(
            "ctest {}.{}.{} writes no JUnit file (--output-junit came in CMake 3.21)",
            version.0, version.1, version.2
        ));
    }
    let preset = match args::last(items, &["--preset"]) {
        Some(name) => Some(args::preset(name, cwd)?),
        None => None,
    };
    let test_dir = match args::last(items, &["--test-dir"]) {
        Some(dir) => cwd.join(dir),
        None => preset
            .as_ref()
            .and_then(|p| p.binary_dir.clone())
            .unwrap_or_else(|| cwd.to_path_buf()),
    };
    let tests = list(real, &args::listing(items, &[]), cwd)?;
    if tests.is_empty() {
        let multi = select::build_dir(&test_dir).multi_config;
        return Err(if multi && !args::has(items, &["-C", "--build-config"]) {
            "ctest lists no test of a multi-config build without -C".into()
        } else {
            "ctest listed no test".into()
        });
    }
    let config = args::last(items, &["-C", "--build-config"]);
    Ok((select::identify(tests, root, config), preset, test_dir))
}

/// What each unit's programs are built from, merged.
pub fn walk(tests: &[Test], root: &Path, path: Option<OsString>) -> BTreeMap<String, Built> {
    let mut walker = Walker::new(root, path);
    let mut out: BTreeMap<String, Built> = BTreeMap::new();
    let mut done: BTreeSet<PathBuf> = BTreeSet::new();
    let mut named_done: BTreeSet<(String, select::Named)> = BTreeSet::new();
    let merge =
        |out: &mut BTreeMap<String, Built>, unit: &str, built: Built| match out.get_mut(unit) {
            None => {
                out.insert(unit.to_string(), built);
            }
            Some(had) => {
                had.reads.extend(built.reads);
                had.own.extend(built.own);
                had.every_source |= built.every_source;
                had.stale = had.stale.take().or(built.stale);
            }
        };
    for test in tests {
        let Program::Built { path, build, named } = &test.program else {
            continue;
        };
        if done.insert(path.clone()) {
            let built = walker.program(path, build);
            merge(&mut out, &test.unit, built);
        }
        // What the test names besides: a program it starts, a file it reads.
        for n in named {
            if named_done.insert((test.unit.clone(), n.clone())) {
                let built = walker.named(n);
                merge(&mut out, &test.unit, built);
            }
        }
    }
    out
}

fn prepare(call: &Call<'_>, units: &Path) -> Result<(Vec<OsString>, Pending), Skip> {
    let run: RunFile = std::fs::read(units.join(facts::RUN_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|run: &RunFile| run.schema == facts::RUN_SCHEMA)
        .ok_or(Skip::Passthrough)?;
    let words: Vec<String> = call
        .args
        .iter()
        .map(|a| a.to_str().map(str::to_string))
        .collect::<Option<_>>()
        .ok_or_else(|| "ctest's arguments aren't all UTF-8".to_string())?;
    let items = args::parse(&words);
    let select = match args::mode(&items) {
        Mode::Passthrough => {
            return Err("ctest was started for something other than a test run"
                .to_string()
                .into());
        }
        Mode::Unread(why) => return Err(why.into()),
        Mode::Observe(_) => false,
        Mode::Select => true,
    };
    let root = std::fs::canonicalize(&run.root).map_err(|err| format!("the repository: {err}"))?;
    let cwd = std::env::current_dir().map_err(|err| format!("where ctest runs: {err}"))?;
    let (tests, preset, test_dir) = listed(call.real, &items, &cwd, &root)?;
    let built = walk(&tests, &root, path_was());
    let started = SystemTime::now();
    let nanos = started
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let own_dir = units.join(format!("ctest-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&own_dir).map_err(|err| format!("{}: {err}", own_dir.display()))?;

    let reuse: BTreeSet<String> = run.reuse.iter().cloned().collect();
    let mut leave = BTreeSet::new();
    let mut words_out: Vec<String> = words.clone();
    let mut selection = if select {
        "nothing listed".to_string()
    } else {
        "observed only".to_string()
    };
    if select && (!run.reuse.is_empty() || !run.reuse_tests.is_empty()) {
        let plan = plan_file(units);
        let verified: BTreeMap<&str, bool> = tests
            .iter()
            .map(|t| t.unit.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|unit| (unit, verify(unit, &tests, &built, plan.as_ref())))
            .collect();
        let planned = |t: &Test| {
            verified.get(t.unit.as_str()).copied().unwrap_or(false)
                && (reuse.contains(&t.unit)
                    || run
                        .reuse_tests
                        .get(&t.unit)
                        .is_some_and(|ids| ids.contains(&t.id)))
        };
        let candidate = select::left_out(&tests, &planned, &|unit| reuse.contains(unit));
        let chosen = (!candidate.is_empty())
            .then(|| {
                choose(
                    call.real,
                    &items,
                    &tests,
                    &candidate,
                    preset.as_ref(),
                    &cwd,
                    &own_dir,
                )
            })
            .flatten();
        selection = match chosen {
            Some((chosen, mechanism)) => {
                leave = candidate;
                words_out = chosen;
                mechanism.to_string()
            }
            None if candidate.is_empty() => "nothing could be left out".to_string(),
            None => "no way of leaving tests out listed exactly the tests expected".to_string(),
        };
    }

    // The JUnit file: the user's, or the preset's, as it is named; else one of hushrun's.
    let named = args::last(&items, &["--output-junit"])
        .map(str::to_string)
        .or_else(|| preset.as_ref().and_then(|p| p.junit.clone()));
    let (junit, before) = match named {
        Some(name) => {
            let path = test_dir.join(name);
            let before = std::fs::metadata(&path)
                .ok()
                .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
            (path, Some(before))
        }
        None => {
            let path = own_dir.join("junit.xml");
            words_out.push("--output-junit".into());
            words_out.push(path.to_string_lossy().into_owned());
            (path, None)
        }
    };
    let pending = Pending {
        units_dir: units.to_path_buf(),
        tests,
        leave,
        built,
        junit,
        before,
        started,
        retries: args::retries(&items),
        report_tests: run.unit.as_deref() == Some("test"),
        reuse,
        selection,
    };
    Ok((words_out.into_iter().map(OsString::from).collect(), pending))
}

/// The plan's file, in the run's directory, or for an audit's second run in the first run's.
fn plan_file(units: &Path) -> Option<(PlanFile, SystemTime)> {
    let mut places = vec![units.join(PLAN_FILE)];
    if units.file_name().is_some_and(|n| n == "again")
        && let Some(parent) = units.parent()
    {
        places.push(parent.join(PLAN_FILE));
    }
    places.into_iter().find_map(|path| {
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        let plan: PlanFile = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        (plan.schema == super::PLAN_SCHEMA).then_some((plan, modified))
    })
}

/// \[ADR 0087\] Whether a unit the plan lists may be left out, checked here, when `ctest` runs:
/// what stands in for binding a reused test program to its bytes, which the plan can't know
/// before COMMAND builds them. Every one of its tests' programs is built here and up to date, none
/// was built again after the plan, and the C and C++ files of the scope it is built from now are
/// exactly those of every list its key was made from: so the key held every file its code came
/// from, and the build that passed made it from the same ones.
fn verify(
    unit: &str,
    tests: &[Test],
    built: &BTreeMap<String, Built>,
    plan: Option<&(PlanFile, SystemTime)>,
) -> bool {
    let Some((plan, planned_at)) = plan else {
        return false;
    };
    let Some(lists) = plan.units.get(unit).filter(|l| !l.is_empty()) else {
        return false;
    };
    let Some(built) = built.get(unit) else {
        return false;
    };
    if built.stale.is_some() {
        return false;
    }
    for test in tests.iter().filter(|t| t.unit == unit) {
        let Program::Built { path, named, .. } = &test.program else {
            return false;
        };
        let programs = std::iter::once(path).chain(named.iter().filter_map(|n| match n {
            select::Named::Built { path, .. } => Some(path),
            _ => None,
        }));
        for program in programs {
            let rebuilt = std::fs::metadata(program)
                .and_then(|m| m.modified())
                .map_or(true, |m| m > *planned_at);
            if rebuilt {
                return false;
            }
        }
    }
    let scope: BTreeSet<&str> = plan.scope.iter().map(String::as_str).collect();
    let now: BTreeSet<&str> = if built.every_source {
        scope.clone()
    } else {
        built
            .reads
            .iter()
            .map(String::as_str)
            .filter(|f| build::is_source(f) && scope.contains(f))
            .collect()
    };
    lists.iter().all(|list| {
        let list: BTreeSet<&str> = list
            .iter()
            .map(String::as_str)
            .filter(|f| build::is_source(f))
            .collect();
        list == now
    })
}

/// The command line that leaves out exactly `leave`, by the first mechanism whose listing gives
/// back exactly the tests expected; `None` when none does, and then nothing is left out.
fn choose(
    real: &Path,
    items: &[args::Item],
    tests: &[Test],
    leave: &BTreeSet<usize>,
    preset: Option<&args::Preset>,
    cwd: &Path,
    own_dir: &Path,
) -> Option<(Vec<String>, &'static str)> {
    let left: BTreeSet<String> = leave
        .iter()
        .map(|i| tests[*i].listed.name.clone())
        .collect();
    let kept: BTreeSet<String> = tests
        .iter()
        .enumerate()
        .filter(|(i, _)| !leave.contains(i))
        .map(|(_, t)| t.listed.name.clone())
        .collect();
    let user_exclude = args::last(items, &["-E", "--exclude-regex"])
        .map(str::to_string)
        .or_else(|| {
            (!args::has(items, &["-E", "--exclude-regex"]))
                .then(|| preset.and_then(|p| p.exclude_name.clone()))
                .flatten()
        });
    let user_file: Option<Vec<String>> = match args::last(items, &["--exclude-from-file"]) {
        Some(file) => {
            let bytes = std::fs::read(cwd.join(file)).ok()?;
            Some(
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(|l| l.trim_end_matches('\r').to_string())
                    .collect(),
            )
        }
        None => None,
    };
    let version = version(real).ok()?;
    let file = own_dir.join("exclude.txt");
    let at = SelectAt {
        version,
        left: &left,
        kept: &kept,
        user_exclude: user_exclude.as_deref(),
        user_file: user_file.as_deref(),
        preset_file: preset.is_some_and(|p| p.exclude_file),
        file: &file,
    };
    let mut expected: Vec<(String, String)> = tests
        .iter()
        .enumerate()
        .filter(|(i, _)| !leave.contains(i))
        .map(|(_, t)| t.listed.identity())
        .collect();
    expected.sort();
    for selection in select::mechanisms(&at) {
        let mut words = args::without(items, &selection.drop);
        words.extend(selection.add.iter().cloned());
        if kept.is_empty() {
            // Everything left out: a pass stays a pass (ran: the last --no-tests wins).
            words.push("--no-tests=ignore".into());
        }
        let Ok(again) = list(real, &args::listing(&args::parse(&words), &[]), cwd) else {
            continue;
        };
        let mut got: Vec<(String, String)> = again.iter().map(select::Listed::identity).collect();
        got.sort();
        if got == expected {
            return Some((words, selection.mechanism.word()));
        }
    }
    None
}

/// What the run came to, as one file of facts.
fn record(p: &Pending) {
    let read = std::fs::metadata(&p.junit)
        .map_err(|_| "ctest wrote no JUnit file".to_string())
        .and_then(|meta| {
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if let Some(before) = &p.before
                && *before == Some((modified, meta.len()))
            {
                return Err("the JUnit file is the one from before this run".into());
            }
            let fresh = modified
                .checked_add(std::time::Duration::from_secs(2))
                .is_some_and(|m| m >= p.started);
            if !fresh {
                return Err("the JUnit file is older than this run".into());
            }
            std::fs::read(&p.junit).map_err(|err| err.to_string())
        })
        .and_then(|bytes| junit::read(&bytes));
    let names: Vec<&str> = p
        .tests
        .iter()
        .enumerate()
        .filter(|(i, _)| !p.leave.contains(i))
        .map(|(_, t)| t.listed.name.as_str())
        .collect();
    let outcomes = read
        .as_ref()
        .ok()
        .map(|cases| junit::matched(&names, cases));
    let mut outcome_of: BTreeMap<usize, (Outcome, u64)> = BTreeMap::new();
    if let Some(outcomes) = &outcomes {
        let ran = (0..p.tests.len()).filter(|i| !p.leave.contains(i));
        for (i, outcome) in ran.zip(outcomes.iter()) {
            outcome_of.insert(i, *outcome);
        }
    }
    let mut units: BTreeMap<String, UnitFacts> = BTreeMap::new();
    let mut own: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut every_source: Vec<String> = Vec::new();
    for (i, test) in p.tests.iter().enumerate() {
        let unit = units.entry(test.unit.clone()).or_insert_with(|| {
            let mut facts = UnitFacts {
                unit: test.unit.clone(),
                deselected: true,
                ..UnitFacts::default()
            };
            match &test.program {
                Program::Elsewhere(program) => facts.not_followed.push(Unfollowed {
                    why: "not built here".into(),
                    detail: program.clone(),
                }),
                Program::Built { .. } | Program::None => {}
            }
            if let Some(built) = p.built.get(&test.unit) {
                facts.reads = built.reads.iter().cloned().collect();
                if let Some(why) = &built.stale {
                    facts.not_followed.push(Unfollowed {
                        why: "stale program".into(),
                        detail: why.clone(),
                    });
                }
                own.insert(test.unit.clone(), built.own.iter().cloned().collect());
                if built.every_source {
                    every_source.push(test.unit.clone());
                }
            }
            if p.report_tests {
                facts.tests = Some(Vec::new());
            }
            facts
        });
        let left = p.leave.contains(&i);
        let disabled = test.listed.disabled();
        let (outcome, duration_ms) = if left {
            (Outcome::NotRun, 0)
        } else {
            outcome_of.get(&i).copied().unwrap_or((Outcome::NotRun, 0))
        };
        let finished = matches!(
            outcome,
            Outcome::Passed | Outcome::Failed | Outcome::Skipped
        );
        let failed = matches!(outcome, Outcome::Failed | Outcome::Unknown { failed: true });
        let mut not_followed = Vec::new();
        if p.retries && matches!(outcome, Outcome::Passed | Outcome::Skipped) {
            not_followed.push(Unfollowed {
                why: "repeat until-pass".into(),
                detail: "a pass under --repeat may have come after a failure".into(),
            });
        }
        // A whole unit is deselected when `reuse` listed it and every test of it was left out.
        unit.deselected &= left && p.reuse.contains(&test.unit);
        if !left && !disabled {
            unit.selected += 1;
            unit.finished += u64::from(finished);
        }
        unit.failed |= failed;
        unit.duration_ms = unit.duration_ms.saturating_add(duration_ms);
        match &mut unit.tests {
            Some(tests) => tests.push(TestFacts {
                id: test.id.clone(),
                finished,
                failed,
                deselected: left,
                duration_ms,
                not_followed,
                ..TestFacts::default()
            }),
            None => {
                for n in not_followed {
                    if !unit.not_followed.contains(&n) {
                        unit.not_followed.push(n);
                    }
                }
            }
        }
    }
    let mut settings = BTreeMap::new();
    settings.insert("own".to_string(), serde_json::json!(own));
    settings.insert("everySource".to_string(), serde_json::json!(every_source));
    settings.insert("selection".to_string(), serde_json::json!(p.selection));
    if let Err(why) = &read {
        settings.insert(
            "junit".to_string(),
            serde_json::json!(format!("ctest's JUnit file wasn't read: {why}")),
        );
    }
    write_facts(
        &p.units_dir,
        units.into_values().collect(),
        settings,
        p.report_tests,
    );
}

fn settings_why(why: &str) -> BTreeMap<String, serde_json::Value> {
    let mut settings = BTreeMap::new();
    settings.insert("notObserved".to_string(), serde_json::json!(why));
    settings
}

/// One file of facts in the run's directory, whole, under a name no other start has.
fn write_facts(
    dir: &Path,
    units: Vec<UnitFacts>,
    settings: BTreeMap<String, serde_json::Value>,
    tests: bool,
) {
    let file = FactsFile {
        schema: if tests {
            facts::FACTS_SCHEMA_V1
        } else {
            facts::FACTS_SCHEMA
        }
        .into(),
        adapter: ADAPTER.into(),
        failures_by_unit: true,
        settings,
        units,
        ..FactsFile::default()
    };
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = dir.join(format!("{ADAPTER}-{}-{nanos}.json", std::process::id()));
    if let Ok(bytes) = serde_json::to_vec(&file) {
        let _ = facts::write_whole(&path, &bytes);
    }
}
