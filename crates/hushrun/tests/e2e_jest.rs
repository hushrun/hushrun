//! \[ADR 0085\] Jest's adapter end to end: the `hushrun` binary, as a person or CI runs it, for
//! what only the CLI does with Jest's test files and tests. Every other test of the adapter is
//! in-process, in `units_jest.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: the run's lines as the
//! CLI says them, its last line, the check's own result and skip, the audit's second start of
//! COMMAND and its suspension, and `hushrun why`. The tools are those of `units_jest.rs`.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/jest.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use cli::{Cli as _, Output};
use fixtures::*;
use repo::Repo;

const JEST_RUN: &[&str] = &[
    "run",
    "unit",
    "--",
    "node",
    "node_modules/jest/bin/jest.js",
    "--ci",
];

/// What a start of the binary wrote, its output and its lines.
fn out(output: &Output) -> String {
    format!("{}\n{}", output.stdout, output.stderr)
}

// covers: C-34, C-44
#[test]
fn a_run_says_what_it_reuses_and_hushrun_why_follows_what_jest_required() {
    for jest in jests() {
        let r = repo(&jest, &shop("reuse"));
        let first = r.run(JEST_RUN, &[]);
        assert_eq!(first.code, 0, "{}", out(&first));
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed Jest's test files yet"
        );
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let second = r.run(JEST_RUN, &[]);
        assert_eq!(second.code, 0, "{}", out(&second));
        assert_eq!(
            second.line("hushrun: unit: 3"),
            "hushrun: unit: 3 of 7 test files run; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "Jest {}",
            jest.major
        );
        assert!(
            second.stderr.contains("Test Suites: 3 passed, 3 total"),
            "Jest {}: {}",
            jest.major,
            second.stderr
        );
        // Its saving, or under a second, the count.
        assert!(
            hushrun_testkit::lines::says_reused(&second.stderr, 4, "test files"),
            "{}",
            second.stderr
        );
        // Why, with the imports the adapter saw.
        r.write("src/pricing.js", "exports.price = (x) => 2 * x;\n");
        let why = r.run(&["why", "unit", "test/cart.test.js"], &[]);
        assert_eq!(why.code, 0, "{}", why.stderr);
        assert!(
            why.stdout.starts_with(
                "test/cart.test.js runs: it read src/pricing.js, which changed\n  test/cart.test.js imports src/cart.js imports src/pricing.js\n  as far as this machine knows: Jest's list"
            ),
            "{}",
            why.stdout
        );
        // Every test file left after Jest's own choice already passed: the run passes, as the
        // run of those files did, where Jest would say it found no test.
        let only = r.run(
            &[
                "run",
                "unit",
                "--",
                "node",
                "node_modules/jest/bin/jest.js",
                "--ci",
                "greet",
            ],
            &[],
        );
        assert_eq!(only.code, 0, "{}", out(&only));
    }
}

// covers: C-39, C-44
#[test]
fn with_tests_as_the_unit_the_result_counts_each_test_and_the_same_code_skips() {
    for jest in jests() {
        let r = repo(&jest, &flips());
        let first = r.run(JEST_RUN, &[("FAIL", "1")]);
        assert_eq!(first.code, 1, "{}", out(&first));
        let map = r.map();
        assert_eq!(
            map["units"]["test/cart.test.js"]["tests"],
            json!({"cart > one": "passed", "cart > two": "passed", "cart > flips": "failed",
                   "cart > same": "passed", "cart > same [2]": "passed", "skipped": "not-run"}),
            "Jest {}: {map:#}",
            jest.major
        );
        // The flip fixed: each file's own key failed, so their tests are asked one by one; of the
        // six that passed, one runs as a check; the skipped test, which a run that failed recorded
        // no pass for (C-32), is asked again.
        let second = r.run(JEST_RUN, &[]);
        assert_eq!(second.code, 0, "{}", out(&second));
        assert_eq!(
            second.line("hushrun: unit: "),
            "hushrun: unit: 4 of 8 tests run, in 2 of 2 test files; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "Jest {}",
            jest.major
        );
        // Jest left out what the plan listed, before any of their hooks, and says so, beside the
        // test the file skips itself: whichever of the five the audit picks (by its key and the
        // run's start), four are left out and the skipped test runs. Before a run that failed
        // stopped recording a skip as a pass, the skipped test was a sixth that could be picked,
        // and Jest then said 6.
        assert!(
            second.stderr.contains("5 skipped"),
            "Jest {}: {}",
            jest.major,
            second.stderr
        );
        assert_eq!(
            r.map()["units"]["test/cart.test.js"]["tests"]["skipped"],
            "passed",
            "Jest {}: the skipped test ran again",
            jest.major
        );
        let result = store_result(&r);
        assert_eq!(result["units"]["tests"]["reused"], 4, "{result:#}");
        // The same code again: the check skips whole.
        let again = r.run(JEST_RUN, &[]);
        assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
    }
}

// covers: C-37, C-39, C-44
#[test]
fn why_says_a_jest_test_kept_nothing_after_an_error_no_test_owns() {
    // A timer `leaks` left throws while `waits` runs: Jest 29 charges it to `waits`, Jest 30 to
    // `leaks`; either way no test of the file keeps a result, and `hushrun why` says what happened.
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file(
            "test/leak.test.js",
            "test('leaks', () => { setTimeout(() => { throw new Error('late timer'); }, 20); });\ntest('waits', async () => { await new Promise((r) => setTimeout(r, 150)); });\ntest('after', () => {});\n",
        ),
    ];
    for jest in jests() {
        let r = repo(&jest, &files);
        let first = r.run(JEST_RUN, &[]);
        assert_eq!(first.code, 1, "{}", out(&first));
        let why = r.run(&["why", "unit", "test/leak.test.js::after"], &[]);
        assert!(
            out(&why).contains(
                "Jest reported an error it couldn't charge to a test (late timer), so no test of its file keeps a result of that run"
            ),
            "Jest {}: {}",
            jest.major,
            out(&why)
        );
    }
}

/// The newest run result this cache holds.
fn store_result(r: &Repo) -> Value {
    let mut found = Vec::new();
    collect(&r.cache.join("results/results/v0"), &mut found);
    found.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
    let newest = found.last().expect("a run result");
    serde_json::from_slice(&std::fs::read(newest).unwrap()).unwrap()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

// covers: C-39, C-34, C-44
#[test]
fn an_audited_test_that_fails_again_alone_suspends_the_checks_reuse() {
    for jest in jests() {
        // test/two.test.js's `ok` is the one test that could be reused: it runs as a check, and
        // fails. Its second run is COMMAND again with a directory of its own, `also` listed:
        // what the adapter does there comes from that directory alone.
        let mut files = flips();
        files.retain(|(p, _)| p != "test/cart.test.js");
        let r = repo(&jest, &files);
        assert_eq!(r.run(JEST_RUN, &[("FAIL", "1")]).code, 1);
        let caught = r.run(JEST_RUN, &[("HIDDEN", "1")]);
        assert_eq!(caught.code, 1, "{}", out(&caught));
        assert_eq!(
            caught.line("hushrun: unit: all"),
            "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs anyway, as a check"
        );
        assert!(
            caught.stderr.contains(
                "hushrun: unit: test/two.test.js::ok failed, though it passed before on the same files it read: running it once more"
            ),
            "{}",
            caught.stderr
        );
        // The second run ran `ok` alone: Jest left `also` out.
        assert!(
            caught
                .stderr
                .contains("Tests:       1 failed, 1 skipped, 2 total"),
            "Jest {}: {}",
            jest.major,
            caught.stderr
        );
        assert!(
            caught.stderr.contains(
                "hushrun: unit: test/two.test.js::ok failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
            ),
            "{}",
            caught.stderr
        );
        assert_eq!(r.disagreements(), ["test/two.test.js::ok"]);
    }
}

// covers: C-34, C-44
#[test]
fn an_injected_missed_dependency_in_jest_is_caught_by_an_audit() {
    for jest in jests() {
        // A test file that fails when a file outside the check's files exists, named by a
        // variable: what no adapter sees. With one other test file, it is the one an audit runs.
        let files = vec![
            file("hushrun.yaml", &yaml("reuse")),
            file(".gitignore", "node_modules\n"),
            file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
            file("jest.config.js", CONFIG),
            file("src/other.js", "exports.other = 1;\n"),
            file(
                "test/hidden.test.js",
                "const fs = require('fs');\ntest('hidden', () => {\n  let there = true;\n  try { fs.lstatSync(process.env.FLAG_FILE || ''); } catch { there = false; }\n  expect(there).toBe(false);\n});\n",
            ),
            file(
                "test/other.test.js",
                "const { other } = require('../src/other');\ntest('other', () => { expect(other).toBe(1); });\n",
            ),
        ];
        let r = repo(&jest, &files);
        let flag = r.root.join("flag");
        let flag = flag.to_str().unwrap().to_string();
        let env = [("FLAG_FILE", flag.as_str())];
        assert_eq!(r.run(JEST_RUN, &env).code, 0);
        std::fs::write(&flag, "").unwrap();
        r.write("src/other.js", "exports.other = 2 - 1;\n");
        let caught = r.run(JEST_RUN, &env);
        assert_eq!(caught.code, 1, "{}", out(&caught));
        assert!(
            caught.stderr.contains("unit: test/hidden.test.js failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"),
            "{}",
            caught.stderr
        );
    }
}

/// What `hushrun why` says, stdout and stderr, of `unit` after the runs so far.
fn why_of(r: &Repo, unit: &str) -> String {
    out(&r.run(&["why", "unit", unit], &[]))
}

// covers: C-37, C-44
#[test]
fn why_says_what_an_error_no_jest_test_owns_left_of_its_file_and_of_another() {
    // A timer `leaks` left throws while `waits` runs: Jest 29 charges it to `waits`, Jest 30 to
    // `leaks`. leak.test.js failed, and no test of it keeps a result; ok.test.js passed, and
    // keeps its pass: Jest tells failures by test file.
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file("jest.config.js", CONFIG),
        file(
            "test/leak.test.js",
            "test('leaks', () => { setTimeout(() => { throw new Error('late timer'); }, 20); });\ntest('waits', async () => { await new Promise((r) => setTimeout(r, 150)); });\ntest('after', () => {});\n",
        ),
        file("test/ok.test.js", "test('ok', () => {});\n"),
    ];
    let said = "Jest reported an error it couldn't charge to a test (late timer), so no test of its file keeps a result of that run";
    for jest in jests() {
        let r = repo(&jest, &files);
        let first = r.run(JEST_RUN, &[]);
        assert_eq!(first.code, 1, "{}", out(&first));
        let after = why_of(&r, "test/leak.test.js::after");
        assert!(after.contains(said), "Jest {}: {after}", jest.major);
        let file = why_of(&r, "test/leak.test.js");
        assert!(
            file.contains(
                "test/leak.test.js runs: its newest result on the files it reads now failed"
            ),
            "Jest {}: {file}",
            jest.major
        );
        for unit in ["test/ok.test.js", "test/ok.test.js::ok"] {
            let why = why_of(&r, unit);
            assert!(
                why.contains("would be reused") && !why.contains("couldn't charge"),
                "Jest {}: {unit}: {why}",
                jest.major
            );
        }
    }
}

// covers: C-37, C-44
#[cfg(unix)]
#[test]
fn why_says_a_second_start_of_jest_kept_every_pass_of_its_run_out() {
    use std::os::unix::fs::PermissionsExt as _;
    let said = "Jest started more than once in its command, and hushrun observes only the first start: what another start ran can't be told, so no pass of that run is kept";
    for jest in jests() {
        let r = repo(
            &jest,
            &[
                file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
                file(".gitignore", "node_modules\n"),
                file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
                file("jest.config.js", CONFIG),
                file("test/a.test.js", "test('a1', () => {});\n"),
                file(
                    "test/f.test.js",
                    "test('f1', () => { expect(process.env.FAILNOW).toBeUndefined(); });\n",
                ),
            ],
        );
        // As a package script does under `pnpm jest`: a stand-in `pnpm` that starts it twice.
        let bin = r.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("pnpm"),
            "#!/bin/sh\nnode node_modules/jest/bin/jest.js --ci && FAILNOW=1 node node_modules/jest/bin/jest.js --ci\n",
        )
        .unwrap();
        std::fs::set_permissions(bin.join("pnpm"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let ran = r.run(&["run", "unit", "--", "pnpm", "jest"], &[("PATH", &path)]);
        assert_eq!(ran.code, 1, "Jest {}: {}", jest.major, out(&ran));
        for unit in [
            "test/f.test.js::f1",
            "test/f.test.js",
            "test/a.test.js::a1",
            "test/a.test.js",
        ] {
            let why = why_of(&r, unit);
            assert!(why.contains(said), "Jest {}: {unit}: {why}", jest.major);
        }
    }
}
