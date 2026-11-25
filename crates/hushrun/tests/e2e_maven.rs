//! \[ADR 0088\] Maven's adapter end to end: the `hushrun` binary, as a person or CI runs it, for
//! what only the CLI does with Maven's test classes. Every other test of the adapter is
//! in-process, in `units_maven.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: the run's lines as
//! the CLI says them, its last line and exit code, the check's own result, the audit's second
//! start of COMMAND and its suspension, and `hushrun why`. The tools are those of
//! `units_maven.rs`, and CI's fill of Maven's repository covers these reactors too.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/maven.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use serde_json::json;

use cli::{Cli as _, Output};
use fixtures::*;

/// `hushrun run unit -- mvn … ARGS`, with `env` besides: the tests' Maven, offline
/// ([`Reactor::mvn`]).
fn run(r: &Reactor, args: &[&str], env: &[(&str, &str)]) -> Output {
    let words = r.mvn(args);
    let mut cli = vec!["run", "unit", "--"];
    cli.extend(words.iter().map(String::as_str));
    r.run(&cli, env)
}

/// What `hushrun why unit UNIT` says.
fn why(r: &Reactor, unit: &str) -> String {
    r.run(&["why", "unit", unit], &[]).stdout
}

// covers: C-30, C-34, C-37, C-47
#[test]
fn a_run_says_what_it_reuses_records_it_and_hushrun_why_tells_a_class_by_its_file() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&shop("reuse"), maven);
    let first = run(&r, &["test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    assert_eq!(
        r.newest_result()["units"],
        json!({"total": 6, "ran": 6, "reused": 0})
    );

    // A change in `other` runs its classes, never lib's or app's, but the one of those three
    // the audit runs.
    r.write(
        "other/src/main/java/shop/other/Greet.java",
        "package shop.other;\n\npublic class Greet {\n  public static String hello() { return \"h\" + \"i\"; }\n}\n",
    );
    let other = run(&r, &["test"], &[]);
    assert_eq!(other.code, 0, "{}\n{}", other.stderr, other.stdout);
    assert_eq!(
        other.line("hushrun: unit: "),
        "hushrun: unit: 4 of 6 test classes run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    let units = r.newest_result()["units"].clone();
    assert_eq!(
        units,
        json!({"total": 6, "ran": 4, "reused": 2, "savedMs": units["savedMs"]})
    );
    // \[C-37\] `hushrun why` knows a class by the name the store keeps, the reused ones too.
    let said = why(&r, PRICE_TEST);
    assert!(said.contains("PriceTest.java would be reused"), "{said}");
}

// covers: C-34, C-47
#[test]
fn an_audited_class_that_fails_again_alone_suspends_the_checks_reuse() {
    let Some(maven) = maven() else { return };
    let mut files = shop("reuse");
    // Every class fails when asked, as a missed dependency would: whichever the audit runs fails.
    for (path, text) in files.iter_mut() {
        if path.ends_with("Test.java") && !path.ends_with("PriceTest.java") {
            *text = text.replacen(
                "  @Test void ",
                &format!("  @Test void failsWhenAsked() {{ assertNull(System.getenv(\"{FAIL}\")); }}\n  @Test void "),
                1,
            );
        }
    }
    let r = Reactor::new(&files, maven);
    // other's GreetTest fails, so the check didn't pass; the five others did.
    let first = run(&r, &["test"], &[(OTHER, "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    let caught = run(&r, &["test"], &[(FAIL, "1")]);
    assert_ne!(caught.code, 0, "{}\n{}", caught.stderr, caught.stdout);
    assert!(
        caught.stderr.contains(
            "failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(r.disagreements().len(), 1);
    let after = run(&r, &["test"], &[]);
    assert!(
        after
            .stderr
            .contains("hushrun: unit: every test class runs: reuse of its test files is suspended"),
        "{}",
        after.stderr
    );
}

// covers: C-32, C-47
#[test]
fn the_last_line_counts_mavens_tests_and_never_testngs_copies_of_its_reports() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&testng("reuse", SUITE_SETUP), maven);
    // The suite's setup fails: TestNG skips every test, OrderTest's failing one among them.
    let first = run(&r, &["test"], &[("SHOP_NO_DB", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stderr, first.stdout);
    // The run's counts are Maven's, `Tests run: 12, Failures: 1, Skipped: 11`: TestNG's own
    // copies of the reports below `junitreports/` aren't counted again.
    assert!(
        first.stderr.contains("): 12 tests, 1 failed, 11 skipped"),
        "{}",
        first.stderr
    );
}

// covers: C-37, C-39, C-47
#[test]
fn hushrun_why_never_says_a_suite_class_would_be_reused() {
    let Some(maven) = maven() else { return };
    let r = Reactor::new(&suites("{mode: reuse, unit: test}"), maven);
    let first = run(&r, &["test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    // A change in `other`: core's plain classes are reused, the suite class runs, as the hook
    // can't leave it out, and `hushrun why` says so.
    r.write(
        "other/src/test/java/shop/other/CalcTest.java",
        "package shop.other;\n\nimport org.junit.Test;\nimport static org.junit.Assert.*;\n\npublic class CalcTest {\n  @Test public void adds() { assertEquals(3, 2 + 1); }\n}\n",
    );
    let second = run(&r, &["test"], &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stderr, second.stdout);
    let said = why(&r, SUITE_TEST);
    assert!(!said.contains("would be reused"), "{said}");
    assert!(said.contains("can't leave it out"), "{said}");
}

// covers: C-30, C-37, C-47
#[test]
fn hushrun_why_reuses_a_test_class_of_an_execution_a_failed_run_didnt_reach() {
    let Some(maven) = maven() else { return };
    let files = failsafe("{mode: reuse, ignore: ['notes/**']}");
    let r = Reactor::new(&files, maven);
    let first = run(&r, &["verify"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stderr, first.stdout);
    // lib changes and its PriceTest fails: Surefire stops the build, and Failsafe never runs
    // PriceIT. It is a test class all the same, and keeps the pass it had.
    let price_test = files
        .iter()
        .find(|(path, _)| path == PRICE_TEST)
        .map(|(_, text)| text.clone())
        .unwrap();
    r.write(PRICE_TEST, &format!("{price_test}// changed\n"));
    let failed = run(&r, &["verify"], &[(FAIL, "1")]);
    assert_eq!(failed.code, 1, "{}\n{}", failed.stderr, failed.stdout);
    // Back at the files that passed, before the run: what passed on them would be reused.
    r.write(PRICE_TEST, &price_test);
    r.write("notes/n.txt", "2\n");
    for unit in [PRICE_IT, ROUND_TEST, CART_TEST] {
        let said = why(&r, unit);
        assert!(said.contains("would be reused"), "{unit}: {said}");
    }
}
