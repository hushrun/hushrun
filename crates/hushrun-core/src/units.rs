//! \[ADR 0068\] Test by test: a check's test files, which RFC 0003 and the spec call units.
//!
//! - `checks.<name>.tests` in `hushrun.yaml` (C-5): [`Tests`].
//! - The four sets every file of a check's scope falls in, by one function, [`classify`] (C-30).
//! - A unit's key, by one function, [`unit_key`], with [`shape`] and [`Tree::digest`] for the
//!   digests it holds (C-31).
//! - A unit's state in a run, decided once, by [`state`], from the facts an adapter reported
//!   (C-32, ADR 0060).
//!
//! Like the rest of this crate it reads no file: what a run observed is passed in.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::digest::Digest;
use crate::glob::Pattern;
use crate::jcs::{self, JcsError};
use crate::names::{CheckName, ScopeName};

/// The schema of a unit's key (C-31). Changing it gives every unit a new key, so no earlier unit
/// result counts.
pub const UNIT_KEY_SCHEMA: &str = "hushrun.unit-key/v0";

/// `checks.<name>.tests`'s mode (C-5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// The default: the check passes or runs whole, as it always did.
    #[default]
    Off,
    /// Every test file runs, and hushrun records and says what reuse would have done.
    Observe,
    /// \[ADR 0069\] Test files that already passed on the files they read are reused, with audit
    /// runs (C-34).
    Reuse,
}

impl Mode {
    /// The word `hushrun.yaml` writes.
    pub fn word(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Observe => "observe",
            Mode::Reuse => "reuse",
        }
    }

    /// The mode a word names.
    pub fn from_word(word: &str) -> Option<Mode> {
        match word {
            "off" => Some(Mode::Off),
            "observe" => Some(Mode::Observe),
            "reuse" => Some(Mode::Reuse),
            _ => None,
        }
    }
}

/// `checks.<name>.tests` (C-5): its mode, the files no test reads (`ignore`) and the test files
/// that always run (`always`). Not part of the check's definition (E-6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tests {
    pub mode: Mode,
    /// Globs from the repository root: files in the ignored set (C-30).
    pub ignore: Vec<Pattern>,
    /// Globs from the repository root: units that always run.
    pub always: Vec<Pattern>,
}

impl Tests {
    /// Whether hushrun looks at the check's test files at all.
    pub fn is_on(&self) -> bool {
        self.mode != Mode::Off
    }

    /// Whether `tests.always` names `unit`.
    pub fn always_runs(&self, unit: &str) -> bool {
        self.always.iter().any(|glob| glob.is_match(unit))
    }
}

/// The set a file of the scope is in (C-30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Set {
    /// A test file, by the adapter's unit rule.
    Units,
    /// A file of a kind the adapter observes being read, or for a compiler-given adapter a `.rs`
    /// file some build names.
    Sources,
    /// Every other file: a change to one runs every unit.
    Rest,
    /// `tests.ignore`: a change to one runs nothing.
    Ignored,
}

/// What a source is, for one adapter (C-30).
#[derive(Debug, Clone, Copy)]
pub enum Sources<'a> {
    /// An observed adapter's kinds, by suffix: `.py` for pytest.
    Kinds(&'a [&'a str]),
    /// A compiler-given adapter's: the files the build names (rustc's dep-info), of which only
    /// those ending in one of the suffixes, `.rs` for Rust. A file of another kind that a build
    /// names, such as an `include_str!` target, stays in the rest set: a test may open it.
    Named(&'a BTreeSet<String>, &'a [&'a str]),
}

impl Sources<'_> {
    fn holds(&self, path: &str) -> bool {
        match self {
            Sources::Kinds(suffixes) => suffixes.iter().any(|s| path.ends_with(s)),
            Sources::Named(named, suffixes) => {
                named.contains(path) && suffixes.iter().any(|s| path.ends_with(s))
            }
        }
    }
}

/// Every file of a scope in exactly one of the four sets (C-30), as [`classify`] put it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sets {
    by_path: BTreeMap<String, Set>,
}

impl Sets {
    /// The set `path` is in; `None` for a file outside the scope.
    pub fn of(&self, path: &str) -> Option<Set> {
        self.by_path.get(path).copied()
    }

    /// The files of `set`, sorted by bytes.
    pub fn files(&self, set: Set) -> impl Iterator<Item = &str> {
        self.by_path
            .iter()
            .filter(move |(_, s)| **s == set)
            .map(|(path, _)| path.as_str())
    }

    /// Of `reads`, those a unit's key commits to: the units and sources it read (C-31). A file
    /// outside the scope, in the rest set or ignored is left out: the rest set has a digest of its
    /// own, and the others change nothing.
    pub fn keyed<'a>(&self, reads: impl IntoIterator<Item = &'a str>) -> BTreeSet<&'a str> {
        reads
            .into_iter()
            .filter(|path| matches!(self.of(path), Some(Set::Units | Set::Sources)))
            .collect()
    }
}

/// \[C-30\] The one function that puts each file of a check's scope in one of the four sets, for
/// an adapter whose unit rule is `is_unit` and whose sources are `sources`, with `ignore` the
/// check's `tests.ignore`. `ignore` wins, then the unit rule, then the sources; every other file
/// is in the rest set. Every reader calls it: the plan, the record and what `observe` says.
pub fn classify<'p>(
    scope: impl IntoIterator<Item = &'p str>,
    is_unit: &dyn Fn(&str) -> bool,
    sources: Sources<'_>,
    ignore: &[Pattern],
) -> Sets {
    let by_path = scope
        .into_iter()
        .map(|path| {
            let set = if ignore.iter().any(|glob| glob.is_match(path)) {
                Set::Ignored
            } else if is_unit(path) {
                Set::Units
            } else if sources.holds(path) {
                Set::Sources
            } else {
                Set::Rest
            };
            (path.to_string(), set)
        })
        .collect();
    Sets { by_path }
}

/// \[C-31\] `shape`: the SHA-256 of the sources set's paths, their names and not their contents:
/// each path's UTF-8 bytes followed by one NUL byte, sorted by bytes as D-4 sorts them.
pub fn shape<'a>(sources: impl IntoIterator<Item = &'a str>) -> Digest {
    let mut paths: Vec<&[u8]> = sources.into_iter().map(str::as_bytes).collect();
    paths.sort_unstable();
    paths.dedup();
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path);
        bytes.push(0);
    }
    Digest::of(&bytes)
}

/// The files of a scope with their kinds and content hashes, as a source manifest (D-4) lists
/// them: what [`Tree::digest`] hashes for `reads` and `rest` (C-31).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    /// Path to (kind code, content hash in lowercase hex).
    entries: BTreeMap<String, (char, String)>,
}

impl Tree {
    /// The tree a manifest (D-4) lists; `None` when the bytes aren't one.
    pub fn from_manifest(bytes: &[u8]) -> Option<Tree> {
        let header = format!("{}\n", crate::CANONICALIZATION);
        let mut rest = bytes.strip_prefix(header.as_bytes())?;
        let mut entries = BTreeMap::new();
        while !rest.is_empty() {
            let end = rest.iter().position(|b| *b == 0)?;
            let line = std::str::from_utf8(&rest[..end]).ok()?;
            rest = &rest[end + 1..];
            let mut parts = line.splitn(3, ' ');
            let kind = parts.next()?;
            let hash = parts.next()?;
            let path = parts.next()?;
            let mut chars = kind.chars();
            let code = chars.next()?;
            if chars.next().is_some() || hash.len() != 64 {
                return None;
            }
            entries.insert(path.to_string(), (code, hash.to_string()));
        }
        Some(Tree { entries })
    }

    /// Every path, sorted by bytes.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Whether the tree holds `path`.
    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    /// The kind code and content hash of `path`, as its manifest line writes them.
    pub fn entry(&self, path: &str) -> Option<(char, &str)> {
        self.entries
            .get(path)
            .map(|(kind, hash)| (*kind, hash.as_str()))
    }

    /// The source digest (D-4, D-5) of those of `paths` the tree holds. Unlike a scope's (D-6),
    /// it may be of no file: an empty set digests the manifest's header line alone (C-31).
    pub fn digest<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Digest {
        let chosen: BTreeSet<&str> = paths
            .into_iter()
            .filter(|path| self.entries.contains_key(*path))
            .collect();
        let mut bytes = format!("{}\n", crate::CANONICALIZATION).into_bytes();
        // A BTreeSet of &str orders by bytes, as D-4 does.
        for path in chosen {
            let (kind, hash) = &self.entries[path];
            bytes.extend_from_slice(format!("{kind} {hash} {path}").as_bytes());
            bytes.push(0);
        }
        Digest::of(&bytes)
    }
}

/// What a unit's key holds of its run's key (C-9, C-31): every field but `digest`, `external`
/// and `counts_from` as the run key has them, or `None` where it has none.
#[derive(Debug, Clone)]
pub struct KeyBase {
    pub check: CheckName,
    pub command: Vec<String>,
    pub cwd: String,
    pub scope: ScopeName,
    pub external: Option<Value>,
    pub counts_from: Option<Value>,
}

/// \[C-31\] The one function for a unit's key: the SHA-256 of the JCS form of the run key's fields
/// but `digest` (`base`), the unit's path, and the digests of what it read (`reads`), of the rest
/// set (`rest`) and of the sources set's paths (`shape`).
pub fn unit_key(
    base: &KeyBase,
    unit: &str,
    reads: &Digest,
    rest: &Digest,
    shape: &Digest,
) -> Result<Digest, JcsError> {
    let mut input = json!({
        "schema": UNIT_KEY_SCHEMA,
        "check": base.check,
        "command": base.command,
        "cwd": base.cwd,
        "scope": base.scope,
        "unit": unit,
        "reads": reads,
        "rest": rest,
        "shape": shape,
    });
    if let Some(external) = &base.external {
        input["external"] = external.clone();
    }
    if let Some(rule) = &base.counts_from {
        input["counts_from"] = rule.clone();
    }
    jcs::digest(&input)
}

/// Something an adapter couldn't follow while a unit ran (C-32): a subprocess, a fork, the
/// repository's own native code, a build it can't read. `why` is a word, `detail` informs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NotFollowed {
    pub why: String,
    pub detail: String,
}

/// What an adapter reported of one unit in a run, merged over its processes (C-32). The adapter
/// reports facts; [`state`] decides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Every test the runner selected of it finished: vacuously so for a file with none.
    pub complete: bool,
    /// A test of it failed or errored, or collecting it did.
    pub failed: bool,
    /// What the adapter couldn't follow while it ran.
    pub not_followed: BTreeSet<NotFollowed>,
    /// The adapter deselected it, as the plan listed it: every process that collected it did.
    pub deselected: bool,
}

/// What a run knows beyond a unit's own facts (C-32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunFacts {
    /// COMMAND exited 0.
    pub command_passed: bool,
    /// The adapter reports each failure by unit, so a unit's pass holds when COMMAND failed.
    pub failures_by_unit: bool,
    /// The plan listed the unit as one to reuse.
    pub planned: bool,
}

/// A unit's state in a run (RFC 0003 §3.5, C-32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    /// The plan listed it and the adapter deselected it: nothing is recorded, as reuse never
    /// chains.
    Reused,
    /// It ran, the adapter saw all it read, and it passed: `pass` under its key.
    Passed,
    /// It ran and a test of it failed or errored: `fail` under its key.
    Failed,
    /// It ran, and the adapter couldn't follow something it did: nothing, so it runs every time.
    NotObserved,
    /// It was collected and didn't run whole: the runner stopped first. Nothing.
    NotRun,
}

impl State {
    /// The word `hushrun` uses for it in its own files.
    pub fn word(self) -> &'static str {
        match self {
            State::Reused => "reused",
            State::Passed => "passed",
            State::Failed => "failed",
            State::NotObserved => "not-observed",
            State::NotRun => "not-run",
        }
    }

    /// What a unit in this state records under its key: `Some(true)` a pass, `Some(false)` a
    /// fail, `None` nothing.
    pub fn recorded(self) -> Option<bool> {
        match self {
            State::Passed => Some(true),
            State::Failed => Some(false),
            State::Reused | State::NotObserved | State::NotRun => None,
        }
    }

    /// Whether the unit ran, in whole or in part, in a way the run's counts say it ran.
    pub fn ran(self) -> bool {
        matches!(self, State::Passed | State::Failed | State::NotObserved)
    }
}

/// \[C-32, ADR 0060\] The one place a unit's state is decided, from what its adapter reported and
/// what the run knows. A failure is a failure, whatever else is true, a deselection included; a
/// unit the plan listed and the adapter deselected is `reused`; a unit that didn't run whole is
/// `not run`; one with something the adapter couldn't follow is `not observed`; and a pass holds
/// only when COMMAND passed or the adapter reports each failure by unit.
pub fn state(facts: &Facts, run: &RunFacts) -> State {
    // \[ADR 0069\] First, before a deselection: a process that may not deselect (a nested pytest, an
    // xdist worker the plan doesn't speak for) can run a unit another deselected, and its failure
    // must never read as a reuse.
    if facts.failed {
        return State::Failed;
    }
    if facts.deselected {
        // Deselected by something other than the plan, such as another plugin: nothing ran.
        return if run.planned {
            State::Reused
        } else {
            State::NotRun
        };
    }
    if !facts.complete {
        return State::NotRun;
    }
    if !facts.not_followed.is_empty() {
        return State::NotObserved;
    }
    if run.command_passed || run.failures_by_unit {
        State::Passed
    } else {
        State::NotRun
    }
}

/// \[C-34, ADR 0069\] How many of `reusable` units an audit runs anyway: 2%, rounded up, and at
/// least one when there is any (RFC 0003 D5).
pub fn audit_share(reusable: usize) -> usize {
    if reusable == 0 {
        0
    } else {
        reusable.div_ceil(50).max(1)
    }
}

/// \[C-34, ADR 0069\] The one function that chooses a run's audited units among those the plan
/// could reuse, each named with the key whose pass would reuse it: [`audit_share`] of them, those
/// whose SHA-256 of `<key hex>\n<run start, RFC 3339>` is lowest. Nobody picks: the key holds what
/// the unit read, and the start is the run's own.
pub fn audited<'a>(
    reusable: impl IntoIterator<Item = (&'a str, &'a Digest)>,
    started: &str,
) -> BTreeSet<String> {
    let mut ranked: Vec<(Digest, &str)> = reusable
        .into_iter()
        .map(|(unit, key)| {
            (
                Digest::of(format!("{}\n{started}", key.hex()).as_bytes()),
                unit,
            )
        })
        .collect();
    ranked.sort();
    let take = audit_share(ranked.len());
    ranked
        .into_iter()
        .take(take)
        .map(|(_, unit)| unit.to_string())
        .collect()
}

#[cfg(test)]
mod tests;
