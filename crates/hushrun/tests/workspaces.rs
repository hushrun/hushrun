//! \[ADR 0051\] A workspace package is a scope, end to end: init splits a JavaScript workspace by
//! its package graph, each package's script runs under its package's scope, and a change runs
//! only the checks of the packages that reach it (W1 to W5). What init can't read keeps one scope,
//! and says why (C-14); a turbo.json it can't edit keeps hushrun's variables from what turbo runs,
//! which then runs every time, and the plan says so.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::git;
use serde_json::Value;

/// Variables a test must never inherit.
const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_JOB",
    "GITHUB_REPOSITORY",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_WORKSPACE",
    "HUSHRUN_VARIANT",
    "HUSHRUN_WORKFLOW",
    "HUSHRUN_INPUTS",
    "GITLAB_CI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "npm_lifecycle_event",
];

/// Where the hosted backend would be: nothing listens on the discard port.
const NO_HOST: &str = "http://127.0.0.1:9";

/// The backend a dry run signed out to nothing names.
const PLACEHOLDER: &str = "<your org's URL, once you sign in>";

const WORKFLOW: &str = "on: [push, pull_request]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v5\n      - uses: pnpm/action-setup@v4\n      - run: pnpm install\n      - run: pnpm test\n";

/// A repository with a remote, and a cache and credentials of its own.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::create_dir_all(root.join("bin")).expect("mkdir");
        std::os::unix::fs::symlink(
            assert_cmd::cargo::cargo_bin("hushrun"),
            root.join("bin/hushrun"),
        )
        .expect("link");
        git::init(&repo, "sha1");
        git::run(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/shop.git",
            ],
            None,
        );
        let r = Repo {
            _dir: dir,
            root,
            repo,
        };
        for (path, text) in files {
            r.write(path, text);
        }
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    /// `program` here, with no inherited configuration, the fixtures' tools first on PATH.
    fn command(&self, program: &Path, dir: &str) -> Command {
        let mut cmd = Command::new(program);
        let path = std::env::var_os("PATH").expect("PATH");
        let path = std::env::join_paths(
            [hushrun_testkit::testdata("init/bin"), self.root.join("bin")]
                .into_iter()
                .chain(std::env::split_paths(&path)),
        )
        .expect("PATH");
        cmd.current_dir(self.repo.join(dir))
            .env("PATH", path)
            .env("HUSHRUN_CACHE_DIR", self.root.join("cache"))
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for var in CLEARED {
            cmd.env_remove(var);
        }
        cmd
    }

    /// init's plan for the repository, as JSON.
    fn plan(&self) -> Value {
        let out = self
            .command(&assert_cmd::cargo::cargo_bin("hushrun"), "")
            .args(["init", "--dry-run", "--no-browser", "--format", "json"])
            .output()
            .expect("init");
        assert!(out.status.success(), "{}", said(&out));
        serde_json::from_slice(&out.stdout).expect("a plan")
    }

    /// Applies the plan's edits, with `backend` for the org's URL, and commits them.
    fn apply(&self, plan: &Value, backend: &str) {
        for edit in plan["edits"].as_array().expect("edits") {
            let diff = edit["diff"]
                .as_str()
                .expect("a diff")
                .replace(PLACEHOLDER, backend);
            git::run(&self.repo, &["apply", "-"], Some(diff.as_bytes()));
        }
        git::commit_all(&self.repo, "init's edits");
    }

    /// Runs package `dir`'s script `script` as its package manager would: what hushrun said.
    fn script(&self, fake: &FakeResults, dir: &str, script: &str) -> String {
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(self.repo.join(dir).join("package.json")).expect("package.json"),
        )
        .expect("JSON");
        let line = manifest["scripts"][script].as_str().expect("the script");
        let out = self
            .command(Path::new("/bin/sh"), dir)
            .env("HUSHRUN_BACKEND", fake.url())
            .env("HUSHRUN_TOKEN", FakeResults::TOKEN)
            .env("npm_lifecycle_event", script)
            .args(["-c", line])
            .output()
            .expect("sh");
        assert!(out.status.success(), "{dir} {script}: {}", said(&out));
        said(&out)
    }

    /// Changes `path` and commits it, as a pull request would.
    fn change(&self, path: &str) {
        let file = self.repo.join(path);
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        text.push_str("// changed\n");
        self.write(path, &text);
        git::commit_all(&self.repo, &format!("change {path}"));
    }
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A pnpm workspace: `a` depends on `b`; `a`'s test reads a shared config outside both.
fn pnpm_workspace() -> Repo {
    Repo::new(&[
        ("pnpm-workspace.yaml", "packages:\n  - \"packages/*\"\n"),
        (
            "package.json",
            r#"{"name": "monorepo", "private": true, "packageManager": "pnpm@10.0.0", "scripts": {"test": "pnpm -r test"}}"#,
        ),
        (
            "packages/a/package.json",
            r#"{"name": "a", "scripts": {"test": "vitest run --config ../../configs/vitest.shared.ts"}, "dependencies": {"b": "workspace:*"}}"#,
        ),
        ("packages/a/src/index.ts", "export { b } from \"b\";\n"),
        (
            "packages/b/package.json",
            r#"{"name": "b", "scripts": {"test": "vitest run"}}"#,
        ),
        ("packages/b/src/index.ts", "export const b = 1;\n"),
        ("configs/vitest.shared.ts", "export default {};\n"),
        ("tsconfig.json", "{}\n"),
        (".github/workflows/ci.yml", WORKFLOW),
    ])
}

fn skipped(said: &str) -> bool {
    said.contains("hushrun: test skipped")
}

// covers: C-15, C-14
#[test]
fn a_change_runs_the_checks_of_the_packages_that_reach_it() {
    let fake = FakeResults::start();
    let repo = pnpm_workspace();
    let plan = repo.plan();
    let wrapped: Vec<(&str, &str)> = plan["scripts"]["wrapped"]
        .as_array()
        .expect("wrapped")
        .iter()
        .map(|w| (w["package"].as_str().unwrap(), w["after"].as_str().unwrap()))
        .collect();
    assert!(
        wrapped.contains(&(
            "packages/a/package.json",
            "hushrun run --scope a -- vitest run --config ../../configs/vitest.shared.ts"
        )),
        "{wrapped:?}"
    );
    assert!(
        wrapped.contains(&(
            "packages/b/package.json",
            "hushrun run --scope b -- vitest run"
        )),
        "{wrapped:?}"
    );
    repo.apply(&plan, &fake.url());
    let both = |repo: &Repo| {
        (
            repo.script(&fake, "packages/a", "test"),
            repo.script(&fake, "packages/b", "test"),
        )
    };
    let (a, b) = both(&repo);
    assert!(!skipped(&a) && !skipped(&b), "a first run runs:\n{a}\n{b}");
    let (a, b) = both(&repo);
    assert!(skipped(&a) && skipped(&b), "a pass skips:\n{a}\n{b}");

    // W1: what `a` depends on changes, and both run.
    repo.change("packages/b/src/index.ts");
    let (a, b) = both(&repo);
    assert!(!skipped(&a) && !skipped(&b), "b changed:\n{a}\n{b}");

    // W1: `a` changes, and `b`, which doesn't depend on it, skips.
    repo.change("packages/a/src/index.ts");
    let (a, b) = both(&repo);
    assert!(!skipped(&a), "a changed, a runs:\n{a}");
    assert!(skipped(&b), "a changed, b skips:\n{b}");

    // W2: a file at the root changes, and everything runs.
    repo.change("tsconfig.json");
    let (a, b) = both(&repo);
    assert!(!skipped(&a) && !skipped(&b), "the root changed:\n{a}\n{b}");

    // W3: the shared config `a`'s script names changes: `a` runs, and `b` skips.
    repo.change("configs/vitest.shared.ts");
    let (a, b) = both(&repo);
    assert!(!skipped(&a), "its config changed, a runs:\n{a}");
    assert!(skipped(&b), "a's config changed, b skips:\n{b}");
}

/// The directories of the plan's scopes, by name.
fn scopes(plan: &Value) -> Vec<(String, String)> {
    plan["scopes"]
        .as_array()
        .expect("scopes")
        .iter()
        .map(|scope| {
            (
                scope["name"].as_str().unwrap_or_default().to_string(),
                scope["dir"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// What a case is: its name, its root's package.json, and the files beside it.
type Workspace<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

// covers: C-15
#[test]
fn npm_yarn_and_bun_workspaces_name_their_packages() {
    let cases: [Workspace; 3] = [
        (
            "npm, workspaces as an object",
            r#"{"name": "shop", "private": true, "workspaces": {"packages": ["packages/*"]}}"#,
            &[("package-lock.json", "{\"lockfileVersion\": 3}\n")],
        ),
        (
            "Yarn",
            r#"{"name": "shop", "private": true, "packageManager": "yarn@4.5.0", "workspaces": ["packages/*"]}"#,
            &[],
        ),
        (
            "Bun",
            r#"{"name": "shop", "private": true, "workspaces": ["packages/*"]}"#,
            &[("bun.lock", "{\"lockfileVersion\": 1}\n")],
        ),
    ];
    for (what, root, extra) in cases {
        let mut files: Vec<(&str, &str)> = vec![
            ("package.json", root),
            (
                "packages/web/package.json",
                r#"{"name": "web", "scripts": {"test": "vitest run"}, "dependencies": {"core": "workspace:*"}}"#,
            ),
            (
                "packages/core/package.json",
                r#"{"name": "core", "scripts": {"test": "vitest run"}}"#,
            ),
        ];
        files.extend_from_slice(extra);
        let repo = Repo::new(&files);
        let plan = repo.plan();
        let found = scopes(&plan);
        assert!(
            found.contains(&("web".to_string(), "packages/web".to_string()))
                && found.contains(&("core".to_string(), "packages/core".to_string())),
            "{what}: {found:?}"
        );
    }
}

// covers: C-14
#[test]
fn a_graph_hushrun_cant_read_keeps_one_scope_and_says_why() {
    let repo = Repo::new(&[
        ("pnpm-workspace.yaml", "packages:\n  - \"packages/*\"\n"),
        (
            "package.json",
            r#"{"name": "monorepo", "private": true, "packageManager": "pnpm@10.0.0"}"#,
        ),
        (
            "packages/web/package.json",
            r#"{"name": "web", "scripts": {"test": "vitest run"}, "dependencies": {"core": "github:acme/core"}}"#,
        ),
        (
            "packages/core/package.json",
            r#"{"name": "core", "scripts": {"test": "vitest run"}}"#,
        ),
    ]);
    let plan = repo.plan();
    let scopes = plan["scopes"].as_array().expect("scopes");
    assert_eq!(scopes.len(), 1, "{scopes:?}");
    assert!(
        scopes[0]["why"]
            .as_str()
            .is_some_and(|why| why.contains("can't place")),
        "{scopes:?}"
    );
    // Its scripts still run through hushrun, under the one scope.
    let wrapped = plan["scripts"]["wrapped"].as_array().expect("wrapped");
    assert!(
        wrapped
            .iter()
            .any(|w| w["after"] == "hushrun run -- vitest run"),
        "{wrapped:?}"
    );
}

// covers: C-14, C-15
#[test]
fn what_turbo_runs_runs_every_time_until_turbo_json_passes_hushruns_variables() {
    let repo = Repo::new(&[
        ("pnpm-workspace.yaml", "packages:\n  - \"packages/*\"\n"),
        (
            "package.json",
            r#"{"name": "monorepo", "private": true, "packageManager": "pnpm@10.0.0", "scripts": {"test": "turbo run test"}, "devDependencies": {"turbo": "^2.5.0"}}"#,
        ),
        // No list init can add to.
        (
            "turbo.json",
            r#"{"globalPassThroughEnv": "NODE_ENV", "tasks": {"test": {}}}"#,
        ),
        (
            "packages/a/package.json",
            r#"{"name": "a", "scripts": {"test": "vitest run"}}"#,
        ),
        (".github/workflows/ci.yml", WORKFLOW),
    ]);
    let plan = repo.plan();
    let edits = plan["edits"].as_array().expect("edits");
    assert!(edits.iter().all(|e| e["path"] != "turbo.json"), "{edits:?}");
    let notes: Vec<&str> = plan["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        notes.iter().any(|note| note.starts_with(
            "Change by hand: turbo.json: its \"globalPassThroughEnv\" isn't a list; add \"ACTIONS_ID_TOKEN_REQUEST_*\", \"GITHUB_*\", \"HUSHRUN_*\" to its \"globalPassThroughEnv\""
        )),
        "{notes:?}"
    );
    assert!(
        notes.contains(&"package.json \"test\" runs each package's test, which init wraps there, so it skips package by package, once turbo.json is changed by hand as below"),
        "{notes:?}"
    );
    // The job's run of the package's script gets none of hushrun's variables: it runs.
    let script = &plan["jobs"][0]["scripts"][0];
    assert_eq!(script["package"], "packages/a/package.json", "{script}");
    assert_eq!(
        (&script["ci"], &script["ciWhy"]),
        (&"runs".into(), &"cant-tell".into())
    );
    assert!(
        script["why"]
            .as_str()
            .is_some_and(|why| why.contains("which turbo.json doesn't pass on")),
        "{script}"
    );
    // A list gets them after its own, and the script skips.
    repo.write(
        "turbo.json",
        r#"{"globalPassThroughEnv": ["NODE_ENV"], "tasks": {"test": {}}}"#,
    );
    git::commit_all(&repo.repo, "a list");
    let plan = repo.plan();
    let turbo = plan["edits"]
        .as_array()
        .expect("edits")
        .iter()
        .find(|e| e["path"] == "turbo.json")
        .expect("turbo.json's edit");
    assert!(
        turbo["diff"].as_str().expect("a diff").contains(
            r#"+{"globalPassThroughEnv": ["NODE_ENV", "ACTIONS_ID_TOKEN_REQUEST_*", "GITHUB_*", "HUSHRUN_*"], "tasks": {"test": {}}}"#
        ),
        "{turbo}"
    );
    assert_eq!(plan["jobs"][0]["scripts"][0]["ci"], "skips");
}
