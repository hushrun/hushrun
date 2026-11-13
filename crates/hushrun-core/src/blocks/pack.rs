//! \[B-27, B-28\] The pack: one build block as one sealed object, `hushrun.block-pack/v1`, the only
//! form in which a block leaves a machine. Pure apart from `std::io`'s traits: the caller chooses
//! where bytes come from and go.
//!
//! # The bytes
//!
//! ```text
//! "HRBP" | version 1 | codec 1 (zstd) | cipher 1 (ChaCha20-Poly1305) | 32-byte salt
//! chunks: u32 length | ciphertext and its 16-byte tag    (big-endian; the length counts both)
//! ```
//!
//! The plaintext the chunks carry is one zstd stream, compressed on one thread at level 3 with
//! nothing else configured, of:
//!
//! ```text
//! u32 length | the record, JCS JSON
//! each file's bytes, in the order the record lists them
//! ```
//!
//! Every chunk carries 1 MiB of that stream but the last, which carries the rest (possibly
//! nothing). Exactly one chunk is the last.
//!
//! # The seal
//!
//! - `salt = HMAC-SHA-256(key = the block's key, "hushrun.block-pack-salt/v1\n" + SHA-256(the
//!   compressed stream))`: taken from exactly the bytes handed to the cipher. So the same block packs
//!   to the same bytes every time (a retry is the same upload), and two different contents under one
//!   key never share a secret and a nonce, the one misuse this cipher doesn't survive. It is keyed
//!   with the block's key, so it tells nothing to whoever can't compute that key.
//! - `secret = HKDF-SHA-256(input = the block's key, salt, info = "hushrun.block-pack/v1")`.
//! - Each chunk's nonce is its index, an 11-byte big-endian counter from 0, then one byte: 1 for
//!   the last chunk, 0 otherwise. A pack cut short, or with a chunk added after the last, fails.
//! - The 39 header bytes are the associated data of every chunk: no byte of the pack goes unsealed.
//!
//! Whoever can compute a block's key can open its pack: that takes the same sources, flags and
//! compiler, which is who could have built the block. The backend and the object store can't.
//!
//! # The record
//!
//! `{"schema": "hushrun.block-pack-record/v1", "key", "step", "description", "files": [{"name",
//! "sha256", "size"}]}`: the block's key and first key, the store's description of it with what
//! differs per machine taken out ([`normalize`]), and each file's name, size and SHA-256.
//!
//! # Opening
//!
//! [`open`] checks, and when several fail says the first of: the pack's size and SHA-256 against
//! what the server said, the header and the cipher's tags, the stream and the record, each file's
//! size and SHA-256 against the record, the record's key and first key against what was asked, and
//! the digest of the outputs against what the server's row holds. It writes the files to a
//! [`Sink`] as it goes: the caller keeps nothing unless it answers `Ok`.
//!
//! Hints go sealed the same way, with their first key ([`seal_hint`]).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::rc::Rc;

use ring::aead::{self, Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::{hkdf, hmac};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{Hex32, MOST_HINT_BYTES, MOST_PACK_BYTES, OutputFile, outputs_digest};

/// The schema of the record inside a pack.
pub const RECORD_SCHEMA: &str = "hushrun.block-pack-record/v1";
/// The bytes a pack starts with.
pub const MAGIC: &[u8; 4] = b"HRBP";
/// The bytes a sealed hint starts with.
pub const HINT_MAGIC: &[u8; 4] = b"HRBH";
/// The version, codec and cipher bytes of the header.
pub const VERSION: u8 = 1;
pub const CODEC_ZSTD: u8 = 1;
pub const CIPHER_CHACHA20_POLY1305: u8 = 1;
/// The header: magic, version, codec, cipher and the salt.
pub const HEADER_BYTES: usize = 4 + 3 + 32;
/// The plaintext of every chunk but the last.
pub const CHUNK: usize = 1024 * 1024;
/// A chunk's tag.
const TAG: usize = 16;
/// zstd's level, on one thread.
pub const LEVEL: i32 = 3;
/// The largest record.
pub const MOST_RECORD_BYTES: usize = 16 * 1024 * 1024;
/// The most files one block holds.
pub const MOST_FILES: usize = 100_000;
/// The most bytes a pack's files hold together, past which a reader stops: a 2 GiB pack of
/// compiler output inflates to a few times that.
pub const MOST_FILE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// The most bytes a hint opens to.
const MOST_HINT_PLAIN: usize = 16 * 1024 * 1024;

/// A pack's info and salt domain, and a sealed hint's.
struct Construction {
    magic: &'static [u8; 4],
    info: &'static [u8],
    salt_domain: &'static [u8],
}

const PACK: Construction = Construction {
    magic: MAGIC,
    info: b"hushrun.block-pack/v1",
    salt_domain: b"hushrun.block-pack-salt/v1\n",
};

const HINT: Construction = Construction {
    magic: HINT_MAGIC,
    info: b"hushrun.block-hint/v1",
    salt_domain: b"hushrun.block-hint-salt/v1\n",
};

/// One file of a block, as its record lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFile {
    pub name: String,
    pub size: u64,
    pub sha256: Hex32,
}

/// What a pack says of its block: its key, its first key, the store's description of it, and its
/// files in the order their bytes follow.
#[derive(Debug, Clone, PartialEq)]
pub struct PackRecord {
    pub key: Hex32,
    pub step: Hex32,
    pub description: Value,
    pub files: Vec<PackFile>,
}

/// A pack written: its size and its SHA-256, which `begin` names and a reader checks first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sealed {
    pub bytes: u64,
    pub sha256: Hex32,
}

/// What a reader was told to expect of a pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expect {
    /// The key the asking compile computed: the pack opens with it alone.
    pub key: [u8; 32],
    /// The first key of that compile.
    pub step: Option<Hex32>,
    /// The pack's SHA-256 and size, as the server's row holds them.
    pub pack_sha256: Option<Hex32>,
    pub bytes: Option<u64>,
    /// The digest of the block's outputs, as the server's row holds it.
    pub outputs: Option<Hex32>,
}

/// Where [`open`] writes a block's files: `file` before each file's bytes, then its bytes in
/// pieces.
pub trait Sink {
    fn file(&mut self, index: usize, name: &str, size: u64) -> io::Result<()>;
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
}

/// Why a pack couldn't be written or opened.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// Its size isn't the one the server named.
    #[error("the pack's size isn't the one asked for")]
    PackSize,
    /// Its SHA-256 isn't the one the server named.
    #[error("the pack's SHA-256 isn't the one asked for")]
    PackSha,
    /// Its header isn't one this version reads.
    #[error("the pack's header {0}")]
    Header(&'static str),
    /// A chunk didn't open with the key, or the chunks are cut short, out of order, or followed
    /// by more.
    #[error("the pack doesn't open: {0}")]
    Seal(&'static str),
    /// The stream inside isn't zstd.
    #[error("the pack's stream doesn't decompress")]
    Decompress,
    /// The record is missing, too large, or not one this version reads.
    #[error("the pack's record {0}")]
    Record(&'static str),
    /// A file's bytes end before its size.
    #[error("{name:?} in the pack is shorter than its record says")]
    FileSize { name: String },
    /// A file's bytes aren't the SHA-256 its record names.
    #[error("{name:?} in the pack isn't the bytes its record names")]
    FileSha { name: String },
    /// The record names another key than the one asked for.
    #[error("the pack's record names another key")]
    Key,
    /// The record names another first key than the one asked for.
    #[error("the pack's record names another first key")]
    Step,
    /// The files aren't the outputs the server's row holds.
    #[error("the pack's files aren't the outputs asked for")]
    Outputs,
    /// Past [`MOST_PACK_BYTES`], or for a hint past [`MOST_HINT_BYTES`]: not shared.
    #[error("the pack is larger than may be shared")]
    TooLarge,
    /// A file changed while it was packed: it isn't what its record says. Nothing is sent.
    #[error("{name:?} changed while it was packed")]
    Changed { name: String },
    /// Reading or writing failed.
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl PackError {
    /// Whether the pack itself is wrong, which a reader reports as `unreadable`: every error but
    /// one of input and output, or of the writer's own files.
    pub fn unreadable(&self) -> bool {
        !matches!(self, PackError::Io(_) | PackError::Changed { .. })
    }

    /// How a log line names the error, in one word.
    pub fn word(&self) -> &'static str {
        match self {
            PackError::PackSize => "pack-size",
            PackError::PackSha => "pack-sha256",
            PackError::Header(_) => "header",
            PackError::Seal(_) => "seal",
            PackError::Decompress => "decompress",
            PackError::Record(_) => "record",
            PackError::FileSize { .. } => "file-size",
            PackError::FileSha { .. } => "file-sha256",
            PackError::Key => "key",
            PackError::Step => "step",
            PackError::Outputs => "outputs",
            PackError::TooLarge => "too-large",
            PackError::Changed { .. } => "changed",
            PackError::Io(_) => "io",
        }
    }
}

/// The description a pack carries: the store's, with what differs from machine to machine and
/// would tell a reader where and when the block was first built taken out: the repository that
/// built it, the date and the compile time. A pack must be the same bytes on every machine that
/// builds the block, and a registry crate's pack opens for anyone who builds that crate. The
/// reader fills these in for its own machine.
pub fn normalize(description: &Value) -> Value {
    let mut out = description.clone();
    if let Value::Object(members) = &mut out {
        for name in ["repository", "built_ms", "rustc_ms"] {
            members.remove(name);
        }
    }
    out
}

/// The digest of the outputs a record lists: [`outputs_digest`] over its files.
pub fn outputs_of(record: &PackRecord) -> Hex32 {
    let files: Vec<OutputFile<'_>> = record
        .files
        .iter()
        .map(|f| OutputFile {
            name: &f.name,
            size: f.size,
            sha256: f.sha256,
        })
        .collect();
    outputs_digest(&files)
}

/// The record's bytes, as a pack holds them: JCS of its JSON, the description normalized.
fn record_bytes(record: &PackRecord) -> Result<Vec<u8>, PackError> {
    let files: Vec<Value> = record
        .files
        .iter()
        .map(|f| json!({"name": f.name, "sha256": f.sha256.to_string(), "size": f.size}))
        .collect();
    let value = json!({
        "schema": RECORD_SCHEMA,
        "key": record.key.to_string(),
        "step": record.step.to_string(),
        "description": normalize(&record.description),
        "files": files,
    });
    let text =
        crate::jcs::canonicalize(&value).map_err(|_| PackError::Record("has no JSON form"))?;
    if text.len() > MOST_RECORD_BYTES {
        return Err(PackError::Record("is too large"));
    }
    Ok(text.into_bytes())
}

/// A writer that hashes and counts what goes through it.
struct Hashing<'a> {
    inner: &'a mut dyn Write,
    hasher: Sha256,
    bytes: u64,
}

impl Write for Hashing<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The first pass of packing: the zstd stream of `record` and its files, written to `out`, and
/// its SHA-256, from which [`seal`] takes the salt. `open_file(i)` gives the bytes of the `i`th
/// file the record lists; each is checked against its size and SHA-256 while it is read, and a
/// file that isn't what the record says ends the pack ([`PackError::Changed`]).
pub fn compress<'f>(
    record: &PackRecord,
    open_file: &mut dyn FnMut(usize) -> io::Result<Box<dyn Read + 'f>>,
    out: &mut dyn Write,
) -> Result<Hex32, PackError> {
    check_record(record)?;
    let bytes = record_bytes(record)?;
    let mut hashing = Hashing {
        inner: out,
        hasher: Sha256::new(),
        bytes: 0,
    };
    let mut encoder = zstd::stream::write::Encoder::new(&mut hashing, LEVEL)?;
    encoder.write_all(&(bytes.len() as u32).to_be_bytes())?;
    encoder.write_all(&bytes)?;
    let mut buf = vec![0u8; 64 * 1024];
    for (index, file) in record.files.iter().enumerate() {
        let changed = || PackError::Changed {
            name: file.name.clone(),
        };
        let mut reader = open_file(index)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err.into()),
            };
            size += n as u64;
            if size > file.size {
                return Err(changed());
            }
            hasher.update(&buf[..n]);
            encoder.write_all(&buf[..n])?;
        }
        if size != file.size || Hex32(hasher.finalize().into()) != file.sha256 {
            return Err(changed());
        }
    }
    encoder.finish()?;
    Ok(Hex32(hashing.hasher.finalize().into()))
}

/// The salt of a stream whose SHA-256 is `stream_sha256`, for `key`.
fn salt(construction: &Construction, key: &[u8; 32], stream_sha256: &Hex32) -> [u8; 32] {
    let mut message = construction.salt_domain.to_vec();
    message.extend_from_slice(&stream_sha256.0);
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), &message);
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// The 32 bytes HKDF gives.
struct Len32;

impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

/// The cipher's key for `key` and `salt`.
fn cipher(construction: &Construction, key: &[u8; 32], salt: &[u8; 32]) -> LessSafeKey {
    let mut secret = [0u8; 32];
    hkdf::Salt::new(hkdf::HKDF_SHA256, salt)
        .extract(key)
        .expand(&[construction.info], Len32)
        .and_then(|okm| okm.fill(&mut secret))
        .expect("32 bytes is a length HKDF-SHA-256 gives");
    LessSafeKey::new(
        UnboundKey::new(&CHACHA20_POLY1305, &secret).expect("a 32-byte key is ChaCha20's"),
    )
}

fn header(construction: &Construction, salt: &[u8; 32]) -> [u8; HEADER_BYTES] {
    let mut out = [0u8; HEADER_BYTES];
    out[..4].copy_from_slice(construction.magic);
    out[4] = VERSION;
    out[5] = CODEC_ZSTD;
    out[6] = CIPHER_CHACHA20_POLY1305;
    out[7..].copy_from_slice(salt);
    out
}

fn nonce(counter: u64, last: bool) -> Nonce {
    let mut bytes = [0u8; aead::NONCE_LEN];
    bytes[3..11].copy_from_slice(&counter.to_be_bytes());
    bytes[11] = u8::from(last);
    Nonce::assume_unique_for_key(bytes)
}

/// Reads up to `buf.len()` bytes, as many as there are: fewer only at the end.
fn fill(reader: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// The second pass of packing: `stream`, the bytes [`compress`] wrote, sealed with `key` into a
/// pack written to `out`. `stream_sha256` is what `compress` answered; the stream is hashed
/// again as it is read, and a stream that isn't those bytes ends with an error, so no two
/// contents are ever sealed under one salt. The caller drops what was written on any error.
pub fn seal(
    key: &[u8; 32],
    stream_sha256: &Hex32,
    stream: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<Sealed, PackError> {
    seal_with(&PACK, key, stream_sha256, stream, out, MOST_PACK_BYTES)
}

fn seal_with(
    construction: &Construction,
    key: &[u8; 32],
    stream_sha256: &Hex32,
    stream: &mut dyn Read,
    out: &mut dyn Write,
    most: u64,
) -> Result<Sealed, PackError> {
    let salt = salt(construction, key, stream_sha256);
    let head = header(construction, &salt);
    let cipher = cipher(construction, key, &salt);
    let mut out = Hashing {
        inner: out,
        hasher: Sha256::new(),
        bytes: 0,
    };
    out.write_all(&head)?;
    let mut read = Sha256::new();
    let mut current = vec![0u8; CHUNK];
    let mut current_len = fill(stream, &mut current)?;
    read.update(&current[..current_len]);
    let mut counter = 0u64;
    loop {
        let mut next = vec![0u8; CHUNK];
        let next_len = if current_len == CHUNK {
            fill(stream, &mut next)?
        } else {
            0
        };
        let last = next_len == 0;
        let mut chunk = current[..current_len].to_vec();
        cipher
            .seal_in_place_append_tag(nonce(counter, last), Aad::from(&head), &mut chunk)
            .map_err(|_| PackError::Seal("the cipher refused a chunk"))?;
        if out.bytes + 4 + chunk.len() as u64 > most {
            return Err(PackError::TooLarge);
        }
        out.write_all(&(chunk.len() as u32).to_be_bytes())?;
        out.write_all(&chunk)?;
        if last {
            break;
        }
        counter += 1;
        read.update(&next[..next_len]);
        current = next;
        current_len = next_len;
    }
    if Hex32(read.finalize().into()) != *stream_sha256 {
        return Err(PackError::Changed {
            name: "the compressed stream".to_string(),
        });
    }
    out.flush()?;
    Ok(Sealed {
        bytes: out.bytes,
        sha256: Hex32(out.hasher.finalize().into()),
    })
}

/// A whole pack in memory, from `record` and the bytes of its files in order: what tests and
/// the fuzz target use.
pub fn pack_in_memory(
    key: &[u8; 32],
    record: &PackRecord,
    files: &[&[u8]],
) -> Result<Vec<u8>, PackError> {
    let mut stream = Vec::new();
    let sha = compress(
        record,
        &mut |i| {
            let bytes: &[u8] = files.get(i).copied().unwrap_or_default();
            Ok(Box::new(bytes) as Box<dyn Read>)
        },
        &mut stream,
    )?;
    let mut pack = Vec::new();
    seal(key, &sha, &mut stream.as_slice(), &mut pack)?;
    Ok(pack)
}

/// The record's names are each a plain relative path, once, none a folder of another; within
/// the bounds.
fn check_record(record: &PackRecord) -> Result<(), PackError> {
    if record.files.len() > MOST_FILES {
        return Err(PackError::Record("lists too many files"));
    }
    let mut total = 0u64;
    let mut names = BTreeSet::new();
    for file in &record.files {
        if !plain_name(&file.name) {
            return Err(PackError::Record("names a file outside the block"));
        }
        if !names.insert(file.name.as_str()) {
            return Err(PackError::Record("names a file twice"));
        }
        total = total.saturating_add(file.size);
    }
    if total > MOST_FILE_BYTES {
        return Err(PackError::Record("holds too many bytes"));
    }
    // No name is a folder on the way to another: `a` and `a/b` can't both be files.
    for name in &names {
        let parts: Vec<&str> = name.split('/').collect();
        for end in 1..parts.len() {
            if names.contains(parts[..end].join("/").as_str()) {
                return Err(PackError::Record("names a file that is another's folder"));
            }
        }
    }
    Ok(())
}

/// A block's file name: plain parts joined by `/`, none empty, `.` or `..`, no backslash or NUL,
/// not absolute.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// A reader that hashes and counts every byte read through it.
struct Counted<'a> {
    inner: &'a mut dyn Read,
    hasher: Sha256,
    bytes: u64,
}

impl Read for Counted<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// The plaintext of a pack's chunks, one after the other. An error of the seal is kept in
/// `failed`, so the caller can tell it from the stream's.
struct Chunks<'a, 'b> {
    input: &'b mut Counted<'a>,
    head: [u8; HEADER_BYTES],
    cipher: LessSafeKey,
    counter: u64,
    /// The length of the next chunk, read ahead to know whether the one before is the last.
    pending: Option<u32>,
    done: bool,
    plain: Vec<u8>,
    at: usize,
    failed: Rc<RefCell<Option<PackError>>>,
}

impl Chunks<'_, '_> {
    fn fail(&self, why: &'static str) -> io::Error {
        *self.failed.borrow_mut() = Some(PackError::Seal(why));
        io::Error::other(why)
    }

    /// The next 4-byte length, `None` at the end of the input.
    fn length(&mut self) -> io::Result<Option<u32>> {
        let mut bytes = [0u8; 4];
        match fill(self.input, &mut bytes)? {
            0 => Ok(None),
            4 => Ok(Some(u32::from_be_bytes(bytes))),
            _ => Err(self.fail("it is cut short")),
        }
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        let len = match self.pending.take() {
            Some(len) => len,
            None if self.counter == 0 => match self.length()? {
                Some(len) => len,
                None => return Err(self.fail("it has no chunk")),
            },
            None => return Err(self.fail("it is cut short")),
        } as usize;
        if !(TAG..=CHUNK + TAG).contains(&len) {
            return Err(self.fail("a chunk has a length no writer gives"));
        }
        let mut chunk = vec![0u8; len];
        if fill(self.input, &mut chunk)? != len {
            return Err(self.fail("it is cut short"));
        }
        self.pending = self.length()?;
        let last = self.pending.is_none();
        let opened = self
            .cipher
            .open_in_place(nonce(self.counter, last), Aad::from(&self.head), &mut chunk)
            .map_err(|_| self.fail("a chunk doesn't open with this key"))?
            .len();
        if !last && opened != CHUNK {
            return Err(self.fail("a chunk before the last isn't whole"));
        }
        chunk.truncate(opened);
        self.plain = chunk;
        self.at = 0;
        self.counter += 1;
        self.done = last;
        Ok(())
    }
}

impl Read for Chunks<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at == self.plain.len() {
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = buf.len().min(self.plain.len() - self.at);
        buf[..n].copy_from_slice(&self.plain[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// The error behind an IO error of the stream: the seal's, kept by [`Chunks`], or the stream's
/// own.
fn stream_error(
    err: io::Error,
    failed: &Rc<RefCell<Option<PackError>>>,
    otherwise: PackError,
) -> PackError {
    match failed.borrow_mut().take() {
        Some(seal) => seal,
        None if err.kind() == io::ErrorKind::Other || err.kind() == io::ErrorKind::InvalidData => {
            otherwise
        }
        None => PackError::Io(err),
    }
}

/// Opens a pack: reads `input` to its end, writes the block's files to `sink`, and answers its
/// record once every check passed. The checks, in the order they decide when several fail: the
/// pack's size and SHA-256 when `expect` names them, the header and the seal, the stream and the
/// record, each file's size and SHA-256 against the record, the record's key and first key
/// against `expect`, and the digest of the files against `expect.outputs`. Nothing written to
/// `sink` may be kept unless this answers `Ok`.
pub fn open(
    input: &mut dyn Read,
    expect: &Expect,
    sink: &mut dyn Sink,
) -> Result<PackRecord, PackError> {
    let mut counted = Counted {
        inner: input,
        hasher: Sha256::new(),
        bytes: 0,
    };
    let opened = open_counted(&mut counted, expect, sink);
    if expect.pack_sha256.is_some() || expect.bytes.is_some() {
        // A wrong pack is said to be wrong before anything it holds is judged: read the rest.
        let mut rest = [0u8; 64 * 1024];
        loop {
            match counted.read(&mut rest) {
                Ok(0) => break,
                Ok(_) => {
                    if counted.bytes > MOST_PACK_BYTES {
                        break;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(PackError::Io(err)),
            }
        }
        if expect.bytes.is_some_and(|bytes| bytes != counted.bytes) {
            return Err(PackError::PackSize);
        }
        let sha = Hex32(counted.hasher.clone().finalize().into());
        if expect.pack_sha256.is_some_and(|expected| expected != sha) {
            return Err(PackError::PackSha);
        }
    }
    opened
}

fn open_counted(
    counted: &mut Counted<'_>,
    expect: &Expect,
    sink: &mut dyn Sink,
) -> Result<PackRecord, PackError> {
    let mut head = [0u8; HEADER_BYTES];
    if fill(counted, &mut head)? != HEADER_BYTES {
        return Err(PackError::Header("is cut short"));
    }
    if &head[..4] != MAGIC {
        return Err(PackError::Header("isn't a pack's"));
    }
    if head[4] != VERSION {
        return Err(PackError::Header(
            "is of a version this hushrun doesn't read",
        ));
    }
    if head[5] != CODEC_ZSTD || head[6] != CIPHER_CHACHA20_POLY1305 {
        return Err(PackError::Header(
            "names a codec or a cipher this hushrun doesn't read",
        ));
    }
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&head[7..]);
    let failed = Rc::new(RefCell::new(None));
    let mut chunks = Chunks {
        input: counted,
        head,
        cipher: cipher(&PACK, &expect.key, &salt),
        counter: 0,
        pending: None,
        done: false,
        plain: Vec::new(),
        at: 0,
        failed: failed.clone(),
    };
    // The first chunk opens before zstd sees anything: a wrong key is the seal's error.
    if let Err(err) = chunks.next_chunk() {
        return Err(stream_error(err, &failed, PackError::Decompress));
    }
    let result = read_stream(&mut chunks, &failed, sink);
    let record = match result {
        Ok(record) => record,
        Err(err) => {
            // The seal decides first: open every chunk left, so a later bad tag is said as such.
            let mut rest = [0u8; 64 * 1024];
            loop {
                match chunks.read(&mut rest) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => {
                        if let Some(seal) = failed.borrow_mut().take() {
                            return Err(seal);
                        }
                        break;
                    }
                }
            }
            return Err(err);
        }
    };
    if record.key.0 != expect.key {
        return Err(PackError::Key);
    }
    if expect.step.is_some_and(|step| step != record.step) {
        return Err(PackError::Step);
    }
    if expect
        .outputs
        .is_some_and(|outputs| outputs != outputs_of(&record))
    {
        return Err(PackError::Outputs);
    }
    Ok(record)
}

/// The stream inside the chunks: the record, then each file, checked against the record, then
/// its end, and the end of the chunks.
fn read_stream(
    chunks: &mut Chunks<'_, '_>,
    failed: &Rc<RefCell<Option<PackError>>>,
    sink: &mut dyn Sink,
) -> Result<PackRecord, PackError> {
    let mut decoder = zstd::stream::read::Decoder::new(&mut *chunks)
        .map_err(|err| stream_error(err, failed, PackError::Decompress))?;
    let read_exact = |decoder: &mut dyn Read, buf: &mut [u8]| -> Result<usize, PackError> {
        fill(decoder, buf).map_err(|err| stream_error(err, failed, PackError::Decompress))
    };
    let mut len = [0u8; 4];
    if read_exact(&mut decoder, &mut len)? != 4 {
        return Err(PackError::Record("is missing"));
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MOST_RECORD_BYTES {
        return Err(PackError::Record("is too large"));
    }
    let mut bytes = vec![0u8; len];
    if read_exact(&mut decoder, &mut bytes)? != len {
        return Err(PackError::Record("is cut short"));
    }
    let record = parse_record(&bytes)?;
    let mut buf = vec![0u8; 64 * 1024];
    for (index, file) in record.files.iter().enumerate() {
        sink.file(index, &file.name, file.size)?;
        let mut left = file.size;
        let mut hasher = Sha256::new();
        while left > 0 {
            let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let n = read_exact(&mut decoder, &mut buf[..want])?;
            if n == 0 {
                return Err(PackError::FileSize {
                    name: file.name.clone(),
                });
            }
            hasher.update(&buf[..n]);
            sink.write(&buf[..n])?;
            left -= n as u64;
        }
        if Hex32(hasher.finalize().into()) != file.sha256 {
            return Err(PackError::FileSha {
                name: file.name.clone(),
            });
        }
    }
    // Nothing after the files, in the stream or in the chunks.
    let mut more = [0u8; 1];
    if read_exact(&mut decoder, &mut more)? != 0 {
        return Err(PackError::Record("is followed by more than its files"));
    }
    drop(decoder);
    let mut rest = [0u8; 1];
    match chunks.read(&mut rest) {
        Ok(0) => Ok(record),
        Ok(_) => Err(PackError::Decompress),
        Err(err) => Err(stream_error(err, failed, PackError::Decompress)),
    }
}

fn parse_record(bytes: &[u8]) -> Result<PackRecord, PackError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| PackError::Record("isn't JSON"))?;
    if value.get("schema").and_then(Value::as_str) != Some(RECORD_SCHEMA) {
        return Err(PackError::Record(
            "is of a schema this hushrun doesn't read",
        ));
    }
    let hex = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .and_then(Hex32::parse)
            .ok_or(PackError::Record("lacks a key"))
    };
    let key = hex("key")?;
    let step = hex("step")?;
    let description = value.get("description").cloned().unwrap_or(Value::Null);
    let listed = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or(PackError::Record("lists no files"))?;
    if listed.len() > MOST_FILES {
        return Err(PackError::Record("lists too many files"));
    }
    let mut files = Vec::with_capacity(listed.len());
    for entry in listed {
        let name = entry.get("name").and_then(Value::as_str);
        let size = entry.get("size").and_then(Value::as_u64);
        let sha256 = entry
            .get("sha256")
            .and_then(Value::as_str)
            .and_then(Hex32::parse);
        let (Some(name), Some(size), Some(sha256)) = (name, size, sha256) else {
            return Err(PackError::Record("lists a file it doesn't describe"));
        };
        files.push(PackFile {
            name: name.to_string(),
            size,
            sha256,
        });
    }
    let record = PackRecord {
        key,
        step,
        description,
        files,
    };
    check_record(&record)?;
    Ok(record)
}

/// A hint sealed with its block's first key, as `POST v0/build/hints` sends it: the same seal as
/// a pack's, with its own magic, info and salt domain, over the zstd stream of the hint's bytes.
/// At most [`MOST_HINT_BYTES`]: a larger one isn't shared.
pub fn seal_hint(first_key: &[u8; 32], hint: &[u8]) -> Result<Vec<u8>, PackError> {
    let stream = zstd::stream::encode_all(hint, LEVEL)?;
    let sha = Hex32::sha256(&stream);
    let mut out = Vec::new();
    match seal_with(
        &HINT,
        first_key,
        &sha,
        &mut stream.as_slice(),
        &mut out,
        MOST_HINT_BYTES as u64,
    ) {
        Ok(_) => Ok(out),
        Err(err) => Err(err),
    }
}

/// The bytes of a hint [`seal_hint`] sealed, or `None` when it doesn't open with `first_key` or
/// isn't one.
pub fn open_hint(first_key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() > MOST_HINT_BYTES || sealed.len() < HEADER_BYTES {
        return None;
    }
    let (head, rest) = sealed.split_at(HEADER_BYTES);
    if &head[..4] != HINT_MAGIC
        || head[4] != VERSION
        || head[5] != CODEC_ZSTD
        || head[6] != CIPHER_CHACHA20_POLY1305
    {
        return None;
    }
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&head[7..]);
    let mut header = [0u8; HEADER_BYTES];
    header.copy_from_slice(head);
    let mut input = rest;
    let mut counted = Counted {
        inner: &mut input,
        hasher: Sha256::new(),
        bytes: 0,
    };
    let mut chunks = Chunks {
        input: &mut counted,
        head: header,
        cipher: cipher(&HINT, first_key, &salt),
        counter: 0,
        pending: None,
        done: false,
        plain: Vec::new(),
        at: 0,
        failed: Rc::new(RefCell::new(None)),
    };
    let mut stream = Vec::new();
    chunks.read_to_end(&mut stream).ok()?;
    let mut decoder = zstd::stream::read::Decoder::new(stream.as_slice()).ok()?;
    let mut plain = Vec::new();
    let read = (&mut decoder)
        .take(MOST_HINT_PLAIN as u64 + 1)
        .read_to_end(&mut plain)
        .ok()?;
    (read <= MOST_HINT_PLAIN).then_some(plain)
}

#[cfg(test)]
mod tests;
