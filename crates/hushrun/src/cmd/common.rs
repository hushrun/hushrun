//! Helpers the event-writing commands share.

use anyhow::anyhow;

use hushrun_core::event::{
    ArtifactRef, DigestSet, Event, EventKind, Producer, RunInfo, pinned_digest,
    reference_repository,
};
use hushrun_core::names::ArtifactName;
use hushrun_core::policy::Policy;
use hushrun_core::time::Time;

use crate::cli::RegistryOpts;
use crate::context::Subject;
use crate::exit::Failure;

/// Splits `NAME=VALUE`.
pub fn pair<'a>(text: &'a str, what: &str) -> Result<(&'a str, &'a str), Failure> {
    text.split_once('=')
        .filter(|(name, value)| !name.is_empty() && !value.is_empty())
        .ok_or_else(|| Failure::usage(anyhow!("{what} {text:?}: expected NAME=VALUE")))
}

/// An event with the common predicate fields (E-4) and no kind-specific object yet. \[ADR 0062\]
/// It has no stage.
pub fn base_event(
    kind: EventKind,
    policy: &Policy,
    subject: &Subject,
    run: &RunInfo,
    producer: &Producer,
    time: Time,
) -> Event {
    Event {
        kind,
        repository: policy.repository.clone(),
        scope: subject.scope.clone(),
        stage: None,
        canonicalization: hushrun_core::CANONICALIZATION.to_string(),
        source: subject.source.clone(),
        run: run.clone(),
        producer: producer.clone(),
        time,
        policy_digest: policy.digest,
        forced: None,
        checkpoint: None,
        started: None,
        check: None,
        artifact: None,
        deployed: None,
        passed: None,
        failed: None,
        superseded: None,
        approval: None,
    }
}

/// `reference`, pinned by digest: a tag is resolved through the registry (A-4). Returns the
/// reference unchanged when it is already pinned.
pub fn pin_reference(reference: &str, registry: &RegistryOpts) -> Result<String, Failure> {
    if pinned_digest(reference).is_some() {
        return Ok(reference.to_string());
    }
    let image = hushrun_artifact::ImageRef::parse(reference).map_err(Failure::usage)?;
    let artifacts = connect(registry)?;
    let digest = artifacts.digest(&image).map_err(transfer_failure)?;
    let pinned = image.repository.at_digest(&digest);
    tracing::info!("{reference} is {pinned}");
    Ok(pinned)
}

/// A registry client with the Docker configuration's credentials (§11.5).
pub fn connect(registry: &RegistryOpts) -> Result<hushrun_artifact::Artifacts, Failure> {
    hushrun_artifact::Artifacts::connect(&hushrun_artifact::Options {
        insecure_registries: registry.insecure.clone(),
    })
    .map_err(transfer_failure)
}

/// Registry failures exit 7 (§10.3); malformed references are usage errors.
pub fn transfer_failure(err: hushrun_artifact::ArtifactError) -> Failure {
    match err {
        hushrun_artifact::ArtifactError::Invalid(_) => Failure::usage(err),
        other => Failure::new(crate::exit::Exit::Transfer, other),
    }
}

/// Checks an `--artifact NAME=REF` of `attest` or `mark`: a declared artifact of the scope, at
/// its location (C-6). Only then is
/// a tag resolved through the registry (A-4), so hushrun never asks, or sends credentials to,
/// a registry the policy doesn't name.
pub fn artifact_ref(
    policy: &Policy,
    subject: &Subject,
    name: &str,
    reference: &str,
    registry: &RegistryOpts,
) -> Result<ArtifactRef, Failure> {
    let name = ArtifactName::new(name).map_err(Failure::usage)?;
    let Some(def) = policy.artifacts.get(&name) else {
        return Err(Failure::usage(anyhow!("unknown artifact `{name}`")));
    };
    if def.scope != subject.scope {
        return Err(Failure::usage(anyhow!(
            "artifact `{name}` belongs to scope `{}`, not `{}`",
            def.scope,
            subject.scope
        )));
    }
    let location = &def.location;
    if reference_repository(reference) != location {
        return Err(Failure::usage(anyhow!(
            "{reference} is not at artifact `{name}`'s location, {location}"
        )));
    }
    let reference = pin_reference(reference, registry)?;
    let digest = pinned_digest(&reference).expect("pinned");
    Ok(ArtifactRef {
        name,
        reference,
        digest: DigestSet::of(&digest),
    })
}
