//! TypeScript compiler build blocks (B-100 to B-109, draft): what `tsc` and `tsc --build` write
//! and print, reused between worktrees and runs, keyed by what the compiler read.
//!
//! `hushrun run` adds `--require <hook>` to COMMAND's `NODE_OPTIONS` ([`PRELOAD`], written once
//! per content into the store's folder) and sets [`HUSHRUN_TSC`] to a link `hushrun-tsc` beside
//! `hushrun-rustc`. In a Node process that runs a compiler the hook's table names (TypeScript 4
//! to 6's `tsc`, which compiles in that process), the hook asks the helper before the compiler
//! loads ([`helper::lookup`]); on a miss it records what the compiler read, looked at, listed,
//! resolved, wrote and printed through Node's `fs`, and hands it to the helper as the process
//! ends ([`helper::store`]). In every other Node process it does nothing.
//!
//! For each call the helper ends in one outcome: **reused** (the files, messages and status were
//! put in place and the compiler did not start), **built and stored**, **built, not stored**
//! (with the reason) or **passed through** (the compiler ran exactly as asked, with the reason,
//! B-101).
//!
//! # Rules every module here keeps
//!
//! 1. **Every failure before the helper writes a file runs the compiler as asked** (B-106): the
//!    hook loads nothing and the compiler starts. After a reuse is decided, a restore that fails
//!    part way is a pass-through too: the compiler then writes over what was put in place.
//! 2. **A block exists under a key only because a compile read exactly those bytes and saw
//!    exactly those folders, kinds and links** (B-103). A hint only says what to hash and look
//!    at; the digests of what was read are the hook's, of the very bytes the compiler got.
//! 3. **A restored file is a new file**: written under a temporary name in its folder, checked,
//!    dated now and renamed into place (B-105).
//! 4. **Nothing reads the clock but the helper's entry points**, which pass a time in.

pub mod args;
pub mod call;
pub mod helper;
pub mod key;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The name the binary is called by when the hook asks it (B-100). `main` looks at its own name
/// before anything else.
pub const PROGRAM_NAME: &str = "hushrun-tsc";

/// The variable the hook reads the helper from, and the one that makes it do anything at all.
pub const HUSHRUN_TSC: &str = "HUSHRUN_TSC";

/// The kind of a TypeScript block (B-108).
pub const KIND: &str = "tsc/1";

/// The hook Node loads into every process of COMMAND (B-100).
pub const PRELOAD: &str = include_str!("hushrun_tsc.cjs");

/// The store's folder the hook and the compilers' identities live in.
pub const FOLDER: &str = "tsc";

/// What a run's session holds for the helper: the checkout's root, its given form first, then
/// its resolved one when it differs. A session without it has no helper: a call passes through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TscRun {
    pub roots: Vec<String>,
}

/// Whether the binary was called by the helper's name (`argv[0]`, through any folder).
pub fn called_as_helper(argv0: &OsStr) -> bool {
    Path::new(argv0)
        .file_name()
        .is_some_and(|name| name == PROGRAM_NAME)
}

/// The helper's entry point: `lookup` or `store <plan>`.
pub fn main() -> std::process::ExitCode {
    helper::main()
}

/// Writes the hook into `<store>/tsc/`, named by its content, when it isn't there: a path that
/// stays valid after the run, so that a Node process that outlives it, holding `NODE_OPTIONS`,
/// still starts. The file's path.
pub fn install_preload(store: &Path) -> std::io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(PRELOAD.as_bytes()).hex();
    let dir = store.join(FOLDER);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("hushrun-tsc-{}.cjs", &digest[..16]));
    if std::fs::read(&file).ok().as_deref() == Some(PRELOAD.as_bytes()) {
        return Ok(file);
    }
    let temp = dir.join(crate::baked::unique(".hushrun-tsc.cjs.tmp"));
    std::fs::write(&temp, PRELOAD)?;
    std::fs::rename(&temp, &file)?;
    Ok(file)
}

/// `NODE_OPTIONS` with `--require <file>` added to what COMMAND's environment had (B-100): in
/// double quotes, backslashes doubled, when the path has white space, as Node splits it. `None`
/// for a path with a quote, which `NODE_OPTIONS` can't hold.
pub fn node_options(had: Option<&OsStr>, file: &Path) -> Option<OsString> {
    let text = file.to_str()?;
    if text.contains('"') {
        return None;
    }
    let word = if text.chars().any(char::is_whitespace) {
        format!("\"{}\"", text.replace('\\', "\\\\"))
    } else {
        text.to_string()
    };
    let mut options = had
        .filter(|o| !o.is_empty())
        .map(OsStr::to_os_string)
        .unwrap_or_default();
    if !options.is_empty() {
        options.push(" ");
    }
    options.push(format!("--require {word}"));
    Some(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-100
    #[test]
    fn the_helper_is_known_by_its_own_name() {
        assert!(called_as_helper(OsStr::new("hushrun-tsc")));
        assert!(called_as_helper(OsStr::new("/a b/bin/x/hushrun-tsc")));
        assert!(!called_as_helper(OsStr::new("hushrun")));
        assert!(!called_as_helper(OsStr::new("hushrun-cc")));
        assert!(!called_as_helper(OsStr::new("hushrun-tsc.cjs")));
    }

    // covers: B-100
    #[test]
    fn the_hook_is_added_to_what_node_options_had() {
        let file = Path::new("/s/tsc/hushrun-tsc-0.cjs");
        assert_eq!(
            node_options(None, file).unwrap(),
            "--require /s/tsc/hushrun-tsc-0.cjs"
        );
        assert_eq!(
            node_options(Some(OsStr::new("--max-old-space-size=4096")), file).unwrap(),
            "--max-old-space-size=4096 --require /s/tsc/hushrun-tsc-0.cjs"
        );
        assert_eq!(
            node_options(Some(OsStr::new("")), Path::new("/a b/h.cjs")).unwrap(),
            "--require \"/a b/h.cjs\""
        );
        assert!(node_options(None, Path::new("/a\"b/h.cjs")).is_none());
    }

    // covers: B-100
    #[test]
    fn the_hook_is_written_once_by_its_content() {
        let dir = tempfile::tempdir().unwrap();
        let first = install_preload(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&first).unwrap(), PRELOAD);
        let again = install_preload(dir.path()).unwrap();
        assert_eq!(first, again);
        assert_eq!(
            std::fs::read_dir(dir.path().join(FOLDER)).unwrap().count(),
            1
        );
    }
}
