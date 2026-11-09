//! M10 end to end: keyless signing against a fake Sigstore (Fulcio, a timestamp authority,
//! GitHub's ID tokens), offline verification from a refreshed cache, P-256 keys, and \[ADR 0031\]
//! the repository's CI and revoked machines (T-15). Nothing here reaches the network. \[ADR 0062\]
//! One pipeline: CI signs keylessly without a stage, and CI-only is the repository's one switch.

use std::path::Path;

use hushrun_attest::testing::{TufRepo, github_claims, p256_key};
use hushrun_core::time::Time;
use hushrun_testkit::sigstore::FakeSigstore;
use hushrun_testkit::world::{World, test_key};
use serde_json::Value;

const MEMBER: u8 = 2; // a member's machine, key-signed
const GITHUB: &str = "https://token.actions.githubusercontent.com";

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

fn run(cmd: &mut assert_cmd::Command) -> (Option<i32>, Vec<u8>, String) {
    let output = cmd.output().unwrap();
    (
        output.status.code(),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn ok(cmd: &mut assert_cmd::Command) -> Vec<u8> {
    let (code, stdout, stderr) = run(cmd);
    assert_eq!(
        code,
        Some(0),
        "stdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&stdout)
    );
    stdout
}

fn image(c: char) -> String {
    format!(
        "us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        c.to_string().repeat(64)
    )
}

fn ci_claims() -> Value {
    github_claims(
        "Acme/api",
        "123456",
        "refs/heads/main",
        ".github/workflows/deploy.yml",
        None,
    )
}

/// \[ADR 0031\] A world whose server names GitHub Actions of repository 123456 as the
/// repository's CI (T-15).
fn world() -> World {
    let w = World::new("acme-api.yaml");
    w.fake.set_ci(GITHUB, "123456");
    w
}

/// `hushrun` signing keylessly with `fake`, as a GitHub Actions job would.
fn keyless(w: &World, fake: &FakeSigstore) -> assert_cmd::Command {
    let mut cmd = w.hushrun();
    cmd.envs(fake.env(&w.root)).env("HUSHRUN_RUN_ID", "run-1");
    cmd
}

/// The pipeline: the plan, checks, the image, deployed and passed.
fn pipeline(command: &dyn Fn() -> assert_cmd::Command) {
    ok(command().args(["plan", "--start"]));
    ok(command().args([
        "attest",
        "--check",
        "unit=pass",
        "--check",
        "toolchain=pass",
        "--artifact",
        &format!("image={}", image('a')),
    ]));
    ok(command().args(["mark", "deployed"]));
    ok(command().args(["mark", "passed"]));
}

/// A member's plan of the same digest, key-signed, as a later run on their machine plans it.
fn member_plan(w: &World, extra: &[(&str, String)]) -> Value {
    let mut cmd = w.hushrun_as(MEMBER);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let plan = json(&ok(cmd.args(["plan", "--format", "json"])));
    plan["scopes"][0].clone()
}

fn stored(w: &World) -> Vec<Value> {
    w.keys("v1/events/")
        .iter()
        .map(|k| json(&w.object(k)))
        .collect()
}

// covers: E-1, E-10, E-11, T-15, T-12
#[test]
fn keyless_events_are_bundles_that_verify_by_identity() {
    let fake = FakeSigstore::start();
    fake.set_claims(ci_claims());
    let w = world();
    pipeline(&|| keyless(&w, &fake));
    // Every event is a bundle with a signing certificate and a timestamp, and the ephemeral
    // key never left the process: the bundle has no key.
    let events = stored(&w);
    assert!(events.len() >= 5);
    for event in &events {
        assert_eq!(
            event["mediaType"],
            "application/vnd.dev.sigstore.bundle.v0.3+json"
        );
        let material = &event["verificationMaterial"];
        assert!(material["certificate"]["rawBytes"].is_string(), "{event}");
        assert!(material.get("publicKey").is_none());
        assert_eq!(
            material["timestampVerificationData"]["rfc3161Timestamps"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            event["dsseEnvelope"]["signatures"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    // One certificate per command, reused for its events.
    assert_eq!(fake.count("/api/v2/signingCert"), 4);
    assert!(fake.count("/api/v1/timestamp") >= events.len());
    // A member's plan, key-signed, accepts CI's keyless evidence and reuses its image.
    let trust_root = [(
        "HUSHRUN_SIGSTORE_TRUSTED_ROOT",
        fake.write_trusted_root(&w.root).display().to_string(),
    )];
    let decision = member_plan(&w, &trust_root);
    assert_eq!(decision["mode"], "promote", "{decision}");
    // verify says who signed and when.
    let digest = json(&ok(w.hushrun().args(["digest", "--format", "json"])))["scopes"][0]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let verified = json(&ok(w
        .hushrun()
        .env(trust_root[0].0, &trust_root[0].1)
        .args([
            "verify", "--scope", "default", "--digest", &digest, "--format", "json",
        ])));
    hushrun_testkit::schema::assert_valid(
        &hushrun_testkit::schema::load("verify.v2.schema.json"),
        &verified,
    );
    let first = &verified["results"][0];
    assert_eq!(first["accepted"], true, "{verified}");
    assert_eq!(first["signer"], "ci", "{verified}");
    assert_eq!(first["signature"]["kind"], "identity");
    assert_eq!(first["signature"]["identity"]["repositoryId"], "123456");
    assert!(first["trustedTime"].is_string());
    // Without the test trust root, the public-good one doesn't know the test CA.
    let rejected = json(&ok(w.hushrun().args([
        "verify", "--scope", "default", "--digest", &digest, "--format", "json",
    ])));
    assert_eq!(rejected["results"][0]["accepted"], false);
}

// covers: T-15, E-11
#[test]
fn identity_mismatches_are_rejected() {
    let fake = FakeSigstore::start();
    let w = world();
    // A workflow on another branch of the same repository signs: it is the repository's CI.
    fake.set_claims(github_claims(
        "Acme/api",
        "123456",
        "refs/heads/feature",
        ".github/workflows/deploy.yml",
        None,
    ));
    ok(keyless(&w, &fake).args(["attest", "--check", "unit=pass"]));
    // Another repository calls the same reusable workflow: its subject names that workflow.
    fake.set_claims(github_claims(
        "stranger/app",
        "999",
        "refs/heads/main",
        ".github/workflows/deploy.yml",
        Some("Acme/api/.github/workflows/deploy.yml@refs/heads/main"),
    ));
    ok(keyless(&w, &fake).args(["attest", "--check", "toolchain=pass"]));
    let root = fake.write_trusted_root(&w.root);
    let files: Vec<_> = w.keys("v1/events/").iter().map(|k| w.export(k)).collect();
    let (code, stdout, _) = run(w
        .hushrun()
        .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &root)
        .arg("verify")
        .args(&files));
    assert_eq!(code, Some(5));
    let text = String::from_utf8_lossy(&stdout);
    assert_eq!(text.matches("ok ").count(), 1, "{text}");
    assert_eq!(
        text.matches("isn't the repository's CI").count(),
        1,
        "{text}"
    );
}

// covers: E-11, T-12
#[test]
fn keyless_signing_needs_a_timestamp_and_an_id_token() {
    let fake = FakeSigstore::start();
    fake.set_claims(ci_claims());
    let w = world();
    fake.set_tsa_down(true);
    let (code, _, stderr) = run(keyless(&w, &fake).args(["attest", "--check", "unit=pass"]));
    assert_eq!(code, Some(9), "{stderr}");
    assert!(stderr.contains("needs a trusted timestamp"), "{stderr}");
    assert!(w.keys("v1/events/").is_empty(), "nothing was written");
    // HUSHRUN_TSA=none can't sign keylessly at all.
    let (code, _, stderr) =
        run(keyless(&w, &fake)
            .env("HUSHRUN_TSA", "none")
            .args(["attest", "--check", "unit=pass"]));
    assert_eq!(code, Some(2), "{stderr}");
    // Without `id-token: write`, there is no ID token.
    let (code, _, stderr) = run(keyless(&w, &fake)
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .args(["attest", "--check", "unit=pass"]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("id-token: write"), "{stderr}");
    // Fulcio unreachable: the signer is unavailable.
    let (code, _, stderr) = run(keyless(&w, &fake)
        .env("HUSHRUN_FULCIO_URL", "http://127.0.0.1:9")
        .args(["attest", "--check", "unit=pass"]));
    assert_eq!(code, Some(9), "{stderr}");
}

fn tuf_mirror(repo: &TufRepo) -> hushrun_testkit::http::Server {
    let files = repo.files.clone();
    hushrun_testkit::http::Server::start(move |request| {
        let path = request.path.trim_start_matches('/');
        hushrun_testkit::http::Reply::Send(match files.get(path) {
            Some(bytes) => hushrun_testkit::http::Response::bytes(200, bytes.clone()),
            None => hushrun_testkit::http::Response::new(404),
        })
    })
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

// covers: T-12, E-11
#[test]
fn keyless_events_verify_offline_from_a_refreshed_cache() {
    let fake = FakeSigstore::start();
    fake.set_claims(ci_claims());
    let w = world();
    let repo = TufRepo::new(
        &fake.pki.trusted_root(),
        Time::parse("2099-01-01T00:00:00Z").unwrap(),
    );
    let mirror = tuf_mirror(&repo);
    let initial = w.path("tuf-root.json");
    write(&initial, &repo.root(1));
    let tuf = [
        ("HUSHRUN_SIGSTORE_TUF_URL", mirror.url()),
        ("HUSHRUN_SIGSTORE_TUF_ROOT", initial.display().to_string()),
    ];
    // Before a refresh, there is no trust root for this repository.
    let (code, _, stderr) = run(w
        .hushrun()
        .envs(tuf.clone())
        .args(["sigstore", "refresh", "--format", "json"])
        .env("HUSHRUN_TSA", "none"));
    assert_eq!(code, Some(0), "{stderr}");
    let cached = w
        .cache
        .join("sigstore")
        .join(mirror.addr().to_string().replace(':', "_"));
    // ADR 0061: the set `current` names, written whole.
    let named = std::fs::read_to_string(cached.join("current")).unwrap();
    let cached = cached.join(named.trim());
    assert!(
        cached.join("trusted_root.json").exists(),
        "{}",
        cached.display()
    );
    // Sign with the cached root: no HUSHRUN_SIGSTORE_TRUSTED_ROOT.
    pipeline(&|| {
        let mut cmd = keyless(&w, &fake);
        cmd.env_remove("HUSHRUN_SIGSTORE_TRUSTED_ROOT")
            .envs(tuf.clone());
        cmd
    });
    // Offline: the fake Sigstore and the mirror are gone, and a member's plan still verifies.
    drop(mirror);
    drop(fake);
    let decision = member_plan(&w, &tuf);
    assert_eq!(decision["mode"], "promote", "{decision}");
    // A cache someone changed doesn't verify, and isn't used.
    let mut changed = std::fs::read(cached.join("trusted_root.json")).unwrap();
    changed.push(b'\n');
    std::fs::write(cached.join("trusted_root.json"), changed).unwrap();
    let (code, _, stderr) = run(w.hushrun_as(MEMBER).envs(tuf.clone()).args(["plan"]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("ignoring the cached Sigstore trust root"),
        "{stderr}"
    );
}

// covers: E-2
#[test]
fn p256_key_files_sign_too() {
    let w = World::new("acme-api.yaml");
    let key = w.path("p256.pem");
    let generated = json(&ok(w
        .hushrun()
        .args([
            "keys",
            "generate",
            "--algorithm",
            "ecdsa-p256",
            "--format",
            "json",
            "--out",
        ])
        .arg(&key)));
    hushrun_testkit::schema::assert_valid(
        &hushrun_testkit::schema::load("keys.v1.schema.json"),
        &generated,
    );
    assert!(
        generated["publicKey"]
            .as_str()
            .unwrap()
            .starts_with("ecdsa-p256:")
    );
    // OpenSSL's key, a member's registered machine.
    let openssl = p256_key(1);
    let w = world();
    w.fake
        .register_machine(&openssl.public_key(), "user:bo@acme.dev", false);
    pipeline(&|| {
        let mut cmd = w.hushrun();
        cmd.env("HUSHRUN_SIGNING_KEY", openssl.to_pem().as_str())
            .env("HUSHRUN_RUN_ID", "run-1");
        cmd
    });
    assert_eq!(member_plan(&w, &[])["mode"], "promote");
}

// covers: T-15, E-10
#[test]
fn revoked_machines_count_by_trusted_time() {
    let fake = FakeSigstore::start();
    let now = "2026-10-15T12:00:00Z";
    fake.set_time(Some(Time::parse(now).unwrap()));
    // Key 1's machine is revoked an hour after the pipeline ran: its owner left the org.
    let w = world();
    w.fake.revoke_machine(
        &test_key(1).public_key().keyid(),
        Time::parse("2026-10-15T13:00:00Z").unwrap(),
    );
    let trust_root = fake.write_trusted_root(&w.root);
    // One check timestamped, the other not.
    ok(w.hushrun_as(1)
        .env("HUSHRUN_RUN_ID", "run-1")
        .env("HUSHRUN_TSA", fake.tsa_url())
        .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &trust_root)
        .args(["--now", now, "attest", "--check", "unit=pass"]));
    ok(w.hushrun_as(1).env("HUSHRUN_RUN_ID", "run-1").args([
        "--now",
        now,
        "attest",
        "--check",
        "toolchain=pass",
    ]));
    let digest = json(&ok(w.hushrun().args(["digest", "--format", "json"])))["scopes"][0]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let verify_at = |at: &str| {
        json(&ok(w
            .hushrun()
            .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &trust_root)
            .args([
                "--now", at, "verify", "--scope", "default", "--digest", &digest, "--format",
                "json",
            ])))
    };
    // While the machine counts, both count.
    let during = verify_at("2026-10-15T12:30:00Z");
    assert!(
        during["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["accepted"] == true),
        "{during}"
    );
    // A day later, it doesn't: only the timestamped check still counts, made while it did.
    let verified = verify_at("2026-10-16T12:00:00Z");
    let results = verified["results"].as_array().unwrap();
    let accepted: Vec<&Value> = results.iter().filter(|r| r["accepted"] == true).collect();
    assert_eq!(accepted.len(), 1, "{verified}");
    assert_eq!(accepted[0]["trustedTime"], "2026-10-15T12:00:00.000Z");
    let rejected = results.iter().find(|r| r["accepted"] == false).unwrap();
    assert!(
        rejected["reason"]
            .as_str()
            .unwrap()
            .contains("without a trusted timestamp"),
        "{rejected}"
    );
}

/// A member's two checks at `now` with key 1: `unit` timestamped by `fake`'s authority, and
/// `toolchain` without a timestamp.
fn member_checks(w: &World, fake: &FakeSigstore, trust_root: &Path, now: &str) {
    ok(w.hushrun_as(1)
        .env("HUSHRUN_RUN_ID", "run-1")
        .env("HUSHRUN_TSA", fake.tsa_url())
        .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", trust_root)
        .args(["--now", now, "attest", "--check", "unit=pass"]));
    ok(w.hushrun_as(1).env("HUSHRUN_RUN_ID", "run-1").args([
        "--now",
        now,
        "attest",
        "--check",
        "toolchain=pass",
    ]));
}

/// The status of each required check in a member's plan at `at`.
fn statuses(w: &World, trust_root: &Path, at: &str) -> (Value, Value, Value) {
    let plan = json(&ok(w
        .hushrun_as(MEMBER)
        .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", trust_root)
        .args(["--now", at, "plan", "--format", "json"])))["scopes"][0]
        .clone();
    let status = |name: &str| {
        plan["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["requirement"] == name)
            .unwrap_or_else(|| panic!("{name} in {plan}"))["status"]
            .clone()
    };
    (status("unit"), status("toolchain"), plan.clone())
}

// covers: T-16, L-13, E-10
// ADR 0031 amendment 1, ADR 0062: the server's signers answer names the repository CI-only from
// 12:30. A member's check timestamped at 12:00 keeps counting; an untimestamped one doesn't. A
// 0.8 server that still names a stage CI-only means the same: its periods count for the whole
// repository. A saved answer carries the rule to `verify --signers` offline.
#[test]
fn a_ci_only_repository_from_the_signers_answer_counts_what_others_made_before_it() {
    let since = "2026-10-15T12:30:00Z";
    /// How the server came to name the rule.
    type SetRule = fn(&World, Time);
    let rules: [(&str, SetRule, Value); 2] = [
        (
            "the repository",
            |w, since| w.fake.set_repository_ci_only(since),
            serde_json::json!([{"from": "2026-10-15T12:30:00.000Z"}]),
        ),
        (
            "a 0.8 server's stage",
            |w, since| w.fake.set_ci_only("production", since),
            serde_json::json!([{"stage": "production", "periods": [{"from": "2026-10-15T12:30:00.000Z"}]}]),
        ),
    ];
    for (rule, set, saved_rule) in rules {
        let fake = FakeSigstore::start();
        let now = "2026-10-15T12:00:00Z";
        fake.set_time(Some(Time::parse(now).unwrap()));
        let w = world();
        set(&w, Time::parse(since).unwrap());
        let trust_root = fake.write_trusted_root(&w.root);
        member_checks(&w, &fake, &trust_root, now);
        let at = "2026-10-15T13:00:00Z";
        let (unit, toolchain, plan) = statuses(&w, &trust_root, at);
        assert_eq!(unit, "satisfied", "{rule}: {plan}");
        assert_eq!(toolchain, "missing", "{rule}: {plan}");

        // `repository signers` saves the rule with the rest of the answer; `verify --signers`
        // reads it offline and says which events don't count.
        let saved = w.path("signers.json");
        let shown = ok(w
            .hushrun()
            .args(["repository", "signers", "--out"])
            .arg(&saved));
        let shown = String::from_utf8_lossy(&shown);
        assert!(
            shown.contains(
                "ci-only  evidence from CI only, for events made since 2026-10-15T12:30:00.000Z\n"
            ),
            "{rule}: {shown}"
        );
        let answer: Value = serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
        assert_eq!(answer["signers"]["ci_only"], saved_rule, "{rule}");
        let files: Vec<_> = w.keys("v1/events/").iter().map(|k| w.export(k)).collect();
        let verified = json(&ok(w
            .hushrun()
            .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &trust_root)
            .args(["--now", at, "verify", "--format", "json", "--signers"])
            .arg(&saved)
            .args(&files)));
        hushrun_testkit::schema::assert_valid(
            &hushrun_testkit::schema::load("verify.v2.schema.json"),
            &verified,
        );
        let results = verified["results"].as_array().unwrap();
        assert_eq!(results.len(), 2, "{rule}: {verified}");
        for result in results {
            assert_eq!(result["accepted"], true, "{rule}: {result}");
            assert!(result.get("stage").is_none(), "{rule}: {result}");
            let timestamped = !result["trustedTime"].is_null();
            assert_eq!(result["counts"], timestamped, "{rule}: {result}");
            if timestamped {
                assert!(result["notCounting"].is_null(), "{rule}: {result}");
            } else {
                assert_eq!(
                    result["notCounting"], "CI only, as the repository's server says",
                    "{rule}: {result}"
                );
            }
        }
        let text = String::from_utf8_lossy(&ok(w
            .hushrun()
            .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &trust_root)
            .args(["--now", at, "verify", "--signers"])
            .arg(&saved)
            .args(&files)))
        .into_owned();
        assert_eq!(
            text.matches("doesn't count: CI only, as the repository's server says")
                .count(),
            1,
            "{rule}: {text}"
        );
    }
}

// covers: T-16, L-13, E-10
// ADR 0031 amendment 2, ADR 0062: the repository was CI-only before, was lifted, and is CI-only
// again from 12:30. A member's check timestamped at 12:00 doesn't count when 12:00 falls in the
// earlier period, and does when it falls outside it; an untimestamped one doesn't, since the
// repository is CI-only now. The saved answer carries the periods to `verify --signers` offline.
#[test]
fn an_event_made_while_the_repository_was_ci_only_before_does_not_count_after_it_is_set_again() {
    for (earlier, counts) in [
        (("2026-10-15T11:30:00Z", "2026-10-15T12:15:00Z"), false),
        (("2026-10-15T10:00:00Z", "2026-10-15T11:00:00Z"), true),
    ] {
        let fake = FakeSigstore::start();
        let now = "2026-10-15T12:00:00Z";
        fake.set_time(Some(Time::parse(now).unwrap()));
        let w = world();
        w.fake.add_repository_ci_only_period(
            Time::parse(earlier.0).unwrap(),
            Some(Time::parse(earlier.1).unwrap()),
        );
        w.fake
            .set_repository_ci_only(Time::parse("2026-10-15T12:30:00Z").unwrap());
        let trust_root = fake.write_trusted_root(&w.root);
        member_checks(&w, &fake, &trust_root, now);
        let at = "2026-10-15T13:00:00Z";
        let (unit, toolchain, plan) = statuses(&w, &trust_root, at);
        let expected = if counts { "satisfied" } else { "missing" };
        assert_eq!(unit, expected, "{earlier:?}: {plan}");
        assert_eq!(toolchain, "missing", "{earlier:?}: {plan}");

        let saved = w.path("signers.json");
        let shown = ok(w
            .hushrun()
            .args(["repository", "signers", "--out"])
            .arg(&saved));
        let shown = String::from_utf8_lossy(&shown);
        assert!(
            shown.contains("for events made since 2026-10-15T12:30:00.000Z, and events made from "),
            "{shown}"
        );
        // Stage-less periods, oldest first; only the last is open.
        let answer: Value = serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
        let periods = answer["signers"]["ci_only"].as_array().unwrap();
        assert_eq!(periods.len(), 2, "{answer}");
        assert!(periods.iter().all(|p| p.get("stage").is_none()), "{answer}");
        assert!(periods[0].get("until").is_some(), "{answer}");
        assert!(periods[1].get("until").is_none(), "{answer}");
        let files: Vec<_> = w.keys("v1/events/").iter().map(|k| w.export(k)).collect();
        let verified = json(&ok(w
            .hushrun()
            .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &trust_root)
            .args(["--now", at, "verify", "--format", "json", "--signers"])
            .arg(&saved)
            .args(&files)));
        let results = verified["results"].as_array().unwrap();
        assert_eq!(results.len(), 2, "{verified}");
        for result in results {
            assert_eq!(result["accepted"], true, "{result}");
            let expected = !result["trustedTime"].is_null() && counts;
            assert_eq!(result["counts"], expected, "{earlier:?}: {result}");
        }
    }
}
