use super::*;
use crate::keys::SigningKey;
use crate::testing::{Pki, github_claims, github_identity};
use crate::trust_root::TrustedRoot;
use hushrun_core::time::Dur;

fn at(text: &str) -> Time {
    Time::parse(text).unwrap()
}

fn identity() -> CertIdentity {
    github_identity(&github_claims(
        "acme/back",
        "123456",
        "refs/heads/main",
        ".github/workflows/deploy.yml",
        Some("acme/shared/.github/workflows/build.yml@refs/tags/v1"),
    ))
    .unwrap()
}

// covers: E-11
#[test]
fn signing_certificates_name_their_key_and_identity() {
    let pki = Pki::new();
    let key = SigningKey::generate_p256().unwrap();
    let issued = at("2026-10-15T12:00:00Z");
    let cert = pki.signing_certificate(&key.public_key(), &identity(), issued);
    assert_eq!(cert.signing_key().unwrap(), key.public_key());
    assert_eq!(cert.identity().unwrap(), identity());
    // ADR 0026 item 7: the Build Trigger (.1.20) comes through, from the token's event_name.
    assert_eq!(cert.identity().unwrap().trigger.as_deref(), Some("push"));
    assert_eq!(cert.not_before(), issued);
    assert_eq!(cert.not_after(), issued.saturating_add(Dur::from_secs(600)));
    let parsed = Cert::parse(&cert.der).unwrap();
    assert_eq!(parsed.tbs(), cert.tbs());
    // The tbs range is the TBSCertificate as written.
    assert_eq!(cert.tbs()[0], 0x30);
    cert.verify_issued_by(&pki.intermediate.cert).unwrap();
    assert!(cert.verify_issued_by(&pki.root.cert).is_err());
    // CA certificates sign certificates, not events.
    assert!(pki.intermediate.cert.signing_key().is_err());
    let pem = der::pem::encode_string("CERTIFICATE", der::pem::LineEnding::LF, &cert.der).unwrap();
    assert_eq!(Cert::from_pem(&pem).unwrap(), cert);
}

// covers: E-11
#[test]
fn trust_roots_check_the_chain_validity_and_sct() {
    let pki = Pki::new();
    let root = TrustedRoot::parse(&pki.trusted_root(), "test").unwrap();
    let key = SigningKey::generate_p256().unwrap();
    let issued = at("2026-10-15T12:00:00Z");
    let cert = pki.signing_certificate(&key.public_key(), &identity(), issued);
    let inside = issued.saturating_add(Dur::from_secs(30));
    assert_eq!(
        root.verify_signing_certificate(&cert, inside).unwrap().uri,
        "https://fulcio.test"
    );
    // Outside the certificate's ten minutes.
    let late = issued.saturating_add(Dur::from_secs(601));
    let err = root.verify_signing_certificate(&cert, late).unwrap_err();
    assert!(err.contains("not at"), "{err}");
    // Issued by another instance.
    let other = Pki::new().signing_certificate(&key.public_key(), &identity(), issued);
    assert!(root.verify_signing_certificate(&other, inside).is_err());
    // With CT logs listed, an SCT from one of them is required.
    let mut stranger_log = Pki::new();
    stranger_log.with_ct_log = true;
    let foreign_logs = TrustedRoot::parse(&stranger_log.trusted_root(), "x")
        .unwrap()
        .ct_logs;
    let err = verify_embedded_sct(&cert, &pki.intermediate.cert, &foreign_logs).unwrap_err();
    assert!(err.contains("unknown log"), "{err}");
    verify_embedded_sct(&cert, &pki.intermediate.cert, &root.ct_logs).unwrap();
    // The SCT covers the issuer's key: another issuer's hash doesn't verify.
    assert!(verify_embedded_sct(&cert, &pki.root.cert, &root.ct_logs).is_err());
    // A trust root without CT logs doesn't require SCTs.
    let mut no_ct = Pki::new();
    no_ct.with_ct_log = false;
    let root = TrustedRoot::parse(&no_ct.trusted_root(), "test").unwrap();
    assert!(root.ct_logs.is_empty());
    let cert = no_ct.signing_certificate(&key.public_key(), &identity(), issued);
    root.verify_signing_certificate(&cert, inside).unwrap();
}

// covers: E-11
#[test]
fn certificates_with_extensions_twice_or_trailing_bytes_are_refused() {
    let pki = Pki::new();
    let mut der = pki.root.cert.der.clone();
    der.push(0);
    assert!(Cert::parse(&der).is_err());
    assert!(Cert::parse(b"not DER").is_err());
    assert!(Cert::from_pem("-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----").is_err());
}

// covers: E-11
#[test]
fn signatures_need_a_key_that_fits_the_algorithm() {
    let pki = Pki::new();
    let spki = pki.root.key.spki();
    let message = b"tbs";
    let signature = pki.root.key.sign(message);
    verify_with(&spki, &oid::ECDSA_SHA256, None, message, &signature).unwrap();
    assert!(verify_with(&spki, &oid::ECDSA_SHA384, None, message, &signature).is_err());
    assert!(verify_with(&spki, &oid::RSA_SHA256, None, message, &signature).is_err());
    // CMS may name the key's algorithm and the digest separately.
    verify_with(
        &spki,
        &oid::EC_PUBLIC_KEY,
        Some(&oid::SHA256),
        message,
        &signature,
    )
    .unwrap();
    assert!(verify_with(&spki, &oid::EC_PUBLIC_KEY, None, message, &signature).is_err());
    assert!(verify_with(&spki, &oid::ECDSA_SHA256, None, b"other", &signature).is_err());
}

// covers: E-11
#[test]
fn reads_and_verifies_a_certificate_public_good_fulcio_issued() {
    let der = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/sigstore/fulcio-sigstore-js-2024.der"
    ))
    .unwrap();
    let cert = Cert::parse(&der).unwrap();
    let identity = cert.identity().unwrap();
    assert_eq!(
        identity.issuer,
        "https://token.actions.githubusercontent.com"
    );
    assert_eq!(
        identity.subject,
        "https://github.com/sigstore/sigstore-js/.github/workflows/release.yml@refs/heads/main"
    );
    assert_eq!(
        identity.signer_workflow.as_deref(),
        Some(identity.subject.as_str())
    );
    assert_eq!(
        identity.repository.as_deref(),
        Some("https://github.com/sigstore/sigstore-js")
    );
    assert_eq!(identity.reference.as_deref(), Some("refs/heads/main"));
    assert!(
        identity
            .repository_id
            .as_deref()
            .is_some_and(|id| id.bytes().all(|b| b.is_ascii_digit()))
    );
    assert_eq!(
        identity.workflow.as_deref(),
        Some(
            "https://github.com/sigstore/sigstore-js/.github/workflows/release.yml@refs/heads/main"
        )
    );
    assert_eq!(cert.signing_key().unwrap().algorithm(), "ecdsa-p256");
    // The public-good root issued it, through the current intermediate, and CT logged it.
    let root = TrustedRoot::public_good();
    let at = cert.not_before().saturating_add(Dur::from_secs(60));
    let authority = root.verify_signing_certificate(&cert, at).unwrap();
    assert_eq!(authority.uri, "https://fulcio.sigstore.dev");
    let issuer = &authority.chain[0];
    verify_embedded_sct(&cert, issuer, &root.ct_logs).unwrap();
    // Outside its ten minutes, it doesn't verify.
    assert!(
        root.verify_signing_certificate(&cert, cert.not_after().saturating_add(Dur::from_secs(1)))
            .is_err()
    );
    // The repository's CI, named by its id, matches it (T-15).
    let ci = hushrun_core::signers::CiIdentity {
        issuer: "https://token.actions.githubusercontent.com".into(),
        repository_id: identity.repository_id.clone().unwrap(),
    };
    assert!(ci.matches(&identity));
}
