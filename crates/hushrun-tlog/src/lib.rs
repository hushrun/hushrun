//! The transparency log of an HTTP ledger (RFC 0001 §4.7, H1; spec L-11).
//!
//! A ledger that keeps a log appends one leaf for every object it stores, every configuration
//! write and every deletion, and signs checkpoints of the resulting Merkle tree with its log
//! key, whose public half the repository's signed trust names (T-9). Readers check that what
//! they are shown is in the log; monitors replay the log and compare it with what the ledger
//! signed. Everything here is pure: bytes and hashes in, verdicts out, no storage and no I/O,
//! so the CLI, its monitor and the server share one implementation.
//!
//! - [`hash`] and [`tree`]: RFC 6962 Merkle trees, as RFC 9162 §2.1 restates them, with
//!   inclusion and consistency proofs ([`proof`]).
//! - [`note`] and [`checkpoint`]: C2SP `signed-note` with Ed25519, and C2SP `tlog-checkpoint`.
//! - [`entry`]: what a leaf records.
//! - [`listing`]: the signed statement a listing carries; [`signers`], the one a repository's
//!   signers answer carries (ADR 0031).
//! - [`replay`]: a monitor's replay of the leaves.
//! - \[M19\] [`map`]: the prefix map every checkpoint commits to, and the proofs that a listing
//!   is exactly the keys the map holds (L-12).
#![forbid(unsafe_code)]

pub mod b64;
pub mod checkpoint;
pub mod entry;
pub mod hash;
pub mod listing;
pub mod map;
pub mod note;
pub mod proof;
pub mod replay;
pub mod signers;
pub mod tree;

pub use checkpoint::{Checkpoint, SignedCheckpoint};
pub use entry::{Entry, EntryKind};
pub use hash::Hash;
pub use listing::{Listing, SignedListing, keys_hash};
pub use map::{ListingProof, Map, MapBuilder, MapEntry, MapProof, PageProof};
pub use note::{Signer, Verifier};
pub use proof::ProofError;
pub use replay::Replay;
pub use tree::{CompactRange, MemTree, NodeId};

/// A log's origin for a ledger URL: the URL without its scheme, as C2SP checkpoints name logs
/// (`hushrun.example/l/acme/back` for `https://hushrun.example/l/acme/back`). It is also the
/// name of the log key in signed notes.
pub fn origin_of(ledger_url: &str) -> String {
    let rest = ledger_url
        .split_once("://")
        .map_or(ledger_url, |(_, rest)| rest);
    rest.trim_end_matches('/').to_string()
}

/// Whether `name` may name a key or object in a log entry (L-10's key rules): 1 to 1024 bytes
/// of `/`-separated segments of `[A-Za-z0-9._-]`, none `.` or `..`. So names never hold a
/// newline or a space, which keeps every text format here unambiguous.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && name.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// Whether `prefix` may be a listing prefix: key characters and `/` only, possibly empty, and
/// possibly ending mid-segment (ADR 0007 item 5).
pub fn valid_prefix(prefix: &str) -> bool {
    prefix.len() <= 1024
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_drop_the_scheme() {
        assert_eq!(
            origin_of("https://hushrun.example/l/acme/back"),
            "hushrun.example/l/acme/back"
        );
        assert_eq!(
            origin_of("http://localhost:8789/l/e2e/app"),
            "localhost:8789/l/e2e/app"
        );
        assert_eq!(origin_of("https://h/l/a/b/"), "h/l/a/b");
    }

    #[test]
    fn names_follow_the_key_rules() {
        for good in ["trust.yaml", "root/1.json", "v1/events/a/b", "A-z_0.9"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "", "/a", "a/", "a//b", "a/./b", "a/../b", "a b", "a\nb", "é", "a+b",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name(&"a".repeat(1024)));
        assert!(!valid_name(&"a".repeat(1025)));
        assert!(valid_prefix(""));
        assert!(valid_prefix("v1/ev"));
        assert!(valid_prefix("v1/events/"));
        assert!(!valid_prefix("v1 x"));
        assert!(!valid_prefix("v1\n"));
    }
}
