//! `hushrun run [NAME] -- COMMAND` without `--stage`: it runs the command unless this exact
//! code already passed it, needs no configuration, and runs the command whenever it can't tell.

use std::path::{Path, PathBuf};

use hushrun_testkit::{git, schema};
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
    "HUSHRUN_NO_CI_OUTPUT",
    "HUSHRUN_GIT_READER",
    "HUSHRUN_LOG",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_RUN_ID",
    "HUSHRUN_RUN_ATTEMPT",
    "HUSHRUN_RUN_URL",
    "HUSHRUN_PLAN_JSON",
    "npm_lifecycle_event",
];

/// Where the hosted backend would be: nothing listens on the discard port, so no test reaches
/// the real one.
const NO_HOST: &str = "http://127.0.0.1:9";

/// A repository without hushrun.yaml, and a cache and credentials of its own: a `hushrun
/// login` on the machine running the tests must not reach them.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let r = Repo {
            _dir: dir,
            root,
            repo,
        };
        r.write("src/lib.txt", "one\n");
        r.write(".gitignore", "out/\n");
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }

    /// `hushrun` in `dir`, as a local run with no inherited configuration. Commands it wraps
    /// run git with a fixed identity, as the test kit's git does.
    fn hushrun_in(&self, dir: &Path) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").expect("binary");
        cmd.current_dir(dir)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_AUTHOR_NAME", "hushrun tests")
            .env("GIT_AUTHOR_EMAIL", "tests@hushrun.invalid")
            .env("GIT_COMMITTER_NAME", "hushrun tests")
            .env("GIT_COMMITTER_EMAIL", "tests@hushrun.invalid");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd
    }

    fn hushrun(&self) -> assert_cmd::Command {
        self.hushrun_in(&self.repo)
    }

    /// `hushrun run unit -- git --version`: it prints git's version only when it runs.
    fn version(&self) -> Ran {
        ran(self.hushrun(), &["run", "unit", "--", "git", "--version"])
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn ran(&self) -> bool {
        self.stdout.contains("git version")
    }
}

fn ran(mut cmd: assert_cmd::Command, args: &[&str]) -> Ran {
    let output = cmd.args(args).output().expect("hushrun");
    Ran {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

// covers: C-10
#[test]
fn a_second_run_on_the_same_code_skips() {
    let r = Repo::new();
    let first = r.version();
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.ran());
    assert!(
        first
            .stderr
            .contains("unit running: no pass yet for this exact code"),
        "{}",
        first.stderr
    );
    assert!(
        first.stderr.contains("; it won't run again on this code"),
        "{}",
        first.stderr
    );

    let second = r.version();
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(!second.ran(), "it ran again: {}", second.stdout);
    assert!(second.stdout.is_empty());
    assert!(
        second
            .stderr
            .contains("unit skipped: this exact code passed just now (saves "),
        "{}",
        second.stderr
    );
}

// covers: C-9
#[test]
fn the_files_decide_not_the_commit() {
    let r = Repo::new();
    assert!(r.version().ran());

    // A change runs it again.
    r.write("src/lib.txt", "two\n");
    assert!(r.version().ran());
    // Committing the same files changes nothing: that content already passed.
    git::commit_all(&r.repo, "two");
    assert!(!r.version().ran());
    // Going back to content that passed skips too.
    r.write("src/lib.txt", "one\n");
    assert!(!r.version().ran());
}

// covers: C-9
#[test]
fn untracked_files_count_and_ignored_ones_dont() {
    let r = Repo::new();
    assert!(r.version().ran());
    r.write("out/build.log", "ignored\n");
    assert!(!r.version().ran());
    r.write("notes.txt", "untracked\n");
    assert!(r.version().ran());
}

// covers: C-10
#[test]
fn a_failure_never_skips() {
    let r = Repo::new();
    let args = ["run", "unit", "--", "git", "no-such-command"];
    let first = ran(r.hushrun(), &args);
    assert_eq!(first.code, 1, "{}", first.stderr);
    assert!(first.stderr.contains("unit failed in"), "{}", first.stderr);
    assert!(first.stderr.contains("(exit 1)"), "{}", first.stderr);

    let again = ran(r.hushrun(), &args);
    assert_eq!(again.code, 1, "{}", again.stderr);
    assert!(
        again
            .stderr
            .contains("unit running: it failed on this exact code last time"),
        "{}",
        again.stderr
    );
}

// covers: C-9
#[test]
fn another_command_name_or_directory_is_another_result() {
    let r = Repo::new();
    assert!(r.version().ran());
    // A partial or different run never stands in for the full one.
    let other_command = ran(
        r.hushrun(),
        &["run", "unit", "--", "git", "--version", "--build-options"],
    );
    assert!(other_command.ran(), "{}", other_command.stderr);
    let other_name = ran(r.hushrun(), &["run", "lint", "--", "git", "--version"]);
    assert!(other_name.ran(), "{}", other_name.stderr);

    let src = r.repo.join("src");
    let elsewhere = ran(
        r.hushrun_in(&src),
        &["run", "unit", "--", "git", "--version"],
    );
    assert!(elsewhere.ran(), "{}", elsewhere.stderr);
    let again = ran(
        r.hushrun_in(&src),
        &["run", "unit", "--", "git", "--version"],
    );
    assert!(!again.ran(), "{}", again.stderr);
}

// covers: C-14
#[test]
fn force_runs_it_anyway() {
    let r = Repo::new();
    assert!(r.version().ran());
    let forced = ran(
        r.hushrun(),
        &["run", "unit", "--force", "--", "git", "--version"],
    );
    assert!(forced.ran(), "{}", forced.stderr);
    assert!(
        forced.stderr.contains("unit running (--force)"),
        "{}",
        forced.stderr
    );
}

// covers: C-14
#[test]
fn a_run_that_changes_the_files_isnt_recorded() {
    let r = Repo::new();
    // `git stash` puts the worktree back to the last commit while it runs.
    r.write("src/lib.txt", "dirty\n");
    let args = ["run", "unit", "--", "git", "stash"];
    let first = ran(r.hushrun(), &args);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stderr.contains("unit passed in"), "{}", first.stderr);
    assert!(
        first
            .stderr
            .contains("but files changed while it ran, so it wasn't recorded"),
        "{}",
        first.stderr
    );

    // The same dirty files have no pass.
    r.write("src/lib.txt", "dirty\n");
    let again = ran(r.hushrun(), &args);
    assert!(
        again
            .stderr
            .contains("unit running: no pass yet for this exact code"),
        "{}",
        again.stderr
    );
}

// covers: C-14
#[test]
fn outside_a_repository_it_just_runs() {
    let r = Repo::new();
    let plain = r.root.join("plain");
    std::fs::create_dir_all(&plain).expect("mkdir");
    for _ in 0..2 {
        let run = ran(
            r.hushrun_in(&plain),
            &["run", "unit", "--", "git", "--version"],
        );
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(run.ran(), "{}", run.stderr);
        assert!(
            run.stderr
                .contains("unit running without skipping: can't tell which code this is"),
            "{}",
            run.stderr
        );
    }
}

// covers: C-14
#[test]
fn exit_codes_pass_through() {
    let r = Repo::new();
    let usage = ran(
        r.hushrun(),
        &[
            "run",
            "unit",
            "--",
            env!("CARGO_BIN_EXE_hushrun"),
            "--no-such-flag",
        ],
    );
    assert_eq!(usage.code, 2, "{}", usage.stderr);

    let missing = ran(
        r.hushrun(),
        &["run", "unit", "--", "hushrun-tests-no-such-program"],
    );
    assert_eq!(missing.code, 127, "{}", missing.stderr);
    assert!(
        missing
            .stderr
            .contains("hushrun: can't start hushrun-tests-no-such-program"),
        "{}",
        missing.stderr
    );
}

/// On Windows a name finds npm's `.cmd` shims, as a shell would: `"test": "hushrun run unit --
/// vitest run"` in package.json starts `node_modules\.bin\vitest.cmd`.
#[cfg(windows)]
// covers: C-14
#[test]
fn a_cmd_shim_runs_by_its_name() {
    let r = Repo::new();
    r.write(".gitignore", "out/\nnode_modules/\n");
    r.write("node_modules/.bin/vitest.cmd", "@echo vitest ran: %*\r\n");
    // npm puts node_modules\.bin first on PATH for a script.
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(r.repo.join("node_modules").join(".bin"))
            .chain(std::env::split_paths(&inherited)),
    )
    .expect("PATH");
    let vitest = || {
        let mut cmd = r.hushrun();
        cmd.env("PATH", &path);
        ran(cmd, &["run", "unit", "--", "vitest", "run"])
    };

    let first = vitest();
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains("vitest ran: run"), "{}", first.stdout);

    let second = vitest();
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(second.stdout.is_empty(), "it ran again: {}", second.stdout);
    assert!(
        second
            .stderr
            .contains("unit skipped: this exact code passed just now"),
        "{}",
        second.stderr
    );
}

// covers: C-10, C-11
#[test]
fn a_skip_is_kept_apart_with_the_time_it_saved() {
    let r = Repo::new();
    git::run(
        &r.repo,
        &["remote", "add", "origin", "git@github.com:Acme/shop.git"],
        None,
    );
    // Without a server, results stay in this machine's cache.
    let shared = r.root.join("cache").join("results");
    for _ in 0..2 {
        ran(r.hushrun(), &["run", "unit", "--", "git", "--version"]);
    }
    let only = |dir: PathBuf| -> Value {
        let key = std::fs::read_dir(dir)
            .expect("dir")
            .next()
            .expect("a key")
            .expect("entry");
        let file = std::fs::read_dir(key.path())
            .expect("key")
            .next()
            .expect("one")
            .expect("entry");
        serde_json::from_slice(&std::fs::read(file.path()).expect("read")).expect("JSON")
    };
    let pass = only(shared.join("results").join("v0"));
    let skip = only(shared.join("skips").join("v0"));
    let schema = schema::load("run-result.v0.schema.json");
    schema::assert_valid(&schema, &pass);
    schema::assert_valid(&schema, &skip);
    assert_eq!(pass["result"], "pass");
    assert_eq!(pass["repository"], "github.com/Acme/shop");
    assert_eq!(skip["result"], "skip");
    assert_eq!(skip["savedMs"], pass["durationMs"]);
    assert_eq!(skip["repository"], "github.com/Acme/shop");
    assert!(skip.get("exitCode").is_some_and(Value::is_null), "{skip}");

    // A skip never answers a lookup: the pass still does.
    assert!(!r.version().ran());
}

// covers: C-10, C-11
#[test]
fn results_stay_on_this_machine_without_a_server_or_turn_off() {
    let r = Repo::new();
    assert!(r.version().ran());

    // What it recorded, in this machine's cache.
    let results = r.root.join("cache/results/results/v0");
    let key_dir = std::fs::read_dir(&results)
        .expect("results")
        .next()
        .expect("one key")
        .expect("entry")
        .path();
    let file = std::fs::read_dir(&key_dir)
        .expect("key dir")
        .next()
        .expect("one result")
        .expect("entry")
        .path();
    let result: Value = serde_json::from_slice(&std::fs::read(file).expect("read")).expect("json");
    schema::assert_valid(&schema::load("run-result.v0.schema.json"), &result);
    assert_eq!(result["schema"], "hushrun.run-result/v0");
    assert_eq!(result["check"], "unit");
    assert_eq!(result["result"], "pass");
    assert_eq!(result["exitCode"], 0);
    assert_eq!(result["command"], serde_json::json!(["git", "--version"]));
    assert_eq!(result["cwd"], "");
    assert_eq!(result["scope"], "default");
    assert_eq!(result["provider"], "local");
    assert!(
        result["digest"]
            .as_str()
            .is_some_and(|d| d.starts_with("sha256:"))
    );
    assert!(result["durationMs"].is_u64());

    // This machine skips; another machine, here another cache, runs: only a server shares.
    assert!(!r.version().ran());
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_CACHE_DIR", r.root.join("other-cache"));
    assert!(ran(cmd, &["run", "unit", "--", "git", "--version"]).ran());

    // An empty HUSHRUN_BACKEND turns results off: it always runs.
    for _ in 0..2 {
        let mut cmd = r.hushrun();
        cmd.env("HUSHRUN_BACKEND", "");
        assert!(ran(cmd, &["run", "unit", "--", "git", "--version"]).ran());
    }

    // A backend that isn't an https:// server, such as a directory: it runs, and says why.
    for url in ["file:///tmp/results", "mem://results"] {
        let mut cmd = r.hushrun();
        cmd.env("HUSHRUN_BACKEND", url);
        let other = ran(cmd, &["run", "unit", "--", "git", "--version"]);
        assert!(other.ran(), "{}", other.stderr);
        assert!(
            other.stderr.contains("results are off (HUSHRUN_BACKEND")
                && other.stderr.contains("only https://"),
            "{}",
            other.stderr
        );
    }
}

/// [ADR 0060] `--rev` is the caller's word for the code COMMAND tests: one that tests a commit's
/// own export, as a release gate does, records and skips for that commit from a checkout that has
/// moved on, edits included.
// covers: C-9
#[test]
fn rev_names_the_code_a_command_tests_whatever_is_checked_out() {
    let r = Repo::new();
    let gated = git::text(&r.repo, &["rev-parse", "HEAD"]);
    r.write("src/lib.txt", "two\n");
    git::commit_all(&r.repo, "version");
    let gate = [
        "run",
        "gate",
        "--rev",
        gated.as_str(),
        "--",
        "git",
        "--version",
    ];
    let first = ran(r.hushrun(), &gate);
    assert!(first.ran(), "{}", first.stderr);
    assert!(
        first.stderr.contains("it won't run again on this code"),
        "{}",
        first.stderr
    );
    r.write("src/lib.txt", "three, not committed\n");
    let second = ran(r.hushrun(), &gate);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(!second.ran(), "{}", second.stderr);
    assert!(
        second
            .stderr
            .contains("gate skipped: this exact code passed"),
        "{}",
        second.stderr
    );
    // Without it, the code is the files here, which never passed.
    let here = ran(r.hushrun(), &["run", "gate", "--", "git", "--version"]);
    assert!(here.ran(), "{}", here.stderr);
}

/// [ADR 0050] A word that holds a `*_TOKEN` variable's value shows as `[redacted]` in the record,
/// and only there: the key is the command's own words, so it is the same with and without the
/// redaction.
// covers: C-9, C-10
#[test]
fn a_record_never_shows_a_secret_its_command_held() {
    let r = Repo::new();
    // C-9's example digest, so that the key can be worked out by hand.
    let digest = format!("sha256:{}", "1".repeat(64));
    assert_eq!(
        key_of(&["vitest", "run"], &digest),
        "1abcbeed3a0264f77f5da8fad0aa13880079d906e0708bc931cbca454b345425"
    );
    let secret = "not-a-real-secret-42";
    let flag = format!("x.token={secret}");
    let command = ["git", "-c", flag.as_str(), "--version", secret];
    let args: Vec<&str> = ["run", "unit", "--digest", digest.as_str(), "--"]
        .into_iter()
        .chain(command)
        .collect();
    let shown = ["git", "-c", "x.token=[redacted]", "--version", "[redacted]"];

    let mut cmd = r.hushrun();
    cmd.env("DEPLOY_TOKEN", secret);
    let first = ran(cmd, &args);
    assert!(first.ran(), "{}", first.stderr);
    assert!(first.stderr.contains("unit passed in"), "{}", first.stderr);
    let [(key, text)] = recorded(&r.root.join("cache/results/results/v0"))
        .try_into()
        .expect("one pass");
    assert!(!text.contains(secret), "{text}");
    let pass: Value = serde_json::from_str(&text).expect("JSON");
    schema::assert_valid(&schema::load("run-result.v0.schema.json"), &pass);
    assert_eq!(pass["command"], serde_json::json!(shown));
    // The key is C-9's, of the words as given, never of what the record shows.
    assert_eq!(key, key_of(&command, &digest));
    assert_ne!(key, key_of(&shown, &digest));

    // Without the variable, nothing says the word is a secret, so its record shows it as given;
    // its key, the same, finds the pass.
    let second = ran(r.hushrun(), &args);
    assert!(!second.ran(), "{}", second.stderr);
    assert!(
        second
            .stderr
            .contains("unit skipped: this exact code passed"),
        "{}",
        second.stderr
    );
    let [(same, text)] = recorded(&r.root.join("cache/results/skips/v0"))
        .try_into()
        .expect("one skip");
    assert_eq!(same, key);
    let skip: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(skip["command"], serde_json::json!(command));
}

/// Each result under `dir`, a directory backend's `results/v0` or `skips/v0`: its key, and its
/// text.
fn recorded(dir: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for key in std::fs::read_dir(dir).expect("results") {
        let key = key.expect("key").path();
        for file in std::fs::read_dir(&key).expect("key dir") {
            let text = std::fs::read_to_string(file.expect("entry").path()).expect("read");
            let name = key
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned();
            found.push((name, text));
        }
    }
    found
}

/// The SHA-256 of C-9's JCS form for `unit`'s `command` at the root, in the default scope, on
/// `digest`, by hand: keys sorted, no spaces.
fn key_of(command: &[&str], digest: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let command = serde_json::to_string(command).expect("JSON");
    let text = format!(
        r#"{{"check":"unit","command":{command},"cwd":"","digest":"{digest}","schema":"hushrun.run-key/v0","scope":"default"}}"#
    );
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// covers: C-11, C-14
#[test]
fn a_repositorys_backend_that_isnt_a_server_turns_results_off() {
    let r = Repo::new();
    r.write(
        "package.json",
        r#"{"name": "shop", "hushrun": {"backend": "file:///tmp/results"}}"#,
    );
    for _ in 0..2 {
        let run = r.version();
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(run.ran(), "{}", run.stderr);
        assert!(
            run.stderr
                .contains("results are off (package.json's hushrun.backend: ")
                && run.stderr.contains("only https://"),
            "{}",
            run.stderr
        );
    }
    // HUSHRUN_BACKEND comes first: an empty one turns results off without a word on the file.
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_BACKEND", "");
    let off = ran(cmd, &["run", "unit", "--", "git", "--version"]);
    assert!(off.ran());
    assert!(!off.stderr.contains("package.json"), "{}", off.stderr);
}

// covers: C-9
#[test]
fn the_specs_example_key_is_the_key() {
    let r = Repo::new();
    // C-9's example: unit, `vitest run` at the root, in the default scope, on sha256:1…1.
    let digest = format!("sha256:{}", "1".repeat(64));
    let key = "1abcbeed3a0264f77f5da8fad0aa13880079d906e0708bc931cbca454b345425";
    let dir = r.root.join("cache/results/results/v0").join(key);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let pass = serde_json::json!({
        "schema": "hushrun.run-result/v0",
        "check": "unit",
        "result": "pass",
        "exitCode": 0,
        "command": ["vitest", "run"],
        "cwd": "",
        "scope": "default",
        "digest": digest,
        "startedAt": "2026-10-16T14:05:10.123Z",
        "durationMs": 41000,
        "provider": "github",
        "tool": "hushrun 0.1.0",
    });
    std::fs::write(
        dir.join("01JA0000000000000000000000.json"),
        pass.to_string(),
    )
    .expect("write");

    // vitest isn't installed here, so only a skip exits 0.
    let run = ran(
        r.hushrun(),
        &["run", "unit", "--digest", &digest, "--", "vitest", "run"],
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("unit skipped: this exact code passed in CI"),
        "{}",
        run.stderr
    );
}

// covers: C-11 (ADR 0067: in CI with no server, results are off, and the run says so once)
#[test]
fn in_ci_results_never_stay_on_the_runner() {
    let r = Repo::new();
    for _ in 0..2 {
        let mut ci = r.hushrun();
        ci.env("CI", "true");
        let run = ran(ci, &["run", "unit", "--", "git", "--version"]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(run.ran(), "{}", run.stderr);
        let lines: Vec<&str> = run
            .stderr
            .lines()
            .filter(|line| line.starts_with("hushrun: "))
            .collect();
        assert_eq!(
            lines,
            [
                "hushrun: unit running without skipping: results are off: in CI, nothing names a backend, and a runner's own directory isn't shared: run hushrun init to share results with CI, or set HUSHRUN_BACKEND"
            ],
            "{}",
            run.stderr
        );
    }
    assert!(!r.root.join("cache/results/results").exists());
    // `-q` silences it, as it does every line of hushrun's.
    let mut ci = r.hushrun();
    ci.env("CI", "true");
    let quiet = ran(ci, &["-q", "run", "unit", "--", "git", "--version"]);
    assert!(quiet.ran() && quiet.stderr.is_empty(), "{}", quiet.stderr);
    // Outside CI, the same machine keeps its results.
    assert!(r.version().ran());
    assert!(!r.version().ran());
}

// covers: C-9
#[test]
fn a_pass_in_ci_skips_on_a_laptop() {
    // [ADR 0067] They share a server: CI keeps nothing in a runner's directory.
    let fake = hushrun_testkit::fake_results::FakeResults::start();
    let shared = |mut cmd: assert_cmd::Command| {
        cmd.env("HUSHRUN_BACKEND", fake.url()).env(
            "HUSHRUN_TOKEN",
            hushrun_testkit::fake_results::FakeResults::TOKEN,
        );
        cmd
    };
    let r = Repo::new();
    let mut ci = shared(r.hushrun());
    ci.env("CI", "true")
        .env("HUSHRUN_RUN_URL", "https://ci.example/runs/7");
    assert!(ran(ci, &["run", "unit", "--", "git", "--version"]).ran());

    let laptop = ran(
        shared(r.hushrun()),
        &["run", "unit", "--", "git", "--version"],
    );
    assert!(!laptop.ran(), "{}", laptop.stderr);
    assert!(
        laptop
            .stderr
            .contains("unit skipped: this exact code passed in CI just now"),
        "{}",
        laptop.stderr
    );
}

// covers: C-14
#[test]
fn quiet_leaves_the_output_to_the_command() {
    let r = Repo::new();
    let quiet = ran(
        r.hushrun(),
        &["-q", "run", "unit", "--", "git", "--version"],
    );
    assert!(quiet.ran());
    assert!(quiet.stderr.is_empty(), "{}", quiet.stderr);
}

// covers: C-9, C-14
#[test]
fn usage() {
    let r = Repo::new();
    for (args, says) in [
        (
            &["run", "unit", "lint", "--", "git", "--version"][..],
            "name one check, then its command after --",
        ),
        (&["run", "unit"][..], "give unit's command after --"),
        (
            &["run"][..],
            "name what to run: `hushrun run NAME -- COMMAND`",
        ),
        (
            &["run", "--", "./123", "--version"][..],
            "name the check, as ./123 gives no name: hushrun run NAME -- ./123",
        ),
        (
            &["run", "unit", "--all", "--", "git", "--version"][..],
            "--all goes with hushrun.yaml's checks",
        ),
    ] {
        let run = ran(r.hushrun(), args);
        assert_eq!(run.code, 2, "{args:?}: {}", run.stderr);
        assert!(run.stderr.contains(says), "{args:?}: {}", run.stderr);
        assert!(!run.ran());
    }
}

/// The one result this machine's cache holds, of a pass or a fail.
fn only_result(r: &Repo) -> Value {
    let results = r.root.join("cache/results/results/v0");
    let mut files = Vec::new();
    for key in std::fs::read_dir(&results).expect("results") {
        for file in std::fs::read_dir(key.expect("key").path()).expect("key dir") {
            files.push(file.expect("entry").path());
        }
    }
    let [file] = files.as_slice() else {
        panic!("{files:?}");
    };
    serde_json::from_slice(&std::fs::read(file).expect("read")).expect("JSON")
}

// covers: C-9
#[test]
fn without_a_name_the_script_names_the_check() {
    let r = Repo::new();
    // npm, pnpm, yarn and bun name the script they run.
    let mut npm_test = r.hushrun();
    npm_test.env("npm_lifecycle_event", "test");
    let first = ran(npm_test, &["run", "--", "git", "--version"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.ran());
    assert!(
        first
            .stderr
            .contains("hushrun: test running: no pass yet for this exact code"),
        "{}",
        first.stderr
    );
    assert_eq!(only_result(&r)["check"], "test");

    // The name is part of the key, whichever way it was given.
    let named = ran(r.hushrun(), &["run", "test", "--", "git", "--version"]);
    assert!(!named.ran(), "{}", named.stderr);
    assert!(
        named
            .stderr
            .contains("test skipped: this exact code passed"),
        "{}",
        named.stderr
    );
}

// covers: C-9
#[test]
fn without_a_name_or_a_script_the_program_names_the_check() {
    let r = Repo::new();
    let first = ran(r.hushrun(), &["run", "--", "git", "--version"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(
        first
            .stderr
            .contains("hushrun: git running: no pass yet for this exact code"),
        "{}",
        first.stderr
    );
    assert_eq!(only_result(&r)["check"], "git");

    // npx, which runs what it starts as the script `npx`, and a script whose name leaves no
    // name, leave it to the program: the same check, which skips.
    for script in ["npx", "123"] {
        let mut cmd = r.hushrun();
        cmd.env("npm_lifecycle_event", script);
        let again = ran(cmd, &["run", "--", "git", "--version"]);
        assert!(!again.ran(), "{script}: {}", again.stderr);
        assert!(
            again
                .stderr
                .contains("hushrun: git skipped: this exact code passed"),
            "{script}: {}",
            again.stderr
        );
    }
}

// covers: C-9
#[test]
fn a_scripts_name_is_made_a_name() {
    let r = Repo::new();
    // `"test:unit": "hushrun run -- vitest run"`: the check is the script's, made a name.
    let mut unit = r.hushrun();
    unit.env("npm_lifecycle_event", "test:unit");
    let first = ran(unit, &["run", "--", "git", "--version"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.ran());
    assert!(
        first
            .stderr
            .contains("hushrun: test-unit running: no pass yet for this exact code"),
        "{}",
        first.stderr
    );
    assert_eq!(only_result(&r)["check"], "test-unit");
    // The name it gives is the one a person would write.
    let named = ran(r.hushrun(), &["run", "test-unit", "--", "git", "--version"]);
    assert!(!named.ran(), "{}", named.stderr);
    assert!(
        named
            .stderr
            .contains("hushrun: test-unit skipped: this exact code passed"),
        "{}",
        named.stderr
    );
    for (script, check) in [("format:check", "format-check"), ("Lint", "lint")] {
        let mut cmd = r.hushrun();
        cmd.env("npm_lifecycle_event", script);
        let run = ran(cmd, &["run", "--", "git", "--version"]);
        assert!(run.ran(), "{script}: {}", run.stderr);
        assert!(
            run.stderr.contains(&format!(
                "hushrun: {check} running: no pass yet for this exact code"
            )),
            "{script}: {}",
            run.stderr
        );
    }
}

// covers: C-14
#[test]
fn a_variable_hushrun_cant_read_never_stops_the_check() {
    let r = Repo::new();
    for (i, (vars, says)) in [
        // CI settings such as `HUSHRUN_NO_CI_OUTPUT: ${{ vars.X }}`: X set to 1, or unset.
        (&[("HUSHRUN_NO_CI_OUTPUT", "1")][..], None),
        (&[("HUSHRUN_NO_CI_OUTPUT", "")], None),
        (&[("HUSHRUN_FORMAT", "")], None),
        (
            &[("HUSHRUN_FORMAT", "xml")],
            Some(
                "hushrun: HUSHRUN_FORMAT is \"xml\", not text, json, env or dotenv, so it's ignored",
            ),
        ),
        (
            &[
                ("HUSHRUN_CONFIG", ""),
                ("HUSHRUN_GIT_READER", ""),
                ("HUSHRUN_LOG", ""),
            ],
            None,
        ),
        (
            &[("HUSHRUN_PROVIDER", "jenkins")],
            Some(
                "hushrun: HUSHRUN_PROVIDER is \"jenkins\", not github, gitlab, buildkite, circleci, generic or local, so it's ignored",
            ),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let with = |args: &[&str]| {
            let mut cmd = r.hushrun();
            cmd.envs(vars.iter().copied());
            ran(cmd, args)
        };
        // It runs the command, and exits with its code.
        let check = format!("unit-{i}");
        let passed = with(&["run", &check, "--", "git", "--version"]);
        assert_eq!(passed.code, 0, "{vars:?}: {}", passed.stderr);
        assert!(passed.ran(), "{vars:?}: {}", passed.stderr);
        // `git rev-parse --verify` of a ref that doesn't exist exits 128.
        let failed = with(&[
            "run",
            &check,
            "--",
            "git",
            "rev-parse",
            "--verify",
            "refs/heads/hushrun-no-such-branch",
        ]);
        assert_eq!(failed.code, 128, "{vars:?}: {}", failed.stderr);
        assert!(
            failed
                .stderr
                .contains(&format!("hushrun: {check} failed in")),
            "{vars:?}: {}",
            failed.stderr
        );
        // One line says what hushrun ignored, once; nothing else does.
        for run in [&passed, &failed] {
            let lines: Vec<&str> = run
                .stderr
                .lines()
                .filter(|l| l.contains("ignored"))
                .collect();
            assert_eq!(lines, says.into_iter().collect::<Vec<_>>(), "{vars:?}");
        }
    }
    // -q silences that line too.
    let mut quiet = r.hushrun();
    quiet.env("HUSHRUN_FORMAT", "xml");
    let quiet = ran(quiet, &["-q", "run", "unit-q", "--", "git", "--version"]);
    assert!(quiet.ran());
    assert!(quiet.stderr.is_empty(), "{}", quiet.stderr);
}

// covers: C-14
#[cfg(unix)]
#[test]
fn a_variable_that_isnt_utf8_never_stops_the_check() {
    use std::os::unix::ffi::OsStringExt as _;
    let r = Repo::new();
    // Whoever set it, it is left out of what hushrun reads: the command runs, its result is
    // recorded, and hushrun exits with its code.
    let mut cmd = r.hushrun();
    cmd.env(
        "SOME_TOOL_STATE",
        std::ffi::OsString::from_vec(vec![b'x', 0xff]),
    )
    .env("HUSHRUN_FORMAT", std::ffi::OsString::from_vec(vec![0xff]));
    let failed = ran(
        cmd,
        &[
            "run",
            "unit",
            "--",
            "git",
            "rev-parse",
            "--verify",
            "refs/heads/hushrun-no-such-branch",
        ],
    );
    assert_eq!(failed.code, 128, "{}", failed.stderr);
    assert!(
        failed.stderr.contains("hushrun: unit failed in"),
        "{}",
        failed.stderr
    );
    assert_eq!(only_result(&r)["exitCode"], 128);
}

/// [ADR 0037] A repository whose scope `back` declares the checkout beside it, `../cli`, with its
/// hushrun.yaml committed.
fn with_a_sibling() -> Repo {
    let r = Repo::new();
    let cli = r.root.join("cli");
    std::fs::create_dir_all(cli.join("crates")).expect("mkdir");
    std::fs::create_dir_all(cli.join("docs")).expect("mkdir");
    git::init(&cli, "sha1");
    std::fs::write(cli.join("crates/core.txt"), "one\n").expect("write");
    std::fs::write(cli.join("docs/guide.txt"), "one\n").expect("write");
    git::commit_all(&cli, "init");
    r.write(
        "hushrun.yaml",
        "version: 1\nrepository: github.com/acme/shop\nscopes:\n  back:\n    include: [\"src/**\"]\n    external:\n      - path: ../cli\n        include: [\"crates/**\"]\n",
    );
    git::commit_all(&r.repo, "scopes");
    r
}

const SCOPED: &[&str] = &["run", "--scope", "back", "unit", "--", "git", "--version"];

// covers: C-9
#[test]
fn a_change_in_what_a_scope_builds_against_runs_the_check_again() {
    let r = with_a_sibling();
    assert!(ran(r.hushrun(), SCOPED).ran());
    let again = ran(r.hushrun(), SCOPED);
    assert!(!again.ran(), "{}", again.stderr);

    // What the scope doesn't include of it doesn't matter.
    std::fs::write(r.root.join("cli/docs/guide.txt"), "two\n").expect("write");
    assert!(!ran(r.hushrun(), SCOPED).ran());

    // Committed or not, a change in what it includes is new code, as in the repository.
    std::fs::write(r.root.join("cli/crates/core.txt"), "two\n").expect("write");
    let changed = ran(r.hushrun(), SCOPED);
    assert!(changed.ran(), "{}", changed.stderr);
    assert!(!ran(r.hushrun(), SCOPED).ran());

    // Back to what passed, it skips.
    std::fs::write(r.root.join("cli/crates/core.txt"), "one\n").expect("write");
    assert!(!ran(r.hushrun(), SCOPED).ran());

    // The result says what it was about.
    let dir = r.root.join("cache/results/results/v0");
    let results: Vec<Value> = std::fs::read_dir(&dir)
        .expect("results")
        .flat_map(|key| std::fs::read_dir(key.expect("key").path()).expect("key"))
        .map(|file| {
            serde_json::from_slice(&std::fs::read(file.expect("file").path()).expect("read"))
                .expect("JSON")
        })
        .collect();
    assert_eq!(results.len(), 2);
    for result in &results {
        schema::assert_valid(&schema::load("run-result.v0.schema.json"), result);
        assert_eq!(result["external"][0]["path"], "../cli", "{result}");
        assert!(
            result["external"][0]["digest"]
                .as_str()
                .is_some_and(|digest| digest.starts_with("sha256:")),
            "{result}"
        );
    }
    assert_ne!(results[0]["external"], results[1]["external"]);
}

// covers: C-9, C-14
#[test]
fn an_external_input_that_cant_be_read_runs_without_skipping() {
    let r = with_a_sibling();
    assert!(ran(r.hushrun(), SCOPED).ran());
    let top = r.root.join("cli");

    // It isn't a checkout's top: a directory in one, or no checkout at all, says nothing.
    std::fs::rename(&top, r.root.join("moved")).expect("rename");
    let gone = ran(r.hushrun(), SCOPED);
    assert!(gone.ran(), "{}", gone.stderr);
    assert!(
        gone.stderr.contains("running without skipping")
            && gone.stderr.contains("the external input ../cli"),
        "{}",
        gone.stderr
    );
    std::fs::create_dir_all(top.join("crates")).expect("mkdir");
    std::fs::write(top.join("crates/core.txt"), "one\n").expect("write");
    for _ in 0..2 {
        let plain = ran(r.hushrun(), SCOPED);
        assert!(plain.ran(), "{}", plain.stderr);
        assert!(
            plain.stderr.contains("isn't the top of a git checkout"),
            "{}",
            plain.stderr
        );
    }
    std::fs::remove_dir_all(&top).expect("remove");
    std::fs::rename(r.root.join("moved"), &top).expect("rename");

    // Nothing of it is included: hushrun can't tell what it is, so it doesn't guess.
    std::fs::remove_file(top.join("crates/core.txt")).expect("remove");
    let empty = ran(r.hushrun(), SCOPED);
    assert!(empty.ran(), "{}", empty.stderr);
    assert!(
        empty.stderr.contains("the external input ../cli"),
        "{}",
        empty.stderr
    );
}

// covers: C-9
#[test]
fn a_scope_without_external_inputs_keeps_the_key_it_had() {
    // The same files, with and without an empty neighbour: nothing external is in the key.
    let r = Repo::new();
    r.write(
        "hushrun.yaml",
        "version: 1\nrepository: github.com/acme/shop\nscopes:\n  back:\n    include: [\"src/**\"]\n",
    );
    git::commit_all(&r.repo, "scopes");
    assert!(ran(r.hushrun(), SCOPED).ran());
    let dir = r.root.join("cache/results/results/v0");
    let key = std::fs::read_dir(&dir)
        .expect("results")
        .next()
        .expect("one key")
        .expect("key");
    let file = std::fs::read_dir(key.path())
        .expect("key")
        .next()
        .expect("one result")
        .expect("result");
    let result: Value =
        serde_json::from_slice(&std::fs::read(file.path()).expect("read")).expect("JSON");
    assert!(result.get("external").is_none(), "{result}");
}

// covers: C-9
#[test]
fn the_key_with_an_external_input_is_the_specs() {
    // C-9's second example, worked out apart from hushrun (a pass under this key must skip).
    let r = with_a_sibling();
    let digest = format!("sha256:{}", "1".repeat(64));
    let cli = hushrun_digest_of(&r);
    let key = key_of_second_example(&cli);
    let dir = r.root.join("cache/results/results/v0").join(&key);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let pass = serde_json::json!({
        "schema": "hushrun.run-result/v0",
        "check": "unit",
        "result": "pass",
        "exitCode": 0,
        "command": ["vitest", "run"],
        "cwd": "",
        "scope": "back",
        "digest": digest,
        "startedAt": "2026-10-16T14:05:10.123Z",
        "durationMs": 1,
        "provider": "local",
        "tool": "hushrun 0.1.0",
    });
    std::fs::write(dir.join("01.json"), pass.to_string()).expect("write");
    let skipped = ran(
        r.hushrun(),
        &[
            "run", "--scope", "back", "--digest", &digest, "unit", "--", "vitest", "run",
        ],
    );
    assert_eq!(skipped.code, 0, "{}", skipped.stderr);
    assert!(skipped.stderr.contains("skipped"), "{}", skipped.stderr);
}

/// The source digest of `../cli`'s `crates/**`, as `hushrun digest` gives it there.
fn hushrun_digest_of(r: &Repo) -> String {
    let cli = r.root.join("cli");
    let scratch = r.root.join("digest");
    std::fs::create_dir_all(&scratch).expect("mkdir");
    git::init(&scratch, "sha1");
    std::fs::create_dir_all(scratch.join("crates")).expect("mkdir");
    std::fs::copy(cli.join("crates/core.txt"), scratch.join("crates/core.txt")).expect("copy");
    git::commit_all(&scratch, "init");
    let output = r
        .hushrun_in(&scratch)
        .args(["digest", "--format", "json"])
        .output()
        .expect("digest");
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    value["scopes"][0]["digest"]
        .as_str()
        .expect("digest")
        .to_string()
}

/// The SHA-256 of C-9's JCS form for the example, by hand: keys sorted, no spaces.
fn key_of_second_example(external: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let one = "1".repeat(64);
    let text = format!(
        r#"{{"check":"unit","command":["vitest","run"],"cwd":"","digest":"sha256:{one}","external":[{{"digest":"{external}","path":"../cli"}}],"schema":"hushrun.run-key/v0","scope":"back"}}"#
    );
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Each file under `dir`, by its path from there, `/`-separated.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, from: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, from, found);
            } else {
                let relative = path.strip_prefix(from).expect("below");
                found.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

/// `hushrun run --now <now> <check> -- git --version`.
fn run_at(r: &Repo, now: &str, check: &str) -> Ran {
    ran(
        r.hushrun(),
        &["run", "--now", now, check, "--", "git", "--version"],
    )
}

/// The key directories under `results/v0` or `skips/v0` of this machine's results.
fn key_dirs(r: &Repo, prefix: &str) -> usize {
    std::fs::read_dir(r.root.join("cache/results").join(prefix))
        .map(|entries| entries.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0)
}

// covers: C-11 (ADR 0067: this machine forgets a result 30 days after its run, sweeping at most
// once a day, by the command's clock)
#[test]
fn this_machine_forgets_results_after_30_days() {
    const T0: &str = "2026-01-01T00:00:00Z";
    let r = Repo::new();
    let results = r.root.join("cache/results");
    assert!(run_at(&r, T0, "unit").ran());
    assert!(run_at(&r, T0, "other").ran());
    // Ten days on, the pass still skips, and the skip is kept apart.
    let skip = run_at(&r, "2026-01-11T00:00:00Z", "unit");
    assert!(!skip.ran(), "{}", skip.stderr);
    assert_eq!(key_dirs(&r, "results/v0"), 2);
    assert_eq!(key_dirs(&r, "skips/v0"), 1);
    // What isn't a result of hushrun's stays, whatever its age.
    let unit_dir = std::fs::read_dir(results.join("skips/v0"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let unit_key = unit_dir.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::write(
        results.join("results/v0").join(&unit_key).join("notes.txt"),
        "mine\n",
    )
    .unwrap();
    std::fs::write(
        results.join("results/v0").join(&unit_key).join(".tmp-x"),
        "",
    )
    .unwrap();

    // 31 days on, the sweep forgets both passes of the first day, and the key that is left with
    // nothing, and keeps the skip of the tenth day.
    let late = run_at(&r, "2026-02-01T00:00:00Z", "late");
    assert!(late.ran() && late.code == 0, "{}", late.stderr);
    let left = files_under(&results);
    let cutoff = millis("2026-01-02T00:00:00Z");
    assert!(
        left.iter()
            .filter_map(|path| path.rsplit('/').next()?.strip_suffix(".json"))
            .all(|ulid| ulid_millis(ulid) >= cutoff),
        "{left:#?}"
    );
    assert_eq!(key_dirs(&r, "skips/v0"), 1, "{left:#?}");
    // `unit`'s key keeps the files that aren't results; `other`'s is gone; `late` is new.
    assert_eq!(key_dirs(&r, "results/v0"), 2, "{left:#?}");
    assert!(
        left.contains(&format!("results/v0/{unit_key}/notes.txt")),
        "{left:#?}"
    );
    assert!(
        left.contains(&format!("results/v0/{unit_key}/.tmp-x")),
        "{left:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(results.join("swept"))
            .unwrap()
            .trim(),
        "2026-02-01T00:00:00.000Z"
    );
    // So `other` runs again.
    assert!(run_at(&r, "2026-02-01T01:00:00Z", "other").ran());

    // A result older than 30 days that appears within a day of a sweep stays until the next one,
    // a day later.
    let planted = results
        .join("results/v0")
        .join(&unit_key)
        .join(format!("{}0000000000000000.json", ulid_time(millis(T0))));
    std::fs::write(&planted, "{}").unwrap();
    assert!(run_at(&r, "2026-02-01T23:00:00Z", "other").code == 0);
    assert!(planted.exists(), "swept twice in a day");
    assert!(run_at(&r, "2026-02-02T00:00:01Z", "other").code == 0);
    assert!(!planted.exists(), "a day after a sweep, the next");

    // A clock that went back sweeps too, and never past its own cutoff: by it, these are new.
    std::fs::write(&planted, "{}").unwrap();
    assert!(run_at(&r, "2026-01-15T00:00:00Z", "other").code == 0);
    assert!(planted.exists(), "14 days old by this clock");
    // `unit`'s skip of the tenth day, and `other`'s since.
    assert_eq!(key_dirs(&r, "skips/v0"), 2);
    assert_eq!(
        std::fs::read_to_string(results.join("swept"))
            .unwrap()
            .trim(),
        "2026-01-15T00:00:00.000Z"
    );
}

// covers: C-11 (ADR 0067: sweeps at once never take a result newer than their cutoff, and never
// stop a run)
#[test]
fn sweeps_at_once_keep_every_new_result_and_a_stamp_that_cant_be_written_stops_nothing() {
    let r = Repo::new();
    assert!(run_at(&r, "2026-01-01T00:00:00Z", "old").ran());
    // Several runs at once, 40 days on, each sweeping and recording its own check.
    let children: Vec<_> = (0..6)
        .map(|n| {
            let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
            cmd.current_dir(&r.repo)
                .env("HUSHRUN_CACHE_DIR", r.root.join("cache"))
                .env("HUSHRUN_CREDENTIALS", r.root.join("credentials.json"))
                .env("HUSHRUN_HOST", NO_HOST)
                .env("GIT_CONFIG_NOSYSTEM", "1");
            for var in CLEARED {
                cmd.env_remove(var);
            }
            cmd.args(["run", "--now", "2026-02-10T00:00:00Z"])
                .arg(format!("check-{n}"))
                .args(["--", "git", "--version"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("hushrun")
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().expect("hushrun");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(
        key_dirs(&r, "results/v0"),
        6,
        "each run kept its own, and `old` is gone"
    );
    for n in 0..6 {
        assert!(!run_at(&r, "2026-02-10T00:01:00Z", &format!("check-{n}")).ran());
    }

    // A stamp that can't be written: no sweep, and the run goes on as ever, saying nothing of it.
    let stamp = r.root.join("cache/results/swept");
    std::fs::remove_file(&stamp).unwrap();
    std::fs::create_dir_all(stamp.join("in-the-way")).unwrap();
    let first = run_at(&r, "2026-06-01T00:00:00Z", "check-0");
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(!first.stderr.contains("swept"), "{}", first.stderr);
    assert_eq!(key_dirs(&r, "results/v0"), 6, "no stamp, no sweep");
}

/// Crockford's base 32, as ULIDs write it.
const ULID_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The milliseconds of an RFC 3339 time.
fn millis(time: &str) -> i64 {
    time.parse::<jiff::Timestamp>()
        .expect("a time")
        .as_millisecond()
}

/// The first 10 characters of a ULID: its milliseconds.
fn ulid_time(ms: i64) -> String {
    (0..10)
        .rev()
        .map(|i| ULID_ALPHABET[((ms >> (5 * i)) & 31) as usize] as char)
        .collect()
}

/// The milliseconds a ULID's first 10 characters hold.
fn ulid_millis(ulid: &str) -> i64 {
    ulid.bytes().take(10).fold(0, |ms, c| {
        let digit = ULID_ALPHABET.iter().position(|a| *a == c).expect("a ULID");
        (ms << 5) | digit as i64
    })
}
