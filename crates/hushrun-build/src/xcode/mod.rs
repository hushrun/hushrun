//! Xcode build blocks (B-77 to B-84): what Xcode's own compilation cache keeps, shared between
//! checkouts, branches, the Xcode app and other machines through hushrun's store and relay.
//!
//! From Xcode 26, Swift and Clang (C, C++, Objective-C, their modules) can key each compile by
//! its content and ask a **compilation-cache service** for its result before compiling: a gRPC
//! service on a Unix socket that `COMPILATION_CACHE_REMOTE_SERVICE_PATH` names, with two parts, a
//! key-value store from Xcode's key of a compile to a value, and a store of objects named by
//! their content ([`proto`]). hushrun serves that socket ([`service`]):
//!
//! - **The local side** is the service's own cache in the store's folder ([`cas`]): every object
//!   and value Xcode puts, so a fresh DerivedData (a new worktree, a new branch's checkout, a CI
//!   job's second build) finds what another compiled on this machine.
//! - **A block** is one value and every object it reaches ([`block`]), kind `xcode/1`, under a
//!   key of Xcode's key ([`key`]): what goes up through the relay, comes down from the shared
//!   store or a baked one, and is checked object by object before anything of it is served.
//!
//! `hushrun run` turns it on for COMMAND with an xcconfig file Xcode reads from the environment
//! ([`xcconfig`], `XCODE_XCCONFIG_FILE`); `hushrun build xcode on` does the same for the Xcode app
//! on this Mac, with a service that outlives any run.
//!
//! # Rules every module here keeps
//!
//! 1. **The service never fails a build.** Xcode compiles on any error or miss; every request
//!    gets an answer, and one that fails in any way is a miss or an error, never a hang.
//! 2. **Nothing is served that wasn't checked here**: every object against its id, every value a
//!    block brings against its record, and a value only once every object it reaches is here.
//! 3. **Nothing reads the clock but the service's entry points**, which pass a time in.

pub mod block;
pub mod cas;
pub mod probe;
pub mod proto;
pub mod service;
pub mod xcconfig;

use hushrun_core::Digest;

/// The kind of Xcode blocks on the wire: the adapter and its keys' version.
pub const KIND: &str = "xcode/1";

/// The schema hushrun's key for an Xcode block is taken under.
const KEY_SCHEMA: &str = "hushrun.xcode-key/v1";

/// The variable Xcode reads an xcconfig file from, applied to every target of the build, its
/// Swift packages' included.
pub const XCCONFIG_VAR: &str = "XCODE_XCCONFIG_FILE";

/// The variable that turns the service off for a run: `0`, `off` or `false`.
pub const OFF_VAR: &str = "HUSHRUN_BUILD_XCODE";

/// hushrun's key for the value of Xcode's key `key` (B-81): the SHA-256 of the JCS form of the
/// schema and the key in hex. Xcode's key holds the compiler's version, its arguments and the
/// content of every input; the block's address never shows it.
pub fn key(xcode_key: &[u8]) -> Digest {
    let value = serde_json::json!({
        "key": hushrun_core::digest::to_hex(xcode_key),
        "schema": KEY_SCHEMA,
    });
    let canonical =
        hushrun_core::jcs::canonicalize(&value).expect("hex strings always have a canonical form");
    Digest::of(canonical.as_bytes())
}

/// Whether the variable `OFF_VAR` turns the service off.
pub fn turned_off(value: Option<&std::ffi::OsStr>) -> bool {
    value
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-81
    #[test]
    fn a_key_is_a_digest_of_xcodes_and_never_xcodes_itself() {
        let a = key(b"xcode key");
        assert_eq!(a, key(b"xcode key"));
        assert_ne!(a, key(b"another key"));
        assert_ne!(a, Digest::of(b"xcode key"));
    }

    // covers: B-77
    #[test]
    fn the_variable_turns_the_service_off() {
        for off in ["0", "off", "OFF", "false", "no"] {
            assert!(turned_off(Some(std::ffi::OsStr::new(off))));
        }
        for on in ["1", "on", ""] {
            assert!(!turned_off(Some(std::ffi::OsStr::new(on))));
        }
        assert!(!turned_off(None));
    }
}
