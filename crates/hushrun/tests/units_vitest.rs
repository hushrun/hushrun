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

    /// \[ADR 0081\] The newest run result this machine recorded.
    fn newest_result(&self) -> Value {
        fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.extension().is_some_and(|e| e == "json") {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        collect(&self.cache.join("results/results/v0"), &mut files);
        files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
        let newest = files.last().expect("a result");
        serde_json::from_slice(&std::fs::read(newest).unwrap()).unwrap()
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
            .map(|i| {
                paths[i.as_u64().unwrap() as usize]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
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
    for path in std::fs::read_dir(&units)
        .unwrap()
        .flatten()
        .map(|e| e.path())
    {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("vitest-worker-") || !name.ends_with(".json") {
            continue;
        }
        let facts: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // A worker that tells tests names its unit from the root (ADR 0081).
        let unit = facts["units"][0]["unit"].as_str().unwrap().to_string();
        assert!(!Path::new(&unit).is_absolute(), "{unit}");
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
    // a skipped test and a todo passed; the two that join to `a > b > c` from different suites
    // are told once, by that id, and never left out.
    assert_eq!(
        told.ids("test/names.test.ts"),
        [
            "cart > total",
            "cart > total [2]",
            "cart > in euros > rounds",
            "cart > adds 1",
            "cart > adds 2",
            "a > b > c",
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
        told.stdout
            .contains("Tests  7 passed | 3 skipped | 1 todo (12)"),
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
        [
            "cart > adds 1",
            "cart > in euros > rounds",
            "cart > total [2]"
        ]
    );
    for id in &left {
        assert_eq!(tests[*id]["finished"], false, "{id}");
    }
    // A skipped test isn't left out: Vitest skips it anyway, and it passed.
    assert_eq!(tests["skipped"]["deselected"], false);
    assert_eq!(tests["skipped"]["finished"], true);
    // Two tests that share an id ran, though listed, and say they are never left out.
    assert_eq!(
        (
            &tests["a > b > c"]["deselected"],
            &tests["a > b > c"]["finished"],
            &tests["a > b > c"]["neverLeftOut"]
        ),
        (&json!(false), &json!(true), &json!(true))
    );

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

const OUTCOMES: &str = "import { describe, test, expect, beforeAll, afterAll } from 'vitest'
describe('opens', () => {
  beforeAll(() => { throw new Error('no database') })
  test('one', () => {})
  test('two', () => {})
})
describe('closes', () => {
  afterAll(() => { throw new Error('left open') })
  test('three', () => {})
})
test('fails', () => { expect(1).toBe(2) })
let tries = 0
test('flaky', { retry: 2 }, () => { tries += 1; expect(tries).toBe(2) })
test.fails('known bug', () => { expect(1).toBe(2) })
test('skips itself', (ctx) => { ctx.skip() })
test('plain', () => {})
";

// covers: C-43, C-39
#[test]
fn each_vitest_test_ends_as_vitest_says_and_a_filter_tells_nothing() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = told_repo(&[
        ("test/outcomes.test.ts", OUTCOMES),
        (
            "test/broken.test.ts",
            "import { test } from 'vitest'\nimport '../src/missing'\ntest('never', () => {})\n",
        ),
        (
            "test/only.test.ts",
            "import { test } from 'vitest'\ntest.only('chosen', () => {})\ntest('other', () => {})\ntest.skip('skipped', () => {})\n",
        ),
    ]);
    let told = tests_vitest(&r, "test", json!({}), &[], &[]);
    assert_eq!(told.code, 1, "{}", told.stdout);
    let tests = told.tests("test/outcomes.test.ts");
    let ended = |id: &str| {
        let t = &tests[id];
        (
            t["finished"].as_bool().unwrap(),
            t["failed"].as_bool().unwrap(),
        )
    };
    // A failing `beforeAll` fails its file (Vitest says so of it), and its tests didn't finish.
    assert_eq!(ended("opens > one"), (false, false));
    assert_eq!(ended("opens > two"), (false, false));
    // A failing `afterAll` fails the tests it ended: they don't pass, and run again.
    assert_eq!(ended("closes > three"), (true, true));
    assert_eq!(ended("fails"), (true, true));
    // Retries, `test.fails` and a test that skipped itself: Vitest's word.
    assert_eq!(ended("flaky"), (true, false));
    assert_eq!(ended("known bug"), (true, false));
    assert_eq!(ended("skips itself"), (true, false));
    assert_eq!(ended("plain"), (true, false));
    assert!(tests["plain"]["durationMs"].is_u64());
    // A test that skipped itself ends with no `onAfterRunTask`: the tests after it are still
    // watched alone.
    assert!(tests["plain"]["ran"].is_array(), "{}", tests["plain"]);
    // A file that can't be collected tells no test: hushrun's main process says it failed.
    assert_eq!(told.ids("test/broken.test.ts"), Vec::<String>::new());
    // `.only` leaves the others out: they aren't tests that passed, and aren't told; a skipped
    // test is.
    assert_eq!(told.ids("test/only.test.ts"), ["chosen", "skipped"]);

    // Vitest's own name filter: what it left out isn't told either.
    let told = tests_vitest(&r, "file", json!({}), &["-t", "plain|three"], &[]);
    assert_eq!(
        told.ids("test/outcomes.test.ts"),
        ["closes > three", "plain"]
    );
}

const WATCHED: &str =
    "import { describe, test, expect, beforeAll, afterAll, beforeEach } from 'vitest'
import { readFileSync, readdirSync } from 'node:fs'
import { used, K } from '../src/lib'
import { connect, close } from '../src/db'
import { hook } from '../src/each'
import { NEAR } from '../src/near'
import { atLoad } from '../src/load'
const loaded = atLoad()
describe('db', () => {
  beforeAll(() => { connect() })
  afterAll(() => { close() })
  beforeEach(() => { hook() })
  test('uses a function', () => { expect(used(2)).toBe(4) })
  test('uses a constant', () => { expect(K).toBe(5) })
})
test('reads a file', () => {
  expect(readFileSync('data/fixture.json', 'utf8')).toContain('1')
  expect(readdirSync('data').length).toBe(1)
})
test('nothing else', () => { expect(loaded).toBe(1) })
test('a constant next door', () => { expect(NEAR).toBe(2) })
";

fn watched_repo() -> Repo {
    told_repo(&[
        ("src/rate.ts", "export const RATE = 2\n"),
        (
            "src/lib.ts",
            "import { RATE } from './rate'\nexport function used(x: number) { return x * RATE }\nexport const K = 5\n",
        ),
        (
            "src/db.ts",
            "export function connect() { return 'db' }\nexport function close() { return 'closed' }\n",
        ),
        ("src/each.ts", "export function hook() { return 1 }\n"),
        ("src/far.ts", "export const FAR = 1\n"),
        (
            "src/near.ts",
            "import { FAR } from './far'\nexport const NEAR = FAR + 1\n",
        ),
        ("src/load.ts", "export function atLoad() { return 1 }\n"),
        ("data/fixture.json", "[1]\n"),
        ("test/watched.test.ts", WATCHED),
        (
            "test/together.test.ts",
            "import { describe, test } from 'vitest'\nimport { hook } from '../src/each'\ntest('alone', () => { hook() })\ndescribe.concurrent('at once', () => {\n  test('a', async () => { await new Promise((r) => setTimeout(r, 5)) })\n  test('b', async () => { hook() })\n})\n",
        ),
    ])
}

// covers: C-43
#[test]
fn vitest_watches_what_each_test_ran_opened_and_listed() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = watched_repo();
    let told = tests_vitest(&r, "test", json!({}), &[], &[]);
    assert_eq!(told.code, 0, "{}", told.stdout);
    let unit = "test/watched.test.ts";
    let ran = |id: &str| told.list(unit, id, "ran").unwrap();
    // The files a function of which was called while it ran, its `beforeEach` included; an
    // import's getter is a function, so a constant read from another module counts.
    assert_eq!(
        ran("db > uses a function"),
        ["src/each.ts", "src/lib.ts", "src/rate.ts", unit]
    );
    assert_eq!(
        ran("db > uses a constant"),
        ["src/each.ts", "src/lib.ts", unit]
    );
    // What it opened and listed.
    assert_eq!(ran("reads a file"), [unit]);
    assert_eq!(
        told.list(unit, "reads a file", "opened").unwrap(),
        ["data/fixture.json"]
    );
    assert_eq!(told.list(unit, "reads a file", "dirs").unwrap(), ["data"]);
    assert_eq!(
        told.list(unit, "nothing else", "opened").unwrap(),
        Vec::<String>::new()
    );
    // A module's top-level code isn't a function called: far.ts made NEAR when it was imported.
    assert_eq!(ran("a constant next door"), ["src/near.ts", unit]);
    assert_eq!(ran("nothing else"), [unit]);
    // Each module's direct imports, as the worker evaluated them.
    let imports = told.imports(unit);
    assert_eq!(imports["src/near.ts"], ["src/far.ts"]);
    assert_eq!(imports["src/lib.ts"], ["src/rate.ts"]);
    assert_eq!(
        imports[unit],
        [
            "src/db.ts",
            "src/each.ts",
            "src/lib.ts",
            "src/load.ts",
            "src/near.ts"
        ]
    );
    // A suite's `beforeAll` and `afterAll`, for the tests of the suite.
    let facts = &told.unit(unit)["units"][0];
    let shared: Vec<(Vec<u64>, Vec<String>)> = facts["shared"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| !s["ran"].as_array().unwrap().is_empty())
        .map(|s| {
            let tests = s["tests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_u64().unwrap());
            (tests.collect(), told.names(unit, &s["ran"]).unwrap())
        })
        .collect();
    let ids = told.ids(unit);
    let at = |id: &str| ids.iter().position(|i| i == id).unwrap() as u64;
    assert_eq!(
        shared,
        [(
            vec![at("db > uses a function"), at("db > uses a constant")],
            vec!["src/db.ts".to_string(), unit.to_string()]
        )]
    );
    // What loading the file ran, for every test: a function called at import, and far.ts, whose
    // FAR near.ts read through its getter when it was imported; and what Vitest loads for every
    // test, the config.
    assert_eq!(
        told.names(unit, &facts["loaded"]["ran"]).unwrap(),
        ["src/far.ts", "src/load.ts", unit]
    );
    assert_eq!(
        told.names(unit, &facts["everyTest"]).unwrap(),
        ["vitest.config.ts"]
    );

    // Two tests at once can't be watched alone: no test of that file says what it ran, so each
    // gets its file's list.
    let together = told.tests("test/together.test.ts");
    assert_eq!(together.len(), 3);
    for (id, test) in &together {
        assert_eq!(test["finished"], true, "{id}");
        assert!(test.get("ran").is_none(), "{id}: {test}");
        assert!(test.get("opened").is_none(), "{id}: {test}");
    }
    // `reads: file` watches nothing.
    let told = tests_vitest(&r, "file", json!({}), &[], &[]);
    assert!(told.tests(unit).values().all(|t| t.get("ran").is_none()));
    assert!(told.unit(unit)["units"][0].get("loaded").is_none());
}

// covers: C-43, C-39
#[test]
fn a_program_one_vitest_test_starts_is_that_tests_alone() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = told_repo(&[
        (
            "test/spawn.test.ts",
            "import { describe, test, beforeAll } from 'vitest'\nimport { execFileSync } from 'node:child_process'\nconst node = process.execPath\ntest('spawns', () => { execFileSync(node, ['-e', '1']) })\ntest('plain', () => {})\ndescribe('served', () => {\n  beforeAll(() => { execFileSync(node, ['-e', '2']) })\n  test('one', () => {})\n})\n",
        ),
        (
            "test/load.test.ts",
            "import { test } from 'vitest'\nimport { execFileSync } from 'node:child_process'\nexecFileSync(process.execPath, ['-e', '3'])\ntest('any', () => {})\n",
        ),
    ]);
    let told = tests_vitest(&r, "test", json!({}), &[], &[]);
    assert_eq!(told.code, 0, "{}", told.stdout);
    let unit = "test/spawn.test.ts";
    let tests = told.tests(unit);
    let why = |id: &str| -> Vec<String> {
        tests[id]["notFollowed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["why"].as_str().unwrap().to_string())
            .collect()
    };
    // Inside one test's window: its own; in a suite's `beforeAll`: the tests it serves'.
    assert_eq!(why("spawns"), ["subprocess"]);
    assert_eq!(why("plain"), Vec::<String>::new());
    assert_eq!(why("served > one"), ["subprocess"]);
    assert_eq!(told.unit(unit)["units"][0]["notFollowed"], json!([]));
    // While the file loaded: the file's, every test's.
    let load = &told.unit("test/load.test.ts")["units"][0];
    assert_eq!(load["notFollowed"][0]["why"], "subprocess");
    assert_eq!(load["tests"][0]["notFollowed"], json!([]));
}

/// A shop with tests one by one: a cart test file with a test that fails while FAIL is set, and
/// greet test files that pass.
fn told_shop(tests: &str) -> Vec<(String, String)> {
    let mut files = shop(tests);
    files.retain(|(p, _)| p != "test/cart.test.ts");
    files.push(file(
        "test/cart.test.ts",
        "import { describe, test, expect } from 'vitest'\nimport { total } from '../src/cart'\n\ntest('total', () => { expect(total([1, 2])).toBe(6) })\ntest('flips', () => { expect(process.env.FAIL).toBeUndefined() })\ndescribe('more', () => { test('three', () => { expect(total([1])).toBe(2) }) })\n",
    ));
    files
}

// covers: C-43, C-39
#[test]
fn with_tests_as_the_unit_vitest_runs_again_only_the_test_that_failed() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = Repo::new(&told_shop("{mode: reuse, unit: test}"));
    let first = r.run_env(VITEST_RUN, &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = r.map();
    assert_eq!(
        map["units"]["test/cart.test.ts"]["tests"],
        json!({"flips": "failed", "more > three": "passed", "total": "passed"}),
        "{map:#}"
    );
    // The same files, the flip fixed: cart's own key failed, so its tests are asked one by one.
    // Of those that passed, one runs as a check, and of the files that passed whole, one.
    let second = r.run(VITEST_RUN);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        second
            .stderr
            .lines()
            .find(|l| l.contains(" tests run, in "))
            .unwrap_or_else(|| panic!("{}", second.stderr)),
        "hushrun: unit: 3 of 9 tests run, in 2 of 7 test files; 6 already passed on the files they read, and 2 more that did run as a check"
    );
    // Vitest ran flips, the audited test and the audited file's, and left out the reused test:
    // skipped, or, when the audit chose `total`, in a suite skipped whole and counted in the total
    // only.
    let stdout = plain(&second.stdout);
    assert!(
        stdout.contains("Tests  3 passed | 1 skipped (4)")
            || stdout.contains("Tests  3 passed (4)"),
        "{}",
        second.stdout
    );
    let result = r.newest_result();
    assert_eq!(result["result"], "pass");
    // The tests Vitest told: those of the two files it ran.
    assert_eq!(
        result["units"]["tests"],
        json!({"total": 4, "ran": 3, "reused": 1})
    );
    // A reused test keeps the row it stands on; flips passed now.
    let map = r.map();
    assert_eq!(
        map["units"]["test/cart.test.ts"]["tests"],
        json!({"flips": "passed", "more > three": "passed", "total": "passed"}),
        "{map:#}"
    );
    // The same code again: the check skips whole.
    let again = r.run(VITEST_RUN);
    assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
}

// covers: C-43, C-39, C-34
#[test]
fn an_audited_vitest_test_that_fails_again_alone_suspends_reuse() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    // One test file, two tests: one fails while FAIL is set, the other while HIDDEN is, which no
    // file says, as a missed dependency would.
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
        file(
            "test/two.test.ts",
            "import { test, expect } from 'vitest'\ntest('ok', () => { expect(process.env.HIDDEN).toBeUndefined() })\ntest('flips', () => { expect(process.env.FAIL).toBeUndefined() })\n",
        ),
    ];
    let r = Repo::new(&files);
    assert_eq!(r.run_env(VITEST_RUN, &[("FAIL", "1")]).code, 1);
    // ok passed on these files, and is the one test that could be reused: it runs as a check,
    // and fails. It runs once more, alone, and fails again: a disagreement.
    let caught = r.run_env(VITEST_RUN, &[("HIDDEN", "1")]);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert!(
        caught.stderr.contains(
            "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs as a check"
        ),
        "{}",
        caught.stderr
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: test/two.test.ts::ok failed, though it passed before on the same files it read: running it once more"
        ),
        "{}",
        caught.stderr
    );
    // The second run ran ok alone: flips was left out one by one.
    assert!(
        plain(&caught.stdout).contains("Tests  1 failed | 1 skipped (2)"),
        "{}",
        caught.stdout
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: test/two.test.ts::ok failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(store::disagreements(&r.cache), ["test/two.test.ts::ok"]);
}

/// A coverage provider that measures nothing, so that Vitest's coverage is on with no package.
const NO_COVERAGE: &str = "const self = new URL(import.meta.url).pathname
export default {
  getProvider() {
    let options
    return {
      name: 'none',
      initialize(ctx) { options = { ...ctx.config.coverage, customProviderModule: self } },
      resolveOptions() { return options },
      clean() {}, onAfterSuiteRun() {}, generateCoverage() { return {} }, reportCoverage() {},
    }
  },
  startCoverage() {}, takeCoverage() { return {} }, stopCoverage() {},
}
";

// covers: C-43
#[test]
fn with_vitests_coverage_on_no_test_is_left_out_or_watched() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = told_repo(&[
        ("cov.mjs", NO_COVERAGE),
        (
            "test/two.test.ts",
            "import { test } from 'vitest'\ntest('one', () => {})\ntest('two', () => {})\n",
        ),
    ]);
    r.write(
        "vitest.config.ts",
        &config("  test: { coverage: { enabled: true, provider: 'custom', customProviderModule: './cov.mjs' } },\n"),
    );
    // Coverage shares V8's counters with watching, and a partial run's coverage would read as a
    // drop: every test runs, and none says what it ran.
    let told = tests_vitest(&r, "test", json!({"test/two.test.ts": ["one"]}), &[], &[]);
    assert_eq!(told.code, 0, "{}", told.stdout);
    let tests = told.tests("test/two.test.ts");
    for (id, test) in &tests {
        assert_eq!(
            (&test["finished"], &test["deselected"]),
            (&json!(true), &json!(false)),
            "{id}"
        );
        assert!(test.get("ran").is_none(), "{id}: {test}");
    }
}

// ---------------------------------------------------------------------------------------------
// [ADR 0081] `reads: test` through `hushrun run` (C-40, C-43): what a watched test's own list
// catches, and what Vitest makes of the two misses RFC 0004 §3.2 names.

/// A repository for `reads`, with `files` and a test file every change runs.
fn reads_repo(files: &[(&str, &str)], tests: &str) -> Repo {
    let mut all = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
        file(
            "test/other.test.ts",
            "import { test } from 'vitest'\ntest('other', () => {})\n",
        ),
    ];
    for (path, text) in files {
        all.retain(|(p, _)| p != path);
        all.push(file(path, text));
    }
    Repo::new(&all)
}

/// Whether `observe`, with `reads`, would have reused `target` of `test/a.test.ts` after `change`
/// made it fail: what its disagreement line says.
fn would_reuse_test(files: &[(&str, &str)], change: (&str, &str), reads: &str) -> bool {
    let r = reads_repo(
        files,
        &format!("{{mode: observe, unit: test, reads: {reads}}}"),
    );
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(
        !first.stderr.contains("each test is taken to have read"),
        "{}",
        first.stderr
    );
    r.write(change.0, change.1);
    let second = r.run(VITEST_RUN);
    assert_eq!(
        second.code, 1,
        "the change makes target fail:\n{}\n{}",
        second.stdout, second.stderr
    );
    assert!(
        plain(&second.stdout).contains("target"),
        "{}",
        second.stdout
    );
    second.stderr.contains(
        "hushrun: unit: test/a.test.ts::target failed, though it passed before on the same files it read: with `tests: reuse` it would have been reused",
    )
}

/// `reads: test` follows the change: neither setting would reuse the test.
fn caught_by_vitest(files: &[(&str, &str)], change: (&str, &str)) {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    assert!(!would_reuse_test(files, change, "file"), "reads: file");
    assert!(!would_reuse_test(files, change, "test"), "reads: test");
}

/// A known miss of `reads: test` (RFC 0004 §3.2): `reads: file` runs the test, `reads: test`
/// would reuse it. Whoever closes the miss changes the second assertion.
fn known_miss_of_vitest(files: &[(&str, &str)], change: (&str, &str)) {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    assert!(!would_reuse_test(files, change, "file"), "reads: file");
    assert!(
        would_reuse_test(files, change, "test"),
        "reads: test: a known miss"
    );
}

const T: &str = "import { describe, test, expect, beforeAll } from 'vitest'\n";

// covers: C-40, C-43
#[test]
fn vitest_reads_test_follows_a_function_called() {
    caught_by_vitest(
        &[
            ("src/calc.ts", "export function f() { return 1 }\n"),
            (
                "src/facade.ts",
                "import { f } from './calc'\nexport function g() { return f() }\n",
            ),
            (
                "test/a.test.ts",
                &format!(
                    "{T}import {{ g }} from '../src/facade'\ntest('target', () => {{ expect(g()).toBe(1) }})\ntest('plain', () => {{}})\n"
                ),
            ),
        ],
        ("src/calc.ts", "export function f() { return 2 }\n"),
    );
}

// covers: C-40, C-43
#[test]
fn vitest_reads_test_follows_a_constant_the_code_that_ran_imports_directly() {
    caught_by_vitest(
        &[
            ("src/consts.ts", "export const K = 1\n"),
            (
                "src/facade.ts",
                "import { K } from './consts'\nexport function g() { return K }\n",
            ),
            (
                "test/a.test.ts",
                &format!(
                    "{T}import {{ g }} from '../src/facade'\ntest('target', () => {{ expect(g()).toBe(1) }})\ntest('plain', () => {{}})\n"
                ),
            ),
        ],
        ("src/consts.ts", "export const K = 2\n"),
    );
}

// covers: C-40, C-43
#[test]
fn vitest_reads_test_follows_a_constant_the_test_file_imports_directly() {
    caught_by_vitest(
        &[
            ("src/consts.ts", "export const K = 1\n"),
            (
                "test/a.test.ts",
                &format!(
                    "{T}import {{ K }} from '../src/consts'\ntest('target', () => {{ expect(K).toBe(1) }})\ntest('plain', () => {{}})\n"
                ),
            ),
        ],
        ("src/consts.ts", "export const K = 2\n"),
    );
}

// covers: C-40, C-43
#[test]
fn vitest_reads_test_follows_a_file_opened() {
    caught_by_vitest(
        &[
            ("data/value.txt", "1\n"),
            (
                "test/a.test.ts",
                &format!(
                    "{T}import {{ readFileSync }} from 'node:fs'\ntest('target', () => {{ expect(readFileSync('data/value.txt', 'utf8').trim()).toBe('1') }})\ntest('plain', () => {{}})\n"
                ),
            ),
        ],
        ("data/value.txt", "2\n"),
    );
}

// covers: C-40, C-43
#[test]
fn vitest_reads_test_follows_a_file_a_module_read_once_and_kept() {
    // first opens the file through prices.ts, which keeps what it read; target, after it, gets
    // the kept value and opens nothing.
    caught_by_vitest(
        &[
            ("data/price.txt", "5\n"),
            (
                "src/prices.ts",
                "import { readFileSync } from 'node:fs'\nlet kept: string | undefined\nexport function price() {\n  kept ??= readFileSync('data/price.txt', 'utf8').trim()\n  return kept\n}\n",
            ),
            (
                "test/a.test.ts",
                &format!(
                    "{T}import {{ price }} from '../src/prices'\ntest('first', () => {{ expect(price()).toBeTruthy() }})\ntest('target', () => {{ expect(price()).toBe('5') }})\n"
                ),
            ),
        ],
        ("data/price.txt", "6\n"),
    );
}

// covers: C-40, C-43
#[test]
fn vitest_reads_test_follows_what_a_before_all_read() {
    caught_by_vitest(
        &[
            ("src/helper.ts", "export function make() { return 1 }\n"),
            (
                "src/fx.ts",
                "import { make as made } from './helper'\nexport function make() { return made() }\n",
            ),
            (
                "test/a.test.ts",
                &format!(
                    "{T}import {{ make }} from '../src/fx'\ndescribe('served', () => {{\n  let value = 0\n  beforeAll(() => {{ value = make() }})\n  test('target', () => {{ expect(value).toBe(1) }})\n}})\ntest('plain', () => {{}})\n"
                ),
            ),
        ],
        ("src/helper.ts", "export function make() { return 2 }\n"),
    );
}

// covers: C-40, C-43
#[test]
fn vitest_reads_test_follows_a_module_imported_lazily_inside_a_test() {
    caught_by_vitest(
        &[
            ("src/lazy.ts", "export const K = 1\n"),
            (
                "test/a.test.ts",
                &format!(
                    "{T}test('target', async () => {{\n  const lazy = await import('../src/lazy')\n  expect(lazy.K).toBe(1)\n}})\ntest('plain', () => {{}})\n"
                ),
            ),
        ],
        ("src/lazy.ts", "export const K = 2\n"),
    );
}

/// Two imports away: cart.ts ran and imports settings.ts, which built TAX from defaults.ts, with
/// no function of either called by the test.
fn vitest_two_imports_away() -> Vec<(&'static str, &'static str)> {
    vec![
        ("src/defaults.ts", "export const RATE = 3\n"),
        (
            "src/settings.ts",
            "import { RATE } from './defaults'\nexport const TAX = RATE * 2\n",
        ),
        (
            "src/cart.ts",
            "import { TAX } from './settings'\nexport function total(xs: number[]) { return xs.reduce((a, b) => a + b, 0) + TAX }\n",
        ),
        (
            "test/a.test.ts",
            "import { test, expect } from 'vitest'\nimport { total } from '../src/cart'\ntest('target', () => { expect(total([1])).toBe(7) })\ntest('plain', () => {})\n",
        ),
    ]
}

// covers: C-40, C-43
#[test]
fn vitest_catches_a_value_two_imports_away_which_reads_test_may_miss_elsewhere() {
    // RFC 0004 §3.2's first miss doesn't happen with Vitest: settings.ts read RATE through
    // defaults.ts's export getter while the file loaded, a function of defaults.ts, so every test
    // of the file has it (ADR 0081).
    caught_by_vitest(
        &vitest_two_imports_away(),
        ("src/defaults.ts", "export const RATE = 4\n"),
    );
}

/// A registry plugins.ts fills as boot.ts imports it: the test file imports boot.ts for that and
/// the registry, and nothing of plugins.ts, whose code runs at import only.
fn vitest_registry() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "src/registry.ts",
            "export const HANDLERS: Record<string, number> = {}\n",
        ),
        (
            "src/plugins.ts",
            "import { HANDLERS } from './registry'\nHANDLERS.x = 41\n",
        ),
        ("src/boot.ts", "import './plugins'\n"),
        (
            "test/a.test.ts",
            "import { test, expect } from 'vitest'\nimport '../src/boot'\nimport { HANDLERS } from '../src/registry'\ntest('target', () => { expect(HANDLERS.x).toBe(41) })\ntest('plain', () => {})\n",
        ),
    ]
}

// covers: C-40, C-43
#[test]
fn a_known_miss_of_vitest_reads_test_a_registry_another_file_filled_at_import() {
    known_miss_of_vitest(
        &vitest_registry(),
        (
            "src/plugins.ts",
            "import { HANDLERS } from './registry'\nHANDLERS.x = 42\n",
        ),
    );
}

/// The observe line's count of tests that would have run, `N of M`.
fn would_run(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.contains("with `tests: reuse`,") && l.contains("tests would have run"))
        .unwrap_or_else(|| panic!("no observe line for tests in:\n{stderr}"))
        .split("reuse`, ")
        .nth(1)
        .unwrap()
        .split(" tests")
        .next()
        .unwrap()
        .to_string()
}

// covers: C-40, C-43
#[test]
fn a_concurrent_vitest_test_gets_its_files_list() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    // target uses other.ts alone; plain runs calc.ts's code through facade.ts. A change to calc.ts
    // is two imports away from target: watched alone, target would be reused; run at once with
    // plain, it gets its file's list, and runs.
    let files = |concurrent: &str| {
        vec![
            (
                "src/calc.ts",
                "export function f() { return 1 }\n".to_string(),
            ),
            (
                "src/facade.ts",
                "import { f } from './calc'\nexport function g() { return f() }\n".to_string(),
            ),
            (
                "src/other.ts",
                "export function h() { return 1 }\n".to_string(),
            ),
            (
                "test/a.test.ts",
                format!(
                    "{T}import {{ g }} from '../src/facade'\nimport {{ h }} from '../src/other'\ntest{concurrent}('target', async () => {{ expect(h()).toBe(1) }})\ntest{concurrent}('plain', async () => {{ expect(g()).toBe(1) }})\n"
                ),
            ),
        ]
    };
    let mut counts = Vec::new();
    for concurrent in ["", ".concurrent"] {
        let files = files(concurrent);
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let r = reads_repo(&borrowed, "{mode: observe, unit: test, reads: test}");
        assert_eq!(r.run(VITEST_RUN).code, 0);
        r.write(
            "src/calc.ts",
            "export function f() { return 1 } // changed\n",
        );
        let second = r.run(VITEST_RUN);
        assert_eq!(second.code, 0, "{}", second.stderr);
        counts.push(would_run(&second.stderr));
    }
    assert_eq!(counts, ["1 of 3", "2 of 3"]);
}

// covers: C-40, C-43
#[test]
fn with_vitests_coverage_on_reads_test_watches_nothing_and_says_why() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = reads_repo(
        &[
            ("cov.mjs", NO_COVERAGE),
            (
                "test/a.test.ts",
                "import { test } from 'vitest'\ntest('one', () => {})\n",
            ),
        ],
        "{mode: observe, unit: test, reads: test}",
    );
    r.write(
        "vitest.config.ts",
        &config("  test: { coverage: { enabled: true, provider: 'custom', customProviderModule: './cov.mjs' } },\n"),
    );
    let ran = r.run(VITEST_RUN);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains(
            "hushrun: unit: each test is taken to have read what its test file read: Vitest's coverage is on, and it shares V8's counters with watching a test\n"
        ),
        "{}",
        ran.stderr
    );
}

// covers: C-40, C-43, C-36
#[test]
fn with_vitest_reads_test_a_test_is_reused_on_its_own_list_and_its_file_records_each_test() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    // One test runs a.ts's code through wa.ts, sixty others b.ts's through wb.ts; the test file
    // imports both wrappers: a change to a.ts runs only the first test, and audits.
    let r = reads_repo(
        &[
            ("src/a.ts", "export function f() { return 1 }\n"),
            ("src/b.ts", "export function f() { return 1 }\n"),
            (
                "src/wa.ts",
                "import { f } from './a'\nexport function wa() { return f() }\n",
            ),
            (
                "src/wb.ts",
                "import { f } from './b'\nexport function wb() { return f() }\n",
            ),
            (
                "test/a.test.ts",
                &format!(
                    "{T}import {{ wa }} from '../src/wa'\nimport {{ wb }} from '../src/wb'\ntest('one', () => {{ expect(wa()).toBe(1) }})\ntest.each(Array.from({{ length: 60 }}, (_, i) => i))('two %i', () => {{ expect(wb()).toBe(1) }})\n"
                ),
            ),
        ],
        "{mode: reuse, unit: test, reads: test}",
    );
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // A test file that passed whole records its own pass and each test's.
    let map = r.map();
    assert_eq!(
        map["units"]["test/a.test.ts"]["tests"]
            .as_object()
            .unwrap()
            .len(),
        61
    );
    assert_eq!(unit_results(&r.cache), 2 + 61 + 1);
    r.write("src/a.ts", "export function f() { return 1 } // changed\n");
    let second = r.run(VITEST_RUN);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stderr.contains(
            "hushrun: unit: 4 of 62 tests run, in 2 of 2 test files; 58 already passed on the files they read, and 3 more that did run as a check"
        ),
        "{}",
        second.stderr
    );
    assert_eq!(
        r.newest_result()["units"]["tests"],
        json!({"total": 62, "ran": 4, "reused": 58})
    );
}

/// How many unit results this machine holds.
fn unit_results(cache: &Path) -> usize {
    fn count(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .map(|p| {
                if p.is_dir() {
                    count(&p)
                } else {
                    usize::from(p.extension().is_some_and(|e| e == "json"))
                }
            })
            .sum()
    }
    count(&cache.join("results/units/v0"))
}

// covers: C-40, C-43
#[test]
fn what_a_module_opened_is_kept_with_the_module() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let r = told_repo(&[
        ("data/price.txt", "5\n"),
        (
            "src/prices.ts",
            "import { readFileSync } from 'node:fs'\nlet kept: string | undefined\nexport function price() {\n  kept ??= readFileSync('data/price.txt', 'utf8').trim()\n  return kept\n}\n",
        ),
        (
            "test/a.test.ts",
            "import { test, expect } from 'vitest'\nimport { price } from '../src/prices'\ntest('first', () => { expect(price()).toBeTruthy() })\ntest('target', () => { expect(price()).toBe('5') })\n",
        ),
    ]);
    let told = tests_vitest(&r, "test", json!({}), &[], &[]);
    assert_eq!(told.code, 0, "{}", told.stdout);
    let unit = "test/a.test.ts";
    // target opened nothing, and ran prices.ts, which keeps what it opened for first.
    assert_eq!(
        told.list(unit, "target", "opened").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        told.list(unit, "target", "ran").unwrap(),
        ["src/prices.ts", unit]
    );
    let facts = told.unit(unit);
    let paths = facts["paths"].as_array().unwrap();
    let opens: std::collections::BTreeMap<String, Vec<String>> = facts["opens"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(from, to)| {
            let from = paths[from.parse::<usize>().unwrap()].as_str().unwrap();
            (from.to_string(), told.names(unit, to).unwrap())
        })
        .collect();
    assert_eq!(opens["src/prices.ts"], ["data/price.txt"]);
}
