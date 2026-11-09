//! Custody, triggers and run identity (E-4, §10.4, §11.2).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// How protected a signing key is, from lowest to highest (§3). The order is meaningful:
/// `min_custody` and effective custody (E-8 step 8) compare with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Custody {
    Local,
    Isolated,
    Sandbox,
    Ci,
}

impl Custody {
    pub const ALL: [Custody; 4] = [
        Custody::Local,
        Custody::Isolated,
        Custody::Sandbox,
        Custody::Ci,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Custody::Local => "local",
            Custody::Isolated => "isolated",
            Custody::Sandbox => "sandbox",
            Custody::Ci => "ci",
        }
    }
}

impl fmt::Display for Custody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Custody {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Custody::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| {
                format!("unknown custody {s:?}: expected local, isolated, sandbox or ci")
            })
    }
}

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Push,
    Upstream,
    Manual,
    Schedule,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Push => "push",
            Trigger::Upstream => "upstream",
            Trigger::Manual => "manual",
            Trigger::Schedule => "schedule",
        }
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Trigger {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "push" => Ok(Trigger::Push),
            "upstream" => Ok(Trigger::Upstream),
            "manual" => Ok(Trigger::Manual),
            "schedule" => Ok(Trigger::Schedule),
            other => Err(format!(
                "unknown trigger {other:?}: expected push, upstream, manual or schedule"
            )),
        }
    }
}

/// A CI provider (§11.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Github,
    Gitlab,
    Buildkite,
    Circleci,
    Local,
    Generic,
}

impl Provider {
    pub const ALL: [Provider; 6] = [
        Provider::Github,
        Provider::Gitlab,
        Provider::Buildkite,
        Provider::Circleci,
        Provider::Local,
        Provider::Generic,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Github => "github",
            Provider::Gitlab => "gitlab",
            Provider::Buildkite => "buildkite",
            Provider::Circleci => "circleci",
            Provider::Local => "local",
            Provider::Generic => "generic",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Provider {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Provider::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| {
                format!(
                    "unknown provider {s:?}: expected github, gitlab, buildkite, circleci, generic or local"
                )
            })
    }
}

/// One attempt of one run: what groups lifecycle events (P-5).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RunRef {
    pub provider: Provider,
    pub id: String,
    pub attempt: u32,
}

impl fmt::Display for RunRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} run {} attempt {}",
            self.provider, self.id, self.attempt
        )
    }
}
