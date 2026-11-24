//! What a test's child processes get of the developer's environment: nothing that changes how a
//! runner the test starts behaves, and the same everywhere.
//!
//! A test that starts hushrun, which starts pytest, Node, Go, a JVM or CMake, would otherwise run
//! them with whatever the developer's shell exports: `PYTEST_ADDOPTS`, `NODE_OPTIONS`, a
//! `GOFLAGS`, a `JAVA_TOOL_OPTIONS`. And Python's own bytecode cache compares only a source's
//! whole-second time and its size, so a test that edits a module to one of the same size and runs
//! it again within a second runs the old bytes unless `PYTHONDONTWRITEBYTECODE` is set: a shell
//! that exports it hides the failure, and CI's runners don't. So every child gets it set, and the
//! rest scrubbed. The tool paths the suite is given (`HUSHRUN_TEST_*`, `PATH`, `JAVA_HOME`) are
//! kept.

use std::ffi::OsString;

/// What every child process a test starts gets, whatever the developer's shell says.
pub const SET: &[(&str, &str)] = &[("PYTHONDONTWRITEBYTECODE", "1")];

/// What a test that starts `hushrun run` on two machines sets, so that what the second reuses of
/// the first depends on what the fake backend answered and never on how loaded the machine is:
/// every wait of the relay that the product bounds by time (a fetch waiting for `open`, a
/// manifest or a download; the end of the command waiting for uploads) waits instead for what
/// it waits on to be over. Read only by a debug build of hushrun (`Tuning::settle`). A test of a
/// bound itself (a frozen or slow backend costs one wait) leaves it unset.
pub const SETTLE: (&str, &str) = ("HUSHRUN_BUILD_SETTLE", "1");

/// Variables scrubbed by name: what changes how a runner behaves.
const NAMES: &[&str] = &[
    "NODE_OPTIONS",
    "GOFLAGS",
    "MAVEN_OPTS",
    "MAVEN_ARGS",
    "GRADLE_OPTS",
    "JAVA_TOOL_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "_JAVA_OPTIONS",
    "VIRTUAL_ENV",
    "COVERAGE_CORE",
    "COVERAGE_PROCESS_START",
    // \[ADR 0140\] What says a coding agent runs, or which hosted sandbox this is: a test's
    // hushrun decides CI, sandbox or person by what the test sets, never by the shell it runs in.
    "AI_AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    "CLAUDE_CODE_REMOTE",
    "CURSOR_AGENT_SOCKET",
    "COPILOT_AGENT_SESSION_ID",
    "CODEX_PROXY_CERT",
    "HUSHRUN_SANDBOX",
    "CODEX_SANDBOX",
    "CODEX_CI",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "CURSOR_AGENT",
    "CURSOR_EXTENSION_HOST_ROLE",
    "GEMINI_CLI",
    "AUGMENT_AGENT",
    "ANTIGRAVITY_AGENT",
    "COPILOT_MODEL",
    "COPILOT_ALLOW_ALL",
    "COPILOT_GITHUB_TOKEN",
];

/// Variables scrubbed by their prefix: Python's own (`PYTHONPATH`, `PYTHONHOME`, …), pytest's, and
/// CMake's and CTest's (`CMAKE_GENERATOR`, `CTEST_PARALLEL_LEVEL`, …).
const PREFIXES: &[&str] = &["PYTHON", "PYTEST_", "CMAKE_", "CTEST_"];

/// Whether `name` is a variable of the developer's environment that a child must not get.
pub fn scrubs(name: &str) -> bool {
    NAMES.contains(&name) || PREFIXES.iter().any(|p| name.starts_with(p))
}

/// The variables of this process's environment a child must not get.
pub fn scrubbed() -> Vec<OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_str().is_some_and(scrubs))
        .collect()
}

/// A command that can be given an environment: [`std::process::Command`] and
/// [`assert_cmd::Command`].
pub trait Hermetic {
    /// Scrubs [`scrubbed`] and sets [`SET`]. Call it before what the test itself sets.
    fn hermetic(&mut self) -> &mut Self;
}

impl Hermetic for std::process::Command {
    fn hermetic(&mut self) -> &mut Self {
        for name in scrubbed() {
            self.env_remove(name);
        }
        for (name, value) in SET {
            self.env(name, value);
        }
        self
    }
}

impl Hermetic for assert_cmd::Command {
    fn hermetic(&mut self) -> &mut Self {
        for name in scrubbed() {
            self.env_remove(name);
        }
        for (name, value) in SET {
            self.env(name, value);
        }
        self
    }
}

/// A command a test holds by a mutable reference, as a helper that cleans it does.
impl<T: Hermetic + ?Sized> Hermetic for &mut T {
    fn hermetic(&mut self) -> &mut Self {
        (**self).hermetic();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_changes_a_runner_is_scrubbed_and_tool_paths_are_kept() {
        for name in [
            "PYTEST_ADDOPTS",
            "PYTHONPATH",
            "PYTHONDONTWRITEBYTECODE",
            "NODE_OPTIONS",
            "CMAKE_GENERATOR",
            "CTEST_PARALLEL_LEVEL",
            "GOFLAGS",
        ] {
            assert!(scrubs(name), "{name}");
        }
        for name in [
            "PATH",
            "HOME",
            "JAVA_HOME",
            "HUSHRUN_TEST_PYTHON",
            "HUSHRUN_TEST_CMAKE",
            "GOPATH",
        ] {
            assert!(!scrubs(name), "{name}");
        }
        let mut cmd = std::process::Command::new("true");
        cmd.hermetic();
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "PYTHONDONTWRITEBYTECODE" && v == Some("1".as_ref()))
        );
    }
}
