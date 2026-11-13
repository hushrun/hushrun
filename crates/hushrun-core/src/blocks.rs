//! The shared store's wire types: what a machine and a backend say to each other about build
//! blocks, under an org's base URL (`{base}/v0/build/…`). One definition, used by the CLI's client
//! and by the backend that serves it.
//!
//! The backend never sees a key, a path or a crate name. It sees an **address** (32 bytes, written
//! as 64 lowercase hex characters), a **kind** (`rustc/1`: the adapter and its key schema), and
//! sealed bytes it can't open. Every request is a batch, every one may be repeated, and every
//! request and answer is a JSON object whose members are camelCase. A member this version doesn't
//! know is ignored, and an answer's word this version doesn't know reads as `unknown`, so either
//! side can add one.
//!
//! | Request | Body | Answer |
//! |---|---|---|
//! | `POST v0/build/open` | [`OpenRequest`] | [`Policy`] |
//! | `POST v0/build/hints/lookup` | [`HintsLookupRequest`] | [`HintsLookupResponse`] |
//! | `POST v0/build/hints` | [`HintsRequest`] | [`HintsResponse`] |
//! | `POST v0/build/blocks/lookup` | [`LookupRequest`] | [`LookupResponse`] |
//! | `POST v0/build/blocks/begin` | [`BeginRequest`], signed: [`begin_message`] | [`BeginResponse`] |
//! | `PUT <link>` | the pack, to the object store, as [`Link`] says | |
//! | `POST v0/build/blocks/commit` | [`CommitRequest`] | [`CommitResponse`] |
//! | `POST v0/build/close` | [`CloseRequest`] | `{}` |
//! | `POST v0/build/blocks/report` | [`ReportRequest`] | `{}` |
//!
//! A server with no object store has none of these routes. An org that doesn't share answers an
//! authenticated request to any of them as a route that doesn't exist (404), as a server from before
//! them does. Whether to ask `open` at all, a results lookup's answer says in its
//! [`SHARING_HEADER`]. A request that lost a race in the server's database answers 503 with
//! `Retry-After`: send it again.
//!
//! Like the rest of this crate it does no IO.

use std::collections::BTreeMap;
use std::fmt;

use base64::Engine as _;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

/// The header a results lookup's answer carries, `on`, when the org shares build blocks.
pub const SHARING_HEADER: &str = "hushrun-build";

/// The paths, under an org's base URL.
pub const OPEN_PATH: &str = "v0/build/open";
pub const HINTS_LOOKUP_PATH: &str = "v0/build/hints/lookup";
pub const HINTS_PATH: &str = "v0/build/hints";
pub const LOOKUP_PATH: &str = "v0/build/blocks/lookup";
pub const BEGIN_PATH: &str = "v0/build/blocks/begin";
pub const COMMIT_PATH: &str = "v0/build/blocks/commit";
pub const CLOSE_PATH: &str = "v0/build/close";
pub const REPORT_PATH: &str = "v0/build/blocks/report";

/// The most addresses one lookup, of blocks or of hints, asks for.
pub const MOST_LOOKUP: usize = 500;
/// The most blocks one `begin` or `commit` names.
pub const MOST_BEGIN: usize = 200;
/// The most hints one `POST v0/build/hints` sends, and their bytes together.
pub const MOST_HINTS_SENT: usize = 200;
pub const MOST_HINTS_SENT_BYTES: usize = 1024 * 1024;
/// The largest hint (a sealed read list): 256 KiB.
pub const MOST_HINT_BYTES: usize = 256 * 1024;
/// The most hints kept, and answered, for one address in one scope.
pub const MOST_HINTS_PER_ADDRESS: usize = 4;
/// The largest pack: 2 GiB. A larger block is not shared.
pub const MOST_PACK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// The most entries in a run's manifest: 16,384 pairs of a hint address and a block address
/// (1 MiB of addresses). A build of a large workspace makes a few thousand compiles.
pub const MOST_MANIFEST: usize = 16 * 1024;

/// What the signature on a `begin` signs, before the org and the body's SHA-256.
pub const BEGIN_SIGNATURE_SCHEMA: &str = "hushrun-build-begin/v1";

/// What `Hushrun-Signature` signs on a `begin` sent to org `org` with body `body`: a machine's
/// word that it wrote the blocks it names. `hushrun-build-begin/v1\n<org>\n<SHA-256 of the body,
/// hex>\n`.
pub fn begin_message(org: &str, body: &[u8]) -> String {
    format!(
        "{BEGIN_SIGNATURE_SCHEMA}\n{org}\n{}\n",
        to_hex(&Sha256::digest(body))
    )
}

/// Whether `text` is a kind: a lowercase word of 1 to 16 characters (`a`-`z` first, then `a`-`z`,
/// `0`-`9` or `-`), a `/`, and the version of its keys, 1 to 4 digits. `rustc/1`.
pub fn is_kind(text: &str) -> bool {
    let Some((name, version)) = text.split_once('/') else {
        return false;
    };
    let name_ok = (1..=16).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let version_ok =
        (1..=4).contains(&version.len()) && version.bytes().all(|b| b.is_ascii_digit());
    name_ok && version_ok
}

/// The schema line an address is taken under.
pub const ADDRESS_SCHEMA: &str = "hushrun.block-address/v1";

/// The address of a key of kind `kind`: what the backend knows a block (or, for a first key, its
/// hints) by, since it never sees the key itself. The SHA-256 of
/// `hushrun.block-address/v1\n<kind>\n` followed by the key's 32 bytes. A hint's address is the
/// address of the block's first key, with the same kind.
pub fn address(kind: &str, key: &[u8; 32]) -> Hex32 {
    let mut hasher = Sha256::new();
    hasher.update(ADDRESS_SCHEMA.as_bytes());
    hasher.update(b"\n");
    hasher.update(kind.as_bytes());
    hasher.update(b"\n");
    hasher.update(key);
    Hex32(hasher.finalize().into())
}

/// Whether `text` is a platform: a target triple, 1 to 64 characters of `a`-`z`, `0`-`9`, `_`,
/// `.` and `-`.
pub fn is_platform(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        })
}

/// 32 bytes, written as 64 lowercase hex characters: an address, a pack's SHA-256, a digest of a
/// block's outputs. Anything else fails to read.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Hex32(pub [u8; 32]);

impl Hex32 {
    /// The 64 lowercase hex characters of `text`, or `None`.
    pub fn parse(text: &str) -> Option<Hex32> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks(2).enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(Hex32(out))
    }

    /// The SHA-256 of `bytes`.
    pub fn sha256(bytes: &[u8]) -> Hex32 {
        Hex32(Sha256::digest(bytes).into())
    }
}

fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    out
}

impl fmt::Display for Hex32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&to_hex(&self.0))
    }
}

impl fmt::Debug for Hex32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hex32({self})")
    }
}

impl Serialize for Hex32 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&to_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for Hex32 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Hex32, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Hex32::parse(&text).ok_or_else(|| D::Error::custom("expected 64 lowercase hex characters"))
    }
}

/// Bytes, written as standard base64 (RFC 4648 §4, padded): a sealed hint.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Base64(pub Vec<u8>);

impl fmt::Debug for Base64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Base64({} bytes)", self.0.len())
    }
}

impl Serialize for Base64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Base64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Base64, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map(Base64)
            .map_err(|_| D::Error::custom("expected standard base64"))
    }
}

/// What a block is, as the server keeps it. A word this version doesn't know reads as
/// [`Class::Unknown`]: a server refuses only that block (`unsupported`), and a client reads the rest
/// of the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    Library,
    ProcMacro,
    BuildScript,
    Program,
    Other,
    #[serde(other)]
    Unknown,
}

impl Class {
    /// The word on the wire and in the server's rows.
    pub fn word(self) -> &'static str {
        match self {
            Class::Library => "library",
            Class::ProcMacro => "proc-macro",
            Class::BuildScript => "build-script",
            Class::Program => "program",
            Class::Other => "other",
            Class::Unknown => "unknown",
        }
    }

    /// The class a word names, among those a server keeps.
    pub fn from_word(word: &str) -> Option<Class> {
        [
            Class::Library,
            Class::ProcMacro,
            Class::BuildScript,
            Class::Program,
            Class::Other,
        ]
        .into_iter()
        .find(|c| c.word() == word)
    }
}

/// A link to the object store, signed by the backend: the method, the URL, and the headers the
/// request must carry exactly as given, since the signature covers them. The backend's token is
/// never sent with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// How long the link works from when the answer was sent, in seconds.
    pub expires_in_seconds: u32,
}

/// Whether `text` is a commit as a workspace or a token names it: 40 to 64 lowercase hex
/// characters.
pub fn is_commit(text: &str) -> bool {
    (40..=64).contains(&text.len()) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// One file of a block, as the digest of its outputs reads it: its name in the block, its size
/// and its SHA-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFile<'a> {
    pub name: &'a str,
    pub size: u64,
    pub sha256: Hex32,
}

/// The one function for a block's digest of outputs: the SHA-256 of the JCS form (RFC 8785) of
/// the list of its files sorted by name (bytewise), each `{"name", "sha256", "size"}`, the
/// SHA-256 as 64 lowercase hex characters. Nothing else goes in: not the time, the machine or the
/// repository, which differ on every machine. A machine computes it when it uploads, and again
/// from the files it unpacked before it reuses a download; a server never computes it.
pub fn outputs_digest(files: &[OutputFile<'_>]) -> Hex32 {
    let mut sorted: Vec<&OutputFile<'_>> = files.iter().collect();
    sorted.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let list: Vec<serde_json::Value> = sorted
        .into_iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "sha256": f.sha256.to_string(),
                "size": f.size,
            })
        })
        .collect();
    let canonical = crate::jcs::canonicalize(&serde_json::Value::Array(list))
        .expect("names, sizes and hex strings always have a canonical form");
    Hex32::sha256(canonical.as_bytes())
}

/// One compile of a build, as a manifest lists it: the address of its first key (which hints go
/// by) and the address of its block. Written as a two-element array of 64-hex strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pair(pub Hex32, pub Hex32);

impl Pair {
    pub fn hint(&self) -> Hex32 {
        self.0
    }
    pub fn block(&self) -> Hex32 {
        self.1
    }
}

// --- open --------------------------------------------------------------------------------------

/// `POST v0/build/open`: once per run, before anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub kind: String,
    /// The repository the run is in, as a result names it (`github.com/acme/shop`). For a person
    /// or an agent token the server reads it as the one repository connected under that name now,
    /// or none: it chooses whose trusted blocks the run reads, and which manifest comes back. CI's
    /// comes from its ID token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The commit the workspace has checked out (`git rev-parse HEAD`), or `None` when the client
    /// can't read one. In CI, a job whose commit isn't its token's `sha` is not trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The check's id, as the local store names it: a digest of the repository and the check's
    /// name. It chooses the manifest that comes back: an `open` naming a check gets the newest
    /// manifest kept for that check (of the repository and platform), or none; one naming none
    /// (`hushrun build fetch` with no check) gets the repository's newest for the platform,
    /// whatever its check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Hex32>,
    /// The target triple the run builds for.
    pub platform: String,
}

/// Why a shared store is off for a run although the org shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffReason {
    /// The org's plan includes no shared store.
    Plan,
    #[serde(other)]
    Unknown,
}

/// The org's shared mode, as its admin set it: it starts at `observe`. A person's machine may
/// lower it for itself, never raise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Upload what is built, download and compare, never reuse a download.
    Observe,
    /// Reuse.
    On,
    #[serde(other)]
    Unknown,
}

/// Why a CI job on a branch its repository lists is not trusted, so the run can say it in a line.
/// Such a job writes nothing and reads its repository's trusted blocks alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Untrusted {
    /// The repository's owner id isn't the one an admin confirmed.
    Repository,
    /// The event isn't `push` or `schedule`, nor a `workflow_dispatch` the repository allows.
    Event,
    /// The job names a deployment environment.
    Environment,
    /// The workspace's commit isn't the token's `sha`, or the client couldn't read one.
    Commit,
    /// A self-hosted runner the repository hasn't opted in (or whose opt-in lapsed, or the
    /// repository isn't private).
    Runner,
    /// The run started before the branch was listed, was withdrawn by an admin, or was flagged
    /// for suspending too many keys.
    Run,
    #[serde(other)]
    Unknown,
}

/// The limits a client keeps to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub lookup: u32,
    pub begin: u32,
    pub hints_sent: u32,
    pub hints_sent_bytes: u32,
    pub hint_bytes: u32,
    pub pack_bytes: u64,
    pub manifest: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            lookup: MOST_LOOKUP as u32,
            begin: MOST_BEGIN as u32,
            hints_sent: MOST_HINTS_SENT as u32,
            hints_sent_bytes: MOST_HINTS_SENT_BYTES as u32,
            hint_bytes: MOST_HINT_BYTES as u32,
            pack_bytes: MOST_PACK_BYTES,
            manifest: MOST_MANIFEST as u32,
        }
    }
}

/// `open`'s answer: whether this run shares, where it writes and what it reads, and the newest
/// manifest of a build like this one that it may read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub sharing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<OffReason>,
    /// The org's shared mode. Every caller gets it; a person's machine may lower it for itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// Whether this caller uploads at all: the same as `write` being there.
    #[serde(default)]
    pub writes: bool,
    /// For a CI job on a branch its repository lists that is not trusted: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untrusted: Option<Untrusted>,
    /// The scope this caller writes; `None`: it writes nothing (a job on a tag, say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<String>,
    /// The scopes it reads, in the order a lookup prefers them: its repository's trusted blocks
    /// first, then its own scope.
    #[serde(default)]
    pub read: Vec<String>,
    /// How long a block is kept after it was last used.
    pub kept_days: u32,
    #[serde(default)]
    pub limits: Limits,
    /// What the org's shared store may hold, and holds, in bytes.
    pub allowance_bytes: u64,
    pub held_bytes: u64,
    /// The compiles of the newest build like this one, in order.
    #[serde(default)]
    pub manifest: Vec<Pair>,
}

// --- hints -------------------------------------------------------------------------------------

/// `POST v0/build/hints/lookup`: the sealed read lists kept for first keys' addresses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HintsLookupRequest {
    pub kind: String,
    pub addresses: Vec<Hex32>,
    /// The run's repository, as in [`OpenRequest::repository`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The workspace's commit, as in [`OpenRequest::commit`]: what a CI job reads is decided
    /// again for every request. A server that doesn't read it ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundHints {
    pub address: Hex32,
    /// Newest first, at most [`MOST_HINTS_PER_ADDRESS`].
    pub lists: Vec<Base64>,
}

/// The addresses that have hints, each once; an address with none isn't listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HintsLookupResponse {
    pub hints: Vec<FoundHints>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentHint {
    pub address: Hex32,
    pub list: Base64,
}

/// `POST v0/build/hints`: sealed read lists, into the caller's own scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HintsRequest {
    pub kind: String,
    pub hints: Vec<SentHint>,
    /// The workspace's commit, as in [`OpenRequest::commit`]: where a CI job writes is decided
    /// again for every request that writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HintsResponse {
    pub stored: u32,
}

// --- blocks ------------------------------------------------------------------------------------

/// `POST v0/build/blocks/lookup`: which of these the shared store holds in a scope the caller
/// reads. This is a block's use: it keeps the block alive, with or without `fetch`. With `fetch`,
/// each found block comes with a link to download it. Without, the answer is how a machine learns
/// that the server still vouches for a block it holds: the scope and the digest of outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LookupRequest {
    pub kind: String,
    pub addresses: Vec<Hex32>,
    #[serde(default)]
    pub fetch: bool,
    /// The run's repository, as in [`OpenRequest::repository`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The key id of the machine asking (`sha256:<hex>`), so the answer can say which blocks it
    /// wrote ([`Found::mine`]). Only the caller's own machines count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// The workspace's commit, as in [`OpenRequest::commit`]: what a CI job reads is decided
    /// again for every request. A server that doesn't read it ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub address: Hex32,
    pub scope: String,
    /// The pack's size and SHA-256: a reader checks both before it opens the pack.
    pub bytes: u64,
    pub pack_sha256: Hex32,
    pub outputs: Hex32,
    pub cost_ms: u32,
    pub class: Class,
    /// Whether the caller's own machine (for CI, its own run) wrote it.
    #[serde(default)]
    pub mine: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get: Option<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LookupResponse {
    pub found: Vec<Found>,
    /// Asked addresses whose first key is suspended in the repository: not served.
    #[serde(default)]
    pub suspended: Vec<Hex32>,
}

/// A block a machine built and would upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginBlock {
    pub address: Hex32,
    /// The address of its first key, which hints and suspensions go by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_address: Option<Hex32>,
    pub bytes: u64,
    pub pack_sha256: Hex32,
    pub outputs: Hex32,
    pub class: Class,
    pub cost_ms: u32,
}

/// `POST v0/build/blocks/begin`: the blocks a machine would upload. From a person's or an agent
/// token's machine it carries `Hushrun-Signature`, over [`begin_message`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginRequest {
    pub kind: String,
    pub blocks: Vec<BeginBlock>,
    /// The run's repository, as in [`OpenRequest::repository`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The workspace's commit, as in [`OpenRequest::commit`]: trust is decided again for every
    /// `begin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Why a block won't be taken. It stays on the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The org's shared store is full, and `begin` couldn't make room, until the next sweep does.
    OverAllowance,
    /// Past [`MOST_PACK_BYTES`].
    TooLarge,
    /// Its first key is suspended in the repository.
    Suspended,
    /// A scope the caller reads holds the same key with other outputs (the trusted copy first):
    /// discard the local block. For a trusted job, another trusted build disagreed.
    Differs,
    /// Another upload of it is under way, or it is being deleted.
    Busy,
    /// This caller writes nowhere: no scope of its own, or (for CI) a run that was withdrawn or
    /// flagged.
    NoScope,
    /// The caller's scope has as many uploads under way as it may: send or let them lapse first.
    TooManyUploads,
    /// The server doesn't take blocks of this class.
    Unsupported,
    #[serde(other)]
    Unknown,
}

/// What `begin` says of one block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    /// Upload the pack with this link, then `commit`.
    Put {
        link: Link,
    },
    /// A scope the caller reads holds it already.
    Have {
        scope: String,
    },
    Refused {
        reason: Refusal,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginAnswer {
    pub address: Hex32,
    #[serde(flatten)]
    pub answer: Answer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginResponse {
    pub blocks: Vec<BeginAnswer>,
}

/// `POST v0/build/blocks/commit`: blocks whose packs were uploaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitRequest {
    pub kind: String,
    pub addresses: Vec<Hex32>,
    /// The workspace's commit, as in [`OpenRequest::commit`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Where a block stands after `commit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitState {
    /// Kept: lookups answer it.
    Stored,
    /// The object store has no pack for it yet: upload, then commit again.
    Missing,
    /// The object store holds other bytes than `begin` named. They are deleted.
    Differs,
    /// No upload of it is under way for this caller: never begun, or given up.
    Gone,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Committed {
    pub address: Hex32,
    pub state: CommitState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResponse {
    pub blocks: Vec<Committed>,
}

// --- close and report --------------------------------------------------------------------------

/// What a run did, as its machine counted it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Counts {
    pub reused_local: u64,
    pub reused_shared: u64,
    pub built: u64,
    pub uploaded: u64,
    pub bytes_down: u64,
    pub bytes_up: u64,
    pub saved_ms: u64,
    /// Packs downloaded that no compile asked for.
    pub unused_downloads: u64,
    /// Downloads compared with a local build of the same key (shared `observe`, or an audit).
    pub compared: u64,
    /// Of those, the comparisons of blocks another machine built.
    pub compared_others: u64,
    /// Of `compared`, how many differed. A server from before it ignores it, and a close from a
    /// client from before it reads as 0.
    pub compared_differed: u64,
}

/// `POST v0/build/close`: once per run, at its end. Sent again with the same `run`, it counts
/// once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseRequest {
    pub kind: String,
    /// 32 random bytes the run chose, the same in every try.
    pub run: Hex32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Hex32>,
    pub platform: String,
    /// The workspace's commit, as in [`OpenRequest::commit`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The run's compiles, in order, each once: whether reused locally, downloaded or built. The
    /// server keeps the one it has instead when this one has under half its entries and that one
    /// is under 7 days old.
    #[serde(default)]
    pub manifest: Vec<Pair>,
    #[serde(default)]
    pub counts: Counts,
}

/// What was wrong with a downloaded block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// It opened, and a fresh compile made other outputs.
    Differs,
    /// It didn't open: a wrong size or SHA-256, a seal that failed, a broken pack.
    Unreadable,
    /// The object store had nothing at its link.
    Missing,
    /// It opened, but a compile that depends on it couldn't load it.
    Rejected,
    /// A word this version doesn't know: a server records nothing for it.
    #[serde(other)]
    Unknown,
}

/// `POST v0/build/blocks/report`: a block that a machine found wrong, in a scope it reads. A
/// report is recorded, once per reporter, block and day, only for a block the scope holds, and a
/// reporter's reports are counted only so many a day. A block of the caller's own scope is
/// deleted. A repository's trusted block is deleted, and for `differs` its first key suspended in
/// the repository, only when a trusted job of that repository reports it; anyone else's report
/// changes nothing on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRequest {
    pub kind: String,
    pub address: Hex32,
    pub scope: String,
    pub problem: Problem,
    /// The run's repository, as in [`OpenRequest::repository`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The workspace's commit, as in [`OpenRequest::commit`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[cfg(feature = "pack")]
pub mod pack;

#[cfg(test)]
mod tests;
