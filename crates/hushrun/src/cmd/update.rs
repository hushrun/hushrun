//! `hushrun update` (C-20, ADR 0035): replaces this hushrun with the newest release when
//! hushrun's installer installed it, the way the installer would: the release's archive for this
//! platform, checked against the SHA-256 published beside it, unpacked with the system's `tar`
//! as the shell installer does, into the directory the installer's receipt names; the receipt
//! then says the new version. hushrun installed any other way, such as with npm, from source or
//! in CI, is left alone, and update says how to update it instead.
//!
//! \[ADR 0043\] `hushrun init` installs a release the same way, and leaves a receipt the installer
//! would have left, so that update keeps that copy up to date too.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use semver::Version;
use serde_json::{Value, json};

use crate::cli::GlobalOpts;
use crate::exit::{CmdResult, Exit, Failure};
use crate::releases::{self, Releases};

/// The binary's file name.
const EXE: &str = if cfg!(windows) {
    "hushrun.exe"
} else {
    "hushrun"
};
/// The installer's receipt, in its receipt directory.
const RECEIPT: &str = "hushrun-receipt.json";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LATEST_TIMEOUT: Duration = Duration::from_secs(30);
/// An archive is about 8 MB.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const ARCHIVE_LIMIT: u64 = 256 << 20;
const CHECKSUM_LIMIT: u64 = 4 << 10;

pub fn run(global: &GlobalOpts) -> CmdResult {
    let current = releases::current();
    let base = releases::base().map_err(Failure::usage)?;
    let releases = Releases::new(&base, CONNECT_TIMEOUT)?;
    let latest = releases
        .latest(LATEST_TIMEOUT)
        .context("can't tell which hushrun release is the newest")?;
    match current.cmp_precedence(&latest) {
        Ordering::Equal => {
            println!("hushrun {current} is the newest release.");
            return Ok(Exit::Ok);
        }
        Ordering::Greater => {
            println!("hushrun {current} is newer than the newest release, {latest}.");
            return Ok(Exit::Ok);
        }
        Ordering::Less => {}
    }
    let exe = std::env::current_exe()
        .and_then(fs::canonicalize)
        .context("finding this hushrun's file")?;
    match install_of(&exe, &receipt_paths()) {
        Install::Installer(receipt) => update(global, &releases, &receipt, &current, &latest),
        Install::Npm => Err(Failure::usage(anyhow!(
            "hushrun {latest} is out, and this hushrun ({current}, at {}) came with npm: update the hushrun package with the package manager that installed it, such as npm install hushrun@latest",
            shown(&exe)
        ))),
        Install::Other(named) => {
            let instead = match named {
                Some(binary) => format!(
                    "hushrun's installer put hushrun at {}: run `{} update`",
                    binary.display(),
                    binary.display()
                ),
                None => format!(
                    "install hushrun {latest} the way this one was installed, or with hushrun's installer, whose installs hushrun update keeps up to date: {}",
                    releases::installer_command()
                ),
            };
            Err(Failure::usage(anyhow!(
                "hushrun {latest} is out, and this hushrun ({current}, at {}) wasn't installed by hushrun's installer, so update leaves it alone: {instead}",
                shown(&exe)
            )))
        }
    }
}

/// How this hushrun was installed, as far as update can tell.
enum Install {
    /// By hushrun's installer, whose receipt names this binary.
    Installer(Receipt),
    /// By a JavaScript package manager (npm, pnpm, Yarn, Bun): it is in a `node_modules`
    /// directory.
    Npm,
    /// Some other way: built from source, copied by hand, installed by CI, in the container
    /// image. With the binary an installer's receipt names instead, if one does.
    Other(Option<PathBuf>),
}

/// The install receipt of hushrun's installer (dist's).
struct Receipt {
    path: PathBuf,
    json: Value,
    /// The binary it installed, which is this one, as the receipt spells its path.
    binary: PathBuf,
}

/// Where hushrun's installers leave their receipt: the shell installer in
/// `$XDG_CONFIG_HOME/hushrun`, else `~/.config/hushrun`; the PowerShell installer in
/// `%XDG_CONFIG_HOME%\hushrun`, else `%LOCALAPPDATA%\hushrun`, where the shell installer leaves
/// it too on Windows.
fn receipt_paths() -> Vec<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let mut found: Vec<PathBuf> = var("XDG_CONFIG_HOME").into_iter().collect();
    if cfg!(windows) {
        found.extend(var("LOCALAPPDATA").or_else(dirs::data_local_dir));
    } else if found.is_empty() {
        found.extend(dirs::home_dir().map(|home| home.join(".config")));
    }
    found
        .into_iter()
        .map(|dir| dir.join(hushrun_core::PRODUCT).join(RECEIPT))
        .collect()
}

/// Whether this hushrun came with a JavaScript package manager, as under `npx`.
pub(crate) fn came_with_npm() -> bool {
    std::env::current_exe()
        .and_then(fs::canonicalize)
        .is_ok_and(|exe| matches!(install_of(&exe, &[]), Install::Npm))
}

/// Whether a receipt names a binary that is still there: another install, which isn't on `PATH`
/// or it would have been found, and which a new receipt would take away from update.
pub(crate) fn installed_elsewhere() -> bool {
    receipt_paths().iter().any(|path| {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|json| binaries(&json).iter().any(|binary| binary.exists()))
    })
}

/// How `exe`, this binary's canonical path, was installed, as the receipts at `receipts` tell.
fn install_of(exe: &Path, receipts: &[PathBuf]) -> Install {
    if exe
        .components()
        .any(|component| component.as_os_str() == "node_modules")
    {
        return Install::Npm;
    }
    let mut named = None;
    for path in receipts {
        let Some(json) = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            continue;
        };
        for binary in binaries(&json) {
            if fs::canonicalize(&binary).is_ok_and(|found| same(&found, exe)) {
                return Install::Installer(Receipt {
                    path: path.clone(),
                    json,
                    binary,
                });
            }
            named.get_or_insert(binary);
        }
    }
    Install::Other(named)
}

/// The binary a receipt of hushrun's installer names, where its layout puts it; nothing for
/// another program's receipt.
fn binaries(json: &Value) -> Vec<PathBuf> {
    let source = &json["source"];
    let ours = source["app_name"] == "hushrun"
        && source["owner"] == "hushrun"
        && source["name"] == "hushrun"
        && source["release_type"] == "github"
        && json["binaries"]
            .as_array()
            .is_some_and(|names| names.iter().any(|name| name == EXE));
    let Some(prefix) = json["install_prefix"]
        .as_str()
        .filter(|prefix| ours && !prefix.is_empty())
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    match json["install_layout"].as_str() {
        Some("flat") => vec![prefix.join(EXE)],
        Some("cargo-home" | "hierarchical") => vec![prefix.join("bin").join(EXE)],
        // Receipts of older installers record no layout.
        _ => vec![prefix.join("bin").join(EXE), prefix.join(EXE)],
    }
}

/// A path as people write it: without the `\\?\` that canonical paths start with on Windows.
fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{share}");
    }
    if let Some(local) = text.strip_prefix(r"\\?\") {
        return local.to_string();
    }
    text
}

/// Whether two canonical paths are the same file's: on Windows, whatever their case.
fn same(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Installs release `latest` over the binary `receipt` names, and records it there.
fn update(
    global: &GlobalOpts,
    releases: &Releases,
    receipt: &Receipt,
    current: &Version,
    latest: &Version,
) -> CmdResult {
    let dir = receipt
        .binary
        .parent()
        .ok_or_else(|| anyhow!("{} is in no directory", receipt.binary.display()))?;
    // What the last update on Windows left: the binary that was running then.
    if cfg!(windows) {
        let _ = fs::remove_file(old_of(&receipt.binary));
    }

    // Beside the binary, as the installer works, so that the new one takes its place with a
    // rename on the same file system. Dropped, it is removed with what is left in it.
    let work = tempfile::Builder::new()
        .prefix(".hushrun-update-")
        .tempdir_in(dir)
        .with_context(|| {
            format!(
                "can't write in {}: run hushrun update as the user who installed hushrun there",
                dir.display()
            )
        })?;

    let new = fetch(global, releases, latest, work.path())?;
    match replace(&new, &receipt.binary) {
        Ok(()) => {}
        Err(NotReplaced::Unchanged(err)) => {
            return Err(anyhow!(err)
                .context(format!(
                    "replacing {}: hushrun is unchanged",
                    receipt.binary.display()
                ))
                .into());
        }
        // \[ADR 0061\] Not "unchanged": the only hushrun left is the one moved aside.
        Err(NotReplaced::MovedAside { old, error }) => {
            return Err(anyhow!(error)
                .context(format!(
                    "replacing {}: hushrun is now {}, and couldn't be moved back: rename it to {} to run it",
                    receipt.binary.display(),
                    old.display(),
                    receipt.binary.display()
                ))
                .into());
        }
    }
    let recorded = record(receipt, latest);

    println!(
        "Updated hushrun {current} to {latest} in {}.",
        receipt.binary.display()
    );
    println!("Release notes: {}", releases.page(latest));
    if let Err(err) = recorded {
        eprintln!(
            "warning: the installer's receipt, {}, still says {current}: {err:#}",
            receipt.path.display()
        );
    }
    Ok(Exit::Ok)
}

/// \[ADR 0043\] Installs the newest release in `dir`, a directory on `PATH`, and leaves the receipt
/// the installer would leave with that prefix, so that `hushrun update` takes it from there. A
/// receipt that can't be written costs a warning: the binary is there, and it works.
pub(crate) fn install_newest(global: &GlobalOpts, dir: &Path) -> anyhow::Result<PathBuf> {
    let base = releases::base()?;
    let releases = Releases::new(&base, CONNECT_TIMEOUT)?;
    let latest = releases
        .latest(LATEST_TIMEOUT)
        .context("can't tell which hushrun release is the newest")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let work = tempfile::Builder::new()
        .prefix(".hushrun-update-")
        .tempdir_in(dir)
        .with_context(|| format!("can't write in {}", dir.display()))?;
    let new = fetch(global, &releases, &latest, work.path())?;
    let binary = dir.join(EXE);
    if binary.exists() {
        bail!("{} is there already", binary.display());
    }
    fs::rename(&new, &binary).with_context(|| format!("writing {}", binary.display()))?;
    if let Err(err) = leave_receipt(dir, &latest) {
        eprintln!(
            "warning: hushrun update won't know {}: {err:#}",
            binary.display()
        );
    }
    Ok(binary)
}

/// Writes the receipt of hushrun `version` in `dir`.
fn leave_receipt(dir: &Path, version: &Version) -> anyhow::Result<()> {
    let path = receipt_paths()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no directory for its receipt"))?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} is in no directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let receipt = json!({
        "binaries": [EXE],
        "binary_aliases": {},
        "cdylibs": [],
        "cstaticlibs": [],
        "install_layout": "flat",
        "install_prefix": dir.to_string_lossy(),
        "modify_path": false,
        "provider": {"source": "hushrun init", "version": env!("CARGO_PKG_VERSION")},
        "source": {
            "app_name": "hushrun",
            "name": "hushrun",
            "owner": "hushrun",
            "release_type": "github",
        },
        "version": version.to_string(),
    });
    write_receipt(&path, &receipt).with_context(|| format!("writing {}", path.display()))
}

/// The binary of release `latest` for this machine, in `work`: its archive, checked against the
/// SHA-256 published beside it, unpacked, and asked its version.
fn fetch(
    global: &GlobalOpts,
    releases: &Releases,
    latest: &Version,
    work: &Path,
) -> anyhow::Result<PathBuf> {
    let target = releases::target().ok_or_else(|| {
        anyhow!(
            "hushrun has no release for {} on {}",
            std::env::consts::ARCH,
            std::env::consts::OS
        )
    })?;
    let archive = releases::archive(target);
    if global.quiet == 0 {
        eprintln!("Downloading hushrun {latest} for {target}.");
    }
    let checksums = releases.download(
        latest,
        &format!("{archive}.sha256"),
        CHECKSUM_LIMIT,
        DOWNLOAD_TIMEOUT,
    )?;
    let expected = releases::checksum_for(&checksums, &archive)?;
    let bytes = releases.download(latest, &archive, ARCHIVE_LIMIT, DOWNLOAD_TIMEOUT)?;
    let actual = releases::sha256_hex(&bytes);
    if actual != expected {
        bail!(
            "{archive} has SHA-256 {actual}, but the release's {archive}.sha256 says {expected}: hushrun is unchanged"
        );
    }
    let packed = work.join(&archive);
    fs::write(&packed, &bytes).with_context(|| format!("writing {}", packed.display()))?;
    let unpacked = work.join("unpacked");
    fs::create_dir(&unpacked).with_context(|| format!("creating {}", unpacked.display()))?;
    extract(&packed, &unpacked)?;
    let new = find(&unpacked).ok_or_else(|| anyhow!("{archive} holds no {EXE}"))?;
    check(&new, latest)?;
    Ok(new)
}

/// Unpacks `archive` into `into` with the system's `tar`, as the shell installer does: bsdtar,
/// on macOS and Windows, reads `.tar.xz` and `.zip`; GNU tar reads `.tar.xz` with `xz`.
fn extract(archive: &Path, into: &Path) -> anyhow::Result<()> {
    let tar = tar();
    let output = Command::new(&tar)
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {} to unpack {}", tar.display(), archive.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let why = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("it failed");
    let hint = if cfg!(windows) {
        ""
    } else {
        " (GNU tar needs xz for it: install xz-utils, or xz)"
    };
    bail!(
        "{} couldn't unpack {}: {why}{hint}",
        tar.display(),
        archive.display()
    )
}

/// Windows' own tar, which reads `.zip` archives, rather than another one earlier on `PATH`,
/// such as Git's GNU tar, which doesn't; elsewhere, the `tar` on `PATH`.
fn tar() -> PathBuf {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        return PathBuf::from(root).join("System32").join("tar.exe");
    }
    PathBuf::from("tar")
}

/// The binary in an unpacked archive: at its top, as `.zip` archives hold it, or in its
/// directory, as `.tar.xz` ones do. A regular file only, never a link.
fn find(dir: &Path) -> Option<PathBuf> {
    let regular = |path: &Path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file());
    let top = dir.join(EXE);
    if regular(&top) {
        return Some(top);
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(EXE))
        .find(|path| regular(path))
}

/// Whether the new binary runs here and is release `latest`: `--version` prints
/// `hushrun <latest>`. A binary for another platform, or another release, stops the update
/// before anything changes.
fn check(new: &Path, latest: &Version) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(new, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("making {} executable", new.display()))?;
    }
    let output = Command::new(new)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running the new hushrun, {}", new.display()))?;
    let said = String::from_utf8_lossy(&output.stdout);
    let said = said.trim();
    let expected = format!("hushrun {latest}");
    if !output.status.success() || said != expected {
        bail!(
            "the release's binary says {said:?} to --version, not {expected:?}: hushrun is unchanged"
        );
    }
    Ok(())
}

/// What a replacement that failed left of the installed binary.
#[derive(Debug)]
enum NotReplaced {
    /// It is where it was.
    Unchanged(io::Error),
    /// \[ADR 0061\] It was moved aside to `old`, and couldn't be moved back: `old` is the only
    /// hushrun there.
    MovedAside { old: PathBuf, error: io::Error },
}

/// Puts `new` in place of `installed`, the running binary. A rename within one directory
/// replaces it at once, and the running hushrun keeps the file it started from.
#[cfg(not(windows))]
fn replace(new: &Path, installed: &Path) -> Result<(), NotReplaced> {
    fs::rename(new, installed).map_err(NotReplaced::Unchanged)
}

/// Puts `new` in place of `installed`, the running binary. Windows doesn't let a running
/// program's file be replaced or deleted, but it lets it be renamed: the running hushrun moves
/// aside, as `hushrun.exe.old`, which the next update removes.
#[cfg(windows)]
fn replace(new: &Path, installed: &Path) -> Result<(), NotReplaced> {
    let old = old_of(installed);
    let _ = fs::remove_file(&old);
    swap(new, installed, &old, &|from, to| {
        retrying(|| fs::rename(from, to))
    })
}

/// Moves `installed` aside to `old`, then `new` to `installed`; when the second move fails, moves
/// `installed` back, and says whether that worked.
#[cfg_attr(not(windows), allow(dead_code))]
fn swap(
    new: &Path,
    installed: &Path,
    old: &Path,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), NotReplaced> {
    rename(installed, old).map_err(NotReplaced::Unchanged)?;
    let Err(error) = rename(new, installed) else {
        return Ok(());
    };
    match rename(old, installed) {
        Ok(()) => Err(NotReplaced::Unchanged(error)),
        Err(_) => Err(NotReplaced::MovedAside {
            old: old.to_path_buf(),
            error,
        }),
    }
}

/// A rename, tried again for about a second while something, such as an antivirus scanning the
/// new file, holds it.
#[cfg(windows)]
fn retrying(rename: impl Fn() -> io::Result<()>) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match rename() {
            Err(_) if tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            done => return done,
        }
    }
}

/// Where Windows' running binary moves aside during an update: `hushrun.exe.old`, which isn't a
/// command.
fn old_of(installed: &Path) -> PathBuf {
    let mut name = installed.as_os_str().to_os_string();
    name.push(".old");
    PathBuf::from(name)
}

/// Records `latest` as the receipt's version, as the installer would have written it.
fn record(receipt: &Receipt, latest: &Version) -> anyhow::Result<()> {
    let mut json = receipt.json.clone();
    let fields = json
        .as_object_mut()
        .ok_or_else(|| anyhow!("it isn't a JSON object"))?;
    fields.insert("version".into(), Value::String(latest.to_string()));
    write_receipt(&receipt.path, &json)
}

/// Writes a receipt through a temporary file beside it.
fn write_receipt(path: &Path, json: &Value) -> anyhow::Result<()> {
    let mut text = serde_json::to_string(json)?;
    text.push('\n');
    let temp = path.with_extension("json.tmp");
    let written = fs::write(&temp, text).and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    Ok(written?)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-20
    #[test]
    fn a_swap_that_cant_move_the_binary_back_says_where_it_is() {
        use std::cell::Cell;
        let dir = tempfile::tempdir().unwrap();
        let (new, installed, old) = (
            dir.path().join("new"),
            dir.path().join(EXE),
            dir.path().join("old"),
        );
        // A rename that fails on the calls `fails` names, counting from 1.
        let failing = |fails: &'static [usize]| {
            let calls = Cell::new(0);
            move |a: &Path, b: &Path| {
                calls.set(calls.get() + 1);
                match fails.contains(&calls.get()) {
                    true => Err(io::Error::other("held by another program")),
                    false => fs::rename(a, b),
                }
            }
        };
        let fresh = || {
            let _ = fs::remove_file(&old);
            fs::write(&new, "new").unwrap();
            fs::write(&installed, "running").unwrap();
        };

        fresh();
        assert!(swap(&new, &installed, &old, &|a, b| fs::rename(a, b)).is_ok());
        assert_eq!(fs::read_to_string(&installed).unwrap(), "new");

        // It can't move aside: nothing moved.
        fresh();
        let out = swap(&new, &installed, &old, &failing(&[1]));
        assert!(matches!(out, Err(NotReplaced::Unchanged(_))), "{out:?}");
        assert_eq!(fs::read_to_string(&installed).unwrap(), "running");

        // The new one can't go in, and the old one goes back.
        fresh();
        let out = swap(&new, &installed, &old, &failing(&[2]));
        assert!(matches!(out, Err(NotReplaced::Unchanged(_))), "{out:?}");
        assert_eq!(fs::read_to_string(&installed).unwrap(), "running");

        // Neither: the only hushrun is the one moved aside, and the error says so.
        fresh();
        match swap(&new, &installed, &old, &failing(&[2, 3])) {
            Err(NotReplaced::MovedAside { old: aside, .. }) => assert_eq!(aside, old),
            other => panic!("{other:?}"),
        }
        assert!(!installed.exists());
        assert_eq!(fs::read_to_string(&old).unwrap(), "running");
    }

    fn receipt(prefix: &Path, layout: &str) -> Value {
        json!({
            "binaries": [EXE],
            "binary_aliases": {},
            "cdylibs": [],
            "cstaticlibs": [],
            "install_layout": layout,
            "install_prefix": prefix.to_string_lossy(),
            "modify_path": true,
            "provider": {"source": "cargo-dist", "version": "0.32.0"},
            "source": {"app_name": "hushrun", "name": "hushrun", "owner": "hushrun", "release_type": "github"},
            "version": "0.1.0"
        })
    }

    /// A binary at `dir/<relative>`, by its canonical path.
    fn binary(dir: &Path, relative: &str) -> PathBuf {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "binary").unwrap();
        fs::canonicalize(path).unwrap()
    }

    // covers: C-20
    #[test]
    fn the_installers_receipt_names_the_binary_it_installed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let installed = binary(&home, &format!(".cargo/bin/{EXE}"));
        let path = dir.path().join("config/hushrun/hushrun-receipt.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let receipts = [
            dir.path().join("elsewhere/hushrun-receipt.json"),
            path.clone(),
        ];

        // No receipt: another install, and none named.
        assert!(matches!(
            install_of(&installed, &receipts),
            Install::Other(None)
        ));
        // The cargo-home layout, as hushrun's installers write it: the binary is in bin/.
        fs::write(
            &path,
            receipt(&home.join(".cargo"), "cargo-home").to_string(),
        )
        .unwrap();
        match install_of(&installed, &receipts) {
            Install::Installer(found) => {
                assert_eq!(found.binary, home.join(format!(".cargo/bin/{EXE}")));
                assert_eq!(found.path, path);
                assert_eq!(found.json["version"], "0.1.0");
            }
            _ => panic!("the receipt names this binary"),
        }
        // Another copy of hushrun is another install: update names the installed one.
        let copy = binary(dir.path(), &format!("bin/{EXE}"));
        match install_of(&copy, &receipts) {
            Install::Other(Some(named)) => assert!(named.ends_with(format!(".cargo/bin/{EXE}"))),
            _ => panic!("another install"),
        }
        // The flat layout, and receipts without one.
        let flat = binary(dir.path(), &format!("flat/{EXE}"));
        fs::write(&path, receipt(&dir.path().join("flat"), "flat").to_string()).unwrap();
        assert!(matches!(
            install_of(&flat, &receipts),
            Install::Installer(_)
        ));
        let mut legacy = receipt(&dir.path().join("flat"), "x");
        legacy.as_object_mut().unwrap().remove("install_layout");
        fs::write(&path, legacy.to_string()).unwrap();
        assert!(matches!(
            install_of(&flat, &receipts),
            Install::Installer(_)
        ));
        // Another program's receipt, or one that doesn't parse, names nothing.
        let mut other = receipt(&home.join(".cargo"), "cargo-home");
        other["source"]["app_name"] = json!("other");
        fs::write(&path, other.to_string()).unwrap();
        assert!(matches!(
            install_of(&installed, &receipts),
            Install::Other(None)
        ));
        fs::write(&path, "{\"install_prefix\": \"").unwrap();
        assert!(matches!(
            install_of(&installed, &receipts),
            Install::Other(None)
        ));
        // npm, pnpm, Yarn and Bun keep it in node_modules, whatever the receipts say.
        fs::write(
            &path,
            receipt(&home.join(".cargo"), "cargo-home").to_string(),
        )
        .unwrap();
        let npm = binary(
            dir.path(),
            &format!("app/node_modules/@hushrun/cli-darwin-arm64/bin/{EXE}"),
        );
        assert!(matches!(install_of(&npm, &receipts), Install::Npm));
    }

    // covers: C-20
    #[test]
    fn the_receipt_then_says_the_new_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hushrun-receipt.json");
        let json = receipt(dir.path(), "cargo-home");
        fs::write(&path, json.to_string()).unwrap();
        let receipt = Receipt {
            path: path.clone(),
            json: json.clone(),
            binary: dir.path().join(EXE),
        };
        record(&receipt, &Version::parse("0.2.0").unwrap()).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let mut expected = json;
        expected["version"] = json!("0.2.0");
        assert_eq!(written, expected);
        assert!(fs::read_to_string(&path).unwrap().ends_with("}\n"));
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary file"
        );
    }

    #[test]
    fn an_unpacked_archive_holds_one_binary() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find(dir.path()), None);
        // A .tar.xz archive holds it in a directory.
        let nested = dir.path().join("hushrun-x86_64-unknown-linux-musl");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("README.md"), "").unwrap();
        fs::create_dir(nested.join(EXE)).unwrap();
        assert_eq!(find(dir.path()), None, "a directory isn't the binary");
        fs::remove_dir(nested.join(EXE)).unwrap();
        fs::write(nested.join(EXE), "binary").unwrap();
        assert_eq!(find(dir.path()), Some(nested.join(EXE)));
        // A .zip archive holds it at the top.
        fs::write(dir.path().join(EXE), "binary").unwrap();
        assert_eq!(find(dir.path()), Some(dir.path().join(EXE)));
        assert!(
            old_of(Path::new("bin/hushrun.exe")).ends_with("hushrun.exe.old"),
            "never a command"
        );
        assert_eq!(
            shown(Path::new(r"\\?\C:\Users\ana\.cargo\bin\hushrun.exe")),
            r"C:\Users\ana\.cargo\bin\hushrun.exe"
        );
        assert_eq!(
            shown(Path::new(r"\\?\UNC\server\share\hushrun.exe")),
            r"\\server\share\hushrun.exe"
        );
        assert_eq!(
            shown(Path::new("/usr/local/bin/hushrun")),
            "/usr/local/bin/hushrun"
        );
    }
}
