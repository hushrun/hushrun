//! A fake Sigstore on the loopback: GitHub Actions' ID token endpoint, Fulcio and a timestamp
//! authority, backed by `hushrun_attest::testing::Pki`, and the `trusted_root.json` that trusts
//! them. Keyless signing is tested end to end with it, without the network. \[M8\] It also
//! signs people in, as Sigstore's OIDC issuer does: discovery, an authorization endpoint that
//! sends the browser straight back with a code, and a token endpoint that checks PKCE.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use base64::Engine as _;
use hushrun_attest::testing::{Pki, github_claims, jwt};
use hushrun_core::digest::Digest;
use hushrun_core::time::Time;

use crate::http::{Reply, Request, Response, Server};

/// The token GitHub's runner gives the job to ask for ID tokens.
pub const REQUEST_TOKEN: &str = "actions-request-token";

/// \[M8\] The person the fake issuer signs in.
pub const PERSON: &str = "alice@acme.dev";

/// A sign-in in progress: what the authorization request asked for.
#[derive(Debug, Clone)]
struct SignIn {
    client: String,
    redirect: String,
    challenge: String,
    nonce: String,
}

/// A fake Sigstore instance. Times default to the clock; tests that pass `--now` set them.
pub struct FakeSigstore {
    pub pki: Arc<Pki>,
    server: Server,
    claims: Arc<Mutex<Value>>,
    time: Arc<Mutex<Option<Time>>>,
    tsa_down: Arc<AtomicBool>,
    /// Every request, in order.
    pub calls: Arc<Mutex<Vec<Request>>>,
}

/// \[M8\] Codes the authorization endpoint handed out, and what each sign-in asked for.
type SignIns = Arc<Mutex<BTreeMap<String, SignIn>>>;

impl Default for FakeSigstore {
    fn default() -> Self {
        FakeSigstore::start()
    }
}

impl FakeSigstore {
    /// An instance whose ID tokens are those of `.github/workflows/deploy.yml` on
    /// `refs/heads/main` of `acme/app` (repository id 123456).
    pub fn start() -> FakeSigstore {
        let pki = Arc::new(Pki::new());
        let claims = Arc::new(Mutex::new(github_claims(
            "acme/app",
            "123456",
            "refs/heads/main",
            ".github/workflows/deploy.yml",
            None,
        )));
        let time = Arc::new(Mutex::new(None::<Time>));
        let tsa_down = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sign_ins: SignIns = Arc::new(Mutex::new(BTreeMap::new()));
        let server = {
            let (pki, claims, time, tsa_down, calls) = (
                pki.clone(),
                claims.clone(),
                time.clone(),
                tsa_down.clone(),
                calls.clone(),
            );
            Server::start(move |request| {
                let base = format!("http://{}", request.header("host").unwrap_or("localhost"));
                calls.lock().expect("calls").push(request.clone());
                let now = time
                    .lock()
                    .expect("time")
                    .unwrap_or_else(|| Time::from_timestamp(jiff::Timestamp::now()));
                Reply::Send(match request.path.as_str() {
                    "/oidc" => oidc(&request, &claims.lock().expect("claims")),
                    "/auth/.well-known/openid-configuration" => Response::json(
                        200,
                        &json!({
                            "issuer": format!("{base}/auth"),
                            "authorization_endpoint": format!("{base}/auth/authorize"),
                            "token_endpoint": format!("{base}/auth/token"),
                        }),
                    ),
                    "/auth/authorize" => authorize(&request, &sign_ins),
                    "/auth/token" => token(&request, &sign_ins, &format!("{base}/auth")),
                    "/api/v2/signingCert" => match pki.fulcio_answer(&request.body, now) {
                        Ok(answer) => Response::json(200, &answer),
                        Err(e) => Response::json(400, &json!({"code": 3, "message": e})),
                    },
                    "/api/v1/timestamp" if tsa_down.load(Ordering::SeqCst) => {
                        Response::bytes(503, "unavailable")
                    }
                    "/api/v1/timestamp" => match pki.answer(&request.body, now) {
                        Ok(answer) => Response::bytes(200, answer)
                            .header("content-type", "application/timestamp-reply"),
                        Err(e) => Response::bytes(400, e),
                    },
                    _ => Response::new(404),
                })
            })
        };
        FakeSigstore {
            pki,
            server,
            claims,
            time,
            tsa_down,
            calls,
        }
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    pub fn tsa_url(&self) -> String {
        format!("{}/api/v1/timestamp", self.url())
    }

    /// \[M8\] The issuer people sign in with, as `HUSHRUN_OIDC_ISSUER` names it.
    pub fn issuer(&self) -> String {
        format!("{}/auth", self.url())
    }

    /// The ID tokens' claims from now on.
    pub fn set_claims(&self, claims: Value) {
        *self.claims.lock().expect("claims") = claims;
    }

    /// The time Fulcio's certificates start at and timestamps say, instead of the clock.
    pub fn set_time(&self, time: Option<Time>) {
        *self.time.lock().expect("time") = time;
    }

    /// Makes the timestamp authority answer 503, or answer again.
    pub fn set_tsa_down(&self, down: bool) {
        self.tsa_down.store(down, Ordering::SeqCst);
    }

    /// Writes the trust root to `dir/trusted_root.json` and returns its path.
    pub fn write_trusted_root(&self, dir: &Path) -> PathBuf {
        let path = dir.join("trusted_root.json");
        std::fs::write(&path, self.pki.trusted_root()).expect("write the trust root");
        path
    }

    /// The environment of a GitHub Actions job that signs keylessly with this instance, its
    /// trust root written under `dir`.
    pub fn env(&self, dir: &Path) -> Vec<(String, String)> {
        let root = self.write_trusted_root(dir);
        [
            ("HUSHRUN_SIGNING", "sigstore".to_string()),
            ("HUSHRUN_FULCIO_URL", self.url()),
            ("HUSHRUN_TSA", self.tsa_url()),
            (
                "HUSHRUN_SIGSTORE_TRUSTED_ROOT",
                root.to_string_lossy().into_owned(),
            ),
            (
                "ACTIONS_ID_TOKEN_REQUEST_URL",
                format!("{}/oidc?api-version=2.0", self.url()),
            ),
            ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", REQUEST_TOKEN.to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    /// The environment of a GitLab CI job that signs keylessly with this instance: its trust
    /// root written under `dir`, and in `SIGSTORE_ID_TOKEN` the token an `id_tokens` entry would
    /// hold, with the current claims (their `aud` as they say).
    pub fn gitlab_env(&self, dir: &Path) -> Vec<(String, String)> {
        let root = self.write_trusted_root(dir);
        let token = jwt(&self.claims.lock().expect("claims"));
        [
            ("HUSHRUN_SIGNING", "sigstore".to_string()),
            ("HUSHRUN_FULCIO_URL", self.url()),
            ("HUSHRUN_TSA", self.tsa_url()),
            (
                "HUSHRUN_SIGSTORE_TRUSTED_ROOT",
                root.to_string_lossy().into_owned(),
            ),
            ("SIGSTORE_ID_TOKEN", token),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    /// How many requests went to `path`.
    pub fn count(&self, path: &str) -> usize {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|r| r.path == path)
            .count()
    }
}

/// The authorization endpoint: the person is signed in at once, and the browser goes back to
/// the client's loopback redirect with a code.
fn authorize(request: &Request, sign_ins: &SignIns) -> Response {
    let (Some(client), Some(redirect), Some(state), Some(challenge), Some(nonce)) = (
        request.query("client_id"),
        request.query("redirect_uri"),
        request.query("state"),
        request.query("code_challenge"),
        request.query("nonce"),
    ) else {
        return Response::bytes(400, "a parameter is missing");
    };
    if request.query("response_type") != Some("code")
        || request.query("code_challenge_method") != Some("S256")
        || !redirect.starts_with("http://localhost:")
    {
        return Response::bytes(400, "not an authorization code request with PKCE");
    }
    let code = format!("code-{}", sign_ins.lock().expect("sign-ins").len());
    sign_ins.lock().expect("sign-ins").insert(
        code.clone(),
        SignIn {
            client: client.to_string(),
            redirect: redirect.to_string(),
            challenge: challenge.to_string(),
            nonce: nonce.to_string(),
        },
    );
    Response::new(302).header("location", format!("{redirect}?code={code}&state={state}"))
}

/// The token endpoint: a code once, for the redirect and client it was issued to, with the
/// PKCE verifier whose SHA-256 is the challenge (RFC 7636).
fn token(request: &Request, sign_ins: &SignIns, issuer: &str) -> Response {
    let form: BTreeMap<String, String> = String::from_utf8_lossy(&request.body)
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (crate::http::decode(k), crate::http::decode(v)))
        .collect();
    let field = |name: &str| form.get(name).map(String::as_str).unwrap_or_default();
    let Some(sign_in) = sign_ins.lock().expect("sign-ins").remove(field("code")) else {
        return Response::json(400, &json!({"error": "invalid_grant"}));
    };
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Digest::of(field("code_verifier").as_bytes()).as_bytes());
    if field("grant_type") != "authorization_code"
        || field("client_id") != sign_in.client
        || field("redirect_uri") != sign_in.redirect
        || challenge != sign_in.challenge
    {
        return Response::json(400, &json!({"error": "invalid_grant"}));
    }
    let claims = json!({
        "iss": issuer,
        "aud": sign_in.client,
        "sub": "CgVhbGljZRIGZ2l0aHVi",
        "email": PERSON,
        "email_verified": true,
        "nonce": sign_in.nonce,
        "exp": 4_102_444_800u64,
    });
    Response::json(
        200,
        &json!({"id_token": jwt(&claims), "token_type": "bearer"}),
    )
}

fn oidc(request: &Request, claims: &Value) -> Response {
    if request.header("authorization") != Some(&format!("Bearer {REQUEST_TOKEN}")) {
        return Response::json(401, &json!({"message": "Bad credentials"}));
    }
    let mut claims = claims.clone();
    claims["aud"] = request.query("audience").unwrap_or_default().into();
    Response::json(200, &json!({"count": 1, "value": jwt(&claims)}))
}
