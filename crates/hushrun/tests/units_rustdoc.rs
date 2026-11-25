//! \[ADR 0112\] Doctests with `cargo test --doc` and Cargo's tests (C-74, C-77), in-process: the
//! ways of running Cargo's tests that hushrun can't read say why, and COMMAND runs as it was.
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

use std::process::Command;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, cargo};

use in_process::{Ran, Repo};
use rustdoc::{DOCTEST, ran, workspace};

/// What Cargo's adapter answers that never changes in a run, for the code every adapter shares.
fn profile(adapter: &cargo::Recognized) -> Profile {
    Profile {
        word: adapter.word(),
        runner: adapter.runner(),
        test_file: adapter.test_file(),
        tests: adapter.tests(),
        hints: adapter.hints(),
        binds: adapter.binds(),
        keys: adapter.keys(),
    }
}

/// `hushrun run CHECK -- ARGS`, with `env` besides, in-process, through Cargo's adapter alone,
/// for a COMMAND hushrun doesn't observe: it panics when the adapter would load, which starts
/// hushrun's own rustdoc or target runner (`e2e_rustdoc.rs`).
fn run(r: &Repo, check: &str, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to Cargo's adapter (`cargo::recognized`), with no
    // trait object: what COMMAND is, and why hushrun can't read it.
    let Some(adapter) = cargo::recognize(hushrun::units::command::without_env(&command)) else {
        panic!("{args:?} doesn't start Cargo's tests");
    };
    if let Some(why) = &adapter.refused {
        return in_process::Run::new(r, check, args, env).unobserved(why);
    }
    // As `units::Session::start` opens the observer and asks the adapter's variables: when they
    // say why it can't load, the run is unobserved, and nothing the observer noted is said.
    let mut run = in_process::Run::new(r, check, args, env);
    let Some(observer) = run.open(profile(&adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    match variables {
        Err(why) => in_process::Run::new(r, check, args, env).unobserved(&why),
        Ok(_) => panic!(
            "{args:?} loads Cargo's adapter, whose run starts hushrun itself: e2e_rustdoc.rs"
        ),
    }
}

// covers: C-74, C-77
#[test]
fn a_rustdoc_of_commands_own_says_why_it_isnt_observed_and_runs_as_it_was() {
    let r = rustdoc::repo(&workspace());
    // A rustdoc of the person's own: hushrun doesn't stand in front of it.
    let rustdoc = String::from_utf8(
        Command::new("rustc")
            .args(["--print", "sysroot"])
            .current_dir(&r.repo)
            .output()
            .expect("rustc")
            .stdout,
    )
    .expect("utf-8");
    let rustdoc = format!("{}/bin/rustdoc", rustdoc.trim());
    let theirs = run(&r, "doctest", DOCTEST, &[("RUSTDOC", &rustdoc)]);
    assert_eq!(theirs.code, 0, "{}", theirs.said());
    theirs.line("doctest: test files aren't observed: COMMAND's environment sets RUSTDOC");
    assert!(ran(&theirs.stdout, "one"), "{}", theirs.stdout);
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
        let ran = run(&r, "unit", args, &[]);
        ran.line(&format!("unit: test files aren't observed: {why}"));
    }
}
