//! K-10, mechanism C: a seed, for any part whose reader is one. hushrun puts each file of a part
//! that a lockfile of the checkout pins into the tool's own cache, at the same path below it, and
//! the tool then takes it from there as it takes what it downloads.
//!
//! - A file is linked where it can be (the same file system, a file the run may link), else
//!   copied; either way it is first put beside its place and moved in only once its SHA-256, read
//!   from what was put there, is one the lockfile pins.
//! - A file already there is never written over: a seed only adds.
//! - Once every file of the part is in place, a stamp named for the store
//!   (`.hushrun-packages-<identity>`) is left in the cache, and the next seed of the same store
//!   looks at that and nothing else: one `stat`.
//!
//! Nothing here knows a tool: a part says what its lockfiles pin ([`crate::part::Part::pins`]).

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use hushrun_core::blocks::Hex32;
use sha2_hw::{Digest as _, Sha256};

use crate::part::Seeded;
use crate::store::{Kind, Row};

/// The start of a stamp's name.
pub const STAMP: &str = ".hushrun-packages-";

/// The stamp a seed of the store whose identity is `identity` leaves in a tool's cache.
pub fn stamp_name(identity: &Hex32) -> String {
    format!("{STAMP}{identity}")
}

/// Puts each of `rows` of the part at `dir` into `into`, at the same path, when `pins` gives the
/// SHA-256 of its bytes for its path, and leaves `stamp` in `into` once every row is there.
pub fn seed<'a>(
    dir: &Path,
    rows: &[Row],
    into: &Path,
    stamp: &str,
    pins: &dyn Fn(&str) -> &'a [String],
) -> Seeded {
    let mut seeded = Seeded::default();
    if fs::symlink_metadata(into.join(stamp)).is_ok() {
        seeded.stamped = true;
        return seeded;
    }
    for row in rows.iter().filter(|r| r.kind != Kind::Link) {
        let to = into.join(&row.path);
        if fs::symlink_metadata(&to).is_ok() {
            seeded.had += 1;
            continue;
        }
        let pinned = pins(&row.path);
        if pinned.is_empty() {
            seeded.unpinned += 1;
            continue;
        }
        let from = dir.join(&row.path);
        match put(&from, &to, row.kind, pinned) {
            Ok(Put::Linked) => {
                seeded.copied += 1;
                seeded.linked += 1;
            }
            Ok(Put::Copied) => seeded.copied += 1,
            Ok(Put::There) => seeded.had += 1,
            Ok(Put::Refused) => seeded.refused += 1,
            Err(err) => {
                seeded.error = Some(format!("{}: {err}", to.display()));
                return seeded;
            }
        }
    }
    // A file left out may be pinned by the next lockfile, and fixed in the next store: no stamp.
    if seeded.refused == 0 && seeded.unpinned == 0 {
        let _ = fs::create_dir_all(into).and_then(|()| fs::File::create(into.join(stamp)));
    }
    seeded
}

/// What [`put`] did with one file.
enum Put {
    Linked,
    Copied,
    /// Another writer put a file there first: left as it is.
    There,
    /// Its bytes aren't any `pinned`: nothing was put.
    Refused,
}

/// Puts `from` at `to` through a file beside it: a link, else a copy, hashed as it is made, and
/// moved in only when its SHA-256 is one of `pinned` and `to` is still free.
fn put(from: &Path, to: &Path, kind: Kind, pinned: &[String]) -> io::Result<Put> {
    let parent = to
        .parent()
        .ok_or_else(|| io::Error::other("no folder above it"))?;
    fs::create_dir_all(parent)?;
    let mut name = std::ffi::OsString::from(".");
    name.push(to.file_name().unwrap_or_default());
    name.push(format!(".{}.hushrun", std::process::id()));
    let temp = parent.join(name);
    let _ = fs::remove_file(&temp);
    let put = (|| {
        let (linked, sha) = match fs::hard_link(from, &temp) {
            Ok(()) => (true, crate::store::sha256_file(&temp)?),
            Err(_) => (false, copy_hashed(from, &temp, kind)?),
        };
        if !pinned.contains(&sha.to_string()) {
            return Ok(Put::Refused);
        }
        // A link, not a rename: it never replaces a file another writer put there meanwhile.
        match fs::hard_link(&temp, to) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Ok(Put::There),
            Err(_) if fs::symlink_metadata(to).is_err() => fs::rename(&temp, to)?,
            Err(_) => return Ok(Put::There),
        }
        Ok(if linked { Put::Linked } else { Put::Copied })
    })();
    let _ = fs::remove_file(&temp);
    put
}

/// Copies `from` to the new file `to`, `0644` or `0755` for `exe`, and returns the SHA-256 of
/// the bytes it wrote.
fn copy_hashed(from: &Path, to: &Path, kind: Kind) -> io::Result<Hex32> {
    let mut source = fs::File::open(from)?;
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = source.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = if kind == Kind::Exe { 0o755 } else { 0o644 };
        out.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = kind;
    Ok(Hex32(hasher.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, bytes: &[u8]) -> Row {
        Row {
            path: path.to_string(),
            kind: Kind::File,
            bytes: bytes.len() as u64,
            sha256: Hex32::sha256(bytes),
            target: None,
            package: None,
            locked: None,
        }
    }

    // covers: K-10
    #[test]
    fn a_seed_puts_what_the_lockfile_pins_never_over_a_file_and_stamps_once() {
        let t = tempfile::tempdir().unwrap();
        let (part, cache) = (t.path().join("part"), t.path().join("cache"));
        let files: [(&str, &[u8]); 4] = [
            ("p/a-1.tar", b"aaa"),
            ("p/b-1.tar", b"bbb"),
            ("p/c-1.tar", b"ccc"),
            ("p/old-1.tar", b"old"),
        ];
        let rows: Vec<Row> = files.iter().map(|(p, b)| row(p, b)).collect();
        for (path, bytes) in files {
            fs::create_dir_all(part.join(path).parent().unwrap()).unwrap();
            fs::write(part.join(path), bytes).unwrap();
        }
        // b is in the cache already, with other bytes; c's file changed after the bake.
        fs::create_dir_all(cache.join("p")).unwrap();
        fs::write(cache.join("p/b-1.tar"), b"mine").unwrap();
        fs::write(part.join("p/c-1.tar"), b"cCc").unwrap();
        let pinned = |bytes: &[u8]| vec![Hex32::sha256(bytes).to_string()];
        let mut locks = crate::part::Locks::new();
        locks.insert("p/a-1.tar".into(), pinned(b"aaa"));
        locks.insert("p/b-1.tar".into(), pinned(b"bbb"));
        locks.insert("p/c-1.tar".into(), pinned(b"ccc"));
        let pins = |path: &str| locks.get(path).map_or(&[][..], Vec::as_slice);
        let stamp = stamp_name(&Hex32::sha256(b"store"));
        let done = seed(&part, &rows, &cache, &stamp, &pins);
        assert_eq!(
            (done.copied, done.had, done.refused, done.unpinned),
            (1, 1, 1, 1),
            "{done:?}"
        );
        assert_eq!(fs::read(cache.join("p/a-1.tar")).unwrap(), b"aaa");
        assert_eq!(
            fs::read(cache.join("p/b-1.tar")).unwrap(),
            b"mine",
            "never over a file"
        );
        assert!(
            !cache.join("p/c-1.tar").exists(),
            "a changed file is never put"
        );
        assert!(
            !cache.join("p/old-1.tar").exists(),
            "nor one no lockfile pins"
        );
        let left: Vec<_> = fs::read_dir(cache.join("p"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "no file of the seed's own is left: {left:?}");
        assert!(
            !cache.join(&stamp).exists(),
            "no stamp while a file is left out"
        );
        // The store put right and the lockfile without the old package: every row is in place,
        // and the stamp is left; the seed after it looks at nothing else.
        fs::write(part.join("p/c-1.tar"), b"ccc").unwrap();
        let rows = &rows[..3];
        let done = seed(&part, rows, &cache, &stamp, &pins);
        assert_eq!((done.copied, done.had, done.stamped), (1, 2, false));
        assert!(cache.join(&stamp).is_file());
        fs::remove_file(cache.join("p/a-1.tar")).unwrap();
        let done = seed(&part, rows, &cache, &stamp, &pins);
        assert!(done.stamped);
        assert_eq!(done.copied, 0);
    }

    // covers: K-10
    #[cfg(unix)]
    #[test]
    fn a_seed_links_where_it_can_and_the_link_is_the_stores_file() {
        use std::os::unix::fs::MetadataExt as _;
        let t = tempfile::tempdir().unwrap();
        let (part, cache) = (t.path().join("part"), t.path().join("cache"));
        fs::create_dir_all(&part).unwrap();
        fs::write(part.join("a-1.tar"), b"aaa").unwrap();
        let sha = vec![Hex32::sha256(b"aaa").to_string()];
        let done = seed(&part, &[row("a-1.tar", b"aaa")], &cache, ".s", &|_| &sha);
        assert_eq!((done.copied, done.linked), (1, 1), "{done:?}");
        let (a, b) = (
            fs::metadata(part.join("a-1.tar")).unwrap(),
            fs::metadata(cache.join("a-1.tar")).unwrap(),
        );
        assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
    }
}
