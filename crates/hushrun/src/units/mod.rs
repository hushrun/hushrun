//! \[ADR 0068, 0069\] Test by test, in `hushrun run NAME -- COMMAND`, for a check whose `tests` isn't
//! off (C-5, C-30 to C-34).
//!
//! - `observe`: every test file runs, as it always did: hushrun records what each test file read
//!   and how it ended, and says what `tests: reuse` would have done.
//! - `reuse`: inside a run whose check missed, the test files that already passed on the files
//!   they read aren't run again. The [plan](Session::plan) comes before COMMAND: each test file's
//!   key, one batch lookup, the audited ones (C-34), and the list adapters deselect from. Every
//!   failure of the machinery ends in "ran everything".
//!
//! A [`Session`] starts before COMMAND: it makes the run's `HUSHRUN_UNITS` directory and the
//! variables that load an adapter, and nothing at all when `tests` is off. After COMMAND,
//! [`Session::finish`] reads what the adapter wrote and decides each test file's state with the
//! one function that does ([`hushrun_core::units::state`]); an audited test file that failed runs
//! once more ([`Session::again`]); the run records the check with its units. The directory goes
//! on every outcome.

pub mod cargo;
pub mod facts;
pub mod pytest;
pub mod runner;
pub mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_core::digest::Digest;
use hushrun_core::time::Time;
use hushrun_core::units::{self, Facts, KeyBase, Mode, RunFacts, Set, Sources, State, Tests, Tree};

use crate::results::RunResult;
use store::{Disagreement, Found, Mapped, UnitRecord, UnitResults};

/// \[ADR 0069\] The most test files a plan looks up, and a run records: past it, nothing is reused
/// (results-v0 §3).
pub const MOST_UNITS: usize = 5_000;

/// What an adapter saw of one unit, merged over its processes.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub facts: Facts,
    pub reads: BTreeSet<String>,
    pub duration_ms: u64,
}

/// Which adapter a run loads.
#[derive(Debug, Clone)]
enum Adapter {
    Pytest,
    Cargo(cargo::Recognized),
}

/// What starting to observe a run came to.
pub enum Start {
    /// `tests` is off: nothing changes.
    Off,
    /// No adapter can observe this COMMAND, and why, which the run says once.
    Not(String),
    On(Box<Session>),
}

/// One run's observing: its `HUSHRUN_UNITS` directory, removed when the session ends whatever
/// happened, what COMMAND's environment gains, and \[ADR 0069\] the plan, when reuse made one.
pub struct Session {
    dir: PathBuf,
    cache: PathBuf,
    adapter: Adapter,
    variables: Vec<(String, OsString)>,
    tests: Tests,
    root: PathBuf,
    plan: Option<Plan>,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// \[ADR 0069\] What a plan decided before COMMAND (RFC 0003 §3.5 step 1).
#[derive(Debug, Default)]
pub struct Plan {
    /// The key each unit the plan could compute one for has.
    keys: BTreeMap<String, Digest>,
    /// Of those, the ones whose newest result that counts here passed: what reuse stands on.
    passed: BTreeMap<String, Found>,
    /// What `hushrun.json` lists for adapters to deselect: `passed` but the audited, which run
    /// anyway (C-34).
    reuse: BTreeSet<String>,
}

/// \[ADR 0069\] What a plan came to: the list adapters deselect from, or why every test file runs.
pub enum Planned {
    /// A plan, and its line.
    Plan(String),
    /// No test file is reused, and why: a line after `<NAME>: every test file runs: `.
    Everything(String),
}

impl Session {
    /// Starts observing a run of `command` in `cwd`, in the repository at `root`, for a check
    /// whose `tests` are `tests`: `Off` without them, and then nothing is written or set.
    pub fn start(tests: Option<&Tests>, command: &[String], root: &Path, cwd: &Path) -> Start {
        let Some(tests) = tests.filter(|t| t.is_on()) else {
            return Start::Off;
        };
        let Some(cache) = crate::context::cache_dir() else {
            return Start::Not("there is no cache directory".into());
        };
        let env = |name: &str| std::env::var(name).ok();
        let (adapter, mut variables) = match cargo::recognize(command) {
            Some(recognized) => {
                let exe = match std::env::current_exe() {
                    Ok(exe) => exe,
                    Err(err) => return Start::Not(format!("can't tell where hushrun is: {err}")),
                };
                match cargo::runner_variables(&recognized, &exe, cwd, &env) {
                    Ok(vars) => (
                        Adapter::Cargo(recognized),
                        vars.into_iter()
                            .map(|(name, value)| (name, OsString::from(value)))
                            .collect::<Vec<_>>(),
                    ),
                    Err(why) => return Start::Not(why),
                }
            }
            None => {
                if let Some(why) = pytest::unsafe_for(command) {
                    return Start::Not(why.into());
                }
                let dir = match pytest::install(&cache) {
                    Ok(dir) => dir,
                    Err(err) => return Start::Not(format!("can't write its pytest plugin: {err}")),
                };
                (
                    Adapter::Pytest,
                    pytest::variables(
                        &dir,
                        std::env::var_os("PYTHONPATH"),
                        std::env::var_os("PYTEST_ADDOPTS"),
                    ),
                )
            }
        };
        let name = hushrun_ledger::new_ulid(Time::from_timestamp(jiff::Timestamp::now()).millis());
        let dir = cache.join("units").join("runs").join(name);
        let written =
            std::fs::create_dir_all(&dir).and_then(|()| write_run_file(&dir, root, &[], None, &[]));
        if let Err(err) = written {
            let _ = std::fs::remove_dir_all(&dir);
            return Start::Not(format!("can't make {}: {err}", dir.display()));
        }
        variables.push((runner::UNITS_VAR.to_string(), dir.clone().into_os_string()));
        Start::On(Box::new(Session {
            dir,
            cache,
            adapter,
            variables,
            tests: tests.clone(),
            root: root.to_path_buf(),
            plan: None,
        }))
    }

    /// Gives COMMAND the variables that load the adapter.
    pub fn apply(&self, command: &mut Command) {
        for (name, value) in &self.variables {
            command.env(name, value);
        }
    }

    /// \[ADR 0069\] The plan (RFC 0003 §3.5 step 1), for a check in `reuse`, before COMMAND: each
    /// test file's key, from the map for pytest (a hint, never trusted, §3.4) and from the build
    /// for Cargo, which it makes first; one batch lookup of the newest result that counts here for
    /// each key, with the check's reuse state; the audited (C-34); and `hushrun.json`'s list.
    /// A test file the map doesn't have, one `tests.always` names, one whose last state was `not
    /// observed`, one whose newest result is a fail or none, and an audited one: runs.
    pub fn plan(&mut self, input: &PlanInput<'_>) -> Planned {
        let started = input.started.to_string();
        let (patterns, built) = match &self.adapter {
            Adapter::Pytest => {
                let patterns = pytest::python_files(input.root, input.cwd);
                if pytest::is_test_file(&patterns, "x.py").is_none() {
                    return Planned::Everything(
                        "pytest's python_files names a directory, which this version doesn't read"
                            .into(),
                    );
                }
                (Some(patterns), None)
            }
            Adapter::Cargo(recognized) => {
                match cargo::plan_build(recognized, input.root, input.cwd) {
                    Ok(built) => (None, Some(built)),
                    Err(why) => return Planned::Everything(why),
                }
            }
        };
        let sets = match (&patterns, &built) {
            (Some(patterns), _) => units::classify(
                input.tree.paths(),
                &|path| pytest::is_test_file(patterns, path) == Some(true),
                Sources::Kinds(pytest::SOURCE_KINDS),
                &self.tests.ignore,
            ),
            (None, Some(built)) => units::classify(
                input.tree.paths(),
                &|path| built.unit_files.contains(path),
                Sources::Named(&built.named, cargo::SOURCE_KINDS),
                &self.tests.ignore,
            ),
            (None, None) => return Planned::Everything("no adapter".into()),
        };
        let keyer = Keyer::new(input.tree, &sets, &input.base);
        let map = store::load_map(&self.map_path(&input.base, input.repository));
        // The test files the plan knows of, and the files each read, as far as it can tell.
        let known: Vec<(String, Option<BTreeSet<String>>)> = match &built {
            None => sets
                .files(Set::Units)
                .map(|unit| {
                    let reads = map
                        .units
                        .get(unit)
                        .filter(|mapped| mapped.not_followed.is_empty())
                        .map(|mapped| mapped.reads.clone());
                    (unit.to_string(), reads)
                })
                .collect(),
            Some(built) => built
                .units
                .iter()
                .map(|(unit, b)| {
                    let reads = (b.not_followed.is_empty() && input.tree.contains(unit))
                        .then(|| b.reads.clone());
                    (unit.clone(), reads)
                })
                .collect(),
        };
        let total = known.len();
        if total == 0 {
            // No test file: nothing to say, and nothing to deselect.
            return Planned::Plan(String::new());
        }
        if total > MOST_UNITS {
            return Planned::Everything(format!(
                "it has {total} test files, and this version reuses them in checks of {MOST_UNITS} at most"
            ));
        }
        let mut keys = BTreeMap::new();
        for (unit, reads) in &known {
            if self.tests.always_runs(unit) {
                continue;
            }
            if let Some(key) = reads.as_ref().and_then(|reads| keyer.key(unit, reads)) {
                keys.insert(unit.clone(), key);
            }
        }
        let asked: Vec<Digest> = keys.values().copied().collect();
        let lookup = match input.store.lookup(&asked, input.counts) {
            Ok(lookup) => lookup,
            Err(why) => return Planned::Everything(format!("can't read which passed ({why})")),
        };
        if !lookup.reuse.allows_reuse() {
            return Planned::Everything(suspended(&input.base.check, &lookup.reuse));
        }
        let passed: BTreeMap<String, Found> = keys
            .iter()
            .filter_map(|(unit, key)| {
                let found = lookup.found.get(key)?;
                found.passed.then(|| (unit.clone(), found.clone()))
            })
            .collect();
        let audited = units::audited(
            passed.keys().map(|unit| (unit.as_str(), &keys[unit])),
            &started,
        );
        let mut reuse: BTreeSet<String> = passed
            .keys()
            .filter(|unit| !audited.contains(*unit))
            .cloned()
            .collect();
        // A test binary is reused only as the bytes whose dep-info its key holds.
        let mut binaries = Vec::new();
        if let Some(built) = &built {
            reuse.retain(|unit| {
                let Some(b) = built.units.get(unit) else {
                    return false;
                };
                match std::fs::read(&b.executable) {
                    Ok(bytes) => {
                        binaries.push(facts::Binary {
                            unit: unit.clone(),
                            path: b.executable.clone(),
                            sha256: Digest::of(&bytes).hex(),
                        });
                        true
                    }
                    Err(_) => false,
                }
            });
        }
        if let Err(err) = write_run_file(
            &self.dir,
            &self.root,
            &reuse.iter().cloned().collect::<Vec<_>>(),
            patterns.as_deref(),
            &binaries,
        ) {
            return Planned::Everything(format!("can't write the plan ({err})"));
        }
        // The line before COMMAND (RFC 0003 §1.2), in the words people read.
        let check = &input.base.check;
        let reused = reuse.len();
        let line = if reused == 0 && audited.is_empty() {
            let all = format!("{check}: all {} run", test_files(total as u64));
            let rest = rest_of(&sets, input.tree);
            let sources: BTreeSet<String> = sets.files(Set::Sources).map(str::to_string).collect();
            match why_all(&map, &rest, &sources) {
                Some(why) => format!("{all}: {why}"),
                None if passed.is_empty() => {
                    format!("{all}: none had passed here yet on the files it read")
                }
                None => all,
            }
        } else if reused == 0 {
            format!(
                "{check}: all {} run; {} that already passed on the files {} read {} as a check",
                test_files(total as u64),
                audited.len(),
                if audited.len() == 1 { "it" } else { "they" },
                if audited.len() == 1 { "runs" } else { "run" },
            )
        } else {
            let mut line = format!(
                "{check}: {} of {} run; {reused} already passed on the files {} read",
                total - reused,
                test_files(total as u64),
                if reused == 1 { "it" } else { "they" },
            );
            if !audited.is_empty() {
                line.push_str(&format!(
                    ", and {} more that did {} as a check",
                    audited.len(),
                    if audited.len() == 1 { "runs" } else { "run" },
                ));
            }
            line
        };
        self.plan = Some(Plan {
            keys,
            passed,
            reuse,
        });
        Planned::Plan(line)
    }

    fn map_path(&self, base: &KeyBase, repository: Option<&str>) -> PathBuf {
        store::file(&self.cache, &store::check_id(base, repository), "map")
    }

    /// What the adapter saw, decided: each unit's state, what the plan would have reused, and the
    /// lines that say so. `None` when the adapter wrote nothing, so that everything ran and no
    /// unit is recorded.
    pub fn finish(&self, end: &Finish<'_>) -> Option<Observed> {
        let files = facts::read_all(&self.dir)?;
        let map_path = self.map_path(&end.base, end.repository);
        let audit_path = store::file(
            &self.cache,
            &store::check_id(&end.base, end.repository),
            "audit",
        );
        let map = store::load_map(&map_path);
        let (seen, failures_by_unit, sets, from_this_run, executables) =
            match self.read(&files, end.root, end.cwd, end.tree, end.command_passed) {
                Ok(Some(read)) => read,
                Ok(None) => return None,
                Err(why) => return Observed::unread(&end.base.check, &why),
            };
        let how = match (end.mode, &self.plan) {
            (Mode::Reuse, Some(plan)) => How::Plan(plan),
            (Mode::Reuse, None) => How::Quiet,
            _ => How::Observe,
        };
        let mut observed = decide(
            end,
            &self.tests,
            seen,
            failures_by_unit,
            &sets,
            &map,
            from_this_run,
            map_path,
            audit_path,
            how,
        );
        observed.executables = executables;
        Some(observed)
    }

    /// What the adapter wrote in `dir`, read: each unit as seen, whether failures are reported by
    /// unit, the four sets, whether the keys come from this run (Cargo) and each test binary's
    /// executable. `Ok(None)` when it wrote nothing; `Err` why it couldn't be read.
    #[allow(clippy::type_complexity)]
    fn read(
        &self,
        files: &[facts::FactsFile],
        root: &Path,
        cwd: &Path,
        tree: &Tree,
        command_passed: bool,
    ) -> Result<
        Option<(
            BTreeMap<String, Seen>,
            bool,
            units::Sets,
            bool,
            BTreeMap<String, PathBuf>,
        )>,
        String,
    > {
        match &self.adapter {
            Adapter::Pytest => {
                let merged = facts::merged(files, "pytest");
                if merged.is_empty() {
                    return Ok(None);
                }
                let patterns = pytest::python_files(root, cwd);
                let told = facts::setting(files, "pytest", "pythonFiles");
                let readable = pytest::is_test_file(&patterns, "x.py").is_some();
                let agrees = matches!(&told, Ok(Some(list)) if *list == patterns);
                let guard = (!readable || !agrees).then(|| units::NotFollowed {
                    why: "python-files".into(),
                    detail: "pytest's python_files isn't what hushrun read of its configuration"
                        .into(),
                });
                let failures_by_unit = merged.values().all(|m| m.failures_by_unit);
                let seen: BTreeMap<String, Seen> = merged
                    .into_iter()
                    .map(|(unit, m)| {
                        let mut not_followed = m.not_followed;
                        not_followed.extend(guard.clone());
                        let facts = Facts {
                            complete: m.finished >= m.selected,
                            failed: m.failed,
                            not_followed,
                            deselected: m.deselected,
                        };
                        let seen = Seen {
                            facts,
                            reads: m.reads,
                            duration_ms: m.duration_ms,
                        };
                        (unit, seen)
                    })
                    .collect();
                let is_unit =
                    |path: &str| readable && pytest::is_test_file(&patterns, path) == Some(true);
                let sets = units::classify(
                    tree.paths(),
                    &is_unit,
                    Sources::Kinds(pytest::SOURCE_KINDS),
                    &self.tests.ignore,
                );
                Ok(Some((seen, failures_by_unit, sets, false, BTreeMap::new())))
            }
            Adapter::Cargo(recognized) => {
                if !files.iter().any(|f| f.adapter == cargo::ADAPTER) {
                    return Ok(None);
                }
                let observed = cargo::observe(recognized, files, root, cwd, command_passed)?;
                let sets = units::classify(
                    tree.paths(),
                    &|path| observed.unit_files.contains(path),
                    Sources::Named(&observed.named, cargo::SOURCE_KINDS),
                    &self.tests.ignore,
                );
                Ok(Some((
                    observed.units,
                    true,
                    sets,
                    true,
                    observed.executables,
                )))
            }
        }
    }

    /// \[C-34, ADR 0069\] The second run of the audit (RFC 0003 §3.7): COMMAND as written, again,
    /// with a directory of its own whose `hushrun.json` lists every test file the first run saw
    /// but `again`, so that only those run. `None` when it can't be set up, and then each of them
    /// counts as having failed again.
    pub fn again(&self, observed: &Observed, again: &BTreeSet<String>) -> Option<Again> {
        let dir = self.dir.join("again");
        std::fs::create_dir_all(&dir).ok()?;
        let others: Vec<String> = observed
            .units
            .keys()
            .filter(|unit| !again.contains(*unit))
            .cloned()
            .collect();
        let (patterns, binaries) = match &self.adapter {
            Adapter::Pytest => (
                Some(pytest::python_files(&self.root, &observed.cwd)),
                Vec::new(),
            ),
            Adapter::Cargo(_) => {
                let mut binaries = Vec::new();
                for unit in &others {
                    let path = observed.executables.get(unit)?;
                    let bytes = std::fs::read(path).ok()?;
                    binaries.push(facts::Binary {
                        unit: unit.clone(),
                        path: path.clone(),
                        sha256: Digest::of(&bytes).hex(),
                    });
                }
                (None, binaries)
            }
        };
        write_run_file(&dir, &self.root, &others, patterns.as_deref(), &binaries).ok()?;
        let mut variables = self.variables.clone();
        for (name, value) in &mut variables {
            if name == runner::UNITS_VAR {
                *value = dir.clone().into_os_string();
            }
        }
        Some(Again {
            dir,
            variables,
            units: again.clone(),
        })
    }

    /// \[C-34\] How each test file of `again` ended in its second run: `Passed`, or any other
    /// state, which counts as having failed again. `passed`: the second COMMAND exited 0.
    pub fn again_ended(
        &self,
        again: &Again,
        observed: &Observed,
        tree: &Tree,
        passed: bool,
    ) -> BTreeMap<String, State> {
        let files = facts::read_all(&again.dir).unwrap_or_default();
        let read = self
            .read(&files, &self.root, &observed.cwd, tree, passed)
            .ok()
            .flatten();
        again
            .units
            .iter()
            .map(|unit| {
                let state = read
                    .as_ref()
                    .and_then(|(seen, by_unit, ..)| {
                        let seen = seen.get(unit)?;
                        Some(units::state(
                            &seen.facts,
                            &RunFacts {
                                command_passed: passed,
                                failures_by_unit: *by_unit,
                                planned: false,
                            },
                        ))
                    })
                    .unwrap_or(State::NotRun);
                (unit.clone(), state)
            })
            .collect()
    }
}

/// \[ADR 0069\] Writes `hushrun.json` in `dir` whole: the repository's root, what adapters may
/// deselect, pytest's unit rule as hushrun read it, and the reused binaries.
fn write_run_file(
    dir: &Path,
    root: &Path,
    reuse: &[String],
    python_files: Option<&[String]>,
    binaries: &[facts::Binary],
) -> std::io::Result<()> {
    let run = facts::RunFile {
        schema: facts::RUN_SCHEMA.into(),
        root: root.to_path_buf(),
        reuse: reuse.to_vec(),
        python_files: python_files.map(<[String]>::to_vec),
        binaries: binaries.to_vec(),
    };
    let bytes = serde_json::to_vec(&run).map_err(std::io::Error::other)?;
    facts::write_whole(&dir.join(facts::RUN_FILE), &bytes)
}

/// \[C-34\] The second run of an audit: its directory, the variables that point COMMAND at it,
/// and the test files it runs.
pub struct Again {
    dir: PathBuf,
    variables: Vec<(String, OsString)>,
    pub units: BTreeSet<String>,
}

impl Again {
    /// Gives COMMAND the variables of the second run.
    pub fn apply(&self, command: &mut Command) {
        for (name, value) in &self.variables {
            command.env(name, value);
        }
    }
}

/// What the plan needs (RFC 0003 §3.5 step 1).
pub struct PlanInput<'a> {
    /// What every unit key holds of the run's key.
    pub base: KeyBase,
    /// The repository the run names (C-10), which the check's map and audit are kept by.
    pub repository: Option<&'a str>,
    /// The scope's files now, which COMMAND will run on.
    pub tree: &'a Tree,
    /// Where results are found, and the check's reuse state.
    pub store: &'a dyn UnitResults,
    /// Whether a run's result counts here (C-10, C-25).
    pub counts: &'a dyn Fn(&RunResult) -> bool,
    pub root: &'a Path,
    pub cwd: &'a Path,
    /// The run's start, RFC 3339: with each key, what chooses the audited.
    pub started: &'a str,
}

/// What a run knows when COMMAND has ended, for [`Session::finish`].
pub struct Finish<'a> {
    /// What every unit key holds of the run's key.
    pub base: KeyBase,
    /// The repository the run names (C-10), which the check's map and audit are kept by.
    pub repository: Option<&'a str>,
    /// The scope's files now, which COMMAND ran on.
    pub tree: &'a Tree,
    pub command_passed: bool,
    /// Whether a run's result counts here: the rule a check's pass is held to (C-10, C-25).
    pub counts: &'a dyn Fn(&RunResult) -> bool,
    pub store: &'a dyn UnitResults,
    pub root: &'a Path,
    pub cwd: &'a Path,
    /// The check's mode.
    pub mode: Mode,
}

/// `{total, ran, reused}`, a run's units (C-10), and \[ADR 0069\] why none was reused, when
/// something stopped it: `ranAnyway`, a word as the check's own `ranAnyway` is one.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub total: u64,
    pub ran: u64,
    pub reused: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_anyway: Option<String>,
}

/// How [`decide`] reads a run: as `observe` does, with what the plan would have done; with the
/// plan reuse made; or quietly, in `reuse` with no plan, where a line before COMMAND said why.
enum How<'a> {
    Observe,
    Plan(&'a Plan),
    Quiet,
}

/// What a run observed, decided.
#[derive(Debug, Default)]
pub struct Observed {
    /// Each unit's state, and the key it records under, when it records.
    pub units: BTreeMap<String, (State, Option<Digest>)>,
    /// The units the plan would have reused.
    pub would_reuse: BTreeSet<String>,
    /// `None` when no unit could be read at all.
    pub counts: Option<Counts>,
    /// What the run says, each a line after `hushrun: `.
    pub lines: Vec<String>,
    /// \[ADR 0069\] The time the reused test files took when they ran, by their results.
    pub saved_ms: u64,
    /// \[C-34\] Test files whose key had a pass that counted, which ran and failed: each runs once
    /// more ([`Session::again`]), with the key of that pass.
    pub again: BTreeMap<String, Digest>,
    records: Vec<UnitRecord>,
    mapped: BTreeMap<String, Mapped>,
    rest: BTreeMap<String, String>,
    sources: BTreeSet<String>,
    disagreements: Vec<(String, Digest)>,
    map_path: Option<PathBuf>,
    audit_path: Option<PathBuf>,
    executables: BTreeMap<String, PathBuf>,
    cwd: PathBuf,
}

impl Observed {
    /// A run whose units couldn't be read: it says why, and records nothing of them.
    fn unread(check: &hushrun_core::CheckName, why: &str) -> Option<Observed> {
        Some(Observed {
            lines: vec![format!("{check}: test files weren't observed: {why}")],
            ..Observed::default()
        })
    }

    /// The test files' results this run records (C-33).
    pub fn records(&self) -> &[UnitRecord] {
        &self.records
    }

    /// \[C-34\] Keeps a disagreement that a second run confirmed, for the check's audit file.
    pub fn disagree(&mut self, unit: &str, key: Digest) {
        self.disagreements.push((unit.to_string(), key));
    }

    /// Writes what this run learned for the next: the map, and the audit's disagreements. Only
    /// when the files didn't change while COMMAND ran, which the caller checked.
    pub fn keep(&self, run_key: &Digest, tree: &Tree, now: Time) {
        if let Some(path) = &self.map_path
            && let Err(err) = store::update_map(
                path,
                &self.mapped,
                tree,
                self.rest.clone(),
                self.sources.clone(),
            )
        {
            tracing::debug!("the map wasn't kept: {err:#}");
        }
        if let Some(path) = &self.audit_path {
            let found: Vec<Disagreement> = self
                .disagreements
                .iter()
                .map(|(unit, key)| Disagreement {
                    unit: unit.clone(),
                    key: *key,
                    run_key: *run_key,
                    at: now,
                })
                .collect();
            if let Err(err) = store::add_disagreements(path, &found) {
                tracing::debug!("the audit wasn't kept: {err:#}");
            }
        }
    }
}

/// One test file, or several, for people.
fn test_files(n: u64) -> String {
    match n {
        1 => "1 test file".into(),
        n => format!("{n} test files"),
    }
}

/// \[C-34\] Why every test file runs while a check's reuse is suspended, and how a person turns it
/// on again.
pub fn suspended(check: &hushrun_core::CheckName, reuse: &store::ReuseState) -> String {
    let since = match (&reuse.suspended_by, &reuse.suspended_at) {
        (Some(by), Some(at)) => format!(" by {by} on {at}"),
        _ => String::new(),
    };
    let which = reuse
        .unit
        .as_deref()
        .map_or_else(|| "a test file".to_string(), str::to_string);
    format!(
        "reuse of its test files is suspended{since}, since {which} failed twice though it had passed on the same files it read; a person turns it on again with `hushrun tests resume {check}`"
    )
}

/// The one way a unit's key is computed here (C-31), for a tree and its four sets.
struct Keyer<'a> {
    tree: &'a Tree,
    sets: &'a units::Sets,
    base: &'a KeyBase,
    rest: Digest,
    shape: Digest,
}

impl<'a> Keyer<'a> {
    fn new(tree: &'a Tree, sets: &'a units::Sets, base: &'a KeyBase) -> Keyer<'a> {
        let rest = tree.digest(sets.files(Set::Rest));
        let shape = units::shape(sets.files(Set::Sources));
        Keyer {
            tree,
            sets,
            base,
            rest,
            shape,
        }
    }

    fn key(&self, unit: &str, reads: &BTreeSet<String>) -> Option<Digest> {
        let keyed = self.sets.keyed(reads.iter().map(String::as_str));
        units::unit_key(
            self.base,
            unit,
            &self.tree.digest(keyed),
            &self.rest,
            &self.shape,
        )
        .ok()
    }
}

/// The rest set's files, each with its kind and content hash as a manifest writes them.
fn rest_of(sets: &units::Sets, tree: &Tree) -> BTreeMap<String, String> {
    sets.files(Set::Rest)
        .filter_map(|path| {
            let (kind, hash) = tree.entry(path)?;
            Some((path.to_string(), format!("{kind} {hash}")))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn decide(
    end: &Finish<'_>,
    tests: &Tests,
    seen: BTreeMap<String, Seen>,
    failures_by_unit: bool,
    sets: &units::Sets,
    map: &store::Map,
    from_this_run: bool,
    map_path: PathBuf,
    audit_path: PathBuf,
    how: How<'_>,
) -> Observed {
    let tree = end.tree;
    let keyer = Keyer::new(tree, sets, &end.base);
    let sources: BTreeSet<String> = sets.files(Set::Sources).map(str::to_string).collect();
    let mut out = Observed {
        map_path: Some(map_path),
        audit_path: Some(audit_path),
        cwd: end.cwd.to_path_buf(),
        ..Observed::default()
    };
    // What the plan would have done, in `observe`: from what was recorded before this run, a unit
    // the map has (or, for a compiler-given adapter, this build's list) whose key has a pass that
    // counts here, unless `tests.always` names it. One batch.
    let mut would: BTreeMap<String, (Digest, Found)> = BTreeMap::new();
    if matches!(how, How::Observe) {
        let planned: BTreeMap<String, Digest> = seen
            .iter()
            .filter(|(unit, _)| !tests.always_runs(unit))
            .filter_map(|(unit, s)| {
                let reads = if from_this_run {
                    Some(&s.reads)
                } else {
                    map.units.get(unit).map(|m| &m.reads)
                }?;
                Some((unit.clone(), keyer.key(unit, reads)?))
            })
            .collect();
        let keys: Vec<Digest> = planned.values().copied().collect();
        if let Ok(lookup) = end.store.lookup(&keys, end.counts) {
            for (unit, key) in planned {
                if let Some(found) = lookup.found.get(&key).filter(|f| f.passed) {
                    would.insert(unit, (key, found.clone()));
                }
            }
        }
    }
    let mut counts = Counts::default();
    let mut saved_ms = 0u64;
    let mut not_observed: BTreeMap<String, u64> = BTreeMap::new();
    for (unit, seen) in &seen {
        counts.total += 1;
        let mut facts = seen.facts.clone();
        if !tree.contains(unit) {
            facts.not_followed.insert(units::NotFollowed {
                why: "outside-scope".into(),
                detail: unit.clone(),
            });
        }
        let planned = match &how {
            How::Plan(plan) => plan.reuse.contains(unit),
            _ => false,
        };
        let state = units::state(
            &facts,
            &RunFacts {
                command_passed: end.command_passed,
                failures_by_unit,
                planned,
            },
        );
        if state.ran() {
            counts.ran += 1;
        }
        if state == State::NotObserved {
            for why in facts.not_followed.iter().map(|n| n.why.clone()) {
                *not_observed.entry(why).or_default() += 1;
            }
        }
        match &how {
            How::Observe => {
                if let Some((key, found)) = would.get(unit) {
                    saved_ms = saved_ms.saturating_add(found.duration_ms);
                    out.would_reuse.insert(unit.clone());
                    if state == State::Failed {
                        out.disagreements.push((unit.clone(), *key));
                    }
                }
            }
            How::Plan(plan) => {
                if state == State::Reused {
                    counts.reused += 1;
                    if let Some(found) = plan.passed.get(unit) {
                        saved_ms = saved_ms.saturating_add(found.duration_ms);
                    }
                }
                // \[C-34\] Audited, or listed and run all the same: it had a pass that counted on
                // what the map said it read, and failed. A second run tells a flaky test file from
                // a recorded pass that was wrong for these files.
                if state == State::Failed
                    && plan.passed.contains_key(unit)
                    && let Some(key) = plan.keys.get(unit)
                {
                    out.again.insert(unit.clone(), *key);
                }
            }
            How::Quiet => {}
        }
        let key = state.recorded().and_then(|_| keyer.key(unit, &seen.reads));
        if let (Some(passed), Some(key)) = (state.recorded(), key) {
            out.records.push(UnitRecord {
                key,
                result: if passed { "pass" } else { "fail" }.into(),
                duration_ms: seen.duration_ms,
            });
        }
        if state.ran() {
            out.mapped.insert(
                unit.clone(),
                Mapped {
                    reads: sets
                        .keyed(seen.reads.iter().map(String::as_str))
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    not_followed: facts.not_followed.iter().map(|n| n.why.clone()).collect(),
                },
            );
        }
        out.units.insert(unit.clone(), (state, key));
    }
    out.rest = rest_of(sets, tree);
    out.sources = sources;
    out.saved_ms = saved_ms;
    if out.records.len() > MOST_UNITS {
        // More than a server takes for one run: none goes, so none counts.
        out.records.clear();
    }

    // What `observe` says (RFC 0003 §1.2), in the words people read: test files, the files they
    // read.
    let check = &end.base.check;
    let reused = out.would_reuse.len() as u64;
    if counts.total > 0 && matches!(how, How::Observe) {
        let would_run = counts.total - reused;
        let mut line = if reused == 0 {
            let all = format!(
                "{check}: with `tests: reuse`, all {} would have run",
                test_files(counts.total)
            );
            match why_all(map, &out.rest, &out.sources) {
                Some(why) => format!("{all}: {why}"),
                None if map.schema.is_empty() => {
                    format!("{all}: none had passed here yet on the files it read")
                }
                None => all,
            }
        } else {
            let saving = if saved_ms >= 1000 {
                format!(", saving about {}", crate::cmd::wrap::took(saved_ms))
            } else {
                String::new()
            };
            format!(
                "{check}: with `tests: reuse`, {would_run} of {} would have run; {reused} already passed on the files {}{saving}",
                test_files(counts.total),
                if reused == 1 { "it read" } else { "they read" },
            )
        };
        if !not_observed.is_empty() {
            let n = out
                .units
                .values()
                .filter(|(s, _)| *s == State::NotObserved)
                .count();
            let whys: Vec<String> = not_observed.keys().map(|w| describe(w)).collect();
            line.push_str(&format!(
                "; hushrun couldn't follow all that {} read ({}), so {} always run",
                if n == 1 {
                    "1 of them".to_string()
                } else {
                    format!("{n} of them")
                },
                whys.join(", "),
                if n == 1 { "it would" } else { "they would" }
            ));
        }
        out.lines.push(line);
        for (unit, _) in &out.disagreements {
            out.lines.push(format!(
                "{check}: {unit} failed, though it passed before on the same files it read: with `tests: reuse` it would have been reused"
            ));
        }
    }
    out.counts = Some(counts);
    out
}

/// A word an adapter wrote for what it couldn't follow, for people.
fn describe(why: &str) -> String {
    match why {
        "subprocess" => "a process it started".into(),
        "fork" => "a fork".into(),
        "native" => "native code".into(),
        "no-dep-info" | "target-without-dep-info" => "a build without dep-info".into(),
        "python-files" => "pytest's python_files".into(),
        "adapter-error" => "an error of hushrun's adapter".into(),
        "outside-scope" => "a file outside the check's scope".into(),
        other => other.to_string(),
    }
}

/// Why every test file would have run though some ran here before: the first file of the rest set
/// that changed since the map was written, or a source that is new or gone (RFC 0003 §1.2).
fn why_all(
    map: &store::Map,
    rest: &BTreeMap<String, String>,
    sources: &BTreeSet<String>,
) -> Option<String> {
    if map.schema.is_empty() {
        return None;
    }
    let changed = rest
        .iter()
        .find(|(path, entry)| map.rest.get(*path).is_some_and(|before| before != *entry))
        .map(|(path, _)| format!("{path} changed, and any test may read it"))
        .or_else(|| {
            rest.keys()
                .find(|path| !map.rest.contains_key(*path))
                .map(|path| format!("{path} is new, and any test may read it"))
        })
        .or_else(|| {
            map.rest
                .keys()
                .find(|path| !rest.contains_key(*path))
                .map(|path| format!("{path} is gone, and any test may have read it"))
        });
    changed
        .or_else(|| {
            sources
                .iter()
                .find(|path| !map.sources.contains(*path))
                .map(|path| format!("{path} is new, which can change what tests load"))
        })
        .or_else(|| {
            map.sources
                .iter()
                .find(|path| !sources.contains(*path))
                .map(|path| format!("{path} is gone, which can change what tests load"))
        })
}

/// This machine's results directory, where unit results go in local mode (C-33), with the reuse
/// state of `check` for `repository` (C-34).
pub fn local_store(repository: Option<&str>, check: &str) -> Option<store::Local> {
    let dir = crate::results::local_directory()?;
    let cache = crate::context::cache_dir()?;
    Some(store::Local::with_reuse(
        &dir,
        store::reuse_file(&cache, repository, check),
    ))
}
