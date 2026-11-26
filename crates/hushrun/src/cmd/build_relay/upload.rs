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

use hushrun_build::model::{Class as LocalClass, Kind};
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
    /// Done, the local block stays and its first key is kept on this machine: a trusted copy
    /// of a linked output exists with other bytes, and this machine's linker makes these.
    DoneKeepLocal,
    /// Waiting again, tried after a while.
    Later,
}

/// `begin`'s answer for one block of kind `kind` and class `class`. `differs` for a library says
/// one of two builds had other inputs than its key names, and the trusted copy is served in the
/// local one's place. For a linked output it says the two machines' link environments differ in
/// something the key doesn't hold (B-37): the machine keeps what its own linker makes. For a
/// block a build tool keyed itself it says nothing about either copy, whose bytes needn't repeat
/// (B-81, B-175): the block built here stays, and nothing more is done.
pub(crate) fn after_begin(answer: &Answer, kind: Kind, class: LocalClass) -> Move {
    match answer {
        Answer::Put { .. } => Move::Next,
        Answer::Have { .. } => Move::Done,
        Answer::Refused { reason } => match reason {
            Refusal::TooLarge | Refusal::Unsupported | Refusal::NoScope | Refusal::Suspended => {
                Move::Done
            }
            Refusal::Differs if kind.tool_keyed() => Move::Done,
            Refusal::Differs if class == LocalClass::Library => Move::DoneDiscard,
            Refusal::Differs => Move::DoneKeepLocal,
            Refusal::Busy | Refusal::OverAllowance | Refusal::TooManyUploads | Refusal::Unknown => {
                Move::Later
            }
        },
        Answer::Unknown => Move::Later,
    }
}

/// Whether `begin` refused a block for good: it never goes up, and the marker is done.
pub(crate) fn refused_for_good(reason: Refusal) -> bool {
    after_begin(
        &Answer::Refused { reason },
        Kind::Rustc,
        LocalClass::Library,
    ) == Move::Done
}

/// What a refusal says, in words.
pub(crate) fn refusal_words(reason: Refusal) -> &'static str {
    match reason {
        Refusal::TooLarge => "too large",
        Refusal::Unsupported => "a class the backend doesn't take",
        Refusal::NoScope => "this job writes nowhere",
        Refusal::Suspended => "suspended in the repository",
        Refusal::OverAllowance => "the org's store is full",
        Refusal::Differs => "a trusted copy differs",
        Refusal::Busy => "busy",
        Refusal::TooManyUploads => "too many uploads under way",
        Refusal::Unknown => "a reason this version doesn't know",
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

/// What decides how long the end of a command waits for uploads (B-29, B-31, ADR 0144).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct End {
    /// B-49: a stop signal came while COMMAND ran: nothing is waited for but `close`.
    pub(crate) stopping: bool,
}

/// B-49: the most a run that was asked to stop gives `close`: GitLab's grace period before it
/// kills a cancelled job is short.
pub(crate) const STOPPED_CLOSE: Duration = Duration::from_secs(2);

/// The end of a command's one deadline (B-29, B-31), from the moment the command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EndWait {
    /// Uploads (the drain, PUTs, and `begin` and `commit` in flight) are cut off then.
    pub(crate) uploads: Duration,
    /// Everything after the command, `close` included, is over then.
    pub(crate) total: Duration,
}

/// How long the end of a command waits (B-29, B-31), the one place that says: no deadline at
/// all (`None`), on a person's machine, in a sandbox and in CI alike, unless a stop signal came.
/// Every upload the run built then goes on until it is done or an answer (or a server's silence)
/// puts it off, and `close` follows: what one machine leaves out of its manifest depends on what
/// the backend answered, never on how loaded it was (ADR 0144). Asked to stop, the run cuts its
/// uploads off at once and gives `close` [`STOPPED_CLOSE`].
pub(crate) fn wait_at_end(tuning: &Tuning, end: &End) -> Option<EndWait> {
    end.stopping.then(|| EndWait {
        uploads: Duration::ZERO,
        total: STOPPED_CLOSE.min(tuning.close),
    })
}

/// The class a block goes up as. A block a build tool keyed itself (Gradle's, Maven's) is
/// `other`, whatever its record says: the server suspends a key whose trusted `library` copies
/// differ, and deletes the trusted one, where such a tool's two honest executions may differ in
/// a date (B-81, B-175). `other` it stores and never compares.
fn class_of(kind: Kind, class: hushrun_build::model::Class) -> Class {
    if kind.tool_keyed() {
        return Class::Other;
    }
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
    /// Starts the check's upload loop ([`Relay::uploads`]), unless one runs. It is marked as
    /// running here, before it is spawned: an end of the command that came before the runtime
    /// first polled it would otherwise find no loop, wait for none, and leave every upload for
    /// the next run.
    pub(crate) fn start_uploads(self: &Arc<Relay>) {
        {
            let mut state = self.state();
            if state.uploading {
                return;
            }
            state.uploading = true;
        }
        let relay = self.clone();
        self.runtime.spawn(async move { relay.uploads().await });
    }

    /// The check's upload loop: while the run shares and the policy says it writes, claims a
    /// few waiting markers of this run's host, org and credential, packs them, and sends them.
    /// [`Relay::start_uploads`] marked it as running.
    async fn uploads(self: Arc<Relay>) {
        // However the loop ends, a panic included, it is over: the end of the command waits
        // for it no longer, and a later check starts its own. A panic's claims wait for the
        // run's death, as a crash's do (B-14, B-29).
        struct Over<'a>(&'a Relay);
        impl Drop for Over<'_> {
            fn drop(&mut self) {
                self.0.state().uploading = false;
            }
        }
        let _over = Over(&self);
        self.clone().upload_loop().await;
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
            // One kind a batch: `begin` and `commit` name one kind.
            let kind = ready[0].kind;
            let mut claimed = Vec::new();
            for marker in ready.into_iter().filter(|m| m.kind == kind) {
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
        let kind = claimed.first().map_or(Kind::Rustc, |m| m.kind);
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
            Move::DoneKeepLocal => {
                let _ = outbox.done(&run, &marker.key);
                let now = std::time::SystemTime::now();
                let until = now + Duration::from_secs(u64::from(self.config.keep_days) * 86_400);
                if self
                    .config
                    .store
                    .keep_local(
                        &marker.step,
                        "the repository's trusted copy was linked otherwise",
                        until,
                        now,
                    )
                    .is_ok()
                {
                    self.state().tally.kept_local += 1;
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
        // B-49: nothing is begun that the credential can't finish (a GitLab job's token near
        // its end): the markers wait, untouched, and the run says so once.
        let expiring = !self.client.can_begin();
        if self.ending.load(Ordering::Relaxed) || expiring {
            for (marker, p) in packed {
                p.pack.remove();
                release(&marker, Back::Untouched);
            }
            if expiring {
                self.state().say(format!(
                    "build: no more uploads this run: the job's ID token expires in less than {} minutes",
                    super::credential::LEAST_LEFT_TO_BEGIN.as_secs() / 60
                ));
            }
            return;
        }
        let request = BeginRequest {
            kind: kind.word().to_string(),
            blocks: packed
                .iter()
                .map(|(m, p)| BeginBlock {
                    address: Relay::address_in(kind, &m.key),
                    hint_address: Some(Relay::address_in(kind, &m.step)),
                    bytes: p.sealed.bytes,
                    pack_sha256: p.sealed.sha256,
                    outputs: p.outputs,
                    class: class_of(kind, m.class),
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
        #[cfg(test)]
        super::fault::upload();
        // PUTs, each a task the end of the command may cut off.
        let mut puts = Vec::new();
        for (marker, p) in packed {
            let address = Relay::address_in(kind, &marker.key);
            let answer = answers
                .iter()
                .find(|a| a.address == address)
                .map_or(Answer::Unknown, |a| a.answer.clone());
            let step = after_begin(&answer, kind, marker.class);
            // B-75: refused for good, it is never on the backend: a bake's build that made it
            // isn't whole.
            if let Answer::Refused { reason } = &answer
                && refused_for_good(*reason)
            {
                self.state().refused.push((marker.key, *reason));
            }
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
                let moved = super::pack::Moved::new();
                let (body, file) = p.pack.into_body(&moved).ok()?;
                // The body is streamed: its length goes in the request, as the link names it.
                let named = link
                    .headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case("content-length"));
                let sent = match relay.client.link(&link) {
                    Some(request) => {
                        let request = if named {
                            request
                        } else {
                            request.header("content-length", size.to_string())
                        };
                        // ADR 0144: an upload goes on however long it takes, and ends early only
                        // when its body has not moved, nor an answer come, for the silence.
                        let stalled = async {
                            while moved.still_for() < super::http::SILENCE {
                                tokio::time::sleep(Duration::from_secs(1)).await;
                            }
                        };
                        tokio::select! {
                            sent = request.body(body).send() => match sent {
                                Ok(response) => Some(response),
                                // B-26: the store's host, named once, by its name alone.
                                Err(err) => {
                                    if err.is_connect() {
                                        relay.state().store_unreachable(&link.url);
                                    }
                                    None
                                }
                            },
                            () = stalled => None,
                        }
                    }
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
            kind: kind.word().to_string(),
            addresses: sent
                .iter()
                .map(|m| Relay::address_in(kind, &m.key))
                .collect(),
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
                    let address: Hex32 = Relay::address_in(kind, &marker.key);
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
                // A Go block has no hints.
                if kind != Kind::Go {
                    self.send_hints(kind, &stored).await;
                }
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
    async fn send_hints(&self, kind: Kind, steps: &[hushrun_core::Digest]) {
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
                    address: Relay::address_in(kind, step),
                    list: blocks::Base64(sealed),
                });
            }
        }
        if hints.is_empty() {
            return;
        }
        let request = blocks::HintsRequest {
            kind: kind.word().to_string(),
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

/// B-31, ADR 0144: at the end of a command, the upload loop goes on while a block of this run's
/// is still to go (one waiting that may go now, or one this run holds), until `until` when there
/// is one. It stops once none is, the loop stopped, or the relay rests or stopped.
pub(crate) async fn drain(relay: &Arc<Relay>, until: Option<Instant>) {
    let outbox = Outbox::new(&relay.config.store);
    while until.is_none_or(|until| Instant::now() < until) {
        relay.wake.notify_one();
        let now = now_ms();
        let left = outbox
            .waiting()
            .iter()
            .any(|m| m.by == relay.by && m.not_before_ms <= now)
            || !outbox.claimed(&relay.run).is_empty();
        // Resting after failures, or stopped (B-26): no new request goes, and what is left
        // waits for the next run. An `open` still on the wire is waited for: what goes up
        // never depends on how fast the command was.
        let sending = {
            let mut state = relay.state();
            state.uploading
                && !matches!(
                    state.phase_now(Instant::now()),
                    Phase::Resting(_) | Phase::Stopped(_)
                )
        };
        if !left || !sending {
            return;
        }
        tokio::time::sleep(relay.config.tuning.tick.max(Duration::from_millis(10))).await;
    }
}

/// When the command ends (B-29): no new PUT starts; the PUTs, `begin`s and `commit`s in flight
/// get until `cut`, then are cut off (their markers count a cut-off). Returns once the check's
/// upload loop has stopped, or at `deadline`, the end's own, whatever it is doing. With neither
/// (a settled end, `Tuning::settle`), only once the loop has stopped.
pub(crate) async fn settle(relay: &Arc<Relay>, cut: Option<Instant>, deadline: Option<Instant>) {
    loop {
        if !relay.state().uploading {
            return;
        }
        let now = Instant::now();
        if cut.is_some_and(|cut| now >= cut) {
            relay.cut.send_replace(true);
            for handle in relay.state().puts.values() {
                handle.abort();
            }
        }
        if deadline.is_some_and(|deadline| now >= deadline) {
            return;
        }
        tokio::time::sleep(relay.config.tuning.tick).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-29, B-31, B-49
    #[test]
    fn the_end_of_a_command_has_no_deadline_unless_it_was_asked_to_stop() {
        let tuning = Tuning::default();
        // ADR 0144: a person's machine, a sandbox and a CI job alike: no cut, no deadline.
        assert_eq!(wait_at_end(&tuning, &End { stopping: false }), None);
        // B-49: a run asked to stop waits for no upload, and gives `close` 2 seconds at most.
        assert_eq!(
            wait_at_end(&tuning, &End { stopping: true }),
            Some(EndWait {
                uploads: Duration::ZERO,
                total: Duration::from_secs(2),
            })
        );
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
        let after_begin = |answer: &Answer| after_begin(answer, Kind::Rustc, LocalClass::Library);
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
        // A linked output the trusted copy disagrees with stays, kept to this machine; every
        // other answer moves it as it moves a library.
        for class in [
            LocalClass::ProcMacro,
            LocalClass::BuildScript,
            LocalClass::Program,
        ] {
            let differs = Answer::Refused {
                reason: Refusal::Differs,
            };
            assert_eq!(
                super::after_begin(&differs, Kind::Rustc, class),
                Move::DoneKeepLocal
            );
            let busy = Answer::Refused {
                reason: Refusal::Busy,
            };
            assert_eq!(super::after_begin(&busy, Kind::Rustc, class), Move::Later);
            assert_eq!(super::after_begin(&put, Kind::Rustc, class), Move::Next);
        }
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

    // covers: B-81, B-175
    #[test]
    fn a_block_a_build_tool_keyed_goes_up_as_other_and_stays_when_a_trusted_copy_differs() {
        // The server suspends a key whose trusted `library` copies differ and deletes the
        // trusted one; two honest Gradle executions or Maven builds may differ in a date. Their
        // blocks go up as `other`, which the server never compares, and a `differs` leaves the
        // block built here as it is.
        use hushrun_build::model::Class as Local;
        for kind in [Kind::Gradle, Kind::Maven] {
            assert!(kind.tool_keyed(), "{kind:?}");
            assert_eq!(class_of(kind, Local::Library), Class::Other, "{kind:?}");
            let differs = Answer::Refused {
                reason: Refusal::Differs,
            };
            assert_eq!(
                after_begin(&differs, kind, LocalClass::Library),
                Move::Done,
                "{kind:?}"
            );
            let busy = Answer::Refused {
                reason: Refusal::Busy,
            };
            assert_eq!(after_begin(&busy, kind, LocalClass::Library), Move::Later);
        }
        // Every other kind keeps the class its record names.
        for kind in [Kind::Rustc, Kind::Go, Kind::Cc, Kind::Xcode, Kind::Tsc] {
            assert!(!kind.tool_keyed(), "{kind:?}");
            assert_eq!(class_of(kind, Local::Library), Class::Library, "{kind:?}");
            assert_eq!(class_of(kind, Local::Program), Class::Program, "{kind:?}");
        }
    }
}
