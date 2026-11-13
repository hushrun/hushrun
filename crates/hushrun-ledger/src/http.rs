//! Ledgers served over HTTP (L-10) \[M18\]: `https://host/path`, or `http://` on a loopback host
//! for local development. The hosted ledger is one. The protocol is `hushrun-ledger-http/1`.
//!
//! Every request carries a bearer token from the environment ([`auth`]) and goes to the
//! ledger's own origin only: a redirect to another host is never followed, so a token never
//! reaches one. Retries, timeouts and the budget follow L-4.
//! Storage carries no trust here either (E-8): readers verify what this backend returns.

mod auth;
mod log;
mod token;

use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use futures_util::stream::{self, StreamExt};
use reqwest::header::{self, HeaderMap};
use reqwest::{Method, StatusCode};
use serde::Deserialize;

use hushrun_core::digest::Digest;
use hushrun_core::ledger_url::LedgerUrl;

use crate::runtime::{Io, READ_CONCURRENCY, backoff_from, excerpt, form, install_crypto_provider};
use crate::{Ledger, LedgerError, Log, Options, QUOTA_EXCEEDED, Quota, check_key};

/// The protocol this client speaks (L-10).
pub const PROTOCOL: &str = "hushrun-ledger-http/1";

/// Object and configuration bodies (L-10, E-8 step 1).
const MAX_OBJECT_BYTES: usize = 1 << 20;
/// Keys per batch-get request.
const BATCH_KEYS: usize = 100;
/// Keys per listing page.
const PAGE_KEYS: usize = 1000;
/// The longest key.
const MAX_KEY_BYTES: usize = 1024;
/// A listing page: 1000 keys of at most 1 KiB, as JSON.
const MAX_PAGE_BYTES: usize = 4 << 20;
/// A batch-get answer: 100 objects of at most 1 MiB, in base64.
const MAX_BATCH_BYTES: usize = 150 << 20;
/// Anything else: errors, `GET {base}`, create receipts.
const MAX_SMALL_BYTES: usize = 64 << 10;
/// Same-origin redirects followed before giving up.
const MAX_REDIRECTS: usize = 5;
/// The most keys one listing may take: a server that never stops sending `next` must not
/// keep a command forever (see also `Tuning::max_list_pages`).
const MAX_LIST_KEYS: usize = 10_000_000;

const OCTET_STREAM: &str = "application/octet-stream";
const JSON: &str = "application/json";

/// How requests time out and retry (L-4). Tests shorten it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tuning {
    pub(crate) request_timeout: Duration,
    pub(crate) connect_timeout: Duration,
    /// Attempts per request: the first, and up to three retries.
    pub(crate) attempts: u32,
    /// The first retry's backoff; each later one doubles it.
    pub(crate) backoff: Duration,
    /// The longest `Retry-After` honored by a command without a budget.
    pub(crate) max_retry_after: Duration,
    /// The most pages one listing may take: 20 million keys in full pages.
    pub(crate) max_list_pages: usize,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            request_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            attempts: 4,
            backoff: Duration::from_millis(200),
            max_retry_after: Duration::from_secs(60),
            max_list_pages: 20_000,
        }
    }
}

/// A ledger at an `https://` (or loopback `http://`) URL.
pub struct HttpLedger {
    /// The normalized URL (T-4), without a trailing `/`; also the OIDC audience.
    base: String,
    client: reqwest::Client,
    auth: auth::Auth,
    io: Io,
    tuning: Tuning,
    /// \[M18\] The transparency log, as this command checks it (L-11).
    log: Mutex<log::State>,
    /// An answer to one of hushrun's other requests carried `Hushrun-Build: on`: the org shares
    /// build blocks (build-v0 §1).
    build_sharing: std::sync::atomic::AtomicBool,
}

impl fmt::Debug for HttpLedger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpLedger")
            .field("base", &self.base)
            .field("auth", &self.auth.source())
            .field("io", &self.io)
            .finish_non_exhaustive()
    }
}

/// What `GET {base}` says about a ledger (L-10): `doctor` checks the protocol and the
/// repository.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LedgerInfo {
    pub protocol: String,
    #[serde(default)]
    pub ledger: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// \[M18\] The ledger's transparency log, when it keeps one (L-11). A hint only: readers
    /// trust the log key hushrun.yaml names (T-9).
    #[serde(default)]
    pub log: Option<LogInfo>,
}

/// \[M18\] What `GET {base}` says about the ledger's transparency log.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LogInfo {
    /// The origin its checkpoints name.
    pub origin: String,
    /// The public half of its log key, `ed25519:<base64>`, for `transparency_log` (T-9).
    #[serde(default)]
    pub key: Option<String>,
    /// How many leaves its latest checkpoint has.
    #[serde(default)]
    pub size: Option<u64>,
}

/// What a write's answer must be to count as done (L-10): anything else, such as a proxy's
/// page, proves nothing was stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Done {
    /// `201 Created`, with the stored bytes' `sha256`: object creates.
    Created,
    /// `204 No Content`: configuration writes, root creates and deletes.
    NoContent,
}

/// One request, as the retry loop sends it.
struct Call<'a> {
    method: Method,
    /// Relative to the base: `objects/<key>`, `objects`, `batch-get`, or empty.
    path: &'a str,
    /// An encoded query string, without the `?`.
    query: Option<String>,
    body: Option<(&'a [u8], &'static str)>,
    /// Send `If-None-Match: *`: object creates and root creates (L-10).
    create_only: bool,
    /// One more header, such as a delete's GC statement (L-9).
    header: Option<(&'static str, String)>,
    /// The most response bytes read; the rest is cut off.
    limit: usize,
    /// What the request is about, for messages: a key, a name, a listing.
    what: &'a str,
}

impl<'a> Call<'a> {
    fn new(method: Method, path: &'a str, what: &'a str, limit: usize) -> Self {
        Call {
            method,
            path,
            query: None,
            body: None,
            create_only: false,
            header: None,
            limit,
            what,
        }
    }
}

/// A response, read.
struct Answer {
    status: StatusCode,
    body: Vec<u8>,
    retry_after: Option<Duration>,
    /// The origin a redirect pointed to (never its path or query, which may carry secrets).
    redirect: Option<String>,
    date: Option<String>,
    /// Whether the request carried a token, for the 401 message.
    authorized: bool,
    /// Why the retries stopped early, such as a `Retry-After` beyond the budget.
    gave_up: Option<String>,
    /// It carried `Hushrun-Build: on`.
    build_sharing: bool,
}

#[derive(Deserialize)]
struct Page {
    keys: Vec<String>,
    #[serde(default)]
    next: Option<String>,
    /// \[M18\] The checkpoint the page was read at, and the ledger's signed statement (L-11).
    #[serde(default)]
    checkpoint: Option<String>,
    #[serde(default)]
    statement: Option<String>,
}

/// \[M18\] The receipt in a create's answer, `201` or a retried create's `412` (L-11).
#[derive(Deserialize)]
struct Written {
    #[serde(default)]
    receipt: Option<log::Receipt>,
}

#[derive(Deserialize)]
struct Batch {
    objects: Vec<BatchObject>,
}

#[derive(Deserialize)]
struct BatchObject {
    key: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl HttpLedger {
    /// Opens the ledger at `url`, with the token sources and budget of `options`.
    pub fn open(url: &LedgerUrl, options: &Options) -> Result<HttpLedger, LedgerError> {
        HttpLedger::open_tuned(url, options, Tuning::default())
    }

    pub(crate) fn open_tuned(
        url: &LedgerUrl,
        options: &Options,
        tuning: Tuning,
    ) -> Result<HttpLedger, LedgerError> {
        let base = url.to_string();
        Ok(HttpLedger {
            client: client(&tuning)?,
            auth: auth::Auth::new(options, &base),
            io: Io::new(base.clone(), options.budget)?,
            base,
            tuning,
            log: Mutex::new(log::State::default()),
            build_sharing: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// `GET {base}`: the protocol, the ledger's URL and its repository.
    pub fn info(&self) -> Result<LedgerInfo, LedgerError> {
        self.io.run(async {
            let answer = self
                .call(&Call::new(
                    Method::GET,
                    "",
                    "the repository",
                    MAX_SMALL_BYTES,
                ))
                .await?;
            if !answer.status.is_success() {
                return Err(self.whole_ledger_failure(&answer, "the repository"));
            }
            serde_json::from_slice(&answer.body).map_err(|e| {
                LedgerError::Unavailable(format!(
                    "{}: GET {} didn't answer the repository's description: {e}",
                    self.base, self.base
                ))
            })
        })
    }

    /// The `Date` header of `GET {base}`, which the server sends with any status.
    pub(crate) fn date(&self) -> Result<Option<String>, LedgerError> {
        self.io.run(async {
            let answer = self
                .call(&Call::new(
                    Method::GET,
                    "",
                    "the repository",
                    MAX_SMALL_BYTES,
                ))
                .await?;
            Ok(answer.date)
        })
    }

    /// A JSON request to one of hushrun's other endpoints on this host, at `path` under the
    /// base: the results protocol and `hushrun login`, which `hushrun run` without `--stage`
    /// uses. It goes like every ledger request: to the base's origin only, with the token,
    /// retried on 429 and 5xx (L-4). The status and the body are the caller's to read.
    pub fn json_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<(u16, Vec<u8>), LedgerError> {
        self.json_request_with(method, path, body, None)
    }

    /// A GET of `path` with a query, already encoded, as [`HttpLedger::json_request`] sends one.
    pub fn json_get_query(&self, path: &str, query: &str) -> Result<(u16, Vec<u8>), LedgerError> {
        self.json_call(Method::GET, path, Some(query.to_string()), None, None)
    }

    /// [`HttpLedger::json_request`] with one more header, such as a result's machine signature
    /// (ADR 0028).
    pub fn json_request_with(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        header: Option<(&'static str, String)>,
    ) -> Result<(u16, Vec<u8>), LedgerError> {
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|_| LedgerError::Invalid(format!("{method} isn't an HTTP method")))?;
        self.json_call(method, path, None, body, header)
    }

    /// [`HttpLedger::json_request_with`] for an answer of up to a page (4 MiB), such as a batch
    /// lookup of a check's test files (results-v0 §3).
    pub fn json_request_page(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        header: Option<(&'static str, String)>,
    ) -> Result<(u16, Vec<u8>), LedgerError> {
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|_| LedgerError::Invalid(format!("{method} isn't an HTTP method")))?;
        self.json_call_sized(method, path, None, body, header, MAX_PAGE_BYTES)
    }

    /// [`HttpLedger::json_request`] for an answer of up to `max` bytes, past which it is cut off:
    /// a lookup of hints, which holds their bytes (results-v0, Hints).
    pub fn json_request_sized(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        max: usize,
    ) -> Result<(u16, Vec<u8>), LedgerError> {
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|_| LedgerError::Invalid(format!("{method} isn't an HTTP method")))?;
        self.json_call_sized(method, path, None, body, None, max)
    }

    /// `PUT` of `bytes` as `application/octet-stream` at `path` with `query`, already encoded,
    /// as [`HttpLedger::json_request`] sends a request: a hint (results-v0, Hints). The status
    /// and the JSON answer are the caller's to read.
    pub fn put_bytes(
        &self,
        path: &str,
        query: &str,
        bytes: &[u8],
    ) -> Result<(u16, Vec<u8>), LedgerError> {
        self.check_path(path)?;
        self.io.run(async {
            let mut call = Call::new(Method::PUT, path, path, MAX_SMALL_BYTES);
            call.body = Some((bytes, OCTET_STREAM));
            call.query = Some(query.to_string());
            let answer = self.call(&call).await?;
            if answer.status.is_redirection() {
                return Err(self.failure(&answer, path));
            }
            Ok((answer.status.as_u16(), answer.body))
        })
    }

    /// Whether an answer to one of hushrun's other requests (a results lookup) carried
    /// `Hushrun-Build: on`: the org shares build blocks, so asking `open` is worth a request.
    pub fn build_sharing_seen(&self) -> bool {
        self.build_sharing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether `path` is a path under the base, which a request to another endpoint may name.
    fn check_path(&self, path: &str) -> Result<(), LedgerError> {
        if path.contains(['?', '#', '\\'])
            || path
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return Err(LedgerError::Invalid(format!(
                "{path:?} isn't a path under {}",
                self.base
            )));
        }
        Ok(())
    }

    fn json_call(
        &self,
        method: Method,
        path: &str,
        query: Option<String>,
        body: Option<&[u8]>,
        header: Option<(&'static str, String)>,
    ) -> Result<(u16, Vec<u8>), LedgerError> {
        self.json_call_sized(method, path, query, body, header, MAX_SMALL_BYTES)
    }

    fn json_call_sized(
        &self,
        method: Method,
        path: &str,
        query: Option<String>,
        body: Option<&[u8]>,
        header: Option<(&'static str, String)>,
        max: usize,
    ) -> Result<(u16, Vec<u8>), LedgerError> {
        self.check_path(path)?;
        self.io.run(async {
            let mut call = Call::new(method, path, path, max);
            if let Some(body) = body {
                call.body = Some((body, JSON));
            }
            call.query = query;
            call.header = header;
            let answer = self.call(&call).await?;
            if answer.status.is_redirection() {
                return Err(self.failure(&answer, path));
            }
            if answer.build_sharing {
                self.build_sharing
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Ok((answer.status.as_u16(), answer.body))
        })
    }

    fn url(&self, call: &Call<'_>) -> String {
        let mut url = if call.path.is_empty() {
            self.base.clone()
        } else {
            format!("{}/{}", self.base, call.path)
        };
        if let Some(query) = &call.query {
            url.push('?');
            url.push_str(query);
        }
        url
    }

    /// Sends `call`, retrying timeouts, dropped connections, 429 and 5xx (L-4), and reads the
    /// answer. Any other status is the caller's to map.
    async fn call(&self, call: &Call<'_>) -> Result<Answer, LedgerError> {
        let url = self.url(call);
        let mut attempt = 1;
        loop {
            let token = self.auth.token(&self.client, &self.tuning).await?;
            let mut request = self.client.request(call.method.clone(), &url);
            if let Some((body, content_type)) = call.body {
                request = request
                    .header(header::CONTENT_TYPE, content_type)
                    .body(body.to_vec());
            }
            if call.create_only {
                request = request.header(header::IF_NONE_MATCH, "*");
            }
            if let Some((name, value)) = &call.header {
                request = request.header(*name, value.as_str());
            }
            if let Some(token) = &token {
                // reqwest marks the header sensitive, so it never shows in Debug output.
                request = request.bearer_auth(token.expose());
            }
            tracing::debug!("{} {url}", call.method);
            let wait = match read(request, call.limit).await {
                Ok(mut answer) => {
                    answer.authorized = token.is_some();
                    let retryable = answer.status == StatusCode::TOO_MANY_REQUESTS
                        || answer.status.is_server_error();
                    if !retryable || attempt >= self.tuning.attempts {
                        return Ok(answer);
                    }
                    let asked = answer.retry_after.filter(|_| {
                        matches!(
                            answer.status,
                            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
                        )
                    });
                    let wait = asked.unwrap_or_else(|| backoff_from(self.tuning.backoff, attempt));
                    let ceiling = self.io.remaining().unwrap_or(self.tuning.max_retry_after);
                    if wait > ceiling {
                        answer.gave_up = Some(format!(
                            "; it asked to retry after {} s, more than this command can wait",
                            wait.as_secs()
                        ));
                        return Ok(answer);
                    }
                    wait
                }
                Err(err) if attempt < self.tuning.attempts && is_transient(&err) => {
                    tracing::debug!("{} {}: {}", call.method, call.what, chain(err));
                    backoff_from(self.tuning.backoff, attempt)
                }
                Err(err) => {
                    return Err(LedgerError::Unavailable(format!(
                        "{}: {}: {}",
                        self.base,
                        call.what,
                        chain(err)
                    )));
                }
            };
            tracing::debug!(
                "{}: retrying {} {} in {} ms",
                self.base,
                call.method,
                call.what,
                wait.as_millis()
            );
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    /// The ledger error for an answer the operation didn't expect (L-10's table).
    fn failure(&self, answer: &Answer, what: &str) -> LedgerError {
        let status = answer.status.as_u16();
        // [ADR 0032] A write the org's plan doesn't allow. The retry loop never retries a 402,
        // and the command decides what goes on without it (L-5).
        if let Some(quota) = Quota::from_answer(status, &answer.body) {
            tracing::debug!("{}: {what}: 402 {QUOTA_EXCEEDED}", self.base);
            return LedgerError::Quota(Box::new(quota));
        }
        let detail = match error_message(&answer.body) {
            Some(message) => format!("{status} {message}"),
            None => status.to_string(),
        };
        let gave_up = answer.gave_up.as_deref().unwrap_or("");
        let base = &self.base;
        match status {
            300..=399 => LedgerError::Unavailable(format!(
                "{base}: {what}: {status} redirect to {}, which hushrun doesn't follow: tokens go only to the server's own host",
                answer.redirect.as_deref().unwrap_or("another location")
            )),
            401 if answer.authorized => LedgerError::Denied(format!(
                "{base}: {what}: {detail}: hushrun's server refused the token from {}",
                self.auth.source()
            )),
            401 => LedgerError::Denied(format!(
                "{base}: {what}: {detail}: no token; set HUSHRUN_REPOSITORY_TOKEN or HUSHRUN_REPOSITORY_TOKEN_COMMAND, or in GitHub Actions give the job `permissions: id-token: write`"
            )),
            403 => LedgerError::Denied(format!("{base}: {what}: {detail}")),
            404 => LedgerError::NotFound(what.to_string()),
            412 => LedgerError::AlreadyExists(what.to_string()),
            429 | 500..=599 => {
                LedgerError::Unavailable(format!("{base}: {what}: {detail}{gave_up}"))
            }
            400..=499 => LedgerError::Invalid(format!("{base}: {what}: {detail}")),
            _ => LedgerError::Unavailable(format!("{base}: {what}: unexpected {detail}")),
        }
    }

    /// The ledger error for an endpoint that always exists: a 404 there means that no ledger
    /// answers at this URL, which must not look like a missing object (L-5).
    fn whole_ledger_failure(&self, answer: &Answer, what: &str) -> LedgerError {
        if answer.status == StatusCode::NOT_FOUND {
            return LedgerError::Unavailable(format!(
                "{}: {what}: 404: no repository answers at this URL; check HUSHRUN_REPOSITORY_URL or `repository_url:`",
                self.base
            ));
        }
        self.failure(answer, what)
    }

    async fn fetch(&self, path: &str, what: &str) -> Result<Option<Vec<u8>>, LedgerError> {
        // One byte past the limit, so that an oversized object stays visibly oversized (E-8
        // step 1) instead of failing the whole read.
        let answer = self
            .call(&Call::new(Method::GET, path, what, MAX_OBJECT_BYTES + 1))
            .await?;
        match answer.status {
            status if status.is_success() => Ok(Some(answer.body)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(self.failure(&answer, what)),
        }
    }

    async fn get_object(&self, key: &str) -> Result<Vec<u8>, LedgerError> {
        self.fetch(&format!("objects/{key}"), key)
            .await?
            .ok_or_else(|| LedgerError::NotFound(key.to_string()))
    }

    /// Whether a write's answer is the one L-10 promises; a 2xx that isn't is `Unavailable`.
    fn written(
        &self,
        answer: &Answer,
        what: &str,
        done: Done,
        ours: &str,
    ) -> Result<bool, LedgerError> {
        if !answer.status.is_success() {
            return Ok(false);
        }
        let stored = stored_sha256(&answer.body);
        match (done, answer.status) {
            (Done::Created, StatusCode::CREATED) if stored.as_deref() == Some(ours) => Ok(true),
            (Done::NoContent, StatusCode::NO_CONTENT) => Ok(true),
            (Done::Created, StatusCode::CREATED) if stored.is_some() => {
                Err(LedgerError::Unavailable(format!(
                    "{}: {what}: the server stored sha256:{}, but hushrun sent sha256:{ours}",
                    self.base,
                    stored.unwrap_or_default()
                )))
            }
            (_, status) => Err(LedgerError::Unavailable(format!(
                "{}: {what}: the server answered {status}, not {}, so nothing is known to be written",
                self.base,
                match done {
                    Done::Created => "201 Created with the stored bytes' sha256",
                    Done::NoContent => "204 No Content",
                }
            ))),
        }
    }

    /// A create-only `PUT` (L-3): a 412 whose stored hash equals ours is a retried create that
    /// had succeeded. Without a hash in the answer, the object is read back. Returns the
    /// receipt the answer carries, if any (L-11).
    async fn create(
        &self,
        path: &str,
        what: &str,
        bytes: &[u8],
        done: Done,
    ) -> Result<Option<log::Receipt>, LedgerError> {
        let ours = Digest::of(bytes).hex();
        let mut call = Call::new(Method::PUT, path, what, MAX_SMALL_BYTES);
        call.body = Some((bytes, OCTET_STREAM));
        call.create_only = true;
        let answer = self.call(&call).await?;
        let receipt = || {
            serde_json::from_slice::<Written>(&answer.body)
                .ok()
                .and_then(|w| w.receipt)
        };
        if self.written(&answer, what, done, &ours)? {
            return Ok(receipt());
        }
        if answer.status != StatusCode::PRECONDITION_FAILED {
            return Err(self.failure(&answer, what));
        }
        let same = match stored_sha256(&answer.body) {
            Some(stored) => stored == ours,
            None => self.fetch(path, what).await?.is_some_and(|b| b == bytes),
        };
        if same {
            Ok(receipt())
        } else {
            Err(LedgerError::AlreadyExists(what.to_string()))
        }
    }

    /// One listing page, and what it said about the log.
    async fn page(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<(Page, log::PageLog), LedgerError> {
        let limit_text = limit.to_string();
        let mut pairs = Vec::new();
        if !prefix.is_empty() {
            pairs.push(("prefix", prefix));
        }
        if let Some(after) = after {
            pairs.push(("after", after));
        }
        pairs.push(("limit", &limit_text));
        let what = format!("the listing of {prefix:?}");
        let mut call = Call::new(Method::GET, "objects", &what, MAX_PAGE_BYTES);
        call.query = Some(form(&pairs));
        let answer = self.call(&call).await?;
        if !answer.status.is_success() {
            return Err(self.whole_ledger_failure(&answer, &what));
        }
        let mut page: Page = serde_json::from_slice(&answer.body).map_err(|e| {
            LedgerError::Unavailable(format!("{}: {what}: not a listing page: {e}", self.base))
        })?;
        // `next` must move forward through the prefix's keys, or the listing could go on
        // forever.
        if let Some(next) = &page.next {
            let fail = |why: &str| {
                Err(LedgerError::Unavailable(format!(
                    "{}: {what}: {why}, so the listing can't go on",
                    self.base
                )))
            };
            if page.keys.is_empty() {
                return fail("an empty page says more keys follow");
            }
            if !next.starts_with(prefix) {
                return fail(&format!("the next key {next:?} is outside the prefix"));
            }
            if after.is_some_and(|after| next.as_str() <= after)
                || page.keys.last().is_some_and(|last| next < last)
            {
                return fail(&format!("the next key {next:?} doesn't advance"));
            }
        }
        let page_log = log::PageLog {
            after: after.map(str::to_string),
            next: page.next.clone(),
            keys: page.keys.clone(),
            checkpoint: page.checkpoint.take(),
            statement: page.statement.take(),
        };
        // Only keys under the prefix count, whatever the server sends.
        page.keys.retain(|k| k.starts_with(prefix));
        Ok((page, page_log))
    }

    /// Keys under `prefix`, page by page, up to `n` of them. `list_first` asks for one page of
    /// `n` keys, and follows `next` only when the server answers a shorter page (L-10). Also
    /// returns what each page said about the log, for [`HttpLedger::check_pages`].
    async fn list_keys(
        &self,
        prefix: &str,
        n: Option<usize>,
    ) -> Result<(Vec<String>, Vec<log::PageLog>), LedgerError> {
        let mut keys = Vec::new();
        let mut pages = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..self.tuning.max_list_pages {
            let want = n.map_or(PAGE_KEYS, |n| (n - keys.len()).min(PAGE_KEYS));
            let (page, page_log) = self.page(prefix, after.as_deref(), want).await?;
            pages.push(page_log);
            keys.extend(page.keys);
            if keys.len() > MAX_LIST_KEYS {
                return Err(LedgerError::Unavailable(format!(
                    "{}: the listing of {prefix:?} has more than {MAX_LIST_KEYS} keys",
                    self.base
                )));
            }
            let enough = n.is_some_and(|n| keys.len() >= n);
            match page.next {
                Some(next) if !enough => after = Some(next),
                _ => {
                    after = None;
                    break;
                }
            }
        }
        if after.is_some() {
            return Err(LedgerError::Unavailable(format!(
                "{}: the listing of {prefix:?} takes more than {} pages",
                self.base, self.tuning.max_list_pages
            )));
        }
        keys.sort();
        keys.dedup();
        if let Some(n) = n {
            keys.truncate(n);
        }
        Ok((keys, pages))
    }

    /// `DELETE {base}/objects/{key}`, with an administrator's GC statement when there is one
    /// (L-9, L-11).
    fn delete_with(&self, key: &str, gc: Option<String>) -> Result<(), LedgerError> {
        self.io.run(async {
            let path = format!("objects/{key}");
            let mut call = Call::new(Method::DELETE, &path, key, MAX_SMALL_BYTES);
            call.header = gc.map(|statement| (log::GC_HEADER, statement));
            let answer = self.call(&call).await?;
            // A 404 is a retried delete that had succeeded (L-10).
            if answer.status == StatusCode::NOT_FOUND
                || self.written(&answer, key, Done::NoContent, "")?
            {
                Ok(())
            } else {
                Err(self.failure(&answer, key))
            }
        })
    }

    /// One batch-get of at most 100 keys, in their order.
    async fn batch(&self, keys: &[&String]) -> Vec<Result<Vec<u8>, LedgerError>> {
        match self.batch_request(keys).await {
            Ok(results) => results,
            Err(err) => keys.iter().map(|_| Err(err.clone())).collect(),
        }
    }

    async fn batch_request(
        &self,
        keys: &[&String],
    ) -> Result<Vec<Result<Vec<u8>, LedgerError>>, LedgerError> {
        let body = serde_json::to_vec(&serde_json::json!({ "keys": keys })).expect("json");
        let what = format!("a batch-get of {} key(s)", keys.len());
        let mut call = Call::new(Method::POST, "batch-get", &what, MAX_BATCH_BYTES);
        call.body = Some((&body, JSON));
        let answer = self.call(&call).await?;
        if !answer.status.is_success() {
            return Err(self.whole_ledger_failure(&answer, &what));
        }
        let batch: Batch = serde_json::from_slice(&answer.body).map_err(|e| {
            LedgerError::Unavailable(format!(
                "{}: {what}: not a batch-get answer: {e}",
                self.base
            ))
        })?;
        let in_order = batch.objects.len() == keys.len()
            && batch.objects.iter().zip(keys).all(|(o, k)| &o.key == *k);
        if !in_order {
            return Err(LedgerError::Unavailable(format!(
                "{}: {what}: the answer doesn't list the requested keys in order",
                self.base
            )));
        }
        Ok(batch
            .objects
            .into_iter()
            .map(|object| match (object.data, object.error.as_deref()) {
                (Some(data), None) => base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| {
                        LedgerError::Unavailable(format!(
                            "{}: {}: the batch-get data is not standard base64",
                            self.base, object.key
                        ))
                    }),
                (None, Some("not_found")) => Err(LedgerError::NotFound(object.key)),
                (_, code) => Err(LedgerError::Unavailable(format!(
                    "{}: {}: the server answered `{}` for this key",
                    self.base,
                    object.key,
                    code.unwrap_or("nothing")
                ))),
            })
            .collect())
    }
}

impl Ledger for HttpLedger {
    /// A create whose receipt proves the object is in the log, when hushrun.yaml names the log
    /// key (L-11).
    fn append(&self, key: &str, bytes: &[u8]) -> Result<(), LedgerError> {
        check_http_key(key)?;
        let receipt =
            self.io
                .run(self.create(&format!("objects/{key}"), key, bytes, Done::Created))?;
        self.check_receipt(key, bytes, receipt)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, LedgerError> {
        let (keys, pages) = self.io.run(self.list_keys(prefix, None))?;
        self.check_pages(prefix, pages)?;
        Ok(keys)
    }

    fn list_first(&self, prefix: &str, n: usize) -> Result<Vec<String>, LedgerError> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let (keys, pages) = self.io.run(self.list_keys(prefix, Some(n)))?;
        self.check_pages(prefix, pages)?;
        Ok(keys)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, LedgerError> {
        check_http_key(key)?;
        self.io.run(self.get_object(key))
    }

    /// Batch-gets of 100 keys, 16 in flight (L-10, §12.4). Invalid keys never reach the ledger.
    fn get_many(&self, keys: &[String]) -> Vec<Result<Vec<u8>, LedgerError>> {
        let mut results: Vec<Option<Result<Vec<u8>, LedgerError>>> = keys
            .iter()
            .map(|key| check_http_key(key).err().map(Err))
            .collect();
        let valid: Vec<(usize, &String)> = keys
            .iter()
            .enumerate()
            .filter(|(i, _)| results[*i].is_none())
            .collect();
        let fetch = async {
            let batches: Vec<Vec<(usize, &String)>> =
                valid.chunks(BATCH_KEYS).map(<[_]>::to_vec).collect();
            Ok(stream::iter(batches)
                .map(|batch| async move {
                    let keys: Vec<&String> = batch.iter().map(|(_, k)| *k).collect();
                    let objects = self.batch(&keys).await;
                    (batch, objects)
                })
                .buffered(READ_CONCURRENCY)
                .collect::<Vec<_>>()
                .await)
        };
        match self.io.run(fetch) {
            Ok(answers) => {
                for (batch, objects) in answers {
                    for ((i, _), object) in batch.into_iter().zip(objects) {
                        results[i] = Some(object);
                    }
                }
            }
            Err(err) => {
                for (i, _) in &valid {
                    results[*i] = Some(Err(err.clone()));
                }
            }
        }
        results
            .into_iter()
            .map(|r| r.expect("every key has a result"))
            .collect()
    }

    fn delete(&self, key: &str) -> Result<(), LedgerError> {
        check_http_key(key)?;
        self.delete_with(key, None)
    }

    fn log(&self) -> Option<&dyn Log> {
        Some(self)
    }

    /// `GET {base}/signers` (L-13), its statement checked against the log key when the
    /// repository pins one (T-9).
    fn signers(&self, repository: &str) -> Result<crate::SignersAnswer, LedgerError> {
        let body = self.io.run(async {
            let answer = self
                .call(&Call::new(
                    Method::GET,
                    "signers",
                    "the repository's signers",
                    MAX_SMALL_BYTES,
                ))
                .await?;
            if !answer.status.is_success() {
                return Err(self.whole_ledger_failure(&answer, "the repository's signers"));
            }
            Ok(answer.body)
        })?;
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
            LedgerError::Unavailable(format!(
                "{}: GET {}/signers didn't answer JSON: {e}",
                self.base, self.base
            ))
        })?;
        crate::signers::check_answer(&value, repository, self.log_verifier().as_ref()).map_err(
            |why| {
                LedgerError::Unavailable(format!("{}: the repository's signers: {why}", self.base))
            },
        )
    }
}

/// Checks a key or configuration name against L-10: 1 to 1024 bytes of `/`-separated segments
/// of `[A-Za-z0-9._-]`, none `.` or `..`. Every allowed character is unreserved, so keys go
/// into URLs as they are.
pub(crate) fn check_http_key(key: &str) -> Result<(), LedgerError> {
    check_key(key)?;
    let charset = key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'));
    if key.len() > MAX_KEY_BYTES || !charset {
        return Err(LedgerError::Invalid(format!(
            "{key:?} is not a key hushrun's server accepts: at most {MAX_KEY_BYTES} bytes of letters, digits, `.`, `_`, `-` and `/`"
        )));
    }
    Ok(())
}

/// The client: TLS on rustls with ring (ADR 0007), L-4's timeouts, and only 307 and 308
/// redirects followed, within the origin they started from: they keep the method and the body,
/// where a 301, 302 or 303 would turn a write into a `GET`.
fn client(tuning: &Tuning) -> Result<reqwest::Client, LedgerError> {
    install_crypto_provider();
    reqwest::Client::builder()
        .timeout(tuning.request_timeout)
        .connect_timeout(tuning.connect_timeout)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let next = attempt.url();
            let same_origin = attempt.previous().first().is_some_and(|first| {
                first.scheme() == next.scheme()
                    && first.host_str() == next.host_str()
                    && first.port_or_known_default() == next.port_or_known_default()
            });
            let keeps_method = matches!(
                attempt.status(),
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
            );
            if same_origin && keeps_method && attempt.previous().len() <= MAX_REDIRECTS {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(hushrun_core::user_agent())
        .build()
        .map_err(|e| LedgerError::Unavailable(format!("building the HTTP client: {e}")))
}

/// Sends `request` and reads at most `limit` bytes of the answer.
async fn read(request: reqwest::RequestBuilder, limit: usize) -> Result<Answer, reqwest::Error> {
    let mut response = request.send().await?;
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    while body.len() < limit {
        let Some(chunk) = response.chunk().await? else {
            break;
        };
        let room = limit - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    Ok(Answer {
        status,
        body,
        retry_after: retry_after(&headers),
        redirect: status
            .is_redirection()
            .then(|| redirect_origin(&headers))
            .flatten(),
        date: header_text(&headers, header::DATE),
        authorized: false,
        gave_up: None,
        build_sharing: headers
            .get("hushrun-build")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("on")),
    })
}

fn header_text(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// `Retry-After`, in seconds or as an HTTP date.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = header_text(headers, header::RETRY_AFTER)?;
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let when = jiff::fmt::rfc2822::parse(value).ok()?.timestamp();
    let wait = when.duration_since(jiff::Timestamp::now());
    Some(Duration::try_from(wait).unwrap_or(Duration::ZERO))
}

/// The scheme and host of a redirect's target, without its path or query.
fn redirect_origin(headers: &HeaderMap) -> Option<String> {
    let location = header_text(headers, header::LOCATION)?;
    match reqwest::Url::parse(&location) {
        Ok(url) => Some(format!(
            "{}://{}",
            url.scheme(),
            url.host_str().unwrap_or("?")
        )),
        Err(_) => Some("a relative location".to_string()),
    }
}

/// Timeouts, refused or dropped connections and interrupted bodies are retried (L-4).
fn is_transient(err: &reqwest::Error) -> bool {
    !err.is_builder() && !err.is_redirect() && !err.is_decode()
}

/// An error and its sources, such as "error sending request: … connection refused", without
/// the URL.
fn chain(err: reqwest::Error) -> String {
    let err = err.without_url();
    let mut text = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// The `message` of an error body (L-10), shortened.
fn error_message(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error")?;
    let message = error.get("message").and_then(|m| m.as_str());
    let code = error.get("code").and_then(|c| c.as_str());
    match (code, message) {
        (Some(code), Some(message)) => Some(format!("{code}: {}", excerpt(message, 300))),
        (Some(code), None) => Some(code.to_string()),
        (None, Some(message)) => Some(excerpt(message, 300)),
        (None, None) => None,
    }
}

/// The stored object's hash in a create's answer: top-level `sha256` (L-10), or the one inside
/// `error`, lowercased.
fn stored_sha256(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let hex = value
        .get("sha256")
        .or_else(|| value.get("error").and_then(|e| e.get("sha256")))?
        .as_str()?
        .to_ascii_lowercase();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// The clock of an HTTP ledger's server, from the `Date` of `GET {base}` (`doctor`). It needs
/// no token: an unauthenticated answer has a date too.
pub(crate) fn server_time(url: &LedgerUrl) -> Result<Option<std::time::SystemTime>, LedgerError> {
    let ledger = HttpLedger::open(url, &Options::default())?;
    let Some(date) = ledger.date()? else {
        return Ok(None);
    };
    let parsed = jiff::fmt::rfc2822::parse(&date)
        .map_err(|e| LedgerError::Unavailable(format!("{url}: Date {date:?}: {e}")))?;
    Ok(Some(std::time::SystemTime::from(parsed.timestamp())))
}

#[cfg(test)]
mod tests;
