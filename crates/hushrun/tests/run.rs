//! `hushrun run`, end to end (M6): what it runs, with and without a plan, and what it records.

use std::time::Instant;

use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use serde_json::Value;

const CI: u8 = 1;

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

const LINT_IMAGE: &str = "golangci/golangci-lint@sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn world() -> World {
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/Acme/api
{}
checks:
  ok: {{run: \"echo running ok\"}}
  bad: {{run: \"exit 3\", retries: 1}}
  slow: {{run: \"sleep 5; exit 0\", timeout: 1s}}
  manual: {{}}
  lint: {{run: \"true\", image: \"{LINT_IMAGE}\", platform: linux/amd64}}
deploy:
  branch: develop
  require: [ok, manual]
",
            w.ledger_lines()
        ),
    );
    w.commit("checks");
    w
}

fn run(w: &World, args: &[&str]) -> (i32, Value, String) {
    let output = w
        .hushrun_as(CI)
        .env("HUSHRUN_RUN_ID", "r1")
        .args(["run", "--format", "json"])
        .args(args)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let value = if output.stdout.is_empty() {
        Value::Null
    } else {
        json(&output.stdout)
    };
    (output.status.code().unwrap_or(-1), value, stderr)
}

fn results(value: &Value) -> Vec<(String, String)> {
    value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["check"].as_str().unwrap().to_string(),
                c["result"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn runs_what_lacks_evidence_and_skips_the_rest() {
    let w = world();
    // `manual` has no `run`: `--all` would include it, so name `ok`.
    let (code, out, stderr) = run(&w, &["ok"]);
    assert_eq!(code, 0, "{stderr}");
    schema::assert_valid(&schema::load("run.v2.schema.json"), &out);
    assert_eq!(results(&out), [("ok".into(), "pass".into())]);
    // The check's own output goes to stderr; stdout stays JSON.
    assert!(stderr.contains("running ok"), "{stderr}");
    let key = out["checks"][0]["event"].as_str().unwrap().to_string();
    // No stage: `_` holds its place in the key (ADR 0062).
    assert!(key.ends_with(".check._.json"), "{key}");
    let shown = json(
        &w.hushrun()
            .args(["repository", "show", &key, "--format", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    let check = &shown["statement"]["predicate"]["check"];
    assert_eq!(check["result"], "pass");
    assert_eq!(check["exitCode"], 0);
    assert_eq!(check["attempts"], 1);
    // Valid evidence: skipped, naming what satisfied it; --force runs it again.
    let (code, out, _) = run(&w, &["ok"]);
    assert_eq!(code, 0);
    schema::assert_valid(&schema::load("run.v2.schema.json"), &out);
    assert_eq!(results(&out), [("ok".into(), "skipped".into())]);
    assert!(out["checks"][0]["detail"].as_str().unwrap().contains(&key));
    let (_, out, _) = run(&w, &["ok", "--force"]);
    assert_eq!(results(&out), [("ok".into(), "pass".into())]);
}

#[test]
fn failures_are_recorded_retried_and_exit_8() {
    let w = world();
    let (code, out, _) = run(&w, &["bad"]);
    assert_eq!(code, 8);
    let check = &out["checks"][0];
    assert_eq!(
        (
            check["result"].as_str(),
            check["exitCode"].as_i64(),
            check["attempts"].as_u64()
        ),
        (Some("fail"), Some(3), Some(2))
    );
    // A timeout kills the check, with every process it started, and records a failure. The
    // shell runs `sleep` as its child, everywhere: killing the shell alone would leave it holding
    // hushrun's output open for 5 s.
    let started = Instant::now();
    let (code, out, stderr) = run(&w, &["slow"]);
    assert_eq!(code, 8, "{stderr}");
    assert!(started.elapsed().as_secs() < 4);
    assert_eq!(out["checks"][0]["exitCode"], Value::Null);
}

// covers: C-7
#[test]
fn a_check_without_run_is_attested_or_given_a_command() {
    let w = world();
    // `--all` is what `deploy` requires; `manual` has no `run`, so nothing runs.
    let (code, out, stderr) = run(&w, &["--all"]);
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(out, Value::Null);
    assert!(stderr.contains("`manual` has no `run`"), "{stderr}");
    assert!(
        stderr.contains("hushrun attest --check manual=pass"),
        "{stderr}"
    );
    assert!(w.keys("v1/events/").is_empty());
    // A command after -- is `hushrun run NAME -- CMD`'s: hushrun.yaml's flags don't go with it.
    let (code, _, stderr) = run(&w, &["ok", "--isolated", "--", "true"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("--isolated goes with hushrun.yaml's checks"),
        "{stderr}"
    );
}

#[test]
fn with_a_plan_it_runs_exactly_the_plans_checks() {
    let w = world();
    let plan = w.path("plan.json");
    let output = w
        .hushrun_as(CI)
        .env("HUSHRUN_RUN_ID", "r1")
        .args(["plan", "--out"])
        .arg(&plan)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The plan runs `ok` and `manual`; `manual` has no `run`, so name `ok` alone…
    let (code, out, stderr) = run(&w, &["--plan", plan.to_str().unwrap(), "ok"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(results(&out), [("ok".into(), "pass".into())]);
    // …and a check outside the plan is refused.
    let (code, _, stderr) = run(&w, &["--plan", plan.to_str().unwrap(), "bad"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("`bad` isn't in the plan's checks_to_run"),
        "{stderr}"
    );
    // With no names, every check of the plan runs: `manual` has no `run`, so nothing does.
    let (code, _, stderr) = run(&w, &["--plan", plan.to_str().unwrap()]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("`manual` has no `run`"), "{stderr}");
}

// covers: E-7, P-1
#[test]
fn pinned_checks_run_only_in_their_image() {
    let w = world();
    let (code, _, stderr) = run(&w, &["lint"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("--environment-image"), "{stderr}");
    let other = LINT_IMAGE.replace("1111", "2222");
    let (code, _, stderr) = run(&w, &["lint", "--environment-image", &other]);
    assert_eq!(code, 2);
    assert!(stderr.contains("would never count"), "{stderr}");
    let (code, out, stderr) = run(&w, &["lint", "--environment-image", LINT_IMAGE]);
    assert_eq!(code, 0, "{stderr}");
    let key = out["checks"][0]["event"].as_str().unwrap().to_string();
    let shown = json(
        &w.hushrun()
            .args(["repository", "show", &key, "--format", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(
        shown["statement"]["predicate"]["check"]["environment"]["image"],
        LINT_IMAGE
    );
    assert_eq!(
        shown["statement"]["predicate"]["check"]["environment"]["platform"],
        "linux/amd64"
    );
}

// covers: E-12
#[test]
fn isolated_runs_need_a_container_runtime() {
    let w = world();
    let output = w
        .hushrun_as(CI)
        .env("HUSHRUN_CONTAINER_RUNTIME", w.path("no-such-runtime"))
        .args(["run", "lint", "--isolated"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("HUSHRUN_CONTAINER_RUNTIME"), "{stderr}");
    assert!(w.keys("v1/events/").is_empty());
}
