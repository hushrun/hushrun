//! A CI pass belongs to its job, and a job with several variants doesn't skip (ADR 0044), end to
//! end: a run's key holds no environment (C-9), so these two rules are what keeps one job's pass
//! from standing in for a run on another system (C-10, C-14), whatever an older init wrapped.
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
    "npm_lifecycle_event",
];

/// Where the hosted backend would be: nothing listens on the discard port.
const NO_HOST: &str = "http://127.0.0.1:9";

/// The check of the bug report: it exits with `VERDICT`, so one variant passes and one fails.
const CHECK: &str = "exit \"${VERDICT:-0}\"\n";

const CHECKOUT: &str = "      - uses: actions/checkout@v5\n";

const WRAPPED: &str = r#"{"scripts": {"test": "hushrun run -- sh check.sh"}}"#;

/// The workflow of the bug report, with `step` for what it runs.
fn matrix(step: &str) -> String {
    format!(
        "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, macos-latest]\n        node: [20, 25]\n    runs-on: ${{{{ matrix.os }}}}\n    steps:\n{CHECKOUT}{step}"
    )
}

/// `step` in a job on Linux and a job on macOS.
fn two_systems(step: &str) -> String {
    format!(
        "on: push\njobs:\n  linux:\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}{step}  mac:\n    runs-on: macos-latest\n    steps:\n{CHECKOUT}{step}"
    )
}

/// One job with `step`, on one system, whatever else the file holds.
fn one_job(step: &str) -> String {
    format!("on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}{step}")
}

const NPM_TEST: &str = "      - run: npm test\n";

/// A repository with a remote, a cache and credentials of its own, and \[ADR 0067\] a results
/// server that its runs share, in CI and out: CI keeps no results in a runner's directory.
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
        std::fs::create_dir_all(root.join("bin")).expect("mkdir");
        std::os::unix::fs::symlink(
            assert_cmd::cargo::cargo_bin("hushrun"),
            root.join("bin/hushrun"),
        )
        .expect("link");
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
        for (path, text) in files {
            r.write(path, text);
        }
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        self.write_bytes(path, text.as_bytes());
    }

    fn write_bytes(&self, path: &str, bytes: &[u8]) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write");
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
        shared(cmd, &self.fake)
    }

    /// `hushrun` in a GitHub Actions job of `workflow`.
    fn on_github(&self, workflow: &str, job: &str) -> Command {
        let mut cmd = self.hushrun();
        cmd.env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", "acme/shop")
            .env(
                "GITHUB_WORKFLOW_REF",
                format!("acme/shop/.github/workflows/{workflow}@refs/heads/main"),
            )
            .env("GITHUB_JOB", job);
        cmd
    }

    /// `hushrun` in a GitLab job.
    fn on_gitlab(&self, job: &str, total: &str) -> Command {
        let mut cmd = self.hushrun();
        cmd.env("CI", "true")
            .env("GITLAB_CI", "true")
            .env("CI_JOB_NAME", job)
            .env("CI_NODE_TOTAL", total);
        cmd
    }

    /// The passes and fails recorded, oldest first for each key.
    fn results(&self) -> Vec<Value> {
        self.fake
            .recorded()
            .into_iter()
            .filter(|result| result["result"] != "skip")
            .collect()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// What a command printed, both streams.
fn said(out: &Output) -> String {
    format!("{}{}", text(&out.stdout), text(&out.stderr))
}

/// `hushrun run NAME -- sh check.sh`, whose check exits with `verdict`.
fn run_named(mut cmd: Command, name: &str, verdict: &str) -> Output {
    cmd.env("VERDICT", verdict)
        .args(["run", name, "--", "sh", "check.sh"])
        .output()
        .expect("runs")
}

/// `npm test` as a job runs it: the wrapped script, whose check is `test` (C-9).
fn npm_test(cmd: Command, verdict: &str) -> Output {
    script(cmd, "test", verdict)
}

/// The wrapped script `name` as a job runs it, whose check is made from its name (C-9).
fn script(mut cmd: Command, name: &str, verdict: &str) -> Output {
    cmd.env("npm_lifecycle_event", name)
        .env("VERDICT", verdict)
        .args(["run", "--", "sh", "check.sh"])
        .output()
        .expect("runs")
}

#[track_caller]
fn passed(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", said(out));
    assert!(!said(out).contains("skipped"), "{}", said(out));
}

#[track_caller]
fn skipped(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", said(out));
    assert!(said(out).contains("skipped"), "{}", said(out));
}

/// It ran, so the check's failure is what it exited with.
#[track_caller]
fn failed(out: &Output) {
    assert_eq!(out.status.code(), Some(1), "{}", said(out));
    assert!(!said(out).contains("skipped"), "{}", said(out));
}

// Rule 1: a CI pass belongs to its job.

/// Two jobs with different `runs-on`, and no parsing at all to tell it.
// covers: C-10
#[test]
fn a_pass_of_one_job_doesnt_skip_another() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &two_systems(NPM_TEST)),
    ]);
    passed(&npm_test(r.on_github("ci.yml", "linux"), "0"));
    let second = npm_test(r.on_github("ci.yml", "mac"), "1");
    failed(&second);
    assert!(
        said(&second).contains("another CI job"),
        "{}",
        said(&second)
    );
}

/// The same job on the same code does: a re-run, or the same code on another branch.
// covers: C-10
#[test]
fn a_job_skips_on_its_own_pass() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &one_job(NPM_TEST)),
    ]);
    passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
    skipped(&npm_test(r.on_github("ci.yml", "test"), "1"));
}

/// A result says its job, and the schema takes it.
// covers: C-10
#[test]
fn a_result_names_its_job() {
    let r = Repo::new(&[(".github/workflows/ci.yml", &one_job(NPM_TEST))]);
    passed(&run_named(r.on_github("ci.yml", "test"), "unit", "0"));
    passed(&run_named(r.hushrun(), "local", "0"));
    let schema = schema::load("run-result.v0.schema.json");
    let jobs: Vec<Option<String>> = r
        .results()
        .iter()
        .map(|result| {
            schema::assert_valid(&schema, result);
            result["job"].as_str().map(str::to_string)
        })
        .collect();
    assert!(jobs.contains(&None), "{jobs:?}");
    let job = format!("test@{}", hushrun_ci::platform());
    assert!(jobs.contains(&Some(job)), "{jobs:?}");
}

/// What hushrun is for (plan §1): staging then production on one code, in two workflow files with
/// one job id on one system, the shape of every pair measured in ADR 0044.
// covers: C-10
#[test]
fn a_staging_and_a_production_workflow_share_a_pass() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/deploy-staging.yml", &one_job(NPM_TEST)),
        (
            ".github/workflows/deploy-production.yml",
            &one_job(NPM_TEST),
        ),
    ]);
    passed(&npm_test(r.on_github("deploy-staging.yml", "test"), "0"));
    skipped(&npm_test(r.on_github("deploy-production.yml", "test"), "1"));
}

/// One job id on two systems, such as a `test` job in a macOS nightly. A test runs on one system,
/// so the pass is rewritten to what that job would have recorded there.
// covers: C-10
#[test]
fn a_pass_from_one_job_id_on_another_system_doesnt_skip() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &one_job(NPM_TEST)),
        (".github/workflows/nightly.yml", &one_job(NPM_TEST)),
    ]);
    passed(&npm_test(r.on_github("nightly.yml", "test"), "0"));
    r.fake
        .rewrite(|pass| pass["job"] = "test@plan9/mips".into());

    let out = npm_test(r.on_github("ci.yml", "test"), "1");
    failed(&out);
    assert!(said(&out).contains("another CI job"), "{}", said(&out));
}

/// Finding 1: following hushrun's advice recreated the wrong skip. The outer names differ, the
/// script stays wrapped, and the inner run reused Linux's pass on macOS, so the outer recorded a
/// pass of a check that had failed.
// covers: C-10
#[test]
fn a_run_inside_a_named_run_doesnt_reuse_another_jobs_pass() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (
            "npm-test.sh",
            "npm_lifecycle_event=test exec hushrun run -- sh check.sh\n",
        ),
        (
            ".github/workflows/ci.yml",
            "on: push\njobs:\n  linux:\n    runs-on: ubuntu-latest\n    steps:\n      - run: hushrun run linux-test -- sh npm-test.sh\n  mac:\n    runs-on: macos-latest\n    steps:\n      - run: hushrun run mac-test -- sh npm-test.sh\n",
        ),
    ]);
    let outer = |job: &str, name: &str, verdict: &str| {
        let mut cmd = r.on_github("ci.yml", job);
        cmd.env("VERDICT", verdict)
            .args(["run", name, "--", "sh", "npm-test.sh"])
            .output()
            .expect("runs")
    };
    passed(&outer("linux", "linux-test", "0"));
    let mac = outer("mac", "mac-test", "1");
    failed(&mac);
    assert!(said(&mac).contains("mac-test failed"), "{}", said(&mac));
}

/// Finding 4's two cases: a directory written in a way nothing could read was a miss before, and
/// there is nothing to miss now. Whatever the check's directory, its job is its job.
// covers: C-10
#[test]
fn a_directory_however_written_doesnt_lose_the_job() {
    for step in [
        "      - run: npm test\n        working-directory: ${{ github.workspace }}/web\n",
        "      - run: cd $GITHUB_WORKSPACE/web && npm test\n",
        "      - run: npm --prefix web test\n",
    ] {
        let r = Repo::new(&[
            ("web/package.json", WRAPPED),
            ("web/check.sh", CHECK),
            (".github/workflows/ci.yml", &two_systems(step)),
        ]);
        let in_web = |mut cmd: Command| {
            cmd.current_dir(r.repo.join("web"));
            cmd
        };
        passed(&npm_test(in_web(r.on_github("ci.yml", "linux")), "0"));
        failed(&npm_test(in_web(r.on_github("ci.yml", "mac")), "1"));
    }
}

/// What hushrun can't read two jobs running is no matter either: `make`, `xvfb-run`, `run-s`,
/// `pnpm -r`, composite actions.
// covers: C-10
#[test]
fn a_start_nothing_can_read_doesnt_lose_the_job() {
    for step in [
        "      - run: make test\n",
        "      - run: xvfb-run -a npm test\n",
        "      - run: pnpm -r test\n",
        "      - uses: ./.github/actions/test\n",
    ] {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &two_systems(step)),
        ]);
        passed(&npm_test(r.on_github("ci.yml", "linux"), "0"));
        failed(&npm_test(r.on_github("ci.yml", "mac"), "1"));
    }
}

/// A pass recorded before results named their job can't say whose it is: one extra run, once.
// covers: C-10
#[test]
fn a_ci_pass_with_no_job_runs_once_more() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &one_job(NPM_TEST)),
    ]);
    passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
    r.fake.rewrite(|old| {
        old.as_object_mut().expect("object").remove("job");
    });
    passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
    skipped(&npm_test(r.on_github("ci.yml", "test"), "1"));
}

/// Laptops and agents with `provider: local` stay as C-9 says: their pass stands in for CI's.
// covers: C-10
#[test]
fn a_local_pass_skips_every_job() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &two_systems(NPM_TEST)),
    ]);
    passed(&npm_test(r.hushrun(), "0"));
    skipped(&npm_test(r.on_github("ci.yml", "linux"), "1"));
    skipped(&npm_test(r.on_github("ci.yml", "mac"), "1"));
}

/// V1: two jobs run one check on one code. Each finds the newest pass that counts in its own job,
/// so both skip on a re-run: the cost ADR 0044 stated, that each hid the other's pass, is gone.
// covers: C-10
#[test]
fn a_check_two_jobs_run_skips_in_both_on_a_rerun() {
    let both = |job: &dyn Fn(&str) -> Command| {
        passed(&npm_test(job("linux"), "0"));
        passed(&npm_test(job("mac"), "0"));
        skipped(&npm_test(job("linux"), "1"));
        skipped(&npm_test(job("mac"), "1"));
    };
    // On the repository's server.
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &two_systems(NPM_TEST)),
    ]);
    both(&|name| r.on_github("ci.yml", name));
    // On another, which answers for the job asked.
    let fake = FakeResults::start();
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &two_systems(NPM_TEST)),
    ]);
    both(&|name| shared(r.on_github("ci.yml", name), &fake));
    let asked: Vec<String> = fake
        .raw_requests()
        .iter()
        .filter(|request| request.method == "GET")
        .flat_map(|request| request.query.clone())
        .filter(|(name, _)| name == "job")
        .map(|(_, value)| value)
        .collect();
    assert!(
        asked.iter().any(|job| job.starts_with("linux@")),
        "{asked:?}"
    );
    assert!(asked.iter().any(|job| job.starts_with("mac@")), "{asked:?}");
}

/// V2: V1 against a server from before the query, which answers the newest result for the key:
/// as 0.7.0 did, each job's pass hides the other's, both run, and nothing fails.
// covers: C-10
#[test]
fn an_old_server_answers_the_newest_and_both_jobs_run() {
    let fake = FakeResults::start();
    fake.ignore_job_queries();
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &two_systems(NPM_TEST)),
    ]);
    let job = |name| shared(r.on_github("ci.yml", name), &fake);
    passed(&npm_test(job("linux"), "0"));
    passed(&npm_test(job("mac"), "0"));
    let again = npm_test(job("linux"), "0");
    passed(&again);
    assert!(said(&again).contains("another CI job"), "{}", said(&again));
}

/// `cmd` with the fake results server as its backend.
fn shared(mut cmd: Command, fake: &FakeResults) -> Command {
    cmd.env("HUSHRUN_BACKEND", fake.url())
        .env("HUSHRUN_TOKEN", FakeResults::TOKEN);
    cmd
}

/// Where no CI names a job, a pass counts as it did.
// covers: C-10
#[test]
fn a_ci_with_no_job_stays_as_it_was() {
    let r = Repo::new(&[("package.json", WRAPPED)]);
    let buildkite = || {
        let mut cmd = r.hushrun();
        cmd.env("CI", "true").env("BUILDKITE", "true");
        cmd
    };
    passed(&npm_test(buildkite(), "0"));
    skipped(&npm_test(buildkite(), "1"));
}

/// Finding 6, the likeliest failure: what two jobs say about their setup, compared as text, ended
/// skipping. It is compared with nothing now: a laptop's pass skips them all.
// covers: C-10
#[test]
fn what_jobs_say_of_their_setup_doesnt_stop_a_local_pass() {
    let node = |action: &str, version: &str| {
        format!(
            "      - uses: actions/setup-node@{action}\n        with:\n          node-version: {version}\n      - run: npm test\n"
        )
    };
    let release = |steps: &str| {
        format!(
            "on:\n  release:\n    types: [published]\njobs:\n  publish:\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}{steps}"
        )
    };
    for (ci, other) in [
        (one_job(&node("v5", "20")), release(&node("v4", "20"))),
        (one_job(&node("v5", "20")), release(&node("v5", "'20'"))),
        (
            one_job(NPM_TEST).replace("    steps:", "    env:\n      FORCE_COLOR: 1\n    steps:"),
            release(NPM_TEST),
        ),
        (
            one_job(NPM_TEST),
            release(NPM_TEST).replace("ubuntu-latest", "[ubuntu-latest]"),
        ),
        // Finding 8: hushrun's own variables were said not to count, and did.
        (
            one_job(NPM_TEST).replace(
                "    steps:",
                "    env:\n      HUSHRUN_BACKEND: x\n    steps:",
            ),
            release(NPM_TEST),
        ),
    ] {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &ci),
            (".github/workflows/release.yml", &other),
        ]);
        passed(&npm_test(r.hushrun(), "0"));
        skipped(&npm_test(r.on_github("ci.yml", "test"), "1"));
    }
}

/// What the review found held up, and stays: what says nothing of where a job runs.
// covers: C-10
#[test]
fn jobs_that_differ_by_name_if_or_needs_keep_skipping() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (
            ".github/workflows/ci.yml",
            &format!(
                "on: push\njobs:\n  test:\n    name: Unit\n    if: github.event_name == 'push'\n    timeout-minutes: 10\n    continue-on-error: true\n    permissions:\n      contents: read\n    runs-on: x\n    steps:\n{CHECKOUT}{NPM_TEST}  other:\n    needs: test\n    runs-on: x\n    steps:\n{CHECKOUT}{NPM_TEST}"
            ),
        ),
    ]);
    passed(&npm_test(r.hushrun(), "0"));
    skipped(&npm_test(r.on_github("ci.yml", "test"), "1"));
    skipped(&npm_test(r.on_github("ci.yml", "other"), "1"));
}

// Rule 2: a job with several variants doesn't skip.

/// The bug report: the same code passed on Node 16, then printed `skipped` and exited 0 on Node
/// 25, where the check exits 1. One job, so Rule 1 can't help.
// covers: C-14
#[test]
fn a_pass_of_one_variant_doesnt_skip_another() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &matrix(NPM_TEST)),
    ]);
    passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
    failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
}

/// What a variable reaching a CI job can hold: a good string, which any step's secret can be, and
/// values nothing can read. ADR 0046: none makes a named CI a sandbox.
fn strings(fake: &FakeResults) -> Vec<String> {
    let key = hushrun_attest::SigningKey::generate_p256().expect("a key");
    vec![
        fake.agent_string(&key),
        "x".to_string(),
        "${{ secrets.X }}".to_string(),
    ]
}

// covers: C-10, C-14
#[test]
fn a_string_doesnt_let_a_github_matrix_job_skip() {
    let fake = FakeResults::start();
    for string in strings(&fake) {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &matrix(NPM_TEST)),
        ]);
        let job = || {
            let mut cmd = r.on_github("ci.yml", "test");
            cmd.env("HUSHRUN_AGENT", &string);
            cmd
        };
        passed(&npm_test(job(), "0"));
        // The failing variant runs, and fails, with the first's pass recorded.
        failed(&npm_test(job(), "1"));
        let out = npm_test(job(), "1");
        assert!(
            said(&out).contains("has several variants"),
            "{string}: {}",
            said(&out)
        );
    }
}

// covers: C-10, C-14
#[test]
fn a_string_doesnt_let_a_gitlab_matrix_job_skip() {
    let fake = FakeResults::start();
    for string in strings(&fake) {
        let r = Repo::new(&[("check.sh", CHECK)]);
        let job = |name: &str, total: &str| {
            let mut cmd = r.on_gitlab(name, total);
            cmd.env("HUSHRUN_AGENT", &string);
            cmd
        };
        // `parallel`, as CI_NODE_TOTAL says, and `parallel:matrix`, as the job's name says.
        passed(&run_named(job("test 1/2", "2"), "test", "0"));
        failed(&run_named(job("test 2/2", "2"), "test", "1"));
        passed(&run_named(job("test: [ubuntu, 20]", "1"), "test", "0"));
        failed(&run_named(job("test: [ubuntu, 25]", "1"), "test", "1"));
    }
}

// covers: C-10
#[test]
fn a_string_doesnt_let_a_job_skip_on_another_jobs_pass() {
    let fake = FakeResults::start();
    for string in strings(&fake) {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &two_systems(NPM_TEST)),
        ]);
        let job = |name: &str| {
            let mut cmd = r.on_github("ci.yml", name);
            cmd.env("HUSHRUN_AGENT", &string);
            cmd
        };
        passed(&npm_test(job("linux"), "0"));
        failed(&npm_test(job("mac"), "1"));
    }
}

/// A laptop's pass doesn't skip them either: it stands for one system.
// covers: C-14
#[test]
fn a_local_pass_doesnt_skip_a_job_with_variants() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &matrix(NPM_TEST)),
    ]);
    passed(&npm_test(r.hushrun(), "0"));
    failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
}

/// One line says why, it doesn't promise what won't happen, and the result is still recorded.
// covers: C-14
#[test]
fn a_run_says_why_in_one_line_and_is_still_recorded() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &matrix(NPM_TEST)),
    ]);
    let out = npm_test(r.on_github("ci.yml", "test"), "0");
    let lines: Vec<String> = text(&out.stderr).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        lines[0],
        "hushrun: test running: job test of .github/workflows/ci.yml has several variants, and its HUSHRUN_VARIANT isn't ${{ toJSON(matrix) }}, so no pass skips it"
    );
    assert!(
        lines[1].starts_with("hushrun: test passed in "),
        "{lines:?}"
    );
    assert!(!lines[1].contains("won't run again"), "{lines:?}");
    assert_eq!(r.results().len(), 1);
    // Outside CI nothing changes, and it finds that pass.
    skipped(&npm_test(r.hushrun(), "1"));
}

/// Outside CI nothing changes, in a repository whose CI has variants.
// covers: C-14
#[test]
fn outside_ci_a_pass_still_skips() {
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &matrix(NPM_TEST)),
    ]);
    let first = npm_test(r.hushrun(), "0");
    assert!(
        said(&first).contains("it won't run again"),
        "{}",
        said(&first)
    );
    skipped(&npm_test(r.hushrun(), "1"));
    // An agent's sandbox that sets CI=true says it is none (§11.2).
    let mut sandbox = r.on_github("ci.yml", "test");
    sandbox.env("HUSHRUN_PROVIDER", "local");
    skipped(&npm_test(sandbox, "1"));
    // And a sandbox that sets bare CI=true, with the string it has been given.
    let fake = FakeResults::start();
    let key = hushrun_attest::SigningKey::generate_p256().expect("a key");
    let string = fake.agent_string(&key);
    let mut one = r.hushrun();
    one.env("CI", "true").env("HUSHRUN_AGENT", &string);
    passed(&npm_test(one, "0"));
    let mut two = r.hushrun();
    two.env("CI", "true").env("HUSHRUN_AGENT", &string);
    skipped(&npm_test(two, "1"));
}

/// `hushrun run test-${{ strategy.job-index }} --`, the advice: always a valid name, whatever the
/// matrix holds (finding 7: `18.x`, `macOS-latest`, `lts/*`).
// covers: C-14
#[test]
fn a_name_by_job_index_keeps_skipping_on_any_matrix() {
    let workflow = "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        node: [18.x, lts/*]\n        os: [macOS-latest, ubuntu-22.04]\n    runs-on: ${{ matrix.os }}\n    steps:\n      - run: hushrun run test-${{ strategy.job-index }} -- sh check.sh\n";
    let r = Repo::new(&[(".github/workflows/ci.yml", workflow)]);
    passed(&run_named(r.on_github("ci.yml", "test"), "test-0", "0"));
    skipped(&run_named(r.on_github("ci.yml", "test"), "test-0", "1"));
    failed(&run_named(r.on_github("ci.yml", "test"), "test-1", "1"));
}

/// `${{ matrix.x }}` for every dimension counts too.
// covers: C-14
#[test]
fn a_name_by_every_dimension_keeps_skipping() {
    let line = "      - run: hushrun run t-${{ matrix.os }}-${{ matrix.node }} -- sh check.sh\n";
    let r = Repo::new(&[(".github/workflows/ci.yml", &matrix(line))]);
    let job = || r.on_github("ci.yml", "test");
    passed(&run_named(job(), "t-macos-latest-20", "0"));
    skipped(&run_named(job(), "t-macos-latest-20", "1"));
    failed(&run_named(job(), "t-macos-latest-25", "1"));
}

/// A name that says the system and not the version doesn't tell two versions apart.
// covers: C-14
#[test]
fn a_name_must_differ_by_every_dimension() {
    let line = "      - run: hushrun run t-${{ matrix.os }} -- sh check.sh\n";
    let r = Repo::new(&[(".github/workflows/ci.yml", &matrix(line))]);
    passed(&run_named(
        r.on_github("ci.yml", "test"),
        "t-macos-latest",
        "0",
    ));
    failed(&run_named(
        r.on_github("ci.yml", "test"),
        "t-macos-latest",
        "1",
    ));
}

/// Only a name that is this run's counts: another line of the job says nothing about this one.
// covers: C-14
#[test]
fn another_runs_name_doesnt_count() {
    let line = "      - run: hushrun run lint-${{ strategy.job-index }} -- sh check.sh\n      - run: hushrun run test -- sh check.sh\n";
    let r = Repo::new(&[(".github/workflows/ci.yml", &matrix(line))]);
    passed(&run_named(r.on_github("ci.yml", "test"), "test", "0"));
    failed(&run_named(r.on_github("ci.yml", "test"), "test", "1"));
}

/// Finding 3: nothing but NAME counts. These never reached the key, or don't always.
// covers: C-14
#[test]
fn only_the_name_tells_variants_apart() {
    let pair = "${{ matrix.os }} ${{ matrix.node }}";
    for (script, line) in [
        (WRAPPED, format!("      - run: npm test --x={pair}\n")),
        (
            r#"{"scripts": {"test": "hushrun run -- sh check.sh && echo done"}}"#,
            format!("      - run: npm test -- {pair}\n"),
        ),
        (
            WRAPPED,
            "      - run: |\n          cd ${{ matrix.os }}-${{ matrix.node }} && npm ci && cd ..\n          npm test\n"
                .to_string(),
        ),
        (
            WRAPPED,
            "      - run: |\n          (cd ${{ matrix.os }}-${{ matrix.node }} && npm ci)\n          npm test\n"
                .to_string(),
        ),
        (
            WRAPPED,
            "      - run: |\n          pushd ${{ matrix.os }}-${{ matrix.node }}\n          npm ci\n          popd\n          npm test\n"
                .to_string(),
        ),
        (
            WRAPPED,
            "      - run: npm test\n        working-directory: ${{ matrix.os }}-${{ matrix.node }}\n"
                .to_string(),
        ),
        (
            WRAPPED,
            format!("      - run: hushrun run --scope ${{{{ matrix.os }}}} test -- sh check.sh # {pair}\n"),
        ),
        (
            WRAPPED,
            "      - run: A=${{ matrix.os }} B=${{ matrix.node }} hushrun run test -- sh check.sh\n"
                .to_string(),
        ),
    ] {
        let r = Repo::new(&[
            ("package.json", script),
            (".github/workflows/ci.yml", &matrix(&line)),
        ]);
        passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
        failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
    }
}

/// Finding 2: the reported bug survived in reusable workflows. The job isn't in the file CI names,
/// so hushrun can't tell, and no pass skips.
// covers: C-14
#[test]
fn a_reusable_workflows_job_doesnt_skip() {
    let called = format!(
        "on: workflow_call\njobs:\n  test:\n    runs-on: ${{{{ inputs.os }}}}\n    steps:\n{CHECKOUT}{NPM_TEST}"
    );
    let calls = "    uses: ./.github/workflows/test.yml\n";
    let strategy = "    strategy:\n      matrix:\n        os: [a, b]\n";
    for caller in [
        // A matrix on the caller.
        format!("on: push\njobs:\n  ci:\n{strategy}{calls}"),
        // A matrix in the called file.
        format!("on: push\njobs:\n  ci:\n{calls}"),
        // Two calls with different inputs.
        format!(
            "on: push\njobs:\n  a:\n{calls}    with:\n      os: a\n  b:\n{calls}    with:\n      os: b\n"
        ),
        // The called job has the caller's job's name.
        format!("on: push\njobs:\n  test:\n{calls}"),
    ] {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &caller),
            (".github/workflows/test.yml", &called),
        ]);
        let first = npm_test(r.on_github("ci.yml", "test"), "0");
        passed(&first);
        assert!(
            said(&first).contains("no pass skips it"),
            "{}",
            said(&first)
        );
        failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
        // A laptop's pass doesn't stand in for it either: nothing says it has one variant.
        passed(&npm_test(r.hushrun(), "0"));
        failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
    }
}

/// The second review's first finding, on the line ci.md gives: `test-${{ strategy.job-index }}`
/// claimed `test-unit`, the wrapped script `npm test` starts inside it, so a failing variant printed
/// `skipped`, and the run around it recorded a pass.
// covers: C-14
#[test]
fn a_name_doesnt_claim_a_scripts_run_inside_it() {
    let package =
        r#"{"scripts": {"test": "npm run test:unit", "test:unit": "hushrun run -- sh check.sh"}}"#;
    let by_index = "      - run: npx hushrun run test-${{ strategy.job-index }} -- npm test\n";
    let one_key = format!(
        "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        node: [18, 20]\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}      - run: npx hushrun run test-${{{{ matrix.node }}}} -- npm test\n"
    );
    for workflow in [matrix(by_index), one_key] {
        let r = Repo::new(&[
            ("package.json", package),
            (".github/workflows/ci.yml", &workflow),
        ]);
        passed(&script(r.on_github("ci.yml", "test"), "test:unit", "0"));
        let second = script(r.on_github("ci.yml", "test"), "test:unit", "1");
        failed(&second);
        assert!(
            said(&second).contains("no pass skips it"),
            "{}",
            said(&second)
        );
    }
}

/// The wrapped script `name` inside `hushrun run <outer> -- sh inner.sh`, in a matrix job.
fn inside_a_run(r: &Repo, outer: &str, verdict: &str) -> Output {
    r.on_github("ci.yml", "test")
        .env("VERDICT", verdict)
        .args(["run", outer, "--", "sh", "inner.sh"])
        .output()
        .expect("runs")
}

/// `test-1` is a name `test-${{ strategy.job-index }}` can take. Inside that run it isn't a variant's
/// run, whatever its name looks like: the outer run's key is what differs. A file writes it out
/// here, so only the marker can tell: a script's name, which `test:1` makes, stops earlier.
// covers: C-14
#[test]
fn a_run_inside_a_named_run_doesnt_count_by_an_index() {
    let line = "      - run: hushrun run test-${{ strategy.job-index }} -- sh inner.sh\n";
    let r = Repo::new(&[
        ("inner.sh", "exec hushrun run test-1 -- sh check.sh\n"),
        (".github/workflows/ci.yml", &matrix(line)),
    ]);
    passed(&inside_a_run(&r, "test-0", "0"));
    let other = inside_a_run(&r, "test-5", "1");
    failed(&other);
    assert!(
        said(&other).contains("this run is inside another, so no pass skips it"),
        "{}",
        said(&other)
    );
    // The outer run's own name counts as written, and its pass skips the whole of it.
    skipped(&inside_a_run(&r, "test-0", "1"));
}

/// `test-unit`, which `test-${{ matrix.suite }}` can take when `suite` lists it, the same way.
// covers: C-14
#[test]
fn a_run_inside_a_named_run_doesnt_count_by_a_value() {
    let workflow = format!(
        "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        suite: [unit, e2e]\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}      - run: hushrun run test-${{{{ matrix.suite }}}} -- sh inner.sh\n"
    );
    let r = Repo::new(&[
        ("inner.sh", "exec hushrun run test-unit -- sh check.sh\n"),
        (".github/workflows/ci.yml", &workflow),
    ]);
    passed(&inside_a_run(&r, "test-unit", "0"));
    failed(&inside_a_run(&r, "test-e2e", "1"));
    skipped(&inside_a_run(&r, "test-unit", "1"));
}

/// The marker says nothing outside a job with variants: a run inside another reuses a pass there as
/// it always has.
// covers: C-14
#[test]
fn a_run_inside_another_skips_as_ever_in_a_plain_job() {
    let r = Repo::new(&[
        (
            "inner.sh",
            "npm_lifecycle_event=test exec hushrun run -- sh check.sh\n",
        ),
        (
            ".github/workflows/ci.yml",
            &one_job("      - run: hushrun run outer -- sh inner.sh\n"),
        ),
    ]);
    passed(&inside_a_run(&r, "outer", "0"));
    skipped(&inside_a_run(&r, "other", "1"));
}

/// The same names from a step of their own in that job, which the marker can't see. Only a line of
/// the workflow can name a variant, so a run counts by its name only when it is one.
// covers: C-14
#[test]
fn a_run_in_another_step_doesnt_count_by_a_name_it_could_be() {
    let named = "      - run: hushrun run test-${{ strategy.job-index }} -- sh a.sh\n";
    // A script's name, as `npm run test:1` makes it.
    let line = format!("{named}      - run: npm run test:1\n");
    let r = Repo::new(&[(".github/workflows/ci.yml", &matrix(&line))]);
    passed(&script(r.on_github("ci.yml", "test"), "test:1", "0"));
    failed(&script(r.on_github("ci.yml", "test"), "test:1", "1"));
    // A name a script writes out.
    let written = |verdict: &str| {
        let mut cmd = r.on_github("ci.yml", "test");
        cmd.env("npm_lifecycle_event", "test:2");
        run_named(cmd, "test-2", verdict)
    };
    passed(&written("0"));
    failed(&written("1"));
    // A name a line of the job writes out, the same in every variant.
    let line = format!("{named}      - run: hushrun run test-1 -- sh check.sh\n");
    let r = Repo::new(&[(".github/workflows/ci.yml", &matrix(&line))]);
    passed(&run_named(r.on_github("ci.yml", "test"), "test-1", "0"));
    failed(&run_named(r.on_github("ci.yml", "test"), "test-1", "1"));
    // A program's name, which a run with no name takes.
    let by_tool = format!(
        "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        tool: [sh, go]\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}      - run: hushrun run ${{{{ matrix.tool }}}} -- sh a.sh\n      - run: hushrun run -- sh check.sh\n"
    );
    let r = Repo::new(&[(".github/workflows/ci.yml", &by_tool)]);
    let unnamed = |verdict: &str| {
        r.on_github("ci.yml", "test")
            .env("VERDICT", verdict)
            .args(["run", "--", "sh", "check.sh"])
            .output()
            .expect("runs")
    };
    passed(&unnamed("0"));
    failed(&unnamed("1"));
}

/// A line of the workflow started through a package manager is still that line.
// covers: C-14
#[test]
fn a_named_line_keeps_skipping_through_npx_and_bunx() {
    let line = "      - run: npx hushrun run test-${{ strategy.job-index }} -- sh check.sh\n";
    let r = Repo::new(&[(".github/workflows/ci.yml", &matrix(line))]);
    for (event, name) in [("npx", "test-0"), ("bunx", "test-1"), ("hushrun", "test-2")] {
        let run = |verdict: &str| {
            let mut cmd = r.on_github("ci.yml", "test");
            cmd.env("npm_lifecycle_event", event);
            run_named(cmd, name, verdict)
        };
        passed(&run("0"));
        skipped(&run("1"));
    }
}

/// A variant's own name still skips, by what CI can fill in, and by nothing else.
// covers: C-14
#[test]
fn a_key_fills_in_only_the_values_it_lists() {
    let job = |values: &str| {
        format!(
            "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        node: {values}\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}      - run: hushrun run t-${{{{ matrix.node }}}} -- sh check.sh\n"
        )
    };
    let listed = job("[18, 20]");
    let r = Repo::new(&[(".github/workflows/ci.yml", &listed)]);
    passed(&run_named(r.on_github("ci.yml", "test"), "t-18", "0"));
    skipped(&run_named(r.on_github("ci.yml", "test"), "t-18", "1"));
    // What the list doesn't hold isn't that run's name, and it stands for nothing.
    passed(&run_named(r.on_github("ci.yml", "test"), "t-22", "0"));
    failed(&run_named(r.on_github("ci.yml", "test"), "t-22", "1"));
    // An expression in the list: nothing says what it fills in.
    let told_at_run_time = job("[18, '${{ inputs.node }}']");
    let r = Repo::new(&[(".github/workflows/ci.yml", &told_at_run_time)]);
    passed(&run_named(r.on_github("ci.yml", "test"), "t-18", "0"));
    failed(&run_named(r.on_github("ci.yml", "test"), "t-18", "1"));
}

/// `.github/workflows/<file>`, as a job's `uses:` starts it.
const CALLS: &str = "  call:\n    uses: ./.github/workflows/called.yml\n";

/// A job `id` with several variants, in a file jobs call.
fn called(id: &str) -> String {
    format!(
        "on: workflow_call\njobs:\n  {id}:\n    strategy:\n      matrix:\n        node: [16, 25]\n    runs-on: ubuntu-latest\n    steps:\n{CHECKOUT}{NPM_TEST}"
    )
}

/// The second review's second finding. CI names the caller's file and the called file's job, so a
/// plain job of the caller with that id read as this job, and its one variant let a pass skip.
// covers: C-14
#[test]
fn a_called_job_with_a_plain_jobs_id_doesnt_skip() {
    let caller = format!("{}{CALLS}", one_job("      - run: echo hi\n"));
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &caller),
        (".github/workflows/called.yml", &called("test")),
    ]);
    let first = npm_test(r.on_github("ci.yml", "test"), "0");
    passed(&first);
    assert!(
        said(&first).contains(
            "hushrun: test running: .github/workflows/called.yml, which .github/workflows/ci.yml calls, has a job test too, so hushrun can't tell its variants, and no pass skips it"
        ),
        "{}",
        said(&first)
    );
    failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
}

/// A called file may call another, and two may call each other.
// covers: C-14
#[test]
fn a_call_is_followed_as_far_as_this_repository_goes() {
    let caller = format!("{}{CALLS}", one_job(NPM_TEST));
    let through = "on: workflow_call\njobs:\n  more:\n    uses: ./.github/workflows/last.yml\n  back:\n    uses: ./.github/workflows/ci.yml\n";
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &caller),
        (".github/workflows/called.yml", through),
        (".github/workflows/last.yml", &called("test")),
    ]);
    passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
    failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
}

/// A file that can't be read may hold that id.
// covers: C-14
#[test]
fn a_call_that_cant_be_read_doesnt_skip() {
    let job = |calls: &str| format!("{}  call:\n    uses: {calls}\n", one_job(NPM_TEST));
    for calls in [
        "./.github/workflows/gone.yml",
        "./.github/workflows/../../check.sh",
        "./check.sh",
    ] {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &job(calls)),
        ]);
        let first = npm_test(r.on_github("ci.yml", "test"), "0");
        passed(&first);
        assert!(
            said(&first).contains("can't be read here"),
            "{}",
            said(&first)
        );
        failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
    }
}

/// A plain job keeps skipping beside a call that can't be what is running: a called file without
/// its id, or another repository's, which can't be read (C-14 lists that limit).
// covers: C-14
#[test]
fn a_call_that_cant_be_this_job_says_nothing() {
    for call in [
        CALLS,
        "  call:\n    uses: acme/rules/.github/workflows/called.yml@v1\n",
    ] {
        let caller = format!("{}{call}", one_job(NPM_TEST));
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &caller),
            (".github/workflows/called.yml", &called("other")),
        ]);
        passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
        skipped(&npm_test(r.on_github("ci.yml", "test"), "1"));
    }
}

/// Whatever can't be read: no pass skips, one line says so, and nothing fails for it.
// covers: C-14
#[test]
fn a_job_hushrun_cant_read_doesnt_skip() {
    let matrix = matrix(NPM_TEST);
    let mut not_utf8 = matrix.clone().into_bytes();
    not_utf8.extend(b"# caf\xe9\n");
    let not_yaml = b"on: [push\njobs: {\n".to_vec();
    let duplicate = b"jobs:\n  test:\n    runs-on: a\n  test:\n    runs-on: b\n".to_vec();
    for (bytes, why) in [
        (not_utf8, "can't be read as YAML"),
        (not_yaml, "can't be read as YAML"),
        (duplicate, "can't be read as YAML"),
    ] {
        let r = Repo::new(&[("package.json", WRAPPED)]);
        r.write_bytes(".github/workflows/ci.yml", &bytes);
        git::commit_all(&r.repo, "a workflow");
        passed(&npm_test(r.hushrun(), "0"));
        let out = npm_test(r.on_github("ci.yml", "test"), "1");
        failed(&out);
        assert!(said(&out).contains(why), "{}", said(&out));
    }
    // A sparse checkout, or a file that isn't there.
    let r = Repo::new(&[("package.json", WRAPPED)]);
    passed(&npm_test(r.hushrun(), "0"));
    failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
}

/// What GitHub says of the job, when it doesn't name one of this repository's.
// covers: C-14
#[test]
fn a_job_ci_doesnt_name_doesnt_skip() {
    for (reference, job) in [
        ("", "test"),
        ("garbage", "test"),
        ("acme/shop/.github/workflows/ci.yml", "test"),
        ("acme/shop/../../etc/passwd@x", "test"),
        (
            "acme/rules/.github/workflows/ci.yml@refs/heads/main",
            "test",
        ),
        ("acme/shop/.github/workflows/ci.yml@refs/heads/main", ""),
    ] {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &one_job(NPM_TEST)),
        ]);
        passed(&npm_test(r.hushrun(), "0"));
        let mut cmd = r.on_github("ci.yml", "test");
        cmd.env("GITHUB_WORKFLOW_REF", reference)
            .env("GITHUB_JOB", job);
        failed(&npm_test(cmd, "1"));
    }
}

/// `include:` items that are expressions can't be counted (finding 4).
// covers: C-14
#[test]
fn a_matrix_only_run_time_knows_doesnt_skip() {
    let job = |matrix: &str| {
        format!(
            "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n{matrix}    runs-on: x\n    steps:\n{CHECKOUT}{NPM_TEST}"
        )
    };
    for matrix in [
        "        include:\n          - ${{ fromJSON(needs.a.outputs.x) }}\n          - ${{ fromJSON(needs.a.outputs.y) }}\n",
        "        include: ${{ fromJSON(needs.a.outputs.x) }}\n",
        "        os: ${{ fromJSON(needs.a.outputs.x) }}\n",
        "        os: ['${{ fromJSON(needs.a.outputs.x) }}']\n",
    ] {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", &job(matrix)),
        ]);
        passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
        failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
    }
}

/// A matrix of one is no matrix, however it is written, and flow style and anchors are the YAML
/// parser's to read.
// covers: C-14
#[test]
fn a_matrix_of_one_skips_and_flow_style_is_read() {
    let one = "on: push\njobs:\n  test:\n    strategy:\n      matrix:\n        include:\n          - node: 20\n            os: a\n    runs-on: x\n    steps:\n      - run: npm test\n";
    let r = Repo::new(&[("package.json", WRAPPED), (".github/workflows/ci.yml", one)]);
    passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
    skipped(&npm_test(r.on_github("ci.yml", "test"), "1"));

    for flow in [
        "on: push\njobs:\n  test: {runs-on: x, strategy: {matrix: {os: [a, b]}}, steps: [{run: npm test}]}\n",
        "on: push\njobs:\n  test:\n    runs-on: x\n    strategy: &s\n      matrix:\n        os: [a, b]\n    steps:\n      - run: npm test\n  other:\n    runs-on: x\n    strategy: *s\n    steps:\n      - run: npm test\n",
        "on: push\njobs:\n  test:\n    \"strategy\":\n      'matrix':\n        \"os\": [a, b]\n    runs-on: x\n    steps:\n      - run: npm test\n",
    ] {
        let r = Repo::new(&[
            ("package.json", WRAPPED),
            (".github/workflows/ci.yml", flow),
        ]);
        passed(&npm_test(r.on_github("ci.yml", "test"), "0"));
        failed(&npm_test(r.on_github("ci.yml", "test"), "1"));
    }
}

// covers: C-14
#[test]
fn a_gitlab_jobs_instances_dont_skip() {
    let r = Repo::new(&[]);
    passed(&run_named(r.hushrun(), "test", "0"));
    failed(&run_named(r.on_gitlab("test 2/3", "3"), "test", "1"));
    let r = Repo::new(&[]);
    passed(&run_named(r.hushrun(), "test", "0"));
    failed(&run_named(
        r.on_gitlab("test: [ubuntu, 20]", "1"),
        "test",
        "1",
    ));
    // With one, its own pass skips.
    let r = Repo::new(&[]);
    passed(&run_named(r.on_gitlab("test", "1"), "test", "0"));
    skipped(&run_named(r.on_gitlab("test", "1"), "test", "1"));
}

/// GitLab's two jobs with two images, written the way its docs write them: Rule 1 needs none of
/// `extends`, `!reference`, `rules` or `include:` to be read.
// covers: C-10
#[test]
fn two_gitlab_jobs_dont_share_a_pass() {
    let r = Repo::new(&[(
        ".gitlab-ci.yml",
        ".test:\n  script:\n    - hushrun run test -- sh check.sh\n\ntest:20:\n  extends: .test\n  image: node:20\n\ntest:25:\n  extends: .test\n  image: node:25\n",
    )]);
    passed(&run_named(r.on_gitlab("test:20", "1"), "test", "0"));
    failed(&run_named(r.on_gitlab("test:25", "1"), "test", "1"));
}

/// A file that `extends` itself once ran past a minute. Nothing recurses through `extends` now.
// covers: C-15
#[test]
fn an_extends_that_goes_round_ends() {
    let mut pipeline = String::new();
    for name in ["a", "b", "c", "d", "e", "f"] {
        pipeline.push_str(&format!(
            ".{name}:\n  extends: [.a, .b, .c, .d, .e, .f]\n  script:\n    - npm test\n"
        ));
    }
    pipeline.push_str("test:\n  extends: .a\n  script:\n    - npm test\n");
    let r = Repo::new(&[
        ("package.json", r#"{"scripts": {"test": "node t.js"}}"#),
        (".gitlab-ci.yml", &pipeline),
    ]);
    let started = std::time::Instant::now();
    let plan = plan(&r);
    assert!(started.elapsed().as_secs() < 30, "{plan}");
}

fn plan(r: &Repo) -> String {
    let out = r
        .hushrun()
        .args(["init", "--dry-run", "--yes", "--no-browser", "--no-run"])
        .output()
        .expect("runs");
    assert!(out.status.success(), "{}", said(&out));
    said(&out)
}

/// The second review's fourth finding: the guard in `hushrun run` makes this safe, so refusing the
/// script added nothing, and cost laptops and agents their skips. The job gets what any job gets.
// covers: C-15
#[test]
fn init_wraps_a_script_a_job_with_variants_runs() {
    let r = Repo::new(&[
        ("package.json", r#"{"scripts": {"test": "node t.js"}}"#),
        (".github/workflows/ci.yml", &matrix(NPM_TEST)),
    ]);
    let plan = plan(&r);
    assert!(plan.contains("→ \"hushrun run -- node t.js\""), "{plan}");
    assert!(
        plan.contains("job test: adds permissions contents: read and id-token: write"),
        "{plan}"
    );
    assert!(!plan.contains("It leaves these scripts"), "{plan}");
    assert!(!plan.contains("several variants"), "{plan}");
}

/// What one workflow's matrix says doesn't reach another's push CI.
// covers: C-15
#[test]
fn a_matrix_in_a_nightly_workflow_doesnt_stop_wrapping() {
    let nightly = matrix(NPM_TEST).replace("on: push", "on:\n  schedule:\n    - cron: '0 3 * * *'");
    let r = Repo::new(&[
        ("package.json", r#"{"scripts": {"test": "node t.js"}}"#),
        (".github/workflows/ci.yml", &one_job(NPM_TEST)),
        (".github/workflows/nightly.yml", &nightly),
    ]);
    assert!(plan(&r).contains("→ \"hushrun run -- node t.js\""));
}

/// Two jobs need nothing from init: Rule 1 decides what they share, and ADR 0040 stands.
// covers: C-15
#[test]
fn init_wraps_a_script_two_jobs_run() {
    let r = Repo::new(&[
        ("package.json", r#"{"scripts": {"test": "node t.js"}}"#),
        (".github/workflows/ci.yml", &two_systems(NPM_TEST)),
    ]);
    let plan = plan(&r);
    assert!(plan.contains("→ \"hushrun run -- node t.js\""), "{plan}");
}

// covers: C-15
#[test]
fn init_wraps_a_script_a_gitlab_jobs_instances_run() {
    let r = Repo::new(&[
        ("package.json", r#"{"scripts": {"test": "node t.js"}}"#),
        (
            ".gitlab-ci.yml",
            "test:\n  parallel: 2\n  script:\n    - npm test\n",
        ),
    ]);
    let plan = plan(&r);
    assert!(plan.contains("→ \"hushrun run -- node t.js\""), "{plan}");
    assert!(!plan.contains("several instances"), "{plan}");
}

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
        .filter(|check| check["check"] == "variants")
        .map(|check| {
            (
                check["status"].as_str().unwrap_or_default().to_string(),
                check["detail"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// It is safe, so doctor says it: CI runs it every time, or \[ADR 0049\] each variant until it
/// passes, when the job says which variant each run is.
// covers: C-22
#[test]
fn doctor_says_ci_runs_a_wrapped_check_in_a_job_with_variants_every_time() {
    let wrapped = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &matrix(NPM_TEST)),
    ]);
    let says = doctor_says(&wrapped);
    assert_eq!(says.len(), 1, "{says:?}");
    assert_eq!(says[0].0, "ok", "{says:?}");
    assert_eq!(
        says[0].1,
        "test: CI runs it every time: job test of .github/workflows/ci.yml has several variants, and its HUSHRUN_VARIANT isn't ${{ toJSON(matrix) }}. To skip there too, run hushrun init"
    );

    let identified = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &with_identity(NPM_TEST)),
    ]);
    assert_eq!(
        doctor_says(&identified),
        [(
            "ok".to_string(),
            "test: CI runs each variant until that variant passes on this code".to_string()
        )]
    );

    let line = matrix("      - run: hushrun run test -- sh check.sh\n");
    let lines = Repo::new(&[(".github/workflows/ci.yml", &line)]);
    let says = doctor_says(&lines);
    assert_eq!(says[0].0, "ok", "{says:?}");
    assert!(says[0].1.starts_with("test: CI runs it"), "{says:?}");

    // [ADR 0049] GitLab tells each instance apart in its job's name.
    let gitlab = Repo::new(&[
        ("package.json", WRAPPED),
        (
            ".gitlab-ci.yml",
            "test:\n  parallel: 2\n  script:\n    - npm test\n",
        ),
    ]);
    assert_eq!(
        doctor_says(&gitlab)[0].1,
        "test: CI runs each variant until that variant passes on this code"
    );

    let two = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &two_systems(NPM_TEST)),
    ]);
    assert_eq!(
        doctor_says(&two),
        [(
            "ok".to_string(),
            "no job with variants in CI's files runs a wrapped check".to_string()
        )]
    );
}

/// \[ADR 0049\] A job that sets what tells its runs apart wrong never skips there: doctor warns,
/// with what it sets and what it should.
// covers: C-14, C-22
#[test]
fn doctor_warns_of_a_job_that_tells_its_runs_apart_wrong() {
    let wrong_variant = with_identity(NPM_TEST).replace(
        "HUSHRUN_VARIANT: ${{ toJSON(matrix) }}",
        "HUSHRUN_VARIANT: ${{ matrix.os }}",
    );
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &wrong_variant),
    ]);
    assert_eq!(
        doctor_says(&r),
        [(
            "warning".to_string(),
            "test: job test of .github/workflows/ci.yml sets HUSHRUN_VARIANT to ${{ matrix.os }}, not ${{ toJSON(matrix) }}, so no pass skips it there".to_string()
        )]
    );

    // A called file whose job names another file.
    let other = called_test(
        "      HUSHRUN_WORKFLOW: .github/workflows/other.yml\n      HUSHRUN_INPUTS: ${{ toJSON(inputs) }}\n",
    );
    let r = Repo::new(&[
        (".github/workflows/ci.yml", CALLER),
        (".github/workflows/test.yml", &other),
    ]);
    assert_eq!(
        doctor_says(&r),
        [(
            "warning".to_string(),
            "test: job test of .github/workflows/test.yml sets HUSHRUN_WORKFLOW to .github/workflows/other.yml, not its own file, so no pass skips it there".to_string()
        )]
    );

    // Its inputs, set to something else.
    let inputs = called_test(
        "      HUSHRUN_WORKFLOW: .github/workflows/test.yml\n      HUSHRUN_INPUTS: ${{ inputs.os }}\n",
    );
    let r = Repo::new(&[
        (".github/workflows/ci.yml", CALLER),
        (".github/workflows/test.yml", &inputs),
    ]);
    assert_eq!(
        doctor_says(&r),
        [(
            "warning".to_string(),
            "test: job test of .github/workflows/test.yml sets HUSHRUN_INPUTS to ${{ inputs.os }}, not ${{ toJSON(inputs) }}, so no pass skips it there".to_string()
        )]
    );

    // As init leaves it: each call runs until that call passes.
    let r = Repo::new(&[
        (".github/workflows/ci.yml", CALLER),
        (".github/workflows/test.yml", &called_test(SAYS_ITS_FILE)),
    ]);
    assert_eq!(
        doctor_says(&r),
        [(
            "ok".to_string(),
            "test: CI runs each call until that call passes on this code".to_string()
        )]
    );
}

/// \[ADR 0049\] A workflow that starts on its own too runs as itself on a push, which a pass here
/// is for: only a workflow a call alone starts must name its file.
// covers: C-22
#[test]
fn a_workflow_that_starts_on_its_own_too_is_judged_by_its_own_runs() {
    let both = "on: [push, workflow_call]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: npm test\n";
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/test.yml", both),
    ]);
    assert_eq!(
        doctor_says(&r),
        [(
            "ok".to_string(),
            "no job with variants in CI's files runs a wrapped check".to_string()
        )]
    );
    // A call alone starts it: its job has to say its file.
    let only = both.replace("on: [push, workflow_call]", "on: workflow_call");
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (
            ".github/workflows/ci.yml",
            "on: push\njobs:\n  call:\n    uses: ./.github/workflows/test.yml\n",
        ),
        (".github/workflows/test.yml", &only),
    ]);
    assert_eq!(
        doctor_says(&r),
        [(
            "ok".to_string(),
            "test: CI runs it every time: job test of .github/workflows/test.yml, a workflow another calls, doesn't set HUSHRUN_WORKFLOW to that file. To skip there too, run hushrun init".to_string()
        )]
    );
}

/// The second review's third finding: a job runs what the scripts it starts start.
// covers: C-22
#[test]
fn doctor_follows_a_job_through_the_scripts_it_starts() {
    for package in [
        r#"{"scripts": {"test": "npm run unit", "unit": "hushrun run -- sh check.sh"}}"#,
        // Past a run of hushrun's own, and through a chain.
        r#"{"scripts": {"test": "hushrun run t -- npm run ci", "ci": "tsc && npm run unit", "unit": "hushrun run -- sh check.sh"}}"#,
        // Two that start each other end.
        r#"{"scripts": {"test": "npm run again", "again": "npm test && npm run unit", "unit": "hushrun run -- sh check.sh"}}"#,
    ] {
        let r = Repo::new(&[
            ("package.json", package),
            (".github/workflows/ci.yml", &matrix(NPM_TEST)),
        ]);
        let says = doctor_says(&r);
        assert!(
            says.iter()
                .any(|(_, line)| line.starts_with("unit: CI runs it every time")),
            "{package}: {says:?}"
        );
    }
}

/// A run CI names per variant has a key of its own in each: what it starts is its business, so
/// what doctor says doesn't outlive following its advice. `ready` is in hook.rs.
// covers: C-22
#[test]
fn doctor_says_nothing_of_a_script_inside_a_named_variant() {
    let line = "      - run: hushrun run test-${{ strategy.job-index }} -- npm test\n";
    let r = Repo::new(&[
        ("package.json", WRAPPED),
        (".github/workflows/ci.yml", &matrix(line)),
    ]);
    assert_eq!(
        doctor_says(&r),
        [(
            "ok".to_string(),
            "no job with variants in CI's files runs a wrapped check".to_string()
        )]
    );
}

// ADR 0049: each variant is its own job.

/// The line a matrix job runs the check with.
const LINE: &str = "      - run: hushrun run test -- sh check.sh\n";
const NODE_20: &str = r#"{"os": "ubuntu-latest", "node": 20}"#;
const NODE_25: &str = r#"{"os": "ubuntu-latest", "node": 25}"#;

/// The bug report's workflow, whose job says which variant each run is, as init writes it.
fn with_identity(step: &str) -> String {
    matrix(step).replace(
        "    runs-on: ${{ matrix.os }}\n",
        "    runs-on: ${{ matrix.os }}\n    env:\n      HUSHRUN_VARIANT: ${{ toJSON(matrix) }}\n",
    )
}

/// `hushrun` in the job `test`'s variant `values`, as GitHub fills in its `HUSHRUN_VARIANT`.
fn in_variant(r: &Repo, values: &str) -> Command {
    let mut cmd = r.on_github("ci.yml", "test");
    cmd.env("HUSHRUN_VARIANT", values);
    cmd
}

/// V3: a variant's pass skips that variant only; the other runs, records, then skips.
// covers: C-10, C-14
#[test]
fn each_variant_skips_on_its_own_pass() {
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", &with_identity(LINE)),
    ]);
    passed(&run_named(in_variant(&r, NODE_20), "test", "0"));
    skipped(&run_named(in_variant(&r, NODE_20), "test", "1"));
    let other = run_named(in_variant(&r, NODE_25), "test", "0");
    passed(&other);
    assert!(
        said(&other).contains("hushrun: test running: this exact code passed in another variant of this job, and each variant counts only its own passes\n"),
        "{}",
        said(&other)
    );
    skipped(&run_named(in_variant(&r, NODE_25), "test", "1"));
    // Each result says its variant, by its values in key order.
    let labels: Vec<Value> = r
        .results()
        .iter()
        .map(|result| result["variant"]["label"].clone())
        .collect();
    assert_eq!(labels, ["20, ubuntu-latest", "25, ubuntu-latest"]);
    for result in r.results() {
        let key = result["variant"]["key"].as_str().unwrap_or_default();
        assert!(key.starts_with("sha256:") && key.len() == 71, "{result}");
        schema::assert_valid(&schema::load("run-result.v0.schema.json"), &result);
    }
}

/// V3 against a server: the lookup asks for the job and the variant.
// covers: C-10, C-14
#[test]
fn a_variant_asks_the_server_for_its_own_pass() {
    let fake = FakeResults::start();
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", &with_identity(LINE)),
    ]);
    let job = |values| shared(in_variant(&r, values), &fake);
    passed(&run_named(job(NODE_20), "test", "0"));
    passed(&run_named(job(NODE_25), "test", "0"));
    skipped(&run_named(job(NODE_20), "test", "1"));
    skipped(&run_named(job(NODE_25), "test", "1"));
    let variants: Vec<String> = fake
        .raw_requests()
        .iter()
        .filter(|request| request.method == "GET")
        .flat_map(|request| request.query.clone())
        .filter(|(name, _)| name == "variant")
        .map(|(_, value)| value)
        .collect();
    // Each of the four runs asked for its own variant, and there are two.
    assert_eq!(variants.len(), 4, "{variants:?}");
    assert!(
        variants.iter().all(|v| v.starts_with("sha256:")),
        "{variants:?}"
    );
    let distinct: std::collections::BTreeSet<&String> = variants.iter().collect();
    assert_eq!(distinct.len(), 2, "{variants:?}");
}

/// V4: a pass from outside CI, in a matrix job that says its variants, doesn't skip one (D3).
// covers: C-10, C-14
#[test]
fn a_pass_from_outside_ci_doesnt_skip_a_variant() {
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", &with_identity(LINE)),
    ]);
    passed(&run_named(r.hushrun(), "test", "0"));
    let ci = run_named(in_variant(&r, NODE_20), "test", "0");
    passed(&ci);
    assert!(
        said(&ci).contains("hushrun: test running: this exact code passed outside CI, and a job with several variants counts only its own variant's passes\n"),
        "{}",
        said(&ci)
    );
    // Its own pass then skips it.
    skipped(&run_named(in_variant(&r, NODE_20), "test", "1"));
}

/// V5: HUSHRUN_VARIANT is set, but the job's file doesn't set it to the expression.
// covers: C-14
#[test]
fn a_variant_the_file_doesnt_set_to_the_expression_runs() {
    let workflow = matrix(LINE).replace(
        "    runs-on: ${{ matrix.os }}\n",
        "    runs-on: ${{ matrix.os }}\n    env:\n      HUSHRUN_VARIANT: ${{ matrix.node }}\n",
    );
    let r = Repo::new(&[("check.sh", CHECK), (".github/workflows/ci.yml", &workflow)]);
    passed(&run_named(in_variant(&r, NODE_20), "test", "0"));
    let again = run_named(in_variant(&r, NODE_20), "test", "0");
    passed(&again);
    assert!(
        said(&again).contains("hushrun: test running: job test of .github/workflows/ci.yml has several variants, and its HUSHRUN_VARIANT isn't ${{ toJSON(matrix) }}, so no pass skips it\n"),
        "{}",
        said(&again)
    );
}

/// V6: HUSHRUN_VARIANT isn't JSON.
// covers: C-14
#[test]
fn a_variant_that_isnt_json_runs() {
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", &with_identity(LINE)),
    ]);
    passed(&run_named(in_variant(&r, "node 20"), "test", "0"));
    let again = run_named(in_variant(&r, "node 20"), "test", "0");
    passed(&again);
    assert!(
        said(&again).contains("hushrun: test running: HUSHRUN_VARIANT isn't JSON, so hushrun can't tell this variant, and no pass skips it\n"),
        "{}",
        said(&again)
    );
}

/// V7: a matrix only run time knows, built by `fromJSON(…)`: the value is the variant as it ran.
// covers: C-14
#[test]
fn a_matrix_built_by_an_expression_skips_per_variant() {
    let workflow = "on: push\njobs:\n  test:\n    strategy:\n      matrix: ${{ fromJSON(needs.plan.outputs.matrix) }}\n    runs-on: ubuntu-latest\n    env:\n      HUSHRUN_VARIANT: ${{ toJSON(matrix) }}\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run test -- sh check.sh\n";
    let r = Repo::new(&[("check.sh", CHECK), (".github/workflows/ci.yml", workflow)]);
    passed(&run_named(in_variant(&r, r#"{"shard": 1}"#), "test", "0"));
    skipped(&run_named(in_variant(&r, r#"{"shard": 1}"#), "test", "1"));
    passed(&run_named(in_variant(&r, r#"{"shard": 2}"#), "test", "0"));
}

/// V8: the same values in another key order and with other spaces are the same variant.
// covers: C-14
#[test]
fn a_variants_values_in_another_order_are_the_same_variant() {
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", &with_identity(LINE)),
    ]);
    passed(&run_named(in_variant(&r, NODE_20), "test", "0"));
    skipped(&run_named(
        in_variant(&r, "{\n  \"node\": 20,\n  \"os\": \"ubuntu-latest\"\n}"),
        "test",
        "1",
    ));
}

/// V9: a name per variant still works, and with the variable too, the variable decides.
// covers: C-14
#[test]
fn with_a_name_and_the_variable_the_variable_decides() {
    let named = "      - run: hushrun run test-${{ strategy.job-index }} -- sh check.sh\n";
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", &with_identity(named)),
    ]);
    passed(&run_named(in_variant(&r, NODE_20), "test-0", "0"));
    skipped(&run_named(in_variant(&r, NODE_20), "test-0", "1"));
    let results = r.results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["variant"]["label"], "20, ubuntu-latest");
    // Without the variable, the name alone lets each variant skip, as before.
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", &matrix(named)),
    ]);
    passed(&run_named(r.on_github("ci.yml", "test"), "test-0", "0"));
    skipped(&run_named(r.on_github("ci.yml", "test"), "test-0", "1"));
    assert!(r.results()[0].get("variant").is_none());
}

/// V10: a run inside another run, in a job of several variants, doesn't decide, as before.
// covers: C-14
#[test]
fn a_run_inside_another_in_a_variant_doesnt_decide() {
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", &with_identity(LINE)),
    ]);
    let inside = || {
        let mut cmd = in_variant(&r, NODE_20);
        cmd.env("HUSHRUN_INSIDE_RUN", "1");
        cmd
    };
    passed(&run_named(inside(), "test", "0"));
    let again = run_named(inside(), "test", "0");
    passed(&again);
    assert!(
        said(&again).contains("and this run is inside another, so no pass skips it\n"),
        "{}",
        said(&again)
    );
}

/// V14: GitLab names each instance of `parallel:matrix`, and of `parallel`, apart: each instance's
/// pass counts for itself only, and a pass from outside CI for none (D3).
// covers: C-10, C-14
#[test]
fn a_gitlab_instance_skips_on_its_own_pass() {
    let r = Repo::new(&[("check.sh", CHECK)]);
    passed(&run_named(
        r.on_gitlab("test: [linux, 20]", "2"),
        "test",
        "0",
    ));
    skipped(&run_named(
        r.on_gitlab("test: [linux, 20]", "2"),
        "test",
        "1",
    ));
    failed(&run_named(
        r.on_gitlab("test: [linux, 25]", "2"),
        "test",
        "1",
    ));
    passed(&run_named(r.on_gitlab("test 1/2", "2"), "test", "0"));
    skipped(&run_named(r.on_gitlab("test 1/2", "2"), "test", "1"));
    let r = Repo::new(&[("check.sh", CHECK)]);
    passed(&run_named(r.hushrun(), "test", "0"));
    let instance = run_named(r.on_gitlab("test 2/2", "2"), "test", "0");
    passed(&instance);
    assert!(
        said(&instance).contains("this exact code passed outside CI"),
        "{}",
        said(&instance)
    );
}

/// A workflow that calls `test.yml` twice, with two operating systems.
const CALLER: &str = "on: push\njobs:\n  linux:\n    uses: ./.github/workflows/test.yml\n    with:\n      os: linux\n  mac:\n    uses: ./.github/workflows/test.yml\n    with:\n      os: mac\n";

/// The workflow `test.yml`, as init leaves it: its job says its file and its inputs, and `env`
/// what else the job's env holds.
fn called_test(env: &str) -> String {
    format!(
        "on:\n  workflow_call:\n    inputs:\n      os:\n        type: string\n        required: true\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n{env}    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run test -- sh check.sh\n"
    )
}

const SAYS_ITS_FILE: &str = "      HUSHRUN_WORKFLOW: .github/workflows/test.yml\n      HUSHRUN_INPUTS: ${{ toJSON(inputs) }}\n";

/// `hushrun` in the job `test` of `test.yml`, called by `ci.yml` with `inputs`: GitHub names the
/// caller's file, and the job's env says its own.
fn in_call(r: &Repo, inputs: &str) -> Command {
    let mut cmd = r.on_github("ci.yml", "test");
    cmd.env("HUSHRUN_WORKFLOW", ".github/workflows/test.yml")
        .env("HUSHRUN_INPUTS", inputs);
    cmd
}

/// V11: a reusable workflow called twice with different inputs: each call skips only on its own
/// pass, and the inputs show nowhere.
// covers: C-10, C-14
#[test]
fn each_call_of_a_reusable_workflow_skips_on_its_own_pass() {
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", CALLER),
        (".github/workflows/test.yml", &called_test(SAYS_ITS_FILE)),
    ]);
    // Values nothing else in a line or a record holds, such as the system it runs on.
    let first = r#"{"os": "quokka"}"#;
    let second = r#"{"os": "wombat"}"#;
    let mut lines = String::new();
    let mut run = |inputs, verdict| {
        let out = run_named(in_call(&r, inputs), "test", verdict);
        lines.push_str(&said(&out));
        out
    };
    passed(&run(first, "0"));
    skipped(&run(first, "1"));
    passed(&run(second, "0"));
    skipped(&run(second, "1"));
    // Inputs are hashed and never shown: not in a line, not in a record.
    assert!(
        !lines.contains("quokka") && !lines.contains("wombat"),
        "{lines}"
    );
    let results = r.results();
    assert_eq!(results.len(), 2);
    for result in results {
        let text = result.to_string();
        assert!(
            !text.contains("quokka") && !text.contains("wombat"),
            "{text}"
        );
        assert_eq!(result["variant"]["label"], "", "{result}");
    }
}

/// V12: HUSHRUN_WORKFLOW names a file whose job doesn't name that file.
// covers: C-14
#[test]
fn a_called_job_that_doesnt_name_its_file_runs() {
    let wrong = "      HUSHRUN_WORKFLOW: .github/workflows/other.yml\n      HUSHRUN_INPUTS: ${{ toJSON(inputs) }}\n";
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", CALLER),
        (".github/workflows/test.yml", &called_test(wrong)),
    ]);
    passed(&run_named(in_call(&r, r#"{"os": "linux"}"#), "test", "0"));
    let again = run_named(in_call(&r, r#"{"os": "linux"}"#), "test", "0");
    passed(&again);
    assert!(
        said(&again).contains("hushrun: test running: HUSHRUN_WORKFLOW names .github/workflows/test.yml, whose job test doesn't name that file, so hushrun can't tell this job, and no pass skips it\n"),
        "{}",
        said(&again)
    );
    // A file that takes inputs needs HUSHRUN_INPUTS to tell its calls apart.
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", CALLER),
        (
            ".github/workflows/test.yml",
            &called_test("      HUSHRUN_WORKFLOW: .github/workflows/test.yml\n"),
        ),
    ]);
    passed(&run_named(in_call(&r, r#"{"os": "linux"}"#), "test", "0"));
    let again = run_named(in_call(&r, r#"{"os": "linux"}"#), "test", "0");
    passed(&again);
    assert!(
        said(&again).contains("job test of .github/workflows/test.yml takes inputs, and its HUSHRUN_INPUTS isn't ${{ toJSON(inputs) }}, so no pass skips it\n"),
        "{}",
        said(&again)
    );
    // [ADR 0049] A variable the running job has and the file's job doesn't set is another file's,
    // as a caller's checkout can hold one at the same path: its values would go untold.
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", CALLER),
        (".github/workflows/test.yml", &called_test(SAYS_ITS_FILE)),
    ]);
    let in_node = |node: &str| {
        let mut cmd = in_call(&r, r#"{"os": "linux"}"#);
        cmd.env("HUSHRUN_VARIANT", format!(r#"{{"node": {node}}}"#));
        cmd
    };
    passed(&run_named(in_node("20"), "test", "0"));
    let again = run_named(in_node("22"), "test", "0");
    passed(&again);
    assert!(
        said(&again).contains("hushrun: test running: HUSHRUN_VARIANT is set, and job test of .github/workflows/test.yml doesn't set it, so hushrun can't tell this job, and no pass skips it\n"),
        "{}",
        said(&again)
    );
    // A value the file's job sets tells its runs apart, even where the file shows one variant.
    let both = format!("{SAYS_ITS_FILE}      HUSHRUN_VARIANT: ${{{{ toJSON(matrix) }}}}\n");
    let r = Repo::new(&[
        ("check.sh", CHECK),
        (".github/workflows/ci.yml", CALLER),
        (".github/workflows/test.yml", &called_test(&both)),
    ]);
    let in_node = |node: &str| {
        let mut cmd = in_call(&r, r#"{"os": "linux"}"#);
        cmd.env("HUSHRUN_VARIANT", format!(r#"{{"node": {node}}}"#));
        cmd
    };
    passed(&run_named(in_node("20"), "test", "0"));
    let other = run_named(in_node("22"), "test", "0");
    passed(&other);
    assert!(
        said(&other).contains("this exact code passed in another variant of this job, and each variant counts only its own passes"),
        "{}",
        said(&other)
    );
    skipped(&run_named(in_node("20"), "test", "1"));
}

/// V13: a file called from another repository can't be read here, so the check runs, as before.
// covers: C-14
#[test]
fn a_file_called_from_another_repository_runs() {
    let caller = "on: push\njobs:\n  test:\n    uses: acme/shared/.github/workflows/test.yml@v1\n";
    let r = Repo::new(&[("check.sh", CHECK), (".github/workflows/ci.yml", caller)]);
    passed(&run_named(r.on_github("ci.yml", "test"), "test", "0"));
    let again = run_named(r.on_github("ci.yml", "test"), "test", "0");
    passed(&again);
    assert!(
        said(&again).contains("calls a reusable workflow"),
        "{}",
        said(&again)
    );
    // Even when its job names a file that isn't in this checkout.
    let again = run_named(in_call(&r, r#"{"os": "linux"}"#), "test", "0");
    passed(&again);
    assert!(
        said(&again).contains(
            "HUSHRUN_WORKFLOW names .github/workflows/test.yml, which can't be read here"
        ),
        "{}",
        said(&again)
    );
}
