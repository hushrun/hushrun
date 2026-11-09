//! The GitLab CI provider, end to end (M9): run identity, dotenv reports, the project's settings
//! from a fake GitLab API on the loopback, and keyless signing with GitLab's ID tokens against a
//! fake Sigstore. \[ADR 0062\] One pipeline, on main: a plan never asks GitLab about pipelines.

use hushrun_attest::SigningKey;
use hushrun_attest::testing::gitlab_claims;
use hushrun_testkit::fake_gitlab::{FakeGitLab, Pipeline};
use hushrun_testkit::fake_ledger::{FakeLedger, Role};
use hushrun_testkit::sigstore::FakeSigstore;
use hushrun_testkit::world::World;
use serde_json::Value;

const MEMBER: u8 = 1; // a member's machine
const OTHER_MEMBER: u8 = 2;
const PROJECT: u64 = 4242;

fn json(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

fn run(cmd: &mut assert_cmd::Command) -> (Option<i32>, Vec<u8>, String) {
    let output = cmd.output().unwrap();
    (
        output.status.code(),
        output.stdout,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn ok(cmd: &mut assert_cmd::Command) -> Vec<u8> {
    let (code, stdout, stderr) = run(cmd);
    assert_eq!(
        code,
        Some(0),
        "stdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&stdout)
    );
    stdout
}

/// \[ADR 0062\] `testdata/policies/gitlab-back.yaml`: the image deployed from main, by digest.
fn world() -> World {
    World::new("gitlab-back.yaml")
}

/// `hushrun` as a job of GitLab pipeline `pipeline` on `branch`, started by `source`.
fn on_gitlab(
    w: &World,
    key: Option<u8>,
    pipeline: &str,
    branch: &str,
    source: &str,
) -> assert_cmd::Command {
    let mut cmd = match key {
        Some(n) => w.hushrun_as(n),
        None => w.hushrun(),
    };
    cmd.env("GITLAB_CI", "true")
        .env("CI", "true")
        .env("CI_PIPELINE_ID", pipeline)
        .env(
            "CI_PIPELINE_URL",
            format!("https://gitlab.com/acme/back/-/pipelines/{pipeline}"),
        )
        .env("CI_PIPELINE_SOURCE", source)
        .env("CI_PROJECT_URL", "https://gitlab.com/acme/back")
        .env("CI_PROJECT_ID", PROJECT.to_string())
        .env("CI_COMMIT_BRANCH", branch)
        .env("CI_COMMIT_SHA", w.head())
        .env("CI_JOB_NAME", "plan")
        .env("CI_JOB_ID", "9001")
        .env("HUSHRUN_REPOSITORY_URL", w.ledger_url());
    cmd
}

/// The API of `fake`, with its token.
fn with_api(cmd: &mut assert_cmd::Command, fake: &FakeGitLab) {
    cmd.env("CI_API_V4_URL", fake.url())
        .env("HUSHRUN_GITLAB_TOKEN", FakeGitLab::TOKEN);
}

fn api() -> FakeGitLab {
    let fake = FakeGitLab::start();
    fake.add_project(PROJECT, "acme/back", "main");
    fake
}

fn pipeline(id: u64, reference: &str, sha: &str, status: &str) -> Pipeline {
    Pipeline {
        id,
        reference: reference.into(),
        sha: sha.into(),
        status: status.into(),
        source: "push".into(),
    }
}

fn show(w: &World, kind: &str) -> Vec<Value> {
    w.keys("v1/events/")
        .into_iter()
        .filter(|k| k.contains(&format!(".{kind}.")))
        .map(|k| {
            json(&ok(w.hushrun().args([
                "repository",
                "show",
                &k,
                "--format",
                "json",
            ])))
        })
        .collect()
}

// covers: T-4
#[test]
fn a_gitlab_job_records_its_pipeline_and_writes_a_dotenv_report() {
    let w = world();
    let report = w.path("plan.env");
    let plan_file = w.path("plan.json");
    let plan = json(&ok(on_gitlab(&w, Some(MEMBER), "100", "main", "push")
        .args([
            "plan",
            "--start",
            "--format",
            "json",
            "--out",
            plan_file.to_str().unwrap(),
            "--dotenv",
            report.to_str().unwrap(),
        ])));
    let run = &plan["run"];
    assert_eq!(run["provider"], "gitlab", "{plan}");
    assert_eq!(run["id"], "100");
    assert_eq!(run["attempt"], 1);
    assert_eq!(run["trigger"], "push");
    assert_eq!(run["url"], "https://gitlab.com/acme/back/-/pipelines/100");
    assert_eq!(run["job"], "plan");
    assert_eq!(run["jobId"], "9001");
    let started = show(&w, "started");
    let recorded = &started[0]["statement"]["predicate"]["run"];
    assert_eq!(recorded["provider"], "gitlab");
    assert_eq!(recorded["jobId"], "9001");
    // The dotenv report: KEY=value lines GitLab can read, without the plan's JSON.
    let text = std::fs::read_to_string(&report).unwrap();
    assert!(text.contains("HUSHRUN_MODE=full\n"), "{text}");
    assert!(text.contains("HUSHRUN_CHECKS=unit\n"), "{text}");
    assert!(text.contains("HUSHRUN_BUILD=true\n"), "{text}");
    assert!(!text.contains("HUSHRUN_JSON"), "{text}");
    for line in text.lines() {
        let (key, _) = line.split_once('=').expect("KEY=value");
        assert!(
            !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "{line}"
        );
    }
    // A later job carries out the plan: the same pipeline and attempt, its own job.
    ok(on_gitlab(&w, Some(MEMBER), "100", "main", "push")
        .env("CI_JOB_NAME", "test")
        .env("CI_JOB_ID", "9002")
        .args([
            "attest",
            "--plan",
            plan_file.to_str().unwrap(),
            "--check",
            "unit=pass",
        ]));
    let checks = show(&w, "check");
    let recorded = &checks[0]["statement"]["predicate"]["run"];
    assert_eq!(recorded["id"], "100");
    assert_eq!(recorded["attempt"], 1);
    assert_eq!(recorded["job"], "test");
    assert_eq!(recorded["jobId"], "9002");
    // No GitHub runner files on GitLab: the output of `digest` goes to stdout only.
    let digest =
        ok(on_gitlab(&w, None, "100", "main", "push").args(["digest", "--format", "dotenv"]));
    assert!(String::from_utf8_lossy(&digest).starts_with("HUSHRUN_DIGEST=sha256:"));
}

// covers: P-5
#[test]
fn a_retried_plan_job_starts_a_new_attempt() {
    let w = world();
    let plan = |extra: &[(&str, &str)]| {
        let mut cmd = on_gitlab(&w, Some(MEMBER), "100", "main", "push");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        json(&ok(cmd.args(["plan", "--start", "--format", "json"])))
    };
    assert_eq!(plan(&[])["run"]["attempt"], 1);
    // GitLab keeps the pipeline id when a job is retried, and counts no attempts.
    assert_eq!(plan(&[])["run"]["attempt"], 2);
    assert_eq!(plan(&[("HUSHRUN_RUN_ATTEMPT", "7")])["run"]["attempt"], 7);
    // HUSHRUN_RUN_ID doesn't change the pipeline.
    assert_eq!(plan(&[("HUSHRUN_RUN_ID", "elsewhere")])["run"]["id"], "100");
}

// covers: P-6, P-13
#[test]
fn a_pipeline_without_an_outcome_lends_nothing_and_gitlab_is_never_asked() {
    let w = world();
    // Pipeline 200 started this digest and built its image, then ended without recording an
    // outcome.
    ok(on_gitlab(&w, Some(MEMBER), "200", "main", "push").args(["plan", "--start"]));
    ok(on_gitlab(&w, Some(MEMBER), "200", "main", "push").args([
        "attest",
        "--artifact",
        &format!("image={}", image('a')),
    ]));
    // GitLab's API would say what became of it, and that another pipeline of this commit runs.
    let fake = api();
    fake.add_pipeline(PROJECT, pipeline(200, "main", &w.head(), "failed"));
    fake.add_pipeline(PROJECT, pipeline(201, "main", &w.head(), "running"));
    for status in ["failed", "canceled", "success", "running", "manual"] {
        fake.set_status(PROJECT, 200, status);
        let mut cmd = on_gitlab(&w, None, "300", "main", "push");
        with_api(&mut cmd, &fake);
        let (code, stdout, stderr) = run(cmd.args(["plan", "--format", "json"]));
        assert_eq!(code, Some(0), "{status}: {stderr}");
        let decision = &json(&stdout)["scopes"][0];
        // [ADR 0062] Only an attempt that recorded `passed` lends its image, whatever GitLab
        // says of it, and no plan waits for another pipeline.
        assert_eq!(decision["mode"], "full", "{status}: {decision}");
        assert_eq!(decision["build"], true, "{decision}");
        assert!(
            decision["reason"]
                .as_str()
                .unwrap()
                .contains("recorded ones aren't reusable"),
            "{decision}"
        );
        assert!(decision.get("upstream").is_none(), "{decision}");
    }
    // Nothing asks GitLab about pipelines: not their status, not which are pending.
    assert_eq!(fake.requests(), Vec::<String>::new());
    // So an API that doesn't answer changes nothing, and nothing names it or its token.
    let (code, stdout, stderr) = run(on_gitlab(&w, None, "301", "main", "push")
        .env("CI_API_V4_URL", "http://127.0.0.1:9/api/v4")
        .env("HUSHRUN_GITLAB_TOKEN", "glpat-test-token")
        .args(["plan", "--format", "json"]));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(json(&stdout)["scopes"][0]["mode"], "full");
    assert!(!stderr.contains("GitLab API"), "{stderr}");
    assert!(!stderr.contains("glpat-test-token"), "{stderr}");
}

// covers: C-2
#[test]
fn a_project_mismatch_is_a_warning() {
    let w = world();
    // The provider names another project than hushrun.yaml.
    w.serve_origin_as("https://gitlab.com/someone/else");
    let output = on_gitlab(&w, None, "400", "main", "push")
        .env("CI_PROJECT_URL", "https://gitlab.com/someone/else")
        .args(["plan"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("gitlab.com/someone/else"));
}

/// An image at the artifact's location.
fn image(c: char) -> String {
    format!(
        "us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        c.to_string().repeat(64)
    )
}

/// A job of main's pipeline 500 that signs keylessly with `fake`'s ID token.
fn keyless(w: &World, fake: &FakeSigstore) -> assert_cmd::Command {
    let mut cmd = on_gitlab(w, None, "500", "main", "push");
    cmd.envs(fake.gitlab_env(&w.root));
    cmd
}

// covers: E-11, T-15, T-12
#[test]
fn keyless_signing_on_gitlab_uses_its_id_tokens() {
    let w = world();
    // [ADR 0031] The server names GitLab CI of project 4242 as the repository's CI (T-15).
    w.fake.set_ci("https://gitlab.com", "4242");
    let fake = FakeSigstore::start();
    fake.set_claims(gitlab_claims("acme/back", "4242", "main", "sigstore"));
    ok(keyless(&w, &fake).args(["plan", "--start"]));
    ok(keyless(&w, &fake).args([
        "attest",
        "--check",
        "unit=pass",
        "--artifact",
        &format!("image={}", image('a')),
    ]));
    ok(keyless(&w, &fake).args(["mark", "deployed"]));
    ok(keyless(&w, &fake).args(["mark", "passed"]));
    // Fulcio received GitLab's token, and certified what its claims say.
    assert_eq!(fake.count("/api/v2/signingCert"), 4);
    // A later pipeline verifies those keyless events by their GitLab identity. One a pipeline
    // started finds this digest deployed: nothing to deploy (P-10).
    let root = fake.write_trusted_root(&w.root);
    let later = |trigger: &[&str]| {
        json(&ok(on_gitlab(
            &w,
            Some(OTHER_MEMBER),
            "600",
            "main",
            "pipeline",
        )
        .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &root)
        .args(["plan", "--format", "json"])
        .args(trigger)))
    };
    let plan = later(&[]);
    assert_eq!(plan["run"]["trigger"], "upstream");
    assert_eq!(plan["scopes"][0]["mode"], "current", "{plan}");
    // Started by hand, it deploys the image pipeline 500 built and checked, by digest (P-14).
    let plan = later(&["--trigger", "manual"]);
    assert_eq!(plan["scopes"][0]["mode"], "promote", "{plan}");
    assert_eq!(plan["scopes"][0]["artifacts"]["image"]["ref"], image('a'));
    let digest = plan["scopes"][0]["digest"].as_str().unwrap().to_string();
    let verified = json(&ok(w
        .hushrun()
        .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &root)
        .args([
            "verify", "--scope", "default", "--digest", &digest, "--format", "json",
        ])));
    let first = &verified["results"][0];
    assert_eq!(first["signer"], "ci", "{verified}");
    let identity = &first["signature"]["identity"];
    assert_eq!(identity["issuer"], "https://gitlab.com");
    assert_eq!(identity["repositoryId"], "4242");
    assert_eq!(identity["ref"], "refs/heads/main");
    assert_eq!(
        identity["workflow"],
        "https://gitlab.com/acme/back//.gitlab-ci.yml@refs/heads/main"
    );
}

// covers: T-15, T-12
#[test]
fn gitlab_id_tokens_must_suit_fulcio_and_the_signers() {
    let w = world();
    // [ADR 0031] The server names GitLab CI of project 4242 as the repository's CI (T-15).
    w.fake.set_ci("https://gitlab.com", "4242");
    let fake = FakeSigstore::start();
    let attest =
        |fake: &FakeSigstore| run(keyless(&w, fake).args(["attest", "--check", "unit=pass"]));
    // A token minted for another audience never reaches Fulcio.
    fake.set_claims(gitlab_claims(
        "acme/back",
        "4242",
        "main",
        "https://gitlab.com",
    ));
    let (code, _, stderr) = attest(&fake);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("`SIGSTORE_ID_TOKEN: {aud: sigstore}`"),
        "{stderr}"
    );
    assert_eq!(fake.count("/api/v2/signingCert"), 0);
    // No token: the job didn't declare one.
    let (code, _, stderr) = run(keyless(&w, &fake).env_remove("SIGSTORE_ID_TOKEN").args([
        "attest",
        "--check",
        "unit=pass",
    ]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("`id_tokens`"), "{stderr}");
    // Another project's pipeline signs, but its events aren't this repository's CI's (T-15).
    // Outside its CI, so that the project id check doesn't stop it first.
    fake.set_claims(gitlab_claims("acme/other", "77", "main", "sigstore"));
    let (code, _, stderr) = run(w
        .hushrun()
        .envs(fake.gitlab_env(&w.root))
        .env("HUSHRUN_RUN_ID", "x")
        .args(["attest", "--check", "unit=pass"]));
    assert_eq!(code, Some(0), "{stderr}");
    let files: Vec<_> = w.keys("v1/events/").iter().map(|k| w.export(k)).collect();
    let root = fake.write_trusted_root(&w.root);
    let (code, stdout, _) = run(w
        .hushrun()
        .env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &root)
        .arg("verify")
        .args(&files));
    assert_eq!(code, Some(5));
    assert!(
        String::from_utf8_lossy(&stdout).contains("isn't the repository's CI"),
        "{}",
        String::from_utf8_lossy(&stdout)
    );
}

fn doctor_checks(output: &[u8]) -> Vec<(String, String, String)> {
    json(output)["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["check"].as_str().unwrap().to_string(),
                c["status"].as_str().unwrap().to_string(),
                c["detail"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn find<'a>(
    checks: &'a [(String, String, String)],
    name: &str,
    detail: &str,
) -> &'a (String, String, String) {
    checks
        .iter()
        .find(|(n, _, d)| n == name && d.contains(detail))
        .unwrap_or_else(|| panic!("no {name} check about {detail:?}: {checks:#?}"))
}

#[test]
fn doctor_checks_the_gitlab_project() {
    let w = world();
    w.write(
        ".gitlab-ci.yml",
        "plan:\n  script: [hushrun plan --start]\n",
    );
    w.commit("pipeline");
    let fake = api();
    fake.set(
        PROJECT,
        "ci_pipeline_variables_minimum_override_role",
        "developer".into(),
    );
    let mut cmd = on_gitlab(&w, Some(MEMBER), "100", "main", "push");
    with_api(&mut cmd, &fake);
    let checks = doctor_checks(&ok(cmd.args(["doctor", "--format", "json"])));
    assert_eq!(find(&checks, "ci", "gitlab run 100").1, "ok");
    assert_eq!(
        find(&checks, "gitlab api", "acme/back (project 4242)").1,
        "ok"
    );
    // [ADR 0062] The one branch it checks is the one it deploys from, `deploy.branch`.
    assert_eq!(
        find(&checks, "protected branch", "main isn't protected").1,
        "warning"
    );
    assert_eq!(
        checks
            .iter()
            .filter(|(n, _, _)| n == "protected branch")
            .count(),
        1,
        "{checks:#?}"
    );
    assert_eq!(
        find(&checks, "pipeline variables", "No one allowed").1,
        "warning"
    );
    assert_eq!(find(&checks, "deploy workflow", ".gitlab-ci.yml").1, "ok");
    assert!(
        !checks.iter().any(|(n, _, _)| n == "default branch"),
        "{checks:#?}"
    );
    fake.protect(PROJECT, "main");
    let mut cmd = on_gitlab(&w, Some(MEMBER), "100", "main", "push");
    with_api(&mut cmd, &fake);
    let checks = doctor_checks(&ok(cmd.args(["doctor", "--format", "json"])));
    assert_eq!(
        find(&checks, "protected branch", "main is protected").1,
        "ok"
    );
    // A Reporter's token can't see either setting: both are skipped, not failed.
    fake.set(
        PROJECT,
        "ci_pipeline_variables_minimum_override_role",
        Value::Null,
    );
    fake.forbid("/protected_branches/");
    let mut cmd = on_gitlab(&w, Some(MEMBER), "100", "main", "push");
    with_api(&mut cmd, &fake);
    let checks = doctor_checks(&ok(cmd.args(["doctor", "--format", "json"])));
    assert_eq!(
        find(&checks, "protected branch", "may not read").1,
        "skipped"
    );
    assert_eq!(
        find(&checks, "pipeline variables", "Maintainer role").1,
        "skipped"
    );
    // Without a token, GitLab jobs are told what they miss.
    let checks = doctor_checks(&ok(
        on_gitlab(&w, Some(MEMBER), "100", "main", "push").args(["doctor", "--format", "json"])
    ));
    assert_eq!(
        find(&checks, "gitlab api", "HUSHRUN_GITLAB_TOKEN").1,
        "warning"
    );
}

// covers: T-15
#[test]
fn doctor_shows_whether_a_gitlab_job_signs_as_the_repositorys_ci() {
    let w = world();
    // [ADR 0031] The server names GitLab CI of project 4242 as the repository's CI (T-15).
    w.fake.set_ci("https://gitlab.com", "4242");
    let fake = FakeSigstore::start();
    fake.set_claims(gitlab_claims("acme/back", "4242", "main", "sigstore"));
    let checks = doctor_checks(&ok(keyless(&w, &fake).args(["doctor", "--format", "json"])));
    assert_eq!(
        find(&checks, "signer", "signs as the repository's CI").1,
        "ok",
        "{checks:#?}"
    );
    // Another project's pipeline isn't this repository's CI: doctor says so.
    fake.set_claims(gitlab_claims("acme/other", "77", "main", "sigstore"));
    let (code, stdout, _) = run(w
        .hushrun()
        .envs(fake.gitlab_env(&w.root))
        .env("GITLAB_CI", "true")
        .env("CI", "true")
        .args(["doctor", "--format", "json"]));
    assert_eq!(code, Some(2));
    let checks = doctor_checks(&stdout);
    let (_, status, detail) = find(&checks, "signer", "isn't the repository's CI");
    assert_eq!(status, "error");
    assert!(detail.contains("repository_id 77"), "{detail}");
}

/// The agent token an org admin makes for the pipeline, with its machine key: `hushrun token
/// create --agent gitlab-ci --expires 365d`.
const AGENT_TOKEN: &str = "hra_gl1Ab2-gitlab-ci-token";

/// The world's server origin, `http://127.0.0.1:<port>`: hushrun's host in these tests.
fn host(w: &World) -> String {
    let url = w.ledger_url();
    url[..url.find("/l/").expect("a repository URL")].to_string()
}

/// `hushrun` as job `job` of main's pipeline 700, set up as `hushrun init --pipeline` says:
/// the agent's token in HUSHRUN_TOKEN, the repository's URL in hushrun.yaml only, on hushrun's
/// host, and no other credential or signer. Each caller gives it the agent's machine key.
fn as_gitlab_agent(w: &World, job: &str) -> assert_cmd::Command {
    let mut cmd = on_gitlab(w, None, "700", "main", "push");
    cmd.env_remove("HUSHRUN_REPOSITORY_URL")
        .env_remove("HUSHRUN_REPOSITORY_TOKEN")
        .env("CI_JOB_NAME", job)
        .env("HUSHRUN_HOST", host(w))
        .env("HUSHRUN_TOKEN", AGENT_TOKEN);
    cmd
}

// covers: L-10, T-12, T-15, C-18
#[test]
fn a_gitlab_pipeline_writes_and_signs_as_its_agent() {
    let w = world();
    // `hushrun token create` registered the agent's machine key with its token.
    let key = SigningKey::generate_p256().unwrap();
    w.fake.add_token(AGENT_TOKEN, Role::Writer);
    w.fake
        .register_machine(&key.public_key(), "agent:gitlab-ci", false);
    // A File variable: GitLab writes its value beside the project's directory, and the
    // variable holds the path.
    let key_file = w.path("repo.tmp/HUSHRUN_MACHINE_KEY_FILE");
    std::fs::create_dir_all(key_file.parent().unwrap()).unwrap();
    std::fs::write(&key_file, key.to_pem().as_str()).unwrap();
    let plan_file = w.path("plan.json");
    let plan_file = plan_file.to_str().unwrap();
    let before = w.fake.requests().len();

    let plan = json(&ok(as_gitlab_agent(&w, "plan")
        .env("HUSHRUN_MACHINE_KEY_FILE", &key_file)
        .args(["plan", "--start", "--format", "json", "--out", plan_file])));
    assert_eq!(plan["scopes"][0]["mode"], "full", "{plan}");
    ok(as_gitlab_agent(&w, "test")
        .env("HUSHRUN_MACHINE_KEY_FILE", &key_file)
        .args(["attest", "--plan", plan_file, "--check", "unit=pass"]));
    // The key in a variable of its own signs the same.
    ok(as_gitlab_agent(&w, "deploy")
        .env("HUSHRUN_MACHINE_KEY", key.to_pem().as_str())
        .args([
            "mark",
            "deployed",
            "--plan",
            plan_file,
            "--artifact",
            &format!("image={}", image('a')),
        ]));

    // Every request carried the agent's token, though only hushrun.yaml names the URL.
    let seen = w.fake.requests().split_off(before);
    assert!(seen.iter().any(|r| r.method == "PUT"), "{seen:?}");
    let bearer = format!("Bearer {AGENT_TOKEN}");
    assert!(
        seen.iter()
            .all(|r| r.authorization.as_deref() == Some(bearer.as_str())),
        "{seen:?}"
    );
    // Its events are signed by the agent's machine key, and count as agent:gitlab-ci's.
    let digest = plan["scopes"][0]["digest"].as_str().unwrap();
    let verified = json(&ok(w.hushrun().args([
        "verify", "--scope", "default", "--digest", digest, "--format", "json",
    ])));
    let results = verified["results"].as_array().unwrap();
    let kinds: Vec<&str> = results.iter().filter_map(|r| r["kind"].as_str()).collect();
    for kind in ["started", "check", "deployed"] {
        assert!(kinds.contains(&kind), "{kind}: {verified}");
    }
    for result in results {
        assert_eq!(result["accepted"], true, "{verified}");
        assert_eq!(result["signer"], "agent:gitlab-ci", "{verified}");
        assert_eq!(result["signature"]["kind"], "machine", "{verified}");
        assert_eq!(
            result["signature"]["keyid"],
            key.public_key().keyid(),
            "{verified}"
        );
        // A machine is never the CI: its custody stops at isolated (T-15).
        assert_eq!(result["effectiveCustody"], "isolated", "{verified}");
    }
    // The deployed index names the deploy, as the agent recorded it, under no stage (L-8).
    assert_eq!(w.keys("v1/deployed/default/_/").len(), 1);
}

// covers: L-10, T-4
#[test]
fn in_ci_no_other_token_follows_the_url_hushrun_yaml_names() {
    let w = world();
    w.fake.add_token(AGENT_TOKEN, Role::Writer);
    let attest = |token: &str, host: &str| {
        let mut cmd = on_gitlab(&w, Some(MEMBER), "800", "main", "push");
        cmd.env_remove("HUSHRUN_REPOSITORY_URL")
            .env_remove("HUSHRUN_REPOSITORY_TOKEN")
            .env("HUSHRUN_HOST", host)
            .env("HUSHRUN_TOKEN", token)
            .args(["attest", "--check", "unit=pass"]);
        let before = w.fake.requests().len();
        let (code, _, stderr) = run(&mut cmd);
        (code, stderr, w.fake.requests().split_off(before))
    };
    // HUSHRUN_TOKEN holds a token the server would take, but not an agent's: it stays put,
    // on hushrun's host too, as does HUSHRUN_REPOSITORY_TOKEN.
    let (code, stderr, seen) = attest(FakeLedger::WRITER, &host(&w));
    assert_eq!(code, Some(6), "{stderr}");
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|r| r.authorization.is_none()), "{seen:?}");
    // An agent token goes only to hushrun's host: elsewhere, nothing goes.
    let (code, stderr, seen) = attest(AGENT_TOKEN, "http://127.0.0.1:9");
    assert_eq!(code, Some(6), "{stderr}");
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|r| r.authorization.is_none()), "{seen:?}");
    assert!(w.keys("v1/events/").is_empty());
}

// covers: T-12
#[test]
fn ci_signs_with_an_agents_machine_key_unless_github_actions_gives_an_id_token() {
    let w = world();
    let key = SigningKey::generate_p256().unwrap();
    let pem = key.to_pem().as_str().to_string();
    let public =
        |cmd: &mut assert_cmd::Command| run(cmd.args(["keys", "public", "--format", "json"]));
    let keyless = "signs without a key";
    // A GitLab job with the agent's key signs with it, even with an ID token for Fulcio: only
    // GitHub Actions is a repository's keyless CI.
    for id_token in [None, Some("eyJ.e30.sig")] {
        let mut cmd = on_gitlab(&w, None, "1", "main", "push");
        cmd.env("HUSHRUN_MACHINE_KEY", &pem);
        if let Some(token) = id_token {
            cmd.env("SIGSTORE_ID_TOKEN", token);
        }
        let (code, stdout, stderr) = public(&mut cmd);
        assert_eq!(code, Some(0), "{id_token:?}: {stderr}");
        assert_eq!(json(&stdout)["keyid"], key.public_key().keyid());
    }
    // Without a key, its ID token signs keylessly, as before; without either, it is told how
    // to get a signer.
    let (code, _, stderr) =
        public(on_gitlab(&w, None, "1", "main", "push").env("SIGSTORE_ID_TOKEN", "eyJ.e30.sig"));
    assert_eq!(code, Some(2));
    assert!(stderr.contains(keyless), "{stderr}");
    let (code, _, stderr) = public(&mut on_gitlab(&w, None, "1", "main", "push"));
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("HUSHRUN_MACHINE_KEY") && stderr.contains("hushrun token create --agent"),
        "{stderr}"
    );
    // The key a person's sign-in left on a runner, beside its credentials, never signs a CI
    // job's events: they would count as that person's.
    let left = w.root.join("machine-key.pem");
    std::fs::write(&left, &pem).unwrap();
    let (code, _, stderr) = public(&mut on_gitlab(&w, None, "1", "main", "push"));
    std::fs::remove_file(&left).unwrap();
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("hushrun token create --agent"), "{stderr}");
    // A GitHub Actions job with an ID token signs keylessly, machine key or not.
    let (code, _, stderr) = public(
        w.hushrun()
            .env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_RUN_ID", "1")
            .env("ACTIONS_ID_TOKEN_REQUEST_URL", "http://127.0.0.1:9/token")
            .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "runner-request-token")
            .env("HUSHRUN_MACHINE_KEY", &pem),
    );
    assert_eq!(code, Some(2));
    assert!(stderr.contains(keyless), "{stderr}");
}

/// The recipe's jobs, by name, without the hidden templates.
fn recipe() -> Value {
    let path = hushrun_testkit::workspace_root().join("recipes/gitlab/.gitlab-ci.yml");
    serde_saphyr::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn script_lines(job: &Value) -> Vec<String> {
    ["before_script", "script", "after_script"]
        .iter()
        .flat_map(|key| job[key].as_array().into_iter().flatten())
        .filter_map(|line| line.as_str().map(str::to_string))
        .collect()
}

fn extends(job: &Value) -> Vec<String> {
    match &job["extends"] {
        Value::String(one) => vec![one.clone()],
        Value::Array(many) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

// covers: T-4, P-6
#[test]
fn the_recipe_carries_out_one_plan_and_fails_loudly() {
    let recipe = recipe();
    // [ADR 0035] The newest release: the image's latest tag, and no version pinned.
    assert!(recipe["variables"]["HUSHRUN_VERSION"].is_null());
    assert_eq!(
        recipe["variables"]["HUSHRUN_IMAGE"],
        "ghcr.io/hushrun/hushrun:latest"
    );
    // The plan job passes the release it ran to later jobs, in its dotenv report.
    assert!(
        script_lines(&recipe["plan"]).contains(
            &"echo \"HUSHRUN_VERSION=$(hushrun --version | cut -d ' ' -f 2)\" >> plan.env"
                .to_string()
        ),
        "{}",
        recipe["plan"]
    );
    // On any other image, or one that isn't that release, the template installs it.
    let install = script_lines(&recipe[".hushrun"]).join("\n");
    assert!(
        install.contains(
            "if [ -z \"$have\" ] || { [ -n \"${HUSHRUN_VERSION:-}\" ] && [ \"$have\" != \"hushrun $HUSHRUN_VERSION\" ]; }; then"
        ) && install.contains("sh ci/install-hushrun.sh"),
        "{install}"
    );
    // [ADR 0062] One pipeline, on main, which sets nothing: the pipeline signs with the agent's
    // machine key from the project's settings, by default. Any other ref runs nothing.
    let rules = recipe["workflow"]["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2, "{rules:?}");
    assert_eq!(rules[0]["if"], "$CI_COMMIT_BRANCH == \"main\"");
    assert!(rules[0].get("variables").is_none(), "{rules:?}");
    assert_eq!(rules[1]["when"], "never");
    let text = std::fs::read_to_string(
        hushrun_testkit::workspace_root().join("recipes/gitlab/.gitlab-ci.yml"),
    )
    .unwrap();
    assert!(
        !text.contains("HUSHRUN_STAGE") && !text.contains("--stage"),
        "{text}"
    );
    fn keys(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    found.push(key.clone());
                    keys(inner, found);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| keys(i, found)),
            _ => {}
        }
    }
    let mut all = Vec::new();
    keys(&recipe, &mut all);
    for setting in [
        "HUSHRUN_REPOSITORY_URL",
        "HUSHRUN_GITLAB_TOKEN",
        "HUSHRUN_REPOSITORY_TOKEN",
        "HUSHRUN_TOKEN",
        "HUSHRUN_MACHINE_KEY",
        "HUSHRUN_MACHINE_KEY_FILE",
        "HUSHRUN_SIGNING",
        "HUSHRUN_SIGNING_KEY",
        "HUSHRUN_SIGNING_KEY_FILE",
        "HUSHRUN_HOST",
    ] {
        assert!(
            !all.iter().any(|k| k == setting),
            "{setting} belongs in the project's settings (T-4)"
        );
    }
    let jobs: Vec<(&String, &Value)> = recipe
        .as_object()
        .unwrap()
        .iter()
        .filter(|(name, job)| !name.starts_with('.') && job.get("stage").is_some())
        .collect();
    let stages: Vec<&str> = recipe["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    for (name, job) in &jobs {
        let lines = script_lines(job);
        let runs_hushrun = lines.iter().any(|l| l.contains("hushrun "));
        let templates = extends(job);
        if runs_hushrun {
            assert!(
                templates.iter().any(|t| t == ".hushrun"),
                "{name} runs hushrun without the .hushrun template"
            );
        }
        for line in &lines {
            // `eval "$(cmd)"` succeeds when cmd fails; a variable carries its status.
            assert!(!line.contains("eval \"$("), "{name}: {line}");
            assert!(!line.contains("&& eval"), "{name}: {line}");
        }
        // A job's own before_script or after_script would replace the template's.
        if templates.iter().any(|t| t == ".hushrun") {
            assert!(
                job.get("before_script").is_none(),
                "{name} replaces the install step"
            );
            assert!(
                job.get("after_script").is_none(),
                "{name} replaces the cancel hook"
            );
        }
    }
    for name in ["plan", "mark-failed"] {
        assert_eq!(recipe[name]["image"], "$HUSHRUN_IMAGE", "{name}");
    }
    let job = |name: &str| &recipe[name];
    let plan = script_lines(job("plan")).join("\n");
    assert!(
        plan.contains("hushrun plan --start --out plan.json --dotenv plan.env"),
        "{plan}"
    );
    assert_eq!(job("plan")["artifacts"]["reports"]["dotenv"], "plan.env");
    // The test job runs exactly the plan's checks, when it has any.
    let test = script_lines(job("test")).join("\n");
    assert!(test.contains("hushrun run --plan plan.json"), "{test}");
    let deploy = script_lines(job("deploy"));
    let at = |needle: &str| {
        deploy
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("deploy doesn't run {needle}"))
    };
    assert!(at("hushrun guard") < at("hushrun artifact resolve"));
    assert!(at("hushrun artifact resolve") < at("./deploy.sh"));
    assert!(at("./deploy.sh") < at("hushrun mark deployed"));
    assert!(at("hushrun mark deployed") < at("hushrun mark passed"));
    // One deploy at a time.
    assert_eq!(job("deploy")["resource_group"], "deploy");
    assert_eq!(job("mark-failed")["when"], "on_failure");
    let stage_of = |name: &str| {
        let stage = job(name)["stage"].as_str().unwrap();
        stages.iter().position(|s| *s == stage).unwrap()
    };
    assert!(stage_of("mark-failed") > stage_of("deploy"));
    // [ADR 0062] Nothing plans again after this pipeline's outcome: no other stage follows it.
    assert!(recipe.get("plan-production").is_none());
    assert!(
        !jobs.iter().any(|(_, job)| job.get("trigger").is_some()),
        "a job triggers another pipeline"
    );
    let cancel = recipe[".hushrun"]["after_script"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(cancel.contains("CI_JOB_STATUS\" = canceled"), "{cancel}");
    assert!(cancel.contains("mark superseded") && cancel.contains("--reason cancelled"));
}
