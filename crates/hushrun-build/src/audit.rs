//! Audit (B-16): a share of the compiles that could be reused run anyway and are compared byte
//! for byte with the block. A difference serves the fresh build and suspends the key.

use hushrun_core::Digest;

/// One compile in this many is audited.
pub const ONE_IN: u64 = 50;

/// Whether the audit picks this compile (B-16): a hash of the key and the run's start, so nobody
/// chooses, and the same compile in the same run always gets the same answer.
pub fn picked(key: &Digest, started_ms: u64) -> bool {
    let mut bytes = Vec::with_capacity(40);
    bytes.extend_from_slice(key.as_bytes());
    bytes.extend_from_slice(&started_ms.to_be_bytes());
    let hash = Digest::of(&bytes);
    let mut first = [0u8; 8];
    first.copy_from_slice(&hash.as_bytes()[..8]);
    u64::from_be_bytes(first) % ONE_IN == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-16
    #[test]
    fn about_one_compile_in_fifty_is_picked_and_the_pick_is_stable() {
        let keys: Vec<Digest> = (0u32..20_000)
            .map(|n| Digest::of(&n.to_be_bytes()))
            .collect();
        let picked_now = keys
            .iter()
            .filter(|key| picked(key, 1_800_000_000_000))
            .count();
        // 400 expected; far outside this range means the pick is broken, not unlucky.
        assert!((300..500).contains(&picked_now), "{picked_now}");
        for key in keys.iter().take(200) {
            assert_eq!(picked(key, 7), picked(key, 7));
        }
        // Another run picks other compiles.
        let other = keys
            .iter()
            .filter(|key| picked(key, 1) != picked(key, 2))
            .count();
        assert!(other > 0);
    }
}
