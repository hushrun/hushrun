//! `hushrun artifact resolve | copy | digest` (§9, §10.4).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::anyhow;
use serde_json::json;

use hushrun_artifact::{ArtifactError, CopyReport, ImageRef, Repository, hints};
use hushrun_attest::{Expected, Location, verify_with};
use hushrun_core::digest::Digest;
use hushrun_core::event::{EventKind, pinned_digest, reference_repository};
use hushrun_core::names::ArtifactName;
use hushrun_core::plan::counts_for;
use hushrun_core::plan::{ChosenArtifact, Decision, Mode};
use hushrun_core::policy::Policy;
use hushrun_core::signers::Signers;
use hushrun_ledger::Ledger;
use hushrun_ledger::layout::parse_event_key;

use crate::app::App;
use crate::cli::{ArtifactCommand, PlanArgs, RegistryOpts, ResolveArgs, ScopeOpts};
use crate::cmd::common::{connect, transfer_failure};
use crate::cmd::plan;
use crate::context::{Content, Source};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format, Outputs};
use crate::planfile::PlanFile;

pub fn run(app: &App<'_>, command: &ArtifactCommand) -> CmdResult {
    match command {
        ArtifactCommand::Resolve(args) => resolve(app, args),
        ArtifactCommand::Copy {
            source,
            destination,
            registry,
        } => copy(app, source, destination, registry),
        ArtifactCommand::Digest {
            reference,
            registry,
        } => digest(app, reference, registry),
    }
}

/// `artifact digest REF`: the digest a tag points to (or confirms a pinned one exists).
fn digest(app: &App<'_>, reference: &str, registry: &RegistryOpts) -> CmdResult {
    let image = ImageRef::parse(reference).map_err(Failure::usage)?;
    let digest = connect(registry)?
        .digest(&image)
        .map_err(transfer_failure)?;
    let pinned = image.repository.at_digest(&digest);
    let mut outputs = Outputs::new();
    outputs.set("digest", digest.to_string());
    outputs.set("ref", &pinned);
    finish(
        app,
        &outputs,
        json!({"schema": "hushrun.artifact.digest/v1", "reference": reference, "digest": digest, "ref": pinned}),
        format!("{pinned}\n"),
    )
}

/// `artifact copy SRC DST[:tag]`: the low-level copy by digest (A-2). No events.
fn copy(app: &App<'_>, source: &str, destination: &str, registry: &RegistryOpts) -> CmdResult {
    let source = ImageRef::parse(source).map_err(Failure::usage)?;
    let destination = ImageRef::parse(destination).map_err(Failure::usage)?;
    if destination.digest.is_some() {
        return Err(Failure::usage(anyhow!(
            "the destination is a repository with an optional tag, not a digest"
        )));
    }
    let artifacts = connect(registry)?;
    let digest = match source.digest {
        Some(digest) => digest,
        None => artifacts.digest(&source).map_err(transfer_failure)?,
    };
    let pinned = ImageRef {
        digest: Some(digest),
        ..source.clone()
    };
    let report = artifacts
        .copy(&pinned, &destination.repository, destination.tag.as_deref())
        .map_err(|e| {
            match (
                &e,
                hints::permissions(&pinned.repository, &destination.repository),
            ) {
                (ArtifactError::Denied { .. }, Some(hint)) => {
                    Failure::new(Exit::Transfer, anyhow!("{e} ({hint})"))
                }
                _ => transfer_failure(e),
            }
        })?;
    let reference = destination.repository.at_digest(&digest);
    let mut outputs = Outputs::new();
    outputs.set("digest", digest.to_string());
    outputs.set("ref", &reference);
    finish(
        app,
        &outputs,
        json!({
            "schema": "hushrun.artifact.copy/v1",
            "source": pinned.repository.at_digest(&digest),
            "ref": reference,
            "digest": digest,
            "tag": destination.tag,
            "report": report_json(&report),
        }),
        format!("{reference}\n{}\n", summary(&report)),
    )
}

/// What `resolve` decided for one artifact.
#[derive(Debug)]
struct Resolved {
    reference: String,
    digest: Option<Digest>,
}

/// `artifact resolve` (A-1): the reference to deploy for the plan's decision. \[ADR 0062\] An
/// artifact has one location, where P-13 found the one it reuses: nothing is copied.
fn resolve(app: &App<'_>, args: &ResolveArgs) -> CmdResult {
    let plan = match PlanFile::load(app)? {
        Some(plan) => plan,
        None if app.run.in_ci => {
            return Err(Failure::usage(anyhow!(
                "in CI, artifact resolve carries out the plan job's decision: pass --plan or set HUSHRUN_PLAN_JSON"
            )));
        }
        None => {
            // Locally, decide now, without recording anything.
            let plan_args = PlanArgs {
                scopes: ScopeOpts {
                    scopes: args.scope.iter().cloned().collect(),
                    all_scopes: false,
                },
                rev: None,
                worktree: args.worktree,
                trigger: None,
                attempt: None,
                force_full: false,
                start: false,
                lease: None,
                out: None,
                explain_json: None,
            };
            plan::make(app, &plan_args, true)?.plan
        }
    };
    let snapshot = crate::cli::snapshot(None, args.worktree);
    let content = Content::open(app.global, Source::Repo(snapshot))?;
    let policy = content.policy()?;
    plan.check(&content)?;
    let decision = plan.decision(args.scope.as_ref())?;
    let tag = match (&args.tag, &plan.revision) {
        (Some(tag), _) => tag.clone(),
        (None, Some(revision)) => revision.clone(),
        (None, None) => {
            return Err(Failure::usage(anyhow!(
                "the plan has no revision to tag with: pass --tag"
            )));
        }
    };
    if !hushrun_artifact::valid_tag(&tag) {
        return Err(Failure::usage(anyhow!("{tag:?} is not a valid tag")));
    }
    let declared: Vec<ArtifactName> = policy
        .artifacts_of(&decision.scope)
        .map(|(name, _)| name.clone())
        .collect();
    let build_all = || -> Result<BTreeMap<ArtifactName, Resolved>, Failure> {
        declared
            .iter()
            .map(|name| {
                let location =
                    Repository::parse(&policy.artifacts[name].location).map_err(Failure::usage)?;
                Ok((
                    name.clone(),
                    Resolved {
                        reference: location.at_tag(&tag),
                        digest: None,
                    },
                ))
            })
            .collect()
    };

    let mut warnings = Vec::new();
    let (build, resolved) = match decision.mode {
        Mode::Current | Mode::Blocked => {
            return Err(Failure::usage(anyhow!(
                "the plan's decision for `{}` is {}: there is nothing to deploy, so the pipeline shouldn't reach artifact resolve",
                decision.scope,
                decision.mode
            )));
        }
        Mode::Full => (true, build_all()?),
        Mode::Promote | Mode::Refresh => {
            let (_, ledger) = app.ledger(Some(policy))?;
            let signers = app.signers(policy, ledger.as_ref())?;
            let sigstore = app.sigstore_root()?;
            let session = Session {
                app,
                policy,
                signers: &signers,
                sigstore: &sigstore,
                ledger: ledger.as_ref(),
                decision,
            };
            match session.reuse() {
                Ok(resolved) => (false, resolved),
                Err(message) => {
                    if args.no_fallback {
                        return Err(Failure::new(
                            Exit::Transfer,
                            anyhow!("{message}; --no-fallback"),
                        ));
                    }
                    warnings.push(format!("{message}; building instead"));
                    (true, build_all()?)
                }
            }
        }
    };
    for warning in &warnings {
        tracing::warn!("{warning}");
    }

    let mut outputs = Outputs::new();
    outputs.set("build", build.to_string());
    if let [(_, only)] = resolved.iter().collect::<Vec<_>>().as_slice() {
        outputs.set("ref", &only.reference);
        if let Some(digest) = &only.digest {
            outputs.set("digest", digest.to_string());
        }
    }
    for (name, r) in &resolved {
        outputs.set(
            format!("ref_{}", name.env_key().to_lowercase()),
            &r.reference,
        );
    }
    let mut text = String::new();
    for (name, r) in &resolved {
        let how = if build { "build" } else { "reuse" };
        let _ = writeln!(text, "{name}: {} ({how})", r.reference);
    }
    if resolved.is_empty() {
        let _ = writeln!(text, "no artifacts; build: {build}");
    }
    finish(
        app,
        &outputs,
        json!({
            "schema": "hushrun.artifact.resolve/v2",
            "scope": decision.scope,
            "mode": decision.mode,
            "build": build,
            "tag": tag,
            "artifacts": resolved.iter().map(|(name, r)| (name.to_string(), json!({
                "ref": r.reference, "digest": r.digest,
            }))).collect::<serde_json::Map<_, _>>(),
            "warnings": warnings,
        }),
        text,
    )
}

/// One `resolve` of reusable artifacts: each re-verified.
struct Session<'a> {
    app: &'a App<'a>,
    policy: &'a Policy,
    signers: &'a Signers,
    sigstore: &'a hushrun_attest::TrustedRoot,
    ledger: &'a dyn Ledger,
    decision: &'a Decision,
}

impl Session<'_> {
    fn reuse(&self) -> Result<BTreeMap<ArtifactName, Resolved>, String> {
        let mut resolved = BTreeMap::new();
        for (name, chosen) in &self.decision.artifacts {
            self.reverify(name, chosen)?;
            resolved.insert(
                name.clone(),
                Resolved {
                    reference: chosen.reference.clone(),
                    digest: Some(chosen.digest),
                },
            );
        }
        Ok(resolved)
    }

    /// Re-fetches and re-verifies the event that justified `chosen` (A-1): signed by a signer
    /// that counts (T-16), for this digest, pinned, and at the artifact's location (P-13).
    fn reverify(&self, name: &ArtifactName, chosen: &ChosenArtifact) -> Result<(), String> {
        let key = &chosen.event;
        let meta = parse_event_key(key)
            .filter(|m| m.kind == EventKind::Artifact)
            .ok_or_else(|| format!("{key} is not an artifact event key"))?;
        if meta.scope != self.decision.scope || meta.digest != self.decision.digest {
            return Err(format!("{key} is not about this scope and digest"));
        }
        let bytes = self
            .ledger
            .get(key)
            .map_err(|e| format!("re-reading {key}: {e}"))?;
        let expected = Expected {
            repository: &self.policy.repository,
            scope: &self.decision.scope,
            location: Location::Events {
                digest: &self.decision.digest,
                kind: EventKind::Artifact,
                stage: meta.stage.as_ref(),
            },
        };
        let verified = verify_with(&bytes, self.signers, self.sigstore, &expected, self.app.now)
            .map_err(|rejection| format!("{key} no longer verifies: {rejection}"))?;
        let artifact = verified
            .event
            .artifact
            .as_ref()
            .ok_or_else(|| format!("{key} has no artifact"))?;
        if &artifact.name != name
            || artifact.reference != chosen.reference
            || artifact.digest.digest() != Some(chosen.digest)
            || pinned_digest(&chosen.reference) != Some(chosen.digest)
        {
            return Err(format!(
                "{key} doesn't record {} for `{name}`",
                chosen.reference
            ));
        }
        let event = hushrun_core::event::VerifiedEvent {
            key: key.clone(),
            signer: verified.signer.clone(),
            effective_custody: verified.effective_custody,
            digest: verified.subject_digest,
            event: verified.event.clone(),
            trusted_time: verified.trusted_time,
        };
        if !counts_for(&self.signers.ci_only, &event) {
            return Err(format!(
                "{key} is signed by {}, and the repository takes evidence from its CI only, as its server says",
                verified.signer
            ));
        }
        let location = &self.policy.artifacts[name].location;
        if reference_repository(&chosen.reference) != location {
            return Err(format!(
                "{} is not at `{name}`'s location, {location}",
                chosen.reference
            ));
        }
        Ok(())
    }
}

fn report_json(report: &CopyReport) -> serde_json::Value {
    json!({
        "manifests": report.manifests,
        "blobsCopied": report.blobs_copied,
        "blobsMounted": report.blobs_mounted,
        "blobsPresent": report.blobs_present,
        "blobsForeign": report.blobs_foreign,
    })
}

fn summary(report: &CopyReport) -> String {
    format!(
        "{} manifest(s); blobs: {} copied, {} mounted, {} already there, {} non-distributable",
        report.manifests,
        report.blobs_copied,
        report.blobs_mounted,
        report.blobs_present,
        report.blobs_foreign
    )
}

fn finish(app: &App<'_>, outputs: &Outputs, json: serde_json::Value, text: String) -> CmdResult {
    if let Some(path) = &app.global.dotenv {
        outputs.write_dotenv(path)?;
    }
    let ci = crate::ci::Ci::of(app);
    let title = json["schema"]
        .as_str()
        .unwrap_or("hushrun artifact")
        .to_string();
    ci.report(
        outputs,
        &format!("### {title}\n\n```\n{}\n```\n", text.trim_end()),
    )?;
    for warning in json["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            ci.annotate(
                hushrun_ci::github::Level::Warning,
                "hushrun artifact resolve",
                warning,
            );
        }
    }
    match app.global.format {
        Format::Text => output::print(&text)?,
        Format::Json => output::print_json(&json)?,
        Format::Env => output::print(&outputs.to_env())?,
        Format::Dotenv => output::print(&outputs.to_dotenv()?)?,
    }
    Ok(Exit::Ok)
}
