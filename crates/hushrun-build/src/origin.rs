//! Where a block came from, and who vouches for it (B-32, B-34): the small record the store
//! keeps beside each block, `origin.json` in its folder. It is written before the block is
//! published, inside the folder that the publishing rename moves into place, and rewritten whole
//! (a temporary file, then a rename) under the block's lock when it changes ([`crate::store`]).
//!
//! | Field | Means |
//! |---|---|
//! | `built` | A compile here made it: the run, and (when the run had a backend) the backend's host, the org and a fingerprint of the credential. Absent: not built here |
//! | `vouched` | One entry per host, org and repository whose server said it serves this block to this machine: the scope, the digest of outputs and the server's date of the last time it said so. A download makes the first one |
//! | `reused` | Whether it has been reused here yet |
//!
//! A block with no record (every block from before this record existed) is reused locally as
//! before and never goes up. A block that was only downloaded never goes up either, and it is
//! reused without asking only while a server the run belongs to vouched for it within a day.

use hushrun_core::blocks::Hex32;
use serde::{Deserialize, Serialize};

/// The schema name in every `origin.json`.
pub const ORIGIN_SCHEMA: &str = "hushrun.build-origin/v1";

/// How long a server's word that it serves a downloaded block lets the block be reused without
/// asking again (B-34): a day.
pub const VOUCH_FRESH_MS: u64 = 24 * 60 * 60 * 1000;

/// Who a run with a backend was: the backend's host, the org, and a fingerprint of the
/// credential (which person's token, which agent token, a CI job; never the secret). A marker
/// goes up only from a run with the same three (B-29).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity {
    /// The backend's origin, `https://api.example.com`.
    pub host: String,
    /// The org, as in the backend's URL.
    pub org: String,
    /// The credential's fingerprint ([`fingerprint`]).
    pub credential: String,
}

/// A compile here made the block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Built {
    /// The run that built it: its session's name.
    pub run: String,
    /// Who the run was, when it had a backend. A run with none records none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Identity>,
}

/// A server's word that it serves the block to this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vouch {
    /// The backend's origin.
    pub host: String,
    /// The org.
    pub org: String,
    /// The run's repository the server answered for; empty when the run named none.
    pub repository: String,
    /// The scope it serves the block from: where a report about it goes.
    pub scope: String,
    /// The digest of outputs the server's row holds.
    pub outputs: Hex32,
    /// When the server last said so, in milliseconds since the epoch, by the server's clock.
    pub date_ms: u64,
}

/// `origin.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// [`ORIGIN_SCHEMA`].
    pub schema: String,
    /// A compile here made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built: Option<Built>,
    /// Who vouches for it, one entry per host, org and repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vouched: Vec<Vouch>,
    /// Whether it has been reused here yet.
    #[serde(default)]
    pub reused: bool,
}

impl Origin {
    /// The record of a block a compile of run `run` made, by `by` when the run had a backend.
    pub fn built_here(run: &str, by: Option<Identity>) -> Origin {
        Origin {
            schema: ORIGIN_SCHEMA.to_string(),
            built: Some(Built {
                run: run.to_string(),
                by,
            }),
            vouched: Vec::new(),
            reused: false,
        }
    }

    /// The record of a block that was downloaded, with the server's word for it.
    pub fn downloaded(vouch: Vouch) -> Origin {
        Origin {
            schema: ORIGIN_SCHEMA.to_string(),
            built: None,
            vouched: vec![vouch],
            reused: false,
        }
    }

    /// Whether the block was only downloaded: no compile here made it.
    pub fn downloaded_only(&self) -> bool {
        self.built.is_none() && !self.vouched.is_empty()
    }

    /// The entry of `host`, `org` and `repository`, when there is one.
    pub fn vouch(&self, host: &str, org: &str, repository: &str) -> Option<&Vouch> {
        self.vouched
            .iter()
            .find(|v| v.host == host && v.org == org && v.repository == repository)
    }

    /// Whether a downloaded block may be reused without asking (B-34): its entry for this run's
    /// host, org and repository is under a day old at `now_ms`. A date ahead of `now_ms` (a
    /// clock behind the server's) counts as fresh.
    pub fn fresh(&self, host: &str, org: &str, repository: &str, now_ms: u64) -> bool {
        self.vouch(host, org, repository)
            .is_some_and(|v| now_ms.saturating_sub(v.date_ms) < VOUCH_FRESH_MS)
    }

    /// Records `vouch`, replacing the entry of the same host, org and repository.
    pub fn renew(&mut self, vouch: Vouch) {
        self.vouched.retain(|v| {
            !(v.host == vouch.host && v.org == vouch.org && v.repository == vouch.repository)
        });
        self.vouched.push(vouch);
        self.vouched.sort_by(|a, b| {
            (&a.host, &a.org, &a.repository).cmp(&(&b.host, &b.org, &b.repository))
        });
    }

    /// Whether a marker for this block may go up from a run that is `by` (B-29): only a block
    /// built here, by a run with the same host, org and credential.
    pub fn may_go_up_as(&self, by: &Identity) -> bool {
        self.built
            .as_ref()
            .and_then(|built| built.by.as_ref())
            .is_some_and(|built| built == by)
    }

    /// The scope a report about this block goes to, for a run on `host` and `org`: the scope of
    /// the entry for the run's repository, else of any entry of that host and org.
    pub fn report_scope(&self, host: &str, org: &str, repository: &str) -> Option<&str> {
        self.vouch(host, org, repository)
            .or_else(|| self.vouched.iter().find(|v| v.host == host && v.org == org))
            .map(|v| v.scope.as_str())
    }
}

/// A credential's fingerprint: the first 16 bytes of SHA-256 over a domain and the secret, in
/// hex. It tells two credentials apart and gives nothing of either.
pub fn fingerprint(secret: &str) -> String {
    let digest =
        hushrun_core::Digest::of(format!("hushrun.build-credential/v1\n{secret}").as_bytes());
    digest.hex()[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vouch(repository: &str, date_ms: u64) -> Vouch {
        Vouch {
            host: "https://api.example.com".into(),
            org: "acme".into(),
            repository: repository.into(),
            scope: "person/1".into(),
            outputs: Hex32([7; 32]),
            date_ms,
        }
    }

    fn me() -> Identity {
        Identity {
            host: "https://api.example.com".into(),
            org: "acme".into(),
            credential: fingerprint("hru_one"),
        }
    }

    // covers: B-32
    #[test]
    fn only_a_block_built_here_by_the_same_host_org_and_credential_goes_up() {
        let built = Origin::built_here("1-2", Some(me()));
        assert!(built.may_go_up_as(&me()));
        let other_org = Identity {
            org: "other".into(),
            ..me()
        };
        assert!(!built.may_go_up_as(&other_org));
        let other_credential = Identity {
            credential: fingerprint("hra_agent"),
            ..me()
        };
        assert!(!built.may_go_up_as(&other_credential));
        assert!(!Origin::built_here("1-2", None).may_go_up_as(&me()));
        let downloaded = Origin::downloaded(vouch("github.com/acme/shop", 0));
        assert!(downloaded.downloaded_only());
        assert!(!downloaded.may_go_up_as(&me()));
        assert_ne!(fingerprint("hru_one"), fingerprint("hru_two"));
        assert!(!fingerprint("hru_one").contains("hru_one"));
    }

    // covers: B-34
    #[test]
    fn a_download_is_fresh_for_a_day_for_its_host_org_and_repository() {
        let day = VOUCH_FRESH_MS;
        let mut origin = Origin::downloaded(vouch("github.com/acme/shop", 10 * day));
        let host = "https://api.example.com";
        assert!(origin.fresh(host, "acme", "github.com/acme/shop", 10 * day + day - 1));
        assert!(!origin.fresh(host, "acme", "github.com/acme/shop", 11 * day));
        assert!(!origin.fresh(host, "acme", "github.com/acme/other", 10 * day));
        assert!(!origin.fresh(host, "other", "github.com/acme/shop", 10 * day));
        origin.renew(vouch("github.com/acme/shop", 11 * day));
        assert_eq!(origin.vouched.len(), 1);
        assert!(origin.fresh(host, "acme", "github.com/acme/shop", 11 * day));
        origin.renew(vouch("github.com/acme/other", 11 * day));
        assert_eq!(origin.vouched.len(), 2);
        assert_eq!(
            origin.report_scope(host, "acme", "github.com/acme/none"),
            Some("person/1")
        );
    }
}
