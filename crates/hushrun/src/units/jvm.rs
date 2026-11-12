//! \[ADR 0085\] The JVM jars this binary embeds, for the adapters of JVM build tools, and what
//! they read and write.
//!
//! Each jar is built from Java sources in this repository by `cargo xtask jars`, from pinned
//! inputs, and checked in; CI builds it again and fails when the bytes differ. A run writes the
//! jar it needs to `<cache dir>/adapters/jvm-<first 16 hex of its SHA-256>/`, whole.
//!
//! The test-JVM hook ([`TEST_JVM_HOOK`]) goes on a test JVM's classpath. It has no part specific
//! to a build tool:
//!
//! - a JUnit Platform filter, after discovery, that leaves out the tests a list names, and nothing
//!   else;
//! - a TestNG method interceptor that does the same inside TestNG, and leaves nothing of a test
//!   context out when a method of it depends on another method or a group;
//! - a JUnit Platform listener that writes each test's and container's events, flushed one line at
//!   a time, to a file of its own in a directory.
//!
//! The list is the file the system property [`LEAVE_OUT_PROPERTY`] names, else the variable
//! [`LEAVE_OUT_VARIABLE`], else `$HUSHRUN_UNITS/test-jvm/leave-out.txt` ([`LEAVE_OUT_FILE`]); the
//! directory is [`EVENTS_PROPERTY`], else [`EVENTS_VARIABLE`], else `$HUSHRUN_UNITS/test-jvm/events`
//! ([`EVENTS_DIR`]). The last form follows the run's directory into a second run, whose
//! `HUSHRUN_UNITS` is another.
//!
//! The list is UTF-8, one entry a line, blank lines and lines starting with `#` ignored:
//! `pkg.Outer` (that class and every class nested in it), `pkg.Outer$Inner` (that nested class
//! down), `pkg.Outer#method` and `pkg.Outer$Inner#method` (every test of exactly that class whose
//! method has that name: every overload, every invocation; a name is compared up to its first
//! `[`). Class names are binary names, as `Class.getName()` gives them. Nothing is a pattern. A line
//! `@<directory>` scopes the entries after it to a JVM whose `basedir` system property, else working
//! directory, is that directory (canonical paths). No list leaves nothing out; a list that can't be
//! read leaves nothing out, and the events say so. The events' lines are described in the hook's
//! `Events.java`.

#![allow(dead_code, reason = "the adapters of JVM build tools load it")]

use std::io;
use std::path::{Path, PathBuf};

/// The test-JVM hook, as `cargo xtask jars` builds it.
pub const TEST_JVM_HOOK: &[u8] = include_bytes!("../../jvm/hushrun-test-jvm-hook.jar");
/// Its file name in the cache.
pub const TEST_JVM_HOOK_NAME: &str = "hushrun-test-jvm-hook.jar";
/// The system property that names the list of tests to leave out.
pub const LEAVE_OUT_PROPERTY: &str = "hushrun.tests.leaveOut";
/// The variable that names it when the property isn't set.
pub const LEAVE_OUT_VARIABLE: &str = "HUSHRUN_TESTS_LEAVE_OUT";
/// The list's path below `HUSHRUN_UNITS` when neither names one.
pub const LEAVE_OUT_FILE: &str = "test-jvm/leave-out.txt";
/// The system property that names the directory for the events.
pub const EVENTS_PROPERTY: &str = "hushrun.tests.events";
/// The variable that names it when the property isn't set.
pub const EVENTS_VARIABLE: &str = "HUSHRUN_TESTS_EVENTS";
/// The events' directory below `HUSHRUN_UNITS` when neither names one.
pub const EVENTS_DIR: &str = "test-jvm/events";

/// The path of `bytes`, written whole as `<cache dir>/adapters/jvm-<digest>/<name>` when it isn't
/// there with these bytes yet.
pub fn install(cache: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let digest = hushrun_core::Digest::of(bytes).hex();
    let path = cache
        .join("adapters")
        .join(format!("jvm-{}", &digest[..16]))
        .join(name);
    if std::fs::read(&path).ok().as_deref() != Some(bytes) {
        super::facts::write_whole(&path, bytes)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_is_a_jar_with_its_services_and_written_whole_once() {
        assert_eq!(&TEST_JVM_HOOK[..4], b"PK\x03\x04");
        for service in [
            "META-INF/services/org.junit.platform.launcher.PostDiscoveryFilter",
            "META-INF/services/org.junit.platform.launcher.TestExecutionListener",
            "META-INF/services/org.testng.ITestNGListener",
        ] {
            assert!(
                TEST_JVM_HOOK
                    .windows(service.len())
                    .any(|w| w == service.as_bytes()),
                "{service}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let path = install(dir.path(), TEST_JVM_HOOK_NAME, TEST_JVM_HOOK).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), TEST_JVM_HOOK);
        assert_eq!(
            install(dir.path(), TEST_JVM_HOOK_NAME, TEST_JVM_HOOK).unwrap(),
            path
        );
    }
}
