//! `hushrun repository monitor` \[M18\]: replays the repository's transparency log on hushrun's
//! server and checks what the server signed against it (L-11, RFC 0001 §4.7, H1).
//!
//! - The leaves must hash to the server's latest signed checkpoint, which must extend the one a
//!   previous run remembered (`--state`).
//! - Every checkpoint and listing statement that plans recorded in `started` events (E-4, E-5)
//!   must agree with the log at its size: a listing that left out a key the log already held is
//!   the server's own signed statement, contradicted by its own log.
//! - Every deletion must carry a GC statement signed by an admin's machine, as the signers name it
//!   (L-9), and the log must never overwrite an object.
//! - \[M19\] Every map leaf must record the map of the leaves before it: the map follows from
//!   the log, at every checkpoint (L-12).
//!
//! It needs read access and hushrun.yaml's log key only, and runs anywhere: the customer's CI, an
//! auditor's machine, or the hosted index.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, anyhow};
use serde_json::{Value, json};

use hushrun_attest::gc::verify_gc;
use hushrun_attest::{Envelope, Expected, Location, verify_with};
use hushrun_core::event::{Event, EventKind};
use hushrun_core::policy::Policy;
use hushrun_ledger::layout::parse_event_key;
use hushrun_ledger::{Ledger, LogKey};
use hushrun_tlog::checkpoint::SignedCheckpoint;
use hushrun_tlog::listing::SignedListing;
use hushrun_tlog::note::Verifier;
use hushrun_tlog::replay::{Anomaly, CheckpointStatus, ListingStatus};
use hushrun_tlog::{EntryKind, Hash, Replay};
use hushrun_vcs::Snapshot;

use crate::app::{App, ledger_failure, log_key};
use crate::context::{Content, Source};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};

/// The schema of the JSON output.
pub const SCHEMA: &str = "hushrun.repository.monitor/v1";
/// The schema of `--state`.
const STATE_SCHEMA: &str = "hushrun.repository.monitor.state/v1";
/// Leaves per request.
const PAGE: u64 = 1000;

/// What the monitor found.
#[derive(Debug, Clone)]
struct Finding {
    kind: &'static str,
    /// Errors are proof that the ledger contradicted its own log; warnings are what the
    /// monitor couldn't check.
    error: bool,
    message: String,
    details: Value,
}

impl Finding {
    fn error(kind: &'static str, message: String, details: Value) -> Finding {
        Finding {
            kind,
            error: true,
            message,
            details,
        }
    }

    fn warning(kind: &'static str, message: String, details: Value) -> Finding {
        Finding {
            kind,
            error: false,
            message,
            details,
        }
    }

    fn to_json(&self) -> Value {
        let mut value = json!({
            "kind": self.kind,
            "severity": if self.error { "error" } else { "warning" },
            "message": self.message,
        });
        if let (Some(object), Some(details)) = (value.as_object_mut(), self.details.as_object()) {
            for (k, v) in details {
                object.insert(k.clone(), v.clone());
            }
        }
        value
    }
}

#[derive(Debug, Default)]
struct Counts {
    started: usize,
    listings: usize,
    checkpoints: usize,
    deletions: usize,
    /// \[M19\] Map leaves checked against the replayed map.
    maps: u64,
}

pub fn run(app: &App<'_>, state: Option<&Path>) -> CmdResult {
    let content = Content::open(app.global, Source::Repo(Snapshot::default()))?;
    let policy = content.policy()?;
    let (url, ledger) = app.ledger(Some(policy))?;
    let signers = app.signers(policy, ledger.as_ref())?;
    let Some(log) = ledger.log() else {
        return Err(Failure::usage(anyhow!("{url} keeps no transparency log")));
    };
    let Some(ledger_log) = &policy.transparency_log else {
        return Err(Failure::usage(anyhow!(
            "hushrun.yaml names no log key for {url} (`transparency_log`), so the monitor can't tell the server's signatures from anyone else's"
        )));
    };
    let verifier = LogKey {
        origin: ledger_log.origin.clone(),
        key: log_key(ledger_log)?,
    }
    .verifier()
    .map_err(|e| Failure::usage(anyhow!("`transparency_log`: {e}")))?;

    let latest = log.latest().map_err(ledger_failure)?;
    let mut replay = Replay::new();
    let mut statements: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut findings = Vec::new();
    while replay.size() < latest.size() {
        let want = (latest.size() - replay.size()).min(PAGE);
        let leaves = log.leaves(replay.size(), want).map_err(ledger_failure)?;
        if leaves.is_empty() {
            findings.push(Finding::error(
                "missing_leaves",
                format!(
                    "the server signed a tree of {} leaves, but serves only {}",
                    latest.size(),
                    replay.size()
                ),
                json!({"checkpoint": latest.note, "served": replay.size()}),
            ));
            break;
        }
        for leaf in leaves {
            if let Some(statement) = leaf.gc_statement {
                statements.insert(leaf.index, statement);
            }
            replay.push(leaf.entry);
        }
    }
    let mut counts = Counts::default();
    let consistent = replay.root_at(latest.size()) == Some(*latest.root());
    if !consistent {
        findings.push(Finding::error(
            "inconsistent_leaves",
            format!(
                "the leaves the server serves don't hash to its latest signed checkpoint, of {} leaves",
                latest.size()
            ),
            json!({
                "checkpoint": latest.note,
                "replayedRoot": replay.root_at(replay.size()).map(|h| h.base64()),
            }),
        ));
    }

    counts.maps = replay.maps();
    if replay.maps() > 0 && latest.size() > 0 && replay.last_map() != Some(latest.size() - 1) {
        findings.push(Finding::warning(
            "unmapped_checkpoint",
            format!(
                "the latest checkpoint, of {} leaves, doesn't end with a map leaf, so no listing can be proven at it",
                latest.size()
            ),
            json!({"checkpoint": latest.note}),
        ));
    }
    // \[ADR 0061\] Whether the run may move the remembered checkpoint on: not past one it
    // couldn't check, or the anchor would reset without a word.
    let mut moves_on = true;
    if let Some(path) = state {
        moves_on = check_state(
            path,
            &verifier,
            &replay,
            &latest,
            &mut findings,
            &mut counts,
        )?;
    }
    anomalies(&replay, &mut findings);
    deletions(
        &replay,
        &statements,
        &url.to_string(),
        &Attribution {
            signers: &signers,
            sigstore: &app.sigstore_root()?,
            now: app.now,
        },
        &mut findings,
        &mut counts,
    );
    started_events(
        app,
        ledger.as_ref(),
        policy,
        &signers,
        &verifier,
        &replay,
        &latest,
        &mut findings,
        &mut counts,
    )?;

    let errors = findings.iter().filter(|f| f.error).count();
    if let Some(path) = state
        && consistent
        && errors == 0
        && moves_on
    {
        let remembered =
            json!({"schema": STATE_SCHEMA, "ledger": url.to_string(), "checkpoint": latest.note});
        // \[ADR 0061\] Whole or not at all: a run cut short never leaves half a state, which the
        // next run would refuse.
        let text = serde_json::to_string_pretty(&remembered).map_err(anyhow::Error::from)?;
        crate::cmd::setup::write(path, &text)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    let kinds = |kind: EntryKind| replay.entries().iter().filter(|e| e.kind == kind).count();
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": SCHEMA,
            "ledger": url.to_string(),
            "origin": verifier.name(),
            "checkpoint": {
                "size": latest.size(),
                "root": latest.root().base64(),
                "note": latest.note,
            },
            "leaves": {
                "objects": kinds(EntryKind::Object),
                "config": kinds(EntryKind::Config),
                "tombstones": kinds(EntryKind::Tombstone),
            },
            "checked": {
                "startedEvents": counts.started,
                "listings": counts.listings,
                "checkpoints": counts.checkpoints,
                "deletions": counts.deletions,
                "maps": counts.maps,
            },
            "ok": errors == 0,
            "findings": findings.iter().map(Finding::to_json).collect::<Vec<_>>(),
        }))?,
        _ => {
            let mut text = format!(
                "{url}: a log of {} leaves ({} objects, {} configuration writes, {} deletions), signed by {}\nchecked {} started event(s), {} listing(s), {} checkpoint(s), {} deletion(s) and {} map(s)\n",
                latest.size(),
                kinds(EntryKind::Object),
                kinds(EntryKind::Config),
                kinds(EntryKind::Tombstone),
                verifier.name(),
                counts.started,
                counts.listings,
                counts.checkpoints,
                counts.deletions,
                counts.maps
            );
            for finding in &findings {
                let _ = writeln!(
                    text,
                    "{}: {}: {}",
                    if finding.error { "error" } else { "warning" },
                    finding.kind,
                    finding.message
                );
                for note_field in ["statement", "note"] {
                    if let Some(note) = finding.details.get(note_field).and_then(Value::as_str) {
                        for line in note.lines() {
                            let _ = writeln!(text, "    {line}");
                        }
                    }
                }
            }
            let _ = writeln!(
                text,
                "{}",
                if errors > 0 {
                    format!("{errors} problem(s): the server contradicts its own log")
                } else {
                    "ok: the server's log agrees with everything it signed".to_string()
                }
            );
            output::print(&text)?;
        }
    }
    Ok(if errors > 0 {
        Exit::VerifyFailed
    } else {
        Exit::Ok
    })
}

/// `--state`: the checkpoint a previous run verified must be one the log still extends. Whether
/// the run may remember a newer one: \[ADR 0061\] not when the remembered one can't be checked
/// against the log key, which a run that went on would forget.
fn check_state(
    path: &Path,
    verifier: &Verifier,
    replay: &Replay,
    latest: &SignedCheckpoint,
    findings: &mut Vec<Finding>,
    counts: &mut Counts,
) -> Result<bool, Failure> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => {
            return Err(Failure::usage(anyhow!("reading {}: {e}", path.display())));
        }
    };
    let state: Value = serde_json::from_slice(&bytes)
        .map_err(|e| Failure::usage(anyhow!("{}: {e}", path.display())))?;
    let Some(note) = state.get("checkpoint").and_then(Value::as_str) else {
        return Err(Failure::usage(anyhow!(
            "{} has no checkpoint; it isn't a {STATE_SCHEMA} file",
            path.display()
        )));
    };
    let source = format!("the checkpoint {} remembered", path.display());
    let checked = check_checkpoint(note, &source, verifier, replay, latest, findings, counts);
    if !checked && let Some(finding) = findings.last_mut() {
        let _ = write!(
            finding.message,
            "; {} keeps it: remove the file to start again from this checkpoint",
            path.display()
        );
    }
    Ok(checked)
}

/// A signed checkpoint against the replayed log: its root at that size, or beyond. Whether its
/// signature verified, so that it could be checked at all.
fn check_checkpoint(
    note: &str,
    source: &str,
    verifier: &Verifier,
    replay: &Replay,
    latest: &SignedCheckpoint,
    findings: &mut Vec<Finding>,
    counts: &mut Counts,
) -> bool {
    let checkpoint = match SignedCheckpoint::open(note, verifier) {
        Ok(checkpoint) => checkpoint,
        Err(e) => {
            findings.push(Finding::warning(
                "unverifiable_checkpoint",
                format!("{source} isn't signed by the log key hushrun.yaml names: {e}"),
                json!({"source": source, "note": note}),
            ));
            return false;
        }
    };
    counts.checkpoints += 1;
    match replay.check_checkpoint(&checkpoint.checkpoint) {
        CheckpointStatus::Matches => {}
        CheckpointStatus::Beyond { replayed } => findings.push(Finding::error(
            "rollback",
            format!(
                "{source} is a signed tree of {} leaves, but the log now has {replayed}",
                checkpoint.size()
            ),
            json!({"source": source, "note": note, "logSize": replayed, "latest": latest.note}),
        )),
        CheckpointStatus::Differs { replayed } => findings.push(Finding::error(
            "fork",
            format!(
                "{source} is a signed tree of {} leaves whose root isn't the log's at that size: the server showed two histories",
                checkpoint.size()
            ),
            json!({"source": source, "note": note, "replayedRoot": replayed.base64(), "latest": latest.note}),
        )),
    }
    true
}

/// What a ledger keeping L-6 and L-9 never writes, and \[M19\] maps that don't follow from
/// the log (L-12).
fn anomalies(replay: &Replay, findings: &mut Vec<Finding>) {
    for anomaly in replay.anomalies() {
        if let Anomaly::MapMismatch {
            index,
            recorded,
            computed,
        } = anomaly
        {
            findings.push(Finding::error(
                "map_mismatch",
                format!(
                    "map leaf {index} commits to a map the log's leaves before it don't make: the server committed a map that leaves keys out or adds some, so its listings were proven against a lie"
                ),
                json!({
                    "index": index,
                    "recordedRoot": recorded.hex(),
                    "replayedRoot": computed.hex(),
                }),
            ));
            continue;
        }
        let (kind, message, key) = match anomaly {
            Anomaly::Overwrite { key, .. } => (
                "overwrite",
                format!(
                    "the log creates {key} while it exists: an overwrite, which creates never do"
                ),
                key,
            ),
            Anomaly::DeleteMissing { key, .. } => (
                "delete_missing",
                format!("the log deletes {key}, which it doesn't hold"),
                key,
            ),
            Anomaly::DeleteMismatch { key, .. } => (
                "delete_mismatch",
                format!("the log deletes {key} with a hash other than the object's"),
                key,
            ),
            Anomaly::MapMismatch { .. } => unreachable!("reported above"),
        };
        findings.push(Finding::error(
            kind,
            message,
            json!({"index": anomaly.index(), "key": key}),
        ));
    }
}

/// What attributes a deletion (L-9): \[ADR 0031\] the machines the signers mark `gc`.
struct Attribution<'a> {
    signers: &'a hushrun_core::signers::Signers,
    sigstore: &'a hushrun_attest::TrustedRoot,
    now: hushrun_core::time::Time,
}

impl Attribution<'_> {
    fn verify(&self, bytes: &[u8]) -> Result<hushrun_attest::gc::VerifiedGc, String> {
        verify_gc(bytes, self.signers, self.sigstore, self.now)
    }
}

/// Every deletion needs a GC statement signed by an admin's machine (L-9).
fn deletions(
    replay: &Replay,
    statements: &BTreeMap<u64, Vec<u8>>,
    url: &str,
    attribution: &Attribution<'_>,
    findings: &mut Vec<Finding>,
    counts: &mut Counts,
) {
    for (index, entry) in replay.entries().iter().enumerate() {
        if entry.kind != EntryKind::Tombstone {
            continue;
        }
        counts.deletions += 1;
        let index = index as u64;
        let why = match (&entry.gc, statements.get(&index)) {
            (None, _) => Some("nobody signed it".to_string()),
            (Some(_), None) => Some("the server doesn't serve its GC statement".to_string()),
            (Some(_), Some(bytes)) => match attribution.verify(bytes) {
                Err(e) => Some(e),
                Ok(gc) if gc.payload.key != entry.name || gc.payload.ledger != url => {
                    Some(format!(
                        "its GC statement is for {} on {}",
                        gc.payload.key, gc.payload.ledger
                    ))
                }
                Ok(_) => None,
            },
        };
        if let Some(why) = why {
            findings.push(Finding::error(
                "unsigned_deletion",
                format!(
                    "the log deletes {} (leaf {index}) without an administrator's signature: {why}",
                    entry.name
                ),
                json!({"index": index, "key": entry.name}),
            ));
        }
    }
}

/// Every `started` event's checkpoint (E-4) and recorded listings (E-5), against the log.
#[allow(clippy::too_many_arguments)]
fn started_events(
    app: &App<'_>,
    ledger: &dyn Ledger,
    policy: &Policy,
    signers: &hushrun_core::signers::Signers,
    verifier: &Verifier,
    replay: &Replay,
    latest: &SignedCheckpoint,
    findings: &mut Vec<Finding>,
    counts: &mut Counts,
) -> Result<(), Failure> {
    let mut keys = Vec::new();
    for entry in replay.entries() {
        if entry.kind != EntryKind::Object {
            continue;
        }
        let Some(meta) = parse_event_key(&entry.name) else {
            continue;
        };
        if meta.kind != EventKind::Started || keys.iter().any(|(k, _, _)| *k == entry.name) {
            continue;
        }
        match replay.live(&entry.name) {
            Some(index) => keys.push((entry.name.clone(), meta, index)),
            None => findings.push(Finding::warning(
                "deleted_event",
                format!(
                    "{} was deleted, so its listings can't be checked any more",
                    entry.name
                ),
                json!({"event": entry.name}),
            )),
        }
    }
    let names: Vec<String> = keys.iter().map(|(k, _, _)| k.clone()).collect();
    let objects = ledger.get_many(&names);
    let sigstore = app.sigstore_root()?;
    for ((key, meta, index), bytes) in keys.into_iter().zip(objects) {
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(hushrun_ledger::LedgerError::NotFound(_)) => {
                findings.push(Finding::error(
                    "missing_object",
                    format!("the log holds {key} (leaf {index}), but the server doesn't serve it"),
                    json!({"event": key, "index": index}),
                ));
                continue;
            }
            Err(e) => return Err(ledger_failure(e)),
        };
        let leaf = replay.entry(index).expect("a live key's leaf");
        if Hash::of(&bytes) != leaf.sha256 {
            findings.push(Finding::error(
                "wrong_bytes",
                format!(
                    "the server serves other bytes for {key} than its log holds (leaf {index})"
                ),
                json!({"event": key, "index": index}),
            ));
            continue;
        }
        counts.started += 1;
        let expected = Expected {
            repository: &policy.repository,
            scope: &meta.scope,
            location: Location::Events {
                digest: &meta.digest,
                kind: meta.kind,
                stage: meta.stage.as_ref(),
            },
        };
        let event = match verify_with(&bytes, signers, &sigstore, &expected, app.now) {
            Ok(verified) => verified.event,
            Err(rejection) => {
                // Statements are the ledger's own signatures: whoever recorded them, they
                // still have to agree with the log.
                findings.push(Finding::warning(
                    "unverified_event",
                    format!("{key} doesn't verify ({rejection}); its listings are checked anyway"),
                    json!({"event": key}),
                ));
                match unverified_event(&bytes) {
                    Some(event) => event,
                    None => continue,
                }
            }
        };
        let context = json!({
            "event": key,
            "scope": meta.scope,
            "stage": meta.stage,
            "digest": meta.digest,
            "run": {"provider": event.run.provider, "id": event.run.id, "attempt": event.run.attempt},
        });
        if let Some(note) = &event.checkpoint {
            check_checkpoint(
                note,
                &format!("the checkpoint in {key}"),
                verifier,
                replay,
                latest,
                findings,
                counts,
            );
        }
        let listings = event
            .started
            .as_ref()
            .and_then(|s| s.listings.as_ref())
            .cloned()
            .unwrap_or_default();
        for record in listings {
            check_listing(
                &record.statement,
                record.keys.as_deref(),
                &key,
                &context,
                verifier,
                replay,
                latest,
                findings,
                counts,
            );
        }
    }
    Ok(())
}

/// The event inside an envelope, read without verifying it.
fn unverified_event(bytes: &[u8]) -> Option<Event> {
    let envelope = Envelope::parse(bytes).ok()?;
    let statement: Value = serde_json::from_slice(&envelope.payload_bytes().ok()?).ok()?;
    serde_json::from_value(statement.get("predicate")?.clone()).ok()
}

/// One recorded listing: the ledger's statement, recomputed from the log at its checkpoint.
#[allow(clippy::too_many_arguments)]
fn check_listing(
    note: &str,
    recorded: Option<&[String]>,
    event: &str,
    context: &Value,
    verifier: &Verifier,
    replay: &Replay,
    latest: &SignedCheckpoint,
    findings: &mut Vec<Finding>,
    counts: &mut Counts,
) {
    let with = |extra: Value| {
        let mut details = context.clone();
        if let (Some(object), Some(extra)) = (details.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                object.insert(k.clone(), v.clone());
            }
        }
        details
    };
    let statement = match SignedListing::open(note, verifier) {
        Ok(statement) => statement,
        Err(e) => {
            findings.push(Finding::warning(
                "unverifiable_listing",
                format!(
                    "{event} records a listing the log key hushrun.yaml names didn't sign: {e}"
                ),
                with(json!({"statement": note})),
            ));
            return;
        }
    };
    counts.listings += 1;
    let listing = &statement.listing;
    let check = replay.check_listing(listing, recorded);
    let range = format!(
        "the listing of {:?}{}{} at a tree of {} leaves",
        listing.prefix,
        listing
            .after
            .as_deref()
            .map(|a| format!(" after {a}"))
            .unwrap_or_default(),
        listing
            .next
            .as_deref()
            .map(|n| format!(" up to {n}"))
            .unwrap_or_default(),
        listing.size
    );
    match check.status {
        ListingStatus::Complete => {}
        ListingStatus::Beyond => findings.push(Finding::error(
            "rollback",
            format!(
                "{range}, which {event} recorded, is signed for a tree the log no longer has (it has {} leaves)",
                replay.size()
            ),
            with(json!({"statement": note, "latest": latest.note})),
        )),
        ListingStatus::Differs => findings.push(Finding::error(
            "fork",
            format!(
                "{range}, which {event} recorded, is signed for a root the log never had at that size: the server showed two histories"
            ),
            with(json!({"statement": note, "latest": latest.note})),
        )),
        ListingStatus::Incomplete => {
            let omitted: Vec<Value> = check
                .omitted
                .iter()
                .map(|(key, index)| json!({"key": key, "index": index}))
                .collect();
            let message = if check.omitted.is_empty() {
                format!(
                    "{range}, which {event} recorded, isn't what the log held there: the server signed for {} key(s), and the log held {}",
                    listing.count,
                    check.log_keys.len()
                )
            } else {
                format!(
                    "{range}, which {event} recorded, left out {} key(s) the log already held: {}",
                    check.omitted.len(),
                    check
                        .omitted
                        .iter()
                        .map(|(key, index)| format!("{key} (leaf {index})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            findings.push(Finding::error(
                "omitted_keys",
                message,
                with(json!({
                    "statement": note,
                    "omitted": omitted,
                    "extra": check.extra,
                    "logKeys": check.log_keys.len(),
                    "recordedKeys": check.recorded_match,
                })),
            ));
        }
    }
}
