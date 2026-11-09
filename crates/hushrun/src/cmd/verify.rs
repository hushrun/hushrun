//! `hushrun verify`: verifies envelopes and shows who signed them (E-8). Exits 5 when a given
//! file fails.
//!
//! \[ADR 0031\] An event counts when its signer is one the repository's server names (T-15): a
//! machine of a member or an agent of the org, or the repository's CI. `verify` asks the server,
//! or reads a saved answer (`--signers`), whose statement it checks against the log key
//! hushrun.yaml pins (L-13, T-9): the server is trusted for who is a member, and the log key
//! makes a saved answer checkable later. The answer also says when the repository took evidence
//! from CI only (T-16, set on the server by its admins): `verify` says when an accepted event
//! doesn't count for that reason, and that an approval, which hushrun 0.8 and earlier recorded,
//! counts for nothing (ADR 0062).

use std::fmt::Write as _;

use anyhow::{Context as _, anyhow};
use serde_json::json;

use hushrun_attest::{Expected, Location, Object, SignedBy, Verified, verify_with};
use hushrun_core::event::{Event, Statement};
use hushrun_core::names::ScopeName;
use hushrun_core::policy::Policy;
use hushrun_core::signers::CiOnly;
use hushrun_vcs::Snapshot;

use crate::app::{App, ledger_failure};
use crate::cli::VerifyArgs;
use crate::context::{Content, Source};
use crate::events::{EventStore, Signature};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};

struct Row {
    what: String,
    outcome: Result<Accepted, String>,
}

struct Accepted {
    signer: String,
    custody: String,
    kind: String,
    digest: String,
    /// How it was signed (E-8 step 2), for people and as JSON.
    signed_with: String,
    signer_json: serde_json::Value,
    trusted_time: Option<hushrun_core::time::Time>,
    /// Why it doesn't count, when it doesn't: T-16, it isn't the CI's and the repository was
    /// CI-only when it was made, or is now and it has no trusted time; or \[ADR 0062\] it is
    /// an approval, which counts nowhere since 0.9.
    not_counting: Option<&'static str>,
}

impl Accepted {
    fn new(
        signer: &str,
        custody: hushrun_core::run::Custody,
        event: &hushrun_core::event::Event,
        digest: &hushrun_core::digest::Digest,
        signature: &Signature,
        rules: (&Policy, &CiOnly),
    ) -> Accepted {
        let (kind, identity, machine) = match &signature.signed_by {
            SignedBy::Machine(name) => ("machine", None, Some(name.as_str())),
            SignedBy::Identity(identity) => ("identity", Some(identity.as_ref()), None),
        };
        let mut signer_json = json!({"kind": kind, "keyid": signature.keyid});
        if let Some(machine) = machine {
            signer_json["machine"] = json!(machine);
        }
        if let Some(identity) = identity {
            signer_json["identity"] = serde_json::to_value(identity).expect("serializes");
        }
        Accepted {
            signer: signer.to_string(),
            custody: custody.to_string(),
            kind: event.kind.to_string(),
            digest: digest.to_string(),
            signed_with: signer_text_of(signature),
            signer_json,
            trusted_time: signature.trusted_time,
            not_counting: not_counting(rules.1, signer, event, signature),
        }
    }
}

/// T-16: why `event` doesn't count, when it doesn't: while the repository was CI-only, as the
/// signers answer says, only its CI counts, and others with a trusted time outside every period.
/// An answer that still lists periods per stage is read as their union. \[ADR 0062\] Approvals
/// count nowhere.
fn not_counting(
    ci_only: &CiOnly,
    signer: &str,
    event: &hushrun_core::event::Event,
    signature: &Signature,
) -> Option<&'static str> {
    if event.kind == hushrun_core::event::EventKind::Approval {
        return Some("approvals count nowhere since hushrun 0.9");
    }
    (!ci_only.counts_anywhere(signer, signature.trusted_time))
        .then_some("CI only, as the repository's server says")
}

pub fn run(app: &App<'_>, args: &VerifyArgs) -> CmdResult {
    let content = Content::open(app.global, Source::Repo(Snapshot::default()))?;
    let policy = content.policy()?;
    let mut rows = Vec::new();
    let from_files = args.digest.is_none();
    let sigstore = app.sigstore_root()?;
    if from_files {
        if args.files.is_empty() {
            return Err(Failure::usage(anyhow!(
                "pass envelope files, or --digest and --scope to verify the repository's events"
            )));
        }
        let signers = match &args.signers {
            Some(path) => saved_signers(policy, path)?.signers,
            None => {
                let (_, ledger) = app.ledger(Some(policy))?;
                app.signers_said(policy, ledger.as_ref())?
            }
        };
        for file in &args.files {
            let bytes = std::fs::read(file)
                .with_context(|| format!("reading {}", file.display()))
                .map_err(Failure::usage)?;
            // A file has no storage key: take the scope from the statement, then check
            // everything else against it.
            let scope = claimed_scope(&bytes).unwrap_or_else(ScopeName::default_scope);
            let expected = Expected {
                repository: &policy.repository,
                scope: &scope,
                location: Location::File,
            };
            let outcome = verify_with(&bytes, &signers, &sigstore, &expected, app.now)
                .map(|v| {
                    let signature = Signature {
                        keyid: v.keyid.clone(),
                        signed_by: v.signed_by.clone(),
                        trusted_time: v.trusted_time,
                    };
                    Accepted::new(
                        v.signer.as_str(),
                        v.effective_custody,
                        &v.event,
                        &v.subject_digest,
                        &signature,
                        (policy, &signers.ci_only),
                    )
                })
                .map_err(|r| r.to_string());
            rows.push(Row {
                what: file.display().to_string(),
                outcome,
            });
        }
    } else {
        let (scope, digest) = (
            args.scope.as_ref().expect("clap requires --scope"),
            args.digest.expect("checked"),
        );
        let (_, ledger) = app.ledger(Some(policy))?;
        let signers = app.signers_said(policy, ledger.as_ref())?;
        let store = EventStore {
            ledger: ledger.as_ref(),
            signers: &signers,
            sigstore: &sigstore,
            repository: &policy.repository,
            now: app.now,
        };
        let read = store.read(scope, &digest).map_err(ledger_failure)?;
        for e in read.accepted {
            rows.push(Row {
                outcome: Ok(Accepted::new(
                    e.signer.as_str(),
                    e.effective_custody,
                    &e.event,
                    &e.digest,
                    &read.signatures[&e.key],
                    (policy, &signers.ci_only),
                )),
                what: e.key,
            });
        }
        for (key, why) in read.rejected {
            rows.push(Row {
                what: key,
                outcome: Err(why),
            });
        }
        rows.sort_by(|a, b| a.what.cmp(&b.what));
    }

    let failed = rows.iter().filter(|r| r.outcome.is_err()).count();
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.verify/v2",
            "results": rows.iter().map(|r| match &r.outcome {
                Ok(a) => json!({"source": r.what, "accepted": true, "signer": a.signer, "effectiveCustody": a.custody, "kind": a.kind, "digest": a.digest, "signature": a.signer_json, "trustedTime": a.trusted_time, "counts": a.not_counting.is_none(), "notCounting": a.not_counting}),
                Err(why) => json!({"source": r.what, "accepted": false, "reason": why}),
            }).collect::<Vec<_>>(),
        }))?,
        _ => {
            let mut text = String::new();
            for row in &rows {
                match &row.outcome {
                    Ok(a) => {
                        let _ = writeln!(
                            text,
                            "ok        {}: {} signed by {} (custody {}) for {}, with {}, {}",
                            row.what,
                            a.kind,
                            a.signer,
                            a.custody,
                            a.digest,
                            a.signed_with,
                            a.trusted_time
                                .map_or("no timestamp".to_string(), |t| format!(
                                    "timestamped at {t}"
                                )),
                        );
                        if let Some(why) = a.not_counting {
                            let _ = writeln!(text, "          doesn't count: {why}");
                        }
                    }
                    Err(why) => {
                        let _ = writeln!(text, "REJECTED  {}: {why}", row.what);
                    }
                }
            }
            if rows.is_empty() {
                text.push_str("no events\n");
            }
            output::print(&text)?;
        }
    }
    Ok(if from_files && failed > 0 {
        Exit::VerifyFailed
    } else {
        Exit::Ok
    })
}

/// What made the signature count, for people (E-8 step 2).
pub fn signer_text(verified: &Verified) -> String {
    signer_text_of(&Signature {
        keyid: verified.keyid.clone(),
        signed_by: verified.signed_by.clone(),
        trusted_time: verified.trusted_time,
    })
}

fn signer_text_of(signature: &Signature) -> String {
    match &signature.signed_by {
        SignedBy::Machine(name) => format!("machine {name} ({})", signature.keyid),
        SignedBy::Identity(identity) => {
            let mut text = format!("identity {}", identity.subject);
            if let Some(workflow) = &identity.workflow
                && Some(workflow) != identity.signer_workflow.as_ref()
            {
                text.push_str(&format!(" called from {workflow}"));
            }
            text
        }
    }
}

/// When the signature was timestamped, for people (E-10).
pub fn timestamp_text(verified: &Verified) -> String {
    match verified.trusted_time {
        Some(time) => format!("timestamped at {time}"),
        None => "no timestamp".to_string(),
    }
}

/// The scope a statement names, read without trusting it.
fn claimed_scope(bytes: &[u8]) -> Option<ScopeName> {
    let object = Object::parse(bytes).ok()?;
    let envelope = object.envelope()?;
    let statement: Statement = serde_json::from_slice(&envelope.payload_bytes().ok()?).ok()?;
    let event: Event = serde_json::from_value(statement.predicate).ok()?;
    Some(event.scope)
}

/// \[ADR 0031\] Signers `hushrun repository signers --out` saved (L-13): checked against the log key
/// hushrun.yaml pins when it names one, else taken as they are, with a warning.
pub(crate) fn saved_signers(
    policy: &hushrun_core::policy::Policy,
    path: &std::path::Path,
) -> Result<hushrun_ledger::SignersAnswer, Failure> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading {}", path.display()))
        .map_err(Failure::usage)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} isn't JSON", path.display()))
        .map_err(Failure::usage)?;
    let verifier = match &policy.transparency_log {
        Some(log) => Some(
            hushrun_tlog::note::Verifier::new(&log.origin, crate::app::log_key(log)?)
                .map_err(|e| Failure::usage(anyhow!("the log key hushrun.yaml names: {e}")))?,
        ),
        None => None,
    };
    let answer = hushrun_ledger::check_answer(&value, &policy.repository, verifier.as_ref())
        .map_err(|why| Failure::usage(anyhow!("{}: {why}", path.display())))?;
    if !answer.checked {
        tracing::warn!(
            "hushrun.yaml names no `transparency_log`, so the signers in {} can't be checked against the server's log key: they are taken as saved",
            path.display()
        );
    }
    Ok(answer)
}
