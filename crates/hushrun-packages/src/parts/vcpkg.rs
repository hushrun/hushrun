//! The `vcpkg` part: vcpkg's asset cache, each file named by its SHA-512, read in place as one more
//! asset source in `X_VCPKG_ASSET_SOURCES` (`x-azurl,file://<part>/assets,,read`). vcpkg checks
//! every file it takes from an asset source against the SHA-512 its portfile names, and downloads
//! from the file's own source when the file differs or is missing: the tool checks the part.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::part::{Env, Part, Reader, splits_safely};
use crate::store::VerifiedBy;

/// vcpkg's list of asset sources.
pub const VARIABLE: &str = "X_VCPKG_ASSET_SOURCES";
/// vcpkg's folder for what it downloaded.
pub const DOWNLOADS: &str = "VCPKG_DOWNLOADS";
/// vcpkg's list of binary caches, which a bake's fetch clears.
pub const BINARY_SOURCES: &str = "VCPKG_BINARY_SOURCES";
/// The part's folder that holds the assets, and the one a fetch's cache holds them in.
pub const ASSETS: &str = "assets";

/// The `vcpkg` part.
pub struct Vcpkg;

impl Part for Vcpkg {
    fn name(&self) -> &'static str {
        "vcpkg"
    }

    fn tool(&self) -> &'static str {
        "vcpkg"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| {
                matches!(
                    crate::part::file_name(path),
                    "vcpkg.json" | "vcpkg-configuration.json"
                )
            })
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        // A bake's fetch keeps its assets in `assets/`; a harvest names an asset cache itself.
        let fetched = cache.join(ASSETS);
        if fetched.is_dir() {
            return Some((fetched, ASSETS));
        }
        cache.is_dir().then(|| (cache.to_path_buf(), ASSETS))
    }

    fn fetch_vars_in(
        &self,
        cache: &Path,
        env: Env<'_>,
    ) -> Result<Vec<(&'static str, OsString)>, String> {
        let person = person_words(env)?;
        let assets = cache.join(ASSETS);
        let url = url_of(&assets)
            .ok_or_else(|| format!("the bake's folder, {}, can't be a URL", cache.display()))?;
        fs::create_dir_all(&assets).map_err(|e| format!("{}: {e}", assets.display()))?;
        Ok(vec![
            (
                VARIABLE,
                joined(person.as_deref(), &format!("x-azurl,{url},,readwrite")).into(),
            ),
            (DOWNLOADS, cache.join("downloads").into()),
            (BINARY_SOURCES, "clear".into()),
        ])
    }

    fn keep(&self, path: &str) -> bool {
        path.len() == 128 && path.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

    fn package(&self, _path: &str) -> Option<String> {
        None
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, dir: &Path, _checkout: &Path, env: Env<'_>) -> Reader {
        let person = match person_words(env) {
            Ok(person) => person,
            Err(why) => return Reader::LeftAlone(why),
        };
        let assets = dir.join(ASSETS);
        let Some(url) = url_of(&assets) else {
            return Reader::LeftAlone(format!(
                "the store's folder, {}, has a character {VARIABLE} can't hold",
                dir.display()
            ));
        };
        Reader::InPlace(vec![(
            VARIABLE.to_string(),
            joined(person.as_deref(), &format!("x-azurl,{url},,read")).into(),
        )])
    }
}

/// The person's `X_VCPKG_ASSET_SOURCES`, when it names no asset source of its own: vcpkg reads at
/// most one, and refuses to run with two. Its other words (`clear`, `x-block-origin`) are kept.
fn person_words(env: Env<'_>) -> Result<Option<String>, String> {
    let Some(value) = env(VARIABLE) else {
        return Ok(None);
    };
    let value = value.to_string_lossy().into_owned();
    // A backtick escapes a separator: such a value is the person's alone.
    let names_source = value.contains('`')
        || value.split(';').any(|entry| {
            let provider = entry.split(',').next().unwrap_or_default().trim();
            !matches!(provider, "" | "clear" | "x-block-origin")
        });
    if names_source {
        return Err(format!(
            "{VARIABLE} names an asset source already, and vcpkg reads only one"
        ));
    }
    Ok(Some(value))
}

/// `file://` and `folder`, when the folder's path can ride in the list: absolute, and no white
/// space, quote, backslash or character the list or a URL gives a meaning.
fn url_of(folder: &Path) -> Option<String> {
    let text = folder.to_str()?;
    (folder.is_absolute()
        && splits_safely(folder)
        && !text
            .chars()
            .any(|c| matches!(c, ',' | ';' | '`' | '%' | '#' | '?')))
    .then(|| format!("file://{text}"))
}

/// The person's words, then `entry`.
fn joined(person: Option<&str>, entry: &str) -> String {
    match person
        .map(|p| p.trim().trim_end_matches(';'))
        .filter(|p| !p.is_empty())
    {
        Some(person) => format!("{person};{entry}"),
        None => entry.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<OsString> {
        None
    }

    const SHA: &str = "3463bc11d9008ea14eea1df6ad62f21debbafa53443f688f767856008361c09ad471dd4b0fd9752bd77e6f08771fe4517860750bd66d8c2b6cfd5111a8d3293f";

    // covers: K-10
    #[test]
    fn its_manifest_set_is_every_vcpkg_json_and_configuration() {
        let tracked: Vec<String> = [
            "vcpkg.json",
            "vcpkg-configuration.json",
            "ports/hello/vcpkg.json",
            "CMakeLists.txt",
            "conanfile.txt",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            Vcpkg.manifests(&tracked),
            [
                "vcpkg.json",
                "vcpkg-configuration.json",
                "ports/hello/vcpkg.json"
            ]
        );
    }

    // covers: K-10
    #[test]
    fn it_keeps_only_files_named_by_their_sha512() {
        assert!(Vcpkg.keep(SHA));
        for left in [
            &SHA.to_uppercase(),
            &SHA[1..],
            &format!("{SHA}.part"),
            &format!("x/{SHA}"),
            "hello-1.0.tar.gz",
        ] {
            assert!(!Vcpkg.keep(left), "{left}");
        }
        assert_eq!(Vcpkg.package(SHA), None);
        assert_eq!(Vcpkg.verified_by(), VerifiedBy::Lockfile);
    }

    // covers: K-10
    #[test]
    fn the_reader_adds_one_read_source_and_keeps_the_person_s_words() {
        let dir = Path::new("/hushrun-packages/parts/vcpkg");
        let ours = "x-azurl,file:///hushrun-packages/parts/vcpkg/assets,,read";
        assert_eq!(
            Vcpkg.reader(dir, Path::new("/c"), &none),
            Reader::InPlace(vec![(VARIABLE.into(), ours.into())])
        );
        let block = |n: &str| (n == VARIABLE).then(|| OsString::from("clear;x-block-origin;"));
        assert_eq!(
            Vcpkg.reader(dir, Path::new("/c"), &block),
            Reader::InPlace(vec![(
                VARIABLE.into(),
                format!("clear;x-block-origin;{ours}").into()
            )])
        );
        // A source of the person's: vcpkg refuses a second, so the part is left alone.
        for theirs in [
            "x-azurl,https://mirror.example/assets,,read",
            "x-script,curl {url} -o {dst}",
            "x-block-origin;x-azurl,file:///a`;b,,read",
        ] {
            let set = |n: &str| (n == VARIABLE).then(|| OsString::from(theirs));
            assert_eq!(
                Vcpkg.reader(dir, Path::new("/c"), &set),
                Reader::LeftAlone(
                    "X_VCPKG_ASSET_SOURCES names an asset source already, and vcpkg reads only one"
                        .into()
                ),
                "{theirs}"
            );
        }
        for odd in ["/tmp/my packages", "/tmp/a,b", "/tmp/a;b", "/tmp/50%"] {
            assert!(
                matches!(
                    Vcpkg.reader(Path::new(odd), Path::new("/c"), &none),
                    Reader::LeftAlone(_)
                ),
                "{odd}"
            );
        }
    }

    // covers: K-10
    #[test]
    fn a_fetch_writes_its_assets_and_downloads_into_the_bake_s_folder() {
        let cache = tempfile::tempdir().unwrap();
        let dir = cache.path().join("vcpkg");
        let vars = Vcpkg.fetch_vars_in(&dir, &none).unwrap();
        assert_eq!(
            vars,
            vec![
                (
                    VARIABLE,
                    format!("x-azurl,file://{},,readwrite", dir.join("assets").display()).into()
                ),
                (DOWNLOADS, dir.join("downloads").into_os_string()),
                (BINARY_SOURCES, "clear".into()),
            ]
        );
        // The fetch's folder holds `assets/`, which a harvest takes; a harvest of an asset cache
        // takes that folder itself.
        assert_eq!(Vcpkg.harvest_root(&dir), Some((dir.join("assets"), ASSETS)));
        let theirs = cache.path().join("theirs");
        fs::create_dir_all(&theirs).unwrap();
        assert_eq!(Vcpkg.harvest_root(&theirs), Some((theirs.clone(), ASSETS)));
        assert_eq!(Vcpkg.harvest_root(&cache.path().join("none")), None);
        let set = |n: &str| (n == VARIABLE).then(|| OsString::from("x-azurl,https://m,,read"));
        assert!(Vcpkg.fetch_vars_in(&dir, &set).is_err());
    }
}
