//! `hushrun guide` (C-28, \[ADR 0065\]): what a coding agent needs to know about this hushrun,
//! beyond its skill, from the binary itself, so that it always says what this hushrun does; then
//! what is true here: a newer release the notice learned of (C-21), and each copy of the skill,
//! as [`crate::skill::state_of`] tells it. It asks nobody, and changes nothing.

use std::fmt::Write as _;

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::cli::GlobalOpts;
use crate::cmd::skill::repository_root;
use crate::exit::{CmdResult, Exit};
use crate::output::{self, Format};
use crate::skill::{self, Copy, State};

pub fn run(global: &GlobalOpts) -> CmdResult {
    let root = repository_root(global);
    let copies = skill::copies(root.as_deref(), skill::home().as_deref());
    let newer = crate::notice::known_newer();
    let npm = crate::cmd::update::came_with_npm();
    let current = crate::releases::current();
    match global.format {
        Format::Json => {
            let value = json!({
                "schema": "hushrun.guide/v1",
                "version": current.to_string(),
                "skillRevision": skill::REVISION,
                "guide": skill::GUIDE,
                "update": newer.as_ref().map(|latest| json!({
                    "latest": latest.to_string(),
                    "command": crate::notice::update_command(npm),
                })),
                "skills": copies.iter().map(|copy| json!({
                    "path": copy.shown,
                    "state": copy.state.word(),
                    "revision": copy.state.revision(),
                })).collect::<Vec<Value>>(),
            });
            output::print_json(&value).context("writing the guide")?;
        }
        _ => {
            let mut text = String::from(skill::GUIDE);
            let _ = writeln!(text, "\n## Here\n");
            let _ = writeln!(
                text,
                "This is hushrun {current}, with skill revision {}.",
                skill::REVISION
            );
            if let Some(latest) = &newer {
                let _ = writeln!(text, "{}", crate::notice::agent_line(latest, &current, npm));
            }
            if !copies.is_empty() {
                let _ = writeln!(text);
            }
            let hushrun = crate::cmd::ready::hushrun();
            let update = crate::notice::update_command(npm);
            for copy in &copies {
                let _ = writeln!(text, "- {}: {}", copy.shown, said(copy, hushrun, update));
            }
            output::print(&text).context("writing the guide")?;
        }
    }
    Ok(Exit::Ok)
}

/// What the text says of a copy.
fn said(copy: &Copy, hushrun: &str, update: &str) -> String {
    let global = if copy.personal { " --global" } else { "" };
    match copy.state {
        State::Current => "current".into(),
        State::Older(n) => {
            format!("older, revision {n}: `{hushrun} skill update{global}` updates it")
        }
        State::Newer(n) => format!("newer, revision {n}: update hushrun with `{update}`"),
        State::Edited => "edited by hand: hushrun leaves it".into(),
        State::Absent => "absent".into(),
    }
}
