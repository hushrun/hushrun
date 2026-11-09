//! `hushrun repository ls | show | signers | gc | monitor | usage | stats`.

use std::fmt::Write as _;

use anyhow::Context as _;
use serde_json::json;

use hushrun_attest::{Expected, Location, Object, verify_with};
use hushrun_core::digest::Digest;
use hushrun_core::names::ScopeName;
use hushrun_core::plan::newest_deployment;
use hushrun_core::time::Dur;
use hushrun_ledger::gc;
use hushrun_ledger::layout::{self, parse_deployed_key, parse_event_key};
use hushrun_vcs::Snapshot;

use crate::app::{App, ledger_failure};
use crate::cli::RepositoryCommand;
use crate::context::{Content, Source};
use crate::events::EventStore;
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};

pub fn run(app: &App<'_>, command: &RepositoryCommand) -> CmdResult {
    match command {
        RepositoryCommand::Ls { scope, digest } => ls(app, scope.as_ref(), digest.as_ref()),
        RepositoryCommand::Show { key } => show(app, key),
        RepositoryCommand::Signers { out } => signers(app, out.as_deref()),
        RepositoryCommand::Gc {
            older_than,
            dry_run,
            key,
        } => collect_garbage(app, *older_than, *dry_run, key.as_deref()),
        RepositoryCommand::Monitor { state } => crate::cmd::monitor::run(app, state.as_deref()),
        RepositoryCommand::Usage => crate::cmd::usage::run(app),
        RepositoryCommand::Stats { since, local } => crate::cmd::stats::run(app, *since, *local),
    }
}

fn ls(app: &App<'_>, scope: Option<&ScopeName>, digest: Option<&Digest>) -> CmdResult {
    let content = Content::open(app.global, Source::Repo(Snapshot::default()))?;
    let (_, ledger) = app.ledger(content.policy.as_ref())?;
    let prefix = match (scope, digest) {
        (Some(scope), Some(digest)) => layout::events_prefix(scope, digest),
        (Some(scope), None) => layout::scope_events_prefix(scope),
        (None, _) => "v1/events/".to_string(),
    };
    let keys = ledger.list(&prefix).map_err(ledger_failure)?;
    let rows: Vec<_> = keys
        .iter()
        .filter_map(|k| parse_event_key(k).map(|m| (k, m)))
        .collect();
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.repository.ls/v1",
            "events": rows.iter().map(|(k, m)| json!({
                "key": k, "scope": m.scope, "digest": m.digest, "kind": m.kind, "stage": m.stage, "ulid": m.ulid,
            })).collect::<Vec<_>>(),
        }))?,
        _ => {
            let mut text = String::new();
            for (key, meta) in &rows {
                let _ = writeln!(text, "{:<10} {}", meta.kind.as_str(), key);
            }
            output::print(&text)?;
        }
    }
    Ok(Exit::Ok)
}

fn show(app: &App<'_>, key: &str) -> CmdResult {
    let content = Content::open(app.global, Source::Repo(Snapshot::default()))?;
    let policy = content.policy()?;
    let (_, ledger) = app.ledger(Some(policy))?;
    let bytes = ledger.get(key).map_err(ledger_failure)?;
    let object = Object::parse(&bytes).map_err(Failure::usage)?;
    let envelope = object.envelope().ok_or_else(|| {
        Failure::usage(anyhow::anyhow!("{key}: a bundle without a DSSE envelope"))
    })?;
    let statement: serde_json::Value =
        serde_json::from_slice(&envelope.payload_bytes().map_err(Failure::usage)?)
            .context("the payload is not JSON")?;
    // Verify as the planner would, against the key's location.
    let verdict = match app.signers_said(policy, ledger.as_ref()) {
        Ok(signers) => {
            let location_scope;
            let digest;
            let stage;
            let expected = if let Some(meta) = parse_event_key(key) {
                location_scope = meta.scope.clone();
                digest = meta.digest;
                stage = meta.stage.clone();
                Some(Expected {
                    repository: &policy.repository,
                    scope: &location_scope,
                    location: Location::Events {
                        digest: &digest,
                        kind: meta.kind,
                        stage: stage.as_ref(),
                    },
                })
            } else if let Some(meta) = parse_deployed_key(key) {
                location_scope = meta.scope.clone();
                stage = meta.stage.clone();
                Some(Expected {
                    repository: &policy.repository,
                    scope: &location_scope,
                    location: Location::Deployed {
                        stage: stage.as_ref(),
                    },
                })
            } else {
                None
            };
            match expected {
                Some(expected) => match app
                    .sigstore_root()
                    .map_err(|f| format!("{:#}", f.error))
                    .and_then(|root| {
                        verify_with(&bytes, &signers, &root, &expected, app.now)
                            .map_err(|r| r.to_string())
                    }) {
                    Ok(v) => format!(
                        "accepted: signed by {} with {}, {} (custody {})",
                        v.signer,
                        crate::cmd::verify::signer_text(&v),
                        crate::cmd::verify::timestamp_text(&v),
                        v.effective_custody
                    ),
                    Err(why) => why,
                },
                None => "not an event key".to_string(),
            }
        }
        Err(failure) => format!("not verified: {:#}", failure.error),
    };
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.repository.show/v1",
            "key": key,
            "verification": verdict,
            "signatures": envelope.signatures.iter().map(|s| &s.keyid).collect::<Vec<_>>(),
            "statement": statement,
        }))?,
        _ => output::print(&format!(
            "{key}\n{verdict}\n{}\n",
            serde_json::to_string_pretty(&statement).expect("json")
        ))?,
    }
    Ok(Exit::Ok)
}

/// \[ADR 0031\] `repository signers` (L-13): who counts for the repository, as its server names
/// them, with the server's statement, checked against the log key hushrun.yaml pins. `--out`
/// saves the answer for `hushrun verify --signers`.
fn signers(app: &App<'_>, out: Option<&std::path::Path>) -> CmdResult {
    let content = Content::open(app.global, Source::Repo(Snapshot::default()))?;
    let policy = content.policy()?;
    let (url, ledger) = app.ledger(Some(policy))?;
    crate::app::configure_log(ledger.as_ref(), policy)?;
    let answer = ledger.signers(&policy.repository).map_err(ledger_failure)?;
    let saved = serde_json::to_vec_pretty(&answer.to_json()).map_err(anyhow::Error::from)?;
    if let Some(path) = out {
        std::fs::write(path, &saved)
            .with_context(|| format!("writing {}", path.display()))
            .map_err(Failure::usage)?;
    }
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.repository.signers/v1",
            "repository": url.to_string(),
            "checked": answer.checked,
            "answer": answer.to_json(),
            "file": out.map(|p| p.display().to_string()),
        }))?,
        _ => {
            let mut text = String::new();
            let _ = writeln!(text, "{}: {}", url, answer.signers.summary());
            for ci in &answer.signers.ci {
                let _ = writeln!(text, "  ci       {ci}");
            }
            let periods = answer.signers.ci_only.any_periods();
            if !periods.is_empty() {
                let since = periods.last().filter(|p| p.is_open()).map(|p| p.from);
                let earlier: Vec<String> = periods
                    .iter()
                    .filter_map(|p| Some(format!("{} until {}", p.from, p.until?)))
                    .collect();
                let earlier = if earlier.is_empty() {
                    String::new()
                } else {
                    format!("events made from {}", earlier.join(", from "))
                };
                let _ = match since {
                    Some(since) if earlier.is_empty() => writeln!(
                        text,
                        "  ci-only  evidence from CI only, for events made since {since}"
                    ),
                    Some(since) => writeln!(
                        text,
                        "  ci-only  evidence from CI only, for events made since {since}, and {earlier}"
                    ),
                    None => writeln!(
                        text,
                        "  ci-only  not now; evidence from CI only for {earlier}"
                    ),
                };
            }
            for (keyid, machine) in &answer.signers.machines {
                let state = match machine.revoked {
                    Some(at) => format!("revoked {at}"),
                    None if machine.gc => "counts, may sign GC".to_string(),
                    None => "counts".to_string(),
                };
                let _ = writeln!(
                    text,
                    "  machine  {} of {} ({keyid}): {state}",
                    machine.name, machine.owner
                );
            }
            let _ = writeln!(
                text,
                "{}",
                if answer.checked {
                    "The server's statement verifies with the log key hushrun.yaml names."
                } else {
                    "hushrun.yaml names no log key, so the server's statement wasn't checked."
                }
            );
            if let Some(path) = out {
                let _ = writeln!(
                    text,
                    "Saved to {} for hushrun verify --signers.",
                    path.display()
                );
            }
            output::print(&text)?;
        }
    }
    Ok(Exit::Ok)
}

fn collect_garbage(
    app: &App<'_>,
    older_than: Dur,
    dry_run: bool,
    signer: Option<&std::path::Path>,
) -> CmdResult {
    let content = Content::open(app.global, Source::Repo(Snapshot::default()))?;
    let policy = content.policy()?;
    let (url, ledger) = app.ledger(Some(policy))?;
    let signers = app.signers(policy, ledger.as_ref())?;
    let sigstore = app.sigstore_root()?;
    let gc_signer = match (ledger.log(), dry_run) {
        (Some(log), false) => gc_signer(app, signer, log.verifying(), &sigstore)?,
        _ => None,
    };
    let cutoff = app.now.checked_sub(older_than).ok_or_else(|| {
        Failure::usage(anyhow::anyhow!(
            "--older-than {older_than} reaches before 1970"
        ))
    })?;

    // What the planner would read as each segment's newest deployment (L-8, P-10): the newest
    // verified entry, and the newest one accepted for the segment's events. Both stay. Every
    // segment the index has is read, a stage's or `_`, whatever hushrun.yaml names.
    let deployed_keys = ledger.list("v1/deployed/").map_err(ledger_failure)?;
    let store = EventStore {
        ledger: ledger.as_ref(),
        signers: &signers,
        sigstore: &sigstore,
        repository: &policy.repository,
        now: app.now,
    };
    let mut keep = gc::Keep::default();
    let segments = gc::deployed_segments(&deployed_keys);
    for (scope, stage) in &segments {
        let read = store
            .deployed_at(scope, stage.as_ref())
            .map_err(ledger_failure)?;
        // The planner's own choice, ties and all (P-10), and the newest entry whoever signed it.
        // \[ADR 0062\] Under a stage's prefix, which hushrun 0.8 and earlier wrote, the planner
        // chooses nothing, and the newest entry stays.
        let planned = newest_deployment(&read.accepted, scope, policy, &signers.ci_only)
            .map(|d| (d.event, d.digest));
        let newest = gc::newest(read.accepted.iter());
        for (key, digest) in [newest, planned].into_iter().flatten() {
            keep.deployed.insert(key);
            keep.digests.insert((scope.clone(), digest));
        }
    }

    let mut keys = ledger.list("v1/events/").map_err(ledger_failure)?;
    keys.extend(deployed_keys);
    let delete = gc::select(&keys, &keep, cutoff.millis());
    let mut deleted = 0usize;
    if !dry_run {
        for key in &delete {
            let deleted_one = match (&gc_signer, ledger.log()) {
                (Some(signer), Some(log)) => {
                    let statement =
                        signer.statement(app, &url.to_string(), key, &signers, &sigstore)?;
                    log.delete_signed(key, &statement)
                }
                _ => ledger.delete(key),
            };
            deleted_one.map_err(|e| {
                let failure = ledger_failure(e);
                Failure::new(
                    failure.exit,
                    anyhow::anyhow!(
                        "{:#} (after deleting {deleted} of {}; running gc again continues)",
                        failure.error,
                        delete.len()
                    ),
                )
            })?;
            deleted += 1;
        }
    }

    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.repository.gc/v1",
            "ledger": url.to_string(),
            "olderThan": older_than.to_string(),
            "cutoff": cutoff,
            "dryRun": dry_run,
            "kept": {
                "deployed": keep.deployed,
                "digests": keep.digests.iter().map(|(scope, digest)| json!({"scope": scope, "digest": digest})).collect::<Vec<_>>(),
            },
            "deleted": delete,
        }))?,
        _ => {
            let mut text = String::new();
            for key in &delete {
                let _ = writeln!(
                    text,
                    "{}{key}",
                    if dry_run { "would delete " } else { "deleted " }
                );
            }
            let _ = writeln!(
                text,
                "{} {} object(s) written before {cutoff}; kept the newest deployment of {} scope/segment pair(s) and {} digest(s)",
                if dry_run { "would delete" } else { "deleted" },
                delete.len(),
                segments.len(),
                keep.digests.len()
            );
            output::print(&text)?;
        }
    }
    Ok(Exit::Ok)
}

/// Who signs a GC run's GC statements (L-9): \[ADR 0031\] a machine of an admin or an owner of
/// the org, which the signers mark `gc`.
struct GcSigner(crate::signing::Signing);

impl GcSigner {
    /// The GC statement for deleting `key`, checked against the signers before the delete goes
    /// out, so that a machine that may not sign GC deletes nothing.
    fn statement(
        &self,
        app: &App<'_>,
        ledger: &str,
        key: &str,
        signers: &hushrun_core::signers::Signers,
        sigstore: &hushrun_attest::TrustedRoot,
    ) -> Result<Vec<u8>, Failure> {
        let bytes = self.0.sign_gc(ledger, key)?;
        hushrun_attest::gc::verify_gc(&bytes, signers, sigstore, app.now).map_err(|why| {
            Failure::usage(anyhow::anyhow!(
                "{} can't sign GC statements: {why}. An admin's or an owner's signed-in machine signs them",
                self.0.signer().describe()
            ))
        })?;
        Ok(bytes)
    }
}

/// The signer of a GC run's deletions on a server that keeps a log (L-9, L-11): `--key`, else the
/// command's signer (T-12), by default this machine's key, which must be an admin's machine.
/// On a server whose log key hushrun.yaml doesn't name, deletions without a signer go unsigned,
/// and monitors report them.
fn gc_signer(
    app: &App<'_>,
    path: Option<&std::path::Path>,
    verifying: bool,
    sigstore: &hushrun_attest::TrustedRoot,
) -> Result<Option<GcSigner>, Failure> {
    let key_file = match path {
        Some(path) => {
            let pem = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))
                .map_err(Failure::usage)?;
            Some(hushrun_attest::SigningKey::from_pem(&pem).map_err(Failure::usage)?)
        }
        None => None,
    };
    let signing = match key_file {
        Some(key) => app.signing_by_key(key, sigstore.clone()),
        None => app.signing_with(sigstore),
    };
    match signing {
        Ok(signing) => Ok(Some(GcSigner(signing))),
        Err(failure) if verifying => Err(failure),
        Err(failure) => {
            eprintln!(
                "warning: {:#}; deletions go unsigned, and monitors of this repository's log will report each one",
                failure.error
            );
            Ok(None)
        }
    }
}
