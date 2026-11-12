//! Audit (B-16): a share of the compiles that could be reused run anyway and are compared byte
//! for byte with the block. A difference serves the fresh build and suspends the key.

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

/// One compile in this many is audited.
pub const ONE_IN: u64 = 50;

/// The variable that sets how many compiles a run audits ([`Audit::from_var`]). Read once, as
/// `hushrun run` begins, so every compile of the run sees one answer.
pub const VAR: &str = "HUSHRUN_BUILD_AUDIT";

/// How many of the compiles that could be reused a run audits (B-16).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Audit {
    /// One in [`ONE_IN`], picked by [`picked`].
    #[default]
    OneIn,
    /// Every one: what a replay that compares two machines asks for.
    Every,
    /// None: what a test that expects a reuse asks for.
    Never,
}

impl Audit {
    /// The value of [`VAR`]: `every`, `none`, or anything else (unset among them) for one in
    /// fifty.
    pub fn from_var(value: Option<&str>) -> Audit {
        match value {
            Some("every") => Audit::Every,
            Some("none") => Audit::Never,
            _ => Audit::OneIn,
        }
    }

    /// Whether this compile is audited. `sample` is the one-in-fifty pick, asked only when the
    /// run left it to chance.
    pub fn picks(self, sample: impl FnOnce() -> bool) -> bool {
        match self {
            Audit::OneIn => sample(),
            Audit::Every => true,
            Audit::Never => false,
        }
    }
}

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

    // covers: B-16
    #[test]
    fn a_run_may_audit_every_compile_or_none_and_otherwise_leaves_it_to_the_pick() {
        assert_eq!(Audit::from_var(None), Audit::OneIn);
        assert_eq!(Audit::from_var(Some("")), Audit::OneIn);
        assert_eq!(Audit::from_var(Some("all")), Audit::OneIn);
        assert_eq!(Audit::from_var(Some("every")), Audit::Every);
        assert_eq!(Audit::from_var(Some("none")), Audit::Never);
        assert!(Audit::Every.picks(|| unreachable!("not left to chance")));
        assert!(!Audit::Never.picks(|| unreachable!("not left to chance")));
        assert!(Audit::OneIn.picks(|| true));
        assert!(!Audit::OneIn.picks(|| false));
    }
}
