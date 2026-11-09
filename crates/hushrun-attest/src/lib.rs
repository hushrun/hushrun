//! in-toto statements, DSSE envelopes and Sigstore bundles, keys, signing certificates, RFC 3161
//! timestamps, event verification (E-1 … E-11), and \[M18\] GC statements
//! (L-9).
//!
//! Verification is pure: bytes, trust and a time in, a verdict out. The control plane's server
//! calls [`verify()`] as the CLI does. Signers that use the network (keyless signing, timestamp
//! authorities) live behind the `sigstore` feature.
#![forbid(unsafe_code)]

pub mod bundle;
mod der_raw;
pub mod dsse;
pub mod gc;
pub mod keys;
#[cfg(feature = "sigstore")]
pub mod remote;
pub mod signer;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod timestamp;
pub mod trust_root;
pub mod tuf;
pub mod verify;
pub mod x509;

pub use bundle::{Bundle, Material, Object};
pub use dsse::{Envelope, PAYLOAD_TYPE};
pub use keys::{KeyError, SigningKey};
pub use signer::{NoTimestamps, SignError, Signer, Timestamper};
pub use trust_root::TrustedRoot;
pub use verify::{
    Expected, Location, Rejection, SignedBy, Verified, certified_identity, signed_by_one_of,
    verify, verify_with,
};

use base64::Engine as _;

use hushrun_core::Digest;
use hushrun_core::event::Event;

/// Signs `event`, a statement about source digest `digest`, and returns a plain DSSE envelope:
/// the form of events before M10, which readers still accept.
pub fn sign_event(event: &Event, digest: &Digest, key: &SigningKey) -> Vec<u8> {
    let statement = event.statement(digest);
    let payload = serde_json::to_vec(&statement).expect("statements serialize");
    dsse::sign(&payload, key).to_bytes()
}

/// An event signed into a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedEvent {
    /// The bytes stored in the ledger.
    pub bytes: Vec<u8>,
    /// Why the bundle has no timestamp, when it has none.
    pub untimestamped: Option<String>,
}

/// Signs `event` with `signer` into a Sigstore bundle (E-1), with an RFC 3161 timestamp over the
/// signature from `timestamps` (E-10). A keyless signature needs the timestamp (E-11). A key's
/// goes out without one when no authority answers, and then counts only while its machine counts
/// (T-15).
pub fn sign_event_bundle(
    event: &Event,
    digest: &Digest,
    signer: &dyn Signer,
    timestamps: &dyn Timestamper,
) -> Result<SignedEvent, SignError> {
    let statement = event.statement(digest);
    let payload = serde_json::to_vec(&statement).expect("statements serialize");
    sign_bundle(PAYLOAD_TYPE, &payload, signer, timestamps)
}

/// Signs `payload`, of `payload_type`, into a bundle as [`sign_event_bundle`] signs an event:
/// \[ADR 0026\] GC statements too (L-9), so that a keyless identity can sign them.
pub fn sign_bundle(
    payload_type: &str,
    payload: &[u8],
    signer: &dyn Signer,
    timestamps: &dyn Timestamper,
) -> Result<SignedEvent, SignError> {
    let certificate = signer.certificate()?;
    let material = match (certificate, signer.public_key()) {
        (Some(der), _) => Material::Certificate(der),
        (None, Some(key)) => Material::Key(key.keyid()),
        (None, None) => {
            return Err(SignError::Config(format!(
                "{} has neither a key nor a certificate",
                signer.describe()
            )));
        }
    };
    let keyless = matches!(material, Material::Certificate(_));
    let keyid = match &material {
        Material::Key(keyid) => keyid.clone(),
        Material::Certificate(_) => String::new(),
    };
    let signature = signer.sign(&dsse::pae(payload_type, payload))?;
    let mut envelope = Envelope::unsigned(payload_type, payload);
    envelope.signatures.push(dsse::Signature {
        keyid,
        sig: base64::engine::general_purpose::STANDARD.encode(&signature),
    });
    let (stamps, untimestamped) = match timestamps.timestamp(&signature) {
        Ok(response) => (vec![response], None),
        Err(SignError::Config(why)) if keyless => {
            return Err(SignError::Config(format!(
                "keyless signing needs a timestamp authority: {why}"
            )));
        }
        Err(SignError::Unavailable(why)) if keyless => {
            return Err(SignError::Unavailable(format!(
                "a keyless signature needs a trusted timestamp, and none was made: {why}"
            )));
        }
        Err(why) => (Vec::new(), Some(why.to_string())),
    };
    Ok(SignedEvent {
        bytes: Bundle::new(envelope, material, &stamps).to_bytes(),
        untimestamped,
    })
}
