//! DSSE envelopes (E-1).
//!
//! ```text
//! PAE(type, body) = "DSSEv1" SP LEN(type) SP type SP LEN(body) SP body
//! ```

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::keys::SigningKey;
use crate::signer::{SignError, Signer};

/// The payload type of every event.
pub const PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
/// Larger objects are rejected before parsing (E-8 step 1).
pub const MAX_ENVELOPE_BYTES: usize = 1 << 20;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// A DSSE envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    #[serde(rename = "payloadType")]
    pub payload_type: String,
    /// Standard padded base64 of the payload bytes.
    pub payload: String,
    pub signatures: Vec<Signature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    /// The key id (E-2). DSSE makes it optional, and a keyless signature in a bundle has none:
    /// its certificate says who signed.
    #[serde(default)]
    pub keyid: String,
    /// Standard padded base64 of the signature bytes.
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DsseError {
    #[error("the object is {0} bytes; envelopes are at most 1 MiB")]
    TooLarge(usize),
    #[error("not a DSSE envelope: {0}")]
    Malformed(String),
    #[error("the payload is not standard padded base64")]
    Payload,
}

/// The DSSE pre-authentication encoding.
pub fn pae(payload_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "DSSEv1 {} {payload_type} {} ",
        payload_type.len(),
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// An envelope around `payload`, signed by `key`.
pub fn sign(payload: &[u8], key: &SigningKey) -> Envelope {
    let mut envelope = Envelope::unsigned(PAYLOAD_TYPE, payload);
    envelope
        .signatures
        .push(signature(&envelope.payload_type, payload, key));
    envelope
}

/// `key`'s signature over the PAE of `payload_type` and `payload`.
fn signature(payload_type: &str, payload: &[u8], key: &SigningKey) -> Signature {
    Signature {
        keyid: key.keyid(),
        sig: B64.encode(key.sign(&pae(payload_type, payload))),
    }
}

impl Envelope {
    /// An envelope of `payload_type` around `payload`, with no signature yet.
    pub fn unsigned(payload_type: &str, payload: &[u8]) -> Envelope {
        Envelope {
            payload_type: payload_type.to_string(),
            payload: B64.encode(payload),
            signatures: Vec::new(),
        }
    }

    /// Adds `key`'s signature over this envelope's payload type and payload. Returns `false`,
    /// and changes nothing, when the envelope already has a signature with that key's id.
    pub fn add_signature(&mut self, key: &SigningKey) -> Result<bool, DsseError> {
        let keyid = key.keyid();
        if self.signatures.iter().any(|s| s.keyid == keyid) {
            return Ok(false);
        }
        let payload = self.payload_bytes()?;
        self.signatures
            .push(signature(&self.payload_type, &payload, key));
        Ok(true)
    }

    /// Adds `signer`'s signature, as [`add_signature`](Envelope::add_signature) does, for any
    /// signer with a key, such as a key file.
    pub fn add_signature_by(&mut self, signer: &dyn Signer) -> Result<bool, SignError> {
        let key = signer.public_key().ok_or_else(|| {
            SignError::Config(format!(
                "{} has no key, and envelopes are signed by keys",
                signer.describe()
            ))
        })?;
        let keyid = key.keyid();
        if self.signatures.iter().any(|s| s.keyid == keyid) {
            return Ok(false);
        }
        let payload = self
            .payload_bytes()
            .map_err(|e| SignError::Config(e.to_string()))?;
        let sig = signer.sign(&pae(&self.payload_type, &payload))?;
        self.signatures.push(Signature {
            keyid,
            sig: B64.encode(sig),
        });
        Ok(true)
    }

    /// The envelope of an event's bytes: a plain DSSE envelope, or \[M10\] the one inside a
    /// Sigstore bundle (E-1). Objects over 1 MiB are refused. This reads what an event claims;
    /// only [`verify`](crate::verify()) says whether it counts.
    pub fn parse(bytes: &[u8]) -> Result<Envelope, DsseError> {
        match crate::bundle::Object::parse(bytes)? {
            crate::bundle::Object::Envelope(envelope) => Ok(envelope),
            crate::bundle::Object::Bundle(bundle) => bundle
                .dsse_envelope
                .ok_or_else(|| DsseError::Malformed("a bundle without a DSSE envelope".into())),
        }
    }

    /// A plain DSSE envelope, and nothing else: root versions and signed trust (T-8).
    pub fn parse_plain(bytes: &[u8]) -> Result<Envelope, DsseError> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(DsseError::TooLarge(bytes.len()));
        }
        serde_json::from_slice(bytes).map_err(|e| DsseError::Malformed(e.to_string()))
    }

    /// The compact JSON bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("envelopes serialize")
    }

    /// The decoded payload.
    pub fn payload_bytes(&self) -> Result<Vec<u8>, DsseError> {
        B64.decode(&self.payload).map_err(|_| DsseError::Payload)
    }
}

/// Decodes a signature's bytes, if they are standard padded base64.
pub fn signature_bytes(signature: &Signature) -> Option<Vec<u8>> {
    B64.decode(&signature.sig).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::verify_signature;

    // covers: E-1
    #[test]
    fn pae_matches_the_dsse_spec() {
        // The example of the DSSE protocol specification.
        assert_eq!(
            pae("http://example.com/HelloWorld", b"hello world"),
            b"DSSEv1 29 http://example.com/HelloWorld 11 hello world"
        );
        assert_eq!(pae("", b""), b"DSSEv1 0  0 ");
        // LEN is the byte length, not the character length.
        assert_eq!(pae("t", "é".as_bytes()), "DSSEv1 1 t 2 é".as_bytes());
    }

    // covers: E-1
    #[test]
    fn signs_and_round_trips() {
        let key = SigningKey::from_seed(&[7; 32]);
        let envelope = sign(b"{\"a\":1}", &key);
        assert_eq!(envelope.payload_type, PAYLOAD_TYPE);
        assert_eq!(envelope.payload, "eyJhIjoxfQ==");
        let parsed = Envelope::parse(&envelope.to_bytes()).unwrap();
        assert_eq!(parsed, envelope);
        let sig = signature_bytes(&parsed.signatures[0]).unwrap();
        assert!(verify_signature(
            &key.public_key(),
            &pae(PAYLOAD_TYPE, &parsed.payload_bytes().unwrap()),
            &sig
        ));
        assert!(Envelope::parse(&vec![b' '; MAX_ENVELOPE_BYTES + 1]).is_err());
        assert!(Envelope::parse(b"{\"payloadType\":\"x\"}").is_err());
        // A bundle's envelope, for readers that decode events; signed trust takes plain ones.
        let bundle = crate::bundle::Bundle::new(
            envelope.clone(),
            crate::bundle::Material::Key(key.keyid()),
            &[],
        )
        .to_bytes();
        assert_eq!(Envelope::parse(&bundle).unwrap(), envelope);
        assert!(Envelope::parse_plain(&bundle).is_err());
        assert_eq!(
            Envelope::parse_plain(&envelope.to_bytes()).unwrap(),
            envelope
        );
    }

    // covers: E-1
    #[test]
    fn signatures_are_added_once_per_key() {
        let (a, b) = (
            SigningKey::from_seed(&[1; 32]),
            SigningKey::from_seed(&[2; 32]),
        );
        let mut envelope = Envelope::unsigned("application/x", b"body");
        assert!(envelope.signatures.is_empty());
        assert!(envelope.add_signature(&a).unwrap());
        assert!(envelope.add_signature(&b).unwrap());
        assert!(!envelope.add_signature(&a).unwrap(), "a key signs once");
        assert_eq!(envelope.signatures.len(), 2);
        for (signature, key) in envelope.signatures.iter().zip([&a, &b]) {
            assert!(verify_signature(
                &key.public_key(),
                &pae("application/x", b"body"),
                &signature_bytes(signature).unwrap()
            ));
        }
        // An event envelope is the same thing with the in-toto payload type.
        let mut by_hand = Envelope::unsigned(PAYLOAD_TYPE, b"{}");
        by_hand.add_signature(&a).unwrap();
        assert_eq!(by_hand, sign(b"{}", &a));
    }
}
