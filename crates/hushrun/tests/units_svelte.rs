//! \[ADR 0109\] Svelte with Vitest (C-62 to C-65): a test file, and a test, read the `.svelte`
//! files and runes modules it ran through vite-plugin-svelte's compile, what a preprocessor read
//! for them, and the Svelte config; SvelteKit's modules made from its config don't make a test
//! file `not observed`; and a component test in jsdom loads hushrun's setup file at all.
//!
//! The fixtures need Node and, installed from npm outside the repository with
//! `HUSHRUN_TEST_SVELTE` naming its `node_modules`, Vitest 4.1.12 with Svelte, vite-plugin-svelte,
//! SvelteKit, Testing Library and jsdom (CI: `npm install --prefix <dir> --legacy-peer-deps
//! vitest@4.1.12 vite@7.3.8 svelte@5.59.1 @sveltejs/vite-plugin-svelte@6.2.4
//! @sveltejs/kit@2.70.4 @testing-library/svelte@5.4.2 jsdom@26.1.0`). They say so and pass
//! where it is missing, unless `HUSHRUN_TEST_TOOLS=required`.

#[path = "common/store.rs"]
mod store;

use std::path::PathBuf;
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

/// The Vitest these tests are written against, with the Svelte it compiles.
const VITEST: &str = "4.1.12";
const SVELTE: &str = "5.59.1";

const CLEARED: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "BUILDKITE",
    "CIRCLECI",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "HUSHRUN_AGENT",
    "HUSHRUN_BACKEND",
    "HUSHRUN_TOKEN",
    "HUSHRUN_CONFIG",
    "HUSHRUN_FORMAT",
    "HUSHRUN_PROVIDER",
    "HUSHRUN_INSIDE_RUN",
    "HUSHRUN_UNITS",
    "HUSHRUN_VITEST",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "NODE_OPTIONS",
    "VITEST",
    "VITEST_POOL_ID",
    "VITEST_WORKER_ID",
    "npm_lifecycle_event",
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
    "HUSHRUN_BUILD",
    "HUSHRUN_BUILD_KEEP_DAYS",
    "HUSHRUN_BUILD_DIR",
    "HUSHRUN_BUILD_SESSION",
    "HUSHRUN_CC",
    "CMAKE_C_COMPILER_LAUNCHER",
    "CMAKE_CXX_COMPILER_LAUNCHER",
    "RUSTC_WRAPPER",
];

const NO_HOST: &str = "http://127.0.0.1:9";
const LINE: &str = "process.env.HUSHRUN_VITEST ? import(process.env.HUSHRUN_VITEST).then((m) => m.default()) : undefined";
const VITEST_RUN: &[&str] = &["run", "unit", "--", "node_modules/.bin/vitest", "run"];

/// The `node_modules` with the pinned Vitest and Svelte, when this machine has it.
fn svelte_modules() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HUSHRUN_TEST_SVELTE")?);
    let version = |package: &str| -> Option<String> {
        let text = std::fs::read(dir.join(package).join("package.json")).ok()?;
        let json: Value = serde_json::from_slice(&text).ok()?;
        json["version"].as_str().map(str::to_string)
    };
    let pinned = version("vitest")? == VITEST && version("svelte")? == SVELTE;
    let rest = [
        "@sveltejs/vite-plugin-svelte",
        "@sveltejs/kit",
        "@testing-library/svelte",
        "jsdom",
    ]
    .iter()
    .all(|p| version(p).is_some());
    (pinned && rest).then_some(dir)
}

fn has_svelte() -> bool {
    svelte_modules().is_some()
        && Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn tools() -> bool {
    if has_svelte() {
        return true;
    }
    let what = format!("Node, Vitest {VITEST} and Svelte {SVELTE} (HUSHRUN_TEST_SVELTE)");
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// Whether hushrun printed `line`, or `line` with the time it would save after it
    /// (`, saving about 1s`), which depends on how long the reused tests took.
    fn says(&self, line: &str) -> bool {
        self.stderr.lines().any(|l| {
            l == line
                || l.strip_prefix(line)
                    .is_some_and(|rest| rest.starts_with(", saving about "))
        })
    }
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("real path");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git::init(&repo, "sha1");
        let r = Repo {
            _dir: dir,
            cache: root.join("cache"),
            root,
            repo,
        };
        for (path, text) in files {
            r.write(path, text);
        }
        let modules = svelte_modules().expect("svelte");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&modules, r.repo.join("node_modules")).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&modules, r.repo.join("node_modules")).expect("link");
        git::commit_all(&r.repo, "init");
        r
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn run(&self, args: &[&str]) -> Ran {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hushrun"));
        cmd.current_dir(&self.repo)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", self.root.join("credentials.json"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_NO_UPDATE_CHECK", "1")
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            );
        for var in CLEARED {
            cmd.env_remove(var);
        }
        hushrun_testkit::env::Hermetic::hermetic(&mut cmd);
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("NEXTEST")
                || (name.starts_with("CARGO_") && name != "CARGO_HOME")
                || name.starts_with("CLAUDE")
            {
                cmd.env_remove(name.as_ref());
            }
        }
        let output = cmd.args(args).output().expect("hushrun");
        Ran {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// What the check's map says `unit` read, and what wasn't followed of it.
    fn reads(&self, unit: &str) -> (Vec<String>, Value) {
        let map = store::map(&self.cache);
        let reads = map["units"][unit]["reads"]
            .as_array()
            .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        (reads, map["units"][unit]["notFollowed"].clone())
    }
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A Svelte 5 app tested with Testing Library in jsdom: a counter whose label is a component of
/// its own and whose state is a runes module, a greeting, and a module no component imports.
fn app(tests: &str) -> Vec<(&'static str, String)> {
    vec![
        ("hushrun.yaml", yaml(tests)),
        (".gitignore", "node_modules\n".into()),
        (
            "package.json",
            "{\"name\": \"app\", \"private\": true, \"type\": \"module\"}\n".into(),
        ),
        (
            "vite.config.js",
            format!(
                "import {{ defineConfig }} from 'vitest/config'\nimport {{ svelte }} from '@sveltejs/vite-plugin-svelte'\nimport {{ svelteTesting }} from '@testing-library/svelte/vite'\nimport {{ searchForWorkspaceRoot }} from 'vite'\nimport {{ realpathSync }} from 'node:fs'\n\nexport default defineConfig({{\n  plugins: [svelte(), svelteTesting(), {LINE}],\n  // The fixture's node_modules is a link to a directory outside it.\n  server: {{ fs: {{ allow: [searchForWorkspaceRoot(process.cwd()), realpathSync('node_modules')] }} }},\n  test: {{ environment: 'jsdom' }},\n}})\n"
            ),
        ),
        (
            "src/lib/counter.svelte.js",
            "export function createCounter(start = 0) {\n  let count = $state(start)\n  return { get count() { return count }, increment() { count += 1 } }\n}\n".into(),
        ),
        (
            "src/lib/Label.svelte",
            "<script>\n  let { text } = $props()\n</script>\n<span>{text}!</span>\n".into(),
        ),
        (
            "src/lib/Counter.svelte",
            "<script>\n  import { createCounter } from './counter.svelte.js'\n  import Label from './Label.svelte'\n  const counter = createCounter(1)\n</script>\n<button onclick={counter.increment}><Label text={`count ${counter.count}`} /></button>\n".into(),
        ),
        (
            "src/lib/Hello.svelte",
            "<script>\n  let { name } = $props()\n</script>\n<h1>Hello {name}</h1>\n<style>h1 { color: red; }</style>\n".into(),
        ),
        ("src/lib/math.js", "export const add = (a, b) => a + b\n".into()),
        (
            "src/counter.test.js",
            "import { render, screen, fireEvent } from '@testing-library/svelte'\nimport { test, expect } from 'vitest'\nimport Counter from './lib/Counter.svelte'\n\ntest('counts', async () => {\n  render(Counter)\n  const b = screen.getByRole('button')\n  expect(b.textContent).toBe('count 1!')\n  await fireEvent.click(b)\n  expect(b.textContent).toBe('count 2!')\n})\n".into(),
        ),
        (
            "src/both.test.js",
            "import { render, screen } from '@testing-library/svelte'\nimport { test, expect } from 'vitest'\nimport Counter from './lib/Counter.svelte'\nimport Hello from './lib/Hello.svelte'\n\ntest('counter', () => { render(Counter); expect(screen.getByRole('button').textContent).toBe('count 1!') })\ntest('hello', () => { render(Hello, { name: 'y' }); expect(screen.getByRole('heading').textContent).toBe('Hello y') })\n".into(),
        ),
        (
            "src/runes.test.js",
            "import { test, expect } from 'vitest'\nimport { createCounter } from './lib/counter.svelte.js'\n\ntest('runes', () => { const c = createCounter(5); c.increment(); expect(c.count).toBe(6) })\n".into(),
        ),
        (
            "src/math.test.js",
            "import { test, expect } from 'vitest'\nimport { add } from './lib/math.js'\n\ntest('adds', () => { expect(add(1, 2)).toBe(3) })\n".into(),
        ),
    ]
}

fn repo(files: &[(&'static str, String)]) -> Repo {
    let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    Repo::new(&files)
}

// covers: C-62, C-63
#[test]
fn a_svelte_test_file_reads_the_components_and_runes_modules_it_ran() {
    if !tools() {
        return;
    }
    let r = repo(&app("observe"));
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // jsdom's worker loaded hushrun's setup file, which Vite serves from outside the workspace,
    // and every test file was followed: vite-plugin-svelte's plugins are known.
    let (reads, not_followed) = r.reads("src/counter.test.js");
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
    let (reads, _) = r.reads("src/runes.test.js");
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
    let second = r.run(VITEST_RUN);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.says("hushrun: unit: with `tests: reuse`, 2 of 4 test files would have run; 2 already passed on the files they read"),
        "{}",
        second.stderr
    );
    // A runes module's, those that ran it.
    r.write(
        "src/lib/Label.svelte",
        "<script>\n  let { text } = $props()\n</script>\n<span>{text}!</span>\n",
    );
    let third = r.run(VITEST_RUN);
    assert_eq!(third.code, 0, "{}\n{}", third.stdout, third.stderr);
    r.write(
        "src/lib/counter.svelte.js",
        "export function createCounter(start = 0) {\n  let count = $state(start)\n  return { get count() { return count }, increment() { count += 2 } }\n}\n",
    );
    let fourth = r.run(VITEST_RUN);
    assert_eq!(fourth.code, 1, "{}\n{}", fourth.stdout, fourth.stderr);
    assert!(
        fourth.says("hushrun: unit: with `tests: reuse`, 3 of 4 test files would have run; 1 already passed on the files it read"),
        "{}",
        fourth.stderr
    );
}

// covers: C-62, C-63
#[test]
fn a_svelte_test_reads_the_components_it_rendered_and_no_other() {
    if !tools() {
        return;
    }
    let r = repo(&app("{mode: observe, unit: test, reads: test}"));
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // Label is two imports away from the test file, and only Counter's render runs it: `hello`,
    // in the same file, ran no function of it (C-40 still gives every test of a file what the
    // file imports directly).
    r.write(
        "src/lib/Label.svelte",
        "<script>\n  let { text } = $props()\n</script>\n<span>{text}?</span>\n",
    );
    let second = r.run(VITEST_RUN);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.says("hushrun: unit: with `tests: reuse`, 2 of 5 tests would have run, in 2 of 4 test files; 3 already passed on the files they read"),
        "{}",
        second.stderr
    );
    let why = r.run(&["why", "unit", "src/both.test.js::hello"]);
    assert!(
        why.stdout
            .starts_with("src/both.test.js::hello would be reused with `tests: reuse`"),
        "{}\n{}",
        why.stdout,
        why.stderr
    );
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
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // The file the preprocessor included, which Vite's process read, not the worker; and the
    // config with the file it imports, which every test file reads.
    let (reads, not_followed) = r.reads("src/page.test.js");
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
    let (reads, _) = r.reads("src/math.test.js");
    assert!(reads.contains(&"tools/include.js".to_string()), "{reads:?}");
    r.write("src/partials/Footer.svelte", "<p>footer words</p>\n");
    let second = r.run(VITEST_RUN);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.says("hushrun: unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read"),
        "{}",
        second.stderr
    );
}

// covers: C-64
#[test]
fn what_a_svelte_preprocessor_read_is_read_by_the_test_that_rendered_it() {
    if !tools() {
        return;
    }
    let r = repo(&preprocessed("{mode: observe, unit: test, reads: test}"));
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    r.write("src/partials/Footer.svelte", "<p>footer words</p>\n");
    let second = r.run(VITEST_RUN);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.says("hushrun: unit: with `tests: reuse`, 1 of 6 tests would have run, in 1 of 5 test files; 5 already passed on the files they read"),
        "{}",
        second.stderr
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
    let first = r.run(VITEST_RUN);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    // `$app/environment` and `$env/static/public` are made from the config and `.env`, which
    // every test file reads (`.env` as the rest set), not from no one file.
    let (reads, not_followed) = r.reads("src/where.test.js");
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
    let second = r.run(VITEST_RUN);
    assert_eq!(second.code, 1, "{}\n{}", second.stdout, second.stderr);
    assert!(
        second.says("hushrun: unit: with `tests: reuse`, 1 of 2 test files would have run; 1 already passed on the files it read"),
        "{}",
        second.stderr
    );
    // An env file changes what every test may read.
    r.write("src/lib/two.js", "export const two = () => 2\n");
    assert_eq!(r.run(VITEST_RUN).code, 0);
    r.write(".env", "PUBLIC_GREETING=ho\n");
    let third = r.run(VITEST_RUN);
    assert_eq!(third.code, 1, "{}\n{}", third.stdout, third.stderr);
    assert!(
        third.says("hushrun: unit: with `tests: reuse`, all 2 test files would have run: .env changed, and any test may read it"),
        "{}",
        third.stderr
    );
}
