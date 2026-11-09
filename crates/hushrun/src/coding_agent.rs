//! Who runs hushrun: a coding agent, or a person (C-23). The pre-push hook refuses an agent's
//! push, and \[ADR 0065\] the newer-release notice (C-21) tells an agent without a terminal. One
//! definition, so the two never disagree about who is an agent.

/// The name of the coding agent this runs under, as the environment `var` reads tells it.
/// hushrun's own settings first, which an org gives its agents; then what the agents set
/// themselves. `CURSOR_TRACE_ID` isn't one of them: people's terminals in Cursor have it too.
/// \[ADR 0046\] `HUSHRUN_AGENT` comes before them all: a label is a word, and the string is an
/// agent's credential, readable or not, so `human:` beside it doesn't make a person.
pub fn agent(var: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    if var(hushrun_core::agent::VARIABLE).is_some_and(|text| !text.trim().is_empty()) {
        return Some("an agent token".into());
    }
    match var("HUSHRUN_INVOKED_BY").as_deref().map(str::trim) {
        Some(label) if label.starts_with("human:") => return None,
        Some(label) if label.starts_with("agent:") => return Some(label[6..].trim().into()),
        _ => {}
    }
    if var("HUSHRUN_TOKEN").is_some_and(|token| crate::results::is_agent_token(&token)) {
        return Some("an agent token".into());
    }
    if let Some(name) = var("AI_AGENT").filter(|name| !name.trim().is_empty()) {
        return Some(name.trim().to_string());
    }
    const NAMED: &[(&[&str], &str)] = &[
        (&["CLAUDECODE", "CLAUDE_CODE"], "Claude Code"),
        (&["CODEX_SANDBOX", "CODEX_CI", "CODEX_THREAD_ID"], "Codex"),
        (&["OPENCODE_CLIENT"], "OpenCode"),
        (&["CURSOR_AGENT"], "Cursor"),
        (&["GEMINI_CLI"], "Gemini CLI"),
        (&["AUGMENT_AGENT"], "Augment"),
        (&["ANTIGRAVITY_AGENT"], "Antigravity"),
        (
            &["COPILOT_MODEL", "COPILOT_ALLOW_ALL", "COPILOT_GITHUB_TOKEN"],
            "GitHub Copilot",
        ),
    ];
    for (names, agent) in NAMED {
        if names.iter().any(|name| var(name).is_some()) {
            return Some((*agent).to_string());
        }
    }
    if var("OPENCODE").as_deref() == Some("1") {
        return Some("OpenCode".into());
    }
    if var("CURSOR_EXTENSION_HOST_ROLE").as_deref() == Some("agent-exec") {
        return Some("Cursor".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-23
    #[test]
    fn an_agent_is_who_the_environment_says() {
        let who = |vars: &[(&str, &str)]| {
            agent(&|name| {
                vars.iter()
                    .find(|(var, _)| *var == name)
                    .map(|(_, value)| (*value).to_string())
            })
        };
        assert_eq!(who(&[]), None);
        assert_eq!(who(&[("CLAUDECODE", "1")]).as_deref(), Some("Claude Code"));
        // ADR 0046: whether hushrun can read it or not.
        assert_eq!(
            who(&[("HUSHRUN_AGENT", "not a string")]).as_deref(),
            Some("an agent token")
        );
        assert_eq!(who(&[("HUSHRUN_AGENT", " ")]), None);
        assert_eq!(
            who(&[("HUSHRUN_AGENT", "x"), ("HUSHRUN_INVOKED_BY", "human:ana")]).as_deref(),
            Some("an agent token")
        );
        assert_eq!(
            who(&[("CODEX_SANDBOX", "seatbelt")]).as_deref(),
            Some("Codex")
        );
        assert_eq!(who(&[("OPENCODE", "1")]).as_deref(), Some("OpenCode"));
        assert_eq!(who(&[("OPENCODE", "0")]), None);
        assert_eq!(who(&[("CURSOR_AGENT", "1")]).as_deref(), Some("Cursor"));
        assert_eq!(who(&[("AI_AGENT", "my-bot")]).as_deref(), Some("my-bot"));
        // Everyone's terminal in Cursor has this.
        assert_eq!(who(&[("CURSOR_TRACE_ID", "abc")]), None);
        // An org's own word comes first.
        assert_eq!(
            who(&[("HUSHRUN_TOKEN", "hra_abc")]).as_deref(),
            Some("an agent token")
        );
        assert_eq!(who(&[("HUSHRUN_TOKEN", "hru_abc")]), None);
        assert_eq!(
            who(&[("HUSHRUN_INVOKED_BY", "agent:fleet-1")]).as_deref(),
            Some("fleet-1")
        );
        assert_eq!(
            who(&[("HUSHRUN_INVOKED_BY", "human:ana"), ("CLAUDECODE", "1")]),
            None
        );
    }
}
