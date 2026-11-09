//! `hushrun digest` end to end.

use std::path::Path;

use assert_cmd::Command;
use hushrun_testkit::git;
use predicates::prelude::*;

fn hushrun(dir: &Path, cache: &Path) -> Command {
    let mut cmd = Command::cargo_bin("hushrun").unwrap();
    cmd.current_dir(dir)
        .env("HUSHRUN_CACHE_DIR", cache)
        .env_remove("HUSHRUN_CONFIG")
        .env_remove("HUSHRUN_FORMAT")
        .env_remove("HUSHRUN_LOG")
        .env_remove("HUSHRUN_GIT_READER");
    cmd
}

fn write(dir: &Path, path: &str, bytes: &[u8]) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

struct Repo {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    cache: std::path::PathBuf,
}

fn repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let cache = tmp.path().join("cache");
    std::fs::create_dir(&root).unwrap();
    git::init(&root, "sha1");
    write(&root, "README.md", b"# app\n");
    write(&root, "services/api/main.go", b"package main\n");
    write(&root, "services/api/main_test.go", b"package main\n");
    write(&root, "services/web/index.ts", b"export {}\n");
    git::commit_all(&root, "init");
    Repo {
        _tmp: tmp,
        root,
        cache,
    }
}

const SCOPES: &str = "\
version: 1
repository: example.com/app
scopes:
  api:
    include: [\"services/api/**\"]
    exclude: [\"**/*_test.go\"]
  web:
    include: [\"services/web/**\"]
";

fn stdout(cmd: &mut Command) -> String {
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

// covers: D-5
#[test]
fn the_manifest_hashes_to_the_digest() {
    let r = repo();
    let manifest = r.root.join("../manifest");
    let out = stdout(hushrun(&r.root, &r.cache).args([
        "digest",
        "--manifest-out",
        manifest.to_str().unwrap(),
    ]));
    let digest = out.split_whitespace().next().unwrap();
    let bytes = std::fs::read(&manifest).unwrap();
    assert!(bytes.starts_with(b"hushrun-source-v1\n"));
    let sha = hushrun_digest::hash::sha256(&bytes);
    assert_eq!(
        digest,
        format!("sha256:{}", hushrun_core::digest::to_hex(&sha))
    );
    assert!(out.trim_end().ends_with("  default"), "{out}");
}

#[test]
fn formats() {
    let r = repo();
    let json: serde_json::Value = serde_json::from_str(&stdout(
        hushrun(&r.root, &r.cache).args(["digest", "--format", "json"]),
    ))
    .unwrap();
    hushrun_testkit::schema::assert_valid(
        &hushrun_testkit::schema::load("digest.v1.schema.json"),
        &json,
    );
    assert_eq!(json["schema"], "hushrun.digest/v1");
    assert_eq!(json["canonicalization"], "hushrun-source-v1");
    assert_eq!(json["source"]["vcs"], "git");
    assert_eq!(json["source"]["ref"], "refs/heads/main");
    assert_eq!(json["scopes"][0]["scope"], "default");
    assert_eq!(json["scopes"][0]["files"], 4);
    let digest = json["scopes"][0]["digest"].as_str().unwrap().to_string();

    let env = stdout(hushrun(&r.root, &r.cache).args(["digest", "--format", "env"]));
    assert!(env.contains(&format!("HUSHRUN_DIGEST={digest}\n")), "{env}");
    assert!(env.contains("HUSHRUN_REVISION="), "{env}");

    let dotenv_file = r.root.join("../digest.env");
    let dotenv = stdout(hushrun(&r.root, &r.cache).args([
        "--format",
        "dotenv",
        "--dotenv",
        dotenv_file.to_str().unwrap(),
        "digest",
    ]));
    assert!(
        dotenv.starts_with(&format!("HUSHRUN_DIGEST={digest}\n")),
        "{dotenv}"
    );
    assert_eq!(std::fs::read_to_string(dotenv_file).unwrap(), dotenv);

    // HUSHRUN_FORMAT is the default for --format.
    let env2 = stdout(
        hushrun(&r.root, &r.cache)
            .env("HUSHRUN_FORMAT", "env")
            .arg("digest"),
    );
    assert_eq!(env2, env);
}

// covers: C-4, D-7
#[test]
fn scopes_come_from_hushrun_yaml_in_the_evaluated_revision() {
    let r = repo();
    write(&r.root, "hushrun.yaml", SCOPES.as_bytes());
    git::commit_all(&r.root, "scopes");
    let all = stdout(hushrun(&r.root, &r.cache).arg("digest"));
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines.len(), 2, "{all}");
    assert!(
        lines[0].ends_with("  api") && lines[1].ends_with("  web"),
        "{all}"
    );

    let api = stdout(hushrun(&r.root, &r.cache).args(["digest", "--scope", "api"]));
    assert_eq!(api.trim(), lines[0]);
    // A test file is outside the api scope: changing it keeps the digest.
    write(
        &r.root,
        "services/api/main_test.go",
        b"package main // changed\n",
    );
    git::commit_all(&r.root, "test only");
    let api_after = stdout(hushrun(&r.root, &r.cache).args(["digest", "--scope", "api"]));
    assert_eq!(api_after, api);

    // An uncommitted edit of hushrun.yaml doesn't change revision mode, only worktree mode.
    write(
        &r.root,
        "hushrun.yaml",
        b"version: 1\nrepository: example.com/app\n",
    );
    let committed = stdout(hushrun(&r.root, &r.cache).args(["digest", "--all-scopes"]));
    assert_eq!(committed, all.replace(&api, &api_after));
    let worktree = stdout(hushrun(&r.root, &r.cache).args(["digest", "--worktree"]));
    assert!(worktree.trim_end().ends_with("  default"), "{worktree}");

    hushrun(&r.root, &r.cache)
        .args(["digest", "--scope", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown scope `nope`"));
}

// covers: D-6
#[test]
fn an_empty_scope_is_a_configuration_error() {
    let r = repo();
    write(
        &r.root,
        "hushrun.yaml",
        b"version: 1\nrepository: example.com/app\nscopes:\n  docs:\n    include: [\"docs/**\"]\n",
    );
    git::commit_all(&r.root, "scopes");
    hushrun(&r.root, &r.cache)
        .arg("digest")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("E_EMPTY_SCOPE"));
}

// covers: C-8
#[test]
fn invalid_patterns_report_file_line_and_column() {
    let r = repo();
    write(
        &r.root,
        "hushrun.yaml",
        b"version: 1\nrepository: example.com/app\nscopes:\n  api:\n    include: [\"a//b\"]\n",
    );
    git::commit_all(&r.root, "bad scopes");
    hushrun(&r.root, &r.cache)
        .arg("digest")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "hushrun.yaml:5:15: invalid pattern",
        ));
}

// covers: D-7
#[test]
fn explain_lists_entries_and_the_patterns_that_decided() {
    let r = repo();
    write(&r.root, "hushrun.yaml", SCOPES.as_bytes());
    git::commit_all(&r.root, "scopes");
    let out = stdout(hushrun(&r.root, &r.cache).args(["digest", "--scope", "api", "--explain"]));
    assert!(
        out.contains("services/api/main.go  [api: in, by services/api/**]"),
        "{out}"
    );
    assert!(
        out.contains("services/api/main_test.go  [api: out, excluded by **/*_test.go]"),
        "{out}"
    );
    assert!(
        out.contains("services/web/index.ts  [api: out, no include matches]"),
        "{out}"
    );
    let json: serde_json::Value = serde_json::from_str(&stdout(hushrun(&r.root, &r.cache).args([
        "digest",
        "--scope",
        "api",
        "--explain",
        "--format",
        "json",
    ])))
    .unwrap();
    hushrun_testkit::schema::assert_valid(
        &hushrun_testkit::schema::load("digest.v1.schema.json"),
        &json,
    );
    let entries = json["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["path"] == "services/api/main.go" && e["hash"].is_string())
    );
    assert!(
        entries
            .iter()
            .any(|e| e["path"] == "README.md" && e["hash"].is_null())
    );
}

// covers: D-11, D-13
#[test]
fn dir_mode_matches_git_on_a_clean_export() {
    let r = repo();
    let export = r.root.join("../export");
    std::fs::create_dir(&export).unwrap();
    for file in [
        "README.md",
        "services/api/main.go",
        "services/api/main_test.go",
        "services/web/index.ts",
    ] {
        write(&export, file, &std::fs::read(r.root.join(file)).unwrap());
    }
    let from_git = stdout(hushrun(&r.root, &r.cache).arg("digest"));
    let from_dir =
        stdout(hushrun(&r.root, &r.cache).args(["digest", "--dir", export.to_str().unwrap()]));
    assert_eq!(from_git, from_dir);
}

#[test]
fn outside_a_repository_needs_dir() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "a.txt", b"a\n");
    hushrun(tmp.path(), &tmp.path().join("cache"))
        .arg("digest")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("pass --dir"));
    hushrun(tmp.path(), &tmp.path().join("cache"))
        .args(["digest", "--dir", "."])
        .assert()
        .success();
}

#[test]
fn readers_and_caches_agree() {
    let r = repo();
    let auto = stdout(hushrun(&r.root, &r.cache).arg("digest"));
    let cli = stdout(hushrun(&r.root, &r.cache).args(["digest", "--git-reader", "cli"]));
    let gix = stdout(
        hushrun(&r.root, &r.cache)
            .env("HUSHRUN_GIT_READER", "gix")
            .arg("digest"),
    );
    let uncached = stdout(
        hushrun(&r.root, &r.cache)
            .env("HUSHRUN_CACHE_DIR", "")
            .arg("digest"),
    );
    assert_eq!(auto, cli);
    assert_eq!(auto, gix);
    assert_eq!(auto, uncached);
    let json: serde_json::Value = serde_json::from_str(&stdout(
        hushrun(&r.root, &r.cache).args(["digest", "--format", "json"]),
    ))
    .unwrap();
    assert_eq!(json["stats"]["cacheHits"], 4, "{json}");
}

// covers: D-9
#[test]
fn git_has_the_last_word_on_what_gix_cant_find() {
    let r = repo();
    // The commit's own `git gc` would take the objects this test moves.
    git::run(&r.root, &["config", "maintenance.auto", "false"], None);
    write(&r.root, "hushrun.yaml", SCOPES.as_bytes());
    write(&r.root, "services/web/late.ts", b"export {}; // late\n");
    git::commit_all(&r.root, "scopes");
    let digests = stdout(hushrun(&r.root, &r.cache).arg("digest"));

    // A `git gc` moving objects under gix is a race, so this holds the same moment still: every
    // object is where git can reach it and gix, which doesn't read the environment, can't.
    let objects = r.root.join(".git/objects");
    let moved = r.cache.with_file_name("moved");
    std::fs::rename(&objects, &moved).unwrap();
    std::fs::create_dir(&objects).unwrap();
    let cache = r.cache.with_file_name("no-hashes-yet");
    let with_git = |reader: &str| {
        let mut cmd = hushrun(&r.root, &cache);
        cmd.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &moved)
            .env("HUSHRUN_GIT_READER", reader)
            .arg("digest");
        cmd
    };
    assert_eq!(stdout(&mut with_git("auto")), digests);
    // The moment is real: gix alone, which a person can ask for, can't read this.
    with_git("gix").assert().failure();

    // What git can't find either is missing, as ever.
    std::fs::remove_dir(&objects).unwrap();
    std::fs::rename(&moved, &objects).unwrap();
    let late = git::text(&r.root, &["rev-parse", "HEAD:services/web/late.ts"]);
    std::fs::remove_file(objects.join(&late[..2]).join(&late[2..])).unwrap();
    hushrun(&r.root, &r.cache)
        .env("HUSHRUN_CACHE_DIR", "")
        .arg("digest")
        .assert()
        .failure()
        .stderr(predicate::str::contains("E_MISSING_OBJECTS"));
}

#[test]
fn version() {
    Command::cargo_bin("hushrun")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("hushrun "));
}
