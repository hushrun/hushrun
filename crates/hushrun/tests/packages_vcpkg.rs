//! `hushrun packages` with vcpkg (K-6, K-10): a store baked by the fetch `vcpkg install
//! --only-downloads` holds vcpkg's assets, each named by its SHA-512, and an install reads it in
//! place as one more asset source, with the origin gone. vcpkg checks every file it takes against
//! its portfile's SHA-512, so no signature is needed. Needs vcpkg (`HUSHRUN_TEST_VCPKG`), but for
//! the test of what the reader leaves alone.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;
#[path = "common/vcpkg.rs"]
mod vcpkg;

use std::path::{Path, PathBuf};
use std::process::Output;

use cli::Cli as _;
use repo::Repo;

const CLEARED: &[&str] = &["HUSHRUN_PACKAGES_LOWER", "HUSHRUN_PACKAGES", "CI"];

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A repository whose `vcpkg.json` depends on `hello`, with a port `world` beside it, and vcpkg.
struct Fixture {
    r: Repo,
    vcpkg: String,
    ports: Vec<vcpkg::Port>,
    canary: PathBuf,
    store: PathBuf,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        let vcpkg = vcpkg::vcpkg()?;
        let dir = tempfile::tempdir().unwrap().keep();
        let canary = dir.join("canary");
        let (files, ports) = vcpkg::files(
            &vcpkg,
            &dir.join("origin"),
            &canary,
            &["hello", "world"],
            &["hello"],
        );
        let mut r = Repo::new(&files).clear(vcpkg::CLEARED).clear(CLEARED);
        for (name, value) in vcpkg::GIVEN {
            r = r.give(name, value);
        }
        let store = r.root.join("packages");
        Some(Fixture {
            r,
            vcpkg,
            ports,
            canary,
            store,
        })
    }

    /// `hushrun packages bake` with the fetch the docs name.
    fn bake(&self) -> Output {
        let mut cmd = self.r.hushrun();
        cmd.args(["packages", "bake", "--no-sign", "--out"])
            .arg(&self.store)
            .args(["--", &self.vcpkg, "install", "--only-downloads"])
            .args(vcpkg::roots(&self.r.root.join("bake")));
        cmd.output().unwrap()
    }

    /// `vcpkg install` through `hushrun packages exec`, from a downloads folder of its own, with
    /// the store or none.
    fn install(&self, scratch: &str, store: bool) -> Output {
        let scratch = self.r.root.join(scratch);
        let mut cmd = self.r.hushrun();
        cmd.args(["packages", "exec", "--", &self.vcpkg, "install"])
            .args(vcpkg::roots(&scratch))
            .env("VCPKG_DOWNLOADS", scratch.join("downloads"))
            .env(
                "HUSHRUN_PACKAGES_LOWER",
                if store {
                    self.store.as_os_str()
                } else {
                    "".as_ref()
                },
            );
        cmd.output().unwrap()
    }

    fn port(&self, name: &str) -> &vcpkg::Port {
        self.ports.iter().find(|p| p.name == name).unwrap()
    }

    /// Takes the origin of `name` away, or gives it back.
    fn origin(&self, name: &str, there: bool) {
        let origin = &self.port(name).origin;
        let away = origin.with_extension("away");
        if there {
            std::fs::rename(&away, origin).unwrap();
        } else {
            std::fs::rename(origin, &away).unwrap();
        }
    }

    /// The store's file of the asset `name` downloads.
    fn asset(&self, name: &str) -> PathBuf {
        let sha = vcpkg::hash(&self.vcpkg, &self.port(name).origin);
        self.store.join("parts/vcpkg/assets").join(sha)
    }
}

// covers: K-6, K-10
#[test]
fn a_baked_store_is_read_with_the_origin_gone_and_vcpkg_checks_every_file() {
    let Some(f) = Fixture::new() else { return };
    let baked = f.bake();
    let out = said(&baked);
    assert!(baked.status.success(), "{out}");
    assert!(
        out.contains("packages: vcpkg: 1 files")
            && out.contains("(ports/hello/vcpkg.json, ports/world/vcpkg.json, vcpkg-configuration.json, vcpkg.json)"),
        "{out}"
    );
    // vcpkg checks each file itself: an unsigned store is read by any run.
    assert!(
        out.contains("each part is checked against its lockfile, so any run reads it"),
        "{out}"
    );
    let asset = f.asset("hello");
    assert!(asset.is_file(), "{out}");
    // The fetch runs no process of a port: download mode refuses them.
    assert!(!f.canary.exists(), "a port's process ran in the bake");
    let packages: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.store.join("packages.json")).unwrap()).unwrap();
    assert_eq!(packages["parts"][0]["part"], "vcpkg");
    assert_eq!(packages["parts"][0]["verified_by"], "lockfile");

    // The origin gone, an empty downloads folder: the install takes the file from the store, and
    // the store gains nothing.
    f.origin("hello", false);
    let read = f.install("read", true);
    let out = said(&read);
    assert!(read.status.success(), "{out}");
    assert!(out.contains("Asset cache hit"), "{out}");
    assert!(vcpkg::installed(&f.r.root.join("read"), "hello").is_some());
    let files = std::fs::read_dir(f.store.join("parts/vcpkg/assets"))
        .unwrap()
        .count();
    assert_eq!(files, 1, "read in place, nothing written to the store");
    // Without the store the same install fails: the store is what it read.
    let none = f.install("none", false);
    assert!(!none.status.success(), "{}", said(&none));

    // The lockfile names one package more: that one is downloaded, the rest read from the store,
    // and the store says it is behind.
    f.r.write("vcpkg.json", &vcpkg::manifest(&["hello", "world"]));
    let more = f.install("more", true);
    let out = said(&more);
    assert!(more.status.success(), "{out}");
    assert!(vcpkg::installed(&f.r.root.join("more"), "world").is_some());
    let status = f.r.run(
        &["packages", "status"],
        &[("HUSHRUN_PACKAGES_LOWER", f.store.to_str().unwrap())],
    );
    assert!(
        status.stdout.contains("vcpkg: 1 files") && status.stdout.contains("behind"),
        "{}",
        status.stdout
    );
    f.r.write("vcpkg.json", &vcpkg::manifest(&["hello"]));

    // One byte of the store's file changed, same size: vcpkg refuses it for its SHA-512 and, the
    // origin gone, the install fails; with the origin back it downloads the file from there.
    let mut bytes = std::fs::read(&asset).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&asset, &bytes).unwrap();
    let tampered = f.install("tampered", true);
    let out = said(&tampered);
    assert!(!tampered.status.success(), "{out}");
    assert!(out.contains("unexpected hash"), "{out}");
    f.origin("hello", true);
    let again = f.install("again", true);
    let out = said(&again);
    assert!(again.status.success(), "{out}");
    assert!(out.contains("trying authoritative source"), "{out}");
    // `verify` finds it too, naming the file.
    let verify = f.r.run(
        &["packages", "verify"],
        &[("HUSHRUN_PACKAGES_LOWER", f.store.to_str().unwrap())],
    );
    assert!(verify.code != 0, "{}", verify.stdout);
}

// covers: K-10
#[test]
fn the_check_s_key_is_the_same_with_and_without_the_store() {
    let Some(f) = Fixture::new() else { return };
    let baked = f.bake();
    assert!(baked.status.success(), "{}", said(&baked));
    // The same command both times, from an empty downloads folder: the command is in the key.
    let run = |store: &Path| {
        let scratch = f.r.root.join("run");
        let _ = std::fs::remove_dir_all(&scratch);
        let mut cmd = f.r.hushrun();
        cmd.args(["run", "t", "--", &f.vcpkg, "install"])
            .args(vcpkg::roots(&scratch))
            .env("VCPKG_DOWNLOADS", scratch.join("downloads"))
            .env("HUSHRUN_PACKAGES_LOWER", store);
        let out = cmd.output().unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    // The origin gone: the first run reads the file from the store.
    f.origin("hello", false);
    let first = run(&f.store);
    assert!(first.contains("t passed"), "{first}");
    // The reader's variable is in no key: the run without the store skips.
    f.origin("hello", true);
    let second = run(Path::new(""));
    assert!(second.contains("t skipped"), "{second}");
}

// covers: K-6, K-10
#[test]
fn a_harvest_keeps_only_assets_and_a_person_s_own_asset_source_leaves_the_part_alone() {
    let r = Repo::new(&[
        (
            "vcpkg.json".into(),
            "{\"dependencies\": [\"zlib\"]}\n".into(),
        ),
        ("main.c".into(), "int main(void) { return 0; }\n".into()),
    ])
    .clear(vcpkg::CLEARED)
    .clear(CLEARED);
    let assets = r.root.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("a".repeat(128)), b"an asset").unwrap();
    std::fs::write(assets.join("zlib-1.3.tar.gz"), b"not named by its hash").unwrap();
    let store = r.root.join("packages");
    let baked = r.run(
        &[
            "packages",
            "bake",
            "--no-sign",
            "--out",
            store.to_str().unwrap(),
            "--harvest",
            &format!("vcpkg={}", assets.display()),
        ],
        &[],
    );
    assert!(baked.code == 0, "{}{}", baked.stdout, baked.stderr);
    assert!(
        baked.stdout.contains("packages: vcpkg: 1 files"),
        "{}",
        baked.stdout
    );
    assert!(
        baked.stdout.contains("left out 1 files"),
        "{}",
        baked.stdout
    );
    let lower = store.to_str().unwrap();
    let ours = format!("x-azurl,file://{lower}/parts/vcpkg/assets,,read");
    // No variable of the person's: the store is the one asset source.
    let env = r.run(&["packages", "env"], &[("HUSHRUN_PACKAGES_LOWER", lower)]);
    assert_eq!(
        env.stdout,
        format!("export X_VCPKG_ASSET_SOURCES='{ours}'\n")
    );
    // The person's words that name no source are kept, first.
    let env = r.run(
        &["packages", "env"],
        &[
            ("HUSHRUN_PACKAGES_LOWER", lower),
            ("X_VCPKG_ASSET_SOURCES", "x-block-origin"),
        ],
    );
    assert_eq!(
        env.stdout,
        format!("export X_VCPKG_ASSET_SOURCES='x-block-origin;{ours}'\n")
    );
    // A source of the person's: vcpkg reads only one, so the part is left alone, with a line.
    let env = r.run(
        &["packages", "env"],
        &[
            ("HUSHRUN_PACKAGES_LOWER", lower),
            (
                "X_VCPKG_ASSET_SOURCES",
                "x-azurl,https://mirror.example/a,,read",
            ),
        ],
    );
    assert_eq!(env.stdout, "");
    assert!(
        env.stderr.contains(
            "packages: vcpkg: left alone: X_VCPKG_ASSET_SOURCES names an asset source already, and vcpkg reads only one"
        ),
        "{}",
        env.stderr
    );
    // In CI too, where nothing is signed: the tool checks the part.
    let env = r.run(
        &["packages", "env"],
        &[("HUSHRUN_PACKAGES_LOWER", lower), ("CI", "true")],
    );
    assert_eq!(
        env.stdout,
        format!("export X_VCPKG_ASSET_SOURCES='{ours}'\n")
    );
}
