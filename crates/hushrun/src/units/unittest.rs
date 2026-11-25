//! \[ADR 0110\] The unittest adapter, observed (C-30, C-32, C-39, C-66 to C-69): for the standard
//! library's runner, `python -m unittest`, and Django's `manage.py test`, which drives it. A unit
//! is a test module's file, as unittest's discovery pattern names it; a test is one `TestCase`
//! method, by its id after its module's name (`CartTest.test_total`).
//!
//! - Loading: a `sitecustomize.py` embedded in this binary, written with the adapter to
//!   `<cache dir>/adapters/unittest-<hash>/` and put first on `PYTHONPATH`: Python imports it as
//!   it starts, it starts the adapter in the process whose command line runs the tests, and hands
//!   over to the `sitecustomize` it hides. No file of the repository changes, and COMMAND is kept
//!   byte for byte as written.
//! - The unit rule: a `.py` file whose name is a module's and matches the pattern discovery uses
//!   (C-66), read from COMMAND: `discover`'s `-p` (or its second positional), Django's
//!   `--pattern`, else `test*.py`, both runners' default.
//! - Observing: the observer pytest's adapter drives (`hushrun_observe.py`), with unittest's
//!   loader, suite and result as its points instead of pytest's hooks (C-42 holds for both).
//! - Selection: tests are removed from the top-level suite before it runs (C-67).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::units::{self, Facts, Set, Sets, Sources};

use super::Seen;
use super::adapter::{
    Adapter, Hints, KeysOutside, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt,
    WhyAt,
};
use super::command::program_name;
use super::shared::Shared;
use super::store::Map;

/// The file Python imports as it starts, written as `sitecustomize.py`.
const SITE: &str = include_str!("hushrun_unittest_site.py");
/// The adapter.
const ADAPTER: &str = include_str!("hushrun_unittest_impl.py");
/// The adapter's word (C-36).
pub const WORD: &str = "unittest";
/// What unittest's discovery, and Django's, match test files by when COMMAND says nothing.
pub const DEFAULT_PATTERN: &str = "test*.py";
/// The kinds of files the adapter observes being read: its sources (C-30).
pub const SOURCE_KINDS: &[&str] = &[".py"];
/// The programs that start Django's management commands.
const DJANGO_PROGRAMS: &[&str] = &["manage.py", "django-admin", "django-admin.py"];
/// Launchers that start a program in a project's environment, before their program.
const LAUNCHERS: &[&str] = &["uv", "poetry", "pipenv", "pdm", "rye", "hatch"];
/// Why a Python started so can't load the adapter.
const ISOLATED: &str = "a Python started with -I, -E or -S doesn't import the sitecustomize that loads hushrun's adapter";
/// \[C-66\] Why COMMAND's own `env PYTHONPATH=…` keeps the adapter out.
const ASSIGNED: &str = "COMMAND sets PYTHONPATH itself (env PYTHONPATH=…), which replaces the one that loads hushrun's adapter: set it in the environment hushrun runs in instead";
/// \[C-66\] Why Django's parallel runner can't be observed.
const PARALLEL: &str = "Django's --parallel runs the tests in worker processes, which hushrun's unittest adapter doesn't follow: run them with --parallel 1";

/// The directory that holds the adapter for this binary, written whole when it isn't there yet:
/// `<cache dir>/adapters/unittest-<first 16 hex of its SHA-256>`.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let observe = super::python::OBSERVE;
    let digest = hushrun_core::Digest::of(format!("{SITE}\0{ADAPTER}\0{observe}").as_bytes()).hex();
    let dir = cache
        .join("adapters")
        .join(format!("unittest-{}", &digest[..16]));
    for (name, text) in [
        ("sitecustomize.py", SITE),
        ("hushrun_unittest_impl.py", ADAPTER),
        ("hushrun_observe.py", observe),
    ] {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
            super::facts::write_whole(&path, text.as_bytes())?;
        }
    }
    Ok(dir)
}

/// The variable that loads the adapter from `dir`: `PYTHONPATH` with `dir` first, keeping what
/// COMMAND's environment already says.
pub fn variables(dir: &Path, pythonpath: Option<OsString>) -> Vec<(String, OsString)> {
    let mut path = dir.as_os_str().to_os_string();
    if let Some(existing) = pythonpath.filter(|p| !p.is_empty()) {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(existing);
    }
    vec![("PYTHONPATH".to_string(), path)]
}

/// \[ADR 0110\] What COMMAND is to the unittest adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unittest {
    /// \[C-66\] The pattern its discovery matches test files' names by.
    pub pattern: String,
    /// It is Django's runner, `manage.py test`.
    pub django: bool,
}

/// \[C-66\] The adapter for `command`, when it runs unittest's tests; `Err` why it can't load
/// safely.
pub fn recognized(command: &[String], _cwd: &Path) -> Option<Result<Box<dyn Adapter>, String>> {
    recognize(command).map(|found| found.map(|unittest| Box::new(unittest) as Box<dyn Adapter>))
}

/// \[C-66\] What `command` runs: `python -m unittest …`, `python manage.py test …`, `manage.py
/// test …`, `django-admin test …` or `python -m django test …`, each also after `uv run`, `poetry
/// run`, `pipenv run`, `pdm run`, `rye run` or `hatch run`, and through `coverage run`.
pub fn recognize(command: &[String]) -> Option<Result<Unittest, String>> {
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    let words = past_launcher(&words);
    let (first, rest) = words.split_first()?;
    let name = program_name(first);
    if is_python(&name) {
        return python(rest);
    }
    if name == "coverage" {
        return coverage(rest);
    }
    if DJANGO_PROGRAMS.contains(&&*name) {
        return django(rest);
    }
    None
}

/// A Python's own program name: `python`, `python3`, `python3.13`, `pythonw`, `py`.
fn is_python(name: &str) -> bool {
    if name == "py" || name == "pythonw" {
        return true;
    }
    name.strip_prefix("python")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// `words` past a launcher and its own options: from the first word that names a program this
/// adapter knows.
fn past_launcher<'a>(words: &'a [&'a str]) -> &'a [&'a str] {
    let [launcher, run, rest @ ..] = words else {
        return words;
    };
    if *run != "run" || !LAUNCHERS.contains(&&*program_name(launcher)) {
        return words;
    }
    let names = |at: usize| {
        let name = program_name(rest[at]);
        is_python(&name)
            || DJANGO_PROGRAMS.contains(&&*name)
            || (name == "coverage" && rest.get(at + 1) == Some(&"run"))
    };
    match (0..rest.len()).find(|at| names(*at)) {
        Some(at) => &rest[at..],
        None => words,
    }
}

/// What a Python's arguments run.
fn python(args: &[&str]) -> Option<Result<Unittest, String>> {
    let mut isolated = false;
    let mut at = 0;
    while let Some(word) = args.get(at) {
        if !word.starts_with('-') || *word == "-" {
            break;
        }
        if let Some(module) = word.strip_prefix("-m") {
            let (module, after) = if module.is_empty() {
                (*args.get(at + 1)?, at + 2)
            } else {
                (module, at + 1)
            };
            let found = module_runs(module, &args[after..])?;
            return Some(if isolated {
                Err(ISOLATED.into())
            } else {
                found
            });
        }
        if *word == "--check-hash-based-pycs" {
            at += 2;
            continue;
        }
        if word.starts_with("--") {
            at += 1;
            continue;
        }
        let flags = &word[1..];
        let mut takes_next = false;
        for (i, flag) in flags.char_indices() {
            match flag {
                'I' | 'E' | 'S' => isolated = true,
                'c' => return None,
                'W' | 'X' => {
                    takes_next = i + 1 == flags.len();
                    break;
                }
                _ => {}
            }
        }
        at += if takes_next { 2 } else { 1 };
    }
    let script = args.get(at)?;
    let rest = &args[at + 1..];
    let name = program_name(script);
    let found = if DJANGO_PROGRAMS.contains(&&*name) {
        django(rest)?
    } else if name == "coverage" {
        coverage(rest)?
    } else {
        return None;
    };
    Some(if isolated {
        Err(ISOLATED.into())
    } else {
        found
    })
}

/// What `-m module` runs.
fn module_runs(module: &str, args: &[&str]) -> Option<Result<Unittest, String>> {
    match module {
        "unittest" | "unittest.__main__" => Some(Ok(Unittest {
            pattern: unittest_pattern(args),
            django: false,
        })),
        "django" => django(args),
        "coverage" => coverage(args),
        _ => None,
    }
}

/// What `coverage run …` runs: a module after `-m`, or a script.
fn coverage(args: &[&str]) -> Option<Result<Unittest, String>> {
    const VALUED: &[&str] = &[
        "--concurrency",
        "--context",
        "--data-file",
        "--include",
        "--omit",
        "--rcfile",
        "--source",
        "--save-signal",
    ];
    let (run, args) = args.split_first()?;
    if *run != "run" {
        return None;
    }
    let mut module = false;
    let mut at = 0;
    while let Some(word) = args.get(at) {
        if *word == "-m" || *word == "--module" {
            module = true;
            at += 1;
        } else if VALUED.contains(word) {
            at += 2;
        } else if word.starts_with('-') {
            at += 1;
        } else {
            break;
        }
    }
    let target = args.get(at)?;
    let rest = &args[at + 1..];
    if module {
        return module_runs(target, rest);
    }
    DJANGO_PROGRAMS
        .contains(&&*program_name(target))
        .then(|| django(rest))
        .flatten()
}

/// \[C-66\] The pattern `python -m unittest …` discovers by: `discover`'s `-p`, `--pattern` or
/// second positional; `test*.py` for every other form, which discovers with it or names its
/// modules.
pub fn unittest_pattern(args: &[&str]) -> String {
    let Some(("discover", args)) = args.split_first().map(|(f, r)| (*f, r)) else {
        return DEFAULT_PATTERN.into();
    };
    let mut pattern: Option<&str> = None;
    let mut positionals: Vec<&str> = Vec::new();
    let mut at = 0;
    while let Some(word) = args.get(at) {
        match *word {
            "-p" | "--pattern" => {
                pattern = args.get(at + 1).copied();
                at += 2;
            }
            "-s" | "--start-directory" | "-t" | "--top-level-directory" | "-k" | "--durations" => {
                at += 2;
            }
            _ if word.starts_with("--pattern=") => {
                pattern = Some(&word["--pattern=".len()..]);
                at += 1;
            }
            _ if word.starts_with("-p") && word.len() > 2 => {
                pattern = Some(&word[2..]);
                at += 1;
            }
            _ if word.starts_with('-') => at += 1,
            _ => {
                positionals.push(word);
                at += 1;
            }
        }
    }
    pattern
        .or_else(|| positionals.get(1).copied())
        .unwrap_or(DEFAULT_PATTERN)
        .to_string()
}

/// \[C-66\] What Django's `test` command runs, after its program: its `--pattern`, and `Err` for
/// its parallel runner.
fn django(args: &[&str]) -> Option<Result<Unittest, String>> {
    let (command, args) = args.split_first()?;
    if *command != "test" {
        return None;
    }
    let mut pattern: Option<&str> = None;
    let mut at = 0;
    while let Some(word) = args.get(at) {
        if *word == "-p" || *word == "--pattern" {
            pattern = args.get(at + 1).copied();
            at += 2;
            continue;
        }
        if let Some(value) = word.strip_prefix("--pattern=") {
            pattern = Some(value);
        } else if let Some(value) = word.strip_prefix("--parallel") {
            let value = match value.strip_prefix('=') {
                Some(value) => Some(value),
                None if value.is_empty() => args
                    .get(at + 1)
                    .copied()
                    .filter(|next| *next == "auto" || next.parse::<u32>().is_ok()),
                None => {
                    at += 1;
                    continue;
                }
            };
            // `--parallel` alone is `auto`.
            if value.is_none_or(|v| v == "auto" || v.parse::<u32>().is_ok_and(|n| n > 1)) {
                return Some(Err(PARALLEL.into()));
            }
        } else if word.starts_with("-p") && !word.starts_with("--") && word.len() > 2 {
            pattern = Some(&word[2..]);
        }
        at += 1;
    }
    Some(Ok(Unittest {
        pattern: pattern.unwrap_or(DEFAULT_PATTERN).to_string(),
        django: true,
    }))
}

/// \[C-66\] Whether `path`, from the repository root, is a test file by `pattern`, as unittest's
/// discovery matches it: a `.py` file whose name is a module's (a letter or `_`, then letters,
/// digits and `_`) and matches the pattern (`fnmatch`, on the name; on Windows, in any case).
pub fn is_test_file(pattern: &str, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = name
        .strip_suffix(".py")
        .or_else(|| cfg!(windows).then(|| name.strip_suffix(".PY")).flatten())
    else {
        return false;
    };
    let mut chars = stem.chars();
    let module = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_');
    if !module {
        return false;
    }
    if cfg!(windows) {
        super::python::fnmatch(&pattern.to_lowercase(), &name.to_lowercase())
    } else {
        super::python::fnmatch(pattern, name)
    }
}

/// \[ADR 0074\] As pytest's: a plan says anything of a check's test files only once the adapter
/// reported them here, or a hint says so (C-38).
fn speaks_of_test_files(map: &Map, shared: &Shared) -> bool {
    map.reported() || shared.names_any()
}

impl Unittest {
    fn sets(&self, at_tree: &units::Tree, ignore: &[hushrun_core::glob::Pattern]) -> Sets {
        units::classify(
            at_tree.paths(),
            &|path| is_test_file(&self.pattern, path),
            Sources::Kinds(SOURCE_KINDS),
            ignore,
        )
    }
}

impl Adapter for Unittest {
    fn word(&self) -> &'static str {
        WORD
    }

    fn runner(&self) -> &'static str {
        if self.django { "Django's" } else { "unittest" }
    }

    fn test_file(&self) -> &'static str {
        "test module"
    }

    fn tests(&self) -> KnowsTests {
        // \[C-68\] It watches each test on Python 3.12 or later, as pytest's adapter does, and
        // below says why it doesn't (`settings.watch`).
        KnowsTests {
            unit: true,
            reads: true,
        }
    }

    // `hints`, `binds`, `keys` and `start_variables` are the trait's defaults, written out: a call
    // on this type then names this impl's own method, which the static test map follows to it
    // alone, where the trait's would count every adapter that overrides it (CONTRIBUTING.md,
    // "Which tests CI runs again").
    fn hints(&self) -> Hints {
        Hints::Sources
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
        if at.assigned.iter().any(|(name, _)| name == "PYTHONPATH") {
            return Err(ASSIGNED.into());
        }
        install(at.cache)
            .map(|dir| variables(&dir, (at.env)("PYTHONPATH")))
            .map_err(|err| format!("can't write its unittest adapter: {err}"))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let sets = self.sets(at.tree, at.ignore);
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        if !speaks_of_test_files(at.map, &shared) {
            return Ok(None);
        }
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
                python_files: Some(vec![self.pattern.clone()]),
                test_files: None,
            },
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let merged = super::facts::merged(at.files, WORD);
        if merged.is_empty() {
            return Ok(None);
        }
        // \[C-66\] Every discovery the runner made used the pattern hushrun read of COMMAND; a
        // module it loaded by name is a unit only by that pattern too.
        let told = super::facts::setting(at.files, WORD, "pythonFiles");
        let agrees =
            matches!(&told, Ok(Some(patterns)) if patterns.iter().all(|p| *p == self.pattern));
        let guard = (!agrees).then(|| units::NotFollowed {
            why: "unittest-pattern".into(),
            detail: format!(
                "unittest discovered test files by another pattern than the one hushrun read of COMMAND ({})",
                self.pattern
            ),
        });
        let failures_by_unit = merged.values().all(|m| m.failures_by_unit);
        let seen: BTreeMap<String, Seen> = merged
            .into_iter()
            .filter(|(unit, _)| guard.is_some() || is_test_file(&self.pattern, unit))
            .map(|(unit, m)| {
                let mut not_followed = m.not_followed;
                not_followed.extend(guard.clone());
                let facts = Facts {
                    complete: m.complete,
                    failed: m.failed,
                    not_followed,
                    deselected: m.deselected,
                    skipped: m.skipped > 0,
                    ..Facts::default()
                };
                let seen = Seen {
                    outside: Default::default(),
                    declared: None,
                    getenv: Default::default(),
                    facts,
                    reads: m.reads,
                    duration_ms: m.duration_ms,
                    tests: m.tests,
                };
                (unit, seen)
            })
            .collect();
        let sets = self.sets(at.tree, at.ignore);
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = seen
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
            seen,
            failures_by_unit,
            sets,
            planned,
            edges: super::facts::edges_of(at.files, WORD),
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, _: &Path, _: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras {
            python_files: Some(vec![self.pattern.clone()]),
            test_files: None,
        }
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        self.sets(at.tree, at.ignore)
    }

    fn why_not_a_unit(&self, at: &WhyAt<'_>, check: &str, unit: &str) -> String {
        if !at.map.reported() && at.shared.hints.is_empty() {
            return format!(
                "{unit} isn't a test module of {check} as far as this machine knows: no run of {check} here has had unittest report its test modules"
            );
        }
        format!(
            "{unit} isn't a test module of {check}: unittest's discovery pattern ({}) doesn't match its name",
            self.pattern
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    fn found(text: &str) -> Option<Result<Unittest, String>> {
        recognize(&words(text))
    }

    fn pattern(text: &str) -> Option<String> {
        found(text).and_then(Result::ok).map(|u| u.pattern)
    }

    // covers: C-66
    #[test]
    fn the_runner_is_recognized_in_every_form_that_starts_it() {
        for text in [
            "python -m unittest",
            "python3 -m unittest -v",
            "python3.13 -B -X dev -m unittest discover",
            "/usr/bin/python3 -W error -munittest tests.test_cart",
            "uv run python -m unittest",
            "uv run --with coverage --python 3.12 python -m unittest",
            "poetry run python -m unittest discover -s tests",
            "coverage run -m unittest",
            "coverage run --branch --source app -m unittest discover",
            "python -m coverage run -m unittest",
        ] {
            assert_eq!(
                found(text),
                Some(Ok(Unittest {
                    pattern: pattern(text).unwrap(),
                    django: false
                })),
                "{text}"
            );
        }
        for text in [
            "python manage.py test",
            "./manage.py test app.tests",
            "python3 src/manage.py test --keepdb",
            "django-admin test --settings=proj.settings",
            "python -m django test",
            "pipenv run python manage.py test",
            "coverage run manage.py test",
            "python manage.py test --parallel 1",
        ] {
            assert!(
                matches!(found(text), Some(Ok(Unittest { django: true, .. }))),
                "{text}"
            );
        }
        for text in [
            "pytest",
            "python -m pytest",
            "python -c import unittest",
            "python manage.py migrate",
            "python tests/run.py",
            "uv run pytest",
            "coverage report",
            "tox -e py312",
        ] {
            assert_eq!(found(text), None, "{text}");
        }
    }

    // covers: C-66
    #[test]
    fn a_python_that_ignores_pythonpath_or_site_and_djangos_parallel_runner_are_said() {
        for text in [
            "python -I -m unittest",
            "python3 -sE -m unittest",
            "python -S manage.py test",
        ] {
            assert_eq!(found(text), Some(Err(ISOLATED.into())), "{text}");
        }
        for text in [
            "python manage.py test --parallel",
            "python manage.py test --parallel auto",
            "python manage.py test --parallel 4",
            "python manage.py test --parallel=8 app",
        ] {
            assert_eq!(found(text), Some(Err(PARALLEL.into())), "{text}");
        }
        assert!(matches!(
            found("python manage.py test --parallel=1"),
            Some(Ok(_))
        ));
    }

    // covers: C-66
    #[test]
    fn the_unit_rule_is_the_pattern_discovery_uses() {
        assert_eq!(pattern("python -m unittest").as_deref(), Some("test*.py"));
        assert_eq!(
            pattern("python -m unittest discover -s tests -p *_test.py").as_deref(),
            Some("*_test.py")
        );
        assert_eq!(
            pattern("python -m unittest discover --pattern=check_*.py").as_deref(),
            Some("check_*.py")
        );
        assert_eq!(
            pattern("python -m unittest discover tests spec_*.py .").as_deref(),
            Some("spec_*.py")
        );
        assert_eq!(
            pattern("python -m unittest discover -v -s src -t . -pt_*.py").as_deref(),
            Some("t_*.py")
        );
        assert_eq!(
            pattern("python manage.py test -p tests_*.py app").as_deref(),
            Some("tests_*.py")
        );
        assert_eq!(
            pattern("python manage.py test --pattern=x*.py").as_deref(),
            Some("x*.py")
        );
        let default = DEFAULT_PATTERN;
        assert!(is_test_file(default, "tests/test_cart.py"));
        assert!(is_test_file(default, "app/tests.py"));
        assert!(!is_test_file(default, "tests/conftest.py"));
        assert!(!is_test_file(default, "tests/test-cart.py"));
        assert!(!is_test_file(default, "tests/test_data.txt"));
        assert!(!is_test_file(default, "testdata/helpers.py"));
        assert!(is_test_file("*_test.py", "pkg/cart_test.py"));
        assert!(!is_test_file("*_test.py", "pkg/test_cart.py"));
    }

    #[test]
    fn the_adapter_loads_before_what_the_environment_says() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let vars = variables(
            Path::new("/c/adapters/unittest-1"),
            Some("/repo/src".into()),
        );
        assert_eq!(
            vars,
            [(
                "PYTHONPATH".to_string(),
                OsString::from(format!("/c/adapters/unittest-1{sep}/repo/src"))
            )]
        );
        let dir = tempfile::tempdir().unwrap();
        let installed = install(dir.path()).unwrap();
        for name in [
            "sitecustomize.py",
            "hushrun_unittest_impl.py",
            "hushrun_observe.py",
        ] {
            assert!(installed.join(name).is_file(), "{name}");
        }
    }
}
