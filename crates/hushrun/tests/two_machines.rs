//! Test by test across machines, phase 0 (C-31, C-33, C-34): what a machine with an empty cache
//! reuses, against the same results server as a machine that ran the check before.
//!
//! Each scenario runs a check in `tests: reuse` on a first machine, which records every test file's
//! result on the fake results server. One source file then changes, and two more machines run the
//! check on a clone of the same commit with that change, each finding the server as the first left
//! it: a fresh machine, whose cache is empty, as a CI runner's or a sandbox's is; and a machine that
//! has the first machine's store, copied into its cache. The second is the bar: **parity** is the
//! fresh machine reusing exactly the test files it reuses.
//!
//! Each runner has two tests. One says what happens today, and passes: Cargo reuses all it could,
//! since its keys come from the build; Go reuses a package whose tests opened no file the key
//! holds; pytest and Vitest reuse nothing, since what a test file read comes from the map alone.
//! The other is parity, which phases 3 and 4 of test-by-test-across-machines make pass for pytest,
//! Vitest and Go, and which passes for Cargo already.
//!
//! Which test files ran is read from the runner's own output, not from hushrun's store. Which of
//! the reusable test files runs as a check (C-34) depends on the run's start, so a machine is held
//! to the set its plan could reuse: it reused only test files of that set, ran every other, and
//! reused as many as the set leaves after its audit share.
//!
//! The fixtures need Python with pytest and pytest-xdist, Node with Vitest 4.1.12 (named by
//! `HUSHRUN_TEST_VITEST`), Go and cargo-nextest. Each says so and passes where its tool is
//! missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/store.rs"]
mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::git;
use serde_json::Value;

/// Environment variables a test must never inherit: CI's, hushrun's, an outer run's, the
/// runners', and what says a coding agent runs it.
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
    "HUSHRUN_GOFLAGS",
    "HUSHRUN_VITEST",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "PYTEST_ADDOPTS",
    "PYTHONPATH",
    "CARGO_TARGET_DIR",
    "GOFLAGS",
    "GOOS",
    "GOARCH",
    "GOWORK",
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
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// The Vitest these tests are written against.
const VITEST: &str = "4.1.12";
const VITEST_LINE: &str = "plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())],";

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

fn succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn python() -> String {
    std::env::var("HUSHRUN_TEST_PYTHON").unwrap_or_else(|_| "python3".into())
}

/// The `node_modules` with the pinned Vitest, when this machine has it.
fn vitest_modules() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HUSHRUN_TEST_VITEST")?);
    let package = std::fs::read(dir.join("vitest/package.json")).ok()?;
    let package: Value = serde_json::from_slice(&package).ok()?;
    (package["version"] == VITEST).then_some(dir)
}

/// Go's own build cache, which the tests share, so that the standard library builds once.
fn go_cache() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            Command::new("go")
                .args(["env", "GOCACHE"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| {
                    std::env::temp_dir()
                        .join("hushrun-test-gocache")
                        .display()
                        .to_string()
                })
        })
        .clone()
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn yaml(check: &str) -> String {
    format!(
        "version: 1\nrepository: example.com/acme/shop\nchecks:\n  {check}:\n    tests: reuse\n"
    )
}

/// One runner's scenario: a repository, its check's COMMAND, the change, and what each machine
/// is held to.
struct Scenario {
    check: &'static str,
    files: Vec<(String, String)>,
    /// What follows `hushrun run CHECK --`.
    command: Vec<String>,
    /// Variables COMMAND gets on every machine, beside hushrun's.
    env: Vec<(&'static str, String)>,
    /// What a clone needs that git doesn't carry, such as Vitest's `node_modules`.
    prepare: fn(&Path),
    /// The one source file that changes, and its new text.
    change: (&'static str, &'static str),
    /// Every test file, as hushrun names it.
    all: BTreeSet<String>,
    /// The test files that run on a machine that has the first machine's store: those that read
    /// the changed file, and those hushrun couldn't follow.
    must_run: BTreeSet<String>,
    /// The test files of `must_run` that hushrun can't follow, which record nothing.
    not_followed: BTreeSet<String>,
    /// The test files that ran, from the runner's output.
    ran: fn(&Ran) -> BTreeSet<String>,
}

impl Scenario {
    /// The test files a plan with the first machine's store could reuse.
    fn reusable(&self) -> BTreeSet<String> {
        self.all.difference(&self.must_run).cloned().collect()
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The line before COMMAND that says what the plan reuses, or why every test file runs.
    fn plan_line(&self) -> Option<String> {
        self.stderr
            .lines()
            .find(|l| {
                l.contains("test files run")
                    || l.contains("test file run")
                    || l.contains("every test file runs")
            })
            .map(str::to_string)
    }
}

/// One machine: a clone of the repository and its own cache.
struct Machine {
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
}

impl Machine {
    fn at(root: PathBuf) -> Machine {
        std::fs::create_dir_all(&root).expect("mkdir");
        Machine {
            repo: root.join("repo"),
            cache: root.join("cache"),
            root,
        }
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn run(&self, s: &Scenario, results: &FakeResults) -> Ran {
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
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("RUSTC_WORKSPACE_WRAPPER")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        cmd.env("HUSHRUN_BACKEND", results.url())
            .env("HUSHRUN_TOKEN", FakeResults::TOKEN);
        for (name, value) in &s.env {
            cmd.env(name, value);
        }
        let output = cmd
            .args(["run", s.check, "--"])
            .args(&s.command)
            .output()
            .expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// What one machine's run did.
#[derive(Debug)]
struct Outcome {
    plan: Option<String>,
    ran: BTreeSet<String>,
    /// The run's `units`, as the server recorded it: `{total, ran, reused}`.
    units: Value,
    stderr: String,
}

/// What the fresh machine and the machine with the first machine's store did.
struct Measured {
    fresh: Outcome,
    copied: Outcome,
}

/// Runs `s` on three machines: the first records, then the fresh one and the one with the
/// first's store each run on the server as the first left it.
fn two_machines(s: &Scenario) -> Measured {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("real path");
    let results = FakeResults::start();

    let first = Machine::at(root.join("first"));
    std::fs::create_dir_all(&first.repo).expect("mkdir");
    git::init(&first.repo, "sha1");
    for (path, text) in &s.files {
        first.write(path, text);
    }
    (s.prepare)(&first.repo);
    git::commit_all(&first.repo, "init");
    let ran = first.run(s, &results);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!((s.ran)(&ran), s.all, "the first run runs every test file");
    assert_eq!(
        results.unit_results(),
        s.all.len() - s.not_followed.len(),
        "every test file that passed and was followed is recorded on the server"
    );
    let left = results.recorded_so_far();

    let outcome = |name: &str, copy_store: bool| -> Outcome {
        let machine = Machine::at(root.join(name));
        git::run(
            &root,
            &[
                "clone",
                "-q",
                &first.repo.to_string_lossy(),
                &machine.repo.to_string_lossy(),
            ],
            None,
        );
        git::run(&machine.repo, &["config", "core.autocrlf", "false"], None);
        (s.prepare)(&machine.repo);
        if copy_store {
            // \[ADR 0072\] The store whole, as a cache step saves it once hushrun has exited.
            let units = machine.cache.join("units");
            std::fs::create_dir_all(&units).expect("mkdir");
            let stores = store::stores(&first.cache);
            assert_eq!(stores.len(), 1, "the first machine keeps one store");
            for path in stores {
                std::fs::copy(&path, units.join(path.file_name().expect("name")))
                    .expect("copy the store");
            }
        }
        machine.write(s.change.0, s.change.1);
        results.go_back_to(&left);
        let ran = machine.run(s, &results);
        assert_eq!(ran.code, 0, "{name}:\n{}\n{}", ran.stdout, ran.stderr);
        let runs = results.runs();
        assert_eq!(
            runs.len(),
            2,
            "{name}: the first run and this one: {runs:?}"
        );
        Outcome {
            plan: ran.plan_line(),
            ran: (s.ran)(&ran),
            units: runs[1]["units"].clone(),
            stderr: ran.stderr,
        }
    };
    let fresh = outcome("fresh", false);
    let copied = outcome("copied", true);
    Measured { fresh, copied }
}

/// Asserts that `o`'s plan could reuse exactly `could` of the scenario's test files (C-34): it
/// reused only test files of `could`, ran every other, and reused as many as `could` leaves after
/// its audit share, which runs the rest of `could` as a check.
fn assert_could_reuse(what: &str, s: &Scenario, o: &Outcome, could: &BTreeSet<String>) {
    assert!(
        o.ran.is_subset(&s.all),
        "{what}: ran {:?}, beyond the test files {:?}\n{}",
        o.ran,
        s.all,
        o.stderr
    );
    let reused: BTreeSet<String> = s.all.difference(&o.ran).cloned().collect();
    assert!(
        reused.is_subset(could),
        "{what}: reused {reused:?}, which isn't within {could:?}\n{}",
        o.stderr
    );
    let audited = hushrun_core::units::audit_share(could.len());
    assert_eq!(
        reused.len(),
        could.len() - audited,
        "{what}: reused {reused:?}; its plan could reuse {could:?}, of which {audited} runs as a check\n{}",
        o.stderr
    );
    let total = s.all.len() as u64;
    let reused = reused.len() as u64;
    assert_eq!(o.units["total"], total, "{what}: {}", o.units);
    assert_eq!(o.units["ran"], total - reused, "{what}: {}", o.units);
    assert_eq!(o.units["reused"], reused, "{what}: {}", o.units);
}

/// Asserts parity: the fresh machine's plan could reuse what the one with the first machine's
/// store could, and says so in the same words.
fn assert_parity(s: &Scenario, m: &Measured) {
    let could = s.reusable();
    assert_could_reuse("with the first machine's store", s, &m.copied, &could);
    assert_could_reuse("on a fresh machine", s, &m.fresh, &could);
    assert_eq!(m.fresh.plan, m.copied.plan, "{}", m.fresh.stderr);
}

// pytest

/// A shop: two modules each read by one test file, a greeting read by five, and a test file that
/// starts a process, which hushrun can't follow.
fn pytest_scenario() -> Scenario {
    let mut files = vec![
        file("hushrun.yaml", &yaml("unit")),
        file(".gitignore", "__pycache__/\n.pytest_cache/\n"),
        file(
            "pytest.ini",
            "[pytest]\ntestpaths = tests\npythonpath = .\n",
        ),
        file("app/__init__.py", ""),
        file("app/pricing.py", "def price(x):\n    return x * 2\n"),
        file(
            "app/cart.py",
            "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)\n",
        ),
        file("app/greet.py", "def hello():\n    return 'hi'\n"),
        file(
            "tests/test_cart.py",
            "from app.cart import total\n\ndef test_total():\n    assert total([1, 2]) == 6\n",
        ),
        file(
            "tests/test_pricing.py",
            "from app.pricing import price\n\ndef test_price():\n    assert price(3) == 6\n",
        ),
        file(
            "tests/test_child.py",
            "import subprocess, sys\n\ndef test_child():\n    assert subprocess.run([sys.executable, '-c', 'pass']).returncode == 0\n",
        ),
    ];
    let mut all = set(&[
        "tests/test_cart.py",
        "tests/test_pricing.py",
        "tests/test_child.py",
    ]);
    for n in 1..=5 {
        let name = format!("tests/test_greet_{n}.py");
        files.push(file(
            &name,
            "from app.greet import hello\n\ndef test_hello():\n    assert hello() == 'hi'\n",
        ));
        all.insert(name);
    }
    Scenario {
        check: "unit",
        files,
        command: [
            &python(),
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
            "-rA",
        ]
        .map(str::to_string)
        .to_vec(),
        env: vec![],
        prepare: |_| {},
        change: (
            "app/cart.py",
            "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n",
        ),
        all,
        must_run: set(&["tests/test_cart.py", "tests/test_child.py"]),
        not_followed: set(&["tests/test_child.py"]),
        ran: pytest_ran,
    }
}

/// The test files of pytest's `-rA` summary.
fn pytest_ran(ran: &Ran) -> BTreeSet<String> {
    ran.stdout
        .lines()
        .filter_map(|l| {
            l.strip_prefix("PASSED ")
                .or_else(|| l.strip_prefix("FAILED "))
        })
        .filter_map(|l| l.split("::").next())
        .map(str::to_string)
        .collect()
}

fn has_pytest() -> bool {
    succeeds(
        &python(),
        &[
            "-c",
            "import sys, pytest, xdist; sys.exit(0 if sys.version_info >= (3, 8) else 1)",
        ],
    )
}

// covers: C-33, C-34
#[test]
fn pytest_on_a_fresh_machine_today_runs_every_test_file_and_says_nothing_of_them() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let s = pytest_scenario();
    let m = two_machines(&s);
    assert_could_reuse(
        "with the first machine's store",
        &s,
        &m.copied,
        &s.reusable(),
    );
    assert_eq!(
        m.copied.plan.as_deref(),
        Some(
            "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that did runs as a check"
        )
    );
    // No map: no pytest has reported this check's test files on this machine (ADR 0074).
    assert_could_reuse("on a fresh machine", &s, &m.fresh, &BTreeSet::new());
    assert_eq!(m.fresh.plan, None, "{}", m.fresh.stderr);
}

// covers: C-33, C-34
#[test]
#[ignore = "phase 3 and 4 of test-by-test-across-machines: a fresh machine has no map of what test files read, which the server's hints will give it"]
fn pytest_on_a_fresh_machine_reuses_what_the_first_machines_store_would() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let s = pytest_scenario();
    assert_parity(&s, &two_machines(&s));
}

// Vitest

fn vitest_scenario() -> Scenario {
    let mut files = vec![
        file("hushrun.yaml", &yaml("unit")),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file(
            "vitest.config.ts",
            &format!(
                "import {{ defineConfig }} from 'vitest/config'\n\nexport default defineConfig({{\n  {VITEST_LINE}\n}})\n"
            ),
        ),
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
    let mut all = set(&["test/cart.test.ts", "test/pricing.test.ts"]);
    for n in 1..=5 {
        let name = format!("test/greet{n}.test.ts");
        files.push(file(
            &name,
            "import { test, expect } from 'vitest'\nimport { hello } from '../src/greet'\n\ntest('hello', () => { expect(hello()).toBe('hi') })\n",
        ));
        all.insert(name);
    }
    Scenario {
        check: "unit",
        files,
        command: ["node_modules/.bin/vitest", "run", "--reporter=verbose"]
            .map(str::to_string)
            .to_vec(),
        env: vec![],
        prepare: |repo| {
            // Vitest as installed outside the repository, which git ignores.
            let modules = vitest_modules().expect("vitest");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&modules, repo.join("node_modules")).expect("symlink");
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&modules, repo.join("node_modules")).expect("link");
        },
        change: (
            "src/cart.ts",
            "import { price } from './pricing'\n\nexport const total = (xs: number[]) => xs.reduce((n, x) => n + price(x), 0) // changed\n",
        ),
        all,
        must_run: set(&["test/cart.test.ts"]),
        not_followed: BTreeSet::new(),
        ran: vitest_ran,
    }
}

/// The test files of Vitest's verbose reporter: each test's line names its file.
fn vitest_ran(ran: &Ran) -> BTreeSet<String> {
    plain(&ran.stdout)
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            l.strip_prefix("✓ ").or_else(|| l.strip_prefix("× "))
        })
        .filter_map(|l| l.split(" > ").next())
        .map(|l| l.split_whitespace().next().unwrap_or_default().to_string())
        .filter(|l| l.ends_with(".test.ts"))
        .collect()
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

fn has_vitest() -> bool {
    vitest_modules().is_some() && succeeds("node", &["--version"])
}

// covers: C-33, C-34
#[test]
fn vitest_on_a_fresh_machine_today_runs_every_test_file() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let s = vitest_scenario();
    let m = two_machines(&s);
    assert_could_reuse(
        "with the first machine's store",
        &s,
        &m.copied,
        &s.reusable(),
    );
    assert_eq!(
        m.copied.plan.as_deref(),
        Some(
            "hushrun: unit: 2 of 7 test files run; 5 already passed on the files they read, and 1 more that did runs as a check"
        )
    );
    // Vitest's list of test files is the map's (ADR 0071).
    assert_could_reuse("on a fresh machine", &s, &m.fresh, &BTreeSet::new());
    assert_eq!(
        m.fresh.plan.as_deref(),
        Some("hushrun: unit: every test file runs: no run here has listed Vitest's test files yet")
    );
}

// covers: C-33, C-34
#[test]
#[ignore = "phase 3 and 4 of test-by-test-across-machines: a fresh machine has no map of what test files read, which the server's hints will give it"]
fn vitest_on_a_fresh_machine_reuses_what_the_first_machines_store_would() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let s = vitest_scenario();
    assert_parity(&s, &two_machines(&s));
}

// Go

const MODULE: &str = "example.com/shop";

/// A shop in Go: a pricing package and a cart that imports it; a greeting; a package whose test
/// opens a file of its testdata, which no key holds (the rest set); and one whose test opens
/// pricing's source as data, which its key holds and only the map knows.
fn go_scenario() -> Scenario {
    let test = |package: &str, body: &str| {
        format!(
            "package {package}\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nvar _ = os.ReadFile\n\nfunc TestIt(t *testing.T) {{\n{body}}}\n"
        )
    };
    let files = vec![
        file("hushrun.yaml", &yaml("unit")),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file(
            "pricing/pricing.go",
            "package pricing\n\nfunc Price(x int) int { return x * 2 }\n",
        ),
        file(
            "pricing/pricing_test.go",
            &test(
                "pricing",
                "\tif Price(3) != 6 {\n\t\tt.Fatal(\"price\")\n\t}\n",
            ),
        ),
        file(
            "cart/cart.go",
            &format!(
                "package cart\n\nimport \"{MODULE}/pricing\"\n\nfunc Total(xs []int) int {{\n\tn := 0\n\tfor _, x := range xs {{\n\t\tn += pricing.Price(x)\n\t}}\n\treturn n\n}}\n"
            ),
        ),
        file(
            "cart/cart_test.go",
            &test(
                "cart",
                "\tif Total([]int{1, 2}) != 6 {\n\t\tt.Fatal(\"total\")\n\t}\n",
            ),
        ),
        file(
            "greet/greet.go",
            "package greet\n\nfunc Hello() string { return \"hi\" }\n",
        ),
        file(
            "greet/greet_test.go",
            &test(
                "greet",
                "\tif Hello() != \"hi\" {\n\t\tt.Fatal(\"hello\")\n\t}\n",
            ),
        ),
        file(
            "fixtures/fixtures.go",
            "package fixtures\n\nfunc Name() string { return \"x\" }\n",
        ),
        file("fixtures/testdata/x.txt", "x\n"),
        file(
            "fixtures/fixtures_test.go",
            &test(
                "fixtures",
                "\tif _, err := os.ReadFile(\"testdata/x.txt\"); err != nil {\n\t\tt.Fatal(err)\n\t}\n",
            ),
        ),
        file("data/data.go", "package data\n\nconst Name = \"data\"\n"),
        file(
            "data/data_test.go",
            &test(
                "data",
                "\tif _, err := os.ReadFile(\"../pricing/pricing.go\"); err != nil {\n\t\tt.Fatal(err)\n\t}\n",
            ),
        ),
    ];
    Scenario {
        check: "unit",
        files,
        command: ["go", "test", "-json", "./..."]
            .map(str::to_string)
            .to_vec(),
        env: vec![
            ("GOCACHE", go_cache()),
            ("GOTOOLCHAIN", "local".into()),
            ("GOENV", "off".into()),
        ],
        prepare: |_| {},
        change: (
            "greet/greet.go",
            "package greet\n\nfunc Hello() string { return \"h\" + \"i\" }\n",
        ),
        all: set(&["cart", "data", "fixtures", "greet", "pricing"]),
        must_run: set(&["greet"]),
        not_followed: BTreeSet::new(),
        ran: go_ran,
    }
}

/// The packages of `go test -json` a test of which ran, by their directory.
fn go_ran(ran: &Ran) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in ran.stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let (Some("run"), Some(_), Some(package)) = (
            event["Action"].as_str(),
            event["Test"].as_str(),
            event["Package"].as_str(),
        ) {
            let dir = package
                .strip_prefix(MODULE)
                .unwrap_or(package)
                .trim_start_matches('/');
            out.insert(dir.to_string());
        }
    }
    out
}

// covers: C-33, C-34, C-35
#[test]
fn go_on_a_fresh_machine_today_reuses_a_package_whose_tests_opened_no_source() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let s = go_scenario();
    let m = two_machines(&s);
    assert_could_reuse(
        "with the first machine's store",
        &s,
        &m.copied,
        &s.reusable(),
    );
    assert_eq!(
        m.copied.plan.as_deref(),
        Some(
            "hushrun: unit: 2 of 5 test files run; 3 already passed on the files they read, and 1 more that did runs as a check"
        )
    );
    // `go list` says what each test binary is built from. What a test opened comes from the map:
    // `data` opened pricing's source, which its key holds, so it runs; `fixtures` opened a file of
    // the rest set, which no key holds, so it is reused as if it had opened nothing.
    assert_could_reuse(
        "on a fresh machine",
        &s,
        &m.fresh,
        &set(&["cart", "fixtures", "pricing"]),
    );
    assert_eq!(
        m.fresh.plan.as_deref(),
        Some(
            "hushrun: unit: 3 of 5 test files run; 2 already passed on the files they read, and 1 more that did runs as a check"
        )
    );
}

// covers: C-33, C-34, C-35
#[test]
#[ignore = "phase 3 and 4 of test-by-test-across-machines: a fresh machine has no map of what test files read, which the server's hints will give it"]
fn go_on_a_fresh_machine_reuses_what_the_first_machines_store_would() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let s = go_scenario();
    assert_parity(&s, &two_machines(&s));
}

// Cargo

/// A Cargo workspace: a library with tests, a binary crate with a test of its own and an
/// integration test that runs it, and a second library.
fn cargo_scenario() -> Scenario {
    let files = vec![
        file("hushrun.yaml", &yaml("test")),
        file(".gitignore", "target/\n"),
        file(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/core\", \"crates/app\", \"crates/other\"]\nresolver = \"2\"\n",
        ),
        file(
            "crates/core/Cargo.toml",
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"core_lib\"\n",
        ),
        file(
            "crates/core/src/lib.rs",
            "pub fn answer() -> u32 { 42 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_is_42() { assert_eq!(super::answer(), 42); }\n}\n",
        ),
        file(
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
        ),
        file(
            "crates/app/src/main.rs",
            "fn main() { println!(\"{}\", core_lib::answer()); }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answers() { assert_eq!(core_lib::answer(), 42); }\n}\n",
        ),
        file(
            "crates/app/tests/runs_binary.rs",
            "#[test]\nfn prints_the_answer() {\n    let out = std::process::Command::new(env!(\"CARGO_BIN_EXE_app\")).output().unwrap();\n    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), \"42\");\n}\n",
        ),
        file(
            "crates/other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        file(
            "crates/other/src/lib.rs",
            "pub fn two() -> u32 { 2 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
        ),
    ];
    Scenario {
        check: "test",
        files,
        command: ["cargo", "nextest", "run", "--workspace", "--locked"]
            .map(str::to_string)
            .to_vec(),
        env: vec![],
        prepare: |repo| {
            if repo.join("Cargo.lock").exists() {
                return;
            }
            let lock = Command::new("cargo")
                .args(["generate-lockfile", "--offline", "-q"])
                .current_dir(repo)
                .env_remove("CARGO_TARGET_DIR")
                .status()
                .expect("cargo");
            assert!(lock.success());
        },
        change: (
            "crates/other/src/lib.rs",
            "pub fn two() -> u32 { 1 + 1 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn two() { assert_eq!(super::two(), 2); }\n}\n",
        ),
        all: set(&[
            "crates/app/src/main.rs",
            "crates/app/tests/runs_binary.rs",
            "crates/core/src/lib.rs",
            "crates/other/src/lib.rs",
        ]),
        must_run: set(&["crates/other/src/lib.rs"]),
        not_followed: BTreeSet::new(),
        ran: cargo_ran,
    }
}

/// The test files of nextest's lines for the tests it ran, from each test binary's id.
fn cargo_ran(ran: &Ran) -> BTreeSet<String> {
    let binaries: BTreeMap<&str, &str> = [
        ("core-lib", "crates/core/src/lib.rs"),
        ("app::bin/app", "crates/app/src/main.rs"),
        ("app::runs_binary", "crates/app/tests/runs_binary.rs"),
        ("other", "crates/other/src/lib.rs"),
    ]
    .into_iter()
    .collect();
    let text = plain(&format!("{}\n{}", ran.stdout, ran.stderr));
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let l = l
                .strip_prefix("PASS [")
                .or_else(|| l.strip_prefix("FAIL ["))?;
            let after = l.split_once(']')?.1.trim_start();
            // nextest 0.9.100 and later count the tests: `(1/4) core-lib tests::answer_is_42`.
            let after = match after.strip_prefix('(') {
                Some(counted) => counted.split_once(')')?.1.trim_start(),
                None => after,
            };
            let id = after.split_whitespace().next()?;
            binaries.get(id).map(|unit| unit.to_string())
        })
        .collect()
}

// covers: C-33, C-34
#[test]
fn cargo_on_a_fresh_machine_today_reuses_all_it_could() {
    if !tools(
        "cargo-nextest",
        succeeds("cargo", &["nextest", "--version"]),
    ) {
        return;
    }
    let s = cargo_scenario();
    let m = two_machines(&s);
    // Keys come from the build's dep-info, made on this machine before COMMAND: no map needed.
    assert_could_reuse("on a fresh machine", &s, &m.fresh, &s.reusable());
    assert_eq!(
        m.fresh.plan.as_deref(),
        Some(
            "hushrun: test: 2 of 4 test files run; 2 already passed on the files they read, and 1 more that did runs as a check"
        )
    );
}

// covers: C-33, C-34
#[test]
fn cargo_on_a_fresh_machine_reuses_what_the_first_machines_store_would() {
    if !tools(
        "cargo-nextest",
        succeeds("cargo", &["nextest", "--version"]),
    ) {
        return;
    }
    let s = cargo_scenario();
    assert_parity(&s, &two_machines(&s));
}
