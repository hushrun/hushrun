//! \[ADR 0085\] Test files and tests with Jest (C-30 to C-35, C-39, C-44): a unit is a test file,
//! what it read is what Jest compiled and opened while it ran, with the files every test file
//! reads; each test is told on its own; and reuse never misses a failure.
//!
//! The fixtures need Node and Jest, installed from npm outside the repository: Jest 29.7.0 with
//! ts-jest and TypeScript in the `node_modules` `HUSHRUN_TEST_JEST29` names, and Jest 30 in the
//! one `HUSHRUN_TEST_JEST30` names (CI: `npm install --prefix <dir> jest@29.7.0 ts-jest@29.4.15
//! typescript@5.9.3`, and the same with `jest@30.5.2`). Each runs on every Jest it finds, says so
//! and passes where both are missing, unless `HUSHRUN_TEST_TOOLS=required`, which needs both.

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
    "HUSHRUN_JEST",
    "HUSHRUN_JEST_RUN",
    "HUSHRUN_JEST_DESELECT",
    "HUSHRUN_JEST_WATCH",
    "HUSHRUN_JEST_START",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "NODE_OPTIONS",
    "JEST_WORKER_ID",
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
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// One Jest these tests run on: its major and its `node_modules`.
#[derive(Clone)]
struct Jest {
    major: u32,
    modules: PathBuf,
}

/// The `node_modules` a variable names, when it holds a Jest of `major`.
fn jest_in(variable: &str, major: u32) -> Option<Jest> {
    let dir = PathBuf::from(std::env::var_os(variable)?);
    let package = std::fs::read(dir.join("jest/package.json")).ok()?;
    let package: Value = serde_json::from_slice(&package).ok()?;
    let version = package["version"].as_str()?;
    version.starts_with(&format!("{major}.")).then_some(Jest {
        major,
        modules: dir,
    })
}

fn has_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Every Jest this machine has for these tests; none, said, where it has neither, and a failure
/// with `HUSHRUN_TEST_TOOLS=required` where it lacks one.
fn jests() -> Vec<Jest> {
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

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
    #[allow(dead_code, reason = "the Jest a repository runs on, for messages")]
    jest: Jest,
}

impl Repo {
    fn new(jest: &Jest, files: &[(String, String)]) -> Repo {
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
            jest: jest.clone(),
        };
        for (path, text) in files {
            r.write(path, text);
        }
        // Jest as installed outside the repository, which git ignores.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&jest.modules, r.repo.join("node_modules")).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&jest.modules, r.repo.join("node_modules"))
            .expect("link");
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn remove(&self, path: &str) {
        std::fs::remove_file(self.repo.join(path)).expect("remove");
    }

    fn clean(cmd: &mut Command) {
        for var in CLEARED {
            cmd.env_remove(var);
        }
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
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
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        Repo::clean(&mut cmd);
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

    /// Jest with everything run, outside hushrun: the test files that fail.
    fn truth(&self) -> Vec<String> {
        let report = self.root.join("truth.json");
        let _ = std::fs::remove_file(&report);
        let mut cmd = Command::new("node");
        cmd.current_dir(&self.repo);
        Repo::clean(&mut cmd);
        cmd.arg("node_modules/jest/bin/jest.js")
            .args(["--ci", "--json", "--outputFile"])
            .arg(&report)
            .output()
            .expect("jest");
        let Ok(bytes) = std::fs::read(&report) else {
            return Vec::new();
        };
        let report: Value = serde_json::from_slice(&bytes).unwrap_or_default();
        report["testResults"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["status"] != "passed")
            .filter_map(|r| r["name"].as_str().map(str::to_string))
            .collect()
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

    /// The adapter's directory in this cache, once a run wrote it.
    fn adapter(&self) -> PathBuf {
        std::fs::read_dir(self.cache.join("adapters"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("jest-")
            })
            .expect("the Jest adapter was written")
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

const CONFIG: &str = "module.exports = { testEnvironment: 'node', transform: {} };\n";

/// A shop: a cart that requires pricing, each with a test file; a greeting; and five test files
/// that read only the greeting, so that a plan has something to reuse.
fn shop(tests: &str) -> Vec<(String, String)> {
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

const JEST_RUN: &[&str] = &[
    "run",
    "unit",
    "--",
    "node",
    "node_modules/jest/bin/jest.js",
    "--ci",
];

// covers: C-30, C-31, C-32, C-44
#[test]
fn jest_test_files_are_observed_from_what_jest_compiled_and_opened() {
    for jest in jests() {
        let mut files = shop("observe");
        files.push(file("src/dyn.js", "exports.dyn = () => 7;\n"));
        files.push(file("src/data.js", "exports.data = 1;\n"));
        files.push(file(
            "test/dyn.test.js",
            "const fs = require('fs');\ntest('dyn', () => {\n  const name = 'dyn';\n  expect(require(`../src/${name}`).dyn()).toBe(7);\n  expect(fs.readFileSync('src/data.js', 'utf8')).toContain('= 1');\n});\n",
        ));
        let r = Repo::new(&jest, &files);
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 0, "Jest {}: {}", jest.major, first.out());
        assert_eq!(
            first.line("hushrun: unit: with"),
            "hushrun: unit: with `tests: reuse`, all 8 test files would have run: none had passed here yet on the files it read",
            "Jest {}",
            jest.major
        );
        // What Jest compiled for it, the module a computed require named included, what it
        // opened, and Jest's config, which every test file reads.
        assert_eq!(
            r.reads("test/cart.test.js"),
            [
                "jest.config.js",
                "src/cart.js",
                "src/pricing.js",
                "test/cart.test.js"
            ],
            "Jest {}",
            jest.major
        );
        assert_eq!(
            r.reads("test/dyn.test.js"),
            [
                "jest.config.js",
                "src/data.js",
                "src/dyn.js",
                "test/dyn.test.js"
            ],
            "Jest {}",
            jest.major
        );
        let map = r.map();
        assert_eq!(map["unitFiles"].as_array().unwrap().len(), 8, "{map:#}");
        // One source changes: what read it would run, and the rest would be reused.
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let changed = r.run(JEST_RUN);
        assert_eq!(changed.code, 0, "{}", changed.out());
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("hushrun: unit: with")).starts_with(
                "hushrun: unit: with `tests: reuse`, 2 of 8 test files would have run; 6 already passed on the files they read"
            ),
            "Jest {}: {}",
            jest.major,
            changed.stderr
        );
    }
}

// covers: C-34, C-44
#[test]
fn jest_reuses_test_files_that_already_passed() {
    for jest in jests() {
        let r = Repo::new(&jest, &shop("reuse"));
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed Jest's test files yet"
        );
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let second = r.run(JEST_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        assert_eq!(
            second.line("hushrun: unit: 3"),
            "hushrun: unit: 3 of 7 test files run; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "Jest {}",
            jest.major
        );
        assert!(
            second.stderr.contains("Test Suites: 3 passed, 3 total"),
            "Jest {}: {}",
            jest.major,
            second.stderr
        );
        // Its saving, or under a second, the count.
        assert!(
            hushrun_testkit::lines::says_reused(&second.stderr, 4, "test files"),
            "{}",
            second.stderr
        );
        // Why, with the imports the adapter saw.
        r.write("src/pricing.js", "exports.price = (x) => 2 * x;\n");
        let why = r.run(&["why", "unit", "test/cart.test.js"]);
        assert_eq!(why.code, 0, "{}", why.stderr);
        assert!(
            why.stdout.starts_with(
                "test/cart.test.js runs: it read src/pricing.js, which changed\n  test/cart.test.js imports src/cart.js imports src/pricing.js\n  as far as this machine knows: Jest's list"
            ),
            "{}",
            why.stdout
        );
        // Every test file left after Jest's own choice already passed: the run passes, as the
        // run of those files did, where Jest would say it found no test.
        let only = r.run(&[
            "run",
            "unit",
            "--",
            "node",
            "node_modules/jest/bin/jest.js",
            "--ci",
            "greet",
        ]);
        assert_eq!(only.code, 0, "{}", only.out());
    }
}

/// A test file whose tests fail while FAIL is set, and one whose `ok` fails while HIDDEN is,
/// which no file says, as a missed dependency would.
fn flips() -> Vec<(String, String)> {
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

// covers: C-39, C-44
#[test]
fn a_test_that_failed_runs_alone_and_its_files_others_are_reused_one_by_one() {
    for jest in jests() {
        let r = Repo::new(&jest, &flips());
        let first = r.run_env(JEST_RUN, &[("FAIL", "1")]);
        assert_eq!(first.code, 1, "{}", first.out());
        let map = r.map();
        assert_eq!(
            map["units"]["test/cart.test.js"]["tests"],
            json!({"cart > one": "passed", "cart > two": "passed", "cart > flips": "failed",
                   "cart > same": "passed", "cart > same [2]": "passed", "skipped": "not-run"}),
            "Jest {}: {map:#}",
            jest.major
        );
        // The flip fixed: each file's own key failed, so their tests are asked one by one; of the
        // six that passed, one runs as a check; the skipped test, which a run that failed recorded
        // no pass for (C-32), is asked again.
        let second = r.run(JEST_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        assert_eq!(
            second.line("hushrun: unit: "),
            "hushrun: unit: 4 of 8 tests run, in 2 of 2 test files; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "Jest {}",
            jest.major
        );
        // Jest left out what the plan listed, before any of their hooks, and says so, beside the
        // test the file skips itself: whichever of the five the audit picks (by its key and the
        // run's start), four are left out and the skipped test runs. Before a run that failed
        // stopped recording a skip as a pass, the skipped test was a sixth that could be picked,
        // and Jest then said 6.
        assert!(
            second.stderr.contains("5 skipped"),
            "Jest {}: {}",
            jest.major,
            second.stderr
        );
        assert_eq!(
            r.map()["units"]["test/cart.test.js"]["tests"]["skipped"],
            "passed",
            "Jest {}: the skipped test ran again",
            jest.major
        );
        let result = store_result(&r);
        assert_eq!(result["units"]["tests"]["reused"], 4, "{result:#}");
        // The same code again: the check skips whole.
        let again = r.run(JEST_RUN);
        assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
    }
}

/// Each test's state the store holds for `unit`, by id.
fn tests_of(r: &Repo, unit: &str) -> serde_json::Map<String, Value> {
    r.map()["units"][unit]["tests"]
        .as_object()
        .cloned()
        .unwrap_or_default()
}

// covers: C-39, C-44
#[test]
fn a_failing_after_all_or_an_error_no_test_owns_leaves_no_test_of_its_file_passed() {
    // Each file has a test that always fails, so that its failure is never "unexplained": the
    // adapter itself must charge the tests under a failing afterAll, and every test of a file in
    // which an error arrived that no test can be charged with (a timer one test left throws while
    // another runs: Jest 29 fails the one running, Jest 30 the one that left it).
    let mut files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file(
            "test/hooks.test.js",
            "describe('blk', () => {\n  afterAll(() => { throw new Error('block teardown'); });\n  test('one', () => {});\n  test('two', () => {});\n});\ntest('bad', () => { expect(1).toBe(2); });\n",
        ),
        file(
            "test/root.test.js",
            "afterAll(() => { throw new Error('root teardown'); });\ntest('r1', () => {});\ntest('r2', () => {});\ntest('bad', () => { expect(1).toBe(2); });\n",
        ),
        file(
            "test/leak.test.js",
            "test('leaks', () => { setTimeout(() => { throw new Error('late timer'); }, 20); });\ntest('waits', async () => { await new Promise((r) => setTimeout(r, 150)); });\ntest('after', () => {});\ntest('bad', () => { expect(1).toBe(2); });\n",
        ),
    ];
    files.push(file("test/fine.test.js", "test('fine', () => {});\n"));
    for jest in jests() {
        let r = Repo::new(&jest, &files);
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 1, "{}", first.out());
        let failed = |id: &str| (id.to_string(), json!("failed"));
        assert_eq!(
            tests_of(&r, "test/hooks.test.js"),
            [failed("blk > one"), failed("blk > two"), failed("bad")]
                .into_iter()
                .collect(),
            "Jest {}",
            jest.major
        );
        assert_eq!(
            tests_of(&r, "test/root.test.js"),
            [failed("r1"), failed("r2"), failed("bad")]
                .into_iter()
                .collect(),
            "Jest {}",
            jest.major
        );
        let leak = tests_of(&r, "test/leak.test.js");
        assert_eq!(leak.len(), 4, "{leak:?}");
        assert!(
            leak.values().all(|state| state != "passed"),
            "Jest {}: {leak:?}",
            jest.major
        );
        // The same code again: every one of them runs, and the teardowns and the timer fail again.
        let second = r.run(JEST_RUN);
        assert_eq!(second.code, 1, "{}", second.out());
        for said in ["block teardown", "root teardown", "late timer"] {
            assert!(
                second.out().contains(said),
                "Jest {}: {said}:\n{}",
                jest.major,
                second.out()
            );
        }
        assert_eq!(
            tests_of(&r, "test/hooks.test.js")["blk > one"],
            "failed",
            "Jest {}",
            jest.major
        );
    }
}

// covers: C-32, C-39, C-44
#[test]
fn a_skip_in_a_jest_run_that_failed_records_nothing() {
    // A skip says the body didn't run: in a run that failed that may be the failure's doing, so a
    // skipped test and its file record no pass; in a run that passed, a skip is a pass.
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file(
            "test/a.test.js",
            "test.skip('skipped', () => {});\ndescribe.skip('box', () => { test('inner', () => {}); });\ntest.todo('later');\ntest('ok', () => {});\ntest('bad', () => { expect(process.env.FIXED).toBeTruthy(); });\n",
        ),
    ];
    for jest in jests() {
        let r = Repo::new(&jest, &files);
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 1, "{}", first.out());
        assert_eq!(
            Value::Object(tests_of(&r, "test/a.test.js")),
            json!({"skipped": "not-run", "box > inner": "not-run", "later": "not-run",
                   "ok": "passed", "bad": "failed"}),
            "Jest {}",
            jest.major
        );
        let second = r.run_env(JEST_RUN, &[("FIXED", "1")]);
        assert_eq!(second.code, 0, "{}", second.out());
        assert_eq!(
            Value::Object(tests_of(&r, "test/a.test.js")),
            json!({"skipped": "passed", "box > inner": "passed", "later": "passed",
                   "ok": "passed", "bad": "passed"}),
            "Jest {}",
            jest.major
        );
    }
}

// covers: C-37, C-39, C-44
#[test]
fn why_says_a_jest_test_kept_nothing_after_an_error_no_test_owns() {
    // A timer `leaks` left throws while `waits` runs: Jest 29 charges it to `waits`, Jest 30 to
    // `leaks`; either way no test of the file keeps a result, and `hushrun why` says what happened.
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file(
            "test/leak.test.js",
            "test('leaks', () => { setTimeout(() => { throw new Error('late timer'); }, 20); });\ntest('waits', async () => { await new Promise((r) => setTimeout(r, 150)); });\ntest('after', () => {});\n",
        ),
    ];
    for jest in jests() {
        let r = Repo::new(&jest, &files);
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 1, "{}", first.out());
        let why = r.run(&["why", "unit", "test/leak.test.js::after"]);
        assert!(
            why.out().contains(
                "Jest reported an error it couldn't charge to a test (late timer), so no test of its file keeps a result of that run"
            ),
            "Jest {}: {}",
            jest.major,
            why.out()
        );
    }
}

// covers: C-39, C-44
#[test]
fn tests_whose_ids_collide_are_never_left_out_after_a_change() {
    // `x > y` twice, from two nestings: one id, which no plan lists. The file passes whole and
    // stores them as passed; a change to what only `z` read then asks the file's tests one by
    // one, and both still run.
    for jest in jests() {
        let r = Repo::new(
            &jest,
            &[
                file(
                    "hushrun.yaml",
                    &yaml("{mode: reuse, unit: test, reads: test}"),
                ),
                file(".gitignore", "node_modules\n"),
                file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
                file("jest.config.js", CONFIG),
                file("fx.js", "module.exports = 1;\n"),
                file(
                    "test/c.test.js",
                    "const fs = require('fs');\nconst log = (n) => fs.appendFileSync(process.env.RAN_LOG, n + '\\n');\ndescribe('x', () => { test('y', () => { log('nested'); }); });\ntest('x > y', () => { log('flat'); });\ntest('z', () => { expect(fs.readFileSync('fx.js', 'utf8')).toContain('1'); });\n",
                ),
                file("test/other.test.js", "test('other', () => {});\n"),
            ],
        );
        let log = r.root.join("ran.log");
        let log_text = log.display().to_string();
        assert_eq!(r.run_env(JEST_RUN, &[("RAN_LOG", &log_text)]).code, 0);
        r.write("fx.js", "module.exports = 1; // touched\n");
        let _ = std::fs::remove_file(&log);
        let second = r.run_env(JEST_RUN, &[("RAN_LOG", &log_text)]);
        assert_eq!(second.code, 0, "{}", second.out());
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            ran.contains("nested") && ran.contains("flat"),
            "Jest {}: {ran:?}\n{}",
            jest.major,
            second.stderr
        );
    }
}

/// The newest run result this cache holds.
fn store_result(r: &Repo) -> Value {
    let mut found = Vec::new();
    collect(&r.cache.join("results/results/v0"), &mut found);
    found.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
    let newest = found.last().expect("a run result");
    serde_json::from_slice(&std::fs::read(newest).unwrap()).unwrap()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

// covers: C-39, C-34, C-44
#[test]
fn an_audited_test_that_fails_again_alone_suspends_the_checks_reuse() {
    for jest in jests() {
        // test/two.test.js's `ok` is the one test that could be reused: it runs as a check, and
        // fails. Its second run is COMMAND again with a directory of its own, `also` listed:
        // what the adapter does there comes from that directory alone.
        let mut files = flips();
        files.retain(|(p, _)| p != "test/cart.test.js");
        let r = Repo::new(&jest, &files);
        assert_eq!(r.run_env(JEST_RUN, &[("FAIL", "1")]).code, 1);
        let caught = r.run_env(JEST_RUN, &[("HIDDEN", "1")]);
        assert_eq!(caught.code, 1, "{}", caught.out());
        assert_eq!(
            caught.line("hushrun: unit: all"),
            "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs anyway, as a check"
        );
        assert!(
            caught.stderr.contains(
                "hushrun: unit: test/two.test.js::ok failed, though it passed before on the same files it read: running it once more"
            ),
            "{}",
            caught.stderr
        );
        // The second run ran `ok` alone: Jest left `also` out.
        assert!(
            caught
                .stderr
                .contains("Tests:       1 failed, 1 skipped, 2 total"),
            "Jest {}: {}",
            jest.major,
            caught.stderr
        );
        assert!(
            caught.stderr.contains(
                "hushrun: unit: test/two.test.js::ok failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
            ),
            "{}",
            caught.stderr
        );
        assert_eq!(store::disagreements(&r.cache), ["test/two.test.js::ok"]);
    }
}

// covers: C-34, C-44
#[test]
fn an_injected_missed_dependency_in_jest_is_caught_by_an_audit() {
    for jest in jests() {
        // A test file that fails when a file outside the check's files exists, named by a
        // variable: what no adapter sees. With one other test file, it is the one an audit runs.
        let files = vec![
            file("hushrun.yaml", &yaml("reuse")),
            file(".gitignore", "node_modules\n"),
            file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
            file("jest.config.js", CONFIG),
            file("src/other.js", "exports.other = 1;\n"),
            file(
                "test/hidden.test.js",
                "const fs = require('fs');\ntest('hidden', () => {\n  let there = true;\n  try { fs.lstatSync(process.env.FLAG_FILE || ''); } catch { there = false; }\n  expect(there).toBe(false);\n});\n",
            ),
            file(
                "test/other.test.js",
                "const { other } = require('../src/other');\ntest('other', () => { expect(other).toBe(1); });\n",
            ),
        ];
        let r = Repo::new(&jest, &files);
        let flag = r.root.join("flag");
        let flag = flag.to_str().unwrap().to_string();
        let env = [("FLAG_FILE", flag.as_str())];
        assert_eq!(r.run_env(JEST_RUN, &env).code, 0);
        std::fs::write(&flag, "").unwrap();
        r.write("src/other.js", "exports.other = 2 - 1;\n");
        let caught = r.run_env(JEST_RUN, &env);
        assert_eq!(caught.code, 1, "{}", caught.out());
        assert!(
            caught.stderr.contains("unit: test/hidden.test.js failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"),
            "{}",
            caught.stderr
        );
    }
}

// covers: C-32, C-44
#[test]
fn jest_commands_hushrun_doesnt_observe_run_as_written_and_say_why() {
    for jest in jests() {
        let r = Repo::new(&jest, &shop("reuse"));
        for (args, why) in [
            (
                &["--listTests"][..],
                "test files aren't observed: jest --listTests isn't one run of the test files",
            ),
            (
                &["--onlyChanged"][..],
                "test files aren't observed: jest --onlyChanged chooses test files from git",
            ),
            (
                &["--onlyFailures"][..],
                "test files aren't observed: jest --onlyFailures chooses test files from an earlier run",
            ),
        ] {
            let mut command = vec!["run", "unit", "--", "node", "node_modules/jest/bin/jest.js"];
            command.extend(args);
            let ran = r.run(&command);
            assert!(
                ran.stderr.contains(&format!("hushrun: unit: {why}")),
                "Jest {} {args:?}: {}",
                jest.major,
                ran.stderr
            );
        }
        // Coverage: everything runs and is observed, nothing is left out.
        assert_eq!(r.run(JEST_RUN).code, 0);
        let covered = r.run(&[
            "run",
            "unit",
            "--",
            "node",
            "node_modules/jest/bin/jest.js",
            "--ci",
            "--coverage",
        ]);
        assert_eq!(covered.code, 0, "{}", covered.out());
        assert!(
            covered.stderr.contains(
                "hushrun: unit: every test file runs: Jest's coverage is on, and a partial run's coverage would read as a drop"
            ),
            "{}",
            covered.stderr
        );
        // Coverage from the config: the adapter leaves nothing out, and says why.
        r.write(
            "jest.config.js",
            "module.exports = { testEnvironment: 'node', transform: {}, collectCoverage: true };\n",
        );
        assert_eq!(r.run(JEST_RUN).code, 0);
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let config = r.run(JEST_RUN);
        assert_eq!(config.code, 0, "{}", config.out());
        assert!(
            config.stderr.contains(
                "hushrun: unit: no test file was reused: Jest's coverage is on, and a partial run's coverage would read as a drop"
            ),
            "Jest {}: {}",
            jest.major,
            config.stderr
        );
        assert!(
            config.stderr.contains("Test Suites: 7 passed, 7 total"),
            "{}",
            config.stderr
        );
    }
}

// covers: C-32, C-44
#[cfg(unix)]
#[test]
fn a_script_that_replaces_node_options_is_said_with_the_fix_and_the_config_line_loads_it() {
    use std::os::unix::fs::PermissionsExt;
    for jest in jests() {
        let mut files = shop("observe");
        files.push(file(
            "bin/jest",
            "#!/bin/sh\nNODE_OPTIONS=--no-warnings exec node node_modules/jest/bin/jest.js \"$@\"\n",
        ));
        let r = Repo::new(&jest, &files);
        let script = r.repo.join("bin/jest");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = ["run", "unit", "--", "bin/jest", "--ci"];
        let lost = r.run(&command);
        assert_eq!(lost.code, 0, "{}", lost.out());
        assert!(
            lost.stderr.contains("hushrun: unit: test files weren't observed: Jest wrote nothing for hushrun: its processes didn't load hushrun's adapter, as when a script sets NODE_OPTIONS instead of adding to it (`NODE_OPTIONS=--experimental-vm-modules jest`); keep what it had (`NODE_OPTIONS=\"$NODE_OPTIONS --experimental-vm-modules\" jest`), or put this line first in jest.config.js: `if (process.env.HUSHRUN_JEST) require(process.env.HUSHRUN_JEST);`"),
            "Jest {}: {}",
            jest.major,
            lost.stderr
        );
        // The line, first in the config: the run is observed through it, workers included.
        r.write(
            "jest.config.js",
            &format!("if (process.env.HUSHRUN_JEST) require(process.env.HUSHRUN_JEST);\n{CONFIG}"),
        );
        let loaded = r.run(&command);
        assert_eq!(loaded.code, 0, "{}", loaded.out());
        assert_eq!(
            loaded.line("hushrun: unit: with"),
            "hushrun: unit: with `tests: reuse`, all 7 test files would have run: none had passed here yet on the files it read",
            "Jest {}",
            jest.major
        );
        assert_eq!(
            r.reads("test/cart.test.js"),
            [
                "jest.config.js",
                "src/cart.js",
                "src/pricing.js",
                "test/cart.test.js"
            ]
        );
        // The form proposed before 0.14 still loads it: whoever added it isn't told it's missing.
        r.write(
            "jest.config.js",
            &format!("process.env.HUSHRUN_JEST && require(process.env.HUSHRUN_JEST);\n{CONFIG}"),
        );
        let before = r.run(&command);
        assert_eq!(before.code, 0, "{}", before.out());
        assert!(
            !before.stderr.contains("weren't observed"),
            "Jest {}: {}",
            jest.major,
            before.stderr
        );
        // An ES module config takes the other form, in band and with workers alike.
        r.write("package.json", "{\"name\": \"shop\", \"private\": true}\n");
        r.remove("jest.config.js");
        r.write(
            "jest.config.mjs",
            "import { createRequire } from 'node:module'; if (process.env.HUSHRUN_JEST) createRequire(import.meta.url)(process.env.HUSHRUN_JEST);\nexport default { testEnvironment: 'node', transform: {} };\n",
        );
        let esm = r.run(&["run", "unit", "--", "bin/jest", "--ci", "-i"]);
        assert_eq!(esm.code, 0, "{}", esm.out());
        assert!(
            !esm.stderr.contains("weren't observed"),
            "Jest {}: {}",
            jest.major,
            esm.stderr
        );
    }
}

/// Jest with hushrun's adapter and a `hushrun.json` written here, as a plan would: its exit code,
/// its output and the facts files it wrote.
fn adapter_jest(r: &Repo, run: Value, args: &[&str]) -> (i32, String, Vec<Value>) {
    adapter_jest_env(r, run, args, &[])
}

fn adapter_jest_env(
    r: &Repo,
    run: Value,
    args: &[&str],
    env: &[(&str, &str)],
) -> (i32, String, Vec<Value>) {
    let units = r.root.join("units");
    let _ = std::fs::remove_dir_all(&units);
    std::fs::create_dir_all(&units).unwrap();
    let mut run = run;
    run["schema"] = json!("hushrun.units/v0");
    run["root"] = json!(r.repo);
    std::fs::write(units.join("hushrun.json"), run.to_string()).unwrap();
    let preload = r.adapter().join("hushrun-jest.cjs");
    let mut cmd = Command::new("node");
    Repo::clean(&mut cmd);
    let out = cmd
        .arg("node_modules/jest/bin/jest.js")
        .arg("--ci")
        .args(args)
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", &units)
        .env("NODE_OPTIONS", format!("--require {}", preload.display()))
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let mut facts: Vec<Value> = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.starts_with("jest") && name.ends_with(".json")
        })
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect();
    facts.sort_by_key(|f| f["adapter"].as_str().unwrap_or("").to_string());
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        facts,
    )
}

fn main_of(facts: &[Value]) -> &Value {
    facts
        .iter()
        .find(|f| f["adapter"] == "jest-main")
        .expect("the main process's facts")
}

/// The worker's facts of `unit`, with its paths table resolved in each list.
fn unit_of(facts: &[Value], unit: &str) -> Value {
    let file = facts
        .iter()
        .find(|f| f["adapter"] == "jest" && f["units"][0]["unit"] == unit)
        .unwrap_or_else(|| panic!("no facts of {unit} in {facts:#?}"));
    let paths: Vec<String> = file["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    let names = |v: &Value| -> Value {
        let mut out: Vec<String> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| paths[i.as_u64().unwrap() as usize].clone())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        json!(out)
    };
    let mut u = file["units"][0].clone();
    for t in u["tests"].as_array_mut().into_iter().flatten() {
        for k in ["ran", "opened", "dirs"] {
            if t.get(k).is_some() {
                t[k] = names(&t[k]);
            }
        }
    }
    if let Some(loaded) = u.get_mut("loaded") {
        loaded["ran"] = names(&loaded["ran"]);
        loaded["opened"] = names(&loaded["opened"]);
    }
    for s in u["shared"].as_array_mut().into_iter().flatten() {
        s["ran"] = names(&s["ran"]);
        s["opened"] = names(&s["opened"]);
    }
    let mut imports = serde_json::Map::new();
    for (from, to) in file["imports"].as_object().into_iter().flatten() {
        imports.insert(paths[from.parse::<usize>().unwrap()].clone(), names(to));
    }
    u["imports"] = Value::Object(imports);
    let mut opens = serde_json::Map::new();
    for (from, to) in file["opens"].as_object().into_iter().flatten() {
        opens.insert(paths[from.parse::<usize>().unwrap()].clone(), names(to));
    }
    u["opens"] = Value::Object(opens);
    if u.get("everyTest").is_some() {
        u["everyTest"] = names(&u["everyTest"]);
    }
    u["settings"] = file["settings"].clone();
    u
}

fn test_of<'a>(unit: &'a Value, id: &str) -> &'a Value {
    unit["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap_or_else(|| panic!("no test {id} in {unit:#}"))
}

fn listed(r: &Repo) -> Vec<String> {
    r.map()["unitFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

// covers: C-32, C-44
#[test]
fn the_adapter_leaves_out_what_it_is_told_and_no_tests_found_stays_jests_own() {
    for jest in jests() {
        let r = Repo::new(&jest, &shop("observe"));
        assert_eq!(r.run(JEST_RUN).code, 0);
        let every = listed(&r);
        // Every test file Jest chose was reused: Jest's "no tests found" would turn a pass into a
        // fail, so the run passes; the main process says what it left out.
        let (code, out, facts) = adapter_jest(&r, json!({"reuse": every, "testFiles": every}), &[]);
        assert_eq!(code, 0, "{out}");
        let main = main_of(&facts);
        assert_eq!(main["removedItems"], 7, "{main:#}");
        assert!(
            main["units"]
                .as_array()
                .unwrap()
                .iter()
                .all(|u| u["deselected"] == true)
        );
        // A filter, then one left: still a pass.
        let (code, out, _) =
            adapter_jest(&r, json!({"reuse": every, "testFiles": every}), &["cart"]);
        assert_eq!(code, 0, "{out}");
        // A filter that matches nothing stays Jest's own failure.
        let (code, _, _) = adapter_jest(
            &r,
            json!({"reuse": every, "testFiles": every}),
            &["no-such-test"],
        );
        assert_eq!(code, 1);
        // What isn't on the list runs, and its failure is the run's.
        r.write("src/greet.js", "exports.hello = () => 'ho';\n");
        let (code, _, facts) = adapter_jest(
            &r,
            json!({"reuse": ["test/cart.test.js"], "testFiles": every}),
            &[],
        );
        assert_eq!(code, 1);
        assert_eq!(main_of(&facts)["removedItems"], 1);
        // Jest's list isn't the plan's: nothing is left out, and the main process says why.
        let mut fewer = every.clone();
        fewer.pop();
        let (code, _, facts) = adapter_jest(
            &r,
            json!({"reuse": ["test/cart.test.js"], "testFiles": fewer, "unit": "test",
                   "reuseTests": {"test/pricing.test.js": ["price"]}}),
            &[],
        );
        assert_eq!(code, 1);
        let main = main_of(&facts);
        assert_eq!(main["removedItems"], 0);
        assert_eq!(
            main["settings"]["refused"],
            "Jest's test files aren't the ones hushrun planned from"
        );
        assert_eq!(
            test_of(&unit_of(&facts, "test/pricing.test.js"), "price")["deselected"],
            false
        );
        // With --shard, each shard leaves out of its own share.
        r.write("src/greet.js", "exports.hello = () => 'hi';\n");
        for shard in ["1/2", "2/2"] {
            let (code, out, _) = adapter_jest(
                &r,
                json!({"reuse": every, "testFiles": every}),
                &[&format!("--shard={shard}")],
            );
            assert_eq!(code, 0, "{shard}: {out}");
        }
    }
}

// covers: C-39, C-44
#[test]
fn each_test_is_told_with_its_id_and_outcome_and_left_out_before_its_hooks() {
    let source = "\
let hooks = 0;
describe('a', () => {
  beforeAll(() => { hooks += 1; });
  test('one', () => { expect(1).toBe(1); });
  test('one', () => { expect(2).toBe(2); });
  test.each([[1, 2], [2, 4]])('double %i', (x, y) => { expect(x * 2).toBe(y); });
  describe('b', () => {
    beforeAll(() => { if (process.env.HOOK) throw new Error('hook'); });
    test('deep', () => { expect(hooks).toBe(1); });
  });
});
describe('a > b', () => { test('deep', () => { expect(1).toBe(1); }); });
test('x [2]', () => {});
test('x', () => {});
test('x', () => {});
test.skip('skipped', () => { throw new Error('never'); });
test.todo('later');
test('retried', () => {
  globalThis.tries = (globalThis.tries || 0) + 1;
  expect(globalThis.tries).toBe(2);
});
";
    for jest in jests() {
        let mut files = shop("observe");
        files.push(file(
            "test/ids.test.js",
            &format!("jest.retryTimes(2);\n{source}"),
        ));
        let r = Repo::new(&jest, &files);
        assert_eq!(r.run(JEST_RUN).code, 0);
        let every = listed(&r);
        let (code, out, facts) = adapter_jest(&r, json!({"unit": "test", "testFiles": every}), &[]);
        assert_eq!(code, 0, "{out}");
        let unit = unit_of(&facts, "test/ids.test.js");
        let ids: Vec<&str> = unit["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "a > one",
                "a > one [2]",
                "a > double 1",
                "a > double 2",
                "a > b > deep",
                "a > b > deep [2]",
                "x [2]",
                "x",
                "skipped",
                "later",
                "retried"
            ],
            "Jest {}",
            jest.major
        );
        // Tests whose ids collide are told as one, never left out, with no list of their own:
        // `x [2]` and the second `x`; `a > b > deep` and `a > b > deep [2]`, two nestings alike.
        for id in ["x [2]", "a > b > deep", "a > b > deep [2]"] {
            assert_eq!(test_of(&unit, id)["neverLeftOut"], true, "{id}");
        }
        assert!(test_of(&unit, "x").get("neverLeftOut").is_none());
        assert_eq!(test_of(&unit, "x [2]")["finished"], true);
        // Skipped by its own code, it passed; a retry is counted.
        assert_eq!(test_of(&unit, "skipped")["finished"], true);
        assert_eq!(test_of(&unit, "later")["finished"], true);
        assert_eq!(test_of(&unit, "retried")["attempts"], 2);
        assert_eq!(test_of(&unit, "retried")["failed"], false);
        // Left out as told: never an id that collides, or that two nestings make; the rest before
        // any of their hooks, so `a`'s beforeAll ran once, for `deep`.
        let (code, out, facts) = adapter_jest(
            &r,
            json!({"unit": "test", "testFiles": every, "reuseTests": {"test/ids.test.js":
                ["a > one", "a > double 1", "x [2]", "a > b > deep [2]", "skipped"]}}),
            &[],
        );
        assert_eq!(code, 0, "{out}");
        let unit = unit_of(&facts, "test/ids.test.js");
        let deselected: Vec<&str> = unit["tests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["deselected"] == true)
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            deselected,
            ["a > one", "a > double 1", "skipped"],
            "Jest {}",
            jest.major
        );
        assert_eq!(test_of(&unit, "a > b > deep")["failed"], false);
        // --testNamePattern and a focus leave tests that didn't run: not finished.
        let (_, _, facts) = adapter_jest(
            &r,
            json!({"unit": "test", "testFiles": every}),
            &["-t", "double", "test/ids"],
        );
        let unit = unit_of(&facts, "test/ids.test.js");
        assert_eq!(test_of(&unit, "a > double 1")["finished"], true);
        assert_eq!(test_of(&unit, "a > one")["finished"], false);
        assert_eq!(test_of(&unit, "skipped")["finished"], true);
        // A failing beforeAll fails the file, and the tests under it.
        let (code, _, facts) = adapter_jest_env(
            &r,
            json!({"unit": "test", "testFiles": every}),
            &["test/ids"],
            &[("HOOK", "1")],
        );
        assert_eq!(code, 1);
        let unit = unit_of(&facts, "test/ids.test.js");
        assert_eq!(unit["failed"], true, "{unit:#}");
        assert_eq!(
            test_of(&unit, "a > b > deep")["failed"],
            true,
            "Jest {}",
            jest.major
        );
        assert_eq!(test_of(&unit, "a > one")["failed"], false);
    }
}

// covers: C-39, C-44
#[test]
fn with_reads_test_each_test_says_what_it_ran_opened_and_imported() {
    for jest in jests() {
        let files = vec![
            file(
                "hushrun.yaml",
                &yaml("{mode: observe, unit: test, reads: test}"),
            ),
            file(".gitignore", "node_modules\ncoverage\n"),
            file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
            file(
                "jest.config.js",
                "module.exports = { testEnvironment: 'node', transform: {}, setupFiles: ['./test/setup.js'] };\n",
            ),
            file(
                "test/setup.js",
                "globalThis.LEVEL = require('../src/level').level;\n",
            ),
            file("src/level.js", "exports.level = 1;\n"),
            file("src/helper.js", "exports.stamp = () => 'stamp';\n"),
            file(
                "src/db.js",
                "const { stamp } = require('./helper');\nexports.connect = () => ({ stamp: stamp() });\nexports.query = (q) => `rows for ${q}`;\n",
            ),
            file("src/names.js", "exports.names = () => ['ann'];\n"),
            file("src/lazy.js", "exports.lazy = () => 'lazy';\n"),
            file("data/fixture.json", "{\"n\": 3}\n"),
            file(
                "test/db.test.js",
                "const fs = require('fs');\nconst { execFileSync } = require('child_process');\nconst db = require('../src/db');\nconst { names } = require('../src/names');\nconst all = names();\nlet conn;\nbeforeAll(() => { conn = db.connect(); });\ndescribe('db', () => {\n  test('connected', () => { expect(conn.stamp).toBe('stamp'); });\n  test('queries', () => { expect(db.query('x')).toBe('rows for x'); });\n  test('reads', () => { expect(JSON.parse(fs.readFileSync('data/fixture.json', 'utf8')).n).toBe(3); });\n  test('lazily', () => { expect(require('../src/lazy').lazy()).toBe('lazy'); });\n  test('starts', () => { expect(execFileSync(process.execPath, ['-e', 'process.stdout.write(\"x\")']).toString()).toBe('x'); });\n  test('level', () => { expect(LEVEL + all.length).toBe(2); });\n});\n",
            ),
            file(
                "test/together.test.js",
                "const db = require('../src/db');\ntest.concurrent('together', async () => { expect(db.query('y')).toBe('rows for y'); });\ntest('alone', () => { expect(1).toBe(1); });\n",
            ),
        ];
        let r = Repo::new(&jest, &files);
        assert_eq!(r.run(JEST_RUN).code, 0);
        let every = listed(&r);
        let run = json!({"unit": "test", "reads": "test", "testFiles": every});
        for band in [&["-i"][..], &[][..]] {
            let (code, out, facts) = adapter_jest(&r, run.clone(), band);
            assert_eq!(code, 0, "{out}");
            let unit = unit_of(&facts, "test/db.test.js");
            assert_eq!(
                unit["settings"]["watched"], true,
                "Jest {}: {unit:#}",
                jest.major
            );
            // A function called in the window: its file. A module's top level running because it
            // was required isn't, but a module first required in the window is.
            assert_eq!(
                test_of(&unit, "db > queries")["ran"],
                json!(["src/db.js", "test/db.test.js"])
            );
            assert_eq!(
                test_of(&unit, "db > connected")["ran"],
                json!(["test/db.test.js"])
            );
            assert_eq!(
                test_of(&unit, "db > reads")["opened"],
                json!(["data/fixture.json"])
            );
            assert_eq!(
                test_of(&unit, "db > lazily")["ran"],
                json!(["src/lazy.js", "test/db.test.js"])
            );
            // What it couldn't follow inside its window is its own.
            assert_eq!(
                test_of(&unit, "db > starts")["notFollowed"][0]["why"],
                "subprocess"
            );
            assert_eq!(test_of(&unit, "db > queries")["notFollowed"], json!([]));
            assert_eq!(unit["notFollowed"], json!([]));
            // A concurrent test can't be watched alone: it gets its file's list.
            let together = unit_of(&facts, "test/together.test.js");
            assert!(
                test_of(&together, "together").get("ran").is_none(),
                "{together:#}"
            );
            assert!(
                test_of(&together, "alone").get("ran").is_some(),
                "{together:#}"
            );
            // The beforeAll serves every test under it; loading the file ran the setup file and
            // what collection called.
            let shared = &unit["shared"][0];
            assert_eq!(
                shared["ran"],
                json!(["src/db.js", "src/helper.js", "test/db.test.js"])
            );
            assert_eq!(shared["tests"].as_array().unwrap().len(), 6, "{shared:#}");
            assert!(
                unit["loaded"]["ran"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("src/names.js")),
                "{unit:#}"
            );
            // What the runner loads for every test: setup files, and Jest's config.
            for every in ["jest.config.js", "src/level.js", "test/setup.js"] {
                assert!(
                    unit["everyTest"]
                        .as_array()
                        .unwrap()
                        .contains(&json!(every)),
                    "{every}: {unit:#}"
                );
            }
            assert!(
                !unit["loaded"]["ran"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("test/setup.js"))
            );
            // Each file's direct imports.
            assert_eq!(
                unit["imports"]["test/db.test.js"],
                json!(["src/db.js", "src/lazy.js", "src/names.js"])
            );
            assert_eq!(unit["imports"]["src/db.js"], json!(["src/helper.js"]));
        }
        // Jest's own V8 coverage takes the same counters: nothing is watched.
        let (code, out, facts) =
            adapter_jest(&r, run.clone(), &["--coverage", "--coverageProvider=v8"]);
        assert_eq!(code, 0, "{out}");
        let unit = unit_of(&facts, "test/db.test.js");
        assert_eq!(unit["settings"]["watched"], false);
        assert_eq!(
            unit["settings"]["watch"],
            "Jest's own V8 coverage is on, and it takes the counters watching a test needs"
        );
        assert!(test_of(&unit, "db > queries").get("ran").is_none());
        assert!(out.contains("db.js"), "coverage still reports: {out}");
        // With `reads: file`, nothing is watched.
        let (_, _, facts) = adapter_jest(
            &r,
            json!({"unit": "test", "reads": "file", "testFiles": every}),
            &[],
        );
        assert_eq!(
            unit_of(&facts, "test/db.test.js")["settings"]["watched"],
            false
        );
    }
}

/// The lines `observe` says for test files reuse would have reused though they failed.
fn missed(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| l.ends_with("with `tests: reuse` it would have been reused"))
        .map(str::to_string)
        .collect()
}

fn would_reuse(stderr: &str) -> u64 {
    stderr
        .lines()
        .find(|l| l.contains("with `tests: reuse`,") && l.contains(" already passed"))
        .and_then(|l| l.split("; ").nth(1))
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// What a hazard does to a repository.
struct Hazard {
    name: &'static str,
    files: Vec<(String, String)>,
    change: fn(&Repo),
    /// The command, after `hushrun run unit --`, when it isn't Jest with `--ci`.
    command: Option<&'static [&'static str]>,
}

fn hazard(name: &'static str, files: Vec<(String, String)>, change: fn(&Repo)) -> Hazard {
    Hazard {
        name,
        files,
        change,
        command: None,
    }
}

fn hazards() -> Vec<Hazard> {
    let config = |more: &str| {
        format!("module.exports = {{ testEnvironment: 'node', transform: {{}}, {more} }};\n")
    };
    vec![
        hazard(
            "a computed require",
            vec![
                file("src/dyn.js", "exports.dyn = () => 7;\n"),
                file(
                    "test/dyn.test.js",
                    "test('dyn', () => { const name = 'dyn'; expect(require(`../src/${name}`).dyn()).toBe(7); });\n",
                ),
            ],
            |r| r.write("src/dyn.js", "exports.dyn = () => 8;\n"),
        ),
        hazard(
            "a source read as data",
            vec![
                file("src/data.js", "exports.data = 1;\n"),
                file(
                    "test/data.test.js",
                    "const fs = require('fs');\ntest('data', () => { expect(fs.readFileSync('src/data.js', 'utf8')).toContain('= 1'); });\n",
                ),
            ],
            |r| r.write("src/data.js", "exports.data = 2;\n"),
        ),
        hazard(
            "a source read only by a child process",
            vec![
                file("src/data.js", "exports.data = 1;\n"),
                file(
                    "test/child.test.js",
                    "const { execFileSync } = require('child_process');\ntest('child', () => { expect(execFileSync(process.execPath, ['-e', \"process.stdout.write(require('fs').readFileSync('src/data.js', 'utf8'))\"]).toString()).toContain('= 1'); });\n",
                ),
            ],
            |r| r.write("src/data.js", "exports.data = 2;\n"),
        ),
        hazard(
            "package.json, read by a test",
            vec![file(
                "test/pkg.test.js",
                "const fs = require('fs');\ntest('pkg', () => { expect(fs.readFileSync('package.json', 'utf8')).not.toContain('changed'); });\n",
            )],
            |r| {
                r.write(
                    "package.json",
                    "{\"name\": \"shop\", \"private\": true, \"description\": \"changed\"}\n",
                )
            },
        ),
        hazard(
            "Jest's config, which every test file reads",
            vec![
                file("jest.config.js", &config("globals: { LEVEL: 1 }")),
                file(
                    "test/level.test.js",
                    "test('level', () => { expect(LEVEL).toBe(1); });\n",
                ),
            ],
            |r| {
                r.write(
                    "jest.config.js",
                    "module.exports = { testEnvironment: 'node', transform: {}, globals: { LEVEL: 2 } };\n",
                )
            },
        ),
        hazard(
            "a new file in a directory a test lists",
            vec![file(
                "test/count.test.js",
                "const fs = require('fs');\ntest('count', () => { expect(fs.readdirSync('test').length).toBe(8); });\n",
            )],
            |r| r.write("test/more.test.js", "test('more', () => {});\n"),
        ),
        hazard(
            "a new file that shadows a module",
            vec![
                file("src/util/index.js", "exports.util = 1;\n"),
                file(
                    "test/util.test.js",
                    "test('util', () => { expect(require('../src/util').util).toBe(1); });\n",
                ),
            ],
            |r| r.write("src/util.js", "exports.util = 2;\n"),
        ),
        hazard(
            "a setup file",
            vec![
                file("jest.config.js", &config("setupFiles: ['./test/setup.js']")),
                file("test/setup.js", "globalThis.LEVEL = 1;\n"),
                file(
                    "test/level.test.js",
                    "test('level', () => { expect(globalThis.LEVEL).toBe(1); });\n",
                ),
            ],
            |r| r.write("test/setup.js", "globalThis.LEVEL = 2;\n"),
        ),
        hazard(
            "a shared helper",
            vec![
                file("test/helpers.js", "exports.want = 6;\n"),
                file(
                    "test/helped.test.js",
                    "const { want } = require('./helpers');\nconst { total } = require('../src/cart');\ntest('helped', () => { expect(total([1, 2])).toBe(want); });\n",
                ),
            ],
            |r| r.write("test/helpers.js", "exports.want = 7;\n"),
        ),
        hazard(
            "a deleted file a test reads",
            vec![
                file("test/fixtures/names.js", "exports.names = ['ann'];\n"),
                file(
                    "test/names.test.js",
                    "const fs = require('fs');\ntest('names', () => { expect(fs.existsSync('test/fixtures/names.js')).toBe(true); });\n",
                ),
            ],
            |r| r.remove("test/fixtures/names.js"),
        ),
        hazard(
            "a manual mock",
            vec![
                file("src/mocked.js", "exports.mocked = () => 'real';\n"),
                file(
                    "src/__mocks__/mocked.js",
                    "exports.mocked = () => 'mock';\n",
                ),
                file(
                    "test/mock.test.js",
                    "jest.mock('../src/mocked');\nconst { mocked } = require('../src/mocked');\ntest('mock', () => { expect(mocked()).toBe('mock'); });\n",
                ),
            ],
            |r| {
                r.write(
                    "src/__mocks__/mocked.js",
                    "exports.mocked = () => 'other';\n",
                )
            },
        ),
        hazard(
            "a new manual mock",
            vec![
                file("src/mocked.js", "exports.mocked = () => 'real';\n"),
                file(
                    "test/mock.test.js",
                    "jest.mock('../src/mocked');\nconst { mocked } = require('../src/mocked');\ntest('mock', () => { expect(mocked()).toBeUndefined(); });\n",
                ),
            ],
            |r| {
                r.write(
                    "src/__mocks__/mocked.js",
                    "exports.mocked = () => 'mock';\n",
                )
            },
        ),
        hazard(
            "a snapshot",
            vec![file(
                "test/snap.test.js",
                "test('snap', () => { expect({ level: 1 }).toMatchSnapshot(); });\n",
            )],
            |r| {
                let snap = r.repo.join("test/__snapshots__/snap.test.js.snap");
                let text = std::fs::read_to_string(&snap).expect("the snapshot was written");
                std::fs::write(&snap, text.replace("\"level\": 1", "\"level\": 2")).unwrap();
            },
        ),
        hazard(
            "a module moduleNameMapper names",
            vec![
                file(
                    "jest.config.js",
                    &config("moduleNameMapper: { '^@level$': '<rootDir>/src/level.js' }"),
                ),
                file("src/level.js", "exports.level = 1;\n"),
                file(
                    "test/mapped.test.js",
                    "test('mapped', () => { expect(require('@level').level).toBe(1); });\n",
                ),
            ],
            |r| r.write("src/level.js", "exports.level = 2;\n"),
        ),
    ]
}

/// Runs each hazard in `observe`, which runs everything and computes the plan's keys without its
/// audits: every test file that fails is one the plan wouldn't have reused.
fn check_hazards(jest: &Jest, hazards: Vec<Hazard>, some_reused: bool) {
    let mut reused = 0;
    for hazard in hazards {
        let mut files = shop("observe");
        for (path, text) in &hazard.files {
            files.retain(|(p, _)| p != path);
            files.push((path.clone(), text.clone()));
        }
        let r = Repo::new(jest, &files);
        let command: Vec<&str> = match hazard.command {
            Some(words) => ["run", "unit", "--"].iter().chain(words).copied().collect(),
            None => JEST_RUN.to_vec(),
        };
        // A snapshot is written by the first run, which keeps nothing: the files changed.
        if hazard.name == "a snapshot" {
            let mut write = command.clone();
            write.retain(|w| *w != "--ci");
            assert_eq!(r.run(&write).code, 0, "{}", hazard.name);
        }
        let first = r.run(&command);
        assert_eq!(
            first.code,
            0,
            "Jest {} {}: {}",
            jest.major,
            hazard.name,
            first.out()
        );
        (hazard.change)(&r);
        let failing = r.truth();
        assert!(
            !failing.is_empty(),
            "Jest {} {}: nothing fails, so it tests nothing",
            jest.major,
            hazard.name
        );
        let ran = r.run(&command);
        assert_ne!(ran.code, 0, "{}: {}", hazard.name, ran.stderr);
        let missed = missed(&ran.stderr);
        assert!(
            missed.is_empty(),
            "Jest {} {}: the plan would have reused what fails ({failing:?}): {missed:?}\n{}",
            jest.major,
            hazard.name,
            ran.stderr
        );
        reused += would_reuse(&ran.stderr);
    }
    assert!(reused > 0 || !some_reused);
}

// covers: C-34, C-44
#[test]
fn jest_hazards_miss_no_failure() {
    for jest in jests() {
        check_hazards(&jest, hazards(), true);
    }
}

// covers: C-44
#[test]
fn what_every_test_file_reads_with_jest() {
    for jest in jests() {
        // A repository's own environment, globalSetup and resolver run for every test file: their
        // files, and what Node compiled for them, are read by every test file.
        let config = "module.exports = { testEnvironment: './env/custom.js', globalSetup: './setup/global.js', resolver: './setup/resolver.js', transform: {} };\n";
        let files = vec![
            file("jest.config.js", config),
            file(
                "env/custom.js",
                "const { TestEnvironment } = require('jest-environment-node');\nconst { level } = require('./level');\nmodule.exports = class extends TestEnvironment { async setup() { await super.setup(); this.global.LEVEL = level; } };\n",
            ),
            file("env/level.js", "exports.level = 1;\n"),
            file(
                "setup/global.js",
                "module.exports = async () => { process.env.SEEDED = String(require('./seed').seed); };\n",
            ),
            file("setup/seed.js", "exports.seed = 1;\n"),
            file(
                "setup/resolver.js",
                "module.exports = (path, options) => options.defaultResolver(path, options);\n",
            ),
            file(
                "test/level.test.js",
                "test('level', () => { expect(LEVEL).toBe(1); expect(process.env.SEEDED).toBe('1'); });\n",
            ),
        ];
        let mut hazards = Vec::new();
        for (name, change) in [
            (
                "an environment's own import",
                (|r: &Repo| r.write("env/level.js", "exports.level = 2;\n")) as fn(&Repo),
            ),
            ("what globalSetup required", |r: &Repo| {
                r.write("setup/seed.js", "exports.seed = 2;\n")
            }),
        ] {
            hazards.push(hazard(name, files.clone(), change));
        }
        // Every test file reads them: none would be reused.
        check_hazards(&jest, hazards, false);
        let mut all = shop("observe");
        for (path, text) in &files {
            all.retain(|(p, _)| p != path);
            all.push((path.clone(), text.clone()));
        }
        let r = Repo::new(&jest, &all);
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        let reads = r.reads("test/cart.test.js");
        for every in [
            "env/custom.js",
            "env/level.js",
            "jest.config.js",
            "setup/global.js",
            "setup/resolver.js",
            "setup/seed.js",
        ] {
            assert!(
                reads.contains(&every.to_string()),
                "Jest {} {every}: {reads:?}",
                jest.major
            );
        }
        // A globalSetup that starts a program: no test file is observed.
        r.write(
            "setup/global.js",
            "module.exports = async () => { require('child_process').execFileSync(process.execPath, ['-e', '1']); process.env.SEEDED = '1'; };\n",
        );
        let started = r.run(JEST_RUN);
        assert_eq!(started.code, 0, "{}", started.out());
        let map = r.map();
        assert_eq!(
            map["units"]["test/cart.test.js"]["notFollowed"][0], "subprocess",
            "Jest {}: {map:#}",
            jest.major
        );
    }
}

// covers: C-44
#[test]
fn ts_jest_checking_types_makes_every_file_of_code_read_by_every_test_file() {
    for jest in jests() {
        let tsconfig = |isolated: bool| {
            format!(
                "{{\"compilerOptions\": {{\"strict\": true, \"esModuleInterop\": true, \"isolatedModules\": {isolated}}}}}\n"
            )
        };
        let files = vec![
            file("hushrun.yaml", &yaml("observe")),
            file(".gitignore", "node_modules\n"),
            file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
            file(
                "jest.config.js",
                "module.exports = { preset: 'ts-jest', testEnvironment: 'node' };\n",
            ),
            file("tsconfig.json", &tsconfig(false)),
            file("src/types.ts", "export interface P { x: number }\n"),
            file(
                "src/price.ts",
                "import type { P } from './types';\nexport const price = (p: P): number => p.x * 2;\n",
            ),
            file("src/other.ts", "export const other = 1;\n"),
            file(
                "test/price.test.ts",
                "import { test, expect } from '@jest/globals';\nimport { price } from '../src/price';\ntest('price', () => { expect(price({ x: 1 })).toBe(2); });\n",
            ),
            file(
                "test/other.test.ts",
                "import { test, expect } from '@jest/globals';\nimport { other } from '../src/other';\ntest('other', () => { expect(other).toBe(1); });\n",
            ),
        ];
        let r = Repo::new(&jest, &files);
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        // A type-only import is erased: no run shows it. Every file of code is read instead.
        let reads = r.reads("test/other.test.ts");
        assert!(
            reads.contains(&"src/types.ts".to_string()),
            "Jest {}: {reads:?}",
            jest.major
        );
        assert!(reads.contains(&"src/price.ts".to_string()), "{reads:?}");
        // The type changes, and the test file that used it fails to compile: it would run.
        r.write("src/types.ts", "export interface P { y: number }\n");
        assert!(!r.truth().is_empty(), "a type error fails it");
        let changed = r.run(JEST_RUN);
        assert_ne!(changed.code, 0);
        assert!(missed(&changed.stderr).is_empty(), "{}", changed.stderr);
        // With isolatedModules, ts-jest compiles each file alone: no type checker reads every
        // file, and each test file reads what Jest compiled for it (and, for the first of each
        // process, the directories ts-jest listed reading tsconfig.json).
        r.write("src/types.ts", "export interface P { x: number }\n");
        let every = listed(&r);
        let run = json!({"testFiles": every});
        let (code, out, facts) = adapter_jest(&r, run.clone(), &[]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            unit_of(&facts, "test/other.test.ts")["settings"]["typeAware"],
            true
        );
        r.write("tsconfig.json", &tsconfig(true));
        let (code, out, facts) = adapter_jest(&r, run, &[]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            unit_of(&facts, "test/other.test.ts")["settings"]["typeAware"],
            false,
            "Jest {}",
            jest.major
        );
    }
}

// covers: C-44
#[test]
fn jest_projects_snapshots_and_worker_threads() {
    for jest in jests() {
        // One test file in two projects is one unit, by its path: it read what both runs read.
        let mut files = shop("observe");
        files.retain(|(p, _)| p != "jest.config.js");
        files.push(file(
            "jest.config.js",
            "module.exports = { projects: [{ displayName: 'a', testEnvironment: 'node', transform: {}, testMatch: ['**/test/*.test.js'] }, { displayName: 'b', testEnvironment: 'node', transform: {}, testMatch: ['**/test/cart.test.js'], setupFiles: ['./test/b.js'] }] };\n",
        ));
        files.push(file("test/b.js", "globalThis.B = 1;\n"));
        files.push(file(
            "test/thread.test.js",
            "const { Worker } = require('worker_threads');\ntest('thread', async () => { const w = new Worker('process.exit(0)', { eval: true }); await new Promise((r) => w.on('exit', r)); });\n",
        ));
        let r = Repo::new(&jest, &files);
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        let map = r.map();
        assert_eq!(map["unitFiles"].as_array().unwrap().len(), 8, "{map:#}");
        assert!(
            r.reads("test/cart.test.js")
                .contains(&"test/b.js".to_string())
        );
        assert!(
            !r.reads("test/pricing.test.js")
                .contains(&"test/b.js".to_string())
        );
        // A worker thread isn't followed.
        assert_eq!(
            map["units"]["test/thread.test.js"]["notFollowed"][0], "worker",
            "Jest {}: {map:#}",
            jest.major
        );
        // A runner other than jest-circus, even one that hands over to it: no test file is
        // observed.
        r.write(
            "test/runner.js",
            "module.exports = require('jest-circus/runner');\n",
        );
        r.write(
            "jest.config.js",
            "module.exports = { testEnvironment: 'node', transform: {}, testRunner: './test/runner.js' };\n",
        );
        let other = r.run(JEST_RUN);
        assert_eq!(other.code, 0, "{}", other.out());
        let map = r.map();
        assert_eq!(
            map["units"]["test/cart.test.js"]["notFollowed"],
            json!(["runner"]),
            "Jest {}: {map:#}",
            jest.major
        );
    }
}

// covers: C-44
#[test]
fn jest_in_its_es_module_mode_is_observed() {
    for jest in jests() {
        let files = vec![
            file("hushrun.yaml", &yaml("observe")),
            file(".gitignore", "node_modules\n"),
            file(
                "package.json",
                "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
            ),
            file(
                "jest.config.js",
                "export default { testEnvironment: 'node', transform: {} };\n",
            ),
            file("src/pricing.js", "export const price = (x) => x * 2;\n"),
            file(
                "src/cart.js",
                "import { price } from './pricing.js';\nexport const total = (xs) => xs.reduce((n, x) => n + price(x), 0);\n",
            ),
            file(
                "test/cart.test.js",
                "import { test, expect } from '@jest/globals';\nimport { total } from '../src/cart.js';\ntest('total', () => { expect(total([1, 2])).toBe(6); });\n",
            ),
            file(
                "test/other.test.js",
                "import { test, expect } from '@jest/globals';\ntest('other', () => { expect(1).toBe(1); });\n",
            ),
        ];
        let r = Repo::new(&jest, &files);
        let ran = r.run(&[
            "run",
            "unit",
            "--",
            "node",
            "--experimental-vm-modules",
            "node_modules/jest/bin/jest.js",
            "--ci",
        ]);
        assert_eq!(ran.code, 0, "{}", ran.out());
        assert_eq!(
            r.reads("test/cart.test.js"),
            [
                "jest.config.js",
                "src/cart.js",
                "src/pricing.js",
                "test/cart.test.js"
            ],
            "Jest {}",
            jest.major
        );
        assert_eq!(
            r.reads("test/other.test.js"),
            ["jest.config.js", "test/other.test.js"]
        );
    }
}

// covers: C-44
#[test]
fn a_test_file_that_fails_to_load_or_kills_its_worker_is_told() {
    for jest in jests() {
        let mut files = shop("observe");
        files.push(file(
            "test/broken.test.js",
            "require('./missing');\ntest('never', () => {});\n",
        ));
        files.push(file(
            "test/crash.test.js",
            "test('crash', () => { process.kill(process.pid, 'SIGKILL'); });\n",
        ));
        let r = Repo::new(&jest, &files);
        let ran = r.run(&[
            "run",
            "unit",
            "--",
            "node",
            "node_modules/jest/bin/jest.js",
            "--ci",
            "--maxWorkers=2",
        ]);
        assert_ne!(ran.code, 0, "{}", ran.out());
        let map = r.map();
        // The worker that ran it died before it said what it read: not observed.
        assert_eq!(
            map["units"]["test/crash.test.js"]["notFollowed"],
            json!(["unseen"]),
            "Jest {}: {map:#}",
            jest.major
        );
        // A file that fails to load fails, before any of its tests ran.
        let every = listed(&r);
        let (code, _, facts) =
            adapter_jest(&r, json!({"unit": "test", "testFiles": every}), &["broken"]);
        assert_eq!(code, 1);
        let unit = unit_of(&facts, "test/broken.test.js");
        assert_eq!(unit["failed"], true, "{unit:#}");
        assert_eq!(unit["settings"]["fileFailures"][0]["in"], "collection");
        let main = main_of(&facts);
        assert_eq!(main["units"][0]["failed"], true, "{main:#}");
    }
}

// ------------------------------------------------------------------ `reads: test`, end to end

/// A repository whose `test/a.test.js` has `target` among `files`, and a test file every change
/// runs, so that the check's own key misses.
fn reads_repo(jest: &Jest, files: &[(&str, &str)], tests: &str) -> Repo {
    let mut all = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\ncoverage\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file(
            "test/other.test.js",
            "test('other', () => { expect(1).toBe(1); });\n",
        ),
    ];
    for (path, text) in files {
        all.retain(|(p, _)| p != path);
        all.push(file(path, text));
    }
    Repo::new(jest, &all)
}

/// Whether `observe`, with `reads`, would have reused `target` of `test/a.test.js` after
/// `change` made it fail: what its disagreement line says.
fn would_reuse_test(
    jest: &Jest,
    files: &[(&str, &str)],
    change: (&str, &str),
    reads: &str,
) -> bool {
    let r = reads_repo(
        jest,
        files,
        &format!("{{mode: observe, unit: test, reads: {reads}}}"),
    );
    let first = r.run(JEST_RUN);
    assert_eq!(first.code, 0, "Jest {}: {}", jest.major, first.out());
    assert!(
        !first.stderr.contains("each test is taken to have read"),
        "{}",
        first.stderr
    );
    r.write(change.0, change.1);
    let second = r.run(JEST_RUN);
    assert_eq!(
        second.code,
        1,
        "the change makes target fail:\n{}",
        second.out()
    );
    second.stderr.contains(
        "hushrun: unit: test/a.test.js::target failed, though it passed before on the same files it read: with `tests: reuse` it would have been reused",
    )
}

/// `reads: test` follows the change: neither setting would reuse the test.
fn caught(files: &[(&str, &str)], change: (&str, &str)) {
    for jest in jests() {
        assert!(
            !would_reuse_test(&jest, files, change, "file"),
            "Jest {} reads: file",
            jest.major
        );
        assert!(
            !would_reuse_test(&jest, files, change, "test"),
            "Jest {} reads: test",
            jest.major
        );
    }
}

/// A known miss of `reads: test` (RFC 0004 §3.2): `reads: file` runs the test, `reads: test`
/// would reuse it. Whoever closes the miss changes the second assertion.
fn known_miss(files: &[(&str, &str)], change: (&str, &str)) {
    for jest in jests() {
        assert!(
            !would_reuse_test(&jest, files, change, "file"),
            "Jest {} reads: file",
            jest.major
        );
        assert!(
            would_reuse_test(&jest, files, change, "test"),
            "Jest {} reads: test: a known miss",
            jest.major
        );
    }
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_function_called() {
    caught(
        &[
            ("src/calc.js", "exports.f = () => 1;\n"),
            (
                "src/facade.js",
                "const calc = require('./calc');\nexports.g = () => calc.f();\n",
            ),
            (
                "test/a.test.js",
                "const facade = require('../src/facade');\ntest('target', () => { expect(facade.g()).toBe(1); });\ntest('plain', () => {});\n",
            ),
        ],
        ("src/calc.js", "exports.f = () => 2;\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_constant_the_code_that_ran_imports_directly() {
    caught(
        &[
            ("src/consts.js", "exports.K = 1;\n"),
            (
                "src/facade.js",
                "const { K } = require('./consts');\nexports.g = () => K;\n",
            ),
            (
                "test/a.test.js",
                "const facade = require('../src/facade');\ntest('target', () => { expect(facade.g()).toBe(1); });\n",
            ),
        ],
        ("src/consts.js", "exports.K = 2;\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_constant_the_test_file_imports_directly() {
    caught(
        &[
            ("src/consts.js", "exports.K = 1;\n"),
            (
                "test/a.test.js",
                "const { K } = require('../src/consts');\ntest('target', () => { expect(K).toBe(1); });\n",
            ),
        ],
        ("src/consts.js", "exports.K = 2;\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_file_opened() {
    caught(
        &[
            ("src/data.js", "1\n"),
            (
                "test/a.test.js",
                "const fs = require('fs');\ntest('target', () => { expect(fs.readFileSync('src/data.js', 'utf8').trim()).toBe('1'); });\ntest('plain', () => {});\n",
            ),
        ],
        ("src/data.js", "2\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_file_a_module_read_once_and_kept() {
    caught(
        &[
            ("src/price.js", "5\n"),
            (
                "src/prices.js",
                "const fs = require('fs');\nconst path = require('path');\nlet kept;\nexports.price = () => (kept = kept || fs.readFileSync(path.join(__dirname, 'price.js'), 'utf8').trim());\n",
            ),
            (
                "test/a.test.js",
                "const prices = require('../src/prices');\ntest('first', () => { expect(prices.price()).toBeTruthy(); });\ntest('target', () => { expect(prices.price()).toBe('5'); });\n",
            ),
        ],
        ("src/price.js", "6\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_file_a_module_above_the_one_that_read_it_kept() {
    // config.js keeps what fsutil.js's readJson read for it: `target` calls config() and not
    // readJson, which the first test's call left cached.
    caught(
        &[
            ("src/settings.js", "{\"rate\": 1}\n"),
            (
                "src/fsutil.js",
                "const fs = require('fs');\nexports.readJson = (p) => JSON.parse(fs.readFileSync(p, 'utf8'));\n",
            ),
            (
                "src/config.js",
                "const path = require('path');\nconst { readJson } = require('./fsutil');\nlet kept;\nexports.config = () => (kept = kept || readJson(path.join(__dirname, 'settings.js')));\n",
            ),
            (
                "test/a.test.js",
                "const { config } = require('../src/config');\ntest('first', () => { expect(config()).toBeTruthy(); });\ntest('target', () => { expect(config().rate).toBe(1); });\n",
            ),
        ],
        ("src/settings.js", "{\"rate\": 2}\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_file_the_test_file_read_once_and_kept() {
    caught(
        &[
            ("src/local.js", "{\"n\": 1}\n"),
            (
                "test/a.test.js",
                "const fs = require('fs');\nlet local;\nconst mine = () => (local = local || JSON.parse(fs.readFileSync('src/local.js', 'utf8')));\ntest('first', () => { expect(mine()).toBeTruthy(); });\ntest('target', () => { expect(mine().n).toBe(1); });\n",
            ),
        ],
        ("src/local.js", "{\"n\": 2}\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_chain_of_re_exports_to_the_end() {
    // The test reads rate through two CommonJS barrels, none of whose code runs when it does. The
    // middle one then points at another file.
    caught(
        &[
            ("src/index.js", "module.exports = require('./sub');\n"),
            ("src/sub/index.js", "module.exports = require('./impl');\n"),
            ("src/sub/impl.js", "exports.rate = () => 1;\n"),
            ("src/sub/impl2.js", "exports.rate = () => 2;\n"),
            (
                "test/a.test.js",
                "const { rate } = require('../src');\ntest('target', () => { expect(rate()).toBe(1); });\ntest('plain', () => {});\n",
            ),
        ],
        ("src/sub/index.js", "module.exports = require('./impl2');\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_what_a_before_all_read() {
    caught(
        &[
            ("src/made.js", "exports.make = () => 1;\n"),
            (
                "test/a.test.js",
                "let made;\ndescribe('d', () => {\n  beforeAll(() => { made = require('../src/made').make(); });\n  test('target', () => { expect(made).toBe(1); });\n});\ntest('plain', () => {});\n",
            ),
        ],
        ("src/made.js", "exports.make = () => 2;\n"),
    );
}

// covers: C-40, C-44
#[test]
fn jest_reads_test_follows_a_module_required_lazily_inside_a_test() {
    caught(
        &[
            ("src/lazy.js", "exports.K = 1;\n"),
            (
                "test/a.test.js",
                "test('target', () => { expect(require('../src/lazy').K).toBe(1); });\ntest('plain', () => {});\n",
            ),
        ],
        ("src/lazy.js", "exports.K = 2;\n"),
    );
}

/// Two imports away: cart.js ran and imports settings.js, which built TAX from defaults.js at
/// import, with no function of either called.
fn two_imports_away() -> Vec<(&'static str, &'static str)> {
    vec![
        ("src/defaults.js", "exports.RATE = 3;\n"),
        (
            "src/settings.js",
            "const { RATE } = require('./defaults');\nexports.TAX = RATE * 2;\n",
        ),
        (
            "src/cart.js",
            "const { TAX } = require('./settings');\nexports.total = (xs) => xs.reduce((n, x) => n + x, 0) + TAX;\n",
        ),
        (
            "test/a.test.js",
            "const { total } = require('../src/cart');\ntest('target', () => { expect(total([1])).toBe(7); });\n",
        ),
    ]
}

// covers: C-40, C-44
#[test]
fn a_known_miss_of_jest_reads_test_a_value_two_imports_away() {
    known_miss(
        &two_imports_away(),
        ("src/defaults.js", "exports.RATE = 4;\n"),
    );
}

/// A registry plugins.js fills as boot.js requires it: the test file requires boot and the
/// registry, and nothing of plugins.js, whose code runs at import only.
fn a_registry() -> Vec<(&'static str, &'static str)> {
    vec![
        ("src/registry.js", "exports.HANDLERS = {};\n"),
        (
            "src/plugins.js",
            "const { HANDLERS } = require('./registry');\nHANDLERS.x = 41;\n",
        ),
        ("src/boot.js", "require('./plugins');\n"),
        (
            "test/a.test.js",
            "require('../src/boot');\nconst { HANDLERS } = require('../src/registry');\ntest('target', () => { expect(HANDLERS.x).toBe(41); });\n",
        ),
    ]
}

// covers: C-40, C-44
#[test]
fn a_known_miss_of_jest_reads_test_a_registry_another_file_filled_at_import() {
    known_miss(
        &a_registry(),
        (
            "src/plugins.js",
            "const { HANDLERS } = require('./registry');\nHANDLERS.x = 42;\n",
        ),
    );
}

/// The line `observe` says of what reusing tests would have done.
fn observe_tests_line(ran: &Ran) -> String {
    ran.line("hushrun: unit: with `tests: reuse`,").to_string()
}

// covers: C-40, C-44
#[test]
fn a_concurrent_test_gets_its_files_list() {
    for jest in jests() {
        // `plain` runs other.js's code through facade.js; `target` touches nothing. A change to other.js runs
        // `plain`, and `target` too when it is concurrent: its test file read other.js.
        for (concurrent, expected) in [
            (false, "1 of 3 tests would have run"),
            (true, "2 of 3 tests would have run"),
        ] {
            let target = if concurrent {
                "test.concurrent('target', async () => { expect(1).toBe(1); });"
            } else {
                "test('target', () => { expect(1).toBe(1); });"
            };
            let files = [
                ("src/other.js", "exports.f = () => 1;\n"),
                (
                    "src/facade.js",
                    "const other = require('./other');\nexports.g = () => other.f();\n",
                ),
                (
                    "test/a.test.js",
                    &*format!(
                        "const o = require('../src/facade');\n{target}\ntest('plain', () => {{ expect(o.g()).toBe(1); }});\n"
                    ),
                ),
            ];
            let r = reads_repo(&jest, &files, "{mode: observe, unit: test, reads: test}");
            let first = r.run(JEST_RUN);
            assert_eq!(first.code, 0, "{}", first.out());
            assert_eq!(
                first.stderr.contains("each test is taken to have read what its test file read: test.concurrent tests run at once"),
                concurrent,
                "{}",
                first.stderr
            );
            r.write("src/other.js", "exports.f = () => 1; // changed\n");
            let second = r.run(JEST_RUN);
            assert_eq!(second.code, 0, "{}", second.out());
            assert!(
                observe_tests_line(&second).contains(expected),
                "Jest {} concurrent {concurrent}: {}",
                jest.major,
                second.stderr
            );
        }
    }
}

// covers: C-40, C-44
#[test]
fn ts_jest_checking_types_and_jests_v8_coverage_watch_no_test_and_say_why() {
    for jest in jests() {
        let files = [
            (
                "jest.config.js",
                "module.exports = { preset: 'ts-jest', testEnvironment: 'node' };\n",
            ),
            (
                "tsconfig.json",
                "{\"compilerOptions\": {\"strict\": true, \"esModuleInterop\": true}}\n",
            ),
            ("src/k.ts", "export const k = 1;\n"),
            (
                "test/a.test.ts",
                "import { test, expect } from '@jest/globals';\nimport { k } from '../src/k';\ntest('target', () => { expect(k).toBe(1); });\n",
            ),
        ];
        let r = reads_repo(&jest, &files, "{mode: observe, unit: test, reads: test}");
        let typed = r.run(JEST_RUN);
        assert_eq!(typed.code, 0, "{}", typed.out());
        assert!(
            typed.stderr.contains("hushrun: unit: each test is taken to have read what its test file read: ts-jest checks types with a language service, which reads every file of code for every test file"),
            "Jest {}: {}",
            jest.major,
            typed.stderr
        );
        let r = reads_repo(
            &jest,
            &[("test/a.test.js", "test('target', () => {});\n")],
            "{mode: observe, unit: test, reads: test}",
        );
        let covered = r.run(&[
            "run",
            "unit",
            "--",
            "node",
            "node_modules/jest/bin/jest.js",
            "--ci",
            "--coverage",
            "--coverageProvider=v8",
        ]);
        assert_eq!(covered.code, 0, "{}", covered.out());
        assert!(
            covered.stderr.contains("hushrun: unit: each test is taken to have read what its test file read: Jest's own V8 coverage is on, and it takes the counters watching a test needs"),
            "Jest {}: {}",
            jest.major,
            covered.stderr
        );
    }
}

// covers: C-40, C-44, C-36
#[test]
fn with_jest_reads_test_a_test_is_reused_on_its_own_list_and_its_file_records_each_test() {
    for jest in jests() {
        // One test of a file runs a.js's code through wa.js, sixty others b.js's through wb.js;
        // the test file requires both wrappers: a change to a.js runs only the first test, and
        // two of the sixty, chosen by their keys, as a check.
        let files = [
            ("src/a.js", "exports.f = () => 1;\n"),
            ("src/b.js", "exports.f = () => 1;\n"),
            (
                "src/wa.js",
                "const a = require('./a');\nexports.f = () => a.f();\n",
            ),
            (
                "src/wb.js",
                "const b = require('./b');\nexports.f = () => b.f();\n",
            ),
            (
                "test/a.test.js",
                "const wa = require('../src/wa');\nconst wb = require('../src/wb');\ntest('one', () => { expect(wa.f()).toBe(1); });\ntest.each([...Array(60).keys()])('two %i', () => { expect(wb.f()).toBe(1); });\n",
            ),
        ];
        let r = reads_repo(&jest, &files, "{mode: reuse, unit: test, reads: test}");
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 0, "{}", first.out());
        let map = r.map();
        assert_eq!(
            map["units"]["test/a.test.js"]["tests"]
                .as_object()
                .unwrap()
                .len(),
            61
        );
        let mut results = Vec::new();
        collect(&r.cache.join("results/units/v0"), &mut results);
        assert_eq!(results.len(), 2 + 61 + 1, "Jest {}", jest.major);
        r.write("src/a.js", "exports.f = () => 1; // changed\n");
        let second = r.run(JEST_RUN);
        assert_eq!(second.code, 0, "{}", second.out());
        assert!(
            second.stderr.lines().any(|l| hushrun_testkit::lines::drawn(l)
                == hushrun_testkit::lines::drawn("hushrun: unit: 3 of 62 tests run, in 1 of 2 test files; 59 already passed on the files they read, and 2 more that already passed run anyway, as a check")),
            "Jest {}: {}",
            jest.major,
            second.stderr
        );
        assert_eq!(
            store_result(&r)["units"]["tests"],
            json!({"total": 62, "ran": 3, "reused": 59})
        );
    }
}

// covers: C-32, C-44
#[cfg(unix)]
#[test]
fn a_start_of_jest_that_runs_no_test_file_is_no_second_start() {
    use std::os::unix::fs::PermissionsExt as _;
    for jest in jests() {
        let r = Repo::new(
            &jest,
            &[
                file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
                file(".gitignore", "node_modules\n"),
                file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
                // A cache of its own, beside the repository: `--clearCache` empties Jest's cache
                // directory, which by default every Jest of this machine shares in the system's
                // temporary directory, other tests' and the person's included.
                file(
                    "jest.config.js",
                    "module.exports = { testEnvironment: 'node', transform: {}, cacheDirectory: '<rootDir>/../jest-cache' };\n",
                ),
                file("test/a.test.js", "test('a1', () => {});\n"),
            ],
        );
        // A script that starts Jest for what runs no test file first (`jest --clearCache && jest`),
        // with the other such forms, and then for the tests: one start ran tests.
        let bin = r.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let jest_js = "node node_modules/jest/bin/jest.js";
        std::fs::write(
            bin.join("pnpm"),
            format!(
                "#!/bin/sh\n{jest_js} --clearCache >/dev/null && {jest_js} --showConfig >/dev/null && {jest_js} --listTests >/dev/null && {jest_js} --version >/dev/null && {jest_js} --ci --passWithNoTests no-such-test && {jest_js} --ci\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(bin.join("pnpm"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let ran = r.run_env(&["run", "unit", "--", "pnpm", "jest"], &[("PATH", &path)]);
        assert_eq!(ran.code, 0, "Jest {}: {}", jest.major, ran.out());
        assert!(
            !ran.stderr.contains("records a pass"),
            "Jest {}: {}",
            jest.major,
            ran.stderr
        );
        assert_eq!(
            r.map()["units"]["test/a.test.js"]["tests"]["a1"],
            "passed",
            "Jest {}",
            jest.major
        );
        // The other way round: the tests first, then what runs none.
        r.write(
            "test/a.test.js",
            "test('a1', () => { expect(1).toBe(1); });\n",
        );
        std::fs::write(
            bin.join("pnpm"),
            format!(
                "#!/bin/sh\n{jest_js} --ci && {jest_js} --clearCache >/dev/null && {jest_js} --listTests >/dev/null\n"
            ),
        )
        .unwrap();
        let after = r.run_env(&["run", "unit", "--", "pnpm", "jest"], &[("PATH", &path)]);
        assert_eq!(after.code, 0, "Jest {}: {}", jest.major, after.out());
        assert!(
            !after.stderr.contains("records a pass"),
            "Jest {}: {}",
            jest.major,
            after.stderr
        );
    }
}

/// What a run says when a second start of the runner in COMMAND wasn't observed.
#[cfg(unix)]
const SECOND_START: &str = "no test file or test records a pass: Jest started more than once in its command, and hushrun observes only the first start: what another start ran can't be told";

// covers: C-32, C-44
#[cfg(unix)]
#[test]
fn a_second_start_of_jest_in_one_command_keeps_every_pass_of_the_run_out() {
    use std::os::unix::fs::PermissionsExt as _;
    for jest in jests() {
        // The first start runs both files, and passes; the second runs them again with FAILNOW
        // set, and f1 fails there. Only the first start is observed.
        let r = Repo::new(
            &jest,
            &[
                file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
                file(".gitignore", "node_modules\n"),
                file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
                file("jest.config.js", CONFIG),
                file("test/a.test.js", "test('a1', () => {});\n"),
                file(
                    "test/f.test.js",
                    "test('f1', () => { expect(process.env.FAILNOW).toBeUndefined(); });\ntest('f2', () => {});\n",
                ),
            ],
        );
        // As a package script does under `pnpm jest`: a stand-in `pnpm` that starts it twice.
        let bin = r.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("pnpm"), "#!/bin/sh\nnode node_modules/jest/bin/jest.js --ci && FAILNOW=1 node node_modules/jest/bin/jest.js --ci\n").unwrap();
        std::fs::set_permissions(bin.join("pnpm"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let ran = r.run_env(&["run", "unit", "--", "pnpm", "jest"], &[("PATH", &path)]);
        // The second start's failure is COMMAND's.
        assert_eq!(ran.code, 1, "Jest {}: {}", jest.major, ran.out());
        // No pass of that run is kept: f1 failed in the second start.
        let map = r.map();
        for (unit, test) in [("test/f.test.js", "f1"), ("test/a.test.js", "a1")] {
            assert_ne!(
                map["units"][unit]["tests"][test], "passed",
                "Jest {}: {unit}::{test}: {map:#}",
                jest.major
            );
        }
        // The run says why, once.
        assert_eq!(
            ran.stderr.matches(SECOND_START).count(),
            1,
            "Jest {}: {}",
            jest.major,
            ran.stderr
        );
        // The same COMMAND with one start: as before, its passes are kept and nothing is said.
        std::fs::write(
            bin.join("pnpm"),
            "#!/bin/sh\nnode node_modules/jest/bin/jest.js --ci\n",
        )
        .unwrap();
        let once = r.run_env(&["run", "unit", "--", "pnpm", "jest"], &[("PATH", &path)]);
        assert_eq!(once.code, 0, "Jest {}: {}", jest.major, once.out());
        assert!(
            !once.stderr.contains("records a pass"),
            "Jest {}: {}",
            jest.major,
            once.stderr
        );
        assert_eq!(
            r.map()["units"]["test/f.test.js"]["tests"]["f1"],
            "passed",
            "Jest {}",
            jest.major
        );
    }
}

/// What `hushrun why` says, stdout and stderr, of `unit` after the runs so far.
fn why_of(r: &Repo, unit: &str) -> String {
    r.run(&["why", "unit", unit]).out()
}

// covers: C-37, C-44
#[test]
fn why_says_what_an_error_no_jest_test_owns_left_of_its_file_and_of_another() {
    // A timer `leaks` left throws while `waits` runs: Jest 29 charges it to `waits`, Jest 30 to
    // `leaks`. leak.test.js failed, and no test of it keeps a result; ok.test.js passed, and
    // keeps its pass: Jest tells failures by test file.
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file(
            "test/leak.test.js",
            "test('leaks', () => { setTimeout(() => { throw new Error('late timer'); }, 20); });\ntest('waits', async () => { await new Promise((r) => setTimeout(r, 150)); });\ntest('after', () => {});\n",
        ),
        file("test/ok.test.js", "test('ok', () => {});\n"),
    ];
    let said = "Jest reported an error it couldn't charge to a test (late timer), so no test of its file keeps a result of that run";
    for jest in jests() {
        let r = Repo::new(&jest, &files);
        let first = r.run(JEST_RUN);
        assert_eq!(first.code, 1, "{}", first.out());
        let after = why_of(&r, "test/leak.test.js::after");
        assert!(after.contains(said), "Jest {}: {after}", jest.major);
        let file = why_of(&r, "test/leak.test.js");
        assert!(
            file.contains(
                "test/leak.test.js runs: its newest result on the files it reads now failed"
            ),
            "Jest {}: {file}",
            jest.major
        );
        for unit in ["test/ok.test.js", "test/ok.test.js::ok"] {
            let why = why_of(&r, unit);
            assert!(
                why.contains("would be reused") && !why.contains("couldn't charge"),
                "Jest {}: {unit}: {why}",
                jest.major
            );
        }
    }
}

// covers: C-37, C-44
#[cfg(unix)]
#[test]
fn why_says_a_second_start_of_jest_kept_every_pass_of_its_run_out() {
    use std::os::unix::fs::PermissionsExt as _;
    let said = "Jest started more than once in its command, and hushrun observes only the first start: what another start ran can't be told, so no pass of that run is kept";
    for jest in jests() {
        let r = Repo::new(
            &jest,
            &[
                file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
                file(".gitignore", "node_modules\n"),
                file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
                file("jest.config.js", CONFIG),
                file("test/a.test.js", "test('a1', () => {});\n"),
                file(
                    "test/f.test.js",
                    "test('f1', () => { expect(process.env.FAILNOW).toBeUndefined(); });\n",
                ),
            ],
        );
        // As a package script does under `pnpm jest`: a stand-in `pnpm` that starts it twice.
        let bin = r.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("pnpm"),
            "#!/bin/sh\nnode node_modules/jest/bin/jest.js --ci && FAILNOW=1 node node_modules/jest/bin/jest.js --ci\n",
        )
        .unwrap();
        std::fs::set_permissions(bin.join("pnpm"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let ran = r.run_env(&["run", "unit", "--", "pnpm", "jest"], &[("PATH", &path)]);
        assert_eq!(ran.code, 1, "Jest {}: {}", jest.major, ran.out());
        for unit in [
            "test/f.test.js::f1",
            "test/f.test.js",
            "test/a.test.js::a1",
            "test/a.test.js",
        ] {
            let why = why_of(&r, unit);
            assert!(why.contains(said), "Jest {}: {unit}: {why}", jest.major);
        }
    }
}

/// Test files that look at what Node's wrapped functions carry, as their callers do: each fails
/// if a wrapper of the adapter's drops what the function it wraps carries.
fn carried_files(tests: &str) -> Vec<(String, String)> {
    let t = |body: &str| {
        format!(
            "const {{ promisify }} = require('util');\nconst {{ exec, execFile }} = require('child_process');\nconst fs = require('fs');\nconst node = process.execPath;\nconst script = 'process.stdout.write(\"out\"); process.stderr.write(\"err\")';\n{body}\n"
        )
    };
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file("src/data.js", "exports.data = 1;\n"),
        file(
            "test/execfile.test.js",
            &t(
                "test('execFile', async () => {\n  expect(typeof execFile[promisify.custom]).toBe('function');\n  expect([execFile.name, execFile.length]).toEqual(['execFile', 4]);\n  const run = promisify(execFile);\n  expect(await run(node, ['-e', script])).toEqual({ stdout: 'out', stderr: 'err' });\n  const failed = await run(node, ['-e', script + '; process.exit(3)']).then(() => null, (e) => e);\n  expect([failed.code, failed.stdout, failed.stderr]).toEqual([3, 'out', 'err']);\n});",
            ),
        ),
        file(
            "test/exec.test.js",
            &t(
                "test('exec', async () => {\n  expect(typeof exec[promisify.custom]).toBe('function');\n  const run = promisify(exec);\n  const line = `${JSON.stringify(node)} -e ${JSON.stringify(script)}`;\n  expect(await run(line)).toEqual({ stdout: 'out', stderr: 'err' });\n  const failed = await run(line + ' && exit 3 || exit 3').then(() => null, (e) => e);\n  expect([failed.code, failed.stdout, failed.stderr]).toEqual([3, 'out', 'err']);\n});",
            ),
        ),
        file(
            "test/fs.test.js",
            &t(
                "test('fs', async () => {\n  expect(typeof fs.exists[promisify.custom]).toBe('function');\n  expect(await promisify(fs.exists)('src/data.js')).toBe(true);\n  const fd = fs.openSync('src/data.js', 'r');\n  const got = await promisify(fs.read)(fd, Buffer.alloc(7), 0, 7, 0);\n  fs.closeSync(fd);\n  expect([got.bytesRead, got.buffer.toString()]).toEqual([7, 'exports']);\n  expect([fs.readFile.name, fs.readFile.length]).toEqual(['readFile', 3]);\n  expect(Reflect.ownKeys(fs.readFile).map(String).sort()).toEqual(['length', 'name', 'prototype']);\n  expect(Reflect.ownKeys(fs.promises.readFile).map(String).sort()).toEqual(['length', 'name']);\n  expect(Object.keys(fs.realpathSync)).toEqual(['native']);\n  expect(fs.realpathSync.native('src/data.js')).toBe(fs.realpathSync('src/data.js'));\n  const real = await new Promise((ok, no) => fs.realpath.native('src/data.js', (e, p) => (e ? no(e) : ok(p))));\n  expect(real).toBe(fs.realpathSync('src/data.js'));\n});",
            ),
        ),
        file(
            "test/shapes.test.js",
            &t(
                "const { Worker } = require('worker_threads');\nconst vm = require('vm');\ntest('shapes', () => {\n  expect([Worker.name, Worker.length]).toEqual(['Worker', 1]);\n  expect(Object.getOwnPropertyNames(Worker).sort()).toEqual(['length', 'name', 'prototype']);\n  expect([process.dlopen.name, process.dlopen.length]).toEqual(['dlopen', 0]);\n  expect([vm.Script.name, vm.Script.length]).toEqual(['Script', 1]);\n  expect([vm.compileFunction.name, vm.compileFunction.length]).toEqual(['compileFunction', 2]);\n  expect(Object.getOwnPropertyNames(vm.Script).sort()).toEqual(['length', 'name', 'prototype']);\n});",
            ),
        ),
    ]
}

// covers: C-44
#[test]
fn wrapped_node_functions_keep_what_callers_see_with_jest_and_their_promisified_forms_stay_observed()
 {
    for jest in jests() {
        // Node's `exec` and `execFile` carry `util.promisify.custom`, which resolves to `{ stdout,
        // stderr }`, `fs.exists` one that resolves to a boolean, `fs.read` the names of what its
        // promisified form resolves to, and `fs.realpath` and `fs.realpathSync` carry `native`;
        // `Worker`, `process.dlopen` and `vm`'s have their names and lengths. A child process a
        // promisified form starts is still one hushrun can't follow, and what it read is read.
        let r = Repo::new(&jest, &carried_files("observe"));
        let ran = r.run(JEST_RUN);
        assert_eq!(ran.code, 0, "Jest {}: {}", jest.major, ran.out());
        let map = r.map();
        for unit in ["test/execfile.test.js", "test/exec.test.js"] {
            assert_eq!(
                map["units"][unit]["notFollowed"],
                json!(["subprocess"]),
                "Jest {}: {unit}: {map:#}",
                jest.major
            );
        }
        assert!(
            r.reads("test/fs.test.js")
                .contains(&"src/data.js".to_string()),
            "Jest {}: {map:#}",
            jest.major
        );
    }
}

// covers: C-39, C-44
#[test]
fn with_tests_one_by_one_jests_wrappers_keep_what_callers_see_too() {
    for jest in jests() {
        // With tests one by one and what each read, the same: Node's functions as callers see
        // them, and a promisified child process not followed.
        let r = Repo::new(
            &jest,
            &carried_files("{mode: reuse, unit: test, reads: test}"),
        );
        let ran = r.run(JEST_RUN);
        assert_eq!(ran.code, 0, "Jest {}: {}", jest.major, ran.out());
        let map = r.map();
        for (unit, test) in [
            ("test/execfile.test.js", "execFile"),
            ("test/exec.test.js", "exec"),
        ] {
            assert_eq!(
                map["units"][unit]["tests"][test], "not-observed",
                "Jest {}: {unit}: {map:#}",
                jest.major
            );
        }
    }
}
