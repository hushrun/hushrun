//! \[ADR 0048\] A run keeps a summary of its reports (C-27), end to end: what `hushrun run` finds
//! after the command, records and says, what a skip carries and writes for GitHub, and what
//! `hushrun ready` shows. Scenarios S1 to S15 of the plan; S13, the record's bound, is the
//! reports crate's proptest, and S14 is with the readers rule's in `tests/reports.rs`.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::{git, schema};
use serde_json::{Value, json};

/// Variables a test must never inherit: GitHub sets the first ones on the jobs that run these.
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
    "GITHUB_OUTPUT",
    "GITHUB_STEP_SUMMARY",
    "GITHUB_RUN_ID",
    "GITHUB_RUN_ATTEMPT",
    "GITHUB_SERVER_URL",
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
    "HUSHRUN_NO_CI_OUTPUT",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_REPORTS_READ",
    "npm_lifecycle_event",
];

/// Where the hosted backend would be: nothing listens on the discard port.
const NO_HOST: &str = "http://127.0.0.1:9";

const WORKFLOW: &str = ".github/workflows/ci.yml";

/// A job that runs the check and reads none of its reports, so a pass skips it in CI.
const JOB: &str = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh check.sh\n";

/// What the checks write is no part of the code.
const IGNORED: &str = "*.xml\n*.info\nout/\ncustom/\n";

/// A report vector's path, for a check to copy.
fn vector(name: &str) -> String {
    hushrun_testkit::testdata(&format!("reports/{name}"))
        .display()
        .to_string()
}

/// The summary of `junit/basic.xml` and `lcov/basic.info`, as a record holds it.
fn basic() -> Value {
    json!({
        "files": [
            {"path": "junit.xml", "kind": "tests", "format": "junit"},
            {"path": "lcov.info", "kind": "coverage", "format": "lcov"},
        ],
        "tests": {"total": 7, "failed": 1, "errors": 1, "skipped": 1, "durationMs": 12500},
        "coverage": {"format": "lcov", "lines": {"covered": 3, "total": 5}, "branches": {"covered": 3, "total": 4}},
        "filesComplete": true,
    })
}

/// A check that writes `junit/basic.xml` and `lcov/basic.info` and passes.
fn writes_basic() -> String {
    format!(
        "cp '{}' junit.xml\ncp '{}' lcov.info\n",
        vector("junit/basic.xml"),
        vector("lcov/basic.info")
    )
}

/// A repository with a remote, a cache and credentials of its own, `check.sh`, and \[ADR 0067\]
/// a results server its runs share, in CI and out: CI keeps no results in a runner's directory.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    fake: FakeResults,
}

impl Repo {
    fn new(check: &str, files: &[(&str, &str)]) -> Repo {
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
        r.write(".gitignore", IGNORED);
        r.write("check.sh", check);
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

    /// `hushrun` here, as a local run with no inherited configuration.
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
        shared(cmd, &self.fake)
    }

    /// `hushrun` in the GitHub Actions job `test` of `repository`'s `ci.yml`, run 41, with its
    /// outputs and step summary in this repository's root.
    fn on_github(&self, repository: &str) -> Command {
        let mut cmd = self.hushrun();
        cmd.env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", repository)
            .env(
                "GITHUB_WORKFLOW_REF",
                format!("{repository}/{WORKFLOW}@refs/heads/main"),
            )
            .env("GITHUB_JOB", "test")
            .env("GITHUB_RUN_ID", "41")
            .env("GITHUB_SERVER_URL", "https://github.com")
            .env("GITHUB_OUTPUT", self.root.join("output"))
            .env("GITHUB_STEP_SUMMARY", self.root.join("summary"));
        cmd
    }

    /// What GitHub's outputs file and step summary hold.
    fn github(&self) -> (String, String) {
        let read = |name: &str| std::fs::read_to_string(self.root.join(name)).unwrap_or_default();
        (read("output"), read("summary"))
    }

    /// The results recorded here, skips included, oldest first.
    fn results(&self) -> Vec<Value> {
        let mut found = self.fake.recorded();
        // RFC 3339 times of one form sort by time; a stable sort keeps the order of a tie.
        found.sort_by(|a, b| a["startedAt"].as_str().cmp(&b["startedAt"].as_str()));
        found
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

/// `hushrun run unit -- sh check.sh` and `args`.
fn unit(mut cmd: Command, args: &[&str]) -> Output {
    cmd.args(["run", "unit", "--", "sh", "check.sh"])
        .args(args)
        .output()
        .expect("runs")
}

#[track_caller]
fn says(out: &Output, text: &str) {
    assert!(
        said(out).contains(text),
        "{text:?} isn't in:\n{}",
        said(out)
    );
}

/// S1: a stub writes a vector's JUnit file, and coverage beside it.
// covers: C-27
#[test]
fn a_run_records_the_summary_of_the_reports_it_wrote() {
    let r = Repo::new(&writes_basic(), &[]);
    let out = unit(r.hushrun(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    says(
        &out,
        ": 7 tests, 2 failed, 1 skipped, 60.0% of lines covered; it won't run again on this code\n",
    );
    let results = r.results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["reports"], basic());
    schema::assert_valid(&schema::load("run-result.v0.schema.json"), &results[0]);
}

/// S2: Cobertura and LCOV of the same data count once, LCOV's.
// covers: C-27
#[test]
fn coverage_written_twice_counts_once() {
    let check = format!(
        "cp '{}' coverage.xml\ncp '{}' lcov.info\n",
        vector("cobertura/lines-only.xml"),
        vector("lcov/basic.info")
    );
    let r = Repo::new(&check, &[]);
    let out = unit(r.hushrun(), &[]);
    says(&out, ": 60.0% of lines covered; it won't run again");
    let reports = &r.results()[0]["reports"];
    assert_eq!(reports["coverage"]["format"], "lcov");
    assert_eq!(
        reports["files"],
        json!([
            {"path": "coverage.xml", "kind": "coverage", "format": "cobertura"},
            {"path": "lcov.info", "kind": "coverage", "format": "lcov"},
        ])
    );
}

/// S3: a report from before the run is none of its own.
// covers: C-27
#[test]
fn a_report_older_than_the_run_is_ignored() {
    let r = Repo::new("exit 0\n", &[]);
    let old = r.repo.join("junit.xml");
    std::fs::copy(vector("junit/basic.xml"), &old).expect("copy");
    let file = std::fs::File::options()
        .write(true)
        .open(&old)
        .expect("open");
    file.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .expect("an hour ago");
    let out = unit(r.hushrun(), &[]);
    says(&out, "hushrun: unit passed in ");
    assert!(!said(&out).contains("tests"), "{}", said(&out));
    // It found nothing it wrote, and says so.
    assert_eq!(
        r.results()[0]["reports"],
        json!({"filesComplete": true}),
        "{:?}",
        r.results()
    );
}

/// S4: the command line names its report.
// covers: C-27
#[test]
fn the_command_line_names_its_report() {
    let check = format!(
        "mkdir -p out\ncp '{}' out/r.xml\n",
        vector("junit/basic.xml")
    );
    let r = Repo::new(&check, &[]);
    let out = unit(r.hushrun(), &["--junitxml=out/r.xml"]);
    says(&out, ": 7 tests, 2 failed, 1 skipped; it won't run again");
    assert_eq!(r.results()[0]["reports"]["files"][0]["path"], "out/r.xml");
}

/// S5: hushrun.yaml names a report file no tool would.
// covers: C-27, C-5
#[test]
fn hushrun_yaml_names_a_report_file() {
    let check = format!(
        "mkdir -p custom\ncp '{}' custom/results.data\n",
        vector("junit/basic.xml")
    );
    let policy = "version: 1\nrepository: github.com/acme/shop\nchecks:\n  unit:\n    reports:\n      files: [custom/*.data]\n";
    let r = Repo::new(&check, &[("hushrun.yaml", policy)]);
    let out = unit(r.hushrun(), &[]);
    says(&out, ": 7 tests, 2 failed, 1 skipped; it won't run again");
    assert_eq!(
        r.results()[0]["reports"]["files"][0]["path"],
        "custom/results.data"
    );
}

/// S6: a broken report is no summary, and the command's exit code stands.
// covers: C-27
#[test]
fn a_broken_report_changes_nothing_of_the_run() {
    let r = Repo::new(
        "printf '<testsuites><testsuite>' > junit.xml\nexit 3\n",
        &[],
    );
    let out = unit(r.hushrun(), &[]);
    assert_eq!(out.status.code(), Some(3), "{}", said(&out));
    says(&out, "hushrun: unit failed in ");
    says(&out, " (exit 3)\n");
    let results = r.results();
    assert_eq!(results[0]["result"], "fail");
    assert!(results[0].get("reports").is_none(), "{results:?}");
}

/// S7: a report over 64 MiB drops its kind's summary, and only its kind's.
// covers: C-27
#[test]
fn a_report_too_big_to_read_drops_its_kind() {
    let check = format!(
        "{{ printf '<testsuite><!--'; head -c 68000000 /dev/zero | tr '\\000' x; printf -- '--><testcase name=\"a\"/></testsuite>'; }} > junit.xml\ncp '{}' lcov.info\n",
        vector("lcov/basic.info")
    );
    let r = Repo::new(&check, &[]);
    let out = unit(r.hushrun(), &[]);
    says(&out, ": 60.0% of lines covered; it won't run again");
    let reports = &r.results()[0]["reports"];
    assert!(reports.get("tests").is_none(), "{reports}");
    assert_eq!(reports["coverage"]["format"], "lcov");
}

/// S8: a skip shows its pass's summary, carries it, and writes GitHub's summary and outputs.
// covers: C-27
#[test]
fn a_skip_carries_and_shows_its_passs_summary() {
    let r = Repo::new(&writes_basic(), &[(WORKFLOW, JOB)]);
    let first = unit(r.on_github("acme/shop"), &[]);
    assert_eq!(first.status.code(), Some(0), "{}", said(&first));
    let (outputs, summary) = r.github();
    assert_eq!(
        outputs,
        "hushrun-result=pass\nhushrun-skipped=false\nhushrun-tests=7\nhushrun-tests-failed=2\nhushrun-coverage-lines=60.0\n"
    );
    assert_eq!(summary, "", "a run writes no step summary");
    let second = unit(r.on_github("acme/shop"), &[]);
    assert_eq!(second.status.code(), Some(0), "{}", said(&second));
    says(
        &second,
        "hushrun: unit skipped: this exact code passed in CI ",
    );
    says(
        &second,
        "): 7 tests, 2 failed, 1 skipped, 60.0% of lines covered\n",
    );
    let results = r.results();
    assert_eq!(results.len(), 2);
    assert_eq!(results[1]["result"], "skip");
    assert_eq!(results[1]["reports"], results[0]["reports"]);
    schema::assert_valid(&schema::load("run-result.v0.schema.json"), &results[1]);
    let (outputs, summary) = r.github();
    assert!(
        outputs.ends_with(
            "hushrun-result=skip\nhushrun-skipped=true\nhushrun-tests=7\nhushrun-tests-failed=2\nhushrun-coverage-lines=60.0\n"
        ),
        "{outputs}"
    );
    assert!(
        summary.starts_with("### unit skipped\n\nThis exact code passed in CI "),
        "{summary}"
    );
    assert!(
        summary.contains(" ([its run](https://github.com/acme/shop/actions/runs/41/attempts/1)), so this run saved "),
        "{summary}"
    );
    assert!(
        summary.ends_with(
            "\n\n| Tests | Failed | Skipped | Lines covered | Branches covered |\n|---:|---:|---:|---:|---:|\n| 7 | 2 | 1 | 60.0% (3 of 5) | 75.0% (3 of 4) |\n"
        ),
        "{summary}"
    );
}

/// S9: a pass from another repository of the org is neither named nor linked.
// covers: C-27
#[test]
fn a_pass_from_another_repository_is_not_linked() {
    let r = Repo::new(&writes_basic(), &[(WORKFLOW, JOB)]);
    let first = unit(r.on_github("acme/other"), &[]);
    assert_eq!(first.status.code(), Some(0), "{}", said(&first));
    let second = unit(r.on_github("acme/shop"), &[]);
    says(
        &second,
        "hushrun: unit skipped: this exact code passed in CI ",
    );
    let (_, summary) = r.github();
    assert!(
        summary.contains("This exact code passed in CI "),
        "{summary}"
    );
    assert!(!summary.contains("its run"), "{summary}");
    assert!(!summary.contains("acme/other"), "{summary}");
    assert!(!summary.contains("https://"), "{summary}");
}

/// S10: a failing run records its failures, and says them.
// covers: C-27
#[test]
fn a_failing_run_says_its_failures() {
    let junit = "<testsuite time=\"2\"><testcase name=\"a\"/><testcase name=\"b\"/><testcase name=\"c\"><failure/></testcase><testcase name=\"d\"><failure/></testcase><testcase name=\"e\"><error/></testcase></testsuite>";
    let r = Repo::new(&format!("printf '{junit}' > junit.xml\nexit 1\n"), &[]);
    let out = unit(r.hushrun(), &[]);
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    says(&out, " (exit 1): 5 tests, 3 failed\n");
    let tests = &r.results()[0]["reports"]["tests"];
    assert_eq!(
        *tests,
        json!({"total": 5, "failed": 2, "errors": 1, "skipped": 0, "durationMs": 2000})
    );
}

/// `cmd` sharing its results with `fake`.
fn shared(mut cmd: Command, fake: &FakeResults) -> Command {
    cmd.env("HUSHRUN_BACKEND", fake.url())
        .env("HUSHRUN_TOKEN", FakeResults::TOKEN);
    cmd
}

/// S11: `hushrun ready` shows a passed check's summary, in its text and its JSON.
// covers: C-27, C-22
#[test]
fn ready_shows_a_passed_checks_summary() {
    let fake = FakeResults::start();
    let r = Repo::new(&writes_basic(), &[(WORKFLOW, JOB)]);
    let ran = unit(shared(r.hushrun(), &fake), &[]);
    assert_eq!(ran.status.code(), Some(0), "{}", said(&ran));
    let text = shared(r.hushrun(), &fake)
        .arg("ready")
        .output()
        .expect("runs");
    says(&text, "passed  unit");
    says(
        &text,
        "7 tests, 2 failed, 1 skipped, 60.0% of lines covered\n",
    );
    let json = shared(r.hushrun(), &fake)
        .args(["ready", "--format", "json"])
        .output()
        .expect("runs");
    let value: Value =
        serde_json::from_slice(&json.stdout).unwrap_or_else(|e| panic!("{e}: {}", said(&json)));
    schema::assert_valid(&schema::load("ready.v2.schema.json"), &value);
    let check = &value["checks"][0];
    assert_eq!(check["state"], "passed", "{value}");
    assert_eq!(check["reports"], basic());
}

/// S12: a server that refuses the summary leaves the run unrecorded, and says why.
// covers: C-27, C-14
#[test]
fn a_refused_summary_is_said_and_the_exit_code_stands() {
    let fake = FakeResults::start();
    let refusal = json!({"error": {"code": "invalid", "message": "a result's `reports.files[0].kind` is tests or coverage"}});
    fake.answer("POST", "v0/results/", 400, refusal, 1);
    let r = Repo::new(&writes_basic(), &[]);
    let out = unit(shared(r.hushrun(), &fake), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    says(
        &out,
        ": 7 tests, 2 failed, 1 skipped, 60.0% of lines covered; not recorded: a result's `reports.files[0].kind` is tests or coverage\n",
    );
}

/// S15: a report that is a link to outside the repository is never read.
// covers: C-27
#[test]
fn a_report_that_links_outside_is_ignored() {
    let check = format!(
        "cp '{}' ../outside.xml\nln -sf ../outside.xml junit.xml\n",
        vector("junit/basic.xml")
    );
    let r = Repo::new(&check, &[]);
    let out = unit(r.hushrun(), &[]);
    says(&out, "hushrun: unit passed in ");
    assert!(!said(&out).contains("tests"), "{}", said(&out));
    // It found nothing it wrote, and says so.
    assert_eq!(
        r.results()[0]["reports"],
        json!({"filesComplete": true}),
        "{:?}",
        r.results()
    );
}
