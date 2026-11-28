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

/// \[ADR 0142\] An agent cloud a team uses, whose files init writes (C-145).
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

/// \[ADR 0142\] The clouds init writes files for: `--agent-clouds`, and none without it (a
/// repository's own `.claude/settings.json` is no sign the team wants hooks there). A person at a terminal (no `--yes`, `--dry-run` or JSON) who named none is told how,
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
                    "; this repository has files for {}",
                    found
                        .iter()
                        .map(|c| c.word())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
    }
    Ok(Vec::new())
}

/// What differs from the repository at `root`. \[ADR 0065\] A copy a newer hushrun wrote stays:
/// this hushrun's skill is the older one, and `ready` says to update hushrun. \[ADR 0142\] And
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

/// \[ADR 0142\] What each cloud gets (C-145): an end-of-session hook that runs `hushrun flush`
/// (Claude Code's `Stop` in `.claude/settings.json`, which also gets a `SessionStart` that runs
/// `hushrun hook session-start` in Claude Code on the web, ADR 0161; Cursor's `stop` in `.cursor/hooks.json`; Copilot's
/// `sessionEnd` in `.github/hooks/hushrun.json`), Cursor's install script and the `install` of its
/// `.cursor/environment.json` (ADR 0164), Copilot's setup steps, and a dev container's
/// feature. A file there keeps everything it has: JSON gets hushrun's hook added where it lacks it;
/// a workflow or a dev container's file that exists is left as it is.
fn cloud_files(root: &Path, clouds: &[Cloud], npm: bool) -> Vec<File> {
    let run = runner(npm);
    let flush = format!("{run} flush -q || true");
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
                let start = session_start(run);
                let entry = serde_json::json!({
                    "type": "command",
                    "command": start,
                    "timeout": crate::cmd::session_start::HOOK_TIMEOUT_SECONDS,
                });
                let migrates = read(".claude/settings.json")
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                    .is_some_and(|mut value| replace_old_pull(&mut value, &entry, &start));
                files.extend(json_hook(
                    ".claude/settings.json",
                    &|value| {
                        let a = add_hook(value, &["hooks", "Stop"], || {
                            serde_json::json!({"hooks": [{"type": "command", "command": flush}]})
                        }, &flush);
                        // \[ADR 0161\] An earlier init's `build pull` gives way to this hook.
                        let b = replace_old_pull(value, &entry, &start);
                        let c = add_hook(value, &["hooks", "SessionStart"], || {
                            serde_json::json!({"hooks": [entry.clone()]})
                        }, &start);
                        a || b || c
                    },
                    if migrates {
                        "Claude Code's hooks: the session-start hook an earlier init wrote becomes `hook session-start`, which warms a cloud session and tells Claude what passed"
                    } else {
                        "Claude Code's hooks: send what a session passed when it stops, and warm a cloud session when it starts and tell Claude what passed"
                    },
                ));
            }
            Cloud::Cursor => {
                files.extend(json_hook(
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
                ));
                // \[ADR 0164\] What Cursor's Build runs before it snapshots the disk.
                files.extend(script_file(
                    read(CURSOR_SCRIPT).as_deref(),
                    &OLD_CURSOR_SCRIPTS,
                ));
                files.extend(json_hook(
                    ".cursor/environment.json",
                    &with_cursor_install,
                    "Cursor's environment: run that install script when Cursor builds the environment, after the install command already there",
                ));
            }
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

/// \[ADR 0164\] Where Cursor's install script goes, and the command `install` runs it with.
const CURSOR_SCRIPT: &str = ".cursor/hushrun-install.sh";
const CURSOR_INSTALL: &str = "sh .cursor/hushrun-install.sh";

/// \[ADR 0164\] Cursor's install script (C-145): installs hushrun when it is missing, pulls the
/// baked store into `/hushrun` as root, and fetches results as the user; it always exits 0.
/// `recipes/agent-clouds/` is the one place its text lives.
fn cursor_install_script() -> &'static str {
    include_str!("../../../../../recipes/agent-clouds/cursor-hushrun-install.sh")
}

/// \[ADR 0164\] The install scripts an earlier hushrun wrote, which init replaces: none yet. A
/// change to the script adds the text it replaces here.
const OLD_CURSOR_SCRIPTS: [&str; 0] = [];

/// What the install script in a repository is, next to this hushrun's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Absent,
    Current,
    /// One an earlier hushrun wrote, as it wrote it.
    Older,
    /// Anything else: a hand edit, which stays.
    Edited,
}

/// The one place that says what `found` is: missing, this hushrun's script, one of `older`, or
/// edited.
fn script_state(found: Option<&str>, older: &[&str]) -> Script {
    match found {
        None => Script::Absent,
        Some(text) if text == cursor_install_script() => Script::Current,
        Some(text) if older.contains(&text) => Script::Older,
        Some(_) => Script::Edited,
    }
}

/// The install script to write over `found`, or `None` when it stays as it is.
fn script_file(found: Option<&str>, older: &[&str]) -> Option<File> {
    let change = match script_state(found, older) {
        Script::Current | Script::Edited => return None,
        Script::Absent => {
            "Cursor's install script: pull the baked store and fetch results while Cursor builds the environment"
        }
        Script::Older => {
            "Cursor's install script: an older copy becomes the one that pulls the baked store and fetches results"
        }
    };
    Some(File {
        path: CURSOR_SCRIPT.to_string(),
        text: cursor_install_script().to_string(),
        change: change.to_string(),
    })
}

/// \[ADR 0164\] Points `.cursor/environment.json`'s `install` at the script, after the command
/// already there, joined with `&&` so that a failing dependency install still fails the Build.
/// An `install` that already names the script, or isn't a string, stays. Whether it changed.
fn with_cursor_install(value: &mut serde_json::Value) -> bool {
    let install = match value.get("install") {
        None => CURSOR_INSTALL.to_string(),
        Some(serde_json::Value::String(text)) if text.contains(CURSOR_SCRIPT) => return false,
        Some(serde_json::Value::String(text)) => {
            let text = text.trim_end_matches(|c: char| c.is_whitespace() || c == ';');
            if text.trim().is_empty() {
                CURSOR_INSTALL.to_string()
            } else {
                format!("{text} && {CURSOR_INSTALL}")
            }
        }
        Some(_) => return false,
    };
    value["install"] = serde_json::Value::String(install);
    true
}

/// \[ADR 0161\] Claude Code's `SessionStart` command (C-145): gated on Claude Code on the web, so
/// nothing starts elsewhere, and never failing the session.
pub(crate) fn session_start(run: &str) -> String {
    format!("[ \"$CLAUDE_CODE_REMOTE\" != true ] || {run} hook session-start || true")
}

/// \[ADR 0161\] The `SessionStart` commands an earlier init wrote (ADR 0143), for either runner.
fn old_pulls() -> [String; 2] {
    [runner(true), runner(false)]
        .map(|run| format!("[ \"$CLAUDE_CODE_REMOTE\" != true ] || {run} build pull -q || true"))
}

/// \[ADR 0161\] Puts `hook` where `SessionStart` holds an earlier init's `build pull`, and takes out
/// any other copy of it, an entry left with no hook going too; when a hook already runs `command`,
/// the old one only goes. Every other entry and field stays. Whether it changed anything.
fn replace_old_pull(
    value: &mut serde_json::Value,
    hook: &serde_json::Value,
    command: &str,
) -> bool {
    let old = old_pulls();
    let is_old = |item: &serde_json::Value| {
        item.get("command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| old.iter().any(|old| text.trim() == old))
    };
    let Some(list) = value
        .pointer_mut("/hooks/SessionStart")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    let mut placed = list.iter().any(|entry| {
        entry["hooks"].as_array().is_some_and(|hooks| {
            hooks.iter().any(|item| {
                item.get("command").and_then(serde_json::Value::as_str) == Some(command)
            })
        })
    });
    let mut changed = false;
    for entry in list.iter_mut() {
        let Some(hooks) = entry
            .get_mut("hooks")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        let mut kept = Vec::with_capacity(hooks.len());
        for item in hooks.drain(..) {
            if !is_old(&item) {
                kept.push(item);
                continue;
            }
            changed = true;
            if !placed {
                kept.push(hook.clone());
                placed = true;
            }
        }
        *hooks = kept;
    }
    if changed {
        list.retain(|entry| !entry["hooks"].as_array().is_some_and(Vec::is_empty));
    }
    changed
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

/// A dev container that installs hushrun with its feature (ADR 0143).
const DEVCONTAINER: &str = r#"{
  "name": "hushrun",
  "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
  "features": {
    "ghcr.io/hushrun/features/hushrun:1": {}
  }
}
"#;

/// Copilot's setup steps: install, a session for the agent (C-142) and the baked store (B-240).
fn copilot_steps(npm: bool) -> String {
    let (install, run) = if npm {
        ("      - run: npm ci || true\n", "npx --no-install hushrun")
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

    // covers: C-145
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
                ".cursor/hushrun-install.sh",
                ".cursor/environment.json",
                ".github/hooks/hushrun.json",
                ".github/workflows/copilot-setup-steps.yml",
                ".devcontainer/devcontainer.json",
            ]
        );
        let claude: serde_json::Value = serde_json::from_str(&files[0].text).unwrap();
        assert_eq!(claude["permissions"]["allow"][0], "Bash(npm test)");
        assert_eq!(
            claude["hooks"]["Stop"][0]["hooks"][0]["command"],
            "npx --no-install hushrun flush -q || true"
        );
        // ADR 0161: the one command, gated on Claude Code on the web, with a timeout for its
        // budget.
        assert_eq!(
            claude["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "[ \"$CLAUDE_CODE_REMOTE\" != true ] || npx --no-install hushrun hook session-start || true"
        );
        assert_eq!(
            claude["hooks"]["SessionStart"][0]["hooks"][0]["timeout"],
            crate::cmd::session_start::HOOK_TIMEOUT_SECONDS
        );
        const _: () = assert!(
            crate::cmd::session_start::BUDGET_SECONDS + 10
                <= crate::cmd::session_start::HOOK_TIMEOUT_SECONDS
        );
        let cursor: serde_json::Value = serde_json::from_str(&files[1].text).unwrap();
        assert_eq!(
            cursor["hooks"]["stop"][0]["command"],
            "npx --no-install hushrun flush -q || true"
        );
        // ADR 0164: the install script pulls before it fetches, as root through sudo with no
        // password, from PATH, and never fails the Build.
        let script = &files[2].text;
        let pull = script.find("build pull").unwrap();
        assert!(pull < script.find("hushrun fetch").unwrap(), "{script}");
        assert!(script.contains("sudo -n"), "{script}");
        assert!(
            script.contains("--preserve-env=HUSHRUN_REGISTRY_TOKEN"),
            "{script}"
        );
        assert!(script.trim_end().ends_with("exit 0"), "{script}");
        assert!(!script.contains("npx"), "{script}");
        let environment: serde_json::Value = serde_json::from_str(&files[3].text).unwrap();
        assert_eq!(
            environment,
            serde_json::json!({"install": "sh .cursor/hushrun-install.sh"})
        );
        let copilot: serde_json::Value = serde_json::from_str(&files[4].text).unwrap();
        assert_eq!(
            copilot["hooks"]["sessionEnd"][0]["bash"],
            "npx --no-install hushrun flush -q || true"
        );
        assert!(files[5].text.contains("id-token: write"));
        assert!(files[5].text.contains("login --session"));
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
        let without = cloud_files(dir.path(), &[Cloud::Cursor], false);
        assert!(
            without[0].text.contains("\"hushrun flush -q || true\""),
            "{}",
            without[0].text
        );
        // Only the stop hook follows npm: the script is the same either way.
        assert_eq!(without[1].path, ".cursor/hushrun-install.sh");
        assert_eq!(without[1].text, files[2].text);
        assert_eq!(without[2].text, files[3].text);
    }

    /// Cursor's files, written into `root` with `environment` as `.cursor/environment.json` (none
    /// when `None`), and what init plans for Cursor then.
    fn cursor_plan(environment: Option<&str>) -> (tempfile::TempDir, Vec<File>) {
        let dir = tempfile::tempdir().unwrap();
        if let Some(text) = environment {
            std::fs::create_dir_all(dir.path().join(".cursor")).unwrap();
            std::fs::write(dir.path().join(".cursor/environment.json"), text).unwrap();
        }
        let files = cloud_files(dir.path(), &[Cloud::Cursor], false);
        (dir, files)
    }

    /// `install` in the plan's `environment.json`, or `None` when the plan has no such file.
    fn planned_install(files: &[File]) -> Option<serde_json::Value> {
        let file = files
            .iter()
            .find(|f| f.path == ".cursor/environment.json")?;
        assert!(file.text.ends_with("}\n"), "{}", file.text);
        let value: serde_json::Value = serde_json::from_str(&file.text).unwrap();
        Some(value)
    }

    // covers: C-145
    #[test]
    fn cursors_install_runs_hushruns_script_after_the_command_already_there() {
        let (_dir, files) = cursor_plan(Some(
            r#"{"install": "pnpm install && ./custom_script.sh", "start": "sudo service docker start", "terminals": [{"name": "dev", "command": "pnpm dev"}]}"#,
        ));
        let value = planned_install(&files).unwrap();
        assert_eq!(
            value["install"],
            "pnpm install && ./custom_script.sh && sh .cursor/hushrun-install.sh"
        );
        assert_eq!(value["start"], "sudo service docker start");
        assert_eq!(value["terminals"][0]["command"], "pnpm dev");
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["install", "start", "terminals"],
            "keys keep their order"
        );
        assert!(
            files[2]
                .change
                .contains("after the install command already there"),
            "{}",
            files[2].change
        );
        // With no file, the recipe's bytes.
        let (_dir, files) = cursor_plan(None);
        assert_eq!(
            files[2].text,
            include_str!("../../../../../recipes/agent-clouds/cursor-environment.json")
        );
        // A trailing `;` or newline isn't left before `&&`.
        let (_dir, files) = cursor_plan(Some("{\"install\": \"npm ci; \\n\"}"));
        assert_eq!(
            planned_install(&files).unwrap()["install"],
            "npm ci && sh .cursor/hushrun-install.sh"
        );
        // An empty install, or none, is the script alone.
        for text in [
            r#"{"install": "  "}"#,
            r#"{"build": {"dockerfile": "Dockerfile"}}"#,
        ] {
            let (_dir, files) = cursor_plan(Some(text));
            let value = planned_install(&files).unwrap();
            assert_eq!(value["install"], "sh .cursor/hushrun-install.sh", "{text}");
        }
        // Already there, however it starts the script: nothing to add.
        for install in [
            "pnpm install && sh .cursor/hushrun-install.sh",
            "bash .cursor/hushrun-install.sh",
        ] {
            let (_dir, files) =
                cursor_plan(Some(&serde_json::json!({ "install": install }).to_string()));
            assert!(planned_install(&files).is_none(), "{install}");
            assert!(files.iter().any(|f| f.path == ".cursor/hushrun-install.sh"));
        }
        // Not a string, or not JSON: the file stays, and the script is still written.
        for text in [r#"{"install": ["pnpm install"]}"#, "{not json", "[]"] {
            let (_dir, files) = cursor_plan(Some(text));
            assert!(planned_install(&files).is_none(), "{text}");
            let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(
                paths,
                [".cursor/hooks.json", ".cursor/hushrun-install.sh"],
                "{text}"
            );
        }
    }

    // covers: C-145
    #[test]
    fn cursors_install_script_is_replaced_only_when_hushrun_wrote_it() {
        let current = cursor_install_script();
        assert_eq!(script_state(None, &[]), Script::Absent);
        assert_eq!(script_state(Some(current), &[]), Script::Current);
        assert_eq!(
            script_state(
                Some("#!/bin/sh\nhushrun build pull\n"),
                &["#!/bin/sh\nhushrun build pull\n"]
            ),
            Script::Older
        );
        assert_eq!(
            script_state(
                Some("#!/bin/sh\nmy own\n"),
                &["#!/bin/sh\nhushrun build pull\n"]
            ),
            Script::Edited
        );
        // A hand edit stays, and the rest of Cursor's files are still planned.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cursor")).unwrap();
        let edited = format!("{current}echo mine\n");
        std::fs::write(dir.path().join(".cursor/hushrun-install.sh"), &edited).unwrap();
        let files = cloud_files(dir.path(), &[Cloud::Cursor], false);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, [".cursor/hooks.json", ".cursor/environment.json"]);
        // Missing, it is written, and says what it does.
        let (_dir, files) = cursor_plan(None);
        assert_eq!(files[1].text, current);
        assert!(files[1].change.starts_with("Cursor's install script: pull"));
        // An older copy is replaced, and the plan says so.
        let older = script_file(Some("old"), &["old"]).unwrap();
        assert_eq!(older.text, current);
        assert!(older.change.contains("an older copy"), "{}", older.change);
        assert!(script_file(Some(current), &["old"]).is_none());
    }

    /// The install script, run by `sh` with `hushrun`, `id`, `sudo` and `curl` faked on `PATH`:
    /// what each was asked, a line each, and the script's exit code.
    #[cfg(unix)]
    fn run_install_script(
        uid: &str,
        sudo: bool,
        pull_fails: bool,
        lower: Option<&str>,
    ) -> (String, i32) {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = dir.path().join("log");
        let fake = |name: &str, body: &str| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let pull = if pull_fails { "exit 1" } else { "exit 0" };
        fake(
            "hushrun",
            &format!(
                "echo \"hushrun $* token=$HUSHRUN_REGISTRY_TOKEN\" >> \"$LOG\"\n[ \"$1\" = build ] && {pull}\nexit 0"
            ),
        );
        fake("id", &format!("echo {uid}"));
        // What sudo runs is looked up on PATH, which holds only the fakes.
        fake("true", "exit 0");
        fake("curl", "echo \"curl $*\" >> \"$LOG\"; exit 1");
        if sudo {
            // Records what it was asked, then runs the command after its options.
            fake(
                "sudo",
                "echo \"sudo $*\" >> \"$LOG\"\nwhile [ \"${1#-}\" != \"$1\" ]; do shift; done\nexec \"$@\"",
            );
        }
        let script = dir.path().join("hushrun-install.sh");
        std::fs::write(&script, cursor_install_script()).unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg(&script)
            .env_clear()
            // Only the fakes: a real sudo must never run here.
            .env("PATH", &bin)
            .env("LOG", &log)
            .env("HUSHRUN_REGISTRY_TOKEN", "pull-token");
        if let Some(lower) = lower {
            command.env("HUSHRUN_BUILD_LOWER", lower);
        }
        let status = command.status().unwrap();
        let said = std::fs::read_to_string(&log).unwrap_or_default();
        (said, status.code().unwrap_or(-1))
    }

    // covers: C-145
    #[cfg(unix)]
    #[test]
    fn cursors_install_script_pulls_as_root_and_fetches_as_the_user() {
        // As root: no sudo.
        let (said, code) = run_install_script("0", true, false, None);
        assert_eq!(code, 0);
        assert_eq!(
            said.lines().collect::<Vec<_>>(),
            [
                "hushrun build pull token=pull-token",
                "hushrun fetch token=pull-token"
            ],
            "{said}"
        );
        // As a user with sudo that needs no password: the pull through sudo, by its path, with
        // the token kept; fetch as the user.
        let (said, code) = run_install_script("1000", true, false, None);
        assert_eq!(code, 0);
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines[0], "sudo -n true", "{said}");
        assert!(
            lines[1].starts_with(
                "sudo -n --preserve-env=HUSHRUN_REGISTRY_TOKEN,HUSHRUN_BACKEND,HUSHRUN_HOST /"
            ) && lines[1].ends_with("/bin/hushrun build pull"),
            "{said}"
        );
        assert_eq!(lines[2], "hushrun build pull token=pull-token", "{said}");
        assert_eq!(lines[3], "hushrun fetch token=pull-token", "{said}");
        assert_eq!(lines.len(), 4, "{said}");
        // A store someone named is theirs: no sudo for the pull.
        let (said, code) = run_install_script("1000", true, false, Some("/home/me/baked"));
        assert_eq!(code, 0);
        assert!(!said.contains("sudo -n --preserve-env"), "{said}");
        assert!(
            said.contains("hushrun build pull token=pull-token"),
            "{said}"
        );
        // A failed pull, and no sudo: fetch still runs, and the Build goes on.
        let (said, code) = run_install_script("1000", false, true, None);
        assert_eq!(code, 0);
        assert_eq!(
            said.lines().collect::<Vec<_>>(),
            [
                "hushrun build pull token=pull-token",
                "hushrun fetch token=pull-token"
            ],
            "{said}"
        );
    }
    // covers: C-145, C-168
    #[test]
    fn an_earlier_inits_build_pull_becomes_session_start_and_the_rest_stays() {
        for npm in [true, false] {
            let run = runner(npm);
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            std::fs::create_dir_all(root.join(".claude")).unwrap();
            let old = format!("[ \"$CLAUDE_CODE_REMOTE\" != true ] || {run} build pull -q || true");
            let flush = format!("{run} flush -q || true");
            let settings = serde_json::json!({
                "permissions": {"allow": ["Bash(npm test)"]},
                "hooks": {
                    "SessionStart": [
                        {"matcher": "startup", "hooks": [{"type": "command", "command": "./scripts/setup.sh"}]},
                        {"hooks": [{"type": "command", "command": old}]},
                    ],
                    "Stop": [{"hooks": [{"type": "command", "command": flush}]}],
                },
            });
            std::fs::write(
                root.join(".claude/settings.json"),
                serde_json::to_string_pretty(&settings).unwrap(),
            )
            .unwrap();
            let files = cloud_files(root, &[Cloud::ClaudeCode], npm);
            assert_eq!(files.len(), 1);
            assert!(
                files[0].change.contains("an earlier init wrote"),
                "{}",
                files[0].change
            );
            let value: serde_json::Value = serde_json::from_str(&files[0].text).unwrap();
            let start = value["hooks"]["SessionStart"].as_array().unwrap();
            assert_eq!(start.len(), 2, "replaced, not added: {value:#}");
            assert_eq!(start[0]["matcher"], "startup");
            assert_eq!(start[0]["hooks"][0]["command"], "./scripts/setup.sh");
            assert_eq!(start[1]["hooks"][0]["command"], session_start(run));
            assert_eq!(start[1]["hooks"][0]["timeout"], 60);
            assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 1);
            assert_eq!(value["permissions"]["allow"][0], "Bash(npm test)");
            assert!(!files[0].text.contains("build pull"), "{}", files[0].text);
            // Written, init has nothing more to do.
            std::fs::write(root.join(".claude/settings.json"), &files[0].text).unwrap();
            assert!(cloud_files(root, &[Cloud::ClaudeCode], npm).is_empty());
        }
        // Beside the new hook already, the old one only goes.
        let mut value = serde_json::json!({"hooks": {"SessionStart": [
            {"hooks": [{"type": "command", "command": session_start("hushrun")}]},
            {"hooks": [
                {"type": "command", "command": "[ \"$CLAUDE_CODE_REMOTE\" != true ] || hushrun build pull -q || true"},
                {"type": "command", "command": "echo hi"},
            ]},
        ]}});
        let entry = serde_json::json!({"type": "command", "command": session_start("hushrun")});
        assert!(replace_old_pull(
            &mut value,
            &entry,
            &session_start("hushrun")
        ));
        assert_eq!(
            value["hooks"]["SessionStart"],
            serde_json::json!([
                {"hooks": [{"type": "command", "command": session_start("hushrun")}]},
                {"hooks": [{"type": "command", "command": "echo hi"}]},
            ])
        );
        assert!(!replace_old_pull(
            &mut value,
            &entry,
            &session_start("hushrun")
        ));
    }
}
