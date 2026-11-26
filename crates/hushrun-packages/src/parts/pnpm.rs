//! The `pnpm` part: pnpm's content-addressed store (`v<N>/files/…` and `v<N>/index/…`), seeded
//! into the store the person's pnpm uses (K-10, mechanism C, K-18). Never read in place: pnpm
//! writes into its store as it installs, and a lockfile that changed since the bake names
//! packages the part lacks. Never pnpm's global virtual store: `node_modules` would then link out
//! of the checkout, and a `tsc/1` key holds every path `tsc` looked at.
//!
//! The lockfile's `integrity` is a hash of a package's tarball, which pnpm's store doesn't keep:
//! it keeps the tarball's files, each named by its own SHA-512, and an index per package that
//! lists them. Nothing in `pnpm-lock.yaml` hashes an index, so the part is verified by a
//! signature, as Gradle's and Maven's are; pnpm itself checks every file against its index as it
//! links it into `node_modules`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::part::{Env, Part, Reader, file_name};
use crate::store::VerifiedBy;

/// The part's folder under which pnpm's store is, as `store-dir` names it.
pub const STORE: &str = "store";
/// The variable pnpm reads `store-dir` from.
pub const STORE_DIR: &str = "npm_config_store_dir";

/// The `pnpm` part.
pub struct Pnpm;

/// Whether `name` is one of pnpm's store versions: `v` and digits.
fn is_version(name: &str) -> bool {
    name.strip_prefix('v')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

impl Part for Pnpm {
    fn name(&self) -> &'static str {
        "pnpm"
    }

    fn tool(&self) -> &'static str {
        "pnpm"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| file_name(path) == "pnpm-lock.yaml")
            .cloned()
            .collect()
    }

    /// `cache` is a `store-dir`: the folder that holds `v10/` (`pnpm store path`'s parent).
    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let holds_a_store = std::fs::read_dir(cache).ok()?.flatten().any(|e| {
            e.file_name().to_str().is_some_and(is_version) && e.path().join("files").is_dir()
        });
        holds_a_store.then(|| (cache.to_path_buf(), STORE))
    }

    /// Only the files and the indexes of a store's version: not `tmp/`, not `projects/` (links
    /// to the checkouts that installed from it), not a lock.
    fn keep(&self, path: &str) -> bool {
        let mut parts = path.split('/');
        let (Some(version), Some(kind), Some(_)) = (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        is_version(version) && matches!(kind, "files" | "index") && !path.ends_with(".lock")
    }

    /// `v10/index/<2 hex>/<62 hex>-<name>@<version>.json`: the package, as pnpm writes it (a
    /// scope's `/` as `+`).
    fn package(&self, path: &str) -> Option<String> {
        let mut parts = path.split('/');
        is_version(parts.next()?).then_some(())?;
        (parts.next()? == "index").then_some(())?;
        parts.next()?;
        let (_, package) = parts.next()?.strip_suffix(".json")?.split_once('-')?;
        parts.next().is_none().then(|| package.to_string())
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Signature
    }

    fn reader(&self, _dir: &Path, _checkout: &Path, _env: Env<'_>) -> Reader {
        Reader::Seed
    }

    fn seed_into(&self, checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        for variable in [STORE_DIR, "NPM_CONFIG_STORE_DIR"] {
            if let Some(dir) = env(variable).filter(|v| !v.is_empty()) {
                return Ok((PathBuf::from(dir), STORE));
            }
        }
        // `pnpm store path` reads the project's `.npmrc`, the user's and the platform's default,
        // and names the version's folder (`…/store/v10`).
        let program = env("HUSHRUN_PNPM").unwrap_or_else(|| OsString::from("pnpm"));
        let out = Command::new(&program)
            .args(["store", "path"])
            .current_dir(checkout)
            .output()
            .map_err(|e| format!("`pnpm store path` couldn't start: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let path = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(str::trim);
        match path.map(PathBuf::from) {
            Some(path) if out.status.success() && path.is_absolute() => match path.parent() {
                Some(parent)
                    if path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(is_version) =>
                {
                    Ok((parent.to_path_buf(), STORE))
                }
                _ => Err(format!(
                    "`pnpm store path` named {}, not a store's version",
                    path.display()
                )),
            },
            _ => Err("`pnpm store path` named no store".to_string()),
        }
    }
}
