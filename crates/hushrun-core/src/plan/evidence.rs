//! Evidence (§8.2): which events count for each required check, and its status.

use crate::event::{EvidenceKind, Outcome, VerifiedEvent, pinned_digest};
use crate::names::CheckName;
use crate::plan::{Status, counts_for};
use crate::policy::{CheckDef, Policy};
use crate::signers::CiOnly;
use crate::time::Time;

/// A requirement's status, and the event that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Evaluated<'a> {
    pub status: Status,
    pub event: Option<&'a VerifiedEvent>,
}

/// Evaluates one required check (P-1, P-4), from the signers that count (T-16). `events` are in
/// E-9 order.
pub(crate) fn evaluate<'a>(
    check: &CheckName,
    events: &'a [VerifiedEvent],
    policy: &Policy,
    ci_only: &CiOnly,
    now: Time,
) -> Evaluated<'a> {
    let Some(def) = policy.checks.get(check) else {
        return Evaluated {
            status: Status::Missing,
            event: None,
        };
    };
    let candidates = check_candidates(check, def, events, ci_only);
    status_of(def, candidates.last().copied(), now)
}

/// P-1: the source check events that count, from the signers that count (T-16). A source check's
/// evidence is about the files, so hushrun 0.8's events recorded at a stage count too; its
/// deployment checks' events never do (ADR 0062).
fn check_candidates<'a>(
    check: &CheckName,
    def: &CheckDef,
    events: &'a [VerifiedEvent],
    ci_only: &CiOnly,
) -> Vec<&'a VerifiedEvent> {
    events
        .iter()
        .filter(|e| {
            e.event.check.as_ref().is_some_and(|c| {
                &c.name == check
                    && c.definition_digest == def.definition_digest
                    && c.check_kind == EvidenceKind::Source
            })
        })
        .filter(|e| counts_for(ci_only, e))
        .filter(|e| environment_matches(def, e))
        .collect()
}

/// E-7: a check that pins an image or platform counts only evidence from that environment.
fn environment_matches(def: &CheckDef, event: &VerifiedEvent) -> bool {
    if def.image.is_none() && def.platform.is_none() {
        return true;
    }
    let Some(environment) = event
        .event
        .check
        .as_ref()
        .and_then(|c| c.environment.as_ref())
    else {
        return false;
    };
    let image_ok = match &def.image {
        None => true,
        Some(pinned) => environment.image.as_deref().is_some_and(|image| {
            pinned_digest(image).is_some() && pinned_digest(image) == pinned_digest(pinned)
        }),
    };
    let platform_ok = def.platform.is_none() || environment.platform == def.platform;
    image_ok && platform_ok
}

/// P-4: the newest candidate decides.
fn status_of<'a>(def: &CheckDef, newest: Option<&'a VerifiedEvent>, now: Time) -> Evaluated<'a> {
    let Some(event) = newest else {
        return Evaluated {
            status: Status::Missing,
            event: None,
        };
    };
    let (passed, measured_from) = match &event.event.check {
        Some(check) => (
            check.result == Outcome::Pass,
            check.finished_at.min(event.event.time),
        ),
        None => (false, event.event.time),
    };
    let status = if !passed {
        Status::Failed
    } else if def
        .max_age
        .is_some_and(|max| now.millis_since(measured_from) > max.millis() as i64)
    {
        Status::Expired
    } else {
        Status::Satisfied
    };
    Evaluated {
        status,
        event: Some(event),
    }
}
