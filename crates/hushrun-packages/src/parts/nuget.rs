//! The `nuget` part: a NuGet global packages folder, read in place as a fallback folder through
//! `NUGET_FALLBACK_PACKAGES`. The variable replaces every fallback folder a NuGet.Config names, so
//! the part is left alone where a config names one; a variable the person set keeps its folders,
//! first, and the part's joins after them.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::part::{Env, Part, Reader, file_name};
use crate::store::VerifiedBy;

/// NuGet's variable for fallback package folders, `;`-separated.
pub const VARIABLE: &str = "NUGET_FALLBACK_PACKAGES";
/// The part's folder under which the packages are.
pub const PACKAGES: &str = "packages";
/// The NuGet.Config key whose folders the variable replaces.
const FALLBACK_KEY: &str = "fallbackPackageFolders";

/// The `nuget` part.
pub struct Nuget;

impl Part for Nuget {
    fn name(&self) -> &'static str {
        "nuget"
    }

    fn tool(&self) -> &'static str {
        "NuGet"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| {
                let name = file_name(path);
                let lower = name.to_ascii_lowercase();
                [".csproj", ".fsproj", ".vbproj"]
                    .iter()
                    .any(|ext| lower.ends_with(ext))
                    || lower == "nuget.config"
                    || matches!(
                        name,
                        "packages.lock.json"
                            | "packages.config"
                            | "Directory.Packages.props"
                            | "Directory.Build.props"
                            | "Directory.Build.targets"
                            | "global.json"
                    )
                    || path.as_str() == ".config/dotnet-tools.json"
            })
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        cache.is_dir().then(|| (cache.to_path_buf(), PACKAGES))
    }

    fn keep(&self, path: &str) -> bool {
        // <id>/<version>/<file>…: a package's folder holds only what NuGet extracted from it, its
        // `.nupkg`, `.nuspec`, hash and the mark. Nothing above a version's folder is a package's
        // (`.tools`, a folder half extracted under a random name).
        let mut parts = path.split('/');
        let (Some(id), Some(version), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
            return false;
        };
        !id.starts_with('.')
            && version.starts_with(|c: char| c.is_ascii_digit())
            && !file_name(path).ends_with(".lock")
    }

    fn package(&self, path: &str) -> Option<String> {
        let mut parts = path.split('/');
        let (id, version) = (parts.next()?, parts.next()?);
        parts.next()?;
        Some(format!("{id}/{version}"))
    }

    fn verified_by(&self) -> VerifiedBy {
        // NuGet hashes no file it reads from a fallback folder: it compares a lockfile's hash
        // with the `.nupkg.metadata` beside the package, never with the bytes.
        VerifiedBy::Signature
    }

    fn reader(&self, dir: &Path, checkout: &Path, env: Env<'_>) -> Reader {
        let packages = dir.join(PACKAGES);
        let Some(ours) = packages.to_str().filter(|p| !p.contains(';')) else {
            return Reader::LeftAlone(format!(
                "the store's folder, {}, has a `;`, which {VARIABLE} splits on",
                dir.display()
            ));
        };
        if let Some(theirs) = env(VARIABLE) {
            let theirs = theirs.to_string_lossy();
            let value = if theirs.split(';').any(|f| f == ours) {
                theirs.into_owned()
            } else {
                format!("{};{ours}", theirs.trim_end_matches(';'))
            };
            return Reader::InPlace(vec![(VARIABLE.to_string(), OsString::from(value))]);
        }
        if let Some(config) = config_naming_fallbacks(checkout, env) {
            return Reader::LeftAlone(format!(
                "{} names fallback folders, which {VARIABLE} would replace",
                config.display()
            ));
        }
        Reader::InPlace(vec![(VARIABLE.to_string(), OsString::from(ours))])
    }
}

/// A NuGet.Config the run's NuGet may read that names fallback folders: in the checkout's root or
/// a folder above it, the user's (`~/.nuget/NuGet`, `~/.nuget/config`) or the machine's.
fn config_naming_fallbacks(checkout: &Path, env: Env<'_>) -> Option<PathBuf> {
    let mut configs: Vec<PathBuf> = checkout
        .ancestors()
        .flat_map(|dir| fs::read_dir(dir).into_iter().flatten().flatten())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("nuget.config")
        })
        .map(|e| e.path())
        .collect();
    let mut folders: Vec<PathBuf> = ["HOME", "DOTNET_CLI_HOME"]
        .into_iter()
        .filter_map(|home| env(home).filter(|h| !h.is_empty()))
        .flat_map(|home| {
            let nuget = PathBuf::from(home).join(".nuget");
            [nuget.join("NuGet"), nuget.join("config")]
        })
        .collect();
    let machine = env("NUGET_COMMON_APPLICATION_DATA").filter(|h| !h.is_empty());
    folders.push(
        machine
            .map_or_else(|| PathBuf::from("/etc/opt"), PathBuf::from)
            .join("NuGet/Config"),
    );
    for dir in folders {
        configs.extend(
            fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .ends_with(".config")
                })
                .map(|e| e.path()),
        );
    }
    configs
        .into_iter()
        .find(|c| fs::read_to_string(c).is_ok_and(|t| t.contains(FALLBACK_KEY)))
}
