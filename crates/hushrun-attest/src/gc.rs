//! \[M18\] GC statements (L-9, L-11): an administrator's signature on one deletion from an HTTP
//! ledger that keeps a transparency log. The statement travels with the delete, and the
//! tombstone's leaf records its SHA-256, so every deletion in the log is attributed to a signer,
//! and a deletion nobody signed stands out to monitors.
//!
//! \[ADR 0031\] The signer is a machine of an admin or an owner of the repository's org: one the
//! signers answer marks `gc` (T-15).
//!
//! A GC statement is a DSSE envelope (E-1) of [`GC_PAYLOAD_TYPE`], plain or in a Sigstore bundle,
//! around
//!
//! ```json
//! {"_type": "hushrun.gc/v1", "ledger": "https://hushrun.example/l/acme/back",
//!  "key": "v1/events/…", "time": "2026-10-15T00:00:00.000Z"}
//! ```

use serde::{Deserialize, Serialize};

use hushrun_core::signers::Signers;
use hushrun_core::time::Time;

use crate::dsse::Envelope;
use crate::signer::{SignError, Signer, Timestamper};
use crate::trust_root::TrustedRoot;
use crate::{SigningKey, sign_bundle};

/// The payload type of a GC statement.
pub const GC_PAYLOAD_TYPE: &str = "application/vnd.hushrun.gc+json";
/// The `_type` of its payload.
pub const GC_TYPE: &str = "hushrun.gc/v1";

/// A GC statement's payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcPayload {
    #[serde(rename = "_type")]
    pub payload_type: String,
    /// The ledger's normalized URL (L-10).
    pub ledger: String,
    /// The key deleted.
    pub key: String,
    /// When the administrator signed it.
    pub time: Time,
}

/// A GC statement that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGc {
    pub payload: GcPayload,
    /// The key id of the machine's signature.
    pub signers: Vec<String>,
    /// \[ADR 0031\] Whose machine signed it: `user:<email>`.
    pub signer: String,
}

fn payload(ledger: &str, key: &str, time: Time) -> Vec<u8> {
    serde_json::to_vec(&GcPayload {
        payload_type: GC_TYPE.to_string(),
        ledger: ledger.to_string(),
        key: key.to_string(),
        time,
    })
    .expect("payloads serialize")
}

/// Signs the deletion of `key` from `ledger`, as of `time`, with a key: a plain envelope.
pub fn sign_gc(ledger: &str, key: &str, time: Time, signer: &SigningKey) -> Vec<u8> {
    let mut envelope = Envelope::unsigned(GC_PAYLOAD_TYPE, &payload(ledger, key, time));
    envelope
        .add_signature(signer)
        .expect("an unsigned envelope takes a signature");
    envelope.to_bytes()
}

/// \[ADR 0026\] Signs the deletion of `key` from `ledger` with any signer into a bundle, as events
/// are signed (E-1, E-10): a keyless one needs the timestamp.
pub fn sign_gc_bundle(
    ledger: &str,
    key: &str,
    time: Time,
    signer: &dyn Signer,
    timestamps: &dyn Timestamper,
) -> Result<Vec<u8>, SignError> {
    sign_bundle(
        GC_PAYLOAD_TYPE,
        &payload(ledger, key, time),
        signer,
        timestamps,
    )
    .map(|signed| signed.bytes)
}

fn parse_payload(bytes: &[u8]) -> Result<GcPayload, String> {
    let payload: GcPayload =
        serde_json::from_slice(bytes).map_err(|e| format!("the GC statement's payload: {e}"))?;
    if payload.payload_type != GC_TYPE {
        return Err(format!("_type {:?}, not {GC_TYPE}", payload.payload_type));
    }
    Ok(payload)
}

/// \[ADR 0031\] Verifies a GC statement against the signers (L-9): its shape, and a signature that
/// counts as an event's would (E-8 step 2: a listed machine's, valid at its trusted time or now)
/// by a machine the signers mark `gc`, an admin's or owner's. `sigstore` verifies timestamps.
/// The caller checks the ledger and the key against the deletion.
pub fn verify_gc(
    bytes: &[u8],
    signers: &Signers,
    sigstore: &TrustedRoot,
    now: Time,
) -> Result<VerifiedGc, String> {
    let (payload, signer) = crate::verify::signed(bytes, GC_PAYLOAD_TYPE, signers, sigstore, now)
        .map_err(|r| match r {
        crate::verify::Rejection::Envelope(why) => format!("not a GC statement: {why}"),
        other => format!("its signature: {}", rejection_reason(&other)),
    })?;
    let payload = parse_payload(&payload)?;
    let may = signers.machine(&signer.keyid).is_some_and(|m| m.gc);
    if !may {
        return Err(format!(
            "signed by a machine of {}, who isn't an admin or an owner of the org",
            signer.signer
        ));
    }
    Ok(VerifiedGc {
        payload,
        signers: vec![signer.keyid],
        signer: signer.signer,
    })
}

fn rejection_reason(rejection: &crate::verify::Rejection) -> String {
    use crate::verify::Rejection::*;
    match rejection {
        Envelope(w) | Signature(w) | Statement(w) | Subject(w) | StorageKey(w) | Time(w)
        | Constraint(w) => w.clone(),
    }
}

#[cfg(test)]
mod gc_tests {
    use super::*;
    use crate::bundle::{Bundle, Material};
    use crate::signer::NoTimestamps;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_seed(&[n; 32])
    }

    fn signers() -> Signers {
        let mut doc = Signers::example(
            "github.com/a/b",
            &[
                (&key(1).public_key(), "user:admin@a.dev"),
                (&key(2).public_key(), "user:dev@a.dev"),
            ],
        );
        doc["machines"][0]["gc"] = serde_json::json!(true);
        Signers::from_json(&doc).unwrap()
    }

    fn now() -> Time {
        Time::parse("2026-10-15T00:00:00.000Z").unwrap()
    }

    fn root() -> &'static TrustedRoot {
        TrustedRoot::public_good()
    }

    // covers: L-9
    #[test]
    fn gc_statements_are_signed_by_admins_machines() {
        let trust = signers();
        let plain = sign_gc("https://h/l/a/b", "v1/events/x", now(), &key(1));
        let verified = verify_gc(&plain, &trust, root(), now()).unwrap();
        assert_eq!(verified.payload.key, "v1/events/x");
        assert_eq!(verified.signer, "user:admin@a.dev");
        assert_eq!(verified.signers, [key(1).keyid()]);
        // In a bundle too, as events are signed.
        let bundled = sign_gc_bundle(
            "https://h/l/a/b",
            "v1/events/x",
            now(),
            &key(1),
            &NoTimestamps,
        )
        .unwrap();
        assert!(matches!(
            crate::Object::parse(&bundled).unwrap(),
            crate::Object::Bundle(_)
        ));
        assert_eq!(
            verify_gc(&bundled, &trust, root(), now()).unwrap().signer,
            "user:admin@a.dev"
        );
        // A member who isn't an admin, or a stranger: refused.
        let by_member = sign_gc("https://h/l/a/b", "v1/events/x", now(), &key(2));
        let err = verify_gc(&by_member, &trust, root(), now()).unwrap_err();
        assert!(err.contains("dev@a.dev") && err.contains("admin"), "{err}");
        let by_stranger = sign_gc("https://h/l/a/b", "v1/events/x", now(), &key(3));
        assert!(verify_gc(&by_stranger, &trust, root(), now()).is_err());
        // An event's envelope isn't a GC statement.
        let event = Bundle::new(
            Envelope::unsigned(crate::PAYLOAD_TYPE, b"{}"),
            Material::Key(key(1).keyid()),
            &[],
        )
        .to_bytes();
        let err = verify_gc(&event, &trust, root(), now()).unwrap_err();
        assert!(err.contains("not a GC statement"), "{err}");
    }
}
