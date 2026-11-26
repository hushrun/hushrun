//! K-10, mechanism C: a seed (K-50). hushrun copies a part's files into the tool's own cache, or
//! links them where the part allows it and the file system can, each file hashed as it is put and
//! checked against its row, never over a file that is there, then the tool installs as usual. A stamp in the cache (`.hushrun-packages-<part identity>`) makes the
//! second seed of the same part cost one `stat`.

use std::fs;
use std::io;
use std::path::Path;

use hushrun_core::blocks::Hex32;

use crate::part::Admit;
use crate::state::ReadPart;
use crate::store::Kind;

/// What a seed did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Seeded {
    /// Files put into the cache.
    pub copied: usize,
    /// Of those, how many are hard links to the store's file, not copies.
    pub linked: usize,
    /// Files the cache had already.
    pub there: usize,
    /// Files whose bytes aren't their row's any more: not copied. The tool downloads what they
    /// were of.
    pub changed: Vec<String>,
    /// Files the lockfile in the checkout doesn't name now: not copied.
    pub not_locked: usize,
    /// The stamp was there: nothing was looked at.
    pub stamped: bool,
    /// Files a seed that unpacks couldn't unpack, each with why: not placed. The tool downloads
    /// what they were of.
    pub refused: Vec<(String, String)>,
}

/// The stamp a seed of `read` leaves in `dest`.
pub fn stamp_name(read: &ReadPart) -> String {
    format!(".hushrun-packages-{}", read.entry.identity)
}

/// Copies every file of `read` under its folder `under` into `dest`, at the same path below it.
/// `admit` says, for each file by its path below `under` and its bytes, whether it is copied.
/// With `link`, a file is a hard link to the store's where the file system allows one, and a copy
/// where it doesn't; its bytes are read from the link, so what is checked is what the tool reads.
pub fn seed(
    read: &ReadPart,
    under: &str,
    dest: &Path,
    admit: &dyn Fn(&str, &[u8]) -> Admit,
    link: bool,
) -> io::Result<Seeded> {
    let stamp = dest.join(stamp_name(read));
    if fs::symlink_metadata(&stamp).is_ok() {
        return Ok(Seeded {
            stamped: true,
            ..Seeded::default()
        });
    }
    let prefix = format!("{under}/");
    let mut done = Seeded::default();
    for row in &read.rows {
        let Some(rest) = row.path.strip_prefix(&prefix) else {
            continue;
        };
        if row.kind == Kind::Link {
            continue;
        }
        let to = dest.join(rest);
        if fs::symlink_metadata(&to).is_ok() {
            done.there += 1;
            continue;
        }
        // Written (or linked) beside, then renamed: a tool never sees half a file.
        let temp = to.with_file_name(format!(
            ".{}.hushrun-seed",
            to.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ));
        let from = read.dir.join(&row.path);
        let linked = link && to.parent().is_some_and(|p| fs::create_dir_all(p).is_ok()) && {
            let _ = fs::remove_file(&temp);
            fs::hard_link(&from, &temp).is_ok()
        };
        let bytes = fs::read(if linked { &temp } else { &from })?;
        let verdict = if Hex32::sha256(&bytes) != row.sha256 {
            Admit::Changed
        } else {
            admit(rest, &bytes)
        };
        if verdict != Admit::Copy {
            if linked {
                let _ = fs::remove_file(&temp);
            }
            match verdict {
                Admit::NotLocked => done.not_locked += 1,
                _ => done.changed.push(row.path.clone()),
            }
            continue;
        }
        if !linked {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&temp, &bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = if row.kind == Kind::Exe { 0o755 } else { 0o644 };
                fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
            }
        }
        fs::rename(&temp, &to)?;
        done.copied += 1;
        done.linked += usize::from(linked);
    }
    // A file the lockfile doesn't name may be named by the next one: no stamp then.
    if done.changed.is_empty() && done.not_locked == 0 {
        fs::create_dir_all(dest)?;
        fs::write(&stamp, b"")?;
    }
    Ok(done)
}

/// K-20: a seed that unpacks. Each tarball of `read` under its folder `under` is checked as
/// [`seed`] checks a file (its row's SHA-256, then `admit`), and unpacked into the folder below
/// `dest` that `folder_of` names for its path, never over a folder that is there. A tarball that
/// can't be unpacked is left out, with why. The stamp is [`seed`]'s.
pub fn seed_unpacking(
    read: &ReadPart,
    under: &str,
    dest: &Path,
    admit: &dyn Fn(&str, &[u8]) -> Admit,
    folder_of: &dyn Fn(&str) -> Option<String>,
) -> io::Result<Seeded> {
    let stamp = dest.join(stamp_name(read));
    if fs::symlink_metadata(&stamp).is_ok() {
        return Ok(Seeded {
            stamped: true,
            ..Seeded::default()
        });
    }
    let prefix = format!("{under}/");
    let mut done = Seeded::default();
    for row in &read.rows {
        let Some(rest) = row.path.strip_prefix(&prefix) else {
            continue;
        };
        let Some(folder) = folder_of(rest).filter(|_| row.kind != Kind::Link) else {
            continue;
        };
        let to = dest.join(&folder);
        if fs::symlink_metadata(&to).is_ok() {
            done.there += 1;
            continue;
        }
        let bytes = fs::read(read.dir.join(&row.path))?;
        if Hex32::sha256(&bytes) != row.sha256 {
            done.changed.push(row.path.clone());
            continue;
        }
        match admit(rest, &bytes) {
            Admit::Copy => {}
            Admit::NotLocked => {
                done.not_locked += 1;
                continue;
            }
            Admit::Changed => {
                done.changed.push(row.path.clone());
                continue;
            }
        }
        let parent = to.parent().unwrap_or(dest);
        fs::create_dir_all(parent)?;
        // Unpacked beside, then renamed: a tool never sees half a package.
        let temp = parent.join(format!(
            ".{}.hushrun-seed-{}",
            to.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default(),
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        if let Err(err) = crate::tgz::unpack(&bytes, &temp) {
            let _ = fs::remove_dir_all(&temp);
            done.refused.push((row.path.clone(), err.to_string()));
            continue;
        }
        if fs::rename(&temp, &to).is_err() {
            // Another install put it there meanwhile.
            let _ = fs::remove_dir_all(&temp);
            done.there += 1;
            continue;
        }
        done.copied += 1;
    }
    if done.changed.is_empty() && done.not_locked == 0 && done.refused.is_empty() {
        fs::create_dir_all(dest)?;
        fs::write(&stamp, b"")?;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Class, PartEntry, Row, RowsFile, VerifiedBy};

    fn read(dir: &Path, files: &[(&str, &[u8])]) -> ReadPart {
        let rows = files
            .iter()
            .map(|(path, bytes)| {
                let at = dir.join(path);
                fs::create_dir_all(at.parent().unwrap()).unwrap();
                fs::write(&at, bytes).unwrap();
                Row {
                    path: path.to_string(),
                    kind: Kind::File,
                    bytes: bytes.len() as u64,
                    sha256: Hex32::sha256(bytes),
                    target: None,
                    package: None,
                    locked: None,
                }
            })
            .collect();
        ReadPart {
            entry: PartEntry {
                part: "pnpm".into(),
                class: Class::Download,
                tool: "pnpm".into(),
                runtime: None,
                inputs: Hex32::sha256(b"i"),
                manifests: vec![],
                identity: Hex32::sha256(b"id"),
                rows: RowsFile {
                    sha256: Hex32::sha256(b"r"),
                    bytes: 0,
                    count: 0,
                },
                bytes: 0,
                verified_by: VerifiedBy::Signature,
            },
            rows,
            dir: dir.to_path_buf(),
            behind: false,
        }
    }

    // covers: K-10, K-18
    #[test]
    fn a_seed_copies_what_is_missing_checks_every_byte_and_stamps_once() {
        let all = |_: &str, _: &[u8]| Admit::Copy;
        let t = tempfile::tempdir().unwrap();
        let part = t.path().join("part");
        let dest = t.path().join("cache");
        let r = read(
            &part,
            &[
                ("store/v10/files/aa/one", b"one"),
                ("store/v10/files/bb/two", b"two"),
                ("store/v10/index/cc/x-ms@2.1.3.json", b"{}"),
            ],
        );
        fs::create_dir_all(dest.join("v10/files/bb")).unwrap();
        fs::write(dest.join("v10/files/bb/two"), b"mine").unwrap();
        // One byte of one file flipped after the bake: the seed catches it and copies the rest.
        fs::write(part.join("store/v10/files/aa/one"), b"onE").unwrap();
        let done = seed(&r, "store", &dest, &all, false).unwrap();
        assert_eq!(done.copied, 1);
        assert_eq!(done.there, 1);
        assert_eq!(done.changed, vec!["store/v10/files/aa/one".to_string()]);
        assert!(!dest.join("v10/files/aa/one").exists());
        assert_eq!(fs::read(dest.join("v10/files/bb/two")).unwrap(), b"mine");
        assert!(dest.join("v10/index/cc/x-ms@2.1.3.json").is_file());
        assert!(
            !dest.join(stamp_name(&r)).exists(),
            "no stamp over a changed file"
        );
        // Put right, the next seed copies it and stamps; the one after looks at nothing.
        fs::write(part.join("store/v10/files/aa/one"), b"one").unwrap();
        let done = seed(&r, "store", &dest, &all, false).unwrap();
        assert_eq!((done.copied, done.there, done.stamped), (1, 2, false));
        assert!(seed(&r, "store", &dest, &all, false).unwrap().stamped);
    }

    // covers: K-50
    #[cfg(unix)]
    #[test]
    fn a_seed_that_may_link_links_and_checks_what_it_linked() {
        use std::os::unix::fs::MetadataExt as _;
        let all = |_: &str, _: &[u8]| Admit::Copy;
        let t = tempfile::tempdir().unwrap();
        let part = t.path().join("part");
        let dest = t.path().join("cache");
        let r = read(
            &part,
            &[
                ("packages/hexpm/a-1.tar", b"aaa"),
                ("packages/hexpm/b-1.tar", b"bbb"),
            ],
        );
        fs::write(part.join("packages/hexpm/b-1.tar"), b"bBb").unwrap();
        let done = seed(&r, "packages/hexpm", &dest, &all, true).unwrap();
        assert_eq!(
            (done.copied, done.linked, done.changed.len()),
            (1, 1, 1),
            "{done:?}"
        );
        let (a, b) = (
            fs::metadata(part.join("packages/hexpm/a-1.tar")).unwrap(),
            fs::metadata(dest.join("a-1.tar")).unwrap(),
        );
        assert_eq!(
            (a.dev(), a.ino()),
            (b.dev(), b.ino()),
            "the store's own file"
        );
        assert!(!dest.join("b-1.tar").exists());
        let left: Vec<_> = fs::read_dir(&dest)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left.len(), 1, "no link of a refused file is left: {left:?}");
    }
}
