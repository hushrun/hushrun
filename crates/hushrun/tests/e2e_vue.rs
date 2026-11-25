//! \[ADR 0108\] Vue's components under Vitest end to end: the `hushrun` binary, as a person or CI
//! runs it, for what only the CLI does: the lines it writes and a run's last line. Every other test
//! is in-process, in `units_vue.rs`, whose tools these are.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/vue.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use cli::Cli as _;
use fixtures::*;

const VITEST_RUN: &[&str] = &["run", "unit", "--", "node_modules/.bin/vitest", "run"];

// covers: C-60
#[test]
fn a_run_with_vue_says_what_it_reused_in_its_last_line() {
    let Some(modules) = tools(
        "Node and Vitest with Vue (HUSHRUN_TEST_VUE)",
        vitest_modules(),
    ) else {
        return;
    };
    let r = vitest_shop("reuse", &modules);
    r.run(VITEST_RUN, &[]);
    r.write(
        "src/Plain.vue",
        "<template>\n  <p>static words</p>\n</template>\n",
    );
    let ran = r.run(VITEST_RUN, &[]);
    assert_eq!(ran.code, 1, "{}\n{}", ran.stdout, ran.stderr);
    // The changed component's test file runs, and fails; one that passed runs as a check (C-34).
    let plan = ran
        .stderr
        .lines()
        .find(|l| l.contains("test files run"))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", ran.stderr));
    assert_eq!(
        plan,
        "hushrun: unit: 2 of 5 test files run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(ran.stdout.contains("test/plain.test.ts"), "{}", ran.stdout);
    assert!(
        hushrun_testkit::lines::says_reused(&ran.stderr, 3, "test files"),
        "{}",
        ran.stderr
    );
}
