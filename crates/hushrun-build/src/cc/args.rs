//! Which compiler calls the launcher handles (B-51): one compile of one source of C or C++ to one
//! object, with depfile flags only among `-MD`, `-MMD`, `-MF`, `-MT`, `-MQ` and `-MP`. Every other
//! call runs exactly as asked, and the reason says why.
//!
//! The parser knows which flags take their value as the next argument. An argument it can't
//! place (a positional that is no source, a value of a flag it doesn't know) is a reason to pass
//! through, never a guess: a flag it doesn't know whose value is attached is keyed by its text.

use std::ffi::OsString;
use std::path::Path;

/// The language of the one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// C.
    C,
    /// C++.
    Cxx,
}

impl Lang {
    /// The word `-x` takes, which the key and the driver's probe use.
    pub const fn word(self) -> &'static str {
        match self {
            Lang::C => "c",
            Lang::Cxx => "c++",
        }
    }
}

/// Which depfile the call asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepMode {
    /// None.
    None,
    /// `-MD`: every file read.
    All,
    /// `-MMD`: without the files in system folders.
    User,
}

/// A target of the depfile as the call named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepTarget {
    /// `-MT`: written as it is.
    Plain(String),
    /// `-MQ`: quoted as Make reads it.
    Quoted(String),
}

/// The depfile the call asked for (B-57).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepAsked {
    /// `-MD`, `-MMD` or neither.
    pub mode: DepMode,
    /// `-MF`'s path, as given.
    pub file: Option<String>,
    /// `-MT` and `-MQ`, in order.
    pub targets: Vec<DepTarget>,
    /// `-MP`: a phony target per header.
    pub phony: bool,
}

impl DepAsked {
    /// Where the compiler would write the depfile: `-MF`, else the output with its suffix
    /// replaced by `.d`. `None` when none was asked.
    pub fn path(&self, output: &str) -> Option<String> {
        if self.mode == DepMode::None {
            return None;
        }
        if let Some(file) = &self.file {
            return Some(file.clone());
        }
        let out = Path::new(output);
        Some(out.with_extension("d").to_string_lossy().into_owned())
    }
}

/// One folder of the include search, in the order the compiler searches (B-54).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir {
    /// The folder as given.
    pub path: String,
    /// A framework folder (`-F`, `-iframework`): a header `<N/h.h>` is `N.framework/Headers/h.h`.
    pub framework: bool,
    /// A system folder (`-isystem`, `-idirafter`, `-iframework`): `-MMD` leaves its files out.
    pub system: bool,
}

/// One compile the launcher handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compile {
    /// The compiler, as the call named it.
    pub compiler: OsString,
    /// The arguments after the compiler, as given.
    pub args: Vec<String>,
    /// The source's language.
    pub lang: Lang,
    /// The source, as given.
    pub source: String,
    /// `-o`'s path, as given.
    pub output: String,
    /// The depfile asked for.
    pub dep: DepAsked,
    /// The arguments in order without `-o`, `-MF`, `-MT`, `-MQ` and their values, and without
    /// `-MD`, `-MMD` and `-MP`, which [`Compile::dep`] holds: what the first key holds (B-53).
    pub keyed: Vec<String>,
    /// The quote folders (`-iquote`), searched first for `"…"` after the including file's.
    pub quote: Vec<Dir>,
    /// `-I` and `-F`, in order, then `-isystem` and `-iframework`, then `-idirafter`.
    pub search: Vec<Dir>,
    /// The flags that change what the driver does with no source (the target, the sysroot, the
    /// standard library, the default folders): asked of the driver for the compiler's identity
    /// and its system folders (B-52).
    pub driver_flags: Vec<String>,
    /// `-march=native`, `-mcpu=native`, `-mtune=native` as given: keyed by what the driver
    /// resolves them to (B-53).
    pub native: Vec<String>,
    /// `-flto` in any form.
    pub lto: bool,
    /// `-frandom-seed` in any form.
    pub random_seed: bool,
    /// Compressed debug sections (`-gz`, `--compress-debug-sections`): a root in them can't be
    /// found in the object's bytes, so the block is tied to its paths (B-56).
    pub compressed_debug: bool,
}

/// Why a call runs exactly as asked (B-51). The word is the run's log's; stable.
pub type Why = &'static str;

/// What [`classify`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    /// A compile the launcher handles.
    Compile(Box<Compile>),
    /// A call that runs exactly as asked.
    PassThrough(Why),
}

/// The names a compiler that is itself a launcher goes by (B-51).
pub const LAUNCHERS: &[&str] = &["ccache", "sccache", "distcc", "icecc", "buildcache"];

/// Variables that make the compiler write a file of its own or log itself (B-51).
fn dependency_variable(name: &str) -> bool {
    name == "DEPENDENCIES_OUTPUT"
        || name == "SUNPRO_DEPENDENCIES"
        || name.starts_with("CC_PRINT_")
        || name.starts_with("CC_LOG_")
}

/// Source extensions of C and C++ (B-51). `.C` is C++ on a case-sensitive name.
fn source_lang(arg: &str) -> Option<Lang> {
    let ext = Path::new(arg).extension()?.to_str()?;
    match ext {
        "c" => Some(Lang::C),
        "cc" | "cpp" | "cxx" | "c++" | "C" | "CPP" | "cp" => Some(Lang::Cxx),
        _ => None,
    }
}

/// A source of another language the launcher leaves alone, named so the log says which.
fn other_source(arg: &str) -> Option<Why> {
    let ext = Path::new(arg).extension()?.to_str()?;
    Some(match ext {
        "m" | "mm" | "M" => "objective-c",
        "s" | "S" | "sx" | "asm" => "assembly",
        "cu" | "cuh" => "cuda",
        "cppm" | "ixx" | "mpp" | "ccm" | "cxxm" | "c++m" => "modules",
        "h" | "hh" | "hpp" | "hxx" | "h++" | "H" | "gch" | "pch" | "pcm" => "pch",
        "i" | "ii" => "preprocessed",
        "o" | "obj" | "a" | "so" | "dylib" | "lib" => "link",
        "ll" | "bc" => "llvm-ir",
        "f" | "f90" | "f95" | "for" | "F" | "F90" => "fortran",
        _ => return None,
    })
}

/// Flags whose value is the next argument. A flag of this list written with its value attached
/// (`-Ifoo`, `-DX=1`) is read as such where the compiler allows it.
const TAKES_VALUE: &[&str] = &[
    "-o",
    "-MF",
    "-MT",
    "-MQ",
    "-I",
    "-isystem",
    "-iquote",
    "-idirafter",
    "-iframework",
    "-F",
    "-include",
    "-imacros",
    "-isysroot",
    "--sysroot",
    "-arch",
    "-target",
    "-D",
    "-U",
    "-x",
    "-Xassembler",
    "-Xpreprocessor",
    "-Xlinker",
    "-Xarch_x86_64",
    "-Xarch_arm64",
    "-iprefix",
    "-iwithprefix",
    "-iwithprefixbefore",
    "-iwithsysroot",
    "-imultilib",
    "-cxx-isystem",
    "-L",
    "-l",
    "-u",
    "-T",
    "-framework",
    "--param",
    "-G",
    "-z",
    "-aux-info",
    "-std",
    "-stdlib",
    "-ivfsoverlay",
    "-include-pch",
    "-Xclang",
    "-mllvm",
    "-B",
    "-specs",
    "-wrapper",
    "-MJ",
    "--serialize-diagnostics",
    "-serialize-diagnostics",
    "-index-store-path",
    "-fmodule-file",
    "-Xanalyzer",
    "-dependency-file",
    "-dependency-dot",
    "--config",
];

/// The reason a flag passes the call through, or `None` (B-51).
fn refused(arg: &str) -> Option<Why> {
    let exact: &[(&str, Why)] = &[
        ("-E", "preprocess"),
        ("-S", "assembly-output"),
        ("-M", "deps-only"),
        ("-MM", "deps-only"),
        ("-MG", "deps-only"),
        ("-emit-pch", "pch"),
        ("-include-pch", "pch"),
        ("-fmodules", "modules"),
        ("-fcxx-modules", "modules"),
        ("--coverage", "coverage"),
        ("-coverage", "coverage"),
        ("-ftest-coverage", "coverage"),
        ("-fprofile-arcs", "coverage"),
        ("-fcoverage-mapping", "coverage"),
        ("-fauto-profile", "profile"),
        ("-gsplit-dwarf", "second-output"),
        ("-fstack-usage", "second-output"),
        ("-fcallgraph-info", "second-output"),
        ("-MJ", "second-output"),
        ("--serialize-diagnostics", "second-output"),
        ("-serialize-diagnostics", "second-output"),
        ("-index-store-path", "second-output"),
        ("-aux-info", "second-output"),
        ("-fsave-optimization-record", "second-output"),
        ("-Xclang", "loads-code"),
        ("-mllvm", "loads-code"),
        ("--config", "loads-code"),
        ("-B", "loads-code"),
        ("-specs", "loads-code"),
        ("-wrapper", "loads-code"),
        ("-ivfsoverlay", "loads-code"),
        ("-dependency-file", "dependency-flags"),
        ("-dependency-dot", "dependency-flags"),
        ("-iprefix", "include-prefix"),
        ("-iwithprefix", "include-prefix"),
        ("-iwithprefixbefore", "include-prefix"),
        ("-iwithsysroot", "include-prefix"),
        ("-I-", "include-prefix"),
    ];
    if let Some((_, why)) = exact.iter().find(|(flag, _)| *flag == arg) {
        return Some(why);
    }
    let prefixes: &[(&str, Why)] = &[
        ("-fmodule-", "modules"),
        ("-fmodules-", "modules"),
        ("-fprebuilt-module-path", "modules"),
        ("-fprofile-", "profile"),
        ("-fauto-profile=", "profile"),
        ("-fcs-profile-generate", "profile"),
        ("-fcoverage-", "coverage"),
        ("-save-temps", "second-output"),
        ("-ftime-trace", "second-output"),
        ("-fdump-", "second-output"),
        ("-fcrash-diagnostics-dir", "second-output"),
        ("-foptimization-record-file", "second-output"),
        ("-fsave-optimization-record=", "second-output"),
        ("-gsplit-dwarf=", "second-output"),
        ("-fcallgraph-info=", "second-output"),
        ("-fdiagnostics-add-output=", "second-output"),
        ("-fdiagnostics-set-output=", "second-output"),
        ("-fplugin", "loads-code"),
        ("-fpass-plugin", "loads-code"),
        ("--config=", "loads-code"),
        ("-specs=", "loads-code"),
        ("--specs", "loads-code"),
        ("-ivfsoverlay=", "loads-code"),
        ("-fmodule-file=", "modules"),
        ("--serialize-diagnostics=", "second-output"),
        ("-index-store-path=", "second-output"),
    ];
    if let Some((_, why)) = prefixes.iter().find(|(p, _)| arg.starts_with(p)) {
        return Some(why);
    }
    // `-B<dir>` attached; `-Bstatic`/`-Bdynamic` are the linker's and mean nothing with `-c`,
    // but read no file either way: the folder form is the one that loads programs.
    if arg.starts_with("-B") && arg.len() > 2 {
        return Some("loads-code");
    }
    // `-fopt-info` naming a file writes one.
    if let Some(rest) = arg.strip_prefix("-fopt-info")
        && rest.contains('=')
    {
        return Some("second-output");
    }
    // GCC's diagnostics written to a file (`json-file`, `sarif-file`).
    if let Some(format) = arg.strip_prefix("-fdiagnostics-format=")
        && format.ends_with("-file")
    {
        return Some("second-output");
    }
    // The assembler's listing to a file (`-Wa,-adhln=file`) or its own depfile.
    if let Some(rest) = arg.strip_prefix("-Wa,")
        && rest.split(',').any(assembler_writes)
    {
        return Some("second-output");
    }
    // `-Wp,-MD,file` and its kin write a depfile through the preprocessor's own flags.
    if let Some(rest) = arg.strip_prefix("-Wp,")
        && rest.split(',').any(|part| part.starts_with("-M"))
    {
        return Some("dependency-flags");
    }
    // A depfile flag outside the six (B-51): `-MD`'s kin are handled by the caller.
    if arg.starts_with("-M")
        && !matches!(arg, "-MD" | "-MMD" | "-MF" | "-MT" | "-MQ" | "-MP")
        && !(arg.starts_with("-MF") || arg.starts_with("-MT") || arg.starts_with("-MQ"))
    {
        return Some("deps-only");
    }
    None
}

/// Whether an assembler flag writes a file: a listing to a file (`-a…=file`) or a depfile
/// (`--MD`).
fn assembler_writes(flag: &str) -> bool {
    (flag.starts_with("-a") && flag.contains('=')) || flag.starts_with("--MD")
}

/// Whether a flag goes into the driver's probe (B-52): what changes the target, the sysroot,
/// the standard library or the default folders.
fn driver_flag(flag: &str) -> bool {
    matches!(
        flag,
        "-m32" | "-m64" | "-nostdinc" | "-nostdinc++" | "-nostdlibinc" | "-nobuiltininc"
    ) || [
        "--target=",
        "-march=",
        "-mcpu=",
        "-stdlib=",
        "--sysroot=",
        "--gcc-toolchain=",
        "--gcc-install-dir=",
        "-mabi=",
        "-mfloat-abi=",
        "-mmacosx-version-min=",
        "-mios-version-min=",
        "-mtargetos=",
    ]
    .iter()
    .any(|p| flag.starts_with(p))
}

/// Decides one call (B-51). `compiler` is the call's first argument; `args` the rest; `env`
/// the call's variables (only names are looked at).
pub fn classify(compiler: &OsString, args: &[OsString], env_names: &[OsString]) -> Classified {
    use Classified::PassThrough;
    if cfg!(windows) {
        return PassThrough("windows");
    }
    if let Some(why) = compiler_refused(compiler) {
        return PassThrough(why);
    }
    if env_names
        .iter()
        .any(|name| name.to_str().is_some_and(dependency_variable))
    {
        return PassThrough("dependency-variable");
    }
    let mut text = Vec::with_capacity(args.len());
    for arg in args {
        match arg.to_str() {
            Some(arg) => text.push(arg.to_string()),
            None => return PassThrough("not-utf8"),
        }
    }
    parse(compiler.clone(), text)
}

/// Whether the compiler is one the launcher never stands in front of: another launcher, or
/// MSVC's driver, by its name or its link's target's.
fn compiler_refused(compiler: &OsString) -> Option<Why> {
    let path = Path::new(compiler);
    let mut names = vec![path.file_name().map(|n| n.to_string_lossy().into_owned())];
    if let Ok(target) = std::fs::read_link(path) {
        names.push(target.file_name().map(|n| n.to_string_lossy().into_owned()));
    }
    for name in names.into_iter().flatten() {
        let stem = name.strip_suffix(".exe").unwrap_or(&name);
        if LAUNCHERS.contains(&stem) {
            return Some("launcher-compiler");
        }
        if stem == "cl" || stem == "clang-cl" || stem.ends_with("-cl") {
            return Some("msvc");
        }
    }
    None
}

fn parse(compiler: OsString, args: Vec<String>) -> Classified {
    use Classified::PassThrough;
    let mut keyed = Vec::new();
    let mut quote = Vec::new();
    let mut search: Vec<Dir> = Vec::new();
    let mut system: Vec<Dir> = Vec::new();
    let mut after: Vec<Dir> = Vec::new();
    let mut driver_flags = Vec::new();
    let mut native = Vec::new();
    let mut includes = Vec::new();
    let (mut compile, mut lto, mut random_seed) = (false, false, false);
    let mut compressed_debug = false;
    let mut source: Option<(String, Lang)> = None;
    let mut output: Option<String> = None;
    let mut dep = DepAsked {
        mode: DepMode::None,
        file: None,
        targets: Vec::new(),
        phony: false,
    };
    // `-x`'s language for the sources after it: `None` is by extension.
    let mut forced: Option<Lang> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg.starts_with('@') {
            return PassThrough("response-file");
        }
        if let Some(why) = refused(arg) {
            return PassThrough(why);
        }
        let value = |i: usize| args.get(i + 1).cloned();
        match arg {
            "-c" => {
                compile = true;
                keyed.push(arg.to_string());
            }
            "-MD" => dep.mode = DepMode::All,
            "-MMD" => {
                if dep.mode == DepMode::None {
                    dep.mode = DepMode::User;
                }
            }
            "-MP" => dep.phony = true,
            "-o" | "-MF" | "-MT" | "-MQ" => {
                let Some(v) = value(i) else {
                    return PassThrough("malformed");
                };
                match arg {
                    "-o" => {
                        if output.replace(v).is_some() {
                            return PassThrough("several-outputs");
                        }
                    }
                    "-MF" => dep.file = Some(v),
                    "-MT" => dep.targets.push(DepTarget::Plain(v)),
                    _ => dep.targets.push(DepTarget::Quoted(v)),
                }
                i += 1;
            }
            "-x" => {
                let Some(v) = value(i) else {
                    return PassThrough("malformed");
                };
                forced = match v.as_str() {
                    "c" => Some(Lang::C),
                    "c++" => Some(Lang::Cxx),
                    "none" => None,
                    "c-header" | "c++-header" | "c-header-unit" | "c++-header-unit"
                    | "c++-system-header" | "c++-user-header" => return PassThrough("pch"),
                    "objective-c"
                    | "objective-c++"
                    | "objective-c-header"
                    | "objective-c++-header" => return PassThrough("objective-c"),
                    "assembler" | "assembler-with-cpp" => return PassThrough("assembly"),
                    "cuda" | "hip" => return PassThrough("cuda"),
                    "c++-module" => return PassThrough("modules"),
                    _ => return PassThrough("language"),
                };
                keyed.push(arg.to_string());
                keyed.push(v);
                i += 1;
            }
            _ if TAKES_VALUE.contains(&arg) => {
                let Some(v) = value(i) else {
                    return PassThrough("malformed");
                };
                if arg == "-Xassembler" && assembler_writes(&v) {
                    return PassThrough("second-output");
                }
                match arg {
                    "-I" => search.push(dir(&v, false, false)),
                    "-F" => search.push(dir(&v, true, false)),
                    "-iquote" => quote.push(dir(&v, false, false)),
                    "-isystem" | "-cxx-isystem" => system.push(dir(&v, false, true)),
                    "-iframework" => system.push(dir(&v, true, true)),
                    "-idirafter" => after.push(dir(&v, false, true)),
                    "-include" | "-imacros" => includes.push(v.clone()),
                    "-isysroot" | "-arch" | "-target" | "--sysroot" | "-stdlib" | "-std" => {
                        if arg != "-std" {
                            driver_flags.push(arg.to_string());
                            driver_flags.push(v.clone());
                        }
                    }
                    _ => {}
                }
                if (arg == "-I" || arg == "-isystem" || arg == "-idirafter" || arg == "-F")
                    && v.starts_with('=')
                {
                    return PassThrough("include-prefix");
                }
                keyed.push(arg.to_string());
                keyed.push(v);
                i += 1;
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                if let Some(v) = arg.strip_prefix("-o") {
                    if output.replace(v.to_string()).is_some() {
                        return PassThrough("several-outputs");
                    }
                } else if let Some(v) = arg.strip_prefix("-MF") {
                    dep.file = Some(v.to_string());
                } else if let Some(v) = arg.strip_prefix("-MT") {
                    dep.targets.push(DepTarget::Plain(v.to_string()));
                } else if let Some(v) = arg.strip_prefix("-MQ") {
                    dep.targets.push(DepTarget::Quoted(v.to_string()));
                } else {
                    attached(
                        arg,
                        &mut search,
                        &mut quote,
                        &mut system,
                        &mut after,
                        &mut includes,
                    );
                    if ["-I=", "-isystem=", "-idirafter="]
                        .iter()
                        .any(|p| arg.starts_with(p))
                    {
                        return PassThrough("include-prefix");
                    }
                    if arg.starts_with("-flto") && arg != "-fno-lto" {
                        lto = true;
                    }
                    if arg.starts_with("-frandom-seed") {
                        random_seed = true;
                    }
                    if (arg.starts_with("-gz") && arg != "-gz=none")
                        || arg.contains("compress-debug-sections")
                    {
                        compressed_debug = true;
                    }
                    if matches!(arg, "-march=native" | "-mcpu=native" | "-mtune=native") {
                        native.push(arg.to_string());
                    } else if driver_flag(arg) {
                        driver_flags.push(arg.to_string());
                    }
                    keyed.push(arg.to_string());
                }
            }
            "-" => return PassThrough("stdin"),
            _ => {
                if let Some(why) = forced.is_none().then(|| other_source(arg)).flatten() {
                    return PassThrough(why);
                }
                let Some(lang) = forced.or_else(|| source_lang(arg)) else {
                    return PassThrough("unknown-input");
                };
                if source.replace((arg.to_string(), lang)).is_some() {
                    return PassThrough("several-sources");
                }
                keyed.push(arg.to_string());
            }
        }
        i += 1;
    }
    if !compile {
        return PassThrough("no-compile");
    }
    let Some((source, lang)) = source else {
        return PassThrough("no-source");
    };
    let Some(output) = output else {
        return PassThrough("no-output");
    };
    if output == "-" {
        return PassThrough("no-output");
    }
    if dep.mode == DepMode::None && (dep.file.is_some() || !dep.targets.is_empty() || dep.phony) {
        return PassThrough("deps-only");
    }
    // A forced include beside which a precompiled header exists is a precompiled header used.
    for include in &includes {
        if include.ends_with(".gch") || include.ends_with(".pch") {
            return PassThrough("pch");
        }
        let gch = format!("{include}.gch");
        let pch = format!("{include}.pch");
        if Path::new(&gch).exists() || Path::new(&pch).exists() {
            return PassThrough("pch");
        }
    }
    search.extend(system);
    search.extend(after);
    Classified::Compile(Box::new(Compile {
        compiler,
        args,
        lang,
        source,
        output,
        dep,
        keyed,
        quote,
        search,
        driver_flags,
        native,
        lto,
        random_seed,
        compressed_debug,
    }))
}

fn dir(path: &str, framework: bool, system: bool) -> Dir {
    Dir {
        path: path.to_string(),
        framework,
        system,
    }
}

/// A search flag with its value attached (`-Ifoo`, `-isystemfoo`, `-iquotefoo`, `-Ffoo`).
fn attached(
    arg: &str,
    search: &mut Vec<Dir>,
    quote: &mut Vec<Dir>,
    system: &mut Vec<Dir>,
    after: &mut Vec<Dir>,
    includes: &mut Vec<String>,
) {
    if let Some(v) = arg.strip_prefix("-isystem") {
        system.push(dir(v, false, true));
    } else if let Some(v) = arg.strip_prefix("-iquote") {
        quote.push(dir(v, false, false));
    } else if let Some(v) = arg.strip_prefix("-idirafter") {
        after.push(dir(v, false, true));
    } else if let Some(v) = arg.strip_prefix("-iframework") {
        system.push(dir(v, true, true));
    } else if let Some(v) = arg.strip_prefix("-include") {
        if !v.is_empty() && !v.starts_with('-') {
            includes.push(v.to_string());
        }
    } else if let Some(v) = arg.strip_prefix("-I") {
        search.push(dir(v, false, false));
    } else if let Some(v) = arg.strip_prefix("-F") {
        search.push(dir(v, true, false));
    } else if let Some(v) = arg.strip_prefix("--include-directory=") {
        search.push(dir(v, false, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(words: &[&str]) -> Classified {
        let args: Vec<OsString> = words.iter().map(OsString::from).collect();
        classify(&OsString::from("/usr/bin/c++"), &args, &[])
    }

    fn compile(words: &[&str]) -> Compile {
        match call(words) {
            Classified::Compile(c) => *c,
            other => panic!("{words:?}: {other:?}"),
        }
    }

    const BASE: &[&str] = &["-O2", "-Iinc", "-c", "a.cc", "-o", "a.o"];

    fn with(extra: &[&str]) -> Vec<&'static str> {
        // Leaks a little in tests to keep the table short.
        let mut v: Vec<&'static str> = BASE.to_vec();
        for e in extra {
            v.push(Box::leak(e.to_string().into_boxed_str()));
        }
        v
    }

    // covers: B-51
    #[test]
    fn cmake_s_compile_is_handled_and_its_depfile_flags_are_kept_apart() {
        let c = compile(&[
            "-DX=1",
            "-I/r/inc",
            "-isystem",
            "/r/sys",
            "-O2",
            "-std=c++17",
            "-MD",
            "-MT",
            "obj/a.cc.o",
            "-MF",
            "obj/a.cc.o.d",
            "-o",
            "obj/a.cc.o",
            "-c",
            "/r/a.cc",
        ]);
        assert_eq!(c.lang, Lang::Cxx);
        assert_eq!(c.source, "/r/a.cc");
        assert_eq!(c.output, "obj/a.cc.o");
        assert_eq!(c.dep.mode, DepMode::All);
        assert_eq!(c.dep.file.as_deref(), Some("obj/a.cc.o.d"));
        assert_eq!(c.dep.targets, vec![DepTarget::Plain("obj/a.cc.o".into())]);
        assert_eq!(
            c.keyed,
            [
                "-DX=1",
                "-I/r/inc",
                "-isystem",
                "/r/sys",
                "-O2",
                "-std=c++17",
                "-c",
                "/r/a.cc"
            ]
        );
        assert_eq!(c.search.len(), 2);
        assert!(c.search[1].system);
        assert_eq!(c.dep.path(&c.output).as_deref(), Some("obj/a.cc.o.d"));
    }

    // covers: B-51
    #[test]
    fn a_language_flag_before_the_source_decides_it() {
        assert_eq!(
            compile(&["-x", "c", "-c", "a.inc", "-o", "a.o"]).lang,
            Lang::C
        );
        assert_eq!(compile(&["-c", "a.c", "-o", "a.o"]).lang, Lang::C);
        assert_eq!(compile(&["-c", "a.C", "-o", "a.o"]).lang, Lang::Cxx);
        let d = compile(&["-MMD", "-c", "src/a.c", "-o", "out/a.o"]);
        assert_eq!(d.dep.path(&d.output).as_deref(), Some("out/a.d"));
    }

    // covers: B-51
    #[test]
    fn every_named_pass_through_has_its_reason() {
        let table: &[(&[&str], &str)] = &[
            (&["a.c", "-o", "a"], "no-compile"),
            (&["-E", "-c", "a.c", "-o", "a.o"], "preprocess"),
            (&["-S", "-c", "a.c", "-o", "a.o"], "assembly-output"),
            (&["-M", "-c", "a.c", "-o", "a.o"], "deps-only"),
            (&["-MM", "-c", "a.c", "-o", "a.o"], "deps-only"),
            (&["-MG", "-MD", "-c", "a.c", "-o", "a.o"], "deps-only"),
            (&["-c", "a.c", "b.c"], "several-sources"),
            (&["-c", "a.c", "b.c", "-o", "a.o"], "several-sources"),
            (&["-c", "-o", "a.o"], "no-source"),
            (&["-c", "a.c"], "no-output"),
            (&["@args.rsp"], "response-file"),
            (&["-x", "c-header", "-c", "a.h", "-o", "a.gch"], "pch"),
            (&["-x", "c++-header", "-c", "a.h", "-o", "a.pch"], "pch"),
            (&["-include-pch", "p.pch", "-c", "a.c", "-o", "a.o"], "pch"),
            (&["-include", "p.h.gch", "-c", "a.c", "-o", "a.o"], "pch"),
            (&["-emit-pch", "-c", "a.c", "-o", "a.o"], "pch"),
            (&["-fmodules", "-c", "a.c", "-o", "a.o"], "modules"),
            (
                &["-fmodule-map-file=m", "-c", "a.c", "-o", "a.o"],
                "modules",
            ),
            (&["-fcxx-modules", "-c", "a.cc", "-o", "a.o"], "modules"),
            (
                &["-fmodule-file=x.pcm", "-c", "a.cc", "-o", "a.o"],
                "modules",
            ),
            (&["-c", "a.cppm", "-o", "a.o"], "modules"),
            (&["-c", "a.ixx", "-o", "a.o"], "modules"),
            (&["-c", "a.mpp", "-o", "a.o"], "modules"),
            (&["-fprofile-generate", "-c", "a.c", "-o", "a.o"], "profile"),
            (&["-fprofile-use=p", "-c", "a.c", "-o", "a.o"], "profile"),
            (
                &["-fprofile-instr-generate", "-c", "a.c", "-o", "a.o"],
                "profile",
            ),
            (&["-fauto-profile", "-c", "a.c", "-o", "a.o"], "profile"),
            (&["--coverage", "-c", "a.c", "-o", "a.o"], "coverage"),
            (&["-ftest-coverage", "-c", "a.c", "-o", "a.o"], "coverage"),
            (&["-fprofile-arcs", "-c", "a.c", "-o", "a.o"], "coverage"),
            (
                &["-fcoverage-mapping", "-c", "a.c", "-o", "a.o"],
                "coverage",
            ),
            (
                &["-gsplit-dwarf", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (&["-save-temps", "-c", "a.c", "-o", "a.o"], "second-output"),
            (
                &["-save-temps=obj", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (&["-ftime-trace", "-c", "a.c", "-o", "a.o"], "second-output"),
            (
                &["-fstack-usage", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-fcallgraph-info", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-fdump-tree-all", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-MJ", "x.json", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-fcrash-diagnostics-dir=d", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-fopt-info-vec=f.txt", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-gsplit-dwarf=single", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-fcallgraph-info=su", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-fdiagnostics-format=sarif-file", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-fdiagnostics-add-output=sarif", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-Wa,-adhln=a.lst", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-Xassembler", "-adhln=a.lst", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (
                &["-Wa,--MD,a.ad", "-c", "a.c", "-o", "a.o"],
                "second-output",
            ),
            (&["-fplugin=p.so", "-c", "a.c", "-o", "a.o"], "loads-code"),
            (
                &["-fpass-plugin=p.so", "-c", "a.c", "-o", "a.o"],
                "loads-code",
            ),
            (
                &["-Xclang", "-load", "-c", "a.c", "-o", "a.o"],
                "loads-code",
            ),
            (&["-mllvm", "-x", "-c", "a.c", "-o", "a.o"], "loads-code"),
            (
                &["--config", "c.cfg", "-c", "a.c", "-o", "a.o"],
                "loads-code",
            ),
            (&["--config=c.cfg", "-c", "a.c", "-o", "a.o"], "loads-code"),
            (&["-B", "/x", "-c", "a.c", "-o", "a.o"], "loads-code"),
            (&["-B/x", "-c", "a.c", "-o", "a.o"], "loads-code"),
            (&["-specs=s", "-c", "a.c", "-o", "a.o"], "loads-code"),
            (&["-wrapper", "gdb", "-c", "a.c", "-o", "a.o"], "loads-code"),
            (&["-c", "a.m", "-o", "a.o"], "objective-c"),
            (&["-c", "a.mm", "-o", "a.o"], "objective-c"),
            (
                &["-x", "objective-c", "-c", "a.c", "-o", "a.o"],
                "objective-c",
            ),
            (&["-c", "a.S", "-o", "a.o"], "assembly"),
            (&["-c", "a.s", "-o", "a.o"], "assembly"),
            (&["-c", "a.cu", "-o", "a.o"], "cuda"),
            (&["-x", "cuda", "-c", "a.c", "-o", "a.o"], "cuda"),
            (&["-x", "rust", "-c", "a.c", "-o", "a.o"], "language"),
            (
                &["-Wp,-MD,a.d", "-c", "a.c", "-o", "a.o"],
                "dependency-flags",
            ),
            (&["-c", "-", "-o", "a.o"], "stdin"),
            (&["-c", "a.c", "x.o", "-o", "a.o"], "link"),
            (&["-c", "a.c", "README", "-o", "a.o"], "unknown-input"),
            (&["-c", "a.c", "-o"], "malformed"),
            (&["-MF", "a.d", "-c", "a.c", "-o", "a.o"], "deps-only"),
            (
                &["-iprefix", "/p", "-c", "a.c", "-o", "a.o"],
                "include-prefix",
            ),
            (&["-I=/inc", "-c", "a.c", "-o", "a.o"], "include-prefix"),
        ];
        for (words, why) in table {
            assert_eq!(call(words), Classified::PassThrough(why), "{words:?}");
        }
    }

    // covers: B-51
    #[test]
    fn a_compiler_that_is_a_launcher_or_msvc_passes_through() {
        let args: Vec<OsString> = BASE.iter().map(OsString::from).collect();
        for name in [
            "ccache",
            "/usr/bin/sccache",
            "distcc",
            "icecc",
            "buildcache",
        ] {
            assert_eq!(
                classify(&OsString::from(name), &args, &[]),
                Classified::PassThrough("launcher-compiler"),
                "{name}"
            );
        }
        for name in ["cl", "cl.exe", "clang-cl", "/opt/llvm/bin/clang-cl"] {
            assert_eq!(
                classify(&OsString::from(name), &args, &[]),
                Classified::PassThrough("msvc"),
                "{name}"
            );
        }
        // A link named otherwise whose target is ccache.
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("c++");
        std::os::unix::fs::symlink("/usr/local/bin/ccache", &link).unwrap();
        assert_eq!(
            classify(&link.into_os_string(), &args, &[]),
            Classified::PassThrough("launcher-compiler")
        );
    }

    // covers: B-51
    #[test]
    fn variables_that_write_or_log_pass_through() {
        let args: Vec<OsString> = BASE.iter().map(OsString::from).collect();
        for name in [
            "DEPENDENCIES_OUTPUT",
            "SUNPRO_DEPENDENCIES",
            "CC_PRINT_HEADERS",
            "CC_PRINT_HEADERS_FILE",
            "CC_LOG_DIAGNOSTICS",
        ] {
            assert_eq!(
                classify(&OsString::from("cc"), &args, &[OsString::from(name)]),
                Classified::PassThrough("dependency-variable"),
                "{name}"
            );
        }
    }

    // covers: B-51
    #[test]
    fn a_precompiled_header_beside_a_forced_include_passes_through() {
        let dir = tempfile::tempdir().unwrap();
        let header = dir.path().join("p.h");
        std::fs::write(&header, "").unwrap();
        let h = header.to_str().unwrap().to_string();
        assert!(matches!(
            call(&with(&["-include", &h])),
            Classified::Compile(_)
        ));
        std::fs::write(format!("{h}.gch"), "").unwrap();
        assert_eq!(
            call(&with(&["-include", &h])),
            Classified::PassThrough("pch")
        );
    }

    // covers: B-51, B-52, B-53
    #[test]
    fn target_lto_and_native_flags_are_seen() {
        let c = compile(&with(&[
            "-target",
            "x86_64-apple-macos13",
            "-m64",
            "--sysroot=/s",
            "-march=native",
            "-flto=thin",
            "-stdlib=libc++",
        ]));
        assert_eq!(
            c.driver_flags,
            [
                "-target",
                "x86_64-apple-macos13",
                "-m64",
                "--sysroot=/s",
                "-stdlib=libc++"
            ]
        );
        assert_eq!(c.native, ["-march=native"]);
        assert!(c.lto && !c.random_seed);
        assert!(compile(&with(&["-flto", "-frandom-seed=x"])).random_seed);
        assert!(compile(&with(&["-g", "-gz=zlib"])).compressed_debug);
        assert!(compile(&with(&["-Wa,--compress-debug-sections"])).compressed_debug);
        assert!(!compile(&with(&["-g", "-gz=none"])).compressed_debug);
    }
}
