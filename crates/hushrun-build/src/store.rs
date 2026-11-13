//! The store (B-13): blocks, hints and suspensions on disk. The only code that moves a block
//! between writing, stored, expired and gone; every move is a rename.
//!
//! A block's state is the folder it is in, and nothing else:
//!
//! | State   | Folder                      |
//! |---------|-----------------------------|
//! | writing | `tmp/<unique>/`             |
//! | stored  | `blocks/<2 hex>/<key hex>/` |
//! | expired | `trash/<unique>/`           |
//! | gone    | none                        |
//!
//! A stored block's folder holds `block.json` (its description and each file's name, size and
//! SHA-256), `files/<name>` (read-only) and `used`, whose modification time is the block's last
//! use (B-14). A name is one file name, or a path of plain parts inside the block for a file of a
//! folder the block carries (a program's debug bundle, `<program>.dSYM/Contents/...`). Per first key, `steps/<2 hex>/<key hex>/` holds `hints/<SHA-256 of the hint>`,
//! one file per distinct hint dated by its last refresh (B-7), and `suspended`, whose presence
//! is the suspension (B-15). Small files are written whole: a temporary file under `tmp/`, then
//! a rename.
//!
//! Many processes use one store at once with no lock. Each move is one rename, and every step
//! tolerates another process doing the same thing at the same time: a file already gone is fine.

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hushrun_core::Digest;

use crate::origin::Origin;
use serde::{Deserialize, Serialize};

/// The schema name in every `block.json`.
pub const BLOCK_SCHEMA: &str = "hushrun.build-block/v0";
/// The most hints [`Store::hints`] gives for one first key.
pub const MAX_HINTS: usize = 16;
/// A day: the age past which `tmp/` and `trash/` are swept, and how often the sweep runs.
pub const DAY: Duration = Duration::from_secs(24 * 60 * 60);

const BLOCKS: &str = "blocks";
const STEPS: &str = "steps";
pub(crate) const TMP: &str = "tmp";
pub(crate) const TRASH: &str = "trash";
const BLOCK_JSON: &str = "block.json";
const USED: &str = "used";
const FILES: &str = "files";
const HINTS: &str = "hints";
const SUSPENDED: &str = "suspended";
/// Hints downloaded from a shared store, kept apart from the ones compiles here wrote (B-30).
const SHARED_HINTS: &str = "shared-hints";
/// What the last compile here of a first key took, in milliseconds (B-26).
const COST: &str = "cost";
/// A block's record of where it came from (B-32).
pub(crate) const ORIGIN: &str = "origin.json";
/// The lock its rewrites take.
const ORIGIN_LOCK: &str = "origin.lock";
/// The registry of output folders the wrapper wrote unit records in (B-23): one file each.
const OUT_DIRS: &str = "out-dirs";
/// The schema name of an entry of that registry.
const OUT_DIR_SCHEMA: &str = "hushrun.build.out-dir/2";
/// How many times a step is tried when a name is taken or a folder vanished under it.
const ATTEMPTS: usize = 8;

/// Why the store couldn't do what was asked. Every one ends in rustc running as asked (B-19).
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A block's file name is not a plain relative path inside the block, appears twice, or
    /// names a folder of another of its files.
    #[error("{name:?} is not a block's file name: {why}")]
    Name {
        /// The name given.
        name: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// Reading or writing a file failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// A block's record couldn't be written.
    #[error("{}: {source}", path.display())]
    Json {
        /// The record's path.
        path: PathBuf,
        /// The error.
        #[source]
        source: serde_json::Error,
    },
}

/// A file to store in a new block: `source` is copied in as `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    /// The name in the block: one file name, or plain parts joined by `/`, none empty, `.` or
    /// `..`.
    pub name: String,
    /// The file to copy.
    pub source: PathBuf,
}

/// What [`Store::publish`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published {
    /// The block is now stored under its key.
    Stored,
    /// A block was stored under the key first. It is left as it was and ours is dropped.
    AlreadyThere,
}

/// A stored block, as [`Store::block`] found it: complete when it was read.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// The key it is stored under.
    pub key: Digest,
    /// What the caller recorded with it.
    pub description: serde_json::Value,
    /// Its files, in the order they were published.
    pub files: Vec<BlockFile>,
}

/// One file of a stored block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockFile {
    /// Its name in the block.
    pub name: String,
    /// Its size in bytes, as recorded and as found.
    pub size: u64,
    /// Its SHA-256, as recorded when it was published.
    pub sha256: Digest,
    /// Where it is. Read-only: copy it, don't change it.
    pub path: PathBuf,
}

/// What the store holds (B-21's `hushrun build status`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Status {
    /// How many blocks are stored.
    pub blocks: u64,
    /// Their files' apparent size, in bytes.
    pub bytes: u64,
    /// Blocks last used less than a day ago.
    pub used_today: u64,
    /// Blocks last used one to seven days ago.
    pub used_this_week: u64,
    /// Blocks last used more than seven days ago.
    pub used_before: u64,
    /// Folders under `blocks/` whose record can't be read. They are not counted above.
    pub damaged: u64,
    /// Blocks and bytes by the repository that built them, the most bytes first.
    pub by_repository: Vec<RepositoryUsage>,
}

/// The blocks one repository built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryUsage {
    /// The description's top-level `"repository"`, when it has one.
    pub repository: Option<String>,
    /// How many blocks.
    pub blocks: u64,
    /// Their files' apparent size, in bytes.
    pub bytes: u64,
}

/// What one step of a sweep did: how many things it removed and how many errors it met.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// How many things it removed or moved.
    pub removed: u64,
    /// How many errors it met.
    pub errors: u64,
}

/// `block.json`, as read.
#[derive(Deserialize)]
struct Record {
    schema: String,
    key: Digest,
    description: serde_json::Value,
    files: Vec<RecordFile>,
}

/// `block.json`, as written.
#[derive(Serialize)]
struct RecordOut<'a> {
    schema: &'a str,
    key: &'a Digest,
    description: &'a serde_json::Value,
    files: &'a [RecordFile],
}

#[derive(Serialize, Deserialize)]
struct RecordFile {
    name: String,
    size: u64,
    sha256: Digest,
}

/// An output folder the wrapper wrote unit records in (B-23), as the registry holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutDir {
    /// The output folder (`--out-dir`), resolved.
    pub out_dir: PathBuf,
    /// The working directories of its compiles (the workspaces that share it), resolved.
    pub cwds: Vec<PathBuf>,
    /// When a run last saw a compile there, in milliseconds since the epoch.
    pub seen_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct OutDirFile {
    schema: String,
    #[serde(flatten)]
    entry: OutDir,
}

/// The store's folder (B-13). Cheap to make: nothing is read or written until asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// A store at `root`. No I/O: folders are made when first needed.
    pub fn new(root: &Path) -> Store {
        Store {
            root: root.to_path_buf(),
        }
    }

    /// The store's folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stores a block under `key`: each file is copied into a new folder under `tmp/`, recorded
    /// with its size and SHA-256 beside `description`, made read-only and synced, the last use
    /// is set to `now`, and the folder is renamed to its place. The first writer wins: when a
    /// block is there already it is left untouched and the answer is
    /// [`Published::AlreadyThere`].
    pub fn publish(
        &self,
        key: &Digest,
        description: &serde_json::Value,
        files: &[NewFile],
        now: SystemTime,
    ) -> Result<Published, StoreError> {
        self.publish_inner(key, description, files, None, now, true)
    }

    /// [`Store::publish`], with the block's record of where it came from (B-32) written into its
    /// folder before the rename that publishes it, so no reader ever sees the block without it.
    pub fn publish_with_origin(
        &self,
        key: &Digest,
        description: &serde_json::Value,
        files: &[NewFile],
        origin: &Origin,
        now: SystemTime,
    ) -> Result<Published, StoreError> {
        self.publish_inner(key, description, files, Some(origin), now, true)
    }

    fn publish_inner(
        &self,
        key: &Digest,
        description: &serde_json::Value,
        files: &[NewFile],
        origin: Option<&Origin>,
        now: SystemTime,
        look_first: bool,
    ) -> Result<Published, StoreError> {
        check_names(files)?;
        let dest = self.block_dir(key);
        // Saves the copies when the block is there already. The rename decides either way.
        if look_first && dest.join(BLOCK_JSON).exists() {
            return Ok(Published::AlreadyThere);
        }
        let temp = self.reserve_dir(&key.hex())?;
        let result = write_block(&temp, key, description, files, origin, now)
            .and_then(|()| place(&temp, &dest).map_err(at(&dest)));
        if !matches!(result, Ok(Published::Stored)) {
            let _ = remove_tree(&temp);
        }
        result
    }

    /// The block stored under `key`, complete, or `None`. A block whose record can't be read,
    /// or with a file missing or of another size, is damaged: it is expired and the answer is
    /// `None`, so the caller compiles (B-19).
    pub fn block(&self, key: &Digest) -> Result<Option<Block>, StoreError> {
        self.lookup(key, || {})
    }

    /// [`Store::block`], with `between` called after the record is read and before the files
    /// are checked: where a sweep or a discard may move the folder.
    fn lookup(&self, key: &Digest, mut between: impl FnMut()) -> Result<Option<Block>, StoreError> {
        let dir = self.block_dir(key);
        let json = dir.join(BLOCK_JSON);
        // The record stays open until the check below, so its inode can't be reused meanwhile.
        let (bytes, id, _held) = match read_with_id(&json) {
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // No folder: no block. A folder with no record is damaged, unless a block was
                // published there since.
                return match fs::symlink_metadata(&dir) {
                    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(err) => Err(at(&dir)(err)),
                    Ok(_) => {
                        if !json.exists() {
                            let _ = self.discard(key);
                        }
                        Ok(None)
                    }
                };
            }
            Err(err) => return Err(at(&json)(err)),
        };
        let record = serde_json::from_slice::<Record>(&bytes)
            .ok()
            .filter(|record| record.schema == BLOCK_SCHEMA && record.key == *key);
        between();
        let files = record.as_ref().and_then(|record| {
            let folder = dir.join(FILES);
            record
                .files
                .iter()
                .map(|file| {
                    check_name(&file.name).ok()?;
                    let path = folder.join(&file.name);
                    let meta = fs::symlink_metadata(&path).ok()?;
                    (meta.is_file() && meta.len() == file.size).then(|| BlockFile {
                        name: file.name.clone(),
                        size: file.size,
                        sha256: file.sha256,
                        path,
                    })
                })
                .collect::<Option<Vec<_>>>()
        });
        // The folder may have moved while it was read, and another block taken its place: then
        // what was read describes neither, and the caller compiles.
        let same = fs::symlink_metadata(&json).is_ok_and(|meta| file_id(&meta) == id);
        if !same {
            return Ok(None);
        }
        match (record, files) {
            (Some(record), Some(files)) => Ok(Some(Block {
                key: *key,
                description: record.description,
                files,
            })),
            _ => {
                let _ = self.discard(key);
                Ok(None)
            }
        }
    }

    /// The record of where the block under `key` came from (B-32), or `None`: no block, a block
    /// from before the record existed, or a record that can't be read. A block with none is
    /// reused here as before and never goes up.
    pub fn origin(&self, key: &Digest) -> Option<Origin> {
        read_origin(&self.block_dir(key).join(ORIGIN))
    }

    /// Changes the record of where the block under `key` came from, under the block's lock:
    /// `change` gets the record as it is now and returns the one to write, or `None` to leave it.
    /// The record is written whole (a temporary file, then a rename). Whether a record was
    /// written; an error when there is no block.
    pub fn update_origin(
        &self,
        key: &Digest,
        change: impl FnOnce(Option<Origin>) -> Option<Origin>,
    ) -> Result<bool, StoreError> {
        let dir = self.block_dir(key);
        let _lock = lock_block(&dir).map_err(at(&dir))?;
        let path = dir.join(ORIGIN);
        let Some(next) = change(read_origin(&path)) else {
            return Ok(false);
        };
        let bytes = serde_json::to_vec(&next).map_err(|source| StoreError::Json {
            path: path.clone(),
            source,
        })?;
        self.write_whole(&path, &bytes, None, true)
            .map_err(at(&path))?;
        Ok(true)
    }

    /// Puts a new block under `key` in place of the one there, when `may` says the one there may
    /// be replaced (B-32: a block that was only downloaded may give way to this run's download
    /// or compile; a block built here never does). Decided under the block's lock, with the
    /// record read under it. With no block there it is a plain publish.
    pub fn replace(
        &self,
        key: &Digest,
        description: &serde_json::Value,
        files: &[NewFile],
        origin: &Origin,
        now: SystemTime,
        may: impl FnOnce(Option<&Origin>) -> bool,
    ) -> Result<Replaced, StoreError> {
        check_names(files)?;
        let dest = self.block_dir(key);
        if !dest.join(BLOCK_JSON).exists() {
            return match self.publish_inner(key, description, files, Some(origin), now, false)? {
                Published::Stored => Ok(Replaced::Stored),
                Published::AlreadyThere => Ok(Replaced::Kept),
            };
        }
        let lock = lock_block(&dest).map_err(at(&dest))?;
        if !may(read_origin(&dest.join(ORIGIN)).as_ref()) {
            return Ok(Replaced::Kept);
        }
        let temp = self.reserve_dir(&key.hex())?;
        if let Err(err) = write_block(&temp, key, description, files, Some(origin), now) {
            let _ = remove_tree(&temp);
            return Err(err);
        }
        // The one there goes to the trash, still under its lock, and ours takes its place. A
        // block another process publishes in between wins, as any first writer does.
        let trashed = self.to_trash(&dest).map_err(at(&dest))?;
        drop(lock);
        let placed = place(&temp, &dest).map_err(at(&dest));
        if !matches!(placed, Ok(Published::Stored)) {
            let _ = remove_tree(&temp);
        }
        if let Some(trashed) = trashed {
            let _ = remove_tree(&trashed);
        }
        match placed? {
            Published::Stored => Ok(Replaced::Stored),
            Published::AlreadyThere => Ok(Replaced::Kept),
        }
    }

    /// Sets the last use of the block under `key` to `now` (B-14). An error when there is no
    /// such block.
    pub fn used(&self, key: &Digest, now: SystemTime) -> Result<(), StoreError> {
        let path = self.block_dir(key).join(USED);
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .and_then(|file| file.set_modified(now))
            .map_err(at(&path))
    }

    /// The last use of the block under `key`, or `None` when there is no such block or it has
    /// no record of one.
    pub fn last_used(&self, key: &Digest) -> Option<SystemTime> {
        last_use_in(&self.block_dir(key)).ok().flatten()
    }

    /// The hints recorded for the first key `step` (B-7), the most recently refreshed first, at
    /// most [`MAX_HINTS`]. A hint is never trusted: it only says what to hash.
    pub fn hints(&self, step: &Digest) -> Vec<Vec<u8>> {
        newest_files(&self.step_dir(step).join(HINTS))
    }

    /// Every first key the store holds hints, a cost or a suspension for (B-30).
    pub fn steps(&self) -> Vec<Digest> {
        let mut found = Vec::new();
        for shard in subfolders(&self.root.join(STEPS)) {
            for dir in subfolders(&shard) {
                if let Some(step) = dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| Digest::from_hex(name).ok())
                {
                    found.push(step);
                }
            }
        }
        found
    }

    /// Every stored block's key (B-30).
    pub fn keys(&self) -> Vec<Digest> {
        self.stored().into_iter().map(|(key, _)| key).collect()
    }
}

/// The files of `dir`, the most recently dated first, at most [`MAX_HINTS`].
fn newest_files(dir: &Path) -> Vec<Vec<u8>> {
    {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut found: Vec<(SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                meta.is_file()
                    .then(|| Some((meta.modified().ok()?, entry.path())))
                    .flatten()
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        found
            .into_iter()
            .filter_map(|(_, path)| fs::read(path).ok())
            .take(MAX_HINTS)
            .collect()
    }
}

impl Store {
    /// Records `hint` for the first key `step`, dated `now`. A hint recorded already keeps its
    /// bytes and takes the new date.
    pub fn add_hint(&self, step: &Digest, hint: &[u8], now: SystemTime) -> Result<(), StoreError> {
        let path = self.step_dir(step).join(HINTS).join(Digest::of(hint).hex());
        // Named by its SHA-256, an existing hint holds the same bytes: only its date changes.
        match OpenOptions::new().write(true).open(&path) {
            Ok(file) => return file.set_modified(now).map_err(at(&path)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(at(&path)(err)),
        }
        self.write_whole(&path, hint, Some(now), false)
            .map_err(at(&path))
    }

    /// The hints downloaded from a shared store for the first key `step` (B-30), the most
    /// recently written first, at most [`MAX_HINTS`]. They are kept apart from the hints compiles
    /// here wrote, which are tried first and which these never push out.
    pub fn shared_hints(&self, step: &Digest) -> Vec<Vec<u8>> {
        newest_files(&self.step_dir(step).join(SHARED_HINTS))
    }

    /// Records a hint downloaded from a shared store for the first key `step`.
    pub fn add_shared_hint(
        &self,
        step: &Digest,
        hint: &[u8],
        now: SystemTime,
    ) -> Result<(), StoreError> {
        let path = self
            .step_dir(step)
            .join(SHARED_HINTS)
            .join(Digest::of(hint).hex());
        self.write_whole(&path, hint, Some(now), false)
            .map_err(at(&path))
    }

    /// What the last compile here of the first key `step` took, in milliseconds (B-26), when one
    /// was recorded.
    pub fn step_cost(&self, step: &Digest) -> Option<u64> {
        fs::read_to_string(self.step_dir(step).join(COST))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Records what a compile here of the first key `step` took.
    pub fn set_step_cost(&self, step: &Digest, ms: u64, now: SystemTime) -> Result<(), StoreError> {
        let path = self.step_dir(step).join(COST);
        self.write_whole(&path, ms.to_string().as_bytes(), Some(now), false)
            .map_err(at(&path))
    }

    /// Suspends the first key `step` (B-15): no block of it is reused until
    /// [`Store::clean`]. `reason` says why.
    pub fn suspend(&self, step: &Digest, reason: &str, now: SystemTime) -> Result<(), StoreError> {
        let path = self.step_dir(step).join(SUSPENDED);
        self.write_whole(&path, reason.as_bytes(), Some(now), true)
            .map_err(at(&path))
    }

    /// Suspends the first key `step` until `until` (B-15, B-34): what a downloaded block's
    /// difference or rejection causes, which lasts the store's kept days and not until
    /// `hushrun build clean`.
    pub fn suspend_until(
        &self,
        step: &Digest,
        reason: &str,
        until: SystemTime,
        now: SystemTime,
    ) -> Result<(), StoreError> {
        let path = self.step_dir(step).join(SUSPENDED);
        let text = format!("{UNTIL}{}\n{reason}", epoch_ms(until));
        self.write_whole(&path, text.as_bytes(), Some(now), true)
            .map_err(at(&path))
    }

    /// Why the first key `step` is suspended, or `None` when it is not. A suspension that
    /// can't be read counts as one: the caller compiles.
    pub fn suspended(&self, step: &Digest) -> Option<String> {
        self.suspended_at(step, SystemTime::now())
    }

    /// [`Store::suspended`] at `now`: a suspension with an end ([`Store::suspend_until`]) is over
    /// once `now` reaches it.
    pub fn suspended_at(&self, step: &Digest, now: SystemTime) -> Option<String> {
        match fs::read(self.step_dir(step).join(SUSPENDED)) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if let Some(rest) = text.strip_prefix(UNTIL) {
                    let (until, reason) = rest.split_once('\n').unwrap_or((rest, ""));
                    if until
                        .parse::<u64>()
                        .is_ok_and(|until| epoch_ms(now) >= until)
                    {
                        return None;
                    }
                    return Some(reason.to_string());
                }
                Some(text)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => Some(format!("its suspension can't be read: {err}")),
        }
    }

    /// Expires the block under `key`: it is renamed into `trash/` and deleted there. Used when
    /// a dependent rejects it. No block is fine.
    pub fn discard(&self, key: &Digest) -> Result<(), StoreError> {
        let dir = self.block_dir(key);
        match self.to_trash(&dir) {
            Ok(Some(trashed)) => remove_tree(&trashed).map_err(at(&trashed)),
            Ok(None) => Ok(()),
            Err(err) => Err(at(&dir)(err)),
        }
    }

    /// What the store holds at `now`: blocks, bytes, blocks by age of last use, and blocks and
    /// bytes by repository. A store that doesn't exist yet holds nothing.
    pub fn status(&self, now: SystemTime) -> Status {
        let mut status = Status::default();
        let mut by_repository: BTreeMap<Option<String>, (u64, u64)> = BTreeMap::new();
        for (key, dir) in self.stored() {
            let record = fs::read(dir.join(BLOCK_JSON))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
                .filter(|record| record.schema == BLOCK_SCHEMA && record.key == key);
            let Some(record) = record else {
                status.damaged += 1;
                continue;
            };
            let bytes = record
                .files
                .iter()
                .fold(0u64, |sum, file| sum.saturating_add(file.size));
            status.blocks += 1;
            status.bytes = status.bytes.saturating_add(bytes);
            // A last use after `now` is a clock set back: today.
            let age = match last_use_in(&dir) {
                Ok(Some(last)) => now.duration_since(last).unwrap_or_default(),
                _ => Duration::MAX,
            };
            if age < DAY {
                status.used_today += 1;
            } else if age < 7 * DAY {
                status.used_this_week += 1;
            } else {
                status.used_before += 1;
            }
            let repository = record
                .description
                .get("repository")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let entry = by_repository.entry(repository).or_default();
            entry.0 += 1;
            entry.1 = entry.1.saturating_add(bytes);
        }
        status.by_repository = by_repository
            .into_iter()
            .map(|(repository, (blocks, bytes))| RepositoryUsage {
                repository,
                blocks,
                bytes,
            })
            .collect();
        status.by_repository.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then_with(|| a.repository.cmp(&b.repository))
        });
        status
    }

    /// Empties the store (B-21's `hushrun build clean`): blocks, hints and suspensions are
    /// renamed into `trash/` and deleted there. A block being written is left to finish; the
    /// sweep removes what a crash left in `tmp/`.
    pub fn clean(&self) -> Result<(), StoreError> {
        let mut first_error = None;
        for name in [BLOCKS, STEPS] {
            let path = self.root.join(name);
            let failed = match self.to_trash(&path) {
                Ok(Some(trashed)) => remove_tree(&trashed).err().map(|err| at(&trashed)(err)),
                Ok(None) => None,
                Err(err) => Some(at(&path)(err)),
            };
            if let Some(err) = failed {
                first_error.get_or_insert(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Free space on the store's volume, in bytes (B-22). Before the store's folder exists,
    /// its nearest existing parent's volume. `u64::MAX` where it can't be known.
    pub fn free_bytes(&self) -> io::Result<u64> {
        #[cfg(unix)]
        {
            let mut path = self.root.as_path();
            loop {
                let asked = if path.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    path
                };
                match rustix::fs::statvfs(asked) {
                    Ok(stat) => return Ok(stat.f_bavail.saturating_mul(stat.f_frsize)),
                    Err(rustix::io::Errno::NOENT) => match path.parent() {
                        Some(parent) => path = parent,
                        None => return Err(rustix::io::Errno::NOENT.into()),
                    },
                    Err(err) => return Err(err.into()),
                }
            }
        }
        #[cfg(not(unix))]
        {
            Ok(u64::MAX)
        }
    }

    /// Writes `bytes` to `dest` whole: a new file under `tmp/`, dated `modified` when given and
    /// synced when `sync`, then renamed over `dest`.
    pub(crate) fn write_whole(
        &self,
        dest: &Path,
        bytes: &[u8],
        modified: Option<SystemTime>,
        sync: bool,
    ) -> io::Result<()> {
        let label = dest
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (temp, mut file) = self.reserve_file(&label)?;
        let written = (|| {
            file.write_all(bytes)?;
            if let Some(time) = modified {
                file.set_modified(time)?;
            }
            if sync {
                sync_file(&file)?;
            }
            Ok(())
        })();
        drop(file);
        let placed = written.and_then(|()| rename_making_parent(&temp, dest));
        if placed.is_err() {
            let _ = fs::remove_file(&temp);
        }
        placed
    }

    /// Every stored block's key and folder: `blocks/<2 hex>/<key hex>/`. Anything else under
    /// `blocks/` is not ours and is left alone.
    pub(crate) fn stored(&self) -> Vec<(Digest, PathBuf)> {
        let mut found = Vec::new();
        for shard in subfolders(&self.root.join(BLOCKS)) {
            for dir in subfolders(&shard) {
                let key = dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| Digest::from_hex(name).ok());
                if let Some(key) = key
                    && dir == self.block_dir(&key)
                {
                    found.push((key, dir));
                }
            }
        }
        found
    }

    /// Expires the block in `dir` when its last use is before `cutoff`, or it has no record of
    /// one. Whether it was expired: a block another process expired or used meanwhile is not.
    pub(crate) fn expire_unused(&self, dir: &Path, cutoff: SystemTime) -> io::Result<bool> {
        if let Some(last) = last_use_in(dir)?
            && last >= cutoff
        {
            return Ok(false);
        }
        let Some(trashed) = self.to_trash(dir)? else {
            return Ok(false);
        };
        // A wrapper may have used it between the read and the move: it goes back, unless
        // another copy took its place.
        if matches!(last_use_in(&trashed), Ok(Some(last)) if last >= cutoff)
            && rename_making_parent(&trashed, dir).is_ok()
        {
            return Ok(false);
        }
        remove_tree(&trashed)?;
        Ok(true)
    }

    /// Removes the folders under `blocks/` that hold no block any more.
    pub(crate) fn remove_empty_shards(&self) {
        for shard in subfolders(&self.root.join(BLOCKS)) {
            // Only an empty one goes: one a block was just published into stays.
            let _ = fs::remove_dir(&shard);
        }
    }

    /// Remembers an output folder the wrapper wrote a unit record in, and the working directory
    /// of its compiles (B-23): one small file named by a digest of the folder, written whole,
    /// that keeps every working directory seen there (workspaces may share a target folder).
    /// Paths are resolved, so that the check finds them from any form of the folder it runs in.
    pub fn register_out_dir(
        &self,
        out_dir: &Path,
        cwd: &Path,
        now: SystemTime,
    ) -> Result<(), StoreError> {
        let resolve = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let out_dir = resolve(out_dir);
        let name = Digest::of(out_dir.as_os_str().as_encoded_bytes()).hex();
        let dir = self.root.join(OUT_DIRS);
        let dest = dir.join(format!("{}.json", &name[..32]));
        // Two runs adding a working directory at once may keep one: the next run adds it again.
        let mut cwds: Vec<PathBuf> = fs::read(&dest)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<OutDirFile>(&bytes).ok())
            .filter(|file| file.schema == OUT_DIR_SCHEMA && file.entry.out_dir == out_dir)
            .map(|file| file.entry.cwds)
            .unwrap_or_default();
        let cwd = resolve(cwd);
        if !cwds.contains(&cwd) {
            cwds.push(cwd);
            cwds.sort();
        }
        let entry = OutDir {
            out_dir,
            cwds,
            seen_ms: now.duration_since(UNIX_EPOCH).map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            }),
        };
        fs::create_dir_all(&dir).map_err(at(&dir))?;
        let bytes = serde_json::to_vec(&OutDirFile {
            schema: OUT_DIR_SCHEMA.to_string(),
            entry,
        })
        .map_err(|err| StoreError::Io {
            path: dir.clone(),
            source: io::Error::other(err),
        })?;
        let temp = dir.join(format!(".{}.tmp", unique(&name[..16])));
        fs::write(&temp, bytes)
            .and_then(|()| fs::rename(&temp, &dest))
            .map_err(|err| {
                let _ = fs::remove_file(&temp);
                StoreError::Io {
                    path: dest.clone(),
                    source: err,
                }
            })
    }

    /// The output folders the registry holds (B-23). An entry that can't be read is left out.
    pub fn out_dirs(&self) -> Vec<OutDir> {
        let Ok(entries) = fs::read_dir(self.root.join(OUT_DIRS)) else {
            return Vec::new();
        };
        let mut out: Vec<OutDir> = entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
            .filter_map(|entry| {
                let bytes = fs::read(entry.path()).ok()?;
                let file: OutDirFile = serde_json::from_slice(&bytes).ok()?;
                (file.schema == OUT_DIR_SCHEMA).then_some(file.entry)
            })
            .collect();
        out.sort_by(|a, b| a.out_dir.cmp(&b.out_dir));
        out
    }

    /// Drops the registry's entries whose output folder is gone or that no run saw since
    /// `before` (B-23), and entries that can't be read.
    pub(crate) fn remove_stale_out_dirs(&self, before: SystemTime) -> Tally {
        let mut tally = Tally::default();
        let Ok(entries) = fs::read_dir(self.root.join(OUT_DIRS)) else {
            return tally;
        };
        let cutoff = before.duration_since(UNIX_EPOCH).map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        });
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let entry = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<OutDirFile>(&bytes).ok())
                .filter(|file| file.schema == OUT_DIR_SCHEMA)
                .map(|file| file.entry);
            let stale = match entry {
                None => true,
                Some(entry) => entry.seen_ms < cutoff || !entry.out_dir.is_dir(),
            };
            if stale {
                count(&mut tally, fs::remove_file(&path).map(|()| true));
            }
        }
        tally
    }

    /// Removes everything under `tmp/` and `trash/` last changed before `before`: what a crash
    /// left behind.
    pub(crate) fn remove_leftovers(&self, before: SystemTime) -> Tally {
        let mut tally = Tally::default();
        for name in [TMP, TRASH] {
            let Ok(entries) = fs::read_dir(self.root.join(name)) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let old = fs::symlink_metadata(&path)
                    .and_then(|meta| meta.modified())
                    .is_ok_and(|changed| changed < before);
                if !old {
                    continue;
                }
                count(&mut tally, remove_tree(&path).map(|()| true));
            }
        }
        tally
    }

    /// Removes what the compiler memos' folder holds that no run used since `cutoff` (B-14,
    /// B-17): a memo (a memo of an older form is never read again, and one in use is only
    /// computed again), and a toolchain's view without sources whose marker no run dated since.
    /// A view without its marker, and what a maker left (a temporary or moved-aside folder, a
    /// lock), go once they are older than `leftover`. A view goes by a rename into `trash/`, so
    /// no compile is ever given half of one.
    pub(crate) fn remove_unused_compilers(
        &self,
        cutoff: SystemTime,
        leftover: SystemTime,
    ) -> Tally {
        let mut tally = Tally::default();
        let dated = |path: &Path| {
            fs::symlink_metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        };
        let memos = self.root.join(crate::compiler::MEMOS);
        let Ok(entries) = fs::read_dir(&memos) else {
            return tally;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == crate::compiler::SYSROOTS {
                continue;
            }
            let limit = if name.starts_with('.') {
                leftover
            } else {
                cutoff
            };
            if dated(&path).is_some_and(|changed| changed < limit) {
                count(&mut tally, remove_tree(&path).map(|()| true));
            }
        }
        let Ok(entries) = fs::read_dir(memos.join(crate::compiler::SYSROOTS)) else {
            return tally;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let unused = if name.starts_with('.') {
                dated(&path).is_some_and(|changed| changed < leftover)
            } else {
                match dated(&path.join(crate::compiler::VIEW_MARKER)) {
                    Some(used) => used < cutoff,
                    None => dated(&path).is_some_and(|changed| changed < leftover),
                }
            };
            if !unused {
                continue;
            }
            if name.starts_with('.') {
                count(&mut tally, remove_tree(&path).map(|()| true));
            } else {
                count(
                    &mut tally,
                    self.to_trash(&path).and_then(|moved| match moved {
                        Some(moved) => remove_tree(&moved).map(|()| true),
                        None => Ok(false),
                    }),
                );
            }
        }
        tally
    }

    /// Removes the hints last refreshed before `before`, and the folders of first keys left
    /// with no hint and no suspension.
    pub(crate) fn remove_stale_hints(&self, before: SystemTime) -> Tally {
        let mut tally = Tally::default();
        for shard in subfolders(&self.root.join(STEPS)) {
            for step in subfolders(&shard) {
                for folder in [HINTS, SHARED_HINTS] {
                    let hints = step.join(folder);
                    if let Ok(entries) = fs::read_dir(&hints) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            let stale = entry
                                .metadata()
                                .and_then(|meta| meta.modified())
                                .is_ok_and(|refreshed| refreshed < before);
                            if stale {
                                count(&mut tally, fs::remove_file(&path).map(|()| true));
                            }
                        }
                    }
                    // Only empty ones go: a hint written meanwhile keeps them.
                    let _ = fs::remove_dir(&hints);
                }
                // A cost and an ended suspension go with their age as hints do.
                for name in [COST, SUSPENDED] {
                    let path = step.join(name);
                    let stale = fs::metadata(&path)
                        .and_then(|meta| meta.modified())
                        .is_ok_and(|changed| changed < before);
                    let ended = name == COST
                        || fs::read(&path).is_ok_and(|bytes| {
                            String::from_utf8_lossy(&bytes)
                                .strip_prefix(UNTIL)
                                .and_then(|rest| rest.split('\n').next()?.parse::<u64>().ok())
                                .is_some_and(|until| until < epoch_ms(before))
                        });
                    if stale && ended {
                        count(&mut tally, fs::remove_file(&path).map(|()| true));
                    }
                }
                let _ = fs::remove_dir(&step);
            }
            let _ = fs::remove_dir(&shard);
        }
        tally
    }

    fn block_dir(&self, key: &Digest) -> PathBuf {
        sharded(&self.root.join(BLOCKS), key)
    }

    fn step_dir(&self, step: &Digest) -> PathBuf {
        sharded(&self.root.join(STEPS), step)
    }

    /// A new, empty folder under `tmp/`, made by this call alone.
    fn reserve_dir(&self, label: &str) -> Result<PathBuf, StoreError> {
        let tmp = self.root.join(TMP);
        for _ in 0..ATTEMPTS {
            fs::create_dir_all(&tmp).map_err(at(&tmp))?;
            let dir = tmp.join(unique(label));
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(dir),
                // Taken, or `tmp/` removed meanwhile: another name, or make it again.
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
                    ) => {}
                Err(err) => return Err(at(&dir)(err)),
            }
        }
        Err(at(&tmp)(gave_up()))
    }

    /// A new, empty file under `tmp/`, made by this call alone.
    fn reserve_file(&self, label: &str) -> io::Result<(PathBuf, File)> {
        let tmp = self.root.join(TMP);
        for _ in 0..ATTEMPTS {
            fs::create_dir_all(&tmp)?;
            let path = tmp.join(unique(label));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
                    ) => {}
                Err(err) => return Err(err),
            }
        }
        Err(gave_up())
    }

    /// Renames `path` to a new name under `trash/`. `None` when it is gone already.
    fn to_trash(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        let trash = self.root.join(TRASH);
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        for _ in 0..ATTEMPTS {
            fs::create_dir_all(&trash)?;
            let target = trash.join(unique(&label));
            match fs::rename(path, &target) {
                Ok(()) => return Ok(Some(target)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    if fs::symlink_metadata(path)
                        .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
                    {
                        return Ok(None);
                    }
                    // `trash/` was removed meanwhile: it is made again.
                }
                Err(err) if taken(&err) => {}
                Err(err) => return Err(err),
            }
        }
        Err(gave_up())
    }
}

/// Fills the new block folder `dir`: `files/`, `block.json` and `used`, all synced, and the
/// drive's cache flushed once at the end.
fn write_block(
    dir: &Path,
    key: &Digest,
    description: &serde_json::Value,
    files: &[NewFile],
    origin: Option<&Origin>,
    now: SystemTime,
) -> Result<(), StoreError> {
    let folder = dir.join(FILES);
    fs::create_dir(&folder).map_err(at(&folder))?;
    let mut records = Vec::with_capacity(files.len());
    let mut made = std::collections::BTreeSet::new();
    for file in files {
        let path = folder.join(&file.name);
        // The folders of a nested name, each made once and synced with its entries below.
        if let Some(parent) = path.parent()
            && parent != folder
            && made.insert(parent.to_path_buf())
        {
            fs::create_dir_all(parent).map_err(at(parent))?;
        }
        // A copy-on-write clone on APFS.
        fs::copy(&file.source, &path).map_err(at(&file.source))?;
        let (sha256, size) = hushrun_digest::hash::hash_file(&path).map_err(at(&path))?;
        seal(&path).map_err(at(&path))?;
        records.push(RecordFile {
            name: file.name.clone(),
            size,
            sha256: Digest::from_bytes(sha256),
        });
    }
    for parent in made.iter().rev() {
        sync_dir(parent).map_err(at(parent))?;
    }
    sync_dir(&folder).map_err(at(&folder))?;
    let json = dir.join(BLOCK_JSON);
    let bytes = serde_json::to_vec(&RecordOut {
        schema: BLOCK_SCHEMA,
        key,
        description,
        files: &records,
    })
    .map_err(|source| StoreError::Json {
        path: json.clone(),
        source,
    })?;
    (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&json)?;
        file.write_all(&bytes)?;
        drop(file);
        seal(&json)
    })()
    .map_err(at(&json))?;
    if let Some(origin) = origin {
        let path = dir.join(ORIGIN);
        let bytes = serde_json::to_vec(origin).map_err(|source| StoreError::Json {
            path: path.clone(),
            source,
        })?;
        (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(&bytes)?;
            sync_file(&file)
        })()
        .map_err(at(&path))?;
    }
    let used = dir.join(USED);
    (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&used)?;
        file.set_modified(now)?;
        sync_file(&file)
    })()
    .map_err(at(&used))?;
    (|| {
        let folder = File::open(dir)?;
        sync_file(&folder)?;
        flush_drive(&folder)
    })()
    .map_err(at(dir))
}

/// What [`Store::replace`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replaced {
    /// The new block is in place.
    Stored,
    /// The block there stays: it may not be replaced, or another process published first.
    Kept,
}

/// A suspension with an end starts with this, then the end in milliseconds since the epoch.
const UNTIL: &str = "until ";

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// A block's record of where it came from, or `None`.
fn read_origin(path: &Path) -> Option<Origin> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice::<Origin>(&bytes)
        .ok()
        .filter(|origin| origin.schema == crate::origin::ORIGIN_SCHEMA)
}

/// Takes the lock of the block folder `dir`, which every rewrite of its record and every
/// replacement takes. The lock file lives in the folder, so a block moved to the trash takes its
/// lock along: after taking it, the lock is checked to still be the folder's, else taken again.
fn lock_block(dir: &Path) -> io::Result<File> {
    let path = dir.join(ORIGIN_LOCK);
    for _ in 0..ATTEMPTS {
        if !dir.join(BLOCK_JSON).exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no block"));
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        file.lock()?;
        let held = file.metadata()?;
        let there = fs::metadata(&path);
        if there.is_ok_and(|there| file_id(&there) == file_id(&held)) {
            return Ok(file);
        }
    }
    Err(gave_up())
}

/// Renames the finished folder `temp` to `dest`. The first writer wins: a block there already
/// stays, and the answer is [`Published::AlreadyThere`].
fn place(temp: &Path, dest: &Path) -> io::Result<Published> {
    let Some(parent) = dest.parent() else {
        return Err(io::Error::other("a block's folder has a parent"));
    };
    for _ in 0..ATTEMPTS {
        fs::create_dir_all(parent)?;
        match fs::rename(temp, dest) {
            Ok(()) => {
                // The rename is done; this only makes it durable sooner.
                let _ = sync_dir(parent);
                return Ok(Published::Stored);
            }
            // A sweep removed the empty parent between the two steps: it is made again.
            Err(err) if err.kind() == io::ErrorKind::NotFound && temp.exists() => {}
            Err(err) if taken(&err) || dest.exists() => return Ok(Published::AlreadyThere),
            Err(err) => return Err(err),
        }
    }
    Err(gave_up())
}

/// Renames `from` to `to`, making `to`'s parent when a sweep removed it meanwhile.
fn rename_making_parent(from: &Path, to: &Path) -> io::Result<()> {
    let Some(parent) = to.parent() else {
        return fs::rename(from, to);
    };
    for _ in 0..ATTEMPTS {
        fs::create_dir_all(parent)?;
        match fs::rename(from, to) {
            Err(err) if err.kind() == io::ErrorKind::NotFound && from.exists() => {}
            other => return other,
        }
    }
    Err(gave_up())
}

/// The last use recorded in the block folder `dir`, or `None` when it has no `used` file.
fn last_use_in(dir: &Path) -> io::Result<Option<SystemTime>> {
    match fs::metadata(dir.join(USED)) {
        Ok(meta) => meta.modified().map(Some),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Every name passes [`check_name`], none appears twice, and none is a folder of another (`a`
/// and `a/b`), which could not both be files.
fn check_names(files: &[NewFile]) -> Result<(), StoreError> {
    for (i, file) in files.iter().enumerate() {
        check_name(&file.name)?;
        for other in &files[..i] {
            let why = if other.name == file.name {
                Some("it appears twice")
            } else if is_folder_of(&other.name, &file.name) || is_folder_of(&file.name, &other.name)
            {
                Some("it is a folder of another file")
            } else {
                None
            };
            if let Some(why) = why {
                return Err(StoreError::Name {
                    name: file.name.clone(),
                    why,
                });
            }
        }
    }
    Ok(())
}

/// Whether `folder` is a folder on the way to `name`.
fn is_folder_of(folder: &str, name: &str) -> bool {
    name.strip_prefix(folder)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// A block's file name: one file name, or plain parts joined by `/`. No part is empty, `.` or
/// `..`, none holds a `\` or a NUL, and the name doesn't start with `/`: the file lies inside
/// the block's folder, and a reuse puts it inside the output folder.
fn check_name(name: &str) -> Result<(), StoreError> {
    let why = if name.is_empty() {
        Some("it is empty")
    } else if name.starts_with('/') {
        Some("it is absolute")
    } else if name.contains('\\') {
        Some("it has a backslash")
    } else if name.contains('\0') {
        Some("it has a NUL")
    } else if name.split('/').any(|part| part.is_empty()) {
        Some("it has an empty part")
    } else if name.split('/').any(|part| part == "." || part == "..") {
        Some("it names a folder")
    } else {
        None
    };
    match why {
        Some(why) => Err(StoreError::Name {
            name: name.to_string(),
            why,
        }),
        None => Ok(()),
    }
}

/// `<top>/<first 2 hex digits>/<hex>`: a folder never holds more than a few hundred entries
/// per thousand keys.
fn sharded(top: &Path, key: &Digest) -> PathBuf {
    let hex = key.hex();
    top.join(&hex[..2]).join(hex)
}

/// A name nobody else is using: the label, this process and a random number. Callers still
/// create exclusively and try again when it is taken.
fn unique(label: &str) -> String {
    let random = RandomState::new().build_hasher().finish();
    format!("{label}.{}.{random:016x}", std::process::id())
}

/// Makes a stored file read-only and syncs it.
fn seal(path: &Path) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)?;
    sync_file(&File::open(path)?)
}

/// Syncs a folder's entries.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        sync_file(&File::open(dir)?)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Hands a file's or a folder's writes to the drive: `fsync(2)`. On macOS that leaves them in
/// the drive's cache, which [`flush_drive`] empties once per block instead of once per file
/// (std's `sync_all` there is a full flush each time, about 5 ms).
fn sync_file(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::fsync(file).map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        file.sync_all()
    }
}

/// Flushes the drive's cache, so everything synced before survives a power cut. Linux's
/// `fsync(2)` has done so already.
fn flush_drive(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        rustix::fs::fcntl_fullfsync(file).map_err(io::Error::from)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = file;
        Ok(())
    }
}

/// Removes a file or a folder and everything in it. Gone already is fine.
fn remove_tree(path: &Path) -> io::Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) => Err(err),
    };
    match removed {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The folders directly in `dir`, none when it can't be read.
fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

/// A file's bytes and identity, from one open file, and the file.
fn read_with_id(path: &Path) -> io::Result<(Vec<u8>, (u64, u64), File)> {
    let mut file = File::open(path)?;
    let id = file_id(&file.metadata()?);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((bytes, id, file))
}

/// A file's device and inode. Where they aren't known every file looks the same, and the
/// check that a block didn't move while it was read is skipped.
fn file_id(meta: &fs::Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        (0, 0)
    }
}

/// A name already in use: a folder can't be renamed onto a folder that holds anything.
fn taken(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
    )
}

fn gave_up() -> io::Error {
    io::Error::other("another process kept taking the name or removing the folder")
}

/// Counts one removal: done, or gone already, or an error.
fn count(tally: &mut Tally, result: io::Result<bool>) {
    match result {
        Ok(true) => tally.removed += 1,
        Ok(false) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_) => tally.errors += 1,
    }
}

fn at(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    /// Midnight UTC on day `n` after 2026-01-01.
    fn day(n: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_767_225_600 + n * 86_400)
    }

    fn key(n: u32) -> Digest {
        Digest::of(&n.to_le_bytes())
    }

    struct World {
        _dir: tempfile::TempDir,
        sources: PathBuf,
        store: Store,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let sources = dir.path().join("sources");
        fs::create_dir(&sources).unwrap();
        let store = Store::new(&dir.path().join("build"));
        World {
            _dir: dir,
            sources,
            store,
        }
    }

    impl World {
        /// A source file with these bytes, unique to this call.
        fn file(&self, name: &str, bytes: &[u8]) -> NewFile {
            let source = self.sources.join(format!(
                "{}.{}",
                name.replace('/', "_"),
                Digest::of(bytes).hex()
            ));
            fs::write(&source, bytes).unwrap();
            NewFile {
                name: name.to_string(),
                source,
            }
        }

        fn entries(&self, name: &str) -> usize {
            fs::read_dir(self.store.root().join(name)).map_or(0, |entries| entries.count())
        }
    }

    fn description(repository: &str) -> serde_json::Value {
        serde_json::json!({ "repository": repository, "crate": "demo" })
    }

    fn vouch(date_ms: u64) -> crate::origin::Vouch {
        crate::origin::Vouch {
            host: "https://api.example.com".into(),
            org: "acme".into(),
            repository: String::new(),
            scope: "person/1".into(),
            outputs: hushrun_core::blocks::Hex32([1; 32]),
            date_ms,
        }
    }

    // covers: B-32
    #[test]
    fn a_block_s_origin_is_there_from_its_publish_and_rewritten_whole() {
        let w = world();
        let files = [w.file("liba.rlib", b"one")];
        let origin = Origin::built_here("1-2", None);
        assert_eq!(
            w.store
                .publish_with_origin(&key(1), &description("r"), &files, &origin, day(1))
                .unwrap(),
            Published::Stored
        );
        assert_eq!(w.store.origin(&key(1)), Some(origin.clone()));
        // A block from before the record: none, and nothing to update it with.
        w.store
            .publish(&key(2), &description("r"), &files, day(1))
            .unwrap();
        assert_eq!(w.store.origin(&key(2)), None);
        assert!(w.store.update_origin(&key(3), |o| o).is_err());
        let wrote = w
            .store
            .update_origin(&key(1), |o| {
                let mut o = o?;
                o.reused = true;
                Some(o)
            })
            .unwrap();
        assert!(wrote);
        assert!(w.store.origin(&key(1)).unwrap().reused);
        assert!(!w.store.update_origin(&key(1), |_| None).unwrap());
        // Nothing left half written: the temporary files went by rename.
        assert_eq!(w.entries(TMP), 0);
        // The block itself still reads.
        assert!(w.store.block(&key(1)).unwrap().is_some());
    }

    // covers: B-32
    #[test]
    fn only_a_block_that_was_only_downloaded_gives_way() {
        let w = world();
        let downloaded = [w.file("liba.rlib", b"from a pack")];
        let compiled = [w.file("liba.rlib", b"compiled here")];
        let only_downloaded = |o: Option<&Origin>| o.is_some_and(Origin::downloaded_only);
        w.store
            .publish_with_origin(
                &key(1),
                &description("r"),
                &downloaded,
                &Origin::downloaded(vouch(0)),
                day(1),
            )
            .unwrap();
        let built = Origin::built_here("1-2", None);
        assert_eq!(
            w.store
                .replace(
                    &key(1),
                    &description("r"),
                    &compiled,
                    &built,
                    day(1),
                    only_downloaded
                )
                .unwrap(),
            Replaced::Stored
        );
        let block = w.store.block(&key(1)).unwrap().unwrap();
        assert_eq!(fs::read(&block.files[0].path).unwrap(), b"compiled here");
        assert_eq!(w.store.origin(&key(1)), Some(built.clone()));
        // A block built here never gives way, to a download or a compile.
        assert_eq!(
            w.store
                .replace(
                    &key(1),
                    &description("r"),
                    &downloaded,
                    &Origin::downloaded(vouch(0)),
                    day(1),
                    only_downloaded
                )
                .unwrap(),
            Replaced::Kept
        );
        let block = w.store.block(&key(1)).unwrap().unwrap();
        assert_eq!(fs::read(&block.files[0].path).unwrap(), b"compiled here");
        // Nor does one from before the record.
        w.store
            .publish(&key(2), &description("r"), &compiled, day(1))
            .unwrap();
        assert_eq!(
            w.store
                .replace(
                    &key(2),
                    &description("r"),
                    &downloaded,
                    &built,
                    day(1),
                    only_downloaded
                )
                .unwrap(),
            Replaced::Kept
        );
        // With nothing there it is a publish.
        assert_eq!(
            w.store
                .replace(
                    &key(3),
                    &description("r"),
                    &downloaded,
                    &built,
                    day(1),
                    only_downloaded
                )
                .unwrap(),
            Replaced::Stored
        );
        assert_eq!(w.entries(TMP), 0);
    }

    // covers: B-15, B-34
    #[test]
    fn a_suspension_with_an_end_is_over_at_its_end() {
        let w = world();
        w.store
            .suspend_until(&key(1), "a downloaded block differed", day(16), day(1))
            .unwrap();
        assert_eq!(
            w.store.suspended_at(&key(1), day(15)).as_deref(),
            Some("a downloaded block differed")
        );
        assert_eq!(w.store.suspended_at(&key(1), day(16)), None);
        w.store.suspend(&key(2), "for good", day(1)).unwrap();
        assert_eq!(
            w.store.suspended_at(&key(2), day(400)).as_deref(),
            Some("for good")
        );
    }

    // covers: B-30
    #[test]
    fn downloaded_hints_are_kept_apart_and_a_cost_is_remembered() {
        let w = world();
        w.store.add_hint(&key(1), b"local", day(1)).unwrap();
        w.store
            .add_shared_hint(&key(1), b"downloaded", day(2))
            .unwrap();
        assert_eq!(w.store.hints(&key(1)), vec![b"local".to_vec()]);
        assert_eq!(w.store.shared_hints(&key(1)), vec![b"downloaded".to_vec()]);
        assert_eq!(w.store.step_cost(&key(1)), None);
        w.store.set_step_cost(&key(1), 1234, day(1)).unwrap();
        assert_eq!(w.store.step_cost(&key(1)), Some(1234));
        assert_eq!(w.store.steps(), vec![key(1)]);
        // All of it goes with its age.
        w.store.remove_stale_hints(day(5));
        assert_eq!(w.store.steps(), Vec::<Digest>::new());
    }

    // covers: B-13
    #[test]
    fn a_published_block_is_complete_read_only_and_used_at_its_creation() {
        let w = world();
        let files = [
            w.file("libdemo.rlib", b"rlib"),
            w.file("demo.d", b"dep-info"),
        ];
        let published = w
            .store
            .publish(&key(1), &description("r"), &files, day(3))
            .unwrap();
        assert_eq!(published, Published::Stored);
        let block = w.store.block(&key(1)).unwrap().unwrap();
        assert_eq!(block.description, description("r"));
        let names: Vec<_> = block.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["libdemo.rlib", "demo.d"]);
        assert_eq!(block.files[0].size, 4);
        assert_eq!(block.files[0].sha256, Digest::of(b"rlib"));
        assert_eq!(fs::read(&block.files[1].path).unwrap(), b"dep-info");
        assert!(
            fs::metadata(&block.files[0].path)
                .unwrap()
                .permissions()
                .readonly()
        );
        assert_eq!(w.store.last_used(&key(1)), Some(day(3)));
        assert_eq!(w.entries(TMP), 0);
    }

    // covers: B-13
    #[test]
    fn two_threads_publishing_one_key_give_one_block_and_leave_nothing_in_tmp() {
        let w = world();
        for round in 0..16 {
            let k = key(round);
            let files = [w.file("lib.rlib", b"same bytes")];
            let barrier = Arc::new(Barrier::new(4));
            let results: Vec<Published> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..4)
                    .map(|i| {
                        let (barrier, store, files) = (barrier.clone(), &w.store, &files);
                        scope.spawn(move || {
                            barrier.wait();
                            // Half skip the early look, so the rename itself decides.
                            store
                                .publish_inner(
                                    &k,
                                    &description("r"),
                                    files,
                                    None,
                                    day(1),
                                    i % 2 == 0,
                                )
                                .unwrap()
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let stored = results.iter().filter(|r| **r == Published::Stored).count();
            assert_eq!(stored, 1, "round {round}: {results:?}");
            assert!(w.store.block(&k).unwrap().is_some());
        }
        assert_eq!(w.entries(TMP), 0);
    }

    // covers: B-13
    #[test]
    fn already_there_leaves_the_first_blocks_bytes_untouched() {
        let w = world();
        let first = [w.file("lib.rlib", b"first")];
        let second = [w.file("lib.rlib", b"second, longer")];
        w.store
            .publish(&key(1), &description("a"), &first, day(1))
            .unwrap();
        // Past the early look, the rename decides.
        let again = w
            .store
            .publish_inner(&key(1), &description("b"), &second, None, day(2), false)
            .unwrap();
        assert_eq!(again, Published::AlreadyThere);
        let block = w.store.block(&key(1)).unwrap().unwrap();
        assert_eq!(block.description, description("a"));
        assert_eq!(fs::read(&block.files[0].path).unwrap(), b"first");
        assert_eq!(block.files[0].sha256, Digest::of(b"first"));
        assert_eq!(w.store.last_used(&key(1)), Some(day(1)));
        assert_eq!(w.entries(TMP), 0);
    }

    // covers: B-13
    #[test]
    fn a_file_name_that_is_not_a_plain_path_inside_the_block_is_refused() {
        let w = world();
        for name in [
            "../lib.rlib",
            "..",
            ".",
            "",
            "a\\b",
            "/abs/lib.rlib",
            "x.dSYM/../../escape",
            "x.dSYM/./Contents",
            "x.dSYM//Contents",
            "x.dSYM/Contents/",
            "x.dSYM/Contents/..",
        ] {
            let file = NewFile {
                name: name.to_string(),
                source: w.file("x", b"x").source,
            };
            let err = w
                .store
                .publish(&key(1), &description("r"), &[file], day(1))
                .unwrap_err();
            assert!(matches!(err, StoreError::Name { .. }), "{name:?}: {err}");
        }
        let twice = [w.file("lib.rlib", b"a"), w.file("lib.rlib", b"b")];
        assert!(matches!(
            w.store.publish(&key(1), &description("r"), &twice, day(1)),
            Err(StoreError::Name { .. })
        ));
        // A file and a folder of the same name can't both be stored.
        let clash = [
            w.file("x.dSYM", b"a"),
            w.file("x.dSYM/Contents/Info.plist", b"b"),
        ];
        assert!(matches!(
            w.store.publish(&key(1), &description("r"), &clash, day(1)),
            Err(StoreError::Name { .. })
        ));
        assert!(w.store.block(&key(1)).unwrap().is_none());
        assert_eq!(w.entries(TMP), 0);
    }

    /// A program, its debug bundle's files under nested names, and a library beside them.
    fn bundled(w: &World, dwarf: &[u8]) -> Vec<NewFile> {
        vec![
            w.file("prog-1", b"program"),
            w.file("prog-1.dSYM/Contents/Info.plist", b"<plist/>"),
            w.file("prog-1.dSYM/Contents/Resources/DWARF/prog-1", dwarf),
        ]
    }

    // covers: B-12, B-13
    #[test]
    fn a_debug_bundle_is_stored_under_nested_names_and_read_back_whole() {
        let w = world();
        let files = bundled(&w, b"dwarf bytes");
        assert_eq!(
            w.store
                .publish(&key(1), &description("r"), &files, day(1))
                .unwrap(),
            Published::Stored
        );
        let block = w.store.block(&key(1)).unwrap().unwrap();
        let names: Vec<_> = block.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "prog-1",
                "prog-1.dSYM/Contents/Info.plist",
                "prog-1.dSYM/Contents/Resources/DWARF/prog-1"
            ]
        );
        let dwarf = &block.files[2];
        assert!(
            dwarf
                .path
                .starts_with(w.store.block_dir(&key(1)).join(FILES))
        );
        assert_eq!(fs::read(&dwarf.path).unwrap(), b"dwarf bytes");
        assert_eq!(dwarf.sha256, Digest::of(b"dwarf bytes"));
        assert!(fs::metadata(&dwarf.path).unwrap().permissions().readonly());
        assert_eq!(w.store.status(day(1)).bytes, 7 + 8 + 11);
        // A nested file cut short makes the whole block absent.
        let mut perms = fs::metadata(&dwarf.path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&dwarf.path, perms).unwrap();
        fs::write(&dwarf.path, b"dwarf").unwrap();
        assert_eq!(w.store.block(&key(1)).unwrap(), None);
        assert!(!w.store.block_dir(&key(1)).exists());
        assert_eq!(w.entries(TRASH), 0);
    }

    // covers: B-13
    #[test]
    fn two_writers_of_one_bundled_block_give_one_block_and_leave_nothing_in_tmp() {
        let w = world();
        for round in 0..8 {
            let k = key(100 + round);
            let files = bundled(&w, b"same dwarf");
            let barrier = Arc::new(Barrier::new(4));
            let results: Vec<Published> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..4)
                    .map(|i| {
                        let (barrier, store, files) = (barrier.clone(), &w.store, &files);
                        scope.spawn(move || {
                            barrier.wait();
                            store
                                .publish_inner(
                                    &k,
                                    &description("r"),
                                    files,
                                    None,
                                    day(1),
                                    i % 2 == 0,
                                )
                                .unwrap()
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let stored = results.iter().filter(|r| **r == Published::Stored).count();
            assert_eq!(stored, 1, "round {round}: {results:?}");
            let block = w.store.block(&k).unwrap().unwrap();
            assert_eq!(block.files.len(), 3);
        }
        assert_eq!(w.entries(TMP), 0);
    }

    // covers: B-19
    #[test]
    fn a_block_with_a_truncated_file_is_expired_and_absent() {
        let w = world();
        let files = [w.file("lib.rlib", b"0123456789")];
        w.store
            .publish(&key(1), &description("r"), &files, day(1))
            .unwrap();
        let path = w.store.block(&key(1)).unwrap().unwrap().files[0]
            .path
            .clone();
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"01234").unwrap();
        assert_eq!(w.store.block(&key(1)).unwrap(), None);
        assert!(!w.store.block_dir(&key(1)).exists());
        assert_eq!(w.entries(TRASH), 0);
        // The next compile stores it again.
        assert_eq!(
            w.store
                .publish(&key(1), &description("r"), &files, day(2))
                .unwrap(),
            Published::Stored
        );
    }

    // covers: B-19
    #[test]
    fn a_block_whose_record_cant_be_read_or_whose_file_is_missing_is_absent() {
        let w = world();
        let files = [w.file("lib.rlib", b"bytes")];
        w.store
            .publish(&key(1), &description("r"), &files, day(1))
            .unwrap();
        let json = w.store.block_dir(&key(1)).join(BLOCK_JSON);
        fs::remove_file(&json).unwrap();
        fs::write(&json, b"{not json").unwrap();
        assert_eq!(w.store.block(&key(1)).unwrap(), None);
        assert!(!w.store.block_dir(&key(1)).exists());

        w.store
            .publish(&key(2), &description("r"), &files, day(1))
            .unwrap();
        fs::remove_file(w.store.block_dir(&key(2)).join(FILES).join("lib.rlib")).unwrap();
        assert_eq!(w.store.block(&key(2)).unwrap(), None);
        assert!(!w.store.block_dir(&key(2)).exists());
    }

    // covers: B-19
    #[test]
    fn a_reader_while_the_block_moves_to_trash_gets_none_never_a_partial_block() {
        let w = world();
        let files = [w.file("lib.rlib", b"abc"), w.file("lib.rmeta", b"abcde")];
        w.store
            .publish(&key(1), &description("r"), &files, day(1))
            .unwrap();
        let got = w
            .store
            .lookup(&key(1), || w.store.discard(&key(1)).unwrap())
            .unwrap();
        assert_eq!(got, None);

        // Moved and replaced by another block of other sizes: neither is described, and the
        // new one is not taken for damaged.
        w.store
            .publish(&key(1), &description("r"), &files, day(1))
            .unwrap();
        let other = [w.file("lib.rlib", b"abcdefgh"), w.file("lib.rmeta", b"a")];
        let got = w
            .store
            .lookup(&key(1), || {
                w.store.discard(&key(1)).unwrap();
                w.store
                    .publish(&key(1), &description("new"), &other, day(2))
                    .unwrap();
            })
            .unwrap();
        assert_eq!(got, None);
        let now = w.store.block(&key(1)).unwrap().unwrap();
        assert_eq!(now.description, description("new"));
    }

    // covers: B-19
    #[test]
    fn readers_racing_a_mover_see_a_complete_block_or_none() {
        let w = world();
        let files = [w.file("lib.rlib", b"abc"), w.file("lib.rmeta", b"abcde")];
        w.store
            .publish(&key(1), &description("r"), &files, day(1))
            .unwrap();
        std::thread::scope(|scope| {
            let mover = scope.spawn(|| {
                for _ in 0..200 {
                    w.store.discard(&key(1)).unwrap();
                    w.store
                        .publish(&key(1), &description("r"), &files, day(1))
                        .unwrap();
                }
            });
            while !mover.is_finished() {
                if let Some(block) = w.store.block(&key(1)).unwrap() {
                    let sizes: Vec<u64> = block.files.iter().map(|f| f.size).collect();
                    assert_eq!(sizes, [3, 5]);
                    assert_eq!(block.description, description("r"));
                }
            }
        });
        assert!(w.store.block(&key(1)).unwrap().is_some());
    }

    // covers: B-19
    #[test]
    fn a_folder_left_in_tmp_by_a_crash_is_not_a_block() {
        let w = world();
        let files = [w.file("lib.rlib", b"bytes")];
        let temp = w.store.reserve_dir(&key(1).hex()).unwrap();
        write_block(&temp, &key(1), &description("r"), &files, None, day(1)).unwrap();
        assert_eq!(w.store.block(&key(1)).unwrap(), None);
        assert_eq!(w.store.status(day(1)).blocks, 0);
    }

    // covers: B-14
    #[test]
    fn a_reuse_sets_the_last_use_and_only_the_used_file_changes() {
        let w = world();
        let files = [w.file("lib.rlib", b"bytes")];
        w.store
            .publish(&key(1), &description("r"), &files, day(1))
            .unwrap();
        let json = w.store.block_dir(&key(1)).join(BLOCK_JSON);
        let before = fs::read(&json).unwrap();
        w.store.used(&key(1), day(9)).unwrap();
        assert_eq!(w.store.last_used(&key(1)), Some(day(9)));
        // A clock set back is recorded as given: the last use is the command's time.
        w.store.used(&key(1), day(5)).unwrap();
        assert_eq!(w.store.last_used(&key(1)), Some(day(5)));
        assert_eq!(fs::read(&json).unwrap(), before);
        assert!(w.store.used(&key(2), day(9)).is_err());
        assert_eq!(w.store.last_used(&key(2)), None);
    }

    // covers: B-7
    #[test]
    fn hints_come_newest_first_at_most_sixteen_and_a_repeat_refreshes_its_date() {
        let w = world();
        let step = key(7);
        assert!(w.store.hints(&step).is_empty());
        for n in 0..20u64 {
            w.store
                .add_hint(&step, format!("hint {n}").as_bytes(), day(n))
                .unwrap();
        }
        let hints = w.store.hints(&step);
        assert_eq!(hints.len(), MAX_HINTS);
        assert_eq!(hints[0], b"hint 19");
        assert_eq!(hints[15], b"hint 4");
        w.store.add_hint(&step, b"hint 0", day(30)).unwrap();
        let hints = Store::new(w.store.root()).hints(&step);
        assert_eq!(hints[0], b"hint 0");
        assert_eq!(hints[1], b"hint 19");
        // Each written whole, nothing left behind, other keys apart.
        assert_eq!(w.entries(TMP), 0);
        assert!(w.store.hints(&key(8)).is_empty());
    }

    // covers: B-15
    #[test]
    fn a_suspension_lasts_across_processes_until_clean() {
        let w = world();
        let step = key(3);
        assert_eq!(w.store.suspended(&step), None);
        w.store.suspend(&step, "an audit differed", day(1)).unwrap();
        let other = Store::new(w.store.root());
        assert_eq!(other.suspended(&step).as_deref(), Some("an audit differed"));
        assert_eq!(other.suspended(&key(4)), None);
        // Suspending again keeps it suspended, with the newer reason.
        other.suspend(&step, "a dependent failed", day(2)).unwrap();
        assert_eq!(
            w.store.suspended(&step).as_deref(),
            Some("a dependent failed")
        );
        let files = [w.file("lib.rlib", b"bytes")];
        w.store
            .publish(&key(1), &description("r"), &files, day(1))
            .unwrap();
        w.store.add_hint(&step, b"hint", day(1)).unwrap();
        other.clean().unwrap();
        assert_eq!(w.store.suspended(&step), None);
        assert!(w.store.hints(&step).is_empty());
        assert_eq!(w.store.block(&key(1)).unwrap(), None);
        assert_eq!(w.entries(TRASH), 0);
        // An empty store cleans too.
        w.store.clean().unwrap();
    }

    // covers: B-15
    #[test]
    fn a_discarded_block_is_gone() {
        let w = world();
        let files = [w.file("lib.rlib", b"bytes")];
        w.store
            .publish(&key(1), &description("r"), &files, day(1))
            .unwrap();
        w.store.discard(&key(1)).unwrap();
        assert_eq!(w.store.block(&key(1)).unwrap(), None);
        assert_eq!(w.entries(TRASH), 0);
        w.store.discard(&key(1)).unwrap();
    }

    // covers: B-21
    #[test]
    fn status_counts_blocks_by_age_and_repository() {
        let w = world();
        assert_eq!(w.store.status(day(0)), Status::default());
        let small = [w.file("a", b"12")];
        let big = [w.file("a", b"1234"), w.file("b", b"123456")];
        w.store
            .publish(&key(1), &description("alpha"), &small, day(20))
            .unwrap();
        w.store
            .publish(&key(2), &description("beta"), &big, day(17))
            .unwrap();
        w.store
            .publish(&key(3), &serde_json::json!({}), &small, day(1))
            .unwrap();
        w.store
            .publish(&key(4), &description("alpha"), &small, day(21))
            .unwrap();
        let status = w.store.status(day(20) + Duration::from_secs(60));
        assert_eq!(status.blocks, 4);
        assert_eq!(status.bytes, 2 + 10 + 2 + 2);
        assert_eq!(
            status.used_today, 2,
            "day 20, and day 21 from a clock set back"
        );
        assert_eq!(status.used_this_week, 1);
        assert_eq!(status.used_before, 1);
        assert_eq!(
            status.by_repository,
            [
                RepositoryUsage {
                    repository: Some("beta".into()),
                    blocks: 1,
                    bytes: 10
                },
                RepositoryUsage {
                    repository: Some("alpha".into()),
                    blocks: 2,
                    bytes: 4
                },
                RepositoryUsage {
                    repository: None,
                    blocks: 1,
                    bytes: 2
                },
            ]
        );
    }

    // covers: B-22
    #[test]
    fn free_bytes_reads_the_volume_before_the_store_exists() {
        let w = world();
        let deep = Store::new(&w.store.root().join("not/yet/made"));
        let free = deep.free_bytes().unwrap();
        assert!(free > 0);
        assert!(!deep.root().exists());
    }
}
