//! `hushrun packages` with pnpm (K-10, K-18): a store baked by `pnpm fetch` is seeded into an
//! empty pnpm store by `hushrun packages exec`, and `pnpm install --frozen-lockfile` then installs
//! with no registry to reach. The real pnpm: `HUSHRUN_TEST_PNPM`, else `pnpm` on `PATH`; a machine
//! without it passes with a line. A pnpm part changes nothing a run gives COMMAND, so no check's
//! key, no test adapter's `NODE_OPTIONS` and no `tsc/1` key: those tests need no pnpm.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/node.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;
#[path = "common/vitest.rs"]
mod vitest;

use std::path::{Path, PathBuf};
use std::process::Command;

use cli::Cli as _;
use fixtures::{NODE_RUN, file, node_shop, nodes};
use repo::Repo;

const CLEAR: &[&str] = &[
    "HUSHRUN_PACKAGES_LOWER",
    "HUSHRUN_PACKAGES_LOWER_UNTRUSTED",
    "HUSHRUN_PACKAGES",
    "npm_config_store_dir",
    "NPM_CONFIG_STORE_DIR",
    "npm_config_registry",
    "NODE_OPTIONS",
];

const LOCK: &str = "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\n\nimporters:\n\n  .:\n    dependencies:\n      ms:\n        specifier: 2.1.3\n        version: 2.1.3\n\npackages:\n\n  ms@2.1.3:\n    resolution: {integrity: sha512-6FlzubTLZG3J2a/NVCAleEhjzq5oxgHyaCU9yYXvcLsvoVaHJq/s5xXI6/XXP6tz7R9xAOtHnSO/tXtF3WRTlA==}\n\nsnapshots:\n\n  ms@2.1.3: {}\n";

fn pnpm() -> Option<String> {
    let program = std::env::var("HUSHRUN_TEST_PNPM").unwrap_or_else(|_| "pnpm".to_string());
    let works = Command::new(&program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if works {
        return Some(program);
    }
    if std::env::var_os("HUSHRUN_TEST_PNPM").is_some() {
        panic!("HUSHRUN_TEST_PNPM names a pnpm that doesn't run");
    }
    eprintln!("note: skipping: no pnpm (HUSHRUN_TEST_PNPM, or pnpm on PATH)");
    None
}

fn out(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A store with a `pnpm` part harvested from a hand-made pnpm store: no pnpm needed.
fn harvested(r: &Repo) -> PathBuf {
    let fake = r.root.join("pnpm-store");
    std::fs::create_dir_all(fake.join("v10/files/aa")).unwrap();
    std::fs::write(fake.join("v10/files/aa/0123"), "module.exports = 1\n").unwrap();
    std::fs::create_dir_all(fake.join("v10/index/bb")).unwrap();
    std::fs::write(fake.join("v10/index/bb/cdef-ms@2.1.3.json"), "{}").unwrap();
    std::fs::create_dir_all(fake.join("v10/tmp")).unwrap();
    std::fs::write(fake.join("v10/tmp/x"), "").unwrap();
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .arg(format!("--harvest=pnpm={}", fake.display()))
        .output()
        .unwrap();
    assert!(baked.status.success(), "{}", out(&baked));
    assert!(
        out(&baked).contains("packages: pnpm: 2 files"),
        "{}",
        out(&baked)
    );
    store
}

// covers: K-6, K-10, K-18
#[test]
fn a_store_baked_by_pnpm_fetch_installs_into_an_empty_store_with_no_registry() {
    let Some(pnpm) = pnpm() else {
        return;
    };
    let r = Repo::new(&[
        file(
            "package.json",
            "{\"name\":\"shop\",\"private\":true,\"dependencies\":{\"ms\":\"2.1.3\"}}\n",
        ),
        file("pnpm-lock.yaml", LOCK),
        file(".gitignore", "node_modules\n"),
    ])
    .clear(CLEAR);
    let home = r.root.join("home");
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .args(["--", &pnpm, "fetch"])
        .env("XDG_DATA_HOME", &home)
        .env("XDG_CACHE_HOME", &home)
        .output()
        .unwrap();
    let said = out(&baked);
    assert!(baked.status.success(), "{said}");
    assert!(said.contains("(pnpm-lock.yaml)"), "{said}");
    let _ = std::fs::remove_dir_all(r.repo.join("node_modules"));
    let rows = std::fs::read_to_string(store.join("rows/pnpm.jsonl")).unwrap();
    assert!(rows.contains("\"package\":\"ms@2.1.3\""), "{rows}");
    assert!(!rows.contains("/projects/"), "{rows}");
    // An install with an empty store and a registry nobody answers.
    let install = |pnpm_store: &Path, lower: &Path| {
        let _ = std::fs::remove_dir_all(r.repo.join("node_modules"));
        r.hushrun()
            .args([
                "packages",
                "exec",
                "--",
                &pnpm,
                "install",
                "--frozen-lockfile",
            ])
            .env("XDG_DATA_HOME", &home)
            .env("XDG_CACHE_HOME", &home)
            .env("npm_config_store_dir", pnpm_store)
            .env("npm_config_registry", "http://127.0.0.1:9/")
            .env("npm_config_fetch_retries", "0")
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap()
    };
    let empty = r.root.join("empty");
    let first = install(&empty, &store);
    let said = out(&first);
    assert!(first.status.success(), "{said}");
    assert!(said.contains("hushrun: packages: pnpm: seeded "), "{said}");
    assert!(said.contains("downloaded 0"), "{said}");
    assert!(r.repo.join("node_modules/ms/package.json").is_file());
    // node_modules links inside the checkout, as an install from the network does.
    let link = std::fs::read_link(r.repo.join("node_modules/ms")).unwrap();
    assert!(link.is_relative(), "{}", link.display());
    // The second seed costs one stat.
    let again = install(&empty, &store);
    assert!(again.status.success(), "{}", out(&again));
    assert!(
        out(&again).contains("pnpm: seeded already into"),
        "{}",
        out(&again)
    );
    // Without the store the same install fails: the store is what it read.
    assert!(
        !install(&r.root.join("empty2"), Path::new(""))
            .status
            .success()
    );
    // One byte of one file flipped: the seed catches it, and pnpm, with no registry, fails.
    let index = rows
        .lines()
        .find(|l| l.contains("\"package\":\"ms@2.1.3\""))
        .and_then(|l| l.split("\"path\":\"").nth(1))
        .and_then(|l| l.split('"').next())
        .unwrap()
        .to_string();
    let at = store.join("parts/pnpm").join(&index);
    let mut bytes = std::fs::read(&at).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&at, bytes).unwrap();
    let tampered = install(&r.root.join("empty3"), &store);
    assert!(
        out(&tampered).contains("files whose bytes changed since the bake"),
        "{}",
        out(&tampered)
    );
    assert!(!tampered.status.success(), "{}", out(&tampered));
}

// covers: K-10, K-18
#[test]
fn a_pnpm_part_gives_a_run_nothing_and_exec_keeps_node_options() {
    let r = Repo::new(&[file("pnpm-lock.yaml", LOCK), file("hushrun.yaml", "version: 1\nrepository: example.com/acme/shop\nchecks:\n  e:\n    run: sh -c 'env | sort'\n")]).clear(CLEAR);
    let store = harvested(&r);
    let pnpm_store = r.root.join("seeded");
    // Everything COMMAND sees in a run is the same with and without the store.
    let env_of = |lower: &Path| {
        let ran = r
            .hushrun()
            .args(["run", "e", "--force", "--", "sh", "-c", "env | sort"])
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .env("npm_config_store_dir", &pnpm_store)
            .output()
            .unwrap();
        assert!(ran.status.success(), "{}", out(&ran));
        String::from_utf8_lossy(&ran.stdout)
            .lines()
            // The store's own variable, and the folder each run's build session has.
            .filter(|l| !l.starts_with("HUSHRUN_PACKAGES_LOWER=") && !l.contains("/build/sessions/"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let (with, without) = (env_of(&store), env_of(Path::new("")));
    let differ: Vec<&String> = with
        .iter()
        .filter(|l| !without.contains(l))
        .chain(without.iter().filter(|l| !with.contains(l)))
        .collect();
    assert!(
        differ.is_empty(),
        "the store changed what COMMAND sees: {differ:?}"
    );
    // `hushrun run` never seeds.
    assert!(!pnpm_store.exists());
    // `packages exec` seeds, and passes NODE_OPTIONS on as the person set it, or not at all.
    let exec = |node_options: Option<&str>| {
        let mut cmd = r.hushrun();
        cmd.args([
            "packages",
            "exec",
            "--",
            "sh",
            "-c",
            "echo \"${NODE_OPTIONS-none}\"",
        ])
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("npm_config_store_dir", &pnpm_store);
        if let Some(value) = node_options {
            cmd.env("NODE_OPTIONS", value);
        }
        let ran = cmd.output().unwrap();
        assert!(ran.status.success(), "{}", out(&ran));
        String::from_utf8_lossy(&ran.stdout).trim().to_string()
    };
    assert_eq!(exec(None), "none");
    assert_eq!(
        exec(Some("--max-old-space-size=512")),
        "--max-old-space-size=512"
    );
    assert!(pnpm_store.join("v10/files/aa/0123").is_file());
    assert!(!pnpm_store.join("v10/tmp/x").exists());
    // `restore` says what it did.
    let restore = r
        .hushrun()
        .args(["packages", "restore"])
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("npm_config_store_dir", &pnpm_store)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&restore.stdout),
        format!(
            "packages: pnpm: seeded already into {}\n",
            pnpm_store.display()
        )
    );
}

// covers: K-10, K-18
// The keys of §3.3: a Node test runner check (whose adapter sets NODE_OPTIONS) run with the
// store is skipped without it: the same key.
#[test]
fn a_node_test_check_has_the_same_key_with_and_without_a_pnpm_part() {
    for node in nodes() {
        let mut files = node_shop("reuse");
        files.push(file("pnpm-lock.yaml", LOCK));
        let r = fixtures::repo(&node, &files).clear(CLEAR);
        let store = harvested(&r);
        let with = r.run(
            NODE_RUN,
            &[
                ("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1"),
                ("HUSHRUN_PACKAGES_LOWER", store.to_str().unwrap()),
            ],
        );
        assert_eq!(with.code, 0, "{}\n{}", with.stdout, with.stderr);
        let without = r.run(NODE_RUN, &[("HUSHRUN_PACKAGES_LOWER", "")]);
        assert!(
            without.stderr.contains("unit skipped"),
            "{}",
            without.stderr
        );
    }
}

// covers: K-10, K-18
// A Vitest check: the same key, and the same reads of each test file, with and without the store.
#[test]
fn a_vitest_check_has_the_same_key_and_reads_with_and_without_a_pnpm_part() {
    if !vitest::have_vitest() {
        return;
    }
    let mut files = vitest::shop("reuse");
    files.push(file("pnpm-lock.yaml", LOCK));
    let r = vitest::repo(&files).clear(CLEAR);
    let store = harvested(&r);
    let lower = store.to_str().unwrap();
    let run = ["run", "unit", "--", "node_modules/.bin/vitest", "run"];
    let with = r.run(
        &run,
        &[
            ("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1"),
            ("HUSHRUN_PACKAGES_LOWER", lower),
        ],
    );
    assert_eq!(with.code, 0, "{}\n{}", with.stdout, with.stderr);
    let reads = vitest::reads(&r, "test/cart.test.ts");
    let without = r.run(&run, &[("HUSHRUN_PACKAGES_LOWER", "")]);
    assert!(
        without.stderr.contains("unit skipped"),
        "{}",
        without.stderr
    );
    let forced = [
        "run",
        "unit",
        "--force",
        "--",
        "node_modules/.bin/vitest",
        "run",
    ];
    let again = r.run(&forced, &[("HUSHRUN_PACKAGES_LOWER", "")]);
    assert_eq!(again.code, 0, "{}\n{}", again.stdout, again.stderr);
    assert_eq!(vitest::reads(&r, "test/cart.test.ts"), reads);
}

/// TypeScript 6, as `build_tsc.rs` finds it.
fn typescript() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HUSHRUN_TEST_TSC")?);
    let package = std::fs::read_to_string(dir.join("typescript/package.json")).ok()?;
    package.contains("\"version\": \"6.").then_some(dir)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let kind = entry.file_type().unwrap();
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest);
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path()).unwrap(), &dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

// covers: K-10, K-18
// A `tsc` compile built with the store is reused without it: the same `tsc/1` key.
#[test]
fn a_tsc_compile_has_the_same_key_with_and_without_a_pnpm_part() {
    let Some(modules) = typescript() else {
        if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
            panic!("no TypeScript 6 in HUSHRUN_TEST_TSC, and HUSHRUN_TEST_TOOLS=required");
        }
        eprintln!("note: skipping: no TypeScript 6 (HUSHRUN_TEST_TSC)");
        return;
    };
    let r = Repo::new(&[
        file("pnpm-lock.yaml", LOCK),
        file(".gitignore", "node_modules/\ndist/\n*.tsbuildinfo\n"),
        file("package.json", "{\"name\":\"m\",\"private\":true}\n"),
        file(
            "tsconfig.json",
            "{\"compilerOptions\":{\"composite\":true,\"module\":\"nodenext\",\"outDir\":\"dist\",\"rootDir\":\"src\",\"strict\":true},\"include\":[\"src\"]}\n",
        ),
        file("src/index.ts", "export const twice = (x: number): number => x * 2\n"),
    ])
    .clear(CLEAR)
    .clear(&["HUSHRUN_TSC", "HUSHRUN_UNITS", "HUSHRUN_JEST", "HUSHRUN_VITEST"]);
    copy_tree(&modules, &r.repo.join("node_modules"));
    let store = harvested(&r);
    let build = |lower: &Path| {
        let _ = std::fs::remove_dir_all(r.repo.join("dist"));
        let _ = std::fs::remove_file(r.repo.join("tsconfig.tsbuildinfo"));
        let ran = r
            .hushrun()
            .args([
                "run",
                "build",
                "--force",
                "--",
                "node",
                "node_modules/typescript/bin/tsc",
                "-b",
            ])
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap();
        let said = out(&ran);
        assert!(ran.status.success(), "{said}");
        said.lines()
            .find(|l| l.contains("build: typescript: ") && l.contains("compile"))
            .map_or_else(|| said.clone(), str::to_string)
    };
    let line = build(&store);
    assert!(line.contains("1 built"), "{line}");
    let line = build(Path::new(""));
    assert!(line.contains("1 reused"), "{line}");
    assert!(r.repo.join("dist/index.js").is_file());
}
