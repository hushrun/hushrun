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

use std::ffi::OsStr;
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
        let mut out = Vec::new();
        let mut iter = self.args.iter();
        while let Some(arg) = iter.next() {
            let value = match arg.strip_prefix("-B") {
                Some("") => iter.next().map(String::as_str),
                Some(rest)
                    if !rest.starts_with("static")
                        && !rest.starts_with("dynamic")
                        && !rest.starts_with("symbolic") =>
                {
                    Some(rest)
                }
                _ => None,
            };
            if let Some(value) = value {
                push_once(&mut out, cwd.join(value));
            }
        }
        out
    }
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

/// Finds the linker `-fuse-ld=<name>` makes the C driver run, as gcc and clang look for it: a
/// value with a `/` is a path; a name is `ld.<name>`, then `<name>`, first in each `-B` folder
/// and then on `path`. `None` when it can't be found: the link can't be keyed (B-18).
///
/// Apple's `/usr/bin/cc` is a shim that runs the selected Xcode's driver; its identity is the
/// shim's. The SDK's identity (its path and the date of its settings file) changes with an Xcode
/// update in practice, so the shim is left as it is.
pub fn fuse_linker(
    name: &str,
    prefixes: &[PathBuf],
    path: Option<&OsStr>,
    cwd: &Path,
) -> Option<Linker> {
    if name.is_empty() {
        return None;
    }
    if name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
        return stat(cwd.join(name), false);
    }
    let names = [format!("ld.{name}"), name.to_owned()];
    for program in &names {
        if let Some(found) = prefixes
            .iter()
            .find_map(|dir| stat(cwd.join(dir).join(program), true))
        {
            return Some(found);
        }
    }
    names.iter().find_map(|program| linker(program, path, cwd))
}

/// The files the linker may take a library named with `-l` (or `-framework`) from: every
/// candidate name in every search folder, in order (B-6). A name the linker finds elsewhere is
/// a system library, keyed by name with the SDK and the linker. `-l:file` names the file itself.
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

    #[cfg(unix)]
    #[test]
    // covers: B-18
    fn the_linker_fuse_ld_names_is_found_like_the_driver_finds_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let exec = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        exec(&root.join("gcc-ld/ld.lld"));
        exec(&root.join("bin/ld.lld"));
        exec(&root.join("bin/mold"));
        let path = root.join("bin");
        let prefixes = [root.join("gcc-ld")];
        let found = fuse_linker("lld", &prefixes, Some(path.as_os_str()), root).unwrap();
        assert_eq!(found.path, root.join("gcc-ld/ld.lld"));
        let found = fuse_linker("lld", &[], Some(path.as_os_str()), root).unwrap();
        assert_eq!(found.path, root.join("bin/ld.lld"));
        let found = fuse_linker("mold", &[], Some(path.as_os_str()), root).unwrap();
        assert_eq!(found.path, root.join("bin/mold"));
        let found = fuse_linker("bin/mold", &[], None, root).unwrap();
        assert_eq!(found.path, root.join("bin/mold"));
        assert_eq!(
            fuse_linker("gold", &prefixes, Some(path.as_os_str()), root),
            None
        );
        assert_eq!(
            fuse_linker("", &prefixes, Some(path.as_os_str()), root),
            None
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
