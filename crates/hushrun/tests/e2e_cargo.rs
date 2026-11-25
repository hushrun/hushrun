//! \[ADR 0068, 0069, 0070\] Cargo's adapter end to end, and Go's beside it: the `hushrun` binary,
//! as a person or CI runs it. Every start of a test binary goes through the hushrun binary
//! itself, Cargo's as its target runner and Go's as `go test`'s `-exec` (`GOFLAGS`), so each of
//! these flows reaches all of it and stays end to end (CONTRIBUTING.md, "Which tests CI runs
//! again"). Each module holds the Cargo and Go tests of the file it is named after, which were
//! there before the pytest tests beside them moved in-process.
//!
//! The fixtures need cargo-nextest, and Go for Go's. They say so and pass where those are
//! missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

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
