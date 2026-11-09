//! The transparency log of an HTTP ledger, as the client checks it (L-11): every create's
//! receipt, the statement every listing page carries, and checkpoints, against the log key that
//! hushrun.yaml names (T-9). All of a command's checkpoints must be one history: each new one is
//! joined to the newest one by a consistency proof, and a ledger that can't produce one has
//! shown two histories.
//!
//! \[M19\] A listing of a map prefix also carries a proof against the map its checkpoint commits
//! to (L-12). The client doesn't check it: it would show only that the ledger's map agrees with
//! the history the ledger showed this command, and the monitor checks every map against the log.

use std::collections::BTreeMap;
use std::sync::MutexGuard;

use base64::Engine as _;
use reqwest::{Method, StatusCode};
use serde::Deserialize;

use hushrun_tlog::checkpoint::SignedCheckpoint;
use hushrun_tlog::listing::SignedListing;
use hushrun_tlog::note::Verifier;
use hushrun_tlog::proof::{verify_consistency, verify_inclusion};
use hushrun_tlog::{Entry, EntryKind, Hash};

use super::{Call, HttpLedger, MAX_SMALL_BYTES, check_http_key};
use crate::LedgerError;
use crate::log::{LeafRecord, Log, LogKey, RecordedListing};

/// The most leaves one request asks for.
pub(super) const LEAVES_PER_PAGE: u64 = 1000;
/// A page of leaves: 1000 names of at most 1 KiB, and tombstones' GC statements in base64.
const MAX_LEAVES_BYTES: usize = 32 << 20;
/// A GC statement (L-9) is a small envelope.
const MAX_GC_STATEMENT: usize = 16 << 10;
/// The header that carries a GC statement with a delete.
pub(super) const GC_HEADER: &str = "x-hushrun-gc";

/// A create's receipt: the leaf's index, a checkpoint whose tree holds it, and the inclusion
/// proof.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Receipt {
    pub index: u64,
    pub checkpoint: String,
    #[serde(default)]
    pub proof: Vec<String>,
}

/// What a listing page said about the log, with the keys as the ledger answered them.
#[derive(Debug, Clone)]
pub(super) struct PageLog {
    pub after: Option<String>,
    pub next: Option<String>,
    pub keys: Vec<String>,
    pub checkpoint: Option<String>,
    pub statement: Option<String>,
}

impl PageLog {
    fn has_log(&self) -> bool {
        self.checkpoint.is_some() || self.statement.is_some()
    }
}

/// The log key, as far as the command knows it.
#[derive(Debug, Clone, Default)]
enum Key {
    /// hushrun.yaml isn't read yet: what arrives is kept, and checked once it is.
    #[default]
    Unset,
    /// hushrun.yaml names no log key: nothing is checked (migration, T-9).
    Absent,
    /// The log key.
    Verify(Verifier),
}

/// An answer that arrived before the key was set, to check once it is.
#[derive(Debug, Clone)]
enum Unchecked {
    /// A listing page, of a prefix.
    Page(String, PageLog),
    /// A create's receipt, for its entry.
    Receipt(Entry, Receipt),
}

/// The client's side of the log, for one command.
#[derive(Debug, Default)]
pub(super) struct State {
    key: Key,
    /// The newest checkpoint verified.
    newest: Option<SignedCheckpoint>,
    /// Sizes known to be one history with `newest`, and their roots.
    consistent: BTreeMap<u64, Hash>,
    /// What arrived before the key was set, in the order it arrived: checked in that order, as
    /// it would have been with the key set.
    unchecked: Vec<Unchecked>,
    /// Listings with their statements, until `take_listings`.
    listings: Vec<RecordedListing>,
    /// Whether an answer showed that the ledger keeps a log.
    serves: bool,
    warned: bool,
}

#[derive(Deserialize)]
struct ProofAnswer {
    proof: Vec<String>,
}

#[derive(Deserialize)]
struct ObjectProof {
    receipt: Receipt,
}

#[derive(Deserialize)]
struct LeavesAnswer {
    leaves: Vec<LeafAnswer>,
}

#[derive(Deserialize)]
struct LeafAnswer {
    index: u64,
    kind: String,
    name: String,
    sha256: String,
    #[serde(default)]
    gc: Option<String>,
    #[serde(default)]
    statement: Option<String>,
}

fn hashes(base: &str, what: &str, proof: &[String]) -> Result<Vec<Hash>, LedgerError> {
    proof
        .iter()
        .map(|h| {
            Hash::from_base64(h).ok_or_else(|| {
                LedgerError::Unavailable(format!("{base}: {what}: a proof hash isn't base64"))
            })
        })
        .collect()
}

/// A note, indented, for messages that quote it.
fn quote(note: &str) -> String {
    note.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl HttpLedger {
    fn state(&self) -> MutexGuard<'_, State> {
        self.log.lock().expect("log state")
    }

    fn unavailable(&self, why: impl std::fmt::Display) -> LedgerError {
        LedgerError::Unavailable(format!("{}: {why}", self.base))
    }

    /// The error that ends a command when the ledger showed two histories: it quotes both
    /// signed checkpoints, which together prove it.
    fn fork(
        &self,
        theirs: &SignedCheckpoint,
        source: &str,
        ours: &SignedCheckpoint,
        why: &str,
    ) -> LedgerError {
        self.unavailable(format!(
            "the server showed two histories: {why}. The checkpoint {source} carries\n{}\nand the checkpoint this command verified\n{}\nare signed by the server's log key; keep both: together they prove the fork",
            quote(&theirs.note),
            quote(&ours.note)
        ))
    }

    /// Joins `checkpoint` to this command's history: a consistency proof with the newest
    /// checkpoint, whichever is larger, and the larger becomes the newest. A listing
    /// (`listing: true`) must not be older than the newest: it would hide what was proven since.
    ///
    /// The proof is fetched without the state's lock, so another answer may move the newest
    /// meanwhile: then the checkpoint is joined to that one instead, so that the newest only
    /// ever grows, and every size kept is proven one history with it.
    fn adopt(
        &self,
        checkpoint: SignedCheckpoint,
        source: &str,
        listing: bool,
    ) -> Result<(), LedgerError> {
        loop {
            let newest = self.state().newest.clone();
            if self.adopt_against(&checkpoint, newest.as_ref(), source, listing)? {
                return Ok(());
            }
        }
    }

    /// [`Self::adopt`] against `newest`, the newest checkpoint as it was read: `false` when the
    /// newest changed before the result could be kept, and nothing was.
    fn adopt_against(
        &self,
        checkpoint: &SignedCheckpoint,
        newest: Option<&SignedCheckpoint>,
        source: &str,
        listing: bool,
    ) -> Result<bool, LedgerError> {
        // Whether the newest is still the one this was checked against.
        let unchanged = |state: &State| {
            state.newest.as_ref().map(|n| (n.size(), *n.root()))
                == newest.map(|n| (n.size(), *n.root()))
        };
        let Some(newest) = newest else {
            let mut state = self.state();
            if !unchanged(&state) {
                return Ok(false);
            }
            state
                .consistent
                .insert(checkpoint.size(), *checkpoint.root());
            state.newest = Some(checkpoint.clone());
            return Ok(true);
        };
        let known = self.state().consistent.get(&checkpoint.size()).copied();
        if known.is_some_and(|root| root != *checkpoint.root()) {
            return Err(self.fork(
                checkpoint,
                source,
                newest,
                "two trees of the same size with different roots",
            ));
        }
        // An older tree, even one of this history, would hide what was proven since, such as
        // this command's own writes.
        if checkpoint.size() < newest.size() && listing {
            return Err(self.unavailable(format!(
                "{source} was read at a tree of {} leaves, older than the {} leaves this command already verified; a listing must be at least as new",
                checkpoint.size(),
                newest.size()
            )));
        }
        if known.is_some() {
            return Ok(true);
        }
        let (old, new) = if checkpoint.size() < newest.size() {
            (checkpoint, newest)
        } else {
            (newest, checkpoint)
        };
        let proof = self.fetch_consistency(old.size(), new.size())?;
        if let Err(why) = verify_consistency(old.size(), new.size(), &proof, old.root(), new.root())
        {
            return Err(self.fork(
                checkpoint,
                source,
                newest,
                &format!("the consistency proof between them fails: {why}"),
            ));
        }
        let mut state = self.state();
        if !unchanged(&state) {
            return Ok(false);
        }
        state
            .consistent
            .insert(checkpoint.size(), *checkpoint.root());
        if checkpoint.size() > newest.size() {
            state.newest = Some(checkpoint.clone());
        }
        Ok(true)
    }

    /// Checks one listing page against the log key: its checkpoint, its statement, and that the
    /// statement is about this page.
    fn verify_page(
        &self,
        verifier: &Verifier,
        prefix: &str,
        page: &PageLog,
    ) -> Result<SignedCheckpoint, LedgerError> {
        let what = format!("the listing of {prefix:?}");
        let (Some(checkpoint), Some(statement)) = (&page.checkpoint, &page.statement) else {
            return Err(self.unavailable(format!(
                "{what} carries no checkpoint and signed statement, but hushrun.yaml names the server's log key"
            )));
        };
        let checkpoint = SignedCheckpoint::open(checkpoint, verifier).map_err(|e| {
            self.unavailable(format!(
                "{what}: its checkpoint doesn't verify with the log key hushrun.yaml names: {e}"
            ))
        })?;
        let statement = SignedListing::open(statement, verifier).map_err(|e| {
            self.unavailable(format!(
                "{what}: its statement doesn't verify with the log key hushrun.yaml names: {e}"
            ))
        })?;
        let listing = &statement.listing;
        let about_this_page = listing.prefix == prefix
            && listing.after == page.after
            && listing.next == page.next
            && listing.matches(&page.keys)
            && listing.origin == checkpoint.checkpoint.origin
            && listing.size == checkpoint.size()
            && listing.root == *checkpoint.root();
        if !about_this_page {
            return Err(self.unavailable(format!(
                "{what}: the server's signed statement is about another page than the one it answered"
            )));
        }
        Ok(checkpoint)
    }

    /// After a listing: checks each page, joins its checkpoint to the command's history, and
    /// records the page for `plan --start` (E-5).
    pub(super) fn check_pages(&self, prefix: &str, pages: Vec<PageLog>) -> Result<(), LedgerError> {
        for page in pages {
            if page.has_log() {
                self.state().serves = true;
            }
            match self.key() {
                Key::Verify(verifier) => {
                    let checkpoint = self.verify_page(&verifier, prefix, &page)?;
                    self.adopt(checkpoint, &format!("the listing of {prefix:?}"), true)?;
                }
                Key::Unset => self
                    .state()
                    .unchecked
                    .push(Unchecked::Page(prefix.to_string(), page.clone())),
                Key::Absent => self.warn_unverified(),
            }
            if let Some(statement) = page.statement {
                self.state().listings.push(RecordedListing {
                    prefix: prefix.to_string(),
                    statement,
                    keys: page.keys,
                });
            }
        }
        Ok(())
    }

    fn verify_receipt(
        &self,
        verifier: &Verifier,
        entry: &Entry,
        receipt: &Receipt,
    ) -> Result<SignedCheckpoint, LedgerError> {
        let what = &entry.name;
        let checkpoint = SignedCheckpoint::open(&receipt.checkpoint, verifier).map_err(|e| {
            self.unavailable(format!(
                "{what}: the receipt's checkpoint doesn't verify with the log key hushrun.yaml names: {e}"
            ))
        })?;
        let proof = hashes(&self.base, what, &receipt.proof)?;
        verify_inclusion(
            &entry.leaf_hash(),
            receipt.index,
            checkpoint.size(),
            &proof,
            checkpoint.root(),
        )
        .map_err(|e| {
            self.unavailable(format!(
                "{what}: the receipt doesn't prove the object is in the log, so it isn't known to be written: {e}"
            ))
        })?;
        Ok(checkpoint)
    }

    /// After a create: with a log key, the receipt must prove the object is in the log (L-11);
    /// otherwise the write isn't known to be done.
    pub(super) fn check_receipt(
        &self,
        key: &str,
        bytes: &[u8],
        receipt: Option<Receipt>,
    ) -> Result<(), LedgerError> {
        let entry = Entry::object(key, bytes);
        if receipt.is_some() {
            self.state().serves = true;
        }
        let verifier = match self.key() {
            Key::Verify(verifier) => verifier,
            Key::Unset => {
                if let Some(receipt) = receipt {
                    self.state()
                        .unchecked
                        .push(Unchecked::Receipt(entry, receipt));
                }
                return Ok(());
            }
            Key::Absent => {
                self.warn_unverified();
                return Ok(());
            }
        };
        let Some(receipt) = receipt else {
            return Err(self.unavailable(format!(
                "{key}: the create's answer carries no receipt, but hushrun.yaml names the server's log key, so the object isn't known to be in the log"
            )));
        };
        let checkpoint = self.verify_receipt(&verifier, &entry, &receipt)?;
        self.adopt(checkpoint, &format!("the receipt for {key}"), false)
    }

    /// Warns once that the ledger keeps a log that hushrun.yaml doesn't name (migration).
    fn warn_unverified(&self) {
        let mut state = self.state();
        if !state.serves || state.warned {
            return;
        }
        state.warned = true;
        tracing::warn!(
            "{} keeps a transparency log, but hushrun.yaml names no log key (`transparency_log`), so hushrun can't check its receipts and listings; add the `transparency_log` lines the server shows to hushrun.yaml",
            self.base
        );
    }

    fn key(&self) -> Key {
        self.state().key.clone()
    }

    fn fetch_text(
        &self,
        path: &str,
        query: Option<String>,
        what: &str,
        limit: usize,
    ) -> Result<Vec<u8>, LedgerError> {
        self.io.run(async {
            let mut call = Call::new(Method::GET, path, what, limit);
            call.query = query;
            let answer = self.call(&call).await?;
            if answer.status == StatusCode::NOT_FOUND {
                return Err(
                    self.unavailable(format!("{what}: 404: the server keeps no transparency log"))
                );
            }
            if !answer.status.is_success() {
                return Err(self.failure(&answer, what));
            }
            Ok(answer.body)
        })
    }

    fn fetch_consistency(&self, old: u64, new: u64) -> Result<Vec<Hash>, LedgerError> {
        if old == 0 || old == new {
            return Ok(Vec::new());
        }
        let what = format!("the consistency proof from {old} to {new} leaves");
        let body = self.fetch_text(
            "log/consistency",
            Some(super::form(&[
                ("from", &old.to_string()),
                ("to", &new.to_string()),
            ])),
            &what,
            MAX_SMALL_BYTES,
        )?;
        let answer: ProofAnswer = serde_json::from_slice(&body)
            .map_err(|e| self.unavailable(format!("{what}: not a proof: {e}")))?;
        hashes(&self.base, &what, &answer.proof)
    }

    fn fetch_latest(&self) -> Result<String, LedgerError> {
        let body = self.fetch_text(
            "log/checkpoint",
            None,
            "the latest checkpoint",
            MAX_SMALL_BYTES,
        )?;
        String::from_utf8(body).map_err(|_| self.unavailable("the latest checkpoint isn't UTF-8"))
    }

    /// Leaves `[start, start + limit)`, as far as the latest checkpoint.
    fn fetch_leaves(&self, start: u64, limit: u64) -> Result<Vec<LeafRecord>, LedgerError> {
        let limit = limit.clamp(1, LEAVES_PER_PAGE);
        let what = format!("the log's leaves from {start}");
        let body = self.fetch_text(
            "log/leaves",
            Some(super::form(&[
                ("start", &start.to_string()),
                ("limit", &limit.to_string()),
            ])),
            &what,
            MAX_LEAVES_BYTES,
        )?;
        let answer: LeavesAnswer = serde_json::from_slice(&body)
            .map_err(|e| self.unavailable(format!("{what}: not a page of leaves: {e}")))?;
        if answer.leaves.len() as u64 > limit {
            return Err(self.unavailable(format!("{what}: more leaves than asked for")));
        }
        let mut leaves = Vec::with_capacity(answer.leaves.len());
        for (i, leaf) in answer.leaves.into_iter().enumerate() {
            let index = start + i as u64;
            let bad = |why: &str| self.unavailable(format!("{what}: leaf {index}: {why}"));
            if leaf.index != index {
                return Err(bad("the leaves aren't consecutive"));
            }
            let kind = EntryKind::parse(&leaf.kind).ok_or_else(|| bad("an unknown kind"))?;
            let sha256 = Hash::from_hex(&leaf.sha256).ok_or_else(|| bad("the hash isn't hex"))?;
            let gc = match (&leaf.gc, kind) {
                (Some(gc), EntryKind::Tombstone) => {
                    Some(Hash::from_hex(gc).ok_or_else(|| bad("the GC hash isn't hex"))?)
                }
                (None, _) => None,
                (Some(_), _) => return Err(bad("only a tombstone has a GC statement")),
            };
            let entry = Entry {
                kind,
                name: leaf.name,
                sha256,
                gc,
            };
            entry.check().map_err(|e| bad(&e.to_string()))?;
            let gc_statement = match leaf.statement {
                None => None,
                Some(encoded) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .map_err(|_| bad("the GC statement isn't base64"))?;
                    if entry.gc != Some(Hash::of(&bytes)) {
                        return Err(bad("the GC statement isn't the one the leaf records"));
                    }
                    Some(bytes)
                }
            };
            leaves.push(LeafRecord {
                index,
                entry,
                gc_statement,
            });
        }
        Ok(leaves)
    }

    /// The ledger's latest checkpoint, verified and joined to this command's history.
    fn fetch_checkpoint(&self) -> Result<SignedCheckpoint, LedgerError> {
        let Some(verifier) = self.verifier() else {
            return Err(LedgerError::Invalid(format!(
                "{}: hushrun.yaml names no log key (`transparency_log`), so the server's checkpoints can't be checked",
                self.base
            )));
        };
        let note = self.fetch_latest()?;
        let checkpoint = SignedCheckpoint::open(&note, &verifier).map_err(|e| {
            self.unavailable(format!(
                "the latest checkpoint doesn't verify with the log key hushrun.yaml names: {e}"
            ))
        })?;
        let newest = self.state().newest.clone();
        if let Some(newest) = newest
            && checkpoint.size() < newest.size()
        {
            return Err(self.fork(
                &checkpoint,
                "the server's latest",
                &newest,
                "the server's latest tree is smaller than one it already signed",
            ));
        }
        self.adopt(checkpoint.clone(), "the server's latest", false)?;
        Ok(checkpoint)
    }

    fn verifier(&self) -> Option<Verifier> {
        match self.key() {
            Key::Verify(verifier) => Some(verifier),
            _ => None,
        }
    }

    /// The log key the repository pins, when the command configured one (T-9).
    pub(crate) fn log_verifier(&self) -> Option<Verifier> {
        self.verifier()
    }
}

impl Log for HttpLedger {
    fn configure(&self, key: Option<LogKey>) -> Result<(), LedgerError> {
        let verifier = key
            .map(|k| {
                k.verifier().map_err(|e| {
                    LedgerError::Invalid(format!("the log key hushrun.yaml names: {e}"))
                })
            })
            .transpose()?;
        let unchecked = {
            let mut state = self.state();
            state.key = match &verifier {
                Some(v) => Key::Verify(v.clone()),
                None => Key::Absent,
            };
            std::mem::take(&mut state.unchecked)
        };
        let Some(verifier) = verifier else {
            self.warn_unverified();
            return Ok(());
        };
        // In the order they arrived, as they would have been checked with the key set: a
        // listing older than a receipt before it would hide that write.
        for answer in unchecked {
            match answer {
                Unchecked::Page(prefix, page) => {
                    let checkpoint = self.verify_page(&verifier, &prefix, &page)?;
                    self.adopt(checkpoint, &format!("the listing of {prefix:?}"), true)?;
                }
                Unchecked::Receipt(entry, receipt) => {
                    let checkpoint = self.verify_receipt(&verifier, &entry, &receipt)?;
                    self.adopt(
                        checkpoint,
                        &format!("the receipt for {}", entry.name),
                        false,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn verifying(&self) -> bool {
        matches!(self.key(), Key::Verify(..))
    }

    fn configured(&self) -> bool {
        !matches!(self.key(), Key::Unset)
    }

    fn checkpoint(&self) -> Result<Option<SignedCheckpoint>, LedgerError> {
        if !self.verifying() {
            return Ok(None);
        }
        if let Some(newest) = self.state().newest.clone() {
            return Ok(Some(newest));
        }
        self.latest().map(Some)
    }

    fn latest(&self) -> Result<SignedCheckpoint, LedgerError> {
        self.fetch_checkpoint()
    }

    fn check_checkpoint(&self, note: &str, source: &str) -> Result<(), LedgerError> {
        let Some(verifier) = self.verifier() else {
            return Ok(());
        };
        let theirs = match SignedCheckpoint::open(note, &verifier) {
            Ok(checkpoint) => checkpoint,
            Err(e) => {
                tracing::warn!(
                    "{source} carries a checkpoint the log key hushrun.yaml names didn't sign ({e}); it proves nothing, so it isn't compared"
                );
                return Ok(());
            }
        };
        let newest = match self.state().newest.clone() {
            Some(newest) => newest,
            None => self.latest()?,
        };
        if theirs.size() > newest.size() {
            // This command's view may just be older: compare with the ledger's latest.
            let latest = self.latest()?;
            if theirs.size() > latest.size() {
                return Err(self.fork(
                    &theirs,
                    source,
                    &latest,
                    "the server shows a smaller tree than one it signed for a writer",
                ));
            }
        }
        self.adopt(theirs, source, false)
    }

    fn take_listings(&self) -> Vec<RecordedListing> {
        std::mem::take(&mut self.state().listings)
    }

    fn prove_object(&self, key: &str, bytes: &[u8]) -> Result<(), LedgerError> {
        check_http_key(key)?;
        let Some(verifier) = self.verifier() else {
            return Ok(());
        };
        let what = format!("the proof that {key} is in the log");
        let body = self.fetch_text(
            "log/inclusion",
            Some(super::form(&[("key", key)])),
            &what,
            MAX_SMALL_BYTES,
        )?;
        let answer: ObjectProof = serde_json::from_slice(&body)
            .map_err(|e| self.unavailable(format!("{what}: not a receipt: {e}")))?;
        let entry = Entry::object(key, bytes);
        let checkpoint = self
            .verify_receipt(&verifier, &entry, &answer.receipt)
            .map_err(|_| {
                self.unavailable(format!(
                    "{key}: the server served bytes its log doesn't hold for that key"
                ))
            })?;
        self.adopt(checkpoint, &what, false)
    }

    fn leaves(&self, start: u64, limit: u64) -> Result<Vec<LeafRecord>, LedgerError> {
        self.fetch_leaves(start, limit)
    }

    fn consistency_proof(&self, old: u64, new: u64) -> Result<Vec<Hash>, LedgerError> {
        self.fetch_consistency(old, new)
    }

    fn delete_signed(&self, key: &str, statement: &[u8]) -> Result<(), LedgerError> {
        check_http_key(key)?;
        if statement.len() > MAX_GC_STATEMENT {
            return Err(LedgerError::Invalid(format!(
                "{key}: a GC statement of {} bytes is larger than {MAX_GC_STATEMENT}",
                statement.len()
            )));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(statement);
        self.delete_with(key, Some(encoded))
    }
}
