//! \[ADR 0107\] The adapters for Node's test runner and Mocha end to end: the `hushrun` binary, as a
//! person or CI runs it, for what only the CLI does with their test files and tests. Every other
//! test of the two adapters is in-process, in `units_node.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: the run's lines as the
//! CLI says them, its last line, the check's skip, and `hushrun why`. The tools are those of
//! `units_node.rs`.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/node.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use serde_json::json;

use cli::{Cli as _, Output};
use fixtures::*;

/// What a start of the binary wrote, its output and its lines.
fn out(output: &Output) -> String {
    format!("{}\n{}", output.stdout, output.stderr)
}

// covers: C-34, C-54
#[test]
fn a_run_of_nodes_runner_says_what_it_reuses_and_hushrun_why_follows_its_imports() {
    for node in nodes() {
        let r = repo(&node, &node_shop("reuse"));
        let first = r.run(NODE_RUN, &[]);
        assert_eq!(first.code, 0, "{}", out(&first));
        assert_eq!(
            first.line("hushrun: unit: every"),
            "hushrun: unit: every test file runs: no run here has listed the test files of Node's test runner yet"
        );
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let second = r.run(NODE_RUN, &[]);
        assert_eq!(second.code, 0, "{}", out(&second));
        assert_eq!(
            second.line("hushrun: unit: 2"),
            "hushrun: unit: 2 of 6 test files run; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "{}",
            node.name
        );
        assert!(
            hushrun_testkit::lines::says_reused(&second.stderr, 4, "test files"),
            "{}",
            second.stderr
        );
        // A change that breaks the cart: its test file runs, and fails.
        r.write("src/pricing.js", "exports.price = (x) => x * 3;\n");
        let broken = r.run(NODE_RUN, &[]);
        assert_eq!(broken.code, 1, "{}", out(&broken));
        // A change to a data file it opened is caught too.
        r.write("src/pricing.js", "exports.price = (x) => x * 2;\n");
        assert_eq!(r.run(NODE_RUN, &[]).code, 0);
        r.write("fixtures/rate.js", "3\n");
        let data = r.run(NODE_RUN, &[]);
        assert_eq!(data.code, 1, "{}", out(&data));
        // Why, with the imports the adapter saw.
        r.write("fixtures/rate.js", "2\n");
        assert_eq!(r.run(NODE_RUN, &[]).code, 0);
        r.write("src/pricing.js", "exports.price = (x) => 2 * x;\n");
        let why = r.run(&["why", "unit", "test/cart.test.js"], &[]);
        assert_eq!(why.code, 0, "{}", why.stderr);
        assert!(
            why.stdout.starts_with(
                "test/cart.test.js runs: it read src/pricing.js, which changed\n  test/cart.test.js imports src/cart.js imports src/pricing.js"
            ),
            "{}",
            why.stdout
        );
    }
}

// covers: C-39, C-55
#[test]
fn with_tests_as_the_unit_nodes_runner_runs_what_failed_and_the_same_code_skips() {
    for node in nodes() {
        let r = repo(&node, &node_tests());
        let first = r.run(NODE_RUN, &[("FAIL", "1")]);
        assert_eq!(first.code, 1, "{}", out(&first));
        let cart = tests_of(&r, "test/cart.test.js");
        assert_eq!(cart["cart > one"], "passed", "{}: {cart:?}", node.name);
        assert_eq!(cart["cart > flips"], "failed", "{cart:?}");
        assert_eq!(cart["cart > same"], "passed", "{cart:?}");
        assert_eq!(cart["cart > same [2]"], "passed", "{cart:?}");
        assert_eq!(cart["parent > kid"], "passed", "{cart:?}");
        assert_eq!(tests_of(&r, "test/two.test.mjs")["also"], "failed");
        // The two that failed run; everything that passed is left out, one by one, through
        // `require('node:test')` and `import test from 'node:test'` alike; a test with subtests
        // runs its body, its subtest left out.
        let second = r.run(NODE_RUN, &[]);
        assert_eq!(second.code, 0, "{}", out(&second));
        let out = out(&second);
        assert!(
            out.contains("hushrun: passed before on the files it read"),
            "{}: {out}",
            node.name
        );
        for ran in ["flips", "also"] {
            assert!(
                out.lines()
                    .any(|l| l.contains(ran) && !l.contains("hushrun:")),
                "{ran} ran: {out}"
            );
        }
        assert_eq!(tests_of(&r, "test/cart.test.js")["cart > flips"], "passed");
        let again = r.run(NODE_RUN, &[]);
        assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
    }
}

// covers: C-34, C-39, C-56, C-57
#[test]
fn a_run_of_mocha_says_how_many_tests_it_reused() {
    for mocha in mochas() {
        let r = repo(&mocha, &mocha_shop("{mode: reuse, unit: test}"));
        let first = r.run(MOCHA_RUN, &[]);
        assert_eq!(first.code, 0, "{}", out(&first));
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let second = r.run(MOCHA_RUN, &[]);
        assert_eq!(second.code, 0, "{}", out(&second));
        // The cart's two tests, one more as a check, and the root hook file's own test, which
        // isn't a unit and always runs.
        assert!(
            out(&second).contains("4 passing"),
            "{}: {}",
            mocha.name,
            out(&second)
        );
        assert!(
            hushrun_testkit::lines::says_reused(&second.stderr, 4, "tests"),
            "{}",
            second.stderr
        );
        // A change to the root hook file runs every spec file.
        r.write(
            "test/hooks.spec.js",
            "const assert = require('assert');\nbeforeEach(function () { this.base = 2; });\nit('base', function () { assert.equal(this.base, 2); });\n",
        );
        let hooks = r.run(MOCHA_RUN, &[]);
        assert_eq!(hooks.code, 3, "{}", out(&hooks));
        assert!(out(&hooks).contains("3 failing"), "{}", out(&hooks));
        assert_eq!(
            tests_of(&r, "test/greet1.spec.js")["hello"],
            json!("failed"),
            "{}",
            mocha.name
        );
    }
}
