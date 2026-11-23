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

/// \[ADR 0106\] An agent cloud a team uses, whose files init writes (C-55).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cloud {
    ClaudeCode,
    Cursor,
    Copilot,
    Codex,
    Jules,
    Devin,
    Devcontainer,
}

impl Cloud {
    const ALL: [Cloud; 7] = [
        Cloud::ClaudeCode,
        Cloud::Cursor,
        Cloud::Copilot,
        Cloud::Codex,
        Cloud::Jules,
        Cloud::Devin,
        Cloud::Devcontainer,
    ];

    /// Its word, as `--agent-clouds` takes it.
    pub fn word(self) -> &'static str {
        match self {
            Cloud::ClaudeCode => "claude-code",
            Cloud::Cursor => "cursor",
            Cloud::Copilot => "copilot",
            Cloud::Codex => "codex",
            Cloud::Jules => "jules",
            Cloud::Devin => "devin",
            Cloud::Devcontainer => "devcontainer",
        }
    }

    fn parse(word: &str) -> Option<Cloud> {
        Cloud::ALL.into_iter().find(|cloud| cloud.word() == word)
    }
}

/// Where each cloud's own files say a team uses it: files the team wrote, never one init writes.
const SIGNS: [(Cloud, &str); 4] = [
    (Cloud::ClaudeCode, ".claude/settings.json"),
    (Cloud::Cursor, ".cursor"),
    (Cloud::Copilot, ".github/workflows/copilot-setup-steps.yml"),
    (Cloud::Devcontainer, ".devcontainer/devcontainer.json"),
];

/// The clouds the repository at `root` already has files for.
pub fn clouds_in(root: &Path) -> Vec<Cloud> {
    SIGNS
        .iter()
        .filter(|(_, path)| root.join(path).exists())
        .map(|(cloud, _)| *cloud)
        .collect()
}

/// `words`, read: a word hushrun doesn't know is a usage error; `none` is none.
pub fn parse_clouds(words: &[String]) -> Result<Vec<Cloud>, String> {
    let mut clouds = Vec::new();
    for word in words.iter().map(|w| w.trim().to_ascii_lowercase()) {
        match word.as_str() {
            "" | "none" => {}
            word => match Cloud::parse(word) {
                Some(cloud) if !clouds.contains(&cloud) => clouds.push(cloud),
                Some(_) => {}
                None => {
                    return Err(format!(
                        "--agent-clouds: {word:?} is none of {}, none",
                        Cloud::ALL.map(Cloud::word).join(", ")
                    ));
                }
            },
        }
    }
    clouds.sort();
    Ok(clouds)
}

/// \[ADR 0106\] The clouds init writes files for: `--agent-clouds`; else what the repository has
/// files for. A person at a terminal (no `--yes`, `--dry-run` or JSON) who named none is told how,
/// in one line: init asks nothing more of the terminal, whose answers its other questions read.
pub fn chosen(
    args: &crate::cli::InitArgs,
    root: &Path,
    json: bool,
) -> Result<Vec<Cloud>, crate::exit::Failure> {
    use std::io::IsTerminal as _;
    if let Some(words) = &args.agent_clouds {
        return parse_clouds(words).map_err(|e| crate::exit::Failure::usage(anyhow::anyhow!(e)));
    }
    let found = clouds_in(root);
    let terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if terminal && !args.yes && !args.dry_run && !json {
        eprintln!(
            "hushrun: which agent clouds does your team use? hushrun init --agent-clouds {} adds their hooks{}",
            Cloud::ALL.map(Cloud::word).join(","),
            if found.is_empty() {
                String::new()
            } else {
                format!(
                    "; this run writes {}'s, which this repository has files for",
                    found
                        .iter()
                        .map(|c| c.word())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
    }
    Ok(found)
}

/// What differs from the repository at `root`. \[ADR 0065\] A copy a newer hushrun wrote stays:
/// this hushrun's skill is the older one, and `ready` says to update hushrun. \[ADR 0106\] And
/// the files of the agent clouds `clouds`, each started through npx where `npm` (a package.json
/// holds hushrun), else from `PATH`.
pub fn plan(root: &Path, clouds: &[Cloud], npm: bool) -> Vec<File> {
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
    files.extend(cloud_files(root, clouds, npm));
    files
}

/// The command that starts hushrun in a hook.
fn runner(npm: bool) -> &'static str {
    if npm {
        "npx --no-install hushrun"
    } else {
        "hushrun"
    }
}

/// \[ADR 0106\] What each cloud gets (C-55): an end-of-session hook that runs `hushrun flush`
/// (Claude Code's `Stop` in `.claude/settings.json`, which also gets a `SessionStart` that pulls
/// the baked store in Claude Code on the web; Cursor's `stop` in `.cursor/hooks.json`; Copilot's
/// `sessionEnd` in `.github/hooks/hushrun.json`), Copilot's setup steps, and a dev container's
/// feature. A file there keeps everything it has: JSON gets hushrun's hook added where it lacks it;
/// a workflow or a dev container's file that exists is left as it is.
fn cloud_files(root: &Path, clouds: &[Cloud], npm: bool) -> Vec<File> {
    let run = runner(npm);
    let flush = format!("{run} flush -q");
    let mut files = Vec::new();
    let read = |path: &str| std::fs::read_to_string(root.join(path)).ok();
    let json_hook =
        |path: &str, add: &dyn Fn(&mut serde_json::Value) -> bool, change: &str| -> Option<File> {
            let found = read(path);
            let mut value = match found.as_deref() {
                Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(value) if value.is_object() => value,
                    // A file that doesn't read stays as it is.
                    _ => return None,
                },
                None => serde_json::json!({}),
            };
            add(&mut value).then(|| {
                let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
                text.push('\n');
                File {
                    path: path.to_string(),
                    text,
                    change: change.to_string(),
                }
            })
        };
    for cloud in clouds {
        match cloud {
            Cloud::ClaudeCode => {
                let pull =
                    format!("[ \"$CLAUDE_CODE_REMOTE\" != true ] || {run} build pull -q || true");
                files.extend(json_hook(
                    ".claude/settings.json",
                    &|value| {
                        let a = add_hook(value, &["hooks", "Stop"], || {
                            serde_json::json!({"hooks": [{"type": "command", "command": flush}]})
                        }, &flush);
                        let b = add_hook(value, &["hooks", "SessionStart"], || {
                            serde_json::json!({"hooks": [{"type": "command", "command": pull}]})
                        }, &pull);
                        a || b
                    },
                    "Claude Code's hooks: send what a session passed when it stops, and pull the baked store in a cloud session",
                ));
            }
            Cloud::Cursor => files.extend(json_hook(
                ".cursor/hooks.json",
                &|value| {
                    if value.get("version").is_none() {
                        value["version"] = serde_json::json!(1);
                    }
                    add_hook(
                        value,
                        &["hooks", "stop"],
                        || serde_json::json!({"command": flush}),
                        &flush,
                    )
                },
                "Cursor's stop hook: send what an agent passed when it stops",
            )),
            Cloud::Copilot => {
                files.extend(json_hook(
                    ".github/hooks/hushrun.json",
                    &|value| {
                        if value.get("version").is_none() {
                            value["version"] = serde_json::json!(1);
                        }
                        add_hook(value, &["hooks", "sessionEnd"], || {
                            serde_json::json!({"type": "command", "bash": flush, "timeoutSec": 60})
                        }, &flush)
                    },
                    "Copilot's sessionEnd hook: send what the agent passed when its session ends",
                ));
                let steps = ".github/workflows/copilot-setup-steps.yml";
                if read(steps).is_none() {
                    files.push(File {
                        path: steps.to_string(),
                        text: copilot_steps(npm),
                        change:
                            "Copilot's setup steps: a session for the agent, and the baked store"
                                .into(),
                    });
                }
            }
            Cloud::Devcontainer => {
                let path = ".devcontainer/devcontainer.json";
                if read(path).is_none() {
                    files.push(File {
                        path: path.to_string(),
                        text: DEVCONTAINER.to_string(),
                        change: "a dev container with hushrun's feature".into(),
                    });
                }
            }
            // Their setup lives in the platform's settings: the docs say what goes there.
            Cloud::Codex | Cloud::Jules | Cloud::Devin => {}
        }
    }
    files
}

/// Adds `entry` under `path` in `value` unless an entry there already holds `command`; whether it
/// added it.
fn add_hook(
    value: &mut serde_json::Value,
    path: &[&str],
    entry: impl Fn() -> serde_json::Value,
    command: &str,
) -> bool {
    let mut at = &mut *value;
    for key in path {
        if !at.get(*key).is_some_and(|v| v.is_object() || v.is_array()) {
            at[*key] = if *key == path[path.len() - 1] {
                serde_json::json!([])
            } else {
                serde_json::json!({})
            };
        }
        at = &mut at[*key];
    }
    let Some(list) = at.as_array_mut() else {
        return false;
    };
    if list.iter().any(|item| {
        item.to_string()
            .contains(&serde_json::to_string(command).unwrap_or_default()[1..])
    }) {
        return false;
    }
    list.push(entry());
    true
}

/// A dev container that installs hushrun with its feature (ADR 0107).
const DEVCONTAINER: &str = r#"{
  "name": "hushrun",
  "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
  "features": {
    "ghcr.io/hushrun/features/hushrun:1": {}
  }
}
"#;

/// Copilot's setup steps: install, a session for the agent (C-52) and the baked store (B-77).
fn copilot_steps(npm: bool) -> String {
    let (install, run) = if npm {
        ("      - run: npm ci\n", "npx --no-install hushrun")
    } else {
        ("      - uses: hushrun/hushrun/action/setup@v0\n", "hushrun")
    };
    format!(
        "name: Copilot setup steps\n\
         on: workflow_dispatch\n\
         jobs:\n\
         \x20 copilot-setup-steps:\n\
         \x20   runs-on: ubuntu-latest\n\
         \x20   permissions:\n\
         \x20     contents: read\n\
         \x20     id-token: write\n\
         \x20   steps:\n\
         \x20     - uses: actions/checkout@v5\n\
         {install}\
         \x20     - run: {run} login --session || true\n\
         \x20     - run: {run} build pull || true\n"
    )
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
        let planned = plan(dir.path(), &[], false);
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
        let planned = plan(dir.path(), &[], false);
        assert_eq!(
            planned
                .iter()
                .filter(|f| f.path.ends_with("SKILL.md"))
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            [SKILLS[0]]
        );
    }

    // covers: C-24 (ADR 0067: the skill is one text too, and init --local writes it)
    #[test]
    fn the_skill_holds_where_results_stay_on_this_machine() {
        let skill = SKILL.split_whitespace().collect::<Vec<_>>().join(" ");
        for gone in [
            // After init --local, and for a member who isn't signed in, CI skips nothing.
            "hushrun skips CI checks that this exact code already passed",
            "A pass here skips the same check in CI,",
            // In another language, a repository set up with --local names no backend.
            "when it has a `hushrun.yaml` that names a `backend`",
        ] {
            assert!(!skill.contains(gone), "{gone:?}");
        }
        for says in [
            "in CI too once the repository shares its results",
            "## Where results stay on this machine",
            "CI runs every check",
            "setting up with no account",
        ] {
            assert!(skill.contains(says), "{says:?}");
        }
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

    // covers: C-55
    #[test]
    fn each_cloud_gets_its_hooks_once_and_keeps_what_its_files_have() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(clouds_in(root).is_empty());
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(
            root.join(".claude/settings.json"),
            r#"{"permissions": {"allow": ["Bash(npm test)"]}}"#,
        )
        .unwrap();
        assert_eq!(clouds_in(root), [Cloud::ClaudeCode]);
        let clouds = parse_clouds(&[
            "copilot".into(),
            "claude-code".into(),
            "cursor".into(),
            "devcontainer".into(),
            "codex".into(),
        ])
        .unwrap();
        let files = cloud_files(root, &clouds, true);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".claude/settings.json",
                ".cursor/hooks.json",
                ".github/hooks/hushrun.json",
                ".github/workflows/copilot-setup-steps.yml",
                ".devcontainer/devcontainer.json",
            ]
        );
        let claude: serde_json::Value = serde_json::from_str(&files[0].text).unwrap();
        assert_eq!(claude["permissions"]["allow"][0], "Bash(npm test)");
        assert_eq!(
            claude["hooks"]["Stop"][0]["hooks"][0]["command"],
            "npx --no-install hushrun flush -q"
        );
        assert!(
            claude["hooks"]["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("CLAUDE_CODE_REMOTE")
        );
        let cursor: serde_json::Value = serde_json::from_str(&files[1].text).unwrap();
        assert_eq!(
            cursor["hooks"]["stop"][0]["command"],
            "npx --no-install hushrun flush -q"
        );
        let copilot: serde_json::Value = serde_json::from_str(&files[2].text).unwrap();
        assert_eq!(
            copilot["hooks"]["sessionEnd"][0]["bash"],
            "npx --no-install hushrun flush -q"
        );
        assert!(files[3].text.contains("id-token: write"));
        assert!(files[3].text.contains("login --session"));
        // Written, init has nothing more to add.
        for file in &files {
            let path = root.join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &file.text).unwrap();
        }
        assert!(cloud_files(root, &clouds, true).is_empty());
        assert!(parse_clouds(&["vscode".into()]).is_err());
        assert!(parse_clouds(&["none".into()]).unwrap().is_empty());
        // Without npm, hushrun from PATH.
        let dir = tempfile::tempdir().unwrap();
        let files = cloud_files(dir.path(), &[Cloud::Cursor], false);
        assert!(
            files[0].text.contains("\"hushrun flush -q\""),
            "{}",
            files[0].text
        );
    }
}
