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

use std::collections::{BTreeMap, BTreeSet};

use hushrun_core::digest::Digest;
use hushrun_core::glob::Pattern;
use hushrun_core::hints::{self, Key, Os};
use hushrun_core::units::{self, KeyBase, Set, Sets, Sources, Tree};

use super::db::{HintHow, HintKnown};
use super::store::{Found, HintPlace, Lookup, Mapped, Origin, UnitResults};
use super::{MOST_UNITS, gotest, vitest};
use crate::results::RunResult;

/// The most lists a test file's key is computed from.
pub const MOST_LISTS: usize = 3;

/// \[C-38\] Whose hints a machine reads: those of the systems the check's `counts_from` names,
/// one lookup each, since only those systems leave results for it (ADR 0045); else its own. None
/// on a system that shares none. A machine always writes under its own.
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
    if named.is_empty() { vec![this] } else { named }
}

/// \[C-38\] The sources a check's hints are addressed and keyed by: its sources set, the files
/// `shape` hashes (C-31). For Vitest, whose list of test files a hint holds, every file of a
/// source kind, with no test file named: a new test file then gives another address, and
/// everything runs once, as Vitest's adapter does here (ADR 0071). The plan and the run that
/// writes a hint both ask it.
pub fn sources(vitest: bool, sets: &Sets, tree: &Tree, ignore: &[Pattern]) -> Vec<String> {
    if vitest {
        units::classify(
            tree.paths(),
            &|_| false,
            Sources::Kinds(vitest::SOURCE_KINDS),
            ignore,
        )
        .files(Set::Sources)
        .map(str::to_string)
        .collect()
    } else {
        sets.files(Set::Sources).map(str::to_string).collect()
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
}

/// \[C-38\] The hints a run read, newest first, each with only the test files every path of which
/// is a file of the scope; and the content id of each it read, which it won't send back.
#[derive(Debug, Clone, Default)]
pub struct Shared {
    pub hints: Vec<hints::Hint>,
    pub received: Vec<HintKnown>,
}

impl Shared {
    /// \[ADR 0071\] The newest list of test files a hint holds that names any: Vitest's.
    pub fn unit_files(&self) -> BTreeSet<String> {
        self.hints
            .iter()
            .map(|hint| &hint.unit_files)
            .find(|files| !files.is_empty())
            .cloned()
            .unwrap_or_default()
    }

    /// Whether another machine saw `unit` read anything.
    pub fn knows(&self, unit: &str) -> bool {
        self.hints.iter().any(|hint| hint.units.contains_key(unit))
    }
}

impl Asker<'_> {
    /// \[C-38\] The newest hints for the check's address for `sources`, of each system it reads,
    /// read against the scope's files in `tree`. What can't be fetched or read is left out, said
    /// for debug only: no hint costs only what it would have spared.
    pub fn fetch(&self, sources: &[String], tree: &Tree) -> Shared {
        let place = Place::new(&self.check_id, sources);
        let mut out = Shared::default();
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
                            out.hints.push(read.hint);
                        }
                    }
                    Err(why) => tracing::debug!("a hint was left out: {why}"),
                }
            }
        }
        out
    }
}

/// Where a list of files came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// This machine's own entry: what the test file read when it ran here, or a list another
    /// machine saw that reused it here.
    Here(Origin),
    /// \[ADR 0069, 0070\] What a build says it is made of alone: Cargo's dep-info, or for Go,
    /// with no entry here, what `go list` says.
    Build,
    /// A hint read in this run.
    Hint,
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
        matches!(self.from, From::Hint | From::Here(Origin::Shared))
    }
}

/// \[C-38\] The one function that answers which lists of files `unit`'s key might be made from:
/// this machine's own entry, `own`, when it has one; then the lists of the hints, newest first;
/// the same list once, and [`MOST_LISTS`] at most. For Go, `compiled` is what its test binary is
/// built from now, which every list holds, and alone stands for the entry this machine lacks.
/// `None` when this machine's entry says something of it wasn't followed: it runs, and records
/// nothing (C-34). An empty list of lists: nothing to key it by, and it runs.
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
    match own {
        Some(mapped) if !mapped.not_followed.is_empty() => return None,
        Some(mapped) => {
            let reads: BTreeSet<String> = mapped.reads.keys().cloned().collect();
            add(make(&reads, &mapped.dirs, From::Here(mapped.origin)));
        }
        None if compiled.is_some() => add(make(&BTreeSet::new(), &BTreeSet::new(), From::Build)),
        None => {}
    }
    for hint in &shared.hints {
        if let Some(seen) = hint.units.get(unit) {
            add(make(&seen.reads, &seen.dirs, From::Hint));
        }
    }
    Some(out)
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
