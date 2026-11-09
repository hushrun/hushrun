//! Where an HTTP ledger's bearer token comes from (L-10), in order:
//!
//! 1. `HUSHRUN_REPOSITORY_TOKEN`;
//! 2. what `HUSHRUN_REPOSITORY_TOKEN_COMMAND` prints, reused for 5 minutes (ADR 0007 item 6);
//!    \[M19\] it runs with `HUSHRUN_REPOSITORY_URL` set to the ledger's normalized URL;
//! 3. in GitHub Actions, an ID token whose audience is the ledger's normalized URL, cached until
//!    a minute before it expires (the job needs `permissions: id-token: write`);
//! 4. none: requests carry no `Authorization`, and the ledger answers 401.
//!
//! All of them come from the environment only, never from the repository (T-4). Tokens are
//! secrets: nothing here logs them, and only the ledger's origin receives one.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use reqwest::StatusCode;
use reqwest::header;
use tokio::sync::Mutex;

use hushrun_core::agent::is_agent_string;
use hushrun_core::ledger_url::LOOPBACK_HOSTS;

use super::token::TokenCommand;
use super::{Tuning, chain};
use crate::runtime::{backoff_from, excerpt, form};
use crate::{LedgerError, OidcEndpoint, Options, Secret};

/// An ID token is renewed this long before it expires.
const RENEW_BEFORE: Duration = Duration::from_secs(60);
/// How long an ID token whose expiry can't be read is reused.
const UNKNOWN_EXPIRY: Duration = Duration::from_secs(60);

pub(crate) enum Auth {
    None,
    Static(Secret),
    Command(TokenCommand),
    Oidc(Oidc),
}

pub(crate) struct Oidc {
    endpoint: OidcEndpoint,
    /// The normalized ledger URL (L-1).
    audience: String,
    cache: Mutex<Option<(Secret, Instant)>>,
}

impl Auth {
    /// The first source `options` names (L-10's order).
    pub(crate) fn new(options: &Options, audience: &str) -> Auth {
        if let Some(token) = &options.token {
            Auth::Static(token.clone())
        } else if let Some(command) = &options.token_command {
            Auth::Command(TokenCommand::new(command).for_ledger(audience))
        } else if let Some(endpoint) = &options.github_oidc {
            Auth::Oidc(Oidc {
                endpoint: endpoint.clone(),
                audience: audience.to_string(),
                cache: Mutex::new(None),
            })
        } else {
            Auth::None
        }
    }

    /// Where the token comes from, for messages.
    pub(crate) fn source(&self) -> &'static str {
        match self {
            Auth::None => "nowhere",
            Auth::Static(_) => "HUSHRUN_REPOSITORY_TOKEN",
            Auth::Command(_) => "HUSHRUN_REPOSITORY_TOKEN_COMMAND",
            Auth::Oidc(_) => "the GitHub Actions ID token",
        }
    }

    /// The token for the next request, if any.
    pub(crate) async fn token(
        &self,
        client: &reqwest::Client,
        tuning: &Tuning,
    ) -> Result<Option<Secret>, LedgerError> {
        let token = match self {
            Auth::None => None,
            Auth::Static(token) => Some(token.clone()),
            Auth::Command(command) => Some(Secret::new(
                command
                    .token()
                    .await
                    .map_err(|e| LedgerError::Denied(e.0))?,
            )),
            Auth::Oidc(oidc) => Some(oidc.token(client, tuning).await?),
        };
        // \[ADR 0046\] A `HUSHRUN_AGENT` string holds a private key, so it never goes in a header,
        // wherever a token came from and whatever is around it. Every request's token comes
        // through here.
        if token
            .as_ref()
            .is_some_and(|token| is_agent_string(token.expose()))
        {
            return Err(LedgerError::Denied(format!(
                "{} holds a HUSHRUN_AGENT string, which is never sent: set it as HUSHRUN_AGENT",
                self.source()
            )));
        }
        Ok(token)
    }
}

impl Oidc {
    async fn token(
        &self,
        client: &reqwest::Client,
        tuning: &Tuning,
    ) -> Result<Secret, LedgerError> {
        let mut cache = self.cache.lock().await;
        if let Some((token, renew_at)) = cache.as_ref()
            && Instant::now() < *renew_at
        {
            return Ok(token.clone());
        }
        let token = self.request(client, tuning).await?;
        let lifetime = match expiry(token.expose()) {
            Some(expires) => expires
                .duration_since(SystemTime::now())
                .unwrap_or(Duration::ZERO)
                .saturating_sub(RENEW_BEFORE),
            None => UNKNOWN_EXPIRY,
        };
        *cache = Some((token.clone(), Instant::now() + lifetime));
        Ok(token)
    }

    /// Asks GitHub for an ID token, retrying like ledger requests (L-4). Errors name the
    /// endpoint's host only: its URL and the request token stay out of messages.
    async fn request(
        &self,
        client: &reqwest::Client,
        tuning: &Tuning,
    ) -> Result<Secret, LedgerError> {
        let url = reqwest::Url::parse(&self.endpoint.url).map_err(|_| {
            LedgerError::Denied("ACTIONS_ID_TOKEN_REQUEST_URL is not a URL".to_string())
        })?;
        let host = url.host_str().unwrap_or_default().to_string();
        let allowed = url.scheme() == "https"
            || (url.scheme() == "http" && LOOPBACK_HOSTS.contains(&host.as_str()));
        if !allowed {
            return Err(LedgerError::Denied(format!(
                "ACTIONS_ID_TOKEN_REQUEST_URL must be an https:// URL, not {}://{host}",
                url.scheme()
            )));
        }
        let separator = if self.endpoint.url.contains('?') {
            '&'
        } else {
            '?'
        };
        let audience = form(&[("audience", self.audience.as_str())]);
        let request_url = format!("{}{separator}{audience}", self.endpoint.url);
        let what = format!("the GitHub Actions ID token request to {host}");
        let mut attempt = 1;
        loop {
            tracing::debug!("GET {host}: ID token for {}", self.audience);
            let sent = client
                .get(&request_url)
                .bearer_auth(self.endpoint.token.expose())
                .header(header::ACCEPT, "application/json")
                .send()
                .await;
            let retryable = match sent {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        let body = response.bytes().await.map_err(|e| {
                            LedgerError::Unavailable(format!("{what}: {}", chain(e)))
                        })?;
                        return token_value(&body).ok_or_else(|| {
                            LedgerError::Denied(format!("{what}: the answer has no `value` token"))
                        });
                    }
                    let body = response.text().await.unwrap_or_default();
                    let failure = format!("{what}: {status}: {}", excerpt(&body, 200));
                    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                        return Err(LedgerError::Denied(format!(
                            "{failure}; does the job have `permissions: id-token: write`?"
                        )));
                    }
                    if !(status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()) {
                        return Err(LedgerError::Denied(failure));
                    }
                    failure
                }
                Err(err) if super::is_transient(&err) => format!("{what}: {}", chain(err)),
                Err(err) => {
                    return Err(LedgerError::Unavailable(format!("{what}: {}", chain(err))));
                }
            };
            if attempt >= tuning.attempts {
                return Err(LedgerError::Unavailable(retryable));
            }
            tokio::time::sleep(backoff_from(tuning.backoff, attempt)).await;
            attempt += 1;
        }
    }
}

/// The `value` of GitHub's answer, `{"count": …, "value": "<jwt>"}`.
fn token_value(body: &[u8]) -> Option<Secret> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let token = value.get("value")?.as_str()?.trim();
    (!token.is_empty() && !token.contains(char::is_whitespace)).then(|| Secret::new(token))
}

/// When a JWT expires, from its `exp` claim. Only the cache uses it; the ledger verifies the
/// token.
fn expiry(token: &str) -> Option<SystemTime> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = claims.get("exp")?;
    let seconds = exp.as_u64().or_else(|| {
        exp.as_f64()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| s as u64)
    })?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &str) -> String {
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!("{}.{}.sig", b64(r#"{"alg":"RS256"}"#), b64(claims))
    }

    #[test]
    fn reads_the_expiry_of_a_jwt() {
        assert_eq!(
            expiry(&jwt(r#"{"exp":1760000000,"aud":"x"}"#)),
            Some(UNIX_EPOCH + Duration::from_secs(1_760_000_000))
        );
        assert_eq!(
            expiry(&jwt(r#"{"exp":1760000000.5}"#)),
            Some(UNIX_EPOCH + Duration::from_secs(1_760_000_000))
        );
        for opaque in [
            "hrl_abc",
            "a.b.c",
            &jwt(r#"{"aud":"x"}"#),
            &jwt(r#"{"exp":-1}"#),
        ] {
            assert_eq!(expiry(opaque), None, "{opaque}");
        }
    }

    #[test]
    fn reads_githubs_answer() {
        assert_eq!(
            token_value(br#"{"count":1,"value":"eyJ.a.b"}"#).map(|s| s.expose().to_string()),
            Some("eyJ.a.b".to_string())
        );
        assert!(token_value(br#"{"value":""}"#).is_none());
        assert!(token_value(br#"{"value":"a b"}"#).is_none());
        assert!(token_value(b"not json").is_none());
    }

    // covers: L-10
    #[test]
    fn sources_come_in_order() {
        let endpoint = OidcEndpoint {
            url: "https://token.example/idtoken?api-version=2.0".into(),
            token: Secret::new("request-token"),
        };
        let all = Options {
            token: Some(Secret::new("hrl_x")),
            token_command: Some("echo t".into()),
            github_oidc: Some(endpoint.clone()),
            ..Options::default()
        };
        assert_eq!(
            Auth::new(&all, "https://l").source(),
            "HUSHRUN_REPOSITORY_TOKEN"
        );
        let command = Options {
            token: None,
            ..all.clone()
        };
        assert_eq!(
            Auth::new(&command, "https://l").source(),
            "HUSHRUN_REPOSITORY_TOKEN_COMMAND"
        );
        let oidc = Options {
            token_command: None,
            ..command
        };
        assert_eq!(
            Auth::new(&oidc, "https://l").source(),
            "the GitHub Actions ID token"
        );
        assert_eq!(
            Auth::new(&Options::default(), "https://l").source(),
            "nowhere"
        );
    }
}
