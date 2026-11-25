//! \[ADR 0089\] Test targets and tests with `swift test` (C-48), in-process: what of Swift's
//! adapter needs none of hushrun's helpers, through its own type, `swift::Swift`, and never the
//! `hushrun` binary (`common/in_process.rs`, CONTRIBUTING.md, "Which tests CI runs again").
//!
//! The adapter gets inside `swift test` through hushrun's `swift` on `PATH`, which is the
//! `hushrun` binary itself (`units::program`): every run it observes starts that helper, so it
//! reaches the whole binary and is end to end, in `e2e_swift.rs`. What is here runs no helper: a
//! COMMAND the adapter refuses, which runs as it is. The fixtures are `common/swift.rs`'s, and
//! need Swift 6.

#[path = "common/swift.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::Path;

use hushrun::units::swift as adapter;

use fixtures::*;
use in_process::{Ran, Repo};

/// Variables of hushrun's helpers' and of the fixtures' that COMMAND must never inherit, besides
/// `common/repo.rs`'s.
const RUNNER: &[&str] = &[
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "FAIL",
    "HIDDEN",
    "CRASH",
    "NESTED_FAIL",
];

/// A repository of `files`, whose COMMAND gets `swift` first on `PATH`.
fn repository(swift: &Path, files: &[(String, String)]) -> Repo {
    let r = Repo::new(files)
        .clear(RUNNER)
        .give("PATH", fixtures::path(swift));
    std::fs::create_dir_all(r.root.join("home")).expect("home");
    r
}

/// `hushrun run unit -- ARGS`, in-process, for a COMMAND Swift's adapter refuses: COMMAND as it
/// is, and why nothing of it is observed. It panics when the adapter would observe COMMAND,
/// which needs its helper and an end-to-end test.
fn refused(r: &Repo, args: &[&str]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    match adapter::recognize(hushrun::units::command::without_env(&command), &r.repo) {
        Some(Err(why)) => in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        Some(Ok(_)) => panic!("{args:?} is observed, through hushrun's swift: e2e_swift.rs"),
        None => panic!("{args:?} isn't `swift test`"),
    }
}

// covers: C-48
#[test]
fn xcrun_swift_test_is_refused_and_runs_as_it_is() {
    let Some(swift) = swift() else {
        tools("Swift 6", false);
        return;
    };
    // xcrun is macOS's alone.
    if !cfg!(target_os = "macos") {
        return;
    }
    let r = repository(&swift, &shop("{mode: reuse, ignore: ['notes/**']}"));
    let xcrun = refused(&r, &["xcrun", "swift", "test", "--skip-build"]);
    assert_eq!(
        xcrun.line("unit: test files aren't observed: "),
        "unit: test files aren't observed: xcrun starts swift from the selected toolchain, never from PATH, where hushrun's swift is",
        "{}",
        xcrun.said()
    );
}
