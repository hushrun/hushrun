//! \[ADR 0069\] `tests: reuse` (C-5, C-10, C-26, C-27, C-32 to C-34): inside a run whose check
//! missed, the test files that already passed on the files they read aren't run again; audit runs
//! check that, and a disagreement suspends reuse until a person turns it on again.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- python -m pytest …` would, through pytest's adapter's own type, `pytest::Pytest`
//! (`common/pytest.rs`), and the code every adapter shares, and never starts the `hushrun` binary.
//! pytest runs as COMMAND, for real. What only the CLI shows (a run's last line, the check's own
//! result and skip, the audit's messages, a person resuming reuse, a results server, CI, `hushrun
//! ready` and the hook) is in `e2e_pytest/reuse.rs`; the Cargo and Go tests, whose COMMAND starts
//! the binary itself, are in `e2e_cargo/reuse.rs`. The fixtures both share are in
//! `common/reuse.rs`.
//!
//! The fixtures need Python with pytest and pytest-xdist (`common/python.rs`). They say so and
//! pass where those are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/reuse.rs"]
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

use std::path::Path;

use serde_json::{Value, json};

use fixtures::{hidden, shop};
use in_process::{Ran, Repo};
use pytest::{pytest_command, run};
use python::{has_pytest, python, tools};

fn repo_of(files: &[(String, String)]) -> Repo {
    pytest::repo(files)
}

/// How many unit results this machine holds.
fn unit_count(r: &Repo) -> usize {
    r.unit_results().values().sum()
}

/// The line that says what the plan reuses, or why every test file runs.
fn plan_line(ran: &Ran) -> &str {
    ran.lines
        .iter()
        .find(|l| l.contains("test files run") || l.contains("every test file runs"))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", ran.said()))
}

// covers: C-34, C-10, C-27, C-33
#[test]
fn pytest_reuses_what_already_passed_on_the_files_it_read() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&shop("reuse"));
    let python = python();
    let pytest = |extra: &[&str]| run(&r, &pytest_command(&python, extra), &[]);

    // The first run: every test file runs, and \[ADR 0074\] the plan says nothing, since no
    // pytest has reported this check's test files here yet: the COMMAND might not start one.
    let first = pytest(&[]);
    assert_eq!(first.code, 0, "{}", first.said());
    assert!(!first.said().contains("test files run"), "{}", first.said());
    assert_eq!(unit_count(&r), 7, "every observed test file that passed");

    // One module changes: the test file that read it runs, with one audited test file of the six
    // that could be reused, and the one that starts a process, which is never reused.
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let changed = pytest(&[]);
    assert_eq!(changed.code, 0, "{}", changed.said());
    assert_eq!(
        plan_line(&changed),
        "unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(
        changed.stdout.contains("5 deselected"),
        "{}",
        changed.stdout
    );
    // \[ADR 0074\] With what reusing saved: the reused test files' recorded durations, added up.
    let saved = changed.units["savedMs"].as_u64().expect("savedMs");
    assert_eq!(
        changed.units,
        json!({"total": 8, "ran": 3, "reused": 5, "savedMs": saved, "audited": 1})
    );
    // Reuse never chains: a reused test file records nothing.
    assert_eq!(unit_count(&r), 7 + 3 - 1, "{}", changed.said());
    // The run's last line, the check's result, this machine's stats, the skip of the same code
    // and `--force` are the CLI's: e2e_pytest/reuse.rs.

    // The rest set changes: any test may read it, so every test file runs.
    r.write("requirements.txt", "nothing==2.0\n");
    let rest = pytest(&[]);
    assert_eq!(
        plan_line(&rest),
        "unit: all 8 test files run: requirements.txt changed, and any test may read it"
    );
    // A new source can change what tests load: every test file runs.
    r.write("app/extra.py", "X = 1\n");
    let new = pytest(&[]);
    assert_eq!(
        plan_line(&new),
        "unit: all 8 test files run: app/extra.py is new, which can change what tests load"
    );
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
    let pytest = || run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let changed = pytest();
    // test_cart (changed), test_child (not observed), the two `always` names, and one audited of
    // the three left: five run.
    assert_eq!(
        plan_line(&changed),
        "unit: 5 of 8 test files run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
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
    let pytest = || run(&r, &pytest_command(&python, &[]), &[]);
    let first = pytest();
    assert_eq!(first.code, 0, "{}", first.said());
    // tests/check_extra.py was a source; pytest now takes it for a test file. It is on disk.
    r.write(
        "pytest.ini",
        "[pytest]\ntestpaths = tests\npythonpath = .\npython_files = test_*.py check_*.py\n",
    );
    let moved = pytest();
    assert_eq!(moved.code, 0, "{}", moved.said());
    assert_eq!(
        plan_line(&moved),
        "unit: all 9 test files run: tests/check_extra.py is now a test file, which can change what tests load"
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
    let pytest = || run(&r, &pytest_command(&python, &[]), &[]);
    r.write("app/cart.py", "def total(xs):\n    return 0\n");
    let failing = pytest();
    assert_eq!(failing.code, 1, "{}", failing.said());
    // Every test file that ran whole and passed is recorded, and the failing one's fail; the one
    // that starts a process, nothing.
    assert_eq!(unit_count(&r), 7, "{}", failing.said());
    r.write(
        "app/cart.py",
        "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)\n",
    );
    let fixed = pytest();
    assert_eq!(fixed.code, 0, "{}", fixed.said());
    assert_eq!(
        plan_line(&fixed),
        "unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
}

// covers: C-34, C-161
#[test]
fn an_injected_missed_dependency_is_caught_by_an_audit_and_suspends_reuse() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let pytest = || run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    // The recorded pass of test_hidden.py is wrong for what it reads now: the flag, which its key
    // can't see. Only it could be reused, so it is the one audited.
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    let caught = pytest();
    assert_eq!(caught.code, 1, "{}", caught.said());
    assert_eq!(
        plan_line(&caught),
        "unit: all 2 test files run; 1 that already passed on the files it read runs anyway, as a check"
    );
    // It failed again when it ran once more, alone: a disagreement, which suspends reuse. What
    // the CLI says of it, and a person turning reuse on again, are in e2e_pytest/reuse.rs.
    assert_eq!(r.disagreements(), ["test_hidden.py"], "{}", caught.said());
    // \[C-161\] A disagreement marks nothing: its fail says no `again`.
    assert!(
        r.newest_unit_run().iter().all(|u| u.get("again").is_none()),
        "{:#?}",
        r.newest_unit_run()
    );
    // The next run reuses nothing, and says why.
    r.write("libb.py", "Y = 1  # again\n");
    let suspended = pytest();
    assert!(
        plan_line(&suspended).starts_with(
            "unit: every test file runs: reuse of its test files is suspended by a person"
        ),
        "{}",
        suspended.said()
    );
}

// covers: C-34, C-160, C-161, C-163
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
    let pytest = || run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    std::fs::remove_file(r.repo.join("seen")).unwrap();
    r.write("libb.py", "Y = 1  # changed\n");
    let flaky = pytest();
    // COMMAND failed, and its exit code is the run's; the second run passed: flaky, as G13 says
    // of checks, and reuse stays on. What the CLI says of it is in e2e_pytest/reuse.rs.
    assert_eq!(flaky.code, 1, "{}", flaky.said());
    assert!(r.disagreements().is_empty(), "{}", flaky.said());
    assert!(!flaky.said().contains("suspended"), "{}", flaky.said());
    // \[C-161\] Its fail is what is recorded, and says that it passed when it ran again; with
    // `unit: file` a test file is the smallest unit (C-160). Only the fail says it.
    let records = r.newest_unit_run();
    let fails: Vec<&Value> = records.iter().filter(|u| u["result"] == "fail").collect();
    assert_eq!(fails.len(), 1, "{records:#?}");
    assert_eq!(fails[0]["level"], "file", "{records:#?}");
    assert_eq!(fails[0]["again"], "pass", "{records:#?}");
    assert!(
        records
            .iter()
            .filter(|u| u["result"] == "pass")
            .all(|u| u["level"] == "file" && u.get("again").is_none()),
        "{records:#?}"
    );
    // \[C-163\] It was the one audited, and the run's counts say so.
    assert_eq!(flaky.units["audited"], 1, "{}", flaky.units);
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
    let out = hushrun_testkit::tool::command(python())
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
    run(&r, &pytest_command(&python(), &[]), &[]);
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
        let out = hushrun_testkit::tool::command(python())
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
    run(&r, &pytest_command(&python(), &[]), &[]);
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

// covers: C-22, C-34, C-30
#[test]
fn a_check_where_no_pytest_ever_reported_says_nothing_of_test_files() {
    // A check whose COMMAND hushrun doesn't recognize gets the pytest adapter, which does nothing
    // where pytest never starts: its plan counts no file that only looks like a test. What
    // `hushrun ready` says of it is in e2e_pytest/reuse.rs.
    let r = repo_of(&shop("reuse"));
    let ran = run(&r, &["git", "--version"], &[]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(!ran.said().contains("test file"), "{}", ran.said());
    r.write("app/greet.py", "def hello():\n    return 'hi'  # changed\n");
    let ran = run(&r, &["git", "--version"], &[]);
    assert!(!ran.said().contains("test file"), "{}", ran.said());
}
