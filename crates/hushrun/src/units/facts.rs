//! \[C-32\] The `HUSHRUN_UNITS` directory: what hushrun writes there for adapters, and what they
//! write back, each file whole.
//!
//! - `hushrun.json`, hushrun's, written before COMMAND starts: `{"schema": "hushrun.units/v0",
//!   "root": <the repository's root>, "reuse": [<units the plan lets adapters deselect>]}`, and
//!   \[ADR 0069\] what an adapter checks before it deselects any: `pythonFiles`, pytest's unit rule
//!   as hushrun read it, and `binaries`, each reused test binary's path and SHA-256; \[ADR 0077\]
//!   with `unit`, `reads` and `reuseTests`, the tests it may deselect one by one (C-39). An adapter
//!   that reads none of these three deselects only whole units, which runs more and never less.
//! - `<anything>.json`, an adapter's, one per process: `{"schema": "hushrun.unit-facts/v0", …}`,
//!   [`FactsFile`]. A name that starts with `.` is a file being written, and is never read.
//!   \[ADR 0077\] Or `hushrun.unit-facts/v1`, which adds to each unit its `tests` ([`TestFacts`]),
//!   what loading it ran and opened (`loaded`), and what shared setups ran and opened for which of
//!   its tests (`shared`); and to the file one `paths` table, which `ran`, `opened`, `dirs` and
//!   `imports` index, with `imports`, each file's direct imports (C-39). A `v0` file reads as it
//!   always did. Every list is parsed and merged across an adapter's processes ([`merged`]);
//!   which of it a run uses is the reader's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use hushrun_core::units::NotFollowed;

/// The schema of `hushrun.json`.
pub const RUN_SCHEMA: &str = "hushrun.units/v0";
/// The schema of an adapter's file.
pub const FACTS_SCHEMA: &str = "hushrun.unit-facts/v0";
/// \[ADR 0077\] The schema of an adapter's file that reports tests one by one (C-39).
pub const FACTS_SCHEMA_V1: &str = "hushrun.unit-facts/v1";
/// hushrun's own file in the directory.
pub const RUN_FILE: &str = "hushrun.json";
/// The most adapter files a run reads: past it, hushrun records no unit (C-32).
pub const MOST_FILES: usize = 100_000;

/// `hushrun.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunFile {
    pub schema: String,
    pub root: PathBuf,
    /// \[ADR 0069\] Units the plan lets an adapter deselect (C-32): those whose newest result that
    /// counts here passed on the files they read, but the audited ones.
    pub reuse: Vec<String>,
    /// \[ADR 0069\] pytest's `python_files` as hushrun read it: a plugin whose pytest uses another
    /// deselects nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_files: Option<Vec<String>>,
    /// \[ADR 0069\] The test binaries of `reuse`, each by the path and the SHA-256 of the build
    /// whose dep-info its key holds: the target runner reuses only a binary that is that one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binaries: Vec<Binary>,
    /// \[ADR 0071\] The test files the plan classified as such, from the root: Vitest's adapter
    /// deselects nothing when its own list isn't this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_files: Option<Vec<String>>,
    /// \[ADR 0077\] What the run reuses one by one, `file` or `test` (C-39): an adapter that knows
    /// tests reports each of them with `test`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// \[ADR 0077\] What a test is taken to have read, `file` or `test`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reads: Option<String>,
    /// \[ADR 0077\] Tests the plan lets an adapter deselect one by one, by their unit, each by its
    /// id: those whose newest result that counts here passed on the files they read, but the
    /// audited ones (C-39). A unit `reuse` lists isn't here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reuse_tests: BTreeMap<String, Vec<String>>,
}

/// \[ADR 0069\] A test binary the plan reuses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binary {
    pub unit: String,
    /// Its path, as the build named it.
    pub path: PathBuf,
    /// The SHA-256 of its bytes, lowercase hex.
    pub sha256: String,
}

/// Something an adapter couldn't follow, as its file writes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unfollowed {
    pub why: String,
    #[serde(default)]
    pub detail: String,
}

impl From<&Unfollowed> for NotFollowed {
    fn from(u: &Unfollowed) -> NotFollowed {
        NotFollowed {
            why: u.why.clone(),
            detail: u.detail.clone(),
        }
    }
}

/// One adapter process's file (C-32). An observed adapter writes `units`; hushrun's target
/// runner writes one `invocation` per test binary it started.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactsFile {
    pub schema: String,
    pub adapter: String,
    /// An xdist controller's file, which holds no unit.
    #[serde(default)]
    pub controller: bool,
    /// The adapter reports each failure by unit: a unit's pass holds when COMMAND failed.
    #[serde(default)]
    pub failures_by_unit: bool,
    /// What the runner said of its own configuration, such as pytest's `python_files`.
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    /// What it couldn't follow outside any unit: it applies to every unit of this file.
    #[serde(default)]
    pub not_followed: Vec<Unfollowed>,
    #[serde(default)]
    pub units: Vec<UnitFacts>,
    #[serde(default)]
    pub invocation: Option<Invocation>,
    /// The adapter's own errors: with any, nothing of this file's units is recorded.
    #[serde(default)]
    pub errors: Vec<String>,
    /// \[ADR 0069\] How many tests this process deselected, as `hushrun.json` listed their units.
    #[serde(default)]
    pub removed_items: u64,
    /// \[ADR 0072\] Direct edges between files of the repository it saw, `[from, to, kind]`,
    /// `kind` `import`, `open` or `read`: what `hushrun why` shows a path with, never a key.
    #[serde(default, deserialize_with = "edges")]
    pub edges: Vec<(String, String, String)>,
    /// \[ADR 0077\] The paths, from the repository root, that this file's `ran`, `opened`, `dirs`
    /// and `imports` name by their index.
    #[serde(default)]
    pub paths: Vec<String>,
    /// \[ADR 0077\] Each file's direct imports: the index of a file, as a string, to the indices
    /// of the files it imports.
    #[serde(default)]
    pub imports: BTreeMap<String, Vec<usize>>,
    /// \[ADR 0078\] The files each module opened from its own code: the index of a module, as a
    /// string, to the indices of the files. What a module read once and kept reaches each test
    /// that ran its code (RFC 0003 §3.3, C-40).
    #[serde(default)]
    pub opens: BTreeMap<String, Vec<usize>>,
    /// \[C-32\] The start of the runner this process belongs to: one id for every process of one
    /// start (pytest's controller and its xdist workers, one `ctest`, one Jest and its workers),
    /// another for each other start. A unit's tests are counted within a start and never summed
    /// across starts ([`merged`]). Files that say none are one start, as before.
    #[serde(default)]
    pub start: Option<String>,
    /// \[C-40\] The files each file re-exports from, shaped like `imports`: the index of a file,
    /// as a string, to the indices of the files it re-exports (`export * from`, `export { x }
    /// from`). A test that reads a file reads what it re-exports, to the end.
    #[serde(default)]
    pub reexports: BTreeMap<String, Vec<usize>>,
    /// \[C-32\] The runner left out something the plan didn't list, and why, in words people
    /// read: the run starts COMMAND once more with nothing left out, and goes by that start alone.
    #[serde(default)]
    pub left_out_unlisted: Option<String>,
    /// \[C-32\] The runner ran what was built before the files now (a stale program), and why, in
    /// words people read: a run that passed records nothing, not even the check's pass.
    #[serde(default)]
    pub stale_build: Option<String>,
    /// \[C-37\] Why no pass of the run is kept when COMMAND failed and its failures aren't told
    /// by unit (`failuresByUnit: false`), in words people read: such as an error no test owns. It
    /// is what `hushrun why` says of each test file and test of the run, in place of the core's.
    #[serde(default)]
    pub not_kept: Option<String>,
}

impl FactsFile {
    /// \[ADR 0077\] The paths `indices` name in this file's table; `None` when one names none,
    /// and then the list can't be told.
    pub fn resolve(&self, indices: &[usize]) -> Option<BTreeSet<String>> {
        indices
            .iter()
            .map(|i| self.paths.get(*i).cloned())
            .collect()
    }
}

/// A file's `edges`, or none when they don't read: they explain, and never stop a run.
fn edges<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<(String, String, String)>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

/// \[ADR 0072\] The edges `adapter`'s files report, those of a kind the store knows.
pub fn edges_of(files: &[FactsFile], adapter: &str) -> BTreeSet<super::db::Edge> {
    files
        .iter()
        .filter(|f| f.adapter == adapter && !f.controller)
        .flat_map(|f| f.edges.iter())
        .filter_map(|(from, to, kind)| {
            Some((
                from.clone(),
                to.clone(),
                super::db::EdgeKind::from_word(kind)?,
            ))
        })
        .collect()
}

/// What an observed adapter saw of one unit in one process.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitFacts {
    /// Its path from the repository root, `/`-separated.
    pub unit: String,
    /// How many of its tests the runner selected to run, after every deselection.
    #[serde(default)]
    pub selected: u64,
    /// How many of those finished, pass or fail.
    #[serde(default)]
    pub finished: u64,
    /// A test of it failed or errored, or collecting it did.
    #[serde(default)]
    pub failed: bool,
    /// The adapter deselected it, as `hushrun.json`'s `reuse` listed it.
    #[serde(default)]
    pub deselected: bool,
    #[serde(default)]
    pub duration_ms: u64,
    /// Files it read, from the repository root.
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub not_followed: Vec<Unfollowed>,
    /// \[ADR 0071\] Directories it listed, from the repository root or absolute: it read every
    /// file below each.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// \[ADR 0077\] Its tests, each on its own: present in a `v1` file whose run's `unit` is
    /// `test` (C-39). Absent, the adapter told nothing of its tests.
    #[serde(default)]
    pub tests: Option<Vec<TestFacts>>,
    /// \[ADR 0077\] What loading it ran and opened, outside any test's window.
    #[serde(default)]
    pub loaded: Option<Touched>,
    /// \[ADR 0077\] What each shared setup ran and opened (a fixture of module, class or session
    /// scope, a `beforeAll`), with the tests it served, by their index in `tests`.
    #[serde(default)]
    pub shared: Vec<SharedSetup>,
    /// \[ADR 0078\] What the runner loads for every test of it, by index into `paths`, as files
    /// whose code ran: pytest's `conftest.py` files above it (C-40).
    #[serde(default)]
    pub every_test: Vec<usize>,
    /// \[C-32\] How many of its finished tests were skipped (their body didn't run), for a unit
    /// told without its tests: in a run whose COMMAND failed it records no pass.
    #[serde(default)]
    pub skipped: u64,
}

/// \[ADR 0077\] What an adapter saw of one test in one process (C-39). An adapter that knows tests
/// and not what each read sends `id`, `finished`, `failed`, `deselected` and `durationMs` only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestFacts {
    /// Its id within its unit: for pytest, its node id after `<path>::`.
    pub id: String,
    /// It finished, pass or fail: its setup, body and teardown ran.
    #[serde(default)]
    pub finished: bool,
    /// It failed or errored, in setup, body or teardown.
    #[serde(default)]
    pub failed: bool,
    /// The adapter deselected it, as `hushrun.json` listed it or its unit.
    #[serde(default)]
    pub deselected: bool,
    #[serde(default)]
    pub duration_ms: u64,
    /// The files whose functions were called while it ran, by index into `paths`.
    #[serde(default)]
    pub ran: Option<Vec<usize>>,
    /// The files it opened, by index into `paths`.
    #[serde(default)]
    pub opened: Option<Vec<usize>>,
    /// The directories it listed, by index into `paths`.
    #[serde(default)]
    pub dirs: Option<Vec<usize>>,
    /// What the adapter couldn't follow inside its own window: its alone.
    #[serde(default)]
    pub not_followed: Vec<Unfollowed>,
    /// \[C-32, C-39\] It finished without its body running: skipped, disabled, an assumption not
    /// met. In a run whose COMMAND failed it records nothing, and its test file no pass.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    /// \[C-39\] The adapter never leaves it out, whatever `reuseTests` lists, such as one whose id
    /// another test of its file shares: it records no key of its own, so no plan lists it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub never_left_out: bool,
    /// \[C-37\] Why it kept no result, when it didn't finish and didn't fail, in people's words:
    /// this machine's store keeps it with the test's row, and `hushrun why` says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_kept: Option<String>,
}

/// \[ADR 0077\] Files that ran and files opened, by index into the file's `paths`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Touched {
    #[serde(default)]
    pub ran: Vec<usize>,
    #[serde(default)]
    pub opened: Vec<usize>,
}

/// \[ADR 0077\] A shared setup: the tests it served, by index into its unit's `tests`, and what it
/// ran and opened.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedSetup {
    pub tests: Vec<usize>,
    #[serde(default)]
    pub ran: Vec<usize>,
    #[serde(default)]
    pub opened: Vec<usize>,
}

/// One start of a test binary by hushrun's target runner.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invocation {
    /// The binary, as Cargo or nextest named it.
    pub binary: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// For a `--list`, the tests it listed (libtest's terse format), else none.
    #[serde(default)]
    pub listed: Option<Vec<String>>,
    /// Its exit code, when it exited.
    pub exit: Option<i32>,
    /// The signal that stopped it, when one did.
    pub signal: Option<i32>,
    pub duration_ms: u64,
    /// \[ADR 0069\] The runner didn't start it: `hushrun.json` lists it, and it is the binary the
    /// plan's key was for. A list answered with no test, a run with exit 0.
    #[serde(default)]
    pub reused: bool,
    /// \[ADR 0070\] The directory it ran in, which for `go test` is its package's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// \[ADR 0070\] Go's test log of the start, its lines as the binary wrote them, when it wrote
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testlog: Option<Vec<String>>,
    /// \[ADR 0070\] The test log had more lines than hushrun reads.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub testlog_truncated: bool,
}

/// Every adapter file of a run's directory, in name order, with those that don't parse or have
/// another schema left out; `None` when there are too many to read.
pub fn read_all(dir: &Path) -> Option<Vec<FactsFile>> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.ends_with(".json") && !name.starts_with('.') && name != RUN_FILE
        })
        .collect();
    if names.len() > MOST_FILES {
        return None;
    }
    names.sort();
    Some(
        names
            .iter()
            .filter_map(|path| {
                let bytes = std::fs::read(path).ok()?;
                let file: FactsFile = serde_json::from_slice(&bytes)
                    .map_err(|err| tracing::debug!("{}: {err}", path.display()))
                    .ok()?;
                (file.schema == FACTS_SCHEMA || file.schema == FACTS_SCHEMA_V1).then_some(file)
            })
            .collect(),
    )
}

/// What every process of an observed adapter saw of one unit, merged: the most tests any process
/// selected, since each collects them all; the tests every process finished; deselected only when
/// every process that saw it deselected it; and the union of everything else.
#[derive(Debug, Clone, Default)]
#[allow(
    dead_code,
    reason = "`loaded` and `shared` are what `reads: test` reads"
)]
pub struct Merged {
    /// The most any process selected, and the tests every process finished, over every start:
    /// what they say, never whether it ran whole (`complete`).
    pub selected: u64,
    pub finished: u64,
    /// \[C-32\] It ran whole: in every start that selected any of it, its processes finished as
    /// many as the most any of them selected (pytest-xdist's workers each select all and run a
    /// share). Never summed across starts.
    pub complete: bool,
    pub failed: bool,
    pub deselected: bool,
    pub duration_ms: u64,
    pub reads: BTreeSet<String>,
    pub not_followed: BTreeSet<NotFollowed>,
    pub failures_by_unit: bool,
    /// \[C-32\] How many of its finished tests were skipped, over its processes.
    pub skipped: u64,
    /// \[ADR 0077\] Its tests, merged across the processes that reported them, by id; `None` when
    /// no process told its tests (C-39).
    pub tests: Option<BTreeMap<String, MergedTest>>,
    /// \[ADR 0077\] What loading it ran and opened, from every process that said; `None` when
    /// none did, or `loaded_broken`.
    pub loaded: Option<(BTreeSet<String>, BTreeSet<String>)>,
    /// \[ADR 0077\] A process sent a `loaded` that can't be read: what loading it touched can't
    /// be told.
    pub loaded_broken: bool,
    /// \[ADR 0077\] Each shared setup: the ids of the tests it served, what it ran and opened.
    pub shared: Vec<(BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)>,
}

/// \[ADR 0077\] What every process saw of one test: it finished or failed in any; deselected only
/// when every process that reported it deselected it, as a unit is; the union of the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "`ran`, `opened` and `dirs` are what `reads: test` reads"
)]
pub struct MergedTest {
    pub finished: bool,
    pub failed: bool,
    pub deselected: bool,
    pub duration_ms: u64,
    pub not_followed: BTreeSet<NotFollowed>,
    /// \[C-39\] A process said it never leaves it out.
    pub never_left_out: bool,
    /// \[C-32\] A process that ran it said it was skipped.
    pub skipped: bool,
    /// \[C-37\] Why it kept no result, as the first process that said so said it.
    pub not_kept: Option<String>,
    /// What ran, was opened and listed while it ran, from the root; `None` when a process didn't
    /// say, or said it in a way that can't be read: then what it read can't be told test by test.
    pub ran: Option<BTreeSet<String>>,
    pub opened: Option<BTreeSet<String>>,
    pub dirs: Option<BTreeSet<String>>,
}

/// \[ADR 0077\] Each file's direct imports in `adapter`'s files, from the root, over its processes.
/// A file whose table names nothing for an index adds nothing.
#[allow(dead_code, reason = "`watches` reads every adapter's")]
pub fn imports(files: &[FactsFile], adapter: &str) -> BTreeMap<String, BTreeSet<String>> {
    let files: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == adapter).collect();
    edges_by_index(&files, |f| &f.imports)
}

/// A table of each file's files by index, over `files` but controllers', from the root.
fn edges_by_index(
    files: &[&FactsFile],
    table: impl Fn(&FactsFile) -> &BTreeMap<String, Vec<usize>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in files.iter().filter(|f| !f.controller) {
        for (from, to) in table(file) {
            let Some(from) = from.parse::<usize>().ok().and_then(|i| file.paths.get(i)) else {
                continue;
            };
            if let Some(to) = file.resolve(to) {
                out.entry(from.clone()).or_default().extend(to);
            }
        }
    }
    out
}

/// \[ADR 0078\] What every adapter file of a run says of what its tests read beyond each test:
/// each file's direct imports and opens, and per unit what loading it touched, what the runner
/// loads for each of its tests, and its shared setups. Read from every file of the run's directory
/// whatever adapter wrote it, so that no adapter has to give it (C-40).
#[derive(Debug, Clone, Default)]
pub struct Watches {
    pub imports: BTreeMap<String, BTreeSet<String>>,
    /// \[C-40\] Each file's re-exports, over every file of the run.
    pub reexports: BTreeMap<String, BTreeSet<String>>,
    pub opens: BTreeMap<String, BTreeSet<String>>,
    pub units: BTreeMap<String, UnitWatch>,
}

/// \[ADR 0078\] What one unit's processes said of it beyond its tests.
#[derive(Debug, Clone, Default)]
pub struct UnitWatch {
    /// What loading it ran and opened; `None` when a process didn't say, or said something of it
    /// that can't be read: then no test of it is told on its own.
    pub loaded: Option<(BTreeSet<String>, BTreeSet<String>)>,
    pub every: BTreeSet<String>,
    pub shared: Vec<(BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)>,
}

/// \[ADR 0078\] [`Watches`] of `files`.
pub fn watches(files: &[FactsFile]) -> Watches {
    let all: Vec<&FactsFile> = files.iter().collect();
    let mut out = Watches {
        imports: edges_by_index(&all, |f| &f.imports),
        reexports: edges_by_index(&all, |f| &f.reexports),
        opens: edges_by_index(&all, |f| &f.opens),
        units: BTreeMap::new(),
    };
    let mut broken: BTreeSet<String> = BTreeSet::new();
    for file in files.iter().filter(|f| !f.controller) {
        for unit in &file.units {
            if unit.tests.is_none() {
                continue;
            }
            let watch = out.units.entry(unit.unit.clone()).or_default();
            let loaded = unit
                .loaded
                .as_ref()
                .and_then(|l| file.resolve(&l.ran).zip(file.resolve(&l.opened)));
            let every = file.resolve(&unit.every_test);
            let shared: Option<Vec<_>> = unit
                .shared
                .iter()
                .map(|setup| {
                    let ids: Option<BTreeSet<String>> = setup
                        .tests
                        .iter()
                        .map(|i| unit.tests.as_ref()?.get(*i).map(|t| t.id.clone()))
                        .collect();
                    Some((
                        ids?,
                        file.resolve(&setup.ran)?,
                        file.resolve(&setup.opened)?,
                    ))
                })
                .collect();
            match (loaded, every, shared) {
                (Some((ran, opened)), Some(every), Some(shared))
                    if !broken.contains(&unit.unit) =>
                {
                    let into = watch.loaded.get_or_insert_with(Default::default);
                    into.0.extend(ran);
                    into.1.extend(opened);
                    watch.every.extend(every);
                    watch.shared.extend(shared);
                }
                _ => {
                    broken.insert(unit.unit.clone());
                    watch.loaded = None;
                }
            }
        }
    }
    out
}

/// \[ADR 0077\] Merges one process's tests of a unit into `into`. `running` holds the ids of the
/// tests a process before this one reported without deselecting them.
fn merge_tests(
    into: &mut BTreeMap<String, MergedTest>,
    running: &mut BTreeSet<String>,
    file: &FactsFile,
    tests: &[TestFacts],
) {
    let list = |indices: &Option<Vec<usize>>| indices.as_ref().and_then(|i| file.resolve(i));
    for test in tests {
        let first = !into.contains_key(&test.id);
        let merged = into.entry(test.id.clone()).or_insert_with(|| MergedTest {
            deselected: true,
            ..MergedTest::default()
        });
        // \[C-32\] It finished only when every process that reported it, and didn't deselect it,
        // finished it: a pytest-xdist worker reports only the tests it ran or deselected, and two
        // starts that both ran it must both have finished it.
        if !test.deselected {
            merged.finished = if running.insert(test.id.clone()) {
                test.finished
            } else {
                merged.finished && test.finished
            };
        }
        merged.failed |= test.failed;
        merged.skipped |= test.skipped && !test.deselected;
        merged.deselected &= test.deselected;
        merged.never_left_out |= test.never_left_out;
        if merged.not_kept.is_none() {
            merged.not_kept.clone_from(&test.not_kept);
        }
        merged.duration_ms = merged.duration_ms.saturating_add(test.duration_ms);
        merged
            .not_followed
            .extend(test.not_followed.iter().map(NotFollowed::from));
        // What it read is known only when every process that reported it said it.
        for (into, from) in [
            (&mut merged.ran, list(&test.ran)),
            (&mut merged.opened, list(&test.opened)),
            (&mut merged.dirs, list(&test.dirs)),
        ] {
            *into = match (first, into.take(), from) {
                (true, _, from) => from,
                (false, Some(mut had), Some(from)) => {
                    had.extend(from);
                    Some(had)
                }
                (false, _, _) => None,
            };
        }
    }
}

/// The units of `adapter`'s files, merged across its processes (C-32). Within one start
/// ([`FactsFile::start`]) a unit's `selected` is the most any process says and its `finished`
/// the sum, as pytest-xdist's workers need; across starts it ran whole (`complete`) only when it
/// did in every start that selected any of it, and it failed when it failed in any.
pub fn merged(files: &[FactsFile], adapter: &str) -> BTreeMap<String, Merged> {
    let mut out: BTreeMap<String, Merged> = BTreeMap::new();
    // Per unit, per start: the most selected and the sum finished.
    let mut by_start: BTreeMap<String, BTreeMap<Option<&str>, (u64, u64)>> = BTreeMap::new();
    let mut running: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in files
        .iter()
        .filter(|f| f.adapter == adapter && !f.controller)
    {
        let mut whole: BTreeSet<NotFollowed> = file.not_followed.iter().map(Into::into).collect();
        if !file.errors.is_empty() {
            whole.insert(NotFollowed {
                why: "adapter-error".into(),
                detail: file.errors[0].chars().take(200).collect(),
            });
        }
        for unit in &file.units {
            let merged = out.entry(unit.unit.clone()).or_insert_with(|| Merged {
                failures_by_unit: true,
                deselected: true,
                ..Merged::default()
            });
            merged.selected = merged.selected.max(unit.selected);
            merged.finished = merged.finished.saturating_add(unit.finished);
            let counted = by_start
                .entry(unit.unit.clone())
                .or_default()
                .entry(file.start.as_deref())
                .or_default();
            counted.0 = counted.0.max(unit.selected);
            counted.1 = counted.1.saturating_add(unit.finished);
            merged.failed |= unit.failed;
            merged.skipped = merged.skipped.saturating_add(unit.skipped);
            // \[ADR 0069\] A process that ran it, as one the plan doesn't speak for may, outweighs one
            // that deselected it.
            merged.deselected &= unit.deselected;
            merged.duration_ms = merged.duration_ms.saturating_add(unit.duration_ms);
            merged.reads.extend(unit.reads.iter().cloned());
            merged
                .not_followed
                .extend(unit.not_followed.iter().map(NotFollowed::from));
            merged.not_followed.extend(whole.iter().cloned());
            merged.failures_by_unit &= file.failures_by_unit;
            // \[ADR 0077\] Tests, and what loading it and its shared setups touched (C-39).
            if let Some(tests) = &unit.tests {
                merge_tests(
                    merged.tests.get_or_insert_with(BTreeMap::new),
                    running.entry(unit.unit.clone()).or_default(),
                    file,
                    tests,
                );
            }
            if let Some(loaded) = &unit.loaded {
                match file.resolve(&loaded.ran).zip(file.resolve(&loaded.opened)) {
                    Some((ran, opened)) if !merged.loaded_broken => {
                        let into = merged.loaded.get_or_insert_with(Default::default);
                        into.0.extend(ran);
                        into.1.extend(opened);
                    }
                    _ => {
                        merged.loaded_broken = true;
                        merged.loaded = None;
                    }
                }
            }
            for setup in &unit.shared {
                let ids: Option<BTreeSet<String>> = setup
                    .tests
                    .iter()
                    .map(|i| unit.tests.as_ref()?.get(*i).map(|t| t.id.clone()))
                    .collect();
                if let (Some(ids), Some(ran), Some(opened)) =
                    (ids, file.resolve(&setup.ran), file.resolve(&setup.opened))
                {
                    merged.shared.push((ids, ran, opened));
                }
            }
        }
    }
    for (unit, merged) in &mut out {
        merged.complete = by_start
            .get(unit)
            .into_iter()
            .flat_map(BTreeMap::values)
            .all(|(selected, finished)| finished >= selected);
    }
    out
}

/// \[C-32\] The suffix of the file a process of an adapter may make in the run's directory when it
/// begins (`<anything>.started`), and removes once its facts file is written whole.
pub const STARTED_SUFFIX: &str = ".started";

/// \[C-32\] Why a start of the runner never said what it saw, when one didn't: a
/// `<anything>.started` file left in the run's directory `dir` after COMMAND. The reason is the
/// `why` of the first such file, by name, whose content is a JSON object with a string `why`;
/// else [`ENDED_EARLY`].
pub fn unfinished_starts(dir: &Path) -> Option<String> {
    let mut left: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| {
            entry.file_type().is_ok_and(|t| t.is_file())
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(STARTED_SUFFIX))
        })
        .map(|entry| entry.path())
        .collect();
    if left.is_empty() {
        return None;
    }
    left.sort();
    let said = left.iter().find_map(|path| {
        let bytes = std::fs::read(path).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let why = value.get("why")?.as_str()?.trim();
        (!why.is_empty()).then(|| why.chars().take(300).collect::<String>())
    });
    Some(said.unwrap_or_else(|| ENDED_EARLY.to_string()))
}

/// \[C-32\] Why a `.started` file was left, when it doesn't say.
pub const ENDED_EARLY: &str = "a start of its runner ended before it said what it saw";

/// \[C-32\] Why the first file of `files` that says so ran on a stale build.
pub fn stale_build(files: &[FactsFile]) -> Option<String> {
    files
        .iter()
        .find_map(|f| f.stale_build.clone())
        .map(|why| why.chars().take(300).collect())
}

/// \[C-37\] Why no pass of the run is kept, as the first file of `files` that says so said it.
pub fn run_not_kept(files: &[FactsFile]) -> Option<String> {
    files
        .iter()
        .filter_map(|f| f.not_kept.as_deref())
        .map(str::trim)
        .find(|why| !why.is_empty())
        .map(|why| why.chars().take(300).collect())
}

/// \[C-32\] What the adapter said when its runner executed no test: the note of a file whose
/// `settings.ranNothing` says so (Gradle, every test task up to date or from its cache), or a
/// plain sentence when it has none.
pub fn ran_nothing(files: &[FactsFile]) -> Option<String> {
    let file = files
        .iter()
        .find(|f| f.settings.contains_key("ranNothing"))?;
    Some(
        file.settings
            .get("note")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .unwrap_or("its runner executed no test")
            .chars()
            .take(2000)
            .collect(),
    )
}

/// \[C-46\] What each JUnit 4 task of the run leaves out at most (`settings.leaveOutAtMost`,
/// `{most, units}`), one group per facts file that says it; `None` when no facts file of a task
/// told what it ran (`settings.gradle`), as in a run that executed none, so that the store keeps
/// what it had.
pub fn leave_out_at_most(files: &[FactsFile]) -> Option<Vec<super::store::LeaveOutAtMost>> {
    let told = files.iter().any(|f| f.settings.contains_key("gradle"));
    let groups: Vec<super::store::LeaveOutAtMost> = files
        .iter()
        .filter_map(|f| f.settings.get("leaveOutAtMost"))
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    (told || !groups.is_empty()).then_some(groups)
}

/// \[C-32\] Why the first file of `files` that says so left out what the plan didn't list.
pub fn left_out(files: &[FactsFile]) -> Option<String> {
    files
        .iter()
        .find_map(|f| f.left_out_unlisted.clone())
        .map(|why| why.chars().take(300).collect())
}

/// What a file says of a runner setting, as a list of strings, when every file that says it
/// agrees; `Err(())` when two disagree.
pub fn setting(files: &[FactsFile], adapter: &str, key: &str) -> Result<Option<Vec<String>>, ()> {
    let mut found: Option<Vec<String>> = None;
    for file in files.iter().filter(|f| f.adapter == adapter) {
        let Some(value) = file.settings.get(key) else {
            continue;
        };
        let list: Vec<String> = match value.as_array() {
            Some(items) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string).ok_or(()))
                .collect::<Result<_, _>>()?,
            None => return Err(()),
        };
        match &found {
            Some(seen) if *seen != list => return Err(()),
            _ => found = Some(list),
        }
    }
    Ok(found)
}

/// Writes `bytes` to `path` whole: a temporary file beside it, then a rename, so that a reader
/// never sees half of it.
pub fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let temp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let written = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-32
    #[test]
    fn a_started_file_left_says_its_own_why_or_that_its_start_ended_early() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unfinished_starts(dir.path()), None);
        // pytest's: its pid, which says nothing of why.
        std::fs::write(dir.path().join("pytest-7.started"), "7").unwrap();
        assert_eq!(unfinished_starts(dir.path()).as_deref(), Some(ENDED_EARLY));
        // One that says why, in a JSON object: that is the reason, whatever else was left.
        std::fs::write(
            dir.path().join("jest-9.started"),
            r#"{"why": "Jest started more than once"}"#,
        )
        .unwrap();
        assert_eq!(
            unfinished_starts(dir.path()).as_deref(),
            Some("Jest started more than once")
        );
        // A file of another name is no start.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("jest-claim"), "9").unwrap();
        assert_eq!(unfinished_starts(other.path()), None);
    }

    fn file(adapter: &str, units: Vec<UnitFacts>) -> FactsFile {
        FactsFile {
            schema: FACTS_SCHEMA.into(),
            adapter: adapter.into(),
            failures_by_unit: true,
            units,
            ..FactsFile::default()
        }
    }

    fn unit(name: &str, selected: u64, finished: u64, reads: &[&str]) -> UnitFacts {
        UnitFacts {
            unit: name.into(),
            selected,
            finished,
            reads: reads.iter().map(|r| r.to_string()).collect(),
            ..UnitFacts::default()
        }
    }

    // covers: C-32
    #[test]
    fn processes_merge_into_one_unit() {
        // Two xdist workers each collected all three tests of the file and ran some of them.
        let a = file("pytest", vec![unit("tests/test_a.py", 3, 2, &["app/x.py"])]);
        let mut b = file("pytest", vec![unit("tests/test_a.py", 3, 1, &["app/y.py"])]);
        b.not_followed.push(Unfollowed {
            why: "subprocess".into(),
            detail: "git".into(),
        });
        let controller = FactsFile {
            controller: true,
            ..file(
                "pytest",
                vec![unit("tests/test_a.py", 9, 0, &["secret.py"])],
            )
        };
        let merged = merged(&[a, b, controller], "pytest");
        let m = &merged["tests/test_a.py"];
        assert_eq!((m.selected, m.finished), (3, 3));
        assert_eq!(
            m.reads.iter().map(String::as_str).collect::<Vec<_>>(),
            ["app/x.py", "app/y.py"]
        );
        // What a process couldn't follow outside any unit reaches every unit it reported.
        assert!(m.not_followed.iter().any(|n| n.why == "subprocess"));
        // An adapter's own error makes its units not observed.
        let mut broken = file("pytest", vec![unit("tests/test_b.py", 1, 1, &[])]);
        broken.errors.push("on_open: boom".into());
        let merged = super::merged(&[broken], "pytest");
        assert!(
            merged["tests/test_b.py"]
                .not_followed
                .iter()
                .any(|n| n.why == "adapter-error")
        );
    }

    // covers: C-32
    #[test]
    fn a_unit_is_deselected_only_when_every_process_that_saw_it_deselected_it() {
        let mut gone = unit("tests/test_a.py", 0, 0, &[]);
        gone.deselected = true;
        let mut ran = unit("tests/test_a.py", 2, 2, &["app/x.py"]);
        ran.failed = true;
        let merged = merged(
            &[
                file("pytest", vec![gone.clone()]),
                file("pytest", vec![ran]),
            ],
            "pytest",
        );
        let m = &merged["tests/test_a.py"];
        assert!(!m.deselected && m.failed && m.finished == 2);
        let both = super::merged(
            &[
                file("pytest", vec![gone.clone()]),
                file("pytest", vec![gone]),
            ],
            "pytest",
        );
        assert!(both["tests/test_a.py"].deselected);
    }

    /// Whether the merged unit ran whole, as an adapter that uses [`merged`] reads it.
    fn ran_whole(m: &Merged) -> bool {
        m.complete
    }

    fn started(start: Option<&str>, units: Vec<UnitFacts>) -> FactsFile {
        let mut file: FactsFile = serde_json::from_value(serde_json::json!({
            "schema": FACTS_SCHEMA,
            "adapter": "ctest",
            "failuresByUnit": true,
            "start": start,
        }))
        .unwrap();
        file.units = units;
        file
    }

    // covers: C-32
    #[test]
    fn completeness_is_never_summed_across_separate_starts() {
        // `ctest -L L1; ctest -L L2 --stop-on-failure`: the first start selected one test of the
        // program and finished it, the second selected two and stopped after one. Summed, two of
        // two finished; one of the second's never ran.
        let first = started(Some("a"), vec![unit("prog", 1, 1, &[])]);
        let second = started(Some("b"), vec![unit("prog", 2, 1, &[])]);
        let m = merged(&[first.clone(), second], "ctest");
        assert!(!ran_whole(&m["prog"]));
        // Two pytest-xdist workers of one start: each selected all three and ran a share.
        let w1 = started(Some("x"), vec![unit("t.py", 3, 2, &[])]);
        let w2 = started(Some("x"), vec![unit("t.py", 3, 1, &[])]);
        assert!(ran_whole(
            &merged(&[w1.clone(), w2.clone()], "ctest")["t.py"]
        ));
        // A start complete in every start that selected any of it, and one that selected none.
        let none = started(Some("y"), vec![unit("t.py", 0, 0, &[])]);
        assert!(ran_whole(
            &merged(&[w1.clone(), w2.clone(), none], "ctest")["t.py"]
        ));
        let short = started(Some("z"), vec![unit("t.py", 3, 2, &[])]);
        assert!(!ran_whole(
            &merged(&[w1.clone(), w2.clone(), short], "ctest")["t.py"]
        ));
        // Files that say no start are one start, as before.
        let n1 = started(None, vec![unit("t.py", 3, 2, &[])]);
        let n2 = started(None, vec![unit("t.py", 3, 1, &[])]);
        assert!(ran_whole(&merged(&[n1, n2], "ctest")["t.py"]));
        // It failed when it failed in any.
        let mut failing = unit("prog", 1, 1, &[]);
        failing.failed = true;
        let m = merged(&[first, started(Some("b"), vec![failing])], "ctest");
        assert!(m["prog"].failed && ran_whole(&m["prog"]));
    }

    // covers: C-32, C-39
    #[test]
    fn a_test_finished_only_when_every_process_that_ran_it_finished_it() {
        let told = |tests: serde_json::Value| -> FactsFile {
            serde_json::from_value(serde_json::json!({
                "schema": FACTS_SCHEMA_V1,
                "adapter": "pytest",
                "failuresByUnit": true,
                "units": [{"unit": "t.py", "selected": 1, "finished": 1, "tests": tests}],
            }))
            .unwrap()
        };
        // Two starts ran it: one finished it, one stopped inside it.
        let done = told(serde_json::json!([{"id": "a", "finished": true}]));
        let cut = told(serde_json::json!([{"id": "a", "finished": false}]));
        let m = merged(&[done.clone(), cut], "pytest");
        assert!(!m["t.py"].tests.as_ref().unwrap()["a"].finished);
        // pytest-xdist: a worker that deselected it says nothing of whether it finished.
        let gone = told(serde_json::json!([{"id": "a", "deselected": true}]));
        let m = merged(&[gone.clone(), done.clone()], "pytest");
        let a = &m["t.py"].tests.as_ref().unwrap()["a"];
        assert!(a.finished && !a.deselected);
        let m = merged(&[done, gone.clone()], "pytest");
        assert!(m["t.py"].tests.as_ref().unwrap()["a"].finished);
        let m = merged(&[gone.clone(), gone], "pytest");
        let a = &m["t.py"].tests.as_ref().unwrap()["a"];
        assert!(!a.finished && a.deselected);
    }

    // covers: C-32
    #[test]
    fn a_directory_reads_only_whole_files_of_its_schema() {
        let dir = tempfile::tempdir().unwrap();
        let good = file("pytest", vec![unit("t.py", 1, 1, &[])]);
        write_whole(
            &dir.path().join("pytest-1.json"),
            &serde_json::to_vec(&good).unwrap(),
        )
        .unwrap();
        // Being written, another schema, hushrun's own and not JSON: none is read.
        std::fs::write(dir.path().join(".pytest-2.json.tmp"), b"{").unwrap();
        std::fs::write(dir.path().join(".pytest-3.json"), b"{}").unwrap();
        std::fs::write(
            dir.path().join("other.json"),
            br#"{"schema":"other/v1","adapter":"pytest"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.json"), b"{\"schema\":").unwrap();
        std::fs::write(
            dir.path().join(RUN_FILE),
            br#"{"schema":"hushrun.units/v0","root":"/","reuse":[]}"#,
        )
        .unwrap();
        let files = read_all(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].units[0].unit, "t.py");
        assert!(
            std::fs::read_dir(dir.path()).unwrap().flatten().all(|e| !e
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
                || e.file_name() == ".pytest-2.json.tmp")
        );
    }

    // covers: C-39
    #[test]
    fn a_v1_file_reports_tests_and_merges_them_across_processes() {
        // Two xdist workers: each collected the file's two tests, deselected the one the plan
        // listed, and ran one of the others. One saw a subprocess inside its test's window.
        let worker = |ran: &str, saw: Option<&str>| -> FactsFile {
            serde_json::from_value(serde_json::json!({
                "schema": FACTS_SCHEMA_V1,
                "adapter": "pytest",
                "failuresByUnit": true,
                "paths": ["tests/test_a.py", "app/x.py", "data.json", "app/y.py"],
                "imports": {"0": [1], "1": [3], "9": [1]},
                "units": [{
                    "unit": "tests/test_a.py", "selected": 1, "finished": 1,
                    "loaded": {"ran": [0], "opened": []},
                    "shared": [{"tests": [0, 1], "ran": [1], "opened": [2]}],
                    "tests": [
                        {"id": "test_reused", "deselected": true},
                        {"id": ran, "finished": true, "durationMs": 5, "ran": [0, 1],
                         "opened": [2], "dirs": [],
                         "notFollowed": saw.map(|d| vec![serde_json::json!({"why": "subprocess", "detail": d})]).unwrap_or_default()},
                    ],
                }],
            }))
            .unwrap()
        };
        let files = [worker("test_one", None), worker("test_two", Some("git"))];
        let merged = merged(&files, "pytest");
        let m = &merged["tests/test_a.py"];
        let tests = m.tests.as_ref().unwrap();
        assert_eq!(
            tests.keys().map(String::as_str).collect::<Vec<_>>(),
            ["test_one", "test_reused", "test_two"]
        );
        // Deselected only when every process that reported it deselected it.
        assert!(tests["test_reused"].deselected && !tests["test_reused"].finished);
        assert!(tests["test_one"].finished && !tests["test_one"].deselected);
        // What wasn't followed inside a test's window is that test's alone.
        assert!(tests["test_one"].not_followed.is_empty());
        assert_eq!(tests["test_two"].not_followed.len(), 1);
        assert!(m.not_followed.is_empty());
        // Lists come through the file's table.
        assert_eq!(
            tests["test_one"]
                .opened
                .as_ref()
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            ["data.json"]
        );
        assert_eq!(tests["test_reused"].ran, None);
        assert_eq!(m.shared.len(), 2);
        assert!(m.loaded.as_ref().unwrap().0.contains("tests/test_a.py"));
        let imports = imports(&files, "pytest");
        assert_eq!(imports["app/x.py"].iter().collect::<Vec<_>>(), ["app/y.py"]);
        assert!(!imports.contains_key("9"));
        // An index the table doesn't hold: what it read can't be told, and nothing else changes.
        let mut broken = worker("test_one", None);
        broken.units[0].tests.as_mut().unwrap()[1].ran = Some(vec![42]);
        let merged = super::merged(&[broken], "pytest");
        let t = &merged["tests/test_a.py"].tests.as_ref().unwrap()["test_one"];
        assert_eq!(t.ran, None);
        assert!(t.finished);
        // A v0 file says nothing of tests, and reads as it always did.
        let v0 = file("pytest", vec![unit("tests/test_a.py", 1, 1, &[])]);
        assert!(
            super::merged(&[v0], "pytest")["tests/test_a.py"]
                .tests
                .is_none()
        );
        // A `tests` that isn't a list of tests: the file isn't read at all.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pytest-1.json"),
            br#"{"schema":"hushrun.unit-facts/v1","adapter":"pytest","units":[{"unit":"t.py","tests":[{"finished":true}]}]}"#,
        )
        .unwrap();
        assert!(read_all(dir.path()).unwrap().is_empty());
    }
}
