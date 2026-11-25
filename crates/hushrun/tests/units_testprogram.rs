//! \[ADR 0120\] C++ test programs COMMAND starts itself, by their path, not through CTest
//! (C-106 to C-108): a GoogleTest or Catch2 3 program of a CMake build directory is a unit, each of
//! its tests a test, what it read what its build directory says it is built from, and hushrun
//! leaves tests out through the framework's own variables, after listing again with them.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- build/t_calc` would, through the adapter's own type, `testprogram::Recognized`,
//! and the code every adapter shares, and never starts the `hushrun` binary
//! (`common/in_process.rs`). The test program runs as COMMAND, for real: no helper of hushrun's
//! stands between, since the frameworks take what to leave out from COMMAND's environment. What
//! only the CLI shows (its last line, and its line for a pass it doesn't record) is in
//! `e2e_testprogram.rs`. The fixtures are `common/testprogram.rs`'s.

#![cfg(unix)]

#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;
#[path = "common/testprogram.rs"]
mod testprogram;

use std::collections::BTreeSet;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, testprogram as adapter};

use in_process::{Ran, Repo};
use testprogram::*;

/// What the test-program adapter answers that never changes in a run, for the code every adapter
/// shares.
fn profile(adapter: &adapter::Recognized) -> Profile {
    Profile {
        word: adapter.word(),
        runner: adapter.runner(),
        test_file: adapter.test_file(),
        tests: adapter.tests(),
        hints: adapter.hints(),
        binds: adapter.binds(),
        keys: adapter.keys(),
        not_by_test: adapter.not_by_test(),
        not_watched: adapter.not_watched(),
    }
}

/// `hushrun run unit -- ARGS`, in-process, through the test-program adapter alone.
fn run(r: &Repo, args: &[&str]) -> Ran {
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to the adapter (`testprogram::recognized`), with no
    // trait object: what COMMAND's program is, and whether its framework can be observed.
    match adapter::recognize(hushrun::units::command::without_env(&command), &r.repo) {
        Some(Ok(adapter)) => observe(r, args, &[], &adapter),
        Some(Err(why)) => in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        None => panic!("{args:?} doesn't start a test program"),
    }
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order, on `testprogram::Recognized` itself (`units_gradle.rs`'s, with the type changed).
fn observe(r: &Repo, args: &[&str], env: &[(&str, &str)], adapter: &adapter::Recognized) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // The adapter's own `plan`, which walks the program's build before COMMAND.
            Ok(context) => {
                let planning = adapter.plan(&observer.plan_at(&input, &context));
                observer.planned(&input, context, planning)
            }
            Err(planned) => planned,
        };
        drop(input);
        run.planned(&observer, planned);
    }
    // COMMAND, once, or \[C-32\] once more with nothing left out when the adapter left out what
    // the plan didn't list.
    let mut starts = 0;
    let (code, mut observed) = loop {
        starts += 1;
        let start = adapter.start_variables(observer.run_dir());
        let (code, passed) = run.start_command(&observer, &start);
        let end = run.finish_input(passed);
        let observed = match observer.read_context(&end) {
            Ok(context) => {
                let read = adapter.read(&observer.read_at(&end, &context));
                observer.finished(&end, context, read)
            }
            Err(observed) => observed.map(|observed| *observed),
        };
        drop(end);
        if starts == 1 && observed.as_ref().is_some_and(|o| o.left_out.is_some()) {
            let extras = adapter.again(observer.root(), observer.cwd(), &BTreeSet::new());
            if observer.whole_again(extras).is_ok() {
                continue;
            }
        }
        break (code, observed);
    };
    // \[C-34\] An audited unit that failed runs once more, alone.
    if let Some(o) = observed.as_mut().filter(|o| !o.again.is_empty()) {
        let wanted: BTreeSet<String> = o.again.keys().cloned().collect();
        let extras = adapter.again(observer.root(), o.cwd(), o.unit_files());
        let ended = match observer.again(o, &wanted, extras) {
            Some(again) => {
                let start = adapter.start_variables(again.dir());
                let again = again.with_start(start);
                let first = adapter.start_variables(observer.run_dir());
                let passed = run.start_again(&observer, &first, &again);
                let files = again.files();
                let map = in_process::no_map();
                let read = adapter
                    .read(&observer.again_read_at(&files, o, &run.tree, passed, &map))
                    .ok()
                    .flatten();
                observer.again_ended(&again, o, passed, read)
            }
            None => in_process::Run::not_again(&wanted),
        };
        run.disagreements(o, ended);
    }
    run.record(code, observed)
}

/// Whether one of the run's lines says `text`.
fn says(ran: &Ran, text: &str) -> bool {
    ran.lines.iter().any(|l| l.contains(text))
}

/// How many tests the run reused one by one, as its result counts them.
fn tests_reused(ran: &Ran) -> u64 {
    ran.units["tests"]["reused"].as_u64().unwrap_or(0)
}

// covers: C-106, C-108
#[test]
fn googletests_tests_that_passed_on_their_programs_files_are_left_out_and_a_change_runs_them() {
    if !wanted() {
        return;
    }
    let p = project();
    let first = run(&p.r, &["build/t_calc"]);
    assert_eq!(first.code, 0, "{}", first.said());
    assert_eq!(gtest_ran(&first.stdout).len(), 5, "{}", first.stdout);
    // A file of another program only: t_calc's tests passed on what it is built from.
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n",
    );
    p.build();
    // Its first naming of t_other's sources runs everything once; after that, they're reused.
    let _ = run(&p.r, &["build/t_calc"]);
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n// two\n",
    );
    p.build();
    let second = run(&p.r, &["build/t_calc"]);
    assert_eq!(second.code, 0, "{}", second.said());
    assert!(
        says(&second, "already passed on the files they read"),
        "{}",
        second.said()
    );
    // What the run's last line counts as `tests reused` (e2e_testprogram.rs says it).
    assert!(tests_reused(&second) > 0, "{:#}", second.units);
    assert!(
        gtest_ran(&second.stdout).len() <= 1,
        "the audit runs one: {}",
        second.stdout
    );
    assert!(
        second.stdout.contains("Google Test filter = -"),
        "{}",
        second.stdout
    );
    // A change to what t_calc is built from runs every test of it, and the broken ones fail.
    p.write("src/calc.cpp", CALC_BROKEN);
    p.build();
    let broken = run(&p.r, &["build/t_calc"]);
    assert_ne!(broken.code, 0, "{}", broken.said());
    assert_eq!(gtest_ran(&broken.stdout).len(), 5, "{}", broken.stdout);
    assert!(
        broken.stdout.contains("[  FAILED  ] Mul.Small"),
        "{}",
        broken.stdout
    );
}

// covers: C-106, C-108
#[test]
fn a_program_older_than_what_it_is_built_from_records_no_pass() {
    if !wanted() {
        return;
    }
    let p = project();
    p.write("src/calc.cpp", CALC_BROKEN);
    let ran = run(&p.r, &["build/t_calc"]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    // What the CLI says after `unit passed, but it isn't recorded: ` (e2e_testprogram.rs).
    let why = ran.stale_build.as_deref().unwrap_or_default();
    assert!(why.contains("ran old code"), "{why:?}\n{}", ran.said());
    assert_eq!(ran.units, serde_json::Value::Null, "{}", ran.said());
    assert!(p.r.unit_results().is_empty(), "{:?}", p.r.unit_results());
}

// covers: C-106
#[test]
fn a_filter_of_commands_own_is_observed_and_leaves_nothing_out() {
    if !wanted() {
        return;
    }
    let p = project();
    let first = run(&p.r, &["build/t_calc", "--gtest_filter=Add.*"]);
    assert_eq!(first.code, 0, "{}", first.said());
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n",
    );
    p.build();
    let again = run(&p.r, &["build/t_calc", "--gtest_filter=Add.*"]);
    assert!(
        says(&again, "wins over the GTEST_FILTER"),
        "{}",
        again.said()
    );
    assert_eq!(
        gtest_ran(&again.stdout),
        ["Add.Small", "Add.Zero"],
        "{}",
        again.stdout
    );
}

// covers: C-107, C-108
#[test]
fn catch2s_tests_are_left_out_by_their_names_through_bazels_variables() {
    if !wanted() {
        return;
    }
    let p = project();
    let catch2 = ["build/c_calc", "--durations", "yes"];
    let first = run(&p.r, &catch2);
    assert_eq!(first.code, 0, "{}", first.said());
    assert_eq!(catch2_ran(&first.stdout).len(), 3, "{}", first.stdout);
    for text in ["// one\n", "// two\n"] {
        p.write(
            "tests/t_other.cpp",
            &format!("#include <gtest/gtest.h>\nTEST(Other, Works) {{ }}\n{text}"),
        );
        p.build();
        let _ = run(&p.r, &catch2);
    }
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n// three\n",
    );
    p.build();
    let reused = run(&p.r, &catch2);
    assert_eq!(reused.code, 0, "{}", reused.said());
    // What the run's last line counts as `tests reused` (e2e_testprogram.rs says it).
    assert!(tests_reused(&reused) > 0, "{:#}", reused.units);
    // Never every test: Catch2 fails when it runs none, so the audit's, or one, runs.
    assert!(!catch2_ran(&reused.stdout).is_empty(), "{}", reused.stdout);
    assert!(catch2_ran(&reused.stdout).len() < 3, "{}", reused.stdout);
    assert!(reused.stdout.contains("Filters:"), "{}", reused.stdout);
    p.write("src/calc.cpp", CALC_BROKEN);
    p.build();
    let broken = run(&p.r, &catch2);
    assert_ne!(broken.code, 0, "{}", broken.stdout);
    assert_eq!(catch2_ran(&broken.stdout).len(), 3, "{}", broken.stdout);
}

// covers: C-106
#[test]
fn a_doctest_program_is_said_not_to_be_observed() {
    if !wanted() {
        return;
    }
    let Some(p) = doctest_project() else {
        return;
    };
    let ran = run(&p.r, &["build/t_d"]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(says(&ran, "doctest takes what to run"), "{}", ran.said());
}
