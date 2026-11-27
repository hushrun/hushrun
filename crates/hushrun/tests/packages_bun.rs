//! `hushrun packages` with Bun (K-10, K-20): a bake downloads the tarballs `bun.lock` names, each
//! checked against its `integrity`, with no tool run, so an unsigned store is read; `hushrun
//! packages exec -- bun install` unpacks each tarball the lockfile still names into an empty Bun
//! cache, and Bun installs with no registry to reach, leaving `node_modules` as an install from the
//! network does, link for link, under both of Bun's linkers. The real Bun: `HUSHRUN_TEST_BUN`, else
//! `bun` on `PATH`; a machine without it passes with a line. The bakes download from npm's
//! registry. A run gets nothing from a Bun part, so no key changes.

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
    "BUN_INSTALL_CACHE_DIR",
    "BUN_INSTALL",
    "BUN_CONFIG_REGISTRY",
    "NODE_OPTIONS",
];

const PACKAGE: &str = include_str!("../../../testdata/packages/bun/package.json");
const LOCK: &str = include_str!("../../../testdata/packages/bun/bun.lock");
/// `is-number@7.0.0`'s integrity in [`LOCK`].
const IS_NUMBER: &str = "sha512-41Cifkg6e8TylSpdtTpeLVMqvSBEVzTttHvERD741+pnZ8ANv0004MRL43QKPDlK9cGvNp6NZWZUBlbGXYxxng==";

/// No route to any registry: every proxy variable at a closed port.
const NO_NETWORK: &[(&str, &str)] = &[
    ("HTTPS_PROXY", "http://127.0.0.1:9"),
    ("HTTP_PROXY", "http://127.0.0.1:9"),
    ("https_proxy", "http://127.0.0.1:9"),
    ("http_proxy", "http://127.0.0.1:9"),
    ("NO_PROXY", ""),
    ("no_proxy", ""),
];

fn bun() -> Option<String> {
    let program = std::env::var("HUSHRUN_TEST_BUN").unwrap_or_else(|_| "bun".to_string());
    let works = Command::new(&program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if works {
        return Some(program);
    }
    if std::env::var_os("HUSHRUN_TEST_BUN").is_some() {
        panic!("HUSHRUN_TEST_BUN names a Bun that doesn't run");
    }
    eprintln!("note: skipping: no Bun (HUSHRUN_TEST_BUN, or bun on PATH)");
    None
}

fn out(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Every path under `node_modules`, a link with its target, a file with its size: what `tsc/1`'s
/// keys can see of an install.
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

fn bake(r: &Repo) -> PathBuf {
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .output()
        .unwrap();
    let said = out(&baked);
    assert!(baked.status.success(), "{said}");
    assert!(
        said.contains("packages: bun: 7 files") && said.contains("by the lockfile"),
        "{said}"
    );
    store
}

// covers: K-6, K-7, K-10, K-20
#[test]
fn a_store_baked_from_bun_lock_installs_with_no_registry_as_the_network_does() {
    let Some(bun) = bun() else {
        return;
    };
    let r = Repo::new(&[
        file("package.json", PACKAGE),
        file("bun.lock", LOCK),
        file(".gitignore", "node_modules\n"),
    ])
    .clear(CLEAR);
    // What an install from the network leaves, under each linker.
    let linkers = ["hoisted", "isolated"];
    let mut from_network = Vec::new();
    for linker in linkers {
        let _ = std::fs::remove_dir_all(r.repo.join("node_modules"));
        let ran = Command::new(&bun)
            .args(["install", "--frozen-lockfile", "--linker", linker])
            .current_dir(&r.repo)
            .env(
                "BUN_INSTALL_CACHE_DIR",
                r.root.join(format!("net-{linker}")),
            )
            .output()
            .unwrap();
        assert!(ran.status.success(), "{}", out(&ran));
        from_network.push(tree(&r.repo));
    }
    // The bake runs no tool: it downloads what bun.lock names, and the rows carry its hashes.
    let store = bake(&r);
    let rows = std::fs::read_to_string(store.join("rows/bun.jsonl")).unwrap();
    assert!(
        rows.contains(&format!("\"locked\":\"{IS_NUMBER}\"")),
        "{rows}"
    );
    assert!(
        rows.contains("\"path\":\"tarballs/chalk@4.1.2.tgz\""),
        "{rows}"
    );
    let install = |cache: &str, lower: &Path, linker: &str| {
        let _ = std::fs::remove_dir_all(r.repo.join("node_modules"));
        r.hushrun()
            .args(["packages", "exec", "--"])
            .arg(&bun)
            .args(["install", "--frozen-lockfile", "--linker", linker])
            .env("BUN_INSTALL_CACHE_DIR", r.root.join(cache))
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .envs(NO_NETWORK.iter().copied())
            .output()
            .unwrap()
    };
    // Unsigned, and no say-so: the lockfile is the evidence. Link for link as from the network.
    for (linker, network) in linkers.iter().zip(&from_network) {
        let seeded = install(&format!("empty-{linker}"), &store, linker);
        let said = out(&seeded);
        assert!(seeded.status.success(), "{said}");
        assert!(said.contains("packages: bun: seeded 7 files"), "{said}");
        assert_eq!(&tree(&r.repo), network, "{linker}");
    }
    assert!(
        r.root
            .join("empty-hoisted/chalk@4.1.2@@@1/package.json")
            .is_file()
    );
    // Without the store the same install fails: the store is what it read.
    assert!(
        !install("none", Path::new(""), "hoisted").status.success(),
        "an install with no store reached a registry"
    );
    // The lockfile changed by one package: the tarball it no longer names is left out, and no
    // stamp is left, so the next lockfile can name it.
    r.write(
        "bun.lock",
        &LOCK.replace(IS_NUMBER, &format!("sha512-{}==", "A".repeat(86))),
    );
    let changed = install("changed", &store, "hoisted");
    let said = out(&changed);
    assert!(said.contains("seeded 6 files"), "{said}");
    assert!(
        said.contains("left out 1 files no lockfile here names now"),
        "{said}"
    );
    r.write("bun.lock", LOCK);
    // One byte of one tarball flipped: the seed catches it, and Bun, with no registry, fails.
    let at = store.join("parts/bun/tarballs/chalk@4.1.2.tgz");
    let mut bytes = std::fs::read(&at).unwrap();
    bytes[20] ^= 1;
    std::fs::write(&at, bytes).unwrap();
    let tampered = install("tampered", &store, "hoisted");
    let said = out(&tampered);
    assert!(
        said.contains(
            "left out 1 files whose bytes changed since the bake, such as tarballs/chalk@4.1.2.tgz"
        ),
        "{said}"
    );
    assert!(!tampered.status.success(), "{said}");
    assert!(!r.root.join("tampered/chalk@4.1.2@@@1").exists());
}

// covers: K-10, K-20
#[test]
fn a_bun_lockb_alone_or_a_lockfile_version_unknown_bakes_nothing_and_says_why() {
    let r = Repo::new(&[file("bun.lockb", "\0binary")]).clear(CLEAR);
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(r.root.join("packages"))
        .output()
        .unwrap();
    let said = out(&baked);
    assert!(!baked.status.success(), "{said}");
    assert!(
        said.contains(
            "packages: bun: left out: bun.lockb is Bun's binary lockfile, which names no hashes"
        ),
        "{said}"
    );
    let r = Repo::new(&[file(
        "bun.lock",
        &LOCK.replace("\"lockfileVersion\": 2", "\"lockfileVersion\": 9"),
    )])
    .clear(CLEAR);
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(r.root.join("packages"))
        .output()
        .unwrap();
    let said = out(&baked);
    assert!(
        said.contains("packages: bun: left out: bun.lock is a version this hushrun doesn't read (lockfileVersion 9)"),
        "{said}"
    );
}

// covers: K-10, K-20
// The keys of §3.3: everything COMMAND sees in a run is the same with and without the store, a
// Node test runner check (whose adapter sets NODE_OPTIONS) run with it is skipped without it, and
// `packages exec`, outside a run, passes on no NODE_OPTIONS hook of hushrun's.
#[test]
fn a_bun_part_gives_a_run_nothing_and_changes_no_key() {
    if bun().is_none() {
        return;
    }
    let r = Repo::new(&[file("bun.lock", LOCK)]).clear(CLEAR);
    let store = bake(&r);
    let cache = r.root.join("seeded");
    let env_of = |lower: &Path| {
        let ran = r
            .hushrun()
            .args(["run", "e", "--force", "--", "sh", "-c", "env | sort"])
            .env("HUSHRUN_BUILD", "on")
            .env("HUSHRUN_PACKAGES_LOWER", lower)
            .env("BUN_INSTALL_CACHE_DIR", &cache)
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
    let exec = r
        .hushrun()
        .args([
            "packages",
            "exec",
            "--",
            "sh",
            "-c",
            "echo \"[${NODE_OPTIONS-}]\"",
        ])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("BUN_INSTALL_CACHE_DIR", &cache)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&exec.stdout).contains("[]"),
        "{}",
        out(&exec)
    );
    for node in nodes() {
        let mut files = node_shop("reuse");
        files.push(file("bun.lock", LOCK));
        let r = fixtures::repo(&node, &files).clear(CLEAR);
        let store = bake(&r);
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
