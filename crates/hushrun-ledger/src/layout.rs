//! The ledger layout (L-2), relative to the ledger URL:
//!
//! ```text
//! v1/events/<scope>/<digest hex>/<ULID>.<kind>.<stage or _>.json
//! v1/deployed/<scope>/<stage or _>/<inverted ms>.<ULID>.json
//! v1/policies/<sha256 hex>.yaml           [M18] hushrun.yaml files that `plan --start` evaluated
//! ```

use hushrun_core::digest::Digest;
use hushrun_core::event::EventKind;
use hushrun_core::names::{ScopeName, StageName};
use hushrun_core::time::Time;

/// `9999999999999 − unix_ms`, so the newest deployment sorts first.
pub const INVERTED_MS_BASE: i64 = 9_999_999_999_999;

/// `v1/events/<scope>/<digest hex>/`
pub fn events_prefix(scope: &ScopeName, digest: &Digest) -> String {
    format!("v1/events/{scope}/{}/", digest.hex())
}

/// `v1/events/<scope>/`
pub fn scope_events_prefix(scope: &ScopeName) -> String {
    format!("v1/events/{scope}/")
}

/// `v1/events/<scope>/<digest hex>/<ULID>.<kind>.<stage or _>.json`
pub fn event_key(
    scope: &ScopeName,
    digest: &Digest,
    ulid: &str,
    kind: EventKind,
    stage: Option<&StageName>,
) -> String {
    format!(
        "{}{ulid}.{kind}.{}.json",
        events_prefix(scope, digest),
        stage.map_or("_", |s| s.as_str())
    )
}

/// `v1/deployed/<scope>/<stage or _>/`
pub fn deployed_prefix(scope: &ScopeName, stage: Option<&StageName>) -> String {
    format!("v1/deployed/{scope}/{}/", stage.map_or("_", |s| s.as_str()))
}

/// `v1/deployed/<scope>/<stage or _>/<inverted ms>.<ULID>.json`: `_` for a deployment without a
/// stage.
pub fn deployed_key(
    scope: &ScopeName,
    stage: Option<&StageName>,
    time: Time,
    ulid: &str,
) -> String {
    let inverted = (INVERTED_MS_BASE - time.millis()).max(0);
    format!(
        "{}{inverted:013}.{ulid}.json",
        deployed_prefix(scope, stage)
    )
}

/// `v1/policies/`
pub const POLICIES_PREFIX: &str = "v1/policies/";

/// `v1/policies/<sha256 hex>.yaml`: the `hushrun.yaml` bytes whose SHA-256 is `digest`, the
/// `policyDigest` of the events they produced (E-4).
pub fn policy_key(digest: &Digest) -> String {
    format!("{POLICIES_PREFIX}{}.yaml", digest.hex())
}

/// The digest a `v1/policies/<sha256 hex>.yaml` key names.
pub fn parse_policy_key(key: &str) -> Option<Digest> {
    let hex = key.strip_prefix(POLICIES_PREFIX)?.strip_suffix(".yaml")?;
    Digest::from_hex(hex).ok()
}

/// A parsed event key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventKey {
    pub scope: ScopeName,
    pub digest: Digest,
    pub ulid: String,
    pub kind: EventKind,
    pub stage: Option<StageName>,
}

/// Parses `v1/events/<scope>/<digest hex>/<ULID>.<kind>.<stage or _>.json`.
pub fn parse_event_key(key: &str) -> Option<EventKey> {
    let rest = key.strip_prefix("v1/events/")?;
    let mut parts = rest.split('/');
    let scope = ScopeName::new(parts.next()?).ok()?;
    let digest = Digest::from_hex(parts.next()?).ok()?;
    let file = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let name = file.strip_suffix(".json")?;
    let mut fields = name.split('.');
    let ulid = fields.next()?;
    let kind = EventKind::parse(fields.next()?)?;
    let stage = match fields.next()? {
        "_" => None,
        stage => Some(StageName::new(stage).ok()?),
    };
    if fields.next().is_some() || ulid.len() != 26 {
        return None;
    }
    Some(EventKey {
        scope,
        digest,
        ulid: ulid.to_string(),
        kind,
        stage,
    })
}

/// A parsed deployed-index key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployedKey {
    pub scope: ScopeName,
    /// `None` for `_`: a deployment without a stage.
    pub stage: Option<StageName>,
    /// Milliseconds since the epoch, from the inverted timestamp (the signer's clock).
    pub millis: i64,
    pub ulid: String,
}

/// Parses `v1/deployed/<scope>/<stage or _>/<inverted ms>.<ULID>.json`.
pub fn parse_deployed_key(key: &str) -> Option<DeployedKey> {
    let rest = key.strip_prefix("v1/deployed/")?;
    let mut parts = rest.split('/');
    let scope = ScopeName::new(parts.next()?).ok()?;
    let stage = match parts.next()? {
        "_" => None,
        stage => Some(StageName::new(stage).ok()?),
    };
    let file = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (inverted, rest) = file.strip_suffix(".json")?.split_once('.')?;
    // Exactly 13 digits: `i64::from_str` would also take a sign.
    if inverted.len() != 13 || !inverted.bytes().all(|b| b.is_ascii_digit()) || rest.len() != 26 {
        return None;
    }
    let inverted: i64 = inverted.parse().ok()?;
    Some(DeployedKey {
        scope,
        stage,
        millis: INVERTED_MS_BASE - inverted,
        ulid: rest.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ULID: &str = "01JA0000000000000000000000";

    // covers: L-2
    #[test]
    fn event_keys_round_trip() {
        let scope = ScopeName::new("default").unwrap();
        let stage = StageName::new("staging").unwrap();
        let digest = Digest::of(b"x");
        let key = event_key(&scope, &digest, ULID, EventKind::Check, Some(&stage));
        assert_eq!(
            key,
            format!(
                "v1/events/default/{}/{ULID}.check.staging.json",
                digest.hex()
            )
        );
        let parsed = parse_event_key(&key).unwrap();
        assert_eq!(parsed.kind, EventKind::Check);
        assert_eq!(parsed.stage.as_ref(), Some(&stage));
        assert_eq!(parsed.digest, digest);
        let source = event_key(&scope, &digest, ULID, EventKind::Check, None);
        assert!(source.ends_with(".check._.json"));
        assert_eq!(parse_event_key(&source).unwrap().stage, None);
        for bad in [
            "v1/events/default/xyz/a.check._.json",
            &format!("v1/events/Default/{}/{ULID}.check._.json", digest.hex()),
            &format!("v1/events/default/{}/{ULID}.nope._.json", digest.hex()),
            &format!("v1/events/default/{}/{ULID}.check._.yaml", digest.hex()),
            &format!("v1/events/default/{}/sub/{ULID}.check._.json", digest.hex()),
        ] {
            assert!(parse_event_key(bad).is_none(), "{bad}");
        }
    }

    // covers: L-2
    #[test]
    fn policy_keys_name_the_policy_digest() {
        let digest = Digest::of(b"version: 1\n");
        let key = policy_key(&digest);
        assert_eq!(key, format!("v1/policies/{}.yaml", digest.hex()));
        assert_eq!(parse_policy_key(&key), Some(digest));
        for bad in [
            "v1/policies/abc.yaml".to_string(),
            format!("v1/policies/{}.yml", digest.hex()),
            format!("v1/policies/{}.yaml", digest.hex().to_uppercase()),
            format!("v1/policies/x/{}.yaml", digest.hex()),
        ] {
            assert_eq!(parse_policy_key(&bad), None, "{bad}");
        }
        // No other reader parses it as an event or a deployment.
        assert!(parse_event_key(&key).is_none() && parse_deployed_key(&key).is_none());
    }

    // covers: L-2, L-8
    #[test]
    fn deployed_keys_sort_newest_first() {
        let scope = ScopeName::new("default").unwrap();
        let stage = StageName::new("production").unwrap();
        let older = Time::parse("2026-10-13T15:04:05.123Z").unwrap();
        let newer = Time::parse("2026-10-13T15:04:05.124Z").unwrap();
        let a = deployed_key(&scope, Some(&stage), older, ULID);
        let b = deployed_key(&scope, Some(&stage), newer, ULID);
        assert!(b < a, "{b} sorts before {a}");
        assert_eq!(
            a,
            format!(
                "v1/deployed/default/production/{:013}.{ULID}.json",
                INVERTED_MS_BASE - older.millis()
            )
        );
        let parsed = parse_deployed_key(&a).unwrap();
        assert_eq!(parsed.millis, older.millis());
        assert_eq!(parsed.stage, Some(stage));
        // The timestamp is 13 digits: no sign, no space.
        for bad in [
            "+999999999999",
            "-999999999999",
            " 999999999999",
            "99999999999 9",
            "99999999999.9",
            "999999999999",
        ] {
            let key = format!("v1/deployed/default/production/{bad}.{ULID}.json");
            assert!(parse_deployed_key(&key).is_none(), "{key}");
        }
    }

    // covers: L-2
    #[test]
    fn deployed_keys_without_a_stage_use_an_underscore() {
        let scope = ScopeName::new("default").unwrap();
        let time = Time::parse("2026-10-13T15:04:05.123Z").unwrap();
        let key = deployed_key(&scope, None, time, ULID);
        assert_eq!(
            key,
            format!(
                "v1/deployed/default/_/{:013}.{ULID}.json",
                INVERTED_MS_BASE - time.millis()
            )
        );
        assert!(key.starts_with(&deployed_prefix(&scope, None)));
        let parsed = parse_deployed_key(&key).unwrap();
        assert_eq!(parsed.scope, scope);
        assert_eq!(parsed.stage, None);
        assert_eq!(parsed.millis, time.millis());
        assert_eq!(parsed.ulid, ULID);
        // 0.8's keys, with a stage, read as before beside it.
        let staged = deployed_key(
            &scope,
            Some(&StageName::new("production").unwrap()),
            time,
            ULID,
        );
        assert_eq!(
            parse_deployed_key(&staged).unwrap().stage.unwrap().as_str(),
            "production"
        );
        // Only `_` stands for no stage: an empty or invalid segment is no key.
        for bad in ["", "__", "Prod", "-"] {
            let key = format!("v1/deployed/default/{bad}/{:013}.{ULID}.json", 1);
            assert!(parse_deployed_key(&key).is_none(), "{key}");
        }
    }
}
