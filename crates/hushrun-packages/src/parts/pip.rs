//! The `pip` part: the files a requirements file with hashes names, as `pip download` writes them,
//! read through `PIP_FIND_LINKS`, a folder pip looks in beside its index. pip checks each file
//! against the requirements file's hashes itself. With the index reachable, pip takes an equal
//! file from the index instead, so the part is what an install reads when the index isn't.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::part::{Download, Env, Locked, Part, Reader, file_name, python_package, splits_safely};
use crate::store::VerifiedBy;

/// pip's variable for the folders it looks in beside its index.
pub const VARIABLE: &str = "PIP_FIND_LINKS";

/// The `pip` part.
pub struct Pip;

impl Part for Pip {
    fn name(&self) -> &'static str {
        "pip"
    }

    fn tool(&self) -> &'static str {
        "pip"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| is_requirements(path))
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        cache.is_dir().then(|| (cache.to_path_buf(), ""))
    }

    fn keep(&self, path: &str) -> bool {
        !path.contains('/') && python_package(path).is_some()
    }

    fn package(&self, path: &str) -> Option<String> {
        python_package(path)
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, dir: &Path, _checkout: &Path, env: Env<'_>) -> Reader {
        if env(VARIABLE).is_some() {
            return Reader::LeftAlone(format!("{VARIABLE} is set already"));
        }
        if let Some(file) = config_naming_find_links(env) {
            return Reader::LeftAlone(format!("find-links is set already ({})", file.display()));
        }
        if !splits_safely(dir) {
            return Reader::LeftAlone(format!(
                "the store's folder, {}, has a space, which {VARIABLE} can't hold",
                dir.display()
            ));
        }
        Reader::InPlace(vec![(VARIABLE.to_string(), OsString::from(dir))])
    }

    fn locked(&self, checkout: &Path, manifests: &[String]) -> Locked {
        let mut locked = Locked::default();
        for path in manifests {
            if let Ok(text) = fs::read_to_string(checkout.join(path)) {
                for hash in hashes(&text) {
                    locked.add(hash, None);
                }
            }
        }
        locked
    }

    fn download(&self, checkout: &Path, manifests: &[String], into: &Path) -> Download {
        let hashed: Vec<&String> = manifests
            .iter()
            .filter(|p| fs::read_to_string(checkout.join(p)).is_ok_and(|t| !hashes(&t).is_empty()))
            .collect();
        if hashed.is_empty() {
            return Some(Err(
                "no requirements file names its packages' hashes (--hash=sha256:…)".to_string(),
            ));
        }
        let python = if cfg!(windows) { "python" } else { "python3" };
        Some(Ok(hashed
            .into_iter()
            .map(|path| {
                [
                    python,
                    "-m",
                    "pip",
                    "download",
                    "--quiet",
                    "--disable-pip-version-check",
                    "--no-deps",
                    "--require-hashes",
                    "--requirement",
                ]
                .iter()
                .map(OsString::from)
                .chain([
                    OsString::from(path),
                    OsString::from("--dest"),
                    into.as_os_str().to_owned(),
                ])
                .collect()
            })
            .collect()))
    }
}

/// Whether `path` is a requirements file by its name: `requirements*.txt`, `*-requirements.txt`,
/// or a `.txt` file in a `requirements` folder.
fn is_requirements(path: &str) -> bool {
    let name = file_name(path);
    let in_folder = path
        .rsplit('/')
        .nth(1)
        .is_some_and(|parent| parent == "requirements");
    name.ends_with(".txt")
        && (name.starts_with("requirements") || name.ends_with("-requirements.txt") || in_folder)
}

/// The SHA-256 hashes a requirements file names (`--hash=sha256:<hex>`, or `--hash sha256:<hex>`).
pub fn hashes(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split(" #").next().unwrap_or(line);
        if line.trim_start().starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace().peekable();
        while let Some(word) = words.next() {
            let value = match word.strip_prefix("--hash") {
                Some("") => words.next(),
                Some(rest) => rest.strip_prefix('='),
                None => None,
            };
            if let Some(hex) = value.and_then(|v| v.strip_prefix("sha256:")) {
                out.push(hex);
            }
        }
    }
    out
}

/// A pip configuration file that names `find-links`, which `PIP_FIND_LINKS` would replace.
fn config_naming_find_links(env: Env<'_>) -> Option<PathBuf> {
    let var = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    // pip reads no configuration file at all when PIP_CONFIG_FILE is the null device.
    if var("PIP_CONFIG_FILE")
        .is_some_and(|f| f == Path::new(if cfg!(windows) { "nul" } else { "/dev/null" }))
    {
        return None;
    }
    let mut files: Vec<PathBuf> = Vec::new();
    files.extend(var("PIP_CONFIG_FILE"));
    files.extend(var("VIRTUAL_ENV").map(|v| v.join("pip.conf")));
    if cfg!(windows) {
        files.extend(var("APPDATA").map(|d| d.join("pip/pip.ini")));
        files.extend(var("PROGRAMDATA").map(|d| d.join("pip/pip.ini")));
    } else {
        let home = var("HOME");
        let config = var("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config")));
        files.extend(config.map(|c| c.join("pip/pip.conf")));
        files.extend(home.as_ref().map(|h| h.join(".pip/pip.conf")));
        files.extend(
            home.as_ref()
                .map(|h| h.join("Library/Application Support/pip/pip.conf")),
        );
        files.push(PathBuf::from("/etc/xdg/pip/pip.conf"));
        files.push(PathBuf::from("/etc/pip.conf"));
    }
    files.into_iter().find(|file| {
        fs::read_to_string(file).is_ok_and(|t| t.contains("find-links") || t.contains("find_links"))
    })
}
