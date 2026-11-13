//! What a program was linked from (B-18): every library the linker was handed, the linker and
//! the SDK.
//!
//! With `--print=link-args` rustc writes the linker command to stdout just before it runs it,
//! and links as usual. On rustc 1.94 (`rustc_codegen_ssa/src/back/link.rs`) the line is
//! `format!("{cmd:?}\n")`, which is std's `Debug` for `Command`
//! (`library/std/src/sys/process/unix/common.rs`):
//!
//! ```text
//! [cd "DIR" && ][env -i |env -u NAME -u NAME ]NAME="VALUE" NAME="VALUE" ["PROGRAM"] "ARG0" "ARG1" ...
//! ```
//!
//! On macOS that is, cut short:
//!
//! ```text
//! env -u IPHONEOS_DEPLOYMENT_TARGET -u TVOS_DEPLOYMENT_TARGET -u XROS_DEPLOYMENT_TARGET
//! LC_ALL="C" PATH="<toolchain>/lib/rustlib/aarch64-apple-darwin/bin:..." SDKROOT="/Applications/
//! Xcode.app/.../MacOSX27.2.sdk" VSLANG="1033" ZERO_AR_DATE="1" "cc" ".../rustcXXXX/symbols.o"
//! "out/m.m.1f5e-cgu.0.rcgu.o" "-lz" "/abs/deps/libb-bb.rlib" "deps/libc-cc.rlib" ... "-lSystem"
//! "-arch" "arm64" "-L" "/usr/lib" "-o" "out/m" "-Wl,-dead_strip" "-nodefaultlibs"
//! ```
//!
//! So the line also says which variables the linker ran with: rustc puts the toolchain's own
//! `bin` folder first in `PATH`, and on macOS passes the SDK as `SDKROOT` rather than as an
//! argument. Each quoted part is Rust's `{:?}` of an `OsStr` (or of a `CString` for the program
//! and the first argument): `\"`, `\\`, `\'`, `\n`, `\r`, `\t`, `\0`, `\u{…}`, and `\xNN` for a
//! byte that isn't UTF-8. Paths are as rustc passed them, so relative ones are relative to
//! rustc's working directory. The objects include rustc's own temporary files (`rustcXXXX/`) and
//! the compile's own `.rcgu.o` outputs, which are gone after the link.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The linker command rustc printed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkArgs {
    /// The folder the command ran in, when it was set (`cd "DIR" &&`). rustc doesn't set one.
    pub cwd: Option<String>,
    /// Whether the command started from an empty environment (`env -i`).
    pub env_cleared: bool,
    /// The variables the command changed, as printed: each removed one (`None`) and then each
    /// set one. Every other variable was inherited from rustc.
    pub env: Vec<(String, Option<String>)>,
    /// The linker program as rustc named it: `cc`, a path, or the value of `-C linker`.
    pub linker: String,
    /// The arguments after the program, in order.
    pub args: Vec<String>,
}

/// What a link read, by kind. Each list keeps the command's order, each entry once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inputs {
    /// Libraries given by path: `.rlib`, `.a`, `.lib`, `.dylib`, `.so` (also `.so.1.2`), `.tbd`.
    pub libraries: Vec<PathBuf>,
    /// Objects given by path: `.o`, `.obj`. These include rustc's temporary files and the
    /// compile's own outputs, which the wrapper doesn't hash.
    pub objects: Vec<PathBuf>,
    /// Library search folders: `-L x`, `-Lx`, `--library-path`.
    pub search: Vec<PathBuf>,
    /// Framework search folders: `-F x`, `-Fx`.
    pub framework_search: Vec<PathBuf>,
    /// Libraries given by name (`-lz`, `-l z`, `-l:libz.a`, `--library=z` give `z` or `:libz.a`)
    /// and frameworks (`-framework Security` and its weak, needed and reexport forms give
    /// `Security.framework`).
    pub named: Vec<String>,
    /// The SDK or sysroot: the last of `-isysroot`, `-syslibroot` and `--sysroot`, in any form,
    /// or else the `SDKROOT` the command set.
    pub sdk: Option<PathBuf>,
    /// Response files (`@file`). Their contents are more arguments that this list doesn't see,
    /// so a link that has one can't be keyed (B-18).
    pub response_files: Vec<PathBuf>,
    /// Files options name for the linker to read: linker scripts (`-T`, `--script`), symbol
    /// lists and the like, in order, as given.
    pub files: Vec<FileArg>,
    /// The linker the C driver runs instead of its own, from the last `-fuse-ld=`.
    pub fuse_ld: Option<String>,
    /// The C driver's program folders (`-B x`, `-Bx`), searched first for the linker.
    pub prefixes: Vec<PathBuf>,
    /// Every other argument that may name a file (anything not an option, and not the value of
    /// one that names an output), joined to the working directory: the caller keys each that is
    /// an existing regular file it didn't key already (B-18).
    pub paths: Vec<PathBuf>,
}

/// A file an option names for the linker to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileArg {
    /// The value as given.
    pub value: String,
    /// A linker script (`-T`, `--script`): looked for in the working directory and then in each
    /// `-L` folder, and it may name more files.
    pub script: bool,
}

/// Options whose value is a file the linker reads, besides libraries and objects: GNU ld's and
/// lld's scripts and lists, and ld64's lists.
const FILE_OPTIONS: &[(&str, bool)] = &[
    ("-T", true),
    ("--script", true),
    ("--version-script", false),
    ("--dynamic-list", false),
    ("--retain-symbols-file", false),
    ("--just-symbols", false),
    ("-exported_symbols_list", false),
    ("-unexported_symbols_list", false),
    ("-reexported_symbols_list", false),
    ("-order_file", false),
    ("-filelist", false),
    ("-alias_list", false),
    ("-bundle_loader", false),
];

/// Options whose third value is a file whose bytes go into the output: ld64's
/// `-sectcreate segment section file`.
const SECTION_FILE_OPTIONS: &[&str] = &["-sectcreate", "-segcreate"];

/// The linker program as the system found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linker {
    /// Where it was found: the path given, joined to the working directory when relative, or the
    /// first `PATH` entry that holds an executable file by that name.
    pub path: PathBuf,
    /// Its size, following symlinks.
    pub size: u64,
    /// Its modification time in nanoseconds since the epoch, following symlinks.
    pub modified_ns: i64,
}

/// Why rustc's stdout gave no linker command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// No line of stdout reads as a linker command.
    #[error("no linker command in rustc's output")]
    NoCommand,
    /// More than one line reads as a linker command: taking one would be a guess.
    #[error("{0} lines of rustc's output read as a linker command")]
    Several(usize),
    /// The command names a path or value that isn't UTF-8.
    #[error("the linker command holds a value that is not UTF-8")]
    NotUtf8,
}

/// Why one line isn't a linker command.
#[derive(Debug, PartialEq, Eq)]
enum LineError {
    Malformed,
    NotUtf8,
}

/// Reads the linker command from rustc's stdout. Exactly one line must read as one; other lines
/// (a proc macro's own printing, say) are skipped, and none or several is an error.
pub fn parse(stdout: &str) -> Result<LinkArgs, Error> {
    let mut found = Vec::new();
    let mut not_utf8 = false;
    for line in stdout.lines() {
        match parse_line(line) {
            Ok(args) => found.push(args),
            Err(LineError::NotUtf8) => not_utf8 = true,
            Err(LineError::Malformed) => {}
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 if not_utf8 => Err(Error::NotUtf8),
        0 => Err(Error::NoCommand),
        n => Err(Error::Several(n)),
    }
}

/// rustc's stdout as it goes on to the wrapper's stdout: every line, byte for byte, but the
/// linker command `--print=link-args` added, which is the last line that reads as one (rustc
/// links after every macro ran). A proc macro's own printing gets through (B-18).
pub fn without_command(stdout: &[u8]) -> Vec<u8> {
    let lines: Vec<&[u8]> = stdout.split_inclusive(|&b| b == b'\n').collect();
    let command = lines.iter().rposition(|line| {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        std::str::from_utf8(line).is_ok_and(|text| parse_line(text).is_ok())
    });
    let mut out = Vec::with_capacity(stdout.len());
    for (i, line) in lines.iter().enumerate() {
        if Some(i) != command {
            out.extend_from_slice(line);
        }
    }
    out
}

/// A cursor over one line.
struct Line<'a> {
    rest: &'a str,
}

impl<'a> Line<'a> {
    fn eat(&mut self, prefix: &str) -> bool {
        match self.rest.strip_prefix(prefix) {
            Some(rest) => {
                self.rest = rest;
                true
            }
            None => false,
        }
    }

    fn peek(&self) -> Option<char> {
        self.rest.chars().next()
    }

    /// A bare word up to `stop` (exclusive), not empty and without spaces or quotes.
    fn word(&mut self, stop: char) -> Result<&'a str, LineError> {
        let end = self.rest.find(stop).ok_or(LineError::Malformed)?;
        let word = &self.rest[..end];
        if word.is_empty() || word.contains([' ', '"', '[']) {
            return Err(LineError::Malformed);
        }
        self.rest = &self.rest[end + stop.len_utf8()..];
        Ok(word)
    }

    /// One `{:?}`-quoted value.
    fn quoted(&mut self) -> Result<String, LineError> {
        if !self.eat("\"") {
            return Err(LineError::Malformed);
        }
        let mut bytes = Vec::new();
        let mut chars = self.rest.char_indices();
        loop {
            let (i, c) = chars.next().ok_or(LineError::Malformed)?;
            match c {
                '"' => {
                    self.rest = &self.rest[i + 1..];
                    break;
                }
                '\\' => {
                    let (_, e) = chars.next().ok_or(LineError::Malformed)?;
                    match e {
                        '"' | '\\' | '\'' => push_char(&mut bytes, e),
                        'n' => bytes.push(b'\n'),
                        'r' => bytes.push(b'\r'),
                        't' => bytes.push(b'\t'),
                        '0' => bytes.push(0),
                        'x' => {
                            let hi = chars.next().ok_or(LineError::Malformed)?.1;
                            let lo = chars.next().ok_or(LineError::Malformed)?.1;
                            let hex: String = [hi, lo].iter().collect();
                            let byte =
                                u8::from_str_radix(&hex, 16).map_err(|_| LineError::Malformed)?;
                            bytes.push(byte);
                        }
                        'u' => {
                            if chars.next().map(|(_, c)| c) != Some('{') {
                                return Err(LineError::Malformed);
                            }
                            let mut hex = String::new();
                            loop {
                                let (_, h) = chars.next().ok_or(LineError::Malformed)?;
                                if h == '}' {
                                    break;
                                }
                                if hex.len() >= 6 {
                                    return Err(LineError::Malformed);
                                }
                                hex.push(h);
                            }
                            let code =
                                u32::from_str_radix(&hex, 16).map_err(|_| LineError::Malformed)?;
                            let c = char::from_u32(code).ok_or(LineError::Malformed)?;
                            push_char(&mut bytes, c);
                        }
                        _ => return Err(LineError::Malformed),
                    }
                }
                _ => push_char(&mut bytes, c),
            }
        }
        String::from_utf8(bytes).map_err(|_| LineError::NotUtf8)
    }
}

fn push_char(bytes: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// Reads one line as std's `Debug` of a `Command`.
fn parse_line(text: &str) -> Result<LinkArgs, LineError> {
    let mut line = Line { rest: text };
    let mut out = LinkArgs::default();
    if line.eat("cd ") {
        out.cwd = Some(line.quoted()?);
        if !line.eat(" && ") {
            return Err(LineError::Malformed);
        }
    }
    if line.eat("env -i ") {
        out.env_cleared = true;
    } else if line.eat("env ") {
        let mut any = false;
        while line.eat("-u ") {
            out.env.push((line.word(' ')?.to_owned(), None));
            any = true;
        }
        if !any {
            return Err(LineError::Malformed);
        }
    }
    while !matches!(line.peek(), Some('"' | '[') | None) {
        let name = line.word('=')?.to_owned();
        let value = line.quoted()?;
        if !line.eat(" ") {
            return Err(LineError::Malformed);
        }
        out.env.push((name, Some(value)));
    }
    let program = if line.eat("[") {
        let program = line.quoted()?;
        if !line.eat("] ") {
            return Err(LineError::Malformed);
        }
        Some(program)
    } else {
        None
    };
    let first = line.quoted()?;
    while line.eat(" ") {
        out.args.push(line.quoted()?);
    }
    if !line.rest.is_empty() {
        return Err(LineError::Malformed);
    }
    out.linker = program.unwrap_or(first);
    Ok(out)
}

/// Options whose next argument is an output or a name, never an input, even when it ends like a
/// library (`-o libfoo.dylib`, `-soname libfoo.so.1`).
const VALUE_NOT_INPUT: &[&str] = &[
    "-o",
    "-rpath",
    "-install_name",
    "-dylib_install_name",
    "-soname",
    "-object_path_lto",
    "-dependency_info",
    "-Map",
    "--out-implib",
];

const FRAMEWORK_FLAGS: &[&str] = &[
    "-framework",
    "-weak_framework",
    "-needed_framework",
    "-reexport_framework",
];

const SYSROOT_FLAGS: &[&str] = &["-isysroot", "-syslibroot", "--sysroot"];

impl LinkArgs {
    /// The value the command set for `name`, if it set one. `None` also when the variable was
    /// inherited from rustc unchanged.
    pub fn set_var(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// The inputs the arguments name, with relative paths joined to `cwd` (rustc's working
    /// directory). `-Wl,a,b` and `-Xlinker a` are read as the linker's own arguments.
    pub fn inputs(&self, cwd: &Path) -> Inputs {
        let tokens = flatten(&self.args);
        let mut inputs = Inputs::default();
        let mut sdk: Option<&str> = None;
        let mut i = 0;
        while i < tokens.len() {
            let token = tokens[i];
            let next = tokens.get(i + 1).copied();
            i += 1;
            if VALUE_NOT_INPUT.contains(&token) {
                i += 1;
                continue;
            }
            if let Some(value) = take_value(token, next, &["-L"], &["--library-path"]) {
                let (value, used_next) = value;
                i += usize::from(used_next);
                push_once(&mut inputs.search, cwd.join(value));
                continue;
            }
            if let Some((value, used_next)) = take_value(token, next, &["-F"], &[]) {
                i += usize::from(used_next);
                push_once(&mut inputs.framework_search, cwd.join(value));
                continue;
            }
            if let Some(value) = token.strip_prefix("-fuse-ld=") {
                inputs.fuse_ld = Some(value.to_owned());
                continue;
            }
            if let Some((value, used_next, script)) = take_file(token, next) {
                i += usize::from(used_next);
                inputs.files.push(FileArg {
                    value: value.to_owned(),
                    script,
                });
                continue;
            }
            // ld64's `-dylib_file install_name:file`: the file after the colon.
            if token == "-dylib_file" {
                if let Some((_, file)) = next.and_then(|value| value.split_once(':')) {
                    inputs.files.push(FileArg {
                        value: file.to_owned(),
                        script: false,
                    });
                }
                i += 1;
                continue;
            }
            if SECTION_FILE_OPTIONS.contains(&token) {
                if let Some(file) = tokens.get(i + 2) {
                    inputs.files.push(FileArg {
                        value: (*file).to_owned(),
                        script: false,
                    });
                }
                i += 3;
                continue;
            }
            if let Some((value, used_next)) = take_value(token, next, &["-l"], &["--library"]) {
                i += usize::from(used_next);
                push_once(&mut inputs.named, value.to_owned());
                continue;
            }
            if FRAMEWORK_FLAGS.contains(&token) {
                if let Some(name) = next {
                    i += 1;
                    push_once(&mut inputs.named, format!("{name}.framework"));
                }
                continue;
            }
            if let Some((value, used_next)) = take_sysroot(token, next) {
                i += usize::from(used_next);
                sdk = Some(value);
                continue;
            }
            if let Some(file) = token.strip_prefix('@') {
                push_once(&mut inputs.response_files, cwd.join(file));
                continue;
            }
            if token.starts_with('-') || token.is_empty() {
                continue;
            }
            push_once(&mut inputs.paths, cwd.join(token));
            match kind_of(token) {
                Some(Kind::Library) => push_once(&mut inputs.libraries, cwd.join(token)),
                Some(Kind::Object) => push_once(&mut inputs.objects, cwd.join(token)),
                None => {}
            }
        }
        inputs.sdk = sdk.or_else(|| self.set_var("SDKROOT")).map(|s| cwd.join(s));
        inputs.prefixes = self.prefixes(cwd);
        inputs
    }

    /// The C driver's own `-B` folders, from its arguments (never from `-Wl,`, where `-Bstatic`
    /// and `-Bdynamic` are the linker's).
    fn prefixes(&self, cwd: &Path) -> Vec<PathBuf> {
        prefixes(&self.args, cwd)
    }

    /// Whether the line hands the C driver a file of options this list can't read (B-18): a
    /// gcc specs file (`-specs=`, `--specs=`, `--specs`) or a clang configuration file
    /// (`--config`, and the folders it is looked for in). Either can change which `ld` runs and
    /// what it is handed.
    pub fn driver_reads_options(&self) -> bool {
        self.args.iter().any(|arg| {
            ["-specs", "--specs", "--config"]
                .iter()
                .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
                || arg.starts_with("--config-system-dir")
                || arg.starts_with("--config-user-dir")
        })
    }

    /// The C driver's options that change which `ld` it runs or where it looks for a library,
    /// in order (B-18): `-B`, `-fuse-ld=`, `--ld-path=`, `--sysroot`, the target and the
    /// multilib options (`-m*`), the GCC installation clang takes, and the kind of output
    /// (`-shared`, `-static`, `-pie`, …). They are handed to every question asked of the driver,
    /// so its answers are the link's. `-Wl,` and `-Xlinker` values are the linker's, never here.
    pub fn driver_flags(&self) -> Vec<String> {
        const ALONE: &[&str] = &[
            "-shared",
            "-static",
            "-static-pie",
            "-pie",
            "-no-pie",
            "-nostdlib",
            "-nodefaultlibs",
            "-nostartfiles",
        ];
        const JOINED: &[&str] = &[
            "-fuse-ld=",
            "--ld-path=",
            "--sysroot=",
            "--target=",
            "--gcc-toolchain=",
            "--gcc-install-dir=",
            "-rtlib=",
            "--rtlib=",
            "-stdlib=",
        ];
        const WITH_VALUE: &[&str] = &["-B", "--sysroot", "-target"];
        let mut out = Vec::new();
        // rustc runs `rust-lld` as the linker itself, no C driver, with `-flavor` first.
        if let [flag, flavor, ..] = self.args.as_slice()
            && flag == "-flavor"
        {
            return vec![flag.clone(), flavor.clone()];
        }
        let mut iter = self.args.iter();
        while let Some(arg) = iter.next() {
            if arg == "-Xlinker" {
                iter.next();
                continue;
            }
            if WITH_VALUE.contains(&arg.as_str()) {
                if let Some(value) = iter.next() {
                    out.push(arg.clone());
                    out.push(value.clone());
                }
                continue;
            }
            if ALONE.contains(&arg.as_str())
                || JOINED.iter().any(|flag| arg.starts_with(flag))
                || joined_prefix(arg).is_some()
                || (arg.starts_with("-m") && arg.len() > 2)
            {
                out.push(arg.clone());
            }
        }
        out
    }

    /// The sysroot the line gives `ld`: the last `--sysroot`, whether the driver's (which passes
    /// it on) or one of `-Wl,` (B-6).
    pub fn ld_sysroot(&self) -> Option<String> {
        let tokens = flatten(&self.args);
        let mut found = None;
        let mut i = 0;
        while i < tokens.len() {
            if tokens[i] == "--sysroot" {
                found = tokens.get(i + 1).map(|v| (*v).to_owned());
                i += 1;
            } else if let Some(value) = tokens[i].strip_prefix("--sysroot=") {
                found = Some(value.to_owned());
            }
            i += 1;
        }
        found
    }
}

/// What follows `-B` in a C driver's `-B<folder>`: the folder, or nothing when it is the next
/// argument. `None` for any other argument, the linker's `-Bstatic`, `-Bdynamic` and
/// `-Bsymbolic` among them.
fn joined_prefix(arg: &str) -> Option<&str> {
    arg.strip_prefix("-B").filter(|rest| {
        !rest.starts_with("static") && !rest.starts_with("dynamic") && !rest.starts_with("symbolic")
    })
}

/// A driver flag ([`LinkArgs::driver_flags`]) with the paths it holds rewritten by `path`, which
/// replaces a root where one starts a path (B-8). The folder of a joined `-B<folder>` is handed
/// over alone: after the letter it would not start a path, and rustc names its own linker that
/// way (`-B<sysroot>/lib/rustlib/<target>/bin/gcc-ld`), so the toolchain's folder would stay in
/// what a hint keeps as this machine wrote it.
pub fn flag_with_paths(flag: &str, path: impl Fn(&str) -> String) -> String {
    match joined_prefix(flag) {
        Some(folder) if !folder.is_empty() => format!("-B{}", path(folder)),
        _ => path(flag),
    }
}

/// The C driver's `-B` folders among `args`, joined to `cwd`, each once.
fn prefixes(args: &[String], cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let value = match joined_prefix(arg) {
            Some("") => iter.next().map(String::as_str),
            joined => joined,
        };
        if let Some(value) = value {
            push_once(&mut out, cwd.join(value));
        }
    }
    out
}

/// The arguments as the linker sees them: `-Wl,a,b` gives `a` and `b`, `-Xlinker a` gives `a`.
fn flatten(args: &[String]) -> Vec<&str> {
    let mut tokens = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(rest) = arg.strip_prefix("-Wl,") {
            tokens.extend(rest.split(','));
        } else if arg == "-Xlinker" {
            if let Some(value) = iter.next() {
                tokens.push(value.as_str());
            }
        } else {
            tokens.push(arg.as_str());
        }
    }
    tokens
}

/// The value of a short option (`-Lx` or `-L x`) or a long one (`--opt=x` or `--opt x`), and
/// whether it was the next token.
fn take_value<'a>(
    token: &'a str,
    next: Option<&'a str>,
    short: &[&str],
    long: &[&str],
) -> Option<(&'a str, bool)> {
    for flag in short {
        if token == *flag {
            return next.map(|v| (v, true));
        }
        if let Some(value) = token.strip_prefix(flag)
            && !value.is_empty()
        {
            return Some((value, false));
        }
    }
    for flag in long {
        if token == *flag {
            return next.map(|v| (v, true));
        }
        if let Some(value) = token.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            return Some((value, false));
        }
    }
    None
}

/// A file option's value (`-T x`, `-Tx`, `--script=x`, `--version-script x`, …), whether it
/// was the next token, and whether it is a linker script. `-Tbss`, `-Tdata` and `-Ttext` take an
/// address, not a file.
fn take_file<'a>(token: &'a str, next: Option<&'a str>) -> Option<(&'a str, bool, bool)> {
    for (flag, script) in FILE_OPTIONS {
        if token == *flag {
            return next.map(|v| (v, true, *script));
        }
        let Some(rest) = token.strip_prefix(flag) else {
            continue;
        };
        if let Some(value) = rest.strip_prefix('=') {
            return Some((value, false, *script));
        }
        if *flag == "-T" && !rest.is_empty() {
            if matches!(
                rest,
                "bss" | "data" | "text" | "text-segment" | "rodata-segment"
            ) || rest.starts_with("ext=")
            {
                return None;
            }
            return Some((rest, false, true));
        }
    }
    None
}

/// `-isysroot X`, `-isysrootX`, `-isysroot=X`, `-syslibroot X`, `--sysroot X`, `--sysroot=X`.
fn take_sysroot<'a>(token: &'a str, next: Option<&'a str>) -> Option<(&'a str, bool)> {
    for flag in SYSROOT_FLAGS {
        if token == *flag {
            return next.map(|v| (v, true));
        }
        if let Some(value) = token.strip_prefix(flag) {
            let value = value.strip_prefix('=').unwrap_or(value);
            // `--sysroot` takes no joined form without `=`; clang's `-isysroot` does.
            if !value.is_empty() && (*flag == "-isysroot" || token.contains('=')) {
                return Some((value, false));
            }
        }
    }
    None
}

enum Kind {
    Library,
    Object,
}

fn kind_of(token: &str) -> Option<Kind> {
    let name = token.rsplit(['/', '\\']).next().unwrap_or(token);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    match ext {
        "rlib" | "a" | "lib" | "dylib" | "so" | "tbd" => Some(Kind::Library),
        "o" | "obj" => Some(Kind::Object),
        _ if is_versioned_so(name) => Some(Kind::Library),
        _ => None,
    }
}

/// `libfoo.so.1` or `libfoo.so.1.2.3`.
fn is_versioned_so(name: &str) -> bool {
    match name.split_once(".so.") {
        Some((stem, version)) => {
            !stem.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        }
        None => false,
    }
}

fn push_once<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// Finds the linker the way the system would run it: a program with a `/` is a path (joined to
/// `cwd` when relative); a bare name is looked up in each entry of `path` in order (an empty or
/// relative entry is under `cwd`), taking the first executable file. `None` when nothing is
/// found or it can't be read.
pub fn linker(program: &str, path: Option<&OsStr>, cwd: &Path) -> Option<Linker> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') || program.contains(std::path::MAIN_SEPARATOR) {
        return stat(cwd.join(program), false);
    }
    std::env::split_paths(path?).find_map(|dir| stat(cwd.join(dir).join(program), true))
}

/// The link command's `PATH` in two parts (B-18): the entries rustc put before the one it
/// inherited (`inherited`, the wrapper's own), and whether the rest is that one. rustc puts the
/// toolchain's own `bin` first; with no `PATH` printed, the command inherited it whole.
pub fn path_parts(printed: Option<&str>, inherited: Option<&OsStr>) -> (Vec<PathBuf>, bool) {
    let Some(printed) = printed else {
        return (Vec::new(), true);
    };
    let entries: Vec<PathBuf> = std::env::split_paths(printed).collect();
    let inherited: Vec<PathBuf> = inherited
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    if inherited.is_empty() {
        return (entries, true);
    }
    match entries.strip_suffix(inherited.as_slice()) {
        Some(before) => (before.to_vec(), true),
        None => (entries, false),
    }
}

/// The link command's `PATH` from its two parts ([`path_parts`]) and the `PATH` it inherits now;
/// `None` when it has no entry.
pub fn link_path(
    before: &[PathBuf],
    inherits: bool,
    inherited: Option<&OsStr>,
) -> Option<OsString> {
    let mut entries = before.to_vec();
    if inherits && let Some(path) = inherited {
        entries.extend(std::env::split_paths(path));
    }
    if entries.is_empty() {
        return None;
    }
    std::env::join_paths(entries).ok()
}

/// What a program the wrapper asked answered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    /// Whether it exited with success.
    pub success: bool,
    /// What it wrote on stdout.
    pub stdout: String,
}

/// Asks a program (the C driver, `ld`) one question: runs it with these arguments, the link's
/// `PATH` and `LC_ALL=C`, nothing on stdin, and the wrapper's other variables. `None` when it
/// can't be started.
pub type Ask<'a> = dyn Fn(&Path, &[String], &OsStr) -> Option<Answer> + 'a;

/// What decides which `ld` a C driver runs and where that `ld` looks for a library named with
/// `-l` (B-6, B-18): asked again at every link and before every lookup, never remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The C driver, as found.
    pub driver: PathBuf,
    /// Its options that change the answers ([`LinkArgs::driver_flags`]).
    pub flags: Vec<String>,
    /// The sysroot the line gives `ld` ([`LinkArgs::ld_sysroot`]).
    pub ld_sysroot: Option<String>,
    /// The link command's `PATH`.
    pub path: OsString,
    /// rustc's working directory.
    pub cwd: PathBuf,
    /// The target is Apple's: its linker is Apple's `ld`, which searches the SDK.
    pub apple: bool,
    /// The SDK the link names, if any.
    pub sdk: Option<PathBuf>,
    /// The `LIBRARY_PATH` the driver reads: gcc and clang hand `ld` each of its folders.
    pub library_path: Option<OsString>,
}

/// Which `ld` a link runs and where it looks for libraries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    /// The `ld` the driver runs, as found.
    pub ld: PathBuf,
    /// The folders the driver hands `ld` and those `ld` searches by itself, after rustc's own,
    /// in order, each once, resolved on disk: where a `-l` found in none of rustc's folders is.
    pub folders: Vec<PathBuf>,
    /// The framework folders Apple's `ld` searches after `-F` ones.
    pub frameworks: Vec<PathBuf>,
    /// The sysroot `ld` resolves a linker script's absolute names under, when it is not `/`.
    pub sysroot: Option<String>,
}

/// Why a link's environment can't be told: the link is `built, not stored` (B-18).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unknown {
    /// The driver couldn't be asked, or its answer can't be read.
    #[error("the C driver can't be asked which ld it runs")]
    Driver,
    /// The `ld` it runs is found nowhere.
    #[error("the ld the C driver runs is found nowhere")]
    NoLd,
}

/// How the driver picks its `ld`.
enum Choice {
    /// A path: `--ld-path=` (clang's, which wins), or a `-fuse-ld=` that holds a `/`.
    Path(String),
    /// A name it looks for: `ld`, or `ld.<name>` for `-fuse-ld=<name>`.
    Named(String),
    /// No C driver: rustc runs `rust-lld` itself (`-flavor` first), which is the `ld`.
    Itself,
}

fn choice(flags: &[String]) -> Choice {
    if flags.first().is_some_and(|flag| flag == "-flavor") {
        return Choice::Itself;
    }
    let mut fuse = None;
    let mut ld_path = None;
    for flag in flags {
        if let Some(value) = flag.strip_prefix("--ld-path=") {
            ld_path = Some(value.to_owned());
        } else if let Some(value) = flag.strip_prefix("-fuse-ld=") {
            fuse = Some(value.to_owned());
        }
    }
    match (ld_path, fuse) {
        (Some(path), _) => Choice::Path(path),
        (None, Some(value)) if value.contains('/') => Choice::Path(value),
        (None, Some(value)) if !value.is_empty() => Choice::Named(format!("ld.{value}")),
        _ => Choice::Named("ld".to_owned()),
    }
}

/// gcc's and clang's `-print-search-dirs`: the program folders `collect2` looks for `ld` in
/// before `PATH`, and the library folders the driver hands the linker.
#[derive(Debug, Clone, Default)]
struct SearchDirs {
    programs: Vec<String>,
    libraries: Vec<String>,
}

fn search_dirs(query: &Query, ask: &Ask<'_>) -> Result<SearchDirs, Unknown> {
    let mut args = query.flags.clone();
    args.push("-print-search-dirs".to_owned());
    let answer = ask(&query.driver, &args, &query.path).ok_or(Unknown::Driver)?;
    if !answer.success {
        return Err(Unknown::Driver);
    }
    let mut dirs = SearchDirs::default();
    let list = |rest: &str| -> Vec<String> {
        let rest = rest.strip_prefix('=').unwrap_or(rest);
        rest.split(':')
            .filter(|dir| !dir.is_empty())
            .map(str::to_owned)
            .collect()
    };
    for line in answer.stdout.lines() {
        if let Some(rest) = line.strip_prefix("programs: ") {
            dirs.programs = list(rest);
        } else if let Some(rest) = line.strip_prefix("libraries: ") {
            dirs.libraries = list(rest);
        }
    }
    Ok(dirs)
}

/// Which `ld` the driver runs (B-18). `--ld-path=` (clang's) or a `-fuse-ld=` path is it.
/// Otherwise the driver is asked `-print-prog-name=ld` (`ld.<name>` for `-fuse-ld=<name>`: gcc
/// asked with `-fuse-ld=lld -print-prog-name=ld` still says `ld`). A path in the answer is it
/// (clang searches `PATH` itself; Apple's driver names the selected Xcode's `ld`). A bare name
/// is looked up as `collect2` does: `real-ld`, `collect-ld`, then the name in the `-B` folders
/// and the driver's program folders, then the name on the link's `PATH`.
fn which_ld(
    query: &Query,
    ask: &Ask<'_>,
    dirs: &mut Option<SearchDirs>,
) -> Result<PathBuf, Unknown> {
    let name = match choice(&query.flags) {
        Choice::Itself => return Ok(query.driver.clone()),
        Choice::Path(path) => {
            let path = query.cwd.join(path);
            return match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => Ok(path),
                _ => Err(Unknown::NoLd),
            };
        }
        Choice::Named(name) => name,
    };
    let mut args = query.flags.clone();
    args.push(format!("-print-prog-name={name}"));
    let answer = ask(&query.driver, &args, &query.path).ok_or(Unknown::Driver)?;
    let said = answer
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if !answer.success || said.is_empty() {
        return Err(Unknown::Driver);
    }
    if said.contains('/') {
        let path = query.cwd.join(said);
        return match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => Ok(path),
            _ => Err(Unknown::NoLd),
        };
    }
    if dirs.is_none() {
        *dirs = Some(search_dirs(query, ask)?);
    }
    let programs = dirs
        .as_ref()
        .map(|d| d.programs.clone())
        .unwrap_or_default();
    let mut compiler_path = prefixes(&query.flags, &query.cwd);
    for dir in programs {
        push_once(&mut compiler_path, query.cwd.join(dir));
    }
    for program in ["real-ld", "collect-ld", said] {
        if let Some(found) = compiler_path
            .iter()
            .find_map(|dir| stat(dir.join(program), true))
        {
            return Ok(found.path);
        }
    }
    linker(said, Some(&query.path), &query.cwd)
        .map(|found| found.path)
        .ok_or(Unknown::NoLd)
}

/// Which `ld` a link runs and every folder it may take a `-l` from after rustc's own (B-6,
/// B-18). First `LIBRARY_PATH`'s folders, which gcc and clang hand `ld` as `-L`. Then, on
/// Apple's targets: the SDK's `usr/lib` and `usr/local/lib`, and `/usr/local/lib` (Apple's `ld`
/// takes a folder under the SDK when there is one, else the folder itself), and the SDK's
/// framework folders. Elsewhere: the driver's library folders (`-print-search-dirs`, which for
/// clang are the other `-L` folders it hands `ld`), then `ld`'s own
/// `SEARCH_DIR`s (`ld --verbose`; none where `ld` has no such option, as `lld`), those that
/// start with `=` under `ld --print-sysroot`. Cost: one question on Apple's targets, four
/// elsewhere (a gcc driver answers in under a millisecond each, clang in about 15).
pub fn environment(query: &Query, ask: &Ask<'_>) -> Result<Environment, Unknown> {
    let mut dirs = None;
    let ld = which_ld(query, ask, &mut dirs)?;
    let mut folders = Vec::new();
    let mut frameworks = Vec::new();
    let mut sysroot = None;
    // `LIBRARY_PATH`'s folders, which both drivers hand `ld` as `-L` (gcc lists them among its
    // library folders too; clang doesn't).
    let library_path: Vec<PathBuf> = query
        .library_path
        .as_deref()
        .map(|path| {
            std::env::split_paths(path)
                .map(|dir| query.cwd.join(dir))
                .collect()
        })
        .unwrap_or_default();
    if query.apple {
        let root = query.sdk.as_deref().map(|sdk| query.cwd.join(sdk));
        let under = |dir: &str| root.as_ref().map(|root| root.join(dir));
        let push = |list: &mut Vec<PathBuf>, path: PathBuf| push_once(list, resolve_folder(&path));
        // Apple's `ld` takes each `-L` folder under the SDK when there is one there.
        for dir in &library_path {
            if let (Some(root), Ok(rest)) = (&root, dir.strip_prefix("/")) {
                push(&mut folders, root.join(rest));
            }
            push(&mut folders, dir.clone());
        }
        match &root {
            Some(_) => {
                for dir in ["usr/lib", "usr/local/lib"] {
                    push(&mut folders, under(dir).unwrap_or_default());
                }
                push(&mut folders, PathBuf::from("/usr/local/lib"));
                for dir in ["Library/Frameworks", "System/Library/Frameworks"] {
                    push(&mut frameworks, under(dir).unwrap_or_default());
                }
                push(&mut frameworks, PathBuf::from("/Library/Frameworks"));
            }
            None => {
                for dir in ["/usr/lib", "/usr/local/lib"] {
                    push(&mut folders, PathBuf::from(dir));
                }
                for dir in ["/Library/Frameworks", "/System/Library/Frameworks"] {
                    push(&mut frameworks, PathBuf::from(dir));
                }
            }
        }
    } else if matches!(choice(&query.flags), Choice::Itself) {
        // `rust-lld` adds no folder of its own, and reads no `LIBRARY_PATH`.
        folders.clear();
    } else {
        if dirs.is_none() {
            dirs = Some(search_dirs(query, ask)?);
        }
        for dir in &library_path {
            push_once(&mut folders, resolve_folder(dir));
        }
        for dir in dirs.map(|d| d.libraries).unwrap_or_default() {
            push_once(&mut folders, resolve_folder(&query.cwd.join(dir)));
        }
        let mut args = Vec::new();
        if let Some(root) = &query.ld_sysroot {
            args.push(format!("--sysroot={root}"));
        }
        args.push("--print-sysroot".to_owned());
        let said = match ask(&ld, &args, &query.path) {
            Some(answer) if answer.success => answer.stdout.trim().to_owned(),
            // No such option (`lld`): the line's own, if any.
            _ => query.ld_sysroot.clone().unwrap_or_default(),
        };
        sysroot = (!said.is_empty() && Path::new(&said) != Path::new("/")).then_some(said);
        if let Some(answer) = ask(&ld, &["--verbose".to_owned()], &query.path)
            && answer.success
        {
            for dir in search_dir_entries(&answer.stdout) {
                let dir = match dir.strip_prefix('=') {
                    Some(rest) => match &sysroot {
                        Some(root) => format!(
                            "{}/{}",
                            root.trim_end_matches('/'),
                            rest.trim_start_matches('/')
                        ),
                        None => rest.to_owned(),
                    },
                    None => dir,
                };
                push_once(&mut folders, resolve_folder(&query.cwd.join(dir)));
            }
        }
    }
    Ok(Environment {
        ld,
        folders,
        frameworks,
        sysroot,
    })
}

/// The folders of `SEARCH_DIR("…")` in `ld --verbose`'s default script, in order.
fn search_dir_entries(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("SEARCH_DIR(") {
        rest = &rest[at + "SEARCH_DIR(".len()..];
        let Some(end) = rest.find(')') else {
            break;
        };
        let dir = rest[..end].trim().trim_matches('"');
        if !dir.is_empty() {
            out.push(dir.to_owned());
        }
        rest = &rest[end..];
    }
    out
}

/// A folder as the kernel finds it: the longest part of it that exists, resolved on disk, and
/// the rest after it with `.` and `..` taken away by name (a part that doesn't exist is no link).
/// A driver names its folders with `..` (`/usr/lib/gcc/<target>/14/../../../../lib/`), and two
/// of its names are often one folder.
pub fn resolve_folder(path: &Path) -> PathBuf {
    use std::path::Component;
    let parts: Vec<Component<'_>> = path.components().collect();
    for end in (1..=parts.len()).rev() {
        let head: PathBuf = parts[..end].iter().collect();
        if let Ok(mut out) = std::fs::canonicalize(&head) {
            for part in &parts[end..] {
                match part {
                    Component::ParentDir => {
                        out.pop();
                    }
                    Component::CurDir => {}
                    other => out.push(other),
                }
            }
            return out;
        }
    }
    path.to_path_buf()
}

/// The C runtime the standard library asks the linker for (`-lc`, `-lm`, …, `-lSystem`): where
/// it is found in the driver's and `ld`'s folders is keyed, and left out of what a dependent
/// names a proc macro by, like the linker and the SDK (B-6).
pub const C_RUNTIME: &[&str] = &[
    "c", "m", "rt", "pthread", "dl", "util", "gcc_s", "gcc", "gcc_eh", "System",
];

/// The places in `folders` a library named with `-l` (`name`) may be, or a framework
/// (`<name>.framework`) in `frameworks`, in order.
pub fn places_in(name: &str, folders: &[PathBuf], frameworks: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(framework) = name.strip_suffix(".framework") {
        for dir in frameworks {
            for file in framework_files(framework) {
                push_once(&mut out, dir.join(file));
            }
        }
        return out;
    }
    let files = match name.strip_prefix(':') {
        Some(file) => vec![file.to_owned()],
        None => library_files(name),
    };
    for dir in folders {
        for file in &files {
            push_once(&mut out, dir.join(file));
        }
    }
    out
}

/// A name a GNU linker script hands the linker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptName {
    /// A file, as written.
    File(String),
    /// `-l<name>`: a library searched for like one on the line.
    Library(String),
}

/// Why a linker script can't be followed: the link is `built, not stored` (B-6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    /// A command other than `GROUP`, `INPUT`, `AS_NEEDED` and `OUTPUT_FORMAT`.
    #[error("a linker script command other than GROUP, INPUT, AS_NEEDED and OUTPUT_FORMAT")]
    Command,
    /// A name under the sysroot (`=…`, `$SYSROOT…`).
    #[error("a linker script names a file under the sysroot")]
    Sysroot,
    /// Text that doesn't read as such a script.
    #[error("a linker script that can't be read")]
    Malformed,
}

/// Whether the start of a file the linker takes for a library is a GNU linker script: text,
/// not an object, an archive, a Mach-O file or Apple's text stub (`.tbd`).
pub fn is_script(path: &Path, head: &[u8]) -> bool {
    const MAGIC: &[&[u8]] = &[
        b"\x7fELF",
        b"!<arch>\n",
        b"!<thin>\n",
        b"\xfe\xed\xfa\xce",
        b"\xfe\xed\xfa\xcf",
        b"\xce\xfa\xed\xfe",
        b"\xcf\xfa\xed\xfe",
        b"\xca\xfe\xba\xbe",
        b"\xbe\xba\xfe\xca",
        b"BC\xc0\xde",
        b"\xde\xc0\x17\x0b",
        b"---",
    ];
    path.extension().is_none_or(|ext| ext != "tbd")
        && !MAGIC.iter().any(|magic| head.starts_with(magic))
        && !head.contains(&0)
}

/// The names a GNU linker script hands the linker, when it holds nothing but `GROUP`, `INPUT`,
/// `AS_NEEDED` and `OUTPUT_FORMAT` (comments allowed), as glibc's `libc.so` and gcc's
/// `libgcc_s.so` do. Any other command, or a name under the sysroot, is an error.
pub fn script_names(text: &[u8]) -> Result<Vec<ScriptName>, ScriptError> {
    let text = std::str::from_utf8(text).map_err(|_| ScriptError::Malformed)?;
    let mut tokens = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == ';');
        if rest.is_empty() {
            break;
        }
        if let Some(after) = rest.strip_prefix("/*") {
            let end = after.find("*/").ok_or(ScriptError::Malformed)?;
            rest = &after[end + 2..];
            continue;
        }
        if let Some(after) = rest.strip_prefix(['(', ')']) {
            tokens.push(&rest[..1]);
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('"') {
            let end = after.find('"').ok_or(ScriptError::Malformed)?;
            tokens.push(&after[..end]);
            rest = &after[end + 1..];
            continue;
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | '"'))
            .unwrap_or(rest.len());
        tokens.push(&rest[..end]);
        rest = &rest[end..];
    }
    let mut names = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let command = tokens[i];
        if tokens.get(i + 1) != Some(&"(") {
            return Err(ScriptError::Command);
        }
        let close = matching(&tokens, i + 1).ok_or(ScriptError::Malformed)?;
        match command {
            "OUTPUT_FORMAT" => {}
            "GROUP" | "INPUT" => names_in(&tokens[i + 2..close], &mut names)?,
            _ => return Err(ScriptError::Command),
        }
        i = close + 1;
    }
    Ok(names)
}

/// The index of the `)` that closes the `(` at `open`.
fn matching(tokens: &[&str], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        match *token {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The names inside `GROUP ( … )` or `INPUT ( … )`, with `AS_NEEDED ( … )` inside them.
fn names_in(tokens: &[&str], names: &mut Vec<ScriptName>) -> Result<(), ScriptError> {
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        if token == "AS_NEEDED" && tokens.get(i + 1) == Some(&"(") {
            let close = matching(tokens, i + 1).ok_or(ScriptError::Malformed)?;
            names_in(&tokens[i + 2..close], names)?;
            i = close + 1;
            continue;
        }
        if matches!(token, "(" | ")") || token.is_empty() {
            return Err(ScriptError::Malformed);
        }
        if token.starts_with('=') || token.starts_with("$SYSROOT") {
            return Err(ScriptError::Sysroot);
        }
        names.push(match token.strip_prefix("-l") {
            Some(name) if !name.is_empty() => ScriptName::Library(name.to_owned()),
            _ => ScriptName::File(token.to_owned()),
        });
        i += 1;
    }
    Ok(())
}

/// The places `ld` may take a name a linker script hands it from (B-6): an absolute name as it
/// is; a relative one in the script's folder, then rustc's working directory, then each search
/// folder; `-l<name>` as a library named on the line, in the search folders.
pub fn script_places(
    name: &ScriptName,
    script: &Path,
    cwd: &Path,
    search: &[PathBuf],
) -> Vec<PathBuf> {
    match name {
        ScriptName::Library(library) => places_in(library, search, &[]),
        ScriptName::File(file) if Path::new(file).is_absolute() => vec![PathBuf::from(file)],
        ScriptName::File(file) => {
            let mut out = Vec::new();
            if let Some(dir) = script.parent() {
                push_once(&mut out, dir.join(file));
            }
            push_once(&mut out, cwd.join(file));
            for dir in search {
                push_once(&mut out, dir.join(file));
            }
            out
        }
    }
}

/// The files the linker may take a library named with `-l` (or `-framework`) from in the
/// line's own `-L` and `-F` folders: every candidate name in every one, in order (B-6). The
/// folders the driver and `ld` add are [`environment`]'s, their places [`places_in`]'s.
/// `-l:file` names the file itself.
pub fn library_places(inputs: &Inputs) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for named in &inputs.named {
        if let Some(framework) = named.strip_suffix(".framework") {
            for dir in &inputs.framework_search {
                for file in framework_files(framework) {
                    push_once(&mut out, dir.join(file));
                }
            }
            continue;
        }
        let files = match named.strip_prefix(':') {
            Some(file) => vec![file.to_owned()],
            None => library_files(named),
        };
        for dir in &inputs.search {
            for file in &files {
                push_once(&mut out, dir.join(file));
            }
        }
    }
    out
}

/// The names a library `name` may have in a search folder, for any toolchain.
pub fn library_files(name: &str) -> Vec<String> {
    ["a", "dylib", "so", "tbd", "rlib"]
        .iter()
        .map(|ext| format!("lib{name}.{ext}"))
        .chain([format!("{name}.lib")])
        .collect()
}

/// The files of framework `name` in a framework folder: its binary or its text stub.
pub fn framework_files(name: &str) -> Vec<String> {
    vec![
        format!("{name}.framework/{name}"),
        format!("{name}.framework/{name}.tbd"),
    ]
}

/// The places each file option's value may be: a path as given (joined to `cwd`), and for a
/// linker script also in each `-L` folder, as GNU ld looks for one.
pub fn file_places(inputs: &Inputs, cwd: &Path) -> Vec<(Vec<PathBuf>, bool)> {
    inputs
        .files
        .iter()
        .map(|file| {
            let mut places = vec![cwd.join(&file.value)];
            if file.script && !Path::new(&file.value).is_absolute() {
                for dir in &inputs.search {
                    push_once(&mut places, dir.join(&file.value));
                }
            }
            (places, file.script)
        })
        .collect()
}

/// Whether a path lies in one of rustc's own temporary folders (`rustc` and six characters):
/// rustc writes the symbol list there and removes it after the link. What it holds comes from
/// the compile itself, and its name is new each time.
pub fn in_rustc_temp(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix("rustc"))
        .is_some_and(|rest| rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Whether `path` lies in one of rustc's own temporary folders in the output folder `out_dir`,
/// at any depth: on Linux rustc hands the linker `-L <out_dir>/rustcXXXXXX/raw-dylibs`, a
/// folder of stubs it writes from the compile itself and removes after the link, under a name
/// that is new each time (B-18). Only a folder directly in `out_dir` counts, so a folder of the
/// person's that happens to have such a name elsewhere is still keyed.
pub fn under_rustc_temp(path: &Path, out_dir: &Path) -> bool {
    path.strip_prefix(out_dir)
        .ok()
        .and_then(|rest| rest.components().next())
        .and_then(|first| first.as_os_str().to_str())
        .and_then(|name| name.strip_prefix("rustc"))
        .is_some_and(|rest| rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Whether a linker script's text may name more files the linker reads: `INCLUDE`, `INPUT`,
/// `GROUP`, `SEARCH_DIR`, `STARTUP`. Those files can't be keyed without reading the script as
/// the linker does, so such a link isn't stored.
pub fn script_names_files(text: &[u8]) -> bool {
    const WORDS: [&[u8]; 5] = [b"INCLUDE", b"INPUT", b"GROUP", b"SEARCH_DIR", b"STARTUP"];
    WORDS
        .iter()
        .any(|word| text.windows(word.len()).any(|window| window == *word))
}

fn stat(path: PathBuf, need_exec: bool) -> Option<Linker> {
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || (need_exec && !is_executable(&meta)) {
        return None;
    }
    let stat = hushrun_digest::cache::Stat::of(&meta);
    Some(Linker {
        path,
        size: stat.size,
        modified_ns: stat.mtime_ns,
    })
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real stdout of rustc 1.94.0 on aarch64-apple-darwin for
    /// `rustc --crate-type bin --crate-name m m.rs --out-dir out2 --extern b=out/libb-bb.rlib
    /// -L dependency=out --print=link-args -C link-arg=-Wl,-headerpad,0x100
    /// -C 'link-arg=-Wl,-rpath,/x/quote"d dir\é' -L native=/usr/lib -l z`, where `b` depends on
    /// the rlib `c`. Home and work folders made up, `PATH` shortened.
    const MAC_BIN: &str = r#"env -u IPHONEOS_DEPLOYMENT_TARGET -u TVOS_DEPLOYMENT_TARGET -u XROS_DEPLOYMENT_TARGET LC_ALL="C" PATH="/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/bin:/home/dev/.cargo/bin:/usr/bin:/bin" SDKROOT="/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX27.2.sdk" VSLANG="1033" ZERO_AR_DATE="1" "cc" "/home/dev/demo/out2/rustc5BEql0/symbols.o" "out2/m.m.1f5ef0f70860fe6f-cgu.0.rcgu.o" "out2/m.aq8q9ph4zdggpqf279mwcw70t.rcgu.o" "-lz" "/home/dev/demo/out/libb-bb.rlib" "out/libc-cc.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libstd-5c291ca0698fa83b.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libpanic_unwind-84424eeee9ae6f5e.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libobject-54eed290fa3dfbf0.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libmemchr-9153871523b6c11d.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libaddr2line-74e9187b4f228085.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libgimli-080f3df39fb7032e.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libcfg_if-06deaf314d0ad593.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/librustc_demangle-dd201dd808509abb.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libstd_detect-19f17ad96e7dc1a8.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libhashbrown-4a1fc9a50d6a6d59.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/librustc_std_workspace_alloc-8a1256d5688efa7f.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libminiz_oxide-65beecf27467f206.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libadler2-93fcc938a09a3999.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libunwind-a14fdd6b23ce5a22.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/liblibc-b923b314a7e8e4e5.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/librustc_std_workspace_core-80c52f673cb30ffc.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/liballoc-cbe6dc6c34e9883f.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libcore-7025c4c036cf0618.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libcompiler_builtins-78743b33fda82a82.rlib" "-lSystem" "-lc" "-lm" "-arch" "arm64" "-mmacosx-version-min=11.0.0" "-L" "/usr/lib" "-o" "out2/m" "-Wl,-dead_strip" "-nodefaultlibs" "-Wl,-headerpad,0x100" "-Wl,-rpath,/x/quote\"d dir\\é""#;

    /// Real stdout of rustc 1.94.0 on aarch64-apple-darwin for a proc macro:
    /// `rustc --crate-type proc-macro --crate-name pm pm.rs --out-dir out3 -C extra-filename=-pp
    /// --print=link-args --error-format=json --json=artifacts`. Folders made up as above.
    const MAC_PROC_MACRO: &str = r#"env -u IPHONEOS_DEPLOYMENT_TARGET -u TVOS_DEPLOYMENT_TARGET -u XROS_DEPLOYMENT_TARGET LC_ALL="C" PATH="/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/bin:/home/dev/.cargo/bin:/usr/bin:/bin" SDKROOT="/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX27.2.sdk" VSLANG="1033" ZERO_AR_DATE="1" "cc" "-Wl,-exported_symbols_list" "-Wl,/home/dev/demo/out3/rustcqqZKHU/list" "/home/dev/demo/out3/rustcqqZKHU/symbols.o" "out3/pm-pp.pm.b3742f92a9b87361-cgu.0.rcgu.o" "/home/dev/demo/out3/rustcqqZKHU/rmeta.o" "out3/pm-pp.2uor1vs0nfwl8zgy454bfh1ly.rcgu.o" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libproc_macro-33fb95282b853a73.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/librustc_literal_escaper-ca4f80fffeb5e119.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libstd-5c291ca0698fa83b.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libpanic_unwind-84424eeee9ae6f5e.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libobject-54eed290fa3dfbf0.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libmemchr-9153871523b6c11d.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libaddr2line-74e9187b4f228085.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libgimli-080f3df39fb7032e.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libcfg_if-06deaf314d0ad593.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/librustc_demangle-dd201dd808509abb.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libstd_detect-19f17ad96e7dc1a8.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libhashbrown-4a1fc9a50d6a6d59.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/librustc_std_workspace_alloc-8a1256d5688efa7f.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libminiz_oxide-65beecf27467f206.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libadler2-93fcc938a09a3999.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libunwind-a14fdd6b23ce5a22.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/liblibc-b923b314a7e8e4e5.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/librustc_std_workspace_core-80c52f673cb30ffc.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/liballoc-cbe6dc6c34e9883f.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libcore-7025c4c036cf0618.rlib" "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib/libcompiler_builtins-78743b33fda82a82.rlib" "-lSystem" "-lc" "-lm" "-arch" "arm64" "-mmacosx-version-min=11.0.0" "-o" "out3/libpm-pp.dylib" "-Wl,-dead_strip" "-dynamiclib" "-nodefaultlibs""#;

    /// Built by hand in the same format (std's `Command` `Debug`) from what rustc passes `cc` on
    /// x86_64-unknown-linux-gnu, with a few driver forms added.
    const LINUX_BIN: &str = concat!(
        r#"LC_ALL="C" PATH="/home/u/.rustup/toolchains/1.94.0-x86_64-unknown-linux-gnu/lib/rustlib/x86_64-unknown-linux-gnu/bin:/usr/bin" VSLANG="1033" "#,
        r#""cc" "-m64" "/tmp/rustcAbC123/symbols.o" "target/debug/deps/app-1.app.abc-cgu.0.rcgu.o" "#,
        r#""-Wl,--as-needed" "-Wl,-Bstatic" "/work/target/debug/deps/libdep-2.rlib" "#,
        r#""/work/target/debug/build/ring-3/out/libring_core_0_17_14_.a" "-Wl,-Bdynamic" "#,
        r#""-lgcc_s" "-l" "ssl" "-l:libcrypto.a" "-Wl,-L,/opt/ssl/lib" "-L/opt/other" "#,
        r#""-Xlinker" "-L" "-Xlinker" "/opt/x" "/usr/lib/x86_64-linux-gnu/libz.so.1.2.13" "#,
        r#""-B/home/u/.rustup/toolchains/1.94.0-x86_64-unknown-linux-gnu/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld" "#,
        r#""-fuse-ld=lld" "-Wl,--sysroot=/opt/sysroot" "-L" "target/debug/build/ring-3/out" "#,
        r#""-o" "target/debug/deps/app-1" "-Wl,-soname,libapp.so.1" "-Wl,--gc-sections" "-pie" "-nodefaultlibs""#,
    );

    #[test]
    // covers: B-18
    fn real_macos_program_line() {
        let args = parse(MAC_BIN).unwrap();
        assert_eq!(args.linker, "cc");
        assert!(!args.env_cleared);
        assert_eq!(args.cwd, None);
        assert_eq!(
            &args.env[..4],
            [
                ("IPHONEOS_DEPLOYMENT_TARGET".to_owned(), None),
                ("TVOS_DEPLOYMENT_TARGET".to_owned(), None),
                ("XROS_DEPLOYMENT_TARGET".to_owned(), None),
                ("LC_ALL".to_owned(), Some("C".to_owned())),
            ]
        );
        assert_eq!(
            args.set_var("PATH"),
            Some(
                "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/bin:/home/dev/.cargo/bin:/usr/bin:/bin"
            )
        );
        assert_eq!(args.set_var("IPHONEOS_DEPLOYMENT_TARGET"), None);
        // The escaped quote and backslash, and the é rustc leaves as it is.
        assert_eq!(
            args.args.last().map(String::as_str),
            Some(r#"-Wl,-rpath,/x/quote"d dir\é"#)
        );

        let cwd = Path::new("/home/dev/demo");
        let inputs = args.inputs(cwd);
        let sysroot = "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/lib/rustlib/aarch64-apple-darwin/lib";
        // Both dependencies, the one named by `--extern` and the one it depends on, the second
        // given relative to the working directory.
        assert_eq!(
            inputs.libraries[..2],
            [
                PathBuf::from("/home/dev/demo/out/libb-bb.rlib"),
                PathBuf::from("/home/dev/demo/out/libc-cc.rlib"),
            ]
        );
        assert_eq!(inputs.libraries.len(), 2 + 19);
        assert!(inputs.libraries[2..].iter().all(|p| p.starts_with(sysroot)));
        assert_eq!(
            inputs.objects,
            [
                PathBuf::from("/home/dev/demo/out2/rustc5BEql0/symbols.o"),
                PathBuf::from("/home/dev/demo/out2/m.m.1f5ef0f70860fe6f-cgu.0.rcgu.o"),
                PathBuf::from("/home/dev/demo/out2/m.aq8q9ph4zdggpqf279mwcw70t.rcgu.o"),
            ]
        );
        assert_eq!(inputs.named, ["z", "System", "c", "m"]);
        assert_eq!(inputs.search, [PathBuf::from("/usr/lib")]);
        // No `-isysroot`: the SDK comes from the `SDKROOT` the command set.
        assert_eq!(
            inputs.sdk,
            Some(PathBuf::from(
                "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX27.2.sdk"
            ))
        );
        // `-o out2/m` is the output, not an input.
        assert!(!inputs.libraries.iter().any(|p| p.ends_with("out2/m")));
        assert!(inputs.response_files.is_empty());
    }

    #[test]
    // covers: B-18
    fn real_macos_proc_macro_line_does_not_take_its_output_for_a_library() {
        let args = parse(MAC_PROC_MACRO).unwrap();
        assert_eq!(args.linker, "cc");
        assert!(args.args.contains(&"-dynamiclib".to_owned()));
        let inputs = args.inputs(Path::new("/home/dev/demo"));
        // `-o out3/libpm-pp.dylib` ends like a library but is what the link writes.
        assert!(
            !inputs
                .libraries
                .iter()
                .any(|p| p.ends_with("libpm-pp.dylib"))
        );
        assert!(
            inputs
                .libraries
                .iter()
                .any(|p| p.to_str().unwrap().contains("libproc_macro-"))
        );
        // The exported-symbols list is a `-Wl,` value with no library extension: not an input.
        assert!(!inputs.objects.iter().any(|p| p.ends_with("list")));
        assert_eq!(inputs.objects.len(), 4);
    }

    #[test]
    // covers: B-18
    fn gnu_driver_forms() {
        let args = parse(LINUX_BIN).unwrap();
        let inputs = args.inputs(Path::new("/work"));
        assert_eq!(
            inputs.libraries,
            [
                PathBuf::from("/work/target/debug/deps/libdep-2.rlib"),
                PathBuf::from("/work/target/debug/build/ring-3/out/libring_core_0_17_14_.a"),
                PathBuf::from("/usr/lib/x86_64-linux-gnu/libz.so.1.2.13"),
            ]
        );
        assert_eq!(inputs.named, ["gcc_s", "ssl", ":libcrypto.a"]);
        assert_eq!(
            inputs.search,
            [
                PathBuf::from("/opt/ssl/lib"),
                PathBuf::from("/opt/other"),
                PathBuf::from("/opt/x"),
                PathBuf::from("/work/target/debug/build/ring-3/out"),
            ]
        );
        assert_eq!(inputs.sdk, Some(PathBuf::from("/opt/sysroot")));
        // `-soname libapp.so.1` names the output.
        assert!(!inputs.libraries.iter().any(|p| p.ends_with("libapp.so.1")));
        // rustc's own lld on Linux: `-fuse-ld=lld`, found first in the `-B` folder.
        assert_eq!(inputs.fuse_ld.as_deref(), Some("lld"));
        assert_eq!(
            inputs.prefixes,
            [PathBuf::from(
                "/home/u/.rustup/toolchains/1.94.0-x86_64-unknown-linux-gnu/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld"
            )]
        );
    }

    #[test]
    // covers: B-6, B-18
    fn files_the_linker_reads_are_named() {
        let line = r#""cc" "-Tlink.x" "-T" "/abs/memory.x" "-Ttext" "0x100" "-Wl,--version-script=/w/exports.map" "-Wl,--dynamic-list,dyn.list" "-Wl,-exported_symbols_list" "-Wl,/w/out/rustcqqZKHU/list" "-Wl,-sectcreate,__TEXT,__info,Info.plist" "-L" "/w/out" "-lfoo" "-l:libbar.a" "-framework" "Sec" "-F/fw" "-o" "/w/out/app""#;
        let inputs = parse(line).unwrap().inputs(Path::new("/w"));
        let files: Vec<(&str, bool)> = inputs
            .files
            .iter()
            .map(|f| (f.value.as_str(), f.script))
            .collect();
        assert_eq!(
            files,
            [
                ("link.x", true),
                ("/abs/memory.x", true),
                ("/w/exports.map", false),
                ("dyn.list", false),
                ("/w/out/rustcqqZKHU/list", false),
                ("Info.plist", false),
            ]
        );
        let places = file_places(&inputs, Path::new("/w"));
        assert_eq!(
            places[0],
            (
                vec![PathBuf::from("/w/link.x"), PathBuf::from("/w/out/link.x")],
                true
            )
        );
        assert_eq!(places[1].0, [PathBuf::from("/abs/memory.x")]);
        assert_eq!(places[3].0, [PathBuf::from("/w/dyn.list")]);
        assert!(in_rustc_temp(&places[4].0[0]));
        assert!(!in_rustc_temp(Path::new("/w/out/rustc/list")));
        assert!(!in_rustc_temp(Path::new("/w/rustcfoobarbaz/list")));
        // A search folder inside one, as rustc passes it on Linux, at any depth.
        let out = Path::new("/w/target/debug/deps");
        assert!(under_rustc_temp(
            Path::new("/w/target/debug/deps/rustcT5XijC/raw-dylibs/libc.so"),
            out
        ));
        assert!(!under_rustc_temp(
            Path::new("/w/rustcT5XijC/raw-dylibs/libc.so"),
            out
        ));
        assert!(!under_rustc_temp(
            Path::new("/w/target/debug/deps/rustcfoobarbaz/libc.so"),
            out
        ));

        let libraries = library_places(&inputs);
        assert_eq!(libraries[0], PathBuf::from("/w/out/libfoo.a"));
        assert!(libraries.contains(&PathBuf::from("/w/out/libfoo.dylib")));
        assert!(libraries.contains(&PathBuf::from("/w/out/foo.lib")));
        assert!(libraries.contains(&PathBuf::from("/w/out/libbar.a")));
        assert!(libraries.contains(&PathBuf::from("/fw/Sec.framework/Sec")));
        assert_eq!(libraries.len(), 6 + 1 + 2);

        let mac = r#""cc" "-Wl,-bundle_loader,/w/host" "-Wl,-dylib_file,@rpath/libq.dylib:/w/q/libq.dylib" "obj.o" "data.bin" "-arch" "arm64" "-o" "/w/out/app""#;
        let inputs = parse(mac).unwrap().inputs(Path::new("/w"));
        let files: Vec<&str> = inputs.files.iter().map(|f| f.value.as_str()).collect();
        assert_eq!(files, ["/w/host", "/w/q/libq.dylib"]);
        assert!(inputs.libraries.is_empty(), "{:?}", inputs.libraries);
        assert_eq!(
            inputs.paths,
            [
                PathBuf::from("/w/obj.o"),
                PathBuf::from("/w/data.bin"),
                PathBuf::from("/w/arm64"),
            ]
        );

        assert!(script_names_files(b"MEMORY {}\nINCLUDE memory.x\n"));
        assert!(script_names_files(b"GROUP ( libc.so.6 )"));
        assert!(!script_names_files(b"SECTIONS { .text : { *(.text) } }"));
    }

    /// Makes `path` an executable shell script that runs `body`.
    #[cfg(unix)]
    fn script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The questions asked, and a real run of each.
    #[cfg(unix)]
    struct Asked {
        seen: std::cell::RefCell<Vec<(PathBuf, Vec<String>)>>,
    }

    #[cfg(unix)]
    impl Asked {
        fn new() -> Asked {
            Asked {
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn ask(&self) -> impl Fn(&Path, &[String], &OsStr) -> Option<Answer> + '_ {
            |program, args, path| {
                self.seen
                    .borrow_mut()
                    .push((program.to_path_buf(), args.to_vec()));
                let out = std::process::Command::new(program)
                    .args(args)
                    .env("PATH", path)
                    .output()
                    .ok()?;
                Some(Answer {
                    success: out.status.success(),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                })
            }
        }
    }

    /// A gcc-like driver at `<root>/cc`: it answers `-print-prog-name` with the bare name and
    /// `-print-search-dirs` with `programs` and `libraries`, and fails at anything else.
    #[cfg(unix)]
    fn gcc(root: &Path, programs: &str, libraries: &str) -> PathBuf {
        let driver = root.join("cc");
        script(
            &driver,
            &format!(
                "for a in \"$@\"; do case \"$a\" in\n\
                 -print-prog-name=*) echo \"${{a#-print-prog-name=}}\"; exit 0;;\n\
                 -print-search-dirs) printf 'install: /x/\\nprograms: ={programs}\\nlibraries: ={libraries}\\n'; exit 0;;\n\
                 esac; done; exit 1"
            ),
        );
        driver
    }

    #[cfg(unix)]
    fn query(root: &Path, driver: &Path, flags: &[&str], path: &[&Path]) -> Query {
        Query {
            driver: driver.to_path_buf(),
            flags: flags.iter().map(|f| (*f).to_owned()).collect(),
            ld_sysroot: None,
            path: std::env::join_paths(path).unwrap(),
            cwd: root.to_path_buf(),
            apple: false,
            sdk: None,
            library_path: None,
        }
    }

    #[cfg(unix)]
    #[test]
    // covers: B-18
    fn the_ld_a_gcc_driver_runs_is_looked_up_as_collect2_does() {
        let dir = tempfile::tempdir().unwrap();
        let root = &resolve_folder(dir.path());
        let progs = root.join("progs");
        let driver = gcc(
            root,
            &format!("{}/:{}/missing/", progs.display(), root.display()),
            "",
        );
        let early = root.join("early");
        let bin = root.join("bin");
        std::fs::create_dir_all(&early).unwrap();
        script(&bin.join("ld"), "exit 1");
        let asked = Asked::new();
        let ld = |flags: &[&str]| {
            environment(&query(root, &driver, flags, &[&early, &bin]), &asked.ask()).map(|e| e.ld)
        };
        // Found on `PATH`, after the driver said only `ld`.
        assert_eq!(ld(&[]), Ok(bin.join("ld")));
        // Another `ld` appears earlier on `PATH`: that one runs now.
        script(&early.join("ld"), "exit 1");
        assert_eq!(ld(&[]), Ok(early.join("ld")));
        // Not executable: passed over, as `collect2` passes it over.
        std::fs::remove_file(early.join("ld")).unwrap();
        std::fs::write(early.join("ld"), "not a program").unwrap();
        assert_eq!(ld(&[]), Ok(bin.join("ld")));
        // The driver's program folders come before `PATH`, `-B` folders before them, and
        // `real-ld` before `ld` in either.
        script(&progs.join("ld"), "exit 1");
        assert_eq!(ld(&[]), Ok(progs.join("ld")));
        script(&root.join("b/ld"), "exit 1");
        assert_eq!(ld(&["-B", "b"]), Ok(root.join("b/ld")));
        assert_eq!(ld(&["-Bb"]), Ok(root.join("b/ld")));
        script(&progs.join("real-ld"), "exit 1");
        assert_eq!(ld(&[]), Ok(progs.join("real-ld")));
        std::fs::remove_file(progs.join("real-ld")).unwrap();
        // `-fuse-ld=lld`: gcc is asked for `ld.lld`, which `collect2` looks for the same way.
        asked.seen.borrow_mut().clear();
        assert_eq!(ld(&["-fuse-ld=lld"]), Err(Unknown::NoLd));
        assert!(
            asked
                .seen
                .borrow()
                .iter()
                .any(|(program, args)| program == &driver
                    && args == &["-fuse-ld=lld", "-print-prog-name=ld.lld"]),
            "{:?}",
            asked.seen.borrow()
        );
        script(&bin.join("ld.lld"), "exit 1");
        assert_eq!(ld(&["-fuse-ld=lld"]), Ok(bin.join("ld.lld")));
        // A path is the linker, asked nothing: clang's `--ld-path=`, which wins, or a path given
        // to `-fuse-ld=`.
        std::fs::write(root.join("myld"), "x").unwrap();
        asked.seen.borrow_mut().clear();
        let path_only = query(root, &driver, &["-fuse-ld=lld", "--ld-path=myld"], &[&bin]);
        let mut chosen = None;
        which_ld(&path_only, &asked.ask(), &mut chosen).unwrap();
        assert!(asked.seen.borrow().is_empty());
        assert_eq!(
            which_ld(&path_only, &asked.ask(), &mut chosen),
            Ok(root.join("myld"))
        );
        assert_eq!(
            ld(&[&format!("-fuse-ld={}/myld", root.display())]),
            Ok(root.join("myld"))
        );
        assert_eq!(ld(&["--ld-path=nothing-here"]), Err(Unknown::NoLd));
        // No C driver: rustc runs `rust-lld` itself, which is the `ld`, asked nothing.
        asked.seen.borrow_mut().clear();
        let lld = environment(
            &query(root, &driver, &["-flavor", "gnu"], &[&bin]),
            &asked.ask(),
        )
        .unwrap();
        assert_eq!(lld.ld, driver);
        assert!(lld.folders.is_empty());
        assert!(asked.seen.borrow().is_empty());
        let line = parse(r#""rust-lld" "-flavor" "wasm" "-L" "/x" "--fake-flag""#).unwrap();
        assert_eq!(line.driver_flags(), ["-flavor", "wasm"]);
        // Nothing by that name anywhere; a driver that can't answer.
        std::fs::remove_file(progs.join("ld")).unwrap();
        std::fs::remove_file(bin.join("ld")).unwrap();
        assert_eq!(ld(&[]), Err(Unknown::NoLd));
        script(&driver, "exit 1");
        assert_eq!(ld(&[]), Err(Unknown::Driver));
    }

    #[cfg(unix)]
    #[test]
    // covers: B-18
    fn a_driver_that_answers_a_path_names_its_ld() {
        // clang searches `PATH` itself, and Apple's driver names the selected Xcode's `ld`.
        let dir = tempfile::tempdir().unwrap();
        let root = &resolve_folder(dir.path());
        let chosen = root.join("xcode/usr/bin/ld");
        script(&chosen, "exit 1");
        let driver = root.join("clang");
        script(
            &driver,
            &format!(
                "case \"$1\" in -print-prog-name=ld) echo {};; *) exit 1;; esac",
                chosen.display()
            ),
        );
        let asked = Asked::new();
        let mut q = query(root, &driver, &[], &[]);
        q.apple = true;
        let found = environment(&q, &asked.ask()).unwrap();
        assert_eq!(found.ld, chosen);
        // On Apple's targets that is the only question.
        assert_eq!(asked.seen.borrow().len(), 1);
        // Without an SDK, Apple's `ld` searches its two folders and two framework folders.
        assert_eq!(
            found.folders,
            [
                PathBuf::from("/usr/lib"),
                resolve_folder(Path::new("/usr/local/lib"))
            ]
        );
        assert_eq!(found.frameworks.len(), 2);
        // With one, the SDK's first.
        q.sdk = Some(root.join("sdk"));
        let found = environment(&q, &asked.ask()).unwrap();
        assert_eq!(
            found.folders[..2],
            [root.join("sdk/usr/lib"), root.join("sdk/usr/local/lib")]
        );
        assert_eq!(
            found.frameworks[1],
            root.join("sdk/System/Library/Frameworks")
        );
        // Each `LIBRARY_PATH` folder under the SDK, then as it is, before the SDK's own.
        q.library_path = Some(OsString::from("/opt/lp"));
        let found = environment(&q, &asked.ask()).unwrap();
        assert_eq!(
            found.folders[..3],
            [
                root.join("sdk/opt/lp"),
                PathBuf::from("/opt/lp"),
                root.join("sdk/usr/lib")
            ]
        );
        // An answer that names nothing there.
        std::fs::remove_file(&chosen).unwrap();
        assert_eq!(environment(&q, &asked.ask()), Err(Unknown::NoLd));
    }

    #[cfg(unix)]
    #[test]
    // covers: B-6
    fn the_folders_ld_searches_come_from_the_driver_then_from_ld() {
        let dir = tempfile::tempdir().unwrap();
        let root = &resolve_folder(dir.path());
        for folder in ["gcc/14", "lib", "usr/lib", "sr/usr/lib"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        let r = root.display();
        // The driver names one folder twice, once through `..`, and one that doesn't exist.
        let driver = gcc(
            root,
            "",
            &format!("{r}/gcc/14/:{r}/gcc/14/../../lib/:{r}/lib/:{r}/no/../gone/"),
        );
        let bin = root.join("bin");
        script(
            &bin.join("ld"),
            &format!(
                "case \"$1\" in\n\
                 --verbose) echo 'SEARCH_DIR(\"={r}/usr/lib\"); SEARCH_DIR(\"{r}/lib\"); SEARCH_DIR(\"=/usr/lib\");';;\n\
                 --print-sysroot) echo;;\n\
                 --sysroot=*) echo \"${{1#--sysroot=}}\";;\n\
                 *) exit 1;; esac"
            ),
        );
        let asked = Asked::new();
        let q = query(root, &driver, &[], &[&bin]);
        let found = environment(&q, &asked.ask()).unwrap();
        assert_eq!(found.ld, bin.join("ld"));
        assert_eq!(found.sysroot, None);
        assert_eq!(
            found.folders,
            [
                root.join("gcc/14"),
                root.join("lib"),
                root.join("gone"),
                root.join("usr/lib"),
                resolve_folder(Path::new("/usr/lib")),
            ]
        );
        // `ld --print-sysroot` with the line's `--sysroot`: `=` folders are under it.
        let mut q = q;
        q.ld_sysroot = Some(format!("{r}/sr"));
        let found = environment(&q, &asked.ask()).unwrap();
        assert_eq!(found.sysroot, Some(format!("{r}/sr")));
        assert!(
            found.folders.contains(&root.join(format!("sr{r}/usr/lib"))),
            "{:?}",
            found.folders
        );
        assert!(
            found.folders.contains(&root.join("sr/usr/lib")),
            "{:?}",
            found.folders
        );
        // An `ld` with neither option (`lld`): the driver's folders alone.
        script(&bin.join("ld"), "exit 1");
        q.ld_sysroot = None;
        let found = environment(&q, &asked.ask()).unwrap();
        assert_eq!(found.folders.len(), 3);
        assert_eq!(found.sysroot, None);
        // `LIBRARY_PATH`'s folders come first: clang hands them to `ld` without listing them.
        q.library_path = Some(OsString::from(format!("{r}/lp:rel")));
        let found = environment(&q, &asked.ask()).unwrap();
        assert_eq!(found.folders[..2], [root.join("lp"), root.join("rel")]);
    }

    #[test]
    // covers: B-6
    fn linker_scripts_that_only_name_files_are_read() {
        let libc = b"/* GNU ld script\n   Use the shared library, but some functions are only in\n   the static library, so try that secondarily.  */\nOUTPUT_FORMAT(elf64-littleaarch64)\nGROUP ( /lib/aarch64-linux-gnu/libc.so.6 /usr/lib/aarch64-linux-gnu/libc_nonshared.a  AS_NEEDED ( /lib/ld-linux-aarch64.so.1 ) )\n";
        assert_eq!(
            script_names(libc),
            Ok(vec![
                ScriptName::File("/lib/aarch64-linux-gnu/libc.so.6".into()),
                ScriptName::File("/usr/lib/aarch64-linux-gnu/libc_nonshared.a".into()),
                ScriptName::File("/lib/ld-linux-aarch64.so.1".into()),
            ])
        );
        let libgcc_s = b"/* GNU ld script\n   Use the shared library, but some functions are only in\n   the static library.  */\nGROUP ( libgcc_s.so.1 -lgcc )\n";
        assert_eq!(
            script_names(libgcc_s),
            Ok(vec![
                ScriptName::File("libgcc_s.so.1".into()),
                ScriptName::Library("gcc".into()),
            ])
        );
        assert_eq!(
            script_names(b"INPUT(\"a b.o\", c.o)\nOUTPUT_FORMAT(\"elf64-x86-64\", \"elf64-x86-64\", \"elf64-x86-64\")"),
            Ok(vec![ScriptName::File("a b.o".into()), ScriptName::File("c.o".into())])
        );
        assert_eq!(script_names(b""), Ok(vec![]));
        for (text, why) in [
            (
                &b"SECTIONS { .text : { *(.text) } }"[..],
                ScriptError::Command,
            ),
            (
                b"GROUP ( libc.so.6 )\nSEARCH_DIR(/opt/lib)",
                ScriptError::Command,
            ),
            (b"INCLUDE other.ld", ScriptError::Command),
            (b"GROUP ( =/usr/lib/libc.so.6 )", ScriptError::Sysroot),
            (b"GROUP ( $SYSROOT/lib/libc.so.6 )", ScriptError::Sysroot),
            (b"GROUP ( a.so", ScriptError::Malformed),
            (b"/* unterminated", ScriptError::Malformed),
        ] {
            assert_eq!(
                script_names(text),
                Err(why),
                "{}",
                String::from_utf8_lossy(text)
            );
        }
        let at = Path::new("/l/libx.so");
        assert!(is_script(at, b"/* GNU ld script */ GROUP ( x )"));
        assert!(!is_script(at, b"\x7fELF\x02\x01"));
        assert!(!is_script(at, b"!<arch>\nmember"));
        assert!(!is_script(Path::new("/l/libx.dylib"), b"\xcf\xfa\xed\xfe"));
        assert!(!is_script(
            Path::new("/sdk/usr/lib/libSystem.tbd"),
            b"--- !tapi-tbd\n"
        ));
        assert!(!is_script(at, b"text\0with a nul"));
        // Where each name is looked for.
        let search = [PathBuf::from("/s1"), PathBuf::from("/s2")];
        let cwd = Path::new("/w");
        assert_eq!(
            script_places(&ScriptName::File("/abs/libc.so.6".into()), at, cwd, &search),
            [PathBuf::from("/abs/libc.so.6")]
        );
        assert_eq!(
            script_places(&ScriptName::File("libgcc_s.so.1".into()), at, cwd, &search),
            [
                PathBuf::from("/l/libgcc_s.so.1"),
                PathBuf::from("/w/libgcc_s.so.1"),
                PathBuf::from("/s1/libgcc_s.so.1"),
                PathBuf::from("/s2/libgcc_s.so.1"),
            ]
        );
        let gcc = script_places(&ScriptName::Library("gcc".into()), at, cwd, &search);
        assert_eq!(gcc[0], PathBuf::from("/s1/libgcc.a"));
        assert!(gcc.contains(&PathBuf::from("/s2/libgcc.so")));
    }

    #[test]
    // covers: B-18
    fn the_driver_options_that_change_its_answers_are_kept() {
        let line = r#"PATH="/t/bin:/usr/bin" "cc" "-m64" "x.o" "-B" "/t/gcc-ld" "-B/t/two" "-Wl,-Bstatic" "-Bdynamic" "-fuse-ld=lld" "--target=x86_64-unknown-linux-gnu" "-Xlinker" "-m" "-Xlinker" "--sysroot=/ld-only" "-Wl,--sysroot,/later" "-nodefaultlibs" "-lc" "-L/x" "-o" "out""#;
        let args = parse(line).unwrap();
        assert_eq!(
            args.driver_flags(),
            [
                "-m64",
                "-B",
                "/t/gcc-ld",
                "-B/t/two",
                "-fuse-ld=lld",
                "--target=x86_64-unknown-linux-gnu",
                "-nodefaultlibs",
            ]
        );
        assert_eq!(args.ld_sysroot().as_deref(), Some("/later"));
        assert!(!args.driver_reads_options());
        // B-8: each flag's path is handed to what replaces roots as a path of its own, the
        // folder of a joined `-B` too.
        let seen = |flag: &str| flag_with_paths(flag, |path| format!("<{path}>"));
        assert_eq!(seen("-B/t/two"), "-B</t/two>");
        assert_eq!(seen("-B"), "<-B>");
        assert_eq!(seen("/t/gcc-ld"), "</t/gcc-ld>");
        assert_eq!(seen("--sysroot=/s"), "<--sysroot=/s>");
        assert_eq!(seen("-Bstatic"), "<-Bstatic>");
        for option in [
            r#""-specs=lld.specs""#,
            r#""--specs=x""#,
            r#""--specs" "x""#,
            r#""-specs" "x""#,
            r#""--config" "x.cfg""#,
            r#""--config=x.cfg""#,
            r#""--config-user-dir=/c""#,
        ] {
            let args = parse(&format!(r#""cc" "x.o" {option}"#)).unwrap();
            assert!(args.driver_reads_options(), "{option}");
        }
        let args = parse(r#""cc" "-Wl,--specs=not-the-drivers""#).unwrap();
        assert!(!args.driver_reads_options());
    }

    #[test]
    // covers: B-18
    fn the_link_path_is_rustcs_entries_then_the_inherited_one() {
        let inherited = OsStr::new("/home/dev/bin:/usr/bin");
        let (before, inherits) = path_parts(
            Some("/t/lib/rustlib/x/bin:/home/dev/bin:/usr/bin"),
            Some(inherited),
        );
        assert_eq!(before, [PathBuf::from("/t/lib/rustlib/x/bin")]);
        assert!(inherits);
        // The lookup takes the inherited part from its own variables.
        assert_eq!(
            link_path(&before, inherits, Some(OsStr::new("/early:/usr/bin"))),
            Some(OsString::from("/t/lib/rustlib/x/bin:/early:/usr/bin"))
        );
        // A `PATH` that doesn't end with the inherited one is kept whole.
        let (before, inherits) = path_parts(Some("/a:/b"), Some(inherited));
        assert_eq!(before.len(), 2);
        assert!(!inherits);
        assert_eq!(
            link_path(&before, inherits, Some(inherited)),
            Some(OsString::from("/a:/b"))
        );
        // None printed: the inherited one.
        assert_eq!(path_parts(None, Some(inherited)), (vec![], true));
        assert_eq!(
            link_path(&[], true, Some(inherited)),
            Some(inherited.to_os_string())
        );
        assert_eq!(link_path(&[], true, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_is_resolved_as_the_kernel_finds_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = &resolve_folder(dir.path());
        std::fs::create_dir_all(root.join("usr/lib/gcc/14")).unwrap();
        std::os::unix::fs::symlink("usr/lib", root.join("lib")).unwrap();
        assert_eq!(
            resolve_folder(&root.join("usr/lib/gcc/14/../../")),
            root.join("usr/lib")
        );
        // Through a link, `..` is the link's target's parent, as for the kernel.
        assert_eq!(
            resolve_folder(&root.join("lib/../lib/")),
            root.join("usr/lib")
        );
        assert_eq!(
            resolve_folder(&root.join("lib/gcc/14/../../../x")),
            root.join("usr/x")
        );
        // A part that doesn't exist is taken by name.
        assert_eq!(
            resolve_folder(&root.join("usr/lib/none/../more")),
            root.join("usr/lib/more")
        );
    }

    #[test]
    // covers: B-18
    fn sdk_forms_and_frameworks() {
        let line = r#"SDKROOT="/env/sdk" "cc" "-isysroot" "/a/sdk" "-framework" "Security" "-weak_framework" "CoreFoundation" "-F/fw""#;
        let inputs = parse(line).unwrap().inputs(Path::new("/w"));
        assert_eq!(inputs.sdk, Some(PathBuf::from("/a/sdk")));
        assert_eq!(
            inputs.named,
            ["Security.framework", "CoreFoundation.framework"]
        );
        assert!(inputs.search.is_empty());
        assert_eq!(inputs.framework_search, [PathBuf::from("/fw")]);

        for (arg, want) in [
            (r#""-isysroot/b/sdk""#, "/b/sdk"),
            (r#""-isysroot=/c/sdk""#, "/c/sdk"),
            (r#""--sysroot=/d""#, "/d"),
            (r#""--sysroot" "/e""#, "/e"),
            (r#""-Wl,-syslibroot,/f/sdk""#, "/f/sdk"),
            (r#""-syslibroot" "/g""#, "/g"),
            (r#""-Xlinker" "-syslibroot" "-Xlinker" "/h""#, "/h"),
        ] {
            let line = format!(r#""ld" {arg}"#);
            let inputs = parse(&line).unwrap().inputs(Path::new("/w"));
            assert_eq!(inputs.sdk, Some(PathBuf::from(want)), "{arg}");
        }
        let none = parse(r#""cc" "-lz""#).unwrap().inputs(Path::new("/w"));
        assert_eq!(none.sdk, None);
    }

    #[test]
    fn escapes_and_spaces_decode() {
        let line = r#"cd "/dir with \"q\"" && env -i A="x\ny" "[\u{7f}]" "a b" "tab\there" "nul\0" "it\'s" "\u{301}é" "\\""#;
        let args = parse(line).unwrap();
        assert_eq!(args.cwd.as_deref(), Some(r#"/dir with "q""#));
        assert!(args.env_cleared);
        assert_eq!(args.env, [("A".to_owned(), Some("x\ny".to_owned()))]);
        assert_eq!(args.linker, "[\u{7f}]");
        assert_eq!(
            args.args,
            ["a b", "tab\there", "nul\0", "it's", "\u{301}é", "\\"]
        );
    }

    #[test]
    fn a_program_that_differs_from_its_first_argument() {
        let args = parse(r#"["/usr/bin/clang"] "cc" "-v""#).unwrap();
        assert_eq!(args.linker, "/usr/bin/clang");
        assert_eq!(args.args, ["-v"]);
    }

    #[test]
    fn utf8_bytes_written_as_hex_are_joined_and_bad_ones_refused() {
        // A CString's `Debug` writes every byte that isn't printable ASCII as `\xNN`.
        let args = parse(r#""\xc3\xa9cc" "x""#).unwrap();
        assert_eq!(args.linker, "écc");
        assert_eq!(parse(r#""cc" "\xff""#), Err(Error::NotUtf8));
    }

    #[test]
    // covers: B-18
    fn other_lines_are_skipped_but_never_guessed_between() {
        let stdout = format!("hello from a proc macro\nwarning: x\n{MAC_BIN}\n");
        assert!(parse(&stdout).is_ok());
        assert_eq!(parse(""), Err(Error::NoCommand));
        assert_eq!(parse("cargo:rerun-if-changed=x\n"), Err(Error::NoCommand));
        let twice = format!("{LINUX_BIN}\n{LINUX_BIN}\n");
        assert_eq!(parse(&twice), Err(Error::Several(2)));
        // A proc macro that prints a quoted word makes two candidates: an error, not a guess.
        let quoted = format!("\"hello\"\n{LINUX_BIN}\n");
        assert_eq!(parse(&quoted), Err(Error::Several(2)));
    }

    #[test]
    // covers: B-18
    fn stdout_goes_on_without_the_linker_command() {
        let mut bytes = format!("hello from a proc macro\n{LINUX_BIN}\nmore\n").into_bytes();
        bytes.extend_from_slice(b"\xfe no newline");
        let mut expected = b"hello from a proc macro\nmore\n".to_vec();
        expected.extend_from_slice(b"\xfe no newline");
        assert_eq!(without_command(&bytes), expected);
        // Two lines that read as commands: the last is rustc's.
        let quoted = format!("\"hello\"\n{LINUX_BIN}\n");
        assert_eq!(without_command(quoted.as_bytes()), b"\"hello\"\n");
        assert_eq!(without_command(b""), b"");
        assert_eq!(without_command(b"only text\n"), b"only text\n");
    }

    #[test]
    fn malformed_lines_are_not_commands() {
        for line in [
            r#""cc"  "two spaces""#,
            r#""cc" "unterminated"#,
            r#""cc" "bad \q escape""#,
            r#"env "cc""#,
            r#"A="1""cc""#,
            r#"A=1 "cc""#,
            r#"A B="1" "cc""#,
            r#""cc" trailing"#,
            r#"cd "/x" "cc""#,
            r#"LC_ALL="C" "#,
            r#""cc" "\u{110000}""#,
            r#""cc" "\u{}""#,
            r#""cc" "\x4""#,
        ] {
            assert_eq!(parse(line), Err(Error::NoCommand), "{line}");
        }
    }

    #[test]
    fn response_files_are_reported() {
        let inputs = parse(r#""cc" "@args.rsp" "-Wl,@/abs/more""#)
            .unwrap()
            .inputs(Path::new("/w"));
        assert_eq!(
            inputs.response_files,
            [PathBuf::from("/w/args.rsp"), PathBuf::from("/abs/more")]
        );
    }

    #[test]
    fn library_names_by_extension() {
        for (name, library) in [
            ("libx.rlib", true),
            ("libx.a", true),
            ("x.lib", true),
            ("libx.dylib", true),
            ("libx.so", true),
            ("libx.so.6", true),
            ("libx.tbd", true),
            ("libx.so.6a", false),
            ("libx.sox", false),
            ("x.rs", false),
            (".a", false),
        ] {
            assert_eq!(
                matches!(kind_of(name), Some(Kind::Library)),
                library,
                "{name}"
            );
        }
        assert!(matches!(kind_of("dir.so/x.o"), Some(Kind::Object)));
        assert!(matches!(kind_of("x.obj"), Some(Kind::Object)));
    }

    #[cfg(unix)]
    #[test]
    // covers: B-18
    fn linker_is_found_like_the_shell_finds_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        // Not executable in the first folder: skipped, as `execvp` would.
        std::fs::write(first.join("cc"), "not me").unwrap();
        std::fs::write(second.join("cc"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(second.join("cc"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let path = std::env::join_paths([&first, &second]).unwrap();
        let found = linker("cc", Some(&path), Path::new("/")).unwrap();
        assert_eq!(found.path, second.join("cc"));
        assert_eq!(found.size, 10);
        assert!(found.modified_ns > 0);

        // A relative entry is under the working directory.
        let found = linker("cc", Some(OsStr::new("nope:second")), root).unwrap();
        assert_eq!(found.path, root.join("second/cc"));
        // A path is taken as it is, joined to the working directory when relative.
        let found = linker("first/cc", None, root).unwrap();
        assert_eq!(found.path, root.join("first/cc"));
        assert_eq!(found.size, 6);
        // Nothing to search, nothing found, a folder by that name.
        assert_eq!(linker("cc", None, root), None);
        assert_eq!(linker("missing", Some(&path), root), None);
        assert_eq!(linker("second", Some(root.as_os_str()), root), None);
        assert_eq!(linker("", Some(&path), root), None);
    }
}
