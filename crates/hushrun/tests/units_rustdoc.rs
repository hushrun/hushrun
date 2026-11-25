//! \[ADR 0112\] Cargo's tests (C-77), in-process: the ways of running Cargo's tests that hushrun
//! can't read say why, and COMMAND runs as it was.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run CHECK -- cargo …` would, through Cargo's adapter's own type, `cargo::Recognized`, and the
//! code every adapter shares, and never starts the `hushrun` binary (`common/in_process.rs`).
//! Cargo itself runs as COMMAND, for real. A run hushrun observes starts hushrun's rustdoc, which
//! is the binary itself, so those flows are end to end: `e2e_rustdoc.rs`.
//!
//! The workspaces are tiny and need only Cargo, offline (`common/rustdoc.rs`). Unix only, as
//! `e2e_rustdoc.rs` is.
#![cfg(unix)]

#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/rustdoc.rs"]
mod rustdoc;
#[path = "common/store.rs"]
mod store;

use hushrun::units::cargo;

use in_process::{Ran, Repo};
use rustdoc::workspace;

/// `hushrun run CHECK -- ARGS`, in-process, through Cargo's adapter alone, for a COMMAND Cargo's
/// adapter refuses to read: it panics when the adapter would load, whose run starts hushrun's own
/// rustdoc or target runner, and so does asking its variables (`StartAt::exe`): those flows are
/// in `e2e_rustdoc.rs`.
fn run(r: &Repo, check: &str, args: &[&str]) -> Ran {
    let env: &[(&str, &str)] = &[];
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to Cargo's adapter (`cargo::recognized`), with no
    // trait object: what COMMAND is, and why hushrun can't read it.
    let Some(adapter) = cargo::recognize(hushrun::units::command::without_env(&command)) else {
        panic!("{args:?} doesn't start Cargo's tests");
    };
    match &adapter.refused {
        Some(why) => in_process::Run::new(r, check, args, env).unobserved(why),
        None => panic!(
            "{args:?} loads Cargo's adapter, whose run starts hushrun itself: e2e_rustdoc.rs"
        ),
    }
}

// covers: C-77
#[test]
fn what_hushrun_cant_read_of_nextests_runs_says_why_and_runs_as_it_was() {
    let r = rustdoc::repo(&workspace());
    // An archive's binaries, and a partition by count: COMMAND runs as it was, whatever it does.
    for (args, why) in [
        (
            &["cargo", "nextest", "run", "--archive-file", "none.tar.zst"][..],
            "cargo nextest runs binaries built elsewhere (--archive-file)",
        ),
        (
            &["cargo", "nextest", "run", "--partition", "count:1/2"][..],
            "cargo nextest splits the tests by their place in the list (--partition count:1/2)",
        ),
    ] {
        r.write(
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/docs\nchecks:\n  unit:\n    tests:\n      mode: reuse\n",
        );
        let ran = run(&r, "unit", args);
        ran.line(&format!("unit: test files aren't observed: {why}"));
    }
}
