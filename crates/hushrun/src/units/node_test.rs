//! \[ADR 0107\] The adapter for Node's own test runner, `node --test`, observed (C-30, C-32, C-39,
//! C-54): a unit is a test file, as the runner's own glob lists it; what it read is what its
//! process compiled, loaded and opened, since the runner gives each test file a process of its
//! own; and each test is told on its own, from the runner's own events.
//!
//! - Loading: a file embedded in this binary, written to `<cache dir>/adapters/`, which every Node
//!   process of COMMAND loads through `NODE_OPTIONS=--require`, added to what the environment
//!   had: no file of the repository changes.
//! - The unit rule: the test files the adapter listed, as the runner lists them, in the run that
//!   wrote the check's map, as Vitest's and Jest's (ADR 0071).
//! - Selection: whole test files in the main process's `process.argv`, which the runner reads
//!   after every preload; tests by `skip`, through `node:test`'s own functions, wrapped.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::units::{self, Facts, NotFollowed, Set, Sets, Sources, Tree};

use super::adapter::{
    Adapter, Hints, KeysOutside, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt,
    WhyAt,
};
use super::command::program_name;
use super::facts::FactsFile;

/// The adapter's word: its test files' facts files, this machine's store and a hint name it so.
pub const ADAPTER: &str = "node-test";
/// The name in the facts file of the runner's main process.
pub const MAIN: &str = "node-test-main";
/// What the adapter observes being read: its sources (C-30). Every other kind is rest.
pub const SOURCE_KINDS: &[&str] = super::node::SOURCE_KINDS;
/// The file every Node process of COMMAND loads.
const PRELOAD: &str = include_str!("hushrun_node_test.cjs");
/// What `hushrun why` and the run say of a test file whose process failed outside its tests.
const UNOWNED: &str = "Node's test runner reported its test file failed outside its tests, so no test of its file keeps a result of that run";

/// Node's options that take the next word as their value, when not written `--name=value`.
const NODE_VALUES: &[&str] = &[
    "-r",
    "--require",
    "--import",
    "--loader",
    "--experimental-loader",
    "-C",
    "--conditions",
    "--title",
    "--inspect-port",
    "--env-file",
    "--test-reporter",
    "--test-reporter-destination",
    "--test-name-pattern",
    "--test-skip-pattern",
    "--test-concurrency",
    "--test-timeout",
    "--test-shard",
    "--test-isolation",
    "--experimental-test-isolation",
    "--test-coverage-include",
    "--test-coverage-exclude",
    "--test-coverage-lines",
    "--test-coverage-branches",
    "--test-coverage-functions",
    "--test-global-setup",
    "--watch-path",
    "--input-type",
    "--stack-trace-limit",
];

/// COMMAND, when it starts Node's test runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// COMMAND turns the runner's coverage on.
    pub coverage: bool,
}

/// What `command` is for this adapter: `None` when it doesn't start Node's test runner;
/// `Some(Err(why))` when it does, in a way hushrun doesn't observe, which the run says.
pub fn recognize(command: &[String]) -> Option<Result<Recognized, String>> {
    let first = command.first()?;
    if !matches!(&*program_name(first), "node" | "nodejs") {
        return None;
    }
    // Node's own options, up to the first word that isn't one: `--test` must be among them, or
    // the words after a script are the script's.
    let mut test = false;
    let mut coverage = false;
    let mut refused = None;
    let mut words = command[1..].iter();
    while let Some(word) = words.next() {
        if word == "--" || !word.starts_with('-') {
            break;
        }
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_string())),
            None => (word.as_str(), None),
        };
        let value = || {
            if inline.is_some() {
                inline.clone()
            } else if NODE_VALUES.contains(&flag) {
                words.clone().next().cloned()
            } else {
                None
            }
        };
        match flag {
            "--test" => test = true,
            "--watch" | "--watch-path" => {
                refused.get_or_insert_with(|| format!("node --test {flag} reruns as files change"));
            }
            "--test-isolation" | "--experimental-test-isolation"
                if value().as_deref() == Some("none") =>
            {
                refused.get_or_insert_with(|| {
                    format!("node --test {flag}=none runs every test file in one process, so what each read can't be told")
                });
            }
            "--experimental-test-coverage" | "--test-coverage" => coverage = true,
            _ => {}
        }
        if inline.is_none() && NODE_VALUES.contains(&flag) {
            words.next();
        }
    }
    if !test {
        return None;
    }
    Some(match refused {
        Some(why) => Err(why),
        None => Ok(Recognized { coverage }),
    })
}

/// The directory that holds the adapter for this binary, written whole when it isn't there yet:
/// `<cache dir>/adapters/node-test-<first 16 hex of its SHA-256>`.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(PRELOAD.as_bytes()).hex();
    let dir = cache
        .join("adapters")
        .join(format!("node-test-{}", &digest[..16]));
    let path = dir.join("hushrun-node-test.cjs");
    if std::fs::read(&path).ok().as_deref() != Some(PRELOAD.as_bytes()) {
        super::facts::write_whole(&path, PRELOAD.as_bytes())?;
    }
    Ok(dir)
}

/// The variables that load the adapter from `dir`: `NODE_OPTIONS` with `--require <file>` added
/// to what COMMAND's environment had.
pub fn variables(
    dir: &Path,
    node_options: Option<OsString>,
) -> Result<Vec<(String, OsString)>, String> {
    Ok(vec![(
        "NODE_OPTIONS".to_string(),
        super::node::require(&dir.join("hushrun-node-test.cjs"), node_options)?,
    )])
}

/// What the adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub units: BTreeMap<String, super::Seen>,
    pub failures_by_unit: bool,
    /// The unit rule: the test files the runner listed, from the root.
    pub unit_files: BTreeSet<String>,
    /// The directories each unit listed, from the root.
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    /// Why the adapter left out nothing, when the plan had listed something.
    pub refused: Option<String>,
    /// \[ADR 0072\] Which module imports which, for `hushrun why`.
    pub edges: BTreeSet<super::db::Edge>,
}

/// How one test file ended, by the runner's main process.
#[derive(Debug, Default)]
struct Ended {
    failed: bool,
    deselected: bool,
    duration_ms: u64,
}

/// A list of strings a file's settings hold under `key`.
fn strings(file: &FactsFile, key: &str) -> Vec<String> {
    file.settings
        .get(key)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Reads what the adapter wrote in a run's directory, for the repository whose files are `tree`.
/// `Err` says why nothing can be told: the runner wrote nothing, stopped before its main process
/// wrote, or the adapter itself couldn't observe it there.
pub fn observe(files: &[FactsFile], tree: &Tree) -> Result<Observed, String> {
    let mains: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == MAIN).collect();
    let workers: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == ADAPTER).collect();
    let main = match mains.as_slice() {
        [] if workers.is_empty() => return Err(NOTHING.into()),
        [] => return Err("Node's test runner ended before hushrun's adapter wrote".into()),
        [main] => *main,
        _ => return Err("more than one of Node's test runners wrote for this run".into()),
    };
    if let Some(error) = main.errors.first() {
        return Err(error.chars().take(300).collect());
    }
    let unit_files: BTreeSet<String> = strings(main, "testFiles").into_iter().collect();
    let unowned: BTreeSet<String> = strings(main, "unowned").into_iter().collect();
    let whole: BTreeSet<NotFollowed> = main.not_followed.iter().map(Into::into).collect();
    let mut ended: BTreeMap<String, Ended> = BTreeMap::new();
    for unit in &main.units {
        ended.insert(
            unit.unit.clone(),
            Ended {
                failed: unit.failed,
                deselected: unit.deselected,
                duration_ms: unit.duration_ms,
            },
        );
    }
    let mut sessions: BTreeMap<String, usize> = BTreeMap::new();
    let mut listed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut failures_by_unit = main.failures_by_unit;
    for worker in &workers {
        failures_by_unit &= worker.failures_by_unit;
        for unit in &worker.units {
            *sessions.entry(unit.unit.clone()).or_default() += 1;
            listed
                .entry(unit.unit.clone())
                .or_default()
                .extend(unit.dirs.iter().cloned());
        }
    }
    let mut merged = super::facts::merged(files, ADAPTER);
    let names: BTreeSet<String> = ended.keys().chain(merged.keys()).cloned().collect();
    let mut out = Observed {
        failures_by_unit,
        unit_files,
        refused: main
            .settings
            .get("refused")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        edges: super::facts::edges_of(files, ADAPTER),
        ..Observed::default()
    };
    for unit in names {
        let end = ended.remove(&unit);
        let seen = merged.remove(&unit);
        let deselected = end.as_ref().is_some_and(|e| e.deselected)
            && seen.as_ref().is_none_or(|s| s.deselected);
        let mut not_followed = whole.clone();
        let mut reads: BTreeSet<String> = BTreeSet::from([unit.clone()]);
        let dirs = listed.remove(&unit).unwrap_or_default();
        let complete = match (&end, &seen) {
            _ if deselected => true,
            (Some(_), Some(seen)) => seen.complete,
            // The runner ran it, and its process never said what it read.
            (Some(_), None) => false,
            // Its process ran, and the main process never heard how it ended.
            (None, _) => false,
        };
        if !deselected && end.is_some() && sessions.get(&unit).copied().unwrap_or(0) == 0 {
            not_followed.insert(NotFollowed {
                why: "unseen".into(),
                detail: "hushrun's adapter didn't see its process run".into(),
            });
        }
        let mut failed = end.as_ref().is_some_and(|e| e.failed);
        let mut skipped = false;
        let mut tests = None;
        let mut duration_ms = end.as_ref().map_or(0, |e| e.duration_ms);
        if let Some(seen) = seen {
            failed |= seen.failed;
            skipped = seen.skipped > 0;
            reads.extend(seen.reads);
            not_followed.extend(seen.not_followed);
            tests = seen.tests;
            if end.is_none() {
                duration_ms = seen.duration_ms;
            }
        }
        // \[ADR 0107\] A test file whose process failed outside its tests (a late uncaught
        // exception, a crash, an exit before its tests): no test of it keeps a result.
        let unowned = unowned.contains(&unit);
        if unowned {
            failed = true;
            for test in tests.iter_mut().flat_map(|t| t.values_mut()) {
                test.finished = false;
                test.not_kept.get_or_insert_with(|| UNOWNED.to_string());
            }
        }
        reads.extend(super::gotest::under(tree, &dirs));
        reads.retain(|p| tree.contains(p));
        out.dirs.insert(unit.clone(), dirs);
        out.units.insert(
            unit,
            super::Seen {
                outside: Default::default(),
                declared: None,
                getenv: Default::default(),
                facts: Facts {
                    complete: complete && !unowned,
                    failed,
                    not_followed,
                    deselected,
                    skipped,
                    ..Facts::default()
                },
                reads,
                duration_ms,
                tests,
            },
        );
    }
    Ok(out)
}

/// What the run says when the runner wrote nothing for hushrun.
const NOTHING: &str = "Node's test runner wrote nothing for hushrun: its processes didn't load hushrun's adapter, as when a script sets NODE_OPTIONS instead of adding to it; keep what it had (`NODE_OPTIONS=\"$NODE_OPTIONS …\"`)";

/// \[ADR 0107\] The adapter for a COMMAND that starts Node's test runner; `None` for any other.
pub fn recognized(command: &[String], _cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    Some(recognize(command)?.map(|r| Box::new(r) as Box<dyn Adapter>))
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "Node's test runner"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            // \[C-54\] What a test read is its test file's: `reads: test` runs as `reads: file`.
            reads: false,
        }
    }

    fn hints(&self) -> Hints {
        Hints::Kinds(SOURCE_KINDS)
    }

    // `test_file`, `binds`, `keys` and `start_variables` are the trait's defaults, written out: a
    // call on this type then names this impl's own method, which the static test map follows to
    // it alone, where the trait's would count every adapter that overrides it (CONTRIBUTING.md,
    // "Which tests CI runs again").
    fn test_file(&self) -> &'static str {
        "test file"
    }

    fn binds(&self) -> bool {
        false
    }

    fn keys(&self) -> KeysOutside {
        KeysOutside::None
    }

    fn start_variables(&self, _: &Path) -> Vec<(String, Option<OsString>)> {
        Vec::new()
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let dir = install(at.cache)
            .map_err(|err| format!("can't write its adapter for Node's test runner: {err}"))?;
        variables(&dir, (at.env)("NODE_OPTIONS"))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        if self.coverage {
            return Err(
                "Node's test coverage is on, and a partial run's coverage would read as a drop"
                    .into(),
            );
        }
        let shared = super::fetch(at.asker, self.hints(), &Sets::default(), at.tree, at.ignore);
        let mut listed: BTreeSet<String> = at
            .map
            .unit_files
            .iter()
            .filter(|f| at.tree.contains(f))
            .cloned()
            .collect();
        if listed.is_empty() {
            listed = shared.unit_files();
        }
        if listed.is_empty() {
            return Err("no run here has listed the test files of Node's test runner yet".into());
        }
        let sets = units::classify(
            at.tree.paths(),
            &|path| listed.contains(path),
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        );
        let known = sets
            .files(Set::Units)
            .map(|unit| {
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    None,
                    None,
                );
                (unit.to_string(), lists)
            })
            .collect();
        Ok(Some(Planning {
            sets,
            known,
            received: shared.received,
            extras: RunExtras {
                python_files: None,
                test_files: Some(listed.into_iter().collect()),
            },
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let observed = observe(at.files, at.tree)?;
        let sets = units::classify(
            at.tree.paths(),
            &|path| observed.unit_files.contains(path),
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = observed
            .units
            .keys()
            .filter_map(|unit| {
                let lists = super::shared::lists(
                    unit,
                    at.map.units.get(unit),
                    &shared,
                    at.tree,
                    None,
                    None,
                )?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen: observed.units,
            failures_by_unit: observed.failures_by_unit,
            sets,
            planned,
            dirs: observed.dirs,
            unit_files: Some(observed.unit_files),
            refused: observed.refused,
            edges: observed.edges,
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, unit_files: &BTreeSet<String>) -> RunExtras {
        RunExtras {
            python_files: None,
            test_files: Some(unit_files.iter().cloned().collect()),
        }
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        units::classify(
            at.tree.paths(),
            &|p| at.listed.contains(p),
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        )
    }

    fn why_not_a_unit(&self, _: &WhyAt<'_>, check: &str, unit: &str) -> String {
        format!(
            "{unit} isn't a test file of {check} as far as this machine knows: Node's test runner didn't list it in its last run here"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("the test runner's list of test files is the one of its last run here")
    }
}

#[cfg(test)]
mod tests;
