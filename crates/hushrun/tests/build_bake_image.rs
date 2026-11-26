//! A baked store as an image (B-65 to B-67): `hushrun build bake --oci-dir` writes a layered,
//! signed OCI image layout; a code change adds only the recent layer; a fresh machine that copies
//! the image's `/hushrun` reuses its blocks on the signature its repository names, and a store
//! changed after signing is trusted for nothing it changed.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hushrun_attest::testing::github_claims;
use hushrun_testkit::sigstore::FakeSigstore;
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
            .env("CARGO_INCREMENTAL", "0")
            .env_remove("HUSHRUN_BUILD_LOWER")
            .env_remove("HUSHRUN_BUILD_LOWER_UNTRUSTED")
            .env_remove("HUSHRUN_SIGNING_KEY")
            .env_remove("HUSHRUN_SIGNING_KEY_FILE")
            .env_remove("HUSHRUN_BUILD_DIR");
        cmd
    }

    /// `hushrun run build --force -- cargo build --offline` in the workspace, into `target`.
    fn cargo_build(&self, target: &Path, lower: Option<&Path>) -> Ran {
        let mut cmd = self.hushrun();
        cmd.current_dir(self.w.repo.join("ws"))
            .env("CARGO_TARGET_DIR", target)
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env("CARGO_TERM_COLOR", "never");
        if let Some(lower) = lower {
            cmd.env("HUSHRUN_BUILD_LOWER", lower);
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

    /// A bake of check `build` into `layout`, signed with `key`, moving the branch's tag, past the
    /// re-bake limit (B-73), which its own test exercises.
    fn bake(&self, layout: &Path, key: &Path) -> Value {
        self.bake_args(layout, key, &["--force"])
    }

    fn bake_args(&self, layout: &Path, key: &Path, args: &[&str]) -> Value {
        let bake = ran(self
            .hushrun()
            .current_dir(self.w.repo.join("ws"))
            .args([
                "build",
                "bake",
                "--check",
                "build",
                "--branch-tag",
                "--oci-dir",
            ])
            .arg(layout)
            .arg("--signing-key")
            .arg(key)
            .args(args)
            .args(["--format", "json"]));
        assert_eq!(bake.code, 0, "{}", bake.stderr);
        serde_json::from_str(&bake.stdout).expect("JSON")
    }
}

/// A workspace of four libraries, `b` depending on `a`, `c` and `d` on nothing, inside the
/// world's checkout.
fn workspace(w: &World) {
    let ws = w.repo.join("ws");
    for (name, body, deps) in [
        ("a", "pub fn one() -> u32 {\n    1\n}\n", ""),
        (
            "b",
            "pub fn two() -> u32 {\n    a::one() + 1\n}\n",
            "a = { path = \"../a\" }\n",
        ),
        ("c", "pub fn three() -> u32 {\n    3\n}\n", ""),
        ("d", "pub fn four() -> u32 {\n    4\n}\n", ""),
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
    std::fs::write(
        ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\", \"c\", \"d\"]\nresolver = \"2\"\n",
    )
    .unwrap();
}

/// The layers of the image the layout's moving tag names, unpacked in order into `root`, as
/// `COPY --from` copies them: a reader of exactly the ustar B-65 writes.
fn unpack_latest(layout: &Path, root: &Path) -> Value {
    let index: Value =
        serde_json::from_slice(&std::fs::read(layout.join("index.json")).unwrap()).unwrap();
    let digest = index["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["annotations"]["org.opencontainers.image.ref.name"]
                .as_str()
                .is_some_and(|t| t.ends_with("-latest"))
        })
        .unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_string();
    let blob = |d: &str| std::fs::read(layout.join("blobs/sha256").join(&d[7..])).unwrap();
    let manifest: Value = serde_json::from_slice(&blob(&digest)).unwrap();
    for layer in manifest["layers"].as_array().unwrap() {
        let bytes = blob(layer["digest"].as_str().unwrap());
        let mut at = 0;
        while at + 512 <= bytes.len() && bytes[at..at + 512].iter().any(|&b| b != 0) {
            let h = &bytes[at..at + 512];
            let field = |from: usize, len: usize| {
                let raw = &h[from..from + len];
                let end = raw.iter().position(|&b| b == 0).unwrap_or(len);
                String::from_utf8(raw[..end].to_vec()).unwrap()
            };
            let (name, prefix) = (field(0, 100), field(345, 155));
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let size = u64::from_str_radix(field(124, 11).trim(), 8).unwrap() as usize;
            at += 512;
            if h[156] == b'5' {
                std::fs::create_dir_all(root.join(path.trim_end_matches('/'))).unwrap();
            } else {
                std::fs::write(root.join(&path), &bytes[at..at + size]).unwrap();
                at += size.div_ceil(512) * 512;
            }
        }
    }
    manifest
}

fn uploaded(said: &Value) -> Vec<String> {
    said["image"]["blobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["uploaded"] == true)
        .map(|b| b["role"].as_str().unwrap().to_string())
        .collect()
}

// covers: B-65, B-66, B-67, B-68
#[test]
fn a_signed_image_serves_a_fresh_machine_and_a_change_after_signing_is_not_trusted() {
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let laptop = Machine::new(&w, "laptop");
    let key = w.root.join("key.pem");
    let made = ran(laptop
        .hushrun()
        .args(["keys", "generate", "--algorithm", "ecdsa-p256", "--out"])
        .arg(&key));
    assert_eq!(made.code, 0, "{}", made.stderr);
    let public = ran(laptop.hushrun().args(["keys", "public", "--key"]).arg(&key));
    let public = public
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("public key: "))
        .expect(&public.stdout)
        .to_string();
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write(
        "hushrun.yaml",
        &format!("{policy}\nbuild:\n  baked:\n    signers:\n      - key: {public}\n"),
    );
    w.commit("signers");
    let target = w.root.join("target");
    let first = laptop.cargo_build(&target, None);
    assert_eq!(first.code, 0, "{}", first.stderr);

    // Bake into an image layout, signed with the key.
    let layout = w.root.join("layout");
    let said = laptop.bake(&layout, &key);
    let image = &said["image"];
    assert!(image["signed"].as_str().unwrap().starts_with("key sha256:"));
    let tags: Vec<&str> = image["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    let platform = said["platform"].as_str().unwrap();
    let manifest = said["manifest"].as_str().unwrap();
    // Outside CI on the default branch the moving tag stays where it is; the branch's tag,
    // asked for, moves.
    assert_eq!(tags.len(), 2, "{tags:?}");
    assert_eq!(tags[0], format!("hushrun-{platform}-{}", &manifest[..16]));
    assert!(
        tags[1].starts_with(&format!("hushrun-{platform}-branch-")) && tags[1].ends_with("-latest"),
        "{tags:?}"
    );
    assert!(
        image["notes"].as_array().unwrap().iter().any(|n| n
            .as_str()
            .unwrap()
            .contains("moves only from CI on the default branch")),
        "{image}"
    );
    // Four crates of the workspace's own: no deps layer, a workspace layer, a recent one.
    let roles: Vec<&str> = image["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["workspace", "recent"], "{image}");

    // `hushrun build manifest --json` names the same identity and tags.
    let watched = ran(laptop
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .args(["build", "manifest", "--check", "build", "--json"]));
    assert_eq!(watched.code, 0, "{}", watched.stderr);
    let watched: Value = serde_json::from_str(&watched.stdout).unwrap();
    assert_eq!(watched["schema"], "hushrun.build-manifest/v1");
    assert_eq!(watched["manifest"], manifest);
    assert_eq!(watched["tags"]["fixed"], tags[0]);
    assert_eq!(
        watched["tags"]["latest"],
        format!("hushrun-{platform}-latest")
    );

    // A code change: the image it builds on is signed by the key hushrun.yaml names, but a store
    // baked from this machine carries no block it no longer uses (no backend vouches for it
    // now), so the workspace layer is written again.
    std::fs::write(
        w.repo.join("ws/b/src/lib.rs"),
        "pub fn two() -> u32 {\n    a::one() + 2\n}\n",
    )
    .unwrap();
    let second = laptop.cargo_build(&target, None);
    assert_eq!(second.code, 0, "{}", second.stderr);
    let again = laptop.bake(&layout, &key);
    assert_ne!(again["manifest"], said["manifest"]);
    let blobs = uploaded(&again);
    assert!(blobs.contains(&"recent".to_string()), "{blobs:?}");
    assert!(
        again["image"]["compacted"]
            .as_str()
            .unwrap()
            .contains("carries none"),
        "{again}"
    );
    assert_eq!(again["image"]["stale"], 0, "{again}");
    // A third bake of the same code, with nothing stale: the workspace layer is kept, and the
    // fixed tag, which names this complete store already, isn't moved: nothing is uploaded.
    let third = laptop.bake(&layout, &key);
    assert!(uploaded(&third).is_empty(), "{third}");
    assert_eq!(third["image"]["digest"], again["image"]["digest"]);

    // A fresh machine copies the image's /hushrun and reuses it on the signature alone: no
    // say-so, no backend.
    let root = w.root.join("root");
    std::fs::create_dir_all(&root).unwrap();
    unpack_latest(&layout, &root);
    let baked = root.join("hushrun");
    let fresh = Machine::new(&w, "fresh");
    std::fs::remove_dir_all(&target).unwrap();
    let reused = fresh.cargo_build(&target, Some(&baked));
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert!(
        reused
            .stderr
            .contains("4 of 4 crates reused (4 from the baked store)"),
        "{}",
        reused.stderr
    );
    let status = ran(fresh
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .env("HUSHRUN_BUILD_LOWER", &baked)
        .args(["build", "status", "--format", "json"]));
    let status: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(status["baked"]["state"], "current", "{status}");
    assert_eq!(status["baked"]["evidence"], "signed");

    // B-67: once hushrun.yaml says no store baked before a time counts, this one, replayed with
    // no backend to say it was replaced, is no evidence.
    let signed_policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write(
        "hushrun.yaml",
        &format!("{signed_policy}    not_before: 2099-01-01T00:00:00Z\n"),
    );
    w.commit("not before");
    let status = ran(fresh
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .env("HUSHRUN_BUILD_LOWER", &baked)
        .args(["build", "status", "--format", "json"]));
    let status: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(status["baked"]["state"], "untrusted", "{status}");
    assert!(
        status["baked_signature"]
            .as_str()
            .unwrap()
            .contains("not_before"),
        "{status}"
    );
    w.write("hushrun.yaml", &signed_policy);
    w.commit("any time");

    // baked.json changed after signing: no evidence, said, and everything compiles.
    let json = std::fs::read(baked.join("baked.json")).unwrap();
    let mut changed: Value = serde_json::from_slice(&json).unwrap();
    changed["toolchains"]
        .as_array_mut()
        .unwrap()
        .push("rustc 9.9.9".into());
    std::fs::write(
        baked.join("baked.json"),
        serde_json::to_vec_pretty(&changed).unwrap(),
    )
    .unwrap();
    let other = Machine::new(&w, "other");
    std::fs::remove_dir_all(&target).unwrap();
    let refused = other.cargo_build(&target, Some(&baked));
    assert_eq!(refused.code, 0, "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("signature isn't evidence here: it was made for another baked.json"),
        "{}",
        refused.stderr
    );
    assert!(
        refused.stderr.contains("4 crates built, none reused"),
        "{}",
        refused.stderr
    );
    std::fs::write(baked.join("baked.json"), &json).unwrap();

    // A pack changed after signing: baked.json still verifies, so the store is read, and that
    // pack is refused for its SHA-256 and compiled; the other is reused.
    let packs: Vec<PathBuf> = std::fs::read_dir(baked.join("packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let rows: Value = serde_json::from_slice(&json).unwrap();
    let a_crate = rows["packs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["cost_ms"].is_u64())
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_string();
    let victim = packs
        .iter()
        .find(|p| p.to_string_lossy().contains(&a_crate))
        .unwrap();
    let mut bytes = std::fs::read(victim).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(victim, bytes).unwrap();
    let last_machine = Machine::new(&w, "last");
    std::fs::remove_dir_all(&target).unwrap();
    let partly = last_machine.cargo_build(&target, Some(&baked));
    assert_eq!(partly.code, 0, "{}", partly.stderr);
    assert!(
        !partly.stderr.contains("4 of 4 crates reused"),
        "{}",
        partly.stderr
    );
    assert!(target.join("debug/libb.rlib").is_file());
}

// covers: B-67
#[test]
fn a_keyless_signature_counts_only_for_the_identity_hushrun_yaml_names() {
    let fake = FakeSigstore::start();
    fake.set_claims(github_claims(
        "Acme/api",
        "123456",
        "refs/heads/main",
        ".github/workflows/bake.yml",
        None,
    ));
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let laptop = Machine::new(&w, "laptop");
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let named = |subject: &str| {
        w.write(
            "hushrun.yaml",
            &format!(
                "{policy}\nbuild:\n  baked:\n    signers:\n      - issuer: https://token.actions.githubusercontent.com\n        subject: {subject}\n"
            ),
        );
        w.commit("signers");
    };
    named("https://github.com/Acme/api/.github/workflows/bake.yml@refs/heads/main");
    let target = w.root.join("target");
    let first = laptop.cargo_build(&target, None);
    assert_eq!(first.code, 0, "{}", first.stderr);
    // Signed keylessly with the ID token at hand, a certificate and a timestamp, no key: this
    // isn't CI, so only because HUSHRUN_SIGNING=sigstore asks.
    let layout = w.root.join("layout");
    let bake = ran(laptop
        .hushrun()
        .envs(fake.env(&w.root))
        .current_dir(w.repo.join("ws"))
        .args([
            "build",
            "bake",
            "--check",
            "build",
            "--branch-tag",
            "--oci-dir",
        ])
        .arg(&layout)
        .args(["--format", "json"]));
    assert_eq!(bake.code, 0, "{}", bake.stderr);
    let said: Value = serde_json::from_str(&bake.stdout).unwrap();
    assert!(said["image"]["signed"].is_string(), "{said}");
    let root = w.root.join("root");
    std::fs::create_dir_all(&root).unwrap();
    unpack_latest(&layout, &root);
    let baked = root.join("hushrun");
    let bundle: Value =
        serde_json::from_slice(&std::fs::read(baked.join("baked.sigstore.json")).unwrap()).unwrap();
    assert!(bundle["verificationMaterial"]["certificate"]["rawBytes"].is_string());
    let trusted_root = fake.write_trusted_root(&w.root);
    let fresh = Machine::new(&w, "fresh");
    std::fs::remove_dir_all(&target).unwrap();
    let mut cmd = fresh.hushrun();
    cmd.env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &trusted_root);
    let status = |cmd: &mut assert_cmd::Command| {
        let out = ran(cmd
            .current_dir(w.repo.join("ws"))
            .env("HUSHRUN_BUILD_LOWER", &baked)
            .args(["build", "status", "--format", "json"]));
        serde_json::from_str::<Value>(&out.stdout).unwrap()
    };
    let read = status(&mut cmd);
    assert_eq!(read["baked"]["evidence"], "signed", "{read}");
    assert!(
        read["baked"]["words"]
            .as_str()
            .unwrap()
            .contains("bake.yml@refs/heads/main"),
        "{read}"
    );
    // The same certificate, and hushrun.yaml naming the workflow on another branch: no evidence.
    named("https://github.com/Acme/api/.github/workflows/bake.yml@refs/heads/feature");
    let mut cmd = fresh.hushrun();
    cmd.env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &trusted_root);
    let refused = status(&mut cmd);
    assert_eq!(refused["baked"]["state"], "untrusted", "{refused}");
    assert!(
        refused["baked_signature"]
            .as_str()
            .unwrap()
            .contains("which hushrun.yaml doesn't name"),
        "{refused}"
    );
    // And with no trust root that issued its certificate (the binary's own, public Sigstore's),
    // none either.
    named("https://github.com/Acme/api/.github/workflows/bake.yml@refs/heads/main");
    let mut cmd = fresh.hushrun();
    cmd.env_remove("HUSHRUN_SIGSTORE_TRUSTED_ROOT");
    let other_root = status(&mut cmd);
    assert_eq!(other_root["baked"]["state"], "untrusted", "{other_root}");

    // The named workflow on the named branch, started by an issue comment (whose text anyone
    // writes): no evidence. Only a push, a schedule, a run by hand or the bake job's trigger
    // signs.
    let mut claims = github_claims(
        "Acme/api",
        "123456",
        "refs/heads/main",
        ".github/workflows/bake.yml",
        None,
    );
    claims["event_name"] = "issue_comment".into();
    fake.set_claims(claims);
    let commented = w.root.join("commented");
    let bake = ran(laptop
        .hushrun()
        .envs(fake.env(&w.root))
        .current_dir(w.repo.join("ws"))
        .args([
            "build",
            "bake",
            "--check",
            "build",
            "--branch-tag",
            "--oci-dir",
        ])
        .arg(&commented)
        .args(["--format", "json"]));
    assert_eq!(bake.code, 0, "{}", bake.stderr);
    let root = w.root.join("commented-root");
    std::fs::create_dir_all(&root).unwrap();
    unpack_latest(&commented, &root);
    let mut cmd = fresh.hushrun();
    cmd.env("HUSHRUN_SIGSTORE_TRUSTED_ROOT", &trusted_root);
    let out = ran(cmd
        .current_dir(w.repo.join("ws"))
        .env("HUSHRUN_BUILD_LOWER", root.join("hushrun"))
        .args(["build", "status", "--format", "json"]));
    let refused: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(refused["baked"]["state"], "untrusted", "{refused}");
    assert!(
        refused["baked_signature"]
            .as_str()
            .unwrap()
            .contains("started by issue_comment"),
        "{refused}"
    );
}

/// A machine set up as the first test does: a key, hushrun.yaml naming it, one build.
fn signed_world(w: &World) -> (Machine<'_>, PathBuf, PathBuf) {
    workspace(w);
    let laptop = Machine::new(w, "laptop");
    let key = w.root.join("key.pem");
    let made = ran(laptop
        .hushrun()
        .args(["keys", "generate", "--algorithm", "ecdsa-p256", "--out"])
        .arg(&key));
    assert_eq!(made.code, 0, "{}", made.stderr);
    let public = ran(laptop.hushrun().args(["keys", "public", "--key"]).arg(&key));
    let public = public
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("public key: "))
        .expect(&public.stdout)
        .to_string();
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write(
        "hushrun.yaml",
        &format!("{policy}\nbuild:\n  baked:\n    signers:\n      - key: {public}\n"),
    );
    w.commit("signers");
    let target = w.root.join("target");
    let first = laptop.cargo_build(&target, None);
    assert_eq!(first.code, 0, "{}", first.stderr);
    (laptop, key, target)
}

fn tag_digest(layout: &Path, suffix: &str) -> String {
    let index: Value =
        serde_json::from_slice(&std::fs::read(layout.join("index.json")).unwrap()).unwrap();
    index["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["annotations"]["org.opencontainers.image.ref.name"]
                .as_str()
                .is_some_and(|t| t.starts_with("hushrun-") && t.ends_with(suffix))
        })
        .unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_string()
}

// covers: B-66
// Two bakes of the same manifest (a docs-only merge, a re-run, a run by hand): the fixed tag
// names the first image for good, and the second uploads nothing.
#[test]
fn the_fixed_tag_never_moves_on_a_second_bake_of_the_same_manifest() {
    let w = World::new("acme-api.yaml");
    let (laptop, key, _) = signed_world(&w);
    let layout = w.root.join("layout");
    let one = laptop.bake(&layout, &key);
    let fixed = one["image"]["tags"][0].as_str().unwrap().to_string();
    let before = tag_digest(&layout, &fixed["hushrun-".len()..]);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let two = laptop.bake(&layout, &key);
    assert_eq!(two["manifest"], one["manifest"]);
    assert_eq!(two["image"]["tags"][0], one["image"]["tags"][0]);
    let after = tag_digest(&layout, &fixed["hushrun-".len()..]);
    assert_eq!(before, after, "the fixed tag moved");
    assert!(uploaded(&two).is_empty(), "{two}");
}

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

// covers: B-65, B-67
// Whoever can move a moving tag (a job with the registry's push right, the registry itself)
// rewrites the image it names: a pack of theirs in the workspace layer, listed in the label, with
// a row. The next bake builds on no image its repository's signers didn't sign: it writes the
// workspace layer again, and neither the row nor the pack reaches the image it signs.
#[test]
fn a_tampered_previous_image_gives_the_next_bake_nothing_of_its_own() {
    let w = World::new("acme-api.yaml");
    let (laptop, key, target) = signed_world(&w);
    let layout = w.root.join("layout");
    let _first = laptop.bake(&layout, &key);

    // The attacker: rewrite the moving tag's image. The workspace layer gains a pack of theirs,
    // the label lists it and carries a row for it.
    let blob_path = |d: &str| layout.join("blobs/sha256").join(&d[7..]);
    let put = |bytes: &[u8]| -> String {
        let d = format!("sha256:{}", sha_hex(bytes));
        std::fs::write(blob_path(&d), bytes).unwrap();
        d
    };
    let original = w.root.join("original");
    std::fs::create_dir_all(&original).unwrap();
    let mut manifest = unpack_latest(&layout, &original);
    let config_digest = manifest["config"]["digest"].as_str().unwrap().to_string();
    let mut config: Value =
        serde_json::from_slice(&std::fs::read(blob_path(&config_digest)).unwrap()).unwrap();
    let mut label: Value = serde_json::from_str(
        config["config"]["Labels"]["dev.hushrun.baked-image"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let evil_bytes = b"attacker's sealed pack".to_vec();
    let evil_address = "e".repeat(64);
    let evil_path = format!("hushrun/packs/{evil_address}.pack");
    let mut row = label["rows"][0].clone();
    row["address"] = evil_address.clone().into();
    row["bytes"] = (evil_bytes.len() as u64).into();
    row["sha256"] = sha_hex(&evil_bytes).into();
    label["rows"].as_array_mut().unwrap().push(row);
    let workspace_at = label["layers"]
        .as_array()
        .unwrap()
        .iter()
        .position(|l| l["role"] == "workspace")
        .unwrap();
    let files = label["layers"][workspace_at]["files"]
        .as_array_mut()
        .unwrap();
    files.push(serde_json::json!({
        "path": evil_path,
        "size": evil_bytes.len(),
        "sha256": format!("sha256:{}", sha_hex(&evil_bytes)),
    }));
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    // The attacker's workspace layer: the original files and theirs, in B-65's bytes.
    let listed: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|f| {
            let path = f["path"].as_str().unwrap().to_string();
            let bytes = if path == evil_path {
                evil_bytes.clone()
            } else {
                std::fs::read(original.join(&path)).unwrap()
            };
            (path, bytes)
        })
        .collect();
    let layer_files: Vec<hushrun_core::blocks::image::File> = listed
        .iter()
        .map(|(path, bytes)| hushrun_core::blocks::image::File {
            path: path.clone(),
            size: bytes.len() as u64,
        })
        .collect();
    let (layer, _) =
        hushrun_core::blocks::image::layer(&layer_files, &|i| listed[i].1.clone()).unwrap();
    let layer_digest = put(&layer);
    manifest["layers"][workspace_at]["digest"] = layer_digest.clone().into();
    manifest["layers"][workspace_at]["size"] = (layer.len() as u64).into();
    config["config"]["Labels"]["dev.hushrun.baked-image"] = label.to_string().into();
    let config_bytes = serde_json::to_vec(&config).unwrap();
    manifest["config"]["digest"] = put(&config_bytes).into();
    manifest["config"]["size"] = (config_bytes.len() as u64).into();
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let manifest_digest = put(&manifest_bytes);
    let mut index: Value =
        serde_json::from_slice(&std::fs::read(layout.join("index.json")).unwrap()).unwrap();
    for m in index["manifests"].as_array_mut().unwrap() {
        if m["annotations"]["org.opencontainers.image.ref.name"]
            .as_str()
            .is_some_and(|t| t.ends_with("-latest"))
        {
            m["digest"] = manifest_digest.clone().into();
            m["size"] = (manifest_bytes.len() as u64).into();
        }
    }
    std::fs::write(
        layout.join("index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();

    // The next merge on main: a code change, built and baked by the trusted signer.
    std::fs::write(
        w.repo.join("ws/b/src/lib.rs"),
        "pub fn two() -> u32 {\n    a::one() + 2\n}\n",
    )
    .unwrap();
    let second = laptop.cargo_build(&target, None);
    assert_eq!(second.code, 0, "{}", second.stderr);
    let again = laptop.bake(&layout, &key);
    assert!(
        again["image"]["compacted"]
            .as_str()
            .unwrap_or_default()
            .contains("signature"),
        "{again}"
    );

    // A consumer's copy of the moving tag: the attacker's pack is there, its row is in
    // baked.json, and the signature the repository names covers it.
    let root = w.root.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let latest = unpack_latest(&layout, &root);
    assert!(
        latest["layers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["digest"] != layer_digest.as_str()),
        "{latest}"
    );
    let baked = root.join("hushrun");
    assert!(!baked.join(&evil_path[8..]).exists());
    let json: Value =
        serde_json::from_slice(&std::fs::read(baked.join("baked.json")).unwrap()).unwrap();
    assert!(
        json["packs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["address"] != evil_address.as_str()),
        "{json}"
    );
    let fresh = Machine::new(&w, "fresh");
    let status = ran(fresh
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .env("HUSHRUN_BUILD_LOWER", &baked)
        .args(["build", "status", "--format", "json"]));
    let status: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(status["baked"]["state"], "current", "{status}");
    assert_eq!(status["baked"]["evidence"], "signed", "{status}");
}

// covers: B-66, B-67
// `build: baked: not_before` is the one lever that retires old signed stores. After it is raised
// past the first image, a second bake of the same manifest must not point its moving tag at the
// old image, whose store every run now refuses: the old image doesn't count, the fixed tag stays
// with it and the bake says so loudly, and the moving tag goes to a new image a run trusts.
#[test]
fn a_bake_after_not_before_never_moves_its_tag_to_an_image_no_run_trusts() {
    let w = World::new("acme-api.yaml");
    let (laptop, key, _) = signed_world(&w);
    let layout = w.root.join("layout");
    let one = laptop.bake(&layout, &key);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let stamp = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    let not_before = String::from_utf8(stamp.stdout).unwrap().trim().to_string();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write(
        "hushrun.yaml",
        &format!("{policy}    not_before: {not_before}\n"),
    );
    w.commit("not before");
    let two = laptop.bake(&layout, &key);
    assert_eq!(two["manifest"], one["manifest"]);
    assert_ne!(two["image"]["digest"], one["image"]["digest"], "{two}");
    assert!(!uploaded(&two).is_empty(), "{two}");
    let warnings = two["image"]["warnings"].to_string();
    assert!(warnings.contains("not_before"), "{two}");
    // The fixed tag never moves: it names the first image still.
    let fixed = one["image"]["tags"][0].as_str().unwrap().to_string();
    assert_eq!(
        tag_digest(&layout, &fixed["hushrun-".len()..]),
        one["image"]["digest"].as_str().unwrap()
    );
    // The moving tag names the second image, whose store a run trusts.
    let root = w.root.join("root");
    std::fs::create_dir_all(&root).unwrap();
    unpack_latest(&layout, &root);
    let fresh = Machine::new(&w, "fresh");
    let status = ran(fresh
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .env("HUSHRUN_BUILD_LOWER", root.join("hushrun"))
        .args(["build", "status", "--format", "json"]));
    let status: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(status["baked"]["state"], "current", "{status}");
    assert_eq!(status["baked"]["evidence"], "signed", "{status}");
}

/// `hushrun build bake` in the workspace with `args`, its output whole.
fn bake_with(machine: &Machine<'_>, args: &[&std::ffi::OsStr], env: &[(&str, &str)]) -> Ran {
    let mut cmd = machine.hushrun();
    cmd.current_dir(machine.w.repo.join("ws"))
        .env_remove("SOURCE_DATE_EPOCH")
        .args(["build", "bake", "--check", "build"])
        .args(args);
    for (name, value) in env {
        cmd.env(name, value);
    }
    ran(&mut cmd)
}

/// Extracts the tar at `tar` into `root` with the system's `tar`, as a snapshot step would.
fn extract(tar: &Path, root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let status = std::process::Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(root)
        .arg("-f")
        .arg(tar)
        .status()
        .unwrap();
    assert!(status.success());
}

// covers: B-71, B-67
#[test]
fn a_signed_tar_extracted_anywhere_serves_a_fresh_machine_on_its_signature() {
    let w = World::new("acme-api.yaml");
    let (laptop, key, target) = signed_world(&w);
    let tar = w.root.join("store.tar");
    let erofs = w.root.join("store.erofs");
    let os = |s: &'static str| std::ffi::OsStr::new(s);
    let bake = bake_with(
        &laptop,
        &[
            os("--tar"),
            tar.as_os_str(),
            os("--erofs"),
            erofs.as_os_str(),
            os("--signing-key"),
            key.as_os_str(),
            os("--format"),
            os("json"),
        ],
        &[],
    );
    assert_eq!(bake.code, 0, "{}", bake.stderr);
    let said: Value = serde_json::from_str(&bake.stdout).expect(&bake.stdout);
    let archives = said["archives"].as_array().unwrap();
    assert_eq!(archives.len(), 2, "{said}");
    assert_eq!(archives[0]["format"], "tar");
    assert_eq!(archives[1]["format"], "erofs");
    assert_eq!(
        archives[0]["bytes"].as_u64().unwrap(),
        std::fs::metadata(&tar).unwrap().len()
    );
    let erofs_bytes = std::fs::read(&erofs).unwrap();
    assert_eq!(&erofs_bytes[1024..1028], &0xE0F5_E1E2u32.to_le_bytes());
    // No folder was asked for: none is left beside the archives.
    let mut left: Vec<String> = std::fs::read_dir(&w.root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .filter(|n| n.starts_with("store") || n.starts_with(".store"))
        .collect();
    left.sort();
    assert_eq!(left, ["store.erofs", "store.tar"]);

    // Extracted somewhere else than /hushrun: the folder is named, and the signature, carried
    // in the tar, is the evidence; no say-so, no backend.
    let root = w.root.join("snapshot");
    extract(&tar, &root);
    let baked = root.join("hushrun");
    assert!(baked.join("baked.sigstore.json").is_file());
    let fresh = Machine::new(&w, "fresh");
    std::fs::remove_dir_all(&target).unwrap();
    let reused = fresh.cargo_build(&target, Some(&baked));
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert!(
        reused
            .stderr
            .contains("4 of 4 crates reused (4 from the baked store)"),
        "{}",
        reused.stderr
    );

    // `--tar -` writes the tar to stdout, and what bake says to stderr.
    let piped = laptop
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .args([
            "build",
            "bake",
            "--check",
            "build",
            "--tar",
            "-",
            "--signing-key",
        ])
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        piped.status.success(),
        "{}",
        String::from_utf8_lossy(&piped.stderr)
    );
    let stderr = String::from_utf8_lossy(&piped.stderr);
    assert!(stderr.contains("Wrote stdout"), "{stderr}");
    assert_eq!(&piped.stdout[..8], b"hushrun/");
    let streamed = w.root.join("streamed.tar");
    std::fs::write(&streamed, &piped.stdout).unwrap();
    let other = w.root.join("other");
    extract(&streamed, &other);
    assert!(other.join("hushrun/baked.json").is_file());
    assert!(other.join("hushrun/baked.sigstore.json").is_file());

    // Both on stdout: refused before anything is baked.
    let both = bake_with(
        &laptop,
        &[os("--tar"), os("-"), os("--erofs"), os("-")],
        &[],
    );
    assert_eq!(both.code, 2, "{}", both.stderr);
}

// covers: B-71, B-63
#[test]
fn two_unsigned_bakes_of_the_same_store_give_the_same_bytes_with_source_date_epoch() {
    let w = World::new("acme-api.yaml");
    let (laptop, key, _target) = signed_world(&w);
    let os = std::ffi::OsStr::new;
    let at = |name: &str| w.root.join(name);
    let bake_to = |tar: &Path, erofs: &Path, extra: &[&std::ffi::OsStr], env: &[(&str, &str)]| {
        let mut args = vec![
            os("--tar"),
            tar.as_os_str(),
            os("--erofs"),
            erofs.as_os_str(),
        ];
        args.extend_from_slice(extra);
        let bake = bake_with(&laptop, &args, env);
        assert_eq!(bake.code, 0, "{}", bake.stderr);
        bake
    };
    let epoch = [("SOURCE_DATE_EPOCH", "1700000000")];
    let no_sign = [os("--no-sign")];
    bake_to(&at("a.tar"), &at("a.erofs"), &no_sign, &epoch);
    std::thread::sleep(std::time::Duration::from_millis(5));
    bake_to(&at("b.tar"), &at("b.erofs"), &no_sign, &epoch);
    let read = |name: &str| std::fs::read(at(name)).unwrap();
    assert_eq!(read("a.tar"), read("b.tar"));
    assert_eq!(read("a.erofs"), read("b.erofs"));
    // The time baked.json holds is SOURCE_DATE_EPOCH's.
    let root = at("a-out");
    extract(&at("a.tar"), &root);
    let json: Value =
        serde_json::from_slice(&std::fs::read(root.join("hushrun/baked.json")).unwrap()).unwrap();
    assert_eq!(json["baked_ms"], 1_700_000_000_000u64);
    // Without it, the clock: another baked.json, another archive.
    bake_to(&at("c.tar"), &at("c.erofs"), &no_sign, &[]);
    assert_ne!(read("a.tar"), read("c.tar"));
    // A signed store's time is the clock's, whatever SOURCE_DATE_EPOCH says: not_before
    // compares it.
    bake_to(
        &at("d.tar"),
        &at("d.erofs"),
        &[os("--signing-key"), key.as_os_str()],
        &epoch,
    );
    let root = at("d-out");
    extract(&at("d.tar"), &root);
    let json: Value =
        serde_json::from_slice(&std::fs::read(root.join("hushrun/baked.json")).unwrap()).unwrap();
    assert!(
        json["baked_ms"].as_u64().unwrap() > 1_700_000_000_000,
        "{json}"
    );
}

// covers: B-73
// Main moving fast: a bake within the hour of the image its tag names, with few blocks changed,
// publishes nothing and says why; `--force` publishes; once the image is older than
// `build: baked: rebake_every`, or enough blocks changed (`rebake_blocks`), it publishes.
#[test]
fn a_bake_waits_while_the_image_its_tag_names_is_young_and_little_changed() {
    let w = World::new("acme-api.yaml");
    let (laptop, key, target) = signed_world(&w);
    let layout = w.root.join("layout");
    let one = laptop.bake_args(&layout, &key, &[]);
    assert_eq!(one["image"]["state"], "published", "{one}");
    let branch = || {
        let tags: Vec<String> = one["image"]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        tags.into_iter().find(|t| t.contains("-branch-")).unwrap()
    };
    let branch_tag = branch();
    let suffix = &branch_tag["hushrun-".len()..];
    let first_digest = tag_digest(&layout, suffix);

    // One crate changes: one new block against an image seconds old.
    std::fs::write(
        w.repo.join("ws/b/src/lib.rs"),
        "pub fn two() -> u32 {\n    a::one() + 2\n}\n",
    )
    .unwrap();
    assert_eq!(laptop.cargo_build(&target, None).code, 0);
    let two = laptop.bake_args(&layout, &key, &[]);
    assert_eq!(two["image"]["state"], "skipped", "{two}");
    let why = two["image"]["skipped"].as_str().unwrap();
    assert!(
        why.contains("only 1 block changed") && why.contains("--force"),
        "{why}"
    );
    assert_eq!(two["image"]["uploaded_bytes"], 0);
    assert_eq!(tag_digest(&layout, suffix), first_digest, "the tag moved");
    // Said in words too, and the exit is 0.
    let said = ran(laptop
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .args([
            "build",
            "bake",
            "--check",
            "build",
            "--branch-tag",
            "--oci-dir",
        ])
        .arg(&layout)
        .arg("--signing-key")
        .arg(&key));
    assert_eq!(said.code, 0, "{}", said.stderr);
    assert!(said.stdout.contains("Not published to "), "{}", said.stdout);

    // --force publishes, and the tag moves.
    let three = laptop.bake_args(&layout, &key, &["--force"]);
    assert_eq!(three["image"]["state"], "published", "{three}");
    assert_ne!(tag_digest(&layout, suffix), first_digest);

    // A shorter window, or fewer blocks, as hushrun.yaml says.
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    w.write("hushrun.yaml", &format!("{policy}    rebake_every: 1s\n"));
    w.commit("rebake every second");
    std::fs::write(
        w.repo.join("ws/c/src/lib.rs"),
        "pub fn three() -> u32 {\n    3 + 0\n}\n",
    )
    .unwrap();
    assert_eq!(laptop.cargo_build(&target, None).code, 0);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let four = laptop.bake_args(&layout, &key, &[]);
    assert_eq!(four["image"]["state"], "published", "{four}");
    w.write("hushrun.yaml", &format!("{policy}    rebake_blocks: 1\n"));
    w.commit("rebake on one block");
    std::fs::write(
        w.repo.join("ws/d/src/lib.rs"),
        "pub fn four() -> u32 {\n    4 + 0\n}\n",
    )
    .unwrap();
    assert_eq!(laptop.cargo_build(&target, None).code, 0);
    let five = laptop.bake_args(&layout, &key, &[]);
    assert_eq!(five["image"]["state"], "published", "{five}");
}

// covers: B-67, E-10
// A bake signed with a key asks no timestamp authority unless HUSHRUN_TSA names one: here every
// outside request would go to a dead proxy, and a request tried would say the signature went out
// without a timestamp.
#[test]
fn a_key_signed_bake_asks_no_timestamp_authority_unless_one_is_named() {
    let w = World::new("acme-api.yaml");
    let (laptop, key, _) = signed_world(&w);
    let out = ran(laptop
        .hushrun()
        .current_dir(w.repo.join("ws"))
        .env_remove("HUSHRUN_TSA")
        .env("HTTPS_PROXY", NO_HOST)
        .env("https_proxy", NO_HOST)
        .args(["build", "bake", "--check", "build", "--force", "--oci-dir"])
        .arg(w.root.join("layout"))
        .arg("--out")
        .arg(w.root.join("plain"))
        .arg("--signing-key")
        .arg(&key));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        !out.stderr.contains("timestamp"),
        "a timestamp authority was asked: {}",
        out.stderr
    );
    // The signature holds the key and no timestamp (nor a transparency log entry).
    let material = |folder: &Path| -> Value {
        let bundle: Value =
            serde_json::from_slice(&std::fs::read(folder.join("baked.sigstore.json")).unwrap())
                .unwrap();
        bundle["verificationMaterial"].clone()
    };
    let plain = material(&w.root.join("plain"));
    assert!(plain["publicKey"].is_object(), "{plain}");
    assert!(plain["timestampVerificationData"].is_null(), "{plain}");
    // With HUSHRUN_TSA naming one, the signature carries its timestamp.
    let sigstore = FakeSigstore::start();
    std::fs::create_dir_all(w.root.join("sigstore")).unwrap();
    let mut cmd = laptop.hushrun();
    for (name, value) in sigstore.env(&w.root.join("sigstore")) {
        if name == "HUSHRUN_TSA" || name == "HUSHRUN_SIGSTORE_TRUSTED_ROOT" {
            cmd.env(name, value);
        }
    }
    let stamped = ran(cmd
        .current_dir(w.repo.join("ws"))
        .args(["build", "bake", "--check", "build", "--force", "--out"])
        .arg(w.root.join("stamped"))
        .arg("--tar")
        .arg(w.root.join("stamped.tar"))
        .arg("--signing-key")
        .arg(&key));
    assert_eq!(stamped.code, 0, "{}", stamped.stderr);
    let stamped = material(&w.root.join("stamped"));
    assert!(
        stamped["timestampVerificationData"]["rfc3161Timestamps"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "{stamped}"
    );
}

// covers: B-66, B-67, B-71
// The fixed-tag shortcut (the store is published already) with --tar, --erofs and --out: the
// folder and the tar carry the folder's signature, which a fresh machine accepts. Before the
// bake signed the folder itself, the shortcut signed nothing.
#[test]
fn the_fixed_tag_shortcut_still_signs_the_folder_and_its_archives() {
    let w = World::new("acme-api.yaml");
    let (laptop, key, target) = signed_world(&w);
    let layout = w.root.join("layout");
    let one = laptop.bake(&layout, &key);
    let fixed = one["image"]["tags"][0].as_str().unwrap().to_string();
    let os = std::ffi::OsStr::new;
    let tar = w.root.join("short.tar");
    let erofs = w.root.join("short.erofs");
    let out = w.root.join("short-out");
    let two = bake_with(
        &laptop,
        &[
            os("--oci-dir"),
            layout.as_os_str(),
            os("--tar"),
            tar.as_os_str(),
            os("--erofs"),
            erofs.as_os_str(),
            os("--out"),
            out.as_os_str(),
            os("--signing-key"),
            key.as_os_str(),
            os("--format"),
            os("json"),
        ],
        &[],
    );
    assert_eq!(two.code, 0, "{}", two.stderr);
    let said: Value = serde_json::from_str(&two.stdout).unwrap();
    assert_eq!(said["image"]["tags"][0].as_str().unwrap(), fixed);
    assert!(uploaded(&said).is_empty(), "shortcut not taken: {said}");
    assert!(out.join("baked.sigstore.json").is_file(), "folder unsigned");
    let root = w.root.join("short-x");
    extract(&tar, &root);
    assert!(root.join("hushrun/baked.sigstore.json").is_file());
    let fresh = Machine::new(&w, "fresh");
    std::fs::remove_dir_all(&target).unwrap();
    let reused = fresh.cargo_build(&target, Some(&root.join("hushrun")));
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert!(
        reused
            .stderr
            .contains("4 of 4 crates reused (4 from the baked store)"),
        "{}",
        reused.stderr
    );
    assert!(erofs.is_file());
}

// covers: B-67
// A bake outside CI (a GitHub Actions job told it is a person's machine, HUSHRUN_PROVIDER=local)
// signs only with a key, as its own line says: the job's ID token at hand signs nothing there
// unless HUSHRUN_SIGNING=sigstore asks for it. It said it ran outside CI and moved no `-latest`,
// and signed as the workflow all the same: the signer took any ID token, whatever "in CI" said.
#[test]
fn a_bake_outside_ci_signs_with_an_id_token_at_hand_only_when_asked() {
    let fake = FakeSigstore::start();
    fake.set_claims(github_claims(
        "Acme/api",
        "123456",
        "refs/heads/main",
        ".github/workflows/bake.yml",
        None,
    ));
    let w = World::new("acme-api.yaml");
    workspace(&w);
    let laptop = Machine::new(&w, "laptop");
    let first = laptop.cargo_build(&w.root.join("target"), None);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let bake = |layout: &Path, signing: Option<&str>| {
        let mut cmd = laptop.hushrun();
        cmd.envs(fake.env(&w.root))
            .env_remove("HUSHRUN_SIGNING")
            .env("CI", "true")
            .env("GITHUB_ACTIONS", "true")
            .env("HUSHRUN_PROVIDER", "local");
        if let Some(signing) = signing {
            cmd.env("HUSHRUN_SIGNING", signing);
        }
        let bake = ran(cmd
            .current_dir(w.repo.join("ws"))
            .args([
                "build",
                "bake",
                "--check",
                "build",
                "--branch-tag",
                "--oci-dir",
            ])
            .arg(layout)
            .args(["--format", "json"]));
        assert_eq!(bake.code, 0, "{}", bake.stderr);
        let said: Value = serde_json::from_str(&bake.stdout).unwrap();
        (said, bake.stderr)
    };
    let (said, stderr) = bake(&w.root.join("unasked"), None);
    assert!(said["image"]["signed"].is_null(), "{said}\n{stderr}");
    assert!(
        stderr
            .contains("the store is published unsigned: outside CI hushrun signs only with a key"),
        "{stderr}"
    );
    assert_eq!(fake.count("/api/v2/signingCert"), 0, "{stderr}");
    // Asked, it signs keylessly with the token at hand.
    let (said, stderr) = bake(&w.root.join("asked"), Some("sigstore"));
    assert!(said["image"]["signed"].is_string(), "{said}\n{stderr}");
    assert_eq!(fake.count("/api/v2/signingCert"), 1, "{stderr}");
}

/// A registry that serves the image layout at `layout`, read only, over plain HTTP on the
/// loopback, as Distribution serves what was pushed to it: each blob it sends is counted, and a
/// layer is sent with its bytes changed while `corrupt` is set.
struct LayoutRegistry {
    server: hushrun_testkit::http::Server,
    sent: Arc<Mutex<Vec<String>>>,
    corrupt: Arc<std::sync::atomic::AtomicBool>,
}

impl LayoutRegistry {
    fn serve(layout: &Path) -> LayoutRegistry {
        use hushrun_testkit::http::{Reply, Response, Server};
        let layout = layout.to_path_buf();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let corrupt = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (counted, corrupting) = (sent.clone(), corrupt.clone());
        let server = Server::start(move |request| {
            let blob = |digest: &str| {
                std::fs::read(
                    layout
                        .join("blobs/sha256")
                        .join(digest.trim_start_matches("sha256:")),
                )
                .ok()
            };
            let path = request.path.as_str();
            if path == "/v2/" {
                return Reply::Send(Response::json(200, &serde_json::json!({})));
            }
            let head = request.method == "HEAD";
            if let Some((_, reference)) = path.split_once("/manifests/") {
                let index: Value =
                    serde_json::from_slice(&std::fs::read(layout.join("index.json")).unwrap())
                        .unwrap();
                let found = index["manifests"].as_array().unwrap().iter().find(|m| {
                    m["digest"] == reference
                        || m["annotations"]["org.opencontainers.image.ref.name"] == reference
                });
                let Some(found) = found else {
                    return Reply::Send(Response::new(404));
                };
                let digest = found["digest"].as_str().unwrap();
                let bytes = blob(digest).unwrap();
                let kind = found["mediaType"].as_str().unwrap().to_string();
                let mut response = Response::bytes(200, if head { Vec::new() } else { bytes })
                    .header("docker-content-digest", digest);
                response.headers.retain(|(n, _)| n != "content-type");
                return Reply::Send(response.header("content-type", kind));
            }
            if let Some((_, digest)) = path.split_once("/blobs/") {
                return Reply::Send(match blob(digest) {
                    Some(mut bytes) => {
                        if !head {
                            counted.lock().unwrap().push(digest.to_string());
                        }
                        // A layer (a tar, larger than a config) with one byte changed.
                        if corrupting.load(std::sync::atomic::Ordering::SeqCst)
                            && bytes.len() > 1024
                        {
                            bytes[600] ^= 1;
                        }
                        Response::bytes(200, if head { Vec::new() } else { bytes })
                            .header("docker-content-digest", digest)
                    }
                    None => Response::new(404),
                });
            }
            Reply::Send(Response::new(404))
        });
        LayoutRegistry {
            server,
            sent,
            corrupt,
        }
    }

    /// The blobs sent since this was last asked.
    fn sent(&self) -> Vec<String> {
        std::mem::take(&mut *self.sent.lock().unwrap())
    }
}

/// `hushrun build pull` of `image:tag` into `to` with `args`, its output whole.
fn pull_with(machine: &Machine<'_>, image: &str, tag: &str, to: &Path, args: &[&str]) -> Ran {
    ran(machine
        .hushrun()
        .args([
            "build",
            "pull",
            "--insecure",
            "--image",
            image,
            "--tag",
            tag,
        ])
        .args(args)
        .arg("--to")
        .arg(to))
}

/// `hushrun build pull` of `image:tag` into `to`, as JSON checked against its schema.
fn pull(machine: &Machine<'_>, image: &str, tag: &str, to: &Path) -> Value {
    let pulled = pull_with(machine, image, tag, to, &["--format", "json"]);
    assert_eq!(pulled.code, 0, "{}", pulled.stderr);
    let said: Value = serde_json::from_str(&pulled.stdout).expect(&pulled.stdout);
    let schema = hushrun_testkit::schema::load("build-pull.v1.schema.json");
    hushrun_testkit::schema::assert_valid(&schema, &said);
    // The line is said on stderr beside the JSON, with the whole digest.
    assert!(
        pulled
            .stderr
            .contains(&format!("({})", said["digest"].as_str().unwrap())),
        "{}",
        pulled.stderr
    );
    said
}

// covers: B-240
#[test]
fn a_baked_image_is_pulled_from_a_registry_into_a_fresh_sandbox_and_a_second_pull_downloads_nothing()
 {
    let w = World::new("acme-api.yaml");
    let (laptop, key, target) = signed_world(&w);
    let layout = w.root.join("layout");
    let said = laptop.bake(&layout, &key);
    let fixed = said["image"]["tags"][0].as_str().unwrap().to_string();
    let digest = said["image"]["digest"].as_str().unwrap().to_string();
    let registry = LayoutRegistry::serve(&layout);
    let image = format!("{}/acme/app", registry.server.addr());
    let sent = || registry.sent().len();
    round_trip(&w, &laptop, &target, &image, &fixed, &digest, Some(&sent));
    let lower = w.root.join("sandbox").join("hushrun");

    // By digest: `--tag sha256:…` names the image itself.
    let by_digest = pull_with(&laptop, &image, &digest, &lower, &["--force"]);
    assert_eq!(by_digest.code, 0, "{}", by_digest.stderr);
    assert!(
        by_digest.stderr.contains("hushrun: pulled "),
        "{}",
        by_digest.stderr
    );
    // A folder that lost a pack is pulled again; `--force` pulls a whole one.
    let packs = lower.join("packs");
    let pack = std::fs::read_dir(&packs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::remove_file(&pack).unwrap();
    registry.sent();
    assert_eq!(pull(&laptop, &image, &fixed, &lower)["state"], "pulled");
    assert!(pack.is_file());
    assert!(!registry.sent().is_empty());
    let forced = pull_with(
        &laptop,
        &image,
        &fixed,
        &lower,
        &["--force", "--format", "json"],
    );
    let forced: Value = serde_json::from_str(&forced.stdout).expect(&forced.stderr);
    assert_eq!(forced["state"], "pulled");

    // A layer whose bytes aren't its digest replaces nothing, and leaves nothing beside it.
    let before = std::fs::read(lower.join("baked.json")).unwrap();
    registry
        .corrupt
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let corrupt = pull_with(&laptop, &image, &fixed, &lower, &["--force"]);
    registry
        .corrupt
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(corrupt.code, 1, "{}", corrupt.stderr);
    assert!(
        corrupt.stderr.contains("error: build pull: "),
        "{}",
        corrupt.stderr
    );
    assert_eq!(std::fs::read(lower.join("baked.json")).unwrap(), before);
    assert_eq!(
        std::fs::read_dir(lower.parent().unwrap()).unwrap().count(),
        1
    );

    // A tag that names nothing exits 1 and says so.
    let none = pull_with(&laptop, &image, "hushrun-linux-x64-nothing", &lower, &[]);
    assert_eq!(none.code, 1);
    assert!(
        none.stderr.contains("has no hushrun-linux-x64-nothing"),
        "{}",
        none.stderr
    );

    // Signers named, and a store none of them signed: replaced by nothing. Without signers,
    // `--require-signed` refuses it too.
    let unsigned = w.root.join("unsigned");
    let plain = bake_with(
        &laptop,
        &[
            "--branch-tag".as_ref(),
            "--force".as_ref(),
            "--no-sign".as_ref(),
            "--oci-dir".as_ref(),
            unsigned.as_os_str(),
        ],
        &[],
    );
    assert_eq!(plain.code, 0, "{}", plain.stderr);
    let other = LayoutRegistry::serve(&unsigned);
    let other_image = format!("{}/acme/app", other.server.addr());
    let refused = pull_with(&laptop, &other_image, &fixed, &lower, &["--force"]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("signature doesn't verify"),
        "{}",
        refused.stderr
    );
    assert_eq!(std::fs::read(lower.join("baked.json")).unwrap(), before);
    let policy = std::fs::read_to_string(w.repo.join("hushrun.yaml")).unwrap();
    let without = policy.split("\nbuild:\n").next().unwrap().to_string();
    w.write("hushrun.yaml", &without);
    let required = pull_with(
        &laptop,
        &other_image,
        &fixed,
        &lower,
        &["--force", "--require-signed"],
    );
    assert_eq!(required.code, 1, "{}", required.stderr);
    assert!(
        required.stderr.contains(&format!(
            "error: build pull: hushrun.yaml names no signer of baked stores, and --require-signed was given: {} is as it was. hushrun init --bake prints the lines to add",
            lower.display()
        )),
        "{}",
        required.stderr
    );
    assert_eq!(std::fs::read(lower.join("baked.json")).unwrap(), before);
    let unchecked = pull(&laptop, &other_image, &fixed, &lower);
    assert_eq!(unchecked["signature"], "not_checked", "{unchecked}");
    assert_eq!(unchecked["signed_by"], Value::Null);
    w.write("hushrun.yaml", &policy);
}

// covers: B-240
/// The same round trip through the registries CI starts (`HUSHRUN_IT_REGISTRIES`, two hosts over
/// plain HTTP, `--features it`): a bake pushes, a pull reads back.
#[cfg(feature = "it")]
#[test]
fn a_bake_pushed_to_each_real_registry_is_pulled_back() {
    let Ok(hosts) = std::env::var("HUSHRUN_IT_REGISTRIES") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "HUSHRUN_IT_REGISTRIES must be set in CI"
        );
        return;
    };
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    for host in hosts.split_whitespace() {
        let w = World::new("acme-api.yaml");
        let (laptop, key, target) = signed_world(&w);
        let image = format!("{host}/acme/pull-{unique}");
        let bake = ran(laptop
            .hushrun()
            .current_dir(w.repo.join("ws"))
            .args([
                "build",
                "bake",
                "--check",
                "build",
                "--branch-tag",
                "--force",
            ])
            .args([
                "--oci",
                &image,
                "--insecure-registry",
                host,
                "--signing-key",
            ])
            .arg(&key)
            .args(["--format", "json"]));
        assert_eq!(bake.code, 0, "{host}: {}", bake.stderr);
        let said: Value = serde_json::from_str(&bake.stdout).expect(&bake.stdout);
        let fixed = said["image"]["tags"][0].as_str().unwrap();
        let digest = said["image"]["digest"].as_str().unwrap();
        round_trip(&w, &laptop, &target, &image, fixed, digest, None);
    }
}

/// B-240's round trip: `image:tag`, a bake's signed image whose manifest is `digest`, pulled into
/// a sandbox's folder, serves a fresh machine on its signature; a second pull downloads no layer
/// (`downloads`, where the test can count them, says how many blobs were sent since it was last
/// asked); `--resolve` says what the tag names and reads no layer.
fn round_trip(
    w: &World,
    laptop: &Machine<'_>,
    target: &Path,
    image: &str,
    tag: &str,
    digest: &str,
    downloads: Option<&dyn Fn() -> usize>,
) {
    let lower = w.root.join("sandbox").join("hushrun");
    let first = pull(laptop, image, tag, &lower);
    assert_eq!(first["schema"], "hushrun.build-pull/v1");
    assert_eq!(first["state"], "pulled", "{first}");
    assert_eq!(first["digest"], digest, "the whole digest: {first}");
    assert_eq!(first["digest"].as_str().unwrap().len(), 71);
    assert_eq!(first["signature"], "named", "{first}");
    assert!(
        first["signed_by"]
            .as_str()
            .unwrap()
            .starts_with("key sha256:"),
        "{first}"
    );
    assert!(first["downloaded_bytes"].as_u64().unwrap() > 0);
    assert!(lower.join("baked.json").is_file());
    if let Some(downloads) = downloads {
        assert!(downloads() > 0);
    }
    // Nothing of the layers is left beside the store.
    assert_eq!(
        std::fs::read_dir(lower.parent().unwrap()).unwrap().count(),
        1
    );
    let again = pull_with(laptop, image, tag, &lower, &[]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(
        again.stderr.contains(&format!(
            "hushrun: {} holds {image}:{tag} ({digest}) already: nothing was downloaded",
            lower.display()
        )),
        "{}",
        again.stderr
    );
    if let Some(downloads) = downloads {
        assert_eq!(
            downloads(),
            1,
            "a second pull reads the config, and no layer"
        );
    }
    let resolved = ran(laptop.hushrun().args([
        "build",
        "pull",
        "--resolve",
        "--insecure",
        "--image",
        image,
        "--tag",
        tag,
    ]));
    assert_eq!(resolved.code, 0, "{}", resolved.stderr);
    assert_eq!(resolved.stdout, format!("{image}@{digest}\n"));
    if let Some(downloads) = downloads {
        assert_eq!(downloads(), 1, "--resolve reads the config, and no layer");
    }

    // A fresh machine reads the pulled store on its signature alone.
    let fresh = Machine::new(w, &format!("fresh-{}", image.len()));
    let _ = std::fs::remove_dir_all(target);
    let reused = fresh.cargo_build(target, Some(&lower));
    assert_eq!(reused.code, 0, "{}", reused.stderr);
    assert!(
        reused
            .stderr
            .contains("4 of 4 crates reused (4 from the baked store)"),
        "{}",
        reused.stderr
    );
}
