//! ID tokens for keyless signing (T-12): GitHub Actions' (`ACTIONS_ID_TOKEN_REQUEST_URL` and
//! `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, which a job gets with `permissions: id-token: write`),
//! GitLab CI's (an `id_tokens` entry, M9), a token another step put in `SIGSTORE_ID_TOKEN`, as
//! Sigstore's other clients read it, or \[M8\] a person's, from signing in with their browser.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::Engine as _;
use hushrun_core::time::Time;

use super::{Http, Secret, checked_url, join};
use crate::signer::SignError;

/// Where ID tokens come from.
pub trait TokenSource: Send + Sync {
    /// An ID token for `audience`.
    fn id_token(&self, http: &Http, audience: &str) -> Result<Secret, SignError>;
    /// Where tokens come from, for messages.
    fn describe(&self) -> &'static str;
}

/// GitHub Actions' ID token endpoint.
pub struct GithubActions {
    url: String,
    token: Secret,
}

impl GithubActions {
    pub fn new(url: impl Into<String>, token: Secret) -> GithubActions {
        GithubActions {
            url: url.into(),
            token,
        }
    }
}

impl TokenSource for GithubActions {
    fn id_token(&self, http: &Http, audience: &str) -> Result<Secret, SignError> {
        let mut url = checked_url("ACTIONS_ID_TOKEN_REQUEST_URL", &self.url)?;
        url.query_pairs_mut().append_pair("audience", audience);
        let what = "the GitHub Actions ID token";
        let answer = http.send_retrying(what, |client| {
            client
                .get(url.clone())
                .bearer_auth(self.token.expose())
                .header("accept", "application/json")
        })?;
        if !answer.is_success() {
            return Err(SignError::Unavailable(format!(
                "{what}: HTTP {}: {}; the job needs `permissions: id-token: write`",
                answer.status,
                answer.excerpt()
            )));
        }
        answer
            .json()
            .ok()
            .and_then(|v| v.get("value").and_then(|t| t.as_str()).map(Secret::new))
            .ok_or_else(|| SignError::Unavailable(format!("{what}: an answer without `value`")))
    }

    fn describe(&self) -> &'static str {
        "GitHub Actions"
    }
}

/// A token GitLab CI minted when the job started, for an `id_tokens` entry such as
/// `SIGSTORE_ID_TOKEN: {aud: sigstore}` (T-12 \[M9\]). GitLab can't mint one for another
/// audience later, so a token for another audience, or one that expired, is refused here, with
/// the entry to declare, before Fulcio sees it.
pub struct GitlabIdToken {
    variable: &'static str,
    token: Secret,
    clock: Box<dyn Fn() -> Time + Send + Sync>,
}

impl GitlabIdToken {
    /// The token in `variable`, checked against `clock`'s time.
    pub fn new(
        variable: &'static str,
        token: Secret,
        clock: Box<dyn Fn() -> Time + Send + Sync>,
    ) -> GitlabIdToken {
        GitlabIdToken {
            variable,
            token,
            clock,
        }
    }
}

impl TokenSource for GitlabIdToken {
    fn id_token(&self, _http: &Http, audience: &str) -> Result<Secret, SignError> {
        let var = self.variable;
        let declare =
            format!("declare it in the job's `id_tokens` as `{var}: {{aud: {audience}}}`");
        let claims = claims(self.token.expose())
            .map_err(|_| SignError::Config(format!("{var} is not an ID token: {declare}")))?;
        let audiences: Vec<&str> = match claims.get("aud") {
            Some(serde_json::Value::String(one)) => vec![one.as_str()],
            Some(serde_json::Value::Array(many)) => {
                many.iter().filter_map(serde_json::Value::as_str).collect()
            }
            _ => Vec::new(),
        };
        if !audiences.contains(&audience) {
            let found = if audiences.is_empty() {
                "no audience".to_string()
            } else {
                format!("audience {}", audiences.join(", "))
            };
            return Err(SignError::Config(format!(
                "the ID token in {var} has {found}, not {audience}: {declare}"
            )));
        }
        let now = (self.clock)().millis() / 1000;
        if let Some(exp) = claims.get("exp").and_then(serde_json::Value::as_i64)
            && exp <= now
        {
            return Err(SignError::Unavailable(format!(
                "the ID token in {var} expired {} s ago: GitLab mints it when the job starts, valid for the job's timeout",
                now - exp
            )));
        }
        Ok(self.token.clone())
    }

    fn describe(&self) -> &'static str {
        "GitLab CI's id_tokens"
    }
}

/// A token another step obtained (`SIGSTORE_ID_TOKEN`).
pub struct Given(pub Secret);

impl TokenSource for Given {
    fn id_token(&self, _http: &Http, _audience: &str) -> Result<Secret, SignError> {
        Ok(self.0.clone())
    }

    fn describe(&self) -> &'static str {
        "SIGSTORE_ID_TOKEN"
    }
}

/// The identity Fulcio certifies for GitLab CI's claims, as its `gitlab-pipeline` template maps
/// them (E-11 \[M9\]): the subject and both workflow URIs are `https://<ci_config_ref_uri>`, the
/// repository `<iss>/<project_path>`, the ref `refs/heads/<ref>` or `refs/tags/<ref>`, and the
/// repository id `project_id`.
pub fn gitlab_identity(
    claims: &serde_json::Value,
) -> Result<hushrun_core::signers::CertIdentity, SignError> {
    let claim = |name: &str| {
        claims
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.is_empty())
    };
    let missing =
        |name: &str| SignError::Config(format!("the GitLab ID token has no `{name}` claim"));
    let issuer = claim("iss").ok_or_else(|| missing("iss"))?;
    let config = claim("ci_config_ref_uri").ok_or_else(|| {
        SignError::Config(
            "the GitLab ID token has no `ci_config_ref_uri`: GitLab sets it only when the pipeline definition is in the project itself, and Fulcio certifies nothing without it".into(),
        )
    })?;
    let config = format!("https://{config}");
    let reference = claim("ref").ok_or_else(|| missing("ref"))?;
    let kind = if claim("ref_type") == Some("branch") {
        "heads"
    } else {
        "tags"
    };
    Ok(hushrun_core::signers::CertIdentity {
        issuer: issuer.to_string(),
        subject: config.clone(),
        repository_id: Some(
            claim("project_id")
                .ok_or_else(|| missing("project_id"))?
                .to_string(),
        ),
        repository: Some(format!(
            "{}/{}",
            issuer.trim_end_matches('/'),
            claim("project_path").ok_or_else(|| missing("project_path"))?
        )),
        reference: Some(format!("refs/{kind}/{reference}")),
        workflow: Some(config.clone()),
        signer_workflow: Some(config),
        trigger: claim("pipeline_source").map(str::to_string),
    })
}

/// \[M8\] Public-good Sigstore's sign-in for people, which Fulcio trusts: it federates GitHub,
/// Google and Microsoft accounts.
pub const SIGSTORE_ISSUER: &str = "https://oauth2.sigstore.dev/auth";
/// The client people sign in to it with.
pub const SIGSTORE_CLIENT: &str = "sigstore";

/// \[M8\] A person's ID token, through OAuth 2.0's authorization code flow with PKCE (RFC 7636)
/// and a loopback redirect (RFC 8252), as Sigstore's clients sign people in: hushrun listens
/// on 127.0.0.1, shows the issuer's sign-in page, and exchanges the code it is sent back for
/// an ID token. The token's audience is the client, so `audience` must be it.
pub struct Interactive {
    /// The OIDC issuer, whose discovery document names its endpoints.
    pub issuer: String,
    pub client_id: String,
    /// Shows the sign-in URL to the person: opens their browser, and prints it.
    pub show: Box<dyn Fn(&str) + Send + Sync>,
    /// How long to wait for the person.
    pub wait: Duration,
}

impl TokenSource for Interactive {
    fn id_token(&self, http: &Http, audience: &str) -> Result<Secret, SignError> {
        if audience != self.client_id {
            return Err(SignError::Config(format!(
                "signing in with client {} gives tokens for {0}, not {audience}",
                self.client_id
            )));
        }
        let discovery = checked_url(
            "the OIDC issuer",
            &join(&self.issuer, ".well-known/openid-configuration"),
        )?;
        let what = "the OIDC issuer's discovery document";
        let answer = http.send_retrying(what, |client| client.get(discovery.clone()))?;
        let document = answer
            .json()
            .ok()
            .filter(|_| answer.is_success())
            .ok_or_else(|| {
                SignError::Unavailable(format!(
                    "{what}: HTTP {}: {}",
                    answer.status,
                    answer.excerpt()
                ))
            })?;
        let endpoint = |name: &str| {
            document[name]
                .as_str()
                .ok_or_else(|| SignError::Unavailable(format!("{what} names no {name}")))
                .and_then(|url| checked_url(name, url))
        };
        let (authorize, token_url) = (
            endpoint("authorization_endpoint")?,
            endpoint("token_endpoint")?,
        );

        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| SignError::Unavailable(format!("listening for the sign-in: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| SignError::Unavailable(format!("listening for the sign-in: {e}")))?
            .port();
        let redirect = format!("http://localhost:{port}/auth/callback");
        let verifier = random_token()?;
        let state = random_token()?;
        let nonce = random_token()?;
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()),
        );
        let mut url = authorize;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("scope", "openid email")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        (self.show)(url.as_str());
        let code = receive_code(&listener, &state, self.wait)?;

        let what = "the OIDC issuer's token endpoint";
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code.expose()),
            ("redirect_uri", redirect.as_str()),
            ("client_id", self.client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ];
        // application/x-www-form-urlencoded, encoded as a URL's query is.
        let mut body = reqwest::Url::parse("http://localhost/").expect("a URL");
        body.query_pairs_mut().extend_pairs(form);
        let body = body.query().unwrap_or_default().to_string();
        let answer = http.send(
            what,
            http.client()
                .post(token_url)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("accept", "application/json")
                .body(body),
        )?;
        if !answer.is_success() {
            return Err(SignError::Unavailable(format!(
                "{what}: HTTP {}: {}",
                answer.status,
                answer.excerpt()
            )));
        }
        let token = answer
            .json()
            .ok()
            .and_then(|v| v.get("id_token").and_then(|t| t.as_str()).map(Secret::new))
            .ok_or_else(|| SignError::Unavailable(format!("{what}: an answer without id_token")))?;
        // The token must be the one this sign-in asked for.
        if claims(token.expose())?
            .get("nonce")
            .and_then(|n| n.as_str())
            != Some(nonce.as_str())
        {
            return Err(SignError::Unavailable(
                "the ID token isn't for this sign-in (its nonce differs)".into(),
            ));
        }
        Ok(token)
    }

    fn describe(&self) -> &'static str {
        "your sign-in"
    }
}

/// 32 random bytes, base64url: a PKCE verifier, a state or a nonce.
fn random_token() -> Result<String, SignError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| SignError::Unavailable(format!("no randomness: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// The longest request line the listener reads: a callback's is far shorter.
const MAX_LINE: usize = 8 * 1024;
/// How long one connection has to send its request line. A browser sends it at once; an idle or
/// trickling connection, such as a browser's spare one, mustn't hold up the sign-in.
const LINE_WAIT: Duration = Duration::from_secs(3);

/// The code the issuer sends the browser back with, to `/auth/callback?code=…&state=…`. Other
/// requests, such as a browser's favicon, one for another sign-in, or one that never finishes
/// its request line, are answered or dropped, and the wait goes on.
fn receive_code(listener: &TcpListener, state: &str, wait: Duration) -> Result<Secret, SignError> {
    listener
        .set_nonblocking(true)
        .map_err(|e| SignError::Unavailable(format!("listening for the sign-in: {e}")))?;
    let deadline = Instant::now() + wait;
    loop {
        // Checked first: connections that keep coming mustn't keep the wait going.
        if Instant::now() >= deadline {
            return Err(SignError::Unavailable(format!(
                "nobody signed in within {} s",
                wait.as_secs()
            )));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(result) = callback(stream, state) {
                    return result;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(SignError::Unavailable(format!(
                    "waiting for the sign-in: {e}"
                )));
            }
        }
    }
}

/// The request line `stream` sends within [`LINE_WAIT`] and [`MAX_LINE`] bytes, without its end.
fn request_line(stream: &mut TcpStream) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    let deadline = Instant::now() + LINE_WAIT;
    let mut line = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())?;
        stream.set_read_timeout(Some(left)).ok()?;
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        line.extend_from_slice(&chunk[..read]);
        if let Some(end) = line.iter().position(|b| *b == b'\n') {
            line.truncate(end);
            return String::from_utf8(line).ok();
        }
        if line.len() > MAX_LINE {
            return None;
        }
    }
}

/// Answers one request to the loopback listener; `None` when it wasn't this sign-in's callback.
fn callback(mut stream: TcpStream, state: &str) -> Option<Result<Secret, SignError>> {
    let line = request_line(&mut stream)?;
    let target = line.split(' ').nth(1).unwrap_or_default();
    let url = reqwest::Url::parse(&format!("http://localhost{target}")).ok()?;
    let reply = |stream: &mut TcpStream, status: &str, text: &str| {
        let page = format!(
            "<!doctype html><meta charset=utf-8><title>hushrun</title><p style=\"font:16px system-ui;margin:3rem\">{text}</p>"
        );
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
    };
    if url.path() != "/auth/callback" {
        reply(&mut stream, "404 Not Found", "Not found.");
        return None;
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    if param("state").as_deref() != Some(state) {
        // Anyone on this host can reach the listener: only the browser knows the state.
        reply(
            &mut stream,
            "400 Bad Request",
            "This sign-in isn't the one hushrun started.",
        );
        return None;
    }
    if let Some(error) = param("error") {
        reply(
            &mut stream,
            "400 Bad Request",
            "Signing in failed: return to your terminal.",
        );
        let detail = param("error_description").unwrap_or_default();
        return Some(Err(SignError::Unavailable(format!(
            "signing in failed: {error} {detail}"
        ))));
    }
    match param("code") {
        Some(code) => {
            reply(
                &mut stream,
                "200 OK",
                "Signed in. You can close this tab and return to your terminal.",
            );
            Some(Ok(Secret::new(code)))
        }
        None => {
            reply(&mut stream, "400 Bad Request", "The sign-in sent no code.");
            Some(Err(SignError::Unavailable(
                "the sign-in sent no code".into(),
            )))
        }
    }
}

/// The claims of a JWT, read without verifying it: Fulcio verifies the token, and hushrun only
/// needs to know what it proves possession for.
pub fn claims(token: &str) -> Result<serde_json::Value, SignError> {
    let bad = || SignError::Unavailable("the ID token is not a JWT".into());
    let payload = token.split('.').nth(1).ok_or_else(bad)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| bad())?;
    serde_json::from_slice(&bytes).map_err(|_| bad())
}

/// What Fulcio's proof of possession signs: the token's `email` when it has one, else its
/// `sub`, as Sigstore's clients do.
pub fn subject(token: &str) -> Result<String, SignError> {
    let claims = claims(token)?;
    claims
        .get("email")
        .and_then(|e| e.as_str())
        .filter(|e| !e.is_empty())
        .or_else(|| claims.get("sub").and_then(|s| s.as_str()))
        .map(str::to_string)
        .ok_or_else(|| SignError::Unavailable("the ID token has no `sub` claim".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{github_claims, jwt};
    use hushrun_testkit::http::{Reply, Response, Server};
    use std::sync::{Arc, Mutex};

    #[test]
    fn asks_github_for_a_token_for_the_audience() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let server = Server::start(move |request| {
            log.lock().unwrap().push(request.clone());
            Reply::Send(Response::json(
                200,
                &serde_json::json!({"value": "the.jwt.token"}),
            ))
        });
        let source = GithubActions::new(
            format!("{}/token?api-version=2.0", server.url()),
            Secret::new("request-token"),
        );
        let http = Http::new().unwrap();
        let token = source.id_token(&http, "sigstore").unwrap();
        assert_eq!(token.expose(), "the.jwt.token");
        let request = seen.lock().unwrap()[0].clone();
        assert_eq!(request.query("audience"), Some("sigstore"));
        assert_eq!(request.query("api-version"), Some("2.0"));
        assert_eq!(
            request.header("authorization"),
            Some("Bearer request-token")
        );
    }

    #[test]
    fn failures_say_what_the_job_needs_and_never_show_tokens() {
        let server = Server::start(|_| Reply::Send(Response::bytes(403, "denied")));
        let source = GithubActions::new(server.url(), Secret::new("request-token"));
        let err = source
            .id_token(&Http::new().unwrap(), "sigstore")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("id-token: write") && err.contains("403"),
            "{err}"
        );
        assert!(!err.contains("request-token"), "{err}");
        let insecure = GithubActions::new("http://example.com/token", Secret::new("t"));
        assert!(matches!(
            insecure.id_token(&Http::new().unwrap(), "sigstore"),
            Err(SignError::Config(_))
        ));
    }

    /// A browser for the sign-in: GETs `url`, follows one redirect, and returns the final
    /// answer's status line.
    fn browse(url: &str) -> String {
        use std::io::Read as _;
        let get = |url: &str| -> String {
            let parsed = reqwest::Url::parse(url).unwrap();
            let host = parsed.host_str().unwrap();
            let port = parsed.port().unwrap();
            let target = match parsed.query() {
                Some(query) => format!("{}?{query}", parsed.path()),
                None => parsed.path().to_string(),
            };
            let mut stream = TcpStream::connect((host, port)).unwrap();
            write!(
                stream,
                "GET {target} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        };
        let first = get(url);
        match first.lines().find_map(|l| {
            l.strip_prefix("location: ")
                .or_else(|| l.strip_prefix("Location: "))
        }) {
            Some(location) => get(location.trim())
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
            None => first.lines().next().unwrap_or_default().to_string(),
        }
    }

    fn interactive(issuer: String, show: impl Fn(&str) + Send + Sync + 'static) -> Interactive {
        Interactive {
            issuer,
            client_id: SIGSTORE_CLIENT.into(),
            show: Box::new(show),
            wait: Duration::from_secs(10),
        }
    }

    // covers: E-11
    #[test]
    fn people_sign_in_with_their_browser_and_pkce() {
        let sigstore = hushrun_testkit::sigstore::FakeSigstore::start();
        let seen = Arc::new(Mutex::new(String::new()));
        let status = seen.clone();
        let source = interactive(sigstore.issuer(), move |url| {
            let url = url.to_string();
            let status = status.clone();
            std::thread::spawn(move || *status.lock().unwrap() = browse(&url));
        });
        let token = source.id_token(&Http::new().unwrap(), "sigstore").unwrap();
        assert_eq!(subject(token.expose()).unwrap(), "alice@acme.dev");
        // The page the person lands on says they can go back to the terminal.
        for _ in 0..100 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            seen.lock().unwrap().contains("200"),
            "{}",
            seen.lock().unwrap()
        );
        // Tokens for another audience can't come from this client.
        assert!(matches!(
            source.id_token(&Http::new().unwrap(), "elsewhere"),
            Err(SignError::Config(_))
        ));
    }

    // covers: E-11
    #[test]
    fn what_isnt_this_sign_in_is_ignored() {
        let sigstore = hushrun_testkit::sigstore::FakeSigstore::start();
        // Before the browser comes back: a connection that sends nothing, one that never ends
        // its line, an endless line, and a callback for another sign-in. None holds it up for
        // long, and none ends it.
        let forged = Arc::new(Mutex::new(String::new()));
        let seen = forged.clone();
        let source = interactive(sigstore.issuer(), move |url| {
            let url = url.to_string();
            let seen = seen.clone();
            let parsed = reqwest::Url::parse(&url).unwrap();
            let redirect = parsed
                .query_pairs()
                .find(|(k, _)| k == "redirect_uri")
                .unwrap()
                .1
                .into_owned();
            let port = reqwest::Url::parse(&redirect).unwrap().port().unwrap();
            std::thread::spawn(move || {
                let _idle = TcpStream::connect(("127.0.0.1", port)).unwrap();
                let mut trickle = TcpStream::connect(("127.0.0.1", port)).unwrap();
                let _ = write!(trickle, "GET /auth/callback?state=");
                let mut endless = TcpStream::connect(("127.0.0.1", port)).unwrap();
                let _ = endless.write_all(&vec![b'a'; MAX_LINE + 2048]);
                *seen.lock().unwrap() = browse(&format!("{redirect}?code=x&state=forged"));
                browse(&url);
                drop((trickle, endless));
            });
        });
        let started = Instant::now();
        let token = source.id_token(&Http::new().unwrap(), "sigstore").unwrap();
        assert_eq!(subject(token.expose()).unwrap(), "alice@acme.dev");
        assert!(
            started.elapsed() < Duration::from_secs(9),
            "{:?}",
            started.elapsed()
        );
        assert!(
            forged.lock().unwrap().contains("400"),
            "{}",
            forged.lock().unwrap()
        );
    }

    // covers: E-11
    #[test]
    fn a_sign_in_that_failed_is_refused() {
        let sigstore = hushrun_testkit::sigstore::FakeSigstore::start();
        // The issuer says the person refused.
        let source = interactive(sigstore.issuer(), |url| {
            let parsed = reqwest::Url::parse(url).unwrap();
            let pair = |name: &str| {
                parsed
                    .query_pairs()
                    .find(|(k, _)| k == name)
                    .unwrap()
                    .1
                    .into_owned()
            };
            let (redirect, state) = (pair("redirect_uri"), pair("state"));
            std::thread::spawn(move || {
                browse(&format!("{redirect}?error=access_denied&state={state}"))
            });
        });
        let err = source
            .id_token(&Http::new().unwrap(), "sigstore")
            .unwrap_err()
            .to_string();
        assert!(err.contains("access_denied"), "{err}");
        // Nobody signs in.
        let mut source = interactive(sigstore.issuer(), |_| {});
        source.wait = Duration::from_millis(200);
        let err = source
            .id_token(&Http::new().unwrap(), "sigstore")
            .unwrap_err()
            .to_string();
        assert!(err.contains("nobody signed in"), "{err}");
    }

    #[test]
    fn gitlab_tokens_must_be_for_the_audience_and_unexpired() {
        let now = || -> Box<dyn Fn() -> Time + Send + Sync> {
            Box::new(|| Time::parse("2026-10-16T12:00:00Z").unwrap())
        };
        let source = |claims: serde_json::Value| {
            GitlabIdToken::new("SIGSTORE_ID_TOKEN", Secret::new(jwt(&claims)), now())
        };
        let http = Http::new().unwrap();
        let exp = Time::parse("2026-10-16T13:00:00Z").unwrap().millis() / 1000;
        let good = source(serde_json::json!({"aud": "sigstore", "sub": "s", "exp": exp}));
        assert!(good.id_token(&http, "sigstore").is_ok());
        assert_eq!(good.describe(), "GitLab CI's id_tokens");
        // GitLab's `aud` may be a list.
        let listed = source(serde_json::json!({"aud": ["https://vault", "sigstore"], "exp": exp}));
        assert!(listed.id_token(&http, "sigstore").is_ok());
        // Another audience names the entry to declare, and is a configuration error.
        let other = source(serde_json::json!({"aud": "https://gitlab.com", "exp": exp}));
        let err = other.id_token(&http, "sigstore").unwrap_err();
        assert!(matches!(err, SignError::Config(_)), "{err}");
        assert!(
            err.to_string()
                .contains("`SIGSTORE_ID_TOKEN: {aud: sigstore}`"),
            "{err}"
        );
        assert!(
            err.to_string().contains("audience https://gitlab.com"),
            "{err}"
        );
        let none = source(serde_json::json!({"exp": exp}));
        assert!(
            none.id_token(&http, "sigstore")
                .unwrap_err()
                .to_string()
                .contains("no audience")
        );
        // An expired token is the signer being unavailable (exit 9).
        let late = source(serde_json::json!({"aud": "sigstore", "exp": exp - 7200}));
        let err = late.id_token(&http, "sigstore").unwrap_err();
        assert!(matches!(err, SignError::Unavailable(_)), "{err}");
        assert!(err.to_string().contains("expired 3600 s ago"), "{err}");
        let garbage = GitlabIdToken::new("SIGSTORE_ID_TOKEN", Secret::new("x"), now());
        assert!(matches!(
            garbage.id_token(&http, "sigstore"),
            Err(SignError::Config(_))
        ));
    }

    #[test]
    fn gitlab_claims_map_as_fulcio_maps_them() {
        let claims = serde_json::json!({
            "iss": "https://gitlab.com",
            "project_id": "4242",
            "project_path": "acme/back",
            "ref": "develop",
            "ref_type": "branch",
            "ci_config_ref_uri": "gitlab.com/acme/back//.gitlab-ci.yml@refs/heads/develop",
        });
        let identity = gitlab_identity(&claims).unwrap();
        let config = "https://gitlab.com/acme/back//.gitlab-ci.yml@refs/heads/develop";
        assert_eq!(identity.issuer, "https://gitlab.com");
        assert_eq!(identity.subject, config);
        assert_eq!(identity.repository_id.as_deref(), Some("4242"));
        assert_eq!(
            identity.repository.as_deref(),
            Some("https://gitlab.com/acme/back")
        );
        assert_eq!(identity.reference.as_deref(), Some("refs/heads/develop"));
        assert_eq!(identity.workflow.as_deref(), Some(config));
        assert_eq!(identity.signer_workflow.as_deref(), Some(config));
        let mut tag = claims.clone();
        tag["ref"] = "v1.2.0".into();
        tag["ref_type"] = "tag".into();
        assert_eq!(
            gitlab_identity(&tag).unwrap().reference.as_deref(),
            Some("refs/tags/v1.2.0")
        );
        let mut elsewhere = claims;
        elsewhere["ci_config_ref_uri"] = serde_json::Value::Null;
        assert!(
            gitlab_identity(&elsewhere)
                .unwrap_err()
                .to_string()
                .contains("ci_config_ref_uri")
        );
    }

    #[test]
    fn the_proof_signs_the_email_or_the_subject() {
        let ci = jwt(&github_claims(
            "acme/app",
            "1",
            "refs/heads/main",
            "w.yml",
            None,
        ));
        assert_eq!(subject(&ci).unwrap(), "repo:acme/app:ref:refs/heads/main");
        let person = jwt(&serde_json::json!({"sub": "123", "email": "alice@acme.dev"}));
        assert_eq!(subject(&person).unwrap(), "alice@acme.dev");
        assert!(subject("not-a-jwt").is_err());
        assert_eq!(
            Given(Secret::new("t"))
                .id_token(&Http::new().unwrap(), "x")
                .unwrap(),
            Secret::new("t")
        );
    }
}
