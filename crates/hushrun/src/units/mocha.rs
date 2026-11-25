//! \[ADR 0107\] The Mocha adapter, observed (C-30, C-32, C-39, C-56): a unit is a spec file, as
//! Mocha collected it, but one that defines a root hook; what it read is what was read while it
//! loaded and while its tests and their hooks ran, with everything its modules load, to the end,
//! and what any of them opened; and each test is told on its own.
//!
//! - Loading: a file embedded in this binary, written to `<cache dir>/adapters/`, which every Node
//!   process of COMMAND loads through `NODE_OPTIONS=--require`, added to what the environment
//!   had: no file of the repository changes.
//! - The unit rule: the spec files the adapter listed in the run that wrote the check's map.
//! - Selection: every spec file loads; at the runner's start the tests `reuse` and `reuseTests`
//!   list are taken out of their suites, before any hook runs.

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

/// The adapter's word: its facts files, this machine's store and a hint name it so.
pub const ADAPTER: &str = "mocha";
/// What the adapter observes being read: its sources (C-30). Every other kind is rest.
pub const SOURCE_KINDS: &[&str] = super::node::SOURCE_KINDS;
/// The file every Node process of COMMAND loads.
const PRELOAD: &str = include_str!("hushrun_mocha.cjs");
/// The Mocha majors the adapter knows.
const MAJORS: &[u64] = &[10, 11];

/// COMMAND, when it starts one run of Mocha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// The Mocha installed where COMMAND runs, when hushrun can tell.
    pub version: Option<String>,
}

/// Whether `word` names Mocha's own program: `mocha`, `_mocha`, a path to either, or
/// `mocha/bin/mocha.js`.
fn is_mocha(word: &str) -> bool {
    let name = program_name(word);
    if name == "mocha" || name == "_mocha" {
        return true;
    }
    let path = word.replace('\\', "/");
    (name == "mocha.js" || name == "_mocha.js")
        && (path.ends_with("mocha/bin/mocha.js") || path.ends_with("mocha/bin/_mocha.js"))
}

/// Node's options that take the next word as their value.
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
];

/// What `command` is for the Mocha adapter: `None` when it doesn't start Mocha; `Some(Err(why))`
/// when it does, in a way hushrun doesn't observe, which the run says.
pub fn recognize(command: &[String]) -> Option<Result<Recognized, String>> {
    let mut rest = command;
    loop {
        let first = rest.first()?;
        if is_mocha(first) {
            break;
        }
        let mut next = &rest[1..];
        match &*program_name(first) {
            "node" | "nodejs" => {
                while let Some(word) = next.first() {
                    if !word.starts_with('-') {
                        break;
                    }
                    let takes = NODE_VALUES.contains(&word.as_str());
                    next = &next[if takes { 2.min(next.len()) } else { 1 }..];
                }
            }
            manager @ ("npx" | "pnpx" | "pnpm" | "yarn" | "npm") => {
                if matches!(next.first().map(String::as_str), Some("exec" | "dlx")) {
                    next = &next[1..];
                } else if manager == "npm" {
                    return None;
                }
                while next.first().is_some_and(|w| {
                    matches!(
                        w.as_str(),
                        "--" | "--no-install" | "--no" | "-y" | "--yes" | "-q" | "--quiet"
                    )
                }) {
                    next = &next[1..];
                }
            }
            _ => return None,
        }
        if !next.first().is_some_and(|w| is_mocha(w)) {
            return None;
        }
        rest = next;
    }
    for word in &rest[1..] {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value)),
            _ => (word.as_str(), None),
        };
        let off = inline == Some("false");
        match flag {
            "--watch" | "-w" if !off => {
                return Some(Err(format!("mocha {flag} reruns as files change")));
            }
            "--parallel" | "-p" if !off => {
                return Some(Err(format!(
                    "mocha {flag} runs spec files in worker processes, which hushrun's adapter doesn't observe"
                )));
            }
            "--dry-run" if !off => {
                return Some(Err("mocha --dry-run reports tests it doesn't run".into()));
            }
            "--list-interfaces" | "--list-reporters" | "--help" | "-h" | "--version" | "-V"
            | "init" => {
                return Some(Err(format!("mocha {flag} isn't one run of the spec files")));
            }
            _ => {}
        }
    }
    Some(Ok(Recognized { version: None }))
}

/// The Mocha installed for a run in `cwd`: the version in the first
/// `node_modules/mocha/package.json` from `cwd` up.
pub fn installed(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(here) = dir {
        if let Ok(bytes) = std::fs::read(here.join("node_modules/mocha/package.json")) {
            let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            return value["version"].as_str().map(str::to_string);
        }
        dir = here.parent();
    }
    None
}

/// The directory that holds the adapter for this binary, written whole when it isn't there yet:
/// `<cache dir>/adapters/mocha-<first 16 hex of its SHA-256>`.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(PRELOAD.as_bytes()).hex();
    let dir = cache
        .join("adapters")
        .join(format!("mocha-{}", &digest[..16]));
    let path = dir.join("hushrun-mocha.cjs");
    if std::fs::read(&path).ok().as_deref() != Some(PRELOAD.as_bytes()) {
        super::facts::write_whole(&path, PRELOAD.as_bytes())?;
    }
    Ok(dir)
}

/// What the Mocha adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub units: BTreeMap<String, super::Seen>,
    pub failures_by_unit: bool,
    /// The unit rule: the spec files Mocha listed, from the root, but those that define a root hook.
    pub unit_files: BTreeSet<String>,
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    pub refused: Option<String>,
    pub edges: BTreeSet<super::db::Edge>,
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

/// Whether `path` is a Mocha config of a code kind, which every spec file reads.
fn read_by_all(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.starts_with(".mocharc.") && SOURCE_KINDS.iter().any(|kind| name.ends_with(kind))
}

/// What the run says when Mocha wrote nothing for hushrun.
const NOTHING: &str = "Mocha wrote nothing for hushrun: its process didn't load hushrun's adapter, as when a script sets NODE_OPTIONS instead of adding to it; keep what it had (`NODE_OPTIONS=\"$NODE_OPTIONS …\"`)";

/// Reads what the Mocha adapter wrote in a run's directory, for the repository whose files are
/// `tree`. `Err` says why nothing can be told.
pub fn observe(files: &[FactsFile], tree: &Tree) -> Result<Observed, String> {
    let mine: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == ADAPTER).collect();
    let main = match mine.as_slice() {
        [] => return Err(NOTHING.into()),
        [main] => *main,
        _ => return Err("more than one Mocha wrote for this run".into()),
    };
    if let Some(error) = main.errors.first() {
        return Err(error.chars().take(300).collect());
    }
    let unit_files: BTreeSet<String> = strings(main, "testFiles").into_iter().collect();
    let every: BTreeSet<String> = tree
        .paths()
        .filter(|p| read_by_all(p))
        .map(str::to_string)
        .collect();
    let mut merged = super::facts::merged(files, ADAPTER);
    let mut out = Observed {
        failures_by_unit: main.failures_by_unit,
        unit_files,
        refused: main
            .settings
            .get("refused")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        edges: super::facts::edges_of(files, ADAPTER),
        ..Observed::default()
    };
    let listed: BTreeMap<String, BTreeSet<String>> = main
        .units
        .iter()
        .map(|u| (u.unit.clone(), u.dirs.iter().cloned().collect()))
        .collect();
    for unit in out.unit_files.clone() {
        let Some(seen) = merged.remove(&unit) else {
            continue;
        };
        let mut reads: BTreeSet<String> = every.clone();
        reads.insert(unit.clone());
        reads.extend(seen.reads);
        let dirs = listed.get(&unit).cloned().unwrap_or_default();
        reads.extend(super::shared::under(tree, &dirs));
        reads.retain(|p| tree.contains(p));
        let not_followed: BTreeSet<NotFollowed> = seen.not_followed;
        out.dirs.insert(unit.clone(), dirs);
        out.units.insert(
            unit,
            super::Seen {
                outside: Default::default(),
                declared: None,
                getenv: Default::default(),
                facts: Facts {
                    complete: seen.deselected || seen.complete,
                    failed: seen.failed,
                    not_followed,
                    deselected: seen.deselected,
                    skipped: seen.skipped > 0,
                    ..Facts::default()
                },
                reads,
                duration_ms: seen.duration_ms,
                tests: seen.tests,
            },
        );
    }
    Ok(out)
}

/// What `command` in `cwd` is for the Mocha adapter, as [`recognized`] finds it, without the
/// trait object: a test calls the adapter on its own type (CONTRIBUTING.md, "Which tests CI runs
/// again").
pub fn recognize_in(command: &[String], cwd: &Path) -> Option<Result<Recognized, String>> {
    let mut recognized = match recognize(command)? {
        Ok(recognized) => recognized,
        Err(why) => return Some(Err(why)),
    };
    recognized.version = installed(cwd);
    if let Some(version) = &recognized.version {
        let major = version
            .split('.')
            .next()
            .and_then(|m| m.parse::<u64>().ok());
        if !major.is_some_and(|m| MAJORS.contains(&m)) {
            return Some(Err(format!(
                "this Mocha, {version}, isn't one hushrun's adapter knows (10, 11)"
            )));
        }
    }
    Some(Ok(recognized))
}

/// \[ADR 0107\] The Mocha adapter for a COMMAND that starts one run of Mocha 10 or 11; `None`
/// for any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    Some(recognize_in(command, cwd)?.map(|r| Box::new(r) as Box<dyn Adapter>))
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "Mocha"
    }

    fn test_file(&self) -> &'static str {
        "spec file"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            // \[C-56\] What a test read is its spec file's: `reads: test` runs as `reads: file`.
            reads: false,
        }
    }

    fn hints(&self) -> Hints {
        Hints::Kinds(SOURCE_KINDS)
    }

    // `binds`, `keys`, `not_by_test`, `not_watched` and `start_variables` are the trait's
    // defaults, written out: a call on this type then names this impl's own method, which the
    // static test map follows to it alone, where the trait's would count every adapter that
    // overrides it (CONTRIBUTING.md, "Which tests CI runs again").
    fn binds(&self) -> bool {
        false
    }

    fn keys(&self) -> KeysOutside {
        KeysOutside::None
    }

    fn not_by_test(&self) -> Option<&'static str> {
        None
    }

    fn not_watched(&self) -> Option<&'static str> {
        None
    }

    fn start_variables(&self, _: &Path) -> Vec<(String, Option<OsString>)> {
        Vec::new()
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let dir =
            install(at.cache).map_err(|err| format!("can't write its Mocha adapter: {err}"))?;
        Ok(vec![(
            "NODE_OPTIONS".to_string(),
            super::node::require(&dir.join("hushrun-mocha.cjs"), (at.env)("NODE_OPTIONS"))?,
        )])
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
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
            return Err("no run here has listed Mocha's spec files yet".into());
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
            "{unit} isn't a spec file of {check} as far as this machine knows: Mocha didn't list it in its last run here, or it defines a root hook, which every spec file runs"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("Mocha's list of spec files is the one of its last run here")
    }
}

#[cfg(test)]
mod tests;
