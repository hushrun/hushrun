//! `hushrun ready` with hushrun.yaml's `deploy` (C-22): the checks it requires, what runs them
//! here, and what stays with CI.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;

use hushrun_testkit::container::FakeRuntime;
use hushrun_testkit::fake_ledger::Fault;
use hushrun_testkit::fake_results::quota_exceeded;
use hushrun_testkit::world::World;
use hushrun_testkit::{git, schema};
use serde_json::{Value, json};

const AGENT: u8 = 1;
const IMAGE: &str =
    "golang@sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn world(checks: &str) -> World {
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/acme/shop
{}
checks:
{checks}
deploy:
  branch: main
  require: [unit]
",
            w.ledger_lines()
        ),
    );
    w.commit("checks");
    w
}

fn ready(w: &World, args: &[&str]) -> (i32, Value) {
    let output = w
        .hushrun_as(AGENT)
        .args(["ready", "--format", "json"])
        .args(args)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!("{e}: {}", String::from_utf8_lossy(&output.stderr));
    });
    schema::assert_valid(&schema::load("ready.v2.schema.json"), &value);
    (output.status.code().unwrap_or(-1), value)
}

// covers: C-22
#[test]
fn a_check_with_no_image_runs_where_it_is() {
    let w = world("  unit: {run: \"true\"}");
    let (code, report) = ready(&w, &["--branch", "develop"]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(report.get("stage"), None, "{report:#}");
    assert_eq!(report["branch"], "develop");
    assert_eq!(report["checks"][0]["state"], "missing");
    assert_eq!(report["next"], json!(["hushrun run unit"]), "{report:#}");

    // The command it names is what makes it ready, and an agent's loop ends.
    let ran = w.hushrun_as(AGENT).args(["run", "unit"]).output().unwrap();
    assert!(ran.status.success(), "{ran:?}");
    let (code, report) = ready(&w, &["--branch", "develop"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["checks"][0]["state"], "passed");

    // Evidence is about the content, so the branch `deploy` names counts it too.
    let (code, report) = ready(&w, &["--branch", "main"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["checks"][0]["state"], "passed");
}

// covers: C-22
#[test]
fn a_runtime_is_looked_for_and_never_started() {
    let w = world(&format!(
        "  unit: {{run: \"true\", image: \"{IMAGE}\", platform: linux/amd64}}"
    ));
    let stub = w.root.join("stub-docker");
    let started = w.root.join("started");
    std::fs::write(&stub, format!("#!/bin/sh\ntouch {}\n", started.display())).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = w
        .hushrun_as(AGENT)
        .env("HUSHRUN_CONTAINER_RUNTIME", &stub)
        .args(["ready", "--format", "json"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"][0]["state"], "missing", "{report:#}");
    assert!(!started.exists(), "ready started the container runtime");
}

// covers: C-22
#[test]
fn a_pinned_check_runs_isolated_and_stays_with_ci_without_a_runtime() {
    let w = world(&format!(
        "  unit: {{run: \"true\", image: \"{IMAGE}\", platform: linux/amd64}}"
    ));
    // No Docker or Podman: nothing here can pass it.
    let output = w
        .hushrun_as(AGENT)
        .env("HUSHRUN_CONTAINER_RUNTIME", "/nothing/here")
        .args(["ready", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"][0]["state"], "ci", "{report:#}");
    assert!(
        report["checks"][0]["why"]
            .as_str()
            .is_some_and(|why| why.contains("no Docker or Podman")),
        "{report:#}"
    );

    // With one, it names the isolated run, of the revision it looked at.
    let _runtime = FakeRuntime::install(&w.root);
    let runtime = w.root.join("fake-docker");
    let with_runtime = |args: &[&str]| {
        let output = w
            .hushrun_as(AGENT)
            .env("HUSHRUN_CONTAINER_RUNTIME", &runtime)
            .args(["ready", "--format", "json"])
            .args(args)
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        (output.status.code(), value)
    };
    let (code, report) = with_runtime(&[]);
    assert_eq!(code, Some(10), "{report:#}");
    assert_eq!(report["next"], json!(["hushrun run --isolated unit"]));
    // An isolated run exports the revision, so other files in the worktree don't matter.
    w.write("elsewhere.txt", "not committed\n");
    let (_, report) = with_runtime(&[]);
    assert_eq!(report["notes"], json!([]), "{report:#}");
    let head = w.head();
    let (_, report) = with_runtime(&["--rev", &head]);
    assert_eq!(
        report["next"],
        json!([format!("hushrun run --rev {head} --isolated unit")])
    );
    let (_, report) = with_runtime(&["--worktree"]);
    assert_eq!(
        report["next"],
        json!(["hushrun run --worktree --isolated unit"])
    );
}

// covers: C-22
#[test]
fn what_cant_count_from_here_stays_with_ci() {
    // A repository that takes CI's evidence only: nothing here would count.
    let w = world("  unit: {run: \"true\"}");
    w.fake
        .set_repository_ci_only(hushrun_core::time::Time::parse("2026-01-01T00:00:00Z").unwrap());
    let (code, report) = ready(&w, &["--branch", "main"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["checks"][0]["state"], "ci");
    assert_eq!(
        report["checks"][0]["why"],
        "this repository takes CI's evidence only"
    );
    // A 0.8 server named a stage: its periods are the repository's.
    let w = world("  unit: {run: \"true\"}");
    w.fake.set_ci_only(
        "production",
        hushrun_core::time::Time::parse("2026-01-01T00:00:00Z").unwrap(),
    );
    let (_, report) = ready(&w, &[]);
    assert_eq!(report["checks"][0]["state"], "ci", "{report:#}");

    // A check with no `run`, whose CI job runs it, and one that needs a secret.
    let w = world("  unit: {}");
    let (code, report) = ready(&w, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["checks"][0]["state"], "ci");
    let w = world("  unit: {run: \"true\", env: [NPM_TOKEN]}");
    let (code, report) = ready(&w, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        report["checks"][0]["why"],
        "it needs NPM_TOKEN, which stays in CI"
    );

    // A key the server doesn't list: its evidence is ignored, so a run here changes nothing.
    let w = world("  unit: {run: \"true\"}");
    let output = w
        .hushrun_as(7)
        .args(["ready", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"][0]["state"], "ci", "{report:#}");
    assert!(
        report["checks"][0]["why"]
            .as_str()
            .is_some_and(|why| why.contains("lists no such machine")),
        "{report:#}"
    );

    // This machine can't sign what it would record.
    let w = world("  unit: {run: \"true\"}");
    let output = w
        .hushrun()
        .args(["ready", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"][0]["state"], "ci", "{report:#}");
    assert!(
        report["checks"][0]["why"]
            .as_str()
            .is_some_and(|why| why.contains("hushrun login")),
        "{report:#}"
    );
}

// covers: C-22
#[test]
fn a_server_that_cant_answer_says_nothing_about_the_code() {
    let w = world("  unit: {run: \"true\"}");
    let output = w
        .hushrun_as(AGENT)
        .env("HUSHRUN_REPOSITORY_URL", "http://127.0.0.1:9/l/acme/shop")
        .timeout(std::time::Duration::from_secs(120))
        .args(["ready", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"][0]["state"], "ci", "{report:#}");
}

// covers: C-22
#[test]
fn a_pass_the_orgs_plan_wont_record_isnt_asked_for_again() {
    let w = world("  unit: {run: \"true\"}");
    w.fake.inject_on(
        Some("PUT"),
        Some(".check."),
        Fault::Answer(402, quota_exceeded("used up", "https://hushrun.example/up")),
        100,
    );
    assert_eq!(ready(&w, &[]).0, 10);
    let ran = w.hushrun_as(AGENT).args(["run", "unit"]).output().unwrap();
    assert!(ran.status.success(), "{ran:?}");
    let (code, report) = ready(&w, &[]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["checks"][0]["state"], "ci");
}

// covers: C-22
#[test]
fn a_failure_after_a_pass_the_orgs_plan_wouldnt_record_is_asked_for_again() {
    // `unit` passes, then fails on the same files: what it reads is outside the repository.
    let w = world("  unit: {run: \"test ! -e ../red\"}");
    w.fake.inject_on(
        Some("PUT"),
        Some(".check."),
        Fault::Answer(402, quota_exceeded("used up", "https://hushrun.example/up")),
        1,
    );
    let run = |w: &World| w.hushrun_as(AGENT).args(["run", "unit"]).output().unwrap();
    assert!(run(&w).status.success());
    assert_eq!(ready(&w, &[]).1["checks"][0]["state"], "ci");

    // The plan has room again, and the failure is recorded: it is what a push waits for.
    std::fs::write(w.root.join("red"), "").unwrap();
    assert_eq!(run(&w).status.code(), Some(8));
    let (code, report) = ready(&w, &[]);
    assert_eq!(code, 10, "{report:#}");
    assert_eq!(report["checks"][0]["state"], "missing", "{report:#}");
    assert_eq!(
        report["checks"][0]["why"],
        "it failed on this exact code last time"
    );

    // A pass recorded here takes the mark away with it.
    std::fs::remove_file(w.root.join("red")).unwrap();
    assert!(run(&w).status.success());
    assert_eq!(ready(&w, &[]).0, 0);
    let marks = std::fs::read_dir(w.cache.join("unrecorded"))
        .unwrap()
        .count();
    assert_eq!(marks, 0, "a recorded result leaves no mark");
}

// covers: C-22
#[test]
fn a_check_runs_on_the_content_its_evidence_is_for() {
    let w = world("  unit: {run: \"true\"}");
    let run = |args: &[&str]| {
        w.hushrun_as(AGENT)
            .arg("run")
            .args(args)
            .arg("unit")
            .output()
            .unwrap()
    };
    let checks = || {
        w.keys("v1/events/")
            .iter()
            .filter(|key| key.contains(".check."))
            .count()
    };
    let refused = |out: &std::process::Output, says: &str| {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains(says), "{stderr}");
    };

    // An edit that isn't committed: HEAD's evidence would be about files that never ran.
    w.write("main.go", "package main // edited\n");
    refused(&run(&[]), "isn't as at");
    refused(&run(&[]), "main.go differs");
    assert_eq!(checks(), 0);
    // For the files as they are, it runs.
    assert!(run(&["--worktree"]).status.success());
    assert_eq!(checks(), 1);

    // Another commit, from this checkout: the command `ready` names for it runs once that commit
    // is checked out.
    let first = w.head();
    w.commit("two");
    let (_, report) = ready(&w, &["--rev", &first]);
    let named = format!("hushrun run --rev {first} unit");
    assert_eq!(report["next"], json!([named]), "{report:#}");
    refused(&run(&["--rev", &first]), "isn't as at");
    git::run(&w.repo, &["checkout", "-q", &first], None);
    assert!(run(&["--rev", &first]).status.success());
    assert_eq!(checks(), 2);

    // A file the commit doesn't have, as a CI job's own steps write one: it runs.
    w.write("gha-creds-1.json", "{}\n");
    assert!(run(&["--force"]).status.success());
    assert_eq!(checks(), 3);

    // A digest that isn't this checkout's.
    let other = format!("sha256:{}", "0".repeat(64));
    refused(&run(&["--force", "--digest", &other]), "digests to");
    assert_eq!(checks(), 3);
}

// covers: C-22
#[test]
fn a_stage_or_an_approval_is_refused_with_what_replaced_it() {
    let w = world("  unit: {run: \"true\"}");
    for (args, says) in [
        (
            &["ready", "--stage", "production"][..],
            "hushrun 0.9 has no stages: drop `--stage production`",
        ),
        (
            &["run", "--stage", "staging", "unit"][..],
            "hushrun 0.9 has no stages: drop `--stage staging`",
        ),
        (
            &["approve", "--stage", "production"][..],
            "hushrun 0.9 has no approvals",
        ),
    ] {
        let output = w.hushrun_as(AGENT).args(args).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(says), "{args:?}: {stderr}");
    }
    assert!(w.keys("v1/events/").is_empty());
}

// covers: C-22
#[test]
fn a_file_without_deploy_leaves_hushrun_yaml_out() {
    // An older file: its stages are retired, so its checks are no push's business.
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1\nrepository: github.com/acme/shop\n{}\nchecks:\n  unit: {{kind: source, run: \"true\"}}\nstages:\n  production:\n    require: [unit]\n",
            w.ledger_lines()
        ),
    );
    w.commit("older");
    let output = w
        .hushrun_as(AGENT)
        .args(["ready", "--format", "json"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"], json!([]), "{report:#}");
    // One notice names what hushrun 0.9 reads past.
    assert_eq!(
        stderr.matches("ignored since hushrun 0.9.0").count(),
        2,
        "{stderr}"
    );
}
