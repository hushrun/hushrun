//! K-10, mechanism C: a part a tool can't read in place is copied into the tool's own cache, one
//! file at a time, and only a file the lockfile in the checkout still names, whose bytes in the
//! store are the ones the lockfile hashes. A file already in the cache is left as it is: the tool
//! checks its own cache against the lockfile.

use std::fs;
use std::io;
use std::path::Path;

use crate::part::{Locked, file_name};
use crate::store::{self, Kind, Row};

/// What a seed did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Seeded {
    /// Files copied into the cache.
    pub copied: usize,
    /// Files the cache held already.
    pub present: usize,
    /// Files the lockfile names no more, left out.
    pub unnamed: usize,
    /// Files whose bytes in the store aren't the ones the lockfile hashes, left out: their paths.
    pub refused: Vec<String>,
}

/// Copies each row of the part at `dir` the lockfile names (`locked`) into `into`, at the row's
/// path, checked against its hash first. A copy is written beside its place and renamed into it,
/// as `0644`, so that a tool never reads half a file.
pub fn seed(dir: &Path, rows: &[Row], locked: &Locked, into: &Path) -> io::Result<Seeded> {
    let mut seeded = Seeded::default();
    for row in rows {
        if row.kind == Kind::Link {
            continue;
        }
        let named = locked.of(file_name(&row.path), &row.sha256);
        if row.locked.is_none() || named != row.locked.as_deref() {
            seeded.unnamed += 1;
            continue;
        }
        let to = into.join(&row.path);
        if fs::symlink_metadata(&to).is_ok() {
            seeded.present += 1;
            continue;
        }
        let from = dir.join(&row.path);
        if store::sha256_file(&from)? != row.sha256 {
            seeded.refused.push(row.path.clone());
            continue;
        }
        let parent = to.parent().unwrap_or(into);
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(
            ".{}.hushrun-{}",
            file_name(&row.path),
            std::process::id()
        ));
        let copied = fs::copy(&from, &temp).and_then(|_| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&temp, fs::Permissions::from_mode(0o644))?;
            }
            fs::rename(&temp, &to)
        });
        if let Err(err) = copied {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        seeded.copied += 1;
    }
    Ok(seeded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hushrun_core::blocks::Hex32;

    fn row(path: &str, bytes: &[u8], locked: Option<&str>) -> Row {
        Row {
            path: path.into(),
            kind: Kind::File,
            bytes: bytes.len() as u64,
            sha256: Hex32::sha256(bytes),
            target: None,
            package: None,
            locked: locked.map(str::to_string),
        }
    }

    // covers: K-10, K-31
    #[test]
    fn a_seed_copies_only_what_the_lockfile_names_with_the_bytes_it_hashes() {
        let part = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let write = |path: &str, bytes: &[u8]| {
            let to = part.path().join(path);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::write(to, bytes).unwrap();
        };
        let (a, b, c, d) = (b"a wheel".as_slice(), b"b wheel", b"c wheel", b"d wheel");
        write("artifacts/aa/a-1.0-py3-none-any.whl", a);
        write("artifacts/bb/b-1.0-py3-none-any.whl", b);
        write("artifacts/cc/c-1.0-py3-none-any.whl", b"c whee!");
        write("artifacts/dd/d-1.0-py3-none-any.whl", d);
        let hex = |bytes: &[u8]| Hex32::sha256(bytes).to_string();
        let mut locked = Locked::default();
        locked.add(&hex(a), Some("a-1.0-py3-none-any.whl"));
        locked.add(&hex(c), Some("c-1.0-py3-none-any.whl"));
        locked.add(&hex(d), Some("d-1.0-py3-none-any.whl"));
        let w = |bytes: &[u8]| Some(format!("sha256:{}", hex(bytes)));
        let rows = vec![
            row("artifacts/aa/a-1.0-py3-none-any.whl", a, w(a).as_deref()),
            // The lockfile names b no more.
            row("artifacts/bb/b-1.0-py3-none-any.whl", b, w(b).as_deref()),
            // c changed in place in the store, its size the same.
            row("artifacts/cc/c-1.0-py3-none-any.whl", c, w(c).as_deref()),
            // d is in the cache already.
            row("artifacts/dd/d-1.0-py3-none-any.whl", d, w(d).as_deref()),
        ];
        fs::create_dir_all(cache.path().join("artifacts/dd")).unwrap();
        fs::write(cache.path().join("artifacts/dd/d-1.0-py3-none-any.whl"), d).unwrap();
        let seeded = seed(part.path(), &rows, &locked, cache.path()).unwrap();
        assert_eq!(
            seeded,
            Seeded {
                copied: 1,
                present: 1,
                unnamed: 1,
                refused: vec!["artifacts/cc/c-1.0-py3-none-any.whl".into()],
            }
        );
        assert_eq!(
            fs::read(cache.path().join("artifacts/aa/a-1.0-py3-none-any.whl")).unwrap(),
            a
        );
        assert!(!cache.path().join("artifacts/bb").exists());
        assert!(!cache.path().join("artifacts/cc").exists());
        // Again: everything there is there already.
        let again = seed(part.path(), &rows, &locked, cache.path()).unwrap();
        assert_eq!((again.copied, again.present), (0, 2));
    }
}
