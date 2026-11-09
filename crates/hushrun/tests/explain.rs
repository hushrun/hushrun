//! \[ADR 0031 amendments 1 and 2\] `hushrun policy explain` asks the repository's signers answer
//! (L-13) whether the repository is CI-only (T-16), or reads a saved one (`--signers`), and shows
//! what hushrun.yaml requires without it, saying so, when there is no server to ask or it doesn't
//! answer. \[ADR 0062\] One pipeline: it shows `deploy` and the checks it requires, with no stage.

use hushrun_core::time::Time;
use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use serde_json::{Value, json};

/// The full `deploy` example: requires unit, lint, e2e and sast.
const POLICY: &str = "deploy.yaml";

fn json_of(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

/// Runs `cmd`, which must exit 0: (stdout, stderr).
fn ok(cmd: &mut assert_cmd::Command) -> (String, String) {
    let output = cmd.output().unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    );
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );
    (stdout, stderr)
}

fn t(text: &str) -> Time {
    Time::parse(text).unwrap()
}

fn explain(w: &World, extra: &[&str]) -> (String, String) {
    ok(w.hushrun().args(["policy", "explain"]).args(extra))
}

fn explain_json(w: &World, extra: &[&str]) -> Value {
    let (stdout, _) = explain(w, &[&["--format", "json"], extra].concat());
    let value = json_of(stdout.as_bytes());
    schema::assert_valid(&schema::load("policy-explain.v2.schema.json"), &value);
    value
}

fn accepts(value: &Value, requirement: &str) -> Value {
    value["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["requirement"] == requirement)
        .unwrap_or_else(|| panic!("{requirement} in {value}"))["accepts"]
        .clone()
}

fn requirements(value: &Value) -> Vec<String> {
    value["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["requirement"].as_str().unwrap().to_string())
        .collect()
}

/// A URL where nothing listens.
fn nowhere() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}/l/acme/shop")
}

// covers: C-7
#[test]
fn explain_shows_deploy_and_the_checks_it_requires() {
    let w = World::new(POLICY);
    let (text, _) = explain(&w, &[]);
    assert!(text.starts_with("deploys from main\n"), "{text}");
    assert!(text.contains("required checks:\n"), "{text}");
    for check in ["unit", "lint", "e2e", "sast"] {
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with(check) && l.contains(" from ")),
            "{check}: {text}"
        );
    }
    assert!(
        text.contains("lease 1h, dedupe on push, force disabled\n"),
        "{text}"
    );
    let value = explain_json(&w, &[]);
    assert_eq!(
        value["deploy"],
        json!({
            "branch": "main",
            "lease": "1h",
            "dedupe": ["push"],
            "allowForce": false,
            "onRepositoryError": "fail",
        }),
        "{value}"
    );
    // `deploy.require`, in its order, with neither a kind nor a producer.
    assert_eq!(requirements(&value), ["unit", "lint", "e2e", "sast"]);
    let unit = &value["requirements"][0];
    assert_eq!(unit["maxAge"], "7d", "{value}");
    assert!(
        unit["definitionDigest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(unit.get("kind").is_none() && unit.get("producedBy").is_none());
    // One location an artifact.
    assert_eq!(
        value["artifacts"],
        json!([
            {"artifact": "api-image", "scope": "api", "location": "ghcr.io/acme/api"},
            {"artifact": "web-image", "scope": "web", "location": "ghcr.io/acme/web"},
        ])
    );

    // Without `deploy`: every check with a `run`, and no deploy settings.
    let w = World::new(POLICY);
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let (kept, _) = policy.split_once("deploy:\n").unwrap();
    w.write("hushrun.yaml", kept);
    w.commit("no deploy");
    let (text, _) = explain(&w, &[]);
    assert!(
        text.starts_with(
            "hushrun.yaml has no `deploy`: these are the checks `hushrun run --all` runs\n"
        ),
        "{text}"
    );
    assert!(!text.contains("lease "), "{text}");
    let value = explain_json(&w, &[]);
    assert_eq!(value["deploy"], Value::Null);
    assert_eq!(requirements(&value), ["e2e", "lint", "unit"], "{value}");
}

// covers: C-7
#[test]
fn a_file_with_retired_keys_is_explained_without_them() {
    // 0.8's stages, kinds and per-stage locations: named once, and read past.
    let w = World::new(POLICY);
    let ledger = w.ledger_lines();
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/acme/shop
{ledger}
checks:
  unit:
    kind: source
    run: go test ./...
  smoke:
    kind: deployment
    run: ./smoke.sh
artifacts:
  image:
    type: oci
    locations:
      production: ghcr.io/acme/prod/shop
stages:
  production:
    require: [unit, smoke@production]
"
        ),
    );
    w.commit("an 0.8 policy");
    let (text, stderr) = explain(&w, &[]);
    for key in [
        "`stages`",
        "`checks.unit.kind`",
        "`artifacts.image.locations`",
    ] {
        assert_eq!(
            stderr
                .lines()
                .filter(|l| l.contains(key) && l.contains("ignored since hushrun 0.9.0"))
                .count(),
            1,
            "{key}: {stderr}"
        );
    }
    assert!(text.starts_with("hushrun.yaml has no `deploy`"), "{text}");
    let value = explain_json(&w, &[]);
    assert_eq!(requirements(&value), ["smoke", "unit"], "{value}");
    assert_eq!(value["deploy"], Value::Null);
}

// covers: T-16, L-13
#[test]
fn a_repository_ci_only_now_counts_only_the_ci() {
    let w = World::new(POLICY);
    w.fake
        .add_repository_ci_only_period(t("2026-10-01T09:00:00Z"), Some(t("2026-10-05T10:00:00Z")));
    w.fake.set_repository_ci_only(t("2026-10-15T12:30:00Z"));
    let (text, _) = explain(&w, &[]);
    assert!(
        text.contains(
            "CI only since 2026-10-15T12:30:00.000Z, as the repository's server says; earlier from 2026-10-01T09:00:00.000Z until 2026-10-05T10:00:00.000Z, and events made then count only from CI\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("events count from the repository's CI only\n"),
        "{text}"
    );
    for check in ["unit", "sast"] {
        assert!(
            text.lines()
                .any(|l| l.contains(check) && l.ends_with("from the repository's CI only")),
            "{check}: {text}"
        );
    }
    assert!(!text.contains("members' machines, agents and"), "{text}");
    assert_eq!(text.matches("required checks:").count(), 1, "{text}");

    let value = explain_json(&w, &[]);
    let ci_only = &value["ciOnly"];
    assert_eq!(ci_only["checked"], true, "{value}");
    assert_eq!(ci_only["from"], "server");
    assert_eq!(ci_only["source"], w.fake.url());
    assert_eq!(ci_only["statementChecked"], true);
    assert_eq!(ci_only["now"], true);
    assert_eq!(ci_only["since"], "2026-10-15T12:30:00.000Z");
    assert_eq!(ci_only["periods"].as_array().unwrap().len(), 2, "{value}");
    for check in ["unit", "lint", "e2e", "sast"] {
        assert_eq!(accepts(&value, check), "ci", "{check}");
    }

    // A 0.8 server still answers per stage: any stage's periods are the repository's, merged.
    let w = World::new(POLICY);
    w.fake.add_ci_only_period(
        "production",
        t("2026-10-01T09:00:00Z"),
        Some(t("2026-10-05T10:00:00Z")),
    );
    w.fake.set_ci_only("staging", t("2026-10-15T12:30:00Z"));
    let value = explain_json(&w, &[]);
    assert_eq!(value["ciOnly"]["since"], "2026-10-15T12:30:00.000Z");
    assert_eq!(
        value["ciOnly"]["periods"],
        json!([
            {"from": "2026-10-01T09:00:00.000Z", "until": "2026-10-05T10:00:00.000Z"},
            {"from": "2026-10-15T12:30:00.000Z"},
        ]),
        "{value}"
    );
    assert_eq!(accepts(&value, "unit"), "ci");
}

// covers: T-16, L-13
#[test]
fn a_lifted_rule_counts_everyone_and_shows_its_history() {
    let w = World::new(POLICY);
    w.fake
        .add_repository_ci_only_period(t("2026-10-01T09:00:00Z"), Some(t("2026-10-05T10:00:00Z")));
    w.fake
        .add_repository_ci_only_period(t("2026-10-10T09:00:00Z"), Some(t("2026-10-12T10:00:00Z")));
    let (text, _) = explain(&w, &[]);
    assert!(
        text.contains(
            "not CI only now, as the repository's server says; earlier from 2026-10-01T09:00:00.000Z until 2026-10-05T10:00:00.000Z, from 2026-10-10T09:00:00.000Z until 2026-10-12T10:00:00.000Z, and events made then count only from CI"
        ),
        "{text}"
    );
    assert!(
        text.contains("events count from members' machines, agents and the repository's CI\n"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("unit") && l.ends_with("from members' machines, agents and CI")),
        "{text}"
    );
    let value = explain_json(&w, &[]);
    assert_eq!(value["ciOnly"]["checked"], true);
    assert_eq!(value["ciOnly"]["now"], false);
    assert_eq!(value["ciOnly"]["since"], Value::Null);
    assert_eq!(
        value["ciOnly"]["periods"][1],
        json!({"from": "2026-10-10T09:00:00.000Z", "until": "2026-10-12T10:00:00.000Z"})
    );
    assert_eq!(accepts(&value, "unit"), "signers");

    // A repository that never was CI-only.
    let w = World::new(POLICY);
    let (text, _) = explain(&w, &[]);
    assert!(
        text.contains("not CI only, as the repository's server says\n"),
        "{text}"
    );
    assert_eq!(explain_json(&w, &[])["ciOnly"]["periods"], json!([]));
}

// covers: T-16, L-5
#[test]
fn without_a_server_explain_says_ci_only_was_not_checked() {
    let w = World::new(POLICY);
    w.fake.set_repository_ci_only(t("2026-10-15T12:30:00Z"));
    let url = nowhere();
    let (text, _) = explain(&w, &["--repository-url", &url]);
    let unchecked: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("whether the repository is CI only wasn't checked: "))
        .collect();
    assert_eq!(unchecked.len(), 1, "{text}");
    assert!(unchecked[0].contains(&url), "{text}");
    assert!(
        text.contains("events count from members' machines, agents and the repository's CI\n"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("unit") && l.ends_with("from the signers")),
        "{text}"
    );
    let value = explain_json(&w, &["--repository-url", &url]);
    assert_eq!(value["ciOnly"]["checked"], false, "{value}");
    assert!(
        value["ciOnly"]["reason"]
            .as_str()
            .unwrap()
            .contains(&url[..16])
    );
    assert_eq!(accepts(&value, "unit"), "signers");

    // No URL at all: hushrun.yaml names none, nor the environment.
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let policy: String = policy
        .lines()
        .filter(|l| !l.starts_with("repository_url:"))
        .map(|l| format!("{l}\n"))
        .collect();
    w.write("hushrun.yaml", &policy);
    w.commit("no repository URL");
    let (text, _) = ok(w
        .hushrun()
        .env_remove("HUSHRUN_REPOSITORY_URL")
        .args(["policy", "explain"]));
    assert!(
        text.contains("whether the repository is CI only wasn't checked: no repository URL"),
        "{text}"
    );
}

// covers: T-9, L-13
#[test]
fn an_unpinned_log_key_warns_and_uses_the_answer() {
    let w = World::without_log_key(POLICY);
    w.fake.set_repository_ci_only(t("2026-10-15T12:30:00Z"));
    let (text, stderr) = explain(&w, &[]);
    assert!(
        text.contains("CI only since 2026-10-15T12:30:00.000Z"),
        "{text}"
    );
    assert!(stderr.contains("transparency_log"), "{stderr}");
    let value = explain_json(&w, &[]);
    assert_eq!(value["ciOnly"]["checked"], true);
    assert_eq!(value["ciOnly"]["statementChecked"], false);
}

// covers: T-16, L-13
#[test]
fn a_saved_answer_tells_offline() {
    let w = World::new(POLICY);
    w.fake.set_repository_ci_only(t("2026-10-15T12:30:00Z"));
    let saved = w.path("signers.json");
    ok(w.hushrun()
        .args(["repository", "signers", "--out"])
        .arg(&saved));
    let file = saved.to_str().unwrap();
    // Nothing listens at the URL: the file answers.
    let url = nowhere();
    let (text, _) = explain(&w, &["--repository-url", &url, "--signers", file]);
    assert!(
        text.contains(&format!(
            "CI only since 2026-10-15T12:30:00.000Z, as {file} says"
        )),
        "{text}"
    );
    let value = explain_json(&w, &["--repository-url", &url, "--signers", file]);
    assert_eq!(value["ciOnly"]["from"], "file");
    assert_eq!(value["ciOnly"]["source"], file);
    assert_eq!(value["ciOnly"]["statementChecked"], true);
    assert_eq!(accepts(&value, "unit"), "ci");

    // A changed file doesn't verify against the pinned key: exit 2, as `verify --signers`.
    let mut answer = json_of(&std::fs::read(&saved).unwrap());
    answer["signers"]["ci_only"] = json!([]);
    std::fs::write(&saved, serde_json::to_vec(&answer).unwrap()).unwrap();
    w.hushrun()
        .args(["policy", "explain", "--signers", file])
        .assert()
        .code(2);
}
