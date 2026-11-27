//! `hushrun packages` with Yarn (K-10): a store baked by the check's own `yarn install` holds Yarn
//! 4's cache zips, or Yarn 1's offline mirror tarballs, which `yarn.lock` verifies, so an unsigned
//! store is read. `hushrun packages exec -- yarn install` seeds them, into the cache folder Yarn 4
//! names, or into a mirror of hushrun's that the install is pointed at for Yarn 1, and Yarn
//! installs with no registry to reach, leaving the same `.pnp.cjs` or `node_modules` as an install
//! from the network. The real tools: `HUSHRUN_TEST_YARN` (a Yarn 4) and `HUSHRUN_TEST_YARN1`,
//! else a Yarn 1 `yarn` on `PATH`; a machine without one passes with a line. A run gets nothing
//! from a Yarn part, so no key changes, which needs no Yarn to show.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/node.rs"]
mod fixtures;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cli::Cli as _;
use fixtures::{NODE_RUN, file, node_shop, nodes};
use repo::Repo;

const CLEAR: &[&str] = &[
    "HUSHRUN_PACKAGES_LOWER",
    "HUSHRUN_PACKAGES_LOWER_UNTRUSTED",
    "HUSHRUN_PACKAGES",
    "HUSHRUN_YARN",
    "YARN_CACHE_FOLDER",
    "YARN_GLOBAL_FOLDER",
    "YARN_ENABLE_GLOBAL_CACHE",
    "YARN_YARN_OFFLINE_MIRROR",
    "YARN_NPM_REGISTRY_SERVER",
    "NODE_OPTIONS",
];

const PACKAGE4: &str = include_str!("../../../testdata/packages/yarn4/package.json");
const LOCK4: &str = include_str!("../../../testdata/packages/yarn4/yarn.lock");
const PACKAGE1: &str = include_str!("../../../testdata/packages/yarn1/package.json");
const LOCK1: &str = include_str!("../../../testdata/packages/yarn1/yarn.lock");
/// `has-flag@4.0.0`'s integrity in [`LOCK1`].
const HAS_FLAG: &str = "sha512-EykJT/Q1KjTWctppgIAgfSO0tKVuZUjhgMr17kqTumMl6Afv3EISleU7qZUzoXDFTAHTDC4NOoG/ZxU3EvlMPQ==";

/// The tool `variable` names, else `fallback` on `PATH` when its version starts with `major`.
fn tool(variable: &str, fallback: Option<&str>, major: &str) -> Option<String> {
    let named = std::env::var(variable).ok();
    let program = named.clone().or(fallback.map(str::to_string))?;
    let version = Command::new(&program)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    match version {
        Some(v) if v.starts_with(major) => Some(program),
        _ if named.is_some() => panic!("{variable} names no Yarn {major} that runs"),
        _ => None,
    }
}

fn yarn4() -> Option<String> {
    let found = tool("HUSHRUN_TEST_YARN", None, "4.");
    if found.is_none() {
        eprintln!("note: skipping: no Yarn 4 (HUSHRUN_TEST_YARN)");
    }
    found
}

fn yarn1() -> Option<String> {
    let found = tool("HUSHRUN_TEST_YARN1", Some("yarn"), "1.");
    if found.is_none() {
        eprintln!("note: skipping: no Yarn 1 (HUSHRUN_TEST_YARN1, or yarn on PATH)");
    }
    found
}

fn out(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Every path under `node_modules`, a link with its target, a file with its size.
fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.join("node_modules")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let shown = path.strip_prefix(root).unwrap().display().to_string();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                out.push(format!(
                    "{shown} -> {}",
                    std::fs::read_link(&path).unwrap().display()
                ));
            } else if meta.is_dir() {
                out.push(format!("{shown}/"));
                stack.push(path);
            } else {
                out.push(format!("{shown} {}", meta.len()));
            }
        }
    }
    out.sort();
    out
}

fn bake(r: &Repo, command: &[&str]) -> (PathBuf, String) {
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .arg("--")
        .args(command)
        .env("YARN_ENABLE_TELEMETRY", "0")
        .output()
        .unwrap();
    let said = out(&baked);
    assert!(baked.status.success(), "{said}");
    (store, said)
}

// covers: K-6, K-7, K-10
#[test]
fn a_store_baked_by_yarn_4_seeds_its_cache_and_installs_with_no_registry() {
    let Some(yarn) = yarn4() else {
        return;
    };
    let r = Repo::new(&[
        file("package.json", PACKAGE4),
        file("yarn.lock", LOCK4),
        file(".gitignore", "node_modules\n.yarn\n.pnp.*\n"),
    ])
    .clear(CLEAR);
    let global = r.root.join("global");
    let clean = || {
        for gone in [".yarn", ".pnp.cjs", ".pnp.loader.mjs", "node_modules"] {
            let at = r.repo.join(gone);
            let _ = std::fs::remove_dir_all(&at);
            let _ = std::fs::remove_file(&at);
        }
        let _ = std::fs::remove_dir_all(&global);
    };
    // What an install from the network leaves.
    let ran = Command::new(&yarn)
        .args(["install", "--immutable"])
        .current_dir(&r.repo)
        .env("YARN_GLOBAL_FOLDER", &global)
        .env("YARN_ENABLE_TELEMETRY", "0")
        .output()
        .unwrap();
    assert!(ran.status.success(), "{}", out(&ran));
    let from_network = std::fs::read_to_string(r.repo.join(".pnp.cjs")).unwrap();
    clean();
    let (store, said) = bake(&r, &[yarn.as_str(), "install", "--immutable"]);
    assert!(said.contains("packages: yarn: 7 files"), "{said}");
    let rows = std::fs::read_to_string(store.join("rows/yarn.jsonl")).unwrap();
    assert!(rows.contains("\"path\":\"berry/chalk-npm-4.1.2-"), "{rows}");
    assert!(
        rows.contains("\"locked\":\"10c0/4a3fef5cc34975c898ffe77141450f679721df9dde00f6c304353fa9c8b571929123b26a0e4617bde5018977eb655b31970c297b91b63ee83bb82aeb04666880\""),
        "{rows}"
    );
    let install = |lower: &Path| {
        clean();
        r.hushrun()
            .args(["packages", "exec", "--"])
            .arg(&yarn)
            .args(["install", "--immutable"])
            .env("HUSHRUN_YARN", &yarn)
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .env("YARN_GLOBAL_FOLDER", &global)
            .env("YARN_ENABLE_TELEMETRY", "0")
            .env("YARN_NPM_REGISTRY_SERVER", "http://127.0.0.1:9")
            .output()
            .unwrap()
    };
    let seeded = install(&store);
    let said = out(&seeded);
    assert!(seeded.status.success(), "{said}");
    assert!(said.contains("packages: yarn: seeded 7 files"), "{said}");
    assert!(global.join("cache").is_dir());
    assert_eq!(
        std::fs::read_to_string(r.repo.join(".pnp.cjs")).unwrap(),
        from_network
    );
    // Without the store the same install fails: the store is what it read.
    assert!(
        !install(Path::new("")).status.success(),
        "an install with no store reached a registry"
    );
    // One byte of one zip flipped: the seed catches it, and Yarn, with no registry, fails.
    let zip = rows
        .lines()
        .find(|l| l.contains("berry/chalk-npm"))
        .and_then(|l| l.split("\"path\":\"").nth(1))
        .and_then(|l| l.split('"').next())
        .unwrap()
        .to_string();
    let at = store.join("parts/yarn").join(&zip);
    let mut bytes = std::fs::read(&at).unwrap();
    bytes[100] ^= 1;
    std::fs::write(&at, bytes).unwrap();
    let tampered = install(&store);
    let said = out(&tampered);
    assert!(
        said.contains(&format!(
            "left out 1 files whose bytes changed since the bake, such as {zip}"
        )),
        "{said}"
    );
    assert!(!tampered.status.success(), "{said}");
}

// covers: K-6, K-7, K-10
#[test]
fn a_store_baked_by_yarn_1_seeds_a_mirror_the_install_is_pointed_at() {
    let Some(yarn) = yarn1() else {
        return;
    };
    let r = Repo::new(&[
        file("package.json", PACKAGE1),
        file("yarn.lock", LOCK1),
        file(".gitignore", "node_modules\n"),
    ])
    .clear(CLEAR);
    let ran = Command::new(&yarn)
        .args(["install", "--frozen-lockfile", "--ignore-scripts"])
        .current_dir(&r.repo)
        .env("YARN_CACHE_FOLDER", r.root.join("net"))
        .output()
        .unwrap();
    assert!(ran.status.success(), "{}", out(&ran));
    let from_network = tree(&r.repo);
    // With `node_modules` up to date, Yarn 1 downloads nothing, and the bake says so.
    let store = r.root.join("packages");
    let up_to_date = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .args([
            "--",
            yarn.as_str(),
            "install",
            "--frozen-lockfile",
            "--ignore-scripts",
        ])
        .output()
        .unwrap();
    assert!(
        out(&up_to_date)
            .contains("packages: yarn: left out: the fetch downloaded nothing into Yarn's cache"),
        "{}",
        out(&up_to_date)
    );
    let _ = std::fs::remove_dir_all(r.repo.join("node_modules"));
    let (store, said) = bake(
        &r,
        &[
            yarn.as_str(),
            "install",
            "--frozen-lockfile",
            "--ignore-scripts",
        ],
    );
    assert!(said.contains("packages: yarn: 6 files"), "{said}");
    let rows = std::fs::read_to_string(store.join("rows/yarn.jsonl")).unwrap();
    assert!(rows.contains("\"path\":\"v1/chalk-4.1.2.tgz\""), "{rows}");
    assert!(
        rows.contains(&format!("\"locked\":\"{HAS_FLAG}\"")),
        "{rows}"
    );
    let install = |cache: &str, lower: &Path| {
        let _ = std::fs::remove_dir_all(r.repo.join("node_modules"));
        r.hushrun()
            .args(["packages", "exec", "--"])
            .arg(&yarn)
            .args([
                "install",
                "--frozen-lockfile",
                "--ignore-scripts",
                "--network-timeout",
                "5000",
            ])
            .env("HUSHRUN_YARN", &yarn)
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .env("YARN_CACHE_FOLDER", r.root.join(cache))
            .env("YARN_HTTPS_PROXY", "http://127.0.0.1:9")
            .env("YARN_PROXY", "http://127.0.0.1:9")
            .output()
            .unwrap()
    };
    let seeded = install("empty", &store);
    let said = out(&seeded);
    assert!(seeded.status.success(), "{said}");
    assert!(said.contains("packages: yarn: seeded 6 files"), "{said}");
    assert!(
        r.root
            .join("empty/hushrun-offline-mirror/chalk-4.1.2.tgz")
            .is_file()
    );
    assert_eq!(tree(&r.repo), from_network);
    // `env` names the mirror, and says the seed is done.
    let env = r
        .hushrun()
        .args(["packages", "env"])
        .env("HUSHRUN_YARN", &yarn)
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("YARN_CACHE_FOLDER", r.root.join("empty"))
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&env.stdout).contains("export YARN_YARN_OFFLINE_MIRROR="),
        "{}",
        out(&env)
    );
    assert!(out(&env).contains("seeded already"), "{}", out(&env));
    // Without the store the same install fails.
    assert!(
        !install("none", Path::new("")).status.success(),
        "an install with no store reached a registry"
    );
    // The lockfile changed by one package: the tarball it no longer names is left out.
    r.write(
        "yarn.lock",
        &LOCK1.replace(HAS_FLAG, &format!("sha512-{}==", "A".repeat(86))),
    );
    let said = out(&install("changed", &store));
    assert!(said.contains("seeded 5 files"), "{said}");
    assert!(
        said.contains("left out 1 files no lockfile here names now"),
        "{said}"
    );
    r.write("yarn.lock", LOCK1);
    // A mirror the person names is theirs: the part is left alone.
    let theirs = r
        .hushrun()
        .args(["packages", "restore"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("YARN_YARN_OFFLINE_MIRROR", r.root.join("theirs"))
        .output()
        .unwrap();
    assert!(
        out(&theirs).contains("packages: yarn: left alone: yarn-offline-mirror is set already (YARN_YARN_OFFLINE_MIRROR)"),
        "{}",
        out(&theirs)
    );
}

/// A version 8 `yarn.lock` naming one package whose cache zip is `zip`.
fn lock_of(zip: &[u8], version: u64) -> String {
    use sha2::Digest as _;
    let hex: String = sha2::Sha512::digest(zip)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "__metadata:\n  version: {version}\n  cacheKey: 10c0\n\n\"a@npm:1.0.0\":\n  version: 1.0.0\n  resolution: \"a@npm:1.0.0\"\n  checksum: 10c0/{hex}\n  languageName: node\n  linkType: hard\n"
    )
}

/// A store with a `yarn` part harvested from a hand-made Yarn 4 global folder: no Yarn needed.
fn harvested(r: &Repo) -> Result<PathBuf, String> {
    let global = r.root.join("yarn-global");
    std::fs::create_dir_all(global.join("cache")).unwrap();
    std::fs::write(global.join("cache/a-npm-1.0.0-0123456789-10c0.zip"), "zip").unwrap();
    std::fs::write(global.join("cache/.gitignore"), "*").unwrap();
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .arg(format!("--harvest=yarn={}", global.display()))
        .output()
        .unwrap();
    if baked.status.success() {
        assert!(
            out(&baked).contains("packages: yarn: 1 files"),
            "{}",
            out(&baked)
        );
        Ok(store)
    } else {
        Err(out(&baked))
    }
}

// covers: K-5, K-10
#[test]
fn a_yarn_3_lockfile_is_one_this_hushrun_doesnt_read() {
    let r = Repo::new(&[file("yarn.lock", &lock_of(b"zip", 6))]).clear(CLEAR);
    let said = harvested(&r).unwrap_err();
    assert!(
        said.contains(
            "packages: yarn: left out: yarn.lock is a version this hushrun doesn't read (version 6)"
        ),
        "{said}"
    );
}

// covers: K-10
// The keys of §3.3: everything COMMAND sees in a run is the same with and without the store, and
// a Node test runner check (whose adapter sets NODE_OPTIONS) run with it is skipped without it.
#[test]
fn a_yarn_part_gives_a_run_nothing_and_changes_no_key() {
    let r = Repo::new(&[file("yarn.lock", &lock_of(b"zip", 8))]).clear(CLEAR);
    let store = harvested(&r).unwrap();
    let rows = std::fs::read_to_string(store.join("rows/yarn.jsonl")).unwrap();
    assert!(rows.contains("\"locked\":\"10c0/"), "{rows}");
    let mirror = r.root.join("seeded");
    let env_of = |lower: &Path| {
        let ran = r
            .hushrun()
            .args(["run", "e", "--force", "--", "sh", "-c", "env | sort"])
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .env("YARN_GLOBAL_FOLDER", &mirror)
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
    assert!(!mirror.exists(), "`hushrun run` never seeds");
    for node in nodes() {
        let mut files = node_shop("reuse");
        files.push(file("yarn.lock", &lock_of(b"zip", 8)));
        let r = fixtures::repo(&node, &files).clear(CLEAR);
        let store = harvested(&r).unwrap();
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
