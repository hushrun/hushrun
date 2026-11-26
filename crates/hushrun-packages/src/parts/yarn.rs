//! The `yarn` part (K-10, mechanism C): for Yarn 4 (Berry), the zips of its cache, each the file
//! whose SHA-512 is `yarn.lock`'s `checksum`, seeded into the cache folder Yarn names; for Yarn 1,
//! the tarballs of an offline mirror, each the file whose SHA-512 is `yarn.lock`'s `integrity`,
//! seeded into a mirror of hushrun's beside Yarn's cache, which `YARN_YARN_OFFLINE_MIRROR` names
//! for the install. The lockfile verifies every file of the part. Never read in place: Yarn 1
//! writes each package it downloads into its mirror, and fails when it can't.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lockfile::{integrity_hex, is_sha512_hex, sha512_hex};
use crate::part::{Admit, Admits, Env, LockedBy, Part, Reader, file_name};
use crate::state::ReadPart;
use crate::store::VerifiedBy;

/// The variable Yarn 1 reads its offline mirror from. Yarn 4 refuses it.
pub const MIRROR: &str = "YARN_YARN_OFFLINE_MIRROR";
/// The part's folder of Yarn 4's zips.
pub const BERRY: &str = "berry";
/// The part's folder of Yarn 1's tarballs.
pub const V1: &str = "v1";
/// The `__metadata` `version`s of a Yarn 4 `yarn.lock` whose `checksum` is a cached zip's SHA-512.
/// Yarn 3's (6) hashes something else.
pub const BERRY_VERSIONS: [u64; 1] = [8];
/// The folder beside Yarn 1's cache that a seed fills as a mirror.
pub const MIRROR_FOLDER: &str = "hushrun-offline-mirror";

/// Which Yarn wrote a `yarn.lock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// Yarn 2 or later: zips in its cache.
    Berry,
    /// Yarn 1: tarballs in an offline mirror.
    V1,
}

impl Flavor {
    /// The part's folder of its files.
    pub fn folder(self) -> &'static str {
        match self {
            Flavor::Berry => BERRY,
            Flavor::V1 => V1,
        }
    }
}

/// A `yarn.lock` read: who wrote it, and each hash it names, by the SHA-512 in hex, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lock {
    /// Which Yarn wrote it.
    pub flavor: Flavor,
    /// Each `checksum` (Yarn 4) or `integrity` (Yarn 1), by its SHA-512 in hex.
    pub hashes: BTreeMap<String, String>,
}

/// Which Yarn wrote `text`, if a Yarn did.
pub fn flavor(text: &str) -> Option<Flavor> {
    if text.lines().any(|l| l.trim_end() == "__metadata:") {
        Some(Flavor::Berry)
    } else if text.lines().any(|l| l.trim() == "# yarn lockfile v1") {
        Some(Flavor::V1)
    } else {
        None
    }
}

/// `text`, a `yarn.lock`. A version this reader doesn't know is an error.
pub fn read_lock(text: &str) -> Result<Lock, String> {
    let flavor = flavor(text).ok_or("isn't a yarn.lock")?;
    let mut hashes = BTreeMap::new();
    if flavor == Flavor::Berry {
        let version = text
            .lines()
            .skip_while(|l| l.trim_end() != "__metadata:")
            .skip(1)
            .take_while(|l| l.starts_with(' '))
            .find_map(|l| l.trim().strip_prefix("version:"))
            .and_then(|v| v.trim().parse::<u64>().ok());
        if !version.is_some_and(|v| BERRY_VERSIONS.contains(&v)) {
            return Err(format!(
                "is a version this hushrun doesn't read (version {})",
                version.map_or_else(|| "missing".to_string(), |v| v.to_string())
            ));
        }
        for line in text.lines() {
            let Some(written) = line.trim().strip_prefix("checksum:") else {
                continue;
            };
            let written = written.trim().trim_matches('"');
            let hex = written.rsplit('/').next().unwrap_or(written);
            if is_sha512_hex(hex) {
                hashes.insert(hex.to_string(), written.to_string());
            }
        }
    } else {
        for line in text.lines() {
            let Some(written) = line.trim().strip_prefix("integrity ") else {
                continue;
            };
            let written = written.trim().trim_matches('"');
            if let Some(hex) = integrity_hex(written) {
                hashes.insert(hex, written.to_string());
            }
        }
    }
    Ok(Lock { flavor, hashes })
}

/// The `yarn.lock` files among `manifests`, read; the root's first. Unreadable ones are skipped.
fn locks(checkout: &Path, manifests: &[String]) -> Vec<Result<Lock, String>> {
    let mut sorted: Vec<&String> = manifests.iter().collect();
    sorted.sort_by_key(|m| (m.contains('/'), m.as_str()));
    sorted
        .into_iter()
        .filter_map(|m| {
            let text = std::fs::read_to_string(checkout.join(m)).ok()?;
            Some(read_lock(&text).map_err(|why| format!("{m} {why}")))
        })
        .collect()
}

/// Every hash the readable `yarn.lock` files among `manifests` name, by SHA-512, as written.
fn hashes(checkout: &Path, manifests: &[String]) -> BTreeMap<String, String> {
    locks(checkout, manifests)
        .into_iter()
        .flatten()
        .flat_map(|lock| lock.hashes)
        .collect()
}

/// The flavor of the checkout's root `yarn.lock`, if it has one.
pub fn root_flavor(checkout: &Path) -> Option<Flavor> {
    flavor(&std::fs::read_to_string(checkout.join("yarn.lock")).ok()?)
}

/// K-6: what a bake's fetch gives Yarn so that it downloads into `cache` (this part's folder of
/// the bake's own): Yarn 4's global folder, with its global cache on; or Yarn 1's offline mirror,
/// and a cache of its own so that every package is downloaded. Only the variables of the Yarn
/// that wrote the checkout's `yarn.lock`: Yarn 4 refuses Yarn 1's.
pub fn fetch_env(checkout: &Path, cache: &Path) -> Vec<(&'static str, OsString)> {
    match root_flavor(checkout) {
        Some(Flavor::Berry) => vec![
            ("YARN_GLOBAL_FOLDER", cache.as_os_str().to_owned()),
            ("YARN_ENABLE_GLOBAL_CACHE", OsString::from("true")),
        ],
        Some(Flavor::V1) => {
            let mirror = cache.join("mirror");
            // Yarn 1 doesn't make its mirror's folder.
            let _ = std::fs::create_dir_all(&mirror);
            vec![
                (MIRROR, mirror.into_os_string()),
                ("YARN_CACHE_FOLDER", cache.join("v1-cache").into_os_string()),
            ]
        }
        None => Vec::new(),
    }
}

/// Whether `dir` holds a file whose name ends in `suffix`.
fn holds(dir: &Path, suffix: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(suffix))
    })
}

/// The last line `yarn ARGS` prints in `checkout`, when it is an absolute path.
fn ask_yarn(checkout: &Path, env: Env<'_>, args: &[&str]) -> Result<PathBuf, String> {
    let program = env("HUSHRUN_YARN").unwrap_or_else(|| OsString::from("yarn"));
    let words = format!("yarn {}", args.join(" "));
    let out = Command::new(&program)
        .args(args)
        .current_dir(checkout)
        .output()
        .map_err(|e| format!("`{words}` couldn't start: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    match text.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
        Some(path) if out.status.success() && Path::new(path).is_absolute() => {
            Ok(PathBuf::from(path))
        }
        _ => Err(format!("`{words}` named no folder")),
    }
}

/// Where the person already names Yarn 1's offline mirror, if anywhere.
fn mirror_named(checkout: &Path, env: Env<'_>) -> Option<String> {
    if env(MIRROR).is_some_and(|v| !v.is_empty()) {
        return Some(MIRROR.to_string());
    }
    let home = env("HOME").map(PathBuf::from);
    let files = [
        Some(checkout.join(".yarnrc")),
        home.map(|h| h.join(".yarnrc")),
    ];
    files.into_iter().flatten().find_map(|file| {
        let text = std::fs::read_to_string(&file).ok()?;
        text.lines()
            .any(|l| l.trim_start().starts_with("yarn-offline-mirror "))
            .then(|| file.display().to_string())
    })
}

/// The `yarn` part.
pub struct Yarn;

impl Part for Yarn {
    fn name(&self) -> &'static str {
        "yarn"
    }

    fn tool(&self) -> &'static str {
        "Yarn"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| file_name(path) == "yarn.lock")
            .cloned()
            .collect()
    }

    /// `cache` is a Yarn 4 global folder (its `cache/` of zips), a cache folder of zips, a bake's
    /// own fetch folder (`cache/` or `mirror/`), or a Yarn 1 offline mirror of tarballs.
    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        if holds(&cache.join("cache"), ".zip") {
            Some((cache.join("cache"), BERRY))
        } else if holds(&cache.join("mirror"), ".tgz") {
            Some((cache.join("mirror"), V1))
        } else if holds(cache, ".zip") {
            Some((cache.to_path_buf(), BERRY))
        } else if holds(cache, ".tgz") {
            Some((cache.to_path_buf(), V1))
        } else {
            None
        }
    }

    fn keep(&self, path: &str) -> bool {
        !path.contains('/')
            && !path.starts_with('.')
            && (path.ends_with(".zip") || path.ends_with(".tgz"))
    }

    fn package(&self, _path: &str) -> Option<String> {
        None
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, _dir: &Path, _checkout: &Path, _env: Env<'_>) -> Reader {
        Reader::Seed
    }

    /// Yarn 4: the folder `yarn config get cacheFolder` names. Yarn 1: a mirror beside the folder
    /// `yarn cache dir` names, unless the person names a mirror of their own.
    fn seed_into(&self, checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        match root_flavor(checkout).ok_or("the checkout's root has no yarn.lock")? {
            Flavor::Berry => Ok((
                ask_yarn(checkout, env, &["config", "get", "cacheFolder"])?,
                BERRY,
            )),
            Flavor::V1 => {
                if let Some(place) = mirror_named(checkout, env) {
                    return Err(format!("yarn-offline-mirror is set already ({place})"));
                }
                let cache = ask_yarn(checkout, env, &["cache", "dir"])?;
                let beside = cache.parent().unwrap_or(&cache);
                Ok((beside.join(MIRROR_FOLDER), V1))
            }
        }
    }

    fn lockfile_unread(&self, checkout: &Path, manifests: &[String]) -> Option<String> {
        locks(checkout, manifests).into_iter().find_map(Result::err)
    }

    /// A file a `yarn.lock` of the checkout names now, by its SHA-512.
    fn admit(&self, checkout: &Path, manifests: &[String]) -> Admits {
        let named = hashes(checkout, manifests);
        Box::new(move |_, bytes| {
            if named.contains_key(&sha512_hex(bytes)) {
                Admit::Copy
            } else {
                Admit::NotLocked
            }
        })
    }

    fn locked_from(&self, checkout: &Path, manifests: &[String]) -> Option<LockedBy> {
        let named = hashes(checkout, manifests);
        Some(Box::new(move |_, bytes| {
            named.get(&sha512_hex(bytes)).cloned()
        }))
    }

    /// Yarn 1 doesn't make its mirror's folder, and the install is given it whatever was seeded.
    fn seed(
        &self,
        read: &ReadPart,
        under: &str,
        dest: &Path,
        admit: &dyn Fn(&str, &[u8]) -> Admit,
    ) -> std::io::Result<crate::seed::Seeded> {
        if under == V1 {
            std::fs::create_dir_all(dest)?;
        }
        crate::seed::seed(read, under, dest, admit, self.links())
    }

    fn seed_env(&self, checkout: &Path, dest: &Path) -> Vec<(String, OsString)> {
        if root_flavor(checkout) == Some(Flavor::V1) {
            vec![(MIRROR.to_string(), dest.as_os_str().to_owned())]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn hex(bytes: &[u8]) -> String {
        sha512_hex(bytes)
    }

    fn berry(version: u64, zip: &[u8]) -> String {
        format!(
            "# This file is generated by running \"yarn install\" inside your project.\n\n__metadata:\n  version: {version}\n  cacheKey: 10c0\n\n\"chalk@npm:4.1.2\":\n  version: 4.1.2\n  resolution: \"chalk@npm:4.1.2\"\n  checksum: 10c0/{}\n  languageName: node\n  linkType: hard\n",
            hex(zip)
        )
    }

    fn v1(tgz: &[u8]) -> String {
        use base64::Engine as _;
        use sha2_hw::Digest as _;
        format!(
            "# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.\n# yarn lockfile v1\n\n\nchalk@4.1.2:\n  version \"4.1.2\"\n  resolved \"https://registry.yarnpkg.com/chalk/-/chalk-4.1.2.tgz#aa\"\n  integrity sha1-abc sha512-{}\n",
            base64::engine::general_purpose::STANDARD.encode(sha2_hw::Sha512::digest(tgz))
        )
    }

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    // covers: K-10
    #[test]
    fn yarn_lock_names_its_hashes_for_yarn_4_and_yarn_1_and_only_versions_it_knows() {
        let lock = read_lock(&berry(8, b"zip")).unwrap();
        assert_eq!(lock.flavor, Flavor::Berry);
        assert_eq!(
            lock.hashes.get(&hex(b"zip")).map(String::as_str),
            Some(format!("10c0/{}", hex(b"zip")).as_str())
        );
        assert_eq!(
            read_lock(&berry(6, b"zip")).unwrap_err(),
            "is a version this hushrun doesn't read (version 6)"
        );
        let lock = read_lock(&v1(b"tgz")).unwrap();
        assert_eq!(lock.flavor, Flavor::V1);
        assert!(lock.hashes.contains_key(&hex(b"tgz")));
        assert!(read_lock("{}").is_err());
    }

    // covers: K-5, K-10
    #[test]
    fn a_harvest_takes_zips_or_tarballs_and_nothing_else() {
        let t = tempfile::tempdir().unwrap();
        let global = t.path().join("global");
        fs::create_dir_all(global.join("cache")).unwrap();
        fs::write(global.join("cache/a-npm-1.0.0-0123456789-10c0.zip"), "z").unwrap();
        assert_eq!(
            Yarn.harvest_root(&global),
            Some((global.join("cache"), BERRY))
        );
        assert_eq!(
            Yarn.harvest_root(&global.join("cache")),
            Some((global.join("cache"), BERRY))
        );
        let mirror = t.path().join("mirror");
        fs::create_dir_all(&mirror).unwrap();
        fs::write(mirror.join("a-1.0.0.tgz"), "t").unwrap();
        assert_eq!(Yarn.harvest_root(&mirror), Some((mirror.clone(), V1)));
        assert_eq!(Yarn.harvest_root(t.path()), Some((mirror, V1)));
        assert_eq!(Yarn.harvest_root(&t.path().join("none")), None);
        assert!(Yarn.keep("a-npm-1.0.0-0123456789-10c0.zip"));
        assert!(Yarn.keep("@s-b-1.0.0.tgz"));
        for left in [".gitignore", "x.tmp", "sub/a.zip", ".a.zip"] {
            assert!(!Yarn.keep(left), "{left}");
        }
    }

    // covers: K-6, K-10
    #[test]
    fn each_yarn_gets_only_its_own_variables() {
        let t = tempfile::tempdir().unwrap();
        let cache = t.path().join("fetch/yarn");
        assert!(fetch_env(t.path(), &cache).is_empty());
        fs::write(t.path().join("yarn.lock"), berry(8, b"zip")).unwrap();
        let names: Vec<&str> = fetch_env(t.path(), &cache)
            .iter()
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(names, ["YARN_GLOBAL_FOLDER", "YARN_ENABLE_GLOBAL_CACHE"]);
        assert!(Yarn.seed_env(t.path(), Path::new("/m")).is_empty());
        fs::write(t.path().join("yarn.lock"), v1(b"tgz")).unwrap();
        let names: Vec<&str> = fetch_env(t.path(), &cache)
            .iter()
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(names, [MIRROR, "YARN_CACHE_FOLDER"]);
        assert!(cache.join("mirror").is_dir(), "Yarn 1 doesn't make it");
        assert_eq!(
            Yarn.seed_env(t.path(), Path::new("/m")),
            [(MIRROR.to_string(), OsString::from("/m"))]
        );
        // A mirror the person names is theirs: the part is left alone.
        let named = |n: &str| (n == MIRROR).then(|| OsString::from("/theirs"));
        assert_eq!(
            Yarn.seed_into(t.path(), &named).unwrap_err(),
            "yarn-offline-mirror is set already (YARN_YARN_OFFLINE_MIRROR)"
        );
        fs::write(t.path().join(".yarnrc"), "yarn-offline-mirror \"./m\"\n").unwrap();
        assert!(
            Yarn.seed_into(t.path(), &no_env)
                .unwrap_err()
                .starts_with("yarn-offline-mirror is set already")
        );
    }

    // covers: K-10
    #[test]
    fn a_seed_takes_a_file_the_lockfile_hashes_now_and_a_bake_writes_that_hash() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("yarn.lock"), berry(8, b"zip")).unwrap();
        let manifests = Yarn.manifests(&["yarn.lock".to_string(), "a/yarn.lock".to_string()]);
        assert_eq!(manifests, ["yarn.lock", "a/yarn.lock"]);
        let admit = Yarn.admit(t.path(), &manifests);
        assert_eq!(admit("a.zip", b"zip"), Admit::Copy);
        assert_eq!(admit("a.zip", b"zap"), Admit::NotLocked);
        let locked = Yarn.locked_from(t.path(), &manifests).unwrap();
        assert_eq!(
            locked("a.zip", b"zip"),
            Some(format!("10c0/{}", hex(b"zip")))
        );
        assert_eq!(Yarn.lockfile_unread(t.path(), &manifests), None);
        fs::write(t.path().join("yarn.lock"), berry(6, b"zip")).unwrap();
        assert_eq!(
            Yarn.lockfile_unread(t.path(), &manifests).as_deref(),
            Some("yarn.lock is a version this hushrun doesn't read (version 6)")
        );
        assert_eq!(
            Yarn.reader(Path::new("/p"), t.path(), &no_env),
            Reader::Seed
        );
    }
}
