use super::*;
use crate::testing::{TestKey, TufRepo};
use crate::trust_root::PUBLIC_GOOD;

fn at(text: &str) -> Time {
    Time::parse(text).unwrap()
}

// covers: T-12
#[test]
fn the_vendored_trust_root_is_the_one_sigstore_published() {
    // Root version 15's targets keys signed targets.json version 14, which pins the vendored
    // trusted_root.json.
    let verified = verify_cached(SIGSTORE_ROOT, &[], SIGSTORE_TARGETS, PUBLIC_GOOD).unwrap();
    assert_eq!(verified.root_version, 15);
    assert_eq!(verified.targets_version, 14);
    let root = Root::parse(SIGSTORE_ROOT).unwrap();
    assert!(root.consistent_snapshot);
    // Any other trust root is refused.
    let mut changed = PUBLIC_GOOD.to_vec();
    changed[10] ^= 1;
    assert!(verify_cached(SIGSTORE_ROOT, &[], SIGSTORE_TARGETS, &changed).is_err());
}

// covers: T-12
#[test]
fn canonical_json_is_securesystemslibs() {
    let value = serde_json::json!({"b": [1, true, null], "a": "x\"y\\z\nw", "é": {"d": 2, "c": 1}});
    assert_eq!(
        String::from_utf8(canonical_json(&value)).unwrap(),
        "{\"a\":\"x\\\"y\\\\z\nw\",\"b\":[1,true,null],\"é\":{\"c\":1,\"d\":2}}"
    );
}

// covers: T-12
#[test]
fn roots_rotate_with_both_thresholds() {
    let expires = at("2099-01-01T00:00:00Z");
    let mut repo = TufRepo::new(b"{}", expires);
    repo.add_root(Some(vec![TestKey::generate(), TestKey::generate()]));
    repo.add_root(None);
    let first = Root::parse(&repo.root(1)).unwrap();
    let second = first.verify_next(&repo.root(2)).unwrap();
    let third = second.verify_next(&repo.root(3)).unwrap();
    assert_eq!(third.version, 3);
    // Skipping a version, or a version the old keys didn't sign, is refused.
    assert!(
        first
            .verify_next(&repo.root(3))
            .unwrap_err()
            .contains("expected 2")
    );
    let mut forked = TufRepo::new(b"{}", expires);
    forked.add_root(None);
    let err = first.verify_next(&forked.root(2)).unwrap_err();
    assert!(err.contains("root keys signed it"), "{err}");
}

// covers: T-12
#[test]
fn roles_and_targets_verify_with_the_root_keys() {
    let trusted_root =
        br#"{"mediaType":"application/vnd.dev.sigstore.trustedroot+json;version=0.1"}"#;
    let repo = TufRepo::new(trusted_root, at("2099-01-01T00:00:00Z"));
    let root = Root::parse(&repo.root(1)).unwrap();
    let timestamp = root
        .verify_role("timestamp", &repo.files["timestamp.json"])
        .unwrap();
    assert_eq!(timestamp.meta_version("snapshot.json").unwrap(), 1);
    let snapshot_bytes = &repo.files["1.snapshot.json"];
    timestamp
        .meta_file("snapshot.json")
        .unwrap()
        .check(snapshot_bytes)
        .unwrap();
    let snapshot = root.verify_role("snapshot", snapshot_bytes).unwrap();
    assert_eq!(snapshot.meta_version("targets.json").unwrap(), 1);
    let targets = root
        .verify_role("targets", &repo.files["1.targets.json"])
        .unwrap();
    let target = targets.target(TRUSTED_ROOT_TARGET).unwrap();
    target.check(trusted_root).unwrap();
    assert!(target.check(b"{}").is_err());
    let path = format!("targets/{}.trusted_root.json", target.sha256().unwrap());
    assert_eq!(repo.files[&path], trusted_root.to_vec());
    // A role signed by the wrong keys, or of another type, is refused.
    let err = root
        .verify_role("targets", &repo.files["timestamp.json"])
        .unwrap_err();
    assert!(err.contains("\"timestamp\""), "{err}");
    let other = TufRepo::new(trusted_root, at("2099-01-01T00:00:00Z"));
    assert!(
        root.verify_role("targets", &other.files["1.targets.json"])
            .is_err()
    );
    verify_cached(
        &repo.root(1),
        &[],
        &repo.files["1.targets.json"],
        trusted_root,
    )
    .unwrap();
}
