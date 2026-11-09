//! VCS adapters (§11.1): list the entries of a revision or snapshot, hash their content, and
//! answer the few questions hushrun asks a VCS.
//!
//! - [`git::Git`]: revision mode (D-9) with two object readers, `gix` and the `git` CLI, and
//!   worktree mode (D-10).
//! - [`dir::Dir`]: a plain directory, such as a clean export (D-11).
#![forbid(unsafe_code)]

pub mod dir;
mod error;
pub mod git;

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Serialize;

use hushrun_core::scope::{Scope, Scopes};
use hushrun_core::{Digest, ScopeName};
use hushrun_digest::{Entry, HashedEntry, Sha256, digest_of, manifest};

pub use error::Error;

/// What to read: a revision, or the working tree as it would be committed now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Snapshot {
    /// A revision, such as `HEAD` or a commit id (D-9).
    Rev(String),
    /// The worktree as `git add -A && git commit` would record it now (D-10).
    Worktree,
}

impl Listing {
    /// The entry at `path`, if any.
    pub fn find(&self, path: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.entries[i])
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot::Rev("HEAD".to_string())
    }
}

/// The VCS revision of a snapshot. Recorded for traceability; never used for matching.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    /// `git` or `dir`.
    pub vcs: String,
    /// The commit id; absent for a directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The full ref name, such as `refs/heads/develop`, when known.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The tree id, for git; in worktree mode, the tree `git write-tree` produced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// `sha1` or `sha256`, for git.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_format: Option<String>,
}

/// The entries of a snapshot: normalized paths (D-2), VCS metadata skipped, sorted by path and
/// unique (D-4).
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub revision: Revision,
    pub entries: Vec<Entry>,
}

/// Counters for one hashing pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashStats {
    /// Entries whose hash came from a cache.
    pub cache_hits: u64,
    /// Entries whose content was read and hashed.
    pub hashed: u64,
    /// Bytes read and hashed.
    pub hashed_bytes: u64,
}

/// A VCS adapter.
pub trait Vcs: Send + Sync {
    /// The root of the checkout or directory.
    fn root(&self) -> &Path;

    /// The entries of `at`.
    fn list(&self, at: &Snapshot) -> Result<Listing, Error>;

    /// The SHA-256 and size of each entry's content, in order. Uses the caches (plan §12.6).
    fn hash(&self, entries: &[&Entry], stats: &mut HashStats) -> Result<Vec<(Sha256, u64)>, Error>;

    /// The bytes of one entry's content, read through the same readers as hashing.
    fn read_entry(&self, entry: &Entry) -> Result<Vec<u8>, Error>;

    /// \[M8\] Calls `sink` once with each entry's content, in no particular order, read through
    /// the same readers as hashing and never through an export tool that applies attributes,
    /// such as `git archive` (§11.1): an isolated run's export (§11.6).
    fn read_entries(&self, entries: &[&Entry], sink: &mut EntrySink<'_>) -> Result<(), Error> {
        for entry in entries {
            sink(entry, self.read_entry(entry)?)?;
        }
        Ok(())
    }

    /// The bytes of `path` at revision `rev` (`--config-ref`, M8).
    fn read_file_at(&self, rev: &str, path: &str) -> Result<Vec<u8>, Error>;

    /// The head of `branch` on `remote`, or `None` if the branch doesn't exist (`guard`).
    fn remote_head(&self, remote: &str, branch: &str) -> Result<Option<String>, Error>;

    /// The tree id of commit `rev`, or `None` where there are no trees (dir mode). Equal trees
    /// have equal digests for every scope (pending-run matching, §10.4).
    fn tree(&self, rev: &str) -> Result<Option<String>, Error>;

    /// Fetches commit `sha` from `remote`, one commit deep, so it can be listed (§10.4).
    fn fetch_commit(&self, remote: &str, sha: &str) -> Result<(), Error>;
}

/// \[M8\] What receives each entry's content in [`Vcs::read_entries`].
pub type EntrySink<'a> = dyn FnMut(&Entry, Vec<u8>) -> Result<(), Error> + 'a;

/// Options shared by adapters.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Where caches live; `None` disables them.
    pub cache_dir: Option<PathBuf>,
    /// Hashing threads; 0 means the number of cores.
    pub jobs: usize,
    /// Which git object reader to use.
    pub git_reader: git::ReaderChoice,
}

/// The VCS found at or above a directory (§11.1, detection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detected {
    Git { root: PathBuf },
    None,
}

/// Looks for VCS metadata at `start` and its parents. git is supported; another VCS without a
/// `vcs:` configuration is an error, because falling back to dir mode would hash build outputs
/// and other untracked files.
pub fn detect(start: &Path) -> Result<Detected, Error> {
    for dir in start.ancestors() {
        if dir.join(".git").exists() {
            return Ok(Detected::Git {
                root: dir.to_path_buf(),
            });
        }
        for (marker, vcs) in [
            (".hg", "Mercurial"),
            (".svn", "Subversion"),
            (".bzr", "Bazaar"),
            ("_darcs", "Darcs"),
        ] {
            if dir.join(marker).exists() {
                return Err(Error::UnsupportedVcs {
                    vcs,
                    root: dir.to_path_buf(),
                });
            }
        }
    }
    Ok(Detected::None)
}

/// The digest of one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDigest {
    pub scope: ScopeName,
    pub digest: Digest,
    pub manifest: Vec<u8>,
    pub files: u64,
    pub bytes: u64,
}

/// One entry of `digest --explain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainEntry {
    pub path: String,
    pub kind: char,
    /// The content hash, for entries in at least one requested scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub scopes: Vec<ExplainScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainScope {
    pub scope: ScopeName,
    pub member: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub included_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_by: Option<String>,
}

/// The result of computing digests.
#[derive(Debug, Clone)]
pub struct DigestRun {
    pub scopes: Vec<ScopeDigest>,
    pub stats: HashStats,
    pub explain: Option<Vec<ExplainEntry>>,
}

/// Computes the digest of each scope in `selected` from a listing (D-1 … D-7): filters entries
/// by scope before hashing, hashes what is needed, and builds each manifest.
pub fn digest_scopes(
    vcs: &dyn Vcs,
    listing: &Listing,
    scopes: &Scopes,
    selected: &[&Scope],
    explain: bool,
) -> Result<DigestRun, Error> {
    let entries = &listing.entries;

    // Scope membership of every entry (D-7), computed before any content is read.
    let membership: Vec<Vec<bool>> = entries
        .par_iter()
        .map(|entry| {
            selected
                .iter()
                .map(|scope| scopes.contains(scope, &entry.path))
                .collect()
        })
        .collect();
    let needed: Vec<usize> = (0..entries.len())
        .filter(|&i| membership[i].iter().any(|m| *m))
        .collect();

    let mut stats = HashStats::default();
    let to_hash: Vec<&Entry> = needed.iter().map(|&i| &entries[i]).collect();
    let hashes = vcs.hash(&to_hash, &mut stats)?;
    let mut hashed: Vec<Option<(Sha256, u64)>> = vec![None; entries.len()];
    for (&i, hash) in needed.iter().zip(hashes) {
        hashed[i] = Some(hash);
    }

    let mut results = Vec::with_capacity(selected.len());
    for (s, scope) in selected.iter().enumerate() {
        let members: Vec<HashedEntry> = entries
            .iter()
            .enumerate()
            .filter(|(i, _)| membership[*i][s])
            .map(|(i, entry)| {
                let (hash, size) = hashed[i].expect("members are hashed");
                HashedEntry {
                    path: entry.path.clone(),
                    kind: entry.kind,
                    hash,
                    size,
                }
            })
            .collect();
        if members.is_empty() {
            return Err(hushrun_digest::Error::EmptyScope {
                scope: scope.name.to_string(),
            }
            .into());
        }
        let bytes = manifest(&members);
        results.push(ScopeDigest {
            scope: scope.name.clone(),
            digest: digest_of(&bytes),
            manifest: bytes,
            files: members.len() as u64,
            bytes: members.iter().map(|m| m.size).sum(),
        });
    }

    let explain = explain.then(|| {
        entries
            .iter()
            .enumerate()
            .map(|(i, entry)| ExplainEntry {
                path: entry.path.clone(),
                kind: entry.kind.code(),
                hash: hashed[i].map(|(h, _)| hushrun_core::digest::to_hex(&h)),
                scopes: selected
                    .iter()
                    .map(|scope| {
                        let m = scopes.membership(scope, &entry.path);
                        ExplainScope {
                            scope: scope.name.clone(),
                            member: m.is_member(),
                            included_by: m.included_by.map(|p| p.as_str().to_string()),
                            excluded_by: m
                                .excluded_by
                                .or(m.globally_excluded_by)
                                .map(|p| p.as_str().to_string()),
                        }
                    })
                    .collect(),
            })
            .collect()
    });

    Ok(DigestRun {
        scopes: results,
        stats,
        explain,
    })
}

/// The number of hashing threads for `jobs` (0 = available cores).
pub fn effective_jobs(jobs: usize) -> usize {
    if jobs > 0 {
        jobs
    } else {
        std::thread::available_parallelism().map_or(4, |n| n.get())
    }
}
