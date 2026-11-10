//! A ledger in a directory: where `hushrun run` keeps its local results (§7.6), and a backend
//! for tests. No ledger URL names one (ADR 0022).
//!
//! Objects are created atomically: the bytes go to a temporary file in the same directory,
//! which is then hard-linked to the final name (the link fails if the name exists) and
//! removed. Where hard links are unsupported, `create_new` with write and fsync is used.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{Ledger, LedgerError, check_key};

/// Temporary files start with this, and listings skip them.
const TEMP_PREFIX: &str = ".tmp-";

#[derive(Debug, Clone)]
pub struct FileLedger {
    root: PathBuf,
}

impl FileLedger {
    pub fn new(root: impl Into<PathBuf>) -> FileLedger {
        FileLedger { root: root.into() }
    }

    fn path(&self, key: &str) -> Result<PathBuf, LedgerError> {
        check_key(key)?;
        Ok(self.root.join(key))
    }

    /// Reads fail closed when the ledger directory itself is missing: a mistyped path must
    /// not look like an empty ledger (L-5).
    fn require_root(&self) -> Result<(), LedgerError> {
        match fs::metadata(&self.root) {
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(_) => Err(LedgerError::Unavailable(format!(
                "{} is not a directory",
                self.root.display()
            ))),
            Err(err) => Err(io_error(&self.root, err)),
        }
    }
}

fn io_error(path: &Path, err: io::Error) -> LedgerError {
    let what = format!("{}: {err}", path.display());
    match err.kind() {
        io::ErrorKind::NotFound => LedgerError::Unavailable(what),
        io::ErrorKind::PermissionDenied => LedgerError::Denied(what),
        _ => LedgerError::Unavailable(what),
    }
}

fn temp_name() -> String {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).expect("the OS random number generator works");
    format!(
        "{TEMP_PREFIX}{}",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

impl Ledger for FileLedger {
    fn append(&self, key: &str, bytes: &[u8]) -> Result<(), LedgerError> {
        let path = self.path(key)?;
        let dir = path.parent().expect("keys have a parent");
        let temp = dir.join(temp_name());
        let write = || -> io::Result<()> {
            fs::create_dir_all(dir)?;
            #[cfg(test)]
            tests::after_the_directory_is_made(dir);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()
        };
        // A directory left empty may be removed as it is made, as `hushrun run`'s sweep of its
        // local results removes one: once more, it is made again.
        let written = match write() {
            Err(err) if err.kind() == io::ErrorKind::NotFound => write(),
            other => other,
        };
        if let Err(err) = written {
            let _ = fs::remove_file(&temp);
            return Err(io_error(&temp, err));
        }
        let linked = fs::hard_link(&temp, &path);
        let _ = fs::remove_file(&temp);
        match linked {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                // A retried create: identical bytes mean the first attempt succeeded.
                match fs::read(&path) {
                    Ok(existing) if existing == bytes => Ok(()),
                    Ok(_) => Err(LedgerError::AlreadyExists(key.to_string())),
                    Err(err) => Err(io_error(&path, err)),
                }
            }
            Err(_) => {
                // No hard links here: create the object directly.
                let created = (|| -> io::Result<()> {
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                    file.write_all(bytes)?;
                    file.sync_all()
                })();
                match created {
                    Ok(()) => Ok(()),
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => match fs::read(&path)
                    {
                        Ok(existing) if existing == bytes => Ok(()),
                        _ => Err(LedgerError::AlreadyExists(key.to_string())),
                    },
                    Err(err) => Err(io_error(&path, err)),
                }
            }
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, LedgerError> {
        self.require_root()?;
        // Walk from the deepest directory the prefix names, then filter by the full prefix.
        let dir_part = match prefix.rfind('/') {
            Some(i) => &prefix[..i],
            None => "",
        };
        if !dir_part.is_empty() {
            check_key(dir_part)?;
        }
        let start = if dir_part.is_empty() {
            self.root.clone()
        } else {
            self.root.join(dir_part)
        };
        let mut keys = Vec::new();
        let mut pending = vec![(start, dir_part.to_string())];
        while let Some((dir, relative)) = pending.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(io_error(&dir, err)),
            };
            for entry in entries {
                let entry = entry.map_err(|e| io_error(&dir, e))?;
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name.starts_with(TEMP_PREFIX) {
                    continue;
                }
                let key = if relative.is_empty() {
                    name.clone()
                } else {
                    format!("{relative}/{name}")
                };
                let file_type = entry.file_type().map_err(|e| io_error(&entry.path(), e))?;
                if file_type.is_dir() {
                    if key.starts_with(prefix) || prefix.starts_with(&format!("{key}/")) {
                        pending.push((entry.path(), key));
                    }
                } else if key.starts_with(prefix) {
                    keys.push(key);
                }
            }
        }
        keys.sort();
        Ok(keys)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, LedgerError> {
        let path = self.path(key)?;
        fs::read(&path).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                if self.require_root().is_err() {
                    io_error(&self.root, err)
                } else {
                    LedgerError::NotFound(key.to_string())
                }
            }
            _ => io_error(&path, err),
        })
    }

    fn delete(&self, key: &str) -> Result<(), LedgerError> {
        let path = self.path(key)?;
        fs::remove_file(&path).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => LedgerError::NotFound(key.to_string()),
            _ => io_error(&path, err),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// What a test does once a key's directory is made.
    type Then = Box<dyn FnMut(&Path)>;

    thread_local! {
        /// What a test does between `append` making a key's directory and writing into it:
        /// where a sweep's removal of an empty directory lands.
        static THEN: RefCell<Option<Then>> = const { RefCell::new(None) };
    }

    pub(super) fn after_the_directory_is_made(dir: &Path) {
        THEN.with(|then| {
            if let Some(then) = then.borrow_mut().as_mut() {
                then(dir);
            }
        });
    }

    fn then(act: impl FnMut(&Path) + 'static) {
        THEN.with(|then| *then.borrow_mut() = Some(Box::new(act)));
    }

    const KEY: &str = "results/v0/aa/01M73PGM5TQ1JSH70TGA34A0ZN.json";

    // covers: L-3 (ADR 0067: a sweep that removes a key's empty directory as a run writes to it
    // loses no result)
    #[test]
    fn a_directory_removed_as_append_writes_is_made_again() {
        let root = tempfile::tempdir().unwrap();
        let ledger = FileLedger::new(root.path());
        let mut removed = 0;
        then(move |dir| {
            if removed == 0 {
                fs::remove_dir(dir).expect("the empty directory goes");
            }
            removed += 1;
        });
        ledger.append(KEY, b"{}").expect("the result is kept");
        assert_eq!(ledger.get(KEY).unwrap(), b"{}");
        assert_eq!(ledger.list("results/v0/").unwrap(), [KEY]);
    }

    // covers: L-3 (a directory that goes every time is an error the caller hears, with nothing
    // left behind)
    #[test]
    fn a_directory_that_keeps_going_fails_and_leaves_nothing() {
        let root = tempfile::tempdir().unwrap();
        let ledger = FileLedger::new(root.path());
        then(|dir| fs::remove_dir(dir).expect("the empty directory goes"));
        let err = ledger.append(KEY, b"{}").unwrap_err();
        assert!(matches!(err, LedgerError::Unavailable(_)), "{err:?}");
        assert!(!root.path().join("results/v0/aa").exists());
    }
}
