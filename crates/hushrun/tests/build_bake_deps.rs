//! `hushrun build bake --deps-only` (B-72) and `--report` (B-74): a dependencies-only build
//! compiles the third-party crates with the keys the real build computes, stores none of the
//! repository's own crates, and its image never sets a full image back; `--report` measures both
//! builds on this machine.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use hushrun_testkit::git;
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

/// A git repository holding one crate, `name`, whose `lib.rs` is `body` and whose build script,
/// when `script`, sets a cfg the library reads: a third-party crate cargo fetches by its URL.
fn remote(root: &Path, name: &str, body: &str, script: bool) -> PathBuf {
    let repo = root.join(format!("remote-{name}"));
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), body).unwrap();
    if script {
        std::fs::write(
            repo.join("build.rs"),
            "fn main() {\n    println!(\"cargo::rustc-check-cfg=cfg(scripted)\");\n    println!(\"cargo::rustc-cfg=scripted\");\n}\n",
        )
        .unwrap();
    }
    git::init(&repo, "sha1");
    git::commit_all(&repo, name);
    repo
}

/// A machine: its own cache directory (so its own store), one cargo home for all.
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
        cmd.current_dir(self.w.repo.join("ws"))
            .env("HUSHRUN_HOST", NO_HOST)
            .env("HUSHRUN_CACHE_DIR", &self.cache)
            .env("HUSHRUN_CREDENTIALS", &self.credentials)
            .env("HUSHRUN_BUILD_AUDIT", "none")
            .env("CARGO_HOME", self.w.root.join("cargo-home"))
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_TERM_COLOR", "never")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("HUSHRUN_BUILD_LOWER")
            .env_remove("HUSHRUN_BUILD_LOWER_UNTRUSTED")
            .env_remove("HUSHRUN_SIGNING_KEY")
            .env_remove("HUSHRUN_SIGNING_KEY_FILE")
            .env_remove("HUSHRUN_BUILD_DIR");
        cmd
    }

    /// The real build: `hushrun run build --force -- cargo build --offline` into `target`.
    fn build(&self, target: &Path, lower: Option<&Path>) -> Ran {
        let mut cmd = self.hushrun();
        cmd.env("CARGO_TARGET_DIR", target);
        if let Some(lower) = lower {
            cmd.env("HUSHRUN_BUILD_LOWER", lower)
                .env("HUSHRUN_BUILD_LOWER_UNTRUSTED", "1");
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

    fn bake(&self, args: &[&str]) -> Ran {
        ran(self
            .hushrun()
            .args(["build", "bake", "--check", "build"])
            .args(args))
    }
}

/// A workspace of the world's checkout: `core` (a library on the git crate `remote`) and `app` (a
/// program with a build script, on `core` and the git crate `scripty`, whose build script sets a
/// cfg), and its lockfile, the git crates fetched into the cargo home.
fn workspace(w: &World) {
    let far = remote(
        &w.root,
        "remote",
        "pub fn far() -> u32 {\n    7\n}\n",
        false,
    );
    let scripty = remote(
        &w.root,
        "scripty",
        "#[cfg(scripted)]\npub fn made() -> bool {\n    true\n}\n",
        true,
    );
    let ws = w.repo.join("ws");
    std::fs::create_dir_all(ws.join("core/src")).unwrap();
    std::fs::create_dir_all(ws.join("app/src")).unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"core\", \"app\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("core/Cargo.toml"),
        format!(
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nremote = {{ git = \"file://{}\" }}\n",
            far.display()
        ),
    )
    .unwrap();
    std::fs::write(
        ws.join("core/src/lib.rs"),
        "pub fn value() -> u32 {\n    remote::far() + 1\n}\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("app/Cargo.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncore-lib = {{ path = \"../core\" }}\nscripty = {{ git = \"file://{}\" }}\n",
            scripty.display()
        ),
    )
    .unwrap();
    std::fs::write(
        ws.join("app/build.rs"),
        "fn main() {\n    println!(\"cargo::rerun-if-changed=build.rs\");\n}\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("app/src/main.rs"),
        "fn main() {\n    assert!(scripty::made());\n    println!(\"{}\", core_lib::value());\n}\n",
    )
    .unwrap();
    let lock = std::process::Command::new("cargo")
        .args(["generate-lockfile", "-q"])
        .current_dir(&ws)
        .env("CARGO_HOME", w.root.join("cargo-home"))
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    w.write(".gitignore", "target/\n");
    w.commit("workspace");
}

fn rows(store: &Path) -> Vec<String> {
    let baked: Value =
        serde_json::from_slice(&std::fs::read(store.join("baked.json")).unwrap()).unwrap();
    let mut out: Vec<String> = baked["packs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["address"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

// covers: B-72, B-74
// The dependencies-only build compiles the git crates (one with a build script) as the real
// build does, while `core-lib` and `app` (and app's build script) are stubs: its store holds the
// third-party blocks alone, each under the very key the real build computed, and a fresh machine
// building the real workspace reuses every one of them from it.
#[test]
fn a_dependencies_only_store_holds_the_real_build_s_third_party_blocks_and_nothing_else() {
    let w = World::new("acme-api.yaml");
    workspace(&w);

    // The real build, on one machine, baked from its store.
    let ci = Machine::new(&w, "ci");
    let built = ci.build(&w.root.join("ci-target"), None);
    assert_eq!(built.code, 0, "{}", built.stderr);
    let real = w.root.join("real");
    let baked = ci.bake(&["--out", real.to_str().unwrap()]);
    assert_eq!(baked.code, 0, "{}", baked.stderr);
    let real_rows = rows(&real);

    // The dependencies-only build, on another machine whose store holds nothing.
    let laptop = Machine::new(&w, "laptop");
    let deps = w.root.join("deps");
    let said = laptop.bake(&[
        "--deps-only",
        "--out",
        deps.to_str().unwrap(),
        "--format",
        "json",
        "--",
        "cargo",
        "build",
        "--offline",
    ]);
    assert_eq!(said.code, 0, "{}\n{}", said.stdout, said.stderr);
    let said: Value = serde_json::from_str(&said.stdout).unwrap();
    assert_eq!(said["deps_only"]["stubbed"], 2, "{said}");
    let deps_rows = rows(&deps);
    // remote, scripty and scripty's build script: three compiles kept, each baked unless it may
    // not leave the machine (a build script's program can hold the toolchain's folder), each one
    // of the real build's blocks.
    assert_eq!(said["deps_only"]["stored"], 3, "{said}");
    let left_out: u64 = said["left_out"]
        .as_object()
        .unwrap()
        .values()
        .map(|n| n.as_u64().unwrap())
        .sum();
    assert_eq!(deps_rows.len() as u64 + left_out, 3, "{said}");
    assert!(deps_rows.len() >= 2, "{said}");
    for row in &deps_rows {
        assert!(
            real_rows.contains(row),
            "{row} isn't a key the real build computed"
        );
    }
    assert!(real_rows.len() > deps_rows.len());
    // Its own store held nothing of the repository's own: the person's store is untouched.
    assert!(!laptop.cache.join("build/manifests").exists());

    // A fresh machine, reading the dependencies-only store, builds the real workspace and reuses
    // all three from it; its own crates compile.
    let fresh = Machine::new(&w, "fresh");
    let warm = fresh.build(&w.root.join("fresh-target"), Some(&deps));
    assert_eq!(warm.code, 0, "{}", warm.stderr);
    assert!(
        warm.stderr
            .contains(&format!("({} from the baked store)", deps_rows.len())),
        "{}",
        warm.stderr
    );

    // --report measures the dependencies' build cold (the build itself) and from the store.
    let report = w.root.join("report");
    let said = laptop.bake(&[
        "--deps-only",
        "--report",
        "--out",
        report.to_str().unwrap(),
        "--",
        "cargo",
        "build",
        "--offline",
    ]);
    assert_eq!(said.code, 0, "{}\n{}", said.stdout, said.stderr);
    assert!(
        said.stdout
            .contains("Measured on this machine: `cargo build --offline` took ")
            && said.stdout.contains(&format!("{} of ", deps_rows.len()))
            && said
                .stdout
                .contains("as stubs: this is the dependencies' share"),
        "{}",
        said.stdout
    );
    // A full bake's report times COMMAND cold, then from its store.
    let said = ci.bake(&[
        "--report",
        "--out",
        w.root.join("full").to_str().unwrap(),
        "--format",
        "json",
        "--",
        "cargo",
        "build",
        "--offline",
    ]);
    assert_eq!(said.code, 0, "{}\n{}", said.stdout, said.stderr);
    let said: Value = serde_json::from_str(&said.stdout).unwrap();
    let report = &said["report"];
    assert_eq!(report["stubbed"], false, "{said}");
    assert_eq!(report["warm_from_baked"], said["blocks"], "{said}");
    assert!(report["cold_ms"].as_u64().unwrap() > 0);
}

// covers: B-72, B-66
// A dependencies-only image takes its fixed tag (its manifest names the dependencies alone, so
// the tag never names a full store missing blocks) and the tags it moves, over nothing or another
// dependencies-only image; over a full image it builds and publishes nothing; a full bake
// replaces it.
#[test]
fn a_dependencies_only_image_never_sets_a_full_image_back() {
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let ci = Machine::new(&w, "ci");
    assert_eq!(ci.build(&w.root.join("ci-target"), None).code, 0);
    let layout = w.root.join("layout");
    let deps_bake = |m: &Machine<'_>| {
        m.bake(&[
            "--deps-only",
            "--branch-tag",
            "--force",
            "--oci-dir",
            layout.to_str().unwrap(),
            "--format",
            "json",
            "--",
            "cargo",
            "build",
            "--offline",
        ])
    };
    let first = deps_bake(&ci);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let first: Value = serde_json::from_str(&first.stdout).unwrap();
    let tags = first["image"]["tags"].to_string();
    assert!(
        tags.contains("-branch-") && first["image"]["tags"].as_array().unwrap().len() == 2,
        "{first}"
    );
    // Over a dependencies-only image, another one publishes.
    let again = deps_bake(&ci);
    assert_eq!(again.code, 0, "{}", again.stderr);
    let again: Value = serde_json::from_str(&again.stdout).unwrap();
    assert_eq!(again["image"]["state"], "published", "{again}");
    // A full bake replaces it.
    let full = ci.bake(&[
        "--branch-tag",
        "--oci-dir",
        layout.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(full.code, 0, "{}", full.stderr);
    let full: Value = serde_json::from_str(&full.stdout).unwrap();
    assert_eq!(full["image"]["state"], "published", "{full}");
    // Now a dependencies-only bake builds nothing and publishes nothing, and says so.
    let late = deps_bake(&ci);
    assert_eq!(late.code, 0, "{}", late.stderr);
    assert!(
        !late.stderr.contains("building the dependencies"),
        "{}",
        late.stderr
    );
    let late: Value = serde_json::from_str(&late.stdout).unwrap();
    assert_eq!(late["image"]["state"], "skipped", "{late}");
    assert!(
        late["image"]["skipped"]
            .as_str()
            .unwrap()
            .contains("full image"),
        "{late}"
    );
}

// covers: B-72
#[test]
fn a_dependencies_only_bake_needs_the_check_s_cargo_command_and_its_lockfile() {
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let m = Machine::new(&w, "m");
    let out = w.root.join("out");
    let said = m.bake(&["--deps-only", "--out", out.to_str().unwrap()]);
    assert_eq!(said.code, 2, "{}", said.stderr);
    assert!(
        said.stderr.contains("needs the check's cargo command"),
        "{}",
        said.stderr
    );
    let said = m.bake(&["--out", out.to_str().unwrap(), "--", "cargo", "build"]);
    assert_eq!(said.code, 2, "{}", said.stderr);
    assert!(
        said.stderr.contains("goes with --deps-only or --report"),
        "{}",
        said.stderr
    );
    let said = m.bake(&[
        "--deps-only",
        "--from",
        "backend",
        "--out",
        out.to_str().unwrap(),
        "--",
        "cargo",
        "build",
    ]);
    assert_eq!(said.code, 2, "{}", said.stderr);
    std::fs::remove_file(w.repo.join("ws/Cargo.lock")).unwrap();
    let said = m.bake(&[
        "--deps-only",
        "--out",
        out.to_str().unwrap(),
        "--",
        "cargo",
        "build",
    ]);
    assert_eq!(said.code, 2, "{}", said.stderr);
    assert!(said.stderr.contains("reads Cargo.lock"), "{}", said.stderr);
}

// covers: B-72
// The documented COMMAND, `cargo test`, ran rustdoc's doctests against the stubbed rlib of the
// repository's own crate, so a dependencies-only bake failed on any doctest: it compiles the
// tests and runs none now.
#[test]
fn a_dependencies_only_bake_of_cargo_test_survives_a_doctest() {
    let w = World::new("acme-api.yaml");
    workspace(&w);
    std::fs::write(
        w.repo.join("ws/core/src/lib.rs"),
        "/// One more than far.\n///\n/// ```\n/// assert_eq!(core_lib::value(), 8);\n/// ```\npub fn value() -> u32 {\n    remote::far() + 1\n}\n",
    )
    .unwrap();
    w.commit("a doctest");
    let laptop = Machine::new(&w, "laptop");
    let deps = w.root.join("deps");
    let said = laptop.bake(&[
        "--deps-only",
        "--out",
        deps.to_str().unwrap(),
        "--",
        "cargo",
        "test",
        "--offline",
    ]);
    assert_eq!(said.code, 0, "{}\n{}", said.stdout, said.stderr);
}
