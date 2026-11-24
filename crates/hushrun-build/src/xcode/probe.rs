//! Whether a run's COMMAND gets the service (B-84): a Mac, an Xcode project in the checkout,
//! and an Xcode whose cache protocol and prefix mapping hushrun knows.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `xcodebuild -version` may take.
const PROBE_WAIT: Duration = Duration::from_secs(10);

/// The major versions of Xcode the service is known to serve: Xcode 27 is the first whose
/// prefix mapping is safe with cached diagnostics (Swift 6.4, swiftlang/swift#90700; Xcode 26
/// crashes on them). A newer major gets no service until it is added here.
pub const KNOWN_MAJORS: &[u32] = &[27];

/// How deep below the checkout's root a project is looked for.
const MOST_DEPTH: usize = 4;

/// Folders never looked into for a project.
const SKIPPED: &[&str] = &[
    "node_modules",
    "Pods",
    "Carthage",
    "DerivedData",
    "build",
    "target",
    "vendor",
];

/// The Xcode a run would build with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xcode {
    /// `Xcode 27.2`'s `27.2`.
    pub version: String,
    /// Its major version.
    pub major: u32,
    /// `Build version 27C5032a`'s `27C5032a`.
    pub build: String,
}

impl Xcode {
    /// The platform of its manifest (B-90): `xcode-` and its build, lower case. Values of one
    /// Xcode build are what another machine with it may reuse.
    pub fn platform(&self) -> String {
        format!("xcode-{}", self.build.to_ascii_lowercase())
    }

    /// Whether the service serves it.
    pub fn known(&self) -> bool {
        KNOWN_MAJORS.contains(&self.major)
    }
}

/// Why a run gets no service. Only [`NotServed::Version`] is said; the rest are silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotServed {
    /// Not macOS.
    NotMac,
    /// Turned off by `HUSHRUN_BUILD_XCODE`.
    Off,
    /// No `.xcodeproj`, `.xcworkspace` or `Package.swift` in the checkout.
    NoProject,
    /// `xcodebuild -version` failed or took too long.
    Failed(String),
    /// An Xcode the service doesn't know.
    Version(Xcode),
}

/// Reads `xcodebuild -version`'s answer.
pub fn parse_version(text: &str) -> Option<Xcode> {
    let mut version = None;
    let mut build = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Xcode ") {
            version = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Build version ") {
            build = Some(rest.trim().to_string());
        }
    }
    let version = version?;
    let build = build
        .filter(|build| !build.is_empty() && build.bytes().all(|b| b.is_ascii_alphanumeric()))?;
    let major = version.split('.').next()?.parse().ok()?;
    Some(Xcode {
        version,
        major,
        build,
    })
}

/// Asks `xcodebuild -version` in `folder`, under the environment `env` gives (`DEVELOPER_DIR`
/// chooses the Xcode).
pub fn xcode(folder: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Xcode, NotServed> {
    let mut command = Command::new("xcodebuild");
    command
        .arg("-version")
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = env("DEVELOPER_DIR") {
        command.env("DEVELOPER_DIR", dir);
    }
    let mut child = command
        .spawn()
        .map_err(|err| NotServed::Failed(format!("xcodebuild -version: {err}")))?;
    let deadline = Instant::now() + PROBE_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(NotServed::Failed(
                    "xcodebuild -version took too long".into(),
                ));
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| NotServed::Failed(format!("xcodebuild -version: {err}")))?;
    if !output.status.success() {
        return Err(NotServed::Failed(format!(
            "xcodebuild -version exited with {}",
            output.status
        )));
    }
    parse_version(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| NotServed::Failed("xcodebuild -version's answer can't be read".into()))
}

/// Whether `root` holds an Xcode project, workspace or Swift package within a few folders, none
/// of them hidden or of a kind that holds others' code (`node_modules`, `Pods` and the like).
pub fn has_project(root: &Path) -> bool {
    find_project(root, 0)
}

fn find_project(dir: &Path, depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut below: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(".xcodeproj") || name.ends_with(".xcworkspace") || name == "Package.swift"
        {
            return true;
        }
        if depth + 1 < MOST_DEPTH
            && !name.starts_with('.')
            && !SKIPPED.contains(&name)
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            below.push(entry.path());
        }
    }
    below.iter().any(|dir| find_project(dir, depth + 1))
}

/// The whole decision for a run in `folder`, whose checkout is `root`.
pub fn decide(
    folder: &Path,
    root: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Xcode, NotServed> {
    if !cfg!(target_os = "macos") && env("HUSHRUN_TEST_XCODE_ANY_OS").is_none() {
        return Err(NotServed::NotMac);
    }
    if super::turned_off(env(super::OFF_VAR).as_deref()) {
        return Err(NotServed::Off);
    }
    if !has_project(root) && !has_project(folder) {
        return Err(NotServed::NoProject);
    }
    let xcode = xcode(folder, env)?;
    if !xcode.known() {
        return Err(NotServed::Version(xcode));
    }
    Ok(xcode)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-84
    #[test]
    fn xcodebuilds_version_is_read() {
        let xcode = parse_version("Xcode 27.2\nBuild version 27C5032a\n").unwrap();
        assert_eq!(xcode.version, "27.2");
        assert_eq!(xcode.major, 27);
        assert_eq!(xcode.platform(), "xcode-27c5032a");
        assert!(xcode.known());
        assert!(hushrun_core::blocks::is_platform(&xcode.platform()));
        let old = parse_version("Xcode 26.4\nBuild version 17E192\n").unwrap();
        assert!(!old.known());
        assert_eq!(parse_version("Xcode 27.2\n"), None);
        assert_eq!(parse_version("Xcode 27.2\nBuild version 27/../x\n"), None);
        assert_eq!(parse_version("nonsense"), None);
    }

    // covers: B-84
    #[test]
    fn a_project_is_found_a_few_folders_down_and_not_in_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_project(dir.path()));
        std::fs::create_dir_all(dir.path().join("node_modules/lib/ios/Lib.xcodeproj")).unwrap();
        assert!(!has_project(dir.path()));
        std::fs::create_dir_all(dir.path().join("ios/App.xcodeproj")).unwrap();
        assert!(has_project(dir.path()));
        let package = tempfile::tempdir().unwrap();
        std::fs::write(
            package.path().join("Package.swift"),
            "// swift-tools-version:6.0",
        )
        .unwrap();
        assert!(has_project(package.path()));
    }
}
