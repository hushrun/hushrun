//! B-26, B-31: what the relay's requests carry. A person's or an agent's token, as it is; or in
//! GitHub Actions the job's ID token, whose audience is the backend. A token is held in memory
//! only: nothing here writes one anywhere.
//!
//! The job's ID token has one home, [`Tokens`], and one place that decides what a request
//! carries, [`Credential::bearer`]:
//!
//! - a token held with more than a minute left is sent as it is;
//! - one with less starts a mint in the background, and is still sent until its real expiry, also
//!   when the renewal fails;
//! - a request with no token it may send (none held, the held one expired, or the one a 401
//!   refused) waits for the mint in flight, within its own deadline: a mint that fails or is late
//!   is, for that request, a server that can't be reached.
//!
//! At most one mint is in flight, and it gets [`MINT_FOR`] whoever waits for it. None starts once
//! the end of the command has begun ([`Credential::ending`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use super::http::Failure;

/// A token is minted again when it has less than this left.
const FRESH_FOR: Duration = Duration::from_secs(60);
/// How long one mint gets, whoever waits for it.
const MINT_FOR: Duration = Duration::from_secs(5);

pub(crate) enum Credential {
    /// B-36: a sandbox's run from what a fetch saved: nothing is sent anywhere.
    Nothing,
    Token(String),
    GithubOidc(Arc<Oidc>),
}

/// A GitHub Actions job's ID tokens (B-31).
pub(crate) struct Oidc {
    /// `ACTIONS_ID_TOKEN_REQUEST_URL`.
    url: String,
    /// `ACTIONS_ID_TOKEN_REQUEST_TOKEN`.
    request_token: String,
    /// The backend's URL: the token's audience, as the results client asks it.
    audience: String,
    /// The job's digest (B-31), which no other job has.
    job: String,
    tokens: Mutex<Tokens>,
    /// Moves on when a mint ends, either way.
    minted: tokio::sync::watch::Sender<u64>,
    /// The end of the command has begun: no mint starts.
    ending: AtomicBool,
}

/// The token held, and the mint in flight.
#[derive(Default)]
struct Tokens {
    held: Option<Held>,
    minting: bool,
    /// Why the last mint failed, for the requests that waited for it.
    failed: Option<String>,
}

struct Held {
    token: String,
    /// When the token expires, from its `exp`; `None`: it says nothing, and is taken as good.
    expires: Option<SystemTime>,
}

impl Held {
    /// Whether the token may still be sent: it hasn't expired, and isn't the one refused.
    fn usable(&self, now: SystemTime, refused: Option<&str>) -> bool {
        refused != Some(self.token.as_str()) && self.expires.is_none_or(|at| at > now)
    }

    /// Whether it has more than [`FRESH_FOR`] left.
    fn fresh(&self, now: SystemTime) -> bool {
        self.expires
            .is_none_or(|at| at.duration_since(now).is_ok_and(|left| left > FRESH_FOR))
    }
}

impl Credential {
    pub(crate) fn of(access: &crate::results::BuildCredential, audience: &str) -> Credential {
        match access {
            crate::results::BuildCredential::Token(token) => Credential::Token(token.clone()),
            crate::results::BuildCredential::GithubOidc {
                url,
                request_token,
                job,
            } => Credential::GithubOidc(Arc::new(Oidc {
                url: url.clone(),
                request_token: request_token.clone(),
                audience: audience.to_string(),
                job: job.clone(),
                tokens: Mutex::new(Tokens::default()),
                minted: tokio::sync::watch::Sender::new(0),
                ending: AtomicBool::new(false),
            })),
        }
    }

    /// What tells two credentials apart in a block's record and a marker (B-32), never the
    /// secret: a token's fingerprint, or a GitHub Actions job's own (B-31), of its repository,
    /// run, attempt, job and request token, so that no other job's markers are ever its.
    pub(crate) fn fingerprint(&self) -> String {
        match self {
            Credential::Nothing => hushrun_build::origin::fingerprint("no credential"),
            Credential::Token(token) => hushrun_build::origin::fingerprint(token),
            Credential::GithubOidc(oidc) => hushrun_build::origin::fingerprint(&format!(
                "github-actions-id-token\n{}",
                oidc.job
            )),
        }
    }

    /// Whether a 401 is worth a fresh token and one more try.
    pub(crate) fn renews(&self) -> bool {
        matches!(self, Credential::GithubOidc(_))
    }

    /// B-31: from the start of a command's end wait (`true`) until its check is over (`false`),
    /// no mint starts; a request sends the token held while it is good, or nothing.
    pub(crate) fn ending(&self, ending: bool) {
        if let Credential::GithubOidc(oidc) = self {
            oidc.ending.store(ending, Ordering::Relaxed);
        }
    }

    /// The bearer token for a request whose deadline is `deadline`; `refused` is the token a
    /// 401 just refused, never sent again.
    pub(crate) async fn bearer(
        &self,
        http: &reqwest::Client,
        refused: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<String, Failure> {
        let oidc = match self {
            Credential::Token(token) => return Ok(token.clone()),
            Credential::Nothing => {
                return Err(Failure::Unreachable(
                    "this run reads only what a fetch saved, and sends nothing".into(),
                ));
            }
            Credential::GithubOidc(oidc) => oidc,
        };
        let mut minted = oidc.minted.subscribe();
        minted.borrow_and_update();
        {
            let mut tokens = oidc.tokens();
            let now = SystemTime::now();
            let usable = tokens
                .held
                .as_ref()
                .filter(|held| held.usable(now, refused));
            if let Some(held) = usable
                && held.fresh(now)
            {
                return Ok(held.token.clone());
            }
            let usable = usable.map(|held| held.token.clone());
            if !tokens.minting && !oidc.ending.load(Ordering::Relaxed) {
                tokens.minting = true;
                tokio::spawn(Oidc::mint(oidc.clone(), http.clone()));
            }
            if let Some(token) = usable {
                return Ok(token);
            }
            if !tokens.minting {
                return Err(Failure::Unreachable(
                    "the job's ID token: none to send, and the command has ended".into(),
                ));
            }
        }
        // The mint in flight, within this request's deadline.
        match tokio::time::timeout_at(deadline, minted.changed()).await {
            Ok(Ok(())) => {}
            _ => {
                return Err(Failure::Unreachable(
                    "the job's ID token: not minted in time".into(),
                ));
            }
        }
        let tokens = oidc.tokens();
        match tokens
            .held
            .as_ref()
            .filter(|held| held.usable(SystemTime::now(), refused))
        {
            Some(held) => Ok(held.token.clone()),
            None => Err(Failure::Unreachable(format!(
                "the job's ID token: {}",
                tokens.failed.as_deref().unwrap_or("none minted")
            ))),
        }
    }
}

impl Oidc {
    fn tokens(&self) -> MutexGuard<'_, Tokens> {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One mint, within [`MINT_FOR`]: its token replaces the one held; a failure leaves the one
    /// held as it is.
    async fn mint(oidc: Arc<Oidc>, http: reqwest::Client) {
        let minted = oidc.ask(&http).await;
        {
            let mut tokens = oidc.tokens();
            tokens.minting = false;
            match minted {
                Ok(token) => {
                    tokens.held = Some(Held {
                        expires: expiry(&token),
                        token,
                    });
                    tokens.failed = None;
                }
                Err(why) => tokens.failed = Some(why),
            }
        }
        oidc.minted.send_modify(|n| *n = n.wrapping_add(1));
    }

    async fn ask(&self, http: &reqwest::Client) -> Result<String, String> {
        let separator = if self.url.contains('?') { '&' } else { '?' };
        let request = format!(
            "{}{separator}audience={}",
            self.url,
            percent(&self.audience)
        );
        let answer = http
            .get(&request)
            .bearer_auth(&self.request_token)
            .header("accept", "application/json")
            .timeout(MINT_FOR)
            .send()
            .await
            .map_err(|err| super::http::chain(&err))?;
        if !answer.status().is_success() {
            return Err(format!(
                "the endpoint answered {}",
                answer.status().as_u16()
            ));
        }
        let body: serde_json::Value = answer.json().await.map_err(|err| err.to_string())?;
        body.get("value")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.contains(char::is_whitespace))
            .map(str::to_string)
            .ok_or_else(|| "no `value`".to_string())
    }
}

/// `application/x-www-form-urlencoded` for one value.
fn percent(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// When a JWT expires, from its `exp` claim; the backend verifies the token, this only decides
/// when to mint the next.
pub(crate) fn expiry(token: &str) -> Option<SystemTime> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(claims.get("exp")?.as_u64()?))
}
