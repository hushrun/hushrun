//! hushrun's hosted backend, and what `hushrun login` and `hushrun init` ask of a backend beyond
//! storing results: signing in with a device code (C-13), which org a URL is and who the caller
//! is to it, letting a repository's CI record results with its own ID token, and the repository's
//! ledger (C-12).
//!
//! Every request goes to one origin only, with the personal token `hushrun login` saved for that
//! origin: a token never leaves its own host.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde::Deserialize;
use serde_json::{Value, json};

use hushrun_core::ledger_url::LedgerUrl;
use hushrun_core::policy::{POLICY_FILE, Policy};
use hushrun_ledger::{HttpLedger, Options, Secret};

use crate::credentials::{self, Credentials, Login};
use crate::exit::{Exit, Failure};
use crate::results::{NamedBy, error_message, plain, refusal};

/// hushrun cloud's backend, when nothing names another.
pub const HOSTED_ORIGIN: &str = "https://api.hushrun.com";

/// The hosted backend's origin: `HUSHRUN_HOST`, else [`HOSTED_ORIGIN`], without a trailing slash.
pub fn origin() -> String {
    let given = hushrun_ci::var("HUSHRUN_HOST")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty());
    match given {
        // As logins are kept: the scheme and host lowercase, and no path. One that doesn't
        // parse stays as given, for the request that uses it to say why.
        Some(text) => LedgerUrl::parse(&text)
            .map(|url| credentials::origin(&url))
            .unwrap_or(text),
        None => HOSTED_ORIGIN.to_string(),
    }
}

/// Whether an agent token from the environment goes to `origin` whoever named it (C-13): hushrun's
/// host, which the environment names and the server there binds the token's org at (ADR 0027).
/// \[ADR 0046\] `HUSHRUN_AGENT`'s goes to the origin of the backend it came with, and no other.
pub fn agent_token_goes_to(env: &hushrun_ci::Env, to: &str) -> bool {
    match env.agent() {
        Some(agent) => credentials::origin(agent.backend()) == to,
        None => to == origin(),
    }
}

/// The backend a repository names, committed with its code so that CI, teammates and agents
/// need no variable (C-11).
pub fn repository_backend(root: &Path) -> Option<String> {
    named_backend(root).map(|(url, _)| url)
}

/// The repository's backend, and the file that names it (C-11): package.json's
/// `"hushrun": {"backend": …}`, else \[ADR 0037\] hushrun.yaml's `backend`. Whichever names it,
/// it gets the tokens a file in the repository may name (C-13). \[ADR 0061\] When both name one,
/// and not the same, package.json's still wins, and hushrun says so once.
pub fn named_backend(root: &Path) -> Option<(String, NamedBy)> {
    let package = package_backend(root);
    let policy = policy_backend(root);
    if let Some(line) = disagreement(package.as_deref(), policy.as_deref()) {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| tracing::warn!("{line}"));
    }
    package
        .map(|url| (url, NamedBy::Repository))
        .or_else(|| policy.map(|url| (url, NamedBy::Policy)))
}

/// \[ADR 0061\] What to say when package.json and hushrun.yaml name two backends: results go to
/// one org and, from the other file, a reader expects them in another. An interrupted move
/// (`hushrun init --org`) leaves them so, since it writes hushrun.yaml first.
fn disagreement(package: Option<&str>, policy: Option<&str>) -> Option<String> {
    let (package, policy) = (package?, policy?);
    let same = |text: &str| {
        LedgerUrl::parse(text.trim()).map_or_else(|_| text.trim().to_string(), |u| u.to_string())
    };
    (same(package) != same(policy)).then(|| {
        let finish = org_of(policy).map_or_else(String::new, |org| {
            format!(": hushrun init --org {org} finishes a move there, or make the two the same")
        });
        format!(
            "package.json's hushrun.backend is {package} and hushrun.yaml's backend is {policy}, so results go to package.json's{finish}"
        )
    })
}

fn package_backend(root: &Path) -> Option<String> {
    let bytes = std::fs::read(root.join("package.json")).ok()?;
    let manifest: Value = serde_json::from_slice(&bytes).ok()?;
    manifest["hushrun"]["backend"]
        .as_str()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

fn policy_backend(root: &Path) -> Option<String> {
    let bytes = std::fs::read(root.join(POLICY_FILE)).ok()?;
    let policy = Policy::parse(POLICY_FILE, &bytes).ok()?;
    policy.backend.map(|url| url.to_string())
}

/// The org's URL in `base`, such as `acme` in `https://host/o/acme`.
pub fn org_of(base: &str) -> Option<String> {
    let url = LedgerUrl::parse(base.trim()).ok()?.to_string();
    let path = url.split_once("://")?.1.split_once('/')?.1;
    let mut parts = path.trim_end_matches('/').split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("o"), Some(org), None) if !org.is_empty() => Some(org.to_string()),
        _ => None,
    }
}

/// Whether `text` can be an org's slug (api-v1 §1), `^[a-z][a-z0-9-]{1,38}$`. It goes into URLs,
/// so nothing else does.
fn is_org_slug(text: &str) -> bool {
    let bytes = text.as_bytes();
    (2..=39).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `--org`'s value.
pub fn org_slug(text: &str) -> Result<String, String> {
    if is_org_slug(text) {
        Ok(text.to_string())
    } else {
        Err("an organization's slug is 2 to 39 characters: a lowercase letter, then lowercase letters, digits and -".to_string())
    }
}

/// The URL of the org `org` at `origin`.
pub fn org_url(origin: &str, org: &str) -> String {
    format!("{}/o/{org}", origin.trim_end_matches('/'))
}

/// An org the login's person belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MyOrg {
    pub slug: String,
    pub name: String,
    /// `owner`, `admin` or `member`.
    pub role: String,
}

/// What `GET {origin}/cli/orgs` answers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MyOrgs {
    pub orgs: Vec<MyOrg>,
    /// The dashboard, whose `/new` page makes another org.
    #[serde(default)]
    pub dashboard: Option<String>,
}

/// What the backend says of the login's orgs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgList {
    Orgs(MyOrgs),
    /// No login is saved, or the backend doesn't take it any more: a sign-in is the way on.
    SignedOut,
    /// A backend from before the list (404, 405).
    Older,
    /// It couldn't say, in these words. Not knowing isn't a reason to take the last org.
    Failed(String),
}

/// How long a plan waits for the list. It has nothing else to wait for, and a real init, which
/// stops without the list, gives a slow server the time its other requests get.
pub const PLAN_BUDGET: Duration = Duration::from_secs(10);

/// `GET {origin}/cli/orgs` (results-v0 §5): the orgs the login for `origin` belongs to.
pub fn my_orgs(origin: &str, budget: Option<Duration>) -> OrgList {
    let Ok(url) = LedgerUrl::parse(origin.trim()) else {
        return OrgList::SignedOut;
    };
    let saved = Credentials::load().ok();
    let Some(login) = saved.as_ref().and_then(|saved| saved.login_for(&url)) else {
        return OrgList::SignedOut;
    };
    let options = Options {
        token: Some(Secret::new(login.token.as_str())),
        budget,
        ..Options::default()
    };
    let answer = HttpLedger::open(&url, &options)
        .and_then(|client| client.json_request("GET", "cli/orgs", None));
    let (status, body) = match answer {
        Ok(answer) => answer,
        Err(err) => return OrgList::Failed(format!("{:#}", plain(err))),
    };
    match status {
        200 => match serde_json::from_slice::<MyOrgs>(&body) {
            Ok(mut mine) => {
                // An org's URL is made from its slug.
                mine.orgs.retain(|org| is_org_slug(&org.slug));
                OrgList::Orgs(mine)
            }
            Err(err) => OrgList::Failed(format!(
                "{origin}: the list of organizations isn't one: {err}"
            )),
        },
        401 => OrgList::SignedOut,
        404 | 405 => OrgList::Older,
        _ => OrgList::Failed(
            error_message(&body).unwrap_or_else(|| format!("{origin} answered {status}")),
        ),
    }
}

/// A finished sign-in.
#[derive(Debug, Clone)]
pub struct SignedIn {
    /// Who the token acts as.
    pub email: String,
    /// The org the person approved the sign-in for, when the server asked.
    pub org: Option<String>,
    /// That org's URL, which hushrun now sends results to.
    pub backend: Option<String>,
    /// The dashboard that approved the sign-in: its verification URL without `/device…`, where
    /// someone without an org creates or joins one.
    pub dashboard: Option<String>,
}

/// `https://app.example` from `https://app.example/device?code=…`.
fn dashboard_of(verification_url: &str) -> Option<String> {
    let url = verification_url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let path = rest.find(['/', '?', '#']).map_or("", |at| &rest[at..]);
    let host = &rest[..rest.len() - path.len()];
    let base = path.split(['?', '#']).next().unwrap_or_default();
    let base = base
        .strip_suffix("/device")
        .unwrap_or(base)
        .trim_end_matches('/');
    (!host.is_empty()).then(|| format!("{scheme}://{host}{base}"))
}

#[derive(Deserialize)]
struct Started {
    device_code: String,
    user_code: String,
    verification_url: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct IssuedMachine {
    keyid: String,
}

#[derive(Deserialize)]
struct Issued {
    token: String,
    email: String,
    #[serde(default)]
    machine: Option<IssuedMachine>,
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    backend: Option<String>,
}

/// Signs in to `origin` with a device code (C-13): the server gives a code, a person signed in to
/// its dashboard approves it, for `org_hint` when they choose so, and hushrun saves the token for
/// `origin`, and the org's URL as the backend when the server names one.
pub fn sign_in(
    origin: &str,
    org_hint: Option<&str>,
    no_browser: bool,
) -> Result<SignedIn, Failure> {
    // \[ADR 0046\] Every sign-in registers this machine's key as a person's, and beside
    // `HUSHRUN_AGENT` that key is the agent's: it would sign what only people may.
    if hushrun_ci::Env::from_process().agent_set() {
        return Err(Failure::usage(anyhow!(
            "signing in is for people, and HUSHRUN_AGENT is set here: it would register the agent's key as a person's machine. Sign in on your own machine, or unset HUSHRUN_AGENT"
        )));
    }
    // Logins are kept by origin, as `credentials` compares them: `https://Host/` is
    // `https://host`.
    let origin = credentials::origin(&parse(origin)?);
    let origin = origin.as_str();
    let host = open(origin, None)?;
    let mut start = json!({"client": client()});
    if let Some(org) = org_hint {
        start["org"] = json!(org);
    }
    // ADR 0028: every sign-in registers this machine's key, made the first time.
    let machine = match crate::machine::load_or_create() {
        Ok(machine) => Some(machine),
        Err(err) => {
            eprintln!("warning: this sign-in won't register the machine: {err:#}");
            None
        }
    };
    let name = crate::machine::name();
    if let Some(machine) = &machine {
        start["machine"] = json!({"key": machine.public_key().to_string(), "name": name});
    }
    let started: Started = post(&host, origin, "cli/login", &start)?;
    println!(
        "Open {} and approve the code {} there.",
        started.verification_url, started.user_code
    );
    if !no_browser {
        open_browser(&started.verification_url);
    }
    eprintln!("Waiting for approval…");

    let deadline = Instant::now() + Duration::from_secs(started.expires_in);
    let mut interval = Duration::from_secs(started.interval.max(1));
    let mut poll = json!({"device_code": started.device_code});
    if let Some(machine) = &machine {
        poll["proof"] = json!(machine.device_proof(&started.device_code));
    }
    let mut said_busy = false;
    let issued: Issued = loop {
        std::thread::sleep(interval);
        let answer = host.json_request(
            "POST",
            "cli/login/token",
            Some(&serde_json::to_vec(&poll).map_err(anyhow::Error::from)?),
        );
        let waiting = match answer {
            Ok((200, body)) => {
                break serde_json::from_slice(&body)
                    .map_err(|e| anyhow!("{origin}: the token answer isn't one: {e}"))?;
            }
            Ok((status, body)) => match poll_answer(status, &body) {
                Polled::Pending => None,
                Polled::SlowDown => {
                    interval += Duration::from_secs(5);
                    None
                }
                Polled::Busy => Some(
                    error_message(&body).unwrap_or_else(|| format!("{origin} answered {status}")),
                ),
                // [ADR 0028, amended 2026-10-28] This machine's key was revoked from the
                // dashboard (or with its owner's membership), and stays revoked: a new key signs
                // in.
                Polled::MachineRevoked => {
                    let why = error_message(&body)
                        .unwrap_or_else(|| format!("{origin} answered {status}"));
                    let fix = match machine.as_ref().map(|m| m.source.as_str()) {
                        Some("HUSHRUN_MACHINE_KEY") | None => {
                            "set HUSHRUN_MACHINE_KEY to a new key".to_string()
                        }
                        Some(path) => format!("remove {path}"),
                    };
                    return Err(Failure::new(
                        Exit::Unexpected,
                        anyhow!(
                            "not signed in: {why}. To make this machine a new key, {fix}, then run `hushrun login` again"
                        ),
                    ));
                }
                // A 409 `machine_limit` says which page frees a place.
                Polled::Ended => {
                    return Err(Failure::new(
                        Exit::Unexpected,
                        anyhow!(
                            "not signed in: {}",
                            error_message(&body)
                                .unwrap_or_else(|| format!("{origin} answered {status}"))
                        ),
                    ));
                }
            },
            // \[ADR 0061\] The code may be approved already: a host that is starting, or out of
            // reach for a moment, is asked again until the code expires.
            Err(err) => Some(format!("{:#}", plain(err))),
        };
        if let Some(why) = &waiting {
            if !said_busy {
                eprintln!("hushrun: {why}: asking again until the code expires");
                said_busy = true;
            }
            tracing::debug!("polling for the sign-in: {why}");
        }
        // Whatever the server asks, the code expires when it said.
        if Instant::now() >= deadline {
            return Err(match waiting {
                Some(why) => Failure::new(Exit::Ledger, anyhow!("not signed in: {why}")),
                None => Failure::new(
                    Exit::Unexpected,
                    anyhow!(
                        "not signed in: the code {} expired before it was approved: run the command again for a new one",
                        started.user_code
                    ),
                ),
            });
        }
    };

    let backend = issued
        .backend
        .as_deref()
        .and_then(|text| LedgerUrl::parse(text.trim()).ok())
        // A token is good at its own origin only: a backend elsewhere is no use to it.
        .filter(|url| credentials::origin(url) == origin)
        .map(|url| url.to_string());
    let registered = match (&machine, &issued.machine) {
        (Some(machine), Some(answer)) if answer.keyid == machine.keyid() => {
            Some((name, answer.keyid.clone()))
        }
        _ => None,
    };
    let login = Login {
        token: issued.token,
        email: issued.email.clone(),
        machine: registered.as_ref().map(|(_, keyid)| keyid.clone()),
        machine_org: registered.as_ref().and_then(|_| issued.org.clone()),
    };
    let ((), path) = Credentials::update(|saved| {
        saved.hosts.insert(origin.to_string(), login);
        if let Some(backend) = &backend {
            saved.backend = Some(backend.clone());
        }
    })?;
    tracing::info!("saved the token in {}", path.display());
    match (&registered, &machine) {
        (Some((name, keyid)), _) => println!(
            "Registered this machine, {name} ({}), for its writes to {origin}.",
            short_keyid(keyid)
        ),
        // Approved for no org (someone waiting to join one): a machine is registered in an org,
        // so this one is, by hushrun run, on its first write to an org that takes it.
        (None, Some(_)) if issued.org.is_none() => eprintln!(
            "note: the sign-in names no organization yet, so this machine isn't registered: hushrun registers it on its first write to one"
        ),
        (None, Some(_)) => eprintln!(
            "note: {origin} doesn't register machines, so this machine's writes go unsigned there"
        ),
        (None, None) => {}
    }
    Ok(SignedIn {
        email: issued.email,
        org: issued.org,
        backend,
        dashboard: dashboard_of(&started.verification_url),
    })
}

/// \[ADR 0061\] What an answer to a poll for the sign-in's token, other than 200, says to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Polled {
    /// Nobody approved the code yet: ask again.
    Pending,
    /// Ask again, 5 s more apart.
    SlowDown,
    /// The host is starting, overloaded or failing (429, 5xx): ask again until the code expires,
    /// since the approval may be in already.
    Busy,
    /// \[ADR 0028\] This machine's key was revoked for good.
    MachineRevoked,
    /// Anything else ends the sign-in: denied, expired, a limit.
    Ended,
}

fn poll_answer(status: u16, body: &[u8]) -> Polled {
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["code"].as_str().map(str::to_string));
    match code.as_deref() {
        Some("authorization_pending") => Polled::Pending,
        Some("slow_down") => Polled::SlowDown,
        Some("machine_revoked") => Polled::MachineRevoked,
        _ if status == 429 || status >= 500 => Polled::Busy,
        _ => Polled::Ended,
    }
}

/// `sha256:3f2a91c0e4b7…`: a key id cut to 12 hex digits, as people read one.
pub fn short_keyid(keyid: &str) -> String {
    match keyid.strip_prefix("sha256:") {
        Some(hex) if hex.len() > 12 => format!("sha256:{}…", &hex[..12]),
        _ => keyid.to_string(),
    }
}

/// The backend `hushrun login` saved, when it is on `origin` and a login for `origin` exists.
pub fn saved_backend(origin: &str) -> Option<String> {
    let origin = credentials::origin(&LedgerUrl::parse(origin.trim()).ok()?);
    let saved = Credentials::load().ok()?;
    let signed_in = saved.login_at(&origin).is_some();
    let backend = saved.backend?;
    let url = LedgerUrl::parse(&backend).ok()?;
    (credentials::origin(&url) == origin && signed_in).then_some(backend)
}

/// Which org a backend URL is, to the saved login.
#[derive(Debug, Clone)]
pub struct OrgInfo {
    pub org: String,
    /// The org's page in the dashboard, when the server says.
    pub dashboard: Option<String>,
}

/// The backend refused `GET {base}/v0` with 403 or 404: the login's person isn't a member of the
/// org (results-v0 §3 answers non-members 404, as for an org that doesn't exist).
#[derive(Debug)]
pub struct NotAMember(pub anyhow::Error);

impl std::fmt::Display for NotAMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for NotAMember {}

/// The backend refused `GET {base}/v0` with 401: it doesn't take the saved login any more, which
/// was revoked, signed out elsewhere or expired. Signing in again is the way on.
#[derive(Debug)]
pub struct SignedOut(pub anyhow::Error);

impl std::fmt::Display for SignedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for SignedOut {}

/// `GET {base}/v0` with the saved login for `base`'s origin. A refusal with 403 or 404 is a
/// [`NotAMember`], and one with 401 a [`SignedOut`], which callers can tell apart with
/// `downcast_ref`.
pub fn org_info(base: &str) -> anyhow::Result<OrgInfo> {
    let (client, base) = signed_client(base)?;
    let (status, body) = client.json_request("GET", "v0", None).map_err(plain)?;
    if status == 401 {
        return Err(SignedOut(refusal(&base, status, &body, "checking access")).into());
    }
    if matches!(status, 403 | 404) {
        return Err(NotAMember(refusal(&base, status, &body, "checking access")).into());
    }
    if status != 200 {
        return Err(refusal(&base, status, &body, "checking access"));
    }
    let info: Value =
        serde_json::from_slice(&body).map_err(|e| anyhow!("{base}: the answer isn't JSON: {e}"))?;
    let text = |field: &str| info[field].as_str().map(str::to_string);
    Ok(OrgInfo {
        org: text("org").ok_or_else(|| anyhow!("{base}: the answer names no org"))?,
        dashboard: text("dashboard"),
    })
}

/// What connecting a repository did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connected {
    /// The backend now takes results from the repository's CI.
    New,
    /// It already did.
    Already,
    /// The backend has no such request: CI needs connecting in its own settings.
    Unsupported,
    /// The backend refused, in its own words.
    Refused(String),
}

/// `POST {base}/v0/repositories` (C-12): lets `repository` (`host/owner/name`) record results at
/// `base` from CI with its own ID token, no secret. A personal token only.
pub fn connect_repository(base: &str, repository: &str) -> anyhow::Result<Connected> {
    let (client, base) = signed_client(base)?;
    let body = serde_json::to_vec(&json!({"repository": repository}))?;
    let (status, answer) = client
        .json_request("POST", "v0/repositories", Some(&body))
        .map_err(plain)?;
    Ok(match status {
        201 => Connected::New,
        200..=299 => Connected::Already,
        404 | 405 => Connected::Unsupported,
        _ => Connected::Refused(
            error_message(&answer).unwrap_or_else(|| format!("{base} answered {status}")),
        ),
    })
}

/// A request to join an org, as `{base}/v0/access-requests` answers it (results-v0 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRequest {
    /// `pending`, `approved` or `declined`.
    pub status: String,
    /// When it was asked, RFC 3339.
    pub requested_at: Option<String>,
}

/// What the backend says of the login's request to join the org at `base`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// The person's latest request, if any, and whether they are a member already.
    Known {
        member: bool,
        request: Option<AccessRequest>,
    },
    /// Filed now (`created`), or the open one answered.
    Requested { created: bool },
    /// The backend has no such request: someone asks an admin for an invitation instead.
    Unsupported,
    /// Refused, with the error's code (`already_member`, `invited`, `declined`, …) and its words.
    Refused { code: String, why: String },
}

fn access_request(value: &Value) -> Option<AccessRequest> {
    Some(AccessRequest {
        status: value["status"].as_str()?.to_string(),
        requested_at: value["requested_at"].as_str().map(str::to_string),
    })
}

fn error_code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["code"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// `GET {base}/v0/access-requests`: the login's latest request to join the org.
pub fn access_state(base: &str) -> anyhow::Result<Access> {
    let (client, base) = signed_client(base)?;
    let (status, answer) = client
        .json_request("GET", "v0/access-requests", None)
        .map_err(plain)?;
    Ok(match status {
        200 => {
            let value: Value = serde_json::from_slice(&answer)
                .map_err(|e| anyhow!("{base}: the answer isn't JSON: {e}"))?;
            Access::Known {
                member: value["member"].as_bool().unwrap_or(false),
                request: access_request(&value["request"]),
            }
        }
        404 | 405 => Access::Unsupported,
        _ => Access::Refused {
            code: error_code(&answer),
            why: error_message(&answer).unwrap_or_else(|| format!("{base} answered {status}")),
        },
    })
}

/// `POST {base}/v0/access-requests` `{"repository"?}`: asks the org's admins to let the login's
/// person in. Asking again answers the open request; nothing makes a second one.
pub fn request_access(base: &str, repository: Option<&str>) -> anyhow::Result<Access> {
    let (client, base) = signed_client(base)?;
    let body = match repository {
        Some(repository) => json!({"repository": repository}),
        None => json!({}),
    };
    let (status, answer) = client
        .json_request(
            "POST",
            "v0/access-requests",
            Some(&serde_json::to_vec(&body)?),
        )
        .map_err(plain)?;
    Ok(match status {
        200..=299 => Access::Requested {
            created: status == 201,
        },
        // A backend without the request, or an org that doesn't exist: nobody to ask here.
        404 | 405 => Access::Unsupported,
        _ => Access::Refused {
            code: error_code(&answer),
            why: error_message(&answer).unwrap_or_else(|| format!("{base} answered {status}")),
        },
    })
}

/// The org's ledger for a repository, as the backend answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    pub name: String,
    /// Its URL, as `HUSHRUN_REPOSITORY_URL` takes it.
    pub url: String,
    /// Its page in the backend's dashboard, when the backend says.
    pub dashboard: Option<String>,
    /// GitHub's id for the repository whose CI may write to it with an ID token, once known.
    pub github_repository_id: Option<i64>,
    /// The repository's default branch, as the ledger keeps it (ADR 0026 item 10).
    pub default_branch: Option<String>,
    /// This request made it.
    pub created: bool,
    /// For a dry run the backend understood: whether the org has it (`Some(true)`), or the
    /// request would make it (`Some(false)`). `None` for a request, or a backend from before
    /// `dry_run`, which then answered as for a request.
    pub exists: Option<bool>,
}

/// What asking for a repository's ledger did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerAnswer {
    Ledger(Ledger),
    /// The backend has no such request: it keeps no ledgers, or makes them in its own settings.
    Unsupported,
    /// The backend refused, with its status, in its own words.
    Refused {
        status: u16,
        why: String,
    },
}

/// What init asks `POST {base}/v0/ledgers` for (C-12, results-v0 §3).
#[derive(Debug, Clone, Copy, Default)]
pub struct LedgerAsk<'a> {
    /// `host/owner/name`.
    pub repository: &'a str,
    /// GitHub's id for the repository, when `gh` told it, so that its CI can write there with
    /// an ID token.
    pub github_repository_id: Option<i64>,
    /// The repository's default branch, where its trust lives (ADR 0026 item 10).
    pub default_branch: Option<&'a str>,
    /// Find it, or say what would be made, without making anything (ADR 0026 item 19).
    pub dry_run: bool,
}

/// `POST {base}/v0/ledgers` (C-12): the org's ledger for a repository, which the backend makes
/// when the org has none and the login's person may make one, unless `dry_run`. A personal token
/// only.
pub fn repository_ledger(base: &str, ask: LedgerAsk<'_>) -> anyhow::Result<LedgerAnswer> {
    let (client, base) = signed_client(base)?;
    let mut body = json!({"repository": ask.repository});
    if let Some(id) = ask.github_repository_id {
        body["github_repository_id"] = json!(id);
    }
    if let Some(branch) = ask.default_branch {
        body["default_branch"] = json!(branch);
    }
    if ask.dry_run {
        body["dry_run"] = json!(true);
    }
    let (status, answer) = client
        .json_request("POST", "v0/ledgers", Some(&serde_json::to_vec(&body)?))
        .map_err(plain)?;
    match status {
        200..=299 => {
            let value: Value = serde_json::from_slice(&answer)
                .map_err(|e| anyhow!("{base}: the answer for the repository isn't JSON: {e}"))?;
            let text = |field: &str| value[field].as_str().map(str::to_string);
            let dry_run = value["dry_run"].as_bool() == Some(true);
            Ok(LedgerAnswer::Ledger(Ledger {
                name: text("name")
                    .ok_or_else(|| anyhow!("{base}: the answer gives the repository no name"))?,
                url: text("url").unwrap_or_default(),
                dashboard: text("dashboard"),
                github_repository_id: value["github_repository_id"].as_i64(),
                default_branch: text("default_branch"),
                created: status == 201,
                exists: dry_run.then(|| value["exists"].as_bool().unwrap_or(true)),
            }))
        }
        404 | 405 => Ok(LedgerAnswer::Unsupported),
        _ => Ok(LedgerAnswer::Refused {
            status,
            why: error_message(&answer).unwrap_or_else(|| format!("{base} answered {status}")),
        }),
    }
}

/// \[C-38\] Whether the org's backend at `base` keeps what machines saw test files read (hints),
/// asked once with the saved login: `POST {base}/v0/units/hints/lookup` at a place no check has.
/// `Some(false)` when it answers 404 or 405, a server from before them; `Some(true)` when it
/// answers; `None` when it can't tell (unreachable, refused), and then init says nothing of them.
pub fn keeps_hints(base: &str, repository: Option<&str>) -> Option<bool> {
    let (client, _) = signed_client(base).ok()?;
    let nowhere = "0".repeat(64);
    let body = json!({
        "repository": repository,
        "checkId": nowhere,
        "address": nowhere,
        "os": hushrun_core::hints::Os::this().map_or("linux", hushrun_core::hints::Os::word),
    });
    let bytes = serde_json::to_vec(&body).ok()?;
    let (status, _) = client
        .json_request("POST", "v0/units/hints/lookup", Some(&bytes))
        .map_err(|e| tracing::debug!("asking for hints: {e}"))
        .ok()?;
    match status {
        200..=299 => Some(true),
        404 | 405 => Some(false),
        _ => None,
    }
}

/// A client for `base` that carries the login saved for its origin.
fn signed_client(base: &str) -> anyhow::Result<(HttpLedger, String)> {
    let url = LedgerUrl::parse(base.trim()).map_err(|e| anyhow!("{base}: {e}"))?;
    let saved = Credentials::load()?;
    let login = saved
        .login_for(&url)
        .ok_or_else(|| anyhow!("not signed in to {}", credentials::origin(&url)))?;
    let client = open(&url.to_string(), Some(&login.token)).map_err(|failure| failure.error)?;
    Ok((client, url.to_string()))
}

/// A client for `base`, with `token` if given.
pub(crate) fn open(base: &str, token: Option<&str>) -> Result<HttpLedger, Failure> {
    let options = Options {
        token: token.map(Secret::new),
        ..Options::default()
    };
    HttpLedger::open(&parse(base)?, &options).map_err(|e| Failure::new(Exit::Ledger, plain(e)))
}

fn parse(base: &str) -> Result<LedgerUrl, Failure> {
    LedgerUrl::parse(base.trim()).map_err(|e| {
        Failure::usage(anyhow!(
            "hushrun signs in to an https:// backend, such as https://hushrun.example/o/acme: {e}"
        ))
    })
}

fn post<T: serde::de::DeserializeOwned>(
    host: &HttpLedger,
    origin: &str,
    path: &str,
    body: &Value,
) -> Result<T, Failure> {
    let bytes = serde_json::to_vec(body).map_err(anyhow::Error::from)?;
    let (status, answer) = host
        .json_request("POST", path, Some(&bytes))
        .map_err(|e| Failure::new(Exit::Ledger, plain(e)))?;
    if status != 200 {
        return Err(Failure::new(
            Exit::Ledger,
            refusal(origin, status, &answer, "starting a sign-in"),
        ));
    }
    serde_json::from_slice(&answer)
        .map_err(|e| Failure::new(Exit::Ledger, anyhow!("{origin}: {path}: {e}")))
}

/// What the approval page shows the person: hushrun's version and this machine's name.
fn client() -> String {
    let host = Command::new("hostname")
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty());
    let version = env!("CARGO_PKG_VERSION");
    match host {
        Some(host) => format!("hushrun {version} on {host}"),
        None => format!("hushrun {version}"),
    }
}

pub(crate) fn open_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        Command::new("xdg-open")
    };
    let _ = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-13
    #[test]
    fn a_poll_waits_for_a_busy_host_and_ends_on_a_refusal() {
        let error = |code: &str| format!(r#"{{"error": {{"code": "{code}"}}}}"#).into_bytes();
        assert_eq!(
            poll_answer(400, &error("authorization_pending")),
            Polled::Pending
        );
        assert_eq!(poll_answer(400, &error("slow_down")), Polled::SlowDown);
        assert_eq!(
            poll_answer(409, &error("machine_revoked")),
            Polled::MachineRevoked
        );
        assert_eq!(poll_answer(503, b"<html>starting</html>"), Polled::Busy);
        assert_eq!(poll_answer(429, &error("rate_limited")), Polled::Busy);
        assert_eq!(poll_answer(400, &error("access_denied")), Polled::Ended);
        assert_eq!(poll_answer(400, &error("expired_token")), Polled::Ended);
        assert_eq!(poll_answer(409, &error("machine_limit")), Polled::Ended);
    }

    #[test]
    fn the_dashboard_is_the_verification_url_without_its_page() {
        for (url, dashboard) in [
            (
                "https://app.hushrun.com/device?code=BCDF-GHJK",
                Some("https://app.hushrun.com"),
            ),
            (
                "http://127.0.0.1:23010/device?code=X",
                Some("http://127.0.0.1:23010"),
            ),
            (
                "https://example.com/hushrun/device",
                Some("https://example.com/hushrun"),
            ),
            ("https://example.com", Some("https://example.com")),
            ("ftp://example.com/device", None),
            ("not a url", None),
        ] {
            assert_eq!(dashboard_of(url).as_deref(), dashboard, "{url}");
        }
    }

    #[test]
    fn org_slugs_are_what_a_url_can_carry() {
        for good in ["ab", "acme", "acme-2", "a1"] {
            assert_eq!(org_slug(good).as_deref(), Ok(good), "{good}");
        }
        let long = "a".repeat(40);
        for bad in [
            "", "a", "Acme", "2acme", "ac me", "../x", "a/b", "a_b", &long,
        ] {
            assert!(org_slug(bad).is_err(), "{bad}");
        }
        assert_eq!(org_url("https://host/", "acme"), "https://host/o/acme");
        assert_eq!(
            org_of(&org_url("https://host", "acme")).as_deref(),
            Some("acme")
        );
    }

    #[test]
    fn org_of_reads_the_org_path() {
        assert_eq!(org_of("https://host/o/acme").as_deref(), Some("acme"));
        assert_eq!(org_of("https://host/o/acme/").as_deref(), Some("acme"));
        assert_eq!(
            org_of("http://127.0.0.1:8787/o/acme").as_deref(),
            Some("acme")
        );
        assert_eq!(org_of("https://host"), None);
        assert_eq!(org_of("https://host/l/acme/shop"), None);
    }

    #[test]
    fn repository_backend_reads_hushrun_yaml_after_package_json() {
        let dir = tempfile::tempdir().unwrap();
        let yaml =
            "version: 1\nrepository: github.com/acme/shop\nbackend: https://yaml.example/o/acme\n";
        std::fs::write(dir.path().join(POLICY_FILE), yaml).unwrap();
        assert_eq!(
            named_backend(dir.path()),
            Some(("https://yaml.example/o/acme".into(), NamedBy::Policy))
        );
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"hushrun": {"backend": "https://json.example/o/acme"}}"#,
        )
        .unwrap();
        assert_eq!(
            named_backend(dir.path()),
            Some(("https://json.example/o/acme".into(), NamedBy::Repository))
        );
        // A hushrun.yaml that doesn't parse names nothing.
        std::fs::remove_file(dir.path().join("package.json")).unwrap();
        std::fs::write(dir.path().join(POLICY_FILE), "version: 2\n").unwrap();
        assert_eq!(named_backend(dir.path()), None);
    }

    // covers: C-11
    #[test]
    fn two_files_that_name_two_backends_are_said() {
        let a = "https://host/o/acme";
        assert_eq!(disagreement(Some(a), None), None);
        assert_eq!(disagreement(None, Some(a)), None);
        assert_eq!(disagreement(Some(a), Some("https://HOST/o/acme/")), None);
        assert_eq!(
            disagreement(Some(a), Some("https://host/o/initech")).as_deref(),
            Some(
                "package.json's hushrun.backend is https://host/o/acme and hushrun.yaml's backend is https://host/o/initech, so results go to package.json's: hushrun init --org initech finishes a move there, or make the two the same"
            )
        );
    }

    #[test]
    fn repository_backend_reads_package_json() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(repository_backend(dir.path()), None);
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name": "shop", "hushrun": {"backend": " https://host/o/acme "}}"#,
        )
        .unwrap();
        assert_eq!(
            repository_backend(dir.path()).as_deref(),
            Some("https://host/o/acme")
        );
        std::fs::write(dir.path().join("package.json"), r#"{"hushrun": {}}"#).unwrap();
        assert_eq!(repository_backend(dir.path()), None);
    }
}
