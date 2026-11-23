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
    "HUSHRUN_ID_TOKEN",
    "CI_JOB_ID",
    "CI_JOB_TOKEN",
    "CI_SERVER_URL",
    "HUSHRUN_PROVIDER",
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
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
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
        // git's global configuration for the runs: an empty file of this machine's own. Its
        // path is in no compiled output, where `/dev/null` is: on Linux a proc macro's file
        // holds that string, from the standard library linked into it, and the run would keep
        // the macro's block on the machine for holding the variable's value (B-24).
        std::fs::write(root.join("gitconfig"), "").unwrap();
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
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("HUSHRUN_BUILD_KEY_LOG", self.root.join("keys.jsonl"));
        cmd
    }

    /// Each crate this machine compiled and whether its block may leave it, with every reason
    /// it may not, from the runs' key log: what a failed "it uploaded" says beside the run's
    /// own lines, which name only some of the reasons.
    fn may_leave(&self) -> String {
        let log = std::fs::read_to_string(self.root.join("keys.jsonl")).unwrap_or_default();
        log.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .map(|line| format!("{}: {}\n", line["crate_name"], line["portable"]))
            .collect()
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

/// A workspace of one library, `app`, whose build script gives it a variable, committed in a
/// repository.
fn script_workspace(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    let write = |path: &str, text: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        "build.rs",
        "fn main() {\n    println!(\"cargo:rustc-env=APP_GREETING=hello\");\n}\n",
    );
    write(
        "src/lib.rs",
        "pub fn greeting() -> &'static str {\n    env!(\"APP_GREETING\")\n}\n",
    );
    write(".gitignore", "target/\n");
    git::init(&repo, "sha1");
    git::commit_all(&repo, "init");
    repo
}

/// The files of each build script's block in the store under `dir` that a compile here didn't
/// make: the blocks only downloaded.
fn downloaded_build_scripts(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(shards) = std::fs::read_dir(dir.join("blocks")) else {
        return found;
    };
    for block in shards
        .flatten()
        .flat_map(|s| std::fs::read_dir(s.path()))
        .flatten()
    {
        let block = block.unwrap().path();
        let record = std::fs::read_to_string(block.join("block.json")).unwrap_or_default();
        let origin = std::fs::read_to_string(block.join("origin.json")).unwrap_or_default();
        if record.contains("\"build-script\"") && !origin.contains("\"built\"") {
            let files = std::fs::read_dir(block.join("files")).unwrap();
            found.extend(files.flatten().map(|f| f.path()));
        }
    }
    found
}

// covers: B-12, B-28
#[test]
fn a_build_script_from_the_shared_store_starts_also_when_the_store_holds_it_without_its_run_bits() {
    use std::os::unix::fs::PermissionsExt as _;
    // A pack keeps no file mode. Cargo starts a build script itself, so one put in place without
    // its run bits fails the build with "Permission denied", and every build after it.
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::with(dir.path(), "a", None, script_workspace);
    for _ in 0..5 {
        let (code, stderr) = a.build(&fake);
        assert_eq!(code, 0, "{stderr}");
        let stored = fake.stored();
        if stored.len() >= 2
            && stored
                .iter()
                .any(|b| b.class == hushrun_core::blocks::Class::BuildScript)
        {
            break;
        }
    }
    let stored = fake.stored();
    assert!(
        stored
            .iter()
            .any(|b| b.class == hushrun_core::blocks::Class::BuildScript),
        "A's build script is stored: {}",
        a.may_leave()
    );

    let b = Machine::with(dir.path(), "b", Some(&a.repo), script_workspace);
    let (code, stderr) = b.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    let store = b.root.join("cache/build");
    let scripts = downloaded_build_scripts(&store);
    assert!(
        !scripts.is_empty(),
        "B downloaded no build script: {stderr}"
    );

    // The store as a hushrun that unpacked it without its run bits left it: B's next build in a
    // clean target folder reuses that block, and cargo starts what it puts in place.
    for file in &scripts {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let stderr = rebuilt(&b, &fake, |_| {});
    assert!(stderr.contains("2 of 2 crates reused"), "{stderr}");
    for file in &scripts {
        let mode = std::fs::metadata(file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o444, "a stored block is never rewritten");
    }
}

// covers: B-26
#[test]
fn a_server_that_sleeps_ten_seconds_on_open_doesnt_delay_cargo() {
    let fake = FakeBuild::start();
    let delay = Duration::from_secs(10);
    fake.delay(route::OPEN, delay);
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let started = Instant::now();
    let (code, stderr) = a.build(&fake);
    let took = started.elapsed();
    assert_eq!(code, 0, "{stderr}");
    // What the run took beyond its command (cargo, by cargo's own count, then the two seconds
    // the command sleeps) is far from the server's sleep: a run that waited for `open` spends
    // all of it there. Measured against the command, so a slow machine's compile is not taken
    // for a wait.
    let command = cargo_took(&stderr) + Duration::from_secs(2);
    let beyond = took.saturating_sub(command);
    assert!(
        beyond < delay - Duration::from_secs(3),
        "{beyond:?} beyond the command's {command:?}: {stderr}"
    );
    assert!(stderr.contains("build: 2 crates built"), "{stderr}");
}

/// How long cargo says its build took, from its `Finished … in 1.23s` (or `in 1m 02s`) line.
fn cargo_took(stderr: &str) -> Duration {
    let line = stderr
        .lines()
        .find(|line| line.trim_start().starts_with("Finished"))
        .unwrap_or_else(|| panic!("cargo's last line: {stderr}"));
    let said = line.rsplit(" in ").next().unwrap();
    let seconds: f64 = said
        .split_whitespace()
        .map(|part| match part.strip_suffix('m') {
            Some(minutes) => minutes.parse::<f64>().unwrap() * 60.0,
            None => part.trim_end_matches('s').parse::<f64>().unwrap(),
        })
        .sum();
    Duration::from_secs_f64(seconds)
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
                .env("BUILDKITE", "true");
        }),
        "ci        this is CI: `hushrun run NAME -- COMMAND` reuses in the job's own store when the org shares, reading the org's blocks and uploading none (an agent token outside GitHub Actions and GitLab CI); no person's settings are read"
    );
    // GitLab CI: the job's ID token, or nothing, whatever agent token it holds (B-48).
    let gitlab = |token: Option<String>| {
        let fake = &fake;
        move |cmd: &mut assert_cmd::Command| {
            cmd.env_remove("GITHUB_ACTIONS")
                .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
                .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
                .env("GITLAB_CI", "true")
                .env("CI_JOB_ID", "5000")
                .env("CI_JOB_TOKEN", "glcbt-job-token-of-5000");
            if let Some(token) = token {
                cmd.env("HUSHRUN_ID_TOKEN", token);
            }
            let _ = fake;
        }
    };
    assert_eq!(
        ci_status(&a, &fake, gitlab(None)),
        format!(
            "ci        this is CI, where `hushrun run` reuses nothing here: the job can't mint an ID token (declare `id_tokens: {{HUSHRUN_ID_TOKEN: {{aud: {}}}}}` in the job)",
            fake.url()
        )
    );
    assert_eq!(
        ci_status(&a, &fake, gitlab(Some(fake.gitlab_token(5000)))),
        "ci        this is CI: `hushrun run NAME -- COMMAND` reuses in the job's own store when the org shares, and shares as the org's mode says; no person's settings are read"
    );
    // No backend.
    assert_eq!(
        ci_status(&a, &fake, |cmd| {
            cmd.env_remove("HUSHRUN_BACKEND")
                .env_remove("HUSHRUN_TOKEN");
        }),
        format!(
            "ci        this is CI, where `hushrun run` reuses nothing here: {NO_BACKEND_IN_CI}"
        )
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
    let delay = Duration::from_secs(10);
    fake.delay(route::OPEN, delay);
    let seen = fake.seen().len();
    let (stale, stderr) = timed_build(&b, &fake, "on");
    assert!(stderr.contains("2 of 2 crates reused"), "{stderr}");
    // Bounded by the delay injected, not by the machine's speed: a crate that waited for `open`
    // itself spends all 10 seconds, and two builds of a loaded machine differ by a second or
    // two with nothing waiting.
    assert!(
        stale < fresh + delay / 2,
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

/// [`Machine::build`], with cargo held back until the run's `open` has been answered. A run
/// outside CI never waits for `open` (B-26), and a wrapper that asks before its answer gets a
/// miss, which is what a loaded machine gives a test that needs the downloads.
fn build_once_open_answered(m: &Machine, fake: &FakeBuild) -> (i32, String) {
    let go = m.root.join("go");
    let _ = std::fs::remove_file(&go);
    let opens = fake.count(route::OPEN);
    std::thread::scope(|scope| {
        let run = scope.spawn(|| {
            let output = m
                .hushrun(fake)
                .env("GO", &go)
                .args([
                    "run",
                    "build",
                    "--force",
                    "--",
                    "sh",
                    "-c",
                    "while [ ! -e \"$GO\" ]; do sleep 0.05; done; cargo build --offline && sleep 2",
                ])
                .output()
                .unwrap();
            (
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )
        });
        // The fake counts `open` when it arrives and answers at once; the relay then takes
        // the answer in, for which it gets a moment.
        while fake.count(route::OPEN) == opens && !run.is_finished() {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(500));
        std::fs::write(&go, "").unwrap();
        run.join().unwrap()
    })
}

/// Rewrites what every compile recorded in the store under `dir` took, to `ms`: the files
/// named `cost` (B-26). Panics when there is none.
fn slow_compiles_recorded(dir: &Path, ms: u64) {
    fn walk(dir: &Path, ms: u64, found: &mut usize) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, ms, found);
            } else if path.file_name().is_some_and(|name| name == "cost") {
                std::fs::write(&path, ms.to_string()).unwrap();
                *found += 1;
            }
        }
    }
    let mut found = 0;
    walk(dir, ms, &mut found);
    assert!(found > 0, "no compile's cost under {}", dir.display());
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
    // A block A's run had no time to send waits in its outbox for A's next run (B-29), which
    // a loaded machine makes likelier: A runs until both are in the store, before B asks.
    for _ in 0..5 {
        if fake.stored().len() >= 2 {
            break;
        }
        let (code, stderr) = a.build(fake);
        assert_eq!(code, 0, "{stderr}");
    }
    assert_eq!(fake.stored().len(), 2, "A's two blocks are stored");
    // A wrapper waits for a download half of what the crate's compile took, there and here
    // (B-26): a few milliseconds for these crates, which a loaded machine's download misses.
    // Both sides say a minute, so B waits for its downloads however long they take.
    fake.set_costs(60_000);
    slow_compiles_recorded(&b.root.join("cache/build"), 60_000);
    let (code, stderr) = build_once_open_answered(&b, fake);
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

/// B-34: only a lookup no server answered leaves a run "unreached". After the org stopped
/// sharing, a results lookup that got any other answer than a well-formed one is no reason to
/// reuse what B only downloaded.
fn an_answer_that_is_no_yes_reuses_no_download(fault: fn(&FakeBuild)) {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let (_, b) = built_and_downloaded(dir.path(), &fake);
    fake.set_sharing(false);
    fault(&fake);
    let said = rebuilt(&b, &fake, |_| {});
    assert!(said.contains("2 crates built, none reused"), "{said}");
}

// covers: B-34
#[test]
fn a_redirect_on_the_results_lookup_reuses_no_downloaded_block() {
    an_answer_that_is_no_yes_reuses_no_download(|fake| fake.fail(route::RESULTS, 302, 1000));
}

// covers: B-34
#[test]
fn a_cut_answer_to_the_results_lookup_reuses_no_downloaded_block() {
    an_answer_that_is_no_yes_reuses_no_download(|fake| fake.cut(route::RESULTS, 1000));
}

// covers: B-34
#[test]
fn a_page_that_is_no_json_on_the_results_lookup_reuses_no_downloaded_block() {
    an_answer_that_is_no_yes_reuses_no_download(|fake| {
        fake.answer_no_json(route::RESULTS, 1000);
    });
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
}

// covers: B-25
#[test]
fn a_written_build_off_wins_in_a_sandbox_and_no_setting_leaves_reuse_on() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    // No person signed in, and an agent token: a sandbox. Its settings file says `off`, as
    // `hushrun build off` writes it: nothing is reused, asked or said.
    let c = Machine::new(dir.path(), "c", Some(&a.repo));
    std::fs::write(
        c.root.join("build.json"),
        br#"{"version": 1, "mode": "off"}"#,
    )
    .unwrap();
    let opens = fake.count(route::OPEN);
    let said = sandbox_run(&c, &fake, |_| {});
    assert!(!said.contains("build:"), "{said}");
    assert_eq!(fake.count(route::OPEN), opens);
    assert!(!c.root.join("cache/build").exists());
    // `hushrun build status` says the same, from the same definition.
    let sharing = |m: &Machine| {
        let output = m
            .hushrun(&fake)
            .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN)
            .env_remove("HUSHRUN_BUILD")
            .args(["build", "status", "--format", "json"])
            .output()
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        status["sharing"].clone()
    };
    assert_eq!(
        sharing(&c),
        serde_json::json!({"mode": "off", "why": "build reuse is off on this machine"})
    );
    // `HUSHRUN_BUILD=on` is this command's mode, over the file, as on a person's machine.
    let said = sandbox_run(&c, &fake, |cmd| {
        cmd.env("HUSHRUN_BUILD", "on");
    });
    assert!(
        said.contains("2 of 2 crates reused (2 from the shared store)"),
        "{said}"
    );
    // A settings file that names no mode has no `off` written: reuse stays on, the sandbox's
    // default, as with no file at all.
    let d = Machine::new(dir.path(), "d", Some(&a.repo));
    std::fs::write(
        d.root.join("build.json"),
        br#"{"version": 1, "keep_days": 7}"#,
    )
    .unwrap();
    let said = sandbox_run(&d, &fake, |_| {});
    assert!(
        said.contains("2 of 2 crates reused (2 from the shared store)"),
        "{said}"
    );
    let e = Machine::new(dir.path(), "e", Some(&a.repo));
    assert!(!e.root.join("build.json").exists());
    assert_eq!(sharing(&e)["mode"], "on");
    let said = sandbox_run(&e, &fake, |_| {});
    assert!(
        said.contains("2 of 2 crates reused (2 from the shared store)"),
        "{said}"
    );
}

// covers: B-21, B-25
#[test]
fn a_save_of_another_setting_in_a_sandbox_writes_no_mode_and_reuse_stays_on() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stderr) = a.build(&fake);
    assert_eq!(code, 0, "{stderr}");
    // A sandbox where a setup step saved the share setting: nobody chose a mode, so the file
    // names none, and reuse stays on, the sandbox's default.
    let f = Machine::new(dir.path(), "f", Some(&a.repo));
    let saved = f
        .hushrun(&fake)
        .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN)
        .args(["build", "share", "observe"])
        .output()
        .unwrap();
    assert!(saved.status.success());
    let file = |m: &Machine| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(m.root.join("build.json")).unwrap()).unwrap()
    };
    assert_eq!(file(&f)["share"], "observe");
    assert!(file(&f).get("mode").is_none(), "{}", file(&f));
    let said = sandbox_run(&f, &fake, |_| {});
    assert!(
        said.contains("2 of 2 crates reused (2 from the shared store)"),
        "{said}"
    );
    // `hushrun build off` is a choice: it is written, and wins. A later save of another
    // setting keeps it.
    let off = f.hushrun(&fake).args(["build", "off"]).output().unwrap();
    assert!(off.status.success());
    assert_eq!(file(&f)["mode"], "off");
    let saved = f
        .hushrun(&fake)
        .args(["build", "share", "on"])
        .output()
        .unwrap();
    assert!(saved.status.success());
    assert_eq!(file(&f)["mode"], "off");
    let said = sandbox_run(&f, &fake, |_| {});
    assert!(!said.contains("build:"), "{said}");
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
    // The second's endpoint takes 3 seconds to mint: the longest that leaves room under the
    // 5 seconds a mint gets and a job waits for `open`.
    let mint = Duration::from_secs(3);
    let slow = FakeBuild::start();
    slow.set_id_token_seconds(61);
    slow.delay("/id-token", mint);
    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (cargo_slow, after_slow, said) = timed_ci_build(&b, &slow);
    assert!(said.contains("2 crates built"), "{said}");
    // Requests that waited for a token would cost cargo two mints here and the end one. The
    // bounds are half of each, taken from the mint and not from what the prompt job took: a
    // loaded machine has seconds of room in cargo's part before it looks like a wait.
    assert!(
        cargo_slow < cargo_prompt + mint,
        "cargo took {cargo_slow:?} against {cargo_prompt:?}: {said}"
    );
    assert!(
        after_slow < after_prompt + mint / 2,
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

/// The proc macro's file in `repo`'s target folder: the only one there.
fn macro_file(repo: &Path) -> PathBuf {
    let mut files: Vec<PathBuf> = std::fs::read_dir(repo.join("target/debug/deps"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("libmac-") && name.ends_with(std::env::consts::DLL_SUFFIX)
        })
        .collect();
    assert_eq!(files.len(), 1, "one proc macro's file: {files:?}");
    files.remove(0)
}

/// What a CI cache that someone poisoned holds: `repo`'s target folder with the proc macro's
/// file built from other source (by plain cargo, as an attacker's job would), and the macro's
/// record rewritten to match the new bytes while it still names the honest macro.
fn poison_macro(repo: &Path) {
    let honest = std::fs::read(macro_file(repo)).unwrap();
    let source = repo.join("mac/src/lib.rs");
    std::fs::write(&source, macro_source("43")).unwrap();
    let mut plain = std::process::Command::new("cargo");
    for var in CLEARED {
        plain.env_remove(var);
    }
    // With the job's own CARGO_INCREMENTAL: cargo names a crate's files by its profile, which
    // that variable is part of, so a build without it writes a second file beside the job's
    // and doctors nothing.
    let built = plain
        .args(["build", "-q", "--offline", "-p", "mac"])
        .env("CARGO_TERM_COLOR", "never")
        .env("CARGO_INCREMENTAL", "0")
        .current_dir(repo)
        .status()
        .unwrap();
    assert!(built.success());
    std::fs::write(&source, macro_source("42")).unwrap();
    let so = macro_file(repo);
    let bytes = std::fs::read(&so).unwrap();
    assert_ne!(bytes, honest, "the job's own macro file was replaced");
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
    // The change is written until its file is newer than everything restored: cargo takes a
    // source no newer than its last build's record for unchanged, and a restore and a write a
    // few milliseconds apart can carry one time on Linux.
    let restored = m.root.join("restored");
    std::fs::write(&restored, "").unwrap();
    let time = |path: &Path| std::fs::metadata(path).unwrap().modified().unwrap();
    let source = m.repo.join("app/src/lib.rs");
    loop {
        std::fs::write(
            &source,
            "pub fn answer() -> u32 {\n    mac::answer!() + 0\n}\n",
        )
        .unwrap();
        if time(&source) > time(&restored) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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
    assert!(!fake.stored().is_empty(), "{stderr}\n{}", a.may_leave());

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
    // against the doctored macro came down under its key. The macro is the first job's honest
    // one, which may come down now that a proc macro travels (within its download's budget:
    // either is right), and is the only thing that may.
    let b = pushed(dir.path(), "b", &a.repo, None);
    let (stderr, close) = job(&b, "302");
    let close = close.expect(&stderr);
    assert!(close.counts.reused_shared <= 1, "{close:?}\n{stderr}");
    assert_eq!(
        close.counts.built + close.counts.reused_shared,
        2,
        "{close:?}\n{stderr}"
    );
    // `app` is what it built: its block goes up, below, which a reused one never does.
    assert!(close.counts.built >= 1, "{close:?}\n{stderr}");
    assert!(
        fake.stored().len() > stored,
        "the honest app went up: {stderr}\n{}",
        b.may_leave()
    );
}

/// What a GitLab CI job's environment adds (B-48): its id, the project, a temporary folder of the
/// test's own as the system's, the job's ID token when it has one, and an agent token beside it.
fn in_gitlab_ci(cmd: &mut assert_cmd::Command, temp: &Path, job: &str, token: Option<&str>) {
    std::fs::create_dir_all(temp).unwrap();
    cmd.env("CI", "true")
        .env("GITLAB_CI", "true")
        .env("CI_JOB_ID", job)
        .env("CI_PIPELINE_ID", "900")
        .env("CI_PROJECT_URL", "https://gitlab.com/acme/shop")
        .env("CI_SERVER_URL", "https://gitlab.com")
        .env("CI_JOB_TOKEN", format!("glcbt-job-token-of-{job}"))
        .env("TMPDIR", temp)
        .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN);
    if let Some(token) = token {
        cmd.env("HUSHRUN_ID_TOKEN", token);
    }
}

/// `hushrun run build -- sh -c SCRIPT` as GitLab job `job`, with `token` as its
/// HUSHRUN_ID_TOKEN, on a runner whose temporary folder is `temp`: SCRIPT's stdout says the
/// wrapper it was given. Its exit code, stdout, stderr, and the close it sent last.
fn gitlab_run(
    m: &Machine,
    fake: &FakeBuild,
    temp: &Path,
    job: &str,
    token: Option<&str>,
    script: &str,
    change: impl FnOnce(&mut assert_cmd::Command),
) -> (
    i32,
    String,
    String,
    Option<hushrun_core::blocks::CloseRequest>,
) {
    let closes = fake.closes().len();
    let mut cmd = m.hushrun(fake);
    in_gitlab_ci(&mut cmd, temp, job, token);
    change(&mut cmd);
    let script = format!("echo \"wrapper=${{RUSTC_WRAPPER:-none}}\"; {script}");
    let output = cmd
        .args(["run", "build", "--force", "--", "sh", "-c", &script])
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        fake.closes().get(closes).cloned(),
    )
}

// covers: B-31, B-63, B-69
// GitLab CI has no RUNNER_TEMP: the bake job init writes there bakes in a store of the run's own
// under the system's temporary folder, removed when it ends, as a run's is.
#[test]
fn a_gitlab_bake_job_bakes_in_a_store_of_its_own() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/gitlab.com/acme/shop"),
        &["protected/gitlab.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let temp = a.root.join("tmp");
    let token = fake.gitlab_token(5101);
    let (code, _, stderr, _) = gitlab_run(
        &a,
        &fake,
        &temp,
        "5101",
        Some(&token),
        "cargo build --offline && sleep 2",
        |_| {},
    );
    assert_eq!(code, 0, "{stderr}");
    let token = fake.gitlab_token(5102);
    let baked = dir.path().join("baked");
    let mut cmd = a.hushrun(&fake);
    in_gitlab_ci(&mut cmd, &temp, "5102", Some(&token));
    let out = cmd
        .args([
            "build",
            "bake",
            "--from",
            "backend",
            "--check",
            "build",
            "--trusted",
        ])
        .args(["--format", "json", "--out"])
        .arg(&baked)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let said: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(said["blocks"], 2, "{said}");
    // The run's store went with the bake.
    assert!(
        named(&temp, "hushrun-build").is_empty(),
        "{:?}",
        named(&temp, "")
    );
    // A store of the command's own holds no run's blocks: `--from store` says so and reads none.
    for command in [
        &["build", "bake", "--out"][..],
        &["build", "manifest", "--json"][..],
    ] {
        let mut cmd = a.hushrun(&fake);
        in_gitlab_ci(&mut cmd, &temp, "5103", None);
        let out = cmd
            .args(command)
            .args(if command[1] == "bake" {
                vec![dir.path().join("from-store").into_os_string()]
            } else {
                Vec::new()
            })
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains("each hushrun command has a store of its own"),
            "{stderr}"
        );
    }
}

/// The build requests the fake has seen since the `after`th request.
fn build_requests(fake: &FakeBuild, after: usize) -> Vec<hushrun_testkit::fake_build::Seen> {
    fake.seen()
        .into_iter()
        .skip(after)
        .filter(|s| s.path.contains("/v0/build/"))
        .collect()
}

/// The entries of `dir` whose names start with `prefix`.
fn named(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect()
}

// covers: B-31, B-48, B-49
#[test]
fn a_gitlab_job_shares_with_its_id_token_and_signs_every_request_with_one_key() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let temp = a.root.join("tmp");
    let token = fake.gitlab_token(5001);
    let (code, stdout, stderr, close) = gitlab_run(
        &a,
        &fake,
        &temp,
        "5001",
        Some(&token),
        "cargo build --offline && sleep 2",
        |_| {},
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    // Every build request carried the job's ID token, never the agent token beside it, and the
    // one key the job's first `open` bound, which signed each.
    let build = build_requests(&fake, 0);
    assert!(!build.is_empty(), "{stderr}");
    let key = build[0].job_key.clone().expect("a job key");
    assert!(key.starts_with("ed25519:"), "{key}");
    for seen in &build {
        assert_eq!(
            seen.authorization.as_deref(),
            Some(format!("Bearer {token}").as_str()),
            "{}",
            seen.path
        );
        assert_eq!(seen.job_key.as_deref(), Some(key.as_str()), "{}", seen.path);
    }
    let job = &fake.gitlab_jobs()[&5001];
    assert!(!job.withdrawn, "{job:?}");
    for route in [route::OPEN, route::BEGIN, route::COMMIT, route::CLOSE] {
        assert!(
            job.signed.get(route).is_some_and(|n| *n >= 1),
            "{route}: {job:?}\n{stderr}"
        );
    }
    assert!(close.is_some(), "{stderr}");
    assert!(!fake.stored().is_empty(), "{stderr}\n{}", a.may_leave());
    // The results lookup went with the agent token, the job's ID token beside it.
    let results: Vec<_> = fake
        .seen()
        .into_iter()
        .filter(|s| s.route == route::RESULTS)
        .collect();
    assert!(!results.is_empty());
    for seen in &results {
        let bearer = format!("Bearer {}", FakeBuild::AGENT_TOKEN);
        assert_eq!(seen.authorization.as_deref(), Some(bearer.as_str()));
        assert_eq!(seen.id_token.as_deref(), Some(token.as_str()));
    }
    // The run's store went with the run, and the job's key is made from its job token, never
    // kept: nothing is left in the temporary folder.
    assert!(
        named(&temp, "hushrun-").is_empty(),
        "{:?}",
        named(&temp, "")
    );
    assert!(!a.root.join("cache/build").exists());

    // The job's next `hushrun run`: the same key, so the job stays bound, and it closes again.
    let before = fake.seen().len();
    let (code, _, stderr, close) = gitlab_run(
        &a,
        &fake,
        &temp,
        "5001",
        Some(&token),
        "cargo clean -q && cargo build --offline",
        |_| {},
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(close.is_some(), "{stderr}");
    let again = build_requests(&fake, before);
    assert!(!again.is_empty());
    assert!(
        again
            .iter()
            .all(|s| s.job_key.as_deref() == Some(key.as_str()))
    );
    let job = &fake.gitlab_jobs()[&5001];
    assert!(!job.withdrawn, "{job:?}");
    assert_eq!(job.signed[route::CLOSE], 2, "{job:?}");
}

// covers: B-31, B-48
#[test]
fn a_gitlab_job_without_its_id_token_reads_nothing_whatever_agent_token_it_holds() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, stdout, stderr, _) = gitlab_run(
        &a,
        &fake,
        &a.root.join("tmp"),
        "5002",
        None,
        "cargo build --offline",
        |_| {},
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    let build: Vec<_> = build_requests(&fake, 0)
        .into_iter()
        .map(|s| s.path)
        .collect();
    assert!(
        build.is_empty(),
        "the agent token read: {build:?}\n{stderr}"
    );
    // Said once, with what to declare.
    let line = format!(
        "build reuse is off for this run: the job can't mint an ID token (declare `id_tokens: {{HUSHRUN_ID_TOKEN: {{aud: {}}}}}` in the job)",
        fake.url()
    );
    assert_eq!(stderr.matches(&line).count(), 1, "{stderr}");
}

// covers: B-48
#[test]
fn a_gitlab_id_token_for_another_audience_goes_to_no_server_and_says_why() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    for (aud, says) in [
        (
            serde_json::json!("https://elsewhere.example/o/acme"),
            "audience is https://elsewhere.example/o/acme",
        ),
        (serde_json::json!([fake.url(), "sigstore"]), "audience is ["),
    ] {
        let token = fake.gitlab_token_with(5003, |c| c["aud"] = aud.clone());
        let before = fake.seen().len();
        let (code, stdout, stderr, _) = gitlab_run(
            &a,
            &fake,
            &a.root.join("tmp"),
            "5003",
            Some(&token),
            "cargo build --offline",
            |_| {},
        );
        assert_eq!(code, 0, "{stderr}");
        assert!(stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
        assert!(stderr.contains(says), "{stderr}");
        assert!(stderr.contains("declare `id_tokens:"), "{stderr}");
        let seen: Vec<_> = fake.seen().into_iter().skip(before).collect();
        assert!(build_requests(&fake, before).is_empty(), "{stderr}");
        for s in &seen {
            assert!(
                s.id_token.is_none()
                    && !s
                        .authorization
                        .as_deref()
                        .unwrap_or_default()
                        .contains(&token),
                "{} carried the token",
                s.path
            );
        }
    }
    // An expired one is said too, and goes nowhere.
    let token = fake.gitlab_token_with(5004, |c| c["exp"] = serde_json::json!(1));
    let before = fake.seen().len();
    let (_, stdout, stderr, _) = gitlab_run(
        &a,
        &fake,
        &a.root.join("tmp"),
        "5004",
        Some(&token),
        "true",
        |_| {},
    );
    assert!(stdout.contains("wrapper=none"), "{stdout}");
    assert!(stderr.contains("HUSHRUN_ID_TOKEN has expired"), "{stderr}");
    assert!(
        fake.seen()
            .iter()
            .skip(before)
            .all(|s| s.id_token.is_none())
    );
}

// covers: B-25, B-48
#[test]
fn a_gitlab_job_whose_org_doesnt_share_pays_no_build_request_and_says_nothing() {
    let fake = FakeBuild::start();
    fake.set_sharing(false);
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let temp = a.root.join("tmp");
    let token = fake.gitlab_token(5005);
    let (code, stdout, stderr, _) =
        gitlab_run(&a, &fake, &temp, "5005", Some(&token), "true", |_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}");
    assert!(build_requests(&fake, 0).is_empty(), "{stderr}");
    assert!(!stderr.contains("hushrun: build reuse"), "{stderr}");
    assert!(!stderr.contains("hushrun: build:"), "{stderr}");
    // Nothing was made for the job: no store, no key.
    assert!(
        named(&temp, "hushrun-").is_empty(),
        "{:?}",
        named(&temp, "")
    );

    // With no token to look results up with, `open` asks once, and the org's no is as silent.
    let (code, stdout, stderr, _) =
        gitlab_run(&a, &fake, &temp, "5005", Some(&token), "true", |cmd| {
            cmd.env_remove("HUSHRUN_TOKEN");
        });
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}");
    let build: Vec<_> = build_requests(&fake, 0);
    assert_eq!(
        build.len(),
        1,
        "{:?}",
        build.iter().map(|s| &s.path).collect::<Vec<_>>()
    );
    assert_eq!(build[0].route, route::OPEN);
    assert!(!stderr.contains("hushrun: build reuse"), "{stderr}");
    assert!(!stderr.contains("hushrun: build:"), "{stderr}");
    assert!(named(&temp, "hushrun-build").is_empty());
}

// covers: B-48, B-49
#[test]
fn a_gitlab_job_with_no_token_but_its_own_shares_once_open_says_the_org_does() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let token = fake.gitlab_token(5006);
    let (code, stdout, stderr, close) = gitlab_run(
        &a,
        &fake,
        &a.root.join("tmp"),
        "5006",
        Some(&token),
        "cargo build --offline && sleep 2",
        |cmd| {
            cmd.env_remove("HUSHRUN_TOKEN");
        },
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    assert!(close.is_some(), "{stderr}");
    assert!(!fake.stored().is_empty(), "{stderr}");
    assert!(!fake.gitlab_jobs()[&5006].withdrawn);
}

// covers: B-49
#[test]
fn a_gitlab_job_closes_its_run_whatever_its_command_does() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let token = fake.gitlab_token(5007);
    let (code, stdout, stderr, close) = gitlab_run(
        &a,
        &fake,
        &a.root.join("tmp"),
        "5007",
        Some(&token),
        "cargo build --offline && sleep 2; exit 3",
        |_| {},
    );
    assert_ne!(code, 0, "{stdout}\n{stderr}");
    assert!(!stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    assert!(close.is_some(), "no close after a failed command: {stderr}");
    assert_eq!(fake.gitlab_jobs()[&5007].signed[route::CLOSE], 1);
    assert!(named(&a.root.join("tmp"), "hushrun-build").is_empty());
}

// covers: B-6, B-24, B-31, B-48
#[test]
fn a_gitlab_job_that_restores_a_doctored_proc_macro_uploads_nothing_built_against_it() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    // Each push's job is a job of its own, with its own token.
    let job = |m: &Machine, id: i64| {
        let token = fake.gitlab_token(id);
        let (code, _, stderr, close) = gitlab_run(
            m,
            &fake,
            &m.root.join("tmp"),
            &id.to_string(),
            Some(&token),
            "CARGO_INCREMENTAL=0 cargo build --offline && sleep 2",
            |_| {},
        );
        assert_eq!(code, 0, "{stderr}");
        (stderr, close)
    };
    let a = Machine::with(dir.path(), "a", None, macro_workspace);
    let (stderr, close) = job(&a, 5100);
    assert_eq!(close.expect(&stderr).counts.built, 2, "{stderr}");
    assert!(!fake.stored().is_empty(), "{stderr}\n{}", a.may_leave());
    // GitLab's cache brings a `target/` whose macro was doctored, and `app` changed.
    poison_macro(&a.repo);
    let c = pushed(dir.path(), "c", &a.repo, Some(&a.repo.join("target")));
    let stored = fake.stored().len();
    let (stderr, close) = job(&c, 5101);
    assert!(stderr.contains("1 crate built, none reused"), "{stderr}");
    assert_eq!(close.expect(&stderr).counts.built, 1, "{stderr}");
    assert_eq!(fake.stored().len(), stored, "it uploaded: {stderr}");
}

// covers: B-49
#[test]
fn every_run_of_a_gitlab_job_signs_with_one_key_whatever_its_temporary_folder() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let token = fake.gitlab_token(5008);
    let run = |temp: &str, change: &dyn Fn(&mut assert_cmd::Command)| {
        gitlab_run(
            &a,
            &fake,
            &a.root.join(temp),
            "5008",
            Some(&token),
            "cargo clean -q && cargo build --offline && sleep 2",
            change,
        )
    };
    let (code, _, stderr, close) = run("tmp", &|_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(close.is_some(), "{stderr}");
    // A step in a container of its own, with another /tmp: the same job, the same key.
    let (code, stdout, stderr, close) = run("tmp-of-a-container", &|_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    assert!(close.is_some(), "{stderr}");
    let job = &fake.gitlab_jobs()[&5008];
    assert!(!job.withdrawn, "an honest job withdrew itself: {stderr}");
    assert_eq!(job.signed[route::CLOSE], 2, "{job:?}");
    // Its ID token with another job's job token, as a thief who copied the ID token alone: another
    // key, which withdraws the job, and every later run of it is refused.
    let (code, stdout, stderr, _) = run("tmp", &|cmd| {
        cmd.env("CI_JOB_TOKEN", "glcbt-someone-else");
    });
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    assert!(fake.gitlab_jobs()[&5008].withdrawn);
    let line = "build: sharing stopped for this run: the backend answered 403 job_withdrawn: a request with another key carried this job's ID token, so the backend withdrew the job: nothing it wrote is served, and its later runs are refused too\n";
    assert_eq!(stderr.matches(line).count(), 1, "{stderr}");
    assert!(!stderr.contains("uploads wait"), "{stderr}");
}

// covers: B-49
#[test]
fn a_gitlab_job_without_its_job_token_sends_nothing_and_says_why() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let token = fake.gitlab_token(5009);
    let (code, stdout, stderr, _) = gitlab_run(
        &a,
        &fake,
        &a.root.join("tmp"),
        "5009",
        Some(&token),
        "cargo build --offline",
        |cmd| {
            cmd.env_remove("CI_JOB_TOKEN");
        },
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}\n{stderr}");
    assert!(build_requests(&fake, 0).is_empty(), "{stderr}");
    assert!(fake.gitlab_jobs().is_empty());
    let line = "build reuse is off for this run: HUSHRUN_ID_TOKEN is set and CI_JOB_TOKEN isn't: the job's key is made from it, and a request with another key would withdraw the job (a step in a container needs both)";
    assert_eq!(stderr.matches(line).count(), 1, "{stderr}");
}

// covers: B-48
#[test]
fn a_gitlab_job_is_known_by_its_variables_also_without_gitlab_ci_or_under_another_provider() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    // A nested container that was handed the job's tokens and GitLab's job variables, not
    // GITLAB_CI or CI; and a job that names another provider.
    type Change<'a> = &'a dyn Fn(&mut assert_cmd::Command);
    let cases: [(&str, i64, Change); 2] = [
        ("nested", 5010, &|cmd| {
            cmd.env_remove("GITLAB_CI").env_remove("CI");
        }),
        ("generic", 5011, &|cmd| {
            cmd.env("HUSHRUN_PROVIDER", "generic");
        }),
    ];
    for (name, job, change) in cases {
        let token = fake.gitlab_token(job);
        let before = fake.seen().len();
        let (code, stdout, stderr, close) = gitlab_run(
            &a,
            &fake,
            &a.root.join(name),
            &job.to_string(),
            Some(&token),
            "cargo clean -q && cargo build --offline && sleep 2",
            change,
        );
        assert_eq!(code, 0, "{name}: {stderr}");
        assert!(
            !stdout.contains("wrapper=none"),
            "{name}: {stdout}\n{stderr}"
        );
        assert!(close.is_some(), "{name}: {stderr}");
        let build = build_requests(&fake, before);
        assert!(!build.is_empty(), "{name}");
        for seen in &build {
            assert_eq!(
                seen.authorization.as_deref(),
                Some(format!("Bearer {token}").as_str()),
                "{name}: {} carried the agent token",
                seen.path
            );
        }
        // Without the job's ID token there, the agent token reads nothing either.
        let before = fake.seen().len();
        let (_, stdout, stderr, _) = gitlab_run(
            &a,
            &fake,
            &a.root.join(name),
            &job.to_string(),
            None,
            "cargo build --offline",
            |cmd| {
                change(cmd);
                cmd.env("HUSHRUN_ID_TOKEN", "");
            },
        );
        let _ = stdout;
        assert!(
            build_requests(&fake, before).iter().all(|s| !s
                .authorization
                .as_deref()
                .unwrap_or_default()
                .contains(FakeBuild::AGENT_TOKEN)),
            "{name}: the agent token read: {stderr}"
        );
    }
}

// covers: B-49
#[test]
fn a_gitlab_job_stopped_by_sigterm_stops_its_command_closes_and_leaves_no_store() {
    use std::process::Stdio;
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let temp = a.root.join("tmp");
    std::fs::create_dir_all(&temp).unwrap();
    let token = fake.gitlab_token(5012);
    // As gitlab_run, but spawned, so the test can stop it mid-COMMAND.
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("hushrun"));
    for var in CLEARED {
        cmd.env_remove(var);
    }
    cmd.current_dir(&a.repo)
        .env("HUSHRUN_CACHE_DIR", a.root.join("cache"))
        .env("HUSHRUN_CREDENTIALS", a.root.join("credentials.json"))
        .env("HUSHRUN_MACHINE_KEY_FILE", a.root.join("machine.pem"))
        .env("HUSHRUN_HOST", "http://127.0.0.1:9")
        .env("HUSHRUN_NO_UPDATE_CHECK", "1")
        .env("HUSHRUN_BACKEND", fake.url())
        .env("HUSHRUN_BUILD", "on")
        .env("HUSHRUN_BUILD_AUDIT", "none")
        .env("CARGO_TERM_COLOR", "never")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", a.root.join("gitconfig"))
        .env("CI", "true")
        .env("GITLAB_CI", "true")
        .env("CI_JOB_ID", "5012")
        .env("CI_PIPELINE_ID", "900")
        .env("CI_PROJECT_URL", "https://gitlab.com/acme/shop")
        .env("CI_SERVER_URL", "https://gitlab.com")
        .env("CI_JOB_TOKEN", "glcbt-job-token-of-5012")
        .env("TMPDIR", &temp)
        .env("HUSHRUN_TOKEN", FakeBuild::AGENT_TOKEN)
        .env("HUSHRUN_ID_TOKEN", &token)
        .args([
            "run",
            "build",
            "--force",
            "--",
            "sh",
            "-c",
            "cargo build --offline && touch built && sleep 20",
        ])
        .stdout(Stdio::null())
        // A file, not a pipe: COMMAND's own children may outlive it, holding a pipe open.
        .stderr(std::fs::File::create(a.root.join("stderr.log")).unwrap());
    let mut child = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !a.repo.join("built").exists() {
        assert!(Instant::now() < deadline, "the build never finished");
        std::thread::sleep(Duration::from_millis(50));
    }
    // GitLab cancels the job: SIGTERM to hushrun alone, then a grace period.
    let stopped = Instant::now();
    let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let status = child.wait().unwrap();
    let took = stopped.elapsed();
    let stderr = std::fs::read_to_string(a.root.join("stderr.log")).unwrap();
    assert!(!status.success(), "{stderr}");
    assert!(took < Duration::from_secs(10), "{took:?}\n{stderr}");
    let job = &fake.gitlab_jobs()[&5012];
    assert_eq!(
        job.signed.get(route::CLOSE),
        Some(&1),
        "no close after SIGTERM: {stderr}"
    );
    assert!(
        named(&temp, "hushrun-build").is_empty(),
        "{:?}",
        named(&temp, "")
    );
}

// covers: B-63, B-64
#[test]
fn a_forged_baked_store_claiming_trust_gets_no_reuse_in_ci() {
    // A laptop bakes its store, and the folder is edited to claim what a trusted bake from the
    // org's backend would: consistent, trusted, protected rows, this repository. No server ever
    // vouched for those blocks, so a CI job reads none of them, whatever the folder says or the
    // job's variables accept, and what it uploads it compiled itself.
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let laptop = Machine::new(dir.path(), "laptop", None);
    let built = laptop
        .hushrun(&fake)
        .env("HUSHRUN_BACKEND", "")
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
    assert!(built.status.success(), "{built:?}");
    let baked = dir.path().join("baked");
    let bake = laptop
        .hushrun(&fake)
        .env("HUSHRUN_BACKEND", "")
        .args(["build", "bake", "--check", "build", "--out"])
        .arg(&baked)
        .output()
        .unwrap();
    assert!(bake.status.success(), "{bake:?}");
    let path = baked.join("baked.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(!json["packs"].as_array().unwrap().is_empty());
    json["trust"] = serde_json::json!({"level": "trusted"});
    json["source"] = serde_json::json!({"from": "backend", "host": fake.origin(), "org": "acme"});
    json["repository"] = "github.com/acme/shop".into();
    for row in json["packs"].as_array_mut().unwrap() {
        row["scope"] = "protected/github.com/acme/shop".into();
    }
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

    let job = Machine::new(dir.path(), "job", Some(&laptop.repo));
    // The same checkout path as the laptop's is no matter here: these crates are portable.
    let (code, _, stderr) = ci_build(&job, &fake, |cmd| {
        cmd.env("HUSHRUN_BUILD_LOWER", &baked)
            .env("HUSHRUN_BUILD_LOWER_UNTRUSTED", "1");
    });
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("from the baked store"), "{stderr}");
    assert!(stderr.contains("2 crates built, none reused"), "{stderr}");
}

// covers: B-63, B-64
#[test]
fn a_store_baked_from_the_backend_is_reused_in_ci_block_by_block_as_the_backend_vouches() {
    // CI on the default branch builds and uploads to the repository's trusted scope; a bake
    // from the backend gathers those packs; another job reads them underneath its own store,
    // each one only as the backend vouches for it as the job begins.
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, _, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(!fake.stored().is_empty(), "{stderr}");
    let baked = dir.path().join("baked");
    let mut bake = a.hushrun(&fake);
    in_github_actions(&mut bake, &fake, &a.root.join("runner-temp-bake"));
    let bake = bake
        .args([
            "build",
            "bake",
            "--from",
            "backend",
            "--check",
            "build",
            "--trusted",
        ])
        .args(["--format", "json", "--out"])
        .arg(&baked)
        .output()
        .unwrap();
    let said: serde_json::Value = serde_json::from_slice(&bake.stdout).unwrap_or_default();
    assert!(bake.status.success(), "{bake:?}");
    assert_eq!(said["trust"]["level"], "trusted", "{said}");
    assert_eq!(said["blocks"], 2, "{said}");

    let b = Machine::new(dir.path(), "b", Some(&a.repo));
    let (code, _, stderr) = ci_build(&b, &fake, |cmd| {
        cmd.env("HUSHRUN_BUILD_LOWER", &baked);
    });
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("(2 from the baked store)"), "{stderr}");
}

/// [`workspace`] with a program beside `app`'s library: `app/src/main.rs`.
fn program_workspace(root: &Path) -> PathBuf {
    let repo = workspace(root);
    std::fs::write(
        repo.join("app/src/main.rs"),
        "fn main() {\n    println!(\"{}\", app::four());\n}\n",
    )
    .unwrap();
    git::commit_all(&repo, "a program");
    repo
}

// covers: B-63, B-30
// CI builds a program too, and uploads it: a bake from the backend downloads every block the
// manifest names, programs too (a run asks for a program only when a compile does, B-30; a bake
// is the one that asks), so the store is whole, not partial.
#[test]
fn a_bake_from_the_backend_downloads_the_programs_its_manifest_names() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::with(dir.path(), "a", None, program_workspace);
    let (code, _, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    let stored = fake.stored().len();
    assert!(stored >= 3, "the program went up too: {stderr}");
    let baked = dir.path().join("baked");
    let mut bake = a.hushrun(&fake);
    in_github_actions(&mut bake, &fake, &a.root.join("runner-temp-bake"));
    let bake = bake
        .args([
            "build",
            "bake",
            "--from",
            "backend",
            "--check",
            "build",
            "--trusted",
        ])
        .args(["--format", "json", "--out"])
        .arg(&baked)
        .output()
        .unwrap();
    let said: serde_json::Value = serde_json::from_slice(&bake.stdout).unwrap_or_default();
    assert!(bake.status.success(), "{bake:?}");
    assert_eq!(said["missing"], 0, "{said}");
    assert_eq!(said["partial"], false, "{said}");
    assert_eq!(said["blocks"], stored, "{said}");
}

// covers: B-30, B-31
// A CI job's uploads are cut off at the end of its command (a quarter of what its compiles took):
// its manifest names only blocks the server confirmed stored, so a bake from it is whole; the
// blocks it couldn't upload stay in the outbox, and the next run in that store uploads them.
#[test]
fn a_ci_job_s_manifest_names_only_the_blocks_the_server_stored() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    // Every upload's begin hangs past the end's wait.
    fake.delay(
        hushrun_testkit::fake_build::route::BEGIN,
        std::time::Duration::from_secs(60),
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, _, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    assert!(fake.stored().is_empty(), "{stderr}");
    let stored: std::collections::HashSet<_> = fake.stored().iter().map(|b| b.address).collect();
    let close = fake
        .closes()
        .into_iter()
        .rfind(|c| c.kind == "rustc/1")
        .expect("a close");
    for pair in &close.manifest {
        assert!(
            stored.contains(&pair.1),
            "the manifest names {} the server never stored: {stderr}",
            pair.1
        );
    }
}

// covers: B-70
// A partial store's image would take the backend registry's fixed tag for good: refused before
// anything is signed (a keyless signature writes a public timestamp), so the folder holds no
// signature and nothing was published.
#[test]
fn a_partial_store_is_refused_for_the_backend_s_registry_before_anything_is_signed() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let (code, _, stderr) = ci_build(&a, &fake, |_| {});
    assert_eq!(code, 0, "{stderr}");
    let gone = fake.stored()[0].address;
    fake.withdraw(gone);
    let key = a.root.join("signing.pem");
    let made = a
        .hushrun(&fake)
        .args(["keys", "generate", "--algorithm", "ecdsa-p256", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    let out = dir.path().join("out");
    let mut cmd = a.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp-bake"));
    on_the_default_branch(&mut cmd, &a.root);
    let bake = cmd
        .args([
            "build",
            "bake",
            "--from",
            "backend",
            "--check",
            "build",
            "--trusted",
            "--oci",
            "hushrun",
            "--out",
        ])
        .arg(&out)
        .arg("--signing-key")
        .arg(&key)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&bake.stderr);
    assert_eq!(bake.status.code(), Some(11), "{stderr}");
    assert!(stderr.contains("nothing was signed"), "{stderr}");
    assert!(
        !out.join("baked.sigstore.json").exists(),
        "signed before refusing"
    );
    assert!(fake.publishes().is_empty());
}

// covers: B-75, B-69
// A merge whose CI skipped (the pull request passed this exact code) compiles nothing on the
// default branch, so the backend's newest build is the commit before. The bake, in CI on the
// default branch, in a checkout of its own, sees its manifest is of another commit and builds what
// no trusted store holds as CI's own file runs it: the command init wrapped there
// (`hushrun run test -- cargo test`, with no `run` in hushrun.yaml) and the variables CI's file
// gives every step (here `CARGO_INCREMENTAL: 0`, which the bake job's own environment doesn't
// have: without it no key would match CI's). It bakes the merged code's blocks and records no
// result. With --no-build it bakes the older build.
#[test]
fn a_merge_whose_ci_skipped_is_built_by_the_bake_before_it_bakes() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::create_dir_all(a.repo.join(".github/workflows")).unwrap();
    std::fs::write(
        a.repo.join(".github/workflows/ci.yml"),
        "name: CI\non:\n  push:\n    branches: [main]\nenv:\n  CARGO_INCREMENTAL: \"0\"\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - run: cargo test --offline\n",
    )
    .unwrap();
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "ci");
    // init wraps CI's command: its plan's edit of ci.yml, applied as it is written.
    let plan = a
        .hushrun(&fake)
        .env("HOME", &a.root)
        .args(["init", "--dry-run", "--no-browser", "--format", "json"])
        .output()
        .unwrap();
    assert!(plan.status.success(), "{plan:?}");
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let diff = plan["edits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == ".github/workflows/ci.yml")
        .and_then(|e| e["diff"].as_str())
        .unwrap_or_else(|| panic!("init edits no CI file: {plan}"))
        .to_string();
    std::fs::write(a.root.join("ci.diff"), &diff).unwrap();
    git::run(
        &a.repo,
        &["apply", a.root.join("ci.diff").to_str().unwrap()],
        None,
    );
    let ci = std::fs::read_to_string(a.repo.join(".github/workflows/ci.yml")).unwrap();
    assert!(
        ci.contains("hushrun run test -- cargo test --offline"),
        "{ci}"
    );
    git::commit_all(&a.repo, "init");
    // CI on the default branch, as its file runs it.
    let mut ci_run = a.hushrun(&fake);
    in_github_actions(&mut ci_run, &fake, &a.root.join("runner-temp"));
    on_the_default_branch(&mut ci_run, &a.root);
    let ci_run = ci_run
        .env("CARGO_INCREMENTAL", "0")
        .args(["run", "test", "--force", "--", "cargo", "test", "--offline"])
        .output()
        .unwrap();
    assert!(ci_run.status.success(), "{ci_run:?}");
    // B-31: CI's command ends before its last uploads when its compiles were this cheap (a
    // quarter of what they took), and which block it leaves in its job's outbox is whichever
    // compile finished last: dep's test program on some runs. What CI's job built counts as
    // before the merge whether it went up or not, by the keys its compiles logged; one it left
    // the bake builds again, under the same address.
    let mut before: std::collections::HashSet<_> =
        fake.stored().iter().map(|b| b.address).collect();
    before.extend(built_by(&a));
    // The merge: app changes, and CI skips.
    std::fs::write(
        a.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 40\n}\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "the merge");
    let reports = fake.reports().len();
    // The bake job's checkout: CI's paths (a runner's are the same in every job), fresh.
    std::fs::remove_dir_all(a.repo.join("target")).unwrap();
    let job = &a;
    let bake = |n: u32, extra: &[&str]| {
        let mut cmd = job.hushrun(&fake);
        in_github_actions(
            &mut cmd,
            &fake,
            &job.root.join(format!("runner-temp-bake-{n}")),
        );
        on_the_default_branch(&mut cmd, &job.root);
        let out = cmd
            .args([
                "build",
                "bake",
                "--from",
                "backend",
                "--trusted",
                "--format",
                "json",
                "--out",
            ])
            .arg(dir.path().join(format!("out-{n}")))
            .args(extra)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "{:?} {stderr} {}",
            out.status,
            String::from_utf8_lossy(&out.stdout)
        );
        (
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
            stderr,
        )
    };
    let rows = |n: u32| -> std::collections::HashSet<String> {
        let json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(format!("out-{n}/baked.json"))).unwrap(),
        )
        .unwrap();
        json["packs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["address"].as_str().unwrap().to_string())
            .collect()
    };
    // --no-build: the older build, as before.
    let (_, stderr) = bake(1, &["--no-build"]);
    assert!(!stderr.contains("building what"), "{stderr}");
    let old_rows = rows(1);
    // The bake builds what's missing first, as CI's file says: the new app blocks went up.
    let (said, stderr) = bake(2, &[]);
    assert!(stderr.contains("its newest build is of"), "{stderr}");
    assert!(
        stderr.contains("building what no trusted store holds with `test`, as CI builds them"),
        "{stderr}"
    );
    assert!(stderr.contains("at most 30 minutes"), "{stderr}");
    assert!(said["built_missing"]["why"].is_string(), "{said}");
    let new: Vec<_> = fake
        .stored()
        .iter()
        .map(|b| b.address)
        .filter(|a| !before.contains(a))
        .collect();
    // dep's library came from CI's build (its key matches only with CI's CARGO_INCREMENTAL), and
    // its test program too when CI's job sent it and it was downloaded before cargo asked (bound
    // to CI's folders); app's library and test program compiled, and went up.
    assert_eq!(new.len(), 2, "only app was compiled: {stderr}");
    let reused: u32 = stderr
        .split("check `test`: built in ")
        .nth(1)
        .and_then(|rest| rest.split_once("s ("))
        .and_then(|(_, rest)| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    assert!(
        (1..=2).contains(&reused) && stderr.contains(" of 4 compiles reused"),
        "{stderr}"
    );
    let new_rows = rows(2);
    for block in &new {
        assert!(new_rows.contains(&block.to_string()), "{said}");
        assert!(!old_rows.contains(&block.to_string()));
    }
    assert_eq!(said["missing"], 0, "{said}");
    // B-76: CI baked its trusted scope, so the store names the job's folders, which blocks bound
    // to them need.
    let baked: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("out-2/baked.json")).unwrap())
            .unwrap();
    let home = std::env::var("HOME").unwrap();
    assert_eq!(baked["paths"]["home"], home.as_str(), "{baked}");
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{home}/.cargo"));
    assert_eq!(baked["paths"]["cargo_home"], cargo_home.as_str(), "{baked}");
    assert_eq!(
        baked["paths"]["checkout"],
        job.repo.canonicalize().unwrap().to_str().unwrap()
    );
    // It recorded no result, its own or the pull request's, and left no target folder behind.
    assert_eq!(fake.reports().len(), reports);
    assert!(!job.repo.join("target").exists());
    // Now the backend's newest is of HEAD: a second bake builds nothing.
    let (_, stderr) = bake(3, &[]);
    assert!(stderr.contains("nothing to build"), "{stderr}");
}

/// The addresses of the blocks the machine's runs so far compiled or reused, from its key log:
/// what a run built is known here even when the end of its command cut off the upload (B-31).
fn built_by(m: &Machine) -> Vec<hushrun_core::blocks::Hex32> {
    let log = std::fs::read_to_string(m.root.join("keys.jsonl")).unwrap_or_default();
    log.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|line| line["keys"]["block"].as_str().map(str::to_string))
        .map(|key| {
            let key = hushrun_core::Digest::parse(&key).unwrap();
            hushrun_core::blocks::address("rustc/1", key.as_bytes())
        })
        .collect()
}

/// A GitHub Actions job on the default branch, as a bake's: a push to `main`, whose event names
/// `main` the default branch.
fn on_the_default_branch(cmd: &mut assert_cmd::Command, root: &Path) {
    let event = root.join("event.json");
    std::fs::write(&event, r#"{"repository": {"default_branch": "main"}}"#).unwrap();
    cmd.env("GITHUB_REF", "refs/heads/main")
        .env("GITHUB_EVENT_NAME", "push")
        .env("GITHUB_EVENT_PATH", &event);
}

// covers: B-65, B-66, B-67
#[test]
fn an_image_baked_from_the_backend_carries_only_the_blocks_the_backend_still_vouches_for() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let key = a.root.join("signing.pem");
    let made = a
        .hushrun(&fake)
        .args(["keys", "generate", "--algorithm", "ecdsa-p256", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    let public = a
        .hushrun(&fake)
        .args(["keys", "public", "--key"])
        .arg(&key)
        .output()
        .unwrap();
    let public = String::from_utf8_lossy(&public.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("public key: ").map(str::to_string))
        .unwrap();
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        format!("version: 1\nrepository: github.com/acme/shop\nbuild:\n  baked:\n    signers:\n      - key: {public}\n"),
    )
    .unwrap();
    git::commit_all(&a.repo, "signers");
    let layout = dir.path().join("layout");
    let bake = |n: u32| {
        let mut cmd = a.hushrun(&fake);
        in_github_actions(
            &mut cmd,
            &fake,
            &a.root.join(format!("runner-temp-bake-{n}")),
        );
        on_the_default_branch(&mut cmd, &a.root);
        let out = cmd
            .args([
                "build",
                "bake",
                "--from",
                "backend",
                "--check",
                "build",
                "--trusted",
            ])
            .args(["--format", "json", "--oci-dir"])
            .arg(&layout)
            .arg("--out")
            .arg(dir.path().join(format!("out-{n}")))
            .arg("--signing-key")
            .arg(&key)
            // Bakes seconds apart: past the re-bake limit (B-73), which its own test exercises.
            .arg("--force")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let rows = |n: u32| -> Vec<String> {
        let json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(format!("out-{n}/baked.json"))).unwrap(),
        )
        .unwrap();
        json["packs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["address"].as_str().unwrap().to_string())
            .collect()
    };
    let change = |n: u32| {
        std::fs::write(
            a.repo.join("app/src/lib.rs"),
            format!("pub fn four() -> u32 {{\n    dep::two() * 2 + {n}\n}}\n"),
        )
        .unwrap();
        let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
            on_the_default_branch(cmd, &a.root);
            // A fresh target folder, as a CI job has: every crate's compile is logged, reused
            // or built, so the run's manifest names the whole build.
            cmd.env("GITHUB_RUN_ID", format!("{}", 100 + n))
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_TARGET_DIR", a.root.join(format!("target-{n}")));
        });
        assert_eq!(code, 0, "{stderr}");
    };
    let uploaded = |said: &serde_json::Value| -> Vec<String> {
        said["image"]["blobs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["uploaded"] == true)
            .map(|b| b["role"].as_str().unwrap().to_string())
            .collect()
    };
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
        // Not incremental: a workspace crate's compile is then one a store keeps (B-4).
        cmd.env("CARGO_INCREMENTAL", "0");
    });
    assert_eq!(code, 0, "{stderr}");

    // CI on the default branch: the moving tag moves.
    let first = bake(1);
    let tags: Vec<&str> = first["image"]["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(tags.iter().any(|t| t.ends_with("-latest")), "{first}");
    let first_app: Vec<String> = first["image"]["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["role"].as_str().unwrap().to_string())
        .collect();
    assert!(first_app.contains(&"workspace".to_string()), "{first}");

    // A code change: the previous image is signed by the key hushrun.yaml names, and the backend
    // still vouches for the block the workspace layer keeps unused: carried, only the recent
    // layer goes up.
    change(1);
    let second = bake(2);
    assert!(second["image"]["compacted"].is_null(), "{second}");
    assert_eq!(second["image"]["stale"], 1, "{second}");
    let blobs = uploaded(&second);
    assert!(!blobs.iter().any(|b| b == "workspace"), "{blobs:?}");

    // The backend withdraws the block the workspace layer keeps unused: never carried again;
    // the workspace layer is written again.
    let now_used = rows(2);
    let stale: Vec<String> = rows(1)
        .into_iter()
        .filter(|address| !now_used.contains(address))
        .collect();
    assert_eq!(stale.len(), 1, "{stale:?}");
    let stale = fake
        .stored()
        .iter()
        .map(|b| b.address)
        .find(|address| address.to_string() == stale[0])
        .unwrap();
    fake.withdraw(stale);
    change(2);
    let third = bake(3);
    assert!(
        third["image"]["compacted"]
            .as_str()
            .unwrap_or_default()
            .contains("no longer vouches"),
        "{third}"
    );

    // B-68: `build manifest --from backend` names the identity, and so the fixed tag, the bake
    // published: the same parts, from the same function.
    let mut cmd = a.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp-manifest"));
    on_the_default_branch(&mut cmd, &a.root);
    let out = cmd
        .args([
            "build", "manifest", "--from", "backend", "--check", "build", "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let named: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(named["manifest"], third["manifest"], "{named}");
    assert_eq!(
        named["tags"]["fixed"].as_str(),
        third["image"]["tags"][0].as_str(),
        "{named}"
    );
}

// covers: B-63
// The person's say-so is decided on the run's authority, not on CI alone: a person whose backend
// lets them write the repository's trusted scope accepts no unverified store, since what they
// write others trust; one who writes only their own scope may.
#[test]
fn a_person_who_writes_the_trusted_scope_accepts_no_unverified_baked_store() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let laptop = Machine::new(dir.path(), "laptop", None);
    let built = laptop
        .hushrun(&fake)
        .env("HUSHRUN_BACKEND", "")
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
    assert!(built.status.success(), "{built:?}");
    let baked = dir.path().join("baked");
    let bake = laptop
        .hushrun(&fake)
        .env("HUSHRUN_BACKEND", "")
        .args(["build", "bake", "--check", "build", "--out"])
        .arg(&baked)
        .output()
        .unwrap();
    assert!(bake.status.success(), "{bake:?}");
    let run = |name: &str, write: &str| {
        fake.set_scopes(Some(write), &[write]);
        let person = Machine::new(dir.path(), name, Some(&laptop.repo));
        let out = person
            .hushrun(&fake)
            .env("HUSHRUN_BUILD_LOWER", &baked)
            .env("HUSHRUN_BUILD_LOWER_UNTRUSTED", "1")
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
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let trusted = run("maintainer", "protected/github.com/acme/shop");
    assert!(!trusted.contains("from the baked store"), "{trusted}");
    let own = run("contributor", "person/1");
    assert!(own.contains("from the baked store"), "{own}");
}

// covers: B-31, B-75
// On GitLab (no RUNNER_TEMP) a bake's store is a store of the command's own. The bake's build
// begins its sessions there, and their end used to remove that very store (lock, packs,
// downloads) while the bake still needed it: the second check reused nothing, and an image with
// no --out lost the folder being baked. The command's guard holds the store now.
#[test]
fn a_gitlab_bake_that_builds_keeps_its_store() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/gitlab.com/acme/shop"),
        &["protected/gitlab.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: gitlab.com/acme/shop\nchecks:\n  build:\n    run: cargo build --offline\n  zlist:\n    run: S=\"$(dirname $(dirname $HUSHRUN_BUILD_SESSION))\"; ls \"$S\" >&2; test -d \"$S/tmp\" || echo STORE-GONE >&2; cargo build --offline\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "checks");
    let temp = a.root.join("tmp");
    let token = fake.gitlab_token(6101);
    let (code, _, stderr, _) = gitlab_run(
        &a,
        &fake,
        &temp,
        "6101",
        Some(&token),
        "cargo build --offline",
        |cmd| {
            cmd.env("CI_COMMIT_BRANCH", "main")
                .env("CI_DEFAULT_BRANCH", "main")
                .env("CI_PIPELINE_SOURCE", "push");
        },
    );
    assert_eq!(code, 0, "{stderr}");
    std::fs::write(
        a.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 40\n}\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "the merge");
    let token = fake.gitlab_token(6102);
    let baked = dir.path().join("baked");
    let mut cmd = a.hushrun(&fake);
    in_gitlab_ci(&mut cmd, &temp, "6102", Some(&token));
    cmd.env("CI_COMMIT_BRANCH", "main")
        .env("CI_DEFAULT_BRANCH", "main")
        .env("CI_PIPELINE_SOURCE", "push");
    let out = cmd
        .args(["build", "bake", "--from", "backend", "--trusted"])
        .args(["--format", "json", "--out"])
        .arg(&baked)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    let said: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let lines: Vec<String> = said["built_missing"]["lines"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|l| l.as_str().map(str::to_string))
        .filter(|l| l.starts_with("check `"))
        .collect();
    assert_eq!(lines.len(), 2, "{said}");
    assert!(lines.iter().all(|l| l.contains(": built")), "{said}");
    assert!(
        !stderr.contains("STORE-GONE"),
        "the bake's store was removed after the first check's build: {stderr}"
    );
    // The first check reused what CI's build stored (the bake's checkout has CI's target
    // folder, so it built into a fresh one), and the second shares its target, as CI's
    // checks do: nothing to compile again.
    assert!(lines[0].contains("1 of 2 compiles reused"), "{said}");
    assert!(lines[1].contains("0 of 0 compiles reused"), "{said}");
    assert_eq!(said["missing"], 0, "{said}");
}

// covers: B-75
// A B-75 build that fails part way (a compile error, a missing service before the
// last compile, the 30-minute stop) still closes a manifest naming HEAD, so the backend's newest
// build "is of HEAD": the bake bakes that partial manifest, and every later bake says "nothing to
// build" and bakes it again, where --no-build would have baked the last whole build.
#[test]
fn a_failed_build_in_the_bake_leaves_no_partial_manifest_of_head() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\nchecks:\n  build:\n    run: cargo build --offline\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "checks");
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
        cmd.env("CARGO_INCREMENTAL", "0");
    });
    assert_eq!(code, 0, "{stderr}");
    // The merge, whose build in the bake stops after `dep` (as a stop at 30 minutes would).
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\nchecks:\n  build:\n    run: cargo build --offline -p dep && exit 1\n",
    )
    .unwrap();
    std::fs::write(
        a.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 40\n}\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "the merge");
    let bake = |n: u32, extra: &[&str]| {
        let mut cmd = a.hushrun(&fake);
        in_github_actions(
            &mut cmd,
            &fake,
            &a.root.join(format!("runner-temp-bake-{n}")),
        );
        on_the_default_branch(&mut cmd, &a.root);
        let out = cmd
            .args([
                "build",
                "bake",
                "--from",
                "backend",
                "--check",
                "build",
                "--trusted",
                "--format",
                "json",
                "--out",
            ])
            .arg(dir.path().join(format!("out-{n}")))
            .args(extra)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{stderr}");
        (
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
            stderr,
        )
    };
    let (no_build, _) = bake(1, &["--no-build"]);
    let (built, stderr2) = bake(2, &[]);
    let (_, stderr3) = bake(3, &[]);
    assert!(
        stderr2.contains("no manifest was closed for this commit"),
        "{stderr2}"
    );
    // What the failed build leaves is never less than the whole build --no-build bakes, and a
    // later bake builds again rather than calling a partial manifest current.
    assert!(
        built["blocks"].as_u64() >= no_build["blocks"].as_u64(),
        "{built}"
    );
    assert!(!stderr3.contains("nothing to build"), "{stderr3}");
}

// covers: B-75
// The bake's build runs code the repository chose (build scripts, proc macros, tests, copied
// tools) in the job that holds the signing identity: it gets none of the job's credentials (its
// ID token request, GITHUB_TOKEN, hushrun's token), looks up and records no result of its own
// (HUSHRUN_BACKEND empty, and it knows it runs inside a run), and on Linux, where the bake is its
// subreaper, nothing it left running outlives it.
#[test]
fn a_bake_s_build_gets_no_credential_and_leaves_no_process_behind() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let out = a.root.join("seen");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\nchecks:\n  build:\n    run: env > \"$SEEN/env.txt\"; (sleep 300 >/dev/null 2>&1 & echo $! > \"$SEEN/pid\"); cargo build --offline\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "checks");
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
        cmd.env("CARGO_INCREMENTAL", "0");
    });
    assert_eq!(code, 0, "{stderr}");
    std::fs::write(
        a.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 40\n}\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "the merge");
    std::fs::remove_dir_all(a.repo.join("target")).unwrap();
    let mut cmd = a.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp-bake"));
    on_the_default_branch(&mut cmd, &a.root);
    let baked = cmd
        .env("SEEN", &out)
        .env("GITHUB_TOKEN", "ghs_the_jobs_own_token")
        .env("CARGO_INCREMENTAL", "0")
        .args(["build", "bake", "--from", "backend", "--trusted", "--out"])
        .arg(dir.path().join("out"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&baked.stderr).into_owned();
    assert!(baked.status.success(), "{stderr}");
    assert!(stderr.contains("check `build`: built"), "{stderr}");
    let env = std::fs::read_to_string(out.join("env.txt")).unwrap();
    for secret in [
        FakeBuild::ID_TOKEN_REQUEST,
        FakeBuild::AGENT_TOKEN,
        "ghs_the_jobs_own_token",
    ] {
        assert!(!env.contains(secret), "the build saw {secret}:\n{env}");
    }
    for name in [
        "ACTIONS_ID_TOKEN_REQUEST_URL=",
        "HUSHRUN_TOKEN=",
        "GITHUB_TOKEN=",
    ] {
        assert!(!env.lines().any(|l| l.starts_with(name)), "{name}\n{env}");
    }
    assert!(env.lines().any(|l| l == "HUSHRUN_BACKEND="), "{env}");
    assert!(env.lines().any(|l| l == "HUSHRUN_INSIDE_RUN=1"), "{env}");
    let pid = std::fs::read_to_string(out.join("pid")).unwrap();
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .is_ok_and(|s| s.success())
    };
    if cfg!(target_os = "linux") {
        assert!(!alive(), "the build's background process outlived the bake");
    } else {
        let _ = std::process::Command::new("kill").arg(pid.trim()).status();
    }
}

// covers: B-75
// A bake's build whose blocks didn't all go up (the object store refused them) closes no
// manifest for the commit: the backend's newest stays the last whole build, and the next bake
// builds again, where a manifest of the blocks that went up would have been taken for the
// commit's build for good.
#[test]
fn a_bake_s_build_whose_blocks_didn_t_all_go_up_closes_no_manifest() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\nchecks:\n  build:\n    run: cargo build --offline\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "checks");
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
        cmd.env("CARGO_INCREMENTAL", "0");
    });
    assert_eq!(code, 0, "{stderr}");
    std::fs::write(
        a.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 40\n}\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "the merge");
    let closes = fake.closes().len();
    fake.fail(hushrun_testkit::fake_build::route::STORE_PUT, 500, 1000);
    let bake = |n: u32| {
        let _ = std::fs::remove_dir_all(a.repo.join("target"));
        let mut cmd = a.hushrun(&fake);
        in_github_actions(
            &mut cmd,
            &fake,
            &a.root.join(format!("runner-temp-bake-{n}")),
        );
        on_the_default_branch(&mut cmd, &a.root);
        let baked = cmd
            .env("CARGO_INCREMENTAL", "0")
            .args(["build", "bake", "--from", "backend", "--trusted", "--out"])
            .arg(dir.path().join(format!("out-{n}")))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&baked.stderr).into_owned();
        assert!(baked.status.success(), "{stderr}");
        stderr
    };
    let started = std::time::Instant::now();
    let first = bake(1);
    assert!(
        first.contains("no manifest was closed for this commit"),
        "{first}"
    );
    assert!(first.contains("wasn't uploaded"), "{first}");
    assert_eq!(fake.closes().len(), closes, "a part build was closed");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(120),
        "{:?}",
        started.elapsed()
    );
    // The next bake builds again.
    let second = bake(2);
    assert!(
        second.contains("building what no trusted store holds"),
        "{second}"
    );
}

// covers: B-31, B-72
// `--deps-only` builds in a store of the bake's own and bakes that one, and init's plan names it
// for a job on the default branch. The refusal of `--from store` in a CI command's own store
// (GitLab, no RUNNER_TEMP) is for a bake that reads that store: `--deps-only`, whose `--from` is
// `store` by default, reads its own build's, and a GitLab job bakes it.
#[test]
fn a_gitlab_deps_only_bake_is_not_refused_for_its_command_s_store() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let lock = std::process::Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&a.repo)
        .output()
        .unwrap();
    assert!(lock.status.success(), "{lock:?}");
    git::commit_all(&a.repo, "lockfile");
    let temp = a.root.join("tmp");
    let mut cmd = a.hushrun(&fake);
    in_gitlab_ci(&mut cmd, &temp, "7101", None);
    cmd.env("CI_COMMIT_BRANCH", "main")
        .env("CI_DEFAULT_BRANCH", "main")
        .env("CI_PIPELINE_SOURCE", "push");
    let out = cmd
        .args(["build", "bake", "--deps-only", "--out"])
        .arg(dir.path().join("deps"))
        .args(["--", "cargo", "build", "--offline"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("each hushrun command has a store of its own"),
        "{stderr}"
    );
}

// covers: B-75
// The bake asks for every check CI's files wrap. A check that compiles nothing (a linter, a JS
// test, a docs check) closes an empty manifest of the commit its run named, which counts as its
// build of that commit: at the very commit CI built, a bake builds nothing, where it used to run
// every check's command (tests included, up to 30 minutes, in the job that signs). Only the
// checks that are behind are built, and the line names them alone.
#[test]
fn a_check_that_compiles_nothing_doesnt_make_every_bake_build() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\nchecks:\n  build:\n    run: cargo build --offline\n  lint:\n    run: \"true\"\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "checks");
    // CI on the default branch, both checks, at this commit.
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
        cmd.env("CARGO_INCREMENTAL", "0");
    });
    assert_eq!(code, 0, "{stderr}");
    let mut lint = a.hushrun(&fake);
    in_github_actions(&mut lint, &fake, &a.root.join("runner-temp"));
    on_the_default_branch(&mut lint, &a.root);
    let lint = lint
        .args(["run", "lint", "--force", "--", "true"])
        .output()
        .unwrap();
    assert!(lint.status.success(), "{lint:?}");
    // The bake, at the commit CI built: nothing is missing.
    let mut cmd = a.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp-bake"));
    on_the_default_branch(&mut cmd, &a.root);
    let out = cmd
        .args(["build", "bake", "--from", "backend", "--trusted"])
        .args(["--format", "json", "--out"])
        .arg(dir.path().join("out"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    assert!(
        !stderr.contains("building what no trusted store holds"),
        "a bake at the commit CI built ran every check again: {stderr}"
    );
    assert!(stderr.contains("nothing to build"), "{stderr}");
    // A merge whose `build` CI skipped and whose `lint` ran: only `build` is behind, built, and
    // named.
    std::fs::write(
        a.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 40\n}\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "the merge");
    let mut lint = a.hushrun(&fake);
    in_github_actions(&mut lint, &fake, &a.root.join("runner-temp-lint"));
    on_the_default_branch(&mut lint, &a.root);
    let lint = lint
        .args(["run", "lint", "--force", "--", "true"])
        .output()
        .unwrap();
    assert!(lint.status.success(), "{lint:?}");
    let _ = std::fs::remove_dir_all(a.repo.join("target"));
    let mut cmd = a.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp-bake-2"));
    on_the_default_branch(&mut cmd, &a.root);
    let out = cmd
        .env("CARGO_INCREMENTAL", "0")
        .args(["build", "bake", "--from", "backend", "--trusted", "--out"])
        .arg(dir.path().join("out-2"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("`build`: its newest build is of"),
        "{stderr}"
    );
    assert!(
        stderr.contains("building what no trusted store holds with `build`, as CI builds them"),
        "{stderr}"
    );
    assert!(!stderr.contains("`lint`"), "{stderr}");
    assert!(!stderr.contains("check `lint`"), "{stderr}");
}

// covers: B-72, B-74
// ADR 0102 decision 5 keeps every credential of the signing job from what a bake's build runs,
// on every path that runs one: `--deps-only` (which init's plan names for a job on the default
// branch, signed keylessly as that job) and `--report`'s builds go through the same function as
// the build of what's missing. Third-party build scripts and proc macros get neither the ID token
// request, GITHUB_TOKEN nor hushrun's token, record no result, and leave nothing running.
#[test]
fn a_deps_only_build_in_ci_gets_no_credential() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let lock = std::process::Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&a.repo)
        .output()
        .unwrap();
    assert!(lock.status.success(), "{lock:?}");
    git::commit_all(&a.repo, "lockfile");
    let seen = a.root.join("seen");
    std::fs::create_dir_all(&seen).unwrap();
    // Each build writes what it was handed, and leaves a process running.
    let script = "env > \"$SEEN/env-$$.txt\"; (sleep 300 >/dev/null 2>&1 & echo $! > \"$SEEN/pid-$$\"); cargo build --offline";
    let bake = |extra: &[&str], out: &str| {
        let mut cmd = a.hushrun(&fake);
        in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp"));
        on_the_default_branch(&mut cmd, &a.root);
        cmd.env("GITHUB_TOKEN", "ghs_the_jobs_own_token")
            .env("SEEN", &seen)
            .args(["build", "bake"])
            .args(extra)
            .arg("--out")
            .arg(dir.path().join(out))
            .args(["--", "sh", "-c", script])
            .output()
            .unwrap()
    };
    // The dependencies-only build (this repository has no third-party crate, so it bakes
    // nothing; its build ran all the same).
    let _ = bake(&["--deps-only"], "deps");
    // `--report`'s two builds, cold and from the baked store, of a store a run here left.
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
    });
    assert_eq!(code, 0, "{stderr}");
    let reported = bake(&["--report"], "report");
    let stderr = String::from_utf8_lossy(&reported.stderr);
    assert!(reported.status.success(), "{stderr}");
    let mut envs = Vec::new();
    let mut pids = Vec::new();
    for entry in std::fs::read_dir(&seen).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("env-") {
            envs.push(text);
        } else {
            pids.push(text.trim().to_string());
        }
    }
    assert_eq!(
        envs.len(),
        3,
        "the deps-only build and --report's two: {stderr}"
    );
    for env in &envs {
        let held: Vec<&str> = [
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "GITHUB_TOKEN",
            "HUSHRUN_TOKEN",
        ]
        .into_iter()
        .filter(|name| env.lines().any(|l| l.starts_with(&format!("{name}="))))
        .collect();
        assert!(held.is_empty(), "a bake's build was handed {held:?}");
        assert!(env.lines().any(|l| l == "HUSHRUN_BACKEND="), "{env}");
        assert!(env.lines().any(|l| l == "HUSHRUN_INSIDE_RUN=1"), "{env}");
    }
    for pid in pids {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .status()
            .is_ok_and(|s| s.success());
        if cfg!(target_os = "linux") {
            assert!(!alive, "a process of the bake's build outlived it");
        } else {
            let _ = std::process::Command::new("kill").arg(&pid).status();
        }
    }
}

// covers: B-75
// A block `begin` refuses for good (`suspended`, `too_large`, `unsupported`, `no_scope`) is never
// on the backend: a bake's build that made one isn't whole, so it closes no manifest for the
// commit and says which blocks were refused and why, where counting them as sent would have named
// blocks the backend doesn't hold and left every later bake taking the part for the commit's
// build.
#[test]
fn a_bake_s_build_with_a_refused_block_closes_no_manifest() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\nchecks:\n  build:\n    run: cargo build --offline\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "checks");
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
        cmd.env("CARGO_INCREMENTAL", "0");
    });
    assert_eq!(code, 0, "{stderr}");
    std::fs::write(
        a.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 40\n}\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "the merge");
    let closes = fake.closes().len();
    fake.refuse_begins("suspended", 1000);
    let _ = std::fs::remove_dir_all(a.repo.join("target"));
    let mut cmd = a.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp-bake"));
    on_the_default_branch(&mut cmd, &a.root);
    let baked = cmd
        .env("CARGO_INCREMENTAL", "0")
        .args(["build", "bake", "--from", "backend", "--trusted", "--out"])
        .arg(dir.path().join("out"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&baked.stderr).into_owned();
    assert!(baked.status.success(), "{stderr}");
    assert!(stderr.contains("check `build`: built"), "{stderr}");
    assert!(
        stderr.contains("no manifest was closed for this commit: the backend refused"),
        "{stderr}"
    );
    assert!(stderr.contains("suspended in the repository"), "{stderr}");
    assert_eq!(fake.closes().len(), closes, "a part build was closed");
}

// covers: B-65
// ADR 0102: the bake settles its registry's credential before it builds. A build that plants a
// credential helper on PATH (one the Docker configuration names) is never run by the bake after
// the build, with the bake's own environment.
#[cfg(unix)]
#[test]
fn a_credential_helper_the_build_planted_never_runs() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let plant = a.root.join("bin");
    let docker = a.root.join("docker");
    std::fs::create_dir_all(&plant).unwrap();
    std::fs::create_dir_all(&docker).unwrap();
    std::fs::write(
        docker.join("config.json"),
        r#"{"credHelpers": {"127.0.0.1:9": "planted"}}"#,
    )
    .unwrap();
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\nchecks:\n  build:\n    run: printf '#!/bin/sh\\ntouch \"$MARK\"\\nexit 1\\n' > \"$PLANT/docker-credential-planted\"; chmod +x \"$PLANT/docker-credential-planted\"; cargo build --offline\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "checks");
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
        cmd.env("CARGO_INCREMENTAL", "0");
    });
    assert_eq!(code, 0, "{stderr}");
    std::fs::write(
        a.repo.join("app/src/lib.rs"),
        "pub fn four() -> u32 {\n    dep::two() * 2 + 40\n}\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "the merge");
    let _ = std::fs::remove_dir_all(a.repo.join("target"));
    let mark = a.root.join("helper-ran");
    let path = format!(
        "{}:{}",
        plant.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = a.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp-bake"));
    on_the_default_branch(&mut cmd, &a.root);
    let baked = cmd
        .env("CARGO_INCREMENTAL", "0")
        .env("PATH", &path)
        .env("PLANT", &plant)
        .env("MARK", &mark)
        .env("DOCKER_CONFIG", &docker)
        .args([
            "build",
            "bake",
            "--from",
            "backend",
            "--trusted",
            "--no-sign",
        ])
        .args([
            "--oci",
            "127.0.0.1:9/acme/shop",
            "--insecure-registry",
            "127.0.0.1:9",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&baked.stderr).into_owned();
    assert!(stderr.contains("check `build`: built"), "{stderr}");
    assert!(plant.join("docker-credential-planted").exists(), "{stderr}");
    assert!(
        !mark.exists(),
        "the bake ran a credential helper its build planted: {stderr}"
    );
}

// covers: B-76
// A baked store records the folders the default branch's build ran with: CI's file gives its
// build job CARGO_HOME under the workspace (`${{ github.workspace }}`, the bake job's same
// folder), so baked.json names that one, not the bake process's own.
#[test]
fn a_baked_store_names_the_folders_ci_s_file_gives_the_build() {
    let fake = FakeBuild::start();
    fake.set_scopes(
        Some("protected/github.com/acme/shop"),
        &["protected/github.com/acme/shop"],
    );
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    std::fs::create_dir_all(a.repo.join(".github/workflows")).unwrap();
    std::fs::write(
        a.repo.join(".github/workflows/ci.yml"),
        "name: CI\non:\n  push:\n    branches: [main]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    env:\n      CARGO_HOME: ${{ github.workspace }}/.cargo-ci\n    steps:\n      - uses: actions/checkout@v5\n      - run: hushrun run build -- cargo build --offline\n",
    )
    .unwrap();
    std::fs::write(
        a.repo.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\n",
    )
    .unwrap();
    git::commit_all(&a.repo, "ci");
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        on_the_default_branch(cmd, &a.root);
    });
    assert_eq!(code, 0, "{stderr}");
    let workspace = a.repo.canonicalize().unwrap();
    let mut cmd = a.hushrun(&fake);
    in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp-bake"));
    on_the_default_branch(&mut cmd, &a.root);
    let baked = cmd
        .env("GITHUB_WORKSPACE", &workspace)
        .args([
            "build",
            "bake",
            "--from",
            "backend",
            "--trusted",
            "--no-build",
        ])
        .arg("--out")
        .arg(dir.path().join("out"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&baked.stderr).into_owned();
    assert!(baked.status.success(), "{stderr}");
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("out/baked.json")).unwrap()).unwrap();
    assert_eq!(
        json["paths"]["cargo_home"],
        format!("{}/.cargo-ci", workspace.display()).as_str(),
        "{json}"
    );
}

/// What a CI job with no backend reads, from `hushrun run` when the job asks for reuse, from
/// `hushrun build status` and from a bake of the job's store.
const NO_BACKEND_IN_CI: &str = "in CI hushrun keeps compiled code only in the job's own store, for a backend that shares it, and nothing names one here (hushrun.yaml's `backend`, or HUSHRUN_BACKEND)";

// covers: B-2, B-25, B-31, B-63
// A GitHub Actions job with no backend (`HUSHRUN_BACKEND=""`) keeps no compiled code (B-25's "CI
// otherwise"). A run whose job asked for reuse (`HUSHRUN_BUILD=on`) says so in one line, and a
// later bake of the job's store says what works there instead of telling it to run the build
// through hushrun first, which it just did. A job that asked nothing reads nothing.
#[test]
fn a_job_with_no_backend_says_why_it_keeps_nothing_and_its_bake_says_what_works_there() {
    let fake = FakeBuild::start();
    let dir = tempfile::tempdir().unwrap();
    let a = Machine::new(dir.path(), "a", None);
    let no_backend = |cmd: &mut assert_cmd::Command| {
        cmd.env("HUSHRUN_BACKEND", "").env_remove("HUSHRUN_TOKEN");
    };
    let (code, stdout, stderr) = ci_build(&a, &fake, no_backend);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrapper=none"), "{stdout}");
    let line = format!("hushrun: build reuse is off for this run: {NO_BACKEND_IN_CI}");
    assert_eq!(stderr.matches(&line).count(), 1, "{stderr}");
    // Nobody asked: nothing said.
    let (code, _, stderr) = ci_build(&a, &fake, |cmd| {
        no_backend(cmd);
        cmd.env_remove("HUSHRUN_BUILD");
    });
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("build reuse is off"), "{stderr}");
    // The bake that follows in the job, and `build manifest`: refused, with what works.
    let job = |args: &[&str]| {
        let mut cmd = a.hushrun(&fake);
        in_github_actions(&mut cmd, &fake, &a.root.join("runner-temp"));
        no_backend(&mut cmd);
        let output = cmd.args(args).output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let out = a.root.join("baked");
    let (code, stderr) = job(&[
        "build",
        "bake",
        "--from",
        "store",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains(NO_BACKEND_IN_CI), "{stderr}");
    assert!(stderr.contains("--deps-only -- COMMAND"), "{stderr}");
    assert!(!stderr.contains("first, with build reuse on"), "{stderr}");
    let (code, stderr) = job(&["build", "manifest"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains(NO_BACKEND_IN_CI), "{stderr}");
    assert!(!out.exists());
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}
