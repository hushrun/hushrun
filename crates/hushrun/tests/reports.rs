//! \[ADR 0047\] A skip never breaks a report (C-26), end to end: in a CI job where a later step
//! reads a check's reports, a pass that counts there doesn't skip it. The check runs, records as
//! usual and says why; outside CI nothing changes. Scenarios R1 to R11 of the plan.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::git;
use serde_json::Value;

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
    "HUSHRUN_REPORTS_READ",
    "npm_lifecycle_event",
];

/// Where the hosted backend would be: nothing listens on the discard port.
const NO_HOST: &str = "http://127.0.0.1:9";

const WORKFLOW: &str = ".github/workflows/ci.yml";

/// The job `test`: it checks out, runs the check, then `steps`.
fn job(steps: &str) -> String {
    format!(
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh check.sh\n{steps}"
    )
}

const CODECOV: &str =
    "      - uses: codecov/codecov-action@v5\n        with:\n          files: coverage.xml\n";
const TEST_REPORTER: &str = "      - uses: dorny/test-reporter@v2\n        with:\n          name: tests\n          path: junit.xml\n          reporter: java-junit\n";
const SARIF: &str = "      - uses: github/codeql-action/upload-sarif@v3\n        with:\n          sarif_file: results.sarif\n";

/// The test check: the reports REPORTS names, then VERDICT.
const CHECK: &str = "for f in $REPORTS; do mkdir -p \"$(dirname \"$f\")\"; echo '<testsuite><testcase name=\"a\"/></testsuite>' > \"$f\"; done\nexit \"${VERDICT:-0}\"\n";

/// A repository with a remote, a check that passes, a cache and credentials of its own, and
/// \[ADR 0067\] a results server its runs share, in CI and out: CI keeps no results in a runner's
/// directory.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    fake: FakeResults,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        // A linter that passes, for the checks that are one.
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::fs::write(bin.join("eslint"), "#!/bin/sh\nexit 0\n").expect("write");
        let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(bin.join("eslint"), mode).expect("chmod");
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
        // It writes a test-results file at each path REPORTS names, as a check writes the reports a
        // later step reads, and exits with VERDICT, so a test can fail it on the same code.
        r.write("check.sh", CHECK);
        r.write(".gitignore", "*.xml\ncoverage/\ntest-results/\n");
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
        let path = std::env::var_os("PATH").expect("PATH");
        let path = std::env::join_paths(
            std::iter::once(self.root.join("bin")).chain(std::env::split_paths(&path)),
        )
        .expect("PATH");
        cmd.current_dir(&self.repo)
            .env("PATH", path)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        // What the developer's shell exports never reaches a runner a test starts.
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        shared(cmd, &self.fake)
    }

    /// `hushrun` in the GitHub Actions job `job` of `ci.yml`, or of `file`.
    fn on_github(&self, job: &str) -> Command {
        self.on_github_in(WORKFLOW, job)
    }

    fn on_github_in(&self, file: &str, job: &str) -> Command {
        let mut cmd = self.hushrun();
        cmd.env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", "acme/shop")
            .env(
                "GITHUB_WORKFLOW_REF",
                format!("acme/shop/{file}@refs/heads/main"),
            )
            .env("GITHUB_JOB", job)
            .env("GITHUB_WORKSPACE", &self.repo);
        cmd
    }

    /// `hushrun` in a GitLab job of one instance.
    fn on_gitlab(&self) -> Command {
        let mut cmd = self.hushrun();
        cmd.env("CI", "true")
            .env("GITLAB_CI", "true")
            .env("CI_JOB_NAME", "test")
            .env("CI_NODE_TOTAL", "1");
        cmd
    }

    /// The passes and fails recorded.
    fn results(&self) -> Vec<Value> {
        self.fake
            .recorded()
            .into_iter()
            .filter(|result| result["result"] != "skip")
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

/// `hushrun run unit -- sh check.sh`, the test check.
fn unit(mut cmd: Command) -> Output {
    cmd.args(["run", "unit", "--", "sh", "check.sh"])
        .output()
        .expect("runs")
}

/// `hushrun run lint -- eslint .`, a linter.
fn lint(mut cmd: Command) -> Output {
    cmd.args(["run", "lint", "--", "eslint", "."])
        .output()
        .expect("runs")
}

/// The line a run says when a later step reads its reports.
fn reads(check: &str, reader: &str) -> String {
    format!(
        "hushrun: {check} running: a later step reads its reports ({reader}), and a skip writes none\n"
    )
}

#[track_caller]
fn ran(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", said(out));
    assert!(!said(out).contains("skipped"), "{}", said(out));
}

#[track_caller]
fn skipped(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", said(out));
    assert!(said(out).contains("skipped"), "{}", said(out));
}

/// The second run of `run` in a CI job, once a first has passed there.
fn twice(first: Command, second: Command, run: fn(Command) -> Output) -> Output {
    ran(&run(first));
    run(second)
}

/// R1: a Codecov upload follows the test, and a pass of this job is on record.
// covers: C-26
#[test]
fn a_check_runs_when_a_later_step_reads_its_reports() {
    let r = Repo::new(&[(WORKFLOW, &job(CODECOV))]);
    let first = unit(r.on_github("test"));
    ran(&first);
    // It runs every time, so its pass says nothing of the next run.
    assert!(
        said(&first).contains("hushrun: unit passed in "),
        "{}",
        said(&first)
    );
    assert!(
        !said(&first).contains("won't run again"),
        "{}",
        said(&first)
    );
    let second = unit(r.on_github("test"));
    ran(&second);
    assert!(
        said(&second).contains(&reads("unit", "codecov/codecov-action")),
        "{}",
        said(&second)
    );
    // It records as usual: both runs are passes.
    let verdicts: Vec<String> = r
        .results()
        .iter()
        .map(|result| result["result"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(verdicts, ["pass", "pass"]);
}

/// R2: the same job, with nothing that reads reports.
// covers: C-26
#[test]
fn a_check_skips_when_no_step_reads_reports() {
    let r = Repo::new(&[(WORKFLOW, &job(""))]);
    let second = twice(r.on_github("test"), r.on_github("test"), unit);
    skipped(&second);
    assert!(!said(&second).contains("reports"), "{}", said(&second));
}

/// R3: a linter skips beside readers of coverage and test results, and runs beside one of SARIF.
// covers: C-26
#[test]
fn a_linter_runs_only_for_reports_it_may_write() {
    let r = Repo::new(&[(WORKFLOW, &job(&format!("{CODECOV}{TEST_REPORTER}")))]);
    skipped(&twice(r.on_github("test"), r.on_github("test"), lint));
    // The test beside it runs, naming the first reader.
    let second = twice(r.on_github("test"), r.on_github("test"), unit);
    assert!(
        said(&second).contains(&reads("unit", "codecov/codecov-action")),
        "{}",
        said(&second)
    );
    let r = Repo::new(&[(WORKFLOW, &job(&format!("{CODECOV}{SARIF}")))]);
    let second = twice(r.on_github("test"), r.on_github("test"), lint);
    ran(&second);
    assert!(
        said(&second).contains(&reads("lint", "github/codeql-action/upload-sarif")),
        "{}",
        said(&second)
    );
}

/// R4: the only reader runs on failure, and a skip succeeds.
// covers: C-26
#[test]
fn a_reader_that_runs_only_on_failure_doesnt_stop_a_skip() {
    let on_failure = "      - uses: actions/upload-artifact@v4\n        if: ${{ failure() }}\n        with:\n          name: junit\n          path: junit.xml\n";
    let r = Repo::new(&[(WORKFLOW, &job(on_failure))]);
    skipped(&twice(r.on_github("test"), r.on_github("test"), unit));
}

/// R5: an artifact reads reports when its path names one.
// covers: C-26
#[test]
fn an_artifact_reads_reports_when_its_path_names_one() {
    let upload = |path: &str, extra: &str| {
        format!(
            "      - uses: actions/upload-artifact@v4\n        with:\n          name: out\n          path: {path}\n{extra}"
        )
    };
    let r = Repo::new(&[(WORKFLOW, &job(&upload("dist/", "")))]);
    skipped(&twice(r.on_github("test"), r.on_github("test"), unit));
    // The check writes what the upload reads.
    let writing = |r: &Repo, reports: &str| {
        let mut cmd = r.on_github("test");
        cmd.env("REPORTS", reports);
        cmd
    };
    let r = Repo::new(&[(WORKFLOW, &job(&upload("coverage/", "")))]);
    let second = twice(
        writing(&r, "coverage/junit.xml"),
        writing(&r, "coverage/junit.xml"),
        unit,
    );
    assert!(
        said(&second).contains(&reads("unit", "actions/upload-artifact")),
        "{}",
        said(&second)
    );
    let r = Repo::new(&[(
        WORKFLOW,
        &job(&upload(
            "'test-results/**/*.xml'",
            "          if-no-files-found: error\n",
        )),
    )]);
    let second = twice(
        writing(&r, "test-results/unit.xml"),
        writing(&r, "test-results/unit.xml"),
        unit,
    );
    ran(&second);
    assert!(
        said(&second).contains(&reads("unit", "actions/upload-artifact")),
        "{}",
        said(&second)
    );
}

/// R6: a `run:` step reads reports through a command that does.
// covers: C-26
#[test]
fn a_command_that_reads_reports_makes_its_step_a_reader() {
    for (run, reader) in [
        (
            "codecovcli upload-process -f coverage.xml",
            "run: codecovcli upload-process -f…",
        ),
        (
            "cargo llvm-cov report --lcov --output-path lcov.info",
            "run: cargo llvm-cov report --lcov…",
        ),
    ] {
        let r = Repo::new(&[(WORKFLOW, &job(&format!("      - run: {run}\n")))]);
        let second = twice(r.on_github("test"), r.on_github("test"), unit);
        ran(&second);
        assert!(
            said(&second).contains(&reads("unit", reader)),
            "{run}: {}",
            said(&second)
        );
    }
}

/// R7: the reader is a step of a local composite action the job uses.
// covers: C-26
#[test]
fn a_reader_inside_a_local_composite_action_counts() {
    let r = Repo::new(&[
        (WORKFLOW, &job("      - uses: ./.github/actions/coverage\n")),
        (
            ".github/actions/coverage/action.yml",
            "name: coverage\nruns:\n  using: composite\n  steps:\n    - uses: codecov/codecov-action@v5\n      with:\n        files: coverage.xml\n",
        ),
    ]);
    let second = twice(r.on_github("test"), r.on_github("test"), unit);
    ran(&second);
    assert!(
        said(&second).contains(&reads("unit", "./.github/actions/coverage")),
        "{}",
        said(&second)
    );
}

/// R8: hushrun.yaml says the repository's readers handle a skip.
// covers: C-26, C-5
#[test]
fn reports_ci_skip_lets_ci_skip() {
    let r = Repo::new(&[
        (WORKFLOW, &job(CODECOV)),
        (
            "hushrun.yaml",
            "version: 1\nrepository: github.com/acme/shop\nchecks:\n  unit:\n    reports:\n      ci: skip\n",
        ),
    ]);
    skipped(&twice(r.on_github("test"), r.on_github("test"), unit));
    // Another check of the job still runs for them.
    let second = twice(r.on_github("test"), r.on_github("test"), |mut cmd| {
        cmd.args(["run", "e2e", "--", "sh", "check.sh"])
            .output()
            .expect("runs")
    });
    assert!(
        said(&second).contains(&reads("e2e", "codecov/codecov-action")),
        "{}",
        said(&second)
    );
}

/// R9: on GitLab, nothing is read at run time, and HUSHRUN_REPORTS_READ says it.
// covers: C-26
#[test]
fn hushrun_reports_read_says_a_job_reads_reports() {
    let r = Repo::new(&[]);
    let with = |value: &str| {
        let mut cmd = r.on_gitlab();
        cmd.env("HUSHRUN_REPORTS_READ", value);
        cmd
    };
    ran(&unit(r.on_gitlab()));
    let second = unit(with("true"));
    ran(&second);
    assert!(
        said(&second).contains(
            "hushrun: unit running: HUSHRUN_REPORTS_READ says this job reads reports, and a skip writes none\n"
        ),
        "{}",
        said(&second)
    );
    skipped(&unit(with("false")));
    let maybe = unit(with("maybe"));
    skipped(&maybe);
    assert!(
        said(&maybe).contains(
            "hushrun: HUSHRUN_REPORTS_READ is \"maybe\", not true or false, so it's ignored\n"
        ),
        "{}",
        said(&maybe)
    );
    // On GitHub too, and then a linter has no exemption: nothing says what the readers read.
    let r = Repo::new(&[(WORKFLOW, &job(""))]);
    ran(&lint(r.on_github("test")));
    let mut cmd = r.on_github("test");
    cmd.env("HUSHRUN_REPORTS_READ", "1");
    let second = lint(cmd);
    ran(&second);
    assert!(
        said(&second).contains("HUSHRUN_REPORTS_READ says this job reads reports"),
        "{}",
        said(&second)
    );
}

/// R10: outside CI, nothing changes.
// covers: C-26
#[test]
fn outside_ci_a_reader_changes_nothing() {
    let r = Repo::new(&[(WORKFLOW, &job(CODECOV))]);
    let first = unit(r.hushrun());
    ran(&first);
    assert!(
        said(&first).contains("it won't run again"),
        "{}",
        said(&first)
    );
    let second = unit(r.hushrun());
    skipped(&second);
    assert!(!said(&second).contains("reports"), "{}", said(&second));
    let mut cmd = r.hushrun();
    cmd.env("HUSHRUN_REPORTS_READ", "true");
    skipped(&unit(cmd));
}

/// R11: the workflow file can't be read, and the run can't tell which job it is.
// covers: C-26, C-14
#[test]
fn a_workflow_that_cant_be_read_runs_as_before() {
    let r = Repo::new(&[(WORKFLOW, &job(CODECOV))]);
    let missing = ".github/workflows/missing.yml";
    ran(&unit(r.on_github_in(missing, "test")));
    let second = unit(r.on_github_in(missing, "test"));
    ran(&second);
    assert!(
        said(&second).contains(
            "hushrun: unit running: .github/workflows/missing.yml can't be read here, so hushrun can't tell its variants, and no pass skips it\n"
        ),
        "{}",
        said(&second)
    );
}

/// The job of S14: two checks, then a Codecov upload of `coverage.xml` and nothing else.
fn two_checks(codecov: &str) -> String {
    format!(
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run unit -- sh check.sh\n      - run: hushrun run cov -- sh check.sh\n      - uses: codecov/codecov-action@v5\n        with:\n{codecov}"
    )
}

const ONLY_COVERAGE_XML: &str = "          files: coverage.xml\n          disable_search: true\n";

/// `check` in the job, writing `reports`.
fn writes(r: &Repo, check: &str, reports: &str) -> Output {
    let mut cmd = r.on_github("test");
    cmd.env("REPORTS", reports)
        .args(["run", check, "--", "sh", "check.sh"]);
    cmd.output().expect("runs")
}

/// S14: in a job with two checks and an upload of one's coverage, the other skips once it passed
/// there; what its pass wrote shows the upload doesn't read it.
// covers: C-26, C-27
#[test]
fn a_check_whose_pass_wrote_nothing_a_reader_reads_skips() {
    let r = Repo::new(&[(WORKFLOW, &two_checks(ONLY_COVERAGE_XML))]);
    let first = writes(&r, "unit", "junit.xml");
    ran(&first);
    // Its pass says the next one skips.
    assert!(
        said(&first).contains("it won't run again on this code"),
        "{}",
        said(&first)
    );
    ran(&writes(&r, "cov", "coverage.xml"));
    skipped(&writes(&r, "unit", "junit.xml"));
    let cov = writes(&r, "cov", "coverage.xml");
    ran(&cov);
    assert!(
        said(&cov).contains(&reads("cov", "codecov/codecov-action")),
        "{}",
        said(&cov)
    );
    // The pass's record says it named every file it wrote.
    let unit_pass = r
        .results()
        .into_iter()
        .find(|result| result["check"] == "unit" && result["result"] == "pass")
        .expect("unit's pass");
    assert_eq!(
        unit_pass["reports"]["files"],
        serde_json::json!([{"path": "junit.xml", "kind": "tests", "format": "junit"}])
    );
    assert_eq!(unit_pass["reports"]["filesComplete"], true);
}

/// S14's limits: whenever hushrun can't tell that no reader reads what the pass wrote, it runs.
// covers: C-26, C-14
#[test]
fn a_reader_hushrun_cant_bound_still_runs_the_check() {
    let unit_twice = |r: &Repo| {
        ran(&writes(r, "unit", "junit.xml"));
        writes(r, "unit", "junit.xml")
    };
    // It runs for the upload, which it says.
    #[track_caller]
    fn for_codecov(out: &Output) {
        ran(out);
        assert!(
            said(out).contains(&reads("unit", "codecov/codecov-action")),
            "{}",
            said(out)
        );
    }
    // Codecov looks for coverage of its own besides `files`, unless told not to.
    let r = Repo::new(&[(WORKFLOW, &two_checks("          files: coverage.xml\n"))]);
    for_codecov(&unit_twice(&r));
    // A path CI fills in.
    let r = Repo::new(&[(
        WORKFLOW,
        &two_checks("          files: ${{ env.REPORT }}\n          disable_search: true\n"),
    )]);
    for_codecov(&unit_twice(&r));
    // The workflow isn't in the check's scope, so its readers can change under the same key.
    let r = Repo::new(&[
        (WORKFLOW, &two_checks(ONLY_COVERAGE_XML)),
        (
            "hushrun.yaml",
            "version: 1\nrepository: github.com/acme/shop\nscopes:\n  app:\n    include: [\"check.sh\", \"hushrun.yaml\"]\n",
        ),
    ]);
    for_codecov(&unit_twice(&r));
    // The pass's record can't say what it wrote: a file it wrote isn't a report hushrun reads.
    let r = Repo::new(&[
        (WORKFLOW, &two_checks(ONLY_COVERAGE_XML)),
        ("html.sh", "echo '<html/>' > report.xml\nsh check.sh\n"),
    ]);
    let html = |r: &Repo| {
        let mut cmd = r.on_github("test");
        cmd.env("REPORTS", "junit.xml")
            .args(["run", "unit", "--", "sh", "html.sh"]);
        cmd.output().expect("runs")
    };
    ran(&html(&r));
    for_codecov(&html(&r));
    // This checkout isn't the job's workspace, where the readers' paths start.
    let r = Repo::new(&[(WORKFLOW, &two_checks(ONLY_COVERAGE_XML))]);
    let elsewhere = |r: &Repo| {
        let mut cmd = r.on_github("test");
        cmd.env("REPORTS", "junit.xml")
            .env("GITHUB_WORKSPACE", &r.root)
            .args(["run", "unit", "--", "sh", "check.sh"]);
        cmd.output().expect("runs")
    };
    ran(&elsewhere(&r));
    for_codecov(&elsewhere(&r));
    // Another hushrun recorded the pass: it may read the job's readers otherwise.
    let r = Repo::new(&[(WORKFLOW, &two_checks(ONLY_COVERAGE_XML))]);
    ran(&writes(&r, "unit", "junit.xml"));
    r.fake
        .rewrite(|result| result["tool"] = Value::from("hushrun 0.0.1"));
    for_codecov(&writes(&r, "unit", "junit.xml"));
}

/// `cmd` with the fake results server as its backend, so that CI would see a pass from here.
fn shared(mut cmd: Command, fake: &FakeResults) -> Command {
    cmd.env("HUSHRUN_BACKEND", fake.url())
        .env("HUSHRUN_TOKEN", FakeResults::TOKEN);
    cmd
}

/// `hushrun ready --format json` here, sharing results with `fake`: each check, its state and why.
fn ready(r: &Repo, fake: &FakeResults) -> Vec<(String, String, String)> {
    let out = shared(r.hushrun(), fake)
        .args(["ready", "--format", "json"])
        .output()
        .expect("runs");
    let value: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", said(&out)));
    value["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| {
            let text = |key: &str| check[key].as_str().unwrap_or_default().to_string();
            (text("check"), text("state"), text("why"))
        })
        .collect()
}

/// What `hushrun doctor --format json`'s `reports` check says, each line with its status.
fn doctor_says(r: &Repo) -> Vec<(String, String)> {
    let out = r
        .hushrun()
        .args(["doctor", "--format", "json"])
        .output()
        .expect("runs");
    let doctor: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", said(&out)));
    doctor["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter(|check| check["check"] == "reports")
        .map(|check| {
            let text = |key: &str| check[key].as_str().unwrap_or_default().to_string();
            (text("status"), text("detail"))
        })
        .collect()
}

/// `ready` says CI runs such a check every time, as it does for a job with several variants, unless
/// it failed here, which is still to fix first.
// covers: C-22, C-26
#[test]
fn ready_says_ci_runs_a_check_whose_reports_a_later_step_reads() {
    let fake = FakeResults::start();
    let r = Repo::new(&[(WORKFLOW, &job(CODECOV))]);
    let every_time = (
        "unit".to_string(),
        "ci".to_string(),
        "CI runs it every time: a later step reads its reports (codecov/codecov-action)"
            .to_string(),
    );
    assert_eq!(ready(&r, &fake), std::slice::from_ref(&every_time));
    ran(&unit(shared(r.hushrun(), &fake)));
    assert_eq!(ready(&r, &fake), [every_time]);
    // Outside CI a pass skips it, so the failure is forced.
    let mut fails = shared(r.hushrun(), &fake);
    fails
        .env("VERDICT", "1")
        .args(["run", "--force", "unit", "--", "sh", "check.sh"]);
    assert_eq!(fails.output().expect("runs").status.code(), Some(1));
    let checks = ready(&r, &fake);
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].1, "missing", "{checks:?}");
    // Where the repository's readers handle a skip, a pass here is enough.
    let r = Repo::new(&[
        (WORKFLOW, &job(CODECOV)),
        (
            "hushrun.yaml",
            "version: 1\nrepository: github.com/acme/shop\nchecks:\n  unit:\n    reports:\n      ci: skip\n",
        ),
    ]);
    ran(&unit(shared(r.hushrun(), &fake)));
    let checks = ready(&r, &fake);
    assert_ne!(checks[0].1, "ci", "{checks:?}");
}

/// It is safe, so doctor says it and warns about nothing.
// covers: C-22, C-26
#[test]
fn doctor_says_ci_runs_a_check_whose_reports_a_later_step_reads() {
    let r = Repo::new(&[(WORKFLOW, &job(CODECOV))]);
    assert_eq!(
        doctor_says(&r),
        [(
            "ok".to_string(),
            "unit: CI runs it every time, since codecov/codecov-action reads its reports"
                .to_string()
        )]
    );
    let r = Repo::new(&[(WORKFLOW, &job(""))]);
    assert_eq!(
        doctor_says(&r),
        [(
            "ok".to_string(),
            "no step in CI's files reads the reports of a wrapped check".to_string()
        )]
    );
    // A linter beside readers of coverage only: its skip leaves them as they were.
    let lint = "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run lint -- eslint .\n      - uses: codecov/codecov-action@v5\n";
    let r = Repo::new(&[(WORKFLOW, lint)]);
    assert_eq!(
        doctor_says(&r)[0].1,
        "no step in CI's files reads the reports of a wrapped check"
    );
}
