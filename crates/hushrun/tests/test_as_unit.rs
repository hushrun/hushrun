//! \[ADR 0080\] A test as the unit (C-5, C-39): with `tests.unit: test`, each test of a test file
//! is reused on its own, beside its test files, in two tiers; a test that wasn't followed costs
//! only itself; an audited test that fails runs once more, alone.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- python -m pytest …` would, through pytest's adapter's own type, `pytest::Pytest`,
//! and the code every adapter shares (`common/pytest.rs`), and never starts the `hushrun` binary.
//! pytest runs as COMMAND, for real. What only the CLI shows (`hushrun why`, `hushrun ready`, a
//! results server, the run's own lines and last line, the audit's second start and its
//! suspension, the check's skip) is in `e2e_pytest/test_as_unit.rs`. The fixtures both share are
//! `common/test_as_unit.rs`'s.
//!
//! The pytest fixtures need Python with pytest and pytest-xdist. They say so and pass where those
//! are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/test_as_unit.rs"]
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

use fixtures::*;
use in_process::{Ran, Repo};
use pytest::{pytest_command, repo, repo_of, run};
use python::*;

/// How many unit results this machine holds.
fn unit_results(r: &Repo) -> usize {
    r.unit_results().values().sum()
}

/// Whether a line the run's steps said is `line`.
fn says(ran: &Ran, line: &str) -> bool {
    ran.lines.iter().any(|l| l == line)
}

/// Whether a line the run's steps said holds `text`.
fn says_in(ran: &Ran, text: &str) -> bool {
    ran.lines.iter().any(|l| l.contains(text))
}

// covers: C-39, C-5, C-10, C-36, C-160, C-163
#[test]
fn a_test_that_failed_runs_alone_and_its_files_others_are_reused_one_by_one() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo(&shop("{mode: reuse, unit: test}"));
    let python = python();
    // The first run, with test_flips failing: every test runs, and each is recorded on its own
    // where its test file didn't pass whole.
    let first = run(&r, &pytest_command(&python, &[]), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    let map = r.map();
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
    assert_eq!(unit_results(&r), 1 + 6 + 3 + 3);
    // \[C-160\] Each failure is said once, by the smallest unit that holds it: test_flips, as a
    // test; test_cart's own fail carries no level. A test file whose tests record nothing of
    // their own in this run (the greet files, which passed whole) is a file.
    let levels = |records: &[Value]| {
        let mut out = std::collections::BTreeMap::<(String, String), usize>::new();
        for record in records {
            let level = record["level"].as_str().unwrap_or("none").to_string();
            let result = record["result"].as_str().unwrap().to_string();
            *out.entry((level, result)).or_default() += 1;
        }
        out
    };
    let pair = |level: &str, result: &str, n| ((level.to_string(), result.to_string()), n);
    assert_eq!(
        levels(&r.newest_unit_run()),
        [
            pair("file", "pass", 3),
            pair("none", "fail", 1),
            pair("test", "fail", 1),
            pair("test", "pass", 8),
        ]
        .into_iter()
        .collect(),
        "{:#?}",
        r.newest_unit_run()
    );
    // No second run passed and no runner retried: nothing says either.
    assert!(
        r.newest_unit_run()
            .iter()
            .all(|u| u.get("again").is_none() && u.get("retried").is_none())
    );
    let units = &first.units;
    // \[C-163\] A run with no plan audits nothing, and says nothing of it.
    assert!(units.get("audited").is_none(), "{units:#}");
    assert_eq!(units["tests"], json!({"total": 13, "ran": 13, "reused": 0}));
    assert_eq!(
        (units["total"].clone(), units["ran"].clone()),
        (json!(5), json!(5))
    );

    // The same files, the flip fixed: test_cart's own key failed, so its tests are asked one by
    // one: five passed on these files. test_child isn't reused whole, since one of its tests
    // wasn't followed: its other three may be. Of those eight, one, chosen by its key, runs as a
    // check; and of the greet files, which passed whole, one.
    let second = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert_eq!(
        hushrun_testkit::lines::drawn(second.line("unit: ")),
        hushrun_testkit::lines::drawn(
            "unit: 3 of 13 tests run, in 3 of 5 test files; 10 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    // pytest ran what the plan left: test_flips, test_spawns and the audited test, wherever the
    // draw found it.
    assert!(
        second.stdout.contains("3 passed, 10 deselected"),
        "{}",
        second.stdout
    );
    // The check's result: a pass, and its units' counts.
    let units = &second.units;
    assert!(units["savedMs"].is_u64(), "{units:#}");
    assert_eq!(units["total"], 5, "{units:#}");
    assert_eq!(units["tests"], json!({"total": 13, "ran": 3, "reused": 10}));
    // \[C-163\] The audit's test, counted as the plan's line counts it.
    assert_eq!(units["audited"], 1, "{units:#}");
    // \[C-10\] Its last line, which says the 10 tests reused, and the check's skip on the same
    // code again, are the CLI's: e2e_pytest/test_as_unit.rs.
    // Reuse never chains: a test file that didn't run whole records no pass of its own, and
    // a reused test nothing; the two tests that ran record theirs (test_spawns, not followed,
    // nothing), wherever the audited one sits: a greet file it sits in didn't run whole either.
    assert_eq!(unit_results(&r), 13 + 2, "{}", second.said());
    // A reused test keeps the row it stands on.
    let map = r.map();
    assert_eq!(
        map["units"]["tests/test_cart.py"]["tests"]["test_flips"],
        "passed"
    );
    assert_eq!(
        map["units"]["tests/test_cart.py"]["tests"]["test_one"],
        "passed"
    );
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
    let files = skips();
    let r = repo(&files);
    let python = python();
    let first = run(
        &r,
        &pytest_command(&python, &[]),
        &[("FAIL", "1"), ("SKIP", "1")],
    );
    assert_eq!(first.code, 1, "{}", first.said());
    let map = r.map();
    // The skipped tests are not run, and record nothing; the one that ran records its pass.
    assert_eq!(
        map["units"]["tests/test_skips.py"]["tests"],
        json!({"test_in_setup": "not-run", "test_not_run": "not-run", "test_ran": "passed",
               "test_skipped": "not-run"}),
        "{map:#}"
    );
    // \[C-37\] What `hushrun why` says of each is end to end: e2e_pytest/test_as_unit.rs.
    // In a run that passed, a skip is a pass, as before.
    let passed = run(&r, &pytest_command(&python, &[]), &[("SKIP", "1")]);
    assert_eq!(passed.code, 0, "{}", passed.said());
    let map = r.map();
    assert_eq!(
        map["units"]["tests/test_skips.py"]["tests"]["test_skipped"], "passed",
        "{map:#}"
    );
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
        let r = repo(&files);
        let python = python();
        let ran = run(&r, &pytest_command(&python, &[]), &[(var, "1")]);
        assert_ne!(ran.code, 0, "{var}: {}", ran.said());
        let map = r.map();
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
    let files = leak();
    let r = repo(&files);
    let python = python();
    let first = run(&r, &pytest_command(&python, &[]), &[("BUG", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    let map = r.map();
    let tests = &map["units"]["tests/test_leak.py"]["tests"];
    assert_ne!(tests["test_opens"], "passed", "{map:#}");
    assert_ne!(tests["test_more[3]"], "passed", "{map:#}");
    // \[C-37\] What `hushrun why` says of it is end to end: e2e_pytest/test_as_unit.rs.
    // Run again on the same code: the leak is caught again.
    let second = run(&r, &pytest_command(&python, &[]), &[("BUG", "1")]);
    assert_eq!(second.code, 1, "{}", second.said());
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
    let files = session_leak();
    let r = repo(&files);
    let python = python();
    let first = run(&r, &pytest_command(&python, &[]), &[("BUG", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    let map = r.map();
    assert_ne!(
        map["units"]["tests/test_l.py"]["tests"]["test_leaky"], "passed",
        "{map:#}"
    );
    // \[C-37\] What `hushrun why` says of its test file is end to end:
    // e2e_pytest/test_as_unit.rs.
    let second = run(&r, &pytest_command(&python, &[]), &[("BUG", "1")]);
    assert_eq!(second.code, 1, "{}", second.said());
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
    let r = repo(&files);
    let python = python();
    let first = run(&r, &pytest_command(&python, &[]), &[("FAIL_A", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    let map = r.map();
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
    let r = repo_of(&[
        ("hushrun.yaml", &yaml("observe")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", FAKE_RUNNER),
        ("test_a.py", "def test_a():\n    pass\n"),
    ]);
    let state = r.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let python = python();
    let state_dir = state.to_string_lossy().into_owned();
    let ran = run(
        &r,
        &[&python, "fake.py"],
        &[
            ("FAKE_STATE", state_dir.as_str()),
            ("FAKE_FILES", "test_a.py"),
            ("FAKE_LEFT_OUT", "1 2"),
            ("FAKE_EXIT_2", "1"),
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert_eq!(
        std::fs::read_dir(&state).unwrap().count(),
        1,
        "{}",
        ran.said()
    );
    assert!(
        says(
            &ran,
            "unit: test files weren't observed: its adapter left out what nothing listed (2 features of test_a.py nobody listed)"
        ),
        "{}",
        ran.said()
    );
    assert_eq!(unit_results(&r), 0);
    // The check's result keeps no units.
    assert!(ran.units.is_null(), "{}", ran.units);
}

// covers: C-32
#[test]
fn a_pass_on_a_stale_build_records_no_unit_and_neither_does_a_failure_on_it() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // The adapter knows the runner ran a program built before the last change: the run passed,
    // and proves nothing of this code. What the CLI says of it, and that it records no result of
    // the check, is end to end: e2e_pytest/test_as_unit.rs.
    let r = repo_of(&[
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
    let ran = run(&r, &[&python, "fake.py"], &env);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(ran.units.is_null(), "{}", ran.units);
    assert_eq!(unit_results(&r), 0);
    // A failure is recorded as usual.
    let failed = run(
        &r,
        &[&python, "fake.py"],
        &[env[0], env[1], env[2], ("FAKE_EXIT_2", "1")],
    );
    assert_eq!(failed.code, 1, "{}", failed.said());
    assert!(!failed.units.is_null(), "{}", failed.said());
    // \[C-32\] Its test file passed on that stale build in a run that failed: no pass of it is
    // kept.
    assert_eq!(unit_results(&r), 0);
}

// covers: C-39
#[test]
fn a_test_file_that_failed_while_none_of_its_tests_did_keeps_none_of_them() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // \[C-37\] What `hushrun why` says of its tests is end to end: e2e_pytest/test_as_unit.rs.
    let r = repo_of(&[
        ("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", UNEXPLAINED_RUNNER),
        (
            "test_a.py",
            "def test_one():\n    pass\n\ndef test_two():\n    pass\n",
        ),
    ]);
    let python = python();
    let ran = run(&r, &[&python, "fake.py"], &[]);
    assert_eq!(ran.code, 1, "{}", ran.said());
    let map = r.map();
    assert_eq!(
        map["units"]["test_a.py"]["tests"],
        json!({"test_one": "not-run", "test_two": "not-run"}),
        "{map:#}"
    );
}

// covers: C-39
#[test]
fn with_tests_as_the_unit_a_file_that_changed_runs_every_test_it_has() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo(&shop("{mode: reuse, unit: test}"));
    let python = python();
    assert_eq!(
        run(&r, &pytest_command(&python, &[]), &[("FAIL", "1")]).code,
        1
    );
    // cart.py changes: with `reads: file` every test of every test file that read it runs, those
    // that passed included: their keys hold what their file read.
    r.write(
        "app/cart.py",
        "def total(xs):\n    return sum(xs)  # changed\n",
    );
    let changed = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(changed.code, 0, "{}", changed.said());
    assert_eq!(
        changed.line("unit: "),
        "unit: 11 of 13 tests run, in 3 of 5 test files; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
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
    let files = two_tests();
    let r = repo(&files);
    let python = python();
    assert_eq!(
        run(&r, &pytest_command(&python, &[]), &[("FAIL", "1")]).code,
        1
    );
    // test_ok passed on these files, and is the one test that could be reused: it runs as a
    // check, and fails. It runs once more, alone, and fails again: a disagreement.
    let caught = run(&r, &pytest_command(&python, &[]), &[("HIDDEN", "1")]);
    assert_eq!(caught.code, 1, "{}", caught.said());
    assert_eq!(
        caught.line("unit: all"),
        "unit: all 2 tests run; 1 that already passed on the files it read runs anyway, as a check"
    );
    // The second run ran test_ok alone: test_flips was deselected one by one. What the CLI says
    // as it runs it once more, and as it suspends reuse, is end to end:
    // e2e_pytest/test_as_unit.rs.
    assert!(
        caught.stdout.contains("1 failed, 1 deselected"),
        "{}",
        caught.stdout
    );
    assert_eq!(r.disagreements(), ["tests/test_two.py::test_ok"]);
    // Suspended: every test file runs, tests too.
    let after = run(&r, &pytest_command(&python, &[]), &[]);
    assert!(
        says_in(
            &after,
            "unit: every test file runs: reuse of its test files is suspended"
        ),
        "{}",
        after.said()
    );
}

// covers: C-39, C-5
#[test]
fn observe_says_what_reusing_tests_would_have_done_and_test_files_stay_as_they_were() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo(&shop("{mode: observe, unit: test}"));
    let python = python();
    assert_eq!(
        run(&r, &pytest_command(&python, &[]), &[("FAIL", "1")]).code,
        1
    );
    let second = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    // Everything ran: observe changes nothing.
    assert!(second.stdout.contains("13 passed"), "{}", second.stdout);
    assert!(
        hushrun_testkit::lines::without_saving(second.line("unit: with `tests: reuse`"))
            .starts_with(
                "unit: with `tests: reuse`, 2 of 13 tests would have run, in 2 of 5 test files; 11 already passed on the files they read"
            ),
        "{}",
        second.said()
    );
    assert_eq!(
        second.units["tests"],
        json!({"total": 13, "ran": 13, "reused": 0})
    );
    // \[ADR 0083\] `reads: test`: pytest watches each test on Python 3.12 and later, and says
    // nothing; below, it says once why each test is taken to have read its test file's list.
    r.write(
        "hushrun.yaml",
        &yaml("{mode: observe, unit: test, reads: test}"),
    );
    let reads = run(&r, &pytest_command(&python, &[]), &[]);
    let said = reads
        .lines
        .join("\n")
        .matches("each test is taken to have read what its test file read")
        .count();
    if watches() {
        assert_eq!(said, 0, "{}", reads.said());
    } else {
        assert_eq!(said, 1, "{}", reads.said());
        assert!(
            says_in(&reads, "has no sys.monitoring, which watching a test needs"),
            "{}",
            reads.said()
        );
    }
    // Test files, as before: no test is told, and the counts are test files'.
    r.write("hushrun.yaml", &yaml("observe"));
    r.write("app/greet.py", "def hello():\n    return 'hi'  # changed\n");
    let files = run(&r, &pytest_command(&python, &[]), &[]);
    assert!(
        files
            .line("unit: with `tests: reuse`")
            .contains("test files would have run"),
        "{}",
        files.said()
    );
    assert!(files.units.get("tests").is_none(), "{}", files.units);
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
    let r = repo(&[
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
    let _ = run(&r, &pytest_command(&python, &[]), &[]);
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
    let out = hushrun_testkit::tool::command(python())
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
    let r = repo(&[
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
    let _ = run(&r, &pytest_command(&python, &[]), &[]);
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
    let has_cov = hushrun_testkit::tool::command(&python)
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
    let r = repo(&[
        ("hushrun.yaml".into(), yaml("observe")),
        (
            "tests/test_db.py".into(),
            "import os, sqlite3\n\n\
             def test_db(tmp_path):\n    path = os.path.join(os.path.dirname(__file__), 'data', 'shop.sqlite')\n    with sqlite3.connect(path) as c:\n        c.execute('create table if not exists t (x)')\n    sqlite3.connect(':memory:').close()\n"
                .into(),
        ),
        ("tests/data/keep.txt".into(), String::new()),
    ]);
    // Writes the plugin into the cache, as a run does: a run here would write the database into
    // the scope as it ran, which `hushrun run` would observe nothing of.
    hushrun::units::pytest::install(&r.cache).expect("the plugin is written");
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
    let r = repo(&files);
    // A run writes the plugin where hushrun keeps it.
    run(&r, &pytest_command(&python(), &[]), &[]);
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

// covers: C-33, C-39
#[test]
fn observe_counts_what_it_couldnt_follow_in_the_unit_its_line_counts() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_a.py started a process outside its tests (a package's TestMain, a module that runs
    // one at import), and its runner couldn't tell its tests' ends: its two tests would always
    // run, whatever else kept each from a pass. The line counts tests, so does its tail.
    let python = python();
    let names = ["test_a.py", "test_b.py", "test_c.py"];
    let mut files = vec![
        (
            "hushrun.yaml".to_string(),
            yaml("{mode: observe, unit: test}"),
        ),
        (".gitignore".to_string(), "__pycache__/\n".to_string()),
        ("fake.py".to_string(), RULES_RUNNER.to_string()),
    ];
    for name in names {
        files.push((name.to_string(), "def t1():\n    pass\n".to_string()));
    }
    let r = repo(&files);
    let units = json!([
        {"unit": "test_a.py", "selected": 2, "finished": 2, "reads": ["test_a.py"],
         "notFollowed": [{"why": "subprocess", "detail": "git"}],
         "tests": [{"id": "t1"}, {"id": "t2"}]},
        {"unit": "test_b.py", "selected": 1, "finished": 1, "reads": ["test_b.py"],
         "tests": [{"id": "t1", "finished": true}]},
        {"unit": "test_c.py", "selected": 1, "finished": 1, "reads": ["test_c.py"],
         "tests": [{"id": "t1", "finished": true}]}
    ])
    .to_string();
    let env = [("FAKE_UNITS", units.as_str())];
    let first = run(&r, &[&python, "fake.py"], &env);
    assert_eq!(first.code, 0, "{}", first.said());
    r.write("test_c.py", "def t1():\n    pass\n# changed\n");
    let second = run(&r, &[&python, "fake.py"], &env);
    let line = second.line("unit: with");
    assert!(
        line.contains("hushrun couldn't follow all that 2 of them read (a process it started), so they would always run"),
        "{line}"
    );
}

// covers: C-34, C-39
#[test]
fn with_tests_as_the_unit_an_audit_runs_two_percent_of_the_tests_whatever_a_files_size() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // tests/test_big.py holds 100 tests, tests/test_small.py one. test_small.py changes: of what
    // could be left out, 100 tests, an audit runs 2, not the whole of test_big.py, which is then
    // left out test by test.
    let files = big_and_small();
    let r = repo(&files);
    let python = python();
    let first = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    r.write(
        "tests/test_small.py",
        "def test_s():\n    pass\n# changed\n",
    );
    let second = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert_eq!(
        second.line("unit: "),
        "unit: 3 of 101 tests run, in 2 of 2 test files; 98 already passed on the files they read, and 2 more that already passed run anyway, as a check",
        "{}",
        second.said()
    );
    assert!(second.stdout.contains("3 passed"), "{}", second.stdout);
    // \[C-34\] That the run's last line counts the 98 left out as reused tests is end to end:
    // e2e_pytest/test_as_unit.rs.
}

// covers: C-34, C-39
#[test]
fn an_audited_test_of_a_file_reused_whole_that_fails_again_suspends_reuse() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // tests/test_target.py passed whole; it fails while BROKEN is set, which no key holds. With
    // tests/test_changed.py changed, its one test is all the plan could leave out: the audit runs
    // it whatever its draw, it fails, runs once more alone, fails again, and reuse is suspended.
    let files = target_and_changed();
    let r = repo(&files);
    let python = python();
    let first = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(first.code, 0, "{}", first.said());
    r.write(
        "tests/test_changed.py",
        "def test_c():\n    pass\n# changed\n",
    );
    let second = run(&r, &pytest_command(&python, &[]), &[("BROKEN", "1")]);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        says(
            &second,
            "unit: all 2 tests run; 1 that already passed on the files it read runs anyway, as a check"
        ),
        "{}",
        second.said()
    );
    // It failed again: the check's reuse is suspended, which the CLI says (e2e_pytest/
    // test_as_unit.rs).
    assert_eq!(r.disagreements(), ["tests/test_target.py"]);
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
    let r = repo(&files);
    let python = python();
    assert_eq!(run(&r, &pytest_command(&python, &[]), &[]).code, 0);
    // A new test file: the check runs again, and test_ok would have been reused.
    r.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let second = run(&r, &pytest_command(&python, &[]), &[]);
    let said = second.line("unit: with `tests: reuse`");
    assert!(
        said.starts_with("unit: with `tests: reuse`, 3 of 4 tests would have run"),
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
    let r = repo(&shop("{mode: reuse, unit: test}"));
    let python = python();
    let xdist = ["-n", "2"];
    let first = run(&r, &pytest_command(&python, &xdist), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}", first.said());
    let second = run(&r, &pytest_command(&python, &xdist), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert_eq!(
        hushrun_testkit::lines::drawn(second.line("unit: ")),
        hushrun_testkit::lines::drawn(
            "unit: 3 of 13 tests run, in 3 of 5 test files; 10 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    assert!(second.stdout.contains("3 passed"), "{}", second.stdout);
    // The check's result: a pass (its code), and its units' counts.
    assert_eq!(
        second.units["tests"],
        json!({"total": 13, "ran": 3, "reused": 10}),
        "{}",
        second.said()
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
    let r = repo(&shop("{mode: reuse, unit: test}"));
    let python = python();
    assert_eq!(
        run(&r, &pytest_command(&python, &[]), &[("FAIL", "1")]).code,
        1
    );
    r.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let second = run(&r, &pytest_command(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert_eq!(
        hushrun_testkit::lines::drawn(second.line("unit: ")),
        hushrun_testkit::lines::drawn(
            "unit: 4 of 6 test files run; 2 already passed on the files they read, and so did 8 tests of the others, and 1 more test that already passed runs anyway, as a check"
        )
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
    let r = repo(&files);
    let ran = run(&r, &pytest_command(&python(), &[]), &[]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.lines.iter().any(|l| l
            == "unit: its tests are reused by test file in this run: it has 50,001 tests, more than the 50,000 this version reuses one by one"),
        "{}",
        ran.said()
    );
    // Counted by test file: the run's units say nothing of tests, and its test file recorded its
    // own pass.
    let units = &ran.units;
    assert!(units.get("tests").is_none(), "{units}");
    assert_eq!(units["ran"], 1, "{units}");
}
