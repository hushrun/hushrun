//! \[M18\] hushrun.yaml's `transparency_log` (T-9): the key that signs the server's checkpoints
//! and listings, as the repository pins it, so that the server can't vouch for its own log.
//! \[ADR 0031\] A top-level key of hushrun.yaml, read with the rest of the file (T-5).

use serde::Deserialize;

use crate::signers::PublicKey;
use crate::yaml::{Errors, Spanned};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLedgerLog {
    pub(crate) origin: Spanned<String>,
    pub(crate) key: Spanned<String>,
}

// --- Validated ---

/// \[M18\] The transparency log of an HTTP ledger (T-9): the origin its checkpoints name, and the
/// public half of the key that signs them, so that the ledger can't vouch for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerLog {
    /// The log's name, also the name of its key in signed notes: the ledger's URL without its
    /// scheme, such as `hushrun.example/l/acme/back`.
    pub origin: String,
    pub key: PublicKey,
}

/// \[M18\] `transparency_log` (T-9), validated.
pub fn parse_ledger_log(raw: &RawLedgerLog, errors: &mut Errors) -> Option<LedgerLog> {
    if let Err(reason) = check_log_origin(&raw.origin.value) {
        errors.push(raw.origin.pos, reason);
    }
    let key = parse_log_key(&raw.key.value)
        .map_err(|reason| errors.push(raw.key.pos, reason))
        .ok();
    Some(LedgerLog {
        origin: raw.origin.value.clone(),
        key: key?,
    })
}

/// Whether `origin` can name a log: printable ASCII without spaces or `+` (C2SP signed-note
/// key names), and no scheme.
pub fn check_log_origin(origin: &str) -> Result<(), String> {
    if origin.is_empty() || !origin.bytes().all(|b| b.is_ascii_graphic() && b != b'+') {
        return Err(format!(
            "{origin:?} can't name a log: an origin is printable ASCII without spaces or `+`"
        ));
    }
    if origin.contains("://") {
        return Err(format!(
            "{origin:?} has a scheme; an origin is the repository's URL without it, such as hushrun.example/l/acme/back"
        ));
    }
    Ok(())
}

/// A log key (T-9): Ed25519 only, since the server signs its checkpoints as signed notes.
pub fn parse_log_key(text: &str) -> Result<PublicKey, String> {
    match PublicKey::parse(text)? {
        key if key.ed25519().is_none() => Err(
            "a log key is Ed25519 (`ed25519:<base64>`): the server signs its checkpoints as signed notes, which use Ed25519"
                .to_string(),
        ),
        key => Ok(key),
    }
}
