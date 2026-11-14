//! \[C-38\] What other machines saw a check's test files read, and the lists of files a test
//! file's key might be made from.
//!
//! A machine with an empty cache, such as a CI runner or a sandbox, has no map of what each test
//! file read, so it can compute no key. A server keeps what machines saw, as hints (results-v0,
//! Hints; [`hushrun_core::hints`]): every path in one a keyed hash, under a key derived from the
//! names of the check's sources, which the server never has. Here:
//!
//! - [`Asker::fetch`]: the newest hints for the check's address, of the systems [`systems`] names,
//!   read against the scope's files. Asked only where they can change something: a run whose
//!   check's own key missed, with `tests` on, whose results go to a server, and whose adapter isn't
//!   Cargo's. A hint that can't be read is left out, and the others are used.
//! - [`lists`]: the one function that answers which lists of files a test file's key might be
//!   made from. The run's plan, `hushrun ready`, `observe`'s "would have run" and `hushrun why`
//!   all call it.
//! - [`choose`]: the one rule they decide by. A test file may be reused when some list's key has a
//!   newest result that counts here and is a pass, and no list's key has one that is a fail.
//!
//! A hint is never trusted: a list that is wrong, stale or hostile makes a key no run recorded,
//! and that test file runs (RFC 0003 §3.4).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use hushrun_core::digest::Digest;
use hushrun_core::glob::Pattern;
use hushrun_core::hints::{self, Key, Os};
use hushrun_core::units::{self, KeyBase, Set, Sets, Sources, Tree};

use super::adapter::Hints;
use super::db::{HintHow, HintKnown};
use super::store::{Found, HintPlace, Lookup, Mapped, Origin, UnitResults};
use super::{MOST_UNITS, gotest};
use crate::results::RunResult;

/// The most lists a test file's key is computed from.
pub const MOST_LISTS: usize = 3;

/// \[C-38\] Whose hints a machine reads: its own system's (the plan's S3), when there is no rule
/// or the check's `counts_from` names this system among others; only when the rule names
/// systems and this machine's isn't one of them, those systems', one lookup each, since only
/// they leave results for the check (ADR 0045). Never one pool of several. None on a system that
/// shares none. A machine always writes under its own.
pub fn systems(base: &KeyBase) -> Vec<Os> {
    let Some(this) = Os::this() else {
        return Vec::new();
    };
    let named: Vec<Os> = base
        .counts_from
        .as_ref()
        .and_then(|rule| rule.get("os"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|word| word.as_str().and_then(Os::from_word))
        .collect();
    if named.is_empty() || named.contains(&this) {
        vec![this]
    } else {
        named
    }
}

/// \[C-38\] The sources a check's hints are addressed and keyed by: its sources set, the files
/// `shape` hashes (C-31). For Vitest, whose list of test files a hint holds, every file of a
/// source kind, with no test file named: a new test file then gives another address, and
/// everything runs once, as Vitest's adapter does here (ADR 0071). The plan and the run that
/// writes a hint both ask it.
pub fn sources(hints: Hints, sets: &Sets, tree: &Tree, ignore: &[Pattern]) -> Vec<String> {
    match hints {
        Hints::Kinds(kinds) => {
            units::classify(tree.paths(), &|_| false, Sources::Kinds(kinds), ignore)
                .files(Set::Sources)
                .map(str::to_string)
                .collect()
        }
        Hints::None | Hints::Sources => sets.files(Set::Sources).map(str::to_string).collect(),
    }
}

/// \[C-38\] Where a check's hints are for its `sources`, and the key their paths are hashed
/// under.
#[derive(Debug, Clone)]
pub struct Place {
    pub address: Digest,
    pub key: Key,
}

impl Place {
    pub fn new(check_id: &Digest, sources: &[String]) -> Place {
        let paths = || sources.iter().map(String::as_str);
        Place {
            address: hints::address(check_id, &units::shape(paths())),
            key: Key::derive(paths()),
        }
    }
}

/// What a run asks hints with: where results are found, the check, whose hints, and the
/// adapter's word, which a hint must have.
pub struct Asker<'a> {
    pub store: &'a dyn UnitResults,
    pub check_id: Digest,
    pub systems: Vec<Os>,
    pub adapter: &'static str,
    /// \[C-41\] What the last [`Asker::fetch`] read, which an adapter asked for its test files:
    /// the plan reads their tests from it, without asking again.
    pub fetched: RefCell<Shared>,
}

impl<'a> Asker<'a> {
    pub fn new(
        store: &'a dyn UnitResults,
        check_id: Digest,
        systems: Vec<Os>,
        adapter: &'static str,
    ) -> Asker<'a> {
        Asker {
            store,
            check_id,
            systems,
            adapter,
            fetched: RefCell::new(Shared::default()),
        }
    }
}

/// \[C-38\] The hints a run read, newest first, each with the system it was asked for and only the
/// test files every path of which is a file of the scope; the content id of each it read, which
/// it won't send back; and the systems this run reads, which a `shared` entry of this machine's
/// must be from to count ([`lists`]).
#[derive(Debug, Clone, Default)]
pub struct Shared {
    pub hints: Vec<(Os, hints::Hint)>,
    pub received: Vec<HintKnown>,
    pub systems: Vec<Os>,
}

impl Shared {
    /// \[ADR 0071\] The newest list of test files a hint holds that names any: Vitest's.
    pub fn unit_files(&self) -> BTreeSet<String> {
        self.hints
            .iter()
            .map(|(_, hint)| &hint.unit_files)
            .find(|files| !files.is_empty())
            .cloned()
            .unwrap_or_default()
    }

    /// Whether another machine saw `unit` read anything.
    pub fn knows(&self, unit: &str) -> bool {
        self.hints
            .iter()
            .any(|(_, hint)| hint.units.contains_key(unit))
    }

    /// Whether any hint read names a test file.
    pub fn names_any(&self) -> bool {
        self.hints.iter().any(|(_, hint)| !hint.units.is_empty())
    }

    /// \[C-41\] The ids of the tests hints hold of `unit`.
    pub fn test_ids(&self, unit: &str) -> BTreeSet<&str> {
        self.hints
            .iter()
            .filter_map(|(_, hint)| hint.tests.get(unit))
            .flat_map(|tests| tests.keys().map(String::as_str))
            .collect()
    }

    /// \[C-41\] What hints say test `id` of `unit` read, newest first, each with the system it was
    /// asked for: its own list, or `None` for its test file's.
    pub fn test_seen(&self, unit: &str, id: &str) -> Vec<(Os, Option<&hints::Unit>)> {
        self.hints
            .iter()
            .filter_map(|(os, hint)| Some((*os, hint.tests.get(unit)?.get(id)?.as_ref())))
            .collect()
    }
}

impl Asker<'_> {
    /// \[C-38\] The newest hints for the check's address for `sources`, of each system it reads,
    /// read against the scope's files in `tree`. What can't be fetched or read is left out, said
    /// for debug only: no hint costs only what it would have spared.
    pub fn fetch(&self, sources: &[String], tree: &Tree) -> Shared {
        let place = Place::new(&self.check_id, sources);
        let mut out = Shared {
            systems: self.systems.clone(),
            ..Shared::default()
        };
        for os in &self.systems {
            let at = HintPlace {
                check_id: self.check_id,
                address: place.address,
                os: *os,
            };
            let found = match self.store.hints(&at) {
                Ok(found) => found,
                Err(why) => {
                    tracing::debug!("no hints for {}: {why}", os.word());
                    continue;
                }
            };
            for bytes in found {
                match hints::read(&bytes, &place.key, tree.paths()) {
                    Ok(read) => {
                        out.received.push(HintKnown {
                            address: place.address,
                            os: *os,
                            content_id: read.content_id,
                            how: HintHow::Received,
                        });
                        if read.hint.adapter == self.adapter {
                            out.hints.push((*os, read.hint));
                        }
                    }
                    Err(why) => tracing::debug!("a hint was left out: {why}"),
                }
            }
        }
        *self.fetched.borrow_mut() = out.clone();
        out
    }
}

/// Where a list of files came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// This machine's own entry: what the test file read when it ran here, or a list another
    /// machine of a system saw that reused it here.
    Here(Origin),
    /// \[ADR 0069, 0070\] What a build says it is made of alone: Cargo's dep-info, or for Go,
    /// with no entry here, what `go list` says.
    Build,
    /// A hint read in this run, of the system it was asked for.
    Hint(Os),
}

/// One list of files a test file's key might be made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    /// What its key hashes: the files read, every file now below a directory listed, and for Go
    /// what its test binary is built from now.
    pub files: BTreeSet<String>,
    /// The files read, as the entry or the hint has them.
    pub reads: BTreeSet<String>,
    /// \[ADR 0070\] The directories listed.
    pub dirs: BTreeSet<String>,
    pub from: From,
}

impl List {
    /// Whether another machine saw the test file read these files.
    pub fn seen_elsewhere(&self) -> bool {
        matches!(self.from, From::Hint(_) | From::Here(Origin::Shared(_)))
    }
}

/// \[C-38\] The one function that answers which lists of files `unit`'s key might be made from:
/// this machine's own entry, `own`, when it has one; then the lists of the hints, newest first;
/// the same list once, and [`MOST_LISTS`] at most. For Go, `compiled` is what its test binary is
/// built from now, which every list holds, and alone stands for the entry this machine lacks.
/// An entry here that is another system's list (`shared`) counts only when that system is one
/// this run reads (`shared.systems`, [`systems`]); otherwise it is as if there were none, so a
/// list a rule borrowed stops keying anything once the rule is gone. `None` when this machine's
/// entry says something of it wasn't followed: it runs, and records nothing (C-34). An empty
/// list of lists: nothing to key it by, and it runs.
pub fn lists(
    unit: &str,
    own: Option<&Mapped>,
    shared: &Shared,
    tree: &Tree,
    compiled: Option<&BTreeSet<String>>,
) -> Option<Vec<List>> {
    let make = |reads: &BTreeSet<String>, dirs: &BTreeSet<String>, from: From| {
        let mut files = compiled.cloned().unwrap_or_default();
        files.extend(reads.iter().cloned());
        files.extend(gotest::under(tree, dirs));
        List {
            files,
            reads: reads.clone(),
            dirs: dirs.clone(),
            from,
        }
    };
    let mut out: Vec<List> = Vec::new();
    let mut add = |list: List| {
        if out.len() < MOST_LISTS && !out.iter().any(|l| l.files == list.files) {
            out.push(list);
        }
    };
    let own = own.filter(|mapped| match mapped.origin {
        Origin::SeenHere => true,
        Origin::Shared(os) => shared.systems.contains(&os),
    });
    match own {
        Some(mapped) if !mapped.not_followed.is_empty() => return None,
        Some(mapped) => {
            let reads: BTreeSet<String> = mapped.reads.keys().cloned().collect();
            add(make(&reads, &mapped.dirs, From::Here(mapped.origin)));
        }
        None if compiled.is_some() => add(make(&BTreeSet::new(), &BTreeSet::new(), From::Build)),
        None => {}
    }
    for (os, hint) in &shared.hints {
        if let Some(seen) = hint.units.get(unit) {
            add(make(&seen.reads, &seen.dirs, From::Hint(*os)));
        }
    }
    Some(out)
}

/// \[C-40, C-41\] One list a test's key may be made from, and whether it is the test's own
/// (`reads: test`) rather than its test file's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestList {
    pub list: List,
    pub own: bool,
}

/// \[C-40, C-41, ADR 0081, 0082\] The lists a test's key may be made from, which the plan,
/// `observe`, `hushrun ready` and `hushrun why` all ask. With `reads: file`, its test file's
/// (`file_lists`, from [`lists`], which hold the hints' lists of it). With `reads: test`: the list
/// this machine's row for it holds, every file now below a directory it listed included, or its
/// test file's lists when the row holds none (it wasn't watched alone, and recorded on those);
/// then what hints say it read (`seen`, newest first), its own list or, for `None`, its test
/// file's. The same list once, and [`MOST_LISTS`] at most. With `reads: test`, a test neither a
/// row nor a hint holds has nothing to key it by.
pub fn test_lists(
    reads: units::Reads,
    file_lists: &[List],
    row: Option<&super::store::TestRow>,
    seen: &[(Os, Option<&hints::Unit>)],
    tree: &Tree,
) -> Vec<TestList> {
    let of_file = || {
        file_lists.iter().map(|list| TestList {
            list: list.clone(),
            own: false,
        })
    };
    if reads == units::Reads::File {
        return of_file().collect();
    }
    let own = |reads: BTreeSet<String>, dirs: &BTreeSet<String>, from: From| {
        let mut files = reads.clone();
        files.extend(gotest::under(tree, dirs));
        TestList {
            list: List {
                files,
                reads,
                dirs: dirs.clone(),
                from,
            },
            own: true,
        }
    };
    let mut out: Vec<TestList> = Vec::new();
    let mut add = |list: TestList| {
        if out.len() < MOST_LISTS && !out.iter().any(|l| l.list.files == list.list.files) {
            out.push(list);
        }
    };
    match row.map(|row| &row.list) {
        Some(Some(list)) => add(own(
            list.reads.keys().cloned().collect(),
            &list.dirs,
            From::Here(Origin::SeenHere),
        )),
        Some(None) => of_file().for_each(&mut add),
        None => {}
    }
    for (os, list) in seen {
        match list {
            Some(list) => add(own(list.reads.clone(), &list.dirs, From::Hint(*os))),
            None => of_file().for_each(&mut add),
        }
    }
    out
}

/// What [`choose`] decided of one test file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// It may be reused: the first list whose key passed, by its index, and that key.
    Passed(usize, Digest),
    /// Some list's key has a newest result that is a fail.
    Failed,
    /// No list's key has a result that counts here.
    Nothing,
}

/// \[C-38\] The one rule every reader decides a test file by, from its lists' keys (`None` for a
/// list no key can be computed for) and what the lookup found: it may be reused when some key has
/// a newest result that counts here and is a pass, and no key has one that is a fail. Two runs
/// that saw it read different files and disagree is a reason to run.
pub fn choose(keys: &[Option<Digest>], found: &BTreeMap<Digest, Found>) -> Choice {
    let results: Vec<Option<(Digest, &Found)>> = keys
        .iter()
        .map(|key| key.and_then(|key| found.get(&key).map(|f| (key, f))))
        .collect();
    if results.iter().flatten().any(|(_, f)| !f.passed) {
        return Choice::Failed;
    }
    results
        .iter()
        .enumerate()
        .find_map(|(at, result)| result.map(|(key, _)| Choice::Passed(at, key)))
        .unwrap_or(Choice::Nothing)
}

/// The batch lookup of `keys`, each once, in pages of [`MOST_UNITS`] keys (results-v0 §3): what
/// each found, and the check's reuse state, suspended when any page says so. One request when
/// there is no key, for the state.
pub fn lookup(
    store: &dyn UnitResults,
    keys: impl IntoIterator<Item = Digest>,
    counts: &dyn Fn(&RunResult) -> bool,
) -> Result<Lookup, String> {
    let keys: Vec<Digest> = keys
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if keys.is_empty() {
        return store.lookup(&[], counts);
    }
    let mut out: Option<Lookup> = None;
    for page in keys.chunks(MOST_UNITS) {
        let found = store.lookup(page, counts)?;
        match out.as_mut() {
            None => out = Some(found),
            Some(out) => {
                out.found.extend(found.found);
                if out.reuse.allows_reuse() && !found.reuse.allows_reuse() {
                    out.reuse = found.reuse;
                }
            }
        }
    }
    Ok(out.unwrap_or_default())
}

#[cfg(test)]
mod tests;
