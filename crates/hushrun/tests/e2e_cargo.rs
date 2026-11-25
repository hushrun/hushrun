//! \[ADR 0068, 0069\] Cargo's adapter end to end: the `hushrun` binary, as a person or CI runs it.
//! Every start of a test binary goes through the target runner, which is the hushrun binary
//! itself, so each of these flows reaches all of it and stays end to end (CONTRIBUTING.md, "Which
//! tests CI runs again"). Each module holds the Cargo tests of the file it is named after, which
//! were there before the pytest tests beside them moved in-process.
//!
//! The fixtures need cargo-nextest. They say so and pass where it is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/python.rs"]
mod python;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

#[path = "e2e_cargo/observe.rs"]
mod observe;
#[path = "e2e_cargo/reuse.rs"]
mod reuse;
