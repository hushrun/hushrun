//! \[ADR 0107\] The Node and Mocha repositories and tools the tests of the adapters for Node's
//! test runner and Mocha run, in-process (`units_node.rs`) and end to end (`e2e_node.rs`). See
//! `units_node.rs` for the tools.

#![allow(dead_code)]

use std::path::PathBuf;

use serde_json::Value;

use super::repo::Repo;

/// A Node these tests run on: its `bin` directory, and Mocha's `node_modules` when the test needs
/// Mocha.
#[derive(Clone)]
pub struct Tools {
    pub name: String,
    pub node: PathBuf,
    pub modules: Option<PathBuf>,
}

/// The `bin` directory a variable names, when its `node` is 22.15 or later.
pub fn node_in(variable: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(variable)?);
    let out = hushrun_testkit::tool::command(dir.join("node"))
        .arg("--version")
        .output()
        .ok()?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut parts = version.trim_start_matches('v').split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    (major > 22 || (major == 22 && minor >= 15)).then_some(dir)
}

pub fn required() -> bool {
    std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required")
}

/// Every Node this machine has for these tests.
pub fn nodes() -> Vec<Tools> {
    let found: Vec<Tools> = ["HUSHRUN_TEST_NODE22", "HUSHRUN_TEST_NODE24"]
        .iter()
        .filter_map(|v| {
            Some(Tools {
                name: v.trim_start_matches("HUSHRUN_TEST_").to_lowercase(),
                node: node_in(v)?,
                modules: None,
            })
        })
        .collect();
    if required() && found.len() < 2 {
        panic!(
            "Node 22 (HUSHRUN_TEST_NODE22) and Node 24 (HUSHRUN_TEST_NODE24) are missing, and HUSHRUN_TEST_TOOLS=required"
        );
    }
    if found.is_empty() {
        eprintln!(
            "note: skipping: no Node of 22.15 or later (HUSHRUN_TEST_NODE22, HUSHRUN_TEST_NODE24; HUSHRUN_TEST_TOOLS=required makes this fail)"
        );
    }
    found
}

/// Every Mocha this machine has for these tests, each on the first Node found.
pub fn mochas() -> Vec<Tools> {
    let Some(node) = nodes().into_iter().next() else {
        return Vec::new();
    };
    let found: Vec<Tools> = [
        ("HUSHRUN_TEST_MOCHA10", "10."),
        ("HUSHRUN_TEST_MOCHA11", "11."),
    ]
    .iter()
    .filter_map(|(v, major)| {
        let dir = PathBuf::from(std::env::var_os(v)?);
        let package = std::fs::read(dir.join("mocha/package.json")).ok()?;
        let package: Value = serde_json::from_slice(&package).ok()?;
        package["version"]
            .as_str()?
            .starts_with(major)
            .then(|| Tools {
                name: format!("mocha {}", package["version"].as_str().unwrap_or("")),
                node: node.node.clone(),
                modules: Some(dir),
            })
    })
    .collect();
    if required() && found.len() < 2 {
        panic!(
            "Mocha 10 (HUSHRUN_TEST_MOCHA10) and Mocha 11 (HUSHRUN_TEST_MOCHA11) are missing, and HUSHRUN_TEST_TOOLS=required"
        );
    }
    if found.is_empty() {
        eprintln!(
            "note: skipping: Mocha 10 or 11 is missing (HUSHRUN_TEST_TOOLS=required makes this fail)"
        );
    }
    found
}

/// A repository of `files` whose COMMAND runs `tools`' Node, first on its `PATH`, and Mocha's
/// `node_modules` when it has one, as installed outside the repository, which git ignores; with
/// none of a person's Node settings.
pub fn repo(tools: &Tools, files: &[(String, String)]) -> Repo {
    let mut paths = vec![tools.node.clone()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).expect("PATH");
    let r = Repo::new(files)
        .clear(&[
            "HUSHRUN_MOCHA_IN",
            "HUSHRUN_NODE_TEST_DESELECT",
            "HUSHRUN_NODE_TEST_IN",
            "HUSHRUN_NODE_TEST_RUN",
            "HUSHRUN_NODE_TEST_START",
            "NODE_OPTIONS",
            "NODE_TEST_CONTEXT",
        ])
        .give("PATH", path);
    if let Some(modules) = &tools.modules {
        #[cfg(unix)]
        std::os::unix::fs::symlink(modules, r.repo.join("node_modules")).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(modules, r.repo.join("node_modules")).expect("link");
    }
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

/// Each test's state the store holds for `unit`, by id.
pub fn tests_of(r: &Repo, unit: &str) -> serde_json::Map<String, Value> {
    r.map()["units"][unit]["tests"]
        .as_object()
        .cloned()
        .unwrap_or_default()
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

pub const NODE_RUN: &[&str] = &["run", "unit", "--", "node", "--test"];
pub const MOCHA_RUN: &[&str] = &[
    "run",
    "unit",
    "--",
    "node",
    "node_modules/mocha/bin/mocha.js",
];

/// A shop for Node's runner: a cart that requires pricing, a greeting read by an ES module test
/// file, and test files that read only the greeting, so that a plan has something to reuse.
pub fn node_shop(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("src/pricing.js", "exports.price = (x) => x * 2;\n"),
        file(
            "src/cart.js",
            "const { price } = require('./pricing');\nexports.total = (xs) => xs.reduce((n, x) => n + price(x), 0);\n",
        ),
        file("src/greet.mjs", "export const hello = () => 'hi';\n"),
        file("fixtures/rate.js", "2\n"),
        file(
            "test/cart.test.js",
            "const test = require('node:test');\nconst assert = require('node:assert');\nconst { total } = require('../src/cart');\ntest('total', () => { assert.equal(total([1, 2]), 6); });\n",
        ),
        file(
            "test/rate.test.mjs",
            "import { test } from 'node:test';\nimport assert from 'node:assert';\nimport { readFileSync } from 'node:fs';\ntest('rate', () => { assert.equal(readFileSync('fixtures/rate.js', 'utf8'), '2\\n'); });\n",
        ),
    ];
    for n in 1..=4 {
        files.push(file(
            &format!("test/greet{n}.test.mjs"),
            "import test from 'node:test';\nimport assert from 'node:assert';\nimport { hello } from '../src/greet.mjs';\ntest('hello', () => { assert.equal(hello(), 'hi'); });\n",
        ));
    }
    files
}

/// Test files whose tests are told one by one: suites, a test with subtests, ids that collide,
/// a test that fails while FAIL is set.
pub fn node_tests() -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("src/price.js", "exports.price = (x) => x * 2;\n"),
        file(
            "test/cart.test.js",
            "const { describe, it, test } = require('node:test');\nconst assert = require('node:assert');\nconst { price } = require('../src/price');\ndescribe('cart', () => {\n  it('one', () => { assert.equal(price(1), 2); });\n  it('flips', () => { assert.equal(process.env.FAIL, undefined); });\n  it('same', () => {});\n  it('same', () => {});\n});\ntest('parent', async (t) => {\n  await t.test('kid', () => {});\n});\ntest.skip('skipped', () => { throw new Error('never'); });\n",
        ),
        file(
            "test/two.test.mjs",
            "import test from 'node:test';\nimport assert from 'node:assert';\ntest('ok', () => {});\ntest('also', () => { assert.equal(process.env.FAIL, undefined); });\n",
        ),
    ]
}

/// A shop for Mocha: a cart, a cache module that reads a file once and keeps it, a root hook
/// file, and spec files that read only the greeting.
pub fn mocha_shop(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file(".mocharc.json", "{\"spec\": \"test/**/*.spec.{js,mjs}\"}\n"),
        file("src/pricing.js", "exports.price = (x) => x * 2;\n"),
        file(
            "src/cart.js",
            "const { price } = require('./pricing');\nexports.total = (xs) => xs.reduce((n, x) => n + price(x), 0);\n",
        ),
        file(
            "src/config.js",
            "const fs = require('fs');\nconst path = require('path');\nlet kept;\nexports.rate = () => kept || (kept = fs.readFileSync(path.join(__dirname, '..', 'fixtures', 'rate.js'), 'utf8').trim());\n",
        ),
        file("fixtures/rate.js", "2\n"),
        file("src/greet.js", "exports.hello = () => 'hi';\n"),
        file(
            "test/cart.spec.js",
            "const assert = require('assert');\nconst { total } = require('../src/cart');\nconst config = require('../src/config');\ndescribe('cart', () => {\n  it('total', () => { assert.equal(total([1, 2]), 6); });\n  it('rate', () => { assert.equal(config.rate(), '2'); });\n});\n",
        ),
        // Reads the config module after cart.spec.js did: the file it kept is this one's too.
        file(
            "test/rate.spec.js",
            "const assert = require('assert');\nconst config = require('../src/config');\nit('rate again', () => { assert.equal(config.rate(), '2'); });\n",
        ),
        file(
            "test/hooks.spec.js",
            "const assert = require('assert');\nbeforeEach(function () { this.base = 1; });\nit('base', function () { assert.equal(this.base, 1); });\n",
        ),
    ];
    for n in 1..=3 {
        files.push(file(
            &format!("test/greet{n}.spec.js"),
            "const assert = require('assert');\nconst { hello } = require('../src/greet');\nit('hello', function () { assert.equal(hello(), 'hi'); assert.equal(this.base, 1); });\n",
        ));
    }
    files.push(file(
        "test/esm.spec.mjs",
        "import assert from 'node:assert';\nimport greet from '../src/greet.js';\nit('esm hello', () => { assert.equal(greet.hello(), 'hi'); });\n",
    ));
    files
}
