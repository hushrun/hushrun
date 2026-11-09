//! Public-good Sigstore, for real (`cargo test -p hushrun-attest --features it`): its timestamp
//! authority, its TUF repository, and, in a GitHub Actions job with `id-token: write`, Fulcio.
//! These use the network, so they run only with the `it` feature, in their own CI job.
#![cfg(feature = "it")]

use hushrun_attest::remote::fulcio::{self, Fulcio, Keyless};
use hushrun_attest::remote::oidc::GithubActions;
use hushrun_attest::remote::tsa::{self, Authorities};
use hushrun_attest::remote::{Secret, tuf as remote_tuf};
use hushrun_attest::signer::{Signer as _, Timestamper as _};
use hushrun_attest::tuf::{SIGSTORE_MIRROR, SIGSTORE_ROOT, verify_cached};
use hushrun_attest::{TrustedRoot, timestamp};
use hushrun_core::time::Time;

fn now() -> Time {
    Time::from_timestamp(jiff::Timestamp::now())
}

// covers: E-10
#[test]
fn sigstores_timestamp_authority_answers_with_a_timestamp_that_verifies() {
    let root = TrustedRoot::public_good();
    let authorities = Authorities::new(&[tsa::PUBLIC_GOOD.to_string()], root.clone()).unwrap();
    let response = authorities
        .timestamp(b"a hushrun integration test")
        .unwrap();
    let stamp = timestamp::verify(&response, b"a hushrun integration test", root, None).unwrap();
    assert_eq!(stamp.authority, tsa::PUBLIC_GOOD);
    assert!(
        stamp.time.millis_since(now()).abs() < 10 * 60 * 1000,
        "{}",
        stamp.time
    );
}

// covers: T-12
#[test]
fn sigstores_tuf_repository_refreshes_the_vendored_trust_root() {
    let refreshed = remote_tuf::refresh(SIGSTORE_MIRROR, SIGSTORE_ROOT, &[], None, now()).unwrap();
    assert!(refreshed.root_version >= 15);
    assert!(refreshed.targets_version >= 14);
    verify_cached(
        SIGSTORE_ROOT,
        &refreshed.roots,
        &refreshed.targets,
        &refreshed.trusted_root,
    )
    .unwrap();
    let root = TrustedRoot::parse(&refreshed.trusted_root, "refreshed").unwrap();
    assert!(!root.certificate_authorities.is_empty());
}

// covers: E-11
#[test]
fn fulcio_certifies_this_github_actions_job() {
    let (Ok(url), Ok(token)) = (
        std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL"),
        std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
    ) else {
        eprintln!("skipped: not a GitHub Actions job with id-token: write");
        return;
    };
    let root = TrustedRoot::public_good().clone();
    let signer = Keyless::new(
        Fulcio::new(fulcio::PUBLIC_GOOD).unwrap(),
        Box::new(GithubActions::new(url, Secret::new(token))),
        root.clone(),
        Box::new(now),
    )
    .unwrap();
    let der = signer.certificate().unwrap().unwrap();
    let cert = hushrun_attest::x509::Cert::parse(&der).unwrap();
    let identity = cert.identity().unwrap();
    assert_eq!(
        identity.issuer,
        "https://token.actions.githubusercontent.com"
    );
    assert!(identity.repository_id.is_some(), "{identity:?}");
    root.verify_signing_certificate(&cert, cert.not_before())
        .unwrap();
}
