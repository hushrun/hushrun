//! Doctests' fixtures, which `units_rustdoc.rs` (in-process) and `e2e_rustdoc.rs` share: tiny
//! Cargo workspaces of libraries with doctests, which need only Cargo, offline.

#![allow(dead_code)]

use std::process::Command;

use super::repo::Repo;

/// Environment variables COMMAND must never inherit besides [`super::repo::CLEARED`]: what
/// changes how Cargo builds and documents, and hushrun's programs' own.
pub const CLEARED: &[&str] = &[
    "HUSHRUN_PROGRAMS",
    "HUSHRUN_PROGRAMS_PATH",
    "HUSHRUN_RUSTDOC",
    "RUSTDOC",
    "RUSTDOCFLAGS",
    "CARGO_TARGET_DIR",
    "RUSTC_WORKSPACE_WRAPPER",
];

/// `cargo test --doc` of every library, offline: COMMAND of the `doctest` check.
pub const DOCTEST: &[&str] = &[
    "cargo",
    "test",
    "--workspace",
    "--doc",
    "--locked",
    "--offline",
];

/// A repository of `files`, with its `Cargo.lock`, committed. COMMAND keeps this machine's
/// `HOME`, where Cargo's and rustup's homes are.
pub fn repo(files: &[(String, String)]) -> Repo {
    let mut r = Repo::new(files).clear(CLEARED);
    if let Some(home) = std::env::var_os("HOME") {
        r = r.give("HOME", home);
    }
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "-q"])
        .current_dir(&r.repo)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("cargo");
    assert!(lock.success());
    hushrun_testkit::git::commit_all(&r.repo, "lock");
    r
}

/// Whether rustdoc ran the doctests of the library in the folder `package`, by what it printed
/// (`stdout`): it names each doctest by its file.
pub fn ran(stdout: &str, package: &str) -> bool {
    let prefix = format!("test {package}/src/lib.rs - ");
    stdout.lines().any(|l| l.starts_with(&prefix))
}

/// The map's reads of `unit`.
pub fn reads(r: &Repo, unit: &str) -> Vec<String> {
    let map = r.map();
    map["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit} isn't in the map: {map:#}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

pub fn package(name: &str, deps: &str, dev: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}\n[dev-dependencies]\n{dev}"
    )
}

/// A library whose one doctest says it returns `n`.
pub fn library(name: &str, n: u32) -> String {
    format!(
        "/// ```\n/// assert_eq!({name}::value(), {n});\n/// ```\npub fn value() -> u32 {{\n    {n}\n}}\n"
    )
}

/// A workspace of five libraries with doctests: `core`, whose doctests use its dev-dependency
/// `helper`, `app`, which depends on `core`, and `one`, `two` and `three` on their own.
pub fn workspace() -> Vec<(String, String)> {
    let files = vec![
        (
            "hushrun.yaml",
            "version: 1\nrepository: example.com/acme/docs\nchecks:\n  doctest:\n    tests:\n      mode: reuse\n".to_string(),
        ),
        (".gitignore", "target/\n".to_string()),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"core\", \"helper\", \"app\", \"one\", \"two\", \"three\"]\nresolver = \"2\"\n".to_string(),
        ),
        // `helper`'s own doctests are off: it is only a dev-dependency here.
        (
            "helper/Cargo.toml",
            format!("{}\n[lib]\ndoctest = false\n", package("helper", "", "")),
        ),
        ("helper/src/lib.rs", "pub fn half(x: u32) -> u32 {\n    x / 2\n}\n".to_string()),
        (
            "core/Cargo.toml",
            package("core", "", "helper = { path = \"../helper\" }\n"),
        ),
        (
            "core/src/lib.rs",
            "/// ```\n/// assert_eq!(helper::half(core::value()), 21);\n/// ```\npub fn value() -> u32 {\n    42\n}\n".to_string(),
        ),
        (
            "app/Cargo.toml",
            package("app", "core = { path = \"../core\" }\n", ""),
        ),
        ("app/src/lib.rs", "/// ```\n/// assert_eq!(app::value(), 43);\n/// ```\npub fn value() -> u32 {\n    core::value() + 1\n}\n".to_string()),
    ];
    let mut files: Vec<(String, String)> = files
        .into_iter()
        .map(|(path, text)| (path.to_string(), text))
        .collect();
    for (n, name) in [(1, "one"), (2, "two"), (3, "three")] {
        files.push((format!("{name}/Cargo.toml"), package(name, "", "")));
        files.push((format!("{name}/src/lib.rs"), library(name, n)));
    }
    files
}
