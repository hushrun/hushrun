//! What signs (E-1, E-2, T-12): a key file or a keyless certificate, and the timestamp
//! authorities that timestamp each signature (E-10). Network signers live behind the
//! `sigstore` feature; these traits are all verification-free code needs.

use hushrun_core::signers::PublicKey;

/// Why signing failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignError {
    /// The signer couldn't be reached, or refused: the CI's ID token, Fulcio, or every
    /// timestamp authority (keyless signing). Commands exit 9.
    #[error("{0}")]
    Unavailable(String),
    /// The signer is configured wrong. Commands exit 2.
    #[error("{0}")]
    Config(String),
}

/// Signs DSSE pre-authentication encodings (E-1).
pub trait Signer: Send + Sync {
    /// The key that verifies this signer's signatures; `None` for a keyless signer, whose
    /// certificate says who it is.
    fn public_key(&self) -> Option<PublicKey>;

    /// The DER certificate the next signature is made under, fetched or renewed when needed
    /// (keyless signers only). Callers ask for it before each signature.
    fn certificate(&self) -> Result<Option<Vec<u8>>, SignError> {
        Ok(None)
    }

    /// A signature over `message`, in the form E-2 gives for the key's algorithm.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SignError>;

    /// What signs, for messages, such as `key sha256:…`.
    fn describe(&self) -> String;
}

/// RFC 3161 timestamps over signatures (E-10).
pub trait Timestamper: Send + Sync {
    /// The DER `TimeStampResp` of the first authority that answers with a timestamp over
    /// `signature` that verifies; the error says why none did.
    fn timestamp(&self, signature: &[u8]) -> Result<Vec<u8>, SignError>;
}

/// No timestamp authority (`HUSHRUN_TSA=none`): events go out without a timestamp.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoTimestamps;

impl Timestamper for NoTimestamps {
    fn timestamp(&self, _signature: &[u8]) -> Result<Vec<u8>, SignError> {
        Err(SignError::Config(
            "no timestamp authority is configured (HUSHRUN_TSA=none)".into(),
        ))
    }
}
