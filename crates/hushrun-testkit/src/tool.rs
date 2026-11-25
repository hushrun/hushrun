//! Starting a program a test runs that is none of the workspace's own binaries: a test runner
//! (Gradle, pytest, `go`), a fixture's script (`./gradlew`), a tool's `--version`.
//!
//! The static test map counts a test that starts a program it can't name, a `Command::new` of a
//! variable, as reading the whole `hushrun` binary, since that program could be it (C-151). A
//! test that starts a runner from `HUSHRUN_TEST_GRADLE` would then run again on any change to
//! hushrun. [`command`] carries the map's marker, `hushrun::starts_no_workspace_binary`, so its
//! `Command::new` doesn't count, and it keeps that promise at run time: it panics when `program`
//! is one of the workspace's binaries, so a broken promise fails the test, never passes it.
//!
//! A test that does start hushrun starts it as before, by `env!("CARGO_BIN_EXE_hushrun")` or
//! [`crate::world::World`], and reads the whole binary, as it should.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// `program`, which is none of the workspace's binaries, as a [`Command`]: it panics when it is
/// one ([`refuse_workspace_binary`]).
#[doc(alias = "hushrun::starts_no_workspace_binary")]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let program = program.as_ref();
    refuse_workspace_binary(program);
    Command::new(program)
}

/// The workspace as `cargo metadata` tells it: where it builds, and its binary targets' names.
struct Workspace {
    target: PathBuf,
    bins: Vec<String>,
}

/// The workspace, asked once of `cargo metadata --no-deps` in this crate's directory. It uses no
/// `current_exe()`, which the map would count as starting a workspace binary (C-151).
fn workspace() -> &'static Workspace {
    static WORKSPACE: OnceLock<Workspace> = OnceLock::new();
    WORKSPACE.get_or_init(|| {
        let output = Command::new("cargo")
            .args([
                "metadata",
                "--no-deps",
                "--format-version",
                "1",
                "--offline",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("cargo metadata starts");
        assert!(
            output.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("cargo metadata's JSON");
        let target = metadata["target_directory"]
            .as_str()
            .map(PathBuf::from)
            .expect("cargo metadata names the target directory");
        let target = std::fs::canonicalize(&target).unwrap_or(target);
        let bins = metadata["packages"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|package| package["targets"].as_array().into_iter().flatten())
            .filter(|target| {
                target["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
            })
            .filter_map(|target| target["name"].as_str().map(str::to_string))
            .collect();
        Workspace { target, bins }
    })
}

/// Panics when `program` is one of the workspace's binaries: when its file name, without an
/// extension, is a binary target's name (`hushrun`, `xtask`), or when it resolves, as a path or
/// a bare name on `PATH`, to a file below the workspace's target directory.
pub fn refuse_workspace_binary(program: &OsStr) {
    let workspace = workspace();
    let path = Path::new(program);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    assert!(
        !workspace.bins.contains(&stem),
        "{} is a binary of this workspace: start it as a workspace binary (env!(\"CARGO_BIN_EXE_…\")), not through hushrun_testkit::tool::command",
        path.display()
    );
    if let Some(found) = resolve(program).and_then(|found| std::fs::canonicalize(found).ok()) {
        assert!(
            !found.starts_with(&workspace.target),
            "{} is below this workspace's target directory ({}): start a workspace binary as one, not through hushrun_testkit::tool::command",
            found.display(),
            workspace.target.display()
        );
    }
}

/// Where `program` is: itself when it names a directory, else the first match on `PATH`.
fn resolve(program: &OsStr) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return Some(path.to_path_buf());
    }
    let paths = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_of_its_own_starts() {
        let output = command("git").arg("--version").output().expect("git");
        assert!(output.status.success());
    }

    #[test]
    #[should_panic(expected = "is a binary of this workspace")]
    fn the_workspaces_own_binary_is_refused_by_its_name() {
        let _ = command("/somewhere/else/hushrun");
    }

    #[test]
    #[should_panic(expected = "below this workspace's target directory")]
    fn what_is_below_the_target_directory_is_refused_by_where_it_is() {
        let _ = command(&workspace().target);
    }
}
