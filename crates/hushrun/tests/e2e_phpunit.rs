//! \[ADR 0116\] The PHPUnit adapter end to end: the `hushrun` binary, as a person or CI runs it,
//! for what only the CLI does with PHPUnit's test files. Every other test of the adapter is
//! in-process, in `units_phpunit.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: that the CLI gives
//! COMMAND the PHPUnit adapter, the run's lines and last line as the CLI says them, the check's
//! own skip, and `hushrun why`. The tools are those of `units_phpunit.rs`.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/phpunit.rs"]
mod php;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use cli::Cli as _;
use php::*;

const PHPUNIT_RUN: &[&str] = &["run", "unit", "--", "vendor/bin/phpunit"];

// covers: C-34, C-37, C-39, C-91, C-93
#[test]
fn a_run_says_how_many_tests_it_reused_and_hushrun_why_tells_why_a_test_file_runs() {
    for runner in phpunits() {
        let r = repo(&runner, &shop("{mode: reuse, unit: test}"));
        let first = r.run(PHPUNIT_RUN, &[]);
        assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed PHPUnit's test files yet"
        );
        // A test file that no test file reads changes: only its own tests run, and one more that
        // already passed, as a check.
        r.write(
            "tests/Unit/PricingTest.php",
            &test_class(
                "Shop\\Tests\\Unit",
                "PricingTest",
                "    public function testPrice(): void { $this->assertSame(8, \\Shop\\Pricing::price(4)); }",
            ),
        );
        let second = r.run(PHPUNIT_RUN, &[]);
        assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
        assert_eq!(
            second.line("hushrun: unit: 2"),
            "hushrun: unit: 2 of 10 tests run, in 2 of 8 test files; 8 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "{}",
            runner.name()
        );
        assert!(
            hushrun_testkit::lines::says_reused(&second.stderr, 8, "tests"),
            "{}",
            second.stderr
        );
        // \[C-37\] Why a test file runs, from this machine alone.
        r.write(
            "src/Pricing.php",
            &class(
                "Pricing",
                "    public static function price(int $x): int { return 2 * $x; }",
            ),
        );
        let why = r.run(&["why", "unit", "tests/Unit/CartTest.php"], &[]);
        assert_eq!(why.code, 0, "{}", why.stderr);
        assert!(
            why.stdout.starts_with(
                "tests/Unit/CartTest.php runs: it read src/Pricing.php, which changed"
            ),
            "{}",
            why.stdout
        );
        assert!(
            why.stdout
                .contains("PHPUnit's list of test files is the one of its last run here"),
            "{}",
            why.stdout
        );
    }
}

// covers: C-39, C-92
#[test]
fn a_check_whose_failures_were_fixed_is_skipped_once_it_passed() {
    for runner in phpunits() {
        let r = repo(&runner, &flips());
        let first = r.run(PHPUNIT_RUN, &[("FAIL", "1"), ("BREAK", "1")]);
        assert_ne!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
        let second = r.run(PHPUNIT_RUN, &[]);
        assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
        assert_eq!(
            second.line("hushrun: unit: "),
            "hushrun: unit: all 5 tests run; 1 that already passed on the files it read runs anyway, as a check",
            "{}",
            runner.name()
        );
        let again = r.run(PHPUNIT_RUN, &[]);
        assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
    }
}
