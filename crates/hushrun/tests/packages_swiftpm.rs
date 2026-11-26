//! `hushrun packages` with SwiftPM (K-6, K-10): a store baked by SwiftPM's own `swift package
//! resolve`, each repository a bare clone as SwiftPM's cache keeps it, is seeded into the user's
//! own cache, checked; SwiftPM then resolves the pinned revisions with no network.
//!
//! Most of it runs a stand-in `swift` that writes a cache as `swift package resolve
//! --cache-path` does, on every machine. The rest needs Swift 6 (`HUSHRUN_TEST_SWIFT`, as the
//! `swift test` adapter's tests) and the network, to bake from GitHub; it is skipped where Swift is
//! missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.
#![cfg(unix)]

#[path = "common/cli.rs"]
mod cli;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;
#[path = "common/swift.rs"]
mod swift;

use std::path::{Path, PathBuf};

use cli::Cli as _;
use repo::Repo;
use swift::file;

/// A pin of a remote repository, as Package.resolved version 3 writes it.
const RESOLVED: &str = r#"{
  "originHash" : "0000",
  "pins" : [
    {
      "identity" : "foo",
      "kind" : "remoteSourceControl",
      "location" : "https://example.com/acme/foo.git",
      "state" : { "revision" : "1111111111111111111111111111111111111111", "version" : "1.0.0" }
    }
  ],
  "version" : 3
}
"#;

/// A `swift` that writes, under its `--cache-path`, what `swift package resolve` writes there: a
/// bare clone, as git makes one with its empty folders, and SwiftPM's and git's own files, which a
/// bake leaves out. Its environment goes into the clone's description, so that a credential the
/// bake withholds would show.
const FAKE_SWIFT: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do
  if [ "$1" = "--cache-path" ]; then cache=$2; fi
  shift
done
d=$cache/repositories/foo-0a1b2c3d
git init -q --bare "$d"
mkdir -p "$d/objects/ab"
env > "$d/description"
printf object > "$d/objects/ab/cdef"
printf "1111111111111111111111111111111111111111 refs/tags/1.0.0\n" > "$d/packed-refs"
printf fetched > "$d/FETCH_HEAD"
: > "$cache/repositories/foo-0a1b2c3d.lock"
"#;

/// SwiftPM's cache under `home`, as this system keeps it.
fn swiftpm_cache(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Caches/org.swift.swiftpm")
    } else {
        home.join(".cache/org.swift.swiftpm")
    }
}

const CLEARED: &[&str] = &[
    "HUSHRUN_PACKAGES_LOWER",
    "HUSHRUN_PACKAGES",
    "HUSHRUN_PACKAGES_LOWER_UNTRUSTED",
    "XDG_CACHE_HOME",
];

/// A package that pins one remote repository, with the stand-in `swift` first on `PATH`.
fn fixture() -> Repo {
    let r = Repo::new(&[
        file(".gitignore", ".build/\n"),
        file("Package.swift", "// swift-tools-version: 6.0\n"),
        file("Package.resolved", RESOLVED),
    ])
    .clear(CLEARED);
    let fake = r.root.join("bin/swift");
    std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
    std::fs::write(&fake, FAKE_SWIFT).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = swift::path(&fake);
    r.give("PATH", path)
}

fn text(out: &std::process::Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every file under `dir`.
fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    repo::collect(dir, &mut out);
    out
}

// covers: K-6, K-10
#[test]
fn a_store_baked_by_swifts_own_resolve_is_seeded_checked_and_once() {
    let r = fixture();
    let store = r.root.join("packages");
    let canary = "canary-5e1d";
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .env("HUSHRUN_TOKEN", canary)
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", canary)
        .env("SIGSTORE_ID_TOKEN", canary)
        .output()
        .unwrap();
    let (said, err) = text(&baked);
    assert!(baked.status.success(), "{said}{err}");
    assert!(
        said.contains("packages: swiftpm: 6 files, ")
            && said.contains("(Package.resolved, Package.swift)"),
        "{said}"
    );
    // The clone, without git's and SwiftPM's notes, and no withheld credential in any file.
    let clone = store.join("parts/swiftpm/repositories/foo-0a1b2c3d");
    assert!(clone.join("objects/ab/cdef").is_file());
    assert!(!clone.join("FETCH_HEAD").exists());
    assert!(
        !store
            .join("parts/swiftpm/repositories/foo-0a1b2c3d.lock")
            .exists()
    );
    for f in all_files(&store) {
        let bytes = std::fs::read(&f).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(canary),
            "{} holds a withheld credential",
            f.display()
        );
    }
    // An install outside CI with the person's say-so: seeded into SwiftPM's cache before COMMAND.
    let exec = |home: &Path, vars: &[(&str, &str)]| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "exec", "--", "sh", "-c", "true"])
            .env("HOME", home)
            .env("HUSHRUN_PACKAGES_LOWER", &store);
        for (n, v) in vars {
            cmd.env(n, v);
        }
        text(&cmd.output().unwrap()).1
    };
    let untrusted = [("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")];
    let home = r.root.join("home");
    let cache = swiftpm_cache(&home);
    let err = exec(&home, &untrusted);
    assert!(
        err.contains(&format!(
            "packages: swiftpm: seeded 6 files into {} (0 there already)",
            cache.join("repositories").display()
        )),
        "{err}"
    );
    let seeded = cache.join("repositories/foo-0a1b2c3d");
    // A store holds files only; git needs a clone's folders too, which the seed makes.
    let git = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(&seeded)
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&git.stdout).trim(),
        "true",
        "git reads the seeded clone: {}",
        String::from_utf8_lossy(&git.stderr)
    );
    assert_eq!(
        std::fs::read(seeded.join("objects/ab/cdef")).unwrap(),
        b"object"
    );
    let stamps = std::fs::read_dir(cache.join("repositories"))
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".hushrun-packages-")
        })
        .count();
    assert_eq!(stamps, 1, "a stamp");
    // Again: the stamp says so, and nothing is copied.
    let err = exec(&home, &untrusted);
    assert!(
        err.contains("packages: swiftpm: seeded already into "),
        "{err}"
    );
    let restore = r
        .hushrun()
        .args(["packages", "restore"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .output()
        .unwrap();
    assert!(
        text(&restore)
            .0
            .contains("packages: swiftpm: seeded already into "),
        "{}",
        text(&restore).0
    );
    // `env` seeds as well, and exports nothing for it.
    let env = r
        .hushrun()
        .args(["packages", "env"])
        .env("HOME", r.root.join("home-env"))
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .output()
        .unwrap();
    assert_eq!(text(&env).0, "");
    assert!(
        swiftpm_cache(&r.root.join("home-env"))
            .join("repositories/foo-0a1b2c3d/HEAD")
            .is_file(),
        "{}",
        text(&env).1
    );
    // Unsigned, without the say-so: nothing is seeded.
    let bare = r.root.join("home-bare");
    let err = exec(&bare, &[]);
    assert!(err.contains("nothing here verifies it"), "{err}");
    assert!(!swiftpm_cache(&bare).exists());
    // A lockfile that pins it no more: nothing of it is seeded.
    r.write("Package.resolved", "{\"version\": 3, \"pins\": []}\n");
    let fresh = r.root.join("home-unpinned");
    let err = exec(&fresh, &untrusted);
    assert!(
        err.contains("packages: swiftpm: left out 6 files no lockfile here names now"),
        "{err}"
    );
    assert!(
        !swiftpm_cache(&fresh)
            .join("repositories/foo-0a1b2c3d")
            .exists()
    );
    // One it can't read: left alone, with why.
    r.write("Package.resolved", "{\"version\": 9}\n");
    let err = exec(&fresh, &untrusted);
    assert!(
        err.contains(
            "packages: swiftpm: left alone: hushrun can't read Package.resolved (version 9"
        ),
        "{err}"
    );
    r.write("Package.resolved", RESOLVED);
    // One byte of a file changed in the store, its size the same: the seed hashes what it copies,
    // and leaves that file out; `verify` finds it too.
    std::fs::write(clone.join("objects/ab/cdef"), b"objecT").unwrap();
    let changed = r.root.join("home-changed");
    let err = exec(&changed, &untrusted);
    assert!(
        err.contains("packages: swiftpm: left out 1 files whose bytes changed since the bake, such as repositories/foo-0a1b2c3d/objects/ab/cdef"),
        "{err}"
    );
    assert!(
        !swiftpm_cache(&changed)
            .join("repositories/foo-0a1b2c3d/objects/ab/cdef")
            .exists()
    );
    let verify = r
        .hushrun()
        .args(["packages", "verify"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert!(!verify.status.success());
    std::fs::write(clone.join("objects/ab/cdef"), b"object").unwrap();
    // A file no row names, or a link out of the part: the store is invalid.
    let status = |at: &Path| {
        let out = r
            .hushrun()
            .args(["packages", "status", "--format", "json"])
            .env("HUSHRUN_PACKAGES_LOWER", at)
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .output()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        json["state"].as_str().unwrap_or_default().to_string()
    };
    assert_eq!(status(&store), "current");
    std::fs::write(clone.join("objects/ab/added"), b"x").unwrap();
    assert_eq!(status(&store), "invalid");
    std::fs::remove_file(clone.join("objects/ab/added")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", clone.join("objects/ab/out")).unwrap();
    assert_eq!(status(&store), "invalid");
}

// covers: K-7, K-8
#[test]
fn a_signed_store_is_seeded_in_ci_and_a_changed_byte_keeps_the_part_out() {
    let r = fixture();
    let key = r.root.join("key.pem");
    let made = r
        .hushrun()
        .args(["keys", "generate", "--algorithm", "ecdsa-p256", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", text(&made).1);
    let public = r
        .hushrun()
        .args(["keys", "public", "--key"])
        .arg(&key)
        .output()
        .unwrap();
    let public = text(&public)
        .0
        .lines()
        .find_map(|l| l.strip_prefix("public key: ").map(str::to_string))
        .expect("a public key");
    r.write(
        "hushrun.yaml",
        &format!(
            "version: 1\nrepository: github.com/acme/shop\nbuild:\n  baked:\n    signers:\n      - key: {public}\nchecks:\n  test:\n    run: swift test\n"
        ),
    );
    hushrun_testkit::git::commit_all(&r.repo, "signers");
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--out"])
        .arg(&store)
        .arg("--signing-key")
        .arg(&key)
        .output()
        .unwrap();
    let (said, err) = text(&baked);
    assert!(baked.status.success(), "{said}{err}");
    assert!(said.contains("packages: signed by "), "{said}");
    let exec = |home: &Path| {
        let out = r
            .hushrun()
            .args(["packages", "exec", "--", "sh", "-c", "true"])
            .env("HOME", home)
            .env("HUSHRUN_PACKAGES_LOWER", &store)
            .env("CI", "true")
            .output()
            .unwrap();
        text(&out).1
    };
    let home = r.root.join("home-ci");
    let err = exec(&home);
    assert!(err.contains("packages: swiftpm: seeded 6 files"), "{err}");
    // In CI every file is hashed before any tool reads it: one byte changed, the size the same,
    // and the part is left out, with a line; SwiftPM downloads as it does today.
    let file = store.join("parts/swiftpm/repositories/foo-0a1b2c3d/objects/ab/cdef");
    std::fs::write(&file, b"objecT").unwrap();
    let other = r.root.join("home-ci-changed");
    let err = exec(&other);
    assert!(!err.contains("seeded"), "{err}");
    assert!(err.contains("swiftpm: ignored: "), "{err}");
    assert!(!swiftpm_cache(&other).join("repositories").exists());
}

/// The fixture for real Swift: a package whose one test target needs nothing, and which pins
/// swift-argument-parser, which no target uses: SwiftPM resolves it all the same.
fn real_fixture(version: &str) -> Vec<(String, String)> {
    vec![
        file(".gitignore", ".build/\n"),
        file(
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {mode: reuse}\n",
        ),
        file(
            "Package.swift",
            &format!(
                r#"// swift-tools-version: 6.0
import PackageDescription
let package = Package(
  name: "Shop",
  dependencies: [
    .package(url: "https://github.com/apple/swift-argument-parser.git", exact: "{version}"),
  ],
  targets: [
    .target(name: "Core"),
    .testTarget(name: "CoreTests", dependencies: ["Core"]),
  ]
)
"#
            ),
        ),
        file(
            "Sources/Core/Core.swift",
            "public func price(_ x: Int) -> Int { x * 2 }\n",
        ),
        file(
            "Tests/CoreTests/CoreTests.swift",
            "import XCTest\n@testable import Core\n\nfinal class CoreTests: XCTestCase {\n    func testPrice() { XCTAssertEqual(price(3), 6) }\n}\n",
        ),
    ]
}

/// Variables that leave every download nowhere to go: git and SwiftPM read them.
const NO_NETWORK: &[(&str, &str)] = &[
    ("HTTPS_PROXY", "http://127.0.0.1:9"),
    ("https_proxy", "http://127.0.0.1:9"),
    ("HTTP_PROXY", "http://127.0.0.1:9"),
    ("http_proxy", "http://127.0.0.1:9"),
    ("ALL_PROXY", "http://127.0.0.1:9"),
    ("all_proxy", "http://127.0.0.1:9"),
    ("NO_PROXY", ""),
    ("no_proxy", ""),
    ("GIT_TERMINAL_PROMPT", "0"),
];

// covers: K-6, K-10, K-12
#[test]
fn swiftpm_resolves_offline_from_a_seeded_cache_and_no_units_key_moves() {
    let Some(swift) = swift::swift() else {
        swift::tools("Swift 6", false);
        return;
    };
    let r = Repo::new(&real_fixture("1.5.0"))
        .clear(CLEARED)
        .give("PATH", swift::path(&swift));
    std::fs::create_dir_all(r.root.join("home")).unwrap();
    // Package.resolved, written by SwiftPM online, with a cache of its own, then committed.
    let resolve_online = |r: &Repo| {
        let out = r
            .command(swift.to_str().unwrap())
            .args(["package", "resolve", "--cache-path"])
            .arg(r.root.join("online-cache"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}{}", text(&out).0, text(&out).1);
        let _ = std::fs::remove_dir_all(r.repo.join(".build"));
        hushrun_testkit::git::commit_all(&r.repo, "resolved");
    };
    resolve_online(&r);
    // The bake runs SwiftPM's own resolve, into a cache of its own.
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .output()
        .unwrap();
    let (said, err) = text(&baked);
    assert!(baked.status.success(), "{said}{err}");
    assert!(said.contains("packages: swiftpm: "), "{said}");
    assert!(
        !r.repo.join(".build").exists(),
        "the bake wrote nothing of the checkout"
    );
    // The control: offline, with an empty cache and no store, SwiftPM can't resolve.
    let resolve = |home: &str, with_store: bool| {
        let mut cmd = r.hushrun();
        cmd.args(["packages", "exec", "--"])
            .arg(&swift)
            .args(["package", "resolve"])
            .env("HOME", r.root.join(home))
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .envs(NO_NETWORK.iter().copied());
        if with_store {
            cmd.env("HUSHRUN_PACKAGES_LOWER", &store);
        } else {
            cmd.env("HUSHRUN_PACKAGES_LOWER", "");
        }
        let _ = std::fs::remove_dir_all(r.repo.join(".build"));
        std::fs::create_dir_all(r.root.join(home)).unwrap();
        cmd.output().unwrap()
    };
    let control = resolve("home-control", false);
    assert!(
        !control.status.success(),
        "the network is off: {}",
        text(&control).1
    );
    // Offline, with the store: seeded, then resolved.
    let read = resolve("home", true);
    let (out, err) = text(&read);
    assert!(read.status.success(), "{out}{err}");
    assert!(err.contains("packages: swiftpm: seeded "), "{err}");
    assert!(
        std::fs::read_dir(swiftpm_cache(&r.root.join("home")).join("repositories"))
            .unwrap()
            .flatten()
            .any(|e| e
                .file_name()
                .to_string_lossy()
                .starts_with("swift-argument-parser-")),
        "the clone is in SwiftPM's cache"
    );
    // covers: K-10
    // `swift test`'s units, offline, with the store; then the same without it skips: a seed
    // moves no key.
    let run = |home: &str, store_var: &Path| {
        let out = r
            .hushrun()
            .args(["run", "unit", "--"])
            .arg(&swift)
            .arg("test")
            .env("HOME", r.root.join(home))
            .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
            .env("HUSHRUN_PACKAGES_LOWER", store_var)
            .envs(NO_NETWORK.iter().copied())
            .output()
            .unwrap();
        text(&out).1
    };
    // A run seeds nothing: the install before it does, here `restore`, as a setup script would.
    let restore = r
        .hushrun()
        .args(["packages", "restore"])
        .env("HOME", r.root.join("home-run"))
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .output()
        .unwrap();
    assert!(
        text(&restore).0.contains("packages: swiftpm: seeded "),
        "{}",
        text(&restore).0
    );
    let _ = std::fs::remove_dir_all(r.repo.join(".build"));
    let first = run("home-run", &store);
    assert!(first.contains("unit passed"), "{first}");
    let second = run("home-run-bare", Path::new(""));
    assert!(second.contains("unit skipped"), "{second}");
    // The lockfile changed by one package: the part is behind, still read, and SwiftPM downloads
    // what it lacks, with the network on.
    r.write(
        "Package.swift",
        &real_fixture("1.4.0")
            .into_iter()
            .find(|(p, _)| p == "Package.swift")
            .unwrap()
            .1,
    );
    resolve_online(&r);
    let status = r
        .hushrun()
        .args(["packages", "status", "--format", "json"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(json["state"], "behind", "{json}");
    let _ = std::fs::remove_dir_all(r.repo.join(".build"));
    let online = r
        .hushrun()
        .args(["packages", "exec", "--"])
        .arg(&swift)
        .args(["package", "resolve"])
        .env("HOME", r.root.join("home-behind"))
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .env("HUSHRUN_PACKAGES_LOWER_UNTRUSTED", "1")
        .output()
        .unwrap();
    assert!(online.status.success(), "{}", text(&online).1);
}
