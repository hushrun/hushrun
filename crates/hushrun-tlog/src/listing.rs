//! The statement a listing page carries (L-11): the ledger's signature, with its log key, over
//! the prefix, the page's bounds, the checkpoint the page was read at, and the hash of its keys.
//!
//! ```text
//! hushrun-listing/v1
//! origin hushrun.example/l/acme/back
//! size 1234
//! root XcnaeacGWamtVZy3Ad7ZoqudgjqtL0lgz+Nw7/RgQyg=
//! prefix v1/events/api/5f0c…e1/
//! after
//! next
//! keys 7 9a1b…
//! ```
//!
//! It says: the live keys under `prefix`, after `after` (exclusive, none when empty), up to
//! `next` (inclusive, the page's end, none when empty: the rest of the prefix), in the tree of
//! `size` leaves whose root is `root`, are exactly `count` keys whose hash is `keys`. The hash
//! is SHA-256 over each key followed by a newline, in byte order. A monitor replays the log to
//! that size and recomputes the page: a key the log held there and the page left out is the
//! ledger's own signed statement, contradicted by its own log.

use std::fmt;

use ring::digest::{Context, SHA256};

use crate::checkpoint::{Checkpoint, parse_size};
use crate::hash::Hash;
use crate::note::{self, NoteError, Signer, Verifier};

/// The first line of every statement.
pub const LISTING_HEADER: &str = "hushrun-listing/v1";

/// Why a statement doesn't parse or verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListingError {
    Malformed(String),
    Note(NoteError),
    WrongOrigin { expected: String, got: String },
}

impl fmt::Display for ListingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListingError::Malformed(why) => write!(f, "not a listing statement: {why}"),
            ListingError::Note(err) => write!(f, "the listing statement's signature: {err}"),
            ListingError::WrongOrigin { expected, got } => {
                write!(
                    f,
                    "the listing statement is of log {got:?}, not {expected:?}"
                )
            }
        }
    }
}

impl std::error::Error for ListingError {}

impl From<NoteError> for ListingError {
    fn from(err: NoteError) -> Self {
        ListingError::Note(err)
    }
}

/// SHA-256 over each key and a newline, in the order given (byte order, as listings return
/// them).
pub fn keys_hash<S: AsRef<str>>(keys: &[S]) -> Hash {
    let mut context = Context::new(&SHA256);
    for key in keys {
        context.update(key.as_ref().as_bytes());
        context.update(b"\n");
    }
    Hash::from_slice(context.finish().as_ref()).expect("SHA-256 is 32 bytes")
}

/// What one listing page says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub origin: String,
    pub size: u64,
    pub root: Hash,
    pub prefix: String,
    /// The exclusive lower bound the page was asked for.
    pub after: Option<String>,
    /// The page's inclusive upper bound when more keys follow; `None` when the page reaches
    /// the end of the prefix.
    pub next: Option<String>,
    pub count: u64,
    pub keys: Hash,
}

impl Listing {
    /// The statement for a page of `keys`, read at `checkpoint`.
    pub fn new<S: AsRef<str>>(
        checkpoint: &Checkpoint,
        prefix: &str,
        after: Option<&str>,
        next: Option<&str>,
        keys: &[S],
    ) -> Result<Listing, ListingError> {
        let listing = Listing {
            origin: checkpoint.origin.clone(),
            size: checkpoint.size,
            root: checkpoint.root,
            prefix: prefix.to_string(),
            after: after.map(str::to_string),
            next: next.map(str::to_string),
            count: keys.len() as u64,
            keys: keys_hash(keys),
        };
        listing.check()?;
        Ok(listing)
    }

    fn check(&self) -> Result<(), ListingError> {
        let malformed = |why: String| Err(ListingError::Malformed(why));
        if !note::valid_key_name(&self.origin) {
            return malformed(format!("{:?} can't be an origin", self.origin));
        }
        if !crate::valid_prefix(&self.prefix) {
            return malformed(format!("{:?} is not a listing prefix", self.prefix));
        }
        for bound in [&self.after, &self.next].into_iter().flatten() {
            if !crate::valid_name(bound) {
                return malformed(format!("{bound:?} is not a key"));
            }
        }
        Ok(())
    }

    /// The checkpoint the page was read at, without extension lines.
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint::new(self.origin.clone(), self.size, self.root)
    }

    /// Whether `key` falls in the page's range: under the prefix, after `after`, and not past
    /// `next`.
    pub fn covers(&self, key: &str) -> bool {
        key.starts_with(self.prefix.as_str())
            && self.after.as_deref().is_none_or(|after| key > after)
            && self.next.as_deref().is_none_or(|next| key <= next)
    }

    /// Whether `keys` are the keys this statement is about.
    pub fn matches<S: AsRef<str>>(&self, keys: &[S]) -> bool {
        self.count == keys.len() as u64 && self.keys == keys_hash(keys)
    }

    /// The note text.
    pub fn text(&self) -> String {
        format!(
            "{LISTING_HEADER}\norigin {}\nsize {}\nroot {}\nprefix {}\nafter {}\nnext {}\nkeys {} {}\n",
            self.origin,
            self.size,
            self.root.base64(),
            self.prefix,
            self.after.as_deref().unwrap_or(""),
            self.next.as_deref().unwrap_or(""),
            self.count,
            self.keys.hex()
        )
    }

    /// Parses the note text of a statement.
    pub fn parse(text: &str) -> Result<Listing, ListingError> {
        let malformed = |why: &str| ListingError::Malformed(why.to_string());
        let body = text
            .strip_suffix('\n')
            .ok_or_else(|| malformed("it doesn't end in a newline"))?;
        let lines: Vec<&str> = body.split('\n').collect();
        let [header, origin, size, root, prefix, after, next, keys] = lines.as_slice() else {
            return Err(malformed("it doesn't have eight lines"));
        };
        if *header != LISTING_HEADER {
            return Err(malformed("the first line isn't hushrun-listing/v1"));
        }
        let field = |line: &'_ str, label: &str| -> Result<String, ListingError> {
            line.strip_prefix(label)
                .and_then(|rest| rest.strip_prefix(' '))
                .map(str::to_string)
                .ok_or_else(|| ListingError::Malformed(format!("no `{label}` line")))
        };
        let optional = |value: String| (!value.is_empty()).then_some(value);
        let size = parse_size(&field(size, "size")?)
            .ok_or_else(|| malformed("the size is not a number"))?;
        let root = Hash::from_base64(&field(root, "root")?)
            .ok_or_else(|| malformed("the root is not a base64 hash"))?;
        let keys_line = field(keys, "keys")?;
        let (count, hash) = keys_line
            .split_once(' ')
            .ok_or_else(|| malformed("the keys line is not `keys <count> <hex>`"))?;
        let count = parse_size(count).ok_or_else(|| malformed("the key count is not a number"))?;
        let keys = Hash::from_hex(hash).ok_or_else(|| malformed("the keys hash is not hex"))?;
        let listing = Listing {
            origin: field(origin, "origin")?,
            size,
            root,
            prefix: field(prefix, "prefix")?,
            after: optional(field(after, "after")?),
            next: optional(field(next, "next")?),
            count,
            keys,
        };
        listing.check()?;
        if listing.text() != text {
            return Err(malformed("it isn't in the canonical form"));
        }
        Ok(listing)
    }
}

/// A statement and the signed note it came in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedListing {
    pub listing: Listing,
    pub note: String,
}

impl SignedListing {
    /// Signs `listing` with the log key, which must be named after its origin.
    pub fn sign(listing: Listing, signer: &Signer) -> Result<SignedListing, ListingError> {
        if signer.name() != listing.origin {
            return Err(ListingError::WrongOrigin {
                expected: signer.name().to_string(),
                got: listing.origin,
            });
        }
        let note = signer.sign(&listing.text())?;
        Ok(SignedListing { listing, note })
    }

    /// Opens a statement signed by the log key `verifier`, named after its origin.
    pub fn open(note: &str, verifier: &Verifier) -> Result<SignedListing, ListingError> {
        let opened = note::open(note.as_bytes(), &[verifier])?;
        let listing = Listing::parse(&opened.text)?;
        if listing.origin != verifier.name() {
            return Err(ListingError::WrongOrigin {
                expected: verifier.name().to_string(),
                got: listing.origin,
            });
        }
        Ok(SignedListing {
            listing,
            note: note.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "hushrun.example/l/acme/back";

    fn signer() -> Signer {
        Signer::new(ORIGIN, [3; 32]).unwrap()
    }

    fn checkpoint() -> Checkpoint {
        Checkpoint::new(ORIGIN, 12, Hash::leaf(b"root"))
    }

    // covers: L-11
    #[test]
    fn statements_round_trip() {
        let keys = [
            "v1/events/a/x/1.check._.json",
            "v1/events/a/x/2.started.s.json",
        ];
        let listing = Listing::new(&checkpoint(), "v1/events/a/x/", None, None, &keys).unwrap();
        assert_eq!(
            listing.text(),
            format!(
                "hushrun-listing/v1\norigin {ORIGIN}\nsize 12\nroot {}\nprefix v1/events/a/x/\nafter \nnext \nkeys 2 {}\n",
                Hash::leaf(b"root").base64(),
                keys_hash(&keys).hex()
            )
        );
        let signed = SignedListing::sign(listing.clone(), &signer()).unwrap();
        let opened = SignedListing::open(&signed.note, &signer().verifier()).unwrap();
        assert_eq!(opened.listing, listing);
        assert!(listing.matches(&keys));
        assert!(!listing.matches(&keys[..1]));
        assert_eq!(listing.checkpoint(), checkpoint());
        let paged = Listing::new(
            &checkpoint(),
            "",
            Some("v1/a"),
            Some("v1/c"),
            &["v1/b", "v1/c"],
        )
        .unwrap();
        assert_eq!(Listing::parse(&paged.text()).unwrap(), paged);
    }

    #[test]
    fn keys_hash_is_newline_terminated() {
        assert_eq!(keys_hash::<&str>(&[]), Hash::empty());
        assert_eq!(keys_hash(&["a", "b"]), Hash::of(b"a\nb\n"));
        assert_ne!(keys_hash(&["a", "b"]), keys_hash(&["ab"]));
    }

    #[test]
    fn ranges() {
        let first =
            Listing::new(&checkpoint(), "v1/e/", None, Some("v1/e/m"), &["v1/e/m"]).unwrap();
        assert!(first.covers("v1/e/a"));
        assert!(first.covers("v1/e/m"));
        assert!(!first.covers("v1/e/n"));
        assert!(!first.covers("v1/d"));
        let rest = Listing::new(&checkpoint(), "v1/e/", Some("v1/e/m"), None, &["v1/e/z"]).unwrap();
        assert!(!rest.covers("v1/e/m"));
        assert!(rest.covers("v1/e/n"));
        assert!(rest.covers("v1/e/zzz"));
        assert!(!rest.covers("v1/f"));
    }

    #[test]
    fn statements_parse_strictly() {
        let good = Listing::new(&checkpoint(), "p/", None, None, &["p/a"])
            .unwrap()
            .text();
        for bad in [
            good.replace("hushrun-listing/v1", "hushrun-listing/v2"),
            good.replace("size 12", "size 012"),
            good.replace("prefix p/", "prefix p /"),
            good.replace("after \n", "after a b\n"),
            good.replace("keys 1 ", "keys 01 "),
            good.replace("\nnext \n", "\n"),
            good.replace("origin ", "origin  "),
            format!("{good}extra\n"),
            good.trim_end().to_string(),
        ] {
            assert!(Listing::parse(&bad).is_err(), "{bad:?}");
        }
        assert!(Listing::new(&checkpoint(), "a b", None, None, &["x"]).is_err());
        assert!(Listing::new(&checkpoint(), "", Some("a\nb"), None, &["x"]).is_err());
    }

    #[test]
    fn only_the_log_key_counts() {
        let listing = Listing::new(&checkpoint(), "", None, None, &["a"]).unwrap();
        let impostor = Signer::new(ORIGIN, [4; 32]).unwrap();
        let forged = SignedListing::sign(listing.clone(), &impostor).unwrap();
        assert!(SignedListing::open(&forged.note, &signer().verifier()).is_err());
        // A checkpoint note is not a listing statement, though the same key signs both.
        let checkpoint_note = signer().sign(&checkpoint().text()).unwrap();
        assert!(SignedListing::open(&checkpoint_note, &signer().verifier()).is_err());
        let statement = SignedListing::sign(listing, &signer()).unwrap();
        assert!(crate::SignedCheckpoint::open(&statement.note, &signer().verifier()).is_err());
    }
}
