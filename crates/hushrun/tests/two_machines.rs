//! Test by test across machines (C-31, C-33, C-34, C-38): what a machine with an empty cache
//! reuses, against the same results server as a machine that ran the check before.
//!
//! Each scenario runs a check in `tests: reuse` on a first machine, which records every test file's
//! result on the fake results server. One source file then changes, and two more machines run the
//! check on a clone of the same commit with that change, each finding the server as the first left
//! it: a fresh machine, whose cache is empty, as a CI runner's or a sandbox's is; and a machine that
//! has the first machine's store, copied into its cache. The second is the bar: **parity** is the
//! fresh machine reusing exactly the test files it reuses.
//!
//! Each runner has two tests. One is a server without hints, as one from before them answers
//! (404): Cargo reuses all it could, since its keys come from the build; Go reuses a package whose
//! tests opened no file the key holds; pytest and Vitest reuse nothing, since what a test file
//! read comes from the map alone. The other is parity, with a server that keeps hints (C-38): the
//! first machine sends what its test files read, and the fresh machine reuses exactly what the
//! one with the store reuses, says the same, and `hushrun ready` says the same counts. No request
//! for hints holds a path of the repository.
//!
//! The rest are Go's, which runs on any machine: `observe` says the same on both; hints that are
//! wrong, made of random tokens or written under another key run what they would key; two lists
//! that disagree run the test file; a hint that couldn't be sent is sent by the next run, and the
//! same hint isn't sent twice; a server from before hints changes nothing and is said once; and
//! `counts_from` reads the hints of the systems it names.
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

use hushrun_core::hints;
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
    // B-1, B-2: the developer's build mode, store and wrapper stay out: a test run that cargo
    // itself compiles through the wrapper doesn't pass its session on to a fixture's cargo.
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";

/// The Vitest these tests are written against.
const VITEST: &str = "4.1.12";
const VITEST_LINE: &str = "plugins: [process.env.HUSHRUN_VITEST ? import(process.env.HUSHRUN_VITEST).then((m) => m.default()) : undefined],";

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

const REPOSITORY: &str = "example.com/acme/shop";

fn yaml(check: &str) -> String {
    format!("version: 1\nrepository: {REPOSITORY}\nchecks:\n  {check}:\n    tests: reuse\n")
}

/// The scenario with its check in `tests: observe`.
fn observing(mut s: Scenario) -> Scenario {
    for (path, text) in &mut s.files {
        if path == "hushrun.yaml" {
            *text = text.replace("tests: reuse", "tests: observe");
        }
    }
    s
}

/// What the line after COMMAND says `tests: reuse` would have done, in `observe`.
fn observe_line(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .find(|l| l.contains("with `tests: reuse`"))
        .map(str::to_string)
}

/// \[C-33, ADR 0077 amended\] The fresh machine's `observe` line says what the copied store's does,
/// count for count. Its saving it may not say: that is bounded by the longest a run of the check
/// took here reusing nothing, which this machine's store keeps and a hint doesn't, and on its
/// first run there is none, so it says the counts without a time; the copy says one when its
/// tests took a second or more, as under load they do.
#[track_caller]
fn says_what_the_copy_does(fresh: &str, copied: Option<&str>) {
    let line = observe_line(fresh);
    assert!(
        line.as_deref()
            .is_some_and(|l| !l.contains(", saving about ")),
        "{fresh}"
    );
    assert_eq!(
        line.as_deref().map(hushrun_testkit::lines::without_saving),
        copied.map(hushrun_testkit::lines::without_saving),
        "{fresh}"
    );
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
    /// \[C-22\] Whether `hushrun ready` can say how many test files a run would run: its
    /// `package.json` names the check's command.
    ready: bool,
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
                // \[C-39\] In any adapter's word: `test files`, `test binaries`, `packages`.
                [
                    "test file",
                    "test files",
                    "test binary",
                    "test binaries",
                    "package",
                    "packages",
                    "test program",
                    "test programs",
                    "test class",
                    "test classes",
                    "test target",
                    "test targets",
                ]
                .iter()
                .any(|w| l.contains(&format!("{w} run")))
                    || l.contains("every test file runs")
            })
            .map(str::to_string)
    }
}

/// One machine: a clone of the repository and its own cache.
#[derive(Debug)]
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
        let mut args = vec!["run".to_string(), s.check.to_string(), "--".to_string()];
        args.extend(s.command.iter().cloned());
        self.hushrun(s, results, &args)
    }

    fn hushrun(&self, s: &Scenario, results: &FakeResults, args: &[String]) -> Ran {
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
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
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
        let output = cmd.args(args).output().expect("hushrun");
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
    /// \[C-22\] What `hushrun ready` said of the check's test files before the run.
    ready: Option<String>,
    /// \[C-38\] Where each test file's row of the machine's store came from after the run.
    origins: BTreeMap<String, String>,
    machine: Option<Machine>,
}

/// What the fresh machine and the machine with the first machine's store did.
struct Measured {
    fresh: Outcome,
    copied: Outcome,
    results: FakeResults,
    _dir: tempfile::TempDir,
}

/// What the results server keeps of hints (C-38).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hints {
    Kept,
    /// It answers this status to them, as a server from before them does.
    Older(u16),
}

/// Runs `s` on three machines: the first records, then the fresh one and the one with the
/// first's store each run on the server as the first left it.
fn two_machines(s: &Scenario, server: Hints) -> Measured {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("real path");
    let results = FakeResults::start();
    if let Hints::Older(status) = server {
        results.forget_hints(status);
    }

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
        let ready = s.ready.then(|| {
            git::commit_all(&machine.repo, "change");
            let ready = machine.hushrun(s, &results, &["ready".to_string()]);
            ready
                .stdout
                .lines()
                .find(|l| l.contains("would run") && l.contains("test file"))
                .map(|l| l.trim().to_string())
                .unwrap_or_else(|| format!("nothing: {}\n{}", ready.stdout, ready.stderr))
        });
        let ran = machine.run(s, &results);
        assert_eq!(ran.code, 0, "{name}:\n{}\n{}", ran.stdout, ran.stderr);
        let runs = results.runs();
        assert_eq!(
            runs.len(),
            2,
            "{name}: the first run and this one: {runs:?}"
        );
        let origins = if store::stores(&machine.cache).len() == 1 {
            store::origins(&machine.cache)
        } else {
            BTreeMap::new()
        };
        Outcome {
            plan: ran.plan_line(),
            ran: (s.ran)(&ran),
            units: runs[1]["units"].clone(),
            stderr: ran.stderr,
            ready,
            origins,
            machine: Some(machine),
        }
    };
    let fresh = outcome("fresh", false);
    let copied = outcome("copied", true);
    if server == Hints::Kept {
        assert_private(s, &results);
    }
    Measured {
        fresh,
        copied,
        results,
        _dir: dir,
    }
}

/// \[C-38\] No request for hints holds a path of the repository: not in its query, nor in its
/// body as sent, inflated or read as base64. Each file's path and each directory's is looked for.
fn assert_private(s: &Scenario, results: &FakeResults) {
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for (path, _) in &s.files {
        paths.insert(path.clone());
        let mut dir = path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            paths.insert(parent.to_string());
            dir = parent;
        }
    }
    paths.extend(s.all.iter().cloned());
    // A path that is part of a word every hint holds, such as `test` of `vitest`, says nothing.
    let words = format!("{} pytest vitest go cargo", hints::SCHEMA);
    paths.retain(|p| p.len() >= 3 && !words.contains(p.as_str()));
    let requests: Vec<_> = results
        .raw_requests()
        .into_iter()
        .filter(|r| r.path.contains("/v0/units/hints/"))
        .collect();
    // Cargo needs no hint: its keys come from the build.
    assert_eq!(
        requests.is_empty(),
        s.check == "test",
        "requests for hints: {}",
        requests.len()
    );
    for request in requests {
        let mut seen: Vec<Vec<u8>> = vec![
            request.path.clone().into_bytes(),
            request.body.clone(),
            request
                .query
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&")
                .into_bytes(),
        ];
        if let Ok(content) = hints::inflate(&request.body) {
            seen.push(content);
        }
        if let Ok(json) = serde_json::from_slice::<Value>(&request.body) {
            for value in json.as_object().into_iter().flat_map(|o| o.values()) {
                if let Some(decoded) = value.as_str().and_then(|text| {
                    use base64::Engine as _;
                    base64::engine::general_purpose::STANDARD.decode(text).ok()
                }) {
                    seen.push(decoded);
                }
            }
        }
        for path in &paths {
            for bytes in &seen {
                assert!(
                    !bytes.windows(path.len()).any(|w| w == path.as_bytes()),
                    "{} {} holds {path}",
                    request.method,
                    request.path
                );
            }
        }
    }
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
/// store could, and says so in the same words, as `hushrun ready` does before it; and \[C-38\]
/// the fresh machine keeps each test file it reused on another machine's list as `shared`.
fn assert_parity(s: &Scenario, m: &Measured) {
    let could = s.reusable();
    assert_could_reuse("with the first machine's store", s, &m.copied, &could);
    assert_could_reuse("on a fresh machine", s, &m.fresh, &could);
    assert_eq!(m.fresh.plan, m.copied.plan, "{}", m.fresh.stderr);
    assert_eq!(m.fresh.ready, m.copied.ready, "{}", m.fresh.stderr);
    if let Some(ready) = &m.copied.ready {
        assert!(ready.contains("already passed"), "{ready}");
    }
    let reused: BTreeSet<String> = s.all.difference(&m.fresh.ran).cloned().collect();
    for (unit, origin) in &m.fresh.origins {
        let want = if reused.contains(unit) {
            "shared"
        } else {
            "seen here"
        };
        assert_eq!(origin, want, "{unit}: {:?}", m.fresh.origins);
    }
    assert!(
        m.copied.origins.values().all(|o| o == "seen here"),
        "{:?}",
        m.copied.origins
    );
    // \[C-38\] The first machine sent its hint; the other two saw their test files read what it
    // said, so neither sent one.
    let puts = m
        .results
        .requests()
        .iter()
        .filter(|r| r.method == "PUT" && r.path.starts_with("v0/units/hints/"))
        .count();
    assert_eq!(puts, usize::from(s.check != "test"), "{}", m.fresh.stderr);
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
        file(
            "package.json",
            &format!(
                "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"hushrun run unit -- {}\"}}}}\n",
                pytest_command().join(" ")
            ),
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
        command: pytest_command(),
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
        ready: true,
    }
}

/// pytest's command.
fn pytest_command() -> Vec<String> {
    [
        &python(),
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
        "-rA",
    ]
    .map(str::to_string)
    .to_vec()
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
fn pytest_on_a_fresh_machine_with_a_server_without_hints_runs_every_test_file() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let s = pytest_scenario();
    let m = two_machines(&s, Hints::Older(404));
    assert_could_reuse(
        "with the first machine's store",
        &s,
        &m.copied,
        &s.reusable(),
    );
    assert_eq!(
        m.copied.plan.as_deref(),
        Some(
            "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    // No map: no pytest has reported this check's test files on this machine (ADR 0074).
    assert_could_reuse("on a fresh machine", &s, &m.fresh, &BTreeSet::new());
    assert_eq!(m.fresh.plan, None, "{}", m.fresh.stderr);
}

// covers: C-33, C-34, C-38, C-22
#[test]
fn pytest_on_a_fresh_machine_reuses_what_the_first_machines_store_would() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let s = pytest_scenario();
    assert_parity(&s, &two_machines(&s, Hints::Kept));
}

// covers: C-33, C-38
#[test]
fn pytest_in_observe_on_a_fresh_machine_says_what_the_first_machines_store_would() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let s = observing(pytest_scenario());
    let m = two_machines(&s, Hints::Kept);
    let copied = observe_line(&m.copied.stderr);
    assert!(
        copied
            .as_deref()
            .is_some_and(|l| l.contains("2 of 8 test files would have run")),
        "{}",
        m.copied.stderr
    );
    says_what_the_copy_does(&m.fresh.stderr, copied.as_deref());
}

// Vitest

fn vitest_scenario() -> Scenario {
    let mut files = vec![
        file("hushrun.yaml", &yaml("unit")),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\", \"scripts\": {\"test\": \"hushrun run unit -- node_modules/.bin/vitest run --reporter=verbose\"}}\n",
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
        ready: true,
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
fn vitest_on_a_fresh_machine_with_a_server_without_hints_runs_every_test_file() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let s = vitest_scenario();
    let m = two_machines(&s, Hints::Older(405));
    assert_could_reuse(
        "with the first machine's store",
        &s,
        &m.copied,
        &s.reusable(),
    );
    assert_eq!(
        m.copied.plan.as_deref(),
        Some(
            "hushrun: unit: 2 of 7 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    // Vitest's list of test files is the map's (ADR 0071).
    assert_could_reuse("on a fresh machine", &s, &m.fresh, &BTreeSet::new());
    assert_eq!(
        m.fresh.plan.as_deref(),
        Some("hushrun: unit: every test file runs: no run here has listed Vitest's test files yet")
    );
}

// covers: C-33, C-34, C-38, C-22
#[test]
fn vitest_on_a_fresh_machine_reuses_what_the_first_machines_store_would() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let s = vitest_scenario();
    assert_parity(&s, &two_machines(&s, Hints::Kept));
}

// covers: C-33, C-38
#[test]
fn vitest_in_observe_on_a_fresh_machine_says_what_the_first_machines_store_would() {
    if !tools(&format!("Node and Vitest {VITEST}"), has_vitest()) {
        return;
    }
    let s = observing(vitest_scenario());
    let m = two_machines(&s, Hints::Kept);
    let copied = observe_line(&m.copied.stderr);
    assert!(
        copied
            .as_deref()
            .is_some_and(|l| l.contains("1 of 7 test files would have run")),
        "{}",
        m.copied.stderr
    );
    says_what_the_copy_does(&m.fresh.stderr, copied.as_deref());
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
        ready: false,
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
fn go_on_a_fresh_machine_with_a_server_without_hints_reuses_a_package_whose_tests_opened_no_source()
{
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let s = go_scenario();
    let m = two_machines(&s, Hints::Older(404));
    assert_could_reuse(
        "with the first machine's store",
        &s,
        &m.copied,
        &s.reusable(),
    );
    assert_eq!(
        m.copied.plan.as_deref(),
        Some(
            "hushrun: unit: 2 of 5 packages run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
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
            "hushrun: unit: 3 of 5 packages run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
}

// covers: C-33, C-34, C-35, C-38
#[test]
fn go_on_a_fresh_machine_reuses_what_the_first_machines_store_would() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let s = go_scenario();
    let m = two_machines(&s, Hints::Kept);
    assert_parity(&s, &m);
    // \[C-37\] `hushrun why` says which list it was, for a package the fresh machine reused on
    // another machine's: `data` opened pricing's source, which only the first machine saw.
    if m.fresh.origins.get("data").map(String::as_str) == Some("shared") {
        let why = m.fresh.machine.as_ref().expect("the machine").hushrun(
            &s,
            &m.results,
            &["why".into(), "unit".into(), "data".into()],
        );
        assert!(
            why.stdout.contains("another machine saw it read"),
            "{}\n{}",
            why.stdout,
            why.stderr
        );
        // The next run of it here replaces that row with what it saw.
        let fresh = m.fresh.machine.as_ref().expect("the machine");
        fresh.write(
            "data/data.go",
            "package data\n\nconst Name = \"data, again\"\n",
        );
        let ran = fresh.run(&s, &m.results);
        assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
        assert_eq!(store::origins(&fresh.cache)["data"], "seen here");
    }
}

// covers: C-33, C-38
#[test]
fn go_in_observe_on_a_fresh_machine_says_what_the_first_machines_store_would() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let s = observing(go_scenario());
    let m = two_machines(&s, Hints::Kept);
    let copied = observe_line(&m.copied.stderr);
    assert!(
        copied
            .as_deref()
            .is_some_and(|l| l.contains("1 of 5 packages would have run")),
        "{}",
        m.copied.stderr
    );
    says_what_the_copy_does(&m.fresh.stderr, copied.as_deref());
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
        ready: false,
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
fn cargo_on_a_fresh_machine_with_a_server_without_hints_reuses_all_it_could() {
    if !tools(
        "cargo-nextest",
        succeeds("cargo", &["nextest", "--version"]),
    ) {
        return;
    }
    let s = cargo_scenario();
    let m = two_machines(&s, Hints::Older(404));
    // Keys come from the build's dep-info, made on this machine before COMMAND: no map needed.
    assert_could_reuse("on a fresh machine", &s, &m.fresh, &s.reusable());
    assert_eq!(
        m.fresh.plan.as_deref(),
        Some(
            "hushrun: test: 2 of 4 test binaries run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
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
    assert_parity(&s, &two_machines(&s, Hints::Kept));
}

// Go, with what a server keeps of hints (C-38)

/// What a server says when it keeps no hints, once in a run.
const NO_HINTS: &str = "doesn't keep what other machines saw its test files read";

/// A repository on a first machine, against one results server, and the machines that clone it.
struct Lab {
    _dir: tempfile::TempDir,
    root: PathBuf,
    results: FakeResults,
    first: Machine,
}

impl Lab {
    fn new(s: &Scenario) -> Lab {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let first = Machine::at(root.join("first"));
        std::fs::create_dir_all(&first.repo).expect("mkdir");
        git::init(&first.repo, "sha1");
        for (path, text) in &s.files {
            first.write(path, text);
        }
        (s.prepare)(&first.repo);
        git::commit_all(&first.repo, "init");
        Lab {
            _dir: dir,
            root,
            results: FakeResults::start(),
            first,
        }
    }

    /// A machine with an empty cache, on a clone of the first machine's commit.
    fn fresh(&self, name: &str, s: &Scenario) -> Machine {
        let machine = Machine::at(self.root.join(name));
        git::run(
            &self.root,
            &[
                "clone",
                "-q",
                &self.first.repo.to_string_lossy(),
                &machine.repo.to_string_lossy(),
            ],
            None,
        );
        (s.prepare)(&machine.repo);
        machine
    }

    /// The hint requests the server got: (method, path), oldest first.
    fn hint_requests(&self) -> Vec<(String, String)> {
        self.results
            .requests()
            .into_iter()
            .filter(|r| r.path.starts_with("v0/units/hints/"))
            .map(|r| (r.method, r.path))
            .collect()
    }

    fn puts(&self) -> usize {
        self.hint_requests()
            .iter()
            .filter(|(m, _)| m == "PUT")
            .count()
    }
}

/// The go list sources of [`go_scenario`], which its hints' key is derived from (C-38).
const GO_SOURCES: [&str; 5] = [
    "cart/cart.go",
    "data/data.go",
    "fixtures/fixtures.go",
    "greet/greet.go",
    "pricing/pricing.go",
];

fn go_files(s: &Scenario) -> Vec<&str> {
    s.files.iter().map(|(p, _)| p.as_str()).collect()
}

/// Runs the Go scenario on its first machine, which sends its hint; and returns the hint, read.
fn go_with_its_hint(s: &Scenario) -> (Lab, hints::Read) {
    let lab = Lab::new(s);
    let ran = lab.first.run(s, &lab.results);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    let kept = lab.results.hints();
    assert_eq!(kept.len(), 1, "the first run sends one hint: {kept:?}");
    let key = hints::Key::derive(GO_SOURCES);
    let read = hints::read(&kept[0].body, &key, go_files(s)).expect("the hint reads");
    assert_eq!(read.dropped, 0, "{read:?}");
    assert_eq!(
        read.hint.units.keys().cloned().collect::<BTreeSet<_>>(),
        s.all
    );
    assert!(
        read.hint.units["data"].reads.contains("pricing/pricing.go"),
        "{read:?}"
    );
    assert_eq!(kept[0].os, hints::Os::this().expect("a system").word());
    assert_eq!(kept[0].repository, REPOSITORY);
    (lab, read)
}

/// Replaces the server's hints with `hint`, at the first one's place, written under `key`.
fn only_hint(lab: &Lab, hint: &hints::Hint, key: &hints::Key) {
    let place = lab.results.hints()[0].clone();
    lab.results.drop_hints();
    let written = hints::write(hint, key).expect("a hint");
    lab.results.add_hint(
        &place.repository,
        &place.check_id,
        &place.address,
        &place.os,
        &written.bytes,
    );
}

// covers: C-38, C-34
#[test]
fn a_hint_that_is_wrong_random_or_under_another_key_runs_what_it_would_key() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let s = go_scenario();
    let (lab, honest) = go_with_its_hint(&s);
    let left = lab.results.recorded_so_far();
    // What a fresh machine reuses with no hint at all: what go list tells it alone.
    let alone = set(&["cart", "fixtures", "pricing"]);
    let run = |name: &str, change: Option<(&str, &str)>| -> BTreeSet<String> {
        let machine = lab.fresh(name, &s);
        if let Some((path, text)) = change {
            machine.write(path, text);
        }
        let ran = machine.run(&s, &lab.results);
        assert_eq!(ran.code, 0, "{name}:\n{}\n{}", ran.stdout, ran.stderr);
        let ran = (s.ran)(&ran);
        lab.results.go_back_to(&left);
        ran
    };

    // A hint that leaves out pricing's source, which `data`'s test opened, when it changed.
    let mut wrong = honest.hint.clone();
    wrong
        .units
        .get_mut("data")
        .expect("data")
        .reads
        .remove("pricing/pricing.go");
    only_hint(&lab, &wrong, &hints::Key::derive(GO_SOURCES));
    let ran = run(
        "wrong",
        Some((
            "pricing/pricing.go",
            "package pricing\n\nfunc Price(x int) int { return x + x }\n",
        )),
    );
    assert!(ran.contains("data"), "{ran:?}");

    // A hint whose every file read is a token of a path that isn't there.
    let mut random = honest.hint.clone();
    for (n, unit) in random.units.values_mut().enumerate() {
        unit.reads = (0..3).map(|i| format!("nowhere/{n}/{i}.go")).collect();
    }
    only_hint(&lab, &random, &hints::Key::derive(GO_SOURCES));
    let ran = run("random", Some(s.change));
    assert!(ran.contains("data"), "{ran:?}");
    assert!(s.all.difference(&ran).all(|u| alone.contains(u)), "{ran:?}");

    // The honest hint, under a key another list of sources gives.
    only_hint(&lab, &honest.hint, &hints::Key::derive(["another/list.go"]));
    let ran = run("another-key", Some(s.change));
    assert!(ran.contains("data"), "{ran:?}");
    assert!(s.all.difference(&ran).all(|u| alone.contains(u)), "{ran:?}");
}

/// [`go_scenario`] with `data`'s test opening pricing's source only when `HUSHRUN_TEST_OPEN` is
/// 1, and failing otherwise: two machines see it read two lists, and it passes on one only.
fn go_disagreeing(open: bool) -> Scenario {
    let mut s = go_scenario();
    for (path, text) in &mut s.files {
        if path == "data/data_test.go" {
            *text = "package data\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestIt(t *testing.T) {\n\tif os.Getenv(\"HUSHRUN_TEST_OPEN\") != \"1\" {\n\t\tt.Fatal(\"not opened\")\n\t}\n\tif _, err := os.ReadFile(\"../pricing/pricing.go\"); err != nil {\n\t\tt.Fatal(err)\n\t}\n}\n".to_string();
        }
    }
    s.env
        .push(("HUSHRUN_TEST_OPEN", if open { "1" } else { "0" }.into()));
    s
}

// covers: C-38, C-34
#[test]
fn a_pass_on_one_list_and_a_fail_on_another_runs_the_test_file() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let closed = go_disagreeing(false);
    let open = go_disagreeing(true);
    let lab = Lab::new(&open);
    // A machine where `data` opens nothing and fails: a fail under the key of what go list says.
    let ran = lab.fresh("closed", &closed).run(&closed, &lab.results);
    assert_ne!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    // One where it opens pricing's source and passes: a pass under that list's key.
    let ran = lab.fresh("open", &open).run(&open, &lab.results);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!((open.ran)(&ran).contains("data"), "{}", ran.stderr);
    assert_eq!(lab.results.hints().len(), 2, "{:?}", lab.results.hints());
    // A third, with greet's source changed: one list has a pass, another a fail. It runs `data`.
    let third = lab.fresh("third", &open);
    third.write(open.change.0, open.change.1);
    let ran = third.run(&open, &lab.results);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    let runs = lab.results.runs();
    let o = Outcome {
        plan: ran.plan_line(),
        ran: (open.ran)(&ran),
        units: runs[runs.len() - 1]["units"].clone(),
        stderr: ran.stderr,
        ready: None,
        origins: BTreeMap::new(),
        machine: None,
    };
    assert!(o.ran.contains("data"), "{}", o.stderr);
    assert_could_reuse(
        "with two lists that disagree",
        &open,
        &o,
        &set(&["cart", "fixtures", "pricing"]),
    );
}

// covers: C-38
#[test]
fn a_hint_that_couldnt_be_sent_is_sent_by_the_next_run_and_never_twice() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let s = go_scenario();
    let lab = Lab::new(&s);
    lab.results.answer(
        "PUT",
        "v0/units/hints/",
        403,
        serde_json::json!({"error": {"code": "forbidden", "message": "not now"}}),
        1,
    );
    let ran = lab.first.run(&s, &lab.results);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert_eq!(lab.puts(), 1, "{:?}", lab.hint_requests());
    assert!(lab.results.hints().is_empty());
    // Sending it failing changes nothing a person reads.
    assert!(!ran.stderr.contains("hint"), "{}", ran.stderr);
    // The next run sends the same hint again, from what this machine kept, and nothing else.
    for (n, text) in ["\"h\" + \"i\"", "\"hi\" + \"\""].iter().enumerate() {
        lab.first.write(
            "greet/greet.go",
            &format!("package greet\n\nfunc Hello() string {{ return {text} }}\n"),
        );
        let ran = lab.first.run(&s, &lab.results);
        assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
        // Then the hint is the server's, and a run that would send the same sends nothing.
        assert_eq!(lab.puts(), 2, "run {n}: {:?}", lab.hint_requests());
        assert_eq!(lab.results.hints().len(), 1);
    }
}

// covers: C-38
#[test]
fn a_server_from_before_hints_changes_nothing_and_is_said_once() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    for status in [404, 405] {
        let s = go_scenario();
        let lab = Lab::new(&s);
        lab.results.forget_hints(status);
        let ran = lab.first.run(&s, &lab.results);
        assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
        assert_eq!(ran.stderr.matches(NO_HINTS).count(), 1, "{}", ran.stderr);
        let fresh = lab.fresh("fresh", &s);
        fresh.write(s.change.0, s.change.1);
        let ran = fresh.run(&s, &lab.results);
        assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
        assert_eq!(ran.stderr.matches(NO_HINTS).count(), 1, "{}", ran.stderr);
        // As before hints: what go list tells a fresh machine alone.
        assert_eq!(
            ran.plan_line().as_deref(),
            Some(
                "hushrun: unit: 3 of 5 packages run; 2 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
            ),
            "{status}: {}",
            ran.stderr
        );
        // One lookup a run, and nothing sent after the first said no.
        assert_eq!(
            lab.hint_requests(),
            vec![
                ("POST".to_string(), "v0/units/hints/lookup".to_string()),
                ("POST".to_string(), "v0/units/hints/lookup".to_string()),
            ],
            "{status}"
        );
    }
}

// covers: C-38, C-25
#[test]
fn counts_from_an_os_reads_that_systems_hints_and_a_run_that_doesnt_meet_it_sends_none() {
    if !tools("Go", succeeds("go", &["version"])) {
        return;
    }
    let here = hints::Os::this().expect("a system");
    let other = if here == hints::Os::Linux {
        hints::Os::Macos
    } else {
        hints::Os::Linux
    };
    let mut s = go_scenario();
    for (path, text) in &mut s.files {
        if path == "hushrun.yaml" {
            *text = format!("{text}counts_from:\n  os: {}\n", other.word());
        }
    }
    let lab = Lab::new(&s);
    let ran = lab.first.run(&s, &lab.results);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!(ran.stderr.contains("won't count"), "{}", ran.stderr);
    let asked: Vec<Value> = lab
        .results
        .raw_requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/v0/units/hints/lookup"))
        .map(|r| serde_json::from_slice(&r.body).expect("JSON"))
        .collect();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["os"], other.word(), "{asked:?}");
    assert_eq!(lab.puts(), 0, "{:?}", lab.hint_requests());
}

// Tests one by one across machines (C-39, C-40, C-41)

/// A shop whose one test file has `passing` tests that call the cart through `app/wa.py`, two
/// that call pricing through `app/wb.py`, and with `failing`, one that fails: each test reads
/// what it calls, and the test file reads both.
fn tests_scenario(tests: &str, failing: bool) -> Scenario {
    let mut test_file = String::from(
        "from app import wa, wb\n\ndef test_price_a():\n    assert wb.run() == 6\n\ndef test_price_b():\n    assert wb.run() == 6\n",
    );
    for n in 0..10 {
        test_file.push_str(&format!(
            "\ndef test_total_{n}():\n    assert wa.run() == 6\n"
        ));
    }
    if failing {
        test_file.push_str("\ndef test_fails():\n    assert wa.run() == 7\n");
    }
    let files = vec![
        file(
            "hushrun.yaml",
            &format!(
                "version: 1\nrepository: {REPOSITORY}\nchecks:\n  unit:\n    tests: {tests}\n"
            ),
        ),
        file(".gitignore", "__pycache__/\n.pytest_cache/\n"),
        file(
            "pytest.ini",
            "[pytest]\ntestpaths = tests\npythonpath = .\n",
        ),
        file("app/__init__.py", ""),
        file("app/cart.py", "def total(xs):\n    return sum(xs) * 2\n"),
        file("app/pricing.py", "def price(x):\n    return x * 2\n"),
        file(
            "app/wa.py",
            "from app.cart import total\n\ndef run():\n    return total([1, 2])\n",
        ),
        file(
            "app/wb.py",
            "from app.pricing import price\n\ndef run():\n    return price(3)\n",
        ),
        file("tests/test_shop.py", &test_file),
    ];
    Scenario {
        check: "unit",
        files,
        command: pytest_command(),
        env: vec![],
        prepare: |_| {},
        change: (
            "app/pricing.py",
            "def price(x):\n    return x * 2  # changed\n",
        ),
        all: set(&["tests/test_shop.py"]),
        must_run: set(&["tests/test_shop.py"]),
        not_followed: BTreeSet::new(),
        ran: pytest_ran,
        ready: false,
    }
}

/// The tests pytest's `-rA` summary says ran.
fn tests_ran(ran: &Ran) -> BTreeSet<String> {
    ran.stdout
        .lines()
        .filter_map(|l| {
            l.strip_prefix("PASSED ")
                .or_else(|| l.strip_prefix("FAILED "))
        })
        .map(|l| l.split(' ').next().unwrap_or(l).to_string())
        .collect()
}

/// The line before COMMAND when tests are the unit.
fn tests_plan_line(ran: &Ran) -> Option<String> {
    ran.stderr
        .lines()
        .find(|l| l.starts_with("hushrun: unit: ") && l.contains(" run"))
        .map(str::to_string)
}

/// \[C-41\] Runs `s` on a first machine, then, on the server as it left it and with `change`
/// made or not, on a machine with its store and on a fresh one: each run's tests and plan line.
/// No hint the first machine sent holds a test's id in its content.
fn tests_on_two_machines(s: &Scenario, change: bool) -> [(BTreeSet<String>, Option<String>); 2] {
    let lab = Lab::new(s);
    let first = lab.first.run(s, &lab.results);
    assert_eq!((s.ran)(&first), s.all, "{}\n{}", first.stdout, first.stderr);
    let kept = lab.results.hints();
    assert_eq!(kept.len(), 1, "the first run sends one hint: {kept:?}");
    let content = hints::inflate(&kept[0].body).expect("a hint");
    assert!(
        content.starts_with(b"hushrun.unit-hints/v1\n"),
        "a hint with tests"
    );
    for id in ["test_total_0", "test_price_a", "test_fails"] {
        assert!(
            !content.windows(id.len()).any(|w| w == id.as_bytes()),
            "{id} is sealed"
        );
    }
    let left = lab.results.recorded_so_far();
    let run_on = |name: &str, copy_store: bool| {
        let machine = lab.fresh(name, s);
        if copy_store {
            let units = machine.cache.join("units");
            std::fs::create_dir_all(&units).expect("mkdir");
            for path in store::stores(&lab.first.cache) {
                std::fs::copy(&path, units.join(path.file_name().expect("name")))
                    .expect("copy the store");
            }
        }
        if change {
            machine.write(s.change.0, s.change.1);
        }
        lab.results.go_back_to(&left);
        let ran = machine.run(s, &lab.results);
        (tests_ran(&ran), tests_plan_line(&ran), ran.stderr)
    };
    let (copied_ran, copied_line, copied_err) = run_on("copied", true);
    let (fresh_ran, fresh_line, fresh_err) = run_on("fresh", false);
    assert!(copied_line.is_some(), "{copied_err}");
    assert!(fresh_line.is_some(), "{fresh_err}");
    [(copied_ran, copied_line), (fresh_ran, fresh_line)]
}

/// Asserts that `ran` holds every test of `must`, and of the others all but the audit's share of
/// `reusable`, which run as a check.
fn assert_tests_reused(what: &str, ran: &BTreeSet<String>, must: &[&str], reusable: usize) {
    for test in must {
        assert!(ran.contains(*test), "{what}: {test} ran: {ran:?}");
    }
    let audited = hushrun_core::units::audit_share(reusable);
    assert_eq!(
        ran.len(),
        must.len() + audited,
        "{what}: {} tests ran; {} must and {audited} as a check: {ran:?}",
        ran.len(),
        must.len()
    );
}

// covers: C-41, C-39
#[test]
fn with_reads_file_a_fresh_machine_reuses_the_passing_tests_of_a_test_file_that_failed() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // One test fails: the test file records no pass, each test its own, and its hint holds their
    // ids. Nothing changes: a machine with its store, and a fresh one with its hint, reuse every
    // test that passed but the audit's share, and run the one that failed.
    let s = tests_scenario("{mode: reuse, unit: test}", true);
    let [(copied, copied_line), (fresh, fresh_line)] = tests_on_two_machines(&s, false);
    let failing = "tests/test_shop.py::test_fails";
    assert_tests_reused("with its store", &copied, &[failing], 12);
    assert_tests_reused("on a fresh machine", &fresh, &[failing], 12);
    assert_eq!(fresh_line, copied_line);
    assert_eq!(
        copied_line.as_deref(),
        Some(
            "hushrun: unit: 2 of 13 tests run, in 1 of 1 test file; 11 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
}

/// Whether the Python the fixtures run watches a test: 3.12 or later (C-42).
fn python_watches() -> bool {
    succeeds(
        &python(),
        &[
            "-c",
            "import sys; sys.exit(0 if hasattr(sys, 'monitoring') else 1)",
        ],
    )
}

// covers: C-41, C-40
#[test]
fn with_reads_test_a_fresh_machine_reuses_each_test_on_the_list_another_machine_saw_it_read() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    if !python_watches() {
        eprintln!("skipped: this Python has no sys.monitoring, so pytest watches no test");
        return;
    }
    // Pricing changes: the test file and its two tests that call pricing run; the ten that
    // call the cart didn't read it, and are reused but the audit's share, on the lists the first
    // machine saw: from its store, and on a fresh machine from its hint.
    let s = tests_scenario("{mode: reuse, unit: test, reads: test}", false);
    let [(copied, copied_line), (fresh, fresh_line)] = tests_on_two_machines(&s, true);
    let pricing = [
        "tests/test_shop.py::test_price_a",
        "tests/test_shop.py::test_price_b",
    ];
    assert_tests_reused("with its store", &copied, &pricing, 10);
    assert_tests_reused("on a fresh machine", &fresh, &pricing, 10);
    assert_eq!(fresh_line, copied_line);
    assert_eq!(
        copied_line.as_deref(),
        Some(
            "hushrun: unit: 3 of 12 tests run, in 1 of 1 test file; 9 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
}
