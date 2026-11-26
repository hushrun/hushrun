//! `hushrun packages` with Hex (K-3, K-6, K-8, K-10, ADR 0157): a store baked by `mix deps.get`
//! holds Hex's package tarballs, and a run seeds them into an empty Hex cache, from which `mix
//! deps.get` unpacks `deps/` and downloads no tarball. Each test serves a Hex repository of its
//! own on 127.0.0.1 (`common/hex.rs`), which writes down every tarball asked of it and can refuse
//! them all: "the network off" for packages. Needs Elixir (`HUSHRUN_TEST_MIX`) and Hex
//! (`HUSHRUN_TEST_HEX`).

#[path = "common/cli.rs"]
mod cli;
#[path = "common/hex.rs"]
mod hex;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::process::Output;

use cli::Cli as _;
use hex::{Hex, Package, Server};
use repo::Repo;

/// A checkout that depends on two packages of the tests' repository, `canary` (whose `mix.exs`
/// and compile each write a file) and `leaf`, with its `mix.lock` committed; the repository's
/// server; and the store a bake writes.
struct World {
    hex: Hex,
    server: Server,
    r: Repo,
    store: PathBuf,
    /// What the canary's `mix.exs` writes when Mix reads it, and what its compile writes.
    canary: [PathBuf; 2],
    _registry: tempfile::TempDir,
}

fn world() -> Option<World> {
    let hex = hex::hex()?;
    let registry = tempfile::tempdir().unwrap();
    let canary = [
        registry.path().join("canary-read"),
        registry.path().join("canary-compiled"),
    ];
    let public = hex::repository(
        &hex,
        registry.path(),
        &[
            Package::canary("canary", &canary[0], &canary[1]),
            Package::plain("leaf", "1.0.0"),
            Package::plain("leaf", "1.1.0"),
        ],
    );
    // `mix hex.build` read the canary's mix.exs; nothing from here on may.
    for f in &canary {
        let _ = std::fs::remove_file(f);
    }
    let server = Server::start(&public);
    let mut r = Repo::new(&[
        file(".gitignore", "/_build/\n/deps/\n"),
        file(
            "mix.exs",
            &hex::mix_exs("shop", &[("canary", "0.1.0"), ("leaf", "1.0.0")]),
        ),
        file(
            "lib/shop.ex",
            "defmodule Shop do\n  def leaf, do: Leaf.version()\nend\n",
        ),
    ])
    .clear(Hex::CLEARED)
    .clear(&["HUSHRUN_PACKAGES_LOWER", "HUSHRUN_PACKAGES", "ERL_LIBS"]);
    for (name, value) in hex.vars(&server) {
        r = r.give(name, value);
    }
    let mut path = vec![hex.mix.parent().unwrap_or(Path::new("")).to_path_buf()];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    r = r.give("PATH", std::env::join_paths(path).unwrap());
    let store = r.root.join("packages");
    let w = World {
        hex,
        server,
        r,
        store,
        canary,
        _registry: registry,
    };
    w.lock();
    Some(w)
}

impl World {
    /// `mix ARGS` in the checkout, with Hex's cache at `home`.
    fn mix(&self, home: &Path, args: &[&str]) -> Output {
        let mut cmd = self.r.command(&self.hex.mix.to_string_lossy());
        cmd.args(args)
            .env("HEX_HOME", home)
            .env("MIX_HOME", self.r.root.join("mix-home"));
        cmd.output().unwrap()
    }

    /// Writes `mix.lock` with a Hex cache of its own, and commits it.
    fn lock(&self) {
        let out = self.mix(&self.r.root.join("lock-home"), &["deps.get"]);
        assert!(out.status.success(), "{}", said(&out));
        let _ = std::fs::remove_dir_all(self.r.repo.join("deps"));
        let _ = std::fs::remove_dir_all(self.r.repo.join("_build"));
        hushrun_testkit::git::commit_all(&self.r.repo, "lock");
        self.server.tarballs_asked();
    }

    /// `hushrun ARGS` in the checkout, reading the store, with Hex's cache at `home`.
    fn hushrun(&self, home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = self.r.hushrun();
        cmd.args(args)
            .env("HUSHRUN_PACKAGES_LOWER", &self.store)
            .env("HEX_HOME", home)
            .env("MIX_HOME", self.r.root.join("mix-home"));
        for (n, v) in env {
            cmd.env(n, v);
        }
        cmd.output().unwrap()
    }

    /// `hushrun packages bake --out STORE --no-sign -- mix deps.get`.
    fn bake(&self, env: &[(&str, &str)]) -> Output {
        let mut cmd = self.r.hushrun();
        cmd.args(["packages", "bake", "--no-sign", "--out"])
            .arg(&self.store)
            .args(["--", "mix", "deps.get"])
            .env("MIX_HOME", self.r.root.join("mix-home"));
        for (n, v) in env {
            cmd.env(n, v);
        }
        cmd.output().unwrap()
    }

    fn part_file(&self, name: &str) -> PathBuf {
        self.store.join("parts/hex/packages/hexpm").join(name)
    }

    /// An empty folder for Hex's cache, and no `deps/` in the checkout.
    fn fresh(&self, name: &str) -> PathBuf {
        let _ = std::fs::remove_dir_all(self.r.repo.join("deps"));
        let _ = std::fs::remove_dir_all(self.r.repo.join("_build"));
        self.r.root.join(name)
    }
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn sha256(path: &Path) -> String {
    hushrun_core::blocks::Hex32::sha256(&std::fs::read(path).unwrap()).to_string()
}

/// Every file under `dir`.
fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    out
}

// covers: K-6, K-10, K-50
#[test]
fn a_baked_hex_part_is_seeded_and_mix_deps_get_downloads_no_tarball() {
    let Some(w) = world() else { return };
    // The bake: `mix deps.get` with Hex's cache, `deps/` and `_build` at folders of the bake's
    // own. A credential in the job's environment reaches no file of the store.
    let secret = "canary-9b1e";
    let baked = w.bake(&[("HUSHRUN_TOKEN", secret), ("SIGSTORE_ID_TOKEN", secret)]);
    let out = said(&baked);
    assert!(baked.status.success(), "{out}");
    assert!(out.contains("packages: hex: 2 files"), "{out}");
    assert!(out.contains("(mix.exs, mix.lock)"), "{out}");
    assert!(
        out.contains("every run reads it, since each file is checked against the lockfile"),
        "{out}"
    );
    assert_eq!(
        w.server.tarballs_asked(),
        ["canary-0.1.0.tar", "leaf-1.0.0.tar"]
    );
    // No dependency's code ran: not its mix.exs, not its compile.
    for f in &w.canary {
        assert!(
            !f.exists(),
            "{} was written by the bake's fetch",
            f.display()
        );
    }
    // The checkout's deps/ and _build are as they were: none.
    assert!(!w.r.repo.join("deps").exists());
    assert!(!w.r.repo.join("_build").exists());
    let mut files: Vec<String> = all_files(&w.store.join("parts/hex"))
        .iter()
        .map(|f| f.strip_prefix(&w.store).unwrap().display().to_string())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "parts/hex/packages/hexpm/canary-0.1.0.tar",
            "parts/hex/packages/hexpm/leaf-1.0.0.tar"
        ],
        "the tarballs and nothing else of Hex's cache"
    );
    for f in all_files(&w.store) {
        let bytes = std::fs::read(&f).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(secret),
            "{} holds a credential",
            f.display()
        );
    }
    // Each row names its package and the lockfile's outer checksum, which is its SHA-256.
    let rows = std::fs::read_to_string(w.store.join("rows/hex.jsonl")).unwrap();
    for (name, package) in [
        ("canary-0.1.0.tar", "canary 0.1.0"),
        ("leaf-1.0.0.tar", "leaf 1.0.0"),
    ] {
        let sha = sha256(&w.part_file(name));
        let row = rows.lines().find(|l| l.contains(name)).unwrap();
        assert!(row.contains(&format!("\"locked\":\"{sha}\"")), "{row}");
        assert!(row.contains(&format!("\"package\":\"{package}\"")), "{row}");
    }
    // An unsigned store of lockfile parts is current, in CI too.
    let home = w.fresh("status-home");
    for ci in ["false", "true"] {
        let status = w.hushrun(&home, &["packages", "status"], &[("CI", ci)]);
        let out = said(&status);
        assert!(
            out.contains(", current (only the parts a lockfile verifies)"),
            "{out}"
        );
    }

    // Tarballs refused: seeded, `mix deps.get` unpacks deps/ and asks for no tarball.
    w.server.refuse_tarballs(true);
    let home = w.fresh("run-home");
    let read = w.hushrun(&home, &["packages", "exec", "--", "mix", "deps.get"], &[]);
    let out = said(&read);
    assert!(read.status.success(), "{out}");
    assert!(
        out.contains(&format!(
            "packages: hex: seeded 2 files into {} (0 there already)",
            home.join("packages/hexpm").display()
        )),
        "{out}"
    );
    assert_eq!(w.server.tarballs_asked(), Vec::<String>::new(), "{out}");
    assert!(w.r.repo.join("deps/leaf/lib/leaf.ex").is_file());
    assert!(w.r.repo.join("deps/canary/mix.exs").is_file());
    // The tarballs are links to the store's own files.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let (a, b) = (
            std::fs::metadata(w.part_file("leaf-1.0.0.tar")).unwrap(),
            std::fs::metadata(home.join("packages/hexpm/leaf-1.0.0.tar")).unwrap(),
        );
        assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
    }
    // The compile then runs as it does today, outside the bake.
    let compiled = w.mix(&home, &["deps.compile"]);
    assert!(compiled.status.success(), "{}", said(&compiled));
    assert!(w.canary[1].exists(), "the dependency's compile ran");
    // The second seed of the same part looks at its stamp and nothing else.
    let stamps: Vec<PathBuf> = std::fs::read_dir(home.join("packages/hexpm"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".hushrun-packages-"))
        })
        .collect();
    assert_eq!(stamps.len(), 1, "{stamps:?}");
    std::fs::remove_file(home.join("packages/hexpm/leaf-1.0.0.tar")).unwrap();
    let again = w.hushrun(&home, &["packages", "restore"], &[]);
    assert!(
        said(&again).contains("packages: hex: seeded already into "),
        "{}",
        said(&again)
    );
    assert!(!home.join("packages/hexpm/leaf-1.0.0.tar").exists());

    // `env` seeds too, and exports nothing. `hushrun run` gives COMMAND nothing for a seed, and
    // the check's key is the same with and without the store: the run without it skips.
    let home = w.fresh("check-home");
    let env = w.hushrun(&home, &["packages", "env"], &[]);
    assert_eq!(String::from_utf8_lossy(&env.stdout), "");
    assert!(
        String::from_utf8_lossy(&env.stderr).contains("packages: hex: seeded 2 files into"),
        "{}",
        said(&env)
    );
    let run = |store: &Path| {
        let mut cmd = w.r.hushrun();
        cmd.args(["run", "t", "--", "mix", "deps.get"])
            .env("HUSHRUN_PACKAGES_LOWER", store)
            .env("HEX_HOME", &home)
            .env("MIX_HOME", w.r.root.join("mix-home"));
        cmd.output().unwrap()
    };
    let first = run(&w.store);
    let out = said(&first);
    assert!(first.status.success(), "{out}");
    assert!(out.contains("t passed"), "{out}");
    assert!(!out.contains("packages: hex: seeded"), "{out}");
    let second = run(Path::new(""));
    assert!(said(&second).contains("t skipped"), "{}", said(&second));

    // Without the store, the same install fails: the tarballs came from the store.
    let home = w.fresh("bare-home");
    let mut cmd = w.r.hushrun();
    cmd.args(["packages", "exec", "--", "mix", "deps.get"])
        .env("HUSHRUN_PACKAGES_LOWER", "")
        .env("HEX_HOME", &home)
        .env("MIX_HOME", w.r.root.join("mix-home"));
    let bare = cmd.output().unwrap();
    assert!(!bare.status.success(), "{}", said(&bare));
    assert!(!w.server.tarballs_asked().is_empty());
}

// covers: K-3, K-10, K-12
#[test]
fn a_lockfile_changed_by_one_package_downloads_that_package_alone() {
    let Some(w) = world() else { return };
    let baked = w.bake(&[]);
    assert!(baked.status.success(), "{}", said(&baked));
    w.server.tarballs_asked();
    w.r.write(
        "mix.exs",
        &hex::mix_exs("shop", &[("canary", "0.1.0"), ("leaf", "1.1.0")]),
    );
    w.lock();
    let home = w.fresh("run-home");
    let read = w.hushrun(&home, &["packages", "exec", "--", "mix", "deps.get"], &[]);
    let out = said(&read);
    assert!(read.status.success(), "{out}");
    assert!(out.contains("packages: hex: seeded 1 files into"), "{out}");
    assert!(
        out.contains("packages: hex: left out 1 files no lockfile here names now"),
        "{out}"
    );
    assert!(
        out.contains("packages: hex: behind: the build files changed"),
        "{out}"
    );
    assert_eq!(w.server.tarballs_asked(), ["leaf-1.1.0.tar"], "{out}");
    assert!(
        !home.join("packages/hexpm/leaf-1.0.0.tar").exists(),
        "a package the lockfile no longer pins stays in the store"
    );
    assert!(
        std::fs::read_dir(home.join("packages/hexpm"))
            .unwrap()
            .flatten()
            .all(|e| !e
                .file_name()
                .to_string_lossy()
                .starts_with(".hushrun-packages-")),
        "no stamp while a file of the part is left out"
    );
}

// covers: K-1, K-8, K-10
#[test]
fn one_byte_changed_is_refused_by_the_seed_found_by_ci_and_by_verify() {
    let Some(w) = world() else { return };
    let baked = w.bake(&[]);
    assert!(baked.status.success(), "{}", said(&baked));
    w.server.tarballs_asked();
    let leaf = w.part_file("leaf-1.0.0.tar");
    let good = std::fs::read(&leaf).unwrap();
    let mut bad = good.clone();
    let at = bad.len() / 2;
    bad[at] ^= 1;
    std::fs::write(&leaf, &bad).unwrap();
    // Outside CI the check by names and sizes passes it; the seed refuses it, with one line, and
    // Hex downloads that one package.
    let home = w.fresh("run-home");
    let read = w.hushrun(&home, &["packages", "exec", "--", "mix", "deps.get"], &[]);
    let out = said(&read);
    assert!(read.status.success(), "{out}");
    assert!(
        out.contains("packages: hex: left out 1 files whose bytes changed since the bake, such as packages/hexpm/leaf-1.0.0.tar: Hex downloads what they were of"),
        "{out}"
    );
    assert!(out.contains("packages: hex: seeded 1 files into"), "{out}");
    assert_eq!(w.server.tarballs_asked(), ["leaf-1.0.0.tar"]);
    assert_eq!(
        std::fs::read(home.join("packages/hexpm/leaf-1.0.0.tar")).unwrap(),
        good
    );
    // In CI the run hashes every file first, and leaves the part out.
    let home = w.fresh("ci-home");
    let read = w.hushrun(
        &home,
        &["packages", "exec", "--", "mix", "deps.get"],
        &[("CI", "true")],
    );
    let out = said(&read);
    assert!(read.status.success(), "{out}");
    assert!(out.contains("packages: hex: ignored: "), "{out}");
    assert!(!out.contains("packages: hex: seeded"), "{out}");
    assert_eq!(
        w.server.tarballs_asked(),
        ["canary-0.1.0.tar", "leaf-1.0.0.tar"]
    );
    // `hushrun packages verify` finds it anywhere.
    let verify = w.hushrun(&home, &["packages", "verify"], &[]);
    assert!(!verify.status.success(), "{}", said(&verify));
    std::fs::write(&leaf, &good).unwrap();
    let verify = w.hushrun(&home, &["packages", "verify"], &[]);
    assert!(verify.status.success(), "{}", said(&verify));
    // A file no row names, or a link out of the part: the store is ignored.
    let status = |w: &World| said(&w.hushrun(&home, &["packages", "status"], &[]));
    let extra = w.part_file("extra-1.0.0.tar");
    std::fs::write(&extra, b"x").unwrap();
    assert!(status(&w).contains("is ignored: "), "{}", status(&w));
    std::fs::remove_file(&extra).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hostname", &extra).unwrap();
        assert!(status(&w).contains("is ignored: "), "{}", status(&w));
        std::fs::remove_file(&extra).unwrap();
    }
    assert!(status(&w).contains(", current ("), "{}", status(&w));
}

// covers: K-3
#[test]
fn a_mix_lock_in_a_form_hushrun_doesnt_read_leaves_the_part_out() {
    let Some(w) = world() else { return };
    let baked = w.bake(&[]);
    assert!(baked.status.success(), "{}", said(&baked));
    // The lockfile of a Hex older than 0.21: six fields, no outer checksum.
    let lock = std::fs::read_to_string(w.r.repo.join("mix.lock")).unwrap();
    let older: String = lock
        .lines()
        .map(|line| match line.find(", \"hexpm\", ") {
            Some(at) => format!("{}}},", &line[..at]),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(older, lock);
    w.r.write("mix.lock", &older);
    hushrun_testkit::git::commit_all(&w.r.repo, "older lock");
    let home = w.fresh("run-home");
    let restore = w.hushrun(&home, &["packages", "restore"], &[]);
    let out = said(&restore);
    assert!(
        out.contains(
            "packages: hex: left alone: mix.lock is in a form this hushrun doesn't read (line "
        ),
        "{out}"
    );
    assert!(!home.join("packages").exists(), "{out}");
    // A bake leaves the part out with the same words, and with no other part has nothing to bake.
    // (A fetch, `mix deps.get`, would write the lockfile anew: this bake takes a filled cache.)
    let rebaked =
        w.r.hushrun()
            .args(["packages", "bake", "--no-sign", "--out"])
            .arg(w.r.root.join("other"))
            .arg("--harvest")
            .arg(format!("hex={}", w.r.root.join("lock-home").display()))
            .output()
            .unwrap();
    let out = said(&rebaked);
    assert!(!rebaked.status.success(), "{out}");
    assert!(
        out.contains("packages: hex: left out: mix.lock is in a form this hushrun doesn't read"),
        "{out}"
    );
}
