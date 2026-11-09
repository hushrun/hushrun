//! A pull request from a fork reads what its repository's CI recorded (ADR 0052): GitHub gives
//! its job no ID token, so `hushrun run` asks as the repository's CI with no credential, skips on
//! a pass that counts in its job, and records nothing. hushrun cloud answers such a lookup for a
//! public repository of its open-source program (results-v0 §2's fourth caller); any other server
//! refuses it, and the checks run, as before.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::git;

/// Variables a test must never inherit: GitHub sets the first ones on the jobs that run these.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_JOB",
    "GITHUB_REPOSITORY",
    "GITHUB_SERVER_URL",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_WORKSPACE",
    "GITHUB_EVENT_NAME",
    "GITHUB_EVENT_PATH",
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

/// One workflow for pull requests and pushes to `main`, whose job `test` runs the check.
const CI: &str = "on:\n  pull_request:\n  push:\n    branches: [main]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh check.sh\n";

/// What the last line of a fork's run says when the server answered its lookup.
const FORK_READ: &str = "hushrun: results aren't shared with acme: GitHub gives a pull request from a fork no ID token, so it records nothing";

/// A repository on github.com, acme/shop, whose results the fake keeps.
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
        let r = Repo {
            _dir: dir,
            root,
            repo,
            fake: FakeResults::start(),
        };
        r.write("check.sh", "exit 0\n");
        r.write(".github/workflows/ci.yml", CI);
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
        git::commit_all(&self.repo, "change");
    }

    /// `hushrun`, with the fake as its backend and nothing else configured.
    fn hushrun(&self) -> Command {
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
        cmd.env("HUSHRUN_BACKEND", self.fake.url());
        cmd
    }

    /// The same in the job `test` of ci.yml, run for `event` on `reference`.
    fn on_github(&self, event: &str, reference: &str) -> Command {
        let mut cmd = self.hushrun();
        cmd.env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_SERVER_URL", "https://github.com")
            .env("GITHUB_REPOSITORY", "acme/shop")
            .env("GITHUB_RUN_ID", "41")
            .env(
                "GITHUB_WORKFLOW_REF",
                format!("acme/shop/.github/workflows/ci.yml@{reference}"),
            )
            .env("GITHUB_JOB", "test")
            .env("GITHUB_EVENT_NAME", event)
            .env("GITHUB_REF", reference);
        cmd
    }

    /// The repository's own CI on a push to `main`, with a token to record with.
    fn main(&self) -> Command {
        let mut cmd = self.on_github("push", "refs/heads/main");
        cmd.env("HUSHRUN_TOKEN", FakeResults::TOKEN);
        cmd
    }

    /// Pull request 12, from `head` (`owner/name`), with no credential, as GitHub runs a pull
    /// request from a fork, and one of the repository itself without `id-token: write`.
    fn pull_request_from(&self, head: &str) -> Command {
        let event = self.root.join("event.json");
        std::fs::write(
            &event,
            format!(r#"{{"pull_request": {{"head": {{"repo": {{"full_name": "{head}"}}}}}}}}"#),
        )
        .expect("write");
        let mut cmd = self.on_github("pull_request", "refs/pull/12/merge");
        cmd.env("GITHUB_BASE_REF", "main")
            .env("GITHUB_EVENT_PATH", event);
        cmd
    }

    /// What the fake was asked from request `from` on.
    fn asked_since(&self, from: usize) -> Vec<Asked> {
        self.fake.raw_requests()[from..]
            .iter()
            .map(|request| Asked {
                method: request.method.clone(),
                credential: request.header("authorization").is_some(),
                query: (request.query.iter())
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect(),
            })
            .collect()
    }
}

/// A request the fake was sent, as these tests look at it.
#[derive(Debug)]
struct Asked {
    method: String,
    /// Whether it carried a credential.
    credential: bool,
    /// Its query, `name=value` each, in order.
    query: Vec<String>,
}

/// What a command printed, both streams.
fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// hushrun's own lines.
fn lines(out: &Output) -> Vec<String> {
    said(out)
        .lines()
        .filter(|line| line.starts_with("hushrun: ") && !line.contains("the pre-push hook"))
        .map(str::to_string)
        .collect()
}

fn unit(mut cmd: Command) -> Output {
    let out = cmd
        .args(["run", "unit", "--", "sh", "check.sh"])
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    out
}

// covers: C-10, C-11, C-13
#[test]
fn a_forks_pull_request_skips_on_the_repositorys_ci_pass_and_records_nothing() {
    let r = Repo::new();
    r.fake.answer_without_a_token("github.com/acme/shop");
    let main = unit(r.main());
    assert!(
        said(&main).contains("hushrun: unit passed"),
        "{}",
        said(&main)
    );
    assert_eq!(r.fake.recorded().len(), 1);

    let before = r.fake.raw_requests().len();
    let fork = unit(r.pull_request_from("someone/shop"));
    let said_by_fork = lines(&fork);
    assert!(
        said_by_fork[0].starts_with("hushrun: unit skipped: this exact code passed in CI"),
        "{said_by_fork:?}"
    );
    assert_eq!(said_by_fork.last().map(String::as_str), Some(FORK_READ));
    // It asked as the repository's CI, in its job, with no credential, and sent nothing else.
    let asked = r.asked_since(before);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(
        (asked[0].method.as_str(), asked[0].credential),
        ("GET", false)
    );
    let query = &asked[0].query;
    assert_eq!(query[..2], ["as=ci", "repository=github.com/acme/shop"]);
    assert!(query[2].starts_with("job=test@"), "{query:?}");
    assert_eq!(r.fake.recorded().len(), 1);

    // What the pull request changed runs, and isn't recorded either.
    r.write("check.sh", "exit 0 # the pull request's change\n");
    let before = r.fake.raw_requests().len();
    let changed = unit(r.pull_request_from("someone/shop"));
    let said_by_fork = lines(&changed);
    assert_eq!(
        said_by_fork[0], "hushrun: unit running: no pass yet for this exact code",
        "{said_by_fork:?}"
    );
    assert_eq!(said_by_fork.last().map(String::as_str), Some(FORK_READ));
    assert!(
        r.asked_since(before)
            .iter()
            .all(|asked| asked.method == "GET" && !asked.credential)
    );
    assert_eq!(r.fake.recorded().len(), 1);
}

/// A job of the repository itself without `permissions: id-token: write` reads the same way, and
/// is told how to record.
// covers: C-11, C-14
#[test]
fn a_job_without_an_id_token_reads_and_is_told_how_to_record() {
    let r = Repo::new();
    r.fake.answer_without_a_token("github.com/acme/shop");
    unit(r.main());
    let own = unit(r.pull_request_from("acme/shop"));
    let said_by_it = lines(&own);
    assert!(
        said_by_it[0].starts_with("hushrun: unit skipped"),
        "{said_by_it:?}"
    );
    assert_eq!(
        said_by_it.last().map(String::as_str),
        Some(
            "hushrun: results aren't shared with acme: no token: give the job `permissions: id-token: write`"
        )
    );
    assert_eq!(r.fake.recorded().len(), 1);
}

/// Where the server answers no lookup without a token, a fork's checks run, as before; outside
/// CI, and on a GitHub other than github.com, hushrun never asks as the repository's CI.
// covers: C-11, C-13
#[test]
fn without_an_answer_the_checks_run_and_only_github_actions_asks_as_ci() {
    let r = Repo::new();
    unit(r.main());
    let fork = unit(r.pull_request_from("someone/shop"));
    let said_by_fork = lines(&fork);
    assert_eq!(
        said_by_fork,
        [
            "hushrun: results aren't shared with acme: GitHub gives a pull request from a fork no ID token, so its checks run, and record nothing"
        ],
        "{}",
        said(&fork)
    );

    let before = r.fake.raw_requests().len();
    unit(r.hushrun());
    let mut enterprise = r.pull_request_from("someone/shop");
    enterprise.env("GITHUB_SERVER_URL", "https://github.acme.dev");
    unit(enterprise);
    let asked = r.asked_since(before);
    assert!(!asked.is_empty());
    assert!(
        asked
            .iter()
            .all(|asked| asked.query.iter().all(|pair| !pair.starts_with("as="))),
        "{asked:?}"
    );
    assert_eq!(r.fake.recorded().len(), 1);
}
