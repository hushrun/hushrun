//! \[ADR 0116\] Test files and tests with PHPUnit and Pest (C-30 to C-34, C-39, C-90 to C-93): a
//! unit is a test file of PHPUnit's suite, what it read is what the process had included and
//! opened when its last test ended, but other test files it doesn't name; each test is told on
//! its own and left out of the suite PHPUnit runs; and reuse never misses a failure.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- vendor/bin/phpunit` would, through the PHPUnit adapter's own type,
//! `phpunit::Recognized`, and the code every adapter shares, and never starts the `hushrun`
//! binary (`common/in_process.rs`). PHP itself runs as COMMAND, for real. What only the CLI shows
//! of it (the run's last line, the check's skip, `hushrun why`) is in `e2e_phpunit.rs`.
//!
//! The fixtures need PHP 8.3 and Composer's `vendor` of each runner, installed outside the
//! repository: PHPUnit 10, 11 and 12 in the directories `HUSHRUN_TEST_PHPUNIT10`,
//! `HUSHRUN_TEST_PHPUNIT11` and `HUSHRUN_TEST_PHPUNIT12` name, and Pest 3 and 4 in those
//! `HUSHRUN_TEST_PEST3` and `HUSHRUN_TEST_PEST4` name (CI: `composer require phpunit/phpunit:<v>`
//! in a directory of its own, then its `vendor`). Each test runs on every runner it finds, says so
//! and passes where all are missing, unless `HUSHRUN_TEST_TOOLS=required` (and `HUSHRUN_TEST_PHP`
//! isn't `skip`), which needs them all.

#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/phpunit.rs"]
mod php;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;
use std::process::Command;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, phpunit};
use serde_json::json;

use in_process::{Ran, Repo};
use php::*;

/// `vendor/bin/phpunit`, the COMMAND most tests run.
const PHPUNIT: &[&str] = &["vendor/bin/phpunit"];

/// What the PHPUnit adapter answers that never changes in a run, for the code every adapter
/// shares.
fn profile(adapter: &phpunit::Recognized) -> Profile {
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

/// `hushrun run unit -- ARGS`, with `env` besides, in-process, through the PHPUnit adapter alone.
fn run(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to the PHPUnit adapter (`phpunit::recognized`), with
    // no trait object: what COMMAND is, and whether the PHPUnit or Pest installed is one it knows.
    let adapter = match phpunit::recognize(hushrun::units::command::without_env(&command), &r.repo)
    {
        Some(Ok(adapter)) => adapter,
        Some(Err(why)) => return in_process::Run::new(r, "unit", args, env).unobserved(&why),
        None => panic!("{args:?} starts neither PHPUnit nor Pest"),
    };
    if let Some(why) = phpunit::unloadable(&adapter, &r.repo) {
        return in_process::Run::new(r, "unit", args, env).unobserved(&why);
    }
    observe(r, args, env, &adapter)
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order. Every call is on `phpunit::Recognized` itself, which the static map resolves to its
/// own `impl Adapter`; through a trait object or a generic it would reach every adapter
/// (CONTRIBUTING.md, "Which tests CI runs again"). Another adapter's test file copies this
/// function and changes the type.
fn observe(r: &Repo, args: &[&str], env: &[(&str, &str)], adapter: &phpunit::Recognized) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // PHPUnit's adapter keeps the trait's `plan`, which is its `plan_from_files`.
            Ok(context) => {
                let planning = adapter.plan_from_files(&observer.plan_at(&input, &context));
                observer.planned(&input, context, planning)
            }
            Err(planned) => planned,
        };
        drop(input);
        run.planned(&observer, planned);
    }
    // COMMAND, once, or \[C-32\] once more with nothing left out when the adapter left out what
    // the plan didn't list.
    let mut starts = 0;
    let (code, mut observed) = loop {
        starts += 1;
        let start = adapter.start_variables(observer.run_dir());
        let (code, passed) = run.start_command(&observer, &start);
        let end = run.finish_input(passed);
        let observed = match observer.read_context(&end) {
            Ok(context) => {
                let read = adapter.read(&observer.read_at(&end, &context));
                observer.finished(&end, context, read)
            }
            Err(observed) => observed.map(|observed| *observed),
        };
        drop(end);
        if starts == 1 && observed.as_ref().is_some_and(|o| o.left_out.is_some()) {
            let extras = adapter.again(observer.root(), observer.cwd(), &BTreeSet::new());
            if observer.whole_again(extras).is_ok() {
                continue;
            }
        }
        break (code, observed);
    };
    // \[C-34\] An audited unit that failed runs once more, alone.
    if let Some(o) = observed.as_mut().filter(|o| !o.again.is_empty()) {
        let wanted: BTreeSet<String> = o.again.keys().cloned().collect();
        let extras = adapter.again(observer.root(), o.cwd(), o.unit_files());
        let ended = match observer.again(o, &wanted, extras) {
            Some(again) => {
                let start = adapter.start_variables(again.dir());
                let again = again.with_start(start);
                let first = adapter.start_variables(observer.run_dir());
                let passed = run.start_again(&observer, &first, &again);
                let files = again.files();
                let map = in_process::no_map();
                let read = adapter
                    .read(&observer.again_read_at(&files, o, &run.tree, passed, &map))
                    .ok()
                    .flatten();
                observer.again_ended(&again, o, passed, read)
            }
            None => in_process::Run::not_again(&wanted),
        };
        run.disagreements(o, ended);
    }
    run.record(code, observed)
}

// covers: C-30, C-31, C-90, C-92
#[test]
fn phpunit_test_files_are_observed_from_what_php_included_and_opened() {
    for runner in phpunits() {
        let r = repo(&runner, &shop("observe"));
        let first = run(&r, PHPUNIT, &[]);
        assert_eq!(first.code, 0, "{}: {}", runner.name(), first.said());
        assert_eq!(
            first.line("unit: with"),
            "unit: with `tests: reuse`, all 8 test files would have run: none had passed here yet on the files it read",
            "{}",
            runner.name()
        );
        // What the process had included when its last test ended: the bootstrap, which every
        // test file reads, and what the greeting's test files, which PHPUnit ran first, loaded;
        // never another test file, which PHPUnit included before any test ran.
        assert_eq!(
            reads(&r, "tests/Unit/CartTest.php"),
            [
                "src/Cart.php",
                "src/Greet.php",
                "src/Pricing.php",
                "tests/Unit/CartTest.php",
                "tests/bootstrap.php"
            ],
            "{}",
            runner.name()
        );
        // A PHP file opened as data, not included, is read too.
        assert!(
            reads(&r, "tests/Unit/TemplateTest.php").contains(&"src/template.php".to_string()),
            "{}: {:?}",
            runner.name(),
            reads(&r, "tests/Unit/TemplateTest.php")
        );
        let map = r.map();
        assert_eq!(map["unitFiles"].as_array().unwrap().len(), 8, "{map:#}");
        // One source changes: what read it would run, and the rest would be reused. The cart is
        // loaded first by CartTest, which PHPUnit runs after the greetings and before the other
        // two of tests/Unit: those three would run, and no greeting.
        r.write(
            "src/Cart.php",
            &class(
                "Cart",
                "    public static function total(array $xs): int { return (int) array_sum(array_map([Pricing::class, 'price'], $xs)); }",
            ),
        );
        let changed = run(&r, PHPUNIT, &[]);
        assert_eq!(changed.code, 0, "{}", changed.said());
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("unit: with")).starts_with(
                "unit: with `tests: reuse`, 3 of 8 test files would have run; 5 already passed on the files they read"
            ),
            "{}: {}",
            runner.name(),
            changed.said()
        );
    }
}

// covers: C-34, C-39, C-91, C-93
#[test]
fn phpunit_reuses_test_files_and_tests_that_already_passed() {
    for runner in phpunits() {
        let r = repo(&runner, &shop("{mode: reuse, unit: test}"));
        let first = run(&r, PHPUNIT, &[]);
        assert_eq!(first.code, 0, "{}", first.said());
        assert_eq!(
            first.line("unit: every"),
            "unit: every test file runs: no run here has listed PHPUnit's test files yet"
        );
        assert_eq!(
            tests_of(&r, "tests/Unit/CartTest.php"),
            json!({
                "Shop\\Tests\\Unit\\CartTest::testTotal": "passed",
                "Shop\\Tests\\Unit\\CartTest::testSums with data set \"one\"": "passed",
                "Shop\\Tests\\Unit\\CartTest::testSums with data set #0": "passed"
            }),
            "{}",
            runner.name()
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
        let second = run(&r, PHPUNIT, &[]);
        assert_eq!(second.code, 0, "{}", second.said());
        assert_eq!(
            second.line("unit: 2"),
            "unit: 2 of 10 tests run, in 2 of 8 test files; 8 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "{}",
            runner.name()
        );
        // PHPUnit ran what was left.
        assert!(
            second.stdout.contains("OK (2 tests, 2 assertions)"),
            "{}: {}",
            runner.name(),
            second.said()
        );
        // That the run's last line says how many tests it reused is the CLI's: e2e_phpunit.rs.
        // Everything already passed but the audited test: PHPUnit 12, which fails a run of no
        // test, passes, as the run of those tests did.
        let again = run(&r, &["vendor/bin/phpunit", "--testsuite", "all"], &[]);
        assert_eq!(again.code, 0, "{}", again.said());
        // \[C-37\] Why a test file runs, from this machine alone, is `hushrun why`'s: e2e_phpunit.rs.
    }
}

// covers: C-39, C-92
#[test]
fn a_failure_is_never_reused_and_a_failing_class_hook_fails_its_tests() {
    for runner in phpunits() {
        let r = repo(&runner, &flips());
        let first = run(&r, PHPUNIT, &[("FAIL", "1"), ("BREAK", "1")]);
        assert_ne!(first.code, 0, "{}", first.said());
        assert_eq!(
            tests_of(&r, "tests/FlipTest.php"),
            json!({
                "Shop\\Tests\\FlipTest::testOne": "passed",
                "Shop\\Tests\\FlipTest::testFlips": "failed",
                "Shop\\Tests\\FlipTest::testSkipped": "not-run"
            }),
            "{}",
            runner.name()
        );
        // The class's teardown failed: every test of it failed with it.
        assert_eq!(
            tests_of(&r, "tests/HookTest.php"),
            json!({"Shop\\Tests\\HookTest::testA": "failed", "Shop\\Tests\\HookTest::testB": "failed"}),
            "{}",
            runner.name()
        );
        // Fixed: what failed runs, and what didn't record a pass; the one test that passed may
        // be reused or audited.
        let second = run(&r, PHPUNIT, &[]);
        assert_eq!(second.code, 0, "{}", second.said());
        assert_eq!(
            second.line("unit: "),
            "unit: all 5 tests run; 1 that already passed on the files it read runs anyway, as a check",
            "{}",
            runner.name()
        );
        // That the next run skips the check, which passed on this code, is the CLI's:
        // e2e_phpunit.rs.
    }
}

// covers: C-39, C-90, C-91, C-93
#[test]
fn pest_is_observed_and_reused_through_composer_test() {
    if !Command::new("composer")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        required("Composer", 0, 1);
        return;
    }
    for runner in pests() {
        let r = repo(&runner, &pest_shop());
        let composer = &["composer", "test"];
        let first = run(&r, composer, &[]);
        assert_eq!(first.code, 0, "{}: {}", runner.name(), first.said());
        assert_eq!(
            first.line("unit: every"),
            "unit: every test file runs: no run here has listed Pest's test files yet"
        );
        // Pest ran GreetTest first, so what PricingTest read is GreetTest's and its own.
        assert_eq!(
            reads(&r, "tests/Unit/PricingTest.php"),
            [
                "src/Greet.php",
                "src/Pricing.php",
                "tests/Pest.php",
                "tests/Unit/PricingTest.php",
                "tests/bootstrap.php"
            ],
            "{}",
            runner.name()
        );
        // A test's id is Pest's name of it, without its class, which Pest makes from the path.
        assert_eq!(
            tests_of(&r, "tests/Unit/PricingTest.php"),
            json!({
                "it_prices": "passed",
                "sums with data set \"dataset \"one\"\"": "passed",
                "sums with data set \"(2, 4)\"": "passed"
            }),
            "{}",
            runner.name()
        );
        assert_eq!(
            reads(&r, "tests/Unit/GreetTest.php"),
            [
                "src/Greet.php",
                "tests/Pest.php",
                "tests/Unit/GreetTest.php",
                "tests/bootstrap.php"
            ],
            "{}",
            runner.name()
        );
        // Pricing changes: what PricingTest read, which Pest ran last; of the greetings, which
        // didn't read it, one runs as a check.
        r.write(
            "src/Pricing.php",
            &class(
                "Pricing",
                "    public static function price(int $x): int { return 2 * $x; }",
            ),
        );
        let second = run(&r, composer, &[]);
        assert_eq!(second.code, 0, "{}", second.said());
        assert_eq!(
            second.line("unit: 4"),
            "unit: 4 of 6 tests run, in 2 of 4 test files; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "{}",
            runner.name()
        );
        assert!(
            second.stdout.contains("4 passed") || second.stderr.contains("4 passed"),
            "{}: {}",
            runner.name(),
            second.said()
        );
    }
}

// covers: C-91
#[test]
fn a_php_that_reads_no_ini_file_is_said_and_runs_everything() {
    for runner in phpunits().into_iter().take(1) {
        let r = repo(&runner, &shop("{mode: reuse, unit: test}"));
        let ran = run(&r, &["php", "-n", "vendor/bin/phpunit"], &[]);
        assert!(
            ran.said().contains(
                "unit: test files aren't observed: php -n reads no ini file, and hushrun's adapter loads through one"
            ),
            "{}",
            ran.said()
        );
    }
}
