//! Signers and services on the network (feature `sigstore`): the CI's ID token, Fulcio, RFC
//! 3161 timestamp authorities, and the TUF repository of a trust root.
//! Calls block on a small runtime of their own, as HTTP ledgers do (ADR 0005). Nothing here
//! logs a token, a signature request's credentials or an Authorization header.

use std::fmt;
use std::future::Future;
use std::sync::Once;
use std::time::Duration;

use crate::signer::SignError;

pub mod fulcio;
pub mod oidc;
pub mod tsa;
pub mod tuf;

/// Each request's timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The most bytes read from any answer.
const MAX_ANSWER: usize = 4 << 20;

/// A secret, such as an ID token: `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// An HTTP client and the runtime its calls block on.
pub struct Http {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl fmt::Debug for Http {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Http")
    }
}

/// An answer, read whole.
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// At most 300 characters of the body, for error messages.
    pub fn excerpt(&self) -> String {
        let text = String::from_utf8_lossy(&self.body);
        let text = text.trim();
        match text.char_indices().nth(300) {
            Some((cut, _)) => format!("{}…", &text[..cut]),
            None => text.to_string(),
        }
    }

    /// The body as JSON.
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("not JSON: {e}"))
    }
}

impl Http {
    pub fn new() -> Result<Http, SignError> {
        install_crypto_provider();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| SignError::Unavailable(format!("starting the I/O runtime: {e}")))?;
        // Redirects are not followed: a signing request never goes anywhere it wasn't sent.
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(hushrun_core::user_agent())
            .build()
            .map_err(|e| SignError::Unavailable(format!("building the HTTP client: {e}")))?;
        Ok(Http { runtime, client })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// Sends `request` and reads the answer, up to 4 MiB. Errors name `what`, never the
    /// request's headers or body.
    pub fn send(&self, what: &str, request: reqwest::RequestBuilder) -> Result<Answer, SignError> {
        self.block_on(async {
            let mut response = request
                .send()
                .await
                .map_err(|e| SignError::Unavailable(format!("{what}: {}", e.without_url())))?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| SignError::Unavailable(format!("{what}: {}", e.without_url())))?
            {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_ANSWER {
                    return Err(SignError::Unavailable(format!(
                        "{what}: the answer is over 4 MiB"
                    )));
                }
            }
            Ok(Answer { status, body })
        })
    }

    /// Sends what `request` builds, retrying twice after timeouts, dropped connections, 429
    /// and 5xx, with backoff.
    pub fn send_retrying(
        &self,
        what: &str,
        request: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<Answer, SignError> {
        let mut pause = Duration::from_millis(200);
        let mut attempt = 1;
        loop {
            let result = self.send(what, request(&self.client));
            let retry = match &result {
                Ok(answer) => answer.status == 429 || answer.status >= 500,
                Err(SignError::Unavailable(_)) => true,
                Err(_) => false,
            };
            if !retry || attempt == 3 {
                return result;
            }
            attempt += 1;
            std::thread::sleep(pause);
            pause *= 2;
        }
    }
}

/// rustls uses ring (plan §12.3); reqwest is built without a provider, so one must be
/// installed before the first client.
fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The hosts `http://` may reach: the loopback, for tests and local fakes.
const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// `url`, parsed, when it may receive requests: `https://`, or `http://` on the loopback.
/// `what` names it in errors, which never show its query.
pub fn checked_url(what: &str, url: &str) -> Result<reqwest::Url, SignError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| SignError::Config(format!("{what} is not a URL")))?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    let allowed = parsed.scheme() == "https"
        || (parsed.scheme() == "http" && LOOPBACK.contains(&host.as_str()));
    if !allowed || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(SignError::Config(format!(
            "{what} must be an https:// URL without credentials, not {}://{host}",
            parsed.scheme()
        )));
    }
    Ok(parsed)
}

/// Joins a base URL and a path without doubling the `/`.
pub fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_https_or_loopback() {
        assert!(checked_url("x", "https://fulcio.sigstore.dev").is_ok());
        assert!(checked_url("x", "http://127.0.0.1:8080/api").is_ok());
        assert!(checked_url("x", "http://localhost:1").is_ok());
        for bad in [
            "http://fulcio.sigstore.dev",
            "ftp://x",
            "https://user:pass@x.dev",
            "not a url",
        ] {
            assert!(checked_url("HUSHRUN_FULCIO_URL", bad).is_err(), "{bad}");
        }
        let err = checked_url("HUSHRUN_FULCIO_URL", "http://evil.dev/?token=abc")
            .unwrap_err()
            .to_string();
        assert!(!err.contains("token=abc"), "{err}");
        assert_eq!(join("https://a.dev/", "/api/v2"), "https://a.dev/api/v2");
        assert_eq!(format!("{:?}", Secret::new("jwt")), "Secret(…)");
    }
}
