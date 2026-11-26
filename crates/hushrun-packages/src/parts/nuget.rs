//! The `nuget` part: a NuGet global packages folder, read in place as a fallback folder through
//! `NUGET_FALLBACK_PACKAGES`. The variable replaces every fallback folder a NuGet.Config names, so
//! the reader carries the configs' list into it, first, and the part's joins after; a variable the
//! person set keeps its folders, first, the same way.

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
        match configs_fallbacks(checkout, env) {
            Err(why) => Reader::LeftAlone(why),
            Ok(folders) => {
                let mut value: Vec<String> = folders;
                if !value.iter().any(|f| f == ours) {
                    value.push(ours.to_string());
                }
                Reader::InPlace(vec![(
                    VARIABLE.to_string(),
                    OsString::from(value.join(";")),
                )])
            }
        }
    }
}

/// The NuGet.Configs the run's NuGet may read, the closest first: the checkout's root and each
/// folder above it, the user's (`~/.nuget/NuGet/NuGet.Config`, then `~/.nuget/config/*.config`),
/// then the machine's.
fn configs(checkout: &Path, env: Env<'_>) -> Vec<PathBuf> {
    let named = |dir: &Path, keep: &dyn Fn(&str) -> bool| -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| keep(&e.file_name().to_string_lossy().to_ascii_lowercase()))
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        found.sort();
        found
    };
    let is_config = |n: &str| n == "nuget.config";
    let any_config = |n: &str| n.ends_with(".config");
    let mut out: Vec<PathBuf> = checkout
        .ancestors()
        .flat_map(|dir| named(dir, &is_config).into_iter().take(1))
        .collect();
    let mut homes: Vec<PathBuf> = Vec::new();
    for home in ["DOTNET_CLI_HOME", "HOME"] {
        if let Some(home) = env(home).filter(|h| !h.is_empty()).map(PathBuf::from)
            && !homes.contains(&home)
        {
            homes.push(home);
        }
    }
    for home in homes {
        let nuget = home.join(".nuget");
        out.extend(named(&nuget.join("NuGet"), &is_config).into_iter().take(1));
        out.extend(named(&nuget.join("config"), &any_config));
    }
    let machine = env("NUGET_COMMON_APPLICATION_DATA")
        .filter(|h| !h.is_empty())
        .map_or_else(|| PathBuf::from("/etc/opt"), PathBuf::from);
    out.extend(named(&machine.join("NuGet/Config"), &any_config));
    out
}

/// K-10: the fallback folders the configs name, as NuGet merges them: the closest config's first,
/// in its order; a key a closer config named already is skipped; a relative folder is from its
/// config's folder; `<clear />` ends the list there. `Err` says why the list can't be read exactly,
/// and the part is left alone.
fn configs_fallbacks(checkout: &Path, env: Env<'_>) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = Vec::new();
    let mut folders: Vec<String> = Vec::new();
    for config in configs(checkout, env) {
        let Ok(text) = fs::read_to_string(&config) else {
            continue;
        };
        let cant = |what: &str| {
            format!(
                "{} names fallback folders with {what}, which hushrun can't carry into {VARIABLE}",
                config.display()
            )
        };
        let Some(section) = section(&text).map_err(|()| cant("markup it doesn't read"))? else {
            continue;
        };
        let (items, cleared) = items(section).map_err(|what| cant(what))?;
        let dir = config.parent().unwrap_or(Path::new("/"));
        for (key, value) in items {
            if keys.contains(&key) {
                continue;
            }
            let folder = if Path::new(&value).is_absolute() {
                PathBuf::from(&value)
            } else {
                dir.join(&value)
            };
            let folder = folder.to_string_lossy().into_owned();
            if folder.contains(';') {
                return Err(cant("a `;` in a folder"));
            }
            keys.push(key);
            folders.push(folder);
        }
        if cleared {
            break;
        }
    }
    Ok(folders)
}

/// The inside of a config's `<fallbackPackageFolders>`, if it has one.
fn section(text: &str) -> Result<Option<&str>, ()> {
    let Some(at) = text.find(&format!("<{FALLBACK_KEY}")) else {
        return Ok(None);
    };
    let rest = &text[at + FALLBACK_KEY.len() + 1..];
    let open = rest.find('>').ok_or(())?;
    if rest[..open].trim_end().ends_with('/') {
        return Ok(Some(""));
    }
    let rest = &rest[open + 1..];
    let close = rest.find(&format!("</{FALLBACK_KEY}")).ok_or(())?;
    if rest[close + 1..].contains(&format!("<{FALLBACK_KEY}")) {
        return Err(());
    }
    Ok(Some(&rest[..close]))
}

/// A section's `<add key value />` items after its last `<clear />`, and whether it had one.
fn items(section: &str) -> Result<(Vec<(String, String)>, bool), &'static str> {
    let mut out = Vec::new();
    let mut cleared = false;
    let mut rest = section;
    while let Some(at) = rest.find('<') {
        let end = rest[at..].find('>').ok_or("markup it doesn't read")? + at;
        let tag = rest[at + 1..end].trim().trim_end_matches('/').trim();
        rest = &rest[end + 1..];
        if tag.starts_with("!--") {
            continue;
        }
        let (name, attrs) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        match name {
            "clear" => {
                out.clear();
                cleared = true;
            }
            "add" => {
                let key = attribute(attrs, "key").ok_or("an item with no key")?;
                let value = attribute(attrs, "value").ok_or("an item with no value")?;
                if value.contains('%') || value.contains('&') || key.contains('&') {
                    return Err("a variable or an entity");
                }
                out.push((key, value));
            }
            _ => return Err("an item other than add or clear"),
        }
    }
    Ok((out, cleared))
}

/// The value of `name` among a tag's attributes, in either quote.
fn attribute(attrs: &str, name: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        let eq = rest.find('=')?;
        let key = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let quote = after.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let close = after[1..].find(quote)? + 1;
        if key == name {
            return Some(after[1..close].to_string());
        }
        rest = &after[close + 1..];
    }
}
