//! `hushrun guide` (C-28, \[ADR 0065\]): what a coding agent needs to know about this hushrun,
//! beyond its skill, from the binary itself, so that it always says what this hushrun does; then
//! what is true here: a newer release the notice learned of (C-21), build reuse's mode and store
//! as `hushrun build status` reads them (B-21), and each copy of the skill, as
//! [`crate::skill::state_of`] tells it. It asks nobody, and changes nothing.

use std::fmt::Write as _;

use anyhow::Context as _;
use serde_json::{Value, json};

use hushrun_build::model::Mode;

use crate::cli::GlobalOpts;
use crate::cmd::build::Here;
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
    let build = Here::read(global);
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
                "build": build.as_ref().map(build_json),
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
            if let Some(build) = &build {
                for line in build_lines(build) {
                    let _ = writeln!(text, "{line}");
                }
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

/// What the text says of build reuse here: the lines of `hushrun build status` an agent needs,
/// the mode and the store.
fn build_lines(build: &Here) -> Vec<String> {
    let mut lines = Vec::new();
    let is = |mode: Mode| match mode {
        Mode::Observe => "in observe",
        mode => mode.as_str(),
    };
    match (&build.mode, build.saved) {
        (Ok(mode), None) => lines.push(format!(
            "Build reuse here is {}. The person's build settings: {}.",
            is(*mode),
            build.settings.display()
        )),
        (Ok(mode), Some(saved)) => lines.push(format!(
            "Build reuse here is {}: HUSHRUN_BUILD sets it in this shell, over the person's build settings ({}), which say {}.",
            is(*mode),
            build.settings.display(),
            saved.as_str()
        )),
        (Err(err), _) => lines.push(format!(
            "Build reuse here is unknown, so `hushrun run` reuses nothing: {err}."
        )),
    }
    lines.push(format!("Its store: {}.", build.store_words()));
    if build.in_ci {
        lines.push(
            "This is CI, where `hushrun run` reads no person's settings: `hushrun build status` says what it does here."
                .to_string(),
        );
    }
    lines
}

/// The same, as data.
fn build_json(build: &Here) -> Value {
    json!({
        "mode": build.mode.as_ref().ok().map(|mode| mode.as_str()),
        "savedMode": build.saved.map(|mode| mode.as_str()),
        "error": build.mode.as_ref().err(),
        "settings": build.settings,
        "store": build.store,
        "blocks": build.held.as_ref().map(|held| held.blocks),
        "bytes": build.held.as_ref().map(|held| held.bytes),
        "inCi": build.in_ci,
    })
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
