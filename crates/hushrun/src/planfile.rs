//! The plan as the single source (§10.4): `plan --out` and the `json` output hold the whole
//! plan, and later jobs carry it out with `--plan FILE` or `HUSHRUN_PLAN_JSON` instead of
//! deciding again.

use std::io::Read as _;

use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Serialize};

use hushrun_core::event::RunInfo;
use hushrun_core::names::ScopeName;
use hushrun_core::plan::Decision;
use hushrun_core::time::Time;
use hushrun_vcs::digest_scopes;

use crate::app::App;
use crate::context::{Content, Subject};
use crate::exit::Failure;

/// \[ADR 0062\] v2 has no stage. A plan file only travels between the jobs of one run, which
/// install one release, so a v1 file is refused rather than read.
pub const PLAN_SCHEMA: &str = "hushrun.plan/v2";

/// A plan (`hushrun.plan/v2`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFile {
    pub schema: String,
    pub repository: String,
    /// The revision the plan was made for; commands that carry it out check the checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub run: RunInfo,
    pub forced: bool,
    pub time: Time,
    pub scopes: Vec<Decision>,
}

impl PlanFile {
    /// The `--plan` file (`-` for stdin), else `HUSHRUN_PLAN_JSON`.
    pub fn load(app: &App<'_>) -> Result<Option<PlanFile>, Failure> {
        let text = match app.global.plan.as_deref() {
            Some("-") => {
                let mut text = String::new();
                std::io::stdin()
                    .read_to_string(&mut text)
                    .context("reading the plan from stdin")?;
                text
            }
            Some(path) => std::fs::read_to_string(path)
                .with_context(|| format!("reading the plan {path}"))
                .map_err(Failure::usage)?,
            None => match app.env.get("HUSHRUN_PLAN_JSON") {
                Some(json) => json.to_string(),
                None => return Ok(None),
            },
        };
        let schema = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("schema").and_then(|s| s.as_str()).map(str::to_string));
        if schema.as_deref() == Some("hushrun.plan/v1") {
            return Err(Failure::usage(anyhow!(
                "the plan is a hushrun.plan/v1 document, which a hushrun before 0.9 made for a stage; plan again with this release"
            )));
        }
        let plan: PlanFile = serde_json::from_str(&text).map_err(|e| {
            Failure::usage(anyhow!("the plan is not a {PLAN_SCHEMA} document: {e}"))
        })?;
        if plan.schema != PLAN_SCHEMA {
            return Err(Failure::usage(anyhow!(
                "the plan's schema is {:?}, not {PLAN_SCHEMA}",
                plan.schema
            )));
        }
        Ok(Some(plan))
    }

    /// Fails unless the checkout is at the plan's revision and its content is the plan's.
    pub fn check(&self, content: &Content) -> Result<(), Failure> {
        let here = content.listing.revision.revision.as_deref();
        if self.revision.as_deref() != here {
            return Err(Failure::usage(anyhow!(
                "the plan was made for revision {}, but the checkout is at {}; carry out a plan in the checkout it was made for",
                self.revision.as_deref().unwrap_or("(none)"),
                here.unwrap_or("(none)")
            )));
        }
        // \[ADR 0060\] A revision isn't its content: a plan made for the worktree, or before an
        // edit to it, has a digest of its own at the same commit.
        let mut scopes = Vec::new();
        for decision in &self.scopes {
            let scope = content.scopes.get(decision.scope.as_str()).ok_or_else(|| {
                Failure::usage(anyhow!(
                    "the plan has a scope `{}`, which this checkout's hushrun.yaml doesn't",
                    decision.scope
                ))
            })?;
            scopes.push(scope);
        }
        let digested = digest_scopes(
            content.vcs.as_ref(),
            &content.listing,
            &content.scopes,
            &scopes,
            false,
        )?;
        for (decision, here) in self.scopes.iter().zip(&digested.scopes) {
            if here.digest != decision.digest {
                return Err(Failure::usage(anyhow!(
                    "the plan was made for `{}` at {}, but here it digests to {}; carry out a plan on the content it was made for",
                    decision.scope,
                    decision.digest,
                    here.digest
                )));
            }
        }
        Ok(())
    }

    /// The decision for `scope`, or the only one.
    pub fn decision(&self, scope: Option<&ScopeName>) -> Result<&Decision, Failure> {
        match scope {
            Some(scope) => self
                .scopes
                .iter()
                .find(|d| &d.scope == scope)
                .ok_or_else(|| Failure::usage(anyhow!("the plan has no scope `{scope}`"))),
            None => match self.scopes.as_slice() {
                [only] => Ok(only),
                _ => Err(Failure::usage(anyhow!(
                    "the plan covers several scopes; pass --scope NAME"
                ))),
            },
        }
    }

    /// The subject of a decision: its scope and digest, with the content's source.
    pub fn subject(
        &self,
        scope: Option<&ScopeName>,
        content: &Content,
    ) -> Result<Subject, Failure> {
        let decision = self.decision(scope)?;
        Ok(Subject {
            scope: decision.scope.clone(),
            digest: decision.digest,
            source: content.source_info(),
        })
    }
}
