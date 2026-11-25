//! \[ADR 0071\] Vitest's adapter end to end: the `hushrun` binary, as a person or CI runs it, for
//! what only the CLI does with Vitest's test files. Every other test of the adapter is in-process,
//! in `units_vitest.rs`.
//!
//! This file is part of the end-to-end set (`tests/e2e_*.rs`, CONTRIBUTING.md, "Which tests CI
//! runs again"): each test starts the binary, so it runs again whenever anything in hushrun
//! changes. A test belongs here only for what no in-process test can see: the run's lines as the
//! CLI says them, its last line, the check's own result and skip, the audit's second start of
//! COMMAND and its suspension, `hushrun why` and `hushrun init`. The tools are those of
//! `units_vitest.rs`.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/vitest.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use serde_json::json;

use cli::{Cli as _, Output};
use fixtures::*;

const VITEST_RUN: &[&str] = &["run", "unit", "--", "node_modules/.bin/vitest", "run"];

/// The plan's line, in Vitest's word.
fn plan_line(ran: &Output) -> &str {
    ran.stderr
        .lines()
        .find(|l| l.contains("test files run") || l.contains("every test file runs"))
        .unwrap_or_else(|| panic!("no plan line in:\n{}", ran.stderr))
}

// covers: C-34, C-35
#[test]
fn a_run_says_what_it_reuses_and_hushrun_why_follows_vites_imports() {
    if !have_vitest() {
        return;
    }
    let r = repo(&shop("reuse"));
    let first = r.run(VITEST_RUN, &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    assert_eq!(
        plan_line(&first),
        "hushrun: unit: every test file runs: no run here has listed Vitest's test files yet"
    );
    r.write(
        "src/pricing.ts",
        "export const price = (x: number) => x + x\n",
    );
    let second = r.run(VITEST_RUN, &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        plan_line(&second),
        "hushrun: unit: 3 of 7 test files run; 4 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
    );
    assert!(
        plain(&second.stdout).contains("Test Files  3 passed (3)"),
        "{}",
        second.stdout
    );
    assert!(
        hushrun_testkit::lines::says_reused(&second.stderr, 4, "test files"),
        "{}",
        second.stderr
    );
    // \[ADR 0073\] Why, with the imports Vite's module graph saw.
    r.write(
        "src/pricing.ts",
        "export const price = (x: number) => 2 * x\n",
    );
    let why = r.run(&["why", "unit", "test/cart.test.ts"], &[]);
    assert_eq!(why.code, 0, "{}", why.stderr);
    assert!(
        why.stdout.starts_with(
            "test/cart.test.ts runs: it read src/pricing.ts, which changed\n  test/cart.test.ts imports src/cart.ts imports src/pricing.ts\n  as far as this machine knows: Vitest's list"
        ),
        "{}",
        why.stdout
    );
}

// covers: C-32
#[test]
fn the_cli_says_why_vitest_is_left_alone_before_command_runs_as_it_is() {
    if !have_vitest() {
        return;
    }
    let mut files = shop("observe");
    files.retain(|(p, _)| p != "vitest.config.ts");
    files.push(file("vite.config.ts", "export default { test: {} }\n"));
    let r = repo(&files);
    let ran = r.run(VITEST_RUN, &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        ran.stderr.contains("hushrun: unit: test files aren't observed: Vitest loads hushrun's adapter from one line in vite.config.ts, which `hushrun init` proposes: `plugins: [process.env.HUSHRUN_VITEST ? import(process.env.HUSHRUN_VITEST).then((m) => m.default()) : undefined]`"),
        "{}",
        ran.stderr
    );
    let watching = r.run(
        &[
            "run",
            "unit",
            "--",
            "node_modules/.bin/vitest",
            "--watch=false",
        ],
        &[],
    );
    assert!(
        watching
            .stderr
            .contains("test files aren't observed: vitest without run"),
        "{}",
        watching.stderr
    );
}

// covers: C-43, C-39
#[test]
fn with_tests_as_the_unit_the_result_counts_each_test_and_the_same_code_skips() {
    if !have_vitest() {
        return;
    }
    let r = repo(&told_shop("{mode: reuse, unit: test}"));
    let first = r.run(VITEST_RUN, &[("FAIL", "1")]);
    assert_eq!(first.code, 1, "{}\n{}", first.stdout, first.stderr);
    let map = r.map();
    assert_eq!(
        map["units"]["test/cart.test.ts"]["tests"],
        json!({"flips": "failed", "more > three": "passed", "total": "passed"}),
        "{map:#}"
    );
    // The same files, the flip fixed: cart's own key failed, so its tests are asked one by one.
    // Of those that passed, one runs as a check, and of the files that passed whole, one.
    let second = r.run(VITEST_RUN, &[]);
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    assert_eq!(
        hushrun_testkit::lines::drawn(
            second
                .stderr
                .lines()
                .find(|l| l.contains(" tests run, in "))
                .unwrap_or_else(|| panic!("{}", second.stderr))
        ),
        hushrun_testkit::lines::drawn(
            "hushrun: unit: 2 of 9 tests run, in 1 of 7 test files; 7 already passed on the files they read, and 1 more that already passed runs anyway, as a check"
        )
    );
    // Vitest ran flips and the audited test, wherever the draw found it, and left out the reused
    // tests: skipped in a file it loaded, or never loaded with a file reused whole.
    let stdout = plain(&second.stdout);
    assert!(stdout.contains("Tests  2 passed"), "{}", second.stdout);
    let result = r.newest_result();
    assert_eq!(result["result"], "pass");
    // \[C-39\] The tests Vitest told, those of the two files it ran, and those this machine knows
    // of the files reused whole: as the line counts them.
    assert_eq!(
        result["units"]["tests"],
        json!({"total": 9, "ran": 2, "reused": 7})
    );
    // A reused test keeps the row it stands on; flips passed now.
    let map = r.map();
    assert_eq!(
        map["units"]["test/cart.test.ts"]["tests"],
        json!({"flips": "passed", "more > three": "passed", "total": "passed"}),
        "{map:#}"
    );
    // The same code again: the check skips whole.
    let again = r.run(VITEST_RUN, &[]);
    assert!(again.stderr.contains("unit skipped"), "{}", again.stderr);
}

// covers: C-34, C-35
#[test]
fn an_injected_missed_dependency_in_vitest_is_caught_by_an_audit() {
    if !have_vitest() {
        return;
    }
    // A test file that fails when a file outside the check's files exists, named by a variable:
    // what no adapter sees. With one other test file, it is the one an audit runs.
    let files = vec![
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\"}\n",
        ),
        file("vitest.config.ts", &config("")),
        file("src/other.ts", "export const other = 1\n"),
        file(
            "test/hidden.test.ts",
            "import { test, expect } from 'vitest'\nimport { lstatSync } from 'node:fs'\n\ntest('hidden', () => {\n  let there = true\n  try { lstatSync(process.env.FLAG_FILE ?? '') } catch { there = false }\n  expect(there).toBe(false)\n})\n",
        ),
        file(
            "test/other.test.ts",
            "import { test, expect } from 'vitest'\nimport { other } from '../src/other'\n\ntest('other', () => { expect(other).toBe(1) })\n",
        ),
    ];
    let r = repo(&files);
    let flag = r.root.join("flag");
    let flag = flag.to_str().unwrap().to_string();
    let env = [("FLAG_FILE", flag.as_str())];
    assert_eq!(r.run(VITEST_RUN, &env).code, 0);
    std::fs::write(&flag, "").unwrap();
    r.write("src/other.ts", "export const other = 2 - 1\n");
    let caught = r.run(VITEST_RUN, &env);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert_eq!(
        plan_line(&caught),
        "hushrun: unit: all 2 test files run; 1 that already passed on the files it read runs anyway, as a check"
    );
    assert!(
        caught.stderr.contains("unit: test/hidden.test.ts failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"),
        "{}",
        caught.stderr
    );
}

// covers: C-43, C-39, C-34
#[test]
fn an_audited_vitest_test_that_fails_again_alone_suspends_reuse() {
    if !have_vitest() {
        return;
    }
    // One test file, two tests: one fails while FAIL is set, the other while HIDDEN is, which no
    // file says, as a missed dependency would.
    let files = vec![
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
    ];
    let r = repo(&files);
    assert_eq!(r.run(VITEST_RUN, &[("FAIL", "1")]).code, 1);
    // ok passed on these files, and is the one test that could be reused: it runs as a check,
    // and fails. It runs once more, alone, and fails again: a disagreement.
    let caught = r.run(VITEST_RUN, &[("HIDDEN", "1")]);
    assert_eq!(caught.code, 1, "{}", caught.stderr);
    assert!(
        caught.stderr.contains(
            "hushrun: unit: all 2 tests run; 1 that already passed on the files it read runs anyway, as a check"
        ),
        "{}",
        caught.stderr
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: test/two.test.ts::ok failed, though it passed before on the same files it read: running it once more"
        ),
        "{}",
        caught.stderr
    );
    // The second run ran ok alone: flips was left out one by one.
    assert!(
        plain(&caught.stdout).contains("Tests  1 failed | 1 skipped (2)"),
        "{}",
        caught.stdout
    );
    assert!(
        caught.stderr.contains(
            "hushrun: unit: test/two.test.ts::ok failed again, though it passed before on the same files it read: reuse of unit's test files is suspended"
        ),
        "{}",
        caught.stderr
    );
    assert_eq!(r.disagreements(), ["test/two.test.ts::ok"]);
}

/// What `hushrun why` says, stdout and stderr, of `unit` after the runs so far.
fn why_of(r: &repo::Repo, unit: &str) -> String {
    let why = r.run(&["why", "unit", unit], &[]);
    format!("{}\n{}", why.stdout, why.stderr)
}

// covers: C-37, C-43
#[test]
fn why_says_no_pass_of_a_vitest_run_is_kept_after_an_error_no_test_owns() {
    if !have_vitest() {
        return;
    }
    // A timer `leaks` left throws after it: Vitest reports an error no test owns and fails the
    // run. No pass of it is kept, a.test.ts's included, and `hushrun why` says so of each test
    // and test file, in bad.test.ts and in a.test.ts, which passed.
    let r = repo(&[
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
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
    ]);
    let ran = r.run(VITEST_RUN, &[]);
    assert_eq!(ran.code, 1, "{}\n{}", ran.stdout, ran.stderr);
    let said = "Vitest reported an error no test owns (in test/bad.test.ts: late timer), so no pass of that run is kept";
    for unit in [
        "test/bad.test.ts::waits",
        "test/bad.test.ts",
        "test/a.test.ts::a1",
        "test/a.test.ts",
    ] {
        let why = why_of(&r, unit);
        assert!(why.contains(said), "{unit}: {why}");
    }
}

// covers: C-37, C-43
#[cfg(unix)]
#[test]
fn why_says_a_second_start_of_vitest_kept_every_pass_of_its_run_out() {
    use std::os::unix::fs::PermissionsExt as _;
    if !have_vitest() {
        return;
    }
    let r = repo(&[
        file("hushrun.yaml", &yaml("{mode: reuse, unit: test}")),
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
            "test/f.test.ts",
            "import { test, expect } from 'vitest'\ntest('f1', () => { expect(process.env.FAILNOW).toBeUndefined() })\n",
        ),
    ]);
    // As a package script does under `pnpm vitest run`: a stand-in `pnpm` that starts it twice.
    let bin = r.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("pnpm"),
        "#!/bin/sh\nnode_modules/.bin/vitest run && FAILNOW=1 node_modules/.bin/vitest run\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("pnpm"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let ran = r.run(
        &["run", "unit", "--", "pnpm", "vitest", "run"],
        &[("PATH", &path)],
    );
    assert_eq!(ran.code, 1, "{}\n{}", ran.stdout, ran.stderr);
    let said = "Vitest started more than once in its command, and hushrun observes only the first start: what another start ran can't be told, so no pass of that run is kept";
    for unit in [
        "test/f.test.ts::f1",
        "test/f.test.ts",
        "test/a.test.ts::a1",
        "test/a.test.ts",
    ] {
        let why = why_of(&r, unit);
        assert!(why.contains(said), "{unit}: {why}");
    }
}

/// The line `hushrun init` proposed before 0.14, which a config that type-checks itself refuses:
/// `process.env.X && …` is `"" | Promise<any> | undefined`, and `""` is no `PluginOption`.
const OLD_LINE: &str = "plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())]";

// covers: C-15, C-35
#[test]
fn the_line_init_proposes_for_vitest_passes_a_config_that_type_checks_itself() {
    if !have_vitest() {
        return;
    }
    let modules = vitest_modules().expect("Vitest");
    let typed = ["typescript/package.json", "@types/node/package.json"]
        .iter()
        .all(|p| modules.join(p).is_file());
    if !tools(
        "TypeScript and @types/node beside Vitest (HUSHRUN_TEST_VITEST)",
        typed,
    ) {
        return;
    }
    // What init proposes, for a project like svgo's: Vitest 4, its test script `vitest run`.
    let r = repo(&[
        file("hushrun.yaml", &yaml("reuse")),
        file(".gitignore", "node_modules\n"),
        file(
            "package.json",
            "{\"name\": \"shop\", \"private\": true, \"type\": \"module\", \"scripts\": {\"test\": \"vitest run\"}, \"devDependencies\": {\"vitest\": \"4.1.12\"}}\n",
        ),
        file("package-lock.json", "{\"lockfileVersion\": 3}\n"),
        file(
            "vitest.config.ts",
            "import { defineConfig } from 'vitest/config'\nexport default defineConfig({ test: {} })\n",
        ),
        file(
            "tsconfig.json",
            "{\"compilerOptions\": {\"strict\": true, \"noEmit\": true, \"module\": \"nodenext\", \"moduleResolution\": \"nodenext\", \"target\": \"es2022\", \"skipLibCheck\": true, \"types\": [\"node\"]}, \"include\": [\"vitest.config.ts\", \"test/**/*\"]}\n",
        ),
        file(
            "test/a.test.ts",
            "import { test, expect } from 'vitest'\ntest('one', () => { expect(1).toBe(1) })\n",
        ),
        file(
            "test/a.test-d.ts",
            "import { expectTypeOf, test } from 'vitest'\ntest('types', () => { expectTypeOf(1).toEqualTypeOf<number>() })\n",
        ),
    ]);
    let init = r.run(&["init", "--local", "--dry-run"], &[]);
    let said = format!("{}\n{}", init.stdout, init.stderr);
    let proposed = said
        .split("vitest.config.ts: add `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .unwrap_or_else(|| panic!("init proposed no line for Vitest:\n{said}"))
        .to_string();
    // The config type-checks itself (`typecheck.enabled`, under `strict`), in three forms.
    let forms = |line: &str| -> Vec<(&'static str, String)> {
        vec![
            (
                "defineConfig",
                format!(
                    "import {{ defineConfig }} from 'vitest/config'\nexport default defineConfig({{\n  {line},\n  test: {{ typecheck: {{ enabled: true }} }},\n}})\n"
                ),
            ),
            (
                "an object of ViteUserConfig",
                format!(
                    "import type {{ ViteUserConfig }} from 'vitest/config'\nconst config: ViteUserConfig = {{\n  {line},\n  test: {{ typecheck: {{ enabled: true }} }},\n}}\nexport default config\n"
                ),
            ),
            (
                "a project's plugins",
                format!(
                    "import {{ defineConfig, defineProject }} from 'vitest/config'\nexport default defineConfig({{\n  test: {{ typecheck: {{ enabled: true }}, projects: [defineProject({{ {line}, test: {{ name: 'one', typecheck: {{ enabled: true }} }} }})] }},\n}})\n"
                ),
            ),
        ]
    };
    // Plain Vitest, no hushrun anywhere: the project's own test command.
    let vitest_run = || {
        let mut cmd = r.command(&r.repo.join("node_modules/.bin/vitest").to_string_lossy());
        let out = cmd.arg("run").current_dir(&r.repo).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                plain(&String::from_utf8_lossy(&out.stdout)),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    for ((form, before), (_, after)) in forms(OLD_LINE).into_iter().zip(forms(&proposed)) {
        r.write("vitest.config.ts", &before);
        let (code, out) = vitest_run();
        assert!(
            code != 0 && out.contains("is not assignable to type 'PluginOption'"),
            "{form}, the old line: {out}"
        );
        r.write("vitest.config.ts", &after);
        let (code, out) = vitest_run();
        assert_eq!(code, 0, "{form}, `{proposed}`: {out}");
    }
}
