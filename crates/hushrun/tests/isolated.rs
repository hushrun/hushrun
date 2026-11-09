//! M8, end to end: `run --isolated` (E-12) through a fake container runtime, and \[ADR 0031\]
//! an agent's and a laptop's evidence counting where it counts (T-15, T-16). The fake runtime
//! runs each check on the host, in the export, with only the variables hushrun passed in; what it
//! can't fake (the network, the image) is checked in its calls. \[ADR 0062\] One pipeline: the
//! checks run without a stage, and CI-only is the repository's one switch.
#![cfg(unix)]

use hushrun_testkit::container::{FakeRuntime, option};
use hushrun_testkit::schema;
use hushrun_testkit::world::{World, test_key};
use serde_json::Value;

const CI: u8 = 1;
const AGENT: u8 = 3; // an agent token's machine, agent:fleet-a
const LAPTOP: u8 = 5; // a member's laptop

const IMAGE: &str =
    "golang@sha256:1111111111111111111111111111111111111111111111111111111111111111";

/// A repository whose one scope is the API, with checks pinned to an image and a deploy that
/// requires `unit`; its server names an agent's machine and a member's laptop besides the
/// world's members (T-15).
fn world(checks: &str) -> (World, FakeRuntime) {
    let w = World::new("acme-api.yaml");
    let ledger = w.ledger_lines();
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/acme/shop
{ledger}
scopes:
  api:
    include: [\"services/api/**\", \"go.mod\", \"tools/**\"]
digest:
  exclude: [\"**/*.md\"]
checks:
{checks}
deploy:
  branch: main
  require: [unit]
"
        ),
    );
    w.write("services/api/README.md", "excluded from the digest\n");
    w.write("tools/build.sh", "#!/bin/sh\necho built\n");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            w.repo.join("tools/build.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    w.commit("checks");
    w.fake
        .register_machine(&test_key(AGENT).public_key(), "agent:fleet-a", false);
    w.fake.register_machine(
        &test_key(LAPTOP).public_key(),
        "user:laptop@acme.dev",
        false,
    );
    let runtime = FakeRuntime::install(&w.root);
    (w, runtime)
}

/// The unit check, pinned to `IMAGE`, running `script`.
fn unit(script: &str, more: &str) -> String {
    format!(
        "  unit:\n    image: \"{IMAGE}\"\n    platform: linux/amd64\n    run: {}\n{more}",
        serde_json::to_string(script).unwrap()
    )
}

fn hushrun(w: &World, rt: &FakeRuntime, key: u8) -> assert_cmd::Command {
    let mut cmd = w.hushrun_as(key);
    cmd.env("HUSHRUN_CONTAINER_RUNTIME", &rt.path)
        .env("HUSHRUN_EXPORT_DIR", w.root.join("exports"));
    cmd
}

/// A command as CI runs it: a generic provider, the ledger from the environment.
fn ci(w: &World, key: u8, run_id: &str) -> assert_cmd::Command {
    let mut cmd = w.hushrun_as(key);
    cmd.env("CI", "true")
        .env("HUSHRUN_PROVIDER", "generic")
        .env("HUSHRUN_REPOSITORY_URL", w.ledger_url())
        .env("HUSHRUN_RUN_ID", run_id);
    cmd
}

fn output(cmd: &mut assert_cmd::Command) -> (i32, Value, String) {
    let out = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out.status.code().unwrap_or(-1), value, stderr)
}

fn show(w: &World, key: &str) -> Value {
    let out = w
        .hushrun()
        .args(["repository", "show", key, "--format", "json"])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

/// `plan` in CI, as JSON.
fn plan(w: &World, key: u8, run_id: &str) -> Value {
    let (code, value, stderr) = output(ci(w, key, run_id).args(["plan", "--format", "json"]));
    assert!(code == 0 || code == 3, "{stderr}");
    value
}

fn checks_to_run(plan: &Value) -> Vec<(String, String)> {
    plan["scopes"][0]["checksToRun"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["check"].as_str().unwrap().to_string(),
                c["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

// covers: E-12, D-10
#[test]
fn isolated_runs_see_exactly_the_digested_entries() {
    let (w, rt) = world(&unit(
        "set -e; find . | sort >&2; test -x tools/build.sh; test ! -x go.mod; test ! -e services/api/README.md; test ! -e web; test ! -e .git",
        "",
    ));
    // Uncommitted, and part of what --worktree exports.
    w.write("services/api/new.go", "package api\n");
    let (code, out, stderr) = output(hushrun(&w, &rt, AGENT).args([
        "run",
        "unit",
        "--isolated",
        "--worktree",
        "--format",
        "json",
    ]));
    assert_eq!(code, 0, "{stderr}");
    schema::assert_valid(&schema::load("run.v2.schema.json"), &out);
    assert_eq!(out["isolated"], true);
    assert_eq!(out["checks"][0]["result"], "pass", "{stderr}");
    for path in [
        "./go.mod",
        "./services/api/main.go",
        "./services/api/new.go",
        "./tools/build.sh",
    ] {
        assert!(stderr.contains(path), "{path} not exported:\n{stderr}");
    }
    assert!(!stderr.contains("./main.go\n"), "{stderr}");
    // The evidence is about the worktree's digest, the one `digest --worktree` prints.
    let digest = String::from_utf8(
        w.hushrun()
            .args(["digest", "--worktree", "--format", "json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let digest: Value = serde_json::from_str(&digest).unwrap();
    assert_eq!(out["digest"], digest["scopes"][0]["digest"]);
    // The export is gone afterwards.
    let left: Vec<_> = std::fs::read_dir(w.root.join("exports")).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

// covers: E-12, E-7, E-4
#[test]
fn isolated_evidence_records_its_environment_and_custody() {
    let (w, rt) = world(&unit("true", ""));
    let (code, out, stderr) =
        output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated", "--format", "json"]));
    assert_eq!(code, 0, "{stderr}");
    // The container is labeled, has an init process, and is removed once inspected, never by
    // `--rm`.
    let run = &rt.runs()[0];
    assert_eq!(option(run, "--label"), Some("com.hushrun.check=unit"));
    assert!(run.iter().any(|a| a == "--init"), "{run:?}");
    assert!(!run.iter().any(|a| a == "--rm"), "{run:?}");
    assert!(rt.containers().is_empty(), "{:?}", rt.containers());
    let shown = show(&w, out["checks"][0]["event"].as_str().unwrap());
    let predicate = &shown["statement"]["predicate"];
    assert_eq!(predicate["producer"]["custody"], "isolated");
    let environment = &predicate["check"]["environment"];
    assert_eq!(environment["image"], IMAGE);
    assert_eq!(environment["platform"], "linux/amd64");
    assert_eq!(environment["network"], "none");
    assert_eq!(
        environment["runner"],
        "hushrun run --isolated (fake-docker 99.1.0)"
    );
    // The digest of the environment's JCS form (E-7).
    assert!(
        predicate["check"]["environmentDigest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    // An operator's ephemeral runner that holds the key claims sandbox; isolated stays local
    // anywhere else.
    let (_, out, _) = output(
        hushrun(&w, &rt, AGENT)
            .env("HUSHRUN_CUSTODY", "sandbox")
            .args(["run", "unit", "--isolated", "--force", "--format", "json"]),
    );
    assert_eq!(out["checks"][0]["custody"], "sandbox");
    let (code, out, stderr) = output(
        hushrun(&w, &rt, AGENT)
            .env("HUSHRUN_CUSTODY", "isolated")
            .args(["run", "unit", "--force", "--format", "json"])
            .args(["--environment-image", IMAGE]),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(out["checks"][0]["custody"], "local");
    assert!(stderr.contains("recording custody local"), "{stderr}");
}

// covers: E-12
#[test]
fn only_the_checks_variables_go_in_and_never_secrets() {
    let (w, rt) = world(&unit(
        "env >&2; test \"$GO_VERSION\" = 1.23",
        "    env: [GO_VERSION]\n",
    ));
    let (code, _, stderr) = output(
        hushrun(&w, &rt, AGENT)
            .env("GO_VERSION", "1.23")
            .env("UNRELATED", "from the host")
            .args(["run", "unit", "--isolated"]),
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("GO_VERSION=1.23"), "{stderr}");
    for absent in [
        "UNRELATED",
        "HUSHRUN_SIGNING_KEY",
        "HOME=",
        "HUSHRUN_CACHE_DIR",
    ] {
        assert!(!stderr.contains(absent), "{absent} went in:\n{stderr}");
    }
    // Values never reach the runtime's command line.
    let run = &rt.runs()[0];
    assert_eq!(option(run, "--env"), Some("GO_VERSION"));
    assert!(!run.iter().any(|a| a.starts_with("GO_VERSION=")), "{run:?}");

    // A check that names a secret doesn't run isolated at all.
    let (w, rt) = world(&unit("true", "    env: [GITHUB_TOKEN]\n"));
    let (code, _, stderr) = output(
        hushrun(&w, &rt, AGENT)
            .env("GITHUB_TOKEN", "ghs_secret")
            .args(["run", "unit", "--isolated"]),
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("never passes secrets in"), "{stderr}");
    assert!(rt.runs().is_empty());
    assert!(w.keys("v1/events/").is_empty());
}

// covers: E-12, C-5
#[test]
fn the_network_is_off_unless_a_check_declares_one() {
    let (w, rt) = world(&format!(
        "{}{}{}",
        unit("true", ""),
        "  lint:\n    image: \"golang@sha256:2222222222222222222222222222222222222222222222222222222222222222\"\n    run: \"true\"\n    network: open\n",
        "  fetch:\n    image: \"golang@sha256:3333333333333333333333333333333333333333333333333333333333333333\"\n    run: \"true\"\n    network: restricted\n",
    ));
    let (code, _, stderr) =
        output(hushrun(&w, &rt, AGENT).args(["run", "unit", "lint", "--isolated"]));
    assert_eq!(code, 0, "{stderr}");
    let runs = rt.runs();
    assert_eq!(option(&runs[0], "--network"), Some("none"));
    assert_eq!(option(&runs[0], "--platform"), Some("linux/amd64"));
    assert_eq!(
        option(&runs[0], "--security-opt"),
        Some("no-new-privileges")
    );
    assert!(runs[0].contains(&IMAGE.to_string()), "{:?}", runs[0]);
    assert_eq!(
        option(&runs[1], "--network"),
        None,
        "open uses the runtime's network"
    );
    // hushrun can't restrict a network itself: the operator names one.
    let (code, _, stderr) = output(hushrun(&w, &rt, AGENT).args(["run", "fetch", "--isolated"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("HUSHRUN_RESTRICTED_NETWORK"), "{stderr}");
    rt.clear();
    let (code, _, stderr) = output(
        hushrun(&w, &rt, AGENT)
            .env("HUSHRUN_RESTRICTED_NETWORK", "egress-filtered")
            .args(["run", "fetch", "--isolated"]),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(option(&rt.runs()[0], "--network"), Some("egress-filtered"));
}

// covers: E-12
#[test]
fn checks_without_a_pinned_image_cannot_run_isolated() {
    let (w, rt) = world("  unit:\n    run: \"true\"\n");
    let (code, _, stderr) = output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated"]));
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("pins no `image`"), "{stderr}");
    // --environment-image is for jobs that run in the image themselves.
    let (code, _, stderr) = output(hushrun(&w, &rt, AGENT).args([
        "run",
        "unit",
        "--isolated",
        "--environment-image",
        IMAGE,
    ]));
    assert_eq!(code, 2, "{stderr}");
}

// covers: E-12
#[test]
fn the_runtimes_own_failures_record_nothing() {
    let (w, rt) = world(&unit("true", ""));
    rt.set("run-fails", true);
    let (code, _, stderr) = output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated"]));
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("nothing was recorded"), "{stderr}");
    assert!(w.keys("v1/events/").is_empty());
    rt.set("run-fails", false);
    // A missing image is pulled for the pinned platform; a failed pull records nothing.
    rt.set("absent", true);
    rt.set("pull-fails", true);
    let (code, _, stderr) = output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated"]));
    assert_eq!(code, 1, "{stderr}");
    assert!(w.keys("v1/events/").is_empty());
    rt.set("pull-fails", false);
    rt.clear();
    let (code, _, stderr) = output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated"]));
    assert_eq!(code, 0, "{stderr}");
    let pull = rt
        .calls()
        .into_iter()
        .find(|c| c[0] == "pull")
        .expect("a pull");
    assert_eq!(option(&pull, "--platform"), Some("linux/amd64"));
    assert_eq!(pull.last().map(String::as_str), Some(IMAGE));
}

// covers: E-12
#[test]
fn a_check_that_exits_125_itself_fails_and_is_recorded() {
    // The CLI answers 125 for the check too: the container says the check started and ended.
    let (w, rt) = world(&unit("exit 125", ""));
    let (code, out, stderr) =
        output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated", "--format", "json"]));
    assert_eq!(code, 8, "{stderr}");
    assert_eq!(out["checks"][0]["result"], "fail");
    let shown = show(&w, out["checks"][0]["event"].as_str().unwrap());
    assert_eq!(shown["statement"]["predicate"]["check"]["exitCode"], 125);
    assert!(
        rt.calls()
            .iter()
            .any(|c| c.len() > 1 && c[0] == "container" && c[1] == "inspect"),
        "{:?}",
        rt.calls()
    );
    assert!(rt.containers().is_empty(), "{:?}", rt.containers());
}

/// Starts `cmd` without waiting for it, with its output captured.
fn spawn(cmd: &assert_cmd::Command) -> std::process::Child {
    let mut std = std::process::Command::new(cmd.get_program());
    std.args(cmd.get_args());
    for (key, value) in cmd.get_envs() {
        match value {
            Some(value) => std.env(key, value),
            None => std.env_remove(key),
        };
    }
    if let Some(dir) = cmd.get_current_dir() {
        std.current_dir(dir);
    }
    std.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start hushrun")
}

// covers: E-12
#[test]
fn a_stop_signal_removes_the_container_and_the_export_and_records_nothing() {
    use std::os::unix::process::ExitStatusExt as _;
    use std::time::{Duration, Instant};
    for (signal, number) in [("TERM", 15), ("INT", 2), ("HUP", 1)] {
        let (w, rt) = world(&unit("sleep 30", ""));
        let mut cmd = hushrun(&w, &rt, AGENT);
        cmd.args(["run", "unit", "--isolated"]);
        let child = spawn(&cmd);
        let deadline = Instant::now() + Duration::from_secs(30);
        while rt.containers().is_empty() {
            assert!(Instant::now() < deadline, "the check never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let sent = Instant::now();
        let status = std::process::Command::new("kill")
            .args([format!("-{signal}"), child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        // hushrun ends as the signal asked, without waiting for the check.
        assert_eq!(out.status.signal(), Some(number), "{signal}: {stderr}");
        assert!(
            sent.elapsed() < Duration::from_secs(15),
            "{signal}: {stderr}"
        );
        assert!(stderr.contains("interrupted"), "{signal}: {stderr}");
        assert!(rt.calls().iter().any(|c| c[0] == "rm"), "{:?}", rt.calls());
        assert!(rt.containers().is_empty(), "{:?}", rt.containers());
        let left: Vec<_> = std::fs::read_dir(w.root.join("exports")).unwrap().collect();
        assert!(left.is_empty(), "{signal}: {left:?}");
        assert!(w.keys("v1/events/").is_empty(), "{signal}");
    }
}

// covers: E-12
#[test]
fn what_runs_that_did_not_end_left_is_removed() {
    let (w, rt) = world(&unit("true", ""));
    let exports = w.root.join("exports");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let export = |ago_ms: i64| {
        let dir = exports.join(format!(
            "hushrun-{}",
            hushrun_ledger::new_ulid(now - ago_ms).to_lowercase()
        ));
        std::fs::create_dir_all(dir.join("services")).unwrap();
        std::fs::write(dir.join("services/main.go"), "package main\n").unwrap();
        dir
    };
    let killed = export(25 * 3600 * 1000);
    let running = export(3600 * 1000);
    let unrelated = exports.join("hushrun-notes");
    std::fs::create_dir_all(&unrelated).unwrap();
    let (code, _, stderr) = output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated"]));
    assert_eq!(code, 0, "{stderr}");
    assert!(!killed.exists(), "a day-old export stays");
    assert!(running.exists(), "another run's export is gone");
    assert!(unrelated.exists());
    // Stopped containers older than a day, of isolated runs only.
    let prune = rt
        .calls()
        .into_iter()
        .find(|c| c.len() > 1 && c[0] == "container" && c[1] == "prune")
        .expect("a prune");
    assert!(
        prune.contains(&"label=com.hushrun.check".to_string())
            && prune.contains(&"until=24h".to_string()),
        "{prune:?}"
    );
}

// covers: E-12
#[test]
fn the_signing_key_never_goes_in() {
    let (w, rt) = world(&unit("true", ""));
    // An agent that keeps its key in the repository, where the scope covers it: --worktree
    // would export it, and the commit would hold it.
    w.write("tools/agent.pem", test_key(AGENT).to_pem().as_str());
    let in_repo = w.repo.join("tools/agent.pem");
    let (code, _, stderr) = output(
        hushrun(&w, &rt, AGENT)
            .env("HUSHRUN_SIGNING_KEY_FILE", &in_repo)
            .args(["run", "unit", "--isolated", "--worktree"]),
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("tools/agent.pem") && stderr.contains("outside the repository"),
        "{stderr}"
    );
    assert!(rt.runs().is_empty());
    assert!(w.keys("v1/events/").is_empty());
    // Outside the repository, it stays with hushrun.
    let outside = w.root.join("agent.pem");
    std::fs::rename(&in_repo, &outside).unwrap();
    let (code, _, stderr) = output(
        hushrun(&w, &rt, AGENT)
            .env("HUSHRUN_SIGNING_KEY_FILE", &outside)
            .args(["run", "unit", "--isolated", "--worktree"]),
    );
    assert_eq!(code, 0, "{stderr}");
}

// covers: E-12
#[test]
fn timeouts_remove_the_container_and_retries_start_from_a_fresh_export() {
    let (w, rt) = world(&unit("sleep 30", "    timeout: 1s\n"));
    let (code, out, stderr) =
        output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated", "--format", "json"]));
    assert_eq!(code, 8, "{stderr}");
    assert_eq!(out["checks"][0]["result"], "fail");
    assert!(rt.calls().iter().any(|c| c[0] == "rm"), "{:?}", rt.calls());
    assert!(rt.containers().is_empty(), "{:?}", rt.containers());

    // Each attempt sees the export as it was digested: a file the first attempt left is gone.
    let (w, rt) = world(&unit(
        "if [ -e left-over ]; then exit 0; fi; touch left-over; exit 1",
        "    retries: 1\n",
    ));
    let (code, out, stderr) =
        output(hushrun(&w, &rt, LAPTOP).args(["run", "unit", "--isolated", "--format", "json"]));
    assert_eq!(code, 8, "{stderr}");
    assert_eq!(out["checks"][0]["attempts"], 2);
}

// covers: E-12
#[test]
fn an_isolated_run_exports_the_content_its_evidence_names() {
    let (w, rt) = world(&unit("true", ""));
    let committed = String::from_utf8(
        w.hushrun()
            .args(["digest", "--format", "json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let committed: Value = serde_json::from_str(&committed).unwrap();
    let digest = committed["scopes"][0]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    // Another digest than the checkout's: the export would be other content.
    let other = format!("sha256:{}", "e".repeat(64));
    let (code, _, stderr) =
        output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated", "--digest", &other]));
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("an isolated run exports this checkout"),
        "{stderr}"
    );
    let (code, _, stderr) =
        output(hushrun(&w, &rt, AGENT).args(["run", "unit", "--isolated", "--digest", &digest]));
    assert_eq!(code, 0, "{stderr}");
}

// covers: P-1, T-15, T-16, E-8
#[test]
fn agent_evidence_makes_ci_skip_its_checks_and_local_evidence_counts_only_where_accepted() {
    let (w, rt) = world(&unit("true", ""));
    // The agent validates before pushing, as the AGENTS.md snippet says.
    let (code, _, stderr) =
        output(hushrun(&w, &rt, AGENT).args(["run", "--all", "--isolated", "--worktree"]));
    assert_eq!(code, 0, "{stderr}");
    // Again: nothing lacks evidence, so nothing runs, and no runtime is needed.
    let (code, out, stderr) = output(
        hushrun(&w, &rt, AGENT)
            .env("HUSHRUN_CONTAINER_RUNTIME", w.path("no-such-runtime"))
            .args([
                "run",
                "--all",
                "--isolated",
                "--worktree",
                "--format",
                "json",
            ]),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(out["checks"][0]["result"], "skipped", "{out}");
    // CI's plan skips unit: an agent of the org counts, with isolated custody.
    let planned = plan(&w, CI, "ci-1");
    assert_eq!(
        checks_to_run(&planned),
        Vec::<(String, String)>::new(),
        "{planned}"
    );
    assert_eq!(
        planned["scopes"][0]["requirements"][0]["status"],
        "satisfied"
    );

    // A laptop's local evidence counts while the repository takes every signer that counts,
    // and still after a CI-only period that ended before it was made; not once the server names
    // the repository CI-only (T-16), as its admins set it in the dashboard.
    let (w, rt) = world(&unit("true", ""));
    let (code, _, stderr) =
        output(hushrun(&w, &rt, LAPTOP).args(["run", "unit", "--environment-image", IMAGE]));
    assert_eq!(code, 0, "{stderr}");
    let planned = plan(&w, CI, "ci-2");
    assert!(checks_to_run(&planned).is_empty(), "{planned}");
    let time = |t: &str| hushrun_core::time::Time::parse(t).unwrap();
    w.fake.add_repository_ci_only_period(
        time("2026-01-01T00:00:00Z"),
        Some(time("2026-01-02T00:00:00Z")),
    );
    let planned = plan(&w, CI, "ci-3");
    assert!(checks_to_run(&planned).is_empty(), "{planned}");
    w.fake.set_repository_ci_only(time("2026-01-01T00:00:00Z"));
    let planned = plan(&w, CI, "ci-4");
    assert_eq!(
        planned["scopes"][0]["requirements"][0]["status"], "missing",
        "{planned}"
    );
    assert_eq!(checks_to_run(&planned), [("unit".into(), "missing".into())]);

    // A key the server doesn't list: ignored everywhere.
    let (w, rt) = world(&unit("true", ""));
    let (code, _, stderr) = output(hushrun(&w, &rt, 7).args(["run", "unit", "--isolated"]));
    assert_eq!(code, 0, "{stderr}");
    let planned = plan(&w, CI, "ci-5");
    assert_eq!(checks_to_run(&planned), [("unit".into(), "missing".into())]);
}
