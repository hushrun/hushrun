//! RFC 6962 Merkle trees (RFC 9162 §2.1): roots, and which subtrees a proof lists.
//!
//! A *complete node* is the hash of a perfect subtree of `2^level` leaves that starts at leaf
//! `index << level`. Every hash a root or a proof needs is a complete node, or a fold of a few
//! of them along the tree's right edge. So a store that keeps every complete node, as the
//! server does, answers any proof with O(log n) lookups: [`inclusion_ranges`] and
//! [`consistency_ranges`] say which ranges of leaves a proof lists, [`range_nodes`] which
//! complete nodes make up a range, and [`fold`] combines them.

use std::ops::Range;

use crate::hash::Hash;

/// A complete node: the perfect subtree of `2^level` leaves starting at leaf `index << level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId {
    pub level: u8,
    pub index: u64,
}

impl NodeId {
    pub fn new(level: u8, index: u64) -> NodeId {
        NodeId { level, index }
    }

    /// The first leaf below this node.
    pub fn begin(&self) -> u64 {
        self.index << self.level
    }

    /// One past the last leaf below this node.
    pub fn end(&self) -> u64 {
        (self.index + 1) << self.level
    }
}

/// The largest power of two strictly smaller than `n`, where RFC 6962 splits a tree of `n ≥ 2`
/// leaves.
fn split(n: u64) -> u64 {
    debug_assert!(n >= 2);
    1 << (63 - (n - 1).leading_zeros())
}

/// The complete nodes that make up the leaves `[begin, end)`, left to right: the largest
/// aligned subtrees first. For the ranges proofs list (a complete node, or a node on the
/// tree's right edge), they are the subtrees RFC 6962's recursion reaches.
pub fn range_nodes(begin: u64, end: u64) -> Vec<NodeId> {
    let mut nodes = Vec::new();
    let mut at = begin;
    while at < end {
        let aligned = if at == 0 { 63 } else { at.trailing_zeros() };
        let fits = 63 - (end - at).leading_zeros();
        let level = aligned.min(fits);
        nodes.push(NodeId::new(level as u8, at >> level));
        at += 1 << level;
    }
    nodes
}

/// The hash of a range from the hashes of its complete nodes (left to right), folded from the
/// right as RFC 6962's recursion combines them. `None` for no nodes.
pub fn fold(hashes: &[Hash]) -> Option<Hash> {
    let (last, rest) = hashes.split_last()?;
    Some(
        rest.iter()
            .rev()
            .fold(*last, |right, left| Hash::node(left, &right)),
    )
}

/// The ranges of leaves whose hashes make up the inclusion proof of leaf `index` in the tree
/// of `size` leaves, nearest the leaf first (RFC 6962 `PATH(m, D[n])`). `None` when `index` is
/// not below `size`.
pub fn inclusion_ranges(index: u64, size: u64) -> Option<Vec<Range<u64>>> {
    if index >= size {
        return None;
    }
    let mut out = Vec::new();
    path(index, 0, size, &mut out);
    Some(out)
}

fn path(index: u64, lo: u64, hi: u64, out: &mut Vec<Range<u64>>) {
    if hi - lo <= 1 {
        return;
    }
    let k = split(hi - lo);
    if index < lo + k {
        path(index, lo, lo + k, out);
        out.push(lo + k..hi);
    } else {
        path(index, lo + k, hi, out);
        out.push(lo..lo + k);
    }
}

/// The ranges of leaves whose hashes make up the consistency proof from the tree of `old`
/// leaves to the tree of `new` leaves (RFC 6962 `PROOF(m, D[n])`). Empty when `old` is 0 or
/// equals `new`; `None` when `old > new`.
pub fn consistency_ranges(old: u64, new: u64) -> Option<Vec<Range<u64>>> {
    if old > new {
        return None;
    }
    let mut out = Vec::new();
    if old > 0 && old < new {
        subproof(old, 0, new, true, &mut out);
    }
    Some(out)
}

fn subproof(m: u64, lo: u64, hi: u64, whole: bool, out: &mut Vec<Range<u64>>) {
    let n = hi - lo;
    if m == n {
        if !whole {
            out.push(lo..hi);
        }
        return;
    }
    let k = split(n);
    if m <= k {
        subproof(m, lo, lo + k, whole, out);
        out.push(lo + k..hi);
    } else {
        subproof(m - k, lo + k, hi, false, out);
        out.push(lo..lo + k);
    }
}

/// Every complete node the ranges need, each once, in order.
pub fn nodes_for(ranges: &[Range<u64>]) -> Vec<NodeId> {
    let mut nodes: Vec<NodeId> = ranges
        .iter()
        .flat_map(|r| range_nodes(r.start, r.end))
        .collect();
    nodes.sort();
    nodes.dedup();
    nodes
}

/// The hashes of `ranges`, from a lookup of complete nodes. `None` when a node is missing.
pub fn assemble(
    ranges: &[Range<u64>],
    mut node: impl FnMut(NodeId) -> Option<Hash>,
) -> Option<Vec<Hash>> {
    ranges
        .iter()
        .map(|r| {
            let hashes = range_nodes(r.start, r.end)
                .into_iter()
                .map(&mut node)
                .collect::<Option<Vec<Hash>>>()?;
            fold(&hashes)
        })
        .collect()
}

/// The root of the tree of `size` leaves, from a lookup of complete nodes; the empty tree's
/// root for 0 leaves.
pub fn root(size: u64, mut node: impl FnMut(NodeId) -> Option<Hash>) -> Option<Hash> {
    if size == 0 {
        return Some(Hash::empty());
    }
    let hashes = range_nodes(0, size)
        .into_iter()
        .map(&mut node)
        .collect::<Option<Vec<Hash>>>()?;
    fold(&hashes)
}

/// A whole tree in memory: every complete node, level by level. Monitors replay a log into
/// one, and tests use it as a reference.
#[derive(Clone, Debug, Default)]
pub struct MemTree {
    /// `levels[l][i]` is the complete node `(l, i)`.
    levels: Vec<Vec<Hash>>,
}

impl MemTree {
    pub fn new() -> MemTree {
        MemTree::default()
    }

    /// A tree of these leaf hashes.
    pub fn from_leaves(leaves: impl IntoIterator<Item = Hash>) -> MemTree {
        let mut tree = MemTree::new();
        for leaf in leaves {
            tree.push(leaf);
        }
        tree
    }

    /// How many leaves the tree has.
    pub fn size(&self) -> u64 {
        self.levels.first().map_or(0, |leaves| leaves.len() as u64)
    }

    /// Appends a leaf hash. Returns the complete nodes this created, the leaf first.
    pub fn push(&mut self, leaf: Hash) -> Vec<(NodeId, Hash)> {
        if self.levels.is_empty() {
            self.levels.push(Vec::new());
        }
        let mut index = self.size();
        self.levels[0].push(leaf);
        let mut created = vec![(NodeId::new(0, index), leaf)];
        let mut level = 0usize;
        let mut hash = leaf;
        while index & 1 == 1 {
            let left = self.levels[level][(index - 1) as usize];
            hash = Hash::node(&left, &hash);
            level += 1;
            index >>= 1;
            if self.levels.len() == level {
                self.levels.push(Vec::new());
            }
            self.levels[level].push(hash);
            created.push((NodeId::new(level as u8, index), hash));
        }
        created
    }

    /// A complete node, if the tree has it.
    pub fn node(&self, id: NodeId) -> Option<Hash> {
        self.levels
            .get(usize::from(id.level))?
            .get(usize::try_from(id.index).ok()?)
            .copied()
    }

    /// The hash of leaf `index`.
    pub fn leaf(&self, index: u64) -> Option<Hash> {
        self.node(NodeId::new(0, index))
    }

    /// The root of the whole tree.
    pub fn root(&self) -> Hash {
        self.root_at(self.size())
            .expect("the tree has its own size")
    }

    /// The root of the tree's first `size` leaves; `None` beyond the tree.
    pub fn root_at(&self, size: u64) -> Option<Hash> {
        if size > self.size() {
            return None;
        }
        root(size, |id| self.node(id))
    }

    /// The inclusion proof of leaf `index` in the tree's first `size` leaves.
    pub fn inclusion_proof(&self, index: u64, size: u64) -> Option<Vec<Hash>> {
        if size > self.size() {
            return None;
        }
        assemble(&inclusion_ranges(index, size)?, |id| self.node(id))
    }

    /// The consistency proof between the tree's first `old` and first `new` leaves.
    pub fn consistency_proof(&self, old: u64, new: u64) -> Option<Vec<Hash>> {
        if new > self.size() {
            return None;
        }
        assemble(&consistency_ranges(old, new)?, |id| self.node(id))
    }

    /// \[M19\] A range proof of leaves `[begin, end)` in the tree's first `size` leaves: the
    /// complete nodes of the leaves before them, and of the leaves after them, as
    /// [`crate::proof::root_from_range`] takes them.
    pub fn range_proof(&self, begin: u64, end: u64, size: u64) -> Option<(Vec<Hash>, Vec<Hash>)> {
        if begin > end || end > size || size > self.size() {
            return None;
        }
        let hashes = |nodes: Vec<NodeId>| {
            nodes
                .into_iter()
                .map(|id| self.node(id))
                .collect::<Option<Vec<Hash>>>()
        };
        Some((
            hashes(range_nodes(0, begin))?,
            hashes(range_nodes(end, size))?,
        ))
    }

    /// The complete nodes of the tree's right edge, for a [`CompactRange`].
    pub fn compact_range(&self) -> CompactRange {
        let size = self.size();
        let nodes = range_nodes(0, size)
            .into_iter()
            .map(|id| self.node(id).expect("the tree has its own nodes"))
            .collect();
        CompactRange { size, nodes }
    }
}

/// The right edge of a tree: the complete nodes that make up its leaves `[0, size)`, one per
/// bit set in `size`. It is all an appender needs to extend a tree, compute its root, and know
/// which complete nodes each new leaf creates; the server keeps those nodes in its store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactRange {
    size: u64,
    nodes: Vec<Hash>,
}

impl CompactRange {
    /// The empty tree's.
    pub fn new() -> CompactRange {
        CompactRange::default()
    }

    /// The complete nodes a compact range of `size` leaves holds, left to right.
    pub fn ids(size: u64) -> Vec<NodeId> {
        range_nodes(0, size)
    }

    /// The compact range of `size` leaves from its nodes' hashes, as [`CompactRange::ids`]
    /// lists them. `None` when there are not as many hashes as nodes.
    pub fn from_parts(size: u64, nodes: Vec<Hash>) -> Option<CompactRange> {
        (nodes.len() == size.count_ones() as usize).then_some(CompactRange { size, nodes })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// The tree's root.
    pub fn root(&self) -> Hash {
        fold(&self.nodes).unwrap_or_else(Hash::empty)
    }

    /// Appends a leaf hash. Returns the complete nodes this created, the leaf first.
    pub fn append(&mut self, leaf: Hash) -> Vec<(NodeId, Hash)> {
        let mut index = self.size;
        let mut level = 0u8;
        let mut hash = leaf;
        let mut created = vec![(NodeId::new(0, index), leaf)];
        while index & 1 == 1 {
            let left = self.nodes.pop().expect("an odd index has a left sibling");
            hash = Hash::node(&left, &hash);
            level += 1;
            index >>= 1;
            created.push((NodeId::new(level, index), hash));
        }
        self.nodes.push(hash);
        self.size += 1;
        created
    }

    /// \[M19\] Appends a whole complete node, which must start where the range ends: `false`,
    /// and nothing appended, otherwise. Range proofs append the subtrees right of a range this
    /// way.
    pub fn append_node(&mut self, id: NodeId, hash: Hash) -> bool {
        if id.level >= 64 || id.begin() != self.size || id.begin() >> id.level != id.index {
            return false;
        }
        let mut index = id.index;
        let mut hash = hash;
        while index & 1 == 1 {
            let left = self
                .nodes
                .pop()
                .expect("an odd index has a left sibling of its size");
            hash = Hash::node(&left, &hash);
            index >>= 1;
        }
        self.nodes.push(hash);
        self.size += 1 << id.level;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(i: u64) -> Hash {
        Hash::leaf(&i.to_be_bytes())
    }

    /// RFC 6962's recursive definition of the tree hash, written out literally.
    fn mth(leaves: &[Hash]) -> Hash {
        match leaves.len() {
            0 => Hash::empty(),
            1 => leaves[0],
            n => {
                let k = split(n as u64) as usize;
                Hash::node(&mth(&leaves[..k]), &mth(&leaves[k..]))
            }
        }
    }

    #[test]
    fn splits() {
        let cases = [
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 4),
            (8, 4),
            (9, 8),
            (1 << 40, 1 << 39),
        ];
        for (n, k) in cases {
            assert_eq!(split(n), k, "{n}");
        }
    }

    #[test]
    fn ranges_decompose_into_aligned_nodes() {
        assert_eq!(range_nodes(0, 0), vec![]);
        assert_eq!(range_nodes(0, 1), vec![NodeId::new(0, 0)]);
        assert_eq!(
            range_nodes(0, 7),
            vec![NodeId::new(2, 0), NodeId::new(1, 2), NodeId::new(0, 6)]
        );
        assert_eq!(range_nodes(4, 8), vec![NodeId::new(2, 1)]);
        assert_eq!(
            range_nodes(8, 13),
            vec![NodeId::new(2, 2), NodeId::new(0, 12)]
        );
        for (b, e) in [(0, 13), (8, 13), (12, 13), (16, 31)] {
            let nodes = range_nodes(b, e);
            assert_eq!(nodes.first().map(NodeId::begin), Some(b));
            assert_eq!(nodes.last().map(NodeId::end), Some(e));
            for pair in nodes.windows(2) {
                assert_eq!(pair[0].end(), pair[1].begin());
                assert!(pair[0].level > pair[1].level);
            }
        }
    }

    #[test]
    fn roots_match_the_recursive_definition() {
        let leaves: Vec<Hash> = (0..70).map(leaf).collect();
        let mut tree = MemTree::new();
        let mut compact = CompactRange::new();
        assert_eq!(tree.root(), Hash::empty());
        assert_eq!(compact.root(), Hash::empty());
        for (i, &l) in leaves.iter().enumerate() {
            let created = tree.push(l);
            assert_eq!(compact.append(l), created);
            assert_eq!(tree.root(), mth(&leaves[..=i]), "size {}", i + 1);
            assert_eq!(compact.root(), tree.root());
            assert_eq!(tree.compact_range(), compact);
        }
        for size in 0..=70 {
            assert_eq!(tree.root_at(size), Some(mth(&leaves[..size as usize])));
        }
        assert_eq!(tree.root_at(71), None);
        assert_eq!(CompactRange::from_parts(70, Vec::new()), None);
    }

    #[test]
    fn appends_create_the_complete_nodes() {
        let mut tree = MemTree::new();
        assert_eq!(tree.push(leaf(0)), vec![(NodeId::new(0, 0), leaf(0))]);
        let created = tree.push(leaf(1));
        assert_eq!(
            created.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![NodeId::new(0, 1), NodeId::new(1, 0)]
        );
        tree.push(leaf(2));
        let created = tree.push(leaf(3));
        assert_eq!(
            created.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![NodeId::new(0, 3), NodeId::new(1, 1), NodeId::new(2, 0)]
        );
        assert_eq!(tree.node(NodeId::new(2, 0)), Some(tree.root()));
        assert_eq!(tree.node(NodeId::new(2, 1)), None);
    }

    #[test]
    fn whole_nodes_append_like_their_leaves() {
        let leaves: Vec<Hash> = (0..40).map(leaf).collect();
        let tree = MemTree::from_leaves(leaves.iter().copied());
        for begin in 0..=40u64 {
            for end in begin..=40u64 {
                // Leaves up to `begin`, then the complete nodes of the rest.
                let mut range = CompactRange::new();
                for &l in &leaves[..begin as usize] {
                    range.append(l);
                }
                for id in range_nodes(begin, end) {
                    assert!(range.append_node(id, tree.node(id).unwrap()), "{id:?}");
                }
                assert_eq!(range.size(), end);
                assert_eq!(range.root(), tree.root_at(end).unwrap(), "{begin}..{end}");
            }
        }
        // Only where the range ends.
        let mut range = CompactRange::new();
        range.append(leaves[0]);
        assert!(!range.append_node(NodeId::new(1, 0), Hash::empty()));
        assert!(!range.append_node(NodeId::new(1, 1), Hash::empty()));
        assert!(range.append_node(NodeId::new(0, 1), leaves[1]));
        assert!(range.append_node(NodeId::new(1, 1), tree.node(NodeId::new(1, 1)).unwrap()));
        assert_eq!(range.root(), tree.root_at(4).unwrap());
        assert_eq!(tree.range_proof(3, 2, 10), None);
        assert_eq!(tree.range_proof(0, 41, 41), None);
    }

    #[test]
    fn compact_ranges_have_one_node_per_bit() {
        assert!(CompactRange::from_parts(5, vec![Hash::empty(); 2]).is_some());
        assert!(CompactRange::from_parts(5, vec![Hash::empty(); 1]).is_none());
        assert_eq!(
            CompactRange::ids(5),
            vec![NodeId::new(2, 0), NodeId::new(0, 4)]
        );
    }

    #[test]
    fn proof_ranges() {
        assert_eq!(inclusion_ranges(0, 0), None);
        assert_eq!(inclusion_ranges(0, 1), Some(vec![]));
        // PATH(5, D[8]): leaf 4, then [6, 8), then [0, 4).
        assert_eq!(inclusion_ranges(5, 8), Some(vec![4..5, 6..8, 0..4]));
        assert_eq!(consistency_ranges(3, 2), None);
        assert_eq!(consistency_ranges(0, 5), Some(vec![]));
        assert_eq!(consistency_ranges(5, 5), Some(vec![]));
        // PROOF(3, D[4]) = [D[2], D[3], MTH(D[0:2])].
        assert_eq!(consistency_ranges(3, 4), Some(vec![2..3, 3..4, 0..2]));
        assert_eq!(
            nodes_for(&[2..3, 3..4, 0..2]),
            vec![NodeId::new(0, 2), NodeId::new(0, 3), NodeId::new(1, 0)]
        );
    }
}
