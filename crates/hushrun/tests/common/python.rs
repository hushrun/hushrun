//! The Python the pytest tests run, shared by the in-process tests (`common/pytest.rs`) and the
//! end-to-end ones: `HUSHRUN_TEST_PYTHON` names it, `python3` by default, with pytest and
//! pytest-xdist. A test says so and passes where they are missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets: then a missing tool fails it.

#![allow(dead_code)]

/// Variables a pytest test's COMMAND never inherits besides [`super::repo::CLEARED`]: what would
/// change how pytest starts, and where Cargo builds.
pub const PYTEST_CLEARED: &[&str] = &["PYTEST_ADDOPTS", "PYTHONPATH", "CARGO_TARGET_DIR"];

/// Whether a tool the test needs is `here`: it panics where it is missing and
/// `HUSHRUN_TEST_TOOLS=required`, and else says so, which the test returns on.
pub fn tools(what: &str, here: bool) -> bool {
    if here {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

/// The Python the tests run.
pub fn python() -> String {
    std::env::var("HUSHRUN_TEST_PYTHON").unwrap_or_else(|_| "python3".into())
}

/// Whether `python -c CHECK` passes.
pub fn runs(python: &str, check: &str) -> bool {
    hushrun_testkit::tool::command(python)
        .args(["-c", check])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether the Python has pytest and pytest-xdist.
pub fn has_pytest() -> bool {
    runs(
        &python(),
        "import sys, pytest, xdist; sys.exit(0 if sys.version_info >= (3, 8) else 1)",
    )
}

/// Whether the Python the tests run has `sys.monitoring` (3.12 and later), with which pytest's
/// adapter watches each test (ADR 0083).
pub fn watches() -> bool {
    runs(
        &python(),
        "import sys; sys.exit(0 if hasattr(sys, 'monitoring') else 1)",
    )
}
