//! Signers' public keys (E-2), validity windows, and what a signing certificate says about its
//! signer (E-11). Matching only: verifying signatures, certificates and timestamps is
//! `hushrun-attest`'s job.

use std::fmt;

use base64::Engine as _;

use crate::digest::Digest;
use crate::time::Time;

const ED25519_PREFIX: &str = "ed25519:";
const P256_PREFIX: &str = "ecdsa-p256:";

/// The DER `SubjectPublicKeyInfo` of a P-256 key up to its point: `SEQUENCE { SEQUENCE {
/// id-ecPublicKey, prime256v1 }, BIT STRING }` with 66 bytes of key and no unused bits.
const P256_SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];
/// The length of a P-256 `SubjectPublicKeyInfo`: the prefix and the 65-byte uncompressed point.
const P256_SPKI_LEN: usize = P256_SPKI_PREFIX.len() + 65;

/// A public key (E-2): Ed25519, or \[M10\] ECDSA P-256, the curve hardware keys, browsers and
/// Sigstore sign with.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PublicKey {
    /// The 32 raw bytes, written `ed25519:<standard base64 of them>`.
    Ed25519([u8; 32]),
    /// The uncompressed point (`0x04`, x, y), written `ecdsa-p256:<standard base64 of the DER
    /// SubjectPublicKeyInfo>`.
    P256([u8; 65]),
}

impl PublicKey {
    /// Parses `ed25519:<base64 of 32 bytes>` or `ecdsa-p256:<base64 of a SubjectPublicKeyInfo>`.
    pub fn parse(text: &str) -> Result<PublicKey, String> {
        let decode = |b64: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| format!("{text:?} is not valid standard base64"))
        };
        if let Some(b64) = text.strip_prefix(ED25519_PREFIX) {
            let raw: [u8; 32] = decode(b64)?
                .try_into()
                .map_err(|_| format!("{text:?} is not 32 bytes"))?;
            return Ok(PublicKey::Ed25519(raw));
        }
        if let Some(b64) = text.strip_prefix(P256_PREFIX) {
            return PublicKey::from_p256_spki(&decode(b64)?).map_err(|e| format!("{text:?}: {e}"));
        }
        Err(format!(
            "{text:?} is not an ed25519:<base64> or ecdsa-p256:<base64> public key"
        ))
    }

    /// A P-256 key from its DER `SubjectPublicKeyInfo`, as browsers and X.509 certificates give it.
    /// Only the canonical form is accepted (an uncompressed point and no other encoding), so a
    /// key has one text and one key id.
    pub fn from_p256_spki(der: &[u8]) -> Result<PublicKey, String> {
        if der.len() != P256_SPKI_LEN || der[..P256_SPKI_PREFIX.len()] != P256_SPKI_PREFIX {
            return Err(
                "not the DER SubjectPublicKeyInfo of a P-256 (prime256v1) key with an uncompressed point"
                    .into(),
            );
        }
        let point: [u8; 65] = der[P256_SPKI_PREFIX.len()..]
            .try_into()
            .expect("checked length");
        PublicKey::from_p256_point(&point)
    }

    /// A P-256 key from its uncompressed point: `0x04`, then x and y.
    pub fn from_p256_point(point: &[u8]) -> Result<PublicKey, String> {
        let point: [u8; 65] = point
            .try_into()
            .map_err(|_| format!("a P-256 point is 65 bytes, not {}", point.len()))?;
        if point[0] != 0x04 {
            return Err("only uncompressed P-256 points (0x04, x, y) are accepted".into());
        }
        Ok(PublicKey::P256(point))
    }

    /// The bytes the text form encodes and the key id hashes: Ed25519's 32 raw bytes, or a
    /// P-256 key's DER `SubjectPublicKeyInfo`.
    pub fn encoded(&self) -> Vec<u8> {
        match self {
            PublicKey::Ed25519(raw) => raw.to_vec(),
            PublicKey::P256(point) => {
                let mut der = Vec::with_capacity(P256_SPKI_LEN);
                der.extend_from_slice(&P256_SPKI_PREFIX);
                der.extend_from_slice(point);
                der
            }
        }
    }

    /// `sha256:<hex SHA-256 of the encoded bytes>` (E-2).
    pub fn keyid(&self) -> String {
        Digest::of(&self.encoded()).to_string()
    }

    /// `ed25519` or `ecdsa-p256`.
    pub fn algorithm(&self) -> &'static str {
        match self {
            PublicKey::Ed25519(_) => "ed25519",
            PublicKey::P256(_) => "ecdsa-p256",
        }
    }

    /// An Ed25519 key's 32 raw bytes, for formats that sign with Ed25519 only, such as a
    /// ledger's signed notes (T-9).
    pub fn ed25519(&self) -> Option<[u8; 32]> {
        match self {
            PublicKey::Ed25519(raw) => Some(*raw),
            PublicKey::P256(_) => None,
        }
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self {
            PublicKey::Ed25519(_) => ED25519_PREFIX,
            PublicKey::P256(_) => P256_PREFIX,
        };
        write!(
            f,
            "{prefix}{}",
            base64::engine::general_purpose::STANDARD.encode(self.encoded())
        )
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// When a key may sign: `not_before` and `not_after`, inclusive, compared with trusted time
/// only. A machine's ends when it is revoked (ADR 0031).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Window {
    pub not_before: Option<Time>,
    pub not_after: Option<Time>,
}

impl Window {
    /// No bounds: valid at any time.
    pub const ALWAYS: Window = Window {
        not_before: None,
        not_after: None,
    };

    pub fn contains(&self, at: Time) -> bool {
        self.not_before.is_none_or(|from| at >= from) && self.not_after.is_none_or(|to| at <= to)
    }

    /// Whether the window has a bound.
    pub fn is_bounded(&self) -> bool {
        self.not_before.is_some() || self.not_after.is_some()
    }
}

impl fmt::Display for Window {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.not_before, self.not_after) {
            (None, None) => f.write_str("at any time"),
            (Some(from), None) => write!(f, "from {from}"),
            (None, Some(to)) => write!(f, "until {to}"),
            (Some(from), Some(to)) => write!(f, "from {from} until {to}"),
        }
    }
}

/// What a signing certificate says about its signer (E-11): the OIDC issuer, the subject
/// alternative name, and the extensions Fulcio adds for CI workflows. Fields a certificate
/// doesn't carry are `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertIdentity {
    /// The OIDC issuer (1.3.6.1.4.1.57264.1.8, or the older .1.1).
    pub issuer: String,
    /// The subject alternative name: a URI or an email address.
    pub subject: String,
    /// Source Repository Identifier (.1.15): the numeric repository id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    /// Source Repository URI (.1.12), such as `https://github.com/acme/back`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Source Repository Ref (.1.14), such as `refs/heads/main`.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Build Config URI (.1.18): the calling workflow, `<repository>/<path>@<ref>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Build Signer URI (.1.9): the workflow that signed, a reusable one when called.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_workflow: Option<String>,
    /// Build Trigger (.1.20): the event that started the run, such as `push` or
    /// `pull_request_target`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
}

/// The trigger that runs a base branch's workflow, with the base branch's ref, for a pull
/// request, often a fork's: a certificate with it never counts as the repository's CI (ADR 0031).
pub const PULL_REQUEST_TARGET: &str = "pull_request_target";

#[cfg(test)]
mod tests {
    use super::*;

    fn time(text: &str) -> Time {
        Time::parse(text).unwrap()
    }

    // covers: E-2
    #[test]
    fn p256_keys_are_written_as_their_spki() {
        let mut point = [7u8; 65];
        point[0] = 0x04;
        let key = PublicKey::from_p256_point(&point).unwrap();
        let der = key.encoded();
        assert_eq!(der.len(), 91);
        assert_eq!(&der[..26], &P256_SPKI_PREFIX);
        let text = key.to_string();
        assert!(
            text.starts_with("ecdsa-p256:MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE"),
            "{text}"
        );
        assert_eq!(PublicKey::parse(&text).unwrap(), key);
        assert_eq!(key.keyid(), Digest::of(&der).to_string());
        assert_eq!(key.algorithm(), "ecdsa-p256");
        // Compressed points, other curves and trailing bytes are refused.
        let mut compressed = der.clone();
        compressed[26] = 0x02;
        assert!(PublicKey::from_p256_spki(&compressed).is_err());
        let mut longer = der.clone();
        longer.push(0);
        assert!(PublicKey::from_p256_spki(&longer).is_err());
        let mut p384 = der.clone();
        p384[22] = 0x22;
        assert!(PublicKey::from_p256_spki(&p384).is_err());
        assert!(PublicKey::parse("ecdsa-p256:AAAA").is_err());
        assert!(PublicKey::parse("ecdsa-p384:AAAA").is_err());
    }

    // covers: T-15
    #[test]
    fn windows_are_inclusive() {
        let window = Window {
            not_before: Some(time("2026-01-01T00:00:00Z")),
            not_after: Some(time("2026-12-31T00:00:00Z")),
        };
        assert!(window.contains(time("2026-01-01T00:00:00Z")));
        assert!(window.contains(time("2026-12-31T00:00:00Z")));
        assert!(!window.contains(time("2025-12-31T23:59:59.999Z")));
        assert!(!window.contains(time("2026-12-31T00:00:00.001Z")));
        assert!(Window::ALWAYS.contains(time("1970-01-01T00:00:00Z")));
        assert!(!Window::ALWAYS.is_bounded());
        assert_eq!(
            window.to_string(),
            "from 2026-01-01T00:00:00.000Z until 2026-12-31T00:00:00.000Z"
        );
    }
}
