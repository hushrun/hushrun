//! The compiler's identity (B-17), by content, so that the same toolchain installed on two
//! machines, under any folder or name, is one compiler, and a toolchain replaced under the same
//! name is a new one. A rustup proxy with no toolchain named is a reason to pass through.
//!
//! The identity is: the text of `rustc -vV`; the SHA-256 of the compiler's own binary (under
//! rustup the toolchain's `bin/rustc`, never the proxy, which is rustup itself); the SHA-256 of
//! each compiler library in the toolchain's `lib/` (`librustc_driver-*`, where nearly all of the
//! compiler lives, and `libLLVM*`, which it loads on some hosts), by file name; and the SHA-256 of
//! each file of the standard library, `<sysroot>/lib/rustlib/<triple>/lib/`, by its path there,
//! for the host and for the call's `--target`. No path, size, date or variable is part of it.
//! It also knows the compiler's built-in targets (`rustc --print target-list`): a `--target`
//! outside them loads a target file (B-4).
//!
//! Hashing a toolchain reads a few hundred megabytes, so it happens once per toolchain install
//! and target, not once per call: everything is kept in a memo on this machine, named by what
//! cargo itself watches for its own cache of `rustc -vV` (`util/rustc.rs`, `rustc_fingerprint`):
//! the compiler's path, size and dates; `RUSTUP_HOME` and `RUSTUP_TOOLCHAIN`; under rustup, the
//! dates of the toolchain's own `rustc`, because `rustup update nightly` changes neither the
//! proxy nor the variable; and the `lib/` folders' own stamps. That name never enters a key.
//! Each call stats the compiler's files, each compiler library the memo names, each `lib/`
//! folder and each standard library folder: an installer adds, removes or renames files there,
//! which changes the folder's dates, and a memo whose folders changed is computed again. A
//! standard library file rewritten in place, keeping its folder's dates, is not seen; a compiler
//! library rewritten in place is, by its own stat. A memo is kept only when all it is trusted by
//! is dated before the memo file: Linux dates files once a tick, and a folder changed twice in a
//! tick would otherwise keep a stamp the memo trusts. Calls that find no memo at once (a cold
//! build starts many) take a lock on its name first, so one of them hashes and the others read
//! what it wrote.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use hushrun_core::jcs;
use hushrun_digest::cache::Stat;
use serde_json::{Value, json};

/// Names the form of the identity, so a change to what goes in gives new identities. The memo's
/// name carries it too, so a memo of an older form is never read.
const SCHEMA: &str = "hushrun.build.compiler/4";

/// The prefixes of the compiler libraries' file names in a toolchain's `lib/`: the driver, and
/// LLVM where the driver loads it as a library of its own.
const DRIVER_PREFIXES: [&str; 2] = ["librustc_driver-", "libLLVM"];

/// Which compiler a call is in front of (B-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The SHA-256 of the JCS form of the compiler's content: the output of `rustc -vV`, the
    /// compiler's binary, its libraries and the standard library, each by SHA-256. It names no
    /// folder of this machine.
    pub digest: Digest,
    /// The parts of [`Identity::digest`], each by its own digest, to tell which one differs
    /// between two machines.
    pub parts: Parts,
    /// What `rustc -vV` printed.
    pub verbose_version: String,
    /// The `host:` line of that output: the target the compiler runs on.
    pub host: String,
    /// The `release:` line of that output, such as `1.94.0`.
    pub release: String,
    /// The compiler's built-in targets, from `rustc --print target-list`, sorted.
    pub targets: Vec<String>,
    /// The sysroot rustc uses: the toolchain's folder under rustup, else the folder above the
    /// compiler's own `bin/`. A link's `-B` folder outside it is not keyed (B-18).
    pub sysroot: Option<PathBuf>,
}

impl Identity {
    /// Whether `target` is one of the compiler's built-in targets. Any other name makes rustc
    /// look for `<name>.json` in `RUST_TARGET_PATH` or the working directory (B-4).
    pub fn builds_for(&self, target: &str) -> bool {
        self.targets
            .binary_search_by(|t| t.as_str().cmp(target))
            .is_ok()
    }
}

/// The parts of a compiler's identity, each by the SHA-256 of its JCS form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Parts {
    /// The text of `rustc -vV`.
    pub verbose_version: Digest,
    /// The compiler's binary.
    pub rustc: Digest,
    /// The compiler libraries, by name and SHA-256.
    pub rustc_driver: Digest,
    /// The standard library of each triple, by its files' names and SHA-256.
    pub std: Vec<(String, Digest)>,
}

/// What a memo holds besides its key.
struct Memo {
    verbose_version: String,
    host: String,
    release: String,
    targets: Vec<String>,
    std: Vec<Value>,
    /// Every compiler library in the `lib/` folders, by path, size and dates.
    drivers: Vec<Value>,
    /// What the identity is made of, by content: `rustc`, `rustc_driver` and `std`.
    content: Value,
}

/// Why the compiler's identity can't be established. The wrapper passes the call through.
#[derive(Debug, thiserror::Error)]
pub enum Unknown {
    /// A rustup proxy, with `RUSTUP_TOOLCHAIN` or `RUSTUP_HOME` unset: it would pick a toolchain
    /// from the working directory, which the identity can't see.
    #[error("{} is a rustup proxy and RUSTUP_TOOLCHAIN or RUSTUP_HOME is not set", .0.display())]
    ProxyWithoutToolchain(PathBuf),
    /// A bare name that is not found on `PATH`.
    #[error("no {} on PATH", .0.display())]
    NotOnPath(PathBuf),
    /// A file of the compiler that can't be found or read.
    #[error("can't read {}: {source}", .path.display())]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// What reading it said.
        source: io::Error,
    },
    /// A path or variable that isn't UTF-8, so it has no exact form in the identity.
    #[error("{0} is not UTF-8")]
    NotUtf8(String),
    /// `RUSTUP_TOOLCHAIN` names a toolchain by a relative path, which depends on the folder.
    #[error("RUSTUP_TOOLCHAIN names a relative path: {0}")]
    RelativeToolchain(String),
    /// `rustc -vV` or `rustc --print target-list` could not be run.
    #[error("running {} -vV or --print target-list: {source}", .path.display())]
    VersionFailed {
        /// The compiler as given.
        path: PathBuf,
        /// What running it said.
        source: io::Error,
    },
    /// `rustc -vV` printed no `host:` or no `release:` line, so this is not a rustc.
    #[error("{} -vV printed no host or release", .0.display())]
    NotRustc(PathBuf),
    /// The identity has no canonical form.
    #[error("the compiler's identity has no canonical form: {0}")]
    Canonical(#[from] jcs::JcsError),
}

/// The identity of the compiler at `rustc`, the path cargo handed the wrapper.
///
/// `target` is the call's `--target`. `env` looks up a variable of the call (`PATH`,
/// `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`, `CARGO_HOME`, `HOME`). `run` runs `<rustc> -vV` and
/// `targets` runs `<rustc> --print target-list`, each returning what it printed; they are called
/// only when `memo_dir` has no memo for this compiler and target, or one whose standard library
/// folders changed. A memo that can't be read or written is no error: they run instead.
pub fn identity(
    rustc: &Path,
    target: Option<&str>,
    env: &dyn Fn(&str) -> Option<OsString>,
    memo_dir: &Path,
    run: &dyn Fn(&Path) -> io::Result<String>,
    targets: &dyn Fn(&Path) -> io::Result<String>,
) -> Result<Identity, Unknown> {
    let located = locate(rustc, env)?;
    let resolved = fs::canonicalize(&located).map_err(|source| Unknown::Unreadable {
        path: located.clone(),
        source,
    })?;
    let meta = fs::metadata(&resolved).map_err(|source| Unknown::Unreadable {
        path: resolved.clone(),
        source,
    })?;

    let rustup_home = set(env, "RUSTUP_HOME")?;
    let rustup_toolchain = set(env, "RUSTUP_TOOLCHAIN")?;
    let toolchain = match (&rustup_home, &rustup_toolchain) {
        (Some(home), Some(name)) => Some(toolchain_dir(home, name)?),
        _ if is_proxy(&located, &resolved, &meta, env) => {
            return Err(Unknown::ProxyWithoutToolchain(located));
        }
        _ => None,
    };

    // The library sits in `lib/` beside `bin/`: the toolchain's under rustup, and the sysroot of
    // the compiler given in every case, so a plain path is covered too.
    let mut libraries = Vec::new();
    if let Some(sysroot) = resolved.parent().and_then(Path::parent) {
        libraries.push(sysroot.join("lib"));
    }
    // The sysroot rustc uses: the toolchain's under rustup, else the folder above the compiler's
    // own `bin/`.
    let sysroot = toolchain.clone().or_else(|| {
        resolved
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    });
    let toolchain_rustc = match &toolchain {
        Some(dir) => {
            let lib = dir.join("lib");
            if !libraries.contains(&lib) {
                libraries.push(lib);
            }
            let real = dir
                .join("bin")
                .join("rustc")
                .with_extension(std::env::consts::EXE_EXTENSION);
            let real_meta = fs::metadata(&real).map_err(|source| Unknown::Unreadable {
                path: real.clone(),
                source,
            })?;
            Some(stat_value(&real, &real_meta)?)
        }
        None => None,
    };
    // The `lib/` folders by their stamps: their listing is in the memo, which a change of
    // either names anew (cost: one stat per folder instead of a listing per call).
    let lib_stamps: Vec<Value> = libraries
        .iter()
        .map(|lib| {
            Ok(json!({
                "folder": lib.to_str().ok_or_else(|| Unknown::NotUtf8(lib.to_string_lossy().into_owned()))?,
                "stamp": folder_stamp(lib),
            }))
        })
        .collect::<Result<_, Unknown>>()?;

    // The memo's name: what cargo watches for its own cache of `rustc -vV`. It holds this
    // machine's paths and dates, and never enters a key.
    let facts = json!({
        "schema": SCHEMA,
        "rustc": stat_value(&resolved, &meta)?,
        "rustup_home": rustup_home,
        "rustup_toolchain": rustup_toolchain,
        "toolchain_rustc": toolchain_rustc,
        "libraries": lib_stamps,
        "target": target,
    });
    let memo_key = jcs::digest(&facts)?;
    let memo = memo_dir.join(format!("{}.json", memo_key.hex()));
    let std_dirs = |host: &str| -> Vec<(String, PathBuf)> {
        let mut triples = vec![host.to_string()];
        if let Some(target) = target
            && target != host
        {
            triples.push(target.to_string());
        }
        triples
            .into_iter()
            .filter_map(|triple| {
                let dir = sysroot
                    .as_ref()?
                    .join("lib/rustlib")
                    .join(&triple)
                    .join("lib");
                Some((triple, dir))
            })
            .collect()
    };
    // The compiler's own binary: the toolchain's under rustup, never the proxy, which is
    // rustup itself and differs between machines that run the same compiler.
    let compiler_file = match &toolchain {
        Some(dir) => dir
            .join("bin")
            .join("rustc")
            .with_extension(std::env::consts::EXE_EXTENSION),
        None => resolved.clone(),
    };

    let holds = |found: &Memo| {
        // One stat per standard library folder: the memo holds while none changed.
        let dirs = std_dirs(&found.host);
        let std_holds = dirs.len() == found.std.len()
            && dirs.iter().zip(&found.std).all(|((triple, dir), kept)| {
                kept.get("triple").and_then(Value::as_str) == Some(triple.as_str())
                    && kept.get("folder").and_then(Value::as_str) == Some(&folder_stamp(dir))
            });
        // And one per compiler library it names: rewritten in place, it is a new compiler.
        std_holds
            && found.drivers.iter().all(|kept| {
                kept.get("path")
                    .and_then(Value::as_str)
                    .map(Path::new)
                    .and_then(|path| stat_value(path, &fs::metadata(path).ok()?).ok())
                    .as_ref()
                    == Some(kept)
            })
    };
    let compute = || -> Result<Memo, Unknown> {
        let out = run(rustc).map_err(|source| Unknown::VersionFailed {
            path: rustc.to_path_buf(),
            source,
        })?;
        let (host, release) =
            parse_verbose(&out).ok_or_else(|| Unknown::NotRustc(rustc.to_path_buf()))?;
        let list = targets(rustc).map_err(|source| Unknown::VersionFailed {
            path: rustc.to_path_buf(),
            source,
        })?;
        let std = std_dirs(&host)
            .iter()
            .map(|(triple, dir)| std_listing(triple, dir))
            .collect::<Result<Vec<_>, _>>()?;
        let mut drivers = Vec::new();
        for lib in &libraries {
            drivers.extend(driver_stats(lib)?);
        }
        // Every folder and file whose stat the memo is trusted by, for the racy check.
        let mut relied: Vec<PathBuf> = libraries.clone();
        relied.push(resolved.clone());
        relied.push(compiler_file.clone());
        for (_, dir) in std_dirs(&host) {
            if let Ok(entries) = fs::read_dir(&dir) {
                relied.extend(entries.filter_map(|e| e.ok().map(|e| e.path())));
            }
            relied.push(dir);
        }
        relied.extend(
            drivers
                .iter()
                .filter_map(|d| d.get("path").and_then(Value::as_str).map(PathBuf::from)),
        );
        // The content, read once per install: the compiler, its libraries by file name (the
        // same file may be listed under two forms of one folder), the standard libraries.
        let mut libs: Vec<(String, String)> = Vec::new();
        for kept in &drivers {
            let Some(path) = kept.get("path").and_then(Value::as_str).map(Path::new) else {
                continue;
            };
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| Unknown::NotUtf8(path.to_string_lossy().into_owned()))?;
            libs.push((name.to_string(), sha256(path)?.to_string()));
        }
        libs.sort();
        libs.dedup();
        let std_content = std_dirs(&host)
            .iter()
            .map(|(triple, dir)| Ok(json!({ "triple": triple, "files": std_files(dir)? })))
            .collect::<Result<Vec<_>, Unknown>>()?;
        let content = json!({
            "rustc": sha256(&compiler_file)?.to_string(),
            "rustc_driver": libs,
            "std": std_content,
        });
        let fresh = Memo {
            verbose_version: out,
            host,
            release,
            targets: parse_targets(&list),
            std,
            drivers,
            content,
        };
        // A memo not written only means it is computed again next time.
        let _ = write_memo(memo_dir, &memo, &memo_key, &fresh, newest(&relied));
        Ok(fresh)
    };
    let memo_found = match read_memo(&memo, &memo_key).filter(holds) {
        Some(found) => found,
        None => {
            // One call hashes the toolchain; the others that started at once wait for it and
            // read its memo. Without a lock (it can't be taken here), each computes its own.
            let _lock = MemoLock::take(memo_dir, &memo_key);
            match read_memo(&memo, &memo_key).filter(holds) {
                Some(found) => found,
                None => compute()?,
            }
        }
    };

    let content = &memo_found.content;
    let part = |name: &str| content.get(name).cloned().unwrap_or(Value::Null);
    let std = part("std");
    let parts = Parts {
        verbose_version: jcs::digest(&Value::String(memo_found.verbose_version.clone()))?,
        rustc: jcs::digest(&part("rustc"))?,
        rustc_driver: jcs::digest(&part("rustc_driver"))?,
        std: std
            .as_array()
            .into_iter()
            .flatten()
            .map(|kept| {
                let triple = kept.get("triple").and_then(Value::as_str).unwrap_or("");
                let files = kept.get("files").cloned().unwrap_or(Value::Null);
                Ok((triple.to_string(), jcs::digest(&files)?))
            })
            .collect::<Result<_, Unknown>>()?,
    };
    // The identity: content only. No path, size, date or variable of this machine.
    let identity = json!({
        "schema": SCHEMA,
        "verbose_version": memo_found.verbose_version,
        "rustc": part("rustc"),
        "rustc_driver": part("rustc_driver"),
        "std": std,
        "target": target,
    });
    Ok(Identity {
        digest: jcs::digest(&identity)?,
        parts,
        verbose_version: memo_found.verbose_version,
        host: memo_found.host,
        release: memo_found.release,
        targets: memo_found.targets,
        sysroot,
    })
}

/// A lock on a memo's name while one call computes it, removed when dropped. `None` when it
/// can't be taken: the call then computes the memo itself.
struct MemoLock {
    file: fs::File,
    path: PathBuf,
}

impl MemoLock {
    fn take(dir: &Path, key: &Digest) -> Option<MemoLock> {
        fs::create_dir_all(dir).ok()?;
        let path = dir.join(format!(".{}.lock", key.hex()));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .ok()?;
        file.lock().ok()?;
        Some(MemoLock { file, path })
    }
}

impl Drop for MemoLock {
    fn drop(&mut self) {
        // Removed while still held: a call that comes later reads the memo, written by now.
        let _ = fs::remove_file(&self.path);
        let _ = self.file.unlock();
    }
}

/// The SHA-256 of a file of the compiler.
fn sha256(path: &Path) -> Result<Digest, Unknown> {
    hushrun_digest::hash::hash_file(path)
        .map(|(sha, _)| Digest::from_bytes(sha))
        .map_err(|source| Unknown::Unreadable {
            path: path.to_path_buf(),
            source,
        })
}

/// Every file under a standard library folder, by its path there (`/`-separated) and SHA-256,
/// in path order. A folder that doesn't exist holds none.
fn std_files(dir: &Path) -> Result<Vec<(String, String)>, Unknown> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((folder, prefix)) = stack.pop() {
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound && folder == dir => break,
            Err(source) => {
                return Err(Unknown::Unreadable {
                    path: folder,
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| Unknown::Unreadable {
                path: folder.clone(),
                source,
            })?;
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|name| Unknown::NotUtf8(name.to_string_lossy().into_owned()))?;
            let rest = format!("{prefix}{name}");
            if fs::metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                stack.push((path, format!("{rest}/")));
            } else {
                out.push((rest, sha256(&path)?.to_string()));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// A folder's dates and inode as one text, or `absent`: what changes when files are added,
/// removed or renamed in it.
fn folder_stamp(dir: &Path) -> String {
    match fs::metadata(dir) {
        Ok(meta) => {
            let stat = Stat::of(&meta);
            format!("{} {} {}", stat.mtime_ns, stat.ctime_ns, stat.inode)
        }
        Err(_) => "absent".to_string(),
    }
}

/// The standard library folder of one triple: its stamp, and each entry's name, size and
/// modification time, in name order. A folder that doesn't exist holds nothing: rustc fails by
/// itself for a target that isn't installed.
fn std_listing(triple: &str, dir: &Path) -> Result<Value, Unknown> {
    let stamp = folder_stamp(dir);
    let mut files = Vec::new();
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|source| Unknown::Unreadable {
                    path: dir.to_path_buf(),
                    source,
                })?;
                let path = entry.path();
                let meta = fs::metadata(&path).map_err(|source| Unknown::Unreadable {
                    path: path.clone(),
                    source,
                })?;
                let stat = Stat::of(&meta);
                files.push((
                    entry.file_name().to_string_lossy().into_owned(),
                    stat.size.to_string(),
                    stat.mtime_ns.to_string(),
                ));
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Unknown::Unreadable {
                path: dir.to_path_buf(),
                source,
            });
        }
    }
    files.sort();
    Ok(json!({
        "triple": triple,
        "folder": stamp,
        "files": files.into_iter().map(|(n, s, m)| json!([n, s, m])).collect::<Vec<_>>(),
    }))
}

/// The lines of `rustc --print target-list`, sorted.
fn parse_targets(out: &str) -> Vec<String> {
    let mut targets: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

/// The compiler's file: a bare name is looked up on `PATH`, as cargo's `resolve_executable`
/// does; anything else is taken as given.
fn locate(rustc: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Result<PathBuf, Unknown> {
    if rustc.components().count() != 1 {
        return Ok(rustc.to_path_buf());
    }
    let paths = env("PATH").ok_or_else(|| Unknown::NotOnPath(rustc.to_path_buf()))?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(rustc);
        if candidate.is_file() {
            return Ok(candidate);
        }
        let exe = candidate.with_extension(std::env::consts::EXE_EXTENSION);
        if !std::env::consts::EXE_EXTENSION.is_empty() && exe.is_file() {
            return Ok(exe);
        }
    }
    Err(Unknown::NotOnPath(rustc.to_path_buf()))
}

/// A variable's value, or nothing when it is unset or empty.
fn set(env: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Result<Option<String>, Unknown> {
    match env(name) {
        None => Ok(None),
        Some(value) if value.is_empty() => Ok(None),
        Some(value) => value
            .into_string()
            .map(Some)
            .map_err(|_| Unknown::NotUtf8(name.to_string())),
    }
}

/// The toolchain's folder. A toolchain named by an absolute path is that folder, as rustup
/// allows; a name is a folder under `<rustup home>/toolchains`.
fn toolchain_dir(home: &str, name: &str) -> Result<PathBuf, Unknown> {
    if name.contains(['/', '\\']) {
        let path = Path::new(name);
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        return Err(Unknown::RelativeToolchain(name.to_string()));
    }
    Ok(Path::new(home).join("toolchains").join(name))
}

/// Whether the compiler is a rustup proxy, which picks its toolchain at each call. Cargo's two
/// tests: the path is `<cargo home>/bin/rustc` (`rustc_fingerprint`), or it is the same size as
/// the `rustup` beside it, since the proxies are links to one binary (`get_tool`). A proxy that
/// is a symbolic link to `rustup` or `rustup-init` resolves to it.
fn is_proxy(
    located: &Path,
    resolved: &Path,
    meta: &fs::Metadata,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> bool {
    let rustc_exe = Path::new("rustc").with_extension(std::env::consts::EXE_EXTENSION);
    let cargo_home = env("CARGO_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".cargo"))
        });
    if cargo_home.is_some_and(|home| located == home.join("bin").join(&rustc_exe)) {
        return true;
    }
    let stem = resolved.file_stem().and_then(OsStr::to_str);
    if matches!(stem, Some("rustup" | "rustup-init")) {
        return true;
    }
    let rustup_exe = Path::new("rustup").with_extension(std::env::consts::EXE_EXTENSION);
    [located, resolved].iter().any(|path| {
        fs::metadata(path.with_file_name(&rustup_exe))
            .is_ok_and(|rustup| rustup.is_file() && rustup.len() == meta.len())
    })
}

/// Every compiler library in `lib` (`librustc_driver-*`, `libLLVM*`), by path, size and dates,
/// in name order. A folder that doesn't exist holds none.
fn driver_stats(lib: &Path) -> Result<Vec<Value>, Unknown> {
    let entries = match fs::read_dir(lib) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Unknown::Unreadable {
                path: lib.to_path_buf(),
                source,
            });
        }
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| Unknown::Unreadable {
            path: lib.to_path_buf(),
            source,
        })?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| DRIVER_PREFIXES.iter().any(|p| name.starts_with(p)))
        {
            found.push(entry.path());
        }
    }
    found.sort();
    found
        .iter()
        .map(|path| {
            let meta = fs::metadata(path).map_err(|source| Unknown::Unreadable {
                path: path.clone(),
                source,
            })?;
            stat_value(path, &meta)
        })
        .collect()
}

/// A file's path, size, modification time and, where the file system keeps it, creation time.
/// Cargo uses both dates: distributions that build reproducibly may clamp the modification time.
/// Numbers are written as text, since JCS numbers are doubles and nanoseconds don't fit.
fn stat_value(path: &Path, meta: &fs::Metadata) -> Result<Value, Unknown> {
    let text = path
        .to_str()
        .ok_or_else(|| Unknown::NotUtf8(path.to_string_lossy().into_owned()))?;
    let stat = Stat::of(meta);
    Ok(json!({
        "path": text,
        "size": stat.size.to_string(),
        "modified": stat.mtime_ns.to_string(),
        "created": meta.created().ok().map(nanos),
    }))
}

/// Nanoseconds since the epoch, as text, negative before it.
fn nanos(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_nanos().to_string(),
        Err(before) => format!("-{}", before.duration().as_nanos()),
    }
}

/// The `host:` and `release:` lines of `rustc -vV`.
fn parse_verbose(out: &str) -> Option<(String, String)> {
    let line = |prefix: &str| {
        out.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some((line("host: ")?, line("release: ")?))
}

/// What a memo holds: the key it was written under, so a file that doesn't match its name is
/// ignored, the outputs, the standard library's listings and the content digests. A memo
/// without one of them (an older one) is no memo.
fn read_memo(memo: &Path, key: &Digest) -> Option<Memo> {
    let bytes = fs::read(memo).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("key")?.as_str()? != key.to_string() {
        return None;
    }
    let out = value.get("verbose_version")?.as_str()?.to_string();
    let (host, release) = parse_verbose(&out)?;
    let targets = parse_targets(value.get("target_list")?.as_str()?);
    let std = value.get("std")?.as_array()?.clone();
    let drivers = value.get("drivers")?.as_array()?.clone();
    let content = value.get("content")?.clone();
    for part in ["rustc", "rustc_driver", "std"] {
        content.get(part)?;
    }
    Some(Memo {
        verbose_version: out,
        host,
        release,
        targets,
        std,
        drivers,
        content,
    })
}

/// The newest date, modification or change, of `paths`, in nanoseconds. A path that isn't there
/// is skipped (its absence is in the stamps already); one that can't be stat'ed is the newest
/// of all, so no memo is kept.
fn newest(paths: &[PathBuf]) -> i64 {
    paths
        .iter()
        .filter_map(|path| match fs::metadata(path) {
            Ok(meta) => {
                let stat = Stat::of(&meta);
                Some(stat.mtime_ns.max(stat.ctime_ns))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(_) => Some(i64::MAX),
        })
        .max()
        .unwrap_or(i64::MIN)
}

/// Writes the memo whole: a temporary file of this writer's own, then a rename. Two writers of
/// the same memo write the same bytes, so whichever rename comes last is right.
///
/// The memo is trusted later by stats alone, and Linux dates files from a clock that moves
/// once a tick (1 to 10 ms): a folder changed again in the tick it was listed keeps its stamp.
/// So the memo is published only when everything it is trusted by (`newest`, from
/// [`newest`]) is dated before the memo file itself, from the same clock; otherwise it is
/// dropped and computed again by a later call, as git treats a racily clean index entry.
fn write_memo(dir: &Path, memo: &Path, key: &Digest, found: &Memo, newest: i64) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(&json!({
        "key": key.to_string(),
        "verbose_version": found.verbose_version,
        "target_list": found.targets.join("\n"),
        "std": found.std,
        "drivers": found.drivers,
        "content": found.content,
    }))?;
    let name = memo.file_name().unwrap_or_default().to_string_lossy();
    for n in 0..64 {
        let temp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let written = file.write_all(&bytes).and_then(|()| {
            let dated = Stat::of(&file.metadata()?).mtime_ns;
            if newest >= dated {
                return Err(io::Error::other("the memo's inputs changed in its tick"));
            }
            fs::rename(&temp, memo)
        });
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        return written;
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free temporary name for the memo",
    ))
}

#[cfg(test)]
mod tests {
    use super::{Identity, Unknown};
    use hushrun_core::Digest;
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    const VERBOSE: &str = "rustc 1.94.0 (4a4ef493e 2026-03-02)\nbinary: rustc\n\
        commit-hash: 4a4ef493e\nhost: aarch64-apple-darwin\nrelease: 1.94.0\nLLVM version: 21.1.8\n";

    const TARGETS: &str = "aarch64-apple-darwin\naarch64-apple-ios\nx86_64-unknown-linux-gnu\n";

    /// [`super::identity`] with no `--target` and a fixed target list.
    fn identity(
        rustc: &Path,
        env: &dyn Fn(&str) -> Option<OsString>,
        memo_dir: &Path,
        run: &dyn Fn(&Path) -> io::Result<String>,
    ) -> Result<Identity, Unknown> {
        super::identity(rustc, None, env, memo_dir, run, &|_| Ok(TARGETS.into()))
    }

    /// A fake `-vV` that counts its calls.
    struct Fake {
        calls: Cell<usize>,
        out: String,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                calls: Cell::new(0),
                out: VERBOSE.to_string(),
            }
        }

        fn run(&self) -> impl Fn(&Path) -> io::Result<String> + '_ {
            |_| {
                self.calls.set(self.calls.get() + 1);
                Ok(self.out.clone())
            }
        }
    }

    fn env(pairs: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.as_os_str().to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn write(path: &Path, bytes: &[u8], modified_secs: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        set_modified(path, modified_secs);
    }

    fn set_modified(path: &Path, secs: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    /// Waits out a tick of the clock Linux dates files from (1 to 10 ms), so that what was
    /// written before is dated before what is written after.
    fn settle() {
        std::thread::sleep(Duration::from_millis(20));
    }

    /// A rustup home with one toolchain: its real rustc and its compiler library.
    struct Rustup {
        _dir: tempfile::TempDir,
        home: PathBuf,
        memo: PathBuf,
        rustc: PathBuf,
        driver: PathBuf,
        cargo_home: PathBuf,
    }

    fn rustup() -> Rustup {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("rustup");
        let toolchain = home.join("toolchains/nightly-aarch64-apple-darwin");
        let rustc = toolchain.join("bin/rustc");
        let driver = toolchain.join("lib/librustc_driver-0a1b2c.dylib");
        write(&rustc, b"rustc binary", 1_000);
        write(&driver, b"the whole compiler", 1_000);
        write(&toolchain.join("lib/libstd-9f8e.dylib"), b"std", 1_000);
        let cargo_home = root.join("cargo");
        write(
            &cargo_home.join("bin/rustup"),
            b"the rustup multicall binary",
            1_000,
        );
        fs::hard_link(cargo_home.join("bin/rustup"), cargo_home.join("bin/rustc")).unwrap();
        // A memo is kept only for files dated before it, and Linux dates them a tick late.
        settle();
        Rustup {
            _dir: dir,
            home,
            memo: root.join("memo"),
            rustc,
            driver,
            cargo_home,
        }
    }

    impl Rustup {
        fn env(&self) -> impl Fn(&str) -> Option<OsString> + use<> {
            env(&[
                ("RUSTUP_HOME", &self.home),
                (
                    "RUSTUP_TOOLCHAIN",
                    Path::new("nightly-aarch64-apple-darwin"),
                ),
                ("CARGO_HOME", &self.cargo_home),
            ])
        }

        fn identity(&self, rustc: &Path, fake: &Fake) -> Result<Identity, Unknown> {
            identity(rustc, &self.env(), &self.memo, &fake.run())
        }
    }

    // covers: B-17
    #[test]
    fn an_unchanged_compiler_runs_dash_vv_once_across_calls() {
        let setup = rustup();
        let fake = Fake::new();
        let first = setup.identity(&setup.rustc, &fake).unwrap();
        let second = setup.identity(&setup.rustc, &fake).unwrap();
        let third = setup.identity(&setup.rustc, &Fake::new()).unwrap();
        assert_eq!(fake.calls.get(), 1);
        assert_eq!(first, second);
        assert_eq!(first, third);
        assert_eq!(first.host, "aarch64-apple-darwin");
        assert_eq!(first.release, "1.94.0");
        assert_eq!(first.verbose_version, VERBOSE);
    }

    // covers: B-17
    #[test]
    fn a_toolchain_replaced_under_the_same_name_is_a_new_compiler() {
        let setup = rustup();
        let fake = Fake::new();
        let before = setup.identity(&setup.rustc, &fake).unwrap();

        // Same path, same size, a new date: looked at again, and the same bytes are the same
        // compiler.
        set_modified(&setup.rustc, 2_000);
        let touched = setup.identity(&setup.rustc, &fake).unwrap();
        assert_eq!(before.digest, touched.digest);
        assert_eq!(fake.calls.get(), 2);

        // Same path, same size, other bytes: `rustup update nightly`.
        write(&setup.rustc, b"rustc binary-2", 2_000);
        let newer = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(before.digest, newer.digest);
        assert_ne!(before.parts.rustc, newer.parts.rustc);
        assert_eq!(before.parts.rustc_driver, newer.parts.rustc_driver);
        assert_eq!(fake.calls.get(), 3);

        // Same path, same date, a new size.
        write(&setup.rustc, b"a longer rustc binary", 2_000);
        let bigger = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(newer.digest, bigger.digest);
        assert_eq!(fake.calls.get(), 4);
    }

    // covers: B-17
    #[test]
    fn a_changed_compiler_library_is_a_new_compiler() {
        let setup = rustup();
        let fake = Fake::new();
        let before = setup.identity(&setup.rustc, &fake).unwrap();
        write(&setup.driver, b"the whole compiler, patched", 1_000);
        let after = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(before.digest, after.digest);
        assert_eq!(fake.calls.get(), 2);
    }

    // covers: B-17
    #[test]
    fn a_plain_compiler_outside_rustup_counts_its_library_too() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let rustc = root.join("opt/rust/bin/rustc");
        let driver = root.join("opt/rust/lib/librustc_driver-77.so");
        write(&rustc, b"rustc", 1_000);
        write(&driver, b"compiler", 1_000);
        let no_env = env(&[]);
        let fake = Fake::new();
        let before = identity(&rustc, &no_env, &root.join("memo"), &fake.run()).unwrap();
        write(&driver, b"compiler, rebuilt", 1_000);
        let after = identity(&rustc, &no_env, &root.join("memo"), &fake.run()).unwrap();
        assert_ne!(before.digest, after.digest);
    }

    // covers: B-17
    #[test]
    fn a_proxy_names_its_toolchain_and_follows_it() {
        let setup = rustup();
        let proxy = setup.cargo_home.join("bin/rustc");
        let fake = Fake::new();
        let before = setup.identity(&proxy, &fake).unwrap();
        // The proxy is rustup itself: its bytes are not the compiler's.
        assert_eq!(before, setup.identity(&setup.rustc, &fake).unwrap());
        // The proxy and the variable stay; the toolchain under them changes.
        write(&setup.rustc, b"rustc binary, updated", 5_000);
        let after = setup.identity(&proxy, &fake).unwrap();
        assert_ne!(before.digest, after.digest);
    }

    // covers: B-17
    #[test]
    fn a_proxy_without_rustup_toolchain_is_unknown() {
        let setup = rustup();
        let proxy = setup.cargo_home.join("bin/rustc");
        let fake = Fake::new();
        let by_cargo_home = env(&[
            ("CARGO_HOME", &setup.cargo_home),
            ("RUSTUP_HOME", &setup.home),
        ]);
        let err = identity(&proxy, &by_cargo_home, &setup.memo, &fake.run()).unwrap_err();
        assert!(matches!(err, Unknown::ProxyWithoutToolchain(_)), "{err}");

        // Elsewhere than the cargo home, known by the `rustup` beside it.
        let elsewhere = setup.home.parent().unwrap().join("tools/bin");
        write(&elsewhere.join("rustup"), b"rustup multicall", 1_000);
        fs::hard_link(elsewhere.join("rustup"), elsewhere.join("rustc")).unwrap();
        let err = identity(
            &elsewhere.join("rustc"),
            &env(&[]),
            &setup.memo,
            &fake.run(),
        )
        .unwrap_err();
        assert!(matches!(err, Unknown::ProxyWithoutToolchain(_)), "{err}");

        // Found on PATH by its bare name.
        let on_path = env(&[("PATH", &elsewhere)]);
        let err = identity(Path::new("rustc"), &on_path, &setup.memo, &fake.run()).unwrap_err();
        assert!(matches!(err, Unknown::ProxyWithoutToolchain(_)), "{err}");
        assert_eq!(fake.calls.get(), 0);
    }

    // covers: B-17
    #[cfg(unix)]
    #[test]
    fn a_proxy_linked_to_rustup_init_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let init = root.join("cellar/rustup/bin/rustup-init");
        write(&init, b"rustup", 1_000);
        let rustc = root.join("bin/rustc");
        fs::create_dir_all(rustc.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&init, &rustc).unwrap();
        let fake = Fake::new();
        let err = identity(&rustc, &env(&[]), &root.join("memo"), &fake.run()).unwrap_err();
        assert!(matches!(err, Unknown::ProxyWithoutToolchain(_)), "{err}");
    }

    // covers: B-17
    #[test]
    fn a_missing_compiler_is_unknown() {
        let setup = rustup();
        let fake = Fake::new();
        let gone = setup.home.join("toolchains/gone/bin/rustc");
        let err = setup.identity(&gone, &fake).unwrap_err();
        assert!(matches!(err, Unknown::Unreadable { .. }), "{err}");

        // The toolchain the variable names is gone, as cargo also refuses.
        let missing_toolchain = env(&[
            ("RUSTUP_HOME", &setup.home),
            ("RUSTUP_TOOLCHAIN", Path::new("stable-aarch64-apple-darwin")),
        ]);
        let err = identity(&setup.rustc, &missing_toolchain, &setup.memo, &fake.run()).unwrap_err();
        assert!(matches!(err, Unknown::Unreadable { .. }), "{err}");

        let err = identity(Path::new("rustc"), &env(&[]), &setup.memo, &fake.run()).unwrap_err();
        assert!(matches!(err, Unknown::NotOnPath(_)), "{err}");
        assert_eq!(fake.calls.get(), 0);
    }

    // covers: B-17
    #[test]
    fn a_corrupt_memo_is_ignored() {
        let setup = rustup();
        let fake = Fake::new();
        let first = setup.identity(&setup.rustc, &fake).unwrap();
        let memos: Vec<_> = fs::read_dir(&setup.memo)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(memos.len(), 1, "one memo, no temporary file left");

        for corrupt in [
            &b"{\"key\": \"sha256:"[..],
            b"",
            b"{\"key\":\"x\",\"verbose_version\":\"y\"}",
        ] {
            fs::write(&memos[0], corrupt).unwrap();
            let again = setup.identity(&setup.rustc, &fake).unwrap();
            assert_eq!(first, again);
        }
        assert_eq!(fake.calls.get(), 4);
        // And it was written whole again.
        setup.identity(&setup.rustc, &fake).unwrap();
        assert_eq!(fake.calls.get(), 4);
    }

    // covers: B-17
    #[test]
    fn a_memo_that_cannot_be_written_is_no_error() {
        let setup = rustup();
        // A file where the memo folder should be.
        fs::write(&setup.memo, b"not a folder").unwrap();
        let fake = Fake::new();
        setup.identity(&setup.rustc, &fake).unwrap();
        setup.identity(&setup.rustc, &fake).unwrap();
        assert_eq!(fake.calls.get(), 2);
    }

    // covers: B-17
    #[test]
    fn the_output_of_dash_vv_is_part_of_the_identity() {
        let setup = rustup();
        let a = setup.identity(&setup.rustc, &Fake::new()).unwrap();
        let other = Fake {
            calls: Default::default(),
            out: VERBOSE.replace("4a4ef493e", "0000000"),
        };
        let b = identity(
            &setup.rustc,
            &setup.env(),
            &setup.home.join("memo-b"),
            &other.run(),
        )
        .unwrap();
        assert_ne!(a.digest, b.digest);

        let not_rustc = Fake {
            calls: Default::default(),
            out: "clippy-driver 0.1\n".into(),
        };
        let err = identity(
            &setup.rustc,
            &setup.env(),
            &setup.home.join("memo-c"),
            &not_rustc.run(),
        )
        .unwrap_err();
        assert!(matches!(err, Unknown::NotRustc(_)), "{err}");
    }

    // covers: B-17
    #[test]
    fn a_changed_standard_library_is_a_new_compiler() {
        let setup = rustup();
        let fake = Fake::new();
        let std_dir = setup
            .home
            .join("toolchains/nightly-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib");
        write(&std_dir.join("libstd-1.rlib"), b"std", 1_000);
        let before = setup.identity(&setup.rustc, &fake).unwrap();
        // A std replaced the way an installer does it: a new file renamed over the old one.
        write(&std_dir.join(".new"), b"std, patched", 2_000);
        fs::rename(std_dir.join(".new"), std_dir.join("libstd-1.rlib")).unwrap();
        let after = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(before.digest, after.digest);
    }

    // covers: B-17
    #[test]
    fn a_standard_library_changed_in_the_tick_its_memo_was_written_is_a_new_compiler() {
        // Linux dates files and folders from a clock that moves once a tick (1 to 10 ms), so a
        // folder changed twice in one tick keeps its stamp. A memo written between the two
        // changes must not hold. Each round adds a file and replaces it at once, which on Linux
        // often falls in one tick; on macOS the stamps always differ.
        let setup = rustup();
        let fake = Fake::new();
        let std_dir = setup
            .home
            .join("toolchains/nightly-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib");
        for round in 0..200 {
            let name = format!("libstd-{round}.rlib");
            fs::create_dir_all(&std_dir).unwrap();
            fs::write(std_dir.join(&name), b"std").unwrap();
            let before = setup.identity(&setup.rustc, &fake).unwrap();
            fs::write(std_dir.join(".new"), b"std, patched").unwrap();
            fs::rename(std_dir.join(".new"), std_dir.join(&name)).unwrap();
            let after = setup.identity(&setup.rustc, &fake).unwrap();
            assert_ne!(before.digest, after.digest, "round {round}");
        }
    }

    // covers: B-4, B-17
    #[test]
    fn the_built_in_targets_are_listed_once_per_compiler_and_target() {
        let setup = rustup();
        let fake = Fake::new();
        let listed = Cell::new(0);
        let targets = |_: &Path| {
            listed.set(listed.get() + 1);
            Ok(TARGETS.to_string())
        };
        let call = |target: Option<&str>| {
            super::identity(
                &setup.rustc,
                target,
                &setup.env(),
                &setup.memo,
                &fake.run(),
                &targets,
            )
            .unwrap()
        };
        let host = call(None);
        assert!(host.builds_for("aarch64-apple-ios"));
        assert!(!host.builds_for("mytgt"));
        assert!(!host.builds_for("aarch64-apple"));
        call(None);
        assert_eq!((fake.calls.get(), listed.get()), (1, 1));
        // Another target is another identity, with its own memo.
        let ios = call(Some("aarch64-apple-ios"));
        call(Some("aarch64-apple-ios"));
        assert_ne!(host.digest, ios.digest);
        assert_eq!((fake.calls.get(), listed.get()), (2, 2));
        // The target's standard library counts too, once it is there.
        let ios_std = setup
            .home
            .join("toolchains/nightly-aarch64-apple-darwin/lib/rustlib/aarch64-apple-ios/lib");
        write(&ios_std.join("libcore-1.rlib"), b"core", 1_000);
        let installed = call(Some("aarch64-apple-ios"));
        assert_ne!(ios.digest, installed.digest);
        assert_eq!(call(None).digest, host.digest);

        // A list that can't be had: the identity can't be established.
        let failing = |_: &Path| -> io::Result<String> { Err(io::Error::other("no")) };
        let err = super::identity(
            &setup.rustc,
            None,
            &setup.env(),
            &setup.home.join("memo-x"),
            &fake.run(),
            &failing,
        )
        .unwrap_err();
        assert!(matches!(err, Unknown::VersionFailed { .. }), "{err}");
    }

    // covers: B-17
    #[test]
    fn a_memo_written_before_the_target_list_is_computed_again() {
        let setup = rustup();
        let fake = Fake::new();
        let first = setup.identity(&setup.rustc, &fake).unwrap();
        let memo = fs::read_dir(&setup.memo)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&memo).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("target_list");
        fs::write(&memo, serde_json::to_vec(&value).unwrap()).unwrap();
        let again = setup.identity(&setup.rustc, &fake).unwrap();
        assert_eq!(first, again);
        assert_eq!(fake.calls.get(), 2);
    }

    // covers: B-17
    #[test]
    fn the_toolchains_lib_folder_is_listed_once_and_again_when_it_changes() {
        let setup = rustup();
        let fake = Fake::new();
        let before = setup.identity(&setup.rustc, &fake).unwrap();
        // The memo keeps the compiler libraries and their digests: while the folder's own
        // stamp and the libraries' stats hold, nothing is listed or read again. A memo whose
        // digests were changed (which nothing but this test writes) proves it: they are what
        // the identity holds now.
        let memo = fs::read_dir(&setup.memo)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = fs::read_to_string(&memo).unwrap();
        assert!(text.contains("librustc_driver-0a1b2c.dylib"), "{text}");
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["content"]["rustc_driver"] = serde_json::json!([[
            "librustc_driver-0a1b2c.dylib",
            Digest::of(b"not the file").to_string()
        ]]);
        fs::write(&memo, serde_json::to_vec(&value).unwrap()).unwrap();
        let kept = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(kept.digest, before.digest, "the memo answered");
        assert_eq!(fake.calls.get(), 1);
        // A driver added changes the folder's stamp: listed and read again, a new compiler.
        let lib = setup.driver.parent().unwrap();
        write(
            &lib.join("librustc_driver-ffff.dylib"),
            b"a second compiler",
            1_000,
        );
        let added = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(added.digest, before.digest);
        assert_ne!(added.digest, kept.digest);
        assert_eq!(fake.calls.get(), 2);
        // A file that is no compiler library is listed again too, and changes nothing.
        settle();
        write(&lib.join("other"), b"another file", 1_000);
        settle();
        assert_eq!(
            setup.identity(&setup.rustc, &fake).unwrap().digest,
            added.digest
        );
    }

    // covers: B-17
    #[test]
    fn the_rustup_variables_are_not_part_of_the_identity() {
        let setup = rustup();
        let fake = Fake::new();
        let named = setup.identity(&setup.rustc, &fake).unwrap();
        let plain = identity(&setup.rustc, &env(&[]), &setup.memo, &fake.run()).unwrap();
        assert_eq!(named.digest, plain.digest);
    }

    /// A toolchain like [`rustup`]'s, with a standard library, at `root` under the toolchain
    /// name `name`, its files dated `secs`.
    fn toolchain_at(root: &Path, name: &str, secs: u64) -> (PathBuf, PathBuf) {
        let home = root.join("rustup");
        let toolchain = home.join("toolchains").join(name);
        write(&toolchain.join("bin/rustc"), b"rustc binary", secs);
        write(
            &toolchain.join("lib/librustc_driver-0a1b2c.dylib"),
            b"the whole compiler",
            secs,
        );
        write(
            &toolchain.join("lib/libLLVM.so.21.1-rust-1.94.0"),
            b"llvm",
            secs,
        );
        let std = toolchain.join("lib/rustlib/aarch64-apple-darwin/lib");
        write(&std.join("libstd-9f8e.rlib"), b"std", secs);
        write(&std.join("libcore-1a2b.rmeta"), b"core", secs);
        write(&std.join("self-contained/crt1.o"), b"crt", secs);
        (home, toolchain)
    }

    // covers: B-17
    #[test]
    fn the_same_toolchain_in_two_folders_is_one_compiler() {
        // Two machines: other users, other homes, the toolchain installed under another name
        // (`stable-…` and `1.94.0-…`) on other days. Same bytes: one compiler.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (home_a, toolchain_a) = toolchain_at(&root.join("home/alice"), "stable-x", 1_000);
        let (home_b, toolchain_b) =
            toolchain_at(&root.join("Users/someone-else"), "1.94.0-x", 9_000);
        settle();
        let fake = Fake::new();
        let at = |home: &Path, name: &str, toolchain: &Path| {
            let vars = env(&[("RUSTUP_HOME", home), ("RUSTUP_TOOLCHAIN", Path::new(name))]);
            identity(
                &toolchain.join("bin/rustc"),
                &vars,
                &home.join("memo"),
                &fake.run(),
            )
            .unwrap()
        };
        let a = at(&home_a, "stable-x", &toolchain_a);
        let b = at(&home_b, "1.94.0-x", &toolchain_b);
        assert_eq!(a.digest, b.digest);
        assert_eq!(a.parts, b.parts);
        assert_ne!(a.sysroot, b.sysroot);
        // Each part is the content: a standard library file in a sub-folder, LLVM.
        write(
            &toolchain_b.join("lib/rustlib/aarch64-apple-darwin/lib/self-contained/crt1.o"),
            b"crt, other",
            9_000,
        );
        write(
            &toolchain_b.join("lib/rustlib/aarch64-apple-darwin/lib/.touch"),
            b"",
            9_000,
        );
        fs::remove_file(toolchain_b.join("lib/rustlib/aarch64-apple-darwin/lib/.touch")).unwrap();
        settle();
        let b2 = at(&home_b, "1.94.0-x", &toolchain_b);
        assert_ne!(a.digest, b2.digest);
        assert_ne!(a.parts.std, b2.parts.std);
        write(
            &toolchain_a.join("lib/libLLVM.so.21.1-rust-1.94.0"),
            b"llvm-2",
            1_000,
        );
        let a2 = at(&home_a, "stable-x", &toolchain_a);
        assert_ne!(a.parts.rustc_driver, a2.parts.rustc_driver);
    }

    // covers: B-17
    #[test]
    fn the_toolchain_is_hashed_once_per_install() {
        let setup = rustup();
        let std_dir = setup
            .home
            .join("toolchains/nightly-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib");
        write(&std_dir.join("libstd-1.rlib"), b"std one", 1_000);
        settle();
        let fake = Fake::new();
        let first = setup.identity(&setup.rustc, &fake).unwrap();
        // The file's bytes change in place, same size and date, its folder untouched: nothing
        // a memo is trusted by moved, so the files are not read again. The memo answers.
        fs::write(std_dir.join("libstd-1.rlib"), b"std two").unwrap();
        set_modified(&std_dir.join("libstd-1.rlib"), 1_000);
        for _ in 0..3 {
            assert_eq!(setup.identity(&setup.rustc, &fake).unwrap(), first);
        }
        assert_eq!(fake.calls.get(), 1);
        // A file installed in the folder moves its stamp: read again, a new compiler.
        write(&std_dir.join("libstd-2.rlib"), b"std 2", 1_000);
        let installed = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(installed.digest, first.digest);
        assert_eq!(fake.calls.get(), 2);
    }

    // covers: B-17
    #[test]
    fn calls_that_start_at_once_hash_the_toolchain_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let setup = rustup();
        let calls = AtomicUsize::new(0);
        let digests: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let run = |_: &Path| {
                            calls.fetch_add(1, Ordering::SeqCst);
                            // Slow enough that the others arrive while it computes.
                            std::thread::sleep(Duration::from_millis(50));
                            Ok(VERBOSE.to_string())
                        };
                        identity(&setup.rustc, &setup.env(), &setup.memo, &run)
                            .unwrap()
                            .digest
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(digests.windows(2).all(|w| w[0] == w[1]));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // The lock is gone once the memo is written: one memo file, nothing else.
        assert_eq!(fs::read_dir(&setup.memo).unwrap().count(), 1);
    }
}
