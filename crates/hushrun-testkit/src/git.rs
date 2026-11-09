//! Running git in tests, with a fixed identity and no user configuration.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `git -C dir args`, feeding `stdin`, and returns stdout. Panics on failure.
pub fn run(dir: &Path, args: &[&str], stdin: Option<&[u8]>) -> Vec<u8> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_AUTHOR_NAME", "hushrun tests")
        .env("GIT_AUTHOR_EMAIL", "tests@hushrun.invalid")
        .env("GIT_COMMITTER_NAME", "hushrun tests")
        .env("GIT_COMMITTER_EMAIL", "tests@hushrun.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("git runs");
    if let Some(bytes) = stdin {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(bytes)
            .expect("write to git");
    }
    let output = child.wait_with_output().expect("git finishes");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// `run`, trimmed, as text.
pub fn text(dir: &Path, args: &[&str]) -> String {
    String::from_utf8(run(dir, args, None))
        .expect("utf-8")
        .trim()
        .to_string()
}

/// A fresh repository with deterministic settings.
pub fn init(dir: &Path, object_format: &str) {
    run(
        dir,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={object_format}"),
        ],
        None,
    );
    run(dir, &["config", "core.autocrlf", "false"], None);
}

/// Commits everything in the worktree and returns the commit id.
pub fn commit_all(dir: &Path, message: &str) -> String {
    run(dir, &["add", "-A"], None);
    run(dir, &["commit", "-q", "--allow-empty", "-m", message], None);
    text(dir, &["rev-parse", "HEAD"])
}
