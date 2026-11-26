//! The `swiftpm` part: the bare clones under `repositories/` of SwiftPM's shared cache, seeded
//! (mechanism C) into the run's own cache before COMMAND, one repository at a time, and only a
//! repository a `Package.resolved` of the checkout pins. SwiftPM reads no second cache, so nothing
//! can be read in place. With a pin's revision in the cache it fetches nothing: it copies the clone
//! into the package's `.build` and checks the revision out.
//!
//! The cache is `~/Library/Caches/org.swift.swiftpm` on macOS and `$XDG_CACHE_HOME/org.swift.swiftpm`
//! (`~/.cache/org.swift.swiftpm`) on Linux, unless a command names another with `--cache-path`;
//! the part is the same on both. Git checks no object against its name when it clones from a
//! folder, so a changed object would be checked out at the pinned revision: the part is verified by
//! a signature, and the seed hashes every file it copies.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hushrun_core::blocks::Hex32;

use crate::part::{Download, Env, Locks, Part, Reader, Seeded, file_name};
use crate::store::{self, Kind, Row, VerifiedBy};

/// The part's folder, as under SwiftPM's cache: one bare clone per repository.
pub const REPOSITORIES: &str = "repositories";
/// SwiftPM's cache's folder, under the user's caches folder.
pub const CACHE: &str = "org.swift.swiftpm";
/// The key of [`Locks`] under which `locks` lists each `Package.resolved` it can't read.
pub const UNREAD: &str = "/unread";
/// The stamp a seed leaves in SwiftPM's cache: the next seed of the same files costs one `stat`.
pub const STAMP: &str = ".hushrun-packages-";

/// The `swiftpm` part.
pub struct SwiftPm;

impl Part for SwiftPm {
    fn name(&self) -> &'static str {
        "swiftpm"
    }

    fn tool(&self) -> &'static str {
        "SwiftPM"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| {
                let name = file_name(path);
                name == "Package.resolved"
                    || name == "Package.swift"
                    || (name.starts_with("Package@swift-") && name.ends_with(".swift"))
            })
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let root = cache.join(REPOSITORIES);
        root.is_dir().then_some((root, REPOSITORIES))
    }

    fn keep(&self, path: &str) -> bool {
        // <repository>/<a file of its bare clone>; nothing at the top (SwiftPM's locks), no
        // notes of git's own use, no hook, nothing that points outside.
        let parts: Vec<&str> = path.split('/').collect();
        let [repository, inner @ ..] = parts.as_slice() else {
            return false;
        };
        let name = file_name(path);
        !(inner.is_empty()
            || repository.starts_with('.')
            || name.ends_with(".lock")
            || name.starts_with("tmp_")
            || matches!(name, "FETCH_HEAD" | "ORIG_HEAD" | "gc.log" | "gc.pid")
            || matches!(inner.first(), Some(&"hooks") | Some(&"logs"))
            || inner == ["objects", "info", "alternates"])
    }

    fn package(&self, path: &str) -> Option<String> {
        let repository = path.split('/').next()?;
        Some(basename_of_folder(repository).to_string())
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Signature
    }

    fn reader(&self, _dir: &Path, _checkout: &Path, env: Env<'_>) -> Reader {
        if cfg!(windows) {
            return Reader::LeftAlone(
                "hushrun seeds SwiftPM's cache on Linux and macOS only".to_string(),
            );
        }
        match cache_dir(env, cfg!(target_os = "macos")) {
            Some(cache) => Reader::Seed(cache),
            None => Reader::LeftAlone("HOME isn't set".to_string()),
        }
    }

    fn locks(&self, checkout: &Path, manifests: &[String]) -> Locks {
        let mut locks = Locks::new();
        for path in manifests
            .iter()
            .filter(|p| file_name(p) == "Package.resolved")
        {
            let Ok(text) = fs::read_to_string(checkout.join(path)) else {
                continue;
            };
            match pins(&text) {
                Ok(pins) => {
                    for (location, revision) in pins {
                        let revisions = locks.entry(basename(&location).to_string()).or_default();
                        if !revisions.contains(&revision) {
                            revisions.push(revision);
                        }
                    }
                }
                Err(why) => locks
                    .entry(UNREAD.to_string())
                    .or_default()
                    .push(format!("{path} ({why})")),
            }
        }
        locks
    }

    fn seed(&self, dir: &Path, rows: &[Row], into: &Path, locks: &Locks) -> Seeded {
        let mut seeded = Seeded::default();
        if let Some(unread) = locks.get(UNREAD) {
            seeded.error = Some(format!("hushrun can't read {}", unread.join(", ")));
            return seeded;
        }
        // Each repository the lockfiles pin, with its rows.
        let mut repositories: Vec<(&str, Vec<&Row>)> = Vec::new();
        for row in rows {
            let Some(inner) = row.path.strip_prefix("repositories/") else {
                seeded.unpinned += 1;
                continue;
            };
            let repository = inner.split('/').next().unwrap_or(inner);
            if !locks.contains_key(basename_of_folder(repository)) {
                seeded.unpinned += 1;
                continue;
            }
            match repositories.last_mut() {
                Some((name, list)) if *name == repository => list.push(row),
                _ => repositories.push((repository, vec![row])),
            }
        }
        if repositories.is_empty() {
            return seeded;
        }
        let stamp = into.join(format!("{STAMP}{}", stamp_of(&repositories, locks)));
        if fs::symlink_metadata(&stamp).is_ok() {
            seeded.had = repositories.iter().map(|(_, r)| r.len()).sum();
            return seeded;
        }
        for (repository, rows) in &repositories {
            let to = into.join(REPOSITORIES).join(repository);
            // Never over what is there: a clone SwiftPM has is SwiftPM's, and it fetches what
            // that lacks.
            if fs::symlink_metadata(&to).is_ok() {
                seeded.had += rows.len();
                continue;
            }
            match seed_repository(dir, rows, &to) {
                Ok(true) => seeded.copied += rows.len(),
                Ok(false) => seeded.refused += rows.len(),
                Err(err) => {
                    seeded.error = Some(format!("{}: {err}", to.display()));
                    return seeded;
                }
            }
        }
        if seeded.refused == 0 {
            let _ = fs::write(&stamp, b"");
        }
        seeded
    }

    fn download(&self, checkout: &Path, manifests: &[String], into: &Path) -> Download {
        // `swift package resolve` in each package a Package.resolved pins, with the bake's own
        // cache, and its own scratch folder, so that nothing of the checkout is written.
        let packages: Vec<&str> = manifests
            .iter()
            .filter(|p| file_name(p) == "Package.resolved")
            .map(|p| p.rsplit_once('/').map_or("", |(dir, _)| dir))
            .filter(|dir| {
                let swift = if dir.is_empty() {
                    "Package.swift".to_string()
                } else {
                    format!("{dir}/Package.swift")
                };
                manifests.contains(&swift)
            })
            .collect();
        if packages.is_empty() {
            return Some(Err(
                "no Package.resolved beside a Package.swift pins a package to resolve: harvest a filled cache with --harvest swiftpm=FOLDER".to_string(),
            ));
        }
        let scratch = into.with_file_name(format!("{}-scratch", self.name()));
        Some(Ok(packages
            .iter()
            .enumerate()
            .map(|(n, dir)| {
                let mut command: Vec<OsString> = ["swift", "package", "resolve"]
                    .into_iter()
                    .map(OsString::from)
                    .collect();
                command.push("--package-path".into());
                command.push(if dir.is_empty() {
                    checkout.as_os_str().to_owned()
                } else {
                    checkout.join(dir).into_os_string()
                });
                command.push("--cache-path".into());
                command.push(into.as_os_str().to_owned());
                command.push("--scratch-path".into());
                command.push(scratch.join(n.to_string()).into_os_string());
                command.push("--force-resolved-versions".into());
                command
            })
            .collect()))
    }
}

/// K-10: SwiftPM's shared cache for a run whose variables are `env`: `<caches>/org.swift.swiftpm`,
/// where the caches are `~/Library/Caches` on macOS and `$XDG_CACHE_HOME`, else `~/.cache`,
/// elsewhere.
pub fn cache_dir(env: Env<'_>, macos: bool) -> Option<PathBuf> {
    let home = || {
        env("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
    };
    let caches = if macos {
        home()?.join("Library/Caches")
    } else {
        match env("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            Some(xdg) => xdg,
            None => home()?.join(".cache"),
        }
    };
    Some(caches.join(CACHE))
}

/// SwiftPM's name of a repository at `location`: its last component, less `.git`. The folder SwiftPM
/// keeps its clone in is that, `-`, and 8 hex characters of a hash of the location.
pub fn basename(location: &str) -> &str {
    let last = location
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(location);
    last.strip_suffix(".git").unwrap_or(last)
}

/// The repository's name of a clone's folder in the cache: the folder's name less `-<8 hex>`.
fn basename_of_folder(folder: &str) -> &str {
    match folder.rsplit_once('-') {
        Some((name, hash))
            if hash.len() == 8
                && hash.bytes().all(|b| b.is_ascii_hexdigit())
                && !name.is_empty() =>
        {
            name
        }
        _ => folder,
    }
}

/// K-3: each remote repository a `Package.resolved` pins, as its location and revision. Versions 1,
/// 2 and 3 are read; another version is an error, and nothing of that file is guessed. A local or
/// registry package isn't in SwiftPM's repositories, and isn't listed.
pub fn pins(text: &str) -> Result<Vec<(String, String)>, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|_| "not JSON".to_string())?;
    let version = json.get("version").and_then(serde_json::Value::as_u64);
    let str_of = |pin: &serde_json::Value, path: &[&str]| -> Option<String> {
        let mut at = pin;
        for key in path {
            at = at.get(key)?;
        }
        at.as_str().map(str::to_string)
    };
    let (list, location, kind) = match version {
        Some(1) => (json.pointer("/object/pins"), "repositoryURL", false),
        Some(2 | 3) => (json.get("pins"), "location", true),
        Some(other) => return Err(format!("version {other}, which it doesn't read")),
        None => return Err("no version".to_string()),
    };
    let list = list
        .and_then(serde_json::Value::as_array)
        .ok_or("no pins")?;
    Ok(list
        .iter()
        .filter(|pin| !kind || str_of(pin, &["kind"]).as_deref() == Some("remoteSourceControl"))
        .filter_map(|pin| {
            Some((
                str_of(pin, &[location])?,
                str_of(pin, &["state", "revision"])?,
            ))
        })
        .collect())
}

/// The stamp's digest: the repositories a seed copies, their rows, and the pins.
fn stamp_of(repositories: &[(&str, Vec<&Row>)], locks: &Locks) -> Hex32 {
    let mut text = String::from("hushrun.packages-seed/v1\0");
    for (_, rows) in repositories {
        for row in rows {
            text.push_str(&format!("{}\0{}\0", row.path, row.sha256));
        }
    }
    for (name, revisions) in locks {
        text.push_str(&format!("{name}\0{}\0", revisions.join(",")));
    }
    Hex32::sha256(text.as_bytes())
}

/// Seeds one repository at `to` from its `rows` in the part at `dir`: into a folder beside it,
/// renamed into place once every file is there, so that SwiftPM never sees half a clone. A file of
/// git's `objects/`, which git never writes in place, is linked where it can be, any other copied;
/// each is hashed as the tool will read it. `Ok(false)` when a file isn't its row's: nothing is left.
fn seed_repository(dir: &Path, rows: &[&Row], to: &Path) -> io::Result<bool> {
    let parent = to.parent().ok_or_else(|| io::Error::other("no folder"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.hushrun-{}",
        file_name(&to.to_string_lossy()),
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp);
    let written = (|| -> io::Result<bool> {
        for row in rows {
            let inner = row
                .path
                .strip_prefix("repositories/")
                .and_then(|p| p.split_once('/'))
                .map_or("", |(_, rest)| rest);
            let from = dir.join(&row.path);
            let at = temp.join(inner);
            fs::create_dir_all(at.parent().unwrap_or(&temp))?;
            match row.kind {
                Kind::Link => {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(row.target.as_deref().unwrap_or_default(), &at)?;
                    continue;
                }
                Kind::File | Kind::Exe => {}
            }
            let linked = inner.starts_with("objects/") && fs::hard_link(&from, &at).is_ok();
            if !linked {
                fs::copy(&from, &at)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let mode = if row.kind == Kind::Exe { 0o755 } else { 0o644 };
                    fs::set_permissions(&at, fs::Permissions::from_mode(mode))?;
                }
            }
            if store::sha256_file(&at)? != row.sha256 {
                return Ok(false);
            }
        }
        Ok(true)
    })();
    match written {
        Ok(true) => match fs::rename(&temp, to) {
            Ok(()) => Ok(true),
            // Another run put the same clone there first.
            Err(_) if to.is_dir() => {
                let _ = fs::remove_dir_all(&temp);
                Ok(true)
            }
            Err(err) => {
                let _ = fs::remove_dir_all(&temp);
                Err(err)
            }
        },
        other => {
            let _ = fs::remove_dir_all(&temp);
            other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    const V2: &str = r#"{
  "originHash" : "abc",
  "pins" : [
    {
      "identity" : "swift-argument-parser",
      "kind" : "remoteSourceControl",
      "location" : "https://github.com/apple/swift-argument-parser.git",
      "state" : { "revision" : "41982a3656a71c768319979febd796c6fd111d5c", "version" : "1.5.0" }
    },
    {
      "identity" : "local",
      "kind" : "localSourceControl",
      "location" : "/src/local",
      "state" : { "revision" : "1111111111111111111111111111111111111111" }
    },
    {
      "identity" : "acme.thing",
      "kind" : "registry",
      "location" : "",
      "state" : { "version" : "1.0.0" }
    }
  ],
  "version" : 3
}"#;

    // covers: K-4
    #[test]
    fn its_manifest_set_is_every_package_resolved_and_package_swift() {
        let tracked = paths(&[
            "Package.swift",
            "Package.resolved",
            "Package@swift-5.9.swift",
            "App.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved",
            "Sources/A/A.swift",
            "pom.xml",
        ]);
        assert_eq!(
            SwiftPm.manifests(&tracked),
            paths(&[
                "Package.swift",
                "Package.resolved",
                "Package@swift-5.9.swift",
                "App.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved",
            ])
        );
    }

    // covers: K-5
    #[test]
    fn a_harvest_keeps_bare_clones_and_leaves_out_locks_hooks_and_gits_notes() {
        for kept in [
            "swift-argument-parser-0a1b2c3d/HEAD",
            "swift-argument-parser-0a1b2c3d/config",
            "swift-argument-parser-0a1b2c3d/packed-refs",
            "swift-argument-parser-0a1b2c3d/objects/pack/pack-1.pack",
            "swift-argument-parser-0a1b2c3d/objects/ab/cdef",
            "swift-argument-parser-0a1b2c3d/refs/tags/1.5.0",
        ] {
            assert!(SwiftPm.keep(kept), "{kept}");
        }
        for left in [
            "swift-argument-parser-0a1b2c3d.lock",
            ".DS_Store",
            "swift-argument-parser-0a1b2c3d/FETCH_HEAD",
            "swift-argument-parser-0a1b2c3d/hooks/pre-commit.sample",
            "swift-argument-parser-0a1b2c3d/logs/HEAD",
            "swift-argument-parser-0a1b2c3d/objects/info/alternates",
            "swift-argument-parser-0a1b2c3d/objects/pack/tmp_pack_x",
            "swift-argument-parser-0a1b2c3d/config.lock",
        ] {
            assert!(!SwiftPm.keep(left), "{left}");
        }
        assert_eq!(
            SwiftPm
                .package("swift-argument-parser-0a1b2c3d/HEAD")
                .as_deref(),
            Some("swift-argument-parser")
        );
    }

    // covers: K-3
    #[test]
    fn package_resolved_of_each_version_it_reads_and_none_it_doesnt() {
        let revision = "41982a3656a71c768319979febd796c6fd111d5c".to_string();
        let url = "https://github.com/apple/swift-argument-parser.git".to_string();
        assert_eq!(pins(V2).unwrap(), vec![(url.clone(), revision.clone())]);
        let v1 = r#"{"object": {"pins": [{"package": "ArgumentParser",
            "repositoryURL": "https://github.com/apple/swift-argument-parser.git",
            "state": {"branch": null, "revision": "41982a3656a71c768319979febd796c6fd111d5c", "version": "1.5.0"}}]},
            "version": 1}"#;
        assert_eq!(pins(v1).unwrap(), vec![(url, revision)]);
        assert_eq!(
            pins(r#"{"version": 4, "pins": []}"#).unwrap_err(),
            "version 4, which it doesn't read"
        );
        assert!(pins("{}").is_err());
        assert_eq!(
            basename("https://github.com/apple/swift-argument-parser.git"),
            "swift-argument-parser"
        );
        assert_eq!(basename("git@github.com:apple/swift-nio"), "swift-nio");
        assert_eq!(basename("https://example.com/a/b/"), "b");
        // The lockfile in a checkout, and one it can't read.
        let checkout = tempfile::tempdir().unwrap();
        fs::write(checkout.path().join("Package.resolved"), V2).unwrap();
        let locks = SwiftPm.locks(
            checkout.path(),
            &paths(&["Package.swift", "Package.resolved"]),
        );
        assert_eq!(locks.keys().collect::<Vec<_>>(), ["swift-argument-parser"]);
        fs::write(
            checkout.path().join("Package.resolved"),
            r#"{"version": 9}"#,
        )
        .unwrap();
        let locks = SwiftPm.locks(checkout.path(), &paths(&["Package.resolved"]));
        assert_eq!(
            locks.get(UNREAD).unwrap(),
            &["Package.resolved (version 9, which it doesn't read)".to_string()]
        );
    }

    // covers: K-10
    #[test]
    fn swiftpms_cache_is_in_the_users_caches_on_either_system() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let home = env(&[("HOME", "/home/a")]);
        assert_eq!(
            cache_dir(&home, false),
            Some(PathBuf::from("/home/a/.cache/org.swift.swiftpm"))
        );
        assert_eq!(
            cache_dir(&home, true),
            Some(PathBuf::from("/home/a/Library/Caches/org.swift.swiftpm"))
        );
        let xdg = env(&[("HOME", "/home/a"), ("XDG_CACHE_HOME", "/x")]);
        assert_eq!(
            cache_dir(&xdg, false),
            Some(PathBuf::from("/x/org.swift.swiftpm"))
        );
        assert_eq!(
            cache_dir(&xdg, true),
            Some(PathBuf::from("/home/a/Library/Caches/org.swift.swiftpm"))
        );
        assert_eq!(cache_dir(&env(&[]), false), None);
        // A seed sets no variable: nothing of a key's moves.
        if !cfg!(windows) {
            assert!(matches!(
                SwiftPm.reader(Path::new("/p"), Path::new("/c"), &home),
                Reader::Seed(_)
            ));
        }
    }

    fn row(path: &str, bytes: &[u8]) -> Row {
        Row {
            path: path.into(),
            kind: Kind::File,
            bytes: bytes.len() as u64,
            sha256: Hex32::sha256(bytes),
            target: None,
            package: None,
            locked: None,
        }
    }

    // covers: K-10
    #[test]
    fn a_seed_copies_whole_pinned_clones_checked_and_never_over_what_is_there() {
        let part = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let files: &[(&str, &[u8])] = &[
            (
                "repositories/other-00000000/HEAD",
                b"ref: refs/heads/main\n",
            ),
            (
                "repositories/swift-argument-parser-0a1b2c3d/HEAD",
                b"ref: refs/heads/main\n",
            ),
            (
                "repositories/swift-argument-parser-0a1b2c3d/objects/ab/cdef",
                b"object",
            ),
            (
                "repositories/swift-nio-12345678/HEAD",
                b"ref: refs/heads/main\n",
            ),
        ];
        for (path, bytes) in files {
            let to = part.path().join(path);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::write(to, bytes).unwrap();
        }
        let rows: Vec<Row> = files.iter().map(|(p, b)| row(p, b)).collect();
        let mut locks = Locks::new();
        locks.insert("swift-argument-parser".into(), vec!["41982a".into()]);
        locks.insert("swift-nio".into(), vec!["aaaaaa".into()]);
        // swift-nio's clone is in the cache already: SwiftPM's own, left as it is.
        let nio = cache.path().join("repositories/swift-nio-12345678");
        fs::create_dir_all(&nio).unwrap();
        let seeded = SwiftPm.seed(part.path(), &rows, cache.path(), &locks);
        assert_eq!(
            seeded,
            Seeded {
                copied: 2,
                had: 1,
                unpinned: 1,
                refused: 0,
                error: None
            }
        );
        let clone = cache
            .path()
            .join("repositories/swift-argument-parser-0a1b2c3d");
        assert_eq!(fs::read(clone.join("objects/ab/cdef")).unwrap(), b"object");
        assert!(!nio.join("HEAD").exists());
        assert!(!cache.path().join("repositories/other-00000000").exists());
        // The second seed: the stamp, and nothing else.
        let again = SwiftPm.seed(part.path(), &rows, cache.path(), &locks);
        assert_eq!((again.copied, again.had), (0, 3));
        // A byte changed in the store, its size the same: refused, and no half clone is left.
        let cache = tempfile::tempdir().unwrap();
        fs::write(
            part.path()
                .join("repositories/swift-argument-parser-0a1b2c3d/objects/ab/cdef"),
            b"objecT",
        )
        .unwrap();
        let seeded = SwiftPm.seed(part.path(), &rows, cache.path(), &locks);
        assert_eq!((seeded.copied, seeded.refused), (1, 2));
        assert!(
            !cache
                .path()
                .join("repositories/swift-argument-parser-0a1b2c3d")
                .exists()
        );
        assert!(
            fs::read_dir(cache.path().join("repositories"))
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with('.')),
            "no folder of a seed is left"
        );
        // A Package.resolved it can't read: the part is left alone, with why.
        let mut unread = Locks::new();
        unread.insert(
            UNREAD.into(),
            vec!["Package.resolved (version 9, which it doesn't read)".into()],
        );
        let seeded = SwiftPm.seed(part.path(), &rows, cache.path(), &unread);
        assert_eq!(seeded.copied, 0);
        assert!(seeded.error.unwrap().contains("version 9"));
    }

    // covers: K-6
    #[test]
    fn a_bake_resolves_each_pinned_package_with_its_own_cache_and_scratch_folder() {
        let into = Path::new("/bake/cache/swiftpm");
        let Some(Ok(commands)) = SwiftPm.download(
            Path::new("/c"),
            &paths(&[
                "Package.swift",
                "Package.resolved",
                "tools/Package.resolved",
            ]),
            into,
        ) else {
            panic!("a command");
        };
        assert_eq!(commands.len(), 1, "tools/ has no Package.swift");
        let words: Vec<String> = commands[0]
            .iter()
            .map(|w| w.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            words,
            [
                "swift",
                "package",
                "resolve",
                "--package-path",
                "/c",
                "--cache-path",
                "/bake/cache/swiftpm",
                "--scratch-path",
                "/bake/cache/swiftpm-scratch/0",
                "--force-resolved-versions"
            ]
        );
        assert!(matches!(
            SwiftPm.download(Path::new("/c"), &paths(&["Package.swift"]), into),
            Some(Err(_))
        ));
    }
}
