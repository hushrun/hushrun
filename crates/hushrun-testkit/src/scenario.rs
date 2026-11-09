//! The planner scenario catalog (`testdata/scenarios/**/*.yaml`).
//!
//! A scenario names a policy (a fixture), the run being planned, the verified events in a
//! compact form, and the expected decision. The runner builds the
//! planner's input directly: scenarios test decisions, not signatures. The format is
//! documented in `docs/validation/scenarios.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use hushrun_core::digest::Digest;
use hushrun_core::event::{Event, EventKind, VerifiedEvent};
use hushrun_core::names::ScopeName;
use hushrun_core::plan::{Decision, Liveness, LivenessAnswer, PlanContext, PlanInput, plan};
use hushrun_core::policy::Policy;
use hushrun_core::run::{Custody, Provider, RunRef, Trigger};
use hushrun_core::signers::{CI_SIGNER, CiOnly, Period, Signers};
use hushrun_core::time::{Dur, Time};

/// The digest scenarios plan for.
pub fn this_digest() -> Digest {
    Digest::of(b"scenario: this digest")
}

/// Another digest, for events and deployments of other content.
pub fn other_digest() -> Digest {
    Digest::of(b"scenario: another digest")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub covers: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// A fixture under `testdata/policies/`, or inline YAML with `policy_yaml`.
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub policy_yaml: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    /// `HH:MM[:SS]` on 2026-10-13 UTC, or a full RFC 3339 timestamp.
    pub now: String,
    #[serde(default)]
    pub context: ContextSpec,
    #[serde(default)]
    pub events: Vec<EventSpec>,
    #[serde(default)]
    pub deployed: Vec<DeployedSpec>,
    #[serde(default)]
    pub liveness: Vec<LivenessSpec>,
    /// Whether the signers answer says the repository is CI-only (T-16, L-13), from the start
    /// of the scenario's day.
    #[serde(default)]
    pub ci_only: bool,
    pub expect: Expect,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSpec {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default = "push")]
    pub trigger: Trigger,
    #[serde(default = "one")]
    pub attempt: u32,
    #[serde(default)]
    pub force: bool,
    #[serde(default = "this_run")]
    pub run: String,
}

impl Default for ContextSpec {
    fn default() -> Self {
        ContextSpec {
            branch: None,
            trigger: Trigger::Push,
            attempt: 1,
            force: false,
            run: this_run(),
        }
    }
}

fn push() -> Trigger {
    Trigger::Push
}

fn one() -> u32 {
    1
}

fn this_run() -> String {
    "this-run".to_string()
}

/// One verified event, compactly.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSpec {
    pub kind: EventKind,
    /// The stage hushrun 0.8 or earlier recorded it at; none since 0.9 (ADR 0062).
    #[serde(default)]
    pub stage: Option<String>,
    pub run: String,
    #[serde(default = "one")]
    pub attempt: u32,
    pub at: String,
    /// The principal that signed it.
    pub by: String,
    /// The custody the producer claimed [default: ci]; the effective custody is capped by the
    /// principal's.
    #[serde(default)]
    pub custody: Option<Custody>,
    /// `this` \[default\] or `other`.
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// What started the run that recorded it [default: push].
    #[serde(default)]
    pub trigger: Option<Trigger>,
    // started
    #[serde(default)]
    pub lease: Option<Dur>,
    // check
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub finished: Option<String>,
    #[serde(default)]
    pub attempts: Option<u32>,
    /// `stale` makes the definition digest differ from the policy's.
    #[serde(default)]
    pub definition: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub check_kind: Option<String>,
    // artifact
    #[serde(default)]
    pub artifact: Option<String>,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    // deployed, passed
    #[serde(default)]
    pub artifacts: BTreeMap<String, String>,
    // failed, superseded
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployedSpec {
    #[serde(default)]
    pub digest: Option<String>,
    pub at: String,
    pub by: String,
    #[serde(default)]
    pub run: Option<String>,
    /// The stage hushrun 0.8 or earlier deployed it at; none since 0.9 (ADR 0062).
    #[serde(default)]
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LivenessSpec {
    pub run: String,
    #[serde(default = "one")]
    pub attempt: u32,
    pub state: Liveness,
}

/// What the decision must be. Absent fields aren't checked.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub mode: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub build: Option<bool>,
    /// Artifact name to its reference.
    #[serde(default)]
    pub artifacts: Option<BTreeMap<String, String>>,
    /// The checks to run, in order.
    #[serde(default)]
    pub checks: Option<Vec<String>>,
    /// Requirement to status.
    #[serde(default)]
    pub requirements: Option<BTreeMap<String, String>>,
}

/// Every scenario under `testdata/scenarios/`, with its path.
pub fn load_all() -> Vec<(PathBuf, Scenario)> {
    let mut found = Vec::new();
    collect(&crate::testdata("scenarios"), &mut found);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn collect(dir: &Path, out: &mut Vec<(PathBuf, Scenario)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "yaml") {
            let text = std::fs::read_to_string(&path).expect("scenario");
            let scenario: Scenario =
                serde_saphyr::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            out.push((path, scenario));
        }
    }
}

/// A time: `HH:MM[:SS]` on 2026-10-13, or RFC 3339.
pub fn time(text: &str) -> Time {
    let full = if text.contains('T') {
        text.to_string()
    } else if text.len() == 5 {
        format!("2026-10-13T{text}:00Z")
    } else {
        format!("2026-10-13T{text}Z")
    };
    Time::parse(&full).unwrap_or_else(|_| panic!("bad time {text:?}"))
}

/// Expands a reference ending in `@x` (one hex digit) to `@sha256:` and 64 of that digit.
pub fn reference(text: &str) -> String {
    match text.rsplit_once('@') {
        Some((repo, short)) if short.len() == 1 => format!("{repo}@sha256:{}", short.repeat(64)),
        _ => text.to_string(),
    }
}

impl Scenario {
    pub fn policy(&self) -> Policy {
        let text = match (&self.policy, &self.policy_yaml) {
            (_, Some(yaml)) => yaml.clone(),
            (Some(file), None) => {
                std::fs::read_to_string(crate::testdata(&format!("policies/{file}")))
                    .expect("policy")
            }
            (None, None) => panic!("{}: needs policy or policy_yaml", self.name),
        };
        Policy::parse("hushrun.yaml", text.as_bytes())
            .unwrap_or_else(|e| panic!("{}: {e}", self.name))
    }

    pub fn scope(&self) -> ScopeName {
        ScopeName::new(self.scope.as_deref().unwrap_or("default")).expect("scope")
    }

    /// The planner's input.
    pub fn input(&self, policy: &Policy) -> PlanInput {
        let events = self
            .events
            .iter()
            .enumerate()
            .map(|(i, e)| self.event(i, e, policy))
            .collect();
        let deployed = self
            .deployed
            .iter()
            .enumerate()
            .map(|(i, d)| self.deployed_event(i, d, policy))
            .collect();
        PlanInput {
            scope: self.scope(),
            digest: this_digest(),
            context: PlanContext {
                branch: self.context.branch.clone(),
                trigger: self.context.trigger,
                attempt: self.context.attempt,
                force_full: self.context.force,
                run: RunRef {
                    provider: Provider::Generic,
                    id: self.context.run.clone(),
                    attempt: self.context.attempt,
                },
                now: time(&self.now),
            },
            events,
            deployed,
            liveness: self
                .liveness
                .iter()
                .map(|l| LivenessAnswer {
                    run: RunRef {
                        provider: Provider::Generic,
                        id: l.run.clone(),
                        attempt: l.attempt,
                    },
                    liveness: l.state,
                })
                .collect(),
            ci_only: self.ci_only(),
        }
    }

    /// The repository's CI-only rule, since the start of the scenario's day (T-16).
    pub fn ci_only(&self) -> CiOnly {
        if self.ci_only {
            CiOnly::from_periods(vec![Period {
                from: time("00:00"),
                until: None,
            }])
        } else {
            CiOnly::none()
        }
    }

    fn event(&self, index: usize, spec: &EventSpec, policy: &Policy) -> VerifiedEvent {
        let at = time(&spec.at);
        let digest = match spec.digest.as_deref() {
            Some("other") => other_digest(),
            _ => this_digest(),
        };
        let scope = spec
            .scope
            .clone()
            .unwrap_or_else(|| self.scope().to_string());
        let mut value = json!({
            "kind": spec.kind,
            "repository": policy.repository,
            "scope": scope,
            "canonicalization": "hushrun-source-v1",
            "source": {"vcs": "git", "revision": format!("rev-{}", spec.run)},
            "run": {"provider": "generic", "id": spec.run, "attempt": spec.attempt, "trigger": spec.trigger.unwrap_or(Trigger::Push)},
            "producer": {"tool": "hushrun", "version": "0.1.0", "custody": spec.custody.unwrap_or(Custody::Ci)},
            "time": at,
            "policyDigest": policy.digest,
        });
        if let Some(stage) = &spec.stage {
            value["stage"] = json!(stage);
        }
        if let Some(url) = &spec.url {
            value["run"]["url"] = json!(url);
        }
        let lease = policy
            .deploy
            .as_ref()
            .map_or(Dur::from_secs(3 * 3600), |d| d.lease);
        let artifact_list = |artifacts: &BTreeMap<String, String>| -> Value {
            artifacts
                .iter()
                .map(|(name, r)| {
                    let r = reference(r);
                    let hex = r
                        .rsplit_once("@sha256:")
                        .map_or(String::new(), |(_, h)| h.to_string());
                    json!({"name": name, "ref": r, "digest": {"sha256": hex}})
                })
                .collect()
        };
        let object = match spec.kind {
            EventKind::Started => {
                json!({"leaseUntil": at.saturating_add(spec.lease.unwrap_or(lease))})
            }
            EventKind::Check => {
                let name = spec.check.as_deref().expect("check events name a check");
                let def = policy.checks.get(name);
                let definition = match (spec.definition.as_deref(), def) {
                    (Some("stale"), _) | (_, None) => Digest::of(b"stale definition"),
                    (_, Some(def)) => def.definition_digest,
                };
                let kind = spec.check_kind.clone().unwrap_or_else(|| "source".into());
                let finished = spec.finished.as_deref().map_or(at, time);
                let mut check = json!({
                    "name": name, "checkKind": kind, "definitionDigest": definition,
                    "result": spec.result.as_deref().unwrap_or("pass"),
                    "attempts": spec.attempts.unwrap_or(1), "durationMs": 1000,
                    "startedAt": finished, "finishedAt": finished,
                });
                if spec.image.is_some() || spec.platform.is_some() {
                    let mut environment = json!({});
                    if let Some(image) = &spec.image {
                        environment["image"] = json!(reference(image));
                    }
                    if let Some(platform) = &spec.platform {
                        environment["platform"] = json!(platform);
                    }
                    check["environmentDigest"] =
                        json!(hushrun_core::jcs::digest(&environment).expect("jcs"));
                    check["environment"] = environment;
                }
                check
            }
            EventKind::Artifact => {
                let r = reference(
                    spec.reference
                        .as_deref()
                        .expect("artifact events have a ref"),
                );
                let hex = r
                    .rsplit_once("@sha256:")
                    .map_or("0".repeat(64), |(_, h)| h.to_string());
                json!({
                    "name": spec.artifact.as_deref().expect("artifact events name the artifact"),
                    "type": "oci", "ref": r, "digest": {"sha256": hex}, "origin": "built",
                })
            }
            EventKind::Deployed => json!({"artifacts": artifact_list(&spec.artifacts)}),
            EventKind::Passed => json!({"artifacts": artifact_list(&spec.artifacts)}),
            EventKind::Failed => json!({"reason": spec.reason.as_deref().unwrap_or("failed")}),
            EventKind::Superseded => {
                json!({"reason": spec.reason.as_deref().unwrap_or("cancelled")})
            }
            EventKind::Approval => {
                json!({"check": spec.check.as_deref().expect("approval events name a check")})
            }
        };
        value[spec.kind.as_str()] = object;
        let event: Event = serde_json::from_value(value)
            .unwrap_or_else(|e| panic!("{}: event {index}: {e}", self.name));
        verified(index, event, digest, &spec.by)
    }

    fn deployed_event(&self, index: usize, spec: &DeployedSpec, policy: &Policy) -> VerifiedEvent {
        let digest = match spec.digest.as_deref() {
            Some("this") => this_digest(),
            _ => other_digest(),
        };
        let at = time(&spec.at);
        let mut value = json!({
            "kind": "deployed",
            "repository": policy.repository,
            "scope": self.scope(),
            "canonicalization": "hushrun-source-v1",
            "source": {"vcs": "git"},
            "run": {"provider": "generic", "id": spec.run.clone().unwrap_or_else(|| format!("deploy-{index}")), "attempt": 1, "trigger": "push"},
            "producer": {"tool": "hushrun", "version": "0.1.0", "custody": "ci"},
            "time": at,
            "policyDigest": policy.digest,
            "deployed": {"artifacts": []},
        });
        if let Some(stage) = &spec.stage {
            value["stage"] = json!(stage);
        }
        let event: Event = serde_json::from_value(value).expect("deployed event");
        let mut verified = verified(1000 + index, event, digest, &spec.by);
        let segment = spec.stage.as_deref().unwrap_or("_");
        verified.key = format!("v1/deployed/{}/{segment}/{index:013}.json", self.scope());
        verified
    }

    /// Runs the planner.
    pub fn decide(&self) -> Decision {
        let policy = self.policy();
        plan(&self.input(&policy), &policy)
    }

    /// Every way the decision differs from `expect`.
    pub fn mismatches(&self, decision: &Decision) -> Vec<String> {
        let mut out = Vec::new();
        let e = &self.expect;
        if decision.mode.as_str() != e.mode {
            out.push(format!("mode {}, expected {}", decision.mode, e.mode));
        }
        if let Some(reason) = &e.reason
            && !decision.reason.contains(reason.as_str())
        {
            out.push(format!(
                "reason {:?} doesn't contain {reason:?}",
                decision.reason
            ));
        }
        if let Some(build) = e.build
            && decision.build != build
        {
            out.push(format!("build {}, expected {build}", decision.build));
        }
        if let Some(artifacts) = &e.artifacts {
            let got: BTreeMap<String, String> = decision
                .artifacts
                .iter()
                .map(|(n, a)| (n.to_string(), a.reference.clone()))
                .collect();
            let want: BTreeMap<String, String> = artifacts
                .iter()
                .map(|(n, r)| (n.clone(), reference(r)))
                .collect();
            if got != want {
                out.push(format!("artifacts {got:?}, expected {want:?}"));
            }
        }
        if let Some(checks) = &e.checks {
            let got: Vec<String> = decision
                .checks_to_run
                .iter()
                .map(|c| c.check.to_string())
                .collect();
            if &got != checks {
                out.push(format!("checks {got:?}, expected {checks:?}"));
            }
        }
        if let Some(requirements) = &e.requirements {
            for (requirement, status) in requirements {
                let got = decision
                    .requirements
                    .iter()
                    .find(|r| &r.requirement == requirement)
                    .map(|r| {
                        serde_json::to_value(r.status)
                            .expect("status")
                            .as_str()
                            .unwrap_or("")
                            .to_string()
                    });
                if got.as_deref() != Some(status.as_str()) {
                    out.push(format!(
                        "requirement {requirement} is {got:?}, expected {status}"
                    ));
                }
            }
        }
        out
    }
}

/// The signer a scenario's `by` names (T-15): `ci` and `ci-<anything>` are the repository's CI,
/// `agent-<name>` an agent's machine, and anything else a member's machine, `user:<by>@example.com`.
pub fn signer(by: &str) -> String {
    if by == CI_SIGNER || by.starts_with("ci-") {
        CI_SIGNER.to_string()
    } else if let Some(agent) = by.strip_prefix("agent-") {
        format!("agent:{agent}")
    } else {
        format!("user:{by}@example.com")
    }
}

/// A verified event signed by the signer `by` names, with effective custody capped by the
/// signer's (E-8 step 8).
pub fn verified(index: usize, event: Event, digest: Digest, by: &str) -> VerifiedEvent {
    let signer = signer(by);
    let max = Signers::max_custody(&signer);
    let key = format!(
        "v1/events/{}/{}/{index:026}.{}.{}.json",
        event.scope,
        digest.hex(),
        event.kind,
        event.stage.as_ref().map_or("_", |s| s.as_str())
    );
    VerifiedEvent {
        key,
        effective_custody: event.producer.custody.min(max),
        signer,
        digest,
        event,
        trusted_time: None,
    }
}
