//! What the hook and the helper say to each other (B-100): a call, the helper's answer, the
//! record of a compile, and the plan the helper keeps between a lookup and a store.

use std::collections::BTreeMap;

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

/// The schema of a call.
pub const CALL_SCHEMA: &str = "hushrun.tsc-call/v1";
/// The schema of a record.
pub const RECORD_SCHEMA: &str = "hushrun.tsc-record/v1";

/// One compiler call, as the hook saw it before the compiler loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub schema: String,
    /// The hook's row: `tsc`.
    pub program: String,
    /// The compiler's package folder.
    pub package: String,
    /// The package's version.
    pub version: String,
    /// The file whose bytes are the compiler's identity (B-102).
    pub compiler: String,
    /// The arguments after the program.
    pub args: Vec<String>,
    /// The working directory, as Node's `process.cwd()` gives it.
    pub cwd: String,
    /// Whether stdout is a terminal: the compiler's default for `--pretty`.
    pub tty: bool,
    /// Node's `process.platform`.
    pub platform: String,
    /// The variables the compiler reads, those set (B-102).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub pid: u32,
}

/// The helper's answer to a lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Answer {
    /// Run the compiler exactly as asked, and record nothing.
    Pass,
    /// The files are in place: print these and end with `code`.
    Reused {
        stdout: String,
        stderr: String,
        code: i32,
    },
    /// Run the compiler, record what it does into `record`, then call `store <plan>`.
    Record { record: String, plan: String },
}

/// What the hook recorded of one compile, as the process ended.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub schema: String,
    /// The process's exit code.
    pub code: i32,
    /// Each file read: its path, its SHA-256 as read, its size.
    #[serde(default)]
    pub reads: Vec<(String, String, u64)>,
    /// Each path looked at: `f`, `d`, `o` (another kind) or `-` (absent).
    #[serde(default)]
    pub looks: Vec<(String, String)>,
    /// Each folder listed, with its entries' names and kinds (`f`, `d`, `l`, `o`), or none
    /// when it couldn't be.
    #[serde(default)]
    pub lists: Vec<(String, Option<Vec<(String, String)>>)>,
    /// Each path resolved, with what it resolved to, or none when it couldn't be.
    #[serde(default)]
    pub links: Vec<(String, Option<String>)>,
    /// Each file written, in the order first written.
    #[serde(default)]
    pub writes: Vec<String>,
    /// What the compiler did that a block can't hold or key, in words.
    #[serde(default)]
    pub other: Vec<String>,
    /// The hook's own errors.
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

/// What the helper keeps between a lookup that answered [`Answer::Record`] and its store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub call: Call,
    /// The first key.
    pub step: Digest,
    /// The compiler's identity (B-102).
    pub identity: Digest,
    /// When the lookup began, in milliseconds since the epoch.
    pub start_ms: u64,
    /// A block found that this compile is compared with (observe, an audit).
    #[serde(default)]
    pub found: Option<Digest>,
    /// The first key is suspended: compile, store nothing.
    #[serde(default)]
    pub suspended: bool,
    /// Where the hook writes its record.
    pub record: String,
    /// Whether the call is `--build`.
    #[serde(default)]
    pub build: bool,
}
