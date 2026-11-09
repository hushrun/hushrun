//! The planner's input and output (§8.1, §8.5).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::digest::Digest;
use crate::event::VerifiedEvent;
use crate::names::{ArtifactName, CheckName, ScopeName};
use crate::run::{RunRef, Trigger};
use crate::signers::CiOnly;
use crate::time::Time;

/// Everything one decision depends on, besides the policy (§8.1). It is
/// serializable so that `plan --explain-json` can replay any decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanInput {
    /// The scope `X`.
    pub scope: ScopeName,
    /// The source digest `D`.
    pub digest: Digest,
    pub context: PlanContext,
    /// The verified events for (`X`, `D`).
    pub events: Vec<VerifiedEvent>,
    /// The verified entries of the deployed index for `X`, newest first (L-8).
    #[serde(default)]
    pub deployed: Vec<VerifiedEvent>,
    /// Liveness answers for attempts with no terminal event (P-6).
    #[serde(default)]
    pub liveness: Vec<LivenessAnswer>,
    /// When the repository took evidence from CI only, as the signers answer says (T-16, L-13).
    #[serde(default, skip_serializing_if = "CiOnly::is_empty")]
    pub ci_only: CiOnly,
}

/// The run being planned (§8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanContext {
    /// The branch the run is on, when known.
    pub branch: Option<String>,
    pub trigger: Trigger,
    pub attempt: u32,
    pub force_full: bool,
    /// This run's identity.
    pub run: RunRef,
    pub now: Time,
}

/// What a CI provider says about a run attempt without a terminal event, mapped onto P-6's
/// table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Liveness {
    /// Queued, in progress, waiting, requested or pending.
    InFlight,
    /// Completed successfully, but it recorded no outcome.
    NoVerdict,
    /// Cancelled, skipped or stale.
    Superseded,
    /// Any other conclusion.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivenessAnswer {
    pub run: RunRef,
    pub liveness: Liveness,
}

/// The five plan modes (§3). \[ADR 0062\] `wait` went with upstream stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Deploy the existing artifact by digest; no checks, no build.
    Promote,
    /// Run only the missing, expired or failed source checks, then deploy the existing
    /// artifact.
    Refresh,
    /// Build, run the checks that lack valid evidence, deploy.
    Full,
    /// The repository already runs this digest: nothing to deploy. hushrun 0.6.0 and earlier called
    /// it `skip`, which the plan files they wrote still say.
    #[serde(alias = "skip")]
    Current,
    /// Nothing; the run fails (exit 3).
    Blocked,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Promote => "promote",
            Mode::Refresh => "refresh",
            Mode::Full => "full",
            Mode::Current => "current",
            Mode::Blocked => "blocked",
        }
    }

    /// Whether the pipeline deploys in this mode.
    pub fn deploys(self) -> bool {
        matches!(self, Mode::Promote | Mode::Refresh | Mode::Full)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The evidence status of a requirement (P-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Satisfied,
    Expired,
    Failed,
    Missing,
}

/// Why a check is in `checks_to_run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunReason {
    Missing,
    Expired,
    Failed,
    /// A forced run runs every source check (P-9).
    Forced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckToRun {
    pub check: CheckName,
    pub reason: RunReason,
}

impl CheckToRun {
    /// A check to run for `reason`.
    pub fn new(check: CheckName, reason: RunReason) -> CheckToRun {
        CheckToRun { check, reason }
    }
}

/// One required check, with its status and the event that decided it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementState {
    pub requirement: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

/// The artifact a decision deploys, and the event that justifies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenArtifact {
    #[serde(rename = "ref")]
    pub reference: String,
    pub digest: Digest,
    pub event: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployedReport {
    pub digest: Digest,
    pub event: String,
    pub time: Time,
}

/// A decision for one scope (§8.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub scope: ScopeName,
    pub digest: Digest,
    pub mode: Mode,
    /// One sentence a person can act on, naming runs and URLs.
    pub reason: String,
    pub build: bool,
    pub artifacts: BTreeMap<ArtifactName, ChosenArtifact>,
    pub checks_to_run: Vec<CheckToRun>,
    pub requirements: Vec<RequirementState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployed: Option<DeployedReport>,
}
