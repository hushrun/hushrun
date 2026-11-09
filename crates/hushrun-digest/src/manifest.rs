//! The canonical manifest (D-4) and the digest (D-5).

use hushrun_core::Digest;

use crate::HashedEntry;
use crate::hash::sha256;

/// The canonicalization this crate implements. Frozen once M1 ships (D-8).
pub const CANONICALIZATION: &str = hushrun_core::CANONICALIZATION;

/// The first bytes of every manifest.
pub const HEADER: &[u8] = b"hushrun-source-v1\n";

/// The manifest of `entries`, which must be sorted by path with no duplicates (D-4):
///
/// ```text
/// manifest = "hushrun-source-v1\n" || entry_1 || … || entry_n
/// entry    = kind || " " || content_hash || " " || path || "\0"
/// ```
pub fn manifest<'a>(entries: impl IntoIterator<Item = &'a HashedEntry>) -> Vec<u8> {
    let entries = entries.into_iter();
    let mut out = Vec::with_capacity(HEADER.len() + entries.size_hint().0 * 100);
    out.extend_from_slice(HEADER);
    let mut previous: Option<&str> = None;
    for entry in entries {
        debug_assert!(
            previous.is_none_or(|p| p < entry.path.as_str()),
            "manifest entries must be sorted and unique"
        );
        previous = Some(&entry.path);
        out.push(entry.kind.code() as u8);
        out.push(b' ');
        out.extend_from_slice(hushrun_core::digest::to_hex(&entry.hash).as_bytes());
        out.push(b' ');
        out.extend_from_slice(entry.path.as_bytes());
        out.push(0);
    }
    out
}

/// The digest of a manifest: `sha256:` of its bytes (D-5).
pub fn digest_of(manifest: &[u8]) -> Digest {
    Digest::from_bytes(sha256(manifest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;

    fn entry(path: &str, kind: Kind, content: &[u8]) -> HashedEntry {
        HashedEntry {
            path: path.to_string(),
            kind,
            hash: sha256(content),
            size: content.len() as u64,
        }
    }

    // covers: D-3, D-4, D-5
    #[test]
    fn manifest_bytes_follow_the_spec() {
        let entries = [
            entry("README.md", Kind::File, b"hello\n"),
            entry("run.sh", Kind::Exec, b"#!/bin/sh\n"),
            entry(
                "sub",
                Kind::Gitlink,
                b"sha1:3f9c000000000000000000000000000000000000",
            ),
            entry("z", Kind::Link, b"README.md"),
        ];
        let bytes = manifest(&entries);
        let expected = format!(
            "hushrun-source-v1\nf {} README.md\0x {} run.sh\0g {} sub\0l {} z\0",
            hushrun_core::digest::to_hex(&sha256(b"hello\n")),
            hushrun_core::digest::to_hex(&sha256(b"#!/bin/sh\n")),
            hushrun_core::digest::to_hex(&sha256(b"sha1:3f9c000000000000000000000000000000000000")),
            hushrun_core::digest::to_hex(&sha256(b"README.md")),
        );
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(
            digest_of(&bytes),
            Digest::from_bytes(sha256(expected.as_bytes()))
        );
        // A well-known value: sha256("hello\n").
        assert_eq!(
            hushrun_core::digest::to_hex(&sha256(b"hello\n")),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    // covers: D-4, D-6
    #[test]
    fn empty_manifest_is_only_the_header() {
        assert_eq!(manifest(&[]), HEADER);
    }

    // covers: D-8
    #[test]
    fn the_canonicalization_is_named_in_every_manifest() {
        // Frozen once M1 ships: a change is a new canonicalization, never an edit of this one.
        assert_eq!(CANONICALIZATION, "hushrun-source-v1");
        assert_eq!(HEADER, format!("{CANONICALIZATION}\n").as_bytes());
        let one = [entry("a", Kind::File, b"a\n")];
        assert!(manifest(&one).starts_with(HEADER));
        // The header is hashed: the same entries under another canonicalization would give
        // another digest.
        let mut other = manifest(&one);
        other.splice(..HEADER.len(), b"hushrun-source-v2\n".iter().copied());
        assert_ne!(digest_of(&other), digest_of(&manifest(&one)));
    }
}
