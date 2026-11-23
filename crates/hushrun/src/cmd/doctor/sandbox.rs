//! \[ADR 0104\] `hushrun doctor` in a coding agent's sandbox (C-50): which platform this is, where
//! the credential comes from, whether each host hushrun needs answers here, the exact entry that
//! platform's allowlist needs, and what will be shared. It names variables and hosts, never a
//! value.

use std::time::Duration;

use hushrun_ci::platforms::{Evidence, Kind, Platform};

use super::{Level, Report};
use crate::hosted;
use crate::reach::{Reach, probe};
use crate::results::{Credential, Local, Place};

/// How long each host may take to answer.
const TIMEOUT: Duration = Duration::from_secs(8);

/// The shared store's hosts, as an allowlist names them: hushrun cloud's store is Cloudflare R2,
/// whose presigned URLs name the account's host and can't use a domain of ours.
pub const STORE_HOSTS: &str = "*.r2.cloudflarestorage.com";

/// The hosts hushrun needs from here, each with what it is for and the URL that asks it: the
/// backend's, its registry's, and, for hushrun's own backend, the shared store's.
pub fn hosts(backend: Option<&str>) -> Vec<(String, &'static str, String)> {
    let origin = backend
        .and_then(|base| hushrun_core::ledger_url::LedgerUrl::parse(base).ok())
        .map(|url| crate::credentials::origin(&url))
        .unwrap_or_else(hosted::origin);
    let host = host_of(&origin);
    let mut hosts = vec![(
        host.clone(),
        "results, and what the store holds",
        format!("{origin}/healthz"),
    )];
    let registry = hosted::registry_host(&origin);
    if registry != host {
        hosts.push((
            registry.clone(),
            "baked stores (hushrun build pull)",
            format!("https://{registry}/v2/"),
        ));
    }
    if origin.eq_ignore_ascii_case(hosted::HOSTED_ORIGIN) {
        hosts.push((
            STORE_HOSTS.to_string(),
            "compiled code other machines built",
            "https://r2.cloudflarestorage.com/".to_string(),
        ));
    }
    hosts
}

fn host_of(origin: &str) -> String {
    origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// The checks: `sandbox`, then, in a sandbox or when asked, `hosts`, `allowlist` and `shared`.
pub(super) fn check(
    report: &mut Report,
    env: &hushrun_ci::Env,
    place: Option<&Place>,
    asked: bool,
) {
    let detected = hushrun_ci::detection(env);
    let platform = match detected {
        hushrun_ci::Detected::Sandbox(platform) => Some(platform),
        _ => None,
    };
    match platform {
        Some(platform) => report.add("sandbox", sandbox_level(platform), sandbox_line(platform)),
        None if asked => report.add("sandbox", Level::Ok, "no coding agent's sandbox detected"),
        None => return,
    }
    report.add("credential", Level::Ok, credential_line(env, place));
    let backend = match place {
        Some(Place::Server { base, .. }) => Some(base.as_str()),
        Some(Place::Directory {
            local: Local::SignedOut { base, .. },
            ..
        }) => Some(base.as_str()),
        _ => None,
    };
    let hosts = hosts(backend);
    let mut blocked = Vec::new();
    for (host, what, url) in &hosts {
        let reach = probe(url, TIMEOUT);
        if !reach.answered() {
            blocked.push(host.clone());
        }
        report.add(
            "hosts",
            if reach.answered() {
                Level::Ok
            } else {
                Level::Warning
            },
            format!("{host}, for {what}: {}", reach.describe()),
        );
        if let Reach::Tls(_) = reach {
            report.add(
                "hosts",
                Level::Warning,
                "a proxy that re-signs TLS needs its CA in the system's store (update-ca-certificates), which hushrun trusts",
            );
        }
    }
    let entry: Vec<&str> = hosts.iter().map(|(host, _, _)| host.as_str()).collect();
    let allowlist = match platform.and_then(|p| p.allowlist) {
        Some(place) => format!("allow {} in {place}", entry.join(", ")),
        None => format!(
            "where this network allows only some hosts, allow {}",
            entry.join(", ")
        ),
    };
    report.add(
        "allowlist",
        if blocked.is_empty() {
            Level::Ok
        } else {
            Level::Warning
        },
        allowlist,
    );
    report.add("shared", Level::Ok, shared_line(place));
}

fn sandbox_level(platform: &Platform) -> Level {
    match platform.evidence {
        Evidence::Unconfirmed => Level::Warning,
        Evidence::Probed | Evidence::Documented => Level::Ok,
    }
}

/// What the `sandbox` line says.
pub fn sandbox_line(platform: &Platform) -> String {
    let marker = platform
        .markers
        .iter()
        .map(|m| m.name())
        .collect::<Vec<_>>()
        .join(" or ");
    let kind = match platform.kind {
        Kind::Platform => "a hosted sandbox",
        Kind::Agent => "a coding agent's sandbox, as CI=true beside it says",
    };
    let mut line = format!(
        "{}: {kind}, as {marker} says; runs are local",
        platform.name
    );
    if platform.evidence == Evidence::Unconfirmed {
        line.push_str(&format!(
            " ({marker} is unconfirmed: if this isn't {}, set HUSHRUN_PROVIDER)",
            platform.name
        ));
    }
    line
}

/// Where the credential comes from, by name only.
pub fn credential_line(env: &hushrun_ci::Env, place: Option<&Place>) -> String {
    match place {
        Some(Place::Server { credential, .. }) => match credential {
            Credential::Agent => "HUSHRUN_AGENT (the agent token and its key)".to_string(),
            Credential::Token { agent: true } => "HUSHRUN_TOKEN, an agent token".to_string(),
            Credential::Token { agent: false } => "HUSHRUN_TOKEN".to_string(),
            Credential::Login { .. } => "the token hushrun login saved".to_string(),
            Credential::GithubOidc => "GitHub Actions' ID token".to_string(),
            Credential::Nothing => "none: requests carry no token".to_string(),
        },
        Some(Place::Directory { .. }) | None if env.agent_set() => {
            "HUSHRUN_AGENT is set, but results don't go to a server".to_string()
        }
        Some(Place::Directory { .. }) => {
            "none needed: results stay on this machine for this session".to_string()
        }
        None => "none: results are off".to_string(),
    }
}

/// What will be shared.
pub fn shared_line(place: Option<&Place>) -> String {
    match place {
        Some(Place::Server { base, .. }) => {
            format!("checks' results go to {base}, and passes there skip checks here")
        }
        Some(Place::Directory {
            local: Local::SignedOut { base, .. },
            ..
        }) => format!(
            "nothing: results stay in this sandbox, and skip checks only within this session; {base} gets nothing until a credential is set"
        ),
        Some(Place::Directory { .. }) => {
            "nothing: results stay in this sandbox, and skip checks only within this session"
                .to_string()
        }
        None => "nothing: results are off".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-50
    #[test]
    fn hushruns_own_backend_needs_three_hosts_and_another_one() {
        let names = |backend: Option<&str>| {
            hosts(backend)
                .into_iter()
                .map(|(host, _, _)| host)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(Some("https://api.hushrun.com/o/acme")),
            ["api.hushrun.com", "registry.hushrun.com", STORE_HOSTS]
        );
        assert_eq!(
            names(Some("https://hushrun.acme.dev/o/acme")),
            ["hushrun.acme.dev"]
        );
    }

    // covers: C-50
    #[test]
    fn lines_name_variables_and_never_a_value() {
        let env = hushrun_ci::Env::from_pairs([("CLAUDE_CODE_REMOTE", "true")]);
        let platform = hushrun_ci::sandbox(&env).unwrap();
        assert_eq!(
            sandbox_line(platform),
            "Claude Code on the web: a hosted sandbox, as CLAUDE_CODE_REMOTE says; runs are local"
        );
        let copilot = hushrun_ci::platforms::by_id("copilot-cloud").unwrap();
        assert!(sandbox_line(copilot).contains("unconfirmed"));
        assert_eq!(sandbox_level(copilot), Level::Warning);
        let dir = Place::Directory {
            dir: "/tmp/r".into(),
            local: Local::Unnamed,
        };
        assert_eq!(
            credential_line(&env, Some(&dir)),
            "none needed: results stay on this machine for this session"
        );
        assert!(shared_line(Some(&dir)).contains("only within this session"));
        let server = Place::Server {
            base: "https://api.hushrun.com/o/acme".into(),
            named_by: crate::results::NamedBy::Agent,
            credential: Credential::Agent,
            withheld: false,
        };
        assert_eq!(
            credential_line(&env, Some(&server)),
            "HUSHRUN_AGENT (the agent token and its key)"
        );
    }
}
