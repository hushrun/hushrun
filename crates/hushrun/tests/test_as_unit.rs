//! \[ADR 0077\] A test as the unit (C-5, C-39): with `tests.unit: test`, each test of a test file
//! is reused on its own, beside its test files, in two tiers; a test that wasn't followed costs
//! only itself; an audited test that fails runs once more, alone.
//!
//! The pytest fixtures need Python with pytest and pytest-xdist. They say so and pass where those
//! are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::fake_results::FakeResults;
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

/// Whether the Python the tests run has `sys.monitoring` (3.12 and later), with which pytest's
/// adapter watches each test (ADR 0080).
fn watches() -> bool {
    Command::new(python())
        .args([
            "-c",
            "import sys; sys.exit(0 if hasattr(sys, 'monitoring') else 1)",
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: {REPOSITORY}\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A cart with six tests, one of which fails while `FAIL` is set; a test file whose one test starts
/// a process while three don't; and three small test files of one test each.
fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (
            ".gitignore".into(),
            "__pycache__/\n.pytest_cache/\n".into(),
        ),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("requirements.txt".into(), "nothing==1.0\n".into()),
        ("app/__init__.py".into(), String::new()),
        ("app/cart.py".into(), "def total(xs):\n    return sum(xs)\n".into()),
        ("app/greet.py".into(), "def hello():\n    return 'hi'\n".into()),
        (
            "tests/test_cart.py".into(),
            "import os\nimport pytest\nfrom app.cart import total\n\n\
             def test_one():\n    assert total([1]) == 1\n\n\
             def test_two():\n    assert total([1, 1]) == 2\n\n\
             def test_flips():\n    assert not os.environ.get('FAIL')\n\n\
             class TestMore:\n    def test_three(self):\n        assert total([3]) == 3\n\n\
             @pytest.mark.parametrize('n', [4, 5])\ndef test_n(n):\n    assert total([n]) == n\n"
                .into(),
        ),
        (
            "tests/test_child.py".into(),
            "import subprocess, sys\nfrom app.cart import total\n\n\
             def test_spawns():\n    assert subprocess.run([sys.executable, '-c', 'pass']).returncode == 0\n\n\
             def test_a():\n    assert total([1]) == 1\n\n\
             def test_b():\n    assert total([2]) == 2\n\n\
             def test_c():\n    assert total([3]) == 3\n"
                .into(),
        ),
    ];
    for n in 1..=3 {
        files.push((
            format!("tests/test_greet_{n}.py"),
            "from app.greet import hello\n\ndef test_hello():\n    assert hello() == 'hi'\n".into(),
        ));
    }
    files
}

fn repo_of(files: &[(String, String)]) -> Repo {
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    Repo::new(&borrowed)
}

fn pytest_args<'a>(python: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "run",
        "unit",
        "--",
        python,
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
    ];
    args.extend(extra);
    args
}

/// A line of what hushrun said that starts so.
fn line<'a>(ran: &'a Ran, start: &str) -> &'a str {
    ran.stderr
        .lines()
        .find(|l| l.starts_with(start))
        .unwrap_or_else(|| panic!("no line starting {start:?} in:\n{}", ran.stderr))
}

// covers: C-39, C-5, C-10, C-36
#[test]
fn a_test_that_failed_runs_alone_and_its_files_others_are_reused_one_by_one() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&shop("{mode: reuse, unit: test}"));
    let python = python();
    // The first run, with test_flips failing: every test runs, and each is recorded on its own
    // where its test file didn't pass whole.
    let first = r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = store::map(&r.cache);
    assert_eq!(
        map["units"]["tests/test_cart.py"]["tests"],
        json!({"TestMore::test_three": "passed", "test_flips": "failed", "test_n[4]": "passed",
               "test_n[5]": "passed", "test_one": "passed", "test_two": "passed"}),
        "{map:#}"
    );
    // test_child's subprocess is its own test's: its file keeps no `notFollowed`.
    assert_eq!(
        map["units"]["tests/test_child.py"]["tests"]["test_spawns"],
        "not-observed"
    );
    assert!(
        map["units"]["tests/test_child.py"]
            .get("notFollowed")
            .is_none(),
        "{map:#}"
    );
    // test_cart failed: its own fail and each of its tests; test_child, not observed: its three
    // followed tests; and each greet file's pass.
    assert_eq!(r.unit_results(), 1 + 6 + 3 + 3);
    let units = &r.newest_result()["units"];
    assert_eq!(units["tests"], json!({"total": 13, "ran": 13, "reused": 0}));
    assert_eq!(
        (units["total"].clone(), units["ran"].clone()),
        (json!(5), json!(5))
    );

    // The same files, the flip fixed: test_cart's own key failed, so its tests are asked one by
    // one: five passed on these files. test_child isn't reused whole, since one of its tests
    // wasn't followed: its other three may be. Of those eight, one, chosen by its key, runs as a
    // check; and of the greet files, which passed whole, one.
    let second = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        line(&second, "hushrun: unit: "),
        "hushrun: unit: 4 of 13 tests run, in 3 of 5 test files; 9 already passed on the files they read, and 2 more that already passed run anyway, as a check"
    );
    // pytest ran what the plan left: test_flips, test_spawns, the audited test and the audited
    // greet file's.
    assert!(
        second.stdout.contains("4 passed, 9 deselected"),
        "{}",
        second.stdout
    );
    let result = r.newest_result();
    assert_eq!(result["result"], "pass");
    let saved = result["units"]["savedMs"].as_u64().expect("savedMs");
    assert_eq!(
        result["units"],
        json!({"total": 5, "ran": 3, "reused": 2, "savedMs": saved,
               "tests": {"total": 13, "ran": 4, "reused": 9}})
    );
    assert!(
        hushrun_testkit::lines::says_reused(line(&second, "hushrun: unit passed in"), 9, "tests"),
        "{}",
        second.stderr
    );
    // Reuse never chains: a test file that didn't run whole records no pass of its own, and
    // a reused test nothing; the tests that ran record theirs, and the audited greet file its pass.
    assert_eq!(r.unit_results(), 13 + 2 + 1, "{}", second.stderr);
    // A reused test keeps the row it stands on.
    let map = store::map(&r.cache);
    assert_eq!(
        map["units"]["tests/test_cart.py"]["tests"]["test_flips"],
        "passed"
    );
    assert_eq!(
        map["units"]["tests/test_cart.py"]["tests"]["test_one"],
        "passed"
    );

    // The same code again: the check skips whole (C-10).
    let again = r.run(&pytest_args(&python, &[]), &[]);
    assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
}

// covers: C-39, C-32
#[test]
fn a_skip_in_a_run_that_failed_records_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // While SKIP is set, test_skipped's body doesn't run (a skipif, a skip in a fixture's setup,
    // an xfail that isn't run), and test_flips fails while FAIL is set. A skip in a run that
    // failed proves nothing: a suite setup that failed can skip what it hides.
    let mut files = shop("{mode: reuse, unit: test}");
    files.push((
        "tests/test_skips.py".into(),
        "import os\nimport pytest\n\n\
         @pytest.fixture\ndef ready():\n    if os.environ.get('SKIP'):\n        pytest.skip('not ready')\n\n\
         @pytest.mark.skipif(bool(os.environ.get('SKIP')), reason='skipped')\ndef test_skipped():\n    assert True\n\n\
         def test_in_setup(ready):\n    assert True\n\n\
         @pytest.mark.xfail(bool(os.environ.get('SKIP')), run=False, reason='not run')\ndef test_not_run():\n    assert True\n\n\
         def test_ran():\n    assert True\n"
            .into(),
    ));
    files.push((
        "tests/test_skipmod.py".into(),
        "import os\nimport pytest\n\nif os.environ.get('SKIP'):\n    pytest.skip('not here', allow_module_level=True)\n\ndef test_m():\n    assert True\n"
            .into(),
    ));
    let r = repo_of(&files);
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[("FAIL", "1"), ("SKIP", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = store::map(&r.cache);
    // The skipped tests are not run, and record nothing; the one that ran records its pass.
    assert_eq!(
        map["units"]["tests/test_skips.py"]["tests"],
        json!({"test_in_setup": "not-run", "test_not_run": "not-run", "test_ran": "passed",
               "test_skipped": "not-run"}),
        "{map:#}"
    );
    // \[C-37\] `hushrun why` says why each kept nothing, as the run decided it: the test, and its
    // test file, which one of its tests' skips kept from a pass.
    let skipped = why_json(&r, "tests/test_skips.py::test_skipped");
    assert_eq!(skipped["reason"], "not-kept", "{skipped:#}");
    assert_eq!(
        skipped["says"],
        format!("tests/test_skips.py::test_skipped runs: {SKIPPED_IN_A_FAILED_RUN}"),
        "{skipped:#}"
    );
    let file = why_json(&r, "tests/test_skips.py");
    assert_eq!(file["reason"], "not-kept", "{file:#}");
    assert_eq!(
        file["says"],
        "tests/test_skips.py runs: its test test_in_setup kept no result: it was skipped in a run that failed, and a skip there proves nothing: what failed may have skipped what it hides",
        "{file:#}"
    );
    // A test file skipped whole as it loads, which no test of it says: the same reason, not that
    // it never ran here.
    let whole = why_json(&r, "tests/test_skipmod.py");
    assert_eq!(whole["reason"], "not-kept", "{whole:#}");
    // In a run that passed, a skip is a pass, as before.
    let passed = r.run(&pytest_args(&python, &[]), &[("SKIP", "1")]);
    assert_eq!(passed.code, 0, "{}\n{}", passed.stdout, passed.stderr);
    let map = store::map(&r.cache);
    assert_eq!(
        map["units"]["tests/test_skips.py"]["tests"]["test_skipped"], "passed",
        "{map:#}"
    );
}

/// What `hushrun why` says of a test skipped in a run that failed (C-37).
const SKIPPED_IN_A_FAILED_RUN: &str = "it was skipped in a run that failed, and a skip there proves nothing: what failed may have skipped what it hides";

/// A runner that writes an adapter's facts itself, as pytest's plugin would: each test file
/// of `FAKE_FILES` ran whole, and in the starts `FAKE_LEFT_OUT` names (by their number across
/// every run, from 1) it says it left out what the plan didn't list. Each start keeps the
/// `hushrun.json` it saw in `FAKE_STATE`, and exits with `FAKE_EXIT_<n>`, else 0.
const FAKE_RUNNER: &str = r#"import json, os, sys
d = os.environ["HUSHRUN_UNITS"]
with open(os.path.join(d, "hushrun.json")) as f:
    run = json.load(f)
state = os.environ["FAKE_STATE"]
n = len(os.listdir(state)) + 1
with open(os.path.join(state, str(n)), "w") as f:
    json.dump(run, f)
units = [{"unit": u, "selected": 1, "finished": 1, "reads": [u]}
         for u in os.environ["FAKE_FILES"].split()]
facts = {"schema": "hushrun.unit-facts/v0", "adapter": "pytest", "failuresByUnit": True,
         "settings": {"pythonFiles": ["test_*.py", "*_test.py"]}, "units": units}
if str(n) in os.environ.get("FAKE_LEFT_OUT", "").split():
    facts["leftOutUnlisted"] = "2 features of test_a.py nobody listed"
if os.environ.get("FAKE_STALE"):
    facts["staleBuild"] = os.environ["FAKE_STALE"]
with open(os.path.join(d, "fake-%d.json" % n), "w") as f:
    json.dump(facts, f)
sys.exit(int(os.environ.get("FAKE_EXIT_%d" % n, "0")))
"#;

// covers: C-32, C-34
#[test]
fn an_adapter_that_left_out_what_the_plan_didnt_list_runs_again_with_nothing_left_out() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = Repo::new(&[
        ("hushrun.yaml", &yaml("reuse")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", FAKE_RUNNER),
        ("other.py", "X = 1\n"),
        ("test_a.py", "def test_a():\n    pass\n"),
        ("test_b.py", "def test_b():\n    pass\n"),
    ]);
    let state = r.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let python = python();
    let state_dir = state.to_string_lossy().into_owned();
    let run = |extra: &[(&str, &str)]| {
        let mut env = vec![
            ("FAKE_STATE", state_dir.as_str()),
            ("FAKE_FILES", "test_a.py test_b.py"),
        ];
        env.extend_from_slice(extra);
        r.run(&["run", "unit", "--", &python, "fake.py"], &env)
    };
    let seen = |n: usize| -> Value {
        serde_json::from_slice(&std::fs::read(state.join(n.to_string())).unwrap()).unwrap()
    };
    // Start 1: both test files pass, and are recorded.
    let first = run(&[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(r.unit_results(), 2, "{}", first.stderr);
    // Start 2 is planned with one of them listed, fails, and says it left out what the plan
    // didn't list: start 3 runs with nothing left out, passes, and alone decides the run.
    r.write("other.py", "X = 2\n");
    let second = run(&[("FAKE_LEFT_OUT", "2"), ("FAKE_EXIT_2", "1")]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(
        second.stderr.contains(
            "hushrun: unit: running it once more with nothing left out: its adapter left out what the plan didn't list (2 features of test_a.py nobody listed)"
        ),
        "{}",
        second.stderr
    );
    assert_eq!(seen(2)["reuse"].as_array().unwrap().len(), 1, "{}", seen(2));
    assert_eq!(seen(3)["reuse"], json!([]), "{}", seen(3));
    assert!(seen(3).get("reuseTests").is_none(), "{}", seen(3));
    let result = r.newest_result();
    assert_eq!(result["result"], "pass", "{result:#}");
    assert_eq!(
        (
            result["units"]["total"].clone(),
            result["units"]["reused"].clone()
        ),
        (json!(2), json!(0))
    );
    // Start 5 says it again: nothing is recorded, and hushrun fails, saying why.
    r.write("other.py", "X = 3\n");
    let before = r.unit_results();
    let results = || {
        let mut files = Vec::new();
        collect(&r.cache.join("results/results/v0"), &mut files);
        files.len()
    };
    let recorded = results();
    let third = run(&[("FAKE_LEFT_OUT", "4 5")]);
    assert_eq!(third.code, 1, "{}", third.stderr);
    assert!(
        third.stderr.contains(
            "hushrun: unit wasn't recorded: run again with nothing left out, its adapter still left out what the plan didn't list (2 features of test_a.py nobody listed)"
        ),
        "{}",
        third.stderr
    );
    assert_eq!(results(), recorded);
    assert_eq!(r.unit_results(), before);
}

// covers: C-39, C-32
#[test]
fn a_test_cut_short_by_pytest_exit_or_an_interrupt_records_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // A session fixture ends the session with pytest.exit; an interrupt arrives in a body. Neither
    // test finished: no pass of either, and their files didn't run whole.
    let files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\npythonpath = .\n".into()),
        (
            "tests/conftest.py".into(),
            "import os, pytest\n\n@pytest.fixture(scope='session')\ndef db():\n    if os.environ.get('NODB'):\n        pytest.exit('database not reachable', returncode=3)\n    return 1\n".into(),
        ),
        (
            "tests/test_k.py".into(),
            "import os, signal, time\n\ndef test_1():\n    assert True\n\n\
             def test_db(db):\n    assert True\n\n\
             def test_slow():\n    if os.environ.get('INT'):\n        os.kill(os.getpid(), signal.SIGINT)\n        time.sleep(1)\n\n\
             def test_3():\n    assert True\n"
                .into(),
        ),
    ];
    for (var, cut) in [("NODB", "test_db"), ("INT", "test_slow")] {
        let r = repo_of(&files);
        let python = python();
        let ran = r.run(&pytest_args(&python, &[]), &[(var, "1")]);
        assert_ne!(ran.code, 0, "{var}: {}\n{}", ran.stdout, ran.stderr);
        let map = store::map(&r.cache);
        let tests = &map["units"]["tests/test_k.py"]["tests"];
        assert_ne!(tests[cut], "passed", "{var}: {map:#}");
        assert_eq!(tests["test_1"], "passed", "{var}: {map:#}");
        assert_ne!(tests["test_3"], "passed", "{var}: {map:#}");
    }
}

// covers: C-39, C-32
#[test]
fn a_failing_teardown_of_a_wider_fixture_keeps_every_test_it_served_from_a_pass() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // An autouse module fixture checks at its end that no test leaked; test_opens leaks, and
    // pytest reports the error on test_last. Left out on the next run, test_opens would let the
    // check pass on the same code.
    let files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\npythonpath = .\n".into()),
        (
            "tests/test_leak.py".into(),
            "import os, pytest\nLEAKS = []\n\n\
             @pytest.fixture(scope='module', autouse=True)\ndef no_leaks():\n    yield\n    assert not LEAKS, 'a test left a connection open'\n\n\
             def test_opens():\n    if os.environ.get('BUG'):\n        LEAKS.append('conn')\n\n\
             @pytest.mark.parametrize('i', range(10))\ndef test_more(i):\n    assert True\n\n\
             def test_last():\n    assert True\n"
                .into(),
        ),
        (
            "tests/test_other.py".into(),
            "def test_other():\n    assert True\n".into(),
        ),
    ];
    let r = repo_of(&files);
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[("BUG", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = store::map(&r.cache);
    let tests = &map["units"]["tests/test_leak.py"]["tests"];
    assert_ne!(tests["test_opens"], "passed", "{map:#}");
    assert_ne!(tests["test_more[3]"], "passed", "{map:#}");
    // \[C-37\] `hushrun why` says the fixture's end kept it, not that it didn't finish.
    let why = why_json(&r, "tests/test_leak.py::test_opens");
    assert_eq!(why["reason"], "not-kept", "{why:#}");
    assert!(
        why["says"]
            .as_str()
            .unwrap()
            .contains("a fixture of a wider scope it was served (no_leaks) failed as it ended"),
        "{why:#}"
    );
    // Run again on the same code: the leak is caught again.
    let second = r.run(&pytest_args(&python, &[]), &[("BUG", "1")]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
}

// covers: C-32
#[test]
fn a_session_fixture_asked_for_at_run_time_whose_end_fails_keeps_the_test_from_a_pass() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_leaky takes the session fixture `db` with `request.getfixturevalue`, which no test's
    // `fixturenames` holds, and leaks; `db` says so at the session's end, which pytest reports on
    // the last test. Left out next time, test_leaky would let the check pass on the same code.
    let files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\npythonpath = .\n".into()),
        (
            "tests/conftest.py".into(),
            "import pytest\nOPEN = []\n\n\
             @pytest.fixture(scope='session')\ndef db():\n    yield OPEN\n    assert not OPEN, 'a connection was left open'\n"
                .into(),
        ),
        (
            "tests/test_l.py".into(),
            "import os\n\ndef test_leaky(request):\n    db = request.getfixturevalue('db')\n    if os.environ.get('BUG'):\n        db.append('c')\n"
                .into(),
        ),
        (
            "tests/test_m.py".into(),
            "import pytest\n\n@pytest.mark.parametrize('i', range(5))\ndef test_many(i, db):\n    assert True\n"
                .into(),
        ),
    ];
    let r = repo_of(&files);
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[("BUG", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = store::map(&r.cache);
    assert_ne!(
        map["units"]["tests/test_l.py"]["tests"]["test_leaky"], "passed",
        "{map:#}"
    );
    // \[C-37\] Its test file keeps no pass for the same reason, said through the test.
    let file = why_json(&r, "tests/test_l.py");
    assert_eq!(file["reason"], "not-kept", "{file:#}");
    let says = file["says"].as_str().unwrap();
    assert!(
        says.starts_with("tests/test_l.py runs: its test test_leaky kept no result: a fixture of a wider scope it was served (db) failed as it ended"),
        "{file:#}"
    );
    let second = r.run(&pytest_args(&python, &[]), &[("BUG", "1")]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
}

// covers: C-32
#[test]
fn a_wider_fixture_whose_end_fails_keeps_no_pass_from_a_test_served_another_of_its_name() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // Two fixtures named `res`: a/'s, of module scope, fails as it ends; b/'s, a function's,
    // has nothing to do with it. test_b keeps its pass.
    let files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\npythonpath = .\n".into()),
        ("tests/a/__init__.py".into(), String::new()),
        (
            "tests/a/conftest.py".into(),
            "import os, pytest\n\n@pytest.fixture(scope='module')\ndef res():\n    yield 1\n    if os.environ.get('FAIL_A'):\n        raise RuntimeError('a res end failed')\n"
                .into(),
        ),
        ("tests/a/test_a.py".into(), "def test_a(res):\n    assert res\n".into()),
        ("tests/b/__init__.py".into(), String::new()),
        (
            "tests/b/conftest.py".into(),
            "import pytest\n\n@pytest.fixture\ndef res():\n    return 2\n".into(),
        ),
        ("tests/b/test_b.py".into(), "def test_b(res):\n    assert res\n".into()),
    ];
    let r = repo_of(&files);
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[("FAIL_A", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = store::map(&r.cache);
    assert_ne!(
        map["units"]["tests/a/test_a.py"]["tests"]["test_a"], "passed",
        "{map:#}"
    );
    assert_eq!(
        map["units"]["tests/b/test_b.py"]["tests"]["test_b"], "passed",
        "{map:#}"
    );
}

// covers: C-32
#[test]
fn in_observe_a_start_that_left_out_what_nothing_listed_records_no_unit_and_keeps_its_exit() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // Nothing was planned to be left out: running again with nothing left out would be the same
    // run, and could fail one that passed.
    let r = Repo::new(&[
        ("hushrun.yaml", &yaml("observe")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", FAKE_RUNNER),
        ("test_a.py", "def test_a():\n    pass\n"),
    ]);
    let state = r.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let python = python();
    let state_dir = state.to_string_lossy().into_owned();
    let ran = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[
            ("FAKE_STATE", state_dir.as_str()),
            ("FAKE_FILES", "test_a.py"),
            ("FAKE_LEFT_OUT", "1 2"),
            ("FAKE_EXIT_2", "1"),
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        std::fs::read_dir(&state).unwrap().count(),
        1,
        "{}",
        ran.stderr
    );
    assert!(
        ran.stderr.contains(
            "hushrun: unit: test files weren't observed: its adapter left out what nothing listed (2 features of test_a.py nobody listed)"
        ),
        "{}",
        ran.stderr
    );
    assert_eq!(r.unit_results(), 0);
    assert!(r.newest_result().get("units").is_none());
}

// covers: C-32
#[test]
fn a_run_on_a_stale_build_records_no_pass_of_its_check() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // The adapter knows the runner ran a program built before the last change: the run passed,
    // and proves nothing of this code.
    let r = Repo::new(&[
        ("hushrun.yaml", &yaml("observe")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", FAKE_RUNNER),
        ("test_a.py", "def test_a():\n    pass\n"),
    ]);
    let state = r.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let python = python();
    let state_dir = state.to_string_lossy().into_owned();
    let env = [
        ("FAKE_STATE", state_dir.as_str()),
        ("FAKE_FILES", "test_a.py"),
        (
            "FAKE_STALE",
            "build/prog is older than what it is built from",
        ),
    ];
    let ran = r.run(&["run", "unit", "--", &python, "fake.py"], &env);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains(
            "hushrun: unit passed, but it isn't recorded: build/prog is older than what it is built from"
        ),
        "{}",
        ran.stderr
    );
    assert!(!ran.stderr.contains("it won't run again"), "{}", ran.stderr);
    let mut files = Vec::new();
    collect(&r.cache.join("results/results/v0"), &mut files);
    assert!(files.is_empty(), "{files:?}");
    assert_eq!(r.unit_results(), 0);
    // A failure is recorded as usual.
    let failed = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[env[0], env[1], env[2], ("FAKE_EXIT_2", "1")],
    );
    assert_eq!(failed.code, 1, "{}", failed.stderr);
    assert_eq!(r.newest_result()["result"], "fail");
    // \[C-32\] Its test file passed on that stale build in a run that failed: no pass of it is
    // kept.
    assert_eq!(r.unit_results(), 0);
}

/// A runner that writes pytest's facts itself with tests as the unit, and fails: `test_a.py`
/// failed while each of its tests passed, as a failure no test explains would say (a failing
/// `teardown_module`, a crash between tests); with `FAKE_TWO_FAILS`, its `test_two` failed; with
/// `FAKE_STALE`, it ran on a stale build.
const UNEXPLAINED_RUNNER: &str = r#"import json, os, sys
d = os.environ["HUSHRUN_UNITS"]
two = bool(os.environ.get("FAKE_TWO_FAILS"))
tests = [{"id": "test_one", "finished": True}, {"id": "test_two", "finished": True, "failed": two}]
units = [{"unit": "test_a.py", "selected": 2, "finished": 2, "failed": True,
          "reads": ["test_a.py"], "tests": tests}]
facts = {"schema": "hushrun.unit-facts/v1", "adapter": "pytest", "failuresByUnit": True,
         "settings": {"pythonFiles": ["test_*.py", "*_test.py"]}, "units": units}
if os.environ.get("FAKE_STALE"):
    facts["staleBuild"] = os.environ["FAKE_STALE"]
with open(os.path.join(d, "fake.json"), "w") as f:
    json.dump(facts, f)
sys.exit(1)
"#;

// covers: C-37, C-39
#[test]
fn why_says_a_test_kept_nothing_as_its_test_file_failed_while_none_of_its_tests_did() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = Repo::new(&[
        ("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", UNEXPLAINED_RUNNER),
        (
            "test_a.py",
            "def test_one():\n    pass\n\ndef test_two():\n    pass\n",
        ),
    ]);
    let python = python();
    let ran = r.run(&["run", "unit", "--", &python, "fake.py"], &[]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    let map = store::map(&r.cache);
    assert_eq!(
        map["units"]["test_a.py"]["tests"],
        json!({"test_one": "not-run", "test_two": "not-run"}),
        "{map:#}"
    );
    let one = why_json(&r, "test_a.py::test_one");
    assert_eq!(one["reason"], "not-kept", "{one:#}");
    assert_eq!(
        one["says"],
        "test_a.py::test_one runs: its test file failed while none of its tests did, and what failed may need this test to run, so it keeps no pass",
        "{one:#}"
    );
    // \[C-32\] On a stale build, test_one's pass in a run test_two failed proves nothing either.
    let ran = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[
            ("FAKE_TWO_FAILS", "1"),
            ("FAKE_STALE", "build/prog is older"),
        ],
    );
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    let one = why_json(&r, "test_a.py::test_one");
    assert_eq!(
        one["says"],
        "test_a.py::test_one runs: it ran on a build older than these files, so its pass proves nothing of them",
        "{one:#}"
    );
}

// covers: C-39
#[test]
fn with_tests_as_the_unit_a_file_that_changed_runs_every_test_it_has() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&shop("{mode: reuse, unit: test}"));
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);
    // cart.py changes: with `reads: file` every test of every test file that read it runs, those
    // that passed included: their keys hold what their file read.
    r.write(
        "app/cart.py",
        "def total(xs):\n    return sum(xs)  # changed\n",
    );
    let changed = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        line(&changed, "hushrun: unit: "),
        "hushrun: unit: 11 of 13 tests run, in 3 of 5 test files; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
}

// covers: C-39, C-34
#[test]
fn an_audited_test_that_fails_again_alone_suspends_the_checks_reuse() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // One test file, two tests: one fails while FAIL is set, the other while HIDDEN is, which no
    // file says, as a missed dependency would.
    let files = vec![
        (
            "hushrun.yaml".to_string(),
            yaml("{mode: reuse, unit: test}"),
        ),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        (
            "tests/test_two.py".into(),
            "import os\n\ndef test_ok():\n    assert not os.environ.get('HIDDEN')\n\n\
             def test_flips():\n    assert not os.environ.get('FAIL')\n"
                .into(),
        ),
    ];
    let r = repo_of(&files);
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);
    // test_ok passed on these files, and is the one test that could be reused: it runs as a
    // check, and fails. It runs once more, alone, and fails again: a disagreement.
    let caught = r.run(&pytest_args(&python, &[]), &[("HIDDEN", "1")]);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        line(&caught, "hushrun: unit: all"),
        "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs anyway, as a check"
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: tests/test_two.py::test_ok failed, though it passed before on the same files it read: running it once more"
        ),
        "{}",
        caught.stderr
    );
    // The second run ran test_ok alone: test_flips was deselected one by one.
    assert!(
        caught.stdout.contains("1 failed, 1 deselected"),
        "{}",
        caught.stdout
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: tests/test_two.py::test_ok failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(
        store::disagreements(&r.cache),
        ["tests/test_two.py::test_ok"]
    );
    // Suspended: every test file runs, tests too.
    let after = r.run(&pytest_args(&python, &[]), &[]);
    assert!(
        after
            .stderr
            .contains("hushrun: unit: every test file runs: reuse of its test files is suspended"),
        "{}",
        after.stderr
    );
}

// covers: C-39, C-5
#[test]
fn observe_says_what_reusing_tests_would_have_done_and_test_files_stay_as_they_were() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&shop("{mode: observe, unit: test}"));
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);
    let second = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    // Everything ran: observe changes nothing.
    assert!(second.stdout.contains("13 passed"), "{}", second.stdout);
    assert!(
        hushrun_testkit::lines::without_saving(line(&second, "hushrun: unit: with `tests: reuse`"))
            .starts_with(
                "hushrun: unit: with `tests: reuse`, 2 of 13 tests would have run, in 2 of 5 test files; 11 already passed on the files they read"
            ),
        "{}",
        second.stderr
    );
    assert_eq!(
        r.newest_result()["units"]["tests"],
        json!({"total": 13, "ran": 13, "reused": 0})
    );
    // \[ADR 0080\] `reads: test`: pytest watches each test on Python 3.12 and later, and says
    // nothing; below, it says once why each test is taken to have read its test file's list.
    r.write(
        "hushrun.yaml",
        &yaml("{mode: observe, unit: test, reads: test}"),
    );
    let reads = r.run(&pytest_args(&python, &[]), &[]);
    let said = reads
        .stderr
        .matches("each test is taken to have read what its test file read")
        .count();
    if watches() {
        assert_eq!(said, 0, "{}", reads.stderr);
    } else {
        assert_eq!(said, 1, "{}", reads.stderr);
        assert!(
            reads
                .stderr
                .contains("has no sys.monitoring, which watching a test needs"),
            "{}",
            reads.stderr
        );
    }
    // Test files, as before: no test is told, and the counts are test files'.
    r.write("hushrun.yaml", &yaml("observe"));
    r.write("app/greet.py", "def hello():\n    return 'hi'  # changed\n");
    let files = r.run(&pytest_args(&python, &[]), &[]);
    assert!(
        line(&files, "hushrun: unit: with `tests: reuse`").contains("test files would have run"),
        "{}",
        files.stderr
    );
    assert!(r.newest_result()["units"].get("tests").is_none());
}

fn plugin_pytest(r: &Repo, units: &Path, run: Value, args: &[&str]) -> (i32, String, Vec<Value>) {
    let _ = std::fs::remove_dir_all(units);
    std::fs::create_dir_all(units).unwrap();
    std::fs::write(units.join("hushrun.json"), run.to_string()).unwrap();
    let adapter = std::fs::read_dir(r.cache.join("adapters"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let out = Command::new(python())
        .args(["-m", "pytest", "-q", "-p", "no:cacheprovider"])
        .args(args)
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", units)
        .env("PYTHONPATH", adapter)
        .env("PYTEST_ADDOPTS", "-p hushrun_pytest")
        .output()
        .unwrap();
    let facts = std::fs::read_dir(units)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.starts_with("pytest-") && name.ends_with(".json")
        })
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        facts,
    )
}

// covers: C-39, C-42
#[test]
fn a_wider_fixture_that_switches_its_parameter_ends_the_old_one_for_the_test_file() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // pytest ends the module fixture's first value inside test_x[2]'s setup, where its teardown
    // starts a process: that is the test file's, as a teardown that may end a wider fixture is,
    // and never test_x[2]'s alone.
    let r = repo_of(&[
        ("hushrun.yaml".into(), yaml("observe")),
        (
            "tests/test_p.py".into(),
            "import subprocess, sys\nimport pytest\n\n\
             @pytest.fixture(scope='module', params=[1, 2])\ndef p(request):\n    yield request.param\n    if request.param == 1:\n        subprocess.run([sys.executable, '-c', 'pass'])\n\n\
             def test_x(p):\n    assert p\n\ndef test_y(p):\n    assert p\n"
                .into(),
        ),
    ]);
    let python = python();
    // Writes the plugin into the cache.
    let _ = r.run(&pytest_args(&python, &[]), &[]);
    let units = r.root.join("units");
    let run = json!({"schema": "hushrun.units/v0", "root": r.repo, "reuse": [], "unit": "test"});
    let (code, out, facts) = plugin_pytest(&r, &units, run, &["tests/test_p.py"]);
    assert_eq!(code, 0, "{out}");
    let unit = unit_of(&facts[0], "tests/test_p.py");
    for test in unit["tests"].as_array().unwrap() {
        assert_eq!(test["notFollowed"], json!([]), "{test:#}");
    }
    assert!(
        unit["notFollowed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["why"] == "subprocess"),
        "{unit:#}"
    );
}

/// The facts of a run of the plugin in `tests/test_m.py` with `reads: test`, and its exit code
/// and output, with `env` set.
fn watched_facts(r: &Repo, env: &[(&str, &str)], args: &[&str]) -> (i32, String, Value) {
    let units = r.root.join("units");
    let _ = std::fs::remove_dir_all(&units);
    std::fs::create_dir_all(&units).unwrap();
    let run = json!({"schema": "hushrun.units/v0", "root": r.repo, "reuse": [], "unit": "test",
                     "reads": "test"});
    std::fs::write(units.join("hushrun.json"), run.to_string()).unwrap();
    let adapter = std::fs::read_dir(r.cache.join("adapters"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let out = Command::new(python())
        .args(["-m", "pytest", "-q", "-p", "no:cacheprovider"])
        .args(args)
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", &units)
        .env("PYTHONPATH", &adapter)
        .env("PYTEST_ADDOPTS", "-p hushrun_pytest")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let facts = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pytest-")
        })
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .unwrap_or(Value::Null);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        facts,
    )
}

// covers: C-42
#[test]
fn pytest_watches_only_while_no_other_tool_uses_sys_monitoring() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) || !watches() {
        return;
    }
    // test_tool takes sys.monitoring's tool ids 3 and 4 for a tool of its own, as a repository
    // that tests its own tool would. hushrun gives way: it stops watching, says why, and the test
    // passes.
    let r = repo_of(&[
        ("hushrun.yaml".into(), yaml("observe")),
        (
            "tests/test_m.py".into(),
            "import sys\n\n\
             def test_before():\n    assert True\n\n\
             def test_tool():\n    for tool in (3, 4):\n        sys.monitoring.use_tool_id(tool, 'mine')\n    for tool in (3, 4):\n        sys.monitoring.free_tool_id(tool)\n\n\
             def test_after():\n    assert True\n"
                .into(),
        ),
    ]);
    let python = python();
    let _ = r.run(&pytest_args(&python, &[]), &[]);
    let (code, out, facts) = watched_facts(&r, &[], &["tests/test_m.py"]);
    assert_eq!(code, 0, "{out}");
    let watch = facts["settings"]["watch"].as_str().unwrap_or_default();
    assert!(watch.contains("sys.monitoring"), "{facts:#}");
    // It watched no test of the process: each reads its test file's list.
    let unit = unit_of(&facts, "tests/test_m.py");
    for test in unit["tests"].as_array().unwrap() {
        assert!(test.get("ran").is_none(), "{test:#}");
    }
    // Another tool already in use, as coverage.py's sysmon core is under pytest-cov: hushrun
    // doesn't watch at all, and says why.
    let has_cov = Command::new(&python)
        .args(["-c", "import pytest_cov, coverage"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !tools("pytest-cov", has_cov) {
        return;
    }
    let (code, out, facts) = watched_facts(
        &r,
        &[("COVERAGE_CORE", "sysmon")],
        &["--cov=tests", "tests/test_m.py", "-k", "not test_tool"],
    );
    assert_eq!(code, 0, "{out}");
    let watch = facts["settings"]["watch"].as_str().unwrap_or_default();
    assert!(watch.contains("sys.monitoring"), "{facts:#}");
}

// covers: C-32, C-42
#[test]
fn a_sqlite_database_a_test_connects_to_is_read() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) || !watches() {
        return;
    }
    // sqlite3 opens the file in C: Python's only word of it is the audit event
    // `sqlite3.connect`, which names the path. `:memory:` is no file.
    let r = repo_of(&[
        ("hushrun.yaml".into(), yaml("observe")),
        (
            "tests/test_db.py".into(),
            "import os, sqlite3\n\n\
             def test_db(tmp_path):\n    path = os.path.join(os.path.dirname(__file__), 'data', 'shop.sqlite')\n    with sqlite3.connect(path) as c:\n        c.execute('create table if not exists t (x)')\n    sqlite3.connect(':memory:').close()\n"
                .into(),
        ),
        ("tests/data/keep.txt".into(), String::new()),
    ]);
    let python = python();
    let _ = r.run(&pytest_args(&python, &[]), &[]);
    let (code, out, facts) = watched_facts(&r, &[], &["tests/test_db.py"]);
    assert_eq!(code, 0, "{out}");
    let unit = unit_of(&facts, "tests/test_db.py");
    let reads: Vec<&str> = unit["reads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(reads.contains(&"tests/data/shop.sqlite"), "{unit:#}");
    let paths = facts["paths"].as_array().unwrap();
    let opened: Vec<&str> = unit["tests"][0]["opened"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| paths[i.as_u64().unwrap() as usize].as_str().unwrap())
        .collect();
    assert!(opened.contains(&"tests/data/shop.sqlite"), "{unit:#}");
}

fn unit_of<'a>(facts: &'a Value, unit: &str) -> &'a Value {
    facts["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["unit"] == unit)
        .unwrap_or_else(|| panic!("no {unit} in {facts:#}"))
}

// covers: C-39, C-32
#[test]
fn the_plugin_reports_each_test_and_deselects_the_tests_it_is_told() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let mut files = shop("observe");
    // A module-scoped fixture that starts a process: it serves every test of its file, so what it
    // did is the file's, not the first test's.
    files.push((
        "tests/test_fixture.py".into(),
        "import subprocess, sys\nimport pytest\n\n\
         @pytest.fixture(scope='module')\ndef started():\n    subprocess.run([sys.executable, '-c', 'pass'])\n    return 1\n\n\
         def test_x(started):\n    assert started == 1\n\n\
         def test_y(started):\n    assert started == 1\n"
            .into(),
    ));
    let r = repo_of(&files);
    // A run writes the plugin where hushrun keeps it.
    r.run(&pytest_args(&python(), &[]), &[]);
    let units = r.root.join("units");
    let run = json!({"schema": "hushrun.units/v0", "root": r.repo, "unit": "test", "reads": "file",
        "reuse": ["tests/test_greet_1.py"],
        "reuseTests": {"tests/test_cart.py": ["test_one", "test_n[4]", "TestMore::test_three"]},
        "pythonFiles": ["test_*.py", "*_test.py"]});
    let (code, stdout, facts) = plugin_pytest(&r, &units, run.clone(), &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("4 deselected"), "{stdout}");
    let facts = &facts[0];
    assert_eq!(facts["schema"], "hushrun.unit-facts/v1");
    assert_eq!(facts["removedItems"], 4);
    let cart = unit_of(facts, "tests/test_cart.py");
    // The file wasn't deselected whole: some of its tests were, one by one.
    assert_eq!(cart["deselected"], false);
    let tests: Vec<(String, bool, bool)> = cart["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["id"].as_str().unwrap().to_string(),
                t["deselected"].as_bool().unwrap(),
                t["finished"].as_bool().unwrap(),
            )
        })
        .collect();
    let mut sorted = tests.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            ("TestMore::test_three".to_string(), true, false),
            ("test_flips".to_string(), false, true),
            ("test_n[4]".to_string(), true, false),
            ("test_n[5]".to_string(), false, true),
            ("test_one".to_string(), true, false),
            ("test_two".to_string(), false, true),
        ]
    );
    // A file reused whole: its test, deselected with it.
    let greet = unit_of(facts, "tests/test_greet_1.py");
    assert_eq!(greet["deselected"], true);
    assert_eq!(greet["tests"][0]["deselected"], true);
    // A process started inside one test's window is that test's alone.
    let child = unit_of(facts, "tests/test_child.py");
    assert_eq!(child["notFollowed"], json!([]));
    let spawns = child["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "test_spawns")
        .unwrap();
    assert_eq!(spawns["notFollowed"][0]["why"], "subprocess");
    // One started by a fixture that serves the whole file is the file's.
    let fixture = unit_of(facts, "tests/test_fixture.py");
    assert_eq!(
        fixture["notFollowed"][0]["why"], "subprocess",
        "{fixture:#}"
    );
    assert!(
        fixture["tests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["notFollowed"] == json!([])),
        "{fixture:#}"
    );
    // Every test left deselected by it: pytest's "no tests collected" becomes 0, as for files.
    let (code, stdout, _) = plugin_pytest(
        &r,
        &units,
        run.clone(),
        &["tests/test_cart.py", "-k", "one or n or three"],
    );
    assert_eq!(code, 0, "{stdout}");
    // pytest-xdist: each worker deselects the same tests and reports those it ran.
    let (code, stdout, facts) = plugin_pytest(&r, &units, run, &["-n", "2"]);
    assert_eq!(code, 0, "{stdout}");
    let ran: usize = facts
        .iter()
        .filter(|f| f["controller"] != true)
        .map(|f| {
            unit_of(f, "tests/test_cart.py")["tests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| t["finished"] == true)
                .count()
        })
        .sum();
    assert_eq!(ran, 3, "{facts:#?}");
    // With `unit: file`, the plugin's file is as it always was.
    let (code, _, facts) = plugin_pytest(
        &r,
        &units,
        json!({"schema": "hushrun.units/v0", "root": r.repo, "reuse": [],
               "pythonFiles": ["test_*.py", "*_test.py"]}),
        &[],
    );
    assert_eq!(code, 0);
    assert_eq!(facts[0]["schema"], "hushrun.unit-facts/v0");
    assert!(
        unit_of(&facts[0], "tests/test_cart.py")
            .get("tests")
            .is_none()
    );
    let child = unit_of(&facts[0], "tests/test_child.py");
    assert_eq!(child["notFollowed"][0]["why"], "subprocess");
}

fn with_server(results: &FakeResults) -> Vec<(&'static str, String)> {
    vec![
        ("HUSHRUN_BACKEND", results.url()),
        ("HUSHRUN_TOKEN", FakeResults::TOKEN.to_string()),
    ]
}

fn envs<'a>(pairs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    pairs.iter().map(|(n, v)| (*n, v.as_str())).collect()
}

/// A test file of 6,000 tests, one of which fails while `FAIL` is set, and one more test file.
fn many(tests: &str) -> Vec<(String, String)> {
    vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\n".into()),
        (
            "tests/test_many.py".into(),
            "import os\nimport pytest\n\n@pytest.mark.parametrize('n', range(5999))\ndef test_n(n):\n    pass\n\ndef test_flips():\n    assert not os.environ.get('FAIL')\n".into(),
        ),
        ("tests/test_other.py".into(), "def test_other():\n    pass\n".into()),
    ]
}

// covers: C-39, C-12
#[test]
fn an_older_server_keeps_the_first_5000_results_failures_first_and_a_saving_without_a_reused_file()
{
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    results.take_older_limits();
    let server = with_server(&results);
    let r = repo_of(&many("{mode: reuse, unit: test}"));
    let python = python();
    let mut env = envs(&server);
    env.push(("FAIL", "1"));
    // test_many.py failed: its own fail and each of its 6,000 tests, and test_other.py's pass:
    // 6,002 results, past the 5,000 the server takes. They go again, the first 5,000, the two
    // failures first, and the run says once what wasn't kept.
    let first = r.run(&pytest_args(&python, &[]), &env);
    assert_eq!(first.code, 1, "{}", first.stderr);
    assert!(
        first.stderr.contains(
            "hushrun: unit: 1,002 of its 6,002 results of test files and tests weren't kept: acme keeps 5,000 a run"
        ),
        "{}",
        first.stderr
    );
    assert_eq!(results.unit_results(), 5_000);
    assert_eq!(results.unit_results_that("fail"), 2);
    assert_eq!(results.recorded().len(), 1);
    // The flip fixed: test_many.py's other tests, whose passes the server kept, are reused one by
    // one and no test file whole. The server refuses `savedMs` then, and the result goes again
    // without it: it is recorded, with its tests' counts.
    let second = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(second.code, 0, "{}", second.stderr);
    let recorded = results.recorded();
    let newest = recorded
        .iter()
        .find(|r| r["result"] == "pass")
        .expect("the pass is recorded");
    assert!(newest["units"].get("savedMs").is_none(), "{newest:#}");
    assert!(
        newest["units"]["tests"]["reused"].as_u64() > Some(0),
        "{newest:#}"
    );
    assert!(!second.stderr.contains("not recorded"), "{}", second.stderr);
}

// covers: C-12
#[test]
fn a_body_too_large_for_an_older_server_goes_again_as_the_first_5000() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    // A server from before tests, whose limit on a body (1 MiB) came first.
    results.answer(
        "POST",
        "v0/runs/",
        413,
        json!("Failed to buffer the request body: length limit exceeded"),
        1,
    );
    let server = with_server(&results);
    let r = repo_of(&many("{mode: reuse, unit: test}"));
    let mut env = envs(&server);
    env.push(("FAIL", "1"));
    let first = r.run(&pytest_args(&python(), &[]), &env);
    assert_eq!(first.code, 1, "{}", first.stderr);
    assert_eq!(results.unit_results(), 5_000);
    assert_eq!(results.unit_results_that("fail"), 2);
    assert!(
        first.stderr.contains("1,002 of its 6,002 results"),
        "{}",
        first.stderr
    );
}

// covers: C-12
#[test]
fn a_413_in_any_words_goes_again_as_the_first_5000_and_a_second_is_a_refusal() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // A proxy in front of the server with a smaller body limit (nginx's is 1 MiB) answers 413 in
    // its own words. hushrun's body fits 8 MiB, so whoever answered takes less: the first 5,000
    // go again, once.
    let proxy = json!("<html><head><title>413 Request Entity Too Large</title></head></html>");
    for (times, kept) in [(1, 5_000), (2, 0)] {
        let results = FakeResults::start();
        results.answer("POST", "v0/runs/", 413, proxy.clone(), times);
        let server = with_server(&results);
        let r = repo_of(&many("{mode: reuse, unit: test}"));
        let mut env = envs(&server);
        env.push(("FAIL", "1"));
        let first = r.run(&pytest_args(&python(), &[]), &env);
        assert_eq!(first.code, 1, "{}", first.stderr);
        assert_eq!(results.unit_results(), kept, "{}", first.stderr);
        assert_eq!(
            first.stderr.contains("1,002 of its 6,002 results"),
            kept > 0,
            "{}",
            first.stderr
        );
    }
}

/// A runner that writes pytest's facts itself with tests as the unit: `FAKE_UNITS` as its units,
/// a test file the run's `hushrun.json` lists in `reuse`, and a test it lists in `reuseTests`, left
/// out; `failuresByUnit` unless `FAKE_BY_UNIT` is `0`; `staleBuild`, `notKept` and
/// `leftOutUnlisted` from `FAKE_STALE`, `FAKE_NOT_KEPT` and `FAKE_LEFT_OUT`; a `.started` file
/// left with `FAKE_STARTED` as its content; and `FAKE_EXIT` as its exit code.
const RULES_RUNNER: &str = r#"import json, os, sys
d = os.environ["HUSHRUN_UNITS"]
with open(os.path.join(d, "hushrun.json")) as f:
    run = json.load(f)
n = len([x for x in os.listdir(d) if x.startswith("fake-")]) + 1
units = json.loads(os.environ["FAKE_UNITS"])
whole = run.get("reuse") or []
listed = run.get("reuseTests") or {}
for u in units:
    if u["unit"] in whole:
        u.update(deselected=True, finished=0, failed=False)
    for t in u.get("tests", []):
        if u["unit"] in whole or t["id"] in listed.get(u["unit"], []):
            t.update(finished=False, deselected=True, failed=False)
facts = {"schema": "hushrun.unit-facts/v1", "adapter": "pytest",
         "failuresByUnit": os.environ.get("FAKE_BY_UNIT", "1") == "1",
         "settings": {"pythonFiles": ["test_*.py", "*_test.py"]}, "units": units}
for env, key in [("FAKE_STALE", "staleBuild"), ("FAKE_NOT_KEPT", "notKept"),
                 ("FAKE_LEFT_OUT", "leftOutUnlisted")]:
    if os.environ.get(env):
        facts[key] = os.environ[env]
with open(os.path.join(d, "fake-%d.json" % n), "w") as f:
    json.dump(facts, f)
if os.environ.get("FAKE_STARTED"):
    with open(os.path.join(d, "fake-%d.started" % n), "w") as f:
        f.write(os.environ["FAKE_STARTED"])
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
"#;

/// `test_a.py`'s facts: `ran` its tests (`t1`, `t2`, and `t3` when it fails), and `extra` over it.
fn rules_units(tests: Value, extra: Value) -> String {
    let n = tests.as_array().map_or(0, Vec::len);
    let mut unit = json!({"unit": "test_a.py", "selected": n, "finished": n, "reads": ["test_a.py"], "tests": tests});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        unit[k] = v;
    }
    json!([unit, {"unit": "test_b.py", "selected": 1, "finished": 1, "reads": ["test_b.py"],
                  "tests": [{"id": "tb", "finished": true}]}])
    .to_string()
}

// covers: C-32, C-37
#[test]
fn each_rule_that_keeps_a_result_says_why_through_hushrun_why() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let ok = |id: &str| json!({"id": id, "finished": true});
    let failed = json!({"id": "t3", "finished": true, "failed": true});
    // Each case: its facts, its variables, and what `hushrun why` says of t1 and of test_a.py
    // after one run (`None`: another reason, the test file's failure or reuse, comes first).
    type Case = (
        &'static str,
        String,
        Vec<(&'static str, &'static str)>,
        Option<&'static str>,
        Option<&'static str>,
    );
    let cases: Vec<Case> = vec![
        (
            "left out by something else",
            rules_units(
                json!([{"id": "t1", "deselected": true}, ok("t2"), failed.clone()]),
                json!({"failed": true}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("something other than hushrun's plan left it out"),
            None,
        ),
        (
            "a test file left out by something else",
            rules_units(
                json!([]),
                json!({"deselected": true, "selected": 0, "finished": 0}),
            ),
            vec![],
            None,
            Some("something other than hushrun's plan left it out"),
        ),
        (
            "unfinished",
            rules_units(
                json!([{"id": "t1"}, ok("t2"), failed.clone()]),
                json!({"failed": true, "finished": 2}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("it didn't finish in every process that ran it"),
            None,
        ),
        (
            "unfinished, in the adapter's words",
            rules_units(
                json!([{"id": "t1", "notKept": "its process was killed by a signal"}, ok("t2"), failed.clone()]),
                json!({"failed": true, "finished": 2}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("its process was killed by a signal"),
            None,
        ),
        (
            "a test file that didn't run whole",
            rules_units(json!([ok("t1"), {"id": "t2"}]), json!({"finished": 1})),
            vec![],
            None,
            Some("its test t2 kept no result: it didn't finish in every process that ran it"),
        ),
        (
            "a start that died",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![("FAKE_STARTED", "4242")],
            Some(
                "a start of its runner ended before it said what it saw, so no pass of that run is kept",
            ),
            Some(
                "a start of its runner ended before it said what it saw, so no pass of that run is kept",
            ),
        ),
        (
            "a second start, in its adapter's words",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![(
                "FAKE_STARTED",
                r#"{"why": "Jest started more than once in its command"}"#,
            )],
            Some("Jest started more than once in its command, so no pass of that run is kept"),
            Some("Jest started more than once in its command, so no pass of that run is kept"),
        ),
        (
            "a skip in a failed run",
            rules_units(
                json!([{"id": "t1", "finished": true, "skipped": true}, ok("t2")]),
                json!({}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("it was skipped in a run that failed"),
            Some("its test t1 kept no result: it was skipped in a run that failed"),
        ),
        (
            "a failure no test explains",
            rules_units(json!([ok("t1"), ok("t2")]), json!({"failed": true})),
            vec![("FAKE_EXIT", "1")],
            Some("its test file failed while none of its tests did"),
            None,
        ),
        (
            "a stale build in a run that passed",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![("FAKE_STALE", "build/prog is older than its sources")],
            Some("it ran on a build older than these files"),
            Some("it ran on a build older than these files"),
        ),
        (
            "a failed run whose failures aren't by test",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![("FAKE_EXIT", "1"), ("FAKE_BY_UNIT", "0")],
            Some("its run failed, and its adapter doesn't report each failure by test,"),
            Some("its run failed, and its adapter doesn't report each failure by test file"),
        ),
        (
            "a failed run, in its adapter's words",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![
                ("FAKE_EXIT", "1"),
                ("FAKE_BY_UNIT", "0"),
                ("FAKE_NOT_KEPT", "an error no test owns stopped the run"),
            ],
            Some("an error no test owns stopped the run"),
            Some("an error no test owns stopped the run"),
        ),
        (
            "a test its adapter never leaves out",
            rules_units(
                json!([{"id": "t1", "finished": true, "neverLeftOut": true}, ok("t2"), failed.clone()]),
                json!({"failed": true}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("its adapter can't leave it out of a run"),
            None,
        ),
    ];
    let python = python();
    for (name, units, env, test, file) in cases {
        let r = Repo::new(&[
            ("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
            (".gitignore", "__pycache__/\n"),
            ("fake.py", RULES_RUNNER),
            ("test_a.py", "def t1():\n    pass\n"),
            ("test_b.py", "def tb():\n    pass\n"),
        ]);
        let mut vars = vec![("FAKE_UNITS", units.as_str())];
        vars.extend(env.iter().copied());
        let ran = r.run(&["run", "unit", "--", &python, "fake.py"], &vars);
        assert!(
            ran.stderr.contains("hushrun: unit"),
            "{name}: {}",
            ran.stderr
        );
        for (asked, want) in [("test_a.py::t1", test), ("test_a.py", file)] {
            let Some(want) = want else { continue };
            let why = why_json(&r, asked);
            assert_eq!(
                why["reason"], "not-kept",
                "{name}, {asked}: {why:#}\n{}",
                ran.stderr
            );
            let says = why["says"].as_str().unwrap_or_default();
            assert!(
                says.starts_with(&format!("{asked} runs: ")) && says.contains(want),
                "{name}, {asked}: {says}"
            );
        }
    }
}

// covers: C-32, C-37
#[test]
fn a_run_left_out_twice_says_why_through_hushrun_why() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // Four test files pass; then test_b.py changes, and the plan lists the others but the one
    // an audit runs. Every start of the runner says it left out what the plan didn't list, the
    // start with nothing listed too: nothing of the run is kept, and `hushrun why` says so.
    let python = python();
    let names = ["test_a.py", "test_b.py", "test_c.py", "test_d.py"];
    let mut files = vec![
        (
            "hushrun.yaml".to_string(),
            yaml("{mode: reuse, unit: test}"),
        ),
        (".gitignore".to_string(), "__pycache__/\n".to_string()),
        ("fake.py".to_string(), RULES_RUNNER.to_string()),
    ];
    for name in names {
        files.push((name.to_string(), "def t1():\n    pass\n".to_string()));
    }
    let r = repo_of(&files);
    let units = json!(
        names
            .iter()
            .map(
                |u| json!({"unit": u, "selected": 1, "finished": 1, "reads": [u],
                        "tests": [{"id": "t1", "finished": true}]})
            )
            .collect::<Vec<_>>()
    )
    .to_string();
    let first = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[("FAKE_UNITS", &units)],
    );
    assert_eq!(first.code, 0, "{}", first.stderr);
    r.write("test_b.py", "def t1():\n    pass\n# changed\n");
    let again = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[
            ("FAKE_UNITS", &units),
            ("FAKE_LEFT_OUT", "a feature nobody listed"),
        ],
    );
    assert!(again.stderr.contains("wasn't recorded"), "{}", again.stderr);
    for asked in ["test_b.py", "test_b.py::t1"] {
        let why = why_json(&r, asked);
        assert_eq!(why["reason"], "not-kept", "{why:#}\n{}", again.stderr);
        assert!(
            why["says"]
                .as_str()
                .unwrap()
                .contains("even when it ran again with nothing listed"),
            "{why:#}"
        );
    }
}

/// `hushrun why unit <asked> --format json`, read.
fn why_json(r: &Repo, asked: &str) -> Value {
    let ran = r.run(&["why", "unit", asked, "--format", "json"], &[]);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    serde_json::from_str(&ran.stdout).unwrap_or_else(|_| panic!("{}", ran.stdout))
}

// covers: C-37, C-39
#[test]
fn why_explains_one_test_and_a_test_file_with_a_test_not_followed() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let child = "import os, subprocess, sys\nfrom app.cart import total\n\n\
                 def test_spawns():\n    if os.environ.get('SPAWN'):\n        subprocess.run([sys.executable, '-c', 'pass'])\n\n\
                 def test_a():\n    assert total([1]) == 1\n";
    let files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("app/__init__.py".into(), String::new()),
        (
            "app/cart.py".into(),
            "def total(xs):\n    return sum(xs)\n".into(),
        ),
        (
            "tests/test_cart.py".into(),
            "import os\nfrom app.cart import total\n\n\
             def test_one():\n    assert total([1]) == 1\n\n\
             def test_flips():\n    assert not os.environ.get('FAIL')\n"
                .into(),
        ),
        ("tests/test_child.py".into(), child.into()),
    ];
    let r = repo_of(&files);
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);

    // test_cart.py failed: each of its tests is explained on its own.
    let one = why_json(&r, "tests/test_cart.py::test_one");
    assert_eq!(
        (&one["unit"], &one["test"], &one["decision"], &one["reason"]),
        (
            &json!("tests/test_cart.py"),
            &json!("test_one"),
            &json!("reused"),
            &json!("reused")
        ),
        "{one:#}"
    );
    assert_eq!(
        one["says"],
        "tests/test_cart.py::test_one would be reused: it passed on the files it read, and none of them has changed since"
    );
    let flips = why_json(&r, "tests/test_cart.py::test_flips");
    assert_eq!(
        flips["says"],
        "tests/test_cart.py::test_flips runs: its newest result on the files it reads now failed"
    );
    let nope = why_json(&r, "tests/test_cart.py::test_nope");
    assert_eq!(nope["reason"], "never-ran-here", "{nope:#}");
    // test_child.py passed whole: its tests go with it; \[C-37\] a test it doesn't have doesn't.
    let none = why_json(&r, "tests/test_child.py::test_nope");
    assert_eq!(none["reason"], "never-ran-here", "{none:#}");
    let a = why_json(&r, "tests/test_child.py::test_a");
    assert_eq!(
        a["says"],
        "tests/test_child.py::test_a would be reused: its test file passed whole on the files it read, and none of them has changed since"
    );

    // test_spawns starts a process in a run where test_child.py changed; then test_child.py is
    // as it was, so its own key has its pass, but it isn't reused whole.
    r.write("tests/test_child.py", &format!("{child}# changed\n"));
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("SPAWN", "1")]).code, 0);
    r.write("tests/test_child.py", child);
    let whole = why_json(&r, "tests/test_child.py");
    assert_eq!(whole["decision"], "runs", "{whole:#}");
    assert_eq!(whole["reason"], "test-not-observed", "{whole:#}");
    assert!(
        whole["says"]
            .as_str()
            .unwrap()
            .contains("its test test_spawns read the last time it ran here"),
        "{whole:#}"
    );
    let spawns = why_json(&r, "tests/test_child.py::test_spawns");
    assert_eq!(spawns["reason"], "not-observed", "{spawns:#}");
    let a = why_json(&r, "tests/test_child.py::test_a");
    assert!(
        a["detail"].as_array().unwrap().iter().any(|l| l
            .as_str()
            .unwrap()
            .starts_with("its test file isn't reused whole")),
        "{a:#}"
    );
}

// covers: C-22, C-39
#[test]
fn ready_counts_tests_where_tests_are_the_unit() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let python = python();
    let mut files = shop("{mode: reuse, unit: test}");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"hushrun run unit -- {python} -m pytest -q -p no:cacheprovider\"}}}}\n"
        ),
    ));
    let r = repo_of(&files);
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);
    // cart.py changes, committed: what a run would do is said with tests, as its line says it.
    r.write(
        "app/cart.py",
        "def total(xs):\n    return sum(xs)  # changed\n",
    );
    git::commit_all(&r.repo, "change");
    let ready = r.run(&["ready"], &[]);
    let line = ready
        .stdout
        .lines()
        .find(|l| l.contains("a run here would"))
        .unwrap_or_else(|| panic!("{}\n{}", ready.stdout, ready.stderr))
        .trim()
        .to_string();
    assert!(
        line.ends_with(
            "a run here would run 11 of 13 tests, in 3 of 5 test files; 2 already passed here on the files they read"
        ),
        "{line}"
    );
}

// covers: C-39
#[test]
fn observe_counts_the_tests_it_couldnt_follow_where_it_counts_tests() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml("{mode: observe, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        (
            "tests/test_child.py".into(),
            "import subprocess, sys\n\n\
             def test_one():\n    subprocess.run([sys.executable, '-c', 'pass'])\n\n\
             def test_two():\n    subprocess.run([sys.executable, '-c', 'pass'])\n\n\
             def test_ok():\n    assert True\n"
                .into(),
        ),
    ];
    let r = repo_of(&files);
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &[]).code, 0);
    // A new test file: the check runs again, and test_ok would have been reused.
    r.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let second = r.run(&pytest_args(&python, &[]), &[]);
    let said = line(&second, "hushrun: unit: with `tests: reuse`");
    assert!(
        said.starts_with("hushrun: unit: with `tests: reuse`, 3 of 4 tests would have run"),
        "{said}"
    );
    assert!(
        said.contains("; hushrun couldn't follow all that 2 of them read"),
        "{said}"
    );
    assert!(said.ends_with("so they would always run"), "{said}");
}

// covers: C-39, C-32
#[test]
fn with_xdist_each_worker_leaves_out_the_tests_the_plan_lists() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // The first test's runs, with two xdist workers: each worker leaves out the tests
    // `reuseTests` lists and reports its own, and the run comes to the same plan and result.
    let r = repo_of(&shop("{mode: reuse, unit: test}"));
    let python = python();
    let xdist = ["-n", "2"];
    let first = r.run(&pytest_args(&python, &xdist), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let second = r.run(&pytest_args(&python, &xdist), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        line(&second, "hushrun: unit: "),
        "hushrun: unit: 4 of 13 tests run, in 3 of 5 test files; 9 already passed on the files they read, and 2 more that already passed run anyway, as a check"
    );
    assert!(second.stdout.contains("4 passed"), "{}", second.stdout);
    let result = r.newest_result();
    assert_eq!(result["result"], "pass");
    assert_eq!(
        result["units"]["tests"],
        json!({"total": 13, "ran": 4, "reused": 9}),
        "{}",
        second.stderr
    );
}

// covers: C-39
#[test]
fn a_plan_that_doesnt_know_every_test_files_tests_counts_test_files() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // A test file new since the last run: its tests aren't known here, so the line counts test
    // files, with the tests reused one by one beside them.
    let r = repo_of(&shop("{mode: reuse, unit: test}"));
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);
    r.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let second = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        line(&second, "hushrun: unit: "),
        "hushrun: unit: 4 of 6 test files run; 2 already passed on the files they read, and so did 7 tests of the others, and 2 more that already passed run anyway, as a check"
    );
}

// covers: C-39
#[test]
fn past_50000_tests_a_run_counts_by_test_file_and_says_so() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml("{mode: observe, unit: test}")),
        (
            ".gitignore".into(),
            "__pycache__/\n.pytest_cache/\n".into(),
        ),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\n".into(),
        ),
        (
            "tests/test_big.py".into(),
            "import pytest\n\n@pytest.mark.parametrize('n', range(50001))\ndef test_n(n):\n    pass\n"
                .into(),
        ),
    ];
    let r = repo_of(&files);
    let ran = r.run(&pytest_args(&python(), &[]), &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.lines().any(|l| l
            == "hushrun: unit: its tests are reused by test file in this run: it has 50,001 tests, more than the 50,000 this version reuses one by one"),
        "{}",
        ran.stderr
    );
    // Counted by test file: the run's units say nothing of tests, and its test file recorded its
    // own pass.
    let units = &r.newest_result()["units"];
    assert!(units.get("tests").is_none(), "{units}");
    assert_eq!(units["ran"], 1, "{units}");
}
