//! \[ADR 0071\] The Vitest repositories and tools the tests of Vitest's adapter run, in-process
//! (`units_vitest.rs`) and end to end (`e2e_vitest.rs`). See `units_vitest.rs` for the tools.

#![allow(dead_code)]

use std::path::PathBuf;

use serde_json::Value;

use super::repo::Repo;

/// The Vitest these tests are written against.
pub const VITEST: &str = "4.1.12";

/// The line a config loads hushrun's adapter with ([`hushrun::units::vitest::CONFIG_LINE`]).
pub const LINE: &str = "plugins: [process.env.HUSHRUN_VITEST ? import(process.env.HUSHRUN_VITEST).then((m) => m.default()) : undefined],";

/// The `node_modules` with the pinned Vitest, when this machine has it.
pub fn vitest_modules() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HUSHRUN_TEST_VITEST")?);
    let package = std::fs::read(dir.join("vitest/package.json")).ok()?;
    let package: Value = serde_json::from_slice(&package).ok()?;
    (package["version"] == VITEST).then_some(dir)
}

pub fn has_vitest() -> bool {
    vitest_modules().is_some()
        && std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Whether a tool the tests need is `here`: a test without it says so and passes, but under
/// `HUSHRUN_TEST_TOOLS=required`, which CI sets, where it fails.
pub fn tools(what: &str, here: bool) -> bool {
    if here {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

/// Whether Node and the pinned Vitest are here, as [`tools`] asks it.
pub fn have_vitest() -> bool {
    tools(&format!("Node and Vitest {VITEST}"), has_vitest())
}

/// A repository of `files` whose COMMAND runs the pinned Vitest: as installed outside the
/// repository, which git ignores, and with none of a person's Node or Vitest settings.
pub fn repo(files: &[(String, String)]) -> Repo {
    let r = Repo::new(files).clear(&[
        "HUSHRUN_VITEST",
        "NODE_OPTIONS",
        "VITEST",
        "VITEST_POOL_ID",
        "VITEST_WORKER_ID",
    ]);
    let modules = vitest_modules().expect("vitest");
    r.link_dir(&modules, "node_modules");
    r
}

/// What the store's map says test file `unit` read.
pub fn reads(r: &Repo, unit: &str) -> Vec<String> {
    let map = r.map();
    map["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// Text without its terminal colors.
pub fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

pub fn config(more: &str) -> String {
    format!(
        "import {{ defineConfig }} from 'vitest/config'\n\nexport default defineConfig({{\n  {LINE}\n{more}}})\n"
    )
}

/// A shop: a cart that imports pricing, each with a test file; a greeting; and five test files
/// that read only the greeting, so that a plan has something to reuse.
pub fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files = vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
        file(
            "src/pricing.ts",
            "export const price = (x: number) => x * 2\n",
        ),
        file(
            "src/cart.ts",
            "import { price } from './pricing'\n\nexport const total = (xs: number[]) => xs.reduce((n, x) => n + price(x), 0)\n",
        ),
        file("src/greet.ts", "export const hello = () => 'hi'\n"),
        file(
            "test/cart.test.ts",
            "import { test, expect } from 'vitest'\nimport { total } from '../src/cart'\n\ntest('total', () => { expect(total([1, 2])).toBe(6) })\n",
        ),
        file(
            "test/pricing.test.ts",
            "import { test, expect } from 'vitest'\nimport { price } from '../src/pricing'\n\ntest('price', () => { expect(price(3)).toBe(6) })\n",
        ),
    ];
    for n in 1..=5 {
        files.push(file(
            &format!("test/greet{n}.test.ts"),
            "import { test, expect } from 'vitest'\nimport { hello } from '../src/greet'\n\ntest('hello', () => { expect(hello()).toBe('hi') })\n",
        ));
    }
    files
}

/// A shop with tests one by one: a cart test file with a test that fails while FAIL is set, and
/// greet test files that pass.
pub fn told_shop(tests: &str) -> Vec<(String, String)> {
    let mut files = shop(tests);
    files.retain(|(p, _)| p != "test/cart.test.ts");
    files.push(file(
        "test/cart.test.ts",
        "import { describe, test, expect } from 'vitest'\nimport { total } from '../src/cart'\n\ntest('total', () => { expect(total([1, 2])).toBe(6) })\ntest('flips', () => { expect(process.env.FAIL).toBeUndefined() })\ndescribe('more', () => { test('three', () => { expect(total([1])).toBe(2) }) })\n",
    ));
    files
}

/// A repository of one test file whose two tests fail while FAIL, or HIDDEN, is set: HIDDEN is
/// what no file says, as a missed dependency would be.
pub fn two_tests() -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
        file(
            "test/two.test.ts",
            "import { test, expect } from 'vitest'\ntest('ok', () => { expect(process.env.HIDDEN).toBeUndefined() })\ntest('flips', () => { expect(process.env.FAIL).toBeUndefined() })\n",
        ),
    ]
}

/// Two test files, one of whose tests leaves a timer that throws after it: Vitest reports an
/// error no test owns, and fails the run.
pub fn late_timer(tests: &str) -> Vec<(String, String)> {
    vec![
        file("hushrun.yaml", &yaml(tests)),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
        file(
            "test/a.test.ts",
            "import { test } from 'vitest'\ntest('a1', () => {})\n",
        ),
        file(
            "test/bad.test.ts",
            "import { test } from 'vitest'\ntest('leaks', () => { setTimeout(() => { throw new Error('late timer') }, 10) })\ntest('waits', async () => { await new Promise((r) => setTimeout(r, 100)) })\n",
        ),
    ]
}
