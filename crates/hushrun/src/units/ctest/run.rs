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

/// The real `ctest`, or another tool, in `cwd` with the environment COMMAND had: `PATH` as it
/// was, and \[C-32\] COMMAND's own `env NAME=value` assignments (`assigned`), which the plan's
/// listing, made before COMMAND, needs given (inside COMMAND they are there already).
pub fn tool(program: &Path, cwd: &Path, assigned: &[(String, String)]) -> Command {
    let mut command = super::super::command::tool(program, cwd, assigned);
    program::as_command_had(&mut command);
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

/// The real `ctest`'s listing with `args`: its tests. A listing writes a test preset's
/// `outputJUnitFile` and `outputLogFile` (ran, CMake 4.4): it is given a JUnit file of its own,
/// in a directory of its own, which replaces the preset's; and the preset's log, `log`, which a
/// command line's `-O` doesn't replace (and on CMake 3.22 makes a listing write), is put back as
/// it was, so that no file the user reads is left changed by hushrun.
pub fn list(
    real: &Path,
    args: &[String],
    cwd: &Path,
    log: Option<&Path>,
    assigned: &[(String, String)],
) -> Result<Vec<select::Listed>, String> {
    let own = tempfile::tempdir().map_err(|err| format!("no temporary directory: {err}"))?;
    let kept = log.map(Kept::new);
    // A signal while the preset's log is changed waits until it is put back, then acts.
    let held = kept.as_ref().map(|_| HeldSignals::new());
    let output = tool(real, cwd, assigned)
        .args(args)
        .arg("--show-only=json-v1")
        .arg("--output-junit")
        .arg(own.path().join("listing.xml"))
        .current_dir(cwd)
        .output();
    if let Some(kept) = kept {
        kept.put_back();
    }
    drop(held);
    let output = output.map_err(|err| format!("can't start {}: {err}", real.display()))?;
    if !output.status.success() {
        return Err("ctest couldn't list its tests".into());
    }
    select::parse_listing(&output.stdout).map(|(listing, _)| listing.tests)
}

/// The signals that stop a process, held while it lives: one that arrives then acts once it is
/// dropped, as it would have.
struct HeldSignals {
    #[cfg(unix)]
    arrived: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(unix)]
    ids: Vec<signal_hook::SigId>,
}

impl HeldSignals {
    fn new() -> HeldSignals {
        #[cfg(unix)]
        {
            use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
            let arrived = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ids = [SIGINT, SIGTERM, SIGHUP, SIGQUIT]
                .into_iter()
                .filter_map(|signal| {
                    let value = usize::try_from(signal).ok()?;
                    signal_hook::flag::register_usize(signal, arrived.clone(), value).ok()
                })
                .collect();
            HeldSignals { arrived, ids }
        }
        #[cfg(not(unix))]
        {
            HeldSignals {}
        }
    }
}

impl Drop for HeldSignals {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            for id in &self.ids {
                signal_hook::low_level::unregister(*id);
            }
            let signal = self.arrived.load(std::sync::atomic::Ordering::SeqCst);
            if let Ok(signal) = i32::try_from(signal)
                && signal != 0
            {
                let _ = signal_hook::low_level::emulate_default_handler(signal);
            }
        }
    }
}

/// A file as it was before a listing, to put back after it.
struct Kept {
    path: PathBuf,
    was: Option<(Vec<u8>, SystemTime)>,
}

impl Kept {
    fn new(path: &Path) -> Kept {
        let was = std::fs::read(path).ok().and_then(|bytes| {
            let modified = std::fs::metadata(path).ok()?.modified().ok()?;
            Some((bytes, modified))
        });
        Kept {
            path: path.to_path_buf(),
            was,
        }
    }

    /// The file as it was: gone if it wasn't there, else its bytes and its time, whole.
    fn put_back(self) {
        match self.was {
            None => {
                let _ = std::fs::remove_file(&self.path);
            }
            Some((bytes, modified)) => {
                let same = std::fs::read(&self.path).is_ok_and(|now| now == bytes)
                    && std::fs::metadata(&self.path)
                        .and_then(|m| m.modified())
                        .is_ok_and(|m| m == modified);
                if !same
                    && facts::write_whole(&self.path, &bytes).is_ok()
                    && let Ok(file) = std::fs::File::options().write(true).open(&self.path)
                {
                    let _ = file.set_modified(modified);
                }
            }
        }
    }
}

/// \[ADR 0087\] A listing kept in a run's directory, for the same `ctest`, words and directory: the
/// plan's, made just before COMMAND `ctest …` starts, which hushrun's `ctest` then needn't make
/// again (a listing runs the project's discovery scripts, `TEST_INCLUDE_FILES`).
#[derive(serde::Serialize, serde::Deserialize)]
struct KeptListing {
    schema: String,
    real: PathBuf,
    words: Vec<String>,
    cwd: PathBuf,
    tests: Vec<select::Listed>,
}

const KEPT_LISTING: &str = "ctest-listing.v1";
const KEPT_LISTING_SCHEMA: &str = "hushrun.ctest-listing/v1";

impl KeptListing {
    fn read(dir: &Path, real: &Path, words: &[String], cwd: &Path) -> Option<Vec<select::Listed>> {
        let bytes = std::fs::read(dir.join(KEPT_LISTING)).ok()?;
        let kept: KeptListing = serde_json::from_slice(&bytes).ok()?;
        (kept.schema == KEPT_LISTING_SCHEMA
            && kept.real == real
            && kept.words == words
            && kept.cwd == cwd)
            .then_some(kept.tests)
    }

    fn write(dir: &Path, real: &Path, words: &[String], cwd: &Path, tests: &[select::Listed]) {
        let kept = KeptListing {
            schema: KEPT_LISTING_SCHEMA.into(),
            real: real.to_path_buf(),
            words: words.to_vec(),
            cwd: cwd.to_path_buf(),
            tests: tests.to_vec(),
        };
        if let Ok(bytes) = serde_json::to_vec(&kept) {
            let _ = facts::write_whole(&dir.join(KEPT_LISTING), &bytes);
        }
    }
}

/// The real `ctest`'s version.
pub fn version(
    real: &Path,
    cwd: &Path,
    assigned: &[(String, String)],
) -> Result<(u32, u32, u32), String> {
    let output = tool(real, cwd, assigned)
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
    assigned: &[(String, String)],
    kept: Option<&Path>,
) -> Result<(Vec<Test>, Option<args::Preset>, PathBuf), String> {
    let version = version(real, cwd, assigned)?;
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
    let log = preset
        .as_ref()
        .and_then(|p| p.log.as_ref())
        .map(|l| cwd.join(l));
    let words = args::listing(items, &[]);
    let tests = match kept.and_then(|dir| KeptListing::read(dir, real, &words, cwd)) {
        Some(tests) => tests,
        None => {
            let tests = list(real, &words, cwd, log.as_deref(), assigned)?;
            if let Some(dir) = kept {
                KeptListing::write(dir, real, &words, cwd, &tests);
            }
            tests
        }
    };
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
pub fn walk(
    tests: &[Test],
    root: &Path,
    path: Option<OsString>,
    assigned: &[(String, String)],
) -> BTreeMap<String, Built> {
    let mut walker = Walker::new(root, path, assigned);
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
                had.refused = had.refused.take().or(built.refused);
            }
        };
    let mut walked: BTreeMap<PathBuf, Built> = BTreeMap::new();
    let mut helper_done: BTreeSet<(String, usize)> = BTreeSet::new();
    for (i, test) in tests.iter().enumerate() {
        let Program::Built { path, build, named } = &test.program else {
            continue;
        };
        if done.insert(path.clone()) {
            let built = walker.program(path, build);
            walked.insert(path.clone(), built.clone());
            merge(&mut out, &test.unit, built);
        }
        // What the test names besides: a program it starts, a file it reads.
        for n in named {
            if named_done.insert((test.unit.clone(), n.clone())) {
                let built = walker.named(n);
                merge(&mut out, &test.unit, built);
            }
        }
        // \[ADR 0087\] What runs before it for it: the setup and cleanup of each fixture it
        // requires, followed through the fixtures they require, and each test it DEPENDS on. What
        // their programs are built from, and what they name, the test reads too.
        for h in helpers(tests, i) {
            if !helper_done.insert((test.unit.clone(), h)) {
                continue;
            }
            match &tests[h].program {
                Program::Built { path, build, named } => {
                    let mut built = match walked.get(path) {
                        Some(built) => built.clone(),
                        None => {
                            let built = walker.program(path, build);
                            walked.insert(path.clone(), built.clone());
                            built
                        }
                    };
                    built.own.clear();
                    merge(&mut out, &test.unit, built);
                    for n in named {
                        if named_done.insert((test.unit.clone(), n.clone())) {
                            let built = walker.named(n);
                            merge(&mut out, &test.unit, built);
                        }
                    }
                }
                // A program not built here (a script): any built program it starts reads only
                // sources, so the test reads every one.
                Program::Elsewhere(_) | Program::None => {
                    let built = Built {
                        every_source: true,
                        method: "every source",
                        ..Built::default()
                    };
                    merge(&mut out, &test.unit, built);
                }
            }
        }
    }
    out
}

/// The tests that run before test `i` for it, by index, but itself: the setup and cleanup tests of
/// each fixture it requires (`FIXTURES_REQUIRED`, `FIXTURES_SETUP`, `FIXTURES_CLEANUP`) and the
/// tests it `DEPENDS` on, and theirs in turn.
fn helpers(tests: &[Test], i: usize) -> BTreeSet<usize> {
    let mut found: BTreeSet<usize> = BTreeSet::new();
    let mut stack = vec![i];
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    while let Some(at) = stack.pop() {
        if !seen.insert(at) {
            continue;
        }
        let test = &tests[at].listed;
        let needs: BTreeSet<String> = test.words("FIXTURES_REQUIRED").into_iter().collect();
        let after: BTreeSet<String> = test.words("DEPENDS").into_iter().collect();
        for (j, other) in tests.iter().enumerate() {
            let provides = other
                .listed
                .words("FIXTURES_SETUP")
                .into_iter()
                .chain(other.listed.words("FIXTURES_CLEANUP"))
                .any(|f| needs.contains(&f));
            if j != i && (provides || after.contains(&other.listed.name)) {
                found.insert(j);
                stack.push(j);
            }
        }
    }
    found
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
    let (tests, preset, test_dir) = listed(call.real, &items, &cwd, &root, &[], Some(units))?;
    if preset.as_ref().is_some_and(|p| p.show_only) {
        return Err(
            "the test preset sets execution.showOnly, and ctest runs no test"
                .to_string()
                .into(),
        );
    }
    // A preset's `filter.include.index` picks tests by their position, as `-I` does.
    let select = select && !preset.as_ref().is_some_and(|p| p.by_position);
    let built = walk(&tests, &root, path_was(), &[]);
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
        retries: args::retries(&items, preset.as_ref()),
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
    if built.stale.is_some() || built.refused.is_some() {
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
    let version = version(real, cwd, &[]).ok()?;
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
        let log = preset.and_then(|p| p.log.as_ref()).map(|l| cwd.join(l));
        let Ok(again) = list(
            real,
            &args::listing(&args::parse(&words), &[]),
            cwd,
            log.as_deref(),
            &[],
        ) else {
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
            if let Some(built) = p.built.get(&test.unit) {
                facts.reads = built.reads.iter().cloned().collect();
                if let Some(why) = &built.refused {
                    facts.not_followed.push(Unfollowed {
                        why: "in-source build".into(),
                        detail: why.clone(),
                    });
                }
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
        // Each test's own program: one not built here is that test's, whichever test of its unit
        // comes first.
        if let Program::Elsewhere(program) = &test.program {
            not_followed.push(Unfollowed {
                why: "not built here".into(),
                detail: program.clone(),
            });
        }
        if p.retries && matches!(outcome, Outcome::Passed | Outcome::Skipped) {
            not_followed.push(Unfollowed {
                why: "repeat until-pass".into(),
                detail: "a pass under --repeat may have come after a failure".into(),
            });
        }
        // A whole unit is deselected when `reuse` listed it and every test of it was left out.
        unit.deselected &= left && p.reuse.contains(&test.unit);
        // \[C-32\] Its body didn't run: CTest skipped it (`SKIP_RETURN_CODE`,
        // `SKIP_REGULAR_EXPRESSION`), or it is disabled. In a run that failed, it proves nothing.
        let skipped = !left && (disabled || matches!(outcome, Outcome::Skipped));
        if !left && !disabled {
            unit.selected += 1;
            unit.finished += u64::from(finished);
        }
        if unit.tests.is_none() {
            unit.skipped += u64::from(skipped);
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
                skipped,
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
        // \[C-32\] Each `ctest` is a start of its own, and writes one file: a program counts as
        // run whole only when each start that ran any of it ran it whole.
        start: Some(format!("{ADAPTER}-{}-{nanos}", std::process::id())),
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

    fn test(name: &str, unit: &str, program: Program) -> Test {
        Test {
            listed: select::Listed {
                name: name.into(),
                command: Some(vec![format!("/b/{name}")]),
                properties: Vec::new(),
            },
            unit: unit.into(),
            id: name.into(),
            program,
        }
    }

    fn recorded(tests: Vec<Test>, report_tests: bool) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending {
            units_dir: dir.path().to_path_buf(),
            tests,
            leave: BTreeSet::new(),
            built: BTreeMap::new(),
            junit: dir.path().join("none.xml"),
            before: None,
            started: SystemTime::now(),
            retries: false,
            report_tests,
            reuse: BTreeSet::new(),
            selection: String::new(),
        };
        record(&pending);
        let file = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .unwrap();
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap()
    }

    // covers: C-49
    #[test]
    fn a_program_not_built_here_marks_its_own_test_whichever_comes_first() {
        // Two programs one unit's name: one built here, one found on PATH (a name clash when the
        // build names no configuration). The one not built here is told, whichever is first.
        let built = || Program::Built {
            path: "/b/t_x".into(),
            build: "/b".into(),
            named: Vec::new(),
        };
        let facts = recorded(
            vec![
                test("a", "t_x", built()),
                test("b", "t_x", Program::Elsewhere("t_x".into())),
            ],
            false,
        );
        assert_eq!(
            facts["units"][0]["notFollowed"][0]["why"],
            serde_json::json!("not built here"),
            "{facts:#}"
        );
        // Told one by one: the test whose program isn't built here, and not the other.
        let facts = recorded(
            vec![
                test("a", "t_x", Program::Elsewhere("t_x".into())),
                test("b", "t_x", built()),
            ],
            true,
        );
        let unit = &facts["units"][0];
        assert_eq!(
            unit["notFollowed"].as_array().map_or(0, Vec::len),
            0,
            "{facts:#}"
        );
        assert_eq!(
            unit["tests"][0]["notFollowed"][0]["why"],
            serde_json::json!("not built here"),
            "{facts:#}"
        );
        assert_eq!(
            unit["tests"][1]["notFollowed"]
                .as_array()
                .map_or(0, Vec::len),
            0,
            "{facts:#}"
        );
    }
}
