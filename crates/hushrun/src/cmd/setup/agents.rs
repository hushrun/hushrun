//! What `hushrun init` writes for coding agents (C-24): a skill, and a block in AGENTS.md.
//!
//! Claude Code reads `.claude/skills`, and Codex `.agents/skills`; OpenCode and Cursor read both,
//! so these two copies reach all four. They are in the repository, so every clone and sandbox has
//! them, and `recipes/agents/` is the one place their text lives. \[ADR 0065\] What each copy is,
//! next to this hushrun's, is [`skill::state_of`]'s to say.

use std::path::Path;

use crate::skill::{self, State};

/// The skill.
pub const SKILL: &str = skill::SKILL;
/// The block for AGENTS.md.
const BLOCK: &str = include_str!("../../../../../recipes/agents/AGENTS.md");

const SKILLS: [&str; 2] = skill::IN_REPOSITORY;
const INSTRUCTIONS: &str = "AGENTS.md";
const BLOCK_START: &str = "<!-- hushrun:start";
const BLOCK_END: &str = "<!-- hushrun:end -->";

/// A file init writes for agents, and what it is for.
pub struct File {
    pub path: String,
    pub text: String,
    pub change: String,
}

/// What differs from the repository at `root`. \[ADR 0065\] A copy a newer hushrun wrote stays:
/// this hushrun's skill is the older one, and `ready` says to update hushrun.
pub fn plan(root: &Path) -> Vec<File> {
    let read = |path: &str| std::fs::read_to_string(root.join(path)).ok();
    let mut files = Vec::new();
    for path in SKILLS {
        let found = read(path);
        let change = match skill::state_of(found.as_deref()) {
            State::Current | State::Newer(_) => continue,
            State::Absent => "hushrun's skill for coding agents",
            State::Older(_) | State::Edited => "updates hushrun's skill for coding agents",
        };
        files.push(File {
            path: path.to_string(),
            text: SKILL.to_string(),
            change: change.into(),
        });
    }
    let found = read(INSTRUCTIONS);
    if let Some(text) = with_block(found.as_deref()) {
        files.push(File {
            path: INSTRUCTIONS.to_string(),
            text,
            change: match found {
                Some(found) if found.contains(BLOCK_START) => "updates its hushrun block".into(),
                Some(_) => "adds a hushrun block, and keeps the rest".into(),
                None => "a new file: what agents do before they push".into(),
            },
        });
    }
    files
}

/// AGENTS.md with hushrun's block, or `None` when it has it as it should be. An old block is
/// replaced where it is; a new one goes last.
fn with_block(existing: Option<&str>) -> Option<String> {
    let Some(text) = existing else {
        return Some(BLOCK.to_string());
    };
    if text.contains(BLOCK) {
        return None;
    }
    if let Some(start) = text.find(BLOCK_START)
        && let Some(end) = text[start..].find(BLOCK_END)
    {
        let end = start + end + BLOCK_END.len();
        let rest = text[end..].strip_prefix('\n').unwrap_or(&text[end..]);
        return Some(format!("{}{BLOCK}{rest}", &text[..start]));
    }
    let gap = match text {
        "" => "",
        text if text.ends_with("\n\n") => "",
        text if text.ends_with('\n') => "\n",
        _ => "\n\n",
    };
    Some(format!("{text}{gap}{BLOCK}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-24
    #[test]
    fn the_skill_is_one_every_agent_loads() {
        let (front, _) = SKILL
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---\n"))
            .expect("the skill starts with its frontmatter");
        let value = |key: &str| {
            front
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .map(str::trim)
                .unwrap_or_else(|| panic!("no {key}"))
        };
        // OpenCode's rules are the strictest: the directory's name, and 1024 characters.
        assert_eq!(value("name:"), "hushrun");
        let description = value("description:");
        assert!((1..=1024).contains(&description.chars().count()));
        for path in SKILLS {
            assert!(path.ends_with("/hushrun/SKILL.md"), "{path}");
        }
        // It says what the commands it names are really called.
        for says in [
            "hushrun ready",
            "exit code 10",
            "--no-verify",
            "hushrun init --yes",
            // [ADR 0065] The rest is the guide's, which the binary prints.
            "hushrun guide",
            "hushrun skill update",
        ] {
            assert!(SKILL.contains(says), "{says}");
        }
        for says in [
            "hushrun init --yes --org <slug>",
            "hushrun hook install --warn",
            "hushrun run --force test -- vitest run",
            "hushrun skill update --global",
            "HUSHRUN_NO_UPDATE_CHECK",
        ] {
            assert!(skill::GUIDE.contains(says), "{says}");
        }
        // The agentskills.io spec's fields only, which every agent takes, and a revision.
        for line in front.lines().filter(|line| !line.starts_with(' ')) {
            let key = line.split(':').next().unwrap();
            assert!(
                ["name", "description", "compatibility", "metadata"].contains(&key),
                "{line}"
            );
        }
        assert!(value("compatibility:").chars().count() <= 500);
        // A copy that comes from a marketplace says nothing untrue, such as who wrote it.
        assert!(!SKILL.contains("written-by"));
        // Nothing that ADR 0031 took away, nor ADR 0062.
        for gone in ["audit", "first try", "hushrun approve", "--stage"] {
            assert!(!SKILL.contains(gone), "{gone}");
            assert!(!skill::GUIDE.contains(gone), "{gone}");
            assert!(!BLOCK.contains(gone), "{gone}");
        }
    }

    // covers: C-24
    #[test]
    fn init_never_takes_a_skill_back_to_an_older_revision() {
        let dir = tempfile::tempdir().unwrap();
        let newer = SKILL.replacen(
            &format!("hushrun-skill: \"{}\"", crate::skill::REVISION),
            "hushrun-skill: \"999\"",
            1,
        );
        assert_ne!(newer, SKILL);
        for path in SKILLS {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &newer).unwrap();
        }
        // [ADR 0065] A teammate's newer hushrun wrote them: this one leaves them, and says so.
        let planned = plan(dir.path());
        assert!(
            planned.iter().all(|file| !file.path.ends_with("SKILL.md")),
            "{:?}",
            planned.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        // Any other copy that differs, older or edited by hand, is replaced, as before: init
        // shows the change and asks.
        let older =
            "---\nname: hushrun\ndescription: old\nmetadata:\n  written-by: hushrun init\n---\n";
        std::fs::write(dir.path().join(SKILLS[0]), older).unwrap();
        let planned = plan(dir.path());
        assert_eq!(
            planned
                .iter()
                .filter(|f| f.path.ends_with("SKILL.md"))
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            [SKILLS[0]]
        );
    }

    // covers: C-24 (ADR 0067: init writes one block, with or without --local, so it says
    // nothing that only holds where results are shared)
    #[test]
    fn the_block_holds_where_results_stay_on_this_machine() {
        let block = BLOCK.split_whitespace().collect::<Vec<_>>().join(" ");
        // After init --local, CI skips nothing.
        assert!(!block.contains("skips a check in CI when"), "{block}");
        for says in [
            "skips a check when this exact code already passed it: on this machine, and in CI once the repository shares its results",
            "which `hushrun ready` said CI still runs",
        ] {
            assert!(block.contains(says), "{says:?} in {block}");
        }
    }

    // covers: C-24
    #[test]
    fn agents_md_keeps_what_it_has() {
        assert_eq!(with_block(None).unwrap(), BLOCK);
        assert_eq!(with_block(Some("")).unwrap(), BLOCK);
        assert_eq!(
            with_block(Some("# Rules\n\nBe kind.\n")).unwrap(),
            format!("# Rules\n\nBe kind.\n\n{BLOCK}")
        );
        assert_eq!(
            with_block(Some("# Rules")).unwrap(),
            format!("# Rules\n\n{BLOCK}")
        );
        // Run again, nothing changes.
        let once = with_block(Some("# Rules\n")).unwrap();
        assert_eq!(with_block(Some(&once)), None);
        // An older block is replaced where it is.
        let old = format!("# Rules\n\n{BLOCK_START} -->\nold\n{BLOCK_END}\n\n## After\n");
        assert_eq!(
            with_block(Some(&old)).unwrap(),
            format!("# Rules\n\n{BLOCK}\n## After\n")
        );
    }
}
