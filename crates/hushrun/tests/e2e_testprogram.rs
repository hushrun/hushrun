//! \[ADR 0120\] C++ test programs COMMAND starts by their path, end to end: the `hushrun` binary,
//! as a person or CI runs it, for what only the CLI says of them. Every other test of the
//! adapter is in-process, in `units_testprogram.rs`, with the same fixtures
//! (`common/testprogram.rs`).
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: the run's last line
//! (`tests reused`), and the line for a pass it doesn't record.

#![cfg(unix)]

#[path = "common/cli.rs"]
mod cli;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;
#[path = "common/testprogram.rs"]
mod testprogram;

use cli::{Cli as _, Output};
use testprogram::*;

fn says(ran: &Output, text: &str) -> bool {
    ran.stderr.contains(text)
}

// covers: C-106, C-108
#[test]
fn a_googletest_run_that_leaves_tests_out_says_how_many_it_reused() {
    if !wanted() {
        return;
    }
    let p = project();
    let first = p.r.run(&["run", "unit", "--", "build/t_calc"], &[]);
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    // A file of another program only: t_calc's tests passed on what it is built from.
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n",
    );
    p.build();
    // Its first naming of t_other's sources runs everything once; after that, they're reused.
    let _ = p.r.run(&["run", "unit", "--", "build/t_calc"], &[]);
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n// two\n",
    );
    p.build();
    let second = p.r.run(&["run", "unit", "--", "build/t_calc"], &[]);
    assert_eq!(second.code, 0, "{}{}", second.stdout, second.stderr);
    assert!(
        says(&second, "already passed on the files they read"),
        "{}",
        second.stderr
    );
    assert!(says(&second, "tests reused"), "{}", second.stderr);
    assert!(
        gtest_ran(&second.stdout).len() <= 1,
        "the audit runs one: {}",
        second.stdout
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
    let ran = p.r.run(&["run", "unit", "--", "build/t_calc"], &[]);
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert!(says(&ran, "ran old code"), "{}", ran.stderr);
    assert!(says(&ran, "isn't recorded"), "{}", ran.stderr);
}

// covers: C-107, C-108
#[test]
fn a_catch2_run_that_leaves_tests_out_says_how_many_it_reused() {
    if !wanted() {
        return;
    }
    let p = project();
    let catch2 = ["run", "unit", "--", "build/c_calc", "--durations", "yes"];
    let first = p.r.run(&catch2, &[]);
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    for text in ["// one\n", "// two\n"] {
        p.write(
            "tests/t_other.cpp",
            &format!("#include <gtest/gtest.h>\nTEST(Other, Works) {{ }}\n{text}"),
        );
        p.build();
        let _ = p.r.run(&catch2, &[]);
    }
    p.write(
        "tests/t_other.cpp",
        "#include <gtest/gtest.h>\nTEST(Other, Works) { }\n// three\n",
    );
    p.build();
    let reused = p.r.run(&catch2, &[]);
    assert_eq!(reused.code, 0, "{}{}", reused.stdout, reused.stderr);
    assert!(says(&reused, "tests reused"), "{}", reused.stderr);
    assert!(catch2_ran(&reused.stdout).len() < 3, "{}", reused.stdout);
}
