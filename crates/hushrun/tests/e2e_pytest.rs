//! \[ADR 0068, 0069, 0080, 0083\] pytest's adapter end to end: the `hushrun` binary, as a person or
//! CI runs it, for what only the CLI does with pytest's test files. Every other test of the
//! adapter is in-process, in `units.rs`, `test_as_unit.rs`, `reads_test.rs` and `reuse.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: the run's lines as the
//! CLI says them and their order, its last line and exit code, the check's own result and skip,
//! how the CLI chooses an adapter, a signal, a results server, `hushrun why`, `hushrun ready` and
//! the hook. Each module holds the end-to-end tests of the in-process file it is named after.
//!
//! The fixtures need Python with pytest and pytest-xdist (`common/python.rs`). They say so and
//! pass where those are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/python.rs"]
mod python;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

#[path = "e2e_pytest/observe.rs"]
mod observe;
#[path = "e2e_pytest/reads_test.rs"]
mod reads_test;
#[path = "e2e_pytest/reuse.rs"]
mod reuse;
#[path = "e2e_pytest/test_as_unit.rs"]
mod test_as_unit;
