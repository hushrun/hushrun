//! \[ADR 0109\] Svelte with Vitest (C-62 to C-65): a test file, and a test, read the `.svelte`
//! files and runes modules it ran through vite-plugin-svelte's compile, what a preprocessor read
//! for them, and the Svelte config; SvelteKit's modules made from its config don't make a test
//! file `not observed`; and a component test in jsdom loads hushrun's setup file at all.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- vitest run` would, through Vitest's adapter's own type, `vitest::Recognized`, and
//! the code every adapter shares, and never starts the `hushrun` binary (`common/in_process.rs`).
//! Vitest and Svelte run as COMMAND, for real. `hushrun why` is the CLI's: `e2e_svelte.rs`.
//!
//! The fixtures need Node and, installed from npm outside the repository with
//! `HUSHRUN_TEST_SVELTE` naming its `node_modules`, Vitest 4.1.12 with Svelte, vite-plugin-svelte,
//! SvelteKit, Testing Library and jsdom (CI: `npm install --prefix <dir> --legacy-peer-deps
//! vitest@4.1.12 vite@7.3.8 svelte@5.59.1 @sveltejs/vite-plugin-svelte@6.2.4
//! @sveltejs/kit@2.70.4 @testing-library/svelte@5.4.2 jsdom@26.1.0`). They say so and pass
//! where it is missing, unless `HUSHRUN_TEST_TOOLS=required`.

#[path = "common/svelte.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::collections::BTreeSet;

use hushrun::units::adapter::Adapter as _;
use hushrun::units::{Profile, vitest};

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
        not_by_test: adapter.not_by_test(),
        not_watched: adapter.not_watched(),
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

/// What a run's lines say, as the tests ask it: each is a line `hushrun run` writes after
/// `hushrun: `.
trait Said {
    /// Whether one of the lines is `line`, but for the `, saving about …` that `observe`'s line
    /// gains when what it would have reused took a second or more, which is the run's timing.
    fn says(&self, line: &str) -> bool;
}

impl Said for Ran {
    fn says(&self, line: &str) -> bool {
        self.lines
            .iter()
            .any(|l| hushrun_testkit::lines::without_saving(l) == line)
    }
}

const VITEST_RUN: &[&str] = &["run", "unit", "--", "node_modules/.bin/vitest", "run"];

// covers: C-62, C-63
#[test]
fn a_svelte_test_file_reads_the_components_and_runes_modules_it_ran() {
    if !tools() {
        return;
    }
    let r = repo(&app("observe"));
    let first = run_vitest(&r, VITEST_RUN);
    assert_eq!(first.code, 0, "{}", first.said());
    // jsdom's worker loaded hushrun's setup file, which Vite serves from outside the workspace,
    // and every test file was followed: vite-plugin-svelte's plugins are known.
    let (reads, not_followed) = read_by(&r, "src/counter.test.js");
    assert_eq!(
        reads,
        [
            "src/counter.test.js",
            "src/lib/Counter.svelte",
            "src/lib/Label.svelte",
            "src/lib/counter.svelte.js",
            "vite.config.js"
        ]
    );
    assert!(not_followed.is_null(), "{not_followed:#}");
    let (reads, _) = read_by(&r, "src/runes.test.js");
    assert_eq!(
        reads,
        [
            "src/lib/counter.svelte.js",
            "src/runes.test.js",
            "vite.config.js"
        ]
    );
    // A component's change runs the test files that rendered it, and no other.
    r.write(
        "src/lib/Label.svelte",
        "<script>\n  let { text } = $props()\n</script>\n<span>{text}?</span>\n",
    );
    let second = run_vitest(&r, VITEST_RUN);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.says("unit: with `tests: reuse`, 2 of 4 test files would have run; 2 already passed on the files they read"),
        "{}",
        second.said()
    );
    // A runes module's, those that ran it.
    r.write(
        "src/lib/Label.svelte",
        "<script>\n  let { text } = $props()\n</script>\n<span>{text}!</span>\n",
    );
    let third = run_vitest(&r, VITEST_RUN);
    assert_eq!(third.code, 0, "{}", third.said());
    r.write(
        "src/lib/counter.svelte.js",
        "export function createCounter(start = 0) {\n  let count = $state(start)\n  return { get count() { return count }, increment() { count += 2 } }\n}\n",
    );
    let fourth = run_vitest(&r, VITEST_RUN);
    assert_eq!(fourth.code, 1, "{}", fourth.said());
    assert!(
        fourth.says("unit: with `tests: reuse`, 3 of 4 test files would have run; 1 already passed on the files it read"),
        "{}",
        fourth.said()
    );
}

// covers: C-62, C-63
#[test]
fn a_svelte_test_reads_the_components_it_rendered_and_no_other() {
    if !tools() {
        return;
    }
    let r = repo(&app("{mode: observe, unit: test, reads: test}"));
    let first = run_vitest(&r, VITEST_RUN);
    assert_eq!(first.code, 0, "{}", first.said());
    // Label is two imports away from the test file, and only Counter's render runs it: `hello`,
    // in the same file, ran no function of it (C-40 still gives every test of a file what the
    // file imports directly).
    r.write(
        "src/lib/Label.svelte",
        "<script>\n  let { text } = $props()\n</script>\n<span>{text}?</span>\n",
    );
    let second = run_vitest(&r, VITEST_RUN);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.says("unit: with `tests: reuse`, 2 of 5 tests would have run, in 2 of 4 test files; 3 already passed on the files they read"),
        "{}",
        second.said()
    );
    // What `hushrun why` says of the test is end to end: e2e_svelte.rs.
}

/// A Svelte config with a markup preprocessor of the repository's, in a file of its own, that
/// includes another file into a component and says so in its `dependencies`.
fn preprocessed(tests: &str) -> Vec<(&'static str, String)> {
    let mut files = app(tests);
    files.extend([
        (
            "svelte.config.js",
            "import { include } from './tools/include.js'\n\nexport default { preprocess: [include()] }\n".into(),
        ),
        (
            "tools/include.js",
            "import fs from 'node:fs'\nimport path from 'node:path'\n\nexport function include() {\n  return {\n    name: 'include',\n    markup({ content, filename }) {\n      const dependencies = []\n      const code = content.replace(/<!-- include:(\\S+) -->/g, (_, rel) => {\n        const file = path.resolve(path.dirname(filename), rel)\n        dependencies.push(file)\n        return fs.readFileSync(file, 'utf8')\n      })\n      return { code, dependencies }\n    },\n  }\n}\n".into(),
        ),
        ("src/partials/Footer.svelte", "<p>footer text</p>\n".into()),
        (
            "src/lib/Page.svelte",
            "<main>page</main>\n<!-- include:../partials/Footer.svelte -->\n".into(),
        ),
        (
            "src/page.test.js",
            "import { render } from '@testing-library/svelte'\nimport { test, expect } from 'vitest'\nimport Page from './lib/Page.svelte'\n\ntest('page', () => { expect(render(Page).container.textContent).toContain('footer text') })\n".into(),
        ),
    ]);
    files
}

// covers: C-64
#[test]
fn what_a_svelte_preprocessor_read_and_the_svelte_config_are_read() {
    if !tools() {
        return;
    }
    let r = repo(&preprocessed("observe"));
    let first = run_vitest(&r, VITEST_RUN);
    assert_eq!(first.code, 0, "{}", first.said());
    // The file the preprocessor included, which Vite's process read, not the worker; and the
    // config with the file it imports, which every test file reads.
    let (reads, not_followed) = read_by(&r, "src/page.test.js");
    assert_eq!(
        reads,
        [
            "src/lib/Page.svelte",
            "src/page.test.js",
            "src/partials/Footer.svelte",
            "svelte.config.js",
            "tools/include.js",
            "vite.config.js"
        ]
    );
    assert!(not_followed.is_null(), "{not_followed:#}");
    let (reads, _) = read_by(&r, "src/math.test.js");
    assert!(reads.contains(&"tools/include.js".to_string()), "{reads:?}");
    r.write("src/partials/Footer.svelte", "<p>footer words</p>\n");
    let second = run_vitest(&r, VITEST_RUN);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.says("unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read"),
        "{}",
        second.said()
    );
}

// covers: C-64
#[test]
fn what_a_svelte_preprocessor_read_is_read_by_the_test_that_rendered_it() {
    if !tools() {
        return;
    }
    let r = repo(&preprocessed("{mode: observe, unit: test, reads: test}"));
    let first = run_vitest(&r, VITEST_RUN);
    assert_eq!(first.code, 0, "{}", first.said());
    r.write("src/partials/Footer.svelte", "<p>footer words</p>\n");
    let second = run_vitest(&r, VITEST_RUN);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.says("unit: with `tests: reuse`, 1 of 6 tests would have run, in 1 of 5 test files; 5 already passed on the files they read"),
        "{}",
        second.said()
    );
}

/// A SvelteKit 2 app, its config in `svelte.config.js` with a file it imports, whose module reads
/// `$app/environment` and `$env/static/public`.
fn kit() -> Vec<(&'static str, String)> {
    vec![
        ("hushrun.yaml", yaml("observe")),
        (".gitignore", "node_modules\n.svelte-kit\n".into()),
        (
            "package.json",
            "{\"name\": \"kit\", \"private\": true, \"type\": \"module\"}\n".into(),
        ),
        (
            "vite.config.js",
            format!(
                "import {{ sveltekit }} from '@sveltejs/kit/vite'\nimport {{ defineConfig }} from 'vitest/config'\n\nexport default defineConfig({{\n  plugins: [sveltekit(), {LINE}],\n  test: {{ include: ['src/**/*.test.js'] }},\n}})\n"
            ),
        ),
        (
            "svelte.config.js",
            "import { paths } from './config/paths.js'\n\nexport default { kit: { paths } }\n".into(),
        ),
        ("config/paths.js", "export const paths = { base: '' }\n".into()),
        (".env", "PUBLIC_GREETING=hey\n".into()),
        (
            "src/app.html",
            "<!doctype html><html><head>%sveltekit.head%</head><body>%sveltekit.body%</body></html>\n".into(),
        ),
        ("src/routes/+page.svelte", "<h1>home</h1>\n".into()),
        (
            "src/lib/where.js",
            "import { browser, dev } from '$app/environment'\nimport { PUBLIC_GREETING } from '$env/static/public'\n\nexport const where = () => `${browser ? 'browser' : 'server'} ${dev} ${PUBLIC_GREETING}`\n".into(),
        ),
        ("src/lib/two.js", "export const two = () => 2\n".into()),
        (
            "src/where.test.js",
            "import { test, expect } from 'vitest'\nimport { where } from '$lib/where.js'\n\ntest('where', () => { expect(where()).toBe('server true hey') })\n".into(),
        ),
        (
            "src/two.test.js",
            "import { test, expect } from 'vitest'\nimport { two } from '$lib/two.js'\n\ntest('two', () => { expect(two()).toBe(2) })\n".into(),
        ),
    ]
}

// covers: C-65
#[test]
fn a_sveltekit_test_file_reads_its_config_and_its_env_modules_are_no_virtual_module() {
    if !tools() {
        return;
    }
    let r = repo(&kit());
    let first = run_vitest(&r, VITEST_RUN);
    assert_eq!(first.code, 0, "{}", first.said());
    // `$app/environment` and `$env/static/public` are made from the config and `.env`, which
    // every test file reads (`.env` as the rest set), not from no one file.
    let (reads, not_followed) = read_by(&r, "src/where.test.js");
    assert_eq!(
        reads,
        [
            "config/paths.js",
            "src/lib/where.js",
            "src/where.test.js",
            "svelte.config.js",
            "vite.config.js"
        ]
    );
    assert!(not_followed.is_null(), "{not_followed:#}");
    r.write("src/lib/two.js", "export const two = () => 3\n");
    let second = run_vitest(&r, VITEST_RUN);
    assert_eq!(second.code, 1, "{}", second.said());
    assert!(
        second.says("unit: with `tests: reuse`, 1 of 2 test files would have run; 1 already passed on the files it read"),
        "{}",
        second.said()
    );
    // An env file changes what every test may read.
    r.write("src/lib/two.js", "export const two = () => 2\n");
    assert_eq!(run_vitest(&r, VITEST_RUN).code, 0);
    r.write(".env", "PUBLIC_GREETING=ho\n");
    let third = run_vitest(&r, VITEST_RUN);
    assert_eq!(third.code, 1, "{}", third.said());
    assert!(
        third.says("unit: with `tests: reuse`, all 2 test files would have run: .env changed, and any test may read it"),
        "{}",
        third.said()
    );
}
