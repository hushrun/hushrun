//! \[ADR 0109\] The Svelte repositories and tools the tests of Svelte with Vitest run, in-process
//! (`units_svelte.rs`) and end to end (`e2e_svelte.rs`). See `units_svelte.rs` for the tools.

#![allow(dead_code)]

use std::path::PathBuf;

use serde_json::Value;

use super::repo::Repo;

/// The Vitest these tests are written against, with the Svelte it compiles.
pub const VITEST: &str = "4.1.12";
pub const SVELTE: &str = "5.59.1";
pub const LINE: &str = "process.env.HUSHRUN_VITEST ? import(process.env.HUSHRUN_VITEST).then((m) => m.default()) : undefined";

/// The `node_modules` with the pinned Vitest and Svelte, when this machine has it.
pub fn svelte_modules() -> Option<PathBuf> {
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

pub fn has_svelte() -> bool {
    svelte_modules().is_some()
        && std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

pub fn tools() -> bool {
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

/// A repository of `files` whose COMMAND runs the pinned Vitest and Svelte: as installed outside
/// the repository, which git ignores, and with none of a person's Node or Vitest settings.
pub fn repo(files: &[(&'static str, String)]) -> Repo {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.clone()))
        .collect();
    let r = Repo::new(&files).clear(&[
        "HUSHRUN_VITEST",
        "NODE_OPTIONS",
        "VITEST",
        "VITEST_POOL_ID",
        "VITEST_WORKER_ID",
    ]);
    let modules = svelte_modules().expect("svelte");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&modules, r.repo.join("node_modules")).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&modules, r.repo.join("node_modules")).expect("link");
    r
}

/// What the check's map says `unit` read, and what wasn't followed of it.
pub fn read_by(r: &Repo, unit: &str) -> (Vec<String>, Value) {
    let map = r.map();
    let reads = map["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    (reads, map["units"][unit]["notFollowed"].clone())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A Svelte 5 app tested with Testing Library in jsdom: a counter whose label is a component of
/// its own and whose state is a runes module, a greeting, and a module no component imports.
pub fn app(tests: &str) -> Vec<(&'static str, String)> {
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
