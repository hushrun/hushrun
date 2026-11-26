//! The `bun` part: the npm tarballs `bun.lock` names, each checked against the lockfile's
//! `integrity` (K-10, mechanism C, K-20). Bun keeps its cache unpacked (`<name>@<version>@@@1/`),
//! which no lockfile hash can check, but it reads a folder it didn't write: so the part holds the
//! tarballs, a bake downloads them by the lockfile with no tool run, and a seed unpacks each
//! tarball the lockfile in the checkout names now into Bun's cache. Never read in place: Bun links
//! `node_modules` files out of its one cache, and fails an install that must add to a cache it
//! can't write.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::lockfile::{integrity_hex, json_with_trailing_commas, sha512_hex};
use crate::part::{Admit, Admits, Download, Downloads, Env, LockedBy, Part, Reader, file_name};
use crate::state::ReadPart;
use crate::store::VerifiedBy;

/// The variable Bun reads its cache folder from.
pub const CACHE_DIR: &str = "BUN_INSTALL_CACHE_DIR";
/// The part's folder of tarballs.
pub const TARBALLS: &str = "tarballs";
/// The `lockfileVersion`s of `bun.lock` this reader knows.
pub const LOCKFILE_VERSIONS: [u64; 2] = [1, 2];
/// Where a package `bun.lock` names with no registry of its own is downloaded from.
pub const REGISTRY: &str = "https://registry.npmjs.org";

/// The `bun` part.
pub struct Bun;

/// One package of `bun.lock` that came from a registry, with its integrity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locked {
    /// Its name, with its scope (`@scope/name`).
    pub name: String,
    /// Its version.
    pub version: String,
    /// The registry's tarball URL `bun.lock` wrote, empty for the default registry.
    pub registry: String,
    /// Its `integrity`, as `bun.lock` writes it.
    pub integrity: String,
}

impl Locked {
    /// Its tarball's path under [`TARBALLS`].
    pub fn tarball(&self) -> String {
        format!("{}@{}.tgz", self.name, self.version)
    }
}

/// `name@version.tgz` (a path under [`TARBALLS`]) as its name and version.
fn name_version(path: &str) -> Option<(&str, &str)> {
    let spec = path.strip_suffix(".tgz")?;
    let at = spec.rfind('@').filter(|at| *at > 0)?;
    let (name, version) = (&spec[..at], &spec[at + 1..]);
    let scoped = name.split('/').count() == 2 && name.starts_with('@');
    (!version.is_empty() && (scoped || !name.contains('/'))).then_some((name, version))
}

/// The packages from a registry a `bun.lock` names. A `lockfileVersion` this reader doesn't
/// know, or text that isn't a `bun.lock`, is an error. A workspace's packages and `file:`,
/// `link:`, `git` and `github` entries have no integrity, and are not listed.
pub fn read_lock(text: &str) -> Result<Vec<Locked>, String> {
    let value = json_with_trailing_commas(text).map_err(|e| format!("isn't a bun.lock: {e}"))?;
    let version = value.get("lockfileVersion").and_then(|v| v.as_u64());
    if !version.is_some_and(|v| LOCKFILE_VERSIONS.contains(&v)) {
        return Err(format!(
            "is a version this hushrun doesn't read (lockfileVersion {})",
            version.map_or_else(|| "missing".to_string(), |v| v.to_string())
        ));
    }
    let mut out = Vec::new();
    let Some(packages) = value.get("packages").and_then(|p| p.as_object()) else {
        return Ok(out);
    };
    for entry in packages.values() {
        let Some(fields) = entry.as_array() else {
            continue;
        };
        let (Some(spec), Some(registry), Some(integrity)) = (
            fields.first().and_then(|v| v.as_str()),
            fields.get(1).and_then(|v| v.as_str()),
            fields.get(3).and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let Some(at) = spec.rfind('@').filter(|at| *at > 0) else {
            continue;
        };
        let (name, version) = (&spec[..at], &spec[at + 1..]);
        if version.is_empty() || version.contains(':') || integrity_hex(integrity).is_none() {
            continue;
        }
        out.push(Locked {
            name: name.to_string(),
            version: version.to_string(),
            registry: registry.to_string(),
            integrity: integrity.to_string(),
        });
    }
    out.sort_by_key(Locked::tarball);
    out.dedup();
    Ok(out)
}

/// Every package the `bun.lock` files among `manifests` name, by tarball path, with the SHA-512
/// each must hash to and its integrity as written.
fn locked_hashes(checkout: &Path, manifests: &[String]) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    for manifest in manifests.iter().filter(|m| file_name(m) == "bun.lock") {
        let Ok(text) = std::fs::read_to_string(checkout.join(manifest)) else {
            continue;
        };
        for locked in read_lock(&text).unwrap_or_default() {
            if let Some(hex) = integrity_hex(&locked.integrity) {
                out.insert(locked.tarball(), (hex, locked.integrity));
            }
        }
    }
    out
}

/// The cache folder Bun names in `bunfig.toml`'s `[install.cache]` `dir`, if any, as written.
fn bunfig_cache_dir(text: &str) -> Option<String> {
    let mut section = String::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let named = (section == "install.cache" && key == "dir")
            || (section == "install" && key == "cache.dir");
        if named {
            return Some(
                value
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string(),
            );
        }
    }
    None
}

impl Part for Bun {
    fn name(&self) -> &'static str {
        "bun"
    }

    fn tool(&self) -> &'static str {
        "Bun"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| matches!(file_name(path), "bun.lock" | "bun.lockb"))
            .cloned()
            .collect()
    }

    /// Bun's own cache holds no tarball: a `bun` part is downloaded by the lockfile, never
    /// harvested.
    fn harvest_root(&self, _cache: &Path) -> Option<(PathBuf, &'static str)> {
        None
    }

    fn keep(&self, path: &str) -> bool {
        name_version(path).is_some()
    }

    fn package(&self, path: &str) -> Option<String> {
        name_version(path).map(|(name, version)| format!("{name}@{version}"))
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, _dir: &Path, _checkout: &Path, _env: Env<'_>) -> Reader {
        Reader::Seed
    }

    /// `$BUN_INSTALL_CACHE_DIR`, else `bunfig.toml`'s `[install.cache] dir`, else
    /// `$BUN_INSTALL/install/cache`, else `~/.bun/install/cache`.
    fn seed_into(&self, checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        if let Some(dir) = env(CACHE_DIR).filter(|v| !v.is_empty()) {
            return Ok((PathBuf::from(dir), TARBALLS));
        }
        if let Some(dir) = std::fs::read_to_string(checkout.join("bunfig.toml"))
            .ok()
            .and_then(|t| bunfig_cache_dir(&t))
        {
            let path = PathBuf::from(&dir);
            if !path.is_absolute() {
                return Err(format!(
                    "bunfig.toml names its cache folder as {dir}, which hushrun doesn't resolve"
                ));
            }
            return Ok((path, TARBALLS));
        }
        if let Some(home) = env("BUN_INSTALL").filter(|v| !v.is_empty()) {
            return Ok((PathBuf::from(home).join("install/cache"), TARBALLS));
        }
        let home = env("HOME")
            .or_else(|| env("USERPROFILE"))
            .filter(|v| !v.is_empty())
            .ok_or("no home folder to find Bun's cache in, and BUN_INSTALL_CACHE_DIR is unset")?;
        Ok((PathBuf::from(home).join(".bun/install/cache"), TARBALLS))
    }

    /// K-20: a tarball the checkout's `bun.lock` names now, by its name, version and integrity.
    /// A tarball whose row's SHA-256 holds and that the lockfile names with another integrity is
    /// one the lockfile doesn't name now.
    fn admit(&self, checkout: &Path, manifests: &[String]) -> Admits {
        let named = locked_hashes(checkout, manifests);
        Box::new(move |path, bytes| match named.get(path) {
            Some((hex, _)) if *hex == sha512_hex(bytes) => Admit::Copy,
            _ => Admit::NotLocked,
        })
    }

    fn locked_by(&self, checkout: &Path, manifests: &[String]) -> Option<LockedBy> {
        let named = locked_hashes(checkout, manifests);
        Some(Box::new(move |path, bytes| {
            named
                .get(path)
                .filter(|(hex, _)| *hex == sha512_hex(bytes))
                .map(|(_, integrity)| integrity.clone())
        }))
    }

    fn downloads(
        &self,
        checkout: &Path,
        manifests: &[String],
    ) -> Option<Result<Downloads, String>> {
        if manifests.is_empty() {
            return None;
        }
        let texts: Vec<&String> = manifests
            .iter()
            .filter(|m| file_name(m) == "bun.lock")
            .collect();
        if texts.is_empty() {
            return Some(Err(
                "bun.lockb is Bun's binary lockfile, which names no hashes (`bun install --save-text-lockfile` writes a bun.lock)"
                    .to_string(),
            ));
        }
        let mut downloads = Downloads::default();
        let mut elsewhere = 0;
        let mut seen = std::collections::BTreeSet::new();
        for manifest in texts {
            let text = match std::fs::read_to_string(checkout.join(manifest)) {
                Ok(text) => text,
                Err(err) => return Some(Err(format!("{manifest}: {err}"))),
            };
            let locked = match read_lock(&text) {
                Ok(locked) => locked,
                Err(why) => return Some(Err(format!("{manifest} {why}"))),
            };
            for package in locked {
                if !seen.insert(package.tarball()) {
                    continue;
                }
                if !package.registry.is_empty() {
                    elsewhere += 1;
                    continue;
                }
                let base = package.name.rsplit('/').next().unwrap_or(&package.name);
                downloads.list.push(Download {
                    url: format!(
                        "{REGISTRY}/{}/-/{base}-{}.tgz",
                        package.name, package.version
                    ),
                    path: format!("{TARBALLS}/{}", package.tarball()),
                    package: format!("{}@{}", package.name, package.version),
                    locked: package.integrity,
                });
            }
        }
        if elsewhere > 0 {
            downloads
                .left_out
                .push((elsewhere, "from a registry other than npm's".to_string()));
        }
        Some(Ok(downloads))
    }

    /// K-20: each tarball unpacked into `<name>@<version>@@@1/`, the folder Bun names a package
    /// of the default registry by.
    fn seed(
        &self,
        read: &ReadPart,
        under: &str,
        dest: &Path,
        admit: &dyn Fn(&str, &[u8]) -> Admit,
    ) -> std::io::Result<crate::seed::Seeded> {
        crate::seed::seed_unpacking(read, under, dest, admit, &|path| {
            name_version(path).map(|(name, version)| format!("{name}@{version}@@@1"))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::*;
    use crate::store::{Class, Kind, PartEntry, Row, RowsFile};
    use hushrun_core::blocks::Hex32;

    const CHALK: &str = "sha512-oKnbhFyRIXpUuez8iBMmyEa4nbj4IOQyuhc/wy9kY7/WVPcwIO9VA668Pu8RkO7+0G76SLROeyw9CpQ061i4mA==";

    fn lock(version: u64) -> String {
        format!(
            r#"{{
  "lockfileVersion": {version},
  "workspaces": {{ "": {{ "name": "p", "dependencies": {{ "chalk": "4.1.2" }}, }}, }},
  "packages": {{
    "chalk": ["chalk@4.1.2", "", {{ "dependencies": {{ "x": "^1" }} }}, "{CHALK}"],
    "@scope/pkg": ["@scope/pkg@1.0.0", "", {{}}, "{CHALK}"],
    "mine": ["mine@https://npm.example.com/mine/-/mine-2.0.0.tgz", "", {{}}, "{CHALK}"],
    "other": ["other@3.0.0", "https://npm.example.com/other/-/other-3.0.0.tgz", {{}}, "{CHALK}"],
    "app": ["app@workspace:packages/app"],
    "local": ["local@file:../local", {{}}],
    "gh": ["gh@github:o/r#abc", {{}}, "o-r-abc"],
  }}
}}
"#
        )
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    // covers: K-10, K-20
    #[test]
    fn bun_lock_names_its_registry_packages_and_only_versions_it_knows() {
        let locked = read_lock(&lock(2)).unwrap();
        let names: Vec<String> = locked.iter().map(Locked::tarball).collect();
        assert_eq!(
            names,
            ["@scope/pkg@1.0.0.tgz", "chalk@4.1.2.tgz", "other@3.0.0.tgz"]
        );
        assert_eq!(locked[1].integrity, CHALK);
        assert!(read_lock(&lock(1)).is_ok());
        assert_eq!(
            read_lock(&lock(3)).unwrap_err(),
            "is a version this hushrun doesn't read (lockfileVersion 3)"
        );
        assert!(
            read_lock("{\"packages\": {}}")
                .unwrap_err()
                .contains("missing")
        );
        assert!(read_lock("\0").unwrap_err().starts_with("isn't a bun.lock"));
    }

    // covers: K-20
    #[test]
    fn a_bake_downloads_from_npm_s_registry_and_leaves_out_the_rest() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("bun.lock"), lock(2)).unwrap();
        let tracked = vec!["bun.lock".to_string(), "src/a.ts".to_string()];
        let manifests = Bun.manifests(&tracked);
        assert_eq!(manifests, ["bun.lock"]);
        let downloads = Bun.downloads(t.path(), &manifests).unwrap().unwrap();
        let urls: Vec<&str> = downloads.list.iter().map(|d| d.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://registry.npmjs.org/@scope/pkg/-/pkg-1.0.0.tgz",
                "https://registry.npmjs.org/chalk/-/chalk-4.1.2.tgz"
            ]
        );
        assert_eq!(downloads.list[1].path, "tarballs/chalk@4.1.2.tgz");
        assert_eq!(
            downloads.left_out,
            [(1, "from a registry other than npm's".to_string())]
        );
        assert!(Bun.downloads(t.path(), &[]).is_none());
        let only_binary = vec!["bun.lockb".to_string()];
        assert!(
            Bun.downloads(t.path(), &only_binary)
                .unwrap()
                .unwrap_err()
                .contains("names no hashes")
        );
    }

    // covers: K-10
    #[test]
    fn a_tarball_names_its_package_and_the_cache_is_found_as_bun_finds_it() {
        assert_eq!(
            Bun.package("@scope/pkg@1.0.0.tgz").as_deref(),
            Some("@scope/pkg@1.0.0")
        );
        assert_eq!(
            Bun.package("chalk@4.1.2.tgz").as_deref(),
            Some("chalk@4.1.2")
        );
        for not in ["chalk.tgz", "a/b/c@1.tgz", "@4.1.2.tgz", "chalk@4.1.2.zip"] {
            assert!(!Bun.keep(not), "{not}");
        }
        let c = tempfile::tempdir().unwrap();
        let into = |pairs| Bun.seed_into(c.path(), &env(pairs)).map(|(d, _)| d);
        assert_eq!(
            into(&[(CACHE_DIR, "/c"), ("HOME", "/h")]).unwrap(),
            PathBuf::from("/c")
        );
        assert_eq!(
            into(&[("BUN_INSTALL", "/b"), ("HOME", "/h")]).unwrap(),
            PathBuf::from("/b/install/cache")
        );
        assert_eq!(
            into(&[("HOME", "/h")]).unwrap(),
            PathBuf::from("/h/.bun/install/cache")
        );
        fs::write(
            c.path().join("bunfig.toml"),
            "[install.cache]\ndir = \"/f\" # mine\n",
        )
        .unwrap();
        assert_eq!(into(&[("HOME", "/h")]).unwrap(), PathBuf::from("/f"));
        fs::write(
            c.path().join("bunfig.toml"),
            "[install]\ncache.dir = \"rel\"\n",
        )
        .unwrap();
        assert!(
            into(&[("HOME", "/h")])
                .unwrap_err()
                .contains("doesn't resolve")
        );
        assert_eq!(
            Bun.reader(Path::new("/p"), c.path(), &env(&[])),
            Reader::Seed
        );
    }

    // covers: K-10, K-20
    #[test]
    fn a_seed_unpacks_each_tarball_the_lockfile_names_into_bun_s_folder_for_it() {
        let t = tempfile::tempdir().unwrap();
        let checkout = t.path().join("checkout");
        let part = t.path().join("part");
        let cache = t.path().join("cache");
        let good = crate::tgz::tgz(&[("package/package.json", b'0', b"{\"name\":\"a\"}", 0o644)]);
        let other = crate::tgz::tgz(&[("package/index.js", b'0', b"1", 0o644)]);
        let integrity = |bytes: &[u8]| {
            use base64::Engine as _;
            use sha2_hw::Digest as _;
            format!(
                "sha512-{}",
                base64::engine::general_purpose::STANDARD.encode(sha2_hw::Sha512::digest(bytes))
            )
        };
        fs::create_dir_all(&checkout).unwrap();
        fs::write(
            checkout.join("bun.lock"),
            format!(
                "{{\"lockfileVersion\": 2, \"packages\": {{\"a\": [\"a@1.0.0\", \"\", {{}}, \"{}\"], \"@s/b\": [\"@s/b@2.0.0\", \"\", {{}}, \"{}\"],}}}}",
                integrity(&good),
                integrity(b"not these bytes")
            ),
        )
        .unwrap();
        let files: [(&str, &[u8]); 3] = [
            ("tarballs/a@1.0.0.tgz", &good),
            ("tarballs/@s/b@2.0.0.tgz", &other),
            ("tarballs/gone@3.0.0.tgz", &other),
        ];
        let rows = files
            .iter()
            .map(|(path, bytes)| {
                let at = part.join(path);
                fs::create_dir_all(at.parent().unwrap()).unwrap();
                fs::write(&at, bytes).unwrap();
                Row {
                    path: path.to_string(),
                    kind: Kind::File,
                    bytes: bytes.len() as u64,
                    sha256: Hex32::sha256(bytes),
                    target: None,
                    package: None,
                    locked: None,
                }
            })
            .collect();
        let read = ReadPart {
            entry: PartEntry {
                part: "bun".into(),
                class: Class::Download,
                tool: "t".into(),
                runtime: None,
                inputs: Hex32::sha256(b"i"),
                manifests: vec![],
                identity: Hex32::sha256(b"id"),
                rows: RowsFile {
                    sha256: Hex32::sha256(b"r"),
                    bytes: 0,
                    count: 0,
                },
                bytes: 0,
                verified_by: VerifiedBy::Lockfile,
            },
            rows,
            dir: part.clone(),
            behind: false,
        };
        let manifests = vec!["bun.lock".to_string()];
        let admit = Bun.admit(&checkout, &manifests);
        let done = Bun.seed(&read, TARBALLS, &cache, &*admit).unwrap();
        assert_eq!(done.copied, 1);
        assert!(done.changed.is_empty());
        assert_eq!(done.not_locked, 2);
        assert_eq!(
            fs::read(cache.join("a@1.0.0@@@1/package.json")).unwrap(),
            b"{\"name\":\"a\"}"
        );
        assert!(!cache.join("@s/b@2.0.0@@@1").exists());
        assert!(!cache.join("gone@3.0.0@@@1").exists());
        let locked = Bun.locked_by(&checkout, &manifests).unwrap();
        assert_eq!(locked("a@1.0.0.tgz", &good), Some(integrity(&good)));
        assert_eq!(locked("@s/b@2.0.0.tgz", &other), None);
    }
}
