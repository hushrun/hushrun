//! `hushrun adopt` (M14): a starter hushrun.yaml and two workflows that call the reusable
//! workflow, from what a repository looks like and what its organization requires.

use std::path::PathBuf;

use hushrun_testkit::git;
use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use serde_json::Value;

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

#[track_caller]
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

/// A repository to adopt hushrun in, beside a world that gives a clean environment.
struct Repo {
    world: World,
    dir: PathBuf,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let world = World::new("acme-api.yaml");
        let dir = world.path("adopting");
        std::fs::create_dir_all(&dir).unwrap();
        git::init(&dir, "sha1");
        for (path, content) in files {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        git::commit_all(&dir, "init");
        Repo { world, dir }
    }

    fn adopt(&self, args: &[&str]) -> assert_cmd::Command {
        let mut cmd = self.world.hushrun();
        cmd.current_dir(&self.dir).arg("adopt").args(args);
        cmd
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
    }
}

const DEPLOY_YML: &str = "name: Deploy
on:
  push:
    branches: [main]
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: gcloud run deploy shop --image us-docker.pkg.dev/acme/prod/shop --region us-central1
";

/// The golden copy of `path` under testdata/adopt/<name>/, which CI also runs actionlint on.
#[track_caller]
fn golden(repo: &Repo, name: &str, path: &str) {
    let expected_path = hushrun_testkit::testdata(&format!("adopt/{name}/{path}"));
    let actual = repo.read(path);
    if std::env::var_os("HUSHRUN_BLESS").is_some() {
        std::fs::create_dir_all(expected_path.parent().unwrap()).unwrap();
        std::fs::write(&expected_path, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(&expected_path).unwrap_or_default();
    assert_eq!(
        actual,
        expected,
        "{} is out of date (HUSHRUN_BLESS=1 rewrites it); adopt writes:\n{actual}",
        expected_path.display()
    );
}

fn policy_of(repo: &Repo) -> hushrun_core::policy::Policy {
    hushrun_core::policy::Policy::parse("hushrun.yaml", repo.read("hushrun.yaml").as_bytes())
        .unwrap()
}

// covers: T-15
#[test]
fn adopt_writes_hushrun_yaml_and_keyless_workflows_and_is_idempotent() {
    let repo = Repo::new(&[
        ("go.mod", "module example.com/shop\n"),
        ("main.go", "package main\n"),
        ("Dockerfile", "FROM scratch\n"),
        (".github/workflows/deploy.yml", DEPLOY_YML),
    ]);
    let base = "https://hushrun.example/l/acme";
    let adopt = |args: &[&str]| {
        let mut cmd = repo.adopt(args);
        cmd.env("HUSHRUN_REPOSITORY_BASE", base);
        cmd
    };
    let out = json(&ok(&mut adopt(&[
        "--repository",
        "github.com/acme/shop",
        "--format",
        "json",
    ])));
    schema::assert_valid(&schema::load("adopt.v2.schema.json"), &out);
    assert_eq!(out["detected"]["language"], "go");
    assert_eq!(out["branch"], "main");
    assert_eq!(out["detected"]["dockerfiles"], serde_json::json!(["."]));
    assert_eq!(
        out["detected"]["workflows"][0]["file"],
        ".github/workflows/deploy.yml"
    );
    assert_eq!(
        out["ledger"],
        "https://hushrun.example/l/acme/github.com/acme/shop"
    );
    assert!(
        out["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["status"] == "written"),
        "{out}"
    );
    for path in ["hushrun.yaml", ".github/workflows/hushrun.yml"] {
        golden(&repo, "shop", path);
    }
    // ADR 0062: one deploy, no stages, and the team's own deploy workflow is left as it was.
    let policy = policy_of(&repo);
    assert!(policy.retired.is_empty());
    assert_eq!(
        policy.deploy.as_ref().unwrap().branch.as_deref(),
        Some("main")
    );
    assert_eq!(repo.read(".github/workflows/deploy.yml"), DEPLOY_YML);
    // ADR 0031: no trust, and the workflows sign keylessly, as the repository's CI.
    let text = repo.read("hushrun.yaml");
    assert!(!text.contains("trust"), "{text}");
    let workflow = repo.read(".github/workflows/hushrun.yml");
    assert!(
        workflow.contains("signing: sigstore")
            && !workflow.contains("HUSHRUN_SIGNING_KEY")
            && !workflow.contains("stage"),
        "{workflow}"
    );
    // ADR 0035: no hushrun version, so each run installs the newest release, and a comment
    // that says how to pin one.
    assert!(out["version"].is_null(), "{out}");
    assert!(
        workflow.contains(&format!(
            "      # version: {}          # pins hushrun; without it, each run installs the newest release\n",
            env!("CARGO_PKG_VERSION")
        )),
        "{workflow}"
    );

    // Again: nothing changes.
    let again = json(&ok(&mut adopt(&[
        "--repository",
        "github.com/acme/shop",
        "--format",
        "json",
    ])));
    assert!(
        again["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["status"] == "unchanged"),
        "{again}"
    );
    // A file the team changed is kept, and --dry-run shows what --force would write.
    let edited = repo
        .read("hushrun.yaml")
        .replace("go test ./...", "make test");
    std::fs::write(repo.dir.join("hushrun.yaml"), &edited).unwrap();
    let kept =
        String::from_utf8(ok(&mut adopt(&["--repository", "github.com/acme/shop"]))).unwrap();
    assert!(kept.contains("kept          hushrun.yaml"), "{kept}");
    assert_eq!(repo.read("hushrun.yaml"), edited);
    let dry = String::from_utf8(ok(&mut adopt(&[
        "--repository",
        "github.com/acme/shop",
        "--dry-run",
        "--force",
    ])))
    .unwrap();
    assert!(
        dry.contains("==> hushrun.yaml (would-replace)") && dry.contains("go test ./..."),
        "{dry}"
    );
    assert_eq!(
        repo.read("hushrun.yaml"),
        edited,
        "--dry-run writes nothing"
    );
    ok(&mut adopt(&[
        "--repository",
        "github.com/acme/shop",
        "--force",
    ]));
    assert!(repo.read("hushrun.yaml").contains("go test ./..."));
}

#[test]
fn adopt_without_an_organization_follows_the_deploy_workflows() {
    let repo = Repo::new(&[
        ("package.json", "{\"name\": \"web\"}\n"),
        ("pnpm-lock.yaml", "lockfileVersion: 9\n"),
        ("Dockerfile", "FROM scratch\n"),
        (
            ".github/workflows/deploy.yml",
            &DEPLOY_YML.replace("[main]", "[master]"),
        ),
    ]);
    let text =
        String::from_utf8(ok(&mut repo.adopt(&["--repository", "github.com/acme/web"]))).unwrap();
    assert!(
        text.contains("Remove .github/workflows/deploy.yml"),
        "{text}"
    );
    let policy = policy_of(&repo);
    assert_eq!(
        policy.deploy.as_ref().unwrap().branch.as_deref(),
        Some("master")
    );
    assert_eq!(
        policy.checks[&check("unit")].run.as_deref(),
        Some("pnpm test")
    );
    assert!(
        policy.ledger.is_none(),
        "no base: the ledger line is a comment"
    );
    assert!(
        repo.read(".github/workflows/hushrun.yml")
            .contains("toolchain: node")
    );
    // With the organization's variable set, hushrun.yaml names the ledger under it.
    let repo = Repo::new(&[("go.mod", "module x\n")]);
    ok(repo
        .adopt(&[
            "--repository",
            "github.com/acme/api",
            "--branch",
            "release",
            "--image",
            "ghcr.io/acme/api",
        ])
        .env("HUSHRUN_REPOSITORY_BASE", "https://hushrun.example/l/acme"));
    let policy = policy_of(&repo);
    assert_eq!(
        policy.ledger.as_ref().unwrap().to_string(),
        "https://hushrun.example/l/acme/github.com/acme/api"
    );
    assert_eq!(
        policy.deploy.as_ref().unwrap().branch.as_deref(),
        Some("release")
    );
    assert_eq!(
        policy.artifacts.values().next().unwrap().location,
        "ghcr.io/acme/api"
    );
    let workflow = repo.read(".github/workflows/hushrun.yml");
    assert!(
        workflow.contains("    branches: [release]") && workflow.contains("aws-role-to-assume"),
        "{workflow}"
    );
}

#[test]
fn adopt_gives_a_monorepo_a_scope_per_dockerfile() {
    let repo = Repo::new(&[
        ("go.mod", "module x\n"),
        ("services/api/Dockerfile", "FROM scratch\n"),
        ("services/api/main.go", "package main\n"),
        ("web/Dockerfile", "FROM scratch\n"),
    ]);
    ok(&mut repo.adopt(&["--repository", "github.com/acme/mono"]));
    let policy = policy_of(&repo);
    let scopes: Vec<&str> = policy.scopes.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(scopes, ["api", "web"]);
    assert_eq!(policy.artifacts.len(), 2);
    assert!(
        policy
            .scopes
            .contains(policy.scopes.get("api").unwrap(), "services/api/main.go")
    );
    // Each scope covers the deploy workflow (D-7).
    assert!(policy.scopes.contains(
        policy.scopes.get("web").unwrap(),
        ".github/workflows/hushrun.yml"
    ));
    let workflow = repo.read(".github/workflows/hushrun.yml");
    assert!(
        workflow.contains("  api:\n")
            && workflow.contains("  web:\n")
            && workflow.contains("scope: api")
            && workflow.contains("docker build -t \"$REF\" services/api"),
        "{workflow}"
    );
}

// [ADR 0035] A team that wants each run to install the same release pins it.
#[test]
fn adopt_pins_hushrun_when_asked() {
    let repo = Repo::new(&[("go.mod", "module x\n")]);
    let out = json(&ok(&mut repo.adopt(&[
        "--repository",
        "github.com/acme/x",
        "--hushrun-version",
        "0.2.0",
        "--format",
        "json",
    ])));
    assert_eq!(out["version"], "0.2.0");
    schema::assert_valid(&schema::load("adopt.v2.schema.json"), &out);
    let workflow = repo.read(".github/workflows/hushrun.yml");
    assert!(
        workflow.contains("      version: 0.2.0\n") && !workflow.contains("# version:"),
        "{workflow}"
    );
}

#[test]
fn adopt_refuses_bad_arguments() {
    let repo = Repo::new(&[("go.mod", "module x\n")]);
    for (args, needle) in [
        (
            vec![
                "--repository",
                "github.com/acme/x",
                "--stage",
                "staging=develop",
            ],
            "hushrun 0.9 has no stages",
        ),
        (
            vec![
                "--repository",
                "github.com/acme/x",
                "--image",
                "qa=ghcr.io/x",
            ],
            "names no artifact adopt writes",
        ),
        (
            vec![
                "--repository",
                "github.com/acme/x",
                "--hushrun-version",
                "latest",
            ],
            "isn't a release: it pins one, such as 0.2.0",
        ),
    ] {
        let output = repo.adopt(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
    }
    let outside = World::new("acme-api.yaml");
    let dir = outside.path("not-a-repo");
    std::fs::create_dir_all(&dir).unwrap();
    let output = outside
        .hushrun()
        .current_dir(&dir)
        .args(["adopt", "--repository", "github.com/a/b"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

fn check(name: &str) -> hushrun_core::names::CheckName {
    hushrun_core::names::CheckName::new(name).unwrap()
}
