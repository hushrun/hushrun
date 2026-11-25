//! \[ADR 0085\] The Jest repositories and tools the tests of Jest's adapter run, in-process
//! (`units_jest.rs`) and end to end (`e2e_jest.rs`). See `units_jest.rs` for the tools.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use super::repo::Repo;

/// One Jest these tests run on: its major and its `node_modules`.
#[derive(Clone)]
pub struct Jest {
    pub major: u32,
    pub modules: PathBuf,
}

/// The `node_modules` a variable names, when it holds a Jest of `major`.
pub fn jest_in(variable: &str, major: u32) -> Option<Jest> {
    let dir = PathBuf::from(std::env::var_os(variable)?);
    let package = std::fs::read(dir.join("jest/package.json")).ok()?;
    let package: Value = serde_json::from_slice(&package).ok()?;
    let version = package["version"].as_str()?;
    version.starts_with(&format!("{major}.")).then_some(Jest {
        major,
        modules: dir,
    })
}

pub fn has_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Every Jest this machine has for these tests; none, said, where it has neither, and a failure
/// with `HUSHRUN_TEST_TOOLS=required` where it lacks one.
pub fn jests() -> Vec<Jest> {
    let found: Vec<Jest> = [("HUSHRUN_TEST_JEST29", 29), ("HUSHRUN_TEST_JEST30", 30)]
        .iter()
        .filter_map(|(v, major)| jest_in(v, *major))
        .filter(|_| has_node())
        .collect();
    let required = std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required");
    if required && found.len() < 2 {
        panic!(
            "Node with Jest 29 (HUSHRUN_TEST_JEST29) and Jest 30 (HUSHRUN_TEST_JEST30) is missing, and HUSHRUN_TEST_TOOLS=required"
        );
    }
    if found.is_empty() {
        eprintln!(
            "note: skipping: Node with Jest 29 or 30 is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
        );
    }
    found
}

/// A repository of `files` whose COMMAND runs `jest`: as installed outside the repository, which
/// git ignores, and with none of a person's Node or Jest settings.
pub fn repo(jest: &Jest, files: &[(String, String)]) -> Repo {
    let r = Repo::new(files).clear(&[
        "HUSHRUN_JEST",
        "HUSHRUN_JEST_DESELECT",
        "HUSHRUN_JEST_RUN",
        "HUSHRUN_JEST_START",
        "HUSHRUN_JEST_WATCH",
        "JEST_WORKER_ID",
        "NODE_OPTIONS",
    ]);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&jest.modules, r.repo.join("node_modules")).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&jest.modules, r.repo.join("node_modules")).expect("link");
    r
}

/// What the store's map says test file `unit` read.
pub fn reads(r: &Repo, unit: &str) -> Vec<String> {
    let map = r.map();
    map["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

pub const CONFIG: &str = "module.exports = { testEnvironment: 'node', transform: {} };\n";

/// A shop: a cart that requires pricing, each with a test file; a greeting; and five test files
/// that read only the greeting, so that a plan has something to reuse.
pub fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\ncoverage\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file("src/pricing.js", "exports.price = (x) => x * 2;\n"),
        file(
            "src/cart.js",
            "const { price } = require('./pricing');\nexports.total = (xs) => xs.reduce((n, x) => n + price(x), 0);\n",
        ),
        file("src/greet.js", "exports.hello = () => 'hi';\n"),
        file(
            "test/cart.test.js",
            "const { total } = require('../src/cart');\ntest('total', () => { expect(total([1, 2])).toBe(6); });\n",
        ),
        file(
            "test/pricing.test.js",
            "const { price } = require('../src/pricing');\ntest('price', () => { expect(price(3)).toBe(6); });\n",
        ),
    ];
    for n in 1..=5 {
        files.push(file(
            &format!("test/greet{n}.test.js"),
            "const { hello } = require('../src/greet');\ntest('hello', () => { expect(hello()).toBe('hi'); });\n",
        ));
    }
    files
}

/// A test file whose tests fail while FAIL is set, and one whose `ok` fails while HIDDEN is,
/// which no file says, as a missed dependency would.
pub fn flips() -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file("src/price.js", "exports.price = (x) => x * 2;\n"),
        file(
            "test/cart.test.js",
            "const { price } = require('../src/price');\ndescribe('cart', () => {\n  test('one', () => { expect(price(1)).toBe(2); });\n  test('two', () => { expect(price(2)).toBe(4); });\n  test('flips', () => { expect(process.env.FAIL).toBeUndefined(); });\n  test('same', () => { expect(1).toBe(1); });\n  test('same', () => { expect(2).toBe(2); });\n});\ntest.skip('skipped', () => { throw new Error('never'); });\n",
        ),
        file(
            "test/two.test.js",
            "test('ok', () => { expect(process.env.HIDDEN).toBeUndefined(); });\ntest('also', () => { expect(process.env.FAIL).toBeUndefined(); });\n",
        ),
    ]
}
