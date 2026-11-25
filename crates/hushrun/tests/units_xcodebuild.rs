//! \[ADR 0121\] Test targets and tests with `xcodebuild test` (C-110 to C-113), in-process: what
//! of the adapter needs none of hushrun's helpers, through its own type,
//! `xcodebuild::Recognized`, and never the `hushrun` binary (`common/in_process.rs`,
//! CONTRIBUTING.md, "Which tests CI runs again").
//!
//! The adapter gets inside `xcodebuild` through hushrun's `xcodebuild` on `PATH`, which is the
//! `hushrun` binary itself (`units::program`): every run it observes starts that helper, so it
//! reaches the whole binary and is end to end, in `e2e_xcodebuild.rs`. What is here runs no
//! helper: a COMMAND the adapter refuses, which runs as it is. The fixtures are
//! `common/xcodebuild.rs`'s, and need macOS with Xcode 16 or later.

#[path = "common/xcodebuild.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::Path;

use hushrun::units::xcodebuild as adapter;

use fixtures::*;
use in_process::{Ran, Repo};

/// Variables of hushrun's helpers' and of the fixture's that COMMAND must never inherit, besides
/// `common/repo.rs`'s.
const RUNNER: &[&str] = &[
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "FAIL",
    "HIDDEN",
    "CRASH",
    "NESTED_FAIL",
];

/// A repository of `files`, whose COMMAND gets `xcodebuild` first on `PATH`.
fn repository(xcodebuild: &Path, files: &[(String, String)]) -> Repo {
    let r = Repo::new(files)
        .clear(RUNNER)
        .give("PATH", fixtures::path(xcodebuild));
    std::fs::create_dir_all(r.root.join("home")).expect("home");
    r
}

/// `hushrun run unit -- ARGS`, in-process, for a COMMAND the adapter refuses: COMMAND as it is,
/// and why nothing of it is observed. It panics when the adapter would observe COMMAND, which
/// needs its helper and an end-to-end test.
fn refused(r: &Repo, args: &[&str]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    match adapter::recognize(hushrun::units::command::without_env(&command), &r.repo) {
        Some(Err(why)) => in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        Some(Ok(_)) => {
            panic!("{args:?} is observed, through hushrun's xcodebuild: e2e_xcodebuild.rs")
        }
        None => panic!("{args:?} isn't `xcodebuild test`"),
    }
}

// covers: C-110
#[test]
fn xcrun_xcodebuild_is_refused_and_everything_runs() {
    let Some(xcodebuild) = xcodebuild() else {
        // xcodebuild is macOS's alone: elsewhere there is nothing to miss.
        tools("Xcode 16", !cfg!(target_os = "macos"));
        return;
    };
    let r = repository(&xcodebuild, &project("{mode: reuse}"));
    let mut args = test_command(&r.root.join("dd"));
    args.insert(0, "xcrun".into());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = refused(&r, &args);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.line("unit: test files aren't observed: ")
            .contains("xcrun xcodebuild"),
        "{}",
        ran.said()
    );
}
