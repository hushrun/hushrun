//! Go build blocks (B-39 to B-47): package archives of pure Go reused between machines through
//! Go's own cache-program protocol, `GOCACHEPROG` (standard from Go 1.24).
//!
//! `hushrun run` sets `GOCACHEPROG` to a link named `hushrun-gocache` that points at the hushrun
//! binary ([`probe`]). The `go` command starts it once per command and asks it to `get` and `put`
//! every entry of its build cache, by Go's own key, the **action id** ([`program`]). The local
//! side stays Go's own cache: the program reads and writes the person's `GOCACHE` in Go's own
//! format ([`gocache`]), so plain `go` and `hushrun run go` share one cache. Beside it, only
//! package archives that may travel ([`archive::portable`]) go into hushrun's store, under a key
//! of Go's action id and hushrun's identity of the toolchain ([`key`], [`toolchain`]), and from
//! there through the relay to the shared store, kind `go/1`.
//!
//! # Rules every module here keeps
//!
//! 1. **The program never hangs `go` and never kills it** (B-40). Go waits for every answer
//!    without a limit and fails the whole command if the program exits before `close`. Every
//!    request gets exactly one answer, on every path: a panic and a watchdog both answer a
//!    miss, and a duplicate answer, which Go takes as fatal, can't be written.
//! 2. **Go checks only the size of what it is handed.** So nothing is served from hushrun's
//!    store that wasn't checked here: its SHA-256 against its record, its build ID against the
//!    action asked, its content id against its bytes.
//! 3. **Nothing reads the clock but the program's own entry points**, which pass a time in.

pub mod archive;
pub mod command;
pub mod gocache;
pub mod probe;
pub mod program;
pub mod toolchain;

use std::ffi::OsStr;
use std::path::Path;

use hushrun_core::Digest;

/// The name the binary is called by when Go starts it as its cache program (B-39). `main` looks
/// at its own name before anything else.
pub const PROGRAM_NAME: &str = "hushrun-gocache";

/// The variable `hushrun run` sets for COMMAND, which Go reads its cache program from.
pub const GOCACHEPROG: &str = "GOCACHEPROG";

/// The schema hushrun's key for a Go block is taken under.
const KEY_SCHEMA: &str = "hushrun.go-key/v1";

/// Whether the binary was called by the program's name (`argv[0]`, through any folder).
pub fn called_as_program(argv0: &OsStr) -> bool {
    Path::new(argv0)
        .file_name()
        .is_some_and(|name| name == PROGRAM_NAME)
}

/// hushrun's key for a Go entry (B-43): the SHA-256 of the JCS form of the schema, Go's action
/// id in hex, and the toolchain's identity in hex. Go's id holds the version, the flags, the
/// sources and the imports' contents; the identity holds what Go names by version alone.
pub fn key(action: &[u8; 32], toolchain: &Digest) -> Digest {
    let value = serde_json::json!({
        "action": Digest::from_bytes(*action).hex(),
        "schema": KEY_SCHEMA,
        "toolchain": toolchain.hex(),
    });
    let canonical =
        hushrun_core::jcs::canonicalize(&value).expect("hex strings always have a canonical form");
    Digest::of(canonical.as_bytes())
}

/// Go's `cache.Subkey`: the id of an entry Go keeps beside an action's, such as its compiler's
/// output (`"stdout"`): the SHA-256 of `subkey:`, the parent id and the name.
pub fn subkey(parent: &[u8; 32], name: &str) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(7 + 32 + name.len());
    bytes.extend_from_slice(b"subkey:");
    bytes.extend_from_slice(parent);
    bytes.extend_from_slice(name.as_bytes());
    *Digest::of(&bytes).as_bytes()
}

/// The program's entry point: serves the `go` command on stdin and stdout until `close`.
pub fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-39
    #[test]
    fn the_program_is_known_by_its_own_name() {
        assert!(called_as_program(OsStr::new("hushrun-gocache")));
        assert!(called_as_program(OsStr::new("/a b/bin/x/hushrun-gocache")));
        assert!(!called_as_program(OsStr::new("hushrun")));
        assert!(!called_as_program(OsStr::new("hushrun-rustc")));
    }

    // covers: B-43
    #[test]
    fn one_action_under_two_toolchains_is_two_keys() {
        let action = [1u8; 32];
        let a = key(&action, &Digest::of(b"official"));
        assert_eq!(a, key(&action, &Digest::of(b"official")));
        assert_ne!(a, key(&action, &Digest::of(b"patched")));
        assert_ne!(a, key(&[2u8; 32], &Digest::of(b"official")));
        // Not Go's id itself: a block's address never shows Go's key.
        assert_ne!(a.as_bytes(), &action);
    }
}
