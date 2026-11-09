//! Recording lifecycle events: the implicit `started` (§10.4) and the terminal events P-6
//! would ignore.

use hushrun_attest::TrustedRoot;
use hushrun_core::event::{
    Event, EventKind, ListingRecord, Producer, RunInfo, Started, VerifiedEvent,
};
use hushrun_core::plan::{failure_wins, records_attempt};
use hushrun_core::policy::Policy;
use hushrun_core::signers::Signers;
use hushrun_core::time::{Dur, Time};
use hushrun_ledger::Ledger;

use crate::app::ledger_failure;
use crate::cmd::common::base_event;
use crate::context::Subject;
use crate::events::{self, EventStore, Read};
use crate::exit::Failure;
use crate::signing::Signing;

/// Everything needed to sign and record events for one run.
pub struct Recorder<'a> {
    pub ledger: &'a dyn Ledger,
    pub signers: &'a Signers,
    pub policy: &'a Policy,
    pub signing: &'a Signing,
    /// The Sigstore trust root for reading (T-12).
    pub sigstore: &'a TrustedRoot,
    pub producer: &'a Producer,
    pub run: &'a RunInfo,
    pub now: Time,
    /// Events of a forced run carry `forced: true` (E-4).
    pub forced: bool,
}

/// What `record` did.
pub struct Recorded {
    pub written: Vec<(EventKind, Vec<String>)>,
    /// Why nothing was recorded, when P-6 would ignore the event.
    pub ignored: Option<String>,
}

impl Recorder<'_> {
    /// A new event of `kind` for this run, with the common fields.
    pub fn event(&self, kind: EventKind, subject: &Subject) -> Event {
        let mut event = base_event(
            kind,
            self.policy,
            subject,
            self.run,
            self.producer,
            self.now,
        );
        if self.forced {
            event.forced = Some(true);
        }
        event
    }

    /// The verified events of the subject.
    pub fn read(&self, subject: &Subject) -> Result<Read, Failure> {
        EventStore {
            ledger: self.ledger,
            signers: self.signers,
            sigstore: self.sigstore,
            repository: &self.policy.repository,
            now: self.now,
        }
        .read(&subject.scope, &subject.digest)
        .map_err(ledger_failure)
    }

    /// \[M18\] Whether this attempt has the `started` that `plan --start` writes, which records
    /// the listings the plan used (E-5).
    pub fn planned(&self, read: &Read) -> bool {
        self.this_attempt(read).iter().any(|e| is_planned_start(e))
    }

    /// Whether this attempt has a `started` from `plan --start` that doesn't count, as a
    /// machine's doesn't while the repository takes CI's evidence only (T-16).
    pub fn planned_without_counting(&self, read: &Read) -> bool {
        read.accepted.iter().any(|e| {
            e.event.run.run_ref() == self.run.run_ref()
                && is_planned_start(e)
                && !records_attempt(e, &self.signers.ci_only)
        })
    }

    /// This attempt's lifecycle events, from signers that count (T-16), as P-5 groups them.
    pub fn this_attempt<'r>(&self, read: &'r Read) -> Vec<&'r VerifiedEvent> {
        read.accepted
            .iter()
            .filter(|e| {
                e.event.run.run_ref() == self.run.run_ref()
                    && records_attempt(e, &self.signers.ci_only)
            })
            .collect()
    }

    /// Writes `started` for this attempt unless it has one (`always` writes it regardless).
    /// \[M18\] `listings` are the signed listings `plan --start` decided from (E-5).
    pub fn ensure_started(
        &self,
        subject: &Subject,
        read: &Read,
        lease: Option<Dur>,
        always: bool,
        listings: Option<Vec<ListingRecord>>,
    ) -> Result<Vec<(EventKind, Vec<String>)>, Failure> {
        let has = self
            .this_attempt(read)
            .iter()
            .any(|e| e.event.kind == EventKind::Started);
        if has && !always {
            return Ok(Vec::new());
        }
        let lease = lease.unwrap_or_else(|| self.policy.deploy_or_default().lease);
        let mut started = self.event(EventKind::Started, subject);
        started.started = Some(Started {
            lease_until: self.now.saturating_add(lease),
            listings,
        });
        let keys = events::write(self.ledger, self.signing, &started, &subject.digest)?;
        Ok(vec![(EventKind::Started, keys)])
    }

    /// Records a terminal or `deployed` event built by `fill`, after the implicit `started`,
    /// unless P-6 would ignore it.
    pub fn record(
        &self,
        subject: &Subject,
        kind: EventKind,
        fill: impl FnOnce(&mut Event, &Read) -> Result<(), Failure>,
    ) -> Result<Recorded, Failure> {
        let read = self.read(subject)?;
        let ignored = |reason| {
            Ok(Recorded {
                written: Vec::new(),
                ignored: Some(reason),
            })
        };
        if kind == EventKind::Superseded
            && let Some(reason) = self.verdict_kept(&read)
        {
            return ignored(reason);
        }
        let mut event = self.event(kind, subject);
        fill(&mut event, &read)?;
        // [ADR 0056] The same deployment this attempt recorded already, whose deployed-index
        // entry a failed second write left missing: completed, not written again as a pair.
        if kind == EventKind::Deployed
            && let Some(earlier) =
                self.this_attempt(&read).into_iter().rev().find(|e| {
                    e.event.kind == EventKind::Deployed && e.event.deployed == event.deployed
                })
            && let Some(keys) =
                events::complete_deployed(self.ledger, &earlier.key, &earlier.event)?
        {
            return Ok(Recorded {
                written: vec![(kind, keys)],
                ignored: None,
            });
        }
        if kind == EventKind::Passed && self.failed_in(&read).next().is_some() {
            // P-6 compares the `passed`'s `time`, which is when it is signed (E-4), with the
            // `failed`'s: not when this command started. Signing it tells when; it is signed
            // again below, later still, which P-6 counts all the more.
            let (signed, _) = events::sign(self.ledger, self.signing, &event, &subject.digest)?;
            if self
                .failed_in(&read)
                .any(|failed| failure_wins(failed.event.time, signed.time))
            {
                return ignored(
                    "this attempt recorded `failed` less than 60 s before this `passed`, and the failure wins".to_string(),
                );
            }
        }
        let mut written = self.ensure_started(subject, &read, None, false, None)?;
        let keys = events::write(self.ledger, self.signing, &event, &subject.digest)?;
        written.push((kind, keys));
        Ok(Recorded {
            written,
            ignored: None,
        })
    }

    /// This attempt's `failed` events.
    fn failed_in<'r>(&self, read: &'r Read) -> impl Iterator<Item = &'r VerifiedEvent> {
        self.this_attempt(read)
            .into_iter()
            .filter(|e| e.event.kind == EventKind::Failed)
    }

    /// Why P-6 would ignore a `superseded`: this attempt already has a verdict.
    fn verdict_kept(&self, read: &Read) -> Option<String> {
        self.this_attempt(read)
            .iter()
            .find(|e| matches!(e.event.kind, EventKind::Passed | EventKind::Failed))
            .map(|e| {
                format!(
                    "this attempt already recorded `{}`, and a cancellation never hides a verdict",
                    e.event.kind
                )
            })
    }
}

/// A `started` that `plan --start` wrote: it records the listings the plan decided from (E-5).
fn is_planned_start(e: &VerifiedEvent) -> bool {
    e.event.kind == EventKind::Started
        && e.event
            .started
            .as_ref()
            .is_some_and(|s| s.listings.is_some())
}
