//! `hushrun repository usage` (C-19, ADR 0032): the org's plan and this period's recorded checks
//! on the server `hushrun run` records results to here (C-11), against what the plan includes
//! and its limit, with this repository's share, and what it recorded for free, outside the plan.
//! It asks `GET {base}/v0/usage` (C-12), with the credential `hushrun run` sends there (C-13), and
//! nothing else.

use std::fmt::Write as _;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::App;
use crate::cmd::login::repository_root;
use crate::cmd::token::{now, when};
use crate::cmd::whoami::{named, named_by_key};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};
use crate::results::{Backend, Place, Selection, Usage};

const SCHEMA: &str = "hushrun.repository.usage/v1";

/// `GET {base}/v0/usage`'s answer (C-12). Only `used` is sure to be there: `included` and
/// `limit` are null for a plan without them, and a server may leave out what it doesn't track.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct Answer {
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    plan_name: Option<String>,
    #[serde(default)]
    period_start: Option<String>,
    #[serde(default)]
    period_end: Option<String>,
    used: u64,
    #[serde(default)]
    included: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    overage: Option<u64>,
    #[serde(default)]
    overage_cents: Option<u64>,
    #[serde(default)]
    state: Option<String>,
    /// Each repository's checks this period, most first; `""` for checks without one.
    #[serde(default)]
    repositories: Option<Vec<Share>>,
    /// What open source repositories recorded for free this period, most first: in no other
    /// number here. A server from before the field leaves it out.
    #[serde(default)]
    free: Option<Vec<Share>>,
    #[serde(default)]
    dashboard: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Share {
    #[serde(default)]
    repository: String,
    checks: u64,
}

impl Answer {
    /// The checks of `repository` this period.
    fn share(&self, repository: &str) -> u64 {
        checks_of(self.repositories.as_deref(), repository)
    }

    /// What `repository` recorded for free this period.
    fn free_share(&self, repository: &str) -> u64 {
        checks_of(self.free.as_deref(), repository)
    }

    /// What the org's repositories recorded for free this period.
    fn free_total(&self) -> u64 {
        self.free.iter().flatten().map(|share| share.checks).sum()
    }
}

/// The checks of `repository` in `shares`: its entries, ignoring case, as hosts and GitHub's names
/// do.
fn checks_of(shares: Option<&[Share]>, repository: &str) -> u64 {
    shares
        .unwrap_or_default()
        .iter()
        .filter(|share| share.repository.eq_ignore_ascii_case(repository))
        .map(|share| share.checks)
        .sum()
}

/// What the command found.
struct Report {
    /// Where results go; `None` when they are off, and why.
    place: Option<Place>,
    off: Option<String>,
    /// This repository, as `hushrun run`'s results name it (C-10).
    repository: Option<String>,
    /// The server's answer, when it reports usage.
    usage: Option<Answer>,
    /// The status of a server that doesn't report usage: 404 or 405.
    unreported: Option<u16>,
}

pub fn run(app: &App<'_>) -> CmdResult {
    let global = app.global;
    let repository = crate::cmd::wrap::results_repository(global);
    let mut report = Report {
        place: None,
        off: None,
        repository,
        usage: None,
        unreported: None,
    };
    // Results off: nothing is recorded, so nothing is metered, as `whoami` says.
    let backend = match Backend::select(repository_root(global).as_deref()) {
        Ok(Selection::On(backend)) => backend,
        Ok(Selection::Off(off)) => {
            report.off = Some(off.reason().to_string());
            return finish(app, &report);
        }
        Err(err) => {
            report.off = Some(format!("{err:#}"));
            return finish(app, &report);
        }
    };
    let place = backend.place();
    let answer = backend
        .usage()
        .map_err(|err| Failure::new(Exit::Ledger, err))?;
    match answer {
        Usage::Directory => {}
        Usage::Unreported(status) => report.unreported = Some(status),
        Usage::Reported(value) => {
            let base = match &place {
                Place::Server { base, .. } => base.as_str(),
                Place::Directory { .. } => "",
            };
            let usage: Answer = serde_json::from_value(value).map_err(|e| {
                Failure::new(
                    Exit::Ledger,
                    anyhow!("{base}: GET {base}/v0/usage didn't answer the org's usage: {e}"),
                )
            })?;
            report.usage = Some(usage);
        }
    }
    report.place = Some(place);
    finish(app, &report)
}

fn finish(app: &App<'_>, report: &Report) -> CmdResult {
    match app.global.format {
        Format::Json => output::print_json(&json_of(report))?,
        _ => output::print(&text(report, now(app.global)))?,
    }
    Ok(Exit::Ok)
}

fn text(report: &Report, now: i64) -> String {
    let mut out = String::new();
    let mut line = |label: &str, value: String| {
        let _ = writeln!(out, "{label:<11}{value}");
    };
    let Some(place) = &report.place else {
        line(
            "backend",
            format!(
                "none: results are off ({})",
                report.off.as_deref().unwrap_or("?")
            ),
        );
        line(
            "usage",
            "nothing is recorded, so nothing is metered".to_string(),
        );
        return out;
    };
    let (base, named_by) = match place {
        Place::Directory { dir, local } => {
            line(
                "backend",
                format!("this machine only, in {}", dir.display()),
            );
            line(
                "usage",
                match local.org() {
                    // \[ADR 0067\] The repository names an org, and nobody here is signed in to it.
                    Some(org) => format!(
                        "results stay on this machine, so nothing is metered: hushrun login shares them with {org}, whose plan then counts them"
                    ),
                    None => "results stay on this machine, so nothing is metered".to_string(),
                },
            );
            return out;
        }
        Place::Server { base, named_by, .. } => (base, *named_by),
    };
    line("backend", format!("{base} ({})", named(named_by)));
    let Some(usage) = &report.usage else {
        line(
            "usage",
            format!(
                "this server doesn't report usage: GET {base}/v0/usage answered {}",
                report.unreported.unwrap_or(404)
            ),
        );
        return out;
    };
    if let Some(org) = usage.org.clone().or_else(|| crate::hosted::org_of(base)) {
        line("org", org);
    }
    if let Some(plan) = usage.plan_name.as_ref().or(usage.plan.as_ref()) {
        line("plan", plan.clone());
    }
    match (&usage.period_start, &usage.period_end) {
        (Some(start), Some(end)) => line("period", format!("{} to {}", utc(start), when(end, now))),
        (None, Some(end)) => line("period", format!("until {}", when(end, now))),
        (Some(start), None) => line("period", format!("since {}", utc(start))),
        (None, None) => {}
    }
    line(
        "checks",
        match usage.included {
            Some(included) => format!(
                "{} recorded, of {} included",
                thousands(usage.used),
                thousands(included)
            ),
            None => format!("{} recorded", thousands(usage.used)),
        },
    );
    line(
        "limit",
        match usage.limit {
            Some(limit) if usage.used >= limit => format!(
                "{}, reached: checks still run, but their results aren't recorded until the period ends or the org upgrades",
                thousands(limit)
            ),
            Some(limit) => format!(
                "{}: past it, checks still run, but their results aren't recorded",
                thousands(limit)
            ),
            None => "none".to_string(),
        },
    );
    if let Some(overage) = usage.overage.filter(|n| *n > 0) {
        let cost = usage
            .overage_cents
            .filter(|c| *c > 0)
            .map(|c| format!(", {}", dollars(c)))
            .unwrap_or_default();
        line(
            "overage",
            format!("{} past what the plan includes{cost}", thousands(overage)),
        );
    }
    if let Some(repository) = &report.repository {
        let share = usage.share(repository);
        line(
            "repository",
            match (share, usage.used) {
                (0, _) => format!("{repository}: none of them"),
                (share, used) if used > 0 => format!(
                    "{repository}: {} of them ({}%)",
                    thousands(share),
                    (share * 100 + used / 2) / used
                ),
                (share, _) => format!("{repository}: {}", thousands(share)),
            },
        );
    }
    // Only when there are some, so an org without free checks sees what it always did.
    let (whose, free) = match &report.repository {
        Some(repository) => (format!("{repository}: "), usage.free_share(repository)),
        None => (String::new(), usage.free_total()),
    };
    if free > 0 {
        line(
            "free",
            format!(
                "{whose}{} recorded for free, outside the plan",
                thousands(free)
            ),
        );
    }
    if let Some(dashboard) = &usage.dashboard {
        line("dashboard", dashboard.clone());
    }
    out
}

fn json_of(report: &Report) -> Value {
    let (backend, named_by, directory) = match &report.place {
        None => (None, None, None),
        Some(Place::Directory { dir, .. }) => (None, None, Some(dir.display().to_string())),
        Some(Place::Server { base, named_by, .. }) => {
            (Some(base.clone()), Some(named_by_key(*named_by)), None)
        }
    };
    let usage = report.usage.as_ref().map(|usage| {
        let mut answer = json!({
            "org": usage.org,
            "plan": usage.plan,
            "plan_name": usage.plan_name,
            "period_start": usage.period_start,
            "period_end": usage.period_end,
            "used": usage.used,
            "included": usage.included,
            "limit": usage.limit,
            "overage": usage.overage,
            "overage_cents": usage.overage_cents,
            "state": usage.state,
            "repositories": usage.repositories.clone().unwrap_or_default(),
            "repository_checks": report.repository.as_deref().map(|r| usage.share(r)),
            "dashboard": usage.dashboard,
        });
        // Only when the server sent it: `[]` is a server with none, and no field is one that
        // doesn't say, so an older server's output stays what it was.
        if let Some(free) = &usage.free {
            answer["free"] = json!(free);
        }
        answer
    });
    json!({
        "schema": SCHEMA,
        "backend": backend,
        "named_by": named_by,
        "directory": directory,
        "off": report.off,
        "repository": report.repository,
        "usage": usage,
    })
}

/// An RFC 3339 time as `2026-10-01 00:00 UTC`; as given when it doesn't parse.
fn utc(time: &str) -> String {
    match time.parse::<jiff::Timestamp>() {
        Ok(at) => at.strftime("%Y-%m-%d %H:%M UTC").to_string(),
        Err(_) => time.to_string(),
    }
}

/// `1,234,567`.
pub(crate) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Cents as dollars, the currency of hushrun's plans: `$7.04`, `$1,500.00`.
fn dollars(cents: u64) -> String {
    format!("${}.{:02}", thousands(cents / 100), cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_like_people_write_them() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(2000), "2,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(dollars(704), "$7.04");
        assert_eq!(dollars(150_000), "$1,500.00");
        assert_eq!(utc("2026-10-01T00:00:00Z"), "2026-10-01 00:00 UTC");
        assert_eq!(utc("soon"), "soon");
    }

    // covers: C-19
    #[test]
    fn this_repositorys_share_ignores_case() {
        let answer: Answer = serde_json::from_value(json!({
            "used": 10,
            "repositories": [
                {"repository": "github.com/Acme/Shop", "checks": 6},
                {"repository": "github.com/acme/shop", "checks": 1},
                {"repository": "", "checks": 3},
            ],
        }))
        .unwrap();
        assert_eq!(answer.share("github.com/acme/shop"), 7);
        assert_eq!(answer.share("github.com/acme/other"), 0);
        // A server that names no repositories, or sends null.
        let bare: Answer =
            serde_json::from_value(json!({"used": 1, "repositories": null})).unwrap();
        assert_eq!(bare.share("github.com/acme/shop"), 0);
        assert!(serde_json::from_value::<Answer>(json!({"plan": "free"})).is_err());
    }

    // covers: C-19
    #[test]
    fn free_checks_add_up_by_repository_and_for_the_org() {
        let answer: Answer = serde_json::from_value(json!({
            "used": 0,
            "free": [
                {"repository": "github.com/Acme/OSS", "checks": 6},
                {"repository": "github.com/acme/oss", "checks": 1},
                {"repository": "github.com/acme/docs", "checks": 3},
            ],
        }))
        .unwrap();
        assert_eq!(answer.free_share("github.com/acme/oss"), 7);
        assert_eq!(answer.free_share("github.com/acme/other"), 0);
        assert_eq!(answer.free_total(), 10);
        // Neither is `repositories`' number.
        assert_eq!(answer.share("github.com/acme/oss"), 0);

        // An older server leaves it out; `null` reads the same.
        for answer in [json!({"used": 1}), json!({"used": 1, "free": null})] {
            let answer: Answer = serde_json::from_value(answer).unwrap();
            assert!(answer.free.is_none());
            assert_eq!(answer.free_share("github.com/acme/oss"), 0);
            assert_eq!(answer.free_total(), 0);
        }
    }
}
