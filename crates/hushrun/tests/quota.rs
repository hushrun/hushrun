//! ADR 0032, end to end: the hosted service's plan limits. A check event or a result that the
//! org's plan doesn't let hushrun's server record isn't recorded, and the command goes on with
//! one warning and its usual exit code; `hushrun repository usage` shows the plan and this
//! period's checks; `hushrun repository stats` counts volume from one listing, or from this
//! machine's results, and sends nothing else.

use std::path::Path;

use hushrun_core::digest::Digest;
use hushrun_core::event::EventKind;
use hushrun_core::names::{ScopeName, StageName};
use hushrun_core::time::Time;
use hushrun_ledger::layout::{deployed_key, event_key};
use hushrun_testkit::fake_ledger::Fault;
use hushrun_testkit::fake_results::{FakeResults, quota_exceeded};
use hushrun_testkit::schema;
use hushrun_testkit::world::World;
use serde_json::{Value, json};

const MACHINE: u8 = 1; // a member's machine in the world's org

/// What the fake servers say when the org's plan records no more checks.
const MESSAGE: &str = "acme used the 2,000 checks of its Free plan this month";
const UPGRADE: &str = "https://app.hushrun.com/acme/-/billing";

fn json_of(output: &[u8]) -> Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(output)))
}

struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn ran(cmd: &mut assert_cmd::Command) -> Ran {
    let output = cmd.output().expect("hushrun");
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// The lines of `stderr` that start with `warning:`.
fn warnings(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.starts_with("warning:"))
        .collect()
}

/// The one warning a command says when the server refuses its check events.
fn the_warning() -> String {
    format!("warning: check results aren't recorded: {MESSAGE}; upgrade: {UPGRADE}")
}

/// From now on, the world's server answers every new check event with the 402 of an org that
/// used its plan's checks, as the hosted service does; other events are recorded.
fn refuse_checks(w: &World) {
    w.fake.inject_on(
        Some("PUT"),
        Some(".check."),
        Fault::Answer(402, quota_exceeded(MESSAGE, UPGRADE)),
        100,
    );
}

/// A repository whose deploy requires `ok`, with a check that fails, `bad`.
fn pipeline() -> World {
    let w = World::new("acme-api.yaml");
    w.write(
        "hushrun.yaml",
        &format!(
            "version: 1
repository: github.com/Acme/api
{}
checks:
  ok: {{run: \"echo running ok\"}}
  bad: {{run: \"exit 3\"}}
deploy:
  branch: develop
  require: [ok]
",
            w.ledger_lines()
        ),
    );
    w.commit("checks");
    w
}

// covers: L-5, L-10
#[test]
fn run_goes_on_without_the_check_event_the_orgs_plan_refuses() {
    let w = pipeline();
    let plan = w.path("plan.json");
    let planned = ran(w
        .hushrun_as(MACHINE)
        .env("HUSHRUN_RUN_ID", "r1")
        .env("HUSHRUN_RUN_ATTEMPT", "1")
        .args(["plan", "--out"])
        .arg(&plan));
    assert_eq!(planned.code, Some(0), "{}", planned.stderr);
    refuse_checks(&w);
    let before = w.fake.requests().len();
    // A re-run of the plan's run: its first write starts the new attempt (§10.4), and the
    // server records that `started`, refusing only the check event.
    let run = ran(w
        .hushrun_as(MACHINE)
        .env("HUSHRUN_RUN_ID", "r1")
        .env("HUSHRUN_RUN_ATTEMPT", "2")
        .args(["run", "--format", "json", "--plan"])
        .arg(&plan)
        .arg("ok"));
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(warnings(&run.stderr), [the_warning()], "{}", run.stderr);
    assert!(run.stderr.contains("running ok"), "{}", run.stderr);
    let out = json_of(run.stdout.as_bytes());
    schema::assert_valid(&schema::load("run.v2.schema.json"), &out);
    let check = &out["checks"][0];
    assert_eq!(check["check"], "ok");
    assert_eq!(check["result"], "pass");
    assert_eq!(check["exitCode"], 0);
    assert_eq!(check["detail"], "exit 0; not recorded");
    assert_eq!(check["event"], Value::Null);
    assert_eq!(check["custody"], Value::Null);
    let keys = w.keys("v1/events/");
    assert!(!keys.iter().any(|k| k.contains(".check.")), "{keys:?}");
    assert_eq!(
        keys.iter().filter(|k| k.contains(".started._.")).count(),
        1,
        "{keys:?}"
    );
    // One create for the check event: a 402 is never retried (L-4).
    let creates = w.fake.requests()[before..]
        .iter()
        .filter(|r| r.method == "PUT" && r.path.contains(".check."))
        .count();
    assert_eq!(creates, 1);
}

// covers: L-5
#[test]
fn a_failing_check_the_orgs_plan_refuses_still_exits_8() {
    let w = pipeline();
    refuse_checks(&w);
    let run = ran(w
        .hushrun_as(MACHINE)
        .env("HUSHRUN_RUN_ID", "r1")
        .args(["run", "ok", "bad"]));
    assert_eq!(run.code, Some(8), "{}", run.stderr);
    // Two refusals, one warning; the output marks each check that wasn't recorded.
    assert_eq!(warnings(&run.stderr), [the_warning()], "{}", run.stderr);
    assert!(
        run.stdout.contains("pass     ok (exit 0; not recorded)\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("fail     bad (exit 3; not recorded)\n"),
        "{}",
        run.stdout
    );
    assert!(w.keys("v1/events/").is_empty());
}

fn image(c: char) -> String {
    format!(
        "image=us-central1-docker.pkg.dev/acme-prod/api/go@sha256:{}",
        c.to_string().repeat(64)
    )
}

// covers: L-5
#[test]
fn attest_records_the_rest_when_the_orgs_plan_refuses_its_checks() {
    let w = World::new("acme-api.yaml");
    refuse_checks(&w);
    let attest = ran(w.hushrun_as(MACHINE).env("HUSHRUN_RUN_ID", "r1").args([
        "attest",
        "--check",
        "unit=pass",
        "--check",
        "toolchain=fail",
        "--artifact",
        &image('a'),
        "--format",
        "json",
    ]));
    assert_eq!(attest.code, Some(0), "{}", attest.stderr);
    assert_eq!(
        warnings(&attest.stderr),
        [the_warning()],
        "{}",
        attest.stderr
    );
    let out = json_of(attest.stdout.as_bytes());
    schema::assert_valid(&schema::load("attest.v2.schema.json"), &out);
    let events = out["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{out}");
    assert_eq!(events[0]["kind"], "artifact");
    let artifact = events[0]["keys"][0].as_str().unwrap();
    assert_eq!(w.keys("v1/events/"), [artifact.to_string()]);

    // In text, each check that wasn't recorded has its line.
    let attest = ran(w.hushrun_as(MACHINE).env("HUSHRUN_RUN_ID", "r2").args([
        "attest",
        "--check",
        "unit=pass",
    ]));
    assert_eq!(attest.code, Some(0), "{}", attest.stderr);
    assert_eq!(
        warnings(&attest.stderr),
        [the_warning()],
        "{}",
        attest.stderr
    );
    assert_eq!(attest.stdout, "not recorded check: unit\n");
    assert!(!w.keys("v1/events/").iter().any(|k| k.contains(".check.")));
}

// covers: L-5
#[test]
fn on_github_actions_the_warning_is_an_annotation() {
    let w = World::new("acme-api.yaml");
    refuse_checks(&w);
    let attest = ran(w
        .hushrun_as(MACHINE)
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_RUN_ID", "100")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_REPOSITORY", "Acme/api")
        .env("GITHUB_SERVER_URL", "https://github.com")
        .env("GITHUB_EVENT_NAME", "push")
        .env("GITHUB_REF", "refs/heads/develop")
        .env("GITHUB_SHA", w.head())
        .env("GITHUB_JOB", "unit")
        .env("GITHUB_OUTPUT", w.path("output"))
        .env("GITHUB_STEP_SUMMARY", w.path("summary"))
        .args(["attest", "--check", "unit=pass"]));
    assert_eq!(attest.code, Some(0), "{}", attest.stderr);
    // One warning still: the annotation, which the run's log and summary show.
    assert!(warnings(&attest.stderr).is_empty(), "{}", attest.stderr);
    let annotations: Vec<&str> = attest
        .stderr
        .lines()
        .filter(|line| line.starts_with("::warning "))
        .collect();
    assert_eq!(
        annotations,
        [format!(
            "::warning title=hushrun%3A check results not recorded::check results aren't recorded: {MESSAGE}; upgrade: {UPGRADE}"
        )],
        "{}",
        attest.stderr
    );
    let summary = std::fs::read_to_string(w.path("summary")).unwrap();
    assert!(
        summary.contains("- not recorded `check`: `unit`\n"),
        "{summary}"
    );
}

// covers: L-5
#[test]
fn other_refusals_still_fail_the_command() {
    let w = World::new("acme-api.yaml");
    w.fake
        .inject_on(Some("PUT"), Some(".check."), Fault::Status(503, None), 100);
    let attest = ran(w.hushrun_as(MACHINE).env("HUSHRUN_RUN_ID", "r1").args([
        "attest",
        "--check",
        "unit=pass",
    ]));
    assert_eq!(attest.code, Some(6), "{}", attest.stderr);
    assert!(warnings(&attest.stderr).is_empty(), "{}", attest.stderr);
}

/// hushrun's own lines in `stderr`.
fn hushrun_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.starts_with("hushrun: "))
        .collect()
}

/// `hushrun` in the world's repository, recording results to `results` with its token.
fn with_results(w: &World, results: &FakeResults) -> assert_cmd::Command {
    let mut cmd = w.hushrun();
    cmd.env("HUSHRUN_BACKEND", results.url())
        .env("HUSHRUN_TOKEN", FakeResults::TOKEN);
    cmd
}

// covers: C-12, C-14
#[test]
fn hushrun_run_keeps_its_commands_exit_code_when_the_orgs_plan_refuses_the_result() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.answer(
        "POST",
        "v0/results/",
        402,
        quota_exceeded(MESSAGE, UPGRADE),
        10,
    );
    // `git --version` passes and `git no-such-command` fails, on every OS.
    let passed = ran(with_results(&w, &results).args(["run", "unit", "--", "git", "--version"]));
    assert_eq!(passed.code, Some(0), "{}", passed.stderr);
    assert!(passed.stdout.contains("git version"), "{}", passed.stdout);
    let last = *hushrun_lines(&passed.stderr).last().unwrap();
    let (took, rest) = last
        .strip_prefix("hushrun: unit passed in ")
        .and_then(|rest| rest.split_once("; "))
        .unwrap_or_else(|| panic!("{}", passed.stderr));
    assert!(took.ends_with('s'), "{last}");
    assert_eq!(rest, format!("not recorded: {MESSAGE}; upgrade: {UPGRADE}"));

    let failed =
        ran(with_results(&w, &results).args(["run", "unit", "--", "git", "no-such-command"]));
    assert_eq!(failed.code, Some(1), "{}", failed.stderr);
    let last = *hushrun_lines(&failed.stderr).last().unwrap();
    let (took, rest) = last
        .strip_prefix("hushrun: unit failed in ")
        .and_then(|rest| rest.split_once("; "))
        .unwrap_or_else(|| panic!("{}", failed.stderr));
    assert!(took.ends_with("s (exit 1)"), "{last}");
    assert_eq!(rest, format!("not recorded: {MESSAGE}; upgrade: {UPGRADE}"));

    // Nothing was recorded, and each record was asked for once: a 402 is never retried.
    assert!(results.recorded().is_empty());
    let records = results
        .requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path.starts_with("v0/results/"))
        .count();
    assert_eq!(records, 2);
}

/// What the fake answers for the org's usage, with this repository's checks
/// under another case.
fn usage_answer() -> Value {
    json!({
        "org": "acme", "plan": "free", "plan_name": "Free",
        "period_start": "2026-10-01T00:00:00Z", "period_end": "2026-11-01T00:00:00Z",
        "used": 1234, "included": 2000, "limit": 2000, "overage": 0, "overage_cents": 0,
        "state": "ok",
        "repositories": [
            {"repository": "github.com/acme/api", "checks": 800},
            {"repository": "github.com/acme/shop", "checks": 434},
        ],
        "dashboard": UPGRADE,
    })
}

const NOW: &str = "2026-10-26T00:00:00Z";

// covers: C-19, C-12, C-13
#[test]
fn repository_usage_shows_the_plan_and_this_repositorys_share() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.set_usage(usage_answer());
    let usage = ran(with_results(&w, &results).args(["--now", NOW, "repository", "usage"]));
    assert_eq!(usage.code, Some(0), "{}", usage.stderr);
    assert_eq!(
        usage.stdout,
        format!(
            "backend    {} (HUSHRUN_BACKEND names it)
org        acme
plan       Free
period     2026-10-01 00:00 UTC to 2026-11-01 00:00 UTC (in 6 days)
checks     1,234 recorded, of 2,000 included
limit      2,000: past it, checks still run, but their results aren't recorded
repository github.com/Acme/api: 800 of them (65%)
dashboard  {UPGRADE}
",
            results.url()
        )
    );
    // With the credential hushrun run sends there (C-13).
    let asked: Vec<_> = results
        .requests()
        .into_iter()
        .filter(|r| r.path == "v0/usage")
        .collect();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].authorization.as_deref(),
        Some(format!("Bearer {}", FakeResults::TOKEN).as_str())
    );

    let usage = ran(with_results(&w, &results).args([
        "--now",
        NOW,
        "repository",
        "usage",
        "--format",
        "json",
    ]));
    assert_eq!(usage.code, Some(0), "{}", usage.stderr);
    let out = json_of(usage.stdout.as_bytes());
    schema::assert_valid(&schema::load("repository-usage.v1.schema.json"), &out);
    assert_eq!(out["backend"], results.url());
    assert_eq!(out["named_by"], "HUSHRUN_BACKEND");
    assert_eq!(out["repository"], "github.com/Acme/api");
    assert_eq!(out["usage"]["used"], 1234);
    assert_eq!(out["usage"]["limit"], 2000);
    assert_eq!(out["usage"]["repository_checks"], 800);
    assert_eq!(out["usage"]["repositories"][1]["checks"], 434);

    // At the limit, on a paid plan past what it includes.
    let mut over = usage_answer();
    over["plan_name"] = json!("Team");
    over["used"] = json!(20000);
    over["included"] = json!(10000);
    over["limit"] = json!(20000);
    over["overage"] = json!(10000);
    over["overage_cents"] = json!(3000);
    results.set_usage(over);
    let usage = ran(with_results(&w, &results).args(["--now", NOW, "repository", "usage"]));
    assert_eq!(usage.code, Some(0), "{}", usage.stderr);
    for line in [
        "checks     20,000 recorded, of 10,000 included\n",
        "limit      20,000, reached: checks still run, but their results aren't recorded until the period ends or the org upgrades\n",
        "overage    10,000 past what the plan includes, $30.00\n",
        "repository github.com/Acme/api: 800 of them (4%)\n",
    ] {
        assert!(usage.stdout.contains(line), "{line}{}", usage.stdout);
    }
}

/// [`usage_answer`] without this repository's checks, as for one whose checks are all free.
fn usage_answer_without_this_repository() -> Value {
    let mut answer = usage_answer();
    answer["repositories"] = json!([{"repository": "github.com/acme/shop", "checks": 1234}]);
    answer
}

/// `hushrun` outside any repository, recording results to `results` with its token.
fn outside_a_repository(w: &World, results: &FakeResults) -> assert_cmd::Command {
    let dir = w.path("outside");
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = with_results(w, results);
    cmd.current_dir(dir);
    cmd
}

/// What `repository usage` prints with `cmd`.
fn usage_text(mut cmd: assert_cmd::Command) -> String {
    let usage = ran(cmd.args(["--now", NOW, "repository", "usage"]));
    assert_eq!(usage.code, Some(0), "{}", usage.stderr);
    usage.stdout
}

/// What `repository usage --format json` prints with `cmd`, which its schema must take.
fn usage_json(mut cmd: assert_cmd::Command) -> Value {
    let usage = ran(cmd.args(["--now", NOW, "repository", "usage", "--format", "json"]));
    assert_eq!(usage.code, Some(0), "{}", usage.stderr);
    let out = json_of(usage.stdout.as_bytes());
    schema::assert_valid(&schema::load("repository-usage.v1.schema.json"), &out);
    out
}

/// What `repository usage` prints for [`usage_answer`], with `lines` before the dashboard's.
fn expected_usage(results: &FakeResults, lines: &str) -> String {
    format!(
        "backend    {} (HUSHRUN_BACKEND names it)
org        acme
plan       Free
period     2026-10-01 00:00 UTC to 2026-11-01 00:00 UTC (in 6 days)
checks     1,234 recorded, of 2,000 included
limit      2,000: past it, checks still run, but their results aren't recorded
{lines}dashboard  {UPGRADE}
",
        results.url()
    )
}

// covers: C-19, C-12
#[test]
fn repository_usage_shows_this_repositorys_free_checks() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.set_usage(usage_answer_without_this_repository());
    results.set_free(&[("github.com/Acme/api", 1204)]);
    assert_eq!(
        usage_text(with_results(&w, &results)),
        expected_usage(
            &results,
            "repository github.com/Acme/api: none of them
free       github.com/Acme/api: 1,204 recorded for free, outside the plan
"
        )
    );

    // One free check, beside checks the plan counts: neither number moves the other.
    results.set_usage(usage_answer());
    results.set_free(&[("github.com/Acme/api", 1)]);
    assert_eq!(
        usage_text(with_results(&w, &results)),
        expected_usage(
            &results,
            "repository github.com/Acme/api: 800 of them (65%)
free       github.com/Acme/api: 1 recorded for free, outside the plan
"
        )
    );
}

// covers: C-19
#[test]
fn repository_usage_finds_free_checks_under_any_case() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.set_usage(usage_answer_without_this_repository());
    // The server keeps each spelling apart, as it does in `repositories`.
    results.set_free(&[("github.com/ACME/API", 1200), ("github.com/acme/api", 4)]);
    assert_eq!(
        usage_text(with_results(&w, &results)),
        expected_usage(
            &results,
            "repository github.com/Acme/api: none of them
free       github.com/Acme/api: 1,204 recorded for free, outside the plan
"
        )
    );
}

// covers: C-19
#[test]
fn repository_usage_has_no_free_line_for_a_repository_without_free_checks() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.set_usage(usage_answer());
    let without = expected_usage(
        &results,
        "repository github.com/Acme/api: 800 of them (65%)\n",
    );
    // Another of the org's repositories has some, and this one's entry says none.
    results.set_free(&[("github.com/acme/oss", 1204), ("github.com/Acme/api", 0)]);
    assert_eq!(usage_text(with_results(&w, &results)), without);
    results.set_free(&[]);
    assert_eq!(usage_text(with_results(&w, &results)), without);
}

// covers: C-19
#[test]
fn repository_usage_shows_the_orgs_free_checks_outside_a_repository() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.set_usage(usage_answer());
    results.set_free(&[("github.com/acme/oss", 1000), ("github.com/acme/docs", 204)]);
    assert_eq!(
        usage_text(outside_a_repository(&w, &results)),
        expected_usage(
            &results,
            "free       1,204 recorded for free, outside the plan\n"
        )
    );
    let out = usage_json(outside_a_repository(&w, &results));
    assert_eq!(out["repository"], Value::Null);
    assert_eq!(out["usage"]["repository_checks"], Value::Null);
    assert_eq!(
        out["usage"]["free"],
        json!([
            {"repository": "github.com/acme/oss", "checks": 1000},
            {"repository": "github.com/acme/docs", "checks": 204},
        ])
    );

    // With none, there's no line.
    results.set_free(&[]);
    assert_eq!(
        usage_text(outside_a_repository(&w, &results)),
        expected_usage(&results, "")
    );
}

// covers: C-19, C-12
#[test]
fn repository_usage_json_lists_free_checks_apart_from_the_plans() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.set_usage(usage_answer());
    results.set_free(&[("github.com/acme/api", 1000), ("github.com/acme/oss", 204)]);
    let out = usage_json(with_results(&w, &results));
    assert_eq!(
        out["usage"]["free"],
        json!([
            {"repository": "github.com/acme/api", "checks": 1000},
            {"repository": "github.com/acme/oss", "checks": 204},
        ])
    );
    assert_eq!(out["usage"]["used"], 1234);
    assert_eq!(out["usage"]["repository_checks"], 800);
    assert_eq!(out["usage"]["repositories"][0]["checks"], 800);

    // A server with none says so, which isn't the same as not saying.
    results.set_free(&[]);
    assert_eq!(
        usage_json(with_results(&w, &results))["usage"]["free"],
        json!([])
    );
}

// covers: C-19, C-12
#[test]
fn repository_usage_of_a_server_without_free_prints_what_it_always_did() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.set_usage(usage_answer_without_this_repository());
    assert_eq!(
        usage_text(with_results(&w, &results)),
        expected_usage(&results, "repository github.com/Acme/api: none of them\n")
    );
    assert_eq!(
        usage_text(outside_a_repository(&w, &results)),
        expected_usage(&results, "")
    );
    // Every field, so that one more of them shows.
    assert_eq!(
        usage_json(with_results(&w, &results)),
        json!({
            "schema": "hushrun.repository.usage/v1",
            "backend": results.url(),
            "named_by": "HUSHRUN_BACKEND",
            "directory": null,
            "off": null,
            "repository": "github.com/Acme/api",
            "usage": {
                "org": "acme", "plan": "free", "plan_name": "Free",
                "period_start": "2026-10-01T00:00:00Z", "period_end": "2026-11-01T00:00:00Z",
                "used": 1234, "included": 2000, "limit": 2000, "overage": 0, "overage_cents": 0,
                "state": "ok",
                "repositories": [{"repository": "github.com/acme/shop", "checks": 1234}],
                "repository_checks": 0,
                "dashboard": UPGRADE,
            },
        })
    );
}

// covers: C-19
#[test]
fn repository_usage_says_when_nothing_is_metered() {
    let w = World::new("acme-api.yaml");
    let usage_schema = schema::load("repository-usage.v1.schema.json");
    // No server: results stay on this machine.
    let local = ran(w.hushrun().args(["repository", "usage"]));
    assert_eq!(local.code, Some(0), "{}", local.stderr);
    assert!(
        local
            .stdout
            .contains("usage      results stay on this machine, so nothing is metered\n"),
        "{}",
        local.stdout
    );
    let out = json_of(
        ran(w
            .hushrun()
            .args(["repository", "usage", "--format", "json"]))
        .stdout
        .as_bytes(),
    );
    schema::assert_valid(&usage_schema, &out);
    assert_eq!(out["usage"], Value::Null);
    assert_eq!(out["backend"], Value::Null);
    assert_eq!(
        Path::new(out["directory"].as_str().unwrap()),
        w.cache.join("results")
    );
    // Results off.
    let off = ran(w
        .hushrun()
        .env("HUSHRUN_BACKEND", "")
        .args(["repository", "usage"]));
    assert_eq!(off.code, Some(0), "{}", off.stderr);
    assert!(
        off.stdout
            .contains("backend    none: results are off (HUSHRUN_BACKEND is empty)\n"),
        "{}",
        off.stdout
    );

    // A server that doesn't report usage answers 404, or 405.
    let results = FakeResults::start();
    let unreported = ran(with_results(&w, &results).args(["repository", "usage"]));
    assert_eq!(unreported.code, Some(0), "{}", unreported.stderr);
    assert!(
        unreported.stdout.contains(&format!(
            "usage      this server doesn't report usage: GET {}/v0/usage answered 404\n",
            results.url()
        )),
        "{}",
        unreported.stdout
    );
    results.answer(
        "GET",
        "v0/usage",
        405,
        json!({"error": {"code": "invalid", "message": "no such method"}}),
        1,
    );
    let out = json_of(
        ran(with_results(&w, &results).args(["repository", "usage", "--format", "json"]))
            .stdout
            .as_bytes(),
    );
    schema::assert_valid(&usage_schema, &out);
    assert_eq!(out["usage"], Value::Null);
    assert_eq!(out["backend"], results.url());
}

// covers: C-19
#[test]
fn repository_usage_exits_6_when_the_server_refuses_or_cant_be_reached() {
    let w = World::new("acme-api.yaml");
    let results = FakeResults::start();
    results.set_usage(usage_answer());
    // Without its token, the server refuses: its message, and exit 6.
    let refused = ran(w
        .hushrun()
        .env("HUSHRUN_BACKEND", results.url())
        .args(["repository", "usage"]));
    assert_eq!(refused.code, Some(6), "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("reading the org's usage: 401: no token: run hushrun login"),
        "{}",
        refused.stderr
    );
    assert!(refused.stdout.is_empty(), "{}", refused.stdout);
    // An answer that isn't the org's usage.
    results.set_usage(json!({"plan": "free"}));
    let garbled = ran(with_results(&w, &results).args(["repository", "usage"]));
    assert_eq!(garbled.code, Some(6), "{}", garbled.stderr);
    assert!(
        garbled.stderr.contains("didn't answer the org's usage"),
        "{}",
        garbled.stderr
    );
    // Nothing listens on the discard port.
    let gone = ran(w
        .hushrun()
        .env("HUSHRUN_BACKEND", "http://127.0.0.1:9/o/acme")
        .args(["repository", "usage"]));
    assert_eq!(gone.code, Some(6), "{}", gone.stderr);
}

fn millis(time: &str) -> i64 {
    Time::parse(time).unwrap().millis()
}

/// Stores an event key in the world's server, written at `time`: stats read only keys.
fn put_event(
    w: &World,
    scope: &str,
    digest: &Digest,
    time: &str,
    kind: EventKind,
    stage: Option<&str>,
) -> String {
    let stage = stage.map(|s| StageName::new(s).unwrap());
    let ulid = hushrun_ledger::new_ulid(millis(time));
    let key = event_key(
        &ScopeName::new(scope).unwrap(),
        digest,
        &ulid,
        kind,
        stage.as_ref(),
    );
    w.put_object(&key, b"{}");
    key
}

// covers: L-14, L-2
#[test]
fn repository_stats_counts_one_listing_of_the_repositorys_events() {
    let w = World::new("acme-api.yaml");
    let (one, two, old) = (Digest::of(b"one"), Digest::of(b"two"), Digest::of(b"old"));
    use EventKind::{Artifact, Check, Deployed, Passed, Started};
    // In the last 30 days. Most are as hushrun 0.8 recorded them, with a stage: they count all the
    // same (ADR 0062).
    put_event(
        &w,
        "default",
        &one,
        "2026-10-20T10:00:00Z",
        Started,
        Some("staging"),
    );
    put_event(
        &w,
        "default",
        &one,
        "2026-10-20T10:01:00Z",
        Check,
        Some("staging"),
    );
    put_event(
        &w,
        "default",
        &one,
        "2026-10-20T10:02:00Z",
        Check,
        Some("staging"),
    );
    put_event(
        &w,
        "default",
        &one,
        "2026-10-20T10:03:00Z",
        Artifact,
        Some("staging"),
    );
    put_event(
        &w,
        "default",
        &one,
        "2026-10-21T10:00:00Z",
        Deployed,
        Some("staging"),
    );
    // The deployed index isn't under v1/events/, so its copy isn't counted again.
    w.put_object(
        &deployed_key(
            &ScopeName::new("default").unwrap(),
            Some(&StageName::new("staging").unwrap()),
            Time::parse("2026-10-21T10:00:00Z").unwrap(),
            &hushrun_ledger::new_ulid(millis("2026-10-21T10:00:00Z")),
        ),
        b"{}",
    );
    put_event(&w, "default", &two, "2026-10-25T10:00:00Z", Check, None);
    put_event(
        &w,
        "api",
        &two,
        "2026-10-25T11:00:00Z",
        Check,
        Some("production"),
    );
    // Before the last 30 days.
    put_event(
        &w,
        "default",
        &old,
        "2026-08-01T00:00:00Z",
        Check,
        Some("staging"),
    );
    put_event(
        &w,
        "default",
        &old,
        "2026-08-01T00:01:00Z",
        Passed,
        Some("staging"),
    );
    // Not an event's key (L-2): not counted.
    w.put_object(
        &format!("v1/events/default/{}/notes.json", one.hex()),
        b"{}",
    );

    let before = w.fake.requests().len();
    let stats = ran(w
        .hushrun()
        .args(["--now", NOW, "repository", "stats", "--format", "json"]));
    assert_eq!(stats.code, Some(0), "{}", stats.stderr);
    let out = json_of(stats.stdout.as_bytes());
    schema::assert_valid(&schema::load("repository-stats.v2.schema.json"), &out);
    assert_eq!(out["source"], "repository");
    assert_eq!(out["repository"], w.ledger_url());
    assert_eq!(out["since"], "30d");
    assert_eq!(out["from"], "2026-09-26T00:00:00.000Z");
    assert_eq!(out["until"], "2026-10-26T00:00:00.000Z");
    assert_eq!(out["events"], 7);
    assert_eq!(
        out["byKind"],
        json!({"started": 1, "check": 4, "artifact": 1, "deployed": 1, "passed": 0, "failed": 0, "superseded": 0, "approval": 0})
    );
    assert_eq!(out["digests"], 3, "default/one, default/two and api/two");
    assert_eq!(out.get("checksByStage"), None, "{out}");
    // One listing, and nothing else: no event is read.
    let seen = &w.fake.requests()[before..];
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("GET", "objects")
    );
    assert!(
        seen[0]
            .query
            .contains(&("prefix".to_string(), "v1/events/".to_string())),
        "{seen:?}"
    );

    // A longer window counts the older events too; text is for pasting.
    let stats = ran(w
        .hushrun()
        .args(["--now", NOW, "repository", "stats", "--since", "120d"]));
    assert_eq!(stats.code, Some(0), "{}", stats.stderr);
    assert_eq!(
        stats.stdout,
        format!(
            "events in the last 120d (since 2026-06-28 00:00 UTC), at {}
total            9
by kind          check 5, started 1, artifact 1, deployed 1, passed 1
digests          4
",
            w.ledger_url()
        )
    );

    // A server that refuses the listing: exit 6 (L-5).
    let refused = ran(w
        .hushrun()
        .env_remove("HUSHRUN_REPOSITORY_TOKEN")
        .args(["repository", "stats"]));
    assert_eq!(refused.code, Some(6), "{}", refused.stderr);
}

// covers: L-14
#[test]
fn repository_stats_help_says_it_is_for_sharing_volume_without_telemetry() {
    let w = World::new("acme-api.yaml");
    let help = ran(w.hushrun().args(["repository", "stats", "--help"]));
    assert_eq!(help.code, Some(0), "{}", help.stderr);
    assert!(
        help.stdout.contains("to share volume without telemetry"),
        "{}",
        help.stdout
    );
}

// covers: L-14, C-11
#[test]
fn repository_stats_local_counts_this_machines_results_and_sends_nothing() {
    let w = World::new("acme-api.yaml");
    let at = "2026-10-20T10:00:00Z";
    let run = |args: &[&str]| ran(w.hushrun().args(["--now", at]).args(args));
    // A pass, the skip it gives, and a failure, kept in the cache directory (C-11).
    assert_eq!(
        run(&["run", "unit", "--", "git", "--version"]).code,
        Some(0)
    );
    let skipped = run(&["run", "unit", "--", "git", "--version"]);
    assert!(
        skipped.stderr.contains("unit skipped"),
        "{}",
        skipped.stderr
    );
    assert_eq!(
        run(&["run", "lint", "--", "git", "no-such-command"]).code,
        Some(1)
    );
    let results = w.cache.join("results");
    // The pass's time is what the skip saved.
    let pass: Value = {
        let dir = std::fs::read_dir(results.join("results/v0"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .find_map(|key| {
                let file = std::fs::read_dir(&key).unwrap().next()?.unwrap().path();
                let result: Value = json_of(&std::fs::read(&file).unwrap());
                (result["check"] == "unit").then_some(result)
            });
        dir.expect("unit's pass")
    };
    // An older result, and a file that isn't one.
    let old = results.join(format!(
        "results/v0/{}/{}.json",
        "e".repeat(64),
        hushrun_ledger::new_ulid(millis("2026-08-01T00:00:00Z"))
    ));
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    let mut old_result = pass.clone();
    old_result["check"] = json!("e2e");
    std::fs::write(&old, serde_json::to_vec(&old_result).unwrap()).unwrap();
    let junk = results.join(format!(
        "results/v0/{}/{}.json",
        "f".repeat(64),
        hushrun_ledger::new_ulid(millis("2026-10-21T00:00:00Z"))
    ));
    std::fs::create_dir_all(junk.parent().unwrap()).unwrap();
    std::fs::write(&junk, "not a result").unwrap();

    let before = w.fake.requests().len();
    let stats = ran(w.hushrun().args([
        "--now",
        NOW,
        "repository",
        "stats",
        "--local",
        "--format",
        "json",
    ]));
    assert_eq!(stats.code, Some(0), "{}", stats.stderr);
    let out = json_of(stats.stdout.as_bytes());
    schema::assert_valid(&schema::load("repository-stats.v2.schema.json"), &out);
    assert_eq!(out["source"], "local");
    assert_eq!(Path::new(out["directory"].as_str().unwrap()), results);
    assert_eq!(out["results"], 3);
    assert_eq!(out["byVerdict"], json!({"pass": 1, "fail": 1, "skip": 1}));
    assert_eq!(out["keys"], 2, "unit's run and lint's");
    assert_eq!(out["byCheck"], json!({"unit": 2, "lint": 1}));
    assert_eq!(out["savedMs"], pass["durationMs"]);
    // It sends nothing.
    assert_eq!(w.fake.requests().len(), before);

    let stats = ran(w.hushrun().args([
        "--now",
        NOW,
        "repository",
        "stats",
        "--local",
        "--since",
        "120d",
    ]));
    assert_eq!(stats.code, Some(0), "{}", stats.stderr);
    let lines: Vec<&str> = stats.stdout.lines().collect();
    assert_eq!(
        lines[0],
        format!(
            "hushrun run's results on this machine in the last 120d (since 2026-06-28 00:00 UTC), in {}",
            results.display()
        )
    );
    assert_eq!(
        &lines[1..5],
        [
            "total            4",
            "by verdict       pass 2, fail 1, skip 1",
            "run keys         3",
            "by check         unit 2, e2e 1, lint 1",
        ]
    );
    assert!(
        lines[5].starts_with("saved by skips   "),
        "{}",
        stats.stdout
    );
    assert_eq!(w.fake.requests().len(), before);
}
