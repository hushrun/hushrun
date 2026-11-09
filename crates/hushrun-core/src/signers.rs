//! Who counts (§7.2, T-15, ADR 0031): the signers whose events count for a repository, as its
//! server names them. Membership is the policy: a machine registered at sign-in (ADR 0028) of a
//! member of the org that owns the repository, or of one of the org's agent tokens, and the
//! repository's own CI, signing keylessly. Nothing is read from the repository: the server's
//! answer, `GET {repository_url}/signers` (L-13), is the whole list.
//!
//! This module parses that answer and matches signers against it; verifying signatures,
//! certificates and timestamps is `hushrun-attest`'s job, and checking the answer's statement
//! against the log key is the ledger client's.

mod ci_only;
mod keys;

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::digest::Digest;
use crate::run::Custody;
use crate::time::Time;

pub use ci_only::{CiOnly, CiOnlyEntry, CiOnlyStage, Period};
pub use keys::{CertIdentity, PULL_REQUEST_TARGET, PublicKey, Window};

/// The schema of the signers document (L-13).
pub const SIGNERS_SCHEMA: &str = "hushrun.signers/v1";

/// The signer label of the repository's CI (T-15): every keyless signature its CI makes counts
/// as this one signer.
pub const CI_SIGNER: &str = "ci";

/// GitHub Actions' OIDC issuer, as Fulcio certificates name it.
pub const GITHUB_ACTIONS_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// The repository's CI, as the server names it: certificates of this issuer whose Source
/// Repository Identifier is this id (T-15). Any workflow, on any ref, of that repository, but a
/// run that `pull_request_target` started, which runs the base branch's workflow for a change
/// that hasn't merged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiIdentity {
    /// The OIDC issuer, compared exactly.
    pub issuer: String,
    /// The repository's numeric id, compared exactly: a renamed repository keeps it, and a name
    /// taken over gets another.
    pub repository_id: String,
}

impl CiIdentity {
    /// Whether `cert` is a signature of this CI.
    pub fn matches(&self, cert: &CertIdentity) -> bool {
        cert.issuer == self.issuer
            && cert.repository_id.as_deref() == Some(self.repository_id.as_str())
            && cert.trigger.as_deref() != Some(PULL_REQUEST_TARGET)
    }
}

impl fmt::Display for CiIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{issuer: {}, repository_id: {}}}",
            self.issuer, self.repository_id
        )
    }
}

/// A machine whose key counts (T-15): registered at a sign-in (ADR 0028), by a person who is a
/// member of the org, or by one of the org's agent tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub key: PublicKey,
    /// The name it registered with, such as a host name.
    pub name: String,
    /// Whose it is: `user:<email>` or `agent:<name>`. Its events count as this signer.
    pub owner: String,
    pub registered: Option<Time>,
    /// When it stopped counting: its sign-in ended, its owner left the org, its agent token was
    /// revoked, or someone revoked it. Signatures with a trusted time before it still count.
    pub revoked: Option<Time>,
    /// Whether its owner may sign GC statements (L-9): an admin or owner of the org.
    pub gc: bool,
}

impl Machine {
    /// When the machine may sign: until it was revoked (T-15, as a key's window, E-10).
    pub fn window(&self) -> Window {
        Window {
            not_before: None,
            not_after: self.revoked,
        }
    }
}

/// The signers whose events count for one repository (T-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signers {
    /// The repository they are for (C-2).
    pub repository: String,
    /// When the server answered.
    pub time: Option<Time>,
    /// The repository's CI.
    pub ci: Vec<CiIdentity>,
    /// Machines, by key id.
    pub machines: BTreeMap<String, Machine>,
    /// The stages that take evidence from the repository's CI only (T-16), as the org's admins
    /// set them on the server.
    pub ci_only: CiOnly,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawSigners {
    schema: String,
    repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    time: Option<Time>,
    #[serde(default)]
    ci: Vec<CiIdentity>,
    #[serde(default)]
    machines: Vec<RawMachine>,
    #[serde(default, skip_serializing_if = "CiOnly::is_empty")]
    ci_only: CiOnly,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawMachine {
    keyid: String,
    key: String,
    name: String,
    owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registered: Option<Time>,
    #[serde(default)]
    revoked: Option<Time>,
    #[serde(default)]
    gc: bool,
}

impl Signers {
    /// Signers with no machine and no CI: nothing counts.
    pub fn none(repository: &str) -> Signers {
        Signers {
            repository: repository.to_string(),
            time: None,
            ci: Vec::new(),
            machines: BTreeMap::new(),
            ci_only: CiOnly::none(),
        }
    }

    /// Parses the signers document (L-13).
    pub fn from_json(value: &Value) -> Result<Signers, String> {
        let raw: RawSigners = serde_json::from_value(value.clone())
            .map_err(|e| format!("not a {SIGNERS_SCHEMA} document: {e}"))?;
        if raw.schema != SIGNERS_SCHEMA {
            return Err(format!(
                "a {:?} document, and hushrun reads {SIGNERS_SCHEMA}",
                raw.schema
            ));
        }
        let mut machines = BTreeMap::new();
        for m in raw.machines {
            let key = PublicKey::parse(&m.key).map_err(|e| format!("machine {}: {e}", m.name))?;
            if key.keyid() != m.keyid {
                return Err(format!(
                    "machine {}: its key id is {}, and the document says {}",
                    m.name,
                    key.keyid(),
                    m.keyid
                ));
            }
            if !(m.owner.starts_with("user:") || m.owner.starts_with("agent:")) {
                return Err(format!(
                    "machine {}: its owner {:?} is neither user:<email> nor agent:<name>",
                    m.name, m.owner
                ));
            }
            if machines.contains_key(&m.keyid) {
                return Err(format!("the key {} is listed twice", m.keyid));
            }
            machines.insert(
                m.keyid,
                Machine {
                    key,
                    name: m.name,
                    owner: m.owner,
                    registered: m.registered,
                    revoked: m.revoked,
                    gc: m.gc,
                },
            );
        }
        Ok(Signers {
            repository: raw.repository,
            time: raw.time,
            ci: raw.ci,
            machines,
            ci_only: raw.ci_only,
        })
    }

    /// Parses the signers document from bytes.
    pub fn parse(bytes: &[u8]) -> Result<Signers, String> {
        let value: Value = serde_json::from_slice(bytes).map_err(|e| format!("not JSON: {e}"))?;
        Signers::from_json(&value)
    }

    /// The document (L-13), as the server writes it and a reader saves it.
    pub fn to_json(&self) -> Value {
        let raw = RawSigners {
            schema: SIGNERS_SCHEMA.to_string(),
            repository: self.repository.clone(),
            time: self.time,
            ci: self.ci.clone(),
            machines: self
                .machines
                .iter()
                .map(|(keyid, m)| RawMachine {
                    keyid: keyid.clone(),
                    key: m.key.to_string(),
                    name: m.name.clone(),
                    owner: m.owner.clone(),
                    registered: m.registered,
                    revoked: m.revoked,
                    gc: m.gc,
                })
                .collect(),
            ci_only: self.ci_only.clone(),
        };
        serde_json::to_value(raw).expect("serializes")
    }

    /// `sha256:` of the document's JCS form: what the server's statement signs (L-13).
    pub fn digest_of(document: &Value) -> Result<Digest, String> {
        crate::jcs::digest(document).map_err(|e| e.to_string())
    }

    /// The machine whose key id is `keyid`, revoked or not.
    pub fn machine(&self, keyid: &str) -> Option<&Machine> {
        self.machines.get(keyid)
    }

    /// The repository's CI identity that `cert` matches, if any.
    pub fn ci_matching(&self, cert: &CertIdentity) -> Option<&CiIdentity> {
        self.ci.iter().find(|ci| ci.matches(cert))
    }

    /// The highest custody a signer may claim (E-8 step 8): `ci` for the repository's CI, and
    /// `isolated` for a machine, whose runs on a laptop or in a sandbox are never CI's.
    pub fn max_custody(signer: &str) -> Custody {
        if signer == CI_SIGNER {
            Custody::Ci
        } else {
            Custody::Isolated
        }
    }

    /// A summary, for `doctor` and messages: how many machines count now, and the CI.
    pub fn summary(&self) -> String {
        let counting = self
            .machines
            .values()
            .filter(|m| m.revoked.is_none())
            .count();
        let ci: Vec<String> = self
            .ci
            .iter()
            .map(|ci| {
                if ci.issuer == GITHUB_ACTIONS_ISSUER {
                    format!("GitHub Actions of repository id {}", ci.repository_id)
                } else {
                    format!("{} of repository id {}", ci.issuer, ci.repository_id)
                }
            })
            .collect();
        let mut summary = format!(
            "{counting} machine{} of members and agents, and {}",
            if counting == 1 { "" } else { "s" },
            if ci.is_empty() {
                "no CI yet (it counts once the server knows the repository's id)".to_string()
            } else {
                format!("CI: {}", ci.join(", "))
            }
        );
        let stages: Vec<&str> = self.ci_only.current().map(|(s, _)| s.as_str()).collect();
        if self.ci_only.stageless().last().is_some_and(|p| p.is_open()) {
            summary.push_str("; CI only");
        } else if !stages.is_empty() {
            summary.push_str(&format!("; CI only at {}", stages.join(", ")));
        }
        summary
    }

    /// A one-machine document for tests and examples.
    pub fn example(repository: &str, keys: &[(&PublicKey, &str)]) -> Value {
        json!({
            "schema": SIGNERS_SCHEMA,
            "repository": repository,
            "ci": [],
            "machines": keys.iter().map(|(key, owner)| json!({
                "keyid": key.keyid(),
                "key": key.to_string(),
                "name": owner.split(':').nth(1).unwrap_or(owner),
                "owner": owner,
                "revoked": null,
                "gc": false,
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> PublicKey {
        PublicKey::Ed25519([n; 32])
    }

    fn cert(trigger: &str) -> CertIdentity {
        CertIdentity {
            issuer: GITHUB_ACTIONS_ISSUER.into(),
            subject: "https://github.com/acme/back/.github/workflows/ci.yml@refs/heads/feature"
                .into(),
            repository_id: Some("123456".into()),
            repository: Some("https://github.com/acme/back".into()),
            reference: Some("refs/heads/feature".into()),
            workflow: None,
            signer_workflow: None,
            trigger: Some(trigger.into()),
        }
    }

    // covers: T-15
    #[test]
    fn parses_the_servers_document() {
        let doc = json!({
            "schema": SIGNERS_SCHEMA,
            "repository": "github.com/acme/back",
            "time": "2026-10-22T10:00:00Z",
            "ci": [{"issuer": GITHUB_ACTIONS_ISSUER, "repository_id": "123456"}],
            "machines": [
                {"keyid": key(1).keyid(), "key": key(1).to_string(), "name": "ana-mbp",
                 "owner": "user:ana@acme.dev", "registered": "2026-10-01T00:00:00Z",
                 "revoked": null, "gc": true},
                {"keyid": key(2).keyid(), "key": key(2).to_string(), "name": "fleet",
                 "owner": "agent:fleet-a", "revoked": "2026-10-20T00:00:00Z"},
            ],
            "ci_only": [
                {"stage": "canary", "periods": [
                    {"from": "2026-10-01T00:00:00Z", "until": "2026-10-02T00:00:00Z"}
                ]},
                {"stage": "production", "periods": [{"from": "2026-10-21T09:00:00Z"}]},
            ],
        });
        let signers = Signers::from_json(&doc).unwrap();
        assert_eq!(signers.machines.len(), 2);
        let ana = signers.machine(&key(1).keyid()).unwrap();
        assert!(ana.gc && ana.revoked.is_none());
        let fleet = signers.machine(&key(2).keyid()).unwrap();
        assert!(
            !fleet
                .window()
                .contains(Time::parse("2026-10-21T00:00:00Z").unwrap())
        );
        assert!(
            fleet
                .window()
                .contains(Time::parse("2026-10-19T00:00:00Z").unwrap())
        );
        assert_eq!(Signers::from_json(&signers.to_json()).unwrap(), signers);
        assert_eq!(
            signers.summary(),
            "1 machine of members and agents, and CI: GitHub Actions of repository id 123456; CI only at production"
        );
        let production = crate::names::StageName::new("production").unwrap();
        assert_eq!(
            signers.ci_only.since(&production),
            Some(Time::parse("2026-10-21T09:00:00Z").unwrap())
        );
        // canary was CI-only once: it stays in the history, not in the summary.
        let canary = crate::names::StageName::new("canary").unwrap();
        assert_eq!(signers.ci_only.since(&canary), None);
        assert_eq!(signers.ci_only.periods(&canary).len(), 1);
        // Without CI-only stages the document leaves the list out.
        let mut none = signers.clone();
        none.ci_only = CiOnly::none();
        assert!(none.to_json().get("ci_only").is_none());
    }

    // covers: L-13, T-16
    #[test]
    fn parses_a_ci_only_list_without_stages() {
        let mut doc = Signers::example("github.com/acme/back", &[(&key(1), "user:ana@acme.dev")]);
        doc["ci_only"] = json!([
            {"from": "2026-10-01T00:00:00.000Z", "until": "2026-10-02T00:00:00.000Z"},
            {"from": "2026-10-21T09:00:00.000Z"},
        ]);
        let signers = Signers::from_json(&doc).unwrap();
        assert_eq!(
            signers.ci_only.since_anywhere(),
            Some(Time::parse("2026-10-21T09:00:00Z").unwrap())
        );
        assert!(!signers.ci_only.counts_anywhere("user:ana@acme.dev", None));
        assert!(
            signers.summary().ends_with("; CI only"),
            "{}",
            signers.summary()
        );
        assert_eq!(Signers::from_json(&signers.to_json()).unwrap(), signers);
        assert_eq!(signers.to_json()["ci_only"], doc["ci_only"]);
    }

    // covers: T-15
    #[test]
    fn refuses_documents_that_lie_about_keys() {
        let mut doc = Signers::example("github.com/acme/back", &[(&key(1), "user:a@b")]);
        doc["machines"][0]["keyid"] = json!(key(2).keyid());
        assert!(Signers::from_json(&doc).unwrap_err().contains("key id"));
        let mut doc = Signers::example("github.com/acme/back", &[(&key(1), "someone")]);
        assert!(Signers::from_json(&doc).unwrap_err().contains("owner"));
        doc["schema"] = json!("hushrun.signers/v9");
        assert!(Signers::from_json(&doc).is_err());
    }

    // covers: T-15
    #[test]
    fn ci_is_the_repositorys_workflows_but_pull_request_target() {
        let ci = CiIdentity {
            issuer: GITHUB_ACTIONS_ISSUER.into(),
            repository_id: "123456".into(),
        };
        assert!(ci.matches(&cert("push")));
        assert!(ci.matches(&cert("workflow_dispatch")));
        assert!(!ci.matches(&cert(PULL_REQUEST_TARGET)));
        let mut other = cert("push");
        other.repository_id = Some("999".into());
        assert!(!ci.matches(&other));
        let mut person = cert("push");
        person.issuer = "https://accounts.google.com".into();
        person.repository_id = None;
        assert!(!ci.matches(&person));
        assert_eq!(Signers::max_custody(CI_SIGNER), Custody::Ci);
        assert_eq!(Signers::max_custody("user:a@b"), Custody::Isolated);
    }
}
