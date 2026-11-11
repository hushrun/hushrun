//! Expiry (B-14): at most once a day, blocks not used for the kept days are deleted, and what
//! a crash left in `tmp/` and `trash/` with them.
//!
//! The pattern is `hushrun run`'s sweep of its local results: the stamp first, written whole,
//! then idempotent deletes. Every move of a block is [`Store`]'s; this module decides when and
//! what. A sweep never fails a run: errors are counted, not returned.

use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::store::{DAY, Store};

/// The file in the store's folder that says when a sweep last began: seconds and nanoseconds
/// since the Unix epoch, `<secs>.<nanos>`.
pub const SWEPT: &str = "swept";

/// What a sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Whether it ran: false when the store was swept less than a day before, or its stamp
    /// couldn't be written.
    pub swept: bool,
    /// Blocks expired: renamed into `trash/` and deleted there.
    pub blocks: u64,
    /// Entries more than a day old removed from `tmp/` and `trash/`.
    pub leftovers: u64,
    /// Hints removed for not being refreshed for the kept days.
    pub hints: u64,
    /// Steps that failed and were skipped. Another process removing the same thing first is
    /// not one.
    pub errors: u64,
}

/// Sweeps the store at `now` unless it was swept less than a day before (B-14). A stamp after
/// `now` is a clock set back: the store is swept by `now`.
pub fn sweep(store: &Store, now: SystemTime, keep_days: u32) -> SweepReport {
    if let Some(last) = read_stamp(store)
        && now.duration_since(last).is_ok_and(|since| since < DAY)
    {
        return SweepReport::default();
    }
    force(store, now, keep_days)
}

/// Sweeps the store at `now` whenever it was last swept, as `hushrun build clean --older`
/// does. The stamp is still written first, and without it nothing is deleted.
pub fn force(store: &Store, now: SystemTime, keep_days: u32) -> SweepReport {
    let mut report = SweepReport::default();
    // Without a stamp every run would sweep: none does.
    if write_stamp(store, now).is_err() {
        return report;
    }
    report.swept = true;
    let kept = Duration::from_secs(u64::from(keep_days) * DAY.as_secs());
    if let Some(cutoff) = now.checked_sub(kept) {
        for (_, dir) in store.stored() {
            match store.expire_unused(&dir, cutoff) {
                Ok(true) => report.blocks += 1,
                Ok(false) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => report.errors += 1,
            }
        }
        store.remove_empty_shards();
        let hints = store.remove_stale_hints(cutoff);
        report.hints = hints.removed;
        report.errors += hints.errors;
    }
    if let Some(before) = now.checked_sub(DAY) {
        let leftovers = store.remove_leftovers(before);
        report.leftovers = leftovers.removed;
        report.errors += leftovers.errors;
    }
    report
}

/// When the store was last swept, if it says.
fn read_stamp(store: &Store) -> Option<SystemTime> {
    let text = std::fs::read_to_string(store.root().join(SWEPT)).ok()?;
    let (secs, nanos) = text.trim().split_once('.')?;
    let since = Duration::new(secs.parse().ok()?, nanos.parse().ok()?);
    UNIX_EPOCH.checked_add(since)
}

fn write_stamp(store: &Store, now: SystemTime) -> io::Result<()> {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let text = format!("{}.{:09}\n", since.as_secs(), since.subsec_nanos());
    store.write_whole(&store.root().join(SWEPT), text.as_bytes(), None, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewFile, TMP, TRASH};
    use hushrun_core::Digest;
    use std::fs;
    use std::path::PathBuf;

    const KEEP: u32 = 15;

    /// Midnight UTC on day `n` after 2026-01-01.
    fn day(n: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_767_225_600 + n * 86_400)
    }

    fn hours(n: u64) -> Duration {
        Duration::from_secs(n * 3600)
    }

    fn key(n: u32) -> Digest {
        Digest::of(&n.to_le_bytes())
    }

    struct World {
        dir: tempfile::TempDir,
        store: Store,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().join("build"));
        World { dir, store }
    }

    impl World {
        fn publish(&self, n: u32, used: SystemTime) {
            let source = self.dir.path().join(format!("source-{n}"));
            fs::write(&source, n.to_le_bytes()).unwrap();
            let file = NewFile {
                name: "lib.rlib".into(),
                source,
            };
            let description = serde_json::json!({ "repository": "r" });
            self.store
                .publish(&key(n), &description, &[file], used)
                .unwrap();
        }

        fn has(&self, n: u32) -> bool {
            self.store.block(&key(n)).unwrap().is_some()
        }

        fn stamp(&self) -> Option<SystemTime> {
            read_stamp(&self.store)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.store.root().join(name)
        }
    }

    // covers: B-14
    #[test]
    fn a_block_used_on_day_14_survives_day_16_and_one_used_on_day_0_does_not() {
        let w = world();
        w.publish(1, day(0));
        w.publish(2, day(0));
        w.store.used(&key(2), day(14)).unwrap();
        w.publish(3, day(1));
        let report = sweep(&w.store, day(16), KEEP);
        assert!(report.swept);
        assert_eq!(report.blocks, 1);
        assert_eq!(report.errors, 0);
        assert!(!w.has(1));
        assert!(w.has(2));
        // Day 1 is exactly fifteen days before day 16: not more, so it stays.
        assert!(w.has(3));
        assert_eq!(fs::read_dir(w.path(TRASH)).unwrap().count(), 0);
    }

    // covers: B-14
    #[test]
    fn a_second_sweep_within_a_day_does_nothing() {
        let w = world();
        w.publish(1, day(0));
        assert_eq!(sweep(&w.store, day(16), KEEP).blocks, 1);
        assert_eq!(w.stamp(), Some(day(16)));
        w.publish(2, day(0));
        let again = sweep(&w.store, day(16) + hours(23), KEEP);
        assert_eq!(again, SweepReport::default());
        assert!(w.has(2));
        assert_eq!(w.stamp(), Some(day(16)));
        let next_day = sweep(&w.store, day(17), KEEP);
        assert!(next_day.swept);
        assert!(!w.has(2));
        assert_eq!(w.stamp(), Some(day(17)));
    }

    // covers: B-14
    #[test]
    fn a_clock_set_back_before_the_stamp_sweeps_by_its_own_time() {
        let w = world();
        sweep(&w.store, day(30), KEEP);
        assert_eq!(w.stamp(), Some(day(30)));
        w.publish(1, day(2));
        w.publish(2, day(10));
        let report = sweep(&w.store, day(20), KEEP);
        assert!(report.swept);
        assert!(!w.has(1), "eighteen days before day 20");
        assert!(w.has(2));
        assert_eq!(w.stamp(), Some(day(20)));
    }

    // covers: B-14
    #[test]
    fn without_a_stamp_written_nothing_is_deleted() {
        let w = world();
        w.publish(1, day(0));
        // A folder where the stamp goes: it can't be written.
        fs::create_dir_all(w.path(SWEPT).join("in the way")).unwrap();
        let report = sweep(&w.store, day(16), KEEP);
        assert!(!report.swept);
        assert!(w.has(1));
        assert_eq!(fs::read_dir(w.path(TMP)).unwrap().count(), 0);
    }

    // covers: B-14
    #[test]
    fn the_stamp_is_on_disk_even_when_no_block_can_be_removed() {
        let w = world();
        w.publish(1, day(0));
        // A file where `trash/` goes: no block can be moved into it.
        fs::write(w.path(TRASH), b"in the way").unwrap();
        let report = sweep(&w.store, day(16), KEEP);
        assert!(report.swept);
        assert_eq!(report.blocks, 0);
        assert_eq!(report.errors, 1);
        assert!(w.has(1));
        assert_eq!(w.stamp(), Some(day(16)));
        // Once it can, the next day's sweep removes it.
        fs::remove_file(w.path(TRASH)).unwrap();
        assert_eq!(sweep(&w.store, day(17), KEEP).blocks, 1);
        assert!(!w.has(1));
    }

    // covers: B-19
    #[test]
    fn a_folder_a_crash_left_in_tmp_is_removed_a_day_later() {
        let w = world();
        w.publish(1, day(0));
        // A publish killed between the copy and the rename.
        let left = w.path(TMP).join(format!("{}.1.0", key(9).hex()));
        fs::create_dir_all(left.join("files")).unwrap();
        fs::write(left.join("files").join("lib.rlib"), b"half").unwrap();
        fs::write(left.join("block.json"), b"{}").unwrap();
        assert!(w.store.block(&key(9)).unwrap().is_none());
        let made = fs::metadata(&left).unwrap().modified().unwrap();
        let early = force(&w.store, made + hours(23), KEEP);
        assert_eq!(early.leftovers, 0);
        assert!(left.exists());
        let later = force(&w.store, made + hours(25), KEEP);
        assert_eq!(later.leftovers, 1);
        assert!(!left.exists());
        // The stored block, last used long before, expired in the first sweep.
        assert!(!w.has(1));
    }

    // covers: B-14
    #[test]
    fn what_trash_holds_from_a_day_before_is_removed() {
        let w = world();
        let left = w.path(TRASH).join("abandoned.1.0");
        fs::create_dir_all(left.join("files")).unwrap();
        let made = fs::metadata(&left).unwrap().modified().unwrap();
        assert_eq!(force(&w.store, made + hours(1), KEEP).leftovers, 0);
        assert_eq!(force(&w.store, made + hours(25), KEEP).leftovers, 1);
        assert!(!left.exists());
    }

    // covers: B-7
    #[test]
    fn hints_not_refreshed_for_the_kept_days_are_removed_and_suspensions_stay() {
        let w = world();
        let (old, fresh, suspended) = (key(1), key(2), key(3));
        w.store.add_hint(&old, b"old", day(0)).unwrap();
        w.store.add_hint(&fresh, b"stale", day(0)).unwrap();
        w.store.add_hint(&fresh, b"fresh", day(10)).unwrap();
        w.store.add_hint(&suspended, b"old", day(0)).unwrap();
        w.store.suspend(&suspended, "audit", day(0)).unwrap();
        let report = sweep(&w.store, day(20), KEEP);
        assert_eq!(report.hints, 3);
        assert!(w.store.hints(&old).is_empty());
        assert_eq!(w.store.hints(&fresh), [b"fresh".to_vec()]);
        assert_eq!(w.store.suspended(&suspended).as_deref(), Some("audit"));
        // Folders left empty went; the suspended key's stays.
        let left: usize = fs::read_dir(w.path("steps"))
            .unwrap()
            .flatten()
            .map(|shard| fs::read_dir(shard.path()).unwrap().count())
            .sum();
        assert_eq!(left, 2);
    }

    // covers: B-14
    #[test]
    fn folders_left_empty_are_removed_and_two_sweeps_at_once_agree() {
        let w = world();
        for n in 0..40 {
            w.publish(n, day(0));
        }
        w.publish(100, day(16));
        let reports: Vec<SweepReport> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| force(&w.store, day(16), KEEP)))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(reports.iter().map(|r| r.blocks).sum::<u64>(), 40);
        assert_eq!(reports.iter().map(|r| r.errors).sum::<u64>(), 0);
        assert!(w.has(100));
        let shards = fs::read_dir(w.path("blocks")).unwrap().count();
        assert_eq!(shards, 1);
        assert_eq!(w.store.status(day(16)).blocks, 1);
    }

    // covers: B-14
    #[test]
    fn a_store_that_does_not_exist_sweeps_to_nothing() {
        let w = world();
        let report = sweep(&w.store, day(16), KEEP);
        assert!(report.swept);
        assert_eq!(
            report,
            SweepReport {
                swept: true,
                ..SweepReport::default()
            }
        );
    }
}
