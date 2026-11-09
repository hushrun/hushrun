//! Artifacts (P-13): which recorded artifact each declared artifact of the scope reuses.

use std::collections::BTreeMap;

use crate::event::{SupersededReason, VerifiedEvent, pinned_digest, reference_repository};
use crate::names::ArtifactName;
use crate::plan::attempts::{Attempt, AttemptState};
use crate::plan::{ChosenArtifact, PlanInput, counts_for};
use crate::policy::Policy;

/// Chooses an artifact event for every artifact of the scope, or reports the first that has
/// none. `input_events` are already this digest's, scope's and repository's (`plan` filters
/// them); `attempts` are the pipeline's.
pub(crate) fn choose(
    input_events: &[VerifiedEvent],
    input: &PlanInput,
    attempts: &[Attempt<'_>],
    policy: &Policy,
) -> Result<BTreeMap<ArtifactName, ChosenArtifact>, ArtifactName> {
    let mut chosen = BTreeMap::new();
    for (name, def) in policy.artifacts_of(&input.scope) {
        let candidates: Vec<&VerifiedEvent> = input_events
            .iter()
            .filter(|e| {
                let Some(artifact) = &e.event.artifact else {
                    return false;
                };
                if &artifact.name != name || !counts_for(&input.ci_only, e) {
                    return false;
                }
                // Pinned by digest, to the recorded digest.
                let Some(pin) = pinned_digest(&artifact.reference) else {
                    return false;
                };
                if Some(pin) != artifact.digest.digest() {
                    return false;
                }
                // Recorded by a passed or stale-superseded attempt of the pipeline.
                let reusable = attempts.iter().any(|a| {
                    a.run == e.event.run.run_ref()
                        && matches!(
                            a.state,
                            AttemptState::Passed
                                | AttemptState::Superseded(Some(SupersededReason::Stale))
                        )
                });
                // At the artifact's location.
                reusable && reference_repository(&artifact.reference) == def.location
            })
            .collect();
        let Some(event) = candidates.last().copied() else {
            return Err(name.clone());
        };
        let artifact = event
            .event
            .artifact
            .as_ref()
            .expect("candidates are artifact events");
        chosen.insert(
            name.clone(),
            ChosenArtifact {
                reference: artifact.reference.clone(),
                digest: artifact.digest.digest().expect("checked above"),
                event: event.key.clone(),
            },
        );
    }
    Ok(chosen)
}
