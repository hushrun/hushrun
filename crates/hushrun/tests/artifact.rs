//! `hushrun artifact resolve | copy | digest`, without a registry (M5): every case here either
//! needs no registry or fails to reach one on a closed port. Copies run against real registries
//! in `artifact_registries.rs` (`--features it`). \[ADR 0062\] One pipeline: `resolve` reuses an
//! artifact an earlier passed attempt recorded at the artifact's one `location`, re-verified,
//! and never copies it anywhere.

use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use serde_json::Value;

const WRITER: u8 = 1; // the run that builds and deploys
const READER: u8 = 2; // a later run of the same digest
/// Nothing listens here: connections are refused at once.
const CLOSED: &str = "127.0.0.1:9";
const LOCATION: &str = "127.0.0.1:9/acme/app";

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

/// A world whose image lives at `LOCATION`, with `deploy` set by `deploy`.
fn world_with(deploy: &str) -> World {
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/acme/app
{ledger}
checks:
  unit: {{}}
artifacts:
  image:
    type: oci
    location: {LOCATION}
deploy:
{deploy}",
            ledger = w.ledger_lines()
        ),
    );
    w.commit("artifact policy");
    w
}

fn world() -> World {
    world_with("  branch: main\n  require: [unit]\n")
}

fn image_at(location: &str, c: char) -> String {
    format!("{location}@sha256:{}", c.to_string().repeat(64))
}

/// One run validates and deploys `image`.
fn deploy_passes(w: &World, image: &str) {
    let writer = || {
        let mut cmd = w.hushrun_as(WRITER);
        cmd.env("HUSHRUN_RUN_ID", "s1");
        cmd
    };
    ok(writer().args(["plan", "--start"]));
    ok(writer().args([
        "attest",
        "--check",
        "unit=pass",
        "--artifact",
        &format!("image={image}"),
    ]));
    ok(writer().args(["mark", "deployed"]));
    ok(writer().args(["mark", "passed"]));
}

/// A later run's plan, saved for `resolve`; a blocked plan exits 3.
fn later_plan(w: &World, extra: &[&str]) -> std::path::PathBuf {
    let plan = w.path("plan.json");
    let output = w
        .hushrun_as(READER)
        .env("HUSHRUN_RUN_ID", "p1")
        .arg("plan")
        .args(extra)
        .arg("--out")
        .arg(&plan)
        .output()
        .unwrap();
    assert!(
        matches!(output.status.code(), Some(0 | 3)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    plan
}

// covers: A-1, P-15
#[test]
fn full_builds_at_the_location_tagged_with_the_revision() {
    let w = world();
    let plan = later_plan(&w, &[]);
    let out = json(&ok(w
        .hushrun()
        .arg("--plan")
        .arg(&plan)
        .args(["artifact", "resolve", "--format", "json"])));
    schema::assert_valid(&schema::load("artifact-resolve.v2.schema.json"), &out);
    assert_eq!(out["mode"], "full");
    assert_eq!(out["build"], true);
    assert_eq!(
        out["artifacts"]["image"]["ref"],
        format!("{LOCATION}:{}", w.head())
    );
    assert!(out["artifacts"]["image"]["digest"].is_null(), "{out}");
    let env = String::from_utf8(ok(w
        .hushrun()
        .arg("--plan")
        .arg(&plan)
        .args(["artifact", "resolve", "--tag", "v1.2.3", "--format", "env"])))
    .unwrap();
    assert!(env.contains("HUSHRUN_BUILD=true\n"), "{env}");
    assert!(
        env.contains(&format!("HUSHRUN_REF={LOCATION}:v1.2.3\n")),
        "{env}"
    );
    assert!(
        env.contains(&format!("HUSHRUN_REF_IMAGE={LOCATION}:v1.2.3\n")),
        "{env}"
    );
}

// covers: A-1, A-4, P-13
#[test]
fn a_passed_attempts_artifact_deploys_as_is() {
    let w = world();
    let image = image_at(LOCATION, 'a');
    deploy_passes(&w, &image);
    let plan = later_plan(&w, &[]);
    let out = json(&ok(w
        .hushrun()
        .arg("--plan")
        .arg(&plan)
        .args(["artifact", "resolve", "--format", "json"])));
    schema::assert_valid(&schema::load("artifact-resolve.v2.schema.json"), &out);
    assert_eq!(out["mode"], "promote");
    assert_eq!(out["build"], false);
    // The recorded reference, pinned by digest, and nothing copied.
    assert_eq!(out["artifacts"]["image"]["ref"], image);
    assert_eq!(
        out["artifacts"]["image"]["digest"],
        format!("sha256:{}", "a".repeat(64))
    );
    assert!(out["warnings"].as_array().unwrap().is_empty(), "{out}");
    assert!(
        w.keys("v1/events/")
            .iter()
            .filter(|k| k.contains(".artifact."))
            .count()
            == 1,
        "resolve records no artifact event"
    );
}

// covers: P-13
#[test]
fn an_artifact_elsewhere_than_its_location_is_never_recorded() {
    let w = world();
    let output = w
        .hushrun_as(WRITER)
        .args([
            "attest",
            "--check",
            "unit=pass",
            "--artifact",
            &format!("image={}", image_at("127.0.0.1:9/acme/elsewhere", 'a')),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "is not at artifact `image`'s location, {LOCATION}"
        )),
        "{stderr}"
    );
    assert!(w.keys("v1/events/").is_empty(), "nothing was recorded");
}

// covers: A-3, T-16
#[test]
fn an_artifact_that_no_longer_counts_falls_back_to_a_build() {
    let w = world();
    deploy_passes(&w, &image_at(LOCATION, 'a'));
    let plan = later_plan(&w, &[]);
    // Between the plan and the deploy, the repository's admins make it CI-only: the event that
    // justified the reuse, a member's, no longer counts when resolve re-verifies it.
    w.fake
        .set_repository_ci_only(hushrun_core::time::Time::parse("2026-01-01T00:00:00Z").unwrap());
    let resolve = |extra: &[&str]| {
        let mut cmd = w.hushrun_as(READER);
        cmd.arg("--plan")
            .arg(&plan)
            .args(["artifact", "resolve", "--insecure-registry", CLOSED])
            .args(extra);
        cmd.output().unwrap()
    };
    let output = resolve(&["--format", "json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = json(&output.stdout);
    schema::assert_valid(&schema::load("artifact-resolve.v2.schema.json"), &out);
    assert_eq!(out["mode"], "promote");
    assert_eq!(out["build"], true);
    assert_eq!(
        out["artifacts"]["image"]["ref"],
        format!("{LOCATION}:{}", w.head())
    );
    let warning = out["warnings"][0].as_str().unwrap();
    assert!(
        warning.contains("CI only") && warning.ends_with("building instead"),
        "{warning}"
    );
    // --no-fallback turns the same failure into exit 7.
    let output = resolve(&["--no-fallback"]);
    assert_eq!(output.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--no-fallback"));
}

// covers: A-1, P-10
#[test]
fn modes_that_deploy_nothing_are_usage_errors() {
    // A push of the digest the repository already runs: current.
    let w = world();
    deploy_passes(&w, &image_at(LOCATION, 'a'));
    let plan = later_plan(&w, &["--trigger", "push"]);
    let output = w
        .hushrun()
        .arg("--plan")
        .arg(&plan)
        .args(["artifact", "resolve"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is current"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Forcing where `deploy.allow_force` is false: blocked.
    let w = world_with("  require: [unit]\n  allow_force: false\n");
    let plan = later_plan(&w, &["--force-full"]);
    let output = w
        .hushrun()
        .arg("--plan")
        .arg(&plan)
        .args(["artifact", "resolve"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is blocked"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn in_ci_resolve_needs_the_plan() {
    let w = world();
    w.hushrun()
        .env("CI", "true")
        .env("HUSHRUN_RUN_ID", "c1")
        .env("HUSHRUN_REPOSITORY_URL", w.ledger_url())
        .args(["artifact", "resolve"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("HUSHRUN_PLAN_JSON"));
}

// covers: A-1
#[test]
fn locally_resolve_plans_itself_and_refuses_retired_keys() {
    // Without a plan, a local resolve decides now and records nothing.
    let w = world();
    deploy_passes(&w, &image_at(LOCATION, 'a'));
    let before = w.keys("v1/events/");
    let out = json(&ok(w
        .hushrun_as(READER)
        .args(["artifact", "resolve", "--format", "json"])));
    assert_eq!(out["mode"], "promote", "{out}");
    assert_eq!(out["artifacts"]["image"]["ref"], image_at(LOCATION, 'a'));
    assert_eq!(w.keys("v1/events/"), before);
    // A file that still has 0.8's stages: a deploy that ignored them could skip their gates.
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/acme/app
{ledger}
checks:
  unit: {{}}
artifacts:
  image:
    type: oci
    locations:
      production: {LOCATION}
stages:
  production:
    require: [unit]
",
            ledger = w.ledger_lines()
        ),
    );
    w.commit("an 0.8 policy");
    w.hushrun()
        .args(["artifact", "resolve"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("`stages`"))
        .stderr(predicates::str::contains("which hushrun 0.9 doesn't read"));
}

#[test]
fn unreachable_registries_exit_7() {
    let w = world();
    let reference = format!("{CLOSED}/acme/app:v1");
    for args in [
        vec!["artifact", "digest", reference.as_str()],
        vec![
            "artifact",
            "copy",
            reference.as_str(),
            "127.0.0.1:9/acme/other",
        ],
    ] {
        w.hushrun()
            .args(&args)
            .args(["--insecure-registry", CLOSED])
            .assert()
            .code(7);
    }
    // A tag in attest --artifact is resolved through the registry.
    w.hushrun_as(WRITER)
        .args([
            "attest",
            "--artifact",
            &format!("image={LOCATION}:v1"),
            "--insecure-registry",
            CLOSED,
        ])
        .assert()
        .code(7);
    // Malformed references are usage errors.
    w.hushrun()
        .args(["artifact", "digest", "Not A Ref"])
        .assert()
        .code(2);
}
