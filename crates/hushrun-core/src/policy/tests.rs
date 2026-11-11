use super::*;
use serde_json::json;

fn fixture(name: &str) -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/policies/");
    std::fs::read(format!("{path}{name}")).unwrap()
}

fn errors(yaml: &str) -> Vec<String> {
    match Policy::parse("hushrun.yaml", yaml.as_bytes()) {
        Ok(_) => Vec::new(),
        Err(errs) => errs.0.iter().map(ToString::to_string).collect(),
    }
}

/// The single error of `yaml`, which must mention `needle`, at `line`.
#[track_caller]
fn assert_error(yaml: &str, line: u64, needle: &str) {
    let found = errors(yaml);
    assert!(
        found
            .iter()
            .any(|e| e.contains(needle) && e.starts_with(&format!("hushrun.yaml:{line}:"))),
        "expected an error at line {line} mentioning {needle:?}, got {found:#?}"
    );
}

const HEAD: &str = "version: 1\nrepository: github.com/acme/app\n";

// covers: C-1, C-2, C-3, C-5, C-6, C-7
#[test]
fn the_plan_example_parses() {
    let policy = Policy::parse("hushrun.yaml", &fixture("acme-api.yaml")).unwrap();
    assert_eq!(policy.repository, "github.com/Acme/api");
    assert_eq!(
        policy.ledger.as_ref().unwrap().to_string(),
        "https://hushrun.example/l/acme/api"
    );
    assert_eq!(policy.scopes.len(), 1);
    let unit = &policy.checks[&CheckName::new("unit").unwrap()];
    assert_eq!(unit.timeout, Some(Dur::parse("20m").unwrap()));
    assert_eq!(unit.retries, 0);
    let deploy = policy.deploy.as_ref().unwrap();
    assert_eq!(deploy.branch.as_deref(), Some("main"));
    assert_eq!(deploy.lease, DEFAULT_LEASE);
    assert_eq!(deploy.dedupe, [Trigger::Push, Trigger::Upstream]);
    assert!(deploy.allow_force);
    assert_eq!(deploy.on_ledger_error, OnLedgerError::Fail);
    let image = &policy.artifacts[&ArtifactName::new("image").unwrap()];
    assert_eq!(image.scope, "default");
    assert_eq!(
        image.location,
        "us-central1-docker.pkg.dev/acme-prod/api/go"
    );
    assert_eq!(
        policy.required(),
        [
            CheckName::new("toolchain").unwrap(),
            CheckName::new("unit").unwrap()
        ]
    );
    assert!(policy.retired.is_empty());
    assert_eq!(policy.digest, Digest::of(&fixture("acme-api.yaml")));
}

// covers: C-1, C-5, C-6, C-7 (ADR 0062: a file from hushrun 0.8 or earlier, with stages, a
// deployment check, an approval and a location per stage, still parses; what is gone is named)
#[test]
fn an_older_file_with_stages_is_read_past_and_named() {
    let bytes = fixture("three-stages.yaml");
    let policy = Policy::parse("hushrun.yaml", &bytes).unwrap();
    let keys: Vec<&str> = policy.retired.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "checks.unit.kind",
            "checks.lint.kind",
            "checks.e2e.kind",
            "checks.release-signoff.kind",
            "artifacts.api-image.locations",
            "artifacts.web-image.locations",
            "stages",
        ]
    );
    let text = String::from_utf8_lossy(&bytes);
    let line_of = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap() as u64 + 1;
    let stages = policy.retired.last().unwrap();
    assert!(stages.pos.line >= line_of("stages:"), "{stages:?}");
    assert!(
        stages
            .notice("hushrun.yaml")
            .contains("`stages` is ignored since hushrun 0.9.0"),
        "{}",
        stages.notice("hushrun.yaml")
    );
    // Only artifacts with one `location` are declared; the old ones are left out.
    assert!(policy.artifacts.is_empty());
    assert!(policy.deploy.is_none());
    // What it requires now: every check with `run`.
    let required: Vec<String> = policy.required().iter().map(ToString::to_string).collect();
    assert_eq!(required, ["e2e", "lint", "unit"]);
    // Scopes and digest exclusions read as before, so its runs keep their keys.
    assert_eq!(policy.scopes.len(), 2);
}

// covers: E-6
#[test]
fn definition_digests_are_the_jcs_of_the_definition() {
    let policy = Policy::parse("hushrun.yaml", &fixture("acme-api.yaml")).unwrap();
    // Written by hand from E-6: members sorted, env as the sorted names; timeout excluded.
    let unit = r#"{"kind":"source","name":"unit","run":"go test ./..."}"#;
    assert_eq!(
        policy.checks[&CheckName::new("unit").unwrap()].definition_digest,
        Digest::of(unit.as_bytes())
    );
    let toolchain = r#"{"env":["GO_VERSION"],"kind":"source","name":"toolchain","run":".github/scripts/check-toolchain.sh"}"#;
    assert_eq!(
        policy.checks[&CheckName::new("toolchain").unwrap()].definition_digest,
        Digest::of(toolchain.as_bytes())
    );
    // max_age, timeout and retries don't change the definition; version does.
    let base = format!("{HEAD}checks:\n  unit:\n    run: go test\n");
    let digest = |yaml: &str| {
        Policy::parse("hushrun.yaml", yaml.as_bytes())
            .unwrap()
            .checks[&CheckName::new("unit").unwrap()]
            .definition_digest
    };
    let plain = digest(&base);
    assert_eq!(
        digest(&format!(
            "{base}    timeout: 5m\n    retries: 2\n    max_age: 1d\n"
        )),
        plain
    );
    assert_ne!(digest(&format!("{base}    version: \"2\"\n")), plain);
    assert_ne!(digest(&format!("{base}    env: [A]\n")), plain);
    assert_eq!(
        digest(&format!("{base}    env: [B, A]\n")),
        digest(&format!("{base}    env: [A, B]\n"))
    );
}

// covers: C-1, C-8
#[test]
fn version_and_unknown_keys() {
    assert_error(
        "version: 2\nrepository: github.com/a/b\n",
        1,
        "`version` must be 1",
    );
    assert_error(&format!("{HEAD}deploy:\n  brnach: main\n"), 4, "brnach");
    assert_error(&format!("{HEAD}chekcs: {{}}\n"), 3, "chekcs");
    assert!(errors("repository: github.com/a/b\n")[0].contains("version"));
}

// covers: C-2
#[test]
fn repository_must_be_canonical() {
    for bad in [
        "https://github.com/acme/app",
        "git@github.com:acme/app",
        "github.com/acme/app.git",
        "GitHub.com/acme/app",
        "github.com",
        "github.com/acme//app",
    ] {
        assert_error(&format!("version: 1\nrepository: {bad}\n"), 2, "repository");
    }
}

// covers: C-3, L-1
#[test]
fn repository_url_must_be_an_https_url() {
    assert_error(
        &format!("{HEAD}repository_url: acme/prefix\n"),
        3,
        "not a repository URL",
    );
    // Only https:// ledgers are left (ADR 0022), and other schemes say so.
    for url in ["gs://acme-hushrun-ledger/app", "file:///var/hushrun/app"] {
        assert_error(
            &format!("{HEAD}repository_url: {url}\n"),
            3,
            "only https:// repository URLs are supported",
        );
    }
    assert_error(
        &format!("{HEAD}repository_url: http://ledger.example.com/l/acme/app\n"),
        3,
        "loopback",
    );
}

// ADR 0030: the names before ADR 0029 were never released, so they are unknown keys, not
// aliases.
#[test]
fn the_names_before_adr_0029_are_unknown_keys() {
    let url = "https://hushrun.example/l/acme/app";
    assert_error(
        &format!("{HEAD}ledger: {url}\n"),
        3,
        "unknown field `ledger`",
    );
    assert_error(
        &format!("{HEAD}deploy:\n  on_ledger_error: full\n"),
        4,
        "unknown field `on_ledger_error`",
    );
}

// covers: C-3, C-8 (ADR 0026 amendment 2: there is no root to pin)
#[test]
fn ledger_root_is_an_unknown_key() {
    let hex = "ab".repeat(32);
    assert_error(
        &format!("{HEAD}ledger_root: sha256:{hex}\n"),
        3,
        "unknown field `ledger_root`",
    );
}

// covers: C-7, C-8 (ADR 0062: `deploy.require` names checks, never a stage)
#[test]
fn deploy_requires_checks_by_name() {
    let checks = "checks:\n  unit: {run: \"true\"}\n  e2e: {run: ./e2e.sh}\n";
    let yaml = |deploy: &str| format!("{HEAD}{checks}deploy:\n{deploy}");
    assert_error(&yaml("  require: [nope]\n"), 7, "unknown check `nope`");
    assert_error(
        &yaml("  require: [e2e@staging]\n"),
        7,
        "names the stage `staging`, and hushrun 0.9 has no stages",
    );
    assert_error(&yaml("  require: [unit, unit]\n"), 7, "required twice");
    assert_error(&yaml("  branch: refs/heads/main\n"), 7, "branch name");
    let ok = parsed(&format!("{checks}deploy:\n  require: [unit]\n"));
    assert_eq!(ok.required(), [CheckName::new("unit").unwrap()]);
    // An empty list requires nothing; no `require` requires every check with `run`.
    let none = parsed(&format!("{checks}deploy:\n  require: []\n"));
    assert!(none.required().is_empty());
    let every = parsed(&format!("{checks}deploy: {{}}\n"));
    assert_eq!(every.required().len(), 2);
}

// covers: C-7 (ADR 0062: what pointed at another stage has no place in `deploy`)
#[test]
fn what_pointed_at_another_stage_is_an_unknown_key_of_deploy() {
    for (key, value) in [
        ("after", "staging"),
        ("upstream", "passed"),
        ("upstream_grace", "30s"),
        ("ci", "{github: {workflow: deploy.yml}}"),
    ] {
        assert_error(
            &format!("{HEAD}deploy:\n  {key}: {value}\n"),
            4,
            &format!("unknown field `{key}`"),
        );
    }
}

// covers: C-7, L-5 (ADR 0062: with no upstream, no deployment check and no approval, nothing
// a full run would skip)
#[test]
fn on_repository_error_full_is_allowed() {
    let policy = parsed(
        "checks:\n  unit: {run: \"true\"}\ndeploy:\n  on_repository_error: full\n  require: [unit]\n",
    );
    assert_eq!(policy.deploy.unwrap().on_ledger_error, OnLedgerError::Full);
}

// covers: C-6, C-8
#[test]
fn an_artifact_has_one_location() {
    assert_error(
        &format!("{HEAD}artifacts:\n  image:\n    type: oci\n"),
        4,
        "needs a `location`",
    );
    assert_error(
        &format!("{HEAD}artifacts:\n  image:\n    type: oci\n    location: ghcr.io/a/b:latest\n"),
        6,
        "no tag or digest",
    );
    assert_error(
        &format!(
            "{HEAD}artifacts:\n  image:\n    type: oci\n    scope: api\n    location: ghcr.io/a/b\n"
        ),
        6,
        "unknown scope `api`",
    );
    let policy = parsed("artifacts:\n  image:\n    type: oci\n    location: ghcr.io/a/b\n");
    assert_eq!(
        policy.artifacts[&ArtifactName::new("image").unwrap()].location,
        "ghcr.io/a/b"
    );
    // Both: the one location is used, and `locations` is named.
    let both = parsed(
        "artifacts:\n  image:\n    type: oci\n    location: ghcr.io/a/b\n    locations:\n      staging: ghcr.io/a/c\n",
    );
    assert_eq!(both.artifacts.len(), 1);
    assert_eq!(both.retired[0].key, "artifacts.image.locations");
}

// covers: C-5, C-8
#[test]
fn check_pins_and_names() {
    assert_error(
        &format!("{HEAD}checks:\n  lint:\n    image: golangci/golangci-lint:v1\n"),
        5,
        "isn't pinned by digest",
    );
    assert_error(
        &format!("{HEAD}checks:\n  lint:\n    env: [GO-VERSION]\n"),
        5,
        "not an environment variable name",
    );
    assert_error(
        &format!("{HEAD}checks:\n  Unit: {{run: x}}\n"),
        4,
        "check name",
    );
    assert_error(
        &format!("{HEAD}checks:\n  unit:\n    timeout: soon\n"),
        5,
        "duration",
    );
}

// covers: C-8, L-2
#[test]
fn names_colliding_as_environment_keys() {
    assert_error(
        &format!("{HEAD}checks:\n  a-b: {{run: x}}\n  a_b: {{run: x}}\n"),
        5,
        "collide as the environment key A_B",
    );
}

// covers: C-8
#[test]
fn every_error_is_reported_with_its_position() {
    let yaml = format!(
        "{HEAD}checks:\n  e2e: {{run: x}}\ndeploy:\n  require: [e2e@staging, nope]\nartifacts:\n  image: {{type: oci}}\n"
    );
    let found = errors(&yaml);
    assert_eq!(found.len(), 3, "{found:#?}");
    for error in &found {
        let (file, rest) = error.split_once(':').unwrap();
        assert_eq!(file, "hushrun.yaml");
        let mut parts = rest.splitn(3, ':');
        assert!(parts.next().unwrap().parse::<u64>().unwrap() > 0, "{error}");
        assert!(parts.next().unwrap().parse::<u64>().unwrap() > 0, "{error}");
    }
}

// covers: T-16, C-7
// ADR 0031 amendment 1: whether a repository takes evidence from CI only is set on the server,
// where a pull request can't drop it. `accept` was never released, so it is an unknown key.
#[test]
fn accept_is_an_unknown_key() {
    assert_error(
        &format!("{HEAD}deploy:\n  accept: ci\n"),
        4,
        "unknown field `accept`",
    );
}

// covers: T-9, T-13
#[test]
fn transparency_log_is_a_top_level_key() {
    let key = format!("ed25519:{}", "A".repeat(43) + "=");
    let policy = Policy::parse(
        "hushrun.yaml",
        format!(
            "{HEAD}transparency_log:\n  origin: hushrun.example/l/acme/app\n  key: \"{key}\"\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let log = policy.transparency_log.unwrap();
    assert_eq!(log.origin, "hushrun.example/l/acme/app");
    assert_error(
        &format!(
            "{HEAD}transparency_log:\n  origin: https://hushrun.example/l/a/b\n  key: \"{key}\"\n"
        ),
        4,
        "scheme",
    );
    // ADR 0033: witnesses are gone, and so are the keys that named them, like any unknown key.
    for (line, field) in [
        ("witnesses: []", "witnesses"),
        ("quorum: 1", "quorum"),
        ("map_quorum: 1", "map_quorum"),
        ("max_age: 5m", "max_age"),
    ] {
        assert_error(
            &format!(
                "{HEAD}transparency_log:\n  origin: hushrun.example/l/acme/app\n  key: \"{key}\"\n  {line}\n"
            ),
            6,
            &format!("unknown field `{field}`"),
        );
    }
    // `trust:` is gone, like any unknown key (C-1).
    assert_error(&format!("{HEAD}trust:\n  principals: {{}}\n"), 3, "trust");
}

// covers: C-4
#[test]
fn a_scope_declares_what_it_builds_against_outside_the_repository() {
    let policy = Policy::parse(
        "hushrun.yaml",
        format!(
            "{HEAD}scopes:\n  back:\n    include: [\"back/**\"]\n    external:\n      - path: ../zed\n      - path: ../cli\n        include: [\"crates/**\", Cargo.lock]\n        exclude: [\"**/*.md\"]\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let back = policy.scopes.get("back").unwrap();
    // In one order, whatever order the file has them in: a run's key lists them.
    let paths: Vec<&str> = back.external.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["../cli", "../zed"]);
    assert_eq!(back.external[0].include.len(), 2);
    assert_eq!(back.external[0].exclude.len(), 1);
    // Without `include`, all of it.
    assert_eq!(back.external[1].include[0].as_str(), "**");

    let scope = |external: &str| {
        format!("{HEAD}scopes:\n  back:\n    include: [\"back/**\"]\n    external:\n{external}")
    };
    for bad in [
        "cli",
        "./cli",
        "/opt/cli",
        "..",
        "../..",
        "../",
        "../a//b",
        "../a/./b",
        "..\\\\cli",
    ] {
        assert_error(
            &scope(&format!("      - path: \"{bad}\"\n")),
            7,
            "outside the repository",
        );
    }
    assert_error(
        &scope("      - path: ../cli\n      - path: ../cli\n"),
        8,
        "listed twice",
    );
    assert_error(
        &scope("      - path: ../cli\n        include: []\n"),
        8,
        "empty `include`",
    );
}

// covers: C-4
#[test]
fn a_scope_with_external_inputs_goes_without_deploy() {
    assert_error(
        &format!(
            "{HEAD}scopes:\n  back:\n    external:\n      - path: ../cli\nchecks:\n  unit: {{run: \"true\"}}\ndeploy:\n  require: [unit]\n"
        ),
        6,
        "a deploy's evidence covers the repository's own files",
    );
    // An older file's `stages` doesn't refuse it any more: hushrun reads past them.
    let older = parsed(
        "scopes:\n  back:\n    external:\n      - path: ../cli\nchecks:\n  unit: {run: \"true\"}\nstages:\n  staging:\n    require: [unit]\n",
    );
    assert_eq!(older.retired[0].key, "stages");
}

// covers: C-3
#[test]
fn hushrun_yaml_names_where_results_go() {
    let policy = Policy::parse(
        "hushrun.yaml",
        format!("{HEAD}backend: https://hushrun.example/o/acme\n").as_bytes(),
    )
    .unwrap();
    assert_eq!(
        policy.backend.unwrap().to_string(),
        "https://hushrun.example/o/acme"
    );
    assert!(
        Policy::parse("hushrun.yaml", HEAD.as_bytes())
            .unwrap()
            .backend
            .is_none()
    );
    // The rules of any URL hushrun sends a token to.
    assert_error(
        &format!("{HEAD}backend: http://hushrun.example/o/acme\n"),
        3,
        "http",
    );
}

fn parsed(yaml: &str) -> Policy {
    Policy::parse("hushrun.yaml", format!("{HEAD}{yaml}").as_bytes()).unwrap()
}

fn rule_of<'a>(
    policy: &'a Policy,
    check: &str,
    scope: &str,
) -> Option<&'a crate::counts_from::Rule> {
    policy.counts_from.resolve(
        &CheckName::new(check).unwrap(),
        &ScopeName::new(scope).unwrap(),
    )
}

// covers: C-25
#[test]
fn counts_from_as_the_examples_write_it() {
    let ios = parsed("counts_from:\n  os: macos\n");
    let rule = rule_of(&ios, "lint", "default").unwrap();
    assert_eq!(rule.os, ["macos"]);
    assert_eq!(rule.normal(), json!({"os": ["macos"]}));

    let go = parsed("counts_from:\n  os: linux\n  go: \"1.25\"\n");
    assert_eq!(
        rule_of(&go, "unit", "default").unwrap().normal(),
        json!({"os": ["linux"], "go": ["1.25"]})
    );

    // Every key takes one value or a list, and a list has one form in any order.
    let list = parsed(
        "counts_from: {arch: [arm64, amd64], node: [\"24\", \"22\"], env: {SANDBOX: [b, a], CI_NODE: only}}\n",
    );
    assert_eq!(
        rule_of(&list, "unit", "default").unwrap().normal(),
        json!({"arch": ["amd64", "arm64"], "node": ["22", "24"],
               "env": {"CI_NODE": ["only"], "SANDBOX": ["a", "b"]}})
    );
    let shuffled = parsed(
        "counts_from: {env: {CI_NODE: [only], SANDBOX: [a, b]}, node: [\"22\", \"24\"], arch: [amd64, arm64]}\n",
    );
    assert_eq!(
        rule_of(&list, "unit", "default"),
        rule_of(&shuffled, "unit", "default")
    );

    // A version YAML reads as text needs no quotes, and a quoted one is what it says.
    let plain = parsed("counts_from: {go: 1.25.3, node: \"24\"}\n");
    assert_eq!(
        rule_of(&plain, "unit", "default").unwrap().normal(),
        json!({"go": ["1.25.3"], "node": ["24"]})
    );
}

// covers: C-25
#[test]
fn the_nearest_counts_from_wins_whole() {
    let policy = parsed(
        "counts_from: {os: linux}
scopes:
  ios:
    include: [\"ios/**\"]
    counts_from: {os: macos}
  api:
    include: [\"api/**\"]
  web:
    include: [\"web/**\"]
    counts_from: anywhere
checks:
  swiftlint:
    counts_from: anywhere
  e2e:
    counts_from: {node: \"24\"}
",
    );
    let os = |check: &str, scope: &str| rule_of(&policy, check, scope).map(|rule| rule.normal());
    // A scope's, else the file's.
    assert_eq!(os("test", "ios"), Some(json!({"os": ["macos"]})));
    assert_eq!(os("test", "api"), Some(json!({"os": ["linux"]})));
    // `anywhere` is no rule at its level, and what's further out doesn't apply.
    assert_eq!(os("test", "web"), None);
    assert_eq!(os("swiftlint", "ios"), None);
    assert_eq!(os("swiftlint", "api"), None);
    // A check's, by name in every scope, with nothing merged.
    for scope in ["ios", "api", "web"] {
        assert_eq!(os("e2e", scope), Some(json!({"node": ["24"]})), "{scope}");
    }
}

// covers: C-5, E-6 (ADR 0062: a check's `kind` is read past, and every definition still says
// `source`, so evidence recorded before 0.9 keeps its digest)
#[test]
fn kind_is_read_past_and_digests_dont_move() {
    let without = parsed("checks:\n  unit: {run: go test ./...}\n");
    let with = parsed("checks:\n  unit: {kind: source, run: go test ./...}\n");
    let deployment = parsed("checks:\n  unit: {kind: deployment, run: go test ./...}\n");
    let ruled = parsed("checks:\n  unit: {run: go test ./..., counts_from: {os: linux}}\n");
    let unit = CheckName::new("unit").unwrap();
    let digest = with.checks[&unit].definition_digest;
    assert_eq!(without.checks[&unit].definition_digest, digest);
    assert_eq!(deployment.checks[&unit].definition_digest, digest);
    assert_eq!(ruled.checks[&unit].definition_digest, digest);
    assert!(without.retired.is_empty());
    assert_eq!(with.retired[0].key, "checks.unit.kind");
    // What a check is for hushrun.yaml's sake needs no `run`, and isn't required.
    let bare = parsed("checks:\n  swiftlint:\n    counts_from: anywhere\n");
    assert!(
        bare.checks
            .contains_key(&CheckName::new("swiftlint").unwrap())
    );
    assert!(bare.required().is_empty());
}

// covers: C-5, E-6, C-26
#[test]
fn a_checks_reports_say_its_files_and_what_ci_does() {
    let unit = CheckName::new("unit").unwrap();
    let plain = parsed("checks:\n  unit: {run: pytest}\n");
    let both = parsed(
        "checks:\n  unit:\n    run: pytest\n    reports:\n      files: [coverage.xml, \"test-results/**/*.xml\"]\n      ci: skip\n",
    );
    assert_eq!(
        both.reports[&unit],
        Reports {
            files: vec!["coverage.xml".into(), "test-results/**/*.xml".into()],
            ci: ReportsCi::Skip,
        }
    );
    // Not part of the check's definition.
    assert_eq!(
        both.checks[&unit].definition_digest,
        plain.checks[&unit].definition_digest
    );
    assert!(plain.reports.is_empty());
    // CI runs it unless it says skip, and a check needs nothing else to say so.
    let bare = parsed("checks:\n  lint:\n    reports: {ci: run}\n");
    assert_eq!(
        bare.reports[&CheckName::new("lint").unwrap()],
        Reports::default()
    );
    let files = parsed("checks:\n  unit:\n    reports: {files: [out/junit.xml]}\n");
    assert_eq!(files.reports[&unit].ci, ReportsCi::Run);
    // An unknown key or value, too many files, an absolute or an empty one.
    let check = |reports: &str| format!("{HEAD}checks:\n  unit:\n    reports:\n{reports}");
    assert_error(&check("      ci: maybe\n"), 6, "maybe");
    assert_error(&check("      upload: true\n"), 6, "upload");
    let many: Vec<String> = (0..33).map(|i| format!("r{i}.xml")).collect();
    assert_error(
        &check(&format!("      files: [{}]\n", many.join(", "))),
        6,
        "at most 32",
    );
    assert_error(
        &check("      files: [/tmp/junit.xml]\n"),
        6,
        "isn't relative",
    );
    assert_error(
        &check("      files: ['C:\\out\\junit.xml']\n"),
        6,
        "isn't relative",
    );
    assert_error(&check("      files: [\"\"]\n"), 6, "empty report file");
    // Only a check has them.
    assert_error(&format!("{HEAD}reports: {{ci: skip}}\n"), 3, "reports");
}

// covers: C-8, C-25
#[test]
fn a_file_writes_counts_from_at_any_level_or_none() {
    assert!(parsed("").counts_from.is_empty());
    assert!(
        parsed("checks:\n  unit: {kind: source}\n")
            .counts_from
            .is_empty()
    );
    for level in [
        "counts_from: {os: linux}\n",
        "counts_from: anywhere\n",
        "scopes:\n  a:\n    include: [\"**\"]\n    counts_from: anywhere\n",
        "checks:\n  unit:\n    counts_from: {os: macos}\n",
        "checks:\n  unit:\n    counts_from: anywhere\n",
    ] {
        assert!(!parsed(level).counts_from.is_empty(), "{level}");
    }
}

// covers: C-8, C-25
#[test]
fn counts_from_says_what_is_wrong_with_it() {
    let bad = |yaml: &str, line: u64, needle: &str| {
        assert_error(&format!("{HEAD}{yaml}"), line, needle);
    };
    // A number YAML has already changed: `1.20` reads as 1.2.
    bad("counts_from: {go: 1.20}\n", 3, "quote it");
    bad("counts_from: {go: 1.25}\n", 3, "YAML makes 1.20 into 1.2");
    bad("counts_from: {node: 24}\n", 3, "quote it");
    bad("counts_from: {xcode: [\"16\", 15]}\n", 3, "quote it");
    bad("counts_from: {env: {SANDBOX: 7}}\n", 3, "quote it");
    bad("counts_from: {env: {READY: true}}\n", 3, "quote it");
    bad("counts_from:\n  go:\n", 4, "quote it");
    // A closed list, each with what to write.
    bad(
        "counts_from: {os: darwin}\n",
        3,
        "`os` is `macos`, not `darwin`",
    );
    bad(
        "counts_from: {os: Linux}\n",
        3,
        "`os` is `linux`, not `Linux`",
    );
    bad(
        "counts_from: {os: freebsd}\n",
        3,
        "one of macos, linux, windows",
    );
    bad("counts_from: {arch: x86_64}\n", 3, "`arch` is `amd64`");
    bad("counts_from: {arch: aarch64}\n", 3, "`arch` is `arm64`");
    bad("counts_from: {arch: riscv64}\n", 3, "one of arm64, amd64");
    bad("counts_from: {python: \"3.12\"}\n", 3, "no key `python`");
    bad("counts_from: {go: v1.25}\n", 3, "a version such as");
    bad("counts_from: {go: \"1.25.x\"}\n", 3, "a version such as");
    bad(
        "counts_from: {env: {SAND-BOX: x}}\n",
        3,
        "environment variable name",
    );
    bad("counts_from: {env: [SANDBOX]}\n", 3, "maps variables");
    // Nothing that reads as "no rule" by accident.
    bad("counts_from:\n", 3, "`anywhere`, or what a check needs");
    bad(
        "counts_from: everywhere\n",
        3,
        "`anywhere`, or what a check needs",
    );
    bad(
        "counts_from: [linux]\n",
        3,
        "`anywhere`, or what a check needs",
    );
    bad("counts_from: {}\n", 3, "is empty");
    bad("counts_from: {os: []}\n", 3, "lists nothing");
    bad("counts_from: {env: {}}\n", 3, "lists nothing");
    bad("counts_from: {env: {X: []}}\n", 3, "lists nothing");
    bad("counts_from: {os: [linux, linux]}\n", 3, "listed twice");
    // At each level.
    bad(
        "scopes:\n  a:\n    include: [\"**\"]\n    counts_from: {os: darwin}\n",
        6,
        "`macos`",
    );
    bad("checks:\n  a:\n    counts_from: {go: 1.2}\n", 5, "quote it");
}

// covers: C-8, C-25
#[test]
fn counts_from_is_for_files_without_deploy() {
    const DEPLOY: &str = "deploy:\n  require: [unit]\n";
    const UNIT: &str = "checks:\n  unit: {run: \"true\"}\n";
    let needle = "this file has `deploy`";
    assert_error(
        &format!("{HEAD}counts_from: {{os: linux}}\n{UNIT}{DEPLOY}"),
        3,
        needle,
    );
    assert_error(
        &format!("{HEAD}counts_from: anywhere\n{UNIT}{DEPLOY}"),
        3,
        needle,
    );
    assert_error(
        &format!("{HEAD}scopes:\n  default:\n    counts_from: {{os: linux}}\n{UNIT}{DEPLOY}"),
        5,
        needle,
    );
    assert_error(
        &format!(
            "{HEAD}checks:\n  unit:\n    run: \"true\"\n    counts_from: {{os: linux}}\n{DEPLOY}"
        ),
        6,
        needle,
    );
    // A deploy with no rule stays as it was; an older file's stages don't refuse a rule.
    assert!(errors(&format!("{HEAD}{UNIT}{DEPLOY}")).is_empty());
    assert!(
        errors(&format!(
            "{HEAD}counts_from: anywhere\n{UNIT}stages:\n  staging: {{}}\n"
        ))
        .is_empty()
    );
}

// covers: C-3, C-17, C-25 (ADR 0046: this file gets printed and is in git, so one that holds a string
// is refused before anything can quote it, wherever in the file it is; words that name one aren't)
#[test]
fn a_file_that_holds_a_string_is_refused_and_doesnt_quote_it() {
    let string = crate::agent::AgentString::encode(
        "https://app.example/o/acme",
        "hra_0123456789abcdefghijABCDEFGHIJ0123456789",
        "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIA==\n-----END PRIVATE KEY-----\n",
    );
    let body = &string["hushrun_agent_v1_".len()..];
    for (yaml, line) in [
        (
            format!("{HEAD}counts_from: {{env: {{SANDBOX: [{string}]}}}}\n"),
            3,
        ),
        (
            format!("{HEAD}counts_from: {{env: {{SANDBOX: [\"{body}\"]}}}}\n"),
            3,
        ),
        (format!("{HEAD}counts_from: {{env: {{{string}: x}}}}\n"), 3),
        (
            format!("{HEAD}\ncounts_from:\n  env:\n    SANDBOX: {string}\n"),
            6,
        ),
        (format!("{HEAD}# {string}\n"), 3),
    ] {
        let found = errors(&yaml);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0].starts_with(&format!("hushrun.yaml:{line}:")),
            "{found:#?}"
        );
        assert!(
            found[0].contains("holds a HUSHRUN_AGENT string"),
            "{found:#?}"
        );
        assert!(!found[0].contains(body), "{found:#?}");
    }
    assert_eq!(
        errors(&format!(
            "{HEAD}# hushrun_agent_v1_ is what HUSHRUN_AGENT holds\n"
        )),
        Vec::<String>::new()
    );
}

// covers: C-5, E-6 (ADR 0068: `tests`, short or long, isn't part of a check's definition)
#[test]
fn a_checks_tests_are_a_mode_or_a_mode_with_globs() {
    use crate::units::Mode;
    let unit = CheckName::new("unit").unwrap();
    let plain = parsed("checks:\n  unit: {run: pytest}\n");
    assert!(plain.tests.is_empty());
    // `off` is as good as absent.
    assert!(parsed("checks:\n  unit: {tests: off}\n").tests.is_empty());
    let short = parsed("checks:\n  unit: {run: pytest, tests: observe}\n");
    assert_eq!(short.tests[&unit].mode, Mode::Observe);
    assert!(short.tests[&unit].ignore.is_empty());
    assert_eq!(
        short.checks[&unit].definition_digest,
        plain.checks[&unit].definition_digest
    );
    let reuse = parsed("checks:\n  unit: {tests: reuse}\n");
    assert_eq!(reuse.tests[&unit].mode, Mode::Reuse);
    let long = parsed(
        "checks:\n  unit:\n    tests:\n      mode: observe\n      ignore: [\"docs/adr/**\", \"**/*.md\"]\n      always: [tests/test_lint.py]\n",
    );
    let tests = &long.tests[&unit];
    assert_eq!(tests.mode, Mode::Observe);
    assert!(tests.ignore.iter().any(|g| g.is_match("docs/adr/0001.md")));
    assert!(tests.ignore.iter().any(|g| g.is_match("README.md")));
    assert!(tests.always_runs("tests/test_lint.py"));
    // What it refuses.
    let check = |tests: &str| format!("{HEAD}checks:\n  unit:\n    tests:{tests}");
    assert_error(&check(" sometimes\n"), 5, "off, observe or reuse");
    assert_error(&check(" true\n"), 5, "off, observe or reuse");
    assert_error(&check("\n      ignore: [docs/**]\n"), 6, "needs a `mode`");
    assert_error(
        &check("\n      mode: observe\n      only: [a]\n"),
        7,
        "no key `only`",
    );
    assert_error(
        &check("\n      mode: observe\n      ignore: docs/**\n"),
        7,
        "list of globs",
    );
    assert_error(
        &check("\n      mode: observe\n      always: [\"[\"]\n"),
        7,
        "",
    );
    assert_error(&check("\n      mode: maybe\n"), 6, "maybe");
}
