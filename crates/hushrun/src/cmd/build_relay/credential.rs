//! B-26, B-31: what the relay's requests carry. A person's or an agent's token, as it is; or in
//! GitHub Actions the job's ID token, whose audience is the backend, minted when the relay holds
//! none, when the one it holds is within a minute of expiring, and again after a 401. A token is
//! held in memory only: nothing here writes one anywhere.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use super::http::Failure;

/// A token is minted again when it has less than this left.
const FRESH_FOR: Duration = Duration::from_secs(60);

pub(crate) enum Credential {
    Token(String),
    GithubOidc {
        /// `ACTIONS_ID_TOKEN_REQUEST_URL`.
        url: String,
        /// `ACTIONS_ID_TOKEN_REQUEST_TOKEN`.
        request_token: String,
        /// The backend's URL: the token's audience, as the results client asks it.
        audience: String,
        held: tokio::sync::Mutex<Option<(String, Option<SystemTime>)>>,
    },
}

impl Credential {
    pub(crate) fn of(access: &crate::results::BuildCredential, audience: &str) -> Credential {
        match access {
            crate::results::BuildCredential::Token(token) => Credential::Token(token.clone()),
            crate::results::BuildCredential::GithubOidc { url, request_token } => {
                Credential::GithubOidc {
                    url: url.clone(),
                    request_token: request_token.clone(),
                    audience: audience.to_string(),
                    held: tokio::sync::Mutex::new(None),
                }
            }
        }
    }

    /// What tells two credentials apart in a block's record and a marker (B-32), never the
    /// secret: a token's fingerprint, or one word for a CI job, whose store dies with it.
    pub(crate) fn fingerprint(&self) -> String {
        match self {
            Credential::Token(token) => hushrun_build::origin::fingerprint(token),
            Credential::GithubOidc { .. } => {
                hushrun_build::origin::fingerprint("github-actions-id-token")
            }
        }
    }

    /// Whether a 401 is worth a fresh token and one more try.
    pub(crate) fn renews(&self) -> bool {
        matches!(self, Credential::GithubOidc { .. })
    }

    /// The bearer token for the next request: `fresh` mints one whatever is held.
    pub(crate) async fn bearer(
        &self,
        http: &reqwest::Client,
        fresh: bool,
    ) -> Result<String, Failure> {
        let (url, request_token, audience, held) = match self {
            Credential::Token(token) => return Ok(token.clone()),
            Credential::GithubOidc {
                url,
                request_token,
                audience,
                held,
            } => (url, request_token, audience, held),
        };
        let mut held = held.lock().await;
        if !fresh
            && let Some((token, expires)) = held.as_ref()
            && expires.is_none_or(|at| {
                at.duration_since(SystemTime::now())
                    .is_ok_and(|left| left > FRESH_FOR)
            })
        {
            return Ok(token.clone());
        }
        let separator = if url.contains('?') { '&' } else { '?' };
        let request = format!("{url}{separator}audience={}", percent(audience));
        let answer = http
            .get(&request)
            .bearer_auth(request_token)
            .header("accept", "application/json")
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|err| {
                Failure::Unreachable(format!("the job's ID token: {}", super::http::chain(&err)))
            })?;
        if !answer.status().is_success() {
            return Err(Failure::Refused(
                answer.status().as_u16(),
                Some("id_token".into()),
            ));
        }
        let body: serde_json::Value = answer
            .json()
            .await
            .map_err(|err| Failure::Failed(format!("the job's ID token: {err}")))?;
        let token = body
            .get("value")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.contains(char::is_whitespace))
            .ok_or_else(|| Failure::Failed("the job's ID token: no `value`".into()))?
            .to_string();
        *held = Some((token.clone(), expiry(&token)));
        Ok(token)
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
