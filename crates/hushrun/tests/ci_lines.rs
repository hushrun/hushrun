//! \[ADR 0050\] init reads what CI runs, wherever it is written (C-15). A line whose words CI or
//! the shell fills in wraps when each of them is safe to record, and its key is then over the words
//! as they ran (C-9), so each value has passes of its own. A line that would record a secret is
//! left, saying why (C-14).
#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
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

/// The tools CI's lines run here: each passes, whatever it is given.
const TOOLS: [&str; 2] = ["pytest", "go"];

/// A repository with a remote, a cache and credentials of its own, [`TOOLS`] on its `PATH`, and
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
        let bin = root.join("bin");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::os::unix::fs::symlink(assert_cmd::cargo::cargo_bin("hushrun"), bin.join("hushrun"))
            .expect("link");
        for tool in TOOLS {
            let path = bin.join(tool);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
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
        for (path, text) in files {
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

    /// `program` here, with no inherited configuration.
    fn command(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
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
        cmd.env("HUSHRUN_BACKEND", self.fake.url())
            .env("HUSHRUN_TOKEN", FakeResults::TOKEN);
        cmd
    }

    /// init's plan, which its schema takes.
    fn plan(&self) -> Value {
        let out = self
            .command(&assert_cmd::cargo::cargo_bin("hushrun"))
            .args(["init", "--dry-run", "--format", "json"])
            .output()
            .expect("hushrun");
        assert!(out.status.success(), "{}", said(&out));
        let plan: Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", said(&out)));
        schema::assert_valid(&schema::load("init-plan.v1.schema.json"), &plan);
        plan
    }

    /// The plan's edits of CI's files, made as a person makes them once the org is known.
    fn apply(&self, plan: &Value) {
        for edit in plan["edits"].as_array().expect("edits") {
            if edit["path"]
                .as_str()
                .is_some_and(|path| path.starts_with(".github/"))
            {
                let diff = edit["diff"].as_str().expect("a diff");
                git::run(&self.repo, &["apply", "-"], Some(diff.as_bytes()));
            }
        }
        git::commit_all(&self.repo, "init's edits");
    }

    /// `line` as a step of the job `job` of `.github/workflows/ci.yml` runs it, once GitHub has
    /// filled in its expressions, with `env` what GitHub gives the step besides.
    fn step(&self, job: &str, env: &[(&str, &str)], line: &str) -> Output {
        let mut cmd = self.command(Path::new("/bin/sh"));
        cmd.env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", "acme/shop")
            .env(
                "GITHUB_WORKFLOW_REF",
                "acme/shop/.github/workflows/ci.yml@refs/heads/main",
            )
            .env("GITHUB_JOB", job)
            .env("GITHUB_WORKSPACE", &self.repo)
            .envs(env.iter().copied())
            .args(["-c", line]);
        cmd.output().expect("sh")
    }

    /// The file at `path`, as it is now.
    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.repo.join(path)).expect("read")
    }

    /// The key of each result recorded here, by the command it ran, its words joined by spaces.
    fn keys(&self) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for (key, value) in self.fake.recorded_by_key() {
            if value["result"] == "skip" {
                continue;
            }
            let words: Vec<&str> = value["command"]
                .as_array()
                .expect("command")
                .iter()
                .map(|word| word.as_str().expect("a word"))
                .collect();
            found.insert(words.join(" "), key);
        }
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

/// The plan's word on the line `before` of a CI file.
#[track_caller]
fn line<'p>(plan: &'p Value, before: &str) -> &'p Value {
    plan["jobs"]
        .as_array()
        .expect("jobs")
        .iter()
        .flat_map(|job| job["commands"].as_array().expect("commands"))
        .find(|command| command["before"] == before)
        .unwrap_or_else(|| panic!("no line {before:?}: {plan:#}"))
}

/// The plan's word on the job `id`.
#[track_caller]
fn job_of<'p>(plan: &'p Value, id: &str) -> &'p Value {
    plan["jobs"]
        .as_array()
        .expect("jobs")
        .iter()
        .find(|job| job["job"] == id)
        .unwrap_or_else(|| panic!("no job {id}: {plan:#}"))
}

/// `step` in the job `test`, on Linux, with `more` in the job before its steps.
fn job(more: &str, step: &str) -> String {
    format!(
        "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n{more}    steps:\n      - uses: actions/checkout@v5\n{step}"
    )
}

/// C2: a matrix's values in a command wrap, and each value has passes of its own.
// covers: C-15, C-9
#[test]
fn a_matrix_value_in_a_command_wraps_and_each_value_has_its_own_key() {
    let matrix = "    strategy:\n      matrix:\n        args: [\"-m fast\", \"-m slow\"]\n";
    let r = Repo::new(&[(
        ".github/workflows/ci.yml",
        &job(matrix, "      - run: pytest ${{ matrix.args }}\n"),
    )]);
    let plan = r.plan();
    let wrapped = line(&plan, "pytest ${{ matrix.args }}");
    assert_eq!(wrapped["status"], "wrapped", "{wrapped:#}");
    assert_eq!(
        wrapped["after"],
        "hushrun run test -- pytest ${{ matrix.args }}"
    );
    assert_eq!(wrapped["ci"], "skips", "{wrapped:#}");
    r.apply(&plan);
    // GitHub fills in each variant's values, in the line and in the job's `HUSHRUN_VARIANT`.
    let run = |args: &str| {
        let variant = format!(r#"{{"args": "{args}"}}"#);
        r.step(
            "test",
            &[("HUSHRUN_VARIANT", &variant)],
            &format!("hushrun run test -- pytest {args}"),
        )
    };
    passed(&run("-m fast"));
    passed(&run("-m slow"));
    skipped(&run("-m fast"));
    skipped(&run("-m slow"));
    let keys = r.keys();
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert_ne!(keys["pytest -m fast"], keys["pytest -m slow"]);
}

/// C3: a line that would record a secret is left, saying why, whether it names the secret or a
/// variable set from one.
// covers: C-15, C-14
#[test]
fn a_command_that_passes_a_secret_is_left_saying_why() {
    let r = Repo::new(&[(
        ".github/workflows/ci.yml",
        &job(
            "    env:\n      TOKEN: ${{ secrets.NPM_TOKEN }}\n",
            "      - run: pytest --token ${{ secrets.NPM_TOKEN }}\n      - run: pytest --token $TOKEN\n      - run: pytest --level $LEVEL\n",
        ),
    )]);
    let plan = r.plan();
    for (before, why) in [
        (
            "pytest --token ${{ secrets.NPM_TOKEN }}",
            "it passes a secret on its command line (${{ secrets.NPM_TOKEN }}), which a result would record",
        ),
        (
            "pytest --token $TOKEN",
            "it passes a secret on its command line ($TOKEN), which a result would record",
        ),
        (
            "pytest --level $LEVEL",
            "an earlier step sets LEVEL, and hushrun can't tell it holds no secret",
        ),
    ] {
        let left = line(&plan, before);
        assert_eq!(left["status"], "left", "{left:#}");
        assert_eq!(left["why"], why, "{left:#}");
    }
    let edits = plan["edits"].as_array().expect("edits");
    assert!(
        edits
            .iter()
            .all(|edit| edit["path"] != ".github/workflows/ci.yml"),
        "{plan:#}"
    );
}

/// C4: variables set in front of a command go through `env`, and so into its key: one value's pass
/// doesn't skip another.
// covers: C-15, C-9
#[test]
fn variables_set_first_go_through_env_and_into_the_key() {
    let r = Repo::new(&[(
        ".github/workflows/ci.yml",
        &job("", "      - run: GOARCH=386 go test ./...\n"),
    )]);
    let plan = r.plan();
    let wrapped = line(&plan, "GOARCH=386 go test ./...");
    assert_eq!(wrapped["status"], "wrapped", "{wrapped:#}");
    let check = wrapped["check"].as_str().expect("check");
    assert_eq!(
        wrapped["after"],
        format!("hushrun run {check} -- env GOARCH=386 go test ./...")
    );
    assert_eq!(wrapped["ci"], "skips", "{wrapped:#}");
    r.apply(&plan);
    let run = |arch: &str| {
        r.step(
            "test",
            &[],
            &format!("hushrun run {check} -- env GOARCH={arch} go test ./..."),
        )
    };
    passed(&run("386"));
    skipped(&run("386"));
    passed(&run("amd64"));
    let keys = r.keys();
    assert_ne!(
        keys["env GOARCH=386 go test ./..."],
        keys["env GOARCH=amd64 go test ./..."]
    );
    // A quoted value goes through `env` as written, so the shell still passes it as one word.
    let quoted = Repo::new(&[(
        ".github/workflows/ci.yml",
        &job("", "      - run: RUSTFLAGS=\"-D warnings\" go test ./...\n"),
    )]);
    let plan = quoted.plan();
    let wrapped = line(&plan, "RUSTFLAGS=-D warnings go test ./...");
    assert_eq!(wrapped["status"], "wrapped", "{wrapped:#}");
    quoted.apply(&plan);
    assert!(
        quoted.read(".github/workflows/ci.yml").contains(
            "      - run: hushrun run test -- env RUSTFLAGS=\"-D warnings\" go test ./...\n"
        ),
        "{plan:#}"
    );
    let ran = quoted.step(
        "test",
        &[],
        "hushrun run test -- env RUSTFLAGS=\"-D warnings\" go test ./...",
    );
    passed(&ran);
    assert!(
        quoted
            .keys()
            .contains_key("env RUSTFLAGS=-D warnings go test ./..."),
        "{:?}",
        quoted.keys()
    );
    // A Windows runner's steps run pwsh, where no variable is set that way.
    let windows = Repo::new(&[(
        ".github/workflows/ci.yml",
        &job("", "      - run: GOARCH=386 go test ./...\n")
            .replace("ubuntu-latest", "windows-latest"),
    )]);
    let plan = windows.plan();
    let left = line(&plan, "GOARCH=386 go test ./...");
    assert_eq!(left["status"], "left", "{left:#}");
    assert_eq!(left["why"], "it sets GOARCH=386 first", "{left:#}");
}

/// A composite action of the repository that runs pytest with what the step that uses it gives.
const ACTION: &str = "name: tests\ninputs:\n  args:\n    description: what pytest takes\nruns:\n  using: composite\n  steps:\n    - run: pytest ${{ inputs.args }}\n      shell: bash\n";

/// Jobs `unit` and `matrix`, of two variants, each with `uses`, a step that uses [`ACTION`].
fn using_the_action(uses: &str) -> String {
    format!(
        "on: push\njobs:\n  unit:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n{uses}  matrix:\n    runs-on: ubuntu-latest\n    strategy:\n      matrix:\n        python: [\"3.12\", \"3.13\"]\n    steps:\n      - uses: actions/checkout@v5\n{uses}"
    )
}

const USES: &str =
    "      - uses: ./.github/actions/tests\n        with:\n          args: -m unit\n";

/// C1: a check inside a composite action of the repository. Its line runs hushrun, edited once;
/// each job that uses the action gets hushrun's setup above that step, its ID token and its
/// identity; and at run time each job, and each variant, skips on its own pass.
// covers: C-15, C-10
#[test]
fn a_check_inside_a_composite_action_skips_in_each_job_that_uses_it() {
    let r = Repo::new(&[
        (".github/workflows/ci.yml", &using_the_action(USES)),
        (".github/actions/tests/action.yml", ACTION),
    ]);
    let plan = r.plan();
    for id in ["unit", "matrix"] {
        let job = job_of(&plan, id);
        let wrapped = &job["commands"][0];
        assert_eq!(wrapped["status"], "wrapped", "{job:#}");
        assert_eq!(wrapped["action"], ".github/actions/tests/action.yml");
        assert_eq!(wrapped["line"], 8);
        assert_eq!(
            wrapped["after"],
            "hushrun run test -- pytest ${{ inputs.args }}"
        );
        assert_eq!(wrapped["ci"], "skips", "{job:#}");
        assert_eq!(job["setup"], "added", "{job:#}");
    }
    assert_eq!(job_of(&plan, "matrix")["variantIdentity"], "env");
    r.apply(&plan);
    assert_eq!(
        r.read(".github/actions/tests/action.yml"),
        ACTION.replace("    - run: pytest", "    - run: hushrun run test -- pytest")
    );
    let ci = r.read(".github/workflows/ci.yml");
    let setup = format!(
        "      - uses: hushrun/hushrun/action/setup@v{}\n{USES}",
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(ci.matches(&setup).count(), 2, "{ci}");
    assert_eq!(ci.matches("      id-token: write\n").count(), 2, "{ci}");
    assert!(
        ci.contains("    env:\n      HUSHRUN_VARIANT: ${{ toJSON(matrix) }}\n"),
        "{ci}"
    );
    // The action's step runs as the job that uses it: GitHub names that job, and its env reaches
    // the step. One job's pass doesn't skip the other, nor one variant's another.
    let line = "hushrun run test -- pytest -m unit";
    let variant = |python: &str| format!(r#"{{"python": "{python}"}}"#);
    passed(&r.step("unit", &[], line));
    skipped(&r.step("unit", &[], line));
    passed(&r.step("matrix", &[("HUSHRUN_VARIANT", &variant("3.12"))], line));
    skipped(&r.step("matrix", &[("HUSHRUN_VARIANT", &variant("3.12"))], line));
    passed(&r.step("matrix", &[("HUSHRUN_VARIANT", &variant("3.13"))], line));
    // init again proposes nothing more.
    let again = r.plan();
    let paths: Vec<&Value> = again["edits"]
        .as_array()
        .expect("edits")
        .iter()
        .map(|edit| &edit["path"])
        .filter(|path| {
            path.as_str()
                .is_some_and(|path| path.starts_with(".github/"))
        })
        .collect();
    assert!(paths.is_empty(), "{again:#}");
    assert_eq!(job_of(&again, "unit")["commands"][0]["status"], "already");
    assert_eq!(
        job_of(&again, "unit")["commands"][0]["action"],
        ".github/actions/tests/action.yml"
    );
}

/// C1, the other way: a composite action's line is one line, so a job that gives it a secret
/// leaves it for every job that uses the action, each told where.
// covers: C-15, C-14
#[test]
fn a_composite_actions_line_given_a_secret_by_one_job_is_left_for_all() {
    let secret = "      - uses: ./.github/actions/tests\n        with:\n          args: --token ${{ secrets.PYPI_TOKEN }}\n";
    let workflow = using_the_action(USES).replacen(USES, secret, 1);
    let r = Repo::new(&[
        (".github/workflows/ci.yml", &workflow),
        (".github/actions/tests/action.yml", ACTION),
    ]);
    let plan = r.plan();
    let secret =
        "it passes a secret on its command line (${{ inputs.args }}), which a result would record";
    let left = &job_of(&plan, "unit")["commands"][0];
    assert_eq!(left["status"], "left", "{plan:#}");
    assert_eq!(left["action"], ".github/actions/tests/action.yml");
    assert_eq!(left["why"], secret);
    let left = &job_of(&plan, "matrix")["commands"][0];
    assert_eq!(left["status"], "left", "{plan:#}");
    assert_eq!(
        left["why"],
        format!("where job unit of .github/workflows/ci.yml runs it, {secret}")
    );
    assert!(
        plan["edits"]
            .as_array()
            .expect("edits")
            .iter()
            .all(|edit| !edit["path"]
                .as_str()
                .is_some_and(|path| path.starts_with(".github/"))),
        "{plan:#}"
    );
}

/// \[ADR 0050\] Every job that runs an action's line gets hushrun, a nightly's too, which init
/// wraps nothing of: the line would run `hushrun` there all the same. init again proposes nothing.
// covers: C-15, C-14
#[test]
fn every_job_that_runs_an_actions_line_gets_hushrun() {
    let nightly = "on:\n  schedule:\n    - cron: '0 3 * * *'\njobs:\n  nightly:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - uses: ./.github/actions/tests\n        with:\n          args: -m nightly\n      - run: pytest -m slow\n";
    let r = Repo::new(&[
        (".github/workflows/ci.yml", &using_the_action(USES)),
        (".github/workflows/nightly.yml", nightly),
        (".github/actions/tests/action.yml", ACTION),
    ]);
    let plan = r.plan();
    let job = job_of(&plan, "nightly");
    assert_eq!(job["setup"], "added", "{job:#}");
    assert_eq!(job["commands"][0]["status"], "wrapped", "{job:#}");
    assert_eq!(job["commands"].as_array().map(Vec::len), Some(1), "{job:#}");
    r.apply(&plan);
    let edited = r.read(".github/workflows/nightly.yml");
    assert!(
        edited.contains(&format!(
            "      - uses: hushrun/hushrun/action/setup@v{}\n      - uses: ./.github/actions/tests\n",
            env!("CARGO_PKG_VERSION")
        )),
        "{edited}"
    );
    // Its own line isn't one a push runs: it stays as it was.
    assert!(edited.contains("      - run: pytest -m slow\n"), "{edited}");
    let again = r.plan();
    let paths: Vec<&Value> = again["edits"]
        .as_array()
        .expect("edits")
        .iter()
        .map(|edit| &edit["path"])
        .filter(|path| {
            path.as_str()
                .is_some_and(|path| path.starts_with(".github/"))
        })
        .collect();
    assert!(paths.is_empty(), "{again:#}");
}

/// \[ADR 0050\] An action's line stays as it is when a job init can't give hushrun may run it, or
/// when another repository may use the action.
// covers: C-15, C-14
#[test]
fn an_actions_line_stays_where_a_job_could_run_it_without_hushrun() {
    let flow = "on: push\njobs:\n  flow: {runs-on: ubuntu-latest, steps: [{uses: actions/checkout@v5}, {uses: ./.github/actions/tests}]}\n";
    let r = Repo::new(&[
        (".github/workflows/ci.yml", &using_the_action(USES)),
        (".github/workflows/flow.yml", flow),
        (".github/actions/tests/action.yml", ACTION),
    ]);
    let plan = r.plan();
    let left = &job_of(&plan, "unit")["commands"][0];
    assert_eq!(left["status"], "left", "{plan:#}");
    assert_eq!(
        left["why"],
        "job flow of .github/workflows/flow.yml may use its action, and init can't give that job hushrun"
    );
    // An action outside .github/ may be another repository's to use.
    let r = Repo::new(&[
        (
            ".github/workflows/ci.yml",
            &using_the_action(&USES.replace("./.github/actions/tests", "./actions/tests")),
        ),
        ("actions/tests/action.yml", ACTION),
    ]);
    let plan = r.plan();
    let left = &job_of(&plan, "unit")["commands"][0];
    assert_eq!(left["status"], "left", "{plan:#}");
    assert_eq!(left["action"], "actions/tests/action.yml");
    assert_eq!(
        left["why"],
        "its action isn't under .github/, so another repository may use it, where nothing installs hushrun"
    );
    assert!(
        plan["edits"]
            .as_array()
            .expect("edits")
            .iter()
            .all(|edit| edit["path"] != "actions/tests/action.yml"),
        "{plan:#}"
    );
}
