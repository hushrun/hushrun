//! \[ADR 0116\] The PHPUnits, Pests and shops the tests of the PHPUnit adapter run, in-process
//! (`units_phpunit.rs`) and end to end (`e2e_phpunit.rs`). See `units_phpunit.rs` for the tools.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::repo::Repo;

/// One runner these tests run on: PHPUnit's or Pest's major and its Composer `vendor`.
#[derive(Clone)]
pub struct Runner {
    pub pest: bool,
    pub major: u32,
    pub vendor: PathBuf,
}

impl Runner {
    pub fn name(&self) -> String {
        format!(
            "{} {}",
            if self.pest { "Pest" } else { "PHPUnit" },
            self.major
        )
    }
}

/// The `vendor` a variable names, when it holds `package` of `major`.
pub fn runner_in(variable: &str, package: &str, pest: bool, major: u32) -> Option<Runner> {
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

pub fn has_php() -> bool {
    Command::new("php")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn required(what: &str, found: usize, wanted: usize) {
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
pub fn phpunits() -> Vec<Runner> {
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
pub fn pests() -> Vec<Runner> {
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
pub fn link_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir").flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() {
            link_tree(&path, &target);
        } else if kind.is_symlink() {
            let link = std::fs::read_link(&path).expect("read_link");
            relink(&path, &link, &target);
        } else if std::fs::hard_link(&path, &target).is_err() {
            std::fs::copy(&path, &target).expect("copy");
        }
    }
}

/// A repository of `files` whose COMMAND runs `runner`, installed outside it, which git ignores:
/// none of a person's Composer settings reach it.
pub fn repo(runner: &Runner, files: &[(String, String)]) -> Repo {
    // What the developer's shell exports of Composer's never reaches it.
    let composer: Vec<&'static str> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("COMPOSER"))
        .map(|name| &*Box::leak(name.into_boxed_str()))
        .collect();
    let r = Repo::new(files)
        .clear(&[
            "HUSHRUN_PHP_PREPEND",
            "HUSHRUN_PHPUNIT_BOOTSTRAP",
            "PHP_INI_SCAN_DIR",
            "XDEBUG_MODE",
        ])
        .clear(&composer);
    let home = r.root.join("composer");
    let r = r
        .give("COMPOSER_HOME", home)
        .give("COMPOSER_ALLOW_SUPERUSER", "1");
    if runner.pest {
        link_tree(&runner.vendor, &r.repo.join("vendor"));
    } else {
        link_dir(&runner.vendor, &r.repo.join("vendor"));
    }
    r
}

/// `path`, a link to `link`, again at `target`: a link where the system can make one, a copy
/// where it can't. One function a system, as [`link_dir`].
#[cfg(unix)]
fn relink(_path: &Path, link: &Path, target: &Path) {
    std::os::unix::fs::symlink(link, target).expect("symlink");
}

#[cfg(windows)]
fn relink(path: &Path, link: &Path, target: &Path) {
    std::fs::copy(path.parent().unwrap().join(link), target).expect("copy");
}

/// A link at `to` to the directory `from`. One function a system, so that the static map, which
/// reads only this system's code, never takes the other's names for items of hushrun's.
#[cfg(unix)]
fn link_dir(from: &Path, to: &Path) {
    std::os::unix::fs::symlink(from, to).expect("symlink");
}

#[cfg(windows)]
fn link_dir(from: &Path, to: &Path) {
    std::os::windows::fs::symlink_dir(from, to).expect("link");
}

/// What `unit` read, as this machine's store keeps it.
pub fn reads(r: &Repo, unit: &str) -> Vec<String> {
    let map = r.map();
    map["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// The tests of `unit` this machine's store keeps, and how each ended.
pub fn tests_of(r: &Repo, unit: &str) -> Value {
    r.map()["units"][unit]["tests"].clone()
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// The bootstrap the configuration names: Composer's autoloader, and the shop's classes from
/// `src/` by their names.
pub const BOOTSTRAP: &str = "<?php\nrequire __DIR__ . '/../vendor/autoload.php';\nspl_autoload_register(static function (string $class): void {\n    if (strncmp($class, 'Shop\\\\', 5) === 0) {\n        $file = __DIR__ . '/../src/' . str_replace('\\\\', '/', substr($class, 5)) . '.php';\n        if (is_file($file)) {\n            require $file;\n        }\n    }\n});\n";

pub const PHPUNIT_XML: &str = "<?xml version=\"1.0\"?>\n<phpunit bootstrap=\"tests/bootstrap.php\" cacheResult=\"false\">\n  <testsuites>\n    <testsuite name=\"all\"><directory>tests</directory></testsuite>\n  </testsuites>\n</phpunit>\n";

pub fn class(name: &str, body: &str) -> String {
    format!("<?php\nnamespace Shop;\nfinal class {name}\n{{\n{body}\n}}\n")
}

pub fn test_class(namespace: &str, name: &str, body: &str) -> String {
    format!(
        "<?php\nnamespace {namespace};\nuse PHPUnit\\Framework\\TestCase;\nfinal class {name} extends TestCase\n{{\n{body}\n}}\n"
    )
}

/// A shop: a cart that uses pricing, each with a test file; a greeting with five test files that
/// read only it; and a test file that opens a PHP file as data.
pub fn shop(tests: &str) -> Vec<(String, String)> {
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

/// A test file whose tests fail while FAIL is set, and one whose class teardown fails while
/// BREAK is.
pub fn flips() -> Vec<(String, String)> {
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

/// A Pest shop, run through `composer test`.
pub fn pest_shop() -> Vec<(String, String)> {
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
