//! Caches that make repeated digests fast (plan §12.6).
//!
//! - [`OidCache`] maps a VCS object id to the SHA-256 and size of its content. Object ids are
//!   content hashes, so an entry can never be stale, and the file can be shared between CI
//!   runs (`HUSHRUN_CACHE_DIR` with `actions/cache`).
//! - [`StatCache`] maps a file's path and `stat` data to its SHA-256, for the dir adapter.
//!
//! Each file starts with a format header; a mismatch or a damaged file discards it. Files are
//! written at the end of a command, through a temporary file and a rename. Nothing here is ever
//! an error: a cache that can't be read or written is simply empty.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::Sha256;
use crate::hash::sha256;

/// The longest object id, in bytes (SHA-256 object format).
pub const MAX_OID_LEN: usize = 32;

const OID_MAGIC: &[u8] = b"hushrun-oid-sha256 v1\n";
const STAT_MAGIC: &[u8] = b"hushrun-dir-stat v1\n";
/// Entries modified this close to the cache file's write time are re-hashed: a file changed
/// again within the same timestamp granularity would otherwise look unchanged.
const RACY_NS: i64 = 2_000_000_000;

/// Object id → (SHA-256, size), for one object format.
#[derive(Debug)]
pub struct OidCache {
    path: Option<PathBuf>,
    oid_len: usize,
    map: HashMap<[u8; MAX_OID_LEN], (Sha256, u64)>,
    /// Records in the file when it was loaded, duplicates included.
    records_on_disk: usize,
    added: Vec<[u8; MAX_OID_LEN]>,
}

impl OidCache {
    /// A cache that stores nothing.
    pub fn disabled(oid_len: usize) -> Self {
        OidCache {
            path: None,
            oid_len,
            map: HashMap::new(),
            records_on_disk: 0,
            added: Vec::new(),
        }
    }

    /// Loads `<dir>/oid-sha256/<object format>.bin`; any problem yields an empty cache.
    pub fn open(dir: &Path, object_format: &str, oid_len: usize) -> Self {
        assert!(oid_len <= MAX_OID_LEN, "object ids are at most 32 bytes");
        let path = dir.join("oid-sha256").join(format!("{object_format}.bin"));
        let mut cache = OidCache {
            path: Some(path.clone()),
            ..OidCache::disabled(oid_len)
        };
        if let Ok(bytes) = fs::read(&path) {
            cache.load(&bytes);
        }
        cache
    }

    fn record_len(&self) -> usize {
        self.oid_len + 32 + 8
    }

    fn load(&mut self, bytes: &[u8]) {
        let Some(body) = bytes.strip_prefix(OID_MAGIC) else {
            return;
        };
        let record_len = self.record_len();
        // A trailing partial record (a torn write) is ignored.
        for record in body.chunks_exact(record_len) {
            let (oid, rest) = record.split_at(self.oid_len);
            let (sha, size) = rest.split_at(32);
            self.map.insert(
                key(oid),
                (
                    sha.try_into().expect("32 bytes"),
                    u64::from_le_bytes(size.try_into().expect("8 bytes")),
                ),
            );
            self.records_on_disk += 1;
        }
    }

    pub fn get(&self, oid: &[u8]) -> Option<(Sha256, u64)> {
        self.map.get(&key(oid)).copied()
    }

    pub fn insert(&mut self, oid: &[u8], sha: Sha256, size: u64) {
        let k = key(oid);
        if self.map.insert(k, (sha, size)).is_none() {
            self.added.push(k);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Writes new records, compacting when duplicates exceed half the file.
    pub fn save(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if self.added.is_empty() {
            return Ok(());
        }
        // Another process may have written since we loaded: merge with what is there now.
        let existing = fs::read(&path).ok();
        let mut current = OidCache::disabled(self.oid_len);
        if let Some(bytes) = &existing {
            current.load(bytes);
        }
        let mut new_keys: Vec<[u8; MAX_OID_LEN]> = self
            .added
            .iter()
            .filter(|k| !current.map.contains_key(*k))
            .copied()
            .collect();
        let duplicates = current.records_on_disk - current.map.len();
        let compact = duplicates * 2 > current.records_on_disk;
        if new_keys.is_empty() && !compact {
            self.added.clear();
            return Ok(());
        }
        let record_len = self.record_len();
        let mut body = Vec::new();
        if compact {
            new_keys.extend(current.map.keys().copied());
        } else if let Some(old) = existing.as_deref().and_then(|b| b.strip_prefix(OID_MAGIC)) {
            body.extend_from_slice(&old[..old.len() / record_len * record_len]);
        }
        new_keys.sort_unstable();
        new_keys.dedup();
        for k in &new_keys {
            let (sha, size) = self
                .map
                .get(k)
                .or_else(|| current.map.get(k))
                .copied()
                .expect("every key comes from one of the two maps");
            body.extend_from_slice(&k[..self.oid_len]);
            body.extend_from_slice(&sha);
            body.extend_from_slice(&size.to_le_bytes());
        }
        write_atomic(&path, OID_MAGIC, &body)?;
        self.added.clear();
        Ok(())
    }
}

fn key(oid: &[u8]) -> [u8; MAX_OID_LEN] {
    let mut k = [0u8; MAX_OID_LEN];
    k[..oid.len()].copy_from_slice(oid);
    k
}

/// What the stat cache compares to decide that a file is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stat {
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub inode: u64,
    pub device: u64,
    pub mode: u32,
}

impl Stat {
    pub fn of(meta: &fs::Metadata) -> Stat {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Stat {
                size: meta.len(),
                mtime_ns: meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
                ctime_ns: meta.ctime() * 1_000_000_000 + meta.ctime_nsec(),
                inode: meta.ino(),
                device: meta.dev(),
                mode: meta.mode(),
            }
        }
        #[cfg(not(unix))]
        {
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as i64);
            Stat {
                size: meta.len(),
                mtime_ns,
                ..Stat::default()
            }
        }
    }
}

/// (path, stat) → SHA-256 for the files under one directory root.
#[derive(Debug)]
pub struct StatCache {
    path: Option<PathBuf>,
    written_ns: i64,
    map: HashMap<String, (Stat, Sha256)>,
    changed: bool,
}

impl StatCache {
    pub fn disabled() -> Self {
        StatCache {
            path: None,
            written_ns: 0,
            map: HashMap::new(),
            changed: false,
        }
    }

    /// Loads `<dir>/dir-stat/<sha256 of the root path>.bin`.
    pub fn open(dir: &Path, root: &Path) -> Self {
        let name = hushrun_core::digest::to_hex(&sha256(root.to_string_lossy().as_bytes()));
        let path = dir.join("dir-stat").join(format!("{name}.bin"));
        let mut cache = StatCache {
            path: Some(path.clone()),
            ..StatCache::disabled()
        };
        if let Ok(bytes) = fs::read(&path) {
            cache.load(&bytes);
        }
        cache
    }

    fn load(&mut self, bytes: &[u8]) {
        let Some(mut rest) = bytes.strip_prefix(STAT_MAGIC) else {
            return;
        };
        let Some(written) = take::<8>(&mut rest) else {
            return;
        };
        self.written_ns = i64::from_le_bytes(written);
        while let Some(len) = take::<4>(&mut rest) {
            let len = u32::from_le_bytes(len) as usize;
            if rest.len() < len {
                break;
            }
            let (path, tail) = rest.split_at(len);
            rest = tail;
            let (
                Some(size),
                Some(mtime),
                Some(ctime),
                Some(inode),
                Some(device),
                Some(mode),
                Some(sha),
            ) = (
                take::<8>(&mut rest),
                take::<8>(&mut rest),
                take::<8>(&mut rest),
                take::<8>(&mut rest),
                take::<8>(&mut rest),
                take::<4>(&mut rest),
                take::<32>(&mut rest),
            )
            else {
                break;
            };
            let Ok(path) = String::from_utf8(path.to_vec()) else {
                break;
            };
            let stat = Stat {
                size: u64::from_le_bytes(size),
                mtime_ns: i64::from_le_bytes(mtime),
                ctime_ns: i64::from_le_bytes(ctime),
                inode: u64::from_le_bytes(inode),
                device: u64::from_le_bytes(device),
                mode: u32::from_le_bytes(mode),
            };
            self.map.insert(path, (stat, sha));
        }
    }

    /// The cached hash of `path`, if its stat data is unchanged and it wasn't modified
    /// within 2 s of the cache's write time.
    pub fn get(&self, path: &str, stat: &Stat) -> Option<Sha256> {
        let (cached, sha) = self.map.get(path)?;
        (cached == stat && stat.mtime_ns < self.written_ns - RACY_NS).then_some(*sha)
    }

    pub fn insert(&mut self, path: &str, stat: Stat, sha: Sha256) {
        if self.map.get(path) != Some(&(stat, sha)) {
            self.map.insert(path.to_string(), (stat, sha));
            self.changed = true;
        }
    }

    /// Keeps only the given paths (files that disappeared drop out).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        let before = self.map.len();
        self.map.retain(|p, _| keep(p));
        self.changed |= self.map.len() != before;
    }

    pub fn save(&mut self, now_ns: i64) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        // Rewrite when something changed, or when racy entries could become trusted.
        let racy = self
            .map
            .values()
            .any(|(stat, _)| stat.mtime_ns >= self.written_ns - RACY_NS);
        if !self.changed && !racy {
            return Ok(());
        }
        let mut entries: Vec<(&String, &(Stat, Sha256))> = self.map.iter().collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let mut body = Vec::with_capacity(8 + entries.len() * 120);
        body.extend_from_slice(&now_ns.to_le_bytes());
        for (path, (stat, sha)) in entries {
            body.extend_from_slice(&(path.len() as u32).to_le_bytes());
            body.extend_from_slice(path.as_bytes());
            body.extend_from_slice(&stat.size.to_le_bytes());
            body.extend_from_slice(&stat.mtime_ns.to_le_bytes());
            body.extend_from_slice(&stat.ctime_ns.to_le_bytes());
            body.extend_from_slice(&stat.inode.to_le_bytes());
            body.extend_from_slice(&stat.device.to_le_bytes());
            body.extend_from_slice(&stat.mode.to_le_bytes());
            body.extend_from_slice(sha);
        }
        write_atomic(&path, STAT_MAGIC, &body)?;
        self.written_ns = now_ns;
        self.changed = false;
        Ok(())
    }
}

fn take<const N: usize>(rest: &mut &[u8]) -> Option<[u8; N]> {
    if rest.len() < N {
        return None;
    }
    let (head, tail) = rest.split_at(N);
    *rest = tail;
    head.try_into().ok()
}

/// Writes `magic || body` to a temporary file next to `path`, then renames it into place.
fn write_atomic(path: &Path, magic: &[u8], body: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(magic)?;
    tmp.write_all(body)?;
    tmp.as_file().sync_all().ok();
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oid_cache_round_trips_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = OidCache::open(dir.path(), "sha1", 20);
        a.insert(&[1; 20], [7; 32], 5);
        a.save().unwrap();
        let mut b = OidCache::open(dir.path(), "sha1", 20);
        assert_eq!(b.get(&[1; 20]), Some(([7; 32], 5)));
        b.insert(&[2; 20], [8; 32], 6);
        // A concurrent writer added another record meanwhile.
        let mut c = OidCache::open(dir.path(), "sha1", 20);
        c.insert(&[3; 20], [9; 32], 7);
        c.save().unwrap();
        b.save().unwrap();
        let d = OidCache::open(dir.path(), "sha1", 20);
        assert_eq!(d.len(), 3);
        assert_eq!(d.get(&[3; 20]), Some(([9; 32], 7)));
        // Another object format is another file.
        assert!(OidCache::open(dir.path(), "sha256", 32).is_empty());
    }

    #[test]
    fn damaged_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oid-sha256/sha1.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"something else entirely").unwrap();
        assert!(OidCache::open(dir.path(), "sha1", 20).is_empty());
        let mut torn = OID_MAGIC.to_vec();
        torn.extend_from_slice(&[1; 20]);
        torn.extend_from_slice(&[7; 32]);
        torn.extend_from_slice(&5u64.to_le_bytes());
        torn.extend_from_slice(&[2; 10]);
        fs::write(&path, &torn).unwrap();
        let cache = OidCache::open(dir.path(), "sha1", 20);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn stat_cache_rehashes_racy_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("/repo");
        let stat = Stat {
            size: 3,
            mtime_ns: 10_000_000_000,
            ..Stat::default()
        };
        let mut cache = StatCache::open(dir.path(), root);
        cache.insert("a", stat, [1; 32]);
        // Written 1 s after the file changed: too close to trust.
        cache.save(11_000_000_000).unwrap();
        let cache = StatCache::open(dir.path(), root);
        assert_eq!(cache.get("a", &stat), None);
        let mut cache = StatCache::open(dir.path(), root);
        cache.insert("a", stat, [1; 32]);
        cache.save(20_000_000_000).unwrap();
        let cache = StatCache::open(dir.path(), root);
        assert_eq!(cache.get("a", &stat), Some([1; 32]));
        let changed = Stat { size: 4, ..stat };
        assert_eq!(cache.get("a", &changed), None);
    }
}
