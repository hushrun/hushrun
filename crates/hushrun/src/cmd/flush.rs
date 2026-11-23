//! \[ADR 0106\] `hushrun flush` (C-53): sends what the outbox holds for the backend `hushrun run`
//! records to here, and says what it did. Run at the end of an agent's session, by the hooks
//! `hushrun init` writes, so what a session passed reaches the backend before the sandbox goes.

use crate::cli::{FetchArgs, GlobalOpts};
use crate::cmd::login::repository_root;
use crate::exit::{CmdResult, Exit};
use crate::output::{self, Format};
use crate::results::{Backend, Selection};

/// `1 result`, `3 results`.
pub fn results(n: usize) -> String {
    if n == 1 {
        "1 result".to_string()
    } else {
        format!("{n} results")
    }
}

pub fn run(global: &GlobalOpts) -> CmdResult {
    let root = repository_root(global);
    let flushed = match Backend::select(root.as_deref()) {
        Ok(Selection::On(backend)) => backend.flush(),
        _ => None,
    };
    let waiting = crate::outbox::directory()
        .map(|dir| {
            crate::outbox::entries(&dir)
                .iter()
                .filter(|entry| {
                    crate::outbox::decide(
                        entry.state,
                        entry.created_at,
                        crate::outbox::now_ms(),
                        None,
                    ) == crate::outbox::State::Pending
                })
                .count()
        })
        .unwrap_or(0);
    let flushed = flushed.unwrap_or_default();
    if global.format == Format::Json {
        output::print_json(&serde_json::json!({
            "schema": "hushrun.flush/v1",
            "sent": flushed.sent,
            "refused": flushed.refused,
            "expired": flushed.expired,
            "waiting": waiting,
        }))?;
        return Ok(Exit::Ok);
    }
    if global.quiet > 0 {
        return Ok(Exit::Ok);
    }
    let mut parts = Vec::new();
    if flushed.sent > 0 {
        parts.push(format!("sent {}", results(flushed.sent)));
    }
    if flushed.refused > 0 {
        parts.push(format!(
            "{} refused by the backend",
            results(flushed.refused)
        ));
    }
    if flushed.expired > 0 {
        parts.push(format!(
            "{} given up after 7 days",
            results(flushed.expired)
        ));
    }
    if waiting > 0 {
        parts.push(format!(
            "{} still waiting: the backend is out of reach, or they are for another",
            results(waiting)
        ));
    }
    if parts.is_empty() {
        eprintln!("hushrun: nothing waits to be sent");
    } else {
        eprintln!("hushrun: {}", parts.join("; "));
    }
    Ok(Exit::Ok)
}

/// \[ADR 0106\] `hushrun fetch [CHECK]...` (C-54): `hushrun ready`'s lookups of every check of
/// the worktree, each answer kept for 24 hours, then `hushrun build fetch` of the checks named.
/// What can't be fetched is a line, never a failure: the setup script goes on.
pub fn fetch(global: &GlobalOpts, args: &FetchArgs) -> CmdResult {
    crate::results::keep_fetched();
    let target = crate::cmd::ready::Target {
        snapshot: hushrun_vcs::Snapshot::Worktree,
        flag: Some("--worktree".to_string()),
        branch: None,
        scopes: None,
    };
    let checks = match crate::cmd::ready::evaluate(global, &target) {
        Ok(report) => Some(report.checks.len()),
        Err(failure) => {
            eprintln!("hushrun: fetch: no results: {:#}", failure.error);
            None
        }
    };
    let kept = crate::results::fetched_count();
    if let Some(checks) = checks {
        eprintln!(
            "hushrun: fetch: {kept} of {checks} checks have a newest result, kept here for 24 hours"
        );
    }
    // Compiled code: what build reuse fetches, where it shares.
    if let Err(failure) = crate::cmd::build::fetch_blocks(global, &args.checks) {
        eprintln!("hushrun: fetch: no compiled code: {:#}", failure.error);
    }
    Ok(Exit::Ok)
}
