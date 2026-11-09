//! \[M18\] The hosted ledger's transparency log, end to end (L-11, RFC 0001 §4.7, H1): the CLI
//! against the testkit's fake HTTP ledger, which keeps a log as the hosted ledger does and can
//! lie as a dishonest one would.
//!
//! - A ledger that hides a failed attempt from a listing can sway one plan, but the monitor finds
//!   the listing the plan recorded, and the keys the log already held that it left out.
//! - A ledger that shows a reader another history than a writer fails the reader's consistency
//!   check, which names both signed checkpoints.
//! - \[M19\] A ledger that commits to a map leaving an attempt out can sway a plan too, and the
//!   monitor finds the map leaf that lies (L-12).
//!
//! \[ADR 0062\] One pipeline: every run plans, attests and marks without a stage.

use hushrun_attest::Envelope;
use hushrun_testkit::fake_ledger::{FakeLedger, ReceiptFault, Role};
use hushrun_testkit::world::World;
use hushrun_tlog::{SignedCheckpoint, Verifier};
use serde_json::Value;

const WRITER: u8 = 1; // an admin's machine
const READER: u8 = 2; // a member's machine
/// A token for the reader's runs, which the fake can show another history.
const READER_TOKEN: &str = "hrl_fake-reader-token";

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

fn run(cmd: &mut assert_cmd::Command) -> (i32, Vec<u8>, String) {
    let output = cmd.output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn ok(cmd: &mut assert_cmd::Command) -> Vec<u8> {
    let (code, stdout, stderr) = run(cmd);
    assert_eq!(
        code,
        0,
        "stdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&stdout)
    );
    stdout
}

fn world() -> World {
    let w = World::new("acme-api.yaml");
    fake(&w).add_token(READER_TOKEN, Role::Writer);
    w
}

fn fake(w: &World) -> &FakeLedger {
    &w.fake
}

fn verifier(w: &World) -> Verifier {
    let (origin, key) = fake(w).log_key();
    let key = hushrun_core::signers::PublicKey::parse(&key).unwrap();
    Verifier::new(&origin, key.ed25519().expect("an Ed25519 log key")).unwrap()
}

fn image() -> String {
    format!(
        "us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        "a".repeat(64)
    )
}

fn as_writer(w: &World, run_id: &str) -> impl Fn() -> assert_cmd::Command {
    move || {
        let mut cmd = w.hushrun_as(WRITER);
        cmd.env("HUSHRUN_RUN_ID", run_id);
        cmd
    }
}

fn as_reader(w: &World, run_id: &str) -> impl Fn() -> assert_cmd::Command {
    move || {
        let mut cmd = w.hushrun_as(READER);
        cmd.env("HUSHRUN_RUN_ID", run_id)
            .env("HUSHRUN_REPOSITORY_TOKEN", READER_TOKEN);
        cmd
    }
}

/// A run that plans, attests, deploys and passes.
fn deploy_passes(w: &World, run_id: &str) {
    let writer = as_writer(w, run_id);
    ok(writer().args(["plan", "--start"]));
    ok(writer().args([
        "attest",
        "--check",
        "unit=pass",
        "--check",
        "toolchain=pass",
        "--artifact",
        &format!("image={}", image()),
    ]));
    ok(writer().args(["mark", "deployed"]));
    ok(writer().args(["mark", "passed"]));
}

/// A second run of the same digest that plans, finds `unit` failing, and fails: the keys of its
/// three events, which a dishonest ledger hides.
fn unit_fails(w: &World, run_id: &str) -> Vec<String> {
    let before = w.keys("v1/events/");
    let writer = as_writer(w, run_id);
    ok(writer().args(["plan", "--start"]));
    ok(writer().args(["attest", "--check", "unit=fail"]));
    ok(writer().args(["mark", "failed", "--reason", "unit failed"]));
    let new: Vec<String> = w
        .keys("v1/events/")
        .into_iter()
        .filter(|k| !before.contains(k))
        .collect();
    assert_eq!(new.len(), 3, "started, the check and failed: {new:?}");
    new
}

/// The decoded predicate of every event in the ledger, by key.
fn events(w: &World) -> Vec<(String, Value)> {
    w.keys("v1/events/")
        .into_iter()
        .map(|key| {
            // [M10] Events are Sigstore bundles; `Envelope::parse` reads the envelope inside.
            let envelope = Envelope::parse(&fake(w).object(&key).unwrap()).unwrap();
            let statement: Value =
                serde_json::from_slice(&envelope.payload_bytes().unwrap()).unwrap();
            (key, statement["predicate"].clone())
        })
        .collect()
}

fn monitor(w: &World, extra: &[&str]) -> (i32, Value, String) {
    let (code, stdout, stderr) = run(w
        .hushrun()
        .args(["repository", "monitor", "--format", "json"])
        .args(extra));
    (code, json(&stdout), stderr)
}

// covers: E-4, E-5, L-11
#[test]
fn events_carry_checkpoints_and_plans_record_their_listings() {
    let w = world();
    deploy_passes(&w, "s1");
    let verifier = verifier(&w);
    let all = events(&w);
    assert!(all.len() >= 5, "{all:?}");
    let mut sizes = Vec::new();
    for (key, predicate) in &all {
        let note = predicate["checkpoint"]
            .as_str()
            .unwrap_or_else(|| panic!("{key} carries no checkpoint: {predicate}"));
        sizes.push(SignedCheckpoint::open(note, &verifier).unwrap().size());
    }
    // Each writer verified a newer checkpoint than the one before it.
    assert!(sizes.windows(2).all(|w| w[0] <= w[1]) || !sizes.is_empty());
    let started: Vec<&Value> = all
        .iter()
        .filter(|(k, _)| k.contains(".started."))
        .map(|(_, p)| p)
        .collect();
    assert_eq!(started.len(), 1);
    let listings = started[0]["started"]["listings"].as_array().unwrap();
    assert!(!listings.is_empty(), "{}", started[0]);
    for listing in listings {
        let statement =
            hushrun_tlog::SignedListing::open(listing["statement"].as_str().unwrap(), &verifier)
                .unwrap();
        let keys: Vec<String> = listing["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect();
        assert!(statement.listing.matches(&keys));
    }
    // An honest ledger agrees with everything it signed.
    let (code, out, stderr) = monitor(&w, &[]);
    assert_eq!(code, 0, "{out}\n{stderr}");
    hushrun_testkit::schema::assert_valid(
        &hushrun_testkit::schema::load("repository-monitor.v1.schema.json"),
        &out,
    );
    assert_eq!(out["ok"], true);
    assert_eq!(out["checked"]["startedEvents"], 1);
    assert!(out["checked"]["listings"].as_u64().unwrap() >= 2);
    assert!(out["checked"]["checkpoints"].as_u64().unwrap() >= 1);
    assert_eq!(out["findings"].as_array().unwrap().len(), 0, "{out}");
}

// covers: L-11, E-5, P-4
#[test]
fn a_ledger_hiding_an_attempt_from_a_listing_is_caught_by_the_monitor() {
    let w = world();
    deploy_passes(&w, "s1");
    // The same digest runs again, and this time unit fails.
    let hidden = unit_fails(&w, "s2");

    // An honest listing makes the next run run unit again: its newest result failed (P-4).
    let reader = as_reader(&w, "p1");
    let plan = json(&ok(reader().args(["plan", "--format", "json"])));
    assert_eq!(plan["scopes"][0]["mode"], "refresh", "{plan}");
    assert_eq!(
        plan["scopes"][0]["checksToRun"][0]["check"], "unit",
        "{plan}"
    );
    assert_eq!(
        plan["scopes"][0]["checksToRun"][0]["reason"], "failed",
        "{plan}"
    );

    // The ledger hides the failed attempt, and signs for the listing it shows.
    for key in &hidden {
        fake(&w).hide_from_listings(key);
    }
    let reader = as_reader(&w, "p2");
    let plan = json(&ok(reader().args(["plan", "--start", "--format", "json"])));
    assert_eq!(
        plan["scopes"][0]["mode"], "promote",
        "the lie sways one plan: {plan}"
    );

    // The monitor finds it, with the ledger's own signed statement as proof.
    let (code, out, stderr) = monitor(&w, &[]);
    assert_eq!(code, 5, "{out}\n{stderr}");
    assert_eq!(out["ok"], false);
    let omitted: Vec<&Value> = out["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == "omitted_keys")
        .collect();
    assert_eq!(omitted.len(), 1, "{out}");
    let finding = omitted[0];
    assert_eq!(finding["severity"], "error");
    assert!(finding["stage"].is_null(), "{finding}");
    assert_eq!(finding["run"]["id"], "p2");
    assert!(
        finding["event"].as_str().unwrap().contains(".started._."),
        "{finding}"
    );
    let mut named: Vec<String> = finding["omitted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap().to_string())
        .collect();
    named.sort();
    let mut expected = hidden.clone();
    expected.sort();
    assert_eq!(named, expected);
    // The proof: a statement the ledger signed, whose keys the log contradicts.
    let statement =
        hushrun_tlog::SignedListing::open(finding["statement"].as_str().unwrap(), &verifier(&w))
            .unwrap();
    assert!(hidden.iter().all(|k| statement.listing.covers(k)));
    // The text output names them too.
    let (code, stdout, _) = run(w.hushrun().args(["repository", "monitor"]));
    assert_eq!(code, 5);
    let text = String::from_utf8_lossy(&stdout);
    assert!(text.contains("left out 3 key(s)"), "{text}");
    assert!(text.contains("hushrun-listing/v1"), "{text}");
}

// covers: L-11, E-4
#[test]
fn a_forked_view_fails_the_consistency_check() {
    let w = world();
    // The first attempt fails; the second starts after it, having seen the failure.
    let first = as_writer(&w, "s1");
    ok(first().args(["plan", "--start"]));
    ok(first().args(["mark", "failed", "--reason", "unit"]));
    let failed_attempt = w.keys("v1/events/");
    let second = as_writer(&w, "s2");
    ok(second().args(["plan", "--start"]));

    // A reader is shown a history in which the first attempt never was.
    fake(&w).fork_for(READER_TOKEN, &failed_attempt);
    let reader = as_reader(&w, "p1");
    let (code, stdout, stderr) = run(reader().args(["plan"]));
    assert_eq!(code, 6, "{}\n{stderr}", String::from_utf8_lossy(&stdout));
    assert!(stderr.contains("two histories"), "{stderr}");
    // Both signed checkpoints are named: the one in the second attempt's event, and the
    // reader's own.
    let notes: Vec<&str> = stderr
        .lines()
        .filter(|l| l.trim_start().starts_with('\u{2014}'))
        .collect();
    assert_eq!(notes.len(), 2, "{stderr}");
    assert_ne!(notes[0], notes[1]);
    // A writer shown the true history is fine.
    let honest = as_writer(&w, "s3");
    ok(honest().args(["plan"]));
}

// covers: L-11
#[test]
fn deploying_on_an_https_ledger_needs_plan_start() {
    let w = world();
    let writer = as_writer(&w, "s1");
    ok(writer().args([
        "attest",
        "--check",
        "unit=pass",
        "--check",
        "toolchain=pass",
        "--artifact",
        &format!("image={}", image()),
    ]));
    let (code, _, stderr) = run(writer().args(["mark", "deployed"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("plan --start"), "{stderr}");
    assert!(w.keys("v1/deployed/").is_empty());
    // An implicit `started` from another command doesn't count: only the plan's records listings.
    ok(writer().args(["mark", "started"]));
    let (code, _, _) = run(writer().args(["mark", "deployed"]));
    assert_eq!(code, 2);
    // The plan's attempt is the next one of this run; `mark` carries it with --plan.
    let plan = w.path("plan.json");
    ok(writer().args(["plan", "--start", "--out"]).arg(&plan));
    ok(writer().arg("--plan").arg(&plan).args([
        "mark",
        "deployed",
        "--artifact",
        &format!("image={}", image()),
    ]));
    // \[ADR 0062\] The deployed index has `_` where 0.8 put the stage.
    let deployed = w.keys("v1/deployed/");
    assert_eq!(deployed.len(), 1, "{deployed:?}");
    assert!(
        deployed[0].starts_with("v1/deployed/default/_/"),
        "{deployed:?}"
    );
}

// covers: T-9, L-11
#[test]
fn until_trust_names_the_log_key_hushrun_warns_and_checks_nothing() {
    let w = World::without_log_key("acme-api.yaml");
    let writer = as_writer(&w, "s1");
    let (code, _, stderr) = run(writer().args(["plan", "--start"]));
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("names no log key"), "{stderr}");
    assert_eq!(
        stderr.matches("names no log key").count(),
        1,
        "once: {stderr}"
    );
    for (key, predicate) in events(&w) {
        assert!(predicate.get("checkpoint").is_none(), "{key}");
        // The listings are recorded anyway: their statements verify once the key is known.
        if key.contains(".started.") {
            assert!(
                !predicate["started"]["listings"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
    let (code, _, stderr) = run(w.hushrun().args(["repository", "monitor"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("transparency_log"), "{stderr}");
}

// covers: L-9, L-11, T-15
#[test]
fn admins_machines_sign_deletions_and_the_monitor_attributes_them() {
    let w = World::new("acme-api.yaml");
    let old = || {
        let mut cmd = w.hushrun_as(WRITER);
        cmd.env("HUSHRUN_RUN_ID", "old")
            .args(["--now", "2026-01-01T00:00:00Z"]);
        cmd
    };
    ok(old().args(["attest", "--check", "unit=pass"]));
    let written = w.keys("v1/events/");
    assert!(!written.is_empty());
    let key_file = |n: u8| {
        let path = w.path(&format!("key{n}.pem"));
        std::fs::write(&path, hushrun_testkit::world::test_key(n).to_pem().as_str()).unwrap();
        path
    };
    // Neither a stranger nor a member who isn't an admin (the signers mark only admins' and
    // owners' machines `gc`) deletes anything.
    for n in [7, READER] {
        let (code, _, stderr) = run(w
            .hushrun()
            .args(["repository", "gc", "--older-than", "30d", "--key"])
            .arg(key_file(n)));
        assert_eq!(code, 2, "{stderr}");
        assert!(stderr.contains("can't sign GC statements"), "{stderr}");
        assert_eq!(w.keys("v1/events/"), written);
    }
    // An admin's machine does.
    ok(w.hushrun()
        .args(["repository", "gc", "--older-than", "30d", "--key"])
        .arg(key_file(WRITER)));
    assert!(w.keys("v1/events/").is_empty());
    let tombstones: Vec<_> = fake(&w)
        .log_entries()
        .into_iter()
        .filter(|e| e.kind == hushrun_tlog::EntryKind::Tombstone)
        .collect();
    assert_eq!(tombstones.len(), written.len());
    assert!(tombstones.iter().all(|t| t.gc.is_some()));
    let (code, out, stderr) = monitor(&w, &[]);
    assert_eq!(code, 0, "{out}\n{stderr}");
    assert_eq!(out["checked"]["deletions"], written.len());
    // A deletion nobody signed is the server's.
    ok(old().args(["attest", "--check", "toolchain=pass"]));
    let key = w.keys("v1/events/").pop().unwrap();
    fake(&w).delete_object(&key);
    let (code, out, _) = monitor(&w, &[]);
    assert_eq!(code, 5, "{out}");
    let finding = out["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "unsigned_deletion")
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(finding["key"], key);
}

// covers: L-11
#[test]
fn the_monitor_remembers_the_log_and_catches_a_rewrite() {
    let w = world();
    deploy_passes(&w, "s1");
    let state = w.path("monitor.json");
    let (code, out, _) = monitor(&w, &["--state", state.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    let remembered: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(remembered["checkpoint"], out["checkpoint"]["note"]);
    // The next run sees a log in which an early event never was.
    let first = w.keys("v1/events/").into_iter().next().unwrap();
    fake(&w).fork_for(FakeLedger::ADMIN, &[first]);
    let (code, out, _) = monitor(&w, &["--state", state.to_str().unwrap()]);
    assert_eq!(code, 5, "{out}");
    assert!(
        out["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["kind"] == "rollback" || f["kind"] == "fork"),
        "{out}"
    );
    // The state keeps the checkpoint the ledger contradicted.
    let kept: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(kept, remembered);
}

// covers: L-11
#[test]
fn a_remembered_checkpoint_the_key_cant_check_isnt_forgotten() {
    let w = world();
    deploy_passes(&w, "s1");
    let state = w.path("monitor.json");
    let (code, out, _) = monitor(&w, &["--state", state.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    let mut remembered: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    // Signed by another key, or changed: the log key can't check it.
    let note = remembered["checkpoint"]
        .as_str()
        .unwrap()
        .replacen('\n', "\nx", 1);
    remembered["checkpoint"] = Value::String(note);
    std::fs::write(&state, serde_json::to_vec(&remembered).unwrap()).unwrap();
    let (code, out, _) = monitor(&w, &["--state", state.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    let finding = out["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "unverifiable_checkpoint")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        finding["message"]
            .as_str()
            .unwrap()
            .ends_with("keeps it: remove the file to start again from this checkpoint"),
        "{finding}"
    );
    // It stays as it was, for the next run to say again, instead of the newest checkpoint.
    let kept: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(kept, remembered);
    // Nothing of a write is left beside it.
    let dir: Vec<_> = std::fs::read_dir(state.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("monitor.json"))
        .collect();
    assert_eq!(dir, ["monitor.json"]);
}

// covers: L-11, L-12, E-5
#[test]
fn the_monitor_checks_that_every_map_follows_from_the_log() {
    let w = world();
    fake(&w).set_maps(true);
    deploy_passes(&w, "s1");
    // An honest ledger's maps follow from its log.
    let (code, out, stderr) = monitor(&w, &[]);
    assert_eq!(code, 0, "{out}\n{stderr}");
    hushrun_testkit::schema::assert_valid(
        &hushrun_testkit::schema::load("repository-monitor.v1.schema.json"),
        &out,
    );
    assert!(out["checked"]["maps"].as_u64().unwrap() > 0, "{out}");
    // The same digest runs again, and this time unit fails.
    let hidden = unit_fails(&w, "s2");
    // The ledger commits to a map without the failed attempt, and lists as that map says.
    for key in &hidden {
        fake(&w).leave_out_of_map(key);
        fake(&w).hide_from_listings(key);
    }
    let reader = as_reader(&w, "p1");
    let plan = json(&ok(reader().args(["plan", "--start", "--format", "json"])));
    assert_eq!(plan["scopes"][0]["mode"], "promote", "{plan}");
    // Every monitor finds the map leaf that lies, whatever plans recorded.
    let (code, out, _) = monitor(&w, &[]);
    assert_eq!(code, 5, "{out}");
    let mismatches: Vec<&Value> = out["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == "map_mismatch")
        .collect();
    assert!(!mismatches.is_empty(), "{out}");
    assert_eq!(mismatches[0]["severity"], "error");
    let (code, stdout, _) = run(w.hushrun().args(["repository", "monitor"]));
    assert_eq!(code, 5);
    let text = String::from_utf8_lossy(&stdout);
    assert!(text.contains("map_mismatch"), "{text}");
}

// covers: L-11, E-4
#[test]
fn a_check_alone_is_checked_and_carries_a_checkpoint() {
    let w = world();
    let writer = as_writer(&w, "s1");
    ok(writer().args(["attest", "--check", "unit=pass"]));
    // The event carries the checkpoint its writer verified (E-4).
    let verifier = verifier(&w);
    let all = events(&w);
    assert!(!all.is_empty());
    for (key, predicate) in &all {
        let note = predicate["checkpoint"]
            .as_str()
            .unwrap_or_else(|| panic!("{key} carries no checkpoint: {predicate}"));
        SignedCheckpoint::open(note, &verifier).unwrap();
    }
    // A receipt that proves nothing: the write isn't known to be done, so it fails closed.
    fake(&w).set_receipt_fault(ReceiptFault::Corrupt);
    let (code, _, stderr) = run(writer().args(["attest", "--check", "toolchain=pass"]));
    assert_eq!(code, 6, "{stderr}");
    assert!(stderr.contains("receipt"), "{stderr}");
}
