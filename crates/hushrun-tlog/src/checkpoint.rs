//! C2SP `tlog-checkpoint` (<https://c2sp.org/tlog-checkpoint>): a log's origin, size and root,
//! as the text of a note the log key signs.
//!
//! ```text
//! hushrun.example/l/acme/back
//! 8
//! XcnaeacGWamtVZy3Ad7ZoqudgjqtL0lgz+Nw7/RgQyg=
//! ```
//!
//! The origin is the first line, the tree size the second (decimal, no leading zeros), the
//! root hash the third (standard base64). Extension lines may follow; hushrun writes none and
//! keeps any it reads. A hushrun log's key is named after its origin, so a checkpoint counts
//! only when the key named in signed trust signs it under that name.

use std::fmt;

use crate::hash::Hash;
use crate::note::{self, NoteError, Signer, Verifier};

/// Why a checkpoint doesn't parse or verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    /// The text isn't a checkpoint.
    Malformed(String),
    /// The note doesn't verify with the log key.
    Note(NoteError),
    /// The checkpoint names another log than the key's.
    WrongOrigin { expected: String, got: String },
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointError::Malformed(why) => write!(f, "not a checkpoint: {why}"),
            CheckpointError::Note(err) => write!(f, "the checkpoint's signature: {err}"),
            CheckpointError::WrongOrigin { expected, got } => {
                write!(f, "the checkpoint is of log {got:?}, not {expected:?}")
            }
        }
    }
}

impl std::error::Error for CheckpointError {}

impl From<NoteError> for CheckpointError {
    fn from(err: NoteError) -> Self {
        CheckpointError::Note(err)
    }
}

/// A checkpoint: the state of a log at one size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub origin: String,
    pub size: u64,
    pub root: Hash,
    pub extensions: Vec<String>,
}

impl Checkpoint {
    pub fn new(origin: impl Into<String>, size: u64, root: Hash) -> Checkpoint {
        Checkpoint {
            origin: origin.into(),
            size,
            root,
            extensions: Vec::new(),
        }
    }

    /// The note text.
    pub fn text(&self) -> String {
        let mut text = format!("{}\n{}\n{}\n", self.origin, self.size, self.root.base64());
        for line in &self.extensions {
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    /// Parses the note text of a checkpoint.
    pub fn parse(text: &str) -> Result<Checkpoint, CheckpointError> {
        let malformed = |why: &str| CheckpointError::Malformed(why.to_string());
        let body = text
            .strip_suffix('\n')
            .ok_or_else(|| malformed("it doesn't end in a newline"))?;
        let mut lines = body.split('\n');
        let origin = lines.next().unwrap_or_default();
        if !note::valid_key_name(origin) {
            return Err(malformed(
                "the origin line is empty, or has a space or a `+`",
            ));
        }
        let size = lines.next().ok_or_else(|| malformed("no tree size"))?;
        let size = parse_size(size).ok_or_else(|| malformed("the tree size is not a number"))?;
        let root = lines.next().ok_or_else(|| malformed("no root hash"))?;
        let root =
            Hash::from_base64(root).ok_or_else(|| malformed("the root is not a base64 hash"))?;
        let extensions: Vec<String> = lines.map(str::to_string).collect();
        if extensions.iter().any(String::is_empty) {
            return Err(malformed("an extension line is empty"));
        }
        Ok(Checkpoint {
            origin: origin.to_string(),
            size,
            root,
            extensions,
        })
    }
}

/// Decimal without leading zeros (`0` itself is fine).
pub(crate) fn parse_size(text: &str) -> Option<u64> {
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    text.parse().ok()
}

/// A checkpoint and the signed note it came in, which is what proves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedCheckpoint {
    pub checkpoint: Checkpoint,
    pub note: String,
}

impl SignedCheckpoint {
    /// Signs `checkpoint` with the log key, which must be named after its origin.
    pub fn sign(
        checkpoint: Checkpoint,
        signer: &Signer,
    ) -> Result<SignedCheckpoint, CheckpointError> {
        if signer.name() != checkpoint.origin {
            return Err(CheckpointError::WrongOrigin {
                expected: signer.name().to_string(),
                got: checkpoint.origin,
            });
        }
        let note = signer.sign(&checkpoint.text())?;
        Ok(SignedCheckpoint { checkpoint, note })
    }

    /// Opens a checkpoint note signed by the log key `verifier`, whose name must be the
    /// checkpoint's origin.
    pub fn open(note: &str, verifier: &Verifier) -> Result<SignedCheckpoint, CheckpointError> {
        let opened = note::open(note.as_bytes(), &[verifier])?;
        let checkpoint = Checkpoint::parse(&opened.text)?;
        if checkpoint.origin != verifier.name() {
            return Err(CheckpointError::WrongOrigin {
                expected: verifier.name().to_string(),
                got: checkpoint.origin,
            });
        }
        Ok(SignedCheckpoint {
            checkpoint,
            note: note.to_string(),
        })
    }

    pub fn size(&self) -> u64 {
        self.checkpoint.size
    }

    pub fn root(&self) -> &Hash {
        &self.checkpoint.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> Signer {
        Signer::new(
            "hushrun.example/l/acme/back",
            core::array::from_fn(|i| i as u8),
        )
        .unwrap()
    }

    // covers: L-11
    #[test]
    fn checkpoints_round_trip() {
        let root = Hash::leaf(b"x");
        let checkpoint = Checkpoint::new("hushrun.example/l/acme/back", 8, root);
        let signed = SignedCheckpoint::sign(checkpoint.clone(), &signer()).unwrap();
        let opened = SignedCheckpoint::open(&signed.note, &signer().verifier()).unwrap();
        assert_eq!(opened.checkpoint, checkpoint);
        assert_eq!(opened.size(), 8);
        assert_eq!(Checkpoint::parse(&checkpoint.text()).unwrap(), checkpoint);
        let with_extension = format!("{}ext one\n", checkpoint.text());
        let parsed = Checkpoint::parse(&with_extension).unwrap();
        assert_eq!(parsed.extensions, ["ext one"]);
        assert_eq!(parsed.text(), with_extension);
    }

    #[test]
    fn checkpoints_parse_strictly() {
        let root = Hash::empty().base64();
        for bad in [
            String::new(),
            format!("origin\n0\n{root}"),
            format!("\n0\n{root}\n"),
            format!("two words\n0\n{root}\n"),
            format!("a+b\n0\n{root}\n"),
            format!("origin\n00\n{root}\n"),
            format!("origin\n01\n{root}\n"),
            format!("origin\n-1\n{root}\n"),
            format!("origin\n18446744073709551616\n{root}\n"),
            "origin\n0\nAAAA\n".to_string(),
            "origin\n0\n".to_string(),
            format!("origin\n0\n{root}\n\n"),
        ] {
            assert!(Checkpoint::parse(&bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            Checkpoint::parse(&format!("origin\n0\n{root}\n"))
                .unwrap()
                .size,
            0
        );
    }

    #[test]
    fn only_the_log_key_under_its_origin_counts() {
        let root = Hash::leaf(b"x");
        // Signed by the right key, but for another origin.
        let other_origin = Checkpoint::new("elsewhere.example/log", 1, root);
        let renamed =
            Signer::new("elsewhere.example/log", core::array::from_fn(|i| i as u8)).unwrap();
        let note = renamed.sign(&other_origin.text()).unwrap();
        assert!(SignedCheckpoint::open(&note, &signer().verifier()).is_err());
        assert!(SignedCheckpoint::sign(other_origin, &signer()).is_err());
        // Signed by another key under the right name.
        let impostor = Signer::new("hushrun.example/l/acme/back", [42; 32]).unwrap();
        let checkpoint = Checkpoint::new("hushrun.example/l/acme/back", 1, root);
        let forged = SignedCheckpoint::sign(checkpoint, &impostor).unwrap();
        assert!(matches!(
            SignedCheckpoint::open(&forged.note, &signer().verifier()),
            Err(CheckpointError::Note(NoteError::Unverified { .. }))
        ));
    }
}
