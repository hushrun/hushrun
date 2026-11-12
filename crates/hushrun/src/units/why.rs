//! \[ADR 0073\] `hushrun why NAME UNIT` (C-37): what a run of the check would do with one test
//! file, for the files on disk now, and why, from what this machine knows.
//!
//! It explains what the plan decides (C-34) and decides nothing: the reasons that don't depend on
//! any recorded result (not a test file, `tests` off, an adapter not offered, `tests.always`, never
//! seen run here, not observed last time) come from `hushrun.yaml`, COMMAND and this machine's store
//! (ADR 0072) alone; whether it would be reused, whether its newest result failed, and a
//! suspension come from one read-only batch lookup, as `hushrun ready`'s (C-22), which sends
//! nothing waiting in the outbox. When the lookup can't answer, or the test files are known only
//! from the last build or run here (Cargo, Go, Vitest), it says the answer is as far as this
//! machine knows. \[C-38\] With results on a server, it reads the hints a run would, and keys the
//! test file by the same lists ([`super::shared::lists`]); when the list it would be reused on is
//! another machine's, it says so. It writes nothing (ADR 0061).
//!
//! \[C-39, C-41\] With tests as the unit, `UNIT::TEST` asks of one test: its test file's reasons
//! first, then the second tier's, keyed by the lists \[C-40\] [`super::shared::test_lists`] gives,
//! hints' included. A test file one of whose tests wasn't followed the last time it ran here isn't
//! reused whole, and says so.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use hushrun_core::units::{self, KeyBase, Mode, Reads, Set, State, Tests, Tree, Unit};

use super::adapter::{self, Adapter, Hints, WhyAt};
use super::db::{self, EdgeKind};
use super::shared::{self, Asker, Choice, Shared};
use super::store::{self, Map, UnitResults};
use super::{Keyer, fetch, rest_of, what_it_reuses, whole_reusable};
use crate::results::RunResult;

/// What `hushrun why` needs.
pub struct WhyInput<'a> {
    /// The check's key fields as a run of it here has them.
    pub base: KeyBase,
    /// The repository the run names (C-10).
    pub repository: Option<&'a str>,
    /// The check's `tests`, `None` when they are off.
    pub tests: Option<&'a Tests>,
    /// The test file asked about, from the repository root; for Go, a package's directory or one
    /// of its test files.
    pub unit: &'a str,
    /// The scope's files on disk now.
    pub tree: &'a Tree,
    pub root: &'a Path,
    pub cwd: &'a Path,
    /// This machine's store.
    pub store: &'a Path,
    /// Where results are found, read-only; `Err` why there is nowhere to look.
    pub results: Result<&'a dyn UnitResults, String>,
    /// Whether a run's result counts here (C-10).
    pub counts: &'a dyn Fn(&RunResult) -> bool,
}

/// What a run would do with the test file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Runs,
    Reused,
    /// It isn't a test file of the check.
    NotATestFile,
    /// The check's test files aren't looked at: they run with it.
    Off,
}

impl Decision {
    pub fn word(self) -> &'static str {
        match self {
            Decision::Runs => "runs",
            Decision::Reused => "reused",
            Decision::NotATestFile => "not-a-test-file",
            Decision::Off => "off",
        }
    }
}

/// One hop of a path between files: `file`, and how it reaches the next, if there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub file: String,
    pub then: Option<EdgeKind>,
}

/// The explanation.
#[derive(Debug, Clone)]
pub struct Explanation {
    /// The test file, as the check names it (for Go, the package's directory).
    pub unit: String,
    /// \[C-39\] The test asked about, by its id, when tests are the unit.
    pub test: Option<String>,
    pub decision: Decision,
    /// A word for the reason, the JSON's.
    pub reason: &'static str,
    /// The first line.
    pub says: String,
    /// Lines after it, indented in text.
    pub detail: Vec<String>,
    /// The answer comes from what this machine knows, which may not be all.
    pub here_only: bool,
    /// The file that changed, or is new or gone, when that is the reason.
    pub file: Option<String>,
    /// A path of direct edges from the test file to that file, when the adapter saw one.
    pub path: Vec<Hop>,
}

/// Explains what a run of the check would do with `input.unit`.
pub fn explain(input: &WhyInput<'_>) -> Explanation {
    let check = &input.base.check;
    let asked = input.unit.trim_start_matches("./").trim_end_matches('/');
    let mut out = Explanation {
        unit: asked.to_string(),
        test: None,
        decision: Decision::Runs,
        reason: "",
        says: String::new(),
        detail: Vec::new(),
        here_only: false,
        file: None,
        path: Vec::new(),
    };
    let Some(tests) = input.tests.filter(|t| t.is_on()) else {
        out.decision = Decision::Off;
        out.reason = "tests-off";
        out.says = format!(
            "{check}'s `tests` is off in hushrun.yaml: its test files run with it, whole, whenever it runs"
        );
        return out;
    };
    let stored = db::adapter_of(input.store, &store::check_id(&input.base, input.repository));
    let adapter = match adapter::choose_kept(&input.base.command, input.cwd, stored.as_deref()) {
        Ok(adapter) => adapter,
        Err(why) => {
            out.decision = Decision::Off;
            out.reason = "not-offered";
            out.says = format!("{check}'s test files aren't observed: {why}");
            return out;
        }
    };
    let check_id = store::check_id(&input.base, input.repository);
    let loaded = db::load(input.store, &check_id);
    if let Some(unread) = &loaded.unread {
        out.detail
            .push(format!("every test file runs: {}", unread.why()));
    }
    let map = loaded.map;
    // \[C-39\] `UNIT::TEST`: a test, when tests are the unit; else its test file, said so. A
    // name without `::` is a test file's, whatever its runner calls its items.
    let (unit_mode, reads_mode, notes) = what_it_reuses(tests, adapter.as_ref());
    let (asked, test) = match asked.split_once("::") {
        Some((file, id)) if !id.is_empty() => (file, Some(id.to_string())),
        _ => (asked, None),
    };
    let test = match test {
        Some(_) if unit_mode == Unit::File => {
            let why = notes
                .first()
                .cloned()
                .unwrap_or_else(|| "`tests.unit` is `file` in hushrun.yaml".to_string());
            out.detail
                .push(format!("its tests are reused with their test file: {why}"));
            None
        }
        test => test,
    };
    // For Go a unit is a package's directory, which one of its test files names too.
    let unit = adapter.why_unit(asked);
    out.unit = unit.clone();
    out.test = test.clone();
    let named = match &test {
        Some(id) => units::test_name(&unit, id),
        None => unit.clone(),
    };
    let would = tests.mode != Mode::Reuse;
    let verb = |decision: Decision| match (decision, would) {
        (Decision::Reused, false) => "would be reused",
        (Decision::Reused, true) => "would be reused with `tests: reuse`",
        (_, false) => "runs",
        (_, true) => "would run with `tests: reuse`",
    };
    let says = |out: &mut Explanation, decision: Decision, reason: &'static str, why: String| {
        out.decision = decision;
        out.reason = reason;
        out.says = match decision {
            Decision::Runs | Decision::Reused => format!("{named} {}: {why}", verb(decision)),
            Decision::NotATestFile | Decision::Off => why,
        };
    };

    // \[C-38\] The hints a run would read: with results on a server, for any adapter but Cargo's.
    let systems = shared::systems(&input.base);
    let asker = match &input.results {
        _ if adapter.hints() == Hints::None => None,
        Ok(results) if !systems.is_empty() => Some(Asker::new(
            *results,
            check_id,
            systems.clone(),
            adapter.word(),
        )),
        _ => None,
    };
    let kinds_shared = match adapter.hints() {
        Hints::Kinds(_) => fetch(
            asker.as_ref(),
            adapter.hints(),
            &units::Sets::default(),
            input.tree,
            &tests.ignore,
        ),
        Hints::None | Hints::Sources => Shared::default(),
    };
    let listed = if map.unit_files.is_empty() {
        kinds_shared.unit_files()
    } else {
        map.unit_files.clone()
    };
    // The four sets, by the adapter's rule (C-30): from files now, or from the last run here, as
    // far as this machine knows, or from a hint.
    let no_hints = Shared::default();
    let sets = adapter.why_sets(&WhyAt {
        tree: input.tree,
        root: input.root,
        cwd: input.cwd,
        map: &map,
        ignore: &tests.ignore,
        shared: &no_hints,
        listed: &listed,
    });
    let mut shared = match adapter.hints() {
        Hints::Kinds(_) => kinds_shared,
        Hints::None => Shared::default(),
        Hints::Sources if !adapter.why_asks_hints(&map) => Shared::default(),
        Hints::Sources => fetch(
            asker.as_ref(),
            adapter.hints(),
            &sets,
            input.tree,
            &tests.ignore,
        ),
    };
    // A `shared` entry here counts only when its system is one a run reads (C-38).
    shared.systems = systems;
    if let Some(why) = adapter.why_known_from() {
        out.here_only = true;
        out.detail
            .push(format!("as far as this machine knows: {why}"));
    }
    let at = WhyAt {
        tree: input.tree,
        root: input.root,
        cwd: input.cwd,
        map: &map,
        ignore: &tests.ignore,
        shared: &shared,
        listed: &listed,
    };

    // Is it a test file of the check at all?
    if !adapter.why_is_unit(&at, &sets, &unit) {
        let why = match sets.of(&unit) {
            None if !adapter.why_units_are_directories() => {
                format!("{unit} isn't a file of {check}'s scope as it is now")
            }
            Some(Set::Ignored) => {
                format!("{unit} is in {check}'s `tests.ignore`: no test reads it")
            }
            _ => adapter.why_not_a_unit(&at, check.as_str(), &unit),
        };
        says(&mut out, Decision::NotATestFile, "not-a-test-file", why);
        return out;
    }
    if tests.always_runs(&unit) {
        says(
            &mut out,
            Decision::Runs,
            "always",
            "`tests.always` in hushrun.yaml names it".into(),
        );
        return out;
    }
    let own = map.units.get(&unit);
    if let Some(mapped) = own
        && !mapped.not_followed.is_empty()
    {
        let what: Vec<String> = mapped
            .not_followed
            .iter()
            .map(|n| match n.detail.as_str() {
                "" => super::describe(&n.why),
                detail => format!("{} ({detail})", super::describe(&n.why)),
            })
            .collect();
        says(
            &mut out,
            Decision::Runs,
            "not-observed",
            format!(
                "hushrun couldn't follow all it read the last time it ran here: {}",
                what.join(", ")
            ),
        );
        out.detail
            .insert(0, "so it runs every time, and records nothing".into());
        return out;
    }
    // \[C-38\] The lists a run would key it by: this machine's entry, then other machines'.
    let lists = shared::lists(&unit, own, &shared, input.tree, None).unwrap_or_default();
    if lists.is_empty() {
        says(
            &mut out,
            Decision::Runs,
            "never-ran-here",
            "this machine has never seen it run, so it doesn't know what it reads".into(),
        );
        return out;
    }
    let reads = match own {
        Some(_) => lists[0].files.clone(),
        None => BTreeSet::new(),
    };
    let keyer = Keyer::new(input.tree, &sets, &input.base);
    // \[C-39\] A test file one of whose tests wasn't followed the last time it ran here isn't
    // reused whole, whatever the check's unit is now: its own pass may stand on a run before that.
    let unfollowed = own.and_then(|mapped| {
        mapped
            .tests
            .iter()
            .find(|(_, row)| row.state == State::NotObserved)
    });
    let keys: Vec<Option<hushrun_core::digest::Digest>> =
        lists.iter().map(|l| keyer.key(&unit, &l.files)).collect();
    let changes = changes(&map, &sets, input.tree, &unit, &reads);

    let looked = match (&input.results, keys.iter().any(Option::is_some)) {
        (Ok(results), true) => {
            shared::lookup(*results, keys.iter().flatten().copied(), input.counts)
        }
        (Err(why), _) => Err(why.clone()),
        (_, false) => Err("its key can't be computed".into()),
    };
    match looked {
        Ok(lookup) if !lookup.reuse.allows_reuse() => {
            says(
                &mut out,
                Decision::Runs,
                "suspended",
                super::suspended(check, &lookup.reuse),
            );
        }
        Ok(lookup) => match shared::choose(&keys, &lookup.found) {
            Choice::Passed(..) if test.is_some() && whole_reusable(&map, &unit) => {
                says(
                    &mut out,
                    Decision::Reused,
                    "reused",
                    "its test file passed whole on the files it read, and none of them has changed since".into(),
                );
                out.detail.insert(
                    0,
                    "an audit run may still run it: each run runs 2% of the test files that could be reused, at least one, chosen by their keys and the run's start".into(),
                );
            }
            Choice::Passed(..) if test.is_none() && unfollowed.is_some() => {
                let Some((id, row)) = unfollowed else {
                    return out;
                };
                says(
                    &mut out,
                    Decision::Runs,
                    "test-not-observed",
                    format!(
                        "hushrun couldn't follow all that its test {id} read the last time it ran here ({}), so it isn't reused whole",
                        not_followed(&row.not_followed)
                    ),
                );
                out.detail.insert(
                    0,
                    match unit_mode {
                        Unit::Test => {
                            "its other tests may still be reused one by one: `hushrun why` of one says"
                        }
                        Unit::File => {
                            "it runs whole until a run of it follows all that its tests read"
                        }
                    }
                    .into(),
                );
            }
            _ if test.is_some() => {
                let id = test.as_deref().unwrap_or_default();
                if let Some((other, _)) = unfollowed.filter(|(other, _)| other.as_str() != id) {
                    out.detail.push(format!(
                        "its test file isn't reused whole: hushrun couldn't follow all that its test {other} read the last time it ran here"
                    ));
                }
                let at = TestAt {
                    unit: &unit,
                    id,
                    reads: reads_mode,
                    file_lists: &lists,
                    map: &map,
                    shared: &shared,
                    sets: &sets,
                    keyer: &keyer,
                };
                explain_test(&mut out, &at, &says, input, adapter.as_ref());
            }
            Choice::Passed(at, _) if lists[at].seen_elsewhere() => {
                says(
                    &mut out,
                    Decision::Reused,
                    "reused",
                    "it passed on the files another machine saw it read, and none of them has changed since".into(),
                );
                let mut files: Vec<&str> = lists[at].reads.iter().map(String::as_str).collect();
                files.extend(lists[at].dirs.iter().map(String::as_str));
                out.detail.insert(
                    0,
                    format!("another machine saw it read {}", files.join(", ")),
                );
                out.detail.insert(
                    1,
                    "an audit run may still run it: each run runs 2% of the test files that could be reused, at least one, chosen by their keys and the run's start".into(),
                );
            }
            Choice::Passed(..) => {
                says(
                    &mut out,
                    Decision::Reused,
                    "reused",
                    "it passed on the files it read, and none of them has changed since".into(),
                );
                out.detail.insert(
                    0,
                    "an audit run may still run it: each run runs 2% of the test files that could be reused, at least one, chosen by their keys and the run's start".into(),
                );
            }
            Choice::Failed if lists.len() > 1 => says(
                &mut out,
                Decision::Runs,
                "failed",
                "machines saw it read different files, and its newest result on one of those lists failed".into(),
            ),
            Choice::Failed => says(
                &mut out,
                Decision::Runs,
                "failed",
                "its newest result on the files it reads now failed".into(),
            ),
            Choice::Nothing if own.is_none() => says(
                &mut out,
                Decision::Runs,
                "never-ran-here",
                "this machine has never seen it run, and no pass that counts here matches what another machine saw it read".into(),
            ),
            Choice::Nothing => {
                explain_change(&mut out, &changes, &says, &map, input, &unit, adapter.as_ref());
            }
        },
        Err(why) => {
            out.here_only = true;
            out.detail.push(format!(
                "as far as this machine knows: can't read which passed ({why})"
            ));
            explain_change(&mut out, &changes, &says, &map, input, &unit, adapter.as_ref());
        }
    }
    out
}

/// What was not followed, for people.
fn not_followed(what: &BTreeSet<units::NotFollowed>) -> String {
    what.iter()
        .map(|n| match n.detail.as_str() {
            "" => super::describe(&n.why),
            detail => format!("{} ({detail})", super::describe(&n.why)),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What one test is explained by.
struct TestAt<'a> {
    unit: &'a str,
    id: &'a str,
    reads: Reads,
    /// Its test file's lists, from [`shared::lists`].
    file_lists: &'a [shared::List],
    map: &'a Map,
    shared: &'a Shared,
    sets: &'a units::Sets,
    keyer: &'a Keyer<'a>,
}

/// \[C-39, C-40, C-41\] Why the second tier would run or reuse one test of a test file it doesn't
/// reuse whole: not followed last time, never seen, its keys' results, or what changed among the
/// files it read.
fn explain_test(
    out: &mut Explanation,
    at: &TestAt<'_>,
    says: &Says<'_>,
    input: &WhyInput<'_>,
    adapter: &dyn Adapter,
) {
    let check = &input.base.check;
    let row = at
        .map
        .units
        .get(at.unit)
        .and_then(|mapped| mapped.tests.get(at.id));
    if let Some(row) = row.filter(|row| row.state == State::NotObserved) {
        says(
            out,
            Decision::Runs,
            "not-observed",
            format!(
                "hushrun couldn't follow all it read the last time it ran here: {}",
                not_followed(&row.not_followed)
            ),
        );
        out.detail
            .insert(0, "so it runs every time, and records nothing".into());
        return;
    }
    let seen = at.shared.test_seen(at.unit, at.id);
    let lists = shared::test_lists(at.reads, at.file_lists, row, &seen, input.tree);
    if (row.is_none() && seen.is_empty()) || lists.is_empty() {
        says(
            out,
            Decision::Runs,
            "never-ran-here",
            "this machine has never seen it run, so it doesn't know what it reads".into(),
        );
        return;
    }
    let keys: Vec<Option<hushrun_core::digest::Digest>> = lists
        .iter()
        .map(|l| at.keyer.test_key(at.unit, at.id, &l.list.files))
        .collect();
    let looked = match (&input.results, keys.iter().any(Option::is_some)) {
        (Ok(results), true) => {
            shared::lookup(*results, keys.iter().flatten().copied(), input.counts)
        }
        (Err(why), _) => Err(why.clone()),
        (_, false) => Err("its key can't be computed".into()),
    };
    let audit = "an audit run may still run it: each run runs 2% of the tests that could be reused one by one, at least one, chosen by their keys and the run's start";
    let lookup = match looked {
        Ok(lookup) if !lookup.reuse.allows_reuse() => {
            says(
                out,
                Decision::Runs,
                "suspended",
                super::suspended(check, &lookup.reuse),
            );
            return;
        }
        Ok(lookup) => Some(lookup),
        Err(why) => {
            out.here_only = true;
            out.detail.push(format!(
                "as far as this machine knows: can't read which passed ({why})"
            ));
            None
        }
    };
    if let Some(lookup) = lookup {
        match shared::choose(&keys, &lookup.found) {
            Choice::Passed(i, _) if lists[i].list.seen_elsewhere() => {
                says(
                    out,
                    Decision::Reused,
                    "reused",
                    "it passed on the files another machine saw it read, and none of them has changed since".into(),
                );
                let mut files: Vec<&str> = lists[i].list.reads.iter().map(String::as_str).collect();
                files.extend(lists[i].list.dirs.iter().map(String::as_str));
                out.detail.insert(
                    0,
                    format!("another machine saw it read {}", files.join(", ")),
                );
                out.detail.insert(1, audit.into());
                return;
            }
            Choice::Passed(..) => {
                says(
                    out,
                    Decision::Reused,
                    "reused",
                    "it passed on the files it read, and none of them has changed since".into(),
                );
                out.detail.insert(0, audit.into());
                return;
            }
            Choice::Failed if lists.len() > 1 => {
                says(
                    out,
                    Decision::Runs,
                    "failed",
                    "machines saw it read different files, and its newest result on one of those lists failed".into(),
                );
                return;
            }
            Choice::Failed => {
                says(
                    out,
                    Decision::Runs,
                    "failed",
                    "its newest result on the files it reads now failed".into(),
                );
                return;
            }
            Choice::Nothing if row.is_none() => {
                says(
                    out,
                    Decision::Runs,
                    "never-ran-here",
                    "this machine has never seen it run, and no pass that counts here matches what another machine saw it read".into(),
                );
                return;
            }
            Choice::Nothing => {}
        }
    }
    // What changed among what it read: its own list, or its test file's.
    let changes = match row.and_then(|row| row.list.as_ref()) {
        Some(list) => {
            let files: BTreeSet<String> = list.reads.keys().cloned().collect();
            let mut files_now = files.clone();
            files_now.extend(super::gotest::under(input.tree, &list.dirs));
            changes_of(Some(&list.reads), at.map, at.sets, input.tree, &files_now)
        }
        None => {
            let reads = at
                .file_lists
                .first()
                .map(|l| l.files.clone())
                .unwrap_or_default();
            changes(at.map, at.sets, input.tree, at.unit, &reads)
        }
    };
    explain_change(out, &changes, says, at.map, input, at.unit, adapter);
}

/// What changed since the test file last ran here, among what its key holds.
#[derive(Debug, Default)]
struct Changes {
    /// A file it read whose bytes changed, is gone, or is new below a directory it listed.
    read: Option<(String, &'static str)>,
    /// A file of the rest set that changed, is new or gone, or a source new or gone: what runs
    /// every test file.
    every: Option<(String, String, &'static str)>,
}

fn changes(
    map: &Map,
    sets: &units::Sets,
    tree: &Tree,
    unit: &str,
    reads: &BTreeSet<String>,
) -> Changes {
    changes_of(
        map.units.get(unit).map(|mapped| &mapped.reads),
        map,
        sets,
        tree,
        reads,
    )
}

/// What changed among `entries`, what a test file or a test read when it last ran here, each with
/// its kind and content hash then, and `reads` now; and what changed for every test file.
fn changes_of(
    entries: Option<&BTreeMap<String, String>>,
    map: &Map,
    sets: &units::Sets,
    tree: &Tree,
    reads: &BTreeSet<String>,
) -> Changes {
    let mut out = Changes::default();
    if let Some(entries) = entries {
        for (path, entry) in entries {
            match tree.entry(path) {
                None => {
                    out.read = Some((path.clone(), "gone"));
                    break;
                }
                Some((kind, hash)) if format!("{kind} {hash}") != *entry => {
                    out.read = Some((path.clone(), "changed"));
                    break;
                }
                Some(_) => {}
            }
        }
        if out.read.is_none()
            && let Some(new) = reads
                .iter()
                .find(|p| sets.keyed([p.as_str()]).len() == 1 && !entries.contains_key(*p))
        {
            out.read = Some((new.clone(), "new"));
        }
    }
    let rest = rest_of(sets, tree);
    let sources: BTreeSet<String> = sets.files(Set::Sources).map(str::to_string).collect();
    out.every = rest
        .iter()
        .find(|(path, entry)| map.rest.get(*path).is_some_and(|before| before != *entry))
        .map(|(p, _)| {
            (
                p.clone(),
                format!("{p} changed, and any test may read it"),
                "rest-changed",
            )
        })
        .or_else(|| {
            rest.keys().find(|p| !map.rest.contains_key(*p)).map(|p| {
                (
                    p.clone(),
                    format!("{p} is new, and any test may read it"),
                    "rest-changed",
                )
            })
        })
        .or_else(|| {
            map.rest.keys().find(|p| !rest.contains_key(*p)).map(|p| {
                (
                    p.clone(),
                    format!("{p} is gone, and any test may have read it"),
                    "rest-changed",
                )
            })
        })
        .or_else(|| {
            sources.iter().find(|p| !map.sources.contains(*p)).map(|p| {
                (
                    p.clone(),
                    format!("{p} is new, which can change what tests load"),
                    "shape",
                )
            })
        })
        .or_else(|| {
            map.sources.iter().find(|p| !sources.contains(*p)).map(|p| {
                (
                    p.clone(),
                    format!("{p} is gone, which can change what tests load"),
                    "shape",
                )
            })
        });
    out
}

type Says<'a> = dyn Fn(&mut Explanation, Decision, &'static str, String) + 'a;

/// Why a test file with no result under its key runs: what changed, most particular first.
fn explain_change(
    out: &mut Explanation,
    changes: &Changes,
    says: &Says<'_>,
    map: &Map,
    input: &WhyInput<'_>,
    unit: &str,
    adapter: &dyn Adapter,
) {
    if let Some((file, how)) = &changes.read {
        let (reason, why) = match *how {
            "gone" => ("read-gone", format!("it read {file}, which is gone")),
            "new" => (
                "read-changed",
                format!("{file} is new in a directory it listed"),
            ),
            _ => ("read-changed", format!("it read {file}, which changed")),
        };
        says(out, Decision::Runs, reason, why);
        out.file = Some(file.clone());
        out.path = path_to(input, map, unit, file, adapter);
        if out.path.len() > 1 {
            let mut line = String::new();
            for hop in &out.path {
                line.push_str(&hop.file);
                if let Some(kind) = hop.then {
                    line.push(' ');
                    line.push_str(match kind {
                        EdgeKind::Import => "imports",
                        EdgeKind::Open => "opens",
                        EdgeKind::Read => "reads",
                    });
                    line.push(' ');
                }
            }
            out.detail.insert(0, line);
        } else if let Some(why) = adapter.why_no_path() {
            out.detail.insert(0, why.into());
        }
        if let Some((_, every, _)) = &changes.every {
            out.detail
                .push(format!("every test file runs in any case: {every}"));
        }
        return;
    }
    if let Some((file, every, reason)) = &changes.every {
        says(out, Decision::Runs, reason, every.clone());
        out.file = Some(file.clone());
        out.detail
            .insert(0, "so every test file of the check runs".into());
        return;
    }
    says(
        out,
        Decision::Runs,
        "no-pass",
        "no pass that counts here is recorded for the files it read, though none of them has changed since it last ran here".into(),
    );
    out.detail.insert(
        0,
        "its last result there didn't count here, or is gone: this machine forgets a result 30 days after its run".into(),
    );
}

/// The shortest path of direct edges the store holds from `unit` to `file` (for Go, to its
/// package's directory), through files `unit` read; empty when there is none.
fn path_to(
    input: &WhyInput<'_>,
    map: &Map,
    unit: &str,
    file: &str,
    adapter: &dyn Adapter,
) -> Vec<Hop> {
    let check_id = store::check_id(&input.base, input.repository);
    let Ok(edges) = db::edges(input.store, &check_id) else {
        return Vec::new();
    };
    let go = adapter.why_units_are_directories();
    let target = if go {
        super::gotest::package_of(file)
    } else {
        file.to_string()
    };
    let allowed: BTreeSet<&str> = map
        .units
        .get(unit)
        .map(|m| m.reads.keys().map(String::as_str).collect())
        .unwrap_or_default();
    // Imports and opens first, which say how: a test file reads every module its collection
    // loaded, which is true and says less.
    for kinds in [
        &[EdgeKind::Import, EdgeKind::Open][..],
        &[EdgeKind::Import, EdgeKind::Open, EdgeKind::Read],
    ] {
        let hops = shortest(&edges, kinds, &allowed, go, unit, &target);
        if !hops.is_empty() {
            return hops;
        }
    }
    Vec::new()
}

/// The shortest path from `unit` to `target` over `edges` of `kinds`, through `allowed` files
/// unless `any`.
fn shortest(
    edges: &BTreeSet<db::Edge>,
    kinds: &[EdgeKind],
    allowed: &BTreeSet<&str>,
    any: bool,
    unit: &str,
    target: &str,
) -> Vec<Hop> {
    let mut next: BTreeMap<&str, Vec<(&str, EdgeKind)>> = BTreeMap::new();
    for (from, to, kind) in edges {
        if kinds.contains(kind) && (any || allowed.contains(to.as_str())) {
            next.entry(from.as_str())
                .or_default()
                .push((to.as_str(), *kind));
        }
    }
    let mut came: BTreeMap<&str, (&str, EdgeKind)> = BTreeMap::new();
    let mut queue = VecDeque::from([unit]);
    let mut seen = BTreeSet::from([unit]);
    while let Some(at) = queue.pop_front() {
        if at == target {
            let mut hops = vec![Hop {
                file: at.to_string(),
                then: None,
            }];
            let mut here = at;
            while let Some((from, kind)) = came.get(here) {
                hops.push(Hop {
                    file: from.to_string(),
                    then: Some(*kind),
                });
                here = from;
            }
            hops.reverse();
            return hops;
        }
        for (to, kind) in next.get(at).into_iter().flatten() {
            if seen.insert(to) {
                came.insert(to, (at, *kind));
                queue.push_back(to);
            }
        }
    }
    Vec::new()
}
