//! B-29: uploads. The outbox's markers move only here (and in the store's sweep), each move by
//! the table below, decided by [`after_begin`], [`after_put`] and [`after_commit`]:
//!
//! | From | To | When |
//! |---|---|---|
//! | waiting | claimed | a relay claims it: a rename, only one wins |
//! | claimed | waiting | the run that claimed it is dead (a relay's start, the sweep) |
//! | claimed | sent | packed, `begin` answered `put`, the PUT succeeded or found it there |
//! | sent | done | `commit` answered `stored` |
//! | claimed | done | `begin` answered `have`, or refused `too_large`, `unsupported`, `no_scope`, `suspended`; or the block is gone here, or can't be packed as recorded |
//! | claimed, sent | done, the local block discarded | `begin` or `commit` answered `differs` |
//! | claimed, sent | waiting, with a date | a network error, a 5xx, 401, 403, 404, an expired link, `busy`, `over_allowance`, `too_many_uploads`, `commit` answering `missing` or `gone`, any word this version doesn't know |
//! | claimed, sent | waiting, one more cut-off | the command ended while its PUT was in flight |
//! | waiting | done | three cut-offs |
//!
//! `begin` goes a few blocks at a time, just before their PUTs: links live 15 minutes. PUTs run
//! 4 at a time, 2 while downloads are in flight.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use hushrun_build::outbox::{Back, Marker, Outbox};
use hushrun_core::blocks::{
    self, Answer, BeginBlock, BeginRequest, BeginResponse, Class, CommitRequest, CommitResponse,
    CommitState, Hex32, Refusal,
};

use super::http::Failure;
use super::pack::{NotPacked, Packed, pack_block_within};
use super::{Phase, Relay, Stop, Tuning};

/// How long a marker waits before it is tried again after a failure that may pass.
const LATER: Duration = Duration::from_secs(10 * 60);
/// PUTs at once, and while downloads are in flight.
const PUTS: usize = 4;
const PUTS_WHILE_DOWNLOADING: usize = 2;

/// What happens to a marker after an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Move {
    /// Go on to the next request.
    Next,
    Done,
    /// Done, and the local block is discarded: a trusted copy exists with other bytes.
    DoneDiscard,
    /// Waiting again, tried after a while.
    Later,
}

/// `begin`'s answer for one block.
pub(crate) fn after_begin(answer: &Answer) -> Move {
    match answer {
        Answer::Put { .. } => Move::Next,
        Answer::Have { .. } => Move::Done,
        Answer::Refused { reason } => match reason {
            Refusal::TooLarge | Refusal::Unsupported | Refusal::NoScope | Refusal::Suspended => {
                Move::Done
            }
            Refusal::Differs => Move::DoneDiscard,
            Refusal::Busy | Refusal::OverAllowance | Refusal::TooManyUploads | Refusal::Unknown => {
                Move::Later
            }
        },
        Answer::Unknown => Move::Later,
    }
}

/// The PUT's status: 2xx, or 412 "already there" (the same pack, sent before).
pub(crate) fn after_put(status: Option<u16>) -> Move {
    match status {
        Some(200..=299 | 412) => Move::Next,
        _ => Move::Later,
    }
}

/// `commit`'s answer for one block.
pub(crate) fn after_commit(state: CommitState) -> Move {
    match state {
        CommitState::Stored => Move::Done,
        CommitState::Differs => Move::DoneDiscard,
        CommitState::Missing | CommitState::Gone | CommitState::Unknown => Move::Later,
    }
}

/// How long uploads in flight get when a command ends (B-29): on a person's machine the
/// tuning's 5 seconds. CI's waits are another phase's, and go here.
pub(crate) fn wait_at_end(tuning: &Tuning, in_ci: bool) -> Duration {
    let _ = in_ci;
    tuning.end_wait
}

fn class_of(class: hushrun_build::model::Class) -> Class {
    match class {
        hushrun_build::model::Class::Library => Class::Library,
        hushrun_build::model::Class::ProcMacro => Class::ProcMacro,
        hushrun_build::model::Class::BuildScript => Class::BuildScript,
        hushrun_build::model::Class::Program => Class::Program,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Relay {
    /// The check's upload loop: while the run shares and the policy says it writes, claims a
    /// few waiting markers of this run's host, org and credential, packs them, and sends them.
    pub(crate) async fn uploads(self: Arc<Relay>) {
        {
            let mut state = self.state();
            if state.uploading {
                return;
            }
            state.uploading = true;
        }
        self.clone().upload_loop().await;
        self.state().uploading = false;
    }

    async fn upload_loop(self: Arc<Relay>) {
        let outbox = Outbox::new(&self.config.store);
        loop {
            if self.ending.load(Ordering::Relaxed) {
                return;
            }
            let (go, running) = {
                let mut state = self.state();
                let phase = state.phase_now(Instant::now()).clone();
                let writes =
                    self.config.access.uploads && state.policy.as_ref().is_some_and(|p| p.writes);
                let running = state
                    .fetches
                    .values()
                    .filter(|f| f.state == super::Download::Downloading)
                    .count();
                match phase {
                    Phase::Stopped(_) => return,
                    Phase::Answering if writes => (true, running),
                    Phase::Answering if state.policy.is_some() => return,
                    _ => (false, running),
                }
            };
            if !go {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            let now = now_ms();
            let ready: Vec<Marker> = outbox
                .waiting()
                .into_iter()
                .filter(|m| m.by == self.by && m.not_before_ms <= now)
                .collect();
            if ready.is_empty() {
                let _ =
                    tokio::time::timeout(Duration::from_millis(200), self.wake.notified()).await;
                continue;
            }
            let most = if running > 0 {
                PUTS_WHILE_DOWNLOADING
            } else {
                PUTS
            };
            let mut claimed = Vec::new();
            for marker in ready {
                if claimed.len() >= most {
                    break;
                }
                if let Ok(Some(marker)) = outbox.claim(&self.run, &marker.key) {
                    claimed.push(marker);
                }
            }
            if claimed.is_empty() {
                continue;
            }
            self.clone().send(&outbox, claimed).await;
        }
    }

    /// Packs, begins, puts and commits a few claimed markers, and moves each by the table.
    async fn send(self: Arc<Relay>, outbox: &Outbox, claimed: Vec<Marker>) {
        let run = self.run.clone();
        let release = |marker: &Marker, back: Back| {
            let _ = outbox.release(&run, marker, back);
        };
        let finish = |marker: &Marker, step: Move| match step {
            Move::Done => {
                let _ = outbox.done(&run, &marker.key);
            }
            Move::DoneDiscard => {
                let _ = outbox.done(&run, &marker.key);
                let _ = self.config.store.discard(&marker.key);
            }
            Move::Later => release(marker, Back::Later(now_ms() + LATER.as_millis() as u64)),
            Move::Next => {}
        };
        // Packed, each checked against the store's record as it is read.
        let mut packed: Vec<(Marker, Packed)> = Vec::new();
        for marker in claimed {
            let store = self.config.store.clone();
            let dir = hushrun_build::outbox::run_folder(
                &self.config.store,
                hushrun_build::outbox::PACKS,
                &self.run,
            );
            let floor = u64::from(self.config.min_free_gb).saturating_mul(1 << 30);
            let key = marker.key;
            let spill = self.config.tuning.spill;
            let result = self
                .runtime
                .spawn_blocking(move || {
                    // B-22: a pack larger than memory holds counts against the floor.
                    if store.free_bytes().unwrap_or(u64::MAX) < floor {
                        return Err(NotPacked::Io("low on space".into()));
                    }
                    pack_block_within(&store, &key, &dir, spill)
                })
                .await
                .unwrap_or(Err(NotPacked::Io("packing stopped".into())));
            match result {
                Ok(p) => packed.push((marker, p)),
                Err(NotPacked::Gone) => finish(&marker, Move::Done),
                Err(NotPacked::Never(why)) => {
                    tracing::debug!("{}: not packed, never sent: {why}", marker.key);
                    finish(&marker, Move::Done);
                }
                Err(NotPacked::Io(why)) => {
                    tracing::debug!("{}: not packed now: {why}", marker.key);
                    finish(&marker, Move::Later);
                }
            }
        }
        if packed.is_empty() {
            return;
        }
        if self.ending.load(Ordering::Relaxed) {
            for (marker, p) in packed {
                p.pack.remove();
                release(&marker, Back::Untouched);
            }
            return;
        }
        let request = BeginRequest {
            kind: super::KIND.to_string(),
            blocks: packed
                .iter()
                .map(|(m, p)| BeginBlock {
                    address: Relay::address(&m.key),
                    hint_address: Some(Relay::address(&m.step)),
                    bytes: p.sealed.bytes,
                    pack_sha256: p.sealed.sha256,
                    outputs: p.outputs,
                    class: class_of(m.class),
                    cost_ms: u32::try_from(m.cost_ms).unwrap_or(u32::MAX),
                })
                .collect(),
            repository: self.config.repository.clone(),
            commit: self.config.commit.clone(),
        };
        let body = serde_json::to_vec(&request).unwrap_or_default();
        let signature = self
            .machine
            .as_ref()
            .map(|machine| machine.begin_signature(&self.by.org, &body));
        let begun = self
            .client
            .post_bytes::<BeginResponse>(
                blocks::BEGIN_PATH,
                body,
                self.config.tuning.lookup,
                signature,
            )
            .await;
        let answers = match begun {
            Ok(answered) => answered.body.blocks,
            Err(failure) => {
                if let Failure::Refused(..) = failure {
                    self.state().stop(Stop::Refused(failure.line()));
                }
                for (marker, p) in packed {
                    p.pack.remove();
                    finish(&marker, Move::Later);
                }
                return;
            }
        };
        // PUTs, each a task the end of the command may cut off.
        let mut puts = Vec::new();
        for (marker, p) in packed {
            let address = Relay::address(&marker.key);
            let answer = answers
                .iter()
                .find(|a| a.address == address)
                .map_or(Answer::Unknown, |a| a.answer.clone());
            let step = after_begin(&answer);
            let Answer::Put { link } = answer else {
                p.pack.remove();
                finish(&marker, step);
                continue;
            };
            if self.ending.load(Ordering::Relaxed) {
                p.pack.remove();
                release(&marker, Back::Untouched);
                continue;
            }
            let relay = self.clone();
            let size = p.sealed.bytes;
            let handle = self.runtime.spawn(async move {
                // Streamed from where the pack is, with the length and checksum the signed
                // link names in its headers.
                let (body, file) = p.pack.into_body().ok()?;
                let sent = match relay.client.link(&link) {
                    Some(request) => request.body(body).send().await.ok(),
                    None => None,
                };
                if let Some(file) = file {
                    let _ = std::fs::remove_file(file);
                }
                Some(sent?.status().as_u16())
            });
            self.state().puts.insert(marker.key, handle.abort_handle());
            puts.push((marker, size, handle));
        }
        let mut sent = Vec::new();
        for (marker, size, handle) in puts {
            let status = handle.await;
            self.state().puts.remove(&marker.key);
            match status {
                Err(err) if err.is_cancelled() => release(&marker, Back::CutOff),
                Err(_) => finish(&marker, Move::Later),
                Ok(status) => match after_put(status) {
                    Move::Next => {
                        self.state().tally.bytes_up += size;
                        sent.push(marker);
                    }
                    step => finish(&marker, step),
                },
            }
        }
        if sent.is_empty() {
            return;
        }
        let request = CommitRequest {
            kind: super::KIND.to_string(),
            addresses: sent.iter().map(|m| Relay::address(&m.key)).collect(),
            commit: self.config.commit.clone(),
        };
        let committed = self
            .client
            .post::<_, CommitResponse>(
                blocks::COMMIT_PATH,
                &request,
                self.config.tuning.lookup,
                None,
            )
            .await;
        match committed {
            Ok(answered) => {
                let mut stored = Vec::new();
                for marker in sent {
                    let address: Hex32 = Relay::address(&marker.key);
                    let state = answered
                        .body
                        .blocks
                        .iter()
                        .find(|b| b.address == address)
                        .map_or(CommitState::Unknown, |b| b.state);
                    let step = after_commit(state);
                    if step == Move::Done {
                        self.state().tally.uploaded += 1;
                        stored.push(marker.step);
                    }
                    finish(&marker, step);
                }
                self.send_hints(&stored).await;
            }
            Err(failure) => {
                if let Failure::Refused(..) = failure {
                    self.state().stop(Stop::Refused(failure.line()));
                }
                for marker in sent {
                    finish(&marker, Move::Later);
                }
            }
        }
    }
}

impl Relay {
    /// B-30: the hints of the first keys of blocks just stored, sealed with each first key, so
    /// another machine can find the second key. The newest few of each, within the protocol's
    /// limits; a hint is never trusted, so one that fails to go costs only a reuse elsewhere.
    async fn send_hints(&self, steps: &[hushrun_core::Digest]) {
        let limits = self
            .state()
            .policy
            .as_ref()
            .map(|p| p.limits.clone())
            .unwrap_or_default();
        let mut hints = Vec::new();
        let mut bytes = 0usize;
        for step in steps {
            for hint in self
                .config
                .store
                .hints(step)
                .into_iter()
                .take(HINTS_PER_BLOCK)
            {
                let Ok(sealed) = blocks::pack::seal_hint(step.as_bytes(), &hint) else {
                    continue;
                };
                if sealed.len() > limits.hint_bytes as usize
                    || hints.len() >= limits.hints_sent as usize
                    || bytes + sealed.len() > limits.hints_sent_bytes as usize
                {
                    continue;
                }
                bytes += sealed.len();
                hints.push(blocks::SentHint {
                    address: Relay::address(step),
                    list: blocks::Base64(sealed),
                });
            }
        }
        if hints.is_empty() {
            return;
        }
        let request = blocks::HintsRequest {
            kind: super::KIND.to_string(),
            hints,
            commit: self.config.commit.clone(),
        };
        let sent = self
            .client
            .post::<_, blocks::HintsResponse>(
                blocks::HINTS_PATH,
                &request,
                self.config.tuning.lookup,
                None,
            )
            .await;
        if let Err(failure @ Failure::Refused(..)) = sent {
            self.state().stop(Stop::Refused(failure.line()));
        }
    }
}

/// How many of a first key's newest hints go up with its block.
const HINTS_PER_BLOCK: usize = 2;

/// When the command ends (B-29): no new PUT starts; those in flight get `wait`, then are cut off
/// (their markers count a cut-off). Returns once the check's upload loop has stopped.
pub(crate) async fn settle(relay: &Arc<Relay>, wait: Duration) {
    let deadline = Instant::now() + wait;
    loop {
        let (uploading, puts) = {
            let state = relay.state();
            (state.uploading, state.puts.len())
        };
        if !uploading {
            return;
        }
        if Instant::now() >= deadline && puts > 0 {
            for handle in relay.state().puts.values() {
                handle.abort();
            }
        }
        tokio::time::sleep(relay.config.tuning.tick).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-29
    #[test]
    fn every_answer_has_its_row() {
        let put = Answer::Put {
            link: blocks::Link {
                method: "PUT".into(),
                url: "http://127.0.0.1:9/x".into(),
                headers: Default::default(),
                expires_in_seconds: 900,
            },
        };
        assert_eq!(after_begin(&put), Move::Next);
        assert_eq!(after_begin(&Answer::Have { scope: "s".into() }), Move::Done);
        for (reason, step) in [
            (Refusal::TooLarge, Move::Done),
            (Refusal::Unsupported, Move::Done),
            (Refusal::NoScope, Move::Done),
            (Refusal::Suspended, Move::Done),
            (Refusal::Differs, Move::DoneDiscard),
            (Refusal::Busy, Move::Later),
            (Refusal::OverAllowance, Move::Later),
            (Refusal::TooManyUploads, Move::Later),
            (Refusal::Unknown, Move::Later),
        ] {
            assert_eq!(after_begin(&Answer::Refused { reason }), step, "{reason:?}");
        }
        assert_eq!(after_begin(&Answer::Unknown), Move::Later);
        assert_eq!(after_put(Some(200)), Move::Next);
        assert_eq!(after_put(Some(412)), Move::Next);
        assert_eq!(after_put(Some(403)), Move::Later);
        assert_eq!(after_put(Some(500)), Move::Later);
        assert_eq!(after_put(None), Move::Later);
        assert_eq!(after_commit(CommitState::Stored), Move::Done);
        assert_eq!(after_commit(CommitState::Differs), Move::DoneDiscard);
        assert_eq!(after_commit(CommitState::Missing), Move::Later);
        assert_eq!(after_commit(CommitState::Gone), Move::Later);
        assert_eq!(after_commit(CommitState::Unknown), Move::Later);
    }
}
