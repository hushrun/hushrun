//! `hushrun guard`: refuses to deploy a revision that is no longer the head of the branch it
//! deploys from (§10.4).

use anyhow::anyhow;
use serde_json::json;

use hushrun_core::event::{EventKind, Superseded, SupersededReason};

use crate::app::App;
use crate::cli::{GuardArgs, OnStale};
use crate::context::{Content, Source, Subject};
use crate::exit::{CmdResult, Exit, Failure};
use crate::lifecycle::Recorder;
use crate::output::{self, Format, Outputs};
use crate::planfile::PlanFile;

pub fn run(app: &App<'_>, args: &GuardArgs) -> CmdResult {
    let plan = PlanFile::load(app)?;
    let snapshot = args.content.snapshot();
    let content = Content::open(app.global, Source::Repo(snapshot))?;
    let policy = content.policy()?;
    if let Some(plan) = &plan {
        plan.check(&content)?;
    }
    let branch = args
        .branch
        .clone()
        .or_else(|| policy.deploy.as_ref().and_then(|d| d.branch.clone()))
        .or_else(|| app.run.branch.clone())
        .ok_or_else(|| {
            Failure::usage(anyhow!(
                "guard needs a branch: pass --branch, or set `deploy.branch` in hushrun.yaml"
            ))
        })?;
    let revision = content
        .listing
        .revision
        .revision
        .clone()
        .ok_or_else(|| Failure::usage(anyhow!("guard needs a VCS revision")))?;
    // The head can't be resolved: exit 1, record nothing.
    let head = match content.vcs.remote_head(&args.remote, &branch) {
        Ok(Some(head)) => head,
        Ok(None) => {
            return Err(Failure::new(
                Exit::Unexpected,
                anyhow!("{} has no branch {branch}; nothing recorded", args.remote),
            ));
        }
        Err(err) => {
            return Err(Failure::new(
                Exit::Unexpected,
                anyhow!(
                    "could not resolve the head of {branch} on {}: {err}; nothing recorded",
                    args.remote
                ),
            ));
        }
    };
    let stale = head != revision;
    let mut recorded = Vec::new();
    let mut notices = Vec::new();
    if stale {
        let subjects: Vec<Subject> = match &plan {
            Some(plan) => plan
                .scopes
                .iter()
                .filter(|d| d.mode.deploys())
                .map(|d| Subject {
                    scope: d.scope.clone(),
                    digest: d.digest,
                    source: content.source_info(),
                })
                .collect(),
            None => vec![content.subject(&args.content.scopes, args.content.digest)?],
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
        for subject in &subjects {
            let result = recorder.record(subject, EventKind::Superseded, |event, _| {
                event.superseded = Some(Superseded {
                    reason: SupersededReason::Stale,
                    by_revision: Some(head.clone()),
                });
                Ok(())
            })?;
            if let Some(reason) = result.ignored {
                notices.push(format!(
                    "{}: not recording `superseded`: {reason}",
                    subject.scope
                ));
            }
            recorded.extend(result.written.into_iter().flat_map(|(_, keys)| keys));
        }
    }

    let mut outputs = Outputs::new();
    outputs.set("stale", stale.to_string());
    outputs.set("head", &head);
    if let Some(path) = &app.global.dotenv {
        outputs.write_dotenv(path)?;
    }
    let ci = crate::ci::Ci::of(app);
    let summary = if stale {
        format!(
            "### hushrun guard: `{branch}`\n\nStale: `{branch}` moved on to `{head}`; this run records `superseded` and skips its deploy steps.\n"
        )
    } else {
        format!("### hushrun guard: `{branch}`\n\nThis revision is still the head of `{branch}`.\n")
    };
    ci.report(&outputs, &summary)?;
    if stale {
        ci.annotate(
            hushrun_ci::github::Level::Warning,
            &format!("hushrun guard: {branch}"),
            &format!("{branch} moved on to {head}; skipping the deploy"),
        );
    }
    for notice in &notices {
        eprintln!("notice: {notice}");
    }
    match app.global.format {
        Format::Text => {
            let text = if stale {
                format!(
                    "stale: {revision} is no longer the head of {branch} ({head} is); recorded superseded{}\n",
                    if recorded.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", recorded.join(", "))
                    }
                )
            } else {
                format!("{revision} is the head of {branch}\n")
            };
            output::print(&text)?;
        }
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.guard/v1",
            "stale": stale,
            "revision": revision,
            "head": head,
            "branch": branch,
            "recorded": recorded,
        }))?,
        Format::Env => output::print(&outputs.to_env())?,
        Format::Dotenv => output::print(&outputs.to_dotenv()?)?,
    }
    Ok(match (stale, args.on_stale) {
        (true, OnStale::Fail) => Exit::Stale,
        _ => Exit::Ok,
    })
}
