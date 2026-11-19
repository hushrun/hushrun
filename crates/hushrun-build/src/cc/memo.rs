//! The digests of what a compile read (B-54), each file hashed once per launcher process.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;

use super::text::{self, Seen};
use crate::regular;

/// A read file's digest, its stat before it was read, and what it holds.
#[derive(Debug, Clone)]
pub struct Hashed {
    /// Its SHA-256.
    pub digest: Digest,
    /// Its stat, taken before the read and checked again after.
    pub stat: Stat,
    /// Its probes and time macros, when it was read whole for them.
    pub seen: Option<Seen>,
}

/// The digests one launcher process has taken.
#[derive(Debug, Default)]
pub struct Digests {
    known: HashMap<PathBuf, Hashed>,
}

impl Digests {
    /// No digests yet.
    pub fn new(_session: Option<&Path>) -> Digests {
        Digests::default()
    }

    /// The digest of `path`, for a lookup. `base`: the file lies under the SDK, the toolchain or
    /// a system folder.
    pub fn digest(&mut self, path: &Path, base: bool) -> io::Result<Digest> {
        let _ = base;
        if let Some(known) = self.known.get(path) {
            return Ok(known.digest);
        }
        let (digest, stat) = regular::hash_steady(path)?;
        self.known.insert(
            path.to_path_buf(),
            Hashed {
                digest,
                stat,
                seen: None,
            },
        );
        Ok(digest)
    }

    /// The digest of `path` and what it holds, for a compile that stores (B-54).
    pub fn read(&mut self, path: &Path, base: bool) -> io::Result<Hashed> {
        let _ = base;
        if let Some(known) = self.known.get(path)
            && known.seen.is_some()
        {
            return Ok(known.clone());
        }
        let hashed = read_steady(path)?;
        self.known.insert(path.to_path_buf(), hashed.clone());
        Ok(hashed)
    }
}

/// Reads a regular file whole, stat'ed through the open handle before and through the path
/// after: an error when it changed while it was read (B-11).
pub fn read_steady(path: &Path) -> io::Result<Hashed> {
    let mut file = regular::open(path)?;
    let before = Stat::of(&file.metadata()?);
    let mut bytes = Vec::with_capacity(usize::try_from(before.size).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    let after = Stat::of(&fs::metadata(path)?);
    if !regular::same(&before, &after) {
        return Err(io::Error::other(format!(
            "{} changed while it was read",
            path.display()
        )));
    }
    Ok(Hashed {
        digest: Digest::of(&bytes),
        stat: before,
        seen: Some(text::scan(&bytes)),
    })
}
