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
        changed.observe_line(),
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
    assert!(
        second.stderr.contains("4 test files reused"),
        "{}",
        second.stderr
    );
    // A reused package stays in the map, though its name is a directory: what it opened is the
    // next plan's hint.
    assert!(r.map()["units"]["greet"].is_object(), "{:#}", r.map());
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
