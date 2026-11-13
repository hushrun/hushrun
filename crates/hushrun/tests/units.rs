//! \[ADR 0068\] `tests: observe` (C-5, C-30 to C-33): every test file runs as before, hushrun
//! records what each read and how it ended, and says what `tests: reuse` would have done. With
//! `tests` off, nothing changes.
//!
//! The pytest and Cargo fixtures need Python with pytest and pytest-xdist, and cargo-nextest. They
//! say so and pass where those are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets:
//! then a missing tool fails them. `HUSHRUN_TEST_PYTHON` names the Python, `python3` by default.

#[path = "common/store.rs"]
mod store;

use std::path::PathBuf;
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

/// Environment variables a test must never inherit.
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
    "PYTEST_ADDOPTS",
    "PYTHONPATH",
    "CARGO_TARGET_DIR",
    "npm_lifecycle_event",
    // B-1, B-2: the developer's build mode, store and wrapper stay out: a test run that cargo
    // itself compiles through the wrapper doesn't pass its session on to a fixture's cargo.
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
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

    fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// `hushrun` in the repository, as a local run with no inherited configuration, and no
    /// variable an outer Cargo or nextest set, which a Cargo it starts would read.
    fn hushrun(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", self.cache())
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
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

    /// The check's map, as this cache's store holds it (ADR 0072).
    fn map(&self) -> Value {
        store::map(&self.cache())
    }

    fn reads(&self, unit: &str) -> Vec<String> {
        let map = self.map();
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    /// How many unit results this machine holds.
    fn unit_results(&self) -> usize {
        let dir = self.cache().join("results/units/v0");
        let Ok(keys) = std::fs::read_dir(dir) else {
            return 0;
        };
        keys.flatten()
            .map(|k| std::fs::read_dir(k.path()).map_or(0, |files| files.count()))
            .sum()
    }

    /// The run directories a stopped or finished run left behind.
    fn run_dirs(&self) -> usize {
        std::fs::read_dir(self.cache().join("units/runs")).map_or(0, |d| d.count())
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The `observe` line: what `tests: reuse` would have done.
    fn observe_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| l.contains("with `tests: reuse`"))
            .unwrap_or_else(|| panic!("no observe line in:\n{}", self.stderr))
    }
}

/// Whether the tools a fixture needs are here: else it says so and the test passes, unless CI
/// requires them.
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

fn has_nextest() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

const YAML_OBSERVE: &str =
    "version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: observe\n";

// covers: C-5, C-32
#[cfg(unix)]
#[test]
fn with_tests_off_command_gets_nothing_new() {
    let print_env = ["run", "unit", "--", "sh", "-c", "env | sort"];
    for yaml in [
        None,
        Some("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    run: x\n"),
        Some("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: off\n"),
    ] {
        let mut files = vec![("src/a.txt", "a\n")];
        if let Some(yaml) = yaml {
            files.push(("hushrun.yaml", yaml));
        }
        let r = Repo::new(&files);
        let ran = r.run(
            &print_env,
            &[("PYTHONPATH", "/kept"), ("PYTEST_ADDOPTS", "-q")],
        );
        assert_eq!(ran.code, 0, "{}", ran.stderr);
        let names: Vec<&str> = ran
            .stdout
            .lines()
            .filter_map(|l| l.split_once('=').map(|(n, _)| n))
            .collect();
        for line in ran.stdout.lines() {
            let runner = line.starts_with("CARGO_TARGET_") && line.contains("_RUNNER=");
            assert!(!line.starts_with("HUSHRUN_UNITS=") && !runner, "{line}");
            // \[ADR 0070, 0071\] Nor what loads the Go and Vitest adapters.
            assert!(
                !line.starts_with("HUSHRUN_VITEST=")
                    && !line.starts_with("HUSHRUN_GOFLAGS=")
                    && !line.starts_with("GOFLAGS="),
                "{line}"
            );
        }
        // What the environment said, as it said it.
        assert!(
            ran.stdout.contains("\nPYTHONPATH=/kept\n"),
            "{}",
            ran.stdout
        );
        assert!(
            ran.stdout.contains("\nPYTEST_ADDOPTS=-q\n"),
            "{}",
            ran.stdout
        );
        assert!(names.contains(&"HUSHRUN_INSIDE_RUN"));
        // No file of hushrun's for test files.
        assert!(!r.cache().join("units").exists());
        assert!(!r.cache().join("adapters").exists());
        assert!(!r.cache().join("results/units").exists());
        assert!(!ran.stderr.contains("test file"), "{}", ran.stderr);
    }
    // With `tests: observe`, COMMAND gets the adapter, beside what it had; and the run's directory
    // is gone afterwards.
    let r = Repo::new(&[("src/a.txt", "a\n"), ("hushrun.yaml", YAML_OBSERVE)]);
    let ran = r.run(
        &print_env,
        &[("PYTHONPATH", "/kept"), ("PYTEST_ADDOPTS", "-q")],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.contains("\nHUSHRUN_UNITS="), "{}", ran.stdout);
    assert!(
        ran.stdout
            .contains("\nPYTEST_ADDOPTS=-q -p hushrun_pytest\n"),
        "{}",
        ran.stdout
    );
    assert!(ran.stdout.contains(":/kept\n"), "{}", ran.stdout);
    assert_eq!(r.run_dirs(), 0);
    // A COMMAND that wrote no facts leaves the check as it was: no line about test files.
    assert!(!ran.stderr.contains("would have run"), "{}", ran.stderr);
}

// covers: C-32
#[cfg(unix)]
#[test]
fn a_run_a_signal_stops_leaves_no_directory() {
    let yaml = "version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {mode: reuse}\n";
    let r = Repo::new(&[("src/a.txt", "a\n"), ("hushrun.yaml", yaml)]);
    let ran = r.run(&["run", "unit", "--", "sh", "-c", "kill -TERM $$"], &[]);
    assert_eq!(ran.code, 128 + 15, "{}", ran.stderr);
    // \[ADR 0069\] Reuse is in this version: with no test file, it says nothing of them.
    assert!(!ran.stderr.contains("test file"), "{}", ran.stderr);
    assert_eq!(r.run_dirs(), 0);
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
    let r = Repo::new(&pytest_files());
    let site = r.root.join("py/site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("registry.py"), REGISTRY).unwrap();
    let site = site.to_string_lossy().into_owned();
    let env = [("PYTHONPATH", site.as_str())];
    let python = python();
    let pytest = |extra: &[&str], force: bool| -> Ran {
        let mut args = vec!["run"];
        if force {
            args.push("--force");
        }
        args.extend([
            "unit",
            "--",
            &python,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
        ]);
        args.extend(extra);
        r.run(&args, &env)
    };

    // The first run: every test file would have run, and each one's reads are in the map.
    let first = pytest(&[], false);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(
        first
            .observe_line()
            .contains("all 11 test files would have run: none had passed here yet"),
        "{}",
        first.stderr
    );
    // An import-time constant, by a test file that imported it first and by one that found it
    // already loaded.
    assert!(
        r.reads("tests/test_a_settings.py")
            .contains(&"app/settings.py".into())
    );
    assert!(
        r.reads("tests/test_b_settings_again.py")
            .contains(&"app/settings.py".into())
    );
    assert!(
        r.reads("tests/test_c_through_consumer.py")
            .contains(&"app/settings.py".into())
    );
    // A lazy import, inside a function.
    assert!(
        r.reads("tests/test_d_lazy.py")
            .contains(&"app/helper.py".into())
    );
    // A file read once and kept: it reaches the test file that got the cached value.
    assert!(
        r.reads("tests/test_f_cache_first.py")
            .contains(&"app/table.py".into())
    );
    assert!(
        r.reads("tests/test_g_cache_second.py")
            .contains(&"app/table.py".into())
    );
    // A test file that imports another test file.
    assert!(
        r.reads("tests/test_h_imports_test.py")
            .contains(&"tests/test_a_settings.py".into())
    );
    // A module a library outside the repository loaded and kept: ambient for later test files.
    assert!(
        r.reads("tests/test_k_registry_use.py")
            .contains(&"app/plugin.py".into())
    );
    // A subprocess: not followed, so it always runs.
    assert_eq!(
        r.map()["units"]["tests/test_i_subprocess.py"]["notFollowed"],
        serde_json::json!(["subprocess"])
    );
    assert!(
        first.stderr.contains("a process it started"),
        "{}",
        first.stderr
    );
    assert_eq!(r.unit_results(), 10, "every observed test file that passed");

    // The same files again: only the one that starts a process would run.
    let again = pytest(&[], true);
    assert!(
        again
            .observe_line()
            .contains("1 of 11 test files would have run; 10 already passed"),
        "{}",
        again.stderr
    );

    // The constant changes: the four test files that read it run, and fail. pytest's exit code is
    // hushrun's, and the seven that passed are recorded all the same. The new text has another
    // length: Python trusts a module's cached bytecode while its file's size and mtime, in whole
    // seconds, are as they were, and on Linux the change and its undoing can share a second.
    r.write("app/settings.py", "LIMIT = 40\n");
    let changed = pytest(&[], false);
    assert_eq!(changed.code, 1, "{}", changed.stderr);
    assert!(
        changed
            .observe_line()
            .contains("5 of 11 test files would have run; 6 already passed"),
        "{}",
        changed.stderr
    );
    assert!(!changed.stderr.contains("though it passed before"));
    r.write("app/settings.py", "LIMIT = 3\n");

    // A data file the tests open is in the rest set: any test may read it.
    r.write("tests/data.txt", "hello\n\n");
    let data = pytest(&[], false);
    assert!(
        data.observe_line().contains(
            "all 11 test files would have run: tests/data.txt changed, and any test may read it"
        ),
        "{}",
        data.stderr
    );
    r.write("tests/data.txt", "hello\n");
    // Back as before: forced, so that the map learns it again.
    pytest(&[], true);

    // A changed lockfile runs everything.
    r.write("requirements.txt", "nothing==2.0\n");
    let lock = pytest(&[], false);
    assert!(
        lock.observe_line()
            .contains("all 11 test files would have run: requirements.txt changed"),
        "{}",
        lock.stderr
    );
    r.write("requirements.txt", "nothing==1.0\n");
    // Back as before: forced, so that the map learns it again.
    pytest(&[], true);

    // A new conftest.py changes what tests load: shape.
    r.write("tests/conftest.py", "\n");
    let shape = pytest(&[], false);
    assert!(
        shape
            .observe_line()
            .contains("all 11 test files would have run: tests/conftest.py is new, which can change what tests load"),
        "{}",
        shape.stderr
    );
    std::fs::remove_file(r.repo.join("tests/conftest.py")).unwrap();
    // Back as before: forced, so that the map learns it again.
    pytest(&[], true);

    // A collection error: pytest stops before any test, and only the broken file records (fail).
    r.write("tests/test_z_broken.py", "def test_x(:\n");
    let before = r.unit_results();
    let broken = pytest(&[], false);
    assert_eq!(broken.code, 2, "{}", broken.stderr);
    assert_eq!(r.unit_results(), before + 1, "{}", broken.stderr);
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
    assert_eq!(first_xdist.code, 0, "{}", first_xdist.stderr);
    let xdist = pytest(&xdist, true);
    assert_eq!(xdist.code, 0, "{}", xdist.stderr);
    assert!(
        xdist
            .observe_line()
            .contains("1 of 11 test files would have run; 10 already passed"),
        "{}",
        xdist.stderr
    );
    assert_eq!(r.run_dirs(), 0);
}

// covers: C-32, C-33
#[test]
fn a_test_file_that_would_have_been_reused_and_fails_is_a_disagreement() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = Repo::new(&[
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
    let run = |force: bool| {
        let mut args = vec!["run"];
        if force {
            args.push("--force");
        }
        args.extend([
            "unit",
            "--",
            &python,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
        ]);
        r.run(&args, &[])
    };
    assert_eq!(run(false).code, 0);
    r.write("flag", "");
    let failed = run(true);
    assert_eq!(failed.code, 1, "{}", failed.stderr);
    assert!(
        failed.stderr.contains(
            "unit: test_flaky.py failed, though it passed before on the same files it read: with `tests: reuse` it would have been reused"
        ),
        "{}",
        failed.stderr
    );
    // \[ADR 0072\] The audit, in this machine's store.
    assert_eq!(store::disagreements(&r.cache()), ["test_flaky.py"]);
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
    let r = Repo::new(&[
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
    let ran = r.run(&["run", "unit", "--", "sh", "-c", &line], &[]);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!(ran.stdout.contains("1 passed"), "{}", ran.stdout);
    // It didn't run whole: no pass of it is recorded.
    assert_eq!(r.unit_results(), 0, "{}", ran.stderr);
}

// covers: C-32
#[test]
fn a_relative_path_opened_after_a_chdir_is_the_file_it_names_there() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // The test file opens `data.py` from the root as it loads, then its test opens `data.py`
    // from `sub/`: the same string, another file, which its list must hold.
    let r = Repo::new(&[
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
    let args = [
        "run",
        "unit",
        "--",
        &python,
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
    ];
    let first = r.run(&args, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(
        r.reads("test_a.py").contains(&"sub/data.py".to_string()),
        "{:?}",
        r.reads("test_a.py")
    );
    r.write("sub/data.py", "2\n");
    let second = r.run(&args, &[]);
    assert_eq!(second.code, 1, "{}", second.stdout);
    assert!(
        !second.stderr.contains("it would have been reused"),
        "{}",
        second.stderr
    );
}

// covers: C-36
#[test]
fn a_store_an_older_hushrun_kept_is_imported_once_and_left_as_it_was() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = Repo::new(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("lib.py", "X = 1\n"),
        ("extra.py", "Y = 1\n"),
        (
            "test_a.py",
            "import lib\n\ndef test_a():\n    assert lib.X == 1\n",
        ),
        ("test_b.py", "def test_b():\n    assert True\n"),
    ]);
    let python = python();
    let args = [
        "run",
        "unit",
        "--",
        &python,
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
    ];
    let first = r.run(&args, &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    // What an older hushrun left: the same rows in `<id>.v1.sqlite`, of schema 1, and no store of
    // schema 2.
    let stores = store::stores(&r.cache());
    assert_eq!(stores.len(), 1);
    let v2 = stores[0].clone();
    let v1 = v2.with_file_name(
        v2.file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".v2.sqlite", ".v1.sqlite"),
    );
    {
        let conn = rusqlite::Connection::open(&v2).unwrap();
        conn.execute_batch(
            "DROP TABLE test_not_followed; DROP TABLE test; DROP TABLE test_list_dir;
             DROP TABLE test_list_read; DROP TABLE test_list; PRAGMA user_version = 1;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
    }
    std::fs::rename(&v2, &v1).unwrap();
    let v1_bytes = std::fs::read(&v1).unwrap();
    // A change no test file read: the check runs, and what reuse would have done reads the map
    // the older store held.
    r.write("extra.py", "Y = 2\n");
    let second = r.run(&args, &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        hushrun_testkit::lines::without_saving(second.observe_line()),
        "hushrun: unit: with `tests: reuse`, 0 of 2 test files would have run; 2 already passed on the files they read"
    );
    // The older file stays as it was, for an older hushrun; this one's store is of schema 2.
    assert_eq!(std::fs::read(&v1).unwrap(), v1_bytes);
    assert!(v2.is_file());
    assert!(!second.stderr.contains("cache step"), "{}", second.stderr);
}

// covers: C-32
#[test]
fn a_test_file_whose_last_test_pytest_exit_cut_short_didnt_run_whole() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_db, the file's last, asks for a fixture that ends the session: two of two tests
    // started, one finished. Neither file runs whole: nothing records a pass.
    let r = Repo::new(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "conftest.py",
            "import pytest\n\n@pytest.fixture\ndef db():\n    pytest.exit('database not reachable', returncode=3)\n",
        ),
        (
            "test_k.py",
            "def test_1():\n    pass\n\ndef test_db(db):\n    pass\n",
        ),
        ("test_z.py", "def test_z():\n    pass\n"),
    ]);
    let python = python();
    let ran = r.run(
        &[
            "run",
            "unit",
            "--",
            &python,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
        ],
        &[],
    );
    assert_eq!(ran.code, 3, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(r.unit_results(), 0, "{}", ran.stderr);
    // \[C-37\] `hushrun why` says a test of it didn't finish, not that a start of its runner
    // didn't run it whole.
    let why = r.run(&["why", "unit", "test_k.py"], &[]);
    assert!(
        why.stdout.contains("test_k.py would run with `tests: reuse`: not every test of it finished the last time it ran"),
        "{}\n{}",
        why.stdout,
        why.stderr
    );
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
    let r = Repo::new(&[
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
    let ran = r.run(
        &[
            "run",
            "unit",
            "--",
            &python,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
        ],
        &[],
    );
    assert_eq!(ran.code, 1, "{}\n{}", ran.stdout, ran.stderr);
    // test_c.py records its fail, and nothing else is recorded.
    assert_eq!(r.unit_results(), 1, "{}", ran.stderr);
}

// covers: C-32
#[test]
fn a_test_file_skipped_whole_as_it_loads_records_no_pass_in_a_run_that_failed() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_mod.py skips itself as it loads, test_imp.py skips for a module that isn't there, and
    // test_fail.py fails: in that run a skip proves nothing, of a whole file too.
    let r = Repo::new(&[
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
    let ran = r.run(
        &[
            "run",
            "unit",
            "--",
            &python,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
        ],
        &[],
    );
    assert_eq!(ran.code, 1, "{}\n{}", ran.stdout, ran.stderr);
    // test_fail.py's fail, and nothing else.
    assert_eq!(r.unit_results(), 1, "{}", ran.stderr);
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
    let r = Repo::new(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "test_x.py",
            "import os, signal\n\ndef test_1():\n    pass\n\ndef test_2():\n    if os.environ.get('CRASH'):\n        os.kill(os.getpid(), signal.SIGKILL)\n",
        ),
    ]);
    let python = python();
    let pytest = format!("{python} -m pytest -q -p no:cacheprovider test_x.py");
    let line = format!("{pytest}; CRASH=1 {pytest}");
    let ran = r.run(&["run", "unit", "--", "sh", "-c", &line], &[]);
    assert_ne!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(r.unit_results(), 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains(
            "hushrun: unit: no test file or test records a pass: a start of its runner ended before it said what it saw"
        ),
        "{}",
        ran.stderr
    );
    // \[C-37\] `hushrun why` says what the run decided, not that it never ran here.
    let why = r.run(&["why", "unit", "test_x.py"], &[]);
    assert!(
        why.stdout
            .contains("a start of its runner ended before it said what it saw"),
        "{}\n{}",
        why.stdout,
        why.stderr
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
    let r = Repo::new(&[
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
    let ran = r.run(
        &[
            "run",
            "unit",
            "--",
            &python,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
            "test_n.py",
            "test_b.py",
        ],
        &[],
    );
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        !ran.stderr.contains("no test file or test records a pass"),
        "{}",
        ran.stderr
    );
    assert!(r.unit_results() > 0, "{}", ran.stderr);
}

// covers: C-32
#[cfg(unix)]
#[test]
fn a_command_pytest_no_longer_reports_for_is_chosen_afresh_next_time() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // `make test` ran pytest, and the store says so; then the Makefile runs ctest. The stored word
    // keeps CTest away only while pytest reports: a run in which it reported nothing clears it.
    let python = python();
    let r = Repo::new(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("test_a.py", "def test_a():\n    pass\n"),
        (
            "Makefile",
            &format!("test:\n\t{python} -m pytest -q -p no:cacheprovider\n"),
        ),
    ]);
    let first = r.run(&["run", "unit", "--", "make", "test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let checks = || -> i64 {
        let store = store::stores(&r.cache());
        rusqlite::Connection::open(&store[0])
            .unwrap()
            .query_row("SELECT count(*) FROM checks", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(checks(), 1);
    // A run that fails before pytest starts (no build directory) proves nothing of who reports:
    // the word stays.
    r.write("Makefile", "test:\n\tctest --test-dir build\n");
    let failed = r.run(&["run", "unit", "--", "make", "test"], &[]);
    assert_ne!(failed.code, 0, "{}\n{}", failed.stdout, failed.stderr);
    assert_eq!(checks(), 1, "{}", failed.stderr);
    // One that passes with pytest silent: the word goes.
    r.write("Makefile", "test:\n\tctest --test-dir build || true\n");
    let second = r.run(&["run", "unit", "--", "make", "test"], &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(checks(), 0, "{}", second.stderr);
}

/// A Cargo workspace: a library crate, a binary crate whose integration test runs the binary, a
/// crate whose test opens a `.rs` file under `testdata/` as data, and an example.
fn rust_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/ws\nchecks:\n  test:\n    tests: observe\n",
        ),
        (".gitignore", "target/\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/core\", \"crates/app\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"core_lib\"\n",
        ),
        (
            "crates/core/src/lib.rs",
            "mod util;\npub fn answer() -> u32 { util::base() + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
        ),
        ("crates/core/src/util.rs", "pub fn base() -> u32 { 41 }\n"),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
        ),
        (
            "crates/app/src/main.rs",
            "fn main() { println!(\"{}\", core_lib::answer()); }\n",
        ),
        (
            "crates/app/examples/demo.rs",
            "fn main() { println!(\"demo\"); }\n",
        ),
        (
            "crates/app/tests/runs_binary.rs",
            "#[test]\nfn prints_the_answer() {\n    let out = std::process::Command::new(env!(\"CARGO_BIN_EXE_app\")).output().unwrap();\n    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), \"42\");\n}\n",
        ),
        (
            "crates/other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "crates/other/src/lib.rs",
            "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
        ),
        (
            "crates/other/tests/reads_data.rs",
            "#[test]\nfn reads_testdata() {\n    let path = concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/testdata/fixture.rs\");\n    assert!(std::fs::read_to_string(path).unwrap().contains(\"fn fixture\"));\n}\n",
        ),
        ("crates/other/testdata/fixture.rs", "fn fixture() {}\n"),
    ]
}

// covers: C-30, C-31, C-32, C-33
#[test]
fn rust_test_binaries_are_observed_from_their_dep_info() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let r = Repo::new(&rust_files());
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    // Four test threads: the four tests start at once, so the slow failure below stops nothing.
    let nextest = [
        "run",
        "test",
        "--",
        "cargo",
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--test-threads",
        "4",
    ];

    let first = r.run(&nextest, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(
        first
            .observe_line()
            .contains("all 5 test files would have run: none had passed here yet"),
        "{}",
        first.stderr
    );
    // A binary crate's integration test reads the binary's sources and its library's, and the
    // package's example: it may run any of them.
    let runs_binary = r.reads("crates/app/tests/runs_binary.rs");
    for file in [
        "crates/app/src/main.rs",
        "crates/core/src/lib.rs",
        "crates/core/src/util.rs",
        "crates/app/examples/demo.rs",
    ] {
        assert!(runs_binary.contains(&file.to_string()), "{runs_binary:?}");
    }
    // A library's unit tests read the library, and nothing of the other crates.
    assert_eq!(
        r.reads("crates/core/src/lib.rs"),
        ["crates/core/src/lib.rs", "crates/core/src/util.rs"]
    );
    // A `.rs` file no build names is data: the rest set, never a source.
    let map = r.map();
    assert!(
        map["rest"]["crates/other/testdata/fixture.rs"].is_string(),
        "{map:#}"
    );
    assert!(
        !map["sources"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("crates/other/testdata/fixture.rs"))
    );
    assert_eq!(r.unit_results(), 5);

    // A change to one crate runs its two test binaries.
    r.write("crates/other/src/lib.rs", "pub fn two() -> u32 { 1 + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n");
    let other = r.run(&nextest, &[]);
    assert!(
        other
            .observe_line()
            .contains("2 of 5 test files would have run; 3 already passed"),
        "{}",
        other.stderr
    );

    // An example is read by its package's integration tests only.
    r.write(
        "crates/app/examples/demo.rs",
        "fn main() { println!(\"demo 2\"); }\n",
    );
    let example = r.run(&nextest, &[]);
    assert!(
        example
            .observe_line()
            .contains("1 of 5 test files would have run; 4 already passed"),
        "{}",
        example.stderr
    );

    // A failing test, with nextest's fail-fast: the exit code is nextest's, and the binaries that
    // ran whole and passed are recorded all the same. Here `other` changes too, so that only this
    // failing run can have recorded its passes; the failure comes late, once they finished.
    let core = "mod util;\npub fn answer() -> u32 { util::base() + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n";
    r.write(
        "crates/core/src/lib.rs",
        &core.replace(
            "{ assert_eq!(super::answer(), 42); }",
            "{ std::thread::sleep(std::time::Duration::from_secs(3)); assert_eq!(super::answer(), 43); }",
        ),
    );
    r.write("crates/other/src/lib.rs", "pub fn two() -> u32 { 2 * 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n");
    let failing = r.run(&nextest, &[]);
    assert_eq!(failing.code, 100, "{}", failing.stderr);
    assert!(
        failing
            .observe_line()
            .contains("5 test files would have run"),
        "{}",
        failing.stderr
    );
    // Fixed: every test file already passed on the files it read, `other`'s in the failing run.
    r.write("crates/core/src/lib.rs", core);
    let fixed = r.run(&nextest, &[]);
    assert_eq!(fixed.code, 0, "{}", fixed.stderr);
    assert!(
        fixed
            .observe_line()
            .contains("0 of 5 test files would have run; 5 already passed"),
        "{}",
        fixed.stderr
    );

    // The data file a test opens changes: the rest set, so everything.
    r.write("crates/other/testdata/fixture.rs", "fn fixture() { }\n");
    let data = r.run(&nextest, &[]);
    assert!(
        data.observe_line().contains(
            "all 5 test files would have run: crates/other/testdata/fixture.rs changed, and any test may read it"
        ),
        "{}",
        data.stderr
    );

    // Plain `cargo test` goes through the same runner, one start per binary.
    let plain = [
        "run",
        "test",
        "--",
        "cargo",
        "test",
        "--workspace",
        "--locked",
        "-q",
    ];
    let first_plain = r.run(&plain, &[]);
    assert_eq!(first_plain.code, 0, "{}", first_plain.stderr);
    assert!(
        first_plain
            .observe_line()
            .contains("all 5 test files would have run")
    );
    r.write("crates/other/src/lib.rs", "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n");
    let second_plain = r.run(&plain, &[]);
    assert!(
        second_plain
            .observe_line()
            .contains("2 of 5 test files would have run; 3 already passed"),
        "{}",
        second_plain.stderr
    );
    assert_eq!(r.run_dirs(), 0);
}

// covers: C-32
#[cfg(unix)]
#[test]
fn the_target_runner_passes_everything_through() {
    let dir = tempfile::tempdir().unwrap();
    let units = dir.path().join("units");
    std::fs::create_dir_all(&units).unwrap();
    let runner = |args: &[&str]| {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").unwrap();
        cmd.arg("__hushrun-test-runner")
            .args(args)
            .env("HUSHRUN_UNITS", &units)
            .env("CARGO_TARGET_X_RUNNER", "set-by-hushrun");
        cmd.output().unwrap()
    };
    // The exit code, and what the test sees of the environment: no directory of hushrun's, and no
    // runner variable when the environment had none.
    let out = runner(&[
        "X",
        "0",
        "sh",
        "-c",
        "echo \"[$HUSHRUN_UNITS][${CARGO_TARGET_X_RUNNER-unset}]\"; exit 3",
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[][unset]\n");
    // A chained runner starts the binary, and the test sees it as its runner.
    let out = runner(&["X", "1", "env", "sh", "-c", "echo $CARGO_TARGET_X_RUNNER"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "env\n");
    // A list goes on as it came, and is kept.
    let out = runner(&[
        "X",
        "0",
        "sh",
        "-c",
        "printf 'a: test\\nb: benchmark\\n'",
        "--list",
    ]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a: test\nb: benchmark\n"
    );
    // A signal ends the runner as it ended the test.
    let out = runner(&["X", "0", "sh", "-c", "kill -TERM $$"]);
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(out.status.signal(), Some(15));
    let files: Vec<Value> = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(files.len(), 4);
    assert!(files.iter().any(|f| f["invocation"]["exit"] == 3));
    assert!(files.iter().any(|f| f["invocation"]["signal"] == 15));
    assert!(
        files
            .iter()
            .any(|f| f["invocation"]["listed"] == serde_json::json!(["a"]))
    );
    // Without a directory it still runs the test, and writes nothing.
    let mut cmd = assert_cmd::Command::cargo_bin("hushrun").unwrap();
    let out = cmd
        .args(["__hushrun-test-runner", "X", "0", "sh", "-c", "exit 0"])
        .env_remove("HUSHRUN_UNITS")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}

// covers: C-39
#[cfg(unix)]
#[test]
fn a_program_on_path_starts_the_real_one_with_path_as_it_was() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // A directory of hushrun's programs, which a real program is never taken from, first on PATH;
    // then the real one, a fake tool that says its arguments and environment.
    let programs = root.join("programs");
    std::fs::create_dir_all(&programs).unwrap();
    std::fs::write(programs.join(".hushrun-programs"), b"").unwrap();
    let real = root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    for at in [&programs, &real] {
        let tool = at.join("faketool");
        std::fs::write(
            &tool,
            "#!/bin/sh\necho \"[$*][$PATH][${HUSHRUN_PROGRAMS-unset}][${HUSHRUN_PROGRAMS_PATH-unset}]\"\nexit 4\n",
        )
        .unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let was = format!("{}:/usr/bin:/bin", real.display());
    let program = |name: &str, args: &[&str]| {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").unwrap();
        cmd.arg("__hushrun-program")
            .arg(name)
            .args(args)
            .env("PATH", format!("{}:{was}", programs.display()))
            .env("HUSHRUN_PROGRAMS", &programs)
            .env("HUSHRUN_PROGRAMS_PATH", format!("={was}"))
            .env("HUSHRUN_UNITS", root.join("units"));
        cmd.output().unwrap()
    };
    // The real program, with no program of hushrun's named for it: its arguments as they came,
    // its exit code, PATH as COMMAND had it, and neither of hushrun's variables.
    let out = program("faketool", &["test", "--parallel"]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("[test --parallel][{was}][unset][unset]\n")
    );
    // None on PATH: 127, as a shell says.
    let out = program("nothere", &[]);
    assert_eq!(out.status.code(), Some(127));
}

/// The first 10 characters of a ULID: its milliseconds.
fn ulid_time(ms: i64) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    (0..10)
        .rev()
        .map(|i| ALPHABET[((ms >> (5 * i)) & 31) as usize] as char)
        .collect()
}

// covers: C-33 (ADR 0067: unit results are swept as this machine's results are)
#[test]
fn this_machine_forgets_unit_results_after_30_days() {
    let r = Repo::new(&[("src/a.txt", "a\n")]);
    let old = ulid_time(
        "2026-01-01T00:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond(),
    );
    let results = r.cache().join("results");
    let entry = results.join(format!(
        "units/v0/{}/{old}0000000000000000.json",
        "a".repeat(64)
    ));
    let run = results.join(format!(
        "unit-runs/v0/{}/{old}0000000000000000.json",
        "b".repeat(64)
    ));
    let map = r.cache().join("units/stale.map.json");
    let left = r.cache().join("units/runs/01OLD");
    for file in [&entry, &run, &map] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "{}").unwrap();
    }
    std::fs::create_dir_all(&left).unwrap();
    // The files' own times are now: the map and a stopped run's directory go by them, so they stay.
    let ran = r.run(
        &[
            "run",
            "--now",
            "2026-02-01T00:00:00Z",
            "unit",
            "--",
            "git",
            "--version",
        ],
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        !entry.exists() && !run.exists(),
        "unit results older than 30 days go"
    );
    assert!(map.exists() && left.exists());
}

// covers: C-33
#[test]
fn two_repositories_with_one_check_keep_their_maps_apart() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let shop = Repo::new(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("lib.py", "X = 1\n"),
        (
            "test_lib.py",
            "import lib\n\ndef test_x():\n    assert lib.X == 1\n",
        ),
    ]);
    let other = Repo::new(&[
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
    let python = python();
    let args = [
        "run",
        "unit",
        "--",
        &python,
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
    ];
    // One cache directory, as one machine has, for both: same check, same command.
    let cache = shop.cache();
    let cache = cache.to_str().unwrap();
    let first = shop.run(&args, &[("HUSHRUN_CACHE_DIR", cache)]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let second = other.run(&args, &[("HUSHRUN_CACHE_DIR", cache)]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    // The other repository's map isn't this one's: nothing of the first says why.
    assert_eq!(
        second.observe_line(),
        "hushrun: unit: with `tests: reuse`, all 1 test file would have run: none had passed here yet on the files it read"
    );
    // \[ADR 0072\] One store per repository.
    assert_eq!(store::stores(&shop.cache()).len(), 2);
}
