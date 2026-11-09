//! `hushrun policy validate` and `hushrun policy explain`.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::anyhow;
use serde_json::json;

use hushrun_core::policy::{OnLedgerError, POLICY_FILE, Policy};
use hushrun_core::signers::Period;
use hushrun_core::time::Time;
use hushrun_vcs::Snapshot;

use crate::app::App;
use crate::cli::PolicyCommand;
use crate::context::{Content, Source, start_dir};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};

pub fn run(app: &App<'_>, command: &PolicyCommand) -> CmdResult {
    match command {
        PolicyCommand::Validate { rev } => validate(app, rev.as_deref()),
        PolicyCommand::Explain { signers } => explain(app, signers.as_deref()),
    }
}

/// Validates hushrun.yaml (the working tree's, `--rev`'s, or `--config`). Never touches the
/// repository's server (T-4).
fn validate(app: &App<'_>, rev: Option<&str>) -> CmdResult {
    let policy = match (rev, &app.global.config) {
        (None, None) => {
            let root = match hushrun_vcs::detect(&start_dir(app.global)?)? {
                hushrun_vcs::Detected::Git { root } => root,
                hushrun_vcs::Detected::None => start_dir(app.global)?,
            };
            let path = root.join(POLICY_FILE);
            let bytes = std::fs::read(&path)
                .map_err(|e| Failure::usage(anyhow!("reading {}: {e}", path.display())))?;
            let policy = Policy::parse(POLICY_FILE, &bytes)?;
            // \[ADR 0062\] What an older file says that this release reads past: valid, and
            // named, as `Content::open` names it for the other branch.
            for retired in &policy.retired {
                eprintln!("warning: {}", retired.notice(&policy.file));
            }
            policy
        }
        (rev, _) => {
            let snapshot = Snapshot::Rev(rev.unwrap_or("HEAD").to_string());
            let content = Content::open(app.global, Source::Repo(snapshot))?;
            content.policy()?.clone()
        }
    };
    if policy.ledger.is_some() && policy.transparency_log.is_none() {
        eprintln!(
            "warning: hushrun.yaml names a `repository_url:` and no `transparency_log`, so hushrun can't check the server's log: `hushrun doctor` prints the lines to add"
        );
    }
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.policy.validate/v1",
            "valid": true,
            "policyDigest": policy.digest,
            "retired": policy.retired.iter().map(|r| json!({
                "key": r.key, "line": r.pos.line, "column": r.pos.column, "instead": r.instead,
            })).collect::<Vec<_>>(),
        }))?,
        _ => output::print(&format!(
            "{POLICY_FILE} is valid: {} scope(s), {} check(s), {} artifact(s){}\n",
            policy.scopes.len(),
            policy.checks.len(),
            policy.artifacts.len(),
            if policy.deploy.is_some() {
                ", and it deploys"
            } else {
                ""
            }
        ))?,
    }
    Ok(Exit::Ok)
}

/// Whose events count (T-15), for people.
const SIGNERS_TEXT: &str = "members' machines, agents and the repository's CI";

/// \[ADR 0031 amendments 1 and 2, ADR 0062\] Whether the repository is CI-only (T-16), as the
/// signers answer says (L-13): the server's, or a saved one. `explain` reads hushrun.yaml offline,
/// so it shows the rest without the answer when there is no server to ask or it doesn't answer.
enum CiOnlyCheck {
    Checked {
        /// `server` or `file`.
        from: &'static str,
        /// The server's URL, or the file's path.
        source: String,
        /// Whether the answer's statement was checked against the log key hushrun.yaml pins.
        statement_checked: bool,
        /// Every period the repository was CI-only, oldest first.
        periods: Vec<Period>,
    },
    Unchecked {
        reason: String,
    },
}

impl CiOnlyCheck {
    /// Since when the repository has been CI-only, when it is now.
    fn since(&self) -> Option<Time> {
        match self {
            CiOnlyCheck::Checked { periods, .. } => {
                periods.last().filter(|p| p.is_open()).map(|p| p.from)
            }
            CiOnlyCheck::Unchecked { .. } => None,
        }
    }

    /// The periods that ended, oldest first.
    fn earlier(&self) -> Vec<Period> {
        match self {
            CiOnlyCheck::Checked { periods, .. } => {
                periods.iter().filter(|p| !p.is_open()).copied().collect()
            }
            CiOnlyCheck::Unchecked { .. } => Vec::new(),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            CiOnlyCheck::Checked {
                from,
                source,
                statement_checked,
                periods,
            } => json!({
                "checked": true,
                "from": from,
                "source": source,
                "statementChecked": statement_checked,
                "now": self.since().is_some(),
                "since": self.since(),
                "periods": periods,
            }),
            CiOnlyCheck::Unchecked { reason } => json!({
                "checked": false,
                "reason": reason,
            }),
        }
    }
}

/// The repository's CI-only periods: from `--signers FILE`, whose errors are the command's, as in
/// `verify`; else from the repository's server, when hushrun.yaml, `--repository-url` or the
/// environment names one. Not reaching it isn't an error: `explain` shows the rest.
fn ci_only_of(app: &App<'_>, policy: &Policy, file: Option<&Path>) -> Result<CiOnlyCheck, Failure> {
    if let Some(path) = file {
        let answer = crate::cmd::verify::saved_signers(policy, path)?;
        return Ok(CiOnlyCheck::Checked {
            from: "file",
            source: path.display().to_string(),
            statement_checked: answer.checked,
            periods: answer.signers.ci_only.any_periods(),
        });
    }
    let asked = app
        .ledger_for_explain(Some(policy))
        .and_then(|(url, ledger)| {
            let answer = app.signers_answer(policy, ledger.as_ref())?;
            Ok((url, answer))
        });
    Ok(match asked {
        Ok((url, answer)) => {
            if !answer.checked {
                tracing::warn!(
                    "hushrun.yaml names no `transparency_log`, so {url}'s signers answer can't be checked against the server's log key: it is taken as answered"
                );
            }
            CiOnlyCheck::Checked {
                from: "server",
                source: url.to_string(),
                statement_checked: answer.checked,
                periods: answer.signers.ci_only.any_periods(),
            }
        }
        Err(failure) => CiOnlyCheck::Unchecked {
            reason: format!("{:#}", failure.error)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        },
    })
}

fn explain(app: &App<'_>, signers: Option<&Path>) -> CmdResult {
    let content = Content::open(app.global, Source::Repo(Snapshot::default()))?;
    let policy = content.policy()?;
    let ci_only = ci_only_of(app, policy, signers)?;
    let accepts = if ci_only.since().is_some() {
        "ci"
    } else {
        "signers"
    };
    let requirements: Vec<_> = policy
        .required()
        .iter()
        .map(|name| {
            let check = &policy.checks[name];
            json!({
                "requirement": name,
                "definitionDigest": check.definition_digest,
                "maxAge": check.max_age.map(|d| d.to_string()),
                "accepts": accepts,
            })
        })
        .collect();
    let artifacts: Vec<_> = policy
        .artifacts
        .iter()
        .map(|(name, a)| {
            json!({
                "artifact": name,
                "scope": a.scope,
                "location": a.location,
            })
        })
        .collect();
    let deploy = policy.deploy.as_ref().map(|d| {
        json!({
            "branch": d.branch,
            "lease": d.lease.to_string(),
            "dedupe": d.dedupe,
            "allowForce": d.allow_force,
            "onRepositoryError": match d.on_ledger_error {
                OnLedgerError::Fail => "fail",
                OnLedgerError::Full => "full",
            },
        })
    });
    let value = json!({
        "schema": "hushrun.policy.explain/v2",
        "deploy": deploy,
        "requirements": requirements,
        "artifacts": artifacts,
        "ciOnly": ci_only.to_json(),
    });
    match app.global.format {
        Format::Json => output::print_json(&value)?,
        _ => output::print(&explain_text(policy, &value, &ci_only))?,
    }
    Ok(Exit::Ok)
}

fn explain_text(policy: &Policy, value: &serde_json::Value, ci_only: &CiOnlyCheck) -> String {
    let deploy = policy.deploy_or_default();
    let mut text = match (&policy.deploy, &deploy.branch) {
        (None, _) => {
            "hushrun.yaml has no `deploy`: these are the checks `hushrun run --all` runs\n"
                .to_string()
        }
        (Some(_), Some(branch)) => format!("deploys from {branch}\n"),
        (Some(_), None) => "deploys from any branch\n".to_string(),
    };
    text.push_str(&ci_only_text(ci_only));
    let _ = writeln!(text, "required checks:");
    for req in value["requirements"].as_array().into_iter().flatten() {
        let from = match (req["accepts"].as_str(), ci_only) {
            (Some("ci"), _) => "the repository's CI only",
            (_, CiOnlyCheck::Checked { .. }) => "members' machines, agents and CI",
            (_, CiOnlyCheck::Unchecked { .. }) => "the signers",
        };
        let _ = writeln!(
            text,
            "  {:<24} from {from}",
            req["requirement"].as_str().unwrap_or(""),
        );
    }
    if value["requirements"].as_array().is_none_or(Vec::is_empty) {
        text.push_str("  (none)\n");
    }
    if policy.deploy.is_some() {
        let _ = writeln!(
            text,
            "lease {}, dedupe on {}, force {}",
            deploy.lease,
            deploy
                .dedupe
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if deploy.allow_force {
                "allowed"
            } else {
                "disabled"
            }
        );
    }
    text
}

/// T-16 and T-15, for people: whether the repository is CI-only, since when and before, and whose
/// events count, or that it wasn't checked.
fn ci_only_text(ci_only: &CiOnlyCheck) -> String {
    let (from, source) = match ci_only {
        CiOnlyCheck::Unchecked { reason } => {
            return format!(
                "events count from {SIGNERS_TEXT}\nwhether the repository is CI only wasn't checked: {reason}\n"
            );
        }
        CiOnlyCheck::Checked { from, source, .. } => (*from, source),
    };
    let says = match from {
        "file" => format!("as {source} says"),
        _ => "as the repository's server says".to_string(),
    };
    let earlier: Vec<String> = ci_only
        .earlier()
        .iter()
        .filter_map(|p| Some(format!("from {} until {}", p.from, p.until?)))
        .collect();
    let mut text = match ci_only.since() {
        Some(since) => format!("CI only since {since}, {says}"),
        None if earlier.is_empty() => format!("not CI only, {says}"),
        None => format!("not CI only now, {says}"),
    };
    if !earlier.is_empty() {
        let _ = write!(
            text,
            "; earlier {}, and events made then count only from CI",
            earlier.join(", ")
        );
    }
    text.push('\n');
    if ci_only.since().is_some() {
        text.push_str("events count from the repository's CI only\n");
    } else {
        let _ = writeln!(text, "events count from {SIGNERS_TEXT}");
    }
    text
}
