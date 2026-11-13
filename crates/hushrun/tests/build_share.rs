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

// covers: B-26, B-34
#[test]
fn a_warm_build_with_stale_vouches_and_a_slow_open_takes_what_it_takes_with_sharing_off() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (code, stderr) = b.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("from the shared store)"), "{stderr}");
    // What a warm build takes with sharing on and vouches fresh, and what a build with sharing
    // off takes on a third checkout (it compiles: downloaded hints aren't followed there).
    let (fresh, stderr) = timed_build(&b, &fake, "on");
    assert!(stderr.contains("2 of 2 crates reused"), "{stderr}");
    let c = Machine::new(dir.path(), "c", Some(&a.repo));
    let (off, _) = timed_build(&c, &fake, "off");
    // A day off: every vouch B holds is older than a day, and `open` takes 10 seconds.
    assert!(age_vouches(&b.root.join("cache/build")) > 0);
    fake.delay(route::OPEN, Duration::from_secs(10));
    let (on, stderr) = timed_build(&b, &fake, "on");
    assert!(stderr.contains("2 of 2 crates reused"), "{stderr}");
    assert!(
        on < fresh.max(off) + Duration::from_millis(1_500),
        "with stale vouches {on:?}, fresh {fresh:?}, sharing off {off:?}: {stderr}"
    );
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
