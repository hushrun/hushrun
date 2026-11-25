//! \[ADR 0068\] `tests: observe` (C-5, C-30 to C-33): every test file runs as before, hushrun
//! records what each read and how it ended, and says what `tests: reuse` would have done.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- python -m pytest …` would, through pytest's adapter's own type, `pytest::Pytest`
//! (`common/pytest.rs`), and the code every adapter shares, and never starts the `hushrun` binary
//! (`common/in_process.rs`). pytest runs as COMMAND, for real. What only the CLI shows (`tests`
//! off, a signal, how the CLI chooses an adapter, the import of an older store, `hushrun why`, the
//! sweep of old results) is in `e2e_pytest/observe.rs`; Cargo's tests, whose target runner is the
//! binary itself, are in `e2e_cargo/observe.rs`.
//!
//! The fixtures need Python with pytest and pytest-xdist. They say so and pass where those are
//! missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets: then a missing tool fails them.
//! `HUSHRUN_TEST_PYTHON` names the Python, `python3` by default.

#[path = "common/observe.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/pytest.rs"]
mod pytest;
#[path = "common/python.rs"]
mod python;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use fixtures::*;
use in_process::{Ran, Repo};
use pytest::{pytest_command, repo_of, run};
use python::{has_pytest, python, tools};

/// The `observe` line: what `tests: reuse` would have done.
fn observe_line(ran: &Ran) -> &str {
    ran.lines
        .iter()
        .find(|l| l.contains("with `tests: reuse`"))
        .unwrap_or_else(|| panic!("no observe line in:\n{}", ran.said()))
}

/// What one module read, as this machine's store keeps the check's map (ADR 0072).
fn reads(r: &Repo, unit: &str) -> Vec<String> {
    let map = r.map();
    map["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// How many unit results this machine holds.
fn unit_results(r: &Repo) -> usize {
    let dir = r.cache.join("results/units/v0");
    let Ok(keys) = std::fs::read_dir(dir) else {
        return 0;
    };
    keys.flatten()
        .map(|k| std::fs::read_dir(k.path()).map_or(0, |files| files.count()))
        .sum()
}

/// The run directories a stopped or finished run left behind.
fn run_dirs(r: &Repo) -> usize {
    std::fs::read_dir(r.cache.join("units/runs")).map_or(0, |d| d.count())
}

// covers: C-5, C-32
#[cfg(unix)]
#[test]
fn with_tests_observe_command_gets_the_adapter_beside_what_it_had() {
    // With `tests: observe`, COMMAND gets the adapter, beside what it had; and the run's directory
    // is gone afterwards. (With `tests` off, nothing changes: e2e_pytest/observe.rs.)
    let r = repo_of(&[("src/a.txt", "a\n"), ("hushrun.yaml", YAML_OBSERVE)]);
    let ran = run(
        &r,
        &["sh", "-c", "env | sort"],
        &[("PYTHONPATH", "/kept"), ("PYTEST_ADDOPTS", "-q")],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(ran.stdout.contains("\nHUSHRUN_UNITS="), "{}", ran.stdout);
    assert!(
        ran.stdout
            .contains("\nPYTEST_ADDOPTS=-q -p hushrun_pytest\n"),
        "{}",
        ran.stdout
    );
    assert!(ran.stdout.contains(":/kept\n"), "{}", ran.stdout);
    assert_eq!(run_dirs(&r), 0);
    // A COMMAND that wrote no facts leaves the check as it was: no line about test files.
    assert!(!ran.said().contains("would have run"), "{}", ran.said());
}

/// The fixture of RFC 0003's milestone 1, one test file per hazard of §3.2 and §3.3.
fn pytest_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "pytest.ini",
            "[pytest]\ntestpaths = tests\npythonpath = . tests\n",
        ),
        ("requirements.txt", "nothing==1.0\n"),
        ("app/__init__.py", ""),
        // A value built at import time: no line of settings.py runs during any test.
        ("app/settings.py", "LIMIT = 3\n"),
        (
            "app/consumer.py",
            "from app.settings import LIMIT\nDOUBLE = LIMIT * 2\n",
        ),
        ("app/helper.py", "X = 1\n"),
        (
            "app/lazy.py",
            "def value():\n    import app.helper\n    return app.helper.X\n",
        ),
        // Read once and kept: the second test file that asks opens nothing.
        (
            "app/cache.py",
            "import functools, json, os\nHERE = os.path.dirname(__file__)\n\n@functools.lru_cache()\ndef table():\n    with open(os.path.join(HERE, \"table.py\")) as f:\n        return json.load(f)\n",
        ),
        ("app/table.py", "{\"k\": 1}\n"),
        ("app/plugin.py", "NAME = \"plugin-one\"\n"),
        ("app/secret.py", "SECRET = 7\n"),
        ("tests/data.txt", "hello\n"),
        (
            "tests/test_a_settings.py",
            "from app.settings import LIMIT\nfrom app import consumer\n\ndef test_limit():\n    assert LIMIT == 3\n",
        ),
        (
            "tests/test_b_settings_again.py",
            "import app.settings\n\ndef test_limit_again():\n    assert app.settings.LIMIT == 3\n",
        ),
        (
            "tests/test_c_through_consumer.py",
            "from app import consumer\n\ndef test_double():\n    assert consumer.DOUBLE == 6\n",
        ),
        (
            "tests/test_d_lazy.py",
            "def test_lazy_import():\n    from app import lazy\n    assert lazy.value() == 1\n",
        ),
        (
            "tests/test_e_data.py",
            "import os\n\ndef test_reads_data():\n    with open(os.path.join(os.path.dirname(__file__), \"data.txt\")) as f:\n        assert f.read().strip() == \"hello\"\n",
        ),
        (
            "tests/test_f_cache_first.py",
            "from app import cache\n\ndef test_cache_first():\n    assert cache.table() == {\"k\": 1}\n",
        ),
        (
            "tests/test_g_cache_second.py",
            "from app import cache\n\ndef test_cache_second():\n    assert cache.table()[\"k\"] == 1\n",
        ),
        (
            "tests/test_h_imports_test.py",
            "from test_a_settings import LIMIT\n\ndef test_imports_another_test_file():\n    assert LIMIT == 3\n",
        ),
        (
            "tests/test_i_subprocess.py",
            "import os, subprocess, sys\n\ndef test_child_reads_secret():\n    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))\n    out = subprocess.run([sys.executable, \"-c\", \"import app.secret as s; print(s.SECRET)\"], cwd=root, stdout=subprocess.PIPE, check=True).stdout\n    assert out.strip() == b\"7\"\n",
        ),
        (
            "tests/test_j_registry_load.py",
            "import registry\n\ndef test_load_plugin():\n    assert registry.load(\"app.plugin\").NAME == \"plugin-one\"\n",
        ),
        (
            "tests/test_k_registry_use.py",
            "import registry\n\ndef test_use_loaded_plugin():\n    assert registry.get(\"app.plugin\").NAME == \"plugin-one\"\n",
        ),
    ]
}

/// A library outside the repository that keeps a repository module it loaded, as a plugin
/// registry or a framework's settings loader does. It lives under a directory named `py`, as
/// pytest's own code once did: a name in a path must never make it pytest's.
const REGISTRY: &str = "import importlib\n_LOADED = {}\n\ndef load(name):\n    _LOADED[name] = importlib.import_module(name)\n    return _LOADED[name]\n\ndef get(name):\n    return _LOADED[name]\n";

// covers: C-30, C-31, C-32, C-33
#[test]
fn pytest_test_files_are_observed_as_the_spec_reads_them() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&pytest_files());
    let site = r.root.join("py/site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("registry.py"), REGISTRY).unwrap();
    let site = site.to_string_lossy().into_owned();
    let env = [("PYTHONPATH", site.as_str())];
    let python = python();
    // `hushrun run [--force] unit -- pytest …`: in-process, every run starts COMMAND, as a forced
    // one does; the check's own skip is the CLI's.
    let pytest =
        |extra: &[&str], _force: bool| -> Ran { run(&r, &pytest_command(&python, extra), &env) };

    // The first run: every test file would have run, and each one's reads are in the map.
    let first = pytest(&[], false);
    assert_eq!(first.code, 0, "{}", first.said());
    assert!(
        observe_line(&first).contains("all 11 test files would have run: none had passed here yet"),
        "{}",
        first.said()
    );
    // An import-time constant, by a test file that imported it first and by one that found it
    // already loaded.
    assert!(reads(&r, "tests/test_a_settings.py").contains(&"app/settings.py".into()));
    assert!(reads(&r, "tests/test_b_settings_again.py").contains(&"app/settings.py".into()));
    assert!(reads(&r, "tests/test_c_through_consumer.py").contains(&"app/settings.py".into()));
    // A lazy import, inside a function.
    assert!(reads(&r, "tests/test_d_lazy.py").contains(&"app/helper.py".into()));
    // A file read once and kept: it reaches the test file that got the cached value.
    assert!(reads(&r, "tests/test_f_cache_first.py").contains(&"app/table.py".into()));
    assert!(reads(&r, "tests/test_g_cache_second.py").contains(&"app/table.py".into()));
    // A test file that imports another test file.
    assert!(reads(&r, "tests/test_h_imports_test.py").contains(&"tests/test_a_settings.py".into()));
    // A module a library outside the repository loaded and kept: ambient for later test files.
    assert!(reads(&r, "tests/test_k_registry_use.py").contains(&"app/plugin.py".into()));
    // A subprocess: not followed, so it always runs.
    assert_eq!(
        r.map()["units"]["tests/test_i_subprocess.py"]["notFollowed"],
        serde_json::json!(["subprocess"])
    );
    assert!(
        first.said().contains("a process it started"),
        "{}",
        first.said()
    );
    assert_eq!(unit_results(&r), 10, "every observed test file that passed");

    // The same files again: only the one that starts a process would run.
    let again = pytest(&[], true);
    assert!(
        observe_line(&again).contains("1 of 11 test files would have run; 10 already passed"),
        "{}",
        again.said()
    );

    // The constant changes: the four test files that read it run, and fail. pytest's exit code is
    // hushrun's, and the seven that passed are recorded all the same. The new text has another
    // length: Python trusts a module's cached bytecode while its file's size and mtime, in whole
    // seconds, are as they were, and on Linux the change and its undoing can share a second.
    r.write("app/settings.py", "LIMIT = 40\n");
    let changed = pytest(&[], false);
    assert_eq!(changed.code, 1, "{}", changed.said());
    assert!(
        observe_line(&changed).contains("5 of 11 test files would have run; 6 already passed"),
        "{}",
        changed.said()
    );
    assert!(!changed.said().contains("though it passed before"));
    r.write("app/settings.py", "LIMIT = 3\n");

    // A data file the tests open is in the rest set: any test may read it.
    r.write("tests/data.txt", "hello\n\n");
    let data = pytest(&[], false);
    assert!(
        observe_line(&data).contains(
            "all 11 test files would have run: tests/data.txt changed, and any test may read it"
        ),
        "{}",
        data.said()
    );
    r.write("tests/data.txt", "hello\n");
    // Back as before: forced, so that the map learns it again.
    pytest(&[], true);

    // A changed lockfile runs everything.
    r.write("requirements.txt", "nothing==2.0\n");
    let lock = pytest(&[], false);
    assert!(
        observe_line(&lock).contains("all 11 test files would have run: requirements.txt changed"),
        "{}",
        lock.said()
    );
    r.write("requirements.txt", "nothing==1.0\n");
    // Back as before: forced, so that the map learns it again.
    pytest(&[], true);

    // A new conftest.py changes what tests load: shape.
    r.write("tests/conftest.py", "\n");
    let shape = pytest(&[], false);
    assert!(
        observe_line(&shape)
            .contains("all 11 test files would have run: tests/conftest.py is new, which can change what tests load"),
        "{}",
        shape.said()
    );
    std::fs::remove_file(r.repo.join("tests/conftest.py")).unwrap();
    // Back as before: forced, so that the map learns it again.
    pytest(&[], true);

    // A collection error: pytest stops before any test, and only the broken file records (fail).
    r.write("tests/test_z_broken.py", "def test_x(:\n");
    let before = unit_results(&r);
    let broken = pytest(&[], false);
    assert_eq!(broken.code, 2, "{}", broken.said());
    assert_eq!(unit_results(&r), before + 1, "{}", broken.said());
    std::fs::remove_file(r.repo.join("tests/test_z_broken.py")).unwrap();

    // xdist: each worker collects every test file and runs some of their tests; their facts merge
    // into the same answer. (The registry's second test file needs the first in its own process,
    // so it is deselected here: a test file with no test selected passes.)
    let xdist = [
        "-n",
        "2",
        "--deselect",
        "tests/test_k_registry_use.py::test_use_loaded_plugin",
    ];
    let first_xdist = pytest(&xdist, false);
    assert_eq!(first_xdist.code, 0, "{}", first_xdist.said());
    let xdist = pytest(&xdist, true);
    assert_eq!(xdist.code, 0, "{}", xdist.said());
    assert!(
        observe_line(&xdist).contains("1 of 11 test files would have run; 10 already passed"),
        "{}",
        xdist.said()
    );
    assert_eq!(run_dirs(&r), 0);
}

// covers: C-32, C-33
#[test]
fn a_test_file_that_would_have_been_reused_and_fails_is_a_disagreement() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\nflag\n"),
        ("lib.py", "X = 1\n"),
        // It fails when a file outside the scope says so: what no adapter can see.
        (
            "test_flaky.py",
            "import os, lib\n\ndef test_x():\n    assert not os.path.exists(os.path.join(os.path.dirname(__file__), 'flag'))\n",
        ),
        (
            "test_other.py",
            "import lib\n\ndef test_y():\n    assert lib.X == 1\n",
        ),
    ]);
    let python = python();
    // In-process, every run starts COMMAND, as `hushrun run --force` does.
    let pytest = || run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    r.write("flag", "");
    let failed = pytest();
    assert_eq!(failed.code, 1, "{}", failed.said());
    assert!(
        failed.said().contains(
            "unit: test_flaky.py failed, though it passed before on the same files it read: with `tests: reuse` it would have been reused"
        ),
        "{}",
        failed.said()
    );
    // \[ADR 0072\] The audit, in this machine's store.
    assert_eq!(r.disagreements(), ["test_flaky.py"]);
}

// covers: C-32
#[cfg(unix)]
#[test]
fn two_starts_of_pytest_in_one_command_never_add_up_to_a_test_file_that_ran_whole() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // The first pytest runs test_one; the second selects test_two and test_three, and test_two
    // ends the session before test_three starts, with exit 0. One of two, then one of two: summed,
    // two of two finished, and test_three never ran.
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "test_a.py",
            "import os, pytest\n\ndef test_one():\n    pass\n\ndef test_two():\n    if os.environ.get('STOP'):\n        pytest.exit('stop', returncode=0)\n\ndef test_three():\n    pass\n",
        ),
    ]);
    let python = python();
    let pytest = format!("{python} -m pytest -q -p no:cacheprovider test_a.py");
    let line = format!("{pytest} -k test_one && STOP=1 {pytest} -k 'test_two or test_three'");
    let ran = run(&r, &["sh", "-c", &line], &[]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(ran.stdout.contains("1 passed"), "{}", ran.stdout);
    // It didn't run whole: no pass of it is recorded.
    assert_eq!(unit_results(&r), 0, "{}", ran.said());
}

// covers: C-32
#[test]
fn a_relative_path_opened_after_a_chdir_is_the_file_it_names_there() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // The test file opens `data.py` from the root as it loads, then its test opens `data.py`
    // from `sub/`: the same string, another file, which its list must hold.
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("data.py", "0\n"),
        ("sub/data.py", "1\n"),
        (
            "test_a.py",
            "import os\n\nwith open('data.py') as f:\n    ROOT = f.read()\n\ndef test_target():\n    os.chdir('sub')\n    try:\n        with open('data.py') as f:\n            assert f.read().strip() == '1'\n    finally:\n        os.chdir('..')\n",
        ),
        ("test_other.py", "def test_other():\n    assert True\n"),
    ]);
    let python = python();
    let args = pytest_command(&python, &[]);
    let first = run(&r, &args, &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    assert!(
        reads(&r, "test_a.py").contains(&"sub/data.py".to_string()),
        "{:?}",
        reads(&r, "test_a.py")
    );
    r.write("sub/data.py", "2\n");
    let second = run(&r, &args, &[]);
    assert_eq!(second.code, 1, "{}", second.stdout);
    assert!(
        !second.said().contains("it would have been reused"),
        "{}",
        second.said()
    );
}

// covers: C-32
#[test]
fn a_test_file_whose_last_test_pytest_exit_cut_short_didnt_run_whole() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_db, the file's last, asks for a fixture that ends the session: two of two tests
    // started, one finished. Neither file runs whole: nothing records a pass.
    let r = repo_of(&cut_short());
    let python = python();
    let ran = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(ran.code, 3, "{}", ran.said());
    assert_eq!(unit_results(&r), 0, "{}", ran.said());
    // \[C-37\] What `hushrun why` says of it is end to end: e2e_pytest/observe.rs.
}

// covers: C-32
#[test]
fn a_session_fixture_whose_end_fails_keeps_every_file_it_served_from_a_pass() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // A session fixture two files use fails as it ends, after the last test: pytest reports it on
    // test_c.py's test, which didn't use it; test_a.py and test_b.py, which it served, record no
    // pass either.
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "conftest.py",
            "import pytest\n\n@pytest.fixture(scope='session')\ndef db():\n    yield 1\n    raise RuntimeError('the database was left dirty')\n",
        ),
        ("test_a.py", "def test_a(db):\n    pass\n"),
        ("test_b.py", "def test_b(db):\n    pass\n"),
        ("test_c.py", "def test_c():\n    pass\n"),
    ]);
    let python = python();
    let ran = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(ran.code, 1, "{}", ran.said());
    // test_c.py records its fail, and nothing else is recorded.
    assert_eq!(unit_results(&r), 1, "{}", ran.said());
}

// covers: C-32
#[test]
fn a_test_file_skipped_whole_as_it_loads_records_no_pass_in_a_run_that_failed() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_mod.py skips itself as it loads, test_imp.py skips for a module that isn't there, and
    // test_fail.py fails: in that run a skip proves nothing, of a whole file too.
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "test_mod.py",
            "import pytest\npytest.skip('module level', allow_module_level=True)\n\ndef test_m():\n    pass\n",
        ),
        (
            "test_imp.py",
            "import pytest\npytest.importorskip('numpy_not_here')\n\ndef test_np():\n    pass\n",
        ),
        ("test_fail.py", "def test_fail():\n    assert False\n"),
    ]);
    let python = python();
    let ran = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(ran.code, 1, "{}", ran.said());
    // test_fail.py's fail, and nothing else.
    assert_eq!(unit_results(&r), 1, "{}", ran.said());
}

// covers: C-32
#[cfg(unix)]
#[test]
fn a_start_of_the_runner_that_dies_before_its_facts_keeps_every_pass_out() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // The first pytest passes; the second dies inside test_2 and writes nothing: the file's pass
    // from the first can't be told from what the second would have said.
    let r = repo_of(&runner_dies());
    let line = runner_dies_line(&python());
    let ran = run(&r, &["sh", "-c", &line], &[]);
    assert_ne!(ran.code, 0, "{}", ran.said());
    assert_eq!(unit_results(&r), 0, "{}", ran.said());
    assert!(
        ran.lines.iter().any(|l| l
            == "unit: no test file or test records a pass: a start of its runner ended before it said what it saw"),
        "{}",
        ran.said()
    );
    // \[C-37\] What `hushrun why` says of it is end to end: e2e_pytest/observe.rs.
}

// covers: C-30, C-33
#[test]
fn observe_counts_the_test_files_the_plan_counts() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // pytest's doctest plugin collects `test*.txt` files too: tests/res/test.txt, a data file,
    // holds no doctest and is no test file of python_files, which a plan counts. `observe` counts
    // the same two.
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("pytest.ini", "[pytest]\ntestpaths = tests\n"),
        ("tests/test_a.py", "def test_a():\n    pass\n"),
        ("tests/test_b.py", "def test_b():\n    pass\n"),
        ("tests/res/test.txt", "some data\n"),
    ]);
    let python = python();
    let ran = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.lines
            .iter()
            .any(|l| l.starts_with("unit: with `tests: reuse`, all 2 test files would have run")),
        "{}",
        ran.said()
    );
}

// covers: C-32
#[test]
fn a_pytest_a_test_starts_and_stops_is_no_start_of_the_runner() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_nested starts a pytest of its own, which inherits the run's variables, and stops it
    // before it says anything: that pytest is the test's doing, which isn't followed, never a
    // start of the runner by COMMAND. The run's passes are kept.
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "test_n.py",
            "import os, subprocess, sys, time\n\ndef test_nested():\n    p = subprocess.Popen([sys.executable, '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'slow_s.py'], env=dict(os.environ, IN_CHILD='1'))\n    time.sleep(3)\n    p.terminate()\n    p.wait()\n",
        ),
        (
            "slow_s.py",
            "import os, time\n\ndef test_s():\n    if os.environ.get('IN_CHILD'):\n        time.sleep(30)\n",
        ),
        ("test_b.py", "def test_b():\n    pass\n"),
    ]);
    let python = python();
    let ran = run(
        &r,
        &pytest_command(&python, &["test_n.py", "test_b.py"]),
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        !ran.said().contains("no test file or test records a pass"),
        "{}",
        ran.said()
    );
    assert!(unit_results(&r) > 0, "{}", ran.said());
}

// covers: C-33
#[test]
fn two_repositories_with_one_check_keep_their_maps_apart() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let shop = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("lib.py", "X = 1\n"),
        (
            "test_lib.py",
            "import lib\n\ndef test_x():\n    assert lib.X == 1\n",
        ),
    ]);
    let mut other = repo_of(&[
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/other\nchecks:\n  unit:\n    tests: observe\n",
        ),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("lib.py", "X = 2\n"),
        (
            "test_lib.py",
            "import lib\n\ndef test_x():\n    assert lib.X == 2\n",
        ),
    ]);
    // One cache directory, as one machine has, for both: same check, same command.
    other.cache = shop.cache.clone();
    let python = python();
    let args = pytest_command(&python, &[]);
    let first = run(&shop, &args, &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    let second = run(&other, &args, &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    // The other repository's map isn't this one's: nothing of the first says why.
    assert_eq!(
        observe_line(&second),
        "unit: with `tests: reuse`, all 1 test file would have run: none had passed here yet on the files it read"
    );
    // \[ADR 0072\] One store per repository.
    assert_eq!(store::stores(&shop.cache).len(), 2);
}
