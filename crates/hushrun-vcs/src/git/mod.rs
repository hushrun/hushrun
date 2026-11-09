//! git (D-9, D-10).
//!
//! Revision mode reads the revision's tree from the object database and never the worktree.
//! There are two object readers, and both pass the same golden vectors: `gix` (the default)
//! and the `git` CLI, used for SHA-256 repositories and whenever gix can't open the
//! repository. Worktree mode builds the tree `git add -A && git commit` would record, in a
//! temporary index, then reads it like a revision.

mod cli;
mod gix_reader;
pub mod remote;

use std::path::{Path, PathBuf};

use hushrun_digest::cache::OidCache;
use hushrun_digest::hash::sha256;
use hushrun_digest::{Content, Entry, Kind, Sha256, check_unique, normalize_path};

use crate::{
    EntrySink, Error, HashStats, Listing, Options, Revision, Snapshot, Vcs, effective_jobs,
};

/// Which object reader to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReaderChoice {
    /// gix for SHA-1 repositories it can open, the git CLI otherwise.
    #[default]
    Auto,
    Gix,
    Cli,
}

impl std::str::FromStr for ReaderChoice {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(ReaderChoice::Auto),
            "gix" => Ok(ReaderChoice::Gix),
            "cli" => Ok(ReaderChoice::Cli),
            other => Err(format!(
                "unknown git reader {other:?}: expected auto, gix or cli"
            )),
        }
    }
}

enum Reader {
    Gix(Box<gix::ThreadSafeRepository>),
    Cli,
}

/// A git repository.
pub struct Git {
    root: PathBuf,
    options: Options,
    reader: Reader,
    object_format: String,
}

impl std::fmt::Debug for Git {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Git")
            .field("root", &self.root)
            .field("reader", &self.reader_name())
            .field("object_format", &self.object_format)
            .finish()
    }
}

impl Git {
    /// Opens the repository whose worktree root is `root`.
    pub fn open(root: &Path, options: Options) -> Result<Git, Error> {
        let opened = match options.git_reader {
            ReaderChoice::Cli => None,
            ReaderChoice::Gix | ReaderChoice::Auto => match gix_reader::open(root) {
                Ok(repo) => Some(repo),
                Err(err) if options.git_reader == ReaderChoice::Gix => return Err(err),
                Err(err) => {
                    tracing::debug!(
                        "gix can't open {}: {err}; using the git CLI",
                        root.display()
                    );
                    None
                }
            },
        };
        let (reader, object_format) = match opened {
            Some(repo) => {
                let format = gix_reader::object_format(&repo);
                // gitoxide's SHA-256 support is incomplete: read those repositories with git.
                if format == "sha256" && options.git_reader == ReaderChoice::Auto {
                    (Reader::Cli, format)
                } else {
                    (Reader::Gix(Box::new(repo)), format)
                }
            }
            None => (Reader::Cli, cli::object_format(root)?),
        };
        Ok(Git {
            root: root.to_path_buf(),
            options,
            reader,
            object_format,
        })
    }

    /// `gix` or `cli`.
    pub fn reader_name(&self) -> &'static str {
        match self.reader {
            Reader::Gix(_) => "gix",
            Reader::Cli => "cli",
        }
    }

    /// `sha1` or `sha256`.
    pub fn object_format(&self) -> &str {
        &self.object_format
    }

    fn oid_len(&self) -> usize {
        if self.object_format == "sha256" {
            32
        } else {
            20
        }
    }

    /// What gix read, or else what git reads, whose errors change nothing: gix is the fast way in,
    /// git has the last word. A `git gc` that a commit left running moves objects into a pack, and
    /// for that moment gix can say an object that's there is missing (D-9).
    fn or_git<T>(
        &self,
        read: Result<T, Error>,
        git: impl FnOnce() -> Result<T, Error>,
    ) -> Result<T, Error> {
        match read {
            Err(err) if self.options.git_reader == ReaderChoice::Auto => {
                tracing::debug!("gix: {err}; asking git");
                git().map_err(|_| err)
            }
            read => read,
        }
    }

    /// The commit, ref and tree of `rev`.
    fn resolve(&self, rev: &str) -> Result<Revision, Error> {
        let with_git = || {
            let (commit, tree) = cli::resolve(&self.root, rev)?;
            let reference = if rev == "HEAD" {
                cli::run_text(&self.root, ["symbolic-ref", "-q", "HEAD"], &[]).ok()
            } else {
                cli::symbolic_full_name(&self.root, rev)
            };
            Ok((commit, tree, reference.filter(|r| !r.is_empty())))
        };
        let (commit, tree, reference) = match &self.reader {
            Reader::Gix(repo) => self.or_git(gix_reader::resolve(repo, rev), with_git)?,
            Reader::Cli => with_git()?,
        };
        Ok(Revision {
            vcs: "git".into(),
            revision: Some(commit),
            reference,
            tree: Some(tree),
            object_format: Some(self.object_format.clone()),
        })
    }

    fn entries_of_tree(&self, tree: &str) -> Result<Vec<Entry>, Error> {
        let records = match &self.reader {
            Reader::Gix(repo) => self.or_git(gix_reader::ls_tree(repo, tree), || {
                cli::ls_tree(&self.root, tree)
            })?,
            Reader::Cli => cli::ls_tree(&self.root, tree)?,
        };
        let mut entries = Vec::with_capacity(records.len());
        for record in records {
            if let Some(entry) =
                to_entry(record.mode, record.oid, &record.path, &self.object_format)?
            {
                entries.push(entry);
            }
        }
        check_unique(&mut entries)?;
        Ok(entries)
    }
}

/// Maps a tree entry to an entry (D-3). A blob's mode is canonicalized as git does when it
/// checks the file out: only the owner execute bit makes it executable.
fn to_entry(
    mode: u32,
    oid: Vec<u8>,
    raw_path: &[u8],
    object_format: &str,
) -> Result<Option<Entry>, Error> {
    let Some(path) = normalize_path(raw_path)? else {
        return Ok(None);
    };
    let (kind, content) = match mode & 0o170000 {
        0o100000 if mode & 0o100 != 0 => (Kind::Exec, Content::Object { id: oid }),
        0o100000 => (Kind::File, Content::Object { id: oid }),
        0o120000 => (Kind::Link, Content::Object { id: oid }),
        0o160000 => {
            let text = format!("{object_format}:{}", hex::encode(&oid));
            (Kind::Gitlink, Content::Bytes(text.into_bytes()))
        }
        _ => {
            return Err(Error::GitOutput {
                args: "ls-tree".into(),
                detail: format!("{path:?} has unsupported mode {mode:o}"),
            });
        }
    };
    Ok(Some(Entry {
        path,
        kind,
        content,
    }))
}

impl Vcs for Git {
    fn root(&self) -> &Path {
        &self.root
    }

    fn list(&self, at: &Snapshot) -> Result<Listing, Error> {
        let revision = match at {
            Snapshot::Rev(rev) => self.resolve(rev)?,
            Snapshot::Worktree => {
                let tree = cli::worktree_tree(&self.root)?;
                // The revision is still HEAD's (for traceability); the content is the tree.
                let mut revision = self.resolve("HEAD").unwrap_or_else(|_| Revision {
                    vcs: "git".into(),
                    object_format: Some(self.object_format.clone()),
                    ..Revision::default()
                });
                revision.tree = Some(tree);
                revision
            }
        };
        let tree = revision.tree.clone().expect("a git revision has a tree");
        let entries = self.entries_of_tree(&tree)?;
        Ok(Listing { revision, entries })
    }

    fn hash(&self, entries: &[&Entry], stats: &mut HashStats) -> Result<Vec<(Sha256, u64)>, Error> {
        let mut cache = match &self.options.cache_dir {
            Some(dir) => OidCache::open(dir, &self.object_format, self.oid_len()),
            None => OidCache::disabled(self.oid_len()),
        };
        let mut out: Vec<Option<(Sha256, u64)>> = vec![None; entries.len()];
        let mut wanted: Vec<&[u8]> = Vec::new();
        for (slot, entry) in out.iter_mut().zip(entries) {
            match &entry.content {
                Content::Bytes(bytes) => *slot = Some((sha256(bytes), bytes.len() as u64)),
                Content::Object { id } => match cache.get(id) {
                    Some(hit) => {
                        *slot = Some(hit);
                        stats.cache_hits += 1;
                    }
                    None => wanted.push(id),
                },
                Content::File { path } => {
                    unreachable!("git entries never point at files ({})", path.display())
                }
            }
        }
        // The same blob can appear at several paths: read each once.
        wanted.sort_unstable();
        wanted.dedup();
        let with_git = || cli::hash_blobs(&self.root, &wanted, effective_jobs(self.options.jobs));
        let hashes = match &self.reader {
            Reader::Gix(repo) => self.or_git(gix_reader::hash_blobs(repo, &wanted), with_git)?,
            Reader::Cli => with_git()?,
        };
        for (oid, (sha, size)) in wanted.iter().zip(&hashes) {
            cache.insert(oid, *sha, *size);
            stats.hashed += 1;
            stats.hashed_bytes += size;
        }
        for (slot, entry) in out.iter_mut().zip(entries) {
            if slot.is_none()
                && let Content::Object { id } = &entry.content
            {
                *slot = cache.get(id);
            }
        }
        if let Err(err) = cache.save() {
            tracing::warn!("could not write the object cache: {err}");
        }
        Ok(out
            .into_iter()
            .map(|h| h.expect("every entry is hashed"))
            .collect())
    }

    fn read_entry(&self, entry: &Entry) -> Result<Vec<u8>, Error> {
        match &entry.content {
            Content::Bytes(bytes) => Ok(bytes.clone()),
            Content::Object { id } => match &self.reader {
                Reader::Gix(repo) => self.or_git(gix_reader::read_blob(repo, id), || {
                    cli::read_blob(&self.root, id)
                }),
                Reader::Cli => cli::read_blob(&self.root, id),
            },
            Content::File { path } => std::fs::read(path).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            }),
        }
    }

    fn read_entries(&self, entries: &[&Entry], sink: &mut EntrySink<'_>) -> Result<(), Error> {
        if matches!(self.reader, Reader::Gix(_)) {
            for entry in entries {
                sink(entry, self.read_entry(entry)?)?;
            }
            return Ok(());
        }
        // The git CLI: one `cat-file --batch` for every object, never a process per file.
        let (objects, others): (Vec<&Entry>, Vec<&Entry>) = entries
            .iter()
            .partition(|e| matches!(e.content, Content::Object { .. }));
        for entry in others {
            sink(entry, self.read_entry(entry)?)?;
        }
        let ids: Vec<&[u8]> = objects
            .iter()
            .map(|e| match &e.content {
                Content::Object { id } => id.as_slice(),
                _ => unreachable!("partitioned"),
            })
            .collect();
        cli::read_blobs(&self.root, &ids, &mut |i, bytes| sink(objects[i], bytes))
    }

    fn read_file_at(&self, rev: &str, path: &str) -> Result<Vec<u8>, Error> {
        cli::read_file_at(&self.root, rev, path)
    }

    fn remote_head(&self, remote: &str, branch: &str) -> Result<Option<String>, Error> {
        cli::remote_head(&self.root, remote, branch)
    }

    fn tree(&self, rev: &str) -> Result<Option<String>, Error> {
        cli::tree(&self.root, rev).map(Some)
    }

    fn fetch_commit(&self, remote: &str, sha: &str) -> Result<(), Error> {
        cli::fetch_commit(&self.root, remote, sha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: D-3
    #[test]
    fn blob_modes_are_canonicalized_like_git_checkouts() {
        let kind = |mode: u32| {
            to_entry(mode, vec![1; 20], b"f", "sha1")
                .unwrap()
                .unwrap()
                .kind
        };
        assert_eq!(kind(0o100644), Kind::File);
        assert_eq!(kind(0o100664), Kind::File);
        assert_eq!(
            kind(0o100654),
            Kind::File,
            "only the owner execute bit counts"
        );
        assert_eq!(kind(0o100755), Kind::Exec);
        assert_eq!(kind(0o100775), Kind::Exec);
        assert_eq!(kind(0o100744), Kind::Exec);
        assert_eq!(kind(0o120000), Kind::Link);
        assert_eq!(kind(0o160000), Kind::Gitlink);
        let gitlink = to_entry(0o160000, vec![0xab; 20], b"sub", "sha1")
            .unwrap()
            .unwrap();
        assert_eq!(
            gitlink.content,
            Content::Bytes(format!("sha1:{}", "ab".repeat(20)).into_bytes())
        );
        assert!(
            to_entry(0o100644, vec![1; 20], b".git/config", "sha1")
                .unwrap()
                .is_none()
        );
    }
}
