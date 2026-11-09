//! Every configuration fixture matches its JSON Schema, which editors use.

use hushrun_testkit::schema;

fn yaml_to_json(path: &std::path::Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap();
    serde_saphyr::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn fixtures(dir: &str) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(hushrun_testkit::testdata(dir))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    paths
}

// covers: C-1
#[test]
fn policies_match_the_schema() {
    let schema = schema::load("hushrun.v1.schema.json");
    for path in fixtures("policies") {
        let found = schema::violations(&schema, &yaml_to_json(&path));
        assert!(found.is_empty(), "{}: {found:#?}", path.display());
    }
    // The schema rejects what hushrun rejects at the shape level.
    let bad = serde_json::json!({"version": 2, "repository": "github.com/a/b", "chekcs": {}});
    assert_eq!(schema::violations(&schema, &bad).len(), 2);
    // ADR 0031: no `trust:` section; amendment 1: no `accept` either, since the repository's
    // CI-only rule is set on the server (T-16).
    let trust = serde_json::json!({"version": 1, "repository": "github.com/a/b", "trust": {}});
    assert_eq!(schema::violations(&schema, &trust).len(), 1);
    let accept = serde_json::json!({
        "version": 1, "repository": "github.com/a/b",
        "deploy": {"accept": "ci"}
    });
    assert_eq!(schema::violations(&schema, &accept).len(), 1);
    // ADR 0062: an older file's `stages`, a check's `kind` and an artifact's `locations` are
    // read past, as hushrun reads past them.
    let older = serde_json::json!({
        "version": 1, "repository": "github.com/a/b",
        "checks": {"unit": {"kind": "source", "run": "true"}},
        "artifacts": {"image": {"type": "oci", "locations": {"staging": "ghcr.io/a/b"}}},
        "stages": {"staging": {"require": ["unit"]}}
    });
    assert!(schema::violations(&schema, &older).is_empty());
}

// covers: C-15
#[test]
fn every_pinned_init_plan_matches_its_schema() {
    let schema = schema::load("init-plan.v1.schema.json");
    let mut cases: Vec<_> = std::fs::read_dir(hushrun_testkit::testdata("init"))
        .unwrap()
        .map(|e| e.unwrap().path().join("expected.json"))
        .filter(|p| p.exists())
        .collect();
    cases.sort();
    assert!(cases.len() >= 6, "{cases:?}");
    for path in cases {
        let plan: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let found = schema::violations(&schema, &plan);
        assert!(found.is_empty(), "{}: {found:#?}", path.display());
    }
    // It refuses what a reader can't rely on: an unknown status, a field it doesn't define.
    let mut bad: serde_json::Value = serde_json::from_slice(
        &std::fs::read(hushrun_testkit::testdata(
            "init/F01-coverage-flag-beside-codecov/expected.json",
        ))
        .unwrap(),
    )
    .unwrap();
    bad["jobs"][0]["commands"][0]["status"] = serde_json::json!("maybe");
    bad["extra"] = serde_json::json!(true);
    assert_eq!(schema::violations(&schema, &bad).len(), 2);
}
