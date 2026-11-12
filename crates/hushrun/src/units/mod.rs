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
//! Four adapters: pytest and \[ADR 0071\] Vitest, observed (a plugin inside the runner); Cargo and
//! \[ADR 0070\] `go test`, compiler-given (the toolchain says what a test binary is built from).
//!
//! A [`Session`] starts before COMMAND: it makes the run's `HUSHRUN_UNITS` directory and the
//! variables that load an adapter, and nothing at all when `tests` is off. After COMMAND,
//! [`Session::finish`] reads what the adapter wrote and decides each test file's state with the
//! one function that does ([`hushrun_core::units::state`]); an audited test file that failed runs
//! once more ([`Session::again`]); the run records the check with its units. The directory goes
//! on every outcome.

pub mod adapter;
pub mod cargo;
pub mod db;
pub mod facts;
pub mod gotest;
pub mod program;
pub mod pytest;
pub mod runner;
pub mod shared;
pub mod store;
pub mod vitest;
pub mod why;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_core::digest::Digest;
use hushrun_core::time::Time;
use hushrun_core::units::{
    self, Facts, KeyBase, Mode, Reads, RunFacts, Set, State, Tests, Tree, Unit,
};

use crate::results::RunResult;
use adapter::{Adapter, Hints, Known, PlanAt, Read, ReadAt, RunExtras, StartAt};
use shared::{Asker, Choice, List, Shared};
use store::{Disagreement, Found, Mapped, Origin, UnitRecord, UnitResults};

pub use hushrun_core::units::MOST_UNITS;

/// What an adapter saw of one unit, merged over its processes.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub facts: Facts,
    pub reads: BTreeSet<String>,
    pub duration_ms: u64,
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
    adapter: Box<dyn Adapter>,
    variables: Vec<(String, OsString)>,
    tests: Tests,
    root: PathBuf,
    plan: Option<Plan>,
    /// What COMMAND's compiles get besides: the build blocks' wrapper and session (B-2), which
    /// the plan's build, COMMAND's own made first, gets too.
    compile_env: Vec<(OsString, OsString)>,
    /// \[C-39\] What this run reuses one by one, as its adapter can: `tests.unit`, or test files
    /// where the adapter knows no test.
    unit: Unit,
    /// \[C-39\] What a test is taken to have read in this run: `tests.reads`, or the test file's
    /// list where the adapter can't watch a test.
    reads: Reads,
    /// \[C-39\] What the run says once, after `<NAME>: `, of what its adapter can't do.
    notes: Vec<String>,
}

/// \[C-39, ADR 0077\] What a check whose `tests` are `tests` reuses one by one with `adapter`, and
/// what a test is taken to have read; with the lines that say, once, where the adapter can't do
/// what the check asks (RFC 0004 §1.2): it then reuses by test file, or takes each test to have
/// read what its test file read.
fn what_it_reuses(tests: &Tests, adapter: &dyn Adapter) -> (Unit, Reads, Vec<String>) {
    let knows = adapter.tests();
    let mut notes = Vec::new();
    let unit = match tests.unit {
        Unit::Test if knows.unit => Unit::Test,
        Unit::Test => {
            notes.push(format!(
                "its tests are reused by {}: {}'s tests can't be reused one by one yet",
                adapter.test_file(),
                adapter.runner()
            ));
            Unit::File
        }
        Unit::File => Unit::File,
    };
    let reads = match tests.reads {
        Reads::Test if unit == Unit::Test && knows.reads => Reads::Test,
        Reads::Test if unit == Unit::Test => {
            notes.push(format!(
                "each test is taken to have read what its {} read: {} can't be watched test by test yet",
                adapter.test_file(),
                adapter.runner()
            ));
            Reads::File
        }
        Reads::Test | Reads::File => Reads::File,
    };
    (unit, reads, notes)
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// \[ADR 0069\] What a plan decided before COMMAND (RFC 0003 §3.5 step 1).
#[derive(Debug, Default)]
pub struct Plan {
    /// \[C-38\] Each unit that may be reused: the key of the first of its lists whose key passed.
    keys: BTreeMap<String, Digest>,
    /// Of those, the newest result that counts here, a pass: what reuse stands on.
    passed: BTreeMap<String, Found>,
    /// What `hushrun.json` lists for adapters to deselect: `passed` but the audited, which run
    /// anyway (C-34).
    reuse: BTreeSet<String>,
    /// \[C-38\] The list whose key passed, of each of `passed`: one from another machine is
    /// kept here as `shared` when the test file is reused.
    chosen: BTreeMap<String, List>,
    /// \[C-38\] The hints this plan read, which this machine won't send back.
    received: Vec<db::HintKnown>,
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
        let adapter = match adapter::choose(command, cwd) {
            Ok(adapter) => adapter,
            Err(why) => return Start::Not(why),
        };
        let (unit, reads, notes) = what_it_reuses(tests, adapter.as_ref());
        let name = hushrun_ledger::new_ulid(Time::from_timestamp(jiff::Timestamp::now()).millis());
        let dir = cache.join("units").join("runs").join(name);
        let written = std::fs::create_dir_all(&dir).and_then(|()| {
            let listed = Listed {
                unit,
                reads,
                ..Listed::default()
            };
            write_run_file(&dir, root, &listed)
        });
        if let Err(err) = written {
            let _ = std::fs::remove_dir_all(&dir);
            return Start::Not(format!("can't make {}: {err}", dir.display()));
        }
        let env = |name: &str| std::env::var_os(name);
        let at = StartAt {
            cache: &cache,
            cwd,
            dir: &dir,
            env: &env,
        };
        let mut variables = match adapter.variables(&at) {
            Ok(variables) => variables,
            Err(why) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Start::Not(why);
            }
        };
        variables.push((runner::UNITS_VAR.to_string(), dir.clone().into_os_string()));
        Start::On(Box::new(Session {
            unit,
            reads,
            notes,
            dir,
            cache,
            adapter,
            variables,
            tests: tests.clone(),
            root: root.to_path_buf(),
            plan: None,
            compile_env: Vec::new(),
        }))
    }

    /// B-2: the variables every start of COMMAND gets to compile through the build blocks'
    /// wrapper, which the plan's build gets too: it is COMMAND's build, made first.
    pub fn compile_with(&mut self, env: Vec<(OsString, OsString)>) {
        self.compile_env = env;
    }

    /// \[C-39\] What the run says once, each after `<NAME>: `, of what its adapter can't do.
    pub fn notes(&self) -> &[String] {
        &self.notes
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
        let check_id = store::check_id(&input.base, input.repository);
        let loaded = db::load(&db::path(&self.cache, input.repository), &check_id);
        // \[ADR 0072\] A store there that can't be read: no map, so nothing is reused, said once.
        if let Some(unread) = &loaded.unread {
            return Planned::Everything(unread.why());
        }
        let map = loaded.map;
        let asker = self.asker(input.store, check_id, &input.base);
        let at = PlanAt {
            tree: input.tree,
            root: input.root,
            cwd: input.cwd,
            map: &map,
            asker: asker.as_ref(),
            ignore: &self.tests.ignore,
            compile_env: &self.compile_env,
        };
        let planning = match self.adapter.plan(&at) {
            Ok(Some(planning)) => planning,
            // \[ADR 0074\] No pytest ever reported here: nothing to say, nothing to reuse.
            Ok(None) => return Planned::Plan(String::new()),
            Err(why) => return Planned::Everything(why),
        };
        let adapter::Planning {
            sets,
            known,
            received,
            extras,
            bound,
        } = planning;
        let keyer = Keyer::new(input.tree, &sets, &input.base);
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
        let keyed = keyed(&keyer, &known, &self.tests);
        let lookup = match shared::lookup(input.store, all_keys(&keyed), input.counts) {
            Ok(lookup) => lookup,
            Err(why) => return Planned::Everything(format!("can't read which passed ({why})")),
        };
        if !lookup.reuse.allows_reuse() {
            return Planned::Everything(suspended(&input.base.check, &lookup.reuse));
        }
        let mut keys = BTreeMap::new();
        let mut passed: BTreeMap<String, Found> = BTreeMap::new();
        let mut chosen = BTreeMap::new();
        for (unit, lists) in keyed {
            let lists_keys: Vec<Option<Digest>> = lists.iter().map(|(key, _)| *key).collect();
            if let Choice::Passed(at, key) = shared::choose(&lists_keys, &lookup.found)
                && let Some(found) = lookup.found.get(&key)
            {
                passed.insert(unit.clone(), found.clone());
                keys.insert(unit.clone(), key);
                if let Some((_, list)) = lists.into_iter().nth(at) {
                    chosen.insert(unit, list);
                }
            }
        }
        let audited = units::audited(
            passed.keys().map(|unit| (unit.as_str(), &keys[unit])),
            &started,
        );
        let mut reuse: BTreeSet<String> = passed
            .keys()
            .filter(|unit| !audited.contains(*unit))
            .cloned()
            .collect();
        // \[ADR 0069\] A unit bound to a file's bytes (a test binary) is reused only as the bytes
        // whose dep-info its key holds.
        let mut binaries = Vec::new();
        if self.adapter.binds() {
            reuse.retain(|unit| {
                let Some(path) = bound.get(unit) else {
                    return false;
                };
                match std::fs::read(path) {
                    Ok(bytes) => {
                        binaries.push(facts::Binary {
                            unit: unit.clone(),
                            path: path.clone(),
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
            &Listed {
                reuse: reuse.iter().cloned().collect(),
                extras,
                binaries,
                unit: self.unit,
                reads: self.reads,
                reuse_tests: BTreeMap::new(),
            },
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
            chosen,
            received,
        });
        Planned::Plan(line)
    }

    /// \[C-38\] What this run asks hints with, when they can change something: `tests` is on,
    /// here, and the adapter isn't Cargo's, whose keys come from its build. Results that stay on
    /// this machine have none ([`UnitResults::hints`]), and a system that shares none asks none.
    fn asker<'s>(
        &self,
        store: &'s dyn UnitResults,
        check_id: Digest,
        base: &KeyBase,
    ) -> Option<Asker<'s>> {
        let systems = shared::systems(base);
        if self.adapter.hints() == Hints::None || systems.is_empty() || !self.tests.is_on() {
            return None;
        }
        Some(Asker {
            store,
            check_id,
            systems,
            adapter: self.adapter.word(),
        })
    }

    /// What the adapter saw, decided: each unit's state, what the plan would have reused, and the
    /// lines that say so. `None` when the adapter wrote nothing, so that everything ran and no
    /// unit is recorded.
    pub fn finish(&self, end: &Finish<'_>) -> Option<Observed> {
        let files = facts::read_all(&self.dir)?;
        let at = StoreAt {
            path: db::path(&self.cache, end.repository),
            check_id: store::check_id(&end.base, end.repository),
            base: end.base.clone(),
            adapter: self.adapter.word(),
            hints: self.adapter.hints(),
        };
        let loaded = db::load(&at.path, &at.check_id);
        let map = loaded.map;
        let how = match (end.mode, &self.plan) {
            (Mode::Reuse, Some(plan)) => How::Plan(plan),
            (Mode::Reuse, None) => How::Quiet,
            _ => How::Observe,
        };
        // \[C-38\] In `observe`, what reuse would have done reads the hints a plan would have.
        let asker = matches!(how, How::Observe)
            .then(|| self.asker(end.store, at.check_id, &end.base))
            .flatten();
        let read = match self.read(
            &files,
            end.root,
            end.cwd,
            end.tree,
            end.command_passed,
            &map,
            asker.as_ref(),
        ) {
            Ok(Some(read)) => read,
            Ok(None) => return None,
            Err(why) => return Observed::unread(&end.base.check, &why),
        };
        let mut observed = decide(end, &self.tests, read, &map, at, how);
        // \[ADR 0072\] In `observe`, what reuse would have done needs the map: say once why there
        // was none. A plan said it already.
        if let (Some(unread), Mode::Observe) = (&loaded.unread, end.mode) {
            observed.lines.push(format!(
                "{}: with `tests: reuse`, every test file would have run: {}",
                end.base.check,
                unread.why()
            ));
        }
        Some(observed)
    }

    /// What the adapter wrote in `dir`, read: each unit as seen, whether failures are reported by
    /// unit, the four sets, the lists a plan would have keyed each unit by, with \[C-38\] the
    /// hints `asker` fetches, and what the map keeps. `Ok(None)` when it wrote nothing; `Err` why
    /// it couldn't be read.
    #[allow(clippy::too_many_arguments)]
    fn read(
        &self,
        files: &[facts::FactsFile],
        root: &Path,
        cwd: &Path,
        tree: &Tree,
        command_passed: bool,
        map: &store::Map,
        asker: Option<&Asker<'_>>,
    ) -> Result<Option<Read>, String> {
        self.adapter.read(&ReadAt {
            files,
            root,
            cwd,
            tree,
            command_passed,
            map,
            asker,
            ignore: &self.tests.ignore,
        })
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
        let extras = self
            .adapter
            .again(&self.root, &observed.cwd, &observed.unit_files);
        let mut binaries = Vec::new();
        if self.adapter.binds() {
            for unit in &others {
                let path = observed.executables.get(unit)?;
                let bytes = std::fs::read(path).ok()?;
                binaries.push(facts::Binary {
                    unit: unit.clone(),
                    path: path.clone(),
                    sha256: Digest::of(&bytes).hex(),
                });
            }
        }
        let listed = Listed {
            reuse: others,
            extras,
            binaries,
            unit: self.unit,
            reads: self.reads,
            reuse_tests: BTreeMap::new(),
        };
        write_run_file(&dir, &self.root, &listed).ok()?;
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
            .read(
                &files,
                &self.root,
                &observed.cwd,
                tree,
                passed,
                &store::Map::default(),
                None,
            )
            .ok()
            .flatten();
        again
            .units
            .iter()
            .map(|unit| {
                let state = read
                    .as_ref()
                    .and_then(|read| {
                        let seen = read.seen.get(unit)?;
                        Some(units::state(
                            &seen.facts,
                            &RunFacts {
                                command_passed: passed,
                                failures_by_unit: read.failures_by_unit,
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

/// \[C-38\] The hints `asker` fetches for the check's sources in `sets` (or every file of the
/// adapter's kinds, Vitest's), read against `tree`; none without one.
fn fetch(
    asker: Option<&Asker<'_>>,
    hints: Hints,
    sets: &units::Sets,
    tree: &Tree,
    ignore: &[hushrun_core::glob::Pattern],
) -> Shared {
    asker.map_or_else(Shared::default, |asker| {
        asker.fetch(&shared::sources(hints, sets, tree, ignore), tree)
    })
}

/// \[C-22\] What `hushrun ready` needs to say how many test files a run would run.
pub struct Preview<'a> {
    pub base: KeyBase,
    pub repository: Option<&'a str>,
    pub command: &'a [String],
    pub tests: &'a Tests,
    /// The scope's files looked at.
    pub tree: &'a Tree,
    pub store: &'a dyn UnitResults,
    pub counts: &'a dyn Fn(&RunResult) -> bool,
    pub root: &'a Path,
    pub cwd: &'a Path,
}

/// \[C-22\] What a run's plan would come to, as far as `hushrun ready` can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Previewed {
    /// `run` of `total` test files would run, `reused` would be reused, and `audited` of those
    /// that could be would run as a check.
    Plan {
        total: usize,
        run: usize,
        reused: usize,
        audited: usize,
    },
    /// The check's reuse is suspended: why, as a run says it.
    Suspended(String),
}

/// \[C-22\] The plan a run of `command` would make (C-34), for `hushrun ready`, from what is on
/// this machine and one batch lookup: `None` when it can't tell without starting a tool or a
/// build (Cargo, Go), when no adapter would load, or when the lookup fails. It writes nothing,
/// and sends nothing but the lookup.
pub fn preview(p: &Preview<'_>) -> Option<Previewed> {
    let adapter = adapter::choose(p.command, p.cwd).ok()?;
    let cache = crate::context::cache_dir()?;
    let check_id = store::check_id(&p.base, p.repository);
    let loaded = db::load(&db::path(&cache, p.repository), &check_id);
    if loaded.unread.is_some() {
        return None;
    }
    let map = loaded.map;
    let systems = shared::systems(&p.base);
    let asker = (!systems.is_empty()).then(|| Asker {
        store: p.store,
        check_id,
        systems,
        adapter: adapter.word(),
    });
    let found = adapter
        .plan_from_files(&PlanAt {
            tree: p.tree,
            root: p.root,
            cwd: p.cwd,
            map: &map,
            asker: asker.as_ref(),
            ignore: &p.tests.ignore,
            compile_env: &[],
        })
        .ok()??;
    let total = found.known.len();
    if total == 0 || total > MOST_UNITS {
        return None;
    }
    let keyer = Keyer::new(p.tree, &found.sets, &p.base);
    let keyed = keyed(&keyer, &found.known, p.tests);
    let lookup = shared::lookup(p.store, all_keys(&keyed), p.counts).ok()?;
    if !lookup.reuse.allows_reuse() {
        return Some(Previewed::Suspended(suspended(
            &p.base.check,
            &lookup.reuse,
        )));
    }
    let passed = keyed
        .values()
        .filter(|lists| {
            let keys: Vec<Option<Digest>> = lists.iter().map(|(key, _)| *key).collect();
            matches!(shared::choose(&keys, &lookup.found), Choice::Passed(..))
        })
        .count();
    let audited = units::audit_share(passed);
    let reused = passed - audited;
    Some(Previewed::Plan {
        total,
        run: total - reused,
        reused,
        audited,
    })
}

/// Each test file's lists with the key of each for the files on disk now (C-31): those
/// `tests.always` names and those no list is known for left out.
type Keyed = BTreeMap<String, Vec<(Option<Digest>, List)>>;

fn keyed(keyer: &Keyer<'_>, known: &Known, tests: &Tests) -> Keyed {
    known
        .iter()
        .filter(|(unit, _)| !tests.always_runs(unit))
        .filter_map(|(unit, lists)| {
            let lists = lists
                .as_ref()?
                .iter()
                .map(|list| (keyer.key(unit, &list.files), list.clone()))
                .collect();
            Some((unit.clone(), lists))
        })
        .collect()
}

/// Every key of `keyed`.
fn all_keys(keyed: &Keyed) -> impl Iterator<Item = Digest> + '_ {
    keyed.values().flatten().filter_map(|(key, _)| *key)
}

/// \[ADR 0069\] The one list a Cargo test binary is keyed by: what its dep-info says it is built
/// from.
fn built_list(reads: &BTreeSet<String>) -> List {
    List {
        files: reads.clone(),
        reads: reads.clone(),
        dirs: BTreeSet::new(),
        from: shared::From::Build,
    }
}

/// What `hushrun.json` lists for adapters (C-32).
#[derive(Debug, Default)]
struct Listed {
    /// Units an adapter may deselect whole.
    reuse: Vec<String>,
    /// What the adapter checks before it deselects any.
    extras: RunExtras,
    /// \[ADR 0069\] The reused test binaries.
    binaries: Vec<facts::Binary>,
    /// \[ADR 0077\] What the run reuses one by one, and what a test is taken to have read.
    unit: Unit,
    reads: Reads,
    /// \[ADR 0077\] Tests an adapter may deselect one by one, by unit, each by its id.
    reuse_tests: BTreeMap<String, Vec<String>>,
}

/// \[ADR 0069\] Writes `hushrun.json` in `dir` whole: the repository's root and what `listed`
/// says.
fn write_run_file(dir: &Path, root: &Path, listed: &Listed) -> std::io::Result<()> {
    let run = facts::RunFile {
        schema: facts::RUN_SCHEMA.into(),
        root: root.to_path_buf(),
        reuse: listed.reuse.clone(),
        python_files: listed.extras.python_files.clone(),
        binaries: listed.binaries.clone(),
        test_files: listed.extras.test_files.clone(),
        unit: Some(listed.unit.word().to_string()),
        reads: Some(listed.reads.word().to_string()),
        reuse_tests: listed.reuse_tests.clone(),
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
    /// \[ADR 0074\] In a run that reused test files, how long they took when they last ran, by
    /// their recorded results: what reusing saved, which the dashboard shows beside what skips
    /// saved. Never in `observe`, which saved nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_ms: Option<u64>,
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
    /// \[C-34\] Test files whose key had a pass that counted, which ran and failed: each runs once
    /// more ([`Session::again`]), with the key of that pass.
    pub again: BTreeMap<String, Digest>,
    records: Vec<UnitRecord>,
    mapped: BTreeMap<String, Mapped>,
    rest: BTreeMap<String, String>,
    sources: BTreeSet<String>,
    disagreements: Vec<(String, Digest)>,
    /// \[ADR 0072\] Where what this run learned goes: this machine's store, and the check in it.
    store: Option<StoreAt>,
    edges: BTreeSet<db::Edge>,
    executables: BTreeMap<String, PathBuf>,
    cwd: PathBuf,
    /// \[ADR 0071\] The test files the runner listed, for the map and a second run.
    unit_files: BTreeSet<String>,
    /// \[C-38\] The hints this run read, which it notes as known.
    received: Vec<db::HintKnown>,
    /// \[C-38\] Where this run's hint goes, and its key.
    share: Option<shared::Place>,
    /// \[C-38\] What this run's hint says of each test file reused on what its build is made of
    /// alone, which no row of the store holds.
    built: BTreeMap<String, hushrun_core::hints::Unit>,
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

    /// Writes what this run learned for the next into this machine's store, in one transaction
    /// (ADR 0072): the map, the edges and the audit's disagreements. Only when the files didn't
    /// change while COMMAND ran, which the caller checked. A store that can't be written costs the
    /// next run its reuse, and nothing else.
    pub fn keep(&self, run_key: &Digest, tree: &Tree, now: Time) {
        let Some(at) = &self.store else {
            return;
        };
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
        let write = db::Write {
            check_id: at.check_id,
            base: &at.base,
            adapter: at.adapter,
            seen: &self.mapped,
            tree,
            rest: &self.rest,
            sources: &self.sources,
            unit_files: (!self.unit_files.is_empty()).then_some(&self.unit_files),
            edges: &self.edges,
            disagreements: &found,
            hints: &self.received,
            now_ms: now.millis(),
        };
        if let Err(err) = db::write(&at.path, &write) {
            tracing::debug!("what the test files read wasn't kept: {}", err.why());
        }
    }

    /// \[C-38\] Sends what this machine's store now says this run's test files read, as a hint
    /// under this machine's system: every test file it executed and fully observed, and every one
    /// it reused, with the list whose key hit. After [`Observed::keep`], for a run recorded on a
    /// server that keeps test files' results. One `PUT`, and none when this machine already sent
    /// or received a hint with its content id for the address and system. A `PUT` that fails
    /// leaves no note, so the next run of the check sends it again; it is said for debug only, as
    /// the store's errors are. `Some` line to say once when the hint is too large to send.
    pub fn share(&self, results: &dyn UnitResults, now: Time) -> Option<String> {
        let (Some(at), Some(place), Some(os)) =
            (&self.store, &self.share, hushrun_core::hints::Os::this())
        else {
            return None;
        };
        let loaded = db::load(&at.path, &at.check_id);
        if let Some(unread) = loaded.unread {
            tracing::debug!("no hint sent: {}", unread.why());
            return None;
        }
        let map = loaded.map;
        let units: BTreeMap<String, hushrun_core::hints::Unit> = self
            .units
            .iter()
            .filter(|(_, (state, _))| state.recorded().is_some() || *state == State::Reused)
            .filter_map(|(unit, _)| {
                let Some(mapped) = map.units.get(unit) else {
                    return Some((unit.clone(), self.built.get(unit)?.clone()));
                };
                // Another system's list stays that system's: never sent as this one's.
                let ours = match mapped.origin {
                    Origin::SeenHere => true,
                    Origin::Shared(from) => from == os,
                };
                (ours && mapped.not_followed.is_empty()).then(|| {
                    let unit_seen = hushrun_core::hints::Unit {
                        reads: mapped.reads.keys().cloned().collect(),
                        dirs: mapped.dirs.clone(),
                    };
                    (unit.clone(), unit_seen)
                })
            })
            .collect();
        if units.is_empty() {
            return None;
        }
        // \[ADR 0071\] Only an adapter whose hints go by its kinds (Vitest) has a list of test
        // files that is its runner's own.
        let unit_files = match at.hints {
            Hints::Kinds(_) => map.unit_files,
            Hints::None | Hints::Sources => BTreeSet::new(),
        };
        let hint = hushrun_core::hints::Hint {
            adapter: at.adapter.to_string(),
            units,
            unit_files,
        };
        let written = match hushrun_core::hints::write(&hint, &place.key) {
            Ok(written) => written,
            Err(hushrun_core::hints::Unwritable::TooLarge { what, size, most }) => {
                let what = match what {
                    "unit files" => "test files listed",
                    "entries" => "files read",
                    "paths" => "files",
                    "bytes of content" | "bytes compressed" => "bytes",
                    other => other,
                };
                return Some(format!(
                    "{}: what its test files read wasn't shared with other machines: it is too large ({size} {what}, more than {most})",
                    at.base.check
                ));
            }
            Err(err) => {
                tracing::debug!("no hint sent: {err}");
                return None;
            }
        };
        if db::hint_known(
            &at.path,
            &at.check_id,
            &place.address,
            os,
            &written.content_id,
        ) {
            return None;
        }
        let to = store::HintPlace {
            check_id: at.check_id,
            address: place.address,
            os,
        };
        match results.put_hint(&to, &written.bytes) {
            Ok(()) => {
                let sent = db::HintKnown {
                    address: place.address,
                    os,
                    content_id: written.content_id,
                    how: db::HintHow::Sent,
                };
                if let Err(err) = db::note_hints(&at.path, &at.check_id, &[sent], now.millis()) {
                    tracing::debug!("the hint sent wasn't noted: {}", err.why());
                }
            }
            Err(why) => tracing::debug!("the hint wasn't sent: {why}"),
        }
        None
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

/// \[ADR 0072\] Where a run's map goes: the repository's store, the check's id in it, and what the
/// store says of the check.
#[derive(Debug)]
struct StoreAt {
    path: PathBuf,
    check_id: Digest,
    base: KeyBase,
    adapter: &'static str,
    hints: Hints,
}

fn decide(
    end: &Finish<'_>,
    tests: &Tests,
    read: Read,
    map: &store::Map,
    at: StoreAt,
    how: How<'_>,
) -> Observed {
    let Read {
        seen,
        failures_by_unit,
        sets,
        planned,
        executables,
        own,
        dirs,
        unit_files,
        refused,
        edges,
        received,
    } = read;
    let sets = &sets;
    let tree = end.tree;
    let keyer = Keyer::new(tree, sets, &end.base);
    let sources: BTreeSet<String> = sets.files(Set::Sources).map(str::to_string).collect();
    // \[C-38\] Where this run's hint goes: none for Cargo, whose keys need none, nor from a system
    // that shares none.
    let share = (at.hints != Hints::None && hushrun_core::hints::Os::this().is_some()).then(|| {
        shared::Place::new(
            &at.check_id,
            &shared::sources(at.hints, sets, tree, &tests.ignore),
        )
    });
    let received = match &how {
        How::Plan(plan) => plan.received.clone(),
        How::Observe | How::Quiet => received,
    };
    let mut out = Observed {
        store: Some(at),
        edges,
        cwd: end.cwd.to_path_buf(),
        executables,
        unit_files: unit_files.unwrap_or_default(),
        received,
        share,
        ..Observed::default()
    };
    if let (How::Plan(plan), Some(why)) = (&how, &refused)
        && !plan.reuse.is_empty()
    {
        out.lines.push(format!(
            "{}: no test file was reused: {why}",
            end.base.check
        ));
    }
    // What the plan would have done, in `observe`: from what was recorded before this run, a unit
    // the plan would have keyed (by the lists \[C-38\] gives, or for Cargo this build's) that the
    // one rule would reuse, unless `tests.always` names it. One batch, in pages.
    let mut would: BTreeMap<String, (Digest, Found)> = BTreeMap::new();
    if matches!(how, How::Observe) {
        let known: Known = planned
            .iter()
            .filter(|(unit, _)| seen.contains_key(*unit))
            .map(|(unit, lists)| (unit.clone(), Some(lists.clone())))
            .collect();
        let keyed = keyed(&keyer, &known, tests);
        if let Ok(lookup) = shared::lookup(end.store, all_keys(&keyed), end.counts) {
            for (unit, lists) in keyed {
                let keys: Vec<Option<Digest>> = lists.iter().map(|(key, _)| *key).collect();
                if let Choice::Passed(_, key) = shared::choose(&keys, &lookup.found)
                    && let Some(found) = lookup.found.get(&key)
                {
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
        let outside = match own.get(unit) {
            Some(files) => files.iter().find(|f| !tree.contains(f)).cloned(),
            None => (!tree.contains(unit)).then(|| unit.clone()),
        };
        if let Some(file) = outside {
            facts.not_followed.insert(units::NotFollowed {
                why: "outside-scope".into(),
                detail: file,
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
                    // \[C-38\] Reused on a list another machine saw: this machine keeps it, as
                    // `shared` with that machine's system, until it runs here. Reused on what its
                    // build is made of alone (Go with no entry here): what this run's hint says
                    // of it, which no row holds.
                    match plan.chosen.get(unit).map(|list| (list, list.from)) {
                        Some((list, shared::From::Hint(os))) => {
                            out.mapped
                                .insert(unit.clone(), shared_row(list, os, sets, tree));
                        }
                        Some((list, shared::From::Build)) => {
                            let seen = hushrun_core::hints::Unit {
                                reads: sets
                                    .keyed(list.files.iter().map(String::as_str))
                                    .into_iter()
                                    .map(str::to_string)
                                    .collect(),
                                dirs: BTreeSet::new(),
                            };
                            out.built.insert(unit.clone(), seen);
                        }
                        _ => {}
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
                        .filter_map(|path| {
                            let (kind, hash) = tree.entry(path)?;
                            Some((path.to_string(), format!("{kind} {hash}")))
                        })
                        .collect(),
                    not_followed: facts.not_followed.clone(),
                    dirs: dirs.get(unit).cloned().unwrap_or_default(),
                    origin: Origin::SeenHere,
                },
            );
        }
        out.units.insert(unit.clone(), (state, key));
    }
    out.rest = rest_of(sets, tree);
    out.sources = sources;
    if matches!(how, How::Plan(_)) && counts.reused > 0 {
        counts.saved_ms = Some(saved_ms);
    }
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
                None if !map.reported() => {
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

/// \[C-38\] The row a test file reused on another machine's list gets in this machine's store: the
/// files of the list its key holds, as they are now, which are as they were when it passed.
fn shared_row(list: &List, os: hushrun_core::hints::Os, sets: &units::Sets, tree: &Tree) -> Mapped {
    Mapped {
        reads: sets
            .keyed(list.reads.iter().map(String::as_str))
            .into_iter()
            .filter_map(|path| {
                let (kind, hash) = tree.entry(path)?;
                Some((path.to_string(), format!("{kind} {hash}")))
            })
            .collect(),
        not_followed: BTreeSet::new(),
        dirs: list.dirs.clone(),
        origin: Origin::Shared(os),
    }
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
        "test-main" => "a TestMain".into(),
        "go-list" => "what go list couldn't load".into(),
        "test-log" => "a test log Go didn't write whole".into(),
        "virtual" => "a module a Vite plugin made".into(),
        "worker" => "a worker thread".into(),
        "unseen" => "what hushrun's setup file didn't see".into(),
        "pool" | "isolate" => "test files that share a worker".into(),
        "plugin" => "a Vite plugin hushrun doesn't know".into(),
        "global-setup" => "a globalSetup".into(),
        "environment" | "runner" => "an environment or runner of the repository's".into(),
        "browser" | "typecheck" | "css" => "what Vitest does outside its workers".into(),
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
    if !map.reported() {
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

#[cfg(test)]
mod tests;
