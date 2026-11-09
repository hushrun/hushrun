//! `hushrun repository stats [--since D] [--local]` (L-14, ADR 0032): how much hushrun recorded,
//! for sharing volume without telemetry. It counts the keys of one listing of the repository's
//! events, whose names carry all it counts (L-2), and reads no event; with `--local`, this
//! machine's `hushrun run` results (C-11), and sends nothing at all. Nothing goes anywhere but
//! the screen, for a team to paste if it chooses.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::anyhow;
use serde::Deserialize;
use serde_json::json;

use hushrun_core::digest::Digest;
use hushrun_core::event::EventKind;
use hushrun_core::names::ScopeName;
use hushrun_core::time::{Dur, Time};
use hushrun_ledger::layout::{EventKey, parse_event_key};
use hushrun_ledger::{FileLedger, Ledger, ulid_millis};
use hushrun_vcs::Snapshot;

use crate::app::{App, ledger_failure};
use crate::context::{Content, Source};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};
use crate::results::{RESULT_SCHEMA, Verdict, plain};

const SCHEMA: &str = "hushrun.repository.stats/v2";
/// The labels' width in the text output.
const LABEL: usize = 17;

/// What is counted: what was recorded from `from` on.
struct Window {
    since: Dur,
    from: Time,
    until: Time,
}

impl Window {
    /// Whether something recorded at `ms` counts: a key's time is its writer's clock, so one a
    /// little ahead of this one counts too.
    fn contains(&self, ms: i64) -> bool {
        ms >= self.from.millis()
    }

    fn header(&self) -> String {
        format!(
            "in the last {} (since {})",
            self.since,
            self.from.timestamp().strftime("%Y-%m-%d %H:%M UTC")
        )
    }
}

pub fn run(app: &App<'_>, since: Dur, local: bool) -> CmdResult {
    let from = app
        .now
        .checked_sub(since)
        .ok_or_else(|| Failure::usage(anyhow!("--since {since} reaches before 1970")))?;
    let window = Window {
        since,
        from,
        until: app.now,
    };
    if local {
        on_this_machine(app, &window)
    } else {
        on_the_server(app, &window)
    }
}

/// The repository's events, counted by kind and digest.
#[derive(Default)]
struct Events {
    total: u64,
    by_kind: BTreeMap<&'static str, u64>,
    /// Each (scope, digest) the events are about.
    digests: BTreeSet<(ScopeName, Digest)>,
}

impl Events {
    fn add(&mut self, event: &EventKey) {
        self.total += 1;
        *self.by_kind.entry(event.kind.as_str()).or_default() += 1;
        self.digests.insert((event.scope.clone(), event.digest));
    }
}

fn on_the_server(app: &App<'_>, window: &Window) -> CmdResult {
    let content = Content::open(app.global, Source::Repo(Snapshot::default()))?;
    let (url, ledger) = app.ledger(content.policy.as_ref())?;
    // One listing, and no event read: each key names its event's scope, digest and kind and, in
    // its ULID, when it was written (L-2).
    let keys = ledger.list("v1/events/").map_err(ledger_failure)?;
    let mut events = Events::default();
    for key in &keys {
        let Some(event) = parse_event_key(key) else {
            tracing::debug!("{key} isn't an event key");
            continue;
        };
        if ulid_millis(&event.ulid).is_some_and(|ms| window.contains(ms)) {
            events.add(&event);
        }
    }
    let by_kind: BTreeMap<&str, u64> = EventKind::ALL
        .iter()
        .map(|kind| {
            let n = events.by_kind.get(kind.as_str()).copied().unwrap_or(0);
            (kind.as_str(), n)
        })
        .collect();
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": SCHEMA,
            "source": "repository",
            "repository": url.to_string(),
            "since": window.since.to_string(),
            "from": window.from,
            "until": window.until,
            "events": events.total,
            "byKind": by_kind,
            "digests": events.digests.len(),
        }))?,
        _ => {
            let mut text = format!("events {}, at {url}\n", window.header());
            let mut line = |label: &str, value: String| {
                let _ = writeln!(text, "{label:<LABEL$}{value}");
            };
            line("total", events.total.to_string());
            line(
                "by kind",
                listed(EventKind::ALL.iter().map(|kind| {
                    (
                        kind.as_str().to_string(),
                        events.by_kind.get(kind.as_str()).copied().unwrap_or(0),
                    )
                })),
            );
            line("digests", events.digests.len().to_string());
            output::print(&text)?;
        }
    }
    Ok(Exit::Ok)
}

/// What `hushrun run` keeps of a run, as far as counting goes (C-10).
#[derive(Deserialize)]
struct Counted {
    schema: String,
    check: String,
    result: Verdict,
    #[serde(default, rename = "savedMs")]
    saved_ms: Option<u64>,
}

/// This machine's results, counted by verdict, run key and check.
#[derive(Default)]
struct Results {
    total: u64,
    pass: u64,
    fail: u64,
    skip: u64,
    keys: BTreeSet<String>,
    by_check: BTreeMap<String, u64>,
    saved_ms: u64,
}

impl Results {
    fn add(&mut self, key: &str, result: &Counted) {
        self.total += 1;
        match result.result {
            Verdict::Pass => self.pass += 1,
            Verdict::Fail => self.fail += 1,
            Verdict::Skip => {
                self.skip += 1;
                self.saved_ms = self.saved_ms.saturating_add(result.saved_ms.unwrap_or(0));
            }
        }
        self.keys.insert(key.to_string());
        *self.by_check.entry(result.check.clone()).or_default() += 1;
    }
}

fn on_this_machine(app: &App<'_>, window: &Window) -> CmdResult {
    let dir = crate::context::cache_dir().map(|cache| cache.join("results"));
    let mut results = Results::default();
    if let Some(dir) = dir.as_ref().filter(|dir| dir.is_dir()) {
        let ledger = FileLedger::new(dir.clone());
        // `results/v0/<key>/<ULID>.json` for passes and fails, `skips/v0/…` for skips, each
        // ULID's time when the run finished (C-11).
        for prefix in ["results/v0/", "skips/v0/"] {
            let objects = ledger
                .list(prefix)
                .map_err(|e| Failure::new(Exit::Unexpected, plain(e)))?;
            for object in objects {
                let Some((key, file)) = object
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.split_once('/'))
                else {
                    continue;
                };
                let finished = file.strip_suffix(".json").and_then(ulid_millis);
                if !finished.is_some_and(|ms| window.contains(ms)) {
                    continue;
                }
                // A result that doesn't parse, or of another schema, counts as none (C-10).
                let counted = ledger
                    .get(&object)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Counted>(&bytes).ok())
                    .filter(|counted| counted.schema == RESULT_SCHEMA);
                match counted {
                    Some(counted) => results.add(key, &counted),
                    None => tracing::debug!("{object} isn't a result hushrun reads"),
                }
            }
        }
    }
    let directory = dir.as_ref().map(|d| d.display().to_string());
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": SCHEMA,
            "source": "local",
            "directory": directory,
            "since": window.since.to_string(),
            "from": window.from,
            "until": window.until,
            "results": results.total,
            "byVerdict": {"pass": results.pass, "fail": results.fail, "skip": results.skip},
            "keys": results.keys.len(),
            "byCheck": results.by_check,
            "savedMs": results.saved_ms,
        }))?,
        _ => {
            let place = match &directory {
                Some(dir) => format!("in {dir}"),
                None => "none: results are off (HUSHRUN_CACHE_DIR is empty)".to_string(),
            };
            let mut text = format!(
                "hushrun run's results on this machine {}, {place}\n",
                window.header()
            );
            let mut line = |label: &str, value: String| {
                let _ = writeln!(text, "{label:<LABEL$}{value}");
            };
            line("total", results.total.to_string());
            line(
                "by verdict",
                listed(
                    [
                        ("pass".to_string(), results.pass),
                        ("fail".to_string(), results.fail),
                        ("skip".to_string(), results.skip),
                    ]
                    .into_iter(),
                ),
            );
            line("run keys", results.keys.len().to_string());
            line("by check", listed(results.by_check.clone().into_iter()));
            line("saved by skips", crate::cmd::wrap::took(results.saved_ms));
            output::print(&text)?;
        }
    }
    Ok(Exit::Ok)
}

/// `check 8, started 2`: the counts that aren't zero, most first, the order given among equals;
/// `none` when every one is.
fn listed(counts: impl Iterator<Item = (String, u64)>) -> String {
    let mut counts: Vec<(String, u64)> = counts.filter(|(_, n)| *n > 0).collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_list_most_first_and_skip_zeros() {
        let counts = [
            ("started".to_string(), 2),
            ("check".to_string(), 8),
            ("artifact".to_string(), 0),
            ("passed".to_string(), 2),
        ];
        assert_eq!(listed(counts.into_iter()), "check 8, started 2, passed 2");
        assert_eq!(listed(std::iter::empty()), "none");
    }
}
