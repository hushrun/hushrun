//! B-36: what `hushrun build fetch` leaves in the store for a sandbox's later runs, which may
//! have no network and no token: `prefetched/`, holding `saved.json` (the policy, the manifest,
//! the rows of the blocks fetched and their sealed hints, and `fetchedMs`, when it was fetched)
//! and `packs/<address>.pack`, each still sealed. No token or link is in it.
//!
//! Its states are the folder's: absent; fresh (fetched less than [`KEPT_FOR`] before the time it
//! is read at); stale, which nothing reads and the sweep removes. A fetch writes a new folder
//! whole beside it and renames it in, so a reader finds the old one or the new one, never half.
//! No run deletes it: a run links a pack into its own `incoming/<run>/` and opens that.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::store::{Store, Tally};

/// The folder, under the store's root.
pub const FOLDER: &str = "prefetched";
/// The file that describes it.
pub const SAVED: &str = "saved.json";
/// The packs' folder in it.
pub const PACKS: &str = "packs";
/// How long a fetch is good for.
pub const KEPT_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// The folder of a store.
pub fn folder(store: &Store) -> PathBuf {
    store.root().join(FOLDER)
}

/// When the store's fetch was made, as `saved.json` says, in milliseconds since the epoch.
pub fn fetched_ms(store: &Store) -> Option<u64> {
    let bytes = fs::read(folder(store).join(SAVED)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value["fetchedMs"].as_u64()
}

/// Whether a fetch made at `fetched_ms` is fresh at `now`: made before `now`, and less than
/// [`KEPT_FOR`] before it. One fetched after `now` is a clock set back, and isn't.
pub fn is_fresh(fetched_ms: u64, now: SystemTime) -> bool {
    let fetched = UNIX_EPOCH + Duration::from_millis(fetched_ms);
    now.duration_since(fetched)
        .is_ok_and(|since| since < KEPT_FOR)
}

/// The store's fetch folder when it is fresh at `now`.
pub fn fresh(store: &Store, now: SystemTime) -> Option<PathBuf> {
    fetched_ms(store)
        .filter(|ms| is_fresh(*ms, now))
        .map(|_| folder(store))
}

/// Writes a new fetch whole: `fill` writes into a folder of this process's beside the old one,
/// which is then renamed in. On any failure the old one stays as it was and the new one goes.
pub fn replace(store: &Store, fill: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
    let pid = std::process::id();
    let root = store.root();
    fs::create_dir_all(root)?;
    let new = root.join(format!("{FOLDER}.{pid}.tmp"));
    let old = root.join(format!("{FOLDER}.{pid}.old"));
    let _ = fs::remove_dir_all(&new);
    let _ = fs::remove_dir_all(&old);
    let written = fs::create_dir_all(new.join(PACKS)).and_then(|()| fill(&new));
    if let Err(err) = written {
        let _ = fs::remove_dir_all(&new);
        return Err(err);
    }
    let current = folder(store);
    let had = match fs::rename(&current, &old) {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => {
            let _ = fs::remove_dir_all(&new);
            return Err(err);
        }
    };
    if let Err(err) = fs::rename(&new, &current) {
        if had {
            let _ = fs::rename(&old, &current);
        }
        let _ = fs::remove_dir_all(&new);
        return Err(err);
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// The sweep's step (B-14, B-36): a fetch that isn't fresh at `now` goes, and so do the folders
/// a fetch whose process is gone left beside it.
pub fn sweep(store: &Store, now: SystemTime) -> Tally {
    let mut tally = Tally::default();
    let current = folder(store);
    if current.exists() && fetched_ms(store).is_none_or(|ms| !is_fresh(ms, now)) {
        match fs::remove_dir_all(&current) {
            Ok(()) => tally.removed += 1,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => tally.errors += 1,
        }
    }
    let Ok(entries) = fs::read_dir(store.root()) else {
        return tally;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&format!("{FOLDER}.")) else {
            continue;
        };
        let pid = rest
            .strip_suffix(".tmp")
            .or_else(|| rest.strip_suffix(".old"))
            .and_then(|pid| pid.parse::<i32>().ok());
        let Some(pid) = pid else { continue };
        if crate::outbox::process_alive(pid) {
            continue;
        }
        match fs::remove_dir_all(entry.path()) {
            Ok(()) => tally.removed += 1,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => tally.errors += 1,
        }
    }
    tally
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(dir: &Path, fetched_ms: u64) -> io::Result<()> {
        fs::write(
            dir.join(SAVED),
            format!("{{\"fetchedMs\":{fetched_ms}}}").as_bytes(),
        )?;
        fs::write(dir.join(PACKS).join("a.pack"), b"sealed")
    }

    // covers: B-36
    #[test]
    fn a_fetch_is_written_whole_good_for_a_day_and_swept_after() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().join("build"));
        let at = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let ms = 1_800_000_000_000;
        replace(&store, |new| saved(new, ms)).unwrap();
        assert_eq!(fetched_ms(&store), Some(ms));
        assert!(fresh(&store, at + Duration::from_secs(60)).is_some());
        assert!(fresh(&store, at + KEPT_FOR - Duration::from_secs(1)).is_some());
        assert!(fresh(&store, at + KEPT_FOR).is_none());
        // A clock set back: not fresh.
        assert!(fresh(&store, at - Duration::from_secs(1)).is_none());
        // A fetch that fails leaves the old one as it was.
        let failed = replace(&store, |_| Err(io::Error::other("network")));
        assert!(failed.is_err());
        assert_eq!(fetched_ms(&store), Some(ms));
        assert!(folder(&store).join(PACKS).join("a.pack").is_file());
        // A new one takes its place whole.
        replace(&store, |new| saved(new, ms + 1)).unwrap();
        assert_eq!(fetched_ms(&store), Some(ms + 1));
        // The sweep keeps a fresh one, and removes it once stale, with what a dead fetch left.
        let dead = store.root().join(format!("{FOLDER}.999999999.tmp"));
        fs::create_dir_all(&dead).unwrap();
        assert_eq!(sweep(&store, at + Duration::from_secs(60)).removed, 1);
        assert!(folder(&store).is_dir());
        assert!(!dead.exists());
        assert_eq!(
            sweep(&store, at + KEPT_FOR + Duration::from_secs(1)).removed,
            1
        );
        assert!(!folder(&store).exists());
    }
}
