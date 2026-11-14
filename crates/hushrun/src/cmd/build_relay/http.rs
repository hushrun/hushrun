//! B-26: the relay's requests to the org's shared store of build blocks (the build protocol), and to the
//! object store its links point at. Every answer is sorted into what the relay does next by one
//! function, [`Failure`]'s kind: a server that can't be reached (the run may go on offline), a
//! refusal of the caller (401, 403 or 404 in the middle of a run stop sharing for the run), or a
//! failure that may pass (5xx, 429, a word this version doesn't know).

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Why a request didn't give an answer to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The server or the object store couldn't be reached, or didn't answer in time.
    Unreachable(String),
    /// 401, 403 or 404: the caller is refused (a removed member, a revoked machine, sharing
    /// turned off). The status and the error's code.
    Refused(u16, Option<String>),
    /// Anything else: a server's failure, a body that isn't the answer, another status.
    Failed(String),
}

impl Failure {
    pub(crate) fn line(&self) -> String {
        match self {
            Failure::Unreachable(why) | Failure::Failed(why) => why.clone(),
            Failure::Refused(status, Some(code)) => format!("answered {status} {code}"),
            Failure::Refused(status, None) => format!("answered {status}"),
        }
    }
}

/// An answer, with the server's date (the `Date` header, in milliseconds since the epoch).
pub(crate) struct Answered<T> {
    pub(crate) body: T,
    pub(crate) date_ms: Option<u64>,
}

/// The relay's HTTP client: the org's base URL and its bearer token. The token goes only to the
/// base's origin; a link to the object store carries only its own headers.
pub(crate) struct Client {
    http: reqwest::Client,
    base: String,
    credential: super::credential::Credential,
    /// The run is a CI job's (B-31).
    in_ci: bool,
}

fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Client {
    pub(crate) fn new(
        base: &str,
        credential: super::credential::Credential,
        in_ci: bool,
    ) -> Result<Client, String> {
        install_crypto_provider();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("hushrun/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|err| format!("building the HTTP client: {err}"))?;
        Ok(Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            credential,
            in_ci,
        })
    }

    /// B-31: whether this client's requests say they come from CI: a CI job's whose credential
    /// is a token (`hushrun_core::blocks::CI_HEADER`), as its results lookup says too. A job's
    /// own ID token says who it is by itself.
    fn says_ci(&self) -> bool {
        self.in_ci && matches!(self.credential, super::credential::Credential::Token(_))
    }

    /// Whether the run's credential is a CI job's own ID token (B-31): what a server trusts
    /// a job by, and what a block bound to a job's folders is offered on (B-24).
    pub(crate) fn is_a_jobs_id_token(&self) -> bool {
        self.credential.is_a_jobs_id_token()
    }

    /// What tells this client's credential apart, never the secret.
    pub(crate) fn fingerprint(&self) -> String {
        self.credential.fingerprint()
    }

    /// B-31: no ID token is minted from the start of a command's end wait to its check's end.
    pub(crate) fn ending(&self, ending: bool) {
        self.credential.ending(ending);
    }

    /// `POST {base}/{path}` with `body`, within `timeout`; one more try after a 503 or 429 whose
    /// `Retry-After` fits, or a connection that failed at once (every request may be sent again).
    pub(crate) async fn post<B: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        timeout: Duration,
        signature: Option<String>,
    ) -> Result<Answered<R>, Failure> {
        let bytes = serde_json::to_vec(body).map_err(|e| Failure::Failed(e.to_string()))?;
        self.post_bytes(path, bytes, timeout, signature).await
    }

    /// [`Client::post`] of a body already serialized: what a signature covers.
    pub(crate) async fn post_bytes<R: DeserializeOwned>(
        &self,
        path: &str,
        bytes: Vec<u8>,
        timeout: Duration,
        signature: Option<String>,
    ) -> Result<Answered<R>, Failure> {
        let url = format!("{}/{path}", self.base);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut tries = 0;
        // The token a 401 refused: never sent again.
        let mut refused: Option<String> = None;
        loop {
            tries += 1;
            // B-31: a token to mint is waited for within this request's deadline.
            let token = self
                .credential
                .bearer(&self.http, refused.as_deref(), deadline)
                .await?;
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Err(Failure::Unreachable(format!("{url}: no answer in time")));
            }
            let mut request = self
                .http
                .post(&url)
                .bearer_auth(&token)
                .header("content-type", "application/json")
                .timeout(left)
                .body(bytes.clone());
            if let Some(signature) = &signature {
                request = request.header("hushrun-signature", signature);
            }
            if self.says_ci() {
                request = request.header(hushrun_core::blocks::CI_HEADER, "1");
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(err) => {
                    if tries < 2 && err.is_connect() && !err.is_timeout() {
                        tokio::time::sleep(Duration::from_millis(200).min(left)).await;
                        continue;
                    }
                    return Err(Failure::Unreachable(format!("{url}: {}", chain(&err))));
                }
            };
            let status = response.status().as_u16();
            let date_ms = response
                .headers()
                .get("date")
                .and_then(|v| v.to_str().ok())
                .and_then(http_date_ms);
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let body = response
                .bytes()
                .await
                .map_err(|err| Failure::Unreachable(format!("{url}: {}", chain(&err))))?;
            match status {
                200..=299 => {
                    return serde_json::from_slice(&body)
                        .map(|body| Answered { body, date_ms })
                        .map_err(|err| {
                            Failure::Failed(format!("{url}: the answer isn't one: {err}"))
                        });
                }
                // An ID token the backend no longer takes: mint a fresh one and try once more.
                401 if refused.is_none() && self.credential.renews() => {
                    refused = Some(token);
                    tries -= 1;
                }
                401 | 403 | 404 | 405 => {
                    let code = serde_json::from_slice::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v["error"]["code"].as_str().map(str::to_string));
                    return Err(Failure::Refused(status, code));
                }
                429 | 503 if tries < 2 => {
                    let wait = retry_after.unwrap_or(Duration::from_secs(1));
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if wait >= left {
                        return Err(Failure::Failed(format!("{url}: answered {status}")));
                    }
                    tokio::time::sleep(wait).await;
                }
                _ => return Err(Failure::Failed(format!("{url}: answered {status}"))),
            }
        }
    }

    /// The object store's request for a link, the token never sent.
    pub(crate) fn link(
        &self,
        link: &hushrun_core::blocks::Link,
    ) -> Option<reqwest::RequestBuilder> {
        if matches!(self.credential, super::credential::Credential::Nothing) {
            return None;
        }
        let method = reqwest::Method::from_bytes(link.method.as_bytes()).ok()?;
        let mut request = self.http.request(method, &link.url);
        for (name, value) in &link.headers {
            request = request.header(name, value);
        }
        Some(request)
    }
}

/// An error and its sources.
pub(crate) fn chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(next) = source {
        let more = next.to_string();
        if !text.contains(&more) {
            text.push_str(": ");
            text.push_str(&more);
        }
        source = next.source();
    }
    text
}

/// An HTTP date (RFC 2822 form, as servers send it) in milliseconds since the epoch.
fn http_date_ms(text: &str) -> Option<u64> {
    let when = jiff::fmt::rfc2822::parse(text.trim()).ok()?.timestamp();
    u64::try_from(when.as_millisecond()).ok()
}
