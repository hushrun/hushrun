//! The person's own Go build cache, `GOCACHE`, read and written in Go's own format (F3, F9):
//! so that plain `go` and `go` inside `hushrun run` share one cache, `go clean -cache` clears
//! everything the program keeps here, and the first run inside `hushrun run` is as warm as plain
//! Go. Go trims its cache only when its own disk cache closes, which never happens while a cache
//! program serves it, so the program trims at `close` as Go would ([`GoCache::trim`]): the same
//! `trim.txt`, the same day between trims and the same five days unused.
//!
//! The format is `cmd/go/internal/cache`'s `DiskCache` (Go 1.10 to 1.27): for an action id `A`,
//! `<GOCACHE>/<first byte of A, hex>/<A hex>-a` holds one line,
//! `v1 <A hex> <output id hex> <size, padded to 20> <unix nanoseconds, padded to 20>\n`, and the
//! body is `<first byte of O>/<O hex>-d`, where the output id `O` is the body's SHA-256. Only a
//! Go version whose format this is gets the program at all ([`super::command::version_known`]).
//!
//! Every file is written whole, under a temporary name in the same folder, then renamed: a body
//! Go holds a path to is never rewritten in place (F8), and a body is named by its SHA-256, so a
//! rename over it puts the same bytes there. The temporary name ends in `-d`, so a file a crash
//! left is trimmed by Go itself after five days.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The length of an entry's line: `v1`, the two ids, the size and the time, with their spaces.
const ENTRY: usize = 2 + 1 + 64 + 1 + 64 + 1 + 20 + 1 + 20 + 1;
/// Go sets a used file's date at most once in this long (`mtimeInterval`).
const MTIME_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Go trims at most once in this long (`trimInterval`).
const TRIM_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Go removes an entry unused this long (`trimLimit`), and an hour more for its dates' slack.
const TRIM_LIMIT: Duration = Duration::from_secs(5 * 24 * 60 * 60);
/// The file the time of the last trim is kept in, seconds since the epoch, as Go writes it.
const TRIM_FILE: &str = "trim.txt";

/// An entry Go's cache holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The body's SHA-256.
    pub output: [u8; 32],
    /// Its size.
    pub size: u64,
    /// When it was put, in nanoseconds since the epoch: what `go clean -testcache` expires by.
    pub time_ns: i64,
    /// The body's file.
    pub path: PathBuf,
}

/// A `GOCACHE` folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoCache {
    dir: PathBuf,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn unhex(text: &[u8]) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in text.chunks(2).enumerate() {
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        out[i] = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(out)
}

impl GoCache {
    /// The cache in `dir`, which the `go` command made and exports as `GOCACHE`.
    pub fn new(dir: &Path) -> GoCache {
        GoCache {
            dir: dir.to_path_buf(),
        }
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, id: &[u8; 32], suffix: &str) -> PathBuf {
        self.dir
            .join(format!("{:02x}", id[0]))
            .join(format!("{}-{suffix}", hex(id)))
    }

    /// The body file of output id `output`.
    pub fn body_path(&self, output: &[u8; 32]) -> PathBuf {
        self.file(output, "d")
    }

    /// The entry under `id`, as `DiskCache.get` and `GetFile` would find it: a well-formed line
    /// naming `id`, and a body file of the size it says. Both files' dates are moved to now when
    /// older than an hour, as Go's `markUsed` does, so Go's trim keeps what is used. Anything
    /// else is no entry.
    pub fn get(&self, id: &[u8; 32], now: SystemTime) -> Option<Hit> {
        let entry = self.file(id, "a");
        let mut line = Vec::with_capacity(ENTRY + 1);
        File::open(&entry)
            .ok()?
            .take(ENTRY as u64 + 1)
            .read_to_end(&mut line)
            .ok()?;
        if line.len() != ENTRY
            || !line.starts_with(b"v1 ")
            || line[3 + 64] != b' '
            || line[3 + 64 + 1 + 64] != b' '
            || line[3 + 64 + 1 + 64 + 1 + 20] != b' '
            || line[ENTRY - 1] != b'\n'
        {
            return None;
        }
        if unhex(&line[3..3 + 64])? != *id {
            return None;
        }
        let output = unhex(&line[3 + 65..3 + 65 + 64])?;
        let number = |field: &[u8]| -> Option<i64> {
            std::str::from_utf8(field).ok()?.trim_start().parse().ok()
        };
        let size = number(&line[3 + 130..3 + 130 + 20])?;
        let time_ns = number(&line[3 + 151..3 + 151 + 20])?;
        let size = u64::try_from(size).ok()?;
        if time_ns < 0 {
            return None;
        }
        let path = self.body_path(&output);
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() || meta.len() != size {
            return None;
        }
        mark_used(&entry, now);
        mark_used(&path, now);
        Some(Hit {
            output,
            size,
            time_ns,
            path,
        })
    }

    /// Puts `body`, whose SHA-256 is `output`, under `id` as Go's `DiskCache.Put` does: the body
    /// first, then the entry dated `now`. The body's file, which Go is handed.
    pub fn put(
        &self,
        id: &[u8; 32],
        output: &[u8; 32],
        body: &[u8],
        now: SystemTime,
    ) -> io::Result<PathBuf> {
        let path = self.body_path(output);
        write_whole(&path, body, now)?;
        let ns = now
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX));
        let line = format!(
            "v1 {} {} {:>20} {:>20}\n",
            hex(id),
            hex(output),
            body.len(),
            ns
        );
        write_whole(&self.file(id, "a"), line.as_bytes(), now)?;
        Ok(path)
    }
}

impl GoCache {
    /// Trims the cache as Go's `DiskCache.Trim` does: unless `trim.txt` names a trim less than a
    /// day before `now` (and not more than an hour after it), it writes `now` there, under the
    /// file's lock as Go's `lockedfile` takes it, and removes every `-a` and `-d` entry of the
    /// 256 shards last used more than five days and an hour before. Whether it trimmed.
    pub fn trim(&self, now: SystemTime) -> bool {
        let path = self.dir.join(TRIM_FILE);
        let skip = |text: &str| {
            let Ok(secs) = text.trim().parse::<i64>() else {
                return false;
            };
            let now_secs = now
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            let since = now_secs - secs;
            since < TRIM_INTERVAL.as_secs() as i64 && since > -(MTIME_INTERVAL.as_secs() as i64)
        };
        // The common case, read without the exclusive lock, as Go reads it.
        if fs::read_to_string(&path).is_ok_and(|text| skip(&text)) {
            return false;
        }
        let Ok(mut file) = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
        else {
            return false;
        };
        if file.lock().is_err() {
            return false;
        }
        let mut text = String::new();
        let _ = file.read_to_string(&mut text);
        if skip(&text) {
            return false;
        }
        let stamp = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let written = file
            .set_len(0)
            .and_then(|()| io::Seek::seek(&mut file, io::SeekFrom::Start(0)))
            .and_then(|_| file.write_all(stamp.to_string().as_bytes()));
        let _ = file.unlock();
        if written.is_err() {
            return false;
        }
        let Some(cutoff) = now.checked_sub(TRIM_LIMIT + MTIME_INTERVAL) else {
            return true;
        };
        for shard in 0..256 {
            let dir = self.dir.join(format!("{shard:02x}"));
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            let names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            for entry in names {
                let name = entry.file_name().map(|n| n.to_string_lossy().into_owned());
                if !name.is_some_and(|n| n.ends_with("-a") || n.ends_with("-d")) {
                    continue;
                }
                let Ok(meta) = fs::metadata(&entry) else {
                    continue;
                };
                if meta.modified().is_ok_and(|m| m < cutoff) {
                    let _ = if meta.is_dir() {
                        fs::remove_dir_all(&entry)
                    } else {
                        fs::remove_file(&entry)
                    };
                }
            }
        }
        true
    }
}

/// Moves a file's date to `now` when it is older than Go's interval, as `markUsed` does. A file
/// that can't be touched is left as it is.
fn mark_used(path: &Path, now: SystemTime) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let old = meta
        .modified()
        .ok()
        .and_then(|m| now.duration_since(m).ok())
        .is_some_and(|age| age >= MTIME_INTERVAL);
    if old && let Ok(file) = OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(now);
    }
}

/// Writes `bytes` to `path` whole: a temporary file beside it, then a rename, so nobody ever
/// reads a part of it and a file someone holds is never changed under them.
pub(crate) fn write_whole(path: &Path, bytes: &[u8], now: SystemTime) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().ok_or_else(|| io::Error::other("no folder"))?;
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(
        "hushrun-{}-{}-d",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.set_modified(now)?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-41
    #[test]
    fn what_the_program_puts_plain_go_reads_and_the_other_way_round() {
        let dir = tempfile::tempdir().unwrap();
        let cache = GoCache::new(dir.path());
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let id = [0xabu8; 32];
        let body = b"a body";
        let output = hushrun_core::Digest::of(body);
        let path = cache.put(&id, output.as_bytes(), body, now).unwrap();
        assert_eq!(fs::read(&path).unwrap(), body);
        // The line is Go's, byte for byte (`putIndexEntry`'s format).
        let line =
            fs::read_to_string(dir.path().join("ab").join(format!("{}-a", hex(&id)))).unwrap();
        assert_eq!(
            line,
            format!(
                "v1 {} {} {:>20} {:>20}\n",
                hex(&id),
                output.hex(),
                6,
                1_800_000_000_000_000_000i64
            )
        );
        let hit = cache.get(&id, now).unwrap();
        assert_eq!(hit.output, *output.as_bytes());
        assert_eq!(hit.size, 6);
        assert_eq!(hit.time_ns, 1_800_000_000_000_000_000);
        assert_eq!(hit.path, path);
        // No temporary file is left.
        let names: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with("hushrun-")),
            "{names:?}"
        );
    }

    // covers: B-41
    #[test]
    fn an_entry_go_wouldnt_read_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = GoCache::new(dir.path());
        let now = SystemTime::now();
        let id = [1u8; 32];
        let output = hushrun_core::Digest::of(b"xyz");
        cache.put(&id, output.as_bytes(), b"xyz", now).unwrap();
        // A body of another size than the line says: Go's `GetFile` says incomplete.
        fs::write(cache.body_path(output.as_bytes()), b"xy").unwrap();
        assert_eq!(cache.get(&id, now), None);
        // A line for another id.
        let other = [2u8; 32];
        let entry = cache.file(&other, "a");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::copy(cache.file(&id, "a"), &entry).unwrap();
        assert_eq!(cache.get(&other, now), None);
        // No entry at all.
        assert_eq!(cache.get(&[3u8; 32], now), None);
    }

    // covers: B-41
    #[test]
    fn the_cache_is_trimmed_as_go_trims_it_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let cache = GoCache::new(dir.path());
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let day = Duration::from_secs(86_400);
        let old_id = [0x10u8; 32];
        let new_id = [0x20u8; 32];
        let old = cache
            .put(&old_id, &[0x11; 32], b"old", now - day * 10)
            .unwrap();
        let fresh = cache.put(&new_id, &[0x21; 32], b"new", now - day).unwrap();
        // Not an entry: left alone whatever its age.
        let other = dir.path().join("10").join("README");
        fs::write(&other, b"x").unwrap();
        File::options()
            .write(true)
            .open(&other)
            .unwrap()
            .set_modified(now - day * 30)
            .unwrap();
        assert!(cache.trim(now));
        assert!(!old.exists() && !cache.file(&old_id, "a").exists());
        assert!(fresh.exists() && cache.file(&new_id, "a").exists());
        assert!(other.exists());
        // Go's own file and format: the time in seconds.
        assert_eq!(
            fs::read_to_string(dir.path().join(TRIM_FILE)).unwrap(),
            "1800000000"
        );
        // Within a day, not again; a day later, again.
        assert!(!cache.trim(now + day / 2));
        assert!(cache.trim(now + day * 2));
        // A file Go can't read is trimmed over.
        fs::write(dir.path().join(TRIM_FILE), b"nonsense").unwrap();
        assert!(cache.trim(now + day * 2));
    }

    // covers: B-41
    #[test]
    fn a_used_entry_is_dated_now_once_an_hour_has_passed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = GoCache::new(dir.path());
        let then = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let id = [9u8; 32];
        let output = hushrun_core::Digest::of(b"b");
        let path = cache.put(&id, output.as_bytes(), b"b", then).unwrap();
        let soon = then + Duration::from_secs(60);
        cache.get(&id, soon).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), then);
        let later = then + Duration::from_secs(2 * 60 * 60);
        cache.get(&id, later).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), later);
    }
}
