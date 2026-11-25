//! \[ADR 0108\] Vue's single-file components under Vitest and Jest (C-58 to C-61): a component is a
//! source, read by the tests whose worker ran it; what its compile read besides itself (a type
//! imported for `defineProps<Props>()`) is read with it; hushrun's setup file loads in a browser's
//! environment (`jsdom`); and reuse never misses a failure.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check through the
//! one adapter's own type, `vitest::Recognized` or `jest::Recognized`, and the code every adapter
//! shares, and never starts the `hushrun` binary (`common/in_process.rs`). Each adapter has its own
//! functions, `run_vitest` and `run_jest`, so that a test of one reaches nothing of the other.
//! Vitest and Jest run as COMMAND, for real. A run's last line is the CLI's: `e2e_vue.rs`.
//!
//! The fixtures need Node and two `node_modules` installed from npm outside the repository:
//! `HUSHRUN_TEST_VUE` with Vitest 4.1.12, Vue, @vitejs/plugin-vue, @vue/test-utils and jsdom (CI:
//! `npm install --prefix <dir> --legacy-peer-deps vitest@4.1.12 vue@3.5.46 @vitejs/plugin-vue@6.0.11
//! @vue/test-utils@2.6.0 jsdom@26.1.0 typescript@5.9.3`), and `HUSHRUN_TEST_VUE_JEST` with Jest 29
//! and @vue/vue3-jest (`jest@29.7.0 jest-environment-jsdom@29.7.0 @vue/vue3-jest@29.2.6
//! babel-jest@29.7.0 @babel/core@7.29.13 @babel/preset-env@7.29.14 vue@3.5.46 @vue/test-utils@2.6.0
//! typescript@5.9.3`). Each says so and passes where its own is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`.

#[path = "common/vue.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, jest, vitest};

use fixtures::*;
use in_process::{Ran, Repo};

/// What Vitest's adapter answers that never changes in a run.
fn profile_vitest(adapter: &vitest::Recognized) -> Profile {
    Profile {
        word: adapter.word(),
        runner: adapter.runner(),
        test_file: adapter.test_file(),
        tests: adapter.tests(),
        hints: adapter.hints(),
        binds: adapter.binds(),
        keys: adapter.keys(),
    }
}

/// What Jest's adapter answers that never changes in a run.
fn profile_jest(adapter: &jest::Recognized) -> Profile {
    Profile {
        word: adapter.word(),
        runner: adapter.runner(),
        test_file: adapter.test_file(),
        tests: adapter.tests(),
        hints: adapter.hints(),
        binds: adapter.binds(),
        keys: adapter.keys(),
    }
}

/// COMMAND of `hushrun ARGS`, which are `run unit -- COMMAND`.
fn command_of<'a>(args: &'a [&'a str]) -> &'a [&'a str] {
    args.strip_prefix(&["run", "unit", "--"][..])
        .unwrap_or_else(|| panic!("{args:?} isn't `run unit -- COMMAND`"))
}

/// `hushrun ARGS`, which are `run unit -- vitest run …`, in-process, through Vitest's adapter
/// alone.
fn run_vitest(r: &Repo, args: &[&str]) -> Ran {
    let args = command_of(args);
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to Vitest's adapter (`vitest::recognized`), with no
    // trait object: what COMMAND is, and whether its config loads the adapter.
    let adapter = match vitest::recognize(hushrun::units::command::without_env(&command)) {
        Some(Ok(adapter)) => adapter,
        Some(Err(why)) => return in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        None => panic!("{args:?} doesn't start Vitest"),
    };
    if let Some(why) = vitest::unloadable(&adapter, &r.repo) {
        return in_process::Run::new(r, "unit", args, &[]).unobserved(&why);
    }
    observe_vitest(r, args, &[], &adapter)
}

/// `hushrun ARGS`, which are `run unit -- node node_modules/jest/bin/jest.js …`, in-process,
/// through Jest's adapter alone.
fn run_jest(r: &Repo, args: &[&str]) -> Ran {
    let args = command_of(args);
    let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    // As `units::adapter::choose_kept` comes to Jest's adapter (`jest::recognized`), with no trait
    // object: what COMMAND is, and which Jest the repository has.
    let adapter = match jest::recognize_in(hushrun::units::command::without_env(&command), &r.repo)
    {
        Some(Ok(adapter)) => adapter,
        Some(Err(why)) => return in_process::Run::new(r, "unit", args, &[]).unobserved(&why),
        None => panic!("{args:?} doesn't start Jest"),
    };
    observe_jest(r, args, &[], &adapter)
}

/// One run of the check, as `hushrun run` makes it, with `adapter`: each step of the code every
/// adapter shares (`units::Observer`), and around it each call `hushrun run` makes of the adapter,
/// in order, on `vitest::Recognized` itself (`units_gradle.rs`'s, with Vitest's type).
fn observe_vitest(
    r: &Repo,
    args: &[&str],
    env: &[(&str, &str)],
    adapter: &vitest::Recognized,
) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile_vitest(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // Vitest's adapter keeps the trait's `plan`, which is its `plan_from_files`.
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
/// in order, on `jest::Recognized` itself (`units_gradle.rs`'s, with Jest's type).
fn observe_jest(r: &Repo, args: &[&str], env: &[(&str, &str)], adapter: &jest::Recognized) -> Ran {
    let mut run = in_process::Run::new(r, "unit", args, env);
    let Some(mut observer) = run.open(profile_jest(adapter)) else {
        return run.plain();
    };
    let at_env = run.env();
    let variables = adapter.variables(&observer.start_at(&at_env));
    drop(at_env);
    run.started(&mut observer, variables);
    if let Some(input) = run.plan_input() {
        let planned = match observer.plan_context(&input) {
            // Jest's adapter keeps the trait's `plan`, which is its `plan_from_files`.
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
    /// `observe`'s line, without what it would have saved.
    fn observe_line(&self) -> String;
    /// Whether the lines, one after another, hold `text`.
    fn says(&self, text: &str) -> bool;
}

impl Said for Ran {
    fn observe_line(&self) -> String {
        let line = self
            .lines
            .iter()
            .find(|l| l.contains("with `tests: reuse`"))
            .unwrap_or_else(|| panic!("no observe line in:\n{}", self.said()));
        hushrun_testkit::lines::without_saving(line)
    }

    fn says(&self, text: &str) -> bool {
        self.lines.join("\n").contains(text)
    }
}

/// A run, by `run` (`run_vitest` or `run_jest`), that must pass, and what it said would have been
/// reused.
fn passes(r: &Repo, run: fn(&Repo, &[&str]) -> Ran, args: &[&str]) -> String {
    let ran = run(r, args);
    assert_eq!(ran.code, 0, "{}", ran.said());
    ran.observe_line()
}

/// After a first run, each change, alone, runs what reads it and leaves the rest reused: each a
/// run by `run` (`run_vitest` or `run_jest`).
fn changes(
    r: &Repo,
    run: fn(&Repo, &[&str]) -> Ran,
    args: &[&str],
    cases: &[(&str, &str, &str, &str)],
) {
    for (path, from, to, expected) in cases {
        let before = std::fs::read_to_string(r.repo.join(path)).expect("read");
        assert!(before.contains(from), "{path} holds {from:?}");
        r.write(path, &before.replacen(from, to, 1));
        let ran = run(r, args);
        assert_eq!(
            ran.observe_line(),
            *expected,
            "after {path}: {from:?} -> {to:?}\n{}",
            ran.said()
        );
        r.write(path, &before);
    }
}

const VITEST_RUN: &[&str] = &["run", "unit", "--", "node_modules/.bin/vitest", "run"];

// covers: C-58, C-59
#[test]
fn vitest_runs_a_component_s_tests_in_jsdom_and_a_change_runs_only_what_read_it() {
    let Some(modules) = tools(
        "Node and Vitest with Vue (HUSHRUN_TEST_VUE)",
        vitest_modules(),
    ) else {
        return;
    };
    let r = vitest_shop("observe", &modules);
    // hushrun's setup file loads in jsdom, and nothing a component's compile made is unknown.
    assert_eq!(
        passes(&r, run_vitest, VITEST_RUN),
        "unit: with `tests: reuse`, all 5 test files would have run: none had passed here yet on the files it read"
    );
    changes(
        &r,
        run_vitest,
        VITEST_RUN,
        &[
            // A component, its `<script setup>`'s import, a `src=` script: their test file alone.
            (
                "src/Plain.vue",
                "static text",
                "static words",
                "unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read",
            ),
            (
                "src/format.ts",
                "count: ",
                "n: ",
                "unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read",
            ),
            (
                "src/price-logic.ts",
                "toFixed(2)",
                "toFixed(1)",
                "unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read",
            ),
            // [C-59] A type a component's props came from: no code of it runs, its test file runs.
            (
                "src/types.ts",
                "on?: boolean",
                "on?: string",
                "unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read",
            ),
            // A `src=` template is HTML, of the rest set: every test file runs.
            (
                "src/price.html",
                "EUR",
                "USD",
                "unit: with `tests: reuse`, all 5 test files would have run: src/price.html changed, and any test may read it",
            ),
        ],
    );
}

// covers: C-40, C-59
#[test]
fn vitest_reads_test_follows_the_type_a_component_s_props_were_compiled_from() {
    let Some(modules) = tools(
        "Node and Vitest with Vue (HUSHRUN_TEST_VUE)",
        vitest_modules(),
    ) else {
        return;
    };
    let r = vitest_shop("{mode: observe, unit: test, reads: test}", &modules);
    passes(&r, run_vitest, VITEST_RUN);
    // The type makes the attribute a string: the test that mounted it fails, and it would have
    // run, with the other test of its file that imports the component.
    r.write(
        "src/types.ts",
        "export interface FlagProps { on?: string }\n",
    );
    let ran = run_vitest(&r, VITEST_RUN);
    assert_eq!(ran.code, 1, "{}", ran.said());
    assert!(
        ran.stdout.contains("a present attribute is true"),
        "{}",
        ran.stdout
    );
    assert!(
        !ran.says("though it passed before on the same files it read"),
        "{}",
        ran.said()
    );
    assert_eq!(
        ran.observe_line(),
        "unit: with `tests: reuse`, 2 of 8 tests would have run, in 1 of 5 test files; 6 already passed on the files they read"
    );
}

// covers: C-60
#[test]
fn vitest_with_vue_reuses_what_already_passed() {
    let Some(modules) = tools(
        "Node and Vitest with Vue (HUSHRUN_TEST_VUE)",
        vitest_modules(),
    ) else {
        return;
    };
    let r = vitest_shop("reuse", &modules);
    run_vitest(&r, VITEST_RUN);
    r.write(
        "src/Plain.vue",
        "<template>\n  <p>static words</p>\n</template>\n",
    );
    let ran = run_vitest(&r, VITEST_RUN);
    assert_eq!(ran.code, 1, "{}", ran.said());
    // The changed component's test file runs, and fails; one that passed runs as a check (C-34).
    let plan = ran
        .lines
        .iter()
        .find(|l| l.contains("test files run"))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", ran.said()));
    assert_eq!(
        plan,
        "unit: 2 of 5 test files run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(ran.stdout.contains("test/plain.test.ts"), "{}", ran.stdout);
    // What the run's last line says, `3 test files reused` (e2e_vue.rs), is the result's count.
    assert_eq!(ran.units["reused"], 3, "{}", ran.said());
}

// ---------------------------------------------------------------------------------------------
// Jest with @vue/vue3-jest.

const JEST_CONFIG: &str = "module.exports = {\n  testEnvironment: 'jsdom',\n  testEnvironmentOptions: { customExportConditions: ['node', 'node-addons'] },\n  moduleFileExtensions: ['js', 'ts', 'json', 'vue'],\n  transform: { '^.+\\\\.vue$': '@vue/vue3-jest', '^.+\\\\.js$': 'babel-jest' },\n  cacheDirectory: '<rootDir>/.jest-cache',\n};\n";

const JEST_RUN: &[&str] = &[
    "run",
    "unit",
    "--",
    "node",
    "node_modules/jest/bin/jest.js",
    "--ci",
];

// covers: C-61
#[test]
fn jest_with_vue3_jest_runs_only_what_read_a_changed_component() {
    let Some(modules) = tools(
        "Node and Jest with @vue/vue3-jest (HUSHRUN_TEST_VUE_JEST)",
        jest_modules(),
    ) else {
        return;
    };
    // Plain JavaScript: ts-jest, and vue3-jest for `lang="ts"`, parse tsconfig.json, which lists
    // every directory it includes, and a run with Jest's cache cold then reads all below them.
    // vue3-jest resolves no imported type (its compile has no file system): no flag here.
    let t = |body: &str| format!("import {{ mount }} from '@vue/test-utils'\n{body}");
    let files: Vec<(String, String)> = [
        ("hushrun.yaml", yaml("observe")),
        (".gitignore", "node_modules\n.jest-cache\n".to_string()),
        ("package.json", "{\"name\": \"shop\", \"private\": true}\n".into()),
        ("jest.config.js", JEST_CONFIG.into()),
        (
            "babel.config.js",
            "module.exports = { presets: [['@babel/preset-env', { targets: { node: 'current' } }]] };\n".into(),
        ),
        ("src/format.js", "export const label = (n) => `count: ${n}`\n".into()),
        ("src/Counter.vue", COUNTER.replace(" lang=\"ts\"", "")),
        (
            "src/price-logic.js",
            "export default { props: { cents: { type: Number, required: true } }, computed: { euros() { return (this.cents / 100).toFixed(2) } } }\n".into(),
        ),
        (
            "src/Price.vue",
            "<script src=\"./price-logic.js\"></script>\n<template>\n  <span>{{ euros }} EUR</span>\n</template>\n".into(),
        ),
        ("src/Badge.vue", BADGE.into()),
        ("src/Plain.vue", "<template>\n  <p>static text</p>\n</template>\n".into()),
        (
            "test/counter.test.js",
            t("import Counter from '../src/Counter.vue'\n\ntest('counts', async () => {\n  const w = mount(Counter)\n  await w.find('button').trigger('click')\n  expect(w.text()).toBe('count: 1')\n})\n"),
        ),
        (
            "test/price.test.js",
            t("import Price from '../src/Price.vue'\n\ntest('euros', () => {\n  expect(mount(Price, { props: { cents: 250 } }).text()).toBe('2.50 EUR')\n})\n"),
        ),
        (
            "test/badge.test.js",
            t("import Badge from '../src/Badge.vue'\n\ntest('badge', () => {\n  expect(mount(Badge, { props: { text: 'hi' } }).text()).toBe('hi')\n})\n"),
        ),
        (
            "test/plain.test.js",
            t("import Plain from '../src/Plain.vue'\n\ntest('plain', () => { expect(mount(Plain).text()).toBe('static text') })\n"),
        ),
    ]
    .into_iter()
    .map(|(p, t)| (p.to_string(), t))
    .collect();
    let r = repo(&borrowed(&files), &modules);
    // Jest's cache holds every component before hushrun first sees them: vue3-jest then reads no
    // `src=` file, and hushrun reads it from the component.
    let mut warm = r.command("node");
    warm.args(["node_modules/jest/bin/jest.js", "--ci"]);
    let warmed = warm.output().expect("jest");
    assert!(
        warmed.status.success(),
        "{}",
        String::from_utf8_lossy(&warmed.stderr)
    );
    assert_eq!(
        passes(&r, run_jest, JEST_RUN),
        "unit: with `tests: reuse`, all 4 test files would have run: none had passed here yet on the files it read"
    );
    changes(
        &r,
        run_jest,
        JEST_RUN,
        &[
            (
                "src/Plain.vue",
                "static text",
                "static words",
                "unit: with `tests: reuse`, 1 of 4 test files would have run; 3 already passed on the files they read",
            ),
            (
                "src/Badge.vue",
                "color: red",
                "color: blue",
                "unit: with `tests: reuse`, 1 of 4 test files would have run; 3 already passed on the files they read",
            ),
            // [C-61] A `src=` script, which vue3-jest reads only when Jest's cache doesn't hold the
            // component (and whose change that cache doesn't see): read with the component.
            (
                "src/price-logic.js",
                "toFixed(2)",
                "toFixed(1)",
                "unit: with `tests: reuse`, 1 of 4 test files would have run; 3 already passed on the files they read",
            ),
        ],
    );
}
