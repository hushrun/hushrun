//! The part of a TUF client (The Update Framework, specification 1.0) that verifies metadata,
//! for the Sigstore trust root (T-12): root rotation, the timestamp, snapshot and targets roles,
//! and target files. It takes bytes and touches no network or disk: `hushrun sigstore refresh`
//! fetches with it, and readers re-verify a cached trust root with it, offline, so that a
//! poisoned cache can't add a certificate authority.
//!
//! Signatures cover the canonical JSON of `signed` (securesystemslib's form). Keys are ECDSA
//! P-256 (PEM `SubjectPublicKeyInfo`, hex DER signatures) or Ed25519 (hex).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;
use sha2::Digest as _;

use hushrun_core::signers::PublicKey;
use hushrun_core::time::Time;

use crate::keys::verify_signature;

/// Sigstore's TUF root, version 15, from which updates start (`sigstore/README.md`).
pub const SIGSTORE_ROOT: &[u8] = include_bytes!("../sigstore/root.json");
/// The `targets.json` that pins the vendored trust root.
pub const SIGSTORE_TARGETS: &[u8] = include_bytes!("../sigstore/targets.json");
/// Where Sigstore publishes it.
pub const SIGSTORE_MIRROR: &str = "https://tuf-repo-cdn.sigstore.dev";
/// The target that holds the trust root.
pub const TRUSTED_ROOT_TARGET: &str = "trusted_root.json";
/// The most root versions an update walks through.
pub const MAX_ROOT_ROTATIONS: u64 = 1024;
/// Metadata larger than this is refused before parsing.
pub const MAX_METADATA_BYTES: usize = 1 << 20;

#[derive(Deserialize)]
struct Metadata {
    signatures: Vec<RawSignature>,
    signed: Value,
}

#[derive(Deserialize)]
struct RawSignature {
    keyid: String,
    sig: String,
}

#[derive(Deserialize)]
struct RawKey {
    keytype: String,
    scheme: String,
    keyval: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawRole {
    keyids: Vec<String>,
    threshold: u64,
}

/// A role's keys and threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub keyids: BTreeSet<String>,
    pub threshold: usize,
}

/// A root, parsed: its keys and the roles they sign for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub version: u64,
    pub expires: Time,
    pub consistent_snapshot: bool,
    keys: BTreeMap<String, PublicKey>,
    roles: BTreeMap<String, Role>,
}

/// A role's metadata, verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub version: u64,
    pub expires: Time,
    /// The whole `signed` object.
    pub body: Value,
}

/// What `targets.json` says about one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub length: u64,
    /// Hex digests by algorithm (`sha256`, `sha512`).
    pub hashes: BTreeMap<String, String>,
}

fn parse(bytes: &[u8]) -> Result<Metadata, String> {
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(format!(
            "{} bytes of metadata; the limit is 1 MiB",
            bytes.len()
        ));
    }
    serde_json::from_slice(bytes).map_err(|e| format!("not TUF metadata: {e}"))
}

fn field<'a>(signed: &'a Value, name: &str) -> Result<&'a Value, String> {
    signed.get(name).ok_or_else(|| format!("no `{name}`"))
}

fn header(signed: &Value, kind: &str) -> Result<(u64, Time), String> {
    let found = field(signed, "_type")?.as_str().unwrap_or_default();
    if found != kind {
        return Err(format!("a {found:?} role, expected {kind:?}"));
    }
    let version = field(signed, "version")?
        .as_u64()
        .ok_or("`version` is not a number")?;
    let expires = field(signed, "expires")?
        .as_str()
        .ok_or("`expires` is not a string")
        .and_then(|t| Time::parse(t).map_err(|_| "`expires` is not a time"))?;
    Ok((version, expires))
}

/// A TUF key as hushrun can verify with it; `None` for other key types, which then count for
/// nothing.
fn key(raw: &RawKey) -> Option<PublicKey> {
    let public = raw.keyval.get("public")?.as_str()?;
    match (raw.keytype.as_str(), raw.scheme.as_str()) {
        ("ecdsa" | "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp256") => {
            let (_, der) = der::pem::decode_vec(public.trim().as_bytes()).ok()?;
            PublicKey::from_p256_spki(&der).ok()
        }
        ("ed25519", "ed25519") => {
            let raw: [u8; 32] = hex::decode(public).ok()?.try_into().ok()?;
            Some(PublicKey::Ed25519(raw))
        }
        _ => None,
    }
}

impl Root {
    /// Parses a root without verifying its signatures.
    pub fn parse(bytes: &[u8]) -> Result<Root, String> {
        let metadata = parse(bytes)?;
        Root::from_signed(&metadata.signed)
    }

    fn from_signed(signed: &Value) -> Result<Root, String> {
        let (version, expires) = header(signed, "root")?;
        let raw_keys: BTreeMap<String, RawKey> =
            serde_json::from_value(field(signed, "keys")?.clone())
                .map_err(|e| format!("`keys`: {e}"))?;
        let raw_roles: BTreeMap<String, RawRole> =
            serde_json::from_value(field(signed, "roles")?.clone())
                .map_err(|e| format!("`roles`: {e}"))?;
        let keys = raw_keys
            .iter()
            .filter_map(|(id, raw)| key(raw).map(|k| (id.clone(), k)))
            .collect();
        let mut roles = BTreeMap::new();
        for (name, role) in raw_roles {
            if role.threshold == 0 {
                return Err(format!("role {name} has threshold 0"));
            }
            roles.insert(
                name,
                Role {
                    keyids: role.keyids.into_iter().collect(),
                    threshold: usize::try_from(role.threshold).unwrap_or(usize::MAX),
                },
            );
        }
        for role in ["root", "timestamp", "snapshot", "targets"] {
            if !roles.contains_key(role) {
                return Err(format!("no `{role}` role"));
            }
        }
        Ok(Root {
            version,
            expires,
            consistent_snapshot: signed
                .get("consistent_snapshot")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            keys,
            roles,
        })
    }

    /// The distinct keys of `role` whose signatures over `signed` verify.
    fn signers(&self, role: &str, metadata: &Metadata) -> usize {
        let Some(role) = self.roles.get(role) else {
            return 0;
        };
        let message = canonical_json(&metadata.signed);
        let mut counted = BTreeSet::new();
        for signature in &metadata.signatures {
            if !role.keyids.contains(&signature.keyid) || counted.contains(&signature.keyid) {
                continue;
            }
            let Some(key) = self.keys.get(&signature.keyid) else {
                continue;
            };
            let Ok(sig) = hex::decode(&signature.sig) else {
                continue;
            };
            if verify_signature(key, &message, &sig) {
                counted.insert(signature.keyid.clone());
            }
        }
        counted.len()
    }

    fn check_threshold(&self, role: &str, metadata: &Metadata, what: &str) -> Result<(), String> {
        let threshold = self.roles.get(role).map_or(usize::MAX, |r| r.threshold);
        let signers = self.signers(role, metadata);
        if signers < threshold {
            return Err(format!(
                "{what}: {signers} of root version {}'s {role} keys signed it, and it needs {threshold}",
                self.version
            ));
        }
        Ok(())
    }

    /// Verifies the next root version: signed by the threshold of this root's root keys and of
    /// its own (TUF 5.3.4), with this version plus one (5.3.5).
    pub fn verify_next(&self, bytes: &[u8]) -> Result<Root, String> {
        let metadata = parse(bytes)?;
        let next = Root::from_signed(&metadata.signed)?;
        let what = format!("root version {}", next.version);
        if next.version != self.version + 1 {
            return Err(format!(
                "{what} follows version {}; expected {}",
                self.version,
                self.version + 1
            ));
        }
        self.check_threshold("root", &metadata, &what)?;
        next.check_threshold("root", &metadata, &what)?;
        Ok(next)
    }

    /// Verifies the metadata of `role` (`timestamp`, `snapshot` or `targets`) with this root's
    /// keys for it. Versions and expiry are the caller's to check.
    pub fn verify_role(&self, role: &str, bytes: &[u8]) -> Result<Signed, String> {
        let metadata = parse(bytes)?;
        let (version, expires) = header(&metadata.signed, role)?;
        self.check_threshold(role, &metadata, &format!("{role} version {version}"))?;
        Ok(Signed {
            version,
            expires,
            body: metadata.signed,
        })
    }
}

impl Signed {
    /// The version `meta.<name>` names (timestamp and snapshot roles).
    pub fn meta_version(&self, name: &str) -> Result<u64, String> {
        self.body
            .get("meta")
            .and_then(|m| m.get(name))
            .and_then(|m| m.get("version"))
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("no version of {name}"))
    }

    /// The length and hashes `meta.<name>` gives, when it gives them.
    pub fn meta_file(&self, name: &str) -> Option<Target> {
        let meta = self.body.get("meta")?.get(name)?;
        target_of(meta).ok()
    }

    /// A target of this `targets` metadata.
    pub fn target(&self, name: &str) -> Result<Target, String> {
        let meta = self
            .body
            .get("targets")
            .and_then(|t| t.get(name))
            .ok_or_else(|| format!("targets.json has no target {name}"))?;
        target_of(meta)
    }
}

fn target_of(meta: &Value) -> Result<Target, String> {
    let length = meta
        .get("length")
        .and_then(Value::as_u64)
        .ok_or("a target without a length")?;
    let hashes: BTreeMap<String, String> = meta
        .get("hashes")
        .and_then(Value::as_object)
        .ok_or("a target without hashes")?
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_ascii_lowercase())))
        .collect();
    if !hashes.contains_key("sha256") && !hashes.contains_key("sha512") {
        return Err("a target without a SHA-256 or SHA-512 hash".into());
    }
    Ok(Target { length, hashes })
}

impl Target {
    /// Checks a downloaded file against its length and every hash hushrun knows.
    pub fn check(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() as u64 != self.length {
            return Err(format!(
                "{} bytes, and the metadata says {}",
                bytes.len(),
                self.length
            ));
        }
        for (algorithm, expected) in &self.hashes {
            let actual = match algorithm.as_str() {
                "sha256" => hex::encode(sha2::Sha256::digest(bytes)),
                "sha512" => hex::encode(sha2::Sha512::digest(bytes)),
                _ => continue,
            };
            if &actual != expected {
                return Err(format!("its {algorithm} is {actual}, not {expected}"));
            }
        }
        Ok(())
    }

    /// The SHA-256 hex digest, which names the file in a consistent-snapshot repository.
    pub fn sha256(&self) -> Option<&str> {
        self.hashes.get("sha256").map(String::as_str)
    }
}

/// securesystemslib's canonical JSON: keys sorted, no whitespace, strings escaping only `"`
/// and `\`, integers only.
pub fn canonical_json(value: &Value) -> Vec<u8> {
    fn string(text: &str, out: &mut Vec<u8>) {
        out.push(b'"');
        for byte in text.bytes() {
            if byte == b'"' || byte == b'\\' {
                out.push(b'\\');
            }
            out.push(byte);
        }
        out.push(b'"');
    }
    fn write(value: &Value, out: &mut Vec<u8>) {
        match value {
            Value::Null => out.extend(b"null"),
            Value::Bool(b) => out.extend(if *b { &b"true"[..] } else { b"false" }),
            Value::Number(n) => out.extend(n.to_string().bytes()),
            Value::String(s) => string(s, out),
            Value::Array(items) => {
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    write(item, out);
                }
                out.push(b']');
            }
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                out.push(b'{');
                for (i, (k, v)) in entries.into_iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    string(k, out);
                    out.push(b':');
                    write(v, out);
                }
                out.push(b'}');
            }
        }
    }
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

/// A cached trust root, verified offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The newest root version, which signed `targets`.
    pub root_version: u64,
    pub targets_version: u64,
}

/// Verifies a trust root fetched earlier, offline (T-12): `roots` are the root versions after
/// `initial`, in order; `targets` is a `targets.json` signed by the newest one's targets keys;
/// and `trusted_root` is the file it pins. Expiry is not checked: verification must keep
/// working offline, and freshness is what `hushrun sigstore refresh` is for.
pub fn verify_cached(
    initial: &[u8],
    roots: &[Vec<u8>],
    targets: &[u8],
    trusted_root: &[u8],
) -> Result<Verified, String> {
    let mut root = Root::parse(initial).map_err(|e| format!("the initial root: {e}"))?;
    for next in roots {
        root = root.verify_next(next)?;
    }
    let targets = root.verify_role("targets", targets)?;
    targets
        .target(TRUSTED_ROOT_TARGET)?
        .check(trusted_root)
        .map_err(|e| format!("{TRUSTED_ROOT_TARGET}: {e}"))?;
    Ok(Verified {
        root_version: root.version,
        targets_version: targets.version,
    })
}

#[cfg(test)]
mod tests;
