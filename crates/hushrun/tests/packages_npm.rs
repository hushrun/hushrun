//! `hushrun packages` with npm (K-10, K-19): a store baked by `npm ci` holds the tarballs of npm's
//! cache, which `package-lock.json`'s `integrity` verifies, so an unsigned store is read; `hushrun
//! packages exec -- npm ci` seeds an empty cache with the tarballs the lockfile names now, and
//! installs with no registry to reach. The real npm, as CI's Node has it; a machine without it
//! passes with a line. A run gets nothing from an npm part, so no key changes.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/node.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::Path;
use std::process::Command;

use cli::Cli as _;
use fixtures::{NODE_RUN, file, node_shop, nodes};
use repo::Repo;

const CLEAR: &[&str] = &[
    "HUSHRUN_PACKAGES_LOWER",
    "HUSHRUN_PACKAGES_LOWER_UNTRUSTED",
    "HUSHRUN_PACKAGES",
    "npm_config_cache",
    "NPM_CONFIG_CACHE",
    "npm_config_registry",
    "NODE_OPTIONS",
];

const LOCK: &str = r#"{
  "name": "shop",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": { "name": "shop", "dependencies": { "is-number": "7.0.0", "ms": "2.1.3" } },
    "node_modules/is-number": {
      "version": "7.0.0",
      "resolved": "https://registry.npmjs.org/is-number/-/is-number-7.0.0.tgz",
      "integrity": "sha512-41Cifkg6e8TylSpdtTpeLVMqvSBEVzTttHvERD741+pnZ8ANv0004MRL43QKPDlK9cGvNp6NZWZUBlbGXYxxng==",
      "license": "MIT",
      "engines": { "node": ">=0.12.0" }
    },
    "node_modules/ms": {
      "version": "2.1.3",
      "resolved": "https://registry.npmjs.org/ms/-/ms-2.1.3.tgz",
      "integrity": "sha512-6FlzubTLZG3J2a/NVCAleEhjzq5oxgHyaCU9yYXvcLsvoVaHJq/s5xXI6/XXP6tz7R9xAOtHnSO/tXtF3WRTlA==",
      "license": "MIT"
    }
  }
}
"#;
const PACKAGE: &str =
    "{\"name\":\"shop\",\"dependencies\":{\"is-number\":\"7.0.0\",\"ms\":\"2.1.3\"}}\n";

fn npm() -> bool {
    let works = Command::new("npm")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !works {
        if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
            panic!("npm is missing, and HUSHRUN_TEST_TOOLS=required");
        }
        eprintln!("note: skipping: no npm (HUSHRUN_TEST_TOOLS=required makes this fail)");
    }
    works
}

fn out(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

// covers: K-6, K-7, K-10, K-19
#[test]
fn an_unsigned_store_baked_by_npm_ci_installs_into_an_empty_cache_with_no_registry() {
    if !npm() {
        return;
    }
    let r = Repo::new(&[
        file("package.json", PACKAGE),
        file("package-lock.json", LOCK),
        file(".gitignore", "node_modules\n"),
    ])
    .clear(CLEAR);
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .args([
            "--",
            "npm",
            "ci",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ])
        .output()
        .unwrap();
    let said = out(&baked);
    assert!(baked.status.success(), "{said}");
    assert!(said.contains("packages: npm: 2 files"), "{said}");
    let rows = std::fs::read_to_string(store.join("rows/npm.jsonl")).unwrap();
    assert!(rows.contains("\"locked\":\"sha512-6Flzub"), "{rows}");
    assert!(!rows.contains("index-v5"), "{rows}");
    // Unsigned, and no say-so: the lockfile is the evidence.
    let install = |cache: &Path, lower: &Path| {
        let _ = std::fs::remove_dir_all(r.repo.join("node_modules"));
        r.hushrun()
            .args([
                "packages",
                "exec",
                "--",
                "npm",
                "ci",
                "--no-audit",
                "--no-fund",
            ])
            .env("npm_config_cache", cache)
            .env("npm_config_registry", "http://127.0.0.1:9/")
            .env("npm_config_fetch_retries", "0")
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .output()
            .unwrap()
    };
    let first = install(&r.root.join("empty"), &store);
    let said = out(&first);
    assert!(first.status.success(), "{said}");
    assert!(said.contains("packages: npm: seeded 2 files"), "{said}");
    assert!(r.repo.join("node_modules/ms/package.json").is_file());
    // Without the store the same install fails: the store is what it read.
    assert!(
        !install(&r.root.join("empty2"), Path::new(""))
            .status
            .success()
    );
    // The lockfile changed by one package: the tarball it no longer names is left out, and the
    // seed leaves no stamp, so the next lockfile can name it.
    r.write(
        "package-lock.json",
        &LOCK.replace(
            "sha512-41Cifkg6e8TylSpdtTpeLVMqvSBEVzTttHvERD741+pnZ8ANv0004MRL43QKPDlK9cGvNp6NZWZUBlbGXYxxng==",
            "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
        ),
    );
    let changed = install(&r.root.join("empty3"), &store);
    let said = out(&changed);
    assert!(said.contains("seeded 1 files"), "{said}");
    assert!(
        said.contains("left out 1 files no lockfile here names now"),
        "{said}"
    );
    r.write("package-lock.json", LOCK);
    // One byte of one tarball flipped: the seed catches it, and npm, with no registry, fails.
    let tarball = rows
        .lines()
        .next()
        .and_then(|l| l.split("\"path\":\"").nth(1))
        .and_then(|l| l.split('"').next())
        .unwrap()
        .to_string();
    let at = store.join("parts/npm").join(&tarball);
    let mut bytes = std::fs::read(&at).unwrap();
    bytes[10] ^= 1;
    std::fs::write(&at, bytes).unwrap();
    let tampered = install(&r.root.join("empty4"), &store);
    assert!(
        out(&tampered).contains("files whose bytes changed since the bake"),
        "{}",
        out(&tampered)
    );
    assert!(!tampered.status.success(), "{}", out(&tampered));
}

/// A store with an `npm` part harvested from a hand-made cache: no npm needed.
fn harvested(r: &Repo) -> std::path::PathBuf {
    let cache = r.root.join("npm-cache");
    let hex = "0".repeat(124);
    let dir = cache.join("_cacache/content-v2/sha512/aa/bb");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&hex), "tarball").unwrap();
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .arg(format!("--harvest=npm={}", cache.display()))
        .output()
        .unwrap();
    assert!(baked.status.success(), "{}", out(&baked));
    store
}

// covers: K-10, K-19
// The keys of §3.3: everything COMMAND sees in a run is the same with and without the store, and
// a Node test runner check (whose adapter sets NODE_OPTIONS) run with it is skipped without it.
#[test]
fn an_npm_part_gives_a_run_nothing_and_changes_no_key() {
    let r = Repo::new(&[file("package-lock.json", LOCK)]).clear(CLEAR);
    let store = harvested(&r);
    let cache = r.root.join("seeded");
    let env_of = |lower: &Path| {
        let ran = r
            .hushrun()
            .args(["run", "e", "--force", "--", "sh", "-c", "env | sort"])
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .env("npm_config_cache", &cache)
            .output()
            .unwrap();
        assert!(ran.status.success(), "{}", out(&ran));
        String::from_utf8_lossy(&ran.stdout)
            .lines()
            .filter(|l| {
                !l.starts_with("HUSHRUN_PACKAGES_LOWER=") && !l.contains("/build/sessions/")
            })
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
    assert!(!cache.exists(), "`hushrun run` never seeds");
    for node in nodes() {
        let mut files = node_shop("reuse");
        files.push(file("package-lock.json", LOCK));
        let r = fixtures::repo(&node, &files).clear(CLEAR);
        let store = harvested(&r);
        let with = r.run(
            NODE_RUN,
            &[("HUSHRUN_PACKAGES_LOWER", store.to_str().unwrap())],
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
