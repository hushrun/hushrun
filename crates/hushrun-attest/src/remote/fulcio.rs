//! Keyless signing (E-11, T-12): an ephemeral P-256 key, never written anywhere, certified by
//! Fulcio for the CI's ID token. A certificate lasts minutes; the signer asks for a new one
//! before it expires, and every signature gets a timestamp that proves when it was made.

use std::sync::Mutex;

use base64::Engine as _;
use serde_json::json;

use hushrun_core::signers::PublicKey;
use hushrun_core::time::{Dur, Time};

use super::oidc::{self, TokenSource};
use super::{Http, Secret, checked_url, join};
use crate::keys::SigningKey;
use crate::signer::{SignError, Signer};
use crate::trust_root::TrustedRoot;
use crate::x509::Cert;

/// Public-good Fulcio.
pub const PUBLIC_GOOD: &str = "https://fulcio.sigstore.dev";
/// The audience Fulcio accepts ID tokens for.
pub const AUDIENCE: &str = "sigstore";
/// A certificate is renewed this long before it expires, so that a signature and its timestamp
/// fit inside it.
const RENEW_BEFORE: Dur = Dur::from_secs(120);

/// A Fulcio instance.
#[derive(Debug, Clone)]
pub struct Fulcio {
    base: String,
}

impl Fulcio {
    pub fn new(base: &str) -> Result<Fulcio, SignError> {
        checked_url("HUSHRUN_FULCIO_URL", base)?;
        Ok(Fulcio {
            base: base.trim_end_matches('/').to_string(),
        })
    }

    /// The host, for messages.
    pub fn host(&self) -> String {
        reqwest::Url::parse(&self.base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// A certificate chain for `key`, leaf first, from `token` (`POST /api/v2/signingCert`). The
    /// proof of possession is `key`'s signature over the token's subject.
    pub fn certificate(
        &self,
        http: &Http,
        token: &Secret,
        key: &SigningKey,
    ) -> Result<Vec<Cert>, SignError> {
        let subject = oidc::subject(token.expose())?;
        let proof = key.sign(subject.as_bytes());
        let pem = der::pem::encode_string(
            "PUBLIC KEY",
            der::pem::LineEnding::LF,
            &key.public_key().encoded(),
        )
        .map_err(|e| SignError::Config(e.to_string()))?;
        let body = json!({
            "credentials": {"oidcIdentityToken": token.expose()},
            "publicKeyRequest": {
                "publicKey": {"algorithm": "ECDSA", "content": pem},
                "proofOfPossession": base64::engine::general_purpose::STANDARD.encode(proof),
            }
        })
        .to_string();
        let url = join(&self.base, "api/v2/signingCert");
        let what = format!("Fulcio ({})", self.host());
        let answer = http.send_retrying(&what, |client| {
            client
                .post(&url)
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .body(body.clone())
        })?;
        if !answer.is_success() {
            return Err(SignError::Unavailable(format!(
                "{what} refused a certificate: HTTP {}: {}",
                answer.status,
                answer.excerpt()
            )));
        }
        let value = answer
            .json()
            .map_err(|e| SignError::Unavailable(format!("{what}: {e}")))?;
        let chain = [
            "signedCertificateEmbeddedSct",
            "signedCertificateDetachedSct",
        ]
        .iter()
        .find_map(|kind| value.get(kind))
        .and_then(|c| c.pointer("/chain/certificates"))
        .and_then(|c| c.as_array())
        .ok_or_else(|| SignError::Unavailable(format!("{what}: an answer without a chain")))?;
        let certs = chain
            .iter()
            .map(|pem| {
                pem.as_str()
                    .ok_or_else(|| "a certificate that isn't a string".to_string())
                    .and_then(Cert::from_pem)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| SignError::Unavailable(format!("{what}: {e}")))?;
        if certs.is_empty() {
            return Err(SignError::Unavailable(format!("{what}: an empty chain")));
        }
        Ok(certs)
    }
}

/// A keyless signer (E-11).
pub struct Keyless {
    http: Http,
    fulcio: Fulcio,
    tokens: Box<dyn TokenSource>,
    root: TrustedRoot,
    clock: Box<dyn Fn() -> Time + Send + Sync>,
    key: SigningKey,
    current: Mutex<Option<Cert>>,
}

impl Keyless {
    /// A signer with a new ephemeral key, which asks `fulcio` for certificates with `tokens`'
    /// ID tokens, and checks them against `root`. `clock` is the time now.
    pub fn new(
        fulcio: Fulcio,
        tokens: Box<dyn TokenSource>,
        root: TrustedRoot,
        clock: Box<dyn Fn() -> Time + Send + Sync>,
    ) -> Result<Keyless, SignError> {
        Ok(Keyless {
            http: Http::new()?,
            fulcio,
            tokens,
            root,
            clock,
            key: SigningKey::generate_p256().map_err(|e| SignError::Config(e.to_string()))?,
            current: Mutex::new(None),
        })
    }

    /// A new certificate, checked: it certifies this signer's key, and the trust root's
    /// certificate authority issued it.
    fn fetch(&self) -> Result<Cert, SignError> {
        let token = self.tokens.id_token(&self.http, AUDIENCE)?;
        let chain = self.fulcio.certificate(&self.http, &token, &self.key)?;
        let leaf = chain.into_iter().next().expect("checked non-empty");
        let key = leaf
            .signing_key()
            .map_err(|e| SignError::Unavailable(format!("Fulcio's certificate: {e}")))?;
        if key != self.key.public_key() {
            return Err(SignError::Unavailable(
                "Fulcio certified another key than the one hushrun sent".into(),
            ));
        }
        // At the certificate's own start, so that this machine's clock doesn't matter.
        self.root
            .verify_signing_certificate(&leaf, leaf.not_before())
            .map_err(|e| {
                SignError::Config(format!(
                    "the certificate {} issued doesn't verify against {}: {e}. HUSHRUN_FULCIO_URL and HUSHRUN_SIGSTORE_TRUSTED_ROOT must name the same Sigstore instance",
                    self.fulcio.host(),
                    self.root.source
                ))
            })?;
        Ok(leaf)
    }
}

impl Signer for Keyless {
    fn public_key(&self) -> Option<PublicKey> {
        None
    }

    fn certificate(&self) -> Result<Option<Vec<u8>>, SignError> {
        let now = (self.clock)();
        let mut current = self.current.lock().expect("certificate lock");
        if let Some(cert) = current.as_ref()
            && cert.not_after() > now.saturating_add(RENEW_BEFORE)
        {
            return Ok(Some(cert.der.clone()));
        }
        let cert = self.fetch()?;
        let der = cert.der.clone();
        *current = Some(cert);
        Ok(Some(der))
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SignError> {
        Ok(self.key.sign(message))
    }

    fn describe(&self) -> String {
        format!(
            "keyless signing ({} with {}'s ID token)",
            self.fulcio.host(),
            self.tokens.describe()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Pki, github_claims, jwt};
    use hushrun_testkit::http::{Reply, Response, Server};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn time(text: &str) -> Time {
        Time::parse(text).unwrap()
    }

    fn fake_fulcio(pki: Arc<Pki>, at: Time, calls: Arc<AtomicUsize>) -> Server {
        Server::start(move |request| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.path, "/api/v2/signingCert");
            match pki.fulcio_answer(&request.body, at) {
                Ok(answer) => Reply::Send(Response::json(200, &answer)),
                Err(e) => Reply::Send(Response::bytes(400, e)),
            }
        })
    }

    fn token() -> Box<dyn TokenSource> {
        Box::new(oidc::Given(Secret::new(jwt(&github_claims(
            "acme/app",
            "123",
            "refs/heads/main",
            ".github/workflows/deploy.yml",
            None,
        )))))
    }

    // covers: E-11, T-12
    #[test]
    fn certificates_are_fetched_checked_and_reused() {
        let pki = Arc::new(Pki::new());
        let issued = time("2026-10-15T12:00:00Z");
        let calls = Arc::new(AtomicUsize::new(0));
        let server = fake_fulcio(pki.clone(), issued, calls.clone());
        let root = TrustedRoot::parse(&pki.trusted_root(), "test").unwrap();
        let now = Arc::new(Mutex::new(issued));
        let clock = now.clone();
        let signer = Keyless::new(
            Fulcio::new(&server.url()).unwrap(),
            token(),
            root,
            Box::new(move || *clock.lock().unwrap()),
        )
        .unwrap();
        let der = signer.certificate().unwrap().unwrap();
        let cert = Cert::parse(&der).unwrap();
        assert_eq!(
            cert.identity().unwrap().repository_id.as_deref(),
            Some("123")
        );
        let signature = signer.sign(b"pae").unwrap();
        assert!(crate::keys::verify_signature(
            &cert.signing_key().unwrap(),
            b"pae",
            &signature
        ));
        // Reused while it has more than two minutes left, then renewed.
        *now.lock().unwrap() = issued.saturating_add(Dur::from_secs(470));
        assert_eq!(signer.certificate().unwrap().unwrap(), der);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        *now.lock().unwrap() = issued.saturating_add(Dur::from_secs(490));
        signer.certificate().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(signer.describe().contains("127.0.0.1"));
    }

    // covers: E-11, T-12
    #[test]
    fn a_certificate_the_trust_root_doesnt_cover_is_refused() {
        let pki = Arc::new(Pki::new());
        let issued = time("2026-10-15T12:00:00Z");
        let server = fake_fulcio(pki, issued, Arc::new(AtomicUsize::new(0)));
        let other = TrustedRoot::parse(&Pki::new().trusted_root(), "another root").unwrap();
        let signer = Keyless::new(
            Fulcio::new(&server.url()).unwrap(),
            token(),
            other,
            Box::new(move || issued),
        )
        .unwrap();
        let err = signer.certificate().unwrap_err();
        assert!(matches!(err, SignError::Config(_)), "{err}");
        assert!(
            err.to_string().contains("HUSHRUN_SIGSTORE_TRUSTED_ROOT"),
            "{err}"
        );
    }

    // covers: E-11
    #[test]
    fn fulcio_refusals_are_unavailable_signers() {
        let server = Server::start(|_| {
            Reply::Send(Response::json(
                401,
                &json!({"code": 16, "message": "error processing the identity token"}),
            ))
        });
        let root = TrustedRoot::parse(&Pki::new().trusted_root(), "test").unwrap();
        let signer = Keyless::new(
            Fulcio::new(&server.url()).unwrap(),
            token(),
            root,
            Box::new(|| time("2026-10-15T12:00:00Z")),
        )
        .unwrap();
        let err = signer.certificate().unwrap_err();
        assert!(matches!(err, SignError::Unavailable(_)), "{err}");
        assert!(err.to_string().contains("identity token"), "{err}");
        assert!(Fulcio::new("http://fulcio.example").is_err());
    }
}
