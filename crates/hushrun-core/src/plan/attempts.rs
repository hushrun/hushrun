//! Run attempts (§8.3, P-5, P-6).

use std::collections::BTreeMap;

use crate::event::{EventKind, SupersededReason, VerifiedEvent};
use crate::plan::{Liveness, LivenessAnswer, counts_for};
use crate::policy::Policy;
use crate::run::RunRef;
use crate::signers::CiOnly;
use crate::time::{Dur, Time};

/// P-6: within this long after a `failed`, a `passed` can't override it (signer clock skew).
/// Writers that skip a `passed` P-6 would ignore read it from here too.
pub const FAILED_WINS_WITHIN: Dur = Dur::from_secs(60);

/// P-6: whether a `passed` at `passed` is too soon after a `failed` at `failed` to override it.
pub fn failure_wins(failed: Time, passed: Time) -> bool {
    passed.millis_since(failed) < FAILED_WINS_WITHIN.millis() as i64
}

/// The state an attempt's terminal events give it (P-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Passed,
    Failed,
    /// With the reason of the event that set it.
    Superseded(Option<SupersededReason>),
}

impl Terminal {
    pub fn as_str(self) -> &'static str {
        match self {
            Terminal::Passed => "passed",
            Terminal::Failed => "failed",
            Terminal::Superseded(_) => "superseded",
        }
    }
}

/// P-5: whether `event` is one of the lifecycle events that make the pipeline's run attempts:
/// from a signer that counts (T-16). \[ADR 0062\] Lifecycle events hushrun 0.8 and earlier
/// recorded for a stage belong to that stage's pipeline, not this one.
pub fn records_attempt(event: &VerifiedEvent, ci_only: &CiOnly) -> bool {
    event.event.kind.is_lifecycle() && event.event.stage.is_none() && counts_for(ci_only, event)
}

/// P-6 for an attempt with terminal events: its `events` (lifecycle events, in E-9 order) in
/// order from no state, and the event that set the state last. `None` without terminal events.
pub fn terminal<'e>(events: &[&'e VerifiedEvent]) -> Option<(Terminal, &'e VerifiedEvent)> {
    let mut state: Option<(Terminal, &VerifiedEvent)> = None;
    let mut failed_at: Option<Time> = None;
    for &event in events {
        match event.event.kind {
            EventKind::Failed => {
                state = Some((Terminal::Failed, event));
                failed_at = Some(failed_at.map_or(event.event.time, |t| t.max(event.event.time)));
            }
            EventKind::Passed => {
                let within_skew = matches!(state, Some((Terminal::Failed, _)))
                    && failed_at.is_some_and(|f| failure_wins(f, event.event.time));
                if !within_skew {
                    state = Some((Terminal::Passed, event));
                }
            }
            EventKind::Superseded if state.is_none() => {
                let reason = event.event.superseded.as_ref().map(|s| s.reason);
                state = Some((Terminal::Superseded(reason), event));
            }
            _ => {}
        }
    }
    state
}

/// P-6 for an attempt without terminal events and no liveness answer: in flight until this
/// time, then unknown. Its `started` events' `leaseUntil`, each capped at the event's `time`
/// plus `lease`, `deploy.lease` (E-5); `None` without a `started` event.
pub fn lease_end(events: &[&VerifiedEvent], lease: Option<Dur>) -> Option<Time> {
    events
        .iter()
        .filter_map(|e| {
            let started = e.event.started.as_ref()?;
            let cap = lease.map_or(started.lease_until, |l| e.event.time.saturating_add(l));
            Some(started.lease_until.min(cap))
        })
        .max()
}

/// The state of one run attempt (P-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptState {
    InFlight,
    Passed,
    Failed,
    /// With the reason of the event that set it; `None` when liveness reported it.
    Superseded(Option<SupersededReason>),
    /// It passed in CI but recorded no outcome.
    NoVerdict,
    /// No terminal event, no liveness answer, and the lease ran out.
    Unknown,
}

/// One run attempt of the pipeline, for one digest (P-5).
#[derive(Debug, Clone)]
pub(crate) struct Attempt<'a> {
    pub run: RunRef,
    /// Its lifecycle events, in E-9 order.
    pub events: Vec<&'a VerifiedEvent>,
    pub state: AttemptState,
}

impl Attempt<'_> {
    /// The event attempts are ordered by: `started`, else the first event.
    fn anchor(&self) -> &VerifiedEvent {
        self.events
            .iter()
            .find(|e| e.event.kind == EventKind::Started)
            .copied()
            .unwrap_or(self.events[0])
    }
}

/// P-5: the pipeline's attempts for this digest, oldest first, from lifecycle events by signers
/// that count (T-16), with their states (P-6).
pub(crate) fn attempts<'a>(
    events: &'a [VerifiedEvent],
    liveness: &[LivenessAnswer],
    policy: &Policy,
    ci_only: &CiOnly,
    now: Time,
) -> Vec<Attempt<'a>> {
    let mut groups: BTreeMap<RunRef, Vec<&VerifiedEvent>> = BTreeMap::new();
    for event in events {
        if records_attempt(event, ci_only) {
            groups
                .entry(event.event.run.run_ref())
                .or_default()
                .push(event);
        }
    }
    let lease = Some(policy.deploy_or_default().lease);
    let mut attempts: Vec<Attempt<'a>> = groups
        .into_iter()
        .map(|(run, events)| {
            let liveness = liveness.iter().find(|l| l.run == run).map(|l| l.liveness);
            let state = state_of(&events, liveness, lease, now);
            Attempt { run, events, state }
        })
        .collect();
    attempts.sort_by(|a, b| {
        let (a, b) = (a.anchor(), b.anchor());
        a.event
            .time
            .cmp(&b.event.time)
            .then_with(|| a.key.cmp(&b.key))
    });
    attempts
}

/// The pipeline's attempts with no terminal event, as P-5 groups them: the runs to ask the CI
/// provider about (P-6's liveness).
pub fn attempts_without_verdict(events: &[VerifiedEvent], ci_only: &CiOnly) -> Vec<RunRef> {
    let mut terminal: BTreeMap<RunRef, bool> = BTreeMap::new();
    for event in events {
        if records_attempt(event, ci_only) {
            *terminal.entry(event.event.run.run_ref()).or_default() |=
                event.event.kind.is_terminal();
        }
    }
    terminal
        .into_iter()
        .filter(|(_, done)| !done)
        .map(|(run, _)| run)
        .collect()
}

/// P-6: an attempt's state from its terminal events, else liveness, else the lease.
fn state_of(
    events: &[&VerifiedEvent],
    liveness: Option<Liveness>,
    lease: Option<Dur>,
    now: Time,
) -> AttemptState {
    if let Some((state, _)) = terminal(events) {
        return match state {
            Terminal::Passed => AttemptState::Passed,
            Terminal::Failed => AttemptState::Failed,
            Terminal::Superseded(reason) => AttemptState::Superseded(reason),
        };
    }
    match liveness {
        Some(Liveness::InFlight) => AttemptState::InFlight,
        Some(Liveness::NoVerdict) => AttemptState::NoVerdict,
        Some(Liveness::Superseded) => AttemptState::Superseded(None),
        Some(Liveness::Failed) => AttemptState::Failed,
        // In flight until the lease ends, capped at `time` plus `deploy.lease` (E-5).
        None => match lease_end(events, lease) {
            Some(end) if now < end => AttemptState::InFlight,
            _ => AttemptState::Unknown,
        },
    }
}
