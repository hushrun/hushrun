//! The GitHub Actions provider, end to end (M6): run identity, the runner's outputs, summary
//! and annotations. \[ADR 0062\] One pipeline, on main: a plan never asks GitHub's REST API
//! about runs, which a fake on the loopback would answer.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use hushrun_testkit::world::World;
use serde_json::Value;

const MEMBER: u8 = 1; // a member's machine
const OTHER_MEMBER: u8 = 2;

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

/// A fake GitHub API: answers by path prefix, 404 otherwise, and records the paths asked.
struct FakeApi {
    base: String,
    asked: Arc<Mutex<Vec<String>>>,
}

impl FakeApi {
    fn start(routes: BTreeMap<String, String>) -> FakeApi {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let path = first.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let found = routes
                    .iter()
                    .find(|(prefix, _)| path.starts_with(prefix.as_str()));
                let (status, body) = match found {
                    Some((_, body)) => ("200 OK", body.clone()),
                    None => ("404 Not Found", "{}".to_string()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        FakeApi { base, asked }
    }
}

/// `hushrun` as a GitHub Actions job: run `run_id`, on `branch`, started by `event`.
fn on_github(
    w: &World,
    key: Option<u8>,
    run_id: &str,
    branch: &str,
    event: &str,
) -> assert_cmd::Command {
    let mut cmd = match key {
        Some(n) => w.hushrun_as(n),
        None => w.hushrun(),
    };
    cmd.env("GITHUB_ACTIONS", "true")
        .env("GITHUB_RUN_ID", run_id)
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_REPOSITORY", "Acme/api")
        .env("GITHUB_SERVER_URL", "https://github.com")
        .env("GITHUB_EVENT_NAME", event)
        .env("GITHUB_REF", format!("refs/heads/{branch}"))
        .env("GITHUB_SHA", w.head())
        .env("GITHUB_JOB", "plan")
        .env("GITHUB_OUTPUT", w.path(&format!("output-{run_id}")))
        .env("GITHUB_STEP_SUMMARY", w.path(&format!("summary-{run_id}")))
        .env("HUSHRUN_REPOSITORY_URL", w.ledger_url());
    cmd
}

fn read(w: &World, name: &str) -> String {
    std::fs::read_to_string(w.path(name)).unwrap_or_default()
}

// covers: T-4
#[test]
fn a_github_job_records_its_run_and_writes_the_runner_outputs() {
    let w = World::new("acme-api.yaml");
    let output = on_github(&w, Some(MEMBER), "100", "main", "push")
        .args(["plan", "--start", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan = json(&output.stdout);
    assert_eq!(plan["run"]["provider"], "github");
    assert_eq!(plan["run"]["id"], "100");
    assert_eq!(plan["run"]["trigger"], "push");
    assert_eq!(
        plan["run"]["url"],
        "https://github.com/Acme/api/actions/runs/100/attempts/1"
    );
    // The started event names the GitHub run.
    let started: Vec<String> = w
        .keys("v1/events/")
        .into_iter()
        .filter(|k| k.contains(".started."))
        .collect();
    let shown = json(
        &w.hushrun()
            .args(["repository", "show", &started[0], "--format", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(shown["statement"]["predicate"]["run"]["provider"], "github");
    assert_eq!(shown["statement"]["predicate"]["run"]["job"], "plan");
    // $GITHUB_OUTPUT gets the outputs, the json with a heredoc delimiter; the summary a table.
    let outputs = read(&w, "output-100");
    assert!(outputs.contains("mode=full\n"), "{outputs}");
    assert!(
        outputs.contains("json<<hushrun_EOF_") || outputs.contains("json={"),
        "{outputs}"
    );
    let summary = read(&w, "summary-100");
    assert!(
        summary.starts_with(
            "### hushrun plan\n\n**Building**, running the checks below, then deploying.\n\n| Mode | Reason |\n|---|---|\n| **full** | "
        ),
        "{summary}"
    );
    // --no-ci-output writes nothing to the runner.
    on_github(&w, None, "101", "main", "push")
        .args(["--no-ci-output", "digest"])
        .assert()
        .success();
    assert!(read(&w, "output-101").is_empty());
    on_github(&w, None, "102", "main", "push")
        .arg("digest")
        .assert()
        .success();
    assert!(read(&w, "output-102").contains("digest=sha256:"));
}

// covers: P-6, P-13
#[test]
fn a_run_without_an_outcome_lends_nothing_and_github_is_never_asked() {
    let w = World::new("acme-api.yaml");
    // Run 200 started this digest and built its image, then died without recording an outcome.
    let plan = w.path("plan-200.json");
    on_github(&w, Some(MEMBER), "200", "main", "push")
        .args(["plan", "--start", "--out"])
        .arg(&plan)
        .assert()
        .success();
    on_github(&w, Some(MEMBER), "200", "main", "push")
        .arg("--plan")
        .arg(&plan)
        .args(["attest", "--artifact", &format!("image={}", image())])
        .assert()
        .success();
    // GitHub's API would say it failed, and that another run of this tree is queued.
    let tree = hushrun_testkit::git::text(&w.repo, &["rev-parse", "HEAD^{tree}"]);
    let api = FakeApi::start(BTreeMap::from([
        (
            "/repos/Acme/api/actions/runs/200/attempts/1".to_string(),
            r#"{"id":200,"status":"completed","conclusion":"failure"}"#.to_string(),
        ),
        (
            "/repos/Acme/api/actions/workflows/".to_string(),
            format!(
                r#"{{"workflow_runs":[{{"id":555,"run_attempt":1,"status":"queued","conclusion":null,"html_url":"https://github.com/Acme/api/actions/runs/555","head_sha":"{}","head_commit":{{"tree_id":"{tree}"}}}}]}}"#,
                w.head()
            ),
        ),
    ]));
    let output = on_github(&w, None, "300", "main", "push")
        .env("GITHUB_API_URL", &api.base)
        .env("GITHUB_TOKEN", "test-token")
        .args(["plan", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan = json(&output.stdout);
    let decision = &plan["scopes"][0];
    // [ADR 0062] Only an attempt that recorded `passed` lends its image: run 200's isn't reused,
    // and no plan waits for run 555.
    assert_eq!(decision["mode"], "full", "{plan}");
    assert_eq!(decision["build"], true, "{plan}");
    assert!(
        decision["reason"]
            .as_str()
            .unwrap()
            .contains("recorded ones aren't reusable"),
        "{decision}"
    );
    assert!(decision.get("upstream").is_none(), "{decision}");
    // Nothing asked GitHub about runs: not their liveness, not which are queued.
    assert!(api.asked.lock().unwrap().is_empty(), "{:?}", api.asked);
    // So an API that doesn't answer changes nothing, and nothing names it or its token.
    let output = on_github(&w, None, "301", "main", "push")
        .env("GITHUB_API_URL", "http://127.0.0.1:9")
        .env("GITHUB_TOKEN", "test-token")
        .args(["plan", "--format", "json"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(json(&output.stdout)["scopes"][0]["mode"], "full");
    assert!(!stderr.contains("GitHub API"), "{stderr}");
    assert!(!stderr.contains("test-token"), "{stderr}");
}

// covers: C-2
#[test]
fn a_repository_mismatch_is_a_warning() {
    let w = World::new("acme-api.yaml");
    // The provider names another repository than hushrun.yaml.
    w.serve_origin_as("https://github.com/someone/else");
    let output = on_github(&w, None, "400", "main", "push")
        .env("GITHUB_REPOSITORY", "someone/else")
        .args(["plan"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("github.com/someone/else"));
}

/// `args` as GitHub job `run_id`: its exit code, its text output, and the step summary it wrote.
fn job(
    w: &World,
    key: Option<u8>,
    (run_id, branch, event): (&str, &str, &str),
    args: &[&str],
) -> (i32, String, String) {
    let output = on_github(w, key, run_id, branch, event)
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let summary = read(w, &format!("summary-{run_id}"));
    // What a person sees, for the docs: cargo test -- --nocapture.
    eprintln!(
        "--- {} (exit {:?})\n{stdout}--- summary\n{summary}",
        args.join(" "),
        output.status.code()
    );
    (output.status.code().unwrap_or(-1), stdout, summary)
}

/// GitHub run `run_id`, signing as test key `key`, plans, runs its checks, builds, deploys and
/// passes this digest.
fn passes(w: &World, key: u8, run_id: &str) {
    let plan = w.path(&format!("plan-{run_id}.json"));
    on_github(w, Some(key), run_id, "main", "push")
        .args(["plan", "--start", "--out"])
        .arg(&plan)
        .assert()
        .success();
    let image = format!("image={}", image());
    for args in [
        &[
            "attest",
            "--check",
            "toolchain=pass",
            "--check",
            "unit=pass",
        ][..],
        &["attest", "--artifact", &image],
        &["mark", "deployed"],
        &["mark", "passed"],
    ] {
        on_github(w, Some(key), run_id, "main", "push")
            .arg("--plan")
            .arg(&plan)
            .args(args)
            .assert()
            .success();
    }
}

/// An image at the artifact's location, pinned by digest.
fn image() -> String {
    format!(
        "us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        "a".repeat(64)
    )
}

// covers: P-10, P-14, P-15
#[test]
fn a_plan_of_the_default_scope_leads_with_what_it_does() {
    let w = World::new("acme-api.yaml");
    // Run 100 is still running this digest: a push's run 300 doesn't wait for it, and builds.
    on_github(&w, Some(MEMBER), "100", "main", "push")
        .args(["plan", "--start"])
        .assert()
        .success();
    let (code, text, summary) = job(&w, None, ("300", "main", "push"), &["plan"]);
    let reason = "no reusable `image` (none was recorded): building and running toolchain (missing), unit (missing)";
    assert_eq!(code, 0);
    assert_eq!(
        text,
        format!(
            "full: {reason}\n  checks to run: toolchain unit\n  requirement toolchain            missing\n  requirement unit                 missing\n"
        )
    );
    assert_eq!(
        summary,
        format!(
            "### hushrun plan\n\n**Building**, running the checks below, then deploying.\n\n| Mode | Reason |\n|---|---|\n| **full** | {reason} |\n"
        )
    );

    // Run 100 passes. A push of the same files finds them deployed: nothing to deploy (P-10).
    passes(&w, MEMBER, "100");
    let (code, text, summary) = job(&w, None, ("301", "main", "push"), &["plan"]);
    let digest = text
        .split_whitespace()
        .find(|word| word.starts_with("sha256:"))
        .unwrap()
        .to_string();
    let deployed_at = text
        .split("(deployed at ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .unwrap()
        .to_string();
    let reason = format!(
        "this repository already runs {digest} (deployed at {deployed_at}); to deploy it again, re-run it or start it by hand"
    );
    assert_eq!(code, 0);
    assert_eq!(
        text,
        format!(
            "current: {reason}\n  requirement toolchain            satisfied\n  requirement unit                 satisfied\n"
        )
    );
    assert_eq!(
        summary,
        format!(
            "### hushrun plan\n\n**Nothing to deploy.** This repository already runs this code.\n\n| Mode | Reason |\n|---|---|\n| **current** | {reason} |\n"
        )
    );

    // Started by hand, it deploys the image run 100 built and checked, by digest (P-14).
    let (code, text, summary) = job(&w, None, ("302", "main", "workflow_dispatch"), &["plan"]);
    let image = image();
    let reason = format!("every required check is met: deploying image {image} by digest");
    assert_eq!(code, 0);
    assert_eq!(
        text,
        format!(
            "promote: {reason}\n  image: {image}\n  requirement toolchain            satisfied\n  requirement unit                 satisfied\n"
        )
    );
    assert_eq!(
        summary,
        format!(
            "### hushrun plan\n\n**Deploying** the image an earlier run built and checked, by digest. Nothing is rebuilt or checked again.\n\n| Mode | Reason |\n|---|---|\n| **promote** | {reason} |\n"
        )
    );
}

// covers: P-5, P-13, T-16
#[test]
fn in_a_ci_only_repository_a_laptops_run_lends_nothing() {
    let w = World::new("acme-api.yaml");
    w.fake
        .set_repository_ci_only(hushrun_core::time::Time::parse("2026-01-01T00:00:00Z").unwrap());
    // A member's laptop records a run under a GitHub run's id: its `started`, its checks and its
    // image. The repository takes CI's evidence only (T-16), so none of it counts.
    let plan = w.path("plan-555.json");
    let laptop = || {
        let mut cmd = on_github(&w, Some(OTHER_MEMBER), "555", "main", "push");
        cmd.arg("--plan").arg(&plan);
        cmd
    };
    on_github(&w, Some(OTHER_MEMBER), "555", "main", "push")
        .args(["plan", "--start", "--out"])
        .arg(&plan)
        .assert()
        .success();
    laptop()
        .args([
            "attest",
            "--check",
            "toolchain=pass",
            "--check",
            "unit=pass",
        ])
        .assert()
        .success();
    laptop()
        .args(["attest", "--artifact", &format!("image={}", image())])
        .assert()
        .success();
    assert!(!w.keys("v1/events/").is_empty());
    // Its `started` makes no attempt (P-5), so it has none to deploy in.
    laptop()
        .args(["mark", "deployed"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "doesn't count: the repository takes CI's evidence only",
        ));
    // CI's plan reuses none of it (P-13), and runs every check.
    let output = on_github(&w, None, "300", "main", "push")
        .args(["plan", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan = json(&output.stdout);
    let decision = &plan["scopes"][0];
    assert_eq!(decision["mode"], "full", "{plan}");
    let statuses: Vec<&str> = decision["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["missing", "missing"], "{decision}");
}

// covers: L-5, P-8, C-7
#[test]
fn on_repository_error_full_still_keeps_deploys_to_their_branch() {
    let w = World::new("acme-api.yaml");
    let missing = format!("{}/missing", w.ledger_url());
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let opted_in = policy.replace(&w.ledger_url(), &missing).replace(
        "deploy:\n  branch: main\n",
        "deploy:\n  branch: main\n  on_repository_error: full\n",
    );
    assert!(opted_in.contains("on_repository_error: full"), "{opted_in}");
    w.write("hushrun.yaml", &opted_in);
    w.commit("opt in, where nothing answers");
    let plan = |branch: &str| {
        on_github(&w, Some(MEMBER), "400", branch, "push")
            .env("HUSHRUN_REPOSITORY_URL", &missing)
            .args(["plan", "--format", "json"])
            .output()
            .unwrap()
    };
    // On the branch it deploys from, the unreadable repository means a full run.
    let output = plan("main");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json(&output.stdout)["scopes"][0]["mode"], "full");
    // On another branch it's blocked, exactly as when the repository answers (P-8).
    let output = plan("develop");
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decision = &json(&output.stdout)["scopes"][0];
    assert_eq!(decision["mode"], "blocked", "{decision}");
    assert_eq!(decision["build"], false);
    assert_eq!(decision["checksToRun"], serde_json::json!([]));
    assert!(
        decision["reason"]
            .as_str()
            .unwrap()
            .contains("deploys only from main"),
        "{decision}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("::error title=hushrun plan::"), "{stderr}");
}
