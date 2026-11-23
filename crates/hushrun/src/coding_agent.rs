//! Who runs hushrun: a coding agent, or a person (C-23). The pre-push hook refuses an agent's
//! push, and \[ADR 0065\] the newer-release notice (C-21) tells an agent without a terminal. One
//! definition, so the two never disagree about who is an agent, and \[ADR 0104\] its agents come
//! from the table `detect` reads, so a sandbox and its agent are named the same way.

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
    // \[ADR 0104\] The one table of agents and their platforms, which `detect` reads too.
    let env = hushrun_ci::Env::from_pairs(
        hushrun_ci::platforms::PLATFORMS
            .iter()
            .flat_map(|platform| platform.markers.iter().map(|marker| marker.name()))
            .filter_map(|name| var(name).map(|value| (name, value)))
            .collect::<Vec<_>>()
            .iter()
            .map(|(name, value)| (*name, value.as_str())),
    );
    if let Some(platform) = hushrun_ci::platforms::of(&env) {
        return Some(platform.agent.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-23, C-50
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
        // ADR 0104: a hosted platform's marker names its agent.
        assert_eq!(
            who(&[("CLAUDE_CODE_REMOTE", "true")]).as_deref(),
            Some("Claude Code")
        );
        assert_eq!(
            who(&[("CURSOR_AGENT_SOCKET", "/tmp/s")]).as_deref(),
            Some("Cursor")
        );
        assert_eq!(
            who(&[("COPILOT_AGENT_SESSION_ID", "x")]).as_deref(),
            Some("GitHub Copilot")
        );
        assert_eq!(
            who(&[("CURSOR_EXTENSION_HOST_ROLE", "agent-exec")]).as_deref(),
            Some("Cursor")
        );
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
