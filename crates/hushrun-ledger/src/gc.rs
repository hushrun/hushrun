//! Garbage collection (L-9): which keys `hushrun repository gc --older-than <duration>` deletes.
//!
//! Selection works on keys alone: an event key carries its write time in its ULID, and a
//! deployed-index key in its inverted timestamp, both from the signer's clock. A key GC doesn't
//! understand is kept, and so is everything outside `v1/`. Policies (`v1/policies/`, L-2) are
//! always kept: they are small, and every remaining event's `policyDigest` may name one.
//!
//! The deployed index is kept per (scope, segment): each `v1/deployed/<scope>/<segment>/` that
//! the ledger lists keeps its newest deployment, a stage's segment or `_` alike, whether or not
//! hushrun.yaml still names the stage.

use std::collections::BTreeSet;

use hushrun_core::digest::Digest;
use hushrun_core::event::VerifiedEvent;
use hushrun_core::names::{ScopeName, StageName};

use crate::layout::{parse_deployed_key, parse_event_key};
use crate::ulid::ulid_millis;

/// What GC keeps however old: the newest deployed entry of each (scope, segment), and every
/// event of the digest each one deployed.
#[derive(Debug, Default, Clone)]
pub struct Keep {
    pub deployed: BTreeSet<String>,
    pub digests: BTreeSet<(ScopeName, Digest)>,
}

/// The (scope, segment) pairs of the deployed-index keys among `keys`: a stage, or `None` for
/// `_`. GC reads each one's entries to find its newest deployment.
pub fn deployed_segments(keys: &[String]) -> BTreeSet<(ScopeName, Option<StageName>)> {
    keys.iter()
        .filter_map(|k| parse_deployed_key(k))
        .map(|k| (k.scope, k.stage))
        .collect()
}

/// The newest of `entries`, verified entries of one segment of the deployed index, by signed
/// time and then the smaller key (P-10's tie): its key and the digest it deployed.
pub fn newest<'a>(entries: impl Iterator<Item = &'a VerifiedEvent>) -> Option<(String, Digest)> {
    entries
        .max_by(|a, b| {
            a.event
                .time
                .cmp(&b.event.time)
                .then_with(|| b.key.cmp(&a.key))
        })
        .map(|e| (e.key.clone(), e.digest))
}

/// The keys of `keys` to delete, sorted: events and deployed-index entries written before
/// `cutoff_ms`, except what `keep` names.
pub fn select(keys: &[String], keep: &Keep, cutoff_ms: i64) -> Vec<String> {
    let mut delete: Vec<String> = keys
        .iter()
        .filter(|key| {
            if let Some(event) = parse_event_key(key) {
                !keep.digests.contains(&(event.scope, event.digest))
                    && ulid_millis(&event.ulid).is_some_and(|ms| ms < cutoff_ms)
            } else if let Some(entry) = parse_deployed_key(key) {
                !keep.deployed.contains(*key) && entry.millis < cutoff_ms
            } else {
                false
            }
        })
        .cloned()
        .collect();
    delete.sort();
    delete
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{deployed_key, event_key};
    use crate::new_ulid;
    use hushrun_core::event::EventKind;
    use hushrun_core::names::StageName;
    use hushrun_core::time::Time;

    const DAY: i64 = 86_400_000;

    fn digest(n: u8) -> Digest {
        Digest::of(&[n])
    }

    fn event(scope: &ScopeName, d: u8, day: i64) -> String {
        event_key(
            scope,
            &digest(d),
            &new_ulid(day * DAY),
            EventKind::Passed,
            Some(&StageName::new("staging").unwrap()),
        )
    }

    // covers: L-9
    #[test]
    fn deletes_old_events_except_the_deployed_digest() {
        let api = ScopeName::new("api").unwrap();
        let web = ScopeName::new("web").unwrap();
        let production = StageName::new("production").unwrap();
        let deployed_old = deployed_key(
            &api,
            Some(&production),
            Time::from_millis(10 * DAY).unwrap(),
            &new_ulid(10 * DAY),
        );
        let deployed_older = deployed_key(
            &api,
            Some(&production),
            Time::from_millis(5 * DAY).unwrap(),
            &new_ulid(5 * DAY),
        );
        let deployed_new = deployed_key(
            &web,
            Some(&production),
            Time::from_millis(300 * DAY).unwrap(),
            &new_ulid(300 * DAY),
        );
        let keys = vec![
            event(&api, 1, 10),  // old, but its digest is deployed: kept
            event(&api, 1, 1),   // older still, same digest: kept
            event(&api, 2, 20),  // old, another digest: deleted
            event(&web, 1, 20),  // same digest in another scope: deleted
            event(&api, 3, 250), // recent: kept
            deployed_old.clone(),
            deployed_older.clone(),
            deployed_new.clone(),
            "v1/events/api/not-a-digest/x.json".to_string(),
            "trust.yaml".to_string(),
            crate::layout::policy_key(&digest(9)),
        ];
        let keep = Keep {
            deployed: [deployed_old.clone()].into(),
            digests: [(api.clone(), digest(1))].into(),
        };
        let cutoff = 200 * DAY;
        let mut expected = vec![keys[2].clone(), keys[3].clone(), deployed_older];
        expected.sort();
        assert_eq!(select(&keys, &keep, cutoff), expected);
        // Nothing is old: nothing goes.
        assert!(select(&keys, &keep, 0).is_empty());
    }

    // covers: L-9 (0.8's stage segments and `_` side by side)
    #[test]
    fn every_segment_of_the_deployed_index_is_kept_on_its_own() {
        let api = ScopeName::new("api").unwrap();
        let production = StageName::new("production").unwrap();
        let at = |stage: Option<&StageName>, day: i64| {
            deployed_key(
                &api,
                stage,
                Time::from_millis(day * DAY).unwrap(),
                &new_ulid(day * DAY),
            )
        };
        let staged_newest = at(Some(&production), 10);
        let staged_older = at(Some(&production), 5);
        let stageless_newest = at(None, 20);
        let stageless_older = at(None, 15);
        let keys = vec![
            staged_older.clone(),
            stageless_older.clone(),
            staged_newest.clone(),
            stageless_newest.clone(),
            // A segment that is neither a stage nor `_`: GC doesn't understand it, so keeps it.
            format!("v1/deployed/api/Prod/{:013}.{}.json", 1, new_ulid(DAY)),
        ];
        assert_eq!(
            deployed_segments(&keys),
            [(api.clone(), None), (api.clone(), Some(production.clone()))].into()
        );
        // GC keeps each segment's newest: the stage's, though older than `_`'s.
        let keep = Keep {
            deployed: [staged_newest.clone(), stageless_newest.clone()].into(),
            digests: BTreeSet::new(),
        };
        let mut expected = vec![staged_older, stageless_older];
        expected.sort();
        assert_eq!(select(&keys, &keep, 300 * DAY), expected);

        // The newest by signed time, then the smaller key.
        let entry = |key: &str, d: u8, ms: i64| VerifiedEvent {
            key: key.to_string(),
            signer: "ci".into(),
            effective_custody: hushrun_core::run::Custody::Ci,
            digest: digest(d),
            event: sample_event(ms),
            trusted_time: None,
        };
        let entries = [
            entry("v1/deployed/api/_/b", 1, DAY),
            entry("v1/deployed/api/_/c", 2, 2 * DAY),
            entry("v1/deployed/api/_/a", 3, 2 * DAY),
        ];
        assert_eq!(
            newest(entries.iter()),
            Some(("v1/deployed/api/_/a".to_string(), digest(3)))
        );
        assert_eq!(newest(entries[..0].iter()), None);
    }

    fn sample_event(ms: i64) -> hushrun_core::event::Event {
        let mut event: hushrun_core::event::Event = serde_json::from_value(serde_json::json!({
            "kind": "deployed",
            "repository": "github.com/acme/app",
            "scope": "api",
            "canonicalization": hushrun_core::CANONICALIZATION,
            "source": {"vcs": "git"},
            "run": {"provider": "generic", "id": "1", "attempt": 1, "trigger": "push"},
            "producer": {"tool": "hushrun", "version": "0.9.0", "custody": "ci"},
            "time": "2026-01-01T00:00:00Z",
            "policyDigest": Digest::of(b"policy").to_string(),
            "deployed": {"artifacts": []},
        }))
        .unwrap();
        event.time = Time::from_millis(ms).unwrap();
        event
    }
}
