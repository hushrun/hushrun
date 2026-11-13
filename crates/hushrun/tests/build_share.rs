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
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let repo = match from {
            None => workspace(&root),
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
    fake.set_manifest(close.manifest);

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
