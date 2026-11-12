//! \[ADR 0077\] What an adapter is to the rest of test by test: one trait, [`Adapter`], and one list
//! of the adapters a COMMAND may get, [`RECOGNIZED`].
//!
//! An adapter reports facts and decides nothing (RFC 0003 §2). The plan, the reading of what it
//! wrote, each unit's state and what a run records are the same code for every adapter, in
//! [`super`]; an adapter answers only what differs between runners:
//!
//! - which COMMAND it is for, and why it can't load safely ([`Recognize`]);
//! - what COMMAND's environment gains to load it ([`Adapter::variables`]);
//! - the units a plan knows of before COMMAND, the four sets, and the lists of files each unit's
//!   key may be made from ([`Adapter::plan`], [`Adapter::plan_from_files`]);
//! - what its files in `HUSHRUN_UNITS` came to after COMMAND ([`Adapter::read`]);
//! - what a second run's `hushrun.json` says besides `reuse` ([`Adapter::again`]);
//! - whether it knows tests one by one ([`Adapter::tests`]), and the words people read of it;
//! - what `hushrun why` can tell of its units from this machine alone (the `why_` methods).
//!
//! A new adapter is one module under `units/` that implements [`Adapter`] and a [`Recognize`]
//! function, one line in [`RECOGNIZED`], and its docs and tests. A runner hushrun can only get
//! into from `PATH` (`swift`, `ctest`) uses [`super::program`]: its adapter makes the directory in
//! [`StartAt::dir`] and returns [`super::program::variables`] from [`Adapter::variables`], and one
//! [`super::program::Program`] in [`super::program::PROGRAMS`] changes the real program's
//! arguments.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::glob::Pattern;
use hushrun_core::units::{Set, Sets, Tree};

use super::db::{Edge, HintKnown};
use super::facts::FactsFile;
use super::shared::{Asker, List, Shared};
use super::store::Map;
use super::{Seen, cargo, gotest, pytest, vitest};

/// How a module says COMMAND is its runner's: `None` when it isn't, `Some(Err)` with why it is but
/// the adapter can't load safely, which the run says once.
pub type Recognize = fn(&[String], &Path) -> Option<Result<Box<dyn Adapter>, String>>;

/// The adapters a COMMAND may get, tried in order. Any COMMAND none of them takes gets pytest's,
/// which does nothing where pytest never starts ([`choose`]).
pub const RECOGNIZED: &[Recognize] = &[cargo::recognized, gotest::recognized, vitest::recognized];

/// \[C-32\] The one adapter a run of `command` in `cwd` gets: the first of [`RECOGNIZED`] that
/// takes it, else pytest's; `Err` says why the one it would get can't load safely. The run,
/// `hushrun ready` and `hushrun why` all ask it.
pub fn choose(command: &[String], cwd: &Path) -> Result<Box<dyn Adapter>, String> {
    for recognize in RECOGNIZED {
        if let Some(found) = recognize(command, cwd) {
            return found;
        }
    }
    pytest::any_other(command)
}

/// \[C-39\] What an adapter knows of tests one by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnowsTests {
    /// It reports each test, leaves out the tests `hushrun.json`'s `reuseTests` lists, and says
    /// what it couldn't follow inside a test's own window: `unit: test` works.
    pub unit: bool,
    /// It watches what each test read: `reads: test` works.
    pub reads: bool,
}

/// \[C-38\] Whether, and by which files, an adapter's hints are addressed and keyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hints {
    /// None: its keys come from a build, which needs no hint (Cargo).
    None,
    /// By the sources set, as `shape` is.
    Sources,
    /// By every file of these kinds, no test file named, and a hint holds the runner's own list of
    /// test files (Vitest, ADR 0071).
    Kinds(&'static [&'static str]),
}

/// What starting an adapter has to go by.
pub struct StartAt<'a> {
    /// hushrun's cache directory, where an embedded plugin is written.
    pub cache: &'a Path,
    /// Where COMMAND runs.
    pub cwd: &'a Path,
    /// The run's own directory, removed when the run ends: where a directory of programs for
    /// `PATH` goes ([`super::program::make`]).
    #[allow(
        dead_code,
        reason = "for an adapter that puts a program on PATH; none does yet"
    )]
    pub dir: &'a Path,
    /// COMMAND's environment as hushrun has it.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
}

impl StartAt<'_> {
    /// Where this hushrun is, for a runner that starts it again.
    pub fn exe(&self) -> Result<PathBuf, String> {
        std::env::current_exe().map_err(|err| format!("can't tell where hushrun is: {err}"))
    }
}

/// What a plan has to go by, before COMMAND.
pub struct PlanAt<'a> {
    /// The scope's files now.
    pub tree: &'a Tree,
    pub root: &'a Path,
    pub cwd: &'a Path,
    /// The check's map on this machine (C-33).
    pub map: &'a Map,
    /// \[C-38\] What hints are asked with, when they can change something.
    pub asker: Option<&'a Asker<'a>>,
    /// `tests.ignore`.
    pub ignore: &'a [Pattern],
    /// What COMMAND's compiles get besides (B-2), for a plan that builds first.
    pub compile_env: &'a [(OsString, OsString)],
}

/// The units a plan knows of, each with the lists of files its key may be made from (C-38): `None`
/// when it can't tell, and the unit runs.
pub type Known = Vec<(String, Option<Vec<List>>)>;

/// What `hushrun.json` says to an adapter besides what the plan reuses (C-32).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunExtras {
    /// \[ADR 0069\] pytest's unit rule as hushrun read it.
    pub python_files: Option<Vec<String>>,
    /// \[ADR 0071\] The test files the plan classified as units, for an adapter whose unit rule is
    /// its runner's own list.
    pub test_files: Option<Vec<String>>,
}

/// What an adapter's plan reads before any key.
#[derive(Debug, Default)]
pub struct Planning {
    /// The four sets (C-30).
    pub sets: Sets,
    pub known: Known,
    /// \[C-38\] The hints read.
    pub received: Vec<HintKnown>,
    pub extras: RunExtras,
    /// \[ADR 0069\] For an adapter that [binds](Adapter::binds) its units: the file whose bytes
    /// each unit's reuse holds to.
    pub bound: BTreeMap<String, PathBuf>,
}

/// What reading an adapter's files has to go by, after COMMAND.
pub struct ReadAt<'a> {
    pub files: &'a [FactsFile],
    pub root: &'a Path,
    pub cwd: &'a Path,
    pub tree: &'a Tree,
    /// COMMAND exited 0.
    pub command_passed: bool,
    pub map: &'a Map,
    pub asker: Option<&'a Asker<'a>>,
    pub ignore: &'a [Pattern],
}

/// What an adapter's files came to, before any state is decided.
#[derive(Debug, Default)]
pub struct Read {
    /// Each unit, as seen, merged over its processes.
    pub seen: BTreeMap<String, Seen>,
    /// The adapter reports each failure by unit.
    pub failures_by_unit: bool,
    /// The four sets (C-30), by the adapter's rule for this run.
    pub sets: Sets,
    /// For each unit a plan of this run would have keyed, the lists it would have keyed it by
    /// (C-38): what `observe` says reuse would have done. A unit not here would have run.
    pub planned: BTreeMap<String, Vec<List>>,
    /// \[ADR 0069\] Each unit's file whose bytes its reuse holds to, by its unit.
    pub executables: BTreeMap<String, PathBuf>,
    /// \[ADR 0070\] The files each unit is made of, when they aren't its own path: a Go package's
    /// test files. All must be in the scope.
    pub own: BTreeMap<String, Vec<String>>,
    /// \[ADR 0070\] The directories each unit listed, which the map keeps for the plan.
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    /// \[ADR 0071\] The test files the runner listed, which the map keeps as the unit rule; \[C-38\]
    /// for Go, `go list`'s, which `hushrun why` reads on a machine whose map lacks a package.
    pub unit_files: Option<BTreeSet<String>>,
    /// \[ADR 0071\] Why the adapter deselected nothing the plan listed.
    pub refused: Option<String>,
    /// \[ADR 0072\] The direct edges the adapter saw, for `hushrun why`.
    pub edges: BTreeSet<Edge>,
    /// \[C-38\] The hints read for `planned`.
    pub received: Vec<HintKnown>,
}

/// What `hushrun why` has to go by: this machine's store and the files now, and no tool.
pub struct WhyAt<'a> {
    pub tree: &'a Tree,
    pub root: &'a Path,
    pub cwd: &'a Path,
    pub map: &'a Map,
    pub ignore: &'a [Pattern],
    /// \[C-38\] The hints read.
    pub shared: &'a Shared,
    /// \[ADR 0071\] The list of test files the map, or a hint, holds.
    pub listed: &'a BTreeSet<String>,
}

/// Why a plan from files alone can't be made for an adapter whose units a build names.
pub const BUILT: &str = "its test files are known from a build, which this doesn't make";

/// \[ADR 0077\] One runner's adapter. Every method but the first five has the answer most adapters
/// give.
pub trait Adapter: std::fmt::Debug {
    /// The word its facts files, this machine's store and a hint name it by: 1 to 32 of `a-z`,
    /// `0-9` and `-` (C-36, C-38).
    fn word(&self) -> &'static str;

    /// Its runner, for people: `its tests are reused by test file: <runner>'s tests can't…`.
    fn runner(&self) -> &'static str;

    /// What COMMAND's environment gains to load it, each keeping what the environment had.
    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String>;

    /// What its files in the run's directory came to: `Ok(None)` when it wrote nothing, so that
    /// everything ran and no unit is recorded; `Err` why they couldn't be read.
    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String>;

    /// What a second run's `hushrun.json` says besides `reuse`, for a run in `cwd` whose first run
    /// listed `unit_files`.
    fn again(&self, root: &Path, cwd: &Path, unit_files: &BTreeSet<String>) -> RunExtras;

    /// What one of its test files is called, for people: `test file`, `test binary`, `package`.
    fn test_file(&self) -> &'static str {
        "test file"
    }

    /// \[C-39\] What it knows of tests one by one. One that knows nothing keeps working with
    /// `unit: test`: its check reuses by test file, and says so once.
    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: false,
            reads: false,
        }
    }

    /// \[C-38\] How its hints are addressed and keyed.
    fn hints(&self) -> Hints {
        Hints::Sources
    }

    /// \[ADR 0069\] Whether a unit's reuse holds to the bytes of a file its build made (a test
    /// binary): a plan lists it with that file's path and SHA-256, and a second run too.
    fn binds(&self) -> bool {
        false
    }

    /// \[C-22\] The plan from the scope's files, the map and hints alone, starting no tool, which
    /// `hushrun ready` makes too. `Ok(None)` when there is nothing to say of the check's test
    /// files; `Err` why nothing can be reused.
    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let _ = at;
        Err(BUILT.into())
    }

    /// The plan's units, sets and lists, before COMMAND: by default the plan from files alone. An
    /// adapter whose units a build or a tool names makes it here.
    fn plan(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        self.plan_from_files(at)
    }

    /// \[C-37\] The unit `hushrun why` was asked about, as the check names it.
    fn why_unit(&self, asked: &str) -> String {
        asked.to_string()
    }

    /// \[C-37\] The four sets, as far as this machine knows, with no tool started.
    fn why_sets(&self, at: &WhyAt<'_>) -> Sets;

    /// \[C-37\] Whether `unit` is a unit of the check, as far as this machine knows.
    fn why_is_unit(&self, at: &WhyAt<'_>, sets: &Sets, unit: &str) -> bool {
        let _ = at;
        sets.of(unit) == Some(Set::Units)
    }

    /// \[C-37\] Why `unit`, in the scope and not ignored, isn't a unit of `check`.
    fn why_not_a_unit(&self, at: &WhyAt<'_>, check: &str, unit: &str) -> String;

    /// \[C-37\] What its units are known from when it isn't the files alone, for the line `as far
    /// as this machine knows: …`.
    fn why_known_from(&self) -> Option<&'static str> {
        None
    }

    /// \[C-37\] Whether `hushrun why` asks hints for it, with this map.
    fn why_asks_hints(&self, map: &Map) -> bool {
        let _ = map;
        true
    }

    /// \[C-37\] Whether its units are named by their directories (Go's packages): a path to a
    /// changed file goes by directories, and a unit isn't a file of the scope.
    fn why_units_are_directories(&self) -> bool {
        false
    }

    /// \[C-37\] What `hushrun why` says when a file a unit read changed and no edge leads to it.
    fn why_no_path(&self) -> Option<&'static str> {
        None
    }
}
