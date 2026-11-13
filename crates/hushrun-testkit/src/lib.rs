//! Test support: golden vectors, fixtures and fakes (dev only).
#![forbid(unsafe_code)]

#[cfg(unix)]
pub mod container;
pub mod env;
pub mod fake_forge;
pub mod fake_github;
pub mod fake_gitlab;
pub mod fake_ledger;
pub mod fake_results;
pub mod git;
pub mod http;
pub mod lines;
pub mod scenario;
pub mod schema;
pub mod sigstore;
pub mod vectors;
pub mod world;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The workspace root, for reading `testdata/`.
///
/// Found when the tests run, never baked in when this library is compiled: a path compiled in
/// would tie the library, and every test binary that links it, to one checkout, so a build
/// reused from another checkout could not be (docs/spec/10-build-blocks.md, B-10). Cargo sets
/// `CARGO_MANIFEST_DIR` for every test and `cargo run` process: the package under test, which in
/// this repository is `crates/<name>`, two levels below the root. Without it, or when the folder
/// two levels up is not the workspace (no `testdata/` and `Cargo.lock` there), the root is the
/// first folder above the running executable that has both, as when cargo's target folder is
/// inside the workspace and a test binary runs on its own.
pub fn workspace_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let package = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
        let two_up = package
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .filter(|dir| is_workspace_root(dir))
            .map(Path::to_path_buf);
        two_up
            .or_else(|| {
                let exe = std::env::current_exe().ok()?;
                exe.ancestors()
                    .skip(1)
                    .find(|dir| is_workspace_root(dir))
                    .map(Path::to_path_buf)
            })
            .expect(
                "the workspace root: CARGO_MANIFEST_DIR two levels below it, or the executable \
                 under it (cargo test or cargo run set CARGO_MANIFEST_DIR)",
            )
    })
    .clone()
}

/// Whether `dir` is the workspace root: it holds `testdata/` and `Cargo.lock`.
fn is_workspace_root(dir: &Path) -> bool {
    dir.join("testdata").is_dir() && dir.join("Cargo.lock").is_file()
}

/// A path under `testdata/`.
pub fn testdata(relative: &str) -> PathBuf {
    workspace_root().join("testdata").join(relative)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Run alone by [`run_child`]: prints what `workspace_root()` gives in this process.
    #[test]
    #[ignore = "run as a child by the tests below"]
    fn print_workspace_root() {
        println!("WORKSPACE_ROOT={}", super::workspace_root().display());
    }

    /// This test binary run again for [`print_workspace_root`] alone, with `CARGO_MANIFEST_DIR`
    /// set to `manifest_dir` or removed: what it printed.
    fn run_child(manifest_dir: Option<&Path>) -> PathBuf {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args([
            "--exact",
            "tests::print_workspace_root",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        match manifest_dir {
            Some(dir) => child.env("CARGO_MANIFEST_DIR", dir),
            None => child.env_remove("CARGO_MANIFEST_DIR"),
        };
        let out = child.output().unwrap();
        // libtest may write a test's output to either stream.
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        let line = text
            .lines()
            .find_map(|line| line.split_once("WORKSPACE_ROOT=").map(|(_, root)| root))
            .unwrap_or_else(|| panic!("no root printed:\n{text}"));
        PathBuf::from(line.trim())
    }

    /// A folder laid out as this workspace is: `testdata/`, `Cargo.lock` and `crates/<name>`.
    fn fake_workspace(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir.join("testdata")).unwrap();
        std::fs::create_dir_all(dir.join("crates/some-crate")).unwrap();
        std::fs::write(dir.join("Cargo.lock"), "").unwrap();
        dir.join("crates/some-crate")
    }

    // covers: B-10
    // The library holds no path of the checkout it was compiled in (docs/spec/10-build-blocks.md:
    // a baked path ties every test that links it to one folder): the root is the one the running
    // test's package is in, so a build reused from another checkout reads this one's testdata.
    #[test]
    fn the_root_is_found_when_the_tests_run() {
        let here = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
        let real = here
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();

        // Another checkout's package: its root, not the one this library was compiled in.
        let other = tempfile::tempdir().unwrap();
        let package = fake_workspace(other.path());
        assert_eq!(run_child(Some(&package)), other.path());

        // A package folder with no workspace two levels up, or none at all: the root is found
        // above the running executable, where cargo's target folder is.
        let exe = std::env::current_exe().unwrap();
        if exe.starts_with(&real) {
            let bare = tempfile::tempdir().unwrap();
            assert_eq!(run_child(Some(bare.path())), real);
            assert_eq!(run_child(None), real);
        }
    }
}
