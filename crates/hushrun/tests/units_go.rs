//! \[ADR 0070\] Test files with `go test` (C-32), in-process: what of Go's adapter needs no runner.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): a test runs a check as `hushrun run
//! unit -- go test …` would, through Go's adapter's own code (`gotest`) and the code every adapter
//! shares, and never starts the `hushrun` binary (`common/in_process.rs`). Go itself runs as
//! COMMAND, for real.
//!
//! Go's adapter puts hushrun itself in front of every test binary `go test` starts, as its
//! `-exec` runner (`gotest::Recognized::variables` names this hushrun, `StartAt::exe`). So every
//! run that observes `go test` runs the whole binary, and is end to end, in `e2e_go.rs`, as are the
//! CLI's lines and `hushrun why`. Here: a COMMAND Go's adapter won't load for, which runs as it is,
//! with no runner.
//!
//! The fixtures (`common/go.rs`) use the standard library only, and need Go. They say so and pass
//! where it is missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/go.rs"]
mod go_fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use hushrun::units::gotest;

use go_fixtures::*;
use in_process::{Ran, Repo};

/// `hushrun run unit -- ARGS`, with `env` besides, in-process, for a COMMAND Go's adapter takes
/// and won't load for: as `units::adapter::choose_kept` comes to Go's adapter
/// (`gotest::recognized`), with no trait object, and COMMAND run as it is. A COMMAND the adapter
/// loads for needs its runner, the hushrun binary: `e2e_go.rs`.
fn refused(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    match gotest::recognize(hushrun::units::command::without_env(&command)) {
        Some(Err(why)) => in_process::Run::new(r, "unit", args, env).unobserved(&why),
        Some(Ok(_)) => panic!("{args:?} loads Go's adapter, whose runner is hushrun: e2e_go.rs"),
        None => panic!("{args:?} doesn't start go test"),
    }
}

// covers: C-32
#[test]
fn go_test_in_a_form_hushrun_doesnt_read_is_left_alone_and_says_why() {
    if !tools("Go", has_go()) {
        return;
    }
    let r = repo(&shop("observe"));
    let ran = refused(&r, &["go", "test", "-exec", "env", "./..."], &[]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert_eq!(
        ran.line("unit: test files aren't observed: "),
        "unit: test files aren't observed: COMMAND starts test binaries through a program of its own (-exec)"
    );
    // That nothing of hushrun's reached COMMAND then, and that GOFLAGS keeps what it said, is the
    // CLI's: e2e_go.rs.
}
