//! \[ADR 0116\] Test files and tests with PHPUnit and Pest (C-30 to C-34, C-39, C-90 to C-93): a
//! unit is a test file of PHPUnit's suite, what it read is what the process had included and
//! opened when its last test ended, but other test files it doesn't name; each test is told on
//! its own and left out of the suite PHPUnit runs; and reuse never misses a failure.
//!
//! The fixtures need PHP 8.3 and Composer's `vendor` of each runner, installed outside the
//! repository: PHPUnit 10, 11 and 12 in the directories `HUSHRUN_TEST_PHPUNIT10`,
//! `HUSHRUN_TEST_PHPUNIT11` and `HUSHRUN_TEST_PHPUNIT12` name, and Pest 3 and 4 in those
//! `HUSHRUN_TEST_PEST3` and `HUSHRUN_TEST_PEST4` name (CI: `composer require phpunit/phpunit:<v>`
//! in a directory of its own, then its `vendor`). Each test runs on every runner it finds, says so
//! and passes where all are missing, unless `HUSHRUN_TEST_TOOLS=required` (and `HUSHRUN_TEST_PHP`
//! isn't `skip`), which needs them all.

#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::{Value, json};

const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_UNITS",
    "HUSHRUN_PHP_PREPEND",
    "HUSHRUN_PHPUNIT_BOOTSTRAP",
    "PHP_INI_SCAN_DIR",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "XDEBUG_MODE",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_CC",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// One runner these tests run on: PHPUnit's or Pest's major and its Composer `vendor`.
#[derive(Clone)]
struct Runner {
    pest: bool,
    major: u32,
    vendor: PathBuf,
}

impl Runner {
    fn name(&self) -> String {
        format!(
            "{} {}",
            if self.pest { "Pest" } else { "PHPUnit" },
            self.major
        )
    }
}

/// The `vendor` a variable names, when it holds `package` of `major`.
fn runner_in(variable: &str, package: &str, pest: bool, major: u32) -> Option<Runner> {
    let vendor = PathBuf::from(std::env::var_os(variable)?);
    let installed = std::fs::read(vendor.join("composer/installed.json")).ok()?;
    let installed: Value = serde_json::from_slice(&installed).ok()?;
    let version = installed["packages"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == package)?["version"]
        .as_str()?
        .trim_start_matches('v')
        .to_string();
    version.starts_with(&format!("{major}.")).then_some(Runner {
        pest,
        major,
        vendor,
    })
}

fn has_php() -> bool {
    Command::new("php")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn required(what: &str, found: usize, wanted: usize) {
    let required = std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required")
        && std::env::var("HUSHRUN_TEST_PHP").as_deref() != Ok("skip");
    if required && found < wanted {
        panic!("PHP with {what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    if found == 0 {
        eprintln!(
            "note: skipping: PHP with {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
        );
    }
}

/// Every PHPUnit this machine has for these tests.
fn phpunits() -> Vec<Runner> {
    let found: Vec<Runner> = [10, 11, 12]
        .iter()
        .filter_map(|major| {
            runner_in(
                &format!("HUSHRUN_TEST_PHPUNIT{major}"),
                "phpunit/phpunit",
                false,
                *major,
            )
        })
        .filter(|_| has_php())
        .collect();
    required(
        "PHPUnit 10, 11 and 12 (HUSHRUN_TEST_PHPUNIT10, 11, 12)",
        found.len(),
        3,
    );
    found
}

/// Every Pest this machine has for these tests.
fn pests() -> Vec<Runner> {
    let found: Vec<Runner> = [3, 4]
        .iter()
        .filter_map(|major| {
            runner_in(
                &format!("HUSHRUN_TEST_PEST{major}"),
                "pestphp/pest",
                true,
                *major,
            )
        })
        .filter(|_| has_php())
        .collect();
    required("Pest 3 and 4 (HUSHRUN_TEST_PEST3, 4)", found.len(), 2);
    found
}

/// `from`'s files below `to`, each linked where it can be and copied where it can't: Pest finds
/// the project by the real path of its own program, so its `vendor` can't be a link.
fn link_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir").flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() {
            link_tree(&path, &target);
        } else if kind.is_symlink() {
            let link = std::fs::read_link(&path).expect("read_link");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link, &target).expect("symlink");
            #[cfg(windows)]
            std::fs::copy(path.parent().unwrap().join(link), &target).expect("copy");
        } else if std::fs::hard_link(&path, &target).is_err() {
            std::fs::copy(&path, &target).expect("copy");
        }
    }
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
}

impl Repo {
    fn new(runner: &Runner, files: &[(String, String)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let r = Repo {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
        };
        for (path, text) in files {
            r.write(path, text);
        }
        // The runner as installed outside the repository, which git ignores.
        if runner.pest {
            link_tree(&runner.vendor, &r.repo.join("vendor"));
        } else {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&runner.vendor, r.repo.join("vendor")).expect("symlink");
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&runner.vendor, r.repo.join("vendor")).expect("link");
        }
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn clean(cmd: &mut Command) {
        for var in CLEARED {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
                || name.starts_with("COMPOSER")
            {
                cmd.env_remove(name.as_ref());
            }
        }
    }

    fn run(&self, args: &[&str]) -> Ran {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("COMPOSER_HOME", self.root.join("composer"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        Repo::clean(&mut cmd);
        cmd.env("COMPOSER_HOME", self.root.join("composer"));
        cmd.env("COMPOSER_ALLOW_SUPERUSER", "1");
        for (name, value) in env {
            cmd.env(name, value);
        }
        let output = cmd.args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn map(&self) -> Value {
        store::map(&self.cache)
    }

    fn reads(&self, unit: &str) -> Vec<String> {
        let map = self.map();
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    fn tests_of(&self, unit: &str) -> Value {
        self.map()["units"][unit]["tests"].clone()
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn line(&self, start: &str) -> &str {
        self.stderr
            .lines()
            .find(|l| l.starts_with(start))
            .unwrap_or_else(|| panic!("no line starting {start:?} in:\n{}", self.out()))
    }

    fn out(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// The bootstrap the configuration names: Composer's autoloader, and the shop's classes from
/// `src/` by their names.
const BOOTSTRAP: &str = "<?php\nrequire __DIR__ . '/../vendor/autoload.php';\nspl_autoload_register(static function (string $class): void {\n    if (strncmp($class, 'Shop\\\\', 5) === 0) {\n        $file = __DIR__ . '/../src/' . str_replace('\\\\', '/', substr($class, 5)) . '.php';\n        if (is_file($file)) {\n            require $file;\n        }\n    }\n});\n";

const PHPUNIT_XML: &str = "<?xml version=\"1.0\"?>\n<phpunit bootstrap=\"tests/bootstrap.php\" cacheResult=\"false\">\n  <testsuites>\n    <testsuite name=\"all\"><directory>tests</directory></testsuite>\n  </testsuites>\n</phpunit>\n";

fn class(name: &str, body: &str) -> String {
    format!("<?php\nnamespace Shop;\nfinal class {name}\n{{\n{body}\n}}\n")
}

fn test_class(namespace: &str, name: &str, body: &str) -> String {
    format!(
        "<?php\nnamespace {namespace};\nuse PHPUnit\\Framework\\TestCase;\nfinal class {name} extends TestCase\n{{\n{body}\n}}\n"
    )
}

/// A shop: a cart that uses pricing, each with a test file; a greeting with five test files that
/// read only it; and a test file that opens a PHP file as data.
fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "vendor\n.phpunit.cache\n"),
        file("phpunit.xml", PHPUNIT_XML),
        file("tests/bootstrap.php", BOOTSTRAP),
        file(
            "src/Pricing.php",
            &class(
                "Pricing",
                "    public static function price(int $x): int { return $x * 2; }",
            ),
        ),
        file(
            "src/Cart.php",
            &class(
                "Cart",
                "    public static function total(array $xs): int { return array_sum(array_map([Pricing::class, 'price'], $xs)); }",
            ),
        ),
        file(
            "src/Greet.php",
            &class(
                "Greet",
                "    public static function hello(): string { return 'hi'; }",
            ),
        ),
        file("src/template.php", "<?php // Hello, <?= $name ?>\n"),
        file(
            "tests/Unit/CartTest.php",
            &test_class(
                "Shop\\Tests\\Unit",
                "CartTest",
                "    public function testTotal(): void { $this->assertSame(6, \\Shop\\Cart::total([1, 2])); }\n    public static function sums(): array { return ['one' => [[1], 2], [[2], 4]]; }\n    #[\\PHPUnit\\Framework\\Attributes\\DataProvider('sums')]\n    public function testSums(array $xs, int $want): void { $this->assertSame($want, \\Shop\\Cart::total($xs)); }",
            ),
        ),
        file(
            "tests/Unit/PricingTest.php",
            &test_class(
                "Shop\\Tests\\Unit",
                "PricingTest",
                "    public function testPrice(): void { $this->assertSame(6, \\Shop\\Pricing::price(3)); }",
            ),
        ),
        file(
            "tests/Unit/TemplateTest.php",
            &test_class(
                "Shop\\Tests\\Unit",
                "TemplateTest",
                "    public function testText(): void { $this->assertStringContainsString('Hello', file_get_contents(__DIR__ . '/../../src/template.php')); }",
            ),
        ),
    ];
    for n in 1..=5 {
        files.push(file(
            &format!("tests/Greet/Greet{n}Test.php"),
            &test_class(
                "Shop\\Tests\\Greet",
                &format!("Greet{n}Test"),
                "    public function testHello(): void { $this->assertSame('hi', \\Shop\\Greet::hello()); }",
            ),
        ));
    }
    files
}

const PHPUNIT_RUN: &[&str] = &["run", "unit", "--", "vendor/bin/phpunit"];

// covers: C-30, C-31, C-90, C-92
#[test]
fn phpunit_test_files_are_observed_from_what_php_included_and_opened() {
    for runner in phpunits() {
        let r = Repo::new(&runner, &shop("observe"));
        let first = r.run(PHPUNIT_RUN);
        assert_eq!(first.code, 0, "{}: {}", runner.name(), first.out());
        assert_eq!(
            first.line("hushrun: unit: with"),
            "hushrun: unit: with `tests: reuse`, all 8 test files would have run: none had passed here yet on the files it read",
            "{}",
            runner.name()
        );
        // What the process had included when its last test ended: the bootstrap, which every
        // test file reads, and what the greeting's test files, which PHPUnit ran first, loaded;
        // never another test file, which PHPUnit included before any test ran.
        assert_eq!(
            r.reads("tests/Unit/CartTest.php"),
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
            r.reads("tests/Unit/TemplateTest.php")
                .contains(&"src/template.php".to_string()),
            "{}: {:?}",
            runner.name(),
            r.reads("tests/Unit/TemplateTest.php")
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
        let changed = r.run(PHPUNIT_RUN);
        assert_eq!(changed.code, 0, "{}", changed.out());
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("hushrun: unit: with")).starts_with(
                "hushrun: unit: with `tests: reuse`, 3 of 8 test files would have run; 5 already passed on the files they read"
            ),
            "{}: {}",
            runner.name(),
            changed.stderr
        );
    }
}

// covers: C-34, C-39, C-91, C-93
#[test]
fn phpunit_reuses_test_files_and_tests_that_already_passed() {
    for runner in phpunits() {
        let r = Repo::new(&runner, &shop("{mode: reuse, unit: test}"));
        let first = r.run(PHPUNIT_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed PHPUnit's test files yet"
        );
        assert_eq!(
            r.tests_of("tests/Unit/CartTest.php"),
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
        let second = r.run(PHPUNIT_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        assert_eq!(
            second.line("hushrun: unit: 2"),
            "hushrun: unit: 2 of 10 tests run, in 2 of 8 test files; 8 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "{}",
            runner.name()
        );
        // PHPUnit ran what was left.
        assert!(
            second.stdout.contains("OK (2 tests, 2 assertions)"),
            "{}: {}",
            runner.name(),
            second.out()
        );
        assert!(
            hushrun_testkit::lines::says_reused(&second.stderr, 8, "tests"),
            "{}",
            second.stderr
        );
        // Everything already passed but the audited test: PHPUnit 12, which fails a run of no
        // test, passes, as the run of those tests did.
        let again = r.run(&[
            "run",
            "unit",
            "--",
            "vendor/bin/phpunit",
            "--testsuite",
            "all",
        ]);
        assert_eq!(again.code, 0, "{}", again.out());
        // Why a test file runs, from this machine alone.
        r.write(
            "src/Pricing.php",
            &class(
                "Pricing",
                "    public static function price(int $x): int { return 2 * $x; }",
            ),
        );
        let why = r.run(&["why", "unit", "tests/Unit/CartTest.php"]);
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

/// A test file whose tests fail while FAIL is set, and one whose class teardown fails while
/// BREAK is.
fn flips() -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "vendor\n"),
        file("phpunit.xml", PHPUNIT_XML),
        file("tests/bootstrap.php", BOOTSTRAP),
        file(
            "tests/FlipTest.php",
            &test_class(
                "Shop\\Tests",
                "FlipTest",
                "    public function testOne(): void { $this->assertTrue(true); }\n    public function testFlips(): void { $this->assertFalse((bool) getenv('FAIL')); }\n    public function testSkipped(): void { $this->markTestSkipped('never'); }",
            ),
        ),
        file(
            "tests/HookTest.php",
            &test_class(
                "Shop\\Tests",
                "HookTest",
                "    public static function tearDownAfterClass(): void { if (getenv('BREAK')) { throw new \\RuntimeException('teardown'); } }\n    public function testA(): void { $this->assertTrue(true); }\n    public function testB(): void { $this->assertTrue(true); }",
            ),
        ),
    ]
}

// covers: C-39, C-92
#[test]
fn a_failure_is_never_reused_and_a_failing_class_hook_fails_its_tests() {
    for runner in phpunits() {
        let r = Repo::new(&runner, &flips());
        let first = r.run_env(PHPUNIT_RUN, &[("FAIL", "1"), ("BREAK", "1")]);
        assert_ne!(first.code, 0, "{}", first.out());
        assert_eq!(
            r.tests_of("tests/FlipTest.php"),
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
            r.tests_of("tests/HookTest.php"),
            json!({"Shop\\Tests\\HookTest::testA": "failed", "Shop\\Tests\\HookTest::testB": "failed"}),
            "{}",
            runner.name()
        );
        // Fixed: what failed runs, and what didn't record a pass; the one test that passed may
        // be reused or audited.
        let second = r.run(PHPUNIT_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        assert_eq!(
            second.line("hushrun: unit: "),
            "hushrun: unit: all 5 tests run; 1 that already passed on the files it read runs anyway, as a check",
            "{}",
            runner.name()
        );
        let again = r.run(PHPUNIT_RUN);
        assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
    }
}

/// A Pest shop, run through `composer test`.
fn pest_shop() -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "vendor\n"),
        file(
            "composer.json",
            "{\"name\": \"acme/shop\", \"scripts\": {\"test\": \"pest\"}}\n",
        ),
        file(
            "phpunit.xml",
            "<?xml version=\"1.0\"?>\n<phpunit bootstrap=\"tests/bootstrap.php\" cacheResult=\"false\">\n  <testsuites>\n    <testsuite name=\"all\"><directory suffix=\"Test.php\">tests</directory></testsuite>\n  </testsuites>\n</phpunit>\n",
        ),
        file("tests/bootstrap.php", BOOTSTRAP),
        file("tests/Pest.php", "<?php\n"),
        file(
            "src/Pricing.php",
            &class(
                "Pricing",
                "    public static function price(int $x): int { return $x * 2; }",
            ),
        ),
        file(
            "src/Greet.php",
            &class(
                "Greet",
                "    public static function hello(): string { return 'hi'; }",
            ),
        ),
        file(
            "tests/Unit/PricingTest.php",
            "<?php\nuse Shop\\Pricing;\nit('prices', function () { expect(Pricing::price(3))->toBe(6); });\ntest('sums', function (int $x, int $want) { expect(Pricing::price($x))->toBe($want); })->with(['one' => [1, 2], [2, 4]]);\n",
        ),
        file(
            "tests/Unit/GreetTest.php",
            "<?php\nit('greets', fn () => expect(Shop\\Greet::hello())->toBe('hi'));\n",
        ),
        file(
            "tests/Unit/Greet2Test.php",
            "<?php\nit('greets', fn () => expect(Shop\\Greet::hello())->toBe('hi'));\n",
        ),
        file(
            "tests/Unit/Greet3Test.php",
            "<?php\nit('greets', fn () => expect(Shop\\Greet::hello())->toBe('hi'));\n",
        ),
    ]
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
        let r = Repo::new(&runner, &pest_shop());
        let run = &["run", "unit", "--", "composer", "test"];
        let first = r.run(run);
        assert_eq!(first.code, 0, "{}: {}", runner.name(), first.out());
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed Pest's test files yet"
        );
        // Pest ran GreetTest first, so what PricingTest read is GreetTest's and its own.
        assert_eq!(
            r.reads("tests/Unit/PricingTest.php"),
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
            r.tests_of("tests/Unit/PricingTest.php"),
            json!({
                "it_prices": "passed",
                "sums with data set \"dataset \"one\"\"": "passed",
                "sums with data set \"(2, 4)\"": "passed"
            }),
            "{}",
            runner.name()
        );
        assert_eq!(
            r.reads("tests/Unit/GreetTest.php"),
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
        let second = r.run(run);
        assert_eq!(second.code, 0, "{}", second.out());
        assert_eq!(
            second.line("hushrun: unit: 4"),
            "hushrun: unit: 4 of 6 tests run, in 2 of 4 test files; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "{}",
            runner.name()
        );
        assert!(
            second.stdout.contains("4 passed") || second.stderr.contains("4 passed"),
            "{}: {}",
            runner.name(),
            second.out()
        );
    }
}

// covers: C-91
#[test]
fn a_php_that_reads_no_ini_file_is_said_and_runs_everything() {
    for runner in phpunits().into_iter().take(1) {
        let r = Repo::new(&runner, &shop("{mode: reuse, unit: test}"));
        let ran = r.run(&["run", "unit", "--", "php", "-n", "vendor/bin/phpunit"]);
        assert!(
            ran.stderr.contains(
                "hushrun: unit: test files aren't observed: php -n reads no ini file, and hushrun's adapter loads through one"
            ),
            "{}",
            ran.out()
        );
    }
}
