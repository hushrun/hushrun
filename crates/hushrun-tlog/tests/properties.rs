//! Property tests: proofs from any tree verify, and nothing else does; a store of complete
//! nodes answers exactly what the whole tree answers; replays find every hidden key.

use std::collections::BTreeMap;

use proptest::prelude::*;

use hushrun_tlog::checkpoint::Checkpoint;
use hushrun_tlog::entry::Entry;
use hushrun_tlog::listing::Listing;
use hushrun_tlog::map::{self, Map, MapBuilder, MapEntry, PageProof, verify_page};
use hushrun_tlog::proof::{root_from_inclusion, verify_consistency, verify_inclusion};
use hushrun_tlog::replay::ListingStatus;
use hushrun_tlog::tree::{self, NodeId};
use hushrun_tlog::{CompactRange, Hash, MemTree, Replay};

fn leaves(n: u64, salt: u8) -> Vec<Hash> {
    (0..n)
        .map(|i| {
            let mut data = i.to_le_bytes().to_vec();
            data.push(salt);
            Hash::leaf(&data)
        })
        .collect()
}

/// RFC 6962's recursive tree hash, as the reference.
fn mth(leaves: &[Hash]) -> Hash {
    match leaves.len() {
        0 => Hash::empty(),
        1 => leaves[0],
        n => {
            let mut k = 1;
            while k * 2 < n {
                k *= 2;
            }
            Hash::node(&mth(&leaves[..k]), &mth(&leaves[k..]))
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // covers: L-11
    #[test]
    fn inclusion_proofs_verify_only_for_their_leaf(n in 1u64..300, pick in any::<u64>(), salt in any::<u8>()) {
        let leaves = leaves(n, salt);
        let tree = MemTree::from_leaves(leaves.iter().copied());
        let size = 1 + pick % n;
        let index = (pick / 7) % size;
        let root = tree.root_at(size).unwrap();
        prop_assert_eq!(root, mth(&leaves[..size as usize]));
        let proof = tree.inclusion_proof(index, size).unwrap();
        prop_assert!(verify_inclusion(&leaves[index as usize], index, size, &proof, &root).is_ok());
        if size > 1 {
            let other = (index + 1) % size;
            prop_assert!(verify_inclusion(&leaves[other as usize], index, size, &proof, &root).is_err());
            prop_assert!(verify_inclusion(&leaves[index as usize], other, size, &proof, &root).is_err());
        }
        // A larger tree's root, for the same proof, never matches.
        if size < n {
            let bigger = tree.root_at(size + 1).unwrap();
            let led = root_from_inclusion(&leaves[index as usize], index, size, &proof).unwrap();
            prop_assert_ne!(led, bigger);
        }
    }

    // covers: L-11
    #[test]
    fn consistency_proofs_verify_only_between_their_trees(n in 1u64..300, a in any::<u64>(), b in any::<u64>(), salt in any::<u8>()) {
        let leaves = leaves(n, salt);
        let tree = MemTree::from_leaves(leaves.iter().copied());
        let new = 1 + a % n;
        let old = b % (new + 1);
        let proof = tree.consistency_proof(old, new).unwrap();
        let old_root = tree.root_at(old).unwrap();
        let new_root = tree.root_at(new).unwrap();
        prop_assert!(verify_consistency(old, new, &proof, &old_root, &new_root).is_ok());
        // Another history with the same sizes: the last leaf of the old tree changed.
        if old > 0 && old < new {
            let mut forked = leaves.clone();
            forked[(old - 1) as usize] = Hash::leaf(b"rewritten");
            let other = MemTree::from_leaves(forked.iter().copied());
            let other_proof = other.consistency_proof(old, new).unwrap();
            prop_assert!(verify_consistency(old, new, &other_proof, &old_root, &other.root_at(new).unwrap()).is_err());
            prop_assert!(verify_consistency(old, new, &proof, &other.root_at(old).unwrap(), &new_root).is_err());
        }
    }

    /// A store of complete nodes, filled from compact-range appends as the server fills it,
    /// answers every root and proof as the whole tree does.
    // covers: L-11
    #[test]
    fn a_node_store_answers_like_the_tree(n in 0u64..200, batches in prop::collection::vec(1usize..9, 1..40), salt in any::<u8>()) {
        let leaves = leaves(n, salt);
        let tree = MemTree::from_leaves(leaves.iter().copied());
        let mut store: BTreeMap<NodeId, Hash> = BTreeMap::new();
        let mut compact = CompactRange::new();
        let mut next = 0usize;
        for batch in batches.iter().cycle() {
            if next >= leaves.len() {
                break;
            }
            // Reload the right edge from the store, as a new sequencing batch does.
            let ids = CompactRange::ids(compact.size());
            let reloaded = CompactRange::from_parts(compact.size(), ids.iter().map(|id| store[id]).collect()).unwrap();
            prop_assert_eq!(&reloaded, &compact);
            for leaf in leaves.iter().skip(next).take(*batch) {
                for (id, hash) in compact.append(*leaf) {
                    store.insert(id, hash);
                }
            }
            next = (next + batch).min(leaves.len());
        }
        prop_assert_eq!(compact.root(), tree.root());
        let lookup = |id: NodeId| store.get(&id).copied();
        for size in [0, n / 3, n / 2, n] {
            prop_assert_eq!(tree::root(size, lookup), tree.root_at(size));
            if size > 0 {
                let index = size / 2;
                let ranges = tree::inclusion_ranges(index, size).unwrap();
                prop_assert_eq!(tree::assemble(&ranges, lookup), tree.inclusion_proof(index, size));
                let ranges = tree::consistency_ranges(size / 3, size).unwrap();
                prop_assert_eq!(tree::assemble(&ranges, lookup), tree.consistency_proof(size / 3, size));
            }
        }
    }

    /// Whatever a page hides, a replay names it, from the statement and the recorded keys.
    // covers: L-11
    #[test]
    fn replays_find_every_hidden_key(n in 1usize..60, hide in prop::collection::vec(any::<prop::sample::Index>(), 0..5), page in 1usize..20) {
        let keys: Vec<String> = (0..n).map(|i| format!("v1/events/s/{:04}.check._.json", i * 7 % 1000)).collect();
        let mut replay = Replay::new();
        for key in &keys {
            replay.push(Entry::object(key, key.as_bytes()));
        }
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        let checkpoint = Checkpoint::new("hushrun.example/l/a/b", replay.size(), replay.tree().root());
        let hidden: Vec<String> = hide.iter().map(|i| sorted[i.index(sorted.len())].clone()).collect();
        let shown: Vec<String> = sorted.iter().filter(|k| !hidden.contains(k)).cloned().collect();
        // Page through what the ledger shows.
        let mut after: Option<String> = None;
        let mut seen_hidden = Vec::new();
        let mut rest: Vec<String> = shown.clone();
        loop {
            let take: Vec<String> = rest.iter().take(page).cloned().collect();
            rest.drain(..take.len());
            let next = (!rest.is_empty()).then(|| take.last().cloned()).flatten();
            let listing = Listing::new(&checkpoint, "v1/events/s/", after.as_deref(), next.as_deref(), &take).unwrap();
            let check = replay.check_listing(&listing, Some(&take));
            let in_range: Vec<String> = hidden.iter().filter(|k| listing.covers(k)).cloned().collect();
            if in_range.is_empty() {
                prop_assert_eq!(check.status, ListingStatus::Complete);
            } else {
                prop_assert_eq!(check.status, ListingStatus::Incomplete);
                let mut omitted: Vec<String> = check.omitted.iter().map(|(k, _)| k.clone()).collect();
                omitted.sort();
                let mut expected = in_range.clone();
                expected.sort();
                expected.dedup();
                prop_assert_eq!(omitted, expected);
                seen_hidden.extend(in_range);
            }
            if next.is_none() {
                break;
            }
            after = next;
        }
        seen_hidden.sort();
        seen_hidden.dedup();
        let mut all_hidden = hidden.clone();
        all_hidden.sort();
        all_hidden.dedup();
        prop_assert_eq!(seen_hidden, all_hidden);
    }
}

/// The map's root by its definition (L-12), from its entries alone.
fn map_reference(entries: &[MapEntry], depth: usize) -> Hash {
    let bit = |e: &MapEntry| {
        let key = map::map_key(&e.prefix);
        (key[depth / 8] >> (7 - depth % 8)) & 1
    };
    match entries {
        [] => map::EMPTY,
        [one] => one.leaf_hash(),
        _ => {
            let (left, right): (Vec<MapEntry>, Vec<MapEntry>) =
                entries.iter().cloned().partition(|e| bit(e) == 0);
            Hash::node(
                &map_reference(&left, depth + 1),
                &map_reference(&right, depth + 1),
            )
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    // covers: L-12
    #[test]
    fn the_map_is_its_entries_whatever_their_history(
        ops in proptest::collection::vec((0u8..24, any::<bool>()), 1..80),
    ) {
        let prefix = |n: u8| format!("v1/events/api/{}/", format!("{n:02x}").repeat(32));
        let mut builder = MapBuilder::new();
        let mut live: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (i, (n, create)) in ops.iter().enumerate() {
            let p = prefix(*n);
            let key = format!("{p}{:04}.check._.json", i % 5);
            if *create {
                builder.insert(&key);
                let keys = live.entry(p).or_default();
                if !keys.contains(&key) {
                    keys.push(key);
                    keys.sort();
                }
            } else {
                builder.remove(&key);
                if let Some(keys) = live.get_mut(&p) {
                    keys.retain(|k| *k != key);
                }
            }
            if i % 7 == 0 {
                builder.commit();
            }
        }
        let root = builder.commit();
        let entries: Vec<MapEntry> = live
            .iter()
            .filter_map(|(p, keys)| MapEntry::of_keys(p, keys))
            .collect();
        prop_assert_eq!(root, map_reference(&entries, 0));
        prop_assert_eq!(root, Map::from_entries(entries.iter().cloned()).root());
        // Every prefix, present or not, is proven as it is.
        for n in 0u8..24 {
            let p = prefix(n);
            let proven = builder.map().prove(&p).verify(&p, &root);
            let expected = entries.iter().find(|e| e.prefix == p).cloned();
            prop_assert_eq!(proven, Ok(expected));
        }
    }

    // covers: L-12
    #[test]
    fn every_page_of_a_key_tree_is_proven_and_no_short_one_is(
        n in 1usize..60,
        start in any::<usize>(),
        len in any::<usize>(),
        hide in any::<usize>(),
    ) {
        let prefix = "v1/deployed/api/production/";
        let keys: Vec<String> = (0..n).map(|i| format!("{prefix}{i:05}.json")).collect();
        let entry = MapEntry::of_keys(prefix, &keys).unwrap();
        let start = start % (n + 1);
        let len = len % (n - start + 1);
        let page = &keys[start..start + len];
        let after = start.checked_sub(1).map(|i| keys[i].as_str());
        let more = start + len < n;
        let next = if more { page.last().map(String::as_str) } else { None };
        let proof = PageProof::new(&keys, start, len).unwrap();
        let honest = verify_page(prefix, Some(&entry), after, page, next, Some(&proof));
        // An empty page that claims the end, with keys still to come, is never complete.
        if len == 0 && more {
            prop_assert!(honest.is_err());
        } else {
            prop_assert_eq!(honest, Ok(()));
        }
        if !page.is_empty() {
            let mut shown = page.to_vec();
            shown.remove(hide % shown.len());
            let next = if more { shown.last().map(String::as_str) } else { None };
            for proof in [Some(proof.clone()), PageProof::new(&keys, start, shown.len())] {
                let result = verify_page(prefix, Some(&entry), after, &shown, next, proof.as_ref());
                // Hiding the page's last key of a page that continues is just a shorter page.
                let shorter_page = more && hide % page.len() == page.len() - 1;
                if !shorter_page {
                    prop_assert!(result.is_err(), "{:?} hid key {}", shown, hide % page.len());
                }
            }
        }
    }
}
