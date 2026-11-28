//! \[ADR 0140\] `hushrun doctor` in a coding agent's sandbox (C-140): which platform this is, where
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

/// The variable that names the image `hushrun packages pull` pulls, `<host>/<org>/<name>`.
pub const PACKAGES_IMAGE: &str = "HUSHRUN_PACKAGES_IMAGE";

/// The hosts hushrun needs from here, each with what it is for and the URL that asks it (`None`
/// for one that can't be asked from here): the backend's, its registry's, and, for hushrun's own
/// backend, the shared store's, whose exact host is the org's own subdomain, which only a run that
/// uses the store learns; then the registry of the packages image `image` names, when it's another.
pub fn hosts(
    backend: Option<&str>,
    image: Option<&str>,
) -> Vec<(String, &'static str, Option<String>)> {
    let origin = backend
        .and_then(|base| hushrun_core::ledger_url::LedgerUrl::parse(base).ok())
        .map(|url| crate::credentials::origin(&url))
        .unwrap_or_else(hosted::origin);
    let host = host_of(&origin);
    let mut hosts = vec![(
        host.clone(),
        "results, and what the store holds",
        Some(format!("{origin}/healthz")),
    )];
    let registry = hosted::registry_host(&origin);
    if registry != host {
        hosts.push((
            registry.clone(),
            "baked stores (hushrun build pull)",
            Some(format!("https://{registry}/v2/")),
        ));
    }
    if origin.eq_ignore_ascii_case(hosted::HOSTED_ORIGIN) {
        hosts.push((
            STORE_HOSTS.to_string(),
            "compiled code other machines built",
            None,
        ));
    }
    if let Some(registry) = image.and_then(image_host)
        && hosts.iter().all(|(host, _, _)| *host != registry)
    {
        hosts.push((
            registry.clone(),
            "the packages image (hushrun packages pull)",
            Some(format!("https://{registry}/v2/")),
        ));
    }
    hosts
}

/// The registry's host of an image reference `<host>/<org>/<name>[:tag]`, port and all.
fn image_host(image: &str) -> Option<String> {
    let image = image.trim();
    let image = image.split_once("://").map_or(image, |(_, rest)| rest);
    let (host, rest) = image.split_once('/')?;
    (!host.is_empty() && !rest.is_empty()).then(|| host.to_ascii_lowercase())
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
    let hosts = hosts(backend, env.get(PACKAGES_IMAGE));
    let mut blocked = Vec::new();
    for (host, what, url) in &hosts {
        let Some(url) = url else {
            report.add(
                "hosts",
                Level::Ok,
                format!("{host}, for {what}: your organization's own host there can't be checked from here; a run that can't reach it names it"),
            );
            continue;
        };
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
    let allowlist = allowlist_line(platform, &entry);
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

/// What the `allowlist` line says: the hosts, and where the platform allows them.
pub fn allowlist_line(platform: Option<&Platform>, hosts: &[&str]) -> String {
    match platform.and_then(|p| p.allowlist) {
        Some(place) => format!("allow {} in {place}", hosts.join(", ")),
        None => format!(
            "where this network allows only some hosts, allow {}",
            hosts.join(", ")
        ),
    }
}

fn sandbox_level(platform: &Platform) -> Level {
    match platform.evidence {
        // A row with no marker is one a person named: nothing unconfirmed decided it.
        Evidence::Unconfirmed if !platform.markers.is_empty() => Level::Warning,
        Evidence::Unconfirmed | Evidence::Probed | Evidence::Documented => Level::Ok,
    }
}

/// What the `sandbox` line says.
pub fn sandbox_line(platform: &Platform) -> String {
    if platform.markers.is_empty() {
        return format!(
            "{}: a hosted sandbox, as {} says, since no variable of its own is known yet; runs are local",
            platform.name,
            hushrun_ci::platforms::SANDBOX_VARIABLE
        );
    }
    let marker = platform
        .markers
        .iter()
        .map(|m| m.name())
        .collect::<Vec<_>>()
        .join(" or ");
    let kind = match platform.kind {
        Kind::Platform => "a hosted sandbox",
        Kind::Agent => "a coding agent's sandbox, as CI=true beside its marker says",
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
            Credential::Session { saved: true } => {
                "the session hushrun login --session saved".to_string()
            }
            Credential::Session { saved: false } => "HUSHRUN_TOKEN, a session token".to_string(),
            Credential::Bearer => "HUSHRUN_TOKEN, sent with no key".to_string(),
            Credential::Proxy => {
                "the credential this network adds to requests for the backend (HUSHRUN_AUTH=proxy)"
                    .to_string()
            }
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

    // covers: C-140
    #[test]
    fn hushruns_own_backend_needs_three_hosts_and_another_one() {
        let names = |backend: Option<&str>| {
            hosts(backend, None)
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

    // covers: C-140
    #[test]
    fn the_packages_image_names_its_registry_too() {
        let names = |image: &str| {
            hosts(Some("https://api.hushrun.com/o/acme"), Some(image))
                .into_iter()
                .map(|(host, _, _)| host)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("ghcr.io/acme/shop:hushrun-packages-linux-latest"),
            [
                "api.hushrun.com",
                "registry.hushrun.com",
                STORE_HOSTS,
                "ghcr.io"
            ]
        );
        assert_eq!(
            names("Registry.Acme.dev:5000/acme/shop")[3],
            "registry.acme.dev:5000"
        );
        // Named already, or no host to name.
        assert_eq!(names("registry.hushrun.com/acme/shop").len(), 3);
        assert_eq!(names("shop").len(), 3);
    }

    // covers: C-140
    #[test]
    fn antigravitys_remote_agents_are_named_by_hushrun_sandbox() {
        let env = hushrun_ci::Env::from_pairs([
            ("HUSHRUN_SANDBOX", "antigravity-remote"),
            ("CI", "true"),
        ]);
        let platform = hushrun_ci::sandbox(&env).unwrap();
        assert_eq!(platform.id, "antigravity-remote");
        assert_eq!(
            sandbox_line(platform),
            "Antigravity Remote Agents: a hosted sandbox, as HUSHRUN_SANDBOX says, since no variable of its own is known yet; runs are local"
        );
        assert_eq!(sandbox_level(platform), Level::Ok);
        let image = hosts(None, Some("ghcr.io/acme/shop"));
        let entry: Vec<&str> = image.iter().map(|(host, _, _)| host.as_str()).collect();
        assert_eq!(
            allowlist_line(Some(platform), &entry),
            "allow api.hushrun.com, registry.hushrun.com, *.r2.cloudflarestorage.com, ghcr.io in the Remote Agents environment's network settings, where it limits egress"
        );
    }

    // covers: C-140
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
