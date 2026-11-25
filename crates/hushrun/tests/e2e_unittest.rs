//! \[ADR 0110\] unittest's adapter end to end: the `hushrun` binary, as a person or CI runs it,
//! for what only the CLI does with unittest's test modules. Every other test of the adapter is
//! in-process, in `units_unittest.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: the run's lines as the
//! CLI says them, and `hushrun why`. The fixtures, and the tools they need, are in
//! `common/unittest.rs`.

#![cfg(unix)]

#[path = "common/cli.rs"]
mod cli;
#[path = "common/unittest.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use cli::Cli as _;
use fixtures::*;

// covers: C-68, C-39, C-40, C-37
#[test]
fn hushrun_why_says_which_test_read_a_module_only_it_called() {
    let Some(python) = with_python() else {
        return;
    };
    if !is_312(&python) {
        eprintln!("note: skipping: this Python has no sys.monitoring, so no test is watched");
        return;
    }
    let r = repo(&shop("{mode: reuse, unit: test, reads: test}"));
    let args = ["run", "unit", "--", &python, "-m", "unittest", "-v"];
    let first = r.run(&args, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);

    // A module two imports away that only Split.test_near called changes: Split.test_other
    // didn't read it.
    r.write("app/deep.py", "def h():\n    return 1  # changed\n");
    let why = |test: &str| {
        let unit = format!("tests/test_split.py::{test}");
        r.run(
            &["why", "unit", &unit, "--", &python, "-m", "unittest", "-v"],
            &[],
        )
        .stdout
    };
    let near = why("Split.test_near");
    assert!(
        near.contains("runs: it read app/deep.py, which changed"),
        "{near}"
    );
    let other = why("Split.test_other");
    assert!(other.contains("would be reused"), "{other}");
    // The CLI says the plan's line as the in-process test reads it.
    let changed = r.run(&args, &[]);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert!(
        changed
            .line("hushrun: unit: ")
            .starts_with("hushrun: unit: 3 of 10 tests run, in "),
        "{}",
        changed.stderr
    );
}

// covers: C-69, C-37
#[test]
fn hushrun_why_says_a_test_whose_class_failed_to_end_runs() {
    let Some(python) = with_python() else {
        return;
    };
    let r = repo(&teardown_shop());
    let ran = r.run(&["run", "unit", "--", &python, "-m", "unittest"], &[]);
    assert_ne!(ran.code, 0, "{}", ran.stderr);
    // `hushrun why` says why the test kept no pass.
    let why = r.run(
        &[
            "why",
            "unit",
            "tests/test_teardown.py::Leaks.test_a",
            "--",
            &python,
            "-m",
            "unittest",
        ],
        &[],
    );
    assert!(why.stdout.contains("runs"), "{}", why.stdout);
}
