//! \[ADR 0073\] `hushrun why NAME UNIT` (C-37): what a run of the check would do with one test file,
//! for the files on disk now, and why, from what this machine knows; and it writes nothing.
//!
//! The pytest fixtures need Python with pytest and pytest-xdist; they say so and pass where those
//! are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

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
    "PYTEST_ADDOPTS",
    "PYTHONPATH",
    "CARGO_TARGET_DIR",
    "npm_lifecycle_event",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CURSOR_AGENT",
    "GEMINI_CLI",
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
    cache: PathBuf,
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Repo {
    fn new(files: &[(&str, String)]) -> Repo {
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

    fn run(&self, args: &[&str]) -> Ran {
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
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        let output = cmd.args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn pytest(&self) -> Ran {
        let python = python();
        self.run(&[
            "run",
            "unit",
            "--",
            &python,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
        ])
    }

    /// `hushrun why unit <unit>`, which explains and exits 0.
    fn why(&self, unit: &str) -> String {
        let ran = self.run(&["why", "unit", unit]);
        assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
        ran.stdout
    }

    fn why_json(&self, unit: &str) -> Value {
        let ran = self.run(&["why", "unit", unit, "--format", "json"]);
        assert_eq!(ran.code, 0, "{}", ran.stderr);
        serde_json::from_str(&ran.stdout).expect("JSON")
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

/// A shop: modules read through imports, a test file that starts a process, one that
/// `tests.always` names, a lockfile in the rest set, and a test file that can be made to fail.
fn shop(tests: &str) -> Vec<(&'static str, String)> {
    let yaml = format!(
        "version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests:\n      mode: {tests}\n      always: [\"tests/test_lint.py\"]\n"
    );
    let mut files = vec![
        ("hushrun.yaml", yaml),
        (".gitignore", "__pycache__/\n.pytest_cache/\nbreak\n".into()),
        (
            "pytest.ini",
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("requirements.txt", "nothing==1.0\n".into()),
        ("app/__init__.py", String::new()),
        ("app/pricing.py", "def price(x):\n    return x * 2\n".into()),
        (
            "app/cart.py",
            "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)\n"
                .into(),
        ),
        ("app/greet.py", "def hello():\n    return 'hi'\n".into()),
        (
            "tests/test_cart.py",
            "from app.cart import total\n\ndef test_total():\n    assert total([1, 2]) == 6\n".into(),
        ),
        (
            "tests/test_child.py",
            "import subprocess, sys\n\ndef test_child():\n    assert subprocess.run([sys.executable, '-c', 'pass']).returncode == 0\n".into(),
        ),
        (
            "tests/test_lint.py",
            "def test_lint():\n    assert True\n".into(),
        ),
        (
            "tests/test_breaks.py",
            "import os\nfrom app.greet import hello\n\ndef test_breaks():\n    assert hello() == 'hi'\n    assert not os.path.exists('break')\n".into(),
        ),
    ];
    for n in 1..=3 {
        files.push((
            [
                "tests/test_greet_1.py",
                "tests/test_greet_2.py",
                "tests/test_greet_3.py",
            ][n - 1],
            "from app.greet import hello\n\ndef test_hello():\n    assert hello() == 'hi'\n".into(),
        ));
    }
    files
}

/// Every file below `dir`, with its size and time.
fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                out.push((entry.path(), meta.len(), meta.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

// covers: C-37, C-34
#[test]
fn why_says_what_a_run_would_do_with_each_test_file_and_why() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = Repo::new(&shop("reuse"));
    // Before any run here: nothing is known of what the check runs.
    let never = r.why("tests/test_cart.py");
    assert!(
        never.starts_with("this machine has never seen unit run with its test files observed"),
        "{never}"
    );
    assert_eq!(r.pytest().code, 0);

    // A file it read changed: named, with the path of imports between them.
    r.write("app/pricing.py", "def price(x):\n    return x + x\n");
    assert_eq!(
        r.why("tests/test_cart.py"),
        "tests/test_cart.py runs: it read app/pricing.py, which changed\n  tests/test_cart.py imports app/cart.py imports app/pricing.py\n"
    );
    let json = r.why_json("tests/test_cart.py");
    assert_eq!(json["schema"], "hushrun.why/v1");
    assert_eq!(json["decision"], "runs");
    assert_eq!(json["reason"], "read-changed");
    assert_eq!(json["file"], "app/pricing.py");
    assert_eq!(json["asFarAsThisMachineKnows"], false);
    assert_eq!(
        json["path"][1],
        serde_json::json!({"file": "app/cart.py", "then": "import"})
    );

    // What didn't change, and passed: reused, and an audit may still run it.
    let reused = r.why("tests/test_greet_1.py");
    assert!(
        reused.starts_with(
            "tests/test_greet_1.py would be reused: it passed on the files it read, and none of them has changed since\n  an audit run may still run it"
        ),
        "{reused}"
    );
    // Not observed, tests.always, not a test file.
    let child = r.why("tests/test_child.py");
    assert!(
        child.starts_with("tests/test_child.py runs: hushrun couldn't follow all it read the last time it ran here: a process it started"),
        "{child}"
    );
    assert_eq!(
        r.why("tests/test_lint.py"),
        "tests/test_lint.py runs: `tests.always` in hushrun.yaml names it\n"
    );
    assert!(
        r.why("app/cart.py")
            .starts_with("app/cart.py isn't a test file of unit: pytest's python_files"),
    );
    assert!(
        r.why("tests/test_gone.py")
            .starts_with("tests/test_gone.py isn't a file of unit's scope as it is now"),
    );
    // A new test file: never seen run here.
    r.write("tests/test_new.py", "def test_new():\n    assert True\n");
    assert!(
        r.why("tests/test_new.py")
            .starts_with("tests/test_new.py runs: this machine has never seen it run"),
    );

    // A new test file runs alone; a new source runs every test file (shape), and so does a rest
    // file that changed.
    assert!(r.why("tests/test_greet_1.py").contains("would be reused"));
    std::fs::remove_file(r.repo.join("tests/test_new.py")).unwrap();
    r.write("app/extra.py", "X = 1\n");
    let all = r.why("tests/test_greet_1.py");
    assert_eq!(
        all,
        "tests/test_greet_1.py runs: app/extra.py is new, which can change what tests load\n  so every test file of the check runs\n"
    );
    std::fs::remove_file(r.repo.join("app/extra.py")).unwrap();
    r.write("requirements.txt", "nothing==2.0\n");
    assert_eq!(
        r.why("tests/test_greet_1.py"),
        "tests/test_greet_1.py runs: requirements.txt changed, and any test may read it\n  so every test file of the check runs\n"
    );
    r.write("requirements.txt", "nothing==1.0\n");
    r.write("app/pricing.py", "def price(x):\n    return x * 2\n");

    // Its newest result on these files failed.
    r.write("break", "");
    r.write("app/greet.py", "def hello():\n    return 'hi'  # changed\n");
    assert_eq!(r.pytest().code, 1);
    assert_eq!(
        r.why("tests/test_breaks.py"),
        "tests/test_breaks.py runs: its newest result on the files it reads now failed\n"
    );
}

// covers: C-37, ADR 0061
#[test]
fn why_writes_nothing_and_tells_off_and_not_offered() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = Repo::new(&shop("observe"));
    assert_eq!(r.pytest().code, 0);
    r.write("app/pricing.py", "def price(x):\n    return x + x\n");
    // What hushrun keeps of test files and results: the digest's own cache of object ids isn't.
    let kept = || {
        [
            snapshot(&r.cache.join("units")),
            snapshot(&r.cache.join("results")),
        ]
        .concat()
    };
    let before = kept();
    // In `observe`: what `tests: reuse` would do.
    let observed = r.why("tests/test_cart.py");
    assert!(
        observed.starts_with(
            "tests/test_cart.py would run with `tests: reuse`: it read app/pricing.py, which changed"
        ),
        "{observed}"
    );
    r.why_json("tests/test_greet_2.py");
    assert_eq!(
        kept(),
        before,
        "why wrote what hushrun keeps of test files or results"
    );
    // A COMMAND whose adapter can't load; and `tests` off.
    let tox = r.run(&["why", "unit", "tests/test_cart.py", "--", "tox", "-e", "py"]);
    assert_eq!(tox.code, 0, "{}", tox.stderr);
    assert!(
        tox.stdout
            .starts_with("unit's test files aren't observed: "),
        "{}",
        tox.stdout
    );
    r.write(
        "hushrun.yaml",
        "version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: off\n",
    );
    assert!(
        r.why("tests/test_cart.py")
            .starts_with("unit's `tests` is off in hushrun.yaml"),
    );
}

// covers: C-37, C-34, C-36
#[test]
fn why_says_a_suspension_and_a_store_it_cant_read() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let yaml =
        "version: 1\nrepository: example.com/acme/hidden\nchecks:\n  unit:\n    tests: reuse\n";
    let r = Repo::new(&[
        ("hushrun.yaml", yaml.into()),
        (".gitignore", "__pycache__/\n.pytest_cache/\nflag\n".into()),
        ("liba.py", "X = 1\n".into()),
        ("libb.py", "Y = 1\n".into()),
        (
            "test_hidden.py",
            "import os, liba\n\ndef test_x():\n    assert not os.path.exists(os.path.join(os.path.dirname(__file__), 'flag'))\n".into(),
        ),
        (
            "test_other.py",
            "import libb\n\ndef test_y():\n    assert libb.Y == 1\n".into(),
        ),
    ]);
    assert_eq!(r.pytest().code, 0);
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    let caught = r.pytest();
    assert!(caught.stderr.contains("is suspended"), "{}", caught.stderr);
    let suspended = r.why("test_hidden.py");
    assert!(
        suspended.starts_with(
            "test_hidden.py runs: reuse of its test files is suspended by a person on "
        ),
        "{suspended}"
    );
    assert!(
        suspended.contains("hushrun tests resume unit"),
        "{suspended}"
    );

    // A store that can't be read: said, and nothing is known.
    let store = std::fs::read_dir(r.cache.join("units"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with(".v1.sqlite"))
        .expect("a store");
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", store.display()));
    }
    std::fs::write(&store, b"not a database").unwrap();
    let broken = r.why("test_other.py");
    assert!(
        broken.contains("this machine's record of what test files read can't be read here"),
        "{broken}"
    );
    // The next run runs every test file, says so once, and starts the store again.
    std::fs::remove_file(r.repo.join("flag")).unwrap();
    r.write("libb.py", "Y = 1  # again\n");
    let ran = r.pytest();
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let said: Vec<&str> = ran
        .stderr
        .lines()
        .filter(|l| l.contains("can't be read"))
        .collect();
    assert_eq!(said.len(), 1, "{}", ran.stderr);
    assert!(
        said[0].starts_with("hushrun: unit: every test file runs: this machine's record of what its test files read can't be read ("),
        "{}",
        said[0]
    );
    assert!(
        r.why("test_other.py")
            .starts_with("test_other.py runs: reuse of its test files is suspended"),
    );
}
