//! The digests of what a compile read (B-52, B-54), and the run memo that answers for the files
//! under the SDK, the toolchain and the system folders by their stat.
//!
//! A unit of a C++ test reads about a thousand files, four in five of them the SDK's or the
//! toolchain's, the same for every unit of the run. Hashing them all at every hit was most of a
//! hit's cost. The memo keeps, for each folder of such files, each file's stat and SHA-256 (and
//! what it holds, for a compile that stores), in one small file under the run's session folder.
//! An entry answers only while the file's size, both dates, inode and device are the ones it
//! holds, as the wrapper's run digests do (B-6). A folder's file is written whole, under a
//! temporary name, and renamed into place; two processes that write one at once lose only what
//! the other added, which is hashed again. The session folder goes with the run.
//!
//! Files under the checkout are hashed every time: they are the ones a person or an agent edits
//! while a build runs.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;
use serde::{Deserialize, Serialize};

use super::text::{self, Seen};
use crate::regular;

/// The folder of entries under the session's folder.
pub const DIR: &str = "cc-digests";

/// The schema every entry carries.
const SCHEMA: &str = "hushrun.cc-run-digests/2";

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

/// One file of a folder's entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    inode: u64,
    device: u64,
    sha256: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen: Option<Seen>,
}

impl Kept {
    fn of(hashed: &Hashed) -> Kept {
        Kept {
            size: hashed.stat.size,
            mtime_ns: hashed.stat.mtime_ns,
            ctime_ns: hashed.stat.ctime_ns,
            inode: hashed.stat.inode,
            device: hashed.stat.device,
            sha256: hashed.digest,
            seen: hashed.seen.clone(),
        }
    }

    fn stat(&self, mode: u32) -> Stat {
        Stat {
            size: self.size,
            mtime_ns: self.mtime_ns,
            ctime_ns: self.ctime_ns,
            inode: self.inode,
            device: self.device,
            mode,
        }
    }
}

/// A folder's entry.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Folder {
    schema: String,
    folder: String,
    files: BTreeMap<String, Kept>,
}

/// The digests one launcher process has taken, and the run memo's folders it has read.
#[derive(Debug, Default)]
pub struct Digests {
    session: Option<PathBuf>,
    known: HashMap<PathBuf, Hashed>,
    /// Each folder read from the memo, and whether this process added to it.
    folders: HashMap<PathBuf, (Folder, bool)>,
}

impl Digests {
    /// No digests yet; `session` is the run's folder, where the memo lives (`None`: no memo).
    pub fn new(session: Option<&Path>) -> Digests {
        Digests {
            session: session.map(Path::to_path_buf),
            ..Digests::default()
        }
    }

    /// The digest of `path`, for a lookup. `base`: the file lies under the SDK, the toolchain or
    /// a system folder, and the memo may answer for it.
    pub fn digest(&mut self, path: &Path, base: bool) -> io::Result<Digest> {
        if let Some(known) = self.known.get(path) {
            return Ok(known.digest);
        }
        if base && let Some(kept) = self.kept(path) {
            let digest = kept.digest;
            self.known.insert(path.to_path_buf(), kept);
            return Ok(digest);
        }
        let (digest, stat) = regular::hash_steady(path)?;
        let hashed = Hashed {
            digest,
            stat,
            seen: None,
        };
        if base {
            self.keep(path, &hashed);
        }
        self.known.insert(path.to_path_buf(), hashed);
        Ok(digest)
    }

    /// The digest of `path` and what it holds, for a compile that stores (B-54).
    pub fn read(&mut self, path: &Path, base: bool) -> io::Result<Hashed> {
        if let Some(known) = self.known.get(path)
            && known.seen.is_some()
        {
            return Ok(known.clone());
        }
        if base
            && let Some(kept) = self.kept(path)
            && kept.seen.is_some()
        {
            self.known.insert(path.to_path_buf(), kept.clone());
            return Ok(kept);
        }
        let hashed = read_steady(path)?;
        if base {
            self.keep(path, &hashed);
        }
        self.known.insert(path.to_path_buf(), hashed.clone());
        Ok(hashed)
    }

    /// What the memo holds for `path`, when its stat still holds.
    fn kept(&mut self, path: &Path) -> Option<Hashed> {
        let (folder, name) = (path.parent()?, path.file_name()?.to_str()?);
        let meta = fs::metadata(path).ok()?;
        if !regular::is_regular(&meta) {
            return None;
        }
        let now = Stat::of(&meta);
        let entry = self.folder(folder)?;
        let kept = entry.files.get(name)?;
        let then = kept.stat(now.mode);
        regular::same(&then, &now).then(|| Hashed {
            digest: kept.sha256,
            stat: then,
            seen: kept.seen.clone(),
        })
    }

    /// Adds `path` to its folder's entry, written by [`Digests::flush`].
    fn keep(&mut self, path: &Path, hashed: &Hashed) {
        let (Some(folder), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        else {
            return;
        };
        if self.session.is_none() {
            return;
        }
        if self.folder(folder).is_none() {
            self.folders.insert(
                folder.to_path_buf(),
                (
                    Folder {
                        schema: SCHEMA.to_string(),
                        folder: folder.to_string_lossy().into_owned(),
                        files: BTreeMap::new(),
                    },
                    false,
                ),
            );
        }
        if let Some((entry, changed)) = self.folders.get_mut(folder) {
            entry.files.insert(name.to_string(), Kept::of(hashed));
            *changed = true;
        }
    }

    /// A folder's entry, read once per process; `None` without a memo.
    fn folder(&mut self, folder: &Path) -> Option<&Folder> {
        let session = self.session.as_ref()?;
        if !self.folders.contains_key(folder) {
            let read = fs::read(entry_path(session, folder))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Folder>(&bytes).ok())
                .filter(|f| f.schema == SCHEMA && Path::new(&f.folder) == folder);
            let read = read?;
            self.folders.insert(folder.to_path_buf(), (read, false));
        }
        self.folders.get(folder).map(|(f, _)| f)
    }

    /// Writes each folder's entry this process added to, whole. Best effort: an entry is a
    /// saving, never the answer's source of truth.
    pub fn flush(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let dir = session.join(DIR);
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        for (folder, (entry, changed)) in &mut self.folders {
            if !*changed {
                continue;
            }
            *changed = false;
            let path = entry_path(&session, folder);
            // What another process wrote since this one read it stays, unless this one hashed
            // the same file again.
            if let Some(theirs) = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Folder>(&bytes).ok())
                .filter(|f| f.schema == SCHEMA && f.folder == entry.folder)
            {
                for (name, kept) in theirs.files {
                    entry.files.entry(name).or_insert(kept);
                }
            }
            let Ok(bytes) = serde_json::to_vec(&entry) else {
                continue;
            };
            let _ = write_whole(&dir, &path, &bytes);
        }
    }
}

fn entry_path(session: &Path, folder: &Path) -> PathBuf {
    let name = Digest::of(folder.to_string_lossy().as_bytes()).hex();
    session.join(DIR).join(format!("{name}.json"))
}

/// Writes `path` in `dir` whole: a temporary file of its own, renamed into place.
pub(crate) fn write_whole(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    for n in 0..64 {
        let temp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let written = file.write_all(bytes).and_then(|()| fs::rename(&temp, path));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        return written;
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free temporary name for a run digest",
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-52
    #[test]
    fn a_base_file_is_answered_by_its_stat_across_processes_and_hashed_again_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let sdk = dir.path().join("sdk/usr/include");
        fs::create_dir_all(&sdk).unwrap();
        let header = sdk.join("stdio.h");
        fs::write(
            &header,
            b"int printf(const char *, ...);\n#if __has_include(<x.h>)\n#endif\n",
        )
        .unwrap();
        let mut first = Digests::new(Some(&session));
        let digest = first.digest(&header, true).unwrap();
        assert_eq!(digest, Digest::of(&fs::read(&header).unwrap()));
        first.flush();
        assert_eq!(fs::read_dir(session.join(DIR)).unwrap().count(), 1);

        // Another process: the memo answers, not the bytes (shown by changing what it holds).
        let path = entry_path(&session, &sdk);
        let mut kept: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        kept["files"]["stdio.h"]["sha256"] =
            serde_json::to_value(Digest::of(b"remembered")).unwrap();
        fs::write(&path, serde_json::to_vec(&kept).unwrap()).unwrap();
        assert_eq!(
            Digests::new(Some(&session)).digest(&header, true).unwrap(),
            Digest::of(b"remembered")
        );
        // A file under the checkout is hashed every time.
        assert_eq!(
            Digests::new(Some(&session)).digest(&header, false).unwrap(),
            digest
        );

        // Rewritten in place with its old modification time: the change time moved.
        let old = fs::metadata(&header).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&header, b"int puts(const char *);\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&header)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let mut again = Digests::new(Some(&session));
        assert_eq!(
            again.digest(&header, true).unwrap(),
            Digest::of(b"int puts(const char *);\n")
        );
        // A read for a store keeps what the file holds with its digest.
        let read = again.read(&header, true).unwrap();
        assert_eq!(read.seen.unwrap(), text::scan(b"int puts(const char *);\n"));
        again.flush();
        let read = Digests::new(Some(&session)).read(&header, true).unwrap();
        assert!(read.seen.is_some());
    }

    // covers: B-52
    #[test]
    fn without_a_session_nothing_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.h");
        fs::write(&file, b"x").unwrap();
        let mut digests = Digests::new(None);
        assert_eq!(digests.digest(&file, true).unwrap(), Digest::of(b"x"));
        digests.flush();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
