//! Putting a block's files in place instead of compiling (B-12).
//!
//! Everything that can be checked is checked before the output folder is touched: the block
//! serves this call's form, every placeholder resolves under this call's roots, the dep-info can
//! be written so cargo reads it back. Then each file is copied (a clone where the file system
//! can) under a temporary name that doesn't begin with `lib`, checked against its SHA-256, made
//! writable, dated with the wrapper's start, and renamed into place: a new file, never one
//! written in place. The messages are rendered up front and handed back; the caller writes them
//! only when every file is in place.

use std::cell::Cell;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hushrun_core::Digest;

use crate::depinfo;
use crate::description::Description;
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
    origin: Option<Digest>,
    step: Digest,
}

/// A file ready to be renamed into place.
#[derive(Debug)]
struct Ready {
    temp: PathBuf,
    dest: PathBuf,
    sha256: Digest,
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
        if let Some(bad) = held.iter().find(|name| !plain(name, &stem)) {
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
            for (n, file) in self.block.files.iter().enumerate() {
                let temp = out_dir.join(format!(".hushrun-restore.{}.{n}.{}", self.pid, file.name));
                let _ = fs::remove_file(&temp);
                fs::copy(&file.path, &temp).map_err(at(&temp))?;
                ready.push(Ready {
                    temp: temp.clone(),
                    dest: out_dir.join(&file.name),
                    sha256: file.sha256,
                });
                let (sha, _) = hushrun_digest::hash::hash_file(&temp).map_err(at(&temp))?;
                if Digest::from_bytes(sha) != file.sha256 {
                    return Err(RestoreError::Damaged(file.path.clone()));
                }
                finish_file(&temp, self.start)?;
            }
            let temp = out_dir.join(format!(".hushrun-restore.{}.d.tmp", self.pid));
            fs::write(&temp, dep_text.as_bytes()).map_err(at(&temp))?;
            ready.push(Ready {
                temp: temp.clone(),
                dest: dep_dest.clone(),
                sha256: Digest::of(dep_text.as_bytes()),
            });
            finish_file(&temp, self.start)
        })();
        if let Err(err) = prepared {
            for file in &ready {
                let _ = fs::remove_file(&file.temp);
            }
            return Err(err);
        }
        Ok(Prepared {
            ready,
            messages,
            // Only a block that may move is known to dependents by its origin (B-6).
            origin: (description.place == PlaceKind::Anywhere).then_some(description.origin),
            step: description.step,
        })
    }
}

impl Prepared {
    /// The temporary files, in the order they are renamed.
    pub fn temporary_files(&self) -> Vec<PathBuf> {
        self.ready.iter().map(|file| file.temp.clone()).collect()
    }

    /// Renames each file into place and records it as reused (B-12, B-15). On an error the
    /// files renamed so far are removed again, so the compile that follows writes every one.
    pub fn place(self) -> Result<Restored, RestoreError> {
        let Prepared {
            ready,
            messages,
            origin,
            step,
        } = self;
        let mut placed: Vec<PathBuf> = Vec::with_capacity(ready.len());
        let renamed = (|| -> Result<(), RestoreError> {
            for file in &ready {
                fs::rename(&file.temp, &file.dest).map_err(at(&file.dest))?;
                placed.push(file.dest.clone());
            }
            for file in &ready {
                sidecar::record_block(&file.dest, file.sha256, origin, Some(step), true)
                    .map_err(at(&file.dest))?;
            }
            Ok(())
        })();
        if let Err(err) = renamed {
            for file in &ready {
                let _ = fs::remove_file(&file.temp);
            }
            for path in &placed {
                let _ = fs::remove_file(path);
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

/// Makes a placed file the owner's to write and dates it `start` (B-12).
fn finish_file(path: &Path, start: SystemTime) -> Result<(), RestoreError> {
    let meta = fs::metadata(path).map_err(at(path))?;
    let mut perms = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() | 0o200);
    }
    #[cfg(not(unix))]
    perms.set_readonly(false);
    fs::set_permissions(path, perms).map_err(at(path))?;
    File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(start))
        .map_err(at(path))
}
