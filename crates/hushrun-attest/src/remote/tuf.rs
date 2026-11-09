//! `hushrun sigstore refresh` (T-12): a TUF update of a Sigstore trust root. It starts from a
//! root the binary or the environment provides, walks the root versions the repository
//! publishes, and checks the timestamp, snapshot and targets roles, their versions and expiry,
//! and the trust root's length and hashes. The caller writes what it returns to the cache,
//! where readers verify it again, offline.

use hushrun_core::time::Time;

use super::{Http, checked_url, join};
use crate::signer::SignError;
use crate::trust_root::TrustedRoot;
use crate::tuf::{MAX_ROOT_ROTATIONS, Root, TRUSTED_ROOT_TARGET};

/// What an update found, verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refreshed {
    /// Every root version after the initial one, in order: the cached ones and the new ones.
    pub roots: Vec<Vec<u8>>,
    pub root_version: u64,
    /// The `targets.json` that pins the trust root.
    pub targets: Vec<u8>,
    pub targets_version: u64,
    pub trusted_root: Vec<u8>,
}

fn fail(what: &str, reason: impl std::fmt::Display) -> SignError {
    SignError::Unavailable(format!("the TUF repository: {what}: {reason}"))
}

/// Updates from `mirror`, starting at `initial` and the root versions cached after it. A
/// `targets.json` older than `cached_targets` is a rollback, and refused.
pub fn refresh(
    mirror: &str,
    initial: &[u8],
    cached_roots: &[Vec<u8>],
    cached_targets: Option<u64>,
    now: Time,
) -> Result<Refreshed, SignError> {
    checked_url("HUSHRUN_SIGSTORE_TUF_URL", mirror)?;
    let http = Http::new()?;
    let fetch = |path: &str| -> Result<Option<Vec<u8>>, SignError> {
        let url = join(mirror, path);
        let answer = http.send_retrying(&format!("the TUF repository ({path})"), |client| {
            client.get(&url)
        })?;
        match answer.status {
            200 => Ok(Some(answer.body)),
            // A missing next root version answers 404, or 403 from some object stores.
            403 | 404 => Ok(None),
            status => Err(fail(path, format!("HTTP {status}: {}", answer.excerpt()))),
        }
    };
    let mut root = Root::parse(initial)
        .map_err(|e| SignError::Config(format!("the initial TUF root: {e}")))?;
    let mut roots = Vec::new();
    for bytes in cached_roots {
        root = root
            .verify_next(bytes)
            .map_err(|e| fail("a cached root", e))?;
        roots.push(bytes.clone());
    }
    for _ in 0..MAX_ROOT_ROTATIONS {
        let path = format!("{}.root.json", root.version + 1);
        let Some(bytes) = fetch(&path)? else {
            break;
        };
        root = root.verify_next(&bytes).map_err(|e| fail(&path, e))?;
        roots.push(bytes);
    }
    let fresh = |what: &str, expires: Time| {
        if expires <= now {
            Err(fail(
                what,
                format!("expired at {expires}, so the repository may be frozen or replayed"),
            ))
        } else {
            Ok(())
        }
    };
    fresh(&format!("root version {}", root.version), root.expires)?;

    let bytes = fetch("timestamp.json")?.ok_or_else(|| fail("timestamp.json", "missing"))?;
    let timestamp = root
        .verify_role("timestamp", &bytes)
        .map_err(|e| fail("timestamp.json", e))?;
    fresh("timestamp.json", timestamp.expires)?;
    let snapshot_version = timestamp
        .meta_version("snapshot.json")
        .map_err(|e| fail("timestamp.json", e))?;

    let path = if root.consistent_snapshot {
        format!("{snapshot_version}.snapshot.json")
    } else {
        "snapshot.json".to_string()
    };
    let bytes = fetch(&path)?.ok_or_else(|| fail(&path, "missing"))?;
    if let Some(meta) = timestamp.meta_file("snapshot.json") {
        meta.check(&bytes).map_err(|e| fail(&path, e))?;
    }
    let snapshot = root
        .verify_role("snapshot", &bytes)
        .map_err(|e| fail(&path, e))?;
    if snapshot.version != snapshot_version {
        return Err(fail(
            &path,
            format!(
                "version {}, and timestamp.json names {snapshot_version}",
                snapshot.version
            ),
        ));
    }
    fresh(&path, snapshot.expires)?;
    let targets_version = snapshot
        .meta_version("targets.json")
        .map_err(|e| fail(&path, e))?;

    let path = if root.consistent_snapshot {
        format!("{targets_version}.targets.json")
    } else {
        "targets.json".to_string()
    };
    let targets_bytes = fetch(&path)?.ok_or_else(|| fail(&path, "missing"))?;
    if let Some(meta) = snapshot.meta_file("targets.json") {
        meta.check(&targets_bytes).map_err(|e| fail(&path, e))?;
    }
    let targets = root
        .verify_role("targets", &targets_bytes)
        .map_err(|e| fail(&path, e))?;
    if targets.version != targets_version {
        return Err(fail(
            &path,
            format!(
                "version {}, and the snapshot names {targets_version}",
                targets.version
            ),
        ));
    }
    fresh(&path, targets.expires)?;
    if let Some(cached) = cached_targets
        && targets.version < cached
    {
        return Err(fail(
            &path,
            format!(
                "version {} is older than the cached version {cached}",
                targets.version
            ),
        ));
    }

    let target = targets
        .target(TRUSTED_ROOT_TARGET)
        .map_err(|e| fail(&path, e))?;
    let target_path = match (root.consistent_snapshot, target.sha256()) {
        (true, Some(sha256)) => format!("targets/{sha256}.{TRUSTED_ROOT_TARGET}"),
        _ => format!("targets/{TRUSTED_ROOT_TARGET}"),
    };
    let trusted_root = fetch(&target_path)?.ok_or_else(|| fail(&target_path, "missing"))?;
    target
        .check(&trusted_root)
        .map_err(|e| fail(&target_path, e))?;
    TrustedRoot::parse(&trusted_root, TRUSTED_ROOT_TARGET).map_err(|e| fail(&target_path, e))?;
    Ok(Refreshed {
        roots,
        root_version: root.version,
        targets: targets_bytes,
        targets_version: targets.version,
        trusted_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Pki, TestKey, TufRepo};
    use hushrun_testkit::http::{Reply, Response, Server};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    fn mirror(files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>) -> Server {
        Server::start(move |request| {
            let path = request.path.trim_start_matches('/').to_string();
            match files.lock().unwrap().get(&path) {
                Some(bytes) => Reply::Send(Response::bytes(200, bytes.clone())),
                None => Reply::Send(Response::new(404)),
            }
        })
    }

    fn time(text: &str) -> Time {
        Time::parse(text).unwrap()
    }

    // covers: T-12
    #[test]
    fn refreshes_through_root_rotations() {
        let trusted_root = Pki::new().trusted_root();
        let mut repo = TufRepo::new(&trusted_root, time("2099-01-01T00:00:00Z"));
        let initial = repo.root(1);
        repo.add_root(Some(vec![TestKey::generate(), TestKey::generate()]));
        repo.publish(&trusted_root, 2);
        let files = Arc::new(Mutex::new(repo.files.clone()));
        let server = mirror(files.clone());
        let now = time("2026-10-15T00:00:00Z");
        let refreshed = refresh(&server.url(), &initial, &[], None, now).unwrap();
        assert_eq!(refreshed.root_version, 2);
        assert_eq!(refreshed.roots.len(), 1);
        assert_eq!(refreshed.targets_version, 2);
        assert_eq!(refreshed.trusted_root, trusted_root);
        // Offline, readers verify the same files.
        crate::tuf::verify_cached(
            &initial,
            &refreshed.roots,
            &refreshed.targets,
            &trusted_root,
        )
        .unwrap();
        // With the cached roots, the update starts after them; an older targets is refused.
        let again = refresh(&server.url(), &initial, &refreshed.roots, Some(2), now).unwrap();
        assert_eq!(again.roots, refreshed.roots);
        let err = refresh(&server.url(), &initial, &[], Some(3), now).unwrap_err();
        assert!(
            err.to_string().contains("older than the cached version 3"),
            "{err}"
        );
    }

    // covers: T-12
    #[test]
    fn refuses_expired_or_tampered_metadata() {
        let trusted_root = Pki::new().trusted_root();
        let repo = TufRepo::new(&trusted_root, time("2026-01-01T00:00:00Z"));
        let initial = repo.root(1);
        let files = Arc::new(Mutex::new(repo.files.clone()));
        let server = mirror(files.clone());
        let err = refresh(
            &server.url(),
            &initial,
            &[],
            None,
            time("2026-10-15T00:00:00Z"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
        let before = time("2025-10-15T00:00:00Z");
        refresh(&server.url(), &initial, &[], None, before).unwrap();
        // A trust root that isn't the one the targets pin.
        let sha = files
            .lock()
            .unwrap()
            .keys()
            .find(|k| k.starts_with("targets/"))
            .cloned()
            .unwrap();
        files.lock().unwrap().insert(sha, b"{}".to_vec());
        let err = refresh(&server.url(), &initial, &[], None, before).unwrap_err();
        assert!(err.to_string().contains("trusted_root.json"), "{err}");
        // Another repository's root doesn't sign this one's roles.
        let other = TufRepo::new(&trusted_root, time("2099-01-01T00:00:00Z"));
        assert!(refresh(&server.url(), &other.root(1), &[], None, before).is_err());
    }
}
