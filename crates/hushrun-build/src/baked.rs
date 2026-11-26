//! A baked store (B-63, B-64): what `hushrun build bake` writes for one repository and one
//! platform, and how a run reads it as a read-only store underneath its own.
//!
//! The folder holds these and nothing else ([`check_contents`]):
//!
//! | Path | What |
//! |---|---|
//! | `baked.json` | [`Baked`]: one entry per check it was baked for, each with one part per kind and platform (`rustc/1`, `go/1`, `cc/1`) and the manifest it was baked from, the claim of trust, and one row per pack, each pack once |
//! | `packs/<address>.pack` | a block's sealed pack, `hushrun.block-pack/v1`, exactly as it travels to and from a backend (B-28) |
//! | `hints/<first-key address>/<SHA-256 of the sealed bytes>` | one sealed hint of that first key (B-27) |
//! | `baked.sigstore.json` | optional: its signature, a Sigstore bundle (B-67) |
//!
//! Its state for a run is one function's answer, [`state`], which the run, `hushrun build
//! status`, the relay and `hushrun build bake` all call:
//!
//! | State | Means | A run |
//! |---|---|---|
//! | absent | no baked store is named (`HUSHRUN_BUILD_LOWER` unset or empty) | reads nothing |
//! | invalid | the folder, `baked.json` or a name in it can't be read, is of a version this one doesn't know, or claims what can't be (a machine's own store trusted, a trusted row outside the trusted scope, a pack named twice) | ignores it, one line |
//! | wrong repository | baked for another repository than the run's | ignores it, one line |
//! | wrong platform | no part of it is for a platform this run builds for | ignores it, one line |
//! | server decides | only `hushrun build status` and `bake` say it: the evidence is the backend's answer, which only a run asks for | (whatever that answer gives) |
//! | untrusted | nothing here verifies it (no answer from the backend it names, no signature by a signer the repository names) and the run may not accept an unverified store | ignores it, one line |
//! | replaced | its source names a newer manifest for one of its checks | reads it; a block from it is reused only as a stale download is (B-34) |
//! | current | none of the above | reads it, as its evidence says |
//!
//! Trust is never read from the file: `baked.json`'s `trust` is the baker's claim, shown and
//! checked for what can't be, and what lets a run reuse a block unasked is **evidence**: the
//! backend vouching for the block in the run's repository's trusted scope (a lookup the relay
//! makes as the run begins), a signature by a signer the repository names over `baked.json` and
//! the hints (B-67, checked by the binary as the run begins: [`Signed`]), or the person's own
//! acceptance of an unverified store, never in a run that may write what others trust.
//!
//! Every piece of evidence names the SHA-256 of the `baked.json` bytes it was given, and [`state`]
//! reads the file once: evidence for other bytes is none. The rows a run may open are written
//! once, as the run begins, into its session folder ([`write_index`]), from those bytes; no
//! wrapper reads `baked.json` again, so what the run verified is exactly what it opens. A pack is
//! checked against its row's SHA-256, size and outputs as it is opened.
//!
//! No clock decides any of it. The packs stay sealed: a run opens one only with the key a compile
//! computed, through [`Unseal`], which the binary installs (zstd and the cipher live there, never
//! in this crate).

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use hushrun_core::Digest;
use hushrun_core::blocks::{self, Class, Hex32, Pair};
use serde::{Deserialize, Serialize};

use crate::model::Kind;
use crate::origin::{Origin, Vouch};
use crate::store::Store;

/// The variable that names a baked store a run reads underneath its own store (B-64).
pub const LOWER: &str = "HUSHRUN_BUILD_LOWER";
/// The variable that lets a run that writes nothing others trust read a baked store nothing
/// verifies (B-63): `1`.
pub const LOWER_UNTRUSTED: &str = "HUSHRUN_BUILD_LOWER_UNTRUSTED";
/// `baked.json`'s name.
pub const BAKED_JSON: &str = "baked.json";
/// B-67: the signature of a baked store, a Sigstore bundle beside `baked.json`.
pub const SIGNATURE: &str = "baked.sigstore.json";
/// B-65: where a run finds a baked store when `HUSHRUN_BUILD_LOWER` is unset: the folder an
/// image's `/hushrun` is copied to.
pub const DEFAULT_LOWER: &str = "/hushrun";
/// The file, in a run's session folder, of the hint files a signature covers, one
/// `hints/<first key>/<name>` a line (B-67).
pub const SIGNED_HINTS: &str = "baked-signed-hints";
/// `baked.json`'s schema. Any other is refused.
pub const SCHEMA: &str = "hushrun.baked-store/v1";
/// The packs' folder.
pub const PACKS: &str = "packs";
/// The hints' folder.
pub const HINTS: &str = "hints";
/// The file, in a run's session folder, of the rows the run may open ([`write_index`]).
pub const INDEX: &str = "baked-rows";
/// What a manifest's identity is the digest of, before its pairs.
const MANIFEST_SCHEMA: &str = "hushrun.block-manifest/v2";
/// What a baked store's identity is the digest of, before its checks' parts.
const STORE_SCHEMA: &str = "hushrun.baked-manifests/v2";
/// Where a run's relay records, in the run's own store, that a baked store's manifest was
/// replaced (B-63): `baked/replaced/<manifest>`.
const REPLACED: &str = "baked/replaced";
/// The most bytes `baked.json` may have.
const MOST_BAKED_JSON: u64 = 64 << 20;
/// The most bytes one sealed hint may have.
const MOST_HINT: u64 = 1 << 20;
/// How a pack begins (B-28): `hushrun_core::blocks::pack::MAGIC`, which this crate can't name,
/// since the format's code lives behind a feature only the binary turns on.
const PACK_MAGIC: &[u8; 4] = b"HRBP";
/// How a sealed hint begins: `hushrun_core::blocks::pack::HINT_MAGIC`.
const HINT_MAGIC: &[u8; 4] = b"HRBH";
/// The kinds a baked store carries: those whose row says so ([`Kind::facts`]).
pub const KINDS: [Kind; 3] = [Kind::Rustc, Kind::Go, Kind::Cc];

/// `baked.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baked {
    /// [`SCHEMA`].
    pub schema: String,
    /// What the baker claims: shown, checked for what can't be, never what a run trusts by.
    pub trust: Trust,
    /// The store's identity: [`store_manifest`] of its checks, what its image's fixed tag names
    /// (B-66).
    pub manifest: Hex32,
    /// Where its blocks came from.
    pub source: Source,
    /// The repository, as a run names it (B-26), when known.
    pub repository: Option<String>,
    /// The platform of the machine that baked it, in a tag's words: `linux-x64`, `macos-arm64`.
    pub platform: String,
    /// One entry per check it was baked for, each check once: every check of the repository
    /// whose manifest the source had, or those `--check` named.
    pub checks: Vec<BakedCheck>,
    /// What the toolchains said where it was baked (`rustc -vV`, `go version`, the C compiler's
    /// first line). Shown, never compared: each block's key holds its compiler by content.
    pub toolchains: Vec<String>,
    /// One row per pack, each pack once, whichever checks name it.
    pub packs: Vec<PackRow>,
    /// When it was baked, by the baking machine's clock. Shown, never compared.
    pub baked_ms: u64,
    /// B-76: the folders the blocks bound to their build's folders were built at, for a store
    /// CI baked from its repository's trusted scope (whose blocks bound so only CI jobs upload,
    /// B-24): the CI job's own. A sandbox reuses those blocks only at the same folders (their
    /// keys hold them); elsewhere they compile. Never a person's: no other bake records any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<BakedPaths>,
}

/// B-76: the folders a block bound to its build's folders is keyed by, as a bake records them and
/// a run compares them: the home folder, cargo's and rustup's homes, and the checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakedPaths {
    pub home: String,
    pub cargo_home: String,
    pub rustup_home: String,
    pub checkout: String,
}

impl BakedPaths {
    /// The folders of this process, as its compiles see them: `HOME`, `CARGO_HOME` (else
    /// `$HOME/.cargo`), `RUSTUP_HOME` (else `$HOME/.rustup`), and the checkout at `checkout`.
    /// `None` without a home, or a folder that isn't text.
    pub fn of(
        env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
        checkout: &Path,
    ) -> Option<BakedPaths> {
        let set = |name: &str| {
            env(name)
                .filter(|v| !v.is_empty())
                .and_then(|v| v.into_string().ok())
        };
        let home = set("HOME")?;
        let under = |name: &str| format!("{}/{name}", home.trim_end_matches('/'));
        Some(BakedPaths {
            cargo_home: set("CARGO_HOME").unwrap_or_else(|| under(".cargo")),
            rustup_home: set("RUSTUP_HOME").unwrap_or_else(|| under(".rustup")),
            checkout: checkout.to_str()?.to_string(),
            home,
        })
    }

    /// Each folder of `self` that isn't `here`'s, in words: `HOME=/home/runner (here /root)`.
    pub fn differences(&self, here: &BakedPaths) -> Vec<String> {
        [
            ("HOME", &self.home, &here.home),
            ("CARGO_HOME", &self.cargo_home, &here.cargo_home),
            ("RUSTUP_HOME", &self.rustup_home, &here.rustup_home),
            ("the checkout", &self.checkout, &here.checkout),
        ]
        .into_iter()
        .filter(|(_, baked, here)| baked != here)
        .map(|(name, baked, here)| {
            if name.starts_with("the ") {
                format!("{name} at {baked} (here {here})")
            } else {
                format!("{name}={baked} (here {here})")
            }
        })
        .collect()
    }
}

/// One check a baked store was baked for, and its manifests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakedCheck {
    /// The check, by its id (B-26), or none: the repository's newest build of any check.
    pub check: Option<Hex32>,
    /// One part per kind and platform.
    pub parts: Vec<Part>,
}

/// One kind of block, for one platform, from one manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// `rustc/1`, `go/1` or `cc/1`.
    pub kind: String,
    /// The platform its manifest is kept for: a target triple (`rustc/1`, `cc/1`) or Go's
    /// `GOOS_GOARCH` (`go/1`).
    pub platform: String,
    /// The manifest's identity ([`manifest_id`]).
    pub manifest: Hex32,
}

/// Whether a published image may be made of a baked store (B-29, B-31): what the baker claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trust {
    /// Every pack came from a backend's trusted scope for the repository, `protected/…`.
    Trusted,
    /// Anything else: for the person who baked it alone.
    Untrusted {
        /// Why.
        because: Untrusted,
    },
}

impl Trust {
    /// Whether it claims [`Trust::Trusted`].
    pub fn is_trusted(self) -> bool {
        self == Trust::Trusted
    }
}

/// Why a baked store is untrusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Untrusted {
    /// Baked from a machine's own store: blocks a laptop, a sandbox or a job built or downloaded.
    LocalStore,
    /// A pack came from a scope other than the repository's trusted one.
    Scope,
}

impl Untrusted {
    /// In words, for a line.
    pub fn words(self) -> &'static str {
        match self {
            Untrusted::LocalStore => "it was baked from a machine's own store",
            Untrusted::Scope => {
                "a block in it came from a scope other than the repository's trusted one"
            }
        }
    }
}

/// Where a baked store's blocks came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// This machine's store, and the manifest its last run of the check saved there.
    Store,
    /// A backend's shared store, and the manifests its `open` answered.
    Backend {
        /// The backend's origin.
        host: String,
        /// The org.
        org: String,
    },
}

/// One pack of a baked store, as a backend's row would describe it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackRow {
    /// The block's kind.
    pub kind: String,
    /// The block's address (B-27): its pack is `packs/<address>.pack`.
    pub address: Hex32,
    /// The pack's size.
    pub bytes: u64,
    /// The pack's SHA-256.
    pub sha256: Hex32,
    /// The digest of the block's outputs (B-28).
    pub outputs: Hex32,
    /// What the block is.
    pub class: Class,
    /// What its compile took on the machine that built it, in milliseconds.
    pub cost_ms: u64,
    /// The backend scope it came from, for a store baked from a backend.
    pub scope: Option<String>,
    /// For a linked output, the digest of its link environment (B-37), which its key holds by
    /// content: another machine's linker and C runtime give another key, and a miss.
    pub link_environment: Option<Hex32>,
}

/// Why `baked.json` couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub String);

impl Baked {
    /// Reads `baked.json`'s bytes: refused when its schema is another version, a field is
    /// unknown or missing (its trust among them), a value is out of shape, or it claims what
    /// can't be: a machine's own store trusted, a trusted pack from no trusted scope, a check
    /// named twice, a part or a row of a kind it doesn't carry, a pack named twice, or a store
    /// identity that isn't its checks'.
    pub fn parse(bytes: &[u8]) -> Result<Baked, Invalid> {
        // The schema first, so that a newer version is named as such and not as a bad field.
        let schema = serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|err| Invalid(format!("{BAKED_JSON} isn't JSON: {err}")))?
            .get("schema")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        match schema.as_deref() {
            Some(SCHEMA) => {}
            Some(other) => {
                return Err(Invalid(format!(
                    "{BAKED_JSON} is {other}, a version this hushrun doesn't read (it reads {SCHEMA})"
                )));
            }
            None => return Err(Invalid(format!("{BAKED_JSON} names no schema"))),
        }
        let baked: Baked =
            serde_json::from_slice(bytes).map_err(|err| Invalid(format!("{BAKED_JSON}: {err}")))?;
        let known = |word: &str| KINDS.iter().any(|k| k.word() == word);
        let mut checks = HashSet::new();
        for entry in &baked.checks {
            if !checks.insert(entry.check) {
                return Err(Invalid(format!(
                    "{BAKED_JSON} names the check {} twice",
                    entry
                        .check
                        .map_or_else(|| "of none".to_string(), |c| c.to_string())
                )));
            }
            let mut seen = HashSet::new();
            for part in &entry.parts {
                if !known(&part.kind) {
                    return Err(Invalid(format!(
                        "{BAKED_JSON} has a part of kind {}, which a baked store doesn't carry",
                        part.kind
                    )));
                }
                if !blocks::is_platform(&part.platform) {
                    return Err(Invalid(format!(
                        "{BAKED_JSON}'s {} part names no platform",
                        part.kind
                    )));
                }
                if !seen.insert((part.kind.clone(), part.platform.clone())) {
                    return Err(Invalid(format!(
                        "{BAKED_JSON} has two {} parts for {} in one check",
                        part.kind, part.platform
                    )));
                }
            }
        }
        if let Some(row) = baked
            .packs
            .iter()
            .find(|row| !baked.parts().any(|p| p.kind == row.kind))
        {
            return Err(Invalid(format!(
                "{BAKED_JSON} has a pack of kind {}, of no part",
                row.kind
            )));
        }
        // B-63: one row per pack. Two rows of one address could each say something else of it,
        // and a reader that checked one could open the other.
        let mut addresses = HashSet::new();
        if let Some(row) = baked
            .packs
            .iter()
            .find(|row| !addresses.insert(row.address))
        {
            return Err(Invalid(format!(
                "{BAKED_JSON} names the pack {} twice",
                row.address
            )));
        }
        if baked.manifest != store_manifest(&baked.checks) {
            return Err(Invalid(format!(
                "{BAKED_JSON}'s manifest isn't its checks'"
            )));
        }
        // What can't be: blocks a machine built itself are nobody's evidence, and a trusted
        // store's packs all come from the trusted scope.
        if baked.trust.is_trusted() {
            if baked.source == Source::Store {
                return Err(Invalid(format!(
                    "{BAKED_JSON} claims a machine's own store is trusted, which it never is"
                )));
            }
            if baked
                .packs
                .iter()
                .any(|row| !row.scope.as_deref().is_some_and(blocks::is_trusted_scope))
            {
                return Err(Invalid(format!(
                    "{BAKED_JSON} claims to be trusted and holds a pack from outside the trusted scope"
                )));
            }
        }
        Ok(baked)
    }

    /// Reads the `baked.json` in `folder`.
    pub fn read(folder: &Path) -> Result<Baked, Invalid> {
        Baked::read_bytes(folder).map(|(baked, _)| baked)
    }

    /// Reads the `baked.json` in `folder` once: what it says, and the SHA-256 of the bytes it
    /// was read from, which every piece of evidence names.
    pub fn read_bytes(folder: &Path) -> Result<(Baked, Hex32), Invalid> {
        let path = folder.join(BAKED_JSON);
        let file =
            fs::File::open(&path).map_err(|err| Invalid(format!("{}: {err}", path.display())))?;
        let mut bytes = Vec::new();
        file.take(MOST_BAKED_JSON + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| Invalid(format!("{}: {err}", path.display())))?;
        if bytes.len() as u64 > MOST_BAKED_JSON {
            return Err(Invalid(format!("{} is too large", path.display())));
        }
        Ok((Baked::parse(&bytes)?, Hex32::sha256(&bytes)))
    }

    /// Every part of every check.
    pub fn parts(&self) -> impl Iterator<Item = &Part> {
        self.checks.iter().flat_map(|c| c.parts.iter())
    }

    /// Each kind and platform its parts are for, once, in order.
    pub fn platforms(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for part in self.parts() {
            let pair = (part.kind.clone(), part.platform.clone());
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
        out
    }

    /// The checks it was baked for.
    pub fn check_ids(&self) -> Vec<Option<Hex32>> {
        self.checks.iter().map(|c| c.check).collect()
    }
}

/// The identity of a manifest: the SHA-256 of `hushrun.block-manifest/v2`, the kind and each
/// distinct pair's two addresses, sorted. A backend's manifest and the one a machine saves for the
/// same run have the same identity, and so do two builds of the same code whose compiles ended in
/// another order (cargo builds in parallel): the order is the prefetch's, never the identity's.
pub fn manifest_id(kind: &str, pairs: &[Pair]) -> Hex32 {
    let mut sorted: Vec<(Hex32, Hex32)> =
        pairs.iter().map(|Pair(step, key)| (*step, *key)).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut bytes = Vec::with_capacity(64 * sorted.len() + 64);
    bytes.extend_from_slice(MANIFEST_SCHEMA.as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(kind.as_bytes());
    bytes.push(b'\n');
    for (step, key) in &sorted {
        bytes.extend_from_slice(&step.0);
        bytes.extend_from_slice(&key.0);
    }
    Hex32::sha256(&bytes)
}

/// A baked store's identity: the SHA-256 of each check's id and its parts' kind, platform and
/// manifest, sorted. Any check's manifest changing changes it; the rows are not in it, so a
/// store's identity is known before it is baked (B-68), and is what its image's fixed tag names.
pub fn store_manifest(checks: &[BakedCheck]) -> Hex32 {
    let mut lines: Vec<String> = checks
        .iter()
        .flat_map(|c| {
            let check = c.check.map_or_else(|| "-".to_string(), |c| c.to_string());
            c.parts
                .iter()
                .map(move |p| format!("{check}\n{}\n{}\n{}\n", p.kind, p.platform, p.manifest))
        })
        .collect();
    lines.sort();
    let mut text = format!("{STORE_SCHEMA}\n");
    for line in lines {
        text.push_str(&line);
    }
    Hex32::sha256(text.as_bytes())
}

/// The platform of a target triple in a tag's words (`linux-x64`); the triple itself for one
/// with no short name. A GNU and a musl triple of one machine have the same words.
pub fn platform_of(target: &str) -> String {
    let arch = match target.split('-').next().unwrap_or_default() {
        "x86_64" | "amd64" => "x64",
        "aarch64" | "arm64" => "arm64",
        other => other,
    };
    let os = if target.contains("-linux") {
        "linux"
    } else if target.contains("-apple-darwin") || target.contains("-darwin") {
        "macos"
    } else if target.contains("-windows") {
        "windows"
    } else {
        return target.to_string();
    };
    format!("{os}-{arch}")
}

/// Whether a part of `kind` baked for `baked` serves a run for `here`: Go's `GOOS_GOARCH` the
/// same, a target triple of the same OS and CPU (B-63: a GNU and a musl build of hushrun, or of
/// the compiler's own triple and the driver's, read one store; each block's key holds its
/// compiler by content, so anything else is a miss, never a wrong block).
pub fn same_platform(kind: &str, baked: &str, here: &str) -> bool {
    if kind == Kind::Go.word() {
        baked == here
    } else {
        platform_of(baked) == platform_of(here)
    }
}

/// The pack of `address` in a baked store's folder.
pub fn pack_path(folder: &Path, address: &Hex32) -> PathBuf {
    folder.join(PACKS).join(format!("{address}.pack"))
}

/// The folder of a first key's sealed hints in a baked store's folder.
pub fn hints_dir(folder: &Path, step_address: &Hex32) -> PathBuf {
    folder.join(HINTS).join(step_address.to_string())
}

/// How much of a folder [`check_contents`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Names and kinds of entries only: what a run checks when it begins.
    Names,
    /// Names, kinds, and each pack's and hint's leading bytes: what `hushrun build bake` checks
    /// before a folder is put in place.
    Bytes,
}

/// B-63: a baked store's folder holds `baked.json`, `packs/<64 hex>.pack` (each a pack) and
/// `hints/<64 hex>/<64 hex>` (each a sealed hint), as regular files and folders, and nothing
/// else: no link, no other file, no other folder. With `baked`, every pack it names is there
/// and every pack there is named. `Err` names the first thing out of place.
pub fn check_contents(folder: &Path, baked: Option<&Baked>, depth: Depth) -> Result<(), String> {
    let mut packs = Vec::new();
    for entry in entries(folder)? {
        let (name, kind) = entry;
        match (name.as_str(), kind) {
            (BAKED_JSON, EntryKind::File) | (SIGNATURE, EntryKind::File) => {}
            (PACKS, EntryKind::Dir) => {
                for (name, kind) in entries(&folder.join(PACKS))? {
                    let address = name
                        .strip_suffix(".pack")
                        .and_then(Hex32::parse)
                        .filter(|_| kind == EntryKind::File)
                        .ok_or_else(|| format!("{PACKS}/{name} is not a pack"))?;
                    if depth == Depth::Bytes {
                        leading(&folder.join(PACKS).join(&name), PACK_MAGIC)
                            .map_err(|why| format!("{PACKS}/{name} {why}"))?;
                    }
                    packs.push(address);
                }
            }
            (HINTS, EntryKind::Dir) => {
                for (name, kind) in entries(&folder.join(HINTS))? {
                    if kind != EntryKind::Dir || Hex32::parse(&name).is_none() {
                        return Err(format!("{HINTS}/{name} is not a first key's hints"));
                    }
                    let dir = folder.join(HINTS).join(&name);
                    for (hint, kind) in entries(&dir)? {
                        if kind != EntryKind::File || Hex32::parse(&hint).is_none() {
                            return Err(format!("{HINTS}/{name}/{hint} is not a sealed hint"));
                        }
                        if depth == Depth::Bytes {
                            leading(&dir.join(&hint), HINT_MAGIC)
                                .map_err(|why| format!("{HINTS}/{name}/{hint} {why}"))?;
                        }
                    }
                }
            }
            (other, EntryKind::Dir) => {
                return Err(format!("{other}/ is not part of a baked store"));
            }
            (other, EntryKind::File) => {
                return Err(format!("{other} is not part of a baked store"));
            }
            (other, EntryKind::Other) => {
                return Err(format!(
                    "{other} is a link or a special file, which a baked store never holds"
                ));
            }
        }
    }
    if let Some(baked) = baked {
        let named: std::collections::HashSet<Hex32> =
            baked.packs.iter().map(|row| row.address).collect();
        if let Some(extra) = packs.iter().find(|a| !named.contains(a)) {
            return Err(format!("{PACKS}/{extra}.pack is not named in {BAKED_JSON}"));
        }
        let there: std::collections::HashSet<Hex32> = packs.into_iter().collect();
        if let Some(missing) = baked.packs.iter().find(|row| !there.contains(&row.address)) {
            return Err(format!(
                "{BAKED_JSON} names {PACKS}/{}.pack, which isn't there",
                missing.address
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EntryKind {
    File,
    Dir,
    Other,
}

/// A folder's entries, by name and kind (a link is never followed).
fn entries(dir: &Path) -> Result<Vec<(String, EntryKind)>, String> {
    let read = fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        let name = entry.file_name().into_string().map_err(|name| {
            format!(
                "{} is not a name a baked store holds",
                name.to_string_lossy()
            )
        })?;
        let kind = match entry.file_type() {
            Ok(t) if t.is_file() => EntryKind::File,
            Ok(t) if t.is_dir() => EntryKind::Dir,
            _ => EntryKind::Other,
        };
        out.push((name, kind));
    }
    out.sort();
    Ok(out)
}

/// Whether the file at `path` begins with `magic`.
fn leading(path: &Path, magic: &[u8; 4]) -> Result<(), &'static str> {
    let mut head = [0u8; 4];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .map_err(|_| "can't be read")?;
    if &head == magic {
        Ok(())
    } else {
        Err("doesn't begin as one")
    }
}

/// Who answered for a baked store as the run began (B-63): the backend it was baked from, for
/// the run's repository, and the scope the backend said is that repository's trusted one (the
/// `protected/…` scope `open` named among those the run reads).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    /// The backend's origin.
    pub host: String,
    /// The org.
    pub org: String,
    /// The run's repository, which the lookups named.
    pub repository: String,
    /// The repository's trusted scope, as `open` named it.
    pub scope: String,
}

/// What the backend vouched for as the run began: the rows of the `baked.json` whose SHA-256
/// is `baked_json` that it serves in the repository's trusted scope, each with that row's
/// outputs, SHA-256 and size, whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vouched {
    /// Who answered.
    pub server: Server,
    /// The SHA-256 of the `baked.json` bytes whose rows were looked up.
    pub baked_json: Hex32,
    /// The rows it vouched for, as that file has them.
    pub rows: Vec<PackRow>,
}

/// What the run knows of the backend's answer, for [`state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked<'a> {
    /// Nobody asked: `hushrun build status` and `hushrun build bake` describe a run, and only a
    /// run asks.
    NotAsked,
    /// The run asked a backend, and got no answer for the store: another backend than the
    /// store's, or none in time.
    NoAnswer,
    /// The run has no backend to ask about build blocks (none named, or one that doesn't share
    /// them): nothing newer than a signature can be known to it.
    NoBackend,
    /// It answered.
    Answered(&'a Vouched),
}

/// B-67: what a signature the run verified as it began says of its baked store: who signed it,
/// the SHA-256 of the `baked.json` it covers (which names every pack's SHA-256), and the file of
/// the hint files it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed {
    /// The signer, in words: the certificate's identity, or the key's id.
    pub signer: String,
    /// The SHA-256 of the `baked.json` the signature covers.
    pub baked_json: Hex32,
    /// The file of the hint files it covers ([`SIGNED_HINTS`]).
    pub hints: PathBuf,
    /// Whether it is all the run can know of the store now (B-16, B-63): the run has no backend
    /// to ask ([`Asked::NoBackend`]). A run whose backend didn't answer for the store, or isn't
    /// its source, can't know whether a block was withdrawn since. Set by [`state`].
    #[serde(default)]
    pub current: bool,
}

/// What lets a run reuse a block from its baked store without asking (B-63): never the file's
/// claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Evidence {
    /// The backend vouched, block by block, in the run's repository's trusted scope: only those
    /// blocks are opened.
    Server(Server),
    /// Nothing verifies it, and the person accepts that (`HUSHRUN_BUILD_LOWER_UNTRUSTED=1`, in a
    /// run that writes nothing others trust): every block is opened, and reused as the person's
    /// own.
    Accepted,
    /// B-67: a signature by a signer the repository names covers its `baked.json` and hints:
    /// every block is opened, and reused without asking, in CI too.
    Signed(Signed),
}

impl Evidence {
    /// Whether a block opened under it is reused without asking (B-63, B-67): the person's
    /// say-so, or a signature the repository names. The server's word stands as a download's.
    pub fn reuses_unasked(&self) -> bool {
        matches!(self, Evidence::Accepted | Evidence::Signed(_))
    }

    /// Whether it verifies where its blocks came from (B-16, B-63): the backend's word in the
    /// repository's trusted scope, or a signature the repository names. Only the person's
    /// say-so doesn't.
    pub fn verifies(&self) -> bool {
        matches!(self, Evidence::Server(_) | Evidence::Signed(_))
    }

    /// B-16, B-63: whether it verifies a block's source and is current for this run, what spares
    /// a block its first-reuse audit: the server's word given in this run, or a signature where
    /// the run has no backend that could say the store was replaced or a block withdrawn
    /// ([`Signed::current`]). A signed store whose backend timed out, or isn't its source, keeps
    /// the audit.
    pub fn current(&self) -> bool {
        match self {
            Evidence::Server(_) => true,
            Evidence::Signed(signed) => signed.current,
            Evidence::Accepted => false,
        }
    }

    /// The word `hushrun build status --format json` writes.
    pub fn word(&self) -> &'static str {
        match self {
            Evidence::Server(_) => "server",
            Evidence::Accepted => "accepted",
            Evidence::Signed(_) => "signed",
        }
    }
}

/// What a run knows of itself, for [`state`].
#[derive(Debug, Clone)]
pub struct Here<'a> {
    /// The run's repository, as a run names it.
    pub repository: Option<&'a str>,
    /// The platforms this run builds for, by kind: the compiler's target triple for `rustc/1`
    /// and `cc/1`, Go's `GOOS_GOARCH` for `go/1` where the run is a Go module's.
    pub platforms: &'a [(Kind, String)],
    /// Whether the run accepts a store nothing verifies ([`accepts_untrusted`]).
    pub untrusted_ok: bool,
    /// What the backend answered.
    pub server: Asked<'a>,
    /// B-67: the store's signature, when the run verified one by a signer its repository names.
    pub signed: Option<&'a Signed>,
    /// The run's own store, which holds what its relays learned (a replaced manifest).
    pub upper: &'a Path,
    /// B-76: the run's own folders ([`BakedPaths::of`]), which the store's bound blocks need.
    pub paths: Option<&'a BakedPaths>,
    /// B-65: the store is the one at the default folder, `/hushrun`, which no variable named:
    /// read only when [`default_refused`] finds nothing. A folder a variable names, `/hushrun`
    /// among them, is the person's choice.
    pub default: bool,
}

/// A part a run reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LowerPart {
    /// Its kind.
    pub kind: String,
    /// Its platform.
    pub platform: String,
}

/// A baked store a run reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lower {
    /// Its folder.
    pub folder: PathBuf,
    /// Its identity.
    pub manifest: Hex32,
    /// The SHA-256 of the `baked.json` bytes the run read, which its evidence names: what a
    /// block opened from it records ([`origin_of`]).
    pub contents: Hex32,
    /// What it claims.
    pub claim: Trust,
    /// Where its blocks came from.
    pub source: Source,
    /// Its repository.
    pub repository: Option<String>,
    /// The checks it was baked for.
    pub checks: Vec<Option<Hex32>>,
    /// The parts this run reads.
    pub parts: Vec<LowerPart>,
    /// What lets the run reuse its blocks.
    pub evidence: Evidence,
    /// What the run says of it: a part for another platform.
    #[serde(default)]
    pub notes: Vec<String>,
    /// The rows the run may open, as [`state`] read them: every row under a signature or the
    /// person's say-so, those the backend vouched for under its word. Written to [`INDEX`] as
    /// the run begins; never kept in `session.json`.
    #[serde(skip)]
    pub openable: Vec<PackRow>,
    /// The run's [`INDEX`] file, once written: what every reader opens rows from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<PathBuf>,
}

impl Lower {
    /// Whether the run reads its blocks of `kind`.
    pub fn reads(&self, kind: Kind) -> bool {
        self.parts.iter().any(|p| p.kind == kind.word())
    }

    /// Whether the run reads its blocks of `kind` for a compile for `platform` (B-60's target
    /// triple, for C and C++).
    pub fn reads_for(&self, kind: Kind, platform: &str) -> bool {
        self.parts
            .iter()
            .any(|p| p.kind == kind.word() && same_platform(&p.kind, &p.platform, platform))
    }
}

/// A baked store's state for a run (B-63): see the module's table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// None is named.
    Absent,
    /// It can't be read, holds what a baked store never holds, or claims what can't be.
    Invalid {
        /// The folder named.
        folder: PathBuf,
        /// Why.
        why: String,
    },
    /// Baked for another repository.
    WrongRepository {
        /// The folder.
        folder: PathBuf,
        /// Its repository.
        baked: Option<String>,
        /// This run's.
        here: Option<String>,
    },
    /// No part of it is for a platform this run builds for.
    WrongPlatform {
        /// The folder.
        folder: PathBuf,
        /// Its parts' platforms.
        baked: String,
        /// This run's.
        here: String,
    },
    /// Its evidence would be its backend's answer, which only a run asks for: what `hushrun
    /// build status` and `hushrun build bake` say, never a run.
    ServerDecides {
        /// The folder.
        folder: PathBuf,
        /// The backend it was baked from.
        host: String,
    },
    /// Nothing here verifies it, and the run doesn't accept that.
    Untrusted {
        /// The folder.
        folder: PathBuf,
        /// What it claims.
        claim: Trust,
    },
    /// Its source named a newer manifest for one of its checks.
    Replaced(Lower),
    /// Read, as its evidence says.
    Current(Lower),
}

impl State {
    /// The baked store a run reads, when it reads one.
    pub fn lower(&self) -> Option<&Lower> {
        match self {
            State::Current(lower) | State::Replaced(lower) => Some(lower),
            _ => None,
        }
    }

    /// The state's word, as `hushrun build status --format json` writes it.
    pub fn word(&self) -> &'static str {
        match self {
            State::Absent => "absent",
            State::Invalid { .. } => "invalid",
            State::WrongRepository { .. } => "wrong-repository",
            State::WrongPlatform { .. } => "wrong-platform",
            State::ServerDecides { .. } => "server-decides",
            State::Untrusted { .. } => "untrusted",
            State::Replaced(_) => "replaced",
            State::Current(_) => "current",
        }
    }

    /// What it means for a run, in words: why it is ignored, or how it is read.
    pub fn words(&self) -> String {
        let evidence = |lower: &Lower| match &lower.evidence {
            Evidence::Server(server) => format!(
                "verified: {} vouched for its blocks in {}'s trusted scope",
                server.host, server.repository
            ),
            Evidence::Accepted => format!(
                "unverified, read because {LOWER_UNTRUSTED}=1; it claims to be {}",
                if lower.claim.is_trusted() {
                    "trusted"
                } else {
                    "untrusted"
                }
            ),
            Evidence::Signed(signed) => {
                format!("signed by {}, a signer the repository names", signed.signer)
            }
        };
        match self {
            State::Absent => format!("none ({LOWER} names none)"),
            State::Invalid { folder, why } => {
                format!("{} is ignored: {why}", folder.display())
            }
            State::WrongRepository {
                folder,
                baked,
                here,
            } => format!(
                "{} is ignored: it was baked for {}, and this run is {}",
                folder.display(),
                baked.as_deref().unwrap_or("no repository"),
                here.as_deref().unwrap_or("no repository"),
            ),
            State::WrongPlatform {
                folder,
                baked,
                here,
            } => format!(
                "{} is ignored: it was baked for {baked}, and this is {here}",
                folder.display()
            ),
            State::ServerDecides { folder, host } => format!(
                "{}: a run reads its blocks only as {host} vouches for each one in the repository's trusted scope, which the run asks as it begins and this command doesn't; with no answer, and no signature a signer the repository names made, the run ignores it",
                folder.display()
            ),
            State::Untrusted { folder, claim } => format!(
                "{} is ignored: nothing here verifies it (no answer from a backend it was baked from, no signature by a signer hushrun.yaml names{}); a run that writes nothing others trust reads it unverified with {LOWER_UNTRUSTED}=1",
                folder.display(),
                match claim {
                    Trust::Untrusted { because } => format!(", and {}", because.words()),
                    Trust::Trusted => String::new(),
                }
            ),
            State::Replaced(lower) => format!(
                "{}, replaced: its source has a newer manifest, so a block from it is reused only once a server vouches for it, or when none can be reached",
                lower.folder.display()
            ),
            State::Current(lower) => {
                format!("{}, current ({})", lower.folder.display(), evidence(lower))
            }
        }
    }

    /// The lines a run says about it: why it is ignored, or its notes.
    pub fn lines(&self) -> Vec<String> {
        match self {
            State::Absent => Vec::new(),
            State::Current(lower) | State::Replaced(lower) => lower
                .notes
                .iter()
                .map(|note| format!("build: the baked store: {note}"))
                .collect(),
            _ => vec![format!("build: the baked store {}", self.words())],
        }
    }
}

/// The baked store a run's variables name, when one is named: `HUSHRUN_BUILD_LOWER`, set and
/// empty for none; unset, [`DEFAULT_LOWER`] when it holds a `baked.json` (B-65: an image's
/// `/hushrun`, copied in with one line). A relative path names a folder `Invalid` says so of.
pub fn named(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    match env(LOWER) {
        Some(value) if value.is_empty() => None,
        Some(value) => Some(PathBuf::from(value)),
        None => default_lower(Path::new(DEFAULT_LOWER)),
    }
}

/// `folder` when it holds a `baked.json`, as [`named`] finds a store no variable names: one
/// `stat` when nothing is there.
pub fn default_lower(folder: &Path) -> Option<PathBuf> {
    (cfg!(unix) && folder.join(BAKED_JSON).is_file()).then(|| folder.to_path_buf())
}

/// B-65: why the folder at the default path, `/hushrun`, which nobody named, isn't read: it, its
/// `baked.json`, or a folder of its packs or hints, is a link, is owned by another user than root
/// or this one, or another user may write it (anyone, or its group), so another process of the
/// machine may have put it there. `None` when it may be read.
pub fn default_refused(folder: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        refused_for(folder, rustix::process::geteuid().as_raw(), &read_only)
    }
    #[cfg(not(unix))]
    {
        let _ = folder;
        Some("a baked store is read from the default folder only on Unix".into())
    }
}

/// [`default_refused`] for the user `me`.
#[cfg(unix)]
fn refused_for(folder: &Path, me: u32, read_only: &dyn Fn(&Path) -> bool) -> Option<String> {
    {
        use std::os::unix::fs::MetadataExt as _;
        // The folder, its baked.json, and the folders its packs and hints are in, each folder of
        // hints too: a pack is checked by its SHA-256 as it is opened, but a hint under the
        // server's word or the person's say-so only by its name.
        let mut paths = vec![folder.to_path_buf(), folder.join(BAKED_JSON)];
        for sub in [PACKS, HINTS] {
            let path = folder.join(sub);
            if fs::symlink_metadata(&path).is_ok() {
                paths.push(path);
            }
        }
        if let Ok(read) = fs::read_dir(folder.join(HINTS)) {
            paths.extend(read.flatten().map(|entry| entry.path()));
        }
        for path in paths {
            let Ok(meta) = fs::symlink_metadata(&path) else {
                return Some(format!("{} can't be read", path.display()));
            };
            if meta.file_type().is_symlink() {
                return Some(format!("{} is a link", path.display()));
            }
            // B-71: on a file system mounted read-only nobody can write it, whoever its owner
            // seems to be: a user-namespaced container (rootless podman, `hostUsers: false`, a
            // remapped daemon) sees a root-owned read-only mount as owned by nobody (65534), and
            // no `chown` can apply there.
            if read_only(&path) {
                continue;
            }
            // B-71: each says how to fix it: an archive extracted by root is root's and one
            // extracted by the user who runs builds is theirs, both read; a umask only takes
            // bits away from the archive's 0644 and 0755, so a writable one was made so after.
            if meta.uid() != 0 && meta.uid() != me {
                return Some(format!(
                    "{} belongs to another user, neither root nor this one: extract it as root or as the user who runs builds, or `chown -R` it to one of them; a volume mounted read-only is read whoever owns it, and {LOWER}={} reads it anyway",
                    path.display(),
                    folder.display()
                ));
            }
            if meta.mode() & 0o002 != 0 {
                return Some(format!(
                    "anyone may write {}: `chmod -R go-w {}` fixes it",
                    path.display(),
                    folder.display()
                ));
            }
            if meta.mode() & 0o020 != 0 {
                return Some(format!(
                    "its group may write {}: `chmod -R g-w {}` fixes it (or name it with {LOWER} to read it anyway)",
                    path.display(),
                    folder.display()
                ));
            }
        }
        None
    }
}

/// Whether `path` is on a file system mounted read-only (`statvfs`'s `ST_RDONLY`).
#[cfg(unix)]
fn read_only(path: &Path) -> bool {
    rustix::fs::statvfs(path)
        .is_ok_and(|stat| stat.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY))
}

/// The SHA-256 of the `baked.json` in `folder`, as a signature covers it.
pub fn baked_json_digest(folder: &Path) -> Option<Hex32> {
    let file = fs::File::open(folder.join(BAKED_JSON)).ok()?;
    let mut bytes = Vec::new();
    file.take(MOST_BAKED_JSON + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MOST_BAKED_JSON).then(|| Hex32::sha256(&bytes))
}

/// B-67: the hint files of the baked store in `folder`, as a signature lists them:
/// `hints/<first key>/<name>`, sorted. `None` when the folder can't be read.
pub fn hint_files(folder: &Path) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let hints = folder.join(HINTS);
    let Ok(read) = fs::read_dir(&hints) else {
        return Some(out);
    };
    for dir in read {
        let dir = dir.ok()?;
        let step = dir.file_name().into_string().ok()?;
        for hint in fs::read_dir(dir.path()).ok()? {
            let name = hint.ok()?.file_name().into_string().ok()?;
            out.push(format!("{HINTS}/{step}/{name}"));
        }
    }
    out.sort();
    Some(out)
}

/// Whether a run accepts a baked store nothing verifies (B-63): `HUSHRUN_BUILD_LOWER_UNTRUSTED=1`,
/// and only in a run that writes nothing others trust. `may_write_trusted` is the run's
/// authority, as [`authority`] decides it.
pub fn accepts_untrusted(env: &dyn Fn(&str) -> Option<OsString>, may_write_trusted: bool) -> bool {
    !may_write_trusted && env(LOWER_UNTRUSTED).is_some_and(|v| v == "1")
}

/// B-63: whether a run may write what others trust, the one definition: in CI, whose results a
/// repository counts as CI's; or with a relay whose server lets it write the repository's
/// trusted scope (`policy` is what `open` answered: `Some(true)` when it writes it), or whose
/// server hasn't said (`None` with a relay), since what it writes isn't known.
pub fn authority(in_ci: bool, relay: Option<Option<bool>>) -> bool {
    in_ci
        || match relay {
            None => false,
            Some(writes_trusted) => writes_trusted.unwrap_or(true),
        }
}

/// Whether `server` may stand as evidence for `baked`: the backend the store names, for the
/// store's repository, which is the run's.
pub fn server_verifies(baked: &Baked, server: &Server) -> bool {
    matches!(&baked.source, Source::Backend { host, org } if *host == server.host && *org == server.org)
        && baked.repository.as_deref() == Some(server.repository.as_str())
}

/// B-63: the one definition of a baked store's state for a run. Read when asked: nothing of it
/// is kept between runs but the replaced marks in the run's own store. `baked.json` is read
/// once, and evidence counts only for the bytes read.
pub fn state(folder: Option<&Path>, here: &Here<'_>) -> State {
    let Some(folder) = folder else {
        return State::Absent;
    };
    let invalid = |why: String| State::Invalid {
        folder: folder.to_path_buf(),
        why,
    };
    if !folder.is_absolute() {
        return invalid(format!("{LOWER} must be an absolute path"));
    }
    if here.default
        && let Some(why) = default_refused(folder)
    {
        return invalid(why);
    }
    // The run's own store writes, sweeps and cleans; a baked store under it would be touched.
    let resolved = fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    let upper = fs::canonicalize(here.upper).unwrap_or_else(|_| here.upper.to_path_buf());
    if resolved.starts_with(&upper) || upper.starts_with(&resolved) {
        return invalid(format!(
            "it overlaps this machine's own store, {}",
            here.upper.display()
        ));
    }
    let (baked, contents) = match Baked::read_bytes(folder) {
        Ok(read) => read,
        Err(Invalid(why)) => return invalid(why),
    };
    if let Err(why) = check_contents(folder, Some(&baked), Depth::Names) {
        return invalid(why);
    }
    if baked.repository.as_deref() != here.repository {
        return State::WrongRepository {
            folder: folder.to_path_buf(),
            baked: baked.repository,
            here: here.repository.map(str::to_string),
        };
    }
    let mut notes = Vec::new();
    let mut parts = Vec::new();
    let mut mismatched = Vec::new();
    for (kind, baked_platform) in baked.platforms() {
        let Some((_, platform)) = here.platforms.iter().find(|(k, _)| k.word() == kind) else {
            continue;
        };
        if !same_platform(&kind, &baked_platform, platform) {
            mismatched.push(format!("{kind} {baked_platform}"));
            notes.push(format!(
                "its {kind} blocks were baked for {baked_platform}, and this is {platform}: they compile here"
            ));
            continue;
        }
        if !parts.iter().any(|p: &LowerPart| p.kind == kind) {
            parts.push(LowerPart {
                kind,
                platform: baked_platform,
            });
        }
    }
    if parts.is_empty() && !mismatched.is_empty() {
        let here_words: Vec<String> = here
            .platforms
            .iter()
            .map(|(kind, p)| format!("{} {p}", kind.word()))
            .collect();
        return State::WrongPlatform {
            folder: folder.to_path_buf(),
            baked: mismatched.join(", "),
            here: here_words.join(", "),
        };
    }
    // B-76: blocks bound to CI's folders are reused only at those folders: said, never refused.
    if let (Some(baked_paths), Some(here_paths)) = (&baked.paths, here.paths) {
        let differ = baked_paths.differences(here_paths);
        if !differ.is_empty() {
            notes.push(format!(
                "its blocks bound to the folders of the CI job that built them are for {}: they compile here",
                differ.join(", ")
            ));
        }
    }
    let vouched = match here.server {
        Asked::Answered(vouched)
            if vouched.baked_json == contents && server_verifies(&baked, &vouched.server) =>
        {
            Some(vouched)
        }
        _ => None,
    };
    let signed = here.signed.filter(|signed| signed.baked_json == contents);
    let (evidence, openable) = match (vouched, signed) {
        (Some(vouched), _) => {
            // Each row as the backend vouched for it, whole: address, kind, outputs, SHA-256,
            // size and the rest. The file is the one looked up, so this holds for every row the
            // lookup kept; it is checked all the same.
            let by_address: HashMap<Hex32, &PackRow> =
                vouched.rows.iter().map(|row| (row.address, row)).collect();
            let rows = baked
                .packs
                .iter()
                .filter(|row| by_address.get(&row.address) == Some(row))
                .cloned()
                .collect();
            (Evidence::Server(vouched.server.clone()), rows)
        }
        (None, Some(signed)) => (
            Evidence::Signed(Signed {
                current: here.server == Asked::NoBackend,
                ..signed.clone()
            }),
            baked.packs.clone(),
        ),
        (None, None) if here.untrusted_ok => (Evidence::Accepted, baked.packs.clone()),
        (None, None) => {
            if here.server == Asked::NotAsked
                && let Source::Backend { host, .. } = &baked.source
            {
                return State::ServerDecides {
                    folder: folder.to_path_buf(),
                    host: host.clone(),
                };
            }
            return State::Untrusted {
                folder: folder.to_path_buf(),
                claim: baked.trust,
            };
        }
    };
    notes.dedup();
    let checks = baked.check_ids();
    let lower = Lower {
        folder: folder.to_path_buf(),
        manifest: baked.manifest,
        contents,
        claim: baked.trust,
        source: baked.source,
        repository: baked.repository,
        checks,
        parts,
        evidence,
        notes,
        openable,
        index: None,
    };
    if is_replaced(here.upper, &baked.manifest) {
        State::Replaced(lower)
    } else {
        State::Current(lower)
    }
}

/// The mark that the store `manifest` was replaced, in a run's own store.
fn replaced_path(upper: &Path, manifest: &Hex32) -> PathBuf {
    upper.join(REPLACED).join(manifest.to_string())
}

/// Whether a relay of this machine saw the store `manifest` replaced.
pub fn is_replaced(upper: &Path, manifest: &Hex32) -> bool {
    replaced_path(upper, manifest).is_file()
}

/// A name no other process uses, on this machine or another sharing the folder: the label and
/// 128 random bits. Never a process id: two containers on one store are both process 1.
pub fn unique(label: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    let random = || {
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    format!("{label}.{:016x}{:016x}", random(), random())
}

/// What a relay records when the source names a newer manifest for a part of the store
/// `manifest`: a file written whole in the run's own store, under a name no other process uses.
/// A mark is never swept: a store replaced stays replaced, and a baked store with another
/// manifest is another store.
pub fn mark_replaced(
    upper: &Store,
    manifest: &Hex32,
    newer: &Hex32,
    now: SystemTime,
) -> io::Result<()> {
    let path = replaced_path(upper.root(), manifest);
    let dir = path.parent().unwrap_or(upper.root());
    fs::create_dir_all(dir)?;
    let ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let temp = dir.join(format!(".{}.tmp", unique(&manifest.to_string())));
    fs::write(
        &temp,
        format!("{{\"newer\":\"{newer}\",\"seen_ms\":{ms}}}\n"),
    )?;
    fs::rename(&temp, &path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// The width of one row of an [`INDEX`] file, its newline included: four digests, the kind,
/// two numbers of 20 digits, the class, seven spaces and the newline.
const INDEX_LINE: usize = 4 * 64 + 7 + 2 * 20 + 12 + 7 + 1;

/// One row of an [`INDEX`] file, of [`INDEX_LINE`] bytes: address, kind, SHA-256, outputs, size,
/// cost, class and link environment, each padded to its width. `None` for a row whose words
/// don't fit, which is never opened.
fn index_line(row: &PackRow) -> Option<String> {
    let link = row
        .link_environment
        .map_or_else(|| "-".repeat(64), |l| l.to_string());
    let line = format!(
        "{} {:<7} {} {} {:020} {:020} {:<12} {link}\n",
        row.address,
        row.kind,
        row.sha256,
        row.outputs,
        row.bytes,
        row.cost_ms,
        row.class.word(),
    );
    (line.len() == INDEX_LINE).then_some(line)
}

/// The row an [`INDEX`] file's line holds.
fn index_row(line: &[u8]) -> Option<PackRow> {
    let text = std::str::from_utf8(line).ok()?.strip_suffix('\n')?;
    let fields: Vec<&str> = text.split(' ').filter(|f| !f.is_empty()).collect();
    let [address, kind, sha256, outputs, bytes, cost, class, link] = fields.as_slice() else {
        return None;
    };
    Some(PackRow {
        kind: (*kind).to_string(),
        address: Hex32::parse(address)?,
        bytes: bytes.parse().ok()?,
        sha256: Hex32::parse(sha256)?,
        outputs: Hex32::parse(outputs)?,
        class: serde_json::from_value(serde_json::Value::String((*class).to_string())).ok()?,
        cost_ms: cost.parse().ok()?,
        scope: None,
        link_environment: if link.bytes().all(|b| b == b'-') {
            None
        } else {
            Some(Hex32::parse(link)?)
        },
    })
}

/// B-64: writes the rows a run may open into its session's [`INDEX`] file, whole, sorted by
/// address, so a reader finds one with a few reads and never parses `baked.json` again.
pub fn write_index(path: &Path, rows: &[PackRow]) -> io::Result<()> {
    let mut rows: Vec<&PackRow> = rows.iter().collect();
    rows.sort_by_key(|row| row.address);
    rows.dedup_by_key(|row| row.address);
    let mut text = String::with_capacity(rows.len() * INDEX_LINE);
    for row in rows {
        if let Some(line) = index_line(row) {
            text.push_str(&line);
        }
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{}.tmp", unique(INDEX)));
    let written = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    })()
    .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// An [`INDEX`] file, open.
struct Index {
    file: Mutex<fs::File>,
    rows: u64,
}

impl Index {
    fn open(path: &Path) -> Option<Index> {
        let file = fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        (len % INDEX_LINE as u64 == 0).then(|| Index {
            file: Mutex::new(file),
            rows: len / INDEX_LINE as u64,
        })
    }

    /// The row of `address`, by a binary search of the sorted lines.
    fn get(&self, address: &Hex32) -> Option<PackRow> {
        let mut file = self.file.lock().ok()?;
        let mut line = [0u8; INDEX_LINE];
        let (mut low, mut high) = (0u64, self.rows);
        let wanted = address.to_string();
        while low < high {
            let mid = low + (high - low) / 2;
            file.seek(SeekFrom::Start(mid * INDEX_LINE as u64)).ok()?;
            file.read_exact(&mut line).ok()?;
            match line[..64].cmp(wanted.as_bytes()) {
                std::cmp::Ordering::Equal => return index_row(&line),
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
            }
        }
        None
    }
}

/// Writes the hint files a signature covers into the session's [`SIGNED_HINTS`] file, whole.
pub fn write_signed_hints(path: &Path, hints: &[String]) -> io::Result<()> {
    let mut text = String::new();
    for hint in hints {
        text.push_str(hint);
        text.push('\n');
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let temp = dir.join(format!(".{}.tmp", unique(SIGNED_HINTS)));
    fs::write(&temp, text)?;
    fs::rename(&temp, path)
}

fn read_signed_hints(path: &Path) -> HashSet<String> {
    fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Where a bake writes before its folder is put in place, beside `out`: a name no other process
/// uses, made exclusively.
pub fn new_folder(out: &Path) -> io::Result<PathBuf> {
    let (parent, name) = parent_and_name(out)?;
    fs::create_dir_all(&parent)?;
    let folder = parent.join(format!(".{name}.bake-{}", unique("new")));
    fs::create_dir(&folder)?;
    Ok(folder)
}

fn parent_and_name(out: &Path) -> io::Result<(PathBuf, String)> {
    let name = out
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::other(format!("{} names no folder", out.display())))?
        .to_string();
    let parent = out
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok((parent, name))
}

/// How long a bake waits for another bake of the same folder to finish before it gives up.
const BAKE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// B-63: the lock of one `--out` folder, `.<name>.bake.lock` beside it, held for a whole bake.
/// Only its holder writes beside `out`, so whatever it finds there was left by a bake that died:
/// no process id is asked, which says nothing across containers sharing a volume. The lock goes
/// with the process, however it ends.
#[derive(Debug)]
pub struct BakeLock {
    _file: fs::File,
}

/// Takes `out`'s [`BakeLock`], waiting a moment for another bake of it; `Err` says another bake
/// holds it.
pub fn lock_bake(out: &Path) -> io::Result<BakeLock> {
    let (parent, name) = parent_and_name(out)?;
    fs::create_dir_all(&parent)?;
    let path = parent.join(format!(".{name}.bake.lock"));
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    let started = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(BakeLock { _file: file }),
            Err(fs::TryLockError::WouldBlock) if started.elapsed() < BAKE_WAIT => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "another bake is writing {} (it holds {}): run one bake of a folder at a time",
                        out.display(),
                        path.display()
                    ),
                ));
            }
            Err(fs::TryLockError::Error(err)) => return Err(err),
        }
    }
}

/// B-63: puts the baked folder `new` in place at `out`, crash-safely: exchanged with the store
/// there in one step where the file system can (both names always name a whole store), else the
/// old store renamed aside first, which [`reconcile`] puts back after a crash between the two
/// renames. The old store goes once the new one is in place.
pub fn put_in_place(new: &Path, out: &Path) -> io::Result<()> {
    if fs::symlink_metadata(out).is_err() {
        return fs::rename(new, out);
    }
    match crate::store::exchange(new, out) {
        Ok(()) => {
            let _ = fs::remove_dir_all(new);
            Ok(())
        }
        Err(_) => {
            let (parent, name) = parent_and_name(out)?;
            let aside = parent.join(format!(".{name}.bake-{}", unique("old")));
            fs::rename(out, &aside)?;
            if let Err(err) = fs::rename(new, out) {
                let _ = fs::rename(&aside, out);
                return Err(err);
            }
            let _ = fs::remove_dir_all(&aside);
            Ok(())
        }
    }
}

/// B-63: what a bake that died left beside `out`, put right before the next one writes, under
/// `out`'s lock (which the type asks for): a store renamed aside (`old`) while `out` is missing
/// goes back; every other leftover (`.<name>.bake-…`) is removed. With the lock held, no live
/// bake has any.
pub fn reconcile(out: &Path, _held: &BakeLock) -> io::Result<()> {
    let (parent, name) = parent_and_name(out)?;
    let Ok(entries) = fs::read_dir(&parent) else {
        return Ok(());
    };
    let prefix = format!(".{name}.bake-");
    let mut leftovers: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .collect();
    leftovers.sort();
    for path in leftovers {
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let old = file.is_some_and(|f| f.starts_with(&format!("{prefix}old.")));
        if old && fs::symlink_metadata(out).is_err() && path.join(BAKED_JSON).is_file() {
            fs::rename(&path, out)?;
        } else {
            let _ = fs::remove_dir_all(&path);
        }
    }
    Ok(())
}

/// What opens a baked store's sealed packs and hints: the binary's, which holds zstd and the
/// cipher. Without one installed, a run reads no baked store.
pub trait Unseal: Sync + Send {
    /// The hint sealed in `sealed` with first key `step`, or `None`.
    fn hint(&self, step: &Digest, sealed: &[u8]) -> Option<Vec<u8>>;
    /// Opens the pack at `pack`, described by `row`, with `key` (of first key `step`), and
    /// publishes its block of `kind` into `store` with `origin`, keeping at least `floor` bytes
    /// free on its volume. `Err` says why not; nothing is published then.
    #[allow(clippy::too_many_arguments)]
    fn block(
        &self,
        store: &Store,
        kind: Kind,
        pack: &Path,
        row: &PackRow,
        key: &Digest,
        step: &Digest,
        origin: &Origin,
        floor: u64,
        repository: Option<&str>,
        now: SystemTime,
    ) -> Result<(), String>;
}

static UNSEAL: OnceLock<&'static dyn Unseal> = OnceLock::new();

/// Installs what opens packs, once, before the wrapper runs. Later calls change nothing.
pub fn install(unseal: &'static dyn Unseal) {
    let _ = UNSEAL.set(unseal);
}

/// What opens packs, when the binary installed it.
pub fn unseal() -> Option<&'static dyn Unseal> {
    UNSEAL.get().copied()
}

/// The record of a block opened from a baked store (B-64) under its evidence, at `now_ms`: from
/// that store (by the SHA-256 of the `baked.json` the run read), and, when the backend vouched
/// for it, with the backend's word in the repository's trusted scope, so that it stands as a
/// download the run's server vouched for today.
pub fn origin_of(lower: &Lower, outputs: Hex32, now_ms: u64) -> Origin {
    let mut origin = Origin::baked(lower.contents, outputs);
    if let Evidence::Server(server) = &lower.evidence {
        origin.renew(Vouch {
            host: server.host.clone(),
            org: server.org.clone(),
            repository: server.repository.clone(),
            scope: server.scope.clone(),
            outputs,
            date_ms: now_ms,
            word: crate::origin::Word::Served,
        });
    }
    origin
}

/// A baked store as one wrapper, Go cache program or launcher reads it: the rows the run may
/// open, from the run's [`INDEX`] file, and the hints, read when first needed.
pub struct Reader<'a> {
    lower: &'a Lower,
    unseal: &'a dyn Unseal,
    index: OnceLock<Option<Index>>,
    signed_hints: OnceLock<HashSet<String>>,
}

impl<'a> Reader<'a> {
    /// A reader of `lower` through `unseal`.
    pub fn new(lower: &'a Lower, unseal: &'a dyn Unseal) -> Reader<'a> {
        Reader {
            lower,
            unseal,
            index: OnceLock::new(),
            signed_hints: OnceLock::new(),
        }
    }

    /// The SHA-256 of the `baked.json` the run read: what a block opened from it records.
    pub fn contents(&self) -> Hex32 {
        self.lower.contents
    }

    fn index(&self) -> Option<&Index> {
        self.index
            .get_or_init(|| self.lower.index.as_deref().and_then(Index::open))
            .as_ref()
    }

    /// The row of a block the run may open: what the run's state wrote into its index, which
    /// under the server's word holds only what it vouched for.
    fn openable(&self, address: &Hex32) -> Option<PackRow> {
        self.index()?.get(address)
    }

    /// B-64: the baked store's sealed hints of first key `step`, of `kind`, opened, each written
    /// into the run's store beside the hints downloaded from a shared store (B-30). How many
    /// were. A hint only says which files a key reads; only a hint file whose bytes are its name
    /// is read, and under a signature only one it covers.
    pub fn hints_into(&self, store: &Store, kind: Kind, step: &Digest, now: SystemTime) -> usize {
        if self.index().is_none() {
            return 0;
        }
        // B-67: under a signature, only the hint files it covers, each whose bytes are its name.
        let signed = match &self.lower.evidence {
            Evidence::Signed(signed) => Some(
                self.signed_hints
                    .get_or_init(|| read_signed_hints(&signed.hints)),
            ),
            _ => None,
        };
        let address = blocks::address(kind.word(), step.as_bytes());
        let dir = hints_dir(&self.lower.folder, &address);
        let Ok(read) = fs::read_dir(&dir) else {
            return 0;
        };
        let mut added = 0;
        for entry in read.flatten() {
            let path = entry.path();
            let small = fs::symlink_metadata(&path)
                .is_ok_and(|meta| meta.is_file() && meta.len() <= MOST_HINT);
            if !small {
                continue;
            }
            let Ok(sealed) = fs::read(&path) else {
                continue;
            };
            // A hint file is named for the SHA-256 of its bytes, whatever the evidence; under a
            // signature it must also be one the signature covers.
            let name = entry.file_name().to_string_lossy().into_owned();
            if Hex32::sha256(&sealed).to_string() != name {
                continue;
            }
            if let Some(covered) = signed
                && !covered.contains(&format!("{HINTS}/{address}/{name}"))
            {
                continue;
            }
            if let Some(hint) = self.unseal.hint(step, &sealed)
                && store.add_shared_hint(step, &hint, now).is_ok()
            {
                added += 1;
            }
        }
        added
    }

    /// B-64: the block of `kind` under `key`, when the run may open its pack: opened with `key`,
    /// checked against its row, and published into the run's store, its record saying it came
    /// from this baked store, and the backend's word when it vouched.
    #[allow(clippy::too_many_arguments)]
    pub fn open_into(
        &self,
        store: &Store,
        kind: Kind,
        key: &Digest,
        step: &Digest,
        floor: u64,
        repository: Option<&str>,
        now: SystemTime,
    ) -> Result<bool, String> {
        let address = blocks::address(kind.word(), key.as_bytes());
        let Some(row) = self.openable(&address) else {
            return Ok(false);
        };
        if row.kind != kind.word() {
            return Ok(false);
        }
        let now_ms = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let origin = origin_of(self.lower, row.outputs, now_ms);
        self.unseal
            .block(
                store,
                kind,
                &pack_path(&self.lower.folder, &address),
                &row,
                key,
                step,
                &origin,
                floor,
                repository,
                now,
            )
            .map(|()| true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-63, B-65
    #[test]
    fn a_unique_name_is_its_label_and_random_bits_never_a_process_id() {
        let one = unique("bake-image");
        let (label, random) = one.rsplit_once('.').unwrap();
        assert_eq!(label, "bake-image");
        assert_eq!(random.len(), 32, "{one}");
        assert!(random.bytes().all(|b| b.is_ascii_hexdigit()), "{one}");
        assert_ne!(one, unique("bake-image"));
    }

    // covers: B-63
    // The index is binary-searched, so it must be sorted: rows handed over out of address order
    // are each found.
    #[test]
    fn an_index_finds_every_row_whatever_order_the_rows_came_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX);
        let rows: Vec<PackRow> = (1..=9u8).rev().map(|n| row(Kind::Rustc, n, None)).collect();
        write_index(&path, &rows).unwrap();
        let index = Index::open(&path).unwrap();
        for r in &rows {
            assert_eq!(
                index.get(&r.address).map(|found| found.address),
                Some(r.address),
                "row {} not found",
                r.address
            );
        }
    }

    const TARGET: &str = "aarch64-unknown-linux-gnu";
    const REPO: &str = "github.com/acme/shop";
    const HOST: &str = "https://api.example.com";

    fn part(kind: Kind, platform: &str) -> Part {
        Part {
            kind: kind.word().into(),
            platform: platform.into(),
            manifest: Hex32([kind as u8 + 1; 32]),
        }
    }

    fn row(kind: Kind, n: u8, scope: Option<&str>) -> PackRow {
        PackRow {
            kind: kind.word().into(),
            address: Hex32([n; 32]),
            bytes: 4,
            sha256: Hex32([2; 32]),
            outputs: Hex32([3; 32]),
            class: Class::Library,
            cost_ms: 100,
            scope: scope.map(str::to_string),
            link_environment: None,
        }
    }

    /// A store baked from a backend, claiming trust, every row from the trusted scope.
    pub(crate) fn sample() -> Baked {
        let checks = vec![BakedCheck {
            check: Some(Hex32([4; 32])),
            parts: vec![part(Kind::Rustc, TARGET), part(Kind::Go, "linux_arm64")],
        }];
        Baked {
            schema: SCHEMA.into(),
            trust: Trust::Trusted,
            manifest: store_manifest(&checks),
            source: Source::Backend {
                host: HOST.into(),
                org: "acme".into(),
            },
            repository: Some(REPO.into()),
            platform: platform_of(TARGET),
            checks,
            toolchains: vec!["rustc 1.94.0 (4a4ef493e 2026-03-02)".into()],
            packs: vec![
                row(Kind::Rustc, 1, Some("protected/github.com/acme/shop")),
                row(Kind::Go, 9, Some("protected/github.com/acme/shop")),
            ],
            baked_ms: 1_800_000_000_000,
            paths: None,
        }
    }

    fn laptop() -> Baked {
        let mut baked = sample();
        baked.trust = Trust::Untrusted {
            because: Untrusted::LocalStore,
        };
        baked.source = Source::Store;
        for row in &mut baked.packs {
            row.scope = None;
        }
        baked
    }

    fn write(folder: &Path, baked: &Baked) {
        fs::create_dir_all(folder.join(PACKS)).unwrap();
        fs::write(
            folder.join(BAKED_JSON),
            serde_json::to_vec_pretty(baked).unwrap(),
        )
        .unwrap();
        for row in &baked.packs {
            fs::write(pack_path(folder, &row.address), b"HRBPsealed").unwrap();
        }
    }

    fn parse(value: &serde_json::Value) -> Result<Baked, Invalid> {
        Baked::parse(&serde_json::to_vec(value).unwrap())
    }

    // covers: B-63
    #[test]
    fn baked_json_round_trips_and_refuses_what_it_can_t_read_or_can_t_be() {
        let baked = sample();
        let value = serde_json::to_value(&baked).unwrap();
        assert_eq!(parse(&value).unwrap(), baked);
        assert_eq!(value["trust"]["level"], "trusted");
        assert_eq!(value["platform"], "linux-arm64");
        let refused = |change: &dyn Fn(&mut serde_json::Value), why: &str| {
            let mut v = value.clone();
            change(&mut v);
            let err = parse(&v).unwrap_err();
            assert!(err.0.contains(why), "{why}: {err:?}");
        };
        refused(
            &|v| v["schema"] = "hushrun.baked-store/v2".into(),
            "a version this hushrun doesn't read",
        );
        refused(&|v| v["secret"] = "x".into(), "unknown field");
        refused(
            &|v| {
                v.as_object_mut().unwrap().remove("trust");
            },
            "trust",
        );
        refused(&|v| v["trust"] = serde_json::json!({"level": "mostly"}), "");
        // What can't be: a machine's own store trusted, a trusted row from no trusted scope.
        refused(
            &|v| v["source"] = serde_json::json!({"from": "store"}),
            "never is",
        );
        refused(
            &|v| v["packs"][0]["scope"] = "person/7".into(),
            "outside the trusted scope",
        );
        refused(
            &|v| v["packs"][0]["scope"] = serde_json::Value::Null,
            "outside the trusted scope",
        );
        refused(
            &|v| v["checks"][0]["parts"][1]["kind"] = "py/1".into(),
            "doesn't carry",
        );
        refused(&|v| v["packs"][1]["kind"] = "cc/1".into(), "of no part");
        refused(
            &|v| v["manifest"] = Hex32([0; 32]).to_string().into(),
            "isn't its checks'",
        );
        refused(
            &|v| v["checks"][0]["parts"][1] = v["checks"][0]["parts"][0].clone(),
            "two rustc/1 parts",
        );
        refused(
            &|v| {
                let first = v["checks"][0].clone();
                v["checks"].as_array_mut().unwrap().push(first);
            },
            "names the check",
        );
        refused(
            &|v| {
                let first = v["packs"][0].clone();
                v["packs"].as_array_mut().unwrap().push(first);
            },
            "twice",
        );
        assert!(Baked::parse(b"not json").is_err());
        // An untrusted store may come from anywhere.
        assert!(parse(&serde_json::to_value(laptop()).unwrap()).is_ok());
    }

    #[test]
    fn platforms_have_a_tag_s_words_and_gnu_and_musl_are_one() {
        assert_eq!(platform_of("x86_64-unknown-linux-gnu"), "linux-x64");
        assert_eq!(platform_of("aarch64-unknown-linux-gnu"), "linux-arm64");
        assert_eq!(platform_of("aarch64-unknown-linux-musl"), "linux-arm64");
        assert_eq!(platform_of("x86_64-linux-gnu"), "linux-x64");
        assert_eq!(platform_of("aarch64-apple-darwin"), "macos-arm64");
        assert_eq!(platform_of("arm64-apple-darwin25.0.0"), "macos-arm64");
        assert_eq!(platform_of("wasm32-wasip1"), "wasm32-wasip1");
        // covers: B-63
        assert!(same_platform(
            "rustc/1",
            "x86_64-unknown-linux-musl",
            "x86_64-unknown-linux-gnu"
        ));
        assert!(same_platform("cc/1", "aarch64-linux-gnu", TARGET));
        assert!(!same_platform(
            "rustc/1",
            "x86_64-unknown-linux-gnu",
            TARGET
        ));
        assert!(same_platform("go/1", "linux_arm64", "linux_arm64"));
        assert!(!same_platform("go/1", "linux_amd64", "linux_arm64"));
    }

    #[test]
    fn a_manifest_s_identity_is_its_kind_and_its_pairs_in_any_order() {
        let a = Pair(Hex32([1; 32]), Hex32([2; 32]));
        let b = Pair(Hex32([3; 32]), Hex32([4; 32]));
        let one = manifest_id("rustc/1", &[a, b]);
        assert_eq!(one, manifest_id("rustc/1", &[a, b]));
        // Cargo's parallel build ends compiles in another order each time: the same pairs, the
        // same identity, and a pair named twice counts once.
        assert_eq!(one, manifest_id("rustc/1", &[b, a]));
        assert_eq!(one, manifest_id("rustc/1", &[b, a, b]));
        assert_ne!(one, manifest_id("go/1", &[a, b]));
        assert_ne!(one, manifest_id("rustc/1", &[a]));
        // A block under another first key is another pair.
        assert_ne!(
            one,
            manifest_id("rustc/1", &[a, Pair(Hex32([5; 32]), Hex32([4; 32]))])
        );
        // A store's identity is its checks' parts, in any order; another check's part is
        // another identity, and so is the same part under another check.
        let checks = sample().checks;
        let mut reversed = checks.clone();
        reversed[0].parts.reverse();
        assert_eq!(store_manifest(&checks), store_manifest(&reversed));
        let mut more = checks.clone();
        more.push(BakedCheck {
            check: Some(Hex32([5; 32])),
            parts: vec![part(Kind::Rustc, TARGET)],
        });
        assert_ne!(store_manifest(&checks), store_manifest(&more));
        let mut moved = checks.clone();
        moved[0].check = Some(Hex32([6; 32]));
        assert_ne!(store_manifest(&checks), store_manifest(&moved));
    }

    // covers: B-63
    #[test]
    fn a_baked_store_holds_its_json_packs_and_hints_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let baked = sample();
        let make = |name: &str| {
            let folder = dir.path().join(name);
            write(&folder, &baked);
            fs::create_dir_all(hints_dir(&folder, &Hex32([5; 32]))).unwrap();
            fs::write(
                hints_dir(&folder, &Hex32([5; 32])).join(Hex32([6; 32]).to_string()),
                b"HRBHsealed",
            )
            .unwrap();
            folder
        };
        let folder = make("baked");
        check_contents(&folder, Some(&baked), Depth::Bytes).unwrap();
        type Spoil = Box<dyn Fn(&Path)>;
        let cases: Vec<(&str, Spoil)> = vec![
            (
                "secret.env",
                Box::new(|f: &Path| fs::write(f.join("secret.env"), "K=v").unwrap()),
            ),
            (
                "target/",
                Box::new(|f: &Path| fs::create_dir(f.join("target")).unwrap()),
            ),
            (
                "packs/notes.txt",
                Box::new(|f: &Path| fs::write(f.join(PACKS).join("notes.txt"), "x").unwrap()),
            ),
            (
                "not named",
                Box::new(|f: &Path| {
                    fs::write(pack_path(f, &Hex32([7; 32])), b"HRBPsealed").unwrap();
                }),
            ),
            (
                "not a sealed hint",
                Box::new(|f: &Path| {
                    fs::write(hints_dir(f, &Hex32([5; 32])).join("plain"), "x").unwrap();
                }),
            ),
        ];
        for (n, (what, spoil)) in cases.into_iter().enumerate() {
            let copy = make(&format!("copy-{n}"));
            spoil(&copy);
            let err = check_contents(&copy, Some(&baked), Depth::Names).unwrap_err();
            assert!(err.contains(what.trim_end_matches('/')), "{what}: {err}");
        }
        fs::write(pack_path(&folder, &Hex32([1; 32])), b"PLAIN").unwrap();
        check_contents(&folder, Some(&baked), Depth::Names).unwrap();
        assert!(check_contents(&folder, Some(&baked), Depth::Bytes).is_err());
        fs::remove_file(pack_path(&folder, &Hex32([1; 32]))).unwrap();
        assert!(
            check_contents(&folder, Some(&baked), Depth::Names)
                .unwrap_err()
                .contains("isn't there")
        );
        #[cfg(unix)]
        {
            let linked = make("linked");
            std::os::unix::fs::symlink("/etc/passwd", linked.join("passwd")).unwrap();
            assert!(
                check_contents(&linked, Some(&baked), Depth::Names)
                    .unwrap_err()
                    .contains("link")
            );
        }
    }

    struct World {
        dir: tempfile::TempDir,
        upper: PathBuf,
        platforms: Vec<(Kind, String)>,
    }

    impl World {
        fn new() -> World {
            let dir = tempfile::tempdir().unwrap();
            let upper = dir.path().join("store");
            fs::create_dir_all(&upper).unwrap();
            World {
                dir,
                upper,
                platforms: vec![
                    (Kind::Rustc, TARGET.into()),
                    (Kind::Cc, "aarch64-linux-gnu".into()),
                    (Kind::Go, "linux_arm64".into()),
                ],
            }
        }

        fn store(&self, name: &str, baked: &Baked) -> PathBuf {
            let folder = self.dir.path().join(name);
            write(&folder, baked);
            folder
        }

        fn server(&self) -> Server {
            Server {
                host: HOST.into(),
                org: "acme".into(),
                repository: REPO.into(),
                scope: "protected/4242".into(),
            }
        }

        /// What `server` answered for the store in `folder` as it is now: every row of it, or
        /// those `only` names.
        fn vouched(&self, server: Server, folder: &Path, only: Option<&[Hex32]>) -> Vouched {
            let (baked, digest) = Baked::read_bytes(folder).unwrap();
            Vouched {
                server,
                baked_json: digest,
                rows: baked
                    .packs
                    .into_iter()
                    .filter(|row| only.is_none_or(|only| only.contains(&row.address)))
                    .collect(),
            }
        }

        /// The store a run reads, as `Session::set_lower` leaves it: its rows in the index.
        fn reading(&self, state: &State) -> Lower {
            let mut lower = state.lower().unwrap().clone();
            let index = self.dir.path().join(format!("{INDEX}-{}", unique("t")));
            write_index(&index, &lower.openable).unwrap();
            lower.index = Some(index);
            lower
        }

        fn here<'a>(&'a self, server: Option<&'a Vouched>, untrusted_ok: bool) -> Here<'a> {
            Here {
                repository: Some(REPO),
                platforms: &self.platforms,
                untrusted_ok,
                server: server.map_or(Asked::NoAnswer, Asked::Answered),
                signed: None,
                upper: &self.upper,
                paths: None,
                default: false,
            }
        }
    }

    // covers: B-76
    // A store CI baked records its folders; a run at other folders reads it all the same and is
    // told its bound blocks compile there. A store with no folders (any other bake) says nothing.
    #[test]
    fn a_store_s_ci_folders_are_said_where_a_run_s_folders_differ() {
        let env = |home: &'static str, cargo: Option<&'static str>| {
            move |name: &str| match name {
                "HOME" => Some(std::ffi::OsString::from(home)),
                "CARGO_HOME" => cargo.map(std::ffi::OsString::from),
                _ => None,
            }
        };
        let ci = BakedPaths::of(
            &env("/home/runner", None),
            Path::new("/home/runner/work/shop/shop"),
        )
        .unwrap();
        assert_eq!(ci.cargo_home, "/home/runner/.cargo");
        assert_eq!(ci.rustup_home, "/home/runner/.rustup");
        assert!(ci.differences(&ci.clone()).is_empty());
        let sandbox =
            BakedPaths::of(&env("/root", Some("/usr/local/cargo")), Path::new("/src")).unwrap();
        let said = ci.differences(&sandbox).join(", ");
        assert!(said.contains("HOME=/home/runner (here /root)"), "{said}");
        assert!(
            said.contains("CARGO_HOME=/home/runner/.cargo (here /usr/local/cargo)"),
            "{said}"
        );
        assert!(
            said.contains("the checkout at /home/runner/work/shop/shop (here /src)"),
            "{said}"
        );
        // `paths` is optional in baked.json: a store without it reads as before.
        let mut store = sample();
        store.paths = Some(ci);
        let json = serde_json::to_string(&store).unwrap();
        assert!(
            json.contains("\"cargo_home\":\"/home/runner/.cargo\""),
            "{json}"
        );
        let back: Baked = serde_json::from_str(&json).unwrap();
        assert_eq!(back, store);
        store.paths = None;
        assert!(!serde_json::to_string(&store).unwrap().contains("paths"));
    }

    // covers: B-63
    #[test]
    fn a_forged_store_that_claims_trust_is_never_trusted_without_the_server_s_word() {
        // A backend-sourced, trusted, self-consistent baked.json that nobody's server vouched
        // for: in CI (nothing accepts an unverified store) it is not read at all.
        let w = World::new();
        let folder = w.store("forged", &sample());
        let ci = w.here(None, false);
        assert_eq!(state(Some(&folder), &ci).word(), "untrusted");
        // Another backend's answer is no evidence for it, nor one for another repository.
        let other = w.vouched(
            Server {
                host: "https://evil.example.com".into(),
                ..w.server()
            },
            &folder,
            None,
        );
        assert_eq!(
            state(Some(&folder), &w.here(Some(&other), false)).word(),
            "untrusted"
        );
        let elsewhere = w.vouched(
            Server {
                repository: "github.com/acme/other".into(),
                ..w.server()
            },
            &folder,
            None,
        );
        assert_eq!(
            state(Some(&folder), &w.here(Some(&elsewhere), false)).word(),
            "untrusted"
        );
        // An answer for other bytes of baked.json is none.
        let mut stale = w.vouched(w.server(), &folder, None);
        stale.baked_json = Hex32([0; 32]);
        assert_eq!(
            state(Some(&folder), &w.here(Some(&stale), false)).word(),
            "untrusted"
        );
        // status and bake ask nobody: whether a run reads it is the backend's to say.
        let mut unasked = w.here(None, false);
        unasked.server = Asked::NotAsked;
        let decides = state(Some(&folder), &unasked);
        assert_eq!(decides.word(), "server-decides");
        assert!(decides.words().contains(HOST), "{}", decides.words());
        // The store's own backend answered: read, block by block as it vouched.
        let server = w.vouched(w.server(), &folder, Some(&[Hex32([1; 32])]));
        let read = state(Some(&folder), &w.here(Some(&server), false));
        assert_eq!(read.word(), "current");
        let lower = read.lower().unwrap();
        assert!(matches!(lower.evidence, Evidence::Server(_)));
        assert!(lower.evidence.verifies());
        let openable: Vec<Hex32> = lower.openable.iter().map(|r| r.address).collect();
        assert_eq!(openable, [Hex32([1; 32])]);
    }

    // covers: B-63
    #[test]
    fn every_state_of_a_baked_store_comes_from_one_function() {
        let w = World::new();
        assert_eq!(state(None, &w.here(None, true)), State::Absent);
        assert_eq!(
            state(Some(Path::new("baked")), &w.here(None, true)).word(),
            "invalid"
        );
        let missing = w.dir.path().join("missing");
        assert_eq!(state(Some(&missing), &w.here(None, true)).word(), "invalid");
        let inside = w.upper.join("baked");
        write(&inside, &laptop());
        assert_eq!(state(Some(&inside), &w.here(None, true)).word(), "invalid");

        let folder = w.store("laptop", &laptop());
        // Untrusted unless accepted; accepted, read unverified.
        let refused = state(Some(&folder), &w.here(None, false));
        assert_eq!(refused.word(), "untrusted");
        assert!(refused.lines()[0].contains(LOWER_UNTRUSTED));
        let read = state(Some(&folder), &w.here(None, true));
        assert_eq!(read.word(), "current");
        let lower = read.lower().unwrap();
        assert_eq!(lower.evidence, Evidence::Accepted);
        assert!(lower.reads(Kind::Rustc) && lower.reads(Kind::Go));
        assert!(!lower.reads(Kind::Cc));
        assert!(read.lines().is_empty(), "{:?}", read.lines());

        // Another repository.
        let mut other = laptop();
        other.repository = Some("github.com/acme/other".into());
        let other = w.store("other", &other);
        assert_eq!(
            state(Some(&other), &w.here(None, true)).word(),
            "wrong-repository"
        );

        // A part for another platform is left out with a line; the rest is read. No part for
        // this platform: wrong platform.
        let mut amd = World::new();
        amd.platforms = vec![
            (Kind::Rustc, TARGET.into()),
            (Kind::Go, "linux_amd64".into()),
        ];
        let folder = amd.store("laptop", &laptop());
        let partly = state(Some(&folder), &amd.here(None, true));
        assert_eq!(partly.word(), "current");
        assert!(!partly.lower().unwrap().reads(Kind::Go));
        assert!(partly.lines()[0].contains("go/1 blocks were baked for linux_arm64"));
        let mut x64 = World::new();
        x64.platforms = vec![
            (Kind::Rustc, "x86_64-unknown-linux-gnu".into()),
            (Kind::Go, "linux_amd64".into()),
        ];
        let folder = x64.store("laptop", &laptop());
        assert_eq!(
            state(Some(&folder), &x64.here(None, true)).word(),
            "wrong-platform"
        );
        // A musl build of hushrun reads a store a GNU one baked.
        let mut musl = World::new();
        musl.platforms = vec![(Kind::Rustc, "aarch64-unknown-linux-musl".into())];
        let folder = musl.store("laptop", &laptop());
        assert!(
            state(Some(&folder), &musl.here(None, true))
                .lower()
                .unwrap()
                .reads(Kind::Rustc)
        );

        // A store baked for two checks reads as one: each kind once, whichever checks name it.
        let mut two = laptop();
        two.checks.push(BakedCheck {
            check: Some(Hex32([7; 32])),
            parts: vec![part(Kind::Rustc, TARGET)],
        });
        two.manifest = store_manifest(&two.checks);
        let folder = w.store("two", &two);
        let read = state(Some(&folder), &w.here(None, true));
        let lower = read.lower().unwrap();
        assert_eq!(lower.checks, [Some(Hex32([4; 32])), Some(Hex32([7; 32]))]);
        assert_eq!(lower.parts.len(), 2, "{:?}", lower.parts);
    }

    // covers: B-63
    #[test]
    fn a_replaced_store_stays_replaced_whatever_the_sweep_does() {
        let w = World::new();
        let baked = laptop();
        let folder = w.store("laptop", &baked);
        let store = Store::new(&w.upper);
        let now = SystemTime::now();
        mark_replaced(&store, &Hex32([8; 32]), &Hex32([9; 32]), now).unwrap();
        assert_eq!(state(Some(&folder), &w.here(None, true)).word(), "current");
        mark_replaced(&store, &baked.manifest, &Hex32([9; 32]), now).unwrap();
        assert_eq!(state(Some(&folder), &w.here(None, true)).word(), "replaced");
        // Days later, every sweep run, and offline: still replaced.
        let later = now + std::time::Duration::from_secs(60 * 24 * 60 * 60);
        assert!(crate::sweep::force(&store, later, 15).swept);
        assert_eq!(state(Some(&folder), &w.here(None, true)).word(), "replaced");
        // Two marks written at once by two processes: one whole file, no leftover.
        mark_replaced(&store, &baked.manifest, &Hex32([10; 32]), now).unwrap();
        let names: Vec<String> = fs::read_dir(w.upper.join(REPLACED))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    struct Fake;

    impl Unseal for Fake {
        fn hint(&self, _: &Digest, sealed: &[u8]) -> Option<Vec<u8>> {
            Some(sealed.to_vec())
        }
        fn block(
            &self,
            _: &Store,
            _: Kind,
            _: &Path,
            _: &PackRow,
            _: &Digest,
            _: &Digest,
            _: &Origin,
            _: u64,
            _: Option<&str>,
            _: SystemTime,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    // covers: B-63, B-64
    #[test]
    fn with_the_server_s_word_only_what_it_vouched_for_in_the_trusted_scope_is_opened() {
        let w = World::new();
        // A key whose address is a row of the store.
        let key = Digest::of(b"a crate");
        let address = blocks::address("rustc/1", key.as_bytes());
        let other = Digest::of(b"another crate");
        let mut baked = sample();
        baked.packs = vec![
            PackRow {
                address,
                ..row(Kind::Rustc, 0, Some("protected/github.com/acme/shop"))
            },
            PackRow {
                address: blocks::address("rustc/1", other.as_bytes()),
                ..row(Kind::Rustc, 0, Some("protected/github.com/acme/shop"))
            },
        ];
        let folder = w.store("verified", &baked);
        // The server vouched for one; for the other it vouched for another row (other
        // outputs), which is no evidence for this one.
        let mut server = w.vouched(w.server(), &folder, None);
        server.rows[1].outputs = Hex32([0xee; 32]);
        let read = state(Some(&folder), &w.here(Some(&server), false));
        let lower = w.reading(&read);
        let lower = &lower;
        let reader = Reader::new(lower, &Fake);
        let store = Store::new(&w.upper);
        let now = SystemTime::now();
        assert_eq!(
            reader.open_into(&store, Kind::Rustc, &key, &key, 0, Some(REPO), now),
            Ok(true)
        );
        assert_eq!(
            reader.open_into(&store, Kind::Rustc, &other, &other, 0, Some(REPO), now),
            Ok(false)
        );
        // Another kind's address of the same key is no row.
        assert_eq!(
            reader.open_into(&store, Kind::Go, &key, &key, 0, Some(REPO), now),
            Ok(false)
        );
        // Its record carries the server's word, fresh for the run's host, org and repository.
        let origin = origin_of(lower, Hex32([3; 32]), 5);
        assert_eq!(
            origin.standing(Some((HOST, "acme", REPO)), 6),
            crate::origin::Standing::Fresh
        );
        assert!(origin.vouched.iter().any(|v| v.scope == "protected/4242"));
        assert!(origin.baked_in(&lower.contents));
    }

    // covers: B-64
    #[cfg(unix)]
    #[test]
    fn housekeeping_never_touches_the_store_underneath() {
        use std::os::unix::fs::PermissionsExt as _;
        let w = World::new();
        let folder = w.store("baked", &laptop());
        let files = [pack_path(&folder, &Hex32([1; 32])), folder.join(BAKED_JSON)];
        for path in &files {
            fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
        }
        for path in [folder.join(PACKS), folder.clone()] {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let listing = || {
            files
                .iter()
                .map(|path| {
                    let meta = fs::metadata(path).unwrap();
                    (path.clone(), meta.len(), meta.modified().unwrap())
                })
                .collect::<Vec<_>>()
        };
        let before = listing();
        let store = Store::new(&w.upper);
        let key = Digest::of(b"opened from the baked store");
        let source = w.dir.path().join("out.rlib");
        fs::write(&source, b"rlib").unwrap();
        let now = SystemTime::now();
        store
            .publish_with_origin(
                &key,
                &serde_json::json!({}),
                &[crate::store::NewFile {
                    name: "out.rlib".into(),
                    source,
                }],
                &Origin::baked(laptop().manifest, Hex32([3; 32])),
                now,
            )
            .unwrap();
        let later = now + std::time::Duration::from_secs(30 * 24 * 60 * 60);
        assert!(crate::sweep::force(&store, later, 15).swept);
        assert!(store.block(&key).unwrap().is_none());
        store.clean().unwrap();
        assert_eq!(listing(), before);
        assert!(check_contents(&folder, None, Depth::Bytes).is_ok());
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(folder.join(PACKS), fs::Permissions::from_mode(0o755)).unwrap();
    }

    // covers: B-63
    #[test]
    fn a_bake_s_folder_goes_in_place_whole_and_what_a_dead_bake_left_is_put_right() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let first = new_folder(&out).unwrap();
        write(&first, &laptop());
        put_in_place(&first, &out).unwrap();
        assert!(out.join(BAKED_JSON).is_file());
        // Over a store there: exchanged, the old one gone, nothing left beside it.
        let second = new_folder(&out).unwrap();
        write(&second, &sample());
        put_in_place(&second, &out).unwrap();
        assert_eq!(Baked::read(&out).unwrap().trust, Trust::Trusted);
        let beside = || {
            fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .filter(|n| n.starts_with(".out."))
                .collect::<Vec<_>>()
        };
        assert!(beside().is_empty(), "{:?}", beside());
        // A bake killed between renaming the old store aside and the new one in: `out` is
        // missing and the old store sits aside under a dead process's name. The next bake puts
        // it back first, and removes a dead bake's half-written folder.
        let dead = 999_999_999;
        fs::rename(&out, dir.path().join(format!(".out.bake-old.{dead}.0"))).unwrap();
        fs::create_dir(dir.path().join(format!(".out.bake-new.{dead}.1"))).unwrap();
        let held = lock_bake(&out).unwrap();
        reconcile(&out, &held).unwrap();
        assert_eq!(Baked::read(&out).unwrap().trust, Trust::Trusted);
        assert!(
            beside().iter().all(|n| n == ".out.bake.lock"),
            "{:?}",
            beside()
        );
        // While one bake holds the folder's lock, another can't take it.
        assert!(lock_bake(&out).is_err());
        drop(held);
        assert!(lock_bake(&out).is_ok());
    }

    #[test]
    fn variables_name_the_lower_and_accept_untrusted_only_without_authority() {
        let env = |name: &str| match name {
            LOWER => Some(OsString::from("/hushrun")),
            LOWER_UNTRUSTED => Some(OsString::from("1")),
            _ => None,
        };
        assert_eq!(named(&env), Some(PathBuf::from("/hushrun")));
        assert!(accepts_untrusted(&env, false));
        assert!(!accepts_untrusted(&env, true));
        let empty = |_: &str| Some(OsString::new());
        assert_eq!(named(&empty), None);
        assert!(!accepts_untrusted(&empty, false));
        // B-63: decided on the run's authority. In CI; with a relay whose server lets it write
        // the repository's trusted scope; or with one whose server hasn't said: it may write
        // what others trust. With no relay, or one whose server says it doesn't, outside CI: no.
        assert!(authority(true, None));
        assert!(authority(false, Some(Some(true))));
        assert!(authority(false, Some(None)));
        assert!(!authority(false, Some(Some(false))));
        assert!(!authority(false, None));
    }

    fn signed_for(w: &World, folder: &Path) -> Signed {
        let hints = w.dir.path().join(SIGNED_HINTS);
        write_signed_hints(&hints, &hint_files(folder).unwrap()).unwrap();
        Signed {
            signer: "issuer https://ci.example, subject https://ci.example/acme/shop".into(),
            baked_json: baked_json_digest(folder).unwrap(),
            hints,
            current: false,
        }
    }

    // covers: B-67
    #[test]
    fn a_signature_over_baked_json_is_evidence_in_ci_and_a_changed_file_is_none() {
        let w = World::new();
        // A laptop's store, untrusted by its own claim, signed by a signer the repository
        // names: read in CI, where no say-so counts, and reused without asking.
        let folder = w.store("signed", &laptop());
        fs::write(folder.join(SIGNATURE), b"{}").unwrap();
        assert!(check_contents(&folder, None, Depth::Names).is_ok());
        let signed = signed_for(&w, &folder);
        let mut ci = w.here(None, false);
        ci.signed = Some(&signed);
        let read = state(Some(&folder), &ci);
        assert_eq!(read.word(), "current");
        let lower = read.lower().unwrap();
        assert!(matches!(lower.evidence, Evidence::Signed(_)));
        assert!(lower.evidence.reuses_unasked());
        // B-16: current only where the run has no backend to ask; a backend that didn't answer
        // for it could have withdrawn a block since.
        assert!(!lower.evidence.current());
        let mut alone = w.here(None, false);
        alone.signed = Some(&signed);
        alone.server = Asked::NoBackend;
        assert!(
            state(Some(&folder), &alone)
                .lower()
                .unwrap()
                .evidence
                .current()
        );
        // A baked.json other than the one signed: no evidence, and in CI nothing else counts.
        let mut changed = laptop();
        changed.toolchains.push("rustc 9.9.9".into());
        fs::write(
            folder.join(BAKED_JSON),
            serde_json::to_vec_pretty(&changed).unwrap(),
        )
        .unwrap();
        assert_eq!(state(Some(&folder), &ci).word(), "untrusted");
        // The server's word, when the store's backend gives it, comes first.
        let server_store = w.store("server", &sample());
        let signed = signed_for(&w, &server_store);
        let server = w.vouched(w.server(), &server_store, None);
        let mut both = w.here(Some(&server), false);
        both.signed = Some(&signed);
        assert!(matches!(
            state(Some(&server_store), &both).lower().unwrap().evidence,
            Evidence::Server(_)
        ));
    }

    // covers: B-67
    #[test]
    fn under_a_signature_a_store_changed_after_the_check_reads_nothing_more() {
        let w = World::new();
        let key = Digest::of(b"a crate");
        let address = blocks::address("rustc/1", key.as_bytes());
        let mut baked = laptop();
        baked.packs = vec![PackRow {
            address,
            ..row(Kind::Rustc, 0, None)
        }];
        let folder = w.store("signed", &baked);
        // One hint the signature covers, and one added after it.
        let step = Digest::of(b"a first key");
        let dir = hints_dir(&folder, &blocks::address("rustc/1", step.as_bytes()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(Hex32::sha256(b"good").to_string()), b"good").unwrap();
        let signed = signed_for(&w, &folder);
        fs::write(dir.join(Hex32::sha256(b"added").to_string()), b"added").unwrap();
        let mut ci = w.here(None, false);
        ci.signed = Some(&signed);
        let read = state(Some(&folder), &ci);
        let lower = w.reading(&read);
        let store = Store::new(&w.upper);
        let now = SystemTime::now();
        let reader = Reader::new(&lower, &Fake);
        assert_eq!(reader.hints_into(&store, Kind::Rustc, &step, now), 1);
        assert_eq!(
            reader.open_into(&store, Kind::Rustc, &key, &key, 0, Some(REPO), now),
            Ok(true)
        );
        // baked.json swapped for another with the same checks (so the same manifest) after the
        // run began: a reader opens the row the run checked, never the new one, and the pack is
        // checked against it as it is opened.
        let mut swapped = baked.clone();
        swapped.packs[0].sha256 = Hex32([9; 32]);
        fs::write(
            folder.join(BAKED_JSON),
            serde_json::to_vec_pretty(&swapped).unwrap(),
        )
        .unwrap();
        let spy = Spy(std::sync::Mutex::new(Vec::new()));
        let late = Reader::new(&lower, &spy);
        assert_eq!(
            late.open_into(&store, Kind::Rustc, &key, &key, 0, Some(REPO), now),
            Ok(true)
        );
        assert_eq!(spy.0.lock().unwrap()[0].0.sha256, Hex32([2; 32]));
        // The hint added after the signature is still not read.
        assert_eq!(late.hints_into(&store, Kind::Rustc, &step, now), 0);
    }

    struct Spy(std::sync::Mutex<Vec<(PackRow, Origin)>>);

    impl Unseal for Spy {
        fn hint(&self, _: &Digest, _: &[u8]) -> Option<Vec<u8>> {
            None
        }
        fn block(
            &self,
            _: &Store,
            _: Kind,
            _: &Path,
            row: &PackRow,
            _: &Digest,
            _: &Digest,
            origin: &Origin,
            _: u64,
            _: Option<&str>,
            _: SystemTime,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push((row.clone(), origin.clone()));
            Ok(())
        }
    }

    // covers: B-63
    #[test]
    fn a_pack_named_twice_is_refused() {
        let mut baked = sample();
        let twice = PackRow {
            outputs: Hex32([0xee; 32]),
            sha256: Hex32([0xef; 32]),
            ..baked.packs[0].clone()
        };
        baked.packs.push(twice);
        let err = Baked::parse(&serde_json::to_vec(&baked).unwrap()).unwrap_err();
        assert!(err.0.contains("twice"), "{err:?}");
    }

    // covers: B-63, B-64
    #[test]
    fn rows_swapped_after_the_server_s_check_open_nothing_of_theirs() {
        // The backend vouched for a row, as the run began. baked.json then changes on a
        // writable folder: same checks (so the same manifest), the row's outputs and pack another
        // build's. What the run opens is what was checked, or nothing.
        let w = World::new();
        let key = Digest::of(b"a crate");
        let address = blocks::address("rustc/1", key.as_bytes());
        let mut baked = sample();
        baked.packs = vec![PackRow {
            address,
            ..row(Kind::Rustc, 0, Some("protected/github.com/acme/shop"))
        }];
        let folder = w.store("verified", &baked);
        let server = w.vouched(w.server(), &folder, None);
        let mut swapped = baked.clone();
        swapped.packs[0].outputs = Hex32([0xee; 32]);
        swapped.packs[0].sha256 = Hex32([0xef; 32]);
        let swap = || {
            fs::write(
                folder.join(BAKED_JSON),
                serde_json::to_vec_pretty(&swapped).unwrap(),
            )
            .unwrap();
        };
        let open = |lower: &Lower| {
            let spy = Spy(std::sync::Mutex::new(Vec::new()));
            let reader = Reader::new(lower, &spy);
            let store = Store::new(&w.upper);
            let _ = reader.open_into(
                &store,
                Kind::Rustc,
                &key,
                &key,
                0,
                Some(REPO),
                SystemTime::now(),
            );
            spy.0.into_inner().unwrap()
        };
        // Swapped after the state was read: the reader opens the checked row.
        let lower = w.reading(&state(Some(&folder), &w.here(Some(&server), false)));
        swap();
        let seen = open(&lower);
        assert_eq!(seen.len(), 1);
        assert!(
            seen.iter().all(|(row, _)| row.outputs == Hex32([3; 32])),
            "opened with a row nobody checked: {seen:?}"
        );
        // Swapped between the server's check and the state: the answer was for other bytes, so
        // it is no evidence, and nothing is read.
        assert_eq!(
            state(Some(&folder), &w.here(Some(&server), false)).word(),
            "untrusted"
        );
    }

    // covers: B-65
    #[cfg(unix)]
    #[test]
    fn the_default_folder_is_read_only_when_root_or_this_user_owns_it_and_nobody_else_writes_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let w = World::new();
        let folder = w.store("hushrun", &laptop());
        assert_eq!(default_refused(&folder), None);
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            default_refused(&folder)
                .unwrap()
                .contains("anyone may write")
        );
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(folder.join(BAKED_JSON), fs::Permissions::from_mode(0o666)).unwrap();
        assert!(
            default_refused(&folder)
                .unwrap()
                .contains("anyone may write")
        );
        fs::set_permissions(folder.join(BAKED_JSON), fs::Permissions::from_mode(0o644)).unwrap();
        let link = w.dir.path().join("linked");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        assert!(default_refused(&link).unwrap().contains("is a link"));
        // Its group may write it (a volume mounted with a group, 2775): refused too, with the fix.
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o2775)).unwrap();
        let why = default_refused(&folder).unwrap();
        assert!(why.contains("group may write"), "{why}");
        assert!(
            why.contains(&format!("`chmod -R g-w {}` fixes it", folder.display())),
            "{why}"
        );
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        // And so the folders its hints are in: a hint is checked by its name alone.
        let hints = folder.join(HINTS).join(Hex32([3; 32]).to_string());
        fs::create_dir_all(&hints).unwrap();
        fs::set_permissions(folder.join(HINTS), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&hints, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            default_refused(&folder)
                .unwrap()
                .contains("anyone may write")
        );
        fs::set_permissions(&hints, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(folder.join(HINTS), fs::Permissions::from_mode(0o775)).unwrap();
        assert!(
            default_refused(&folder)
                .unwrap()
                .contains("group may write")
        );
        fs::set_permissions(folder.join(HINTS), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(default_refused(&folder), None);
        // Owned by neither root nor the user who runs: refused.
        let mine = rustix::process::geteuid().as_raw();
        let writable = |_: &Path| false;
        if mine != 0 {
            assert_eq!(refused_for(&folder, mine, &writable), None);
            let said = refused_for(&folder, mine + 1, &writable).unwrap();
            assert!(said.contains("another user"), "{said}");
            // The line names the variable that reads it anyway.
            assert!(
                said.contains(&format!("{LOWER}={}", folder.display())),
                "{said}"
            );
            // B-71: on a read-only mount, as a user-namespaced container sees a root-owned one
            // (owned by nobody, 65534), nobody can write it: read.
            assert_eq!(refused_for(&folder, mine + 1, &|_: &Path| true), None);
        }
        // Writable by its group, but read-only mounted: read too; a link never.
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(refused_for(&folder, mine, &writable).is_some());
        assert_eq!(refused_for(&folder, mine, &|_: &Path| true), None);
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        // Only the default folder no variable named is checked so: one a variable names is the
        // person's choice.
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o777)).unwrap();
        let mut named = w.here(None, true);
        assert_ne!(state(Some(&folder), &named).word(), "invalid");
        named.default = true;
        assert_eq!(state(Some(&folder), &named).word(), "invalid");
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // covers: B-63, B-64
    // Under the server's word a hint isn't vouched for: one whose bytes aren't its name is never
    // read.
    #[test]
    fn under_the_server_s_word_a_hint_is_read_only_when_its_bytes_are_its_name() {
        let w = World::new();
        let folder = w.store("server", &sample());
        let step = Digest::of(b"a first key");
        let dir = hints_dir(&folder, &blocks::address("rustc/1", step.as_bytes()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(Hex32::sha256(b"good").to_string()), b"good").unwrap();
        fs::write(dir.join(Hex32::sha256(b"other").to_string()), b"swapped").unwrap();
        let server = w.vouched(w.server(), &folder, None);
        let read = state(Some(&folder), &w.here(Some(&server), false));
        assert!(matches!(
            read.lower().unwrap().evidence,
            Evidence::Server(_)
        ));
        let lower = w.reading(&read);
        let store = Store::new(&w.upper);
        let reader = Reader::new(&lower, &Fake);
        assert_eq!(
            reader.hints_into(&store, Kind::Rustc, &step, SystemTime::now()),
            1
        );
    }

    // covers: B-65
    #[test]
    fn with_no_variable_the_default_folder_is_read_when_it_holds_a_store() {
        let w = World::new();
        let folder = w.store("hushrun", &laptop());
        assert_eq!(default_lower(&folder), Some(folder.clone()));
        assert_eq!(default_lower(&w.dir.path().join("nothing")), None);
        // Set and empty: none, whatever is there.
        let empty = |name: &str| (name == LOWER).then(OsString::new);
        assert_eq!(named(&empty), None);
        let set = |name: &str| (name == LOWER).then(|| OsString::from("/elsewhere"));
        assert_eq!(named(&set), Some(PathBuf::from("/elsewhere")));
    }
}
