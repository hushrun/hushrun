//! \[ADR 0110\] The standard library's unittest, and Django's `manage.py test` (C-30 to C-34, C-39,
//! C-66 to C-69): a unit is a test module, a test one `TestCase` method, what each read is what
//! hushrun's observer saw through the `sitecustomize.py` it puts first on `PYTHONPATH`, and the
//! tests of what already passed are removed from the suite before it runs.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- python -m unittest …` would, through unittest's adapter's own type,
//! `unittest::Unittest`, and the code every adapter shares, and never starts the `hushrun` binary
//! (`common/in_process.rs`). Python runs as COMMAND, for real. What only the CLI shows of unittest
//! (`hushrun why`) is in `e2e_unittest.rs`. The fixtures, and the tools they need, are in
//! `common/unittest.rs`.

#![cfg(unix)]

#[path = "common/unittest.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;
use std::path::PathBuf;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, unittest};
use serde_json::Value;

use fixtures::*;
use in_process::{Ran, Repo};

/// What unittest's adapter answers that never changes in a run, for the code every adapter shares.
fn profile(adapter: &unittest::Unittest) -> Profile {
    Profile {
        word: adapter.word(),
        runner: adapter.runner(),
        test_file: adapter.test_file(),
        tests: adapter.tests(),
        hints: adapter.hints(),
        binds: adapter.binds(),
        keys: adapter.keys(),
    }
}

/// `hushrun run unit -- ARGS`, in-process, through unittest's adapter alone.
fn run(r: &Repo, args: &[&str]) -> Ran {
    run_with(r, args, &[])
}

/// `hushrun run unit -- ARGS`, with `env` besides, in-process, through unittest's adapter alone.
fn run_with(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to unittest's adapter (`unittest::recognized`), with
    // no trait object: what COMMAND is, and whether the adapter can load safely for it.
    let adapter = match unittest::recognize(hushrun::units::command::without_env(&command)) {
        Some(Ok(adapter)) => adapter,
        Some(Err(why)) => return in_process::Run::new(r, "unit", args, env).unobserved(&why),
        None => panic!("{args:?} doesn't start unittest"),
    };
    observe(r, args, env, &adapter)
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order, on `unittest::Unittest` itself (`units_gradle.rs`'s `observe`, with the type changed).
fn observe(r: &Repo, args: &[&str], env: &[(&str, &str)], adapter: &unittest::Unittest) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    // \[C-32\] An adapter that can't load observes nothing: COMMAND runs as it is.
    if let Err(why) = &variables {
        return run.unobserved(why);
    }
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // unittest's adapter keeps the trait's `plan`, which is its `plan_from_files`.
            Ok(context) => {
                let planning = adapter.plan_from_files(&observer.plan_at(&input, &context));
                observer.planned(&input, context, planning)
            }
            Err(planned) => planned,
        };
        drop(input);
        run.planned(&observer, planned);
    }
    // COMMAND, once, or \[C-32\] once more with nothing left out when the adapter left out what
    // the plan didn't list.
    let mut starts = 0;
    let (code, mut observed) = loop {
        starts += 1;
        let start = adapter.start_variables(observer.run_dir());
        let (code, passed) = run.start_command(&observer, &start);
        let end = run.finish_input(passed);
        let observed = match observer.read_context(&end) {
            Ok(context) => {
                let read = adapter.read(&observer.read_at(&end, &context));
                observer.finished(&end, context, read)
            }
            Err(observed) => observed.map(|observed| *observed),
        };
        drop(end);
        if starts == 1 && observed.as_ref().is_some_and(|o| o.left_out.is_some()) {
            let extras = adapter.again(observer.root(), observer.cwd(), &BTreeSet::new());
            if observer.whole_again(extras).is_ok() {
                continue;
            }
        }
        break (code, observed);
    };
    // \[C-34\] An audited unit that failed runs once more, alone.
    if let Some(o) = observed.as_mut().filter(|o| !o.again.is_empty()) {
        let wanted: BTreeSet<String> = o.again.keys().cloned().collect();
        let extras = adapter.again(observer.root(), o.cwd(), o.unit_files());
        let ended = match observer.again(o, &wanted, extras) {
            Some(again) => {
                let start = adapter.start_variables(again.dir());
                let again = again.with_start(start);
                let first = adapter.start_variables(observer.run_dir());
                let passed = run.start_again(&observer, &first, &again);
                let files = again.files();
                let map = in_process::no_map();
                let read = adapter
                    .read(&observer.again_read_at(&files, o, &run.tree, passed, &map))
                    .ok()
                    .flatten();
                observer.again_ended(&again, o, passed, read)
            }
            None => in_process::Run::not_again(&wanted),
        };
        run.disagreements(o, ended);
    }
    run.record(code, observed)
}

/// The line that says what the plan reuses, or why every test module runs.
fn plan_line(ran: &Ran) -> &str {
    ran.lines
        .iter()
        .find(|l| {
            l.contains("test modules run")
                || l.contains("tests run")
                || l.contains("test module runs")
        })
        .unwrap_or_else(|| panic!("no plan line in:\n{}", ran.said()))
}

/// The adapter's directory hushrun wrote in this cache.
fn adapter_dir(r: &Repo) -> PathBuf {
    std::fs::read_dir(r.cache.join("adapters"))
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

// covers: C-66, C-67, C-30, C-34
#[test]
fn unittest_reuses_the_test_modules_that_passed_on_the_files_they_read() {
    let Some(python) = with_python() else {
        return;
    };
    let r = repo(&shop("reuse"));
    let unittest = || run(&r, &[&python, "-m", "unittest", "-v"]);

    let first = unittest();
    assert_eq!(first.code, 0, "{}", first.said());
    assert!(
        !first.said().contains("test modules run"),
        "{}",
        first.said()
    );
    // What each module read: its imports, followed, and the package above it; not another's.
    assert_eq!(
        reads(&r, "tests/test_cart.py"),
        [
            "app/__init__.py",
            "app/cart.py",
            "app/pricing.py",
            "tests/__init__.py",
            "tests/test_cart.py"
        ]
    );
    assert_eq!(
        r.map()["units"]["tests/test_child.py"]["notFollowed"],
        serde_json::json!(["subprocess"])
    );

    // One module changes: the test module that read it runs, the one that starts a process runs,
    // and one more of the six others that could be reused, as a check.
    r.write(
        "app/cart.py",
        "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n",
    );
    let changed = unittest();
    assert_eq!(changed.code, 0, "{}", changed.said());
    assert_eq!(
        plan_line(&changed),
        "unit: 3 of 9 test modules run; 6 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(
        changed
            .stderr
            .contains("test_total (tests.test_cart.CartTest.test_total) ... ok"),
        "{}",
        changed.stderr
    );
    // Three modules of one test each, or test_split's two when the audit chose it.
    assert!(
        changed.stderr.contains("Ran 3 tests") || changed.stderr.contains("Ran 4 tests"),
        "{}",
        changed.stderr
    );

    // The rest set changes: any test may read it, so every test module runs.
    r.write("requirements.txt", "nothing==2.0\n");
    let rest = unittest();
    assert_eq!(
        plan_line(&rest),
        "unit: all 9 test modules run: requirements.txt changed, and any test may read it"
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
    let r = repo(&shop("{mode: reuse, unit: test, reads: test}"));
    let args = [python.as_str(), "-m", "unittest", "-v"];
    let first = run(&r, &args);
    assert_eq!(first.code, 0, "{}", first.said());

    // A module two imports away that only Split.test_near called changes: Split.test_other
    // didn't read it, which `hushrun why` says of each (e2e_unittest.rs).
    r.write("app/deep.py", "def h():\n    return 1  # changed\n");
    let changed = run(&r, &args);
    assert_eq!(changed.code, 0, "{}", changed.said());
    assert!(
        plan_line(&changed).starts_with("unit: 3 of 10 tests run, in "),
        "{}",
        changed.said()
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
    let out = hushrun_testkit::tool::command(python)
        .args(["-m", "unittest"])
        .args(args)
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", &units)
        .env("PYTHONPATH", adapter_dir(r))
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
    let r = repo(&shop("reuse"));
    // hushrun writes its adapter on a run.
    let first = run(&r, &[&python, "-m", "unittest"]);
    assert_eq!(first.code, 0, "{}", first.said());
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
    let out = hushrun_testkit::tool::command(&python)
        .args(["-m", "unittest"])
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", &units)
        .env("PYTHONPATH", adapter_dir(&r))
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
    let r = repo(&teardown_shop());
    let ran = run(&r, &[&python, "-m", "unittest"]);
    assert_ne!(ran.code, 0, "{}", ran.said());
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
    // \[C-37\] What `hushrun why` says of the test that kept no pass is end to end:
    // e2e_unittest.rs.
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
    let r = repo(&files);
    let site = r.repo.join("site");
    let site = site.to_str().unwrap();
    let ran = run_with(&r, &[&python, "-m", "unittest"], &[("PYTHONPATH", site)]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    // COMMAND's own `env PYTHONPATH=…` would replace the one that loads the adapter: said.
    r.write("app/greet.py", "def hello():\n    return 'hi'  # changed\n");
    let assign = format!("PYTHONPATH={site}");
    let assigned = run(&r, &["env", &assign, &python, "-m", "unittest"]);
    assert!(
        assigned.said().contains("COMMAND sets PYTHONPATH itself"),
        "{}",
        assigned.said()
    );
    // The test that starts a Python of its own: that Python is the test's, which isn't followed,
    // and wrote no facts of its own.
    assert_eq!(
        r.map()["units"]["tests/test_child.py"]["notFollowed"],
        serde_json::json!(["subprocess"])
    );
    assert!(reads(&r, "tests/test_site.py").contains(&"tests/test_site.py".to_string()));
}

// covers: C-66, C-67
#[test]
fn djangos_manage_py_test_reuses_the_apps_whose_files_didnt_change() {
    let Some(python) = with_django() else {
        return;
    };
    let r = repo(&django_project());
    let manage = || run(&r, &[&python, "manage.py", "test", "-v", "2"]);
    let first = manage();
    assert_eq!(first.code, 0, "{}", first.said());
    // The shop's module read what it imports; the blog's read none of it, though manage.py,
    // under every frame of the run, is read by both, and so are the settings, the models and the
    // migrations, which Django loads for every test. (The fixture and the template are the rest
    // set's, which every test may read.)
    let shop = reads(&r, "shop/tests.py");
    for file in [
        "manage.py",
        "proj/settings.py",
        "shop/migrations/0001_initial.py",
        "shop/models.py",
        "shop/pricing.py",
    ] {
        assert!(shop.contains(&file.to_string()), "{file} in {shop:?}");
    }
    let blog = reads(&r, "blog/tests.py");
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
    assert_eq!(changed.code, 0, "{}", changed.said());
    assert_eq!(
        plan_line(&changed),
        "unit: 2 of 3 test modules run; 1 already passed on the files it read, and 1 more that already passed runs anyway, as a check"
    );

    // Django's parallel runner is said, and everything runs.
    r.write(
        "shop/pricing.py",
        "def with_tax(p):\n    return p * 2  # again\n",
    );
    let parallel = run(&r, &[&python, "manage.py", "test", "--parallel", "2"]);
    assert!(
        parallel
            .said()
            .contains("Django's --parallel runs the tests in worker processes"),
        "{}",
        parallel.said()
    );
}
