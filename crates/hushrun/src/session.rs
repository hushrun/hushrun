//! \[ADR 0105\] Session tokens (C-52): a short credential for one repository that a sandbox's agent
//! gets for a longer one, at `POST {base}/v0/sessions`, and keeps in `sessions.json` beside the
//! credentials file, by origin. `hushrun login --session` makes one with what the environment
//! holds: a bearer agent token (`HUSHRUN_TOKEN`), a Cursor cloud agent's ID token
//! (minted over `CURSOR_AGENT_SOCKET`), or a GitHub Actions job's ID token (Copilot's setup steps).
//! Runs read the file, and in a Cursor cloud agent's sandbox get a session by themselves.
//!
//! A credential goes only where it may (C-13): to hushrun's host, or to a backend the environment
//! or the command line names, never to one a repository's file chose. `HUSHRUN_AGENT` is never
//! swapped: it holds a key, and signs as it is.
//!
//! The file is written whole (a temporary file and a rename, readable by its owner only) while a
//! lock is held, so two sessions made at once keep each other's origins. A session past its expiry
//! by this machine's clock is not read; the server judges it by its own clock anyway. No session
//! token, and no ID token, is ever printed.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use hushrun_core::ledger_url::LedgerUrl;
use hushrun_ledger::{HttpLedger, OidcEndpoint, Options, Secret};

use crate::credentials::{self, Credentials};

/// What a session token starts with.
pub const PREFIX: &str = "hrs_";

/// A session is read until this long before its expiry, so a run doesn't start with one that
/// ends on its way.
const MARGIN_MS: i64 = 60_000;

/// How long a request to swap a credential may take.
const BUDGET: Duration = Duration::from_secs(15);

/// Whether `token` is a session token.
pub fn is_session_token(token: &str) -> bool {
    token.starts_with(PREFIX)
}

/// One session, as the file keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The org's URL it was made at.
    pub backend: String,
    pub token: String,
    pub repository: String,
    /// Its results record as `agent:<agent>`.
    pub agent: String,
    /// Milliseconds since the epoch.
    pub expires_at: i64,
    /// What it was swapped for: `agent token`, `Cursor` or `GitHub Actions`.
    pub source: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    /// By origin, `https://host`.
    #[serde(default)]
    sessions: BTreeMap<String, Session>,
}

/// `sessions.json` beside the credentials file.
pub fn path() -> Option<PathBuf> {
    Credentials::path().map(|path| path.with_file_name("sessions.json"))
}

/// The session for `origin` that is good at `now_ms`, when there is one.
pub fn load_for(origin: &str, now_ms: i64) -> Option<Session> {
    let path = path()?;
    load_from(&path)
        .ok()?
        .sessions
        .remove(origin)
        .filter(|session| session.expires_at - MARGIN_MS > now_ms)
}

fn load_from(path: &Path) -> anyhow::Result<File> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(File::default()),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

/// Saves `session` for its backend's origin, with the lock held; where the file is.
pub fn save(session: &Session) -> anyhow::Result<PathBuf> {
    let path = path().ok_or_else(|| {
        anyhow!("there is no configuration directory to keep a session in: set HUSHRUN_CREDENTIALS")
    })?;
    let url = LedgerUrl::parse(&session.backend).map_err(|e| anyhow!("{e}"))?;
    let origin = credentials::origin(&url);
    save_at(&path, &origin, session)?;
    Ok(path)
}

fn save_at(path: &Path, origin: &str, session: &Session) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("sessions.json.lock"))
        .with_context(|| format!("locking {}", path.display()))?;
    if let Err(err) = lock.lock() {
        tracing::debug!("can't lock {}: {err}", path.display());
    }
    let mut file = load_from(path).unwrap_or_default();
    file.sessions.insert(origin.to_string(), session.clone());
    let temp = dir.join(format!(".sessions-{}.tmp", std::process::id()));
    let written = (|| -> io::Result<()> {
        let mut out = credentials::create_private(&temp)?;
        out.write_all(&serde_json::to_vec_pretty(&file)?)?;
        out.write_all(b"\n")?;
        out.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(err).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

/// What a session is swapped for.
enum Parent {
    AgentToken(String),
    Cursor(String),
    Github(OidcEndpoint),
}

impl Parent {
    fn source(&self) -> &'static str {
        match self {
            Parent::AgentToken(_) => "agent token",
            Parent::Cursor(_) => "Cursor",
            Parent::Github(_) => "GitHub Actions",
        }
    }
}

/// The credential this environment can swap for a session at `base`, in order: an agent token,
/// a Cursor cloud agent's ID token, a GitHub Actions job's ID token.
fn parent(env: &hushrun_ci::Env, base: &LedgerUrl) -> anyhow::Result<Parent> {
    if let Some(token) = env
        .get("HUSHRUN_TOKEN")
        .map(str::trim)
        .filter(|token| token.starts_with(crate::results::AGENT_TOKEN_PREFIX))
    {
        return Ok(Parent::AgentToken(token.to_string()));
    }
    if let Some(socket) = env.get(CURSOR_SOCKET) {
        let audience = credentials::origin(base);
        return cursor_token(socket, &audience).map(Parent::Cursor);
    }
    let endpoint = env.sandbox_github_id_token().map_or_else(
        || {
            let url = env.get("ACTIONS_ID_TOKEN_REQUEST_URL")?;
            let token = env.get("ACTIONS_ID_TOKEN_REQUEST_TOKEN")?;
            Some((url.to_string(), token.to_string()))
        },
        |(url, token)| Some((url.to_string(), token.to_string())),
    );
    if let Some((url, token)) = endpoint {
        return Ok(Parent::Github(OidcEndpoint {
            url,
            token: Secret::new(token),
        }));
    }
    Err(anyhow!(
        "nothing here to swap for a session: set HUSHRUN_TOKEN to an agent token, or run this in a Cursor cloud agent (CURSOR_AGENT_SOCKET) or a GitHub Actions job with `permissions: id-token: write`"
    ))
}

/// Swaps this environment's credential for a session at `base`, of `repository` (for an org-wide
/// agent token), lasting `hours`.
pub fn obtain(
    env: &hushrun_ci::Env,
    base: &LedgerUrl,
    repository: Option<&str>,
    hours: Option<u32>,
) -> anyhow::Result<Session> {
    if env.agent().is_some() {
        return Err(anyhow!(
            "HUSHRUN_AGENT holds the agent's key, and records as it is: a session is for a sandbox with a bearer token, Cursor's or GitHub's ID token"
        ));
    }
    let parent = parent(env, base)?;
    let source = parent.source();
    let options = match parent {
        Parent::AgentToken(token) | Parent::Cursor(token) => Options {
            budget: Some(BUDGET),
            token: Some(Secret::new(token)),
            token_command: None,
            github_oidc: None,
        },
        Parent::Github(endpoint) => Options {
            budget: Some(BUDGET),
            token: None,
            token_command: None,
            github_oidc: Some(endpoint),
        },
    };
    let ledger = HttpLedger::open(base, &options).map_err(crate::results::plain)?;
    let mut body = json!({});
    if let Some(repository) = repository {
        body["repository"] = json!(repository);
    }
    if let Some(hours) = hours {
        body["hours"] = json!(hours);
    }
    let bytes = serde_json::to_vec(&body)?;
    let (status, answer) = ledger
        .json_request("POST", "v0/sessions", Some(&bytes))
        .map_err(crate::results::plain)?;
    if status != 201 {
        return Err(crate::results::refusal(
            &base.to_string(),
            status,
            &answer,
            "a session",
        ));
    }
    let answer: Value = serde_json::from_slice(&answer).context("reading the session")?;
    let text = |name: &str| {
        answer[name]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("{base} answered a session without `{name}`"))
    };
    let token = text("token")?;
    if !is_session_token(&token) {
        return Err(anyhow!("{base} answered a session token that isn't one"));
    }
    let expires = text("expires_at")?;
    let expires_at = hushrun_core::time::Time::parse(&expires)
        .map_err(|_| anyhow!("{base} answered a session whose expires_at doesn't read"))?
        .millis();
    Ok(Session {
        backend: base.to_string(),
        token,
        repository: text("repository")?,
        agent: text("agent")?,
        expires_at,
        source: source.to_string(),
    })
}

/// \[ADR 0105\] In a Cursor cloud agent's sandbox, with no session for `base` yet, gets one and
/// saves it: Cursor's ID token buys it with no secret in the sandbox. `None` when it can't, which
/// only debug logs say: the run goes on as it would have.
pub fn for_cursor(env: &hushrun_ci::Env, base: &LedgerUrl, now_ms: i64) -> Option<Session> {
    if hushrun_ci::sandbox(env).map(|p| p.id) != Some("cursor-cloud") {
        return None;
    }
    let origin = credentials::origin(base);
    if let Some(session) = load_for(&origin, now_ms) {
        return Some(session);
    }
    match obtain(env, base, None, None) {
        Ok(session) => {
            if let Err(err) = save(&session) {
                tracing::debug!("keeping the session: {err:#}");
            }
            Some(session)
        }
        Err(err) => {
            tracing::debug!("a session for Cursor's cloud agent: {err:#}");
            None
        }
    }
}

// --- Cursor's socket ----------------------------------------------------------------------------

/// The socket Cursor gives a cloud agent, which mints its ID tokens.
pub const CURSOR_SOCKET: &str = "CURSOR_AGENT_SOCKET";

/// A Cursor ID token for `audience`, from the cache while it is good for another minute, else
/// minted over `socket` with `POST /v1/tokens/oidc` and cached until it expires.
pub fn cursor_token(socket: &str, audience: &str) -> anyhow::Result<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let cache = crate::context::cache_dir().map(|dir| dir.join("cursor-oidc.json"));
    if let Some(cached) = cache
        .as_deref()
        .and_then(|path| cached_token(path, audience, now))
    {
        return Ok(cached);
    }
    let token = mint(socket, audience)?;
    if let (Some(path), Some(exp)) = (cache.as_deref(), expiry(&token)) {
        let entry = json!({"audience": audience, "token": token, "exp": exp});
        if let Err(err) = write_private(path, &serde_json::to_vec(&entry)?) {
            tracing::debug!("caching Cursor's ID token: {err:#}");
        }
    }
    Ok(token)
}

fn cached_token(path: &Path, audience: &str, now: i64) -> Option<String> {
    let entry: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (entry["audience"].as_str() == Some(audience) && entry["exp"].as_i64()? > now + 60)
        .then(|| entry["token"].as_str().map(str::to_string))
        .flatten()
}

/// A JWT's `exp`, read without verifying it: only the cache's lifetime.
fn expiry(token: &str) -> Option<i64> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()?["exp"].as_i64()
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".cursor-oidc-{}.tmp", std::process::id()));
    let written = (|| -> io::Result<()> {
        let mut out = credentials::create_private(&temp)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    Ok(written?)
}

/// `POST /v1/tokens/oidc` `{"audience"}` over the unix socket at `socket`, HTTP/1.1 by hand: the
/// token in the answer's `token`, `id_token`, `idToken` or `value`.
#[cfg(unix)]
fn mint(socket: &str, audience: &str) -> anyhow::Result<String> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to Cursor's socket ({CURSOR_SOCKET})"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let body = serde_json::to_vec(&json!({"audience": audience}))?;
    let head = format!(
        "POST /v1/tokens/oidc HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // One write: the request whole.
    let mut request = head.into_bytes();
    request.extend_from_slice(&body);
    stream.write_all(&request)?;
    let mut answer = Vec::new();
    stream
        .take(1 << 20)
        .read_to_end(&mut answer)
        .context("reading Cursor's socket")?;
    let text = String::from_utf8_lossy(&answer);
    let (head, rest) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("Cursor's socket answered no HTTP response"))?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| anyhow!("Cursor's socket answered no status"))?;
    let chunked = head
        .lines()
        .any(|line| line.to_ascii_lowercase().replace(' ', "") == "transfer-encoding:chunked");
    let body = if chunked {
        dechunk(rest)
    } else {
        rest.to_string()
    };
    if status != 200 && status != 201 {
        return Err(anyhow!("Cursor's socket answered {status} for an ID token"));
    }
    let value: Value =
        serde_json::from_str(body.trim()).context("reading Cursor's socket's answer")?;
    ["token", "id_token", "idToken", "value"]
        .iter()
        .find_map(|name| value[*name].as_str())
        .filter(|token| token.split('.').count() == 3)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Cursor's socket answered no ID token"))
}

#[cfg(not(unix))]
fn mint(_socket: &str, _audience: &str) -> anyhow::Result<String> {
    Err(anyhow!("Cursor's socket is a unix socket"))
}

fn dechunk(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || after.len() < size {
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].trim_start_matches("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(backend: &str, token: &str, expires_at: i64) -> Session {
        Session {
            backend: backend.to_string(),
            token: token.to_string(),
            repository: "github.com/acme/shop".into(),
            agent: "cursor".into(),
            expires_at,
            source: "Cursor".into(),
        }
    }

    // covers: C-52
    #[test]
    fn sessions_saved_at_the_same_moment_keep_each_others_origins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/sessions.json");
        let threads: Vec<_> = (0..12)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let origin = format!("https://host-{n}.example");
                    save_at(
                        &path,
                        &origin,
                        &session(&format!("{origin}/o/acme"), "hrs_x", 1),
                    )
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        assert_eq!(load_from(&path).unwrap().sessions.len(), 12);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Nothing left over.
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(left.is_empty());
    }

    // covers: C-52
    #[test]
    fn an_answer_from_cursors_socket_reads_whole_or_chunked() {
        assert_eq!(
            dechunk("5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"),
            "hello world"
        );
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjQxMDI0NDQ4MDB9.sig";
        assert_eq!(expiry(jwt), Some(4_102_444_800));
        assert_eq!(expiry("nope"), None);
    }

    // covers: C-52
    #[cfg(unix)]
    #[test]
    fn cursors_socket_mints_a_token_for_the_audience() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("cursor.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0u8; 4096];
            let n = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..n]).into_owned();
            let body = r#"{"token":"aaa.bbb.ccc"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            request
        });
        let token = mint(socket.to_str().unwrap(), "https://api.hushrun.com").unwrap();
        assert_eq!(token, "aaa.bbb.ccc");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/tokens/oidc HTTP/1.1"),
            "{request}"
        );
        assert!(
            request.ends_with(r#"{"audience":"https://api.hushrun.com"}"#),
            "{request}"
        );
    }
}
