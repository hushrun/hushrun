//! Events: in-toto Statements whose predicate records what a pipeline did (E-3 … E-7).
//!
//! Parsing ignores unknown fields (E-8 step 3); every field hushrun writes is listed here.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::digest::{Digest, SHA256_PREFIX};
use crate::names::{ArtifactName, CheckName, ScopeName, StageName};
use crate::run::{Custody, Provider, RunRef, Trigger};
use crate::time::Time;

/// The in-toto Statement type.
pub const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";

/// The predicate type of every event, version 1.
pub fn predicate_type() -> String {
    format!("{}/event/v1", crate::SPEC_URI_BASE)
}

/// An in-toto Statement v1 (E-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Statement {
    #[serde(rename = "_type")]
    pub statement_type: String,
    pub subject: Vec<Subject>,
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
    pub predicate: serde_json::Value,
}

/// A Statement subject: `<repository>#<scope>`, with the source digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    pub name: String,
    pub digest: BTreeMap<String, String>,
}

/// The event kinds (E-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Started,
    Check,
    Artifact,
    Deployed,
    Passed,
    Failed,
    Superseded,
    Approval,
}

impl EventKind {
    pub const ALL: [EventKind; 8] = [
        EventKind::Started,
        EventKind::Check,
        EventKind::Artifact,
        EventKind::Deployed,
        EventKind::Passed,
        EventKind::Failed,
        EventKind::Superseded,
        EventKind::Approval,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Started => "started",
            EventKind::Check => "check",
            EventKind::Artifact => "artifact",
            EventKind::Deployed => "deployed",
            EventKind::Passed => "passed",
            EventKind::Failed => "failed",
            EventKind::Superseded => "superseded",
            EventKind::Approval => "approval",
        }
    }

    pub fn parse(s: &str) -> Option<EventKind> {
        EventKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// A stage's lifecycle events (T-16).
    pub fn is_lifecycle(self) -> bool {
        matches!(
            self,
            EventKind::Started
                | EventKind::Deployed
                | EventKind::Passed
                | EventKind::Failed
                | EventKind::Superseded
        )
    }

    /// Events that end a run attempt (P-6).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            EventKind::Passed | EventKind::Failed | EventKind::Superseded
        )
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The predicate of an event (E-4, E-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub kind: EventKind,
    pub repository: String,
    pub scope: ScopeName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<StageName>,
    pub canonicalization: String,
    pub source: SourceInfo,
    pub run: RunInfo,
    pub producer: Producer,
    pub time: Time,
    pub policy_digest: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced: Option<bool>,
    /// \[M18\] The newest checkpoint of the ledger's transparency log that the writer verified
    /// before signing, as a signed note (E-4, L-11). Readers ask the ledger for a consistency
    /// proof to their own checkpoint, so a ledger that showed two histories is caught.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<Started>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployed: Option<Deployed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<Passed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<Failed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<Superseded>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Approval>,
}

/// Where the content came from. Informational; never used for matching.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub vcs: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_format: Option<String>,
}

/// The run that produced an event (E-4, §11.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub provider: Provider,
    pub id: String,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub trigger: Trigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

impl RunInfo {
    /// The run attempt this event belongs to (P-5).
    pub fn run_ref(&self) -> RunRef {
        RunRef {
            provider: self.provider,
            id: self.id.clone(),
            attempt: self.attempt,
        }
    }
}

/// What signed an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Producer {
    pub tool: String,
    pub version: String,
    pub custody: Custody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoked_by: Option<InvokedBy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokedBy {
    #[serde(rename = "type")]
    pub invoker: Invoker,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Invoker {
    Human,
    Agent,
}

/// `started`: a run attempt began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Started {
    /// How long the attempt counts as in flight; readers cap it at `time` + the stage's lease.
    pub lease_until: Time,
    /// \[M18\] The signed listings the plan decided from (E-5, L-11): `plan --start` records
    /// them on a ledger that keeps a transparency log, so a monitor can recompute each one at
    /// its checkpoint. Present, and possibly empty, on every `started` that `plan --start`
    /// wrote to an HTTP ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listings: Option<Vec<ListingRecord>>,
}

/// \[M18\] One listing page a plan read: the ledger's signed statement (a C2SP signed note,
/// L-11), and the keys the page held, unless there were too many to record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListingRecord {
    pub statement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
}

/// Whether a check's result depends only on content, or on a stage's environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceKind {
    Source,
    Deployment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail,
}

/// `check`: a check's result (E-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckRecord {
    pub name: CheckName,
    pub check_kind: EvidenceKind,
    pub definition_digest: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_digest: Option<Digest>,
    pub result: Outcome,
    pub attempts: u32,
    pub duration_ms: u64,
    pub started_at: Time,
    pub finished_at: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports: Option<Vec<Report>>,
}

/// Where a check ran (E-7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    None,
    Restricted,
    Open,
}

impl Network {
    pub fn as_str(self) -> &'static str {
        match self {
            Network::None => "none",
            Network::Restricted => "restricted",
            Network::Open => "open",
        }
    }
}

/// A SHA-256 digest written as an in-toto DigestSet: `{"sha256": "<hex>"}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DigestSet {
    pub sha256: String,
}

impl DigestSet {
    pub fn of(digest: &Digest) -> Self {
        DigestSet {
            sha256: digest.hex(),
        }
    }

    pub fn digest(&self) -> Option<Digest> {
        Digest::from_hex(&self.sha256).ok()
    }
}

/// A report a check produced, identified by digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub name: String,
    pub digest: DigestSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactType {
    Oci,
    File,
}

impl ArtifactType {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactType::Oci => "oci",
            ArtifactType::File => "file",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Built,
    Copied,
}

/// `artifact`: a build output, pinned by digest (E-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRecord {
    pub name: ArtifactName,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    #[serde(rename = "ref")]
    pub reference: String,
    pub digest: DigestSet,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_from: Option<String>,
}

/// An artifact as `deployed` and `passed` list it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: ArtifactName,
    #[serde(rename = "ref")]
    pub reference: String,
    pub digest: DigestSet,
}

/// `deployed`: the stage now runs these artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployed {
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `passed`: the run attempt passed, with the artifacts it validated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passed {
    pub artifacts: Vec<ArtifactRef>,
}

/// `failed`: the run attempt failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failed {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<CheckName>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SupersededReason {
    /// A newer revision took over before this one deployed (`guard`).
    Stale,
    /// The run was cancelled.
    Cancelled,
}

impl SupersededReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SupersededReason::Stale => "stale",
            SupersededReason::Cancelled => "cancelled",
        }
    }
}

/// `superseded`: the run attempt ended without a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Superseded {
    pub reason: SupersededReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_revision: Option<String>,
}

/// `approval`: a person's sign-off on an approval check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub check: CheckName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

impl Event {
    /// `<repository>#<scope>`, the Statement subject name (E-3).
    pub fn subject_name(&self) -> String {
        subject_name(&self.repository, &self.scope)
    }

    /// The Statement for this event about source digest `digest`.
    pub fn statement(&self, digest: &Digest) -> Statement {
        Statement {
            statement_type: STATEMENT_TYPE.to_string(),
            subject: vec![Subject {
                name: self.subject_name(),
                digest: BTreeMap::from([("sha256".to_string(), digest.hex())]),
            }],
            predicate_type: predicate_type(),
            predicate: serde_json::to_value(self).expect("events serialize"),
        }
    }

    /// Which kind-specific objects are present, for E-8 step 7.
    pub fn kind_objects(&self) -> Vec<EventKind> {
        let mut present = Vec::new();
        let mut note = |present_here: bool, kind| {
            if present_here {
                present.push(kind);
            }
        };
        note(self.started.is_some(), EventKind::Started);
        note(self.check.is_some(), EventKind::Check);
        note(self.artifact.is_some(), EventKind::Artifact);
        note(self.deployed.is_some(), EventKind::Deployed);
        note(self.passed.is_some(), EventKind::Passed);
        note(self.failed.is_some(), EventKind::Failed);
        note(self.superseded.is_some(), EventKind::Superseded);
        note(self.approval.is_some(), EventKind::Approval);
        present
    }
}

/// An event that passed verification (E-8), as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedEvent {
    /// The storage key it was read from.
    pub key: String,
    /// Who signed it, as the signers name it (T-15): `ci`, `user:<email>` or `agent:<name>`.
    pub signer: String,
    pub effective_custody: Custody,
    /// The source digest the statement is about.
    pub digest: Digest,
    pub event: Event,
    /// When its signature existed, by a trusted timestamp (E-10), if it has one: whether it
    /// counts at a CI-only stage when its signer isn't the CI depends on it (T-16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_time: Option<crate::time::Time>,
}

/// E-9: events are ordered by `time`, then by storage key. ULIDs make keys unique; they
/// carry no authority, so they only break ties.
pub fn sort_events(events: &mut [VerifiedEvent]) {
    events.sort_by(|a, b| {
        a.event
            .time
            .cmp(&b.event.time)
            .then_with(|| a.key.cmp(&b.key))
    });
}

/// `<repository>#<scope>`.
pub fn subject_name(repository: &str, scope: &ScopeName) -> String {
    format!("{repository}#{scope}")
}

/// The digest a reference is pinned to: the part after the last `@sha256:`.
pub fn pinned_digest(reference: &str) -> Option<Digest> {
    let at = reference.rfind('@')?;
    let digest = &reference[at + 1..];
    if !digest.starts_with(SHA256_PREFIX) || at == 0 {
        return None;
    }
    Digest::parse(digest).ok()
}

/// The repository part of a reference pinned by digest (`registry/path` of
/// `registry/path@sha256:…`), without a tag.
pub fn reference_repository(reference: &str) -> &str {
    let without_digest = reference.rsplit_once('@').map_or(reference, |(r, _)| r);
    // A tag follows the last `:` after the last `/` (a port has a `/` after it).
    match without_digest.rfind(':') {
        Some(colon) if !without_digest[colon..].contains('/') => &without_digest[..colon],
        _ => without_digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_references() {
        let hex = "a".repeat(64);
        let r = format!("us-central1-docker.pkg.dev/p/back/go@sha256:{hex}");
        assert_eq!(pinned_digest(&r).unwrap().hex(), hex);
        assert_eq!(
            reference_repository(&r),
            "us-central1-docker.pkg.dev/p/back/go"
        );
        assert_eq!(pinned_digest("registry/x:tag"), None);
        assert_eq!(pinned_digest(&format!("@sha256:{hex}")), None);
        assert_eq!(
            reference_repository("localhost:5000/x:v1"),
            "localhost:5000/x"
        );
        assert_eq!(reference_repository("localhost:5000/x"), "localhost:5000/x");
        assert_eq!(
            reference_repository(&format!("localhost:5000/x:v1@sha256:{hex}")),
            "localhost:5000/x"
        );
    }

    // covers: E-9
    #[test]
    fn events_are_ordered_by_time_then_key() {
        let event = |key: &str, time: &str| VerifiedEvent {
            key: key.to_string(),
            signer: "ci".into(),
            effective_custody: Custody::Ci,
            digest: Digest::of(b"x"),
            event: serde_json::from_value(serde_json::json!({
                "kind": "failed", "repository": "github.com/a/b", "scope": "default", "stage": "s",
                "canonicalization": "hushrun-source-v1", "source": {"vcs": "git"},
                "run": {"provider": "local", "id": "1", "attempt": 1, "trigger": "manual"},
                "producer": {"tool": "hushrun", "version": "0", "custody": "ci"},
                "time": time, "policyDigest": Digest::of(b"p"), "failed": {"reason": "x"},
            }))
            .unwrap(),
            trusted_time: None,
        };
        // A key that sorts first but was signed later comes later.
        let mut events = vec![
            event("v1/a", "2026-10-13T15:00:02.000Z"),
            event("v1/c", "2026-10-13T15:00:01.000Z"),
            event("v1/b", "2026-10-13T15:00:01.000Z"),
        ];
        sort_events(&mut events);
        let keys: Vec<&str> = events.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["v1/b", "v1/c", "v1/a"]);
    }

    #[test]
    fn kinds() {
        assert_eq!(EventKind::parse("deployed"), Some(EventKind::Deployed));
        assert!(EventKind::Deployed.is_lifecycle());
        assert!(!EventKind::Check.is_lifecycle());
        assert!(EventKind::Superseded.is_terminal());
        assert!(!EventKind::Started.is_terminal());
    }
}
