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
//! | claimed | done, the local block discarded (and said) | `begin` refused `differs`: a trusted copy with other bytes exists |
//! | claimed, sent | waiting, with a date | a network error, a 5xx, 401, 403, 404, an expired link, `busy`, `over_allowance`, `too_many_uploads`, `commit` answering `missing`, `gone` or `differs` (the upload arrived broken), any word this version doesn't know |
//! | claimed, sent | waiting, one more cut-off | the command's end cut off its `begin`, PUT or `commit` in flight |
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
        // The store holds a size or SHA-256 other than the one `begin` named: the upload arrived
        // broken, and the block here is good. Only `begin`'s `differs` says a trusted copy with
        // other bytes exists.
        CommitState::Differs | CommitState::Missing | CommitState::Gone | CommitState::Unknown => {
            Move::Later
        }
    }
}

/// What decides how long the end of a command waits for uploads (B-29, B-31).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct End {
    /// A CI job: its store goes with the job, so what it built goes up now or never.
    pub(crate) in_ci: bool,
    /// The job writes the repository's trusted blocks, as `Policy::writes_trusted` says.
    pub(crate) trusted: bool,
    /// What the compiles of the blocks still to go up took, together.
    pub(crate) waiting_cost: Duration,
    /// What this `hushrun run` call already waited at the end of its earlier checks.
    pub(crate) waited: Duration,
}

/// The end of a command's one deadline (B-29, B-31), from the moment the command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EndWait {
    /// Uploads (the drain, PUTs, and `begin` and `commit` in flight) are cut off then.
    pub(crate) uploads: Duration,
    /// Everything after the command, `close` included, is over then.
    pub(crate) total: Duration,
}

/// How long the end of a command waits (B-29, B-31), the one place that says. Uploads may take,
/// on a person's machine, the tuning's 5 seconds for those in flight; in CI, for every block
/// still to go, up to 2 minutes for a job that writes trusted blocks and 20 seconds for any
/// other, never more than a quarter of what the waiting blocks' compiles took, and both for the
/// whole `hushrun run` call, less what its earlier checks waited. The whole end is that and
/// `close`'s 5 seconds on a person's machine; in CI that, and never less than `close`'s time.
/// Uploads stop when `close`'s time (or half the whole, if less) is all that is left.
pub(crate) fn wait_at_end(tuning: &Tuning, end: &End) -> EndWait {
    let (uploads, total) = if end.in_ci {
        let most = if end.trusted {
            tuning.ci_trusted_wait
        } else {
            tuning.ci_wait
        };
        let uploads = most.saturating_sub(end.waited).min(end.waiting_cost / 4);
        (uploads, uploads.max(tuning.close))
    } else {
        (tuning.end_wait, tuning.end_wait + tuning.close)
    };
    let for_close = tuning.close.min(total / 2);
    EndWait {
        uploads: uploads.min(total - for_close),
        total,
    }
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
                // The check it was started for is over: a later check starts its own.
                if state.check.is_none() {
                    return;
                }
                let phase = state.phase_now(Instant::now()).clone();
                let writes = state.uploads && state.policy.as_ref().is_some_and(|p| p.writes);
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
                if self.config.store.discard(&marker.key).is_ok() {
                    self.state().tally.discarded += 1;
                }
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
            .until_cut(self.client.post_bytes::<BeginResponse>(
                blocks::BEGIN_PATH,
                body,
                self.config.tuning.lookup,
                signature,
            ))
            .await;
        let answers = match begun {
            Some(Ok(answered)) => answered.body.blocks,
            None => {
                for (marker, p) in packed {
                    p.pack.remove();
                    release(&marker, Back::CutOff);
                }
                return;
            }
            Some(Err(failure)) => {
                // Uploads rest as downloads do: a refusal stops sharing, and a server that
                // can't be reached or fails counts toward the rest (B-26).
                {
                    let mut state = self.state();
                    self.request_failed(&mut state, failure);
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
                    Some(request) => match request.body(body).send().await {
                        Ok(response) => Some(response),
                        // B-26: the store's host, named once, by its name alone.
                        Err(err) => {
                            if err.is_connect() {
                                relay.state().store_unreachable(&link.url);
                            }
                            None
                        }
                    },
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
            // A PUT the object store never answered, or answered with a 5xx, is a failure.
            if let Ok(None | Some(500..=599)) = &status {
                let why = match &status {
                    Ok(Some(code)) => format!("the store answered {code}"),
                    _ => "the store couldn't be reached".to_string(),
                };
                let mut state = self.state();
                self.request_failed(&mut state, Failure::Unreachable(why));
            }
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
            .until_cut(self.client.post::<_, CommitResponse>(
                blocks::COMMIT_PATH,
                &request,
                self.config.tuning.lookup,
                None,
            ))
            .await;
        match committed {
            None => {
                for marker in sent {
                    release(&marker, Back::CutOff);
                }
            }
            Some(Ok(answered)) => {
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
            Some(Err(failure)) => {
                {
                    let mut state = self.state();
                    self.request_failed(&mut state, failure);
                }
                for marker in sent {
                    finish(&marker, Move::Later);
                }
            }
        }
    }
}

impl Relay {
    /// A request of the upload loop, given up (`None`) when the end of the command cuts uploads
    /// off (B-29): it is waited for no longer than the end's one deadline.
    async fn until_cut<T>(&self, request: impl std::future::Future<Output = T>) -> Option<T> {
        let mut cut = self.cut.subscribe();
        let cut = async move {
            let _ = cut.wait_for(|cut| *cut).await;
        };
        let request = std::pin::pin!(request);
        let cut = std::pin::pin!(cut);
        match futures_util::future::select(request, cut).await {
            futures_util::future::Either::Left((answer, _)) => Some(answer),
            futures_util::future::Either::Right(_) => None,
        }
    }

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
            .until_cut(self.client.post::<_, blocks::HintsResponse>(
                blocks::HINTS_PATH,
                &request,
                self.config.tuning.lookup,
                None,
            ))
            .await;
        if let Some(Err(failure @ Failure::Refused(..))) = sent {
            self.state().stop(Stop::Refused(failure.line()));
        }
    }
}

/// How many of a first key's newest hints go up with its block.
const HINTS_PER_BLOCK: usize = 2;

/// B-31: at the end of a CI job's command, the upload loop goes on, up to `until`, while a block
/// of this run's is still to go: one waiting that may go now, or one this run holds. It stops
/// early once none is, or the loop stopped.
pub(crate) async fn drain(relay: &Arc<Relay>, until: Instant) {
    let deadline = until;
    let outbox = Outbox::new(&relay.config.store);
    while Instant::now() < deadline {
        relay.wake.notify_one();
        let now = now_ms();
        let left = outbox
            .waiting()
            .iter()
            .any(|m| m.by == relay.by && m.not_before_ms <= now)
            || !outbox.claimed(&relay.run).is_empty();
        if !left || !relay.state().uploading {
            return;
        }
        tokio::time::sleep(relay.config.tuning.tick.max(Duration::from_millis(10))).await;
    }
}

/// When the command ends (B-29): no new PUT starts; the PUTs, `begin`s and `commit`s in flight
/// get until `cut`, then are cut off (their markers count a cut-off). Returns once the check's
/// upload loop has stopped, or at `deadline`, the end's own, whatever it is doing.
pub(crate) async fn settle(relay: &Arc<Relay>, cut: Instant, deadline: Instant) {
    loop {
        if !relay.state().uploading {
            return;
        }
        let now = Instant::now();
        if now >= cut {
            relay.cut.send_replace(true);
            for handle in relay.state().puts.values() {
                handle.abort();
            }
        }
        if now >= deadline {
            return;
        }
        tokio::time::sleep(relay.config.tuning.tick).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-31
    #[test]
    fn the_end_of_a_command_waits_as_long_as_its_context_says_and_no_longer() {
        let tuning = Tuning::default();
        let end = |in_ci, trusted, cost_s: u64, waited_s: u64| {
            let wait = wait_at_end(
                &tuning,
                &End {
                    in_ci,
                    trusted,
                    waiting_cost: Duration::from_secs(cost_s),
                    waited: Duration::from_secs(waited_s),
                },
            );
            (wait.uploads.as_millis(), wait.total.as_millis())
        };
        // A person's machine: the 5 seconds for those in flight, whatever waits, then `close`'s 5.
        assert_eq!(end(false, true, 10_000, 0), (5_000, 10_000));
        assert_eq!(end(false, false, 0, 0), (5_000, 10_000));
        // CI: up to 2 minutes for a trusted job, 20 seconds for any other, `close` within them.
        assert_eq!(end(true, true, 10_000, 0), (115_000, 120_000));
        assert_eq!(end(true, false, 10_000, 0), (15_000, 20_000));
        // Never more than a quarter of what the waiting blocks' compiles took; `close` keeps its
        // 5 seconds, or half the whole when that is less.
        assert_eq!(end(true, true, 40, 0), (5_000, 10_000));
        assert_eq!(end(true, false, 32, 0), (4_000, 8_000));
        assert_eq!(end(true, false, 4, 0), (1_000, 5_000));
        assert_eq!(end(true, true, 0, 0), (0, 5_000));
        // For the whole call: what earlier checks waited is spent.
        assert_eq!(end(true, false, 10_000, 15), (2_500, 5_000));
        assert_eq!(end(true, true, 10_000, 200), (0, 5_000));
    }

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
        assert_eq!(after_commit(CommitState::Differs), Move::Later);
        assert_eq!(after_commit(CommitState::Missing), Move::Later);
        assert_eq!(after_commit(CommitState::Gone), Move::Later);
        assert_eq!(after_commit(CommitState::Unknown), Move::Later);
    }
}
