//! \[ADR 0043\] hushrun itself, for a repository with no package.json. There the hook runs the
//! `hushrun` on `PATH`, and someone who ran `npx hushrun init` has none: npx's copy is in a cache,
//! on `PATH` only while npx runs. So init offers to install a release in a directory that is
//! already on `PATH`, one of the person's own, and edits no shell's startup file.
//!
//! Only the person's own directories count. A directory on `PATH` isn't always a place to keep a
//! program: a version manager's, such as `~/.nvm/versions/node/v24/bin` or a shim directory,
//! changes under it.

use std::path::{Path, PathBuf};

use hushrun_ci::Env;

use crate::cmd::update;
use crate::releases;

/// Whether `hushrun` is on `PATH`, npx's own directories apart.
pub fn on_path() -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| {
            let text = dir.to_string_lossy();
            !text.contains("_npx") && !text.contains("node_modules")
        })
        .any(|dir| dir.join("hushrun").exists() || dir.join("hushrun.exe").exists())
}

/// The directory init installs hushrun in, when it should. Nothing is written to find out.
///
/// Windows isn't one of those times: its installer is the way there, and nothing tests this on
/// Windows yet. Nor is another install that isn't on `PATH`, whose receipt `hushrun update` needs.
pub fn directory() -> Option<PathBuf> {
    if cfg!(windows)
        || releases::target().is_none()
        || hushrun_ci::context(&Env::from_process()).in_ci
        || on_path()
        || !update::came_with_npm()
        || update::installed_elsewhere()
    {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    let listed: Vec<PathBuf> = std::env::split_paths(&path).collect();
    pick(&own_directories(), &listed)
}

/// The first of `wanted` that `listed` holds and that can take a file.
fn pick(wanted: &[PathBuf], listed: &[PathBuf]) -> Option<PathBuf> {
    wanted
        .iter()
        .find(|dir| listed.iter().any(|on| same(on, dir)) && can_write(dir))
        .cloned()
}

/// The person's own directories for programs, the one hushrun's installer uses first: a release
/// installed by hand later lands on the same file.
fn own_directories() -> Vec<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let home = dirs::home_dir();
    let under_home = |relative: &str| home.as_ref().map(|home| home.join(relative));
    let mut found = Vec::new();
    found.extend(var("CARGO_HOME").map(|dir| dir.join("bin")));
    found.extend(under_home(".cargo/bin"));
    found.extend(var("XDG_BIN_HOME"));
    found.extend(under_home(".local/bin"));
    found.extend(under_home("bin"));
    found.extend(var("GOBIN"));
    if let Some(paths) = var("GOPATH") {
        found.extend(std::env::split_paths(&paths).map(|dir| dir.join("bin")));
    }
    found.extend(under_home("go/bin"));
    found
}

fn same(a: &Path, b: &Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .is_ok_and(|real| std::fs::canonicalize(b).is_ok_and(|other| real == other))
}

/// Whether `dir` looks like it can take a file, without writing one, since a dry run writes
/// nothing. Being wrong costs the warning a failed install prints. One that isn't there yet is
/// made when hushrun goes in it.
fn can_write(dir: &Path) -> bool {
    match std::fs::metadata(dir) {
        Ok(meta) => meta.is_dir() && !meta.permissions().readonly() && owned(&meta),
        Err(err) => err.kind() == std::io::ErrorKind::NotFound,
    }
}

#[cfg(unix)]
fn owned(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    meta.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn owned(_: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_counts_only_when_it_is_on_path_and_takes_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cargo = root.join("cargo/bin");
        let local = root.join("local/bin");
        let other = root.join("other/bin");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let wanted = [cargo.clone(), local.clone()];

        // None of them is on PATH.
        assert_eq!(pick(&wanted, std::slice::from_ref(&other)), None);
        // In the order wanted, whatever PATH's: one that isn't made yet is fine.
        assert_eq!(
            pick(&wanted, &[other.clone(), local.clone(), cargo.clone()]),
            Some(cargo.clone())
        );
        assert_eq!(pick(&wanted, std::slice::from_ref(&local)), Some(local));
        // A file of that name isn't a directory.
        std::fs::create_dir_all(cargo.parent().unwrap()).unwrap();
        std::fs::write(&cargo, "").unwrap();
        assert_eq!(pick(&wanted, std::slice::from_ref(&cargo)), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_directory_takes_no_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(!can_write(&bin));
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(can_write(&bin));
    }
}
