//! \[ADR 0090\] Test programs with CTest (C-30 to C-34, C-39, C-49), in-process: what of CTest's
//! adapter needs none of hushrun's helpers, through its own type, `ctest::Recognized`, and never
//! the `hushrun` binary (`common/in_process.rs`, CONTRIBUTING.md, "Which tests CI runs again").
//!
//! The adapter gets inside CTest through hushrun's `ctest` on `PATH`, which is the `hushrun`
//! binary itself (`units::program`): every run it observes starts that helper, and so does every
//! test of hushrun's `ctest` alone, so they reach the whole binary and are end to end, in
//! `e2e_ctest.rs`. What is here runs no helper: a COMMAND the adapter refuses, which runs as it
//! is. The fixtures are `common/ctest.rs`'s.

#![cfg(unix)]

#[path = "common/ctest.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use hushrun::units::ctest;

use fixtures::*;
use in_process::{Ran, Repo};

/// Variables of CTest's, CMake's and hushrun's helpers' that COMMAND must never inherit, besides
/// `common/repo.rs`'s.
const RUNNER: &[&str] = &[
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "CTEST_OUTPUT_ON_FAILURE",
    "CTEST_PARALLEL_LEVEL",
    "CTEST_NO_TESTS_ACTION",
    "CMAKE_GENERATOR",
    "CMAKE_BUILD_TYPE",
];

/// A repository of `files`, configured in `build` by `cmake` with `generator` and built, whose
/// COMMAND gets that CMake's tools on `PATH`.
fn project(cmake: &Cmake, generator: &str, files: &[(String, String)]) -> Repo {
    let r = Repo::new(files)
        .clear(RUNNER)
        .give("PATH", fixtures::path(cmake));
    let tool = |name: &str| cmake.bin.join(name).to_string_lossy().into_owned();
    let configured = r
        .command(&tool("cmake"))
        .args(configure(cmake, generator, "build"))
        .output()
        .expect("cmake");
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let built = r
        .command(&tool("cmake"))
        .args(["--build", "build"])
        .output()
        .expect("cmake --build");
    assert!(
        built.status.success(),
        "{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    r
}

/// `hushrun run unit -- ARGS`, in-process, for a COMMAND CTest's adapter refuses: COMMAND as it
/// is, and why nothing of it is observed. It panics when the adapter would observe COMMAND,
/// which needs its helper and an end-to-end test.
fn refused(r: &Repo, args: &[&str]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    match ctest::recognize(hushrun::units::command::without_env(&command), &r.repo) {
        Some(Err(why)) => in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        Some(Ok(_)) => panic!("{args:?} is observed, through hushrun's ctest: e2e_ctest.rs"),
        None => panic!("{args:?} isn't CTest's"),
    }
}

// covers: C-49, C-32
#[test]
fn cmakes_test_target_bypasses_path_and_says_so() {
    let Some(cmake) = with_cmake().into_iter().next() else {
        return;
    };
    let generator = generators(&cmake)[0];
    let r = project(&cmake, generator, &shop("observe"));
    let bypassed = refused(&r, &["cmake", "--build", "build", "--target", "test"]);
    assert_eq!(bypassed.code, 0, "{}", bypassed.said());
    assert!(
        bypassed.line("unit: ").starts_with(
            "unit: test files aren't observed: make test, ninja test and cmake --build … --target test start ctest by its full path"
        ),
        "{}",
        bypassed.said()
    );
}
