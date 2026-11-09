//! Keys, configuration, attest, mark, verify and the ledger, end to end (M2).

use hushrun_testkit::schema;
use hushrun_testkit::world::{World, test_key};
use predicates::prelude::*;
use serde_json::Value;

const MEMBER: u8 = 1; // a member's machine

/// An image at the artifact's location, pinned by digest.
fn image(hex_char: char) -> String {
    format!(
        "us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        hex_char.to_string().repeat(64)
    )
}

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

fn ok(cmd: &mut assert_cmd::Command) -> Vec<u8> {
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn world() -> World {
    World::new("acme-api.yaml")
}

// covers: E-2
#[test]
fn keys_generate_and_public() {
    let w = world();
    let key = w.path("k.pem");
    let out = json(&ok(w
        .hushrun()
        .args(["keys", "generate", "--format", "json", "--out"])
        .arg(&key)));
    schema::assert_valid(&schema::load("keys.v1.schema.json"), &out);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Never overwrites a key.
    w.hushrun()
        .args(["keys", "generate", "--out"])
        .arg(&key)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("never overwrites"));
    let public = json(&ok(w
        .hushrun()
        .args(["keys", "public", "--format", "json", "--key"])
        .arg(&key)));
    assert_eq!(public["publicKey"], out["publicKey"]);
    assert_eq!(public["keyid"], out["keyid"]);
    // From HUSHRUN_SIGNING_KEY.
    let from_env = json(&ok(w
        .hushrun_as(3)
        .args(["keys", "public", "--format", "json"])));
    assert_eq!(from_env["publicKey"], test_key(3).public_key().to_string());
    // Without --out, the PEM goes to stdout and the rest to stderr.
    let output = w.hushrun().args(["keys", "generate"]).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("-----BEGIN PRIVATE KEY-----"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("public key: ed25519:"));
}

// covers: C-8
#[test]
fn policy_validate_reports_positions() {
    let w = world();
    let out = ok(w.hushrun().args(["policy", "validate"]));
    assert!(String::from_utf8_lossy(&out).contains("hushrun.yaml is valid"));
    // hushrun.yaml errors carry file:line:column.
    w.write(
        "hushrun.yaml",
        "version: 1\nrepository: github.com/a/b\ndeploy:\n  require: [nope]\n",
    );
    w.hushrun()
        .args(["policy", "validate"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "hushrun.yaml:4:13: unknown check `nope`",
        ));
    // [ADR 0062] A retired key is valid, and named where its value was written: hushrun 0.9
    // reads past it.
    w.write(
        "hushrun.yaml",
        "version: 1\nrepository: github.com/a/b\nchecks:\n  unit: { run: go test }\nstages:\n  prod:\n    require: [nope]\n",
    );
    w.hushrun()
        .args(["policy", "validate"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hushrun.yaml:6:3: `stages` is ignored since hushrun 0.9.0",
        ));
}

// covers: E-4, E-5, E-6, L-2, L-3
#[test]
fn attest_records_checks_and_artifacts() {
    let w = world();
    let out = json(&ok(w.hushrun_as(MEMBER).args([
        "attest",
        "--check",
        "unit=pass",
        "--check",
        "toolchain=fail",
        "--duration",
        "95s",
        "--format",
        "json",
    ])));
    schema::assert_valid(&schema::load("attest.v2.schema.json"), &out);
    let digest = out["digest"].as_str().unwrap().to_string();
    let keys = w.keys("v1/events/default/");
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(
        keys.iter().all(|k| k.ends_with(".check._.json")),
        "{keys:?}"
    );

    // The recorded statements, as the ledger holds them.
    let verify = json(&ok(w.hushrun().args([
        "verify", "--scope", "default", "--digest", &digest, "--format", "json",
    ])));
    schema::assert_valid(&schema::load("verify.v2.schema.json"), &verify);
    let results = verify["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r["accepted"] == true && r["signer"] == "user:key1@acme.dev")
    );

    let image = image('a');
    let out = ok(w.hushrun_as(MEMBER).args([
        "attest",
        "--artifact",
        &format!("image={image}"),
        "--format",
        "env",
    ]));
    let env = String::from_utf8(out).unwrap();
    assert!(env.contains(&format!("HUSHRUN_REF={image}\n")), "{env}");
    assert!(
        env.contains(&format!("HUSHRUN_REF_IMAGE={image}\n")),
        "{env}"
    );
}

// covers: C-6, A-4
#[test]
fn attest_refuses_artifacts_out_of_place() {
    let w = world();
    let attest = |artifact: &str| {
        w.hushrun_as(MEMBER)
            .args(["attest", "--artifact", artifact])
            .assert()
            .code(2)
    };
    // Locations are checked before a tag is resolved: no registry is asked.
    attest("image=us-central1-docker.pkg.dev/acme-prod/api/other:latest").stderr(
        predicate::str::contains("not at artifact `image`'s location"),
    );
    attest(&format!(
        "image={}",
        image('a').replace("api/go", "api/other")
    ))
    .stderr(predicate::str::contains(
        "not at artifact `image`'s location",
    ));
    attest(&format!("nope={}", image('a'))).stderr(predicate::str::contains("unknown artifact"));
    // Another project's registry.
    let other = image('a').replace("acme-prod", "acme-test");
    attest(&format!("image={other}")).stderr(predicate::str::contains("location"));
    // Nothing was written.
    assert!(w.keys("v1/").is_empty());
}

// covers: C-5, E-7
#[test]
fn attest_needs_the_environment_of_pinned_checks() {
    let w = World::new("deploy.yaml");
    let pinned = "golangci/golangci-lint@sha256:1111111111111111111111111111111111111111111111111111111111111111";
    w.hushrun_as(1)
        .args(["attest", "--scope", "api", "--check", "lint=pass"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--environment-image"));
    ok(w.hushrun_as(1).args([
        "attest",
        "--scope",
        "api",
        "--check",
        "lint=pass",
        "--environment-image",
        pinned,
    ]));
    let key = w.keys("v1/events/api/").pop().unwrap();
    let statement = statement_of(&w, &key);
    let check = &statement["predicate"]["check"];
    assert_eq!(check["environment"]["image"], pinned);
    assert_eq!(check["environment"]["platform"], "linux/amd64");
    assert!(
        check["environmentDigest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    // A check with no `run`, which another tool runs, is attested like any other, and records
    // the definition every check has (E-6).
    ok(w.hushrun_as(1)
        .args(["attest", "--scope", "api", "--check", "sast=pass"]));
    let sast = w
        .keys("v1/events/api/")
        .into_iter()
        .map(|k| statement_of(&w, &k))
        .find(|s| s["predicate"]["check"]["name"] == "sast")
        .expect("sast's event");
    assert_eq!(sast["predicate"]["check"]["checkKind"], "source", "{sast}");
}

/// The DSSE envelope of a stored event: the bundle's (E-1), or the object itself.
fn envelope_of(bytes: &[u8]) -> Value {
    let object = json(bytes);
    match object.get("dsseEnvelope") {
        Some(envelope) => envelope.clone(),
        None => object,
    }
}

fn statement_of(w: &World, key: &str) -> Value {
    use base64::Engine as _;
    let envelope = envelope_of(&w.object(key));
    let payload = base64::engine::general_purpose::STANDARD
        .decode(envelope["payload"].as_str().unwrap())
        .unwrap();
    json(&payload)
}

// covers: E-4, E-5, L-2, P-5
#[test]
fn a_deploy_follows_plan_start_and_an_attempt_has_one_started() {
    let w = world();
    let run = |kind: &str, extra: &[&str]| {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.env("HUSHRUN_RUN_ID", "run-1")
            .args(["mark", kind])
            .args(extra);
        ok(&mut cmd)
    };
    // A deploy needs the `started` of `plan --start`, which records its listings (L-11).
    w.hushrun_as(MEMBER)
        .env("HUSHRUN_RUN_ID", "run-0")
        .args(["mark", "deployed"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Run `hushrun plan --start` first"));
    ok(w.hushrun_as(MEMBER)
        .env("HUSHRUN_RUN_ID", "run-1")
        .args(["plan", "--start"]));
    let image = image('b');
    ok(w.hushrun_as(MEMBER).env("HUSHRUN_RUN_ID", "run-1").args([
        "attest",
        "--artifact",
        &format!("image={image}"),
    ]));
    // deployed: the artifact comes from this run's artifact event.
    let out = String::from_utf8(run("deployed", &["--target", "cloud-run=back"])).unwrap();
    assert!(
        !out.contains("recorded started") && out.contains("recorded deployed"),
        "{out}"
    );
    let out = String::from_utf8(run("passed", &[])).unwrap();
    assert!(
        !out.contains("recorded started"),
        "one started per attempt: {out}"
    );
    let started: Vec<_> = w
        .keys("v1/events/")
        .into_iter()
        .filter(|k| k.contains(".started."))
        .collect();
    assert_eq!(started.len(), 1);

    // The deployed index holds the same bytes as the event.
    let deployed_event = w
        .keys("v1/events/")
        .into_iter()
        .find(|k| k.contains(".deployed."))
        .unwrap();
    // [ADR 0062] Under `_`, where hushrun 0.8 wrote the stage (L-8).
    let index = w.keys("v1/deployed/default/_/");
    assert_eq!(index.len(), 1);
    assert_eq!(w.object(&deployed_event), w.object(&index[0]));
    let statement = statement_of(&w, &deployed_event);
    assert_eq!(
        statement["predicate"]["deployed"]["artifacts"][0]["ref"],
        image
    );
    assert_eq!(
        statement["predicate"]["deployed"]["target"]["name"],
        "cloud-run"
    );

    // superseded after passed is not recorded (P-6 would ignore it).
    let mut cmd = w.hushrun_as(MEMBER);
    cmd.env("HUSHRUN_RUN_ID", "run-1")
        .args(["mark", "superseded", "--reason", "cancelled"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("notice: not recording `superseded`"));
    assert!(
        !w.keys("v1/events/")
            .iter()
            .any(|k| k.contains(".superseded."))
    );
    // Another attempt of the same run starts again.
    let mut cmd = w.hushrun_as(MEMBER);
    cmd.env("HUSHRUN_RUN_ID", "run-1")
        .env("HUSHRUN_RUN_ATTEMPT", "2")
        .args([
            "mark",
            "failed",
            "--reason",
            "deploy failed",
            "--format",
            "json",
        ]);
    let out = json(&ok(&mut cmd));
    schema::assert_valid(&schema::load("mark.v2.schema.json"), &out);
    assert_eq!(out["run"]["attempt"], 2);
    let kinds: Vec<&str> = out["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["started", "failed"]);
}

// covers: P-6, E-4
#[test]
fn a_passed_counts_from_when_it_is_signed() {
    let w = world();
    let mark = |run: &str, kind: &str, now: Option<&str>| {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.env("HUSHRUN_RUN_ID", run).args(["mark", kind]);
        if kind == "passed" {
            cmd.args(["--artifact", &format!("image={}", image('b'))]);
        }
        if let Some(now) = now {
            cmd.args(["--now", now]);
        }
        let output = cmd.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let at = |secs_ago: i64| {
        hushrun_core::time::Time::from_millis(
            hushrun_core::time::Time::from_timestamp(jiff::Timestamp::now()).millis()
                - secs_ago * 1000,
        )
        .unwrap()
        .to_string()
    };
    // A failure 57 s before `mark passed` starts. Its reads take 5 s, so the `passed` is signed
    // more than 60 s after the failure, and P-6 lets it override: it is recorded.
    let failed = at(57);
    mark("run-1", "failed", Some(&failed));
    w.fake.set_batch_delay(std::time::Duration::from_secs(5));
    let stderr = mark("run-1", "passed", None);
    assert!(!stderr.contains("not recording"), "{stderr}");
    assert!(
        w.keys("v1/events/").iter().any(|k| k.contains(".passed.")),
        "{stderr}"
    );
    // Within 60 s of a failure, the failure still wins.
    w.fake.set_batch_delay(std::time::Duration::ZERO);
    mark("run-2", "failed", Some(&at(10)));
    let stderr = mark("run-2", "passed", None);
    assert!(
        stderr.contains("notice: not recording `passed`"),
        "{stderr}"
    );
}

// covers: L-2, L-8
#[test]
fn a_deploy_whose_index_write_failed_is_completed_when_it_runs_again() {
    let w = world();
    let run = || {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.env("HUSHRUN_RUN_ID", "run-1");
        cmd
    };
    ok(run().args(["plan", "--start"]));
    ok(run().args(["attest", "--artifact", &format!("image={}", image('b'))]));
    let deployed_events = || -> Vec<String> {
        w.keys("v1/events/")
            .into_iter()
            .filter(|k| k.contains(".deployed."))
            .collect()
    };
    // The event goes to v1/events, and its second write, to the deployed index, is refused.
    w.fake.inject_on(
        Some("PUT"),
        Some("v1/deployed/"),
        hushrun_testkit::fake_ledger::Fault::Status(403, None),
        1,
    );
    let output = run().args(["mark", "deployed"]).output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(deployed_events().len(), 1);
    assert!(w.keys("v1/deployed/").is_empty());
    // Running it again writes the index entry with the same bytes, and no second event.
    let out = String::from_utf8(ok(run().args(["mark", "deployed"]))).unwrap();
    assert!(out.contains("recorded deployed"), "{out}");
    let events = deployed_events();
    assert_eq!(events.len(), 1, "{events:?}");
    let index = w.keys("v1/deployed/default/_/");
    assert_eq!(index.len(), 1);
    assert_eq!(w.object(&events[0]), w.object(&index[0]));
    // Once both are there, a deploy recorded again is a new event, as before.
    ok(run().args(["mark", "deployed"]));
    assert_eq!(deployed_events().len(), 2);
    assert_eq!(w.keys("v1/deployed/default/_/").len(), 2);
}

// covers: C-6
#[test]
fn mark_needs_every_artifact_of_the_scope() {
    let w = world();
    ok(w.hushrun_as(MEMBER)
        .env("HUSHRUN_RUN_ID", "run-1")
        .args(["plan", "--start"]));
    w.hushrun_as(MEMBER)
        .env("HUSHRUN_RUN_ID", "run-1")
        .args(["mark", "deployed"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("declares artifact(s) image"));
    w.hushrun_as(MEMBER)
        .args(["mark", "failed", "--artifact", "image=x"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--artifact doesn't apply"));
}

// covers: C-6, T-15
#[test]
fn mark_takes_only_the_runs_accepted_artifacts() {
    let w = world();
    let attest_as = |key: u8, run: &str, reference: &str| {
        ok(w.hushrun_as(key).env("HUSHRUN_RUN_ID", run).args([
            "attest",
            "--artifact",
            &format!("image={reference}"),
        ]));
    };
    attest_as(MEMBER, "r1", &image('a'));
    // A key the server doesn't list claims the same run: its artifact doesn't count.
    attest_as(7, "r1", &image('b'));
    // Another run of the same digest, by another member.
    attest_as(2, "r2", &image('c'));
    let out = json(&ok(w
        .hushrun_as(MEMBER)
        .env("HUSHRUN_RUN_ID", "r1")
        .args(["mark", "passed", "--format", "json"])));
    let key = out["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "passed")
        .unwrap()["keys"][0]
        .as_str()
        .unwrap()
        .to_string();
    let shown = json(&ok(w.hushrun().args([
        "repository",
        "show",
        &key,
        "--format",
        "json",
    ])));
    let artifacts = &shown["statement"]["predicate"]["passed"]["artifacts"];
    assert_eq!(artifacts.as_array().unwrap().len(), 1, "{artifacts}");
    assert_eq!(artifacts[0]["ref"], image('a'));
}

// covers: E-8
#[test]
fn verify_files_exits_5_on_any_rejection() {
    let w = world();
    ok(w.hushrun_as(MEMBER)
        .args(["attest", "--check", "unit=pass"]));
    let key = w.keys("v1/events/").pop().unwrap();
    let good = w.export(&key);
    ok(w.hushrun().arg("verify").arg(&good));
    // Signed by a key the server doesn't list as a member's machine (T-15).
    ok(w.hushrun_as(7).args(["attest", "--check", "unit=pass"]));
    let unknown = w
        .keys("v1/events/")
        .into_iter()
        .find(|k| k != &key)
        .map(|k| w.export(&k))
        .unwrap();
    w.hushrun()
        .arg("verify")
        .arg(&good)
        .arg(&unknown)
        .assert()
        .code(5)
        .stdout(predicate::str::contains("REJECTED").and(predicate::str::contains("step 2")));
    // Tampered payload.
    let mut envelope = json(&std::fs::read(&good).unwrap());
    let payload = envelope["dsseEnvelope"]["payload"]
        .as_str()
        .unwrap()
        .replace('A', "B");
    envelope["dsseEnvelope"]["payload"] = Value::String(payload);
    let tampered = w.path("tampered.json");
    std::fs::write(&tampered, serde_json::to_vec(&envelope).unwrap()).unwrap();
    w.hushrun().arg("verify").arg(&tampered).assert().code(5);
}

// covers: T-4, L-1
#[test]
fn ci_reads_the_ledger_from_hushrun_ledger_only() {
    let w = world();
    let attest = || {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.env("CI", "true")
            .env("HUSHRUN_RUN_ID", "7")
            .args(["attest", "--check", "unit=pass"]);
        cmd
    };
    // [ADR 0031] Without HUSHRUN_REPOSITORY_URL, hushrun.yaml names the server, which gets the
    // job's ID token but never HUSHRUN_REPOSITORY_TOKEN: this fake takes only that.
    attest()
        .env_remove("HUSHRUN_REPOSITORY_URL")
        .assert()
        .code(6)
        .stderr(predicate::str::contains(
            "comes from hushrun.yaml, so hushrun sends it the job's ID token but not HUSHRUN_REPOSITORY_TOKEN",
        ));
    attest()
        .env("HUSHRUN_REPOSITORY_URL", "https://somewhere.example/l/else")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("hushrun.yaml's `repository_url`"));
    attest()
        .env("HUSHRUN_REPOSITORY_URL", w.ledger_url())
        .args(["--repository-url", "https://other.example/l/x"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "only HUSHRUN_REPOSITORY_URL counts",
        ));
    ok(attest().env("HUSHRUN_REPOSITORY_URL", format!("{}/", w.ledger_url())));
    // Generic CI needs a run id.
    let mut cmd = w.hushrun_as(MEMBER);
    cmd.env("CI", "true")
        .env("HUSHRUN_REPOSITORY_URL", w.ledger_url())
        .args(["attest", "--check", "unit=pass"]);
    cmd.assert()
        .code(2)
        .stderr(predicate::str::contains("HUSHRUN_RUN_ID"));
    // Events from CI claim custody ci.
    let key = w.keys("v1/events/").pop().unwrap();
    assert_eq!(
        statement_of(&w, &key)["predicate"]["producer"]["custody"],
        "ci"
    );
    assert_eq!(
        statement_of(&w, &key)["predicate"]["run"]["provider"],
        "generic"
    );
    // Commands that never touch the ledger don't need it.
    ok(w.hushrun().env("CI", "true").arg("digest"));
}

// covers: L-5
#[test]
fn an_unreachable_ledger_exits_6() {
    let w = world();
    let missing = format!("{}/missing", w.ledger_url());
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write("hushrun.yaml", &policy.replace(&w.ledger_url(), &missing));
    w.commit("a URL where nothing answers");
    w.hushrun()
        .args(["verify", "--scope", "default", "--digest"])
        .arg(format!("sha256:{}", "0".repeat(64)))
        .args(["--repository-url", &missing])
        .assert()
        .code(6);
    // `policy explain` shows the pipeline without the server, saying whether the repository is
    // CI-only wasn't checked (T-16).
    w.hushrun()
        .args(["policy", "explain", "--repository-url", &missing])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("deploys from main\n").and(predicate::str::contains(
                "whether the repository is CI only wasn't checked: ",
            )),
        );
}

// covers: C-1
#[test]
fn a_trust_section_is_an_unknown_key() {
    let w = world();
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write(
        "hushrun.yaml",
        &format!("{policy}trust:\n  principals: {{}}\n"),
    );
    w.commit("an old trust section");
    w.hushrun()
        .args(["policy", "validate"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("trust"));
}

#[test]
fn ledger_ls_and_show() {
    let w = world();
    ok(w.hushrun_as(MEMBER)
        .args(["attest", "--check", "unit=pass"]));
    let out = json(&ok(w.hushrun().args([
        "repository",
        "ls",
        "--format",
        "json",
    ])));
    let events = out["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "check");
    let key = events[0]["key"].as_str().unwrap();
    let shown = json(&ok(w.hushrun().args([
        "repository",
        "show",
        key,
        "--format",
        "json",
    ])));
    assert!(
        shown["verification"]
            .as_str()
            .unwrap()
            .starts_with("accepted"),
        "{shown}"
    );
    assert_eq!(shown["statement"]["predicate"]["check"]["name"], "unit");
    // Another digest's events: none.
    let none = json(&ok(w.hushrun().args([
        "repository",
        "ls",
        "--scope",
        "default",
        "--digest",
        &format!("sha256:{}", "0".repeat(64)),
        "--format",
        "json",
    ])));
    assert!(none["events"].as_array().unwrap().is_empty(), "{none}");
    w.hushrun()
        .args(["repository", "show", "v1/../etc/passwd"])
        .assert()
        .code(2);
}

// covers: T-16, C-7
#[test]
fn policy_explain_shows_acceptance() {
    let w = World::new("deploy.yaml");
    let out = json(&ok(w
        .hushrun()
        .args(["policy", "explain", "--format", "json"])));
    schema::assert_valid(&schema::load("policy-explain.v2.schema.json"), &out);
    assert_eq!(out["ciOnly"]["checked"], true, "{out}");
    // The checks `deploy.require` names, in its order, a check with no `run` too (C-7).
    let reqs = out["requirements"].as_array().unwrap();
    let names: Vec<&str> = reqs
        .iter()
        .map(|r| r["requirement"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["unit", "lint", "e2e", "sast"]);
    // Whether the repository takes evidence from CI only is the server's answer, not the
    // policy's (T-16): this server doesn't make it CI-only, so the signers count
    // (tests/explain.rs).
    assert!(reqs.iter().all(|r| r["accepts"] == "signers"), "{out}");
    assert!(out.get("accepts").is_none());
    assert_eq!(out["deploy"]["branch"], "main");
    assert_eq!(out["deploy"]["lease"], "1h");
    assert_eq!(out["deploy"]["dedupe"], serde_json::json!(["push"]));
    assert_eq!(out["deploy"]["allowForce"], false);
    // [ADR 0062] Nothing per stage, no deployment checks and no approvals.
    for gone in ["stage", "deploymentChecks", "approvals"] {
        assert!(out.get(gone).is_none(), "{gone}: {out}");
    }
    // Once the server makes the repository CI-only, only CI's evidence counts.
    w.fake
        .set_repository_ci_only(hushrun_core::time::Time::parse("2026-01-01T00:00:00Z").unwrap());
    let out = json(&ok(w
        .hushrun()
        .args(["policy", "explain", "--format", "json"])));
    let reqs = out["requirements"].as_array().unwrap();
    assert!(reqs.iter().all(|r| r["accepts"] == "ci"), "{out}");
    assert_eq!(out["ciOnly"]["now"], true, "{out}");
}

/// Python, if this machine has it.
fn python() -> Option<std::process::Command> {
    let ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    ok.then(|| {
        let mut cmd = std::process::Command::new("python3");
        cmd.arg("-I");
        cmd
    })
}

fn refdsse() -> std::path::PathBuf {
    hushrun_testkit::workspace_root().join("tools/refdsse.py")
}

// covers: E-1, E-2
#[test]
fn envelopes_cross_verify_with_the_python_reference() {
    let Some(_) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let w = world();
    // Rust signs; Python verifies.
    ok(w.hushrun_as(MEMBER)
        .args(["attest", "--check", "unit=pass"]));
    let key = w.keys("v1/events/").pop().unwrap();
    let output = python()
        .unwrap()
        .arg(refdsse())
        .arg("verify")
        .arg(w.export(&key))
        .args(["--key", &test_key(MEMBER).public_key().to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let statement = json(&output.stdout);
    assert_eq!(statement["predicate"]["check"]["name"], "unit");

    // Python signs a statement; hushrun verifies it. Same payload, same seed: identical bytes.
    let payload = {
        use base64::Engine as _;
        let envelope = envelope_of(&w.object(&key));
        base64::engine::general_purpose::STANDARD
            .decode(envelope["payload"].as_str().unwrap())
            .unwrap()
    };
    let payload_file = w.path("payload.json");
    std::fs::write(&payload_file, &payload).unwrap();
    let seed_hex = format!("{MEMBER:02x}").repeat(32);
    let output = python()
        .unwrap()
        .arg(refdsse())
        .args(["sign", "--bundle"])
        .arg(&payload_file)
        .args(["--seed", &seed_hex])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let python_envelope = w.path("python-envelope.json");
    std::fs::write(&python_envelope, &output.stdout).unwrap();
    ok(w.hushrun().arg("verify").arg(&python_envelope));
    assert_eq!(
        json(&output.stdout),
        json(&w.object(&key)),
        "Ed25519 is deterministic: both implementations produce the same bundle"
    );
}
