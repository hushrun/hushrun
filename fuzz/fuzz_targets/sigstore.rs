//! Arbitrary bytes through M10's parsers (E-1, E-10, E-11, T-12): bundles, certificates and
//! their SCTs and identities, RFC 3161 responses, TUF metadata and trust roots. None may panic:
//! the control plane's index runs them on whatever a ledger holds.
#![no_main]

use libfuzzer_sys::fuzz_target;
use hushrun_attest::trust_root::TrustedRoot;
use hushrun_attest::{Object, timestamp, tuf, x509};

fuzz_target!(|data: &[u8]| {
    let root = TrustedRoot::public_good();
    let _ = Object::parse(data);
    if let Ok(cert) = x509::Cert::parse(data) {
        let _ = cert.identity();
        let _ = cert.signing_key();
        let at = cert.not_before();
        let _ = root.verify_signing_certificate(&cert, at);
        for authority in &root.certificate_authorities {
            let _ = x509::verify_embedded_sct(&cert, &authority.chain[0], &root.ct_logs);
        }
    }
    let _ = timestamp::verify(data, b"signature", root, Some(&[1, 2, 3]));
    let _ = tuf::Root::parse(data);
    if let Ok(trust_root) = tuf::Root::parse(tuf::SIGSTORE_ROOT) {
        let _ = trust_root.verify_role("targets", data);
        let _ = trust_root.verify_next(data);
    }
    let _ = TrustedRoot::parse(data, "fuzz");
});
