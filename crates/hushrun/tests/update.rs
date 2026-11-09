//! `hushrun update` (C-20) and the newer-release notice (C-21), against a fake GitHub release on
//! the loopback: no test reaches GitHub. The releases' binaries are shell scripts here, and the
//! notice needs a terminal, so these tests run on Linux and macOS.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hushrun_testkit::http::{Reply, Response, Server};
use serde_json::{Value, json};
use sha2::Digest as _;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// What a signed storage URL carries, which no message may show.
const SIGNATURE: &str = "sig=secret-signature";

/// The fake's releases: the newest one's version, and files by path.
#[derive(Default)]
struct Releases {
    latest: Option<String>,
    files: BTreeMap<String, Vec<u8>>,
    /// How long `releases/latest` takes to answer.
    delay: Option<Duration>,
    /// When `releases/latest` was first asked for.
    asked_at: Option<Instant>,
    requests: Vec<String>,
}

/// GitHub's release pages and downloads, as `hushrun update` reads them: `releases/latest`
/// redirects to the newest release's tag, and a download redirects to storage elsewhere, with
/// a signature in its URL.
struct FakeGitHub {
    server: Server,
    state: Arc<Mutex<Releases>>,
}

impl FakeGitHub {
    fn start(latest: &str) -> FakeGitHub {
        let state = Arc::new(Mutex::new(Releases {
            latest: Some(latest.to_string()),
            ..Releases::default()
        }));
        let shared = state.clone();
        let server = Server::start(move |request| {
            let mut state = shared.lock().unwrap();
            state.requests.push(request.path.clone());
            if request.path == "/hushrun/hushrun/releases/latest" {
                state.asked_at.get_or_insert_with(Instant::now);
                let Some(latest) = state.latest.clone() else {
                    return Reply::Send(Response::new(404));
                };
                let answer = Response::new(302).header(
                    "location",
                    format!("https://github.com/hushrun/hushrun/releases/tag/v{latest}"),
                );
                return match state.delay {
                    Some(delay) => Reply::Later(delay, answer),
                    None => Reply::Send(answer),
                };
            }
            if let Some(name) = request
                .path
                .strip_prefix("/hushrun/hushrun/releases/download/")
            {
                return Reply::Send(
                    Response::new(302).header("location", format!("/storage/{name}?{SIGNATURE}")),
                );
            }
            if request.path.starts_with("/storage/")
                && request.query("sig") == Some("secret-signature")
            {
                let name = request.path.trim_start_matches("/storage/");
                if let Some(bytes) = state.files.get(name) {
                    return Reply::Send(Response::bytes(200, bytes.clone()));
                }
            }
            Reply::Send(Response::new(404))
        });
        FakeGitHub { server, state }
    }

    fn put(&self, path: &str, bytes: impl Into<Vec<u8>>) {
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.to_string(), bytes.into());
    }

    fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }

    /// How many times hushrun asked for the newest release.
    fn asked(&self) -> usize {
        self.requests()
            .iter()
            .filter(|path| path.ends_with("/releases/latest"))
            .count()
    }
}

/// The release target of this machine, as releases name their archives.
fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        (os, arch) => panic!("no release for {arch} on {os}"),
    }
}

/// A home with hushrun where its shell installer puts it, `~/.cargo/bin/hushrun` (a copy of the
/// binary under test), and the installer's receipt in `~/.config/hushrun`.
struct Home {
    _dir: tempfile::TempDir,
    root: PathBuf,
    binary: PathBuf,
    receipt: PathBuf,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let bin = root.join("home/.cargo/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let binary = bin.join("hushrun");
        std::fs::copy(env!("CARGO_BIN_EXE_hushrun"), &binary).unwrap();
        let receipt = root.join("home/.config/hushrun/hushrun-receipt.json");
        Home {
            _dir: dir,
            root,
            binary,
            receipt,
        }
    }

    /// The receipt hushrun's shell installer writes (dist 0.32), for `version`.
    fn install_receipt(&self, version: &str) {
        std::fs::create_dir_all(self.receipt.parent().unwrap()).unwrap();
        let prefix = self.root.join("home/.cargo");
        let receipt = json!({
            "binaries": ["hushrun"],
            "binary_aliases": {},
            "cdylibs": [],
            "cstaticlibs": [],
            "install_layout": "cargo-home",
            "install_prefix": prefix.to_string_lossy(),
            "modify_path": true,
            "provider": {"source": "cargo-dist", "version": "0.32.0"},
            "source": {"app_name": "hushrun", "name": "hushrun", "owner": "hushrun", "release_type": "github"},
            "version": version,
        });
        std::fs::write(&self.receipt, format!("{receipt}\n")).unwrap();
    }

    fn receipt(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.receipt).unwrap()).unwrap()
    }

    /// `binary` with nothing inherited but `PATH`: this home, GitHub at `github`, and a cache of
    /// its own.
    fn command(&self, binary: &Path, github: &FakeGitHub) -> Command {
        let mut cmd = Command::new(binary);
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.join("home"))
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_INSTALLER_GITHUB_BASE_URL", github.server.url())
            .current_dir(&self.root);
        cmd
    }
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn ran(cmd: &mut Command) -> Ran {
    let output = cmd.output().unwrap();
    Ran {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// What `binary --version` says.
fn version_of(binary: &Path) -> String {
    let output = Command::new(binary).arg("--version").output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A release archive for this machine, as the release workflow packs it, whose `hushrun` says
/// `hushrun <version>`; and its checksum file.
fn archive(version: &str) -> (String, Vec<u8>, String) {
    let dir = tempfile::tempdir().unwrap();
    let name = format!("hushrun-{}", target());
    let inner = dir.path().join(&name);
    std::fs::create_dir_all(&inner).unwrap();
    let binary = inner.join("hushrun");
    std::fs::write(&binary, format!("#!/bin/sh\necho 'hushrun {version}'\n")).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(inner.join("README.md"), "# hushrun\n").unwrap();
    let file = format!("{name}.tar.xz");
    let status = Command::new("tar")
        .arg("-cJf")
        .arg(dir.path().join(&file))
        .arg("-C")
        .arg(dir.path())
        .arg(&name)
        .status()
        .unwrap();
    assert!(status.success(), "tar -cJf");
    let bytes = std::fs::read(dir.path().join(&file)).unwrap();
    let sum = sha256_hex(&bytes);
    (file.clone(), bytes, format!("{sum} *{file}\n"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Whatever is left beside the binary, other than the binary.
fn leftovers(home: &Home) -> Vec<String> {
    std::fs::read_dir(home.binary.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "hushrun")
        .collect()
}

// covers: C-20
#[test]
fn update_puts_the_newest_release_where_the_installer_put_hushrun() {
    let home = Home::new();
    home.install_receipt(VERSION);
    let github = FakeGitHub::start("9.9.9");
    let (file, bytes, sums) = archive("9.9.9");
    github.put(&format!("v9.9.9/{file}"), bytes);
    github.put(&format!("v9.9.9/{file}.sha256"), sums);

    let update = ran(home.command(&home.binary, &github).arg("update"));
    assert_eq!(update.code, 0, "{}", update.stderr);
    assert_eq!(
        update.stdout,
        format!(
            "Updated hushrun {VERSION} to 9.9.9 in {}.\nRelease notes: {}/hushrun/hushrun/releases/tag/v9.9.9\n",
            home.binary.display(),
            github.server.url()
        )
    );
    assert_eq!(
        update.stderr,
        format!("Downloading hushrun 9.9.9 for {}.\n", target())
    );
    // The new binary is in place, and the receipt says its version, as the installer would
    // have written it.
    assert_eq!(version_of(&home.binary), "hushrun 9.9.9");
    let receipt = home.receipt();
    assert_eq!(receipt["version"], "9.9.9");
    assert_eq!(receipt["install_layout"], "cargo-home");
    assert_eq!(receipt["provider"]["version"], "0.32.0");
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    // The checksum, then the archive, both from the release GitHub named, through storage whose
    // signed URL nothing printed.
    assert_eq!(
        github.requests(),
        [
            "/hushrun/hushrun/releases/latest".to_string(),
            format!("/hushrun/hushrun/releases/download/v9.9.9/{file}.sha256"),
            format!("/storage/v9.9.9/{file}.sha256"),
            format!("/hushrun/hushrun/releases/download/v9.9.9/{file}"),
            format!("/storage/v9.9.9/{file}"),
        ]
    );
    assert!(!update.stdout.contains(SIGNATURE) && !update.stderr.contains(SIGNATURE));
}

// covers: C-20
#[test]
fn update_changes_nothing_unless_the_archive_is_the_releases() {
    let home = Home::new();
    home.install_receipt(VERSION);
    let github = FakeGitHub::start("9.9.9");
    let (file, bytes, _) = archive("9.9.9");
    let wrong = "0".repeat(64);
    github.put(&format!("v9.9.9/{file}"), bytes.clone());
    github.put(
        &format!("v9.9.9/{file}.sha256"),
        format!("{wrong} *{file}\n"),
    );
    let unchanged = |ran: &Ran, needle: &str| {
        assert_eq!(ran.code, 1, "{}", ran.stderr);
        assert!(ran.stderr.contains(needle), "{}", ran.stderr);
        assert!(!ran.stderr.contains(SIGNATURE), "{}", ran.stderr);
        assert_eq!(version_of(&home.binary), format!("hushrun {VERSION}"));
        assert_eq!(home.receipt()["version"], VERSION);
        assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    };

    // A checksum that doesn't match.
    let update = ran(home.command(&home.binary, &github).arg("update"));
    unchanged(
        &update,
        &format!(
            "has SHA-256 {}, but the release's {file}.sha256 says {wrong}: hushrun is unchanged",
            sha256_hex(&bytes)
        ),
    );
    // A checksum file for another archive.
    github.put(
        &format!("v9.9.9/{file}.sha256"),
        format!("{} *hushrun-other.tar.xz\n", sha256_hex(&bytes)),
    );
    let update = ran(home.command(&home.binary, &github).arg("update"));
    unchanged(&update, "is the checksum of hushrun-other.tar.xz");
    // An archive whose binary is another release.
    let (_, other, sums) = archive("9.9.8");
    github.put(&format!("v9.9.9/{file}"), other);
    github.put(&format!("v9.9.9/{file}.sha256"), sums);
    let update = ran(home.command(&home.binary, &github).arg("update"));
    unchanged(
        &update,
        "says \"hushrun 9.9.8\" to --version, not \"hushrun 9.9.9\"",
    );
    // A release without this platform's archive.
    github.state.lock().unwrap().files.clear();
    let update = ran(home.command(&home.binary, &github).arg("update"));
    unchanged(&update, &format!("{file}.sha256: 404"));
}

// covers: C-20
#[test]
fn update_downloads_nothing_where_it_cant_write() {
    // Permissions don't stop root.
    if rustix::process::geteuid().is_root() {
        return;
    }
    let home = Home::new();
    home.install_receipt(VERSION);
    let github = FakeGitHub::start("9.9.9");
    let bin = home.binary.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
    let update = ran(home.command(&home.binary, &github).arg("update"));
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(update.code, 1, "{}", update.stderr);
    assert!(
        update.stderr.contains(&format!(
            "error: can't write in {}: run hushrun update as the user who installed hushrun there",
            bin.display()
        )),
        "{}",
        update.stderr
    );
    assert_eq!(github.requests(), ["/hushrun/hushrun/releases/latest"]);
    assert_eq!(version_of(&home.binary), format!("hushrun {VERSION}"));
}

// covers: C-20
#[test]
fn update_says_when_this_is_the_newest_release() {
    let home = Home::new();
    home.install_receipt(VERSION);
    for (latest, says) in [
        (
            VERSION.to_string(),
            format!("hushrun {VERSION} is the newest release.\n"),
        ),
        (
            "0.0.1".to_string(),
            format!("hushrun {VERSION} is newer than the newest release, 0.0.1.\n"),
        ),
    ] {
        let github = FakeGitHub::start(&latest);
        let update = ran(home.command(&home.binary, &github).arg("update"));
        assert_eq!(update.code, 0, "{}", update.stderr);
        assert_eq!(update.stdout, says);
        assert_eq!(github.requests(), ["/hushrun/hushrun/releases/latest"]);
    }
    // No newest release to name: GitHub's answer, and exit 1.
    let github = FakeGitHub::start(VERSION);
    github.state.lock().unwrap().latest = None;
    let update = ran(home.command(&home.binary, &github).arg("update"));
    assert_eq!(update.code, 1);
    assert!(
        update
            .stderr
            .contains("error: can't tell which hushrun release is the newest: ")
            && update
                .stderr
                .contains("answered 404 Not Found, not a redirect to the newest release"),
        "{}",
        update.stderr
    );
    // Releases come over https://, or from this machine.
    let update = ran(home
        .command(&home.binary, &github)
        .env("HUSHRUN_INSTALLER_GITHUB_BASE_URL", "http://github.com")
        .arg("update"));
    assert_eq!(update.code, 2);
    assert!(
        update.stderr.contains("HUSHRUN_INSTALLER_GITHUB_BASE_URL is \"http://github.com\": releases are read over https:// only"),
        "{}",
        update.stderr
    );
}

// covers: C-20
#[test]
fn update_leaves_other_installs_alone_and_says_how_to_update_them() {
    let home = Home::new();
    let github = FakeGitHub::start("9.9.9");
    let unchanged = |ran: &Ran, binary: &Path| {
        assert_eq!(ran.code, 2, "{}", ran.stderr);
        assert_eq!(ran.stdout, "");
        assert_eq!(version_of(binary), format!("hushrun {VERSION}"));
    };

    // No receipt: built from source, copied by hand, installed by CI.
    let update = ran(home.command(&home.binary, &github).arg("update"));
    unchanged(&update, &home.binary);
    assert!(
        update.stderr.contains(&format!(
            "error: hushrun 9.9.9 is out, and this hushrun ({VERSION}, at {}) wasn't installed by hushrun's installer, so update leaves it alone: install hushrun 9.9.9 the way this one was installed, or with hushrun's installer, whose installs hushrun update keeps up to date: curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh | sh",
            home.binary.display()
        )),
        "{}",
        update.stderr
    );
    // A receipt for another copy: update names it.
    home.install_receipt(VERSION);
    let copy = home.root.join("copy/hushrun");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&home.binary, &copy).unwrap();
    let update = ran(home.command(&copy, &github).arg("update"));
    unchanged(&update, &copy);
    assert!(
        update.stderr.contains(&format!(
            "hushrun's installer put hushrun at {}: run `{} update`",
            home.binary.display(),
            home.binary.display()
        )),
        "{}",
        update.stderr
    );
    // In node_modules: npm's.
    let npm = home
        .root
        .join("app/node_modules/@hushrun/cli-darwin-arm64/bin/hushrun");
    std::fs::create_dir_all(npm.parent().unwrap()).unwrap();
    std::fs::copy(&home.binary, &npm).unwrap();
    let update = ran(home.command(&npm, &github).arg("update"));
    unchanged(&update, &npm);
    assert!(
        update.stderr.contains("came with npm: update the hushrun package with the package manager that installed it, such as npm install hushrun@latest"),
        "{}",
        update.stderr
    );
    // None of them downloaded anything.
    assert!(
        github
            .requests()
            .iter()
            .all(|path| path.ends_with("/releases/latest")),
        "{:?}",
        github.requests()
    );
}

mod terminal {
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    /// Runs `cmd` with its stderr on a terminal of its own, and returns its exit code and what
    /// it wrote there.
    pub fn stderr_on_a_terminal(mut cmd: Command) -> (i32, String) {
        let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("openpt");
        grantpt(&controller).expect("grantpt");
        unlockpt(&controller).expect("unlockpt");
        let name = ptsname(&controller, Vec::new()).expect("ptsname");
        // CLOEXEC: a test beside this one may start a command while this end is open here, and
        // that command would go on holding the terminal once hushrun exits.
        let terminal = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("the terminal");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::from(terminal));
        let mut child = cmd.spawn().expect("hushrun");
        // Only hushrun holds the terminal now, so reading it ends when hushrun exits.
        drop(cmd);
        let mut controller = std::fs::File::from(controller);
        let reader = std::thread::spawn(move || {
            let mut text = Vec::new();
            let mut buf = [0; 4096];
            // Once nothing holds the terminal, Linux answers EIO, and macOS the end.
            while let Ok(n @ 1..) = controller.read(&mut buf) {
                text.extend_from_slice(&buf[..n]);
            }
            String::from_utf8_lossy(&text).replace("\r\n", "\n")
        });
        let status = child.wait().expect("hushrun");
        (status.code().unwrap_or(-1), reader.join().expect("reader"))
    }
}

/// `hushrun digest` of a directory: quick, and needs nothing.
fn digest(home: &Home, github: &FakeGitHub) -> Command {
    let dir = home.root.join("files");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    let mut cmd = home.command(&home.binary, github);
    cmd.args(["digest", "--dir"]).arg(&dir);
    cmd
}

/// `hushrun run unit -- sleep 1`, outside a repository: it runs the command, which takes long
/// enough for the check in the background to answer, however slow the test's machine.
fn a_second(home: &Home, github: &FakeGitHub) -> Command {
    let mut cmd = home.command(&home.binary, github);
    cmd.args(["run", "unit", "--", "sleep", "1"]);
    cmd
}

fn notices(terminal: &str) -> Vec<&str> {
    terminal
        .lines()
        .filter(|line| line.contains(" is available: "))
        .collect()
}

// covers: C-21
#[test]
fn at_a_terminal_hushrun_says_once_a_day_that_a_newer_release_is_out() {
    let home = Home::new();
    let github = FakeGitHub::start("9.9.9");
    let notice = "hushrun 9.9.9 is available: run `hushrun update`";

    // Once the command is done, as its last line.
    let (code, terminal) = terminal::stderr_on_a_terminal(a_second(&home, &github));
    assert_eq!(code, 0, "{terminal}");
    assert_eq!(notices(&terminal), [notice], "{terminal}");
    assert_eq!(terminal.lines().last(), Some(notice), "{terminal}");
    assert_eq!(github.asked(), 1);
    // The rest of the day: no question to GitHub, and nothing said.
    let (code, terminal) = terminal::stderr_on_a_terminal(digest(&home, &github));
    assert_eq!(code, 0, "{terminal}");
    assert_eq!(terminal, "");
    assert_eq!(github.asked(), 1);
    // A day later, asked and said again.
    let state = home.root.join("cache/update-check.json");
    let mut remembered: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(remembered["latest"], "9.9.9");
    for at in ["asked", "answered", "noticed"] {
        remembered[at] = json!(remembered[at].as_i64().unwrap() - 25 * 3600);
    }
    std::fs::write(&state, remembered.to_string()).unwrap();
    let (_, terminal) = terminal::stderr_on_a_terminal(a_second(&home, &github));
    assert_eq!(notices(&terminal), [notice], "{terminal}");
    assert_eq!(github.asked(), 2);
    // Up to date: GitHub answered, and there is nothing to say.
    let current = Home::new();
    let github = FakeGitHub::start(VERSION);
    let (_, terminal) = terminal::stderr_on_a_terminal(a_second(&current, &github));
    assert!(notices(&terminal).is_empty(), "{terminal}");
    assert_eq!(github.asked(), 1);
    let remembered: Value = serde_json::from_slice(
        &std::fs::read(current.root.join("cache/update-check.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(remembered["latest"], VERSION);
}

// covers: C-21
#[test]
fn an_install_of_npms_is_told_to_update_the_package() {
    let home = Home::new();
    let github = FakeGitHub::start("9.9.9");
    let npm = home
        .root
        .join("app/node_modules/@hushrun/cli-darwin-arm64/bin/hushrun");
    std::fs::create_dir_all(npm.parent().unwrap()).unwrap();
    std::fs::copy(&home.binary, &npm).unwrap();
    let mut cmd = home.command(&npm, &github);
    cmd.args(["run", "unit", "--", "sleep", "1"]);
    let (code, terminal) = terminal::stderr_on_a_terminal(cmd);
    assert_eq!(code, 0, "{terminal}");
    assert_eq!(
        notices(&terminal),
        [
            "hushrun 9.9.9 is available: update the hushrun package with the package manager that installed it, such as npm install hushrun@latest"
        ],
        "{terminal}"
    );
}

// covers: C-21
#[test]
fn the_notice_stays_out_of_ci_scripts_and_machine_output() {
    let github = FakeGitHub::start("9.9.9");
    let quiet = |configure: &dyn Fn(&mut Command)| {
        let home = Home::new();
        let mut cmd = digest(&home, &github);
        configure(&mut cmd);
        let (code, terminal) = terminal::stderr_on_a_terminal(cmd);
        assert_eq!(code, 0, "{terminal}");
        assert!(!terminal.contains("is available"), "{terminal}");
        assert!(!home.root.join("cache/update-check.json").exists());
    };
    for (name, value) in [
        ("CI", "true"),
        ("GITHUB_ACTIONS", "true"),
        ("GITLAB_CI", "true"),
        ("BUILDKITE", "true"),
        ("TF_BUILD", "True"),
        ("HUSHRUN_NO_UPDATE_CHECK", "1"),
    ] {
        quiet(&|cmd: &mut Command| {
            cmd.env(name, value);
        });
    }
    // ADR 0061: a Jenkins job, as CI detection knows one.
    quiet(&|cmd: &mut Command| {
        cmd.env("JENKINS_URL", "https://jenkins.example")
            .env("BUILD_ID", "7");
    });
    quiet(&|cmd: &mut Command| {
        cmd.arg("--format").arg("json");
    });
    quiet(&|cmd: &mut Command| {
        cmd.arg("-q");
    });
    // Without a cache to remember the day's check in, hushrun doesn't ask.
    quiet(&|cmd: &mut Command| {
        cmd.env("HUSHRUN_CACHE_DIR", "");
    });
    assert_eq!(github.asked(), 0);
    // Without a terminal: a script, an agent, CI's log.
    let home = Home::new();
    let digest = ran(&mut digest(&home, &github));
    assert_eq!(digest.code, 0, "{}", digest.stderr);
    assert_eq!(digest.stderr, "");
    assert_eq!(github.asked(), 0);
}

// covers: C-21
#[test]
fn a_slow_answer_never_holds_a_command() {
    let home = Home::new();
    let github = FakeGitHub::start("9.9.9");
    github.state.lock().unwrap().delay = Some(Duration::from_secs(5));
    let (code, terminal) = terminal::stderr_on_a_terminal(digest(&home, &github));
    let exited = Instant::now();
    assert_eq!(code, 0, "{terminal}");
    assert_eq!(terminal, "");
    // From the question, not from the start: macOS checks a new binary the first time it runs,
    // which alone took over 3 s on a busy Mac. Waiting for this answer would take 3 s, hushrun's
    // own limit on it.
    let asked = github.state.lock().unwrap().asked_at.expect("the question");
    let held = exited.duration_since(asked);
    assert!(held < Duration::from_secs(2), "{held:?}");
    // The question was asked: the next command, within the hour, doesn't ask again.
    let (_, terminal) = terminal::stderr_on_a_terminal(digest(&home, &github));
    assert_eq!(terminal, "");
    assert_eq!(github.asked(), 1);
}
