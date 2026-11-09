//! RFC 3161 timestamp authorities (E-10), tried in order: the first answer whose timestamp
//! verifies against the trust root is the one the bundle carries.

use super::{Http, checked_url};
use crate::signer::{SignError, Timestamper};
use crate::timestamp::{self, REQUEST_CONTENT_TYPE};
use crate::trust_root::TrustedRoot;

/// Public-good Sigstore's timestamp authority.
pub const PUBLIC_GOOD: &str = "https://timestamp.sigstore.dev/api/v1/timestamp";

/// Timestamp authorities, by URL, and the trust root their timestamps must verify against.
pub struct Authorities {
    http: Http,
    urls: Vec<reqwest::Url>,
    root: TrustedRoot,
}

impl Authorities {
    pub fn new(urls: &[String], root: TrustedRoot) -> Result<Authorities, SignError> {
        if urls.is_empty() {
            return Err(SignError::Config(
                "no timestamp authority is configured".into(),
            ));
        }
        let urls = urls
            .iter()
            .map(|url| checked_url("HUSHRUN_TSA", url))
            .collect::<Result<_, _>>()?;
        Ok(Authorities {
            http: Http::new()?,
            urls,
            root,
        })
    }
}

impl Timestamper for Authorities {
    fn timestamp(&self, signature: &[u8]) -> Result<Vec<u8>, SignError> {
        let mut reasons = Vec::new();
        for url in &self.urls {
            let host = url.host_str().unwrap_or_default();
            let nonce = timestamp::nonce();
            let request = timestamp::request(signature, &nonce);
            let answer = self.http.send(host, {
                self.http
                    .client()
                    .post(url.clone())
                    .header("content-type", REQUEST_CONTENT_TYPE)
                    .header("accept", "application/timestamp-reply")
                    .body(request)
            });
            let reason = match answer {
                Ok(answer) if answer.is_success() => {
                    match timestamp::verify(&answer.body, signature, &self.root, Some(&nonce)) {
                        Ok(_) => return Ok(answer.body),
                        Err(e) => format!("{e} (against {})", self.root.source),
                    }
                }
                Ok(answer) => format!("HTTP {}: {}", answer.status, answer.excerpt()),
                Err(e) => e.to_string(),
            };
            reasons.push(format!("{host}: {reason}"));
        }
        Err(SignError::Unavailable(format!(
            "no timestamp authority answered with a timestamp that verifies ({})",
            reasons.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Pki;
    use hushrun_core::time::Time;
    use hushrun_testkit::http::{Reply, Response, Server};
    use std::sync::Arc;

    fn authority(pki: Arc<Pki>, at: Time) -> Server {
        Server::start(move |request| {
            assert_eq!(request.header("content-type"), Some(REQUEST_CONTENT_TYPE));
            match pki.answer(&request.body, at) {
                Ok(answer) => Reply::Send(Response::bytes(200, answer)),
                Err(e) => Reply::Send(Response::bytes(400, e)),
            }
        })
    }

    // covers: E-10
    #[test]
    fn authorities_are_tried_in_order() {
        let pki = Arc::new(Pki::new());
        let at = Time::parse("2026-10-15T12:00:00Z").unwrap();
        let root = TrustedRoot::parse(&pki.trusted_root(), "test").unwrap();
        let down = Server::start(|_| Reply::Send(Response::bytes(503, "down")));
        let stranger = authority(Arc::new(Pki::new()), at);
        let good = authority(pki.clone(), at);
        let authorities =
            Authorities::new(&[down.url(), stranger.url(), good.url()], root.clone()).unwrap();
        let answer = authorities.timestamp(b"signature").unwrap();
        assert_eq!(
            timestamp::verify(&answer, b"signature", &root, None)
                .unwrap()
                .time,
            at
        );
        // None that verifies: the error names each.
        let none = Authorities::new(&[down.url(), stranger.url()], root).unwrap();
        let err = none.timestamp(b"signature").unwrap_err();
        assert!(matches!(err, SignError::Unavailable(_)));
        let text = err.to_string();
        assert!(
            text.contains("HTTP 503") && text.contains("no timestamp authority of test"),
            "{text}"
        );
        assert!(Authorities::new(&[], TrustedRoot::public_good().clone()).is_err());
        assert!(
            Authorities::new(
                &["http://tsa.example".into()],
                TrustedRoot::public_good().clone()
            )
            .is_err()
        );
    }
}
