//! GitHub's merge queue, as hushrun sees it (G12): a check's key comes from the files it reads,
//! so the queue's run of a pull request's tree skips on that pull request's pass in the same job,
//! and the push to `main` that merging makes skips too; `hushrun doctor` says which pull request
//! the queue's run tests. The live run of the same is `cargo xtask e2e --scenario merge-queue`.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::{git, schema};
use serde_json::Value;

/// Variables a test must never inherit: GitHub sets the first ones on the jobs that run these.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_JOB",
    "GITHUB_REPOSITORY",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_WORKSPACE",
    "GITHUB_EVENT_NAME",
    "GITHUB_REF",
    "GITHUB_BASE_REF",
    "GITHUB_OUTPUT",
    "GITHUB_STEP_SUMMARY",
    "HUSHRUN_VARIANT",
    "HUSHRUN_WORKFLOW",
    "HUSHRUN_INPUTS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_LOG",
    "HUSHRUN_PROVIDER",
    "npm_lifecycle_event",
];

/// Where the hosted backend would be: nothing listens on the discard port.
const NO_HOST: &str = "http://127.0.0.1:9";

/// The pull request's head, which the queue's ref names.
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// One workflow with the three events, whose job `test` runs the check.
const CI: &str = "on:\n  pull_request:\n  merge_group:\n  push:\n    branches: [main]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh check.sh\n";

/// A repository with a remote, a cache and credentials of its own, and \[ADR 0067\] a results
/// server its CI shares: CI keeps no results in a runner's directory.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    fake: FakeResults,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        git::run(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/shop.git",
            ],
            None,
        );
        for (path, text) in [("check.sh", "exit 0\n"), (".github/workflows/ci.yml", CI)] {
            let path = repo.join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        }
        git::commit_all(&repo, "init");
        Repo {
            _dir: dir,
            root,
            repo,
            fake: FakeResults::start(),
        }
    }

    /// `hushrun` in the job `test` of ci.yml, run for `event` on `reference`.
    fn on_github(&self, event: &str, reference: &str) -> Command {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", "acme/shop")
            .env("GITHUB_RUN_ID", "41")
            .env(
                "GITHUB_WORKFLOW_REF",
                format!("acme/shop/.github/workflows/ci.yml@{reference}"),
            )
            .env("GITHUB_JOB", "test")
            .env("GITHUB_EVENT_NAME", event)
            .env("GITHUB_REF", reference)
            .env("HUSHRUN_BACKEND", self.fake.url())
            .env("HUSHRUN_TOKEN", FakeResults::TOKEN);
        cmd
    }

    /// The queue's run of pull request 12 into `main`.
    fn in_the_queue(&self) -> Command {
        self.on_github(
            "merge_group",
            &format!("refs/heads/gh-readonly-queue/main/pr-12-{HEAD}"),
        )
    }
}

/// What a command printed, both streams.
fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn unit(mut cmd: Command) -> Output {
    cmd.args(["run", "unit", "--", "sh", "check.sh"])
        .output()
        .expect("runs")
}

// covers: C-9, C-10
#[test]
fn the_queues_run_skips_on_the_pull_requests_pass() {
    let r = Repo::new();
    let mut pull = r.on_github("pull_request", "refs/pull/12/merge");
    pull.env("GITHUB_BASE_REF", "main");
    let first = unit(pull);
    assert!(
        said(&first).contains("hushrun: unit passed"),
        "{}",
        said(&first)
    );
    let queued = unit(r.in_the_queue());
    assert!(
        said(&queued).contains("hushrun: unit skipped: this exact code passed in CI"),
        "{}",
        said(&queued)
    );
    let merged = unit(r.on_github("push", "refs/heads/main"));
    assert!(
        said(&merged).contains("hushrun: unit skipped"),
        "{}",
        said(&merged)
    );
}

// covers: C-14
#[test]
fn doctor_names_what_the_queues_run_tests() {
    let r = Repo::new();
    let out = r
        .in_the_queue()
        .args(["doctor", "--format", "json"])
        .output()
        .expect("runs");
    let checks: Value = serde_json::from_slice(&out.stdout).expect("JSON");
    schema::assert_valid(&schema::load("doctor.v1.schema.json"), &checks);
    let ci = checks["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["check"] == "ci")
        .and_then(|check| check["detail"].as_str())
        .unwrap_or_else(|| panic!("no ci in {checks}"));
    assert_eq!(
        ci,
        "github run 41, the merge queue's run of pull request #12 into main"
    );
}
