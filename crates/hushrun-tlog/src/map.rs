//! \[M19\] The prefix map (L-12): every checkpoint commits to the sorted keys of each listing
//! prefix, so that a listing can be proven to be exactly the keys the log holds there.
//!
//! - **Prefixes.** A key under `v1/events/<scope>/<digest>/` or `v1/deployed/<scope>/<stage or _>/`
//!   is listed under that prefix: its first four segments (L-2). Other keys, such as policies,
//!   are under no map prefix.
//! - **Key trees.** A prefix's live keys, in byte order, are the leaves of an RFC 6962 tree
//!   whose leaf hashes are `SHA-256(0x00 || key)`. A whole listing is proven by its keys alone;
//!   a page by a range proof ([`PageProof`]).
//! - **The map** is a sparse Merkle tree over `SHA-256(prefix)`, 256 bits, most significant
//!   first, compacted: an empty subtree hashes to 32 zero bytes, a subtree holding one entry to
//!   that entry's hash, and any other to `SHA-256(0x01 || left || right)`. An entry is the text
//!
//!   ```text
//!   hushrun-map-entry/v1
//!   v1/events/api/5f0c…e1/        the prefix
//!   7                             how many live keys it has, at least 1
//!   9a1b…                         the root of its key tree, lowercase hex
//!   ```
//!
//!   and its hash is `SHA-256(0x00 || entry)`. A prefix without live keys has no entry. The
//!   shape depends only on the entries, never on the order they came in, so anyone replaying
//!   the log computes the same root.
//! - **The commitment.** The ledger ends every batch with a `map` leaf ([`crate::entry`]) that
//!   records the map's root for the leaves before it. A listing at a checkpoint is proven by that
//!   leaf (the tree's last), the map's path to the prefix's entry or to where it would be, and
//!   the page's range in the key tree ([`ListingProof`]). Monitors replay the log and check every
//!   map leaf ([`MapBuilder`]): the map follows from the log, or the ledger signed its own
//!   contradiction.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::checkpoint::{Checkpoint, parse_size};
use crate::entry::{Entry, EntryKind};
use crate::hash::Hash;
use crate::proof::{ProofError, root_from_range, verify_inclusion};
use crate::tree::{CompactRange, MemTree};

/// The first line of every map entry.
pub const MAP_ENTRY_HEADER: &str = "hushrun-map-entry/v1";
/// The hash of an empty subtree, and the root of the empty map.
pub const EMPTY: Hash = Hash([0; 32]);
/// The depth of the map: one level per bit of a prefix's hash.
pub const MAX_DEPTH: usize = 256;

/// Why a map or listing proof doesn't verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    /// The proof, or the page it is about, doesn't have the right shape.
    Malformed(String),
    /// The proof doesn't lead to the committed root.
    Mismatch(String),
    /// The page isn't what the proven tree holds: keys left out, added, or out of range.
    Incomplete(String),
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::Malformed(why) => write!(f, "malformed proof: {why}"),
            MapError::Mismatch(why) => write!(f, "the proof doesn't verify: {why}"),
            MapError::Incomplete(why) => write!(f, "the listing isn't what the map holds: {why}"),
        }
    }
}

impl std::error::Error for MapError {}

/// The map prefix `key` is listed under: its first four segments and a `/`, when it is under
/// `v1/events/` or `v1/deployed/` and has more after them.
pub fn map_prefix(key: &str) -> Option<&str> {
    if !(key.starts_with("v1/events/") || key.starts_with("v1/deployed/")) {
        return None;
    }
    let end = key.match_indices('/').nth(3)?.0;
    let (prefix, rest) = (&key[..=end], &key[end + 1..]);
    let valid = crate::valid_name(&key[..end]) && !rest.is_empty();
    valid.then_some(prefix)
}

/// Whether a listing of `prefix` can be proven: it is a map prefix (L-12).
pub fn is_map_prefix(prefix: &str) -> bool {
    prefix.ends_with('/') && map_prefix(&format!("{prefix}x")) == Some(prefix)
}

/// A key's leaf hash in its prefix's key tree.
pub fn key_leaf(key: &str) -> Hash {
    Hash::leaf(key.as_bytes())
}

/// The root of the key tree of `keys`, which must be in byte order.
pub fn keys_root<S: AsRef<str>>(keys: &[S]) -> Hash {
    let mut range = CompactRange::new();
    for key in keys {
        range.append(key_leaf(key.as_ref()));
    }
    range.root()
}

/// A prefix's place in the map: its prefix, how many live keys it has, and their tree's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapEntry {
    pub prefix: String,
    pub count: u64,
    pub root: Hash,
}

impl MapEntry {
    /// The entry of `prefix` with these keys, in byte order; `None` without keys.
    pub fn of_keys<S: AsRef<str>>(prefix: &str, keys: &[S]) -> Option<MapEntry> {
        (!keys.is_empty()).then(|| MapEntry {
            prefix: prefix.to_string(),
            count: keys.len() as u64,
            root: keys_root(keys),
        })
    }

    /// The entry's text.
    pub fn text(&self) -> String {
        format!(
            "{MAP_ENTRY_HEADER}\n{}\n{}\n{}\n",
            self.prefix,
            self.count,
            self.root.hex()
        )
    }

    /// The entry's hash in the map: `SHA-256(0x00 || text)`.
    pub fn leaf_hash(&self) -> Hash {
        Hash::leaf(self.text().as_bytes())
    }

    /// Parses an entry's text.
    pub fn parse(text: &str) -> Result<MapEntry, MapError> {
        let bad = |why: &str| MapError::Malformed(format!("a map entry: {why}"));
        let body = text
            .strip_suffix('\n')
            .ok_or_else(|| bad("it doesn't end in a newline"))?;
        let [header, prefix, count, root] = body.split('\n').collect::<Vec<_>>()[..] else {
            return Err(bad("it isn't four lines"));
        };
        if header != MAP_ENTRY_HEADER {
            return Err(bad("the first line isn't hushrun-map-entry/v1"));
        }
        let entry = MapEntry {
            prefix: prefix.to_string(),
            count: parse_size(count).ok_or_else(|| bad("the count isn't a number"))?,
            root: Hash::from_hex(root).ok_or_else(|| bad("the root isn't 64 lowercase hex"))?,
        };
        entry.check()?;
        Ok(entry)
    }

    /// Whether the entry can be in a map: a map prefix with at least one key.
    pub fn check(&self) -> Result<(), MapError> {
        if !is_map_prefix(&self.prefix) {
            return Err(MapError::Malformed(format!(
                "{:?} is not a map prefix",
                self.prefix
            )));
        }
        if self.count == 0 {
            return Err(MapError::Malformed(format!(
                "the entry of {:?} has no keys",
                self.prefix
            )));
        }
        Ok(())
    }
}

/// The 256-bit position of `prefix` in the map.
pub fn map_key(prefix: &str) -> [u8; 32] {
    Hash::of(prefix.as_bytes()).0
}

fn bit(key: &[u8; 32], depth: usize) -> u8 {
    (key[depth / 8] >> (7 - depth % 8)) & 1
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Leaf(Arc<LeafNode>),
    Inner(Arc<InnerNode>),
}

#[derive(Debug)]
struct LeafNode {
    key: [u8; 32],
    entry: MapEntry,
    hash: Hash,
}

#[derive(Debug)]
struct InnerNode {
    left: Node,
    right: Node,
    hash: Hash,
}

impl Node {
    fn hash(&self) -> Hash {
        match self {
            Node::Empty => EMPTY,
            Node::Leaf(leaf) => leaf.hash,
            Node::Inner(inner) => inner.hash,
        }
    }

    /// A subtree from its two halves, compacted: one entry alone is that entry.
    fn join(left: Node, right: Node) -> Node {
        match (&left, &right) {
            (Node::Empty, Node::Empty) => Node::Empty,
            (Node::Leaf(_), Node::Empty) => left,
            (Node::Empty, Node::Leaf(_)) => right,
            _ => {
                let hash = Hash::node(&left.hash(), &right.hash());
                Node::Inner(Arc::new(InnerNode { left, right, hash }))
            }
        }
    }

    fn insert(&self, depth: usize, leaf: Arc<LeafNode>) -> Node {
        match self {
            Node::Empty => Node::Leaf(leaf),
            Node::Leaf(existing) if existing.key == leaf.key => Node::Leaf(leaf),
            Node::Leaf(existing) => Node::split(existing.clone(), leaf, depth),
            Node::Inner(inner) => {
                if bit(&leaf.key, depth) == 0 {
                    Node::join(inner.left.insert(depth + 1, leaf), inner.right.clone())
                } else {
                    Node::join(inner.left.clone(), inner.right.insert(depth + 1, leaf))
                }
            }
        }
    }

    /// The subtree at `depth` holding two leaves with different keys.
    fn split(a: Arc<LeafNode>, b: Arc<LeafNode>, depth: usize) -> Node {
        let (bit_a, bit_b) = (bit(&a.key, depth), bit(&b.key, depth));
        if bit_a != bit_b {
            let (left, right) = if bit_a == 0 { (a, b) } else { (b, a) };
            return Node::join(Node::Leaf(left), Node::Leaf(right));
        }
        let below = Node::split(a, b, depth + 1);
        if bit_a == 0 {
            Node::join(below, Node::Empty)
        } else {
            Node::join(Node::Empty, below)
        }
    }

    fn remove(&self, depth: usize, key: &[u8; 32]) -> Node {
        match self {
            Node::Leaf(leaf) if leaf.key == *key => Node::Empty,
            Node::Empty | Node::Leaf(_) => self.clone(),
            Node::Inner(inner) => {
                if bit(key, depth) == 0 {
                    Node::join(inner.left.remove(depth + 1, key), inner.right.clone())
                } else {
                    Node::join(inner.left.clone(), inner.right.remove(depth + 1, key))
                }
            }
        }
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a MapEntry>) {
        match self {
            Node::Empty => {}
            Node::Leaf(leaf) => out.push(&leaf.entry),
            Node::Inner(inner) => {
                inner.left.collect(out);
                inner.right.collect(out);
            }
        }
    }
}

/// The map: an immutable sparse Merkle tree whose clones share their nodes, so that keeping
/// the map of each checkpoint is cheap.
#[derive(Clone, Debug)]
pub struct Map {
    root: Node,
    len: usize,
}

impl Default for Map {
    fn default() -> Self {
        Map {
            root: Node::Empty,
            len: 0,
        }
    }
}

impl Map {
    pub fn new() -> Map {
        Map::default()
    }

    /// A map of these entries.
    pub fn from_entries(entries: impl IntoIterator<Item = MapEntry>) -> Map {
        let mut map = Map::new();
        for entry in entries {
            map.insert(entry);
        }
        map
    }

    /// The map's root: what a map leaf records.
    pub fn root(&self) -> Hash {
        self.root.hash()
    }

    /// How many prefixes have live keys.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The entry of `prefix`, if it has live keys.
    pub fn get(&self, prefix: &str) -> Option<&MapEntry> {
        let key = map_key(prefix);
        let mut node = &self.root;
        let mut depth = 0;
        loop {
            match node {
                Node::Empty => return None,
                Node::Leaf(leaf) => return (leaf.key == key).then_some(&leaf.entry),
                Node::Inner(inner) => {
                    node = if bit(&key, depth) == 0 {
                        &inner.left
                    } else {
                        &inner.right
                    };
                    depth += 1;
                }
            }
        }
    }

    /// Sets a prefix's entry.
    pub fn insert(&mut self, entry: MapEntry) {
        if self.get(&entry.prefix).is_none() {
            self.len += 1;
        }
        let leaf = Arc::new(LeafNode {
            key: map_key(&entry.prefix),
            hash: entry.leaf_hash(),
            entry,
        });
        self.root = self.root.insert(0, leaf);
    }

    /// Removes a prefix's entry: its last key went. Returns whether it had one.
    pub fn remove(&mut self, prefix: &str) -> bool {
        if self.get(prefix).is_none() {
            return false;
        }
        self.len -= 1;
        self.root = self.root.remove(0, &map_key(prefix));
        true
    }

    /// Every entry, in the order of their positions.
    pub fn entries(&self) -> Vec<&MapEntry> {
        let mut out = Vec::with_capacity(self.len);
        self.root.collect(&mut out);
        out
    }

    /// The proof of `prefix`'s entry, or that it has none.
    pub fn prove(&self, prefix: &str) -> MapProof {
        let key = map_key(prefix);
        let mut path = Vec::new();
        let mut node = &self.root;
        let mut depth = 0;
        let leaf = loop {
            match node {
                Node::Empty => break None,
                Node::Leaf(leaf) => break Some(leaf.entry.clone()),
                Node::Inner(inner) => {
                    let (next, sibling) = if bit(&key, depth) == 0 {
                        (&inner.left, &inner.right)
                    } else {
                        (&inner.right, &inner.left)
                    };
                    path.push(sibling.hash());
                    node = next;
                    depth += 1;
                }
            }
        };
        path.reverse();
        MapProof { path, leaf }
    }
}

/// The path from the map's root to where a prefix's entry is, or would be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapProof {
    /// The siblings along the path, deepest first.
    pub path: Vec<Hash>,
    /// The entry where the path ends: the prefix's own, or another one whose position shares
    /// the path, which shows the prefix has none; `None` for an empty subtree, which shows that
    /// too.
    pub leaf: Option<MapEntry>,
}

impl MapProof {
    /// Checks the proof against the map's root: the prefix's entry, or `None` when the map
    /// holds none, which means the prefix has no live keys.
    pub fn verify(&self, prefix: &str, root: &Hash) -> Result<Option<MapEntry>, MapError> {
        let depth = self.path.len();
        if depth > MAX_DEPTH {
            return Err(MapError::Malformed("a map path longer than 256".into()));
        }
        let key = map_key(prefix);
        let mut hash = match &self.leaf {
            None => EMPTY,
            Some(entry) => {
                entry.check()?;
                let other = map_key(&entry.prefix);
                if (0..depth).any(|d| bit(&other, d) != bit(&key, d)) {
                    return Err(MapError::Malformed(format!(
                        "the entry of {:?} isn't on the path to {prefix:?}",
                        entry.prefix
                    )));
                }
                entry.leaf_hash()
            }
        };
        for (i, sibling) in self.path.iter().enumerate() {
            hash = if bit(&key, depth - 1 - i) == 0 {
                Hash::node(&hash, sibling)
            } else {
                Hash::node(sibling, &hash)
            };
        }
        if hash != *root {
            return Err(MapError::Mismatch(format!(
                "the path to {prefix:?} doesn't lead to the map's root"
            )));
        }
        Ok(self.leaf.clone().filter(|entry| entry.prefix == prefix))
    }
}

/// Where a listing page sits in its prefix's key tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageProof {
    /// The position of the page's first key among the prefix's keys; for an empty page, how
    /// many keys sort before where it would start.
    pub index: u64,
    /// The key just before the page, at `index − 1`: none when `index` is 0.
    pub previous: Option<String>,
    /// The complete subtrees of the keys before the proven range (`previous` and the page),
    /// and of the keys after it.
    pub left: Vec<Hash>,
    pub right: Vec<Hash>,
}

impl PageProof {
    /// The proof for the page `keys[index..index + len]`, where `keys` are all the prefix's
    /// live keys, in byte order.
    pub fn new<S: AsRef<str>>(keys: &[S], index: usize, len: usize) -> Option<PageProof> {
        if index.checked_add(len)? > keys.len() {
            return None;
        }
        let begin = index.saturating_sub(1);
        let tree = MemTree::from_leaves(keys.iter().map(|k| key_leaf(k.as_ref())));
        let size = keys.len() as u64;
        let (left, right) = tree.range_proof(begin as u64, (index + len) as u64, size)?;
        Some(PageProof {
            index: index as u64,
            previous: (index > 0).then(|| keys[index - 1].as_ref().to_string()),
            left,
            right,
        })
    }
}

/// Checks that `keys` are exactly the keys of the prefix's tree (as the map holds it: `entry`,
/// or no keys at all) that come after `after`, up to `next` (to the end of the prefix without
/// one): the page a listing answered.
pub fn verify_page(
    prefix: &str,
    entry: Option<&MapEntry>,
    after: Option<&str>,
    keys: &[String],
    next: Option<&str>,
    proof: Option<&PageProof>,
) -> Result<(), MapError> {
    let incomplete = |why: String| Err(MapError::Incomplete(why));
    for (i, key) in keys.iter().enumerate() {
        if !key.starts_with(prefix) {
            return incomplete(format!("{key:?} isn't under the prefix"));
        }
        if after.is_some_and(|after| key.as_str() <= after) {
            return incomplete(format!("{key:?} isn't after {:?}", after.unwrap_or("")));
        }
        if i > 0 && keys[i - 1] >= *key {
            return incomplete("the keys aren't in byte order".into());
        }
    }
    if let Some(next) = next
        && keys.last().map(String::as_str) != Some(next)
    {
        return incomplete(format!(
            "the page says {next:?} is next, but doesn't end with it"
        ));
    }
    let Some(entry) = entry else {
        // The map holds nothing under the prefix.
        if keys.is_empty() && next.is_none() {
            return Ok(());
        }
        return incomplete(format!(
            "the map holds no keys under {prefix:?}, and the page lists {}",
            keys.len()
        ));
    };
    let Some(proof) = proof else {
        return Err(MapError::Malformed(format!(
            "the map holds {} key(s) under {prefix:?}, and the page has no proof of where it is among them",
            entry.count
        )));
    };
    let end = proof
        .index
        .checked_add(keys.len() as u64)
        .filter(|end| *end <= entry.count)
        .ok_or_else(|| {
            MapError::Incomplete(format!(
                "the page runs past the {} key(s) the map holds",
                entry.count
            ))
        })?;
    let mut leaves = Vec::with_capacity(keys.len() + 1);
    match (&proof.previous, proof.index) {
        (None, 0) => {}
        (Some(previous), index) if index > 0 => {
            // Nothing between `after` and the page: the key before it is at most `after`.
            if after.is_none_or(|after| previous.as_str() > after) {
                return incomplete(format!(
                    "the page starts at key {index} of {:?}, and {previous:?} before it wasn't listed",
                    prefix
                ));
            }
            if keys.first().is_some_and(|first| first <= previous) {
                return incomplete("the key before the page isn't before it".into());
            }
            leaves.push(key_leaf(previous));
        }
        _ => {
            return Err(MapError::Malformed(
                "a page proof names the key before the page exactly when it has one".into(),
            ));
        }
    }
    if next.is_none() && end != entry.count {
        return incomplete(format!(
            "the page ends at key {end} of the {} under {prefix:?}, but says nothing follows",
            entry.count
        ));
    }
    leaves.extend(keys.iter().map(|k| key_leaf(k)));
    let begin = proof.index - u64::from(proof.previous.is_some());
    let root = root_from_range(entry.count, begin, &leaves, &proof.left, &proof.right)
        .map_err(|e: ProofError| MapError::Malformed(format!("the page's range proof: {e}")))?;
    if root != entry.root {
        return Err(MapError::Mismatch(format!(
            "the page's keys aren't those of the key tree the map holds for {prefix:?}"
        )));
    }
    Ok(())
}

/// The proof that a listing page is complete at a checkpoint (L-12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingProof {
    /// The map's root, which the checkpoint's last leaf records.
    pub map_root: Hash,
    /// The inclusion proof of that leaf in the checkpoint's tree.
    pub map_leaf: Vec<Hash>,
    /// The map's path to the prefix.
    pub map: MapProof,
    /// The page's place in the prefix's key tree; none when the map holds no keys for it.
    pub page: Option<PageProof>,
}

impl ListingProof {
    /// Checks that `keys` are exactly the keys under `prefix` after `after`, up to `next`, in
    /// the tree of `checkpoint`, a verified checkpoint.
    pub fn verify(
        &self,
        checkpoint: &Checkpoint,
        prefix: &str,
        after: Option<&str>,
        keys: &[String],
        next: Option<&str>,
    ) -> Result<(), MapError> {
        if !is_map_prefix(prefix) {
            return Err(MapError::Malformed(format!(
                "{prefix:?} is not a map prefix, so a listing of it can't be proven"
            )));
        }
        if checkpoint.size == 0 {
            // The empty log's map is empty.
            return verify_page(prefix, None, after, keys, next, None);
        }
        let leaf = Entry::map(self.map_root).leaf_hash();
        verify_inclusion(
            &leaf,
            checkpoint.size - 1,
            checkpoint.size,
            &self.map_leaf,
            &checkpoint.root,
        )
        .map_err(|e| {
            MapError::Mismatch(format!(
                "the checkpoint's last leaf isn't a map leaf with that root: {e}"
            ))
        })?;
        let entry = self.map.verify(prefix, &self.map_root)?;
        verify_page(
            prefix,
            entry.as_ref(),
            after,
            keys,
            next,
            self.page.as_ref(),
        )
    }
}

/// The map as a log builds it: each map prefix's live keys, and the map of their trees. The
/// ledger and monitors feed it every entry; at each map leaf, [`MapBuilder::commit`] gives the
/// root the leaf must record.
#[derive(Debug, Clone, Default)]
pub struct MapBuilder {
    keys: BTreeMap<String, BTreeSet<String>>,
    dirty: BTreeSet<String>,
    map: Map,
}

impl MapBuilder {
    pub fn new() -> MapBuilder {
        MapBuilder::default()
    }

    /// Takes an entry: an object's key goes live, a tombstone's goes. Other kinds change no key.
    pub fn apply(&mut self, entry: &Entry) {
        match entry.kind {
            EntryKind::Object => self.insert(&entry.name),
            EntryKind::Tombstone => self.remove(&entry.name),
            EntryKind::Config | EntryKind::Map => {}
        }
    }

    /// A key goes live.
    pub fn insert(&mut self, key: &str) {
        let Some(prefix) = map_prefix(key) else {
            return;
        };
        let added = match self.keys.get_mut(prefix) {
            Some(keys) => keys.insert(key.to_string()),
            None => {
                self.keys
                    .insert(prefix.to_string(), BTreeSet::from([key.to_string()]));
                true
            }
        };
        if added && !self.dirty.contains(prefix) {
            self.dirty.insert(prefix.to_string());
        }
    }

    /// A key goes.
    pub fn remove(&mut self, key: &str) {
        if let Some(prefix) = map_prefix(key)
            && let Some(keys) = self.keys.get_mut(prefix)
            && keys.remove(key)
        {
            self.dirty.insert(prefix.to_string());
        }
    }

    /// Brings the map up to date with every key so far, and returns its root.
    pub fn commit(&mut self) -> Hash {
        for prefix in std::mem::take(&mut self.dirty) {
            let keys: Vec<&String> = self.keys.get(&prefix).into_iter().flatten().collect();
            match MapEntry::of_keys(&prefix, &keys) {
                Some(entry) => self.map.insert(entry),
                None => {
                    self.keys.remove(&prefix);
                    self.map.remove(&prefix);
                }
            }
        }
        self.map.root()
    }

    /// The map as of the last commit.
    pub fn map(&self) -> &Map {
        &self.map
    }

    /// A prefix's live keys, in byte order, as of the last change.
    pub fn keys(&self, prefix: &str) -> Vec<String> {
        self.keys
            .get(prefix)
            .map(|keys| keys.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(digest: char) -> String {
        format!("v1/events/api/{}/", digest.to_string().repeat(64))
    }

    fn entry(prefix: &str, n: u64) -> MapEntry {
        let keys: Vec<String> = (0..n).map(|i| format!("{prefix}{i:03}.json")).collect();
        MapEntry::of_keys(prefix, &keys).unwrap()
    }

    /// The map's root by its definition, from the entries alone.
    fn reference(entries: &[MapEntry], depth: usize) -> Hash {
        match entries {
            [] => EMPTY,
            [one] => one.leaf_hash(),
            _ => {
                let (left, right): (Vec<MapEntry>, Vec<MapEntry>) = entries
                    .iter()
                    .cloned()
                    .partition(|e| bit(&map_key(&e.prefix), depth) == 0);
                Hash::node(&reference(&left, depth + 1), &reference(&right, depth + 1))
            }
        }
    }

    // covers: L-12
    #[test]
    fn map_prefixes_are_the_planners_listings() {
        let digest = "a".repeat(64);
        let key = format!("v1/events/api/{digest}/01JA.started.staging.json");
        assert_eq!(map_prefix(&key), Some(&key[..14 + 64 + 1]));
        assert_eq!(
            map_prefix("v1/deployed/api/production/9999.01JA.json"),
            Some("v1/deployed/api/production/")
        );
        for none in [
            "v1/policies/abc.yaml",
            "v1/events/api/",
            "v1/events/api/x/",
            "v1/deployed/api/production",
            "trust.yaml",
            "v2/events/a/b/c",
            "v1/events/a//c/d",
        ] {
            assert_eq!(map_prefix(none), None, "{none}");
        }
        assert!(is_map_prefix(&format!("v1/events/api/{digest}/")));
        assert!(is_map_prefix("v1/deployed/api/staging/"));
        for not in [
            "v1/events/",
            "v1/events/api/",
            "v1/deployed/api/staging",
            "",
            "v1/",
        ] {
            assert!(!is_map_prefix(not), "{not}");
        }
    }

    // covers: L-12
    #[test]
    fn entries_have_one_text() {
        let e = entry(&events('a'), 3);
        assert_eq!(MapEntry::parse(&e.text()).unwrap(), e);
        assert!(e.text().starts_with("hushrun-map-entry/v1\nv1/events/api/"));
        for bad in [
            e.text().replace("\n3\n", "\n03\n"),
            e.text().replace("\n3\n", "\n0\n"),
            e.text().replace("v1/events/api/", "v1/events/"),
            e.text()
                .replace("hushrun-map-entry/v1", "hushrun-map-entry/v2"),
            e.text().trim_end().to_string(),
        ] {
            assert!(MapEntry::parse(&bad).is_err(), "{bad:?}");
        }
    }

    // covers: L-12
    #[test]
    fn the_root_depends_only_on_the_entries() {
        let entries: Vec<MapEntry> = "abcdef0123456789"
            .chars()
            .enumerate()
            .map(|(i, c)| entry(&events(c), i as u64 + 1))
            .collect();
        let forward = Map::from_entries(entries.iter().cloned());
        let backward = Map::from_entries(entries.iter().rev().cloned());
        assert_eq!(forward.root(), backward.root());
        assert_eq!(forward.root(), reference(&entries, 0));
        assert_eq!(forward.len(), entries.len());
        // Removing entries gives the map of the rest, whatever came before.
        let mut shrinking = forward.clone();
        for (i, e) in entries.iter().enumerate() {
            assert!(shrinking.remove(&e.prefix));
            assert_eq!(shrinking.root(), reference(&entries[i + 1..], 0));
        }
        assert_eq!(shrinking.root(), EMPTY);
        assert!(!shrinking.remove(&entries[0].prefix));
        // Clones keep their own map.
        assert_eq!(forward.root(), reference(&entries, 0));
        // Replacing an entry.
        let mut changed = forward.clone();
        changed.insert(entry(&entries[0].prefix, 9));
        assert_ne!(changed.root(), forward.root());
        assert_eq!(changed.len(), forward.len());
        assert_eq!(changed.get(&entries[0].prefix).unwrap().count, 9);
    }

    // covers: L-12
    #[test]
    fn proofs_show_entries_and_their_absence() {
        let entries: Vec<MapEntry> = "abcdef".chars().map(|c| entry(&events(c), 2)).collect();
        let map = Map::from_entries(entries.iter().cloned());
        let root = map.root();
        for e in &entries {
            let proof = map.prove(&e.prefix);
            assert_eq!(proof.verify(&e.prefix, &root), Ok(Some(e.clone())));
            // Not against another root.
            assert!(proof.verify(&e.prefix, &EMPTY).is_err());
        }
        for absent in "ghijklmnopqrstuvwxyz".chars().map(events) {
            let proof = map.prove(&absent);
            assert_eq!(proof.verify(&absent, &root), Ok(None), "{absent}");
        }
        // A present prefix can't be shown absent with another's proof.
        let present = &entries[0].prefix;
        for other in "ghijklmnopqrstuvwxyz".chars().map(events) {
            let proof = map.prove(&other);
            assert_ne!(proof.verify(present, &root), Ok(None));
        }
        // The empty map, and a map of one entry.
        let empty = Map::new();
        assert_eq!(empty.prove(present).verify(present, &EMPTY), Ok(None));
        let one = Map::from_entries([entries[0].clone()]);
        assert_eq!(
            one.prove(&entries[1].prefix)
                .verify(&entries[1].prefix, &one.root()),
            Ok(None)
        );
        assert_eq!(
            one.prove(present).verify(present, &one.root()),
            Ok(Some(entries[0].clone()))
        );
    }

    fn keys(prefix: &str, n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}{i:03}.json")).collect()
    }

    // covers: L-12
    #[test]
    fn pages_verify_only_when_complete() {
        let prefix = "v1/deployed/api/production/";
        let all = keys(prefix, 12);
        let entry = MapEntry::of_keys(prefix, &all).unwrap();
        // The whole listing, and every page of every size.
        assert_eq!(
            verify_page(
                prefix,
                Some(&entry),
                None,
                &all,
                None,
                PageProof::new(&all, 0, 12).as_ref()
            ),
            Ok(())
        );
        for start in 0..=12usize {
            for len in 0..=(12 - start) {
                let page = &all[start..start + len];
                let after = start.checked_sub(1).map(|i| all[i].as_str());
                let next = (start + len < 12 && len > 0).then(|| page[len - 1].as_str());
                let proof = PageProof::new(&all, start, len).unwrap();
                let result = verify_page(prefix, Some(&entry), after, page, next, Some(&proof));
                if len == 0 && start < 12 {
                    // An empty page that says nothing follows, with keys still to come.
                    assert!(result.is_err(), "{start}+{len}");
                } else {
                    assert_eq!(result, Ok(()), "{start}+{len}");
                }
            }
        }
        // A first page of 5, as list_first reads it.
        let first = &all[..5];
        let proof = PageProof::new(&all, 0, 5).unwrap();
        assert_eq!(
            verify_page(
                prefix,
                Some(&entry),
                None,
                first,
                Some(&all[4]),
                Some(&proof)
            ),
            Ok(())
        );
        // Leaving a key out, anywhere, fails.
        for hidden in 0..5 {
            let mut shown: Vec<String> = first.to_vec();
            shown.remove(hidden);
            let next = shown.last().map(String::as_str);
            assert!(
                verify_page(prefix, Some(&entry), None, &shown, next, Some(&proof)).is_err(),
                "{hidden}"
            );
            let short = PageProof::new(&all, 0, 4).unwrap();
            assert!(
                verify_page(prefix, Some(&entry), None, &shown, next, Some(&short)).is_err()
                    || hidden == 4,
                "{hidden}"
            );
        }
        // A page that skips ahead of `after`.
        let skipped = &all[6..9];
        let proof = PageProof::new(&all, 6, 3).unwrap();
        assert!(
            verify_page(
                prefix,
                Some(&entry),
                Some(&all[3]),
                skipped,
                Some(&all[8]),
                Some(&proof)
            )
            .is_err()
        );
        // A last page that stops early.
        let proof = PageProof::new(&all, 8, 2).unwrap();
        assert!(
            verify_page(
                prefix,
                Some(&entry),
                Some(&all[7]),
                &all[8..10],
                None,
                Some(&proof)
            )
            .is_err()
        );
        // An absent prefix lists nothing.
        assert_eq!(verify_page(prefix, None, None, &[], None, None), Ok(()));
        assert!(verify_page(prefix, None, None, &all[..1], None, None).is_err());
        // Keys out of order, or outside the prefix.
        let mut swapped = all.clone();
        swapped.swap(0, 1);
        assert!(
            verify_page(
                prefix,
                Some(&entry),
                None,
                &swapped,
                None,
                PageProof::new(&all, 0, 12).as_ref()
            )
            .is_err()
        );
    }

    // covers: L-12
    #[test]
    fn listings_are_proven_by_the_checkpoints_map_leaf() {
        let prefix = events('a');
        let all = keys(&prefix, 4);
        let mut builder = MapBuilder::new();
        let mut tree = MemTree::new();
        for key in &all {
            let object = Entry::object(key, key.as_bytes());
            builder.apply(&object);
            tree.push(object.leaf_hash());
        }
        let root = builder.commit();
        tree.push(Entry::map(root).leaf_hash());
        let checkpoint = Checkpoint::new("hushrun.example/l/acme/back", tree.size(), tree.root());
        let proof = ListingProof {
            map_root: root,
            map_leaf: tree.inclusion_proof(tree.size() - 1, tree.size()).unwrap(),
            map: builder.map().prove(&prefix),
            page: PageProof::new(&all, 0, all.len()),
        };
        assert_eq!(proof.verify(&checkpoint, &prefix, None, &all, None), Ok(()));
        // Without a key: refused.
        assert!(
            proof
                .verify(&checkpoint, &prefix, None, &all[1..], None)
                .is_err()
        );
        // A map leaf of another root: refused.
        let mut lying = proof.clone();
        lying.map_root = Hash::leaf(b"another map");
        assert!(
            lying
                .verify(&checkpoint, &prefix, None, &all, None)
                .is_err()
        );
        // An absent prefix, shown absent.
        let other = events('b');
        let absent = ListingProof {
            map: builder.map().prove(&other),
            page: None,
            ..proof.clone()
        };
        assert_eq!(absent.verify(&checkpoint, &other, None, &[], None), Ok(()));
        // The empty log lists nothing.
        let empty = Checkpoint::new("hushrun.example/l/acme/back", 0, Hash::empty());
        assert_eq!(absent.verify(&empty, &other, None, &[], None), Ok(()));
        // Not a map prefix.
        assert!(
            proof
                .verify(&checkpoint, "v1/events/", None, &all, None)
                .is_err()
        );
    }

    // covers: L-12
    #[test]
    fn the_builder_follows_creates_and_deletions() {
        let a = events('a');
        let mut builder = MapBuilder::new();
        assert_eq!(builder.commit(), EMPTY);
        for key in keys(&a, 3) {
            builder.apply(&Entry::object(&key, b"x"));
        }
        builder.apply(&Entry::object("v1/policies/x.yaml", b"p"));
        builder.apply(&Entry::config("trust.yaml", b"t"));
        let root = builder.commit();
        assert_eq!(
            root,
            Map::from_entries([MapEntry::of_keys(&a, &keys(&a, 3)).unwrap()]).root()
        );
        for key in keys(&a, 3) {
            builder.apply(&Entry::tombstone(&key, Hash::of(b"x"), None));
        }
        assert_eq!(builder.commit(), EMPTY);
        assert!(builder.keys(&a).is_empty());
    }
}
