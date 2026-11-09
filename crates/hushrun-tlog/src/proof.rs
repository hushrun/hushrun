//! Verifying inclusion and consistency proofs, as RFC 9162 §2.1.3.2 and §2.1.4.2 describe.

use std::fmt;

use crate::hash::Hash;
use crate::tree::{CompactRange, range_nodes};

/// Why a proof doesn't verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofError {
    /// The leaf index is not below the tree size.
    IndexOutOfRange { index: u64, size: u64 },
    /// The old tree is larger than the new one.
    SizesOutOfOrder { old: u64, new: u64 },
    /// The proof has too few or too many hashes for these sizes.
    WrongLength { got: usize },
    /// The hashes don't lead to the expected roots.
    Mismatch,
}

impl fmt::Display for ProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProofError::IndexOutOfRange { index, size } => {
                write!(f, "leaf {index} is not in a tree of {size} leaves")
            }
            ProofError::SizesOutOfOrder { old, new } => {
                write!(
                    f,
                    "a tree of {old} leaves can't be a prefix of one of {new}"
                )
            }
            ProofError::WrongLength { got } => {
                write!(f, "a proof of {got} hashes doesn't fit these tree sizes")
            }
            ProofError::Mismatch => f.write_str("the proof doesn't lead to the signed root"),
        }
    }
}

impl std::error::Error for ProofError {}

/// The root an inclusion proof of `leaf` at `index`, in a tree of `size` leaves, leads to.
pub fn root_from_inclusion(
    leaf: &Hash,
    index: u64,
    size: u64,
    proof: &[Hash],
) -> Result<Hash, ProofError> {
    if index >= size {
        return Err(ProofError::IndexOutOfRange { index, size });
    }
    let wrong = || ProofError::WrongLength { got: proof.len() };
    let (mut f_n, mut s_n) = (index, size - 1);
    let mut r = *leaf;
    for p in proof {
        if s_n == 0 {
            return Err(wrong());
        }
        if f_n & 1 == 1 || f_n == s_n {
            r = Hash::node(p, &r);
            while f_n & 1 == 0 && f_n != 0 {
                f_n >>= 1;
                s_n >>= 1;
            }
        } else {
            r = Hash::node(&r, p);
        }
        f_n >>= 1;
        s_n >>= 1;
    }
    if s_n != 0 {
        return Err(wrong());
    }
    Ok(r)
}

/// Checks that `leaf` is leaf `index` of the tree of `size` leaves whose root is `root`.
pub fn verify_inclusion(
    leaf: &Hash,
    index: u64,
    size: u64,
    proof: &[Hash],
    root: &Hash,
) -> Result<(), ProofError> {
    if root_from_inclusion(leaf, index, size, proof)? == *root {
        Ok(())
    } else {
        Err(ProofError::Mismatch)
    }
}

/// Checks that the tree of `old` leaves with root `old_root` is a prefix of the tree of `new`
/// leaves with root `new_root`. Equal sizes need equal roots and no proof; every tree extends
/// the empty one, whose root is [`Hash::empty`].
pub fn verify_consistency(
    old: u64,
    new: u64,
    proof: &[Hash],
    old_root: &Hash,
    new_root: &Hash,
) -> Result<(), ProofError> {
    if old > new {
        return Err(ProofError::SizesOutOfOrder { old, new });
    }
    let wrong = || ProofError::WrongLength { got: proof.len() };
    if old == new || old == 0 {
        if !proof.is_empty() {
            return Err(wrong());
        }
        let expected = if old == 0 { Hash::empty() } else { *new_root };
        return if *old_root == expected {
            Ok(())
        } else {
            Err(ProofError::Mismatch)
        };
    }
    if proof.is_empty() {
        return Err(wrong());
    }
    // A power-of-two old tree is a complete node the proof leaves out: it starts the path.
    let mut path = Vec::with_capacity(proof.len() + 1);
    if old.is_power_of_two() {
        path.push(*old_root);
    }
    path.extend_from_slice(proof);
    let (mut f_n, mut s_n) = (old - 1, new - 1);
    while f_n & 1 == 1 {
        f_n >>= 1;
        s_n >>= 1;
    }
    let (mut f_r, mut s_r) = (path[0], path[0]);
    for c in &path[1..] {
        if s_n == 0 {
            return Err(wrong());
        }
        if f_n & 1 == 1 || f_n == s_n {
            f_r = Hash::node(c, &f_r);
            s_r = Hash::node(c, &s_r);
            while f_n & 1 == 0 && f_n != 0 {
                f_n >>= 1;
                s_n >>= 1;
            }
        } else {
            s_r = Hash::node(&s_r, c);
        }
        f_n >>= 1;
        s_n >>= 1;
    }
    if s_n != 0 {
        return Err(wrong());
    }
    if f_r == *old_root && s_r == *new_root {
        Ok(())
    } else {
        Err(ProofError::Mismatch)
    }
}

/// \[M19\] The root of a tree of `size` leaves, from consecutive leaves starting at `begin` and
/// the complete nodes of the leaves before and after them (a range proof, as
/// [`MemTree::range_proof`](crate::tree::MemTree::range_proof) gives them). The leaves are
/// proven to be those positions of the tree whose root this is.
pub fn root_from_range(
    size: u64,
    begin: u64,
    leaves: &[Hash],
    left: &[Hash],
    right: &[Hash],
) -> Result<Hash, ProofError> {
    let end = begin
        .checked_add(leaves.len() as u64)
        .filter(|end| *end <= size)
        .ok_or(ProofError::IndexOutOfRange { index: begin, size })?;
    let right_ids = range_nodes(end, size);
    let wrong = || ProofError::WrongLength {
        got: left.len() + right.len(),
    };
    if right.len() != right_ids.len() {
        return Err(wrong());
    }
    let mut range = CompactRange::from_parts(begin, left.to_vec()).ok_or_else(wrong)?;
    for leaf in leaves {
        range.append(*leaf);
    }
    for (id, hash) in right_ids.into_iter().zip(right) {
        if !range.append_node(id, *hash) {
            return Err(wrong());
        }
    }
    Ok(range.root())
}

/// \[M19\] Checks that `leaves` are the leaves of the tree of `size` leaves with root `root`
/// that start at `begin`.
pub fn verify_range(
    size: u64,
    begin: u64,
    leaves: &[Hash],
    left: &[Hash],
    right: &[Hash],
    root: &Hash,
) -> Result<(), ProofError> {
    if root_from_range(size, begin, leaves, left, right)? == *root {
        Ok(())
    } else {
        Err(ProofError::Mismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::MemTree;

    fn tree(n: u64) -> MemTree {
        MemTree::from_leaves((0..n).map(|i| Hash::leaf(&i.to_le_bytes())))
    }

    // covers: L-11
    #[test]
    fn every_inclusion_proof_verifies_and_no_other_does() {
        let t = tree(33);
        for size in 1..=33 {
            let root = t.root_at(size).unwrap();
            for index in 0..size {
                let leaf = t.leaf(index).unwrap();
                let proof = t.inclusion_proof(index, size).unwrap();
                assert_eq!(verify_inclusion(&leaf, index, size, &proof, &root), Ok(()));
                // Another leaf, index, size, root or proof doesn't.
                let other = Hash::leaf(b"other");
                assert!(verify_inclusion(&other, index, size, &proof, &root).is_err());
                if size > 1 {
                    let next = (index + 1) % size;
                    assert!(verify_inclusion(&leaf, next, size, &proof, &root).is_err());
                    assert!(verify_inclusion(&leaf, index, size, &proof[1..], &root).is_err());
                    let mut longer = proof.clone();
                    longer.push(root);
                    assert!(verify_inclusion(&leaf, index, size, &longer, &root).is_err());
                    let mut bent = proof.clone();
                    bent[0] = other;
                    assert!(verify_inclusion(&leaf, index, size, &bent, &root).is_err());
                }
                assert!(verify_inclusion(&leaf, index, size, &proof, &Hash::empty()).is_err());
            }
            assert_eq!(
                verify_inclusion(&Hash::empty(), size, size, &[], &root),
                Err(ProofError::IndexOutOfRange { index: size, size })
            );
        }
    }

    // covers: L-11
    #[test]
    fn every_consistency_proof_verifies_and_no_other_does() {
        let t = tree(33);
        for new in 0..=33 {
            let new_root = t.root_at(new).unwrap();
            for old in 0..=new {
                let old_root = t.root_at(old).unwrap();
                let proof = t.consistency_proof(old, new).unwrap();
                assert_eq!(
                    verify_consistency(old, new, &proof, &old_root, &new_root),
                    Ok(()),
                    "{old} -> {new}"
                );
                let other = Hash::leaf(b"other");
                if old > 0 && old < new {
                    assert!(verify_consistency(old, new, &proof, &other, &new_root).is_err());
                    assert!(verify_consistency(old, new, &proof, &old_root, &other).is_err());
                    let mut bent = proof.clone();
                    bent[proof.len() - 1] = other;
                    assert!(verify_consistency(old, new, &bent, &old_root, &new_root).is_err());
                    assert!(
                        verify_consistency(old, new, &proof[1..], &old_root, &new_root).is_err()
                    );
                    assert!(verify_consistency(old, new, &[], &old_root, &new_root).is_err());
                } else if old == new {
                    assert!(verify_consistency(old, new, &[], &other, &new_root).is_err());
                    assert!(verify_consistency(old, new, &[other], &old_root, &new_root).is_err());
                }
            }
        }
        assert_eq!(
            verify_consistency(2, 1, &[], &Hash::empty(), &Hash::empty()),
            Err(ProofError::SizesOutOfOrder { old: 2, new: 1 })
        );
        // Only the empty tree's root is the root of 0 leaves.
        let root = t.root_at(5).unwrap();
        assert!(verify_consistency(0, 5, &[], &Hash::leaf(b"x"), &root).is_err());
    }

    // covers: L-12
    #[test]
    fn every_range_proof_verifies_and_no_other_does() {
        let t = tree(21);
        let leaves: Vec<Hash> = (0..21).map(|i| t.leaf(i).unwrap()).collect();
        for size in 0..=21u64 {
            let root = t.root_at(size).unwrap();
            for begin in 0..=size {
                for end in begin..=size {
                    let (left, right) = t.range_proof(begin, end, size).unwrap();
                    let range = &leaves[begin as usize..end as usize];
                    assert_eq!(
                        verify_range(size, begin, range, &left, &right, &root),
                        Ok(()),
                        "{begin}..{end} of {size}"
                    );
                    if !range.is_empty() {
                        // Another leaf, a shifted range, a dropped leaf: none verifies.
                        let mut bent = range.to_vec();
                        bent[0] = Hash::leaf(b"other");
                        assert!(verify_range(size, begin, &bent, &left, &right, &root).is_err());
                        if end < size {
                            assert!(
                                verify_range(size, begin + 1, range, &left, &right, &root).is_err()
                            );
                        }
                        assert!(
                            verify_range(size, begin, &range[1..], &left, &right, &root).is_err()
                        );
                    }
                    if !right.is_empty() {
                        assert!(
                            verify_range(size, begin, range, &left, &right[1..], &root).is_err()
                        );
                    }
                }
            }
        }
        assert!(matches!(
            verify_range(3, 2, &leaves[..2], &[], &[], &Hash::empty()),
            Err(ProofError::IndexOutOfRange { .. })
        ));
    }

    #[test]
    fn a_different_history_is_not_consistent() {
        let honest = tree(20);
        // The same first 9 leaves, then another tenth.
        let mut fork = MemTree::from_leaves((0..9).map(|i| Hash::leaf(&(i as u64).to_le_bytes())));
        fork.push(Hash::leaf(b"rewritten"));
        for i in 10..20u64 {
            fork.push(Hash::leaf(&i.to_le_bytes()));
        }
        assert_eq!(honest.root_at(9), fork.root_at(9));
        for old in 10..=20 {
            let proof = fork.consistency_proof(old, 20).unwrap();
            assert!(
                verify_consistency(old, 20, &proof, &honest.root_at(old).unwrap(), &fork.root())
                    .is_err()
            );
        }
    }
}
