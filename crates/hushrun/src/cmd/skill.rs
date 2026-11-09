//! `hushrun skill update` (C-29, \[ADR 0065\]): writes this hushrun's skill where agents read it,
//! in the repository or, with `--global`, in the person's home. What each copy is, and so
//! whether it is written, is [`crate::skill::write`]'s to decide.

use std::path::PathBuf;

use anyhow::anyhow;

use crate::cli::{GlobalOpts, SkillCommand, SkillUpdateArgs};
use crate::context;
use crate::exit::{CmdResult, Exit, Failure};
use crate::skill::{self, State, Written};

pub fn run(global: &GlobalOpts, command: &SkillCommand) -> CmdResult {
    match command {
        SkillCommand::Update(args) => update(global, args),
    }
}

fn update(global: &GlobalOpts, args: &SkillUpdateArgs) -> CmdResult {
    let (base, relatives, prefix) = match args.global {
        true => {
            let home = skill::home().ok_or_else(|| {
                Failure::new(
                    Exit::Unexpected,
                    anyhow!("can't tell where your home directory is"),
                )
            })?;
            (home, skill::PERSONAL, "~/")
        }
        false => {
            let root = repository_root(global).ok_or_else(|| {
                Failure::new(
                    Exit::Usage,
                    anyhow!(
                        "this isn't a git repository: run it in one, or use --global for your own copies"
                    ),
                )
            })?;
            (root, skill::IN_REPOSITORY, "")
        }
    };
    let update = crate::notice::update_command(crate::cmd::update::came_with_npm());
    for relative in relatives {
        let shown = format!("{prefix}{relative}");
        let written = skill::write(&base.join(relative), args.force)
            .map_err(|e| Failure::new(Exit::Unexpected, e.context(format!("writing {shown}"))))?;
        match written {
            Written::Wrote => println!("Wrote {shown}."),
            Written::Current => println!("{shown} is current."),
            Written::Left(State::Newer(n)) => eprintln!(
                "warning: left {shown}: a newer hushrun wrote it (skill revision {n}, this hushrun's is {}): update hushrun with `{update}`",
                skill::REVISION
            ),
            Written::Left(_) => {
                eprintln!("warning: left {shown}: it was edited by hand; --force replaces it");
            }
        }
    }
    Ok(Exit::Ok)
}

/// The root of the git repository hushrun runs in, if it runs in one.
pub(crate) fn repository_root(global: &GlobalOpts) -> Option<PathBuf> {
    let start = context::start_dir(global).ok()?;
    match hushrun_vcs::detect(&start) {
        Ok(hushrun_vcs::Detected::Git { root }) => Some(root),
        _ => None,
    }
}
