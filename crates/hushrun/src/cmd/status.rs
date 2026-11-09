//! `hushrun status`: the required checks and their evidence for the current digest, and the
//! digest the repository's pipeline deployed last, per scope.

use std::fmt::Write as _;

use serde_json::json;

use hushrun_core::plan::{PlanContext, PlanInput, plan};
use hushrun_core::run::{RunRef, Trigger};
use hushrun_vcs::digest_scopes;

use crate::app::{App, ledger_failure};
use crate::cli::StatusArgs;
use crate::context::{Content, Source};
use crate::events::EventStore;
use crate::exit::{CmdResult, Exit};
use crate::output::{self, Format};

pub fn run(app: &App<'_>, args: &StatusArgs) -> CmdResult {
    let snapshot = crate::cli::snapshot(args.rev.as_deref(), args.worktree);
    let content = Content::open(app.global, Source::Repo(snapshot))?;
    let policy = content.policy()?;
    let selected = content.select(&args.scopes, true)?;
    let digests = digest_scopes(
        content.vcs.as_ref(),
        &content.listing,
        &content.scopes,
        &selected,
        false,
    )?;
    let (_, ledger) = app.ledger_for_plan(Some(policy))?;
    let signers = app.signers(policy, ledger.as_ref())?;
    let sigstore = app.sigstore_root()?;
    let store = EventStore {
        ledger: ledger.as_ref(),
        signers: &signers,
        sigstore: &sigstore,
        repository: &policy.repository,
        now: app.now,
    };
    let branch = policy.deploy.as_ref().and_then(|d| d.branch.clone());
    let mut scopes = Vec::new();
    for scope in &digests.scopes {
        let events = store
            .read(&scope.scope, &scope.digest)
            .map_err(ledger_failure)?;
        let deployed = store.deployed(&scope.scope).map_err(ledger_failure)?;
        // As a manual first attempt on the branch it deploys from: what this digest has.
        let input = PlanInput {
            scope: scope.scope.clone(),
            digest: scope.digest,
            context: PlanContext {
                branch: branch.clone(),
                trigger: Trigger::Manual,
                attempt: 1,
                force_full: false,
                run: RunRef {
                    provider: hushrun_core::run::Provider::Local,
                    id: "status".into(),
                    attempt: 1,
                },
                now: app.now,
            },
            events: events.accepted.clone(),
            deployed: deployed.accepted,
            liveness: Vec::new(),
            ci_only: signers.ci_only.clone(),
        };
        let decision = plan(&input, policy);
        scopes.push(json!({
            "scope": scope.scope,
            "digest": scope.digest,
            "events": events.accepted.len(),
            "rejected": events.rejected.len(),
            "mode": decision.mode,
            "reason": decision.reason,
            "requirements": decision.requirements,
            "deployed": decision.deployed,
            "runsThisDigest": decision.deployed.as_ref().is_some_and(|d| d.digest == scope.digest),
        }));
    }
    let value = json!({"schema": "hushrun.status/v2", "revision": content.listing.revision.revision, "scopes": scopes});
    match app.global.format {
        Format::Json => output::print_json(&value)?,
        _ => {
            let mut text = String::new();
            for scope in value["scopes"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    text,
                    "{} {} ({} events, {} rejected)",
                    scope["scope"].as_str().unwrap_or(""),
                    scope["digest"].as_str().unwrap_or(""),
                    scope["events"],
                    scope["rejected"]
                );
                let deployed = match (&scope["deployed"], scope["runsThisDigest"].as_bool()) {
                    (serde_json::Value::Null, _) => "nothing deployed".to_string(),
                    (_, Some(true)) => "runs this digest".to_string(),
                    (d, _) => format!("runs {}", d["digest"].as_str().unwrap_or("?")),
                };
                let _ = writeln!(
                    text,
                    "  {:<8} {deployed}",
                    scope["mode"].as_str().unwrap_or("")
                );
                for r in scope["requirements"].as_array().into_iter().flatten() {
                    let _ = writeln!(
                        text,
                        "    {:<22} {}",
                        r["requirement"].as_str().unwrap_or(""),
                        r["status"].as_str().unwrap_or("")
                    );
                }
            }
            output::print(&text)?;
        }
    }
    Ok(Exit::Ok)
}
