//! Baked stores from the command line (B-63, B-64): `hushrun build bake` from this machine's
//! store, and a run on a fresh store that reads the baked folder underneath, read-only.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use hushrun_testkit::world::World;
use serde_json::Value;

/// Where the hosted backend would be: nothing listens on the discard port.
const NO_HOST: &str = "http://127.0.0.1:9";

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn ran(cmd: &mut assert_cmd::Command) -> Ran {
    let output = cmd.output().expect("hushrun");
    Ran {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A machine: its own cache directory (so its own store) and settings, the world's checkout.
struct Machine<'a> {
    w: &'a World,
    cache: PathBuf,
    credentials: PathBuf,
}

impl<'a> Machine<'a> {
    fn new(w: &'a World, name: &str) -> Machine<'a> {
        let machine = Machine {
            w,
            cache: w.root.join(format!("{name}-cache")),
            credentials: w.root.join(format!("{name}-credentials.json")),
        };
        let on = ran(machine.hushrun().args(["build", "on"]));
        assert_eq!(on.code, 0, "{}", on.stderr);
        machine
    }

    fn hushrun(&self) -> assert_cmd::Command {
        let mut cmd = self.w.hushrun();
        cmd.env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", &self.credentials)
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env_remove("HUSHRUN_BUILD_LOWER")
            .env_remove("HUSHRUN_BUILD_LOWER_UNTRUSTED")
            .env_remove("HUSHRUN_BUILD_DIR");
        cmd
    }

    fn store(&self) -> PathBuf {
        self.cache.join("build")
    }

    /// `hushrun run build --force -- cargo build --offline` in the workspace, into `target`.
    fn cargo_build(&self, target: &Path, env: &[(&str, &Path)]) -> Ran {
        let mut cmd = self.hushrun();
        cmd.current_dir(self.w.repo.join("ws"))
            .env("CARGO_TARGET_DIR", target)
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env("CARGO_TERM_COLOR", "never");
        for (name, value) in env {
            cmd.env(name, value);
        }
        ran(cmd.args([
            "run",
            "build",
            "--force",
            "--",
            "cargo",
            "build",
            "--offline",
        ]))
    }
}

/// A workspace of two libraries, `b` depending on `a`, inside the world's checkout.
fn workspace(w: &World) {
    let ws = w.repo.join("ws");
    for (name, body, deps) in [
        ("a", "pub fn one() -> u32 {\n    1\n}\n", ""),
        (
            "b",
            "pub fn two() -> u32 {\n    a::one() + 1\n}\n",
            "a = { path = \"../a\" }\n",
        ),
    ] {
        std::fs::create_dir_all(ws.join(name).join("src")).unwrap();
        std::fs::write(
            ws.join(name).join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
            ),
        )
        .unwrap();
        std::fs::write(ws.join(name).join("src/lib.rs"), body).unwrap();
    }
    // A build script, which cargo starts itself: a reused one must be a program it can start.
    std::fs::write(
        ws.join("a/build.rs"),
        "fn main() {\n    println!(\"cargo::rustc-env=A_BUILT=yes\");\n}\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
    )
    .unwrap();
}

/// From a run's summary, `X of N crates reused (Y from the baked store)`: (X, N, Y).
fn reused_from_baked(stderr: &str) -> Option<(u64, u64, u64)> {
    let line = stderr
        .lines()
        .find(|l| l.contains("from the baked store"))?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "of")?;
    let x = words.get(at.checked_sub(1)?)?.parse().ok()?;
    let n = words.get(at + 1)?.parse().ok()?;
    let y = words.get(at + 4)?.trim_start_matches('(').parse().ok()?;
    Some((x, n, y))
}

/// Every file under `dir` with its size and modification time: what must not change.
fn listing(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                out.push((entry.path(), meta.len(), meta.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn chmod_tree(dir: &Path, writable: bool) {
    let mut stack = vec![dir.to_path_buf()];
    let mut all = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
            all.push(entry.path());
        }
    }
    // Folders last when locking, first when unlocking, so every entry stays reachable.
    if writable {
        all.sort();
    } else {
        all.sort_by(|a, b| b.cmp(a));
    }
    for path in all {
        let meta = std::fs::metadata(&path).unwrap();
        let mode = match (meta.is_dir(), writable) {
            (true, true) => 0o755,
            (true, false) => 0o555,
            (false, true) => 0o644,
            (false, false) => 0o444,
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

// covers: B-63, B-64
#[test]
fn a_store_baked_here_serves_a_fresh_store_from_a_read_only_folder_underneath() {
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let laptop = Machine::new(&w, "laptop");
    let target = w.root.join("target");
    let first = laptop.cargo_build(&target, &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(
        first.stderr.contains("build: 3 crates built"),
        "{}",
        first.stderr
    );

    // Bake it: sealed packs, sealed hints and baked.json, untrusted.
    let baked = w.root.join("baked");
    let bake = ran(laptop
        .hushrun()
        .args(["build", "bake", "--out"])
        .arg(&baked)
        .args(["--check", "build", "--format", "json"]));
    assert_eq!(bake.code, 0, "{}", bake.stderr);
    let said: Value = serde_json::from_str(&bake.stdout).expect("JSON");
    // Both libraries; the build script too where its block may leave the machine (Linux).
    let baked_blocks = said["blocks"].as_u64().unwrap();
    assert!((2..=3).contains(&baked_blocks), "{said}");
    assert_eq!(said["trust"]["level"], "untrusted");
    assert_eq!(said["trust"]["because"], "local-store");
    // As this machine would read it: untrusted, since nothing here accepts that.
    assert_eq!(said["here"], "untrusted");
    let mut names: Vec<String> = std::fs::read_dir(&baked)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["baked.json", "hints", "packs"]);
    let json: Value =
        serde_json::from_slice(&std::fs::read(baked.join("baked.json")).unwrap()).unwrap();
    assert_eq!(json["schema"], "hushrun.baked-store/v1");
    assert_eq!(json["packs"].as_array().unwrap().len() as u64, baked_blocks);
    // B-76: a person's bake names no folder of theirs.
    assert!(json.get("paths").is_none(), "{json}");
    // No pack holds a plain byte of the crate's source.
    for entry in std::fs::read_dir(baked.join("packs")).unwrap().flatten() {
        let bytes = std::fs::read(entry.path()).unwrap();
        assert!(bytes.starts_with(b"HRBP"));
        assert!(!bytes.windows(8).any(|w| w == b"a::one()"));
    }

    // The baked folder becomes read-only, as an image's layer or a shared volume is.
    chmod_tree(&baked, false);
    let before = listing(&baked);

    // A fresh machine with an empty store and an empty target folder, at the same paths.
    let sandbox = Machine::new(&w, "sandbox");
    std::fs::remove_dir_all(&target).unwrap();
    // Untrusted and not accepted: ignored, with one line, and everything compiles.
    let refused = sandbox.cargo_build(&target, &[("HUSHRUN_BUILD_LOWER", &baked)]);
    assert_eq!(refused.code, 0, "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("is ignored: nothing here verifies it"),
        "{}",
        refused.stderr
    );
    assert!(
        refused.stderr.contains("3 crates built, none reused"),
        "{}",
        refused.stderr
    );

    // Accepted: both libraries come from the baked store, and the build script, reused or
    // compiled, starts. (On macOS a build script's block holds the toolchain's place, which is
    // under each store (B-17), so a fresh store compiles it; on Linux it is reused too.)
    let fresh = Machine::new(&w, "fresh");
    std::fs::remove_dir_all(&target).unwrap();
    let one = Path::new("1");
    let reused = fresh.cargo_build(
        &target,
        &[
            ("HUSHRUN_BUILD_LOWER", &baked),
            ("HUSHRUN_BUILD_LOWER_UNTRUSTED", one),
        ],
    );
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    let (x, n, y) = reused_from_baked(&reused.stderr).expect(&reused.stderr);
    assert!(
        x == y && n == 3 && y >= 2 && y <= baked_blocks,
        "{}",
        reused.stderr
    );
    assert!(target.join("debug/libb.rlib").is_file());
    // The blocks went into the fresh machine's own store; the baked folder is as it was.
    assert!(fresh.store().join("blocks").is_dir());
    assert_eq!(listing(&baked), before);

    // `hushrun build status` says how a run here reads it.
    let status = ran(fresh
        .hushrun()
        .env("HUSHRUN_BUILD_LOWER", &baked)
        .env("HUSHRUN_BUILD_LOWER_UNTRUSTED", "1")
        .args(["build", "status", "--format", "json"]));
    assert_eq!(status.code, 0, "{}", status.stderr);
    let status: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(status["baked"]["state"], "current", "{status}");
    assert_eq!(status["baked"]["evidence"], "accepted");

    // `hushrun build clean` empties the fresh machine's own store and never touches the folder
    // underneath.
    let swept = ran(fresh
        .hushrun()
        .env("HUSHRUN_BUILD_LOWER", &baked)
        .env("HUSHRUN_BUILD_LOWER_UNTRUSTED", "1")
        .args(["build", "clean"]));
    assert_eq!(swept.code, 0, "{}", swept.stderr);
    assert_eq!(listing(&baked), before);

    // Baked from a person's store: no folder of theirs, in plain text or as a digest of any
    // kind (nothing in it is derived from a path).
    let text = std::fs::read_to_string(baked.join("baked.json")).unwrap();
    assert!(!text.contains(w.root.to_str().unwrap()), "{text}");
    assert!(!text.contains("binds"), "{text}");

    // The free-space floor is the machine's own store's (B-22): with less free than it, a block
    // only the baked store holds can't be opened into it, and compiles; the folder underneath
    // is as it was.
    let settings = w.root.join("build.json");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    file["min_free_gb"] = 4_000_000_000u64.into();
    std::fs::write(&settings, serde_json::to_vec(&file).unwrap()).unwrap();
    let tight = Machine::new(&w, "tight");
    std::fs::remove_dir_all(&target).unwrap();
    let floor = tight.cargo_build(
        &target,
        &[
            ("HUSHRUN_BUILD_LOWER", &baked),
            ("HUSHRUN_BUILD_LOWER_UNTRUSTED", one),
        ],
    );
    assert_eq!(floor.code, 0, "{}", floor.stderr);
    assert!(
        !floor.stderr.contains("from the baked store"),
        "{}",
        floor.stderr
    );
    assert_eq!(listing(&baked), before);
    chmod_tree(&baked, true);
}

// covers: B-63
#[test]
fn bake_writes_only_into_a_new_or_empty_folder_or_over_a_baked_store() {
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let laptop = Machine::new(&w, "laptop");
    let target = w.root.join("target");
    let built = laptop.cargo_build(&target, &[]);
    assert_eq!(built.code, 0, "{}", built.stderr);
    let bake = |out: &Path| ran(laptop.hushrun().args(["build", "bake", "--out"]).arg(out));
    // A folder with something else in it is refused, and left as it was.
    let busy = w.root.join("busy");
    std::fs::create_dir_all(&busy).unwrap();
    std::fs::write(busy.join("notes.txt"), "mine").unwrap();
    let refused = bake(&busy);
    assert_ne!(refused.code, 0);
    assert!(
        refused
            .stderr
            .contains("holds something other than a baked store"),
        "{}",
        refused.stderr
    );
    assert_eq!(
        std::fs::read_to_string(busy.join("notes.txt")).unwrap(),
        "mine"
    );
    // A new folder, then the same folder again: replaced whole, nothing left beside it but the
    // folder's lock, which stays (removing it could let two bakes hold two locks).
    let out = w.root.join("out");
    assert_eq!(bake(&out).code, 0);
    let again = bake(&out);
    assert_eq!(again.code, 0, "{}", again.stderr);
    let beside: Vec<String> = std::fs::read_dir(&w.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(".out.") && n != ".out.bake.lock")
        .collect();
    assert!(beside.is_empty(), "{beside:?}");
    // `--trusted` is refused for this machine's store.
    let trusted = ran(laptop
        .hushrun()
        .args(["build", "bake", "--trusted", "--out"])
        .arg(w.root.join("t")));
    assert_ne!(trusted.code, 0);
    assert!(
        trusted.stderr.contains("never trusted"),
        "{}",
        trusted.stderr
    );
    // A block the manifest names that is no longer in the store: the store is baked partial
    // and says how many. Routine for a store baked here (blocks get swept), so it exits 0; for
    // an image, which others start from, 11, never a registry's 7.
    let blocks = laptop.store().join("blocks");
    let shard = std::fs::read_dir(&blocks)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let block = std::fs::read_dir(&shard)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    std::fs::remove_dir_all(&block).unwrap();
    let partial = ran(laptop
        .hushrun()
        .args(["build", "bake", "--format", "json", "--out"])
        .arg(w.root.join("partial")));
    assert_eq!(partial.code, 0, "{}", partial.stderr);
    let said: Value = serde_json::from_str(&partial.stdout).unwrap();
    assert_eq!(said["partial"], true, "{said}");
    assert!(said["missing"].as_u64().unwrap() >= 1, "{said}");
    let published = ran(laptop
        .hushrun()
        .args([
            "build",
            "bake",
            "--format",
            "json",
            "--no-sign",
            "--oci-dir",
        ])
        .arg(w.root.join("partial-image")));
    assert_eq!(published.code, 11, "{}", published.stderr);
    // A machine whose runs left no manifest has nothing to bake.
    let empty = Machine::new(&w, "empty");
    let none = ran(empty
        .hushrun()
        .args(["build", "bake", "--out"])
        .arg(w.root.join("n")));
    assert_ne!(none.code, 0);
    assert!(
        none.stderr.contains("has left a manifest"),
        "{}",
        none.stderr
    );
}

// covers: B-64
#[test]
fn a_block_from_an_older_baked_store_is_reused_once_the_new_one_holds_it_too() {
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let laptop = Machine::new(&w, "laptop");
    let target = w.root.join("target");
    assert_eq!(laptop.cargo_build(&target, &[]).code, 0);
    let bake = |out: &Path| {
        let ran = ran(laptop
            .hushrun()
            .args(["build", "bake", "--check", "build", "--out"])
            .arg(out));
        assert_eq!(ran.code, 0, "{}", ran.stderr);
    };
    let old = w.root.join("old");
    bake(&old);

    // A long-lived machine reads the first store.
    let sandbox = Machine::new(&w, "sandbox");
    let one = Path::new("1");
    let reading = |store: &Path| {
        std::fs::remove_dir_all(&target).unwrap();
        sandbox.cargo_build(
            &target,
            &[
                ("HUSHRUN_BUILD_LOWER", store),
                ("HUSHRUN_BUILD_LOWER_UNTRUSTED", one),
            ],
        )
    };
    let first = reading(&old);
    let baked_blocks = |store: &Path| -> usize {
        let json: Value =
            serde_json::from_slice(&std::fs::read(store.join("baked.json")).unwrap()).unwrap();
        json["packs"].as_array().unwrap().len()
    };
    let (x, n, y) = reused_from_baked(&first.stderr).expect(&first.stderr);
    assert!(
        x == y && n == 3 && y >= 2 && y as usize <= baked_blocks(&old),
        "{}",
        first.stderr
    );
    let from_old = y;

    // The workspace gains a crate; the laptop builds and bakes again: another manifest, which
    // holds the first two blocks too.
    let ws = w.repo.join("ws");
    std::fs::create_dir_all(ws.join("c/src")).unwrap();
    std::fs::write(
        ws.join("c/Cargo.toml"),
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("c/src/lib.rs"),
        "pub fn three() -> u32 {\n    3\n}\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    assert_eq!(laptop.cargo_build(&target, &[]).code, 0);
    let new = w.root.join("new");
    bake(&new);
    let old_json: Value =
        serde_json::from_slice(&std::fs::read(old.join("baked.json")).unwrap()).unwrap();
    let new_json: Value =
        serde_json::from_slice(&std::fs::read(new.join("baked.json")).unwrap()).unwrap();
    assert_ne!(old_json["manifest"], new_json["manifest"]);

    // Reading the new store, the blocks the old one gave this machine's store are its too: none
    // compiles again.
    let second = reading(&new);
    assert_eq!(second.code, 0, "{}", second.stderr);
    let (x, n, y) = reused_from_baked(&second.stderr).expect(&second.stderr);
    assert!(
        x == n && n == 4 && y > from_old && y as usize <= baked_blocks(&new),
        "{}",
        second.stderr
    );
}

// covers: B-12
#[test]
fn a_build_script_stored_without_its_run_bit_is_put_back_runnable() {
    // A block an earlier hushrun stored from a pack (which keeps no mode) holds its build script
    // at 0444: a reuse must still give cargo a program it can start.
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let laptop = Machine::new(&w, "laptop");
    let target = w.root.join("target");
    let first = laptop.cargo_build(&target, &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let mut scripts = 0;
    for shard in std::fs::read_dir(laptop.store().join("blocks"))
        .unwrap()
        .flatten()
    {
        for block in std::fs::read_dir(shard.path()).unwrap().flatten() {
            let record: Value =
                serde_json::from_slice(&std::fs::read(block.path().join("block.json")).unwrap())
                    .unwrap();
            if record["description"]["class"] != "build-script" {
                continue;
            }
            for file in std::fs::read_dir(block.path().join("files"))
                .unwrap()
                .flatten()
            {
                std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o444))
                    .unwrap();
                scripts += 1;
            }
        }
    }
    assert_eq!(scripts, 1);
    std::fs::remove_dir_all(&target).unwrap();
    let again = laptop.cargo_build(&target, &[]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(
        again.stderr.contains("3 of 3 crates reused"),
        "{}",
        again.stderr
    );
}

// covers: B-63, B-68
#[test]
fn a_bake_with_no_check_named_holds_every_check_and_each_pack_once() {
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let laptop = Machine::new(&w, "laptop");
    // Two checks: `build` compiles both libraries, `only-a` the first alone, into its own
    // target folder (so its compile of `a` is logged too, from the store).
    let run = |name: &str, target: &Path, extra: &[&str]| {
        let mut cmd = laptop.hushrun();
        cmd.current_dir(w.repo.join("ws"))
            .env("CARGO_TARGET_DIR", target)
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env("CARGO_TERM_COLOR", "never");
        let mut args = vec!["run", name, "--force", "--", "cargo", "build", "--offline"];
        args.extend_from_slice(extra);
        ran(cmd.args(args))
    };
    let built = run("build", &w.root.join("target"), &[]);
    assert_eq!(built.code, 0, "{}", built.stderr);
    let only = run("only-a", &w.root.join("target-a"), &["-p", "a"]);
    assert_eq!(only.code, 0, "{}", only.stderr);

    let bake = |out: &Path, checks: &[&str]| {
        let mut cmd = laptop.hushrun();
        cmd.current_dir(w.repo.join("ws"))
            .args(["build", "bake", "--format", "json", "--out"])
            .arg(out);
        for check in checks {
            cmd.args(["--check", check]);
        }
        let ran = ran(&mut cmd);
        assert_eq!(ran.code, 0, "{}", ran.stderr);
        serde_json::from_str::<Value>(&ran.stdout).unwrap()
    };
    let all = bake(&w.root.join("all"), &[]);
    let json: Value =
        serde_json::from_slice(&std::fs::read(w.root.join("all/baked.json")).unwrap()).unwrap();
    assert_eq!(json["checks"].as_array().unwrap().len(), 2, "{json}");
    // `a` (and its build script) are named by both checks, and go in once.
    let addresses: Vec<&str> = json["packs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["address"].as_str().unwrap())
        .collect();
    let mut unique = addresses.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(addresses.len(), unique.len(), "{json}");
    // Narrowed to one check: that check alone, and another identity.
    let one = bake(&w.root.join("one"), &["only-a"]);
    let one_json: Value =
        serde_json::from_slice(&std::fs::read(w.root.join("one/baked.json")).unwrap()).unwrap();
    assert_eq!(one_json["checks"].as_array().unwrap().len(), 1);
    assert_ne!(one["manifest"], all["manifest"]);
    assert!(
        one["blocks"].as_u64() < all["blocks"].as_u64(),
        "{one} {all}"
    );

    // `build manifest` names the same checks and the same identity a bake writes, from the same
    // function.
    let manifest = ran(laptop
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .args(["build", "manifest", "--json"]));
    assert_eq!(manifest.code, 0, "{}", manifest.stderr);
    let manifest: Value = serde_json::from_str(&manifest.stdout).unwrap();
    assert_eq!(manifest["manifest"], all["manifest"], "{manifest}");
    assert_eq!(manifest["checks"].as_array().unwrap().len(), 2);
    let narrowed = ran(laptop
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .args(["build", "manifest", "--json", "--check", "only-a"]));
    let narrowed: Value = serde_json::from_str(&narrowed.stdout).unwrap();
    assert_eq!(narrowed["manifest"], one["manifest"]);

    // B-63: a store baked from a backend is read only as that backend vouches for its blocks
    // as a run begins. `build status` asks no backend, so it says that this is the backend's to
    // decide, never "untrusted" for a store a run would read.
    let mut from_backend = json.clone();
    from_backend["source"] =
        serde_json::json!({"from": "backend", "host": "https://hushrun.example", "org": "acme"});
    from_backend["trust"] = serde_json::json!({"level": "untrusted", "because": "scope"});
    std::fs::write(
        w.root.join("all/baked.json"),
        serde_json::to_vec(&from_backend).unwrap(),
    )
    .unwrap();
    let status = ran(laptop
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .env("HUSHRUN_BUILD_LOWER", w.root.join("all"))
        .args(["build", "status", "--format", "json"]));
    assert_eq!(status.code, 0, "{}", status.stderr);
    let status: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(status["baked"]["state"], "server-decides", "{status}");
    assert!(
        status["baked"]["words"]
            .as_str()
            .unwrap()
            .contains("https://hushrun.example"),
        "{status}"
    );
}
