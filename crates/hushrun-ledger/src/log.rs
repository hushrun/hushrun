//! \[M18\] What a ledger that keeps a transparency log offers its readers and writers (L-11):
//! receipts for creates, signed listings, checkpoints and consistency proofs, and its leaves for
//! monitors. The HTTP ledger is the one backend that keeps a log; the others have none.

use hushrun_tlog::note::{NoteError, Verifier};
use hushrun_tlog::{Entry, Hash, SignedCheckpoint};

use crate::LedgerError;

/// The log key that hushrun.yaml names (T-9), so that the ledger never
/// vouches for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogKey {
    /// The log's origin, also its key's name in signed notes.
    pub origin: String,
    /// The Ed25519 public key.
    pub key: [u8; 32],
}

impl LogKey {
    /// The note verifier of this key, named after the origin.
    pub fn verifier(&self) -> Result<Verifier, NoteError> {
        Verifier::new(&self.origin, self.key)
    }
}

/// A listing page with the statement the ledger signed for it, as `plan --start` records it in
/// its `started` events (E-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedListing {
    /// The prefix the page was listed under.
    pub prefix: String,
    /// The signed statement (L-11), a note.
    pub statement: String,
    /// The page's keys, as the ledger answered them.
    pub keys: Vec<String>,
}

/// One leaf of the log, as monitors read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafRecord {
    pub index: u64,
    pub entry: Entry,
    /// For a signed tombstone: the GC statement (L-9), whose SHA-256 the entry records.
    pub gc_statement: Option<Vec<u8>>,
}

/// A ledger's transparency log (L-11), as a command sees it: it verifies what the ledger
/// answers against the log key that hushrun.yaml names, and remembers the newest checkpoint it
/// verified.
pub trait Log: Send + Sync {
    /// Sets the log key that hushrun.yaml names (T-9). With a key, every receipt, listing and
    /// checkpoint must verify from now on, and those received before are checked now; without
    /// one, nothing is checked, and a ledger that keeps a log gets a warning, once.
    fn configure(&self, key: Option<LogKey>) -> Result<(), LedgerError>;

    /// Whether a log key is set, so that answers are verified.
    fn verifying(&self) -> bool;

    /// Whether hushrun.yaml was read for this command, so that [`Log::configure`] was called,
    /// with a log key or without one. Writers check it: an event written before would go
    /// unchecked (L-11).
    fn configured(&self) -> bool;

    /// The newest checkpoint this command verified, which every event it writes carries
    /// (E-4). Without one yet, the ledger's latest, verified. `None` when nothing is verified.
    fn checkpoint(&self) -> Result<Option<SignedCheckpoint>, LedgerError>;

    /// The ledger's latest checkpoint, verified, and consistent with the newest one this
    /// command saw.
    fn latest(&self) -> Result<SignedCheckpoint, LedgerError>;

    /// Checks a checkpoint found in an event (E-4) against this command's view: both must be
    /// one history, which a consistency proof shows. A fork is `Unavailable` and names both
    /// signed checkpoints. A checkpoint the log key didn't sign proves nothing, and is skipped.
    fn check_checkpoint(&self, note: &str, source: &str) -> Result<(), LedgerError>;

    /// The listings received since the last call, oldest first.
    fn take_listings(&self) -> Vec<RecordedListing>;

    /// Proves that `bytes` are what the log holds for the live object `key`: an inclusion
    /// proof of its leaf, against a checkpoint of this command's history. Readers ask it for
    /// an event they reject, so that a ledger can't hide an event by serving other bytes.
    fn prove_object(&self, key: &str, bytes: &[u8]) -> Result<(), LedgerError>;

    /// Leaves `[start, start + limit)` of the log, as far as the ledger's latest checkpoint.
    fn leaves(&self, start: u64, limit: u64) -> Result<Vec<LeafRecord>, LedgerError>;

    /// A consistency proof from the tree of `old` leaves to the tree of `new` leaves.
    fn consistency_proof(&self, old: u64, new: u64) -> Result<Vec<Hash>, LedgerError>;

    /// Deletes `key` with an administrator's signed GC statement (L-9), which the tombstone's
    /// leaf records. A 404 is success, as for [`Ledger::delete`](crate::Ledger::delete).
    fn delete_signed(&self, key: &str, statement: &[u8]) -> Result<(), LedgerError>;
}

/// \[M19\] The wire form of a listing page's proof (`proof`, L-12; ledger-http-v1 §11): hashes in
/// standard base64, as the server writes it.
pub mod wire {
    use hushrun_tlog::Hash;
    use hushrun_tlog::map::{ListingProof, MapEntry, MapProof, PageProof};
    use serde::{Deserialize, Serialize};

    /// A listing page's proof.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ListingProofWire {
        /// The map's root, which the checkpoint's last leaf records.
        pub map_root: String,
        /// The inclusion proof of that leaf, the tree's last, in the checkpoint's tree.
        pub map_leaf: Vec<String>,
        pub map: MapProofWire,
        /// The page's place in the prefix's key tree; absent when the map holds no keys for
        /// the prefix.
        #[serde(default)]
        pub page: Option<PageProofWire>,
    }

    /// The map's path to a prefix.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct MapProofWire {
        /// Siblings, deepest first.
        pub path: Vec<String>,
        /// The entry where the path ends, if any.
        #[serde(default)]
        pub leaf: Option<MapEntryWire>,
    }

    /// A map entry.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct MapEntryWire {
        pub prefix: String,
        pub count: u64,
        pub root: String,
    }

    /// A page's range proof in its key tree.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct PageProofWire {
        pub index: u64,
        #[serde(default)]
        pub previous: Option<String>,
        pub left: Vec<String>,
        pub right: Vec<String>,
    }

    fn encode(hashes: &[Hash]) -> Vec<String> {
        hashes.iter().map(Hash::base64).collect()
    }

    fn decode(what: &str, hashes: &[String]) -> Result<Vec<Hash>, String> {
        hashes
            .iter()
            .map(|h| Hash::from_base64(h).ok_or_else(|| format!("{what}: a hash isn't base64")))
            .collect()
    }

    fn hash(what: &str, text: &str) -> Result<Hash, String> {
        Hash::from_base64(text).ok_or_else(|| format!("{what} isn't a base64 hash"))
    }

    impl From<&ListingProof> for ListingProofWire {
        fn from(proof: &ListingProof) -> Self {
            ListingProofWire {
                map_root: proof.map_root.base64(),
                map_leaf: encode(&proof.map_leaf),
                map: MapProofWire {
                    path: encode(&proof.map.path),
                    leaf: proof.map.leaf.as_ref().map(|entry| MapEntryWire {
                        prefix: entry.prefix.clone(),
                        count: entry.count,
                        root: entry.root.base64(),
                    }),
                },
                page: proof.page.as_ref().map(|page| PageProofWire {
                    index: page.index,
                    previous: page.previous.clone(),
                    left: encode(&page.left),
                    right: encode(&page.right),
                }),
            }
        }
    }

    impl ListingProofWire {
        /// The proof, from its wire form.
        pub fn decode(&self) -> Result<ListingProof, String> {
            Ok(ListingProof {
                map_root: hash("map_root", &self.map_root)?,
                map_leaf: decode("map_leaf", &self.map_leaf)?,
                map: MapProof {
                    path: decode("map.path", &self.map.path)?,
                    leaf: self
                        .map
                        .leaf
                        .as_ref()
                        .map(|leaf| {
                            Ok::<_, String>(MapEntry {
                                prefix: leaf.prefix.clone(),
                                count: leaf.count,
                                root: hash("map.leaf.root", &leaf.root)?,
                            })
                        })
                        .transpose()?,
                },
                page: self
                    .page
                    .as_ref()
                    .map(|page| {
                        Ok::<_, String>(PageProof {
                            index: page.index,
                            previous: page.previous.clone(),
                            left: decode("page.left", &page.left)?,
                            right: decode("page.right", &page.right)?,
                        })
                    })
                    .transpose()?,
            })
        }
    }
}
