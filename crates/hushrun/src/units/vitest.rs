//! \[ADR 0071\] The Vitest adapter, observed (C-30, C-32): a unit is a test file, as Vitest's own
//! `include` and `exclude` name it, and what it read is what Vitest's workers ran and opened while
//! it ran, with the config files every test file reads.
//!
//! - Loading: a Vite plugin, embedded in this binary and written to `<cache dir>/adapters/`, that
//!   the repository's Vitest config loads from `HUSHRUN_VITEST` with one line `hushrun init`
//!   proposes ([`CONFIG_LINE`]). Vitest has no documented way to take a plugin, a reporter or a
//!   setup file from the environment, so without the line hushrun observes nothing, and says so.
//! - The unit rule: the test files the adapter listed with Vitest's own matcher in the run that
//!   wrote the check's map. A file no run listed yet is a source until one does; the adapter
//!   deselects nothing when Vitest's list isn't the one the plan had.
//! - Selection: after Vitest's own filters and `--shard`, in its sequencer, before any worker
//!   imports a test file.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::units::{self, Facts, NotFollowed, Set, Sets, Sources, Tree};

use super::adapter::{
    Adapter, Hints, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::facts::{FactsFile, MergedTest, TestFacts, Unfollowed};

/// The adapter's name in its main process's facts file.
pub const ADAPTER: &str = "vitest";
/// The adapter's name in a worker's facts file.
pub const WORKER: &str = "vitest-worker";
/// What the adapter observes being read: its sources (C-30). Vue, Svelte and other kinds a
/// plugin compiles are in the rest set.
pub const SOURCE_KINDS: &[&str] = &[".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"];
/// The variable the config line loads the plugin from.
pub const VARIABLE: &str = "HUSHRUN_VITEST";
/// The line a repository's Vitest config needs, in its `plugins`.
pub const CONFIG_LINE: &str = "plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())]";
/// The plugin, in Vitest's own process.
const PLUGIN: &str = include_str!("hushrun_vitest.mjs");
/// The setup file the plugin puts first, in each worker.
const SETUP: &str = include_str!("hushrun_vitest_setup.mjs");
/// The config files Vitest looks for, in the order it does.
const CONFIG_NAMES: &[&str] = &[
    "vitest.config.ts",
    "vitest.config.mts",
    "vitest.config.cts",
    "vitest.config.js",
    "vitest.config.mjs",
    "vitest.config.cjs",
    "vite.config.ts",
    "vite.config.mts",
    "vite.config.cts",
    "vite.config.js",
    "vite.config.mjs",
    "vite.config.cjs",
];

/// COMMAND, when it starts one run of Vitest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// `--config`'s file, as COMMAND names it.
    pub config: Option<String>,
    /// `--root`'s directory, as COMMAND names it.
    pub root: Option<String>,
    /// COMMAND turns coverage on.
    pub coverage: bool,
}

/// Programs that start the program after them, with the options of theirs that come first.
const LAUNCHERS: &[(&str, &[&str])] = &[
    (
        "npx",
        &["--no-install", "--no", "-y", "--yes", "--quiet", "-q"],
    ),
    ("bunx", &["--bun"]),
    ("pnpx", &[]),
];

/// What `command` is for the Vitest adapter: `None` when it doesn't start Vitest; `Some(Err(why))`
/// when it does, in a way hushrun doesn't observe, which the run says.
pub fn recognize(command: &[String]) -> Option<Result<Recognized, String>> {
    let name = |word: &str| super::command::program_name(word).to_string();
    let mut rest = command;
    loop {
        let first = name(rest.first()?);
        match first.as_str() {
            "vitest" => break,
            "pnpm" | "yarn" | "bun" | "npm" => {
                // `pnpm vitest`, `pnpm exec vitest`, `yarn exec vitest`, `bun x vitest`,
                // `npm exec vitest`, `npm exec -- vitest`.
                let mut next = &rest[1..];
                if matches!(next.first().map(String::as_str), Some("exec" | "x" | "dlx")) {
                    next = &next[1..];
                }
                if next.first().map(String::as_str) == Some("--") {
                    next = &next[1..];
                }
                if next.first().map(|w| name(w)) != Some("vitest".to_string()) {
                    return None;
                }
                rest = next;
            }
            other => {
                let (_, options) = LAUNCHERS.iter().find(|(n, _)| *n == other)?;
                let mut next = &rest[1..];
                while next.first().is_some_and(|w| options.contains(&w.as_str())) {
                    next = &next[1..];
                }
                if next.first().map(|w| name(w)) != Some("vitest".to_string()) {
                    return None;
                }
                rest = next;
            }
        }
    }
    let args = &rest[1..];
    let mut out = Recognized {
        config: None,
        root: None,
        coverage: false,
    };
    let mut run = false;
    let mut words = args.iter().enumerate();
    while let Some((i, word)) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value.to_string())),
            _ => (word.as_str(), None),
        };
        match flag {
            "run" if i == 0 => run = true,
            "watch" | "dev" | "bench" | "list" | "init" | "related" if i == 0 => {
                return Some(Err(format!(
                    "vitest {flag} isn't one run of the test files"
                )));
            }
            "--run" => run = true,
            "--watch" | "-w" => {
                if inline.as_deref() != Some("false") {
                    return Some(Err("vitest --watch reruns as files change".into()));
                }
            }
            "--typecheck" | "--browser" | "--ui" => {
                if inline.as_deref() != Some("false") {
                    return Some(Err(format!(
                        "vitest {flag} runs test files hushrun doesn't observe"
                    )));
                }
            }
            "--coverage" | "--coverage.enabled" => {
                out.coverage = inline.as_deref() != Some("false");
            }
            "--config" | "-c" => {
                out.config = inline.or_else(|| words.next().map(|(_, w)| w.clone()));
            }
            "--root" | "-r" => {
                out.root = inline.or_else(|| words.next().map(|(_, w)| w.clone()));
            }
            _ => {}
        }
    }
    if !run {
        return Some(Err(
            "vitest without run watches files when a terminal is attached: hushrun observes `vitest run`"
                .into(),
        ));
    }
    Some(Ok(out))
}

/// The config file Vitest loads for `recognized` in `cwd`, when there is one.
pub fn config_file(recognized: &Recognized, cwd: &Path) -> Option<PathBuf> {
    if let Some(config) = &recognized.config {
        return Some(cwd.join(config));
    }
    let root = recognized
        .root
        .as_ref()
        .map_or_else(|| cwd.to_path_buf(), |r| cwd.join(r));
    CONFIG_NAMES
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
}

/// Why the adapter can't load for `recognized` in `cwd`: no config, or one without the line.
pub fn unloadable(recognized: &Recognized, cwd: &Path) -> Option<String> {
    let Some(config) = config_file(recognized, cwd) else {
        return Some(format!(
            "Vitest loads hushrun's adapter from one line in its config, and there is none here: `hushrun init` proposes vitest.config.ts with `{CONFIG_LINE}`"
        ));
    };
    let text = std::fs::read_to_string(&config).unwrap_or_default();
    if text.contains(VARIABLE) {
        return None;
    }
    let shown = config
        .strip_prefix(cwd)
        .unwrap_or(&config)
        .display()
        .to_string();
    Some(format!(
        "Vitest loads hushrun's adapter from one line in {shown}, which `hushrun init` proposes: `{CONFIG_LINE}`"
    ))
}

/// \[ADR 0071\] What `hushrun init` proposes, in its "Change by hand" list, for a repository at
/// `root` that runs Vitest, when `checks` look at their test files: the one line in Vitest's
/// config that loads hushrun's adapter. `None` when Vitest isn't here or the line is.
pub fn proposal(root: &Path, checks: &[String]) -> Option<String> {
    let package = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
    let config = CONFIG_NAMES
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file());
    let uses = package.contains("\"vitest\"")
        || config
            .as_ref()
            .is_some_and(|c| c.to_string_lossy().contains("vitest.config"));
    if !uses || checks.is_empty() {
        return None;
    }
    let names = checks.join(", ");
    match config {
        Some(config) => {
            let text = std::fs::read_to_string(&config).unwrap_or_default();
            if text.contains(VARIABLE) {
                return None;
            }
            let name = config.file_name()?.to_string_lossy().into_owned();
            Some(format!(
                "{name}: add `{CONFIG_LINE}`, so that hushrun can look at Vitest's test files for {names} (tests in hushrun.yaml); without HUSHRUN_VITEST, which hushrun sets, it adds nothing"
            ))
        }
        None => Some(format!(
            "vitest.config.ts: write `export default {{ {CONFIG_LINE} }}`, so that hushrun can look at Vitest's test files for {names} (tests in hushrun.yaml)"
        )),
    }
}

/// The directory that holds the plugin and its setup file for this binary, written whole when
/// they aren't there yet: `<cache dir>/adapters/vitest-<first 16 hex of their SHA-256>`.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(format!("{PLUGIN}\0{SETUP}").as_bytes()).hex();
    let dir = cache
        .join("adapters")
        .join(format!("vitest-{}", &digest[..16]));
    for (name, text) in [
        ("hushrun-vitest.mjs", PLUGIN),
        ("hushrun-vitest-setup.mjs", SETUP),
    ] {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
            super::facts::write_whole(&path, text.as_bytes())?;
        }
    }
    Ok(dir)
}

/// The variable that names the plugin, a `file:` URL, as `import()` takes one on every system.
pub fn variables(dir: &Path) -> Vec<(String, OsString)> {
    let path = dir.join("hushrun-vitest.mjs");
    let text = path.to_string_lossy().replace('\\', "/");
    let url = if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    };
    vec![(VARIABLE.to_string(), OsString::from(url))]
}

/// What the Vitest adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub units: BTreeMap<String, super::Seen>,
    pub failures_by_unit: bool,
    /// The unit rule: the test files Vitest listed, from the root.
    pub unit_files: BTreeSet<String>,
    /// The directories each unit listed, from the root.
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    /// Why the adapter deselected nothing, when the plan had listed something.
    pub refused: Option<String>,
    /// \[ADR 0072\] Which module imports which, from the root, for `hushrun why`.
    pub edges: BTreeSet<super::db::Edge>,
}

/// What the workers that ran one test file saw of it.
#[derive(Debug, Default)]
struct WorkerSaw {
    /// Files it ran or opened, from the root.
    files: BTreeSet<String>,
    /// Directories it listed, from the root.
    dirs: BTreeSet<String>,
    not_followed: BTreeSet<NotFollowed>,
    /// \[ADR 0081\] Its tests, by id, when every worker that ran it told them (C-43).
    tests: Option<BTreeMap<String, MergedTest>>,
    /// \[ADR 0081\] A worker that ran it told no tests: none of them can be told.
    untold: bool,
}

/// \[ADR 0081\] Adds what one worker told of a test file's tests to what others did. A file that
/// two projects run runs each of its tests in each: a test finished only where every worker that
/// told it finished it, failed where any failed it, was left out where every one left it out, and
/// what it read is known only where every one said it.
fn merge_tests(into: &mut BTreeMap<String, MergedTest>, worker: &FactsFile, tests: &[TestFacts]) {
    let list = |indices: &Option<Vec<usize>>| indices.as_ref().and_then(|i| worker.resolve(i));
    for test in tests {
        let told = MergedTest {
            finished: test.finished,
            failed: test.failed,
            deselected: test.deselected,
            duration_ms: test.duration_ms,
            not_followed: test.not_followed.iter().map(NotFollowed::from).collect(),
            ran: list(&test.ran),
            opened: list(&test.opened),
            dirs: list(&test.dirs),
            never_left_out: test.never_left_out,
            skipped: test.skipped,
        };
        let Some(had) = into.get_mut(&test.id) else {
            into.insert(test.id.clone(), told);
            continue;
        };
        had.finished &= told.finished;
        had.failed |= told.failed;
        had.deselected &= told.deselected;
        had.never_left_out |= told.never_left_out;
        had.duration_ms = had.duration_ms.saturating_add(told.duration_ms);
        had.not_followed.extend(told.not_followed);
        for (had, told) in [
            (&mut had.ran, told.ran),
            (&mut had.opened, told.opened),
            (&mut had.dirs, told.dirs),
        ] {
            *had = match (had.take(), told) {
                (Some(mut had), Some(told)) => {
                    had.extend(told);
                    Some(had)
                }
                _ => None,
            };
        }
    }
}

/// How one test file ended, by Vitest's main process, over the projects that ran it.
#[derive(Debug, Default)]
struct Ended {
    selected: u64,
    finished: u64,
    failed: bool,
    deselected: bool,
    duration_ms: u64,
    issues: BTreeSet<NotFollowed>,
}

/// A path from the repository root, `/`-separated, when `path` is in it.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let inside = real.strip_prefix(root).ok()?;
    let parts: Vec<String> = inside
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// What a module id a worker ran says: a file, by its path without a query; nothing, for Vite's
/// own helpers (`\0vite/…`); or a module some plugin made from no one file, `Err`.
fn module_path(id: &str) -> Result<Option<PathBuf>, String> {
    if id.starts_with("\0vite/") {
        return Ok(None);
    }
    let id = id.split('?').next().unwrap_or(id);
    let path = Path::new(id);
    if id.starts_with('\0') || id.starts_with("virtual:") || !path.is_absolute() {
        return Err(id.trim_start_matches('\0').chars().take(200).collect());
    }
    Ok(Some(path.to_path_buf()))
}

/// Reads what the Vitest adapter wrote in a run's directory, for the repository at `root` whose
/// files are `tree`. `Ok(None)` when it wrote nothing; `Err` says why nothing can be told.
pub fn observe(files: &[FactsFile], root: &Path, tree: &Tree) -> Result<Option<Observed>, String> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mains: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == ADAPTER).collect();
    let workers: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == WORKER).collect();
    let main = match mains.as_slice() {
        [] if workers.is_empty() => return Ok(None),
        [] => return Err("Vitest's run ended before hushrun's reporter wrote".into()),
        [main] => *main,
        _ => return Err("more than one Vitest wrote for this run".into()),
    };
    if let Some(error) = main.errors.first() {
        return Err(error.chars().take(300).collect());
    }
    let setting = |key: &str| -> Vec<PathBuf> {
        main.settings
            .get(key)
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(PathBuf::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let unit_files: BTreeSet<String> = setting("testFiles")
        .iter()
        .filter_map(|p| relative(&root, p))
        .collect();
    let config_files: BTreeSet<String> = setting("configFiles")
        .iter()
        .filter_map(|p| relative(&root, p))
        .collect();
    let whole: BTreeSet<NotFollowed> = main.not_followed.iter().map(Into::into).collect();
    // What each worker saw of the test file it ran.
    let mut seen_by: BTreeMap<String, WorkerSaw> = BTreeMap::new();
    for worker in &workers {
        for unit in &worker.units {
            // \[ADR 0081\] A worker that tells tests names its unit from the root.
            let path = Path::new(&unit.unit);
            let path = if path.is_relative() {
                root.join(path)
            } else {
                path.to_path_buf()
            };
            let Some(name) = relative(&root, &path) else {
                continue;
            };
            let entry = seen_by.entry(name).or_default();
            for id in &unit.reads {
                match module_path(id) {
                    Ok(Some(path)) => entry.files.extend(relative(&root, &path)),
                    Ok(None) => {}
                    Err(virtual_id) => {
                        entry.not_followed.insert(NotFollowed {
                            why: "virtual".into(),
                            detail: virtual_id,
                        });
                    }
                }
            }
            for dir in &unit.dirs {
                match relative(&root, Path::new(dir)) {
                    Some(dir) => {
                        entry.dirs.insert(dir);
                    }
                    // Listing the root itself reads every file.
                    None if std::fs::canonicalize(dir).is_ok_and(|d| d == root) => {
                        entry.dirs.insert(".".into());
                    }
                    None => {}
                }
            }
            entry
                .not_followed
                .extend(unit.not_followed.iter().map(NotFollowed::from));
            // \[ADR 0081\] Its tests, one by one, when the worker told them (C-43).
            match &unit.tests {
                Some(tests) if !entry.untold => {
                    merge_tests(entry.tests.get_or_insert_with(BTreeMap::new), worker, tests);
                }
                _ => {
                    entry.untold = true;
                    entry.tests = None;
                }
            }
            if let Some(error) = worker.errors.first() {
                entry.not_followed.insert(NotFollowed {
                    why: "adapter-error".into(),
                    detail: error.chars().take(200).collect(),
                });
            }
        }
    }
    let mut out = Observed {
        failures_by_unit: main.failures_by_unit,
        unit_files,
        refused: main
            .settings
            .get("refused")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        edges: main
            .edges
            .iter()
            .filter_map(|(from, to, kind)| {
                Some((
                    relative(&root, Path::new(from))?,
                    relative(&root, Path::new(to))?,
                    super::db::EdgeKind::from_word(kind)?,
                ))
            })
            .collect(),
        ..Observed::default()
    };
    // The main process's word on how each test file ended, merged over the projects that ran it.
    let mut ended: BTreeMap<String, Ended> = BTreeMap::new();
    for unit in &main.units {
        let Some(name) = relative(&root, Path::new(&unit.unit)) else {
            continue;
        };
        let entry = ended.entry(name).or_insert_with(|| Ended {
            deselected: true,
            ..Ended::default()
        });
        entry.selected = entry.selected.saturating_add(unit.selected);
        entry.finished = entry.finished.saturating_add(unit.finished);
        entry.failed |= unit.failed;
        entry.deselected &= unit.deselected;
        entry.duration_ms = entry.duration_ms.saturating_add(unit.duration_ms);
        entry
            .issues
            .extend(unit.not_followed.iter().map(|u: &Unfollowed| u.into()));
    }
    for (unit, end) in ended {
        let Ended {
            selected,
            finished,
            failed,
            deselected,
            duration_ms,
            issues,
        } = end;
        let mut not_followed = whole.clone();
        not_followed.extend(issues);
        let mut reads: BTreeSet<String> = config_files.clone();
        reads.insert(unit.clone());
        let mut dirs = BTreeSet::new();
        let mut tests = None;
        match seen_by.remove(&unit) {
            Some(saw) => {
                reads.extend(saw.files);
                not_followed.extend(saw.not_followed);
                dirs = saw.dirs;
                tests = saw.tests;
            }
            None if !deselected => {
                not_followed.insert(NotFollowed {
                    why: "unseen".into(),
                    detail: "hushrun's setup file didn't see it run".into(),
                });
            }
            None => {}
        }
        reads.extend(super::gotest::under(tree, &dirs));
        out.dirs.insert(unit.clone(), dirs);
        out.units.insert(
            unit,
            super::Seen {
                facts: Facts {
                    complete: finished >= selected,
                    failed,
                    not_followed,
                    deselected,
                    ..Facts::default()
                },
                reads,
                duration_ms,
                tests,
            },
        );
    }
    Ok(Some(out))
}

/// \[ADR 0071\] The Vitest adapter for a COMMAND that starts one run of Vitest, whose config
/// loads it (C-35); `None` for any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    let recognized = match recognize(command)? {
        Ok(recognized) => recognized,
        Err(why) => return Some(Err(why)),
    };
    if let Some(why) = unloadable(&recognized, cwd) {
        return Some(Err(why));
    }
    Some(Ok(Box::new(recognized)))
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "Vitest"
    }

    fn hints(&self) -> Hints {
        Hints::Kinds(SOURCE_KINDS)
    }

    /// \[ADR 0081\] Its setup file names each test, leaves out those `reuseTests` lists, says how
    /// each ended, and watches what each read (C-43).
    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            reads: true,
        }
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        install(at.cache)
            .map(|dir| variables(&dir))
            .map_err(|err| format!("can't write its Vitest plugin: {err}"))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        if self.coverage {
            return Err(
                "Vitest's coverage is on, and a partial run's coverage would read as a drop".into(),
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
            return Err("no run here has listed Vitest's test files yet".into());
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
                let lists =
                    super::shared::lists(unit, at.map.units.get(unit), &shared, at.tree, None);
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
        let Some(observed) = observe(at.files, at.root, at.tree)? else {
            return Ok(None);
        };
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
                let lists =
                    super::shared::lists(unit, at.map.units.get(unit), &shared, at.tree, None)?;
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
            "{unit} isn't a test file of {check} as far as this machine knows: Vitest didn't list it in its last run here"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("Vitest's list of test files is the one of its last run here")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-32
    #[test]
    fn one_run_of_vitest_is_recognized_however_it_starts() {
        for command in [
            "vitest run",
            "vitest --run",
            "npx vitest run --shard=1/2",
            "npx --no-install vitest run",
            "pnpm vitest run",
            "pnpm exec vitest run",
            "yarn exec vitest run",
            "bunx vitest run",
            "bun x vitest run",
            "npm exec -- vitest run",
            "node_modules/.bin/vitest run --pool=threads",
        ] {
            assert!(
                matches!(recognize(&words(command)), Some(Ok(_))),
                "{command}"
            );
        }
        let r = recognize(&words("vitest run -c conf/vitest.config.ts --coverage")).unwrap();
        let r = r.unwrap();
        assert_eq!(r.config.as_deref(), Some("conf/vitest.config.ts"));
        assert!(r.coverage);
        for watching in [
            "vitest",
            "vitest watch",
            "vitest run --watch",
            "vitest bench",
            "vitest run --typecheck",
            "vitest run --browser",
        ] {
            assert!(
                matches!(recognize(&words(watching)), Some(Err(_))),
                "{watching}"
            );
        }
        for other in [
            "npm test",
            "jest",
            "pnpm test",
            "npx jest",
            "node vitest.js run",
        ] {
            assert!(recognize(&words(other)).is_none(), "{other}");
        }
    }

    // covers: C-32
    #[test]
    fn a_config_without_the_line_loads_nothing_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let r = recognize(&words("vitest run")).unwrap().unwrap();
        assert!(
            unloadable(&r, dir.path())
                .unwrap()
                .contains("there is none here")
        );
        std::fs::write(
            dir.path().join("vite.config.ts"),
            "export default { test: {} }\n",
        )
        .unwrap();
        assert!(
            unloadable(&r, dir.path())
                .unwrap()
                .contains("one line in vite.config.ts")
        );
        // vitest.config.ts comes first, as Vitest looks for it first.
        std::fs::write(
            dir.path().join("vitest.config.ts"),
            format!("export default {{ {CONFIG_LINE} }}\n"),
        )
        .unwrap();
        assert!(unloadable(&r, dir.path()).is_none());
    }

    // covers: C-32
    #[test]
    fn a_module_some_plugin_made_is_not_a_file() {
        assert_eq!(
            module_path("/repo/src/a.ts?raw").unwrap(),
            Some(PathBuf::from("/repo/src/a.ts"))
        );
        assert_eq!(
            module_path("\0vite/dynamic-import-helper.js").unwrap(),
            None
        );
        assert!(module_path("\0virtual:routes").is_err());
        assert!(module_path("virtual:routes").is_err());
        assert!(module_path("src/relative.ts").is_err());
    }

    fn told(id: &str, finished: bool, failed: bool, ran: Option<Vec<usize>>) -> TestFacts {
        TestFacts {
            id: id.into(),
            finished,
            failed,
            ran,
            opened: Some(vec![]),
            dirs: Some(vec![]),
            ..TestFacts::default()
        }
    }

    // covers: C-43
    #[test]
    fn a_test_two_projects_ran_finished_only_where_both_finished_it() {
        let worker = |paths: &[&str]| FactsFile {
            paths: paths.iter().map(|p| p.to_string()).collect(),
            ..FactsFile::default()
        };
        let a = worker(&["test/a.test.ts", "src/x.ts"]);
        let b = worker(&["src/y.ts", "test/a.test.ts"]);
        let mut tests = BTreeMap::new();
        merge_tests(
            &mut tests,
            &a,
            &[
                told("t", true, false, Some(vec![0, 1])),
                told("u", true, false, Some(vec![0])),
                told("v", true, false, Some(vec![0])),
            ],
        );
        merge_tests(
            &mut tests,
            &b,
            &[
                told("t", true, false, Some(vec![0, 1])),
                told("u", false, false, Some(vec![1])),
                told("v", true, true, None),
            ],
        );
        // Each worker's indices are its own table's.
        assert_eq!(
            tests["t"].ran,
            Some(BTreeSet::from([
                "test/a.test.ts".to_string(),
                "src/x.ts".to_string(),
                "src/y.ts".to_string()
            ]))
        );
        assert!(tests["t"].finished && !tests["t"].failed);
        // Unfinished in one project: unfinished.
        assert!(!tests["u"].finished);
        // Failed in one: failed; and not watched alone in one: not watched.
        assert!(tests["v"].failed);
        assert_eq!(tests["v"].ran, None);
    }
}
