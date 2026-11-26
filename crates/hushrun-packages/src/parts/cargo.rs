//! The `cargo` part: the `.crate` files of a cargo home's `registry/cache`, seeded (mechanism C)
//! into the run's own cargo home, by `hushrun run` before COMMAND too, each one only when its
//! SHA-256 is the checksum a `Cargo.lock` of the checkout pins. Cargo unpacks them itself, as it unpacks what it downloads.
//! Cargo checks no checksum of a file already in its cache, so hushrun checks it first. Git
//! dependencies are left out: their checkouts are no registry's file and have no checksum.

use std::fs;
use std::path::{Path, PathBuf};

use hushrun_core::blocks::Hex32;

use crate::part::{Admit, Admits, Env, Locks, Part, Reader, file_name};
use crate::store::VerifiedBy;

/// The part's folder under which the crates are, as under a cargo home.
pub const CACHE: &str = "registry/cache";

/// The `cargo` part.
pub struct Cargo;

impl Part for Cargo {
    fn name(&self) -> &'static str {
        "cargo"
    }

    fn tool(&self) -> &'static str {
        "Cargo"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| matches!(file_name(path), "Cargo.lock" | "Cargo.toml"))
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let root = cache.join(CACHE);
        root.is_dir().then_some((root, CACHE))
    }

    fn keep(&self, path: &str) -> bool {
        // <registry's folder>/<name>-<version>.crate, nothing else of the cache.
        let parts: Vec<&str> = path.split('/').collect();
        matches!(parts.as_slice(), [registry, file]
            if !registry.starts_with('.') && !file.starts_with('.') && file.ends_with(".crate"))
    }

    fn package(&self, path: &str) -> Option<String> {
        let stem = file_name(path).strip_suffix(".crate")?;
        // The version begins at the first `-` whose rest reads as one: a name may hold `-` and
        // digits (`x25519-dalek-2.0.0`, `foo-2d-1.0.0`).
        stem.match_indices('-')
            .map(|(at, _)| (&stem[..at], &stem[at + 1..]))
            .find(|(name, version)| !name.is_empty() && is_version(version))
            .map(|(name, version)| format!("{name}@{version}"))
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, _dir: &Path, _checkout: &Path, env: Env<'_>) -> Reader {
        match cargo_home(env) {
            Some(_) => Reader::Seed,
            None => Reader::LeftAlone("neither CARGO_HOME nor HOME is set".to_string()),
        }
    }

    fn seed_into(&self, _checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        cargo_home(env)
            .map(|home| (home.join(CACHE), CACHE))
            .ok_or_else(|| "neither CARGO_HOME nor HOME is set".to_string())
    }

    /// A crate a `Cargo.lock` of the checkout pins now, whose SHA-256 is that checksum. Cargo
    /// checks none of a crate already in its cache, so this is the only check it gets.
    fn admit(&self, checkout: &Path, manifests: &[String]) -> Admits {
        let locks = self.locks(checkout, manifests);
        Box::new(move |path, bytes| match locks.get(file_name(path)) {
            Some(pinned) if pinned.contains(&Hex32::sha256(bytes).to_string()) => Admit::Copy,
            Some(_) => Admit::Changed,
            None => Admit::NotLocked,
        })
    }

    fn locks(&self, checkout: &Path, manifests: &[String]) -> Locks {
        let mut locks = Locks::new();
        for path in manifests.iter().filter(|p| file_name(p) == "Cargo.lock") {
            let Ok(text) = fs::read_to_string(checkout.join(path)) else {
                continue;
            };
            for (file, checksum) in lockfile_checksums(&text) {
                let hashes = locks.entry(file).or_default();
                if !hashes.contains(&checksum) {
                    hashes.push(checksum);
                }
            }
        }
        locks
    }

    fn locked_by(&self, path: &str, locks: &Locks) -> Option<String> {
        locks.get(file_name(path))?.first().cloned()
    }

    fn seeds_in_a_run(&self) -> bool {
        true
    }
}

/// The run's cargo home: `CARGO_HOME`, else `~/.cargo`.
fn cargo_home(env: Env<'_>) -> Option<PathBuf> {
    if let Some(home) = env("CARGO_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let home = env("HOME").or_else(|| env("USERPROFILE"))?;
    (!home.is_empty()).then(|| PathBuf::from(home).join(".cargo"))
}

/// Whether `text` reads as a crate's version: three dot-separated numbers, then anything.
fn is_version(text: &str) -> bool {
    let core = text.split(['-', '+']).next().unwrap_or_default();
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Each package of a `Cargo.lock` from a registry with a checksum, as its cache file's name
/// (`<name>-<version>.crate`) and the checksum. A git or path package has none. Lockfiles of
/// version 1, whose checksums are in a `[metadata]` table, give none, and their crates are
/// downloaded as before.
pub fn lockfile_checksums(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut package: Option<[Option<String>; 4]> = None;
    let mut flush = |package: &mut Option<[Option<String>; 4]>| {
        if let Some([Some(name), Some(version), Some(source), Some(checksum)]) = package.take()
            && (source.starts_with("registry+") || source.starts_with("sparse+"))
        {
            out.push((format!("{name}-{version}.crate"), checksum));
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            flush(&mut package);
            if line == "[[package]]" {
                package = Some(Default::default());
            }
            continue;
        }
        let Some(fields) = package.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(value) = value
            .trim()
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
        else {
            continue;
        };
        let slot = match key.trim() {
            "name" => 0,
            "version" => 1,
            "source" => 2,
            "checksum" => 3,
            _ => continue,
        };
        fields[slot] = Some(value.to_string());
    }
    flush(&mut package);
    out
}
