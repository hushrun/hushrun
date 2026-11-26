//! K-10, mechanism C: a seed. hushrun copies a part's files into the tool's own cache, each file
//! hashed as it is copied and checked against its row, never over a file that is there, then the
//! tool installs as usual. A stamp in the cache (`.hushrun-packages-<part identity>`) makes the
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
    /// Files copied into the cache.
    pub copied: usize,
    /// Files the cache had already.
    pub there: usize,
    /// Files whose bytes aren't their row's any more: not copied. The tool downloads what they
    /// were of.
    pub changed: Vec<String>,
    /// Files the lockfile in the checkout doesn't name now: not copied.
    pub not_locked: usize,
    /// The stamp was there: nothing was looked at.
    pub stamped: bool,
}

/// The stamp a seed of `read` leaves in `dest`.
pub fn stamp_name(read: &ReadPart) -> String {
    format!(".hushrun-packages-{}", read.entry.identity)
}

/// Copies every file of `read` under its folder `under` into `dest`, at the same path below it.
/// `admit` says, for each file by its path below `under` and its bytes, whether it is copied.
pub fn seed(
    read: &ReadPart,
    under: &str,
    dest: &Path,
    admit: &dyn Fn(&str, &[u8]) -> Admit,
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
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        // Written beside, then renamed: a tool never sees half a file.
        let temp = to.with_file_name(format!(
            ".{}.hushrun-seed",
            to.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ));
        fs::write(&temp, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = if row.kind == Kind::Exe { 0o755 } else { 0o644 };
            fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&temp, &to)?;
        done.copied += 1;
    }
    // A file the lockfile doesn't name may be named by the next one: no stamp then.
    if done.changed.is_empty() && done.not_locked == 0 {
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
        let done = seed(&r, "store", &dest, &all).unwrap();
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
        let done = seed(&r, "store", &dest, &all).unwrap();
        assert_eq!((done.copied, done.there, done.stamped), (1, 2, false));
        assert!(seed(&r, "store", &dest, &all).unwrap().stamped);
    }
}
