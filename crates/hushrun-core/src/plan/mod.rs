//! The planner (§8): a pure function from the policy and the verified events of one digest to
//! a decision. No IO, no clock, no environment: `plan --explain-json`
//! records the input, and the same input always gives the same decision.
//!
//! \[ADR 0062\] A repository has one pipeline: no stage, no upstream, no approval.

mod artifacts;
mod attempts;
mod evidence;
mod types;

use std::collections::BTreeMap;
use std::fmt::Write as _;

pub use attempts::{
    FAILED_WINS_WITHIN, Terminal, attempts_without_verdict, failure_wins, lease_end,
    records_attempt, terminal,
};
pub use types::*;

use crate::event::{EventKind, VerifiedEvent, sort_events};
use crate::names::{CheckName, ScopeName};
use crate::policy::{DeployDef, Policy};
use crate::signers::CiOnly;

use attempts::attempts;

/// The status of one required check, as P-1 and P-4 decide it, with the key of the deciding
/// event: what `hushrun run` skips when it is satisfied (§10.4), and what an index shows.
/// `events` must be this digest's, scope's and repository's; `ci_only` is the signers answer's
/// (T-16).
pub fn check_status(
    check: &CheckName,
    events: &[VerifiedEvent],
    policy: &Policy,
    ci_only: &CiOnly,
    now: crate::time::Time,
) -> (Status, Option<String>) {
    let mut sorted = events.to_vec();
    sort_events(&mut sorted);
    let evaluated = evidence::evaluate(check, &sorted, policy, ci_only, now);
    (evaluated.status, evaluated.event.map(|e| e.key.clone()))
}

/// T-16: whether `event` counts: every signer that counts (T-15) does, but while the repository
/// was CI-only (`ci_only`), only its CI, and what others signed with a trusted time outside every
/// period it was. An answer that still lists periods per stage is read as their union.
pub fn counts_for(ci_only: &CiOnly, event: &VerifiedEvent) -> bool {
    ci_only.counts_anywhere(&event.signer, event.trusted_time)
}

/// \[M8\] The newest trusted `deployed` event of `scope` among the deployed index's entries
/// (L-8, P-10), from a signer that counts (T-16). \[ADR 0062\] Deployments hushrun 0.8 and
/// earlier recorded for a stage were that stage's.
pub fn newest_deployment(
    deployed: &[VerifiedEvent],
    scope: &ScopeName,
    policy: &Policy,
    ci_only: &CiOnly,
) -> Option<DeployedReport> {
    deployed
        .iter()
        .filter(|e| {
            e.event.kind == EventKind::Deployed
                && e.event.stage.is_none()
                && &e.event.scope == scope
                && e.event.repository == policy.repository
                && counts_for(ci_only, e)
        })
        .max_by(|a, b| {
            a.event
                .time
                .cmp(&b.event.time)
                .then_with(|| b.key.cmp(&a.key))
        })
        .map(|e| DeployedReport {
            digest: e.digest,
            event: e.key.clone(),
            time: e.event.time,
        })
}

/// P-8: why the pipeline refuses to run on `branch`, when `deploy.branch` names another. An
/// unknown branch (local runs) passes: `guard` checks it before deploying. Nothing overrides
/// this rule, neither forcing (P-9) nor `on_repository_error: full` (L-5).
pub fn wrong_branch(deploy: &DeployDef, branch: Option<&str>) -> Option<String> {
    match (&deploy.branch, branch) {
        (Some(only), Some(on)) if only != on => Some(format!(
            "this repository deploys only from {only}; this run is on {on}, so run it from {only}"
        )),
        _ => None,
    }
}

/// The events the planner sees: about this repository, scope and digest, in E-9 order (I-3,
/// I-4).
fn events_about(input: &PlanInput, policy: &Policy) -> Vec<VerifiedEvent> {
    let mut events: Vec<VerifiedEvent> = input
        .events
        .iter()
        .filter(|e| about(e, input, policy))
        .cloned()
        .collect();
    sort_events(&mut events);
    events
}

/// Decides what the pipeline does with digest `input.digest` (§8.4). Rules apply in order; the
/// first that returns a mode decides.
pub fn plan(input: &PlanInput, policy: &Policy) -> Decision {
    let deploy = policy.deploy_or_default();
    let now = input.context.now;
    let events = events_about(input, policy);

    let mut out = decision(input, Mode::Blocked, String::new());
    out.deployed = newest_deployment(&input.deployed, &input.scope, policy, &input.ci_only);

    // Evidence for every required check (§8.2).
    let required = policy.required();
    let evaluated: Vec<(CheckName, evidence::Evaluated<'_>)> = required
        .iter()
        .map(|check| {
            (
                check.clone(),
                evidence::evaluate(check, &events, policy, &input.ci_only, now),
            )
        })
        .collect();
    out.requirements = evaluated
        .iter()
        .map(|(check, e)| RequirementState {
            requirement: check.to_string(),
            status: e.status,
            event: e.event.map(|e| e.key.clone()),
        })
        .collect();

    // P-8: the wrong branch, which nothing overrides.
    if let Some(reason) = wrong_branch(&deploy, input.context.branch.as_deref()) {
        out.mode = Mode::Blocked;
        out.reason = reason;
        return out;
    }

    // P-9: forcing builds and runs every required check.
    if input.context.force_full {
        if !deploy.allow_force {
            out.mode = Mode::Blocked;
            out.reason =
                "this repository doesn't allow forcing (`deploy.allow_force: false`)".into();
            return out;
        }
        out.mode = Mode::Full;
        out.build = true;
        out.checks_to_run = required
            .iter()
            .map(|check| CheckToRun::new(check.clone(), RunReason::Forced))
            .collect();
        out.reason = "forced: building and running every required check".into();
        return out;
    }

    // P-10: on an automatic first attempt, a digest the repository already runs is current.
    if deploy.dedupe.contains(&input.context.trigger)
        && input.context.attempt == 1
        && let Some(deployed) = &out.deployed
        && deployed.digest == input.digest
    {
        out.mode = Mode::Current;
        out.reason = format!(
            "this repository already runs {} (deployed at {}); to deploy it again, re-run it or start it by hand",
            short(&input.digest),
            deployed.time
        );
        return out;
    }

    // The pipeline's attempts (P-5, P-6).
    let attempts = attempts(&events, &input.liveness, policy, &input.ci_only, now);

    // Required checks without valid evidence (P-14, P-15).
    out.checks_to_run = evaluated
        .iter()
        .filter_map(|(check, e)| {
            let reason = match e.status {
                Status::Satisfied => return None,
                Status::Missing => RunReason::Missing,
                Status::Expired => RunReason::Expired,
                Status::Failed => RunReason::Failed,
            };
            Some(CheckToRun::new(check.clone(), reason))
        })
        .collect();

    // P-13: a reusable artifact for every artifact of the scope.
    match artifacts::choose(&events, input, &attempts, policy) {
        Ok(chosen) => {
            out.artifacts = chosen;
            // P-14.
            let reuse = describe_artifacts(&out.artifacts);
            if out.checks_to_run.is_empty() {
                out.mode = Mode::Promote;
                out.reason = format!("every required check is met{reuse}");
            } else {
                out.mode = Mode::Refresh;
                out.reason = format!(
                    "{}{reuse}; running {} first",
                    if out.artifacts.is_empty() {
                        "nothing to build".to_string()
                    } else {
                        "the artifacts are reusable".to_string()
                    },
                    checks_text(&out.checks_to_run)
                );
            }
        }
        Err(missing_artifact) => {
            // P-15.
            out.mode = Mode::Full;
            out.build = true;
            let recorded = events.iter().any(|e| {
                e.event
                    .artifact
                    .as_ref()
                    .is_some_and(|a| a.name == missing_artifact)
            });
            let why = if recorded {
                "recorded ones aren't reusable: none is pinned at its location by a signer that counts, from an attempt that passed"
            } else {
                "none was recorded"
            };
            out.reason = format!("no reusable `{missing_artifact}` ({why}): building");
            if !out.checks_to_run.is_empty() {
                let _ = write!(
                    out.reason,
                    " and running {}",
                    checks_text(&out.checks_to_run)
                );
            }
        }
    }
    out
}

/// Whether an event is about this repository, scope and digest, and canonicalization.
fn about(event: &VerifiedEvent, input: &PlanInput, policy: &Policy) -> bool {
    event.digest == input.digest
        && event.event.scope == input.scope
        && event.event.repository == policy.repository
        && event.event.canonicalization == crate::CANONICALIZATION
}

fn decision(input: &PlanInput, mode: Mode, reason: String) -> Decision {
    Decision {
        scope: input.scope.clone(),
        digest: input.digest,
        mode,
        reason,
        build: false,
        artifacts: BTreeMap::new(),
        checks_to_run: Vec::new(),
        requirements: Vec::new(),
        deployed: None,
    }
}

/// `sha256:` and the first 12 hex digits, for reasons people read.
fn short(digest: &crate::digest::Digest) -> String {
    format!("sha256:{}", &digest.hex()[..12])
}

fn describe_artifacts(artifacts: &BTreeMap<crate::names::ArtifactName, ChosenArtifact>) -> String {
    if artifacts.is_empty() {
        return String::new();
    }
    let list: Vec<String> = artifacts
        .iter()
        .map(|(name, a)| format!("{name} {}", a.reference))
        .collect();
    format!(": deploying {} by digest", list.join(", "))
}

fn checks_text(checks: &[CheckToRun]) -> String {
    checks
        .iter()
        .map(|c| {
            let why = match c.reason {
                RunReason::Missing => "missing",
                RunReason::Expired => "expired",
                RunReason::Failed => "failed",
                RunReason::Forced => "forced",
            };
            format!("{} ({why})", c.check)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests;
