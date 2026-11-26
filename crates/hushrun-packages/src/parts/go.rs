//! The `go` part: `cache/download` of a Go module cache, served to Go as a registry (mechanism
//! B): `GOPROXY=file://<part>/download,` then the proxies Go would use without it. Go checks every
//! module it takes from there against `go.sum`, or against the checksum database when `go.sum`
//! lacks it, as it checks one it downloads. `GOPROXY` is the one variable the reader sets.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::part::{Env, Locks, Part, Reader, file_name};
use crate::store::VerifiedBy;

/// Go's variable that names where it downloads modules from.
pub const VARIABLE: &str = "GOPROXY";
/// The part's folder that holds the download cache, as a proxy lays it out.
pub const DOWNLOAD: &str = "download";

/// The `go` part.
pub struct Go;

impl Part for Go {
    fn name(&self) -> &'static str {
        "go"
    }

    fn tool(&self) -> &'static str {
        "Go"
    }

    fn manifests(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|path| {
                matches!(
                    file_name(path),
                    "go.mod" | "go.sum" | "go.work" | "go.work.sum"
                )
            })
            .cloned()
            .collect()
    }

    fn harvest_root(&self, cache: &Path) -> Option<(PathBuf, &'static str)> {
        let root = cache.join("cache/download");
        root.is_dir().then_some((root, DOWNLOAD))
    }

    fn keep(&self, path: &str) -> bool {
        // <module>/@v/<version>.info|.mod|.zip: not `list`, `.lock`, `.ziphash`, `.partial`, a
        // temporary file, or the checksum database's tiles (`sumdb/`), all of them Go's notes.
        let parts: Vec<&str> = path.split('/').collect();
        let [module @ .., at_v, file] = parts.as_slice() else {
            return false;
        };
        *at_v == "@v"
            && !module.is_empty()
            && module[0] != "sumdb"
            && !module.iter().chain([file]).any(|c| c.starts_with('.'))
            && [".info", ".mod", ".zip"].iter().any(|ext| {
                file.strip_suffix(ext)
                    .is_some_and(|v| v.starts_with('v') && !v.contains(".tmp"))
            })
    }

    fn package(&self, path: &str) -> Option<String> {
        let (module, version, _) = module_version(path)?;
        Some(format!("{module}@{version}"))
    }

    fn verified_by(&self) -> VerifiedBy {
        VerifiedBy::Lockfile
    }

    fn reader(&self, dir: &Path, _checkout: &Path, env: Env<'_>) -> Reader {
        if env(VARIABLE).is_some() {
            return Reader::LeftAlone(format!("{VARIABLE} is set already"));
        }
        reader_for(dir, settings().as_ref())
    }

    fn locks(&self, checkout: &Path, manifests: &[String]) -> Locks {
        let mut locks = Locks::new();
        for path in manifests
            .iter()
            .filter(|p| matches!(file_name(p), "go.sum" | "go.work.sum"))
        {
            let Ok(text) = fs::read_to_string(checkout.join(path)) else {
                continue;
            };
            for line in text.lines() {
                let mut words = line.split_whitespace();
                let (Some(module), Some(version), Some(hash), None) =
                    (words.next(), words.next(), words.next(), words.next())
                else {
                    continue;
                };
                let hashes = locks.entry(format!("{module}@{version}")).or_default();
                if !hashes.iter().any(|h| h == hash) {
                    hashes.push(hash.to_string());
                }
            }
        }
        locks
    }

    fn locked(&self, path: &str, locks: &Locks) -> Option<String> {
        let (module, version, ext) = module_version(path)?;
        let key = match ext {
            "zip" => format!("{module}@{version}"),
            "mod" => format!("{module}@{version}/go.mod"),
            _ => return None,
        };
        locks.get(&key)?.first().cloned()
    }
}

/// What Go would do without the part, as `go env` says: the proxies it uses, and whether it
/// checks every module against the checksum database when `go.sum` lacks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `GOPROXY`, as Go reads it from its environment file or its own default.
    pub proxy: String,
    /// The first of `GOSUMDB=off`, `GONOSUMDB` and `GOPRIVATE` that is set, if any.
    pub unchecked: Option<&'static str>,
}

/// K-10: the go reader, given what Go would do without it (`None`: Go couldn't say).
pub fn reader_for(dir: &Path, settings: Option<&Settings>) -> Reader {
    let Some(settings) = settings else {
        return Reader::LeftAlone("`go env` didn't run".to_string());
    };
    if let Some(why) = settings.unchecked {
        // Go takes a module go.sum lacks on no evidence there: the store's bytes would be
        // taken on their word.
        return Reader::LeftAlone(format!(
            "{why} is set, so Go takes some modules go.sum doesn't name unchecked"
        ));
    }
    let download = dir.join(DOWNLOAD);
    let Some(path) = download.to_str().filter(|p| {
        p.starts_with('/')
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-~+@".contains(&b))
    }) else {
        return Reader::LeftAlone(format!(
            "the store's folder, {}, has a character a GOPROXY address can't hold",
            dir.display()
        ));
    };
    let rest = settings.proxy.trim();
    let value = if rest.is_empty() {
        format!("file://{path}")
    } else {
        format!("file://{path},{rest}")
    };
    Reader::InPlace(vec![(VARIABLE.to_string(), OsString::from(value))])
}

/// What `go env` says of `GOPROXY`, `GOSUMDB`, `GONOSUMDB` and `GOPRIVATE`, Go's environment
/// file and its own default included. Run with this toolchain (`GOTOOLCHAIN=local`) outside any
/// module, so that no `go.mod` can make it download another.
pub fn settings() -> Option<Settings> {
    let out = std::process::Command::new("go")
        .args(["env", "GOPROXY", "GOSUMDB", "GONOSUMDB", "GOPRIVATE"])
        .env("GOTOOLCHAIN", "local")
        .current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8(out.stdout).ok()?;
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let [proxy, sumdb, nosumdb, private] = lines.as_slice() else {
        return None;
    };
    let unchecked = if *sumdb == "off" {
        Some("GOSUMDB=off")
    } else if !nosumdb.is_empty() {
        Some("GONOSUMDB")
    } else if !private.is_empty() {
        Some("GOPRIVATE")
    } else {
        None
    };
    Some(Settings {
        proxy: (*proxy).to_string(),
        unchecked,
    })
}

/// A cache file's module, version and extension, its path's case-escaping undone (`!x` is `X`).
fn module_version(path: &str) -> Option<(String, String, &str)> {
    let (module, file) = path.split_once("/@v/")?;
    let (version, ext) = file.rsplit_once('.')?;
    Some((unescape(module)?, unescape(version)?, ext))
}

/// Go's case-escaping of a module path or version undone: `!` before a lowercase letter is that
/// letter in upper case.
fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut bang = false;
    for c in text.chars() {
        if bang {
            if !c.is_ascii_lowercase() {
                return None;
            }
            out.push(c.to_ascii_uppercase());
            bang = false;
        } else if c == '!' {
            bang = true;
        } else {
            out.push(c);
        }
    }
    (!bang).then_some(out)
}
