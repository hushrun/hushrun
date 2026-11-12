//! The record beside the outputs in a target folder: which block each file came from and its
//! SHA-256, so dependents do not hash it again. It dies with `cargo clean`.
//!
//! A later compile takes a crate's files as inputs (`--extern name=path`) and keys them by
//! SHA-256 (B-6). Hashing a dependency again in each of its dependents would cost gigabytes per
//! build, so each output file `<out dir>/<name>` gets one small record,
//! `<out dir>/.hushrun/<name>.json`. It describes a file in that folder, so it lives and dies
//! with the folder.
//!
//! # The same file
//!
//! A record answers for a file while the file's size, modification time, inode and device are
//! the ones recorded. The change time is not compared, since a rename or a permission change
//! moves it, and neither is the mode. When any compared field differs the file is hashed: same
//! bytes keep the record with the new stat (cargo resets the modification time of some outputs
//! after the compiler exits, and a reuse sets its own), other bytes replace it with one that
//! names no block.
//!
//! One change can't be seen: the file rewritten in place, on the same inode, to the same size,
//! with its old modification time put back. Nothing in the wrapper or cargo does that.
//!
//! # Writers
//!
//! Many wrapper processes write records in one folder at once, each for its own files. A record
//! is written whole, under a temporary name in `.hushrun/`, and renamed into place, so two
//! writers of one record leave one whole record. A record that can't be read or parsed is
//! treated as absent, never as an error.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use hushrun_core::Digest;
use hushrun_digest::cache::Stat;
use hushrun_digest::hash::Hasher;
use serde::{Deserialize, Serialize};

/// The hidden folder of an output folder that holds the records.
pub const DIR: &str = ".hushrun";

/// The extension of a record's file name, after the output's own name.
const EXT: &str = "json";

/// The schema name every record carries. A record with another one is treated as absent.
const SCHEMA: &str = "hushrun.build.sidecar/2";

/// How much of a file is read at a time when it has to be hashed.
const CHUNK: usize = 1 << 20;

/// What is known of one output file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// The file as it was when recorded.
    pub stat: Stat,
    /// The SHA-256 of its bytes.
    pub sha256: Digest,
    /// The key of the block it came from, without its place, when the wrapper produced or
    /// reused it and the block may move (B-6, B-10). A tied block's file is known by its bytes.
    pub produced_by: Option<Digest>,
    /// The first key of the block it came from, so a dependent that can't load it can suspend
    /// that key (B-15).
    pub step: Option<Digest>,
    /// Whether a reuse put it there. These are the files removed when a dependent fails to
    /// compile against them (B-15).
    pub reused: bool,
    /// Whether a dependent on another machine would name this file as one here does: the block
    /// it came from is portable, or is a proc macro named by what it was built from
    /// ([`crate::portable::Portability::named_portably`]).
    pub portable: bool,
}

/// The answer of [`lookup`]: the file's SHA-256 now and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Known {
    /// The SHA-256 of the file as it is now.
    pub sha256: Digest,
    /// The key of the block it came from, if its record still holds.
    pub produced_by: Option<Digest>,
    /// The first key of the block it came from, if its record still holds.
    pub step: Option<Digest>,
    /// Whether a reuse put it there, if its record still holds.
    pub reused: bool,
    /// Whether another machine names the file the same way, if its record still holds.
    pub portable: bool,
    /// Whether the file had to be hashed to answer.
    pub read: bool,
}

/// A record as written: the stat's fields one by one, so the stat type needs no serde.
#[derive(Serialize, Deserialize)]
struct Stored {
    schema: String,
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    inode: u64,
    device: u64,
    mode: u32,
    sha256: Digest,
    produced_by: Option<Digest>,
    #[serde(default)]
    step: Option<Digest>,
    reused: bool,
    #[serde(default)]
    portable: bool,
}

impl From<&Record> for Stored {
    fn from(record: &Record) -> Stored {
        Stored {
            schema: SCHEMA.to_string(),
            size: record.stat.size,
            mtime_ns: record.stat.mtime_ns,
            ctime_ns: record.stat.ctime_ns,
            inode: record.stat.inode,
            device: record.stat.device,
            mode: record.stat.mode,
            sha256: record.sha256,
            produced_by: record.produced_by,
            step: record.step,
            reused: record.reused,
            portable: record.portable,
        }
    }
}

impl Stored {
    fn into_record(self) -> Option<Record> {
        (self.schema == SCHEMA).then_some(Record {
            stat: Stat {
                size: self.size,
                mtime_ns: self.mtime_ns,
                ctime_ns: self.ctime_ns,
                inode: self.inode,
                device: self.device,
                mode: self.mode,
            },
            sha256: self.sha256,
            produced_by: self.produced_by,
            step: self.step,
            reused: self.reused,
            portable: self.portable,
        })
    }
}

/// Whether two stats describe the same file for a record: size, modification time, inode and
/// device. Not the change time and not the mode.
fn same_file(a: &Stat, b: &Stat) -> bool {
    a.size == b.size && a.mtime_ns == b.mtime_ns && a.inode == b.inode && a.device == b.device
}

/// Whether a file stayed put while it was hashed: every field a change moves, the change time
/// too (B-11).
fn unchanged(a: &Stat, b: &Stat) -> bool {
    same_file(a, b) && a.ctime_ns == b.ctime_ns
}

/// Where the record of `file` lives: `<its folder>/.hushrun/<file name>.json`. `None` for a
/// path with no file name.
pub fn record_path(file: &Path) -> Option<PathBuf> {
    let name = file.file_name()?;
    let dir = file.parent().unwrap_or(Path::new(""));
    let mut record = OsString::with_capacity(name.len() + EXT.len() + 1);
    record.push(name);
    record.push(".");
    record.push(EXT);
    Some(dir.join(DIR).join(record))
}

fn no_name(file: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} has no file name to keep a record for", file.display()),
    )
}

/// Records `file` with its SHA-256, the block it came from and whether a reuse put it there.
/// The file is stat'ed now, so the caller records it after its last change: after rustc exits
/// or after the reuse renamed it into place.
pub fn record(
    file: &Path,
    sha256: Digest,
    produced_by: Option<Digest>,
    reused: bool,
) -> io::Result<()> {
    record_block(file, sha256, produced_by, None, reused, false)
}

/// [`record`], with the first key of the block the file came from and whether another machine
/// names it the same way.
pub fn record_block(
    file: &Path,
    sha256: Digest,
    produced_by: Option<Digest>,
    step: Option<Digest>,
    reused: bool,
    portable: bool,
) -> io::Result<()> {
    let stat = Stat::of(&fs::metadata(file)?);
    write(
        file,
        &Record {
            stat,
            sha256,
            produced_by,
            step,
            reused,
            portable,
        },
    )
}

/// The SHA-256 of `file` as it is now, and the block it came from. A record whose stat still
/// holds answers without reading the file. Otherwise the file is hashed and the record kept
/// (same bytes, new stat) or replaced by one that names no block (other bytes, or no record).
///
/// A record that can't be written is not an error: the answer is right either way and the
/// next lookup hashes again. A file that changes while it is hashed is an error, since its
/// hash describes no version of it.
pub fn lookup(file: &Path) -> io::Result<Known> {
    let now = Stat::of(&fs::metadata(file)?);
    let old = load(file);
    if let Some(old) = old
        && same_file(&old.stat, &now)
    {
        return Ok(Known {
            sha256: old.sha256,
            produced_by: old.produced_by,
            step: old.step,
            reused: old.reused,
            portable: old.portable,
            read: false,
        });
    }
    let (sha256, stat) = hash(file)?;
    let fresh = match old {
        Some(old) if old.sha256 == sha256 => Record { stat, ..old },
        _ => Record {
            stat,
            sha256,
            produced_by: None,
            step: None,
            reused: false,
            portable: false,
        },
    };
    // Best effort: a record is a saving, never the answer's source of truth here.
    let _ = write(file, &fresh);
    Ok(Known {
        sha256: fresh.sha256,
        produced_by: fresh.produced_by,
        step: fresh.step,
        reused: fresh.reused,
        portable: fresh.portable,
        read: true,
    })
}

/// Whether a reuse put `file` there: its record says so and still holds for the file as it is
/// (B-15). Never reads the file itself, and stats it only when its record says reused: a
/// file with no record costs one failed open.
pub fn reused(file: &Path) -> bool {
    let Some(record) = load(file).filter(|record| record.reused) else {
        return false;
    };
    fs::metadata(file).is_ok_and(|meta| same_file(&record.stat, &Stat::of(&meta)))
}

/// Removes the record of `file`. A missing record is fine.
pub fn forget(file: &Path) -> io::Result<()> {
    let path = record_path(file).ok_or_else(|| no_name(file))?;
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// The files in `dir` whose records say a reuse put them there (B-15), sorted. A record whose
/// file is gone is skipped, and so is a record that can't be parsed. The record is not checked
/// against the file's stat: cargo may have reset the file's modification time, and removing a
/// file cargo then builds again costs a compile, while keeping a bad one costs a failed build.
pub fn reused_in(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let records = match fs::read_dir(dir.join(DIR)) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut files = Vec::new();
    for entry in records {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != EXT) {
            continue;
        }
        let Some(name) = path.file_stem() else {
            continue;
        };
        let file = dir.join(name);
        if parse(&path).is_some_and(|record| record.reused) && fs::symlink_metadata(&file).is_ok() {
            files.push(file);
        }
    }
    files.sort();
    Ok(files)
}

/// The record of `file`, or `None` when it is missing, unreadable or not a record.
fn load(file: &Path) -> Option<Record> {
    parse(&record_path(file)?)
}

fn parse(path: &Path) -> Option<Record> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice::<Stored>(&bytes).ok()?.into_record()
}

/// Writes the record of `file` whole: a temporary file in `.hushrun/`, then a rename. The
/// temporary name begins with a dot, never with `lib`, and is unique to this process and
/// attempt, so writers in other processes and threads never share one.
fn write(file: &Path, record: &Record) -> io::Result<()> {
    let path = record_path(file).ok_or_else(|| no_name(file))?;
    let dir = path.parent().unwrap_or(Path::new(""));
    fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(&Stored::from(record)).map_err(io::Error::other)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let pid = std::process::id();
    let mut attempt = 0u32;
    let (tmp, mut out) = loop {
        let tmp = dir.join(format!(".{name}.{pid}.{attempt}.tmp"));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(out) => break (tmp, out),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && attempt < 1000 => {
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    };
    let written = out.write_all(&bytes).and_then(|()| {
        drop(out);
        fs::rename(&tmp, &path)
    });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// The SHA-256 of `file` and its stat when it was opened. An error when the file changed while
/// it was read.
fn hash(file: &Path) -> io::Result<(Digest, Stat)> {
    let mut input = crate::regular::open(file)?;
    let before = Stat::of(&input.metadata()?);
    let mut hasher = Hasher::new();
    let mut buf = vec![
        0u8;
        CHUNK
            .min(usize::try_from(before.size).unwrap_or(CHUNK))
            .max(1)
    ];
    loop {
        match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    let after = Stat::of(&fs::metadata(file)?);
    if !unchanged(&before, &after) {
        return Err(io::Error::other(format!(
            "{} changed while it was hashed",
            file.display()
        )));
    }
    Ok((Digest::from_bytes(hasher.finish()), before))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn key(n: u8) -> Digest {
        Digest::from_bytes([n; 32])
    }

    fn set_mtime(path: &Path, time: SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    fn mtime(path: &Path) -> SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn the_record_of_an_rmeta_sits_in_the_hidden_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rmeta");
        fs::write(&file, b"rmeta").unwrap();
        assert_eq!(
            record_path(&file).unwrap(),
            dir.path().join(".hushrun").join("libfoo-0123.rmeta.json")
        );
        record(&file, Digest::of(b"rmeta"), Some(key(1)), false).unwrap();
        // Only the output and the hidden folder: nothing new for rustc's `lib` scan.
        let mut top: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        top.sort();
        assert_eq!(
            top,
            vec![OsString::from(".hushrun"), "libfoo-0123.rmeta".into()]
        );
        // And no temporary file left beside the record.
        let inside: Vec<_> = fs::read_dir(dir.path().join(DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(inside, vec![OsString::from("libfoo-0123.rmeta.json")]);
    }

    // covers: B-6
    #[test]
    fn a_recorded_file_answers_without_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rlib");
        fs::write(&file, b"rlib bytes").unwrap();
        // A hash that is not the file's: only an answer from the record can give it back.
        let recorded = key(9);
        record(&file, recorded, Some(key(1)), true).unwrap();
        let known = lookup(&file).unwrap();
        assert_eq!(
            known,
            Known {
                sha256: recorded,
                produced_by: Some(key(1)),
                step: None,
                reused: true,
                portable: false,
                read: false,
            }
        );
    }

    // covers: B-6
    #[test]
    fn a_moved_modification_time_over_the_same_bytes_keeps_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rmeta");
        fs::write(&file, b"metadata").unwrap();
        record(&file, Digest::of(b"metadata"), Some(key(2)), false).unwrap();
        // What cargo does to a check output after rustc exits.
        set_mtime(&file, mtime(&file) + Duration::from_secs(3));

        let known = lookup(&file).unwrap();
        assert_eq!(known.sha256, Digest::of(b"metadata"));
        assert_eq!(known.produced_by, Some(key(2)));
        assert!(known.read);

        let again = lookup(&file).unwrap();
        assert_eq!(again.produced_by, Some(key(2)));
        assert!(!again.read, "the new stat was written");
    }

    // covers: B-6
    #[test]
    fn other_bytes_with_the_same_size_and_time_on_a_new_inode_drop_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rlib");
        fs::write(&file, b"version one").unwrap();
        record(&file, Digest::of(b"version one"), Some(key(3)), true).unwrap();
        let old = fs::metadata(&file).unwrap();

        // Written beside it while it exists, so it has another inode, then renamed over it.
        let other = dir.path().join("other");
        fs::write(&other, b"version two").unwrap();
        set_mtime(&other, old.modified().unwrap());
        fs::rename(&other, &file).unwrap();
        let new = fs::metadata(&file).unwrap();
        assert_eq!(new.len(), old.len());
        assert_eq!(new.modified().unwrap(), old.modified().unwrap());

        let known = lookup(&file).unwrap();
        assert_eq!(
            known,
            Known {
                sha256: Digest::of(b"version two"),
                produced_by: None,
                step: None,
                reused: false,
                portable: false,
                read: true,
            }
        );
        let stored = load(&file).unwrap();
        assert_eq!((stored.produced_by, stored.reused), (None, false));
        assert_eq!(stored.sha256, Digest::of(b"version two"));
        assert!(!lookup(&file).unwrap().read);
    }

    /// The one change a stat can't see, kept here so it stays a known limit, not a surprise.
    #[test]
    fn an_in_place_rewrite_with_the_same_size_and_time_is_not_seen() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rlib");
        fs::write(&file, b"version one").unwrap();
        record(&file, Digest::of(b"version one"), Some(key(4)), false).unwrap();
        let old = mtime(&file);
        fs::write(&file, b"version two").unwrap();
        set_mtime(&file, old);
        let known = lookup(&file).unwrap();
        assert_eq!(known.sha256, Digest::of(b"version one"));
        assert!(!known.read);
    }

    // covers: B-6
    #[test]
    fn a_corrupt_record_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rmeta");
        fs::write(&file, b"metadata").unwrap();
        record(&file, key(7), Some(key(5)), true).unwrap();
        let path = record_path(&file).unwrap();
        for corrupt in [&b"{\"schema\": \"hushrun.build.sid"[..], b"", b"[]"] {
            fs::write(&path, corrupt).unwrap();
            let known = lookup(&file).unwrap();
            assert_eq!(
                known,
                Known {
                    sha256: Digest::of(b"metadata"),
                    produced_by: None,
                    step: None,
                    reused: false,
                    portable: false,
                    read: true,
                }
            );
            // Replaced by a whole record.
            assert!(!lookup(&file).unwrap().read);
        }
    }

    #[test]
    fn a_record_of_another_schema_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rmeta");
        fs::write(&file, b"metadata").unwrap();
        record(&file, key(7), Some(key(5)), true).unwrap();
        let path = record_path(&file).unwrap();
        let text = fs::read_to_string(&path)
            .unwrap()
            .replace(SCHEMA, "hushrun.build.sidecar/1");
        fs::write(&path, text).unwrap();
        assert!(load(&file).is_none());
        assert_eq!(lookup(&file).unwrap().produced_by, None);
    }

    // covers: B-6
    #[test]
    fn a_missing_record_hashes_the_file_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libbar-4567.rlib");
        fs::write(&file, b"no record yet").unwrap();
        let known = lookup(&file).unwrap();
        assert_eq!(
            known,
            Known {
                sha256: Digest::of(b"no record yet"),
                produced_by: None,
                step: None,
                reused: false,
                portable: false,
                read: true,
            }
        );
        assert!(!lookup(&file).unwrap().read);
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(lookup(&dir.path().join("gone")).is_err());
        assert!(record(&dir.path().join("gone"), key(1), None, false).is_err());
    }

    #[test]
    fn forget_removes_the_record_and_a_missing_one_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rmeta");
        fs::write(&file, b"metadata").unwrap();
        record(&file, key(7), Some(key(5)), true).unwrap();
        forget(&file).unwrap();
        assert!(load(&file).is_none());
        forget(&file).unwrap();
        forget(&dir.path().join("never-recorded")).unwrap();
    }

    // covers: B-15
    #[test]
    fn reused_in_lists_only_files_a_reuse_put_there() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        assert!(reused_in(out).unwrap().is_empty(), "no records at all");
        let write_file = |name: &str| {
            let path = out.join(name);
            fs::write(&path, name).unwrap();
            path
        };
        let a = write_file("liba-0001.rmeta");
        let b = write_file("libb-0002.rlib");
        let built = write_file("libc-0003.rlib");
        let plain = write_file("libd-0004.rlib");
        let gone = write_file("libe-0005.rlib");
        let corrupt = write_file("libf-0006.rlib");
        record(&a, key(1), Some(key(1)), true).unwrap();
        record(&b, key(2), Some(key(2)), true).unwrap();
        record(&built, key(3), Some(key(3)), false).unwrap();
        lookup(&plain).unwrap();
        record(&gone, key(5), Some(key(5)), true).unwrap();
        fs::remove_file(&gone).unwrap();
        record(&corrupt, key(6), Some(key(6)), true).unwrap();
        fs::write(record_path(&corrupt).unwrap(), b"{").unwrap();
        // A stray temporary file is not a record.
        fs::write(out.join(DIR).join(".liba-0001.rmeta.json.1.0.tmp"), b"{}").unwrap();

        assert_eq!(reused_in(out).unwrap(), vec![a, b]);
    }

    #[test]
    fn writers_of_one_record_leave_one_whole_record() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("libfoo-0123.rlib");
        fs::write(&file, b"shared").unwrap();
        std::thread::scope(|scope| {
            for t in 0..8u8 {
                let file = &file;
                scope.spawn(move || {
                    for _ in 0..50 {
                        record(file, key(t), Some(key(t)), t % 2 == 0).unwrap();
                    }
                });
            }
        });
        let last = load(&file).unwrap();
        assert_eq!(Some(last.sha256), last.produced_by);
        let inside = fs::read_dir(dir.path().join(DIR)).unwrap().count();
        assert_eq!(inside, 1, "no temporary file left");
    }

    #[test]
    fn writers_of_many_records_in_one_folder_each_leave_theirs() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for t in 0..16u8 {
                let dir = dir.path();
                scope.spawn(move || {
                    let file = dir.join(format!("lib{t}-0000.rlib"));
                    fs::write(&file, [t]).unwrap();
                    record(&file, Digest::of(&[t]), Some(key(t)), true).unwrap();
                });
            }
        });
        for t in 0..16u8 {
            let known = lookup(&dir.path().join(format!("lib{t}-0000.rlib"))).unwrap();
            assert_eq!((known.produced_by, known.read), (Some(key(t)), false));
        }
        assert_eq!(reused_in(dir.path()).unwrap().len(), 16);
    }
}
