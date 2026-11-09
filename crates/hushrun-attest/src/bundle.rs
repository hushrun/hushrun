//! Sigstore bundles (E-1) \[M10\]: every event hushrun writes from M10 on is a bundle of version
//! 0.3 around the same DSSE envelope, with exactly one signature. The bundle carries what
//! verifies that signature (a key id, or the signing certificate) and its RFC 3161 timestamps
//! (E-10). Readers accept plain DSSE envelopes too: every event written before M10 is one.
//!
//! ```json
//! {"mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
//!  "verificationMaterial": {"publicKey": {"hint": "sha256:…"}, "tlogEntries": [],
//!                           "timestampVerificationData": {"rfc3161Timestamps": [{"signedTimestamp": "<base64 DER>"}]}},
//!  "dsseEnvelope": {"payload": "…", "payloadType": "application/vnd.in-toto+json",
//!                   "signatures": [{"sig": "…", "keyid": "sha256:…"}]}}
//! ```

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::dsse::{DsseError, Envelope, MAX_ENVELOPE_BYTES};

/// The media type hushrun writes.
pub const BUNDLE_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";
/// The older spelling of version 0.3, which readers also accept.
pub const BUNDLE_MEDIA_TYPE_0_3: &str = "application/vnd.dev.sigstore.bundle+json;version=0.3";

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// A Sigstore bundle, in the protobuf JSON form of `dev.sigstore.bundle.v1.Bundle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    pub media_type: String,
    pub verification_material: VerificationMaterial,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsse_envelope: Option<Envelope>,
    /// A signature over an artifact rather than an envelope: never an event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_signature: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationMaterial {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<PublicKeyIdentifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<RawBytes>,
    /// Bundles before 0.3 carried a chain; version 0.3 carries the leaf only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x509_certificate_chain: Option<serde_json::Value>,
    /// Transparency log entries. hushrun writes none (E-10).
    #[serde(default)]
    pub tlog_entries: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_verification_data: Option<TimestampVerificationData>,
}

/// A key hint: the signature's key id (E-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKeyIdentifier {
    pub hint: String,
}

/// DER bytes in standard base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawBytes {
    pub raw_bytes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimestampVerificationData {
    #[serde(default)]
    pub rfc3161_timestamps: Vec<SignedTimestamp>,
}

/// An RFC 3161 `TimeStampResp`, DER in standard base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedTimestamp {
    pub signed_timestamp: String,
}

/// What verifies a bundle's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Material {
    /// A key, by its key id (E-2).
    Key(String),
    /// A signing certificate, DER (E-11).
    Certificate(Vec<u8>),
}

/// An event object as stored: a plain DSSE envelope, or a bundle around one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Object {
    Envelope(Envelope),
    Bundle(Box<Bundle>),
}

impl Object {
    /// Parses an object of at most 1 MiB: a bundle when it has a `mediaType`, else a DSSE
    /// envelope.
    pub fn parse(bytes: &[u8]) -> Result<Object, DsseError> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(DsseError::TooLarge(bytes.len()));
        }
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| DsseError::Malformed(e.to_string()))?;
        if value.get("mediaType").is_some() {
            let bundle: Bundle = serde_json::from_value(value)
                .map_err(|e| DsseError::Malformed(format!("bundle: {e}")))?;
            Ok(Object::Bundle(Box::new(bundle)))
        } else {
            serde_json::from_value(value)
                .map(Object::Envelope)
                .map_err(|e| DsseError::Malformed(e.to_string()))
        }
    }

    /// The DSSE envelope: the object itself, or the bundle's.
    pub fn envelope(&self) -> Option<&Envelope> {
        match self {
            Object::Envelope(envelope) => Some(envelope),
            Object::Bundle(bundle) => bundle.dsse_envelope.as_ref(),
        }
    }
}

impl Bundle {
    /// A bundle of version 0.3 around `envelope`, which has one signature, with what verifies
    /// it and its timestamps (DER `TimeStampResp`s).
    pub fn new(envelope: Envelope, material: Material, timestamps: &[Vec<u8>]) -> Bundle {
        let (public_key, certificate) = match material {
            Material::Key(keyid) => (Some(PublicKeyIdentifier { hint: keyid }), None),
            Material::Certificate(der) => (
                None,
                Some(RawBytes {
                    raw_bytes: B64.encode(der),
                }),
            ),
        };
        let timestamp_verification_data =
            (!timestamps.is_empty()).then(|| TimestampVerificationData {
                rfc3161_timestamps: timestamps
                    .iter()
                    .map(|der| SignedTimestamp {
                        signed_timestamp: B64.encode(der),
                    })
                    .collect(),
            });
        Bundle {
            media_type: BUNDLE_MEDIA_TYPE.to_string(),
            verification_material: VerificationMaterial {
                public_key,
                certificate,
                x509_certificate_chain: None,
                tlog_entries: Vec::new(),
                timestamp_verification_data,
            },
            dsse_envelope: Some(envelope),
            message_signature: None,
        }
    }

    /// The compact JSON bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("bundles serialize")
    }

    /// The media type is one readers accept: version 0.3.
    pub fn is_supported(&self) -> bool {
        self.media_type == BUNDLE_MEDIA_TYPE || self.media_type == BUNDLE_MEDIA_TYPE_0_3
    }

    /// What verifies the signature, decoded.
    pub fn material(&self) -> Result<Material, String> {
        let material = &self.verification_material;
        if material.x509_certificate_chain.is_some() {
            return Err("a certificate chain is a bundle of version 0.1 or 0.2; version 0.3 carries the signing certificate".into());
        }
        match (&material.public_key, &material.certificate) {
            (Some(key), None) => Ok(Material::Key(key.hint.clone())),
            (None, Some(certificate)) => B64
                .decode(&certificate.raw_bytes)
                .map(Material::Certificate)
                .map_err(|_| "the certificate is not standard base64".to_string()),
            _ => Err(
                "the verification material has neither a public key nor a certificate, or both"
                    .into(),
            ),
        }
    }

    /// The RFC 3161 timestamps, decoded, in order.
    pub fn timestamps(&self) -> Result<Vec<Vec<u8>>, String> {
        self.verification_material
            .timestamp_verification_data
            .iter()
            .flat_map(|data| &data.rfc3161_timestamps)
            .map(|t| {
                B64.decode(&t.signed_timestamp)
                    .map_err(|_| "a timestamp is not standard base64".to_string())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsse::{self, PAYLOAD_TYPE};
    use crate::keys::SigningKey;

    // covers: E-1
    #[test]
    fn bundles_wrap_the_envelope_and_round_trip() {
        let key = SigningKey::from_seed(&[3; 32]);
        let envelope = dsse::sign(b"{\"a\":1}", &key);
        let bundle = Bundle::new(
            envelope.clone(),
            Material::Key(key.keyid()),
            &[vec![0x30, 0x03, 0x02, 0x01, 0x00]],
        );
        let bytes = bundle.to_bytes();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            text.starts_with(
                r#"{"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json","verificationMaterial":{"publicKey":{"hint":"sha256:"#
            ),
            "{text}"
        );
        assert!(text.contains(r#""tlogEntries":[],"timestampVerificationData":{"rfc3161Timestamps":[{"signedTimestamp":"MAMCAQA="}]}"#), "{text}");
        let Object::Bundle(parsed) = Object::parse(&bytes).unwrap() else {
            panic!("a bundle")
        };
        assert_eq!(*parsed, bundle);
        assert!(parsed.is_supported());
        assert_eq!(parsed.material().unwrap(), Material::Key(key.keyid()));
        assert_eq!(
            parsed.timestamps().unwrap(),
            vec![vec![0x30, 0x03, 0x02, 0x01, 0x00]]
        );
        assert_eq!(
            parsed.dsse_envelope.as_ref().unwrap().payload_type,
            PAYLOAD_TYPE
        );
        // A plain envelope is still an envelope.
        assert_eq!(
            Object::parse(&envelope.to_bytes()).unwrap(),
            Object::Envelope(envelope.clone())
        );
        // A certificate, no timestamps.
        let bundle = Bundle::new(envelope, Material::Certificate(vec![1, 2, 3]), &[]);
        let text = String::from_utf8(bundle.to_bytes()).unwrap();
        assert!(
            text.contains(r#""certificate":{"rawBytes":"AQID"},"tlogEntries":[]},"#),
            "{text}"
        );
        let Object::Bundle(parsed) = Object::parse(text.as_bytes()).unwrap() else {
            panic!("a bundle")
        };
        assert_eq!(
            parsed.material().unwrap(),
            Material::Certificate(vec![1, 2, 3])
        );
        assert!(parsed.timestamps().unwrap().is_empty());
        assert!(Object::parse(&vec![b' '; MAX_ENVELOPE_BYTES + 1]).is_err());
    }

    // covers: E-1
    #[test]
    fn other_bundles_are_read_as_written() {
        let other = Bundle {
            media_type: "application/vnd.dev.sigstore.bundle+json;version=0.2".into(),
            ..Bundle::new(
                Envelope::unsigned(PAYLOAD_TYPE, b"{}"),
                Material::Key("k".into()),
                &[],
            )
        };
        assert!(!other.is_supported());
        let mut chain = other.clone();
        chain.verification_material.x509_certificate_chain = Some(serde_json::json!({}));
        assert!(chain.material().unwrap_err().contains("0.3"));
        let mut neither = other;
        neither.verification_material.public_key = None;
        assert!(neither.material().is_err());
    }
}
