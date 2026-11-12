//! \[ADR 0071\] Test files with Vitest (C-30 to C-35): a unit is a test file, what it read is what
//! Vitest's worker ran and opened while it ran, with the config files, and reuse never misses a
//! failure.
//!
//! The fixtures need Node and Vitest 4.1.12, installed from npm outside the repository, with
//! `HUSHRUN_TEST_VITEST` naming its `node_modules` (CI: `npm install --prefix <dir>
//! vitest@4.1.12`). They say so and pass where it is missing, unless `HUSHRUN_TEST_TOOLS=required`.

#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::{Value, json};

/// The Vitest these tests are written against.
const VITEST: &str = "4.1.12";

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
    "HUSHRUN_VITEST",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "NODE_OPTIONS",
    "VITEST",
    "VITEST_POOL_ID",
    "VITEST_WORKER_ID",
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
    // B-1, B-2: the developer's build mode, store and wrapper stay out: a test run that cargo
    // itself compiles through the wrapper doesn't pass its session on to a fixture's cargo.
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";
const LINE: &str = "plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())],";

/// The `node_modules` with the pinned Vitest, when this machine has it.
fn vitest_modules() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HUSHRUN_TEST_VITEST")?);
    let package = std::fs::read(dir.join("vitest/package.json")).ok()?;
    let package: Value = serde_json::from_slice(&package).ok()?;
    (package["version"] == VITEST).then_some(dir)
}

fn has_vitest() -> bool {
    vitest_modules().is_some()
        && Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn tools(what: &str, here: bool) -> bool {
    if here {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
}

impl Repo {
    fn new(files: &[(String, String)]) -> Repo {
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
        // Vitest as installed outside the repository, which git ignores.
        let modules = vitest_modules().expect("vitest");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&modules, r.repo.join("node_modules")).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&modules, r.repo.join("node_modules")).expect("link");
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

    /// Vitest with everything run, outside hushrun: the test files that fail.
    fn truth(&self) -> BTreeSet<String> {
        let report = self.root.join("truth.json");
        let mut cmd = Command::new(self.repo.join("node_modules/.bin/vitest"));
        cmd.current_dir(&self.repo);
        Repo::clean(&mut cmd);
        cmd.args(["run", "--reporter=json", "--outputFile"])
            .arg(&report)
            .output()
            .expect("vitest");
        failed_files(&report, &self.repo)
    }

    /// The check's map, as this cache's store holds it (ADR 0072).
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
}

/// The test files a Vitest JSON report says failed, from the repository root.
fn failed_files(report: &Path, repo: &Path) -> BTreeSet<String> {
    let Ok(bytes) = std::fs::read(report) else {
        return BTreeSet::new();
    };
    let report: Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let repo = std::fs::canonicalize(repo).unwrap();
    report["testResults"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["status"] != "passed")
        .filter_map(|r| r["name"].as_str())
        .filter_map(|name| {
            let real = std::fs::canonicalize(name).ok()?;
            Some(
                real.strip_prefix(&repo)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
        })
        .collect()
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn plan_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| {
                l.contains("test files run")
                    || l.contains("test file run")
                    || l.contains("every test file runs")
            })
            .unwrap_or_else(|| panic!("no plan line in:\n{}", self.stderr))
    }

    fn observe_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| l.contains("with `tests: reuse`"))
            .unwrap_or_else(|| panic!("no observe line in:\n{}", self.stderr))
    }
}

/// Text without its terminal colors.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

fn config(more: &str) -> String {
    format!(
        "import {{ defineConfig }} from 'vitest/config'\n\nexport default defineConfig({{\n  {LINE}\n{more}}})\n"
    )
}

/// A shop: a cart that imports pricing, each with a test file; a greeting; and five test files
/// that read only the greeting, so that a plan has something to reuse.
fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
        file(
            "src/pricing.ts",
            "export const price = (x: number) => x * 2\n",
        ),
        file(
            "src/cart.ts",
            "import { price } from './pricing'\n\nexport const total = (xs: number[]) => xs.reduce((n, x) => n + price(x), 0)\n",
        ),
        file("src/greet.ts", "export const hello = () => 'hi'\n"),
        file(
            "test/cart.test.ts",
            "import { test, expect } from 'vitest'\nimport { total } from '../src/cart'\n\ntest('total', () => { expect(total([1, 2])).toBe(6) })\n",
        ),
        file(
            "test/pricing.test.ts",
            "import { test, expect } from 'vitest'\nimport { price } from '../src/pricing'\n\ntest('price', () => { expect(price(3)).toBe(6) })\n",
        ),
    ];
    for n in 1..=5 {
        files.push(file(
            &format!("test/greet{n}.test.ts"),
            "import { test, expect } from 'vitest'\nimport { hello } from '../src/greet'\n\ntest('hello', () => { expect(hello()).toBe('hi') })\n",
        ));
    }
    files
}

const VITEST_RUN: &[&str] = &["run", "unit", "--", "node_modules/.bin/vitest", "run"];

// covers: C-30, C-31, C-32, C-35
#[test]
fn vitest_test_files_are_observed_from_what_their_workers_ran_and_opened() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let mut files = shop("observe");
    files.push(file("src/dyn.ts", "export const dyn = () => 7\n"));
    files.push(file("src/data.ts", "export const data = 1\n"));
    files.push(file(
        "test/dyn.test.ts",
        "import { test, expect } from 'vitest'\nimport { readFileSync } from 'node:fs'\n\ntest('dyn', async () => {\n  const name = 'dyn'\n  const m = await import(`../src/${name}.ts`)\n  expect(m.dyn()).toBe(7)\n  expect(readFileSync('src/data.ts', 'utf8')).toContain('data')\n})\n",
    ));
    let r = Repo::new(&files);
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.observe_line(),
        "hushrun: unit: with `tests: reuse`, all 8 test files would have run: none had passed here yet on the files it read"
    );
    // What the worker ran, the module a dynamic import named included, what it opened, and the
    // config, which every test file reads.
    assert_eq!(
        r.reads("test/cart.test.ts"),
        [
            "src/cart.ts",
            "src/pricing.ts",
            "test/cart.test.ts",
            "vitest.config.ts"
        ]
    );
    assert_eq!(
        r.reads("test/dyn.test.ts"),
        [
            "src/data.ts",
            "src/dyn.ts",
            "test/dyn.test.ts",
            "vitest.config.ts"
        ]
    );
    let map = r.map();
    assert_eq!(map["unitFiles"].as_array().unwrap().len(), 8, "{map:#}");
    assert!(map["rest"]["package.json"].is_string(), "{map:#}");
    // One source changes: what read it would run, and the rest would be reused.
    r.write(
        "src/pricing.ts",
        "export const price = (x: number) => x + x\n",
    );
    let changed = r.run(VITEST_RUN);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        changed.observe_line(),
        "hushrun: unit: with `tests: reuse`, 2 of 8 test files would have run; 6 already passed on the files they read"
    );
}

// covers: C-32
#[test]
fn without_the_config_line_or_one_run_vitest_is_left_alone_and_says_why() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let mut files = shop("observe");
    files.retain(|(p, _)| p != "vitest.config.ts");
    files.push(file("vite.config.ts", "export default { test: {} }\n"));
    let r = Repo::new(&files);
    let ran = r.run(VITEST_RUN);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains("hushrun: unit: test files aren't observed: Vitest loads hushrun's adapter from one line in vite.config.ts, which `hushrun init` proposes: `plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())]`"),
        "{}",
        ran.stderr
    );
    let watching = r.run(&[
        "run",
        "unit",
        "--",
        "node_modules/.bin/vitest",
        "--watch=false",
    ]);
    assert!(
        watching
            .stderr
            .contains("test files aren't observed: vitest without run"),
        "{}",
        watching.stderr
    );
}

// covers: C-34, C-35
#[test]
fn vitest_reuses_what_already_passed() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = Repo::new(&shop("reuse"));
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        first.plan_line(),
        "hushrun: unit: every test file runs: no run here has listed Vitest's test files yet"
    );
    r.write(
        "src/pricing.ts",
        "export const price = (x: number) => x + x\n",
    );
    let second = r.run(VITEST_RUN);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        second.plan_line(),
        "hushrun: unit: 3 of 7 test files run; 4 already passed on the files they read, and 1 more that did runs as a check"
    );
    assert!(
        plain(&second.stdout).contains("Test Files  3 passed (3)"),
        "{}",
        second.stdout
    );
    assert!(
        second.stderr.contains("4 test files reused"),
        "{}",
        second.stderr
    );
    // \[ADR 0073\] Why, with the imports Vite's module graph saw.
    r.write(
        "src/pricing.ts",
        "export const price = (x: number) => 2 * x\n",
    );
    let why = r.run(&["why", "unit", "test/cart.test.ts"]);
    assert_eq!(why.code, 0, "{}", why.stderr);
    assert!(
        why.stdout.starts_with(
            "test/cart.test.ts runs: it read src/pricing.ts, which changed\n  test/cart.test.ts imports src/cart.ts imports src/pricing.ts\n  as far as this machine knows: Vitest's list"
        ),
        "{}",
        why.stdout
    );
}

// covers: C-34, C-35
#[test]
fn an_injected_missed_dependency_in_vitest_is_caught_by_an_audit() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    // A test file that fails when a file outside the check's files exists, named by a variable:
    // what no adapter sees. With one other test file, it is the one an audit runs.
    let files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
        file("src/other.ts", "export const other = 1\n"),
        file(
            "test/hidden.test.ts",
            "import { test, expect } from 'vitest'\nimport { lstatSync } from 'node:fs'\n\ntest('hidden', () => {\n  let there = true\n  try { lstatSync(process.env.FLAG_FILE ?? '') } catch { there = false }\n  expect(there).toBe(false)\n})\n",
        ),
        file(
            "test/other.test.ts",
            "import { test, expect } from 'vitest'\nimport { other } from '../src/other'\n\ntest('other', () => { expect(other).toBe(1) })\n",
        ),
    ];
    let r = Repo::new(&files);
    let flag = r.root.join("flag");
    let flag = flag.to_str().unwrap().to_string();
    let env = [("FLAG_FILE", flag.as_str())];
    assert_eq!(r.run_env(VITEST_RUN, &env).code, 0);
    std::fs::write(&flag, "").unwrap();
    r.write("src/other.ts", "export const other = 2 - 1\n");
    let caught = r.run_env(VITEST_RUN, &env);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        caught.plan_line(),
        "hushrun: unit: all 2 test files run; 1 that already passed on the files it read runs as a check"
    );
    assert!(
        caught.stderr.contains("unit: test/hidden.test.ts failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"),
        "{}",
        caught.stderr
    );
}

/// Vitest with hushrun's plugin and a `hushrun.json` written here, as a plan would: its exit code
/// and the main process's facts.
fn plugin_vitest(r: &Repo, reuse: &[&str], args: &[&str]) -> (i32, Value) {
    let units = r.root.join("units");
    let _ = std::fs::remove_dir_all(&units);
    std::fs::create_dir_all(&units).unwrap();
    let listed: Vec<String> = r.map()["unitFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    std::fs::write(
        units.join("hushrun.json"),
        json!({"schema": "hushrun.units/v0", "root": r.repo, "reuse": reuse, "testFiles": listed})
            .to_string(),
    )
    .unwrap();
    let adapter = std::fs::read_dir(r.cache.join("adapters"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("vitest-")
        })
        .unwrap();
    let mut cmd = Command::new(r.repo.join("node_modules/.bin/vitest"));
    Repo::clean(&mut cmd);
    let out = cmd
        .arg("run")
        .args(args)
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", &units)
        .env(
            "HUSHRUN_VITEST",
            format!("file://{}", adapter.join("hushrun-vitest.mjs").display()),
        )
        .output()
        .unwrap();
    let main = std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.starts_with("vitest-")
                && !name.starts_with("vitest-worker")
                && name.ends_with(".json")
        })
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .unwrap_or(Value::Null);
    (out.status.code().unwrap_or(-1), main)
}

// covers: C-32
#[test]
fn the_plugin_deselects_what_it_is_told_and_no_test_files_stays_vitests_own() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = Repo::new(&shop("observe"));
    assert_eq!(r.run(VITEST_RUN).code, 0);
    let every: Vec<String> = r.map()["unitFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let every: Vec<&str> = every.iter().map(String::as_str).collect();
    // Every test file left after Vitest's own filters was reused: what Vitest says of no test
    // files (exit 1) would turn a pass into a fail, so the run passes; and the plugin says exactly
    // what it deselected.
    let (code, main) = plugin_vitest(&r, &every, &[]);
    assert_eq!(code, 0, "{main:#}");
    assert_eq!(main["removedItems"], 7);
    assert!(
        main["units"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["deselected"] == true),
        "{main:#}"
    );
    // Vitest's filter, with one of them, deselected: still a pass.
    let (code, _) = plugin_vitest(&r, &every, &["cart"]);
    assert_eq!(code, 0);
    // A filter that matches nothing stays Vitest's own failure.
    let (code, _) = plugin_vitest(&r, &every, &["no-such-test"]);
    assert_eq!(code, 1);
    // What isn't on the list runs, and its failure is the run's.
    r.write("src/greet.ts", "export const hello = () => 'ho'\n");
    let (code, main) = plugin_vitest(&r, &["test/cart.test.ts"], &[]);
    assert_eq!(code, 1);
    assert_eq!(main["removedItems"], 1);
}

/// What a hazard does to a repository.
struct Hazard {
    name: &'static str,
    files: Vec<(String, String)>,
    change: fn(&Repo),
}

fn hazards() -> Vec<Hazard> {
    let t = |body: &str| format!("import {{ test, expect, vi }} from 'vitest'\n{body}\n");
    vec![
        Hazard {
            name: "a dynamic import",
            files: vec![
                file("src/dyn.ts", "export const dyn = () => 7\n"),
                file(
                    "test/dyn.test.ts",
                    &t(
                        "test('dyn', async () => {\n  const name = 'dyn'\n  const m = await import(`../src/${name}.ts`)\n  expect(m.dyn()).toBe(7)\n})",
                    ),
                ),
            ],
            change: |r| r.write("src/dyn.ts", "export const dyn = () => 8\n"),
        },
        Hazard {
            name: "a source read as data",
            files: vec![
                file("src/data.ts", "export const data = 1\n"),
                file(
                    "test/data.test.ts",
                    &t(
                        "import { readFileSync } from 'node:fs'\ntest('data', () => { expect(readFileSync('src/data.ts', 'utf8')).toContain('= 1') })",
                    ),
                ),
            ],
            change: |r| r.write("src/data.ts", "export const data = 2\n"),
        },
        Hazard {
            name: "a source read only by a child process",
            files: vec![
                file("src/data.ts", "export const data = 1\n"),
                file(
                    "test/child.test.ts",
                    &t(
                        "import { execFileSync } from 'node:child_process'\ntest('child', () => { expect(execFileSync(process.execPath, ['-e', \"process.stdout.write(require('fs').readFileSync('src/data.ts', 'utf8'))\"]).toString()).toContain('= 1') })",
                    ),
                ),
            ],
            change: |r| r.write("src/data.ts", "export const data = 2\n"),
        },
        Hazard {
            name: "package.json, read by a test",
            files: vec![file(
                "test/pkg.test.ts",
                &t(
                    "import { readFileSync } from 'node:fs'\ntest('pkg', () => { expect(readFileSync('package.json', 'utf8')).not.toContain('changed') })",
                ),
            )],
            change: |r| {
                r.write(
                    "package.json",
                    "{\"name\": \"shop\", \"private\": true, \"type\": \"module\", \"description\": \"changed\"}\n",
                );
            },
        },
        Hazard {
            name: "the config, which every test file reads",
            files: vec![
                file(
                    "vitest.config.ts",
                    &config("  define: { __LEVEL__: '\"1\"' },\n"),
                ),
                file(
                    "test/level.test.ts",
                    &t(
                        "declare const __LEVEL__: string\ntest('level', () => { expect(__LEVEL__).toBe('1') })",
                    ),
                ),
            ],
            change: |r| {
                r.write(
                    "vitest.config.ts",
                    &config("  define: { __LEVEL__: '\"2\"' },\n"),
                )
            },
        },
        Hazard {
            name: "a new file an import.meta.glob picks up",
            files: vec![
                file("src/mods/a.ts", "export const a = 1\n"),
                file(
                    "test/glob.test.ts",
                    &t(
                        "test('glob', () => { expect(Object.keys(import.meta.glob('../src/mods/*.ts')).length).toBe(1) })",
                    ),
                ),
            ],
            change: |r| r.write("src/mods/b.ts", "export const b = 2\n"),
        },
        Hazard {
            name: "a new file that shadows a module",
            files: vec![],
            change: |r| r.write("src/pricing.js", "export const price = (x) => x * 3\n"),
        },
        Hazard {
            name: "a setup file",
            files: vec![
                file(
                    "vitest.config.ts",
                    &config("  test: { setupFiles: ['test/setup.ts'] },\n"),
                ),
                file("test/setup.ts", "(globalThis as any).LEVEL = 1\n"),
                file(
                    "test/level.test.ts",
                    &t("test('level', () => { expect((globalThis as any).LEVEL).toBe(1) })"),
                ),
            ],
            change: |r| r.write("test/setup.ts", "(globalThis as any).LEVEL = 2\n"),
        },
        Hazard {
            name: "a shared helper",
            files: vec![
                file("test/helpers.ts", "export const want = 6\n"),
                file(
                    "test/helped.test.ts",
                    &t(
                        "import { want } from './helpers'\nimport { total } from '../src/cart'\ntest('helped', () => { expect(total([1, 2])).toBe(want) })",
                    ),
                ),
            ],
            change: |r| r.write("test/helpers.ts", "export const want = 7\n"),
        },
        Hazard {
            name: "a deleted file a test reads",
            files: vec![
                file("test/fixtures/names.json", "[\"ann\"]\n"),
                file(
                    "test/names.test.ts",
                    &t(
                        "import { readFileSync } from 'node:fs'\ntest('names', () => { expect(JSON.parse(readFileSync('test/fixtures/names.json', 'utf8'))).toEqual(['ann']) })",
                    ),
                ),
            ],
            change: |r| r.remove("test/fixtures/names.json"),
        },
        Hazard {
            name: "a mock",
            files: vec![
                file("src/mocked.ts", "export const mocked = () => 'real'\n"),
                file(
                    "src/__mocks__/mocked.ts",
                    "export const mocked = () => 'mock'\n",
                ),
                file(
                    "test/mock.test.ts",
                    &t(
                        "import { mocked } from '../src/mocked'\nvi.mock('../src/mocked')\ntest('mock', () => { expect(mocked()).toBe('mock') })",
                    ),
                ),
            ],
            change: |r| {
                r.write(
                    "src/__mocks__/mocked.ts",
                    "export const mocked = () => 'other'\n",
                )
            },
        },
        Hazard {
            name: "a new test file in a directory a test lists",
            files: vec![file(
                "test/count.test.ts",
                &t(
                    "import { readdirSync } from 'node:fs'\ntest('count', () => { expect(readdirSync('test').length).toBe(8) })",
                ),
            )],
            change: |r| {
                r.write(
                    "test/more.test.ts",
                    "import { test } from 'vitest'\ntest('more', () => {})\n",
                );
            },
        },
        Hazard {
            name: "a module a Vite plugin makes",
            files: vec![
                file(
                    "vitest.config.ts",
                    &format!(
                        "import {{ readFileSync }} from 'node:fs'\nimport {{ defineConfig }} from 'vitest/config'\n\nconst shop = {{\n  name: 'shop-virtual',\n  resolveId: (id: string) => (id === 'virtual:level' ? '\\0virtual:level' : undefined),\n  load: (id: string) => (id === '\\0virtual:level' ? `export default ${{readFileSync('src/level.ts', 'utf8').length}}` : undefined),\n}}\n\nexport default defineConfig({{\n  {}\n}})\n",
                        LINE.replace("plugins: [", "plugins: [shop, ")
                    ),
                ),
                file("src/level.ts", "export const level = 1\n"),
                file(
                    "test/virtual.test.ts",
                    &t(
                        "// @ts-ignore\nimport length from 'virtual:level'\ntest('virtual', () => { expect(length).toBe(23) })",
                    ),
                ),
            ],
            change: |r| r.write("src/level.ts", "export const level = 10\n"),
        },
    ]
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

// covers: C-34, C-35
#[test]
fn vitest_hazards_miss_no_failure() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    // In `observe`, which runs everything and computes the plan's keys without its audits: for
    // each hazard, every test file that fails is one the plan wouldn't have reused.
    let mut reused = 0;
    for hazard in hazards() {
        let mut files = shop("observe");
        for (path, text) in &hazard.files {
            files.retain(|(p, _)| p != path);
            files.push((path.clone(), text.clone()));
        }
        let r = Repo::new(&files);
        let first = r.run(VITEST_RUN);
        assert_eq!(
            first.code, 0,
            "{}: {}\n{}",
            hazard.name, first.stdout, first.stderr
        );
        (hazard.change)(&r);
        let failing = r.truth();
        assert!(
            !failing.is_empty(),
            "{}: nothing fails, so it tests nothing",
            hazard.name
        );
        let ran = r.run(VITEST_RUN);
        assert_ne!(ran.code, 0, "{}: {}", hazard.name, ran.stderr);
        let missed = missed(&ran.stderr);
        assert!(
            missed.is_empty(),
            "{}: the plan would have reused what fails ({failing:?}): {missed:?}\n{}",
            hazard.name,
            ran.stderr
        );
        reused += would_reuse(&ran.stderr);
    }
    assert!(reused > 0);
}

// ---------------------------------------------------------------------------------------------
// [ADR 0081] Tests one by one (C-39, C-43).

/// The plugin and its setup file, as hushrun writes them.
const PLUGIN: &str = include_str!("../src/units/hushrun_vitest.mjs");
const SETUP: &str = include_str!("../src/units/hushrun_vitest_setup.mjs");

/// What Vitest with hushrun's plugin did, with tests told one by one.
struct Told {
    code: i32,
    stdout: String,
    /// Each test file's worker facts, by its path from the root.
    workers: std::collections::BTreeMap<String, Value>,
}

impl Told {
    fn unit(&self, unit: &str) -> &Value {
        self.workers
            .get(unit)
            .unwrap_or_else(|| panic!("no facts for {unit}: {:#?}", self.workers.keys()))
    }

    /// Each test of `unit` that the worker told, by id.
    fn tests(&self, unit: &str) -> std::collections::BTreeMap<String, Value> {
        self.unit(unit)["units"][0]["tests"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} told no tests: {:#}", self.unit(unit)))
            .iter()
            .map(|t| (t["id"].as_str().unwrap().to_string(), t.clone()))
            .collect()
    }

    fn ids(&self, unit: &str) -> Vec<String> {
        self.unit(unit)["units"][0]["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// The paths a list of indices names in `unit`'s facts, in order; `None` for no list.
    fn names(&self, unit: &str, indices: &Value) -> Option<Vec<String>> {
        let paths = self.unit(unit)["paths"].as_array().unwrap();
        let mut out: Vec<String> = indices
            .as_array()?
            .iter()
            .map(|i| paths[i.as_u64().unwrap() as usize].as_str().unwrap().to_string())
            .collect();
        out.sort();
        Some(out)
    }

    /// What one test of `unit` ran, opened or listed (`what`).
    fn list(&self, unit: &str, id: &str, what: &str) -> Option<Vec<String>> {
        let tests = self.tests(unit);
        let test = tests
            .get(id)
            .unwrap_or_else(|| panic!("no test {id:?} in {:?}", tests.keys()));
        self.names(unit, &test[what])
    }

    /// Each file's direct imports in `unit`'s facts, by path.
    fn imports(&self, unit: &str) -> std::collections::BTreeMap<String, Vec<String>> {
        let facts = self.unit(unit);
        let paths = facts["paths"].as_array().unwrap();
        facts["imports"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(from, to)| {
                let from = paths[from.parse::<usize>().unwrap()].as_str().unwrap();
                (from.to_string(), self.names(unit, to).unwrap())
            })
            .collect()
    }
}

/// The test files of `r`, as Vitest's default `include` names them here.
fn test_files(r: &Repo) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(&path, root, out);
            } else if name.ends_with(".test.ts") {
                let rel = path.strip_prefix(root).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(&r.repo, &r.repo, &mut out);
    out.sort();
    out
}

/// Vitest with hushrun's plugin and a `hushrun.json` written here, as a plan with `unit: test`
/// would: `reads`, and the tests it may leave out.
fn tests_vitest(
    r: &Repo,
    reads: &str,
    reuse_tests: Value,
    args: &[&str],
    env: &[(&str, &str)],
) -> Told {
    let units = r.root.join("units");
    let _ = std::fs::remove_dir_all(&units);
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(
        units.join("hushrun.json"),
        json!({"schema": "hushrun.units/v0", "root": r.repo, "reuse": [],
               "testFiles": test_files(r), "unit": "test", "reads": reads,
               "reuseTests": reuse_tests})
        .to_string(),
    )
    .unwrap();
    let adapter = r.root.join("adapter");
    std::fs::create_dir_all(&adapter).unwrap();
    std::fs::write(adapter.join("hushrun-vitest.mjs"), PLUGIN).unwrap();
    std::fs::write(adapter.join("hushrun-vitest-setup.mjs"), SETUP).unwrap();
    let mut cmd = Command::new(r.repo.join("node_modules/.bin/vitest"));
    Repo::clean(&mut cmd);
    let out = cmd
        .arg("run")
        .args(args)
        .current_dir(&r.repo)
        .env("HUSHRUN_UNITS", &units)
        .env(
            "HUSHRUN_VITEST",
            format!("file://{}", adapter.join("hushrun-vitest.mjs").display()),
        )
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let mut workers = std::collections::BTreeMap::new();
    for path in std::fs::read_dir(&units).unwrap().flatten().map(|e| e.path()) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("vitest-worker-") || !name.ends_with(".json") {
            continue;
        }
        let facts: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let unit = std::fs::canonicalize(facts["units"][0]["unit"].as_str().unwrap()).unwrap();
        let unit = unit
            .strip_prefix(std::fs::canonicalize(&r.repo).unwrap())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        workers.insert(unit, facts);
    }
    Told {
        code: out.status.code().unwrap_or(-1),
        stdout: plain(&String::from_utf8_lossy(&out.stdout))
            + &plain(&String::from_utf8_lossy(&out.stderr)),
        workers,
    }
}

/// A repository with `files` besides the config and package.json.
fn told_repo(files: &[(&str, &str)]) -> Repo {
    let mut all = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
    ];
    all.extend(files.iter().map(|(p, t)| file(p, t)));
    Repo::new(&all)
}

const NAMES: &str = "import { describe, test, expect, beforeAll } from 'vitest'
import { writeFileSync } from 'node:fs'
describe('cart', () => {
  test('total', () => {})
  test('total', () => {})
  describe('in euros', () => {
    beforeAll(() => { writeFileSync(process.env.MARK ?? '/dev/null', 'ran') })
    test('rounds', () => {})
  })
  test.each([1, 2])('adds %i', (n) => { expect(n).toBeGreaterThan(0) })
})
describe('a > b', () => { test('c', () => {}) })
describe('a', () => { describe('b', () => { test('c', () => {}) }) })
test('top', () => {})
test.skip('skipped', () => {})
test.todo('later')
";

// covers: C-43, C-39
#[test]
fn vitest_names_each_test_by_its_suites_and_leaves_out_the_listed_ones_before_their_hooks() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = told_repo(&[
        ("test/names.test.ts", NAMES),
        (
            "test/other.test.ts",
            "import { test } from 'vitest'\ntest('one', () => {})\ntest('two', () => {})\n",
        ),
    ]);
    let mark = r.root.join("mark");
    let told = tests_vitest(&r, "file", json!({}), &[], &[]);
    assert_eq!(told.code, 0, "{}", told.stdout);
    // Suites' names and its own, ` [2]` for the second of a name, `test.each`'s cases each a test;
    // a skipped test and a todo passed; the two that join to `a > b > c` from different suites are
    // never told, so never left out.
    assert_eq!(
        told.ids("test/names.test.ts"),
        [
            "cart > total",
            "cart > total [2]",
            "cart > in euros > rounds",
            "cart > adds 1",
            "cart > adds 2",
            "top",
            "skipped",
            "later",
        ]
    );
    let tests = told.tests("test/names.test.ts");
    for (id, test) in &tests {
        assert_eq!(
            (&test["finished"], &test["failed"], &test["deselected"]),
            (&json!(true), &json!(false), &json!(false)),
            "{id}"
        );
        // With `reads: file`, no test is watched.
        assert!(test.get("ran").is_none(), "{id}: {test}");
    }

    // The listed tests are left out after collection: a suite all of whose tests are left out
    // doesn't run its `beforeAll`, and what Vitest says is a pass, with the tests skipped.
    std::fs::write(&mark, "").unwrap();
    let listed = json!({"test/names.test.ts": ["cart > total [2]", "cart > in euros > rounds",
                                               "cart > adds 1", "a > b > c", "skipped", "gone"]});
    let told = tests_vitest(&r, "file", listed, &[], &[("MARK", mark.to_str().unwrap())]);
    assert_eq!(told.code, 0, "{}", told.stdout);
    assert_eq!(std::fs::read_to_string(&mark).unwrap(), "", "beforeAll ran");
    // Vitest counts the tests of a suite it skipped whole in its total only: it reports no
    // result for them.
    assert!(
        told.stdout.contains("Tests  7 passed | 3 skipped | 1 todo (12)"),
        "{}",
        told.stdout
    );
    let tests = told.tests("test/names.test.ts");
    let left: Vec<&String> = tests
        .iter()
        .filter(|(_, t)| t["deselected"] == true)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        left,
        ["cart > adds 1", "cart > in euros > rounds", "cart > total [2]"]
    );
    for id in &left {
        assert_eq!(tests[*id]["finished"], false, "{id}");
    }
    // A skipped test isn't left out: Vitest skips it anyway, and it passed.
    assert_eq!(tests["skipped"]["deselected"], false);
    assert_eq!(tests["skipped"]["finished"], true);

    // Every test of one file left out: the file is skipped, and the run still passes.
    let every = json!({"test/other.test.ts": ["one", "two"]});
    let told = tests_vitest(&r, "file", every, &[], &[]);
    assert_eq!(told.code, 0, "{}", told.stdout);
    assert!(
        told.stdout.contains("Test Files  1 passed | 1 skipped (2)"),
        "{}",
        told.stdout
    );
    assert!(
        told.tests("test/other.test.ts")
            .values()
            .all(|t| t["deselected"] == true)
    );
    // Every test of every file: still a pass.
    let names: Vec<String> = told.ids("test/names.test.ts");
    let all = json!({"test/other.test.ts": ["one", "two"], "test/names.test.ts": names});
    let told = tests_vitest(&r, "file", all, &[], &[]);
    assert_eq!(told.code, 0, "{}", told.stdout);
}
