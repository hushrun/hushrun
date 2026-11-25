//! `reads_test.rs`'s end-to-end test: the line the CLI says when an audit catches a known miss of
//! `reads: test` and suspends the check's reuse. Every other check of what a test read is one an
//! in-process run shows (`reads_test.rs`).

#[path = "../common/reads_test.rs"]
mod fixtures;

use super::cli::Cli as _;
use super::python::*;
use super::repo::Repo;
use super::store;

use fixtures::*;

/// Whether these tests can run here: pytest, on a Python that watches a test. Below 3.12 there is
/// nothing to watch with, which isn't a missing tool, and these skip.
fn can_watch() -> bool {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return false;
    }
    if !watches() {
        eprintln!("note: skipping: this Python has no sys.monitoring, so pytest watches no test");
        return false;
    }
    true
}

fn pytest_args(python: &str) -> Vec<&str> {
    vec![
        "run",
        "unit",
        "--",
        python,
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
    ]
}

// covers: C-40, C-34
#[test]
fn an_audit_that_catches_a_known_miss_of_reads_test_says_it_suspends_reuse() {
    if !can_watch() {
        return;
    }
    // test_target is the one test the plan could leave out (test_other.py changes too, so it
    // runs): the audit, 2% of one test and at least one, runs it as a check whatever its draw; it
    // fails, runs once more alone, fails again, and the check's reuse is suspended.
    let files = files_of(&a_registry(), "{mode: reuse, unit: test, reads: test}");
    let r = Repo::new(&files).clear(PYTEST_CLEARED);
    let python = python();
    let first = r.run(&pytest_args(&python), &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    r.write(
        "app/plugins.py",
        "from app.registry import HANDLERS\nHANDLERS['x'] = 42\n",
    );
    r.write(
        "tests/test_other.py",
        "def test_other():\n    assert True  # changed\n",
    );
    let caught = r.run(&pytest_args(&python), &[]);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert!(
        caught.stderr.contains(
            "hushrun: unit: tests/test_a.py::test_target failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(
        store::disagreements(&r.cache),
        ["tests/test_a.py::test_target"]
    );
}
