use base64::Engine as _;

use super::*;
use crate::dsse::{self, Signature};
use crate::keys::SigningKey;
use hushrun_core::event::*;
use hushrun_core::policy::Policy;
use hushrun_core::run::{Provider, Trigger};
use hushrun_core::signers::CiIdentity;

const POLICY: &str = "version: 1\nrepository: github.com/acme/app\nchecks:\n  unit: {kind: source}\nstages:\n  staging: {}\n";

fn key(n: u8) -> SigningKey {
    SigningKey::from_seed(&[n; 32])
}

/// Key 1 is a member's machine, key 2 an agent's.
fn trust() -> Signers {
    machines(&[
        (&key(1).public_key(), "user:ana@acme.dev"),
        (&key(2).public_key(), "agent:fleet-a"),
    ])
}

fn machines(keys: &[(&PublicKey, &str)]) -> Signers {
    Signers::from_json(&Signers::example("github.com/acme/app", keys)).unwrap()
}

fn now() -> Time {
    Time::parse("2026-10-13T15:10:00.000Z").unwrap()
}

fn digest() -> Digest {
    Digest::of(b"manifest")
}

fn scope() -> ScopeName {
    ScopeName::new("default").unwrap()
}

fn staging() -> StageName {
    StageName::new("staging").unwrap()
}

fn base(kind: EventKind) -> Event {
    let policy = Policy::parse("hushrun.yaml", POLICY.as_bytes()).unwrap();
    Event {
        kind,
        repository: "github.com/acme/app".into(),
        scope: scope(),
        stage: Some(staging()),
        canonicalization: CANONICALIZATION.into(),
        source: SourceInfo {
            vcs: "git".into(),
            revision: Some("1248b05".into()),
            ..SourceInfo::default()
        },
        run: RunInfo {
            provider: Provider::Generic,
            id: "42".into(),
            attempt: 1,
            url: None,
            trigger: Trigger::Push,
            job: None,
            job_id: None,
        },
        producer: Producer {
            tool: "hushrun".into(),
            version: "0.1.0".into(),
            custody: Custody::Ci,
            invoked_by: None,
        },
        time: Time::parse("2026-10-13T15:04:05.123Z").unwrap(),
        policy_digest: policy.digest,
        forced: None,
        checkpoint: None,
        started: None,
        check: None,
        artifact: None,
        deployed: None,
        passed: None,
        failed: None,
        superseded: None,
        approval: None,
    }
}

fn check_event() -> Event {
    let mut event = base(EventKind::Check);
    event.check = Some(CheckRecord {
        name: hushrun_core::CheckName::new("unit").unwrap(),
        check_kind: EvidenceKind::Source,
        definition_digest: Digest::of(b"def"),
        environment: None,
        environment_digest: None,
        result: Outcome::Pass,
        attempts: 1,
        duration_ms: 95410,
        started_at: Time::parse("2026-10-13T15:02:30.000Z").unwrap(),
        finished_at: Time::parse("2026-10-13T15:04:05.000Z").unwrap(),
        exit_code: Some(0),
        command: None,
        reports: None,
    });
    event
}

fn at_events(event: &Event) -> Expected<'static> {
    let stage = event.stage.clone().map(|s| &*Box::leak(Box::new(s)));
    Expected {
        repository: "github.com/acme/app",
        scope: Box::leak(Box::new(scope())),
        location: Location::Events {
            digest: Box::leak(Box::new(digest())),
            kind: event.kind,
            stage,
        },
    }
}

fn signed(event: &Event, n: u8) -> Vec<u8> {
    crate::sign_event(event, &digest(), &key(n))
}

fn rejected(bytes: &[u8], expected: &Expected<'_>) -> Rejection {
    verify(bytes, &trust(), expected, now()).unwrap_err()
}

// covers: E-1, E-3, E-4, E-8
#[test]
fn accepts_a_signed_event() {
    let event = check_event();
    let verified = verify(&signed(&event, 1), &trust(), &at_events(&event), now()).unwrap();
    assert_eq!(verified.event, event);
    assert_eq!(verified.subject_digest, digest());
    assert_eq!(verified.signer, "user:ana@acme.dev");
    // A machine's events are never CI's (E-8 step 8, T-15).
    assert_eq!(verified.effective_custody, Custody::Isolated);
    // The statement is exactly E-3's shape.
    let envelope = Envelope::parse(&signed(&event, 1)).unwrap();
    let statement: serde_json::Value =
        serde_json::from_slice(&envelope.payload_bytes().unwrap()).unwrap();
    assert_eq!(statement["_type"], "https://in-toto.io/Statement/v1");
    assert_eq!(
        statement["predicateType"],
        "https://github.com/hushrun/hushrun/spec/event/v1"
    );
    assert_eq!(
        statement["subject"][0]["name"],
        "github.com/acme/app#default"
    );
    assert_eq!(statement["subject"][0]["digest"]["sha256"], digest().hex());
    assert_eq!(statement["predicate"]["check"]["checkKind"], "source");
    assert_eq!(statement["predicate"]["time"], "2026-10-13T15:04:05.123Z");
}

// covers: E-8
#[test]
fn step1_envelopes() {
    let event = check_event();
    let expected = at_events(&event);
    assert_eq!(rejected(b"not json", &expected).step(), 1);
    assert_eq!(rejected(&vec![b' '; (1 << 20) + 1], &expected).step(), 1);
    let mut envelope = Envelope::parse(&signed(&event, 1)).unwrap();
    envelope.payload_type = "application/json".into();
    assert_eq!(rejected(&envelope.to_bytes(), &expected).step(), 1);
}

// covers: E-2, E-8
#[test]
fn step2_signatures() {
    let event = check_event();
    let expected = at_events(&event);
    // An unknown key.
    assert_eq!(rejected(&signed(&event, 9), &expected).step(), 2);
    // A trusted key id over a payload it didn't sign.
    let mut envelope = Envelope::parse(&signed(&event, 1)).unwrap();
    let mut other = event.clone();
    other.run.id = "43".into();
    envelope.payload = Envelope::parse(&signed(&other, 9)).unwrap().payload;
    assert_eq!(rejected(&envelope.to_bytes(), &expected).step(), 2);
    // A second, valid signature is enough.
    let mut envelope = Envelope::parse(&signed(&event, 1)).unwrap();
    envelope.signatures.insert(
        0,
        Signature {
            keyid: key(1).keyid(),
            sig: base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
        },
    );
    verify(&envelope.to_bytes(), &trust(), &expected, now()).unwrap();
}

fn sign_raw(statement: &serde_json::Value) -> Vec<u8> {
    dsse::sign(&serde_json::to_vec(statement).unwrap(), &key(1)).to_bytes()
}

// covers: E-3, E-8
#[test]
fn step3_statements_and_versions() {
    let event = check_event();
    let expected = at_events(&event);
    let mut statement = serde_json::to_value(event.statement(&digest())).unwrap();
    statement["predicateType"] = "https://github.com/hushrun/hushrun/spec/event/v2".into();
    let rejection = rejected(&sign_raw(&statement), &expected);
    assert_eq!(rejection.step(), 3);
    assert!(
        rejection.to_string().contains("unknown major version"),
        "{rejection}"
    );
    let mut statement = serde_json::to_value(event.statement(&digest())).unwrap();
    statement["_type"] = "https://in-toto.io/Statement/v0.1".into();
    assert_eq!(rejected(&sign_raw(&statement), &expected).step(), 3);
    // Unknown fields are ignored.
    let mut statement = serde_json::to_value(event.statement(&digest())).unwrap();
    statement["predicate"]["futureField"] = serde_json::json!({"x": 1});
    statement["predicate"]["check"]["alsoNew"] = true.into();
    verify(&sign_raw(&statement), &trust(), &expected, now()).unwrap();
}

// covers: E-8
#[test]
fn step4_subjects() {
    let event = check_event();
    let expected = at_events(&event);
    // Replayed under another digest.
    let other = crate::sign_event(&event, &Digest::of(b"other"), &key(1));
    assert_eq!(rejected(&other, &expected).step(), 4);
    // Another repository or scope.
    let mut moved = event.clone();
    moved.repository = "github.com/acme/other".into();
    assert_eq!(rejected(&signed(&moved, 1), &expected).step(), 4);
    let mut moved = event.clone();
    moved.scope = ScopeName::new("api").unwrap();
    assert_eq!(rejected(&signed(&moved, 1), &expected).step(), 4);
    // Another canonicalization.
    let mut v2 = event.clone();
    v2.canonicalization = "hushrun-source-v2".into();
    assert!(
        rejected(&signed(&v2, 1), &expected)
            .to_string()
            .contains("canonicalization")
    );
    // Two subjects.
    let mut statement = serde_json::to_value(event.statement(&digest())).unwrap();
    let subject = statement["subject"][0].clone();
    statement["subject"].as_array_mut().unwrap().push(subject);
    assert_eq!(rejected(&sign_raw(&statement), &expected).step(), 4);
}

// covers: E-8, L-2
#[test]
fn step5_storage_keys() {
    let event = check_event();
    let bytes = signed(&event, 1);
    let scope = scope();
    let digest = digest();
    let production = StageName::new("production").unwrap();
    for location in [
        Location::Events {
            digest: &digest,
            kind: EventKind::Passed,
            stage: Some(&event.stage.clone().unwrap()),
        },
        Location::Events {
            digest: &digest,
            kind: EventKind::Check,
            stage: Some(&production),
        },
        Location::Events {
            digest: &digest,
            kind: EventKind::Check,
            stage: None,
        },
        Location::Deployed {
            stage: Some(&production),
        },
    ] {
        let expected = Expected {
            repository: "github.com/acme/app",
            scope: &scope,
            location,
        };
        assert_eq!(
            verify(&bytes, &trust(), &expected, now())
                .unwrap_err()
                .step(),
            5,
            "{location:?}"
        );
    }
    // The deployed index: kind deployed and its stage.
    let mut deployed = base(EventKind::Deployed);
    let image = format!("ghcr.io/acme/app@sha256:{}", "a".repeat(64));
    deployed.deployed = Some(Deployed {
        artifacts: vec![ArtifactRef {
            name: hushrun_core::ArtifactName::new("image").unwrap(),
            reference: image,
            digest: DigestSet {
                sha256: "a".repeat(64),
            },
        }],
        target: None,
    });
    let staging = staging();
    let expected = Expected {
        repository: "github.com/acme/app",
        scope: &scope,
        location: Location::Deployed {
            stage: Some(&staging),
        },
    };
    let verified = verify(&signed(&deployed, 1), &trust(), &expected, now()).unwrap();
    assert_eq!(verified.subject_digest, digest);
}

// covers: E-8
#[test]
fn step6_timestamps() {
    let mut event = check_event();
    event.time = now().saturating_add(Dur::from_secs(601));
    event.check.as_mut().unwrap().finished_at = event.time;
    assert_eq!(rejected(&signed(&event, 1), &at_events(&event)).step(), 6);
    let mut event = check_event();
    event.time = now().saturating_add(Dur::from_secs(599));
    verify(&signed(&event, 1), &trust(), &at_events(&event), now()).unwrap();
    let mut event = check_event();
    event.check.as_mut().unwrap().finished_at = event.time.saturating_add(Dur::from_secs(61));
    assert_eq!(rejected(&signed(&event, 1), &at_events(&event)).step(), 6);
}

// covers: E-5, E-8
#[test]
fn step7_kind_constraints() {
    let image = |hex: &str| format!("ghcr.io/acme/app@sha256:{hex}");
    let mut event = base(EventKind::Artifact);
    event.artifact = Some(ArtifactRecord {
        name: hushrun_core::ArtifactName::new("image").unwrap(),
        artifact_type: ArtifactType::Oci,
        reference: image(&"b".repeat(64)),
        digest: DigestSet {
            sha256: "a".repeat(64),
        },
        origin: Origin::Built,
        copied_from: None,
    });
    assert_eq!(rejected(&signed(&event, 1), &at_events(&event)).step(), 7);
    event.artifact.as_mut().unwrap().reference = "ghcr.io/acme/app:latest".into();
    assert_eq!(rejected(&signed(&event, 1), &at_events(&event)).step(), 7);
    event.artifact.as_mut().unwrap().reference = image(&"a".repeat(64));
    verify(&signed(&event, 1), &trust(), &at_events(&event), now()).unwrap();
    // A second kind object.
    event.failed = Some(Failed {
        reason: "x".into(),
        checks: None,
    });
    assert_eq!(rejected(&signed(&event, 1), &at_events(&event)).step(), 7);
    // No event needs a stage: lifecycle events and checks verify without one.
    let mut started = base(EventKind::Started);
    started.started = Some(Started {
        lease_until: started.time.saturating_add(Dur::from_secs(10)),
        listings: None,
    });
    started.stage = None;
    verify(&signed(&started, 1), &trust(), &at_events(&started), now()).unwrap();
    let mut check = check_event();
    check.stage = None;
    verify(&signed(&check, 1), &trust(), &at_events(&check), now()).unwrap();
}

fn deployed_event(stage: Option<StageName>) -> Event {
    let mut deployed = base(EventKind::Deployed);
    deployed.stage = stage;
    deployed.deployed = Some(Deployed {
        artifacts: vec![ArtifactRef {
            name: hushrun_core::ArtifactName::new("image").unwrap(),
            reference: format!("ghcr.io/acme/app@sha256:{}", "a".repeat(64)),
            digest: DigestSet {
                sha256: "a".repeat(64),
            },
        }],
        target: None,
    });
    deployed
}

// covers: E-8, L-2
#[test]
fn a_deployment_without_a_stage_verifies_under_underscore_keys() {
    let scope = scope();
    let staging = staging();
    let at = |stage: Option<&'static StageName>| Expected {
        repository: "github.com/acme/app",
        scope: Box::leak(Box::new(scope.clone())),
        location: Location::Deployed { stage },
    };
    let staging: &'static StageName = Box::leak(Box::new(staging));

    // `mark deployed` without a stage: in v1/events under `_`, and in the deployed index's `_`.
    let stageless = deployed_event(None);
    let bytes = signed(&stageless, 1);
    let verified = verify(&bytes, &trust(), &at_events(&stageless), now()).unwrap();
    assert_eq!(verified.event.stage, None);
    assert_eq!(verified.event.kind, EventKind::Deployed);
    let verified = verify(&bytes, &trust(), &at(None), now()).unwrap();
    assert_eq!(verified.subject_digest, digest());
    // The key's segment still matches the event's stage, both ways.
    assert_eq!(rejected(&bytes, &at(Some(staging))).step(), 5);
    let staged = signed(&deployed_event(Some(staging.clone())), 1);
    assert_eq!(rejected(&staged, &at(None)).step(), 5);
    verify(&staged, &trust(), &at(Some(staging)), now()).unwrap();

    // Every other lifecycle event of a run without a stage verifies too.
    let mut passed = base(EventKind::Passed);
    passed.stage = None;
    passed.passed = Some(Passed { artifacts: vec![] });
    verify(&signed(&passed, 1), &trust(), &at_events(&passed), now()).unwrap();
    let mut failed = base(EventKind::Failed);
    failed.stage = None;
    failed.failed = Some(Failed {
        reason: "deploy failed".into(),
        checks: None,
    });
    verify(&signed(&failed, 1), &trust(), &at_events(&failed), now()).unwrap();
}

// covers: E-8, T-15
#[test]
fn step8_effective_custody_is_capped() {
    let mut event = check_event();
    event.producer.custody = Custody::Ci;
    let verified = verify(&signed(&event, 2), &trust(), &at_events(&event), now()).unwrap();
    assert_eq!(verified.signer, "agent:fleet-a");
    assert_eq!(verified.effective_custody, Custody::Isolated);
    event.producer.custody = Custody::Local;
    let verified = verify(&signed(&event, 1), &trust(), &at_events(&event), now()).unwrap();
    assert_eq!(verified.effective_custody, Custody::Local);
}

// --- M10: bundles, timestamps, the repository's CI, revoked machines ----------------------------

use crate::signer::{NoTimestamps, SignError, Timestamper};
use crate::testing::{Pki, github_claims, github_identity, p256_key};
use crate::{SignedEvent, TrustedRoot, sign_event_bundle};

const GITHUB: &str = "https://token.actions.githubusercontent.com";

/// The test authority's timestamps, at a fixed time.
struct Stamps<'a>(&'a Pki, Time);

impl Timestamper for Stamps<'_> {
    fn timestamp(&self, signature: &[u8]) -> Result<Vec<u8>, SignError> {
        Ok(self.0.timestamp(signature, self.1, None))
    }
}

/// A keyless signer: an ephemeral key and the test CA's certificate for it.
struct Keyless {
    key: SigningKey,
    certificate: Vec<u8>,
}

impl crate::signer::Signer for Keyless {
    fn public_key(&self) -> Option<hushrun_core::signers::PublicKey> {
        None
    }
    fn certificate(&self) -> Result<Option<Vec<u8>>, SignError> {
        Ok(Some(self.certificate.clone()))
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SignError> {
        Ok(self.key.sign(message))
    }
    fn describe(&self) -> String {
        "keyless".into()
    }
}

/// When the test signatures were timestamped: two seconds after `time`.
fn stamped() -> Time {
    check_event().time.saturating_add(Dur::from_secs(2))
}

fn keyless(pki: &Pki, repository_id: &str, reference: &str) -> Keyless {
    let key = SigningKey::generate_p256().unwrap();
    let identity = github_identity(&github_claims(
        "acme/app",
        repository_id,
        reference,
        ".github/workflows/deploy.yml",
        None,
    ))
    .unwrap();
    let not_before = check_event().time.checked_sub(Dur::from_secs(1)).unwrap();
    let certificate = pki
        .signing_certificate(&key.public_key(), &identity, not_before)
        .der;
    Keyless { key, certificate }
}

/// The repository's CI: GitHub Actions of repository id 123 (T-15).
fn identity_trust() -> Signers {
    let mut signers = trust();
    signers.ci.push(CiIdentity {
        issuer: GITHUB.into(),
        repository_id: "123".into(),
    });
    signers
}

fn bundle(event: &Event, signer: &dyn crate::signer::Signer, stamps: &dyn Timestamper) -> Vec<u8> {
    sign_event_bundle(event, &digest(), signer, stamps)
        .unwrap()
        .bytes
}

fn root_of(pki: &Pki) -> TrustedRoot {
    TrustedRoot::parse(&pki.trusted_root(), "the test trust root").unwrap()
}

// covers: E-1, E-2, E-10
#[test]
fn bundles_signed_by_keys_verify_with_or_without_a_timestamp() {
    let pki = Pki::new();
    let root = root_of(&pki);
    let event = check_event();
    let expected = at_events(&event);
    // No timestamp authority: the bundle says why it has no timestamp.
    let SignedEvent {
        bytes,
        untimestamped,
    } = sign_event_bundle(&event, &digest(), &key(1), &NoTimestamps).unwrap();
    assert!(untimestamped.unwrap().contains("HUSHRUN_TSA=none"));
    let verified = verify_with(&bytes, &trust(), &root, &expected, now()).unwrap();
    assert_eq!(verified.signed_by, SignedBy::Machine("ana@acme.dev".into()));
    assert_eq!(verified.trusted_time, None);
    assert_eq!(verified.keyid, key(1).keyid());
    // With one, the trusted time is the authority's.
    let bytes = bundle(&event, &key(1), &Stamps(&pki, stamped()));
    let verified = verify_with(&bytes, &trust(), &root, &expected, now()).unwrap();
    assert_eq!(verified.trusted_time, Some(stamped()));
    assert_eq!(verified.event, event);
    // A P-256 key, which OpenSSL wrote.
    let p256 = p256_key(1);
    let p256_trust = machines(&[(&p256.public_key(), "user:bo@acme.dev")]);
    let bytes = bundle(&event, &p256, &Stamps(&pki, stamped()));
    let verified = verify_with(&bytes, &p256_trust, &root, &expected, now()).unwrap();
    assert_eq!(verified.signer, "user:bo@acme.dev");
    // An envelope signed by the same key verifies too, without a timestamp.
    let plain = dsse::sign(
        &serde_json::to_vec(&event.statement(&digest())).unwrap(),
        &p256,
    );
    verify_with(&plain.to_bytes(), &p256_trust, &root, &expected, now()).unwrap();
}

// covers: E-1, E-8
#[test]
fn bundle_shapes() {
    let pki = Pki::new();
    let root = root_of(&pki);
    let event = check_event();
    let expected = at_events(&event);
    let good = bundle(&event, &key(1), &NoTimestamps);
    let edit = |change: &dyn Fn(&mut serde_json::Value)| {
        let mut value: serde_json::Value = serde_json::from_slice(&good).unwrap();
        change(&mut value);
        let rejection = verify_with(
            &serde_json::to_vec(&value).unwrap(),
            &trust(),
            &root,
            &expected,
            now(),
        )
        .unwrap_err();
        assert_eq!(rejection.step(), 1, "{rejection}");
        rejection.to_string()
    };
    assert!(
        edit(&|v| v["mediaType"] = "application/vnd.dev.sigstore.bundle+json;version=0.1".into())
            .contains("0.3")
    );
    assert!(
        edit(&|v| {
            let signature = v["dsseEnvelope"]["signatures"][0].clone();
            v["dsseEnvelope"]["signatures"]
                .as_array_mut()
                .unwrap()
                .push(signature);
        })
        .contains("exactly one")
    );
    assert!(
        edit(&|v| v["verificationMaterial"]["publicKey"]["hint"] = "sha256:00".into())
            .contains("key hint")
    );
    assert!(
        edit(&|v| {
            v.as_object_mut().unwrap().remove("dsseEnvelope");
            v["messageSignature"] = serde_json::json!({});
        })
        .contains("no DSSE envelope")
    );
    // The older media type spelling of 0.3 is accepted.
    let mut value: serde_json::Value = serde_json::from_slice(&good).unwrap();
    value["mediaType"] = "application/vnd.dev.sigstore.bundle+json;version=0.3".into();
    verify_with(
        &serde_json::to_vec(&value).unwrap(),
        &trust(),
        &root,
        &expected,
        now(),
    )
    .unwrap();
}

// covers: E-10, E-11, T-15
#[test]
fn keyless_signatures_verify_by_identity() {
    let pki = Pki::new();
    let root = root_of(&pki);
    let event = check_event();
    let expected = at_events(&event);
    let signer = keyless(&pki, "123", "refs/heads/main");
    let bytes = bundle(&event, &signer, &Stamps(&pki, stamped()));
    let verified = verify_with(&bytes, &identity_trust(), &root, &expected, now()).unwrap();
    assert_eq!(verified.signer, "ci");
    assert_eq!(verified.effective_custody, Custody::Ci);
    assert_eq!(verified.trusted_time, Some(stamped()));
    assert_eq!(verified.keyid, Digest::of(&signer.certificate).to_string());
    let SignedBy::Identity(identity) = &verified.signed_by else {
        panic!("{:?}", verified.signed_by)
    };
    assert_eq!(identity.repository_id.as_deref(), Some("123"));
    assert_eq!(
        identity.workflow.as_deref(),
        Some("https://github.com/acme/app/.github/workflows/deploy.yml@refs/heads/main")
    );
    // The public-good trust root, which `verify` uses, didn't issue the test certificate.
    let rejection = verify(&bytes, &identity_trust(), &expected, now()).unwrap_err();
    assert_eq!(rejection.step(), 2, "{rejection}");
}

// covers: E-10, E-11, T-15
#[test]
fn keyless_rejections() {
    let pki = Pki::new();
    let root = root_of(&pki);
    let event = check_event();
    let expected = at_events(&event);
    let refused = |bytes: &[u8], trust: &Signers, needle: &str| {
        let rejection = verify_with(bytes, trust, &root, &expected, now()).unwrap_err();
        assert_eq!(rejection.step(), 2, "{rejection}");
        assert!(
            rejection.to_string().contains(needle),
            "{needle:?}: {rejection}"
        );
    };
    let signer = keyless(&pki, "123", "refs/heads/main");
    // A keyless signature needs a trusted timestamp.
    let err = sign_event_bundle(&event, &digest(), &signer, &NoTimestamps).unwrap_err();
    assert!(matches!(err, SignError::Config(_)), "{err}");
    let mut value: serde_json::Value =
        serde_json::from_slice(&bundle(&event, &signer, &Stamps(&pki, stamped()))).unwrap();
    value["verificationMaterial"]
        .as_object_mut()
        .unwrap()
        .remove("timestampVerificationData");
    refused(
        &serde_json::to_vec(&value).unwrap(),
        &identity_trust(),
        "needs a trusted timestamp",
    );
    // A timestamp from an authority the trust root doesn't list doesn't count.
    struct Foreign<'a>(&'a Pki);
    impl Timestamper for Foreign<'_> {
        fn timestamp(&self, signature: &[u8]) -> Result<Vec<u8>, SignError> {
            Ok(self.0.foreign_timestamp(signature, stamped()))
        }
    }
    refused(
        &bundle(&event, &signer, &Foreign(&pki)),
        &identity_trust(),
        "timestamps that don't verify",
    );
    // Another repository's workflow isn't this repository's CI.
    refused(
        &bundle(
            &event,
            &keyless(&pki, "456", "refs/heads/main"),
            &Stamps(&pki, stamped()),
        ),
        &identity_trust(),
        "isn't the repository's CI",
    );
    // Any branch of this repository is (ADR 0031): members may push.
    verify_with(
        &bundle(
            &event,
            &keyless(&pki, "123", "refs/heads/develop"),
            &Stamps(&pki, stamped()),
        ),
        &identity_trust(),
        &root,
        &expected,
        now(),
    )
    .unwrap();
    // Before the server knows the repository's id, no certificate is its CI.
    refused(
        &bundle(&event, &signer, &Stamps(&pki, stamped())),
        &trust(),
        "names none yet",
    );
    // A certificate another CA issued.
    let stranger = Pki::new();
    refused(
        &bundle(
            &event,
            &keyless(&stranger, "123", "refs/heads/main"),
            &Stamps(&pki, stamped()),
        ),
        &identity_trust(),
        "no certificate authority",
    );
    // Timestamped after the certificate expired: it lasts 10 minutes.
    let mut late = event.clone();
    late.time = stamped().saturating_add(Dur::from_secs(660));
    let late_expected = at_events(&late);
    let bytes = bundle(
        &late,
        &signer,
        &Stamps(&pki, stamped().saturating_add(Dur::from_secs(661))),
    );
    let rejection =
        verify_with(&bytes, &identity_trust(), &root, &late_expected, late.time).unwrap_err();
    assert!(rejection.to_string().contains("not at"), "{rejection}");
}

// covers: E-10, E-8
#[test]
fn time_must_agree_with_the_trusted_time() {
    let pki = Pki::new();
    let root = root_of(&pki);
    let event = check_event();
    let expected = at_events(&event);
    for (offset_secs, earlier, accepted) in [
        (540, false, true),
        (540, true, true),
        (601, false, false),
        (601, true, false),
    ] {
        let skew = Dur::from_secs(offset_secs);
        let at = if earlier {
            event.time.checked_sub(skew).unwrap()
        } else {
            event.time.saturating_add(skew)
        };
        let bytes = bundle(&event, &key(1), &Stamps(&pki, at));
        match verify_with(&bytes, &trust(), &root, &expected, now()) {
            Ok(_) => assert!(accepted, "{offset_secs} s"),
            Err(rejection) => {
                assert!(!accepted, "{offset_secs} s: {rejection}");
                assert_eq!(rejection.step(), 6, "{rejection}");
                assert!(rejection.to_string().contains("trusted timestamp"));
            }
        }
    }
}

// covers: T-15, E-10
#[test]
fn a_revoked_machine_counts_for_what_it_signed_before() {
    let pki = Pki::new();
    let root = root_of(&pki);
    let event = check_event();
    let expected = at_events(&event);
    let mut revoked = trust();
    revoked.machines.get_mut(&key(1).keyid()).unwrap().revoked =
        Some(Time::parse("2026-10-13T15:06:00Z").unwrap());
    let plain = bundle(&event, &key(1), &NoTimestamps);
    let stamped_bytes = bundle(&event, &key(1), &Stamps(&pki, stamped()));
    let before = Time::parse("2026-10-13T15:05:00Z").unwrap();
    let after = Time::parse("2026-10-20T00:00:00Z").unwrap();
    // While the machine counts, an event counts with or without a timestamp.
    verify_with(&plain, &revoked, &root, &expected, before).unwrap();
    verify_with(&stamped_bytes, &revoked, &root, &expected, before).unwrap();
    // Once it doesn't, only a timestamp from before its revocation counts.
    let rejection = verify_with(&plain, &revoked, &root, &expected, after).unwrap_err();
    assert_eq!(rejection.step(), 2);
    assert!(
        rejection
            .to_string()
            .contains("without a trusted timestamp"),
        "{rejection}"
    );
    let old_plain_envelope = signed(&event, 1);
    assert_eq!(
        verify_with(&old_plain_envelope, &revoked, &root, &expected, after)
            .unwrap_err()
            .step(),
        2
    );
    verify_with(&stamped_bytes, &revoked, &root, &expected, after).unwrap();
    // A stolen key can't sign now and claim the past: the timestamp is now.
    let mut forged = event.clone();
    forged.time = after;
    let forged_expected = at_events(&forged);
    let bytes = bundle(&forged, &key(1), &Stamps(&pki, after));
    let rejection = verify_with(&bytes, &revoked, &root, &forged_expected, after).unwrap_err();
    assert!(
        rejection.to_string().contains("timestamped at"),
        "{rejection}"
    );
    // A key the server doesn't list never counts.
    let stranger = bundle(&event, &key(9), &Stamps(&pki, stamped()));
    let rejection = verify_with(&stranger, &revoked, &root, &expected, before).unwrap_err();
    assert!(
        rejection.to_string().contains("machine of a member"),
        "{rejection}"
    );
}

// covers: E-1, E-2
#[test]
fn a_registered_machines_signature_is_found_among_its_keys() {
    // ADR 0028: the server checks a person's write against their machines' keys, with no trust.
    let pki = Pki::new();
    let event = check_event();
    let machine = p256_key(3);
    let other = key(2);
    let registered = [other.public_key(), machine.public_key()];
    // A plain envelope and a key-signed bundle, timestamped or not.
    let envelope = crate::sign_event(&event, &digest(), &machine);
    assert_eq!(
        signed_by_one_of(&envelope, &registered).unwrap(),
        machine.public_key().keyid()
    );
    let stamped_bundle = bundle(&event, &machine, &Stamps(&pki, stamped()));
    assert_eq!(
        signed_by_one_of(&stamped_bundle, &registered).unwrap(),
        machine.public_key().keyid()
    );
    let plain_bundle = bundle(&event, &machine, &NoTimestamps);
    signed_by_one_of(&plain_bundle, &registered).unwrap();
    // Another machine's key, or none registered, is no match.
    let err = signed_by_one_of(&envelope, &[other.public_key()]).unwrap_err();
    assert_eq!(err.step(), 2, "{err}");
    assert!(signed_by_one_of(&envelope, &[]).is_err());
    // A signature over other bytes doesn't count.
    let mut forged: serde_json::Value = serde_json::from_slice(&envelope).unwrap();
    let other_payload =
        serde_json::to_vec(&check_event().statement(&Digest::of(b"other"))).unwrap();
    forged["payload"] = base64::engine::general_purpose::STANDARD
        .encode(other_payload)
        .into();
    let forged = serde_json::to_vec(&forged).unwrap();
    assert_eq!(
        signed_by_one_of(&forged, &registered).unwrap_err().step(),
        2
    );
    // A keyless bundle never matches: a machine signs with its key.
    let signer = keyless(&pki, "123", "refs/heads/main");
    let keyless_bundle = bundle(&event, &signer, &Stamps(&pki, stamped()));
    let err = signed_by_one_of(&keyless_bundle, &[signer.key.public_key()]).unwrap_err();
    assert!(err.to_string().contains("keyless"), "{err}");
    // Not an event at all.
    assert_eq!(signed_by_one_of(b"{}", &registered).unwrap_err().step(), 1);
}

// covers: E-4, E-11
#[test]
fn a_certificate_identity_verifies_without_trust() {
    let pki = Pki::new();
    let root = root_of(&pki);
    let event = check_event();
    let signer = keyless(&pki, "123", "refs/heads/main");
    let bytes = bundle(&event, &signer, &Stamps(&pki, stamped()));
    let identity = certified_identity(&bytes, &root).unwrap();
    assert_eq!(identity.repository_id.as_deref(), Some("123"));
    assert_eq!(identity.reference.as_deref(), Some("refs/heads/main"));
    // From another trust root, or signed by a key: no identity.
    assert!(certified_identity(&bytes, &root_of(&Pki::new())).is_err());
    let by_key = sign_event_bundle(&event, &digest(), &key(1), &NoTimestamps)
        .unwrap()
        .bytes;
    assert!(certified_identity(&by_key, &root).is_err());
}
