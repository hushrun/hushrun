//! `hushrun mark`: records a lifecycle event (§10.4).
//!
//! Before a lifecycle event of an attempt that has no `started` event, `mark` writes one
//! (P-5). Terminal events P-6 would ignore are not recorded; a notice says so.

use std::fmt::Write as _;

use anyhow::anyhow;
use serde_json::json;

use hushrun_core::event::{
    ArtifactRef, Deployed, DigestSet, EventKind, Failed, Passed, RunInfo, Superseded,
    SupersededReason, Target, VerifiedEvent,
};
use hushrun_core::plan::counts_for;
use hushrun_core::policy::Policy;

use crate::app::App;
use crate::cli::{MarkArgs, MarkKind};
use crate::cmd::common::{artifact_ref, pair};
use crate::context::{Content, Source, Subject};
use crate::exit::{CmdResult, Exit, Failure};
use crate::lifecycle::Recorder;
use crate::output::{self, Format, Outputs};
use crate::planfile::PlanFile;

pub fn run(app: &App<'_>, args: &MarkArgs) -> CmdResult {
    let kind = match args.kind {
        MarkKind::Started => EventKind::Started,
        MarkKind::Deployed => EventKind::Deployed,
        MarkKind::Passed => EventKind::Passed,
        MarkKind::Failed => EventKind::Failed,
        MarkKind::Superseded => EventKind::Superseded,
    };
    check_flags(args, kind)?;
    let plan = PlanFile::load(app)?;
    let snapshot = args.content.snapshot();
    let content = Content::open(app.global, Source::Repo(snapshot))?;
    let policy = content.policy()?;
    let subject = match &plan {
        Some(plan) => {
            plan.check(&content)?;
            plan.subject(args.content.scopes.scopes.first(), &content)?
        }
        None => content.subject(&args.content.scopes, args.content.digest)?,
    };
    let (_, ledger) = app.ledger(Some(policy))?;
    let signers = app.signers(policy, ledger.as_ref())?;
    let signing = app.signing()?;
    let producer = app.producer()?;
    let run = app.run_info_for(plan.as_ref())?;
    let recorder = Recorder {
        ledger: ledger.as_ref(),
        signers: &signers,
        policy,
        signing: &signing,
        sigstore: signing.root(),
        producer: &producer,
        run: &run,
        now: app.now,
        forced: plan.as_ref().is_some_and(|p| p.forced),
    };

    let written = if kind == EventKind::Started {
        let read = recorder.read(&subject)?;
        recorder.ensure_started(&subject, &read, None, true, None)?
    } else {
        let recorded = recorder.record(&subject, kind, |event, read| {
            // [M18] On an HTTP ledger a deploy follows `plan --start`, whose `started` records the
            // signed listings the plan used, for monitors to check (L-11).
            if kind == EventKind::Deployed
                && recorder.ledger.log().is_some()
                && !recorder.planned(read)
            {
                if recorder.planned_without_counting(read) {
                    return Err(Failure::usage(anyhow!(
                        "this run attempt's `started` doesn't count: the repository takes CI's evidence only, so a deploy is CI's to record"
                    )));
                }
                return Err(Failure::usage(anyhow!(
                    "this run attempt has no `started` from `plan --start`, and deploying needs one: it records the listings the plan decided from, which monitors check. Run `hushrun plan --start` first, as the recipes do"
                )));
            }
            match kind {
                EventKind::Deployed | EventKind::Passed => {
                    let artifacts = artifacts_for(
                        args,
                        policy,
                        &subject,
                        &read.accepted,
                        &signers.ci_only,
                        &run,
                        plan.as_ref(),
                    )?;
                    if kind == EventKind::Deployed {
                        let target = args.target.as_deref().map(|t| match t.split_once('=') {
                            Some((name, detail)) => Target {
                                name: name.to_string(),
                                detail: Some(detail.to_string()),
                            },
                            None => Target {
                                name: t.to_string(),
                                detail: None,
                            },
                        });
                        event.deployed = Some(Deployed {
                            artifacts,
                            target,
                        });
                    } else {
                        event.passed = Some(Passed { artifacts });
                    }
                }
                EventKind::Failed => {
                    event.failed = Some(Failed {
                        reason: args.reason.clone().unwrap_or_else(|| "failed".into()),
                        checks: None,
                    });
                }
                EventKind::Superseded => {
                    let reason = match args.reason.as_deref() {
                        Some("stale") => SupersededReason::Stale,
                        Some("cancelled") | None => SupersededReason::Cancelled,
                        Some(other) => {
                            return Err(Failure::usage(anyhow!(
                                "--reason {other:?}: superseded takes stale or cancelled"
                            )));
                        }
                    };
                    event.superseded = Some(Superseded {
                        reason,
                        by_revision: args.by_revision.clone(),
                    });
                }
                _ => unreachable!("mark records lifecycle events"),
            }
            Ok(())
        })?;
        if let Some(reason) = recorded.ignored {
            eprintln!("notice: not recording `{kind}`: {reason}");
            return Ok(Exit::Ok);
        }
        recorded.written
    };

    let mut outputs = Outputs::new();
    outputs.set("digest", subject.digest.to_string());
    outputs.set("attempt", run.attempt.to_string());
    if let Some(path) = &app.global.dotenv {
        outputs.write_dotenv(path)?;
    }
    match app.global.format {
        Format::Text => {
            let mut text = String::new();
            for (kind, keys) in &written {
                let _ = writeln!(text, "recorded {kind}: {}", keys[0]);
            }
            output::print(&text)?;
        }
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.mark/v2",
            "scope": subject.scope,
            "digest": subject.digest,
            "run": run,
            "events": written.iter().map(|(kind, keys)| json!({"kind": kind, "keys": keys})).collect::<Vec<_>>(),
        }))?,
        Format::Env => output::print(&outputs.to_env())?,
        Format::Dotenv => output::print(&outputs.to_dotenv()?)?,
    }
    Ok(Exit::Ok)
}

fn check_flags(args: &MarkArgs, kind: EventKind) -> Result<(), Failure> {
    let only = |flag: bool, name: &str, kinds: &[EventKind]| {
        if flag && !kinds.contains(&kind) {
            Err(Failure::usage(anyhow!(
                "{name} doesn't apply to `mark {kind}`"
            )))
        } else {
            Ok(())
        }
    };
    only(
        !args.artifacts.is_empty(),
        "--artifact",
        &[EventKind::Deployed, EventKind::Passed],
    )?;
    only(args.target.is_some(), "--target", &[EventKind::Deployed])?;
    only(
        args.by_revision.is_some(),
        "--by-revision",
        &[EventKind::Superseded],
    )?;
    only(
        args.reason.is_some(),
        "--reason",
        &[EventKind::Failed, EventKind::Superseded],
    )
}

/// The artifacts of a `deployed` or `passed` event, in order (§10.4): `--artifact`, the
/// artifact events this run recorded, the plan's artifacts. Every artifact of the scope must
/// be present (C-6).
#[allow(clippy::too_many_arguments)]
fn artifacts_for(
    args: &MarkArgs,
    policy: &Policy,
    subject: &Subject,
    accepted: &[VerifiedEvent],
    ci_only: &hushrun_core::signers::CiOnly,
    run: &RunInfo,
    plan: Option<&PlanFile>,
) -> Result<Vec<ArtifactRef>, Failure> {
    let declared: Vec<_> = policy
        .artifacts_of(&subject.scope)
        .map(|(n, _)| n.clone())
        .collect();
    let mut found: Vec<ArtifactRef> = Vec::new();
    for entry in &args.artifacts {
        let (name, reference) = pair(entry, "--artifact")?;
        found.push(artifact_ref(
            policy,
            subject,
            name,
            reference,
            &args.registry,
        )?);
    }
    if found.is_empty() {
        // The newest artifact event this run recorded, per artifact (events are in E-9 order),
        // from signers that count (T-16): another signer can claim the run, but not choose what
        // it deployed.
        for event in accepted.iter().rev() {
            let Some(artifact) = &event.event.artifact else {
                continue;
            };
            if event.event.run.run_ref() != run.run_ref()
                || event.event.stage.is_some()
                || !counts_for(ci_only, event)
                || found.iter().any(|f| f.name == artifact.name)
            {
                continue;
            }
            found.push(ArtifactRef {
                name: artifact.name.clone(),
                reference: artifact.reference.clone(),
                digest: artifact.digest.clone(),
            });
        }
    }
    if found.is_empty()
        && let Some(decision) = plan.and_then(|p| p.decision(Some(&subject.scope)).ok())
    {
        found.extend(decision.artifacts.iter().map(|(name, a)| ArtifactRef {
            name: name.clone(),
            reference: a.reference.clone(),
            digest: DigestSet::of(&a.digest),
        }));
    }
    let missing: Vec<String> = declared
        .iter()
        .filter(|d| !found.iter().any(|f| &f.name == *d))
        .map(ToString::to_string)
        .collect();
    if !missing.is_empty() {
        return Err(Failure::usage(anyhow!(
            "scope `{}` declares artifact(s) {} that this run didn't record: pass --artifact NAME=REF",
            subject.scope,
            missing.join(", ")
        )));
    }
    found.sort();
    Ok(found)
}
