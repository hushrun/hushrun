//! The `composer` part: the archives of Composer's files cache (`files/<vendor>/<name>/<sha1 of
//! the dist URL>.<type>`), seeded into the files cache the person's Composer uses (K-10, mechanism
//! C). `composer install` then extracts each package from its cache with no request and writes
//! `vendor/` itself, scripts and plugins as the person's command says. Composer has no second,
//! read-only cache beside its own, so the part can't be read in place.
//!
//! A lock's `dist.shasum` (a SHA-1) is empty for most packages, Packagist's among them, so nothing
//! in `composer.lock` hashes most archives: the part is verified by a signature (K-7). Where a
//! package's `shasum` is there, the seed also checks the archive against it, as Composer does.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::part::{Admit, Admits, Env, Part, Reader, file_name};
use crate::store::VerifiedBy;

/// The part's folder that mirrors Composer's `cache-files-dir`.
pub const FILES: &str = "files";
/// The variable Composer reads its cache folder from.
pub const CACHE_DIR: &str = "COMPOSER_CACHE_DIR";

/// The `composer` part.
pub struct Composer;

/// The SHA-1 of `bytes` as lowercase hex, or `None` for bytes made to collide.
fn sha1_hex(bytes: &[u8]) -> Option<String> {
    sha1dc::digest(bytes).ok().map(|d| format!("{d:x}"))
}

/// Every archive the `composer.lock` text names, as its path in the files cache, with its lock's
/// `shasum` (empty when the lock has none): `<name>/<SHA-1 of dist.url>.<dist.type>`, as Composer
/// names the archive it downloads from that URL. A package with no `dist` is a path or a clone,
/// and names none.
pub fn locked_archives(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(lock) = serde_json::from_str::<serde_json::Value>(text) else {
        return out;
    };
    for list in ["packages", "packages-dev"] {
        for package in lock[list].as_array().into_iter().flatten() {
            let (Some(name), Some(url), Some(kind)) = (
                package["name"].as_str(),
                package["dist"]["url"].as_str(),
                package["dist"]["type"].as_str(),
            ) else {
                continue;
            };
            let Some(key) = sha1_hex(url.as_bytes()) else {
                continue;
            };
            let shasum = package["dist"]["shasum"].as_str().unwrap_or_default();
            out.insert(
                format!("{}/{key}.{kind}", name.to_ascii_lowercase()),
                shasum.to_ascii_lowercase(),
            );
        }
    }
    out
}

/// Whether `path` (below the files cache) is an archive as Composer names one:
/// `<vendor>/<name>/<40 hex>.<type>`.
fn archive(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let [vendor, name, file] = parts.as_slice() else {
        return false;
    };
    let Some((key, kind)) = file.split_once('.') else {
        return false;
    };
    !vendor.is_empty()
        && !name.is_empty()
        && !vendor.starts_with('.')
        && !name.starts_with('.')
        && key.len() == 40
        && key
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && !kind.is_empty()
        && kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
}

impl Part for Composer {
    fn name(&self) -> &'static str {
        "composer"
    }

    fn tool(&self) -> &'static str {
        "Composer"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| file_name(path) == "composer.lock")
            .cloned()
            .collect()
    }

    /// `cache` is Composer's cache folder, the one that holds `files`.
    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let root = cache.join(FILES);
        root.is_dir().then_some((root, FILES))
    }

    fn keep(&self, path: &str) -> bool {
        archive(path)
    }

    fn package(&self, path: &str) -> Option<String> {
        archive(path).then(|| {
            let mut parts = path.split('/');
            format!(
                "{}/{}",
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default()
            )
        })
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Signature
    }

    fn reader(&self, _dir: &Path, _checkout: &Path, _env: Env<'_>) -> Reader {
        Reader::Seed
    }

    /// The files cache Composer would use in `checkout`, as `composer config cache-files-dir`
    /// names it: it follows `COMPOSER_CACHE_DIR`, `COMPOSER_HOME`, the XDG folders and a
    /// `cache-files-dir` of the person's configuration, which no reading of variables alone does.
    fn seed_into(&self, checkout: &Path, env: Env<'_>) -> Result<(PathBuf, &'static str), String> {
        let program = env("HUSHRUN_COMPOSER").unwrap_or_else(|| OsString::from("composer"));
        let out = Command::new(&program)
            .args([
                "config",
                "--no-interaction",
                "--no-plugins",
                "cache-files-dir",
            ])
            .current_dir(checkout)
            .output()
            .map_err(|e| format!("`composer config cache-files-dir` couldn't start: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        match text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(str::trim)
        {
            Some(path) if out.status.success() && Path::new(path).is_absolute() => {
                Ok((PathBuf::from(path), FILES))
            }
            _ => Err("`composer config cache-files-dir` named no folder".to_string()),
        }
    }

    /// An archive a `composer.lock` of the checkout names now, by its name and its URL's SHA-1,
    /// whose SHA-1 is the lock's `shasum` where the lock has one.
    fn admit(&self, checkout: &Path, manifests: &[String]) -> Admits {
        let mut named = BTreeMap::new();
        for manifest in manifests {
            if let Ok(text) = std::fs::read_to_string(checkout.join(manifest)) {
                named.extend(locked_archives(&text));
            }
        }
        Box::new(move |path, bytes| match named.get(path) {
            None => Admit::NotLocked,
            Some(shasum) if shasum.is_empty() => Admit::Copy,
            Some(shasum) if sha1_hex(bytes).as_deref() == Some(shasum.as_str()) => Admit::Copy,
            Some(_) => Admit::Changed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str =
        "https://api.github.com/repos/php-fig/log/zipball/f16e1d5863e37f8d8c2a01719f5b34baa2b714d3";

    fn lock(shasum: &str) -> String {
        format!(
            r#"{{"packages":[{{"name":"psr/log","version":"3.0.2","dist":{{"type":"zip","url":"{URL}","reference":"f16e1d5863e37f8d8c2a01719f5b34baa2b714d3","shasum":"{shasum}"}}}},
               {{"name":"acme/local","version":"dev-main","dist":{{"type":"path","url":"../local"}}}},
               {{"name":"acme/git","version":"1.0.0","source":{{"type":"git","url":"https://example.com/g.git","reference":"abc"}}}}],
             "packages-dev":[{{"name":"Acme/Dev","version":"1.0.0","dist":{{"type":"tar","url":"https://example.com/d.tar"}}}}]}}"#
        )
    }

    // covers: K-4, K-10
    #[test]
    fn a_lock_names_each_archive_as_composer_caches_it() {
        assert_eq!(
            Composer.manifests(&[
                "composer.json".into(),
                "composer.lock".into(),
                "tools/composer.lock".into(),
                "src/composer.lock.php".into(),
            ]),
            ["composer.lock", "tools/composer.lock"]
        );
        let named = locked_archives(&lock(""));
        // Composer names the archive by the SHA-1 of its URL (2.8.12's FileDownloader); `path`
        // packages are named too, but no bake ever holds them, and a `source` alone names none.
        // The name Composer gave the archive of `http://127.0.0.1:8765/a.zip` in its cache.
        assert_eq!(
            sha1_hex(b"http://127.0.0.1:8765/a.zip").as_deref(),
            Some("03bc553f65654a831e82a96043a78127f47603ea")
        );
        let key = sha1_hex(URL.as_bytes()).unwrap();
        assert_eq!(
            named.get(&format!("psr/log/{key}.zip")),
            Some(&String::new())
        );
        assert!(named.keys().any(|k| k.starts_with("acme/dev/")));
        assert!(!named.keys().any(|k| k.starts_with("acme/git/")));
        assert!(locked_archives("not json").is_empty());
    }

    // covers: K-5
    #[test]
    fn a_harvest_keeps_only_archives() {
        let hex = "a".repeat(40);
        assert!(Composer.keep(&format!("psr/log/{hex}.zip")));
        assert!(Composer.keep(&format!("acme/x/{hex}.tar.gz")));
        for left in [
            ".htaccess".to_string(),
            format!("psr/{hex}.zip"),
            format!("psr/log/{}.zip", "A".repeat(40)),
            format!("psr/log/{}.zip", "a".repeat(39)),
            format!("psr/log/{hex}"),
            format!("psr/log/.{hex}.zip.hushrun-seed"),
            format!(".tmp/log/{hex}.zip"),
        ] {
            assert!(!Composer.keep(&left), "{left}");
        }
        assert_eq!(
            Composer.package(&format!("psr/log/{hex}.zip")).as_deref(),
            Some("psr/log")
        );
    }

    // covers: K-7, K-10
    #[test]
    fn a_seed_takes_what_the_lock_names_and_checks_a_shasum_where_there_is_one() {
        let t = tempfile::tempdir().unwrap();
        let bytes = b"an archive";
        let key = sha1_hex(URL.as_bytes()).unwrap();
        let path = format!("psr/log/{key}.zip");
        let manifests = ["composer.lock".to_string()];
        // No shasum: copied on the signature alone.
        std::fs::write(t.path().join("composer.lock"), lock("")).unwrap();
        let admit = Composer.admit(t.path(), &manifests);
        assert_eq!(admit(&path, bytes), Admit::Copy);
        assert_eq!(
            admit(&format!("psr/log/{}.zip", "b".repeat(40)), bytes),
            Admit::NotLocked
        );
        // A shasum: only the archive it hashes.
        std::fs::write(
            t.path().join("composer.lock"),
            lock(&sha1_hex(bytes).unwrap().to_ascii_uppercase()),
        )
        .unwrap();
        let admit = Composer.admit(t.path(), &manifests);
        assert_eq!(admit(&path, bytes), Admit::Copy);
        assert_eq!(admit(&path, b"another archive"), Admit::Changed);
        assert_eq!(
            Composer.reader(Path::new("/p"), t.path(), &|_| None),
            Reader::Seed
        );
        assert_eq!(Composer.verified_by(), VerifiedBy::Signature);
    }
}
