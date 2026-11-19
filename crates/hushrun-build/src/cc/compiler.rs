//! The C or C++ compiler's identity (B-52): its content, not its name. The binary the call
//! reaches (on macOS through `xcrun`'s trampolines), GCC's `cc1`, `cc1plus` and `as`, what the
//! driver says it does with no flags of the call's (target, resource folder, sysroot, implicit
//! flags), and the SDK's `SDKSettings.json`. Headers of the SDK and the toolchain are not in it:
//! they are read files, keyed with each compile (B-54).
//!
//! The identity is memoised in the store by the compiler's path and stat, the variables that pick
//! a toolchain, the flags that change the driver, and on macOS the folder `xcode-select` chose.
//! An entry answers only while every file it names has the stat it holds.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;
use serde::{Deserialize, Serialize};

use super::args::Lang;
use super::places::{self, Places};
use crate::regular;

/// The schema the identity is taken under.
const SCHEMA: &str = "hushrun.cc-compiler/v1";
/// The schema of a memo entry.
const MEMO_SCHEMA: &str = "hushrun.cc-compiler-memo/1";
/// Where a memo entry's name starts, in the store's compiler memos ([`crate::compiler::MEMOS`]).
const MEMO_PREFIX: &str = "cc-";

/// The variables that choose a toolchain on macOS, and `PATH`, which finds a program by name.
const TOOLCHAIN_VARS: &[&str] = &["DEVELOPER_DIR", "SDKROOT", "TOOLCHAINS", "PATH"];

/// Apple's trampolines in `/usr/bin`, which start the toolchain `xcrun` picks.
const TRAMPOLINES: &[&str] = &["cc", "c++", "clang", "clang++", "gcc", "g++"];

/// One system folder of the compiler's search, as its driver lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemDir {
    /// The folder, resolved.
    pub path: String,
    /// A framework folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub framework: bool,
}

/// What the launcher knows of one compiler for one language and set of driver flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The identity (B-52).
    pub digest: Digest,
    /// The driver is clang's: it writes the include graph when asked (B-55).
    pub clang: bool,
    /// The target triple the driver names: a block's platform (B-60).
    pub target: String,
    /// The SDK the driver uses, resolved, when it names one.
    pub sdk: Option<String>,
    /// The folder above the compiler's `bin`, resolved, when it is not a system folder.
    pub toolchain: Option<String>,
    /// The folders the driver searches for system headers, in order.
    pub system: Vec<SystemDir>,
}

impl Identity {
    /// The bases of this compiler's calls: the SDK and the toolchain, every form.
    pub fn places(&self, roots: &[String]) -> Places {
        let sdk: Vec<String> = self.sdk.iter().cloned().collect();
        let toolchain: Vec<String> = self.toolchain.iter().cloned().collect();
        Places::new(roots, &sdk, &toolchain)
    }

    /// Whether an absolute path lies under one of the driver's system folders, or a usual one.
    pub fn in_system(&self, path: &str) -> bool {
        self.system.iter().any(|d| places::under(path, &d.path))
            || [
                "/usr/include",
                "/usr/local/include",
                "/opt/homebrew/include",
            ]
            .iter()
            .any(|d| places::under(path, d))
    }
}

/// What a program printed.
#[derive(Debug, Clone, Default)]
pub struct Said {
    /// Whether it exited with 0.
    pub success: bool,
    /// Its stdout.
    pub stdout: String,
    /// Its stderr.
    pub stderr: String,
}

/// Runs a program for one of the identity's questions: with the call's variables, `LC_ALL=C`,
/// in `/`, nothing on stdin. `None` when it can't start.
pub type Ask<'a> = dyn Fn(&Path, &[String]) -> Option<Said> + 'a;

/// Why the identity can't be established: the call passes through (B-51).
#[derive(Debug, thiserror::Error)]
pub enum NoIdentity {
    /// The compiler isn't found.
    #[error("the compiler isn't found")]
    NotFound,
    /// A question to the driver failed.
    #[error("the driver didn't answer {0}")]
    Driver(&'static str),
    /// A file of the identity can't be read.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Serialize, Deserialize)]
struct Memo {
    schema: String,
    key: Digest,
    identity: Identity,
    files: Vec<MemoFile>,
}

#[derive(Serialize, Deserialize)]
struct MemoFile {
    path: String,
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    inode: u64,
    device: u64,
}

impl MemoFile {
    fn of(path: &Path) -> std::io::Result<MemoFile> {
        let stat = Stat::of(&fs::metadata(path)?);
        Ok(MemoFile {
            path: path.to_string_lossy().into_owned(),
            size: stat.size,
            mtime_ns: stat.mtime_ns,
            ctime_ns: stat.ctime_ns,
            inode: stat.inode,
            device: stat.device,
        })
    }

    fn holds(&self) -> bool {
        MemoFile::of(Path::new(&self.path)).is_ok_and(|now| {
            now.size == self.size
                && now.mtime_ns == self.mtime_ns
                && now.ctime_ns == self.ctime_ns
                && now.inode == self.inode
                && now.device == self.device
        })
    }
}

/// The program a name reaches on `PATH`, or the path itself when it names a folder.
pub fn which(program: &OsString, path: Option<&OsString>) -> Option<PathBuf> {
    let given = PathBuf::from(program);
    if given.components().count() > 1 {
        return given.is_file().then_some(given);
    }
    let path = path?;
    std::env::split_paths(path)
        .map(|dir| dir.join(&given))
        .find(|candidate| candidate.is_file())
}

/// The identity of `compiler` for `lang` and `driver_flags`, from the memo in `memos` when it
/// holds, else asked of the driver and memoised. `var` reads the call's variables.
pub fn identity(
    compiler: &OsString,
    lang: Lang,
    driver_flags: &[String],
    var: &dyn Fn(&str) -> Option<OsString>,
    memos: &crate::store::Store,
    ask: &Ask<'_>,
) -> Result<Identity, NoIdentity> {
    let exe = which(compiler, var("PATH").as_ref()).ok_or(NoIdentity::NotFound)?;
    let exe_stat = MemoFile::of(&exe)?;
    let mut key_parts = serde_json::json!({
        "schema": MEMO_SCHEMA,
        "compiler": compiler.to_string_lossy(),
        "exe": exe.to_string_lossy(),
        "stat": [exe_stat.size, exe_stat.mtime_ns, exe_stat.ctime_ns, exe_stat.inode, exe_stat.device],
        "lang": lang.word(),
        "flags": driver_flags,
    });
    for name in TOOLCHAIN_VARS {
        key_parts[*name] = var(name).map(|v| v.to_string_lossy().into_owned()).into();
    }
    if cfg!(target_os = "macos") {
        key_parts["xcode_select"] = fs::read_link("/var/db/xcode_select_link")
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .into();
    }
    let key = Digest::of(
        hushrun_core::jcs::canonicalize(&key_parts)
            .unwrap_or_default()
            .as_bytes(),
    );
    let memo_path = memos
        .root()
        .join(crate::compiler::MEMOS)
        .join(format!("{MEMO_PREFIX}{}.json", key.hex()));
    if let Some(identity) = read_memo(&memo_path, &key) {
        return Ok(identity);
    }
    // A cold build starts a compile per core at once: one asks the driver, the others wait and
    // read what it wrote. Without a lock (it can't be taken), each asks.
    let _lock = lock(&memo_path);
    if _lock.is_some()
        && let Some(identity) = read_memo(&memo_path, &key)
    {
        return Ok(identity);
    }
    let (identity, files) = ask_identity(&exe, lang, driver_flags, var, ask)?;
    let memo = Memo {
        schema: MEMO_SCHEMA.to_string(),
        key,
        identity: identity.clone(),
        files,
    };
    if let Ok(bytes) = serde_json::to_vec(&memo) {
        // A memo is a saving: one that can't be written costs the questions next time.
        let _ = memos.write_whole(&memo_path, &bytes, None, false);
    }
    Ok(identity)
}

/// A lock beside a memo entry while one process asks the driver, released when dropped.
struct Lock(fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock(memo: &Path) -> Option<Lock> {
    let dir = memo.parent()?;
    fs::create_dir_all(dir).ok()?;
    let name = memo.file_name()?.to_string_lossy();
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!(".{name}.lock")))
        .ok()?;
    file.lock().ok()?;
    Some(Lock(file))
}

fn read_memo(path: &Path, key: &Digest) -> Option<Identity> {
    let bytes = fs::read(path).ok()?;
    let memo: Memo = serde_json::from_slice(&bytes).ok()?;
    (memo.schema == MEMO_SCHEMA && memo.key == *key && memo.files.iter().all(MemoFile::holds))
        .then_some(memo.identity)
}

/// The value after `flag` in the driver's `-###` text, whose words are quoted.
fn quoted_after(text: &str, flag: &str) -> Option<String> {
    let at = text.find(&format!("\"{flag}\" \""))?;
    let rest = &text[at + flag.len() + 4..];
    Some(rest[..rest.find('"')?].to_string())
}

/// The system folders the driver's `-v` lists between its two lines.
fn search_list(text: &str) -> Vec<SystemDir> {
    let mut dirs = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("#include <...> search starts here:") {
            inside = true;
            continue;
        }
        if line.starts_with("End of search list.") {
            break;
        }
        if inside {
            let line = line.trim();
            let (path, framework) = match line.strip_suffix(" (framework directory)") {
                Some(p) => (p, true),
                None => (line, false),
            };
            let path = fs::canonicalize(path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string());
            dirs.push(SystemDir { path, framework });
        }
    }
    dirs
}

fn ask_identity(
    exe: &Path,
    lang: Lang,
    driver_flags: &[String],
    var: &dyn Fn(&str) -> Option<OsString>,
    ask: &Ask<'_>,
) -> Result<(Identity, Vec<MemoFile>), NoIdentity> {
    let mut files = Vec::new();
    let mut real = fs::canonicalize(exe)?;
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if cfg!(target_os = "macos")
        && (exe.starts_with("/usr/bin") || real.starts_with("/usr/bin"))
        && TRAMPOLINES.contains(&name.as_str())
    {
        let tool = if name.contains("++") {
            "clang++"
        } else {
            "clang"
        };
        let said = ask(Path::new("/usr/bin/xcrun"), &["-f".into(), tool.into()])
            .filter(|s| s.success)
            .ok_or(NoIdentity::Driver("xcrun -f"))?;
        real = fs::canonicalize(said.stdout.trim())?;
    }
    files.push(MemoFile::of(&real)?);
    let (binary, _) = regular::hash_file(&real)?;
    let mut probe: Vec<String> = vec![
        "-###".into(),
        "-c".into(),
        "-x".into(),
        lang.word().into(),
        "/dev/null".into(),
        "-o".into(),
        "/dev/null".into(),
    ];
    probe.extend(driver_flags.iter().cloned());
    let driver = ask(exe, &probe)
        .filter(|s| s.success)
        .ok_or(NoIdentity::Driver("-###"))?;
    let clang = driver.stderr.contains("\"-cc1\"");
    let target = driver
        .stderr
        .lines()
        .find_map(|line| line.strip_prefix("Target: "))
        .unwrap_or_default()
        .trim()
        .to_string();
    let sdk = quoted_after(&driver.stderr, "-isysroot")
        .or_else(|| var("SDKROOT").map(|v| v.to_string_lossy().into_owned()))
        .filter(|s| !s.is_empty())
        .map(|s| {
            fs::canonicalize(&s)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(s)
        });
    let toolchain = real
        .parent()
        .and_then(Path::parent)
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !matches!(p.as_str(), "/" | "/usr" | "/usr/local" | "/opt/homebrew"));
    let mut search = vec![
        "-x".to_string(),
        lang.word().into(),
        "-E".into(),
        "-v".into(),
        "/dev/null".into(),
        "-o".into(),
        "/dev/null".into(),
    ];
    search.extend(driver_flags.iter().cloned());
    let listed = ask(exe, &search)
        .filter(|s| s.success)
        .ok_or(NoIdentity::Driver("-E -v"))?;
    let system = search_list(&listed.stderr);
    let mut programs = serde_json::Map::new();
    if !clang {
        for prog in ["cc1", "cc1plus", "as"] {
            let Some(said) = ask(exe, &[format!("-print-prog-name={prog}")]) else {
                continue;
            };
            let named = said.stdout.trim().to_string();
            if named.is_empty() {
                continue;
            }
            if let Some(path) = which(&OsString::from(&named), var("PATH").as_ref()) {
                let path = fs::canonicalize(&path).unwrap_or(path);
                let (digest, _) = regular::hash_file(&path)?;
                files.push(MemoFile::of(&path)?);
                programs.insert(prog.into(), digest.hex().into());
            }
        }
    }
    let mut sdk_settings = serde_json::Value::Null;
    if let Some(sdk) = &sdk {
        let settings = Path::new(sdk).join("SDKSettings.json");
        if let Ok((digest, _)) = regular::hash_file(&settings) {
            files.push(MemoFile::of(&settings)?);
            sdk_settings = digest.hex().into();
        }
    }
    let places = Places::new(
        &[],
        &sdk.iter().cloned().collect::<Vec<_>>(),
        &toolchain.iter().cloned().collect::<Vec<_>>(),
    );
    let hidden_flags: Vec<String> = driver_flags.iter().map(|f| places.hide(f)).collect();
    let value = serde_json::json!({
        "schema": SCHEMA,
        "binary": binary.hex(),
        "driver": places.hide(&without_temp_names(&driver.stderr)),
        "programs": programs,
        "sdk_settings": sdk_settings,
        "lang": lang.word(),
        "flags": hidden_flags,
    });
    let canonical = hushrun_core::jcs::canonicalize(&value)
        .map_err(|_| NoIdentity::Driver("canonical form"))?;
    Ok((
        Identity {
            digest: Digest::of(canonical.as_bytes()),
            clang,
            target,
            sdk,
            toolchain,
            system,
        },
        files,
    ))
}

/// What the driver resolves `-march=native` and its kin to (B-53): its `-###` text for a compile
/// with them, the toolchain's folders hidden. Asked once per identity and flags, memoised.
pub fn native(
    compiler: &OsString,
    identity: &Identity,
    lang: Lang,
    flags: &[String],
    var: &dyn Fn(&str) -> Option<OsString>,
    memos: &crate::store::Store,
    ask: &Ask<'_>,
) -> Option<String> {
    let key = Digest::of(
        format!(
            "hushrun.cc-native/1\n{}\n{}\n{}",
            identity.digest.hex(),
            lang.word(),
            flags.join("\n")
        )
        .as_bytes(),
    );
    let path = memos
        .root()
        .join(crate::compiler::MEMOS)
        .join(format!("{MEMO_PREFIX}native-{}.txt", key.hex()));
    if let Ok(text) = fs::read_to_string(&path) {
        return Some(text);
    }
    let exe = which(compiler, var("PATH").as_ref())?;
    let mut probe: Vec<String> = vec![
        "-###".into(),
        "-c".into(),
        "-x".into(),
        lang.word().into(),
        "/dev/null".into(),
        "-o".into(),
        "/dev/null".into(),
    ];
    probe.extend(flags.iter().cloned());
    let said = ask(&exe, &probe).filter(|s| s.success)?;
    let text = identity.places(&[]).hide(&without_temp_names(&said.stderr));
    let _ = memos.write_whole(&path, text.as_bytes(), None, false);
    Some(text)
}

/// `text` with each temporary file the driver names (`/tmp/ccT2YAtc.s`, GCC's `-###` between
/// `cc1` and `as`: `cc`, six random letters and digits, an extension) as `{temp}` and its
/// extension: those names are new each time the driver is asked, so an identity holding them
/// would differ between two machines, and between two stores of one, with the same compiler.
pub(crate) fn without_temp_names(text: &str) -> String {
    String::from_utf8(without_temp_names_in(text.as_bytes())).unwrap_or_else(|_| text.to_string())
}

/// Each temporary file the driver names in `bytes` as `{temp}` and its extension, as in the
/// identity: a compile's messages, which name the assembler's input when it warns
/// (`/tmp/ccwUJrhb.s: Assembler messages:`) and need not be UTF-8.
pub fn without_temp_names_in(bytes: &[u8]) -> Vec<u8> {
    let continues = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/');
    let mut out = Vec::with_capacity(bytes.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(found) = memchr::memmem::find(&bytes[from..], b"/cc") {
        let slash = from + found;
        let random = slash + 3;
        let dot = random + 6;
        let mut end = dot + 1;
        while end < bytes.len() && bytes[end].is_ascii_lowercase() {
            end += 1;
        }
        let temp = bytes.len() > dot
            && bytes[random..dot].iter().all(u8::is_ascii_alphanumeric)
            && bytes[dot] == b'.'
            && end > dot + 1
            && !bytes.get(end).copied().is_some_and(continues);
        if !temp {
            from = slash + 3;
            continue;
        }
        let mut start = slash;
        while start > last && continues(bytes[start - 1]) {
            start -= 1;
        }
        out.extend_from_slice(&bytes[last..start]);
        out.extend_from_slice(b"{temp}");
        out.extend_from_slice(&bytes[dot..end]);
        last = end;
        from = end;
    }
    out.extend_from_slice(&bytes[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-52
    #[test]
    fn the_driver_s_temporary_files_are_no_part_of_the_identity() {
        let one = " /usr/lib/gcc/aarch64-linux-gnu/12/cc1 -quiet /dev/null -o /tmp/ccT2YAtc.s\n as -EL -o /dev/null /tmp/ccT2YAtc.s\n";
        let two = " /usr/lib/gcc/aarch64-linux-gnu/12/cc1 -quiet /dev/null -o /var/folders/x/ccQ9z0aB.s\n as -EL -o /dev/null /var/folders/x/ccQ9z0aB.s\n";
        assert_eq!(without_temp_names(one), without_temp_names(two));
        assert_eq!(
            without_temp_names(one),
            " /usr/lib/gcc/aarch64-linux-gnu/12/cc1 -quiet /dev/null -o {temp}.s\n as -EL -o /dev/null {temp}.s\n"
        );
        // A quoted one, as clang quotes its words; and what only looks like one stays.
        assert_eq!(
            without_temp_names("\"-o\" \"/tmp/ccAbCdEf.o\""),
            "\"-o\" \"{temp}.o\""
        );
        for kept in [
            "/usr/bin/cc1plus",
            "/opt/cc123456.sh2",
            "/a/ccshort.s",
            "/x/cc",
        ] {
            assert_eq!(without_temp_names(kept), kept);
        }
    }

    // covers: B-52
    #[test]
    fn the_driver_s_search_list_and_sysroot_are_read() {
        let text = "clang -cc1 ...\n#include \"...\" search starts here:\n#include <...> search starts here:\n /nonexistent/usr/lib/clang/21/include\n /nonexistent/SDK/usr/include\n /nonexistent/SDK/System/Library/Frameworks (framework directory)\nEnd of search list.\n";
        let dirs = search_list(text);
        assert_eq!(dirs.len(), 3);
        assert!(dirs[2].framework && !dirs[0].framework);
        assert_eq!(dirs[1].path, "/nonexistent/SDK/usr/include");
        let driver = r#" "/tc/usr/bin/clang" "-cc1" "-triple" "arm64-apple-macosx15.0.0" "-isysroot" "/sdk/MacOSX.sdk" "-x" "c""#;
        assert_eq!(
            quoted_after(driver, "-isysroot").as_deref(),
            Some("/sdk/MacOSX.sdk")
        );
        assert!(quoted_after(driver, "-resource-dir").is_none());
    }
}
