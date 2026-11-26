//! K-45: a seed (K-10's C). A run links, or else copies, each file the lockfile in the checkout
//! names from a part into the tool's own cache, before the tool runs: never over a file already
//! there, never a file whose bytes aren't its row's, and, for a part a lockfile verifies, never
//! one the lockfile doesn't hash to those very bytes. A stamp in the cache folder, named for the
//! store and holding a digest of what was seeded, lets the next seed of the same files stop at
//! one read.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use hushrun_core::blocks::Hex32;
use sha2_hw::{Digest as _, Sha256};

use crate::part::Seed;
use crate::store::{Kind, Row, VerifiedBy};

/// What a seed did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Seeded {
    /// Files copied into the tool's cache.
    pub copied: usize,
    /// Files the tool's cache held already: left as they are.
    pub present: usize,
    /// Files the lockfile names that the part lacks: the tool downloads them.
    pub lacking: usize,
    /// Files not seeded, and why, one line each.
    pub refused: Vec<String>,
    /// The stamp said these very files were seeded already: nothing was looked at.
    pub stamped: bool,
}

impl Seeded {
    /// What a run says: nothing when every file it could seed was seeded.
    pub fn lines(&self, part: &str) -> Vec<String> {
        match self.refused.as_slice() {
            [] => Vec::new(),
            [one] => vec![format!("packages: {part}: not seeded: {one}")],
            [first, rest @ ..] => vec![format!(
                "packages: {part}: {} files not seeded: {first}, and {} more",
                rest.len() + 1,
                rest.len()
            )],
        }
    }
}

/// The stamp's name in the tool's cache folder, for the store whose identity is `store`.
pub fn stamp_name(store: &Hex32) -> String {
    format!(".hushrun-packages-{store}")
}

/// Seeds `seed.files` from the part at `dir`, whose rows are `rows`, into `seed.into`, for the
/// store whose identity is `store`. An error is the tool's cache folder that can't be made or
/// written.
pub fn seed(
    dir: &Path,
    rows: &[Row],
    verified_by: VerifiedBy,
    seed: &Seed,
    store: &Hex32,
) -> io::Result<Seeded> {
    let stamp = seed.into.join(stamp_name(store));
    let wanted = wanted_digest(seed);
    if fs::read_to_string(&stamp).is_ok_and(|had| had.trim() == wanted.to_string()) {
        return Ok(Seeded {
            stamped: true,
            ..Seeded::default()
        });
    }
    let by_path: HashMap<&str, &Row> = rows.iter().map(|r| (r.path.as_str(), r)).collect();
    let mut out = Seeded::default();
    let mut made = false;
    for file in &seed.files {
        let Some(row) = by_path.get(file.path.as_str()) else {
            out.lacking += 1;
            continue;
        };
        let refuse = match file.sha256 {
            _ if row.kind == Kind::Link => Some("it is a link in the store"),
            _ if !is_plain_name(&file.name) => Some("its name isn't one file's"),
            Some(locked) if locked != row.sha256 => {
                Some("the lockfile here hashes other bytes than the store holds")
            }
            None if verified_by == VerifiedBy::Lockfile => Some(
                "the lockfile here has no hash of it, and only the lockfile verifies this part",
            ),
            _ => None,
        };
        if let Some(why) = refuse {
            out.refused.push(format!("{}: {why}", file.name));
            continue;
        }
        let to = seed.into.join(&file.name);
        if fs::symlink_metadata(&to).is_ok() {
            out.present += 1;
            continue;
        }
        if !made {
            fs::create_dir_all(&seed.into)?;
            made = true;
        }
        match copy_checked(&dir.join(&row.path), &seed.into, &file.name, row.sha256)? {
            Copied::Yes => out.copied += 1,
            Copied::There => out.present += 1,
            Copied::OtherBytes => out.refused.push(format!(
                "{}: the store's file isn't the bytes its row hashes",
                file.name
            )),
        }
    }
    // Only a seed that left nothing out is stamped: one that refused a file says so every time.
    if out.refused.is_empty() && (made || seed.into.is_dir()) {
        let temp = seed
            .into
            .join(format!(".stamp.{}.hushrun-seed", std::process::id()));
        if fs::write(&temp, format!("{wanted}\n")).is_ok() {
            let _ = fs::rename(&temp, &stamp);
        }
    }
    Ok(out)
}

/// What a seed was asked for, as one digest: each file's row path, name and the lockfile's hash.
fn wanted_digest(seed: &Seed) -> Hex32 {
    let mut hasher = Sha256::new();
    hasher.update(b"hushrun.packages-seed/v1\0");
    for file in &seed.files {
        hasher.update(file.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(file.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(
            file.sha256
                .map(|h| h.to_string())
                .unwrap_or_default()
                .as_bytes(),
        );
        hasher.update(b"\0");
    }
    Hex32(hasher.finalize().into())
}

enum Copied {
    Yes,
    There,
    OtherBytes,
}

/// Puts `from` at `into/name` when its SHA-256 is `want`: a hard link to the store's file where
/// the file system allows one, else a copy through a file of its own in `into`, hashed as it is
/// written. Either way it is linked into place, so a file another process put there first stays.
fn copy_checked(from: &Path, into: &Path, name: &str, want: Hex32) -> io::Result<Copied> {
    if crate::store::sha256_file(from)? != want {
        return Ok(Copied::OtherBytes);
    }
    let to = into.join(name);
    match fs::hard_link(from, &to) {
        Ok(()) => return Ok(Copied::Yes),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(Copied::There),
        // Another file system, a read-only store, a file of another owner: copy it.
        Err(_) => {}
    }
    let temp = into.join(format!(".{name}.{}.hushrun-seed", std::process::id()));
    let written = (|| -> io::Result<Hex32> {
        let mut src = fs::File::open(from)?;
        let mut dst = fs::File::create(&temp)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            dst.write_all(&buf[..n])?;
        }
        dst.flush()?;
        Ok(Hex32(hasher.finalize().into()))
    })();
    let result = match written {
        Ok(got) if got == want => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&temp, fs::Permissions::from_mode(0o644))?;
            }
            match fs::hard_link(&temp, &to) {
                Ok(()) => Ok(Copied::Yes),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Copied::There),
                Err(e) => Err(e),
            }
        }
        Ok(_) => Ok(Copied::OtherBytes),
        Err(e) => Err(e),
    };
    let _ = fs::remove_file(&temp);
    result
}

/// One file's name: no separator, not `.` or `..`, no control character.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\') || c.is_control())
}

#[cfg(test)]
mod tests;
