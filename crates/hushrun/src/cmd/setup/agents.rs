//! What `hushrun init` writes for coding agents (C-24): a skill, and a block in AGENTS.md.
//!
//! Claude Code reads `.claude/skills`, and Codex `.agents/skills`; OpenCode and Cursor read both,
//! so these two copies reach all four. They are in the repository, so every clone and sandbox has
//! them, and `recipes/agents/` is the one place their text lives.

use std::path::Path;

/// The skill.
pub const SKILL: &str = include_str!("../../../../../recipes/agents/SKILL.md");
/// The block for AGENTS.md.
const BLOCK: &str = include_str!("../../../../../recipes/agents/AGENTS.md");

const SKILLS: [&str; 2] = [
    ".claude/skills/hushrun/SKILL.md",
    ".agents/skills/hushrun/SKILL.md",
];
const INSTRUCTIONS: &str = "AGENTS.md";
const BLOCK_START: &str = "<!-- hushrun:start";
const BLOCK_END: &str = "<!-- hushrun:end -->";

/// A file init writes for agents, and what it is for.
pub struct File {
    pub path: String,
    pub text: String,
    pub change: String,
}

/// What differs from the repository at `root`.
pub fn plan(root: &Path) -> Vec<File> {
    let read = |path: &str| std::fs::read_to_string(root.join(path)).ok();
    let mut files = Vec::new();
    for path in SKILLS {
        let found = read(path);
        if found.as_deref() != Some(SKILL) {
            files.push(File {
                path: path.to_string(),
                text: SKILL.to_string(),
                change: match found {
                    Some(_) => "updates hushrun's skill for coding agents".into(),
                    None => "hushrun's skill for coding agents".into(),
                },
            });
        }
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
            "hushrun init --yes --org <slug>",
            "hushrun hook install --warn",
            "hushrun run --force test -- vitest run",
        ] {
            assert!(SKILL.contains(says), "{says}");
        }
        // Nothing that ADR 0031 took away, nor ADR 0062.
        for gone in ["audit", "first try", "hushrun approve", "--stage"] {
            assert!(!SKILL.contains(gone), "{gone}");
            assert!(!BLOCK.contains(gone), "{gone}");
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
