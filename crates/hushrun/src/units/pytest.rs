//! \[ADR 0068\] The pytest adapter, observed: a plugin embedded in the binary, written to hushrun's
//! cache directory and loaded through `PYTHONPATH` and `PYTEST_ADDOPTS=-p hushrun_pytest`; and
//! its unit rule, pytest's `python_files`, read from the repository's configuration before any
//! test runs (C-30).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_core::units::{self, Facts, Set, Sets, Sources};

use super::Seen;
use super::adapter::{
    Adapter, KnowsTests, PlanAt, Planning, Read, ReadAt, RunExtras, StartAt, WhyAt,
};
use super::shared::Shared;
use super::store::Map;

/// The plugin pytest loads: any Python parses it, and on 3.8 or later it loads the adapter.
const SHIM: &str = include_str!("hushrun_pytest.py");
/// The adapter.
const ADAPTER: &str = include_str!("hushrun_pytest_impl.py");
/// The module `-p` names.
pub const MODULE: &str = "hushrun_pytest";
/// What pytest collects as test files when its configuration says nothing.
pub const DEFAULT_PYTHON_FILES: [&str; 2] = ["test_*.py", "*_test.py"];
/// The kinds of files the adapter observes being read: its sources (C-30).
pub const SOURCE_KINDS: &[&str] = &[".py"];

/// The directory that holds the plugin for this binary, written whole when it isn't there yet:
/// `<cache dir>/adapters/pytest-<first 16 hex of its SHA-256>`.
pub fn install(cache: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(format!("{SHIM}\0{ADAPTER}").as_bytes()).hex();
    let dir = cache
        .join("adapters")
        .join(format!("pytest-{}", &digest[..16]));
    for (name, text) in [
        ("hushrun_pytest.py", SHIM),
        ("hushrun_pytest_impl.py", ADAPTER),
    ] {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
            super::facts::write_whole(&path, text.as_bytes())?;
        }
    }
    Ok(dir)
}

/// The variables that load the plugin from `dir`, each keeping what COMMAND's environment
/// already says: `PYTHONPATH` with `dir` first, and `PYTEST_ADDOPTS` with `-p hushrun_pytest`
/// last.
pub fn variables(
    dir: &Path,
    pythonpath: Option<OsString>,
    addopts: Option<OsString>,
) -> Vec<(String, OsString)> {
    let mut path = dir.as_os_str().to_os_string();
    if let Some(existing) = pythonpath.filter(|p| !p.is_empty()) {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(existing);
    }
    let mut opts = addopts.filter(|o| !o.is_empty()).unwrap_or_default();
    if !opts.is_empty() {
        opts.push(" ");
    }
    opts.push(format!("-p {MODULE}"));
    vec![
        ("PYTHONPATH".to_string(), path),
        ("PYTEST_ADDOPTS".to_string(), opts),
    ]
}

/// Whether the plugin can't be loaded safely for `command`: a Python started with `-I` or `-E`
/// ignores `PYTHONPATH` while pytest still reads `PYTEST_ADDOPTS`, and tox passes some variables
/// to its environments and not others, so in either the plugin pytest is told to load could be
/// missing, and pytest would stop.
pub fn unsafe_for(command: &[String]) -> Option<&'static str> {
    let python = |word: &str| {
        let file = word.rsplit(['/', '\\']).next().unwrap_or(word);
        file.starts_with("python")
    };
    let mut after_python = false;
    for word in command {
        let file = word.rsplit(['/', '\\']).next().unwrap_or(word);
        if file == "tox" || file.starts_with("tox.") || file == "nox" {
            return Some("tox and nox choose what reaches their environments");
        }
        if python(word) {
            after_python = true;
            continue;
        }
        if after_python
            && word.starts_with('-')
            && !word.starts_with("--")
            && (word.contains('I') || word.contains('E'))
        {
            return Some("a Python started with -I or -E ignores PYTHONPATH");
        }
        if !word.starts_with('-') {
            after_python = false;
        }
    }
    None
}

/// pytest's `python_files` for a run in `cwd`, as its configuration file says, from `cwd` up to
/// the repository's `root`: the first of `pytest.ini`, `pyproject.toml` with
/// `[tool.pytest.ini_options]`, `tox.ini` with `[pytest]` and `setup.cfg` with `[tool:pytest]` in
/// each directory, as pytest finds its configuration; the default without one.
pub fn python_files(root: &Path, cwd: &Path) -> Vec<String> {
    let mut dir = Some(cwd);
    while let Some(here) = dir {
        if let Some(found) = configured(here) {
            return found.unwrap_or_else(default_python_files);
        }
        if here == root {
            break;
        }
        dir = here.parent().filter(|parent| parent.starts_with(root));
    }
    default_python_files()
}

fn default_python_files() -> Vec<String> {
    DEFAULT_PYTHON_FILES.iter().map(|p| p.to_string()).collect()
}

/// The configuration file of `dir`, when it has one pytest reads: `Some(None)` when it says
/// nothing of `python_files`.
fn configured(dir: &Path) -> Option<Option<Vec<String>>> {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    if let Some(text) = read("pytest.ini") {
        // pytest.ini wins even without a [pytest] section.
        return Some(ini_value(&text, &["pytest"], "python_files"));
    }
    if let Some(text) = read("pyproject.toml")
        && let Some(value) = toml_section(&text, &["tool.pytest.ini_options", "tool.pytest"])
    {
        return Some(value);
    }
    for (name, sections) in [
        ("tox.ini", &["pytest"][..]),
        ("setup.cfg", &["tool:pytest"][..]),
    ] {
        if let Some(text) = read(name)
            && has_section(&text, sections)
        {
            return Some(ini_value(&text, sections, "python_files"));
        }
    }
    None
}

fn section_name(line: &str) -> Option<&str> {
    let line = line.trim();
    line.strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

fn has_section(text: &str, sections: &[&str]) -> bool {
    text.lines()
        .filter_map(section_name)
        .any(|name| sections.contains(&name))
}

/// An INI value, continued on indented lines, split at whitespace.
fn ini_value(text: &str, sections: &[&str], key: &str) -> Option<Vec<String>> {
    let mut inside = false;
    let mut value: Option<String> = None;
    for line in text.lines() {
        if let Some(name) = section_name(line) {
            if value.is_some() {
                break;
            }
            inside = sections.contains(&name);
            continue;
        }
        if !inside {
            continue;
        }
        let continued = line.starts_with(' ') || line.starts_with('\t');
        if let Some(found) = value.as_mut() {
            if continued && !line.trim().is_empty() {
                found.push(' ');
                found.push_str(line.trim());
                continue;
            }
            break;
        }
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some((name, rest)) = trimmed.split_once(['=', ':'])
            && name.trim() == key
        {
            value = Some(rest.trim().to_string());
        }
    }
    value.map(|v| v.split_whitespace().map(str::to_string).collect())
}

/// `Some` when `text` has one of `sections`, with what it says of `python_files`: a string, split
/// at whitespace, or a list of strings.
fn toml_section(text: &str, sections: &[&str]) -> Option<Option<Vec<String>>> {
    let mut found = false;
    let mut inside = false;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some(name) = section_name(line) {
            inside = sections.contains(&name);
            found |= inside;
            continue;
        }
        if !inside {
            continue;
        }
        let Some((name, rest)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != "python_files" {
            continue;
        }
        let mut value = rest.trim().to_string();
        if value.starts_with('[') {
            while !value.contains(']') {
                let Some(next) = lines.next() else { break };
                value.push(' ');
                value.push_str(next.trim());
            }
            let inner = value.trim_start_matches('[');
            let inner = inner.split(']').next().unwrap_or("");
            let items = inner
                .split(',')
                .map(|item| item.trim().trim_matches(['"', '\'']).to_string())
                .filter(|item| !item.is_empty())
                .collect();
            return Some(Some(items));
        }
        let text = value.trim_matches(['"', '\'']);
        return Some(Some(text.split_whitespace().map(str::to_string).collect()));
    }
    found.then_some(None)
}

/// Whether `path`, from the repository root, is a test file by `patterns`, as pytest matches
/// `python_files` against a file's name. `None` when a pattern names a directory, which this
/// version doesn't read: then no file is a unit.
pub fn is_test_file(patterns: &[String], path: &str) -> Option<bool> {
    if patterns.iter().any(|p| p.contains('/') || p.contains('\\')) {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    Some(path.ends_with(".py") && patterns.iter().any(|p| fnmatch(p, name)))
}

/// Python's `fnmatch` on a file name: `*`, `?`, `[seq]` and `[!seq]`.
fn fnmatch(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| go(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some('[') => {
                let Some(close) = p.iter().skip(2).position(|c| *c == ']').map(|i| i + 2) else {
                    return n.first() == Some(&'[') && go(&p[1..], &n[1..]);
                };
                let Some(c) = n.first() else { return false };
                let (negate, set) = match p[1] {
                    '!' => (true, &p[2..close]),
                    _ => (false, &p[1..close]),
                };
                let mut hit = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == '-' {
                        hit |= set[i] <= *c && *c <= set[i + 2];
                        i += 3;
                    } else {
                        hit |= set[i] == *c;
                        i += 1;
                    }
                }
                hit != negate && go(&p[close + 1..], &n[1..])
            }
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

/// \[ADR 0068, 0077\] The pytest adapter, which any COMMAND no other adapter takes gets: it does
/// nothing where pytest never starts.
#[derive(Debug, Clone, Copy)]
pub struct Pytest;

/// The adapter any COMMAND gets that no other takes (C-32); `Err` why it can't load safely.
pub fn any_other(command: &[String]) -> Result<Box<dyn Adapter>, String> {
    match unsafe_for(command) {
        Some(why) => Err(why.into()),
        None => Ok(Box::new(Pytest)),
    }
}

/// \[ADR 0074\] Whether a plan, and `hushrun ready`, say anything of a check's test files before
/// it runs: for pytest, which any COMMAND gets, only once pytest reported this check's test files
/// here, as its map says, or \[C-38\] on another machine, as a hint read says. A check that runs
/// `npm test` or `make test` never started pytest, and its Python-looking files are no test files
/// of it.
fn speaks_of_test_files(map: &Map, shared: &Shared) -> bool {
    map.reported() || shared.names_any()
}

impl Adapter for Pytest {
    fn word(&self) -> &'static str {
        "pytest"
    }

    fn runner(&self) -> &'static str {
        "pytest"
    }

    fn tests(&self) -> KnowsTests {
        KnowsTests {
            unit: true,
            reads: false,
        }
    }

    fn variables(&self, at: &StartAt<'_>) -> Result<Vec<(String, OsString)>, String> {
        install(at.cache)
            .map(|dir| variables(&dir, (at.env)("PYTHONPATH"), (at.env)("PYTEST_ADDOPTS")))
            .map_err(|err| format!("can't write its pytest plugin: {err}"))
    }

    fn plan_from_files(&self, at: &PlanAt<'_>) -> Result<Option<Planning>, String> {
        let found = python_files(at.root, at.cwd);
        if is_test_file(&found, "x.py").is_none() {
            if !speaks_of_test_files(at.map, &Shared::default()) {
                return Ok(None);
            }
            return Err(
                "pytest's python_files names a directory, which this version doesn't read".into(),
            );
        }
        let sets = units::classify(
            at.tree.paths(),
            &|path| is_test_file(&found, path) == Some(true),
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        if !speaks_of_test_files(at.map, &shared) {
            return Ok(None);
        }
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
                python_files: Some(found),
                test_files: None,
            },
            ..Planning::default()
        }))
    }

    fn read(&self, at: &ReadAt<'_>) -> Result<Option<Read>, String> {
        let merged = super::facts::merged(at.files, "pytest");
        if merged.is_empty() {
            return Ok(None);
        }
        let patterns = python_files(at.root, at.cwd);
        let told = super::facts::setting(at.files, "pytest", "pythonFiles");
        let readable = is_test_file(&patterns, "x.py").is_some();
        let agrees = matches!(&told, Ok(Some(list)) if *list == patterns);
        let guard = (!readable || !agrees).then(|| units::NotFollowed {
            why: "python-files".into(),
            detail: "pytest's python_files isn't what hushrun read of its configuration".into(),
        });
        let failures_by_unit = merged.values().all(|m| m.failures_by_unit);
        let seen: BTreeMap<String, Seen> = merged
            .into_iter()
            .map(|(unit, m)| {
                let mut not_followed = m.not_followed;
                not_followed.extend(guard.clone());
                let facts = Facts {
                    complete: m.finished >= m.selected,
                    failed: m.failed,
                    not_followed,
                    deselected: m.deselected,
                    ..Facts::default()
                };
                let seen = Seen {
                    facts,
                    reads: m.reads,
                    duration_ms: m.duration_ms,
                    tests: m.tests,
                };
                (unit, seen)
            })
            .collect();
        let is_unit = |path: &str| readable && is_test_file(&patterns, path) == Some(true);
        let sets = units::classify(
            at.tree.paths(),
            &is_unit,
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        );
        let shared = super::fetch(at.asker, self.hints(), &sets, at.tree, at.ignore);
        let planned = seen
            .keys()
            .filter_map(|unit| {
                let lists =
                    super::shared::lists(unit, at.map.units.get(unit), &shared, at.tree, None)?;
                Some((unit.clone(), lists))
            })
            .collect();
        Ok(Some(Read {
            seen,
            failures_by_unit,
            sets,
            planned,
            edges: super::facts::edges_of(at.files, "pytest"),
            received: shared.received,
            ..Read::default()
        }))
    }

    fn again(&self, root: &Path, cwd: &Path, _: &BTreeSet<String>) -> RunExtras {
        RunExtras {
            python_files: Some(python_files(root, cwd)),
            test_files: None,
        }
    }

    fn why_sets(&self, at: &WhyAt<'_>) -> Sets {
        let found = python_files(at.root, at.cwd);
        units::classify(
            at.tree.paths(),
            &|p| is_test_file(&found, p) == Some(true),
            Sources::Kinds(SOURCE_KINDS),
            at.ignore,
        )
    }

    fn why_not_a_unit(&self, at: &WhyAt<'_>, check: &str, unit: &str) -> String {
        if !at.map.reported() && at.shared.hints.is_empty() {
            return format!(
                "{unit} isn't a test file of {check} as far as this machine knows: no run of {check} here has had pytest report its test files"
            );
        }
        format!(
            "{unit} isn't a test file of {check}: pytest's python_files ({}) doesn't name it",
            python_files(at.root, at.cwd).join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-30
    #[test]
    fn the_unit_rule_is_pythons_files_from_the_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sub = root.join("pkg");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(python_files(root, &sub), ["test_*.py", "*_test.py"]);
        std::fs::write(
            root.join("setup.cfg"),
            "[metadata]\nname = x\n[tool:pytest]\npython_files =\n    check_*.py\n    *_check.py\n",
        )
        .unwrap();
        assert_eq!(python_files(root, &sub), ["check_*.py", "*_check.py"]);
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.pytest.ini_options]\npython_files = [\n  \"t_*.py\",\n  'x_*.py',\n]\n",
        )
        .unwrap();
        assert_eq!(python_files(root, &sub), ["t_*.py", "x_*.py"]);
        // A pyproject.toml without pytest's section is passed over.
        std::fs::write(sub.join("pyproject.toml"), "[project]\nname = \"pkg\"\n").unwrap();
        assert_eq!(python_files(root, &sub), ["t_*.py", "x_*.py"]);
        // pytest.ini wins, and says nothing here: the default.
        std::fs::write(sub.join("pytest.ini"), "[pytest]\naddopts = -q\n").unwrap();
        assert_eq!(python_files(root, &sub), ["test_*.py", "*_test.py"]);
        std::fs::write(
            sub.join("pytest.ini"),
            "[pytest]\npython_files = spec_*.py\n",
        )
        .unwrap();
        assert_eq!(python_files(root, &sub), ["spec_*.py"]);
    }

    // covers: C-30
    #[test]
    fn test_files_match_by_name() {
        let defaults: Vec<String> = DEFAULT_PYTHON_FILES.iter().map(|s| s.to_string()).collect();
        assert_eq!(is_test_file(&defaults, "tests/test_cart.py"), Some(true));
        assert_eq!(is_test_file(&defaults, "cart_test.py"), Some(true));
        assert_eq!(is_test_file(&defaults, "tests/conftest.py"), Some(false));
        assert_eq!(is_test_file(&defaults, "tests/test_data.txt"), Some(false));
        assert_eq!(is_test_file(&defaults, "testing/helpers.py"), Some(false));
        let custom = vec!["check_[a-c]?.py".to_string()];
        assert_eq!(is_test_file(&custom, "check_b1.py"), Some(true));
        assert_eq!(is_test_file(&custom, "check_d1.py"), Some(false));
        assert_eq!(
            is_test_file(&["tests/test_*.py".to_string()], "tests/test_a.py"),
            None
        );
    }

    #[test]
    fn the_plugin_loads_beside_what_the_environment_says() {
        let vars = variables(
            Path::new("/c/adapters/pytest-1"),
            Some("/repo/src".into()),
            Some("-q".into()),
        );
        let sep = if cfg!(windows) { ";" } else { ":" };
        assert_eq!(
            vars[0].1,
            OsString::from(format!("/c/adapters/pytest-1{sep}/repo/src"))
        );
        assert_eq!(vars[1].1, OsString::from("-q -p hushrun_pytest"));
        let bare = variables(Path::new("/c/a"), None, Some("".into()));
        assert_eq!(bare[0].1, OsString::from("/c/a"));
        assert_eq!(bare[1].1, OsString::from("-p hushrun_pytest"));
        let words = |text: &str| text.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert!(unsafe_for(&words("python -I -m pytest")).is_some());
        assert!(unsafe_for(&words("python3 -sE -m pytest")).is_some());
        assert!(unsafe_for(&words("tox -e py312")).is_some());
        assert!(unsafe_for(&words("python -m pytest -k Index")).is_none());
        assert!(unsafe_for(&words("pytest -x")).is_none());
        assert!(unsafe_for(&words("uv run pytest")).is_none());
    }
}
