//! B-36: what `hushrun build fetch` saves for a sandbox, and the relay of a later run that reads
//! only it. The folder's own lifecycle (written whole, fresh for a day, swept) is
//! `hushrun_build::prefetched`; this is what is in `saved.json`, and how a relay takes it in.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

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

/// How long a fetch waits for its downloads at most.
const FETCH_FOR: Duration = Duration::from_secs(600);

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

impl Relay {
    /// B-36, `hushrun build fetch`: `open`, then the prefetch of its manifest to the end, then
    /// the sealed packs, hints, policy and rows saved whole into the store's `prefetched/`. No
    /// `close`, no upload. The error is the line to say.
    pub(crate) fn fetch_for_later(self: &Arc<Relay>) -> Result<Fetched, String> {
        let relay = self.clone();
        self.runtime.block_on(async move {
            relay.clone().open(None).await;
            if let Phase::Stopped(stop) = relay.state().phase.clone() {
                return Err(match stop {
                    Stop::Off(why) | Stop::Offline(why) | Stop::Refused(why) => why,
                });
            }
            relay.clone().prefetch_at_first_miss().await;
            let deadline = Instant::now() + FETCH_FOR;
            loop {
                let busy = relay
                    .state()
                    .fetches
                    .values()
                    .any(|f| matches!(f.state, Download::Queued | Download::Downloading));
                if !busy || Instant::now() >= deadline {
                    break;
                }
                relay.pump();
                tokio::time::sleep(relay.config.tuning.tick).await;
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
        let _ = std::fs::remove_dir_all(outbox::run_folder(
            &self.config.store,
            outbox::INCOMING,
            &self.run,
        ));
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
