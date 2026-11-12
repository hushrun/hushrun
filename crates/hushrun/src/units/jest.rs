//! \[ADR 0082\] The Jest adapter, observed (C-30, C-32, C-39, C-44): a unit is a test file, as
//! Jest's own matcher names it in every project; what it read is what Jest compiled and what it
//! opened while it ran, with the files every test file reads; and each test is told on its own,
//! with what it ran and opened when the check asks for `reads: test`.
//!
//! - Loading: a file embedded in this binary, written to `<cache dir>/adapters/`, which every Node
//!   process of COMMAND loads through `NODE_OPTIONS=--require`, added to what the environment
//!   had: no file of the repository changes. A script that replaces `NODE_OPTIONS` loses it; one
//!   line at the top of the Jest config, which `hushrun init` proposes, loads it from
//!   `HUSHRUN_JEST` instead ([`CJS_LINE`], [`ESM_LINE`]).
//! - The unit rule: the test files the adapter listed with Jest's own `SearchSource` in the run
//!   that wrote the check's map, as Vitest's (ADR 0071).
//! - Selection: whole test files in Jest's sequencer, after its filters and `--shard`, before any
//!   worker starts; tests at jest-circus's `run_start`, before any of their hooks.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::units::{self, Facts, NotFollowed, Set, Sets, Sources, Tree};

use super::adapter::{
    Adapter, Hints, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::command::program_name;
use super::facts::FactsFile;

/// The adapter's word: its workers' facts files, this machine's store and a hint name it so.
pub const ADAPTER: &str = "jest";
/// The name in the facts file of the process that claimed the run.
pub const MAIN: &str = "jest-main";
/// The kinds of code the adapter observes being read.
pub const CODE_KINDS: &[&str] = &[".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"];
/// What the adapter observes being read: its sources (C-30). A snapshot is read by its test file
/// alone, through `fs`, so a change to one runs only that test file. Every other kind is rest.
pub const SOURCE_KINDS: &[&str] = &[
    ".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx", ".snap",
];
/// The variable the config line loads the adapter from.
pub const VARIABLE: &str = "HUSHRUN_JEST";
/// The line a CommonJS Jest config needs at its top, where a script replaces `NODE_OPTIONS`.
pub const CJS_LINE: &str = "process.env.HUSHRUN_JEST && require(process.env.HUSHRUN_JEST);";
/// The line an ES module Jest config needs at its top.
pub const ESM_LINE: &str = "import { createRequire } from 'node:module'; if (process.env.HUSHRUN_JEST) createRequire(import.meta.url)(process.env.HUSHRUN_JEST);";
/// The file every Node process of COMMAND loads.
const PRELOAD: &str = include_str!("hushrun_jest.cjs");
/// The reporter it adds to Jest's.
const REPORTER: &str = include_str!("hushrun_jest_reporter.cjs");
/// The config files Jest looks for, in the order it does.
const CONFIG_NAMES: &[&str] = &[
    "jest.config.js",
    "jest.config.ts",
    "jest.config.mjs",
    "jest.config.cjs",
    "jest.config.mts",
    "jest.config.cts",
    "jest.config.json",
];
/// The Jest majors the adapter knows.
const MAJORS: &[u64] = &[29, 30];

/// COMMAND, when it starts one run of Jest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// `--config`'s file, as COMMAND names it.
    pub config: Option<String>,
    /// COMMAND turns coverage on.
    pub coverage: bool,
    /// The Jest installed where COMMAND runs, when hushrun can tell.
    pub version: Option<String>,
}

/// Programs that start the program after them, with the options of theirs that come first.
const LAUNCHERS: &[(&str, &[&str])] = &[
    (
        "npx",
        &["--no-install", "--no", "-y", "--yes", "--quiet", "-q"],
    ),
    ("bunx", &[]),
    ("pnpx", &[]),
];

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

/// Whether `word` names Jest's own program: `jest`, a path to it, or `jest/bin/jest.js`.
fn is_jest(word: &str) -> bool {
    let name = program_name(word);
    let name = &*name;
    if name == "jest" {
        return true;
    }
    let path = word.replace('\\', "/");
    name == "jest.js" && path.ends_with("jest/bin/jest.js")
}

/// What `command` is for the Jest adapter: `None` when it doesn't start Jest; `Some(Err(why))`
/// when it does, in a way hushrun doesn't observe, which the run says.
pub fn recognize(command: &[String]) -> Option<Result<Recognized, String>> {
    let mut rest = command;
    loop {
        let first = rest.first()?;
        if is_jest(first) {
            break;
        }
        match &*program_name(first) {
            "node" | "nodejs" => {
                // `node [options] node_modules/jest/bin/jest.js`, the way to Jest's ESM mode.
                let mut next = &rest[1..];
                while let Some(word) = next.first() {
                    if !word.starts_with('-') {
                        break;
                    }
                    let takes = NODE_VALUES.contains(&word.as_str());
                    next = &next[if takes { 2.min(next.len()) } else { 1 }..];
                }
                if !next.first().is_some_and(|w| is_jest(w)) {
                    return None;
                }
                rest = next;
            }
            "pnpm" | "yarn" | "bun" | "npm" => {
                // `pnpm jest`, `pnpm exec jest`, `yarn jest`, `bun x jest`, `npm exec -- jest`.
                let manager = program_name(first);
                let manager = &*manager;
                let mut next = &rest[1..];
                if manager == "bun" && next.first().map(String::as_str) == Some("--bun") {
                    return Some(Err("Jest under Bun's own runtime doesn't load Node's options, which hushrun's adapter needs".into()));
                }
                if matches!(next.first().map(String::as_str), Some("exec" | "x" | "dlx")) {
                    next = &next[1..];
                } else if manager == "npm" || manager == "bun" {
                    return None;
                }
                if next.first().map(String::as_str) == Some("--") {
                    next = &next[1..];
                }
                if manager == "bun" && next.first().map(String::as_str) == Some("--bun") {
                    return Some(Err("Jest under Bun's own runtime doesn't load Node's options, which hushrun's adapter needs".into()));
                }
                if !next.first().is_some_and(|w| is_jest(w)) {
                    return None;
                }
                rest = next;
            }
            other => {
                let (_, options) = LAUNCHERS.iter().find(|(n, _)| *n == other)?;
                let mut next = &rest[1..];
                if other == "bunx" && next.first().map(String::as_str) == Some("--bun") {
                    return Some(Err("Jest under Bun's own runtime doesn't load Node's options, which hushrun's adapter needs".into()));
                }
                while next.first().is_some_and(|w| options.contains(&w.as_str())) {
                    next = &next[1..];
                }
                if !next.first().is_some_and(|w| is_jest(w)) {
                    return None;
                }
                rest = next;
            }
        }
    }
    let mut out = Recognized {
        config: None,
        coverage: false,
        version: None,
    };
    let mut words = rest[1..].iter();
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value.to_string())),
            _ => (word.as_str(), None),
        };
        let off = inline.as_deref() == Some("false");
        match flag {
            "--watch" | "--watchAll" if !off => {
                return Some(Err(format!("jest {flag} reruns as files change")));
            }
            "--onlyFailures" | "-f" if !off => {
                return Some(Err(
                    "jest --onlyFailures chooses test files from an earlier run".into(),
                ));
            }
            "--onlyChanged" | "-o" | "--changedSince" | "--lastCommit" if !off => {
                return Some(Err(format!("jest {flag} chooses test files from git")));
            }
            "--listTests" | "--showConfig" | "--clearCache" | "--init" | "--version" | "-v"
            | "--help" | "-h" => {
                return Some(Err(format!("jest {flag} isn't one run of the test files")));
            }
            "--testRunner" => {
                let runner = inline.or_else(|| words.next().cloned()).unwrap_or_default();
                if !runner.contains("circus") {
                    return Some(Err(format!(
                        "jest --testRunner {runner}: only jest-circus tells hushrun each test file's tests"
                    )));
                }
            }
            "--coverage" | "--collectCoverage" => out.coverage = !off,
            "--config" | "-c" => {
                out.config = inline.or_else(|| words.next().cloned());
            }
            _ => {}
        }
    }
    Some(Ok(out))
}

/// The Jest installed for a run in `cwd`: the version in the first `node_modules/jest/package.json`
/// from `cwd` up.
pub fn installed(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(here) = dir {
        let package = here.join("node_modules/jest/package.json");
        if let Ok(bytes) = std::fs::read(&package) {
            let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            return value["version"].as_str().map(str::to_string);
        }
        dir = here.parent();
    }
    None
}

/// `(major, minor)` of a version.
fn major_minor(version: &str) -> Option<(u64, u64)> {
    let mut parts = version.split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The config file Jest loads for `recognized` in `cwd`, when there is one in a file of its own.
pub fn config_file(recognized: &Recognized, cwd: &Path) -> Option<PathBuf> {
    if let Some(config) = &recognized.config {
        let path = cwd.join(config);
        return path.is_file().then_some(path);
    }
    let mut dir = Some(cwd);
    while let Some(here) = dir {
        if let Some(found) = CONFIG_NAMES
            .iter()
            .map(|name| here.join(name))
            .find(|path| path.is_file())
        {
            return Some(found);
        }
        // Jest stops at the first directory with a package.json.
        if here.join("package.json").is_file() {
            return None;
        }
        dir = here.parent();
    }
    None
}

/// The line `config` needs: the ES module form for `.mjs` and `.mts`, and for `.js` and `.ts`
/// where the nearest package.json says `"type": "module"`; else the CommonJS form.
pub fn line_for(config: &Path) -> &'static str {
    let name = config.to_string_lossy();
    if name.ends_with(".mjs") || name.ends_with(".mts") {
        return ESM_LINE;
    }
    if name.ends_with(".cjs") || name.ends_with(".cts") {
        return CJS_LINE;
    }
    let mut dir = config.parent();
    while let Some(here) = dir {
        if let Ok(text) = std::fs::read_to_string(here.join("package.json")) {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            return if value["type"] == "module" {
                ESM_LINE
            } else {
                CJS_LINE
            };
        }
        dir = here.parent();
    }
    CJS_LINE
}

/// What the run says when Jest wrote nothing for hushrun: why, and the fix.
fn nothing_written(recognized: &Recognized, cwd: &Path) -> String {
    let config = config_file(recognized, cwd);
    if let Some(config) = &config
        && std::fs::read_to_string(config).is_ok_and(|t| t.contains(VARIABLE))
    {
        return "Jest wrote nothing for hushrun, though its config loads hushrun's adapter: it stopped before it read its config".into();
    }
    let fix = match &config {
        Some(config) => format!(
            "or put this line first in {}: `{}`",
            config
                .strip_prefix(cwd)
                .unwrap_or(config)
                .display()
                .to_string()
                .replace('\\', "/"),
            line_for(config)
        ),
        None => "or give Jest a config file whose first line is `process.env.HUSHRUN_JEST && require(process.env.HUSHRUN_JEST);`".into(),
    };
    format!(
        "Jest wrote nothing for hushrun: its processes didn't load hushrun's adapter, as when a script sets NODE_OPTIONS instead of adding to it (`NODE_OPTIONS=--experimental-vm-modules jest`); keep what it had (`NODE_OPTIONS=\"$NODE_OPTIONS --experimental-vm-modules\" jest`), {fix}"
    )
}

/// Whether a script's text sets `NODE_OPTIONS` without what it had: the usual way to run Jest's
/// ES module mode, which drops hushrun's adapter.
pub fn replaces_node_options(script: &str) -> bool {
    let mut rest = script;
    while let Some(at) = rest.find("NODE_OPTIONS=") {
        let value = rest[at + "NODE_OPTIONS=".len()..].trim_start_matches(['"', '\'']);
        if !value.starts_with("$NODE_OPTIONS")
            && !value.starts_with("${NODE_OPTIONS")
            && !value.starts_with("%NODE_OPTIONS%")
        {
            return true;
        }
        rest = &rest[at + 1..];
    }
    false
}

/// \[ADR 0082\] What `hushrun init` proposes, in its "Change by hand" list, for a repository at
/// `root` that runs Jest, when `checks` look at their test files: where a script that runs Jest
/// replaces `NODE_OPTIONS`, the one line at the top of Jest's config that loads hushrun's adapter
/// anyway, or, with no config file of its own, to keep what `NODE_OPTIONS` had. `None` when
/// Jest isn't here, no script replaces it, or the line is there.
pub fn proposal(root: &Path, checks: &[String]) -> Option<String> {
    if checks.is_empty() {
        return None;
    }
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let package: serde_json::Value = serde_json::from_str(&text).ok()?;
    let uses = ["dependencies", "devDependencies"]
        .iter()
        .any(|k| package[k].get("jest").is_some());
    let scripts = package["scripts"].as_object()?;
    let replacing: Vec<&String> = scripts
        .iter()
        .filter(|(_, v)| {
            v.as_str()
                .is_some_and(|s| s.split_whitespace().any(is_jest) && replaces_node_options(s))
        })
        .map(|(k, _)| k)
        .collect();
    if !uses || replacing.is_empty() {
        return None;
    }
    let names = checks.join(", ");
    let script = replacing[0];
    let found = CONFIG_NAMES
        .iter()
        .filter(|n| !n.ends_with(".json"))
        .map(|name| root.join(name))
        .find(|path| path.is_file());
    match found {
        Some(config) => {
            if std::fs::read_to_string(&config).is_ok_and(|t| t.contains(VARIABLE)) {
                return None;
            }
            let name = config.file_name()?.to_string_lossy().into_owned();
            Some(format!(
                "{name}: add `{}` as its first line, so that hushrun can look at Jest's test files for {names} (tests in hushrun.yaml): the script {script} sets NODE_OPTIONS, which drops hushrun's adapter; without HUSHRUN_JEST, which hushrun sets, it does nothing",
                line_for(&config)
            ))
        }
        None => Some(format!(
            "package.json: in the script {script}, keep what NODE_OPTIONS had (`NODE_OPTIONS=\"$NODE_OPTIONS …\"`), so that hushrun can look at Jest's test files for {names} (tests in hushrun.yaml)"
        )),
    }
}

/// The directory that holds the adapter and its reporter for this binary, written whole when they
/// aren't there yet: `<cache dir>/adapters/jest-<first 16 hex of their SHA-256>`.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(format!("{PRELOAD}\0{REPORTER}").as_bytes()).hex();
    let dir = cache
        .join("adapters")
        .join(format!("jest-{}", &digest[..16]));
    for (name, text) in [
        ("hushrun-jest.cjs", PRELOAD),
        ("hushrun-jest-reporter.cjs", REPORTER),
    ] {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
            super::facts::write_whole(&path, text.as_bytes())?;
        }
    }
    Ok(dir)
}

/// The variables that load the adapter from `dir`: `NODE_OPTIONS` with `--require <file>` added
/// to what COMMAND's environment had, and `HUSHRUN_JEST` naming the file for the config line.
pub fn variables(
    dir: &Path,
    node_options: Option<OsString>,
) -> Result<Vec<(String, OsString)>, String> {
    let file = dir.join("hushrun-jest.cjs");
    let text = file.to_string_lossy().into_owned();
    if text.contains('"') {
        return Err(format!(
            "its file's path, {text}, has a quote, which NODE_OPTIONS can't hold"
        ));
    }
    // Node splits NODE_OPTIONS at spaces outside double quotes, where a backslash escapes.
    let word = if text.chars().any(char::is_whitespace) {
        format!("\"{}\"", text.replace('\\', "\\\\"))
    } else {
        text
    };
    let mut options = node_options.filter(|o| !o.is_empty()).unwrap_or_default();
    if !options.is_empty() {
        options.push(" ");
    }
    options.push(format!("--require {word}"));
    Ok(vec![
        ("NODE_OPTIONS".to_string(), options),
        (VARIABLE.to_string(), file.into_os_string()),
    ])
}

/// What the Jest adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub units: BTreeMap<String, super::Seen>,
    pub failures_by_unit: bool,
    /// The unit rule: the test files Jest listed, from the root.
    pub unit_files: BTreeSet<String>,
    /// The directories each unit listed, from the root.
    pub dirs: BTreeMap<String, BTreeSet<String>>,
    /// Why the adapter deselected nothing, when the plan had listed something.
    pub refused: Option<String>,
    /// \[ADR 0072\] Which module imports which, for `hushrun why`.
    pub edges: BTreeSet<super::db::Edge>,
}

/// How one test file ended, by Jest's main process, over the projects that ran it.
#[derive(Debug, Default)]
struct Ended {
    runs: usize,
    selected: u64,
    finished: u64,
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

/// Whether `path` is a config every test file reads, by its name: Jest's, Babel's, a preset.
fn read_by_all(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    [
        "jest.config.",
        "babel.config.",
        ".babelrc.",
        "jest-preset.",
        "jest.preset.",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
        && CODE_KINDS.iter().any(|kind| name.ends_with(kind))
}

/// Reads what the Jest adapter wrote in a run's directory, for the repository at `root` whose
/// files are `tree`, with `config` the config file COMMAND names, from the root. `Err` says why
/// nothing can be told: Jest wrote nothing (`nothing`), stopped before its main process wrote, or
/// the adapter itself failed there.
pub fn observe(
    files: &[FactsFile],
    tree: &Tree,
    config: Option<&str>,
    nothing: &dyn Fn() -> String,
) -> Result<Observed, String> {
    let mains: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == MAIN).collect();
    let workers: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == ADAPTER).collect();
    let main = match mains.as_slice() {
        [] if workers.is_empty() => return Err(nothing()),
        [] => return Err("Jest's run ended before hushrun's reporter wrote".into()),
        [main] => *main,
        _ => return Err("more than one Jest wrote for this run".into()),
    };
    if let Some(error) = main.errors.first() {
        return Err(error.chars().take(300).collect());
    }
    let unit_files: BTreeSet<String> = strings(main, "testFiles").into_iter().collect();
    // What every test file reads: the config files Node compiled in the main process, what its
    // globalSetup read, and Jest's and the transformers' configs by name.
    let mut every: BTreeSet<String> = strings(main, "configFiles").into_iter().collect();
    every.extend(tree.paths().filter(|p| read_by_all(p)).map(str::to_string));
    every.extend(config.map(str::to_string));
    let global_dirs: BTreeSet<String> = strings(main, "dirs").into_iter().collect();
    let whole: BTreeSet<NotFollowed> = main.not_followed.iter().map(Into::into).collect();
    let code: BTreeSet<String> = tree
        .paths()
        .filter(|p| CODE_KINDS.iter().any(|k| p.ends_with(k)))
        .map(str::to_string)
        .collect();
    // What the main process said of each test file, merged over the projects that ran it.
    let mut ended: BTreeMap<String, Ended> = BTreeMap::new();
    for unit in &main.units {
        let entry = ended.entry(unit.unit.clone()).or_insert_with(|| Ended {
            deselected: true,
            ..Ended::default()
        });
        entry.runs += usize::from(!unit.deselected);
        entry.selected = entry.selected.saturating_add(unit.selected);
        entry.finished = entry.finished.saturating_add(unit.finished);
        entry.failed |= unit.failed;
        entry.deselected &= unit.deselected;
        entry.duration_ms = entry.duration_ms.saturating_add(unit.duration_ms);
    }
    // What the workers said: each test file's runs, the directories it listed, and whether a
    // type checker read every file for it.
    let mut sessions: BTreeMap<String, usize> = BTreeMap::new();
    let mut listed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut type_aware: BTreeSet<String> = BTreeSet::new();
    // \[C-32\] Each process of Jest runs a test file whole: one start of Jest that runs a file in
    // two projects ran it whole only when each of them did, which a sum within the start can't say.
    let mut whole_in_each: BTreeMap<String, bool> = BTreeMap::new();
    let mut failures_by_unit = main.failures_by_unit;
    for worker in &workers {
        failures_by_unit &= worker.failures_by_unit;
        let aware = worker.settings.get("typeAware").and_then(|v| v.as_bool()) == Some(true);
        for unit in &worker.units {
            *sessions.entry(unit.unit.clone()).or_default() += 1;
            *whole_in_each.entry(unit.unit.clone()).or_insert(true) &=
                unit.deselected || unit.finished >= unit.selected;
            listed
                .entry(unit.unit.clone())
                .or_default()
                .extend(unit.dirs.iter().cloned());
            if aware {
                type_aware.insert(unit.unit.clone());
            }
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
        let mut reads: BTreeSet<String> = every.clone();
        reads.insert(unit.clone());
        let mut dirs = listed.remove(&unit).unwrap_or_default();
        dirs.extend(global_dirs.iter().cloned());
        let ran = end.as_ref().map_or(0, |e| e.runs);
        let complete = match (&end, &seen) {
            (_, _) if deselected => true,
            (Some(_), Some(seen)) => {
                seen.complete && whole_in_each.get(&unit).copied().unwrap_or(true)
            }
            // Jest ran it, and no worker said what it read.
            (Some(end), None) => end.finished >= end.selected,
            // A worker ran it, and Jest's main process never heard how it ended.
            (None, _) => false,
        };
        if !deselected && sessions.get(&unit).copied().unwrap_or(0) < ran {
            not_followed.insert(NotFollowed {
                why: "unseen".into(),
                detail: "hushrun's adapter didn't see it run".into(),
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
        // \[ADR 0082\] ts-jest checking types read every file of code, for every test file.
        if type_aware.contains(&unit) {
            reads.extend(code.iter().cloned());
        }
        reads.extend(super::gotest::under(tree, &dirs));
        reads.retain(|p| tree.contains(p));
        out.dirs.insert(unit.clone(), dirs);
        out.units.insert(
            unit,
            super::Seen {
                facts: Facts {
                    complete,
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

/// \[ADR 0082\] The Jest adapter for a COMMAND that starts one run of Jest 29 or 30; `None` for
/// any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    let mut recognized = match recognize(command)? {
        Ok(recognized) => recognized,
        Err(why) => return Some(Err(why)),
    };
    recognized.version = installed(cwd);
    if let Some(version) = &recognized.version
        && !major_minor(version).is_some_and(|(major, _)| MAJORS.contains(&major))
    {
        return Some(Err(format!(
            "this Jest, {version}, isn't one hushrun's adapter knows (29, 30)"
        )));
    }
    Some(Ok(Box::new(recognized)))
}

impl Recognized {
    /// `--config`'s file from the repository root, when it is in it.
    fn config_from_root(&self, root: &Path, cwd: &Path) -> Option<String> {
        let config = cwd.join(self.config.as_ref()?);
        let real = std::fs::canonicalize(&config).ok()?;
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let inside = real.strip_prefix(&root).ok()?;
        Some(inside.to_string_lossy().replace('\\', "/"))
    }
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        "Jest"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            // \[C-40\] Where a test can't be watched, the adapter's facts say why.
            reads: true,
        }
    }

    fn hints(&self) -> Hints {
        Hints::Kinds(SOURCE_KINDS)
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let dir =
            install(at.cache).map_err(|err| format!("can't write its Jest adapter: {err}"))?;
        variables(&dir, (at.env)("NODE_OPTIONS"))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        if self.coverage {
            return Err(
                "Jest's coverage is on, and a partial run's coverage would read as a drop".into(),
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
            return Err("no run here has listed Jest's test files yet".into());
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
        let config = self.config_from_root(at.root, at.cwd);
        let nothing = || nothing_written(self, at.cwd);
        let observed = observe(at.files, at.tree, config.as_deref(), &nothing)?;
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
            "{unit} isn't a test file of {check} as far as this machine knows: Jest didn't list it in its last run here"
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some("Jest's list of test files is the one of its last run here")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-44
    #[test]
    fn one_run_of_jest_is_recognized_however_it_starts() {
        for command in [
            "jest",
            "jest --ci --shard=1/2",
            "npx jest",
            "npx --no-install jest -i",
            "pnpm jest",
            "pnpm exec jest",
            "yarn jest",
            "yarn exec jest",
            "bunx jest",
            "bun x jest",
            "npm exec -- jest",
            "npm exec jest",
            "node_modules/.bin/jest --runInBand",
            "node node_modules/jest/bin/jest.js",
            "node --experimental-vm-modules node_modules/jest/bin/jest.js --ci",
            "node -r ./setup.js --max-old-space-size=4096 node_modules/.bin/jest",
            "jest --watch=false",
            "jest --testRunner=jest-circus/runner",
        ] {
            assert!(
                matches!(recognize(&words(command)), Some(Ok(_))),
                "{command}"
            );
        }
        let r = recognize(&words("jest -c conf/jest.config.js --coverage"))
            .unwrap()
            .unwrap();
        assert_eq!(r.config.as_deref(), Some("conf/jest.config.js"));
        assert!(r.coverage);
        for refused in [
            "jest --watch",
            "jest --watchAll",
            "jest --onlyFailures",
            "jest -f",
            "jest --onlyChanged",
            "jest -o",
            "jest --changedSince=main",
            "jest --lastCommit",
            "jest --listTests",
            "jest --showConfig",
            "jest --version",
            "jest --testRunner=jest-jasmine2",
            "jest --testRunner jasmine2",
            "bunx --bun jest",
            "bun x --bun jest",
        ] {
            assert!(
                matches!(recognize(&words(refused)), Some(Err(_))),
                "{refused}"
            );
        }
        for other in [
            "npm test",
            "npm run test",
            "pnpm test",
            "yarn test",
            "vitest run",
            "npx vitest run",
            "node jest-runner.js",
            "node index.js",
            "bun test",
        ] {
            assert!(recognize(&words(other)).is_none(), "{other}");
        }
    }

    // covers: C-44
    #[test]
    fn the_adapter_loads_beside_what_node_options_had() {
        let vars = variables(
            Path::new("/c/adapters/jest-1"),
            Some("--max-old-space-size=4096".into()),
        )
        .unwrap();
        assert_eq!(
            vars[0],
            (
                "NODE_OPTIONS".to_string(),
                OsString::from(
                    "--max-old-space-size=4096 --require /c/adapters/jest-1/hushrun-jest.cjs"
                )
            )
        );
        assert_eq!(
            vars[1],
            (
                "HUSHRUN_JEST".to_string(),
                OsString::from("/c/adapters/jest-1/hushrun-jest.cjs")
            )
        );
        let spaced = variables(Path::new("/my cache/jest-1"), None).unwrap();
        assert_eq!(
            spaced[0].1,
            OsString::from("--require \"/my cache/jest-1/hushrun-jest.cjs\"")
        );
        assert!(variables(Path::new("/a\"b"), None).is_err());
    }

    // covers: C-44
    #[test]
    fn jest_29_and_30_are_known_and_others_are_said() {
        assert_eq!(major_minor("30.0.0-beta.3"), Some((30, 0)));
        assert!(
            Recognized {
                config: None,
                coverage: false,
                version: None
            }
            .tests()
            .reads
        );
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("pkg");
        std::fs::create_dir_all(dir.path().join("node_modules/jest")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            dir.path().join("node_modules/jest/package.json"),
            r#"{"name": "jest", "version": "28.1.3"}"#,
        )
        .unwrap();
        assert_eq!(installed(&sub).as_deref(), Some("28.1.3"));
        match recognized(&words("jest"), &sub) {
            Some(Err(why)) => assert!(why.contains("28.1.3"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    // covers: C-44
    #[test]
    fn a_script_that_replaces_node_options_is_told_apart() {
        assert!(replaces_node_options(
            "NODE_OPTIONS=--experimental-vm-modules jest"
        ));
        assert!(replaces_node_options(
            "cross-env NODE_OPTIONS='--experimental-vm-modules' jest"
        ));
        assert!(!replaces_node_options(
            "NODE_OPTIONS=\"$NODE_OPTIONS --experimental-vm-modules\" jest"
        ));
        assert!(!replaces_node_options(
            "NODE_OPTIONS=${NODE_OPTIONS} --x jest"
        ));
        assert!(!replaces_node_options("jest --ci"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"type\": \"module\"}").unwrap();
        std::fs::write(dir.path().join("jest.config.js"), "export default {}\n").unwrap();
        assert_eq!(line_for(&dir.path().join("jest.config.js")), ESM_LINE);
        assert_eq!(line_for(&dir.path().join("jest.config.cjs")), CJS_LINE);
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(line_for(&dir.path().join("jest.config.js")), CJS_LINE);
        assert_eq!(line_for(&dir.path().join("jest.config.mjs")), ESM_LINE);
    }

    fn tree_of(paths: &[&str]) -> Tree {
        let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
        let mut paths = paths.to_vec();
        paths.sort();
        for path in paths {
            bytes.extend(format!("f {} {path}\0", "0".repeat(64)).as_bytes());
        }
        Tree::from_manifest(&bytes).unwrap()
    }

    fn facts(value: serde_json::Value) -> FactsFile {
        serde_json::from_value(value).unwrap()
    }

    // covers: C-44
    #[test]
    fn what_the_main_process_and_the_workers_wrote_comes_to_one_unit_each() {
        let tree = tree_of(&[
            "jest.config.js",
            "babel.config.js",
            "src/a.ts",
            "src/b.js",
            "src/data.json",
            "test/a.test.ts",
            "test/b.test.ts",
            "test/c.test.ts",
            "test/__snapshots__/a.test.ts.snap",
        ]);
        let nothing = || "nothing".to_string();
        assert_eq!(observe(&[], &tree, None, &nothing).unwrap_err(), "nothing");
        let main = facts(serde_json::json!({
            "schema": "hushrun.unit-facts/v1", "adapter": MAIN, "failuresByUnit": true,
            "settings": {"testFiles": ["test/a.test.ts", "test/b.test.ts", "test/c.test.ts"],
                         "configFiles": ["jest.config.js"], "refused": null},
            "units": [
                {"unit": "test/a.test.ts", "selected": 1, "finished": 1},
                {"unit": "test/b.test.ts", "selected": 1, "finished": 1, "failed": true},
                {"unit": "test/c.test.ts", "deselected": true},
            ],
        }));
        let worker = facts(serde_json::json!({
            "schema": "hushrun.unit-facts/v1", "adapter": ADAPTER, "failuresByUnit": true,
            "settings": {"typeAware": true},
            "units": [{"unit": "test/a.test.ts", "selected": 1, "finished": 1,
                       "reads": ["src/a.ts", "src/missing.ts", "test/__snapshots__/a.test.ts.snap"],
                       "tests": [{"id": "a", "finished": true}]}],
        }));
        // Workers without the main process: Jest stopped before its reporter wrote.
        assert!(observe(std::slice::from_ref(&worker), &tree, None, &nothing).is_err());
        assert!(observe(&[main.clone(), main.clone()], &tree, None, &nothing).is_err());
        let mut broken = main.clone();
        broken
            .errors
            .push("version: this Jest, 28.1.3, isn't one hushrun's adapter knows (29, 30)".into());
        assert!(
            observe(&[broken], &tree, None, &nothing)
                .unwrap_err()
                .contains("28.1.3")
        );
        let seen = observe(&[main, worker], &tree, Some("jest.config.js"), &nothing).unwrap();
        let a = &seen.units["test/a.test.ts"];
        assert!(a.facts.complete && !a.facts.failed && a.facts.not_followed.is_empty());
        // ts-jest checked types for it: every file of code, and the configs every test file reads;
        // a path that isn't a file of the scope is left out.
        assert_eq!(
            a.reads.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "babel.config.js",
                "jest.config.js",
                "src/a.ts",
                "src/b.js",
                "test/__snapshots__/a.test.ts.snap",
                "test/a.test.ts",
                "test/b.test.ts",
                "test/c.test.ts"
            ]
        );
        assert_eq!(a.tests.as_ref().unwrap().len(), 1);
        // Jest ran it and no worker said what it read: not observed, and its failure kept.
        let b = &seen.units["test/b.test.ts"];
        assert!(b.facts.failed);
        assert_eq!(b.facts.not_followed.iter().next().unwrap().why, "unseen");
        assert!(b.tests.is_none());
        assert!(seen.units["test/c.test.ts"].facts.deselected);
        assert_eq!(seen.unit_files.len(), 3);
    }

    // covers: C-32
    #[test]
    fn a_test_file_two_projects_of_one_start_ran_ran_whole_only_when_each_did() {
        let tree = tree_of(&["jest.config.js", "test/a.test.js"]);
        let nothing = || "nothing".to_string();
        let main = facts(serde_json::json!({
            "schema": "hushrun.unit-facts/v1", "adapter": MAIN, "failuresByUnit": true,
            "start": "s",
            "settings": {"testFiles": ["test/a.test.js"], "configFiles": [], "refused": null},
            "units": [
                {"unit": "test/a.test.js", "selected": 2, "finished": 2},
                {"unit": "test/a.test.js", "selected": 2, "finished": 2},
            ],
        }));
        let worker = |finished: u64, skipped: u64| {
            facts(serde_json::json!({
                "schema": "hushrun.unit-facts/v1", "adapter": ADAPTER, "failuresByUnit": true,
                "start": "s",
                "units": [{"unit": "test/a.test.js", "selected": 2, "finished": finished,
                           "skipped": skipped, "reads": []}],
            }))
        };
        // One project's process finished both tests, the other's one: summed within the start
        // that is two of two, but the file didn't run whole in the second.
        let seen = observe(
            &[main.clone(), worker(2, 0), worker(1, 0)],
            &tree,
            None,
            &nothing,
        )
        .unwrap();
        assert!(!seen.units["test/a.test.js"].facts.complete);
        let seen = observe(&[main, worker(2, 1), worker(2, 0)], &tree, None, &nothing).unwrap();
        let a = &seen.units["test/a.test.js"].facts;
        assert!(a.complete);
        // A skipped test in either: in a run that failed, the file records no pass.
        assert!(a.skipped);
    }
}
