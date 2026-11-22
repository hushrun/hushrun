//! Whether `hushrun run` sets `GOCACHEPROG` for COMMAND (B-39), and to what.
//!
//! It is set only for a Go module (a `go.mod` or `go.work` in the folder or above it), only when
//! the `go` there is a version whose cache this program knows, and never over a cache program the
//! person set: in the environment, in the user's go env file, or in the toolchain's own
//! `$GOROOT/go.env` (F12). `go env` says all three at once, from the folder COMMAND runs in, so
//! it is what is asked, once, as the run starts: a few milliseconds, paid only by a Go module.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `go env` may take. Past it (a toolchain being downloaded) the run doesn't wait:
/// `go` runs as it would without hushrun.
const PROBE_WAIT: Duration = Duration::from_secs(10);

/// What `hushrun run` sets for a Go module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Go {
    /// `GOCACHEPROG`'s value: the program's path, quoted when it holds a space (F7).
    pub value: String,
    /// `GOOS_GOARCH`, the platform the run's Go manifest is kept for (B-45).
    pub platform: String,
    /// `go env GOVERSION`.
    pub version: String,
}

/// Why `GOCACHEPROG` is not set. Only [`NotSet::Unquotable`] and [`NotSet::Failed`] are worth a
/// line; the rest are the ordinary case of a run that is not Go's, or a person's own choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotSet {
    /// No `go.mod` or `go.work` in the folder or above it.
    NotGo,
    /// A cache program is set already, where Go reads one.
    AlreadySet,
    /// A Go version whose cache this program doesn't know (before 1.24, or newer than it knows).
    Version(String),
    /// The program's path holds both a space and quotes: Go can't be given it (F7).
    Unquotable(PathBuf),
    /// `go env` couldn't be run, failed, or took too long.
    Failed(String),
}

/// Whether `folder`, or a folder above it, holds a `go.mod` or `go.work`: a Go module's.
pub fn is_module(folder: &Path) -> bool {
    module_root(folder).is_some()
}

/// The folder `folder` is in, or it, that holds a `go.mod` or `go.work`.
fn module_root(folder: &Path) -> Option<&Path> {
    folder
        .ancestors()
        .find(|dir| dir.join("go.mod").is_file() || dir.join("go.work").is_file())
}

/// `program` as Go splits `GOCACHEPROG` (`quoted.Split`): fields split on white space, a field
/// that starts with `'` or `"` running to the next one, no escapes. A path with no white space
/// that doesn't start with a quote goes as it is; one with white space is wrapped in `'`, or in
/// `"` when it holds a `'`. `None` when it holds white space and both quotes.
pub fn quote(program: &Path) -> Option<String> {
    let text = program.to_str()?;
    let plain = !text.chars().any(char::is_whitespace) && !text.starts_with(['\'', '"']);
    if plain {
        Some(text.to_string())
    } else if !text.contains('\'') {
        Some(format!("'{text}'"))
    } else if !text.contains('"') {
        Some(format!("\"{text}\""))
    } else {
        None
    }
}

/// Decides `GOCACHEPROG` for COMMAND in `folder`: `program` is the `hushrun-gocache` link.
/// `env` is the run's environment, where COMMAND's `GOCACHEPROG` would be.
pub fn probe(
    folder: &Path,
    program: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Go, NotSet> {
    if env(super::GOCACHEPROG).is_some_and(|v| !v.is_empty()) {
        return Err(NotSet::AlreadySet);
    }
    let root = module_root(folder).ok_or(NotSet::NotGo)?;
    let value = quote(program).ok_or_else(|| NotSet::Unquotable(program.to_path_buf()))?;
    let asked = go_env(root, &["GOVERSION", "GOCACHEPROG", "GOOS", "GOARCH"])?;
    let field = |name: &str| {
        asked
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    if !field("GOCACHEPROG").is_empty() {
        return Err(NotSet::AlreadySet);
    }
    let version = field("GOVERSION");
    if !super::command::version_known(&version) {
        return Err(NotSet::Version(version));
    }
    let platform = format!("{}_{}", field("GOOS"), field("GOARCH"));
    if !hushrun_core::blocks::is_platform(&platform) {
        return Err(NotSet::Failed(format!(
            "go env names platform {platform:?}"
        )));
    }
    Ok(Go {
        value,
        platform,
        version,
    })
}

/// What `hushrun build bake` records of Go in `folder` (B-63): `GOOS_GOARCH`, when the folder is
/// a Go module's and `go env` answers. `None` otherwise.
pub fn env_of(folder: &Path) -> Option<String> {
    let root = module_root(folder)?;
    let asked = go_env(root, &["GOOS", "GOARCH"]).ok()?;
    let field = |name: &str| {
        asked
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    let platform = format!("{}_{}", field("GOOS"), field("GOARCH"));
    hushrun_core::blocks::is_platform(&platform).then_some(platform)
}

/// `go env -json NAMES...` in `folder`, within [`PROBE_WAIT`]: its stdout read through a pipe,
/// stdin closed, stderr dropped.
fn go_env(folder: &Path, names: &[&str]) -> Result<serde_json::Value, NotSet> {
    let mut child = Command::new("go")
        .arg("env")
        .arg("-json")
        .args(names)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| NotSet::Failed(format!("go env: {err}")))?;
    let deadline = Instant::now() + PROBE_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(NotSet::Failed("go env took too long".into()));
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| NotSet::Failed(format!("go env: {err}")))?;
    if !output.status.success() {
        return Err(NotSet::Failed(format!(
            "go env exited with {}",
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|err| NotSet::Failed(format!("go env's answer: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-39
    #[test]
    fn a_path_with_a_space_is_quoted_as_go_splits_it() {
        assert_eq!(
            quote(Path::new("/a/b/hushrun-gocache")).unwrap(),
            "/a/b/hushrun-gocache"
        );
        // F7: unquoted, Go would start "/home/Jane" with an argument.
        assert_eq!(
            quote(Path::new("/home/Jane Doe/x/hushrun-gocache")).unwrap(),
            "'/home/Jane Doe/x/hushrun-gocache'"
        );
        assert_eq!(
            quote(Path::new("/home/O'Brien/x")).unwrap(),
            "/home/O'Brien/x"
        );
        assert_eq!(
            quote(Path::new("/home/O'Brien Two/x")).unwrap(),
            "\"/home/O'Brien Two/x\""
        );
        assert_eq!(quote(Path::new("/a \"b\" 'c'/x")), None);
    }

    // covers: B-39
    #[test]
    fn only_a_go_module_is_asked_and_a_set_program_stays() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("hushrun-gocache");
        // No module: nothing is asked.
        assert_eq!(probe(dir.path(), &program, &|_| None), Err(NotSet::NotGo));
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/m\n\ngo 1.24\n",
        )
        .unwrap();
        let below = dir.path().join("cmd/app");
        std::fs::create_dir_all(&below).unwrap();
        // A program the person set in COMMAND's environment stays, before anything is run.
        let set = |name: &str| (name == "GOCACHEPROG").then(|| OsString::from("my-cache"));
        assert_eq!(probe(&below, &program, &set), Err(NotSet::AlreadySet));
    }
}
