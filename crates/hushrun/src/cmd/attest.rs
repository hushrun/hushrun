//! `hushrun attest`: records evidence that other tools produced (§10.4).
//!
//! \[ADR 0032\] A check event that hushrun's server refuses for the org's plan isn't recorded:
//! attest says so once, records the rest, and exits 0.

use std::fmt::Write as _;

use anyhow::{Context as _, anyhow};
use serde_json::json;

use hushrun_core::event::{
    ArtifactRecord, CheckRecord, DigestSet, Environment, EventKind, EvidenceKind, Origin, Outcome,
    Report, pinned_digest,
};
use hushrun_core::names::CheckName;
use hushrun_core::time::Dur;

use crate::app::App;
use crate::cli::AttestArgs;
use crate::cmd::common::{artifact_ref, base_event, pair};
use crate::context::{Content, Source};
use crate::events::{self, Recorded, Unrecorded};
use crate::exit::{CmdResult, Exit, Failure};
use crate::lifecycle::Recorder;
use crate::output::{self, Format, Outputs};
use crate::planfile::PlanFile;

pub fn run(app: &App<'_>, args: &AttestArgs) -> CmdResult {
    if args.checks.is_empty() && args.artifacts.is_empty() {
        return Err(Failure::usage(anyhow!(
            "nothing to attest: pass --check NAME=pass|fail or --artifact NAME=REF"
        )));
    }
    if !args.reports.is_empty() && args.checks.is_empty() {
        return Err(Failure::usage(anyhow!(
            "--report belongs to the --check results it accompanies"
        )));
    }
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
    let signing = app.signing()?;
    let producer = app.producer()?;
    let run = app.run_info_for(plan.as_ref())?;
    let forced = plan.as_ref().is_some_and(|p| p.forced);
    let now = app.now;

    let mut reports = Vec::new();
    for report in &args.reports {
        let (name, file) = pair(report, "--report")?;
        let bytes = std::fs::read(file)
            .with_context(|| format!("reading report {file}"))
            .map_err(Failure::usage)?;
        reports.push(Report {
            name: name.to_string(),
            digest: DigestSet::of(&hushrun_core::Digest::of(&bytes)),
        });
    }
    let duration = args.duration.unwrap_or(Dur::ZERO);
    let attempts = args.attempts.unwrap_or(1);
    if attempts == 0 {
        return Err(Failure::usage(anyhow!("--attempts is at least 1")));
    }

    let mut planned = Vec::new();
    for entry in &args.checks {
        let (name, result) = pair(entry, "--check")?;
        let name = CheckName::new(name).map_err(Failure::usage)?;
        let Some(def) = policy.checks.get(&name) else {
            return Err(Failure::usage(anyhow!("unknown check `{name}`")));
        };
        let check_kind = EvidenceKind::Source;
        let result = match result {
            "pass" => Outcome::Pass,
            "fail" => Outcome::Fail,
            other => {
                return Err(Failure::usage(anyhow!(
                    "--check {name}={other}: expected pass or fail"
                )));
            }
        };
        let environment = match (&def.image, &args.environment_image) {
            (Some(pinned), None) => {
                return Err(Failure::usage(anyhow!(
                    "check `{name}` pins image {pinned}: pass --environment-image with the image the check ran in"
                )));
            }
            (pinned, Some(image)) => {
                if pinned_digest(image).is_none() {
                    return Err(Failure::usage(anyhow!(
                        "--environment-image {image} is not pinned by digest"
                    )));
                }
                if let Some(pinned) = pinned
                    && pinned_digest(pinned) != pinned_digest(image)
                {
                    tracing::warn!(
                        "check `{name}` pins {pinned}, but ran in {image}: this evidence won't count for it"
                    );
                }
                Some(Environment {
                    image: Some(image.clone()),
                    platform: def.platform.clone(),
                    network: None,
                    runner: None,
                })
            }
            (None, None) => None,
        };
        let environment_digest = environment.as_ref().map(|e| {
            hushrun_core::jcs::digest(&serde_json::to_value(e).expect("serializes"))
                .expect("strings")
        });
        let mut event = base_event(EventKind::Check, policy, &subject, &run, &producer, now);
        event.check = Some(CheckRecord {
            name,
            check_kind,
            definition_digest: def.definition_digest,
            environment,
            environment_digest,
            result,
            attempts,
            duration_ms: duration.millis(),
            started_at: now.checked_sub(duration).unwrap_or(now),
            finished_at: now,
            exit_code: None,
            command: None,
            reports: (!reports.is_empty()).then(|| reports.clone()),
        });
        planned.push(event);
    }

    let mut refs = Vec::new();
    // [M18] A ledger that keeps a log checks every write against the log key hushrun.yaml
    // pins, which reading the signers sets (L-11, L-12).
    let needs_signers =
        plan.as_ref().is_some_and(|p| run.attempt > p.run.attempt) || ledger.log().is_some();
    let signers = if needs_signers {
        Some(app.signers(policy, ledger.as_ref())?)
    } else {
        None
    };
    // Carrying out a plan in a newer attempt of the same run (a re-run of failed jobs): the
    // first write starts the attempt (§10.4).
    let mut written = Vec::new();
    if let (Some(plan), Some(signers)) = (&plan, &signers)
        && run.attempt > plan.run.attempt
    {
        let recorder = Recorder {
            ledger: ledger.as_ref(),
            signers,
            policy,
            signing: &signing,
            sigstore: signing.root(),
            producer: &producer,
            run: &run,
            now,
            forced,
        };
        let read = recorder.read(&subject)?;
        written.extend(recorder.ensure_started(&subject, &read, None, false, None)?);
    }
    if !args.artifacts.is_empty() {
        for entry in &args.artifacts {
            let (name, reference) = pair(entry, "--artifact")?;
            let artifact = artifact_ref(policy, &subject, name, reference, &args.registry)?;
            let def = &policy.artifacts[&artifact.name];
            let mut event = base_event(EventKind::Artifact, policy, &subject, &run, &producer, now);
            event.artifact = Some(ArtifactRecord {
                name: artifact.name.clone(),
                artifact_type: def.artifact_type,
                reference: artifact.reference.clone(),
                digest: artifact.digest.clone(),
                origin: Origin::Built,
                copied_from: None,
            });
            refs.push(artifact);
            planned.push(event);
        }
    }

    for event in &mut planned {
        if forced {
            event.forced = Some(true);
        }
    }
    let ci = crate::ci::Ci::of(app);
    let mut unrecorded = Unrecorded::default();
    // The checks the server refused for the org's plan (ADR 0032).
    let mut refused: Vec<CheckName> = Vec::new();
    for event in &planned {
        match events::record(ledger.as_ref(), &signing, event, &subject.digest)? {
            Recorded::Keys(keys) => written.push((event.kind, keys)),
            Recorded::OverQuota(quota) => {
                unrecorded.say(&ci, &quota);
                refused.extend(event.check.as_ref().map(|c| c.name.clone()));
            }
        }
    }

    let mut outputs = Outputs::new();
    outputs.set("digest", subject.digest.to_string());
    if let Some(rev) = &subject.source.revision {
        outputs.set("revision", rev);
    }
    if let [only] = refs.as_slice() {
        outputs.set("ref", &only.reference);
    }
    for r in &refs {
        outputs.set(
            format!("ref_{}", r.name.env_key().to_lowercase()),
            &r.reference,
        );
    }
    if let Some(path) = &app.global.dotenv {
        outputs.write_dotenv(path)?;
    }
    let mut summary = format!("### hushrun attest: `{}`\n\n", subject.scope);
    for (kind, keys) in &written {
        summary.push_str(&format!("- recorded `{kind}`: `{}`\n", keys[0]));
    }
    for name in &refused {
        summary.push_str(&format!("- not recorded `check`: `{name}`\n"));
    }
    ci.report(&outputs, &summary)?;
    match app.global.format {
        Format::Text => {
            let mut text = String::new();
            for (kind, keys) in &written {
                let _ = writeln!(text, "recorded {kind}: {}", keys[0]);
            }
            for name in &refused {
                let _ = writeln!(text, "not recorded check: {name}");
            }
            output::print(&text)?;
        }
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.attest/v2",
            "scope": subject.scope,
            "digest": subject.digest,
            "events": written.iter().map(|(kind, keys)| json!({"kind": kind, "keys": keys})).collect::<Vec<_>>(),
            "refs": refs.iter().map(|r| (r.name.to_string(), r.reference.clone())).collect::<std::collections::BTreeMap<_, _>>(),
        }))?,
        Format::Env => output::print(&outputs.to_env())?,
        Format::Dotenv => output::print(&outputs.to_dotenv()?)?,
    }
    Ok(Exit::Ok)
}
