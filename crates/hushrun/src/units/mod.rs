//! \[ADR 0068\] Test by test, in `hushrun run NAME -- COMMAND`, for a check whose `tests` isn't
//! off (C-5, C-30 to C-33). In this version every test file runs, as it always did: hushrun
//! records what each test file read and how it ended, keeps its result on this machine, and says
//! what `tests: reuse` would have done. Nothing it does changes COMMAND's outcome or exit code.
//!
//! A [`Session`] starts before COMMAND: it makes the run's `HUSHRUN_UNITS` directory and the
//! variables that load an adapter, and nothing at all when `tests` is off. After COMMAND,
//! [`Session::finish`] reads what the adapter wrote and decides each test file's state with the
//! one function that does ([`hushrun_core::units::state`]); the run records the check, then
//! [`Observed::record`] its units. The directory goes on every outcome.

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
use hushrun_core::units::{self, Facts, KeyBase, RunFacts, Set, Sources, State, Tests, Tree};

use crate::results::RunResult;
use store::{Disagreement, Mapped, UnitRecord, UnitResults};

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
/// happened, and what COMMAND's environment gains.
pub struct Session {
    dir: PathBuf,
    cache: PathBuf,
    adapter: Adapter,
    variables: Vec<(String, OsString)>,
    tests: Tests,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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
        let run = facts::RunFile {
            schema: facts::RUN_SCHEMA.into(),
            root: root.to_path_buf(),
            reuse: Vec::new(),
        };
        let written = std::fs::create_dir_all(&dir).and_then(|()| {
            let bytes = serde_json::to_vec(&run).map_err(std::io::Error::other)?;
            facts::write_whole(&dir.join(facts::RUN_FILE), &bytes)
        });
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
        }))
    }

    /// Gives COMMAND the variables that load the adapter.
    pub fn apply(&self, command: &mut Command) {
        for (name, value) in &self.variables {
            command.env(name, value);
        }
    }

    /// What the adapter saw, decided: each unit's state, what the plan would have reused, and the
    /// lines that say so. `None` when the adapter wrote nothing, so that everything ran and no
    /// unit is recorded.
    pub fn finish(&self, end: &Finish<'_>) -> Option<Observed> {
        let files = facts::read_all(&self.dir)?;
        let map_path = store::file(&self.cache, &store::check_id(&end.base), "map");
        let audit_path = store::file(&self.cache, &store::check_id(&end.base), "audit");
        let map = store::load_map(&map_path);
        let (seen, failures_by_unit, sets, from_this_run) = match &self.adapter {
            Adapter::Pytest => {
                let merged = facts::merged(&files, "pytest");
                if merged.is_empty() {
                    return None;
                }
                let patterns = pytest::python_files(end.root, end.cwd);
                let told = facts::setting(&files, "pytest", "pythonFiles");
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
                    end.tree.paths(),
                    &is_unit,
                    Sources::Kinds(pytest::SOURCE_KINDS),
                    &self.tests.ignore,
                );
                (seen, failures_by_unit, sets, false)
            }
            Adapter::Cargo(recognized) => {
                if !files.iter().any(|f| f.adapter == cargo::ADAPTER) {
                    return None;
                }
                let observed =
                    match cargo::observe(recognized, &files, end.root, end.cwd, end.command_passed)
                    {
                        Ok(observed) => observed,
                        Err(why) => {
                            return Some(Observed::unread(&end.base.check, &why));
                        }
                    };
                let sets = units::classify(
                    end.tree.paths(),
                    &|path| observed.unit_files.contains(path),
                    Sources::Named(&observed.named, cargo::SOURCE_KINDS),
                    &self.tests.ignore,
                );
                (observed.units, true, sets, true)
            }
        };
        Some(decide(
            end,
            &self.tests,
            seen,
            failures_by_unit,
            &sets,
            &map,
            from_this_run,
            map_path,
            audit_path,
        ))
    }
}

/// What a run knows when COMMAND has ended, for [`Session::finish`].
pub struct Finish<'a> {
    /// What every unit key holds of the run's key.
    pub base: KeyBase,
    /// The scope's files now, which COMMAND ran on.
    pub tree: &'a Tree,
    pub command_passed: bool,
    /// Whether a run's result counts here: the rule a check's pass is held to (C-10, C-25).
    pub counts: &'a dyn Fn(&RunResult) -> bool,
    pub store: &'a dyn UnitResults,
    pub root: &'a Path,
    pub cwd: &'a Path,
}

/// `{total, ran, reused}`, a run's units (C-10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Counts {
    pub total: u64,
    pub ran: u64,
    pub reused: u64,
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
    records: Vec<UnitRecord>,
    mapped: BTreeMap<String, Mapped>,
    rest: BTreeMap<String, String>,
    sources: BTreeSet<String>,
    disagreements: Vec<(String, Digest)>,
    map_path: Option<PathBuf>,
    audit_path: Option<PathBuf>,
}

impl Observed {
    /// A run whose units couldn't be read: it says why, and records nothing of them.
    fn unread(check: &hushrun_core::CheckName, why: &str) -> Observed {
        Observed {
            lines: vec![format!("{check}: test files weren't observed: {why}")],
            ..Observed::default()
        }
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

    /// Records each unit that ran and passed or failed, with `run`, the check's result, recorded
    /// under `run_key`. Called only once the check's own result was recorded: unit results go
    /// with the run that executed them.
    pub fn record(
        &self,
        store: &dyn UnitResults,
        run_key: &Digest,
        run: &RunResult,
    ) -> anyhow::Result<()> {
        store.record(run_key, run, &self.records)
    }
}

/// One test file, or several, for people.
fn test_files(n: u64) -> String {
    match n {
        1 => "1 test file".into(),
        n => format!("{n} test files"),
    }
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
) -> Observed {
    let tree = end.tree;
    let rest_paths: Vec<&str> = sets.files(Set::Rest).collect();
    let rest = tree.digest(rest_paths.iter().copied());
    let sources: BTreeSet<String> = sets.files(Set::Sources).map(str::to_string).collect();
    let shape = units::shape(sources.iter().map(String::as_str));
    let key_of = |unit: &str, reads: &BTreeSet<String>| -> Option<Digest> {
        let keyed = sets.keyed(reads.iter().map(String::as_str));
        units::unit_key(&end.base, unit, &tree.digest(keyed), &rest, &shape).ok()
    };
    let run = RunFacts {
        command_passed: end.command_passed,
        failures_by_unit,
        planned: false,
    };
    let mut out = Observed {
        map_path: Some(map_path),
        audit_path: Some(audit_path),
        ..Observed::default()
    };
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
        let state = units::state(&facts, &run);
        if state.ran() {
            counts.ran += 1;
        }
        if state == State::NotObserved {
            for why in facts.not_followed.iter().map(|n| n.why.clone()) {
                *not_observed.entry(why).or_default() += 1;
            }
        }
        // What the plan would have done, from what was recorded before this run: a unit the map
        // has (or, for a compiler-given adapter, this build's list) whose key has a pass that
        // counts here, unless `tests.always` names it.
        let planned_reads = if from_this_run {
            Some(&seen.reads)
        } else {
            map.units.get(unit).map(|m| &m.reads)
        };
        let planned_key = planned_reads
            .filter(|_| !tests.always_runs(unit))
            .and_then(|reads| key_of(unit, reads));
        if let Some(key) = planned_key
            && let Some(found) = end
                .store
                .newest(&key, end.counts)
                .filter(|found| found.passed)
        {
            saved_ms = saved_ms.saturating_add(found.duration_ms);
            out.would_reuse.insert(unit.clone());
            if state == State::Failed {
                out.disagreements.push((unit.clone(), key));
            }
        }
        let key = state.recorded().and_then(|_| key_of(unit, &seen.reads));
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
    out.rest = rest_paths
        .iter()
        .filter_map(|path| {
            let (kind, hash) = tree.entry(path)?;
            Some((path.to_string(), format!("{kind} {hash}")))
        })
        .collect();
    out.sources = sources;

    // What `observe` says (RFC 0003 §1.2), in the words people read: test files, the files they
    // read.
    let check = &end.base.check;
    let reused = out.would_reuse.len() as u64;
    if counts.total > 0 {
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
    }
    for (unit, _) in &out.disagreements {
        out.lines.push(format!(
            "{check}: {unit} failed, though it passed before on the same files it read: with `tests: reuse` it would have been reused"
        ));
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

/// The results directory unit results go in: this machine's (C-33).
pub fn store() -> Option<store::Local> {
    crate::results::local_directory().map(|dir| store::Local::new(&dir))
}
