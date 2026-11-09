use super::*;
use crate::testing::Pki;
use hushrun_core::time::Dur;

fn at(text: &str) -> Time {
    Time::parse(text).unwrap()
}

fn root(pki: &Pki) -> TrustedRoot {
    TrustedRoot::parse(&pki.trusted_root(), "test").unwrap()
}

// covers: E-10
#[test]
fn requests_and_answers_round_trip() {
    let pki = Pki::new();
    let nonce = nonce();
    assert!(nonce[0] > 0 && nonce[0] < 0x80);
    let request = request(b"signature", &nonce);
    let parsed = TimeStampReq::from_der(&request).unwrap();
    assert_eq!(parsed.version, 1);
    assert!(parsed.cert_req);
    assert_eq!(
        parsed.message_imprint.hashed_message.as_bytes(),
        digest(&oid::SHA256, b"signature").unwrap()
    );
    let time = at("2026-10-15T12:34:56.789Z");
    let answer = pki.answer(&request, time).unwrap();
    let stamp = verify(&answer, b"signature", &root(&pki), Some(&nonce)).unwrap();
    assert_eq!(stamp.time, time);
    assert_eq!(stamp.authority, "https://tsa.test/api/v1/timestamp");
}

// covers: E-10
#[test]
fn timestamps_that_dont_count() {
    let pki = Pki::new();
    let root = root(&pki);
    let time = at("2026-10-15T12:00:00Z");
    let fails = |answer: &[u8], signature: &[u8], nonce: Option<&[u8]>, needle: &str| {
        let err = verify(answer, signature, &root, nonce).unwrap_err();
        assert!(err.contains(needle), "{needle:?}: {err}");
    };
    let good = pki.timestamp(b"signature", time, Some(&[1, 2, 3]));
    verify(&good, b"signature", &root, Some(&[1, 2, 3])).unwrap();
    fails(&good, b"another signature", None, "over another signature");
    fails(&good, b"signature", Some(&[9]), "nonce");
    fails(
        &pki.foreign_timestamp(b"signature", time),
        b"signature",
        None,
        "no timestamp authority",
    );
    fails(
        &Pki::refusal("busy"),
        b"signature",
        None,
        "refused (status 2: busy)",
    );
    fails(b"\x30\x00", b"signature", None, "not a TimeStampResp");
    // Before the authority's certificate was valid.
    fails(
        &pki.timestamp(b"signature", at("2010-01-01T00:00:00Z"), None),
        b"signature",
        None,
        "not at",
    );
    // A changed TSTInfo no longer matches its signed digest.
    let mut tampered = good.clone();
    let at = tampered
        .windows(4)
        .position(|w| w == b"2026")
        .expect("genTime");
    tampered[at + 3] = b'7';
    let err = verify(&tampered, b"signature", &root, None).unwrap_err();
    assert!(err.contains("message digest"), "{err}");
}

// covers: E-10
#[test]
fn trust_roots_bound_the_authority_in_time() {
    let mut pki = Pki::new();
    pki.valid = (at("2020-01-01T00:00:00Z"), at("2026-01-01T00:00:00Z"));
    let root = TrustedRoot::parse(&pki.trusted_root(), "test").unwrap();
    let answer = pki.timestamp(b"s", at("2026-06-01T00:00:00Z"), None);
    let err = verify(&answer, b"s", &root, None).unwrap_err();
    assert!(err.contains("trusted from 2020"), "{err}");
    let answer = pki.timestamp(b"s", at("2025-06-01T00:00:00Z"), None);
    verify(&answer, b"s", &root, None).unwrap();
    let _ = Dur::ZERO;
}

// covers: E-10
#[test]
fn gen_times_may_have_fractions() {
    let time = |text: &str| gen_time(&Any::new(Tag::GeneralizedTime, text.as_bytes()).unwrap());
    assert_eq!(time("20261015123456Z").unwrap(), at("2026-10-15T12:34:56Z"));
    assert_eq!(
        time("20261015123456.5Z").unwrap(),
        at("2026-10-15T12:34:56.500Z")
    );
    assert_eq!(
        time("20261015123456.123456Z").unwrap(),
        at("2026-10-15T12:34:56.123Z")
    );
    for bad in [
        "20261015123456",
        "2026101512345Z",
        "20261015123456.Z",
        "2026101512345xZ",
    ] {
        assert!(time(bad).is_err(), "{bad}");
    }
    assert!(gen_time(&Any::new(Tag::UtcTime, b"261015123456Z".as_slice()).unwrap()).is_err());
}
