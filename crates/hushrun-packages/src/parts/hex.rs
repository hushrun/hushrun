//! The `hex` part: the package tarballs of a Hex cache, `packages/hexpm/<name>-<version>.tar`,
//! seeded (mechanism C) into Hex's own cache before an install, each one only when its
//! SHA-256 is the outer checksum a `mix.lock` of the checkout pins. `mix deps.get` then unpacks
//! `deps/` from the cache as it does from a download, and Hex checks each cached tarball against
//! `mix.lock` itself. Nothing is set: hushrun never names `HEX_HOME`, `ERL_LIBS` or any variable
//! of Mix's.
//!
//! `mix.lock` is read by the smallest reader that serves: one entry a line, as Mix writes it,
//! and of an entry only a Hex package's eight fields. A lockfile in any other form gives no
//! hashes, and the part is left out with a line, never guessed.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use hushrun_core::blocks::Hex32;

use crate::part::{Admit, Admits, Env, Locked, Locks, Part, Reader, file_name};
use crate::store::VerifiedBy;

/// The part's folder under which the tarballs are, as under a Hex cache.
pub const PACKAGES: &str = "packages/hexpm";
/// The repository whose packages the part holds: Hex's own, as `mix.lock` names it.
pub const REPOSITORY: &str = "hexpm";

/// The `hex` part.
pub struct Hex;

impl Part for Hex {
    fn name(&self) -> &'static str {
        "hex"
    }

    fn tool(&self) -> &'static str {
        "Hex"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| matches!(file_name(path), "mix.lock" | "mix.exs"))
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let root = cache.join(PACKAGES);
        root.is_dir().then_some((root, PACKAGES))
    }

    fn keep(&self, path: &str) -> bool {
        // <name>-<version>.tar, nothing else of the cache: not its registry cache, not a file
        // Hex is writing, not another repository's folder.
        !path.contains('/') && !path.starts_with('.') && self.package(path).is_some()
    }

    fn package(&self, path: &str) -> Option<String> {
        let (name, version) = tarball(file_name(path))?;
        Some(format!("{name} {version}"))
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, _dir: &Path, _checkout: &Path, _env: Env<'_>) -> Reader {
        Reader::Seed
    }

    fn seed_into(&self, _checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        Ok((cache_home(env)?.join(PACKAGES), PACKAGES))
    }

    /// A tarball `mix.lock` pins now, whose SHA-256 is its outer checksum.
    fn admit(&self, checkout: &Path, manifests: &[String]) -> Admits {
        let locks = self.read_locks(checkout, manifests).unwrap_or_default();
        Box::new(move |path, bytes| match locks.get(file_name(path)) {
            Some(pinned) if pinned.contains(&Hex32::sha256(bytes).to_string()) => Admit::Copy,
            Some(_) => Admit::Changed,
            None => Admit::NotLocked,
        })
    }

    fn links(&self) -> bool {
        true
    }

    fn read_locks(&self, checkout: &Path, manifests: &[String]) -> Result<Locks, String> {
        let mut locks = Locks::new();
        for path in manifests.iter().filter(|p| file_name(p) == "mix.lock") {
            let Ok(text) = fs::read_to_string(checkout.join(path)) else {
                continue;
            };
            let pinned = lockfile_checksums(&text)
                .map_err(|why| format!("{path} is in a form this hushrun doesn't read ({why})"))?;
            for (file, checksum) in pinned {
                let hashes = locks.entry(file).or_default();
                if !hashes.contains(&checksum) {
                    hashes.push(checksum);
                }
            }
        }
        Ok(locks)
    }

    /// Each tarball `mix.lock` pins, by its outer checksum: a bake keeps only these, and gives each
    /// its `locked`. A lockfile in a form this part doesn't read pins nothing here; a bake leaves
    /// the part out for it, with a line ([`Part::read_locks`]).
    fn lockfile(&self, checkout: &Path, manifests: &[String]) -> Option<Locked> {
        let locks = self.read_locks(checkout, manifests).ok()?;
        let mut locked = Locked::default();
        for (file, hashes) in &locks {
            for sha256 in hashes {
                locked.add(sha256, Some(file));
            }
        }
        Some(locked)
    }

    /// Hex's cache at the bake's folder, and Mix's `deps` and build folders beside its packages,
    /// so the checkout's are left as they were and no `_build` is ever baked.
    fn fetch_vars(&self, cache: &Path) -> Vec<(&'static str, OsString)> {
        vec![
            ("HEX_HOME", cache.into()),
            ("MIX_DEPS_PATH", cache.join("mix-deps").into()),
            ("MIX_BUILD_PATH", cache.join("mix-build/fetch").into()),
        ]
    }
}

/// A tarball's package name and version, from its file name `<name>-<version>.tar`. A Hex
/// package's name holds no `-`: lowercase letters, digits and `_`, from a letter.
fn tarball(file: &str) -> Option<(&str, &str)> {
    let (name, version) = file.strip_suffix(".tar")?.split_once('-')?;
    (is_package_name(name) && !version.is_empty() && !version.contains('/'))
        .then_some((name, version))
}

fn is_package_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The folder Hex keeps its packages under for the run whose variables are `env`, as Hex 2.x
/// works it out (`mix hex.config cache_home`): `HEX_HOME`; else, with `MIX_XDG` `1` or `true`,
/// `hex` under `XDG_CACHE_HOME` or `~/.cache`; else `~/.hex`. A relative folder is the
/// command's own business, and the part is left alone.
pub fn cache_home(env: Env<'_>) -> Result<PathBuf, String> {
    let absolute = |name: &str| -> Result<Option<PathBuf>, String> {
        match env(name) {
            Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => Ok(Some(PathBuf::from(v))),
            Some(v) if !v.is_empty() || name == "HEX_HOME" => Err(format!(
                "{name} is {}, not an absolute folder",
                if v.is_empty() {
                    "empty".to_string()
                } else {
                    format!("`{}`", v.to_string_lossy())
                }
            )),
            _ => Ok(None),
        }
    };
    if let Some(home) = absolute("HEX_HOME")? {
        return Ok(home);
    }
    let home = match absolute("HOME")? {
        Some(home) => Some(home),
        None => absolute("USERPROFILE")?,
    };
    let xdg = env("MIX_XDG").is_some_and(|v| v == "1" || v == "true");
    if xdg && let Some(cache) = absolute("XDG_CACHE_HOME")? {
        return Ok(cache.join("hex"));
    }
    let home = home.ok_or("neither HEX_HOME nor HOME is set")?;
    Ok(if xdg {
        home.join(".cache/hex")
    } else {
        home.join(".hex")
    })
}

/// Each package of a `mix.lock` from Hex's own repository, as its cache file's name
/// (`<name>-<version>.tar`) and its outer checksum, the SHA-256 of that file. A git or path
/// dependency and a package of another repository (an organization's) have none. A lockfile not
/// in the form Mix writes, one entry a line, or a Hex entry without the eight fields of Hex 0.21
/// and later, is an error: nothing of it is guessed.
pub fn lockfile_checksums(text: &str) -> Result<Vec<(String, String)>, String> {
    let body = text.trim();
    let entries = body
        .strip_prefix("%{")
        .and_then(|b| b.strip_suffix('}'))
        .ok_or("it isn't one map, `%{…}`")?;
    let mut out = Vec::new();
    for (n, line) in entries.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let at = n + 1;
        let tuple = entry(line).ok_or_else(|| {
            format!("line {at} isn't one entry, `\"name\": {{…}},`, as Mix writes them")
        })?;
        let fields = fields(tuple).ok_or_else(|| format!("line {at} doesn't end its tuple"))?;
        if fields.first() != Some(&":hex") {
            continue;
        }
        let [_, name, version, _inner, _managers, _deps, repo, outer] = fields.as_slice() else {
            return Err(format!(
                "line {at} is a Hex package of {} fields, not the eight of Hex 0.21 and later, which hold its outer checksum",
                fields.len()
            ));
        };
        let name = name
            .strip_prefix(':')
            .filter(|n| is_package_name(n))
            .ok_or_else(|| format!("line {at} names no package"))?;
        let (Some(version), Some(repo), Some(outer)) =
            (string(version), string(repo), string(outer))
        else {
            return Err(format!("line {at} isn't a Hex package's fields"));
        };
        if outer.len() != 64
            || !outer
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(format!("line {at}'s outer checksum isn't a SHA-256"));
        }
        if repo == REPOSITORY && !version.is_empty() && !version.contains(['/', '\\']) {
            out.push((format!("{name}-{version}.tar"), outer.to_string()));
        }
    }
    Ok(out)
}

/// The tuple of one line of a lockfile: `"name": {…},` (Elixir 1.10 and later) or
/// `"name" => {…},`.
fn entry(line: &str) -> Option<&str> {
    let line = line.strip_suffix(',').unwrap_or(line);
    let rest = line.strip_prefix('"')?;
    let (_key, rest) = rest.split_once('"')?;
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix(':')
        .or_else(|| rest.strip_prefix("=>"))?
        .trim();
    (rest.starts_with('{') && rest.ends_with('}')).then_some(rest)
}

/// The fields of the tuple `{a, b, …}`, split at its own commas, not those of a list, a tuple or
/// a string inside it; `None` when its brackets or quotes don't close where it ends.
fn fields(tuple: &str) -> Option<Vec<&str>> {
    let inner = tuple.strip_prefix('{')?.strip_suffix('}')?;
    let mut out = Vec::new();
    let (mut depth, mut start, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for (i, c) in inner.char_indices() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                out.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 || in_string {
        return None;
    }
    let last = inner[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    Some(out)
}

/// The text of a string field with no escape in it, `"…"`.
fn string(field: &str) -> Option<&str> {
    let text = field.strip_prefix('"')?.strip_suffix('"')?;
    (!text.contains(['"', '\\'])).then_some(text)
}
