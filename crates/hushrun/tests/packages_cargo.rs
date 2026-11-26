//! `hushrun packages` with Cargo (K-6, K-10): a store harvested from a cargo home is seeded into
//! an empty one by `hushrun run` before COMMAND, each `.crate` only when its SHA-256 is the
//! checksum `Cargo.lock` pins, and an offline build passes. The fixture depends on a crate
//! hushrun itself locks, so the machine's cargo home holds it once hushrun is built; where it
//! doesn't, the test says so and passes.

#[path = "common/cli.rs"]
mod cli;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use std::path::{Path, PathBuf};

use cli::Cli as _;
use repo::Repo;

/// The crate the fixture depends on, as hushrun's own `Cargo.lock` pins it.
const CRATE: &str = "itoa";

/// The version and checksum hushrun's `Cargo.lock` gives [`CRATE`].
fn pinned() -> (String, String) {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = std::fs::read_to_string(lock).unwrap();
    let at = text
        .find(&format!("name = \"{CRATE}\"\n"))
        .expect("hushrun locks the crate");
    let field = |name: &str| {
        let rest = &text[at..];
        let start = rest.find(&format!("{name} = \"")).unwrap() + name.len() + 4;
        rest[start..start + rest[start..].find('"').unwrap()].to_string()
    };
    (field("version"), field("checksum"))
}

/// This machine's cargo home, as the test process has it.
fn machine_cargo_home() -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo"))
}

/// The registry folder of the machine's cargo home that holds `file`, if one does.
fn registry_holding(file: &str) -> Option<String> {
    std::fs::read_dir(machine_cargo_home().join("registry/cache"))
        .ok()?
        .flatten()
        .find(|e| e.path().join(file).is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
}

/// A cargo home with only the index entries of [`CRATE`], copied from the machine's: what
/// `cargo build --offline` needs besides the crate itself. The crate is the store's to give.
fn empty_cargo_home(at: &Path, registry: &str) {
    let from = machine_cargo_home().join("registry/index").join(registry);
    let to = at.join("registry/index").join(registry);
    let entry = format!(".cache/{}/{}/{CRATE}", &CRATE[..2], &CRATE[2..4]);
    for file in ["config.json", entry.as_str()] {
        let dest = to.join(file);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(from.join(file), dest).unwrap();
    }
}

// covers: K-6, K-10
#[test]
fn a_store_seeds_an_empty_cargo_home_with_the_crates_cargo_lock_pins() {
    let (version, checksum) = pinned();
    let file = format!("{CRATE}-{version}.crate");
    let Some(registry) = registry_holding(&file) else {
        eprintln!("note: skipping: this machine's cargo home doesn't hold {file}");
        return;
    };
    // The test's HOME is its own; rustup still finds this machine's toolchains.
    let rustup = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".rustup"));
    let r = Repo::new(&[
        (".gitignore".into(), "target/\n".into()),
        (
            "Cargo.toml".into(),
            format!("[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{CRATE} = \"={version}\"\n"),
        ),
        (
            "Cargo.lock".into(),
            format!("version = 4\n\n[[package]]\nname = \"{CRATE}\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{checksum}\"\n\n[[package]]\nname = \"shop\"\nversion = \"0.1.0\"\ndependencies = [\n \"{CRATE}\",\n]\n"),
        ),
        (
            "src/lib.rs".into(),
            "pub fn show(n: u32) -> String { itoa::Buffer::new().format(n).to_string() }\n".into(),
        ),
    ])
    .clear(&["CARGO_HOME", "CARGO_TARGET_DIR", "HUSHRUN_PACKAGES_LOWER", "HUSHRUN_PACKAGES"])
    .give("RUSTUP_HOME", rustup);
    // A harvest from a cargo home that holds the crate and a file of cargo's own.
    let filled = r.root.join("filled");
    let crates = filled.join("registry/cache").join(&registry);
    std::fs::create_dir_all(&crates).unwrap();
    std::fs::copy(
        machine_cargo_home()
            .join("registry/cache")
            .join(&registry)
            .join(&file),
        crates.join(&file),
    )
    .unwrap();
    std::fs::write(
        filled
            .join("registry/cache")
            .join(&registry)
            .join(".package-cache"),
        "",
    )
    .unwrap();
    let store = r.root.join("packages");
    let baked = r
        .hushrun()
        .args(["packages", "bake", "--no-sign", "--out"])
        .arg(&store)
        .arg("--harvest")
        .arg(format!("cargo={}", filled.display()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&baked.stdout).into_owned();
    assert!(
        baked.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&baked.stderr)
    );
    assert!(said.contains("packages: cargo: 1 files"), "{said}");
    assert!(said.contains("(Cargo.lock, Cargo.toml)"), "{said}");
    assert!(
        said.contains("the store is unsigned (--no-sign): every run reads it, since each file is checked against the lockfile"),
        "{said}"
    );
    // The row names the package and Cargo.lock's checksum.
    let rows = std::fs::read_to_string(store.join("rows/cargo.jsonl")).unwrap();
    assert!(
        rows.contains(&format!("\"package\":\"{CRATE}@{version}\"")),
        "{rows}"
    );
    assert!(
        rows.contains(&format!("\"locked\":\"{checksum}\"")),
        "{rows}"
    );
    let packages = std::fs::read_to_string(store.join("packages.json")).unwrap();
    assert!(
        packages.contains("\"verified_by\": \"lockfile\""),
        "{packages}"
    );

    let home = r.root.join("cargo-home");
    let run = |name: &str, store: &Path| {
        let mut cmd = r.hushrun();
        cmd.args(["run", name, "--", "cargo", "build", "--offline", "--locked"])
            .env("CARGO_HOME", &home)
            .env("HUSHRUN_PACKAGES_LOWER", store);
        cmd.output().unwrap()
    };
    empty_cargo_home(&home, &registry);
    let first = run("b", &store);
    let err = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(first.status.success(), "{err}");
    assert!(
        err.contains("packages: cargo: copied 1 files into "),
        "{err}"
    );
    let seeded = home.join("registry/cache").join(&registry).join(&file);
    assert_eq!(
        hushrun_core::blocks::Hex32::sha256(&std::fs::read(&seeded).unwrap()).to_string(),
        checksum
    );
    // covers: K-10
    // The check's key is the same with and without the store: the run without it skips.
    let second = run("b", Path::new(""));
    let err = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(err.contains("b skipped"), "{err}");

    // covers: K-10
    // Nor does a block's: the crate a seed gave compiles to the blocks the same crate downloaded
    // does, with build reuse on.
    let keyed = |name: &str, store: &Path, home: &Path| {
        let mut cmd = r.hushrun();
        cmd.args(["run", name, "--", "cargo", "build", "--offline", "--locked"])
            .env("HUSHRUN_BUILD", "on")
            .env("CARGO_HOME", home)
            .env("HUSHRUN_PACKAGES_LOWER", store);
        String::from_utf8_lossy(&cmd.output().unwrap().stderr).into_owned()
    };
    let other = r.root.join("cargo-home-2");
    let _ = std::fs::remove_dir_all(r.repo.join("target"));
    empty_cargo_home(&other, &registry);
    let err = keyed("k1", &store, &other);
    assert!(err.contains("build: 2 crates built, none reused"), "{err}");
    let _ = std::fs::remove_dir_all(r.repo.join("target"));
    let _ = std::fs::remove_dir_all(&other);
    empty_cargo_home(&other, &registry);
    let downloaded = other.join("registry/cache").join(&registry);
    std::fs::create_dir_all(&downloaded).unwrap();
    std::fs::copy(crates.join(&file), downloaded.join(&file)).unwrap();
    let err = keyed("k2", Path::new(""), &other);
    assert!(err.contains("build: 2 of 2 crates reused"), "{err}");

    // Without the store the same offline build fails: the store is what it read.
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(r.repo.join("target"));
    empty_cargo_home(&home, &registry);
    assert!(!run("c", Path::new("")).status.success());

    // A crate changed in place, the same size: never copied, and the build doesn't get it.
    let part = store
        .join("parts/cargo/registry/cache")
        .join(&registry)
        .join(&file);
    let mut bytes = std::fs::read(&part).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&part, &bytes).unwrap();
    let tampered = run("d", &store);
    let err = String::from_utf8_lossy(&tampered.stderr).into_owned();
    assert!(!tampered.status.success(), "{err}");
    assert!(
        err.contains(
            "packages: cargo: left out 1 files whose bytes aren't the ones the lockfile pins"
        ),
        "{err}"
    );
    assert!(!seeded.exists());

    // `restore` seeds outside a run, in CI too, unsigned: Cargo.lock is the evidence. `env`
    // exports nothing and says so.
    std::fs::write(&part, {
        bytes[last] ^= 1;
        &bytes
    })
    .unwrap();
    let restore = r
        .hushrun()
        .args(["packages", "restore"])
        .env("CI", "true")
        .env("CARGO_HOME", &home)
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&restore.stdout).into_owned();
    assert!(restore.status.success(), "{out}");
    assert!(
        out.contains("packages: cargo: copied 1 files into "),
        "{out}"
    );
    assert!(seeded.is_file());
    let env = r
        .hushrun()
        .args(["packages", "env"])
        .env("HUSHRUN_PACKAGES_LOWER", &store)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&env.stdout), "");
    assert!(
        String::from_utf8_lossy(&env.stderr).contains("packages: cargo: nothing to export: `hushrun packages restore` copies its files into Cargo's cache"),
    );
}
