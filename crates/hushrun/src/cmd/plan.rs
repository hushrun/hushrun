//! `hushrun plan`: decides what the repository's pipeline does with each scope's digest (§8,
//! §10.4). \[ADR 0062\] One pipeline: no stage, no upstream to wait for.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::anyhow;
use serde_json::json;

use hushrun_core::digest::Digest;
use hushrun_core::event::{ListingRecord, RunInfo, VerifiedEvent};
use hushrun_core::names::ScopeName;
use hushrun_core::plan::{
    CheckToRun, Decision, Mode, PlanContext, PlanInput, RequirementState, RunReason, Status, plan,
    records_attempt,
};
use hushrun_core::policy::{DeployDef, OnLedgerError, Policy};
use hushrun_core::run::Trigger;
use hushrun_core::signers::Signers;
use hushrun_ledger::layout::policy_key;
use hushrun_ledger::{Ledger, LedgerError, RecordedListing};
use hushrun_vcs::digest_scopes;

use crate::app::{App, ledger_failure};
use crate::cli::PlanArgs;
use crate::context::{Content, Source, Subject};
use crate::events::EventStore;
use crate::exit::{CmdResult, Exit, Failure};
use crate::lifecycle::Recorder;
use crate::output::{self, Format, Outputs};
use crate::planfile::{PLAN_SCHEMA, PlanFile};

/// \[M18\] The most keys a `started` event records for one listing page (E-5); a larger page
/// is recorded by its signed statement alone, which still commits to its keys.
const MAX_RECORDED_KEYS: usize = 1000;
/// \[M18\] The most bytes of listings one `started` event records, well inside E-8's 1 MiB.
const MAX_RECORDED_BYTES: usize = 512 << 10;

/// A plan and what made it.
pub(crate) struct Made {
    pub content: Content,
    pub plan: PlanFile,
    pub inputs: Vec<PlanInput>,
    pub ledger_url: hushrun_core::ledger_url::LedgerUrl,
    pub ledger: Box<dyn hushrun_ledger::Ledger>,
    pub signers: Option<Signers>,
    /// The Sigstore trust root the plan read with (T-12).
    pub sigstore: hushrun_attest::TrustedRoot,
    pub subjects: Vec<Subject>,
    /// \[M18\] The signed listings the decision was read from, on a ledger that keeps a log.
    pub listings: Option<Vec<RecordedListing>>,
}

/// Plans: reads the ledger and decides every selected scope. Records nothing. `refuse_retired`
/// refuses a hushrun.yaml with keys this release reads past (ADR 0062): a deploy that ignored a
/// file's `stages` would skip the gates its author wrote.
pub(crate) fn make(app: &App<'_>, args: &PlanArgs, refuse_retired: bool) -> Result<Made, Failure> {
    let snapshot = crate::cli::snapshot(args.rev.as_deref(), args.worktree);
    let content = Content::open(app.global, Source::Repo(snapshot))?;
    let policy = content.policy()?;
    if refuse_retired && !policy.retired.is_empty() {
        let keys: Vec<String> = policy
            .retired
            .iter()
            .map(|r| format!("`{}`", r.key))
            .collect();
        return Err(Failure::usage(anyhow!(
            "{} holds {}, which hushrun 0.9 doesn't read: a deploy that ignored them could skip what they asked for. Move what a stage required to `deploy`, and delete them: https://hushrun.com/docs/reference/configuration#retired",
            policy.file,
            keys.join(", ")
        )));
    }
    let deploy = policy.deploy_or_default();
    if let Some(repository) = &app.run.repository
        && !repository.eq_ignore_ascii_case(&policy.repository)
    {
        tracing::warn!(
            "this run is in {repository}, but hushrun.yaml's repository is {}; events record hushrun.yaml's",
            policy.repository
        );
    }
    let selected = content.select(&args.scopes, false)?;
    let digests = digest_scopes(
        content.vcs.as_ref(),
        &content.listing,
        &content.scopes,
        &selected,
        false,
    )?;
    let subjects: Vec<Subject> = digests
        .scopes
        .iter()
        .map(|s| Subject {
            scope: s.scope.clone(),
            digest: s.digest,
            source: content.source_info(),
        })
        .collect();

    let trigger = args.trigger.or(app.run.trigger).unwrap_or(Trigger::Manual);
    let (ledger_url, ledger) = app.ledger_for_plan(Some(policy))?;
    let mut planner = Planner {
        app,
        args,
        policy,
        deploy: &deploy,
        subjects: &subjects,
        trigger,
        ledger: ledger.as_ref(),
        signers: None,
        sigstore: app.sigstore_root()?,
        run: None,
        listings: None,
    };
    let (decisions, inputs) = match planner.decide() {
        Ok(result) => result,
        Err(failure)
            if failure.exit == Exit::Ledger && deploy.on_ledger_error == OnLedgerError::Full =>
        {
            tracing::warn!("{:#}", failure.error);
            (
                ledger_error_full(
                    policy,
                    &deploy,
                    &subjects,
                    &failure,
                    app.run.branch.as_deref(),
                ),
                Vec::new(),
            )
        }
        Err(failure) => return Err(failure),
    };
    let run = planner.run.clone().unwrap_or_else(|| planner.run_info(&[]));
    let signers = planner.signers.take();
    let sigstore = planner.sigstore.clone();
    let listings = planner.listings.take();
    drop(planner);
    let revision = content.listing.revision.clone();
    let plan = PlanFile {
        schema: PLAN_SCHEMA.to_string(),
        repository: policy.repository.clone(),
        revision: revision.revision.clone(),
        reference: revision.reference.clone(),
        run,
        forced: args.force_full,
        time: app.now,
        scopes: decisions,
    };
    Ok(Made {
        content,
        plan,
        inputs,
        ledger_url,
        ledger,
        signers,
        sigstore,
        subjects,
        listings,
    })
}

pub fn run(app: &App<'_>, args: &PlanArgs) -> CmdResult {
    let Made {
        content,
        plan: plan_file,
        inputs,
        ledger_url,
        ledger,
        signers,
        sigstore,
        subjects,
        listings,
    } = make(app, args, true)?;
    let policy = content.policy()?;
    let run = plan_file.run.clone();

    // --start: `started` for every scope that deploys.
    let mut started_keys = Vec::new();
    if args.start {
        let deploying: Vec<&Decision> = plan_file
            .scopes
            .iter()
            .filter(|d| d.mode.deploys())
            .collect();
        if !deploying.is_empty() {
            let signers = signers.as_ref().ok_or_else(|| {
                Failure::new(
                    Exit::Ledger,
                    anyhow!(
                        "hushrun can't read the repository's events, so `started` can't be recorded"
                    ),
                )
            })?;
            let signing = app.signing_with(&sigstore)?;
            let producer = app.producer()?;
            upload_policy(ledger.as_ref(), policy);
            let recorder = Recorder {
                ledger: ledger.as_ref(),
                signers,
                policy,
                signing: &signing,
                sigstore: &sigstore,
                producer: &producer,
                run: &run,
                now: app.now,
                forced: args.force_full,
            };
            for decision in deploying {
                let subject = subjects
                    .iter()
                    .find(|s| s.scope == decision.scope)
                    .expect("planned scope");
                let read = crate::events::Read::default();
                let recorded = listings
                    .as_deref()
                    .map(|all| listings_of(&decision.scope, all));
                for (_, keys) in
                    recorder.ensure_started(subject, &read, args.lease, true, recorded)?
                {
                    started_keys.extend(keys);
                }
            }
        }
    }

    let json = serde_json::to_string(&plan_file).map_err(anyhow::Error::from)?;
    if let Some(path) = &args.out {
        std::fs::write(path, &json).map_err(|e| anyhow!("writing {}: {e}", path.display()))?;
    }
    if let Some(path) = &args.explain_json {
        let dump = json!({
            "schema": "hushrun.plan.explain/v2",
            "ledger": ledger_url.to_string(),
            "policy": policy.text,
            "signers": signers.as_ref().map(Signers::to_json),
            "inputs": inputs,
            "decisions": plan_file.scopes,
        });
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&dump).map_err(anyhow::Error::from)?,
        )
        .map_err(|e| anyhow!("writing {}: {e}", path.display()))?;
    }

    let outputs = outputs(&plan_file, &json);
    if let Some(path) = &app.global.dotenv {
        outputs.write_dotenv(path)?;
    }
    let ci = crate::ci::Ci::of(app);
    ci.report(
        &outputs,
        &summary(&plan_file, app.run.triggered_by.as_ref()),
    )?;
    for decision in &plan_file.scopes {
        if decision.mode == Mode::Blocked {
            let title = annotation_title(&plan_file, decision);
            ci.annotate(hushrun_ci::github::Level::Error, &title, &decision.reason);
        }
    }
    match app.global.format {
        Format::Text => output::print(&text(&plan_file, &started_keys))?,
        Format::Json => output::print(&json)?,
        Format::Env => output::print(&outputs.to_env())?,
        Format::Dotenv => output::print(&outputs.to_dotenv()?)?,
    }
    Ok(
        if plan_file.scopes.iter().any(|d| d.mode == Mode::Blocked) {
            Exit::Blocked
        } else {
            Exit::Ok
        },
    )
}

/// The `hushrun.yaml` bytes this plan evaluated, at `v1/policies/<sha256 hex>.yaml` (L-2), for
/// readers that show what the pipeline required, such as the index; the hex is the
/// `policyDigest` of the events they produce (E-4). Readers that decide ignore it, so the upload
/// never fails the plan: a key that exists is a success, anything else a warning, and the
/// `started` that follows reports an outage itself.
fn upload_policy(ledger: &dyn Ledger, policy: &Policy) {
    let bytes = policy.text.as_bytes();
    if Digest::of(bytes) != policy.digest {
        tracing::warn!("hushrun.yaml's text doesn't hash to its digest, so it isn't uploaded");
        return;
    }
    let key = policy_key(&policy.digest);
    match ledger.append(&key, bytes) {
        Ok(()) | Err(LedgerError::AlreadyExists(_)) => tracing::debug!("{key} is on the server"),
        Err(err) => tracing::warn!(
            "hushrun.yaml wasn't uploaded to {key}: {err}; the events still name its digest"
        ),
    }
}

/// \[M18\] The listings of `scope` a plan read (its events and its deployed index), as its
/// `started` records them (E-5): each page's signed statement, with its keys unless there are
/// too many.
fn listings_of(scope: &ScopeName, all: &[RecordedListing]) -> Vec<ListingRecord> {
    let events = format!("v1/events/{scope}/");
    let deployed = format!("v1/deployed/{scope}/");
    let mut records: Vec<ListingRecord> = all
        .iter()
        .filter(|l| l.prefix.starts_with(&events) || l.prefix.starts_with(&deployed))
        .map(|l| ListingRecord {
            statement: l.statement.clone(),
            keys: (l.keys.len() <= MAX_RECORDED_KEYS).then(|| l.keys.clone()),
        })
        .collect();
    let size: usize = records
        .iter()
        .map(|r| r.statement.len() + r.keys.iter().flatten().map(|k| k.len() + 3).sum::<usize>())
        .sum();
    if size > MAX_RECORDED_BYTES {
        for record in &mut records {
            record.keys = None;
        }
    }
    records
}

struct Planner<'a> {
    app: &'a App<'a>,
    args: &'a PlanArgs,
    policy: &'a Policy,
    deploy: &'a DeployDef,
    subjects: &'a [Subject],
    trigger: Trigger,
    ledger: &'a dyn hushrun_ledger::Ledger,
    /// \[ADR 0031\] Who counts (T-15), once read.
    signers: Option<Signers>,
    sigstore: hushrun_attest::TrustedRoot,
    run: Option<RunInfo>,
    /// \[M18\] The signed listings the decision read (L-11).
    listings: Option<Vec<RecordedListing>>,
}

impl Planner<'_> {
    /// Reads the ledger and decides every scope. Fails closed when the ledger can't be read
    /// (L-5).
    fn decide(&mut self) -> Result<(Vec<Decision>, Vec<PlanInput>), Failure> {
        if self.signers.is_none() {
            self.signers = Some(self.app.signers(self.policy, self.ledger)?);
        }
        // Only this decision's listings count (E-5).
        if let Some(log) = self.ledger.log() {
            log.take_listings();
        }
        let signers = self.signers.as_ref().expect("loaded");
        let store = EventStore {
            ledger: self.ledger,
            signers,
            sigstore: &self.sigstore,
            repository: &self.policy.repository,
            now: self.app.now,
        };
        let mut reads = Vec::new();
        for subject in self.subjects {
            let events = store
                .read(&subject.scope, &subject.digest)
                .map_err(ledger_failure)?;
            let deployed = store.deployed(&subject.scope).map_err(ledger_failure)?;
            reads.push((events, deployed));
        }
        let all: Vec<VerifiedEvent> = reads
            .iter()
            .flat_map(|(e, _)| e.accepted.iter().cloned())
            .collect();
        let run = self.run_info(&all);
        let mut decisions = Vec::new();
        let mut inputs = Vec::new();
        for (subject, (events, deployed)) in self.subjects.iter().zip(reads) {
            let input = PlanInput {
                scope: subject.scope.clone(),
                digest: subject.digest,
                context: PlanContext {
                    branch: self.app.run.branch.clone(),
                    trigger: self.trigger,
                    attempt: run.attempt,
                    force_full: self.args.force_full,
                    run: run.run_ref(),
                    now: self.app.now,
                },
                events: events.accepted,
                deployed: deployed.accepted,
                liveness: Vec::new(),
                ci_only: signers.ci_only.clone(),
            };
            decisions.push(plan(&input, self.policy));
            inputs.push(input);
        }
        self.listings = self.ledger.log().map(|log| log.take_listings());
        if self.app.run.branch.is_none() && self.deploy.branch.is_some() {
            tracing::warn!(
                "the branch of this run is unknown, so `deploy.branch` wasn't checked; `hushrun guard` checks it before deploying"
            );
        }
        self.run = Some(run);
        Ok((decisions, inputs))
    }

    /// This run's identity (§10.4). The attempt: `--attempt`, the provider's, else one more than
    /// the highest attempt this run recorded (P-5: its lifecycle events, from signers that
    /// count), else 1.
    fn run_info(&self, events: &[VerifiedEvent]) -> RunInfo {
        if let Some(run) = &self.run {
            return run.clone();
        }
        let provider = self.app.run.provider;
        let id = self
            .app
            .run
            .run_id
            .clone()
            .unwrap_or_else(|| hushrun_ledger::new_ulid(self.app.now.millis()));
        let ci_only = self
            .signers
            .as_ref()
            .map(|s| s.ci_only.clone())
            .unwrap_or_default();
        let recorded = events
            .iter()
            .filter(|e| {
                e.event.run.provider == provider
                    && e.event.run.id == id
                    && records_attempt(e, &ci_only)
            })
            .map(|e| e.event.run.attempt)
            .max();
        let attempt = self
            .args
            .attempt
            .or(self.app.run.attempt)
            .or(recorded.map(|a| a + 1))
            .unwrap_or(1);
        RunInfo {
            provider,
            id,
            attempt,
            url: self.app.run.url.clone(),
            trigger: self.trigger,
            job: self.app.run.job.clone(),
            job_id: self.app.run.job_id.clone(),
        }
    }
}

/// L-5: with `on_repository_error: full`, an unreadable ledger means building and running every
/// required check, on a branch the repository deploys from: P-8 needs no events, and nothing
/// overrides it.
fn ledger_error_full(
    policy: &Policy,
    deploy: &DeployDef,
    subjects: &[Subject],
    failure: &Failure,
    branch: Option<&str>,
) -> Vec<Decision> {
    let wrong_branch = hushrun_core::plan::wrong_branch(deploy, branch);
    let required = policy.required();
    subjects
        .iter()
        .map(|subject| Decision {
            scope: subject.scope.clone(),
            digest: subject.digest,
            mode: if wrong_branch.is_some() {
                Mode::Blocked
            } else {
                Mode::Full
            },
            reason: wrong_branch.clone().unwrap_or_else(|| format!(
                "hushrun can't read the repository's events ({:#}); `on_repository_error: full` builds and runs every check",
                failure.error
            )),
            build: wrong_branch.is_none(),
            artifacts: BTreeMap::new(),
            checks_to_run: required
                .iter()
                .filter(|_| wrong_branch.is_none())
                .map(|check| CheckToRun::new(check.clone(), RunReason::Missing))
                .collect(),
            requirements: required
                .iter()
                .map(|check| RequirementState {
                    requirement: check.to_string(),
                    status: Status::Missing,
                    event: None,
                })
                .collect(),
            deployed: None,
        })
        .collect()
}

/// Output keys (§10.2).
fn outputs(plan: &PlanFile, json: &str) -> Outputs {
    let mut out = Outputs::new();
    match plan.scopes.as_slice() {
        [decision] => {
            out.set("mode", decision.mode.as_str());
            out.set("reason", &decision.reason);
            out.set("digest", decision.digest.to_string());
            out.set("revision", plan.revision.clone().unwrap_or_default());
            out.set("attempt", plan.run.attempt.to_string());
            out.set("build", decision.build.to_string());
            let reference = match decision.artifacts.values().collect::<Vec<_>>().as_slice() {
                [only] => only.reference.clone(),
                _ => String::new(),
            };
            out.set("ref", reference);
            let artifacts: BTreeMap<String, String> = decision
                .artifacts
                .iter()
                .map(|(n, a)| (n.to_string(), a.reference.clone()))
                .collect();
            out.set(
                "artifacts",
                serde_json::to_string(&artifacts).expect("json"),
            );
            out.set("checks", checks(&decision.checks_to_run));
        }
        _ => {
            out.set("revision", plan.revision.clone().unwrap_or_default());
            out.set("attempt", plan.run.attempt.to_string());
            let actionable: Vec<_> = plan
                .scopes
                .iter()
                .filter(|d| d.mode.deploys())
                .map(|d| json!({"scope": d.scope, "mode": d.mode, "checks": checks(&d.checks_to_run), "digest": d.digest}))
                .collect();
            out.set(
                "actionable_scopes",
                serde_json::to_string(&actionable).expect("json"),
            );
        }
    }
    out.set("json", json);
    out
}

fn checks(list: &[CheckToRun]) -> String {
    list.iter()
        .map(|c| c.check.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the plan's only scope is `default`, a repository that declares none: then its output
/// names no scope, which read as "by default".
fn only_default(plan: &PlanFile) -> bool {
    matches!(plan.scopes.as_slice(), [only] if only.scope == ScopeName::default_scope())
}

/// The title of a decision's annotation on GitHub Actions.
fn annotation_title(plan: &PlanFile, decision: &Decision) -> String {
    if only_default(plan) {
        "hushrun plan".to_string()
    } else {
        format!("hushrun plan: {}", decision.scope)
    }
}

fn text(plan: &PlanFile, started: &[String]) -> String {
    let mut out = String::new();
    let unnamed = only_default(plan);
    for d in &plan.scopes {
        let _ = if unnamed {
            writeln!(out, "{}: {}", d.mode, d.reason)
        } else {
            writeln!(out, "{} {}: {}", d.scope, d.mode, d.reason)
        };
        if !d.checks_to_run.is_empty() {
            let _ = writeln!(out, "  checks to run: {}", checks(&d.checks_to_run));
        }
        for (name, artifact) in &d.artifacts {
            let _ = writeln!(out, "  {name}: {}", artifact.reference);
        }
        for r in &d.requirements {
            let status = serde_json::to_value(r.status).expect("status");
            let _ = writeln!(
                out,
                "  requirement {:<20} {}",
                r.requirement,
                status.as_str().unwrap_or("")
            );
        }
    }
    for key in started {
        let _ = writeln!(out, "recorded started: {key}");
    }
    out
}

/// The step summary. A plan of the one scope `default` leads with what this run does, then a
/// row with its mode; others have one row per scope.
fn summary(plan: &PlanFile, triggered_by: Option<&hushrun_ci::TriggeringRun>) -> String {
    let mut text = "### hushrun plan\n\n".to_string();
    if only_default(plan) {
        let d = &plan.scopes[0];
        let _ = writeln!(text, "{}\n", outcome(d.mode));
        text.push_str("| Mode | Reason |\n|---|---|\n");
        let _ = writeln!(text, "| **{}** | {} |", d.mode, crate::ci::cell(&d.reason));
    } else {
        text.push_str("| Scope | Mode | Reason |\n|---|---|---|\n");
        for d in &plan.scopes {
            let _ = writeln!(
                text,
                "| {} | **{}** | {} |",
                d.scope,
                d.mode,
                crate::ci::cell(&d.reason)
            );
        }
    }
    if let Some(run) = triggered_by {
        let _ = writeln!(
            text,
            "\nTriggered by {} run [{}]({}){}.",
            run.workflow.as_deref().unwrap_or("an upstream"),
            run.id,
            run.url.as_deref().unwrap_or(""),
            run.conclusion
                .as_deref()
                .map(|c| format!(", which ended `{c}`"))
                .unwrap_or_default()
        );
    }
    text
}

/// What a run in `mode` does, for the step summary's first line.
fn outcome(mode: Mode) -> &'static str {
    match mode {
        Mode::Promote => {
            "**Deploying** the image an earlier run built and checked, by digest. Nothing is rebuilt or checked again."
        }
        Mode::Refresh => {
            "**Deploying** the image an earlier run built, after running the checks below first."
        }
        Mode::Full => "**Building**, running the checks below, then deploying.",
        Mode::Current => "**Nothing to deploy.** This repository already runs this code.",
        Mode::Blocked => "**Not deployed.** The job fails on purpose; the reason says what to do.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_of(scopes: &[(&str, &str)]) -> PlanFile {
        let decisions: Vec<serde_json::Value> = scopes
            .iter()
            .map(|(scope, mode)| {
                json!({
                    "scope": scope, "digest": format!("sha256:{}", "0".repeat(64)), "mode": mode,
                    "reason": "why", "build": false, "artifacts": {}, "checksToRun": [],
                    "requirements": [],
                })
            })
            .collect();
        serde_json::from_value(json!({
            "schema": PLAN_SCHEMA, "repository": "github.com/acme/shop",
            "run": {"provider": "local", "id": "r", "attempt": 1, "trigger": "push"},
            "forced": false, "time": "2026-11-02T12:00:00.000Z", "scopes": decisions,
        }))
        .unwrap()
    }

    #[test]
    fn only_a_plan_of_the_default_scope_leaves_its_name_out() {
        let default = plan_of(&[("default", "current")]);
        assert_eq!(text(&default, &[]), "current: why\n");
        assert_eq!(
            summary(&default, None),
            "### hushrun plan\n\n**Nothing to deploy.** This repository already runs this code.\n\n| Mode | Reason |\n|---|---|\n| **current** | why |\n"
        );
        assert_eq!(
            annotation_title(&default, &default.scopes[0]),
            "hushrun plan"
        );
        // A named scope, or several, keep their names and the table of scopes.
        let named = plan_of(&[("api", "current")]);
        assert_eq!(text(&named, &[]), "api current: why\n");
        assert_eq!(
            annotation_title(&named, &named.scopes[0]),
            "hushrun plan: api"
        );
        assert_eq!(
            summary(&named, None),
            "### hushrun plan\n\n| Scope | Mode | Reason |\n|---|---|---|\n| api | **current** | why |\n"
        );
        let several = plan_of(&[("api", "promote"), ("web", "refresh")]);
        assert_eq!(text(&several, &[]), "api promote: why\nweb refresh: why\n");
        assert_eq!(
            summary(&several, None),
            "### hushrun plan\n\n| Scope | Mode | Reason |\n|---|---|---|\n| api | **promote** | why |\n| web | **refresh** | why |\n"
        );
        // A plan file from hushrun 0.6.0 or earlier says `skip` for `current`.
        assert_eq!(
            plan_of(&[("default", "skip")]).scopes[0].mode,
            Mode::Current
        );
    }
}
