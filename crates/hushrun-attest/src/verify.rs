//! Event verification (E-8). An event is accepted only if every step holds, in order;
//! otherwise it is ignored, and the rejection says why.
//!
//! \[M10\] An event is a plain DSSE envelope, or a Sigstore bundle around one (E-1). \[ADR 0031\]
//! A signature counts when it is a machine's that the repository's signers list (T-15), valid at
//! the trusted time of its RFC 3161 timestamp (E-10) or, without one, now; or a signing
//! certificate of the repository's CI (E-11).

use std::fmt;

use hushrun_core::CANONICALIZATION;
use hushrun_core::digest::Digest;
use hushrun_core::event::{
    Event, EventKind, STATEMENT_TYPE, Statement, pinned_digest, predicate_type,
};
use hushrun_core::names::{ScopeName, StageName};
use hushrun_core::run::Custody;
use hushrun_core::signers::{CI_SIGNER, CertIdentity, Machine, PublicKey, Signers};
use hushrun_core::time::{Dur, Time};

use crate::bundle::{Bundle, Material, Object};
use crate::dsse::{self, Envelope, PAYLOAD_TYPE, pae};
use crate::keys::verify_signature;
use crate::timestamp;
use crate::trust_root::TrustedRoot;
use crate::x509::Cert;

/// How far in the future an event's `time` may be (clock-skew allowance, E-8 step 6).
pub const MAX_FUTURE: Dur = Dur::from_secs(600);
/// How long after `time` a check may say it finished.
pub const MAX_FINISHED_AFTER_TIME: Dur = Dur::from_secs(60);
/// \[M10\] How far `time` may be from the trusted time of a timestamped signature, either way
/// (E-10): the same clock-skew allowance.
pub const MAX_TIMESTAMP_SKEW: Dur = MAX_FUTURE;

/// What the reader expects: the repository and scope, and where the event was stored.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    pub repository: &'a str,
    pub scope: &'a ScopeName,
    pub location: Location<'a>,
}

/// Where the bytes came from (E-8 steps 4 and 5).
#[derive(Debug, Clone, Copy)]
pub enum Location<'a> {
    /// `v1/events/<scope>/<digest>/<ULID>.<kind>.<stage or _>.json`.
    Events {
        digest: &'a Digest,
        kind: EventKind,
        stage: Option<&'a StageName>,
    },
    /// `v1/deployed/<scope>/<stage or _>/…`: no queried digest; the event's own subject is used.
    Deployed { stage: Option<&'a StageName> },
    /// A file outside the ledger (`hushrun verify <file>`): no storage key to compare.
    File,
}

/// An accepted event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub event: Event,
    /// The source digest the statement is about.
    pub subject_digest: Digest,
    /// Who signed it (T-15): `ci`, or a machine's owner, `user:<email>` or `agent:<name>`.
    pub signer: String,
    /// The key id of the key that signed it; for a keyless signature, `sha256:` of the signing
    /// certificate.
    pub keyid: String,
    /// The lower of the claimed custody and the signer's maximum (E-8 step 8).
    pub effective_custody: Custody,
    /// \[M10\] What made the signature count.
    pub signed_by: SignedBy,
    /// \[M10\] When a timestamp authority of the trust root says the signature existed (E-10).
    pub trusted_time: Option<Time>,
}

/// \[M10\] What made a signature count (E-8 step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignedBy {
    /// A machine's key, valid at the trusted time or, without one, now (T-15): its name.
    Machine(String),
    /// A signing certificate of the repository's CI (T-15, E-11).
    Identity(Box<CertIdentity>),
}

/// Why an event was ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// Step 1.
    Envelope(String),
    /// Step 2.
    Signature(String),
    /// Step 3.
    Statement(String),
    /// Step 4.
    Subject(String),
    /// Step 5.
    StorageKey(String),
    /// Step 6.
    Time(String),
    /// Step 7.
    Constraint(String),
}

impl Rejection {
    /// The E-8 step that failed.
    pub fn step(&self) -> u8 {
        match self {
            Rejection::Envelope(_) => 1,
            Rejection::Signature(_) => 2,
            Rejection::Statement(_) => 3,
            Rejection::Subject(_) => 4,
            Rejection::StorageKey(_) => 5,
            Rejection::Time(_) => 6,
            Rejection::Constraint(_) => 7,
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (what, why) = match self {
            Rejection::Envelope(w) => ("envelope", w),
            Rejection::Signature(w) => ("signature", w),
            Rejection::Statement(w) => ("statement", w),
            Rejection::Subject(w) => ("subject", w),
            Rejection::StorageKey(w) => ("storage key", w),
            Rejection::Time(w) => ("time", w),
            Rejection::Constraint(w) => ("content", w),
        };
        write!(f, "rejected at step {} ({what}): {why}", self.step())
    }
}

/// Verifies `bytes` against `signers` (E-8), with the public-good Sigstore trust root for
/// certificates and timestamps.
pub fn verify(
    bytes: &[u8],
    signers: &Signers,
    expected: &Expected<'_>,
    now: Time,
) -> Result<Verified, Rejection> {
    verify_with(bytes, signers, TrustedRoot::public_good(), expected, now)
}

/// Verifies `bytes` against `signers` (E-8), with `sigstore` as the trust root for signing
/// certificates and timestamps (T-12).
pub fn verify_with(
    bytes: &[u8],
    signers: &Signers,
    sigstore: &TrustedRoot,
    expected: &Expected<'_>,
    now: Time,
) -> Result<Verified, Rejection> {
    // 1 and 2. Envelope or bundle, payload type, and a signature that counts.
    let (payload, signer) = signed(bytes, PAYLOAD_TYPE, signers, sigstore, now)?;
    let Signer {
        signer,
        keyid,
        signed_by,
        trusted_time,
    } = signer;

    // 3. A Statement v1 with the event predicate, version 1.
    let statement: Statement =
        serde_json::from_slice(&payload).map_err(|e| Rejection::Statement(e.to_string()))?;
    if statement.statement_type != STATEMENT_TYPE {
        return Err(Rejection::Statement(format!(
            "_type {:?}, expected {STATEMENT_TYPE}",
            statement.statement_type
        )));
    }
    let wanted = predicate_type();
    if statement.predicate_type != wanted {
        let family = wanted.trim_end_matches("v1");
        let why = if statement.predicate_type.starts_with(family) {
            format!("unknown major version {:?}", statement.predicate_type)
        } else {
            format!(
                "predicate type {:?}, expected {wanted}",
                statement.predicate_type
            )
        };
        return Err(Rejection::Statement(why));
    }
    let event: Event = serde_json::from_value(statement.predicate.clone())
        .map_err(|e| Rejection::Statement(format!("predicate: {e}")))?;

    // 4. The subject matches the query.
    let [subject] = statement.subject.as_slice() else {
        return Err(Rejection::Subject(format!(
            "{} subjects; events have exactly one",
            statement.subject.len()
        )));
    };
    let subject_digest = subject
        .digest
        .get("sha256")
        .and_then(|hex| Digest::from_hex(hex).ok())
        .ok_or_else(|| Rejection::Subject("the subject has no sha256 digest".into()))?;
    let expected_name = hushrun_core::event::subject_name(expected.repository, expected.scope);
    if subject.name != expected_name {
        return Err(Rejection::Subject(format!(
            "subject {:?}, expected {expected_name:?}",
            subject.name
        )));
    }
    if event.repository != expected.repository || &event.scope != expected.scope {
        return Err(Rejection::Subject(format!(
            "the predicate names {}#{}, expected {expected_name}",
            event.repository, event.scope
        )));
    }
    if event.canonicalization != CANONICALIZATION {
        return Err(Rejection::Subject(format!(
            "canonicalization {:?}; digests never match across canonicalizations",
            event.canonicalization
        )));
    }

    // 5. Kind and stage match the storage key.
    match expected.location {
        Location::Events {
            digest,
            kind,
            stage,
        } => {
            if &subject_digest != digest {
                return Err(Rejection::Subject(format!(
                    "the statement is about {subject_digest}, stored under {digest}"
                )));
            }
            if event.kind != kind {
                return Err(Rejection::StorageKey(format!(
                    "a {} event stored as {kind}",
                    event.kind
                )));
            }
            if event.stage.as_ref() != stage {
                return Err(Rejection::StorageKey(format!(
                    "stage {}, stored under {}",
                    event.stage.as_ref().map_or("_", |s| s.as_str()),
                    stage.map_or("_", |s| s.as_str())
                )));
            }
        }
        Location::Deployed { stage } => {
            if event.kind != EventKind::Deployed || event.stage.as_ref() != stage {
                return Err(Rejection::StorageKey(format!(
                    "a {} event at stage {} in the deployed index of `{}`",
                    event.kind,
                    event.stage.as_ref().map_or("_", |s| s.as_str()),
                    stage.map_or("_", |s| s.as_str())
                )));
            }
        }
        Location::File => {}
    }

    // 6. Plausible timestamps; `time` agrees with the trusted time.
    if let Some(trusted) = trusted_time {
        let earliest = trusted.checked_sub(MAX_TIMESTAMP_SKEW).unwrap_or(trusted);
        if event.time > trusted.saturating_add(MAX_TIMESTAMP_SKEW) || event.time < earliest {
            return Err(Rejection::Time(format!(
                "signed at {}, more than 10 minutes from its trusted timestamp ({trusted})",
                event.time
            )));
        }
    }
    if event.time > now.saturating_add(MAX_FUTURE) {
        return Err(Rejection::Time(format!(
            "signed at {}, more than 10 minutes after now ({now})",
            event.time
        )));
    }
    if let Some(check) = &event.check
        && check.finished_at > event.time.saturating_add(MAX_FINISHED_AFTER_TIME)
    {
        return Err(Rejection::Time(format!(
            "the check finished at {}, after it was signed at {}",
            check.finished_at, event.time
        )));
    }

    // 7. Kind-specific constraints.
    check_constraints(&event)?;

    // 8. Effective custody.
    let max: Custody = Signers::max_custody(&signer);
    Ok(Verified {
        effective_custody: event.producer.custody.min(max),
        event,
        subject_digest,
        signer,
        keyid,
        signed_by,
        trusted_time,
    })
}

/// Who signed, as step 2 found it.
pub(crate) struct Signer {
    pub(crate) signer: String,
    pub(crate) keyid: String,
    pub(crate) signed_by: SignedBy,
    pub(crate) trusted_time: Option<Time>,
}

/// E-8's steps 1 and 2 for an envelope of `payload_type`, plain or in a bundle: its payload, and
/// the signature that counts: a listed machine's, valid at the trusted time or now, or a
/// certificate of the repository's CI (T-15).
/// Events use it, and \[ADR 0026\] so do GC statements (L-9).
pub(crate) fn signed(
    bytes: &[u8],
    payload_type: &str,
    signers: &Signers,
    sigstore: &TrustedRoot,
    now: Time,
) -> Result<(Vec<u8>, Signer), Rejection> {
    let object = Object::parse(bytes).map_err(|e| Rejection::Envelope(e.to_string()))?;
    let (envelope, bundle) = match &object {
        Object::Envelope(envelope) => (envelope, None),
        Object::Bundle(bundle) => (bundle_envelope(bundle)?, Some(bundle.as_ref())),
    };
    if envelope.payload_type != payload_type {
        return Err(Rejection::Envelope(format!(
            "payload type {:?}, expected {payload_type}",
            envelope.payload_type
        )));
    }
    let payload = envelope
        .payload_bytes()
        .map_err(|e| Rejection::Envelope(e.to_string()))?;
    let message = pae(&envelope.payload_type, &payload);
    let signer = match bundle {
        None => key_signature(envelope, signers, &message, None, now),
        Some(bundle) => bundle_signature(bundle, envelope, signers, sigstore, &message, now),
    }?;
    Ok((payload, signer))
}

/// A bundle's envelope (E-1): version 0.3, around a DSSE envelope with exactly one signature.
/// \[ADR 0028\] Which of `keys` signed `bytes`, an event as a ledger stores it: E-8's steps 1
/// and 2 against a set of keys instead of the signers, so that a server can accept a write from a
/// person or an agent only when one of their registered machines signed it. It checks the
/// envelope or bundle, its payload type and a signature by one of the keys over the envelope's
/// PAE, and returns that key's id. It reads no statement and checks no time: what counts is the
/// readers' business (E-8), and a keyless signature never matches, since a machine signs with
/// its key. `keys` may name the same key twice; it counts once.
pub fn signed_by_one_of(bytes: &[u8], keys: &[PublicKey]) -> Result<String, Rejection> {
    let object = Object::parse(bytes).map_err(|e| Rejection::Envelope(e.to_string()))?;
    let envelope = match &object {
        Object::Envelope(envelope) => envelope,
        Object::Bundle(bundle) => {
            let envelope = bundle_envelope(bundle)?;
            match bundle.material().map_err(Rejection::Envelope)? {
                Material::Key(hint) if hint == envelope.signatures[0].keyid => {}
                Material::Key(hint) => {
                    return Err(Rejection::Envelope(format!(
                        "the bundle's key hint {hint} is not the signature's key id {}",
                        envelope.signatures[0].keyid
                    )));
                }
                Material::Certificate(_) => {
                    return Err(Rejection::Signature(
                        "a keyless signature; a machine signs with its own key".into(),
                    ));
                }
            }
            envelope
        }
    };
    if envelope.payload_type != PAYLOAD_TYPE {
        return Err(Rejection::Envelope(format!(
            "payload type {:?}, expected {PAYLOAD_TYPE}",
            envelope.payload_type
        )));
    }
    let payload = envelope
        .payload_bytes()
        .map_err(|e| Rejection::Envelope(e.to_string()))?;
    let message = pae(&envelope.payload_type, &payload);
    for signature in &envelope.signatures {
        let Some(key) = keys.iter().find(|k| k.keyid() == signature.keyid) else {
            continue;
        };
        let Some(sig) = dsse::signature_bytes(signature) else {
            continue;
        };
        if verify_signature(key, &message, &sig) {
            return Ok(signature.keyid.clone());
        }
    }
    Err(Rejection::Signature(format!(
        "no valid signature by one of {} registered keys",
        keys.len()
    )))
}

fn bundle_envelope(bundle: &Bundle) -> Result<&Envelope, Rejection> {
    if !bundle.is_supported() {
        return Err(Rejection::Envelope(format!(
            "a bundle of media type {:?}; hushrun reads version 0.3",
            bundle.media_type
        )));
    }
    let envelope = bundle.dsse_envelope.as_ref().ok_or_else(|| {
        Rejection::Envelope("the bundle holds no DSSE envelope, so it is no event".into())
    })?;
    if envelope.signatures.len() != 1 {
        return Err(Rejection::Envelope(format!(
            "the bundle's envelope has {} signatures; a bundle's has exactly one",
            envelope.signatures.len()
        )));
    }
    Ok(envelope)
}

/// The first signature of `envelope` by a listed machine that counts (T-15).
fn key_signature(
    envelope: &Envelope,
    signers: &Signers,
    message: &[u8],
    trusted_time: Option<Time>,
    now: Time,
) -> Result<Signer, Rejection> {
    let mut reasons = Vec::new();
    for signature in &envelope.signatures {
        let Some(machine) = signers.machine(&signature.keyid) else {
            continue;
        };
        let Some(sig) = dsse::signature_bytes(signature) else {
            continue;
        };
        if !verify_signature(&machine.key, message, &sig) {
            continue;
        }
        match machine_counts(machine, trusted_time, now) {
            Ok(()) => {
                return Ok(Signer {
                    signer: machine.owner.clone(),
                    keyid: signature.keyid.clone(),
                    signed_by: SignedBy::Machine(machine.name.clone()),
                    trusted_time,
                });
            }
            Err(reason) => reasons.push(format!(
                "{} ({}, {}): {reason}",
                signature.keyid, machine.name, machine.owner
            )),
        }
    }
    if !reasons.is_empty() {
        return Err(Rejection::Signature(reasons.join("; ")));
    }
    let ids: Vec<&str> = envelope
        .signatures
        .iter()
        .map(|s| s.keyid.as_str())
        .collect();
    Err(Rejection::Signature(format!(
        "no valid signature by a machine of a member or an agent of the org (key ids: {})",
        if ids.is_empty() {
            "none".to_string()
        } else {
            ids.join(", ")
        }
    )))
}

/// Whether a verified signature by `machine` counts (T-15): when the trusted time, or without
/// one the time now, is before the machine was revoked.
fn machine_counts(machine: &Machine, trusted_time: Option<Time>, now: Time) -> Result<(), String> {
    let window = machine.window();
    match trusted_time {
        Some(at) if window.contains(at) => Ok(()),
        Some(at) => Err(format!(
            "timestamped at {at}, after the machine stopped counting ({})",
            window
        )),
        None if window.contains(now) => Ok(()),
        None => Err(format!(
            "the machine stopped counting ({window}), and without a trusted timestamp its events count only while it counts"
        )),
    }
}

/// A bundle's signature (E-1, E-10, E-11): its timestamps give the trusted time, then a key's
/// signature counts as for an envelope, and a certificate's when the trust root issued it, it
/// was valid at the trusted time, and it is the repository's CI's (T-15).
fn bundle_signature(
    bundle: &Bundle,
    envelope: &Envelope,
    signers: &Signers,
    sigstore: &TrustedRoot,
    message: &[u8],
    now: Time,
) -> Result<Signer, Rejection> {
    let material = bundle.material().map_err(Rejection::Envelope)?;
    let signature = &envelope.signatures[0];
    let sig = dsse::signature_bytes(signature)
        .ok_or_else(|| Rejection::Envelope("the signature is not standard padded base64".into()))?;
    let (trusted_time, unverified) = trusted_time(bundle, &sig, sigstore)?;
    let with_timestamps = |reason: String| {
        if unverified.is_empty() {
            reason
        } else {
            format!(
                "{reason} (timestamps that don't verify: {})",
                unverified.join("; ")
            )
        }
    };
    let der = match material {
        Material::Key(hint) => {
            if hint != signature.keyid {
                return Err(Rejection::Envelope(format!(
                    "the bundle's key hint {hint} is not the signature's key id {}",
                    signature.keyid
                )));
            }
            return key_signature(envelope, signers, message, trusted_time, now).map_err(
                |rejection| match rejection {
                    Rejection::Signature(reason) => Rejection::Signature(with_timestamps(reason)),
                    other => other,
                },
            );
        }
        Material::Certificate(der) => der,
    };
    let refuse = |reason: String| Rejection::Signature(with_timestamps(reason));
    let at = trusted_time.ok_or_else(|| {
        refuse(
            "a keyless signature needs a trusted timestamp: its certificate is valid for minutes"
                .into(),
        )
    })?;
    let identity = certified(&der, sigstore, message, &sig, at).map_err(refuse)?;
    if signers.ci_matching(&identity).is_none() {
        let named: Vec<String> = signers.ci.iter().map(ToString::to_string).collect();
        return Err(refuse(format!(
            "the certificate ({}) isn't the repository's CI ({})",
            describe(&identity),
            if named.is_empty() {
                "the server names none yet".to_string()
            } else {
                named.join(", ")
            }
        )));
    }
    Ok(Signer {
        signer: CI_SIGNER.to_string(),
        keyid: Digest::of(&der).to_string(),
        signed_by: SignedBy::Identity(Box::new(identity)),
        trusted_time: Some(at),
    })
}

/// The trusted time of a bundle's signature: the first of its timestamps that verifies (E-10),
/// and why the others don't.
fn trusted_time(
    bundle: &Bundle,
    sig: &[u8],
    sigstore: &TrustedRoot,
) -> Result<(Option<Time>, Vec<String>), Rejection> {
    let mut unverified = Vec::new();
    for response in bundle.timestamps().map_err(Rejection::Envelope)? {
        match timestamp::verify(&response, sig, sigstore, None) {
            Ok(stamp) => return Ok((Some(stamp.time), unverified)),
            Err(reason) => unverified.push(reason),
        }
    }
    Ok((None, unverified))
}

/// A signing certificate's identity, once its key made `sig` over `message` and `sigstore`
/// issued it, valid at `at` (E-11).
fn certified(
    der: &[u8],
    sigstore: &TrustedRoot,
    message: &[u8],
    sig: &[u8],
    at: Time,
) -> Result<CertIdentity, String> {
    let cert = Cert::parse(der).map_err(|e| format!("the certificate: {e}"))?;
    let key = cert
        .signing_key()
        .map_err(|e| format!("the certificate: {e}"))?;
    if !verify_signature(&key, message, sig) {
        return Err("the signature doesn't verify with its certificate's key".into());
    }
    sigstore
        .verify_signing_certificate(&cert, at)
        .map_err(|e| format!("the certificate at {at}: {e}"))?;
    cert.identity().map_err(|e| format!("the certificate: {e}"))
}

/// The identity of the certificate that signed `bytes` keylessly, an event or any other envelope
/// in a bundle, verified as E-8 step 2 verifies it, but against no signers: its timestamp, its
/// certificate's chain to `sigstore` at that time, and its signature. For display only.
pub fn certified_identity(bytes: &[u8], sigstore: &TrustedRoot) -> Result<CertIdentity, Rejection> {
    let object = Object::parse(bytes).map_err(|e| Rejection::Envelope(e.to_string()))?;
    let Object::Bundle(bundle) = &object else {
        return Err(Rejection::Signature(
            "a plain envelope, signed by a key".into(),
        ));
    };
    let envelope = bundle_envelope(bundle)?;
    let Material::Certificate(der) = bundle.material().map_err(Rejection::Envelope)? else {
        return Err(Rejection::Signature(
            "signed by a key, not keylessly".into(),
        ));
    };
    let signature = &envelope.signatures[0];
    let sig = dsse::signature_bytes(signature)
        .ok_or_else(|| Rejection::Envelope("the signature is not standard padded base64".into()))?;
    let payload = envelope
        .payload_bytes()
        .map_err(|e| Rejection::Envelope(e.to_string()))?;
    let message = pae(&envelope.payload_type, &payload);
    let (at, _) = trusted_time(bundle, &sig, sigstore)?;
    let at = at.ok_or_else(|| {
        Rejection::Signature("a keyless signature needs a trusted timestamp".into())
    })?;
    certified(&der, sigstore, &message, &sig, at).map_err(Rejection::Signature)
}

/// A certificate's identity, for messages.
pub fn describe(identity: &CertIdentity) -> String {
    let mut text = format!("issuer {}, subject {}", identity.issuer, identity.subject);
    for (name, value) in [
        ("repository_id", &identity.repository_id),
        ("repository", &identity.repository),
        ("ref", &identity.reference),
        ("workflow", &identity.workflow),
        ("signer_workflow", &identity.signer_workflow),
    ] {
        if let Some(value) = value {
            text.push_str(&format!(", {name} {value}"));
        }
    }
    text
}

fn check_constraints(event: &Event) -> Result<(), Rejection> {
    let fail = |why: String| Err(Rejection::Constraint(why));
    let present = event.kind_objects();
    if present != [event.kind] {
        let names: Vec<&str> = present.iter().map(|k| k.as_str()).collect();
        return fail(format!(
            "a {} event must carry exactly its `{}` object, found [{}]",
            event.kind,
            event.kind,
            names.join(", ")
        ));
    }
    if let Some(check) = &event.check {
        if check.attempts == 0 {
            return fail("a check has at least one attempt".into());
        }
        if check.finished_at < check.started_at {
            return fail("a check can't finish before it started".into());
        }
        if check.environment.is_some() != check.environment_digest.is_some() {
            return fail("`environment` and `environmentDigest` come together".into());
        }
    }
    let pinned =
        |reference: &str, digest: &hushrun_core::event::DigestSet| -> Result<(), Rejection> {
            match pinned_digest(reference) {
                Some(pin) if Some(pin) == digest.digest() => Ok(()),
                Some(_) => Err(Rejection::Constraint(format!(
                    "{reference} is pinned to another digest than sha256:{}",
                    digest.sha256
                ))),
                None => Err(Rejection::Constraint(format!(
                    "{reference} is not pinned by digest"
                ))),
            }
        };
    if let Some(artifact) = &event.artifact {
        pinned(&artifact.reference, &artifact.digest)?;
    }
    for artifact in event
        .deployed
        .iter()
        .flat_map(|d| &d.artifacts)
        .chain(event.passed.iter().flat_map(|p| &p.artifacts))
    {
        pinned(&artifact.reference, &artifact.digest)?;
    }
    if let Some(started) = &event.started
        && started.lease_until < event.time
    {
        return fail("a lease can't end before the event was signed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
