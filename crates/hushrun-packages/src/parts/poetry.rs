//! The `poetry` part: `artifacts/` of a Poetry cache, the wheels and source archives Poetry
//! downloaded, unchanged, each in the folder Poetry names by its URL and hash. Poetry reads no
//! second cache, so `hushrun packages exec`, `env` and `restore` seed its own: each file
//! `poetry.lock` still names, checked against the lockfile's hash, is copied into Poetry's cache,
//! where Poetry checks it against the lockfile again before it installs it. A file Poetry built there from a source archive is no download,
//! and the lockfile names none, so a bake leaves it out.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::part::{Admits, Env, Locked, Part, Reader, file_name, python_package};
use crate::store::VerifiedBy;

/// The variable that names Poetry's cache folder, `cache-dir`.
pub const VARIABLE: &str = "POETRY_CACHE_DIR";
/// The folder of a Poetry cache that holds its downloads, and the part's folder of them.
pub const ARTIFACTS: &str = "artifacts";

/// The `poetry` part.
pub struct Poetry;

impl Part for Poetry {
    fn name(&self) -> &'static str {
        "poetry"
    }

    fn tool(&self) -> &'static str {
        "Poetry"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| file_name(path) == "poetry.lock")
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let root = cache.join(ARTIFACTS);
        root.is_dir().then_some((root, ARTIFACTS))
    }

    fn keep(&self, path: &str) -> bool {
        // <2 hex>/<2 hex>/<2 hex>/<58 hex>/<file>
        let parts: Vec<&str> = path.split('/').collect();
        let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
        matches!(parts.as_slice(), [a, b, c, d, file]
            if hex(a, 2) && hex(b, 2) && hex(c, 2) && hex(d, 58) && python_package(file).is_some())
    }

    fn package(&self, path: &str) -> Option<String> {
        python_package(path)
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, _dir: &Path, checkout: &Path, env: Env<'_>) -> Reader {
        match cache_dir(checkout, env) {
            Ok(_) => Reader::Seed,
            Err(why) => Reader::LeftAlone(why),
        }
    }

    fn seed_into(&self, checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        Ok((cache_dir(checkout, env)?.join(ARTIFACTS), ARTIFACTS))
    }

    fn admit(&self, checkout: &Path, manifests: &[String]) -> Admits {
        let locked = self.lockfile(checkout, manifests).unwrap_or_default();
        Box::new(move |path, bytes| locked.admit(path, bytes))
    }

    fn lockfile(&self, checkout: &Path, manifests: &[String]) -> Option<Locked> {
        let mut locked = Locked::default();
        for path in manifests {
            if let Ok(text) = fs::read_to_string(checkout.join(path)) {
                for (file, hash) in files(&text) {
                    locked.add(&hash, Some(&file));
                }
            }
        }
        Some(locked)
    }

    fn fetch_vars(
        &self,
        cache: &Path,
        _env: Env<'_>,
    ) -> Result<Vec<(&'static str, OsString)>, String> {
        Ok(vec![(VARIABLE, cache.as_os_str().to_owned())])
    }
}

/// The files `poetry.lock` names with their SHA-256: each `{file = "…", hash = "sha256:…"}`.
pub fn files(text: &str) -> Vec<(String, String)> {
    let quoted = |inline: &str, key: &str| -> Option<String> {
        let value = &inline[inline.find(key)? + key.len()..];
        Some(value[..value.find('"')?].to_string())
    };
    text.split("{file = \"")
        .skip(1)
        .filter_map(|entry| {
            let inline = format!("file = \"{}", &entry[..entry.find('}')?]);
            Some((
                quoted(&inline, "file = \"")?,
                quoted(&inline, "hash = \"sha256:")?,
            ))
        })
        .collect()
}

/// Poetry's cache folder for the run whose variables are `env`, in `checkout`, as Poetry finds
/// it: `POETRY_CACHE_DIR`, else the platform's. An error when the person names another in a
/// configuration file, which hushrun doesn't read.
pub fn cache_dir(checkout: &Path, env: Env<'_>) -> Result<PathBuf, String> {
    let var = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = var(VARIABLE) {
        return if dir.is_absolute() {
            Ok(dir)
        } else {
            Err(format!("{VARIABLE} isn't an absolute path"))
        };
    }
    let home = var("HOME");
    let config = var("POETRY_CONFIG_DIR").or_else(|| {
        if cfg!(windows) {
            var("APPDATA").map(|d| d.join("pypoetry"))
        } else if cfg!(target_os = "macos") {
            home.as_ref()
                .map(|h| h.join("Library/Application Support/pypoetry"))
        } else {
            var("XDG_CONFIG_HOME")
                .or_else(|| home.as_ref().map(|h| h.join(".config")))
                .map(|c| c.join("pypoetry"))
        }
    });
    for file in [
        Some(checkout.join("poetry.toml")),
        config.map(|c| c.join("config.toml")),
    ]
    .into_iter()
    .flatten()
    {
        if fs::read_to_string(&file).is_ok_and(|t| t.contains("cache-dir")) {
            return Err(format!("cache-dir is set already ({})", file.display()));
        }
    }
    let cache = if cfg!(windows) {
        var("LOCALAPPDATA").map(|d| d.join("pypoetry").join("Cache"))
    } else if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Caches/pypoetry"))
    } else {
        var("XDG_CACHE_HOME")
            .or_else(|| home.map(|h| h.join(".cache")))
            .map(|c| c.join("pypoetry"))
    };
    cache.ok_or_else(|| "no home folder to find Poetry's cache in".to_string())
}
