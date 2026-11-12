//! \[C-32\] Which adapter a COMMAND gets: [`RECOGNIZED`]'s order.

use std::path::Path;

use super::{RECOGNIZED, choose};
use crate::units::{gradle, vitest};

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

/// The word of the adapter `command` gets in `cwd`, or why the one it would get can't load.
fn chosen(command: &[String], cwd: &Path) -> Result<&'static str, String> {
    choose(command, cwd).map(|adapter| adapter.word())
}

/// A directory where Vitest's config and Gradle's settings load hushrun's adapters, so that every
/// adapter can load in it.
fn loads_all() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("vitest.config.ts"),
        format!("export default {{ {} }}\n", vitest::CONFIG_LINE),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("settings.gradle"),
        format!("// {}\n", gradle::VARIABLE),
    )
    .unwrap();
    dir
}

/// One COMMAND for each adapter, by its program's name.
const BY_NAME: &[(&str, &str)] = &[
    ("cargo test", "cargo"),
    ("cargo nextest run", "cargo"),
    ("go test ./...", "go"),
    ("npx vitest run", "vitest"),
    ("npx jest", "jest"),
    ("node node_modules/jest/bin/jest.js --ci", "jest"),
    ("mvn test", "maven"),
    ("./mvnw verify", "maven"),
    ("gradle test", "gradle"),
    ("./gradlew test", "gradle"),
    ("ctest --test-dir build", "ctest"),
    ("swift test", "swift"),
    ("pytest -q", "pytest"),
];

#[test]
fn each_adapter_gets_the_command_that_names_its_program() {
    assert_eq!(RECOGNIZED.len(), 8);
    let dir = loads_all();
    for (command, adapter) in BY_NAME {
        assert_eq!(
            chosen(&words(command), dir.path()),
            Ok(*adapter),
            "{command}"
        );
    }
}

#[test]
fn the_wider_matches_come_after_every_match_by_name() {
    // A Swift package that also has a Makefile whose `test` starts ctest: what Swift's and CTest's
    // wider matches both take.
    let dir = loads_all();
    std::fs::write(
        dir.path().join("Package.swift"),
        "// swift-tools-version:5.9\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        "test:\n\tctest --test-dir build\n",
    )
    .unwrap();
    for (command, adapter) in BY_NAME.iter().filter(|(_, a)| *a != "pytest") {
        assert_eq!(
            chosen(&words(command), dir.path()),
            Ok(*adapter),
            "{command} in a Swift package"
        );
    }
    // CTest's Makefile before Swift's package.
    assert_eq!(chosen(&words("make test"), dir.path()), Ok("ctest"));
    // pytest keeps a shell line that starts it, where no Package.swift is.
    let plain = loads_all();
    let shell = ["sh", "-c", "pytest && ctest"].map(str::to_string);
    assert_eq!(chosen(&shell, plain.path()), Ok("pytest"));
    // And in a Swift package: a wrapper is claimed only for what its text starts.
    assert_eq!(chosen(&shell, dir.path()), Ok("pytest"));
    assert_eq!(chosen(&words("pytest -q"), dir.path()), Ok("pytest"));
}

/// The one rule for a wrapper COMMAND (C-32): a shell's `-c` line, a `#!` script named by its
/// path, or `make` with its Makefile goes to an adapter that loads through `PATH` only when its
/// own text starts that adapter's program by name and names no other runner with an adapter.
#[test]
fn a_wrapper_goes_to_the_runner_its_text_starts_and_no_other() {
    let package = loads_all();
    std::fs::write(
        package.path().join("Package.swift"),
        "// swift-tools-version:5.9\n",
    )
    .unwrap();
    let plain = loads_all();
    let sh = |line: &str| ["sh", "-c", line].map(str::to_string).to_vec();
    for (dir, command, adapter) in [
        // Swift's: only what says `swift test` or `swift-test`.
        (
            &package,
            sh("swift build && swift test --parallel"),
            "swift",
        ),
        (&package, sh("swift-test"), "swift"),
        (&package, words("npm test"), "pytest"),
        (&package, words("bazel test //..."), "pytest"),
        (&package, sh("python -m pytest tests"), "pytest"),
        (&package, sh("swift test && pytest"), "pytest"),
        (&package, sh("swift test; ctest --test-dir build"), "pytest"),
        // CTest's, as before.
        (
            &plain,
            sh("cmake --build build && ctest --test-dir build"),
            "ctest",
        ),
        (&plain, sh("ctest && tox"), "pytest"),
        (&plain, sh("echo hi"), "pytest"),
    ] {
        assert_eq!(chosen(&command, dir.path()), Ok(adapter), "{command:?}");
    }
    // make, by its Makefile.
    for (makefile, adapter) in [
        ("test:\n\tswift test\n", "swift"),
        ("test:\n\tctest --test-dir build\n", "ctest"),
        ("test:\n\tswift test\n\tctest --test-dir build\n", "pytest"),
        ("test:\n\tpytest\n", "pytest"),
        ("test:\n\techo nothing\n", "pytest"),
    ] {
        std::fs::write(package.path().join("Makefile"), makefile).unwrap();
        assert_eq!(
            chosen(&words("make test"), package.path()),
            Ok(adapter),
            "{makefile:?}"
        );
    }
    // A #! script named by its path.
    std::fs::write(
        package.path().join("t.sh"),
        "#!/bin/sh\nset -e\nswift test\n",
    )
    .unwrap();
    assert_eq!(chosen(&words("./t.sh"), package.path()), Ok("swift"));
    std::fs::write(package.path().join("t.sh"), "#!/bin/sh\nexec npm test\n").unwrap();
    assert_eq!(chosen(&words("./t.sh"), package.path()), Ok("pytest"));
}

#[test]
fn a_program_is_known_by_its_name_whatever_its_directory_and_windows_extension() {
    let dir = loads_all();
    for (command, adapter) in [
        ("cargo.EXE test", "cargo"),
        ("C:\\go\\bin\\go.exe test ./...", "go"),
        ("node_modules/.bin/vitest.CMD run", "vitest"),
        ("vitest.bat run", "vitest"),
        ("jest.bat", "jest"),
        ("mvn.cmd test", "maven"),
        ("gradlew.BAT test", "gradle"),
        ("ctest.bat --test-dir build", "ctest"),
    ] {
        assert_eq!(
            chosen(&words(command), dir.path()),
            Ok(adapter),
            "{command}"
        );
    }
}

#[test]
fn env_with_variables_before_a_command_is_read_through_for_every_adapter() {
    let dir = loads_all();
    for (command, adapter) in BY_NAME {
        let with_env = format!("env FOO=1 BAR=a=b {command}");
        assert_eq!(
            chosen(&words(&with_env), dir.path()),
            Ok(*adapter),
            "{with_env}"
        );
    }
    // An option of env's own, `-i` above all, changes what reaches the command: nothing is read
    // through.
    assert_eq!(
        chosen(&words("env -i cargo test"), dir.path()),
        Ok("pytest")
    );
    assert_eq!(chosen(&words("/usr/bin/env"), dir.path()), Ok("pytest"));
    // pytest's own checks still see the Python's options.
    assert!(chosen(&words("env A=1 python -I -m pytest"), dir.path()).is_err());
}
