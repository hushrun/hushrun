//! The compiler's identity (B-17), found the way cargo finds it, so that a toolchain replaced
//! under the same name is a new compiler. A rustup proxy with no toolchain named is a reason to
//! pass through.
//!
//! Cargo keeps the same thing for its own cache of `rustc -vV` (`util/rustc.rs`,
//! `rustc_fingerprint`): the compiler's path, size and dates; `RUSTUP_HOME` and
//! `RUSTUP_TOOLCHAIN`; and, under rustup, the date of the toolchain's own `rustc`, because
//! `rustup update nightly` changes neither the proxy nor the variable. Here the toolchain's
//! `librustc_driver`, where nearly all of the compiler lives, is stated too.
//!
//! `rustc -vV` runs once per compiler, not once per call: its output is kept in a memo named by
//! the hash of everything else in the identity.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;
use hushrun_core::jcs;
use hushrun_digest::cache::Stat;
use serde_json::{Value, json};

/// Names the form of the identity, so a change to what goes in gives new identities.
const SCHEMA: &str = "hushrun.build.compiler/1";

/// The prefix of the compiler library's file name in a toolchain's `lib/`.
const DRIVER_PREFIX: &str = "librustc_driver-";

/// Which compiler a call is in front of (B-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The SHA-256 of the JCS form of everything B-17 lists, the output of `rustc -vV` included.
    pub digest: Digest,
    /// What `rustc -vV` printed.
    pub verbose_version: String,
    /// The `host:` line of that output: the target the compiler runs on.
    pub host: String,
    /// The `release:` line of that output, such as `1.94.0`.
    pub release: String,
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
    /// `rustc -vV` could not be run.
    #[error("running {} -vV: {source}", .path.display())]
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
/// `env` looks up a variable of the call (`PATH`, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`,
/// `CARGO_HOME`, `HOME`). `run` runs `<rustc> -vV` and returns what it printed; it is called only
/// when `memo_dir` has no memo for this compiler. A memo that can't be read or written is no
/// error: `-vV` runs instead.
pub fn identity(
    rustc: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
    memo_dir: &Path,
    run: &dyn Fn(&Path) -> io::Result<String>,
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
    let mut drivers = Vec::new();
    for lib in &libraries {
        drivers.extend(driver_stats(lib)?);
    }

    let mut facts = json!({
        "schema": SCHEMA,
        "rustc": stat_value(&resolved, &meta)?,
        "rustup_home": rustup_home,
        "rustup_toolchain": rustup_toolchain,
        "toolchain_rustc": toolchain_rustc,
        "rustc_driver": drivers,
    });
    let memo_key = jcs::digest(&facts)?;
    let memo = memo_dir.join(format!("{}.json", memo_key.hex()));

    let (verbose_version, host, release) = match read_memo(&memo, &memo_key) {
        Some(found) => found,
        None => {
            let out = run(rustc).map_err(|source| Unknown::VersionFailed {
                path: rustc.to_path_buf(),
                source,
            })?;
            let (host, release) =
                parse_verbose(&out).ok_or_else(|| Unknown::NotRustc(rustc.to_path_buf()))?;
            // A memo not written only means `-vV` runs again next time.
            let _ = write_memo(memo_dir, &memo, &memo_key, &out);
            (out, host, release)
        }
    };

    if let Value::Object(members) = &mut facts {
        members.insert(
            "verbose_version".into(),
            Value::String(verbose_version.clone()),
        );
    }
    Ok(Identity {
        digest: jcs::digest(&facts)?,
        verbose_version,
        host,
        release,
    })
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

/// Every `librustc_driver-*` in `lib`, by path, size and dates, in name order. A folder that
/// doesn't exist holds none.
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
            .is_some_and(|name| name.starts_with(DRIVER_PREFIX))
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
/// ignored, and the output.
fn read_memo(memo: &Path, key: &Digest) -> Option<(String, String, String)> {
    let bytes = fs::read(memo).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("key")?.as_str()? != key.to_string() {
        return None;
    }
    let out = value.get("verbose_version")?.as_str()?.to_string();
    let (host, release) = parse_verbose(&out)?;
    Some((out, host, release))
}

/// Writes the memo whole: a temporary file of this writer's own, then a rename. Two writers of
/// the same memo write the same bytes, so whichever rename comes last is right.
fn write_memo(dir: &Path, memo: &Path, key: &Digest, out: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(&json!({ "key": key.to_string(), "verbose_version": out }))?;
    let name = memo.file_name().unwrap_or_default().to_string_lossy();
    for n in 0..64 {
        let temp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let written = file
            .write_all(&bytes)
            .and_then(|()| fs::rename(&temp, memo));
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
    use super::{Identity, Unknown, identity};
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    const VERBOSE: &str = "rustc 1.94.0 (4a4ef493e 2026-03-02)\nbinary: rustc\n\
        commit-hash: 4a4ef493e\nhost: aarch64-apple-darwin\nrelease: 1.94.0\nLLVM version: 21.1.8\n";

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

        // Same path, same size, a new date: `rustup update nightly`.
        set_modified(&setup.rustc, 2_000);
        let newer = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(before.digest, newer.digest);
        assert_eq!(fake.calls.get(), 2);

        // Same path, same date, a new size.
        write(&setup.rustc, b"a longer rustc binary", 2_000);
        let bigger = setup.identity(&setup.rustc, &fake).unwrap();
        assert_ne!(newer.digest, bigger.digest);
        assert_eq!(fake.calls.get(), 3);
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
        // The proxy and the variable stay; the toolchain under them changes.
        set_modified(&setup.rustc, 5_000);
        let after = setup.identity(&proxy, &fake).unwrap();
        assert_ne!(before.digest, after.digest);
        assert_eq!(fake.calls.get(), 2);
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
    fn the_rustup_variables_are_part_of_the_identity() {
        let setup = rustup();
        let fake = Fake::new();
        let named = setup.identity(&setup.rustc, &fake).unwrap();
        let plain = identity(&setup.rustc, &env(&[]), &setup.memo, &fake.run()).unwrap();
        assert_ne!(named.digest, plain.digest);
    }
}
