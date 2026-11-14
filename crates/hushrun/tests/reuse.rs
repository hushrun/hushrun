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

impl Ran {
    /// The line that says what the plan reuses, or why every test file runs.
    fn plan_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| {
                // \[C-39\] In any adapter's word: `test files`, `test binaries`, `packages`.
                [
                    "test file",
                    "test files",
                    "test binary",
                    "test binaries",
                    "package",
                    "packages",
                    "test program",
                    "test programs",
                    "test class",
                    "test classes",
                    "test target",
                    "test targets",
                ]
                .iter()
                .any(|w| l.contains(&format!("{w} run")))
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

    // The first run: every test file runs, and \[ADR 0074\] the plan says nothing, since no
    // pytest has reported this check's test files here yet: the COMMAND might not start one.
    let first = pytest(&[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(!first.stderr.contains("test files run"), "{}", first.stderr);
    assert_eq!(r.unit_results(), 7, "every observed test file that passed");

    // One module changes: the test file that read it runs, with one audited test file of the six
    // that could be reused, and the one that starts a process, which is never reused.
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let changed = pytest(&[]);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
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
    // \[ADR 0074\] With what reusing saved: the reused test files' recorded durations, added up.
    let saved = result["units"]["savedMs"].as_u64().expect("savedMs");
    assert_eq!(
        result["units"],
        json!({"total": 8, "ran": 3, "reused": 5, "savedMs": saved})
    );
    // Reuse never chains: a reused test file records nothing.
    assert_eq!(r.unit_results(), 7 + 3 - 1, "{}", changed.stderr);
    // \[ADR 0074\] This machine's stats say it, beside what skips saved.
    let stats = r.run(&["repository", "stats", "--local", "--format", "json"], &[]);
    assert_eq!(stats.code, 0, "{}", stats.stderr);
    let stats: Value = serde_json::from_str(&stats.stdout).unwrap();
    assert_eq!(
        stats["testFiles"],
        json!({"runs": 2, "ran": 8 + 3, "reused": 5, "savedMs": saved}),
        "{stats:#}"
    );
    let text = r.run(&["repository", "stats", "--local"], &[]).stdout;
    assert!(
        text.contains("test files       11 ran, 5 reused, in 2 runs\nsaved by reuse   "),
        "{text}"
    );
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
        "hushrun: unit: 5 of 8 test files run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
}

// covers: C-34, C-37
#[test]
fn a_file_that_becomes_a_test_file_is_never_said_to_be_gone() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // pytest.ini is ignored, so a change to the rule that names test files moves a file between
    // the sets and changes nothing else.
    let mut files = shop("{mode: reuse, ignore: [\"pytest.ini\"]}");
    files.push((
        "tests/check_extra.py".into(),
        "def test_extra():\n    assert True\n".into(),
    ));
    let r = repo_of(&files);
    let python = python();
    let pytest = || r.run(&pytest_args(&python, &[]), &[]);
    let first = pytest();
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // tests/check_extra.py was a source; pytest now takes it for a test file. It is on disk.
    r.write(
        "pytest.ini",
        "[pytest]\ntestpaths = tests\npythonpath = .\npython_files = test_*.py check_*.py\n",
    );
    let moved = pytest();
    assert_eq!(moved.code, 0, "{}\n{}", moved.stdout, moved.stderr);
    assert_eq!(
        moved.plan_line(),
        "hushrun: unit: all 9 test files run: tests/check_extra.py is now a test file, which can change what tests load"
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
        "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
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
        "hushrun: unit: all 2 test files run; 1 that already passed on the files it read runs anyway, as a check"
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

// covers: C-32
#[test]
fn with_pytest_xdist_a_plan_that_deselects_every_test_left_still_passes() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let mut files = hidden("observe");
    files.push((
        "test_more.py".into(),
        "def test_one():\n    pass\n\ndef test_two():\n    pass\n".into(),
    ));
    let r = repo_of(&files);
    r.run(&pytest_args(&python(), &[]), &[]);
    let units = r.root.join("units");
    let every = ["test_hidden.py", "test_other.py", "test_more.py"];
    // Every test file listed, no other deselection: the workers remove every test, and the
    // process that claimed the directory reads their files and makes "no tests collected" a 0.
    let (code, facts) = plugin_pytest(&r, &units, &every, &["-n", "2"]);
    assert_eq!(code, 0, "{facts:?}");
    let controller: Vec<&Value> = facts.iter().filter(|f| f["controller"] == true).collect();
    assert_eq!(controller.len(), 1, "{facts:?}");
    let removed: u64 = facts
        .iter()
        .filter(|f| f["controller"] != true)
        .map(|f| f["removedItems"].as_u64().unwrap_or(0))
        .sum();
    // Each worker collects every test and removes them all: 4 tests, 2 workers.
    assert_eq!(removed, 8, "{facts:?}");
    for unit in every {
        assert!(
            facts
                .iter()
                .filter(|f| f["controller"] != true)
                .all(|f| f["units"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|u| u["unit"] == unit)
                    .all(|u| u["deselected"] == true)),
            "{unit}: {facts:?}"
        );
    }
    // What isn't on the list still runs, and its failure is pytest's status.
    r.write("flag", "");
    let (code, _) = plugin_pytest(&r, &units, &["test_other.py", "test_more.py"], &["-n", "2"]);
    assert_eq!(code, 1);
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
        // \[ADR 0072\] The store, as a cache step saves it once hushrun has exited.
        if name.ends_with(".v2.sqlite") {
            std::fs::copy(entry.path(), ci.cache.join("units").join(name)).unwrap();
        }
    }
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let reused = ci.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert_eq!(
        reused.plan_line(),
        "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(
        results
            .recorded()
            .iter()
            .any(|r| r["units"]["savedMs"].is_u64()
                && r["units"]
                    == json!({"total": 8, "ran": 3, "reused": 5, "savedMs": r["units"]["savedMs"]})),
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

// covers: C-34
#[test]
fn a_suspension_that_cant_reach_the_server_waits_and_nothing_is_reused_meanwhile() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let server = with_server(&results);
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let outbox =
        || std::fs::read_dir(r.cache.join("units/outbox")).map_or(0, |d| d.flatten().count());
    let reuse_requests = || {
        results
            .requests()
            .iter()
            .filter(|s| s.path == "v0/units/reuse")
            .count()
    };
    assert_eq!(r.run(&pytest_args(&python, &[]), &envs(&server)).code, 0);
    // The server fails every request that would suspend, each time it is tried (L-4 retries).
    results.answer("POST", "v0/units/reuse", 503, json!({}), 4);
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    let caught = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert!(
        caught.stderr.contains("couldn't be suspended")
            && caught
                .stderr
                .contains("it is sent before the next run reuses anything"),
        "{}",
        caught.stderr
    );
    // It waits on this machine, and the server's state is as it was.
    assert_eq!(outbox(), 1);
    assert_eq!(results.reuse_state(REPOSITORY, "unit")["state"], "on");
    // While it waits, nothing is reused, and the run says why.
    let tries = reuse_requests();
    results.answer("POST", "v0/units/reuse", 503, json!({}), tries);
    r.write("libb.py", "Y = 1  # again\n");
    let waiting = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(waiting.code, 1, "{}", waiting.stderr);
    assert!(
        waiting.plan_line().starts_with(
            "hushrun: unit: every test file runs: can't read which passed (a suspension of unit's reuse hasn't reached"
        ),
        "{}",
        waiting.stderr
    );
    assert_eq!(outbox(), 1);
    assert!(
        results
            .recorded()
            .iter()
            .all(|r| r["units"]["reused"].as_u64().unwrap_or(0) == 0),
        "{:?}",
        results.recorded()
    );
    // The server answers again: the next plan sends the suspension before it reads anything.
    assert_eq!(reuse_requests(), 2 * tries);
    let before_plan = results.requests().len();
    r.write("libb.py", "Y = 1  # once more\n");
    let sent = r.run(&pytest_args(&python, &[]), &envs(&server));
    let paths: Vec<String> = results.requests()[before_plan..]
        .iter()
        .map(|s| s.path.clone())
        .collect();
    let reuse_at = paths.iter().position(|p| p == "v0/units/reuse");
    let lookup_at = paths.iter().position(|p| p == "v0/units/lookup");
    assert!(
        reuse_at.is_some() && reuse_at < lookup_at,
        "{paths:?}\n{}",
        sent.stderr
    );
    assert_eq!(outbox(), 0);
    assert_eq!(
        results.reuse_state(REPOSITORY, "unit")["state"],
        "suspended"
    );
    assert!(
        sent.plan_line()
            .contains("reuse of its test files is suspended"),
        "{}",
        sent.stderr
    );
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
    // The run itself is recorded, as a server from before takes it, and said once.
    assert_eq!(results.recorded().len(), 1);
    assert!(
        old.stderr.contains("test files' results weren't kept"),
        "{}",
        old.stderr
    );
    // The next plan asks it, and says what it answered.
    r.write("liba.py", "X = 1  # changed\n");
    let old = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(old.code, 0, "{}", old.stderr);
    assert_eq!(
        old.plan_line(),
        "hushrun: unit: every test file runs: can't read which passed (its server doesn't keep test files' results yet)"
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
        "hushrun: test: all 5 test binaries run: none had passed here yet on the files it read"
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
        "hushrun: test: 2 of 5 test binaries run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    let units = r.newest_result()["units"].clone();
    assert!(units["savedMs"].is_u64(), "{units}");
    assert_eq!(
        units,
        json!({"total": 5, "ran": 2, "reused": 3, "savedMs": units["savedMs"]})
    );
    // \[ADR 0073\] Why, for a test binary: what it is built from, without a path of edges.
    r.write(
        "crates/core/src/lib.rs",
        "pub fn answer() -> u32 { 40 + 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
    );
    let why = r.run(&["why", "test", "crates/app/tests/runs_binary.rs"], &[]);
    assert_eq!(why.code, 0, "{}", why.stderr);
    assert!(
        why.stdout.starts_with(
            "crates/app/tests/runs_binary.rs runs: it read crates/core/src/lib.rs, which changed\n  its test binary is built from it: rustc's dep-info for the build names it\n  as far as this machine knows: its test binaries"
        ),
        "{}",
        why.stdout
    );
    r.write(
        "crates/core/src/lib.rs",
        "pub fn answer() -> u32 { 42 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
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
        "hushrun: test: 2 of 5 test binaries run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
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
            .contains("every test binary runs: reuse of its test files is suspended"),
        "{}",
        suspended.stderr
    );
}

/// Every file under `dir` with its length and time, to tell that nothing was written.
fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else {
                out.push((path, meta.len(), meta.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

// covers: C-22, C-34
#[test]
fn ready_says_how_many_test_files_a_run_would_run_and_writes_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let python = python();
    let script = format!("hushrun run unit -- {python} -m pytest -q -p no:cacheprovider");
    let mut files = shop("reuse");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"{script}\"}}}}\n"
        ),
    ));
    let r = repo_of(&files);
    let ran = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    git::commit_all(&r.repo, "cart");
    let before = [
        snapshot(&r.cache.join("units")),
        snapshot(&r.cache.join("results")),
    ]
    .concat();
    let ready = r.run(&["ready"], &[]);
    assert_eq!(ready.code, 10, "{}\n{}", ready.stdout, ready.stderr);
    // Results on this machine: what a run here would do, in local mode's words.
    assert!(
        ready.stdout.contains(
            "unit: a run here would run 3 of 8 test files; 5 already passed here on the files they read"
        ),
        "{}",
        ready.stdout
    );
    assert_eq!(
        [
            snapshot(&r.cache.join("units")),
            snapshot(&r.cache.join("results"))
        ]
        .concat(),
        before,
        "ready wrote to the cache's units or results"
    );
    let json = r.run(&["ready", "--format", "json"], &[]);
    let doc: Value = serde_json::from_str(&json.stdout).unwrap();
    assert!(
        doc["notes"].as_array().unwrap().iter().any(|n| n
            .as_str()
            .unwrap()
            .starts_with("unit: a run here would run 3 of 8")),
        "{doc:#}"
    );
    // Its files said otherwise: not the files a plan reads, so nothing is said.
    r.write(
        "app/greet.py",
        "def hello():\n    return 'hi'  # not committed\n",
    );
    let ready = r.run(&["ready"], &[]);
    assert!(!ready.stdout.contains("test files"), "{}", ready.stdout);
}

// covers: C-23, C-22, C-34
#[test]
fn the_pre_push_hook_says_how_many_test_files_a_run_would_run() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let python = python();
    let script = format!("hushrun run unit -- {python} -m pytest -q -p no:cacheprovider");
    let mut files = shop("reuse");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"{script}\"}}}}\n"
        ),
    ));
    let r = repo_of(&files);
    assert_eq!(r.run(&pytest_args(&python, &[]), &[]).code, 0);
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    git::commit_all(&r.repo, "cart");
    let head = String::from_utf8(git::run(&r.repo, &["rev-parse", "HEAD"], None)).unwrap();
    let line = format!(
        "refs/heads/main {} refs/heads/main {}\n",
        head.trim(),
        "0".repeat(40)
    );
    // A coding agent's push waits for the check, and the hook says what its run would run.
    let output = r
        .hushrun()
        .env("CLAUDECODE", "1")
        .args([
            "hook",
            "pre-push",
            "origin",
            "git@example.com:acme/shop.git",
        ])
        .write_stdin(line)
        .output()
        .expect("hushrun");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains("push refused"), "{stderr}");
    assert!(
        stderr.contains(
            "unit: a run here would run 3 of 8 test files; 5 already passed here on the files they read"
        ),
        "{stderr}"
    );
}

// covers: C-22, C-34, C-30
#[test]
fn a_check_where_no_pytest_ever_reported_says_nothing_of_test_files() {
    // A check whose COMMAND hushrun doesn't recognize gets the pytest adapter, which does nothing
    // where pytest never starts: its plan and ready count no file that only looks like a test.
    let script = "hushrun run unit -- git --version";
    let mut files = shop("reuse");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"{script}\"}}}}\n"
        ),
    ));
    let r = repo_of(&files);
    let ran = r.run(&["run", "unit", "--", "git", "--version"], &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(!ran.stderr.contains("test file"), "{}", ran.stderr);
    r.write("app/greet.py", "def hello():\n    return 'hi'  # changed\n");
    git::commit_all(&r.repo, "greet");
    let ran = r.run(&["run", "unit", "--", "git", "--version"], &[]);
    assert!(!ran.stderr.contains("test file"), "{}", ran.stderr);
    r.write("app/greet.py", "def hello():\n    return 'hi'  # again\n");
    git::commit_all(&r.repo, "greet again");
    let ready = r.run(&["ready"], &[]);
    assert!(
        ready.stdout.contains("unit") && !ready.stdout.contains("test file"),
        "{}",
        ready.stdout
    );
}

// covers: C-22, C-34
#[test]
fn ready_asks_the_server_as_ci_counts_and_says_a_suspension() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let server = with_server(&results);
    let python = python();
    let script = format!("hushrun run unit -- {python} -m pytest -q -p no:cacheprovider");
    let mut files = hidden("reuse");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"{script}\"}}}}\n"
        ),
    ));
    for n in 1..=3 {
        files.push((
            format!("test_more_{n}.py"),
            "import libb\n\ndef test_more():\n    assert libb.Y == 1\n".into(),
        ));
    }
    let r = repo_of(&files);
    assert_eq!(r.run(&pytest_args(&python, &[]), &envs(&server)).code, 0);
    r.write("liba.py", "X = 1  # changed\n");
    git::commit_all(&r.repo, "liba");
    let before = results.requests().len();
    let ready = r.run(&["ready"], &envs(&server));
    assert!(
        ready.stdout.contains(
            "unit: CI would run 2 of 5 test files; 3 already passed on the files they read"
        ),
        "{}\n{}",
        ready.stdout,
        ready.stderr
    );
    // \[C-38\] What other machines saw its test files read, then one batch lookup, and nothing
    // written to the server.
    let asked: Vec<String> = results.requests()[before..]
        .iter()
        .map(|s| s.path.clone())
        .filter(|p| p.starts_with("v0/units/"))
        .collect();
    assert_eq!(asked, ["v0/units/hints/lookup", "v0/units/lookup"]);
    // Suspended by an audit's disagreement: ready says so, as a run does. test_hidden.py passes
    // on what it reads now, then the flag it can't see makes it fail when it is the one audited.
    assert_eq!(r.run(&pytest_args(&python, &[]), &envs(&server)).code, 0);
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    git::commit_all(&r.repo, "libb");
    let caught = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        results.reuse_state(REPOSITORY, "unit")["state"],
        "suspended"
    );
    r.write("liba.py", "X = 1  # again\n");
    git::commit_all(&r.repo, "liba again");
    let ready = r.run(&["ready"], &envs(&server));
    assert!(
        ready
            .stdout
            .contains("unit: every test file runs: reuse of its test files is suspended"),
        "{}",
        ready.stdout
    );
}

/// What a step of [`rust_steps`] came to: test reuse's outcome, which build reuse must leave as
/// it is, and what build reuse did.
#[derive(Debug)]
#[cfg(unix)]
struct Step {
    /// Test reuse's own outcome: the exit code, the plan's line, the recorded units without the
    /// time they saved (a time, never the same twice), and whether an audit suspended reuse.
    tests: (i32, String, Value, bool),
    /// The lines that total what the compiles came to (B-20).
    build_totals: Vec<String>,
    /// Each start of `cargo` hushrun made, in order: whether it was COMMAND (inside the run), its
    /// RUSTC_WRAPPER, its HUSHRUN_BUILD_SESSION, whether that session's folder was there (it goes
    /// when the session finishes), and its arguments.
    starts: Vec<(bool, String, String, bool, String)>,
}

/// The steps of the Rust fixture that make test reuse plan, reuse, audit, and start COMMAND a
/// second time for a test file that failed though it had passed on the same files it read
/// (C-34): each step's outcome. `env` goes to every run; `cargo` on PATH is a shim that writes
/// down each start before it hands over to the real one.
#[cfg(unix)]
fn rust_steps(r: &Repo, env: &[(&str, &str)]) -> Vec<Step> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let real = std::env::split_paths(&path)
        .map(|dir| dir.join("cargo"))
        .find(|cargo| cargo.is_file())
        .expect("cargo on PATH");
    let shims = r.root.join("shims");
    std::fs::create_dir_all(&shims).unwrap();
    let log = r.root.join("starts.log");
    let shim = shims.join("cargo");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nthere=no\n[ -n \"$HUSHRUN_BUILD_SESSION\" ] && [ -d \"$HUSHRUN_BUILD_SESSION\" ] && there=yes\nprintf '%s|%s|%s|%s|%s\\n' \"$HUSHRUN_INSIDE_RUN\" \"$RUSTC_WRAPPER\" \"$HUSHRUN_BUILD_SESSION\" \"$there\" \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut dirs = vec![shims];
    dirs.extend(std::env::split_paths(&path));
    let shimmed = std::env::join_paths(dirs).unwrap();
    let shimmed = shimmed.to_str().expect("a PATH in UTF-8");
    let mut env: Vec<(&str, &str)> = env.to_vec();
    env.push(("PATH", shimmed));

    // `--no-fail-fast`: when the audited test file fails, nextest would otherwise cancel the tests
    // it hasn't started, and how many those are depends on how many threads it has and how fast
    // the others finish. A test binary cancelled before it started didn't run whole, so the run's
    // `ran` came to 4 or 5 of 5 by timing alone (on a 3-core macOS runner, not on 4-core Linux).
    let nextest = [
        "run",
        "test",
        "--",
        "cargo",
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--no-fail-fast",
    ];
    let mut steps = Vec::new();
    let mut step = |r: &Repo| {
        let _ = std::fs::remove_file(&log);
        let ran = r.run(&nextest, &env);
        let starts = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let mut parts = line.splitn(5, '|');
                let mut next = || parts.next().unwrap_or_default().to_string();
                (next() == "1", next(), next(), next() == "yes", next())
            })
            .collect();
        let mut units = r.newest_result()["units"].clone();
        if let Some(units) = units.as_object_mut() {
            units.remove("savedMs");
        }
        steps.push(Step {
            tests: (
                ran.code,
                ran.plan_line().to_string(),
                units,
                ran.stderr
                    .contains("reuse of test's test files is suspended"),
            ),
            build_totals: ran
                .stderr
                .lines()
                .filter(|line| {
                    line.strip_prefix("hushrun: build: ").is_some_and(|rest| {
                        rest.starts_with(|c: char| c.is_ascii_digit())
                            && (rest.contains(" built") || rest.contains(" reused"))
                    })
                })
                .map(str::to_string)
                .collect(),
            starts,
        });
    };
    // Everything runs, and passes.
    step(r);
    // One crate changes: its test binary runs, three are reused and one runs as a check.
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 1 + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    step(r);
    // The flag, which no dep-info names: the only test binary that could be reused is the one
    // audited, it fails, and COMMAND starts again for it alone; it fails again, so reuse is
    // suspended.
    r.write("flag", "");
    r.write(
        "crates/core/src/lib.rs",
        "pub fn answer() -> u32 { 40 + 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
    );
    r.write(
        "crates/other/src/lib.rs",
        "pub fn two() -> u32 { 2 * 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
    );
    step(r);
    steps
}

/// The Rust fixture, with its lockfile committed.
#[cfg(unix)]
fn rust_repo() -> Repo {
    let r = Repo::new(&rust_files());
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    git::commit_all(&r.repo, "lock");
    r
}

// covers: B-2, B-20, C-34
// Build blocks and test reuse in the same run: with build reuse on, every start of COMMAND
// compiles through the wrapper in the run's one session, the second start for an audited test
// file too, and so does the plan's build, which is COMMAND's made first; the compiles are totalled
// once a run; and test reuse decides, records and suspends exactly as it does with build reuse
// off. A second checkout that shares the store reuses crates the first one compiled.
#[cfg(unix)]
#[test]
fn build_reuse_and_test_reuse_work_in_the_same_run() {
    if !tools("cargo-nextest", has_nextest()) {
        return;
    }
    let off = rust_repo();
    let without = rust_steps(&off, &[("HUSHRUN_BUILD", "off")]);
    let on = rust_repo();
    let store = on.root.join("store");
    let store = store.to_str().unwrap();
    let with = rust_steps(
        &on,
        &[("HUSHRUN_BUILD", "on"), ("HUSHRUN_BUILD_DIR", store)],
    );

    // Test reuse's own outcome, step by step, is the same with build reuse on or off.
    let outcomes = |steps: &[Step]| steps.iter().map(|s| s.tests.clone()).collect::<Vec<_>>();
    assert_eq!(outcomes(&with), outcomes(&without));
    // And it is what test reuse does on this fixture: plan, reuse, audit, suspend.
    let tests = outcomes(&without);
    assert_eq!(tests[0].0, 0, "{tests:?}");
    assert_eq!(
        tests[1].1,
        "hushrun: test: 2 of 5 test binaries run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert_eq!(tests[1].2, json!({"total": 5, "ran": 2, "reused": 3}));
    assert_ne!(tests[2].0, 0, "{tests:?}");
    // Every test binary ran whole, the audited one too, which failed.
    assert_eq!(tests[2].2, json!({"total": 5, "ran": 5, "reused": 0}));
    assert!(tests[2].3, "{tests:?}");

    for (n, (step, plain)) in with.iter().zip(&without).enumerate() {
        let commands: Vec<_> = step.starts.iter().filter(|s| s.0).collect();
        // Without build reuse, nothing is set and nothing is totalled.
        assert!(plain.build_totals.is_empty(), "{n}: {plain:?}");
        assert!(
            plain
                .starts
                .iter()
                .all(|s| s.1.is_empty() && s.2.is_empty()),
            "{n}: {plain:?}"
        );
        assert_eq!(
            plain.starts.iter().filter(|s| s.0).count(),
            commands.len(),
            "{n}: COMMAND starts as often either way: {plain:?}\n{step:?}"
        );
        // With it: the wrapper and one session on every start of COMMAND.
        assert!(!commands.is_empty(), "{n}: {step:?}");
        let session = &commands[0].2;
        assert!(!session.is_empty(), "{n}: {step:?}");
        for start in &commands {
            assert!(start.1.ends_with("/hushrun-rustc"), "{n}: {step:?}");
            assert_eq!(&start.2, session, "{n}: one session a run: {step:?}");
            assert!(start.3, "{n}: the session is still going: {step:?}");
        }
        // The plan's build, COMMAND's made first, compiles through the wrapper in that session.
        let plan_builds: Vec<_> = step
            .starts
            .iter()
            .take_while(|s| !s.0)
            .filter(|s| s.4.starts_with("test --no-run"))
            .collect();
        assert!(!plan_builds.is_empty(), "{n}: {step:?}");
        assert!(
            plan_builds
                .iter()
                .all(|s| s.1.ends_with("/hushrun-rustc") && &s.2 == session && s.3),
            "{n}: {step:?}"
        );
        // What the compiles came to, once.
        assert_eq!(step.build_totals.len(), 1, "{n}: {step:?}");
    }
    // The first run compiled every crate through the wrapper; the audited test file's second
    // start of COMMAND had it too.
    assert!(
        with[0].build_totals[0].contains(" built, none reused"),
        "{:?}",
        with[0]
    );
    assert_eq!(
        with[2].starts.iter().filter(|s| s.0).count(),
        2,
        "{:?}",
        with[2]
    );

    // Another checkout of the same files, sharing the store: crates are reused, and test reuse
    // ends as it did in the first checkout.
    let other = rust_repo();
    let again = rust_steps(
        &other,
        &[("HUSHRUN_BUILD", "on"), ("HUSHRUN_BUILD_DIR", store)],
    );
    assert_eq!(outcomes(&again), tests);
    assert!(
        again[0].build_totals[0].contains(" reused")
            && !again[0].build_totals[0].contains("none reused"),
        "{:?}",
        again[0]
    );
}
