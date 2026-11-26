//! The `swiftpm` part: the bare clones under `repositories/` of SwiftPM's shared cache, seeded
//! (mechanism C) into the cache of the user who installs, only the clones a `Package.resolved` of
//! the checkout pins. SwiftPM reads no second cache, so nothing can be read in place. With a pin's
//! revision in its cache it fetches nothing: it copies the clone into the package's `.build` and
//! checks the revision out.
//!
//! The cache is `~/Library/Caches/org.swift.swiftpm` on macOS and `$XDG_CACHE_HOME/org.swift.swiftpm`
//! (`~/.cache/org.swift.swiftpm`) on Linux, unless a command names another with `--cache-path`;
//! the part is the same on both. Git checks no object against its name when it clones from a
//! folder, so a changed object would be checked out at the pinned revision: the part is verified by
//! a signature, and the seed hashes every file it copies.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::part::{Admit, Admits, Download, Env, Part, Reader, file_name};
use crate::store::VerifiedBy;

/// The part's folder, as under SwiftPM's cache: one bare clone per repository.
pub const REPOSITORIES: &str = "repositories";
/// SwiftPM's cache's folder, under the user's caches folder.
pub const CACHE: &str = "org.swift.swiftpm";

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

    fn reader(&self, _dir: &Path, _checkout: &Path, _env: Env<'_>) -> Reader {
        if cfg!(windows) {
            return Reader::LeftAlone(
                "hushrun seeds SwiftPM's cache on Linux and macOS only".to_string(),
            );
        }
        Reader::Seed
    }

    fn seed_into(&self, checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        // A Package.resolved hushrun can't read pins nothing it could guess: left alone.
        if let Ok(text) = fs::read_to_string(checkout.join("Package.resolved"))
            && let Err(why) = pins(&text)
        {
            return Err(format!("hushrun can't read Package.resolved ({why})"));
        }
        let cache = cache_dir(env, cfg!(target_os = "macos")).ok_or("HOME isn't set")?;
        Ok((cache.join(REPOSITORIES), REPOSITORIES))
    }

    fn admit(&self, checkout: &Path, manifests: &[String]) -> Admits {
        // Each clone a Package.resolved pins, by its repository's name; the bytes are checked
        // against the row by the seed.
        let pinned: BTreeSet<String> = manifests
            .iter()
            .filter(|p| file_name(p) == "Package.resolved")
            .filter_map(|p| fs::read_to_string(checkout.join(p)).ok())
            .filter_map(|text| pins(&text).ok())
            .flatten()
            .map(|(location, _)| basename(&location).to_string())
            .collect();
        Box::new(move |path, _| {
            let repository = path.split('/').next().unwrap_or(path);
            if pinned.contains(basename_of_folder(repository)) {
                Admit::Copy
            } else {
                Admit::NotLocked
            }
        })
    }

    fn after_seed(&self, dest: &Path) -> std::io::Result<()> {
        // A bare clone's empty folders: git reads no folder as a repository without `refs/` and
        // `objects/`, and SwiftPM then clones the dependency from the network, or fails offline.
        for entry in fs::read_dir(dest)?.flatten() {
            let clone = entry.path();
            if clone.join("HEAD").is_file() {
                for folder in ["refs/heads", "refs/tags", "objects/info", "objects/pack"] {
                    fs::create_dir_all(clone.join(folder))?;
                }
            }
        }
        Ok(())
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
/// elsewhere. On macOS, Foundation takes the home folder from `CFFIXED_USER_HOME` when it is set,
/// and otherwise from the user's account, not from `HOME`; `HOME` stands in for the account's.
pub fn cache_dir(env: Env<'_>, macos: bool) -> Option<PathBuf> {
    let absolute = |name: &str| {
        env(name)
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
    };
    let home = || absolute("HOME");
    let caches = if macos {
        absolute("CFFIXED_USER_HOME")
            .or_else(home)?
            .join("Library/Caches")
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
        // The lockfile in a checkout: a seed admits the clones it pins, and one it can't read
        // leaves the part alone.
        let checkout = tempfile::tempdir().unwrap();
        fs::write(checkout.path().join("Package.resolved"), V2).unwrap();
        let admit = SwiftPm.admit(
            checkout.path(),
            &paths(&["Package.swift", "Package.resolved"]),
        );
        assert_eq!(
            admit("swift-argument-parser-0a1b2c3d/HEAD", b""),
            Admit::Copy
        );
        assert_eq!(admit("swift-nio-12345678/HEAD", b""), Admit::NotLocked);
        let home = |name: &str| (name == "HOME").then(|| OsString::from("/home/a"));
        let (into, under) = SwiftPm.seed_into(checkout.path(), &home).unwrap();
        assert_eq!(under, "repositories");
        assert!(
            into.ends_with("org.swift.swiftpm/repositories"),
            "{}",
            into.display()
        );
        fs::write(
            checkout.path().join("Package.resolved"),
            r#"{"version": 9}"#,
        )
        .unwrap();
        assert_eq!(
            SwiftPm.seed_into(checkout.path(), &home).unwrap_err(),
            "hushrun can't read Package.resolved (version 9, which it doesn't read)"
        );
        let admit = SwiftPm.admit(checkout.path(), &paths(&["Package.resolved"]));
        assert_eq!(
            admit("swift-argument-parser-0a1b2c3d/HEAD", b""),
            Admit::NotLocked
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
        // macOS: Foundation's own home first, which a sandbox or a test may set.
        let fixed = env(&[("HOME", "/home/a"), ("CFFIXED_USER_HOME", "/f")]);
        assert_eq!(
            cache_dir(&fixed, true),
            Some(PathBuf::from("/f/Library/Caches/org.swift.swiftpm"))
        );
        assert_eq!(
            cache_dir(&fixed, false),
            Some(PathBuf::from("/home/a/.cache/org.swift.swiftpm"))
        );
        // A seed sets no variable: nothing of a key's moves.
        if !cfg!(windows) {
            assert!(matches!(
                SwiftPm.reader(Path::new("/p"), Path::new("/c"), &home),
                Reader::Seed
            ));
        }
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
