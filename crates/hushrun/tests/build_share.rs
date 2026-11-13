//! Sharing build blocks through a backend (B-25 to B-34), end to end: `hushrun run NAME --
//! cargo build` on two machines (two checkouts, two stores) against the fake build protocol, with
//! the real compiler.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hushrun_testkit::fake_build::{FakeBuild, route};
use hushrun_testkit::git;

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
    "HUSHRUN_LOG",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_BUILD_SHARE",
    "RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_TARGET_DIR",
    "CARGO_INCREMENTAL",
];

/// A workspace of two libraries, `app` on `dep`, committed in a repository.
fn workspace(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"2\"\n",
    );
    write(
        "dep/Cargo.toml",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        "dep/src/lib.rs",
        "pub fn two() -> u32 {\n    (0..2).map(|x| x * 1).sum::<u32>() + 1\n}\n",
    );
    write(
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n",
    );
    write(
        "app/src/lib.rs",
        "pub fn four() -> u32 {\n    dep::two() * 2\n}\n",
    );
    write(".gitignore", "target/\n");
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    repo
}

/// A machine: its own checkout, cache (and so store) and machine key.
struct Machine {
    root: PathBuf,
    repo: PathBuf,
}

impl Machine {
    fn new(dir: &Path, name: &str, from: Option<&Path>) -> Machine {
        Machine::with(dir, name, from, workspace)
    }

    fn with(dir: &Path, name: &str, from: Option<&Path>, make: fn(&Path) -> PathBuf) -> Machine {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let repo = match from {
            None => make(&root),
            Some(from) => {
                let repo = root.join("checkout");
                git::run(
                    &root,
                    &[
                        "clone",
                        "-q",
                        "--no-hardlinks",
                        from.to_str().unwrap(),
                        repo.to_str().unwrap(),
                    ],
                    None,
                );
                repo
            }
        };
        let key = root.join("machine.pem");
        std::fs::write(&key, hushrun_testkit::world::test_key(7).to_pem().as_str()).unwrap();
        Machine { root, repo }
    }

    fn hushrun(&self, fake: &FakeBuild) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("hushrun").unwrap();
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_MACHINE_KEY_FILE", self.root.join("machine.pem"))
            .env("HUSHRUN_HOST", "http://127.0.0.1:9")
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HUSHRUN_BACKEND", fake.url())
            .env("HUSHRUN_TOKEN", FakeBuild::TOKEN)
            .env("HUSHRUN_BUILD", "on")
            // A test that expects a reuse takes the audit's pick out of play.
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env("CARGO_TERM_COLOR", "never")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd
    }

    /// `cargo build`, in a command that goes on a moment after it: what a build longer than
    /// these two tiny crates gives the uploads, which start no new PUT once the command ends.
    fn build(&self, fake: &FakeBuild) -> (i32, String) {
        let output = self
            .hushrun(fake)
            .args([
                "run",
                "build",
                "--force",
                "--",
                "sh",
                "-c",
                "cargo build --offline && sleep 2",
            ])
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

// covers: B-26, B-29, B-30, B-32
#[test]
fn a_library_one_machine_built_is_reused_on_another_from_the_shared_store() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(fake.count(route::OPEN) >= 1, "{stderr}");
    // What A built went up, or waits in its outbox for its next run; `dep`, built first, went.
    assert!(!fake.stored().is_empty(), "{stderr}");
    let close = fake.closes().last().cloned().expect("a close");
    assert!(close.manifest.len() >= 2, "{close:?}");
    assert_eq!(close.counts.built, 2);

    // B: another checkout, another store, another target folder.
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (code, stderr) = b.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("from the shared store)"),
        "nothing came from the shared store: {stderr}"
    );
    let close = fake.closes().last().cloned().unwrap();
    assert!(close.counts.reused_shared >= 1, "{close:?}");
    // Its downloads are recorded as downloads, and none went back up.
    let begun_by_b = fake.begins().len();
    let (code, stderr) = b.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        fake.begins().len(),
        begun_by_b,
        "B uploaded what it downloaded"
    );
}

// covers: B-26
#[test]
fn a_server_that_sleeps_ten_seconds_on_open_doesnt_delay_cargo() {
    let fake = FakeBuild::start();
    fake.delay(route::OPEN, Duration::from_secs(10));
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let started = Instant::now();
    let (code, stderr) = a.build(&fake);
    let took = started.elapsed();
    assert_eq!(code, 0, "{stderr}");
    // The command itself takes two seconds more than cargo.
    assert!(took < Duration::from_secs(9), "{took:?}: {stderr}");
    assert!(stderr.contains("build: 2 crates built"), "{stderr}");
}

// covers: B-25
#[test]
fn an_org_that_doesnt_share_pays_no_request_and_sharing_off_here_asks_nothing() {
    let fake = FakeBuild::start();
    fake.set_sharing(false);
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(fake.count(route::OPEN), 0);
    fake.set_sharing(true);
    let output = a
        .hushrun(&fake)
        .env("HUSHRUN_BUILD_SHARE", "off")
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(fake.count(route::OPEN), 0);
}

/// `hushrun run build -- COMMAND` with `RUSTC_WRAPPER=sccache` set: COMMAND prints the wrapper
/// it was given. Its stdout and stderr.
fn with_another_wrapper(m: &Machine, fake: &FakeBuild) -> (String, String) {
    let output = m
        .hushrun(fake)
        .env("RUSTC_WRAPPER", "sccache")
        .args([
            "run",
            "build",
            "--force",
            "--",
            "sh",
            "-c",
            "echo \"wrapper=${RUSTC_WRAPPER:-none}\"",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// covers: B-2
#[test]
fn another_compiler_wrapper_passes_through_and_is_said_once_where_the_org_shares() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let line = "build reuse is off for this run: another compiler wrapper is set (RUSTC_WRAPPER)";
    let (stdout, stderr) = with_another_wrapper(&a, &fake);
    assert!(stdout.contains("wrapper=sccache"), "{stdout}");
    assert_eq!(stderr.matches(line).count(), 1, "{stderr}");
    assert_eq!(fake.count(route::OPEN), 0);
    // An org that doesn't share: as designed, nothing is said.
    fake.set_sharing(false);
    let (stdout, stderr) = with_another_wrapper(&a, &fake);
    assert!(stdout.contains("wrapper=sccache"), "{stdout}");
    assert!(!stderr.contains("wrapper"), "{stderr}");
    // Nor where reuse is off here anyway.
    fake.set_sharing(true);
    let output = a
        .hushrun(&fake)
        .env("RUSTC_WRAPPER", "sccache")
        .env("HUSHRUN_BUILD", "off")
        .args(["run", "build", "--force", "--", "true"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("wrapper"), "{stderr}");
}

/// The results lookups the fake saw: each one's `Hushrun-Ci` header and `repository` parameter.
fn results_lookups(fake: &FakeBuild) -> Vec<(Option<String>, Option<String>)> {
    fake.seen()
        .into_iter()
        .filter(|s| s.route == route::RESULTS && s.method == "GET")
        .map(|s| {
            let repository = s
                .query
                .iter()
                .find(|(name, _)| name == "repository")
                .map(|(_, value)| value.clone());
            (s.ci, repository)
        })
        .collect()
}

// covers: B-31
#[test]
fn a_token_in_ci_says_so_on_the_results_lookup_and_every_build_request_and_names_the_repository() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: example.com/acme/shop\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "named");
    // CI that isn't GitHub Actions, with an agent token: the job's only credential.
    let output = a
        .hushrun(&fake)
        .env("CI", "true")
        .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN)
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    let lookups = results_lookups(&fake);
    assert!(!lookups.is_empty(), "{stderr}");
    for (ci, repository) in &lookups {
        assert_eq!(ci.as_deref(), Some("1"), "{lookups:?}");
        assert_eq!(
            repository.as_deref(),
            Some("example.com/acme/shop"),
            "{lookups:?}"
        );
    }
    let build: Vec<_> = fake
        .seen()
        .into_iter()
        .filter(|s| s.path.contains("/v0/build/"))
        .collect();
    assert!(build.iter().any(|s| s.route == route::OPEN), "{stderr}");
    assert!(
        build.iter().all(|s| s.ci.as_deref() == Some("1")),
        "{:?}",
        build.iter().map(|s| (&s.route, &s.ci)).collect::<Vec<_>>()
    );

    // Outside CI the same token says nothing of the kind, anywhere.
    let fake = FakeBuild::start();
    let output = a
        .hushrun(&fake)
        .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN)
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    assert!(!results_lookups(&fake).is_empty(), "{stderr}");
    assert!(fake.seen().iter().all(|s| s.ci.is_none()), "{stderr}");
}

/// What a GitHub Actions job's environment adds: its own temporary folder, the variables that
/// let it mint ID tokens from the fake, and an agent token beside them.
fn in_github_actions(cmd: &mut assert_cmd::Command, fake: &FakeBuild, runner_temp: &Path) {
    std::fs::create_dir_all(runner_temp).unwrap();
    cmd.env("CI", "true")
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REPOSITORY", "acme/shop")
        .env("RUNNER_TEMP", runner_temp)
        .env("ACTIONS_ID_TOKEN_REQUEST_URL", fake.id_token_url())
        .env(
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            FakeBuild::ID_TOKEN_REQUEST,
        )
        .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN);
}

/// `hushrun run build -- …` in a GitHub Actions job: COMMAND prints the wrapper it was given,
/// then builds. Its exit code, stdout and stderr.
fn ci_build(
    m: &Machine,
    fake: &FakeBuild,
    change: impl FnOnce(&mut assert_cmd::Command),
) -> (i32, String, String) {
    let mut cmd = m.hushrun(fake);
    in_github_actions(&mut cmd, fake, &m.root.join("runner-temp"));
    change(&mut cmd);
    let output = cmd
        .args([
            "run",
            "build",
            "--force",
            "--",
            "sh",
            "-c",
            "echo \"wrapper=${RUSTC_WRAPPER:-none}\"; cargo build --offline && sleep 2",
        ])
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// covers: B-2, B-31
#[test]
fn a_github_actions_job_shares_with_its_id_token_from_a_store_that_goes_with_the_job() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stdout, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(
        !stdout.contains("wrapper=none"),
        "{stdout}\n{stderr}\n{:?}",
        fake.seen()
            .iter()
            .map(|s| format!("{} {} {:?}", s.method, s.path, s.authorization))
            .collect::<Vec<_>>()
    );
    // Every build request carried an ID token the fake minted, never the agent token beside it.
    let minted = fake.id_tokens();
    assert!(!minted.is_empty(), "{stderr}");
    let build: Vec<_> = fake
        .seen()
        .into_iter()
        .filter(|s| s.path.contains("/v0/build/"))
        .collect();
    assert!(!build.is_empty(), "{stderr}");
    for seen in &build {
        let bearer = seen.authorization.as_deref().unwrap_or_default();
        assert!(
            minted.iter().any(|t| bearer == format!("Bearer {t}")),
            "{} carried {bearer}",
            seen.path
        );
    }
    assert_eq!(fake.audiences()[0], fake.url());
    // What it built went up from the job's own store, under its temporary folder, and nothing
    // went into the cache, which outlives the job.
    assert!(!fake.stored().is_empty(), "{stderr}");
    let stores = ci_stores(&a.root.join("runner-temp"));
    assert_eq!(stores.len(), 1, "{stderr}");
    assert!(stores[0].is_dir(), "{stderr}");
    assert!(!a.root.join("cache/build").exists());
    // The runner empties the folder when the job ends: the store goes with it.
    std::fs::remove_dir_all(a.root.join("runner-temp")).unwrap();
    assert!(!walk_any(&a.root, "hushrun-build"));
}

/// `hushrun run build -- sh -c SCRIPT` as the GitHub Actions job `job` of run `run`, on the
/// runner's temporary folder `runner_temp`. Its stderr, and the close it sent last.
fn job_run(
    m: &Machine,
    fake: &FakeBuild,
    runner_temp: &Path,
    run: &str,
    script: &str,
) -> (String, Option<hushrun_core::blocks::CloseRequest>) {
    let closes = fake.closes().len();
    let mut cmd = m.hushrun(fake);
    in_github_actions(&mut cmd, fake, runner_temp);
    let output = cmd
        .env("GITHUB_REPOSITORY_ID", "4242")
        .env("GITHUB_RUN_ID", run)
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_JOB", "build")
        .args(["run", "build", "--force", "--", "sh", "-c", script])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    (stderr, fake.closes().get(closes).cloned())
}

// covers: B-31
#[test]
fn a_job_never_uploads_or_reuses_what_another_job_left_in_a_kept_temporary_folder() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    // One runner, whose temporary folder survives between its jobs.
    let runner_temp = a.root.join("runner-temp");
    // A pull request's job, which writes its own scope, on a store too slow to finish: its
    // blocks wait in its outbox when it ends.
    fake.set_scopes(Some("ci/1/pr7"), &["ci/1/pr7", "protected/1"]);
    fake.throttle_store(1);
    let (stderr, close) = job_run(
        &a,
        &fake,
        &runner_temp,
        "100",
        "cargo build --offline && sleep 2",
    );
    let close = close.expect(&stderr);
    assert_eq!(close.counts.built, 2, "{stderr}");
    assert!(fake.stored().is_empty(), "{stderr}");

    // The main branch's job, trusted, on the same folder, with a fast store.
    fake.throttle_store(0);
    fake.set_scopes(Some("protected/1"), &["protected/1"]);
    let (stderr, close) = job_run(
        &a,
        &fake,
        &runner_temp,
        "101",
        "cargo clean -q && cargo build --offline && sleep 3",
    );
    let close = close.expect(&stderr);
    // Nothing the pull request's job left was reused: the main job compiled both crates
    // itself, so what it uploads into the trusted scope is its own.
    assert_eq!(close.counts.reused_local, 0, "{close:?}\n{stderr}");
    assert_eq!(close.counts.built, 2, "{close:?}\n{stderr}");
    // The other job's store is gone, and the main job's next call finds its own.
    assert_eq!(ci_stores(&runner_temp).len(), 1);
    let (stderr, close) = job_run(
        &a,
        &fake,
        &runner_temp,
        "101",
        "cargo clean -q && cargo build --offline",
    );
    let close = close.expect(&stderr);
    assert_eq!(close.counts.reused_local, 2, "{close:?}\n{stderr}");
}

// covers: B-31
#[test]
fn a_job_with_nothing_to_build_sends_no_begin_for_another_jobs_markers() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let runner_temp = a.root.join("runner-temp");
    fake.set_scopes(Some("ci/1/pr7"), &["ci/1/pr7", "protected/1"]);
    fake.throttle_store(1);
    let (stderr, _) = job_run(
        &a,
        &fake,
        &runner_temp,
        "200",
        "cargo build --offline && sleep 2",
    );
    assert!(fake.stored().is_empty(), "{stderr}");
    let begun = fake.begins().len();
    fake.throttle_store(0);
    fake.set_scopes(Some("protected/1"), &["protected/1"]);
    let (stderr, _) = job_run(&a, &fake, &runner_temp, "201", "sleep 4");
    assert_eq!(fake.begins().len(), begun, "{stderr}");
    assert!(fake.stored().is_empty(), "{stderr}");
}

/// The CI stores in a job's temporary folder.
fn ci_stores(runner_temp: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(runner_temp)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("hushrun-build"))
        .map(|e| e.path())
        .collect()
}

/// Whether anything under `dir` has a name that starts with `name`.
fn walk_any(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let path = entry.path();
            path.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(name))
                || (path.is_dir() && !path.is_symlink() && walk_any(&path, name))
        })
}

// covers: B-31
#[test]
fn a_github_job_without_an_id_token_shares_nothing_whatever_agent_token_it_holds() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    // A job without `id-token: write`, with an agent token in HUSHRUN_TOKEN beside it.
    let (code, stdout, stderr) = ci_build(&a, &fake, |cmd| {
        cmd.env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
            .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN");
    });
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    assert!(stderr.contains("id-token: write"), "{stderr}");
    let build: Vec<_> = fake
        .seen()
        .into_iter()
        .filter(|s| s.path.contains("/v0/build/"))
        .map(|s| s.path)
        .collect();
    assert!(build.is_empty(), "{build:?}");

    // With an ID token, the results request carries it, and not the agent token beside it:
    // the answer that turns the wrapper on is one to the job itself.
    let before = fake.seen().len();
    let (code, stdout, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    let minted = fake.id_tokens();
    let results: Vec<_> = fake
        .seen()
        .into_iter()
        .skip(before)
        .filter(|s| s.route == route::RESULTS)
        .collect();
    assert!(!results.is_empty(), "{stderr}");
    for seen in &results {
        let bearer = seen.authorization.as_deref().unwrap_or_default();
        assert!(
            minted.iter().any(|t| bearer == format!("Bearer {t}")),
            "{} carried {bearer}",
            seen.path
        );
    }
}

// covers: B-2
#[test]
fn a_job_whose_org_doesnt_share_runs_unwrapped_and_says_nothing() {
    let fake = FakeBuild::start();
    fake.set_sharing(false);
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stdout, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}");
    assert!(!stderr.contains("hushrun: build reuse"), "{stderr}");
    assert!(!stderr.contains("hushrun: build:"), "{stderr}");
    assert_eq!(fake.count(route::OPEN), 0);
    assert!(ci_stores(&a.root.join("runner-temp")).is_empty());
}

/// `hushrun build status` in a GitHub Actions job, as `change` alters it: its `ci` line.
fn ci_status(
    m: &Machine,
    fake: &FakeBuild,
    change: impl FnOnce(&mut assert_cmd::Command),
) -> String {
    let mut cmd = m.hushrun(fake);
    in_github_actions(&mut cmd, fake, &m.root.join("runner-temp"));
    cmd.env_remove("HUSHRUN_BUILD");
    change(&mut cmd);
    let output = cmd.args(["build", "status"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{stdout}");
    stdout
        .lines()
        .find(|line| line.starts_with("ci "))
        .unwrap_or_else(|| panic!("no ci line: {stdout}"))
        .to_string()
}

// covers: B-25, B-31
#[test]
fn build_status_in_ci_says_what_a_run_does_there_and_why() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    // A job with an ID token.
    assert_eq!(
        ci_status(&a, &fake, |_| {}),
        "ci        this is CI: `hushrun run NAME -- COMMAND` reuses in the job's own store when the org shares, and shares as the org's mode says; no person's settings are read"
    );
    // A job without one.
    assert_eq!(
        ci_status(&a, &fake, |cmd| {
            cmd.env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
                .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN");
        }),
        "ci        this is CI, where `hushrun run` reuses nothing here: the job can't mint an ID token (give the job `permissions: id-token: write`)"
    );
    // Another CI, with an agent token: it reads, and uploads nothing.
    assert_eq!(
        ci_status(&a, &fake, |cmd| {
            cmd.env_remove("GITHUB_ACTIONS")
                .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
                .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
                .env("GITLAB_CI", "true");
        }),
        "ci        this is CI: `hushrun run NAME -- COMMAND` reuses in the job's own store when the org shares, reading the org's blocks and uploading none (an agent token outside GitHub Actions); no person's settings are read"
    );
    // No backend.
    assert_eq!(
        ci_status(&a, &fake, |cmd| {
            cmd.env_remove("HUSHRUN_BACKEND")
                .env_remove("HUSHRUN_TOKEN");
        }),
        "ci        this is CI, where `hushrun run` reuses nothing here: no backend is signed in to"
    );
}

// covers: B-2, B-25
#[test]
fn a_job_whose_orgs_plan_sets_storage_by_contract_runs_unwrapped_and_says_so_once() {
    let fake = FakeBuild::start();
    fake.set_policy(|p| {
        p.sharing = false;
        p.reason = Some(hushrun_core::blocks::OffReason::Contract);
    });
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stdout, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}");
    let line = "build: sharing is off for this run: the org's plan sets its shared storage by contract, and none is set yet: get in touch with hushrun to set it";
    assert_eq!(stderr.matches(line).count(), 1, "{stderr}");
}

// covers: B-2
#[test]
fn a_job_whose_backend_doesnt_answer_runs_unwrapped_with_one_line() {
    // The results lookup gets no answer at all.
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stdout, stderr) = ci_build(&a, &fake, |cmd| {
        cmd.env("HUSHRUN_BACKEND", "http://127.0.0.1:9/o/acme");
    });
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}");
    let line = "build reuse is off for this run: the backend didn't answer in time";
    assert_eq!(stderr.matches(line).count(), 1, "{stderr}");

    // The results lookup answers, and `open` takes longer than the 5 seconds a job gives it.
    fake.delay(route::OPEN, Duration::from_secs(8));
    let started = Instant::now();
    let (code, stdout, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}");
    assert_eq!(stderr.matches(line).count(), 1, "{stderr}");
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(fake.stored().is_empty());
}

/// Every `origin.json` under `dir`, its vouches dated at the epoch: two days old and more.
fn age_vouches(dir: &Path) -> usize {
    let mut aged = 0;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            aged += age_vouches(&path);
        } else if path.file_name().is_some_and(|n| n == "origin.json") {
            let mut origin: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if let Some(vouched) = origin["vouched"].as_array_mut() {
                for vouch in vouched {
                    vouch["date_ms"] = serde_json::json!(0);
                    aged += 1;
                }
            }
            std::fs::write(&path, serde_json::to_vec(&origin).unwrap()).unwrap();
        }
    }
    aged
}

/// `cargo clean`, then a timed `cargo build` through hushrun, with `share` as the share setting.
fn timed_build(m: &Machine, fake: &FakeBuild, share: &str) -> (Duration, String) {
    // The workspace's own target folder: never one CARGO_TARGET_DIR names, which may be the
    // target folder of the tests themselves.
    let clean = std::process::Command::new("cargo")
        .args(["clean", "-q"])
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(&m.repo)
        .status()
        .unwrap();
    assert!(clean.success());
    let started = Instant::now();
    let output = m
        .hushrun(fake)
        .env("HUSHRUN_BUILD_SHARE", share)
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    (took, stderr)
}

/// The lookups the fake was sent after the first `after` requests: whether each fetches, and
/// how many addresses it names.
fn lookups_since(fake: &FakeBuild, after: usize) -> Vec<(bool, usize)> {
    fake.seen()
        .into_iter()
        .skip(after)
        .filter(|s| s.route == route::LOOKUP)
        .map(|s| {
            let asked: hushrun_core::blocks::LookupRequest =
                serde_json::from_slice(&s.body).unwrap();
            (asked.fetch, asked.addresses.len())
        })
        .collect()
}

// covers: B-26, B-34
#[test]
fn a_warm_build_with_stale_vouches_waits_for_open_at_most_half_each_compile_and_asks_once() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    // Compiles that took 400 ms where they were built: a question about a vouch of one may wait
    // 200.
    fake.set_costs(400);
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (code, stderr) = b.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("(2 from the shared store)"), "{stderr}");
    // A warm build with vouches fresh, and the same a day later, when every vouch B holds is
    // older than a day and `open` takes 10 seconds: each crate waits for it at most its 200 ms,
    // then the server can't be reached, for it, and the entry counts.
    let (fresh, stderr) = timed_build(&b, &fake, "on");
    assert!(stderr.contains("2 of 2 crates reused"), "{stderr}");
    assert!(age_vouches(&b.root.join("cache/build")) > 0);
    fake.delay(route::OPEN, Duration::from_secs(10));
    let seen = fake.seen().len();
    let (stale, stderr) = timed_build(&b, &fake, "on");
    assert!(stderr.contains("2 of 2 crates reused"), "{stderr}");
    assert!(
        stale < fresh.mul_f64(1.1) + Duration::from_millis(900),
        "with stale vouches {stale:?}, fresh {fresh:?}: {stderr}"
    );
    assert_eq!(lookups_since(&fake, seen), vec![], "{stderr}");

    // In a CI job `open` has answered before cargo starts: the job's blocks downloaded in its
    // first call, a day old in its second, are asked about in one lookup, and the build takes
    // what one with fresh vouches takes.
    fake.delay(route::OPEN, Duration::ZERO);
    let c = Machine::new(dir.path(), "c", Some(&a.repo));
    let (_, _, said) = timed_ci_build(&c, &fake);
    assert!(said.contains("(2 from the shared store)"), "{said}");
    let stores = ci_stores(&c.root.join("runner-temp"));
    let (fresh, _, said) = timed_ci_build(&c, &fake);
    assert!(said.contains("2 of 2 crates reused"), "{said}");
    assert!(age_vouches(&stores[0]) > 0);
    let seen = fake.seen().len();
    let (stale, _, said) = timed_ci_build(&c, &fake);
    assert!(said.contains("2 of 2 crates reused"), "{said}");
    assert_eq!(lookups_since(&fake, seen), vec![(false, 2)], "{said}");
    assert!(
        stale < fresh.mul_f64(1.1) + Duration::from_millis(500),
        "with stale vouches {stale:?}, fresh {fresh:?}: {said}"
    );
}

/// `cargo build` through hushrun, the target as the last build left it.
fn warm_build(m: &Machine, fake: &FakeBuild) -> String {
    let output = m
        .hushrun(fake)
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    stderr
}

// covers: B-34
#[test]
fn a_withdrawn_download_is_not_reused_once_its_entry_is_a_day_old_and_the_no_is_kept() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    // Two machines download A's blocks; two days later, the server withdraws them.
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let c = Machine::new(dir.path(), "c", Some(&a.repo));
    for m in [&b, &c] {
        let (code, stderr) = m.build(&fake);
        assert_eq!(code, 0, "{stderr}");
        assert!(stderr.contains("(2 from the shared store)"), "{stderr}");
        assert!(age_vouches(&m.root.join("cache/build")) > 0);
    }
    for block in fake.stored() {
        fake.withdraw(block.address);
    }
    // The server up: neither is reused, however short the crates' compiles.
    let (_, stderr) = timed_build(&c, &fake, "on");
    assert!(stderr.contains("2 crates built, none reused"), "{stderr}");
    // A build whose target is warm compiles nothing; its lookup of stale vouches learns "no",
    // and the blocks' records keep it.
    let seen = fake.seen().len();
    let stderr = warm_build(&b, &fake);
    assert_eq!(lookups_since(&fake, seen), vec![(false, 2)], "{stderr}");
    // Then the server is down, and the entries would count at any age: not reused.
    fake.set_unreachable(true);
    let (_, stderr) = timed_build(&b, &fake, "on");
    assert!(stderr.contains("2 crates built, none reused"), "{stderr}");
}

/// Two machines whose libraries are at their second version. A built and uploaded it. B compiled
/// the first version itself, so its own hints lead to each library's key, then downloaded the
/// second from the shared store: blocks it only downloaded, which a run with no relay finds.
fn built_and_downloaded(dir: &Path, fake: &FakeBuild) -> (Machine, Machine) {
    let a = Machine::new(dir, "a", None);
    let b = Machine::new(dir, "b", Some(&a.repo));
    let (_, said) = timed_build(&b, fake, "off");
    assert!(said.contains("2 crates built"), "{said}");
    for m in [&a, &b] {
        std::fs::write(
            m.repo.join("dep/src/lib.rs"),
            "pub fn two() -> u32 {\n    (0..3).map(|x| x * 1).sum::<u32>() - 1\n}\n",
        )
        .unwrap();
        git::commit_all(&m.repo, "second");
    }
    let (code, stderr) = a.build(fake);
    assert_eq!(code, 0, "{stderr}");
    let (code, stderr) = b.build(fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("(2 from the shared store)"), "{stderr}");
    (a, b)
}

/// `cargo clean`, then `cargo build` through hushrun, the command as `change` leaves it: what
/// the run said.
fn rebuilt(m: &Machine, fake: &FakeBuild, change: fn(&mut assert_cmd::Command)) -> String {
    let clean = std::process::Command::new("cargo")
        .args(["clean", "-q"])
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(&m.repo)
        .status()
        .unwrap();
    assert!(clean.success());
    let mut cmd = m.hushrun(fake);
    change(&mut cmd);
    let output = cmd
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    stderr
}

/// B-34: a run that isn't sharing by a decision compiles what B only downloaded, once, and
/// reuses it from then on; what A built itself is reused all along.
fn a_decision_reuses_no_download(
    a: &Machine,
    b: &Machine,
    fake: &FakeBuild,
    change: fn(&mut assert_cmd::Command),
) {
    let said = rebuilt(b, fake, change);
    assert!(said.contains("2 crates built, none reused"), "{said}");
    let said = rebuilt(b, fake, change);
    assert!(said.contains("2 of 2 crates reused"), "{said}");
    let said = rebuilt(a, fake, change);
    assert!(said.contains("2 of 2 crates reused"), "{said}");
}

// covers: B-34
#[test]
fn once_the_org_stops_sharing_a_downloaded_block_is_compiled_once_and_one_built_here_is_reused() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = built_and_downloaded(dir.path(), &fake);
    fake.set_sharing(false);
    a_decision_reuses_no_download(&a, &b, &fake, |_| {});
}

// covers: B-34
#[test]
fn with_share_off_here_a_downloaded_block_is_compiled_once_and_one_built_here_is_reused() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = built_and_downloaded(dir.path(), &fake);
    a_decision_reuses_no_download(&a, &b, &fake, |cmd| {
        cmd.env("HUSHRUN_BUILD_SHARE", "off");
    });
}

// covers: B-34
#[test]
fn signed_out_a_downloaded_block_is_compiled_once_and_one_built_here_is_reused() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = built_and_downloaded(dir.path(), &fake);
    a_decision_reuses_no_download(&a, &b, &fake, |cmd| {
        cmd.env_remove("HUSHRUN_TOKEN");
    });
}

// covers: B-34
#[test]
fn with_the_network_cut_and_sharing_on_a_downloaded_block_of_any_age_is_reused() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = built_and_downloaded(dir.path(), &fake);
    assert!(age_vouches(&b.root.join("cache/build")) > 0);
    fake.set_unreachable(true);
    for m in [&b, &a] {
        let said = rebuilt(m, &fake, |_| {});
        assert!(said.contains("2 of 2 crates reused"), "{said}");
    }
    // Still only downloaded: the network back, the server is asked about them again.
    fake.set_unreachable(false);
    let seen = fake.seen().len();
    let said = rebuilt(&b, &fake, |_| {});
    assert!(said.contains("2 of 2 crates reused"), "{said}");
    assert_eq!(lookups_since(&fake, seen), vec![(false, 2)], "{said}");
}

/// The workspace, with a `hushrun.yaml` of two scopes, one for each library.
fn scoped_workspace(root: &Path) -> PathBuf {
    let repo = workspace(root);
    std::fs::write(
        repo.join("hushrun.yaml"),
        "version: 1\nrepository: example.com/acme/shop\nscopes:\n  app:\n    include: [\"app/**\"]\n  dep:\n    include: [\"dep/**\"]\n",
    )
    .unwrap();
    git::commit_all(&repo, "two scopes");
    repo
}

// covers: B-25
#[test]
fn a_repository_of_several_scopes_shares_without_a_scope_and_an_org_that_doesnt_share_says_nothing()
{
    // `hushrun run build -- …` names no scope: it can't tell which code this is, so it runs
    // without skipping, and its build blocks, which belong to the repository, are shared.
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::with(dir.path(), "a", None, scoped_workspace);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("can't tell which code this is"), "{stderr}");
    assert_eq!(fake.stored().len(), 2, "{stderr}");
    assert_eq!(fake.count(route::RESULTS), 0, "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (code, stderr) = b.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("(2 from the shared store)"), "{stderr}");
    // An org that doesn't share: its `open` says so, and the run says nothing of sharing.
    let fake = FakeBuild::start();
    fake.set_sharing(false);
    let c = Machine::new(dir.path(), "c", Some(&a.repo));
    let (code, stderr) = c.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("sharing"), "{stderr}");
    assert_eq!(fake.count(route::OPEN), 1, "{stderr}");
}

/// A workspace whose proc macro reads `GREETING` while it expands, without telling rustc: the
/// library `app` holds the value in its bytes, and the program `show` prints it.
fn greeting_workspace(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"greet\", \"app\", \"show\"]\nresolver = \"2\"\n",
    );
    write(
        "greet/Cargo.toml",
        "[package]\nname = \"greet\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
    );
    write(
        "greet/src/lib.rs",
        "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn greeting(_: TokenStream) -> TokenStream {\n    let value = std::env::var(\"GREETING\").unwrap_or_default();\n    format!(\"{value:?}\").parse().unwrap()\n}\n",
    );
    write(
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ngreet = { path = \"../greet\" }\n",
    );
    write(
        "app/src/lib.rs",
        "pub const GREETING: &str = greet::greeting!();\n\npub fn greeting() -> &'static str {\n    GREETING\n}\n",
    );
    write(
        "show/Cargo.toml",
        "[package]\nname = \"show\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\napp = { path = \"../app\" }\n",
    );
    write(
        "show/src/main.rs",
        "fn main() {\n    println!(\"greeting={}\", app::greeting());\n}\n",
    );
    write(".gitignore", "target/\n");
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    repo
}

/// `cargo run -p show` through hushrun with `GREETING` set: what the program printed.
fn greet(m: &Machine, fake: &FakeBuild, greeting: &str) -> (String, String) {
    let output = m
        .hushrun(fake)
        .env("GREETING", greeting)
        .args([
            "run",
            "show",
            "--force",
            "--",
            "sh",
            "-c",
            "cargo run -q --offline -p show && sleep 2",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    (String::from_utf8_lossy(&output.stdout).into_owned(), stderr)
}

// covers: B-24
#[test]
fn a_short_value_a_proc_macro_read_without_saying_never_reaches_another_machine() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    // Two values of 14 bytes, under the line that was 16.
    let a = Machine::with(dir.path(), "a", None, greeting_workspace);
    let (printed, stderr) = greet(&a, &fake, "prod-eu-west-1");
    assert!(
        printed.contains("greeting=prod-eu-west-1"),
        "{printed}\n{stderr}"
    );
    // Held back, the variable named and its value nowhere.
    assert!(stderr.contains("stay on this machine"), "{stderr}");
    assert!(stderr.contains("GREETING"), "{stderr}");
    assert!(!stderr.contains("hold the value of prod"), "{stderr}");
    let b = Machine::with(dir.path(), "b", Some(&a.repo), greeting_workspace);
    let (printed, stderr) = greet(&b, &fake, "staging-us-e-1");
    assert!(
        printed.contains("greeting=staging-us-e-1"),
        "the other machine's value: {printed}\n{stderr}"
    );
}

/// Whether any file under `dir` holds `needle`.
fn any_file_holds(dir: &Path, needle: &[u8]) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if let Some(found) = any_file_holds(&path, needle) {
                return Some(found);
            }
        } else if std::fs::read(&path)
            .is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
        {
            return Some(path);
        }
    }
    None
}

/// A sandbox, `b`, whose setup phase fetched what machine `a` built, with the network and an
/// agent token, in the repository `github.com/acme/shop`.
fn fetched_sandbox(dir: &Path, fake: &FakeBuild) -> Machine {
    let a = Machine::new(dir, "a", None);
    set_origin(&a, "https://github.com/acme/shop.git");
    let (code, stderr) = a.build(fake);
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir, "b", Some(&a.repo));
    set_origin(&b, "https://github.com/acme/shop.git");
    let (said, stderr) = fetch(&b, fake, &[]);
    assert!(said.contains("Fetched 2 compiled crates"), "{said}{stderr}");
    b
}

/// `hushrun build fetch CHECK…` in a sandbox's setup phase: its stdout and stderr.
fn fetch(b: &Machine, fake: &FakeBuild, checks: &[&str]) -> (String, String) {
    let output = b
        .hushrun(fake)
        .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN)
        .env_remove("HUSHRUN_BUILD")
        .args(["build", "fetch"])
        .args(checks)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{said}{stderr}");
    (said, stderr)
}

// covers: B-36
#[test]
fn a_fetch_asks_for_each_check_it_names_and_gets_only_that_checks_manifest() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    set_origin(&a, "https://github.com/acme/shop.git");
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    set_origin(&b, "https://github.com/acme/shop.git");
    // A check nobody ran: its manifest is empty, as the backend's would be.
    let (said, stderr) = fetch(&b, &fake, &["lint"]);
    assert!(said.contains("Fetched 0 compiled crates"), "{said}{stderr}");
    // The check that built them, by the name `hushrun run` ran it under.
    let (said, stderr) = fetch(&b, &fake, &["lint", "build"]);
    assert!(said.contains("Fetched 2 compiled crates"), "{said}{stderr}");
    let opens = fake.opens();
    let named: Vec<_> = opens.iter().rev().take(2).map(|o| o.check).collect();
    assert!(named.iter().all(Option::is_some), "{opens:?}");
    assert_ne!(named[0], named[1]);
}

/// `hushrun run build -- cargo build` in a sandbox with the network: an agent token, no person's
/// settings, and `HUSHRUN_BUILD` only as `change` sets it. Its stderr.
fn sandbox_run(
    b: &Machine,
    fake: &FakeBuild,
    change: impl FnOnce(&mut assert_cmd::Command),
) -> String {
    let mut cmd = b.hushrun(fake);
    cmd.env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN)
        .env_remove("HUSHRUN_BUILD");
    change(&mut cmd);
    let output = cmd
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    // The next run builds again.
    let clean = std::process::Command::new("cargo")
        .args(["clean", "-q"])
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(&b.repo)
        .status()
        .unwrap();
    assert!(clean.success());
    stderr
}

// covers: B-25
#[test]
fn an_agents_sandbox_with_the_network_reuses_and_shares_when_the_org_shares() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    // `HUSHRUN_BUILD=off` still turns it off for one command.
    let off = sandbox_run(&b, &fake, |cmd| {
        cmd.env("HUSHRUN_BUILD", "off");
    });
    assert!(!off.contains("build:"), "{off}");
    let stderr = sandbox_run(&b, &fake, |_| {});
    assert!(
        stderr.contains("2 of 2 crates reused (2 from the shared store)"),
        "{stderr}"
    );
    // Nothing wrote a person's settings: the machine is still a sandbox.
    assert!(!b.root.join("build.json").exists());
    // An org that doesn't share: nothing is reused, and nothing said.
    fake.set_sharing(false);
    let opens = fake.count(route::OPEN);
    let stderr = sandbox_run(&b, &fake, |_| {});
    assert!(!stderr.contains("build:"), "{stderr}");
    assert_eq!(fake.count(route::OPEN), opens);
}

/// Signs a person in on `m` for the fake's host, as `hushrun login` leaves the machine.
fn sign_in(m: &Machine, fake: &FakeBuild) {
    let mut hosts = serde_json::Map::new();
    hosts.insert(
        fake.origin(),
        serde_json::json!({"token": FakeBuild::TOKEN, "email": "dev@example.com"}),
    );
    std::fs::write(
        m.root.join("credentials.json"),
        serde_json::to_vec(&serde_json::json!({"hosts": hosts})).unwrap(),
    )
    .unwrap();
}

// covers: B-25
#[test]
fn an_agents_run_where_a_person_is_signed_in_follows_that_persons_settings_off_by_default() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    // A person's laptop: signed in, and nobody ever chose build reuse here. An agent's run
    // with a token is no sandbox's: nothing is reused, asked or said.
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    sign_in(&b, &fake);
    let opens = fake.count(route::OPEN);
    let said = sandbox_run(&b, &fake, |_| {});
    assert!(!said.contains("build:"), "{said}");
    assert_eq!(fake.count(route::OPEN), opens);
    assert!(!b.root.join("cache/build").exists());
    // The person turns it on: the agent's run follows.
    let on = b.hushrun(&fake).args(["build", "on"]).output().unwrap();
    assert!(on.status.success());
    let said = sandbox_run(&b, &fake, |_| {});
    assert!(
        said.contains("2 of 2 crates reused (2 from the shared store)"),
        "{said}"
    );
    // And off again.
    let off = b.hushrun(&fake).args(["build", "off"]).output().unwrap();
    assert!(off.status.success());
    let said = sandbox_run(&b, &fake, |_| {});
    assert!(!said.contains("build:"), "{said}");

    // No person signed in, and an agent token: a sandbox, whatever settings file lies there.
    let c = Machine::new(dir.path(), "c", Some(&a.repo));
    std::fs::write(
        c.root.join("build.json"),
        br#"{"version": 1, "mode": "off"}"#,
    )
    .unwrap();
    let said = sandbox_run(&c, &fake, |_| {});
    assert!(
        said.contains("2 of 2 crates reused (2 from the shared store)"),
        "{said}"
    );
}

// covers: B-26, B-36
#[test]
fn a_fetch_whose_store_host_cant_be_reached_names_the_host_to_allow() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    set_origin(&a, "https://github.com/acme/shop.git");
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    set_origin(&b, "https://github.com/acme/shop.git");
    // The backend answers; its store's host doesn't.
    fake.set_store_origin("http://127.0.0.1:9");
    let (said, stderr) = fetch(&b, &fake, &[]);
    assert!(said.contains("Fetched 0 compiled crates"), "{said}{stderr}");
    assert_eq!(
        stderr
            .matches("the shared store's host, 127.0.0.1, can't be reached from here")
            .count(),
        1,
        "{stderr}"
    );
    assert!(!stderr.contains("/store/"), "{stderr}");
}

// covers: B-36
#[test]
fn a_fetch_with_nothing_to_sign_in_with_names_the_agents_variable() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let b = Machine::new(dir.path(), "b", None);
    let output = b
        .hushrun(&fake)
        .env_remove("HUSHRUN_TOKEN")
        .env_remove("HUSHRUN_BUILD")
        .args(["build", "fetch"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("set HUSHRUN_AGENT for this phase"),
        "{stderr}"
    );
    // Every credential it takes is named.
    for accepted in [
        "an agent token in HUSHRUN_AGENT",
        "an agent or a personal token in HUSHRUN_TOKEN",
        "a person's sign-in (`hushrun login`)",
    ] {
        assert!(stderr.contains(accepted), "no {accepted:?}: {stderr}");
    }
    // The help says the same.
    let help = b
        .hushrun(&fake)
        .args(["build", "fetch", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for accepted in [
        "an agent token in HUSHRUN_AGENT",
        "an agent or a personal token in HUSHRUN_TOKEN",
        "a person's sign-in (`hushrun login`)",
    ] {
        assert!(help.contains(accepted), "no {accepted:?}: {help}");
    }
}

// covers: B-36
#[test]
fn a_fetch_works_with_a_persons_sign_in() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    set_origin(&a, "https://github.com/acme/shop.git");
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    set_origin(&b, "https://github.com/acme/shop.git");
    sign_in(&b, &fake);
    let output = b
        .hushrun(&fake)
        .env_remove("HUSHRUN_TOKEN")
        .env_remove("HUSHRUN_BUILD")
        .args(["build", "fetch"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{said}{stderr}");
    assert!(said.contains("Fetched 2 compiled crates"), "{said}{stderr}");
}

fn set_origin(m: &Machine, url: &str) {
    git::run(&m.repo, &["config", "remote.origin.url", url], None);
}

/// A later phase of the sandbox: no token unless `change` gives one, the backend the fake's org
/// unless `change` names another. Its stderr.
fn later_phase(
    b: &Machine,
    fake: &FakeBuild,
    change: impl FnOnce(&mut assert_cmd::Command),
) -> String {
    let mut cmd = b.hushrun(fake);
    cmd.env_remove("HUSHRUN_TOKEN").env_remove("HUSHRUN_BUILD");
    change(&mut cmd);
    let output = cmd
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    let clean = std::process::Command::new("cargo")
        .args(["clean", "-q"])
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(&b.repo)
        .status()
        .unwrap();
    assert!(clean.success());
    stderr
}

// covers: B-36
#[test]
fn a_sandbox_fetches_at_setup_then_reuses_with_the_network_cut_and_keeps_no_token() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let b = fetched_sandbox(dir.path(), &fake);
    let store = b.root.join("cache/build");
    let packs = store.join("prefetched/packs");
    assert_eq!(std::fs::read_dir(&packs).unwrap().count(), 2);
    assert!(!b.root.join("cache/build/build.json").exists());

    // The later phase: no token, and the backend can't be reached.
    fake.set_unreachable(true);
    let stderr = later_phase(&b, &fake, |_| {});
    assert!(
        stderr.contains("2 of 2 crates reused (2 from the shared store)"),
        "{stderr}"
    );
    // It sent no build request, and the fetch is still there for the next run.
    fake.set_unreachable(false);
    let dropped = fake.dropped();
    assert!(
        dropped.iter().all(|path| !path.contains("/v0/build/")),
        "{dropped:?}"
    );
    assert_eq!(std::fs::read_dir(&packs).unwrap().count(), 2);
    // A later phase that compiles a crate of its own uploads nothing, so it leaves no marker
    // nobody could send, and says nothing about uploads.
    std::fs::write(
        b.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 0\n}\n",
    )
    .unwrap();
    fake.set_unreachable(true);
    let stderr = later_phase(&b, &fake, |_| {});
    fake.set_unreachable(false);
    assert!(stderr.contains("1 of 2 crates reused"), "{stderr}");
    assert!(!stderr.contains("upload"), "{stderr}");
    let markers: Vec<_> = std::fs::read_dir(store.join("outbox"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .collect();
    assert!(markers.is_empty(), "{markers:?}");
    // No token was written under the store or the sessions, by either phase.
    for needle in [FakeBuild::AGENT_TOKEN, FakeBuild::TOKEN] {
        assert_eq!(
            any_file_holds(&b.root.join("cache"), needle.as_bytes()),
            None
        );
    }
}

// covers: B-36
#[test]
fn a_fetch_serves_only_its_own_repository_and_org_and_only_while_the_backend_cant_answer() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let b = fetched_sandbox(dir.path(), &fake);
    let reused = "from the shared store";

    // Offline, in another repository: nothing from the fetch.
    fake.set_unreachable(true);
    set_origin(&b, "https://github.com/acme/other.git");
    let stderr = later_phase(&b, &fake, |_| {});
    assert!(!stderr.contains(reused), "{stderr}");
    set_origin(&b, "https://github.com/acme/shop.git");

    // Offline, naming another org on the same host: nothing.
    let beta = fake.url().replace("/o/acme", "/o/beta");
    let stderr = later_phase(&b, &fake, |cmd| {
        cmd.env("HUSHRUN_BACKEND", &beta);
    });
    assert!(!stderr.contains(reused), "{stderr}");

    // Offline, another host: nothing.
    let stderr = later_phase(&b, &fake, |cmd| {
        cmd.env("HUSHRUN_BACKEND", "http://127.0.0.1:9/o/acme");
    });
    assert!(!stderr.contains(reused), "{stderr}");

    // The backend answers that the org doesn't share: its answer wins.
    fake.set_unreachable(false);
    fake.set_sharing(false);
    let stderr = later_phase(&b, &fake, |cmd| {
        cmd.env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN);
    });
    assert!(!stderr.contains(reused), "{stderr}");
    // It answers 401 to a run with no token: that wins too.
    fake.set_sharing(true);
    let stderr = later_phase(&b, &fake, |_| {});
    assert!(!stderr.contains(reused), "{stderr}");

    // And offline, in its own repository and org, the fetch serves.
    fake.set_unreachable(true);
    let stderr = later_phase(&b, &fake, |_| {});
    assert!(
        stderr.contains("2 of 2 crates reused (2 from the shared store)"),
        "{stderr}"
    );
}

// covers: B-36, B-31
#[test]
fn a_github_job_whose_backend_is_cut_off_reuses_what_its_setup_step_fetched() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    set_origin(&a, "https://github.com/acme/shop.git");
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");

    // A coding agent's job: its setup step fetches with the job's ID token.
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let runner_temp = b.root.join("runner-temp");
    let mut cmd = b.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &runner_temp);
    let output = cmd
        .env("GITHUB_REPOSITORY_ID", "4242")
        .env("GITHUB_RUN_ID", "300")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_JOB", "copilot")
        .args(["build", "fetch"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{said}{stderr}");
    assert!(said.contains("Fetched 2 compiled crates"), "{said}{stderr}");
    // Into the job's own store, never the cache, which outlives the job.
    assert!(!b.root.join("cache/build").exists());
    assert_eq!(ci_stores(&runner_temp).len(), 1);

    // A later step of the same job, behind a firewall that cuts the backend off.
    fake.set_unreachable(true);
    let mut cmd = b.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &runner_temp);
    let output = cmd
        .env("GITHUB_REPOSITORY_ID", "4242")
        .env("GITHUB_RUN_ID", "300")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_JOB", "copilot")
        .args([
            "run",
            "build",
            "--force",
            "--",
            "sh",
            "-c",
            "echo \"wrapper=${RUSTC_WRAPPER:-none}\"; cargo build --offline",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    assert!(!stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    assert!(
        stderr.contains("2 of 2 crates reused (2 from the shared store)"),
        "{stderr}"
    );
    // Read only: no build request left the job.
    let dropped = fake.dropped();
    assert!(
        dropped.iter().all(|path| !path.contains("/v0/build/")),
        "{dropped:?}"
    );
}

/// `cargo clean` and `cargo build` through hushrun in a GitHub Actions job, timed by COMMAND's own marks:
/// what cargo took, what the run took after COMMAND ended, and its stdout and stderr.
fn timed_ci_build(m: &Machine, fake: &FakeBuild) -> (Duration, Duration, String) {
    let marks = m.root.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    let mut cmd = m.hushrun(fake);
    in_github_actions(&mut cmd, fake, &m.root.join("runner-temp"));
    let output = cmd
        .env("MARKS", &marks)
        .args([
            "run",
            "build",
            "--force",
            "--",
            "sh",
            "-c",
            "echo \"wrapper=${RUSTC_WRAPPER:-none}\"; cargo clean -q; touch \"$MARKS/start\"; cargo build --offline; touch \"$MARKS/end\"",
        ])
        .output()
        .unwrap();
    let exited = std::time::SystemTime::now();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{said}");
    assert!(!said.contains("wrapper=none"), "{said}");
    let mark = |name: &str| {
        std::fs::metadata(marks.join(name))
            .unwrap()
            .modified()
            .unwrap()
    };
    let cargo = mark("end").duration_since(mark("start")).unwrap();
    let after = exited.duration_since(mark("end")).unwrap();
    (cargo, after, said)
}

// covers: B-26, B-31
#[test]
fn a_slow_id_token_endpoint_slows_neither_cargo_nor_the_end_of_a_job() {
    // Two cold jobs, each against its own backend, whose ID tokens live 61 seconds: every
    // request finds the token held within a minute of its expiry.
    let dir = tempfile::tempdir().unwrap();
    let prompt = FakeBuild::start();
    prompt.set_id_token_seconds(61);
    let a = Machine::new(dir.path(), "a", None);
    let (cargo_prompt, after_prompt, said) = timed_ci_build(&a, &prompt);
    assert!(said.contains("2 crates built"), "{said}");
    // The second's endpoint takes 3 seconds to mint.
    let slow = FakeBuild::start();
    slow.set_id_token_seconds(61);
    slow.delay("/id-token", Duration::from_secs(3));
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (cargo_slow, after_slow, said) = timed_ci_build(&b, &slow);
    assert!(said.contains("2 crates built"), "{said}");
    assert!(
        cargo_slow < cargo_prompt.mul_f64(1.1) + Duration::from_secs(1),
        "cargo took {cargo_slow:?} against {cargo_prompt:?}: {said}"
    );
    assert!(
        after_slow < after_prompt + Duration::from_secs(1),
        "the end took {after_slow:?} against {after_prompt:?}: {said}"
    );
}

/// The build requests the fake was sent after the first `after`, each its route and whether its
/// body names a commit.
fn commits_since(fake: &FakeBuild, after: usize) -> Vec<(String, bool)> {
    fake.seen()
        .into_iter()
        .skip(after)
        .filter(|s| s.path.contains("/v0/build/"))
        .map(|s| {
            let body: serde_json::Value = serde_json::from_slice(&s.body).unwrap();
            (s.route, body.get("commit").is_some())
        })
        .collect()
}

// covers: B-31
#[test]
fn a_job_whose_tracked_files_differ_from_its_commit_names_no_commit() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    // A clean checkout names its commit in every request.
    let a = Machine::new(dir.path(), "a", None);
    let (code, _, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    let sent = commits_since(&fake, 0);
    assert!(sent.iter().all(|(_, named)| *named), "{sent:?}");
    // A job that changed a tracked file before building, as a version bump or a nightly
    // `cargo update` does: the code it builds isn't its commit's, so no request names one.
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    std::fs::write(
        b.repo.join("dep/src/lib.rs"),
        "pub fn two() -> u32 {\n    (0..2).map(|x| x * 1).sum::<u32>() + 1 + 0\n}\n",
    )
    .unwrap();
    let seen = fake.seen().len();
    let (code, _, stderr) = ci_build(&b, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    let sent = commits_since(&fake, seen);
    assert!(sent.iter().all(|(_, named)| !named), "{sent:?}");
    for route in [route::OPEN, route::LOOKUP, route::BEGIN, route::CLOSE] {
        assert!(sent.iter().any(|(r, _)| r == route), "no {route}: {sent:?}");
    }
    // A file git doesn't track isn't compared.
    let c = Machine::new(dir.path(), "c", Some(&a.repo));
    std::fs::write(c.repo.join("notes.txt"), "not tracked\n").unwrap();
    let seen = fake.seen().len();
    let (code, _, stderr) = ci_build(&c, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    let sent = commits_since(&fake, seen);
    assert!(
        !sent.is_empty() && sent.iter().all(|(_, named)| *named),
        "{sent:?}"
    );
}

/// The block in the store under `store` whose files are `dep`'s, its files overwritten with
/// zeros of the same sizes and its record saying so: a download this compiler can't load, as one
/// from a machine whose bytes are wrong would be. Its key and first key.
fn spoil_dep(store: &Path) -> (hushrun_core::Digest, hushrun_core::Digest) {
    fn records(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() && !path.is_symlink() {
                records(&path, out);
            } else if path.file_name().is_some_and(|n| n == "block.json") {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    records(&store.join("blocks"), &mut found);
    let mut spoiled = None;
    for json in found {
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
        let names: Vec<String> = record["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_string())
            .collect();
        if !names.iter().any(|n| n.starts_with("libdep-")) {
            continue;
        }
        for file in record["files"].as_array_mut().unwrap() {
            let path = json
                .with_file_name("files")
                .join(file["name"].as_str().unwrap());
            let zeros = vec![0u8; file["size"].as_u64().unwrap() as usize];
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, &zeros).unwrap();
            file["sha256"] = serde_json::to_value(hushrun_core::Digest::of(&zeros)).unwrap();
        }
        std::fs::remove_file(&json).unwrap();
        std::fs::write(&json, serde_json::to_vec(&record).unwrap()).unwrap();
        let key: hushrun_core::Digest = serde_json::from_value(record["key"].clone()).unwrap();
        let step: hushrun_core::Digest =
            serde_json::from_value(record["description"]["step"].clone()).unwrap();
        spoiled = Some((key, step));
    }
    spoiled.expect("dep's block")
}

/// `dep`'s outputs in the workspace's target folder.
fn dep_outputs(repo: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(repo.join("target/debug/deps"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("libdep-") && (name.ends_with(".rlib") || name.ends_with(".rmeta"))
        })
        .collect()
}

// covers: B-15, B-34
#[test]
fn a_dependent_that_cant_load_a_downloaded_block_suspends_it_and_reports_it_rejected() {
    let fake = FakeBuild::start();
    // A scope of its own, so the report's is the one the block's record names.
    fake.set_scopes(Some("person/7"), &["person/7"]);
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (code, stderr) = b.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("(2 from the shared store)"), "{stderr}");
    // B's copy of `dep` turns out to be bytes its compiler can't load, and `app` changes, so it
    // compiles against the reused `dep`.
    let store = b.root.join("cache/build");
    let (key, step) = spoil_dep(&store);
    std::fs::write(
        b.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 0\n}\n",
    )
    .unwrap();
    let reports = fake.reports().len();
    let (code, stderr) = clean_build(&b, &fake);
    assert_ne!(code, 0, "{stderr}");
    assert!(
        stderr.contains(
            "app could not load reused dep; suspended and removed so cargo builds them again"
        ),
        "{stderr}"
    );
    // Reported once, `rejected`, to the scope the block's record names.
    let sent = fake.reports();
    assert_eq!(sent.len(), reports + 1, "{sent:?}\n{stderr}");
    let report = sent.last().unwrap();
    let origin = hushrun_build::store::Store::new(&store)
        .origin(&key)
        .expect("the block's record");
    assert_eq!(origin.vouched[0].scope, "person/7");
    assert_eq!(report.scope, origin.vouched[0].scope);
    assert_eq!(
        report.address,
        hushrun_core::blocks::address("rustc/1", key.as_bytes())
    );
    assert_eq!(report.problem, hushrun_core::blocks::Problem::Rejected);
    // Its first key is suspended for the kept days (15), and its reused files are gone.
    let local = hushrun_build::store::Store::new(&store);
    let now = std::time::SystemTime::now();
    let day = Duration::from_secs(86_400);
    assert!(local.suspended_at(&step, now).is_some());
    assert!(local.suspended_at(&step, now + day * 14).is_some());
    assert!(local.suspended_at(&step, now + day * 16).is_none());
    assert_eq!(dep_outputs(&b.repo), Vec::<PathBuf>::new());
    // The next run builds `dep` here, and the build passes.
    let stderr = warm_build(&b, &fake);
    assert!(stderr.contains("2 crates built, none reused"), "{stderr}");
    assert_eq!(fake.reports().len(), reports + 1);
}

/// `cargo clean`, then `cargo build` through hushrun, which may fail: its exit code and stderr.
fn clean_build(m: &Machine, fake: &FakeBuild) -> (i32, String) {
    let clean = std::process::Command::new("cargo")
        .args(["clean", "-q"])
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(&m.repo)
        .status()
        .unwrap();
    assert!(clean.success());
    let output = m
        .hushrun(fake)
        .args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A workspace whose library `app` expands the proc macro `mac`, committed in a repository.
fn macro_workspace(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"mac\", \"app\"]\nresolver = \"2\"\n",
    );
    write(
        "mac/Cargo.toml",
        "[package]\nname = \"mac\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
    );
    write("mac/src/lib.rs", &macro_source("42"));
    write(
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nmac = { path = \"../mac\" }\n",
    );
    write(
        "app/src/lib.rs",
        "pub fn answer() -> u32 {\n    mac::answer!()\n}\n",
    );
    write(".gitignore", "target/\n");
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    repo
}

/// The proc macro's source: `answer!()` expands to `value`.
fn macro_source(value: &str) -> String {
    format!(
        "use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn answer(_: TokenStream) -> TokenStream {{\n    \"{value}\".parse().unwrap()\n}}\n"
    )
}

/// The proc macro's file in `repo`'s target folder.
fn macro_file(repo: &Path) -> PathBuf {
    std::fs::read_dir(repo.join("target/debug/deps"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("libmac-") && name.ends_with(std::env::consts::DLL_SUFFIX)
        })
        .expect("the proc macro's file")
}

/// What a CI cache that someone poisoned holds: `repo`'s target folder with the proc macro's
/// file built from other source (by plain cargo, as an attacker's job would), and the macro's
/// record rewritten to match the new bytes while it still names the honest macro.
fn poison_macro(repo: &Path) {
    let source = repo.join("mac/src/lib.rs");
    std::fs::write(&source, macro_source("43")).unwrap();
    let mut plain = std::process::Command::new("cargo");
    for var in CLEARED {
        plain.env_remove(var);
    }
    let built = plain
        .args(["build", "-q", "--offline", "-p", "mac"])
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(repo)
        .status()
        .unwrap();
    assert!(built.success());
    std::fs::write(&source, macro_source("42")).unwrap();
    let so = macro_file(repo);
    let bytes = std::fs::read(&so).unwrap();
    let record = so.parent().unwrap().join(".hushrun").join(format!(
        "{}.json",
        so.file_name().unwrap().to_string_lossy()
    ));
    let mut text: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert!(text["produced_by"].is_string(), "{text}");
    text["sha256"] = serde_json::to_value(hushrun_core::Digest::of(&bytes)).unwrap();
    std::fs::write(&record, serde_json::to_vec(&text).unwrap()).unwrap();
}

/// A checkout of `from` on another runner, with `app` changed and committed, as a push of that
/// change makes it; `target` (when given) restored into it as a cache restores one.
fn pushed(dir: &Path, name: &str, from: &Path, target: Option<&Path>) -> Machine {
    let m = Machine::with(dir, name, Some(from), macro_workspace);
    if let Some(target) = target {
        let copied = std::process::Command::new("cp")
            .arg("-R")
            .arg(target)
            .arg(m.repo.join("target"))
            .status()
            .unwrap();
        assert!(copied.success());
    }
    std::fs::write(
        m.repo.join("app/src/lib.rs"),
        "pub fn answer() -> u32 {\n    mac::answer!() + 0\n}\n",
    )
    .unwrap();
    git::commit_all(&m.repo, "app changes");
    m
}

// covers: B-6, B-24, B-31
#[test]
fn a_job_that_restores_a_doctored_proc_macro_uploads_nothing_built_against_it() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    // As CI sets it: a restored target folder holds the dep-info an incremental rebuild keys on,
    // and the wrapper passes such a rebuild through (B-9).
    let job = |m: &Machine, run: &str| {
        job_run(
            m,
            &fake,
            &m.root.join("runner-temp"),
            run,
            "CARGO_INCREMENTAL=0 cargo build --offline && sleep 2",
        )
    };
    let a = Machine::with(dir.path(), "a", None, macro_workspace);
    let (stderr, close) = job(&a, "300");
    assert_eq!(close.expect(&stderr).counts.built, 2, "{stderr}");
    assert!(!fake.stored().is_empty(), "{stderr}");

    // The next push's job restores a target folder whose macro was doctored, and `app` changed,
    // so it compiles against that macro, in a store of its own.
    poison_macro(&a.repo);
    let c = pushed(dir.path(), "c", &a.repo, Some(&a.repo.join("target")));
    let stored = fake.stored().len();
    let (stderr, close) = job(&c, "301");
    assert!(stderr.contains("1 crate built, none reused"), "{stderr}");
    assert_eq!(close.expect(&stderr).counts.built, 1, "{stderr}");
    assert_eq!(fake.stored().len(), stored, "it uploaded: {stderr}");

    // A job with an honest checkout of the same push builds `app` itself: nothing compiled
    // against the doctored macro came down under its key.
    let b = pushed(dir.path(), "b", &a.repo, None);
    let (stderr, close) = job(&b, "302");
    let close = close.expect(&stderr);
    assert_eq!(close.counts.reused_shared, 0, "{close:?}\n{stderr}");
    assert_eq!(close.counts.built, 2, "{close:?}\n{stderr}");
    assert!(
        fake.stored().len() > stored,
        "the honest app went up: {stderr}"
    );
}
