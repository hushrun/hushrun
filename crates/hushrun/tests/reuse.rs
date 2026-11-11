//! \[ADR 0069\] `tests: reuse` (C-5, C-10, C-26, C-27, C-32 to C-34): inside a run whose check
//! missed, the test files that already passed on the files they read aren't run again; audit runs
//! check that, and a disagreement suspends reuse until a person turns it on again.
//!
//! The pytest and Cargo fixtures need Python with pytest and pytest-xdist, and cargo-nextest. They
//! say so and pass where those are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

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

impl Ran {
    /// The line that says what the plan reuses, or why every test file runs.
    fn plan_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| {
                l.contains("test files run")
                    || l.contains("test file run")
                    || l.contains("every test file runs")
            })
            .unwrap_or_else(|| panic!("no plan line in:\n{}", self.stderr))
    }
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

fn has_nextest() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: {REPOSITORY}\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A shop: two modules each read by one test file, a greeting read by five, a test file that
/// starts a process, and a lockfile in the rest set.
fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (
            ".gitignore".into(),
            "__pycache__/\n.pytest_cache/\nflag\nflaky-seen\n".into(),
        ),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("requirements.txt".into(), "nothing==1.0\n".into()),
        ("app/__init__.py".into(), String::new()),
        ("app/pricing.py".into(), "def price(x):\n    return x * 2\n".into()),
        (
            "app/cart.py".into(),
            "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)\n"
                .into(),
        ),
        ("app/greet.py".into(), "def hello():\n    return 'hi'\n".into()),
        (
            "tests/test_cart.py".into(),
            "from app.cart import total\n\ndef test_total():\n    assert total([1, 2]) == 6\n".into(),
        ),
        (
            "tests/test_pricing.py".into(),
            "from app.pricing import price\n\ndef test_price():\n    assert price(3) == 6\n".into(),
        ),
        (
            "tests/test_child.py".into(),
            "import subprocess, sys\n\ndef test_child():\n    assert subprocess.run([sys.executable, '-c', 'pass']).returncode == 0\n".into(),
        ),
    ];
    for n in 1..=5 {
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

// covers: C-34, C-10, C-27, C-33
#[test]
fn pytest_reuses_what_already_passed_on_the_files_it_read() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&shop("reuse"));
    let python = python();
    let pytest = |extra: &[&str]| r.run(&pytest_args(&python, extra), &[]);

    // The first run: nothing passed here yet, so every test file runs.
    let first = pytest(&[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.plan_line(),
        "hushrun: unit: all 8 test files run: none had passed here yet on the files it read"
    );
    assert_eq!(r.unit_results(), 7, "every observed test file that passed");

    // One module changes: the test file that read it runs, with one audited test file of the six
    // that could be reused, and the one that starts a process, which is never reused.
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let changed = pytest(&[]);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that did runs as a check"
    );
    assert!(
        changed.stdout.contains("5 deselected"),
        "{}",
        changed.stdout
    );
    assert!(
        changed.stderr.contains("unit passed in") && changed.stderr.contains("5 test files"),
        "{}",
        changed.stderr
    );
    // A pass with reused test files is a pass (D2), with its counts.
    let result = r.newest_result();
    assert_eq!(result["result"], "pass");
    assert_eq!(result["units"], json!({"total": 8, "ran": 3, "reused": 5}));
    // Reuse never chains: a reused test file records nothing.
    assert_eq!(r.unit_results(), 7 + 3 - 1, "{}", changed.stderr);
    // The same code again skips the whole check (C-10).
    let again = pytest(&[]);
    assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);

    // The rest set changes: any test may read it, so every test file runs.
    r.write("requirements.txt", "nothing==2.0\n");
    let rest = pytest(&[]);
    assert_eq!(
        rest.plan_line(),
        "hushrun: unit: all 8 test files run: requirements.txt changed, and any test may read it"
    );
    // A new source can change what tests load: every test file runs.
    r.write("app/extra.py", "X = 1\n");
    let new = pytest(&[]);
    assert_eq!(
        new.plan_line(),
        "hushrun: unit: all 8 test files run: app/extra.py is new, which can change what tests load"
    );
    // --force runs everything, and says so.
    r.write("app/extra.py", "X = 2\n");
    let forced = r.run(
        &[
            "run",
            "--force",
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
    assert_eq!(
        forced.plan_line(),
        "hushrun: unit: every test file runs: --force runs it whole"
    );
    assert_eq!(r.newest_result()["units"]["ranAnyway"], "force");
}

// covers: C-34, C-5
#[test]
fn tests_always_and_not_observed_test_files_always_run() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&shop(
        "{mode: reuse, always: [\"tests/test_greet_1.py\", \"tests/test_greet_2.py\"]}",
    ));
    let python = python();
    let pytest = || r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let changed = pytest();
    // test_cart (changed), test_child (not observed), the two `always` names, and one audited of
    // the three left: five run.
    assert_eq!(
        changed.plan_line(),
        "hushrun: unit: 5 of 8 test files run; 3 already passed on the files they read, and 1 more that did runs as a check"
    );
}

// covers: C-33, C-34
#[test]
fn a_failing_run_records_what_ran_whole_and_the_fix_reuses_it() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&shop("reuse"));
    let python = python();
    let pytest = || r.run(&pytest_args(&python, &[]), &[]);
    r.write("app/cart.py", "def total(xs):\n    return 0\n");
    let failing = pytest();
    assert_eq!(failing.code, 1, "{}", failing.stderr);
    // Every test file that ran whole and passed is recorded, and the failing one's fail; the one
    // that starts a process, nothing.
    assert_eq!(r.unit_results(), 7, "{}", failing.stderr);
    r.write(
        "app/cart.py",
        "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)\n",
    );
    let fixed = pytest();
    assert_eq!(fixed.code, 0, "{}", fixed.stderr);
    assert_eq!(
        fixed.plan_line(),
        "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that did runs as a check"
    );
}

/// A test file that fails when a file outside the check's files exists: what no adapter can see,
/// the missed dependency an audit run is for; and another, read by its own module.
fn hidden(tests: &str) -> Vec<(String, String)> {
    vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (
            ".gitignore".into(),
            "__pycache__/\n.pytest_cache/\nflag\nseen\n".into(),
        ),
        ("liba.py".into(), "X = 1\n".into()),
        ("libb.py".into(), "Y = 1\n".into()),
        (
            "test_hidden.py".into(),
            "import os, liba\n\ndef test_x():\n    assert not os.path.exists(os.path.join(os.path.dirname(__file__), 'flag'))\n".into(),
        ),
        (
            "test_other.py".into(),
            "import libb\n\ndef test_y():\n    assert libb.Y == 1\n".into(),
        ),
    ]
}

// covers: C-34
#[test]
fn an_injected_missed_dependency_is_caught_by_an_audit_and_suspends_reuse() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let pytest = || r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    // The recorded pass of test_hidden.py is wrong for what it reads now: the flag, which its key
    // can't see. Only it could be reused, so it is the one audited.
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    let caught = pytest();
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        caught.plan_line(),
        "hushrun: unit: all 2 test files run; 1 that already passed on the files it read runs as a check"
    );
    assert!(
        caught.stderr.contains("unit: test_hidden.py failed, though it passed before on the same files it read: running it once more"),
        "{}",
        caught.stderr
    );
    assert!(
        caught.stderr.contains("unit: test_hidden.py failed again, though it passed before on the same files it read: reuse of unit's test files is suspended until a person turns it on again with `hushrun tests resume unit`"),
        "{}",
        caught.stderr
    );
    // The next run reuses nothing, and says why.
    r.write("libb.py", "Y = 1  # again\n");
    let suspended = pytest();
    assert!(
        suspended.plan_line().starts_with(
            "hushrun: unit: every test file runs: reuse of its test files is suspended by a person"
        ),
        "{}",
        suspended.stderr
    );
    assert_eq!(r.newest_result()["units"]["ranAnyway"], "suspended");
    // A coding agent can't turn it on again; a person can, once.
    let agent = r.run(&["tests", "resume", "unit"], &[("CLAUDECODE", "1")]);
    assert_eq!(agent.code, 2, "{}", agent.stderr);
    assert!(
        agent.stderr.contains("a person turns reuse"),
        "{}",
        agent.stderr
    );
    std::fs::remove_file(r.repo.join("flag")).unwrap();
    let resumed = r.run(&["tests", "resume", "unit"], &[]);
    assert_eq!(resumed.code, 0, "{}", resumed.stderr);
    assert!(
        resumed.stdout.contains("was suspended by a person")
            && resumed.stdout.contains("when test_hidden.py failed twice")
            && resumed.stdout.contains("Turned it on again"),
        "{}",
        resumed.stdout
    );
    let twice = r.run(&["tests", "resume", "unit"], &[]);
    assert!(
        twice.stdout.contains("is on for example.com/acme/shop"),
        "{}",
        twice.stdout
    );
    // On again: test_hidden.py's newest result is the fail its audit recorded, so it runs; and a
    // pass then makes it reusable again.
    r.write("libb.py", "Y = 1  # on again\n");
    let on = pytest();
    assert_eq!(on.code, 0, "{}", on.stderr);
    assert_eq!(
        on.plan_line(),
        "hushrun: unit: all 2 test files run: none had passed here yet on the files it read"
    );
}

// covers: C-34
#[test]
fn a_test_file_that_passes_when_it_runs_again_is_flaky_and_suspends_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let mut files = hidden("reuse");
    // It fails the first time it runs after `seen` is gone, and passes after.
    files[4].1 = "import os, liba\nSEEN = os.path.join(os.path.dirname(__file__), 'seen')\n\ndef test_x():\n    if os.path.exists(SEEN):\n        return\n    open(SEEN, 'w').close()\n    assert False, 'first time'\n".into();
    let r = repo_of(&files);
    r.write("seen", "");
    let python = python();
    let pytest = || r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    std::fs::remove_file(r.repo.join("seen")).unwrap();
    r.write("libb.py", "Y = 1  # changed\n");
    let flaky = pytest();
    // COMMAND failed, and its exit code is hushrun's; the second run passed: flaky, as G13 says of
    // checks, and reuse stays on.
    assert_eq!(flaky.code, 1, "{}", flaky.stderr);
    assert!(
        flaky.stderr.contains(
            "unit: test_hidden.py failed, then passed when it ran again: it may be flaky"
        ),
        "{}",
        flaky.stderr
    );
    assert!(!flaky.stderr.contains("suspended"), "{}", flaky.stderr);
}

// covers: C-32
#[test]
fn files_that_change_while_command_runs_record_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let ran = r.run(
        &[
            "run",
            "unit",
            "--",
            "sh",
            "-c",
            &format!("{python} -m pytest -q -p no:cacheprovider && echo 'Z = 1' > libb.py"),
        ],
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains("files changed while it ran"),
        "{}",
        ran.stderr
    );
    assert_eq!(r.unit_results(), 0);
}

/// pytest with hushrun's plugin and a `hushrun.json` written here, as a plan would.
fn plugin_pytest(r: &Repo, units: &Path, reuse: &[&str], args: &[&str]) -> (i32, Vec<Value>) {
    let _ = std::fs::remove_dir_all(units);
    std::fs::create_dir_all(units).unwrap();
    std::fs::write(
        units.join("hushrun.json"),
        json!({"schema": "hushrun.units/v0", "root": r.repo, "reuse": reuse,
               "pythonFiles": ["test_*.py", "*_test.py"]})
        .to_string(),
    )
    .unwrap();
    // The plugin as hushrun writes it, from a run in observe.
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
    (out.status.code().unwrap_or(-1), facts)
}

// covers: C-32
#[test]
fn the_plugin_deselects_what_it_is_told_and_no_tests_collected_stays_its_own() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let mut files = hidden("observe");
    files.push((
        "test_more.py".into(),
        "def test_one():\n    pass\n\ndef test_two():\n    pass\n".into(),
    ));
    let r = repo_of(&files);
    // A run in observe writes the plugin where hushrun keeps it.
    r.run(&pytest_args(&python(), &[]), &[]);
    let units = r.root.join("units");
    // Every test left is deselected by the plugin: pytest's "no tests collected" (5) would turn a
    // pass into a fail, so it is 0; and the plugin says exactly what it deselected.
    let (code, facts) = plugin_pytest(&r, &units, &["test_more.py"], &["-k", "one"]);
    assert_eq!(code, 0);
    let more = facts[0]["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["unit"] == "test_more.py")
        .unwrap()
        .clone();
    assert_eq!(more["deselected"], true);
    assert_eq!(facts[0]["removedItems"], 1);
    // Nothing selected without it: 5 stays 5.
    let (code, _) = plugin_pytest(&r, &units, &["test_more.py"], &["-k", "nothing_matches"]);
    assert_eq!(code, 5);
    // A failure among what's left stays a failure.
    r.write("flag", "");
    let (code, _) = plugin_pytest(&r, &units, &["test_more.py", "test_other.py"], &[]);
    assert_eq!(code, 1);
    std::fs::remove_file(r.repo.join("flag")).unwrap();
    // pytest-xdist: its workers deselect, and the controller makes 5 a 0 as above.
    let (code, facts) = plugin_pytest(&r, &units, &["test_more.py"], &["-n", "2", "-k", "one"]);
    assert_eq!(code, 0, "{facts:?}");
    assert!(facts.iter().any(|f| f["removedItems"].as_u64() > Some(0)));
    // A pytest that didn't claim the directory, as one a test starts, deselects nothing.
    std::fs::remove_dir_all(&units).unwrap();
    std::fs::create_dir_all(&units).unwrap();
    let (code, facts) = {
        std::fs::write(units.join("claim"), "1").unwrap();
        let adapter = std::fs::read_dir(r.cache.join("adapters"))
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        std::fs::write(
            units.join("hushrun.json"),
            json!({"schema": "hushrun.units/v0", "root": r.repo, "reuse": ["test_more.py"],
                   "pythonFiles": ["test_*.py", "*_test.py"]})
            .to_string(),
        )
        .unwrap();
        let out = Command::new(python())
            .args(["-m", "pytest", "-q", "-p", "no:cacheprovider"])
            .current_dir(&r.repo)
            .env("HUSHRUN_UNITS", &units)
            .env("PYTHONPATH", adapter)
            .env("PYTEST_ADDOPTS", "-p hushrun_pytest")
            .output()
            .unwrap();
        let facts: Vec<Value> = std::fs::read_dir(&units)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("pytest-")
            })
            .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
            .collect();
        (out.status.code().unwrap_or(-1), facts)
    };
    assert_eq!(code, 0);
    assert_eq!(facts[0]["removedItems"], 0);
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

// covers: C-33, C-34
#[test]
fn a_server_keeps_test_files_with_their_run_and_ci_reuses_them_with_the_map_alone() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let server = with_server(&results);
    let r = repo_of(&shop("reuse"));
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(first.code, 0, "{}", first.stderr);
    // The run and its test files went in one request, and nothing of them stays here.
    assert_eq!(results.unit_results(), 7);
    assert!(
        results
            .requests()
            .iter()
            .any(|s| s.path.starts_with("v0/runs/"))
    );
    assert_eq!(r.unit_results(), 0);
    // Another machine, as CI is: an empty cache, but the map, which CI's cache carries (D3).
    let ci = Repo {
        _dir: tempfile::tempdir().unwrap(),
        root: r.root.clone(),
        repo: r.repo.clone(),
        cache: r.root.join("ci-cache"),
    };
    std::fs::create_dir_all(ci.cache.join("units")).unwrap();
    for entry in std::fs::read_dir(r.cache.join("units")).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".map.json") {
            std::fs::copy(entry.path(), ci.cache.join("units").join(name)).unwrap();
        }
    }
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let reused = ci.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert_eq!(
        reused.plan_line(),
        "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that did runs as a check"
    );
    assert!(
        results
            .recorded()
            .iter()
            .any(|r| r["units"] == json!({"total": 8, "ran": 3, "reused": 5})),
        "{:?}",
        results.recorded()
    );
    assert!(
        results
            .requests()
            .iter()
            .any(|s| s.path == "v0/units/lookup")
    );
}

// covers: C-34
#[test]
fn a_server_keeps_the_suspension_and_a_person_resumes_it_there() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let server = with_server(&results);
    let r = repo_of(&hidden("reuse"));
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &envs(&server)).code, 0);
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    let caught = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        results.reuse_state(REPOSITORY, "unit")["state"],
        "suspended"
    );
    r.write("libb.py", "Y = 1  # again\n");
    let suspended = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert!(
        suspended
            .plan_line()
            .contains("reuse of its test files is suspended"),
        "{}",
        suspended.stderr
    );
    let resumed = r.run(&["tests", "resume", "unit"], &envs(&server));
    assert_eq!(resumed.code, 0, "{}", resumed.stderr);
    assert_eq!(results.reuse_state(REPOSITORY, "unit")["state"], "on");
}

// covers: C-33
#[test]
fn an_older_server_or_one_that_doesnt_answer_means_every_test_file_runs() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    results.forget_units();
    let server = with_server(&results);
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let old = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(old.code, 0, "{}", old.stderr);
    assert_eq!(
        old.plan_line(),
        "hushrun: unit: every test file runs: can't read which passed (its server doesn't keep test files' results yet)"
    );
    // The run itself is recorded, as a server from before takes it, and said once.
    assert_eq!(results.recorded().len(), 1);
    assert!(
        old.stderr.contains("test files' results weren't kept"),
        "{}",
        old.stderr
    );
    // A server that can't be reached: the check runs, and so does every test file.
    let gone = [
        ("HUSHRUN_BACKEND", "http://127.0.0.1:9/o/acme".to_string()),
        ("HUSHRUN_TOKEN", FakeResults::TOKEN.to_string()),
    ];
    r.write("libb.py", "Y = 1  # changed\n");
    let down = r.run(&pytest_args(&python, &[]), &envs(&gone));
    assert_eq!(down.code, 0, "{}", down.stderr);
    assert!(
        down.plan_line()
            .starts_with("hushrun: unit: every test file runs: can't read which passed"),
        "{}",
        down.stderr
    );
}

// covers: C-26, C-34
#[test]
fn in_ci_where_a_later_step_reads_reports_every_test_file_runs() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let mut env = with_server(&results);
    env.push(("CI", "true".to_string()));
    env.push(("HUSHRUN_REPORTS_READ", "true".to_string()));
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let ran = r.run(&pytest_args(&python, &[]), &envs(&env));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        ran.plan_line(),
        "hushrun: unit: every test file runs: HUSHRUN_REPORTS_READ says this job reads reports, and a test file it reuses writes none"
    );
    assert!(
        results
            .recorded()
            .iter()
            .any(|r| r["units"]["ranAnyway"] == "reports"),
        "{:?}",
        results.recorded()
    );
}

/// A Cargo workspace: a library and its tests, a binary crate whose integration test runs it, a
/// second library, and a crate whose one integration test fails when a file outside the
/// workspace's files exists.
fn rust_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/ws\nchecks:\n  test:\n    tests: reuse\n",
        ),
        (".gitignore", "target/\nflag\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/core\", \"crates/app\", \"crates/other\", \"crates/flagged\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/core/Cargo.toml",
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"core_lib\"\n",
        ),
        (
            "crates/core/src/lib.rs",
            "pub fn answer() -> u32 { 42 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
        ),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
        ),
        (
            "crates/app/src/main.rs",
            "fn main() { println!(\"{}\", core_lib::answer()); }\n",
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
            "crates/flagged/Cargo.toml",
            "[package]\nname = \"flagged\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ntest = false\n",
        ),
        ("crates/flagged/src/lib.rs", "pub fn one() -> u32 { 1 }\n"),
        (
            "crates/flagged/tests/reads_flag.rs",
            "#[test]\nfn no_flag() {\n    let flag = concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../flag\");\n    assert!(!std::path::Path::new(flag).exists());\n}\n",
        ),
    ]
}

// covers: C-34, C-32
#[test]
fn rust_test_binaries_are_reused_and_an_audit_catches_a_missed_dependency() {
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
    let nextest = [
        "run",
        "test",
        "--",
        "cargo",
        "nextest",
        "run",
        "--workspace",
        "--locked",
    ];
    let first = r.run(&nextest, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.plan_line(),
        "hushrun: test: all 5 test files run: none had passed here yet on the files it read"
    );
    // One crate changes: its test binary runs, with one audited of the four others.
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 1 + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    let changed = r.run(&nextest, &[]);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: test: 2 of 5 test files run; 3 already passed on the files they read, and 1 more that did runs as a check"
    );
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 5, "ran": 2, "reused": 3})
    );
    // Plain `cargo test` goes through the same runner.
    let plain = [
        "run",
        "test",
        "--",
        "cargo",
        "test",
        "--workspace",
        "--locked",
    ];
    assert_eq!(r.run(&plain, &[]).code, 0);
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    let second = r.run(&plain, &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        second.plan_line(),
        "hushrun: test: 2 of 5 test files run; 3 already passed on the files they read, and 1 more that did runs as a check"
    );

    // The flag: what the flagged test reads and no dep-info names. Every other binary changes, so
    // the flagged one is the only one that could be reused, and the one audited.
    r.write("flag", "");
    r.write(
        "crates/core/src/lib.rs",
        "pub fn answer() -> u32 { 40 + 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
    );
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 2 * 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    let caught = r.run(&nextest, &[]);
    assert_ne!(caught.code, 0, "{}", caught.stderr);
    assert!(
        caught.stderr.contains("test: crates/flagged/tests/reads_flag.rs failed again, though it passed before on the same files it read: reuse of test's test files is suspended"),
        "{}",
        caught.stderr
    );
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 1 * 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    let suspended = r.run(&nextest, &[]);
    assert!(
        suspended
            .plan_line()
            .contains("every test file runs: reuse of its test files is suspended"),
        "{}",
        suspended.stderr
    );
}
