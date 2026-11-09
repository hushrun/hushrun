//! `hushrun whoami` (C-17): where `hushrun run` records results, with which credential, and
//! who the backend says that credential is: a person, CI, or an agent token with its agent,
//! scope, expiry and limit (ADR 0027 item 8). `hushrun doctor` shows the same as one line.
//! \[ADR 0046\] `HUSHRUN_AGENT` shows as what named the backend, what goes there and where the key
//! is, and never as a value.

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::cli::GlobalOpts;
use crate::cmd::login::repository_root;
use crate::cmd::token::{now, when};
use crate::exit::{CmdResult, Exit};
use crate::hosted;
use crate::output::{self, Format};
use crate::results::{Backend, Credential, NamedBy, Place};

/// \[ADR 0046\] What takes signing in's place where an agent is set: it refuses.
const AGENT_UNREADABLE: &str = "HUSHRUN_AGENT can't be read, and only it speaks for an agent";

/// What hushrun knows of its identity at the backend.
pub(crate) struct Identity {
    /// Where results go; `None` when they are off.
    pub place: Option<Place>,
    /// Why results are off, or the backend couldn't be chosen.
    pub off: Option<String>,
    /// `GET {base}/v0`'s answer.
    pub info: Option<Value>,
    /// Why `GET {base}/v0` failed, and whether the server refused the credential (rather than
    /// being out of reach).
    pub error: Option<(String, bool)>,
}

impl Identity {
    /// Chooses the backend as `hushrun run` would here, and asks it who the credential is.
    pub(crate) fn read(global: &GlobalOpts) -> Identity {
        let root = repository_root(global);
        let backend = match Backend::select(root.as_deref()) {
            Ok(Some(backend)) => backend,
            Ok(None) => {
                return Identity {
                    place: None,
                    off: Some(crate::results::off_reason().to_string()),
                    info: None,
                    error: None,
                };
            }
            Err(err) => {
                return Identity {
                    place: None,
                    off: Some(format!("{err:#}")),
                    info: None,
                    error: None,
                };
            }
        };
        let (info, error) = match backend.info() {
            Ok(info) => (info, None),
            Err(err) => (
                None,
                Some((format!("{err:#}"), backend.refusal().is_some())),
            ),
        };
        Identity {
            place: Some(backend.place()),
            off: None,
            info,
            error,
        }
    }

    /// The agent token's details, when the credential is one.
    fn token(&self) -> Option<&Value> {
        self.info
            .as_ref()
            .map(|info| &info["token"])
            .filter(|token| token.is_object())
    }

    /// One line, as `hushrun doctor` shows it, and whether it is good.
    pub(crate) fn line(&self, now: i64) -> (bool, String) {
        let Some(place) = &self.place else {
            return (
                true,
                format!("off: {}", self.off.as_deref().unwrap_or("results are off")),
            );
        };
        let (base, credential) = match place {
            Place::Directory(dir) => {
                return (true, format!("this machine only, in {}", dir.display()));
            }
            Place::Server {
                base, credential, ..
            } => (base, credential),
        };
        if let Some((error, _)) = &self.error {
            return (false, error.clone());
        }
        let info = self.info.as_ref().cloned().unwrap_or_default();
        let mut line = format!(
            "{base} as {} with {}",
            info["as"].as_str().unwrap_or("?"),
            describe(credential)
        );
        if let Some(token) = self.token() {
            let _ = write!(
                line,
                ", for {}, expires {}",
                token["repository"].as_str().unwrap_or("the whole org"),
                token["expires_at"]
                    .as_str()
                    .map_or_else(|| "?".to_string(), |at| when(at, now))
            );
        }
        (true, line)
    }
}

pub fn run(global: &GlobalOpts) -> CmdResult {
    let identity = Identity::read(global);
    let exit = match &identity.error {
        None => Exit::Ok,
        Some((_, true)) => Exit::Unexpected,
        Some((_, false)) => Exit::Ledger,
    };
    if global.format == Format::Json {
        output::print_json(&json(&identity))?;
        return Ok(exit);
    }
    // A refusal is in its own line, so the exit code is all that's left to say.
    output::print(&text(&identity, now(global)))?;
    Ok(exit)
}

/// How a line names the credential.
fn describe(credential: &Credential) -> String {
    match credential {
        Credential::Token { agent: true } => "HUSHRUN_TOKEN, an agent token".to_string(),
        Credential::Token { agent: false } => "HUSHRUN_TOKEN".to_string(),
        Credential::Agent => "HUSHRUN_AGENT, an agent token".to_string(),
        Credential::Login { email } => format!("the token hushrun login saved for {email}"),
        Credential::GithubOidc => "this job's GitHub Actions ID token".to_string(),
        Credential::Nothing => "no token".to_string(),
    }
}

/// What named the backend, as a line says it.
pub(crate) fn named(named_by: NamedBy) -> &'static str {
    match named_by {
        NamedBy::Environment => "HUSHRUN_BACKEND names it",
        NamedBy::Agent => "HUSHRUN_AGENT names it",
        NamedBy::Repository => "this repository's package.json names it",
        NamedBy::Policy => "this repository's hushrun.yaml names it",
        NamedBy::Login => "hushrun login saved it",
    }
}

/// What named the backend, as JSON says it (`named_by`).
pub(crate) fn named_by_key(named_by: NamedBy) -> &'static str {
    match named_by {
        NamedBy::Environment => "HUSHRUN_BACKEND",
        NamedBy::Agent => "HUSHRUN_AGENT",
        NamedBy::Repository => "package.json",
        NamedBy::Policy => "hushrun.yaml",
        NamedBy::Login => "login",
    }
}

fn text(identity: &Identity, now: i64) -> String {
    let mut out = String::new();
    let mut line = |label: &str, value: String| {
        let _ = writeln!(out, "{label:<10}{value}");
    };
    let Some(place) = &identity.place else {
        line(
            "backend",
            format!(
                "none: results are off ({})",
                identity.off.as_deref().unwrap_or("?")
            ),
        );
        return out;
    };
    let (base, named_by, credential, withheld) = match place {
        Place::Directory(dir) => {
            line(
                "backend",
                format!("this machine only, in {}", dir.display()),
            );
            line(
                "share",
                match hushrun_ci::Env::from_process().agent_set() {
                    true => AGENT_UNREADABLE.to_string(),
                    false => {
                        "sign in with hushrun login, or name a backend in package.json".to_string()
                    }
                },
            );
            return out;
        }
        Place::Server {
            base,
            named_by,
            credential,
            withheld,
        } => (base, *named_by, credential, *withheld),
    };
    line("backend", format!("{base} ({})", named(named_by)));
    let token = match credential {
        Credential::Nothing if withheld => format!(
            "none: HUSHRUN_TOKEN isn't sent here, as it goes only to the backend HUSHRUN_BACKEND names, and an agent token (hra_…) to hushrun's host, {}",
            hosted::origin()
        ),
        Credential::Nothing if hushrun_ci::Env::from_process().agent_set() => {
            format!("none: {AGENT_UNREADABLE}")
        }
        Credential::Nothing => "none: sign in with hushrun login, or set HUSHRUN_TOKEN".to_string(),
        other => describe(other),
    };
    line("token", token);
    if let Some((error, refused)) = &identity.error {
        let label = if *refused { "refused" } else { "error" };
        line(label, error.clone());
        return out;
    }
    let info = identity.info.as_ref().cloned().unwrap_or_default();
    line(
        "as",
        format!(
            "{} in {}",
            info["as"].as_str().unwrap_or("?"),
            info["org"].as_str().unwrap_or("?")
        ),
    );
    if let Some(token) = identity.token() {
        line(
            "agent",
            format!(
                "{} ({}…)",
                token["name"].as_str().unwrap_or("?"),
                token["prefix"].as_str().unwrap_or("hra_")
            ),
        );
        line(
            "for",
            token["repository"]
                .as_str()
                .unwrap_or("the whole org")
                .to_string(),
        );
        if let Some(at) = token["expires_at"].as_str() {
            line("expires", when(at, now));
        }
        if let Some(limit) = token["results_per_minute"].as_u64() {
            line("limit", format!("{} a minute", results(limit)));
        }
    }
    if let Some(dashboard) = info["dashboard"].as_str() {
        line("dashboard", dashboard.to_string());
    }
    let agent = matches!(
        credential,
        Credential::Token { agent: true } | Credential::Agent
    );
    match machine(agent) {
        Some((keyid, source)) => line(
            "machine",
            format!("{} ({source})", hosted::short_keyid(&keyid)),
        ),
        // ADR 0046: the key is inside the string, so there is no other variable to set.
        None if *credential == Credential::Agent => line(
            "machine",
            format!("none: {}", crate::results::STRING_NEEDS_REPLACING),
        ),
        // ADR 0028, amended 2026-10-28: an agent's results are recorded only signed by its key.
        None if agent => line(
            "machine",
            format!("none: {}", crate::results::AGENT_NEEDS_ITS_KEY),
        ),
        None => {}
    }
    out
}

/// \[ADR 0028\] The key id of the key that signs results here, and where it is, when there is
/// one: for an agent token, the key the environment names (`HUSHRUN_AGENT`,
/// `HUSHRUN_MACHINE_KEY`, `HUSHRUN_MACHINE_KEY_FILE`), else this machine's.
fn machine(agent: bool) -> Option<(String, String)> {
    let found = if agent {
        crate::machine::named()
    } else {
        crate::machine::load()
    };
    found
        .ok()
        .flatten()
        .map(|machine| (machine.keyid(), machine.source))
}

fn json(identity: &Identity) -> Value {
    let (backend, named_by, directory, credential, withheld) = match &identity.place {
        None => (None, None, None, "none", false),
        Some(Place::Directory(dir)) => (None, None, Some(dir.display().to_string()), "none", false),
        Some(Place::Server {
            base,
            named_by,
            credential,
            withheld,
        }) => (
            Some(base.clone()),
            Some(named_by_key(*named_by)),
            None,
            match credential {
                Credential::Token { agent: true } | Credential::Agent => "agent_token",
                Credential::Token { agent: false } => "token",
                Credential::Login { .. } => "login",
                Credential::GithubOidc => "github_oidc",
                Credential::Nothing => "none",
            },
            *withheld,
        ),
    };
    let info = identity.info.as_ref();
    let text = |field: &str| info.and_then(|i| i[field].as_str()).map(str::to_string);
    json!({
        "schema": "hushrun.whoami/v1",
        "backend": backend,
        "named_by": named_by,
        "directory": directory,
        "off": identity.off,
        "credential": credential,
        "withheld": withheld,
        "org": text("org"),
        "as": text("as"),
        "dashboard": text("dashboard"),
        "token": identity.token().cloned(),
        "error": identity.error.as_ref().map(|(e, _)| e.clone()),
        "machine": machine(credential == "agent_token")
            .map(|(keyid, source)| json!({"keyid": keyid, "source": source})),
    })
}

/// `1 result`, `300 results`.
pub(crate) fn results(n: u64) -> String {
    if n == 1 {
        "1 result".to_string()
    } else {
        format!("{n} results")
    }
}
