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
    assert_eq!(RECOGNIZED.len(), 7);
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
    // In a Swift package, Swift's match takes any COMMAND that doesn't start pytest by its first
    // word, as it does today (ADR 0086).
    assert_eq!(chosen(&shell, dir.path()), Ok("swift"));
    assert_eq!(chosen(&words("pytest -q"), dir.path()), Ok("pytest"));
}
