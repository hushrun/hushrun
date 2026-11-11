//! The words the modules share: mode, class, outcome, the reasons, roots and rooted paths.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The mode of reuse (B-1): one per user, `off` until the person turns it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Nothing is read from the store or written to it.
    #[default]
    Off,
    /// Every compile runs; blocks are stored and compared with what a reuse would have given.
    Observe,
    /// A compile whose block exists is not run.
    On,
}

impl Mode {
    /// The mode as the settings file and `HUSHRUN_BUILD` write it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Observe => "observe",
            Mode::On => "on",
        }
    }

    /// Reads a mode as written by [`Mode::as_str`]. Anything else is not a mode.
    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "off" => Some(Mode::Off),
            "observe" => Some(Mode::Observe),
            "on" => Some(Mode::On),
            _ => None,
        }
    }
}

/// What a compiler call produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    /// A `lib` or `rlib`, also the metadata-only one `cargo check` asks for.
    Library,
    /// A `proc-macro`, loaded by the compiler itself.
    ProcMacro,
    /// A `bin` named `build_script_*`: the build script cargo then runs.
    BuildScript,
    /// Any other `bin`, or a test binary (`--test`).
    Program,
}

impl Class {
    /// Every class, for tests and tables.
    pub const ALL: [Class; 4] = [
        Class::Library,
        Class::ProcMacro,
        Class::BuildScript,
        Class::Program,
    ];

    /// The class as the run's log writes it. Stable.
    pub const fn as_str(self) -> &'static str {
        match self {
            Class::Library => "library",
            Class::ProcMacro => "proc-macro",
            Class::BuildScript => "build-script",
            Class::Program => "program",
        }
    }
}

/// Why a call ran exactly as asked, storing nothing (B-4, B-19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PassedThrough {
    /// The first argument is not rustc: clippy's driver, or a compiler under another name.
    NotRustc,
    /// No `--crate-name`.
    NoCrateName,
    /// `--print`, `-vV` or a source of `-`: one of cargo's probes.
    Probe,
    /// An `@file` argument.
    ArgFile,
    /// A `--target` that names a file, or a name that is not one of the compiler's built-in
    /// targets: rustc would load `<name>.json` from `RUST_TARGET_PATH` or the working directory.
    TargetFile,
    /// A `--sysroot`: the standard library comes from a folder the compiler's identity doesn't
    /// cover.
    Sysroot,
    /// A `-Z` flag outside the short list of ones that read no file.
    Unstable,
    /// An `--extern` with no path other than `proc_macro`: rustc finds the crate through the
    /// search folders, and nothing hashes it.
    ExternWithoutPath,
    /// A `--remap-path-prefix` of the caller's.
    CallerRemap,
    /// `-C instrument-coverage`, `-C profile-generate` or `-C profile-use`.
    Instrumented,
    /// An `--emit` kind other than `dep-info`, `metadata` and `link`, or one written `kind=path`.
    Emit,
    /// More than one `--crate-type`.
    CrateTypes,
    /// A crate type of `cdylib`, `dylib`, `staticlib`, or one this crate doesn't know.
    CrateType,
    /// No `--out-dir`.
    NoOutDir,
    /// No `-C extra-filename`.
    NoExtraFilename,
    /// No `--json` with `artifacts`: cargo would not learn when an output is ready.
    NoArtifacts,
    /// The package is on the person's never-store list.
    NeverStore,
    /// The compiler's identity can't be established (B-17).
    Compiler,
    /// No `--crate-type` and no `--test`: the source may choose the type itself.
    NoCrateType,
    /// An `-o`: the output file is named instead of the output folder.
    OutputFile,
    /// An argument, the working directory or a keyed variable is not UTF-8.
    NotUtf8,
    /// An argument this crate doesn't know how to key (rule 6).
    UnknownArgument,
    /// A flag without its value, or a second source file.
    Malformed,
    /// The mode is `off`.
    Off,
    /// Running in CI (C-14's detection).
    Ci,
    /// Running on Windows.
    Windows,
    /// The settings file can't be read.
    Settings,
    /// The store can't be written.
    Store,
    /// A compile that asks for `-C incremental`, finds no block and already has its dep-info
    /// file: an edit in a warm target, kept incremental (B-9).
    Incremental,
    /// The call's roots can't be used (B-8): a relative working directory, a root of `/`.
    Roots,
}

impl PassedThrough {
    /// Every reason, for tests and tables.
    pub const ALL: [PassedThrough; 30] = [
        PassedThrough::NotRustc,
        PassedThrough::NoCrateName,
        PassedThrough::Probe,
        PassedThrough::ArgFile,
        PassedThrough::TargetFile,
        PassedThrough::Sysroot,
        PassedThrough::Unstable,
        PassedThrough::ExternWithoutPath,
        PassedThrough::CallerRemap,
        PassedThrough::Instrumented,
        PassedThrough::Emit,
        PassedThrough::CrateTypes,
        PassedThrough::CrateType,
        PassedThrough::NoOutDir,
        PassedThrough::NoExtraFilename,
        PassedThrough::NoArtifacts,
        PassedThrough::NeverStore,
        PassedThrough::Compiler,
        PassedThrough::NoCrateType,
        PassedThrough::OutputFile,
        PassedThrough::NotUtf8,
        PassedThrough::UnknownArgument,
        PassedThrough::Malformed,
        PassedThrough::Off,
        PassedThrough::Ci,
        PassedThrough::Windows,
        PassedThrough::Settings,
        PassedThrough::Store,
        PassedThrough::Incremental,
        PassedThrough::Roots,
    ];

    /// The reason as the run's log writes it. Stable: change it only with the log's readers.
    pub const fn as_str(self) -> &'static str {
        match self {
            PassedThrough::NotRustc => "not-rustc",
            PassedThrough::NoCrateName => "no-crate-name",
            PassedThrough::Probe => "probe",
            PassedThrough::ArgFile => "arg-file",
            PassedThrough::TargetFile => "target-file",
            PassedThrough::Sysroot => "sysroot",
            PassedThrough::Unstable => "unstable",
            PassedThrough::ExternWithoutPath => "extern-without-path",
            PassedThrough::CallerRemap => "caller-remap",
            PassedThrough::Instrumented => "instrumented",
            PassedThrough::Emit => "emit",
            PassedThrough::CrateTypes => "crate-types",
            PassedThrough::CrateType => "crate-type",
            PassedThrough::NoOutDir => "no-out-dir",
            PassedThrough::NoExtraFilename => "no-extra-filename",
            PassedThrough::NoArtifacts => "no-artifacts",
            PassedThrough::NeverStore => "never-store",
            PassedThrough::Compiler => "compiler",
            PassedThrough::NoCrateType => "no-crate-type",
            PassedThrough::OutputFile => "output-file",
            PassedThrough::NotUtf8 => "not-utf8",
            PassedThrough::UnknownArgument => "unknown-argument",
            PassedThrough::Malformed => "malformed",
            PassedThrough::Off => "off",
            PassedThrough::Ci => "ci",
            PassedThrough::Windows => "windows",
            PassedThrough::Settings => "settings",
            PassedThrough::Store => "store",
            PassedThrough::Incremental => "incremental",
            PassedThrough::Roots => "roots",
        }
    }
}

/// Why a compile that ran was not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotStored {
    /// rustc exited with an error.
    RustcFailed,
    /// A read file changed while compiling or carries a date at or after its start (B-11).
    Changed,
    /// The store's volume has less free space than `min_free_gb` (B-22).
    LowSpace,
    /// A native library (`-l`) named in a form whose file can't be found with certainty:
    /// `+verbatim`, a rename or `:file`, a kind this crate doesn't know (B-6).
    NativeOutside,
    /// The build script's saved output was not found.
    ScriptOutputMissing,
    /// The libraries handed to the linker could not be read (B-18).
    LinkInputs,
    /// The first key is suspended (B-15).
    Suspended,
    /// The dep-info file is missing, unreadable or not UTF-8.
    DepInfo,
    /// The store refused the block.
    StoreRefused,
    /// Computing a key failed before the compile; rustc ran as asked (B-19).
    Key,
    /// Reading the store failed before the compile; rustc ran as asked (B-19).
    StoreRead,
    /// Putting a block's files in place failed before any message; rustc ran as asked (B-12).
    Restore,
    /// Hashing what the compile read failed.
    Hash,
    /// Searching the outputs for the roots failed (B-10).
    Scan,
    /// The second compile that proves a root read through a variable failed (B-10).
    PlaceCheck,
    /// The compiler's messages could not be recorded.
    Messages,
    /// The fresh outputs could not be compared with the block's (B-16).
    Compare,
    /// The fresh outputs differ from the block's (B-16). The key is suspended.
    Differed,
    /// Storing was planned and nothing stopped it, but it was not tried.
    StoreSkipped,
    /// The outputs rustc announced are not the ones this call names, or lie outside the output
    /// folder.
    Outputs,
    /// A program wrote debug files beside it that a block doesn't carry (`.dSYM`, `.dwp`).
    DebugFiles,
    /// A read lies at a path that can't be keyed: not UTF-8, or not under the working
    /// directory's forms.
    Unkeyable,
}

impl NotStored {
    /// Every reason, for tests and tables.
    pub const ALL: [NotStored; 22] = [
        NotStored::RustcFailed,
        NotStored::Changed,
        NotStored::LowSpace,
        NotStored::NativeOutside,
        NotStored::ScriptOutputMissing,
        NotStored::LinkInputs,
        NotStored::Suspended,
        NotStored::DepInfo,
        NotStored::StoreRefused,
        NotStored::Key,
        NotStored::StoreRead,
        NotStored::Restore,
        NotStored::Hash,
        NotStored::Scan,
        NotStored::PlaceCheck,
        NotStored::Messages,
        NotStored::Compare,
        NotStored::Differed,
        NotStored::StoreSkipped,
        NotStored::Outputs,
        NotStored::DebugFiles,
        NotStored::Unkeyable,
    ];

    /// The reason as the run's log writes it. Stable: change it only with the log's readers.
    pub const fn as_str(self) -> &'static str {
        match self {
            NotStored::RustcFailed => "rustc-failed",
            NotStored::Changed => "changed",
            NotStored::LowSpace => "low-space",
            NotStored::NativeOutside => "native-outside",
            NotStored::ScriptOutputMissing => "script-output-missing",
            NotStored::LinkInputs => "link-inputs",
            NotStored::Suspended => "suspended",
            NotStored::DepInfo => "dep-info",
            NotStored::StoreRefused => "store-refused",
            NotStored::Key => "key",
            NotStored::StoreRead => "store-read",
            NotStored::Restore => "restore",
            NotStored::Hash => "hash",
            NotStored::Scan => "scan",
            NotStored::PlaceCheck => "place-check",
            NotStored::Messages => "messages",
            NotStored::Compare => "compare",
            NotStored::Differed => "differed",
            NotStored::StoreSkipped => "store-skipped",
            NotStored::Outputs => "outputs",
            NotStored::DebugFiles => "debug-files",
            NotStored::Unkeyable => "unkeyable",
        }
    }
}

/// How one compiler call ended (B-3). Only [`crate::decide`] makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "reason", rename_all = "kebab-case")]
pub enum Outcome {
    /// The block's files were put in place and rustc did not start.
    Reused,
    /// rustc ran and the store holds its block.
    BuiltAndStored,
    /// rustc ran and nothing was stored, for this reason.
    BuiltNotStored(NotStored),
    /// rustc ran exactly as asked, for this reason.
    PassedThrough(PassedThrough),
}

impl Outcome {
    /// The outcome's kind as the run's log writes it. Stable.
    pub const fn as_str(self) -> &'static str {
        match self {
            Outcome::Reused => "reused",
            Outcome::BuiltAndStored => "built-and-stored",
            Outcome::BuiltNotStored(_) => "built-not-stored",
            Outcome::PassedThrough(_) => "passed-through",
        }
    }

    /// The reason, for the two outcomes that carry one.
    pub const fn reason(self) -> Option<&'static str> {
        match self {
            Outcome::Reused | Outcome::BuiltAndStored => None,
            Outcome::BuiltNotStored(why) => Some(why.as_str()),
            Outcome::PassedThrough(why) => Some(why.as_str()),
        }
    }
}

/// The name of a root (B-8): a folder a compile's paths start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RootName {
    /// rustc's working directory.
    Workspace,
    /// `CARGO_MANIFEST_DIR`.
    Package,
    /// `OUT_DIR`: the build script's output.
    ScriptOut,
    /// `--out-dir`.
    OutDir,
    /// The i-th `-L dependency=` folder, counted from 0.
    Deps(u8),
}

impl RootName {
    /// The name as records and placeholders write it: `workspace`, `package`, `script-out`,
    /// `out-dir`, `deps-<i>`. Stable.
    pub fn as_string(self) -> String {
        match self {
            RootName::Workspace => "workspace".to_string(),
            RootName::Package => "package".to_string(),
            RootName::ScriptOut => "script-out".to_string(),
            RootName::OutDir => "out-dir".to_string(),
            RootName::Deps(i) => format!("deps-{i}"),
        }
    }

    /// Reads a name written by [`RootName::as_string`]. `deps-01` is not a name: one value has
    /// one spelling.
    pub fn parse(text: &str) -> Option<RootName> {
        match text {
            "workspace" => Some(RootName::Workspace),
            "package" => Some(RootName::Package),
            "script-out" => Some(RootName::ScriptOut),
            "out-dir" => Some(RootName::OutDir),
            _ => {
                let digits = text.strip_prefix("deps-")?;
                let i: u8 = digits.parse().ok()?;
                (i.to_string() == digits).then_some(RootName::Deps(i))
            }
        }
    }
}

impl fmt::Display for RootName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_string())
    }
}

impl Serialize for RootName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RootName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        RootName::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("{text:?} is not a root name")))
    }
}

/// A path recorded from a root: the root's name and the rest, `/`-separated, no leading slash.
/// An empty rest is the root itself.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Rooted {
    /// The root the path starts from.
    pub root: RootName,
    /// The path under it.
    pub rest: String,
}

/// Where a path lies (B-8).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Located {
    /// Under a root: recorded from it, looked up under the current one.
    Rooted(Rooted),
    /// Under a shared folder (cargo home, toolchain, system): the same path for every checkout
    /// on this machine.
    Shared(String),
    /// Anywhere else, as an absolute path. A read here ties the block to its roots.
    Outside(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serde_name<T: Serialize>(value: &T) -> String {
        match serde_json::to_value(value).unwrap() {
            serde_json::Value::String(text) => text,
            other => panic!("not a string: {other}"),
        }
    }

    // covers: B-3
    #[test]
    fn written_names_match_serde_and_are_distinct() {
        let mut seen = std::collections::BTreeSet::new();
        for why in PassedThrough::ALL {
            assert_eq!(serde_name(&why), why.as_str());
            let back: PassedThrough =
                serde_json::from_value(serde_json::Value::String(why.as_str().into())).unwrap();
            assert_eq!(back, why);
            assert!(seen.insert(("p", why.as_str())));
        }
        for why in NotStored::ALL {
            assert_eq!(serde_name(&why), why.as_str());
            assert!(seen.insert(("n", why.as_str())));
        }
        for class in Class::ALL {
            assert_eq!(serde_name(&class), class.as_str());
        }
    }

    // covers: B-3
    #[test]
    fn outcome_serializes_kind_and_reason() {
        let cases = [
            (Outcome::Reused, r#"{"outcome":"reused"}"#),
            (Outcome::BuiltAndStored, r#"{"outcome":"built-and-stored"}"#),
            (
                Outcome::BuiltNotStored(NotStored::Changed),
                r#"{"outcome":"built-not-stored","reason":"changed"}"#,
            ),
            (
                Outcome::PassedThrough(PassedThrough::NotRustc),
                r#"{"outcome":"passed-through","reason":"not-rustc"}"#,
            ),
        ];
        for (outcome, json) in cases {
            assert_eq!(serde_json::to_string(&outcome).unwrap(), json);
            assert_eq!(serde_json::from_str::<Outcome>(json).unwrap(), outcome);
            let kind: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(kind["outcome"], outcome.as_str());
        }
        assert_eq!(
            Outcome::BuiltNotStored(NotStored::LowSpace).reason(),
            Some("low-space")
        );
        assert_eq!(Outcome::Reused.reason(), None);
    }

    // covers: B-8
    #[test]
    fn root_names_have_one_spelling() {
        for name in [
            RootName::Workspace,
            RootName::Package,
            RootName::ScriptOut,
            RootName::OutDir,
            RootName::Deps(0),
            RootName::Deps(255),
        ] {
            assert_eq!(RootName::parse(&name.as_string()), Some(name));
            assert_eq!(serde_name(&name), name.as_string());
        }
        for bad in ["deps-01", "deps-256", "deps-", "deps--1", "Workspace", ""] {
            assert_eq!(RootName::parse(bad), None, "{bad}");
        }
        let located = Located::Rooted(Rooted {
            root: RootName::Deps(2),
            rest: "libx.rlib".into(),
        });
        let json = serde_json::to_string(&located).unwrap();
        assert_eq!(json, r#"{"rooted":{"root":"deps-2","rest":"libx.rlib"}}"#);
        assert_eq!(serde_json::from_str::<Located>(&json).unwrap(), located);
    }

    #[test]
    fn mode_round_trips() {
        for mode in [Mode::Off, Mode::Observe, Mode::On] {
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(Mode::parse("ON"), None);
    }
}
