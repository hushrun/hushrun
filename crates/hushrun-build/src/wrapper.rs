//! The wrapper itself (B-2, B-19): one compiler call from start to exit code.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, ExitCode};

/// Whether the binary was called by the wrapper's name (B-2). `main` asks this first, before it
/// reads any setting.
pub fn called_as_wrapper(argv0: &OsStr) -> bool {
    Path::new(argv0)
        .file_stem()
        .is_some_and(|stem| stem == crate::WRAPPER_NAME)
}

/// One compiler call. The process's own arguments are `[<this name>, <rustc>, <args>…]`, as
/// cargo passes them to a `RUSTC_WRAPPER`. Returns rustc's exit code, or 0 for a reuse.
pub fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(rustc) = args.next() else {
        eprintln!(
            "error: {} stands in for rustc and is run by cargo: it needs the compiler's path as its first argument",
            crate::WRAPPER_NAME
        );
        return ExitCode::from(2);
    };
    pass_through(&rustc, args.collect())
}

/// Runs rustc exactly as asked (B-19) and returns its exit code.
fn pass_through(rustc: &OsStr, args: Vec<OsString>) -> ExitCode {
    match Command::new(rustc).args(args).status() {
        Ok(status) => match status.code() {
            Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
            // Killed by a signal: cargo only needs to know it failed.
            None => ExitCode::FAILURE,
        },
        Err(err) => {
            eprintln!(
                "error: hushrun could not run {}: {err}",
                Path::new(rustc).display()
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-2
    #[test]
    fn the_wrapper_is_known_by_its_own_name() {
        assert!(called_as_wrapper(OsStr::new("hushrun-rustc")));
        assert!(called_as_wrapper(OsStr::new(
            "/store/bin/ab12/hushrun-rustc"
        )));
        assert!(called_as_wrapper(OsStr::new("hushrun-rustc.exe")));
        assert!(!called_as_wrapper(OsStr::new("hushrun")));
        assert!(!called_as_wrapper(OsStr::new("/usr/local/bin/hushrun")));
        assert!(!called_as_wrapper(OsStr::new("rustc")));
        assert!(!called_as_wrapper(OsStr::new("")));
    }
}
