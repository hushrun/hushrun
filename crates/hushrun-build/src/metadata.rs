//! What a dependent's compiler reads from a proc macro (B-6): the crate metadata rustc writes
//! into the linked library, so that a proc macro that came out different for any reason, known
//! or not, gives its dependents another key and never a library that fails to load against it.
//!
//! rustc puts a proc macro's metadata into the library it links, in a section of its own
//! (`.rustc` on ELF, `__DATA,.rustc` on Mach-O): its header (`rust`, three zero bytes and the
//! format's version), the length as a little-endian `u64`, then the metadata, which starts with
//! the same header. That holds the crate hash a dependent's rustc compares with the one it was
//! compiled against (E0460, E0463), and everything else it reads. The linker places the section
//! and doesn't write it, so two linkers give the same bytes there. The section is found by its
//! contents, not by the file's format: exactly one place in the file must start with the header,
//! a length that fits the file, and the header again.

use std::path::Path;

use hushrun_core::Digest;
use serde_json::json;

use crate::key::{BLOCK_SCHEMA, KeyError};

/// What rustc's metadata header starts with; the format's version follows.
const MAGIC: &[u8] = b"rust\0\0\0";

/// The SHA-256 of the crate metadata in a linked library's bytes, header and length included.
/// `None` when no place, or more than one, holds it.
pub fn digest(bytes: &[u8]) -> Option<Digest> {
    let mut found = None;
    for at in memchr::memmem::find_iter(bytes, MAGIC) {
        let Some(header) = bytes.get(at..at + 8) else {
            continue;
        };
        let Some(length) = bytes.get(at + 8..at + 16) else {
            continue;
        };
        let length = u64::from_le_bytes(length.try_into().ok()?);
        let end = usize::try_from(length)
            .ok()
            .and_then(|length| (at + 16).checked_add(length));
        let Some(end) = end.filter(|end| *end <= bytes.len()) else {
            continue;
        };
        if bytes.get(at + 16..at + 24) != Some(header) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(Digest::of(&bytes[at..end]));
    }
    found
}

/// The SHA-256 of the crate metadata in the linked library at `path` ([`digest`]). `None` when
/// the file can't be read or holds no single metadata blob.
pub fn of(path: &Path) -> Option<Digest> {
    digest(&crate::regular::read(path).ok()?)
}

/// What a dependent names a linked library by when its block may move (B-6): what it was built
/// from (`known_by`, [`crate::key::Observed::known_by`]) and the metadata a dependent's
/// compiler reads from it.
pub fn name(known_by: &Digest, metadata: &Digest) -> Result<Digest, KeyError> {
    hushrun_core::jcs::digest(&json!([
        BLOCK_SCHEMA,
        { "known_by": known_by, "metadata": metadata }
    ]))
    .map_err(|e| KeyError::Canonical(e.to_string()))
}

/// What the record of `file`, an output of a block that may move and was built from
/// `known_by`, says dependents name it by: for a linked library (a proc macro), [`name`] of its
/// metadata now, or nothing when that can't be read, so its bytes name it; for any other file,
/// `known_by`. The one place that decides it, for a compile and for a reuse.
pub fn produced_by(known_by: Digest, file: &Path) -> Option<Digest> {
    if !crate::inputs::is_linked_library(file) {
        return Some(known_by);
    }
    name(&known_by, &of(file)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A linked library as rustc and a linker leave it: code, the metadata section, more code.
    fn library(code: &[u8], metadata: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut out = code.to_vec();
        out.extend_from_slice(&section(metadata));
        out.extend_from_slice(tail);
        out
    }

    fn section(metadata: &[u8]) -> Vec<u8> {
        let mut inner = b"rust\0\0\0\x0a".to_vec();
        inner.extend_from_slice(metadata);
        let mut out = b"rust\0\0\0\x0a".to_vec();
        out.extend_from_slice(&(inner.len() as u64).to_le_bytes());
        out.extend_from_slice(&inner);
        out
    }

    // covers: B-6
    #[test]
    fn the_metadata_names_a_library_whatever_the_linker_wrote_around_it() {
        let one = library(b"code from one linker", b"crate hash 1, items", b"symbols");
        let two = library(
            b"other code, longer, another linker",
            b"crate hash 1, items",
            b"",
        );
        assert_ne!(one, two);
        assert_eq!(digest(&one), digest(&two));
        assert!(digest(&one).is_some());
        let other = library(b"code from one linker", b"crate hash 2, items", b"symbols");
        assert_ne!(digest(&one), digest(&other));
        // The header alone, a length past the end, or no header again after it: no metadata.
        assert_eq!(digest(b"rust\0\0\0 and nothing that follows"), None);
        let mut cut = section(b"crate hash 1");
        cut.truncate(cut.len() - 1);
        assert_eq!(digest(&cut), None);
        let mut lone = b"rust\0\0\0\x0a".to_vec();
        lone.extend_from_slice(&4u64.to_le_bytes());
        lone.extend_from_slice(b"abcd");
        assert_eq!(digest(&lone), None);
        // Two blobs: which one rustc reads can't be told, so none is trusted.
        let twice = library(&section(b"a"), b"b", b"");
        assert_eq!(digest(&twice), None);
    }

    // covers: B-6
    #[test]
    fn a_proc_macro_is_named_by_what_it_was_built_from_and_by_its_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let known = Digest::of(b"known by");
        let so = dir
            .path()
            .join(format!("libm-1.{}", std::env::consts::DLL_EXTENSION));
        std::fs::write(&so, library(b"x", b"meta", b"y")).unwrap();
        let named = produced_by(known, &so).unwrap();
        assert_ne!(named, known);
        // Another linker: the same name.
        std::fs::write(&so, library(b"other linker", b"meta", b"")).unwrap();
        assert_eq!(produced_by(known, &so), Some(named));
        // Other metadata, or other inputs: another name.
        std::fs::write(&so, library(b"x", b"other meta", b"y")).unwrap();
        assert_ne!(produced_by(known, &so), Some(named));
        std::fs::write(&so, library(b"x", b"meta", b"y")).unwrap();
        assert_ne!(produced_by(Digest::of(b"other"), &so), Some(named));
        // No metadata found: no name, so dependents key the file by its bytes.
        std::fs::write(&so, b"not a library").unwrap();
        assert_eq!(produced_by(known, &so), None);
        // An rlib or rmeta is named by what it was built from (its dependents key its bytes).
        let rlib = dir.path().join("libm-1.rlib");
        std::fs::write(&rlib, b"rlib").unwrap();
        assert_eq!(produced_by(known, &rlib), Some(known));
    }
}
