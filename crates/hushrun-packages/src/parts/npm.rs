//! The `npm` part: the tarballs of npm's cache (`_cacache/content-v2/sha512/…`), seeded into the
//! cache the person's npm uses (K-10, mechanism C, K-19). Each tarball's path is its own SHA-512,
//! and `package-lock.json`'s `integrity` is the SHA-512 of the same tarball: the lockfile verifies
//! every file of the part, and the seed copies only a tarball the lockfile in the checkout names
//! now, whose bytes hash to that name. npm then reads it from its cache by that hash, with no
//! request. The cache's index (`index-v5/`) is never kept: it names the registry's URLs and the
//! times of the bake's requests, and npm doesn't need it to read a tarball by its hash.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use sha2_hw::Digest as _;

use crate::part::{Admit, Admits, Env, Part, Reader, file_name};
use crate::store::VerifiedBy;

/// The part's folder that mirrors npm's cache folder's `_cacache`.
pub const CACACHE: &str = "_cacache";
/// The variable npm reads `cache` from.
pub const CACHE: &str = "npm_config_cache";

/// The `npm` part.
pub struct Npm;

/// The lowercase hex of each `sha512-…` integrity in a `package-lock.json` (or an
/// `npm-shrinkwrap.json`), every version's: `packages` (v2, v3) and `dependencies` (v1, v2).
pub fn locked_sha512(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return out;
    };
    let mut stack = vec![&value];
    while let Some(v) = stack.pop() {
        if let Some(map) = v.as_object() {
            for (key, child) in map {
                if key == "integrity"
                    && let Some(text) = child.as_str()
                {
                    out.extend(text.split_whitespace().filter_map(sha512_hex));
                } else {
                    stack.push(child);
                }
            }
        }
    }
    out
}

/// `sha512-<base64>` as lowercase hex.
fn sha512_hex(integrity: &str) -> Option<String> {
    let b64 = integrity.strip_prefix("sha512-")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    (bytes.len() == 64).then(|| bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `content-v2/sha512/ab/cd/<124 hex>` as its hex, the tarball's SHA-512.
fn content_hex(path: &str) -> Option<String> {
    let rest = path.strip_prefix("content-v2/sha512/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let [a, b, c] = parts.as_slice() else {
        return None;
    };
    let hex = format!("{a}{b}{c}");
    (a.len() == 2
        && b.len() == 2
        && hex.len() == 128
        && hex
            .bytes()
            .all(|x| x.is_ascii_hexdigit() && !x.is_ascii_uppercase()))
    .then_some(hex)
}

impl Part for Npm {
    fn name(&self) -> &'static str {
        "npm"
    }

    fn tool(&self) -> &'static str {
        "npm"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| matches!(file_name(path), "package-lock.json" | "npm-shrinkwrap.json"))
            .cloned()
            .collect()
    }

    /// `cache` is npm's cache folder, the one that holds `_cacache`.
    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let root = cache.join(CACACHE);
        root.join("content-v2").is_dir().then_some((root, CACACHE))
    }

    fn keep(&self, path: &str) -> bool {
        content_hex(path).is_some()
    }

    fn package(&self, _path: &str) -> Option<String> {
        None
    }

    /// The tarball's own path is its SHA-512: `sha512-<base64>`, as `package-lock.json` writes it.
    fn locked(&self, path: &str) -> Option<String> {
        let hex = content_hex(path)?;
        let bytes: Vec<u8> = (0..64)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16))
            .collect::<Result<_, _>>()
            .ok()?;
        Some(format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, _dir: &Path, _checkout: &Path, _env: Env<'_>) -> Reader {
        Reader::Seed
    }

    fn seed_into(&self, checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        for variable in [CACHE, "NPM_CONFIG_CACHE"] {
            if let Some(dir) = env(variable).filter(|v| !v.is_empty()) {
                return Ok((PathBuf::from(dir).join(CACACHE), CACACHE));
            }
        }
        let program = env("HUSHRUN_NPM").unwrap_or_else(|| OsString::from("npm"));
        let out = Command::new(&program)
            .args(["config", "get", "cache"])
            .current_dir(checkout)
            .output()
            .map_err(|e| format!("`npm config get cache` couldn't start: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        match text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(str::trim)
        {
            Some(path) if out.status.success() && Path::new(path).is_absolute() => {
                Ok((PathBuf::from(path).join(CACACHE), CACACHE))
            }
            _ => Err("`npm config get cache` named no folder".to_string()),
        }
    }

    /// K-19: a tarball the checkout's lockfiles name now, whose bytes are that hash.
    fn admit(&self, checkout: &Path, manifests: &[String]) -> Admits {
        let mut named = BTreeSet::new();
        for manifest in manifests {
            if let Ok(text) = std::fs::read_to_string(checkout.join(manifest)) {
                named.extend(locked_sha512(&text));
            }
        }
        Box::new(move |path, bytes| match content_hex(path) {
            Some(hex) if named.contains(&hex) => {
                let digest = sha2_hw::Sha512::digest(bytes);
                let got: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                if got == hex {
                    Admit::Copy
                } else {
                    Admit::Changed
                }
            }
            _ => Admit::NotLocked,
        })
    }
}
