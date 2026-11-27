//! The vcpkg the tests of the `vcpkg` package part run (`packages_vcpkg.rs`, `build_cc.rs`). A
//! fixture's ports are its own (overlay ports): each downloads one header from a `file://` URL,
//! its "origin", so that no test needs the network, and taking the file away is the network
//! being off for it. They run where `HUSHRUN_TEST_VCPKG` names vcpkg, which CI sets on Linux;
//! elsewhere they say so and pass.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// vcpkg's variables a developer's shell may hold, cleared from every command of a test.
pub const CLEARED: &[&str] = &[
    "X_VCPKG_ASSET_SOURCES",
    "VCPKG_DOWNLOADS",
    "VCPKG_BINARY_SOURCES",
    "VCPKG_ROOT",
    "VCPKG_DEFAULT_TRIPLET",
    "VCPKG_DEFAULT_HOST_TRIPLET",
    "VCPKG_FEATURE_FLAGS",
    "VCPKG_OVERLAY_PORTS",
    "VCPKG_OVERLAY_TRIPLETS",
    "VCPKG_INSTALLED_DIR",
    "VCPKG_FORCE_SYSTEM_BINARIES",
];

/// The variables every vcpkg of a test gets: no binary cache, so that each install takes its
/// sources from an asset source or the origin, and CMake and Ninja from the machine, so that
/// vcpkg downloads no tool.
pub const GIVEN: &[(&str, &str)] = &[
    ("VCPKG_BINARY_SOURCES", "clear"),
    ("VCPKG_FORCE_SYSTEM_BINARIES", "1"),
];

/// vcpkg, when `HUSHRUN_TEST_VCPKG` names one. Named and not running: the test fails.
pub fn vcpkg() -> Option<String> {
    let Some(vcpkg) = std::env::var("HUSHRUN_TEST_VCPKG")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        eprintln!("note: skipping: HUSHRUN_TEST_VCPKG doesn't name vcpkg");
        return None;
    };
    let runs = Command::new(&vcpkg)
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(runs, "HUSHRUN_TEST_VCPKG names {vcpkg}, which doesn't run");
    Some(vcpkg)
}

/// A port of a fixture's own: its name, and the header it downloads from its origin.
pub struct Port {
    pub name: &'static str,
    pub origin: PathBuf,
}

/// Writes each port's header into `origin_dir` and returns the files of a repository that
/// depends on `depends`, with every port in `ports/`: each portfile downloads its header (vcpkg
/// checks it against the SHA-512 it names), then touches `canary` in a process of its own, which
/// download mode never starts, and installs it as `include/<name>.h`.
pub fn files(
    vcpkg: &str,
    origin_dir: &Path,
    canary: &Path,
    ports: &[&'static str],
    depends: &[&str],
) -> (Vec<(String, String)>, Vec<Port>) {
    std::fs::create_dir_all(origin_dir).unwrap();
    let mut files = Vec::new();
    let mut made = Vec::new();
    for name in ports {
        let origin = origin_dir.join(format!("{name}-1.0.h"));
        std::fs::write(
            &origin,
            format!("#define {}_VALUE 42\n", name.to_uppercase()),
        )
        .unwrap();
        let sha512 = hash(vcpkg, &origin);
        files.push((
            format!("ports/{name}/vcpkg.json"),
            format!("{{\"name\": \"{name}\", \"version\": \"1.0\"}}\n"),
        ));
        files.push((
            format!("ports/{name}/portfile.cmake"),
            format!(
                "vcpkg_download_distfile(SOURCE URLS \"file://{origin}\" FILENAME \"{name}-1.0.h\" SHA512 {sha512})\n\
                 vcpkg_execute_required_process(COMMAND touch \"{canary}\" WORKING_DIRECTORY \"${{CURRENT_BUILDTREES_DIR}}\" LOGNAME canary)\n\
                 file(INSTALL \"${{SOURCE}}\" DESTINATION \"${{CURRENT_PACKAGES_DIR}}/include\" RENAME {name}.h)\n\
                 file(WRITE \"${{CURRENT_PACKAGES_DIR}}/share/${{PORT}}/copyright\" \"fixture\")\n",
                origin = origin.display(),
                canary = canary.display(),
            ),
        ));
        made.push(Port { name, origin });
    }
    files.push(("vcpkg.json".into(), manifest(depends)));
    files.push((
        "vcpkg-configuration.json".into(),
        "{\"overlay-ports\": [\"./ports\"]}\n".into(),
    ));
    files.push((".gitignore".into(), "build/\n".into()));
    (files, made)
}

/// `vcpkg.json` for a project that depends on `depends`.
pub fn manifest(depends: &[&str]) -> String {
    let names: Vec<String> = depends.iter().map(|d| format!("\"{d}\"")).collect();
    format!("{{\"dependencies\": [{}]}}\n", names.join(", "))
}

/// The SHA-512 of `file`, as vcpkg computes it.
pub fn hash(vcpkg: &str, file: &Path) -> String {
    let out = Command::new(vcpkg).arg("hash").arg(file).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The arguments that keep an install's folders under `scratch`, and out of vcpkg's own root, so
/// that tests running at once don't meet; and that wait for vcpkg's lock on its root, which they
/// share, instead of failing.
pub fn roots(scratch: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["install", "buildtrees", "packages"]
        .iter()
        .map(|folder| format!("--x-{folder}-root={}", scratch.join(folder).display()))
        .collect();
    args.push("--x-wait-for-lock".into());
    args
}

/// Where an install under `scratch` put `<name>.h`, whatever the triplet.
pub fn installed(scratch: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(scratch.join("install"))
        .ok()?
        .flatten()
        .map(|e| e.path().join("include").join(format!("{name}.h")))
        .find(|p| p.is_file())
}
