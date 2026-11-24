//! The service's own cache on this machine (B-79): every object and every value Xcode puts, in
//! the store's folder under `xcode/`, so that any checkout, worktree or branch on this machine,
//! and the Xcode app beside `xcodebuild`, finds what another compiled. It is to Xcode what
//! `GOCACHE` is to Go: the local side, kept whatever leaves the machine.
//!
//! - **An object** is bytes and an ordered list of other objects' ids. Its **id** is the
//!   SHA-256 of `hushrun.xcode-object/v1\n`, the number of references (8 bytes, big-endian),
//!   each reference's 32 bytes and the bytes ([`id`]). The service chooses ids, and Xcode's
//!   plugin keeps them as it is given them, so every machine must compute them the same way: an
//!   object downloaded from another machine is named by its content, and a value that names it
//!   names the same thing everywhere. An id is a digest of everything below it, so two values
//!   with the same bytes hold the same outputs, all the way down.
//! - **A value** is what Xcode puts under one of its keys (a compile's cache key, hashed by
//!   Xcode from the compiler, its arguments and the content of every input). Kept under the
//!   SHA-256 of the key, so a file name never shows the key.
//!
//! Files are written whole under a temporary name and renamed into place; a reader that finds a
//! file that doesn't parse, or an object whose bytes aren't its id, removes it and answers a
//! miss. A file is dated when it is used, at most once an hour, and the trim at the end of a
//! service removes what nobody used for the store's kept days, at most once a day.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hushrun_core::Digest;

use super::proto::{self, Value};

/// The folder in the store.
pub const DIR: &str = "xcode";
const OBJECTS: &str = "objects";
const VALUES: &str = "values";
const TMP: &str = "tmp";
/// Files the service hands Xcode's plugin in place of bytes, for it to move into its own cache.
const EXPORTS: &str = "exports";
/// The trim's last time, in decimal seconds since the epoch.
const TRIMMED: &str = "trim.txt";
/// The schema an object's id is taken under.
const ID_SCHEMA: &[u8] = b"hushrun.xcode-object/v1\n";
/// The schema a value file begins with.
const VALUE_MAGIC: &[u8] = b"hushrun.xcode-value/v1\n";
/// The schema an object file begins with.
const OBJECT_MAGIC: &[u8] = b"hushrun.xcode-object-file/v1\n";
/// The most bytes one object may hold: a larger one is refused, as a put the service can't keep.
pub const MOST_OBJECT: u64 = 2 << 30;
/// The most references one object may hold.
pub const MOST_REFERENCES: usize = 1 << 20;
/// An id's length.
pub const ID_LEN: usize = 32;

/// An object's id.
pub type Id = [u8; ID_LEN];

/// One object: its bytes and its references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub data: Vec<u8>,
    pub references: Vec<Id>,
}

impl Object {
    /// Its id ([`id`]).
    pub fn id(&self) -> Id {
        id(&self.data, &self.references)
    }

    /// The bytes of its file: the schema, the number of references, each reference, the data.
    pub fn file_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            OBJECT_MAGIC.len() + 8 + self.references.len() * ID_LEN + self.data.len(),
        );
        out.extend_from_slice(OBJECT_MAGIC);
        out.extend_from_slice(&(self.references.len() as u64).to_be_bytes());
        for reference in &self.references {
            out.extend_from_slice(reference);
        }
        out.extend_from_slice(&self.data);
        out
    }

    /// Reads a file [`Object::file_bytes`] wrote, and checks it is the object `expected` names.
    pub fn from_file_bytes(bytes: &[u8], expected: &Id) -> Option<Object> {
        let rest = bytes.strip_prefix(OBJECT_MAGIC)?;
        let (count, rest) = rest.split_first_chunk::<8>()?;
        let count = usize::try_from(u64::from_be_bytes(*count)).ok()?;
        if count > MOST_REFERENCES || rest.len() < count * ID_LEN {
            return None;
        }
        let (refs, data) = rest.split_at(count * ID_LEN);
        let references = refs
            .chunks_exact(ID_LEN)
            .map(|chunk| <Id>::try_from(chunk).expect("chunks are an id long"))
            .collect();
        let object = Object {
            data: data.to_vec(),
            references,
        };
        (object.id() == *expected).then_some(object)
    }
}

/// An object's id: the SHA-256 of the schema, the number of its references, each of them and
/// its bytes. References are ids, a fixed length, so nothing else gives the same input.
pub fn id(data: &[u8], references: &[Id]) -> Id {
    let mut input =
        Vec::with_capacity(ID_SCHEMA.len() + 8 + references.len() * ID_LEN + data.len());
    input.extend_from_slice(ID_SCHEMA);
    input.extend_from_slice(&(references.len() as u64).to_be_bytes());
    for reference in references {
        input.extend_from_slice(reference);
    }
    input.extend_from_slice(data);
    *Digest::of(&input).as_bytes()
}

/// An id as Xcode sent it: 32 bytes, else not one this service gave.
pub fn id_of(bytes: &[u8]) -> Option<Id> {
    <Id>::try_from(bytes).ok()
}

/// The digest a value is named by in a block and compared by in an audit: the SHA-256 of its
/// one encoding ([`proto::encode_value`]).
pub fn value_digest(value: &Value) -> Digest {
    Digest::of(&proto::encode_value(value))
}

/// The cache in a store's folder.
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// The cache under the store at `store`. Nothing is created until something is written.
    pub fn in_store(store: &Path) -> Cache {
        Cache {
            root: store.join(DIR),
        }
    }

    /// The cache at `root` itself.
    pub fn at(root: &Path) -> Cache {
        Cache {
            root: root.to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn object_path(&self, id: &Id) -> PathBuf {
        let hex = hushrun_core::digest::to_hex(id);
        self.root.join(OBJECTS).join(&hex[..2]).join(hex)
    }

    fn value_path(&self, key: &[u8]) -> PathBuf {
        let hex = Digest::of(key).hex();
        self.root.join(VALUES).join(&hex[..2]).join(hex)
    }

    /// Whether the object is here, by its file alone.
    pub fn has(&self, id: &Id) -> bool {
        self.object_path(id).is_file()
    }

    /// The references of the object `id`, read from its file's head alone (no bytes, no hash):
    /// what tells, cheaply, that a value's outputs are all still here. `None`: not here.
    pub fn references(&self, id: &Id) -> Option<Vec<Id>> {
        use std::io::Read as _;
        let mut file = fs::File::open(self.object_path(id)).ok()?;
        let mut head = vec![0u8; OBJECT_MAGIC.len() + 8];
        file.read_exact(&mut head).ok()?;
        let count = head.strip_prefix(OBJECT_MAGIC)?;
        let count = usize::try_from(u64::from_be_bytes(count.try_into().ok()?)).ok()?;
        if count > MOST_REFERENCES {
            return None;
        }
        let mut refs = vec![0u8; count * ID_LEN];
        file.read_exact(&mut refs).ok()?;
        Some(
            refs.chunks_exact(ID_LEN)
                .map(|chunk| <Id>::try_from(chunk).expect("chunks are an id long"))
                .collect(),
        )
    }

    /// The object `id`, checked against its id; dated as used. A file that isn't it is removed.
    pub fn object(&self, id: &Id, now: SystemTime) -> Option<Object> {
        let path = self.object_path(id);
        let bytes = fs::read(&path).ok()?;
        match Object::from_file_bytes(&bytes, id) {
            Some(object) => {
                touch(&path, now);
                Some(object)
            }
            None => {
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    /// Keeps an object. Its id, which the service answers with.
    pub fn put_object(&self, object: &Object, now: SystemTime) -> io::Result<Id> {
        let id = object.id();
        let path = self.object_path(&id);
        if path.is_file() {
            touch(&path, now);
            return Ok(id);
        }
        self.write_whole(&path, &object.file_bytes())?;
        Ok(id)
    }

    /// The value under `key`; dated as used. A file that isn't one is removed.
    pub fn value(&self, key: &[u8], now: SystemTime) -> Option<Value> {
        let path = self.value_path(key);
        let bytes = fs::read(&path).ok()?;
        let parsed = bytes
            .strip_prefix(VALUE_MAGIC)
            .and_then(|rest| proto::read_value(rest).ok());
        match parsed {
            Some(value) => {
                touch(&path, now);
                Some(value)
            }
            None => {
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    /// Keeps `value` under `key`, in place of any other: Xcode put it last.
    pub fn put_value(&self, key: &[u8], value: &Value) -> io::Result<()> {
        let mut bytes = VALUE_MAGIC.to_vec();
        bytes.extend_from_slice(&proto::encode_value(value));
        self.write_whole(&self.value_path(key), &bytes)
    }

    /// Writes `data` to a file of its own for Xcode's plugin to take (`write_to_disk`): what a
    /// large object is handed as, since a gRPC client refuses a message over 4 MiB by default.
    pub fn export(&self, data: &[u8]) -> io::Result<PathBuf> {
        let dir = self.root.join(EXPORTS);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.{}.blob", std::process::id(), unique()));
        let tmp = self.root.join(TMP);
        fs::create_dir_all(&tmp)?;
        let staged = tmp.join(format!("export.{}.{}", std::process::id(), unique()));
        fs::write(&staged, data)?;
        fs::rename(&staged, &path)?;
        Ok(path)
    }

    /// Removes the value under `key`: what a value found wrong leaves.
    pub fn forget_value(&self, key: &[u8]) {
        let _ = fs::remove_file(self.value_path(key));
    }

    fn write_whole(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let tmp_dir = self.root.join(TMP);
        fs::create_dir_all(&tmp_dir)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = tmp_dir.join(format!(
            "{}.{}.{}",
            std::process::id(),
            unique(),
            path.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        ));
        let mut file = fs::File::create(&tmp)?;
        let written = file.write_all(bytes).and_then(|()| file.sync_data());
        drop(file);
        if let Err(err) = written.and_then(|()| fs::rename(&tmp, path)) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
        Ok(())
    }

    /// Removes what nobody used for `keep_days`, at most once a day, and temporary files a dead
    /// service left. What it removed.
    pub fn trim(&self, now: SystemTime, keep_days: u32) -> Trimmed {
        let mut trimmed = Trimmed::default();
        let stamp = self.root.join(TRIMMED);
        let day = Duration::from_secs(86_400);
        let last = fs::read_to_string(&stamp)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
            .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        if last.is_some_and(|last| now.duration_since(last).is_ok_and(|age| age < day)) {
            return trimmed;
        }
        if !self.root.is_dir() {
            return trimmed;
        }
        let keep = Duration::from_secs(u64::from(keep_days.max(1)) * 86_400);
        for (sub, count) in [
            (OBJECTS, &mut trimmed.objects),
            (VALUES, &mut trimmed.values),
        ] {
            let Ok(shards) = fs::read_dir(self.root.join(sub)) else {
                continue;
            };
            for shard in shards.flatten() {
                let Ok(files) = fs::read_dir(shard.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let old = file
                        .metadata()
                        .and_then(|meta| meta.modified())
                        .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > keep));
                    if old && fs::remove_file(file.path()).is_ok() {
                        *count += 1;
                    }
                }
            }
        }
        for file in [TMP, EXPORTS]
            .iter()
            .filter_map(|sub| fs::read_dir(self.root.join(sub)).ok())
            .flatten()
            .flatten()
        {
            let stale = file
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > day));
            if stale {
                let _ = fs::remove_file(file.path());
            }
        }
        let secs = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let _ = self.write_whole(&stamp, format!("{secs}\n").as_bytes());
        trimmed
    }

    /// How many bytes the cache holds, for `hushrun build xcode status`.
    pub fn size(&self) -> u64 {
        fn walk(dir: &Path) -> u64 {
            let Ok(entries) = fs::read_dir(dir) else {
                return 0;
            };
            entries
                .flatten()
                .map(|entry| match entry.file_type() {
                    Ok(kind) if kind.is_dir() => walk(&entry.path()),
                    Ok(_) => entry.metadata().map_or(0, |meta| meta.len()),
                    Err(_) => 0,
                })
                .sum()
        }
        walk(&self.root)
    }
}

/// What a trim removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Trimmed {
    pub objects: u64,
    pub values: u64,
}

/// Dates `path` now when its date is an hour old or more, as Go's `markUsed` does: a hot cache
/// isn't written on every read.
fn touch(path: &Path, now: SystemTime) {
    let old = fs::metadata(path)
        .and_then(|meta| meta.modified())
        .is_ok_and(|at| {
            now.duration_since(at)
                .is_ok_and(|age| age >= Duration::from_secs(3600))
        });
    if old && let Ok(file) = fs::File::options().append(true).open(path) {
        let _ = file.set_modified(now);
    }
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(data: &[u8], references: &[Id]) -> Object {
        Object {
            data: data.to_vec(),
            references: references.to_vec(),
        }
    }

    // covers: B-79
    #[test]
    fn an_id_is_its_content_and_its_references_in_order() {
        let a = object(b"a", &[]).id();
        let b = object(b"b", &[]).id();
        assert_eq!(a, object(b"a", &[]).id());
        assert_ne!(a, b);
        let ab = object(b"x", &[a, b]).id();
        assert_ne!(ab, object(b"x", &[b, a]).id());
        assert_ne!(ab, object(b"x", &[a]).id());
        // The same bytes with no references and as a reference-less object: one id, as LLVM's
        // own store gives a `Save` and a `Put` with none.
        assert_eq!(object(b"x", &[]).id(), id(b"x", &[]));
        // Fixed: every machine must compute the same ids.
        assert_eq!(
            hushrun_core::digest::to_hex(&id(b"", &[])),
            Digest::of(b"hushrun.xcode-object/v1\n\0\0\0\0\0\0\0\0").hex()
        );
    }

    // covers: B-79
    #[test]
    fn objects_and_values_are_kept_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::in_store(dir.path());
        let now = SystemTime::now();
        let leaf = object(b"leaf", &[]);
        let leaf_id = cache.put_object(&leaf, now).unwrap();
        let root = object(b"root", &[leaf_id]);
        let root_id = cache.put_object(&root, now).unwrap();
        assert_eq!(cache.object(&root_id, now), Some(root.clone()));
        assert!(cache.has(&leaf_id));
        // Putting again is the same id and no error.
        assert_eq!(cache.put_object(&root, now).unwrap(), root_id);

        let value: Value = [("value".to_string(), b"v".to_vec())].into();
        cache.put_value(b"key", &value).unwrap();
        assert_eq!(cache.value(b"key", now), Some(value));
        assert_eq!(cache.value(b"other", now), None);

        // A file that isn't its object is removed and is a miss.
        let path = cache.object_path(&leaf_id);
        fs::write(&path, object(b"other", &[]).file_bytes()).unwrap();
        assert_eq!(cache.object(&leaf_id, now), None);
        assert!(!path.exists());
        // A value file that doesn't parse, too.
        let path = cache.value_path(b"key");
        fs::write(&path, b"junk").unwrap();
        assert_eq!(cache.value(b"key", now), None);
        assert!(!path.exists());
    }

    // covers: B-79
    #[test]
    fn the_trim_removes_what_nobody_used_and_waits_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::in_store(dir.path());
        let then = SystemTime::now() - Duration::from_secs(20 * 86_400);
        let old = cache.put_object(&object(b"old", &[]), then).unwrap();
        let fresh = cache.put_object(&object(b"fresh", &[]), then).unwrap();
        let old_file = fs::File::options()
            .append(true)
            .open(cache.object_path(&old))
            .unwrap();
        old_file.set_modified(then).unwrap();
        let now = SystemTime::now();
        let trimmed = cache.trim(now, 15);
        assert_eq!(trimmed.objects, 1);
        assert!(!cache.has(&old));
        assert!(cache.has(&fresh));
        // Again within the day: nothing looked at.
        assert_eq!(cache.trim(now, 15), Trimmed::default());
    }
}
