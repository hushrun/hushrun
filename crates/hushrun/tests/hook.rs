//! `hushrun ready` and the pre-push hook, end to end: real `git push`es to a bare remote, with
//! the hook git runs, against a fake results server.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output};

use hushrun_testkit::fake_results::{FakeResults, quota_exceeded};
use hushrun_testkit::{git, schema};
use serde_json::{Value, json};

/// Variables a test must never inherit: this file's tests may run under a coding agent.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_INVOKED_BY",
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

const PACKAGE: &str = r#"{
  "name": "shop",
  "scripts": {
    "dev": "vite",
    "lint": "hushrun run -- sh checks/lint.sh",
    "test": "hushrun run unit -- sh checks/test.sh"
  }
}
"#;

/// A repository with hushrun's scripts, a bare `origin`, npm and hushrun on PATH, and a backend.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    origin: PathBuf,
    bin: PathBuf,
    backend: String,
}

impl Repo {
    fn new(backend: &str) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real path");
        let repo = root.join("repo");
        let origin = root.join("origin.git");
        let bin = root.join("bin");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::create_dir_all(&origin).expect("mkdir");
        std::fs::create_dir_all(&bin).expect("mkdir");
        git::init(&origin, "sha1");
        git::run(&origin, &["config", "core.bare", "true"], None);
        git::init(&repo, "sha1");
        git::run(
            &repo,
            &["remote", "add", "origin", origin.to_str().expect("utf-8")],
            None,
        );
        let hushrun = assert_cmd::cargo::cargo_bin("hushrun");
        std::os::unix::fs::symlink(hushrun, bin.join("hushrun")).expect("link");
        let r = Repo {
            _dir: dir,
            root,
            repo,
            origin,
            bin,
            backend: backend.to_string(),
        };
        r.tool(
            "npm",
            r#"[ "$1" = run ] || exit 1
script=$(sed -n "s/^ *\"$2\": \"\(.*\)\",\{0,1\}\$/\1/p" package.json | head -n 1)
export npm_lifecycle_event="$2"
exec sh -c "$script"
"#,
        );
        r.write(".gitignore", "node_modules/\n");
        r.write("package.json", PACKAGE);
        r.write("checks/lint.sh", "exit 0\n");
        r.write("checks/test.sh", "exit 0\n");
        r.write("src/lib.txt", "one\n");
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn tool(&self, name: &str, body: &str) {
        let path = self.bin.join(name);
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// The environment of everything a test runs here.
    fn env(&self, cmd: &mut Command) {
        // Not the machine's own hushrun: "hushrun isn't installed" has to be true here.
        let path = std::env::var_os("PATH").expect("PATH");
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone())
                .chain(std::env::split_paths(&path).filter(|dir| !dir.join("hushrun").exists())),
        )
        .expect("PATH");
        cmd.current_dir(&self.repo)
            .env("PATH", path)
            .env("HUSHRUN_TOKEN", FakeResults::TOKEN)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        match self.backend.as_str() {
            ON_THIS_MACHINE => cmd.env_remove("HUSHRUN_BACKEND"),
            SIGNED_OUT => cmd
                .env_remove("HUSHRUN_BACKEND")
                .env_remove("HUSHRUN_TOKEN"),
            backend => cmd.env("HUSHRUN_BACKEND", backend),
        };
    }

    fn hushrun(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
        self.env(&mut cmd);
        cmd.args(args).output().expect("hushrun")
    }

    fn npm(&self, script: &str) -> Output {
        let mut cmd = Command::new(self.bin.join("npm"));
        self.env(&mut cmd);
        cmd.args(["run", script]).output().expect("npm")
    }

    /// `git push origin HEAD:<branch>`, with the variables a coding agent or a person's shell
    /// has.
    fn push(&self, branch: &str, vars: &[(&str, &str)]) -> Output {
        self.push_args(&[&format!("HEAD:refs/heads/{branch}")], vars)
    }

    fn push_args(&self, refs: &[&str], vars: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("git");
        self.env(&mut cmd);
        cmd.args(["push", "origin"]).args(refs);
        for (name, value) in vars {
            cmd.env(name, value);
        }
        cmd.output().expect("git push")
    }

    fn head(&self) -> String {
        let head = git::run(&self.repo, &["rev-parse", "HEAD"], None);
        String::from_utf8_lossy(&head).trim().to_string()
    }

    /// What `branch` is on the remote.
    fn remote(&self, branch: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.origin)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{branch}"))
            .output()
            .expect("git");
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn hook(&self) -> PathBuf {
        self.repo.join(".git/hooks/pre-push")
    }

    fn installed(&self) {
        let out = self.hushrun(&["hook", "install"]);
        assert!(out.status.success(), "{}", stderr(&out));
    }
}

const AGENT: &[(&str, &str)] = &[("CLAUDECODE", "1")];
/// A backend of no server: no variable, no package.json key and no login, so that results stay in
/// this machine's cache directory (C-11).
const ON_THIS_MACHINE: &str = "<this machine>";
/// \[ADR 0067\] No variable and no token: a backend the repository's files name gets nothing, so
/// results stay on this machine too.
const SIGNED_OUT: &str = "<signed out>";

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn ready(repo: &Repo, args: &[&str]) -> (i32, Value) {
    let output = repo.hushrun(&[&["ready", "--format", "json"], args].concat());
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}{}", stdout(&output), stderr(&output)));
    schema::assert_valid(&schema::load("ready.v2.schema.json"), &value);
    (output.status.code().unwrap_or(-1), value)
}

fn states(value: &Value) -> Vec<(String, String)> {
    value["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|c| {
            (
                c["check"].as_str().expect("check").to_string(),
                c["state"].as_str().expect("state").to_string(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
        .collect()
}

// covers: C-22
#[test]
fn ready_says_what_ci_would_run_and_how_to_run_it_here() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());

    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(report["ready"], false);
    assert_eq!(report["revision"], json!(r.head()));
    assert_eq!(
        states(&report),
        pairs(&[("lint", "missing"), ("unit", "missing")])
    );
    assert_eq!(report["next"], json!(["npm run lint", "npm run test"]));
    assert_eq!(report["checks"][1]["run"], "npm run test");

    let text = r.hushrun(&["ready"]);
    assert_eq!(text.status.code(), Some(10));
    let out = stdout(&text);
    assert!(
        out.contains("isn't ready: CI would run 2 checks again."),
        "{out}"
    );
    assert!(
        out.contains("Run these here, then push:\n  npm run lint\n  npm run test\n"),
        "{out}"
    );

    assert!(r.npm("lint").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10);
    assert_eq!(report["next"], json!(["npm run test"]));

    assert!(r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["ready"], true);
    assert_eq!(report["next"], json!([]));
    assert_eq!(
        states(&report),
        pairs(&[("lint", "passed"), ("unit", "passed")])
    );
    assert!(
        stdout(&r.hushrun(&["ready"]))
            .ends_with("is ready to push.\n  passed  lint\n  passed  unit\n")
    );
}

// covers: C-22
#[test]
fn ready_says_so_when_a_check_failed_on_this_code() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.write("checks/test.sh", "exit 1\n");
    git::commit_all(&r.repo, "red");
    assert!(!r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10);
    assert_eq!(
        report["checks"][1]["why"],
        "it failed on this exact code last time"
    );
}

// covers: C-22
#[test]
fn committed_code_is_what_counts_and_uncommitted_changes_are_said() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    assert!(r.npm("lint").status.success());
    assert!(r.npm("test").status.success());
    assert_eq!(ready(&r, &[]).0, 0);

    // An edit: HEAD is still ready, and the worktree isn't.
    r.write("src/lib.txt", "two\n");
    assert_eq!(ready(&r, &[]).0, 0);
    let (code, report) = ready(&r, &["--worktree"]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(report["notes"], json!([]));

    // Committed, with the same files, what ran before the commit counts.
    assert!(r.npm("lint").status.success());
    assert!(r.npm("test").status.success());
    git::commit_all(&r.repo, "two");
    assert_eq!(ready(&r, &[]).0, 0);

    // HEAD lacks a pass, and a script would run on other files.
    r.write("src/lib.txt", "three\n");
    git::commit_all(&r.repo, "three");
    r.write("src/lib.txt", "four\n");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10);
    assert!(
        report["notes"][0]
            .as_str()
            .is_some_and(|n| n.contains("commit or stash")),
        "{report:#}"
    );
}

// covers: C-22
#[test]
fn ready_leaves_with_ci_what_it_cannot_do_anything_about() {
    // Results off: nothing here reaches CI.
    let r = Repo::new("");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("lint", "ci"), ("unit", "ci")]));

    // A backend that can't be reached says nothing about the code.
    let r = Repo::new("http://127.0.0.1:9/o/acme");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("lint", "ci"), ("unit", "ci")]));
    assert!(
        report["checks"][0]["why"]
            .as_str()
            .is_some_and(|w| w.contains("can't read earlier results")),
        "{report:#}"
    );

    // Words the shell would change: the key can't be told, so the script isn't looked at.
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.write(
        "package.json",
        r#"{"scripts": {
  "a": "hushrun run -- eslint src/*.ts",
  "b": "hushrun run --force -- tsc",
  "c": "hushrun run -- tsc || true",
  "d": "hushrun run -- tsc $FLAGS",
  "e": "npm run a && hushrun run -- vitest run"
}}
"#,
    );
    git::commit_all(&r.repo, "scripts");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10);
    assert_eq!(states(&report), pairs(&[("e", "missing")]));

    // Without anything to look at, there is nothing to say.
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.write(
        "package.json",
        "{\"scripts\": {\"test\": \"vitest run\"}}\n",
    );
    git::commit_all(&r.repo, "plain");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0);
    assert_eq!(report["checks"], json!([]));
    assert!(stdout(&r.hushrun(&["ready"])).contains("knows no checks"));
}

// covers: C-22, C-23 (ADR 0067: with results on this machine, ready says what passed here, and
// the hook holds an agent's push to it)
#[test]
fn with_results_on_this_machine_ready_and_the_hook_go_by_what_passed_here() {
    let r = Repo::new(ON_THIS_MACHINE);
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(report["results"], "local", "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("lint", "missing"), ("unit", "missing")])
    );
    assert_eq!(
        report["checks"][0]["why"],
        "no pass here yet for this exact code"
    );
    assert_eq!(report["next"], json!(["npm run lint", "npm run test"]));
    let note = "Results stay on this machine, so CI still runs these checks: hushrun init shares results with CI and teammates.";
    assert_eq!(report["notes"], json!([note]), "{report:#}");
    let text = stdout(&r.hushrun(&["ready"]));
    assert!(
        text.contains("isn't ready: 2 checks haven't passed here.\n"),
        "{text}"
    );
    assert!(text.ends_with(&format!("{note}\n")), "{text}");

    // An agent's push waits for them, as it does with a server.
    r.installed();
    let refused = r.push("feature", AGENT);
    assert!(!refused.status.success(), "{}", stderr(&refused));
    assert_eq!(r.remote("feature"), None);
    assert!(
        stderr(&refused).contains(&format!(
            "hushrun: push refused: lint and unit haven't passed here on {}\n  lint: no pass here yet for this exact code\n  unit: no pass here yet for this exact code\nRun these here, then push again:\n  npm run lint\n  npm run test\n{note}\n",
            &r.head()[..7]
        )),
        "{}",
        stderr(&refused)
    );

    // A failure here is one to fix first.
    r.write("checks/test.sh", "exit 1\n");
    git::commit_all(&r.repo, "red");
    assert!(r.npm("lint").status.success());
    assert!(!r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10);
    assert_eq!(
        states(&report),
        pairs(&[("lint", "passed"), ("unit", "missing")])
    );
    assert_eq!(
        report["checks"][1]["why"],
        "it failed here on this exact code last time"
    );

    // Passed here: ready, and the push goes, saying CI still runs them.
    r.write("checks/test.sh", "exit 0\n");
    git::commit_all(&r.repo, "green");
    assert!(r.npm("lint").status.success());
    assert!(r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("lint", "passed"), ("unit", "passed")])
    );
    assert!(
        stdout(&r.hushrun(&["ready"]))
            .contains("is ready to push.\n  passed  lint  here\n  passed  unit  here\n"),
        "{}",
        stdout(&r.hushrun(&["ready"]))
    );
    let pushed = r.push("feature", AGENT);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    assert_eq!(r.remote("feature"), Some(r.head()));
    let said = stderr(&pushed);
    assert!(
        said.contains(&format!(
            "hushrun: {} is ready: lint and unit passed here\nhushrun: {note}\n",
            &r.head()[..7]
        )),
        "{said}"
    );
    assert!(!said.contains("CI skips"), "{said}");

    // A person is told what hasn't passed here, and goes.
    r.write("src/lib.txt", "two\n");
    git::commit_all(&r.repo, "two");
    let person = r.push("feature", &[]);
    assert!(person.status.success(), "{}", stderr(&person));
    assert!(
        stderr(&person).contains(&format!(
            "hushrun: lint and unit haven't passed here on {}. Run these before you push:\n  npm run lint\n  npm run test\n",
            &r.head()[..7]
        )),
        "{}",
        stderr(&person)
    );
}

// covers: C-22 (ADR 0067: of what makes a check run every time, only what is about this machine
// holds when results stay here)
#[test]
fn with_results_on_this_machine_only_rules_about_this_machine_hold() {
    let r = Repo::new(ON_THIS_MACHINE);
    // CI runs `npm test` in a job of several variants, which no pass from here skips there; here,
    // a pass here skips the next run here.
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        node: [20, 22]\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n",
    );
    // A rule this machine doesn't meet: a run here would record nothing.
    let elsewhere = if cfg!(target_os = "linux") {
        "macos"
    } else {
        "linux"
    };
    r.write(
        "hushrun.yaml",
        &format!(
            "version: 1\nrepository: github.com/acme/shop\nchecks:\n  unit:\n    counts_from: {{os: {elsewhere}}}\n"
        ),
    );
    git::commit_all(&r.repo, "rules");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "ci")]), "{report:#}");
    assert!(
        report["checks"][0]["why"]
            .as_str()
            .is_some_and(|why| why.contains(&format!("needs {elsewhere}"))),
        "{report:#}"
    );
    r.write(
        "hushrun.yaml",
        "version: 1\nrepository: github.com/acme/shop\n",
    );
    git::commit_all(&r.repo, "no rule");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "missing")]), "{report:#}");
    assert!(r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "passed")]), "{report:#}");
}

// covers: C-22 (ADR 0067: a repository's backend nobody here is signed in to)
#[test]
fn signed_out_ready_reads_this_machine_and_says_to_sign_in() {
    let fake = FakeResults::start();
    let r = Repo::new(SIGNED_OUT);
    r.write(
        "package.json",
        &PACKAGE.replace(
            "\"name\": \"shop\",",
            &format!(
                "\"name\": \"shop\",\n  \"hushrun\": {{\"backend\": \"{}\"}},",
                fake.url()
            ),
        ),
    );
    git::commit_all(&r.repo, "backend");
    assert!(r.npm("lint").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(report["results"], "local");
    assert_eq!(
        states(&report),
        pairs(&[("lint", "passed"), ("unit", "missing")])
    );
    assert_eq!(
        report["notes"],
        json!([
            "Results stay on this machine, so CI still runs these checks: hushrun login shares results with acme's CI and teammates."
        ])
    );
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());
    // With a server, the JSON says so.
    let shared = Repo::new(&fake.url());
    assert_eq!(ready(&shared, &[]).1["results"], "shared");
}

// covers: C-22
#[test]
fn checks_ci_runs_are_the_ones_that_count_when_it_runs_any() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npm test\n",
    );
    git::commit_all(&r.repo, "ci");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10);
    assert_eq!(states(&report), pairs(&[("unit", "missing")]));
}

// covers: C-22, C-23
#[test]
fn a_check_ci_runs_outside_hushrun_is_said_until_it_goes_through() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    let workflow = |command: &str| {
        format!(
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: {command}\n"
        )
    };
    r.write(".github/workflows/ci.yml", &workflow("sh checks/test.sh"));
    git::commit_all(&r.repo, "ci");
    assert!(r.npm("lint").status.success());
    assert!(r.npm("test").status.success());
    // Everything passed here, and CI runs it anyway: ready must not say otherwise.
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    let note = "CI runs sh checks/test.sh itself in .github/workflows/ci.yml (job test), so it won't skip there: run hushrun init to put it through hushrun.";
    assert_eq!(report["notes"], json!([note]), "{report:#}");
    let pushed = r.push("feature", &[]);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    let said = stderr(&pushed);
    assert!(said.contains(&format!("hushrun: {note}")), "{said}");
    assert!(!said.contains("CI skips"), "{said}");

    // Through hushrun, under the script's name, it is one check with one key: the script's pass
    // is what CI's line finds.
    r.write(
        ".github/workflows/ci.yml",
        &workflow("hushrun run unit -- sh checks/test.sh"),
    );
    git::commit_all(&r.repo, "ci goes through hushrun");
    assert!(r.npm("lint").status.success());
    assert!(r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["notes"], json!([]), "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("lint", "passed"), ("unit", "passed")])
    );
    let ci = r.hushrun(&["run", "unit", "--", "sh", "checks/test.sh"]);
    assert!(ci.status.success(), "{}", stderr(&ci));
    assert!(
        stderr(&ci).contains("unit skipped: this exact code passed"),
        "{}",
        stderr(&ci)
    );
}

// covers: C-23
#[test]
fn an_agents_push_is_refused_until_the_checks_pass_then_it_goes() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();

    let refused = r.push("feature", AGENT);
    assert!(!refused.status.success(), "{}", stderr(&refused));
    assert_eq!(r.remote("feature"), None, "the push must not have gone");
    let said = stderr(&refused);
    assert!(
        said.contains(&format!(
            "hushrun: push refused: CI would run lint and unit again on {}\n  lint: no pass yet for this exact code\n  unit: no pass yet for this exact code\nRun these here, then push again:\n  npm run lint\n  npm run test\n",
            &r.head()[..7]
        )),
        "{said}"
    );
    // An agent isn't taught to go around it.
    assert!(!said.contains("--no-verify"), "{said}");

    // Doing what it said is all it takes.
    assert!(r.npm("lint").status.success());
    assert!(r.npm("test").status.success());
    let pushed = r.push("feature", AGENT);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    assert_eq!(r.remote("feature"), Some(r.head()));
    assert!(
        stderr(&pushed).contains(&format!(
            "hushrun: {} is ready: CI skips lint and unit",
            &r.head()[..7]
        )),
        "{}",
        stderr(&pushed)
    );

    // New code, and the same again; `--no-verify` is still git's.
    r.write("src/lib.txt", "two\n");
    git::commit_all(&r.repo, "two");
    assert!(!r.push("feature", AGENT).status.success());
    assert!(
        r.push_args(&["--no-verify", "HEAD:refs/heads/feature"], AGENT)
            .status
            .success()
    );
    assert_eq!(r.remote("feature"), Some(r.head()));
}

// covers: C-23
#[test]
fn every_agent_the_environment_names_is_refused() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    for vars in [
        ("CODEX_SANDBOX", "seatbelt"),
        ("OPENCODE", "1"),
        ("CURSOR_AGENT", "1"),
        ("AI_AGENT", "my-bot"),
        ("HUSHRUN_INVOKED_BY", "agent:fleet-1"),
    ] {
        assert!(!r.push("feature", &[vars]).status.success(), "{vars:?}");
    }
}

// covers: C-23
#[test]
fn a_hushrun_agent_string_is_an_agent_whether_it_reads_or_not() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&hushrun_attest::SigningKey::generate_p256().unwrap());
    let r = Repo::new(&fake.url());
    r.installed();
    for vars in [
        vec![("HUSHRUN_AGENT", string.as_str())],
        // A sandbox's CI=true doesn't make it CI, which the hook lets through.
        vec![("HUSHRUN_AGENT", string.as_str()), ("CI", "true")],
        vec![("HUSHRUN_AGENT", "not a string")],
        // In another variable, it says the same.
        vec![("HUSHRUN_INVOKED_BY", string.as_str())],
    ] {
        let refused = r.push("feature", &vars);
        assert!(!refused.status.success(), "{vars:?}");
        assert!(
            stderr(&refused).contains("hushrun: push refused: CI would run"),
            "{}",
            stderr(&refused)
        );
        assert!(
            !stderr(&refused).contains("git push --no-verify"),
            "an agent isn't told how to push anyway: {}",
            stderr(&refused)
        );
        assert_eq!(r.remote("feature"), None);
    }
    // Without it, CI=true alone is still CI, and goes.
    assert!(r.push("feature", &[("CI", "true")]).status.success());
}

// covers: C-23 (ADR 0046: a label is a word, and the string is an agent's credential)
#[test]
fn a_human_label_beside_a_string_is_still_an_agent() {
    let fake = FakeResults::start();
    let string = fake.agent_string(&hushrun_attest::SigningKey::generate_p256().unwrap());
    let r = Repo::new(&fake.url());
    r.installed();
    let human = ("HUSHRUN_INVOKED_BY", "human:ana");
    for beside in [
        ("HUSHRUN_AGENT", string.as_str()),
        ("HUSHRUN_AGENT", "not a string"),
        // Misplaced: in a variable that leaves the fake's token to answer for what's missing.
        ("HUSHRUN_RUN_URL", string.as_str()),
    ] {
        let refused = r.push("feature", &[human, beside]);
        assert!(!refused.status.success(), "{beside:?}");
        assert!(
            stderr(&refused).contains("hushrun: push refused: CI would run"),
            "{}",
            stderr(&refused)
        );
        assert_eq!(r.remote("feature"), None);
    }
    // Alone, the label says what it always did.
    assert!(r.push("feature", &[human]).status.success());
}

// covers: C-23
#[test]
fn a_persons_push_goes_and_says_what_ci_will_run() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    let pushed = r.push("feature", &[]);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    assert_eq!(r.remote("feature"), Some(r.head()));
    let said = stderr(&pushed);
    assert!(
        said.contains(&format!(
            "hushrun: CI will run lint and unit on {}. To skip that, run these before you push:\n  npm run lint\n  npm run test\n",
            &r.head()[..7]
        )),
        "{said}"
    );

    // A person who says so is a person, whatever they run in.
    let pushed = r.push(
        "other",
        &[("HUSHRUN_INVOKED_BY", "human:ana"), ("CLAUDECODE", "1")],
    );
    assert!(pushed.status.success(), "{}", stderr(&pushed));
}

// covers: C-23
#[test]
fn install_says_who_is_refused() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    assert!(r.hushrun(&["hook", "install", "--refuse"]).status.success());
    let refused = r.push("feature", &[]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("To push anyway, git push --no-verify: CI runs them."),
        "{}",
        stderr(&refused)
    );

    assert!(r.hushrun(&["hook", "install", "--warn"]).status.success());
    assert!(r.push("feature", AGENT).status.success());
    assert_eq!(r.remote("feature"), Some(r.head()));

    let text = std::fs::read_to_string(r.hook()).expect("hook");
    assert_eq!(text.matches("hushrun:start").count(), 1, "{text}");
    assert!(text.contains("--warn"), "{text}");

    let both = r.hushrun(&["hook", "install", "--warn", "--refuse"]);
    assert_eq!(both.status.code(), Some(2));
}

// covers: C-23
#[test]
fn only_a_known_gap_stops_a_push() {
    // The server is down.
    let r = Repo::new("http://127.0.0.1:9/o/acme");
    r.installed();
    let pushed = r.push("feature", AGENT);
    assert!(pushed.status.success(), "{}", stderr(&pushed));

    // hushrun.yaml doesn't parse.
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    r.write("hushrun.yaml", "version: 2\nnonsense: [\n");
    git::commit_all(&r.repo, "broken");
    let pushed = r.push("feature", AGENT);
    assert!(pushed.status.success(), "{}", stderr(&pushed));

    // hushrun itself fails any way but the one that means "not ready": a crash, an older
    // hushrun that doesn't know the command, a signal.
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    for (at, body) in ["exit 1", "exit 2", "exit 101", "kill -9 $$"]
        .iter()
        .enumerate()
    {
        r.tool("hushrun", &format!("{body}\n"));
        let pushed = r.push(&format!("b{at}"), AGENT);
        assert!(pushed.status.success(), "{body}: {}", stderr(&pushed));
    }
    r.tool("hushrun", "exit 10\n");
    assert!(!r.push("b4", AGENT).status.success());

    // hushrun isn't on this machine at all.
    std::fs::remove_file(r.bin.join("hushrun")).expect("rm");
    assert!(r.push("b5", AGENT).status.success());
}

// covers: C-23
#[test]
fn a_pass_that_cannot_be_recorded_does_not_trap_an_agent() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    assert!(!r.push("feature", AGENT).status.success());

    // ADR 0032: the org's plan records no more, and a check still runs and passes.
    let quota = quota_exceeded(
        "the plan's checks are used up",
        "https://hushrun.example/up",
    );
    fake.answer("POST", "v0/results/", 402, quota, 2);
    assert!(r.npm("lint").status.success());
    assert!(r.npm("test").status.success());
    assert!(fake.recorded().is_empty());

    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("lint", "ci"), ("unit", "ci")]));
    let pushed = r.push("feature", AGENT);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
}

// covers: C-22, C-23
#[test]
fn a_failure_here_outranks_an_earlier_pass_nothing_recorded() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    // The test passes, then fails on the same code: what it reads is outside the repository.
    r.write("checks/test.sh", "test ! -e \"$HOME/red\"\n");
    git::commit_all(&r.repo, "red when told");
    r.installed();
    assert!(r.npm("lint").status.success());
    let quota = quota_exceeded(
        "the plan's checks are used up",
        "https://hushrun.example/up",
    );
    fake.answer("POST", "v0/results/", 402, quota, 2);
    assert!(r.npm("test").status.success());
    std::fs::write(r.root.join("red"), "").expect("write");
    assert!(!r.npm("test").status.success());
    assert_eq!(fake.recorded().len(), 1, "lint's pass only");

    // Neither run was recorded, and the last one failed: that is still to fix before a push.
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("lint", "passed"), ("unit", "missing")]),
        "{report:#}"
    );
    assert!(!r.push("feature", AGENT).status.success());
}

// covers: C-23
#[test]
fn a_push_of_something_else_is_left_alone() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    // A tag, and a branch's deletion.
    git::run(&r.repo, &["tag", "v1"], None);
    assert!(r.push_args(&["refs/tags/v1"], AGENT).status.success());
    assert!(
        r.push_args(&["--no-verify", "HEAD:refs/heads/old"], &[])
            .status
            .success()
    );
    assert!(r.push_args(&[":refs/heads/old"], AGENT).status.success());
    // Nothing to push.
    assert!(
        r.push_args(&["--no-verify", "HEAD:refs/heads/same"], &[])
            .status
            .success()
    );
    assert!(r.push("same", AGENT).status.success());

    // Another branch than the one checked out is judged by its own commit.
    assert!(r.npm("lint").status.success());
    assert!(r.npm("test").status.success());
    git::run(&r.repo, &["branch", "passed"], None);
    r.write("src/lib.txt", "two\n");
    git::commit_all(&r.repo, "two");
    let pushed = r.push_args(&["passed:refs/heads/passed"], AGENT);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    let refused = r.push("feature", AGENT);
    assert!(!refused.status.success());

    // A CI job pushes for its own reasons, and a sandbox that says it isn't one doesn't.
    assert!(
        r.push("feature", &[("CI", "true"), ("CLAUDECODE", "1")])
            .status
            .success()
    );

    // Another repository's remote: a fork or a mirror has CI of its own.
    git::run(
        &r.repo,
        &[
            "config",
            "remote.origin.url",
            "git@github.com:acme/shop.git",
        ],
        None,
    );
    let sha = r.head();
    let line = format!("refs/heads/main {sha} refs/heads/main {}\n", "0".repeat(40));
    let run = |url: &str| {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(&r.repo)
            .env("HUSHRUN_BACKEND", &r.backend)
            .env("HUSHRUN_TOKEN", FakeResults::TOKEN)
            .env("HUSHRUN_CACHE_DIR", r.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", r.root.join("credentials.json"))
            .env("CLAUDECODE", "1")
            .args(["hook", "pre-push", "origin", url])
            .write_stdin(line.clone())
            .output()
            .expect("hushrun")
            .status
            .code()
    };
    assert_eq!(run("git@github.com:acme/fork.git"), Some(0));
    assert_eq!(run("git@github.com:Acme/Shop.git"), Some(10));
}

// covers: C-23
#[test]
fn a_linked_worktree_and_a_detached_head_work_the_same() {
    // Detached: git names the commit, and the branch it goes to is the remote's.
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    git::run(&r.repo, &["checkout", "-q", "--detach"], None);
    let refused = r.push("feature", AGENT);
    assert!(!refused.status.success(), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("push refused"));

    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    let linked = r.root.join("linked");
    git::run(
        &r.repo,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf-8"),
            "-b",
            "side",
        ],
        None,
    );
    let mut cmd = Command::new("git");
    r.env(&mut cmd);
    cmd.current_dir(&linked)
        .args(["push", "origin", "HEAD:refs/heads/side"])
        .env("CLAUDECODE", "1");
    let refused = cmd.output().expect("git push");
    assert!(!refused.status.success(), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("push refused"),
        "{}",
        stderr(&refused)
    );
}

// covers: C-23
#[test]
fn the_hook_goes_where_the_repository_keeps_hooks_and_comes_out_clean() {
    let fake = FakeResults::start();

    // Fresh: a hook of its own, that git can run.
    let r = Repo::new(&fake.url());
    let out = r.hushrun(&["hook", "install"]);
    assert!(
        stdout(&out).contains("Installed the pre-push hook in .git/hooks/pre-push."),
        "{}",
        stdout(&out)
    );
    let mode = std::fs::metadata(r.hook())
        .expect("hook")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
    let again = r.hushrun(&["hook", "install"]);
    assert!(
        stdout(&again).contains("is in .git/hooks/pre-push already"),
        "{}",
        stdout(&again)
    );
    let out = r.hushrun(&["hook", "uninstall"]);
    assert!(out.status.success());
    assert!(!r.hook().exists());
    assert!(stdout(&r.hushrun(&["hook", "uninstall"])).contains("has no hushrun lines"));

    // Someone else's hook is theirs: hushrun says what to add, and changes nothing.
    let theirs = "#!/usr/bin/env python3\nprint('mine')\n";
    std::fs::create_dir_all(r.hook().parent().expect("hooks")).expect("mkdir");
    std::fs::write(r.hook(), theirs).expect("write");
    let out = r.hushrun(&["hook", "install"]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains(
            "hushrun doesn't edit .git/hooks/pre-push. Add these lines to it:\n# hushrun:start"
        ),
        "{}",
        stdout(&out)
    );
    assert_eq!(std::fs::read_to_string(r.hook()).expect("read"), theirs);
    std::fs::remove_file(r.hook()).expect("rm");

    // husky's file is the team's shell lines, committed: hushrun adds to them, and takes itself out.
    let r = Repo::new(&fake.url());
    r.write(".husky/pre-push", "npm run lint\n");
    let out = r.hushrun(&["hook", "install"]);
    assert!(
        stdout(&out).contains("Installed the pre-push hook in .husky/pre-push."),
        "{}",
        stdout(&out)
    );
    let text = std::fs::read_to_string(r.repo.join(".husky/pre-push")).expect("read");
    assert!(text.starts_with("npm run lint\n# hushrun:start"), "{text}");
    assert!(r.hushrun(&["hook", "uninstall"]).status.success());
    assert_eq!(
        std::fs::read_to_string(r.repo.join(".husky/pre-push")).expect("read"),
        "npm run lint\n"
    );

    // core.hooksPath is where git looks.
    let r = Repo::new(&fake.url());
    git::run(&r.repo, &["config", "core.hooksPath", ".githooks"], None);
    assert!(r.hushrun(&["hook", "install"]).status.success());
    assert!(r.repo.join(".githooks/pre-push").exists());
    assert!(!r.push("feature", AGENT).status.success());

    // A refusal holds in a plain shell whatever comes after hushrun's lines. husky runs its file
    // with `sh -e`, and a team's own hook needn't.
    let r = Repo::new(&fake.url());
    git::run(&r.repo, &["config", "core.hooksPath", ".husky"], None);
    r.write(".husky/pre-push", "#!/bin/sh\n");
    assert!(r.hushrun(&["hook", "install"]).status.success());
    let hook = r.repo.join(".husky/pre-push");
    let text = std::fs::read_to_string(&hook).expect("read");
    std::fs::write(&hook, format!("{text}echo their last line\n")).expect("write");
    let refused = r.push("feature", AGENT);
    assert!(!refused.status.success(), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("push refused"),
        "{}",
        stderr(&refused)
    );
    let said = format!("{}{}", stdout(&refused), stderr(&refused));
    assert!(!said.contains("their last line"), "{said}");
    assert_eq!(r.remote("feature"), None);
    // And a person's push still reaches the team's lines.
    let pushed = r.push("feature", &[]);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    let said = format!("{}{}", stdout(&pushed), stderr(&pushed));
    assert!(said.contains("their last line"), "{said}");

    // lefthook's YAML is a person's to edit.
    let r = Repo::new(&fake.url());
    r.write("lefthook.yml", "pre-commit: {}\n");
    let out = r.hushrun(&["hook", "install"]);
    assert!(stdout(&out).contains("use_stdin: true"), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("hook pre-push {1} {2} || [ \"$?\" -ne 10 ]"),
        "{}",
        stdout(&out)
    );
    assert!(!r.hook().exists());
}

// covers: C-23
#[test]
fn prepare_never_fails_an_install() {
    let dir = tempfile::tempdir().expect("tempdir");
    let run = |vars: &[(&str, &str)]| {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
        cmd.current_dir(dir.path()).args(["hook", "install"]);
        for var in CLEARED {
            cmd.env_remove(var);
        }
        for (name, value) in vars {
            cmd.env(name, value);
        }
        cmd.output().expect("hushrun").status.code()
    };
    // An image without .git. Yarn from 2 on has `postinstall` run it.
    assert_eq!(run(&[("npm_lifecycle_event", "prepare")]), Some(0));
    assert_eq!(run(&[("npm_lifecycle_event", "postinstall")]), Some(0));
    assert_eq!(run(&[]), Some(2));
    // A CI job has no use for it, and says nothing.
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
    r.env(&mut cmd);
    let out = cmd
        .args(["hook", "install"])
        .env("CI", "true")
        .output()
        .expect("hushrun");
    assert!(out.status.success());
    assert!(!r.hook().exists());
    assert!(out.stdout.is_empty());
}

// --- ADR 0043: a clone gets the hook from what people and agents run in it --------------------

const INSTALLED: &str = "hushrun: installed the pre-push hook in .git/hooks/pre-push; hushrun hook uninstall takes it out\n";

impl Repo {
    /// `hushrun ready`, which installs the hook wherever it may.
    fn ready_here(&self) -> Output {
        self.hushrun(&["ready", "--format", "json"])
    }

    fn hook_text(&self) -> String {
        std::fs::read_to_string(self.hook()).expect("hook")
    }

    /// What is in git's hooks directory, samples apart.
    fn in_hooks(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.repo.join(".git/hooks"))
            .map(|dir| {
                dir.map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .filter(|name| !name.ends_with(".sample"))
                .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

// covers: C-23
#[test]
fn ready_puts_the_hook_in_a_clone_that_has_none_and_says_so_once() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    assert!(!r.hook().exists());

    let first = r.ready_here();
    assert_eq!(first.status.code(), Some(10), "{}", stderr(&first));
    assert_eq!(stderr(&first), INSTALLED);
    assert_eq!(r.in_hooks(), ["pre-push"]);
    let mode = std::fs::metadata(r.hook())
        .expect("hook")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
    assert!(
        r.hook_text().starts_with("#!/bin/sh\n# hushrun:start"),
        "{}",
        r.hook_text()
    );

    // Only stderr knows: the report is the same, and valid.
    let second = r.ready_here();
    assert_eq!(stderr(&second), "");
    assert_eq!(stdout(&first), stdout(&second));
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");

    // It is the real hook: it refuses an agent's push, and the code didn't change.
    let refused = r.push("feature", AGENT);
    assert!(!refused.status.success(), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("push refused"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(git::text(&r.repo, &["status", "--porcelain"]), "");
}

// covers: C-23
#[test]
fn a_run_puts_it_there_too_and_quiet_says_nothing() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let out = r.hushrun(&["run", "unit", "--", "sh", "checks/test.sh"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains(INSTALLED), "{}", stderr(&out));
    assert!(r.hook().exists());
    let again = r.hushrun(&["run", "unit", "--", "sh", "checks/test.sh"]);
    assert!(!stderr(&again).contains("pre-push"), "{}", stderr(&again));

    let r = Repo::new(&fake.url());
    let out = r.hushrun(&["-q", "ready"]);
    assert_eq!(out.status.code(), Some(10));
    assert!(!stderr(&out).contains("pre-push"), "{}", stderr(&out));
    assert!(r.hook().exists());
}

// covers: C-23
#[test]
fn the_hooks_own_command_installs_nothing() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
    r.env(&mut cmd);
    let out = cmd
        .args(["hook", "pre-push", "origin", "url"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("hushrun");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!r.hook().exists());
}

// covers: C-23
#[test]
fn ci_gets_no_hook_and_a_sandbox_that_says_local_does() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let out = r.in_ci(".", &["ready"]);
    assert!(!stderr(&out).contains("pre-push"), "{}", stderr(&out));
    let out = r.in_ci(".", &["run", "unit", "--", "sh", "checks/test.sh"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(r.in_hooks(), Vec::<String>::new());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
    r.env(&mut cmd);
    cmd.env("CI", "true")
        .env("HUSHRUN_PROVIDER", "local")
        .arg("ready")
        .output()
        .expect("hushrun");
    assert!(r.hook().exists());
}

// covers: C-23
#[test]
fn only_a_repository_set_up_for_hushrun_gets_it() {
    let fake = FakeResults::start();
    let with = |package: Option<&str>| {
        let r = Repo::new(&fake.url());
        match package {
            Some(text) => r.write("package.json", text),
            None => std::fs::remove_file(r.repo.join("package.json")).expect("rm"),
        }
        git::commit_all(&r.repo, "no hushrun");
        let out = r.hushrun(&["ready"]);
        assert!(out.status.code().is_some(), "{}", stderr(&out));
        r
    };
    // Nothing names hushrun, whatever runs it by hand here.
    assert!(!with(None).hook().exists());
    let plain = with(Some(
        "{\"name\": \"shop\", \"scripts\": {\"test\": \"vitest\"}}\n",
    ));
    assert!(!plain.hook().exists());
    let out = plain.hushrun(&["run", "unit", "--", "sh", "checks/test.sh"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!plain.hook().exists());
    assert!(!with(Some("not JSON\n")).hook().exists());

    // package.json names it in any of these ways.
    for package in [
        "{\"hushrun\": {\"backend\": \"https://acme.invalid\"}}\n",
        "{\"devDependencies\": {\"hushrun\": \"^1.0.0\"}}\n",
        "{\"dependencies\": {\"hushrun\": \"^1.0.0\"}}\n",
        "{\"scripts\": {\"test\": \"hushrun run -- vitest\"}}\n",
    ] {
        assert!(with(Some(package)).hook().exists(), "{package}");
    }

    // hushrun.yaml does, with no package.json at all: a Go repository, say.
    let r = in_two_scopes(&fake.url());
    assert!(!r.hook().exists());
    let out = r.hushrun(&["ready"]);
    assert!(stderr(&out).contains(INSTALLED), "{}", stderr(&out));
    assert!(r.hook().exists());
}

// covers: C-23
#[test]
fn what_is_there_stays_as_it_is() {
    let fake = FakeResults::start();

    // Someone else's hook, a link that leads nowhere, and hushrun's in another mode.
    let r = Repo::new(&fake.url());
    let theirs = "#!/bin/sh\nmake lint\n";
    std::fs::create_dir_all(r.hook().parent().expect("hooks")).expect("mkdir");
    std::fs::write(r.hook(), theirs).expect("write");
    let out = r.ready_here();
    assert_eq!(stderr(&out), "");
    assert_eq!(r.hook_text(), theirs);

    let r = Repo::new(&fake.url());
    std::fs::create_dir_all(r.hook().parent().expect("hooks")).expect("mkdir");
    std::os::unix::fs::symlink("nowhere", r.hook()).expect("link");
    assert_eq!(stderr(&r.ready_here()), "");
    assert_eq!(
        std::fs::read_link(r.hook()).expect("link"),
        PathBuf::from("nowhere")
    );

    for mode in ["--refuse", "--warn"] {
        let r = Repo::new(&fake.url());
        assert!(r.hushrun(&["hook", "install", mode]).status.success());
        let before = r.hook_text();
        assert!(before.contains(mode), "{before}");
        assert_eq!(stderr(&r.ready_here()), "");
        assert_eq!(r.hook_text(), before, "{mode}");
    }
}

// covers: C-23
#[test]
fn hooks_the_repository_keeps_elsewhere_are_left_out_of_it() {
    let fake = FakeResults::start();

    // git looks somewhere else, for this repository or for everything on this machine.
    let r = Repo::new(&fake.url());
    git::run(&r.repo, &["config", "core.hooksPath", ".githooks"], None);
    assert_eq!(stderr(&r.ready_here()), "");
    assert!(!r.hook().exists());
    assert!(!r.repo.join(".githooks").exists());

    let r = Repo::new(&fake.url());
    let out = r.sh("git config --global core.hooksPath \"$HOME/hooks\"");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&r.ready_here()), "");
    assert!(!r.hook().exists());
    assert!(!r.root.join("hooks").exists());

    // husky's file and lefthook's are tracked: a change to them is a change to the code.
    let r = Repo::new(&fake.url());
    r.write(".husky/pre-push", "npm run lint\n");
    git::commit_all(&r.repo, "husky");
    assert_eq!(stderr(&r.ready_here()), "");
    assert!(!r.hook().exists());
    assert_eq!(
        std::fs::read_to_string(r.repo.join(".husky/pre-push")).expect("read"),
        "npm run lint\n"
    );
    assert_eq!(git::text(&r.repo, &["status", "--porcelain"]), "");

    for name in ["lefthook.yml", ".lefthook.yaml"] {
        let r = Repo::new(&fake.url());
        r.write(name, "pre-commit: {}\n");
        git::commit_all(&r.repo, "lefthook");
        assert_eq!(stderr(&r.ready_here()), "", "{name}");
        assert!(!r.hook().exists(), "{name}");
    }
}

// covers: C-23
#[test]
fn an_uninstall_holds_until_someone_asks_for_the_hook() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    assert!(r.ready_here().status.code().is_some());
    assert!(r.hook().exists());

    let out = r.hushrun(&["hook", "uninstall"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Took the pre-push hook out of .git/hooks/pre-push.\n"),
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("until hushrun hook install"),
        "{}",
        stdout(&out)
    );
    assert!(!r.hook().exists());

    // Neither command brings it back, nor does a package manager's install.
    assert_eq!(stderr(&r.ready_here()), "");
    let run = r.hushrun(&["run", "unit", "--", "sh", "checks/test.sh"]);
    assert!(run.status.success(), "{}", stderr(&run));
    let mut prepare = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
    r.env(&mut prepare);
    let out = prepare
        .args(["hook", "install"])
        .env("npm_lifecycle_event", "prepare")
        .output()
        .expect("hushrun");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!r.hook().exists());
    // It is the clone's own: nothing to commit.
    assert_eq!(git::text(&r.repo, &["status", "--porcelain"]), "");

    // Someone asking for it does, and from then on it comes back by itself.
    assert!(r.hushrun(&["hook", "install"]).status.success());
    assert!(r.hook().exists());
    std::fs::remove_file(r.hook()).expect("rm");
    assert_eq!(stderr(&r.ready_here()), INSTALLED);
    assert!(r.hook().exists());

    // An uninstall counts in a clone that never had the hook.
    let r = Repo::new(&fake.url());
    assert!(r.hushrun(&["hook", "uninstall"]).status.success());
    assert_eq!(stderr(&r.ready_here()), "");
    assert!(!r.hook().exists());
}

// covers: C-23
#[test]
fn a_package_managers_install_keeps_the_mode_someone_chose() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let mut scripts: Value = serde_json::from_str(PACKAGE).expect("JSON");
    scripts["scripts"]["prepare"] = json!("hushrun hook install || exit 0");
    r.write("package.json", &format!("{scripts:#}\n"));
    git::commit_all(&r.repo, "prepare");
    for mode in ["--refuse", "--warn"] {
        assert!(r.hushrun(&["hook", "install", mode]).status.success());
        let chosen = r.hook_text();
        let out = r.npm("prepare");
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(r.hook_text(), chosen, "{mode}");
    }
    // A prepare with a flag gives it to a new hook, and leaves one that is there as it is.
    let prepare = |flag: &str| {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
        r.env(&mut cmd);
        cmd.args(["hook", "install", flag])
            .env("npm_lifecycle_event", "prepare")
            .output()
            .expect("hushrun")
    };
    assert!(prepare("--refuse").status.success());
    assert!(
        r.hook_text().contains("hook pre-push --warn"),
        "{}",
        r.hook_text()
    );
    std::fs::remove_file(r.hook()).expect("rm");
    assert!(prepare("--refuse").status.success());
    assert!(
        r.hook_text().contains("hook pre-push --refuse"),
        "{}",
        r.hook_text()
    );
    // Someone asking is what changes it, back to the default too.
    assert!(r.hushrun(&["hook", "install"]).status.success());
    assert!(
        r.hook_text().contains("hook pre-push \"$@\""),
        "{}",
        r.hook_text()
    );
    assert_eq!(r.hook_text().matches("# hushrun:start").count(), 1);
}

// covers: C-23
#[test]
fn a_clone_that_cant_take_the_hook_runs_as_if_nothing_happened() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let hooks = r.repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");
    std::fs::set_permissions(&hooks, std::fs::Permissions::from_mode(0o555)).expect("chmod");

    let report = r.ready_here();
    let run = r.hushrun(&["run", "unit", "--", "sh", "checks/test.sh"]);
    std::fs::set_permissions(&hooks, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(report.status.code(), Some(10), "{}", stderr(&report));
    assert_eq!(stderr(&report), "");
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(!stderr(&run).contains("pre-push"), "{}", stderr(&run));
    assert!(!r.hook().exists());
}

// covers: C-23
#[test]
fn runs_at_the_same_moment_leave_one_whole_hook() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let runs: Vec<_> = (0..12)
        .map(|_| {
            let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
            r.env(&mut cmd);
            cmd.args(["ready", "--format", "json"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("hushrun")
        })
        .collect();
    let said: Vec<String> = runs
        .into_iter()
        .map(|run| stderr(&run.wait_with_output().expect("hushrun")))
        .collect();
    let installs = said.iter().filter(|text| *text == INSTALLED).count();
    assert_eq!(installs, 1, "{said:#?}");
    assert_eq!(said.iter().filter(|text| text.is_empty()).count(), 11);
    assert_eq!(r.in_hooks(), ["pre-push"]);
    assert!(
        r.hook_text().ends_with("# hushrun:end\n"),
        "{}",
        r.hook_text()
    );
}

// covers: C-23
#[test]
fn a_linked_worktree_puts_it_where_git_looks() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let linked = r.root.join("linked");
    git::run(
        &r.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().expect("utf-8"),
        ],
        None,
    );
    let here = linked.to_str().expect("utf-8");
    let first = r.hushrun(&["-C", here, "ready"]);
    assert!(
        stderr(&first).contains("installed the pre-push hook in"),
        "{}",
        stderr(&first)
    );
    assert!(r.hook().exists());
    assert_eq!(stderr(&r.hushrun(&["-C", here, "ready"])), "");
    assert_eq!(stderr(&r.ready_here()), "");
}

// --- ADR 0037: any language, several scopes, and what a scope builds against -----------------

const SCOPES: &str = "version: 1
repository: github.com/acme/shop
scopes:
  api:
    include: [\"api/**\", \".github/workflows/**\"]
  web:
    include: [\"web/**\", \".github/workflows/**\"]
";

const CI: &str = "name: CI
on: [push, pull_request]
jobs:
  api:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: api
    steps:
      - uses: actions/checkout@v5
      - run: hushrun run --scope api test -- sh checks/test.sh
  web:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - run: |
          cd web
          hushrun run --scope web lint -- sh checks/test.sh --quiet
";

/// A repository with no package.json, whose CI files name its checks, in two scopes.
fn in_two_scopes(backend: &str) -> Repo {
    let r = Repo::new(backend);
    std::fs::remove_file(r.repo.join("package.json")).expect("remove");
    r.write("hushrun.yaml", SCOPES);
    r.write(".github/workflows/ci.yml", CI);
    r.write("api/checks/test.sh", "exit 0\n");
    r.write("web/checks/test.sh", "exit 0\n");
    git::commit_all(&r.repo, "two scopes");
    r
}

impl Repo {
    /// `line` in a shell here, as a person or an agent runs what `ready` prints.
    fn sh(&self, line: &str) -> Output {
        let mut cmd = Command::new("sh");
        self.env(&mut cmd);
        cmd.args(["-c", line]).output().expect("sh")
    }

    /// `hushrun run` in `dir`, as the GitHub Actions job `api` runs it.
    fn in_ci(&self, dir: &str, args: &[&str]) -> Output {
        self.in_job("api", dir, args)
    }

    /// `hushrun run` in `dir`, as a GitHub Actions job runs it.
    fn in_job(&self, job: &str, dir: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
        self.env(&mut cmd);
        cmd.current_dir(self.repo.join(dir))
            .env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", "acme/shop")
            .env(
                "GITHUB_WORKFLOW_REF",
                "acme/shop/.github/workflows/ci.yml@refs/heads/main",
            )
            .env("GITHUB_JOB", job)
            .args(args)
            .output()
            .expect("hushrun")
    }
}

// covers: C-22
#[test]
fn ci_files_name_a_repositorys_checks_in_any_language_and_their_scopes() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/test", "missing"), ("web/lint", "missing")])
    );
    assert_eq!(
        report["next"],
        json!([
            "cd api && hushrun run --scope api test -- sh checks/test.sh",
            "cd web && hushrun run --scope web lint -- sh checks/test.sh --quiet",
        ])
    );

    // Doing what it says ends, one scope at a time.
    let line = report["next"][0].as_str().expect("a command");
    let out = r.sh(line);
    assert!(out.status.success(), "{}", stderr(&out));
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/test", "passed"), ("web/lint", "missing")])
    );

    // Another scope's change leaves a pass where it is.
    r.write("web/app.txt", "two\n");
    git::commit_all(&r.repo, "web");
    let (_, report) = ready(&r, &[]);
    assert_eq!(
        states(&report),
        pairs(&[("api/test", "passed"), ("web/lint", "missing")])
    );
    let out = r.sh(report["next"][0].as_str().expect("a command"));
    assert!(out.status.success(), "{}", stderr(&out));
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");

    // CI finds them under the same keys: the job's own directory and words skip.
    let skipped = r.in_ci(
        "api",
        &[
            "run",
            "--scope",
            "api",
            "test",
            "--",
            "sh",
            "checks/test.sh",
        ],
    );
    assert!(skipped.status.success(), "{}", stderr(&skipped));
    assert!(
        stderr(&skipped).contains("test skipped"),
        "{}",
        stderr(&skipped)
    );
}

// covers: C-22
#[test]
fn a_hushrun_run_with_no_scope_among_several_stays_with_ci() {
    // It would run without skipping, and record nothing, so asking for it would never end.
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run test -- sh checks/test.sh\n      - run: hushrun run --scope gone test -- sh checks/test.sh\n",
    );
    git::commit_all(&r.repo, "unscoped");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("test", "ci"), ("test", "ci")]));
    assert_eq!(
        report["checks"][0]["why"],
        "it names no --scope, and hushrun.yaml has several"
    );
    assert_eq!(report["checks"][1]["why"], "hushrun.yaml has no scope gone");
}

// covers: C-22
#[test]
fn package_json_scripts_say_their_scope_too() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.write("hushrun.yaml", SCOPES);
    r.write("api/a.txt", "one\n");
    r.write("web/a.txt", "one\n");
    r.write(
        "package.json",
        "{\n  \"scripts\": {\n    \"test\": \"hushrun run --scope api unit -- sh checks/test.sh\",\n    \"lint\": \"hushrun run --scope web -- sh checks/lint.sh\"\n  }\n}\n",
    );
    git::commit_all(&r.repo, "scoped scripts");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/unit", "missing"), ("web/lint", "missing")])
    );
    assert!(r.npm("test").status.success());
    assert!(r.npm("lint").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/unit", "passed"), ("web/lint", "passed")])
    );
}

// covers: C-23
#[test]
fn an_agents_push_is_refused_in_a_repository_of_several_scopes_until_they_pass() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.installed();
    let out = r.push("fix", AGENT);
    assert!(!out.status.success(), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(
        text.contains("hushrun: push refused: CI would run api/test and web/lint again on"),
        "{text}"
    );
    assert!(
        text.contains("  cd web && hushrun run --scope web lint -- sh checks/test.sh --quiet"),
        "{text}"
    );
    assert_eq!(r.remote("fix"), None);

    // A person's goes on, and says the same.
    let out = r.push("mine", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("CI will run api/test and web/lint"),
        "{}",
        stderr(&out)
    );

    // What the refusal printed lets it through.
    let (_, report) = ready(&r, &[]);
    for line in report["next"].as_array().expect("next") {
        let out = r.sh(line.as_str().expect("a command"));
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let out = r.push("fix", AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(r.remote("fix"), Some(r.head()));
}

// covers: C-22
#[test]
fn what_a_job_needs_that_this_machine_lacks_stays_with_ci() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    let step = "    steps:\n      - uses: actions/checkout@v5\n";
    let run = "      - run: hushrun run --scope api CHECK -- sh checks/test.sh\n";
    let job = |name: &str, extra: &str, step_extra: &str| {
        format!(
            "  {name}:\n    runs-on: ubuntu-latest\n{extra}{step}{}{step_extra}",
            run.replace("CHECK", name)
        )
    };
    let other = if cfg!(target_os = "macos") {
        "windows-latest"
    } else {
        "macos-latest"
    };
    r.write(
        ".github/workflows/ci.yml",
        &format!(
            "on: push\njobs:\n{}{}{}{}{}{}  far:\n    runs-on: {other}\n{step}{}",
            job(
                "back",
                "    services:\n      postgres:\n        image: postgres:17\n",
                ""
            ),
            job("boxed", "    container: rust:1.90\n", ""),
            job(
                "secret",
                "    env:\n      TOKEN: ${{ secrets.NPM_TOKEN }}\n",
                ""
            ),
            job(
                "stepsecret",
                "",
                "        env:\n          KEY: ${{ secrets.DEPLOY_KEY }}\n"
            ),
            job("guarded", "    if: github.ref == 'refs/heads/main'\n", ""),
            job("stepguarded", "", "        if: runner.os == 'Linux'\n"),
            run.replace("CHECK", "far"),
        ),
    );
    git::commit_all(&r.repo, "jobs");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "nothing here can run: {report:#}");
    let why = |check: &str| -> String {
        let item = report["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .find(|item| item["check"] == check)
            .unwrap_or_else(|| panic!("{check}: {report:#}"));
        assert_eq!(item["state"], "ci", "{item}");
        item["why"].as_str().expect("why").to_string()
    };
    assert_eq!(why("api/back"), "its job has services");
    assert_eq!(why("api/boxed"), "its job runs in a container");
    assert_eq!(
        why("api/secret"),
        "it needs secrets.NPM_TOKEN, which stays in CI"
    );
    assert_eq!(
        why("api/stepsecret"),
        "it needs secrets.DEPLOY_KEY, which stays in CI"
    );
    assert_eq!(why("api/guarded"), "its job has a condition");
    assert_eq!(why("api/stepguarded"), "its step has a condition");
    assert_eq!(why("api/far"), format!("its job runs on {other}"));

    // A pass from CI still counts for all of them.
    let out = r.in_job(
        "back",
        "",
        &[
            "run",
            "--scope",
            "api",
            "back",
            "--",
            "sh",
            "checks/test.sh",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let (_, report) = ready(&r, &[]);
    assert_eq!(report["checks"][0]["state"], "passed", "{report:#}");
}

// covers: C-22, C-23
/// \[ADR 0055\] A job behind a condition on other jobs' outputs, as a changes filter writes it,
/// may run: `ready` lists its check as any job's, and the hook refuses an agent's push until it
/// passes here, even on a push CI wouldn't run it for. A condition on anything else stays with
/// CI, and a called workflow's check takes the condition of the job that calls it.
#[test]
fn a_job_behind_a_changes_filter_may_run() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  changes:\n    runs-on: ubuntu-latest\n    outputs:\n      api: ${{ steps.filter.outputs.api }}\n    steps:\n      - id: filter\n        run: echo api=true >> \"$GITHUB_OUTPUT\"\n  filtered:\n    needs: changes\n    if: needs.changes.outputs.api == 'true'\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api filtered -- sh checks/test.sh\n        working-directory: api\n  guarded:\n    needs: changes\n    if: needs.changes.outputs.api == 'true' && github.ref == 'refs/heads/main'\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api guarded -- sh checks/test.sh\n        working-directory: api\n  nightly:\n    if: github.event_name == 'schedule'\n    uses: ./.github/workflows/called.yml\n",
    );
    r.write(
        ".github/workflows/called.yml",
        "on:\n  workflow_call:\njobs:\n  inner:\n    runs-on: ubuntu-latest\n    env:\n      HUSHRUN_WORKFLOW: .github/workflows/called.yml\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api inner -- sh checks/test.sh\n        working-directory: api\n",
    );
    git::commit_all(&r.repo, "a changes filter");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[
            ("api/filtered", "missing"),
            ("api/guarded", "ci"),
            ("api/inner", "ci")
        ])
    );
    let why = |n: usize| {
        report["checks"][n]["why"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(why(0), "no pass yet for this exact code");
    assert_eq!(why(1), "its job has a condition");
    assert_eq!(
        why(2),
        "job nightly of .github/workflows/ci.yml, which calls its workflow, has a condition"
    );

    // An agent's push waits for it, and what ready prints lets it through.
    r.installed();
    let out = r.push("change", AGENT);
    assert!(!out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("push refused: CI would run api/filtered again"),
        "{}",
        stderr(&out)
    );
    for line in report["next"].as_array().expect("next") {
        let out = r.sh(line.as_str().expect("a command"));
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let out = r.push("change", AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(r.remote("change"), Some(r.head()));
}

// covers: C-22
#[test]
fn a_key_ci_fills_in_stays_with_ci() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  many:\n    runs-on: ubuntu-latest\n    strategy:\n      matrix:\n        os: [a, b]\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api test-${{ matrix.os }} -- sh checks/test.sh\n  moved:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: |\n          cd \"$PROJECT\"\n          hushrun run --scope api test -- sh checks/test.sh\n",
    );
    git::commit_all(&r.repo, "unknown keys");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("job many", "ci"), ("test", "ci")])
    );
    assert!(
        report["checks"][0]["why"]
            .as_str()
            .is_some_and(|why| why.starts_with("its key can't be told here")),
        "{report:#}"
    );
    assert_eq!(
        report["checks"][1]["why"],
        "a `cd` before it moves it where hushrun can't follow"
    );
}

/// \[ADR 0050\] C10: a command CI fills in has a key only its run can build, so it stays with CI.
// covers: C-22
#[test]
fn a_command_ci_fills_in_stays_with_ci() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  api:\n    runs-on: ubuntu-latest\n    defaults:\n      run:\n        working-directory: api\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api test -- sh checks/test.sh ${{ vars.LEVEL }}\n",
    );
    git::commit_all(&r.repo, "a command CI fills in");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("test", "ci")]), "{report:#}");
    assert_eq!(
        report["checks"][0]["why"], "its command takes values only CI knows",
        "{report:#}"
    );
}

/// \[ADR 0050\] ready reads the composite actions of the repository a job uses: a check through
/// hushrun there is the job's, with what the job needs, and a pass of the job counts.
// covers: C-22
#[test]
fn ready_reads_a_check_inside_a_composite_action() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  api:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - uses: ./.github/actions/api-tests\n",
    );
    r.write(
        ".github/actions/api-tests/action.yml",
        "runs:\n  using: composite\n  steps:\n    - run: hushrun run --scope api test -- sh checks/test.sh\n      shell: bash\n      working-directory: api\n",
    );
    git::commit_all(&r.repo, "a check in a composite action");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/test", "missing")]),
        "{report:#}"
    );
    let out = r.in_ci(
        "api",
        &[
            "run",
            "--scope",
            "api",
            "test",
            "--",
            "sh",
            "checks/test.sh",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/test", "passed")]),
        "{report:#}"
    );
}

const IN_A_MATRIX: &str = "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, macos-latest]\n    runs-on: ${{ matrix.os }}\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n";

/// What ready says of `check`.
fn why(report: &Value, check: &str) -> String {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|item| item["check"] == check)
        .and_then(|item| item["why"].as_str())
        .unwrap_or_default()
        .to_string()
}

// covers: C-22, C-23
#[test]
fn ready_doesnt_say_ci_skips_a_job_with_variants() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    r.write(".github/workflows/ci.yml", IN_A_MATRIX);
    git::commit_all(&r.repo, "a matrix");
    // Before and after a pass: a pass here changes nothing.
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "ci")]));
    assert!(r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "ci")]));
    assert_eq!(
        why(&report, "unit"),
        "CI runs it every time: job test of .github/workflows/ci.yml has several variants, and its HUSHRUN_VARIANT isn't ${{ toJSON(matrix) }}. To skip there too, run hushrun init",
        "{report:#}"
    );
    // The hook doesn't say otherwise, and doesn't hold an agent back for it.
    let pushed = r.push("feature", &[("CLAUDECODE", "1")]);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    assert!(!stderr(&pushed).contains("CI skips"), "{}", stderr(&pushed));
    // [ADR 0049] A job that says which variant each run is still keeps the check with CI: a laptop
    // can't know a variant's values, and its pass doesn't count there.
    r.write(
        ".github/workflows/ci.yml",
        &IN_A_MATRIX.replace(
            "    runs-on: ${{ matrix.os }}\n",
            "    runs-on: ${{ matrix.os }}\n    env:\n      HUSHRUN_VARIANT: ${{ toJSON(matrix) }}\n",
        ),
    );
    git::commit_all(&r.repo, "each variant says which it is");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "ci")]));
    assert_eq!(
        why(&report, "unit"),
        "CI runs each variant until that variant passes on this code",
        "{report:#}"
    );
}

/// Finding 5: the gate that keeps an agent from pushing what it knows fails holds in a matrix too.
// covers: C-22, C-23
#[test]
fn what_failed_here_is_still_to_fix_in_a_job_with_variants() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    r.write(".github/workflows/ci.yml", IN_A_MATRIX);
    r.write("checks/test.sh", "exit 1\n");
    git::commit_all(&r.repo, "a matrix, and a check that fails");
    assert!(!r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "missing")]));
    assert_eq!(report["ready"], false);
    assert_eq!(
        why(&report, "unit"),
        "it failed on this exact code last time"
    );
    let pushed = r.push("feature", &[("CLAUDECODE", "1")]);
    assert_eq!(pushed.status.code(), Some(1), "{}", stderr(&pushed));
    assert!(
        stderr(&pushed).contains("push refused"),
        "{}",
        stderr(&pushed)
    );
}

/// A pass from another CI job isn't one CI takes, so ready doesn't call it `passed`.
// covers: C-22
#[test]
fn another_ci_jobs_pass_isnt_passed() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let workflow = |job: &str| {
        format!(
            "on: push\njobs:\n  {job}:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n"
        )
    };
    r.write(".github/workflows/ci.yml", &workflow("test"));
    git::commit_all(&r.repo, "one job");
    let in_job = |job: &str| {
        let mut cmd = Command::new("npm");
        r.env(&mut cmd);
        cmd.env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", "acme/shop")
            .env(
                "GITHUB_WORKFLOW_REF",
                "acme/shop/.github/workflows/ci.yml@refs/heads/main",
            )
            .env("GITHUB_JOB", job)
            .args(["run", "test"])
            .output()
            .expect("npm")
    };
    // The job that runs it: its pass is what CI finds.
    let ran = in_job("test");
    assert!(ran.status.success(), "{}", stderr(&ran));
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "passed")]));
    // Another job's pass: CI would run it, and a run here would skip on that pass, so it isn't
    // `missing`, which would ask for a command that changes nothing, for ever.
    assert!(in_job("other").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "ci")]));
    assert_eq!(
        why(&report, "unit"),
        "it passed in another CI job, and a CI pass counts in its own job"
    );
}

/// The second review's third finding: the hook said `CI skips unit` of a check a matrix job starts
/// through `npm test`.
// covers: C-22, C-23
#[test]
fn ready_follows_a_job_through_the_scripts_it_starts() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    r.write(".github/workflows/ci.yml", IN_A_MATRIX);
    r.write(
        "package.json",
        "{\n  \"scripts\": {\n    \"test\": \"npm run all\",\n    \"all\": \"hushrun run unit -- sh checks/test.sh\"\n  }\n}\n",
    );
    git::commit_all(&r.repo, "a matrix, and a script behind a script");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "ci")]));
    assert!(r.npm("all").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "ci")]));
    assert!(
        why(&report, "unit").starts_with("CI runs it every time: job test of "),
        "{report:#}"
    );
    let pushed = r.push("feature", &[("CLAUDECODE", "1")]);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    assert!(!stderr(&pushed).contains("CI skips"), "{}", stderr(&pushed));
}

/// Following what doctor says on a wrapped script. CI's run has a key of its own in each variant,
/// and a pass of the script here is no pass of that, so `ready` doesn't ask for one, and the hook
/// doesn't say CI skips it.
// covers: C-22, C-23
#[test]
fn a_script_inside_a_named_variant_isnt_asked_for() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    r.write(
        ".github/workflows/ci.yml",
        &IN_A_MATRIX.replace(
            "npm test",
            "hushrun run t-${{ strategy.job-index }} -- npm test",
        ),
    );
    r.write(
        "package.json",
        "{\n  \"scripts\": {\n    \"test\": \"npm run all\",\n    \"all\": \"hushrun run unit -- sh checks/test.sh\"\n  }\n}\n",
    );
    git::commit_all(&r.repo, "each variant named");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("job test", "ci")]));
    assert!(r.npm("all").status.success());
    let pushed = r.push("feature", &[("CLAUDECODE", "1")]);
    assert!(pushed.status.success(), "{}", stderr(&pushed));
    assert!(!stderr(&pushed).contains("CI skips"), "{}", stderr(&pushed));
}

/// One run that two jobs start: all of those say what CI does.
// covers: C-22
#[test]
fn every_job_that_starts_a_run_has_its_say() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let line = "    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh checks/test.sh\n";
    r.write(
        ".github/workflows/ci.yml",
        &format!("on: push\njobs:\n  a:\n{line}  b:\n{line}"),
    );
    git::commit_all(&r.repo, "two jobs");
    let ran = r.in_job("a", ".", &["run", "unit", "--", "sh", "checks/test.sh"]);
    assert!(ran.status.success(), "{}", stderr(&ran));
    // Job b won't take a's pass.
    let (_, report) = ready(&r, &[]);
    assert_eq!(
        why(&report, "unit"),
        "it passed in another CI job, and a CI pass counts in its own job"
    );
}

/// One run in a plain job and in a job with variants: the second one's what CI does.
// covers: C-22
#[test]
fn a_job_with_variants_beside_a_plain_job_has_its_say() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let steps = "    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh checks/test.sh\n";
    r.write(
        ".github/workflows/ci.yml",
        &format!(
            "on: push\njobs:\n  a:\n{steps}  b:\n    strategy:\n      matrix:\n        node: [20, 25]\n{steps}"
        ),
    );
    git::commit_all(&r.repo, "a plain job and a matrix");
    let (_, report) = ready(&r, &[]);
    assert!(
        why(&report, "unit").contains("job b of .github/workflows/ci.yml"),
        "{report:#}"
    );
}

/// Staging and production with one job id share a pass, so ready calls staging's `passed`.
// covers: C-22
#[test]
fn the_same_jobs_pass_in_another_workflow_is_passed() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let workflow = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n";
    r.write(".github/workflows/deploy-staging.yml", workflow);
    r.write(".github/workflows/deploy-production.yml", workflow);
    git::commit_all(&r.repo, "two workflows");
    let mut cmd = Command::new("npm");
    r.env(&mut cmd);
    let ran = cmd
        .env("CI", "true")
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REPOSITORY", "acme/shop")
        .env(
            "GITHUB_WORKFLOW_REF",
            "acme/shop/.github/workflows/deploy-staging.yml@refs/heads/develop",
        )
        .env("GITHUB_JOB", "test")
        .args(["run", "test"])
        .output()
        .expect("npm");
    assert!(ran.status.success(), "{}", stderr(&ran));
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("unit", "passed")]));
}

// covers: C-22
#[test]
fn a_workflow_ci_wouldnt_run_for_this_push_says_nothing() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    let jobs = "jobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api test -- sh checks/test.sh\n";
    let with = |on: &str| {
        r.write(".github/workflows/ci.yml", &format!("{on}\n{jobs}"));
        git::commit_all(&r.repo, on);
        ready(&r, &["--branch", "fix/cart"])
    };
    for on in [
        "on:\n  schedule:\n    - cron: \"17 3 * * 0\"\n  workflow_dispatch:",
        "on:\n  push:\n    branches: [main]",
        "on:\n  push:\n    branches:\n      - main\n      - 'release/*'",
        "on:\n  push:\n    tags: ['v*']",
        "on:\n  push:\n    branches-ignore: ['fix/**']",
        "on: workflow_dispatch",
    ] {
        let (code, report) = with(on);
        assert_eq!(code, 0, "{on}: {report:#}");
        assert_eq!(report["checks"], json!([]), "{on}: {report:#}");
    }
    for on in [
        "on: push",
        "on: [pull_request]",
        "on:\n  push:\n    branches: ['fix/*']",
        "on:\n  push:\n    branches: [main]\n  pull_request:",
        "on:\n  push:\n    branches-ignore: [main]",
    ] {
        let (code, report) = with(on);
        assert_eq!(code, 10, "{on}: {report:#}");
        assert_eq!(states(&report), pairs(&[("api/test", "missing")]), "{on}");
    }
    // Whether it runs depends on what changed: nothing hushrun should stop a push for.
    let (code, report) = with("on:\n  push:\n    paths: ['api/**']");
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        report["checks"][0]["why"],
        "its workflow runs only for some paths"
    );
}

// covers: C-22
#[test]
fn gitlabs_pipeline_names_checks_too() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    std::fs::remove_file(r.repo.join(".github/workflows/ci.yml")).expect("remove");
    r.write(
        ".gitlab-ci.yml",
        "stages: [test]\n\n.template:\n  script:\n    - hushrun run --scope api hidden -- sh checks/test.sh\n\nunit:\n  stage: test\n  script:\n    - cd api\n    - hushrun run --scope api unit -- sh checks/test.sh\n\nlint:\n  image: rust:1.90\n  script:\n    - cd web\n    - |\n      hushrun run --scope web lint -- sh checks/test.sh\n\nflaky:\n  rules:\n    - if: $CI_COMMIT_BRANCH == \"main\"\n  script:\n    - hushrun run --scope api flaky -- sh checks/test.sh\n",
    );
    git::commit_all(&r.repo, "gitlab");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[
            ("api/unit", "missing"),
            ("web/lint", "ci"),
            ("api/flaky", "ci"),
        ])
    );
    assert_eq!(report["checks"][1]["why"], "its job runs in an image");
    assert_eq!(report["checks"][2]["why"], "its job has rules");
    assert_eq!(
        report["next"],
        json!(["cd api && hushrun run --scope api unit -- sh checks/test.sh"])
    );
}

// covers: C-22
#[test]
fn a_repository_that_checks_itself_out_elsewhere_counts_from_there() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    defaults:\n      run:\n        working-directory: shop/api\n    steps:\n      - uses: actions/checkout@v5\n        with:\n          path: shop\n      - uses: actions/checkout@v5\n        with:\n          repository: acme/cli\n          path: cli\n      - run: hushrun run --scope api test -- sh checks/test.sh\n      - run: hushrun run --scope api build -- sh checks/test.sh\n        working-directory: cli\n",
    );
    git::commit_all(&r.repo, "elsewhere");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/test", "missing"), ("build", "ci")])
    );
    assert_eq!(
        report["next"],
        json!(["cd api && hushrun run --scope api test -- sh checks/test.sh"])
    );
    assert_eq!(
        report["checks"][1]["why"],
        "its working directory is outside the repository's checkout"
    );
}

/// A repository whose scope `api` declares the checkout beside it, `../cli`.
fn with_a_sibling(fake: &FakeResults) -> (Repo, PathBuf) {
    let r = in_two_scopes(&fake.url());
    let cli = r.root.join("cli");
    std::fs::create_dir_all(&cli).expect("mkdir");
    git::init(&cli, "sha1");
    std::fs::write(cli.join("core.txt"), "one\n").expect("write");
    git::commit_all(&cli, "init");
    r.write(
        "hushrun.yaml",
        &SCOPES.replace("  web:", "    external:\n      - path: ../cli\n  web:"),
    );
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api test -- sh api/checks/test.sh\n",
    );
    git::commit_all(&r.repo, "sibling");
    (r, cli)
}

// covers: C-22
#[test]
fn ready_looks_a_check_up_with_what_its_scope_builds_against() {
    let fake = FakeResults::start();
    let (r, cli) = with_a_sibling(&fake);
    r.installed();
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    let line = report["next"][0].as_str().expect("a command").to_string();
    let out = r.sh(&line);
    assert!(out.status.success(), "{}", stderr(&out));
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("api/test", "passed")]));
    assert!(r.push("fix", AGENT).status.success());

    // A change beside the repository, with nothing changed in it, is new code.
    std::fs::write(cli.join("core.txt"), "two\n").expect("write");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(states(&report), pairs(&[("api/test", "missing")]));
    let out = r.push("fix2", AGENT);
    assert!(!out.status.success(), "{}", stderr(&out));
    assert!(r.sh(&line).status.success());
    assert!(r.push("fix2", AGENT).status.success());

    // Without it, hushrun can't tell: a run wouldn't record, so nothing is asked for.
    std::fs::rename(&cli, r.root.join("moved")).expect("rename");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("test", "ci")]));
    assert!(
        report["checks"][0]["why"]
            .as_str()
            .is_some_and(|why| why.contains("the external input ../cli")),
        "{report:#}"
    );
}

// covers: C-12, C-22
#[test]
fn a_repository_that_reuses_only_cis_results_asks_for_nothing_here() {
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.installed();
    fake.set_ci_reuses_only_ci("github.com/acme/shop");

    // A pass here wouldn't change what CI runs: an agent has nothing to do, and its push goes.
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["next"], json!([]), "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/test", "ci"), ("web/lint", "ci")])
    );
    assert_eq!(
        report["checks"][0]["why"],
        "this repository's CI reuses only CI's results, so a pass here wouldn't change what it runs"
    );
    let out = r.push("fix", AGENT);
    assert!(out.status.success(), "{}", stderr(&out));

    // A laptop's pass isn't a reason to call it passed: CI would still run it.
    let line = "cd api && hushrun run --scope api test -- sh checks/test.sh";
    assert!(r.sh(line).status.success());
    let (_, report) = ready(&r, &[]);
    assert_eq!(report["checks"][0]["state"], "ci", "{report:#}");

    // CI's own pass is.
    let out = r.in_ci(
        "api",
        &[
            "run",
            "--force",
            "--scope",
            "api",
            "test",
            "--",
            "sh",
            "checks/test.sh",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let (_, report) = ready(&r, &[]);
    assert_eq!(report["checks"][0]["state"], "passed", "{report:#}");
}

// covers: C-22, C-14
#[test]
fn a_check_that_writes_files_doesnt_trap_an_agent() {
    // It passes, and changes files, so nothing is recorded, every time: asking again never ends.
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api test -- sh api/checks/test.sh\n",
    );
    r.write("api/checks/test.sh", "date > api/out.txt\nexit 0\n");
    git::commit_all(&r.repo, "writes");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    let out = r.sh(report["next"][0].as_str().expect("a command"));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("files changed while it ran"),
        "{}",
        stderr(&out)
    );
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(states(&report), pairs(&[("api/test", "ci")]));
}

// covers: C-22
#[test]
fn a_tool_this_machine_doesnt_have_stays_with_ci() {
    // It can't start, so nothing would be recorded, and asking again would never end.
    let fake = FakeResults::start();
    let r = in_two_scopes(&fake.url());
    r.write(
        ".github/workflows/ci.yml",
        "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run --scope api a -- a-tool-nobody-has --check\n      - run: hushrun run --scope api b -- ./mvnw verify\n      - run: hushrun run --scope api c -- sh api/checks/test.sh\n",
    );
    git::commit_all(&r.repo, "tools");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("api/a", "ci"), ("api/b", "ci"), ("api/c", "missing")])
    );
    assert_eq!(
        report["checks"][0]["why"],
        "a-tool-nobody-has isn't installed here"
    );
    // A wrapper in the repository counts, from the directory the run starts in.
    r.write("mvnw", "exit 0\n");
    let (_, report) = ready(&r, &[]);
    assert_eq!(report["checks"][1]["state"], "missing", "{report:#}");
}

// covers: C-22
#[test]
fn a_scripts_tool_the_install_didnt_bring_stays_with_ci() {
    // A repository may keep a `lint` for a tool it never added: its push must not wait for it.
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.write(
        "package.json",
        "{\n  \"scripts\": {\n    \"lint\": \"hushrun run -- a-tool-nobody-has .\",\n    \"test\": \"hushrun run unit -- sh checks/test.sh\"\n  }\n}\n",
    );
    git::commit_all(&r.repo, "scripts");

    // Nothing is installed yet, or Yarn's Plug'n'Play holds it: nothing says it's missing.
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("lint", "missing"), ("unit", "missing")])
    );

    r.write("node_modules/.bin/another-tool", "exit 0\n");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(
        states(&report),
        pairs(&[("lint", "ci"), ("unit", "missing")])
    );
    assert_eq!(
        report["checks"][0]["why"],
        "a-tool-nobody-has isn't installed here"
    );
    assert_eq!(report["next"], json!(["npm run test"]));

    // The other check passes, and nothing is left to stop a push.
    assert!(r.npm("test").status.success());
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 0, "{report:#}");

    // Installed, it runs like any other.
    r.write("node_modules/.bin/a-tool-nobody-has", "exit 0\n");
    let (code, report) = ready(&r, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(report["checks"][0]["state"], "missing", "{report:#}");
}

/// A program the repository keeps where the hook's lines look: it leaves a file, and lets a push
/// through.
fn a_trap(r: &Repo, path: &str) {
    r.write(path, "#!/bin/sh\ntouch started-by-the-hook\nexit 0\n");
    std::fs::set_permissions(r.repo.join(path), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
}

// covers: C-23
#[test]
fn a_push_doesnt_start_a_hushrun_the_repository_keeps() {
    for path in [
        "node_modules/.bin/hushrun",
        "Node_Modules/.bin/hushrun",
        "vendor/.bin/hushrun",
    ] {
        let fake = FakeResults::start();
        let r = Repo::new(&fake.url());
        r.write(".gitignore", "");
        a_trap(&r, path);
        if path.starts_with("vendor") {
            std::os::unix::fs::symlink("vendor", r.repo.join("node_modules")).expect("link");
        }
        git::commit_all(&r.repo, "a program of its own");
        // The first `hushrun ready` in a clone installs the hook.
        let out = r.hushrun(&["ready"]);
        assert!(
            stderr(&out).contains("installed the pre-push hook"),
            "{}",
            stderr(&out)
        );
        // PATH's hushrun refuses an agent's push, so the push says which one ran.
        let out = r.push("main", AGENT);
        assert!(
            stderr(&out).contains("push refused"),
            "{path}: {}",
            stderr(&out)
        );
        assert!(
            !r.repo.join("started-by-the-hook").exists(),
            "{path}: git push started the repository's own program"
        );
    }
    // An install stays what it was: it is ignored, and the local one runs.
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    r.installed();
    a_trap(&r, "node_modules/.bin/hushrun");
    let out = r.push("main", AGENT);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(r.repo.join("started-by-the-hook").exists());
}

// covers: C-23
#[test]
fn a_clones_older_lines_are_replaced_by_an_install_and_by_nothing_else() {
    let fake = FakeResults::start();
    let r = Repo::new(&fake.url());
    let old = "#!/bin/sh\n# hushrun:start (an older hushrun)\nhushrun=node_modules/.bin/hushrun\n[ -x \"$hushrun\" ] || hushrun=hushrun\n\"$hushrun\" hook pre-push \"$@\" || [ \"$?\" -ne 10 ] || exit 1\n# hushrun:end\n";
    std::fs::create_dir_all(r.hook().parent().expect("parent")).expect("mkdir");
    std::fs::write(r.hook(), old).expect("write");
    std::fs::set_permissions(r.hook(), std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let out = r.hushrun(&["ready"]);
    assert!(!stderr(&out).contains("installed"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(r.hook()).expect("hook"), old);
    r.installed();
    let text = std::fs::read_to_string(r.hook()).expect("hook");
    assert!(text.contains(":(icase)node_modules"), "{text}");
    assert_eq!(text.matches("# hushrun:start").count(), 1, "{text}");
}
