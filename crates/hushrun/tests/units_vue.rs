//! \[ADR 0108\] Vue's single-file components under Vitest and Jest (C-58 to C-61): a component is a
//! source, read by the tests whose worker ran it; what its compile read besides itself (a type
//! imported for `defineProps<Props>()`) is read with it; hushrun's setup file loads in a browser's
//! environment (`jsdom`); and reuse never misses a failure.
//!
//! The fixtures need Node and two `node_modules` installed from npm outside the repository:
//! `HUSHRUN_TEST_VUE` with Vitest 4.1.12, Vue, @vitejs/plugin-vue, @vue/test-utils and jsdom (CI:
//! `npm install --prefix <dir> --legacy-peer-deps vitest@4.1.12 vue@3.5.46 @vitejs/plugin-vue@6.0.11
//! @vue/test-utils@2.6.0 jsdom@26.1.0 typescript@5.9.3`), and `HUSHRUN_TEST_VUE_JEST` with Jest 29
//! and @vue/vue3-jest (`jest@29.7.0 jest-environment-jsdom@29.7.0 @vue/vue3-jest@29.2.6
//! babel-jest@29.7.0 @babel/core@7.29.13 @babel/preset-env@7.29.14 vue@3.5.46 @vue/test-utils@2.6.0
//! typescript@5.9.3`). Each says so and passes where its own is missing, unless
//! `HUSHRUN_TEST_TOOLS=required`.

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_testkit::git;
use serde_json::Value;

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
    "HUSHRUN_JEST",
    "HUSHRUN_JEST_RUN",
    "HUSHRUN_INVOKED_BY",
    "HUSHRUN_REPORTS_READ",
    "NODE_OPTIONS",
    "VITEST",
    "VITEST_POOL_ID",
    "VITEST_WORKER_ID",
    "JEST_WORKER_ID",
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

/// The `node_modules` a variable names, when it holds `package` at `version`.
fn modules(variable: &str, package: &str, version: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(variable)?);
    let text = std::fs::read(dir.join(package).join("package.json")).ok()?;
    let json: Value = serde_json::from_slice(&text).ok()?;
    (json["version"] == version).then_some(dir)
}

fn vitest_modules() -> Option<PathBuf> {
    modules("HUSHRUN_TEST_VUE", "vitest", "4.1.12")
        .filter(|d| d.join("@vitejs/plugin-vue").is_dir() && d.join("jsdom").is_dir())
}

fn jest_modules() -> Option<PathBuf> {
    modules("HUSHRUN_TEST_VUE_JEST", "jest", "29.7.0").filter(|d| d.join("@vue/vue3-jest").is_dir())
}

fn tools(what: &str, here: Option<PathBuf>) -> Option<PathBuf> {
    let node = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if node && here.is_some() {
        return here;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    None
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
    fn observe_line(&self) -> String {
        let line = self
            .stderr
            .lines()
            .find(|l| l.contains("with `tests: reuse`"))
            .unwrap_or_else(|| panic!("no observe line in:\n{}", self.stderr));
        hushrun_testkit::lines::without_saving(line)
    }
}

impl Repo {
    fn new(files: &[(&str, &str)], modules: &Path) -> Repo {
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
        #[cfg(unix)]
        std::os::unix::fs::symlink(modules, r.repo.join("node_modules")).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(modules, r.repo.join("node_modules")).expect("link");
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

    /// A run that must pass, and what it said would have been reused.
    fn passes(&self, args: &[&str]) -> String {
        let ran = self.run(args);
        assert_eq!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
        ran.observe_line()
    }
}

fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

const VITEST_CONFIG: &str = "import { defineConfig } from 'vitest/config'\nimport vue from '@vitejs/plugin-vue'\n\nexport default defineConfig({\n  plugins: [vue(), process.env.HUSHRUN_VITEST ? import(process.env.HUSHRUN_VITEST).then((m) => m.default()) : undefined],\n  test: { environment: 'jsdom' },\n})\n";

const VITEST_RUN: &[&str] = &["run", "unit", "--", "node_modules/.bin/vitest", "run"];

const COUNTER: &str = "<script setup lang=\"ts\">\nimport { ref } from 'vue'\nimport { label } from './format'\nconst n = ref(0)\n</script>\n\n<template>\n  <button @click=\"n++\">{{ label(n) }}</button>\n</template>\n";

const FLAG: &str = "<script setup lang=\"ts\">\nimport type { FlagProps } from './types'\nconst props = defineProps<FlagProps>()\n</script>\n\n<template><b>{{ props.on === true ? 'yes' : 'no' }}</b></template>\n";

const BADGE: &str = "<script setup>\nconst props = defineProps({ text: String })\n</script>\n\n<template>\n  <span :class=\"$style.badge\">{{ props.text }}</span>\n</template>\n\n<style module>\n.badge { color: red; }\n</style>\n";

/// A shop of components: a counter whose `<script setup>` imports a function, a flag whose props
/// come from an imported type, a price whose script and template are `src=` files, a badge with
/// a CSS module, a component of static text; and a test file for each.
fn shop(tests: &str, config: (&str, &str), test_import: &str) -> Vec<(String, String)> {
    let t = |_name: &str, body: &str| {
        format!("{test_import}import {{ mount }} from '@vue/test-utils'\n{body}")
    };
    let files: Vec<(&str, String)> = vec![
        ("hushrun.yaml", yaml(tests)),
        (".gitignore", "node_modules\n".into()),
        (config.0, config.1.into()),
        ("src/format.ts", "export const label = (n: number) => `count: ${n}`\n".into()),
        ("src/Counter.vue", COUNTER.into()),
        ("src/types.ts", "export interface FlagProps { on?: boolean }\n".into()),
        ("src/Flag.vue", FLAG.into()),
        (
            "src/price-logic.ts",
            "import { defineComponent } from 'vue'\nexport default defineComponent({ props: { cents: { type: Number, required: true } }, computed: { euros() { return (this.cents / 100).toFixed(2) } } })\n".into(),
        ),
        ("src/price.html", "<span>{{ euros }} EUR</span>\n".into()),
        (
            "src/Price.vue",
            "<script src=\"./price-logic.ts\" lang=\"ts\"></script>\n<template src=\"./price.html\"></template>\n".into(),
        ),
        ("src/Badge.vue", BADGE.into()),
        ("src/Plain.vue", "<template>\n  <p>static text</p>\n</template>\n".into()),
        (
            "test/counter.test.ts",
            t("counter", "import Counter from '../src/Counter.vue'\n\ntest('counts', async () => {\n  const w = mount(Counter)\n  await w.find('button').trigger('click')\n  expect(w.text()).toBe('count: 1')\n})\ntest('plain', () => {})\n"),
        ),
        (
            "test/flag.test.ts",
            t("flag", "import Flag from '../src/Flag.vue'\n\ntest('a present attribute is true', () => {\n  expect(mount(Flag, { attrs: { on: '' } }).text()).toBe('yes')\n})\ntest('plain', () => {})\n"),
        ),
        (
            "test/price.test.ts",
            t("price", "import Price from '../src/Price.vue'\n\ntest('euros', () => {\n  expect(mount(Price, { props: { cents: 250 } }).text()).toBe('2.50 EUR')\n})\ntest('plain', () => {})\n"),
        ),
        (
            "test/badge.test.ts",
            t("badge", "import Badge from '../src/Badge.vue'\n\ntest('badge', () => {\n  const w = mount(Badge, { props: { text: 'hi' } })\n  expect(w.text()).toBe('hi')\n  expect(w.find('span').classes().length).toBe(1)\n})\n"),
        ),
        (
            "test/plain.test.ts",
            t("plain", "import Plain from '../src/Plain.vue'\n\ntest('plain', () => { expect(mount(Plain).text()).toBe('static text') })\n"),
        ),
    ];
    files.into_iter().map(|(p, t)| (p.to_string(), t)).collect()
}

fn borrowed(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect()
}

fn vitest_shop(tests: &str, modules: &Path) -> Repo {
    let mut files = shop(
        tests,
        ("vitest.config.ts", VITEST_CONFIG),
        "import { test, expect } from 'vitest'\n",
    );
    files.push((
        "package.json".into(),
        "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n".into(),
    ));
    Repo::new(&borrowed(&files), modules)
}

/// After a first run, each change, alone, runs what reads it and leaves the rest reused.
fn changes(r: &Repo, run: &[&str], cases: &[(&str, &str, &str, &str)]) {
    for (path, from, to, expected) in cases {
        let before = std::fs::read_to_string(r.repo.join(path)).expect("read");
        assert!(before.contains(from), "{path} holds {from:?}");
        r.write(path, &before.replacen(from, to, 1));
        let ran = r.run(run);
        assert_eq!(
            ran.observe_line(),
            *expected,
            "after {path}: {from:?} -> {to:?}\n{}\n{}",
            ran.stdout,
            ran.stderr
        );
        r.write(path, &before);
    }
}

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
        r.passes(VITEST_RUN),
        "hushrun: unit: with `tests: reuse`, all 5 test files would have run: none had passed here yet on the files it read"
    );
    changes(
        &r,
        VITEST_RUN,
        &[
            // A component, its `<script setup>`'s import, a `src=` script: their test file alone.
            (
                "src/Plain.vue",
                "static text",
                "static words",
                "hushrun: unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read",
            ),
            (
                "src/format.ts",
                "count: ",
                "n: ",
                "hushrun: unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read",
            ),
            (
                "src/price-logic.ts",
                "toFixed(2)",
                "toFixed(1)",
                "hushrun: unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read",
            ),
            // [C-59] A type a component's props came from: no code of it runs, its test file runs.
            (
                "src/types.ts",
                "on?: boolean",
                "on?: string",
                "hushrun: unit: with `tests: reuse`, 1 of 5 test files would have run; 4 already passed on the files they read",
            ),
            // A `src=` template is HTML, of the rest set: every test file runs.
            (
                "src/price.html",
                "EUR",
                "USD",
                "hushrun: unit: with `tests: reuse`, all 5 test files would have run: src/price.html changed, and any test may read it",
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
    r.passes(VITEST_RUN);
    // The type makes the attribute a string: the test that mounted it fails, and it would have
    // run, with the other test of its file that imports the component.
    r.write(
        "src/types.ts",
        "export interface FlagProps { on?: string }\n",
    );
    let ran = r.run(VITEST_RUN);
    assert_eq!(ran.code, 1, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains("a present attribute is true"),
        "{}",
        ran.stdout
    );
    assert!(
        !ran.stderr
            .contains("though it passed before on the same files it read"),
        "{}",
        ran.stderr
    );
    assert_eq!(
        ran.observe_line(),
        "hushrun: unit: with `tests: reuse`, 2 of 8 tests would have run, in 1 of 5 test files; 6 already passed on the files they read"
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
    r.run(VITEST_RUN);
    r.write(
        "src/Plain.vue",
        "<template>\n  <p>static words</p>\n</template>\n",
    );
    let ran = r.run(VITEST_RUN);
    assert_eq!(ran.code, 1, "{}\n{}", ran.stdout, ran.stderr);
    // The changed component's test file runs, and fails; one that passed runs as a check (C-34).
    let plan = ran
        .stderr
        .lines()
        .find(|l| l.contains("test files run"))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", ran.stderr));
    assert_eq!(
        plan,
        "hushrun: unit: 2 of 5 test files run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(ran.stdout.contains("test/plain.test.ts"), "{}", ran.stdout);
    assert!(
        hushrun_testkit::lines::says_reused(&ran.stderr, 3, "test files"),
        "{}",
        ran.stderr
    );
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
    let r = Repo::new(&borrowed(&files), &modules);
    // Jest's cache holds every component before hushrun first sees them: vue3-jest then reads no
    // `src=` file, and hushrun reads it from the component.
    let mut warm = Command::new("node");
    warm.current_dir(&r.repo)
        .args(["node_modules/jest/bin/jest.js", "--ci"]);
    for var in CLEARED {
        warm.env_remove(var);
    }
    let warmed = warm.output().expect("jest");
    assert!(
        warmed.status.success(),
        "{}",
        String::from_utf8_lossy(&warmed.stderr)
    );
    assert_eq!(
        r.passes(JEST_RUN),
        "hushrun: unit: with `tests: reuse`, all 4 test files would have run: none had passed here yet on the files it read"
    );
    changes(
        &r,
        JEST_RUN,
        &[
            (
                "src/Plain.vue",
                "static text",
                "static words",
                "hushrun: unit: with `tests: reuse`, 1 of 4 test files would have run; 3 already passed on the files they read",
            ),
            (
                "src/Badge.vue",
                "color: red",
                "color: blue",
                "hushrun: unit: with `tests: reuse`, 1 of 4 test files would have run; 3 already passed on the files they read",
            ),
            // [C-61] A `src=` script, which vue3-jest reads only when Jest's cache doesn't hold the
            // component (and whose change that cache doesn't see): read with the component.
            (
                "src/price-logic.js",
                "toFixed(2)",
                "toFixed(1)",
                "hushrun: unit: with `tests: reuse`, 1 of 4 test files would have run; 3 already passed on the files they read",
            ),
        ],
    );
}
