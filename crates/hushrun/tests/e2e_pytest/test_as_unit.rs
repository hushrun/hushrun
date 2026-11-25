//! The end-to-end tests of `test_as_unit.rs`: a test as the unit (C-5, C-39) where only the CLI
//! shows it, `hushrun why`, `hushrun ready`, a results server, the run's own lines and last line,
//! the audit's second start and its suspension, and the check's skip. The fixtures are
//! `common/test_as_unit.rs`'s.

#[path = "../common/test_as_unit.rs"]
mod fixtures;

use hushrun_testkit::fake_results::FakeResults;
use hushrun_testkit::git;
use serde_json::{Value, json};

use super::cli::Cli as _;
use super::python::*;
use super::repo::Repo;
use super::store;
use fixtures::*;

/// A repository of `files`, committed, whose COMMAND inherits no pytest options.
fn repo_of(files: &[(String, String)]) -> Repo {
    Repo::new(files).clear(PYTEST_CLEARED)
}

/// [`repo_of`], of borrowed files.
fn repo_new(files: &[(&str, &str)]) -> Repo {
    repo_of(&fixtures::files(files))
}

/// `hushrun run unit -- python -m pytest -q -p no:cacheprovider EXTRA`.
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

/// How many unit results this machine holds.
fn unit_results(r: &Repo) -> usize {
    r.unit_results().values().sum()
}

// covers: C-32, C-34
#[test]
fn an_adapter_that_left_out_what_the_plan_didnt_list_runs_again_with_nothing_left_out() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_new(&[
        ("hushrun.yaml", &yaml("reuse")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", FAKE_RUNNER),
        ("other.py", "X = 1\n"),
        ("test_a.py", "def test_a():\n    pass\n"),
        ("test_b.py", "def test_b():\n    pass\n"),
    ]);
    let state = r.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let python = python();
    let state_dir = state.to_string_lossy().into_owned();
    let run = |extra: &[(&str, &str)]| {
        let mut env = vec![
            ("FAKE_STATE", state_dir.as_str()),
            ("FAKE_FILES", "test_a.py test_b.py"),
        ];
        env.extend_from_slice(extra);
        r.run(&["run", "unit", "--", &python, "fake.py"], &env)
    };
    let seen = |n: usize| -> Value {
        serde_json::from_slice(&std::fs::read(state.join(n.to_string())).unwrap()).unwrap()
    };
    // Start 1: both test files pass, and are recorded.
    let first = run(&[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(unit_results(&r), 2, "{}", first.stderr);
    // Start 2 is planned with one of them listed, fails, and says it left out what the plan
    // didn't list: start 3 runs with nothing left out, passes, and alone decides the run.
    r.write("other.py", "X = 2\n");
    let second = run(&[("FAKE_LEFT_OUT", "2"), ("FAKE_EXIT_2", "1")]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(
        second.stderr.contains(
            "hushrun: unit: running it once more with nothing left out: its adapter left out what the plan didn't list (2 features of test_a.py nobody listed)"
        ),
        "{}",
        second.stderr
    );
    assert_eq!(seen(2)["reuse"].as_array().unwrap().len(), 1, "{}", seen(2));
    assert_eq!(seen(3)["reuse"], json!([]), "{}", seen(3));
    assert!(seen(3).get("reuseTests").is_none(), "{}", seen(3));
    let result = r.newest_result();
    assert_eq!(result["result"], "pass", "{result:#}");
    assert_eq!(
        (
            result["units"]["total"].clone(),
            result["units"]["reused"].clone()
        ),
        (json!(2), json!(0))
    );
    // Start 5 says it again: nothing is recorded, and hushrun fails, saying why.
    r.write("other.py", "X = 3\n");
    let before = unit_results(&r);
    let results = || {
        let mut files = Vec::new();
        super::repo::collect(&r.cache.join("results/results/v0"), &mut files);
        files.len()
    };
    let recorded = results();
    let third = run(&[("FAKE_LEFT_OUT", "4 5")]);
    assert_eq!(third.code, 1, "{}", third.stderr);
    assert!(
        third.stderr.contains(
            "hushrun: unit wasn't recorded: run again with nothing left out, its adapter still left out what the plan didn't list (2 features of test_a.py nobody listed)"
        ),
        "{}",
        third.stderr
    );
    assert_eq!(results(), recorded);
    assert_eq!(unit_results(&r), before);
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

// covers: C-39, C-12
#[test]
fn an_older_server_keeps_the_first_5000_results_failures_first_and_a_saving_without_a_reused_file()
{
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    results.take_older_limits();
    let server = with_server(&results);
    let r = repo_of(&many("{mode: reuse, unit: test}"));
    let python = python();
    let mut env = envs(&server);
    env.push(("FAIL", "1"));
    // test_many.py failed: its own fail and each of its 6,000 tests, and test_other.py's pass:
    // 6,002 results, past the 5,000 the server takes. They go again, the first 5,000, the two
    // failures first, and the run says once what wasn't kept.
    let first = r.run(&pytest_args(&python, &[]), &env);
    assert_eq!(first.code, 1, "{}", first.stderr);
    assert!(
        first.stderr.contains(
            "hushrun: unit: 1,002 of its 6,002 results of test files and tests weren't kept: acme keeps 5,000 a run"
        ),
        "{}",
        first.stderr
    );
    assert_eq!(results.unit_results(), 5_000);
    assert_eq!(results.unit_results_that("fail"), 2);
    assert_eq!(results.recorded().len(), 1);
    // The flip fixed, and test_other.py changed, so that it runs: test_many.py's other tests,
    // whose passes the server kept, are reused one by one and no test file whole, whatever the
    // audit draws. The server refuses `savedMs` then, and the result goes again without it: it is
    // recorded, with its tests' counts.
    r.write(
        "tests/test_other.py",
        "def test_other():\n    pass\n# changed\n",
    );
    let second = r.run(&pytest_args(&python, &[]), &envs(&server));
    assert_eq!(second.code, 0, "{}", second.stderr);
    let recorded = results.recorded();
    let newest = recorded
        .iter()
        .find(|r| r["result"] == "pass")
        .expect("the pass is recorded");
    assert!(newest["units"].get("savedMs").is_none(), "{newest:#}");
    assert!(
        newest["units"]["tests"]["reused"].as_u64() > Some(0),
        "{newest:#}"
    );
    assert!(!second.stderr.contains("not recorded"), "{}", second.stderr);
}

// covers: C-12
#[test]
fn a_body_too_large_for_an_older_server_goes_again_as_the_first_5000() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let results = FakeResults::start();
    // A server from before tests, whose limit on a body (1 MiB) came first.
    results.answer(
        "POST",
        "v0/runs/",
        413,
        json!("Failed to buffer the request body: length limit exceeded"),
        1,
    );
    let server = with_server(&results);
    let r = repo_of(&many("{mode: reuse, unit: test}"));
    let mut env = envs(&server);
    env.push(("FAIL", "1"));
    let first = r.run(&pytest_args(&python(), &[]), &env);
    assert_eq!(first.code, 1, "{}", first.stderr);
    assert_eq!(results.unit_results(), 5_000);
    assert_eq!(results.unit_results_that("fail"), 2);
    assert!(
        first.stderr.contains("1,002 of its 6,002 results"),
        "{}",
        first.stderr
    );
}

// covers: C-12
#[test]
fn a_413_in_any_words_goes_again_as_the_first_5000_and_a_second_is_a_refusal() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // A proxy in front of the server with a smaller body limit (nginx's is 1 MiB) answers 413 in
    // its own words. hushrun's body fits 8 MiB, so whoever answered takes less: the first 5,000
    // go again, once.
    let proxy = json!("<html><head><title>413 Request Entity Too Large</title></head></html>");
    for (times, kept) in [(1, 5_000), (2, 0)] {
        let results = FakeResults::start();
        results.answer("POST", "v0/runs/", 413, proxy.clone(), times);
        let server = with_server(&results);
        let r = repo_of(&many("{mode: reuse, unit: test}"));
        let mut env = envs(&server);
        env.push(("FAIL", "1"));
        let first = r.run(&pytest_args(&python(), &[]), &env);
        assert_eq!(first.code, 1, "{}", first.stderr);
        assert_eq!(results.unit_results(), kept, "{}", first.stderr);
        assert_eq!(
            first.stderr.contains("1,002 of its 6,002 results"),
            kept > 0,
            "{}",
            first.stderr
        );
    }
}

// covers: C-32, C-37
#[test]
fn each_rule_that_keeps_a_result_says_why_through_hushrun_why() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let ok = |id: &str| json!({"id": id, "finished": true});
    let failed = json!({"id": "t3", "finished": true, "failed": true});
    // Each case: its facts, its variables, and what `hushrun why` says of t1 and of test_a.py
    // after one run (`None`: another reason, the test file's failure or reuse, comes first).
    type Case = (
        &'static str,
        String,
        Vec<(&'static str, &'static str)>,
        Option<&'static str>,
        Option<&'static str>,
    );
    let cases: Vec<Case> = vec![
        (
            "left out by something else",
            rules_units(
                json!([{"id": "t1", "deselected": true}, ok("t2"), failed.clone()]),
                json!({"failed": true}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("something other than hushrun's plan left it out"),
            None,
        ),
        (
            "a test file left out by something else",
            rules_units(
                json!([]),
                json!({"deselected": true, "selected": 0, "finished": 0}),
            ),
            vec![],
            None,
            Some("something other than hushrun's plan left it out"),
        ),
        (
            "unfinished",
            rules_units(
                json!([{"id": "t1"}, ok("t2"), failed.clone()]),
                json!({"failed": true, "finished": 2}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("it didn't finish in every process that ran it"),
            None,
        ),
        (
            "unfinished, in the adapter's words",
            rules_units(
                json!([{"id": "t1", "notKept": "its process was killed by a signal"}, ok("t2"), failed.clone()]),
                json!({"failed": true, "finished": 2}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("its process was killed by a signal"),
            None,
        ),
        (
            "a test file that didn't run whole",
            rules_units(json!([ok("t1"), {"id": "t2"}]), json!({"finished": 1})),
            vec![],
            None,
            Some("its test t2 kept no result: it didn't finish in every process that ran it"),
        ),
        (
            "a start that died",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![("FAKE_STARTED", "4242")],
            Some(
                "a start of its runner ended before it said what it saw, so no pass of that run is kept",
            ),
            Some(
                "a start of its runner ended before it said what it saw, so no pass of that run is kept",
            ),
        ),
        (
            "a second start, in its adapter's words",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![(
                "FAKE_STARTED",
                r#"{"why": "Jest started more than once in its command"}"#,
            )],
            Some("Jest started more than once in its command, so no pass of that run is kept"),
            Some("Jest started more than once in its command, so no pass of that run is kept"),
        ),
        (
            "a skip in a failed run",
            rules_units(
                json!([{"id": "t1", "finished": true, "skipped": true}, ok("t2")]),
                json!({}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("it was skipped in a run that failed"),
            Some("its test t1 kept no result: it was skipped in a run that failed"),
        ),
        (
            "a failure no test explains",
            rules_units(json!([ok("t1"), ok("t2")]), json!({"failed": true})),
            vec![("FAKE_EXIT", "1")],
            Some("its test file failed while none of its tests did"),
            None,
        ),
        (
            "a stale build in a run that passed",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![("FAKE_STALE", "build/prog is older than its sources")],
            Some("it ran on a build older than these files"),
            Some("it ran on a build older than these files"),
        ),
        (
            "a failed run whose failures aren't by test",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![("FAKE_EXIT", "1"), ("FAKE_BY_UNIT", "0")],
            Some("its run failed, and its adapter doesn't report each failure by test,"),
            Some("its run failed, and its adapter doesn't report each failure by test file"),
        ),
        (
            "a failed run, in its adapter's words",
            rules_units(json!([ok("t1"), ok("t2")]), json!({})),
            vec![
                ("FAKE_EXIT", "1"),
                ("FAKE_BY_UNIT", "0"),
                ("FAKE_NOT_KEPT", "an error no test owns stopped the run"),
            ],
            Some("an error no test owns stopped the run"),
            Some("an error no test owns stopped the run"),
        ),
        (
            "a test its adapter never leaves out",
            rules_units(
                json!([{"id": "t1", "finished": true, "neverLeftOut": true}, ok("t2"), failed.clone()]),
                json!({"failed": true}),
            ),
            vec![("FAKE_EXIT", "1")],
            Some("its adapter can't leave it out of a run"),
            None,
        ),
    ];
    let python = python();
    for (name, units, env, test, file) in cases {
        let r = repo_new(&[
            ("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
            (".gitignore", "__pycache__/\n"),
            ("fake.py", RULES_RUNNER),
            ("test_a.py", "def t1():\n    pass\n"),
            ("test_b.py", "def tb():\n    pass\n"),
        ]);
        let mut vars = vec![("FAKE_UNITS", units.as_str())];
        vars.extend(env.iter().copied());
        let ran = r.run(&["run", "unit", "--", &python, "fake.py"], &vars);
        assert!(
            ran.stderr.contains("hushrun: unit"),
            "{name}: {}",
            ran.stderr
        );
        for (asked, want) in [("test_a.py::t1", test), ("test_a.py", file)] {
            let Some(want) = want else { continue };
            let why = why_json(&r, asked);
            assert_eq!(
                why["reason"], "not-kept",
                "{name}, {asked}: {why:#}\n{}",
                ran.stderr
            );
            let says = why["says"].as_str().unwrap_or_default();
            assert!(
                says.starts_with(&format!("{asked} runs: ")) && says.contains(want),
                "{name}, {asked}: {says}"
            );
        }
    }
}

// covers: C-32, C-37
#[test]
fn a_run_left_out_twice_says_why_through_hushrun_why() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // Four test files pass; then test_b.py changes, and the plan lists the others but the one
    // an audit runs. Every start of the runner says it left out what the plan didn't list, the
    // start with nothing listed too: nothing of the run is kept, and `hushrun why` says so.
    let python = python();
    let names = ["test_a.py", "test_b.py", "test_c.py", "test_d.py"];
    let mut files = vec![
        (
            "hushrun.yaml".to_string(),
            yaml("{mode: reuse, unit: test}"),
        ),
        (".gitignore".to_string(), "__pycache__/\n".to_string()),
        ("fake.py".to_string(), RULES_RUNNER.to_string()),
    ];
    for name in names {
        files.push((name.to_string(), "def t1():\n    pass\n".to_string()));
    }
    let r = repo_of(&files);
    let units = json!(
        names
            .iter()
            .map(
                |u| json!({"unit": u, "selected": 1, "finished": 1, "reads": [u],
                        "tests": [{"id": "t1", "finished": true}]})
            )
            .collect::<Vec<_>>()
    )
    .to_string();
    let first = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[("FAKE_UNITS", &units)],
    );
    assert_eq!(first.code, 0, "{}", first.stderr);
    r.write("test_b.py", "def t1():\n    pass\n# changed\n");
    let again = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[
            ("FAKE_UNITS", &units),
            ("FAKE_LEFT_OUT", "a feature nobody listed"),
        ],
    );
    assert!(again.stderr.contains("wasn't recorded"), "{}", again.stderr);
    for asked in ["test_b.py", "test_b.py::t1"] {
        let why = why_json(&r, asked);
        assert_eq!(why["reason"], "not-kept", "{why:#}\n{}", again.stderr);
        assert!(
            why["says"]
                .as_str()
                .unwrap()
                .contains("even when it ran again with nothing listed"),
            "{why:#}"
        );
    }
}

// covers: C-22, C-33, C-34
#[test]
fn when_every_test_file_runs_the_plan_ready_and_why_give_the_same_true_reason() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let python = python();
    let unit = |name: &str, reads: &[&str], extra: Value| {
        let mut u = json!({"unit": name, "selected": 1, "finished": 1, "reads": reads,
                           "tests": [{"id": "t1", "finished": true}]});
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            u[k] = v;
        }
        u
    };
    let not_followed = json!({"notFollowed": [{"why": "subprocess", "detail": "node"}]});
    // Each case: the first run's facts and variables, what changes, and the reason every test file
    // runs on the second.
    type Case = (
        &'static str,
        Value,
        Vec<(&'static str, &'static str)>,
        (&'static str, &'static str),
        &'static str,
        &'static str,
    );
    let cases: Vec<Case> = vec![
        (
            "nothing was followed",
            json!([
                unit("test_a.py", &["test_a.py"], not_followed.clone()),
                unit("test_b.py", &["test_b.py"], not_followed.clone())
            ]),
            vec![],
            ("notes/n.md", "two\n"),
            "hushrun couldn't follow all that any of them read the last time (a process it started), so they always run",
            "not-observed",
        ),
        (
            "every test file reads every file",
            json!([
                unit("test_a.py", &["test_a.py", "test_b.py"], json!({})),
                unit("test_b.py", &["test_a.py", "test_b.py"], json!({}))
            ]),
            vec![],
            ("test_a.py", "def t1():\n    pass\n# changed\n"),
            "test_a.py changed, and every test file reads it",
            "read-changed",
        ),
        (
            "a file every test file reads changed",
            json!([
                unit("test_a.py", &["test_a.py", "app/core.py"], json!({})),
                unit("test_b.py", &["test_b.py", "app/core.py"], json!({}))
            ]),
            vec![],
            ("app/core.py", "RATE = 2\n"),
            "app/core.py changed, and every test file reads it",
            "read-changed",
        ),
        (
            "no pass was kept",
            json!([
                unit("test_a.py", &["test_a.py"], json!({})),
                unit("test_b.py", &["test_b.py"], json!({}))
            ]),
            vec![("FAKE_EXIT", "1"), ("FAKE_BY_UNIT", "0")],
            ("notes/n.md", "two\n"),
            "none kept a pass the last time they ran here: its run failed, and its adapter doesn't report each failure by test file",
            "not-kept",
        ),
    ];
    for (name, units, env, (path, text), want, reason) in cases {
        let r = repo_of(&[
            (
                "hushrun.yaml".into(),
                yaml("{mode: reuse, unit: test, ignore: ['notes/**']}"),
            ),
            (".gitignore".into(), "__pycache__/\n".into()),
            ("fake.py".into(), RULES_RUNNER.into()),
            ("app/__init__.py".into(), String::new()),
            ("app/core.py".into(), "RATE = 1\n".into()),
            ("notes/n.md".into(), "one\n".into()),
            ("test_a.py".into(), "def t1():\n    pass\n".into()),
            ("test_b.py".into(), "def t1():\n    pass\n".into()),
            (
                "package.json".into(),
                format!(
                    "{{\"name\": \"s\", \"private\": true, \"scripts\": {{\"test\": \"hushrun run unit -- {python} fake.py\"}}}}\n"
                ),
            ),
        ]);
        let units = units.to_string();
        let mut vars = vec![("FAKE_UNITS", units.as_str())];
        vars.extend(env.iter().copied());
        let first = r.run(&["run", "unit", "--", &python, "fake.py"], &vars);
        assert!(
            first.stderr.contains("hushrun: unit"),
            "{name}: {}",
            first.stderr
        );
        r.write(path, text);
        git::commit_all(&r.repo, "change");
        let ready = r.run(&["ready"], &[]);
        let said = ready
            .stdout
            .lines()
            .find(|l| l.contains("a run here would"))
            .unwrap_or_else(|| panic!("{name}: {}\n{}", ready.stdout, ready.stderr))
            .trim()
            .to_string();
        assert!(
            said.ends_with(&format!("a run here would run all 2 test files: {want}"))
                || said.contains(&format!("a run here would run all 2 test files: {want}")),
            "{name}: {said}"
        );
        // `hushrun why` of a test file gives it its own way, for the same cause.
        let why = why_json(&r, "test_b.py");
        assert_eq!(why["reason"], reason, "{name}: {why:#}");
        let second = r.run(
            &["run", "unit", "--", &python, "fake.py"],
            &[("FAKE_UNITS", units.as_str())],
        );
        assert!(
            second
                .stderr
                .contains(&format!("hushrun: unit: all 2 test files run: {want}")),
            "{name}: {}",
            second.stderr
        );
    }
}

// covers: C-33, C-39
#[test]
fn what_reuse_saved_is_never_more_than_the_check_took_running_everything() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // Each test file says it took 15s, as workers in parallel each count their own time; the
    // whole check takes about 1.2s. Reusing two of them saved about what the check took, not 30s.
    let python = python();
    let names = ["test_a.py", "test_b.py", "test_c.py", "test_d.py"];
    let mut files = vec![
        (
            "hushrun.yaml".to_string(),
            yaml("{mode: reuse, unit: test}"),
        ),
        (".gitignore".to_string(), "__pycache__/\n".to_string()),
        ("fake.py".to_string(), RULES_RUNNER.to_string()),
    ];
    for name in names {
        files.push((name.to_string(), "def t1():\n    pass\n".to_string()));
    }
    let r = repo_of(&files);
    let units = json!(names
        .iter()
        .map(|u| json!({"unit": u, "selected": 1, "finished": 1, "reads": [u], "durationMs": 15000,
                        "tests": [{"id": "t1", "finished": true, "durationMs": 15000}]}))
        .collect::<Vec<_>>())
    .to_string();
    let env = [("FAKE_UNITS", units.as_str()), ("FAKE_SLEEP", "1.2")];
    let first = r.run(&["run", "unit", "--", &python, "fake.py"], &env);
    assert_eq!(first.code, 0, "{}", first.stderr);
    r.write("test_b.py", "def t1():\n    pass\n# changed\n");
    let second = r.run(&["run", "unit", "--", &python, "fake.py"], &env);
    assert_eq!(second.code, 0, "{}", second.stderr);
    let line = second
        .stderr
        .lines()
        .find(|l| l.contains("unit passed in"))
        .unwrap_or_else(|| panic!("{}", second.stderr));
    assert!(
        hushrun_testkit::lines::says_reused(line, 2, "tests"),
        "{line}"
    );
    let saved = line
        .split("saved about ")
        .nth(1)
        .and_then(|rest| rest.split('s').next())
        .and_then(|n| n.parse::<f64>().ok());
    assert!(saved.is_some_and(|s| s < 5.0), "{line}");
}

/// `hushrun why unit <asked> --format json`, read.
fn why_json(r: &Repo, asked: &str) -> Value {
    let ran = r.run(&["why", "unit", asked, "--format", "json"], &[]);
    assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    serde_json::from_str(&ran.stdout).unwrap_or_else(|_| panic!("{}", ran.stdout))
}

// covers: C-37, C-39
#[test]
fn why_explains_one_test_and_a_test_file_with_a_test_not_followed() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let child = "import os, subprocess, sys\nfrom app.cart import total\n\n\
                 def test_spawns():\n    if os.environ.get('SPAWN'):\n        subprocess.run([sys.executable, '-c', 'pass'])\n\n\
                 def test_a():\n    assert total([1]) == 1\n";
    let files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("app/__init__.py".into(), String::new()),
        (
            "app/cart.py".into(),
            "def total(xs):\n    return sum(xs)\n".into(),
        ),
        (
            "tests/test_cart.py".into(),
            "import os\nfrom app.cart import total\n\n\
             def test_one():\n    assert total([1]) == 1\n\n\
             def test_flips():\n    assert not os.environ.get('FAIL')\n"
                .into(),
        ),
        ("tests/test_child.py".into(), child.into()),
    ];
    let r = repo_of(&files);
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);

    // test_cart.py failed: each of its tests is explained on its own.
    let one = why_json(&r, "tests/test_cart.py::test_one");
    assert_eq!(
        (&one["unit"], &one["test"], &one["decision"], &one["reason"]),
        (
            &json!("tests/test_cart.py"),
            &json!("test_one"),
            &json!("reused"),
            &json!("reused")
        ),
        "{one:#}"
    );
    assert_eq!(
        one["says"],
        "tests/test_cart.py::test_one would be reused: it passed on the files it read, and none of them has changed since"
    );
    let flips = why_json(&r, "tests/test_cart.py::test_flips");
    assert_eq!(
        flips["says"],
        "tests/test_cart.py::test_flips runs: its newest result on the files it reads now failed"
    );
    let nope = why_json(&r, "tests/test_cart.py::test_nope");
    assert_eq!(nope["reason"], "never-ran-here", "{nope:#}");
    // test_child.py passed whole: its tests go with it; \[C-37\] a test it doesn't have doesn't.
    let none = why_json(&r, "tests/test_child.py::test_nope");
    assert_eq!(none["reason"], "never-ran-here", "{none:#}");
    let a = why_json(&r, "tests/test_child.py::test_a");
    assert_eq!(
        a["says"],
        "tests/test_child.py::test_a would be reused: its test file passed whole on the files it read, and none of them has changed since"
    );

    // test_spawns starts a process in a run where test_child.py changed; then test_child.py is
    // as it was, so its own key has its pass, but it isn't reused whole.
    r.write("tests/test_child.py", &format!("{child}# changed\n"));
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("SPAWN", "1")]).code, 0);
    r.write("tests/test_child.py", child);
    let whole = why_json(&r, "tests/test_child.py");
    assert_eq!(whole["decision"], "runs", "{whole:#}");
    assert_eq!(whole["reason"], "test-not-observed", "{whole:#}");
    assert!(
        whole["says"]
            .as_str()
            .unwrap()
            .contains("its test test_spawns read the last time it ran here"),
        "{whole:#}"
    );
    let spawns = why_json(&r, "tests/test_child.py::test_spawns");
    assert_eq!(spawns["reason"], "not-observed", "{spawns:#}");
    let a = why_json(&r, "tests/test_child.py::test_a");
    assert!(
        a["detail"].as_array().unwrap().iter().any(|l| l
            .as_str()
            .unwrap()
            .starts_with("its test file isn't reused whole")),
        "{a:#}"
    );
}

// covers: C-22, C-39
#[test]
fn ready_counts_tests_where_tests_are_the_unit() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let python = python();
    let mut files = shop("{mode: reuse, unit: test}");
    files.push((
        "package.json".into(),
        format!(
            "{{\"name\": \"shop\", \"private\": true, \"scripts\": {{\"test\": \"hushrun run unit -- {python} -m pytest -q -p no:cacheprovider\"}}}}\n"
        ),
    ));
    let r = repo_of(&files);
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);
    // cart.py changes, committed: what a run would do is said with tests, as its line says it.
    r.write(
        "app/cart.py",
        "def total(xs):\n    return sum(xs)  # changed\n",
    );
    git::commit_all(&r.repo, "change");
    let ready = r.run(&["ready"], &[]);
    let line = ready
        .stdout
        .lines()
        .find(|l| l.contains("a run here would"))
        .unwrap_or_else(|| panic!("{}\n{}", ready.stdout, ready.stderr))
        .trim()
        .to_string();
    assert!(
        line.ends_with(
            "a run here would run 11 of 13 tests, in 3 of 5 test files; 2 already passed here on the files they read"
        ),
        "{line}"
    );
}

// covers: C-39, C-10, C-36
#[test]
fn a_run_says_the_tests_it_reused_in_its_last_line_and_the_same_code_again_skips() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_as_unit.rs's first test, as the CLI says it: what the plan left out is said in the
    // run's last line, the check's result is a pass, and the same code again skips (C-10).
    let r = repo_of(&shop("{mode: reuse, unit: test}"));
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let second = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(r.newest_result()["result"], "pass");
    assert!(
        hushrun_testkit::lines::says_reused(second.line("hushrun: unit passed in"), 10, "tests"),
        "{}",
        second.stderr
    );
    // The same code again: the check skips whole (C-10).
    let again = r.run(&pytest_args(&python, &[]), &[]);
    assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
}

// covers: C-39, C-32, C-37
#[test]
fn hushrun_why_says_a_skip_in_a_run_that_failed_kept_nothing() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&skips());
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[("FAIL", "1"), ("SKIP", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // \[C-37\] `hushrun why` says why each kept nothing, as the run decided it: the test, and its
    // test file, which one of its tests' skips kept from a pass.
    let skipped = why_json(&r, "tests/test_skips.py::test_skipped");
    assert_eq!(skipped["reason"], "not-kept", "{skipped:#}");
    assert_eq!(
        skipped["says"],
        format!("tests/test_skips.py::test_skipped runs: {SKIPPED_IN_A_FAILED_RUN}"),
        "{skipped:#}"
    );
    let file = why_json(&r, "tests/test_skips.py");
    assert_eq!(file["reason"], "not-kept", "{file:#}");
    assert_eq!(
        file["says"],
        "tests/test_skips.py runs: its test test_in_setup kept no result: it was skipped in a run that failed, and a skip there proves nothing: what failed may have skipped what it hides",
        "{file:#}"
    );
    // A test file skipped whole as it loads, which no test of it says: the same reason, not that
    // it never ran here.
    let whole = why_json(&r, "tests/test_skipmod.py");
    assert_eq!(whole["reason"], "not-kept", "{whole:#}");
}

// covers: C-39, C-32, C-37
#[test]
fn hushrun_why_says_the_end_of_a_wider_fixture_kept_a_test_from_a_pass() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&leak());
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[("BUG", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // \[C-37\] `hushrun why` says the fixture's end kept it, not that it didn't finish.
    let why = why_json(&r, "tests/test_leak.py::test_opens");
    assert_eq!(why["reason"], "not-kept", "{why:#}");
    assert!(
        why["says"]
            .as_str()
            .unwrap()
            .contains("a fixture of a wider scope it was served (no_leaks) failed as it ended"),
        "{why:#}"
    );
}

// covers: C-32, C-37
#[test]
fn hushrun_why_says_a_session_fixture_asked_for_at_run_time_kept_a_test_file_from_a_pass() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&session_leak());
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[("BUG", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    // \[C-37\] Its test file keeps no pass for the same reason, said through the test.
    let file = why_json(&r, "tests/test_l.py");
    assert_eq!(file["reason"], "not-kept", "{file:#}");
    let says = file["says"].as_str().unwrap();
    assert!(
        says.starts_with("tests/test_l.py runs: its test test_leaky kept no result: a fixture of a wider scope it was served (db) failed as it ended"),
        "{file:#}"
    );
}

// covers: C-39, C-34
#[test]
fn an_audited_test_that_fails_again_alone_is_said_as_it_runs_again_and_as_reuse_is_suspended() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // test_ok passed on these files, and is the one test that could be reused: it runs as a
    // check, and fails. It runs once more, alone, and fails again: a disagreement, which the CLI
    // says at each step.
    let r = repo_of(&two_tests());
    let python = python();
    assert_eq!(r.run(&pytest_args(&python, &[]), &[("FAIL", "1")]).code, 1);
    let caught = r.run(&pytest_args(&python, &[]), &[("HIDDEN", "1")]);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert!(
        caught.stderr.contains(
            "hushrun: unit: tests/test_two.py::test_ok failed, though it passed before on the same files it read: running it once more"
        ),
        "{}",
        caught.stderr
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: tests/test_two.py::test_ok failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
}

// covers: C-34, C-39
#[test]
fn an_audit_of_two_percent_of_the_tests_says_the_rest_reused_in_its_last_line() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&big_and_small());
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    r.write(
        "tests/test_small.py",
        "def test_s():\n    pass\n# changed\n",
    );
    let second = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    // \[C-34\] What the 98 left out took when their file last ran is what reuse saved: the line
    // counts them as reused tests.
    assert!(
        hushrun_testkit::lines::says_reused(&second.stderr, 98, "tests"),
        "{}",
        second.stderr
    );
}

// covers: C-34, C-39
#[test]
fn an_audited_test_of_a_file_reused_whole_that_fails_again_is_said_to_suspend_reuse() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&target_and_changed());
    let python = python();
    let first = r.run(&pytest_args(&python, &[]), &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    r.write(
        "tests/test_changed.py",
        "def test_c():\n    pass\n# changed\n",
    );
    let second = r.run(&pytest_args(&python, &[]), &[("BROKEN", "1")]);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.stderr.contains(
            "hushrun: unit: tests/test_target.py failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        second.stderr
    );
}

// covers: C-32
#[test]
fn a_run_on_a_stale_build_records_no_pass_of_its_check() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // The adapter knows the runner ran a program built before the last change: the run passed,
    // and proves nothing of this code.
    let r = repo_new(&[
        ("hushrun.yaml", &yaml("observe")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", FAKE_RUNNER),
        ("test_a.py", "def test_a():\n    pass\n"),
    ]);
    let state = r.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let python = python();
    let state_dir = state.to_string_lossy().into_owned();
    let env = [
        ("FAKE_STATE", state_dir.as_str()),
        ("FAKE_FILES", "test_a.py"),
        (
            "FAKE_STALE",
            "build/prog is older than what it is built from",
        ),
    ];
    let ran = r.run(&["run", "unit", "--", &python, "fake.py"], &env);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains(
            "hushrun: unit passed, but it isn't recorded: build/prog is older than what it is built from"
        ),
        "{}",
        ran.stderr
    );
    assert!(!ran.stderr.contains("it won't run again"), "{}", ran.stderr);
    let mut files = Vec::new();
    super::repo::collect(&r.cache.join("results/results/v0"), &mut files);
    assert!(files.is_empty(), "{files:?}");
    assert_eq!(unit_results(&r), 0);
    // A failure is recorded as usual.
    let failed = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[env[0], env[1], env[2], ("FAKE_EXIT_2", "1")],
    );
    assert_eq!(failed.code, 1, "{}", failed.stderr);
    assert_eq!(r.newest_result()["result"], "fail");
    // \[C-32\] Its test file passed on that stale build in a run that failed: no pass of it is
    // kept.
    assert_eq!(unit_results(&r), 0);
}

// covers: C-37, C-39
#[test]
fn why_says_a_test_kept_nothing_as_its_test_file_failed_while_none_of_its_tests_did() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_new(&[
        ("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        (".gitignore", "__pycache__/\n"),
        ("fake.py", UNEXPLAINED_RUNNER),
        (
            "test_a.py",
            "def test_one():\n    pass\n\ndef test_two():\n    pass\n",
        ),
    ]);
    let python = python();
    let ran = r.run(&["run", "unit", "--", &python, "fake.py"], &[]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    let map = store::map(&r.cache);
    assert_eq!(
        map["units"]["test_a.py"]["tests"],
        json!({"test_one": "not-run", "test_two": "not-run"}),
        "{map:#}"
    );
    let one = why_json(&r, "test_a.py::test_one");
    assert_eq!(one["reason"], "not-kept", "{one:#}");
    assert_eq!(
        one["says"],
        "test_a.py::test_one runs: its test file failed while none of its tests did, and what failed may need this test to run, so it keeps no pass",
        "{one:#}"
    );
    // \[C-32\] On a stale build, test_one's pass in a run test_two failed proves nothing either.
    let ran = r.run(
        &["run", "unit", "--", &python, "fake.py"],
        &[
            ("FAKE_TWO_FAILS", "1"),
            ("FAKE_STALE", "build/prog is older"),
        ],
    );
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    let one = why_json(&r, "test_a.py::test_one");
    assert_eq!(
        one["says"],
        "test_a.py::test_one runs: it ran on a build older than these files, so its pass proves nothing of them",
        "{one:#}"
    );
}
