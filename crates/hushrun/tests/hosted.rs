//! `https://` ledgers end to end (L-10): the CLI against the testkit's fake HTTP ledger on the
//! loopback (`http://127.0.0.1`, which L-1 allows for local development).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hushrun_testkit::fake_ledger::{FakeLedger, Role};
use hushrun_testkit::git;
use hushrun_testkit::http::{Reply, Response, Server};
use hushrun_testkit::world::World;
use serde_json::Value;

const CI: u8 = 1;

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

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

fn world() -> World {
    World::new("acme-api.yaml")
}

fn fake(w: &World) -> &FakeLedger {
    &w.fake
}

fn image(c: char) -> String {
    format!(
        "us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        c.to_string().repeat(64)
    )
}

// covers: L-1, L-3, L-8, L-10
#[test]
fn a_pipeline_records_and_plans_against_an_http_ledger() {
    let w = world();
    let ci = |args: &[&str]| {
        let mut cmd = w.hushrun_as(CI);
        cmd.env("HUSHRUN_RUN_ID", "r1").args(args);
        ok(&mut cmd)
    };
    ci(&["plan", "--start"]);
    ci(&[
        "attest",
        "--check",
        "unit=pass",
        "--check",
        "toolchain=pass",
        "--artifact",
        &format!("image={}", image('a')),
    ]);
    ci(&["mark", "deployed"]);
    ci(&["mark", "passed"]);
    // ADR 0062: `_` holds the place a stage had.
    assert_eq!(w.keys("v1/deployed/default/_/").len(), 1);
    assert!(w.keys("v1/events/default/").len() >= 5);

    // A deploy run by hand reads the HTTP ledger and reuses the image that passed.
    let plan = json(&ok(w.hushrun_as(CI).env("HUSHRUN_RUN_ID", "r2").args([
        "plan",
        "--trigger",
        "manual",
        "--format",
        "json",
    ])));
    let decision = &plan["scopes"][0];
    assert_eq!(decision["mode"], "promote", "{plan}");
    assert_eq!(decision["artifacts"]["image"]["ref"], image('a'));
    // One listing and one batch-get per read, not one request per event.
    let seen = fake(&w).requests();
    assert!(
        seen.iter()
            .any(|r| r.method == "POST" && r.path == "batch-get")
    );
    assert!(
        !seen
            .iter()
            .any(|r| r.method == "GET" && r.path.starts_with("objects/v1/events/")),
        "events are read in batches"
    );

    let ls = json(&ok(w.hushrun().args([
        "repository",
        "ls",
        "--format",
        "json",
    ])));
    assert!(ls["events"].as_array().unwrap().len() >= 5);
}

// covers: L-2, L-10
#[test]
fn a_ledger_that_refuses_policies_only_warns() {
    let w = world();
    // The fake checks that the key's hex is the body's SHA-256, as the protocol says.
    ok(w.hushrun_as(CI).args(["plan", "--start"]));
    assert_eq!(w.keys("v1/policies/").len(), 1);
    // Denied: the plan still decides and records `started`, with a warning.
    let w = world();
    fake(&w).inject_on(
        Some("PUT"),
        Some("objects/v1/policies/"),
        hushrun_testkit::fake_ledger::Fault::Status(403, None),
        // The evaluated hushrun.yaml, and the default branch's copy that trust came from.
        2,
    );
    let output = w.hushrun_as(CI).args(["plan", "--start"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("wasn't uploaded"), "{stderr}");
    assert!(w.keys("v1/policies/").is_empty());
    assert_eq!(
        w.keys("v1/events/")
            .iter()
            .filter(|k| k.contains(".started."))
            .count(),
        1
    );
}

// covers: L-10
#[test]
fn doctor_checks_the_protocol_and_the_repository() {
    let w = world();
    let doctor = || {
        let output = w
            .hushrun()
            .args(["doctor", "--format", "json"])
            .output()
            .unwrap();
        (output.status.code(), json(&output.stdout))
    };
    let status = |out: &Value, check: &str| {
        out["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == check)
            .map(|c| c["status"].as_str().unwrap().to_string())
            .unwrap_or_else(|| panic!("no {check} in {out}"))
    };
    let (_, out) = doctor();
    assert_eq!(status(&out, "server"), "ok", "{out}");
    assert_eq!(status(&out, "server protocol"), "ok", "{out}");
    assert_eq!(status(&out, "clock"), "ok", "{out}");
    assert_eq!(status(&out, "transparency log"), "ok", "{out}");
    // A ledger that holds another repository is an error.
    fake(&w).set_repository("github.com/acme/other");
    let (code, out) = doctor();
    assert_eq!(code, Some(2));
    assert_eq!(status(&out, "server protocol"), "error", "{out}");
}

// covers: L-10, T-4
#[test]
fn ledger_tokens_come_from_the_environment_and_are_never_logged() {
    let w = world();
    // No token: the ledger answers 401, and hushrun exits 6 saying where tokens come from.
    let output = w
        .hushrun()
        .env_remove("HUSHRUN_REPOSITORY_TOKEN")
        .args(["repository", "ls"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("HUSHRUN_REPOSITORY_TOKEN"), "{stderr}");
    // Debug logs show requests, never the token.
    let output = w
        .hushrun()
        .args(["-vv", "repository", "ls"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("GET http://127.0.0.1"), "{stderr}");
    assert!(!stderr.contains(FakeLedger::ADMIN), "{stderr}");
    // A reader can't record events: access denied, exit 6 (L-5).
    w.hushrun_as(CI)
        .env("HUSHRUN_REPOSITORY_TOKEN", FakeLedger::READER)
        .args(["attest", "--check", "unit=pass"])
        .assert()
        .code(6);
    #[cfg(unix)]
    ok(w.hushrun()
        .env_remove("HUSHRUN_REPOSITORY_TOKEN")
        .env(
            "HUSHRUN_REPOSITORY_TOKEN_COMMAND",
            format!("echo {}", FakeLedger::READER),
        )
        .args(["repository", "ls"]));
}

// covers: L-10, T-4
#[test]
fn tokens_go_only_to_a_ledger_named_outside_the_repository() {
    let w = world();
    // Another host, which records the Authorization headers it gets and refuses everything.
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let log = seen.clone();
    let elsewhere = Server::start(move |request| {
        log.lock()
            .unwrap()
            .push(request.header("authorization").map(str::to_string));
        Reply::Send(Response::json(
            401,
            &serde_json::json!({"error": {"code": "unauthenticated", "message": "no"}}),
        ))
    });
    let foreign = format!("{}/l/evil/ledger", elsewhere.url());
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();

    // A change to hushrun.yaml points the ledger there; nothing outside the repository names
    // it, so no token, token command or ID token goes there (T-4).
    let redirected: String = policy
        .lines()
        .map(|l| {
            if l.starts_with("repository_url:") {
                format!("repository_url: {foreign}\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    w.write("hushrun.yaml", &redirected);
    w.commit("point the ledger elsewhere");
    let output = w
        .hushrun()
        .env_remove("HUSHRUN_REPOSITORY_URL")
        .env(
            "HUSHRUN_REPOSITORY_TOKEN_COMMAND",
            "echo leaked-command-token",
        )
        .env("ACTIONS_ID_TOKEN_REQUEST_URL", "http://127.0.0.1:9/idtoken")
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "runner-request-token")
        .args(["repository", "ls"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("comes from hushrun.yaml only"), "{stderr}");
    let received = seen.lock().unwrap().clone();
    assert!(!received.is_empty(), "the foreign host was asked");
    assert!(received.iter().all(Option::is_none), "{received:?}");

    w.write("hushrun.yaml", &policy);
    w.commit("the ledger again");
    ok(w.hushrun().args(["plan"]));
    // The ledger itself, named outside the repository, got the token all along.
    let bearer = format!("Bearer {}", FakeLedger::ADMIN);
    assert!(
        fake(&w)
            .requests()
            .iter()
            .all(|r| r.authorization.as_deref() == Some(bearer.as_str()))
    );
}

// covers: L-10
#[test]
fn github_actions_id_tokens_name_the_ledger_as_their_audience() {
    let w = world();
    let id_token = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjQxMDI0NDQ4MDB9.sig";
    fake(&w).add_token(id_token, Role::Writer);
    let issued = Arc::new(AtomicUsize::new(0));
    let count = issued.clone();
    let audience = w.ledger_url();
    let endpoint = Server::start(move |request| {
        let good = request.header("authorization") == Some("Bearer runner-request-token")
            && request.query("audience") == Some(audience.as_str());
        if !good {
            return Reply::Send(Response::new(403));
        }
        count.fetch_add(1, Ordering::SeqCst);
        Reply::Send(Response::json(
            200,
            &serde_json::json!({"count": 1, "value": id_token}),
        ))
    });
    ok(w.hushrun_as(CI)
        .env_remove("HUSHRUN_REPOSITORY_TOKEN")
        .env(
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            format!("{}/idtoken?api-version=2.0", endpoint.url()),
        )
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "runner-request-token")
        .args(["attest", "--check", "unit=pass"]));
    // One ID token for the whole command (it expires in 2100).
    assert_eq!(issued.load(Ordering::SeqCst), 1);
    let seen = fake(&w).requests();
    assert!(
        seen.iter()
            .filter(|r| r.method == "PUT")
            .all(|r| r.authorization.as_deref() == Some(&*format!("Bearer {id_token}")))
    );
    assert!(seen.iter().any(|r| r.method == "PUT"));
}

// covers: L-10
#[test]
fn in_ci_the_jobs_id_token_comes_before_an_agent_token() {
    let w = world();
    let id_token = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjQxMDI0NDQ4MDB9.sig";
    let agent = "hra_Ab12Cd-in-a-github-job";
    fake(&w).add_token(id_token, Role::Writer);
    fake(&w).add_token(agent, Role::Writer);
    let endpoint = Server::start(move |_| {
        Reply::Send(Response::json(
            200,
            &serde_json::json!({"count": 1, "value": id_token}),
        ))
    });
    let url = w.ledger_url();
    let before = fake(&w).requests().len();
    // The repository's URL is on hushrun's host, where an agent token goes too: the job's ID
    // token, which names the URL as its audience, still comes first.
    ok(w.hushrun_as(CI)
        .env_remove("HUSHRUN_REPOSITORY_TOKEN")
        .env("CI", "true")
        .env("HUSHRUN_RUN_ID", "9")
        .env("HUSHRUN_HOST", &url[..url.find("/l/").unwrap()])
        .env("HUSHRUN_TOKEN", agent)
        .env(
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            format!("{}/idtoken?api-version=2.0", endpoint.url()),
        )
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "runner-request-token")
        .args(["attest", "--check", "unit=pass"]));
    let seen = fake(&w).requests().split_off(before);
    assert!(seen.iter().any(|r| r.method == "PUT"));
    let bearer = format!("Bearer {id_token}");
    assert!(
        seen.iter()
            .all(|r| r.authorization.as_deref() == Some(bearer.as_str())),
        "{seen:?}"
    );
}

// covers: T-9, T-4
#[test]
fn doctor_checks_the_log_origin_the_ledger_states() {
    // The ledger reached by another URL, as a base-derived one (T-4) reaches a hosted ledger:
    // its checkpoints still name its own origin, which trust names.
    let w = world();
    let url = fake(&w).url();
    let alias = url.replace("127.0.0.1", "localhost");
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write("hushrun.yaml", &policy.replace(&url, &alias));
    git::commit_all(&w.repo, "the ledger by another name");
    w.push_default_branch();
    let output = w
        .hushrun()
        .env("HUSHRUN_REPOSITORY_URL", &alias)
        .args(["doctor", "--format", "json"])
        .output()
        .unwrap();
    let out = json(&output.stdout);
    let log: Vec<&Value> = out["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["check"] == "transparency log")
        .collect();
    assert!(
        !log.is_empty() && log.iter().all(|c| c["status"] == "ok"),
        "{out}"
    );
}

// covers: T-9, L-11
#[test]
fn doctor_says_how_to_name_the_log_key() {
    let w = World::without_log_key("acme-api.yaml");
    let output = w
        .hushrun()
        .args(["doctor", "--format", "json"])
        .output()
        .unwrap();
    let out = json(&output.stdout);
    let check = out["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["check"] == "transparency log")
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(check["status"], "warning", "{out}");
    let (origin, key) = fake(&w).log_key();
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.contains(&origin) && detail.contains(&key),
        "{detail}"
    );
}
