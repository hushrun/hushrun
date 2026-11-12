//! \[ADR 0068\] Test by test: a check's test files, which RFC 0003 and the spec call units.
//!
//! - `checks.<name>.tests` in `hushrun.yaml` (C-5): [`Tests`], with \[ADR 0077\] the unit a
//!   check reuses by ([`Unit`]) and what a test is taken to have read ([`Reads`]).
//! - The four sets every file of a check's scope falls in, by one function, [`classify`] (C-30).
//! - A unit's key, by one function, [`unit_key`], with [`shape`] and [`Tree::digest`] for the
//!   digests it holds (C-31); \[ADR 0077\] a test's, by one function beside it, [`test_key`]
//!   (C-39), and a test's name, by [`test_name`].
//! - \[ADR 0078\] What a test read, by one function, [`test_list`] (C-40).
//! - A unit's state in a run, decided once, by [`state`], from the facts an adapter reported
//!   (C-32, ADR 0060): a test file's, and \[ADR 0077\] a test's, from its own facts.
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

/// \[ADR 0077\] The schema of a test's key (C-39). A test file's key stays [`UNIT_KEY_SCHEMA`], so
/// every result recorded before tests were a unit counts as it did.
pub const TEST_KEY_SCHEMA: &str = "hushrun.unit-key/v1";

/// \[ADR 0077\] What joins a test's unit and its id in its name (C-39).
pub const TEST_SEPARATOR: &str = "::";

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

/// \[ADR 0077\] `checks.<name>.tests.unit` (C-5): what a check reuses one by one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Unit {
    /// The default: a test file, as RFC 0003 has it.
    #[default]
    File,
    /// Each test of a test file on its own (C-39).
    Test,
}

impl Unit {
    /// The word `hushrun.yaml` and `hushrun.json` write.
    pub fn word(self) -> &'static str {
        match self {
            Unit::File => "file",
            Unit::Test => "test",
        }
    }

    /// The unit a word names.
    pub fn from_word(word: &str) -> Option<Unit> {
        match word {
            "file" => Some(Unit::File),
            "test" => Some(Unit::Test),
            _ => None,
        }
    }
}

/// \[ADR 0077\] `checks.<name>.tests.reads` (C-5): what a test is taken to have read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Reads {
    /// The default: everything its test file read. As safe as a test file.
    #[default]
    File,
    /// What ran while it ran and what that imports directly, and what it opened: finer, and a
    /// trade a developer makes (RFC 0004 §3.2). Needs [`Unit::Test`].
    Test,
}

impl Reads {
    /// The word `hushrun.yaml` and `hushrun.json` write.
    pub fn word(self) -> &'static str {
        match self {
            Reads::File => "file",
            Reads::Test => "test",
        }
    }

    /// What a word names.
    pub fn from_word(word: &str) -> Option<Reads> {
        match word {
            "file" => Some(Reads::File),
            "test" => Some(Reads::Test),
            _ => None,
        }
    }
}

/// `checks.<name>.tests` (C-5): its mode, \[ADR 0077\] its unit and what a test read, the files no
/// test reads (`ignore`) and the test files that always run (`always`). Not part of the check's
/// definition (E-6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tests {
    pub mode: Mode,
    /// \[ADR 0077\] What the check reuses one by one: test files, the default, or tests.
    pub unit: Unit,
    /// \[ADR 0077\] What a test is taken to have read; `test` only with `unit: test`, which
    /// `hushrun.yaml`'s validation holds to (C-8).
    pub reads: Reads,
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

/// \[ADR 0069\] The most test files a plan looks up and a run records, and a hint holds (C-38):
/// past it, nothing is reused (results-v0 §3).
pub const MOST_UNITS: usize = 5_000;

/// \[ADR 0077\] The most tests a check reuses one by one (C-39): past it, the check reuses by test
/// file for that run, and says so.
pub const MOST_TESTS: usize = 50_000;

/// \[C-31\] `shape`: the SHA-256 of the sources set's paths, their names and not their contents:
/// each path's UTF-8 bytes followed by one NUL byte, sorted by bytes as D-4 sorts them.
pub fn shape<'a>(sources: impl IntoIterator<Item = &'a str>) -> Digest {
    Digest::of(&shape_bytes(sources))
}

/// The bytes [`shape`] hashes, which a hint's key is derived from too (C-38).
pub(crate) fn shape_bytes<'a>(sources: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut paths: Vec<&[u8]> = sources.into_iter().map(str::as_bytes).collect();
    paths.sort_unstable();
    paths.dedup();
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path);
        bytes.push(0);
    }
    bytes
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
    key_of(UNIT_KEY_SCHEMA, base, unit, None, reads, rest, shape)
}

/// \[C-39, ADR 0077\] The one function for a test's key: [`unit_key`]'s fields, computed the same
/// way, under [`TEST_KEY_SCHEMA`], with `test`, the test's id within its unit. `reads` digests the
/// test's list (RFC 0004 §3.2).
pub fn test_key(
    base: &KeyBase,
    unit: &str,
    test: &str,
    reads: &Digest,
    rest: &Digest,
    shape: &Digest,
) -> Result<Digest, JcsError> {
    key_of(TEST_KEY_SCHEMA, base, unit, Some(test), reads, rest, shape)
}

/// \[ADR 0077\] A test's name, everywhere a person reads it: its unit, `::`, and its id (C-39).
pub fn test_name(unit: &str, test: &str) -> String {
    format!("{unit}{TEST_SEPARATOR}{test}")
}

fn key_of(
    schema: &str,
    base: &KeyBase,
    unit: &str,
    test: Option<&str>,
    reads: &Digest,
    rest: &Digest,
    shape: &Digest,
) -> Result<Digest, JcsError> {
    let mut input = json!({
        "schema": schema,
        "check": base.check,
        "command": base.command,
        "cwd": base.cwd,
        "scope": base.scope,
        "unit": unit,
        "reads": reads,
        "rest": rest,
        "shape": shape,
    });
    if let Some(test) = test {
        input["test"] = json!(test);
    }
    if let Some(external) = &base.external {
        input["external"] = external.clone();
    }
    if let Some(rule) = &base.counts_from {
        input["counts_from"] = rule.clone();
    }
    jcs::digest(&input)
}

/// \[C-40, ADR 0078\] What an adapter saw of one test file for what its tests read, as paths from
/// the repository root (`hushrun.unit-facts/v1`, C-32).
#[derive(Debug, Clone, Copy)]
pub struct FileWatch<'a> {
    /// The test file.
    pub unit: &'a str,
    /// Its own list (C-31): what it imported, followed to the end, or opened.
    pub file: &'a BTreeSet<String>,
    /// The directories it listed.
    pub file_dirs: &'a BTreeSet<String>,
    /// Each file's direct imports.
    pub imports: &'a BTreeMap<String, BTreeSet<String>>,
    /// \[C-40\] The files each file re-exports from (`export * from`, `export { x } from`): a test
    /// that reads a file reads what it re-exports, followed to the end through re-exports only.
    pub reexports: &'a BTreeMap<String, BTreeSet<String>>,
    /// The files each module opened from its own code, read once and perhaps kept (RFC 0003 §3.3).
    pub opens: &'a BTreeMap<String, BTreeSet<String>>,
    /// What loading it ran (functions called, not top-level code) and opened: `None` when the
    /// adapter didn't say, and then no test of it can be told on its own.
    pub loaded: Option<(&'a BTreeSet<String>, &'a BTreeSet<String>)>,
    /// What the runner loads for every test of it, as files whose code ran: pytest's
    /// `conftest.py` files above it.
    pub every: &'a BTreeSet<String>,
    /// Each shared setup: the ids of the tests it served, what it ran and what it opened.
    pub shared: &'a [(BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)],
}

/// \[C-40\] What an adapter saw while one test ran: `None` for a list it didn't say, and then the
/// test wasn't watched alone.
#[derive(Debug, Clone, Copy)]
pub struct TestWatch<'a> {
    pub id: &'a str,
    /// Files a function of which was called while it ran: setup, body and teardown.
    pub ran: Option<&'a BTreeSet<String>>,
    /// Files it opened.
    pub opened: Option<&'a BTreeSet<String>>,
    /// Directories it listed.
    pub dirs: Option<&'a BTreeSet<String>>,
}

/// \[C-40\] A test's list: the files its key hashes, the directories every file below which it
/// read, and whether it is its own or, unwatched, its test file's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestList {
    pub files: BTreeSet<String>,
    pub dirs: BTreeSet<String>,
    /// It is the test's own (`reads: test`, watched), not its test file's.
    pub own: bool,
}

/// \[C-40, ADR 0078\] The one function for what a test read, which every reader calls: the plan,
/// the record, `observe`, `ready` and `why`.
///
/// With `reads: file`, or for a test the adapter didn't watch alone, or a test file whose loading
/// it didn't say, its test file's list (C-31). With `reads: test` (RFC 0004 §3.2):
///
/// 1. the test's own file;
/// 2. every file a function of which was called while it ran (its setup, body and teardown), and
///    what a shared setup that served it ran, what ran while its test file was loaded, and what
///    the runner loads for every test of the file: files whose code ran;
/// 3. every file that one of those, or its own file, imports directly: one step;
/// 4. every file it opened, every file a module whose code ran opened from its own code (read once
///    and kept), every file below a directory it listed, and what a shared setup that served it,
///    or loading its file, opened;
/// 5. for every file of those, the files it re-exports from, followed to the end through
///    re-exports only: a file that only re-exports has no function of its own to call.
///
/// It can miss a value that reached the test two imports away, or through no import (a registry
/// another file filled), with no function of the way called: that is the trade `reads: test`
/// names (RFC 0004 §3.2), which audit runs stand under.
pub fn test_list(reads: Reads, file: &FileWatch<'_>, test: &TestWatch<'_>) -> TestList {
    let whole = || TestList {
        files: file.file.clone(),
        dirs: file.file_dirs.clone(),
        own: false,
    };
    if reads == Reads::File {
        return whole();
    }
    let (Some(ran), Some(opened), Some(dirs), Some((loaded_ran, loaded_opened))) =
        (test.ran, test.opened, test.dirs, file.loaded)
    else {
        return whole();
    };
    let serving = file
        .shared
        .iter()
        .filter(|(ids, _, _)| ids.contains(test.id));
    let mut code: BTreeSet<String> = BTreeSet::from([file.unit.to_string()]);
    code.extend(ran.iter().cloned());
    code.extend(loaded_ran.iter().cloned());
    code.extend(file.every.iter().cloned());
    let mut files: BTreeSet<String> = opened.clone();
    files.extend(loaded_opened.iter().cloned());
    for (_, shared_ran, shared_opened) in serving {
        code.extend(shared_ran.iter().cloned());
        files.extend(shared_opened.iter().cloned());
    }
    for path in &code {
        files.extend(file.imports.get(path).into_iter().flatten().cloned());
        files.extend(file.opens.get(path).into_iter().flatten().cloned());
    }
    files.extend(code);
    // 5. What every file of it re-exports from, followed to the end through re-exports only.
    let mut pending: Vec<String> = files.iter().cloned().collect();
    while let Some(path) = pending.pop() {
        for next in file.reexports.get(&path).into_iter().flatten() {
            if files.insert(next.clone()) {
                pending.push(next.clone());
            }
        }
    }
    TestList {
        files,
        dirs: dirs.clone(),
        own: true,
    }
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
    /// \[ADR 0077\] Of a test file: some of its tests were reused one by one, so this run didn't
    /// execute all of it, and it didn't pass whole (C-39).
    pub tests_reused: bool,
    /// \[C-32, C-39\] Of a test: it finished without its body running (skipped, disabled, an
    /// assumption not met). Of a test file: one of its finished tests was. In a run whose COMMAND
    /// failed it proves nothing: a suite setup that failed skips what it hides, and a runner that
    /// stopped may call the test in flight skipped.
    pub skipped: bool,
    /// \[C-39\] Of a test: its test file failed while none of its tests failed, a failure no test
    /// explains (a failing `afterAll`, a crash between tests). Its pass would let the next run
    /// leave it out, and what failed would never run again.
    pub unexplained: bool,
    /// \[C-32\] A start of the runner began and never said it ended (a `.started` file left in
    /// `HUSHRUN_UNITS`): what it ran can't be told, so nothing of the run passes.
    pub unfinished_start: bool,
    /// \[C-32\] The runner ran a build older than the files (`staleBuild`): a pass proves nothing
    /// of them.
    pub stale_build: bool,
    /// \[C-39\] Of a test: its adapter never leaves it out (`neverLeftOut`), so no plan could
    /// list it by a pass of its own, and it keeps none.
    pub never_left_out: bool,
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

/// A unit's state in a run (RFC 0003 §3.5, C-32), and \[ADR 0077\] a test's (C-39).
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

/// \[C-37\] Why a test or a test file that ran keeps no result of its own, as [`decided`] said:
/// the rule that decided it, which `hushrun why` says in [`NotKept::sentence`]'s words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotKept {
    /// Something other than the plan deselected it: nothing ran.
    LeftOut,
    /// It didn't finish in every process that ran it.
    Unfinished,
    /// Of a test file: some of its tests were reused one by one, so it didn't run whole.
    TestsReused,
    /// A start of the runner didn't say what it saw: it ended first, or wasn't observed.
    UnfinishedStart,
    /// It was skipped, or one of its tests was, in a run whose COMMAND failed.
    SkippedInFailedRun,
    /// Of a test: its test file failed while none of its tests did.
    Unexplained,
    /// The runner ran a build older than the files.
    StaleBuild,
    /// COMMAND failed, and the adapter doesn't report each failure by unit.
    CommandFailed,
    /// Of a test: its adapter never leaves it out, so it keeps no pass of its own.
    NeverLeftOut,
}

impl NotKept {
    /// What `hushrun why` says of it, of a test (`test`) or of a test file.
    pub fn sentence(self, test: bool) -> &'static str {
        match (self, test) {
            (NotKept::LeftOut, _) => {
                "something other than hushrun's plan left it out the last time it was collected here, so it didn't run"
            }
            (NotKept::Unfinished, true) => {
                "it didn't finish in every process that ran it the last time, so it keeps no result"
            }
            (NotKept::Unfinished, false) => {
                "it didn't run whole in every start of its runner the last time, so it keeps no result of its own"
            }
            (NotKept::TestsReused, _) => {
                "some of its tests were reused one by one the last time it ran, so it didn't run whole and keeps no pass of its own"
            }
            (NotKept::UnfinishedStart, _) => {
                "a start of its runner didn't say what it saw, so no pass of that run is kept"
            }
            (NotKept::SkippedInFailedRun, true) => {
                "it was skipped in a run that failed, and a skip there proves nothing: what failed may have skipped what it hides"
            }
            (NotKept::SkippedInFailedRun, false) => {
                "one of its tests was skipped in a run that failed, and a skip there proves nothing: what failed may have skipped what it hides"
            }
            (NotKept::Unexplained, _) => {
                "its test file failed while none of its tests did, and what failed may need this test to run, so it keeps no pass"
            }
            (NotKept::StaleBuild, _) => {
                "it ran on a build older than these files, so its pass proves nothing of them"
            }
            (NotKept::CommandFailed, true) => {
                "its run failed, and its adapter doesn't report each failure by test, so no pass of that run is kept"
            }
            (NotKept::CommandFailed, false) => {
                "its run failed, and its adapter doesn't report each failure by test file, so no pass of that run is kept"
            }
            (NotKept::NeverLeftOut, _) => {
                "its adapter can't leave it out of a run, so it runs every time and keeps no pass of its own"
            }
        }
    }
}

/// \[C-32, C-37\] A unit's or a test's state in a run, and why it keeps no result of its own
/// when it ran and keeps none: what [`decided`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub state: State,
    /// Why it keeps nothing: for `not run`, and for a `passed` test that keeps no pass
    /// ([`NotKept::NeverLeftOut`]). `None` otherwise: a reuse, a failure, what wasn't followed
    /// (its own words), a pass kept.
    pub not_kept: Option<NotKept>,
}

impl Decision {
    /// What it records under its key: `Some(true)` a pass, `Some(false)` a fail, `None` nothing.
    pub fn recorded(self) -> Option<bool> {
        match (self.state.recorded(), self.not_kept) {
            (Some(true), Some(_)) => None,
            (recorded, _) => recorded,
        }
    }
}

/// A unit's state in a run: [`decided`]'s.
pub fn state(facts: &Facts, run: &RunFacts) -> State {
    decided(facts, run).state
}

/// \[C-32, ADR 0060\] The one place a unit's state is decided, from what its adapter reported and
/// what the run knows. A failure is a failure, whatever else is true, a deselection included; a
/// unit the plan listed and the adapter deselected is `reused`; a unit that didn't run whole is
/// `not run`; one with something the adapter couldn't follow is `not observed`; and a pass holds
/// only when COMMAND passed or the adapter reports each failure by unit.
///
/// \[ADR 0077\] A test is decided here too, from its own facts: `complete` that it finished,
/// `not_followed` what wasn't followed inside its window and outside every window of its file. A
/// test file some of whose tests were reused one by one didn't run whole: `not run`, so it records
/// no pass of its own (C-39).
///
/// \[C-32\] In a run whose COMMAND failed, a test that was skipped, and a test file one of whose
/// tests was, are `not run`: a skip there proves nothing. In a run that passed, a skip is a pass.
/// \[C-39\] A test of a test file that failed while none of its tests failed is `not run`.
///
/// \[C-32\] A run with a start that never said it ended, or on a stale build, is `not run` for
/// all but its failures. \[C-39\] A test its adapter never leaves out passes and keeps no pass.
///
/// \[C-37\] With each `not run`, the rule that said so: why `hushrun why` says it kept nothing.
pub fn decided(facts: &Facts, run: &RunFacts) -> Decision {
    let not_run = |why| Decision {
        state: State::NotRun,
        not_kept: Some(why),
    };
    // \[ADR 0069\] First, before a deselection: a process that may not deselect (a nested pytest, an
    // xdist worker the plan doesn't speak for) can run a unit another deselected, and its failure
    // must never read as a reuse.
    if facts.failed {
        return Decision {
            state: State::Failed,
            not_kept: None,
        };
    }
    if facts.deselected {
        // Deselected by something other than the plan, such as another plugin: nothing ran.
        return if run.planned {
            Decision {
                state: State::Reused,
                not_kept: None,
            }
        } else {
            not_run(NotKept::LeftOut)
        };
    }
    if !facts.complete {
        return not_run(NotKept::Unfinished);
    }
    if facts.tests_reused {
        return not_run(NotKept::TestsReused);
    }
    // \[C-32\] A start that never said it ended: what it ran can't be told.
    if facts.unfinished_start {
        return not_run(NotKept::UnfinishedStart);
    }
    // \[C-32\] A skip in a run whose COMMAND failed: nothing ran that a pass could stand on.
    if facts.skipped && !run.command_passed {
        return not_run(NotKept::SkippedInFailedRun);
    }
    // \[C-39\] Its test file failed and no test of it did: what failed may need this test to run.
    if facts.unexplained {
        return not_run(NotKept::Unexplained);
    }
    if !facts.not_followed.is_empty() {
        return Decision {
            state: State::NotObserved,
            not_kept: None,
        };
    }
    // \[C-32\] What ran was built before these files.
    if facts.stale_build {
        return not_run(NotKept::StaleBuild);
    }
    if !(run.command_passed || run.failures_by_unit) {
        return not_run(NotKept::CommandFailed);
    }
    Decision {
        state: State::Passed,
        not_kept: facts.never_left_out.then_some(NotKept::NeverLeftOut),
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
