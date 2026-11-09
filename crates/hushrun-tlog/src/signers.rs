//! \[ADR 0031\] The statement a repository's signers answer carries (L-13): the server's signature,
//! with its log key, over the hash of the signers document it answered, so that a saved answer
//! can be checked later, offline, against the log key the repository pins (T-9).
//!
//! ```text
//! hushrun-signers/v1
//! origin hushrun.example/l/acme/back
//! document 9a1b…
//! ```
//!
//! It says: the server that keeps the log `origin` answered, as the signers whose events count for
//! its repository, the document whose JCS form hashes (SHA-256) to `document`.

use std::fmt;

use crate::hash::Hash;
use crate::note::{self, NoteError, Signer, Verifier};

/// The first line of every statement.
pub const SIGNERS_HEADER: &str = "hushrun-signers/v1";

/// Why a statement doesn't parse or verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignersError {
    Malformed(String),
    Note(NoteError),
    WrongOrigin { expected: String, got: String },
}

impl fmt::Display for SignersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignersError::Malformed(why) => write!(f, "not a signers statement: {why}"),
            SignersError::Note(err) => write!(f, "the signers statement's signature: {err}"),
            SignersError::WrongOrigin { expected, got } => {
                write!(
                    f,
                    "the signers statement is of log {got:?}, not {expected:?}"
                )
            }
        }
    }
}

impl std::error::Error for SignersError {}

impl From<NoteError> for SignersError {
    fn from(err: NoteError) -> Self {
        SignersError::Note(err)
    }
}

/// What a signers statement says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignersStatement {
    pub origin: String,
    /// SHA-256 of the document's JCS form.
    pub document: Hash,
}

impl SignersStatement {
    /// The note text.
    pub fn text(&self) -> String {
        format!(
            "{SIGNERS_HEADER}\norigin {}\ndocument {}\n",
            self.origin,
            self.document.hex()
        )
    }

    /// Parses the note text of a statement.
    pub fn parse(text: &str) -> Result<SignersStatement, SignersError> {
        let malformed = |why: &str| SignersError::Malformed(why.to_string());
        let body = text
            .strip_suffix('\n')
            .ok_or_else(|| malformed("it doesn't end in a newline"))?;
        let lines: Vec<&str> = body.split('\n').collect();
        let [header, origin, document] = lines.as_slice() else {
            return Err(malformed("it doesn't have three lines"));
        };
        if *header != SIGNERS_HEADER {
            return Err(malformed("the first line isn't hushrun-signers/v1"));
        }
        let origin = origin
            .strip_prefix("origin ")
            .ok_or_else(|| malformed("no `origin` line"))?;
        if !note::valid_key_name(origin) {
            return Err(malformed("the origin can't name a log"));
        }
        let document = document
            .strip_prefix("document ")
            .and_then(Hash::from_hex)
            .ok_or_else(|| malformed("no `document <hex>` line"))?;
        let statement = SignersStatement {
            origin: origin.to_string(),
            document,
        };
        if statement.text() != text {
            return Err(malformed("it isn't in the canonical form"));
        }
        Ok(statement)
    }

    /// Signs the statement with the log key, which must be named after its origin.
    pub fn sign(&self, signer: &Signer) -> Result<String, SignersError> {
        if signer.name() != self.origin {
            return Err(SignersError::WrongOrigin {
                expected: signer.name().to_string(),
                got: self.origin.clone(),
            });
        }
        Ok(signer.sign(&self.text())?)
    }

    /// Opens a statement signed by the log key `verifier`, named after its origin.
    pub fn open(note: &str, verifier: &Verifier) -> Result<SignersStatement, SignersError> {
        let opened = note::open(note.as_bytes(), &[verifier])?;
        let statement = SignersStatement::parse(&opened.text)?;
        if statement.origin != verifier.name() {
            return Err(SignersError::WrongOrigin {
                expected: verifier.name().to_string(),
                got: statement.origin,
            });
        }
        Ok(statement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "hushrun.example/l/acme/back";

    // covers: L-13
    #[test]
    fn signs_and_opens() {
        let signer = Signer::new(ORIGIN, [5; 32]).unwrap();
        let statement = SignersStatement {
            origin: ORIGIN.into(),
            document: Hash::leaf(b"document"),
        };
        let note = statement.sign(&signer).unwrap();
        assert_eq!(
            SignersStatement::open(&note, &signer.verifier()).unwrap(),
            statement
        );
        let other = Signer::new(ORIGIN, [6; 32]).unwrap();
        assert!(SignersStatement::open(&note, &other.verifier()).is_err());
        let elsewhere = Signer::new("hushrun.example/l/acme/front", [5; 32]).unwrap();
        assert!(statement.sign(&elsewhere).is_err());
        assert!(SignersStatement::parse("hushrun-signers/v1\norigin a\n").is_err());
    }
}
