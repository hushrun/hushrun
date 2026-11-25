//! \[ADR 0069\] `tests: reuse` with pytest (C-5, C-10, C-22, C-23, C-26, C-27, C-32 to C-34): the
//! tests of `reuse.rs` that only the CLI shows. A run's last line, the check's own result and
//! skip, `--force`, `hushrun repository stats`, the audit's messages and its suspension, a person
//! turning reuse on again, files that change while COMMAND runs, a results server, CI's
//! detection, `hushrun ready` and the pre-push hook are all the CLI's own (`cmd::wrap` and the
//! commands around it). Every other test of `reuse.rs` is in-process, and the fixtures both share
//! are in `common/reuse.rs`.

use std::path::{Path, PathBuf};

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::git;
use serde_json::{Value, json};

use super::python::{PYTEST_CLEARED, has_pytest, python, tools};
use super::repo::Repo;

#[path = "../common/reuse.rs"]
mod fixtures;

use fixtures::{REPOSITORY, hidden, shop};

/// What a start of the binary came to.
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// The line that says what the plan reuses, or why every test file runs.
    fn plan_line(&self) -> &str {
        self.stderr
            .lines()
            .find(|l| {
                // \[C-39\] In any adapter's word: `test files`, `test binaries`, `packages`.
                [
                    "test file",
                    "test files",
                    "test binary",
                    "test binaries",
                    "package",
                    "packages",
                    "test program",
                    "test programs",
                    "test class",
                    "test classes",
                    "test target",
                    "test targets",
                ]
                .iter()
                .any(|w| l.contains(&format!("{w} run")))
                    || l.contains("every test file runs")
            })
            .unwrap_or_else(|| panic!("no plan line in:\n{}", self.stderr))
    }
}

/// The binary in a [`Repo`], as these tests start it.
trait Binary {
    /// `hushrun ARGS`, with `env` besides, to its end.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran;
    /// The newest result this machine recorded for the check.
    fn newest_result(&self) -> Value;
    /// How many unit results this machine holds.
    fn unit_count(&self) -> usize;
}

impl Binary for Repo {
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut cmd = super::cli::Cli::hushrun(self);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("RUSTC_WORKSPACE_WRAPPER") {
                cmd.env_remove(name.as_ref());
            }
        }
        for (name, value) in env {
            cmd.env(name, value);
        }
        let output = cmd.args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn newest_result(&self) -> Value {
        super::cli::Cli::newest_result(self)
    }

    fn unit_count(&self) -> usize {
        self.unit_results().values().sum()
    }
}

fn repo_of(files: &[(String, String)]) -> Repo {
    Repo::new(files).clear(PYTEST_CLEARED)
}

fn pytest_args<'a>(python: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "run",
        "unit",
        "--",
        python,
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
    ];
    args.extend(extra);
    args
}

// covers: C-34, C-10, C-27, C-33
// What `reuse.rs`'s first test checks of the CLI: the run's last line, the check's result with
// what reusing saved, this machine's stats, the skip of the same code, and `--force`.
#[test]
fn a_run_that_reuses_says_so_records_it_skips_the_same_code_and_force_runs_everything() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&shop("reuse"));
    let python = python();
    let pytest = |extra: &[&str]| r.run(&pytest_args(&python, extra), &[]);

    let first = pytest(&[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert!(!first.stderr.contains("test files run"), "{}", first.stderr);
    assert_eq!(r.unit_count(), 7, "every observed test file that passed");

    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let changed = pytest(&[]);
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    assert_eq!(
        changed.plan_line(),
        "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(
        changed.stderr.contains("unit passed in") && changed.stderr.contains("5 test files"),
        "{}",
        changed.stderr
    );
    // A pass with reused test files is a pass (D2), with its counts.
    let result = r.newest_result();
    assert_eq!(result["result"], "pass");
    // \[ADR 0074\] With what reusing saved: the reused test files' recorded durations, added up.
    let saved = result["units"]["savedMs"].as_u64().expect("savedMs");
    assert_eq!(
        result["units"],
        json!({"total": 8, "ran": 3, "reused": 5, "savedMs": saved})
    );
    // Reuse never chains: a reused test file records nothing.
    assert_eq!(r.unit_count(), 7 + 3 - 1, "{}", changed.stderr);
    // \[ADR 0074\] This machine's stats say it, beside what skips saved.
    let stats = r.run(&["repository", "stats", "--local", "--format", "json"], &[]);
    assert_eq!(stats.code, 0, "{}", stats.stderr);
    let stats: Value = serde_json::from_str(&stats.stdout).unwrap();
    assert_eq!(
        stats["testFiles"],
        json!({"runs": 2, "ran": 8 + 3, "reused": 5, "savedMs": saved}),
        "{stats:#}"
    );
    let text = r.run(&["repository", "stats", "--local"], &[]).stdout;
    assert!(
        text.contains("test files       11 ran, 5 reused, in 2 runs\nsaved by reuse   "),
        "{text}"
    );
    // The same code again skips the whole check (C-10).
    let again = pytest(&[]);
    assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);

    // --force runs everything, and says so.
    r.write("app/extra.py", "X = 2\n");
    let forced = r.run(
        &[
            "run",
            "--force",
            "unit",
            "--",
            &python,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
        ],
        &[],
    );
    assert_eq!(
        forced.plan_line(),
        "hushrun: unit: every test file runs: --force runs it whole"
    );
    assert_eq!(r.newest_result()["units"]["ranAnyway"], "force");
}

fn with_server(results: &FakeResults) -> Vec<(&'static str, String)> {
    vec![
        ("HUSHRUN_BACKEND", results.url()),
        ("HUSHRUN_TOKEN", FakeResults::TOKEN.to_string()),
    ]
}

fn envs<'a>(pairs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    pairs.iter().map(|(n, v)| (*n, v.as_str())).collect()
}

/// Every file under `dir` with its length and time, to tell that nothing was written.
fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else {
                out.push((path, meta.len(), meta.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

// covers: C-34
#[test]
fn an_injected_missed_dependency_is_caught_by_an_audit_and_suspends_reuse() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let pytest = || r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    // The recorded pass of test_hidden.py is wrong for what it reads now: the flag, which its key
    // can't see. Only it could be reused, so it is the one audited.
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    let caught = pytest();
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        caught.plan_line(),
        "hushrun: unit: all 2 test files run; 1 that already passed on the files it read runs anyway, as a check"
    );
    assert!(
        caught.stderr.contains("unit: test_hidden.py failed, though it passed before on the same files it read: running it once more"),
        "{}",
        caught.stderr
    );
    assert!(
        caught.stderr.contains("unit: test_hidden.py failed again, though it passed before on the same files it read: reuse of unit's test files is suspended until a person turns it on again with `hushrun tests resume unit`"),
        "{}",
        caught.stderr
    );
    // The next run reuses nothing, and says why.
    r.write("libb.py", "Y = 1  # again\n");
    let suspended = pytest();
    assert!(
        suspended.plan_line().starts_with(
            "hushrun: unit: every test file runs: reuse of its test files is suspended by a person"
        ),
        "{}",
        suspended.stderr
    );
    assert_eq!(r.newest_result()["units"]["ranAnyway"], "suspended");
    // A coding agent can't turn it on again; a person can, once.
    let agent = r.run(&["tests", "resume", "unit"], &[("CLAUDECODE", "1")]);
    assert_eq!(agent.code, 2, "{}", agent.stderr);
    assert!(
        agent.stderr.contains("a person turns reuse"),
        "{}",
        agent.stderr
    );
    std::fs::remove_file(r.repo.join("flag")).unwrap();
    let resumed = r.run(&["tests", "resume", "unit"], &[]);
    assert_eq!(resumed.code, 0, "{}", resumed.stderr);
    assert!(
        resumed.stdout.contains("was suspended by a person")
            && resumed.stdout.contains("when test_hidden.py failed twice")
            && resumed.stdout.contains("Turned it on again"),
        "{}",
        resumed.stdout
    );
    let twice = r.run(&["tests", "resume", "unit"], &[]);
    assert!(
        twice.stdout.contains("is on for example.com/acme/shop"),
        "{}",
        twice.stdout
    );
    // On again: test_hidden.py's newest result is the fail its audit recorded, so it runs; and a
    // pass then makes it reusable again.
    r.write("libb.py", "Y = 1  # on again\n");
    let on = pytest();
    assert_eq!(on.code, 0, "{}", on.stderr);
    assert_eq!(
        on.plan_line(),
        "hushrun: unit: all 2 test files run: none had passed here yet on the files it read"
    );
}

// covers: C-34
#[test]
fn a_test_file_that_passes_when_it_runs_again_is_flaky_and_suspends_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let mut files = hidden("reuse");
    // It fails the first time it runs after `seen` is gone, and passes after.
    files[4].1 = "import os, liba\nSEEN = os.path.join(os.path.dirname(__file__), 'seen')\n\ndef test_x():\n    if os.path.exists(SEEN):\n        return\n    open(SEEN, 'w').close()\n    assert False, 'first time'\n".into();
    let r = repo_of(&files);
    r.write("seen", "");
    let python = python();
    let pytest = || r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(pytest().code, 0);
    std::fs::remove_file(r.repo.join("seen")).unwrap();
    r.write("libb.py", "Y = 1  # changed\n");
    let flaky = pytest();
    // COMMAND failed, and its exit code is hushrun's; the second run passed: flaky, as G13 says of
    // checks, and reuse stays on.
    assert_eq!(flaky.code, 1, "{}", flaky.stderr);
    assert!(
        flaky.stderr.contains(
            "unit: test_hidden.py failed, then passed when it ran again: it may be flaky"
        ),
        "{}",
        flaky.stderr
    );
    assert!(!flaky.stderr.contains("suspended"), "{}", flaky.stderr);
}

// covers: C-32
#[test]
fn files_that_change_while_command_runs_record_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let ran = r.run(
        &[
            "run",
            "unit",
            "--",
            "sh",
            "-c",
            &format!("{python} -m pytest -q -p no:cacheprovider && echo 'Z = 1' > libb.py"),
        ],
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains("files changed while it ran"),
        "{}",
        ran.stderr
    );
    assert_eq!(r.unit_count(), 0);
}

// covers: C-33, C-34
#[test]
fn a_server_keeps_test_files_with_their_run_and_ci_reuses_them_with_the_map_alone() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let server = with_server(&results);
    let r = repo_of(&shop("reuse"));
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(first.code, 0, "{}", first.stderr);
    // The run and its test files went in one request, and nothing of them stays here.
    assert_eq!(results.unit_results(), 7);
    assert!(
        results
            .requests()
            .iter()
            .any(|s| s.path.starts_with("v0/runs/"))
    );
    assert_eq!(r.unit_count(), 0);
    // Another machine, as CI is: an empty cache, but the map, which CI's cache carries (D3).
    let ci_cache = r.root.join("ci-cache");
    std::fs::create_dir_all(ci_cache.join("units")).unwrap();
    for entry in std::fs::read_dir(r.cache.join("units")).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // \[ADR 0072\] The store, as a cache step saves it once hushrun has exited.
        if name.ends_with(".v2.sqlite") {
            std::fs::copy(entry.path(), ci_cache.join("units").join(name)).unwrap();
        }
    }
    let mut ci = envs(&server);
    ci.push(("HUSHRUN_CACHE_DIR", ci_cache.to_str().unwrap()));
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    let reused = r.run(&pytest_args(&python, &[]), &ci);
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert_eq!(
        reused.plan_line(),
        "hushrun: unit: 3 of 8 test files run; 5 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(
        results
            .recorded()
            .iter()
            .any(|r| r["units"]["savedMs"].is_u64()
                && r["units"]
                    == json!({"total": 8, "ran": 3, "reused": 5, "savedMs": r["units"]["savedMs"]})),
        "{:?}",
        results.recorded()
    );
    assert!(
        results
            .requests()
            .iter()
            .any(|s| s.path == "v0/units/lookup")
    );
}

// covers: C-34
#[test]
fn a_server_keeps_the_suspension_and_a_person_resumes_it_there() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let server = with_server(&results);
    let r = repo_of(&hidden("reuse"));
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &envs(&server)).code, 0);
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    let caught = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        results.reuse_state(REPOSITORY, "unit")["state"],
        "suspended"
    );
    r.write("libb.py", "Y = 1  # again\n");
    let suspended = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert!(
        suspended
            .plan_line()
            .contains("reuse of its test files is suspended"),
        "{}",
        suspended.stderr
    );
    let resumed = r.run(&["tests", "resume", "unit"], &envs(&server));
    assert_eq!(resumed.code, 0, "{}", resumed.stderr);
    assert_eq!(results.reuse_state(REPOSITORY, "unit")["state"], "on");
}

// covers: C-34
#[test]
fn a_suspension_that_cant_reach_the_server_waits_and_nothing_is_reused_meanwhile() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let server = with_server(&results);
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let outbox =
        || std::fs::read_dir(r.cache.join("units/outbox")).map_or(0, |d| d.flatten().count());
    let reuse_requests = || {
        results
            .requests()
            .iter()
            .filter(|s| s.path == "v0/units/reuse")
            .count()
    };
    assert_eq!(r.run(&pytest_args(&python, &[]), &envs(&server)).code, 0);
    // The server fails every request that would suspend, each time it is tried (L-4 retries).
    results.answer("POST", "v0/units/reuse", 503, json!({}), 4);
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    let caught = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert!(
        caught.stderr.contains("couldn't be suspended")
            && caught
                .stderr
                .contains("it is sent before the next run reuses anything"),
        "{}",
        caught.stderr
    );
    // It waits on this machine, and the server's state is as it was.
    assert_eq!(outbox(), 1);
    assert_eq!(results.reuse_state(REPOSITORY, "unit")["state"], "on");
    // While it waits, nothing is reused, and the run says why.
    let tries = reuse_requests();
    results.answer("POST", "v0/units/reuse", 503, json!({}), tries);
    r.write("libb.py", "Y = 1  # again\n");
    let waiting = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(waiting.code, 1, "{}", waiting.stderr);
    assert!(
        waiting.plan_line().starts_with(
            "hushrun: unit: every test file runs: can't read which passed (a suspension of unit's reuse hasn't reached"
        ),
        "{}",
        waiting.stderr
    );
    assert_eq!(outbox(), 1);
    assert!(
        results
            .recorded()
            .iter()
            .all(|r| r["units"]["reused"].as_u64().unwrap_or(0) == 0),
        "{:?}",
        results.recorded()
    );
    // The server answers again: the next plan sends the suspension before it reads anything.
    assert_eq!(reuse_requests(), 2 * tries);
    let before_plan = results.requests().len();
    r.write("libb.py", "Y = 1  # once more\n");
    let sent = r.run(&pytest_args(&python, &[]), &envs(&server));
    let paths: Vec<String> = results.requests()[before_plan..]
        .iter()
        .map(|s| s.path.clone())
        .collect();
    let reuse_at = paths.iter().position(|p| p == "v0/units/reuse");
    let lookup_at = paths.iter().position(|p| p == "v0/units/lookup");
    assert!(
        reuse_at.is_some() && reuse_at < lookup_at,
        "{paths:?}\n{}",
        sent.stderr
    );
    assert_eq!(outbox(), 0);
    assert_eq!(
        results.reuse_state(REPOSITORY, "unit")["state"],
        "suspended"
    );
    assert!(
        sent.plan_line()
            .contains("reuse of its test files is suspended"),
        "{}",
        sent.stderr
    );
}

// covers: C-33
#[test]
fn an_older_server_or_one_that_doesnt_answer_means_every_test_file_runs() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    results.forget_units();
    let server = with_server(&results);
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let old = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(old.code, 0, "{}", old.stderr);
    // The run itself is recorded, as a server from before takes it, and said once.
    assert_eq!(results.recorded().len(), 1);
    assert!(
        old.stderr.contains("test files' results weren't kept"),
        "{}",
        old.stderr
    );
    // The next plan asks it, and says what it answered.
    r.write("liba.py", "X = 1  # changed\n");
    let old = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(old.code, 0, "{}", old.stderr);
    assert_eq!(
        old.plan_line(),
        "hushrun: unit: every test file runs: can't read which passed (its server doesn't keep test files' results yet)"
    );
    // A server that can't be reached: the check runs, and so does every test file.
    let gone = [
        ("HUSHRUN_BACKEND", "http://127.0.0.1:9/o/acme".to_string()),
        ("HUSHRUN_TOKEN", FakeResults::TOKEN.to_string()),
    ];
    r.write("libb.py", "Y = 1  # changed\n");
    let down = r.run(&pytest_args(&python, &[]), &envs(&gone));
    assert_eq!(down.code, 0, "{}", down.stderr);
    assert!(
        down.plan_line()
            .starts_with("hushrun: unit: every test file runs: can't read which passed"),
        "{}",
        down.stderr
    );
}

// covers: C-26, C-34
#[test]
fn in_ci_where_a_later_step_reads_reports_every_test_file_runs() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let mut env = with_server(&results);
    env.push(("CI", "true".to_string()));
    env.push(("HUSHRUN_REPORTS_READ", "true".to_string()));
    let r = repo_of(&hidden("reuse"));
    let python = python();
    let ran = r.run(&pytest_args(&python, &[]), &envs(&env));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(
        ran.plan_line(),
        "hushrun: unit: every test file runs: HUSHRUN_REPORTS_READ says this job reads reports, and a test file it reuses writes none"
    );
    assert!(
        results
            .recorded()
            .iter()
            .any(|r| r["units"]["ranAnyway"] == "reports"),
        "{:?}",
        results.recorded()
    );
}

// covers: C-22, C-34
#[test]
fn ready_says_how_many_test_files_a_run_would_run_and_writes_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let python = python();
    let script = format!("hushrun run unit -- {python} -m pytest -q -p no:cacheprovider");
    let mut files = shop("reuse");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"{script}\"}}}}\n"
        ),
    ));
    let r = repo_of(&files);
    let ran = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    git::commit_all(&r.repo, "cart");
    let before = [
        snapshot(&r.cache.join("units")),
        snapshot(&r.cache.join("results")),
    ]
    .concat();
    let ready = r.run(&["ready"], &[]);
    assert_eq!(ready.code, 10, "{}\n{}", ready.stdout, ready.stderr);
    // Results on this machine: what a run here would do, in local mode's words.
    assert!(
        ready.stdout.contains(
            "unit: a run here would run 3 of 8 test files; 5 already passed here on the files they read"
        ),
        "{}",
        ready.stdout
    );
    assert_eq!(
        [
            snapshot(&r.cache.join("units")),
            snapshot(&r.cache.join("results"))
        ]
        .concat(),
        before,
        "ready wrote to the cache's units or results"
    );
    let json = r.run(&["ready", "--format", "json"], &[]);
    let doc: Value = serde_json::from_str(&json.stdout).unwrap();
    assert!(
        doc["notes"].as_array().unwrap().iter().any(|n| n
            .as_str()
            .unwrap()
            .starts_with("unit: a run here would run 3 of 8")),
        "{doc:#}"
    );
    // Its files said otherwise: not the files a plan reads, so nothing is said.
    r.write(
        "app/greet.py",
        "def hello():\n    return 'hi'  # not committed\n",
    );
    let ready = r.run(&["ready"], &[]);
    assert!(!ready.stdout.contains("test files"), "{}", ready.stdout);
}

// covers: C-23, C-22, C-34
#[test]
fn the_pre_push_hook_says_how_many_test_files_a_run_would_run() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let python = python();
    let script = format!("hushrun run unit -- {python} -m pytest -q -p no:cacheprovider");
    let mut files = shop("reuse");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"{script}\"}}}}\n"
        ),
    ));
    let r = repo_of(&files);
    assert_eq!(r.run(&pytest_args(&python, &[]), &[]).code, 0);
    r.write("app/cart.py", "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)  # changed\n");
    git::commit_all(&r.repo, "cart");
    let head = String::from_utf8(git::run(&r.repo, &["rev-parse", "HEAD"], None)).unwrap();
    let line = format!(
        "refs/heads/main {} refs/heads/main {}\n",
        head.trim(),
        "0".repeat(40)
    );
    // A coding agent's push waits for the check, and the hook says what its run would run.
    let mut hook = super::cli::Cli::hushrun(&r);
    hook.env("CLAUDECODE", "1")
        .args([
            "hook",
            "pre-push",
            "origin",
            "git@example.com:acme/shop.git",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = hook.spawn().expect("hushrun");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(line.as_bytes()).expect("the hook's stdin");
    }
    let output = child.wait_with_output().expect("hushrun");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains("push refused"), "{stderr}");
    assert!(
        stderr.contains(
            "unit: a run here would run 3 of 8 test files; 5 already passed here on the files they read"
        ),
        "{stderr}"
    );
}

// covers: C-22, C-34, C-30
#[test]
fn a_check_where_no_pytest_ever_reported_says_nothing_of_test_files() {
    // A check whose COMMAND hushrun doesn't recognize gets the pytest adapter, which does nothing
    // where pytest never starts: its plan and ready count no file that only looks like a test.
    let script = "hushrun run unit -- git --version";
    let mut files = shop("reuse");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"{script}\"}}}}\n"
        ),
    ));
    let r = repo_of(&files);
    let ran = r.run(&["run", "unit", "--", "git", "--version"], &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(!ran.stderr.contains("test file"), "{}", ran.stderr);
    r.write("app/greet.py", "def hello():\n    return 'hi'  # changed\n");
    git::commit_all(&r.repo, "greet");
    let ran = r.run(&["run", "unit", "--", "git", "--version"], &[]);
    assert!(!ran.stderr.contains("test file"), "{}", ran.stderr);
    r.write("app/greet.py", "def hello():\n    return 'hi'  # again\n");
    git::commit_all(&r.repo, "greet again");
    let ready = r.run(&["ready"], &[]);
    assert!(
        ready.stdout.contains("unit") && !ready.stdout.contains("test file"),
        "{}",
        ready.stdout
    );
}

// covers: C-22, C-34
#[test]
fn ready_asks_the_server_as_ci_counts_and_says_a_suspension() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    let server = with_server(&results);
    let python = python();
    let script = format!("hushrun run unit -- {python} -m pytest -q -p no:cacheprovider");
    let mut files = hidden("reuse");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"{script}\"}}}}\n"
        ),
    ));
    for n in 1..=3 {
        files.push((
            format!("test_more_{n}.py"),
            "import libb\n\ndef test_more():\n    assert libb.Y == 1\n".into(),
        ));
    }
    let r = repo_of(&files);
    assert_eq!(r.run(&pytest_args(&python, &[]), &envs(&server)).code, 0);
    r.write("liba.py", "X = 1  # changed\n");
    git::commit_all(&r.repo, "liba");
    let before = results.requests().len();
    let ready = r.run(&["ready"], &envs(&server));
    assert!(
        ready.stdout.contains(
            "unit: CI would run 2 of 5 test files; 3 already passed on the files they read"
        ),
        "{}\n{}",
        ready.stdout,
        ready.stderr
    );
    // \[C-38\] What other machines saw its test files read, then one batch lookup, and nothing
    // written to the server.
    let asked: Vec<String> = results.requests()[before..]
        .iter()
        .map(|s| s.path.clone())
        .filter(|p| p.starts_with("v0/units/"))
        .collect();
    assert_eq!(asked, ["v0/units/hints/lookup", "v0/units/lookup"]);
    // Suspended by an audit's disagreement: ready says so, as a run does. test_hidden.py passes
    // on what it reads now, then the flag it can't see makes it fail when it is the one audited.
    assert_eq!(r.run(&pytest_args(&python, &[]), &envs(&server)).code, 0);
    r.write("flag", "");
    r.write("libb.py", "Y = 1  # changed\n");
    git::commit_all(&r.repo, "libb");
    let caught = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        results.reuse_state(REPOSITORY, "unit")["state"],
        "suspended"
    );
    r.write("liba.py", "X = 1  # again\n");
    git::commit_all(&r.repo, "liba again");
    let ready = r.run(&["ready"], &envs(&server));
    assert!(
        ready
            .stdout
            .contains("unit: every test file runs: reuse of its test files is suspended"),
        "{}",
        ready.stdout
    );
}
