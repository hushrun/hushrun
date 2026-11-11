//! The one definition of a file the wrapper may open to read (B-19): a regular file. A named
//! pipe, a socket or a device where a file is expected is never opened, since opening a pipe for
//! reading blocks until a writer comes, which may be never. Such an input can't be keyed: the
//! compile it belongs to is not stored, and a hint that names one is skipped.
//!
//! The file is stat'ed first, then opened without blocking and stat'ed again through the open
//! handle, so a pipe put in its place between the two is refused too.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;
use hushrun_digest::hash::Hasher;

/// How much of a file is read at a time.
const CHUNK: usize = 1 << 20;

/// The error a path that is not a regular file gives.
#[derive(Debug, thiserror::Error)]
#[error("{} is not a regular file", .0.display())]
pub struct NotRegular(pub PathBuf);

/// Whether a file of this kind may be opened: a regular file (a symlink is followed first by
/// whoever made `meta`).
pub fn is_regular(meta: &fs::Metadata) -> bool {
    meta.file_type().is_file()
}

/// Whether `err` says the path was not a regular file.
pub fn is_not_regular(err: &io::Error) -> bool {
    err.get_ref().is_some_and(|inner| inner.is::<NotRegular>())
}

fn not_regular(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, NotRegular(path.to_path_buf()))
}

/// Opens `path` for reading when it is a regular file, following symlinks. Anything else is a
/// [`NotRegular`] error, and nothing that could block is opened.
pub fn open(path: &Path) -> io::Result<File> {
    if !is_regular(&fs::metadata(path)?) {
        return Err(not_regular(path));
    }
    let file = open_without_blocking(path)?;
    if !is_regular(&file.metadata()?) {
        return Err(not_regular(path));
    }
    Ok(file)
}

#[cfg(unix)]
fn open_without_blocking(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NONBLOCK changes nothing for a regular file; for a pipe swapped in after the stat it
    // makes `open` return at once, and the check on the handle refuses it.
    let flags = i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap_or(0);
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
}

#[cfg(not(unix))]
fn open_without_blocking(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// The whole of a regular file.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The SHA-256 and size of a regular file, read through [`open`].
pub fn hash_file(path: &Path) -> io::Result<(Digest, u64)> {
    let mut file = open(path)?;
    hash_open(&mut file)
}

fn hash_open(file: &mut File) -> io::Result<(Digest, u64)> {
    let len = file.metadata()?.len();
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; CHUNK.min(usize::try_from(len).unwrap_or(CHUNK)).max(1)];
    let mut size = 0u64;
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                size += n as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok((Digest::from_bytes(hasher.finish()), size))
}

/// The SHA-256 of a regular file, stat'ed through the open handle before and through the path
/// after: an error when it changed while it was read, so the hash describes one version of it.
/// The stat returned is the one from before the read.
pub fn hash_steady(path: &Path) -> io::Result<(Digest, Stat)> {
    let mut file = open(path)?;
    let before = Stat::of(&file.metadata()?);
    let (sha, _) = hash_open(&mut file)?;
    let after = Stat::of(&fs::metadata(path)?);
    if !same(&before, &after) {
        return Err(io::Error::other(format!(
            "{} changed while it was hashed",
            path.display()
        )));
    }
    Ok((sha, before))
}

/// Whether two stats are the same file, unchanged: size, both dates, inode and device (B-11).
pub fn same(a: &Stat, b: &Stat) -> bool {
    a.size == b.size
        && a.mtime_ns == b.mtime_ns
        && a.ctime_ns == b.ctime_ns
        && a.inode == b.inode
        && a.device == b.device
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-19
    #[test]
    fn a_regular_file_is_hashed_and_a_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        fs::write(&file, b"bytes").unwrap();
        assert_eq!(hash_file(&file).unwrap(), (Digest::of(b"bytes"), 5));
        assert_eq!(hash_steady(&file).unwrap().0, Digest::of(b"bytes"));
        assert_eq!(read(&file).unwrap(), b"bytes");
        let err = open(dir.path()).unwrap_err();
        assert!(is_not_regular(&err), "{err}");
        let gone = open(&dir.path().join("gone")).unwrap_err();
        assert_eq!(gone.kind(), io::ErrorKind::NotFound);
        assert!(!is_not_regular(&gone));
    }

    // covers: B-19
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_refused_without_blocking_also_behind_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let pipe = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap();
        assert!(made.success());
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&pipe, &link).unwrap();
        for path in [&pipe, &link] {
            for err in [
                open(path).unwrap_err(),
                hash_file(path).unwrap_err(),
                hash_steady(path).unwrap_err(),
                read(path).unwrap_err(),
            ] {
                assert!(is_not_regular(&err), "{err}");
            }
        }
        // The open that follows the stat doesn't block either.
        assert!(is_not_regular(
            &open_without_blocking(&pipe)
                .and_then(|f| {
                    if is_regular(&f.metadata()?) {
                        Ok(())
                    } else {
                        Err(not_regular(&pipe))
                    }
                })
                .unwrap_err()
        ));
    }
}
