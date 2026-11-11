//! \[C-38\] Hints: what one machine's store says a check's test files read, in a form another
//! machine can fetch from a server and use to compute the same unit keys (C-31), with no file
//! path in it. RFC 0003 §3.11, ADR 0075.
//!
//! A hint is never trusted: a wrong one only makes a key no run recorded, and that test file runs.
//! It holds, for each test file whose reads were fully observed, the files it read that its key
//! holds (units and sources) and the directories it listed (Go); for Vitest, the runner's list of
//! test files; and the adapter's word. No content hash, edge, rest set or result.
//!
//! # The key and tokens
//!
//! Every path in a hint is a **token**: the first 16 bytes of HMAC-SHA-256 (RFC 2104) of the
//! path's UTF-8 bytes, under the hint's **key**. The key is HMAC-SHA-256 with the ASCII bytes of
//! [`KEY_DOMAIN`] as its key, of the bytes [`crate::units::shape`] hashes: the sources set's paths,
//! each path's UTF-8 bytes followed by one NUL byte, sorted by bytes, each once. It is computed from
//! those bytes, never from the shape digest, so a server that holds the address (which holds the
//! shape digest) can't derive it.
//!
//! # The address
//!
//! A hint is stored under its **address**, [`address`]: the SHA-256 of the JCS form (RFC 8785) of
//! `{"schema": "hushrun.unit-hints-address/v0", "check": "sha256:<check id hex>", "shape":
//! "sha256:<shape hex>"}`, the check id that of the local store (C-33's `hushrun.unit-map-id/v0`).
//! Written out, with the members sorted and no whitespace:
//! `{"check":"sha256:…","schema":"hushrun.unit-hints-address/v0","shape":"sha256:…"}`.
//!
//! # The bytes
//!
//! A hint's bytes are one zlib stream (RFC 1950: a two-byte header, raw deflate of RFC 1951, its
//! Adler-32 at the end), with no preset dictionary and nothing after it, of its **content**. Every
//! number in the content is an unsigned 32-bit integer, big-endian (`u32`):
//!
//! ```text
//! "hushrun.unit-hints/v0\n"      the schema and a line feed, 22 bytes
//! u8 adapter length, 1 to 32      then the adapter's word: a-z, 0-9 and '-'
//! u32 T, then T tokens            16 bytes each, strictly ascending as bytes
//! u32 V, then V indices           the unit files (Vitest's list), strictly ascending
//! u32 U, then U units, each:
//!   u32 own index                 the unit's own path; strictly ascending from unit to unit
//!   u32 R, then R indices         the files it read, strictly ascending
//!   u32 D, then D indices         the directories it listed, strictly ascending
//! ```
//!
//! An index names a token by its position in the table, from 0, and is less than T; every token
//! is named by at least one index. Of two units whose own paths have one token, the writer writes
//! the first by path, and only its paths. So one hint has one content, and its **content id**
//! ([`content_id`]) is the SHA-256 of the content: two writers whose compression differs write
//! the same content id. The writer compresses at level 9; a reader takes any level.
//!
//! # Bounds
//!
//! - The bytes: 1 to [`MOST_BYTES`] (2 MiB), what a server keeps.
//! - The content: at most [`MOST_CONTENT`] (16 MiB) once inflated; inflating stops there.
//! - Units and unit files: [`MOST_UNITS`] (5,000) each, as many as a run records.
//! - Tokens: [`MOST_TOKENS`] (1,000,000).
//! - Entries, unit files plus every unit's reads and directories: [`MOST_ENTRIES`] (2,000,000).
//!
//! The writer refuses a hint past any bound ([`Unwritable`]); the reader reads none ([`Unread`]),
//! and checks each count against the bound and the bytes left before it allocates.
//!
//! # Reading
//!
//! [`read`] turns tokens back into paths only by computing the token of each path of the scope it
//! is given, and of each directory that holds one (`""` and `"."` for the root). A file read or a
//! unit file must match a file; a directory listed must match a directory; a unit's own token,
//! either (a Go package is named by its directory). A unit any of whose tokens matches nothing is
//! dropped whole, and it runs; so is one with a token two paths of the scope share. A unit file
//! that matches nothing, or two paths, is left out of the list. So what the reader yields only
//! ever names the scope's files and their directories.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use sha2::{Digest as _, Sha256};
use zlib_rs::{Deflate, DeflateFlush, Inflate, InflateFlush, Status};

use crate::digest::{Digest, to_hex};

/// The schema of a hint's content: its first line.
pub const SCHEMA: &str = "hushrun.unit-hints/v0";

/// The domain string a hint's key is derived under: HMAC-SHA-256's key.
pub const KEY_DOMAIN: &str = "hushrun.unit-hints-key/v0";

/// The schema of a hint's address.
pub const ADDRESS_SCHEMA: &str = "hushrun.unit-hints-address/v0";

/// The most bytes a hint has, compressed: what a server keeps (results-v0, Hints).
pub const MOST_BYTES: usize = 2 * 1024 * 1024;

/// The most bytes a hint's content has once inflated, eight times [`MOST_BYTES`]: room for any
/// hint a real store makes, and a stop for a stream made to inflate without end.
pub const MOST_CONTENT: usize = 16 * 1024 * 1024;

/// The most units, and unit files, a hint holds: as many as a run records (C-33).
pub use crate::units::MOST_UNITS;

/// The most distinct paths a hint holds.
pub const MOST_TOKENS: usize = 1_000_000;

/// The most entries a hint holds: its unit files and every unit's reads and directories.
pub const MOST_ENTRIES: usize = 2_000_000;

/// The longest adapter word.
const MOST_ADAPTER: usize = 32;

/// How much a stream is inflated or deflated into at a time.
const CHUNK: usize = 64 * 1024;

/// zlib's window: 32 KiB, as every zlib writes.
const WINDOW_BITS: u8 = 15;

/// The level the writer compresses at.
const LEVEL: i32 = 9;

/// The system of the machine that saw a hint (C-38). A machine reads only its own system's hints
/// (the plan's S3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    /// This build's system; `None` for any other, which shares no hints.
    pub fn this() -> Option<Os> {
        if cfg!(target_os = "linux") {
            Some(Os::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Os::Macos)
        } else if cfg!(target_os = "windows") {
            Some(Os::Windows)
        } else {
            None
        }
    }

    /// This build's word, `none` for a system that shares no hints: for a debug log, never sent.
    pub fn this_word() -> &'static str {
        Os::this().map_or("none", Os::word)
    }

    /// The word a server takes: `linux`, `macos` or `windows`.
    pub fn word(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Macos => "macos",
            Os::Windows => "windows",
        }
    }

    /// The system a word names.
    pub fn from_word(word: &str) -> Option<Os> {
        match word {
            "linux" => Some(Os::Linux),
            "macos" => Some(Os::Macos),
            "windows" => Some(Os::Windows),
            _ => None,
        }
    }
}

/// HMAC-SHA-256 (RFC 2104) with its two hash states already keyed.
#[derive(Clone)]
struct Mac {
    inner: Sha256,
    outer: Sha256,
}

impl Mac {
    fn new(key: &[u8]) -> Mac {
        let mut block = [0u8; 64];
        if key.len() > block.len() {
            block[..32].copy_from_slice(&Sha256::digest(key));
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let mut inner = Sha256::new();
        inner.update(block.map(|b| b ^ 0x36));
        let mut outer = Sha256::new();
        outer.update(block.map(|b| b ^ 0x5c));
        Mac { inner, outer }
    }

    fn mac(&self, message: &[u8]) -> [u8; 32] {
        let mut inner = self.inner.clone();
        inner.update(message);
        let mut outer = self.outer.clone();
        outer.update(inner.finalize());
        outer.finalize().into()
    }
}

/// HMAC-SHA-256 of `message` under `key` (RFC 2104).
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    Mac::new(key).mac(message)
}

/// A hint's key (C-38): derived from the names of a check's sources, never sent anywhere.
#[derive(Clone)]
pub struct Key {
    bytes: [u8; 32],
    mac: Mac,
}

impl Key {
    /// \[C-38\] The key for a check whose sources set is `sources`: HMAC-SHA-256 under
    /// [`KEY_DOMAIN`] of the bytes [`crate::units::shape`] hashes.
    pub fn derive<'a>(sources: impl IntoIterator<Item = &'a str>) -> Key {
        let bytes = hmac_sha256(KEY_DOMAIN.as_bytes(), &crate::units::shape_bytes(sources));
        Key {
            bytes,
            mac: Mac::new(&bytes),
        }
    }

    /// The key's 32 bytes, for test vectors.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// \[C-38\] The token of `path`: the first 16 bytes of HMAC-SHA-256 of its UTF-8 bytes under
    /// this key.
    pub fn token(&self, path: &str) -> Token {
        let mac = self.mac.mac(path.as_bytes());
        let mut token = [0u8; 16];
        token.copy_from_slice(&mac[..16]);
        Token(token)
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}

/// A path in a hint: a keyed hash of it, 16 bytes (C-38).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Token(pub [u8; 16]);

impl Token {
    /// The 32 lowercase hex digits.
    pub fn hex(&self) -> String {
        to_hex(&self.0)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token({})", self.hex())
    }
}

/// \[C-38\] The address a hint is stored under: the SHA-256 of the JCS form of
/// `{"schema": "hushrun.unit-hints-address/v0", "check", "shape"}`, the check's id (C-33) and its
/// shape (C-31) written `sha256:<hex>`. It holds the shape digest and not the key.
pub fn address(check_id: &Digest, shape: &Digest) -> Digest {
    // Every value is ASCII with nothing to escape, so this is the JCS form: members sorted, no
    // whitespace.
    let canonical =
        format!(r#"{{"check":"{check_id}","schema":"{ADDRESS_SCHEMA}","shape":"{shape}"}}"#);
    Digest::of(canonical.as_bytes())
}

/// What a hint says of one test file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unit {
    /// The files it read that its key holds (units and sources), as the store's map holds them.
    pub reads: BTreeSet<String>,
    /// \[ADR 0070\] The directories it listed, from the root (`""` or `"."` for the root).
    pub dirs: BTreeSet<String>,
}

/// A hint, with paths: what a writer is given and what a reader yields (C-38).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hint {
    /// The adapter's word, such as `pytest`: 1 to 32 of a-z, 0-9 and `-`.
    pub adapter: String,
    /// Each test file whose reads were fully observed, by its path (for Go, its package's
    /// directory).
    pub units: BTreeMap<String, Unit>,
    /// \[ADR 0071\] The test files the runner listed, for an adapter whose unit rule is its
    /// runner's own list (Vitest's); empty for the others.
    pub unit_files: BTreeSet<String>,
}

/// A hint's bytes, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The compressed bytes, 1 to [`MOST_BYTES`].
    pub bytes: Vec<u8>,
    /// The SHA-256 of the content, before compression.
    pub content_id: Digest,
}

/// Why a hint has no bytes (C-38).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unwritable {
    /// Past one of the bounds: `what` is how many there are of what, and `most` the bound.
    #[error("the hint is too large: {size} {what}, more than {most}")]
    TooLarge {
        what: &'static str,
        size: usize,
        most: usize,
    },
    /// The adapter's word isn't 1 to 32 of a-z, 0-9 and `-`.
    #[error("{0:?} isn't an adapter's word")]
    Adapter(String),
}

/// Why a hint's bytes weren't read: for a debug log (C-38). A hint that isn't read is ignored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unread {
    #[error("{0} bytes, more than {MOST_BYTES}")]
    TooLarge(usize),
    #[error("no bytes")]
    Empty,
    #[error("not a zlib stream: {0}")]
    Compression(&'static str),
    #[error("it inflates to more than {MOST_CONTENT} bytes")]
    Inflates,
    #[error("the zlib stream ends before its end")]
    Truncated,
    #[error("bytes after the zlib stream")]
    Trailing,
    #[error("not {SCHEMA}")]
    Schema,
    #[error("damaged: {0}")]
    Damaged(&'static str),
}

/// What [`read`] yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    /// The units every token of which named a path of the scope, and the unit files that did.
    pub hint: Hint,
    /// The SHA-256 of the content: the same for the same hint, whoever compressed it.
    pub content_id: Digest,
    /// How many units the bytes hold.
    pub units: usize,
    /// How many of them were dropped: a token named no path of the scope, or two.
    pub dropped: usize,
}

/// The content id (C-38) of a hint's content: its SHA-256.
pub fn content_id(content: &[u8]) -> Digest {
    Digest::of(content)
}

/// \[C-38\] A hint's content (uncompressed), as the module's docs lay it out.
pub fn content(hint: &Hint, key: &Key) -> Result<Vec<u8>, Unwritable> {
    encode(hint, &|path| key.token(path))
}

/// [`content`] with paths made tokens by `token`.
fn encode(hint: &Hint, token: &dyn Fn(&str) -> Token) -> Result<Vec<u8>, Unwritable> {
    let word = hint.adapter.as_bytes();
    if !adapter_word(word) {
        return Err(Unwritable::Adapter(hint.adapter.clone()));
    }
    bound("test files", hint.units.len(), MOST_UNITS)?;
    bound("unit files", hint.unit_files.len(), MOST_UNITS)?;
    let entries = hint.unit_files.len()
        + hint
            .units
            .values()
            .map(|unit| unit.reads.len() + unit.dirs.len())
            .sum::<usize>();
    bound("entries", entries, MOST_ENTRIES)?;

    // Each unit by its own token; of two units with one token, the first by path stays, so that
    // the table holds no token only the other named. Two paths with one token share it: the
    // reader drops a unit that uses it when both are in its scope.
    let tokens_of = |paths: &BTreeSet<String>| -> BTreeSet<Token> {
        paths.iter().map(|path| token(path)).collect()
    };
    let mut kept: BTreeMap<Token, (BTreeSet<Token>, BTreeSet<Token>)> = BTreeMap::new();
    for (own, unit) in &hint.units {
        kept.entry(token(own))
            .or_insert_with(|| (tokens_of(&unit.reads), tokens_of(&unit.dirs)));
    }
    let unit_files = tokens_of(&hint.unit_files);
    let mut table: BTreeSet<Token> = unit_files.clone();
    for (own, (reads, dirs)) in &kept {
        table.insert(*own);
        table.extend(reads.iter().chain(dirs.iter()).copied());
    }
    let tokens: Vec<Token> = table.into_iter().collect();
    bound("paths", tokens.len(), MOST_TOKENS)?;
    let position = |token: &Token| -> u32 {
        let at = tokens.binary_search(token).unwrap_or_default();
        u32::try_from(at).unwrap_or(u32::MAX)
    };
    // Sorted tokens give sorted indices.
    let indices = |set: &BTreeSet<Token>| -> Vec<u32> { set.iter().map(position).collect() };
    let units: Vec<(u32, Vec<u32>, Vec<u32>)> = kept
        .iter()
        .map(|(own, (reads, dirs))| (position(own), indices(reads), indices(dirs)))
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(SCHEMA.as_bytes());
    out.push(b'\n');
    out.push(u8::try_from(word.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(word);
    push_count(&mut out, tokens.len());
    for token in &tokens {
        out.extend_from_slice(&token.0);
    }
    push_list(&mut out, &indices(&unit_files));
    push_count(&mut out, units.len());
    for (own, reads, dirs) in &units {
        out.extend_from_slice(&own.to_be_bytes());
        push_list(&mut out, reads);
        push_list(&mut out, dirs);
    }
    bound("bytes of content", out.len(), MOST_CONTENT)?;
    Ok(out)
}

/// \[C-38\] The writer: a hint's bytes for `key`, the same bytes for the same hint and key, or
/// why there are none. Past [`MOST_BYTES`] compressed, or any other bound, it is
/// [`Unwritable::TooLarge`].
pub fn write(hint: &Hint, key: &Key) -> Result<Written, Unwritable> {
    let content = content(hint, key)?;
    let bytes = compress(&content)?;
    Ok(Written {
        bytes,
        content_id: content_id(&content),
    })
}

/// \[C-38\] The reader: the hint `bytes` hold, its tokens matched against the paths of `scope`
/// (a [`crate::units::Tree`]'s paths, or any others) under `key`; or why it can't be read. It
/// never panics, and what it yields names only paths of the scope and their directories.
pub fn read<'a>(
    bytes: &[u8],
    key: &Key,
    scope: impl IntoIterator<Item = &'a str>,
) -> Result<Read, Unread> {
    let content = inflate(bytes)?;
    read_content(&content, key, scope)
}

/// \[C-38\] [`read`] for a content already inflated: bounded, and never a panic, the same way.
pub fn read_content<'a>(
    content: &[u8],
    key: &Key,
    scope: impl IntoIterator<Item = &'a str>,
) -> Result<Read, Unread> {
    decode(content, &|path| key.token(path), scope)
}

/// [`read_content`] with paths made tokens by `token`.
fn decode<'a>(
    content: &[u8],
    token: &dyn Fn(&str) -> Token,
    scope: impl IntoIterator<Item = &'a str>,
) -> Result<Read, Unread> {
    if content.len() > MOST_CONTENT {
        return Err(Unread::Inflates);
    }
    let parsed = parse(content)?;
    let found = resolve(&parsed.tokens, token, scope);

    let file = |index: u32| match found.get(usize::try_from(index).ok()?) {
        Some(Found::File(path)) => Some(*path),
        _ => None,
    };
    let dir = |index: u32| match found.get(usize::try_from(index).ok()?) {
        Some(Found::Dir(path)) => Some(*path),
        _ => None,
    };
    let own = |index: u32| match found.get(usize::try_from(index).ok()?) {
        Some(Found::File(path) | Found::Dir(path)) => Some(*path),
        _ => None,
    };

    let mut hint = Hint {
        adapter: parsed.adapter,
        units: BTreeMap::new(),
        unit_files: parsed
            .unit_files
            .iter()
            .filter_map(|i| file(*i))
            .map(str::to_string)
            .collect(),
    };
    let total = parsed.units.len();
    for unit in &parsed.units {
        let Some(path) = own(unit.own) else { continue };
        let Some(reads) = unit
            .reads
            .iter()
            .map(|i| file(*i).map(str::to_string))
            .collect::<Option<BTreeSet<String>>>()
        else {
            continue;
        };
        let Some(dirs) = unit
            .dirs
            .iter()
            .map(|i| dir(*i).map(str::to_string))
            .collect::<Option<BTreeSet<String>>>()
        else {
            continue;
        };
        hint.units.insert(path.to_string(), Unit { reads, dirs });
    }
    Ok(Read {
        dropped: total - hint.units.len(),
        units: total,
        hint,
        content_id: content_id(content),
    })
}

/// A content's structure, before any token is matched.
struct Parsed {
    adapter: String,
    tokens: Vec<Token>,
    unit_files: Vec<u32>,
    units: Vec<ParsedUnit>,
}

struct ParsedUnit {
    own: u32,
    reads: Vec<u32>,
    dirs: Vec<u32>,
}

/// What a token of the table matched in the scope.
#[derive(Clone, Copy)]
enum Found<'a> {
    Nothing,
    File(&'a str),
    Dir(&'a str),
    /// Two paths of the scope have this token.
    Two,
}

/// Matches each token of `tokens` (sorted) with the paths of `scope` and their directories.
fn resolve<'a>(
    tokens: &[Token],
    token: &dyn Fn(&str) -> Token,
    scope: impl IntoIterator<Item = &'a str>,
) -> Vec<Found<'a>> {
    let mut found = vec![Found::Nothing; tokens.len()];
    let mut note = |path: &'a str, as_dir: bool| {
        let Ok(at) = tokens.binary_search(&token(path)) else {
            return;
        };
        let new = if as_dir {
            Found::Dir(path)
        } else {
            Found::File(path)
        };
        found[at] = match found[at] {
            Found::Nothing => new,
            Found::File(was) if !as_dir && was == path => new,
            Found::Dir(was) if as_dir && was == path => new,
            _ => Found::Two,
        };
    };
    let mut dirs: BTreeSet<&'a str> = BTreeSet::from(["", "."]);
    for path in scope {
        note(path, false);
        for (at, byte) in path.bytes().enumerate() {
            if byte == b'/' {
                dirs.insert(&path[..at]);
            }
        }
    }
    for dir in dirs {
        note(dir, true);
    }
    found
}

/// Reads a content's structure, checking every bound and that it is the one canonical content.
fn parse(content: &[u8]) -> Result<Parsed, Unread> {
    let mut r = Reader { rest: content };
    let header = r.take(SCHEMA.len() + 1).ok_or(Unread::Schema)?;
    if &header[..SCHEMA.len()] != SCHEMA.as_bytes() || header[SCHEMA.len()] != b'\n' {
        return Err(Unread::Schema);
    }
    let length = usize::from(r.byte()?);
    let word = r.take(length).ok_or(Unread::Damaged("truncated"))?;
    if !adapter_word(word) {
        return Err(Unread::Damaged("not an adapter's word"));
    }
    let adapter = String::from_utf8(word.to_vec()).map_err(|_| Unread::Damaged("not UTF-8"))?;

    let count = r.count(MOST_TOKENS, 16)?;
    let mut tokens = Vec::with_capacity(count);
    for _ in 0..count {
        let bytes = r.take(16).ok_or(Unread::Damaged("truncated"))?;
        let mut token = [0u8; 16];
        token.copy_from_slice(bytes);
        let token = Token(token);
        if tokens.last().is_some_and(|last| *last >= token) {
            return Err(Unread::Damaged("tokens out of order"));
        }
        tokens.push(token);
    }
    let mut used = vec![false; tokens.len()];
    let mut entries = 0usize;

    let unit_files = r.list(MOST_UNITS, &mut used, &mut entries)?;
    let count = r.count(MOST_UNITS, 12)?;
    let mut units: Vec<ParsedUnit> = Vec::with_capacity(count);
    for _ in 0..count {
        let own = r.index(&mut used)?;
        if units.last().is_some_and(|last| last.own >= own) {
            return Err(Unread::Damaged("units out of order"));
        }
        let reads = r.list(MOST_TOKENS, &mut used, &mut entries)?;
        let dirs = r.list(MOST_TOKENS, &mut used, &mut entries)?;
        units.push(ParsedUnit { own, reads, dirs });
    }
    if !r.rest.is_empty() {
        return Err(Unread::Damaged("bytes after its end"));
    }
    if used.contains(&false) {
        return Err(Unread::Damaged("a token no index names"));
    }
    Ok(Parsed {
        adapter,
        tokens,
        unit_files,
        units,
    })
}

/// A cursor over a content.
struct Reader<'b> {
    rest: &'b [u8],
}

impl<'b> Reader<'b> {
    fn take(&mut self, n: usize) -> Option<&'b [u8]> {
        if n > self.rest.len() {
            return None;
        }
        let (head, rest) = self.rest.split_at(n);
        self.rest = rest;
        Some(head)
    }

    fn byte(&mut self) -> Result<u8, Unread> {
        self.take(1)
            .map(|b| b[0])
            .ok_or(Unread::Damaged("truncated"))
    }

    fn u32(&mut self) -> Result<u32, Unread> {
        let bytes = self.take(4).ok_or(Unread::Damaged("truncated"))?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A count of at most `most` things of at least `each` bytes, which the bytes left can hold.
    fn count(&mut self, most: usize, each: usize) -> Result<usize, Unread> {
        let count = usize::try_from(self.u32()?).map_err(|_| Unread::Damaged("too many"))?;
        if count > most {
            return Err(Unread::Damaged("too many"));
        }
        if count.saturating_mul(each) > self.rest.len() {
            return Err(Unread::Damaged("truncated"));
        }
        Ok(count)
    }

    fn index(&mut self, used: &mut [bool]) -> Result<u32, Unread> {
        let index = self.u32()?;
        let slot = usize::try_from(index)
            .ok()
            .and_then(|i| used.get_mut(i))
            .ok_or(Unread::Damaged("an index past the tokens"))?;
        *slot = true;
        Ok(index)
    }

    /// A list of strictly ascending indices, at most `most`, counted in `entries`.
    fn list(
        &mut self,
        most: usize,
        used: &mut [bool],
        entries: &mut usize,
    ) -> Result<Vec<u32>, Unread> {
        let count = self.count(most, 4)?;
        *entries += count;
        if *entries > MOST_ENTRIES {
            return Err(Unread::Damaged("too many entries"));
        }
        let mut out: Vec<u32> = Vec::with_capacity(count);
        for _ in 0..count {
            let index = self.index(used)?;
            if out.last().is_some_and(|last| *last >= index) {
                return Err(Unread::Damaged("indices out of order"));
            }
            out.push(index);
        }
        Ok(out)
    }
}

fn adapter_word(word: &[u8]) -> bool {
    (1..=MOST_ADAPTER).contains(&word.len())
        && word
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn bound(what: &'static str, size: usize, most: usize) -> Result<(), Unwritable> {
    if size > most {
        Err(Unwritable::TooLarge { what, size, most })
    } else {
        Ok(())
    }
}

/// A count or list length, which [`bound`] kept within `u32`.
fn push_count(out: &mut Vec<u8>, count: usize) {
    out.extend_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_be_bytes());
}

fn push_list(out: &mut Vec<u8>, indices: &[u32]) {
    push_count(out, indices.len());
    for index in indices {
        out.extend_from_slice(&index.to_be_bytes());
    }
}

/// One zlib stream of `content`, at most [`MOST_BYTES`].
fn compress(content: &[u8]) -> Result<Vec<u8>, Unwritable> {
    let too_large = |size| Unwritable::TooLarge {
        what: "bytes compressed",
        size,
        most: MOST_BYTES,
    };
    let mut deflate = Deflate::new(LEVEL, true, WINDOW_BITS);
    let mut out = Vec::new();
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let read = usize::try_from(deflate.total_in()).unwrap_or(usize::MAX);
        let wrote = deflate.total_out();
        let status = deflate
            .compress(
                content.get(read..).unwrap_or_default(),
                &mut chunk,
                DeflateFlush::Finish,
            )
            .map_err(|_| too_large(out.len()))?;
        let new = usize::try_from(deflate.total_out() - wrote).unwrap_or(usize::MAX);
        out.extend_from_slice(chunk.get(..new).unwrap_or_default());
        if out.len() > MOST_BYTES {
            return Err(too_large(out.len()));
        }
        if status == Status::StreamEnd {
            return Ok(out);
        }
        if new == 0 && usize::try_from(deflate.total_in()).ok() == Some(read) {
            // No progress: deflate never stops so, given room.
            return Err(too_large(out.len()));
        }
    }
}

/// The content of a hint's zlib stream, bounded by [`MOST_CONTENT`] as it inflates: what [`read`]
/// parses, and what a test searches for a path in the clear.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, Unread> {
    if bytes.len() > MOST_BYTES {
        return Err(Unread::TooLarge(bytes.len()));
    }
    if bytes.is_empty() {
        return Err(Unread::Empty);
    }
    let mut inflate = Inflate::new(true, WINDOW_BITS);
    let mut out = Vec::new();
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let read = usize::try_from(inflate.total_in()).unwrap_or(usize::MAX);
        let wrote = inflate.total_out();
        let input = bytes.get(read..).unwrap_or_default();
        let status = inflate
            .decompress(input, &mut chunk, InflateFlush::NoFlush)
            .map_err(|err| Unread::Compression(err.as_str()))?;
        let new = usize::try_from(inflate.total_out() - wrote).unwrap_or(usize::MAX);
        if out.len().saturating_add(new) > MOST_CONTENT {
            return Err(Unread::Inflates);
        }
        out.extend_from_slice(chunk.get(..new).unwrap_or_default());
        if status == Status::StreamEnd {
            break;
        }
        let progressed = usize::try_from(inflate.total_in()).unwrap_or(usize::MAX) != read;
        if !progressed && new == 0 {
            return Err(Unread::Truncated);
        }
    }
    if usize::try_from(inflate.total_in()).ok() != Some(bytes.len()) {
        return Err(Unread::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
