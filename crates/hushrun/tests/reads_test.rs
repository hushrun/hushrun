//! \[ADR 0078, 0080\] What a test read, with `tests.reads: test` (C-40, C-42): fixtures that pin
//! the heuristic. Each changes one file so that one test, `test_target`, fails, and asks `observe`
//! whether that test would have been reused: never with `reads: file`, whose list is its test
//! file's; not with `reads: test` where the rule follows the change; and, for the three shapes
//! `reads: test` is known to miss (RFC 0004 §3.2, C-40), yes, which a test names as a known miss.
//! One more shows an audit run catching such a miss and suspending reuse.
//!
//! They need Python 3.12 or later with pytest, to watch a test, and say so and pass otherwise,
//! unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::{Value, json};

/// Environment variables a test must never inherit: CI's, hushrun's, an outer run's, and what
/// says a coding agent runs it.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_UNITS",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "PYTEST_ADDOPTS",
    "PYTHONPATH",
    "CARGO_TARGET_DIR",
    "npm_lifecycle_event",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
    // B-1, B-2: the developer's build mode, store and wrapper stay out: a test run that cargo
    // itself compiles through the wrapper doesn't pass its session on to a fixture's cargo.
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";
const REPOSITORY: &str = "example.com/acme/shop";

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let r = Repo {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
        };
        for (path, text) in files {
            r.write(path, text);
        }
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn hushrun(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        for var in CLEARED {
            cmd.env_remove(var);
        }
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("RUSTC_WORKSPACE_WRAPPER")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        cmd
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = self.hushrun();
        for (name, value) in env {
            cmd.env(name, value);
        }
        let output = cmd.args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// The newest result this machine recorded for the check.
    fn newest_result(&self) -> Value {
        let mut files = Vec::new();
        collect(&self.cache.join("results/results/v0"), &mut files);
        files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
        let newest = files.last().expect("a result");
        serde_json::from_slice(&std::fs::read(newest).unwrap()).unwrap()
    }

    /// How many unit results this machine holds.
    fn unit_results(&self) -> usize {
        let mut files = Vec::new();
        collect(&self.cache.join("results/units/v0"), &mut files);
        files.len()
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn tools(what: &str, here: bool) -> bool {
    if here {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

fn python() -> String {
    std::env::var("HUSHRUN_TEST_PYTHON").unwrap_or_else(|_| "python3".into())
}

fn has_pytest() -> bool {
    Command::new(python())
        .args([
            "-c",
            "import sys, pytest, xdist; sys.exit(0 if sys.version_info >= (3, 8) else 1)",
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether the Python the tests run has `sys.monitoring` (3.12 and later).
fn watches() -> bool {
    Command::new(python())
        .args([
            "-c",
            "import sys; sys.exit(0 if hasattr(sys, 'monitoring') else 1)",
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether these tests can run here: pytest, on a Python that watches a test. Below 3.12 there is
/// nothing to watch with, which isn't a missing tool: `test_as_unit`'s observe test holds what a
/// run says then (C-42), and these skip.
fn can_watch() -> bool {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return false;
    }
    if !watches() {
        eprintln!("note: skipping: this Python has no sys.monitoring, so pytest watches no test");
        return false;
    }
    true
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: {REPOSITORY}\nchecks:\n  unit:\n    tests: {tests}\n")
}

fn repo_of(files: &[(&str, &str)], tests: &str) -> Repo {
    let mut all: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("app/__init__.py".into(), String::new()),
        // A test file every change runs, so that the check's own key misses.
        (
            "tests/test_other.py".into(),
            "def test_other():\n    assert True\n".into(),
        ),
    ];
    for (path, text) in files {
        all.retain(|(p, _)| p != path);
        all.push((path.to_string(), text.to_string()));
    }
    let borrowed: Vec<(&str, &str)> = all.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    Repo::new(&borrowed)
}

fn pytest_args(python: &str) -> Vec<&str> {
    vec![
        "run",
        "unit",
        "--",
        python,
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
    ]
}

/// Whether `observe`, with `reads`, would have reused `test_target` of `tests/test_a.py` after
/// `change` made it fail: what its disagreement line says.
fn would_reuse(files: &[(&str, &str)], change: (&str, &str), reads: &str) -> bool {
    let r = repo_of(
        files,
        &format!("{{mode: observe, unit: test, reads: {reads}}}"),
    );
    let python = python();
    let first = r.run(&pytest_args(&python), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(
        !first.stderr.contains("each test is taken to have read"),
        "{}",
        first.stderr
    );
    r.write(change.0, change.1);
    let second = r.run(&pytest_args(&python), &[]);
    assert_eq!(
        second.code, 1,
        "the change makes test_target fail:\n{}\n{}",
        second.stdout, second.stderr
    );
    assert!(second.stdout.contains("test_target"), "{}", second.stdout);
    second.stderr.contains(
        "hushrun: unit: tests/test_a.py::test_target failed, though it passed before on the same files it read: with `tests: reuse` it would have been reused",
    )
}

/// `reads: test` follows the change: neither setting would reuse the test.
fn caught(files: &[(&str, &str)], change: (&str, &str)) {
    if !can_watch() {
        return;
    }
    assert!(!would_reuse(files, change, "file"), "reads: file");
    assert!(!would_reuse(files, change, "test"), "reads: test");
}

/// A known miss of `reads: test` (RFC 0004 §3.2): `reads: file` runs the test, `reads: test`
/// would reuse it. Whoever closes the miss changes the second assertion.
fn known_miss(files: &[(&str, &str)], change: (&str, &str)) {
    if !can_watch() {
        return;
    }
    assert!(!would_reuse(files, change, "file"), "reads: file");
    assert!(
        would_reuse(files, change, "test"),
        "reads: test: a known miss"
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_function_called() {
    caught(
        &[
            ("app/calc.py", "def f():\n    return 1\n"),
            (
                "app/facade.py",
                "from app import calc\n\ndef g():\n    return calc.f()\n",
            ),
            (
                "tests/test_a.py",
                "from app import facade\n\ndef test_target():\n    assert facade.g() == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/calc.py", "def f():\n    return 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_constant_the_code_that_ran_imports_directly() {
    caught(
        &[
            ("app/consts.py", "K = 1\n"),
            (
                "app/facade.py",
                "from app import consts\n\ndef g():\n    return consts.K\n",
            ),
            (
                "tests/test_a.py",
                "from app import facade\n\ndef test_target():\n    assert facade.g() == 1\n",
            ),
        ],
        ("app/consts.py", "K = 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_constant_the_test_file_imports_directly() {
    caught(
        &[
            ("app/consts.py", "K = 1\n"),
            (
                "tests/test_a.py",
                "from app.consts import K\n\ndef test_target():\n    assert K == 1\n",
            ),
        ],
        ("app/consts.py", "K = 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_file_opened() {
    caught(
        &[
            ("app/data.py", "1\n"),
            (
                "tests/test_a.py",
                "import os\n\ndef test_target():\n    with open(os.path.join('app', 'data.py')) as f:\n        assert f.read().strip() == '1'\n",
            ),
        ],
        ("app/data.py", "2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_file_a_module_read_once_and_kept() {
    // test_first opens the file through prices.py, which keeps what it read; test_target, after
    // it, gets the kept value and opens nothing.
    caught(
        &[
            ("app/data.py", "5\n"),
            (
                "app/prices.py",
                "import os\n_kept = None\n\ndef price():\n    global _kept\n    if _kept is None:\n        with open(os.path.join(os.path.dirname(__file__), 'data.py')) as f:\n            _kept = f.read().strip()\n    return _kept\n",
            ),
            (
                "tests/test_a.py",
                "from app import prices\n\ndef test_first():\n    assert prices.price()\n\ndef test_target():\n    assert prices.price() == '5'\n",
            ),
        ],
        ("app/data.py", "6\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_what_a_shared_fixture_read() {
    caught(
        &[
            ("app/helper.py", "def make():\n    return 1\n"),
            (
                "app/fx.py",
                "from app import helper\n\ndef make():\n    return helper.make()\n",
            ),
            (
                "tests/conftest.py",
                "import pytest\nfrom app import fx\n\n@pytest.fixture(scope='module')\ndef made():\n    return fx.make()\n",
            ),
            (
                "tests/test_a.py",
                "def test_target(made):\n    assert made == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/helper.py", "def make():\n    return 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_module_loaded_lazily_inside_a_test() {
    caught(
        &[
            ("app/lazy.py", "K = 1\n"),
            (
                "tests/test_a.py",
                "def test_target():\n    from app import lazy\n    assert lazy.K == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/lazy.py", "K = 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_module_code_outside_the_repository_loaded() {
    // pytest loads app/ext.py as a plugin, before any test; test_target reads it from
    // sys.modules, and nothing of the repository imports it.
    caught(
        &[
            (
                "pytest.ini",
                "[pytest]\ntestpaths = tests\npythonpath = .\naddopts = -p app.ext\n",
            ),
            ("app/ext.py", "VALUE = 1\n"),
            (
                "tests/test_a.py",
                "import sys\n\ndef test_target():\n    assert sys.modules['app.ext'].VALUE == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/ext.py", "VALUE = 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_fixture_a_test_asked_for_at_run_time() {
    // test_first requests the session fixture and sets it up; test_target asks for it with
    // `request.getfixturevalue`, as pytest-lazy-fixture does, and gets the value pytest kept.
    caught(
        &[
            ("app/helper.py", "def make():\n    return 1\n"),
            (
                "app/fx.py",
                "from app import helper\n\ndef make():\n    return helper.make()\n",
            ),
            (
                "tests/conftest.py",
                "import pytest\nfrom app import fx\n\n@pytest.fixture(scope='session')\ndef made():\n    return fx.make()\n",
            ),
            (
                "tests/test_a.py",
                "def test_first(made):\n    assert made\n\ndef test_target(request):\n    assert request.getfixturevalue('made') == 1\n",
            ),
        ],
        ("app/helper.py", "def make():\n    return 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_fixture_set_up_inside_a_tests_window() {
    // The first to ask for the session fixture asks at run time: its setup starts inside
    // test_target's window, and serves it.
    caught(
        &[
            ("app/helper.py", "def make():\n    return 1\n"),
            (
                "app/fx.py",
                "from app import helper\n\ndef make():\n    return helper.make()\n",
            ),
            (
                "tests/conftest.py",
                "import pytest\nfrom app import fx\n\n@pytest.fixture(scope='session')\ndef made():\n    return fx.make()\n",
            ),
            (
                "tests/test_a.py",
                "def test_target(request):\n    assert request.getfixturevalue('made') == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/helper.py", "def make():\n    return 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_what_a_session_hook_ran_outside_every_test() {
    // conftest.py's pytest_configure, before any test, calls into app/setup.py, which keeps what
    // app/calc.py computes; test_target reads the kept value, and runs neither.
    caught(
        &[
            ("app/state.py", "VALUE = None\n"),
            ("app/calc.py", "def value():\n    return 1\n"),
            (
                "app/setup.py",
                "from app import calc, state\n\ndef configure():\n    state.VALUE = calc.value()\n",
            ),
            (
                "tests/conftest.py",
                "from app import setup\n\ndef pytest_configure(config):\n    setup.configure()\n",
            ),
            (
                "tests/test_a.py",
                "from app import state\n\ndef test_target():\n    assert state.VALUE == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/calc.py", "def value():\n    return 2\n"),
    );
}

/// A package that re-exports: tax.py takes RATE from `app.config`, whose `__init__.py` binds it
/// from `.defaults`; no function of either runs. `head` is the re-exporting module's text.
fn reexported(head: (&'static str, &'static str)) -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/config/defaults.py", "RATE = 0.2\nCURRENCY = 'EUR'\n"),
        head,
        (
            "app/tax.py",
            "from app.config import RATE\n\ndef tax(amount):\n    return round(amount * RATE, 2)\n",
        ),
        (
            "tests/test_a.py",
            "from app.tax import tax\n\ndef test_target():\n    assert tax(10) == 2.0\n\ndef test_plain():\n    assert True\n",
        ),
    ]
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_what_a_packages_init_reexports() {
    caught(
        &reexported((
            "app/config/__init__.py",
            "from .defaults import RATE, CURRENCY\n",
        )),
        ("app/config/defaults.py", "RATE = 0.3\nCURRENCY = 'EUR'\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_star_import() {
    caught(
        &reexported((
            "app/config/__init__.py",
            "from app.config.defaults import *\n",
        )),
        ("app/config/defaults.py", "RATE = 0.3\nCURRENCY = 'EUR'\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_what_a_modules_all_reexports() {
    // A plain module, not a package, whose `__all__` names what it took from another.
    let mut files = reexported(("app/config/__init__.py", ""));
    files.push((
        "app/config/public.py",
        "from app.config.defaults import RATE\n__all__ = ['RATE']\n",
    ));
    files.retain(|(p, _)| *p != "app/tax.py");
    files.push((
        "app/tax.py",
        "from app.config.public import RATE\n\ndef tax(amount):\n    return round(amount * RATE, 2)\n",
    ));
    caught(
        &files,
        ("app/config/defaults.py", "RATE = 0.3\nCURRENCY = 'EUR'\n"),
    );
}

// covers: C-40
#[test]
fn a_known_miss_of_reads_test_a_constant_a_plain_module_passes_on() {
    // A plain module, neither a package's `__init__.py` nor one whose `__all__` names it, takes
    // RATE from defaults.py, and tax.py takes it from there: no re-export C-40 follows, and no
    // function of the way runs. Still the first known miss, two imports away.
    let mut files = reexported(("app/config/__init__.py", ""));
    files.push((
        "app/config/middle.py",
        "from app.config.defaults import RATE\n",
    ));
    files.retain(|(p, _)| *p != "app/tax.py");
    files.push((
        "app/tax.py",
        "from app.config.middle import RATE\n\ndef tax(amount):\n    return round(amount * RATE, 2)\n",
    ));
    known_miss(
        &files,
        ("app/config/defaults.py", "RATE = 0.3\nCURRENCY = 'EUR'\n"),
    );
}

/// Two imports away: cart.py ran and imports settings.py, which built TAX from defaults.py, with
/// no function of either called.
fn two_imports_away() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/defaults.py", "RATE = 3\n"),
        (
            "app/settings.py",
            "from app.defaults import RATE\nTAX = RATE * 2\n",
        ),
        (
            "app/cart.py",
            "from app.settings import TAX\n\ndef total(xs):\n    return sum(xs) + TAX\n",
        ),
        (
            "tests/test_a.py",
            "from app.cart import total\n\ndef test_target():\n    assert total([1]) == 7\n",
        ),
    ]
}

// covers: C-40
#[test]
fn a_known_miss_of_reads_test_a_value_two_imports_away() {
    known_miss(&two_imports_away(), ("app/defaults.py", "RATE = 4\n"));
}

/// A registry plugins.py fills as app/__init__.py imports it: the test file imports the registry
/// and nothing of plugins.py, whose code runs at import only. An `import` statement binds no name
/// it re-exports (a `from app import plugins` in a package's `__init__.py` would, C-40).
fn a_registry() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/__init__.py", "import app.plugins\n"),
        ("app/registry.py", "HANDLERS = {}\n"),
        (
            "app/plugins.py",
            "from app.registry import HANDLERS\nHANDLERS['x'] = 41\n",
        ),
        (
            "tests/test_a.py",
            "from app.registry import HANDLERS\n\ndef test_target():\n    assert HANDLERS['x'] == 41\n",
        ),
    ]
}

// covers: C-40
#[test]
fn a_known_miss_of_reads_test_a_registry_another_file_filled_at_import() {
    known_miss(
        &a_registry(),
        (
            "app/plugins.py",
            "from app.registry import HANDLERS\nHANDLERS['x'] = 42\n",
        ),
    );
}

/// A value test_first computed through rates.py, which a `functools.cache` wrapper (C code, so no
/// function of the way starts again) keeps: test_target, after it, takes it from the cache, and
/// runs nothing of rates.py. Which test misses it depends on the order they run in.
fn a_value_a_cache_kept() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/rates.py", "def rate():\n    return 1\n"),
        (
            "app/heavy.py",
            "import functools\nfrom app import rates\n\n@functools.cache\ndef value():\n    return rates.rate()\n",
        ),
        (
            "tests/test_a.py",
            "from app import heavy\n\ndef test_first():\n    assert heavy.value()\n\ndef test_target():\n    assert heavy.value() == 1\n",
        ),
    ]
}

// covers: C-40
#[test]
fn a_known_miss_of_reads_test_a_value_an_earlier_test_computed_and_a_cache_kept() {
    known_miss(
        &a_value_a_cache_kept(),
        ("app/rates.py", "def rate():\n    return 2\n"),
    );
}

// covers: C-40, C-34
#[test]
fn an_audit_catches_a_known_miss_of_reads_test_and_suspends_reuse() {
    if !can_watch() {
        return;
    }
    // test_target is the one test that could be reused one by one: it runs as a check, fails,
    // runs once more alone, fails again, and the check's reuse is suspended.
    let r = repo_of(&a_registry(), "{mode: reuse, unit: test, reads: test}");
    let python = python();
    let first = r.run(&pytest_args(&python), &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    r.write(
        "app/plugins.py",
        "from app.registry import HANDLERS\nHANDLERS['x'] = 42\n",
    );
    let caught = r.run(&pytest_args(&python), &[]);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert!(
        caught.stderr.contains(
            "hushrun: unit: tests/test_a.py::test_target failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(
        store::disagreements(&r.cache),
        ["tests/test_a.py::test_target"]
    );
}

// covers: C-40, C-42, C-36
#[test]
fn with_reads_test_a_test_is_reused_on_its_own_list_and_its_file_records_each_test() {
    if !can_watch() {
        return;
    }
    // One test of a file runs app/a.py's code through app/wa.py, sixty others app/b.py's through
    // app/wb.py; the test file imports both wrappers, which every test reads: a change to a.py
    // runs only the first test, and two of the sixty, chosen by their keys, as a check.
    let files = [
        ("app/a.py", "def f():\n    return 1\n"),
        ("app/b.py", "def f():\n    return 1\n"),
        (
            "app/wa.py",
            "from app import a\n\ndef f():\n    return a.f()\n",
        ),
        (
            "app/wb.py",
            "from app import b\n\ndef f():\n    return b.f()\n",
        ),
        (
            "tests/test_a.py",
            "import pytest\nfrom app import wa, wb\n\ndef test_one():\n    assert wa.f() == 1\n\n@pytest.mark.parametrize('n', range(60))\ndef test_two(n):\n    assert wb.f() == 1\n",
        ),
    ];
    let r = repo_of(&files, "{mode: reuse, unit: test, reads: test}");
    let python = python();
    let first = r.run(&pytest_args(&python), &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    // A test file that passed whole records its own pass and each test's.
    let map = store::map(&r.cache);
    assert_eq!(
        map["units"]["tests/test_a.py"]["tests"]
            .as_object()
            .unwrap()
            .len(),
        61
    );
    assert_eq!(r.unit_results(), 2 + 61 + 1);
    r.write("app/a.py", "def f():\n    return 1  # changed\n");
    let second = r.run(&pytest_args(&python), &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(
        second.stderr.contains(
            "hushrun: unit: 4 of 62 tests run, in 2 of 2 test files; 58 already passed on the files they read, and 3 more that already passed run anyway, as a check"
        ),
        "{}",
        second.stderr
    );
    assert_eq!(
        r.newest_result()["units"]["tests"],
        json!({"total": 62, "ran": 4, "reused": 58})
    );
    // With `reads: file` the same change runs every test of the file.
    r.write(
        "hushrun.yaml",
        &yaml("{mode: reuse, unit: test, reads: file}"),
    );
    r.write("app/a.py", "def f():\n    return 1  # changed again\n");
    let whole = r.run(&pytest_args(&python), &[]);
    assert_eq!(whole.code, 0, "{}", whole.stderr);
    assert!(
        !whole.stderr.contains("58 already passed"),
        "{}",
        whole.stderr
    );
}
