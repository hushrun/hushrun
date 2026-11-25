//! \[ADR 0116\] The PHPUnit and Pest adapter, observed: two PHP files embedded in the binary,
//! written to hushrun's cache directory, and an ini file there that `PHP_INI_SCAN_DIR` names, so
//! that every PHP process of COMMAND runs the first file before its own code
//! (`auto_prepend_file`). In PHPUnit's or Pest's own program it adds hushrun's bootstrap, which
//! runs the one PHPUnit would have run; in any other it does nothing. The unit rule is PHPUnit's
//! own list of test files, as its last run here listed them (C-30, C-90).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::units::{self, Facts, NotFollowed, Set, Sets, Sources};

use super::adapter::{
    Adapter, Hints, KeysOutside, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt,
    WhyAt,
};
use super::command::program_name;
use super::facts::FactsFile;

/// The adapter's word (C-36).
pub const ADAPTER: &str = "phpunit";
/// The kinds of files the adapter observes being read: PHP's (C-30).
pub const SOURCE_KINDS: &[&str] = &[".php"];
/// The file every PHP process of COMMAND runs first.
const PREPEND: &str = include_str!("hushrun_phpunit_prepend.php");
/// The bootstrap that loads the adapter, then the one PHPUnit would have loaded.
const BOOTSTRAP: &str = include_str!("hushrun_phpunit.php");
/// The variable that names where PHP reads more ini files.
pub const SCAN_DIR: &str = "PHP_INI_SCAN_DIR";
/// \[C-91\] The prepend file PHP's configuration named before, which hushrun's runs first.
pub const PREPEND_VAR: &str = "HUSHRUN_PHP_PREPEND";
/// The PHPUnit majors the adapter knows, and Pest's.
const PHPUNIT_MAJORS: &[u64] = &[10, 11, 12];
const PEST_MAJORS: &[u64] = &[3, 4];

/// COMMAND, when it starts one run of PHPUnit or Pest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// It is Pest's program.
    pub pest: bool,
    /// COMMAND turns code coverage on.
    pub coverage: bool,
    /// The PHP program COMMAND names first, when it does (`php vendor/bin/phpunit`).
    pub php: Option<String>,
    /// The directory of the Composer `vendor` its program is in, when COMMAND names it by path.
    pub vendor: Option<PathBuf>,
}

/// Whether `word` names PHPUnit's program (`Some(false)`) or Pest's (`Some(true)`).
fn runner_of(word: &str) -> Option<bool> {
    match &*program_name(word) {
        "phpunit" => Some(false),
        "pest" => Some(true),
        _ => None,
    }
}

/// Whether `word` names PHP itself: `php`, `php8.3`, `php83`.
fn is_php(word: &str) -> bool {
    let name = program_name(word);
    name.strip_prefix("php")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// PHP's options that take the next word as their value.
const PHP_VALUES: &[&str] = &["-c", "-d", "-z"];

/// What `command`, run in `cwd`, is for the adapter: `None` when it doesn't start PHPUnit or
/// Pest; `Some(Err(why))` when it does, in a way hushrun doesn't observe.
pub fn recognize(command: &[String], cwd: &Path) -> Option<Result<Recognized, String>> {
    recognize_in(command, cwd, 0)
}

fn recognize_in(
    command: &[String],
    cwd: &Path,
    depth: usize,
) -> Option<Result<Recognized, String>> {
    let first = command.first()?;
    let mut php = None;
    let mut rest = command;
    if is_php(first) {
        php = Some(first.clone());
        let mut next = &command[1..];
        while let Some(word) = next.first() {
            if !word.starts_with('-') {
                break;
            }
            if word == "-n" || word == "--no-php-ini" {
                return Some(Err(
                    "php -n reads no ini file, and hushrun's adapter loads through one".into(),
                ));
            }
            let setting = word
                .strip_prefix("-d")
                .map(str::to_string)
                .filter(|s| !s.is_empty())
                .or_else(|| (word == "-d").then(|| next.get(1).cloned()).flatten());
            if setting
                .as_deref()
                .is_some_and(|s| s.trim_start().starts_with("auto_prepend_file"))
            {
                return Some(Err(
                    "php -d auto_prepend_file replaces the file hushrun's adapter loads by".into(),
                ));
            }
            let takes = PHP_VALUES.contains(&word.as_str());
            next = &next[if takes { 2.min(next.len()) } else { 1 }..];
        }
        rest = next;
    } else if program_name(first) == "composer" && depth == 0 {
        return composer_script(command, cwd);
    }
    let program = rest.first()?;
    let pest = runner_of(program)?;
    let word = if pest { "pest" } else { "phpunit" };
    let mut out = Recognized {
        pest,
        coverage: false,
        php,
        vendor: vendor_of(program, cwd),
    };
    for word_ in &rest[1..] {
        if word_ == "--" {
            break;
        }
        let flag = word_.split_once('=').map_or(word_.as_str(), |(f, _)| f);
        match flag {
            "--list-tests"
            | "--list-tests-xml"
            | "--list-suites"
            | "--list-groups"
            | "--list-test-files"
            | "--version"
            | "--help"
            | "-h"
            | "--check-version"
            | "--generate-configuration"
            | "--migrate-configuration"
            | "--generate-baseline"
            | "--warm-coverage-cache"
            | "--init"
            | "--atime" => {
                return Some(Err(format!(
                    "{word} {flag} isn't one run of the test files"
                )));
            }
            "--parallel" | "-p" | "--processes" if pest => {
                return Some(Err(format!(
                    "{word} {flag} runs its tests in workers hushrun's adapter doesn't follow"
                )));
            }
            "--watch" => return Some(Err(format!("{word} --watch reruns as files change"))),
            "--dirty" | "--diff" => {
                return Some(Err(format!("{word} {flag} chooses test files from git")));
            }
            "--coverage" | "--min" | "--path-coverage" | "--coverage-filter" => out.coverage = true,
            _ if flag.starts_with("--coverage-") => out.coverage = true,
            _ => {}
        }
    }
    Some(Ok(out))
}

/// \[C-91\] `composer test`, `composer run[-script] <name>` or `composer <name>`, when the script
/// of that name in `composer.json` runs PHPUnit or Pest: one command of it, the others none of
/// them starting another test run.
fn composer_script(command: &[String], cwd: &Path) -> Option<Result<Recognized, String>> {
    let mut dir = cwd.to_path_buf();
    let mut rest = &command[1..];
    let mut options = |rest: &mut &[String]| {
        while let Some(word) = rest.first().filter(|w| w.starts_with('-')) {
            // `-d <dir>` and `--working-dir`: where Composer reads composer.json.
            if word == "-d" || word == "--working-dir" {
                if let Some(value) = rest.get(1) {
                    dir = cwd.join(value);
                }
                *rest = &rest[2.min(rest.len())..];
                continue;
            }
            if let Some(value) = word.strip_prefix("--working-dir=") {
                dir = cwd.join(value);
            }
            *rest = &rest[1..];
        }
    };
    options(&mut rest);
    let mut name = rest.first()?.as_str();
    if matches!(name, "run" | "run-script") {
        rest = &rest[1..];
        options(&mut rest);
        name = rest.first()?.as_str();
    }
    let cwd = dir.as_path();
    let text = std::fs::read(cwd.join("composer.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_slice(&text).ok()?;
    let scripts = manifest.get("scripts")?;
    let lines = script_lines(scripts, name, 0)?;
    let mut found: Option<Result<Recognized, String>> = None;
    for line in &lines {
        // A script's own `NAME=value` words first set variables, as a shell's do.
        let words: Vec<String> = line
            .split_whitespace()
            .skip_while(|w| w.contains('=') && !w.starts_with('-') && !w.starts_with('='))
            .map(str::to_string)
            .collect();
        let words: Vec<String> = match words.first().map(String::as_str) {
            Some("@php") => std::iter::once("php".to_string())
                .chain(words[1..].iter().cloned())
                .collect(),
            _ => words,
        };
        if let Some(this) = recognize_in(&words, cwd, 1) {
            if found.is_some() {
                return Some(Err(format!(
                    "composer {name} runs more than one test run, which hushrun's adapter tells apart from none"
                )));
            }
            found = Some(this);
        }
    }
    found
}

/// The command lines of Composer's script `name`, `@other` references followed (to a depth of 8),
/// or `None` when it has none or names a PHP callback.
fn script_lines(scripts: &serde_json::Value, name: &str, depth: usize) -> Option<Vec<String>> {
    if depth > 8 {
        return None;
    }
    let entry = scripts.get(name)?;
    let items: Vec<&str> = match entry {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(items) => items.iter().filter_map(|v| v.as_str()).collect(),
        _ => return None,
    };
    let mut out = Vec::new();
    for item in items {
        let item = item.trim();
        if let Some(other) = item.strip_prefix('@')
            && !other.starts_with("php ")
            && !other.starts_with("composer ")
            && !other.starts_with("putenv ")
        {
            out.extend(script_lines(
                scripts,
                other.split_whitespace().next()?,
                depth + 1,
            )?);
        } else {
            out.push(item.to_string());
        }
    }
    Some(out)
}

/// The `vendor` directory a program named by path is in (`vendor/bin/phpunit`).
fn vendor_of(program: &str, cwd: &Path) -> Option<PathBuf> {
    let path = program.replace('\\', "/");
    let at = path.rfind("vendor/")?;
    Some(cwd.join(&path[..at + "vendor".len()]))
}

/// The version Composer installed of `package`, for a run in `cwd`: from `vendor`, else from the
/// first `vendor/composer/installed.json` from `cwd` up.
pub fn installed(package: &str, cwd: &Path, vendor: Option<&Path>) -> Option<String> {
    let mut candidates: Vec<PathBuf> = vendor.map(Path::to_path_buf).into_iter().collect();
    let mut dir = Some(cwd);
    while let Some(here) = dir {
        candidates.push(here.join("vendor"));
        dir = here.parent();
    }
    for vendor in candidates {
        let Ok(bytes) = std::fs::read(vendor.join("composer/installed.json")) else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let packages = value
            .get("packages")
            .and_then(|p| p.as_array())
            .or_else(|| value.as_array())?;
        return packages
            .iter()
            .find(|p| p["name"] == package)
            .and_then(|p| p["version"].as_str())
            .map(|v| v.trim_start_matches('v').to_string());
    }
    None
}

fn major(version: &str) -> Option<u64> {
    version.split('.').next()?.parse().ok()
}

/// The directory that holds the adapter for this binary, written whole when it isn't there yet:
/// `<cache dir>/adapters/phpunit-<first 16 hex of its SHA-256>`, with the ini file that names its
/// prepend file by its path.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(format!("{PREPEND}\0{BOOTSTRAP}").as_bytes()).hex();
    let dir = cache
        .join("adapters")
        .join(format!("phpunit-{}", &digest[..16]));
    let prepend = dir.join("hushrun_phpunit_prepend.php");
    let ini = format!(
        "; hushrun's PHPUnit and Pest adapter (ADR 0116)\nauto_prepend_file = \"{}\"\n",
        prepend.display()
    );
    for (name, text) in [
        ("hushrun_phpunit_prepend.php", PREPEND),
        ("hushrun_phpunit.php", BOOTSTRAP),
        ("zz-hushrun-phpunit.ini", ini.as_str()),
    ] {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
            super::facts::write_whole(&path, text.as_bytes())?;
        }
    }
    Ok(dir)
}

/// The variables that load the adapter from `dir`, keeping what COMMAND's environment says:
/// `PHP_INI_SCAN_DIR` with `dir` last (an empty first entry is PHP's own scan directory), and
/// `before`, the prepend file PHP's configuration named, which hushrun's runs first.
pub fn variables(
    dir: &Path,
    scan: Option<OsString>,
    before: Option<String>,
) -> Vec<(String, OsString)> {
    let separator = if cfg!(windows) { ";" } else { ":" };
    let mut value = scan.unwrap_or_default();
    value.push(separator);
    value.push(dir.as_os_str());
    let mut out = vec![(SCAN_DIR.to_string(), value)];
    if let Some(before) = before.filter(|b| !b.is_empty()) {
        out.push((PREPEND_VAR.to_string(), before.into()));
    }
    out
}

/// \[C-91\] The prepend file PHP's configuration names for COMMAND's PHP, which hushrun's would
/// replace: asked of `php` itself, with COMMAND's assignments. `None` when there is none, or PHP
/// can't be asked.
fn configured_prepend(php: &str, at: &StartAt<'_>) -> Option<String> {
    let output = super::command::tool(php, at.cwd, at.assigned)
        .args(["-r", "echo ini_get('auto_prepend_file');"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (output.status.success() && !text.is_empty()).then(|| text.to_string())
}

/// \[ADR 0116\] The adapter for a COMMAND that starts one run of PHPUnit 10 to 12 or Pest 3 or 4;
/// `None` for any other COMMAND.
pub fn recognized(command: &[String], cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    let recognized = match recognize(command, cwd)? {
        Ok(recognized) => recognized,
        Err(why) => return Some(Err(why)),
    };
    if let Some(why) = unloadable(&recognized, cwd) {
        return Some(Err(why));
    }
    Some(Ok(Box::new(recognized)))
}

/// Why the adapter can't load for `recognized` in `cwd`: the PHPUnit or Pest installed there is a
/// major it doesn't know. `None` when it can, or nothing says which is installed.
pub fn unloadable(recognized: &Recognized, cwd: &Path) -> Option<String> {
    let (package, majors, name) = if recognized.pest {
        ("pestphp/pest", PEST_MAJORS, "Pest")
    } else {
        ("phpunit/phpunit", PHPUNIT_MAJORS, "PHPUnit")
    };
    let version = installed(package, cwd, recognized.vendor.as_deref())?;
    if major(&version).is_some_and(|m| majors.contains(&m)) {
        return None;
    }
    let known: Vec<String> = majors.iter().map(u64::to_string).collect();
    Some(format!(
        "this {name}, {version}, isn't one hushrun's adapter knows ({})",
        known.join(", ")
    ))
}

/// What the adapter observed of a run, before hushrun decides any state.
#[derive(Debug, Default)]
pub struct Observed {
    pub units: BTreeMap<String, super::Seen>,
    pub failures_by_unit: bool,
    /// The unit rule: the test files PHPUnit listed, from the root.
    pub unit_files: BTreeSet<String>,
    /// Why the adapter left nothing out, when the plan had listed something.
    pub refused: Option<String>,
    pub not_kept: Option<String>,
}

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

/// Reads what the adapter wrote in a run's directory, for the repository whose files are `tree`:
/// `Err` says why nothing can be told: nothing was written (`nothing`), more than one run of
/// PHPUnit wrote, or the adapter itself failed.
pub fn observe(
    files: &[FactsFile],
    tree: &units::Tree,
    command_passed: bool,
    nothing: &dyn Fn() -> String,
) -> Result<Observed, String> {
    let ours: Vec<&FactsFile> = files.iter().filter(|f| f.adapter == ADAPTER).collect();
    let file = match ours.as_slice() {
        [] => return Err(nothing()),
        [file] => *file,
        _ => return Err("more than one run of PHPUnit wrote for this run".into()),
    };
    if let Some(error) = file.errors.first() {
        return Err(error.chars().take(300).collect());
    }
    let whole: BTreeSet<NotFollowed> = file.not_followed.iter().map(Into::into).collect();
    let mut merged = super::facts::merged(files, ADAPTER);
    let any_failed = merged.values().any(|m| m.failed);
    let mut out = Observed {
        // \[C-92\] A run that failed with no test file failing failed for what no test explains.
        failures_by_unit: file.failures_by_unit && (command_passed || any_failed),
        unit_files: strings(file, "testFiles").into_iter().collect(),
        refused: file
            .settings
            .get("refused")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        not_kept: file.not_kept.clone(),
        ..Observed::default()
    };
    // \[C-92\] What the process read, in order, and how far into it each test file read.
    let sequence = strings(file, "sequence");
    let read_to = |unit: &str| -> usize {
        file.settings
            .get("readTo")
            .and_then(|v| v.get(unit))
            .and_then(|v| v.as_u64())
            .map_or(sequence.len(), |n| (n as usize).min(sequence.len()))
    };
    let by_name: BTreeSet<String> = strings(file, "byName").into_iter().collect();
    let related = |unit: &str| -> BTreeSet<String> {
        file.settings
            .get("related")
            .and_then(|v| v.get(unit))
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    for (unit, m) in std::mem::take(&mut merged) {
        let mut not_followed = m.not_followed;
        not_followed.extend(whole.iter().cloned());
        let mut reads = m.reads;
        let named = related(&unit);
        // Another test file whose classes the adapter knows is read only where this one names
        // it: PHPUnit included it before any test ran.
        reads.extend(
            sequence[..read_to(&unit)]
                .iter()
                .filter(|p| *p == &unit || !by_name.contains(*p) || named.contains(*p))
                .cloned(),
        );
        reads.insert(unit.clone());
        reads.retain(|p| tree.contains(p));
        out.units.insert(
            unit,
            super::Seen {
                outside: Default::default(),
                declared: None,
                getenv: Default::default(),
                facts: Facts {
                    complete: m.complete,
                    failed: m.failed,
                    not_followed,
                    deselected: m.deselected,
                    skipped: m.skipped > 0,
                    ..Facts::default()
                },
                reads,
                duration_ms: m.duration_ms,
                tests: m.tests,
            },
        );
    }
    Ok(out)
}

impl Recognized {
    fn name(&self) -> &'static str {
        if self.pest { "Pest" } else { "PHPUnit" }
    }

    /// Why PHPUnit wrote nothing, in words people read.
    fn nothing_written(&self) -> String {
        format!(
            "{} wrote nothing for hushrun: no PHP process of the run loaded hushrun's adapter, as when PHP reads no ini file (`php -n`) or a program replaces PHP_INI_SCAN_DIR",
            self.name()
        )
    }
}

impl Adapter for Recognized {
    fn word(&self) -> &'static str {
        ADAPTER
    }

    fn runner(&self) -> &'static str {
        self.name()
    }

    fn test_file(&self) -> &'static str {
        "test file"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            // \[C-92\] PHP runs no code to find a class it loaded once: what a test read is what
            // its test file read.
            reads: false,
        }
    }

    fn hints(&self) -> Hints {
        Hints::Kinds(SOURCE_KINDS)
    }

    // `binds`, `keys` and `start_variables` are the trait's defaults, written out: a call on this
    // type then names this impl's own method, which the static test map follows to it alone,
    // where the trait's would count every adapter that overrides it (CONTRIBUTING.md, "Which
    // tests CI runs again").
    fn binds(&self) -> bool {
        false
    }

    fn keys(&self) -> KeysOutside {
        KeysOutside::None
    }

    fn start_variables(&self, run_dir: &Path) -> Vec<(String, Option<OsString>)> {
        let _ = run_dir;
        Vec::new()
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        let dir =
            install(at.cache).map_err(|err| format!("can't write its PHPUnit adapter: {err}"))?;
        let php = self.php.as_deref().unwrap_or("php");
        let before = configured_prepend(php, at);
        Ok(variables(&dir, (at.env)(SCAN_DIR), before))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        if self.coverage {
            return Err(format!(
                "{}'s coverage is on, and a partial run's coverage would read as a drop",
                self.name()
            ));
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
            return Err(format!(
                "no run here has listed {}'s test files yet",
                self.name()
            ));
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
        let nothing = || self.nothing_written();
        let observed = observe(at.files, at.tree, at.command_passed, &nothing)?;
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
            unit_files: Some(observed.unit_files),
            refused: observed.refused,
            received: shared.received,
            not_kept: observed.not_kept,
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
            "{unit} isn't a test file of {check} as far as this machine knows: {} didn't list it in its last run here",
            self.name()
        )
    }

    fn why_known_from(&self) -> Option<&'static str> {
        Some(if self.pest {
            "Pest's list of test files is the one of its last run here"
        } else {
            "PHPUnit's list of test files is the one of its last run here"
        })
    }
}

#[cfg(test)]
mod tests;
