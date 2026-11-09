//! `hushrun repository gc`, end to end (M4). \[ADR 0062\] The pipeline deploys without a stage,
//! under the deployed index's `_` segment; 0.8's entries at a stage stay readable to gc.

use hushrun_testkit::schema;
use hushrun_testkit::world::{World, test_key};
use serde_json::Value;

const ADMIN: u8 = 1; // an admin's machine

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

fn image(c: char) -> String {
    format!(
        "image=us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        c.to_string().repeat(64)
    )
}

// covers: L-9
#[test]
fn gc_deletes_old_events_but_keeps_the_newest_deployment() {
    let w = World::new("acme-api.yaml");
    // On a server that keeps a log, an admin's machine signs each deletion (L-9): key 1's
    // owner is an admin of the org here.
    let gc_key = w.path("admin.pem");
    std::fs::write(&gc_key, test_key(ADMIN).to_pem().as_str()).unwrap();
    let at = |when: &str, run: &str| {
        let mut cmd = w.hushrun_as(ADMIN);
        cmd.args(["--now", when]).env("HUSHRUN_RUN_ID", run);
        cmd
    };
    // January: the pipeline deploys this content; it stays the newest deployment. Its plan
    // uploads the policy, which GC keeps however old.
    ok(at("2026-01-10T09:59:00Z", "r1").args(["plan", "--start"]));
    let policies = w.keys("v1/policies/");
    assert_eq!(policies.len(), 1);
    ok(at("2026-01-10T10:00:00Z", "r1").args([
        "attest",
        "--check",
        "unit=pass",
        "--artifact",
        &image('a'),
    ]));
    ok(at("2026-01-10T10:05:00Z", "r1").args(["mark", "deployed"]));
    let deployed_events = w.keys("v1/events/").len();
    // February: other content is only checked, never deployed.
    w.write("main.go", "package main // february\n");
    w.commit("february");
    ok(at("2026-02-01T10:00:00Z", "r2").args(["attest", "--check", "unit=fail"]));
    // October: recent evidence for a third digest.
    w.write("main.go", "package main // october\n");
    w.commit("october");
    ok(at("2026-10-10T10:00:00Z", "r3").args(["attest", "--check", "unit=pass"]));
    let before = w.keys("v1/");

    let gc = |dry_run: bool| {
        let mut cmd = w.hushrun();
        cmd.args([
            "--now",
            "2026-10-14T00:00:00Z",
            "repository",
            "gc",
            "--older-than",
            "180d",
            "--format",
            "json",
        ]);
        cmd.arg("--key").arg(&gc_key);
        if dry_run {
            cmd.arg("--dry-run");
        }
        json(&ok(&mut cmd))
    };
    let dry = gc(true);
    schema::assert_valid(&schema::load("repository-gc.v1.schema.json"), &dry);
    assert_eq!(dry["dryRun"], true);
    assert_eq!(dry["cutoff"], "2026-04-17T00:00:00.000Z");
    let doomed: Vec<String> = dry["deleted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    // Only February's events: January's digest is the newest deployment, October's is recent.
    assert_eq!(doomed.len(), 1, "{dry}");
    assert!(doomed[0].contains(".check."), "{doomed:?}");
    assert_eq!(dry["kept"]["deployed"].as_array().unwrap().len(), 1);
    assert_eq!(w.keys("v1/"), before, "a dry run deletes nothing");

    let real = gc(false);
    assert_eq!(real["deleted"], dry["deleted"]);
    let after = w.keys("v1/");
    assert_eq!(after.len(), before.len() - 1);
    assert!(!after.contains(&doomed[0]));
    assert!(w.keys("v1/events/").len() >= deployed_events);
    assert_eq!(
        w.keys("v1/policies/"),
        policies,
        "policies are never collected"
    );
    // Idempotent.
    assert!(gc(false)["deleted"].as_array().unwrap().is_empty());
}

// covers: L-8, L-9, P-10, T-16
#[test]
fn the_deployment_that_counts_is_found_behind_twenty_that_dont() {
    use hushrun_core::time::Time;
    use hushrun_testkit::sigstore::FakeSigstore;
    let sigstore = FakeSigstore::start();
    let now = "2026-10-15T12:00:00Z";
    sigstore.set_time(Some(Time::parse(now).unwrap()));
    let w = World::new("acme-api.yaml");
    let root = sigstore.write_trusted_root(&w.root);
    let at = |run: &str, timestamped: bool| {
        let mut cmd = w.hushrun_as(ADMIN);
        cmd.args(["--now", now])
            .env("HUSHRUN_RUN_ID", run)
            .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &root);
        if timestamped {
            cmd.env("HUSHRUN_TSA", sigstore.tsa_url());
        }
        cmd
    };
    let deploy = |run: &str, timestamped: bool, image_char: char| {
        ok(at(run, timestamped).args(["plan", "--start"]));
        ok(at(run, timestamped).args(["mark", "deployed", "--artifact", &image(image_char)]));
    };
    // The pipeline deploys this content with a trusted timestamp, then other content from a
    // laptop, untimestamped. Then the repository takes CI's evidence only: the first deployment
    // still counts, since it was made before (T-16), the laptop's doesn't. Its entry is copied
    // 20 times, newer: none of those counts either.
    deploy("r1", true, 'a');
    let counting = w.keys("v1/deployed/default/_/");
    assert_eq!(counting.len(), 1);
    w.write("main.go", "package main // laptop\n");
    w.commit("laptop");
    deploy("r2", false, 'b');
    w.fake
        .set_repository_ci_only(Time::parse("2026-10-15T12:00:30Z").unwrap());
    let laptop = w
        .keys("v1/deployed/default/_/")
        .into_iter()
        .find(|k| !counting.contains(k))
        .unwrap();
    let bytes = w.object(&laptop);
    for i in 0..20 {
        w.put_object(
            &format!("v1/deployed/default/_/{:013}.01JB{:022}.json", i, i),
            &bytes,
        );
    }
    // The same deployment under a key with the same time and a smaller ULID: on a tie, the
    // smaller key is the one the planner takes.
    let (prefix, _) = counting[0].rsplit_once('.').unwrap();
    let (prefix, _) = prefix.rsplit_once('.').unwrap();
    let twin = format!("{prefix}.{}.json", "0".repeat(26));
    w.put_object(&twin, &w.object(&counting[0]));
    // The planner reads on past the first page and finds what the repository runs: P-10 knows
    // it.
    let plan = json(&ok(at("r3", false).args(["plan", "--format", "json"])));
    let first = json(&ok(w
        .hushrun()
        .args(["digest", "--rev", "HEAD~1", "--format", "json"])));
    assert_eq!(
        plan["scopes"][0]["deployed"]["digest"], first["scopes"][0]["digest"],
        "{plan}"
    );
    assert_eq!(plan["scopes"][0]["deployed"]["event"], twin.as_str());
    // GC keeps that one, however old.
    let gc = json(&ok(w
        .hushrun()
        .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &root)
        .args([
            "--now",
            "2027-10-15T12:00:00Z",
            "repository",
            "gc",
            "--older-than",
            "30d",
            "--dry-run",
            "--format",
            "json",
        ])));
    let kept: Vec<&str> = gc["kept"]["deployed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert!(kept.contains(&twin.as_str()), "{gc}");
    let deleted: Vec<&str> = gc["deleted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert!(!deleted.contains(&twin.as_str()), "{gc}");
}

/// The deployed event stored at `key`, and the digest it is about.
fn deployed_event(w: &World, key: &str) -> (hushrun_core::event::Event, hushrun_core::Digest) {
    let envelope = hushrun_attest::Envelope::parse(&w.object(key)).unwrap();
    let statement: hushrun_core::event::Statement =
        serde_json::from_slice(&envelope.payload_bytes().unwrap()).unwrap();
    let hex = statement.subject[0].digest["sha256"].clone();
    let event = serde_json::from_value(statement.predicate).unwrap();
    (event, hushrun_core::Digest::from_hex(&hex).unwrap())
}

/// `event` at `stage`, or without one, signed by machine `ADMIN` at `when`, stored as a release
/// stores a deployment: 0.9 without a stage, under `v1/events/…/<ULID>.deployed._.json` and
/// `v1/deployed/<scope>/_/`; 0.8 at a stage, under its name in both. Returns the deployed-index
/// key.
fn deploy_in(
    w: &World,
    stage: Option<&str>,
    event: &hushrun_core::event::Event,
    digest: &hushrun_core::Digest,
    when: &str,
) -> String {
    use hushrun_ledger::layout::{deployed_key, event_key};
    let stage = stage.map(|s| hushrun_core::names::StageName::new(s).unwrap());
    let mut event = event.clone();
    event.stage = stage.clone();
    event.time = hushrun_core::time::Time::parse(when).unwrap();
    let bytes = hushrun_attest::sign_event(&event, digest, &test_key(ADMIN));
    let ulid = hushrun_ledger::new_ulid(event.time.millis());
    let index = deployed_key(&event.scope, stage.as_ref(), event.time, &ulid);
    w.put_object(
        &event_key(&event.scope, digest, &ulid, event.kind, stage.as_ref()),
        &bytes,
    );
    w.put_object(&index, &bytes);
    index
}

// covers: L-2, L-9 (deployments without a stage, `_`, beside 0.8's at a stage)
#[test]
fn gc_keeps_the_newest_deployment_of_every_segment() {
    let w = World::new("acme-api.yaml");
    let at = |when: &str, run: &str| {
        let mut cmd = w.hushrun_as(ADMIN);
        cmd.args(["--now", when]).env("HUSHRUN_RUN_ID", run);
        cmd
    };
    // January: 0.9 deploys this content, without a stage.
    ok(at("2026-01-10T09:59:00Z", "r1").args(["plan", "--start"]));
    ok(at("2026-01-10T10:05:00Z", "r1").args(["mark", "deployed", "--artifact", &image('a')]));
    let deployed = w.keys("v1/deployed/default/_/");
    assert_eq!(deployed.len(), 1, "{:?}", w.keys("v1/deployed/"));
    let older = deployed[0].clone();
    let (event, january) = deployed_event(&w, &older);
    assert!(event.stage.is_none());
    // An 0.8 job deployed it to staging the day before, and its entry is still there; in
    // February 0.9 deploys other content: the newest under `_`, though staging's segment never
    // saw it.
    let staged = deploy_in(
        &w,
        Some("staging"),
        &event,
        &january,
        "2026-01-09T10:00:00Z",
    );
    assert!(
        staged.starts_with("v1/deployed/default/staging/"),
        "{staged}"
    );
    w.write("main.go", "package main // february\n");
    w.commit("february");
    let digest = json(&ok(w.hushrun().args(["digest", "--format", "json"])));
    let february =
        hushrun_core::Digest::parse(digest["scopes"][0]["digest"].as_str().unwrap()).unwrap();
    let newest = deploy_in(&w, None, &event, &february, "2026-02-01T10:00:00Z");
    let february_events = w.keys(&format!("v1/events/default/{}/", february.hex()));
    assert_eq!(february_events.len(), 1);
    let before = w.keys("v1/");

    let gc = json(&ok(w.hushrun().args([
        "--now",
        "2026-10-14T00:00:00Z",
        "repository",
        "gc",
        "--older-than",
        "180d",
        "--dry-run",
        "--format",
        "json",
    ])));
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect()
    };
    let mut kept = strings(&gc["kept"]["deployed"]);
    kept.sort();
    let mut expected = vec![newest.clone(), staged.clone()];
    expected.sort();
    // Each segment keeps its newest: staging's from 0.8, and `_`'s from 0.9.
    assert_eq!(kept, expected, "{gc}");
    // Only `_`'s older entry goes: February's events stay with the deployment that names them,
    // and January's with staging's.
    assert_eq!(
        strings(&gc["deleted"]),
        std::slice::from_ref(&older),
        "{gc}"
    );
    assert!(before.iter().any(|k| k == &older));
    for key in &february_events {
        assert!(!strings(&gc["deleted"]).contains(key), "{key}");
    }
}

#[test]
fn gc_needs_a_duration() {
    let w = World::new("acme-api.yaml");
    w.hushrun()
        .args(["repository", "gc", "--older-than", "soon"])
        .assert()
        .code(2);
}

/// `hushrun repository ls` in a fresh repository, with nothing of the machine's.
fn ls_in(dir: &std::path::Path) -> std::process::Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_hushrun"));
    cmd.current_dir(dir)
        .args(["repository", "ls"])
        .env(
            "HUSHRUN_CREDENTIALS",
            dir.join("..").join("credentials.json"),
        )
        .env("HUSHRUN_CACHE_DIR", dir.join("..").join("cache"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for var in [
        "CI",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "BUILDKITE",
        "CIRCLECI",
        "HUSHRUN_REPOSITORY_URL",
        "HUSHRUN_REPOSITORY_BASE",
        "HUSHRUN_CONFIG",
        "HUSHRUN_PROVIDER",
    ] {
        cmd.env_remove(var);
    }
    cmd.output().expect("hushrun")
}

// covers: T-4, T-5
#[test]
fn a_repository_url_only_in_the_working_tree_says_to_commit_it() {
    use hushrun_testkit::git;
    const NAMED: &str = "version: 1\nrepository: github.com/acme/shop\nrepository_url: https://ledger.example/l/acme/shop\n";
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git::init(&dir, "sha1");
    std::fs::write(dir.join("README.md"), "shop\n").unwrap();
    git::commit_all(&dir, "init");

    // Right after init: hushrun.yaml is in the working tree only.
    std::fs::write(dir.join("hushrun.yaml"), NAMED).unwrap();
    let out = ls_in(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("no repository URL: hushrun.yaml isn't committed yet, and hushrun reads it as committed: commit it, or pass --config hushrun.yaml"),
        "{stderr}"
    );

    // Committed without the key, which the working tree adds.
    std::fs::write(
        dir.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\n",
    )
    .unwrap();
    git::commit_all(&dir, "hushrun.yaml");
    std::fs::write(dir.join("hushrun.yaml"), NAMED).unwrap();
    let out = ls_in(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("no repository URL: hushrun.yaml's `repository_url:` isn't committed yet, and hushrun reads hushrun.yaml as committed: commit it, or pass --config hushrun.yaml"),
        "{stderr}"
    );

    // Neither has it: the message as before.
    std::fs::write(
        dir.join("hushrun.yaml"),
        "version: 1\nrepository: github.com/acme/shop\n",
    )
    .unwrap();
    let out = ls_in(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no repository URL: set `repository_url:` in hushrun.yaml, HUSHRUN_REPOSITORY_URL, or pass --repository-url"),
        "{stderr}"
    );
}
