//! One compiler call: its arguments read and classified (B-4, B-5). Pure.
//!
//! [`parse`] reads what cargo passed the wrapper and either hands back an [`Invocation`] or
//! the reason the call runs exactly as asked. It applies every clause of B-4 that arguments and
//! variables can decide. The never-store list and the compiler's identity are the caller's.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::ops::Range;
use std::path::Path;

use crate::model::{Class, PassedThrough};

/// The variables cargo set for a call, captured once by the caller from the process.
///
/// Names that are not UTF-8 are dropped, except that one cargo sets for a crate (a keyed
/// `CARGO_` prefix) is remembered: it would belong in the key (B-5) and can't be written there,
/// so the call passes through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, OsString>,
    unreadable_cargo_name: bool,
}

impl Env {
    /// Builds a snapshot from name and value pairs, such as `std::env::vars_os()`.
    pub fn from_vars<I, K, V>(vars: I) -> Env
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let mut env = Env::default();
        for (name, value) in vars {
            let name: OsString = name.into();
            match name.into_string() {
                Ok(name) => {
                    env.vars.insert(name, value.into());
                }
                Err(name) => {
                    let bytes = name.as_encoded_bytes();
                    if CARGO_KEYED_PREFIXES
                        .iter()
                        .any(|prefix| bytes.starts_with(prefix.as_bytes()))
                    {
                        env.unreadable_cargo_name = true;
                    }
                }
            }
        }
        env
    }

    /// The value of one variable, if set.
    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(name).map(OsString::as_os_str)
    }

    /// Every name, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.vars.keys().map(String::as_str)
    }
}

/// What [`parse`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// A call the wrapper may key, store and reuse.
    Handled(Box<Invocation>),
    /// A call that runs exactly as asked, for this reason.
    PassedThrough(PassedThrough),
}

/// One kind of `--emit` the wrapper handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Emit {
    /// The `.d` file.
    DepInfo,
    /// The `.rmeta` file.
    Metadata,
    /// The linked output: `.rlib`, `.dylib` of a proc macro, or a binary.
    Link,
}

/// The kind of an `-L` search path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SearchKind {
    /// `dependency=`: where indirect dependencies are found.
    Dependency,
    /// `crate=`.
    Crate,
    /// `native=`: native libraries, often under a build script's output.
    Native,
    /// `framework=`.
    Framework,
    /// No kind, or `all=`.
    All,
}

impl SearchKind {
    /// The kind as `-L` writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            SearchKind::Dependency => "dependency",
            SearchKind::Crate => "crate",
            SearchKind::Native => "native",
            SearchKind::Framework => "framework",
            SearchKind::All => "all",
        }
    }
}

/// One `-L` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPath {
    /// The kind, `All` when none was written.
    pub kind: SearchKind,
    /// The folder, as given.
    pub path: String,
}

/// How a `-l` asks for a native library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NativeKind {
    /// `static=`: bundled into an rlib when the library is compiled.
    Static,
    /// `dylib=`, or no kind.
    Dylib,
    /// `framework=`: found in framework folders, as `<name>.framework`.
    Framework,
}

/// One `-l` whose file can be found the way the toolchain finds it (B-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLib {
    /// The kind.
    pub kind: NativeKind,
    /// The name, without `lib` or an extension.
    pub name: String,
}

/// One `--extern` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extern {
    /// The crate name it is known by.
    pub name: String,
    /// The options before the colon, as given: `priv`, `noprelude`, `nounused`, `force`.
    pub options: Vec<String>,
    /// The file passed, if any. `--extern proc_macro` has none.
    pub path: Option<String>,
}

impl Extern {
    /// The form the first key keeps (B-5): options and name, without the path.
    pub fn reduced(&self) -> String {
        if self.options.is_empty() {
            self.name.clone()
        } else {
            format!("{}:{}", self.options.join(","), self.name)
        }
    }
}

/// The values cargo sets that name the package and its folders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Package {
    /// `CARGO_PKG_NAME`.
    pub name: Option<String>,
    /// `CARGO_PKG_VERSION`.
    pub version: Option<String>,
    /// `CARGO_MANIFEST_DIR`.
    pub manifest_dir: Option<String>,
    /// `OUT_DIR`, set when the package has a build script.
    pub out_dir: Option<String>,
    /// `CARGO_TARGET_DIR`, when set to an absolute path. Not keyed, but a root (B-8): a
    /// location cargo hands a compile under it is recorded from it.
    pub target_dir: Option<String>,
    /// `CARGO_BUILD_TARGET_DIR`, when set to an absolute path; a root like `target_dir`.
    pub build_target_dir: Option<String>,
    /// `CARGO_TARGET_TMPDIR`, when set to an absolute path: an integration test's folder, under
    /// the target folder. A root (B-8).
    pub target_tmpdir: Option<String>,
    /// The folders the `CARGO_BIN_EXE_<name>` values are in, each once, in the order of the
    /// variables' names; only absolute values. Each is a root (B-8).
    pub bin_exe_dirs: Vec<String>,
}

/// What a call says about debug info, each the last one given: `-C debuginfo` (`-g` is `2`),
/// `-C split-debuginfo` and `-C strip`. The wrapper reads them to decide whether a program gets
/// a debug bundle (B-9).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugFlags {
    /// `-C debuginfo`.
    pub debuginfo: Option<String>,
    /// `-C split-debuginfo`.
    pub split: Option<String>,
    /// `-C strip`.
    pub strip: Option<String>,
}

impl DebugFlags {
    /// Whether the compile writes debug info: a `-C debuginfo` other than `0`, `none` or a
    /// spelling of "no". rustc's default is none.
    pub fn any(&self) -> bool {
        !matches!(
            self.debuginfo.as_deref(),
            None | Some("0" | "none" | "n" | "no" | "off" | "false")
        )
    }
}

/// One compiler call the wrapper handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    rustc: String,
    args: Vec<String>,
    tokens: Vec<Token>,
    crate_name: String,
    class: Class,
    emits: Vec<Emit>,
    out_dir: String,
    extra_filename: String,
    source: String,
    externs: Vec<Extern>,
    search_paths: Vec<SearchPath>,
    native: Vec<NativeLib>,
    native_certain: bool,
    incremental: Option<String>,
    target: Option<String>,
    searchable: bool,
    debug: DebugFlags,
    message_form: String,
    form: crate::messages::Form,
    cwd: String,
    package: Package,
    env: BTreeMap<String, String>,
}

/// One argument, or a flag and its value, as it sits in the call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    /// The indices in the arguments it spans.
    span: Range<usize>,
    /// What the first key keeps of it, if anything.
    key: Option<String>,
    /// Whether the compile the wrapper runs keeps it (B-9).
    in_command: bool,
    /// Whether it is the `--out-dir`.
    out_dir: bool,
}

/// The prefixes of the variables cargo sets for a crate's compile that B-5 keys. Cargo's own
/// configuration (`CARGO_TARGET_DIR`, `CARGO_HOME`, `CARGO_TERM_*`, `CARGO_PROFILE_*`, tokens,
/// …) is not here: it reaches rustc only through arguments already keyed, and a token must
/// never reach a key.
const CARGO_KEYED_PREFIXES: [&str; 3] = ["CARGO_PKG_", "CARGO_MANIFEST_", "CARGO_BIN_EXE_"];

/// The other variables cargo sets for a crate's compile that B-5 keys. `CARGO`,
/// `CARGO_MAKEFLAGS`, `CARGO_PRIMARY_PACKAGE`, `CARGO_SBOM_PATH` and `CARGO_RUSTC_CURRENT_DIR`
/// are left out: they don't reach the output.
const CARGO_KEYED: [&str; 3] = ["CARGO_CRATE_NAME", "CARGO_BIN_NAME", "CARGO_TARGET_TMPDIR"];

/// Variables outside `CARGO_*` that B-5 keys: rustc reads them without an `env-dep` line
/// (rustc 1.94: the deployment targets, `RUSTC_FORCE_RUSTC_VERSION`, `LLVM_OVERRIDE_PRODUCER`,
/// `RUST_TARGET_PATH`), the C driver and the linker read the next five, and compilers and build
/// tools take "now" from `SOURCE_DATE_EPOCH` when it is set, so it reaches anything that embeds a
/// date.
const OTHER_KEYED: [&str; 16] = [
    "RUSTC_BOOTSTRAP",
    "MACOSX_DEPLOYMENT_TARGET",
    "IPHONEOS_DEPLOYMENT_TARGET",
    "TVOS_DEPLOYMENT_TARGET",
    "WATCHOS_DEPLOYMENT_TARGET",
    "XROS_DEPLOYMENT_TARGET",
    "SDKROOT",
    "RUSTC_FORCE_RUSTC_VERSION",
    "LLVM_OVERRIDE_PRODUCER",
    "RUST_TARGET_PATH",
    "LIBRARY_PATH",
    "LD_RUN_PATH",
    "DEVELOPER_DIR",
    "COMPILER_PATH",
    "GCC_EXEC_PREFIX",
    "SOURCE_DATE_EPOCH",
];

/// The variables of [`OTHER_KEYED`] that only the C driver (`cc`) and the linker read, or that
/// rustc reads only to link (the SDK: `SDKROOT`, `DEVELOPER_DIR`). A library's compile links
/// nothing (rustc writes the `.rlib` and `.rmeta` itself), so they are keyed for linked outputs
/// only: on macOS a library's bytes are the same under two SDKs and both developer folders.
const LINKER_ONLY: [&str; 6] = [
    "LIBRARY_PATH",
    "LD_RUN_PATH",
    "COMPILER_PATH",
    "GCC_EXEC_PREFIX",
    "SDKROOT",
    "DEVELOPER_DIR",
];

/// The `-Z` flags a call may carry and still be handled: each reads no file, and its text is
/// keyed. Every other `-Z` passes through (B-4).
const UNSTABLE_KEYED: [&str; 5] = [
    "unstable-options",
    "threads",
    "share-generics",
    "macro-backtrace",
    "debuginfo-compression",
];

/// Whether a variable cargo sets for a crate belongs in the first key (B-5).
fn cargo_keyed(name: &str) -> bool {
    CARGO_KEYED.contains(&name)
        || CARGO_KEYED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Reads one call. `argv` is what cargo passed after the wrapper's own name: the compiler's
/// path, then its arguments. `env` is the call's variables and `cwd` its working directory.
pub fn parse(argv: &[OsString], env: &Env, cwd: &Path) -> Parsed {
    match parse_inner(argv, env, cwd) {
        Ok(invocation) => Parsed::Handled(Box::new(invocation)),
        Err(why) => Parsed::PassedThrough(why),
    }
}

fn parse_inner(argv: &[OsString], env: &Env, cwd: &Path) -> Result<Invocation, PassedThrough> {
    let (compiler, rest) = argv.split_first().ok_or(PassedThrough::NotRustc)?;
    if !is_rustc(compiler) {
        return Err(PassedThrough::NotRustc);
    }
    let rustc = utf8(compiler)?;
    let args = rest.iter().map(utf8).collect::<Result<Vec<_>, _>>()?;
    let cwd = cwd.to_str().ok_or(PassedThrough::NotUtf8)?.to_string();

    let raw = tokenize(&args)?;
    let mut seen = Seen::default();
    let mut tokens = Vec::with_capacity(raw.len());
    for token in raw {
        tokens.push(seen.read(token)?);
    }

    let crate_name = seen.crate_name.ok_or(PassedThrough::NoCrateName)?;
    if seen.crate_types.len() > 1 {
        return Err(PassedThrough::CrateTypes);
    }
    let class = classify(&crate_name, seen.crate_types.first(), seen.test)?;
    let out_dir = seen.out_dir.ok_or(PassedThrough::NoOutDir)?;
    let extra_filename = seen.extra_filename.ok_or(PassedThrough::NoExtraFilename)?;
    if !seen
        .json
        .iter()
        .flat_map(|value| value.split(','))
        .any(|kind| kind == "artifacts")
    {
        return Err(PassedThrough::NoArtifacts);
    }
    let source = seen.source.ok_or(PassedThrough::Malformed)?;
    if seen.emits.is_empty() {
        seen.emits.push(Emit::Link);
    }

    let expands = seen.externs.iter().any(|e| {
        e.path
            .as_deref()
            .is_some_and(|path| crate::inputs::is_linked_library(Path::new(path)))
    });
    let (keyed_env, package) = read_env(env, class, expands)?;
    let form = crate::messages::Form::new(
        seen.error_format.as_deref(),
        (!seen.json.is_empty())
            .then(|| seen.json.join(","))
            .as_deref(),
        seen.color.as_deref(),
    );
    let message_form = serde_json::Value::Array(
        [
            seen.error_format,
            (!seen.json.is_empty()).then(|| seen.json.join(",")),
            seen.color,
            seen.diagnostic_width,
        ]
        .into_iter()
        .map(|value| value.map_or(serde_json::Value::Null, serde_json::Value::String))
        .collect(),
    )
    .to_string();

    Ok(Invocation {
        rustc,
        args,
        tokens,
        crate_name,
        class,
        emits: seen.emits,
        out_dir,
        extra_filename,
        source,
        externs: seen.externs,
        search_paths: seen.search_paths,
        native: seen.native,
        native_certain: seen.native_certain,
        incremental: seen.incremental,
        target: seen.target,
        searchable: seen.searchable,
        debug: seen.debug,
        message_form,
        form,
        cwd,
        package,
        env: keyed_env,
    })
}

impl Invocation {
    /// The compiler's path, as cargo gave it.
    pub fn rustc(&self) -> &str {
        &self.rustc
    }

    /// The arguments after the compiler's path, as cargo gave them.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// `--crate-name`.
    pub fn crate_name(&self) -> &str {
        &self.crate_name
    }

    /// What the call produces.
    pub fn class(&self) -> Class {
        self.class
    }

    /// The emit kinds, in the order given. Without `--emit`, rustc's default: `link`.
    pub fn emits(&self) -> &[Emit] {
        &self.emits
    }

    /// `--out-dir`.
    pub fn out_dir(&self) -> &str {
        &self.out_dir
    }

    /// `-C extra-filename`.
    pub fn extra_filename(&self) -> &str {
        &self.extra_filename
    }

    /// The source file, as given: relative to the working directory for a workspace member.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Every `--extern`, in order.
    pub fn externs(&self) -> &[Extern] {
        &self.externs
    }

    /// Every `-L`, in order.
    pub fn search_paths(&self) -> &[SearchPath] {
        &self.search_paths
    }

    /// Every `-l` whose file can be found, in order (B-6).
    pub fn native_libs(&self) -> &[NativeLib] {
        &self.native
    }

    /// Whether every `-l` is in a form whose file can be found with certainty. Not with
    /// `+verbatim`, a rename, `:file` or a kind this crate doesn't know (B-6).
    pub fn native_certain(&self) -> bool {
        self.native_certain
    }

    /// The folders searched for a native library of `kind`, in order: `native=`, `all=` and
    /// bare ones, and `framework=` ones for a framework, as rustc's `PathKind` matches them.
    pub fn native_dirs(&self, kind: NativeKind) -> impl Iterator<Item = &str> {
        self.search_paths
            .iter()
            .filter(move |search| match search.kind {
                SearchKind::All => true,
                SearchKind::Native => kind != NativeKind::Framework,
                SearchKind::Framework => kind == NativeKind::Framework,
                SearchKind::Dependency | SearchKind::Crate => false,
            })
            .map(|search| search.path.as_str())
    }

    /// The `-L dependency=` folders, in order: the `Deps` roots.
    pub fn dependency_dirs(&self) -> impl Iterator<Item = &str> {
        self.search_paths
            .iter()
            .filter(|search| search.kind == SearchKind::Dependency)
            .map(|search| search.path.as_str())
    }

    /// `-C incremental`, which the wrapper leaves out (B-9).
    pub fn incremental(&self) -> Option<&str> {
        self.incremental.as_deref()
    }

    /// `--target`, a triple.
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    /// Whether outputs can be searched for roots as raw bytes (B-10). Not with `-C lto`,
    /// `-C linker-plugin-lto`, `-C embed-bitcode=yes` or compressed debug sections.
    pub fn searchable(&self) -> bool {
        self.searchable
    }

    /// What the call says about debug info (B-9).
    pub fn debug(&self) -> &DebugFlags {
        &self.debug
    }

    /// The form of the compiler's messages (`--error-format`, `--json`, `--color`,
    /// `--diagnostic-width`) as one comparable text (B-12).
    pub fn message_form(&self) -> &str {
        &self.message_form
    }

    /// The form messages are kept and served in (B-12): `--error-format`, `--json` and
    /// `--color`, without the width.
    pub fn form(&self) -> &crate::messages::Form {
        &self.form
    }

    /// The name every output of the call shares: the crate name and `-C extra-filename`.
    pub fn file_stem(&self) -> String {
        format!("{}{}", self.crate_name, self.extra_filename)
    }

    /// The dep-info file rustc writes for the call: `<out dir>/<crate name><extra>.d`.
    pub fn dep_info_path(&self) -> std::path::PathBuf {
        let dir = Path::new(&self.out_dir);
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            Path::new(&self.cwd).join(dir)
        };
        dir.join(format!("{}.d", self.file_stem()))
    }

    /// rustc's working directory.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// The package's name, version and folders, from cargo's variables.
    pub fn package(&self) -> &Package {
        &self.package
    }

    /// B-5's arguments, in order: without `--out-dir`, `-L dependency=`, `-C incremental=`,
    /// `--error-format`, `--json`, `--color` and `--diagnostic-width`, each `--extern` reduced
    /// to its options and name. Each flag is written one way whatever its spelling, so
    /// `-C x=y`, `-Cx=y` and `--codegen=x=y` key alike. Roots are still in place:
    /// [`crate::roots::Roots::to_placeholders`] replaces them.
    pub fn key_args(&self) -> Vec<String> {
        self.tokens
            .iter()
            .filter_map(|token| token.key.clone())
            .collect()
    }

    /// B-5's variables from the call: the ones cargo sets for a crate (`CARGO_PKG_*`,
    /// `CARGO_MANIFEST_*`, `CARGO_BIN_EXE_*`, `CARGO_CRATE_NAME`, `CARGO_BIN_NAME`,
    /// `CARGO_TARGET_TMPDIR`) and the fixed list of others rustc, the C driver and the linker
    /// read, when set. Roots are still in place.
    pub fn key_env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    /// The first key's variables with `extra` added: the person's extra names and the ones
    /// the build script set with `cargo:rustc-env`. An unset name is left out. A value that is
    /// not UTF-8 is an error, which the wrapper turns into a pass-through.
    pub fn key_env_with(
        &self,
        env: &Env,
        extra: &[String],
    ) -> Result<BTreeMap<String, String>, PassedThrough> {
        let mut out = self.env.clone();
        for name in extra {
            if let Some(value) = env.get(name) {
                out.insert(name.clone(), utf8(value)?);
            }
        }
        Ok(out)
    }

    /// The arguments to run rustc with (B-9): everything as given, minus `-C incremental=…`,
    /// then `extra` (the remap flags, and `--print=link-args` for a program).
    pub fn command(&self, extra: &[OsString]) -> Vec<OsString> {
        let mut out = Vec::with_capacity(self.args.len() + extra.len());
        for token in self.tokens.iter().filter(|token| token.in_command) {
            out.extend(self.args[token.span.clone()].iter().map(OsString::from));
        }
        out.extend(extra.iter().cloned());
        out
    }

    /// [`Invocation::command`], with the outputs written to `out_dir` instead (B-10's second
    /// compile).
    pub fn command_in(&self, extra: &[OsString], out_dir: &Path) -> Vec<OsString> {
        let mut out = Vec::with_capacity(self.args.len() + extra.len() + 1);
        for token in self.tokens.iter().filter(|token| token.in_command) {
            if token.out_dir {
                out.push(OsString::from("--out-dir"));
                out.push(out_dir.as_os_str().to_os_string());
            } else {
                out.extend(self.args[token.span.clone()].iter().map(OsString::from));
            }
        }
        out.extend(extra.iter().cloned());
        out
    }
}

fn is_rustc(path: &OsStr) -> bool {
    let name = Path::new(path).file_name();
    name == Some(OsStr::new("rustc")) || name == Some(OsStr::new("rustc.exe"))
}

fn utf8(text: impl AsRef<OsStr>) -> Result<String, PassedThrough> {
    text.as_ref()
        .to_str()
        .map(str::to_string)
        .ok_or(PassedThrough::NotUtf8)
}

/// What an argument means before its value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flag {
    Source,
    CrateName,
    Edition,
    ErrorFormat,
    Json,
    Color,
    DiagnosticWidth,
    CrateType,
    Emit,
    OutDir,
    Extern,
    Cfg,
    CheckCfg,
    CapLints,
    Target,
    /// A lint level, by its long name.
    Lint(&'static str),
    Codegen,
    Unstable,
    Search,
    Library,
    Test,
    Optimize,
    Debug,
}

/// A flag and its value, before they are read.
struct Raw {
    flag: Flag,
    value: String,
    span: Range<usize>,
}

/// How a long flag behaves.
enum Long {
    Bare(Flag),
    Valued(Flag),
    Refuse(PassedThrough),
}

fn long_flag(name: &str) -> Long {
    match name {
        "test" => Long::Bare(Flag::Test),
        "crate-name" => Long::Valued(Flag::CrateName),
        "edition" => Long::Valued(Flag::Edition),
        "error-format" => Long::Valued(Flag::ErrorFormat),
        "json" => Long::Valued(Flag::Json),
        "color" => Long::Valued(Flag::Color),
        "diagnostic-width" => Long::Valued(Flag::DiagnosticWidth),
        "crate-type" => Long::Valued(Flag::CrateType),
        "emit" => Long::Valued(Flag::Emit),
        "out-dir" => Long::Valued(Flag::OutDir),
        "extern" => Long::Valued(Flag::Extern),
        "cfg" => Long::Valued(Flag::Cfg),
        "check-cfg" => Long::Valued(Flag::CheckCfg),
        "cap-lints" => Long::Valued(Flag::CapLints),
        "target" => Long::Valued(Flag::Target),
        "sysroot" => Long::Refuse(PassedThrough::Sysroot),
        "allow" => Long::Valued(Flag::Lint("allow")),
        "warn" => Long::Valued(Flag::Lint("warn")),
        "deny" => Long::Valued(Flag::Lint("deny")),
        "forbid" => Long::Valued(Flag::Lint("forbid")),
        "force-warn" => Long::Valued(Flag::Lint("force-warn")),
        "codegen" => Long::Valued(Flag::Codegen),
        "print" | "version" | "help" | "explain" => Long::Refuse(PassedThrough::Probe),
        "remap-path-prefix" => Long::Refuse(PassedThrough::CallerRemap),
        _ => Long::Refuse(PassedThrough::UnknownArgument),
    }
}

/// Splits the arguments into flags and values. Every spelling of a flag gives the same flag.
fn tokenize(args: &[String]) -> Result<Vec<Raw>, PassedThrough> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let start = i;
        let arg = args[i].as_str();
        i += 1;
        // The value of a flag written apart: the next argument.
        let mut next = || -> Result<String, PassedThrough> {
            let value = args.get(i).ok_or(PassedThrough::Malformed)?.clone();
            i += 1;
            Ok(value)
        };
        if arg == "-" {
            return Err(PassedThrough::Probe);
        }
        if arg.starts_with('@') {
            return Err(PassedThrough::ArgFile);
        }
        let (flag, value) = if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            match long_flag(name) {
                Long::Bare(flag) => {
                    if inline.is_some() {
                        return Err(PassedThrough::Malformed);
                    }
                    (flag, String::new())
                }
                Long::Valued(flag) => match inline {
                    Some(value) => (flag, value),
                    None => (flag, next()?),
                },
                Long::Refuse(why) => return Err(why),
            }
        } else if let Some(short) = arg.strip_prefix('-') {
            let mut chars = short.chars();
            let letter = chars.next().ok_or(PassedThrough::Malformed)?;
            let attached = chars.as_str();
            let valued = |flag: Flag, next: &mut dyn FnMut() -> Result<String, PassedThrough>| {
                if attached.is_empty() {
                    next().map(|value| (flag, value))
                } else {
                    Ok((flag, attached.to_string()))
                }
            };
            match letter {
                'C' => valued(Flag::Codegen, &mut next)?,
                'L' => valued(Flag::Search, &mut next)?,
                'l' => valued(Flag::Library, &mut next)?,
                'Z' => valued(Flag::Unstable, &mut next)?,
                'A' => valued(Flag::Lint("allow"), &mut next)?,
                'W' => valued(Flag::Lint("warn"), &mut next)?,
                'D' => valued(Flag::Lint("deny"), &mut next)?,
                'F' => valued(Flag::Lint("forbid"), &mut next)?,
                'o' => return Err(PassedThrough::OutputFile),
                'O' if attached.is_empty() => (Flag::Optimize, String::new()),
                'g' if attached.is_empty() => (Flag::Debug, String::new()),
                'V' | 'h' => return Err(PassedThrough::Probe),
                'v' if attached == "V" => return Err(PassedThrough::Probe),
                _ => return Err(PassedThrough::UnknownArgument),
            }
        } else {
            (Flag::Source, arg.to_string())
        };
        out.push(Raw {
            flag,
            value,
            span: start..i,
        });
    }
    Ok(out)
}

/// What the arguments said so far.
struct Seen {
    crate_name: Option<String>,
    crate_types: Vec<String>,
    test: bool,
    emits: Vec<Emit>,
    out_dir: Option<String>,
    extra_filename: Option<String>,
    source: Option<String>,
    externs: Vec<Extern>,
    search_paths: Vec<SearchPath>,
    native: Vec<NativeLib>,
    native_certain: bool,
    incremental: Option<String>,
    target: Option<String>,
    searchable: bool,
    debug: DebugFlags,
    error_format: Option<String>,
    json: Vec<String>,
    color: Option<String>,
    diagnostic_width: Option<String>,
}

impl Default for Seen {
    fn default() -> Self {
        Seen {
            crate_name: None,
            crate_types: Vec::new(),
            test: false,
            emits: Vec::new(),
            out_dir: None,
            extra_filename: None,
            source: None,
            externs: Vec::new(),
            search_paths: Vec::new(),
            native: Vec::new(),
            native_certain: true,
            incremental: None,
            target: None,
            searchable: true,
            debug: DebugFlags::default(),
            error_format: None,
            json: Vec::new(),
            color: None,
            diagnostic_width: None,
        }
    }
}

/// Sets a value that may be given once. A second one is a call this crate won't guess about.
fn once(slot: &mut Option<String>, value: &str) -> Result<(), PassedThrough> {
    if slot.is_some() {
        return Err(PassedThrough::Malformed);
    }
    *slot = Some(value.to_string());
    Ok(())
}

/// Whether a `-C` switch's value turns it off. Without a value it is on.
fn switched_off(value: Option<&str>) -> bool {
    matches!(value, Some("no" | "n" | "off" | "false"))
}

impl Seen {
    /// Reads one flag: records what it says and decides what the key and the command keep.
    fn read(&mut self, raw: Raw) -> Result<Token, PassedThrough> {
        let Raw { flag, value, span } = raw;
        let v = value.as_str();
        let mut in_command = true;
        let key = match flag {
            Flag::Source => {
                once(&mut self.source, v)?;
                Some(value.clone())
            }
            Flag::CrateName => {
                once(&mut self.crate_name, v)?;
                Some(format!("--crate-name={v}"))
            }
            Flag::Edition => Some(format!("--edition={v}")),
            Flag::ErrorFormat => {
                once(&mut self.error_format, v)?;
                None
            }
            Flag::Json => {
                self.json.push(value.clone());
                None
            }
            Flag::Color => {
                once(&mut self.color, v)?;
                None
            }
            Flag::DiagnosticWidth => {
                once(&mut self.diagnostic_width, v)?;
                None
            }
            Flag::CrateType => {
                for kind in v.split(',') {
                    self.crate_types.push(kind.to_string());
                }
                Some(format!("--crate-type={v}"))
            }
            Flag::Emit => {
                for kind in v.split(',') {
                    let emit = match kind {
                        "dep-info" => Emit::DepInfo,
                        "metadata" => Emit::Metadata,
                        "link" => Emit::Link,
                        _ => return Err(PassedThrough::Emit),
                    };
                    if !self.emits.contains(&emit) {
                        self.emits.push(emit);
                    }
                }
                Some(format!("--emit={v}"))
            }
            Flag::OutDir => {
                once(&mut self.out_dir, v)?;
                None
            }
            Flag::Extern => {
                let parsed = parse_extern(v)?;
                if parsed.path.is_none() && parsed.name != "proc_macro" {
                    return Err(PassedThrough::ExternWithoutPath);
                }
                let key = format!("--extern={}", parsed.reduced());
                self.externs.push(parsed);
                Some(key)
            }
            Flag::Cfg => Some(format!("--cfg={v}")),
            Flag::CheckCfg => Some(format!("--check-cfg={v}")),
            Flag::CapLints => Some(format!("--cap-lints={v}")),
            Flag::Target => {
                if v.ends_with(".json") || v.contains('/') || v.contains('\\') {
                    return Err(PassedThrough::TargetFile);
                }
                once(&mut self.target, v)?;
                Some(format!("--target={v}"))
            }
            Flag::Lint(level) => Some(format!("--{level}={v}")),
            Flag::Codegen => {
                let (name, setting) = match v.split_once('=') {
                    Some((name, setting)) => (name, Some(setting)),
                    None => (v, None),
                };
                match name {
                    "instrument-coverage" | "profile-generate" | "profile-use" => {
                        return Err(PassedThrough::Instrumented);
                    }
                    "incremental" => {
                        once(
                            &mut self.incremental,
                            setting.ok_or(PassedThrough::Malformed)?,
                        )?;
                        in_command = false;
                    }
                    "extra-filename" => {
                        once(
                            &mut self.extra_filename,
                            setting.ok_or(PassedThrough::Malformed)?,
                        )?;
                    }
                    "lto" | "linker-plugin-lto" | "embed-bitcode" if !switched_off(setting) => {
                        self.searchable = false;
                    }
                    "link-arg" | "link-args" => {
                        if compresses_debug(setting.unwrap_or_default(), name == "link-args") {
                            self.searchable = false;
                        }
                    }
                    "debuginfo" => self.debug.debuginfo = setting.map(str::to_string),
                    "split-debuginfo" => self.debug.split = setting.map(str::to_string),
                    "strip" => self.debug.strip = setting.map(str::to_string),
                    _ => {}
                }
                (name != "incremental").then(|| format!("-C{v}"))
            }
            Flag::Unstable => {
                let name = v.split_once('=').map_or(v, |(name, _)| name);
                if matches!(name, "remap-cwd-prefix" | "remap-path-scope") {
                    return Err(PassedThrough::CallerRemap);
                }
                if !UNSTABLE_KEYED.contains(&name) {
                    return Err(PassedThrough::Unstable);
                }
                if let Some(kind) = v.strip_prefix("debuginfo-compression=")
                    && kind != "none"
                {
                    self.searchable = false;
                }
                Some(format!("-Z{v}"))
            }
            Flag::Search => {
                let search = parse_search(v);
                let key = (search.kind != SearchKind::Dependency)
                    .then(|| format!("-L{}={}", search.kind.as_str(), search.path));
                self.search_paths.push(search);
                key
            }
            Flag::Library => {
                match parse_native(v) {
                    Some(lib) => self.native.push(lib),
                    None => self.native_certain = false,
                }
                Some(format!("-l{v}"))
            }
            Flag::Test => {
                self.test = true;
                Some("--test".to_string())
            }
            Flag::Optimize => Some("-O".to_string()),
            Flag::Debug => {
                self.debug.debuginfo = Some("2".to_string());
                Some("-g".to_string())
            }
        };
        Ok(Token {
            span,
            key,
            in_command,
            out_dir: flag == Flag::OutDir,
        })
    }
}

/// Whether a `-C link-arg` (or the words of a `-C link-args`) asks the C driver or the linker
/// to compress debug sections: `-gz` or `-gz=<kind>`, or `--compress-debug-sections` in any
/// spelling, with any value but `none` (B-10). A flag whose value can't be seen counts.
fn compresses_debug(value: &str, words: bool) -> bool {
    let parts: Vec<&str> = if words {
        value.split_whitespace().collect()
    } else {
        vec![value]
    };
    let mut tokens: Vec<&str> = Vec::new();
    for part in parts {
        match part.strip_prefix("-Wl,") {
            Some(rest) => tokens.extend(rest.split(',')),
            None => tokens.push(part),
        }
    }
    tokens.iter().enumerate().any(|(i, token)| {
        if *token == "-gz" {
            return true;
        }
        if let Some(kind) = token.strip_prefix("-gz=") {
            return kind != "none";
        }
        let flag = token.trim_start_matches('-');
        if token.starts_with('-') && flag.starts_with("compress-debug-sections") {
            return match flag.strip_prefix("compress-debug-sections") {
                Some("") => tokens.get(i + 1) != Some(&"none"),
                Some(rest) => rest != "=none",
                None => true,
            };
        }
        false
    })
}

/// Reads `-l [KIND[:MODIFIERS]=]NAME[:RENAME]` as rustc does. `None` for a form whose file
/// can't be found with certainty: `+verbatim`, a rename or `:file`, `link-arg=`, a kind or
/// modifier this crate doesn't know.
fn parse_native(value: &str) -> Option<NativeLib> {
    let (spec, name) = match value.split_once('=') {
        Some((spec, name)) => (Some(spec), name),
        None => (None, value),
    };
    let (kind, modifiers) = match spec {
        None => ("dylib", None),
        Some(spec) => match spec.split_once(':') {
            Some((kind, modifiers)) => (kind, Some(modifiers)),
            None => (spec, None),
        },
    };
    let kind = match kind {
        "static" => NativeKind::Static,
        "dylib" => NativeKind::Dylib,
        "framework" => NativeKind::Framework,
        _ => return None,
    };
    for modifier in modifiers.into_iter().flat_map(|m| m.split(',')) {
        match modifier {
            "+bundle" | "-bundle" | "+whole-archive" | "-whole-archive" | "+as-needed"
            | "-as-needed" | "-verbatim" => {}
            _ => return None,
        }
    }
    if name.is_empty() || name.contains([':', '/', '\\']) {
        return None;
    }
    Some(NativeLib {
        kind,
        name: name.to_string(),
    })
}

/// Reads `[opt,opt:]name[=path]`.
fn parse_extern(value: &str) -> Result<Extern, PassedThrough> {
    let (spec, path) = match value.split_once('=') {
        Some((spec, path)) => (spec, Some(path.to_string())),
        None => (value, None),
    };
    let (options, name) = match spec.split_once(':') {
        Some((options, name)) => (
            options.split(',').map(str::to_string).collect::<Vec<_>>(),
            name,
        ),
        None => (Vec::new(), spec),
    };
    let known = ["priv", "noprelude", "nounused", "force"];
    if name.is_empty()
        || options
            .iter()
            .any(|option| !known.contains(&option.as_str()))
    {
        return Err(PassedThrough::UnknownArgument);
    }
    Ok(Extern {
        name: name.to_string(),
        options,
        path,
    })
}

/// Reads `[kind=]path` as rustc does: a prefix that is not a kind is part of the path.
fn parse_search(value: &str) -> SearchPath {
    let kinds = [
        SearchKind::Dependency,
        SearchKind::Crate,
        SearchKind::Native,
        SearchKind::Framework,
        SearchKind::All,
    ];
    for kind in kinds {
        if let Some(path) = value
            .strip_prefix(kind.as_str())
            .and_then(|rest| rest.strip_prefix('='))
        {
            return SearchPath {
                kind,
                path: path.to_string(),
            };
        }
    }
    SearchPath {
        kind: SearchKind::All,
        path: value.to_string(),
    }
}

fn classify(name: &str, crate_type: Option<&String>, test: bool) -> Result<Class, PassedThrough> {
    if test {
        return Ok(Class::Program);
    }
    match crate_type.map(String::as_str) {
        None => Err(PassedThrough::NoCrateType),
        Some("lib" | "rlib") => Ok(Class::Library),
        Some("proc-macro") => Ok(Class::ProcMacro),
        Some("bin") if name.starts_with("build_script_") => Ok(Class::BuildScript),
        Some("bin") => Ok(Class::Program),
        Some(_) => Err(PassedThrough::CrateType),
    }
}

/// The variables B-5 keys, and the package's folders. `expands` says whether the compile takes a
/// proc macro (an `--extern` that names a linked library).
fn read_env(
    env: &Env,
    class: Class,
    expands: bool,
) -> Result<(BTreeMap<String, String>, Package), PassedThrough> {
    if env.unreadable_cargo_name {
        return Err(PassedThrough::NotUtf8);
    }
    let mut keyed = BTreeMap::new();
    for (name, value) in &env.vars {
        // A library is written by rustc itself: no C driver or linker runs for it, so what only
        // they read is not its input, and would tie its key to how this machine is set up.
        let linker_only = class == Class::Library && LINKER_ONLY.contains(&name.as_str());
        // rustc never reads `SOURCE_DATE_EPOCH`; a proc macro runs inside it and may. A library
        // that expands none is the same compile under any value.
        let dateless = class == Class::Library && !expands && name == "SOURCE_DATE_EPOCH";
        if (cargo_keyed(name) || OTHER_KEYED.contains(&name.as_str())) && !linker_only && !dateless
        {
            keyed.insert(name.clone(), utf8(value)?);
        }
    }
    // The locations cargo or the person gave, each a root of its own (B-8). Only absolute
    // values: a relative one names no folder by itself, and is keyed as written.
    let absolute = |value: Option<&str>| value.filter(|v| v.starts_with('/')).map(str::to_string);
    let mut bin_exe_dirs: Vec<String> = Vec::new();
    for (name, value) in &keyed {
        if !name.starts_with("CARGO_BIN_EXE_") {
            continue;
        }
        let Some(dir) = absolute(Some(value)).and_then(|v| {
            Path::new(&v)
                .parent()
                .and_then(|p| p.to_str())
                .map(str::to_string)
        }) else {
            continue;
        };
        if !bin_exe_dirs.contains(&dir) {
            bin_exe_dirs.push(dir);
        }
    }
    let package = Package {
        name: keyed.get("CARGO_PKG_NAME").cloned(),
        version: keyed.get("CARGO_PKG_VERSION").cloned(),
        manifest_dir: keyed.get("CARGO_MANIFEST_DIR").cloned(),
        out_dir: env.get("OUT_DIR").map(utf8).transpose()?,
        target_dir: absolute(env.get("CARGO_TARGET_DIR").and_then(OsStr::to_str)),
        build_target_dir: absolute(env.get("CARGO_BUILD_TARGET_DIR").and_then(OsStr::to_str)),
        target_tmpdir: absolute(keyed.get("CARGO_TARGET_TMPDIR").map(String::as_str)),
        bin_exe_dirs,
    };
    Ok((keyed, package))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Splits a command line as cargo's `-v` prints it: words, single quotes, `'\''`.
    pub(crate) fn words(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut word = String::new();
        let mut in_word = false;
        let mut quoted = false;
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                '\'' => {
                    quoted = !quoted;
                    in_word = true;
                }
                '\\' if !quoted => {
                    word.extend(chars.next());
                    in_word = true;
                }
                c if c.is_whitespace() && !quoted => {
                    if in_word {
                        out.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                c => {
                    word.push(c);
                    in_word = true;
                }
            }
        }
        if in_word {
            out.push(word);
        }
        out
    }

    pub(crate) fn argv(line: &str) -> Vec<OsString> {
        words(line).into_iter().map(OsString::from).collect()
    }

    pub(crate) const RUSTC: &str =
        "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/bin/rustc";
    pub(crate) const WS: &str = "/home/dev/ws";
    pub(crate) const REGISTRY: &str =
        "/home/dev/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f";

    /// `cargo build -v` of a workspace library, with `-C incremental`.
    pub(crate) fn workspace_lib() -> (Vec<OsString>, Env) {
        let line = format!(
            "{RUSTC} --crate-name hushrun_build --edition=2024 crates/hushrun-build/src/lib.rs \
             --error-format=json --json=diagnostic-rendered-ansi,artifacts,future-incompat \
             --crate-type lib --emit=dep-info,metadata,link -C embed-bitcode=no -C debuginfo=2 \
             -C split-debuginfo=unpacked --forbid=unsafe_code --allow=unreachable_pub \
             '--deny=clippy::todo' '--deny=clippy::dbg_macro' --check-cfg 'cfg(docsrs,test)' \
             --check-cfg 'cfg(feature, values())' -C metadata=4409a3f27f06eee7 \
             -C extra-filename=-55ed56aeb232fcd2 --out-dir {WS}/target/debug/deps \
             -C incremental={WS}/target/debug/incremental -L dependency={WS}/target/debug/deps \
             --extern hushrun_core={WS}/target/debug/deps/libhushrun_core-f54f7763168a4211.rmeta \
             --extern memchr={WS}/target/debug/deps/libmemchr-caca413160791bee.rmeta"
        );
        let env = Env::from_vars([
            (
                "CARGO",
                "/home/dev/.rustup/toolchains/1.94.0-aarch64-apple-darwin/bin/cargo",
            ),
            ("CARGO_CRATE_NAME", "hushrun_build"),
            ("CARGO_MANIFEST_DIR", "/home/dev/ws/crates/hushrun-build"),
            (
                "CARGO_MANIFEST_PATH",
                "/home/dev/ws/crates/hushrun-build/Cargo.toml",
            ),
            ("CARGO_PKG_NAME", "hushrun-build"),
            ("CARGO_PKG_VERSION", "0.12.0"),
            ("CARGO_PRIMARY_PACKAGE", "1"),
            ("CARGO_MAKEFLAGS", "-j --jobserver-fds=3,4"),
            (
                "DYLD_FALLBACK_LIBRARY_PATH",
                "/home/dev/ws/target/debug/deps",
            ),
            ("HUSHRUN_NO_UPDATE_CHECK", "1"),
            ("HOME", "/home/dev"),
        ]);
        (argv(&line), env)
    }

    /// A registry library whose build script set `OUT_DIR`.
    pub(crate) fn registry_lib() -> (Vec<OsString>, Env) {
        let line = format!(
            "{RUSTC} --crate-name serde_core --edition=2021 {REGISTRY}/serde_core-1.0.229/src/lib.rs \
             --error-format=json --json=diagnostic-rendered-ansi,artifacts,future-incompat \
             --crate-type lib --emit=dep-info,metadata,link -C embed-bitcode=no -C debuginfo=2 \
             -C split-debuginfo=unpacked --cfg 'feature=\"default\"' --cfg 'feature=\"std\"' \
             --check-cfg 'cfg(docsrs,test)' -C metadata=a10b39fce7bf852f \
             -C extra-filename=-6a3c481eef3a1df8 --out-dir {WS}/target/debug/deps \
             -L dependency={WS}/target/debug/deps --cap-lints warn \
             --check-cfg 'cfg(no_core_cstr)'"
        );
        let env = Env::from_vars([
            ("CARGO_CRATE_NAME", "serde_core".to_string()),
            (
                "CARGO_MANIFEST_DIR",
                format!("{REGISTRY}/serde_core-1.0.229"),
            ),
            ("CARGO_PKG_NAME", "serde_core".to_string()),
            ("CARGO_PKG_VERSION", "1.0.229".to_string()),
            (
                "OUT_DIR",
                format!("{WS}/target/debug/build/serde_core-db522bc2a1d3eab6/out"),
            ),
        ]);
        (argv(&line), env)
    }

    /// A proc macro: serde_derive.
    pub(crate) fn proc_macro() -> Vec<OsString> {
        argv(&format!(
            "{RUSTC} --crate-name serde_derive --edition=2021 {REGISTRY}/serde_derive-1.0.229/src/lib.rs \
             --error-format=json --json=diagnostic-rendered-ansi,artifacts,future-incompat \
             --crate-type proc-macro --emit=dep-info,link -C prefer-dynamic -C embed-bitcode=no \
             --cfg 'feature=\"default\"' --check-cfg 'cfg(docsrs,test)' \
             -C metadata=9ba0d270e018e636 -C extra-filename=-7171bff4979b85a2 \
             --out-dir {WS}/target/debug/deps -L dependency={WS}/target/debug/deps \
             --extern proc_macro2={WS}/target/debug/deps/libproc_macro2-3d6a1b93318af5bc.rlib \
             --extern quote={WS}/target/debug/deps/libquote-faa604c83afebbc3.rlib \
             --extern proc_macro --cap-lints warn"
        ))
    }

    /// A build script: libc's.
    pub(crate) fn build_script() -> Vec<OsString> {
        argv(&format!(
            "{RUSTC} --crate-name build_script_build --edition=2021 {REGISTRY}/libc-0.2.190/build.rs \
             --error-format=json --json=diagnostic-rendered-ansi,artifacts,future-incompat \
             --crate-type bin --emit=dep-info,link -C embed-bitcode=no \
             --allow=unused_qualifications '--warn=clippy::ptr_as_ptr' --cfg 'feature=\"std\"' \
             --check-cfg 'cfg(docsrs,test)' -C metadata=17a11390755fa227 \
             -C extra-filename=-6bbf95a7855d1f44 \
             --out-dir {WS}/target/debug/build/libc-6bbf95a7855d1f44 \
             -L dependency={WS}/target/debug/deps --cap-lints warn"
        ))
    }

    /// `cargo check`: metadata only.
    pub(crate) fn check() -> Vec<OsString> {
        argv(&format!(
            "{RUSTC} --crate-name hushrun_build --edition=2024 crates/hushrun-build/src/lib.rs \
             --error-format=json --json=diagnostic-rendered-ansi,artifacts,future-incompat \
             --crate-type lib --emit=dep-info,metadata -C embed-bitcode=no -C debuginfo=2 \
             -C metadata=862e9ef32b215d01 -C extra-filename=-8e5475b7ff47af5a \
             --out-dir {WS}/target/debug/deps -C incremental={WS}/target/debug/incremental \
             -L dependency={WS}/target/debug/deps \
             --extern serde={WS}/target/debug/deps/libserde-2d5af21b7917df4e.rmeta"
        ))
    }

    /// `cargo test --no-run`: a test binary, which has `--test` and no `--crate-type`.
    pub(crate) fn test_binary() -> Vec<OsString> {
        argv(&format!(
            "{RUSTC} --crate-name hushrun_build --edition=2024 crates/hushrun-build/src/lib.rs \
             --error-format=json --json=diagnostic-rendered-ansi,artifacts,future-incompat \
             --emit=dep-info,link -C embed-bitcode=no -C debuginfo=2 -C split-debuginfo=unpacked \
             --test --check-cfg 'cfg(docsrs,test)' -C metadata=39aa4bc977f18d9c \
             -C extra-filename=-fc8e258bbc00b15d --out-dir {WS}/target/debug/deps \
             -C incremental={WS}/target/debug/incremental -L dependency={WS}/target/debug/deps \
             --extern tempfile={WS}/target/debug/deps/libtempfile-17250495e4bfedcf.rlib"
        ))
    }

    fn handled(argv: &[OsString], env: &Env) -> Invocation {
        match parse(argv, env, Path::new(WS)) {
            Parsed::Handled(invocation) => *invocation,
            Parsed::PassedThrough(why) => panic!("passed through: {why:?}"),
        }
    }

    fn why(argv: &[OsString]) -> Option<PassedThrough> {
        match parse(argv, &Env::default(), Path::new(WS)) {
            Parsed::Handled(_) => None,
            Parsed::PassedThrough(why) => Some(why),
        }
    }

    fn line_why(line: &str) -> Option<PassedThrough> {
        why(&argv(line))
    }

    /// The minimal call this crate handles, with `extra` spliced in after the source.
    fn minimal(extra: &str) -> String {
        format!(
            "{RUSTC} --crate-name x src/lib.rs {extra} --crate-type lib --emit=dep-info,metadata,link \
             --error-format=json --json=artifacts -C extra-filename=-1 --out-dir /o"
        )
    }

    #[test]
    fn workspace_library_is_read() {
        let (argv, env) = workspace_lib();
        let inv = handled(&argv, &env);
        assert_eq!(inv.rustc(), RUSTC);
        assert_eq!(inv.crate_name(), "hushrun_build");
        assert_eq!(inv.class(), Class::Library);
        assert_eq!(inv.emits(), [Emit::DepInfo, Emit::Metadata, Emit::Link]);
        assert_eq!(inv.out_dir(), "/home/dev/ws/target/debug/deps");
        assert_eq!(inv.extra_filename(), "-55ed56aeb232fcd2");
        assert_eq!(inv.source(), "crates/hushrun-build/src/lib.rs");
        assert_eq!(
            inv.incremental(),
            Some("/home/dev/ws/target/debug/incremental")
        );
        assert_eq!(inv.target(), None);
        assert!(inv.searchable());
        assert_eq!(
            inv.dependency_dirs().collect::<Vec<_>>(),
            ["/home/dev/ws/target/debug/deps"]
        );
        assert_eq!(inv.externs().len(), 2);
        assert_eq!(inv.externs()[0].name, "hushrun_core");
        assert_eq!(
            inv.externs()[0].path.as_deref(),
            Some("/home/dev/ws/target/debug/deps/libhushrun_core-f54f7763168a4211.rmeta")
        );
        assert_eq!(inv.cwd(), WS);
        assert_eq!(inv.package().name.as_deref(), Some("hushrun-build"));
        assert_eq!(inv.package().version.as_deref(), Some("0.12.0"));
        assert_eq!(
            inv.package().manifest_dir.as_deref(),
            Some("/home/dev/ws/crates/hushrun-build")
        );
        assert_eq!(inv.package().out_dir, None);
        assert_eq!(
            inv.message_form(),
            r#"["json","diagnostic-rendered-ansi,artifacts,future-incompat",null,null]"#
        );
    }

    // covers: B-5
    #[test]
    fn key_args_leave_out_folders_and_message_form() {
        let (argv, env) = workspace_lib();
        let inv = handled(&argv, &env);
        let expected = [
            "--crate-name=hushrun_build",
            "--edition=2024",
            "crates/hushrun-build/src/lib.rs",
            "--crate-type=lib",
            "--emit=dep-info,metadata,link",
            "-Cembed-bitcode=no",
            "-Cdebuginfo=2",
            "-Csplit-debuginfo=unpacked",
            "--forbid=unsafe_code",
            "--allow=unreachable_pub",
            "--deny=clippy::todo",
            "--deny=clippy::dbg_macro",
            "--check-cfg=cfg(docsrs,test)",
            "--check-cfg=cfg(feature, values())",
            "-Cmetadata=4409a3f27f06eee7",
            "-Cextra-filename=-55ed56aeb232fcd2",
            "--extern=hushrun_core",
            "--extern=memchr",
        ];
        assert_eq!(inv.key_args(), expected);
    }

    // covers: B-5
    #[test]
    fn key_env_keeps_cargo_variables_but_the_named_ones() {
        let (argv, mut env) = workspace_lib();
        env.vars
            .insert("MACOSX_DEPLOYMENT_TARGET".into(), "11.0".into());
        env.vars.insert("RUSTC_BOOTSTRAP".into(), "1".into());
        env.vars.insert("CARGO_SBOM_PATH".into(), "/x".into());
        env.vars
            .insert("CARGO_RUSTC_CURRENT_DIR".into(), "/x".into());
        env.vars.insert("MY_FLAG".into(), "on".into());
        let inv = handled(&argv, &env);
        let names: Vec<_> = inv.key_env().keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "CARGO_CRATE_NAME",
                "CARGO_MANIFEST_DIR",
                "CARGO_MANIFEST_PATH",
                "CARGO_PKG_NAME",
                "CARGO_PKG_VERSION",
                "MACOSX_DEPLOYMENT_TARGET",
                "RUSTC_BOOTSTRAP",
            ]
        );
        let with = inv
            .key_env_with(&env, &["MY_FLAG".into(), "UNSET_ONE".into()])
            .unwrap();
        assert_eq!(with.get("MY_FLAG").map(String::as_str), Some("on"));
        assert!(!with.contains_key("UNSET_ONE"));
    }

    // covers: B-5
    #[test]
    fn every_spelling_keys_alike() {
        let spellings = [
            ("-C opt-level=3", "-Copt-level=3"),
            ("-Copt-level=3", "-Copt-level=3"),
            ("--codegen opt-level=3", "-Copt-level=3"),
            ("--codegen=opt-level=3", "-Copt-level=3"),
            ("-L native=/n", "-Lnative=/n"),
            ("-Lnative=/n", "-Lnative=/n"),
            ("-L /n", "-Lall=/n"),
            ("-L all=/n", "-Lall=/n"),
            ("-L weird=/n", "-Lall=weird=/n"),
            ("--cfg foo", "--cfg=foo"),
            ("--cfg=foo", "--cfg=foo"),
            ("--check-cfg cfg(x)", "--check-cfg=cfg(x)"),
            ("--cap-lints warn", "--cap-lints=warn"),
            ("-A warnings", "--allow=warnings"),
            ("-Awarnings", "--allow=warnings"),
            ("--allow warnings", "--allow=warnings"),
            ("-W x", "--warn=x"),
            ("-D x", "--deny=x"),
            ("-F x", "--forbid=x"),
            ("--force-warn x", "--force-warn=x"),
            ("-Z threads=8", "-Zthreads=8"),
            ("-Zthreads=8", "-Zthreads=8"),
            ("-l static=z", "-lstatic=z"),
            ("--edition 2021", "--edition=2021"),
            (
                "--target aarch64-apple-darwin",
                "--target=aarch64-apple-darwin",
            ),
            ("--extern a=/p/liba.rlib", "--extern=a"),
            (
                "--extern priv,noprelude:a=/p/liba.rlib",
                "--extern=priv,noprelude:a",
            ),
            ("-O", "-O"),
            ("-g", "-g"),
        ];
        for (given, keyed) in spellings {
            let argv = argv(&minimal(given));
            let inv = handled(&argv, &Env::default());
            assert!(
                inv.key_args().iter().any(|arg| arg == keyed),
                "{given}: {:?}",
                inv.key_args()
            );
        }
        let excluded = [
            "-C incremental=/i",
            "-Cincremental=/i",
            "--codegen=incremental=/i",
            "-L dependency=/d",
            "-Ldependency=/d",
            "--color always",
            "--color=always",
            "--diagnostic-width 80",
            "--json future-incompat",
        ];
        let base = handled(&argv(&minimal("")), &Env::default()).key_args();
        for given in excluded {
            let inv = handled(&argv(&minimal(given)), &Env::default());
            assert_eq!(inv.key_args(), base, "{given}");
        }
    }

    #[test]
    fn externs_keep_their_options_and_path() {
        let inv = handled(
            &argv(&minimal(
                "--extern noprelude,priv:a=/p/liba.rlib --extern proc_macro --extern b=/p/b=x.rlib",
            )),
            &Env::default(),
        );
        assert_eq!(
            inv.externs(),
            [
                Extern {
                    name: "a".into(),
                    options: vec!["noprelude".into(), "priv".into()],
                    path: Some("/p/liba.rlib".into()),
                },
                Extern {
                    name: "proc_macro".into(),
                    options: vec![],
                    path: None,
                },
                Extern {
                    name: "b".into(),
                    options: vec![],
                    path: Some("/p/b=x.rlib".into()),
                },
            ]
        );
        assert_eq!(
            line_why(&minimal("--extern bogus:a=/p")),
            Some(PassedThrough::UnknownArgument)
        );
        assert_eq!(
            line_why(&minimal("--extern =/p")),
            Some(PassedThrough::UnknownArgument)
        );
    }

    #[test]
    fn search_paths_keep_their_kind() {
        let inv = handled(
            &argv(&minimal(
                "-L native=/n -Lframework=/f -L /a -L crate=/c -L dependency=/d1 -Ldependency=/d2",
            )),
            &Env::default(),
        );
        let kinds: Vec<_> = inv
            .search_paths()
            .iter()
            .map(|s| (s.kind, s.path.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (SearchKind::Native, "/n"),
                (SearchKind::Framework, "/f"),
                (SearchKind::All, "/a"),
                (SearchKind::Crate, "/c"),
                (SearchKind::Dependency, "/d1"),
                (SearchKind::Dependency, "/d2"),
            ]
        );
        assert_eq!(inv.dependency_dirs().collect::<Vec<_>>(), ["/d1", "/d2"]);
    }

    // covers: B-9
    #[test]
    fn command_drops_incremental_in_every_spelling_and_appends() {
        for given in [
            "-C incremental=/i",
            "-Cincremental=/i",
            "--codegen incremental=/i",
            "--codegen=incremental=/i",
        ] {
            let line = minimal(given);
            let inv = handled(&argv(&line), &Env::default());
            assert_eq!(inv.incremental(), Some("/i"));
            let extra = [OsString::from("--remap-path-prefix=/w=.")];
            let command = inv.command(&extra);
            let without: Vec<OsString> = argv(&minimal(""))[1..]
                .iter()
                .cloned()
                .chain(extra.iter().cloned())
                .collect();
            assert_eq!(command, without, "{given}");
        }
        let (argv, env) = workspace_lib();
        let inv = handled(&argv, &env);
        let command = inv.command(&[]);
        assert_eq!(command.len(), argv.len() - 1 - 2);
        assert!(
            !command
                .iter()
                .any(|arg| arg.to_string_lossy().contains("incremental"))
        );
        assert!(command.iter().any(|arg| arg == "--out-dir"));
    }

    // covers: B-4
    #[test]
    fn real_calls_are_classified() {
        let (lib, env) = registry_lib();
        let inv = handled(&lib, &env);
        assert_eq!(inv.class(), Class::Library);
        assert_eq!(
            inv.package().out_dir.as_deref(),
            Some("/home/dev/ws/target/debug/build/serde_core-db522bc2a1d3eab6/out")
        );
        assert!(
            inv.key_args()
                .contains(&"--cfg=feature=\"std\"".to_string())
        );
        assert!(inv.key_args().contains(&"--cap-lints=warn".to_string()));

        let inv = handled(&proc_macro(), &Env::default());
        assert_eq!(inv.class(), Class::ProcMacro);
        assert_eq!(inv.emits(), [Emit::DepInfo, Emit::Link]);
        assert_eq!(inv.externs()[2].path, None);

        let inv = handled(&build_script(), &Env::default());
        assert_eq!(inv.class(), Class::BuildScript);
        assert_eq!(
            inv.out_dir(),
            "/home/dev/ws/target/debug/build/libc-6bbf95a7855d1f44"
        );

        let inv = handled(&check(), &Env::default());
        assert_eq!(inv.class(), Class::Library);
        assert_eq!(inv.emits(), [Emit::DepInfo, Emit::Metadata]);

        let inv = handled(&test_binary(), &Env::default());
        assert_eq!(inv.class(), Class::Program);
        assert!(inv.key_args().contains(&"--test".to_string()));

        let bin = minimal("").replace("--crate-type lib", "--crate-type bin");
        assert_eq!(
            handled(&argv(&bin), &Env::default()).class(),
            Class::Program
        );
        let script = bin.replace("--crate-name x", "--crate-name build_script_main");
        assert_eq!(
            handled(&argv(&script), &Env::default()).class(),
            Class::BuildScript
        );
        let rlib = minimal("").replace("--crate-type lib", "--crate-type=rlib");
        assert_eq!(
            handled(&argv(&rlib), &Env::default()).class(),
            Class::Library
        );
    }

    // covers: B-4
    #[test]
    fn clippy_and_other_drivers_pass_through() {
        let mut clippy = check();
        clippy.insert(
            0,
            OsString::from("/home/dev/.rustup/toolchains/1.94.0/bin/clippy-driver"),
        );
        assert_eq!(why(&clippy), Some(PassedThrough::NotRustc));
        assert_eq!(why(&[]), Some(PassedThrough::NotRustc));
        let renamed = minimal("").replace(RUSTC, "/usr/bin/rustc-1.94");
        assert_eq!(line_why(&renamed), Some(PassedThrough::NotRustc));
        assert_eq!(line_why(&minimal("").replace(RUSTC, "rustc")), None);
    }

    // covers: B-4
    #[test]
    fn every_clause_of_b4_passes_through() {
        let cases: [(&str, PassedThrough); 26] = [
            ("--print=file-names", PassedThrough::Probe),
            ("--print cfg", PassedThrough::Probe),
            ("-vV", PassedThrough::Probe),
            ("-V", PassedThrough::Probe),
            ("--version", PassedThrough::Probe),
            ("-", PassedThrough::Probe),
            ("@/tmp/args", PassedThrough::ArgFile),
            ("--target /t/custom.json", PassedThrough::TargetFile),
            ("--target=custom.json", PassedThrough::TargetFile),
            ("--remap-path-prefix=/a=b", PassedThrough::CallerRemap),
            ("--remap-path-prefix /a=b", PassedThrough::CallerRemap),
            ("-C instrument-coverage", PassedThrough::Instrumented),
            ("-Cprofile-generate=/p", PassedThrough::Instrumented),
            ("--codegen=profile-use=/p", PassedThrough::Instrumented),
            ("--emit=llvm-ir", PassedThrough::Emit),
            ("--emit=link=/out/x", PassedThrough::Emit),
            ("--emit asm", PassedThrough::Emit),
            ("--crate-type bin", PassedThrough::CrateTypes),
            ("-o /x", PassedThrough::OutputFile),
            ("-o/x", PassedThrough::OutputFile),
            ("--frobnicate", PassedThrough::UnknownArgument),
            ("-v", PassedThrough::UnknownArgument),
            ("-Q", PassedThrough::UnknownArgument),
            ("--test=yes", PassedThrough::Malformed),
            ("other.rs", PassedThrough::Malformed),
            ("--out-dir /p", PassedThrough::Malformed),
        ];
        for (extra, expected) in cases {
            assert_eq!(line_why(&minimal(extra)), Some(expected), "{extra}");
        }
        assert_eq!(line_why(&minimal("")), None);

        let lib = minimal("");
        let without = |flag: &str| lib.replace(flag, "");
        assert_eq!(
            line_why(&without("--crate-name x")),
            Some(PassedThrough::NoCrateName)
        );
        assert_eq!(
            line_why(&without("--out-dir /o")),
            Some(PassedThrough::NoOutDir)
        );
        assert_eq!(
            line_why(&without("-C extra-filename=-1")),
            Some(PassedThrough::NoExtraFilename)
        );
        assert_eq!(
            line_why(&lib.replace("--json=artifacts", "--json=diagnostic-short")),
            Some(PassedThrough::NoArtifacts)
        );
        assert_eq!(
            line_why(&without("--json=artifacts")),
            Some(PassedThrough::NoArtifacts)
        );
        assert_eq!(
            line_why(&lib.replace("--json=artifacts", "--json=x --json=artifacts")),
            None
        );
        assert_eq!(
            line_why(&without("--crate-type lib")),
            Some(PassedThrough::NoCrateType)
        );
        assert_eq!(
            line_why(&without("src/lib.rs")),
            Some(PassedThrough::Malformed)
        );
        assert_eq!(
            line_why(&lib.replace("--crate-type lib", "--crate-type lib,rlib")),
            Some(PassedThrough::CrateTypes)
        );
        for kind in ["cdylib", "dylib", "staticlib", "sdylib"] {
            assert_eq!(
                line_why(&lib.replace("--crate-type lib", &format!("--crate-type={kind}"))),
                Some(PassedThrough::CrateType),
                "{kind}"
            );
        }
        let cut = format!("{} --out-dir", without("--out-dir /o"));
        assert_eq!(line_why(&cut), Some(PassedThrough::Malformed));
    }

    // covers: B-4
    #[test]
    fn cargo_probe_passes_through() {
        let probe = format!(
            "{RUSTC} - --crate-name ___ --print=file-names --crate-type bin --crate-type rlib \
             --crate-type dylib --crate-type cdylib --crate-type staticlib --crate-type proc-macro \
             --print=sysroot --print=split-debuginfo --print=crate-name --print=cfg -Wwarnings"
        );
        assert_eq!(line_why(&probe), Some(PassedThrough::Probe));
        assert_eq!(
            line_why(&format!("{RUSTC} -vV")),
            Some(PassedThrough::Probe)
        );
    }

    #[cfg(unix)]
    #[test]
    fn not_utf8_passes_through() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'/', 0xff]);
        let mut args = argv(&minimal(""));
        args.push(OsString::from("--cfg"));
        args.push(bad.clone());
        assert_eq!(why(&args), Some(PassedThrough::NotUtf8));

        let good = argv(&minimal(""));
        let env = Env::from_vars([(OsString::from("CARGO_PKG_NAME"), bad.clone())]);
        assert!(matches!(
            parse(&good, &env, Path::new(WS)),
            Parsed::PassedThrough(PassedThrough::NotUtf8)
        ));
        let mut name = b"CARGO_PKG_".to_vec();
        name.push(0xff);
        let env = Env::from_vars([(OsString::from_vec(name), OsString::from("x"))]);
        assert!(matches!(
            parse(&good, &env, Path::new(WS)),
            Parsed::PassedThrough(PassedThrough::NotUtf8)
        ));
        // A name of cargo's configuration is not keyed, so it can't stop a call.
        let mut name = b"CARGO_TERM_".to_vec();
        name.push(0xff);
        let env = Env::from_vars([(OsString::from_vec(name), OsString::from("x"))]);
        assert!(matches!(
            parse(&good, &env, Path::new(WS)),
            Parsed::Handled(_)
        ));
        let env = Env::from_vars([(OsString::from("HOME"), bad.clone())]);
        assert!(matches!(
            parse(&good, &env, Path::new(WS)),
            Parsed::Handled(_)
        ));
        let cwd = OsString::from_vec(vec![b'/', 0xfe]);
        assert!(matches!(
            parse(&good, &Env::default(), Path::new(&cwd)),
            Parsed::PassedThrough(PassedThrough::NotUtf8)
        ));
    }

    // covers: B-10
    #[test]
    fn outputs_that_cannot_be_searched() {
        let unsearchable = [
            "-C lto",
            "-Clto=fat",
            "-C lto=thin",
            "-C linker-plugin-lto",
            "-C embed-bitcode=yes",
            "-C embed-bitcode",
            "-Z debuginfo-compression=zlib",
            "-C link-arg=-Wl,--compress-debug-sections=zlib",
        ];
        for extra in unsearchable {
            let inv = handled(&argv(&minimal(extra)), &Env::default());
            assert!(!inv.searchable(), "{extra}");
        }
        let searchable = [
            "",
            "-C lto=off",
            "-C lto=no",
            "-C embed-bitcode=no",
            "-Z debuginfo-compression=none",
            "-C link-arg=-Wl,--compress-debug-sections=none",
        ];
        for extra in searchable {
            let inv = handled(&argv(&minimal(extra)), &Env::default());
            assert!(inv.searchable(), "{extra}");
        }
    }

    // covers: B-12
    #[test]
    fn message_form_tells_forms_apart() {
        let form = |extra: &str| {
            handled(&argv(&minimal(extra)), &Env::default())
                .message_form()
                .to_string()
        };
        let plain = form("");
        assert_ne!(plain, form("--color always"));
        assert_ne!(plain, form("--diagnostic-width=80"));
        assert_ne!(form("--color always"), form("--color never"));
        assert_eq!(form("--color always"), form("--color=always"));
        let short = handled(
            &argv(&minimal("").replace("--json=artifacts", "--json=artifacts,diagnostic-short")),
            &Env::default(),
        );
        assert_ne!(plain, short.message_form());
    }

    // covers: B-5
    #[test]
    fn variables_rustc_or_the_linker_read_without_an_env_dep_are_keyed() {
        let read = [
            "RUSTC_BOOTSTRAP",
            "MACOSX_DEPLOYMENT_TARGET",
            "SDKROOT",
            "IPHONEOS_DEPLOYMENT_TARGET",
            "TVOS_DEPLOYMENT_TARGET",
            "WATCHOS_DEPLOYMENT_TARGET",
            "XROS_DEPLOYMENT_TARGET",
            "RUSTC_FORCE_RUSTC_VERSION",
            "LLVM_OVERRIDE_PRODUCER",
            "RUST_TARGET_PATH",
            "LIBRARY_PATH",
            "LD_RUN_PATH",
            "DEVELOPER_DIR",
            "COMPILER_PATH",
            "GCC_EXEC_PREFIX",
            "SOURCE_DATE_EPOCH",
        ];
        let program = minimal("").replace("--crate-type lib", "--crate-type bin");
        for name in read {
            let env = Env::from_vars([(name, "x")]);
            let inv = handled(&argv(&program), &env);
            assert_eq!(
                inv.key_env().get(name).map(String::as_str),
                Some("x"),
                "{name}"
            );
            // A library runs no C driver or linker: what only they read is not its input.
            let library = handled(&argv(&minimal("")), &env);
            // rustc reads the SDK (`SDKROOT`, `DEVELOPER_DIR`) only to link: a library's bytes
            // are the same with any of them (two SDKs and both developer folders tried).
            let linker_only = matches!(
                name,
                "LIBRARY_PATH"
                    | "LD_RUN_PATH"
                    | "COMPILER_PATH"
                    | "GCC_EXEC_PREFIX"
                    | "SDKROOT"
                    | "DEVELOPER_DIR"
            );
            // A library that expands no proc macro takes no date from it (the next test).
            let dateless = name == "SOURCE_DATE_EPOCH";
            assert_eq!(
                library.key_env().get(name).map(String::as_str),
                (!linker_only && !dateless).then_some("x"),
                "{name}"
            );
        }
    }

    // covers: B-5
    #[test]
    fn source_date_epoch_is_part_of_the_first_key_where_a_date_can_reach_the_output() {
        // Compilers and build tools take "now" from it, so it reaches anything that embeds a
        // date: a program (the linker, a C driver) and a library that expands a proc macro (it
        // runs inside rustc and reads rustc's variables). Unset, one value and another are three
        // different compiles for those.
        let key = |line: &str, value: Option<&str>| {
            let env = match value {
                Some(value) => Env::from_vars([("SOURCE_DATE_EPOCH", value)]),
                None => Env::default(),
            };
            let inv = handled(&argv(line), &env);
            crate::key::Asked {
                compiler: hushrun_core::Digest::of(b"rustc"),
                args: inv.key_args(),
                env: inv.key_env().clone(),
                added: Vec::new(),
            }
            .key()
            .unwrap()
        };
        let program = minimal("").replace("--crate-type lib", "--crate-type bin");
        let expands = minimal(&format!(
            "--extern serde_derive=/o/libserde_derive-1.{}",
            std::env::consts::DLL_EXTENSION
        ));
        for line in [program.as_str(), expands.as_str()] {
            let unset = key(line, None);
            let one = key(line, Some("1700000000"));
            assert_ne!(unset, one, "{line}");
            assert_ne!(one, key(line, Some("1800000000")), "{line}");
            assert_eq!(one, key(line, Some("1700000000")), "{line}");
        }
        // rustc itself never reads it: a library that expands no proc macro is the same compile
        // under any value (every library of this workspace compared, on macOS and Linux).
        let plain = minimal("--extern itoa=/o/libitoa-1.rmeta --extern proc_macro");
        assert_eq!(key(&plain, None), key(&plain, Some("1700000000")));
        assert_eq!(key(&plain, Some("1")), key(&plain, Some("2")));
    }

    // covers: B-5
    #[test]
    fn the_persons_cargo_configuration_is_not_keyed() {
        let (argv, env) = workspace_lib();
        let base = handled(&argv, &env).key_env().clone();
        for (name, value) in [
            ("CARGO_TARGET_DIR", "/elsewhere/target"),
            ("CARGO_BUILD_TARGET_DIR", "/elsewhere/t"),
            ("CARGO_HOME", "/home/dev/.cargo"),
            ("CARGO_TERM_COLOR", "always"),
            ("CARGO_REGISTRY_TOKEN", "secret"),
            ("CARGO_REGISTRIES_MINE_TOKEN", "secret"),
            ("CARGO_INCREMENTAL", "0"),
            ("CARGO_BUILD_JOBS", "4"),
            ("CARGO_PROFILE_DEV_OPT_LEVEL", "1"),
            ("CARGO_ENCODED_RUSTFLAGS", "-Copt-level=1"),
            ("CARGO_NET_OFFLINE", "true"),
            ("CARGO_HTTP_TIMEOUT", "5"),
            ("CARGO_LOG", "debug"),
            ("CARGO_UNSTABLE_X", "1"),
            ("CARGO_ALIAS_B", "build"),
            ("CARGO", "/bin/cargo"),
            ("CARGO_MAKEFLAGS", "-j"),
            ("CARGO_PRIMARY_PACKAGE", "1"),
            ("CARGO_SBOM_PATH", "/x"),
            ("CARGO_RUSTC_CURRENT_DIR", "/x"),
        ] {
            let mut env = env.clone();
            env.vars.insert(name.into(), value.into());
            assert_eq!(handled(&argv, &env).key_env(), &base, "{name}");
        }
        for (name, value) in [
            ("CARGO_PKG_VERSION", "0.13.0"),
            ("CARGO_PKG_AUTHORS", "someone"),
            ("CARGO_CRATE_NAME", "other"),
            ("CARGO_MANIFEST_LINKS", "z"),
            ("CARGO_BIN_EXE_tool", "/w/target/debug/tool"),
            ("CARGO_BIN_NAME", "tool"),
            ("CARGO_TARGET_TMPDIR", "/w/target/tmp"),
        ] {
            let mut env = env.clone();
            env.vars.insert(name.into(), value.into());
            assert_ne!(handled(&argv, &env).key_env(), &base, "{name}");
        }
    }

    // covers: B-4
    #[test]
    fn flags_that_name_a_file_rustc_reads_pass_through() {
        for (extra, why) in [
            ("--sysroot /s", PassedThrough::Sysroot),
            ("--sysroot=/s", PassedThrough::Sysroot),
            ("-Z dump-mir=all", PassedThrough::Unstable),
            ("-Zself-profile=/p", PassedThrough::Unstable),
            ("-Zcodegen-backend=/b.so", PassedThrough::Unstable),
            ("-Zllvm-plugins=/p.so", PassedThrough::Unstable),
            ("-Zno-codegen", PassedThrough::Unstable),
            ("--extern foo", PassedThrough::ExternWithoutPath),
            ("--extern priv:foo", PassedThrough::ExternWithoutPath),
        ] {
            assert_eq!(line_why(&minimal(extra)), Some(why), "{extra}");
        }
        for extra in ["-Zremap-cwd-prefix=.", "-Z remap-path-scope=all"] {
            assert_eq!(
                line_why(&minimal(extra)),
                Some(PassedThrough::CallerRemap),
                "{extra}"
            );
        }
        for extra in [
            "-Z unstable-options",
            "-Zthreads=8",
            "-Zshare-generics=y",
            "-Zmacro-backtrace",
            "-Zdebuginfo-compression=none",
            "--extern proc_macro",
        ] {
            assert_eq!(line_why(&minimal(extra)), None, "{extra}");
        }
    }

    // covers: B-10
    #[test]
    fn compressed_debug_sections_asked_through_the_driver_cannot_be_searched() {
        for extra in [
            "-C link-arg=-gz",
            "-C link-arg=-gz=zlib",
            "-C link-arg=-gz=zstd",
            "-C 'link-args=-O1 -gz'",
            "-C link-arg=-Wl,--compress-debug-sections,zlib",
            "-C link-arg=-Wl,--compress-debug-sections",
            "-C link-arg=--compress-debug-sections=zstd",
            "-C link-arg=-Xlinker -C link-arg=--compress-debug-sections=zlib",
        ] {
            let inv = handled(&argv(&minimal(extra)), &Env::default());
            assert!(!inv.searchable(), "{extra}");
        }
        for extra in [
            "-C link-arg=-gz=none",
            "-C link-arg=-Wl,--compress-debug-sections=none",
            "-C link-arg=-Wl,--compress-debug-sections,none",
            "-C link-arg=-gzip-not-a-flag",
        ] {
            let inv = handled(&argv(&minimal(extra)), &Env::default());
            assert!(inv.searchable(), "{extra}");
        }
    }

    // covers: B-6
    #[test]
    fn native_libraries_are_read_as_rustc_reads_them() {
        let inv = handled(
            &argv(&minimal(
                "-l z -l static=foo -lstatic:+whole-archive,-bundle=bar -l framework=Security \
                 -l dylib:-verbatim=baz -L native=/n -L /a -L framework=/f -L dependency=/d -L crate=/c",
            )),
            &Env::default(),
        );
        let libs: Vec<_> = inv
            .native_libs()
            .iter()
            .map(|lib| (lib.kind, lib.name.as_str()))
            .collect();
        assert_eq!(
            libs,
            [
                (NativeKind::Dylib, "z"),
                (NativeKind::Static, "foo"),
                (NativeKind::Static, "bar"),
                (NativeKind::Framework, "Security"),
                (NativeKind::Dylib, "baz"),
            ]
        );
        assert!(inv.native_certain());
        assert_eq!(
            inv.native_dirs(NativeKind::Static).collect::<Vec<_>>(),
            ["/n", "/a"]
        );
        assert_eq!(
            inv.native_dirs(NativeKind::Framework).collect::<Vec<_>>(),
            ["/a", "/f"]
        );
        for uncertain in [
            "-l static:+verbatim=libfoo.a",
            "-l foo:bar",
            "-l :libfoo.a",
            "-l static=:libfoo.a",
            "-l link-arg=-Tx",
            "-l weird=foo",
            "-l static:+unknown=foo",
            "-l static=",
            "-l static=../foo",
        ] {
            let inv = handled(&argv(&minimal(uncertain)), &Env::default());
            assert!(!inv.native_certain(), "{uncertain}");
        }
    }

    // covers: B-5, B-8
    #[test]
    fn the_locations_cargo_hands_a_compile_are_read_as_roots() {
        let env = Env::from_vars([
            ("CARGO_TARGET_DIR", "/elsewhere/target"),
            ("CARGO_BUILD_TARGET_DIR", "relative/target"),
            ("CARGO_TARGET_TMPDIR", "/elsewhere/target/tmp"),
            ("CARGO_BIN_EXE_where", "/elsewhere/target/debug/where"),
            ("CARGO_BIN_EXE_prog", "/elsewhere/target/debug/prog"),
            (
                "CARGO_BIN_EXE_cross",
                "/elsewhere/target/x86_64-apple-darwin/debug/cross",
            ),
            ("CARGO_BIN_EXE_rel", "debug/rel"),
        ]);
        let inv = handled(&argv(&minimal("")), &env);
        let package = inv.package();
        assert_eq!(package.target_dir.as_deref(), Some("/elsewhere/target"));
        assert_eq!(package.build_target_dir, None);
        assert_eq!(
            package.target_tmpdir.as_deref(),
            Some("/elsewhere/target/tmp")
        );
        // In the order of the names: cross, prog (where shares prog's folder); rel is relative.
        assert_eq!(
            package.bin_exe_dirs,
            [
                "/elsewhere/target/x86_64-apple-darwin/debug",
                "/elsewhere/target/debug"
            ]
        );
        let none = handled(&argv(&minimal("")), &Env::default());
        assert_eq!(none.package().target_dir, None);
        assert_eq!(none.package().target_tmpdir, None);
        assert!(none.package().bin_exe_dirs.is_empty());
    }

    #[test]
    fn target_triple_is_kept() {
        let inv = handled(
            &argv(&minimal("--target aarch64-apple-darwin")),
            &Env::default(),
        );
        assert_eq!(inv.target(), Some("aarch64-apple-darwin"));
    }
}
