//! Putting a block's files in place instead of compiling (B-12).
//!
//! Everything that can be checked is checked before the output folder is touched: the block
//! serves this call's form, every placeholder resolves under this call's roots, the dep-info can
//! be written so cargo reads it back. Then each file is copied (a clone where the file system
//! can) under a temporary name that doesn't begin with `lib`, checked against its SHA-256, made
//! writable, dated with the wrapper's start, and renamed into place: a new file, never one
//! written in place. A program's debug bundle (B-9) is copied whole into a temporary folder and
//! that folder is renamed into place, so `<program>.dSYM` is never there half made. The bundle
//! goes first, then the files, the dep-info last. The messages are rendered up front and handed
//! back; the caller writes them only when every file is in place.

use std::cell::Cell;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hushrun_core::Digest;

use crate::depinfo;
use crate::description::{Description, OutputKind};
use crate::invocation::Invocation;
use crate::key::PlaceKind;
use crate::roots::Roots;
use crate::sidecar;
use crate::store::Block;

/// Why a block can't be put in place. Each ends in rustc running as asked (B-19).
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// The block's compile wrote messages in another form, and not only notices (B-12).
    #[error("the block's messages are in another form")]
    Form,
    /// A placeholder names a root this call doesn't have.
    #[error("a placeholder names no root of this call: {0}")]
    Placeholder(String),
    /// The dep-info can't be written so that cargo reads it back.
    #[error("the dep-info can't be written for this folder: {0}")]
    DepInfo(String),
    /// A file of the block is not one this call writes.
    #[error("{0:?} is not an output of this call")]
    Name(String),
    /// A copy is not the bytes the block recorded.
    #[error("{0} is not the block's bytes")]
    Damaged(PathBuf),
    /// Copying, dating or renaming a file failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
}

fn at(path: &Path) -> impl FnOnce(io::Error) -> RestoreError + '_ {
    move |source| RestoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// What a reuse needs.
pub struct Restore<'a> {
    /// The block found.
    pub block: &'a Block,
    /// Its description.
    pub description: &'a Description,
    /// The call.
    pub invocation: &'a Invocation,
    /// Its roots.
    pub roots: &'a Roots,
    /// The wrapper's start: every placed file's modification time.
    pub start: SystemTime,
    /// This process's id, for temporary names no other process uses.
    pub pid: u32,
    /// The store's seal, which each placed file's record is sealed with (B-6).
    pub seal: Option<crate::sidecar::Seal>,
}

/// A block in place.
#[derive(Debug)]
pub struct Restored {
    /// The messages to write to stderr, with this folder's paths, the notices last.
    pub messages: Vec<u8>,
    /// The files placed, the dep-info last.
    pub files: Vec<PathBuf>,
}

/// The block's files copied beside the outputs under temporary names, ready to be renamed into
/// place. Nothing at an output's name has changed yet: a process killed now leaves only hidden
/// temporary files, which neither rustc nor cargo reads.
#[derive(Debug)]
pub struct Prepared {
    ready: Vec<Ready>,
    messages: Vec<u8>,
    pid: u32,
    origin: Option<Digest>,
    step: Digest,
    /// The block's key, which each file's record names.
    block: Digest,
    /// Whether another machine names the block's files as this one does.
    portable: bool,
    seal: Option<crate::sidecar::Seal>,
}

/// A file, or a program's whole debug bundle, ready to be renamed into place.
#[derive(Debug)]
struct Ready {
    temp: PathBuf,
    dest: PathBuf,
    /// The file's SHA-256, or `None` for the bundle's folder, whose files were each checked.
    sha256: Option<Digest>,
    /// For a linked library, the digest of its metadata the block recorded from its bytes.
    metadata: Option<Digest>,
}

/// Whether `name` is one plain file name of this call's outputs: no folder, not hidden, holding
/// the call's file stem.
fn plain(name: &str, stem: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && name.contains(stem)
}

/// The path inside the bundle of a bundle file named `<stem>.dSYM/<rest>`, when `rest` is plain
/// parts: none empty, `.` or `..`, no `\\`. `None` for any other name.
fn in_bundle<'n>(name: &'n str, stem: &str) -> Option<&'n str> {
    let rest = name.strip_prefix(stem)?.strip_prefix(".dSYM/")?;
    let plain = !rest.contains('\\')
        && rest
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    plain.then_some(rest)
}

/// Removes a file or a folder at `path`: a folder is renamed to a hidden name in `dir` first
/// and removed there, so nobody sees it half removed. Gone already is fine.
pub(crate) fn remove_aside(path: &Path, dir: &Path, pid: u32) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if !meta.is_dir() {
        return fs::remove_file(path);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let aside = dir.join(format!(".hushrun-old.{pid}.{name}"));
    let _ = fs::remove_dir_all(&aside);
    fs::rename(path, &aside)?;
    fs::remove_dir_all(&aside)
}

impl Restore<'_> {
    /// Puts the block in place: [`Restore::prepare`], then [`Prepared::place`].
    pub fn run(&self) -> Result<Restored, RestoreError> {
        self.prepare()?.place()
    }

    /// Checks everything that can be checked and copies each file under a temporary name. On an
    /// error the temporary files are removed and nothing at an output's name changed.
    pub fn prepare(&self) -> Result<Prepared, RestoreError> {
        let description = self.description;
        let form = self.invocation.form();
        if description.messages.form() != form && !description.messages.only_notices() {
            return Err(RestoreError::Form);
        }
        let unresolved = Cell::new(None::<String>);
        let resolve = |text: &str| match self.roots.from_placeholders(text) {
            Ok(text) => text,
            Err(err) => {
                unresolved.set(Some(err.to_string()));
                text.to_string()
            }
        };
        let mut messages = Vec::new();
        description
            .messages
            .replay(resolve, &mut messages)
            .map_err(|source| RestoreError::Io {
                path: PathBuf::from("<messages>"),
                source,
            })?;
        if let Some(err) = unresolved.take() {
            return Err(RestoreError::Placeholder(err));
        }
        let info = description
            .dep_info
            .render(|text| self.roots.from_placeholders(text))
            .map_err(|err| RestoreError::Placeholder(err.to_string()))?;
        info.check()
            .map_err(|err| RestoreError::DepInfo(err.to_string()))?;
        let dep_text = depinfo::render(&info);

        let stem = self.invocation.file_stem();
        let mut names: Vec<&str> = description
            .outputs
            .iter()
            .map(|o| o.name.as_str())
            .collect();
        let mut held: Vec<&str> = self.block.files.iter().map(|f| f.name.as_str()).collect();
        names.sort_unstable();
        held.sort_unstable();
        if names != held {
            return Err(RestoreError::Name(held.join(",")));
        }
        // A bundle file lies in the call's bundle; every other file directly in the folder.
        let bundled = |name: &str| {
            description
                .outputs
                .iter()
                .any(|o| o.name == name && o.kind == OutputKind::DebugBundle)
        };
        if let Some(bad) = held.iter().find(|name| match bundled(name) {
            true => in_bundle(name, &stem).is_none(),
            false => !plain(name, &stem),
        }) {
            return Err(RestoreError::Name((*bad).to_string()));
        }

        let out_dir = {
            let dir = Path::new(self.invocation.out_dir());
            if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                Path::new(self.invocation.cwd()).join(dir)
            }
        };
        let dep_dest = self.invocation.dep_info_path();

        let mut ready: Vec<Ready> = Vec::with_capacity(self.block.files.len() + 1);
        let prepared = (|| -> Result<(), RestoreError> {
            // The bundle first, whole, in a folder of its own.
            let bundle: Vec<_> = self
                .block
                .files
                .iter()
                .filter(|file| bundled(&file.name))
                .collect();
            if !bundle.is_empty() {
                let name = format!("{stem}.dSYM");
                let temp = out_dir.join(format!(".hushrun-restore.{}.b.{name}", self.pid));
                let _ = fs::remove_dir_all(&temp);
                fs::create_dir(&temp).map_err(at(&temp))?;
                ready.push(Ready {
                    temp: temp.clone(),
                    dest: out_dir.join(&name),
                    sha256: None,
                    metadata: None,
                });
                for file in bundle {
                    let rest = in_bundle(&file.name, &stem)
                        .ok_or_else(|| RestoreError::Name(file.name.clone()))?;
                    let copy = temp.join(rest);
                    if let Some(parent) = copy.parent() {
                        fs::create_dir_all(parent).map_err(at(parent))?;
                    }
                    copy_checked(&file.path, &copy, file.sha256)?;
                    finish_file(&copy, self.start)?;
                }
            }
            for (n, file) in self.block.files.iter().enumerate() {
                if bundled(&file.name) {
                    continue;
                }
                let temp = out_dir.join(format!(".hushrun-restore.{}.{n}.{}", self.pid, file.name));
                let _ = fs::remove_file(&temp);
                ready.push(Ready {
                    temp: temp.clone(),
                    dest: out_dir.join(&file.name),
                    sha256: Some(file.sha256),
                    metadata: description
                        .outputs
                        .iter()
                        .find(|o| o.name == file.name)
                        .and_then(|o| o.metadata),
                });
                copy_checked(&file.path, &temp, file.sha256)?;
                finish_file(&temp, self.start)?;
            }
            let temp = out_dir.join(format!(".hushrun-restore.{}.d.tmp", self.pid));
            fs::write(&temp, dep_text.as_bytes()).map_err(at(&temp))?;
            ready.push(Ready {
                temp: temp.clone(),
                dest: dep_dest.clone(),
                sha256: Some(Digest::of(dep_text.as_bytes())),
                metadata: None,
            });
            finish_file(&temp, self.start)
        })();
        if let Err(err) = prepared {
            for file in &ready {
                let _ = remove_path(&file.temp);
            }
            return Err(err);
        }
        Ok(Prepared {
            ready,
            messages,
            pid: self.pid,
            // Only a block that may move is known to dependents by its origin (B-6).
            origin: (description.place == PlaceKind::Anywhere).then_some(description.origin),
            step: description.step,
            block: self.block.key,
            portable: description.portability.named_portably(description.class),
            seal: self.seal,
        })
    }
}

impl Prepared {
    /// The temporary files, and the bundle's temporary folder, in the order they are renamed.
    pub fn temporary_files(&self) -> Vec<PathBuf> {
        self.ready.iter().map(|file| file.temp.clone()).collect()
    }

    /// Renames the first `n` of [`Prepared::temporary_files`] into place and stops, as a
    /// process killed then would: no record is written and nothing is cleaned up.
    #[cfg(test)]
    pub(crate) fn place_first(&self, n: usize) -> Result<(), RestoreError> {
        for file in self.ready.iter().take(n) {
            file.rename(self.pid)?;
        }
        Ok(())
    }

    /// Renames each file into place and records it as reused (B-12, B-15). On an error the
    /// files renamed so far are removed again, so the compile that follows writes every one.
    pub fn place(self) -> Result<Restored, RestoreError> {
        let Prepared {
            ready,
            messages,
            pid,
            origin,
            step,
            block,
            portable,
            seal,
        } = self;
        let mut placed: Vec<PathBuf> = Vec::with_capacity(ready.len());
        let renamed = (|| -> Result<(), RestoreError> {
            for file in &ready {
                file.rename(pid)?;
                placed.push(file.dest.clone());
            }
            for file in &ready {
                // A bundle is no dependent's input: no record, and none written inside it.
                let Some(sha256) = file.sha256 else { continue };
                let produced_by = origin.and_then(|known| {
                    crate::metadata::produced_by(known, &file.dest, file.metadata)
                });
                sidecar::record_reused(
                    &file.dest,
                    sha256,
                    produced_by,
                    step,
                    block,
                    portable,
                    seal.as_ref(),
                )
                .map_err(at(&file.dest))?;
            }
            Ok(())
        })();
        if let Err(err) = renamed {
            for file in &ready {
                let _ = remove_path(&file.temp);
            }
            for path in &placed {
                let _ = remove_path(path);
                let _ = sidecar::forget(path);
            }
            return Err(err);
        }
        Ok(Restored {
            messages,
            files: placed,
        })
    }
}

impl Ready {
    /// One rename into place. A bundle that is there already (an earlier compile's) is moved
    /// aside and removed first: a folder can't be renamed over one that holds files.
    fn rename(&self, pid: u32) -> Result<(), RestoreError> {
        if self.sha256.is_none()
            && let Some(dir) = self.dest.parent()
        {
            remove_aside(&self.dest, dir, pid).map_err(at(&self.dest))?;
        }
        fs::rename(&self.temp, &self.dest).map_err(at(&self.dest))
    }
}

/// Copies a stored file to `to` (a clone where the file system can) and checks it is the
/// block's bytes.
fn copy_checked(from: &Path, to: &Path, sha256: Digest) -> Result<(), RestoreError> {
    fs::copy(from, to).map_err(at(to))?;
    let (sha, _) = hushrun_digest::hash::hash_file(to).map_err(at(to))?;
    if Digest::from_bytes(sha) != sha256 {
        return Err(RestoreError::Damaged(from.to_path_buf()));
    }
    Ok(())
}

/// Removes a file or a folder and everything in it. Gone already is fine.
fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Makes a placed file the owner's to write and dates it `start` (B-12).
fn finish_file(path: &Path, start: SystemTime) -> Result<(), RestoreError> {
    let meta = fs::metadata(path).map_err(at(path))?;
    let mut perms = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() | 0o200);
    }
    // Elsewhere this clears the read-only attribute, which is all the owner needs to write.
    #[cfg(not(unix))]
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms).map_err(at(path))?;
    File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(start))
        .map_err(at(path))
}
