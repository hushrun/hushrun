//! Why a run ran although a pass would have skipped it (M1), and a run whose verdict the result
//! that decided it didn't have (M3), end to end: a run records the one rule that stopped its skip,
//! `ranAnyway`, only when a pass of its key is there to stop, and the line that ends a run says
//! when it may be flaky, on GitHub in the step summary too.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::{git, schema};

/// Variables a test must never inherit: GitHub sets the first ones on the jobs that run these.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_JOB",
    "GITHUB_REPOSITORY",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_WORKSPACE",
    "GITHUB_OUTPUT",
    "GITHUB_STEP_SUMMARY",
    "HUSHRUN_VARIANT",
    "HUSHRUN_WORKFLOW",
    "HUSHRUN_INPUTS",
    "HUSHRUN_REPORTS_READ",
    "GITLAB_CI",
    "CI_JOB_NAME",
    "CI_NODE_TOTAL",
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

/// The check: it exits with `VERDICT`.
const CHECK: &str = "exit \"${VERDICT:-0}\"\n";

const STEPS: &str =
    "      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh check.sh\n";

/// The job `test`, once, on one system, then `after`.
fn one_job(after: &str) -> String {
    format!("on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n{STEPS}{after}")
}

/// The job `test`, in two variants that don't say which they are.
fn matrix() -> String {
    format!(
        "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        node: [20, 22]\n    runs-on: ubuntu-latest\n    steps:\n{STEPS}"
    )
}

/// The check in a job on Linux and a job on macOS.
fn two_systems() -> String {
    format!(
        "on: push\njobs:\n  linux:\n    runs-on: ubuntu-latest\n    steps:\n{STEPS}  mac:\n    runs-on: macos-latest\n    steps:\n{STEPS}"
    )
}

const CODECOV: &str =
    "      - uses: codecov/codecov-action@v5\n        with:\n          files: coverage.xml\n";

/// A repository with a remote, a cache and credentials of its own, and \[ADR 0067\] a results
/// server its runs share, in CI and out: CI keeps no results in a runner's directory.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    fake: FakeResults,
}

impl Repo {
    fn new(workflow: &str) -> Repo {
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
        r.write("check.sh", CHECK);
        r.write(".github/workflows/ci.yml", workflow);
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    /// `hushrun` here, with no inherited configuration.
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
        cmd.env("HUSHRUN_BACKEND", self.fake.url())
            .env("HUSHRUN_TOKEN", FakeResults::TOKEN);
        cmd
    }

    /// `hushrun` in the GitHub Actions job `job` of `workflow`, whose step summary is a file here.
    fn on_github(&self, workflow: &str, job: &str) -> Command {
        let mut cmd = self.hushrun();
        cmd.env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", "acme/shop")
            .env(
                "GITHUB_WORKFLOW_REF",
                format!("acme/shop/.github/workflows/{workflow}@refs/heads/main"),
            )
            .env("GITHUB_JOB", job)
            .env("GITHUB_STEP_SUMMARY", self.root.join("summary"));
        cmd
    }

    /// What the step summary holds.
    fn summary(&self) -> String {
        std::fs::read_to_string(self.root.join("summary")).unwrap_or_default()
    }

    /// Each pass's and fail's `ranAnyway`, oldest first, each record as its schema says.
    fn reasons(&self) -> Vec<Option<String>> {
        let records = schema::load("run-result.v0.schema.json");
        self.fake
            .recorded()
            .into_iter()
            .filter(|value| value["result"] != "skip")
            .map(|value| {
                schema::assert_valid(&records, &value);
                value["ranAnyway"].as_str().map(str::to_string)
            })
            .collect()
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

/// `hushrun run unit -- sh check.sh`, which exits with `verdict`, with `flags` before the name.
fn unit(mut cmd: Command, verdict: &str, flags: &[&str]) -> Output {
    cmd.env("VERDICT", verdict)
        .arg("run")
        .args(flags)
        .args(["unit", "--", "sh", "check.sh"])
        .output()
        .expect("runs")
}

#[track_caller]
fn ran(out: &Output, code: i32) {
    assert_eq!(out.status.code(), Some(code), "{}", said(out));
    assert!(!said(out).contains("skipped"), "{}", said(out));
}

/// Two runs in `first` and then `second`, both passing: the reason each recorded.
fn twice(r: &Repo, first: Command, second: Command) -> Vec<Option<String>> {
    ran(&unit(first, "0", &[]), 0);
    ran(&unit(second, "0", &[]), 0);
    r.reasons()
}

fn word(reason: &str) -> Option<String> {
    Some(reason.to_string())
}

// M1: each rule that stops a skip, recorded with the run it stopped; none without a pass.

/// A later step reads the check's reports.
// covers: C-26
#[test]
fn a_reader_is_the_reason_once_a_pass_is_there() {
    let r = Repo::new(&one_job(CODECOV));
    let reasons = twice(
        &r,
        r.on_github("ci.yml", "test"),
        r.on_github("ci.yml", "test"),
    );
    assert_eq!(reasons, [None, word("reports")]);
}

/// The job has several variants and doesn't say which, and inside another run.
// covers: C-14
#[test]
fn variants_are_the_reason_once_a_pass_of_the_job_is_there() {
    let r = Repo::new(&matrix());
    let reasons = twice(
        &r,
        r.on_github("ci.yml", "test"),
        r.on_github("ci.yml", "test"),
    );
    assert_eq!(reasons, [None, word("variants")]);
    let mut inside = r.on_github("ci.yml", "test");
    inside.env("HUSHRUN_INSIDE_RUN", "1");
    ran(&unit(inside, "0", &[]), 0);
    assert_eq!(r.reasons()[2], word("inside-run"));
}

/// A job GitHub names that the file doesn't hold, as a called workflow's, and a workflow that
/// isn't in the checkout.
// covers: C-14
#[test]
fn a_job_hushrun_cant_tell_is_the_reason_once_its_pass_is_there() {
    let r = Repo::new(&one_job(""));
    let reasons = twice(
        &r,
        r.on_github("ci.yml", "called"),
        r.on_github("ci.yml", "called"),
    );
    assert_eq!(reasons, [None, word("reusable")]);
    let r = Repo::new(&one_job(""));
    let reasons = twice(
        &r,
        r.on_github("elsewhere.yml", "test"),
        r.on_github("elsewhere.yml", "test"),
    );
    assert_eq!(reasons, [None, word("cant-tell")]);
}

/// A pass of another job doesn't count in this one.
// covers: C-10
#[test]
fn a_pass_that_doesnt_count_here_is_the_rule() {
    let r = Repo::new(&two_systems());
    let reasons = twice(
        &r,
        r.on_github("ci.yml", "linux"),
        r.on_github("ci.yml", "mac"),
    );
    assert_eq!(reasons, [None, word("rule")]);
}

/// `--force` with a pass of the job on record, and with none: a fail stops no skip.
// covers: C-14
#[test]
fn force_is_the_reason_only_over_a_pass() {
    let r = Repo::new(&one_job(""));
    ran(&unit(r.on_github("ci.yml", "test"), "1", &[]), 1);
    ran(&unit(r.on_github("ci.yml", "test"), "1", &["--force"]), 1);
    assert_eq!(r.reasons(), [None, None]);
    ran(&unit(r.on_github("ci.yml", "test"), "0", &[]), 0);
    ran(&unit(r.on_github("ci.yml", "test"), "0", &["--force"]), 0);
    assert_eq!(r.reasons()[3], word("force"));
    // Outside CI too.
    let r = Repo::new(&one_job(""));
    ran(&unit(r.hushrun(), "0", &[]), 0);
    ran(&unit(r.hushrun(), "0", &["--force"]), 0);
    assert_eq!(r.reasons(), [None, word("force")]);
}

// M3: a verdict the result that decided here didn't have.

/// A fail, then a pass of the same code; a pass, then a forced fail.
// covers: C-14
#[test]
fn a_verdict_the_deciding_result_didnt_have_may_be_flaky() {
    let r = Repo::new(&one_job(""));
    ran(&unit(r.on_github("ci.yml", "test"), "1", &[]), 1);
    let pass = unit(r.on_github("ci.yml", "test"), "0", &[]);
    ran(&pass, 0);
    assert!(
        said(&pass).contains(
            "; it won't run again on this code; it failed on this exact code before, so it may be flaky\n"
        ),
        "{}",
        said(&pass)
    );
    assert!(
        r.summary().contains("### unit may be flaky\n\n")
            && r.summary()
                .contains("; it failed on this exact code before, so it may be flaky"),
        "{}",
        r.summary()
    );
    let fail = unit(r.on_github("ci.yml", "test"), "1", &["--force"]);
    ran(&fail, 1);
    assert!(
        said(&fail).contains("(exit 1); it passed on this exact code before, so it may be flaky\n"),
        "{}",
        said(&fail)
    );
    // The same verdict again says nothing of it.
    let again = unit(r.on_github("ci.yml", "test"), "1", &[]);
    ran(&again, 1);
    assert!(!said(&again).contains("flaky"), "{}", said(&again));
}

/// A pass of another job, or of a job whose variants a run can't tell apart, didn't decide here:
/// a fail after it says nothing of flakiness.
// covers: C-10, C-14
#[test]
fn a_pass_that_didnt_decide_here_says_nothing_of_flakiness() {
    let r = Repo::new(&two_systems());
    ran(&unit(r.on_github("ci.yml", "linux"), "0", &[]), 0);
    let mac = unit(r.on_github("ci.yml", "mac"), "1", &[]);
    ran(&mac, 1);
    assert!(!said(&mac).contains("flaky"), "{}", said(&mac));
    let r = Repo::new(&matrix());
    ran(&unit(r.on_github("ci.yml", "test"), "0", &[]), 0);
    let other = unit(r.on_github("ci.yml", "test"), "1", &[]);
    ran(&other, 1);
    assert!(!said(&other).contains("flaky"), "{}", said(&other));
    assert!(r.summary().is_empty(), "{}", r.summary());
}
