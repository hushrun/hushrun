//! \[ADR 0104\] Coding agents and the hosted platforms they run on (C-50): one table, read by
//! [`crate::detect`], which tells a hosted agent's sandbox from CI, and by the CLI's test of who
//! runs hushrun (C-23), so the two never disagree about which agent this is.
//!
//! Two kinds of row. A **platform** row is a vendor's hosted sandbox, whose marker only that
//! vendor's platform sets: it makes the run a sandbox beside `CI=true` and beside a named CI's own
//! variable, since a platform such as GitHub's Copilot cloud agent runs inside GitHub Actions. An
//! **agent** row is a coding agent that also runs on people's machines and in CI jobs: it makes a
//! run a sandbox beside bare `CI=true` only, as `HUSHRUN_AGENT` does (ADR 0046), and never beside
//! a named CI, where an agent's step in a job keeps ADR 0044's guards.

/// What a row's marker needs of a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// Set, to anything but nothing.
    Set(&'static str),
    /// Set to anything but nothing, `false` or `0`, in any case.
    Yes(&'static str),
    /// Set to exactly this value.
    Is(&'static str, &'static str),
}

impl Marker {
    /// The variable's name.
    pub fn name(self) -> &'static str {
        match self {
            Marker::Set(name) | Marker::Yes(name) | Marker::Is(name, _) => name,
        }
    }
}

/// Which kind of row it is (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A vendor's hosted sandbox: a sandbox beside any CI's variables.
    Platform,
    /// A coding agent that also runs on people's machines: a sandbox beside bare `CI=true` only.
    Agent,
}

/// How hushrun knows a row's marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Seen in that platform's sandbox, with `scripts/agent-cloud-probe.sh`.
    Probed,
    /// The vendor's documentation names it.
    Documented,
    /// Reported by others, and neither seen nor documented: it may change or be wrong.
    Unconfirmed,
}

/// One coding agent or platform.
#[derive(Debug, PartialEq, Eq)]
pub struct Platform {
    /// A stable id, for `--format json` and the docs' pages: `claude-code-web`.
    pub id: &'static str,
    /// The platform's name, as lines say it: `Claude Code on the web`.
    pub name: &'static str,
    /// The coding agent it runs, as C-23 names it: `Claude Code`.
    pub agent: &'static str,
    pub kind: Kind,
    /// Any one of them, set, is the row.
    pub markers: &'static [Marker],
    pub evidence: Evidence,
    /// Where that platform lets a team allow hosts, for `hushrun doctor`; `None` where the
    /// sandbox's network isn't limited, or the row isn't a sandbox of its own.
    pub allowlist: Option<&'static str>,
}

impl Platform {
    /// Whether the environment `env` has this row's marker.
    pub fn matches(&self, env: &crate::Env) -> bool {
        self.markers.iter().any(|marker| match *marker {
            Marker::Set(name) => env.get(name).is_some_and(|v| !v.trim().is_empty()),
            Marker::Yes(name) => env.says_yes(name),
            Marker::Is(name, value) => env.get(name).map(str::trim) == Some(value),
        })
    }
}

/// Every row, platforms first. Each is a coding agent: C-23 takes whoever runs under one for an
/// agent, so a person's own hosted machine, such as a codespace, has no row. The first that
/// matches is the one. A platform runs one of the agents below it (Claude Code on the web sets
/// `CLAUDECODE` too), and its row is the more exact.
pub const PLATFORMS: &[Platform] = &[
    Platform {
        id: "claude-code-web",
        name: "Claude Code on the web",
        agent: "Claude Code",
        kind: Kind::Platform,
        markers: &[Marker::Yes("CLAUDE_CODE_REMOTE")],
        evidence: Evidence::Probed,
        allowlist: Some(
            "the environment's network access: Custom, with the hosts added to the allowed domains",
        ),
    },
    Platform {
        id: "cursor-cloud",
        name: "Cursor cloud agents",
        agent: "Cursor",
        kind: Kind::Platform,
        markers: &[Marker::Set("CURSOR_AGENT_SOCKET")],
        evidence: Evidence::Documented,
        allowlist: Some("the cloud agent environment's network settings"),
    },
    Platform {
        id: "copilot-cloud",
        name: "GitHub Copilot cloud agent",
        agent: "GitHub Copilot",
        kind: Kind::Platform,
        markers: &[Marker::Set("COPILOT_AGENT_SESSION_ID")],
        evidence: Evidence::Unconfirmed,
        allowlist: Some(
            "the repository's Copilot settings: the coding agent's firewall allowlist (custom allowlist)",
        ),
    },
    Platform {
        id: "codex-cloud",
        name: "Codex cloud",
        agent: "Codex",
        kind: Kind::Platform,
        markers: &[Marker::Set("CODEX_PROXY_CERT")],
        evidence: Evidence::Unconfirmed,
        allowlist: Some(
            "the environment's internet access: On, with the hosts in the domain allowlist",
        ),
    },
    Platform {
        id: "claude-code",
        name: "Claude Code",
        agent: "Claude Code",
        kind: Kind::Agent,
        markers: &[Marker::Set("CLAUDECODE"), Marker::Set("CLAUDE_CODE")],
        evidence: Evidence::Documented,
        allowlist: None,
    },
    Platform {
        id: "codex",
        name: "Codex",
        agent: "Codex",
        kind: Kind::Agent,
        markers: &[
            Marker::Set("CODEX_SANDBOX"),
            Marker::Set("CODEX_CI"),
            Marker::Set("CODEX_THREAD_ID"),
        ],
        evidence: Evidence::Documented,
        allowlist: None,
    },
    Platform {
        id: "opencode",
        name: "OpenCode",
        agent: "OpenCode",
        kind: Kind::Agent,
        markers: &[Marker::Set("OPENCODE_CLIENT"), Marker::Is("OPENCODE", "1")],
        evidence: Evidence::Documented,
        allowlist: None,
    },
    Platform {
        id: "cursor",
        name: "Cursor",
        agent: "Cursor",
        kind: Kind::Agent,
        markers: &[
            Marker::Set("CURSOR_AGENT"),
            Marker::Is("CURSOR_EXTENSION_HOST_ROLE", "agent-exec"),
        ],
        evidence: Evidence::Documented,
        allowlist: None,
    },
    Platform {
        id: "gemini-cli",
        name: "Gemini CLI",
        agent: "Gemini CLI",
        kind: Kind::Agent,
        markers: &[Marker::Set("GEMINI_CLI")],
        evidence: Evidence::Documented,
        allowlist: None,
    },
    Platform {
        id: "augment",
        name: "Augment",
        agent: "Augment",
        kind: Kind::Agent,
        markers: &[Marker::Set("AUGMENT_AGENT")],
        evidence: Evidence::Documented,
        allowlist: None,
    },
    Platform {
        id: "antigravity",
        name: "Antigravity",
        agent: "Antigravity",
        kind: Kind::Agent,
        markers: &[Marker::Set("ANTIGRAVITY_AGENT")],
        evidence: Evidence::Documented,
        allowlist: None,
    },
    Platform {
        id: "copilot",
        name: "GitHub Copilot",
        agent: "GitHub Copilot",
        kind: Kind::Agent,
        markers: &[
            Marker::Set("COPILOT_MODEL"),
            Marker::Set("COPILOT_ALLOW_ALL"),
            Marker::Set("COPILOT_GITHUB_TOKEN"),
        ],
        evidence: Evidence::Documented,
        allowlist: None,
    },
];

/// The first row the environment matches, platforms first.
pub fn of(env: &crate::Env) -> Option<&'static Platform> {
    PLATFORMS.iter().find(|platform| platform.matches(env))
}

/// The hosted platform the environment says this is: a [`Kind::Platform`] row only.
pub fn hosted(env: &crate::Env) -> Option<&'static Platform> {
    PLATFORMS
        .iter()
        .filter(|platform| platform.kind == Kind::Platform)
        .find(|platform| platform.matches(env))
}

/// A row by its id.
pub fn by_id(id: &str) -> Option<&'static Platform> {
    PLATFORMS.iter().find(|platform| platform.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Env;

    // covers: C-50
    #[test]
    fn the_table_has_one_row_per_id_and_platforms_first() {
        let mut ids: Vec<&str> = PLATFORMS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PLATFORMS.len(), "ids are unique");
        let first_agent = PLATFORMS
            .iter()
            .position(|p| p.kind == Kind::Agent)
            .unwrap();
        assert!(
            PLATFORMS[first_agent..]
                .iter()
                .all(|p| p.kind == Kind::Agent),
            "platforms come before agents"
        );
        assert!(PLATFORMS.iter().all(|p| !p.markers.is_empty()));
    }

    // covers: C-50
    #[test]
    fn a_platform_is_known_by_its_marker() {
        let env = Env::from_pairs([("CLAUDE_CODE_REMOTE", "true"), ("CLAUDECODE", "1")]);
        assert_eq!(of(&env).map(|p| p.id), Some("claude-code-web"));
        assert_eq!(hosted(&env).map(|p| p.id), Some("claude-code-web"));
        let env = Env::from_pairs([("CLAUDE_CODE_REMOTE", "false"), ("CLAUDECODE", "1")]);
        assert_eq!(of(&env).map(|p| p.id), Some("claude-code"));
        assert_eq!(hosted(&env), None);
        let env = Env::from_pairs([("CURSOR_AGENT_SOCKET", "/run/cursor.sock")]);
        assert_eq!(hosted(&env).map(|p| p.agent), Some("Cursor"));
        let env = Env::from_pairs([("COPILOT_AGENT_SESSION_ID", "abc")]);
        let copilot = hosted(&env).unwrap();
        assert_eq!(copilot.evidence, Evidence::Unconfirmed);
        let env = Env::from_pairs([("OPENCODE", "0")]);
        assert_eq!(of(&env), None);
        let env = Env::from_pairs([("CURSOR_TRACE_ID", "abc")]);
        assert_eq!(of(&env), None, "everyone's terminal in Cursor has it");
        assert_eq!(by_id("codex-cloud").map(|p| p.agent), Some("Codex"));
    }
}
