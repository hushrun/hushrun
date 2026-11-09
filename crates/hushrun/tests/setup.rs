//! `hushrun init` and `hushrun doctor` (M6). Offline: artifact locations point at a closed
//! local port.

use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use serde_json::Value;

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

// covers: C-15
#[test]
fn init_pipeline_writes_a_valid_policy_without_trust_or_keys() {
    let w = World::new("acme-api.yaml");
    std::fs::remove_file(w.repo.join("hushrun.yaml")).unwrap();
    let init = |extra: &[&str]| {
        let mut cmd = w.hushrun();
        cmd.args(["init", "--pipeline"]).args(extra);
        cmd.output().unwrap()
    };
    // No origin remote: the repository must be given.
    let output = init(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--repository"));
    let output = init(&[
        "--repository",
        "github.com/Acme/App",
        "--branch",
        "main",
        "--image",
        "127.0.0.1:9/acme/app",
        "--format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = json(&output.stdout);
    schema::assert_valid(&schema::load("init.v1.schema.json"), &out);
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    assert!(
        policy.contains("repository: github.com/Acme/App"),
        "{policy}"
    );
    assert!(
        policy.contains("run: go test ./..."),
        "the world has a go.mod: {policy}"
    );
    // ADR 0031: no trust and no keys: members' machines and the repository's CI sign (T-15).
    // Amendment 1: whether the repository takes CI's evidence only is set in the dashboard (T-16).
    assert!(!policy.contains("trust"), "{policy}");
    assert!(!policy.contains("accept"), "{policy}");
    // ADR 0062: one pipeline, one place for the image.
    assert!(policy.contains("deploy:\n  branch: main\n"), "{policy}");
    assert!(
        policy.contains("location: 127.0.0.1:9/acme/app"),
        "{policy}"
    );
    assert!(!policy.contains("stage"), "{policy}");
    w.commit("init");
    w.hushrun().args(["policy", "validate"]).assert().success();
    // Again: refused without --force.
    assert_eq!(
        init(&["--repository", "github.com/Acme/App"]).status.code(),
        Some(2)
    );
    assert!(
        init(&["--repository", "github.com/Acme/App", "--force"])
            .status
            .success()
    );
}

/// A world whose artifacts live on a closed local port.
/// Without hushrun.yaml, a pipeline command names the init that writes one: `hushrun init` alone
/// sets up `hushrun run` (C-15).
#[test]
fn without_hushrun_yaml_the_pipeline_points_at_init_pipeline() {
    let w = World::new("acme-api.yaml");
    std::fs::remove_file(w.repo.join("hushrun.yaml")).unwrap();
    w.commit("no hushrun.yaml");
    let output = w.hushrun().args(["policy", "explain"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no hushrun.yaml in this revision; `hushrun init --pipeline` writes"),
        "{stderr}"
    );
}

fn doctor_world() -> World {
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/Acme/api
{}
checks:
  unit: {{run: \"true\"}}
artifacts:
  image:
    type: oci
    location: 127.0.0.1:9/acme/app
deploy:
  branch: main
  require: [unit]
",
            w.ledger_lines()
        ),
    );
    w.commit("doctor policy");
    w
}

fn statuses(out: &Value) -> Vec<(String, String)> {
    out["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["check"].as_str().unwrap().to_string(),
                c["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn status_of(out: &Value, check: &str) -> String {
    statuses(out)
        .into_iter()
        .find(|(c, _)| c == check)
        .map_or_else(|| panic!("no {check} in {out}"), |(_, s)| s)
}

// covers: A-7, D-7, T-15
#[test]
fn doctor_checks_the_setup() {
    let w = doctor_world();
    let doctor = |key: Option<u8>| {
        let mut cmd = match key {
            Some(n) => w.hushrun_as(n),
            None => w.hushrun(),
        };
        let output = cmd
            .args([
                "doctor",
                "--format",
                "json",
                "--insecure-registry",
                "127.0.0.1:9",
            ])
            .output()
            .unwrap();
        (output.status.code(), json(&output.stdout))
    };
    let (code, out) = doctor(Some(1));
    assert_eq!(code, Some(0), "{out}");
    schema::assert_valid(&schema::load("doctor.v1.schema.json"), &out);
    for (check, expected) in [
        ("git", "ok"),
        ("hushrun.yaml", "ok"),
        ("server", "ok"),
        ("signers", "warning"),
        ("sigstore trust root", "ok"),
        ("signer", "ok"),
        ("timestamps", "warning"),
        ("clock", "ok"),
        ("registry", "warning"),
        ("deploy workflow", "skipped"),
        ("baked configuration", "ok"),
    ] {
        assert_eq!(status_of(&out, check), expected, "{check}: {out}");
    }
    // A key no member's machine holds would sign events that don't count (T-15).
    let (code, out) = doctor(Some(5));
    assert_eq!(code, Some(2));
    assert_eq!(status_of(&out, "signer"), "error");

    // A deploy workflow outside the scope, and a build-time variable.
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write(
        "hushrun.yaml",
        &format!("{policy}digest:\n  exclude: [\".github/**\"]\n"),
    );
    w.write(
        ".github/workflows/deploy.yml",
        "jobs:\n  plan:\n    steps:\n      - run: hushrun plan --start\n",
    );
    w.write(
        "web/config.ts",
        "export const api = process.env.NEXT_PUBLIC_API_URL;\n",
    );
    w.commit("workflow and web");
    let (_, out) = doctor(None);
    assert_eq!(status_of(&out, "deploy workflow"), "warning", "{out}");
    assert_eq!(status_of(&out, "baked configuration"), "warning");
}
