//! \[ADR 0119\] hushrun's `meson`: the program first on COMMAND's `PATH`
//! ([`super::super::program`]) that every `meson` started by name reaches. For `meson test` it
//! lists the tests with the real `meson`, leaves out those `hushrun.json` lets it (each checked
//! again here), starts the real `meson` with stdio, signals and the exit status passed through,
//! and writes one file of facts from Meson's own `testlog.json`. Anything in doubt starts the real
//! `meson` with its arguments unchanged. Every other subcommand passes through (`meson setup`
//! with the build launcher's `CC` and `CXX`, B-236).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::SystemTime;

use super::super::ctest::build::{self, Built, Stale, Walker};
use super::super::facts::{self, FactsFile, RunFile, TestFacts, Unfollowed, UnitFacts};
use super::super::program::{self, Call};
use super::{ADAPTER, PLAN_FILE, PlanFile};

/// The program hushrun puts on `PATH` for the `meson test` adapter.
pub struct Meson;

/// What [`Meson`]'s [`args`](program::Program::args) leaves for its
/// [`ended`](program::Program::ended): one start per process.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// One test as `meson test --list` names it, with its program.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Test {
    /// Its name as listed: its id.
    name: String,
    /// Its unit: its program's path from the build folder, with the build type.
    unit: String,
    /// Its program, when the build folder makes it; else what its command starts.
    program: Result<PathBuf, String>,
    /// Its suites.
    suites: Vec<String>,
}

/// What a start of the real `meson test` was given, for reading what it did.
struct Pending {
    units_dir: PathBuf,
    root: PathBuf,
    wd: PathBuf,
    tests: Vec<Test>,
    leave: BTreeSet<usize>,
    log: PathBuf,
    before: Option<(SystemTime, u64)>,
    started: SystemTime,
    report_tests: bool,
    reuse: BTreeSet<String>,
    selection: String,
}

impl program::Program for Meson {
    fn name(&self) -> &'static str {
        "meson"
    }

    fn args(&self, call: &Call<'_>) -> Vec<OsString> {
        let Some(units) = call.units else {
            return call.args.to_vec();
        };
        if call.args.first().is_none_or(|a| a != "test") {
            return call.args.to_vec();
        }
        match prepare(call, units) {
            Ok((args, pending)) => {
                if let Ok(mut slot) = PENDING.lock() {
                    *slot = Some(pending);
                }
                args
            }
            Err(Skip::Passthrough) => call.args.to_vec(),
            Err(Skip::Unobserved(why)) => {
                write_facts(units, Vec::new(), settings_why(&why), false, None);
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
                settings_why("meson test was stopped by a signal, so nothing it ran is known"),
                false,
                None,
            );
            return;
        }
        record(&pending);
    }
}

/// Why hushrun's `meson` starts the real one unchanged.
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

/// `meson test`'s options that take a value, as separate words.
const VALUED: &[&str] = &[
    "--maxfail",
    "--repeat",
    "--gdb-path",
    "--wrapper",
    "-C",
    "--suite",
    "--no-suite",
    "--logbase",
    "-j",
    "--num-processes",
    "-t",
    "--timeout-multiplier",
    "--setup",
    "--test-args",
];

/// What `meson test`'s arguments say, its words after `test`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    /// The build folder, `-C`'s value.
    wd: Option<String>,
    /// The tests named, by name.
    names: Vec<String>,
    /// `--logbase`'s value.
    logbase: Option<String>,
    /// Something that runs no test as usual, or that hushrun doesn't read: why.
    passthrough: Option<&'static str>,
    /// `--no-rebuild`.
    no_rebuild: bool,
}

/// \[C-102\] Reads `meson test`'s words.
fn parse(words: &[String]) -> Args {
    let mut out = Args::default();
    let mut i = 0;
    while i < words.len() {
        let word = words[i].as_str();
        let (name, inline) = match word.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (word, None),
        };
        // `-C<dir>`, `-j4`, `-t2`.
        let (name, inline) = match name {
            n if inline.is_none() && n.len() > 2 && !n.starts_with("--") && n.starts_with('-') => {
                let (flag, rest) = n.split_at(2);
                if VALUED.contains(&flag) {
                    (flag, Some(rest.to_string()))
                } else {
                    (n, None)
                }
            }
            n => (n, inline),
        };
        let value = if VALUED.contains(&name) {
            match inline {
                Some(v) => Some(v),
                None => {
                    i += 1;
                    words.get(i).cloned()
                }
            }
        } else {
            None
        };
        match name {
            "-C" => out.wd = value,
            "--logbase" => out.logbase = value,
            "--no-rebuild" => out.no_rebuild = true,
            "--list" => out.passthrough = Some("list"),
            "--gdb" => out.passthrough = Some("gdb"),
            "--benchmark" => out.passthrough = Some("benchmark"),
            "-h" | "--help" => out.passthrough = Some("help"),
            "-i" | "--interactive" => out.passthrough = Some("interactive"),
            n if n.starts_with('-') => {}
            n => out.names.push(n.to_string()),
        }
        i += 1;
    }
    out
}

/// `PATH` as COMMAND had it.
fn path_was() -> Option<OsString> {
    match program::path_was(std::env::var_os(program::PATH_WAS)) {
        Some(path) => path,
        None => std::env::var_os("PATH"),
    }
}

/// The real `meson`, with the environment COMMAND had.
fn tool(real: &Path, cwd: &Path) -> Command {
    let mut command = super::super::command::tool(real, cwd, &[]);
    program::as_command_had(&mut command);
    command.stdin(Stdio::null()).stderr(Stdio::null());
    command
}

/// The tests `meson test <words> --list` names, in its order.
fn list(real: &Path, words: &[String], cwd: &Path) -> Result<Vec<String>, String> {
    let out = tool(real, cwd)
        .arg("test")
        .args(words)
        .args(["--list", "--no-rebuild"])
        .output()
        .map_err(|err| format!("meson test --list: {err}"))?;
    if !out.status.success() {
        return Err("meson test --list failed".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// One test of Meson's own record of a build folder's tests (`meson-info/intro-tests.json`).
#[derive(Debug, serde::Deserialize)]
struct IntroTest {
    name: String,
    #[serde(default)]
    suite: Vec<String>,
    #[serde(default)]
    cmd: Vec<String>,
}

/// The build folder's build type, from Meson's record of its options.
fn build_type(wd: &Path) -> Option<String> {
    let text = std::fs::read(wd.join("meson-info/intro-buildoptions.json")).ok()?;
    let options: Vec<serde_json::Value> = serde_json::from_slice(&text).ok()?;
    options
        .iter()
        .find(|o| o.get("name").and_then(|n| n.as_str()) == Some("buildtype"))
        .and_then(|o| o.get("value"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// \[C-102\] Each listed test with its unit and program, from the build folder `wd`'s record.
/// A listed name the record doesn't hold once is an `Err`: which program it runs can't be told.
fn identify(listed: &[String], wd: &Path) -> Result<Vec<Test>, String> {
    let text = std::fs::read(wd.join("meson-info/intro-tests.json"))
        .map_err(|err| format!("Meson's record of its tests can't be read: {err}"))?;
    let intro: Vec<IntroTest> = serde_json::from_slice(&text)
        .map_err(|err| format!("Meson's record of its tests can't be read: {err}"))?;
    let build_type = build_type(wd).unwrap_or_else(|| "plain".into());
    let wd_real = std::fs::canonicalize(wd).unwrap_or_else(|_| wd.to_path_buf());
    let mut out = Vec::new();
    for name in listed {
        // A subproject's test is listed `<project>:<name>`.
        let matches: Vec<&IntroTest> = intro
            .iter()
            .filter(|t| {
                t.name == *name
                    || name.split_once(':').is_some_and(|(p, n)| {
                        t.name == n
                            && t.suite
                                .iter()
                                .any(|s| s == p || s.starts_with(&format!("{p}:")))
                    })
            })
            .collect();
        let [test] = matches.as_slice() else {
            return Err(format!(
                "meson test --list names {name}, which Meson's record of its tests holds {} times",
                matches.len()
            ));
        };
        // A program of the build folder may not be built yet: `meson test` builds it first.
        let first = test.cmd.first().cloned().unwrap_or_default();
        let given = PathBuf::from(&first);
        let path = std::fs::canonicalize(&given).unwrap_or_else(|_| given.clone());
        let inside = path
            .strip_prefix(&wd_real)
            .or_else(|_| given.strip_prefix(&wd_real))
            .or_else(|_| given.strip_prefix(wd))
            .ok()
            .filter(|_| given.is_absolute() && !path.is_dir());
        let program = match inside {
            Some(rel) => Ok((wd_real.join(rel), rel.to_path_buf())),
            None => Err(first.clone()),
        };
        let (unit, program) = match program {
            Ok((path, rel)) => (
                format!(
                    "{} ({build_type})",
                    super::super::ctest::select::slashed(&rel)
                ),
                Ok(path),
            ),
            Err(first) => {
                let shown = Path::new(&first)
                    .file_name()
                    .map_or(first.clone(), |n| n.to_string_lossy().into_owned());
                (shown, Err(first))
            }
        };
        out.push(Test {
            name: name.clone(),
            unit,
            program,
            suites: test.suite.clone(),
        });
    }
    Ok(out)
}

/// What each built unit is built from, by its unit.
fn walk(tests: &[Test], root: &Path, wd: &Path) -> BTreeMap<String, Built> {
    let mut walker = Walker::new(root, path_was(), &[]);
    let mut out = BTreeMap::new();
    for test in tests {
        let Ok(program) = &test.program else {
            continue;
        };
        if out.contains_key(&test.unit) {
            continue;
        }
        out.insert(test.unit.clone(), walker.program(program, wd));
    }
    out
}

fn prepare(call: &Call<'_>, units: &Path) -> Result<(Vec<OsString>, Pending), Skip> {
    let run: RunFile = std::fs::read(units.join(facts::RUN_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|run: &RunFile| run.schema == facts::RUN_SCHEMA)
        .ok_or(Skip::Passthrough)?;
    let words: Vec<String> = call.args[1..]
        .iter()
        .map(|a| a.to_str().map(str::to_string))
        .collect::<Option<_>>()
        .ok_or_else(|| "meson test's arguments aren't all UTF-8".to_string())?;
    let args = parse(&words);
    if args.passthrough.is_some() {
        return Err(Skip::Passthrough);
    }
    let root = std::fs::canonicalize(&run.root).map_err(|err| format!("the repository: {err}"))?;
    let cwd = std::env::current_dir().map_err(|err| format!("where meson runs: {err}"))?;
    let wd = cwd.join(args.wd.as_deref().unwrap_or("."));
    if !build::is_meson(&wd) {
        return Err(format!("{} isn't a build folder Meson set up", wd.display()).into());
    }
    let listed = list(call.real, &words, &cwd)?;
    let tests = identify(&listed, &wd)?;
    let built = walk(&tests, &root, &wd);
    let started = SystemTime::now();
    let log = wd.join("meson-logs").join(format!(
        "{}.json",
        args.logbase.as_deref().unwrap_or("testlog")
    ));
    let before = std::fs::metadata(&log)
        .ok()
        .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));

    let reuse: BTreeSet<String> = run.reuse.iter().cloned().collect();
    let mut leave = BTreeSet::new();
    let mut words_out = words.clone();
    let mut selection = "nothing listed".to_string();
    if !run.reuse.is_empty() || !run.reuse_tests.is_empty() {
        let plan = plan_file(units);
        let verified: BTreeMap<&str, bool> = tests
            .iter()
            .map(|t| t.unit.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|unit| (unit, verify(unit, &tests, &built, plan.as_ref())))
            .collect();
        let candidate: BTreeSet<usize> = tests
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                verified.get(t.unit.as_str()).copied().unwrap_or(false)
                    && (reuse.contains(&t.unit)
                        || run
                            .reuse_tests
                            .get(&t.unit)
                            .is_some_and(|ids| ids.contains(&t.name)))
            })
            .map(|(i, _)| i)
            .collect();
        if candidate.is_empty() {
            selection = "nothing could be left out".into();
        } else {
            match chosen(call.real, &words, &args, &tests, &candidate, &cwd) {
                Some(chosen) => {
                    leave = candidate;
                    words_out = chosen;
                    selection = "names".into();
                }
                None => {
                    selection =
                        "no way of leaving tests out listed exactly the tests expected".into();
                }
            }
        }
    }
    let pending = Pending {
        units_dir: units.to_path_buf(),
        root,
        wd,
        tests,
        leave,
        log,
        before,
        started,
        report_tests: run.unit.as_deref() == Some("test"),
        reuse,
        selection,
    };
    let mut out = vec![OsString::from("test")];
    out.extend(words_out.into_iter().map(OsString::from));
    Ok((out, pending))
}

/// \[C-104\] The words that run the tests not in `leave`: COMMAND's own options, and the names of
/// the tests kept in place of the ones it named; with none kept, `--no-suite` for each suite of
/// the tests listed, which runs none and passes. `None` unless a listing with those words gives
/// back exactly the tests kept: then nothing is left out.
fn chosen(
    real: &Path,
    words: &[String],
    args: &Args,
    tests: &[Test],
    leave: &BTreeSet<usize>,
    cwd: &Path,
) -> Option<Vec<String>> {
    let kept: Vec<&str> = tests
        .iter()
        .enumerate()
        .filter(|(i, _)| !leave.contains(i))
        .map(|(_, t)| t.name.as_str())
        .collect();
    // A name given twice, or that two tests answer to, can't leave one of them out.
    let mut options: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let word = &words[i];
        if args.names.iter().any(|n| n == word) && !word.starts_with('-') {
            i += 1;
            continue;
        }
        options.push(word.clone());
        if VALUED.contains(&word.as_str()) {
            if let Some(value) = words.get(i + 1) {
                options.push(value.clone());
            }
            i += 1;
        }
        i += 1;
    }
    let mut out = options;
    if kept.is_empty() {
        let suites: BTreeSet<&str> = tests
            .iter()
            .flat_map(|t| t.suites.iter().map(String::as_str))
            .collect();
        if suites.is_empty() || tests.iter().any(|t| t.suites.is_empty()) {
            return None;
        }
        for suite in suites {
            out.push("--no-suite".into());
            out.push(suite.to_string());
        }
    } else {
        out.extend(kept.iter().map(|k| (*k).to_string()));
    }
    let again = list(real, &out, cwd).ok()?;
    let mut want: Vec<&str> = kept.clone();
    want.sort_unstable();
    let mut got: Vec<&str> = again.iter().map(String::as_str).collect();
    got.sort_unstable();
    (got == want).then_some(out)
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

/// \[C-104\] Whether a unit's tests may be left out: every program of it is built here, up to
/// date, not built again since the plan, and built from exactly the C and C++ files of the scope
/// its key was made from.
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
    if built.stale.is_some() || built.refused.is_some() {
        return false;
    }
    for test in tests.iter().filter(|t| t.unit == unit) {
        let Ok(program) = &test.program else {
            return false;
        };
        let rebuilt = std::fs::metadata(program)
            .and_then(|m| m.modified())
            .map_or(true, |m| m > *planned_at);
        if rebuilt {
            return false;
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

/// How one test ended, from Meson's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    Skipped,
    NotRun,
}

/// \[C-105\] Each test's outcome and time from Meson's `testlog.json`, one JSON object a line: a
/// name logged more than once (`--repeat`) failed if any line did, and was skipped only if every
/// line was. `OK` and `EXPECTEDFAIL` passed; `SKIP` was skipped; every other result failed.
/// \[C-162\] One whose lines include a pass and a failure was retried: the third field.
fn outcomes(text: &str) -> Result<BTreeMap<String, (Outcome, u64, bool)>, String> {
    let mut out: BTreeMap<String, (Outcome, u64, bool)> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|err| format!("a line isn't JSON: {err}"))?;
        let name = value
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or("a line names no test")?;
        let result = value
            .get("result")
            .and_then(|r| r.as_str())
            .ok_or("a line has no result")?;
        let ms = value
            .get("duration")
            .and_then(serde_json::Value::as_f64)
            .map_or(0, |d| (d * 1000.0).max(0.0) as u64);
        let outcome = match result {
            "OK" | "EXPECTEDFAIL" => Outcome::Passed,
            "SKIP" => Outcome::Skipped,
            _ => Outcome::Failed,
        };
        let entry = out.entry(name.to_string()).or_insert((outcome, 0, false));
        entry.1 = entry.1.saturating_add(ms);
        entry.2 |= matches!(
            (entry.0, outcome),
            (Outcome::Failed, Outcome::Passed) | (Outcome::Passed, Outcome::Failed)
        );
        entry.0 = match (entry.0, outcome) {
            (Outcome::Failed, _) | (_, Outcome::Failed) => Outcome::Failed,
            (Outcome::Skipped, Outcome::Skipped) => Outcome::Skipped,
            _ => Outcome::Passed,
        };
    }
    Ok(out)
}

/// The name a test of a subproject has in the log, `<project>:<name>`, or its own.
fn logged<'a>(
    log: &'a BTreeMap<String, (Outcome, u64, bool)>,
    name: &str,
) -> Option<&'a (Outcome, u64, bool)> {
    log.get(name).or_else(|| {
        let bare = name.split_once(':').map(|(_, n)| n)?;
        log.get(bare)
    })
}

fn record(p: &Pending) {
    let read = std::fs::metadata(&p.log)
        .map_err(|_| "meson test wrote no testlog.json".to_string())
        .and_then(|meta| {
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if p.before == Some((modified, meta.len())) {
                return Err("the test log is the one from before this run".into());
            }
            let fresh = modified
                .checked_add(std::time::Duration::from_secs(2))
                .is_some_and(|m| m >= p.started);
            if !fresh {
                return Err("the test log is older than this run".into());
            }
            std::fs::read_to_string(&p.log).map_err(|err| err.to_string())
        })
        .and_then(|text| outcomes(&text));
    // \[C-103\] What each program is built from now: `meson test` built it again before its tests
    // ran, unless `--no-rebuild` said not to.
    let built = walk(&p.tests, &p.root, &p.wd);
    let mut units: BTreeMap<String, UnitFacts> = BTreeMap::new();
    let mut own: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut every_source: Vec<String> = Vec::new();
    let mut stale_build: Option<String> = None;
    for (i, test) in p.tests.iter().enumerate() {
        let unit = units.entry(test.unit.clone()).or_insert_with(|| {
            let mut facts = UnitFacts {
                unit: test.unit.clone(),
                deselected: true,
                ..UnitFacts::default()
            };
            if let Some(built) = built.get(&test.unit) {
                facts.reads = built.reads.iter().cloned().collect();
                if let Some(why) = &built.refused {
                    facts.not_followed.push(Unfollowed {
                        why: "in-source build".into(),
                        detail: why.clone(),
                    });
                }
                if let Some(stale) = &built.stale {
                    facts.not_followed.push(Unfollowed {
                        why: "stale program".into(),
                        detail: stale.why().to_string(),
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
        let (outcome, duration_ms, retried) = if left {
            (Outcome::NotRun, 0, false)
        } else {
            read.as_ref()
                .ok()
                .and_then(|log| logged(log, &test.name))
                .copied()
                .unwrap_or((Outcome::NotRun, 0, false))
        };
        let finished = matches!(
            outcome,
            Outcome::Passed | Outcome::Failed | Outcome::Skipped
        );
        let failed = outcome == Outcome::Failed;
        let mut not_followed = Vec::new();
        if let Err(program) = &test.program {
            not_followed.push(Unfollowed {
                why: "not built here".into(),
                detail: program.clone(),
            });
        }
        unit.deselected &= left && p.reuse.contains(&test.unit);
        let skipped = !left && outcome == Outcome::Skipped;
        if !left {
            unit.selected += 1;
            unit.finished += u64::from(finished);
            if stale_build.is_none()
                && let Some(Stale::Old(why)) = built.get(&test.unit).and_then(|b| b.stale.as_ref())
            {
                stale_build = Some(format!("{} ran old code: {why}", test.unit));
            }
        }
        if unit.tests.is_none() {
            unit.skipped += u64::from(skipped);
        }
        unit.failed |= failed;
        unit.duration_ms = unit.duration_ms.saturating_add(duration_ms);
        match &mut unit.tests {
            Some(tests) => tests.push(TestFacts {
                id: test.name.clone(),
                finished,
                failed,
                deselected: left,
                duration_ms,
                not_followed,
                skipped,
                retried,
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
            "testlog".to_string(),
            serde_json::json!(format!("Meson's test log wasn't read: {why}")),
        );
    }
    write_facts(
        &p.units_dir,
        units.into_values().collect(),
        settings,
        p.report_tests,
        stale_build,
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
    stale_build: Option<String>,
) {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
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
        // Each `meson test` is a start of its own, and writes one file (C-32).
        start: Some(format!("{ADAPTER}-{}-{nanos}", std::process::id())),
        stale_build,
        ..FactsFile::default()
    };
    let path = dir.join(format!("{ADAPTER}-{}-{nanos}.json", std::process::id()));
    if let Ok(bytes) = serde_json::to_vec(&file) {
        let _ = facts::write_whole(&path, &bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-102
    #[test]
    fn meson_test_s_words_are_read_with_their_values() {
        let args = parse(&words(
            "-C build --suite unit -j4 t_add --logbase=mine t_sub",
        ));
        assert_eq!(args.wd.as_deref(), Some("build"));
        assert_eq!(args.names, ["t_add", "t_sub"]);
        assert_eq!(args.logbase.as_deref(), Some("mine"));
        assert_eq!(args.passthrough, None);
        assert_eq!(parse(&words("-Cout")).wd.as_deref(), Some("out"));
        assert_eq!(parse(&words("--wrapper valgrind x")).names, ["x"]);
        for runs_no_test in ["--list", "--gdb", "--benchmark", "-h", "--interactive"] {
            assert!(
                parse(&words(runs_no_test)).passthrough.is_some(),
                "{runs_no_test}"
            );
        }
        assert!(parse(&words("--no-rebuild")).no_rebuild);
    }

    // covers: C-105
    #[test]
    fn outcomes_come_from_each_line_of_the_log_and_a_repeat_that_failed_once_failed() {
        let log = r#"{"name": "a", "result": "OK", "duration": 0.5}
{"name": "b", "result": "FAIL", "duration": 0.1}
{"name": "c", "result": "SKIP", "duration": 0.0}
{"name": "d", "result": "EXPECTEDFAIL", "duration": 0.0}
{"name": "e", "result": "TIMEOUT", "duration": 30.0}
{"name": "f", "result": "OK", "duration": 0.1}
{"name": "f", "result": "FAIL", "duration": 0.1}
{"name": "g", "result": "UNEXPECTEDPASS", "duration": 0.1}
"#;
        let out = outcomes(log).unwrap();
        assert_eq!(out["a"], (Outcome::Passed, 500, false));
        assert_eq!(out["b"].0, Outcome::Failed);
        assert_eq!(out["c"].0, Outcome::Skipped);
        assert_eq!(out["d"].0, Outcome::Passed);
        assert_eq!(out["e"].0, Outcome::Failed);
        assert_eq!(out["f"].0, Outcome::Failed);
        assert_eq!(out["g"].0, Outcome::Failed);
        assert!(outcomes("not json\n").is_err());
        assert_eq!(logged(&out, "sub:a"), Some(&(Outcome::Passed, 500, false)));
    }

    // covers: C-162
    #[test]
    fn a_name_logged_with_a_pass_and_a_failure_was_retried_and_still_failed() {
        let log = r#"{"name": "f", "result": "FAIL", "duration": 0.1}
{"name": "f", "result": "OK", "duration": 0.1}
{"name": "p", "result": "OK", "duration": 0.1}
{"name": "p", "result": "OK", "duration": 0.1}
{"name": "x", "result": "FAIL", "duration": 0.1}
{"name": "x", "result": "TIMEOUT", "duration": 0.1}
{"name": "s", "result": "SKIP", "duration": 0.0}
{"name": "s", "result": "FAIL", "duration": 0.1}
"#;
        let out = outcomes(log).unwrap();
        assert_eq!(
            out["f"],
            (Outcome::Failed, 200, true),
            "the retry's pass is still a fail"
        );
        assert_eq!(
            out["p"],
            (Outcome::Passed, 200, false),
            "two passes didn't disagree"
        );
        assert_eq!(
            out["x"],
            (Outcome::Failed, 200, false),
            "two failures didn't disagree"
        );
        assert!(!out["s"].2, "a skip isn't a pass");
    }

    // covers: C-102
    #[test]
    fn a_test_s_unit_is_its_program_from_the_build_folder_with_the_build_type() {
        let tmp = tempfile::tempdir().unwrap();
        let wd = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(wd.join("meson-info")).unwrap();
        std::fs::create_dir_all(wd.join("tests")).unwrap();
        std::fs::write(wd.join("tests/t_add"), "").unwrap();
        let program = wd.join("tests/t_add");
        std::fs::write(
            wd.join("meson-info/intro-tests.json"),
            serde_json::json!([
                {"name": "add", "suite": ["p"], "cmd": [program]},
                {"name": "add2", "suite": ["p"], "cmd": [program, "x"]},
                {"name": "py", "suite": ["p"], "cmd": ["/usr/bin/python3", "t.py"]},
                {"name": "twice", "suite": ["p"], "cmd": [program]},
                {"name": "twice", "suite": ["q"], "cmd": [program]},
            ])
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            wd.join("meson-info/intro-buildoptions.json"),
            r#"[{"name": "buildtype", "value": "release"}]"#,
        )
        .unwrap();
        let tests = identify(&["add".into(), "add2".into(), "py".into()], &wd).unwrap();
        assert_eq!(tests[0].unit, "tests/t_add (release)");
        assert_eq!(tests[1].unit, "tests/t_add (release)");
        assert_eq!(tests[0].program, Ok(program.clone()));
        assert_eq!(tests[2].unit, "python3");
        assert_eq!(tests[2].program, Err("/usr/bin/python3".to_string()));
        // A name two tests answer to can't be told apart.
        assert!(identify(&["twice".into()], &wd).is_err());
    }
}
