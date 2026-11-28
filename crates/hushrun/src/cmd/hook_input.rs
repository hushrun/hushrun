//! \[ADR 0165\] What an agent's hook gets on stdin: a JSON object, as Claude Code and Google
//! Antigravity pass it. `hushrun hook session-start` and `hushrun hook stop` read it for the
//! workspace's folder and whether the agent is idle, and nothing else.
//!
//! It is read at most [`MOST_READ`] bytes deep and for at most [`WAIT`]; a terminal isn't read at
//! all. What is past the cap is read and dropped in the background, so a writer with a large
//! payload never blocks on a full pipe. The payload holds the person's prompt (Antigravity's
//! `lastUserInput`): it is never logged, stored or printed, and only the two fields are kept.

use std::io::{IsTerminal as _, Read as _};
use std::path::PathBuf;
use std::time::Duration;

/// The most bytes of stdin that are parsed: past it, the payload is dropped whole.
pub const MOST_READ: u64 = 1 << 20;

/// The most it waits for stdin to end: a writer that keeps it open with nothing in it doesn't
/// hold the hook.
pub const WAIT: Duration = Duration::from_millis(500);

/// The two fields hushrun keeps of a hook's payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookInput {
    /// The workspace's folder: Antigravity's `workspacePaths[0]`, else Claude Code's `cwd`.
    pub workspace: Option<PathBuf>,
    /// Antigravity's `fullyIdle` on a `Stop` hook: whether the agent's loop is over, not only a
    /// turn of it. None when the payload doesn't say.
    pub fully_idle: Option<bool>,
}

impl HookInput {
    /// The fields of `bytes`, a payload; nothing when it isn't a JSON object.
    pub fn parse(bytes: &[u8]) -> HookInput {
        let Ok(serde_json::Value::Object(object)) = serde_json::from_slice(bytes) else {
            return HookInput::default();
        };
        let path = |value: Option<&serde_json::Value>| {
            value
                .and_then(serde_json::Value::as_str)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
        };
        let workspace = path(
            object
                .get("workspacePaths")
                .and_then(serde_json::Value::as_array)
                .and_then(|paths| paths.first()),
        )
        .or_else(|| path(object.get("cwd")));
        HookInput {
            workspace,
            fully_idle: object.get("fullyIdle").and_then(serde_json::Value::as_bool),
        }
    }
}

/// Reads the hook's payload on stdin, as the module says.
pub fn read() -> HookInput {
    if std::io::stdin().is_terminal() {
        return HookInput::default();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut bytes = Vec::new();
        let read = (&mut stdin).take(MOST_READ + 1).read_to_end(&mut bytes);
        let input = if read.is_ok() && bytes.len() as u64 <= MOST_READ {
            HookInput::parse(&bytes)
        } else {
            HookInput::default()
        };
        drop(bytes);
        let _ = tx.send(input);
        // The rest is dropped unread, so the writer never waits on a full pipe.
        let _ = std::io::copy(&mut stdin, &mut std::io::sink());
    });
    rx.recv_timeout(WAIT).unwrap_or_default()
}

/// Moves into the payload's workspace when this isn't a repository and `-C` names none: what a
/// hook runs from a folder outside the checkout finds the repository there, and so do the steps
/// it starts, which inherit the folder.
pub fn enter_workspace(global: &crate::cli::GlobalOpts, input: &HookInput) {
    if global.repo.is_some() || crate::cmd::login::repository_root(global).is_some() {
        return;
    }
    let Some(workspace) = &input.workspace else {
        return;
    };
    if matches!(
        hushrun_vcs::detect(workspace),
        Ok(hushrun_vcs::Detected::Git { .. })
    ) && std::env::set_current_dir(workspace).is_ok()
    {
        tracing::debug!("hook: in the workspace's folder, from the hook's payload");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-168
    #[test]
    fn the_workspace_and_idleness_are_all_that_is_kept() {
        let input = HookInput::parse(
            br#"{"conversationId":"c","workspacePaths":["/w/one","/w/two"],"lastUserInput":"a prompt","fullyIdle":false}"#,
        );
        assert_eq!(
            input,
            HookInput {
                workspace: Some(PathBuf::from("/w/one")),
                fully_idle: Some(false),
            }
        );
        let claude = HookInput::parse(br#"{"session_id":"s","cwd":"/repo","source":"startup"}"#);
        assert_eq!(claude.workspace, Some(PathBuf::from("/repo")));
        assert_eq!(claude.fully_idle, None);
        for nothing in [&b""[..], b"[]", b"not json", br#"{"workspacePaths":[""]}"#] {
            assert_eq!(HookInput::parse(nothing), HookInput::default());
        }
    }
}
