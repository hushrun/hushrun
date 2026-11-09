//! Sigstore trust roots (E-10, E-11, T-12): the certificate authorities that issue signing
//! certificates, the timestamp authorities that timestamp signatures, and the certificate
//! transparency logs that certificates must be logged in. The format is `trusted_root.json`,
//! which Sigstore's TUF repository distributes.
//!
//! A trust root is trust: it never comes from the content under evaluation. hushrun embeds a
//! snapshot of the public-good instance's; the environment may name another file, and
//! `hushrun sigstore refresh` caches a newer one that TUF verified.

use std::sync::OnceLock;

use base64::Engine as _;
use serde::Deserialize;

use hushrun_core::digest::Digest;
use hushrun_core::signers::{PublicKey, Window};
use hushrun_core::time::Time;

use crate::x509::{Cert, CtLog, verify_embedded_sct};

/// The media types of `trusted_root.json` hushrun reads.
pub const MEDIA_TYPES: [&str; 2] = [
    "application/vnd.dev.sigstore.trustedroot+json;version=0.1",
    "application/vnd.dev.sigstore.trustedroot.v0.2+json",
];

/// The public-good instance's trust root, as vendored (`sigstore/README.md`).
pub const PUBLIC_GOOD: &[u8] = include_bytes!("../sigstore/trusted_root.json");

/// The CT log key type Sigstore's logs use; logs of other types are ignored.
const CT_KEY_DETAILS: &str = "PKIX_ECDSA_P256_SHA_256";

/// A parsed trust root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedRoot {
    /// Certificate authorities, such as Fulcio's.
    pub certificate_authorities: Vec<Authority>,
    /// Timestamp authorities (RFC 3161).
    pub timestamp_authorities: Vec<Authority>,
    /// Certificate transparency logs (RFC 6962). When there are any, a signing certificate
    /// must carry an SCT from one of them.
    pub ct_logs: Vec<CtLog>,
    /// `sha256:` of the file.
    pub digest: Digest,
    /// Where it came from, for messages.
    pub source: String,
}

/// A certificate or timestamp authority: its certificates, the one nearest the signer first,
/// and when the trust root trusts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    pub uri: String,
    pub chain: Vec<Cert>,
    pub valid_for: Window,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRoot {
    media_type: String,
    #[serde(default)]
    certificate_authorities: Vec<RawAuthority>,
    #[serde(default)]
    timestamp_authorities: Vec<RawAuthority>,
    #[serde(default)]
    ctlogs: Vec<RawLog>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAuthority {
    #[serde(default)]
    uri: String,
    cert_chain: RawChain,
    #[serde(default)]
    valid_for: RawWindow,
}

#[derive(Deserialize)]
struct RawChain {
    certificates: Vec<RawBytes>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBytes {
    raw_bytes: String,
}

#[derive(Default, Deserialize)]
struct RawWindow {
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLog {
    #[serde(default)]
    base_url: String,
    public_key: RawPublicKey,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPublicKey {
    #[serde(default)]
    raw_bytes: Option<String>,
    #[serde(default)]
    key_details: String,
    #[serde(default)]
    valid_for: RawWindow,
}

fn window(raw: &RawWindow) -> Result<Window, String> {
    let time = |text: &Option<String>| {
        text.as_deref()
            .map(|t| Time::parse(t).map_err(|e| e.to_string()))
            .transpose()
    };
    Ok(Window {
        not_before: time(&raw.start)?,
        not_after: time(&raw.end)?,
    })
}

fn decode(b64: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "rawBytes is not standard base64".to_string())
}

fn authorities(raw: &[RawAuthority], what: &str) -> Result<Vec<Authority>, String> {
    raw.iter()
        .enumerate()
        .map(|(i, authority)| {
            let fail = |e: String| format!("{what} {i} ({}): {e}", authority.uri);
            let chain = authority
                .cert_chain
                .certificates
                .iter()
                .map(|c| decode(&c.raw_bytes).and_then(|der| Cert::parse(&der)))
                .collect::<Result<Vec<_>, _>>()
                .map_err(fail)?;
            if chain.is_empty() {
                return Err(fail("no certificates".into()));
            }
            Ok(Authority {
                uri: authority.uri.clone(),
                chain,
                valid_for: window(&authority.valid_for).map_err(fail)?,
            })
        })
        .collect()
}

impl TrustedRoot {
    /// Parses a `trusted_root.json`; `source` says where it came from.
    pub fn parse(bytes: &[u8], source: &str) -> Result<TrustedRoot, String> {
        let raw: RawRoot = serde_json::from_slice(bytes)
            .map_err(|e| format!("{source}: not a Sigstore trusted root: {e}"))?;
        if !MEDIA_TYPES.contains(&raw.media_type.as_str()) {
            return Err(format!(
                "{source}: media type {:?}, expected {}",
                raw.media_type,
                MEDIA_TYPES.join(" or ")
            ));
        }
        let fail = |e: String| format!("{source}: {e}");
        let mut ct_logs = Vec::new();
        for log in &raw.ctlogs {
            if log.public_key.key_details != CT_KEY_DETAILS {
                continue;
            }
            let der = log
                .public_key
                .raw_bytes
                .as_deref()
                .ok_or_else(|| fail(format!("CT log {} has no key", log.base_url)))
                .and_then(|b| decode(b).map_err(fail))?;
            let key = PublicKey::from_p256_spki(&der)
                .map_err(|e| fail(format!("CT log {}: {e}", log.base_url)))?;
            ct_logs.push(CtLog {
                base_url: log.base_url.clone(),
                log_id: *Digest::of(&der).as_bytes(),
                key,
                valid_for: window(&log.public_key.valid_for).map_err(fail)?,
            });
        }
        Ok(TrustedRoot {
            certificate_authorities: authorities(
                &raw.certificate_authorities,
                "certificate authority",
            )
            .map_err(fail)?,
            timestamp_authorities: authorities(&raw.timestamp_authorities, "timestamp authority")
                .map_err(fail)?,
            ct_logs,
            digest: Digest::of(bytes),
            source: source.to_string(),
        })
    }

    /// The public-good instance's trust root, from the snapshot in the binary.
    pub fn public_good() -> &'static TrustedRoot {
        static ROOT: OnceLock<TrustedRoot> = OnceLock::new();
        ROOT.get_or_init(|| {
            TrustedRoot::parse(
                PUBLIC_GOOD,
                "the public-good Sigstore trust root in hushrun",
            )
            .expect("the vendored trust root parses")
        })
    }

    /// Checks that a certificate authority of this root issued `leaf` (E-11): the authority
    /// and every certificate of the chain were valid at `at`, the trusted time of the
    /// signature, and when the root lists CT logs, one of them logged the certificate.
    pub fn verify_signing_certificate(&self, leaf: &Cert, at: Time) -> Result<&Authority, String> {
        let mut reasons = Vec::new();
        for authority in &self.certificate_authorities {
            match issued_by(leaf, authority, at, &self.ct_logs) {
                Ok(()) => return Ok(authority),
                Err(reason) => reasons.push(format!("{}: {reason}", authority.uri)),
            }
        }
        Err(if reasons.is_empty() {
            format!("{} lists no certificate authority", self.source)
        } else {
            format!(
                "no certificate authority of {} issued it ({})",
                self.source,
                reasons.join("; ")
            )
        })
    }
}

fn issued_by(leaf: &Cert, authority: &Authority, at: Time, logs: &[CtLog]) -> Result<(), String> {
    if !authority.valid_for.contains(at) {
        return Err(format!("trusted {}, not at {at}", authority.valid_for));
    }
    let issuer = authority.chain.first().ok_or("an empty chain")?;
    leaf.verify_issued_by(issuer)?;
    for pair in authority.chain.windows(2) {
        pair[0].verify_issued_by(&pair[1])?;
    }
    for cert in std::iter::once(leaf).chain(&authority.chain) {
        if !cert.validity().contains(at) {
            return Err(format!(
                "{} is valid {}, not at {at}",
                cert.inner.tbs_certificate.subject,
                cert.validity()
            ));
        }
    }
    if !logs.is_empty() {
        verify_embedded_sct(leaf, issuer, logs)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: E-11
    #[test]
    fn the_public_good_snapshot_parses() {
        let root = TrustedRoot::public_good();
        assert_eq!(root.certificate_authorities.len(), 2);
        assert!(
            root.certificate_authorities
                .iter()
                .all(|ca| ca.uri == "https://fulcio.sigstore.dev")
        );
        assert_eq!(root.timestamp_authorities.len(), 1);
        assert_eq!(
            root.timestamp_authorities[0].uri,
            "https://timestamp.sigstore.dev/api/v1/timestamp"
        );
        assert_eq!(root.ct_logs.len(), 2);
        assert!(
            root.ct_logs
                .iter()
                .any(|log| log.base_url == "https://ctfe.sigstore.dev/2022")
        );
        assert_eq!(
            root.digest.to_string(),
            "sha256:6494e21ea73fa7ee769f85f57d5a3e6a08725eae1e38c755fc3517c9e6bc0b66"
        );
        // The current intermediate is issued by the root next to it.
        let current = &root.certificate_authorities[1];
        current.chain[0]
            .verify_issued_by(&current.chain[1])
            .unwrap();
        assert!(current.valid_for.not_after.is_none());
    }

    #[test]
    fn refuses_what_is_not_a_trust_root() {
        assert!(TrustedRoot::parse(b"{}", "x").unwrap_err().contains("x:"));
        let other = br#"{"mediaType": "application/json"}"#;
        assert!(
            TrustedRoot::parse(other, "x")
                .unwrap_err()
                .contains("media type")
        );
        let empty_chain =
            br#"{"mediaType": "application/vnd.dev.sigstore.trustedroot+json;version=0.1",
            "certificateAuthorities": [{"uri": "u", "certChain": {"certificates": []}}]}"#;
        assert!(
            TrustedRoot::parse(empty_chain, "x")
                .unwrap_err()
                .contains("no certificates")
        );
        let root = TrustedRoot::parse(
            br#"{"mediaType": "application/vnd.dev.sigstore.trustedroot.v0.2+json"}"#,
            "empty",
        )
        .unwrap();
        assert!(root.certificate_authorities.is_empty() && root.ct_logs.is_empty());
    }
}
