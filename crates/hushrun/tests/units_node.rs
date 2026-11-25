//! \[ADR 0107\] Test files and tests with Node's own test runner (`node --test`) and with Mocha
//! (C-30 to C-35, C-39, C-54 to C-57): a unit is a test file; what it read is what its process
//! (Node's runner) or its windows and module graph (Mocha) read; each test is told on its own; and
//! reuse never misses a failure.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- node --test` or `-- node node_modules/mocha/bin/mocha.js` would, through the one
//! adapter's own type, `node_test::Recognized` or `mocha::Recognized`, and the code every adapter
//! shares, and never starts the `hushrun` binary (`common/in_process.rs`). Each adapter has its
//! own functions, `observe_node` and `observe_mocha`, so that a test of one reaches nothing of the
//! other. Node and Mocha run as COMMAND, for real. What only the CLI shows (a run's last line, the
//! check's skip, `hushrun why`) is in `e2e_node.rs`.
//!
//! The fixtures need a Node of 22.15 or later, outside the repository: the `bin` directories
//! `HUSHRUN_TEST_NODE22` and `HUSHRUN_TEST_NODE24` name (CI: Node 22.20.0 and 24.11.0 from
//! nodejs.org), and Mocha 10 and 11 in the `node_modules` `HUSHRUN_TEST_MOCHA10` and
//! `HUSHRUN_TEST_MOCHA11` name (CI: `npm install --prefix <dir> mocha@10.8.2`, and `mocha@11.7.5`).
//! Each runs on every one it finds, says so and passes where all are missing, unless
//! `HUSHRUN_TEST_TOOLS=required`, which needs them all.

#[path = "common/node.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, mocha, node_test};
use serde_json::json;

use fixtures::*;
use in_process::{Ran, Repo};

/// What the adapter for Node's test runner answers that never changes in a run.
fn profile_node(adapter: &node_test::Recognized) -> Profile {
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

/// What Mocha's adapter answers that never changes in a run.
fn profile_mocha(adapter: &mocha::Recognized) -> Profile {
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

/// COMMAND of `hushrun ARGS`, which are `run unit -- COMMAND`.
fn command_of<'a>(args: &'a [&'a str]) -> &'a [&'a str] {
    args.strip_prefix(&["run", "unit", "--"][..])
        .unwrap_or_else(|| panic!("{args:?} isn't `run unit -- COMMAND`"))
}

/// `hushrun ARGS`, which are `run unit -- node --test …`, in-process, through the adapter for
/// Node's test runner alone.
fn run(r: &Repo, args: &[&str]) -> Ran {
    run_env(r, args, &[])
}

/// [`run`], with `env` besides.
fn run_env(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let args = command_of(args);
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to the adapter (`node_test::recognized`), with no
    // trait object.
    let adapter = match node_test::recognize(hushrun::units::command::without_env(&command)) {
        Some(Ok(adapter)) => adapter,
        Some(Err(why)) => return in_process::Run::new(r, "unit", args, env).unobserved(&why),
        None => panic!("{args:?} doesn't start Node's test runner"),
    };
    observe_node(r, args, env, &adapter)
}

/// `hushrun ARGS`, which are `run unit -- node node_modules/mocha/bin/mocha.js …`, in-process,
/// through Mocha's adapter alone.
fn run_mocha(r: &Repo, args: &[&str]) -> Ran {
    let args = command_of(args);
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to Mocha's adapter (`mocha::recognized`), with no
    // trait object: what COMMAND is, and which Mocha the repository has.
    let adapter = match mocha::recognize_in(hushrun::units::command::without_env(&command), &r.repo)
    {
        Some(Ok(adapter)) => adapter,
        Some(Err(why)) => return in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        None => panic!("{args:?} doesn't start Mocha"),
    };
    observe_mocha(r, args, &[], &adapter)
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order, on `node_test::Recognized` itself (`units_gradle.rs`'s, with the type of the adapter for Node's test runner).
fn observe_node(
    r: &Repo,
    args: &[&str],
    env: &[(&str, &str)],
    adapter: &node_test::Recognized,
) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile_node(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // The adapter keeps the trait's `plan`, which is its `plan_from_files`.
            Ok(context) => {
                let planning = adapter.plan_from_files(&observer.plan_at(&input, &context));
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

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order, on `mocha::Recognized` itself (`units_gradle.rs`'s, with Mocha's type).
fn observe_mocha(
    r: &Repo,
    args: &[&str],
    env: &[(&str, &str)],
    adapter: &mocha::Recognized,
) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile_mocha(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // Mocha's adapter keeps the trait's `plan`, which is its `plan_from_files`.
            Ok(context) => {
                let planning = adapter.plan_from_files(&observer.plan_at(&input, &context));
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

/// What a run's lines say, as the tests ask it: each is a line `hushrun run` writes after
/// `hushrun: `.
trait Said {
    /// Whether the lines, one after another, hold `text`.
    fn says(&self, text: &str) -> bool;
}

impl Said for Ran {
    fn says(&self, text: &str) -> bool {
        let mut lines = self.lines.join("\n");
        lines.push('\n');
        lines.contains(text)
    }
}

// covers: C-30, C-31, C-32, C-54
#[test]
fn nodes_test_files_are_observed_from_what_each_process_loaded_and_opened() {
    for node in nodes() {
        let r = repo(&node, &node_shop("observe"));
        let first = run(&r, NODE_RUN);
        assert_eq!(first.code, 0, "{}: {}", node.name, first.said());
        assert_eq!(
            first.line("unit: with"),
            "unit: with `tests: reuse`, all 6 test files would have run: none had passed here yet on the files it read",
            "{}",
            node.name
        );
        assert_eq!(
            reads(&r, "test/cart.test.js"),
            ["src/cart.js", "src/pricing.js", "test/cart.test.js"],
            "{}",
            node.name
        );
        assert_eq!(
            reads(&r, "test/rate.test.mjs"),
            ["fixtures/rate.js", "test/rate.test.mjs"],
            "{}",
            node.name
        );
        assert_eq!(
            reads(&r, "test/greet1.test.mjs"),
            ["src/greet.mjs", "test/greet1.test.mjs"],
            "{}",
            node.name
        );
        assert_eq!(r.map()["unitFiles"].as_array().unwrap().len(), 6);
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let changed = run(&r, NODE_RUN);
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("unit: with")).starts_with(
                "unit: with `tests: reuse`, 1 of 6 test files would have run; 5 already passed on the files they read"
            ),
            "{}: {}",
            node.name,
            changed.said()
        );
    }
}

// covers: C-34, C-54
#[test]
fn node_reuses_test_files_that_already_passed_and_runs_what_a_change_reaches() {
    for node in nodes() {
        let r = repo(&node, &node_shop("reuse"));
        let first = run(&r, NODE_RUN);
        assert_eq!(first.code, 0, "{}", first.said());
        assert_eq!(
            first.line("unit: every"),
            "unit: every test file runs: no run here has listed the test files of Node's test runner yet"
        );
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let second = run(&r, NODE_RUN);
        assert_eq!(second.code, 0, "{}", second.said());
        assert_eq!(
            second.line("unit: 2"),
            "unit: 2 of 6 test files run; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check",
            "{}",
            node.name
        );
        // What the run's last line says, `4 test files reused` (e2e_node.rs), is the result's
        // count.
        assert_eq!(second.units["reused"], 4, "{}", second.said());
        // A change that breaks the cart: its test file runs, and fails.
        r.write("src/pricing.js", "exports.price = (x) => x * 3;\n");
        let broken = run(&r, NODE_RUN);
        assert_eq!(broken.code, 1, "{}", broken.said());
        // A change to a data file it opened is caught too.
        r.write("src/pricing.js", "exports.price = (x) => x * 2;\n");
        assert_eq!(run(&r, NODE_RUN).code, 0);
        r.write("fixtures/rate.js", "3\n");
        let data = run(&r, NODE_RUN);
        assert_eq!(data.code, 1, "{}", data.said());
        // Why, with the imports the adapter saw, is end to end: e2e_node.rs.
    }
}

// covers: C-39, C-55
#[test]
fn node_tells_each_test_and_leaves_out_one_by_one_what_passed() {
    for node in nodes() {
        let r = repo(&node, &node_tests());
        let first = run_env(&r, NODE_RUN, &[("FAIL", "1")]);
        assert_eq!(first.code, 1, "{}", first.said());
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
        let second = run(&r, NODE_RUN);
        assert_eq!(second.code, 0, "{}", second.said());
        // COMMAND's output alone: hushrun's lines aren't Node's.
        let out = format!("{}\n{}", second.stdout, second.stderr);
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
        // That the same code again skips the check whole is the CLI's: e2e_node.rs.
    }
}

// covers: C-39, C-55
#[test]
fn node_keeps_no_pass_after_a_failing_hook_or_an_error_no_test_owns() {
    let files = vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file("package.json", "{\"name\": \"shop\", \"private\": true}\n"),
        file(
            "test/hooks.test.js",
            "const { describe, it, after, test } = require('node:test');\ndescribe('blk', () => {\n  after(() => { throw new Error('block teardown'); });\n  it('one', () => {});\n});\ntest('fine', () => {});\n",
        ),
        file(
            "test/root.test.js",
            "const { after, test } = require('node:test');\nafter(() => { throw new Error('root teardown'); });\ntest('r1', () => {});\n",
        ),
        file(
            "test/leak.test.js",
            "const { test } = require('node:test');\ntest('leaks', () => { setTimeout(() => { throw new Error('late timer'); }, 5); });\ntest('waits', async () => { await new Promise((r) => setTimeout(r, 50)); });\n",
        ),
        file(
            "test/fine.test.js",
            "require('node:test')('fine', () => {});\n",
        ),
    ];
    for node in nodes() {
        let r = repo(&node, &files);
        let first = run(&r, NODE_RUN);
        assert_eq!(first.code, 1, "{}", first.said());
        let hooks = tests_of(&r, "test/hooks.test.js");
        assert_eq!(hooks["blk > one"], "failed", "{}: {hooks:?}", node.name);
        assert_eq!(hooks["fine"], "passed", "{hooks:?}");
        assert_eq!(tests_of(&r, "test/root.test.js")["r1"], "failed");
        let leak = tests_of(&r, "test/leak.test.js");
        assert!(
            leak.values().all(|s| s != "passed"),
            "{}: {leak:?}",
            node.name
        );
        // The same code again: every one of them runs, and fails again.
        let second = run(&r, NODE_RUN);
        assert_eq!(second.code, 1, "{}", second.said());
        for said in ["block teardown", "root teardown", "late timer"] {
            assert!(second.said().contains(said), "{said}:\n{}", second.said());
        }
    }
}

// covers: C-32, C-54
#[test]
fn node_commands_hushrun_doesnt_observe_run_as_written_and_say_why() {
    for node in nodes() {
        let r = repo(&node, &node_shop("reuse"));
        let none = run(
            &r,
            &[
                "run",
                "unit",
                "--",
                "node",
                "--test",
                "--test-isolation=none",
            ],
        );
        assert!(
            none.says(
                "unit: test files aren't observed: node --test --test-isolation=none runs every test file in one process"
            ),
            "{}",
            none.said()
        );
        assert_eq!(run(&r, NODE_RUN).code, 0);
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let covered = run(
            &r,
            &[
                "run",
                "unit",
                "--",
                "node",
                "--test",
                "--experimental-test-coverage",
            ],
        );
        assert_eq!(covered.code, 0, "{}", covered.said());
        assert!(
            covered.says(
                "unit: every test file runs: Node's test coverage is on, and a partial run's coverage would read as a drop"
            ),
            "{}",
            covered.said()
        );
        // A shard: test files aren't left out, since a shorter list shards otherwise; tests are.
        r.write("src/pricing.js", "exports.price = (x) => x * 2;\n");
        let shard = &["run", "unit", "--", "node", "--test", "--test-shard=1/1"];
        assert_eq!(run(&r, shard).code, 0);
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let sharded = run(&r, shard);
        assert_eq!(sharded.code, 0, "{}", sharded.said());
        assert!(sharded.says("--test-shard"), "{}", sharded.said());
    }
}

// covers: C-30, C-31, C-32, C-56, C-57
#[test]
fn mocha_spec_files_are_observed_through_their_windows_and_module_graph() {
    for mocha in mochas() {
        let r = repo(&mocha, &mocha_shop("observe"));
        let first = run_mocha(&r, MOCHA_RUN);
        assert_eq!(first.code, 0, "{}: {}", mocha.name, first.said());
        // The root hook's spec file isn't a unit: every spec file reads it.
        let units: Vec<String> = r.map()["unitFiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            units,
            [
                "test/cart.spec.js",
                "test/esm.spec.mjs",
                "test/greet1.spec.js",
                "test/greet2.spec.js",
                "test/greet3.spec.js",
                "test/rate.spec.js"
            ],
            "{}",
            mocha.name
        );
        assert_eq!(
            reads(&r, "test/greet1.spec.js"),
            ["src/greet.js", "test/greet1.spec.js", "test/hooks.spec.js"],
            "{}",
            mocha.name
        );
        assert_eq!(
            reads(&r, "test/esm.spec.mjs"),
            ["src/greet.js", "test/esm.spec.mjs", "test/hooks.spec.js"],
            "{}",
            mocha.name
        );
        // What the config module read once and kept, for a spec file whose test read it after.
        assert_eq!(
            reads(&r, "test/rate.spec.js"),
            [
                "fixtures/rate.js",
                "src/config.js",
                "test/hooks.spec.js",
                "test/rate.spec.js"
            ],
            "{}",
            mocha.name
        );
        r.write("fixtures/rate.js", "3\n");
        let changed = run_mocha(&r, MOCHA_RUN);
        // Mocha exits with how many tests failed.
        assert_eq!(changed.code, 2, "{}", changed.said());
        assert!(
            hushrun_testkit::lines::without_saving(changed.line("unit: with")).starts_with(
                "unit: with `tests: reuse`, 2 of 6 spec files would have run; 4 already passed on the files they read"
            ),
            "{}: {}",
            mocha.name,
            changed.said()
        );
    }
}

// covers: C-34, C-39, C-56, C-57
#[test]
fn mocha_reuses_what_passed_and_its_root_hooks_still_run() {
    for mocha in mochas() {
        let r = repo(&mocha, &mocha_shop("{mode: reuse, unit: test}"));
        let first = run_mocha(&r, MOCHA_RUN);
        assert_eq!(first.code, 0, "{}", first.said());
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let second = run_mocha(&r, MOCHA_RUN);
        assert_eq!(second.code, 0, "{}", second.said());
        // The cart's two tests, one more as a check, and the root hook file's own test, which
        // isn't a unit and always runs.
        assert!(
            second.said().contains("4 passing"),
            "{}: {}",
            mocha.name,
            second.said()
        );
        // What the run's last line says, `4 tests reused` (e2e_node.rs), is the result's count.
        assert_eq!(second.units["tests"]["reused"], 4, "{}", second.said());
        // A change to the root hook file runs every spec file.
        r.write(
            "test/hooks.spec.js",
            "const assert = require('assert');\nbeforeEach(function () { this.base = 2; });\nit('base', function () { assert.equal(this.base, 2); });\n",
        );
        let hooks = run_mocha(&r, MOCHA_RUN);
        assert_eq!(hooks.code, 3, "{}", hooks.said());
        assert!(hooks.said().contains("3 failing"), "{}", hooks.said());
        assert_eq!(
            tests_of(&r, "test/greet1.spec.js")["hello"],
            json!("failed"),
            "{}",
            mocha.name
        );
    }
}

// covers: C-32, C-56
#[test]
fn mocha_commands_hushrun_doesnt_observe_run_as_written_and_say_why() {
    for mocha in mochas() {
        let r = repo(&mocha, &mocha_shop("reuse"));
        let parallel = run_mocha(
            &r,
            &[
                "run",
                "unit",
                "--",
                "node",
                "node_modules/mocha/bin/mocha.js",
                "--parallel",
            ],
        );
        assert!(
            parallel.says(
                "unit: test files aren't observed: mocha --parallel runs spec files in worker processes"
            ),
            "{}: {}",
            mocha.name,
            parallel.said()
        );
        // Run as written: in worker processes, a root hook is its own spec file's alone.
        assert!(parallel.said().contains("passing"), "{}", parallel.said());
        // A spec file Mocha lists and the plan doesn't: nothing is left out, and it says why.
        assert_eq!(run_mocha(&r, MOCHA_RUN).code, 0);
        r.write("test/new.spec.js", "it('new', () => {});\n");
        r.write("src/pricing.js", "exports.price = (x) => x + x;\n");
        let listed = run_mocha(&r, MOCHA_RUN);
        assert_eq!(listed.code, 0, "{}", listed.said());
    }
}
