//! C and C++ build blocks (B-50 to B-62, draft): objects reused between worktrees through
//! CMake's compiler launcher.
//!
//! `hushrun run` sets `CMAKE_C_COMPILER_LAUNCHER` and `CMAKE_CXX_COMPILER_LAUNCHER` for COMMAND to
//! [`INDIRECTION`], a shell line that names nothing of hushrun's: outside a run, where
//! [`HUSHRUN_CC`] is unset, the shell replaces itself with the compiler. Inside one,
//! `HUSHRUN_CC` names a link `hushrun-cc` beside `hushrun-rustc`, and CMake's build starts
//! `hushrun-cc <compiler> <arguments>` for each translation unit ([`launcher`]).
//!
//! For each call the launcher ends in one outcome: **reused** (the object, its depfile and its
//! messages were put in place and the compiler did not start), **built and stored**, **built,
//! not stored** (with the reason) or **passed through** (the compiler ran exactly as asked, with
//! the reason, B-51).
//!
//! # Rules every module here keeps
//!
//! 1. **Every failure before the launcher writes a byte or starts a compile of its own runs the
//!    compiler exactly as asked** (B-58), by replacing the process, so its stdout, stderr and
//!    exit status are the compiler's own. A panic is one of those failures.
//! 2. **A block exists under a key only because a compile read exactly those bytes and saw
//!    exactly those places** (B-54). A hint only says what to hash and look at.
//! 3. **A restored object is a new file**: copied under a temporary name in its folder, checked,
//!    dated now and renamed into place (B-57). The depfile is written whole the same way.
//! 4. **Nothing reads the clock but the launcher's entry point**, which passes a time in.

pub mod args;
pub mod cmake;
pub mod compiler;
pub mod depfile;
pub mod graph;
pub mod key;
pub mod launcher;
pub mod memo;
pub mod places;
pub mod text;

use std::ffi::OsStr;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The name the binary is called by when it stands in front of a C or C++ compiler (B-50).
/// `main` looks at its own name before anything else.
pub const LAUNCHER_NAME: &str = "hushrun-cc";

/// The variable the indirection reads the launcher from (owner decision 3). Frozen in the CMake
/// caches of build folders configured inside a run: renaming it leaves those folders without
/// reuse until they are configured again.
pub const HUSHRUN_CC: &str = "HUSHRUN_CC";

/// The variables CMake reads a launcher from, for C and for C++.
pub const CMAKE_LAUNCHERS: [&str; 2] = ["CMAKE_C_COMPILER_LAUNCHER", "CMAKE_CXX_COMPILER_LAUNCHER"];

/// What `hushrun run` sets the launcher variables to (B-50): a CMake list that runs `/bin/sh`,
/// which replaces itself with `$HUSHRUN_CC <compiler> <arguments>` when the variable is set and
/// with `<compiler> <arguments>` when it isn't.
pub const INDIRECTION: &str = r#"/bin/sh;-c;exec ${HUSHRUN_CC:+"$HUSHRUN_CC"} "$0" "$@""#;

/// The kind of a C or C++ block on the wire (B-60).
pub const KIND: &str = "cc/1";

/// What a run's session holds for the launcher: the checkout's root and whether the launcher
/// maps it (B-55). A session without it has no launcher: a call passes through (B-62).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CcRun {
    /// The checkout's root, its given form first, then its resolved one when it differs.
    pub roots: Vec<String>,
    /// Whether the launcher adds `-ffile-prefix-map=<root>=.` (`build: cc: maps:`).
    pub maps: bool,
}

pub use cmake::configured_before;

/// Whether the binary was called by the launcher's name (`argv[0]`, through any folder).
pub fn called_as_launcher(argv0: &OsStr) -> bool {
    Path::new(argv0)
        .file_name()
        .is_some_and(|name| name == LAUNCHER_NAME)
}

/// The launcher's entry point: one compiler call.
pub fn main() -> std::process::ExitCode {
    launcher::main()
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-50
    #[test]
    fn the_launcher_is_known_by_its_own_name() {
        assert!(called_as_launcher(OsStr::new("hushrun-cc")));
        assert!(called_as_launcher(OsStr::new("/a b/bin/x/hushrun-cc")));
        assert!(!called_as_launcher(OsStr::new("hushrun")));
        assert!(!called_as_launcher(OsStr::new("hushrun-rustc")));
        assert!(!called_as_launcher(OsStr::new("hushrun-cc.sh")));
    }

    // covers: B-50
    #[test]
    fn the_indirection_names_nothing_of_hushrun_s() {
        assert!(!INDIRECTION.contains("hushrun-cc"));
        assert!(!INDIRECTION.contains('/') || INDIRECTION.starts_with("/bin/sh;"));
        let parts: Vec<&str> = INDIRECTION.split(';').collect();
        assert_eq!(parts[0], "/bin/sh");
        assert_eq!(parts[1], "-c");
        assert_eq!(parts.len(), 3);
    }

    // covers: B-50, B-62
    #[test]
    fn outside_a_run_the_indirection_is_the_compiler_and_inside_the_launcher() {
        let script = INDIRECTION.split(';').nth(2).unwrap();
        let run = |hushrun_cc: Option<&str>| {
            let mut sh = std::process::Command::new("/bin/sh");
            sh.args(["-c", script, "printf", "%s|", "a b", "c"]);
            sh.env_remove(HUSHRUN_CC);
            if let Some(value) = hushrun_cc {
                sh.env(HUSHRUN_CC, value);
            }
            let out = sh.output().unwrap();
            (String::from_utf8(out.stdout).unwrap(), out.status.code())
        };
        assert_eq!(run(None), ("a b|c|".to_string(), Some(0)));
        assert_eq!(run(Some("")), ("a b|c|".to_string(), Some(0)));
        // Set: the launcher comes first and gets the compiler and its arguments.
        assert_eq!(
            run(Some("echo")),
            ("printf %s| a b c\n".to_string(), Some(0))
        );
    }
}
