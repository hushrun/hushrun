//! `counts_from` end to end (C-25, ADR 0045): a rule joins a run's key, a run that doesn't meet it
//! records nothing under that key, and `hushrun ready` says the same without a server. A machine's
//! tools are scripts on a PATH that holds nothing else, so nothing here depends on what this
//! machine has installed.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::{git, schema};
use serde_json::{Value, json};

/// Variables a test must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_JOB",
    "GITHUB_REPOSITORY",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_WORKSPACE",
    "HUSHRUN_VARIANT",
    "HUSHRUN_WORKFLOW",
    "HUSHRUN_INPUTS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_LOG",
    "HUSHRUN_PROVIDER",
    "npm_lifecycle_event",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
];

/// What a test's PATH holds beside its tools.
const PROGRAMS: &[&str] = &[
    "git", "sh", "cat", "touch", "rm", "mkdir", "chmod", "sed", "head", "env",
];

const HEAD: &str = "version: 1\nrepository: github.com/acme/shop\n";

/// The workflow whose run `hushrun ready` knows: `hushrun run unit -- sh check.sh`.
const CI: &str = "on: push
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - run: hushrun run unit -- sh check.sh
";

const UNIT: &[&str] = &["run", "unit", "--", "sh", "check.sh"];

/// This machine's `os`.
fn here() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// The end of hushrun's line for a passed run on this machine, with what the rule names.
fn counted(tools: &str) -> String {
    format!(" on {}{tools}; it counts", here())
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
    fake: FakeResults,
}

impl Repo {
    /// A repository whose hushrun.yaml adds `rules`, with [`CI`]'s workflow and a check that
    /// exits with `VERDICT`.
    fn new(rules: &str) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real path");
        let repo = root.join("repo");
        let bin = root.join("bin");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::create_dir_all(&bin).expect("mkdir");
        git::init(&repo, "sha1");
        let path = std::env::var_os("PATH").expect("PATH");
        for program in PROGRAMS {
            let found = std::env::split_paths(&path)
                .map(|dir| dir.join(program))
                .find(|file| file.is_file())
                .unwrap_or_else(|| panic!("no {program}"));
            std::os::unix::fs::symlink(found, bin.join(program)).expect("link");
        }
        std::os::unix::fs::symlink(assert_cmd::cargo::cargo_bin("hushrun"), bin.join("hushrun"))
            .expect("link");
        let r = Repo {
            _dir: dir,
            root,
            repo,
            bin,
            fake: FakeResults::start(),
        };
        r.write(".gitignore", "node_modules/\n");
        r.write("hushrun.yaml", &format!("{HEAD}{rules}"));
        r.write(".github/workflows/ci.yml", CI);
        r.write("check.sh", "exit \"${VERDICT:-0}\"\n");
        r.write("src/lib.txt", "one\n");
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    /// A program on this machine, which notes each time it is asked.
    fn tool(&self, name: &str, body: &str) {
        script(&self.bin.join(name), &self.root.join("asked"), name, body);
    }

    fn go(&self, version: &str) {
        self.tool("go", &format!("echo go{version}\n"));
    }

    /// The tools asked so far.
    fn asked(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("asked"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn env(&self, cmd: &mut Command) {
        cmd.current_dir(&self.repo)
            .env("PATH", &self.bin)
            .env("HOME", &self.root)
            .env("HUSHRUN_BACKEND", self.fake.url())
            .env("HUSHRUN_TOKEN", FakeResults::TOKEN)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for var in CLEARED {
            cmd.env_remove(var);
        }
    }

    fn hushrun(&self, args: &[&str]) -> Output {
        self.hushrun_with(args, &[])
    }

    fn hushrun_with(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
        self.env(&mut cmd);
        cmd.envs(vars.iter().copied());
        cmd.args(args).output().expect("hushrun")
    }

    /// `line` in a shell here.
    fn sh(&self, line: &str) -> Output {
        let mut cmd = Command::new(self.bin.join("sh"));
        self.env(&mut cmd);
        cmd.args(["-c", line]).output().expect("sh")
    }

    /// `hushrun ready --format json`, checked against its schema.
    fn ready(&self, args: &[&str]) -> (i32, Value) {
        let output = self.hushrun(&[&["ready", "--format", "json"], args].concat());
        let value: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", err(&output)));
        schema::assert_valid(&schema::load("ready.v2.schema.json"), &value);
        (output.status.code().unwrap_or(-1), value)
    }

    /// The state of the only check.
    fn state(&self) -> (String, Value) {
        let (_, report) = self.ready(&[]);
        let check = &report["checks"][0];
        (
            check["state"].as_str().expect("a state").to_string(),
            check["why"].clone(),
        )
    }

    /// Every result recorded, checked against the schema.
    fn recorded(&self) -> Vec<Value> {
        let results = self.fake.recorded();
        let schema = schema::load("run-result.v0.schema.json");
        for result in &results {
            schema::assert_valid(&schema, result);
        }
        results
    }

    /// How many times the backend was asked to record anything.
    fn posts(&self) -> usize {
        let requests = self.fake.requests();
        requests.iter().filter(|seen| seen.method == "POST").count()
    }

    /// The key lookups have asked for, newest last.
    fn keys(&self) -> Vec<String> {
        (self.fake.requests().iter())
            .filter(|seen| seen.method == "GET")
            .filter_map(|seen| seen.path.strip_prefix("v0/results/").map(str::to_string))
            .collect()
    }
}

fn script(path: &Path, log: &Path, name: &str, body: &str) {
    let _ = std::fs::remove_file(path);
    let text = format!("#!/bin/sh\necho {name} >> {}\n{body}", log.display());
    std::fs::write(path, text).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn last_line(output: &Output) -> String {
    err(output).lines().last().unwrap_or_default().to_string()
}

fn go_rule() -> String {
    format!("counts_from:\n  os: {}\n  go: \"1.25\"\n", here())
}

// covers: C-25
#[test]
fn a_run_that_meets_the_rule_counts_and_the_next_skips_measuring_nothing() {
    let r = Repo::new(&go_rule());
    r.go("1.25.14");
    let first = r.hushrun(UNIT);
    assert_eq!(code(&first), 0, "{}", err(&first));
    let last = last_line(&first);
    assert!(last.starts_with("hushrun: unit passed in "), "{last}");
    assert!(last.ends_with(&counted(", go1.25.14")), "{last}");
    let recorded = r.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["result"], "pass");
    assert_eq!(
        recorded[0]["environment"]["tools"],
        json!({"go": "1.25.14"})
    );
    assert!(
        recorded[0]["environment"]["platform"]
            .as_str()
            .is_some_and(|platform| platform.contains('/')),
        "{}",
        recorded[0]
    );

    // A pass under the key skips on any machine, and skipping asks nothing.
    r.go("1.24.3");
    let before = r.asked().len();
    let second = r.hushrun(UNIT);
    assert_eq!(code(&second), 0, "{}", err(&second));
    assert!(err(&second).contains("unit skipped: this exact code passed"));
    assert_eq!(r.asked().len(), before, "skipping measured something");
    let skip = r.recorded().pop().expect("the skip");
    assert_eq!(skip["result"], "skip");
    assert!(skip["environment"]["platform"].is_string(), "{skip}");
    assert!(skip["environment"].get("tools").is_none(), "{skip}");
}

// covers: C-25
#[test]
fn a_run_that_doesnt_meet_it_records_nothing_and_keeps_its_exit_code() {
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    let out = r.hushrun(UNIT);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        err(&out).contains(
            "hushrun: unit running: this run won't count: needs go 1.25, this is go1.24.3\n"
        ),
        "{}",
        err(&out)
    );
    let last = last_line(&out);
    assert!(last.starts_with("hushrun: unit passed in "), "{last}");
    assert!(
        last.ends_with("; it won't count: needs go 1.25, this is go1.24.3"),
        "{last}"
    );
    assert_eq!(r.posts(), 0, "it cost a record");

    // So it never skips, and the command's own exit code comes back.
    let failed = r.hushrun_with(UNIT, &[("VERDICT", "7")]);
    assert_eq!(code(&failed), 7, "{}", err(&failed));
    assert!(
        last_line(&failed).ends_with("(exit 7); it won't count: needs go 1.25, this is go1.24.3"),
        "{}",
        err(&failed)
    );
    assert_eq!(r.posts(), 0, "it cost a record");
    let again = r.hushrun(UNIT);
    assert!(!err(&again).contains("skipped"), "{}", err(&again));
    assert_eq!(r.posts(), 0);
}

// covers: C-25
#[test]
fn a_failure_on_a_machine_that_meets_it_is_recorded_as_any_other() {
    let r = Repo::new(&go_rule());
    r.go("1.25.14");
    let failed = r.hushrun_with(UNIT, &[("VERDICT", "3")]);
    assert_eq!(code(&failed), 3);
    assert!(last_line(&failed).ends_with("(exit 3)"), "{}", err(&failed));
    let recorded = r.recorded();
    assert_eq!(recorded[0]["result"], "fail");
    assert_eq!(recorded[0]["environment"]["tools"]["go"], "1.25.14");
}

/// A command line before its `--`, and its variables.
type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);

// covers: C-25
#[test]
fn no_way_of_naming_the_code_or_the_config_gets_round_a_rule() {
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    std::fs::write(r.root.join("copy.yaml"), format!("{HEAD}{}", go_rule())).expect("write");
    let copy = r.root.join("copy.yaml");
    let copy = copy.to_str().expect("utf-8");
    let digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    let tail: &[&str] = &["--", "sh", "check.sh"];
    let cases: &[Case<'_>] = &[
        (&["run", "unit", "--rev", "HEAD"], &[]),
        (&["run", "unit", "--digest", digest], &[]),
        (&["--config", copy, "run", "unit"], &[]),
        (&["--config-ref", "HEAD", "run", "unit"], &[]),
        (&["run", "unit"], &[("HUSHRUN_CONFIG", copy)]),
        (&["run", "unit", "--force"], &[]),
    ];
    for (args, vars) in cases {
        let out = r.hushrun_with(&[args, tail].concat(), vars);
        assert_eq!(code(&out), 0, "{args:?}: {}", err(&out));
        assert!(
            last_line(&out).ends_with("; it won't count: needs go 1.25, this is go1.24.3"),
            "{args:?}: {}",
            err(&out)
        );
        assert_eq!(r.posts(), 0, "{args:?} recorded a pass");
    }

    // And each one that does meet it records under the very key a plain run looks up.
    r.go("1.25.14");
    for (args, vars) in cases.iter().filter(|(args, _)| !args.contains(&digest)) {
        let posts = r.posts();
        let out = r.hushrun_with(&[args, tail].concat(), vars);
        assert_eq!(code(&out), 0, "{args:?}: {}", err(&out));
        let said = err(&out);
        assert!(
            said.contains("; it counts") || said.contains("skipped"),
            "{args:?}: {said}"
        );
        assert!(r.posts() > posts, "{args:?}");
    }
    let out = r.hushrun(UNIT);
    assert!(err(&out).contains("skipped"), "{}", err(&out));
}

// covers: C-25
#[test]
fn a_config_with_no_rule_records_under_another_key() {
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    std::fs::write(r.root.join("none.yaml"), HEAD).expect("write");
    let none = r.root.join("none.yaml");
    let none = none.to_str().expect("utf-8");
    let out = r.hushrun(&[&["--config", none], UNIT].concat());
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert_eq!(r.recorded().len(), 1, "no rule, so it records");
    // Which the repository's own rule never finds: a key holds its rule.
    let out = r.hushrun(UNIT);
    assert!(!err(&out).contains("skipped"), "{}", err(&out));
    let keys = r.keys();
    assert_ne!(keys[0], keys[1]);
}

// covers: C-25
#[test]
fn the_rule_at_the_revision_decides_at_that_revision() {
    let r = Repo::new("");
    r.go("1.24.3");
    let plain = git::run(&r.repo, &["rev-parse", "HEAD"], None);
    let plain = String::from_utf8_lossy(&plain).trim().to_string();
    r.write("hushrun.yaml", &format!("{HEAD}{}", go_rule()));
    git::commit_all(&r.repo, "a rule");
    // HEAD has the rule: this machine doesn't meet it.
    let out = r.hushrun(&["run", "unit", "--rev", "HEAD", "--", "sh", "check.sh"]);
    assert!(err(&out).contains("it won't count"), "{}", err(&out));
    assert_eq!(r.posts(), 0);
    // The commit before has none, so it records, under a key with no rule, which HEAD can't find.
    let out = r.hushrun(&["run", "unit", "--rev", &plain, "--", "sh", "check.sh"]);
    assert!(err(&out).contains("won't run again"), "{}", err(&out));
    assert_eq!(r.posts(), 1);
    let out = r.hushrun(&["run", "unit", "--rev", "HEAD", "--", "sh", "check.sh"]);
    assert!(err(&out).contains("it won't count"), "{}", err(&out));
    assert_eq!(r.posts(), 1);
}

// covers: C-25
#[test]
fn what_is_measured_after_the_command_decides() {
    // A toolchain that arrives during the run counts, whatever was said first.
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    let arrives = r.bin.join("arrives");
    script(
        &arrives,
        &r.root.join("asked"),
        "arrives",
        &format!(
            "cat > {go} <<'EOF'\n#!/bin/sh\necho go1.25.2\nEOF\n",
            go = r.bin.join("go").display()
        ),
    );
    let run = &["run", "unit", "--", "arrives"];
    let out = r.hushrun(run);
    assert!(err(&out).contains("this run won't count"), "{}", err(&out));
    assert!(
        last_line(&out).ends_with(&counted(", go1.25.2")),
        "{}",
        err(&out)
    );
    assert_eq!(r.recorded()[0]["environment"]["tools"]["go"], "1.25.2");

    // One that leaves doesn't.
    let r = Repo::new(&go_rule());
    r.go("1.25.2");
    script(
        &r.bin.join("leaves"),
        &r.root.join("asked"),
        "leaves",
        &format!("rm {}\n", r.bin.join("go").display()),
    );
    let out = r.hushrun(&["run", "unit", "--", "leaves"]);
    assert_eq!(code(&out), 0);
    assert!(!err(&out).contains("running: this run won't count"));
    assert!(
        last_line(&out).ends_with("it won't count: needs go 1.25, and go isn't here"),
        "{}",
        err(&out)
    );
    assert_eq!(r.posts(), 0);
}

// covers: C-25
#[test]
fn a_tool_that_reads_stdin_finds_it_closed() {
    let r = Repo::new(&go_rule());
    r.tool("go", "read -r line\necho go1.25.14\n");
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
    r.env(&mut cmd);
    // Held open, a tool that reads it would wait out its 10 s, and not count.
    let mut child = cmd
        .args(UNIT)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("hushrun");
    let held = child.stdin.take();
    let out = child.wait_with_output().expect("hushrun");
    drop(held);
    assert!(err(&out).ends_with("; it counts\n"), "{}", err(&out));
}

// covers: C-25
#[test]
fn a_tool_is_asked_where_the_command_runs_with_the_path_it_gets() {
    let rule = "counts_from: {node: \"24\"}\n";
    let r = Repo::new(rule);
    // The machine's node isn't one the rule takes.
    r.tool("node", "echo v20.1.0\n");
    // A package manager's PATH holds the project's own, and a shim answers by directory.
    let dotbin = r.repo.join("web/node_modules/.bin");
    std::fs::create_dir_all(&dotbin).expect("mkdir");
    script(
        &dotbin.join("node"),
        &r.root.join("asked"),
        "node",
        "echo v$(cat .version)\n",
    );
    r.write("web/.version", "24.3.0\n");
    r.write("web/check.sh", "exit 0\n");
    git::commit_all(&r.repo, "web");

    let path = format!("{}:{}", dotbin.display(), r.bin.display());
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
    r.env(&mut cmd);
    let out = cmd
        .current_dir(r.repo.join("web"))
        .env("PATH", path)
        .env("npm_lifecycle_event", "test")
        .args(["run", "--", "sh", "check.sh"])
        .output()
        .expect("hushrun");
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        last_line(&out).ends_with(&counted(", node 24.3.0")),
        "{}",
        err(&out)
    );

    // Without that PATH, a command starts the machine's, so this doesn't count.
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
    r.env(&mut cmd);
    let out = cmd
        .current_dir(r.repo.join("web"))
        .args(["run", "other", "--", "sh", "check.sh"])
        .output()
        .expect("hushrun");
    assert!(
        last_line(&out).ends_with("needs node 24, this is node 20.1.0"),
        "{}",
        err(&out)
    );
}

// covers: C-25
#[test]
fn a_node_npm_would_run_has_to_meet_the_rule_too() {
    // `hushrun run x -- npm test` runs on the package's node, whatever the machine's is.
    let r = Repo::new("counts_from: {node: \"20\"}\n");
    r.tool("node", "echo v20.1.0\n");
    r.tool("npm", "exit 0\n");
    let dotbin = r.repo.join("web/node_modules/.bin");
    std::fs::create_dir_all(&dotbin).expect("mkdir");
    script(
        &dotbin.join("node"),
        &r.root.join("asked"),
        "node",
        "echo v18.0.0\n",
    );
    r.write("web/a.txt", "a\n");
    git::commit_all(&r.repo, "web");
    let in_web = |args: &[&str]| {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
        r.env(&mut cmd);
        cmd.current_dir(r.repo.join("web"))
            .args(args)
            .output()
            .expect("hushrun")
    };
    let out = in_web(&["run", "unit", "--", "npm", "test"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        last_line(&out).ends_with("needs node 20, this is node 18.0.0"),
        "{}",
        err(&out)
    );
    assert_eq!(r.posts(), 0);
    // And at the root, above that install, it's the machine's alone.
    let out = r.hushrun(&["run", "unit", "--", "npm", "test"]);
    assert!(err(&out).ends_with("; it counts\n"), "{}", err(&out));
}

// covers: C-25
#[test]
fn a_command_that_picks_its_own_toolchain_doesnt_count() {
    let r = Repo::new("counts_from: {rust: \"1.94\"}\n");
    r.tool("rustc", "echo 'rustc 1.94.0 (4a4ef493e 2026-03-02)'\n");
    r.tool("cargo", "exit 0\n");
    let out = r.hushrun(&["run", "test", "--", "cargo", "+nightly", "test"]);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        err(&out).contains(
            "running: this run won't count: needs rust 1.94, and cargo +nightly picks its own toolchain"
        ),
        "{}",
        err(&out)
    );
    assert!(
        last_line(&out).ends_with(
            "it won't count: needs rust 1.94, and cargo +nightly picks its own toolchain"
        ),
        "{}",
        err(&out)
    );
    assert_eq!(r.posts(), 0);
    // Under a rule that doesn't name the tool, it's any other command.
    let r = Repo::new(&format!("counts_from: {{os: {}}}\n", here()));
    r.tool("cargo", "exit 0\n");
    let out = r.hushrun(&["run", "test", "--", "cargo", "+nightly", "test"]);
    assert!(err(&out).ends_with("; it counts\n"), "{}", err(&out));
}

// covers: C-25
#[test]
fn an_old_toolchain_by_path_or_by_variable_doesnt_count_as_the_new_one() {
    let r = Repo::new("counts_from: {go: \"1.25\"}\n");
    r.go("1.25.14");
    r.write("old/bin/go", "#!/bin/sh\nexit 0\n");
    std::fs::set_permissions(
        r.repo.join("old/bin/go"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod");
    git::commit_all(&r.repo, "an old go");
    r.tool("timeout", "shift\nexec \"$@\"\n");
    let old = r.repo.join("old/bin");
    let old = old.to_str().expect("utf-8");
    let go = format!("{old}/go");
    let path = format!("PATH={old}:{}", r.bin.display());
    let cases: [(&str, &[&str]); 5] = [
        ("by-path", &[go.as_str(), "test"]),
        ("in-env", &["env", path.as_str(), "go", "test"]),
        ("in-sh", &["sh", "-c", "PATH=old/bin:$PATH go test"]),
        (
            "chained",
            &["sh", "-c", "cd src && GOTOOLCHAIN=go1.20.0 go test"],
        ),
        (
            "timed",
            &["timeout", "9", "env", "GOROOT=/old", "go", "test"],
        ),
    ];
    for (name, command) in cases {
        let out = r.hushrun(&[&["run", name, "--"], command].concat());
        assert!(
            last_line(&out).contains("picks its own toolchain"),
            "{name}: {}",
            err(&out)
        );
        assert_eq!(r.posts(), 0, "{name} recorded");
    }
}

// covers: C-25
#[test]
fn a_variable_holds_exactly_what_the_rule_says_and_hushrun_never_repeats_it() {
    let r = Repo::new("counts_from: {env: {SANDBOX: [go-7, go-8]}}\n");
    let out = r.hushrun(UNIT);
    assert!(
        last_line(&out).ends_with("needs SANDBOX go-7 or go-8, and it isn't set"),
        "{}",
        err(&out)
    );
    let out = r.hushrun_with(UNIT, &[("SANDBOX", "a-secret-value")]);
    assert!(
        last_line(&out).ends_with("needs SANDBOX go-7 or go-8, and it holds something else"),
        "{}",
        err(&out)
    );
    assert!(!err(&out).contains("a-secret-value"));
    assert_eq!(r.posts(), 0);
    let out = r.hushrun_with(UNIT, &[("SANDBOX", "go-8")]);
    assert!(err(&out).ends_with("; it counts\n"), "{}", err(&out));
    assert_eq!(r.recorded().len(), 1);
    assert!(!r.recorded()[0].to_string().contains("go-8"));
}

// covers: C-17, C-25 (ADR 0046: a rule's variable is read as hushrun reads its own, so a string dropped
// from one isn't there, and hushrun repeats nothing of it)
#[test]
fn a_rule_reads_no_string_from_a_variable_it_was_dropped_from() {
    let r = Repo::new("counts_from: {env: {HUSHRUN_RUN_URL: x}}\n");
    let key = hushrun_attest::SigningKey::generate_p256().unwrap();
    let string = r.fake.agent_string(&key);
    let out = r.hushrun_with(UNIT, &[("HUSHRUN_RUN_URL", string.as_str())]);
    assert!(
        last_line(&out).ends_with("needs HUSHRUN_RUN_URL x, and it isn't set"),
        "{}",
        err(&out)
    );
    assert!(!err(&out).contains(&string), "{}", err(&out));
}

// covers: C-17, C-25 (ADR 0046: a rule can't compare against the string or what's inside it, and a
// rule that holds one prints nothing: the file is refused, and the check runs and records nothing)
#[test]
fn a_rule_neither_names_nor_holds_a_string() {
    let r = Repo::new("");
    let key = hushrun_attest::SigningKey::generate_p256().unwrap();
    let string = r.fake.agent_string(&key);
    let body = &string["hushrun_agent_v1_".len()..];
    for (rule, says) in [
        ("{HUSHRUN_AGENT: x}".to_string(), "can't name HUSHRUN_AGENT"),
        ("{HUSHRUN_TOKEN: x}".to_string(), "can't name HUSHRUN_TOKEN"),
        (
            "{HUSHRUN_MACHINE_KEY: x}".to_string(),
            "can't name HUSHRUN_MACHINE_KEY",
        ),
        (
            format!("{{SANDBOX: [\"{string}\"]}}"),
            "holds a HUSHRUN_AGENT string",
        ),
        (
            format!("{{SANDBOX: [\"{body}\"]}}"),
            "holds a HUSHRUN_AGENT string",
        ),
    ] {
        r.write(
            "hushrun.yaml",
            &format!("{HEAD}counts_from: {{env: {rule}}}\n"),
        );
        git::commit_all(&r.repo, "a rule");
        let posts = r.posts();
        for vars in [&[][..], &[("HUSHRUN_AGENT", string.as_str())]] {
            let ready = r.hushrun_with(&["ready"], vars);
            assert_eq!(code(&ready), 2, "{rule}: {}", err(&ready));
            assert!(err(&ready).contains(says), "{rule}: {}", err(&ready));
            let run = r.hushrun_with(UNIT, vars);
            assert_eq!(code(&run), 0, "{rule}: {}", err(&run));
            for out in [&ready, &run] {
                let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), err(out));
                assert!(!text.contains(body), "{rule}: {text}");
            }
        }
        assert_eq!(r.posts(), posts, "{rule}");
    }
}

// covers: C-9, C-25
#[test]
fn a_run_with_no_rule_measures_nothing_and_says_what_it_said() {
    for rules in ["", "counts_from: anywhere\n"] {
        let r = Repo::new(rules);
        r.go("1.25.14");
        r.tool("rustc", "echo 'rustc 1.94.0'\n");
        r.tool("node", "echo v24.0.0\n");
        r.tool("xcodebuild", "echo 'Xcode 26.0'\n");
        let first = r.hushrun(UNIT);
        let text = err(&first);
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| !line.contains("the pre-push hook"))
            .collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(
            lines[0],
            "hushrun: unit running: no pass yet for this exact code"
        );
        assert!(lines[1].starts_with("hushrun: unit passed in "), "{text}");
        assert!(
            lines[1].ends_with("; it won't run again on this code"),
            "{text}"
        );
        let failed = r.hushrun(&["run", "other", "--", "sh", "-c", "exit 4"]);
        assert_eq!(code(&failed), 4);
        assert!(last_line(&failed).ends_with("(exit 4)"), "{}", err(&failed));
        assert!(r.asked().is_empty(), "measured {:?}", r.asked());
        let result = &r.recorded()[0];
        assert!(result["environment"]["platform"].is_string(), "{result}");
        assert!(result["environment"].get("tools").is_none(), "{result}");
    }
    // `anywhere` and no rule are one key, and a rule is another. Here hushrun.yaml is out of the
    // scope, so that only the rule tells them apart.
    let keys_with = |rules: &str| {
        let r = Repo::new(&format!(
            "scopes:\n  default:\n    include: [\"src/**\"]\n{rules}"
        ));
        r.go("1.25.14");
        r.hushrun(UNIT);
        r.keys()
    };
    let none = keys_with("");
    assert_eq!(none, keys_with("    counts_from: anywhere\n"));
    assert_ne!(none, keys_with("    counts_from: {go: \"1.25\"}\n"));
    assert_eq!(
        keys_with("    counts_from: {os: [linux, macos]}\n"),
        keys_with("    counts_from: {os: [macos, linux]}\n")
    );
}

// covers: C-25, C-14
#[test]
fn a_rule_changes_nothing_of_what_stops_a_skip_in_a_job_with_variants() {
    let r = Repo::new(&go_rule());
    r.go("1.25.14");
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        go: [\"1.24\", \"1.25\"]\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh check.sh\n",
    );
    git::commit_all(&r.repo, "a matrix");
    let vars: &[(&str, &str)] = &[
        ("CI", "true"),
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REPOSITORY", "acme/shop"),
        (
            "GITHUB_WORKFLOW_REF",
            "acme/shop/.github/workflows/ci.yml@refs/heads/main",
        ),
        ("GITHUB_JOB", "test"),
    ];
    // A pass under the key doesn't skip here, and a run that meets the rule records.
    let local = r.hushrun(UNIT);
    assert!(err(&local).contains("; it counts"), "{}", err(&local));
    let ci = r.hushrun_with(UNIT, vars);
    assert!(err(&ci).contains("no pass skips it"), "{}", err(&ci));
    assert!(err(&ci).ends_with("; it counts\n"), "{}", err(&ci));
    assert_eq!(
        r.recorded()[1]["job"]
            .as_str()
            .map(|job| job.starts_with("test@")),
        Some(true)
    );
    // And one that doesn't records nothing there either.
    r.go("1.24.3");
    let posts = r.posts();
    let ci = r.hushrun_with(UNIT, vars);
    assert!(err(&ci).contains("no pass skips it"), "{}", err(&ci));
    assert!(last_line(&ci).ends_with("this is go1.24.3"), "{}", err(&ci));
    assert_eq!(r.posts(), posts);
}

// covers: C-25, C-10
#[test]
fn a_rule_changes_nothing_of_whose_ci_pass_counts() {
    let r = Repo::new(&go_rule());
    r.go("1.25.14");
    r.write(
        ".github/workflows/ci.yml",
        &format!("{CI}  other:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh check.sh\n"),
    );
    git::commit_all(&r.repo, "two jobs");
    let vars = |job: &'static str| {
        [
            ("CI", "true"),
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", "acme/shop"),
            (
                "GITHUB_WORKFLOW_REF",
                "acme/shop/.github/workflows/ci.yml@refs/heads/main",
            ),
            ("GITHUB_JOB", job),
        ]
    };
    let out = r.hushrun_with(UNIT, &vars("test"));
    assert!(err(&out).contains("; it counts"), "{}", err(&out));
    // Another job's pass under the very key still doesn't skip.
    let out = r.hushrun_with(UNIT, &vars("other"));
    assert!(err(&out).contains("in another CI job"), "{}", err(&out));
    assert!(err(&out).contains("; it counts"), "{}", err(&out));
}

// covers: C-22, C-25
#[test]
fn ready_says_who_meets_it_from_what_needs_nothing_started() {
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    // A tool's version is the run's to judge, on any machine.
    let (code_of, report) = r.ready(&[]);
    assert_eq!(code_of, 10, "{report:#}");
    let check = &report["checks"][0];
    assert_eq!(check["state"], "missing", "{report:#}");
    assert_eq!(check["why"], "no pass yet for this exact code");
    assert_eq!(check["run"], "hushrun run unit -- sh check.sh");
    assert!(r.asked().is_empty(), "{:?}", r.asked());

    // The system and a variable need nothing started: `ci`, with why, and nothing to do here.
    let r = Repo::new("counts_from: {os: windows, env: {SANDBOX: go-7}}\n");
    let (code_of, report) = r.ready(&[]);
    assert_eq!(code_of, 0, "{report:#}");
    let check = &report["checks"][0];
    assert_eq!(check["state"], "ci", "{report:#}");
    assert_eq!(
        check["why"],
        format!(
            "needs windows, this is {}; needs SANDBOX go-7, and it isn't set",
            here()
        )
    );
    assert_eq!(check["run"], Value::Null);
    assert_eq!(report["next"], json!([]));
    let text = String::from_utf8_lossy(&r.hushrun(&["ready"]).stdout).into_owned();
    assert!(text.contains("needs SANDBOX go-7"), "{text}");

    // A pass from a machine that met it is `passed`, wherever this is.
    let r = Repo::new(&go_rule());
    r.go("1.25.14");
    let out = r.hushrun(UNIT);
    assert!(err(&out).ends_with("; it counts\n"), "{}", err(&out));
    r.go("1.24.3");
    let before = r.asked().len();
    assert_eq!(r.state().0, "passed");
    assert_eq!(r.asked().len(), before, "ready started a tool");
}

// covers: C-25
#[test]
fn the_monorepo_on_the_docs_page_works_as_it_is_written() {
    // The two files of the docs page hushrun.com/docs/where-a-check-counts.
    let r = Repo::new("");
    r.write(
        "hushrun.yaml",
        r#"version: 1
repository: github.com/acme/shop
backend: https://api.hushrun.com/o/acme
scopes:
  ios:
    include: ["ios/**"]
    counts_from: {os: macos}
  api:
    include: ["api/**"]
    counts_from: {os: linux, go: "1.25"}
checks:
  swiftlint:
    counts_from: anywhere
"#,
    );
    r.write(
        ".github/workflows/ci.yml",
        "on: push
jobs:
  ios:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v5
      - run: hushrun run --scope ios test -- xcodebuild test -scheme App
        working-directory: ios
      - run: hushrun run --scope ios swiftlint -- swiftlint --strict
        working-directory: ios
  api:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - run: hushrun run --scope api test -- go test ./...
        working-directory: api
",
    );
    r.write("ios/App.swift", "// app\n");
    r.write("api/main.go", "package main\n");
    r.tool("xcodebuild", "exit 0\n");
    r.tool("swiftlint", "exit 0\n");
    r.go("1.25.14");
    git::commit_all(&r.repo, "a monorepo");
    let (_, report) = r.ready(&[]);
    assert_eq!(report["notes"], json!([]), "{report:#}");
    let of = |report: &Value, check: &str| -> String {
        let checks = report["checks"].as_array().expect("checks");
        let found = checks.iter().find(|item| item["check"] == check);
        found.unwrap_or_else(|| panic!("{check}: {report:#}"))["state"]
            .as_str()
            .expect("a state")
            .to_string()
    };
    // `hushrun ready` reads the rule of the scope each line names. The job's system comes first.
    let ios = if cfg!(target_os = "macos") {
        "missing"
    } else {
        "ci"
    };
    let api = if cfg!(target_os = "macos") {
        "ci"
    } else {
        "missing"
    };
    assert_eq!(of(&report, "ios/test"), ios, "{report:#}");
    assert_eq!(of(&report, "ios/swiftlint"), ios, "{report:#}");
    assert_eq!(of(&report, "api/test"), api, "{report:#}");
    // Run as the workflow runs them, `swiftlint` counts whatever the system, under its own rule.
    let out = r.sh("cd ios && hushrun run --scope ios swiftlint -- swiftlint --strict");
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(err(&out).contains("won't run again"), "{}", err(&out));
    let (_, report) = r.ready(&[]);
    assert_eq!(of(&report, "ios/swiftlint"), "passed", "{report:#}");
    let out = r.sh("cd api && hushrun run --scope api test -- go test ./...");
    assert_eq!(code(&out), 0, "{}", err(&out));
    let linux = !cfg!(target_os = "macos");
    assert_eq!(err(&out).contains("; it counts"), linux, "{}", err(&out));
}

// covers: C-22, C-25
#[test]
fn ready_finds_the_key_a_run_records_under_at_every_level() {
    let r = Repo::new(&format!(
        "scopes:\n  web:\n    include: [\"web/**\"]\n    counts_from: {{os: {os}}}\n  api:\n    include: [\"api/**\"]\n    counts_from: {{os: windows}}\nchecks:\n  docs:\n    counts_from: anywhere\n  e2e:\n    counts_from: {{env: {{READY: \"1\"}}}}\n",
        os = here()
    ));
    r.write("web/a.txt", "a\n");
    r.write("api/a.txt", "a\n");
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope web unit -- sh check.sh\n      - run: hushrun run --scope api unit -- sh check.sh\n      - run: hushrun run --scope api docs -- sh check.sh\n      - run: hushrun run --scope web e2e -- sh check.sh\n",
    );
    git::commit_all(&r.repo, "scopes");
    let of = |report: &Value, check: &str| -> (String, String) {
        let found = report["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .find(|item| item["check"] == check)
            .unwrap_or_else(|| panic!("{check}: {report:#}"));
        (
            found["state"].as_str().expect("state").to_string(),
            found["why"].as_str().unwrap_or_default().to_string(),
        )
    };
    let (_, report) = r.ready(&[]);
    assert_eq!(of(&report, "web/unit").0, "missing", "{report:#}");
    assert_eq!(
        of(&report, "api/unit"),
        (
            "ci".to_string(),
            format!("needs windows, this is {}", here())
        )
    );
    // A check's `anywhere` wins over its scope's rule.
    assert_eq!(of(&report, "api/docs").0, "missing", "{report:#}");
    assert_eq!(
        of(&report, "web/e2e"),
        (
            "ci".to_string(),
            "needs READY 1, and it isn't set".to_string()
        )
    );
    // What it says to run records under what it looks up.
    for command in report["next"].as_array().expect("next") {
        let out = r.sh(command.as_str().expect("a command"));
        assert_eq!(code(&out), 0, "{}", err(&out));
    }
    let (_, report) = r.ready(&[]);
    assert_eq!(of(&report, "web/unit").0, "passed", "{report:#}");
    assert_eq!(of(&report, "api/docs").0, "passed", "{report:#}");
}

// covers: C-22, C-25
#[test]
fn a_run_that_doesnt_count_says_why_in_ready_pass_or_fail() {
    // A machine with no Xcode: the tool fails to answer, and the check fails, since it is the tool.
    let r = Repo::new("counts_from: {xcode: \"16\"}\n");
    r.tool(
        "xcodebuild",
        "echo 'xcode-select: error: tool xcodebuild requires Xcode' >&2\nexit 1\n",
    );
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- xcodebuild test\n",
    );
    git::commit_all(&r.repo, "xcode");
    let (_, report) = r.ready(&[]);
    assert_eq!(report["checks"][0]["state"], "missing", "{report:#}");
    assert_eq!(
        report["next"],
        json!(["hushrun run unit -- xcodebuild test"])
    );
    // Nothing is said early, and the run says it doesn't count.
    let out = r.sh("hushrun run unit -- xcodebuild test");
    assert_eq!(code(&out), 1, "{}", err(&out));
    assert!(
        !err(&out).contains("running: this run won't count"),
        "{}",
        err(&out)
    );
    let why =
        "needs xcode 16, and hushrun can't tell which xcode this is: xcodebuild exit status: 1";
    assert!(
        last_line(&out).ends_with(&format!("(exit 1); it won't count: {why}")),
        "{}",
        err(&out)
    );
    assert_eq!(r.posts(), 0);
    // So it stops asking, says what the run said, and a push goes.
    let (state, said) = r.state();
    assert_eq!(state, "ci");
    assert_eq!(said, format!("a run here didn't count: {why}"));
}

// covers: C-22, C-25
#[test]
fn a_rule_not_met_is_learned_from_one_run_and_read_without_starting_anything() {
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    assert_eq!(r.state().0, "missing");
    let out = r.hushrun(UNIT);
    assert!(err(&out).contains("this run won't count"), "{}", err(&out));
    let before = r.asked().len();
    let (state, why) = r.state();
    assert_eq!(state, "ci");
    assert_eq!(
        why,
        "a run here didn't count: needs go 1.25, this is go1.24.3"
    );
    assert_eq!(r.asked().len(), before, "ready started a tool");
    // A failure leaves the same.
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    let out = r.hushrun_with(UNIT, &[("VERDICT", "1")]);
    assert_eq!(code(&out), 1, "{}", err(&out));
    assert_eq!(r.state().0, "ci");
}

// covers: C-22, C-25
#[test]
fn what_ready_says_to_run_ends_when_a_script_sees_another_tool() {
    // `ready` never sees what a script does to its PATH: the run does, and says so.
    let r = Repo::new("counts_from: {go: \"1.25\"}\n");
    r.go("1.25.14");
    r.write("tools/go", "#!/bin/sh\necho go1.24.3\n");
    std::fs::set_permissions(
        r.repo.join("tools/go"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod");
    r.write(
        "package.json",
        "{\"scripts\": {\"test\": \"export PATH=$PWD/tools:$PATH && hushrun run unit -- sh check.sh\"}}\n",
    );
    r.write(".github/workflows/ci.yml", "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n");
    r.tool(
        "npm",
        "[ \"$1\" = run ] || exit 1\nexport npm_lifecycle_event=\"$2\"\nexport PATH=$PWD/tools:$PATH\nexec hushrun run unit -- sh check.sh\n",
    );
    git::commit_all(&r.repo, "a script");
    let (_, report) = r.ready(&[]);
    let line = report["next"][0].as_str().expect("a command");
    assert_eq!(line, "npm run test");
    let out = r.sh(line);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        last_line(&out).ends_with("this is go1.24.3"),
        "{}",
        err(&out)
    );
    assert_eq!(r.posts(), 0);
    let (state, why) = r.state();
    assert_eq!(state, "ci", "it would ask for ever: {why}");
}

// covers: C-22, C-25
#[test]
fn a_result_from_this_machine_clears_its_mark() {
    // A pass nothing could measure leaves the mark; a real failure after it is what stands, and
    // this machine can clear that.
    let r = Repo::new("counts_from: {go: \"1.25\"}\n");
    r.tool("go", "echo nonsense\n");
    let out = r.hushrun(UNIT);
    assert!(err(&out).contains("it won't count"), "{}", err(&out));
    assert_eq!(r.state().0, "ci");
    r.go("1.25.14");
    let out = r.hushrun_with(UNIT, &[("VERDICT", "1")]);
    assert_eq!(code(&out), 1);
    let (_, report) = r.ready(&[]);
    let check = &report["checks"][0];
    assert_eq!(check["state"], "missing", "{report:#}");
    assert_eq!(check["why"], "it failed on this exact code last time");
    assert_eq!(check["run"], "hushrun run unit -- sh check.sh");
}

/// What `ready` says of the only check, when a failure stands that a run here can't clear.
fn stuck(r: &Repo, reason: &str) {
    let (code_of, report) = r.ready(&[]);
    assert_eq!(code_of, 10, "{report:#}");
    assert_eq!(report["ready"], false, "{report:#}");
    let check = &report["checks"][0];
    assert_eq!(check["state"], "missing", "{report:#}");
    assert_eq!(
        check["why"],
        format!(
            "it failed on this exact code last time, and a run here wouldn't count ({reason}): change the code, or pass it on a machine that meets the rule"
        )
    );
    assert_eq!(check["run"], Value::Null, "no command can clear it");
    assert_eq!(report["next"], json!([]), "{report:#}");
    let text = String::from_utf8_lossy(&r.hushrun(&["ready"]).stdout).into_owned();
    assert!(!text.contains("Run these here"), "{text}");
}

// covers: C-22, C-23, C-25
#[test]
fn a_recorded_failure_holds_a_push_that_a_run_here_cant_clear() {
    // On the right Go it fails, then passes on the wrong one, which doesn't count.
    let r = Repo::new("counts_from: {go: \"1.25\"}\n");
    r.go("1.25.14");
    let out = r.hushrun_with(UNIT, &[("VERDICT", "1")]);
    assert_eq!(code(&out), 1);
    assert_eq!(r.state().0, "missing");
    r.go("1.24.0");
    let out = r.hushrun(UNIT);
    assert!(err(&out).contains("it won't count"), "{}", err(&out));
    stuck(&r, "needs go 1.25, this is go1.24.0");
    git::run(&r.repo, &["init", "--bare", "../origin.git"], None);
    git::run(&r.repo, &["remote", "add", "origin", "../origin.git"], None);
    let install = r.hushrun(&["hook", "install"]);
    assert!(install.status.success(), "{}", err(&install));
    let push = |agent: bool| {
        let mut push = Command::new("git");
        r.env(&mut push);
        if agent {
            push.env("CLAUDECODE", "1");
        }
        push.args(["push", "origin", "HEAD:refs/heads/main"])
            .output()
            .expect("git push")
    };
    let refused = push(true);
    assert!(!refused.status.success(), "{}", err(&refused));
    assert!(err(&refused).contains("push refused"), "{}", err(&refused));
    assert!(!err(&refused).contains("Run these"), "{}", err(&refused));
    // A person's goes on, told what CI will do, with no command to run first.
    let warned = push(false);
    assert!(warned.status.success(), "{}", err(&warned));
    assert!(
        err(&warned).contains("CI will run unit"),
        "{}",
        err(&warned)
    );
    assert!(!err(&warned).contains("To skip that"), "{}", err(&warned));

    // Failing again on the right Go clears the mark, and the usual command is back.
    r.go("1.25.14");
    let out = r.hushrun_with(UNIT, &[("VERDICT", "1")]);
    assert_eq!(code(&out), 1);
    let (_, report) = r.ready(&[]);
    assert_eq!(report["checks"][0]["state"], "missing", "{report:#}");
    assert_eq!(
        report["checks"][0]["run"],
        "hushrun run unit -- sh check.sh"
    );

    // `os` or `env` not met, with a failure from a machine that met them, and no run here.
    let r = Repo::new("counts_from: {env: {SANDBOX: go-7}}\n");
    let out = r.hushrun_with(UNIT, &[("VERDICT", "1"), ("SANDBOX", "go-7")]);
    assert_eq!(code(&out), 1);
    stuck(&r, "needs SANDBOX go-7, and it isn't set");

    // A mark, then a failure recorded by another machine, which leaves the mark where it is.
    let r = Repo::new("counts_from: {go: \"1.25\"}\n");
    r.go("1.24.0");
    r.hushrun(UNIT);
    assert_eq!(r.state().0, "ci");
    r.go("1.25.14");
    let elsewhere = r.root.join("another-machine");
    let elsewhere = elsewhere.to_str().expect("utf-8");
    let out = r.hushrun_with(UNIT, &[("VERDICT", "1"), ("HUSHRUN_CACHE_DIR", elsewhere)]);
    assert_eq!(code(&out), 1, "{}", err(&out));
    stuck(&r, "needs go 1.25, this is go1.24.0");

    // And a pass from another machine is a pass, whatever is marked here: after that machine's
    // failure on the same code, it may be flaky.
    let out = r.hushrun_with(UNIT, &[("HUSHRUN_CACHE_DIR", elsewhere)]);
    assert!(
        err(&out)
            .ends_with("; it counts; it failed on this exact code before, so it may be flaky\n"),
        "{}",
        err(&out)
    );
    assert_eq!(r.state().0, "passed");
}

// covers: C-22, C-23, C-25
#[test]
fn a_cache_that_keeps_nothing_doesnt_ask_for_ever() {
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    let file = r.root.join("a-file");
    std::fs::write(&file, "").expect("write");
    let locked = r.root.join("locked");
    std::fs::create_dir(&locked).expect("mkdir");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let (file, locked) = (
        file.to_str().expect("utf-8"),
        locked.to_str().expect("utf-8"),
    );
    for cache in ["", file, locked] {
        let vars = [("HUSHRUN_CACHE_DIR", cache)];
        let out = r.hushrun_with(UNIT, &vars);
        assert_eq!(code(&out), 0, "{}", err(&out));
        assert!(
            err(&out).contains("hushrun ready won't learn of this"),
            "{cache}: {}",
            err(&out)
        );
        // Still the verdict last.
        assert!(last_line(&out).contains("it won't count"), "{}", err(&out));
        // Nothing here could ever learn why, so it doesn't ask.
        let out = r.hushrun_with(&["ready", "--format", "json"], &vars);
        let report: Value = serde_json::from_slice(&out.stdout).expect("json");
        let check = &report["checks"][0];
        assert_eq!(check["state"], "ci", "{cache}: {report:#}");
        assert!(
            check["why"]
                .as_str()
                .is_some_and(|why| why.starts_with("a run here couldn't say if it counted: ")),
            "{report:#}"
        );
        assert_eq!(code(&out), 0, "{}", err(&out));
    }
    // A rule on the system or a variable needs no mark: `ready` can read those itself.
    let r = Repo::new(&format!("counts_from: {{os: {}}}\n", here()));
    let out = r.hushrun_with(
        &["ready", "--format", "json"],
        &[("HUSHRUN_CACHE_DIR", locked)],
    );
    let report: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(report["checks"][0]["state"], "missing", "{report:#}");
    std::fs::set_permissions(locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

// covers: C-22, C-25
#[test]
fn a_marks_words_reach_a_terminal_only_as_one_short_line() {
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    r.hushrun(UNIT);
    let dir = r.root.join("cache/unrecorded");
    let marks: Vec<_> = std::fs::read_dir(&dir)
        .expect("marks")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(marks.len(), 1);
    let mark = format!(
        "needs \u{1b}[2Jgo\nand more\r\u{7}{}",
        "x".repeat(5_000_000)
    );
    std::fs::write(&marks[0], mark).expect("write");
    let (_, why) = r.state();
    let why = why.as_str().expect("why");
    assert!(
        why.starts_with("a run here didn't count: needs [2Jgoand more"),
        "{why:.60}"
    );
    assert!(why.len() < 1100, "{}", why.len());
    assert!(!why.chars().any(char::is_control), "{why:.60}");
}

// covers: C-22, C-25
#[test]
fn a_mark_stands_until_a_run_counts() {
    // A machine fixed after a run that didn't count: `ready` can't know, so this code costs CI one
    // run, or one by hand, which clears it.
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    let out = r.hushrun(UNIT);
    assert!(err(&out).contains("it won't count"), "{}", err(&out));
    r.go("1.25.14");
    assert_eq!(r.state().0, "ci");
    let out = r.hushrun(UNIT);
    assert!(err(&out).ends_with("; it counts\n"), "{}", err(&out));
    assert_eq!(r.state().0, "passed");
    // The next code starts clean.
    r.write("src/lib.txt", "two\n");
    git::commit_all(&r.repo, "two");
    assert_eq!(r.state().0, "missing");
}

// covers: C-22, C-23, C-25
#[test]
fn ready_and_the_hook_start_no_tool_for_any_repository() {
    for rules in [
        "",
        "counts_from: {go: \"1.25\", rust: \"1.94\", node: \"24\", xcode: \"16\"}\n",
    ] {
        let r = Repo::new(rules);
        r.go("1.25.14");
        r.tool("rustc", "echo 'rustc 1.94.0'\n");
        r.tool("node", "echo v24.0.0\n");
        r.tool("xcodebuild", "echo 'Xcode 26.0'\n");
        // What a repository can do to name what starts: every one of them a file it commits.
        r.write(".gitignore", "");
        r.write("rust-toolchain", "/some/where/evil\n");
        r.write(".volta/hooks.json", "{}\n");
        for path in [
            "node_modules/.bin/node",
            "Node_Modules/.bin/node",
            "tools/go",
        ] {
            r.write(path, "#!/bin/sh\ntouch started-by-hushrun\necho v24.0.0\n");
            std::fs::set_permissions(r.repo.join(path), std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        r.write(
            "package.json",
            "{\"scripts\": {\"test\": \"hushrun run -- sh check.sh\"}}\n",
        );
        r.write(".github/workflows/ci.yml", "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n");
        git::commit_all(&r.repo, "all of it");
        let (_, report) = r.ready(&[]);
        assert_eq!(report["checks"][0]["state"], "missing", "{report:#}");
        git::run(&r.repo, &["init", "--bare", "../origin.git"], None);
        git::run(&r.repo, &["remote", "add", "origin", "../origin.git"], None);
        let install = r.hushrun(&["hook", "install"]);
        assert!(install.status.success(), "{}", err(&install));
        let mut push = Command::new("git");
        r.env(&mut push);
        let out = push
            .env("CLAUDECODE", "1")
            .args(["push", "origin", "HEAD:refs/heads/main"])
            .output()
            .expect("git push");
        assert!(err(&out).contains("push refused"), "{}", err(&out));
        assert!(r.asked().is_empty(), "started {:?}", r.asked());
        assert!(
            !r.repo.join("started-by-hushrun").exists(),
            "it started the repository's own program"
        );
    }
}

// covers: C-22
#[test]
fn passed_says_who_recorded_it_as_the_server_says() {
    let r = Repo::new(&go_rule());
    r.go("1.25.14");
    r.fake.set_recorded_by("agent:sandbox-7");
    assert_eq!(r.ready(&[]).1["checks"][0]["recordedBy"], Value::Null);
    r.hushrun(UNIT);
    let (_, report) = r.ready(&[]);
    assert_eq!(report["checks"][0]["state"], "passed");
    assert_eq!(report["checks"][0]["recordedBy"], "agent:sandbox-7");
    let text = String::from_utf8_lossy(&r.hushrun(&["ready"]).stdout).into_owned();
    assert!(text.contains("recorded by agent:sandbox-7"), "{text}");
}

// covers: C-22, C-25
#[test]
fn strict_is_ready_only_when_every_check_passed() {
    // Nothing known.
    let r = Repo::new("");
    r.write(".github/workflows/ci.yml", "on: push\njobs: {}\n");
    git::commit_all(&r.repo, "nothing");
    assert_eq!(r.ready(&[]).0, 0);
    let (code_of, report) = r.ready(&["--strict"]);
    assert_eq!(code_of, 10, "{report:#}");
    assert_eq!(report["ready"], false);

    // A check with CI or another machine.
    let r = Repo::new("counts_from: {os: windows}\n");
    let (code_of, report) = r.ready(&[]);
    assert_eq!((code_of, &report["ready"]), (0, &json!(true)), "{report:#}");
    let (code_of, report) = r.ready(&["--strict"]);
    assert_eq!(
        (code_of, &report["ready"]),
        (10, &json!(false)),
        "{report:#}"
    );
    let out = r.hushrun(&["ready", "--strict"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("isn't ready: 1 check left, and nothing here changes that."),
        "{text}"
    );

    // Every check passed.
    let r = Repo::new(&go_rule());
    r.go("1.25.14");
    r.hushrun(UNIT);
    let (code_of, report) = r.ready(&["--strict"]);
    assert_eq!((code_of, &report["ready"]), (0, &json!(true)), "{report:#}");
}

// covers: C-22, C-23, C-25
#[test]
fn a_rule_for_a_name_nothing_runs_is_said_and_ci_still_skips() {
    let r = Repo::new(
        "checks:\n  go-tset:\n    counts_from: {os: linux}\n  unit:\n    counts_from: anywhere\n",
    );
    let (_, report) = r.ready(&[]);
    assert_eq!(
        report["notes"],
        json!(["hushrun.yaml has a rule for go-tset, and nothing runs it"])
    );
    r.hushrun(UNIT);
    git::run(&r.repo, &["init", "--bare", "../origin.git"], None);
    git::run(&r.repo, &["remote", "add", "origin", "../origin.git"], None);
    let install = r.hushrun(&["hook", "install"]);
    assert!(install.status.success(), "{}", err(&install));
    let mut push = Command::new("git");
    r.env(&mut push);
    let out = push
        .args(["push", "origin", "HEAD:refs/heads/main"])
        .output()
        .expect("git push");
    assert!(out.status.success(), "{}", err(&out));
    let said = err(&out);
    // The typo costs "CI skips" nothing: that's still true.
    assert!(said.contains("is ready: CI skips unit"), "{said}");
    assert!(
        said.contains("hushrun: hushrun.yaml has a rule for go-tset, and nothing runs it"),
        "{said}"
    );
}

// covers: C-23, C-25
#[test]
fn the_hook_doesnt_refuse_what_this_machine_cant_count() {
    let r = Repo::new(&go_rule());
    r.go("1.24.3");
    git::run(&r.repo, &["init", "--bare", "../origin.git"], None);
    git::run(&r.repo, &["remote", "add", "origin", "../origin.git"], None);
    let install = r.hushrun(&["hook", "install"]);
    assert!(install.status.success(), "{}", err(&install));
    let push = |vars: &[(&str, &str)]| {
        let mut push = Command::new("git");
        r.env(&mut push);
        push.envs(vars.iter().copied());
        push.args(["push", "origin", "HEAD:refs/heads/main"])
            .output()
            .expect("git push")
    };
    // It can't tell yet: the run will, once.
    let out = push(&[("CLAUDECODE", "1")]);
    assert!(!out.status.success(), "{}", err(&out));
    assert!(err(&out).contains("push refused"), "{}", err(&out));
    let run = r.hushrun(UNIT);
    assert!(err(&run).contains("it won't count"), "{}", err(&run));
    let out = push(&[("CLAUDECODE", "1")]);
    assert!(out.status.success(), "{}", err(&out));
    assert!(!err(&out).contains("CI skips"), "{}", err(&out));

    // Where it meets the rule, the agent's run leaves a pass, and CI skips it.
    r.write("src/lib.txt", "two\n");
    git::commit_all(&r.repo, "two");
    r.go("1.25.14");
    let out = push(&[("CLAUDECODE", "1")]);
    assert!(!out.status.success(), "{}", err(&out));
    r.hushrun(UNIT);
    let out = push(&[("CLAUDECODE", "1")]);
    assert!(out.status.success(), "{}", err(&out));
    assert!(err(&out).contains("CI skips unit"), "{}", err(&out));
}

// covers: C-8
#[test]
fn a_file_with_a_mistake_in_its_rule_runs_the_command_and_says_so() {
    let r = Repo::new("");
    r.write(
        "hushrun.yaml",
        &format!("{HEAD}counts_from: {{os: darwin}}\n"),
    );
    git::commit_all(&r.repo, "a mistake");
    let out = r.hushrun(UNIT);
    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(
        err(&out).contains("can't tell which code this is"),
        "{}",
        err(&out)
    );
    assert!(
        err(&out).contains("`os` is `macos`, not `darwin`"),
        "{}",
        err(&out)
    );
    assert_eq!(r.posts(), 0);
}
