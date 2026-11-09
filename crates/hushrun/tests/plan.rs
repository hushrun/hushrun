//! `plan`, the plan as the single source, `guard` and `status`, end to end (M3). \[ADR 0062\] One
//! pipeline: a plan decides from earlier attempts of the same digest, and waits for none.

use hushrun_testkit::git;
use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use predicates::prelude::*;
use serde_json::Value;

const MEMBER: u8 = 1; // a member's machine
const OTHER_MEMBER: u8 = 2;

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

/// An image at the artifact's location, pinned by digest.
fn image() -> String {
    format!(
        "us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        "a".repeat(64)
    )
}

/// Run `run_id` plans, runs its checks, builds, deploys and passes, as one run.
fn passes(w: &World, run_id: &str) {
    let plan = w.path(&format!("plan-{run_id}.json"));
    let as_run = || {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.env("HUSHRUN_RUN_ID", run_id);
        cmd
    };
    let out = json(&ok(as_run()
        .args(["plan", "--start", "--format", "json", "--out"])
        .arg(&plan)));
    assert_eq!(out["scopes"][0]["mode"], "full", "{out}");
    let carry = || {
        let mut cmd = as_run();
        cmd.arg("--plan").arg(&plan);
        cmd
    };
    ok(carry().args([
        "attest",
        "--check",
        "unit=pass",
        "--check",
        "toolchain=pass",
    ]));
    ok(carry().args(["attest", "--artifact", &format!("image={}", image())]));
    ok(carry().args(["mark", "deployed"]));
    ok(carry().args(["mark", "passed"]));
}

// covers: P-1, P-10, P-13, P-14, L-8
#[test]
fn a_passed_attempts_image_is_promoted_by_digest() {
    let w = World::new("acme-api.yaml");
    passes(&w, "s1");
    let out = json(&ok(w
        .hushrun_as(OTHER_MEMBER)
        .env("HUSHRUN_RUN_ID", "p1")
        .args(["plan", "--format", "json"])));
    schema::assert_valid(&schema::load("plan.v2.schema.json"), &out);
    assert_eq!(out["schema"], "hushrun.plan/v2");
    let decision = &out["scopes"][0];
    assert_eq!(decision["mode"], "promote", "{out}");
    assert_eq!(decision["build"], false);
    assert_eq!(decision["artifacts"]["image"]["ref"], image());
    // [ADR 0062] No upstream, nothing waived, no deployment checks.
    for gone in ["upstream", "waived", "deploymentChecks"] {
        assert!(decision.get(gone).is_none(), "{gone}: {decision}");
    }
    assert!(out.get("stage").is_none(), "{out}");
    assert_eq!(out["run"]["id"], "p1");
    assert_eq!(out["revision"], w.head());

    // The env outputs of §10.2.
    let env = String::from_utf8(ok(w.hushrun().args(["plan", "--format", "env"]))).unwrap();
    for key in ["MODE=promote", "BUILD=false", "CHECKS=''", "ATTEMPT=1"] {
        assert!(env.contains(&format!("HUSHRUN_{key}\n")), "{key} in {env}");
    }
    assert!(!env.contains("HUSHRUN_DEPLOYMENT_CHECKS"), "{env}");
    assert!(env.contains(&format!("HUSHRUN_REF={}\n", image())), "{env}");
    assert!(env.contains("HUSHRUN_JSON="), "{env}");
    assert!(env.contains(r#"HUSHRUN_ARTIFACTS='{"image":"#), "{env}");
    // A push of the digest the repository runs is current: nothing to deploy (P-10).
    let current = json(&ok(w.hushrun().args([
        "plan",
        "--trigger",
        "push",
        "--format",
        "json",
    ])));
    assert_eq!(current["scopes"][0]["mode"], "current", "{current}");
}

// covers: P-6, P-9, P-15, C-7
#[test]
fn an_earlier_failure_runs_again_and_only_a_refused_force_blocks() {
    let w = World::new("acme-api.yaml");
    let plan = w.path("plan-s1.json");
    let as_run = || {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.env("HUSHRUN_RUN_ID", "s1");
        cmd
    };
    ok(as_run().args(["plan", "--start", "--out"]).arg(&plan));
    ok(as_run().arg("--plan").arg(&plan).args([
        "attest",
        "--check",
        "unit=fail",
        "--check",
        "toolchain=pass",
    ]));
    ok(as_run()
        .arg("--plan")
        .arg(&plan)
        .args(["mark", "failed", "--reason", "unit failed"]));
    // [ADR 0062] A failed attempt blocks nothing: the next run builds, and runs the check that
    // failed again, but not the one that passed.
    let (code, stdout, stderr) = run(w.hushrun().args(["plan", "--format", "json"]));
    assert_eq!(code, 0, "{stderr}");
    let out = json(&stdout);
    let decision = &out["scopes"][0];
    assert_eq!(decision["mode"], "full", "{out}");
    assert_eq!(decision["build"], true);
    assert_eq!(
        decision["checksToRun"],
        serde_json::json!([{"check": "unit", "reason": "failed"}]),
        "{decision}"
    );
    assert!(
        decision["reason"]
            .as_str()
            .unwrap()
            .contains("running unit (failed)"),
        "{decision}"
    );
    // Forcing runs every check, with --force-full or HUSHRUN_FORCE_FULL.
    let out = json(&ok(w.hushrun().args([
        "plan",
        "--force-full",
        "--format",
        "json",
    ])));
    assert_eq!(out["scopes"][0]["mode"], "full");
    assert_eq!(out["forced"], true);
    assert_eq!(out["scopes"][0]["checksToRun"].as_array().unwrap().len(), 2);
    let out = json(&ok(w
        .hushrun()
        .env("HUSHRUN_FORCE_FULL", "true")
        .args(["plan", "--format", "json"])));
    assert_eq!(out["forced"], true);
    // `deploy.allow_force: false` refuses it: blocked, exit 3.
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let refusing = policy.replace(
        "deploy:\n  branch: main\n",
        "deploy:\n  branch: main\n  allow_force: false\n",
    );
    assert_ne!(refusing, policy);
    w.write("hushrun.yaml", &refusing);
    w.commit("no forcing");
    let (code, stdout, _) = run(w.hushrun().args(["plan", "--force-full"]));
    assert_eq!(code, 3);
    assert!(
        String::from_utf8_lossy(&stdout)
            .starts_with("blocked: this repository doesn't allow forcing"),
        "{}",
        String::from_utf8_lossy(&stdout)
    );
}

// covers: P-6, P-13, P-14
#[test]
fn a_running_attempt_lends_its_image_only_once_it_passes() {
    let w = World::new("acme-api.yaml");
    let plan = w.path("plan-s1.json");
    let as_run = || {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.env("HUSHRUN_RUN_ID", "s1");
        cmd
    };
    ok(as_run().args(["plan", "--start", "--out"]).arg(&plan));
    ok(as_run().arg("--plan").arg(&plan).args([
        "attest",
        "--artifact",
        &format!("image={}", image()),
    ]));
    // Run s1 is still running: another run of this digest doesn't wait for it, and builds.
    let out = json(&ok(w.hushrun().args(["plan", "--format", "json"])));
    let decision = &out["scopes"][0];
    assert_eq!(decision["mode"], "full", "{out}");
    assert!(
        decision["reason"]
            .as_str()
            .unwrap()
            .contains("recorded ones aren't reusable"),
        "{decision}"
    );
    // [ADR 0062] There is no `wait`, and no flag that waits for it.
    w.hushrun()
        .args(["plan", "--wait-timeout", "1s"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--wait-timeout"));
    // Once s1 passes, its image is reused, and the checks it didn't run run first: refresh.
    ok(as_run().arg("--plan").arg(&plan).args(["mark", "passed"]));
    let out = json(&ok(w.hushrun().args(["plan", "--format", "json"])));
    let decision = &out["scopes"][0];
    assert_eq!(decision["mode"], "refresh", "{out}");
    assert_eq!(decision["build"], false);
    assert_eq!(decision["artifacts"]["image"]["ref"], image());
    let checks: Vec<&str> = decision["checksToRun"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["check"].as_str().unwrap())
        .collect();
    assert_eq!(
        checks,
        ["toolchain", "unit"],
        "in the order deploy requires them"
    );
}

// covers: L-5
#[test]
fn an_unreadable_ledger_fails_closed_unless_deploy_opts_in() {
    let w = World::new("acme-api.yaml");
    let missing = format!("{}/missing", w.ledger_url());
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write("hushrun.yaml", &policy.replace(&w.ledger_url(), &missing));
    w.commit("a URL where nothing answers");
    w.push_default_branch();
    w.hushrun()
        .args(["plan", "--repository-url", &missing])
        .assert()
        .code(6);
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let opted_in = policy.replace(
        "deploy:\n  branch: main\n",
        "deploy:\n  branch: main\n  on_repository_error: full\n",
    );
    assert_ne!(opted_in, policy);
    w.write("hushrun.yaml", &opted_in);
    w.commit("opt in");
    let out = json(&ok(w.hushrun().args([
        "plan",
        "--repository-url",
        &missing,
        "--format",
        "json",
    ])));
    let decision = &out["scopes"][0];
    assert_eq!(decision["mode"], "full");
    assert_eq!(decision["build"], true);
    assert!(
        decision["reason"]
            .as_str()
            .unwrap()
            .contains("on_repository_error: full")
    );
}

#[test]
fn the_plan_is_the_single_source() {
    let w = World::new("acme-api.yaml");
    let plan = w.path("plan.json");
    ok(w.hushrun_as(MEMBER)
        .env("HUSHRUN_RUN_ID", "s1")
        .args(["plan", "--force-full", "--out"])
        .arg(&plan));
    // Events of a forced run carry `forced: true`; the run identity comes from the plan.
    let out = json(&ok(w.hushrun_as(MEMBER).arg("--plan").arg(&plan).args([
        "attest",
        "--check",
        "unit=pass",
        "--format",
        "json",
    ])));
    let key = out["events"][0]["keys"][0].as_str().unwrap();
    let verify = json(&ok(w.hushrun().args([
        "repository",
        "show",
        key,
        "--format",
        "json",
    ])));
    assert_eq!(verify["statement"]["predicate"]["forced"], true);
    assert_eq!(verify["statement"]["predicate"]["run"]["id"], "s1");
    assert!(
        verify["statement"]["predicate"].get("stage").is_none(),
        "{verify}"
    );
    // HUSHRUN_PLAN_JSON carries the same plan.
    let text = std::fs::read_to_string(&plan).unwrap();
    ok(w.hushrun_as(MEMBER).env("HUSHRUN_PLAN_JSON", &text).args([
        "attest",
        "--check",
        "toolchain=pass",
    ]));
    // A plan made for another revision is refused, by every command that carries one out.
    w.write("main.go", "package main // changed\n");
    w.commit("moved on");
    for args in [
        &["attest", "--check", "unit=pass"][..],
        &["mark", "passed"][..],
    ] {
        w.hushrun_as(MEMBER)
            .arg("--plan")
            .arg(&plan)
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("the plan was made for revision"));
    }
}

#[test]
fn a_plan_is_for_its_content_not_only_its_revision() {
    let w = World::new("acme-api.yaml");
    let plan = w.path("plan.json");
    w.write("main.go", "package main // planned\n");
    ok(w.hushrun_as(MEMBER)
        .env("HUSHRUN_RUN_ID", "w1")
        .args(["plan", "--worktree", "--force-full", "--out"])
        .arg(&plan));
    let attest = |content: &[&str]| {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.arg("--plan")
            .arg(&plan)
            .args(["attest", "--check", "unit=pass"])
            .args(content);
        cmd
    };
    // Edited after the plan: the same revision, other content.
    w.write("main.go", "package main // edited after the plan\n");
    attest(&["--worktree"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("the plan was made for"));
    // Nor is HEAD's content the plan's.
    attest(&[])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("the plan was made for"));
    assert!(w.keys("v1/events/").iter().all(|k| !k.contains(".check.")));
    // As it was planned, it is.
    w.write("main.go", "package main // planned\n");
    ok(&mut attest(&["--worktree"]));
}

// ci-promote's "head commit unreadable -> blocked": no decision, and nothing recorded.
#[test]
fn an_unresolvable_revision_has_no_plan() {
    let w = World::new("acme-api.yaml");
    w.hushrun_as(MEMBER)
        .args(["plan", "--start", "--rev"])
        .arg("0".repeat(40))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("can't be resolved"));
    assert!(w.keys("v1/").is_empty());
}

#[test]
fn a_re_run_with_the_same_run_id_is_the_next_attempt() {
    let w = World::new("acme-api.yaml");
    let plan = |attempt_env: Option<&str>| {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.env("HUSHRUN_RUN_ID", "r9")
            .args(["plan", "--start", "--format", "json"]);
        if let Some(a) = attempt_env {
            cmd.env("HUSHRUN_RUN_ATTEMPT", a);
        }
        json(&ok(&mut cmd))
    };
    assert_eq!(plan(None)["run"]["attempt"], 1);
    assert_eq!(plan(None)["run"]["attempt"], 2);
    assert_eq!(plan(Some("7"))["run"]["attempt"], 7);
}

// covers: P-5, T-16
#[test]
fn only_events_that_count_make_an_earlier_attempt() {
    let w = World::new("acme-api.yaml");
    // The repository takes CI's evidence only: what a member's machine records makes no attempt
    // (P-5), so the run's next plan is still its first attempt.
    w.fake
        .set_repository_ci_only(hushrun_core::time::Time::parse("2026-01-01T00:00:00Z").unwrap());
    let plan = || {
        json(&ok(w
            .hushrun_as(MEMBER)
            .env("HUSHRUN_RUN_ID", "r9")
            .args(["plan", "--start", "--format", "json"])))
    };
    assert_eq!(plan()["run"]["attempt"], 1);
    assert!(!w.keys("v1/events/").is_empty(), "the started is recorded");
    assert_eq!(plan()["run"]["attempt"], 1);
}

// covers: C-4
#[test]
fn several_scopes_produce_actionable_scopes() {
    let w = World::new("deploy.yaml");
    w.hushrun()
        .arg("plan")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--all-scopes"));
    let env = String::from_utf8(ok(w.hushrun().args([
        "plan",
        "--all-scopes",
        "--format",
        "env",
    ])))
    .unwrap();
    let line = env
        .lines()
        .find(|l| l.starts_with("HUSHRUN_ACTIONABLE_SCOPES="))
        .unwrap();
    let value: Value = serde_json::from_str(
        line.trim_start_matches("HUSHRUN_ACTIONABLE_SCOPES=")
            .trim_matches('\''),
    )
    .unwrap();
    let scopes: Vec<&str> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["scope"].as_str().unwrap())
        .collect();
    assert_eq!(scopes, ["api", "web"]);
    assert_eq!(value[0]["mode"], "full");
    assert_eq!(
        value[0]["checks"], "unit lint e2e sast",
        "in the order deploy requires them"
    );
    assert!(
        !env.contains("HUSHRUN_MODE="),
        "no per-scope keys with several scopes"
    );
}

// covers: C-1
#[test]
fn a_file_with_retired_keys_has_no_plan() {
    // [ADR 0062] `stages`, `checks.*.kind` and `artifacts.*.locations` are read past elsewhere,
    // but a deploy that ignored them could skip the gates their author wrote.
    let w = World::new("acme-api.yaml");
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write(
        "hushrun.yaml",
        &format!("{policy}stages:\n  production:\n    branch: main\n    require: [unit]\n"),
    );
    w.commit("a stage hushrun 0.8 read");
    let (code, stdout, stderr) = run(w.hushrun_as(MEMBER).args(["plan", "--start"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stdout.is_empty(), "{}", String::from_utf8_lossy(&stdout));
    for key in ["`stages`", "deploy"] {
        assert!(stderr.contains(key), "{key}: {stderr}");
    }
    assert!(w.keys("v1/").is_empty(), "nothing recorded");
}

#[test]
fn explain_json_dumps_the_whole_input() {
    let w = World::new("acme-api.yaml");
    passes(&w, "s1");
    let dump = w.path("explain.json");
    ok(w.hushrun().args(["plan", "--explain-json"]).arg(&dump));
    let value = json(&std::fs::read(&dump).unwrap());
    assert_eq!(value["schema"], "hushrun.plan.explain/v2");
    assert!(
        value["policy"]
            .as_str()
            .unwrap()
            .contains("repository: github.com/Acme/api")
    );
    // ADR 0031: the signers the server named, which decided which events count.
    assert_eq!(value["signers"]["schema"], "hushrun.signers/v1", "{value}");
    let input = &value["inputs"][0];
    assert!(input.get("stage").is_none(), "{input}");
    assert!(input["events"].as_array().unwrap().len() >= 6);
    assert_eq!(value["decisions"][0]["mode"], "promote");
}

// covers: L-2, E-4
#[test]
fn plan_start_uploads_the_policy_it_evaluated() {
    let w = World::new("acme-api.yaml");
    let start = || {
        let mut cmd = w.hushrun_as(MEMBER);
        cmd.args(["plan", "--start"]);
        ok(&mut cmd)
    };
    start();
    let policy = std::fs::read(w.repo.join("hushrun.yaml")).unwrap();
    let digest = hushrun_core::digest::Digest::of(&policy);
    let key = hushrun_ledger::layout::policy_key(&digest);
    assert_eq!(w.keys("v1/policies/"), std::slice::from_ref(&key));
    assert_eq!(w.object(&key), policy, "the exact bytes");
    // Its name is the policyDigest of the events this plan recorded (E-4).
    let started = w.keys("v1/events/").pop().unwrap();
    assert!(started.ends_with(".started._.json"), "{started}");
    let object = hushrun_attest::Object::parse(&w.object(&started)).unwrap();
    let statement = json(&object.envelope().unwrap().payload_bytes().unwrap());
    assert_eq!(statement["predicate"]["policyDigest"], digest.to_string());
    // Planning again with the same policy finds it there; a new policy is a new key.
    start();
    assert_eq!(w.keys("v1/policies/").len(), 1);
    let text = String::from_utf8(policy).unwrap();
    w.write(
        "hushrun.yaml",
        &text.replace("timeout: 20m", "timeout: 25m"),
    );
    w.commit("a longer unit timeout");
    start();
    assert_eq!(w.keys("v1/policies/").len(), 2);
    // A plan that records nothing uploads nothing: one without --start.
    ok(w.hushrun_as(OTHER_MEMBER).arg("plan"));
    assert_eq!(w.keys("v1/policies/").len(), 2);
}

#[test]
fn guard_passes_at_the_head_and_records_superseded_when_stale() {
    let w = World::new("acme-api.yaml");
    // origin's main, the branch hushrun.yaml deploys from (`deploy.branch`), is at this head.
    let head = w.head();
    let env =
        String::from_utf8(ok(w.hushrun_as(MEMBER).args(["guard", "--format", "env"]))).unwrap();
    assert!(env.contains("HUSHRUN_STALE=false"), "{env}");
    assert!(w.keys("v1/").is_empty(), "nothing recorded at the head");

    // main moves on; this checkout stays at the old head.
    w.write("main.go", "package main // newer\n");
    let newer = w.commit("newer");
    w.push_default_branch();
    git::run(&w.repo, &["checkout", "-q", "--detach", &head], None);
    let env = String::from_utf8(ok(w
        .hushrun_as(MEMBER)
        .env("HUSHRUN_RUN_ID", "s1")
        .args(["guard", "--format", "env"])))
    .unwrap();
    assert!(env.contains("HUSHRUN_STALE=true"), "{env}");
    assert!(env.contains(&format!("HUSHRUN_HEAD={newer}")), "{env}");
    let superseded: Vec<String> = w
        .keys("v1/events/")
        .into_iter()
        .filter(|k| k.contains(".superseded."))
        .collect();
    assert_eq!(superseded.len(), 1);
    let shown = json(&ok(w.hushrun().args([
        "repository",
        "show",
        &superseded[0],
        "--format",
        "json",
    ])));
    assert_eq!(
        shown["statement"]["predicate"]["superseded"]["reason"],
        "stale"
    );
    assert_eq!(
        shown["statement"]["predicate"]["superseded"]["byRevision"],
        newer
    );
    // --on-stale fail exits 4.
    w.hushrun_as(MEMBER)
        .args(["guard", "--on-stale", "fail"])
        .assert()
        .code(4);
}

#[test]
fn guard_exits_1_when_the_head_is_unknown() {
    let w = World::new("acme-api.yaml");
    w.hushrun_as(MEMBER)
        .args(["guard", "--branch", "release"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("nothing recorded"));
    assert!(w.keys("v1/").is_empty());
}

#[test]
fn status_shows_the_pipeline_of_each_scope() {
    let w = World::new("acme-api.yaml");
    passes(&w, "s1");
    let out = json(&ok(w.hushrun().args(["status", "--format", "json"])));
    assert_eq!(out["schema"], "hushrun.status/v2");
    let scope = &out["scopes"][0];
    assert!(scope.get("stages").is_none(), "{scope}");
    assert_eq!(scope["runsThisDigest"], true);
    assert_eq!(scope["deployed"]["digest"], scope["digest"]);
    assert_eq!(scope["mode"], "promote");
    let statuses: Vec<&str> = scope["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["satisfied", "satisfied"], "{scope}");
    let text = String::from_utf8(ok(w.hushrun().arg("status"))).unwrap();
    assert!(
        text.contains("promote") && text.contains("runs this digest"),
        "{text}"
    );
}
