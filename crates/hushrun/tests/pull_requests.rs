//! M8: pull-request pipelines read hushrun.yaml from the branch they merge into (T-5,
//! `--config-ref`), and every command that computes digests takes `--worktree` (D-10).

use hushrun_testkit::git;
use hushrun_testkit::world::World;
use serde_json::Value;

const MEMBER: u8 = 1; // a member's machine

fn output(cmd: &mut assert_cmd::Command) -> (i32, Value, String) {
    let out = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out.status.code().unwrap_or(-1), value, stderr)
}

/// The world's develop branch also as `main`, then a change on develop that rewrites
/// hushrun.yaml: its unit check runs nothing, and its scope leaves the service out.
fn world_with_a_policy_change() -> World {
    let w = World::new("acme-api.yaml");
    git::run(&w.repo, &["branch", "main"], None);
    let reviewed = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let changed = reviewed
        .replace("run: go test ./...", "run: \"true\"")
        .replace(
            "# No `scopes:` block",
            "scopes:\n  default:\n    include: [\"go.mod\"]\n# Was: no `scopes:` block",
        );
    assert_ne!(changed, reviewed);
    w.write("hushrun.yaml", &changed);
    w.commit("a pull request that rewrites its own policy");
    w
}

/// A pull request's job on GitHub Actions.
fn pull_request(w: &World, event: &str) -> assert_cmd::Command {
    let mut cmd = w.hushrun_as(MEMBER);
    cmd.env("GITHUB_ACTIONS", "true")
        .env("GITHUB_EVENT_NAME", event)
        .env("GITHUB_BASE_REF", "main")
        .env("GITHUB_REF", "refs/pull/7/merge")
        .env("GITHUB_REPOSITORY", "Acme/api")
        .env("GITHUB_RUN_ID", "4242")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("HUSHRUN_NO_CI_OUTPUT", "true")
        .env("HUSHRUN_REPOSITORY_URL", w.ledger_url());
    cmd
}

fn digest(w: &World, args: &[&str]) -> String {
    let (code, out, stderr) = output(w.hushrun().args(["digest", "--format", "json"]).args(args));
    assert_eq!(code, 0, "{stderr}");
    out["scopes"][0]["digest"].as_str().unwrap().to_string()
}

// covers: T-5
#[test]
fn config_ref_reads_hushrun_yaml_from_another_revision() {
    let w = world_with_a_policy_change();
    // The scope the pull request wrote covers go.mod only; main's covers everything.
    let own = digest(&w, &[]);
    let reviewed = digest(&w, &["--config-ref", "main"]);
    assert_ne!(own, reviewed);
    let (_, plan, stderr) =
        output(
            w.hushrun_as(MEMBER)
                .args(["plan", "--config-ref", "main", "--format", "json"]),
        );
    assert_eq!(plan["scopes"][0]["digest"], reviewed, "{stderr}");
    // The definitions come from main too: the pull request's `run: true` isn't what is required.
    let unit = |args: &[&str]| {
        let (code, out, stderr) = output(
            w.hushrun()
                .args(["policy", "explain", "--format", "json"])
                .args(args),
        );
        assert_eq!(code, 0, "{stderr}");
        out["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["requirement"] == "unit")
            .unwrap()["definitionDigest"]
            .clone()
    };
    let on_main = {
        git::run(&w.repo, &["checkout", "--quiet", "main"], None);
        let digest = unit(&[]);
        git::run(&w.repo, &["checkout", "--quiet", "develop"], None);
        digest
    };
    assert_eq!(unit(&["--config-ref", "main"]), on_main);
    assert_ne!(unit(&[]), on_main);

    // Refs that aren't refs, and a ref without the file, are configuration errors.
    for bad in ["-x", "main:hushrun.yaml", "main\nother"] {
        let (code, _, stderr) = output(w.hushrun().args(["digest", "--config-ref", bad]));
        assert_eq!(code, 2, "{bad:?}: {stderr}");
    }
    let (code, _, stderr) = output(
        w.hushrun()
            .args(["digest", "--config-ref", "no-such-branch"]),
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--config-ref"), "{stderr}");
    // --config and --config-ref are two answers to one question.
    let (code, _, _) =
        output(
            w.hushrun()
                .args(["digest", "--config", "hushrun.yaml", "--config-ref", "main"]),
        );
    assert_eq!(code, 2);
}

// covers: T-5
#[test]
fn pull_request_pipelines_must_read_the_base_branchs_policy() {
    let w = world_with_a_policy_change();
    for event in ["pull_request", "pull_request_target"] {
        let (code, _, stderr) = output(pull_request(&w, event).args(["plan"]));
        assert_eq!(code, 2, "{event}: {stderr}");
        assert!(stderr.contains("--config-ref origin/main"), "{stderr}");
        assert!(
            stderr.contains("hushrun.yaml comes from the branch it merges into"),
            "{stderr}"
        );
    }
    // A ref that isn't the base branch is refused too: HEAD is the change itself.
    let (code, _, stderr) =
        output(pull_request(&w, "pull_request").args(["plan", "--config-ref", "HEAD"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("isn't the branch this pull request merges into"),
        "{stderr}"
    );
    let (code, out, stderr) = output(pull_request(&w, "pull_request").args([
        "plan",
        "--config-ref",
        "main",
        "--format",
        "json",
    ]));
    assert!(code == 0 || code == 3, "{stderr}");
    assert_eq!(out["schema"], "hushrun.plan/v2", "{stderr}");
    // Pushes read the policy of the revision they evaluate, as before.
    let (code, _, stderr) = output(pull_request(&w, "push").args(["plan"]));
    assert!(code == 0 || code == 3, "{stderr}");
    // Commands that touch no ledger, such as validating the pull request's own policy, may.
    let (code, _, stderr) = output(pull_request(&w, "pull_request").args(["policy", "validate"]));
    assert_eq!(code, 0, "{stderr}");
}

// covers: T-5
#[test]
fn gitlab_merge_request_pipelines_must_too() {
    let w = world_with_a_policy_change();
    let (code, _, stderr) = output(
        w.hushrun_as(MEMBER)
            .env("GITLAB_CI", "true")
            .env("CI_PIPELINE_SOURCE", "merge_request_event")
            .env("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", "main")
            .env("HUSHRUN_RUN_ID", "77")
            .env("HUSHRUN_REPOSITORY_URL", w.ledger_url())
            .args(["plan"]),
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--config-ref origin/main"), "{stderr}");
}

// covers: D-10
#[test]
fn every_command_that_digests_takes_the_worktree() {
    let w = World::new("acme-api.yaml");
    w.write("main.go", "package main // not committed yet\n");
    let worktree = digest(&w, &["--worktree"]);
    assert_ne!(worktree, digest(&w, &[]));
    let (_, plan, stderr) =
        output(
            w.hushrun_as(MEMBER)
                .args(["plan", "--worktree", "--format", "json"]),
        );
    assert_eq!(plan["scopes"][0]["digest"], worktree, "{stderr}");
    let (code, out, stderr) = output(w.hushrun_as(MEMBER).env("HUSHRUN_RUN_ID", "r1").args([
        "attest",
        "--worktree",
        "--check",
        "unit=pass",
        "--format",
        "json",
    ]));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(out["digest"], worktree, "{out}");
    let (code, out, stderr) = output(w.hushrun_as(MEMBER).env("HUSHRUN_RUN_ID", "r1").args([
        "mark",
        "failed",
        "--worktree",
        "--reason",
        "x",
        "--format",
        "json",
    ]));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(out["digest"], worktree);
    let (code, out, stderr) =
        output(
            w.hushrun()
                .args(["status", "--worktree", "--format", "json"]),
        );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(out["scopes"][0]["digest"], worktree);
    // Without a plan, artifact resolve decides for the worktree too: nothing was built for it.
    let (code, out, stderr) =
        output(
            w.hushrun()
                .args(["artifact", "resolve", "--worktree", "--format", "json"]),
        );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(out["build"], true, "{out}");
    // --worktree and --rev, or --digest, are two answers to one question.
    let zeros = format!("sha256:{}", "0".repeat(64));
    for args in [
        vec!["plan", "--worktree", "--rev", "HEAD"],
        vec!["mark", "failed", "--worktree", "--digest", &zeros],
    ] {
        let (code, _, _) = output(w.hushrun_as(MEMBER).args(&args));
        assert_eq!(code, 2, "{args:?}");
    }
}
