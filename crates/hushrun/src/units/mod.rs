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
//! \[ADR 0077\] Each is one module behind one trait ([`adapter::Adapter`]), and the code here is
//! the same for every one.
//!
//! \[ADR 0077\] With `tests.unit: test`, a test is a second, finer unit (C-39): the plan asks each
//! test file's own key, then the tests of each that didn't pass (`plan_tests`); a run records a
//! test file that passed whole as before, and each test that ran of one that didn't.
//!
//! A [`Session`] starts before COMMAND: it makes the run's `HUSHRUN_UNITS` directory and the
//! variables that load an adapter, and nothing at all when `tests` is off. After COMMAND,
//! [`Session::finish`] reads what the adapter wrote and decides each test file's state with the
//! one function that does ([`hushrun_core::units::state`]); an audited test file that failed runs
//! once more ([`Session::again`]); the run records the check with its units. The directory goes
//! on every outcome.

pub mod adapter;
pub mod cargo;
pub mod command;
pub mod ctest;
pub mod db;
pub mod facts;
pub mod gotest;
pub mod gradle;
pub mod jest;
pub mod jvm;
pub mod maven;
pub mod program;
pub mod pytest;
pub mod runner;
pub mod shared;
pub mod store;
pub mod swift;
pub mod vitest;
pub mod why;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_core::digest::Digest;
use hushrun_core::time::Time;
use hushrun_core::units::{
    self, Decision, Facts, KeyBase, Mode, Reads, RunFacts, Set, State, Tests, Tree, Unit,
};

use crate::cmd::usage;
use crate::results::RunResult;
use adapter::{Adapter, Hints, Known, PlanAt, Read, ReadAt, RunExtras, StartAt};
use shared::{Asker, Choice, List, Shared};
use store::{Disagreement, Found, Mapped, Origin, UnitRecord, UnitResults};

pub use hushrun_core::units::{MOST_TESTS, MOST_UNITS};

/// What an adapter saw of one unit, merged over its processes.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub facts: Facts,
    pub reads: BTreeSet<String>,
    pub duration_ms: u64,
    /// \[ADR 0077\] Its tests, as the adapter told them, merged over its processes; `None` when it
    /// told none (C-39).
    pub tests: Option<BTreeMap<String, facts::MergedTest>>,
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
    /// \[C-32\] The directory COMMAND's start writes in: `dir`, or for a start that leaves
    /// nothing out after one that left out what the plan didn't list, one of its own inside it.
    run_dir: PathBuf,
    /// Where COMMAND runs.
    cwd: PathBuf,
    /// \[C-32\] This start is the one with nothing left out, after one whose adapter left out
    /// what the plan didn't list.
    whole: bool,
    /// \[C-32\] pytest's adapter was chosen only because this machine's store said pytest last
    /// reported the check ([`adapter::choose_kept`]).
    kept: bool,
    /// \[C-32\] COMMAND's own `env NAME=value` assignments, which every tool an adapter starts
    /// for it gets.
    assigned: Vec<(String, String)>,
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
    /// \[C-39\] What the plan says once, before its line.
    plan_notes: Vec<String>,
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
    /// \[C-39\] The second tier: the tests that may be reused one by one.
    tests: PlannedTests,
}

/// \[C-39\] A test whose key had a pass that counts here, and nothing that failed: what its reuse
/// stands on.
#[derive(Debug, Clone)]
struct TestPassed {
    unit: String,
    id: String,
    key: Digest,
    found: Found,
    /// \[C-41\] The list whose key passed: one another machine saw becomes this test's row here.
    list: shared::TestList,
}

/// \[C-39, ADR 0077\] What a plan decided of tests one by one: each that may be reused, by name,
/// and of those the audited, which run anyway.
#[derive(Debug, Default)]
struct PlannedTests {
    passed: BTreeMap<String, TestPassed>,
    audited: BTreeSet<String>,
    /// \[C-34\] The tests, by name, of each test file the first tier could reuse whole that holds
    /// a test the audit runs: its other tests are left out one by one instead.
    whole: BTreeMap<String, (String, String)>,
    /// \[C-41\] How many tests this machine knows of each test file, from its rows and the hints
    /// it read: what the plan's line counts.
    known: BTreeMap<String, usize>,
}

impl PlannedTests {
    /// What `hushrun.json`'s `reuseTests` lists: the tests that may be reused but the audited, by
    /// unit.
    fn listed(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let tests = self
            .passed
            .iter()
            .map(|(name, test)| (name, (&test.unit, &test.id)))
            .chain(
                self.whole
                    .iter()
                    .map(|(name, (unit, id))| (name, (unit, id))),
            );
        for (name, (unit, id)) in tests {
            if !self.audited.contains(name) {
                out.entry(unit.clone()).or_default().push(id.clone());
            }
        }
        out
    }

    /// Whether `reuseTests` lists the test `id` of `unit`.
    fn lists(&self, unit: &str, id: &str) -> bool {
        let name = units::test_name(unit, id);
        (self.passed.contains_key(&name) || self.whole.contains_key(&name))
            && !self.audited.contains(&name)
    }

    /// \[C-34\] The test files the first tier could reuse whole of which the audit runs a test.
    fn audited_whole(&self) -> BTreeSet<String> {
        self.whole
            .iter()
            .filter(|(name, _)| self.audited.contains(*name))
            .map(|(_, (unit, _))| unit.clone())
            .collect()
    }
}

/// \[C-34\] With `unit: test`, the test files of `passed` whose tests this machine doesn't know
/// (`tested.known`): they can't be left out test by test, so they are audited whole, 2% of them,
/// at least one, as C-34 audits test files.
fn untold_audited(
    passed: &BTreeMap<String, Found>,
    keys: &BTreeMap<String, Digest>,
    tested: &PlannedTests,
    started: &str,
) -> BTreeSet<String> {
    units::audited(
        passed
            .keys()
            .filter(|unit| tested.known.get(*unit).is_none_or(|n| *n == 0))
            .filter_map(|unit| Some((unit.as_str(), keys.get(unit)?))),
        started,
    )
}

/// What the second tier of a plan goes by.
struct TestsAt<'a> {
    keyer: &'a Keyer<'a>,
    /// \[C-40\] What a test is taken to have read in this run.
    reads: Reads,
    tree: &'a Tree,
    keyed: &'a Keyed,
    map: &'a store::Map,
    /// \[C-41\] The hints the adapter read, with the tests they hold.
    shared: &'a Shared,
    /// The test files the first tier may reuse whole: their tests aren't asked.
    passed: &'a BTreeMap<String, Found>,
    /// \[C-34\] Of those, the key whose pass reuses each: what its tests are audited by.
    keys: &'a BTreeMap<String, Digest>,
    store: &'a dyn UnitResults,
    counts: &'a dyn Fn(&RunResult) -> bool,
    started: &'a str,
}

/// \[C-39\] More tests than a check reuses one by one.
struct TooMany(usize);

/// \[C-39, C-41\] The tests a plan knows of `unit`, by id, each with the lists its key may be made
/// from (\[C-40\] [`shared::test_lists`]): those this machine saw run, and those hints hold of it.
/// One that wasn't followed the last time it ran here has none.
fn tests_known(
    reads: Reads,
    unit: &str,
    file_lists: &[List],
    map: &store::Map,
    hints: &Shared,
    tree: &Tree,
) -> BTreeMap<String, Vec<shared::TestList>> {
    let rows = map.units.get(unit).map(|mapped| &mapped.tests);
    let mut ids: BTreeSet<&str> = hints.test_ids(unit);
    ids.extend(
        rows.into_iter()
            .flat_map(|rows| rows.keys().map(String::as_str)),
    );
    ids.into_iter()
        .map(|id| {
            let row = rows.and_then(|rows| rows.get(id));
            // Not followed the last time it ran here: a test of the file, with nothing to key it
            // by.
            if row.is_some_and(|row| row.state == State::NotObserved) {
                return (id.to_string(), Vec::new());
            }
            let seen = hints.test_seen(unit, id);
            let lists = shared::test_lists(reads, file_lists, row, &seen, tree);
            (id.to_string(), lists)
        })
        .collect()
}

/// \[C-39, ADR 0077\] The second tier of a plan (RFC 0004 §3.3): for each test file the first tier
/// didn't reuse whole whose lists are known, each of its tests this machine saw run (not one that
/// wasn't followed then) and \[C-41\] each a hint holds, keyed by its lists with its id; the one
/// rule decides; and 2% of those that may be reused, at least one, run anyway. A lookup that fails
/// reuses no test: they run. `Err` past [`MOST_TESTS`] tests.
fn plan_tests(at: &TestsAt<'_>) -> Result<PlannedTests, TooMany> {
    let mut asked_of: BTreeMap<&String, BTreeMap<String, Vec<shared::TestList>>> = BTreeMap::new();
    for (unit, lists) in at.keyed {
        let file_lists: Vec<List> = lists.iter().map(|(_, list)| list.clone()).collect();
        let known = tests_known(at.reads, unit, &file_lists, at.map, at.shared, at.tree);
        if !known.is_empty() {
            asked_of.insert(unit, known);
        }
    }
    let total: usize = asked_of.values().map(BTreeMap::len).sum();
    if total > MOST_TESTS {
        return Err(TooMany(total));
    }
    let known = asked_of
        .iter()
        .map(|(unit, tests)| ((*unit).clone(), tests.len()))
        .collect();
    // \[C-34\] The tests of the test files the first tier may reuse whole, each audited by its
    // file's key and its id.
    let whole_keys: BTreeMap<String, (String, String, Digest)> = asked_of
        .iter()
        .filter_map(|(unit, tests)| Some((*unit, tests, at.keys.get(*unit)?)))
        .flat_map(|(unit, tests, key)| {
            tests.keys().map(move |id| {
                (
                    units::test_name(unit, id),
                    (
                        unit.clone(),
                        id.clone(),
                        Digest::of(format!("{}\n{id}", key.hex()).as_bytes()),
                    ),
                )
            })
        })
        .collect();
    let asked = asked_of
        .into_iter()
        .filter(|(unit, _)| !at.passed.contains_key(*unit))
        .flat_map(|(unit, tests)| {
            tests
                .into_iter()
                .map(move |(id, lists)| (unit.clone(), id, lists))
        });
    let passed = tests_that_passed(at.keyer, asked, at.store, at.counts).unwrap_or_default();
    // \[C-34\] 2% of every test the plan could leave out, in test files reused whole or not, at
    // least one, chosen as C-39 chooses: an audit costs 2% of the tests, whatever a file's size.
    let audited = units::audited(
        passed
            .iter()
            .map(|(name, t)| (name.as_str(), &t.key))
            .chain(
                whole_keys
                    .iter()
                    .map(|(name, (_, _, key))| (name.as_str(), key)),
            ),
        at.started,
    );
    // A test file reused whole of which a test is audited is left out test by test instead.
    let audited_files: BTreeSet<&String> = whole_keys
        .iter()
        .filter(|(name, _)| audited.contains(*name))
        .map(|(_, (unit, _, _))| unit)
        .collect();
    let whole = whole_keys
        .iter()
        .filter(|(_, (unit, _, _))| audited_files.contains(unit))
        .map(|(name, (unit, id, _))| (name.clone(), (unit.clone(), id.clone())))
        .collect();
    Ok(PlannedTests {
        passed,
        audited,
        whole,
        known,
    })
}

/// \[C-39\] The one place tests are chosen, for a plan and for what `observe` says: of each test,
/// keyed by each of its lists (\[C-40\] [`shared::test_lists`]), those whose keys the one rule
/// ([`shared::choose`]) would reuse, by name, from one batch lookup. `None` when the lookup failed, or the check's
/// reuse is suspended.
fn tests_that_passed(
    keyer: &Keyer<'_>,
    asked: impl Iterator<Item = (String, String, Vec<shared::TestList>)>,
    store: &dyn UnitResults,
    counts: &dyn Fn(&RunResult) -> bool,
) -> Option<BTreeMap<String, TestPassed>> {
    type Asked = (String, String, Vec<Option<Digest>>, Vec<shared::TestList>);
    let mut keyed: Vec<Asked> = Vec::new();
    for (unit, id, lists) in asked {
        let keys = lists
            .iter()
            .map(|list| keyer.test_key(&unit, &id, &list.list.files))
            .collect();
        keyed.push((unit, id, keys, lists));
    }
    if keyed.is_empty() {
        return Some(BTreeMap::new());
    }
    let all = keyed
        .iter()
        .flat_map(|(_, _, keys, _)| keys.iter().flatten().copied());
    let lookup = shared::lookup(store, all, counts).ok()?;
    if !lookup.reuse.allows_reuse() {
        return None;
    }
    let mut out = BTreeMap::new();
    for (unit, id, keys, mut lists) in keyed {
        if let Choice::Passed(at, key) = shared::choose(&keys, &lookup.found)
            && let Some(found) = lookup.found.get(&key)
            && at < lists.len()
        {
            let name = units::test_name(&unit, &id);
            out.insert(
                name,
                TestPassed {
                    unit,
                    id,
                    key,
                    found: found.clone(),
                    list: lists.swap_remove(at),
                },
            );
        }
    }
    Some(out)
}

/// \[C-39\] Each of a test file's tests `told`, with its facts and its decision by the one
/// function: it finished, failed and was deselected as told, and what wasn't followed is what was
/// inside its own window and what `file` says was outside every test's window of its file.
/// `run(id)` is what the run knows for it, the plan's listing of it included. When its test file
/// failed and none of `told` did, no test of it passes: the failure is unexplained. \[C-32\] What
/// holds for the whole run (a start that never ended, a stale build) holds for each test.
fn tests_decided(
    told: &BTreeMap<String, facts::MergedTest>,
    file: &Facts,
    run: impl Fn(&str) -> RunFacts,
) -> BTreeMap<String, (Decision, Facts)> {
    let unexplained = file.failed && told.values().all(|test| !test.failed);
    told.iter()
        .map(|(id, test)| {
            let mut not_followed = test.not_followed.clone();
            not_followed.extend(file.not_followed.iter().cloned());
            let facts = Facts {
                complete: test.finished,
                failed: test.failed,
                not_followed,
                deselected: test.deselected,
                tests_reused: false,
                skipped: test.skipped,
                unexplained,
                unfinished_start: file.unfinished_start,
                stale_build: file.stale_build,
                never_left_out: test.never_left_out,
                left_out_again: file.left_out_again,
            };
            let decision = units::decided(&facts, &run(id));
            (id.clone(), (decision, facts))
        })
        .collect()
}

/// \[C-37\] What the run's adapters said of why no pass of the run is kept, which `hushrun why`
/// says in place of the core's own sentence for the rule it goes with.
#[derive(Debug, Clone, Default)]
struct RunWords {
    /// Why a start of the runner didn't say what it saw: its `.started` file's `why`.
    started: Option<String>,
    /// Why a run that failed keeps no pass, its failures not told by unit: a facts file's
    /// `notKept`.
    failed: Option<String>,
}

/// \[C-37\] Why a test or a test file kept nothing, in people's words, as `decision` says: for a
/// test that didn't finish, its adapter's words when it gave some (`told`); for a rule of the
/// whole run, what its adapter said of it (`words`); else the rule's.
fn not_kept(
    decision: Decision,
    test: bool,
    told: Option<&str>,
    words: &RunWords,
) -> Option<String> {
    let why = decision.not_kept?;
    let said = match why {
        units::NotKept::Unfinished => told.map(str::to_string),
        units::NotKept::UnfinishedStart => words
            .started
            .as_ref()
            .map(|why| format!("{why}, so no pass of that run is kept")),
        units::NotKept::CommandFailed => words.failed.clone(),
        _ => None,
    };
    Some(
        said.filter(|s| !s.is_empty())
            .unwrap_or_else(|| why.sentence(test).to_string()),
    )
}

/// \[C-37\] Why a test file kept no result of its own, as `decision` says: when the rule that
/// decided it is one a test of it was decided by too (not every test finished, a skip in a run
/// that failed), through the first such test and the reason its row keeps (`tests`), so that a
/// test a failing wider fixture served names the fixture for its file too; else the rule's.
fn file_not_kept(
    decision: Decision,
    tests: &BTreeMap<String, store::TestRow>,
    words: &RunWords,
) -> Option<String> {
    let through = match decision.not_kept? {
        units::NotKept::Unfinished | units::NotKept::SkippedInFailedRun => tests
            .iter()
            .find(|(_, row)| row.state == State::NotRun && row.not_kept.is_some()),
        _ => None,
    };
    match through {
        Some((id, row)) => Some(format!(
            "its test {id} kept no result: {}",
            row.not_kept.as_deref().unwrap_or_default()
        )),
        None => not_kept(decision, false, None, words),
    }
}

/// \[C-39, C-22\] What a plan reuses when tests are the unit, counted for the run's line and for
/// `hushrun ready`: the tests, when this machine knows every test file's (its rows or \[C-41\]
/// hints), and the test files, with the tests reused one by one beside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestsCounted {
    /// Every test, and those reused, whole or one by one; `None` when some test file's tests
    /// aren't known here.
    pub tests: Option<(usize, usize)>,
    pub files_total: usize,
    /// The test files that run: neither reused whole nor every test of them one by one.
    pub files_run: usize,
    pub files_reused: usize,
    /// The tests reused one by one.
    pub one_by_one: usize,
}

fn tests_counted<'a>(
    units: impl Iterator<Item = &'a String> + Clone,
    reuse: &BTreeSet<String>,
    tested: &PlannedTests,
) -> TestsCounted {
    let listed = tested.listed();
    let one_by_one: usize = listed.values().map(Vec::len).sum();
    let tests_of = |unit: &str| tested.known.get(unit).copied().unwrap_or(0);
    let all_of = |unit: &str| {
        listed
            .get(unit)
            .is_some_and(|ids| !ids.is_empty() && ids.len() == tests_of(unit))
    };
    let files_total = units.clone().count();
    let files_run = units
        .clone()
        .filter(|unit| !reuse.contains(*unit) && !all_of(unit))
        .count();
    let tests = units.clone().all(|unit| tests_of(unit) > 0).then(|| {
        let total: usize = units.map(|unit| tests_of(unit)).sum();
        let reused: usize = reuse.iter().map(|unit| tests_of(unit)).sum::<usize>() + one_by_one;
        (total, reused)
    });
    TestsCounted {
        tests,
        files_total,
        files_run,
        files_reused: reuse.len(),
        one_by_one,
    }
}

/// \[C-39\] The line before COMMAND when tests are the unit (RFC 0004 §1.2): `None` when nothing
/// is reused, and the test files' lines say why. Tests are counted where this machine knows every
/// test file's tests, from its rows or \[C-41\] hints; else test files are, with the tests reused
/// one by one beside them.
fn tests_line(
    check: &hushrun_core::CheckName,
    known: &Known,
    reuse: &BTreeSet<String>,
    audited: &BTreeSet<String>,
    tested: &PlannedTests,
    noun: &str,
) -> Option<String> {
    let listed = tested.listed();
    let one_by_one: usize = listed.values().map(Vec::len).sum();
    let tests_of = |unit: &str| tested.known.get(unit).copied().unwrap_or(0);
    if reuse.is_empty() && one_by_one == 0 {
        // Only tests run as a check: said as test files' are, with tests.
        let checked = tested.audited.len();
        if checked == 0 || !audited.is_empty() {
            return None;
        }
        let total: usize = known.iter().map(|(unit, _)| tests_of(unit)).sum();
        return Some(format!(
            "{check}: all {} run; {}",
            count_tests(total as u64),
            checked_only(checked)
        ));
    }
    let counted = tests_counted(known.iter().map(|(unit, _)| unit), reuse, tested);
    let files_total = counted.files_total as u64;
    let files_run = counted.files_run as u64;
    let more = |line: &mut String, n: usize| line.push_str(&checked_too(n));
    if let Some((total, reused)) = counted.tests {
        let checked: usize =
            audited.iter().map(|unit| tests_of(unit)).sum::<usize>() + tested.audited.len();
        let mut line = format!(
            "{check}: {} of {} run, in {} of {}; {} already passed on the files {} read",
            usage::thousands((total - reused) as u64),
            count_tests(total as u64),
            usage::thousands(files_run),
            test_files_grouped(files_total, noun),
            usage::thousands(reused as u64),
            if reused == 1 { "it" } else { "they" },
        );
        more(&mut line, checked);
        return Some(line);
    }
    let mut line = format!(
        "{check}: {} of {} run; {} already passed on the files {} read",
        usage::thousands(files_run),
        test_files_grouped(files_total, noun),
        usage::thousands((reuse.len() + one_by_one) as u64),
        if reuse.len() + one_by_one == 1 {
            "it"
        } else {
            "they"
        },
    );
    if one_by_one > 0 {
        line = format!(
            "{check}: {} of {} run; {} already passed on the files {} read, and so did {} of the others",
            usage::thousands(files_run),
            test_files_grouped(files_total, noun),
            usage::thousands(reuse.len() as u64),
            if reuse.len() == 1 { "it" } else { "they" },
            count_tests(one_by_one as u64),
        );
    }
    // \[C-34\] Counted test files and tests apart: the audit's tests, and the test files it
    // runs whole whose tests this machine doesn't know.
    line.push_str(&checked_apart(audited.len(), tested.audited.len(), noun));
    Some(line)
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
    /// \[C-32\] `stored`: the adapter that last reported the check's test files here, as this
    /// machine's store names it ([`db::adapter_of`]), which keeps a COMMAND pytest reported
    /// pytest's ([`adapter::choose_kept`]).
    pub fn start(
        tests: Option<&Tests>,
        command: &[String],
        root: &Path,
        cwd: &Path,
        stored: Option<&str>,
    ) -> Start {
        let Some(tests) = tests.filter(|t| t.is_on()) else {
            return Start::Off;
        };
        let Some(cache) = crate::context::cache_dir() else {
            return Start::Not("there is no cache directory".into());
        };
        let adapter = match adapter::choose_kept(command, cwd, stored) {
            Ok(adapter) => adapter,
            Err(why) => return Start::Not(why),
        };
        // \[C-32\] Whether the stored word chose pytest where CTest or Swift would take COMMAND
        // now: then a run in which pytest reports nothing clears it.
        let kept = adapter.word() == pytest::WORD
            && adapter::choose(command, cwd).is_ok_and(|fresh| fresh.word() != pytest::WORD);
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
        // \[C-32\] COMMAND's own `env NAME=value` assignments: in its environment as an adapter
        // reads it, and in every tool an adapter starts for it.
        let assigned = command::assignments(command);
        let env = |name: &str| {
            assigned
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, value)| OsString::from(value))
                .or_else(|| std::env::var_os(name))
        };
        let at = StartAt {
            cache: &cache,
            cwd,
            dir: &dir,
            env: &env,
            assigned: &assigned,
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
            plan_notes: Vec::new(),
            run_dir: dir.clone(),
            cwd: cwd.to_path_buf(),
            whole: false,
            kept,
            assigned,
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
            assigned: &self.assigned,
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
        for (unit, lists) in &keyed {
            if !whole_reusable(&map, unit) {
                continue;
            }
            let lists_keys: Vec<Option<Digest>> = lists.iter().map(|(key, _)| *key).collect();
            if let Choice::Passed(at, key) = shared::choose(&lists_keys, &lookup.found)
                && let Some(found) = lookup.found.get(&key)
            {
                passed.insert(unit.clone(), found.clone());
                keys.insert(unit.clone(), key);
                if let Some((_, list)) = lists.get(at) {
                    chosen.insert(unit.clone(), list.clone());
                }
            }
        }
        // \[C-34\] An audit of test files with `unit: file`; with `unit: test`, of tests, below.
        let file_audited = units::audited(
            passed.keys().map(|unit| (unit.as_str(), &keys[unit])),
            &started,
        );
        let mut audited = match self.unit {
            Unit::File => file_audited.clone(),
            Unit::Test => BTreeSet::new(),
        };
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
        let check = &input.base.check;
        // \[C-39\] The second tier: the tests of each test file whose own key didn't pass.
        let mut notes = Vec::new();
        let tested = match self.unit {
            Unit::Test => {
                let fetched = asker
                    .as_ref()
                    .map(|asker| asker.fetched.borrow().clone())
                    .unwrap_or_default();
                match plan_tests(&TestsAt {
                    keyer: &keyer,
                    reads: self.reads,
                    tree: input.tree,
                    keyed: &keyed,
                    map: &map,
                    shared: &fetched,
                    passed: &passed,
                    keys: &keys,
                    store: input.store,
                    counts: input.counts,
                    started: &started,
                }) {
                    Ok(tested) => {
                        // \[C-34\] A test file one of whose tests the audit runs isn't reused
                        // whole: its other tests are left out one by one. One whose tests this
                        // machine doesn't know (told without them, a hint without them) can't
                        // be: those are audited whole, as test files are.
                        let partly = tested.audited_whole();
                        audited = untold_audited(&passed, &keys, &tested, &started);
                        reuse.retain(|unit| !partly.contains(unit) && !audited.contains(unit));
                        tested
                    }
                    Err(TooMany(n)) => {
                        // \[C-39\] Past the limit, this run reuses by test file, and says so,
                        // with the test files' audit.
                        self.unit = Unit::File;
                        reuse.retain(|unit| !file_audited.contains(unit));
                        audited = file_audited.clone();
                        notes.push(format!(
                            "{check}: its tests are reused by test file in this run: it has {} tests, more than the {} this version reuses one by one",
                            usage::thousands(n as u64),
                            usage::thousands(MOST_TESTS as u64)
                        ));
                        PlannedTests::default()
                    }
                }
            }
            Unit::File => PlannedTests::default(),
        };
        if let Err(err) = write_run_file(
            &self.run_dir,
            &self.root,
            &Listed {
                reuse: reuse.iter().cloned().collect(),
                extras,
                binaries,
                unit: self.unit,
                reads: self.reads,
                reuse_tests: tested.listed(),
            },
        ) {
            return Planned::Everything(format!("can't write the plan ({err})"));
        }
        // The line before COMMAND (RFC 0003 §1.2, RFC 0004 §1.2), in the words people read.
        let reused = reuse.len();
        let tests_line = (self.unit == Unit::Test)
            .then(|| {
                tests_line(
                    check,
                    &known,
                    &reuse,
                    &audited,
                    &tested,
                    self.adapter.test_file(),
                )
            })
            .flatten();
        let line = if let Some(line) = tests_line {
            line
        } else if reused == 0 && audited.is_empty() {
            let all = format!(
                "{check}: all {} run",
                test_files(total as u64, self.adapter.test_file())
            );
            let rest = rest_of(&sets, input.tree);
            let sources: BTreeSet<String> = sets.files(Set::Sources).map(str::to_string).collect();
            let why = why_every(
                &map,
                input.tree,
                &rest,
                &sources,
                known.iter().map(|(unit, _)| unit),
                self.adapter.test_file(),
                None,
            );
            format!("{all}: {why}")
        } else if reused == 0 {
            format!(
                "{check}: all {} run; {}",
                test_files(total as u64, self.adapter.test_file()),
                checked_only(audited.len())
            )
        } else {
            let mut line = format!(
                "{check}: {} of {} run; {reused} already passed on the files {} read",
                total - reused,
                test_files(total as u64, self.adapter.test_file()),
                if reused == 1 { "it" } else { "they" },
            );
            line.push_str(&checked_too(audited.len()));
            line
        };
        self.plan = Some(Plan {
            keys,
            passed,
            reuse,
            chosen,
            received,
            tests: tested,
        });
        self.plan_notes = notes;
        Planned::Plan(line)
    }

    /// \[C-39\] What the plan says once, before its line, each a line after `hushrun: `.
    pub fn plan_notes(&self) -> &[String] {
        &self.plan_notes
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
        Some(Asker::new(store, check_id, systems, self.adapter.word()))
    }

    /// What the adapter saw, decided: each unit's state, what the plan would have reused, and the
    /// lines that say so. `None` when the adapter wrote nothing, so that everything ran and no
    /// unit is recorded.
    pub fn finish(&self, end: &Finish<'_>) -> Option<Observed> {
        let files = facts::read_all(&self.run_dir)?;
        // \[C-32\] What a facts file says the runner left out that the plan didn't list, and
        // whether it ran on a stale build.
        let unlisted = facts::left_out(&files);
        let stale = facts::stale_build(&files);
        // \[C-32\] A start of the runner that began and never said it ended: what it ran can't
        // be told, so no unit and no test of the run records a pass; a fail still records.
        let unfinished = facts::unfinished_starts(&self.run_dir);
        let at = StoreAt {
            path: db::path(&self.cache, end.repository),
            check_id: store::check_id(&end.base, end.repository),
            base: end.base.clone(),
            adapter: self.adapter.word(),
            hints: self.adapter.hints(),
        };
        // \[C-32\] Kept pytest's by the store's word, COMMAND passed, and pytest reported nothing:
        // counted, in the write that follows the run's record, and the second such run in a row
        // forgets the word, so the next run chooses afresh. One silent run proves nothing (a ctest
        // whose build directory is empty finds no test and passes), nor does a run that failed.
        if self.kept && end.command_passed && !files.iter().any(|f| f.adapter == pytest::WORD) {
            let line = if db::silent_runs(&at.path, &at.check_id) >= 1 {
                "pytest reported none, so the next run chooses how to observe them afresh"
            } else {
                "pytest reported none this time"
            };
            return Some(Observed {
                store: Some(at),
                silent: true,
                lines: vec![format!(
                    "{}: test files weren't observed: {line}",
                    end.base.check
                )],
                ..Observed::default()
            });
        }
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
            // \[C-32\] Left out what the plan didn't list: whatever else could be read, this start
            // decides nothing.
            Ok(None) => {
                return (unlisted.is_some() || stale.is_some()).then(|| Observed {
                    left_out: unlisted,
                    stale_build: stale,
                    ..Observed::default()
                });
            }
            Err(why) => {
                return Observed::unread(&end.base.check, &why).map(|o| Observed {
                    left_out: unlisted,
                    stale_build: stale,
                    ..o
                });
            }
        };
        let left_out = read.left_out.clone().or(unlisted);
        let stale = read.stale_build.clone().or(stale);
        // \[C-32\] Left out what nothing listed, in a start whose plan listed nothing (`observe`,
        // or a plan that reused nothing): another start would be the same, and could fail one that
        // passed. No unit is recorded, said once, and COMMAND's own exit stands.
        let listed = self
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.reuse.is_empty() || !plan.tests.listed().is_empty());
        if let Some(why) = &left_out
            && !listed
            && !self.whole
        {
            return Observed::unread(
                &end.base.check,
                &format!("its adapter left out what nothing listed ({why})"),
            );
        }
        let watches = facts::watches(&files);
        // \[C-40\] Why the adapter watched no test, when it says.
        let unwatched = files.iter().find_map(|f| {
            f.settings
                .get("watch")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
        let fetched = asker
            .as_ref()
            .map(|asker| asker.fetched.borrow().clone())
            .unwrap_or_default();
        let mode = Modes {
            noun: self.adapter.test_file(),
            unit: self.unit,
            reads: self.reads,
            watches: &watches,
            unwatched,
            shared: &fetched,
            words: RunWords {
                started: unfinished.clone(),
                failed: read
                    .not_kept
                    .clone()
                    .or_else(|| facts::run_not_kept(&files)),
            },
        };
        // \[C-32\] What holds for the whole run, in each unit's facts, for the one function: a
        // start that didn't say what it saw, a stale build, and what the start with nothing listed
        // left out all the same.
        let mut read = read;
        for seen in read.seen.values_mut() {
            seen.facts.unfinished_start = unfinished.is_some();
            seen.facts.stale_build = stale.is_some();
            seen.facts.left_out_again = left_out.is_some() && self.whole;
        }
        let mut observed = decide(end, &self.tests, &mode, read, &map, at, how);
        if let Some(why) = &unfinished {
            observed.lines.push(format!(
                "{}: no test file or test records a pass: {why}",
                end.base.check
            ));
        }
        observed.left_out = left_out;
        observed.stale_build = stale;
        // \[C-32\] What an adapter's facts say people should know of this run, each said once.
        let notes: BTreeSet<&str> = files
            .iter()
            .filter_map(|f| f.settings.get("note").and_then(|v| v.as_str()))
            .collect();
        for note in notes {
            observed.lines.push(format!("{}: {note}", end.base.check));
        }
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
            assigned: &self.assigned,
        })
    }

    /// \[C-32\] Sets up COMMAND's start once more, with nothing left out, after a start whose
    /// adapter left out what the plan didn't list (RFC 0003 principle 5: every failure of the
    /// machinery ends in "ran everything"): a directory of its own whose `hushrun.json` lists
    /// nothing, the variables pointing at it, and no plan, so that the run goes by that start
    /// alone. `Err` why it can't be set up.
    pub fn whole_again(&mut self) -> Result<(), String> {
        let dir = self.dir.join("whole");
        std::fs::create_dir_all(&dir)
            .map_err(|err| format!("can't make {}: {err}", dir.display()))?;
        let unit_files = BTreeSet::new();
        let listed = Listed {
            extras: self.adapter.again(&self.root, &self.cwd, &unit_files),
            unit: self.unit,
            reads: self.reads,
            ..Listed::default()
        };
        write_run_file(&dir, &self.root, &listed)
            .map_err(|err| format!("can't write its plan: {err}"))?;
        for (name, value) in &mut self.variables {
            if name == runner::UNITS_VAR {
                *value = dir.clone().into_os_string();
            }
        }
        self.run_dir = dir;
        self.plan = None;
        self.whole = true;
        Ok(())
    }

    /// \[C-34, ADR 0069\] The second run of the audit (RFC 0003 §3.7): COMMAND as written, again,
    /// with a directory of its own whose `hushrun.json` lists every test file the first run saw
    /// but `again`, so that only those run. `None` when it can't be set up, and then each of them
    /// counts as having failed again.
    pub fn again(&self, observed: &Observed, again: &BTreeSet<String>) -> Option<Again> {
        let dir = self.run_dir.join("again");
        std::fs::create_dir_all(&dir).ok()?;
        // \[C-39\] An audited test runs alone: every other test of its file is listed one by one,
        // and its file isn't listed whole.
        let mut reuse_tests: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in again {
            if let Some((unit, id)) = observed.again_tests.get(name) {
                let others = observed
                    .tests
                    .get(unit)
                    .into_iter()
                    .flat_map(BTreeMap::keys)
                    .filter(|other| *other != id && !again.contains(&units::test_name(unit, other)))
                    .cloned();
                reuse_tests.entry(unit.clone()).or_default().extend(others);
            }
        }
        let others: Vec<String> = observed
            .units
            .keys()
            .filter(|unit| !again.contains(*unit) && !reuse_tests.contains_key(*unit))
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
            reuse_tests,
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
    /// \[C-39\] And each test of it, by name, from its own facts.
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
            .map(|name| {
                let state = read
                    .as_ref()
                    .and_then(|read| {
                        let run = RunFacts {
                            command_passed: passed,
                            failures_by_unit: read.failures_by_unit,
                            planned: false,
                        };
                        if let Some((unit, id)) = observed.again_tests.get(name) {
                            let seen = read.seen.get(unit)?;
                            let decided = tests_decided(seen.tests.as_ref()?, &seen.facts, |_| run);
                            return decided.get(id).map(|(decision, _)| decision.state);
                        }
                        let seen = read.seen.get(name)?;
                        Some(units::state(&seen.facts, &run))
                    })
                    .unwrap_or(State::NotRun);
                (name.clone(), state)
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
        /// \[C-39\] With tests as the unit, what the plan would reuse counted with them.
        tests: Option<TestsCounted>,
        /// \[C-39\] The adapter's word for its test files ([`Adapter::test_file`]).
        noun: &'static str,
        /// \[C-34\] Why every one runs, when none would be reused, as a run's line says it.
        why: Option<String>,
    },
    /// The check's reuse is suspended: why, as a run says it.
    Suspended(String),
}

/// \[C-22\] The plan a run of `command` would make (C-34), for `hushrun ready`, from what is on
/// this machine and one batch lookup: `None` when it can't tell without starting a tool or a
/// build (Cargo, Go), when no adapter would load, or when the lookup fails. It writes nothing,
/// and sends nothing but the lookup.
pub fn preview(p: &Preview<'_>) -> Option<Previewed> {
    let cache = crate::context::cache_dir()?;
    let check_id = store::check_id(&p.base, p.repository);
    let stored = db::adapter_of(&db::path(&cache, p.repository), &check_id);
    let adapter = adapter::choose_kept(p.command, p.cwd, stored.as_deref()).ok()?;
    let loaded = db::load(&db::path(&cache, p.repository), &check_id);
    if loaded.unread.is_some() {
        return None;
    }
    let map = loaded.map;
    let systems = shared::systems(&p.base);
    let asker =
        (!systems.is_empty()).then(|| Asker::new(p.store, check_id, systems, adapter.word()));
    let found = adapter
        .plan_from_files(&PlanAt {
            tree: p.tree,
            root: p.root,
            cwd: p.cwd,
            map: &map,
            asker: asker.as_ref(),
            ignore: &p.tests.ignore,
            compile_env: &[],
            assigned: &command::assignments(p.command),
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
    let (unit, reads, _) = what_it_reuses(p.tests, adapter.as_ref());
    let mut passed: BTreeMap<String, Found> = BTreeMap::new();
    let mut keys: BTreeMap<String, Digest> = BTreeMap::new();
    for (test_file, lists) in &keyed {
        let lists_keys: Vec<Option<Digest>> = lists.iter().map(|(key, _)| *key).collect();
        if whole_reusable(&map, test_file)
            && let Choice::Passed(_, key) = shared::choose(&lists_keys, &lookup.found)
            && let Some(found) = lookup.found.get(&key)
        {
            passed.insert(test_file.clone(), found.clone());
            keys.insert(test_file.clone(), key);
        }
    }
    let audited = units::audit_share(passed.len());
    let reused = passed.len() - audited;
    // \[C-39, C-22\] With tests as the unit, the second tier as a run's plan makes it, counted
    // as its line counts it. Which test files and tests an audit runs depends on the run's start:
    // this one's is now.
    let tests = (unit == Unit::Test)
        .then(|| {
            let started = Time::from_timestamp(jiff::Timestamp::now()).to_string();
            let fetched = asker
                .as_ref()
                .map(|asker| asker.fetched.borrow().clone())
                .unwrap_or_default();
            let tested = plan_tests(&TestsAt {
                keyer: &keyer,
                reads,
                tree: p.tree,
                keyed: &keyed,
                map: &map,
                shared: &fetched,
                passed: &passed,
                keys: &keys,
                store: p.store,
                counts: p.counts,
                started: &started,
            })
            .ok()?;
            // \[C-34\] As a run's plan: a test file one of whose tests is audited isn't reused
            // whole.
            let partly = tested.audited_whole();
            let untold = untold_audited(&passed, &keys, &tested, &started);
            let reuse: BTreeSet<String> = passed
                .keys()
                .filter(|unit| !partly.contains(*unit) && !untold.contains(*unit))
                .cloned()
                .collect();
            Some(tests_counted(
                found.known.iter().map(|(unit, _)| unit),
                &reuse,
                &tested,
            ))
        })
        .flatten();
    let why = (passed.is_empty()).then(|| {
        let sources: BTreeSet<String> =
            found.sets.files(Set::Sources).map(str::to_string).collect();
        why_every(
            &map,
            p.tree,
            &rest_of(&found.sets, p.tree),
            &sources,
            found.known.iter().map(|(unit, _)| unit),
            adapter.test_file(),
            None,
        )
    });
    Some(Previewed::Plan {
        total,
        run: total - reused,
        reused,
        audited,
        tests,
        noun: adapter.test_file(),
        why,
    })
}

/// \[C-22\] `hushrun ready`'s line for a check whose plan `previewed` says, after `label`: what CI
/// would run, or with results here (`here`) what a run here would, counted as a run's line counts
/// (C-39), each adapter's units in its own word.
pub fn previewed_line(previewed: &Previewed, label: &str, here: bool) -> String {
    let who = if here { "a run here would" } else { "CI would" };
    let passed = if here { "passed here" } else { "passed" };
    let n = |n: usize| usage::thousands(n as u64);
    match previewed {
        Previewed::Suspended(why) => format!("{label}: every test file runs: {why}"),
        Previewed::Plan {
            total,
            tests: Some(counted),
            noun,
            ..
        } if counted.one_by_one > 0 || counted.tests.is_some_and(|(_, r)| r > 0) => {
            // \[C-39\] With tests as the unit: tests, where every test file's are known here.
            match counted.tests {
                Some((all, reused_tests)) => {
                    let they = if reused_tests == 1 { "it" } else { "they" };
                    format!(
                        "{label}: {who} run {} of {}, in {} of {}; {} already {passed} on the files {they} read",
                        n(all - reused_tests),
                        count_tests(all as u64),
                        n(counted.files_run),
                        test_files_grouped(counted.files_total as u64, noun),
                        n(reused_tests),
                    )
                }
                None => {
                    let they = if counted.files_reused == 1 {
                        "it"
                    } else {
                        "they"
                    };
                    format!(
                        "{label}: {who} run {} of {}; {} already {passed} on the files {they} read, and so did {} of the others",
                        n(counted.files_run),
                        test_files_grouped(*total as u64, noun),
                        n(counted.files_reused),
                        count_tests(counted.one_by_one as u64),
                    )
                }
            }
        }
        Previewed::Plan {
            total,
            run,
            reused,
            noun,
            why,
            ..
        } => {
            if *reused == 0 {
                let all = format!(
                    "{label}: {who} run all {}",
                    test_files_grouped(*total as u64, noun)
                );
                match why {
                    Some(why) => format!("{all}: {why}"),
                    None => all,
                }
            } else {
                let they = if *reused == 1 { "it" } else { "they" };
                format!(
                    "{label}: {who} run {} of {}; {} already {passed} on the files {they} read",
                    n(*run),
                    test_files_grouped(*total as u64, noun),
                    n(*reused),
                )
            }
        }
    }
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
    /// Test files, as before: \[ADR 0077\] one is reused when none of its tests ran.
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
    /// \[ADR 0077\] In a run with `tests.unit: test` whose adapter told its tests, the same three
    /// numbers for tests (C-10, C-39).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestCounts>,
}

/// \[ADR 0077\] `units.tests`: a run's tests, how many it had, ran and reused (C-10).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TestCounts {
    pub total: u64,
    pub ran: u64,
    pub reused: u64,
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
    /// \[C-39\] The adapter's word for its test files (`test program`), for the run's last line;
    /// empty for a run whose units weren't read.
    pub noun: &'static str,
    /// Each unit's state, and the key it records under, when it records.
    pub units: BTreeMap<String, (State, Option<Digest>)>,
    /// The units the plan would have reused.
    pub would_reuse: BTreeSet<String>,
    /// \[C-39\] The tests the plan would have reused one by one, by name.
    pub would_reuse_tests: BTreeSet<String>,
    /// \[C-39\] Each unit's tests and their states, when its adapter told them.
    pub tests: BTreeMap<String, BTreeMap<String, State>>,
    /// \[C-39\] Of `again`, the tests, each by name with its unit and id.
    again_tests: BTreeMap<String, (String, String)>,
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
    /// \[C-32\] The adapter left out what the plan didn't list, and why: this start decides
    /// nothing, and COMMAND starts once more with nothing left out.
    pub left_out: Option<String>,
    /// \[C-32\] The runner ran on a stale build, and why: a run that passed records nothing.
    pub stale_build: Option<String>,
    /// \[C-32\] pytest's adapter was kept by the store's word and reported nothing in a run that
    /// passed: counted, and the second such run in a row forgets the check, word and map.
    silent: bool,
    /// \[C-39\] Whether tests were decided one by one in this run, and \[C-40\] what each was
    /// taken to have read: what its hint holds of them (C-41).
    by_test: bool,
    reads: Reads,
    /// \[C-41\] The test files that ran whole but for tests reused one by one: their hint is
    /// this machine's to send, as a test file that ran is.
    partly: BTreeSet<String>,
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
    pub fn keep(&self, run_key: &Digest, tree: &Tree, now: Time, duration_ms: u64) {
        let Some(at) = &self.store else {
            return;
        };
        if self.silent {
            if let Err(err) = db::note_silent(&at.path, &at.check_id) {
                tracing::debug!("the check's silent run wasn't noted: {}", err.why());
            }
            return;
        }
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
        // \[C-37\] Every unit the run's adapter told, whatever became of it.
        let known: BTreeSet<String> = self.units.keys().cloned().collect();
        // \[C-33\] A run that reused nothing took what reuse can save at most.
        let reused_nothing = self.counts.as_ref().is_none_or(|counts| {
            counts.reused == 0 && counts.tests.as_ref().is_none_or(|tests| tests.reused == 0)
        });
        let write = db::Write {
            check_id: at.check_id,
            base: &at.base,
            adapter: at.adapter,
            seen: &self.mapped,
            tree,
            rest: &self.rest,
            sources: &self.sources,
            unit_files: (!self.unit_files.is_empty()).then_some(&self.unit_files),
            known: &known,
            whole_ms: reused_nothing.then_some(duration_ms),
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
    /// under this machine's system: every test file it executed and fully observed, \[C-41\] or ran
    /// but for tests reused one by one, and every one it reused, with the list whose key hit; and
    /// their tests (`hint_tests`). After [`Observed::keep`], for a run recorded on a
    /// server that keeps test files' results. One `PUT`, and none when this machine already sent
    /// or received a hint with its content id for the address and system. A `PUT` that fails
    /// leaves no note, so the next run of the check sends it again; it is said for debug only, as
    /// the store's errors are. `Some` line to say once when the hint is too large to send, or
    /// \[C-41\] went without tests' lists or tests to fit.
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
            .filter(|(unit, (state, _))| {
                state.recorded().is_some() || *state == State::Reused || self.partly.contains(*unit)
            })
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
            Hints::Kinds(_) => map.unit_files.clone(),
            Hints::None | Hints::Sources => BTreeSet::new(),
        };
        let tests = self.hint_tests(&map, &units);
        let hint = hushrun_core::hints::Hint {
            tests,
            adapter: at.adapter.to_string(),
            units,
            unit_files,
        };
        let check = &at.base.check;
        let (written, said) = match hushrun_core::hints::write_fitting(&hint, &place.key) {
            Ok((written, hushrun_core::hints::Trimmed::Nothing)) => (written, None),
            Ok((written, hushrun_core::hints::Trimmed::Lists)) => (
                written,
                Some(format!(
                    "{check}: what each of its tests read wasn't shared with other machines: it is too large, so they take each test to have read what its test file read"
                )),
            ),
            Ok((written, hushrun_core::hints::Trimmed::Tests)) => (
                written,
                Some(format!(
                    "{check}: its tests weren't shared with other machines: there are too many, so they reuse them by test file"
                )),
            ),
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
            return said;
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
        said
    }

    /// \[C-41, ADR 0079\] The tests this run's hint holds of the test files it sends (`units`),
    /// from this machine's store (`map`), each that has a key: one it saw pass or fail, never one
    /// not followed or not run. With `reads: test`, each with its own list, or `None` when it read
    /// its test file's. With `reads: file`, ids alone, and only of test files that didn't pass
    /// whole: one that did records no test's key. None when tests weren't decided one by one.
    fn hint_tests(
        &self,
        map: &store::Map,
        units: &BTreeMap<String, hushrun_core::hints::Unit>,
    ) -> BTreeMap<String, BTreeMap<String, Option<hushrun_core::hints::Unit>>> {
        if !self.by_test {
            return BTreeMap::new();
        }
        units
            .keys()
            .filter_map(|unit| {
                let mapped = map.units.get(unit)?;
                let whole = self
                    .units
                    .get(unit)
                    .is_some_and(|(state, _)| matches!(state, State::Passed | State::Reused));
                if self.reads == Reads::File && whole {
                    return None;
                }
                let tests: BTreeMap<String, Option<hushrun_core::hints::Unit>> = mapped
                    .tests
                    .iter()
                    .filter(|(_, row)| matches!(row.state, State::Passed | State::Failed))
                    .map(|(id, row)| {
                        let list = match self.reads {
                            Reads::Test => {
                                row.list.as_ref().map(|list| hushrun_core::hints::Unit {
                                    reads: list.reads.keys().cloned().collect(),
                                    dirs: list.dirs.clone(),
                                })
                            }
                            Reads::File => None,
                        };
                        (id.clone(), list)
                    })
                    .collect();
                (!tests.is_empty()).then(|| (unit.clone(), tests))
            })
            .collect()
    }
}

/// One test file, or several, for people, in the adapter's word (`noun`, C-39): `1 test program`,
/// `22 test programs`, `3 test classes`.
fn test_files(n: u64, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {}", plural(noun)),
    }
}

/// \[C-39\] An adapter's word for its test files, made plural: `test binaries`, `test classes`,
/// `packages`.
pub fn plural(noun: &str) -> String {
    if let Some(stem) = noun.strip_suffix('y') {
        format!("{stem}ies")
    } else if noun.ends_with('s') {
        format!("{noun}es")
    } else {
        format!("{noun}s")
    }
}

/// \[C-39\] Whether a test file may be reused whole, beside what its key says: not when one of its
/// tests wasn't followed the last time it ran here, since its own pass may stand on a run before
/// that; with tests as the unit its other tests may still be reused one by one. For either unit:
/// a check switched from `unit: test` back to `unit: file` keeps the guard its test's row holds.
/// The plan, what `observe` says, `hushrun ready` and `hushrun why` all ask it.
fn whole_reusable(map: &store::Map, test_file: &str) -> bool {
    !map.units
        .get(test_file)
        .is_some_and(|m| m.tests.values().any(|t| t.state == State::NotObserved))
}

/// \[C-39\] One test file, or `2,140 test files`, for the lines that count tests, in the adapter's
/// word (`noun`).
fn test_files_grouped(n: u64, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{} {}", usage::thousands(n), plural(noun)),
    }
}

/// \[C-33, C-39\] What reuse saved, from the recorded durations of what it reused (`saved_ms`):
/// never more than `whole_ms`, the longest this check took here when it reused nothing, since a
/// runner with workers counts each test's own time and a person waits for the whole. `None`
/// when no such run is kept here: then the count is said without a time.
fn saving(saved_ms: u64, whole_ms: Option<u64>) -> Option<u64> {
    whole_ms.map(|whole| saved_ms.min(whole))
}

/// \[C-34\] The audit's clause where a line counts test files with the tests reused one by one
/// beside them: its `files` (run whole, their tests unknown here) and its `tests`, each in its
/// word: `, and 1 more test that already passed runs anyway, as a check`, `, and 1 more test file
/// and 2 more tests that already passed run anyway, as a check`.
fn checked_apart(files: usize, tests: usize, noun: &str) -> String {
    let files_said = match files {
        1 => format!("1 more {noun}"),
        n => format!("{} more {}", usage::thousands(n as u64), plural(noun)),
    };
    let tests_said = match tests {
        1 => "1 more test".to_string(),
        n => format!("{} more tests", usage::thousands(n as u64)),
    };
    let what = match (files, tests) {
        (0, 0) => return String::new(),
        (0, _) => tests_said,
        (_, 0) => files_said,
        _ => format!("{files_said} and {tests_said}"),
    };
    let verb = if files + tests == 1 { "runs" } else { "run" };
    format!(", and {what} that already passed {verb} anyway, as a check")
}

/// \[C-34\] The clause a plan's line ends with when an audit runs `n` of what already passed,
/// beside those reused: `, and 1 more that already passed runs anyway, as a check`; nothing for 0.
fn checked_too(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => ", and 1 more that already passed runs anyway, as a check".into(),
        n => format!(
            ", and {} more that already passed run anyway, as a check",
            usage::thousands(n as u64)
        ),
    }
}

/// \[C-34\] What a plan's line says after `all <N> run; ` when nothing is reused and an audit
/// runs `n` that already passed: `1 that already passed on the files it read runs anyway, as a
/// check`.
fn checked_only(n: usize) -> String {
    if n == 1 {
        "1 that already passed on the files it read runs anyway, as a check".into()
    } else {
        format!(
            "{} that already passed on the files they read run anyway, as a check",
            usage::thousands(n as u64)
        )
    }
}

/// \[C-39\] One test, or `2,140 tests`.
fn count_tests(n: u64) -> String {
    match n {
        1 => "1 test".into(),
        n => format!("{} tests", usage::thousands(n)),
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

    /// \[C-39\] A test's key, its id `test` in `unit`, from the files it read (`reads: file`: its
    /// test file's).
    fn test_key(&self, unit: &str, test: &str, reads: &BTreeSet<String>) -> Option<Digest> {
        let keyed = self.sets.keyed(reads.iter().map(String::as_str));
        units::test_key(
            self.base,
            unit,
            test,
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

/// \[C-39, C-40\] What a run reuses one by one, what a test is taken to have read, and what the
/// adapter's files say of what tests read beyond each test.
struct Modes<'a> {
    unit: Unit,
    reads: Reads,
    watches: &'a facts::Watches,
    /// Why the adapter watched no test, as it said.
    unwatched: Option<String>,
    /// \[C-41\] The hints the adapter read, with the tests they hold: what `observe` would have
    /// keyed them by.
    shared: &'a Shared,
    /// \[C-37\] What the run's adapters said of why no pass of it is kept.
    words: RunWords,
    /// \[C-39\] The adapter's word for its test files, for the run's lines.
    noun: &'static str,
}

fn decide(
    end: &Finish<'_>,
    tests: &Tests,
    mode: &Modes<'_>,
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
        left_out: _,
        stale_build: _,
        not_kept: _,
    } = read;
    let mut seen = seen;
    // \[C-39, C-10\] A test file reused whole whose adapter told none of its tests (it left the
    // file out before any test was named): its tests as this machine's store, else a hint, knows
    // them, each left out, so that `units.tests` counts them as reused. Known nowhere, no count
    // of tests is sent, rather than one that leaves them out.
    let mut tests_unknown = false;
    if mode.unit == Unit::Test {
        for (unit, s) in &mut seen {
            if !s.facts.deselected || s.tests.as_ref().is_some_and(|t| !t.is_empty()) {
                continue;
            }
            let known: BTreeSet<String> = match map.units.get(unit).filter(|m| !m.tests.is_empty())
            {
                Some(mapped) => mapped.tests.keys().cloned().collect(),
                None => mode
                    .shared
                    .test_ids(unit)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            };
            if known.is_empty() {
                tests_unknown |= matches!(&how, How::Plan(plan) if plan.reuse.contains(unit));
                continue;
            }
            let left_out = facts::MergedTest {
                deselected: true,
                ..facts::MergedTest::default()
            };
            s.tests = Some(known.into_iter().map(|id| (id, left_out.clone())).collect());
        }
    }
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
        noun: mode.noun,
        store: Some(at),
        reads: mode.reads,
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
    let check = &end.base.check;
    // \[C-39\] Tests one by one: with `unit: test`, for the test files whose adapter told their
    // tests, up to the limit; past it, this run reuses by test file, and `observe` says so.
    let reported: usize = seen
        .values()
        .filter_map(|s| s.tests.as_ref())
        .map(BTreeMap::len)
        .sum();
    let unit_mode = mode.unit;
    // Decided once: a run whose plan listed tests to leave out counts by test whatever it
    // reported, since its adapter left them out and its test files didn't run whole; else past
    // the limit, by test file (RFC 0004 principle 2: reuse never chains).
    let listed_any = matches!(&how, How::Plan(plan) if !plan.tests.listed().is_empty());
    let by_test = listed_any || (unit_mode == Unit::Test && reported <= MOST_TESTS);
    out.by_test = by_test;
    if by_test
        && mode.reads == Reads::Test
        && reported > 0
        && let Some(why) = &mode.unwatched
    {
        out.lines.push(format!(
            "{check}: each test is taken to have read what its test file read: {why}"
        ));
    }
    if unit_mode == Unit::Test && !by_test && matches!(how, How::Observe) {
        out.lines.push(format!(
            "{check}: its tests are reused by test file in this run: it has {} tests, more than the {} this version reuses one by one",
            usage::thousands(reported as u64),
            usage::thousands(MOST_TESTS as u64)
        ));
    }
    // What the plan would have done, in `observe`: from what was recorded before this run, a unit
    // the plan would have keyed (by the lists \[C-38\] gives, or for Cargo this build's) that the
    // one rule would reuse, unless `tests.always` names it. One batch, in pages. \[C-39\] Then the
    // tests of each test file it wouldn't have reused whole, by the same rule.
    let mut would: BTreeMap<String, (Digest, Found)> = BTreeMap::new();
    let mut would_tests: BTreeMap<String, TestPassed> = BTreeMap::new();
    if matches!(how, How::Observe) {
        let known: Known = planned
            .iter()
            .filter(|(unit, _)| seen.contains_key(*unit))
            .map(|(unit, lists)| (unit.clone(), Some(lists.clone())))
            .collect();
        let keyed = keyed(&keyer, &known, tests);
        if let Ok(lookup) = shared::lookup(end.store, all_keys(&keyed), end.counts) {
            for (unit, lists) in &keyed {
                if !whole_reusable(map, unit) {
                    continue;
                }
                let keys: Vec<Option<Digest>> = lists.iter().map(|(key, _)| *key).collect();
                if let Choice::Passed(_, key) = shared::choose(&keys, &lookup.found)
                    && let Some(found) = lookup.found.get(&key)
                {
                    would.insert(unit.clone(), (key, found.clone()));
                }
            }
        }
        if by_test {
            let asked = keyed
                .iter()
                .filter(|(unit, _)| !would.contains_key(*unit))
                .filter_map(|(unit, lists)| Some((unit, lists, seen.get(unit)?.tests.as_ref()?)))
                .flat_map(|(unit, lists, told)| {
                    let file_lists: Vec<List> =
                        lists.iter().map(|(_, list)| list.clone()).collect();
                    let before = map.units.get(unit);
                    told.keys()
                        .map(|id| {
                            let row = before.and_then(|m| m.tests.get(id));
                            let seen = mode.shared.test_seen(unit, id);
                            let lists =
                                shared::test_lists(mode.reads, &file_lists, row, &seen, tree);
                            (unit.clone(), id.clone(), lists)
                        })
                        .collect::<Vec<_>>()
                });
            would_tests =
                tests_that_passed(&keyer, asked, end.store, end.counts).unwrap_or_default();
        }
    }
    let mut counts = Counts::default();
    let mut file_keys: BTreeSet<Digest> = BTreeSet::new();
    let mut test_counts = TestCounts::default();
    let mut told_tests = false;
    let mut saved_ms = 0u64;
    let mut not_observed: BTreeMap<String, u64> = BTreeMap::new();
    // \[C-39\] The tests that always run as not followed: every test of a test file with what
    // wasn't followed outside every test's window, and each test with something inside its own.
    let mut tests_not_followed: usize = 0;
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
        let run = RunFacts {
            command_passed: end.command_passed,
            failures_by_unit,
            planned,
        };
        // \[C-39\] Each test's state, by the one function, from its own facts and what wasn't
        // followed outside every test's window of its file.
        let listed_tests = |id: &str| match &how {
            How::Plan(plan) => plan.tests.lists(unit, id),
            _ => false,
        };
        let decisions: Option<BTreeMap<String, (Decision, Facts)>> = by_test
            .then_some(seen.tests.as_ref())
            .flatten()
            .map(|told| {
                tests_decided(told, &facts, |id| RunFacts {
                    planned: planned || listed_tests(id),
                    ..run
                })
            });
        let test_states: Option<BTreeMap<String, (State, Facts)>> = decisions.as_ref().map(|d| {
            d.iter()
                .map(|(id, (decision, facts))| (id.clone(), (decision.state, facts.clone())))
                .collect()
        });
        // The test file's own state: decided as before from its facts, with what wasn't followed
        // inside any of its tests, and not whole when some of its tests were reused one by one.
        // What wasn't followed inside a test is its test file's too, whether or not its tests
        // are decided one by one in this run: past the limit, they go by test file, and a test
        // that started a process still keeps its file from a pass of its own (C-34).
        let mut whole = facts.clone();
        for test in seen.tests.iter().flat_map(BTreeMap::values) {
            whole.not_followed.extend(test.not_followed.iter().cloned());
            // \[C-32\] A test file one of whose tests was skipped: in a run that failed, no pass.
            whole.skipped |= test.skipped && !test.deselected;
        }
        if let Some(states) = &test_states {
            told_tests = true;
            whole.tests_reused = states
                .iter()
                .any(|(id, (state, _))| *state == State::Reused && listed_tests(id));
        }
        let decision = units::decided(&whole, &run);
        let state = decision.state;
        let tests_ran = test_states
            .as_ref()
            .is_some_and(|states| states.values().any(|(s, _)| s.ran()));
        let ran = state.ran() || tests_ran;
        let reused_whole = state == State::Reused
            || (!ran
                && test_states
                    .as_ref()
                    .is_some_and(|states| states.values().any(|(s, _)| *s == State::Reused)));
        if ran {
            counts.ran += 1;
        }
        if whole.tests_reused && ran && whole.complete {
            out.partly.insert(unit.clone());
        }
        if reused_whole {
            counts.reused += 1;
        }
        if state == State::NotObserved {
            for why in whole.not_followed.iter().map(|n| n.why.clone()) {
                *not_observed.entry(why).or_default() += 1;
            }
        }
        if let Some(states) = &test_states {
            tests_not_followed += if state == State::NotObserved && !facts.not_followed.is_empty() {
                states.len()
            } else {
                states
                    .values()
                    .filter(|(test, _)| *test == State::NotObserved)
                    .count()
            };
        }
        if let Some(states) = &test_states {
            for (test, _) in states.values() {
                test_counts.total += 1;
                test_counts.ran += u64::from(test.ran());
                test_counts.reused += u64::from(*test == State::Reused);
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
                for (id, (test, _)) in test_states.iter().flatten() {
                    let name = units::test_name(unit, id);
                    if let Some(passed) = would_tests.get(&name) {
                        saved_ms = saved_ms.saturating_add(passed.found.duration_ms);
                        out.would_reuse_tests.insert(name.clone());
                        if *test == State::Failed {
                            out.disagreements.push((name, passed.key));
                        }
                    }
                }
            }
            How::Plan(plan) => {
                if state == State::Reused {
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
                // \[C-39\] The same for a test, by its name.
                for (id, (test, _)) in test_states.iter().flatten() {
                    let name = units::test_name(unit, id);
                    // \[C-34\] A test left out of a test file the audit split: what it took when
                    // its file last told it, from the store's row.
                    if *test == State::Reused
                        && plan.tests.whole.contains_key(&name)
                        && let Some(row) = map.units.get(unit).and_then(|m| m.tests.get(id))
                    {
                        saved_ms = saved_ms.saturating_add(row.duration_ms);
                    }
                    let Some(passed) = plan.tests.passed.get(&name) else {
                        continue;
                    };
                    if *test == State::Reused {
                        saved_ms = saved_ms.saturating_add(passed.found.duration_ms);
                    }
                    if *test == State::Failed {
                        out.again.insert(name.clone(), passed.key);
                        out.again_tests.insert(name, (unit.clone(), id.clone()));
                    }
                }
            }
            How::Quiet => {}
        }
        // \[C-39\] A test file some of whose tests were reused read, for this run's keys, what it
        // read now and what it read when it last ran here: its reused tests didn't run, and what
        // only they opened is still theirs. A larger list only makes keys finer.
        let mut reads = seen.reads.clone();
        if whole.tests_reused
            && let Some(before) = map.units.get(unit)
        {
            reads.extend(before.reads.keys().cloned());
        }
        let key = decision.recorded().and_then(|_| keyer.key(unit, &reads));
        if let Some(key) = key {
            file_keys.insert(key);
        }
        if let (Some(passed), Some(key)) = (decision.recorded(), key) {
            out.records.push(UnitRecord {
                key,
                result: if passed { "pass" } else { "fail" }.into(),
                duration_ms: seen.duration_ms,
            });
        }
        // \[C-40\] Each test's list, by the one function: its test file's with `reads: file`.
        let no_dirs = BTreeSet::new();
        let unit_watch = mode.watches.units.get(unit);
        let file_watch = units::FileWatch {
            unit,
            file: &reads,
            file_dirs: dirs.get(unit).unwrap_or(&no_dirs),
            imports: &mode.watches.imports,
            reexports: &mode.watches.reexports,
            opens: &mode.watches.opens,
            loaded: unit_watch
                .and_then(|w| w.loaded.as_ref())
                .map(|(ran, opened)| (ran, opened)),
            every: unit_watch.map_or(&no_dirs, |w| &w.every),
            shared: unit_watch.map_or(&[], |w| w.shared.as_slice()),
        };
        let test_list = |id: &str| {
            let told = seen.tests.as_ref().and_then(|t| t.get(id));
            units::test_list(
                mode.reads,
                &file_watch,
                &units::TestWatch {
                    id,
                    ran: told.and_then(|t| t.ran.as_ref()),
                    opened: told.and_then(|t| t.opened.as_ref()),
                    dirs: told.and_then(|t| t.dirs.as_ref()),
                },
            )
        };
        // What a test's key holds: its own list, with every file now below a directory it
        // listed, or its test file's.
        let test_files = |list: &units::TestList| {
            if list.own {
                let mut files = list.files.clone();
                files.extend(gotest::under(tree, &list.dirs));
                files
            } else {
                reads.clone()
            }
        };
        // \[C-39\] Two tiers: a test file that passed whole records its own pass, and nothing more
        // with `reads: file`, \[C-40\] and each test's with `reads: test`; one that didn't run
        // whole, or didn't pass whole, records each test that ran under the test's own key.
        if state != State::Passed || mode.reads == Reads::Test {
            for (id, (test, _)) in decisions.iter().flatten() {
                // \[C-39\] One its adapter never leaves out records no pass of its own: no plan
                // finds one to list it by, and it runs, as it would. A fail still counts.
                let Some(passed) = test.recorded() else {
                    continue;
                };
                let Some(key) = keyer.test_key(unit, id, &test_files(&test_list(id))) else {
                    continue;
                };
                let duration_ms = seen
                    .tests
                    .as_ref()
                    .and_then(|told| told.get(id))
                    .map_or(0, |t| t.duration_ms);
                out.records.push(UnitRecord {
                    key,
                    result: if passed { "pass" } else { "fail" }.into(),
                    duration_ms,
                });
            }
        }
        // \[C-37\] A test file the run decided `not run` keeps its row too, with why, as a run that
        // recorded nothing else would: `hushrun why` says what the run decided, not that it never
        // ran here. One that didn't run whole (left out by something else, unfinished, tests
        // reused one by one) read less than all it reads, so a row it had stays as it was.
        let kept_row = state == State::NotRun
            && match decision.not_kept {
                Some(
                    units::NotKept::LeftOut
                    | units::NotKept::Unfinished
                    | units::NotKept::TestsReused,
                ) => !map.units.contains_key(unit),
                _ => true,
            };
        if ran || kept_row {
            // \[C-39\] Its tests as they ran, and a test reused as the row it stands on.
            let before = map.units.get(unit);
            let tests_rows = decisions
                .iter()
                .flatten()
                .filter_map(|(id, (decision, _))| {
                    let test = &decision.state;
                    if *test == State::Reused {
                        if let Some(row) = before.and_then(|b| b.tests.get(id)) {
                            return Some((id.clone(), row.clone()));
                        }
                        // \[C-41\] Reused on what another machine saw: the row it stands on is
                        // that machine's pass, with the list whose key hit when it was its own.
                        let How::Plan(plan) = &how else { return None };
                        let passed = plan.tests.passed.get(&units::test_name(unit, id))?;
                        let list = passed.list.own.then(|| store::TestListRow {
                            reads: entries(sets, tree, &passed.list.list.reads),
                            dirs: passed.list.list.dirs.clone(),
                        });
                        return Some((
                            id.clone(),
                            store::TestRow {
                                state: State::Passed,
                                duration_ms: passed.found.duration_ms,
                                not_followed: BTreeSet::new(),
                                not_kept: None,
                                list,
                            },
                        ));
                    }
                    let told = seen.tests.as_ref()?.get(id)?;
                    // \[C-40\] With `reads: test`, its own list, which the next plan keys it by.
                    let list = test_list(id);
                    let list = list.own.then(|| store::TestListRow {
                        reads: entries(sets, tree, &list.files),
                        dirs: list.dirs.clone(),
                    });
                    Some((
                        id.clone(),
                        store::TestRow {
                            state: *test,
                            duration_ms: told.duration_ms,
                            not_followed: told.not_followed.clone(),
                            // \[C-37\] Why it kept nothing, as the one function decided it.
                            not_kept: not_kept(
                                *decision,
                                true,
                                told.not_kept.as_deref(),
                                &mode.words,
                            ),
                            list,
                        },
                    ))
                })
                .collect();
            let file_why = file_not_kept(decision, &tests_rows, &mode.words);
            out.mapped.insert(
                unit.clone(),
                Mapped {
                    reads: sets
                        .keyed(reads.iter().map(String::as_str))
                        .into_iter()
                        .filter_map(|path| {
                            let (kind, hash) = tree.entry(path)?;
                            Some((path.to_string(), format!("{kind} {hash}")))
                        })
                        .collect(),
                    // What wasn't followed outside every test's window: what stops the test file,
                    // and each of its tests, from being keyed at all (C-34). With its tests not
                    // kept one by one, what wasn't followed inside them too: no row then says it.
                    not_followed: if test_states.is_some() {
                        facts.not_followed.clone()
                    } else {
                        whole.not_followed.clone()
                    },
                    dirs: dirs.get(unit).cloned().unwrap_or_default(),
                    origin: Origin::SeenHere,
                    tests: tests_rows,
                    // \[C-37\] Why it kept no result of its own, as the one function decided it.
                    not_kept: file_why,
                },
            );
        }
        if let Some(states) = test_states {
            out.tests.insert(
                unit.clone(),
                states.into_iter().map(|(id, (s, _))| (id, s)).collect(),
            );
        }
        out.units.insert(unit.clone(), (state, key));
    }
    out.rest = rest_of(sets, tree);
    out.sources = sources;
    if told_tests && !tests_unknown {
        counts.tests = Some(test_counts.clone());
    }
    if matches!(how, How::Plan(_)) && (counts.reused > 0 || test_counts.reused > 0) {
        counts.saved_ms = saving(saved_ms, map.whole_ms);
    }
    // \[RFC 0004 §3.8\] In the order whatever a server cuts is a pass of: failures, then test
    // files' results, then tests' passes. The backend sends what its server takes.
    out.records = order_records(std::mem::take(&mut out.records), &file_keys);

    // What `observe` says (RFC 0003 §1.2, RFC 0004 §1.2), in the words people read: test files,
    // tests, the files they read.
    let reused = out.would_reuse.len() as u64;
    if counts.total > 0 && matches!(how, How::Observe) {
        let would_run = counts.total - reused;
        let saving = match saving(saved_ms, map.whole_ms) {
            Some(ms) if ms >= 1000 => format!(", saving about {}", crate::cmd::wrap::took(ms)),
            _ => String::new(),
        };
        // \[C-39\] Tests that would have been reused: those of the test files reused whole, and
        // those reused one by one.
        let tests_reused: u64 = out
            .tests
            .iter()
            .map(|(unit, states)| {
                if out.would_reuse.contains(unit) {
                    states.len() as u64
                } else {
                    states
                        .keys()
                        .filter(|id| out.would_reuse_tests.contains(&units::test_name(unit, id)))
                        .count() as u64
                }
            })
            .sum();
        let mut line = if told_tests && tests_reused > 0 {
            let files_run = counts.total
                - out
                    .tests
                    .iter()
                    .filter(|(unit, states)| {
                        out.would_reuse.contains(*unit)
                            || (!states.is_empty()
                                && states.keys().all(|id| {
                                    out.would_reuse_tests.contains(&units::test_name(unit, id))
                                }))
                    })
                    .count() as u64;
            format!(
                "{check}: with `tests: reuse`, {} of {} would have run, in {} of {}; {} already passed on the files {}{saving}",
                usage::thousands(test_counts.total - tests_reused),
                count_tests(test_counts.total),
                usage::thousands(files_run),
                test_files_grouped(counts.total, mode.noun),
                usage::thousands(tests_reused),
                if tests_reused == 1 {
                    "it read"
                } else {
                    "they read"
                },
            )
        } else if reused == 0 {
            let all = format!(
                "{check}: with `tests: reuse`, all {} would have run",
                test_files(counts.total, mode.noun)
            );
            let why = why_every(
                map,
                tree,
                &out.rest,
                &out.sources,
                out.units.keys(),
                mode.noun,
                None,
            );
            format!("{all}: {why}")
        } else {
            format!(
                "{check}: with `tests: reuse`, {would_run} of {} would have run; {reused} already passed on the files {}{saving}",
                test_files(counts.total, mode.noun),
                if reused == 1 { "it read" } else { "they read" },
            )
        };
        if !not_observed.is_empty() {
            // \[C-39\] Counted as the line counts: tests, where it counts tests: every test of a test
            // file not followed, whatever else kept each from a pass, and each test not followed in
            // the others.
            let n = if told_tests && tests_reused > 0 {
                tests_not_followed
            } else {
                out.units
                    .values()
                    .filter(|(s, _)| *s == State::NotObserved)
                    .count()
            };
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

/// \[RFC 0004 §3.8\] `records` in the order a server that takes fewer keeps the first of: every
/// failure, then the passes of test files (`file_keys`), then the passes of tests. Cutting the end
/// only ever loses a pass, which costs its next run; a lost failure could let an older pass reuse.
fn order_records(records: Vec<UnitRecord>, file_keys: &BTreeSet<Digest>) -> Vec<UnitRecord> {
    let (fails, passes): (Vec<UnitRecord>, Vec<UnitRecord>) =
        records.into_iter().partition(|r| r.result != "pass");
    let (files, tests): (Vec<UnitRecord>, Vec<UnitRecord>) =
        passes.into_iter().partition(|r| file_keys.contains(&r.key));
    fails.into_iter().chain(files).chain(tests).collect()
}

/// Of `files`, those a key holds (units and sources), each with its kind and content hash now, as
/// a manifest writes them: what the store keeps of a list.
fn entries(sets: &units::Sets, tree: &Tree, files: &BTreeSet<String>) -> BTreeMap<String, String> {
    sets.keyed(files.iter().map(String::as_str))
        .into_iter()
        .filter_map(|path| {
            let (kind, hash) = tree.entry(path)?;
            Some((path.to_string(), format!("{kind} {hash}")))
        })
        .collect()
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
        tests: BTreeMap::new(),
        not_kept: None,
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
/// \[C-33, C-34\] Why every one of `units` runs, or would, when none is reused: the one reason
/// the plan's line, `observe`'s and `hushrun ready` give, from what decides the plan, in this
/// order of truth. A file of the rest set changed, or a source is new or gone ([`why_all`]); a file
/// every one of them read the last time changed or is gone; none was followed the last time (and
/// why, from their rows); none kept a pass the last time (and why, from their rows); else none had
/// passed here yet on the files it read. `noun` is the adapter's word for them. `said` is what an
/// adapter said of the whole of the last run, such as a runner that executed nothing (tasks up
/// to date): it comes after what changed and before what the rows say. Units an adapter never
/// leaves out have no key, so a plan counts them as running already.
#[allow(clippy::too_many_arguments)]
fn why_every<'a>(
    map: &store::Map,
    tree: &Tree,
    rest: &BTreeMap<String, String>,
    sources: &BTreeSet<String>,
    units: impl Iterator<Item = &'a String> + Clone,
    noun: &str,
    said: Option<&str>,
) -> String {
    if let Some(why) = why_all(map, rest, sources) {
        return why;
    }
    let changed = why_every_read(map, tree, units.clone(), noun);
    if let Some(why) = changed {
        return why;
    }
    if let Some(said) = said.filter(|s| !s.is_empty()) {
        return said.to_string();
    }
    let none_yet = "none had passed here yet on the files it read".to_string();
    let rows: Option<Vec<&Mapped>> = units.clone().map(|unit| map.units.get(unit)).collect();
    let Some(rows) = rows.filter(|rows| !rows.is_empty()) else {
        return none_yet;
    };
    // None of them followed the last time: what wasn't, as their rows say.
    if rows.iter().all(|row| !row.not_followed.is_empty()) {
        let whys: BTreeSet<String> = rows
            .iter()
            .flat_map(|row| row.not_followed.iter().map(|n| describe(&n.why)))
            .collect();
        return format!(
            "hushrun couldn't follow all that any of them read the last time ({}), so they always run",
            whys.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    // None of them kept a pass the last time: why, as their rows say.
    if rows.iter().all(|row| row.not_kept.is_some()) {
        let why = rows
            .iter()
            .find_map(|row| row.not_kept.as_deref())
            .unwrap_or_default();
        return format!("none kept a pass the last time they ran here: {why}");
    }
    none_yet
}

/// \[C-34\] A file every one of `units` read the last time, as it was then, now changed or gone.
fn why_every_read<'a>(
    map: &store::Map,
    tree: &Tree,
    units: impl Iterator<Item = &'a String>,
    noun: &str,
) -> Option<String> {
    let rows: Vec<&Mapped> = units
        .map(|unit| map.units.get(unit))
        .collect::<Option<_>>()?;
    let (first, others) = rows.split_first()?;
    for (path, then) in &first.reads {
        if !others.iter().all(|row| row.reads.get(path) == Some(then)) {
            continue;
        }
        match tree.entry(path) {
            None => return Some(format!("{path} is gone, and every {noun} read it")),
            Some((kind, hash)) if format!("{kind} {hash}") != *then => {
                return Some(format!("{path} changed, and every {noun} reads it"));
            }
            Some(_) => {}
        }
    }
    None
}

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

/// \[C-36\] Makes this machine's store for `repository` from the one an older hushrun keeps beside
/// it, when there is none of this schema ([`db::upgrade`]): the line a run says once in CI
/// (`in_ci`), where a cache step that names the older file restores one no run updates.
pub fn upgrade_store(
    check: &hushrun_core::CheckName,
    repository: Option<&str>,
    in_ci: bool,
) -> Option<String> {
    let cache = crate::context::cache_dir()?;
    let old = db::upgrade(&db::path(&cache, repository))?;
    in_ci.then(|| upgraded(check, &old))
}

/// \[C-36\] What a run in CI says once when it made this machine's store from `old`.
fn upgraded(check: &hushrun_core::CheckName, old: &str) -> String {
    format!(
        "{check}: what its test files read was taken from {old}, which an older hushrun keeps: a CI cache step that names `units/*.v1.sqlite` restores a file no run updates, so name `units/*.sqlite` in it instead"
    )
}

/// \[C-32\] The adapter that last reported the test files of the check `base` names here, for
/// the repository the run names, as this machine's store says: what [`Session::start`] keeps a
/// COMMAND pytest reported pytest's by.
pub fn stored_adapter(base: &KeyBase, repository: Option<&str>) -> Option<String> {
    let cache = crate::context::cache_dir()?;
    db::adapter_of(
        &db::path(&cache, repository),
        &store::check_id(base, repository),
    )
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
