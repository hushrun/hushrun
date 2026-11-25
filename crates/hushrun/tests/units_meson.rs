//! \[ADR 0119\] Test programs with `meson test` (C-102 to C-105), in-process: what of Meson's
//! adapter needs none of hushrun's helpers, through its own type, `mesontest::Recognized`, and
//! never the `hushrun` binary (`common/in_process.rs`, CONTRIBUTING.md, "Which tests CI runs
//! again").
//!
//! The adapter gets inside `meson test` through a program of hushrun's on `PATH`, which is the
//! `hushrun` binary itself (`units::program`): every run it observes starts that helper, so it
//! reaches the whole binary and is end to end, in `e2e_meson.rs`. What is here runs no helper:
//! a COMMAND the adapter refuses, which runs as it is. The fixtures are `common/meson.rs`'s.

#![cfg(unix)]

#[path = "common/meson.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use hushrun::units::mesontest;

use fixtures::{shop, tools};
use in_process::{Ran, Repo};

/// Variables of Meson's and of hushrun's helpers' that COMMAND must never inherit, besides
/// `common/repo.rs`'s.
const RUNNER: &[&str] = &[
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "MESON_TESTTHREADS",
    "MAKEFILES",
    "CC",
    "CXX",
];

/// A repository of `files`, set up by Meson in `build`, whose COMMAND gets `bin` on `PATH`.
fn project(bin: &[std::path::PathBuf], files: &[(&str, String)]) -> Repo {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.clone()))
        .collect();
    let r = Repo::new(&files)
        .clear(RUNNER)
        .give("PATH", fixtures::path(bin));
    let out = r
        .command("meson")
        .args(["setup", "build"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    r
}

/// `hushrun run unit -- ARGS`, in-process, for a COMMAND Meson's adapter refuses: COMMAND as
/// it is, and why nothing of it is observed. It panics when the adapter would observe COMMAND,
/// which needs its helper and an end-to-end test.
fn refused(r: &Repo, args: &[&str]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    match mesontest::recognize(hushrun::units::command::without_env(&command), &r.repo) {
        Some(Err(why)) => in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        Some(Ok(_)) => panic!("{args:?} is observed, through hushrun's meson: e2e_meson.rs"),
        None => panic!("{args:?} isn't `meson test`"),
    }
}

// covers: C-102
#[test]
fn meson_test_started_by_its_path_says_why_nothing_is_reused() {
    let Some(bin) = tools() else { return };
    let r = project(&bin, &shop(""));
    let meson = bin[0].join("meson");
    let ran = refused(&r, &[meson.to_str().unwrap(), "test", "-C", "build"]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.line("unit: test files aren't observed: ")
            .contains("by its path"),
        "{}",
        ran.said()
    );
}
