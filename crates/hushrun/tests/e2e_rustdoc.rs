//! \[ADR 0112\] Doctests with `cargo test --doc` (C-74 to C-76), end to end: a unit is a library
//! whose doctests Cargo runs, named by its root file; what it read is what its unit test binary
//! would have read; a library whose doctests already passed on those files isn't handed to
//! rustdoc.
//!
//! These run the `hushrun` binary (`common/cli.rs`): Cargo starts hushrun's rustdoc, which is the
//! binary itself (`units::program`), so a flow that observes doctests reads all of hushrun
//! (CONTRIBUTING.md, "Which tests CI runs again"). What needs no stand-in, the ways of running
//! Cargo's tests hushrun can't read, is in `units_rustdoc.rs`, in-process.
//!
//! The workspaces are tiny and need only Cargo, offline (`common/rustdoc.rs`). hushrun's rustdoc
//! builds with a rustdoc that runs nothing, `true`: Unix only.
#![cfg(unix)]

#[path = "common/cli.rs"]
mod cli;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/rustdoc.rs"]
mod rustdoc;
#[path = "common/store.rs"]
mod store;

use hushrun_testkit::git;

use cli::{Cli as _, Output};
use rustdoc::{library, ran, reads, workspace};

/// `hushrun run doctest -- cargo test --doc …`.
const DOCTEST: &[&str] = &[
    "run",
    "doctest",
    "--",
    "cargo",
    "test",
    "--workspace",
    "--doc",
    "--locked",
    "--offline",
];

/// The plan's line for the check.
fn plan_line(ran: &Output) -> &str {
    ran.stderr
        .lines()
        .find(|l| l.starts_with("hushrun: doctest: ") && l.contains(" run"))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", ran.stderr))
}

// covers: C-74, C-75, C-76
#[test]
fn a_librarys_doctests_that_passed_on_what_they_read_are_not_built_again() {
    let r = rustdoc::repo(&workspace());
    let first = r.run(DOCTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        plan_line(&first),
        "hushrun: doctest: all 5 doctest crates run: none had passed here yet on the files it read"
    );
    for package in ["core", "app", "one", "two", "three"] {
        assert!(ran(&first.stdout, package), "{package}: {}", first.stdout);
    }
    // A library's doctests read its sources, and those of the libraries they may link: its
    // dependencies' and its dev-dependencies'.
    let core = reads(&r, "core/src/lib.rs");
    assert!(core.contains(&"helper/src/lib.rs".to_string()), "{core:?}");
    let app = reads(&r, "app/src/lib.rs");
    assert!(app.contains(&"core/src/lib.rs".to_string()), "{app:?}");
    assert!(!app.contains(&"helper/src/lib.rs".to_string()), "{app:?}");

    // `app` changes: its doctests run; of the four others, one runs as a check and three aren't
    // handed to rustdoc at all.
    r.write(
        "app/src/lib.rs",
        "/// ```\n/// assert_eq!(app::value(), 43);\n/// ```\npub fn value() -> u32 {\n    1 + core::value()\n}\n",
    );
    git::commit_all(&r.repo, "app");
    let second = r.run(DOCTEST, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        plan_line(&second),
        "hushrun: doctest: 2 of 5 doctest crates run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(ran(&second.stdout, "app"), "{}", second.stdout);
    let doctests_run = second
        .stdout
        .lines()
        .filter(|l| l.starts_with("test ") && l.contains("/src/lib.rs - "))
        .count();
    assert_eq!(doctests_run, 2, "{}\n{}", second.stdout, second.stderr);

    // A dev-dependency changes: the doctests that use it run, and those of `app`, which links
    // `core` but not its dev-dependencies, don't for that.
    r.write(
        "helper/src/lib.rs",
        "pub fn half(x: u32) -> u32 {\n    x >> 1\n}\n",
    );
    git::commit_all(&r.repo, "helper");
    let third = r.run(DOCTEST, &[]);
    assert_eq!(third.code, 0, "{}\n{}", third.stdout, third.stderr);
    assert!(ran(&third.stdout, "core"), "{}", third.stdout);
    assert!(
        plan_line(&third)
            .starts_with("hushrun: doctest: 2 of 5 doctest crates run; 3 already passed"),
        "{}",
        plan_line(&third)
    );
}

// covers: C-76
#[test]
fn a_doctest_that_fails_runs_again_until_it_passes() {
    let r = rustdoc::repo(&workspace());
    let first = r.run(DOCTEST, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    r.write(
        "two/src/lib.rs",
        &library("two", 2).replace("{\n    2\n}", "{\n    5\n}"),
    );
    git::commit_all(&r.repo, "break two");
    let failed = r.run(DOCTEST, &[]);
    assert_ne!(failed.code, 0, "{}\n{}", failed.stdout, failed.stderr);
    assert!(ran(&failed.stdout, "two"), "{}", failed.stdout);
    // The same code again: `two`'s doctests didn't pass, so they run again.
    let again = r.run(DOCTEST, &[]);
    assert_ne!(again.code, 0, "{}\n{}", again.stdout, again.stderr);
    assert!(ran(&again.stdout, "two"), "{}", again.stdout);
}
