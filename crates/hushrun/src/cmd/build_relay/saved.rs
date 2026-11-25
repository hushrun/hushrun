//! B-36: what `hushrun build fetch` saves for a sandbox, and the relay of a later run that reads
//! only it. The folder's own lifecycle (written whole, fresh for a day, swept) is
//! `hushrun_build::prefetched`; this is what is in `saved.json`, and how a relay takes it in.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Instant, SystemTime};

use hushrun_build::model::Mode;
use hushrun_build::outbox;
use hushrun_build::prefetched;
use hushrun_build::share::{Org, what_a_run_does};
use hushrun_build::store::Store;
use hushrun_core::blocks::{self, Base64, Found, FoundHints, Hex32, Policy};
use serde::{Deserialize, Serialize};

use super::{Download, Fetch, Phase, Relay, Stop};

/// `saved.json`'s schema.
pub(crate) const SCHEMA: &str = "hushrun.build-prefetched/v1";

/// `saved.json`: no token, and no link (a row's `get` is dropped).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Saved {
    pub(crate) schema: String,
    /// When it was fetched, in milliseconds since the epoch: [`prefetched::is_fresh`] reads it.
    pub(crate) fetched_ms: u64,
    /// Who it was fetched from: a downloaded block's record names them.
    pub(crate) host: String,
    pub(crate) org: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) repository: Option<String>,
    pub(crate) policy: Policy,
    /// The rows of the blocks whose packs are in `packs/<address>.pack`.
    pub(crate) found: Vec<Found>,
    /// Sealed hints, by first-key address.
    pub(crate) hints: Vec<FoundHints>,
}

impl Saved {
    /// The store's fetch, when it is fresh at `now` and reads as one.
    pub(crate) fn load(store: &Store, now: SystemTime) -> Option<(Saved, PathBuf)> {
        let folder = prefetched::fresh(store, now)?;
        let bytes = std::fs::read(folder.join(prefetched::SAVED)).ok()?;
        let saved: Saved = serde_json::from_slice(&bytes).ok()?;
        (saved.schema == SCHEMA).then_some((saved, folder))
    }

    /// B-36: whether this fetch may serve a run whose backend is `place` and whose repository is
    /// `repository`: only one of the same host, org and repository, and only while that backend
    /// answered nothing in the run. An answer always wins over a fetch: that the org doesn't
    /// share, a 401 or a 403 included. A run that names no backend, or no repository, is not the
    /// fetch's.
    pub(crate) fn serves(
        &self,
        place: Option<&crate::results::BuildPlace>,
        repository: Option<&str>,
    ) -> bool {
        place.is_some_and(|place| {
            !place.answered && place.host == self.host && place.org == self.org
        }) && self.repository.is_some()
            && self.repository.as_deref() == repository
    }

    /// The org's mode, as the policy said it.
    pub(crate) fn org(&self) -> Org {
        if !self.policy.sharing {
            return Org::NotSharing;
        }
        Org::Shares(Some(match self.policy.mode {
            Some(blocks::Mode::On) => Mode::On,
            _ => Mode::Observe,
        }))
    }
}

fn pack_path(folder: &Path, address: &Hex32) -> PathBuf {
    folder
        .join(prefetched::PACKS)
        .join(format!("{address}.pack"))
}

/// What a fetch left.
pub(crate) struct Fetched {
    pub(crate) blocks: usize,
    pub(crate) bytes: u64,
    pub(crate) hints: usize,
}

/// What a fetch downloaded, before it is saved anywhere: the policy `open` answered (its
/// manifest among it), the sealed packs that arrived in the run's `incoming/` with their rows, and
/// the sealed hints.
pub(crate) struct Pieces {
    pub(crate) policy: Policy,
    pub(crate) arrived: Vec<(Found, PathBuf)>,
    pub(crate) hints: Vec<FoundHints>,
}

/// What a bake's fetch downloaded: each check's manifest of each kind for its platform, the
/// sealed packs that arrived with their kinds and rows, the sealed hints, and the repository's
/// trusted scope as `open` named it (the `protected/…` scope among those the bake reads).
pub(crate) struct Parts {
    pub(crate) manifests: Vec<(
        Option<Hex32>,
        hushrun_build::model::Kind,
        String,
        Vec<blocks::Pair>,
    )>,
    pub(crate) arrived: Vec<(hushrun_build::model::Kind, Found, PathBuf)>,
    pub(crate) hints: Vec<FoundHints>,
    pub(crate) trusted_scope: Option<String>,
}

impl Relay {
    /// B-36, B-63: for each of `checks` (`None`: no check, the repository's newest manifest),
    /// `open` naming it, then the prefetch of its manifest to the end. No `close`, no upload.
    /// The packs stay in the run's `incoming/` until [`Relay::drop_incoming`]. The error is the
    /// line to say.
    pub(crate) fn fetch_pieces(
        self: &Arc<Relay>,
        checks: &[Option<Hex32>],
    ) -> Result<Pieces, String> {
        let relay = self.clone();
        let checks = checks.to_vec();
        self.runtime.block_on(async move {
            for check in checks {
                relay.clone().open(check).await;
                if let Phase::Stopped(stop) = relay.state().phase.clone() {
                    return Err(match stop {
                        Stop::Off(why) | Stop::Offline(why) | Stop::Refused(why) => why,
                    });
                }
                relay.clone().prefetch_at_first_miss().await;
                loop {
                    let busy = relay
                        .state()
                        .fetches
                        .values()
                        .any(|f| matches!(f.state, Download::Queued | Download::Downloading));
                    // ADR 0144: every download to its end, or its server's silence.
                    if !busy {
                        break;
                    }
                    relay.pump();
                    tokio::time::sleep(relay.config.tuning.tick).await;
                }
            }
            Ok(())
        })?;
        let mut state = self.state();
        let policy = state.policy.clone().ok_or("the backend gave no policy")?;
        let arrived: Vec<(Found, PathBuf)> = state
            .fetches
            .values()
            .filter_map(|f| match &f.state {
                Download::Arrived(path) => Some((f.found.clone(), path.clone())),
                _ => None,
            })
            .collect();
        let hints: Vec<FoundHints> = state
            .sealed_hints
            .iter()
            .map(|(address, lists)| FoundHints {
                address: *address,
                lists: lists.iter().map(|l| Base64(l.clone())).collect(),
            })
            .collect();
        state.fetches.clear();
        drop(state);
        Ok(Pieces {
            policy,
            arrived,
            hints,
        })
    }

    /// B-63, `hushrun build bake --from backend`: for each of `checks`, the check's `open` and
    /// the prefetch of its `rustc/1` manifest, then for each other kind and platform of
    /// `wanted`, an `open` for its manifest alone and the prefetch of it; then every download to
    /// its end, within the fetch's wait. No `close`, no upload. The packs stay in the run's
    /// `incoming/` until [`Relay::drop_incoming`]. The error is the line to say.
    pub(crate) fn fetch_parts(
        self: &Arc<Relay>,
        checks: &[Option<Hex32>],
        wanted: &[(hushrun_build::model::Kind, String)],
    ) -> Result<Parts, String> {
        use hushrun_build::model::Kind;
        // B-63: a bake downloads every block the manifests name, programs too.
        self.whole.store(true, std::sync::atomic::Ordering::SeqCst);
        let relay = self.clone();
        let wanted = wanted.to_vec();
        let checks = checks.to_vec();
        let manifests = self.runtime.block_on(async move {
            let mut manifests = Vec::new();
            for check in checks {
                relay.clone().open(check).await;
                if let Phase::Stopped(stop) = relay.state().phase.clone() {
                    return Err(match stop {
                        Stop::Off(why) | Stop::Offline(why) | Stop::Refused(why) => why,
                    });
                }
                let rustc = relay
                    .state()
                    .policy
                    .as_ref()
                    .map(|p| p.manifest.clone())
                    .unwrap_or_default();
                let platform = relay.platform().await;
                manifests.push((check, Kind::Rustc, platform, rustc));
                relay.clone().prefetch_at_first_miss().await;
                for (kind, platform) in wanted.iter().filter(|(k, _)| *k != Kind::Rustc) {
                    let body = blocks::OpenRequest {
                        kind: kind.word().to_string(),
                        repository: relay.config.repository.clone(),
                        commit: relay.config.commit.clone(),
                        check,
                        platform: platform.clone(),
                    };
                    let manifest = match relay
                        .client
                        .post::<_, Policy>(blocks::OPEN_PATH, &body, relay.config.tuning.open, None)
                        .await
                    {
                        Ok(answered) => answered.body.manifest,
                        Err(_) => continue,
                    };
                    relay.clone().prefetch_in(*kind, manifest.clone()).await;
                    manifests.push((check, *kind, platform.clone(), manifest));
                }
            }
            loop {
                let busy = relay
                    .state()
                    .fetches
                    .values()
                    .any(|f| matches!(f.state, Download::Queued | Download::Downloading));
                // ADR 0144: every download to its end, or its server's silence.
                if !busy {
                    break;
                }
                relay.pump();
                tokio::time::sleep(relay.config.tuning.tick).await;
            }
            Ok(manifests)
        })?;
        let mut state = self.state();
        let arrived = state
            .fetches
            .values()
            .filter_map(|f| match &f.state {
                Download::Arrived(path) => Some((f.kind, f.found.clone(), path.clone())),
                _ => None,
            })
            .collect();
        let hints = state
            .sealed_hints
            .iter()
            .map(|(address, lists)| FoundHints {
                address: *address,
                lists: lists.iter().map(|l| Base64(l.clone())).collect(),
            })
            .collect();
        let trusted_scope = state.policy.as_ref().and_then(|policy| {
            policy
                .read
                .iter()
                .find(|scope| blocks::is_trusted_scope(scope))
                .cloned()
        });
        state.fetches.clear();
        drop(state);
        Ok(Parts {
            manifests,
            arrived,
            trusted_scope,
            hints,
        })
    }

    /// B-75: for each check of `checks`, the newest `rustc/1` manifest `open` answers and the
    /// commit its run named, downloading nothing. `Err` when the backend stops the relay.
    pub(crate) fn newest_manifests(
        self: &Arc<Relay>,
        checks: &[Option<Hex32>],
    ) -> Result<Vec<Newest>, String> {
        let relay = self.clone();
        let checks = checks.to_vec();
        self.runtime.block_on(async move {
            let mut out = Vec::new();
            for check in checks {
                relay.clone().open(check).await;
                let state = relay.state();
                if let Phase::Stopped(stop) = state.phase.clone() {
                    return Err(match stop {
                        Stop::Off(why) | Stop::Offline(why) | Stop::Refused(why) => why,
                    });
                }
                let (pairs, commit) = state
                    .policy
                    .as_ref()
                    .map(|p| (p.manifest.clone(), p.manifest_commit.clone()))
                    .unwrap_or_default();
                out.push((check, pairs, commit));
            }
            Ok(out)
        })
    }

    /// Removes this relay's `incoming/` folder, with any pack a fetch left there.
    pub(crate) fn drop_incoming(&self) {
        let _ = std::fs::remove_dir_all(outbox::run_folder(
            &self.config.store,
            outbox::INCOMING,
            &self.run,
        ));
    }

    /// B-36, `hushrun build fetch`: [`Relay::fetch_pieces`], then the sealed packs, hints,
    /// policy and rows saved whole into the store's `prefetched/`. The error is the line to say.
    pub(crate) fn fetch_for_later(
        self: &Arc<Relay>,
        checks: &[Option<Hex32>],
    ) -> Result<Fetched, String> {
        let pieces = self.fetch_pieces(checks);
        let Pieces {
            policy,
            arrived,
            hints,
            ..
        } = match pieces {
            Ok(pieces) => pieces,
            Err(why) => {
                self.drop_incoming();
                return Err(why);
            }
        };
        let fetched_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let saved = Saved {
            schema: SCHEMA.to_string(),
            fetched_ms,
            host: self.by.host.clone(),
            org: self.by.org.clone(),
            repository: self.config.repository.clone(),
            policy,
            found: arrived
                .iter()
                .map(|(found, _)| Found {
                    get: None,
                    ..found.clone()
                })
                .collect(),
            hints,
        };
        let report = Fetched {
            blocks: arrived.len(),
            bytes: arrived.iter().map(|(f, _)| f.bytes).sum(),
            hints: saved.hints.len(),
        };
        let written = prefetched::replace(&self.config.store, |folder| {
            for (found, path) in &arrived {
                std::fs::rename(path, pack_path(folder, &found.address))?;
            }
            let bytes = serde_json::to_vec(&saved).map_err(std::io::Error::other)?;
            std::fs::write(folder.join(prefetched::SAVED), bytes)
        });
        self.drop_incoming();
        written.map_err(|err| format!("{}: {err}", self.config.store.root().display()))?;
        Ok(report)
    }

    /// B-36: a check of a run that reads only a fetch: no `open`, the saved policy, every saved
    /// pack linked into this run's `incoming/` to be opened when a wrapper brings its key, and
    /// the saved hints. Nothing is uploaded, reported or closed: the client sends nothing.
    pub(crate) fn take_in_saved(self: &Arc<Relay>, saved: &Saved, folder: &Path) {
        let incoming = outbox::run_folder(&self.config.store, outbox::INCOMING, &self.run);
        let _ = std::fs::create_dir_all(&incoming);
        let does = what_a_run_does(
            self.config.caller,
            self.config.local,
            self.config.own,
            saved.org(),
        );
        let mut state = self.state();
        state.sharing = does.sharing;
        state.uploads = false;
        state.policy = Some(saved.policy.clone());
        state.manifest = None;
        let now = Instant::now();
        for found in &saved.found {
            let link = incoming.join(format!("{}.pack", found.address));
            let from = pack_path(folder, &found.address);
            if std::fs::hard_link(&from, &link).is_err() && std::fs::copy(&from, &link).is_err() {
                continue;
            }
            state.found.insert(found.address, (found.clone(), now));
            state.fetches.insert(
                found.address,
                Fetch {
                    kind: hushrun_build::model::Kind::Rustc,
                    found: found.clone(),
                    looked_up: now,
                    state: Download::Arrived(link),
                    bytes: found.bytes,
                    started: None,
                    cancel: Arc::new(AtomicBool::new(false)),
                    waited: false,
                },
            );
        }
        let mut asked = HashSet::new();
        for hints in &saved.hints {
            asked.insert(hints.address);
            state.sealed_hints.insert(
                hints.address,
                hints.lists.iter().map(|l| l.0.clone()).collect(),
            );
        }
        state.hints_asked.extend(asked);
        state.phase = match does.sharing {
            hushrun_build::share::Sharing::Off(off) => {
                Phase::Stopped(Stop::Off(off.reason().to_string()))
            }
            _ => Phase::Answering,
        };
    }
}

/// B-75: a check's newest manifest as `open` answers it, and the commit its run named.
pub(crate) type Newest = (Option<Hex32>, Vec<blocks::Pair>, Option<String>);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::BuildPlace;

    fn saved(repository: Option<&str>) -> Saved {
        Saved {
            schema: SCHEMA.into(),
            fetched_ms: 0,
            host: "https://api.example.com".into(),
            org: "acme".into(),
            repository: repository.map(str::to_string),
            policy: serde_json::from_value(serde_json::json!({
                "sharing": true, "keptDays": 30, "allowanceBytes": 0, "heldBytes": 0
            }))
            .unwrap(),
            found: Vec::new(),
            hints: Vec::new(),
        }
    }

    fn place(host: &str, org: &str, answered: bool) -> BuildPlace {
        BuildPlace {
            host: host.into(),
            org: org.into(),
            answered,
        }
    }

    // covers: B-36
    #[test]
    fn a_fetch_serves_only_its_host_org_and_repository_while_the_backend_answered_nothing() {
        let shop = Some("github.com/acme/shop");
        let fetch = saved(shop);
        let here = place("https://api.example.com", "acme", false);
        assert!(fetch.serves(Some(&here), shop));
        assert!(!fetch.serves(Some(&here), Some("github.com/acme/other")));
        assert!(!fetch.serves(Some(&here), None));
        assert!(!fetch.serves(Some(&place("https://api.example.com", "beta", false)), shop));
        assert!(!fetch.serves(
            Some(&place("https://other.example.com", "acme", false)),
            shop
        ));
        // Any answer from the backend wins: sharing off, 401, 403.
        assert!(!fetch.serves(Some(&place("https://api.example.com", "acme", true)), shop));
        // A run that names no backend, or a fetch that names no repository, is no match.
        assert!(!fetch.serves(None, shop));
        assert!(!saved(None).serves(Some(&here), None));
    }
}
