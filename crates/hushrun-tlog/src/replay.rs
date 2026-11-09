//! A monitor's replay of a log (L-11): the tree, and which keys were live at every size, so
//! that every checkpoint and every listing statement the ledger signed can be checked against
//! what its own log says. \[M19\] It also rebuilds the prefix map (L-12) and checks the root
//! every map leaf records: the map must follow from the log.

use std::collections::BTreeMap;
use std::ops::Bound;

use crate::checkpoint::Checkpoint;
use crate::entry::{Entry, EntryKind};
use crate::hash::Hash;
use crate::listing::{Listing, keys_hash};
use crate::map::MapBuilder;
use crate::tree::MemTree;

/// Something in the log that a ledger keeping L-6 and L-9 never writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anomaly {
    /// An object created under a key that was live: an overwrite, which creates never do.
    Overwrite { index: u64, key: String },
    /// A deletion of a key that wasn't live.
    DeleteMissing { index: u64, key: String },
    /// A deletion whose hash isn't that of the live object.
    DeleteMismatch { index: u64, key: String, live: Hash },
    /// \[M19\] A map leaf whose root isn't the map of the leaves before it (L-12): the ledger
    /// committed a map that doesn't follow from its own log.
    MapMismatch {
        index: u64,
        recorded: Hash,
        computed: Hash,
    },
}

impl Anomaly {
    pub fn index(&self) -> u64 {
        match self {
            Anomaly::Overwrite { index, .. }
            | Anomaly::DeleteMissing { index, .. }
            | Anomaly::DeleteMismatch { index, .. }
            | Anomaly::MapMismatch { index, .. } => *index,
        }
    }
}

/// How a checkpoint compares with the replayed log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointStatus {
    /// The log's root at that size is the checkpoint's.
    Matches,
    /// The checkpoint is of a larger tree than the log replayed so far.
    Beyond { replayed: u64 },
    /// The log's root at that size is another one: two histories.
    Differs { replayed: Hash },
}

/// How a listing statement compares with the replayed log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingCheck {
    pub status: ListingStatus,
    /// The keys the log held in the page's range at the page's size, with the index of the
    /// leaf that created each.
    pub log_keys: Vec<(String, u64)>,
    /// Keys the log held that the page left out. Known only from the keys the reader
    /// recorded; empty otherwise.
    pub omitted: Vec<(String, u64)>,
    /// Keys on the page that the log didn't hold.
    pub extra: Vec<String>,
    /// Whether the recorded keys are the ones the statement hashed, when there are any.
    pub recorded_match: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingStatus {
    /// The page is exactly what the log held.
    Complete,
    /// The page differs from what the log held: keys left out, or keys the log didn't have.
    Incomplete,
    /// The statement's checkpoint is beyond the replayed log.
    Beyond,
    /// The statement's root isn't the log's root at its size.
    Differs,
}

/// The replay of a log, leaf by leaf.
#[derive(Debug, Clone, Default)]
pub struct Replay {
    tree: MemTree,
    entries: Vec<Entry>,
    /// Each object key's history: the index of each creation and deletion, and whether the
    /// key was live after it.
    keys: BTreeMap<String, Vec<(u64, bool)>>,
    anomalies: Vec<Anomaly>,
    /// \[M19\] The prefix map of the leaves so far, and the map leaves checked against it.
    map: MapBuilder,
    maps: u64,
    last_map: Option<u64>,
}

impl Replay {
    pub fn new() -> Replay {
        Replay::default()
    }

    /// Appends the next entry. Returns its leaf hash.
    pub fn push(&mut self, entry: Entry) -> Hash {
        let index = self.size();
        let leaf = entry.leaf_hash();
        self.tree.push(leaf);
        self.map.apply(&entry);
        match entry.kind {
            EntryKind::Object => {
                if self.live(&entry.name).is_some() {
                    self.anomalies.push(Anomaly::Overwrite {
                        index,
                        key: entry.name.clone(),
                    });
                }
                self.keys
                    .entry(entry.name.clone())
                    .or_default()
                    .push((index, true));
            }
            EntryKind::Tombstone => {
                match self.live(&entry.name) {
                    None => self.anomalies.push(Anomaly::DeleteMissing {
                        index,
                        key: entry.name.clone(),
                    }),
                    Some(created) => {
                        let live = self.entries[created as usize].sha256;
                        if live != entry.sha256 {
                            self.anomalies.push(Anomaly::DeleteMismatch {
                                index,
                                key: entry.name.clone(),
                                live,
                            });
                        }
                    }
                }
                self.keys
                    .entry(entry.name.clone())
                    .or_default()
                    .push((index, false));
            }
            EntryKind::Map => {
                let computed = self.map.commit();
                self.maps += 1;
                self.last_map = Some(index);
                if computed != entry.sha256 {
                    self.anomalies.push(Anomaly::MapMismatch {
                        index,
                        recorded: entry.sha256,
                        computed,
                    });
                }
            }
            EntryKind::Config => {}
        }
        self.entries.push(entry);
        leaf
    }

    /// How many leaves were replayed.
    pub fn size(&self) -> u64 {
        self.tree.size()
    }

    pub fn tree(&self) -> &MemTree {
        &self.tree
    }

    pub fn entry(&self, index: u64) -> Option<&Entry> {
        self.entries.get(usize::try_from(index).ok()?)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// What a ledger keeping L-6 and L-9 never writes, in log order.
    pub fn anomalies(&self) -> &[Anomaly] {
        &self.anomalies
    }

    /// \[M19\] How many map leaves were checked against the replayed map.
    pub fn maps(&self) -> u64 {
        self.maps
    }

    /// \[M19\] The index of the last map leaf, if any.
    pub fn last_map(&self) -> Option<u64> {
        self.last_map
    }

    /// \[M19\] The prefix map of every leaf replayed, as of the last map leaf.
    pub fn map(&self) -> &crate::map::Map {
        self.map.map()
    }

    /// The root of the log's first `size` leaves.
    pub fn root_at(&self, size: u64) -> Option<Hash> {
        self.tree.root_at(size)
    }

    /// The index of the creation that makes `key` live after the whole replay, if it is.
    pub fn live(&self, key: &str) -> Option<u64> {
        self.live_at(key, self.size())
    }

    /// The index of the creation that makes `key` live in the tree of `size` leaves, if it is.
    pub fn live_at(&self, key: &str, size: u64) -> Option<u64> {
        let history = self.keys.get(key)?;
        let last = history.iter().rev().find(|(index, _)| *index < size)?;
        last.1.then_some(last.0)
    }

    /// The keys live in the tree of `size` leaves that `listing`'s range covers, in byte
    /// order, with the index of the leaf that created each.
    pub fn keys_in(&self, listing: &Listing, size: u64) -> Vec<(String, u64)> {
        let lower = match &listing.after {
            Some(after) if after.as_str() >= listing.prefix.as_str() => {
                Bound::Excluded(after.clone())
            }
            _ => Bound::Included(listing.prefix.clone()),
        };
        self.keys
            .range::<String, _>((lower, Bound::Unbounded))
            .take_while(|(key, _)| key.starts_with(listing.prefix.as_str()))
            .filter(|(key, _)| listing.covers(key))
            .filter_map(|(key, _)| self.live_at(key, size).map(|index| (key.clone(), index)))
            .collect()
    }

    /// Compares a checkpoint with the log.
    pub fn check_checkpoint(&self, checkpoint: &Checkpoint) -> CheckpointStatus {
        match self.root_at(checkpoint.size) {
            None => CheckpointStatus::Beyond {
                replayed: self.size(),
            },
            Some(root) if root == checkpoint.root => CheckpointStatus::Matches,
            Some(root) => CheckpointStatus::Differs { replayed: root },
        }
    }

    /// Recomputes a listing page at its checkpoint and compares it with the statement, and
    /// with the keys a reader recorded for it, when there are any.
    pub fn check_listing(&self, listing: &Listing, recorded: Option<&[String]>) -> ListingCheck {
        let mut check = ListingCheck {
            status: ListingStatus::Complete,
            log_keys: Vec::new(),
            omitted: Vec::new(),
            extra: Vec::new(),
            recorded_match: recorded.map(|keys| listing.matches(keys)),
        };
        match self.check_checkpoint(&listing.checkpoint()) {
            CheckpointStatus::Beyond { .. } => {
                check.status = ListingStatus::Beyond;
                return check;
            }
            CheckpointStatus::Differs { .. } => {
                check.status = ListingStatus::Differs;
                return check;
            }
            CheckpointStatus::Matches => {}
        }
        check.log_keys = self.keys_in(listing, listing.size);
        let log_names: Vec<&str> = check.log_keys.iter().map(|(k, _)| k.as_str()).collect();
        if keys_hash(&log_names) == listing.keys && log_names.len() as u64 == listing.count {
            return check;
        }
        check.status = ListingStatus::Incomplete;
        if let (Some(keys), Some(true)) = (recorded, check.recorded_match) {
            check.omitted = check
                .log_keys
                .iter()
                .filter(|(key, _)| !keys.contains(key))
                .cloned()
                .collect();
            check.extra = keys
                .iter()
                .filter(|key| !log_names.contains(&key.as_str()))
                .cloned()
                .collect();
        }
        check
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "hushrun.example/l/acme/back";

    fn replay(entries: &[Entry]) -> Replay {
        let mut replay = Replay::new();
        for entry in entries {
            replay.push(entry.clone());
        }
        replay
    }

    fn checkpoint(replay: &Replay, size: u64) -> Checkpoint {
        Checkpoint::new(ORIGIN, size, replay.root_at(size).unwrap())
    }

    // covers: L-11
    #[test]
    fn keys_live_and_die_with_their_entries() {
        let r = replay(&[
            Entry::config("trust.yaml", b"t"),
            Entry::object("v1/e/a", b"a"),
            Entry::object("v1/e/b", b"b"),
            Entry::tombstone("v1/e/a", Hash::of(b"a"), Some(Hash::of(b"gc"))),
            Entry::object("v1/e/a", b"a2"),
        ]);
        assert_eq!(r.live_at("v1/e/a", 1), None);
        assert_eq!(r.live_at("v1/e/a", 2), Some(1));
        assert_eq!(r.live_at("v1/e/a", 4), None);
        assert_eq!(r.live_at("v1/e/a", 5), Some(4));
        assert_eq!(r.live("v1/e/b"), Some(2));
        assert!(r.anomalies().is_empty());
        let listing = Listing::new(
            &checkpoint(&r, 3),
            "v1/e/",
            None,
            None,
            &["v1/e/a", "v1/e/b"],
        )
        .unwrap();
        assert_eq!(
            r.keys_in(&listing, 3),
            [("v1/e/a".to_string(), 1), ("v1/e/b".to_string(), 2)]
        );
        assert_eq!(r.keys_in(&listing, 4), [("v1/e/b".to_string(), 2)]);
    }

    #[test]
    fn anomalies_are_found() {
        let r = replay(&[
            Entry::object("v1/e/a", b"a"),
            Entry::object("v1/e/a", b"a again"),
            Entry::tombstone("v1/e/z", Hash::of(b"z"), None),
            Entry::tombstone("v1/e/a", Hash::of(b"wrong"), None),
            Entry::config("inventory/ci.txt", b"i1"),
            Entry::config("inventory/ci.txt", b"i2"),
        ]);
        assert_eq!(
            r.anomalies(),
            [
                Anomaly::Overwrite {
                    index: 1,
                    key: "v1/e/a".into()
                },
                Anomaly::DeleteMissing {
                    index: 2,
                    key: "v1/e/z".into()
                },
                Anomaly::DeleteMismatch {
                    index: 3,
                    key: "v1/e/a".into(),
                    live: Hash::of(b"a again")
                },
            ]
        );
    }

    // covers: L-11
    #[test]
    fn a_listing_that_left_out_a_key_is_found() {
        let r = replay(&[
            Entry::object("v1/e/d/1.started.staging.json", b"1"),
            Entry::object("v1/e/d/2.failed.staging.json", b"2"),
            Entry::object("v1/e/d/3.started.production.json", b"3"),
            Entry::object("v1/e/other", b"4"),
        ]);
        let cp = checkpoint(&r, 3);
        let honest = [
            "v1/e/d/1.started.staging.json".to_string(),
            "v1/e/d/2.failed.staging.json".to_string(),
            "v1/e/d/3.started.production.json".to_string(),
        ];
        let listing = Listing::new(&cp, "v1/e/d/", None, None, &honest).unwrap();
        let check = r.check_listing(&listing, Some(&honest));
        assert_eq!(check.status, ListingStatus::Complete);
        assert_eq!(check.recorded_match, Some(true));

        // The ledger hid the failure, and signed for the rest.
        let shown = vec![honest[0].clone(), honest[2].clone()];
        let lie = Listing::new(&cp, "v1/e/d/", None, None, &shown).unwrap();
        let check = r.check_listing(&lie, Some(&shown));
        assert_eq!(check.status, ListingStatus::Incomplete);
        assert_eq!(check.omitted, [(honest[1].clone(), 1)]);
        assert!(check.extra.is_empty());
        // Without the recorded keys, the statement alone still contradicts the log.
        let check = r.check_listing(&lie, None);
        assert_eq!(check.status, ListingStatus::Incomplete);
        assert_eq!(check.log_keys.len(), 3);
        assert!(check.omitted.is_empty());

        // A key the log didn't hold yet at that size.
        let early = Listing::new(&checkpoint(&r, 2), "v1/e/d/", None, None, &honest).unwrap();
        let check = r.check_listing(&early, Some(&honest));
        assert_eq!(check.status, ListingStatus::Incomplete);
        assert_eq!(check.extra, [honest[2].clone()]);
    }

    #[test]
    fn pages_are_checked_in_their_range() {
        let keys: Vec<String> = (0..10).map(|i| format!("v1/d/s/{i:02}")).collect();
        let r = replay(
            &keys
                .iter()
                .map(|k| Entry::object(k, k.as_bytes()))
                .collect::<Vec<_>>(),
        );
        let cp = checkpoint(&r, 10);
        let first = Listing::new(&cp, "v1/d/s/", None, Some(&keys[3]), &keys[..4]).unwrap();
        assert_eq!(
            r.check_listing(&first, Some(&keys[..4])).status,
            ListingStatus::Complete
        );
        let second = Listing::new(&cp, "v1/d/s/", Some(&keys[3]), None, &keys[4..]).unwrap();
        assert_eq!(
            r.check_listing(&second, Some(&keys[4..])).status,
            ListingStatus::Complete
        );
        // A page that ends early without saying more keys follow.
        let short = Listing::new(&cp, "v1/d/s/", None, None, &keys[..4]).unwrap();
        let check = r.check_listing(&short, Some(&keys[..4]));
        assert_eq!(check.status, ListingStatus::Incomplete);
        assert_eq!(check.omitted.len(), 6);
    }

    // covers: L-12
    #[test]
    fn map_leaves_must_follow_from_the_log() {
        let digest = "a".repeat(64);
        let key = |n: u32| format!("v1/events/api/{digest}/{n:03}.check._.json");
        let mut honest = Replay::new();
        honest.push(Entry::object(&key(1), b"1"));
        honest.push(Entry::object(&key(2), b"2"));
        let mut builder = crate::map::MapBuilder::new();
        builder.insert(&key(1));
        builder.insert(&key(2));
        let root = builder.commit();
        honest.push(Entry::map(root));
        assert!(honest.anomalies().is_empty());
        assert_eq!(honest.maps(), 1);
        assert_eq!(honest.last_map(), Some(2));
        assert_eq!(honest.map().root(), root);
        // A map that leaves the second key out.
        let mut lying = Replay::new();
        lying.push(Entry::object(&key(1), b"1"));
        lying.push(Entry::object(&key(2), b"2"));
        let mut short = crate::map::MapBuilder::new();
        short.insert(&key(1));
        let lie = short.commit();
        lying.push(Entry::map(lie));
        assert_eq!(
            lying.anomalies(),
            [Anomaly::MapMismatch {
                index: 2,
                recorded: lie,
                computed: root
            }]
        );
    }

    #[test]
    fn statements_beyond_or_beside_the_log() {
        let r = replay(&[Entry::object("v1/a", b"a"), Entry::object("v1/b", b"b")]);
        let beyond = Checkpoint::new(ORIGIN, 3, Hash::leaf(b"x"));
        assert_eq!(
            r.check_checkpoint(&beyond),
            CheckpointStatus::Beyond { replayed: 2 }
        );
        let listing = Listing::new(&beyond, "v1/", None, None, &["v1/a"]).unwrap();
        assert_eq!(
            r.check_listing(&listing, None).status,
            ListingStatus::Beyond
        );
        let other = Checkpoint::new(ORIGIN, 2, Hash::leaf(b"x"));
        assert_eq!(
            r.check_checkpoint(&other),
            CheckpointStatus::Differs {
                replayed: r.root_at(2).unwrap()
            }
        );
        let listing = Listing::new(&other, "v1/", None, None, &["v1/a", "v1/b"]).unwrap();
        assert_eq!(
            r.check_listing(&listing, None).status,
            ListingStatus::Differs
        );
        assert_eq!(
            r.check_checkpoint(&checkpoint(&r, 1)),
            CheckpointStatus::Matches
        );
    }
}
