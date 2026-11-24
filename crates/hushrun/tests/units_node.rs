//! \[ADR 0107\] Test files and tests with Node's own test runner (`node --test`) and with Mocha
//! (C-30 to C-35, C-39, C-54 to C-57): a unit is a test file; what it read is what its process
//! (Node's runner) or its windows and module graph (Mocha) read; each test is told on its own; and
//! reuse never misses a failure.
//!
//! The fixtures need a Node of 22.15 or later, outside the repository: the `bin` directories
//! `HUSHRUN_TEST_NODE22` and `HUSHRUN_TEST_NODE24` name (CI: Node 22.20.0 and 24.11.0 from
//! nodejs.org), and Mocha 10 and 11 in the `node_modules` `HUSHRUN_TEST_MOCHA10` and
//! `HUSHRUN_TEST_MOCHA11` name (CI: `npm install --prefix <dir> mocha@10.8.2`, and `mocha@11.7.5`).
//! Each runs on every one it finds, says so and passes where all are missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which needs them all.

#[path = "common/store.rs"]
mod store;

use std::path::PathBuf;
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
    "HUSHRUN_NODE_TEST_IN",
    "HUSHRUN_NODE_TEST_RUN",
    "HUSHRUN_NODE_TEST_START",
    "HUSHRUN_NODE_TEST_DESELECT",
    "HUSHRUN_MOCHA_IN",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "NODE_OPTIONS",
    "NODE_TEST_CONTEXT",
    "npm_lifecycle_event",
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
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// A Node these tests run on: its `bin` directory, and Mocha's `node_modules` when the test needs
/// Mocha.
#[derive(Clone)]
struct Tools {
    name: String,
    node: PathBuf,
    modules: Option<PathBuf>,
}

/// The `bin` directory a variable names, when its `node` is 22.15 or later.
fn node_in(variable: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(variable)?);
    let out = Command::new(dir.join("node"))
        .arg("--version")
        .output()
        .ok()?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut parts = version.trim_start_matches('v').split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    (major > 22 || (major == 22 && minor >= 15)).then_some(dir)
}

fn required() -> bool {
    std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required")
}

/// Every Node this machine has for these tests.
fn nodes() -> Vec<Tools> {
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
fn mochas() -> Vec<Tools> {
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

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    tools: Tools,
}

impl Repo {
    fn new(tools: &Tools, files: &[(String, String)]) -> Repo {
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
            tools: tools.clone(),
        };
        for (path, text) in files {
            r.write(path, text);
        }
        if let Some(modules) = &tools.modules {
            #[cfg(unix)]
            std::os::unix::fs::symlink(modules, r.repo.join("node_modules")).expect("symlink");
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(modules, r.repo.join("node_modules")).expect("link");
        }
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn path(&self) -> std::ffi::OsString {
        let mut paths = vec![self.tools.node.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).expect("PATH")
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
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        for var in CLEARED {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        cmd.env("PATH", self.path());
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

    /// Each test's state the store holds for `unit`, by id.
    fn tests_of(&self, unit: &str) -> serde_json::Map<String, Value> {
        self.map()["units"][unit]["tests"]
            .as_object()
            .cloned()
            .unwrap_or_default()
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
            .unwrap_or_else(|| panic!("no line starting {start:?} in:\n{}", self.stderr))
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

const NODE_RUN: &[&str] = &["run", "unit", "--", "node", "--test"];
const MOCHA_RUN: &[&str] = &[
    "run",
    "unit",
    "--",
    "node",
    "node_modules/mocha/bin/mocha.js",
];

/// A shop for Node's runner: a cart that requires pricing, a greeting read by an ES module test
/// file, and test files that read only the greeting, so that a plan has something to reuse.
fn node_shop(tests: &str) -> Vec<(String, String)> {
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

// covers: C-30, C-31, C-32, C-54
#[test]
fn nodes_test_files_are_observed_from_what_each_process_loaded_and_opened() {
    for node in nodes() {
        let r = Repo::new(&node, &node_shop("observe"));
        let first = r.run(NODE_RUN);
        assert_eq!(first.code, 0, "{}: {}", node.name, first.out());
        assert_eq!(
            first.line("hushrun: unit: with"),
            "hushrun: unit: with `tests: reuse`, all 6 test files would have run: none had passed here yet on the files it read",
            "{}",
            node.name
        );
        assert_eq!(
            r.reads("test/cart.test.js"),
            ["src/cart.js", "src/pricing.js", "test/cart.test.js"],
            "{}",
            node.name
        );
        assert_eq!(
            r.reads("test/rate.test.mjs"),
            ["fixtures/rate.js", "test/rate.test.mjs"],
            "{}",
            node.name
        );
        assert_eq!(
            r.reads("test/greet1.test.mjs"),
            ["src/greet.mjs", "test/greet1.test.mjs"],
            "{}",
            node.name
        );
        assert_eq!(r.map()["unitFiles"].as_array().unwrap().len(), 6);
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let changed = r.run(NODE_RUN);
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("hushrun: unit: with")).starts_with(
                "hushrun: unit: with `tests: reuse`, 1 of 6 test files would have run; 5 already passed on the files they read"
            ),
            "{}: {}",
            node.name,
            changed.stderr
        );
    }
}

// covers: C-34, C-54
#[test]
fn node_reuses_test_files_that_already_passed_and_runs_what_a_change_reaches() {
    for node in nodes() {
        let r = Repo::new(&node, &node_shop("reuse"));
        let first = r.run(NODE_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed the test files of Node's test runner yet"
        );
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let second = r.run(NODE_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        assert_eq!(
            second.line("hushrun: unit: 2"),
            "hushrun: unit: 2 of 6 test files run; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "{}",
            node.name
        );
        assert!(
            hushrun_testkit::lines::says_reused(&second.stderr, 4, "test files"),
            "{}",
            second.stderr
        );
        // A change that breaks the cart: its test file runs, and fails.
        r.write("src/pricing.js", "exports.price = (x) => x * 3;\n");
        let broken = r.run(NODE_RUN);
        assert_eq!(broken.code, 1, "{}", broken.out());
        // A change to a data file it opened is caught too.
        r.write("src/pricing.js", "exports.price = (x) => x * 2;\n");
        assert_eq!(r.run(NODE_RUN).code, 0);
        r.write("fixtures/rate.js", "3\n");
        let data = r.run(NODE_RUN);
        assert_eq!(data.code, 1, "{}", data.out());
        // Why, with the imports the adapter saw.
        r.write("fixtures/rate.js", "2\n");
        assert_eq!(r.run(NODE_RUN).code, 0);
        r.write("src/pricing.js", "exports.price = (x) => 2 * x;\n");
        let why = r.run(&["why", "unit", "test/cart.test.js"]);
        assert_eq!(why.code, 0, "{}", why.stderr);
        assert!(
            why.stdout.starts_with(
                "test/cart.test.js runs: it read src/pricing.js, which changed\n  test/cart.test.js imports src/cart.js imports src/pricing.js"
            ),
            "{}",
            why.stdout
        );
    }
}

/// Test files whose tests are told one by one: suites, a test with subtests, ids that collide,
/// a test that fails while FAIL is set.
fn node_tests() -> Vec<(String, String)> {
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

// covers: C-39, C-55
#[test]
fn node_tells_each_test_and_leaves_out_one_by_one_what_passed() {
    for node in nodes() {
        let r = Repo::new(&node, &node_tests());
        let first = r.run_env(NODE_RUN, &[("FAIL", "1")]);
        assert_eq!(first.code, 1, "{}", first.out());
        let cart = r.tests_of("test/cart.test.js");
        assert_eq!(cart["cart > one"], "passed", "{}: {cart:?}", node.name);
        assert_eq!(cart["cart > flips"], "failed", "{cart:?}");
        assert_eq!(cart["cart > same"], "passed", "{cart:?}");
        assert_eq!(cart["cart > same [2]"], "passed", "{cart:?}");
        assert_eq!(cart["parent > kid"], "passed", "{cart:?}");
        assert_eq!(r.tests_of("test/two.test.mjs")["also"], "failed");
        // The two that failed run; everything that passed is left out, one by one, through
        // `require('node:test')` and `import test from 'node:test'` alike; a test with subtests
        // runs its body, its subtest left out.
        let second = r.run(NODE_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        let out = second.out();
        assert!(
            out.contains("hushrun: passed before on the files it read"),
            "{}: {out}",
            node.name
        );
        for ran in ["flips", "also"] {
            assert!(
                out.lines()
                    .any(|l| l.contains(ran) && !l.contains("hushrun:")),
                "{ran} ran: {out}"
            );
        }
        assert_eq!(r.tests_of("test/cart.test.js")["cart > flips"], "passed");
        let again = r.run(NODE_RUN);
        assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
    }
}

// covers: C-39, C-55
#[test]
fn node_keeps_no_pass_after_a_failing_hook_or_an_error_no_test_owns() {
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file(
            "test/hooks.test.js",
            "const { describe, it, after, test } = require('node:test');\ndescribe('blk', () => {\n  after(() => { throw new Error('block teardown'); });\n  it('one', () => {});\n});\ntest('fine', () => {});\n",
        ),
        file(
            "test/root.test.js",
            "const { after, test } = require('node:test');\nafter(() => { throw new Error('root teardown'); });\ntest('r1', () => {});\n",
        ),
        file(
            "test/leak.test.js",
            "const { test } = require('node:test');\ntest('leaks', () => { setTimeout(() => { throw new Error('late timer'); }, 5); });\ntest('waits', async () => { await new Promise((r) => setTimeout(r, 50)); });\n",
        ),
        file(
            "test/fine.test.js",
            "require('node:test')('fine', () => {});\n",
        ),
    ];
    for node in nodes() {
        let r = Repo::new(&node, &files);
        let first = r.run(NODE_RUN);
        assert_eq!(first.code, 1, "{}", first.out());
        let hooks = r.tests_of("test/hooks.test.js");
        assert_eq!(hooks["blk > one"], "failed", "{}: {hooks:?}", node.name);
        assert_eq!(hooks["fine"], "passed", "{hooks:?}");
        assert_eq!(r.tests_of("test/root.test.js")["r1"], "failed");
        let leak = r.tests_of("test/leak.test.js");
        assert!(
            leak.values().all(|s| s != "passed"),
            "{}: {leak:?}",
            node.name
        );
        // The same code again: every one of them runs, and fails again.
        let second = r.run(NODE_RUN);
        assert_eq!(second.code, 1, "{}", second.out());
        for said in ["block teardown", "root teardown", "late timer"] {
            assert!(second.out().contains(said), "{said}:\n{}", second.out());
        }
    }
}

// covers: C-32, C-54
#[test]
fn node_commands_hushrun_doesnt_observe_run_as_written_and_say_why() {
    for node in nodes() {
        let r = Repo::new(&node, &node_shop("reuse"));
        let none = r.run(&[
            "run",
            "unit",
            "--",
            "node",
            "--test",
            "--test-isolation=none",
        ]);
        assert!(
            none.stderr.contains(
                "hushrun: unit: test files aren't observed: node --test --test-isolation=none runs every test file in one process"
            ),
            "{}",
            none.stderr
        );
        assert_eq!(r.run(NODE_RUN).code, 0);
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let covered = r.run(&[
            "run",
            "unit",
            "--",
            "node",
            "--test",
            "--experimental-test-coverage",
        ]);
        assert_eq!(covered.code, 0, "{}", covered.out());
        assert!(
            covered.stderr.contains(
                "hushrun: unit: every test file runs: Node's test coverage is on, and a partial run's coverage would read as a drop"
            ),
            "{}",
            covered.stderr
        );
        // A shard: test files aren't left out, since a shorter list shards otherwise; tests are.
        r.write("src/pricing.js", "exports.price = (x) => x * 2;\n");
        let shard = &["run", "unit", "--", "node", "--test", "--test-shard=1/1"];
        assert_eq!(r.run(shard).code, 0);
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let sharded = r.run(shard);
        assert_eq!(sharded.code, 0, "{}", sharded.out());
        assert!(
            sharded.stderr.contains("--test-shard"),
            "{}",
            sharded.stderr
        );
    }
}

/// A shop for Mocha: a cart, a cache module that reads a file once and keeps it, a root hook
/// file, and spec files that read only the greeting.
fn mocha_shop(tests: &str) -> Vec<(String, String)> {
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

// covers: C-30, C-31, C-32, C-56, C-57
#[test]
fn mocha_spec_files_are_observed_through_their_windows_and_module_graph() {
    for mocha in mochas() {
        let r = Repo::new(&mocha, &mocha_shop("observe"));
        let first = r.run(MOCHA_RUN);
        assert_eq!(first.code, 0, "{}: {}", mocha.name, first.out());
        // The root hook's spec file isn't a unit: every spec file reads it.
        let units: Vec<String> = r.map()["unitFiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            units,
            [
                "test/cart.spec.js",
                "test/esm.spec.mjs",
                "test/greet1.spec.js",
                "test/greet2.spec.js",
                "test/greet3.spec.js",
                "test/rate.spec.js"
            ],
            "{}",
            mocha.name
        );
        assert_eq!(
            r.reads("test/greet1.spec.js"),
            ["src/greet.js", "test/greet1.spec.js", "test/hooks.spec.js"],
            "{}",
            mocha.name
        );
        assert_eq!(
            r.reads("test/esm.spec.mjs"),
            ["src/greet.js", "test/esm.spec.mjs", "test/hooks.spec.js"],
            "{}",
            mocha.name
        );
        // What the config module read once and kept, for a spec file whose test read it after.
        assert_eq!(
            r.reads("test/rate.spec.js"),
            [
                "fixtures/rate.js",
                "src/config.js",
                "test/hooks.spec.js",
                "test/rate.spec.js"
            ],
            "{}",
            mocha.name
        );
        r.write("fixtures/rate.js", "3\n");
        let changed = r.run(MOCHA_RUN);
        // Mocha exits with how many tests failed.
        assert_eq!(changed.code, 2, "{}", changed.out());
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("hushrun: unit: with")).starts_with(
                "hushrun: unit: with `tests: reuse`, 2 of 6 spec files would have run; 4 already passed on the files they read"
            ),
            "{}: {}",
            mocha.name,
            changed.stderr
        );
    }
}

// covers: C-34, C-39, C-56, C-57
#[test]
fn mocha_reuses_what_passed_and_its_root_hooks_still_run() {
    for mocha in mochas() {
        let r = Repo::new(&mocha, &mocha_shop("{mode: reuse, unit: test}"));
        let first = r.run(MOCHA_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let second = r.run(MOCHA_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        // The cart's two tests, one more as a check, and the root hook file's own test, which
        // isn't a unit and always runs.
        assert!(
            second.out().contains("4 passing"),
            "{}: {}",
            mocha.name,
            second.out()
        );
        assert!(
            hushrun_testkit::lines::says_reused(&second.stderr, 4, "tests"),
            "{}",
            second.stderr
        );
        // A change to the root hook file runs every spec file.
        r.write(
            "test/hooks.spec.js",
            "const assert = require('assert');\nbeforeEach(function () { this.base = 2; });\nit('base', function () { assert.equal(this.base, 2); });\n",
        );
        let hooks = r.run(MOCHA_RUN);
        assert_eq!(hooks.code, 3, "{}", hooks.out());
        assert!(hooks.out().contains("3 failing"), "{}", hooks.out());
        assert_eq!(
            r.tests_of("test/greet1.spec.js")["hello"],
            json!("failed"),
            "{}",
            mocha.name
        );
    }
}

// covers: C-32, C-56
#[test]
fn mocha_commands_hushrun_doesnt_observe_run_as_written_and_say_why() {
    for mocha in mochas() {
        let r = Repo::new(&mocha, &mocha_shop("reuse"));
        let parallel = r.run(&[
            "run",
            "unit",
            "--",
            "node",
            "node_modules/mocha/bin/mocha.js",
            "--parallel",
        ]);
        assert!(
            parallel.stderr.contains(
                "hushrun: unit: test files aren't observed: mocha --parallel runs spec files in worker processes"
            ),
            "{}: {}",
            mocha.name,
            parallel.stderr
        );
        // Run as written: in worker processes, a root hook is its own spec file's alone.
        assert!(parallel.out().contains("passing"), "{}", parallel.out());
        // A spec file Mocha lists and the plan doesn't: nothing is left out, and it says why.
        assert_eq!(r.run(MOCHA_RUN).code, 0);
        r.write("test/new.spec.js", "it('new', () => {});\n");
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let listed = r.run(MOCHA_RUN);
        assert_eq!(listed.code, 0, "{}", listed.out());
    }
}
