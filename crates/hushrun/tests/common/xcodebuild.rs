//! \[ADR 0121\] The fixtures of the `xcodebuild test` adapter's tests, which
//! `units_xcodebuild.rs` (in-process) and `e2e_xcodebuild.rs` (the CLI) share. The project is
//! tiny (`testdata/xcodebuild/Lib`), and needs macOS with Xcode 16 or later:
//! `HUSHRUN_TEST_XCODEBUILD` names the `xcodebuild` to use (by default the one on `PATH`). The
//! tests say so and pass where it is missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI
//! sets on macOS.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The `xcodebuild` these tests run, on macOS with Xcode 16 or later: `HUSHRUN_TEST_XCODEBUILD`
/// names it, by default the one on `PATH`. Elsewhere there is none, and none is missing.
pub fn xcodebuild() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let named = std::env::var_os("HUSHRUN_TEST_XCODEBUILD").map(PathBuf::from);
    let xcodebuild = named.unwrap_or_else(|| PathBuf::from("xcodebuild"));
    let output = hushrun_testkit::tool::command(&xcodebuild)
        .arg("-version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let major: u32 = text
        .strip_prefix("Xcode ")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    (output.status.success() && major >= 16).then_some(xcodebuild)
}

pub fn tools(what: &str, here: bool) -> bool {
    if here {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

pub fn copy(from: &Path, out: &mut Vec<(String, String)>, prefix: &str) {
    for entry in std::fs::read_dir(from).expect("fixture").flatten() {
        let path = entry.path();
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if path.is_dir() {
            copy(&path, out, &format!("{name}/"));
        } else {
            out.push((name, std::fs::read_to_string(&path).expect("text")));
        }
    }
}

/// The fixture under `ios/`: Lib, LibTests on it (XCTest and Swift Testing), OtherTests alone;
/// the scheme `All` tests both. `TEST_RUNNER_FAIL` fails `LibTests.testFlips`.
pub fn project(tests: &str) -> Vec<(String, String)> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/xcodebuild/Lib");
    let mut files = Vec::new();
    copy(&fixture, &mut files, "ios/");
    files.push((
        "hushrun.yaml".into(),
        format!(
            "version: 1\nrepository: example.com/acme/lib\nchecks:\n  unit:\n    tests: {tests}\n"
        ),
    ));
    files.push(("notes/a.md".into(), "a\n".into()));
    files
}

/// `PATH` with `xcodebuild`'s directory first, when it is named by its path, then the test's.
pub fn path(xcodebuild: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if xcodebuild.is_absolute()
        && let Some(parent) = xcodebuild.parent()
    {
        dirs.push(parent.to_path_buf());
    }
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs).expect("PATH")
}

/// COMMAND that tests the fixture's scheme `All` on macOS, with derived data in `dd`.
pub fn test_command(dd: &Path) -> Vec<String> {
    [
        "xcodebuild",
        "-project",
        "ios/Lib.xcodeproj",
        "-scheme",
        "All",
        "-destination",
        "platform=macOS",
        "-derivedDataPath",
        dd.to_str().expect("utf-8"),
        "CODE_SIGNING_ALLOWED=NO",
        "test",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
