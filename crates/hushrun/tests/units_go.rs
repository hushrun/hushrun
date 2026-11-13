//! \[ADR 0070\] Test files with `go test` (C-30 to C-35): a unit is a package's tests, what it read
//! is what `go list` says its test binary is built from and what Go's test log says it opened, and
//! reuse never misses a failure.
//!
//! The fixtures use the standard library only, and need Go. They say so and pass where it is
//! missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/store.rs"]
mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

/// Environment variables a test must never inherit.
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
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "GOFLAGS",
    "GOOS",
    "GOARCH",
    "GOWORK",
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
const MODULE: &str = "example.com/shop";

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

    /// Go as these tests run it: this machine's build cache, so that the standard library builds
    /// once, no toolchain download, and no configuration file of the person's.
    fn go_env(cmd: &mut Command) {
        cmd.env("GOCACHE", go_cache())
            .env("GOTOOLCHAIN", "local")
            .env("GOENV", "off")
            .env("GOFLAGS", "")
            .env_remove("GOFLAGS");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(cmd);
    }

    fn hushrun(&self) -> Command {
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
        Repo::go_env(&mut cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        cmd
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = self.hushrun();
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

    /// `go test -json` with everything run, outside hushrun and Go's own cache: what fails.
    fn truth(&self, flags: &[&str]) -> BTreeSet<String> {
        let mut cmd = Command::new("go");
        cmd.current_dir(&self.repo);
        Repo::go_env(&mut cmd);
        let output = cmd
            .args(["test", "-json", "-count=1"])
            .args(flags)
            .arg("./...")
            .output()
            .expect("go test");
        packages(&String::from_utf8_lossy(&output.stdout))
            .into_iter()
            .filter(|(_, p)| p.failed)
            .map(|(name, _)| name)
            .collect()
    }

    /// The check's map, as this cache's store holds it (ADR 0072).
    fn map(&self) -> Value {
        store::map(&self.cache)
    }

    /// The newest result this machine recorded for the check.
    fn newest_result(&self) -> Value {
        let mut files = Vec::new();
        collect(&self.cache.join("results/results/v0"), &mut files);
        files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
        let newest = files.last().expect("a result");
        serde_json::from_slice(&std::fs::read(newest).unwrap()).unwrap()
    }
}

fn collect(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
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

/// Go's own build cache, which the tests share.
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

/// One package of `go test -json`'s output.
#[derive(Debug, Default)]
struct Package {
    /// A test of it ran.
    ran: bool,
    failed: bool,
}

/// `go test -json`'s packages, by import path.
fn packages(stdout: &str) -> BTreeMap<String, Package> {
    let mut out: BTreeMap<String, Package> = BTreeMap::new();
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(package) = event["Package"].as_str() else {
            continue;
        };
        let entry = out.entry(package.to_string()).or_default();
        match (event["Action"].as_str(), event["Test"].as_str()) {
            (Some("run"), Some(_)) => entry.ran = true,
            (Some("fail"), None) => entry.failed = true,
            _ => {}
        }
    }
    out
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

fn has_go() -> bool {
    Command::new("go")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

/// A shop in Go: a pricing package and a cart that imports it, each with tests; a greeting with
/// tests of its own; and a command with none.
fn shop(tests: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file(
            "pricing/pricing.go",
            "package pricing\n\nfunc Price(x int) int { return x * 2 }\n",
        ),
        file(
            "pricing/pricing_test.go",
            "package pricing\n\nimport \"testing\"\n\nfunc TestPrice(t *testing.T) {\n\tif Price(3) != 6 {\n\t\tt.Fatal(\"price\")\n\t}\n}\n",
        ),
        file(
            "cart/cart.go",
            &format!(
                "package cart\n\nimport \"{MODULE}/pricing\"\n\nfunc Total(xs []int) int {{\n\tn := 0\n\tfor _, x := range xs {{\n\t\tn += pricing.Price(x)\n\t}}\n\treturn n\n}}\n"
            ),
        ),
        file(
            "cart/cart_test.go",
            &format!(
                "package cart_test\n\nimport (\n\t\"testing\"\n\n\t\"{MODULE}/cart\"\n)\n\nfunc TestTotal(t *testing.T) {{\n\tif cart.Total([]int{{1, 2}}) != 6 {{\n\t\tt.Fatal(\"total\")\n\t}}\n}}\n"
            ),
        ),
        file(
            "greet/greet.go",
            "package greet\n\nfunc Hello() string { return \"hi\" }\n",
        ),
        file(
            "greet/greet_test.go",
            "package greet\n\nimport \"testing\"\n\nfunc TestHello(t *testing.T) {\n\tif Hello() != \"hi\" {\n\t\tt.Fatal(\"hello\")\n\t}\n}\n",
        ),
        file("cmd/tool/main.go", "package main\n\nfunc main() {}\n"),
    ]
}

const GO_TEST: &[&str] = &["run", "unit", "--", "go", "test", "-json", "./..."];

// covers: C-30, C-31, C-32, C-35
#[test]
fn go_packages_are_observed_from_go_list_and_their_test_log() {
    if !tools("Go", has_go()) {
        return;
    }
    let mut files = shop("observe");
    files.push(file(
        "greet/greet_data_test.go",
        "package greet\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestData(t *testing.T) {\n\tif _, err := os.ReadFile(\"../pricing/pricing.go\"); err != nil {\n\t\tt.Fatal(err)\n\t}\n\tif _, err := os.ReadDir(\"testdata\"); err != nil {\n\t\tt.Fatal(err)\n\t}\n}\n",
    ));
    files.push(file("greet/testdata/a.txt", "a\n"));
    let r = Repo::new(&files);
    let first = r.run(GO_TEST, &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(
        first.observe_line(),
        "hushrun: unit: with `tests: reuse`, all 3 test files would have run: none had passed here yet on the files it read"
    );
    let map = r.map();
    let reads = |unit: &str| -> Vec<String> {
        map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit}: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    // A package's tests read what its test binary is built from: its own files and those of
    // every package of the repository it imports, its tests included.
    assert_eq!(
        reads("cart"),
        ["cart/cart.go", "cart/cart_test.go", "pricing/pricing.go"]
    );
    assert_eq!(
        reads("pricing"),
        ["pricing/pricing.go", "pricing/pricing_test.go"]
    );
    // And what Go's test log says they opened: a source read as data. A directory they listed
    // is kept for the plan, which reads every file below it then.
    assert_eq!(
        reads("greet"),
        [
            "greet/greet.go",
            "greet/greet_data_test.go",
            "greet/greet_test.go",
            "pricing/pricing.go"
        ]
    );
    assert_eq!(
        map["units"]["greet"]["dirs"],
        serde_json::json!(["greet/testdata"])
    );
    // The sources are the .go files go list names; a command without tests is one.
    let sources: Vec<&str> = map["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(sources.contains(&"cmd/tool/main.go"), "{sources:?}");
    assert!(!sources.contains(&"greet/greet_test.go"), "{sources:?}");
    assert!(map["rest"]["go.mod"].is_string(), "{map:#}");
    assert!(map["rest"]["greet/testdata/a.txt"].is_string(), "{map:#}");
    assert_eq!(
        r.newest_result()["units"],
        serde_json::json!({"total": 3, "ran": 3, "reused": 0})
    );

    // Go's own test cache neither stores nor finds a result through hushrun's runner: run again
    // with --force, and every package runs, none "(cached)".
    let again = r.run(
        &["run", "--force", "unit", "--", "go", "test", "./..."],
        &[],
    );
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(!again.stdout.contains("(cached)"), "{}", again.stdout);
    let again = r.run(
        &["run", "--force", "unit", "--", "go", "test", "./..."],
        &[],
    );
    assert!(!again.stdout.contains("(cached)"), "{}", again.stdout);
    assert_eq!(
        r.newest_result()["units"],
        serde_json::json!({"total": 3, "ran": 3, "reused": 0})
    );

    // With nothing changed but one package, `observe` says what reuse would have done.
    r.write(
        "greet/greet.go",
        "package greet\n\nfunc Hello() string { return \"h\" + \"i\" }\n",
    );
    let changed = r.run(GO_TEST, &[]);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        hushrun_testkit::lines::without_saving(changed.observe_line()),
        "hushrun: unit: with `tests: reuse`, 1 of 3 test files would have run; 2 already passed on the files they read"
    );
}

// covers: C-32, C-35
#[test]
fn what_go_cant_show_makes_a_package_not_observed() {
    if !tools("Go", has_go()) {
        return;
    }
    let mut files = shop("observe");
    // A TestMain runs before Go's test log starts.
    files.push(file(
        "pricing/main_test.go",
        "package pricing\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestMain(m *testing.M) { os.Exit(m.Run()) }\n",
    ));
    // A test that starts a process: what the process reads isn't seen.
    files.push(file(
        "greet/proc_test.go",
        "package greet\n\nimport (\n\t\"os/exec\"\n\t\"testing\"\n)\n\nfunc TestProc(t *testing.T) {\n\tif err := exec.Command(\"go\", \"version\").Run(); err != nil {\n\t\tt.Fatal(err)\n\t}\n}\n",
    ));
    let r = Repo::new(&files);
    let first = r.run(GO_TEST, &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let map = r.map();
    assert_eq!(
        map["units"]["pricing"]["notFollowed"],
        serde_json::json!(["test-main"])
    );
    assert_eq!(
        map["units"]["greet"]["notFollowed"],
        serde_json::json!(["subprocess"])
    );
    assert!(map["units"]["cart"]["notFollowed"].is_null(), "{map:#}");
    assert!(
        first.observe_line().ends_with(
            "; hushrun couldn't follow all that 2 of them read (a process it started, a TestMain), so they would always run"
        ),
        "{}",
        first.stderr
    );
}

// covers: C-32
#[test]
fn go_test_in_a_form_hushrun_doesnt_read_is_left_alone_and_says_why() {
    if !tools("Go", has_go()) {
        return;
    }
    let r = Repo::new(&shop("observe"));
    let ran = r.run(
        &["run", "unit", "--", "go", "test", "-exec", "env", "./..."],
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains(
            "hushrun: unit: test files aren't observed: COMMAND starts test binaries through a program of its own (-exec)"
        ),
        "{}",
        ran.stderr
    );
    // Nothing of hushrun's reached COMMAND: no runner, no directory.
    assert!(
        !r.cache.join("units").join("runs").exists() || {
            std::fs::read_dir(r.cache.join("units/runs"))
                .unwrap()
                .count()
                == 0
        }
    );
    // GOFLAGS keeps what it said, and a test sees it as it was.
    r.write(
        "greet/env_test.go",
        "package greet\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestEnv(t *testing.T) {\n\tif os.Getenv(\"GOFLAGS\") != \"-count=1\" || os.Getenv(\"HUSHRUN_UNITS\") != \"\" || os.Getenv(\"HUSHRUN_GOFLAGS\") != \"\" {\n\t\tt.Fatalf(\"GOFLAGS=%q\", os.Getenv(\"GOFLAGS\"))\n\t}\n}\n",
    );
    let kept = r.run(GO_TEST, &[("GOFLAGS", "-count=1")]);
    assert_eq!(kept.code, 0, "{}\n{}", kept.stdout, kept.stderr);
    assert_eq!(
        r.map()["units"]["greet"]["reads"].as_array().unwrap().len(),
        3
    );
}

// covers: C-32, C-35
#[test]
fn the_tools_hushrun_starts_for_go_get_commands_own_env_assignments() {
    if !tools("Go", has_go()) {
        return;
    }
    // `env GOARCH=<another> go test`: `go env` and `go list`, which hushrun starts, see the
    // GOARCH COMMAND sets, so hushrun knows it tests for another target, and loads nothing, rather
    // than keying packages on the files `go list` names for this one.
    let host = Command::new("go")
        .args(["env", "GOHOSTARCH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let other = if host == "arm64" { "amd64" } else { "arm64" };
    let r = Repo::new(&shop("observe"));
    let goarch = format!("GOARCH={other}");
    let ran = r.run(
        &[
            "run", "unit", "--", "env", &goarch, "go", "test", "-count=1", "./greet",
        ],
        &[],
    );
    assert!(
        ran.stderr.contains(&format!(
            "hushrun: unit: test files aren't observed: it tests for {}/{other} on",
            std::env::consts::OS.replace("macos", "darwin")
        )),
        "{}",
        ran.stderr
    );
}

// covers: C-34, C-35
#[test]
fn go_reuses_what_already_passed_and_audits_it() {
    if !tools("Go", has_go()) {
        return;
    }
    let mut files = shop("reuse");
    for n in 1..=4 {
        files.push(file(
            &format!("extra{n}/extra.go"),
            &format!("package extra{n}\n\nfunc N() int {{ return {n} }}\n"),
        ));
        files.push(file(
            &format!("extra{n}/extra_test.go"),
            &format!("package extra{n}\n\nimport \"testing\"\n\nfunc TestN(t *testing.T) {{\n\tif N() != {n} {{\n\t\tt.Fatal(\"n\")\n\t}}\n}}\n"),
        ));
    }
    let r = Repo::new(&files);
    let first = r.run(GO_TEST, &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(
        first.plan_line(),
        "hushrun: unit: all 7 test files run: none had passed here yet on the files it read"
    );
    // pricing changes: cart, which imports it, runs with it; of the other five, one runs as a
    // check, and four are reused.
    r.write(
        "pricing/pricing.go",
        "package pricing\n\nfunc Price(x int) int { return x + x }\n",
    );
    let second = r.run(GO_TEST, &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        second.plan_line(),
        "hushrun: unit: 3 of 7 test files run; 4 already passed on the files they read, and 1 more that did runs as a check"
    );
    let ran = packages(&second.stdout);
    assert!(ran[&format!("{MODULE}/pricing")].ran && ran[&format!("{MODULE}/cart")].ran);
    assert_eq!(ran.values().filter(|p| p.ran).count(), 3, "{ran:?}");
    let units = r.newest_result()["units"].clone();
    assert!(units["savedMs"].is_u64(), "{units}");
    assert_eq!(
        units,
        serde_json::json!({"total": 7, "ran": 3, "reused": 4, "savedMs": units["savedMs"]})
    );
    // What reuse saved, from the reused packages' recorded times: below a second it says only how
    // many, from a second on how long too, which a loaded machine reaches.
    assert!(
        hushrun_testkit::lines::says_reused(&second.stderr, 4, "test files"),
        "{}",
        second.stderr
    );
    // A reused package stays in the map, though its name is a directory: what it opened is the
    // next plan's hint.
    assert!(r.map()["units"]["greet"].is_object(), "{:#}", r.map());
    // \[C-37\] `hushrun why` knows a reused package by its directory.
    let reused = r.run(&["why", "unit", "extra1"], &[]);
    assert!(
        reused.stdout.contains("extra1 would be reused"),
        "{}\n{}",
        reused.stdout,
        reused.stderr
    );
    // \[ADR 0073\] Why, for a package: by its directory or a test file of it, with the imports
    // between it and what changed, as far as this machine knows without go list.
    r.write(
        "pricing/pricing.go",
        "package pricing\n\nfunc Price(x int) int { return 2 * x }\n",
    );
    let why = r.run(&["why", "unit", "cart/cart_test.go"], &[]);
    assert_eq!(why.code, 0, "{}", why.stderr);
    assert!(
        why.stdout.starts_with(
            "cart runs: it read pricing/pricing.go, which changed\n  cart imports pricing\n  as far as this machine knows: its packages"
        ),
        "{}",
        why.stdout
    );
    let why = r.run(&["why", "unit", "greet"], &[]);
    assert!(
        why.stdout.starts_with("greet would be reused: "),
        "{}",
        why.stdout
    );
}

// covers: C-34, C-35
#[test]
fn an_injected_missed_dependency_in_go_is_caught_by_an_audit() {
    if !tools("Go", has_go()) {
        return;
    }
    // A test that fails when a file outside the check's files exists: what no adapter sees. It
    // looks for it through a variable the test log records, not by its path.
    let files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", "flag\n"),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file("a/a.go", "package a\n\nfunc A() int { return 1 }\n"),
        file(
            "a/a_test.go",
            "package a\n\nimport (\n\t\"os\"\n\t\"path/filepath\"\n\t\"testing\"\n)\n\nfunc TestA(t *testing.T) {\n\tflag := filepath.Join(os.Getenv(\"FLAG_DIR\"), \"flag\")\n\tif _, err := os.Lstat(flag); err == nil {\n\t\tt.Fatal(\"flag\")\n\t}\n}\n",
        ),
        file("b/b.go", "package b\n\nfunc B() int { return 2 }\n"),
        file(
            "b/b_test.go",
            "package b\n\nimport \"testing\"\n\nfunc TestB(t *testing.T) {\n\tif B() != 2 {\n\t\tt.Fatal(\"b\")\n\t}\n}\n",
        ),
    ];
    let r = Repo::new(&files);
    let outside = r.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let flag_dir = outside.to_str().unwrap().to_string();
    let env = [("FLAG_DIR", flag_dir.as_str())];
    assert_eq!(r.run(GO_TEST, &env).code, 0);
    std::fs::write(outside.join("flag"), "").unwrap();
    r.write("b/b.go", "package b\n\nfunc B() int { return 1 + 1 }\n");
    let caught = r.run(GO_TEST, &env);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        caught.plan_line(),
        "hushrun: unit: all 2 test files run; 1 that already passed on the files it read runs as a check"
    );
    assert!(
        caught.stderr.contains("unit: a failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"),
        "{}",
        caught.stderr
    );
}

/// What a hazard does to a repository.
struct Hazard {
    name: &'static str,
    /// Files added to the shop for it.
    files: Vec<(String, String)>,
    /// The change, made after a run where everything passed.
    change: fn(&Repo),
    /// `go test`'s flags, in COMMAND and in the run that says what fails.
    flags: &'static [&'static str],
}

fn hazards() -> Vec<Hazard> {
    let m = MODULE;
    vec![
        Hazard {
            name: "a package registered by a blank import, changed",
            files: vec![
                file(
                    "reg/reg.go",
                    "package reg\n\nvar Names = map[string]int{}\n",
                ),
                file(
                    "plugins/x/x.go",
                    &format!(
                        "package x\n\nimport \"{m}/reg\"\n\nfunc init() {{ reg.Names[\"x\"] = 1 }}\n"
                    ),
                ),
                file(
                    "app/app_test.go",
                    &format!(
                        "package app\n\nimport (\n\t\"testing\"\n\n\t\"{m}/reg\"\n\t_ \"{m}/plugins/x\"\n)\n\nfunc TestRegistered(t *testing.T) {{\n\tif reg.Names[\"x\"] != 1 {{\n\t\tt.Fatal(\"x\")\n\t}}\n}}\n"
                    ),
                ),
                file("app/app.go", "package app\n"),
            ],
            change: |r| {
                r.write(
                    "plugins/x/x.go",
                    &format!("package x\n\nimport \"{MODULE}/reg\"\n\nfunc init() {{ reg.Names[\"x\"] = 2 }}\n"),
                );
            },
            flags: &[],
        },
        Hazard {
            name: "a source read as data",
            files: vec![file(
                "greet/source_test.go",
                "package greet\n\nimport (\n\t\"os\"\n\t\"strings\"\n\t\"testing\"\n)\n\nfunc TestSource(t *testing.T) {\n\tb, err := os.ReadFile(\"../pricing/pricing.go\")\n\tif err != nil || !strings.Contains(string(b), \"x * 2\") {\n\t\tt.Fatal(\"source\")\n\t}\n}\n",
            )],
            change: |r| {
                r.write(
                    "pricing/pricing.go",
                    "package pricing\n\nfunc Price(x int) int { return x + x }\n",
                );
            },
            flags: &[],
        },
        Hazard {
            name: "a source read only by a child process",
            files: vec![file(
                "greet/child_test.go",
                "package greet\n\nimport (\n\t\"os/exec\"\n\t\"strings\"\n\t\"testing\"\n)\n\nfunc TestChild(t *testing.T) {\n\tout, err := exec.Command(\"go\", \"list\", \"-f\", \"{{.Doc}}\", \"../pricing\").Output()\n\tif err != nil || strings.Contains(string(out), \"changed\") {\n\t\tt.Fatalf(\"%s %v\", out, err)\n\t}\n}\n",
            )],
            change: |r| {
                r.write(
                    "pricing/pricing.go",
                    "// Package pricing changed.\npackage pricing\n\nfunc Price(x int) int { return x * 2 }\n",
                );
            },
            flags: &[],
        },
        Hazard {
            name: "a source read only by a program started by a relative path from another directory",
            files: vec![file(
                "greet/relative_test.go",
                "package greet\n\nimport (\n\t\"os\"\n\t\"os/exec\"\n\t\"path/filepath\"\n\t\"testing\"\n)\n\nfunc TestRelative(t *testing.T) {\n\tdir := t.TempDir()\n\tif err := os.WriteFile(filepath.Join(dir, \"read.sh\"), []byte(\"#!/bin/sh\\ngrep -q 'x [*] 2' \\\"$1\\\"\\n\"), 0o755); err != nil {\n\t\tt.Fatal(err)\n\t}\n\tsource, _ := filepath.Abs(\"../pricing/pricing.go\")\n\tcmd := exec.Command(\"./read.sh\", source)\n\tcmd.Dir = dir\n\tif err := cmd.Run(); err != nil {\n\t\tt.Fatal(err)\n\t}\n}\n",
            )],
            change: |r| {
                r.write(
                    "pricing/pricing.go",
                    "package pricing\n\nfunc Price(x int) int { return x + x }\n",
                );
            },
            flags: &[],
        },
        Hazard {
            name: "a source a TestMain reads before the test log starts",
            files: vec![file(
                "greet/main_test.go",
                "package greet\n\nimport (\n\t\"os\"\n\t\"strings\"\n\t\"testing\"\n)\n\nfunc TestMain(m *testing.M) {\n\tb, _ := os.ReadFile(\"../pricing/pricing.go\")\n\tif strings.Contains(string(b), \"x + x\") {\n\t\tos.Exit(3)\n\t}\n\tos.Exit(m.Run())\n}\n",
            )],
            change: |r| {
                r.write(
                    "pricing/pricing.go",
                    "package pricing\n\nfunc Price(x int) int { return x + x }\n",
                );
            },
            flags: &[],
        },
        Hazard {
            name: "a new test file in a directory a test lists",
            files: vec![file(
                "greet/list_test.go",
                "package greet\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestList(t *testing.T) {\n\tentries, err := os.ReadDir(\"../cart\")\n\tif err != nil || len(entries) != 2 {\n\t\tt.Fatal(len(entries), err)\n\t}\n}\n",
            )],
            change: |r| {
                r.write(
                    "cart/more_test.go",
                    "package cart_test\n\nimport \"testing\"\n\nfunc TestMore(t *testing.T) {}\n",
                );
            },
            flags: &[],
        },
        Hazard {
            name: "go.mod, read by a test",
            files: vec![file(
                "greet/gomod_test.go",
                "package greet\n\nimport (\n\t\"os\"\n\t\"strings\"\n\t\"testing\"\n)\n\nfunc TestGoMod(t *testing.T) {\n\tb, _ := os.ReadFile(\"../go.mod\")\n\tif strings.Contains(string(b), \"changed\") {\n\t\tt.Fatal(\"go.mod\")\n\t}\n}\n",
            )],
            change: |r| {
                r.write(
                    "go.mod",
                    &format!("module {MODULE}\n\ngo 1.21\n\n// changed\n"),
                );
            },
            flags: &[],
        },
        Hazard {
            name: "a new file an embed pattern picks up",
            files: vec![
                file("web/static/a.txt", "a\n"),
                file(
                    "web/web.go",
                    "package web\n\nimport \"embed\"\n\n//go:embed static/*\nvar Static embed.FS\n",
                ),
                file(
                    "web/web_test.go",
                    "package web\n\nimport \"testing\"\n\nfunc TestStatic(t *testing.T) {\n\tentries, _ := Static.ReadDir(\"static\")\n\tif len(entries) != 1 {\n\t\tt.Fatal(len(entries))\n\t}\n}\n",
                ),
            ],
            change: |r| r.write("web/static/b.txt", "b\n"),
            flags: &[],
        },
        Hazard {
            name: "a new file that shadows a value",
            files: vec![],
            change: |r| {
                r.write(
                    "pricing/zz_override.go",
                    "package pricing\n\nfunc init() { factor = 3 }\n",
                );
                r.write(
                    "pricing/pricing.go",
                    "package pricing\n\nvar factor = 2\n\nfunc Price(x int) int { return x * factor }\n",
                );
            },
            flags: &[],
        },
        Hazard {
            name: "a shared test helper",
            files: vec![
                file(
                    "internal/testutil/testutil.go",
                    "package testutil\n\nfunc Want() int { return 6 }\n",
                ),
                file(
                    "cart/helper_test.go",
                    &format!(
                        "package cart_test\n\nimport (\n\t\"testing\"\n\n\t\"{m}/cart\"\n\t\"{m}/internal/testutil\"\n)\n\nfunc TestHelper(t *testing.T) {{\n\tif cart.Total([]int{{1, 2}}) != testutil.Want() {{\n\t\tt.Fatal(\"helper\")\n\t}}\n}}\n"
                    ),
                ),
            ],
            change: |r| {
                r.write(
                    "internal/testutil/testutil.go",
                    "package testutil\n\nfunc Want() int { return 7 }\n",
                );
            },
            flags: &[],
        },
        Hazard {
            name: "a deleted file a test reads",
            files: vec![
                file("greet/testdata/names.txt", "ann\n"),
                file(
                    "greet/names_test.go",
                    "package greet\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestNames(t *testing.T) {\n\tif _, err := os.ReadFile(\"testdata/names.txt\"); err != nil {\n\t\tt.Fatal(err)\n\t}\n}\n",
                ),
            ],
            change: |r| r.remove("greet/testdata/names.txt"),
            flags: &[],
        },
        Hazard {
            name: "a deleted source",
            files: vec![
                file(
                    "greet/more.go",
                    "package greet\n\nfunc More() string { return \"more\" }\n",
                ),
                file(
                    "greet/more_test.go",
                    "package greet\n\nimport \"testing\"\n\nfunc TestMore(t *testing.T) {\n\tif Hello()+\"!\" == \"\" {\n\t\tt.Fatal(\"more\")\n\t}\n}\n",
                ),
                file(
                    "greet/uses_more_test.go",
                    "package greet\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestUsesMore(t *testing.T) {\n\tif _, err := os.Stat(\"more.go\"); err != nil {\n\t\tt.Fatal(err)\n\t}\n}\n",
                ),
            ],
            change: |r| r.remove("greet/more.go"),
            flags: &[],
        },
        Hazard {
            name: "a file COMMAND's build tags select",
            files: vec![
                file(
                    "osdep/on.go",
                    "//go:build special\n\npackage osdep\n\nfunc V() int { return 1 }\n",
                ),
                file(
                    "osdep/off.go",
                    "//go:build !special\n\npackage osdep\n\nfunc V() int { return 1 }\n",
                ),
                file(
                    "osdep/osdep_test.go",
                    "package osdep\n\nimport \"testing\"\n\nfunc TestV(t *testing.T) {\n\tif V() != 1 {\n\t\tt.Fatal(\"v\")\n\t}\n}\n",
                ),
            ],
            change: |r| {
                r.write(
                    "osdep/on.go",
                    "//go:build special\n\npackage osdep\n\nfunc V() int { return 2 }\n",
                );
            },
            flags: &["-tags=special"],
        },
        Hazard {
            name: "a file build tags leave out, changed to be built",
            files: vec![
                file(
                    "osdep/on.go",
                    "//go:build special\n\npackage osdep\n\nfunc V() int { return 2 }\n",
                ),
                file(
                    "osdep/off.go",
                    "//go:build !special\n\npackage osdep\n\nfunc V() int { return 1 }\n",
                ),
                file(
                    "osdep/osdep_test.go",
                    "package osdep\n\nimport \"testing\"\n\nfunc TestV(t *testing.T) {\n\tif V() != 1 {\n\t\tt.Fatal(\"v\")\n\t}\n}\n",
                ),
            ],
            change: |r| {
                r.write(
                    "osdep/on.go",
                    "//go:build !special\n\npackage osdep\n\nfunc W() int { return 2 }\n\nfunc V() int { return 2 }\n",
                );
                r.remove("osdep/off.go");
            },
            flags: &[],
        },
    ]
}

/// The test files `observe` says reuse would have reused though they failed: each a failure the
/// plan, audits aside, would have missed.
fn missed(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| l.ends_with("with `tests: reuse` it would have been reused"))
        .map(str::to_string)
        .collect()
}

/// How many test files `observe` says reuse would have reused.
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
fn go_hazards_miss_no_failure() {
    if !tools("Go", has_go()) {
        return;
    }
    // In `observe`, which runs everything and computes the plan's keys without its audits, so
    // that an audit run can't hide a hole: for each hazard, every package that fails is one the
    // plan wouldn't have reused.
    let mut reused = 0;
    for hazard in hazards() {
        let mut files = shop("observe");
        files.extend(hazard.files.iter().cloned());
        let r = Repo::new(&files);
        let mut command: Vec<&str> = vec!["run", "unit", "--", "go", "test", "-json"];
        command.extend(hazard.flags);
        command.push("./...");
        let first = r.run(&command, &[]);
        assert_eq!(
            first.code, 0,
            "{}: {}\n{}",
            hazard.name, first.stdout, first.stderr
        );
        (hazard.change)(&r);
        let failing = r.truth(hazard.flags);
        assert!(
            !failing.is_empty(),
            "{}: nothing fails, so it tests nothing",
            hazard.name
        );
        let ran = r.run(&command, &[]);
        assert_ne!(ran.code, 0, "{}: {}", hazard.name, ran.stderr);
        let seen = packages(&ran.stdout);
        for package in &failing {
            assert!(
                seen.get(package).is_some_and(|s| s.failed),
                "{}: {package} fails when everything runs, and not in COMMAND\n{}",
                hazard.name,
                ran.stdout
            );
        }
        let missed = missed(&ran.stderr);
        assert!(
            missed.is_empty(),
            "{}: the plan would have reused what fails: {missed:?}\n{}",
            hazard.name,
            ran.stderr
        );
        reused += would_reuse(&ran.stderr);
    }
    // Not everything ran every time: the plan reuses where nothing it read changed.
    assert!(reused > 0);
}

fn has_cgo() -> bool {
    let enabled = Command::new("go")
        .args(["env", "CGO_ENABLED"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1");
    enabled
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

// covers: C-35
#[test]
fn a_package_whose_test_links_the_repositorys_cgo_is_not_observed() {
    if !tools("Go with cgo and a C compiler", has_go() && has_cgo()) {
        return;
    }
    let mut files = shop("observe");
    files.push(file(
        "native/native.go",
        "package native\n\n// int answer(void) { return 42; }\nimport \"C\"\n\nfunc Answer() int { return int(C.answer()) }\n",
    ));
    files.push(file(
        "native/native_test.go",
        "package native\n\nimport \"testing\"\n\nfunc TestAnswer(t *testing.T) {\n\tif Answer() != 42 {\n\t\tt.Fatal(\"answer\")\n\t}\n}\n",
    ));
    let r = Repo::new(&files);
    let ran = r.run(GO_TEST, &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let map = r.map();
    assert_eq!(
        map["units"]["native"]["notFollowed"],
        serde_json::json!(["native"])
    );
    assert!(map["units"]["greet"]["notFollowed"].is_null(), "{map:#}");
}

// covers: C-5, C-32
#[test]
fn with_tests_off_go_test_gets_nothing_new() {
    if !tools("Go", has_go()) {
        return;
    }
    let mut files = shop("off");
    files.push(file(
        "greet/env_test.go",
        "package greet\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestEnv(t *testing.T) {\n\tfor _, name := range []string{\"GOFLAGS\", \"HUSHRUN_UNITS\", \"HUSHRUN_GOFLAGS\"} {\n\t\tif v, ok := os.LookupEnv(name); ok {\n\t\t\tt.Fatalf(\"%s=%q\", name, v)\n\t\t}\n\t}\n}\n",
    ));
    let r = Repo::new(&files);
    let ran = r.run(GO_TEST, &[]);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!(!r.cache.join("units").exists());
}

// \[ADR 0083\] Tests one by one (C-45): a Go test is a top-level `Test…`, `Example…` or `Fuzz…`
// function of a package, its id its name.

/// A Go test's body that writes its name to `$RAN_LOG`, outside the repository, so that what ran
/// is known from the tests themselves, whatever `go test` printed.
const RAN: &str = "func ran(t testing.TB) {\n\tif p := os.Getenv(\"RAN_LOG\"); p != \"\" {\n\t\tf, _ := os.OpenFile(p, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)\n\t\tf.WriteString(t.Name() + \"\\n\")\n\t\tf.Close()\n\t}\n}\n";

/// One package, `cart`, with tests: `TestFlip` fails while `FAIL` is set, `TestOk` while `HIDDEN`
/// is, which no file says, as a missed dependency would; `TestFlipToo`, a name `TestFlip` is a
/// prefix of, `TestTwo`, `TestThree` and `TestFour` always pass; `body` adds more.
fn cart(tests: &str, body: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file(
            "cart/cart.go",
            "package cart\n\nfunc Total(xs []int) int {\n\tn := 0\n\tfor _, x := range xs {\n\t\tn += x\n\t}\n\treturn n\n}\n",
        ),
        file(
            "cart/cart_test.go",
            &format!(
                "package cart\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\n{RAN}\n\
                 func TestFlip(t *testing.T) {{\n\tran(t)\n\tif os.Getenv(\"FAIL\") != \"\" {{\n\t\tt.Fatal(\"flip\")\n\t}}\n}}\n\
                 func TestOk(t *testing.T) {{\n\tran(t)\n\tif os.Getenv(\"HIDDEN\") != \"\" {{\n\t\tt.Fatal(\"hidden\")\n\t}}\n}}\n\
                 func TestFlipToo(t *testing.T) {{ ran(t) }}\n\
                 func TestTwo(t *testing.T) {{ ran(t) }}\n\
                 func TestThree(t *testing.T) {{ ran(t) }}\n\
                 func TestFour(t *testing.T) {{ ran(t) }}\n{body}"
            ),
        ),
    ]
}

impl Repo {
    /// `hushrun run unit -- go test <flags> ./...`, with the tests' names written to a log: what
    /// ran, in order.
    fn go_tests(&self, flags: &[&str], env: &[(&str, &str)]) -> (Ran, Vec<String>) {
        let log = self.root.join("ran.log");
        let _ = std::fs::remove_file(&log);
        let mut args = vec!["run", "unit", "--", "go", "test"];
        args.extend(flags);
        args.push("./...");
        let log_text = log.display().to_string();
        let mut env: Vec<(&str, &str)> = env.to_vec();
        env.push(("RAN_LOG", &log_text));
        let ran = self.run(&args, &env);
        let names = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        (ran, names)
    }

    /// The tests of `unit` this machine's store holds, by id, with their last state.
    fn tests_of(&self, unit: &str) -> Value {
        self.map()["units"][unit]["tests"].clone()
    }
}

fn line<'a>(ran: &'a Ran, start: &str) -> &'a str {
    ran.stderr
        .lines()
        .find(|l| l.starts_with(start))
        .unwrap_or_else(|| panic!("no line starting {start:?} in:\n{}", ran.stderr))
}

// covers: C-45, C-39
#[test]
fn a_go_test_that_failed_runs_alone_and_its_packages_others_are_reused_one_by_one() {
    if !tools("Go", has_go()) {
        return;
    }
    let r = Repo::new(&cart(
        "{mode: reuse, unit: test, ignore: [\"docs/**\"]}",
        "",
    ));
    // With -v, so that Go names each test's outcome: without it, a package that failed keeps no
    // pass of its other tests (ADR 0083 Amendment 3). The first run, with TestFlip failing: every
    // test runs; the package didn't pass, so each test is recorded on its own.
    let (first, ran) = r.go_tests(&["-v"], &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(ran.len(), 6, "{ran:?}");
    assert_eq!(
        r.tests_of("cart"),
        serde_json::json!({"TestFlip": "failed", "TestFour": "passed", "TestOk": "passed",
                           "TestFlipToo": "passed", "TestThree": "passed", "TestTwo": "passed"})
    );
    assert_eq!(
        r.newest_result()["units"]["tests"],
        serde_json::json!({"total": 6, "ran": 6, "reused": 0})
    );

    // The same files, TestFlip still failing: only it runs, and one of the five that passed, as
    // a check (C-39's audit).
    let (second, ran) = r.go_tests(&["-v"], &[("FAIL", "1")]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        line(&second, "hushrun: unit: "),
        "hushrun: unit: 2 of 6 tests run, in 1 of 1 test file; 4 already passed on the files they read, and 1 more that did runs as a check"
    );
    assert_eq!(ran.len(), 2, "{ran:?}");
    assert!(ran.contains(&"TestFlip".to_string()), "{ran:?}");
    assert_eq!(
        r.newest_result()["units"]["tests"],
        serde_json::json!({"total": 6, "ran": 2, "reused": 4})
    );

    // Fixed: TestFlip runs again, with one more as a check, and passes; a reused test keeps the
    // row it stands on.
    let (third, ran) = r.go_tests(&["-v"], &[]);
    assert_eq!(third.code, 0, "{}\n{}", third.stdout, third.stderr);
    assert_eq!(ran.len(), 2, "{ran:?}");
    assert!(ran.contains(&"TestFlip".to_string()), "{ran:?}");
    assert_eq!(
        r.tests_of("cart"),
        serde_json::json!({"TestFlip": "passed", "TestFour": "passed", "TestOk": "passed",
                           "TestFlipToo": "passed", "TestThree": "passed", "TestTwo": "passed"})
    );
    // Every test has passed on these files: the same files again reuse them all, and the check
    // skips whole (C-10).
    let (fourth, ran) = r.go_tests(&["-v"], &[]);
    assert!(fourth.stderr.contains("unit skipped"), "{}", fourth.stderr);
    assert!(ran.is_empty(), "{ran:?}");
    // With the check run again for a file no test reads, every test but one, a check, is reused:
    // hushrun's runner left five out of the package's binary with -test.skip.
    r.write("docs/notes.md", "notes\n");
    let (again, ran) = r.go_tests(&["-v"], &[]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(
        line(&again, "hushrun: unit: "),
        "hushrun: unit: 1 of 6 tests run, in 1 of 1 test file; 5 already passed on the files they read, and 1 more that did runs as a check"
    );
    assert_eq!(ran.len(), 1, "{ran:?}");
    assert_eq!(
        r.newest_result()["units"]["tests"],
        serde_json::json!({"total": 6, "ran": 1, "reused": 5})
    );
}

/// One package, `p`, whose `TestMid` does what `MODE` says, between two tests that pass; an
/// example and a fuzz target's seed that fail when `MODE` says so; and a benchmark, which isn't a
/// test.
fn shapes() -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml("{mode: observe, unit: test}")),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file("p/p.go", "package p\n\nfunc One() int { return 1 }\n"),
        file(
            "p/p_test.go",
            r#"package p

import (
	"fmt"
	"os"
	"testing"
	"time"
)

func mode() string { return os.Getenv("MODE") }

func TestFirst(t *testing.T) {}

func TestMid(t *testing.T) {
	switch mode() {
	case "fail":
		t.Error("boom")
	case "panic":
		panic("boom")
	case "exit0":
		os.Exit(0)
	case "exit1":
		os.Exit(1)
	case "hang":
		time.Sleep(time.Minute)
	case "glued-fail":
		fmt.Print("no newline")
		t.Error("boom")
	case "glued-pass":
		fmt.Print("no newline")
	case "skip":
		t.Skip("skipped")
	case "short":
		if testing.Short() {
			t.Skip("short")
		}
		t.Error("not short")
	case "parallel":
		for _, n := range []string{"a", "b"} {
			t.Run(n, func(t *testing.T) {
				t.Parallel()
				if n == "b" {
					t.Fatal("b")
				}
			})
		}
	case "spoof-pass":
		fmt.Println("--- FAIL: TestFirst (0.00s)")
		fmt.Println("PASS")
	case "spoof-exit":
		fmt.Println("--- FAIL: TestFirst (0.00s)")
		fmt.Println("FAIL")
		os.Exit(1)
	}
}

func TestLast(t *testing.T) {}

func Example_e() {
	if mode() == "example" {
		fmt.Println("bad")
	} else {
		fmt.Println("ok")
	}
	// Output: ok
}

func FuzzF(f *testing.F) {
	f.Add(1)
	f.Fuzz(func(t *testing.T, i int) {
		if mode() == "fuzz" {
			t.Fatal("seed")
		}
	})
}

func BenchmarkB(b *testing.B) {}
"#,
        ),
    ]
}

impl Repo {
    /// `hushrun run unit -- go test <flags> ./...` with `MODE` set, on a cache of its own, and
    /// what its store says of `p`'s tests after it.
    fn shape(&self, n: usize, mode: &str, flags: &[&str]) -> (Ran, Value) {
        let cache = self.root.join(format!("cache-{n}"));
        let mut args = vec!["run", "unit", "--", "go", "test"];
        args.extend(flags);
        args.push("./...");
        let cache_text = cache.display().to_string();
        let ran = self.run(&args, &[("MODE", mode), ("HUSHRUN_CACHE_DIR", &cache_text)]);
        (ran, store::map(&cache)["units"]["p"]["tests"].clone())
    }
}

/// Each test's state as the store holds it: `passed`, `failed` or `not-run` for each of
/// `Example_e`, `FuzzF`, `TestFirst`, `TestLast`, `TestMid`, in that order.
fn states(example: &str, fuzz: &str, first: &str, last: &str, mid: &str) -> Value {
    serde_json::json!({"Example_e": example, "FuzzF": fuzz, "TestFirst": first,
                       "TestLast": last, "TestMid": mid})
}

// covers: C-45
#[test]
fn a_go_test_passes_or_fails_by_what_its_binary_printed_and_how_it_ended() {
    if !tools("Go", has_go()) {
        return;
    }
    let r = Repo::new(&shapes());
    let all = states("passed", "passed", "passed", "passed", "passed");
    let mid = states("passed", "passed", "passed", "passed", "failed");
    // Without -v or -json Go names only what failed: a package that failed keeps no other pass.
    let unnamed = states("not-run", "not-run", "not-run", "not-run", "failed");
    // A binary that ended on Go's own summary, with its exit code: every test it ran passed but
    // those it named failed, however many times or in whatever order, when it passed or Go named
    // each test (-v, -json). Benchmarks aren't tests; a skipped test passed.
    let cases: &[(&str, &[&str], &Value)] = &[
        ("", &[], &all),
        ("", &["-json"], &all),
        ("fail", &[], &unnamed),
        ("fail", &["-v"], &mid),
        ("fail", &["-json"], &mid),
        ("fail", &["-count=2"], &unnamed),
        ("fail", &["-count=2", "-v"], &mid),
        ("fail", &["-shuffle=on", "-parallel=1", "-v"], &mid),
        ("fail", &["-bench=.", "-v"], &mid),
        ("parallel", &["-parallel=2", "-v"], &mid),
        ("skip", &[], &all),
        ("short", &["-short"], &all),
        ("glued-fail", &[], &unnamed),
        ("glued-fail", &["-json"], &mid),
        ("glued-pass", &[], &all),
        ("glued-pass", &["-json"], &all),
        // Coverage prints its line after Go's summary.
        ("", &["-cover"], &all),
        ("fail", &["-cover", "-json"], &mid),
    ];
    for (n, (mode, flags, want)) in cases.iter().enumerate() {
        let (ran, got) = r.shape(n, mode, flags);
        assert_eq!(&got, *want, "MODE={mode} {flags:?}\n{}", ran.stderr);
    }
    // A failing example and a fuzz target's failing seed are named, as tests are.
    let (ran, got) = r.shape(100, "example", &["-v"]);
    assert_eq!(
        got,
        states("failed", "passed", "passed", "passed", "passed"),
        "{}",
        ran.stderr
    );
    let (ran, got) = r.shape(101, "fuzz", &["-json"]);
    assert_eq!(
        got,
        states("passed", "failed", "passed", "passed", "passed"),
        "{}",
        ran.stderr
    );
    // A test that prints Go's words itself: what it names failed is read as failed, which only
    // runs that test again; and its "PASS" isn't the last line, Go's is.
    let (ran, got) = r.shape(102, "spoof-pass", &[]);
    assert_eq!(
        got,
        states("passed", "passed", "failed", "passed", "passed"),
        "{}",
        ran.stderr
    );
}

/// Two packages: `p`, whose `TestNeeds` skips when `TestBase` failed, which it does unless
/// `FIXED` is set, and `TestOk`, which passes; and `q1`, `q2`, `q3`, whose one test each passes.
fn needs() -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file("p/p.go", "package p\n"),
        file(
            "p/p_test.go",
            &format!(
                "package p\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\n{RAN}\nvar ready bool\n\nfunc TestBase(t *testing.T) {{\n\tran(t)\n\tif os.Getenv(\"FIXED\") == \"\" {{\n\t\tt.Fatal(\"base\")\n\t}}\n\tready = true\n}}\n\nfunc TestNeeds(t *testing.T) {{\n\tran(t)\n\tif !ready {{\n\t\tt.Skip(\"no base\")\n\t}}\n}}\n\nfunc TestOk(t *testing.T) {{ ran(t) }}\n"
            ),
        ),
    ];
    for n in 1..=3 {
        files.push(file(&format!("q{n}/q.go"), &format!("package q{n}\n")));
        files.push(file(
            &format!("q{n}/q_test.go"),
            &format!(
                "package q{n}\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\n{RAN}\nfunc TestQ{n}(t *testing.T) {{ ran(t) }}\n"
            ),
        ));
    }
    files
}

/// What `hushrun run` says when a failed package's other tests weren't kept.
const UNNAMED: &str = "a package that failed keeps its other tests' results only when go test names each one: add -v to keep them";

// covers: C-32, C-45
#[test]
fn a_go_package_that_failed_without_v_keeps_no_pass_of_its_other_tests() {
    if !tools("Go", has_go()) {
        return;
    }
    // Without -v Go names only the tests that failed: one that skipped because another failed
    // can't be told from one that passed, so no test of a package that failed records a pass.
    let r = Repo::new(&needs());
    let (first, ran) = r.go_tests(&[], &[]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(ran.len(), 6, "{ran:?}");
    assert_eq!(
        r.tests_of("p"),
        serde_json::json!({"TestBase": "failed", "TestNeeds": "not-run", "TestOk": "not-run"})
    );
    assert_eq!(r.tests_of("q1"), serde_json::json!({"TestQ1": "passed"}));
    assert_eq!(first.stderr.matches(UNNAMED).count(), 1, "{}", first.stderr);
    assert!(
        first.stderr.contains(&format!("hushrun: unit: {UNNAMED}")),
        "{}",
        first.stderr
    );
    // `hushrun why` gives the same reason.
    let why = r.run(&["why", "unit", "p::TestOk"], &[]);
    assert!(
        why.stdout.contains(UNNAMED) || why.stderr.contains(UNNAMED),
        "{}\n{}",
        why.stdout,
        why.stderr
    );
    // The same code again: every test of p runs, nothing of it left out; of q1, q2 and q3, which
    // passed whole, all but the one an audit may run are reused.
    let (second, ran) = r.go_tests(&[], &[]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    for test in ["TestBase", "TestNeeds", "TestOk"] {
        assert!(
            ran.contains(&test.to_string()),
            "{test}: {ran:?}\n{}",
            second.stderr
        );
    }
    assert!(ran.len() <= 4, "{ran:?}\n{}", second.stderr);
}

// covers: C-32, C-45
#[test]
fn a_go_package_that_failed_with_v_keeps_the_passes_it_names() {
    if !tools("Go", has_go()) {
        return;
    }
    // With -v Go names each test's outcome: the test that passed keeps its pass, the skip in a
    // run that failed records nothing, and nothing is said.
    let r = Repo::new(&needs());
    let (first, _) = r.go_tests(&["-v"], &[]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.tests_of("p"),
        serde_json::json!({"TestBase": "failed", "TestNeeds": "not-run", "TestOk": "passed"})
    );
    assert!(!first.stderr.contains(UNNAMED), "{}", first.stderr);
}

// covers: C-37, C-45
#[test]
fn why_a_go_test_left_unrun_by_a_panic_kept_no_result_is_how_its_binary_ended() {
    if !tools("Go", has_go()) {
        return;
    }
    // TestBase fails, then TestPanics panics: the binary never gets back to Go's summary, so
    // TestLater never runs. Its reason is how the binary ended, not -v, which would change nothing.
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file("p/p.go", "package p\n"),
        file(
            "p/p_test.go",
            "package p\n\nimport \"testing\"\n\nfunc TestBase(t *testing.T) { t.Error(\"base\") }\n\nfunc TestPanics(t *testing.T) { panic(\"boom\") }\n\nfunc TestLater(t *testing.T) {}\n",
        ),
    ]);
    let (first, _) = r.go_tests(&[], &[]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.tests_of("p"),
        serde_json::json!({"TestBase": "failed", "TestLater": "not-run", "TestPanics": "failed"})
    );
    let why = r.run(&["why", "unit", "p::TestLater"], &[]);
    let told = format!("{}\n{}", why.stdout, why.stderr);
    assert!(told.contains(ENDED_EARLY), "{told}");
    assert!(!told.contains(UNNAMED), "{told}");
}

// covers: C-37, C-45
#[test]
fn a_v_that_only_goflags_gives_keeps_the_passes_and_why_asks_for_no_v() {
    if !tools("Go", has_go()) {
        return;
    }
    // A -v that only GOFLAGS gives: Go names each test, the pass is kept, and the skip in a run
    // that failed records nothing for a reason -v has nothing to do with.
    let r = Repo::new(&needs());
    let (first, _) = r.go_tests(&[], &[("GOFLAGS", "-v")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        r.tests_of("p"),
        serde_json::json!({"TestBase": "failed", "TestNeeds": "not-run", "TestOk": "passed"})
    );
    assert!(!first.stderr.contains(UNNAMED), "{}", first.stderr);
    let why = r.run(&["why", "unit", "p::TestNeeds"], &[("GOFLAGS", "-v")]);
    let told = format!("{}\n{}", why.stdout, why.stderr);
    assert!(!told.contains("-v"), "{told}");
    // \[C-37\] The reason is the one the run decided: a skip in a run that failed.
    assert!(
        told.contains("p::TestNeeds runs: it was skipped in a run that failed, and a skip there proves nothing"),
        "{told}"
    );
}

/// What `hushrun why` says of a test its binary's early end left without a result.
const ENDED_EARLY: &str = "its package's test binary ended before Go's test main returned";

// covers: C-32, C-45
#[test]
fn a_skip_in_a_go_run_that_failed_records_nothing() {
    if !tools("Go", has_go()) {
        return;
    }
    // A skip says the body didn't run: in a run that failed that may be the failure's doing (here
    // TestNeeds skips when TestSetup failed), so the skipped test records no pass; in a run that
    // passed, a skip is a pass. Go prints `--- SKIP` with -v and -json.
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file("p/p.go", "package p\n"),
        file(
            "p/p_test.go",
            "package p\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nvar ready bool\n\nfunc TestSetup(t *testing.T) {\n\tif os.Getenv(\"FIXED\") == \"\" {\n\t\tt.Fatal(\"setup\")\n\t}\n\tready = true\n}\n\nfunc TestNeeds(t *testing.T) {\n\tif !ready {\n\t\tt.Skip(\"no setup\")\n\t}\n}\n\nfunc TestOk(t *testing.T) {}\n",
        ),
    ]);
    for flag in ["-v", "-json"] {
        let cache = r.root.join(format!("cache{flag}"));
        let cache_text = cache.display().to_string();
        let args = ["run", "unit", "--", "go", "test", flag, "./..."];
        let first = r.run(&args, &[("HUSHRUN_CACHE_DIR", &cache_text)]);
        assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
        assert_eq!(
            store::map(&cache)["units"]["p"]["tests"],
            serde_json::json!({"TestNeeds": "not-run", "TestOk": "passed", "TestSetup": "failed"}),
            "{flag}"
        );
        let second = r.run(&args, &[("HUSHRUN_CACHE_DIR", &cache_text), ("FIXED", "1")]);
        assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    }
}

// covers: C-45
#[test]
fn what_a_program_a_go_test_left_running_prints_reaches_go_test_and_no_test_of_it_passes() {
    if !tools("Go", has_go()) {
        return;
    }
    // The program holds the test binary's output for 2 s after the binary exited: longer than the
    // quarter second within which the runner tells output still held open from output closing,
    // and well inside the minute go test itself waits for it under its default timeout. (It held
    // it for 6 s when the runner waited 5 s and then passed the output on itself.)
    let r = Repo::new(&[
        file("hushrun.yaml", &yaml("{mode: observe, unit: test}")),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file("p/p.go", "package p\n"),
        file(
            "p/p_test.go",
            "package p\n\nimport (\n\t\"os\"\n\t\"os/exec\"\n\t\"testing\"\n)\n\nfunc TestStarts(t *testing.T) {\n\tcmd := exec.Command(\"sh\", \"-c\", \"sleep 2; echo late\")\n\tcmd.Stdout = os.Stdout\n\tif err := cmd.Start(); err != nil {\n\t\tt.Fatal(err)\n\t}\n}\n\nfunc TestOther(t *testing.T) {}\n",
        ),
    ]);
    let ran = r.run(&["run", "unit", "--", "go", "test", "-v", "./..."], &[]);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    // go test printed what the program printed, as it does without hushrun.
    assert!(
        ran.stdout.contains("late"),
        "{}\n{}",
        ran.stdout,
        ran.stderr
    );
    assert_eq!(
        r.map()["units"]["p"]["tests"],
        serde_json::json!({"TestOther": "not-run", "TestStarts": "not-run"})
    );
}

/// A package whose one test leaves a program holding its output for `hold` seconds, which then
/// prints `late`.
fn holding(hold: u32) -> Repo {
    Repo::new(&[
        file("hushrun.yaml", &yaml("{mode: observe, unit: test}")),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file("p/p.go", "package p\n"),
        file(
            "p/p_test.go",
            &format!(
                "package p\n\nimport (\n\t\"os\"\n\t\"os/exec\"\n\t\"testing\"\n)\n\nfunc TestStarts(t *testing.T) {{\n\tcmd := exec.Command(\"sh\", \"-c\", \"sleep {hold}; echo late\")\n\tcmd.Stdout = os.Stdout\n\tif err := cmd.Start(); err != nil {{\n\t\tt.Fatal(err)\n\t}}\n}}\n\nfunc TestOther(t *testing.T) {{}}\n"
            ),
        ),
    ])
}

// covers: C-45
#[test]
fn output_held_past_go_tests_own_wait_fails_the_package_as_it_does_without_hushrun() {
    use hushrun_testkit::env::Hermetic as _;
    if !tools("Go", has_go()) {
        return;
    }
    // With -timeout=20s go test waits 5 s for a binary's output after it exits; the program
    // holds it for 8 s. Without hushrun, go test fails the package for it.
    let r = holding(8);
    let words = ["go", "test", "-count=1", "-timeout=20s", "-v", "./..."];
    let plain = Command::new("go")
        // What the developer's shell exports never reaches a runner a test starts.
        .hermetic()
        .args(&words[1..])
        .current_dir(&r.repo)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .unwrap();
    let plain_text = format!(
        "{}{}",
        String::from_utf8_lossy(&plain.stdout),
        String::from_utf8_lossy(&plain.stderr)
    );
    assert_eq!(plain.status.code(), Some(1), "{plain_text}");
    assert!(plain_text.contains("Test I/O incomplete"), "{plain_text}");
    let mut args = vec!["run", "unit", "--"];
    args.extend(words);
    let ran = r.run(&args, &[]);
    let text = format!("{}{}", ran.stdout, ran.stderr);
    // Under hushrun go test sees what it sees without: the binary gone, its output still open.
    assert_eq!(ran.code, 1, "{text}");
    assert!(text.contains("Test I/O incomplete"), "{text}");
    assert!(!text.contains("it won't run again on this code"), "{text}");
    assert_eq!(
        r.map()["units"]["p"]["tests"],
        serde_json::json!({"TestOther": "not-run", "TestStarts": "not-run"})
    );
    // Held for 2 s, inside go test's wait: the package passes, with what the program printed.
    let r = holding(2);
    let ran = r.run(&args, &[]);
    let text = format!("{}{}", ran.stdout, ran.stderr);
    assert_eq!(ran.code, 0, "{text}");
    assert!(text.contains("late") && text.contains("ok "), "{text}");
}

// covers: C-45
#[test]
fn a_go_binary_that_ended_any_other_way_records_its_named_failures_and_no_pass() {
    if !tools("Go", has_go()) {
        return;
    }
    let r = Repo::new(&shapes());
    let none = states("not-run", "not-run", "not-run", "not-run", "not-run");
    let mid = states("not-run", "not-run", "not-run", "not-run", "failed");
    let cases: &[(&str, &[&str], &Value)] = &[
        // A panic: Go names the test that panicked, and stops.
        ("panic", &[], &mid),
        ("panic", &["-json"], &mid),
        // os.Exit(0) in a test is a panic under `go test`.
        ("exit0", &[], &mid),
        // os.Exit(1): no summary, nothing named.
        ("exit1", &[], &none),
        ("exit1", &["-v"], &none),
        // A timeout names the running test as running, not failed.
        ("hang", &["-timeout=2s"], &none),
        // -failfast leaves tests unstarted that nothing names.
        ("fail", &["-failfast"], &mid),
        // A test prints Go's failure and summary, then exits as a failed run would: Go's test
        // main never returned, so no test passed, whatever the output says.
        (
            "spoof-exit",
            &[],
            &states("not-run", "not-run", "failed", "not-run", "not-run"),
        ),
    ];
    for (n, (mode, flags, want)) in cases.iter().enumerate() {
        let (ran, got) = r.shape(n, mode, flags);
        assert_eq!(&got, *want, "MODE={mode} {flags:?}\n{}", ran.stderr);
        assert_ne!(ran.code, 0, "MODE={mode} {flags:?}\n{}", ran.stderr);
    }
}

// covers: C-45, C-39
#[test]
fn with_commands_own_run_or_skip_go_tests_are_reused_by_package_and_it_says_so() {
    if !tools("Go", has_go()) {
        return;
    }
    let r = Repo::new(&cart("{mode: reuse, unit: test}", ""));
    // Which tests ran is Go's matching of COMMAND's pattern: hushrun doesn't reproduce it, and
    // reuses by package, said once.
    for (n, (flag, word)) in [
        (&["-run", "TestFlip|TestOk"][..], "-run"),
        (&["-skip", "TestTwo"], "-skip"),
        (&["-args", "-test.run=TestFlip"], "-run"),
    ]
    .into_iter()
    .enumerate()
    {
        let cache = r.root.join(format!("cache-{n}"));
        let cache_text = cache.display().to_string();
        let mut args = vec!["run", "unit", "--", "go", "test", "./..."];
        args.extend(flag);
        let ran = r.run(&args, &[("FAIL", "1"), ("HUSHRUN_CACHE_DIR", &cache_text)]);
        assert_eq!(ran.code, 1, "{flag:?}\n{}", ran.stderr);
        assert!(
            ran.stderr.contains(&format!(
                "hushrun: unit: its tests are reused by package: go test {word}'s tests can't be reused one by one yet"
            )),
            "{flag:?}\n{}",
            ran.stderr
        );
        let map = store::map(&cache);
        assert!(map["units"]["cart"]["tests"].is_null(), "{flag:?}: {map:#}");
    }
    // A -run COMMAND doesn't show, in GOFLAGS: each binary's own arguments say it, and no test of
    // it is told one by one.
    let (ran, names) = r.go_tests(&[], &[("FAIL", "1"), ("GOFLAGS", "-run=^TestFlip$")]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    assert_eq!(names, ["TestFlip"]);
    assert!(r.tests_of("cart").is_null(), "{:#}", r.map());
    assert!(r.newest_result()["units"]["tests"].is_null());
}

// covers: C-45, C-35
#[test]
fn a_go_package_not_observed_is_so_for_every_test_and_its_test_main_runs_once() {
    if !tools("Go", has_go()) {
        return;
    }
    // A package whose test starts a program: its tests are told, and none is followed, since Go's
    // log is the package's.
    let mut files = cart(
        "{mode: reuse, unit: test}",
        "func TestProc(t *testing.T) {\n\tran(t)\n\tif err := exec.Command(\"go\", \"version\").Run(); err != nil {\n\t\tt.Fatal(err)\n\t}\n}\n",
    );
    files[3].1 = files[3].1.replace("\"os\"\n", "\"os\"\n\t\"os/exec\"\n");
    // A package with a TestMain, which writes to the log too: hushrun doesn't list its tests,
    // which would run it once more.
    files.push(file(
        "main/main_test.go",
        &format!(
            "package main_test\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\n{RAN}\n\
             type named string\n\nfunc (n named) Name() string {{ return string(n) }}\n\n\
             func TestMain(m *testing.M) {{\n\tif p := os.Getenv(\"RAN_LOG\"); p != \"\" {{\n\t\tf, _ := os.OpenFile(p, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)\n\t\tf.WriteString(\"TestMain\\n\")\n\t\tf.Close()\n\t}}\n\tos.Exit(m.Run())\n}}\n\n\
             func TestInMain(t *testing.T) {{ ran(t) }}\n"
        ),
    ));
    let r = Repo::new(&files);
    let (first, names) = r.go_tests(&[], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        names.iter().filter(|n| *n == "TestMain").count(),
        1,
        "{names:?}"
    );
    let map = r.map();
    assert_eq!(
        map["units"]["cart"]["notFollowed"],
        serde_json::json!(["subprocess"])
    );
    let states: BTreeSet<&str> = map["units"]["cart"]["tests"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(states, BTreeSet::from(["not-observed"]), "{map:#}");
    assert_eq!(
        map["units"]["main"]["notFollowed"],
        serde_json::json!(["test-main"])
    );
    assert!(map["units"]["main"]["tests"].is_null(), "{map:#}");
    // Nothing of either is reused.
    r.write("docs/notes.md", "notes\n");
    let (again, names) = r.go_tests(&[], &[]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(names.len(), 9, "{names:?}");
}

// covers: C-45, C-39, C-34
#[test]
fn an_audited_go_test_that_fails_again_alone_suspends_the_checks_reuse() {
    if !tools("Go", has_go()) {
        return;
    }
    // TestFlip and TestOk alone: with TestFlip failed, TestOk is the one test that could be
    // reused, and runs as a check.
    let mut files = cart("{mode: reuse, unit: test}", "");
    files[3].1 = files[3]
        .1
        .lines()
        .filter(|l| {
            !l.starts_with("func TestFlipToo")
                && !l.starts_with("func TestT")
                && !l.starts_with("func TestFour")
        })
        .collect::<Vec<_>>()
        .join("\n");
    // With -v: a package that failed keeps its other tests' passes only when Go names each one.
    let r = Repo::new(&files);
    assert_eq!(r.go_tests(&["-v"], &[("FAIL", "1")]).0.code, 1);
    let (caught, names) = r.go_tests(&["-v"], &[("HIDDEN", "1")]);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        line(&caught, "hushrun: unit: all"),
        "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs as a check"
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: cart::TestOk failed, though it passed before on the same files it read: running it once more"
        ),
        "{}",
        caught.stderr
    );
    // The second run ran TestOk alone: TestFlip was left out one by one.
    assert_eq!(names, ["TestFlip", "TestOk", "TestOk"], "{names:?}");
    assert!(
        caught.stderr.contains(
            "hushrun: unit: cart::TestOk failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(store::disagreements(&r.cache), ["cart::TestOk"]);
}

/// A module whose package `p` has a test file and a source file only the build tag `extra` takes
/// in, as testify's `dumpcgo_test.go` (`cgo && testcgo`) is; and a package `q`.
fn tagged() -> Vec<(String, String)> {
    vec![
        file(
            "hushrun.yaml",
            &yaml("{mode: reuse, unit: test, ignore: [\"docs/**\"]}"),
        ),
        file("go.mod", &format!("module {MODULE}\n\ngo 1.21\n")),
        file("p/p.go", "package p\n\nfunc One() int { return 1 }\n"),
        file(
            "p/p_test.go",
            "package p\n\nimport \"testing\"\n\nfunc TestA(t *testing.T) {\n\tif One() != 1 {\n\t\tt.Fatal(\"one\")\n\t}\n}\n",
        ),
        file(
            "p/tagged.go",
            "//go:build extra\n\npackage p\n\nfunc Two() int { return 2 }\n",
        ),
        file(
            "p/tagged_test.go",
            "//go:build extra\n\npackage p\n\nimport \"testing\"\n\nfunc TestTagged(t *testing.T) {\n\tif Two() != 2 {\n\t\tt.Fatal(\"two\")\n\t}\n}\n",
        ),
        file("q/q.go", "package q\n"),
        file(
            "q/q_test.go",
            "package q\n\nimport \"testing\"\n\nfunc TestQ(t *testing.T) {}\n",
        ),
    ]
}

// covers: C-37, C-45
#[test]
fn why_decides_from_the_files_go_list_names_under_commands_tags() {
    if !tools("Go", has_go()) {
        return;
    }
    for tags in [&[][..], &["-tags", "extra"][..]] {
        let r = Repo::new(&tagged());
        let mut command = vec!["run", "unit", "--", "go", "test"];
        command.extend(tags);
        command.push("./...");
        let first = r.run(&command, &[]);
        assert_eq!(
            first.code, 0,
            "{tags:?}: {}\n{}",
            first.stdout, first.stderr
        );
        // A file no rule reads: the check runs again, and its packages, whose files didn't
        // change, are reused, whatever the tags leave out of them.
        r.write("docs/notes.md", "notes\n");
        let why_test = r.run(&["why", "unit", "p::TestA"], &[]);
        let why_package = r.run(&["why", "unit", "p"], &[]);
        for (what, why) in [("p::TestA", &why_test), ("p", &why_package)] {
            assert_eq!(why.code, 0, "{tags:?} {what}: {}", why.stderr);
            assert!(
                why.stdout.starts_with(&format!("{what} would be reused: ")),
                "{tags:?} {what}: {}",
                why.stdout
            );
            assert!(!why.stdout.contains("is gone"), "{tags:?}: {}", why.stdout);
        }
        let again = r.run(&command, &[]);
        assert_eq!(again.code, 0, "{tags:?}: {}", again.stderr);
        assert!(
            again.stderr.contains("already passed on the files"),
            "{tags:?}: {}",
            again.stderr
        );
    }
}
