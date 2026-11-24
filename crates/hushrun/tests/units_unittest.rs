//! \[ADR 0110\] The standard library's unittest, and Django's `manage.py test` (C-30 to C-34, C-39,
//! C-66 to C-69): a unit is a test module, a test one `TestCase` method, what each read is what
//! hushrun's observer saw through the `sitecustomize.py` it puts first on `PYTHONPATH`, and the
//! tests of what already passed are removed from the suite before it runs.
//!
//! The fixtures need Python 3.10 or later: `HUSHRUN_TEST_PYTHON` names it, `python3` by default;
//! the one that watches each test, 3.12 or later, and it skips below, as `reads_test`'s do. The
//! Django one needs a Python with Django: `HUSHRUN_TEST_DJANGO` names it. They say so and pass where
//! those are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets. No fixture needs the
//! network.

#![cfg(unix)]

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
    "HUSHRUN_UNITTEST_SESSION",
    "PYTEST_ADDOPTS",
    "PYTHONPATH",
    "DJANGO_SETTINGS_MODULE",
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
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
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

impl Ran {
    /// The line that says what the plan reuses, or why every test module runs.
    fn plan_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| {
                l.starts_with("hushrun: ")
                    && (l.contains("test modules run")
                        || l.contains("tests run")
                        || l.contains("test module runs"))
            })
            .unwrap_or_else(|| panic!("no plan line in:\n{}", self.stderr))
    }
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

    fn hushrun(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        for var in CLEARED {
            cmd.env_remove(var);
        }
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

    fn run(&self, args: &[&str]) -> Ran {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
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
    fn reads(&self, unit: &str) -> Vec<String> {
        store::map(&self.cache)["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} has no reads"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    /// The adapter's directory hushrun wrote in this cache.
    fn adapter(&self) -> PathBuf {
        std::fs::read_dir(self.cache.join("adapters"))
            .expect("adapters")
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("unittest-")
            })
            .expect("the unittest adapter")
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

fn runs(python: &str, check: &str) -> bool {
    Command::new(python)
        .args(["-c", check])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A Python 3.10 or later, said when missing. An older Python isn't a missing tool: the adapter
/// starts in none (it needs `sys.orig_argv`), and these skip.
fn with_python() -> Option<String> {
    let python = python();
    if !tools("Python (HUSHRUN_TEST_PYTHON)", runs(&python, "pass")) {
        return None;
    }
    if !runs(
        &python,
        "import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)",
    ) {
        eprintln!(
            "note: skipping: this Python is older than 3.10, where the adapter starts in none"
        );
        return None;
    }
    Some(python)
}

/// Whether `python` is 3.12 or later: it has `sys.monitoring`, and unittest's "no tests ran" is 5.
fn is_312(python: &str) -> bool {
    runs(
        python,
        "import sys; sys.exit(0 if sys.version_info >= (3, 12) else 1)",
    )
}

/// A Python with Django, said when missing.
fn with_django() -> Option<String> {
    let python = std::env::var("HUSHRUN_TEST_DJANGO").unwrap_or_else(|_| "python3".into());
    let here = runs(
        &python,
        "import sys, django; sys.exit(0 if sys.version_info >= (3, 10) else 1)",
    );
    tools(
        "a Python 3.10 or later with Django (HUSHRUN_TEST_DJANGO)",
        here,
    )
    .then_some(python)
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A shop of unittest test modules: two modules each read by one test module, a greeting read by
/// five, one that starts a process, a module two imports away that only one test calls, and a
/// lockfile in the rest set.
fn shop(tests: &str) -> Vec<(&'static str, String)> {
    let mut files = vec![
        ("hushrun.yaml", yaml(tests)),
        (".gitignore", "__pycache__/\n".to_string()),
        ("requirements.txt", "nothing==1.0\n".to_string()),
        ("app/__init__.py", String::new()),
        ("app/pricing.py", "def price(x):\n    return x * 2\n".into()),
        (
            "app/cart.py",
            "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)\n"
                .into(),
        ),
        ("app/greet.py", "def hello():\n    return 'hi'\n".into()),
        ("app/deep.py", "def h():\n    return 1\n".into()),
        (
            "app/near.py",
            "from app import deep\n\ndef f():\n    return deep.h()\n".into(),
        ),
        ("app/other.py", "def g():\n    return 2\n".into()),
        (
            "app/facade.py",
            "from app import near, other\n\ndef fx():\n    return near.f()\n\ndef fy():\n    return other.g()\n".into(),
        ),
        ("tests/__init__.py", String::new()),
        (
            "tests/test_cart.py",
            "import unittest\nfrom app.cart import total\n\nclass CartTest(unittest.TestCase):\n    def test_total(self):\n        self.assertEqual(total([1, 2]), 6)\n".into(),
        ),
        (
            "tests/test_pricing.py",
            "import unittest\nfrom app.pricing import price\n\nclass PriceTest(unittest.TestCase):\n    def test_price(self):\n        self.assertEqual(price(3), 6)\n".into(),
        ),
        (
            "tests/test_child.py",
            "import subprocess, sys, unittest\n\nclass ChildTest(unittest.TestCase):\n    def test_child(self):\n        self.assertEqual(subprocess.run([sys.executable, '-c', 'pass']).returncode, 0)\n".into(),
        ),
        (
            "tests/test_split.py",
            "import unittest\nfrom app import facade\n\nclass Split(unittest.TestCase):\n    def test_near(self):\n        self.assertEqual(facade.fx(), 1)\n\n    def test_other(self):\n        self.assertEqual(facade.fy(), 2)\n".into(),
        ),
    ];
    for name in [
        "tests/test_greet_1.py",
        "tests/test_greet_2.py",
        "tests/test_greet_3.py",
        "tests/test_greet_4.py",
        "tests/test_greet_5.py",
    ] {
        files.push((
            name,
            "import unittest\nfrom app.greet import hello\n\nclass GreetTest(unittest.TestCase):\n    def test_hello(self):\n        self.assertEqual(hello(), 'hi')\n".into(),
        ));
    }
    files
}

// covers: C-66, C-67, C-30, C-34
#[test]
fn unittest_reuses_the_test_modules_that_passed_on_the_files_they_read() {
    let Some(python) = with_python() else {
        return;
    };
    let r = Repo::new(&shop("reuse"));
    let unittest = || r.run(&["run", "unit", "--", &python, "-m", "unittest", "-v"]);

    let first = unittest();
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(
        !first.stderr.contains("test modules run"),
        "{}",
        first.stderr
    );
    // What each module read: its imports, followed, and the package above it; not another's.
    assert_eq!(
        r.reads("tests/test_cart.py"),
        [
            "app/__init__.py",
            "app/cart.py",
            "app/pricing.py",
            "tests/__init__.py",
            "tests/test_cart.py"
        ]
    );
    assert_eq!(
        store::map(&r.cache)["units"]["tests/test_child.py"]["notFollowed"],
        serde_json::json!(["subprocess"])
    );

    // One module changes: the test module that read it runs, the one that starts a process runs,
    // and one more of the six others that could be reused, as a check.
    r.write(
        "app/cart.py",
        "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n",
    );
    let changed = unittest();
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: unit: 3 of 9 test modules run; 6 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(
        changed
            .stderr
            .contains("test_total (tests.test_cart.CartTest.test_total) ... ok"),
        "{}",
        changed.stderr
    );
    assert!(changed.stderr.contains("Ran 3 tests"), "{}", changed.stderr);

    // The rest set changes: any test may read it, so every test module runs.
    r.write("requirements.txt", "nothing==2.0\n");
    let rest = unittest();
    assert_eq!(
        rest.plan_line(),
        "hushrun: unit: all 9 test modules run: requirements.txt changed, and any test may read it"
    );
}

// covers: C-68, C-39, C-40
#[test]
fn each_test_reads_what_it_ran_and_one_only_another_test_calls_reuses_it() {
    let Some(python) = with_python() else {
        return;
    };
    if !is_312(&python) {
        eprintln!("note: skipping: this Python has no sys.monitoring, so no test is watched");
        return;
    }
    let r = Repo::new(&shop("{mode: reuse, unit: test, reads: test}"));
    let args = ["run", "unit", "--", &python, "-m", "unittest", "-v"];
    let first = r.run(&args);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);

    // A module two imports away that only Split.test_near called changes: Split.test_other
    // didn't read it.
    r.write("app/deep.py", "def h():\n    return 1  # changed\n");
    let why = |test: &str| {
        let unit = format!("tests/test_split.py::{test}");
        r.run(&["why", "unit", &unit, "--", &python, "-m", "unittest", "-v"])
            .stdout
    };
    let near = why("Split.test_near");
    assert!(
        near.contains("runs: it read app/deep.py, which changed"),
        "{near}"
    );
    let other = why("Split.test_other");
    assert!(other.contains("would be reused"), "{other}");
    let changed = r.run(&args);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert!(
        changed
            .plan_line()
            .starts_with("hushrun: unit: 3 of 10 tests run, in "),
        "{}",
        changed.stderr
    );
    assert!(
        changed
            .stderr
            .contains("test_near (tests.test_split.Split.test_near) ... ok"),
        "{}",
        changed.stderr
    );
}

/// What one run of the adapter, outside hushrun, wrote: its exit status, its output and its facts.
fn plugin(r: &Repo, python: &str, run: Value, args: &[&str]) -> (i32, String, Vec<Value>) {
    let units = r.root.join("units");
    let _ = std::fs::remove_dir_all(&units);
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(units.join("hushrun.json"), run.to_string()).unwrap();
    let out = Command::new(python)
        .args(["-m", "unittest"])
        .args(args)
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", &units)
        .env("PYTHONPATH", r.adapter())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env_remove("HUSHRUN_UNITTEST_SESSION")
        .output()
        .unwrap();
    let facts = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.starts_with("unittest-") && name.ends_with(".json")
        })
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        facts,
    )
}

fn run_file(r: &Repo, unit: &str, reuse: &[&str]) -> Value {
    serde_json::json!({
        "schema": "hushrun.units/v0",
        "root": r.repo,
        "reuse": reuse,
        "pythonFiles": ["test*.py"],
        "unit": unit,
        "reads": "file",
        "reuseTests": {},
    })
}

fn unit<'a>(facts: &'a [Value], name: &str) -> &'a Value {
    facts[0]["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["unit"] == name)
        .unwrap_or_else(|| panic!("{name} in {facts:?}"))
}

// covers: C-67, C-69
#[test]
fn the_adapter_removes_what_it_is_told_and_no_tests_ran_stays_unittests_own() {
    let Some(python) = with_python() else {
        return;
    };
    let r = Repo::new(&shop("reuse"));
    // hushrun writes its adapter on a run.
    let first = r.run(&["run", "unit", "--", &python, "-m", "unittest"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let all = [
        "tests/test_cart.py",
        "tests/test_pricing.py",
        "tests/test_child.py",
        "tests/test_split.py",
        "tests/test_greet_1.py",
        "tests/test_greet_2.py",
        "tests/test_greet_3.py",
        "tests/test_greet_4.py",
        "tests/test_greet_5.py",
    ];
    // Every test removed: unittest's "no tests ran" (5 from Python 3.12) is a pass.
    let (code, out, facts) = plugin(&r, &python, run_file(&r, "file", &all), &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Ran 0 tests"), "{out}");
    assert_eq!(facts[0]["removedItems"], 10);
    assert_eq!(unit(&facts, "tests/test_cart.py")["deselected"], true);
    // Nothing removed and nothing selected: unittest's own status.
    if is_312(&python) {
        let none = run_file(&r, "file", &[]);
        let (code, _, _) = plugin(&r, &python, none, &["-k", "nothing_matches"]);
        assert_eq!(code, 5);
    }
    // Some removed: the rest run, and say what they read.
    let (code, out, facts) = plugin(&r, &python, run_file(&r, "file", &all[1..]), &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Ran 1 test"), "{out}");
    let cart = unit(&facts, "tests/test_cart.py");
    assert_eq!(
        (&cart["selected"], &cart["finished"]),
        (&1.into(), &1.into())
    );
    // A second unittest of the same run, which didn't claim the run's directory, removes nothing.
    let units = r.root.join("units");
    std::fs::write(units.join("claim"), "1").unwrap();
    let out = Command::new(&python)
        .args(["-m", "unittest"])
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", &units)
        .env("PYTHONPATH", r.adapter())
        .env_remove("HUSHRUN_UNITTEST_SESSION")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Ran 10 tests"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// covers: C-69, C-32
#[test]
fn a_class_whose_end_fails_keeps_no_pass_for_its_tests_and_a_module_that_fails_to_load_fails() {
    let Some(python) = with_python() else {
        return;
    };
    let mut files = shop("{mode: reuse, unit: test}");
    files.push((
        "tests/test_teardown.py",
        "import unittest\n\nclass Leaks(unittest.TestCase):\n    @classmethod\n    def tearDownClass(cls):\n        raise RuntimeError('leaked')\n\n    def test_a(self):\n        pass\n\nclass Clean(unittest.TestCase):\n    def test_b(self):\n        pass\n".into(),
    ));
    files.push((
        "tests/test_broken.py",
        "import no_such_module_here\n".into(),
    ));
    files.push((
        "tests/test_skipped.py",
        "import unittest\nraise unittest.SkipTest('not here')\n".into(),
    ));
    let r = Repo::new(&files);
    let ran = r.run(&["run", "unit", "--", &python, "-m", "unittest"]);
    assert_ne!(ran.code, 0, "{}", ran.stderr);
    let (code, _, facts) = plugin(&r, &python, run_file(&r, "test", &[]), &[]);
    assert_eq!(code, 1);
    let teardown = unit(&facts, "tests/test_teardown.py");
    assert_eq!(teardown["failed"], true);
    let tests = teardown["tests"].as_array().unwrap();
    let leaks = tests.iter().find(|t| t["id"] == "Leaks.test_a").unwrap();
    assert_eq!(leaks["finished"], false);
    assert!(
        leaks["notKept"]
            .as_str()
            .unwrap()
            .contains("class tests.test_teardown.Leaks failed"),
        "{leaks}"
    );
    let clean = tests.iter().find(|t| t["id"] == "Clean.test_b").unwrap();
    assert_eq!(clean["finished"], true);
    assert_eq!(unit(&facts, "tests/test_broken.py")["failed"], true);
    let skipped = unit(&facts, "tests/test_skipped.py");
    assert_eq!(
        (&skipped["failed"], &skipped["skipped"]),
        (&false.into(), &1.into())
    );
    assert_eq!(facts[0]["failuresByUnit"], true);
    // `hushrun why` says why the test kept no pass.
    let why = r.run(&[
        "why",
        "unit",
        "tests/test_teardown.py::Leaks.test_a",
        "--",
        &python,
        "-m",
        "unittest",
    ]);
    assert!(why.stdout.contains("runs"), "{}", why.stdout);
}

// covers: C-66
#[test]
fn the_sitecustomize_hushrun_hides_still_runs_and_a_python_a_test_starts_observes_nothing() {
    let Some(python) = with_python() else {
        return;
    };
    let mut files = shop("reuse");
    files.push((
        "site/sitecustomize.py",
        "import os\nos.environ['FROM_THE_HIDDEN_SITECUSTOMIZE'] = '1'\n".into(),
    ));
    files.push((
        "tests/test_site.py",
        "import os, unittest\n\nclass SiteTest(unittest.TestCase):\n    def test_hidden_ran(self):\n        self.assertEqual(os.environ.get('FROM_THE_HIDDEN_SITECUSTOMIZE'), '1')\n".into(),
    ));
    let r = Repo::new(&files);
    let site = r.repo.join("site");
    let site = site.to_str().unwrap();
    let ran = r.run_with(
        &["run", "unit", "--", &python, "-m", "unittest"],
        &[("PYTHONPATH", site)],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    // COMMAND's own `env PYTHONPATH=…` would replace the one that loads the adapter: said.
    r.write("app/greet.py", "def hello():\n    return 'hi'  # changed\n");
    let assign = format!("PYTHONPATH={site}");
    let assigned = r.run(&[
        "run", "unit", "--", "env", &assign, &python, "-m", "unittest",
    ]);
    assert!(
        assigned.stderr.contains("COMMAND sets PYTHONPATH itself"),
        "{}",
        assigned.stderr
    );
    // The test that starts a Python of its own: that Python is the test's, which isn't followed,
    // and wrote no facts of its own.
    assert_eq!(
        store::map(&r.cache)["units"]["tests/test_child.py"]["notFollowed"],
        serde_json::json!(["subprocess"])
    );
    assert!(
        r.reads("tests/test_site.py")
            .contains(&"tests/test_site.py".to_string())
    );
}

/// A Django project of three apps: a shop with a model, a fixture, a template and a module only
/// it imports; a blog and news that import nothing of the shop's.
fn django_project() -> Vec<(&'static str, String)> {
    let settings = "SECRET_KEY = 'x'\nINSTALLED_APPS = ['django.contrib.contenttypes', 'django.contrib.auth', 'shop', 'blog', 'news']\nDATABASES = {'default': {'ENGINE': 'django.db.backends.sqlite3', 'NAME': 'db.sqlite3'}}\nTEMPLATES = [{'BACKEND': 'django.template.backends.django.DjangoTemplates', 'APP_DIRS': True}]\nUSE_TZ = True\nDEFAULT_AUTO_FIELD = 'django.db.models.AutoField'\n";
    let manage = "import os, sys\n\nif __name__ == '__main__':\n    os.environ.setdefault('DJANGO_SETTINGS_MODULE', 'proj.settings')\n    from django.core.management import execute_from_command_line\n    execute_from_command_line(sys.argv)\n";
    let simple = |class: &str| {
        format!(
            "from django.test import SimpleTestCase\n\nclass {class}(SimpleTestCase):\n    def test_it(self):\n        self.assertTrue(True)\n"
        )
    };
    vec![
        ("hushrun.yaml", yaml("reuse")),
        (".gitignore", "__pycache__/\ndb.sqlite3\n".into()),
        ("manage.py", manage.into()),
        ("proj/__init__.py", String::new()),
        ("proj/settings.py", settings.into()),
        ("shop/__init__.py", String::new()),
        (
            "shop/models.py",
            "from django.db import models\n\nclass Item(models.Model):\n    name = models.CharField(max_length=20)\n    price = models.IntegerField()\n".into(),
        ),
        ("shop/pricing.py", "def with_tax(p):\n    return p * 2\n".into()),
        ("shop/migrations/__init__.py", String::new()),
        (
            "shop/migrations/0001_initial.py",
            "from django.db import migrations, models\n\nclass Migration(migrations.Migration):\n    initial = True\n    dependencies = []\n    operations = [migrations.CreateModel(name='Item', fields=[('id', models.AutoField(primary_key=True)), ('name', models.CharField(max_length=20)), ('price', models.IntegerField())])]\n".into(),
        ),
        (
            "shop/fixtures/items.json",
            "[{\"model\": \"shop.item\", \"pk\": 1, \"fields\": {\"name\": \"a\", \"price\": 3}}]\n"
                .into(),
        ),
        ("shop/templates/shop/item.txt", "Item {{ item.name }}\n".into()),
        (
            "shop/tests.py",
            "from django.template.loader import render_to_string\nfrom django.test import TestCase\nfrom shop.models import Item\nfrom shop import pricing\n\nclass ItemTest(TestCase):\n    fixtures = ['items.json']\n\n    def test_fixture(self):\n        self.assertEqual(pricing.with_tax(Item.objects.get(pk=1).price), 6)\n\n    def test_render(self):\n        self.assertEqual(render_to_string('shop/item.txt', {'item': Item.objects.get(pk=1)}).strip(), 'Item a')\n".into(),
        ),
        ("blog/__init__.py", String::new()),
        ("blog/tests.py", simple("BlogTest")),
        ("news/__init__.py", String::new()),
        ("news/tests.py", simple("NewsTest")),
    ]
}

// covers: C-66, C-67
#[test]
fn djangos_manage_py_test_reuses_the_apps_whose_files_didnt_change() {
    let Some(python) = with_django() else {
        return;
    };
    let r = Repo::new(&django_project());
    let manage = || r.run(&["run", "unit", "--", &python, "manage.py", "test", "-v", "2"]);
    let first = manage();
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // The shop's module read what it imports; the blog's read none of it, though manage.py,
    // under every frame of the run, is read by both, and so are the settings, the models and the
    // migrations, which Django loads for every test. (The fixture and the template are the rest
    // set's, which every test may read.)
    let shop = r.reads("shop/tests.py");
    for file in [
        "manage.py",
        "proj/settings.py",
        "shop/migrations/0001_initial.py",
        "shop/models.py",
        "shop/pricing.py",
    ] {
        assert!(shop.contains(&file.to_string()), "{file} in {shop:?}");
    }
    let blog = r.reads("blog/tests.py");
    for file in ["manage.py", "proj/settings.py", "shop/models.py"] {
        assert!(blog.contains(&file.to_string()), "{file} in {blog:?}");
    }
    for file in ["shop/pricing.py", "shop/tests.py"] {
        assert!(!blog.contains(&file.to_string()), "{file} in {blog:?}");
    }
    // Django's in-memory test database is no file.
    assert!(!blog.iter().any(|f| f.contains("memorydb")), "{blog:?}");

    r.write(
        "shop/pricing.py",
        "def with_tax(p):\n    return p * 2  # changed\n",
    );
    let changed = manage();
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: unit: 2 of 3 test modules run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
    );

    // Django's parallel runner is said, and everything runs.
    r.write(
        "shop/pricing.py",
        "def with_tax(p):\n    return p * 2  # again\n",
    );
    let parallel = r.run(&[
        "run",
        "unit",
        "--",
        &python,
        "manage.py",
        "test",
        "--parallel",
        "2",
    ]);
    assert!(
        parallel
            .stderr
            .contains("Django's --parallel runs the tests in worker processes"),
        "{}",
        parallel.stderr
    );
}
