//! A plain directory, such as a clean export (D-11).
//!
//! Every regular file and symbolic link under the root is an entry, except VCS metadata. No
//! ignore files apply and symbolic links are never followed. On Windows there are no execute
//! bits, and `\` in link targets becomes `/`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;

use hushrun_digest::cache::{Stat, StatCache};
use hushrun_digest::hash::{hash_file, sha256};
use hushrun_digest::{Content, Entry, Kind, Sha256, VCS_METADATA, check_unique, normalize_path};

use crate::{Error, HashStats, Listing, Options, Revision, Snapshot, Vcs};

/// A directory hashed as is.
#[derive(Debug)]
pub struct Dir {
    root: PathBuf,
    options: Options,
}

impl Dir {
    pub fn open(root: &Path, options: Options) -> Result<Dir, Error> {
        let meta = std::fs::metadata(root).map_err(|source| Error::Io {
            path: root.to_path_buf(),
            source,
        })?;
        if !meta.is_dir() {
            return Err(Error::Io {
                path: root.to_path_buf(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a directory"),
            });
        }
        Ok(Dir {
            root: root.to_path_buf(),
            options,
        })
    }

    fn walk(&self) -> Result<Vec<Entry>, Error> {
        let found: Mutex<Vec<Result<Entry, Error>>> = Mutex::new(Vec::new());
        let walker = ignore::WalkBuilder::new(&self.root)
            .standard_filters(false)
            .follow_links(false)
            .filter_entry(|e| {
                e.depth() == 0
                    || !e
                        .file_name()
                        .to_str()
                        .is_some_and(|name| VCS_METADATA.contains(&name))
            })
            .threads(crate::effective_jobs(self.options.jobs).min(8))
            .build_parallel();
        walker.run(|| {
            let found = &found;
            let root = &self.root;
            Box::new(move |result| {
                let item = match result {
                    Ok(dirent) => to_entry(root, &dirent),
                    Err(err) => Some(Err(Error::Io {
                        path: root.clone(),
                        source: err
                            .into_io_error()
                            .unwrap_or_else(|| std::io::Error::other("directory walk failed")),
                    })),
                };
                if let Some(item) = item {
                    found.lock().expect("walk results").push(item);
                }
                ignore::WalkState::Continue
            })
        });
        let mut entries = Vec::new();
        for item in found.into_inner().expect("walk results") {
            entries.push(item?);
        }
        check_unique(&mut entries)?;
        Ok(entries)
    }
}

fn to_entry(root: &Path, dirent: &ignore::DirEntry) -> Option<Result<Entry, Error>> {
    if dirent.depth() == 0 {
        return None;
    }
    let file_type = dirent.file_type()?;
    if file_type.is_dir() {
        return None;
    }
    let path = dirent.path();
    let relative = path.strip_prefix(root).ok()?;
    let normalized = match relative_path(relative) {
        Ok(Some(p)) => p,
        Ok(None) => return None,
        Err(err) => return Some(Err(err)),
    };
    let io = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    if file_type.is_symlink() {
        let target = match std::fs::read_link(path) {
            Ok(target) => target,
            Err(err) => return Some(Err(io(err))),
        };
        return Some(Ok(Entry {
            path: normalized,
            kind: Kind::Link,
            content: Content::Bytes(link_target_bytes(&target)),
        }));
    }
    if file_type.is_file() {
        let kind = match dirent.metadata() {
            Ok(meta) if is_executable(&meta) => Kind::Exec,
            Ok(_) => Kind::File,
            Err(err) => {
                return Some(Err(io(err
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("metadata")))));
            }
        };
        return Some(Ok(Entry {
            path: normalized,
            kind,
            content: Content::File {
                path: path.to_path_buf(),
            },
        }));
    }
    Some(Err(hushrun_digest::Error::UnsupportedFile {
        path: normalized,
        what: special_file_kind(&file_type),
    }
    .into()))
}

/// The entry path of a file below the root: `/`-separated (on Windows the OS separator is
/// converted), UTF-8, NFC-normalized (D-2).
fn relative_path(relative: &Path) -> Result<Option<String>, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(normalize_path(relative.as_os_str().as_bytes())?)
    }
    #[cfg(not(unix))]
    {
        let Some(text) = relative.to_str() else {
            return Err(hushrun_digest::Error::PathEncoding {
                hex: hushrun_core::digest::to_hex(relative.to_string_lossy().as_bytes()),
            }
            .into());
        };
        Ok(normalize_path(text.replace('\\', "/").as_bytes())?)
    }
}

fn link_target_bytes(target: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        target.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        target.to_string_lossy().replace('\\', "/").into_bytes()
    }
}

fn is_executable(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

fn special_file_kind(file_type: &std::fs::FileType) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_fifo() {
            return "FIFO";
        }
        if file_type.is_socket() {
            return "socket";
        }
        if file_type.is_block_device() || file_type.is_char_device() {
            return "device";
        }
    }
    let _ = file_type;
    "special file"
}

impl Vcs for Dir {
    fn root(&self) -> &Path {
        &self.root
    }

    fn list(&self, at: &Snapshot) -> Result<Listing, Error> {
        if *at != Snapshot::default() && *at != Snapshot::Worktree {
            return Err(Error::UnknownRevision {
                rev: format!("{at:?}"),
                detail: "a directory has no revisions".into(),
            });
        }
        Ok(Listing {
            revision: Revision {
                vcs: "dir".into(),
                ..Revision::default()
            },
            entries: self.walk()?,
        })
    }

    fn hash(&self, entries: &[&Entry], stats: &mut HashStats) -> Result<Vec<(Sha256, u64)>, Error> {
        let mut cache = match &self.options.cache_dir {
            Some(dir) => StatCache::open(dir, &self.root),
            None => StatCache::disabled(),
        };
        // Stat every file, then hash the ones the cache can't vouch for, in parallel.
        let stats_of: Vec<Option<Stat>> = entries
            .par_iter()
            .map(|entry| match &entry.content {
                Content::File { path } => {
                    std::fs::symlink_metadata(path).ok().map(|m| Stat::of(&m))
                }
                _ => None,
            })
            .collect();
        let results: Vec<Result<(Sha256, u64, bool), Error>> = entries
            .par_iter()
            .zip(&stats_of)
            .map(|(entry, stat)| match &entry.content {
                Content::Bytes(bytes) => Ok((sha256(bytes), bytes.len() as u64, false)),
                Content::File { path } => {
                    if let Some(stat) = stat
                        && let Some(sha) = cache.get(&entry.path, stat)
                    {
                        return Ok((sha, stat.size, true));
                    }
                    let (sha, size) = hash_file(path).map_err(|source| Error::Io {
                        path: path.clone(),
                        source,
                    })?;
                    Ok((sha, size, false))
                }
                Content::Object { .. } => unreachable!("directories have no objects"),
            })
            .collect();
        let mut out = Vec::with_capacity(entries.len());
        for ((entry, stat), result) in entries.iter().zip(&stats_of).zip(results) {
            let (sha, size, hit) = result?;
            if hit {
                stats.cache_hits += 1;
            } else {
                stats.hashed += 1;
                stats.hashed_bytes += size;
                if let Some(stat) = stat {
                    // Only cache what didn't change while we read it.
                    if stat.size == size {
                        cache.insert(&entry.path, *stat, sha);
                    }
                }
            }
            out.push((sha, size));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as i64);
        if let Err(err) = cache.save(now) {
            tracing::warn!("could not write the stat cache: {err}");
        }
        Ok(out)
    }

    fn read_entry(&self, entry: &Entry) -> Result<Vec<u8>, Error> {
        match &entry.content {
            Content::Bytes(bytes) => Ok(bytes.clone()),
            Content::File { path } => std::fs::read(path).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            }),
            Content::Object { .. } => unreachable!("directories have no objects"),
        }
    }

    fn read_file_at(&self, rev: &str, _path: &str) -> Result<Vec<u8>, Error> {
        Err(Error::UnknownRevision {
            rev: rev.to_string(),
            detail: "a directory has no revisions".into(),
        })
    }

    fn remote_head(&self, _remote: &str, _branch: &str) -> Result<Option<String>, Error> {
        Err(Error::UnknownRevision {
            rev: "remote head".into(),
            detail: "a directory has no remote".into(),
        })
    }

    fn tree(&self, _rev: &str) -> Result<Option<String>, Error> {
        Ok(None)
    }

    fn fetch_commit(&self, _remote: &str, _sha: &str) -> Result<(), Error> {
        Err(Error::UnknownRevision {
            rev: "fetch".into(),
            detail: "a directory has no remote".into(),
        })
    }
}
