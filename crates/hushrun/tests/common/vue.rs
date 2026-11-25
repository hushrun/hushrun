//! \[ADR 0108\] The Vue repositories and tools the tests of Vue's components run, in-process
//! (`units_vue.rs`) and end to end (`e2e_vue.rs`). See `units_vue.rs` for the tools.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::repo::Repo;

/// The `node_modules` a variable names, when it holds `package` at `version`.
pub fn modules(variable: &str, package: &str, version: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(variable)?);
    let text = std::fs::read(dir.join(package).join("package.json")).ok()?;
    let json: Value = serde_json::from_slice(&text).ok()?;
    (json["version"] == version).then_some(dir)
}

pub fn vitest_modules() -> Option<PathBuf> {
    modules("HUSHRUN_TEST_VUE", "vitest", "4.1.12")
        .filter(|d| d.join("@vitejs/plugin-vue").is_dir() && d.join("jsdom").is_dir())
}

pub fn jest_modules() -> Option<PathBuf> {
    modules("HUSHRUN_TEST_VUE_JEST", "jest", "29.7.0").filter(|d| d.join("@vue/vue3-jest").is_dir())
}

pub fn tools(what: &str, here: Option<PathBuf>) -> Option<PathBuf> {
    let node = std::process::Command::new("node")
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

/// A repository of `files` whose COMMAND runs the tools in `modules`: as installed outside the
/// repository, which git ignores, and with none of a person's Node, Vitest or Jest settings.
pub fn repo(files: &[(&str, &str)], modules: &Path) -> Repo {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    let r = Repo::new(&files).clear(&[
        "HUSHRUN_JEST",
        "HUSHRUN_JEST_RUN",
        "HUSHRUN_VITEST",
        "JEST_WORKER_ID",
        "NODE_OPTIONS",
        "VITEST",
        "VITEST_POOL_ID",
        "VITEST_WORKER_ID",
    ]);
    r.link_dir(modules, "node_modules");
    r
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

pub const VITEST_CONFIG: &str = "import { defineConfig } from 'vitest/config'\nimport vue from '@vitejs/plugin-vue'\n\nexport default defineConfig({\n  plugins: [vue(), process.env.HUSHRUN_VITEST ? import(process.env.HUSHRUN_VITEST).then((m) => m.default()) : undefined],\n  test: { environment: 'jsdom' },\n})\n";

pub const COUNTER: &str = "<script setup lang=\"ts\">\nimport { ref } from 'vue'\nimport { label } from './format'\nconst n = ref(0)\n</script>\n\n<template>\n  <button @click=\"n++\">{{ label(n) }}</button>\n</template>\n";

pub const FLAG: &str = "<script setup lang=\"ts\">\nimport type { FlagProps } from './types'\nconst props = defineProps<FlagProps>()\n</script>\n\n<template><b>{{ props.on === true ? 'yes' : 'no' }}</b></template>\n";

pub const BADGE: &str = "<script setup>\nconst props = defineProps({ text: String })\n</script>\n\n<template>\n  <span :class=\"$style.badge\">{{ props.text }}</span>\n</template>\n\n<style module>\n.badge { color: red; }\n</style>\n";

/// A shop of components: a counter whose `<script setup>` imports a function, a flag whose props
/// come from an imported type, a price whose script and template are `src=` files, a badge with
/// a CSS module, a component of static text; and a test file for each.
pub fn shop(tests: &str, config: (&str, &str), test_import: &str) -> Vec<(String, String)> {
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

pub fn borrowed(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect()
}

pub fn vitest_shop(tests: &str, modules: &Path) -> Repo {
    let mut files = shop(
        tests,
        ("vitest.config.ts", VITEST_CONFIG),
        "import { test, expect } from 'vitest'\n",
    );
    files.push((
        "package.json".into(),
        "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n".into(),
    ));
    repo(&borrowed(&files), modules)
}
