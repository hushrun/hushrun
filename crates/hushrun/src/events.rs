//! Reading events from the ledger with verification (E-8), and writing signed events.
//!
//! \[ADR 0032\] A `check` event that hushrun's server refuses for the org's plan isn't recorded,
//! and the command goes on, saying so once ([`record`], [`Unrecorded`], L-5).
//!
//! \[M18\] On a ledger whose transparency log is checked (L-11): every event written carries the
//! newest checkpoint its writer verified (E-4); a checkpoint read in an event must be one
//! history with the reader's; and a listed key the ledger won't serve, or serves other bytes
//! for than its log holds, ends the read (exit 6) instead of quietly dropping the event.

use std::collections::BTreeMap;

use hushrun_attest::{Expected, Location, SignedBy, TrustedRoot, verify_with};
use hushrun_core::digest::Digest;
use hushrun_core::event::{Event, EventKind, VerifiedEvent, sort_events};
use hushrun_core::names::{ScopeName, StageName};
use hushrun_core::plan::counts_for;
use hushrun_core::signers::Signers;
use hushrun_core::time::Time;
use hushrun_ledger::layout::{self, parse_deployed_key, parse_event_key};
use hushrun_ledger::{Ledger, LedgerError, Log, Quota};

use crate::app::ledger_failure;
use crate::ci::Ci;
use crate::exit::Failure;
use crate::signing::Signing;

/// Events read for one (scope, digest).
#[derive(Debug, Default)]
pub struct Read {
    pub accepted: Vec<VerifiedEvent>,
    /// Keys and why their events were ignored.
    pub rejected: Vec<(String, String)>,
    /// How each accepted event was signed, by key (`hushrun verify` shows it).
    pub signatures: BTreeMap<String, Signature>,
}

/// How an accepted event was signed (E-8 step 2, E-10).
#[derive(Debug, Clone)]
pub struct Signature {
    pub keyid: String,
    pub signed_by: SignedBy,
    pub trusted_time: Option<Time>,
}

impl Signature {
    fn of(verified: &hushrun_attest::Verified) -> Signature {
        Signature {
            keyid: verified.keyid.clone(),
            signed_by: verified.signed_by.clone(),
            trusted_time: verified.trusted_time,
        }
    }
}

/// Verifying reads from one ledger.
pub struct EventStore<'a> {
    pub ledger: &'a dyn Ledger,
    /// \[ADR 0031\] Who counts (T-15).
    pub signers: &'a Signers,
    /// The Sigstore trust root for certificates and timestamps (T-12).
    pub sigstore: &'a TrustedRoot,
    pub repository: &'a str,
    pub now: Time,
}

impl EventStore<'_> {
    /// \[M18\] The ledger's log, when its answers are checked (L-11).
    fn log(&self) -> Option<&dyn Log> {
        self.ledger.log().filter(|log| log.verifying())
    }

    /// A listed key whose object the ledger doesn't serve: on a checked log that is the
    /// ledger's failure, never a quietly missing event (L-11).
    fn missing(&self, key: &str) -> Result<(), LedgerError> {
        match self.log() {
            Some(_) => Err(LedgerError::Unavailable(format!(
                "{key} is in the server's signed listing, but the server doesn't serve it; a server that keeps a log can't drop an event that way"
            ))),
            None => Ok(()),
        }
    }

    /// What an accepted event says about the log: its checkpoint (E-4) must be one history with
    /// this command's (L-11).
    fn accepted(&self, key: &str, event: &Event) -> Result<(), LedgerError> {
        match (self.log(), &event.checkpoint) {
            (Some(log), Some(note)) => log.check_checkpoint(note, &format!("the event {key}")),
            _ => Ok(()),
        }
    }

    /// A rejected event's bytes must be the ones the log holds, or the ledger could hide an
    /// event by serving others (L-11).
    fn rejected(&self, key: &str, bytes: &[u8]) -> Result<(), LedgerError> {
        match self.log() {
            Some(log) => log.prove_object(key, bytes),
            None => Ok(()),
        }
    }

    /// Every event under `v1/events/<scope>/<digest>/`, verified: one LIST, then the objects.
    pub fn read(&self, scope: &ScopeName, digest: &Digest) -> Result<Read, LedgerError> {
        let keys = self.ledger.list(&layout::events_prefix(scope, digest))?;
        let mut read = Read::default();
        let mut parsed = Vec::new();
        for key in keys {
            match parse_event_key(&key) {
                Some(meta) => parsed.push((key, meta)),
                None => read.rejected.push((key, "not an event key".into())),
            }
        }
        let names: Vec<String> = parsed.iter().map(|(k, _)| k.clone()).collect();
        for ((key, meta), bytes) in parsed.into_iter().zip(self.ledger.get_many(&names)) {
            let bytes = match bytes {
                Ok(bytes) => bytes,
                Err(LedgerError::NotFound(_)) => {
                    self.missing(&key)?;
                    continue;
                }
                Err(err) => return Err(err),
            };
            let expected = Expected {
                repository: self.repository,
                scope,
                location: Location::Events {
                    digest,
                    kind: meta.kind,
                    stage: meta.stage.as_ref(),
                },
            };
            match verify_with(&bytes, self.signers, self.sigstore, &expected, self.now) {
                Ok(v) => {
                    self.accepted(&key, &v.event)?;
                    read.signatures.insert(key.clone(), Signature::of(&v));
                    read.accepted.push(VerifiedEvent {
                        key,
                        signer: v.signer,
                        effective_custody: v.effective_custody,
                        digest: v.subject_digest,
                        event: v.event,
                        trusted_time: v.trusted_time,
                    });
                }
                Err(rejection) => {
                    self.rejected(&key, &bytes)?;
                    tracing::info!("ignoring {key}: {rejection}");
                    read.rejected.push((key, rejection.to_string()));
                }
            }
        }
        sort_events(&mut read.accepted);
        Ok(read)
    }

    /// The pipeline's deployed index of `scope`, newest key first (L-8): the first 20 entries,
    /// and further pages up to 200 while none of them is trusted: verified, and from a signer
    /// that counts (T-16), as the planner and GC need one. \[ADR 0062\] What hushrun 0.8 and
    /// earlier deployed for a stage is under the stage's prefix, and isn't read.
    pub fn deployed(&self, scope: &ScopeName) -> Result<Read, LedgerError> {
        self.deployed_at(scope, None)
    }

    /// [`EventStore::deployed`] under `stage`'s prefix, where hushrun 0.8 and earlier wrote a
    /// stage's deployments, or `_`'s: what `repository gc` keeps reads every prefix.
    pub fn deployed_at(
        &self,
        scope: &ScopeName,
        stage: Option<&StageName>,
    ) -> Result<Read, LedgerError> {
        let prefix = layout::deployed_prefix(scope, stage);
        let mut read = Read::default();
        let mut seen = 0usize;
        for limit in [20usize, 200] {
            let keys = self.ledger.list_first(&prefix, limit)?;
            let page: Vec<String> = keys.into_iter().skip(seen).collect();
            if page.is_empty() {
                break;
            }
            seen += page.len();
            let bytes = self.ledger.get_many(&page);
            for (key, bytes) in page.into_iter().zip(bytes) {
                if parse_deployed_key(&key).is_none() {
                    read.rejected.push((key, "not a deployed-index key".into()));
                    continue;
                }
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(LedgerError::NotFound(_)) => {
                        self.missing(&key)?;
                        continue;
                    }
                    Err(err) => return Err(err),
                };
                let expected = Expected {
                    repository: self.repository,
                    scope,
                    location: Location::Deployed { stage },
                };
                match verify_with(&bytes, self.signers, self.sigstore, &expected, self.now) {
                    Ok(v) => {
                        self.accepted(&key, &v.event)?;
                        read.accepted.push(VerifiedEvent {
                            key,
                            signer: v.signer,
                            effective_custody: v.effective_custody,
                            digest: v.subject_digest,
                            event: v.event,
                            trusted_time: v.trusted_time,
                        })
                    }
                    Err(rejection) => {
                        self.rejected(&key, &bytes)?;
                        read.rejected.push((key, rejection.to_string()))
                    }
                }
            }
            if read
                .accepted
                .iter()
                .any(|e| counts_for(&self.signers.ci_only, e))
            {
                break;
            }
        }
        sort_events(&mut read.accepted);
        Ok(read)
    }
}

/// Signs `event` and appends it; a `deployed` event also goes to the deployed index, with the
/// same bytes. The event's `time` becomes when it was signed (E-4). Returns the keys written.
/// \[M18\] On a ledger whose log is checked, the event carries the newest checkpoint this
/// command verified (E-4), and every create's receipt is checked before it counts as written
/// (L-11).
pub fn write(
    ledger: &dyn Ledger,
    signing: &Signing,
    event: &Event,
    digest: &Digest,
) -> Result<Vec<String>, Failure> {
    let (event, bytes) = sign(ledger, signing, event, digest)?;
    append(ledger, &event, &bytes, digest)
}

/// Signs `event` as [`write()`] does, without appending it: its `time` becomes when it was
/// signed, and on a ledger whose log is checked it carries the newest checkpoint.
pub fn sign(
    ledger: &dyn Ledger,
    signing: &Signing,
    event: &Event,
    digest: &Digest,
) -> Result<(Event, Vec<u8>), Failure> {
    let mut event = event.clone();
    if let Some(log) = ledger.log() {
        // Every command that writes reads the signers first, which sets the log key hushrun.yaml
        // pins: before, neither the receipt nor the checkpoint the event carries would be checked.
        if !log.configured() {
            return Err(Failure::new(
                crate::exit::Exit::Unexpected,
                anyhow::anyhow!(
                    "an event was about to be written before hushrun.yaml configured the server's transparency log; this is a bug in hushrun"
                ),
            ));
        }
        if let Some(checkpoint) = log.checkpoint().map_err(ledger_failure)? {
            event.checkpoint = Some(checkpoint.note);
        }
    }
    signing.sign(&event, digest)
}

/// Appends a signed `event`'s `bytes` under its keys (L-2); a `deployed` event also goes to
/// the deployed index. Returns the keys written.
pub fn append(
    ledger: &dyn Ledger,
    event: &Event,
    bytes: &[u8],
    digest: &Digest,
) -> Result<Vec<String>, Failure> {
    append_keys(ledger, event, bytes, digest).map_err(ledger_failure)
}

/// \[ADR 0056\] A `deployed` event is two writes (L-2): `v1/events`, then the deployed index,
/// with the same bytes. When an earlier write of the event at `key` (in `v1/events`) left its
/// deployed-index entry missing, as a second write that failed does, this writes the entry with
/// the event's own bytes and returns both keys: the planner reads what the pipeline runs from the
/// index alone (L-8). `None` when the entry is there.
pub fn complete_deployed(
    ledger: &dyn Ledger,
    key: &str,
    event: &Event,
) -> Result<Option<Vec<String>>, Failure> {
    let (Some(meta), EventKind::Deployed) = (parse_event_key(key), event.kind) else {
        return Ok(None);
    };
    let index = layout::deployed_key(&event.scope, event.stage.as_ref(), event.time, &meta.ulid);
    match ledger.get(&index) {
        Ok(_) => return Ok(None),
        Err(LedgerError::NotFound(_)) => {}
        Err(err) => return Err(ledger_failure(err)),
    }
    let bytes = ledger.get(key).map_err(ledger_failure)?;
    ledger.append(&index, &bytes).map_err(ledger_failure)?;
    Ok(Some(vec![key.to_string(), index]))
}

/// What became of an event [`record`] wrote.
#[derive(Debug)]
pub enum Recorded {
    /// It was written, under these keys.
    Keys(Vec<String>),
    /// \[ADR 0032\] A `check` event the server refused for the org's plan (402
    /// `quota_exceeded`, L-10): it isn't recorded.
    OverQuota(Box<Quota>),
}

/// [`write()`], except that a `check` event the server refuses for the org's plan isn't recorded
/// and the command goes on (L-5, ADR 0032): its checks still pass or fail, and its other events
/// are still written. Any other failure, and a refusal of any other event, is the command's, as
/// for [`write()`].
pub fn record(
    ledger: &dyn Ledger,
    signing: &Signing,
    event: &Event,
    digest: &Digest,
) -> Result<Recorded, Failure> {
    let (signed, bytes) = sign(ledger, signing, event, digest)?;
    match append_keys(ledger, &signed, &bytes, digest) {
        Ok(keys) => Ok(Recorded::Keys(keys)),
        Err(LedgerError::Quota(quota)) if signed.kind == EventKind::Check => {
            Ok(Recorded::OverQuota(quota))
        }
        Err(err) => Err(ledger_failure(err)),
    }
}

/// Says once per command that hushrun's server didn't record its check results for the org's
/// plan (L-5, ADR 0032), with the server's message and where to upgrade: on GitHub Actions as a
/// warning annotation, which the run's log and summary show, else as a `warning:` line on
/// stderr. Later refusals in the same command add nothing: the command's output marks each
/// check that wasn't recorded.
#[derive(Debug, Default)]
pub struct Unrecorded {
    said: bool,
}

impl Unrecorded {
    pub fn say(&mut self, ci: &Ci, quota: &Quota) {
        if std::mem::replace(&mut self.said, true) {
            return;
        }
        let text = unrecorded_warning(quota);
        if ci.annotates() {
            ci.annotate(
                hushrun_ci::github::Level::Warning,
                "hushrun: check results not recorded",
                &text,
            );
        } else {
            eprintln!("warning: {text}");
        }
    }
}

/// The warning [`Unrecorded`] says: `check results aren't recorded: <the server's message>;
/// upgrade: <upgrade_url>`.
pub fn unrecorded_warning(quota: &Quota) -> String {
    format!("check results aren't recorded: {}", quota.line())
}

/// [`append`], with the ledger's error as it is.
fn append_keys(
    ledger: &dyn Ledger,
    event: &Event,
    bytes: &[u8],
    digest: &Digest,
) -> Result<Vec<String>, LedgerError> {
    let ulid = hushrun_ledger::new_ulid(event.time.millis());
    let mut keys = vec![layout::event_key(
        &event.scope,
        digest,
        &ulid,
        event.kind,
        event.stage.as_ref(),
    )];
    if event.kind == EventKind::Deployed {
        keys.push(layout::deployed_key(
            &event.scope,
            event.stage.as_ref(),
            event.time,
            &ulid,
        ));
    }
    for k in &keys {
        ledger.append(k, bytes)?;
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: L-5
    #[test]
    fn the_warning_gives_the_servers_message_and_where_to_upgrade() {
        let quota = Quota {
            message: "acme used the 2,000 checks of its Free plan this month".into(),
            upgrade_url: Some("https://app.hushrun.com/acme/-/billing".into()),
            plan: Some("free".into()),
            used: Some(2000),
            limit: Some(2000),
            period_end: None,
        };
        assert_eq!(
            unrecorded_warning(&quota),
            "check results aren't recorded: acme used the 2,000 checks of its Free plan this month; upgrade: https://app.hushrun.com/acme/-/billing"
        );
    }
}
