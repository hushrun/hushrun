//! \[ADR 0109\] Svelte with Vitest end to end: the `hushrun` binary, as a person or CI runs it, for
//! what only the CLI does: `hushrun why` of a test. Every other test is in-process, in
//! `units_svelte.rs`, whose tools these are.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/svelte.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use cli::{Cli as _, Output};
use fixtures::*;

const VITEST_RUN: &[&str] = &["run", "unit", "--", "node_modules/.bin/vitest", "run"];

/// Whether one of the lines hushrun wrote is `line`, but for the `, saving about …` that
/// `observe`'s line gains when what it would have reused took a second or more, which is the run's
/// timing.
fn says(ran: &Output, line: &str) -> bool {
    ran.stderr
        .lines()
        .any(|l| hushrun_testkit::lines::without_saving(l) == line)
}

// covers: C-62, C-63
#[test]
fn hushrun_why_says_a_svelte_test_reads_the_component_it_rendered() {
    if !tools() {
        return;
    }
    let r = repo(&app("{mode: observe, unit: test, reads: test}"));
    let first = r.run(VITEST_RUN, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // Label is two imports away from the test file, and only Counter's render runs it: `hello`,
    // in the same file, ran no function of it (C-40 still gives every test of a file what the
    // file imports directly).
    r.write(
        "src/lib/Label.svelte",
        "<script>\n  let { text } = $props()\n</script>\n<span>{text}?</span>\n",
    );
    let second = r.run(VITEST_RUN, &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        says(
            &second,
            "hushrun: unit: with `tests: reuse`, 2 of 5 tests would have run, in 2 of 4 test files; 3 already passed on the files they read"
        ),
        "{}",
        second.stderr
    );
    let why = r.run(&["why", "unit", "src/both.test.js::hello"], &[]);
    assert!(
        why.stdout
            .starts_with("src/both.test.js::hello would be reused with `tests: reuse`"),
        "{}\n{}",
        why.stdout,
        why.stderr
    );
}
