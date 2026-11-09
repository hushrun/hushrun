//! The append-only event ledger (§6): the [`Ledger`] contract (L-3), the key layout (L-2), and
//! backends. Storage carries no trust: readers verify every event (E-8).
//!
//! The trait is synchronous: commands read one listing and a bounded set of objects, and the
//! HTTP backend (M18) runs its own runtime behind [`Ledger::get_many`], so the rest of hushrun
//! needs no async code. Bucket backends (`gs://`, `s3://`, `az://`, M4) are gone (ADR 0022).
#![forbid(unsafe_code)]

mod file;
pub mod gc;
#[cfg(feature = "cloud")]
mod http;
pub mod layout;
pub mod log;
#[cfg(test)]
mod mem;
mod quota;
#[cfg(feature = "cloud")]
mod runtime;
pub mod signers;
mod ulid;

use std::fmt;
use std::time::Duration;

use hushrun_core::ledger_url::LedgerUrl;

pub use file::FileLedger;
#[cfg(feature = "cloud")]
pub use http::{HttpLedger, LedgerInfo, LogInfo, PROTOCOL};
pub use log::{LeafRecord, Log, LogKey, RecordedListing};
#[cfg(test)]
pub use mem::MemLedger;
pub use quota::{QUOTA_EXCEEDED, Quota};
pub use signers::{SignersAnswer, check_answer};
pub use ulid::{new_ulid, ulid_millis};

/// Ledger errors (L-3). `Unavailable` and `Denied` make commands exit 6 (L-5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error("{0} does not exist on hushrun's server")]
    NotFound(String),
    #[error("{0} already exists on hushrun's server with other content")]
    AlreadyExists(String),
    #[error("access to the repository was denied: {0}")]
    Denied(String),
    #[error("hushrun's server is unavailable: {0}")]
    Unavailable(String),
    #[error("invalid request to hushrun's server: {0}")]
    Invalid(String),
    /// \[ADR 0032\] The server refused a write for the org's plan (402 `quota_exceeded`,
    /// L-10), such as a new `check` event past the checks the plan records. Never retried;
    /// `run --stage` and `attest` go on without the check event (L-5).
    #[error("hushrun's server refused the write for the org's plan: {0}")]
    Quota(Box<Quota>),
}

impl LedgerError {
    /// Whether the ledger couldn't be reached or refused access (exit 6).
    pub fn is_unavailable(&self) -> bool {
        matches!(self, LedgerError::Unavailable(_) | LedgerError::Denied(_))
    }
}

/// The ledger contract (L-3). Keys are `/`-separated relative paths (L-2).
pub trait Ledger: Send + Sync + fmt::Debug {
    /// Creates `key` with `bytes`; never overwrites. A retried create that finds identical
    /// bytes succeeds.
    fn append(&self, key: &str, bytes: &[u8]) -> Result<(), LedgerError>;

    /// Every key under `prefix`, sorted ascending.
    fn list(&self, prefix: &str) -> Result<Vec<String>, LedgerError>;

    /// The first `n` keys under `prefix`, in ascending order.
    fn list_first(&self, prefix: &str, n: usize) -> Result<Vec<String>, LedgerError> {
        let mut keys = self.list(prefix)?;
        keys.truncate(n);
        Ok(keys)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, LedgerError>;

    /// Several objects; the HTTP backend fetches them concurrently (bounded, §12.4).
    fn get_many(&self, keys: &[String]) -> Vec<Result<Vec<u8>, LedgerError>> {
        keys.iter().map(|k| self.get(k)).collect()
    }

    /// Deletes `key` (garbage collection only, L-9).
    fn delete(&self, key: &str) -> Result<(), LedgerError>;

    /// \[M18\] The ledger's transparency log (L-11), when it keeps one: HTTP ledgers do.
    fn log(&self) -> Option<&dyn Log> {
        None
    }

    /// \[ADR 0031\] The signers whose events count for the repository (L-13, T-15), checked
    /// against the log key the repository pins when [`Log::configure`] set one. A ledger with no
    /// server, such as a test's, has none to answer.
    fn signers(&self, repository: &str) -> Result<SignersAnswer, LedgerError> {
        let _ = repository;
        Err(LedgerError::Invalid(
            "this repository has no server to name its signers".into(),
        ))
    }
}

/// How to open a ledger. Every field comes from the environment, never from the repository
/// (T-4).
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The total time this command's requests may take (L-4: 60 s for `plan`). Only HTTP
    /// ledgers spend it.
    pub budget: Option<Duration>,
    /// A command that prints a bearer token for `https://` ledgers
    /// (`HUSHRUN_REPOSITORY_TOKEN_COMMAND`, §11.3, L-10).
    pub token_command: Option<String>,
    /// \[M18\] A bearer token for `https://` ledgers (`HUSHRUN_REPOSITORY_TOKEN`, L-10).
    pub token: Option<Secret>,
    /// \[M18\] GitHub Actions' ID token endpoint, for `https://` ledgers (L-10).
    pub github_oidc: Option<OidcEndpoint>,
}

/// A secret, such as a ledger token: `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    /// The secret itself, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// Where a GitHub Actions job asks for ID tokens: `ACTIONS_ID_TOKEN_REQUEST_URL` and
/// `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, set when the job has `permissions: id-token: write`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcEndpoint {
    pub url: String,
    pub token: Secret,
}

/// Opens the ledger at `url`.
pub fn open(url: &LedgerUrl) -> Result<Box<dyn Ledger>, LedgerError> {
    open_with(url, &Options::default())
}

/// Opens the ledger at `url` with `options`: every ledger speaks the HTTP protocol (L-10).
pub fn open_with(url: &LedgerUrl, options: &Options) -> Result<Box<dyn Ledger>, LedgerError> {
    #[cfg(feature = "cloud")]
    return Ok(Box::new(HttpLedger::open(url, options)?));
    #[cfg(not(feature = "cloud"))]
    {
        let _ = options;
        Err(LedgerError::Invalid(format!(
            "{url}: this build of hushrun-ledger has no HTTP backend (feature `cloud`)"
        )))
    }
}

/// The clock of the server behind a ledger (`doctor`), or `None` when it doesn't say.
pub fn server_time(url: &LedgerUrl) -> Result<Option<std::time::SystemTime>, LedgerError> {
    #[cfg(feature = "cloud")]
    return http::server_time(url);
    #[cfg(not(feature = "cloud"))]
    {
        let _ = url;
        Ok(None)
    }
}

/// \[M18\] What a ledger says about itself (`GET {base}`, L-10), for `doctor`.
#[cfg(feature = "cloud")]
pub fn describe(url: &LedgerUrl, options: &Options) -> Result<LedgerInfo, LedgerError> {
    HttpLedger::open(url, options)?.info()
}

/// Checks that `key` is a safe relative key: `/`-separated, no empty, `.` or `..` segments,
/// no backslash or NUL.
pub fn check_key(key: &str) -> Result<(), LedgerError> {
    let bad = key.is_empty()
        || key.starts_with('/')
        || key.contains(['\\', '\0'])
        || key
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        Err(LedgerError::Invalid(format!(
            "{key:?} is not a key hushrun's server stores"
        )))
    } else {
        Ok(())
    }
}

/// The backend contract as reusable checks, for this crate's tests.
#[cfg(test)]
mod conformance;
