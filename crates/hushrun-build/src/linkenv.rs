//! E, the link environment's identity (B-37): what, outside rustc's own command line, decides a
//! linked output's bytes, by content, computed on each machine from that link's own command and
//! this machine's files.
//!
//! [`identity`] is the one function that computes it, at a link (from rustc's
//! `--print=link-args` line) and before a lookup (from the question a hint keeps, never from an
//! answer it keeps). It asks the C driver for a dry run (`-###`) of the link without its inputs
//! and output, reads the program line the driver would run, and hashes, in order:
//!
//! 1. the dry run's whole text, normalized (rustc's temporary folders, gcc's temporary files,
//!    roots and bases replaced by their names): every flag the link passes and every file and
//!    folder the driver adds, which is what holds `-C rpath`'s relative paths and the loader's
//!    path;
//! 2. the driver (not on Apple's targets, where `cc` is a shim that runs the selected Xcode's
//!    clang, whose dry run is all it gives a link of objects), the program the dry run names
//!    (`collect2`, or the `ld` clang runs), the `ld` `collect2` runs (found as `collect2` finds
//!    it, from the program line's `-fuse-ld=` and the dry run's `COMPILER_PATH`, then `PATH`),
//!    the plugin it loads, and every shared library each of those loads, resolved where the
//!    loader would find it (ELF `DT_NEEDED` through `DT_RPATH`, `LD_LIBRARY_PATH`,
//!    `DT_RUNPATH`, `/etc/ld.so.conf` and the system folders; Mach-O load commands through
//!    `@rpath`, `@loader_path` and `@executable_path`), each by SHA-256;
//! 3. each file the program line names (start files, the compiler runtime), by SHA-256;
//! 4. each library a `-l` of the program line names that the standard library's C runtime asks
//!    for ([`crate::link::C_RUNTIME`]) or that the driver added, as `ld` finds it (the line's
//!    `-L` folders in order, then `ld`'s own), with the files a linker script there names;
//! 5. every file under the toolchain's `lib/rustlib/<host>/bin/` (`rust-lld`, `gcc-ld/*`,
//!    `rust-objcopy`), and `dsymutil` when a program gets a debug bundle;
//! 6. on Apple's targets the SDK's `SDKSettings.json` and `SystemVersion.plist` by content, and
//!    the `-platform_version` the driver passes.
//!
//! E is the SHA-256 of that ordered list of (role, name asked for, digest): which file answered
//! which request is part of it. Where a file lies and the places searched and found empty are
//! not: two machines with the same files in other folders have one E, and a file that appears
//! earlier in a search changes which file answers, so the search is run again at every link and
//! every lookup, never remembered. Only each file's digest is remembered, by the path as found
//! and its stat ([`crate::memo`]).
//!
//! E is **unknown**, with why, when it can't be told with certainty: the driver is neither gcc
//! nor clang by its own dry run; the dry run fails, prints an error or no program line; `ld`'s
//! sysroot is not `/`; a linker script does more than name files; a `-l` is found nowhere; a
//! variable that changes what the loader loads is set for the link (`LD_PRELOAD`, `LD_AUDIT`,
//! any `DYLD_` but the library paths cargo and rustup set, an `LD_LIBRARY_PATH` with a folder
//! that is neither a root nor a base); the link does link-time optimization itself; `-fuse-ld`
//! names a linker other than `bfd`, `gold` and `lld`.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

use crate::link::{self, Answer, Ask, ScriptName};

/// Names the form of E, so that a change to what goes in gives new identities.
const SCHEMA: &str = "hushrun.build.link-environment/1";

/// The output a dry run names: no link runs, and no file is written there.
pub const DRY_OUTPUT: &str = "/nonexistent/hushrun-link";

/// How many linker scripts may lead to one another before the search gives up.
const MOST_SCRIPTS: u8 = 8;

/// How many libraries a program's loads may reach before the search gives up.
const MOST_LOADS: usize = 512;

/// One entry of E: what role a file plays, what was asked for, and its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// `dry-run`, `driver`, `program`, `ld`, `plugin`, `loads`, `interpreter`, `file`,
    /// `library`, `script`, `toolchain`, `dsymutil`, `sdk`, `platform-version`.
    pub role: String,
    /// What was asked for: the name rustc gave the driver, a program as the dry run names it, a
    /// library by `-l<name>` or as a script writes it, a shared library by the name its loader
    /// asks for, a file of the toolchain's `bin/` by its path there. Roots and bases replaced.
    pub name: String,
    /// The file's SHA-256, or the text's for `dry-run` and `platform-version`.
    pub sha256: Digest,
}

/// E for one link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkEnv {
    /// Known: its digest and the ordered list it is the digest of.
    Known {
        /// SHA-256 of the JCS form of the schema and `items`.
        digest: Digest,
        /// Which file answered which request, in order.
        items: Vec<Item>,
    },
    /// Unknown, and why.
    Unknown {
        /// What made it unknown, in words.
        reason: String,
    },
}

impl LinkEnv {
    /// The digest when E is known.
    pub fn digest(&self) -> Option<Digest> {
        match self {
            LinkEnv::Known { digest, .. } => Some(*digest),
            LinkEnv::Unknown { .. } => None,
        }
    }
}

/// What [`identity`] needs: the link's own question and this machine's ways to answer it.
pub struct Question<'a> {
    /// The C driver, as found on the link command's `PATH`.
    pub driver: &'a Path,
    /// The driver as rustc named it (`cc`, a path, `-C linker`'s value), roots replaced.
    pub driver_name: &'a str,
    /// The link's arguments without its inputs and output ([`link::LinkArgs::dry_args`]).
    pub args: &'a [String],
    /// The variables the link command set or removed (rustc's own: `SDKROOT`, `LC_ALL`, …),
    /// `PATH` excepted, which is `path`.
    pub env: &'a [(String, Option<String>)],
    /// The link command's `PATH`.
    pub path: &'a OsStr,
    /// rustc's working directory.
    pub cwd: &'a Path,
    /// The target is Apple's.
    pub apple: bool,
    /// The toolchain's sysroot and host: `<sysroot>/lib/rustlib/<host>/bin/` is hashed.
    pub toolchain: Option<(&'a Path, &'a str)>,
    /// The `dsymutil` rustc runs for a program's debug bundle, as found on `PATH`, when it makes
    /// one.
    pub dsymutil: Option<&'a Path>,
    /// The link itself does link-time optimization: `-C lto` or `-C linker-plugin-lto` on
    /// rustc's command line.
    pub lto: bool,
    /// Every variable of the link's environment whose name starts with `LD_` or `DYLD_`.
    pub loader_env: &'a [(String, String)],
    /// Whether a folder is under a root or a base (B-8): an `LD_LIBRARY_PATH` folder that is
    /// (cargo's `deps`, rustup's toolchain `lib`) is the same in every checkout.
    pub rooted: &'a dyn Fn(&Path) -> bool,
    /// A path or any text with roots and bases replaced by their names (B-8).
    pub placeholders: &'a dyn Fn(&str) -> String,
    /// A file's SHA-256, through the run's digests ([`crate::memo`]).
    pub digest: &'a dyn Fn(&Path) -> io::Result<Digest>,
}

/// Why E is unknown, in words that go into the block's description and the run's log.
type Why = String;

/// E for one link (B-37). Runs the driver's dry run, and on Linux asks the `ld` it finds for
/// its sysroot and its own search folders; every other step is a `stat` or a digest.
pub fn identity(q: &Question<'_>, ask: &Ask<'_>) -> LinkEnv {
    match items(q, ask) {
        Ok(items) => {
            let body = serde_json::json!([SCHEMA, items]);
            match hushrun_core::jcs::digest(&body) {
                Ok(digest) => LinkEnv::Known { digest, items },
                Err(err) => LinkEnv::Unknown {
                    reason: format!("E has no canonical form: {err}"),
                },
            }
        }
        Err(reason) => LinkEnv::Unknown { reason },
    }
}

/// What a dry run said.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DryRun {
    /// The whole text, normalized.
    text: String,
    /// The program line: the last command, split into words.
    program: Vec<String>,
    /// gcc's `COMPILER_PATH=` folders, where `collect2` looks for `ld` before `PATH`.
    compiler_path: Vec<String>,
    /// gcc's `--with-ld=` from `Configured with:`: the `ld` `collect2` runs whatever it finds.
    configured_ld: Option<String>,
}

/// Reads a dry run: an error is a [`Why`].
fn read_dry_run(answer: &Answer, placeholders: &dyn Fn(&str) -> String) -> Result<DryRun, Why> {
    if !answer.success {
        return Err("the C driver's dry run (-###) failed".into());
    }
    // gcc or clang, by their own words: only these two are asked (B-37).
    let mut known_driver = false;
    let mut program = None;
    let mut compiler_path = Vec::new();
    let mut configured_ld = None;
    for line in answer.stderr.lines() {
        if line.starts_with(' ') {
            program = Some(split_command(line)?);
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("error:") {
            return Err(format!("the C driver's dry run printed an error: {line}"));
        }
        if line.starts_with("gcc version ") || line.contains("clang version ") {
            known_driver = true;
        }
        if let Some(rest) = line.strip_prefix("COMPILER_PATH=") {
            compiler_path = rest
                .split(':')
                .filter(|dir| !dir.is_empty())
                .map(str::to_owned)
                .collect();
        }
        if let Some(rest) = line.strip_prefix("Configured with:") {
            configured_ld = rest
                .split_whitespace()
                .find_map(|word| word.strip_prefix("--with-ld="))
                .map(|ld| ld.trim_matches('\'').to_owned());
        }
    }
    if !known_driver {
        return Err("the C driver is neither gcc nor clang by its dry run".into());
    }
    let program = match program {
        Some(words) if !words.is_empty() => words,
        _ => return Err("the C driver's dry run names no program".into()),
    };
    Ok(DryRun {
        text: normalize(&answer.stderr, placeholders),
        program,
        compiler_path,
        configured_ld,
    })
}

/// One command line of a dry run in words: gcc quotes a word with `"` where it must, clang
/// every word, both with `\` before a `"`, a `\` or a `$` inside.
fn split_command(line: &str) -> Result<Vec<String>, Why> {
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(words);
        }
        let mut word = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            match c {
                ' ' if !quoted => break,
                '"' => {
                    quoted = !quoted;
                    chars.next();
                }
                '\\' if quoted => {
                    chars.next();
                    match chars.next() {
                        Some(next) => word.push(next),
                        None => return Err("a dry run's command ends inside a quote".into()),
                    }
                }
                _ => {
                    word.push(c);
                    chars.next();
                }
            }
        }
        if quoted {
            return Err("a dry run's command ends inside a quote".into());
        }
        words.push(word);
    }
}

/// A dry run's text as E holds it: each word with rustc's temporary folder name
/// (`rustcXXXXXX`) and gcc's temporary file names (`ccXXXXXX.res`) made fixed, and roots and
/// bases replaced by their names, also after an option's letters (`-L/…`, `-B/…`); one space
/// between words.
pub fn normalize(text: &str, placeholders: &dyn Fn(&str) -> String) -> String {
    text.lines()
        .map(|line| {
            line.split_whitespace()
                .map(|word| normalize_word(word, placeholders))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_word(word: &str, placeholders: &dyn Fn(&str) -> String) -> String {
    let word = fixed_temporary(word);
    // `-L/x` and `-B/x`: no root starts after a letter, so the part from the `/` is replaced on
    // its own.
    match word.find('/') {
        Some(at)
            if at > 0
                && word[..at]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-') =>
        {
            format!("{}{}", &word[..at], placeholders(&word[at..]))
        }
        _ => placeholders(&word),
    }
}

/// `word` with each `/rustcXXXXXX` folder (rustc's temporary folder, six letters or digits)
/// written `/rustcXXXXXX`, and each `/ccXXXXXX.` (gcc's temporary files) written `/ccXXXXXX.`.
fn fixed_temporary(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    while let Some(at) = rest.find('/') {
        out.push_str(&rest[..=at]);
        rest = &rest[at + 1..];
        for (prefix, end) in [("rustc", None), ("cc", Some('.'))] {
            let Some(after) = rest.strip_prefix(prefix) else {
                continue;
            };
            let name: String = after.chars().take(6).collect();
            let ends = after[name.len()..].chars().next();
            let right_end = match end {
                Some(end) => ends == Some(end),
                None => ends.is_none_or(|c| c == '/' || !c.is_ascii_alphanumeric()),
            };
            if name.len() == 6 && name.chars().all(|c| c.is_ascii_alphanumeric()) && right_end {
                out.push_str(prefix);
                out.push_str("XXXXXX");
                rest = &after[6..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What [`items`] gathers, in order.
struct Gathered<'a, 'q> {
    q: &'a Question<'q>,
    items: Vec<Item>,
    /// Programs and libraries whose loads were followed, by their resolved path.
    followed: Vec<PathBuf>,
}

/// `$ORIGIN` and `${ORIGIN}` in an ELF search folder, as the loader expands them; a folder with
/// any other `$` word (`$LIB`, `$PLATFORM`) can't be told here.
fn expand_origin(dir: &str, origin: &Path) -> Result<PathBuf, Why> {
    let origin = origin.display().to_string();
    let dir = dir
        .replace("${ORIGIN}", &origin)
        .replace("$ORIGIN", &origin);
    if dir.contains('$') {
        return Err(format!("a library search folder names {dir}"));
    }
    Ok(PathBuf::from(dir))
}

impl Gathered<'_, '_> {
    fn text(&mut self, role: &str, name: &str, text: &str) {
        self.items.push(Item {
            role: role.into(),
            name: name.into(),
            sha256: Digest::of(text.as_bytes()),
        });
    }

    fn file(&mut self, role: &str, name: &str, path: &Path) -> Result<(), Why> {
        let sha256 = (self.q.digest)(path)
            .map_err(|err| format!("{} can't be hashed: {err}", path.display()))?;
        self.items.push(Item {
            role: role.into(),
            name: (self.q.placeholders)(name),
            sha256,
        });
        Ok(())
    }

    /// A program that runs on the way, and every shared library it loads.
    fn program(&mut self, role: &str, name: &str, path: &Path) -> Result<(), Why> {
        self.file(role, name, path)?;
        self.loads(path, path)
    }

    /// The libraries `path` loads, and theirs, each once, where the loader finds them. `main` is
    /// the program they are loaded for (`@executable_path`).
    fn loads(&mut self, path: &Path, main: &Path) -> Result<(), Why> {
        let mut queue = vec![path.to_path_buf()];
        while let Some(object) = queue.pop() {
            let resolved = std::fs::canonicalize(&object).unwrap_or_else(|_| object.clone());
            if self.followed.contains(&resolved) {
                continue;
            }
            if self.followed.len() >= MOST_LOADS {
                return Err("the programs on the way load too many libraries to follow".into());
            }
            self.followed.push(resolved);
            let Some(found) = binary::read(&object) else {
                continue;
            };
            let mut next = Vec::new();
            match found {
                binary::Binary::Elf(elf) => {
                    if let Some(interpreter) = &elf.interpreter {
                        let interpreter = PathBuf::from(interpreter);
                        if interpreter.is_file() {
                            self.file(
                                "interpreter",
                                &interpreter.display().to_string(),
                                &interpreter,
                            )?;
                        }
                    }
                    for needed in &elf.needed {
                        let place = self.elf_library(&object, &elf, needed)?.ok_or_else(|| {
                            format!(
                                "{needed}, which {} loads, is found nowhere",
                                object.display()
                            )
                        })?;
                        self.file("loads", needed, &place)?;
                        next.push(place);
                    }
                }
                binary::Binary::MachO(macho) => {
                    for dylib in &macho.dylibs {
                        match self.macho_library(&object, main, &macho, dylib) {
                            MachOPlace::File(place) => {
                                self.file("loads", dylib, &place)?;
                                next.push(place);
                            }
                            MachOPlace::System => self.text("loads", dylib, "system shared cache"),
                            MachOPlace::Nowhere => {
                                return Err(format!(
                                    "{dylib}, which {} loads, is found nowhere",
                                    object.display()
                                ));
                            }
                        }
                    }
                }
            }
            // Depth first, in the order each names them.
            queue.extend(next.into_iter().rev());
        }
        Ok(())
    }

    /// Where the loader takes an ELF object's `DT_NEEDED` entry from: `DT_RPATH` (when there
    /// is no `DT_RUNPATH`), `LD_LIBRARY_PATH`, `DT_RUNPATH`, then the folders `ld.so.conf` lists
    /// and the system's own.
    fn elf_library(
        &self,
        object: &Path,
        elf: &binary::Elf,
        needed: &str,
    ) -> Result<Option<PathBuf>, Why> {
        if needed.contains('/') {
            let path = self.q.cwd.join(needed);
            return Ok(path.is_file().then_some(path));
        }
        let origin = object.parent().unwrap_or(Path::new("/"));
        let expand = |list: &str| -> Result<Vec<PathBuf>, Why> {
            list.split(':')
                .filter(|dir| !dir.is_empty())
                .map(|dir| expand_origin(dir, origin))
                .collect()
        };
        let mut dirs: Vec<PathBuf> = Vec::new();
        if elf.runpath.is_none()
            && let Some(rpath) = &elf.rpath
        {
            dirs.extend(expand(rpath)?);
        }
        if let Some((_, value)) = self
            .q
            .loader_env
            .iter()
            .find(|(n, _)| n == "LD_LIBRARY_PATH")
        {
            dirs.extend(
                value
                    .split(':')
                    .filter(|d| !d.is_empty())
                    .map(PathBuf::from),
            );
        }
        if let Some(runpath) = &elf.runpath {
            dirs.extend(expand(runpath)?);
        }
        dirs.extend(system_library_dirs());
        Ok(dirs
            .into_iter()
            .map(|dir| self.q.cwd.join(dir).join(needed))
            .find(|path| path.is_file()))
    }

    fn macho_library(
        &self,
        object: &Path,
        main: &Path,
        macho: &binary::MachO,
        dylib: &str,
    ) -> MachOPlace {
        let loader = object.parent().unwrap_or(Path::new("/"));
        let executable = main.parent().unwrap_or(Path::new("/"));
        let expand = |text: &String| -> PathBuf {
            if let Some(rest) = text.strip_prefix("@loader_path") {
                return PathBuf::from(format!("{}{rest}", loader.display()));
            }
            if let Some(rest) = text.strip_prefix("@executable_path") {
                return PathBuf::from(format!("{}{rest}", executable.display()));
            }
            PathBuf::from(text)
        };
        // `@rpath` is each `LC_RPATH` of the image that loads it, then of the program it is
        // loaded for, as dyld searches them.
        let main_rpaths: Vec<PathBuf> = match binary::read(main) {
            Some(binary::Binary::MachO(main_macho)) if main != object => main_macho
                .rpaths
                .iter()
                .map(|rpath| {
                    let rest = rpath
                        .strip_prefix("@executable_path")
                        .or_else(|| rpath.strip_prefix("@loader_path"));
                    match rest {
                        Some(rest) => PathBuf::from(format!("{}{rest}", executable.display())),
                        None => PathBuf::from(rpath),
                    }
                })
                .collect(),
            _ => Vec::new(),
        };
        let candidates: Vec<PathBuf> = match dylib.strip_prefix("@rpath/") {
            Some(rest) => macho
                .rpaths
                .iter()
                .map(expand)
                .chain(main_rpaths)
                .map(|dir| dir.join(rest))
                .collect(),
            None => vec![expand(&dylib.to_owned())],
        };
        if let Some(found) = candidates.into_iter().find(|path| path.is_file()) {
            return MachOPlace::File(found);
        }
        // macOS keeps its own libraries only in the shared cache: not files on disk.
        if dylib.starts_with("/usr/lib/") || dylib.starts_with("/System/") {
            return MachOPlace::System;
        }
        MachOPlace::Nowhere
    }
}

enum MachOPlace {
    File(PathBuf),
    System,
    Nowhere,
}

/// The folders the Linux loader searches by default: those `/etc/ld.so.conf` lists (and the
/// files it includes), then the system's own.
fn system_library_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    conf_dirs(Path::new("/etc/ld.so.conf"), &mut out, 0);
    for dir in ["/lib64", "/usr/lib64", "/lib", "/usr/lib"] {
        let dir = PathBuf::from(dir);
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

/// The folders one `ld.so.conf` file lists, with the files its `include` lines name (a `*` in
/// the file's name matches any text), in order.
fn conf_dirs(file: &Path, out: &mut Vec<PathBuf>, depth: u8) {
    if depth > 8 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let here = file.parent().unwrap_or(Path::new("/"));
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() || line.starts_with("hwcap") {
            continue;
        }
        if let Some(pattern) = line.strip_prefix("include") {
            for pattern in pattern.split_whitespace() {
                let pattern = here.join(pattern);
                for included in glob_one(&pattern) {
                    conf_dirs(&included, out, depth + 1);
                }
            }
            continue;
        }
        for dir in line.split([':', ',', ' ', '\t']).filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            if !out.contains(&dir) {
                out.push(dir);
            }
        }
    }
}

/// The files a pattern with `*` only in its last part names, sorted.
fn glob_one(pattern: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (
        pattern.parent(),
        pattern.file_name().and_then(OsStr::to_str),
    ) else {
        return Vec::new();
    };
    let Some((before, after)) = name.split_once('*') else {
        return if pattern.is_file() {
            vec![pattern.to_path_buf()]
        } else {
            Vec::new()
        };
    };
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            (file.len() >= before.len() + after.len()
                && file.starts_with(before)
                && file.ends_with(after))
            .then(|| entry.path())
        })
        .collect();
    out.sort();
    out
}

/// The linkers `-fuse-ld` may name for E to be known.
const KNOWN_LINKERS: &[&str] = &["bfd", "gold", "lld"];

/// The program a dry run may end in: `collect2`, or an `ld` clang runs itself.
fn is_linker_program(name: &str) -> bool {
    matches!(
        name,
        "collect2" | "ld" | "ld.bfd" | "ld.gold" | "ld.lld" | "ld64.lld" | "ld-classic" | "ld64"
    )
}

fn items(q: &Question<'_>, ask: &Ask<'_>) -> Result<Vec<Item>, Why> {
    loader_variables(q)?;
    if q.lto {
        return Err("the link does link-time optimization (-C lto or -C linker-plugin-lto)".into());
    }
    let flat = link::flatten_args(q.args);
    if flat.iter().any(|arg| arg.starts_with("-flto")) {
        return Err("the link does link-time optimization (-flto)".into());
    }
    for arg in q.args {
        if let Some(name) = arg.strip_prefix("-fuse-ld=")
            && !KNOWN_LINKERS.contains(&name)
        {
            return Err(format!("-fuse-ld names {name}, not bfd, gold or lld"));
        }
    }
    let mut dry_args = q.args.to_vec();
    dry_args.extend(["-o".to_owned(), DRY_OUTPUT.to_owned(), "-###".to_owned()]);
    let answer = ask(q.driver, &dry_args, q.path, q.env)
        .ok_or_else(|| "the C driver can't be asked for a dry run".to_owned())?;
    let dry = read_dry_run(&answer, q.placeholders)?;
    let mut g = Gathered {
        q,
        items: Vec::new(),
        followed: Vec::new(),
    };
    g.text("dry-run", &(q.placeholders)(q.driver_name), &dry.text);
    if !q.apple {
        g.program("driver", q.driver_name, q.driver)?;
    }

    // The program the dry run names, and the `ld` it is or runs.
    let program = PathBuf::from(&dry.program[0]);
    let program_name = program
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_owned();
    if !is_linker_program(&program_name) {
        return Err(format!(
            "the C driver runs {program_name}, which is neither collect2 nor an ld hushrun knows"
        ));
    }
    let program = q.cwd.join(program);
    g.program("program", &dry.program[0], &program)?;
    let mut fuse = None;
    for word in &dry.program[1..] {
        if let Some(name) = word.strip_prefix("-fuse-ld=") {
            fuse = Some(name.to_owned());
        }
    }
    if let Some(name) = &fuse
        && !KNOWN_LINKERS.contains(&name.as_str())
    {
        return Err(format!("-fuse-ld names {name}, not bfd, gold or lld"));
    }
    let ld = if program_name == "collect2" {
        let (name, found) = collect2_ld(q, &dry, fuse.as_deref())?;
        g.program("ld", &name, &found)?;
        found
    } else {
        program.clone()
    };

    // What the program line names: the plugin, the files it adds, the SDK.
    let mut sdk = None;
    let mut words = dry.program[1..].iter().peekable();
    let mut platform = None;
    while let Some(word) = words.next() {
        match word.as_str() {
            "-o" => {
                words.next();
            }
            "-plugin" => {
                if let Some(plugin) = words.next() {
                    g.program("plugin", plugin, &q.cwd.join(plugin))?;
                }
            }
            "-syslibroot" => sdk = words.next().cloned(),
            "-platform_version" => {
                let triple: Vec<String> = words.by_ref().take(3).cloned().collect();
                platform = Some(triple.join(" "));
            }
            _ => {
                let file = word.strip_prefix("-plugin-opt=").unwrap_or(word.as_str());
                let path = Path::new(file);
                if path.is_absolute()
                    && !link::in_rustc_temp(path)
                    && std::fs::metadata(path).is_ok_and(|m| m.is_file())
                {
                    g.file("file", file, path)?;
                }
            }
        }
    }

    // `ld`'s sysroot and its own folders (GNU `ld`; Apple's `ld` searches the SDK).
    let mut ld_dirs = Vec::new();
    if q.apple {
        let root = sdk.as_deref().map(|sdk| q.cwd.join(sdk));
        match &root {
            Some(root) => {
                for dir in ["usr/lib", "usr/local/lib"] {
                    ld_dirs.push(root.join(dir));
                }
                ld_dirs.push(PathBuf::from("/usr/local/lib"));
            }
            None => {
                ld_dirs.push(PathBuf::from("/usr/lib"));
                ld_dirs.push(PathBuf::from("/usr/local/lib"));
            }
        }
    } else {
        let line_sysroot = dry.program[1..]
            .iter()
            .rev()
            .find_map(|word| word.strip_prefix("--sysroot="))
            .map(str::to_owned);
        let mut asked = Vec::new();
        if let Some(root) = &line_sysroot {
            asked.push(format!("--sysroot={root}"));
        }
        asked.push("--print-sysroot".to_owned());
        let said = match ask(&ld, &asked, q.path, q.env) {
            Some(answer) if answer.success => answer.stdout.trim().to_owned(),
            // No such option (`lld`): the line's own, if any.
            _ => line_sysroot.unwrap_or_default(),
        };
        if !said.is_empty() && Path::new(&said) != Path::new("/") {
            return Err(format!("ld's sysroot is {said}, not /"));
        }
        if let Some(answer) = ask(&ld, &["--verbose".to_owned()], q.path, q.env)
            && answer.success
        {
            for dir in link::search_dir_entries(&answer.stdout) {
                let dir = dir.strip_prefix('=').unwrap_or(&dir).to_owned();
                ld_dirs.push(q.cwd.join(dir));
            }
        }
    }

    // The C runtime and what the driver added, as `ld` finds them.
    libraries(&mut g, &dry, sdk.as_deref(), &ld_dirs)?;

    // The toolchain's own linker tools.
    if let Some((sysroot, host)) = q.toolchain {
        let bin = sysroot.join("lib/rustlib").join(host).join("bin");
        let mut files = Vec::new();
        tree(&bin, &bin, &mut files);
        for (path, rest) in files {
            g.program("toolchain", &rest, &path)?;
        }
    }
    if let Some(dsymutil) = q.dsymutil {
        g.program("dsymutil", "dsymutil", dsymutil)?;
        // Apple's `/usr/bin/dsymutil` is a shim that runs the selected Xcode's, which lies
        // beside the `ld` clang runs.
        let beside = ld.parent().map(|dir| dir.join("dsymutil"));
        if let Some(beside) = beside
            && q.apple
            && dsymutil.starts_with("/usr/bin")
            && beside.is_file()
        {
            g.program("dsymutil", "dsymutil", &beside)?;
        }
    }
    if q.apple {
        if let Some(sdk) = &sdk {
            let root = q.cwd.join(sdk);
            for (name, rest) in [
                ("SDKSettings.json", "SDKSettings.json"),
                (
                    "SystemVersion.plist",
                    "System/Library/CoreServices/SystemVersion.plist",
                ),
            ] {
                let path = root.join(rest);
                if !path.is_file() {
                    return Err(format!("the SDK {sdk} has no {rest}"));
                }
                g.file("sdk", name, &path)?;
            }
        }
        if let Some(platform) = &platform {
            g.text("platform-version", platform, platform);
        }
    }
    Ok(g.items)
}

/// The variables that change what the loader loads for the programs of a link: E is unknown
/// with any of them, but the library paths cargo and rustup set, whose folders are roots or
/// bases (cargo's `deps`, rustup's toolchain `lib`), and on macOS the fallback folders cargo
/// adds when none was set.
fn loader_variables(q: &Question<'_>) -> Result<(), Why> {
    for (name, value) in q.loader_env {
        if value.is_empty() {
            continue;
        }
        let library_path = matches!(
            name.as_str(),
            "LD_LIBRARY_PATH" | "DYLD_LIBRARY_PATH" | "DYLD_FALLBACK_LIBRARY_PATH"
        );
        if library_path {
            let fallback = name == "DYLD_FALLBACK_LIBRARY_PATH";
            for dir in value.split(':').filter(|dir| !dir.is_empty()) {
                let path = Path::new(dir);
                let default = fallback
                    && (matches!(dir, "/usr/local/lib" | "/usr/lib")
                        || (dir.ends_with("/lib")
                            && path.parent().is_some_and(|home| {
                                std::env::var_os("HOME").is_some_and(|h| Path::new(&h) == home)
                            })));
                if !(q.rooted)(path) && !default {
                    return Err(format!(
                        "{name} names {dir}, which is neither a root nor a base"
                    ));
                }
            }
            continue;
        }
        if name.starts_with("DYLD_") || matches!(name.as_str(), "LD_PRELOAD" | "LD_AUDIT") {
            return Err(format!("{name} is set for the link"));
        }
    }
    Ok(())
}

/// The `ld` `collect2` runs: gcc's configured one, else `real-ld`, `collect-ld` and the name
/// (`ld`, or `ld.<name>` for the program line's `-fuse-ld=<name>`) in the dry run's
/// `COMPILER_PATH`, then the name on the link's `PATH`. The name asked for, and the file.
fn collect2_ld(
    q: &Question<'_>,
    dry: &DryRun,
    fuse: Option<&str>,
) -> Result<(String, PathBuf), Why> {
    let name = match fuse {
        Some(fuse) => format!("ld.{fuse}"),
        None => "ld".to_owned(),
    };
    if let Some(configured) = &dry.configured_ld {
        let path = PathBuf::from(configured);
        if executable(&path) {
            return Ok((name, path));
        }
    }
    for program in ["real-ld", "collect-ld", name.as_str()] {
        for dir in &dry.compiler_path {
            let path = q.cwd.join(dir).join(program);
            if executable(&path) {
                return Ok((program.to_owned(), path));
            }
        }
    }
    for dir in std::env::split_paths(q.path) {
        let path = q.cwd.join(dir).join(&name);
        if executable(&path) {
            return Ok((name, path));
        }
    }
    Err(format!("the {name} collect2 runs is found nowhere"))
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Each library a `-l` of the program line names, when it is the C runtime the standard
/// library asks for or one the driver added (any other is keyed by content in `link`, B-6), as
/// `ld` finds it: in the line's `-L` folders in order (on Apple's targets each under the SDK
/// first, when it is there), then `ld`'s own; `.so` before `.a` while the line links
/// dynamically, `.a` alone while it links statically; with each file a linker script found
/// there names.
fn libraries(
    g: &mut Gathered<'_, '_>,
    dry: &DryRun,
    sdk: Option<&str>,
    ld_dirs: &[PathBuf],
) -> Result<(), Why> {
    let q = g.q;
    let asked: Vec<String> = asked_libraries(q.args);
    let root = sdk.map(|sdk| q.cwd.join(sdk));
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut wanted: Vec<(String, bool)> = Vec::new();
    let mut static_mode = false;
    let mut states = Vec::new();
    let mut words = dry.program[1..].iter();
    while let Some(word) = words.next() {
        let word = word.as_str();
        match word {
            "-Bstatic" | "-dn" | "-non_shared" | "-static" => static_mode = true,
            "-Bdynamic" | "-dy" | "-call_shared" => static_mode = false,
            "--push-state" => states.push(static_mode),
            "--pop-state" => static_mode = states.pop().unwrap_or(false),
            "-L" => {
                if let Some(dir) = words.next() {
                    dirs.push(q.cwd.join(dir));
                }
            }
            "-l" => {
                if let Some(name) = words.next() {
                    wanted.push((name.clone(), static_mode));
                }
            }
            _ => {
                if let Some(dir) = word.strip_prefix("-L") {
                    dirs.push(q.cwd.join(dir));
                } else if let Some(name) = word.strip_prefix("-l") {
                    wanted.push((name.to_owned(), static_mode));
                }
            }
        }
    }
    let mut search: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if q.apple
            && let (Some(root), Ok(rest)) = (&root, dir.strip_prefix("/"))
        {
            let under = root.join(rest);
            if under.is_dir() && !search.contains(&under) {
                search.push(under);
            }
        }
        if !search.contains(&dir) {
            search.push(dir);
        }
    }
    for dir in ld_dirs {
        if !search.contains(dir) {
            search.push(dir.clone());
        }
    }
    let mut seen = Vec::new();
    for (name, static_mode) in wanted {
        let runtime = link::C_RUNTIME.contains(&name.as_str());
        if !runtime && asked.contains(&name) {
            continue;
        }
        if seen.contains(&(name.clone(), static_mode)) {
            continue;
        }
        seen.push((name.clone(), static_mode));
        let found = find_library(&name, static_mode, q.apple, &search)
            .ok_or_else(|| format!("-l{name} is found nowhere"))?;
        g.file("library", &format!("-l{name}"), &found)?;
        follow_script(g, &found, &search, 0)?;
    }
    Ok(())
}

/// The `-l` names rustc's own line gives (with `-Wl,` and `-Xlinker` read as the linker's).
fn asked_libraries(args: &[String]) -> Vec<String> {
    let flat = link::flatten_args(args);
    let mut out = Vec::new();
    let mut i = 0;
    while i < flat.len() {
        let word = flat[i];
        if word == "-l" {
            if let Some(name) = flat.get(i + 1) {
                out.push((*name).to_owned());
            }
            i += 2;
            continue;
        }
        if let Some(name) = word.strip_prefix("-l")
            && !name.is_empty()
        {
            out.push(name.to_owned());
        }
        i += 1;
    }
    out
}

/// The file `ld` takes `-l<name>` from in `dirs`, in order.
fn find_library(name: &str, static_mode: bool, apple: bool, dirs: &[PathBuf]) -> Option<PathBuf> {
    let files: Vec<String> = match name.strip_prefix(':') {
        Some(file) => vec![file.to_owned()],
        None if apple => ["tbd", "dylib", "so", "a"]
            .iter()
            .map(|ext| format!("lib{name}.{ext}"))
            .collect(),
        None if static_mode => vec![format!("lib{name}.a")],
        None => vec![format!("lib{name}.so"), format!("lib{name}.a")],
    };
    dirs.iter()
        .flat_map(|dir| files.iter().map(move |file| dir.join(file)))
        .find(|path| std::fs::metadata(path).is_ok_and(|m| m.is_file()))
}

/// A library found that is a GNU linker script: each file it names, as `ld` takes it.
fn follow_script(
    g: &mut Gathered<'_, '_>,
    found: &Path,
    search: &[PathBuf],
    depth: u8,
) -> Result<(), Why> {
    let mut head = [0u8; 512];
    let read = File::open(found)
        .and_then(|mut file| io::Read::read(&mut file, &mut head))
        .map_err(|err| format!("{} can't be read: {err}", found.display()))?;
    if !link::is_script(found, &head[..read]) {
        return Ok(());
    }
    if depth >= MOST_SCRIPTS {
        return Err("linker scripts lead to one another too deep".into());
    }
    let text = crate::regular::read(found)
        .map_err(|err| format!("{} can't be read: {err}", found.display()))?;
    let names = link::script_names(&text).map_err(|err| format!("{}: {err}", found.display()))?;
    for name in names {
        let (asked, path) = match &name {
            ScriptName::Library(library) => (
                format!("-l{library}"),
                find_library(library, false, g.q.apple, search),
            ),
            ScriptName::File(file) => (
                file.clone(),
                link::script_places(&name, found, g.q.cwd, search)
                    .into_iter()
                    .find(|path| path.is_file()),
            ),
        };
        // `AS_NEEDED` names a library the link may not need; a name found nowhere is one `ld`
        // would fail on, which a link that succeeded didn't.
        let Some(path) = path else {
            return Err(format!(
                "{asked}, which {} names, is found nowhere",
                found.display()
            ));
        };
        g.file("script", &asked, &path)?;
        follow_script(g, &path, search, depth + 1)?;
    }
    Ok(())
}

/// Every regular file under `dir`, with its path from `top`, sorted.
fn tree(top: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            tree(top, &path, out);
        } else if meta.is_file() {
            let rest = path
                .strip_prefix(top)
                .map(|rest| rest.display().to_string())
                .unwrap_or_default();
            out.push((path, rest));
        }
    }
}

/// Just enough of ELF and Mach-O to know what a program loads.
mod binary {
    use std::fs::File;
    use std::path::Path;

    /// What an ELF object asks the loader for.
    pub struct Elf {
        pub interpreter: Option<String>,
        pub needed: Vec<String>,
        pub rpath: Option<String>,
        pub runpath: Option<String>,
    }

    /// What a Mach-O object asks the loader for.
    pub struct MachO {
        pub dylibs: Vec<String>,
        pub rpaths: Vec<String>,
    }

    pub enum Binary {
        Elf(Elf),
        MachO(MachO),
    }

    /// The object at `path`, when it is one of these; `None` for anything else (a script, a
    /// file that can't be read, a static archive).
    pub fn read(path: &Path) -> Option<Binary> {
        let file = File::open(path).ok()?;
        let mut magic = [0u8; 4];
        at(&file, 0, &mut magic)?;
        match magic {
            [0x7f, b'E', b'L', b'F'] => elf(&file).map(Binary::Elf),
            [0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe] => {
                macho(&file, 0).map(Binary::MachO)
            }
            [0xca, 0xfe, 0xba, 0xbe] | [0xca, 0xfe, 0xba, 0xbf] => {
                fat(&file, magic[3] == 0xbf).map(Binary::MachO)
            }
            _ => None,
        }
    }

    fn at(file: &File, offset: u64, buf: &mut [u8]) -> Option<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            file.read_exact_at(buf, offset).ok()
        }
        #[cfg(not(unix))]
        {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = file.try_clone().ok()?;
            file.seek(SeekFrom::Start(offset)).ok()?;
            file.read_exact(buf).ok()
        }
    }

    fn bytes(file: &File, offset: u64, len: u64) -> Option<Vec<u8>> {
        if len > 64 << 20 {
            return None;
        }
        let mut buf = vec![0u8; usize::try_from(len).ok()?];
        at(file, offset, &mut buf)?;
        Some(buf)
    }

    struct Reader {
        big: bool,
    }

    impl Reader {
        fn u16(&self, b: &[u8], at: usize) -> Option<u64> {
            let x: [u8; 2] = b.get(at..at + 2)?.try_into().ok()?;
            Some(u64::from(if self.big {
                u16::from_be_bytes(x)
            } else {
                u16::from_le_bytes(x)
            }))
        }
        fn u32(&self, b: &[u8], at: usize) -> Option<u64> {
            let x: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
            Some(u64::from(if self.big {
                u32::from_be_bytes(x)
            } else {
                u32::from_le_bytes(x)
            }))
        }
        fn u64(&self, b: &[u8], at: usize) -> Option<u64> {
            let x: [u8; 8] = b.get(at..at + 8)?.try_into().ok()?;
            Some(if self.big {
                u64::from_be_bytes(x)
            } else {
                u64::from_le_bytes(x)
            })
        }
        fn word(&self, b: &[u8], at: usize, wide: bool) -> Option<u64> {
            if wide {
                self.u64(b, at)
            } else {
                self.u32(b, at)
            }
        }
    }

    fn c_string(b: &[u8], at: usize) -> Option<String> {
        let rest = b.get(at..)?;
        let end = rest.iter().position(|&c| c == 0)?;
        String::from_utf8(rest[..end].to_vec()).ok()
    }

    fn elf(file: &File) -> Option<Elf> {
        let mut head = [0u8; 64];
        at(file, 0, &mut head)?;
        let wide = head[4] == 2;
        let r = Reader { big: head[5] == 2 };
        let (phoff, shoff, phentsize, phnum, shentsize, shnum) = if wide {
            (
                r.u64(&head, 0x20)?,
                r.u64(&head, 0x28)?,
                r.u16(&head, 0x36)?,
                r.u16(&head, 0x38)?,
                r.u16(&head, 0x3a)?,
                r.u16(&head, 0x3c)?,
            )
        } else {
            (
                r.u32(&head, 0x1c)?,
                r.u32(&head, 0x20)?,
                r.u16(&head, 0x2a)?,
                r.u16(&head, 0x2c)?,
                r.u16(&head, 0x2e)?,
                r.u16(&head, 0x30)?,
            )
        };
        let mut interpreter = None;
        let phdrs = bytes(file, phoff, phentsize * phnum)?;
        for i in 0..phnum {
            let base = usize::try_from(i * phentsize).ok()?;
            if r.u32(&phdrs, base)? != 3 {
                continue;
            }
            let (offset, size) = if wide {
                (r.u64(&phdrs, base + 8)?, r.u64(&phdrs, base + 0x20)?)
            } else {
                (r.u32(&phdrs, base + 4)?, r.u32(&phdrs, base + 0x10)?)
            };
            interpreter = c_string(&bytes(file, offset, size)?, 0);
        }
        let mut out = Elf {
            interpreter,
            needed: Vec::new(),
            rpath: None,
            runpath: None,
        };
        let shdrs = bytes(file, shoff, shentsize * shnum)?;
        let section = |i: u64| -> Option<(u64, u64, u64, u64)> {
            let base = usize::try_from(i * shentsize).ok()?;
            Some(if wide {
                (
                    r.u32(&shdrs, base + 4)?,
                    r.u64(&shdrs, base + 0x18)?,
                    r.u64(&shdrs, base + 0x20)?,
                    r.u32(&shdrs, base + 0x28)?,
                )
            } else {
                (
                    r.u32(&shdrs, base + 4)?,
                    r.u32(&shdrs, base + 0x10)?,
                    r.u32(&shdrs, base + 0x14)?,
                    r.u32(&shdrs, base + 0x18)?,
                )
            })
        };
        for i in 0..shnum {
            let (kind, offset, size, link) = section(i)?;
            if kind != 6 {
                continue;
            }
            let dynamic = bytes(file, offset, size)?;
            let (_, str_offset, str_size, _) = section(link)?;
            let strings = bytes(file, str_offset, str_size)?;
            let entry = if wide { 16 } else { 8 };
            let half = entry / 2;
            for at in (0..dynamic.len().saturating_sub(entry - 1)).step_by(entry) {
                let tag = r.word(&dynamic, at, wide)?;
                let value = usize::try_from(r.word(&dynamic, at + half, wide)?).ok()?;
                match tag {
                    0 => break,
                    1 => out.needed.push(c_string(&strings, value)?),
                    15 => out.rpath = c_string(&strings, value),
                    29 => out.runpath = c_string(&strings, value),
                    _ => {}
                }
            }
        }
        Some(out)
    }

    /// This machine's CPU type as Mach-O names it: a universal file's slice for it is the one
    /// the loader runs.
    fn host_cpu() -> u32 {
        if cfg!(target_arch = "aarch64") {
            0x0100_000c
        } else {
            0x0100_0007
        }
    }

    fn fat(file: &File, wide: bool) -> Option<MachO> {
        let mut head = [0u8; 8];
        at(file, 0, &mut head)?;
        let r = Reader { big: true };
        let count = r.u32(&head, 4)?.min(32);
        let size = if wide { 32 } else { 20 };
        let arches = bytes(file, 8, count * size)?;
        let mut first = None;
        for i in 0..count {
            let base = usize::try_from(i * size).ok()?;
            let cpu = r.u32(&arches, base)?;
            let offset = if wide {
                r.u64(&arches, base + 8)?
            } else {
                r.u32(&arches, base + 8)?
            };
            if u64::from(host_cpu()) == cpu {
                return macho(file, offset);
            }
            first.get_or_insert(offset);
        }
        macho(file, first?)
    }

    fn macho(file: &File, offset: u64) -> Option<MachO> {
        let mut head = [0u8; 32];
        at(file, offset, &mut head)?;
        let r = Reader { big: false };
        let wide = head[0] == 0xcf;
        let count = r.u32(&head, 16)?;
        let size = r.u32(&head, 20)?;
        let commands = bytes(file, offset + if wide { 32 } else { 28 }, size)?;
        let mut out = MachO {
            dylibs: Vec::new(),
            rpaths: Vec::new(),
        };
        let mut at = 0usize;
        for _ in 0..count {
            let cmd = r.u32(&commands, at)?;
            let cmdsize = usize::try_from(r.u32(&commands, at + 4)?).ok()?;
            if cmdsize < 8 {
                return None;
            }
            let body = commands.get(at..at + cmdsize)?;
            match cmd {
                // LC_LOAD_DYLIB, LC_LOAD_WEAK_DYLIB, LC_REEXPORT_DYLIB, LC_LAZY_LOAD_DYLIB,
                // LC_LOAD_UPWARD_DYLIB.
                0xc | 0x8000_0018 | 0x8000_001f | 0x20 | 0x8000_0023 => {
                    let name = usize::try_from(r.u32(body, 8)?).ok()?;
                    out.dylibs.push(c_string(body, name)?);
                }
                // LC_RPATH.
                0x8000_001c => {
                    let path = usize::try_from(r.u32(body, 8)?).ok()?;
                    out.rpaths.push(c_string(body, path)?);
                }
                _ => {}
            }
            at += cmdsize;
        }
        Some(out)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::*;

    /// Makes `path` an executable shell script that runs `body`.
    fn script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// An `ld` with no sysroot that searches `ldlib` beside its folder; `mark` tells two apart.
    fn ld_script(mark: &str) -> String {
        format!(
            "# {mark}\nr=${{0%/*}}/..\n\
             case \"$1\" in --print-sysroot) echo;; --verbose) echo \"SEARCH_DIR(\\\"=$r/ldlib\\\")\";; *) exit 1;; esac"
        )
    }

    fn file(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// A gcc-like setup under `root`: a driver whose dry run names `collect2`, its plugin, a
    /// start file, `-L<root>/lib` and the `-l` it adds (`more`, then the line's own words); a
    /// `libc.so` script there naming `libc.so.6`; an `ld` on `<root>/bin` that has no sysroot
    /// and searches `<root>/ldlib`; a toolchain with its `bin/` tools.
    struct Setup {
        root: PathBuf,
        path: OsString,
        env: Vec<(String, Option<String>)>,
        loader: Vec<(String, String)>,
        lto: bool,
        apple: bool,
        args: Vec<String>,
    }

    impl Setup {
        fn new(root: &Path) -> Setup {
            let root = link::resolve_folder(root);
            Setup::driver(&root, "gcc version 99.1.0 (fixture)", "-lc");
            script(&root.join("gcc/collect2"), "exit 1");
            file(&root.join("gcc/liblto_plugin.so"), b"plugin one");
            script(&root.join("gcc/lto-wrapper"), "exit 1");
            file(&root.join("lib/crt1.o"), b"crt one");
            file(&root.join("lib/libc.so.6"), b"\x7fELF libc one");
            file(
                &root.join("lib/libc.so"),
                b"/* GNU ld script */\nGROUP ( libc.so.6 )\n",
            );
            script(&root.join("bin/ld"), &ld_script("one"));
            let bin = root.join("toolchain/lib/rustlib/host/bin");
            file(&bin.join("rust-lld"), b"lld one");
            file(&bin.join("rust-objcopy"), b"objcopy one");
            file(&bin.join("gcc-ld/ld.lld"), b"wrapper one");
            Setup {
                path: std::env::join_paths([root.join("early"), root.join("bin")]).unwrap(),
                root,
                env: Vec::new(),
                loader: Vec::new(),
                lto: false,
                apple: false,
                args: vec![
                    "-Wl,--as-needed".into(),
                    "-lgcc_s".into(),
                    "-nodefaultlibs".into(),
                ],
            }
        }

        /// The driver, saying `version` and adding the `-l` words `libraries`.
        fn driver(root: &Path, version: &str, libraries: &str) {
            let r = "$r";
            script(
                &root.join("cc"),
                &format!(
                    "for a in \"$@\"; do [ \"$a\" = '-###' ] && dry=1; done\n\
                     [ -n \"$dry\" ] || exit 1\n\
                     r=${{0%/*}}\n\
                     echo 'Using built-in specs.' >&2\n\
                     echo '{version}' >&2\n\
                     echo \"COMPILER_PATH={r}/gcc/\" >&2\n\
                     echo \" {r}/gcc/collect2 -plugin {r}/gcc/liblto_plugin.so \\\"-plugin-opt={r}/gcc/lto-wrapper\\\" \\\"-plugin-opt=-fresolution=/tmp/ccAb12Cd.res\\\" {r}/lib/crt1.o $* -L{r}/lib {libraries}\" >&2"
                ),
            );
            // The `-lgcc_s` the line asks for.
            file(&root.join("lib/libgcc_s.a"), b"!<arch>\ngcc_s");
        }

        fn identity(&self) -> LinkEnv {
            let root = self.root.clone();
            let placeholders = move |text: &str| text.replace(root.to_str().unwrap(), "{root}");
            let rooted = |path: &Path| path.starts_with(&self.root);
            let digest = |path: &Path| fs::read(path).map(|bytes| Digest::of(&bytes));
            let driver = self.root.join("cc");
            let sysroot = self.root.join("toolchain");
            let q = Question {
                driver: &driver,
                driver_name: "cc",
                args: &self.args,
                env: &self.env,
                path: &self.path,
                cwd: &self.root,
                apple: self.apple,
                toolchain: Some((&sysroot, "host")),
                dsymutil: None,
                lto: self.lto,
                loader_env: &self.loader,
                rooted: &rooted,
                placeholders: &placeholders,
                digest: &digest,
            };
            identity(&q, &ask)
        }

        fn digest(&self) -> Digest {
            match self.identity() {
                LinkEnv::Known { digest, .. } => digest,
                LinkEnv::Unknown { reason } => panic!("unknown: {reason}"),
            }
        }

        fn unknown(&self) -> String {
            match self.identity() {
                LinkEnv::Known { items, .. } => panic!("known: {items:#?}"),
                LinkEnv::Unknown { reason } => reason,
            }
        }
    }

    fn ask(
        program: &Path,
        args: &[String],
        path: &OsStr,
        env: &[(String, Option<String>)],
    ) -> Option<Answer> {
        let mut command = std::process::Command::new(program);
        command.args(args).env("PATH", path);
        for (name, value) in env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        let out = command.output().ok()?;
        Some(Answer {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    // covers: B-37
    #[test]
    fn every_file_the_dry_run_leads_to_counts_and_its_list_says_which() {
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::new(dir.path());
        let LinkEnv::Known { digest, items } = s.identity() else {
            panic!("{:?}", s.identity());
        };
        let listed: Vec<(&str, &str)> = items
            .iter()
            .map(|item| (item.role.as_str(), item.name.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("dry-run", "cc"),
                ("driver", "cc"),
                ("program", "{root}/gcc/collect2"),
                ("ld", "ld"),
                ("plugin", "{root}/gcc/liblto_plugin.so"),
                ("file", "{root}/gcc/lto-wrapper"),
                ("file", "{root}/lib/crt1.o"),
                ("library", "-lgcc_s"),
                ("library", "-lc"),
                ("script", "libc.so.6"),
                ("toolchain", "gcc-ld/ld.lld"),
                ("toolchain", "rust-lld"),
                ("toolchain", "rust-objcopy"),
            ]
        );
        // The dry run's text holds no temporary name and no root.
        assert_eq!(s.digest(), digest, "the same setup, the same E");
        for (changed, bytes) in [
            ("lib/crt1.o", &b"crt two"[..]),
            ("lib/libc.so.6", b"\x7fELF libc two"),
            ("gcc/liblto_plugin.so", b"plugin two"),
            (
                "toolchain/lib/rustlib/host/bin/rust-objcopy",
                b"objcopy two",
            ),
            ("toolchain/lib/rustlib/host/bin/rust-lld", b"lld two"),
            (
                "toolchain/lib/rustlib/host/bin/gcc-ld/ld.lld",
                b"wrapper two",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let other = Setup::new(dir.path());
            file(&other.root.join(changed), bytes);
            assert_ne!(other.digest(), digest, "{changed} changed");
        }
        // Another `ld`, the same name.
        let dir = tempfile::tempdir().unwrap();
        let other = Setup::new(dir.path());
        script(&other.root.join("bin/ld"), &ld_script("two"));
        assert_ne!(other.digest(), digest, "ld changed");
        // A flag of the line, even one that names no file (`-C rpath`'s relative paths).
        let dir = tempfile::tempdir().unwrap();
        let mut other = Setup::new(dir.path());
        other.args.push("-Wl,-rpath,$ORIGIN/../../lib".into());
        assert_ne!(other.digest(), digest, "a flag of the line");
    }

    // covers: B-37
    #[test]
    fn where_the_files_lie_is_not_in_e_but_what_they_hold_is() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let a = Setup::new(&one.path().join("a"));
        let b = Setup::new(&two.path().join("somewhere/else"));
        assert_eq!(a.digest(), b.digest());
    }

    // covers: B-37
    #[test]
    fn an_ld_that_appears_earlier_on_path_is_another_e_never_a_remembered_one() {
        // The review's r-memo: nothing that was found is remembered, so a file that appears
        // earlier in a search answers at the next link.
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::new(dir.path());
        let first = s.digest();
        script(&s.root.join("early/ld"), &ld_script("another"));
        let second = s.digest();
        assert_ne!(first, second);
        let LinkEnv::Known { items, .. } = s.identity() else {
            panic!();
        };
        assert!(items.iter().any(|i| i.role == "ld" && i.name == "ld"));
        fs::remove_file(s.root.join("early/ld")).unwrap();
        assert_eq!(s.digest(), first);
    }

    // covers: B-37
    #[test]
    fn which_file_answered_which_library_counts() {
        let dir = tempfile::tempdir().unwrap();
        let s = Setup::new(dir.path());
        Setup::driver(&s.root, "gcc version 99.1.0 (fixture)", "-lc -lm");
        file(&s.root.join("lib/libm.a"), b"!<arch>\nm one");
        file(&s.root.join("lib/libc.so.6"), b"\x7fELF two");
        let before = s.digest();
        // The same two contents, each answering the other's name.
        file(&s.root.join("lib/libm.a"), b"\x7fELF two");
        file(&s.root.join("lib/libc.so.6"), b"!<arch>\nm one");
        assert_ne!(s.digest(), before);
    }

    // covers: B-37
    #[test]
    fn what_e_cant_tell_makes_it_unknown() {
        let new = || Setup::new(tempfile::tempdir().unwrap().keep().as_path());
        // A driver that is neither gcc nor clang by its own words.
        let s = new();
        Setup::driver(&s.root, "tcc version 0.9", "-lc");
        assert!(
            s.unknown().contains("neither gcc nor clang"),
            "{}",
            s.unknown()
        );
        // A dry run that fails or prints an error.
        let s = new();
        script(&s.root.join("cc"), "exit 1");
        assert!(s.unknown().contains("failed"));
        let s = new();
        Setup::driver(
            &s.root,
            "gcc version 9\ncc: error: unrecognized option",
            "-lc",
        );
        assert!(s.unknown().contains("error"));
        // `ld`'s sysroot is not `/` (the review's r-sysroot).
        let s = new();
        script(&s.root.join("bin/ld"), "echo /opt/sysroot");
        assert!(s.unknown().contains("sysroot"), "{}", s.unknown());
        // A script that does more than name files.
        let s = new();
        file(
            &s.root.join("lib/libc.so"),
            b"SEARCH_DIR(/x)\nINPUT ( libc.so.6 )\n",
        );
        assert!(s.unknown().contains("libc.so"), "{}", s.unknown());
        // A library the driver adds is found nowhere.
        let s = new();
        Setup::driver(&s.root, "gcc version 9", "-lc -lgone");
        assert!(s.unknown().contains("-lgone"), "{}", s.unknown());
        // The loader's variables.
        for (name, value) in [
            ("LD_PRELOAD", "/x/libhook.so".to_owned()),
            ("LD_AUDIT", "/x/libaudit.so".to_owned()),
            ("DYLD_INSERT_LIBRARIES", "/x/libhook.dylib".to_owned()),
            ("LD_LIBRARY_PATH", "/opt/elsewhere".to_owned()),
        ] {
            let mut s = new();
            s.loader = vec![(name.to_owned(), value)];
            assert!(s.unknown().contains(name), "{}", s.unknown());
        }
        // What cargo and rustup set, under roots and bases, is not one.
        let mut s = new();
        s.loader = vec![(
            "LD_LIBRARY_PATH".into(),
            format!(
                "{}/target/debug/deps:{}/toolchain/lib",
                s.root.display(),
                s.root.display()
            ),
        )];
        s.digest();
        // The link does link-time optimization itself.
        let mut s = new();
        s.lto = true;
        assert!(s.unknown().contains("link-time"));
        let mut s = new();
        s.args.push("-flto=auto".into());
        assert!(s.unknown().contains("link-time"));
        // A linker hushrun knows nothing of, on rustc's line or on the program line.
        let mut s = new();
        s.args.push("-fuse-ld=mold".into());
        assert!(s.unknown().contains("mold"));
        let s = new();
        Setup::driver(&s.root, "gcc version 9", "-lc -fuse-ld=mold");
        assert!(s.unknown().contains("mold"), "{}", s.unknown());
    }

    // covers: B-37
    #[test]
    fn the_variables_the_link_command_set_reach_the_dry_run() {
        // rustc gives the link `SDKROOT` on macOS; the dry run must see what the link saw.
        let dir = tempfile::tempdir().unwrap();
        let mut s = Setup::new(dir.path());
        let before = s.digest();
        let r = s.root.display().to_string();
        script(
            &s.root.join("cc"),
            &format!(
                "echo 'gcc version 9' >&2\necho \" {r}/gcc/collect2 -L{r}/lib -lc sdk=$SDKROOT\" >&2"
            ),
        );
        s.env = vec![("SDKROOT".into(), Some("/sdk/one".into()))];
        let one = s.digest();
        s.env = vec![("SDKROOT".into(), Some("/sdk/two".into()))];
        assert_ne!(s.digest(), one);
        assert_ne!(one, before);
    }

    // covers: B-37
    #[test]
    fn dry_run_text_is_normalized_and_commands_are_split_as_drivers_quote_them() {
        let placeholders = |text: &str| text.replace("/home/dev/ws", "{ws}");
        assert_eq!(
            normalize(
                "  /x/collect2  \"-plugin-opt=-fresolution=/tmp/ccAb12Cd.res\" -L/home/dev/ws/target \"/home/dev/ws/out/rustcAbC123/list\"\nCOLLECT_GCC_OPTIONS='-L/home/dev/ws/x'",
                &placeholders
            ),
            "/x/collect2 \"-plugin-opt=-fresolution=/tmp/ccXXXXXX.res\" -L{ws}/target \"{ws}/out/rustcXXXXXX/list\"\nCOLLECT_GCC_OPTIONS='-L{ws}/x'"
        );
        // A folder that only looks like one keeps its name.
        assert_eq!(
            normalize("/rustcAbC1234/x /ccABC.o", &placeholders),
            "/rustcAbC1234/x /ccABC.o"
        );
        assert_eq!(
            split_command(r#" /usr/bin/collect2 -plugin /p.so "-plugin-opt=a b" --as-needed"#)
                .unwrap(),
            [
                "/usr/bin/collect2",
                "-plugin",
                "/p.so",
                "-plugin-opt=a b",
                "--as-needed"
            ]
        );
        assert_eq!(
            split_command(r#" "/x/ld" "-o" "/out \"q\"" "a\\b""#).unwrap(),
            ["/x/ld", "-o", "/out \"q\"", "a\\b"]
        );
        assert!(split_command(r#" "/x/ld" "-o"#).is_err());
    }

    // covers: B-37
    #[test]
    fn the_dry_run_is_the_line_without_its_inputs_and_output() {
        let line = link::parse(
            r#""cc" "/w/out/rustcAb12Cd/symbols.o" "out/a.a.cgu.0.rcgu.o" "-Wl,--as-needed" "/t/lib/libstd-1.rlib" "-Xlinker" "/x/keep.a" "-L" "/t/lib" "-lz" "-o" "out/a" "-Wl,-rpath,$ORIGIN/x" "-nodefaultlibs""#,
        )
        .unwrap();
        assert_eq!(
            line.dry_args(),
            [
                "-Wl,--as-needed",
                "-Xlinker",
                "/x/keep.a",
                "-L",
                "/t/lib",
                "-lz",
                "-Wl,-rpath,$ORIGIN/x",
                "-nodefaultlibs"
            ]
        );
    }

    // covers: B-37
    #[cfg(target_os = "macos")]
    #[test]
    fn a_mach_o_program_names_what_it_loads() {
        // The selected Xcode's `ld`, when there is one.
        let ld = Path::new(
            "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/ld",
        );
        if !ld.exists() {
            return;
        }
        let Some(binary::Binary::MachO(found)) = binary::read(ld) else {
            panic!("not read");
        };
        assert!(
            found.dylibs.iter().any(|d| d == "@rpath/libtapi.dylib"),
            "{:?}",
            found.dylibs
        );
        assert!(
            found
                .rpaths
                .iter()
                .any(|r| r.starts_with("@executable_path"))
        );
    }

    // covers: B-37
    #[cfg(target_os = "linux")]
    #[test]
    fn an_elf_program_names_what_it_loads() {
        let Some(binary::Binary::Elf(found)) = binary::read(Path::new("/bin/sh")) else {
            panic!("not read");
        };
        assert!(
            found.needed.iter().any(|n| n.starts_with("libc.so")),
            "{:?}",
            found.needed
        );
        assert!(found.interpreter.is_some());
    }
}
