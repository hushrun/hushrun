//! Digests remembered for the length of one run (B-6): files the wrapper hashes in folders it
//! doesn't own and must not write records in, such as another package's build-script output
//! named with `-L native=`, a system library folder, or the standard library a link reads.
//!
//! One small file per hashed file, under the run's session folder, named by the file's path and
//! holding its stat and SHA-256. An entry answers only while the file's size, both dates, inode
//! and device are the ones it holds, so a file rewritten in place (which moves its change time)
//! is hashed again. The session folder goes with the run.
//!
//! Many wrapper processes read and write entries at once. An entry is written whole, under a
//! temporary name, and renamed into place; one that can't be read or parsed is absent. A large
//! file (a linker, the toolchain's `rust-lld`, B-37) is hashed under a lock on its entry's name,
//! so that the links a cold build starts at once hash it once and the others read the entry.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;
use serde::{Deserialize, Serialize};

use crate::regular;

/// The folder of entries under the session's folder.
pub const DIR: &str = "digests";

/// The schema every entry carries.
const SCHEMA: &str = "hushrun.build.run-digest/1";

/// From this size on, a file is hashed under a lock on its entry's name.
const LOCK_FROM: u64 = 8 << 20;

#[cfg(test)]
static HASHED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Serialize, Deserialize)]
struct Entry {
    schema: String,
    path: String,
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    inode: u64,
    device: u64,
    sha256: Digest,
}

/// The SHA-256 of the regular file `path` and its stat, from the entry under `session` when its
/// stat still holds, else hashed (and the entry written). `session` `None`: always hashed.
///
/// Cost on a hit: one `stat` and one small read. A file that is not a regular file is an error
/// ([`regular::NotRegular`]) and is never opened.
pub fn digest(session: Option<&Path>, path: &Path) -> io::Result<(Digest, Stat)> {
    let Some(session) = session else {
        return regular::hash_steady(path);
    };
    let meta = fs::metadata(path)?;
    if !regular::is_regular(&meta) {
        // Let the one definition say why.
        return regular::hash_steady(path);
    }
    let now = Stat::of(&meta);
    let Some(text) = path.to_str() else {
        return regular::hash_steady(path);
    };
    let dir = session.join(DIR);
    let entry = dir.join(format!("{}.json", Digest::of(text.as_bytes()).hex()));
    if let Some(sha) = read(&entry, text, &now) {
        return Ok((sha, now));
    }
    // One process hashes a large file; the others that wanted it at once wait, then read what
    // it wrote. Without a lock (it can't be taken), each hashes it.
    let _lock = (now.size >= LOCK_FROM)
        .then(|| EntryLock::take(&dir, &entry))
        .flatten();
    if _lock.is_some()
        && let Some(sha) = read(&entry, text, &now)
    {
        return Ok((sha, now));
    }
    #[cfg(test)]
    HASHED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let (sha, stat) = regular::hash_steady(path)?;
    // Best effort: an entry is a saving, never the answer's source of truth.
    let _ = write(&dir, &entry, text, &stat, sha);
    Ok((sha, stat))
}

/// A lock on an entry's name while one process hashes its file, removed when dropped.
struct EntryLock {
    file: fs::File,
    path: std::path::PathBuf,
}

impl EntryLock {
    fn take(dir: &Path, entry: &Path) -> Option<EntryLock> {
        fs::create_dir_all(dir).ok()?;
        let name = entry.file_name()?.to_string_lossy();
        let path = dir.join(format!(".{name}.lock"));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .ok()?;
        file.lock().ok()?;
        Some(EntryLock { file, path })
    }
}

impl Drop for EntryLock {
    fn drop(&mut self) {
        // Removed while still held: a process that comes later reads the entry, written by now.
        let _ = fs::remove_file(&self.path);
        let _ = self.file.unlock();
    }
}

fn read(entry: &Path, path: &str, now: &Stat) -> Option<Digest> {
    let bytes = fs::read(entry).ok()?;
    let kept: Entry = serde_json::from_slice(&bytes).ok()?;
    let then = Stat {
        size: kept.size,
        mtime_ns: kept.mtime_ns,
        ctime_ns: kept.ctime_ns,
        inode: kept.inode,
        device: kept.device,
        mode: now.mode,
    };
    (kept.schema == SCHEMA && kept.path == path && regular::same(&then, now)).then_some(kept.sha256)
}

fn write(dir: &Path, entry: &Path, path: &str, stat: &Stat, sha256: Digest) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(&Entry {
        schema: SCHEMA.to_string(),
        path: path.to_string(),
        size: stat.size,
        mtime_ns: stat.mtime_ns,
        ctime_ns: stat.ctime_ns,
        inode: stat.inode,
        device: stat.device,
        sha256,
    })
    .map_err(io::Error::other)?;
    let name = entry.file_name().unwrap_or_default().to_string_lossy();
    for n in 0..64 {
        let temp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let written = file
            .write_all(&bytes)
            .and_then(|()| fs::rename(&temp, entry));
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

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-6
    #[test]
    fn a_file_is_hashed_once_per_run_and_again_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let lib = dir.path().join("out/libx.a");
        fs::create_dir_all(lib.parent().unwrap()).unwrap();
        fs::write(&lib, b"version one").unwrap();
        let (first, _) = digest(Some(&session), &lib).unwrap();
        assert_eq!(first, Digest::of(b"version one"));
        // Nothing is written beside the file: the folder is another package's.
        assert_eq!(fs::read_dir(lib.parent().unwrap()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(session.join(DIR)).unwrap().count(), 1);

        // The entry answers while the stat holds: other bytes under the very same stat (which
        // nothing but this test makes) are not read.
        let entry = fs::read_dir(session.join(DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut kept: serde_json::Value =
            serde_json::from_slice(&fs::read(&entry).unwrap()).unwrap();
        kept["sha256"] = serde_json::to_value(Digest::of(b"remembered")).unwrap();
        fs::write(&entry, serde_json::to_vec(&kept).unwrap()).unwrap();
        assert_eq!(
            digest(Some(&session), &lib).unwrap().0,
            Digest::of(b"remembered")
        );

        // Rewritten in place with its old modification time: the change time moved.
        let old = fs::metadata(&lib).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&lib, b"version two").unwrap();
        fs::File::options()
            .write(true)
            .open(&lib)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(
            digest(Some(&session), &lib).unwrap().0,
            Digest::of(b"version two")
        );
        // Without a session: hashed every time.
        assert_eq!(digest(None, &lib).unwrap().0, Digest::of(b"version two"));
    }

    // covers: B-37
    #[test]
    fn a_large_file_wanted_by_many_at_once_is_hashed_once() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let big = dir.path().join("rust-lld");
        fs::write(&big, vec![7u8; (LOCK_FROM + 1) as usize]).unwrap();
        let before = HASHED.load(std::sync::atomic::Ordering::SeqCst);
        let digests: Vec<Digest> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| digest(Some(&session), &big).unwrap().0))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(digests.iter().all(|d| *d == digests[0]));
        assert_eq!(HASHED.load(std::sync::atomic::Ordering::SeqCst) - before, 1);
        // No lock is left behind.
        let left: Vec<_> = fs::read_dir(session.join(DIR))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".lock"))
            .collect();
        assert!(left.is_empty());
    }

    // covers: B-19
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_refused_with_or_without_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let pipe = dir.path().join("libx.a");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap();
        assert!(made.success());
        let err = digest(Some(dir.path()), &pipe).unwrap_err();
        assert!(regular::is_not_regular(&err), "{err}");
    }
}
