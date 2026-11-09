//! Source digest v1 (D-1 … D-8): path normalization, the canonical manifest, hashing and
//! caches.
//!
//! A VCS adapter (`hushrun-vcs`) yields the entries of a revision or snapshot. Entries are
//! filtered by scope before hashing (D-7); the hashed entries of a scope, sorted by path, form
//! its manifest (D-4), and the digest is the SHA-256 of the manifest (D-5).
#![forbid(unsafe_code)]

pub mod cache;
mod error;
pub mod hash;
mod manifest;
mod path;

use std::path::PathBuf;

use serde::Serialize;

pub use error::Error;
pub use manifest::{CANONICALIZATION, HEADER, digest_of, manifest};
pub use path::{VCS_METADATA, check_unique, normalize_path};

/// A SHA-256 hash of some content.
pub type Sha256 = [u8; 32];

/// The kind of an entry (D-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A regular file without execute bits.
    File,
    /// A regular file with any execute bit.
    Exec,
    /// A symbolic link; its content is the link target.
    Link,
    /// A git submodule; its content is `<object format>:<commit id hex>`.
    Gitlink,
}

impl Kind {
    /// The one-letter code written in the manifest.
    pub fn code(self) -> char {
        match self {
            Kind::File => 'f',
            Kind::Exec => 'x',
            Kind::Link => 'l',
            Kind::Gitlink => 'g',
        }
    }
}

/// Where an entry's content comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// An object in a VCS object database, read by the adapter. `id` is the raw object id.
    Object { id: Vec<u8> },
    /// A regular file on disk.
    File { path: PathBuf },
    /// Content known up front: a symbolic link's target, or a gitlink's text.
    Bytes(Vec<u8>),
}

/// An entry of a revision or snapshot (D-1), with a normalized path (D-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    pub content: Content,
}

/// An entry with the SHA-256 and size of its content: one line of a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashedEntry {
    pub path: String,
    pub kind: Kind,
    pub hash: Sha256,
    pub size: u64,
}
