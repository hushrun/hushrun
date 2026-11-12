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
//! method has that name: every overload, every invocation; a name is compared whole, but that a
//! JUnit Vintage test's `name[…]`, JUnit 4's Parameterized invocation, is `name`'s). Class names
//! are binary names, as `Class.getName()` gives them. Nothing is a pattern. A line
//! `@<directory>` scopes the entries after it to a JVM whose `basedir` system property, else working
//! directory, is that directory (canonical paths). No list leaves nothing out; a list that can't be
//! read leaves nothing out, and the events say so. The events' lines are described in the hook's
//! `Events.java`: each test the hook left out is told with the entry that listed it, which
//! [`left_out_unlisted`] holds against the list.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// The test-JVM hook, as `cargo xtask jars` builds it.
pub const TEST_JVM_HOOK: &[u8] = include_bytes!("../../jvm/hushrun-test-jvm-hook.jar");
/// Its file name in the cache.
pub const TEST_JVM_HOOK_NAME: &str = "hushrun-test-jvm-hook.jar";
/// The system property that names the list of tests to leave out.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the hook reads it; Gradle's script passes it as written, which a test checks"
    )
)]
pub const LEAVE_OUT_PROPERTY: &str = "hushrun.tests.leaveOut";
/// The variable that names it when the property isn't set.
pub const LEAVE_OUT_VARIABLE: &str = "HUSHRUN_TESTS_LEAVE_OUT";
/// The list's path below `HUSHRUN_UNITS` when neither names one.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the hook reads it; Maven's extension writes the list there itself"
    )
)]
pub const LEAVE_OUT_FILE: &str = "test-jvm/leave-out.txt";
/// The system property that names the directory for the events.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the hook reads it; Gradle's script passes it as written, which a test checks"
    )
)]
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

/// The most bytes of one event file hushrun reads: past it, the file counts for nothing.
pub const MOST_EVENT_BYTES: u64 = 512 * 1024 * 1024;

/// What one test JVM's event file says, read: by top-level class, each test's events, with what
/// can't be tied to a class counted apart. It decides nothing: the adapter turns it into facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JvmEvents {
    /// The JVM's working directory and its `basedir` system property, as the `jvm` line said.
    pub dir: Option<String>,
    pub basedir: Option<String>,
    /// The JUnit Platform's listener ran: the events of tests are there.
    pub listened: bool,
    /// A line didn't read, and what followed it was left: the JVM died writing, or the file was
    /// cut.
    pub torn: bool,
    /// Why the list couldn't be read, when it couldn't.
    pub list_unread: Option<String>,
    /// By top-level class, `pkg.Outer`.
    pub classes: BTreeMap<String, ClassEvents>,
    /// Tests, or failures, whose class can't be told.
    pub unattributed: u64,
    /// What the hook left out, each as it said it: class, method, and the entry that listed it.
    pub left_out: Vec<LeftOutEvent>,
    /// Each test that started or was skipped, `class#method` as its source names them.
    pub seen: BTreeSet<String>,
}

/// One test, or class, the hook left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeftOutEvent {
    pub class: String,
    pub method: Option<String>,
    /// The list's entry that listed it, as the list spells it.
    pub entry: Option<String>,
}

/// What a JVM's events say of one top-level class and the classes nested in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassEvents {
    /// Each test by its id: its method, after its nested class and `#` when it has one; a JUnit
    /// Vintage test's up to its first `[`.
    pub tests: BTreeMap<String, TestEvents>,
    /// The class's own container started, and finished.
    pub started: bool,
    pub finished: bool,
    /// The class's own container was skipped (disabled), so none of its tests ran.
    pub skipped: bool,
    /// The class failed outside any test: a container of it failed (a failing `@BeforeAll`).
    pub failed: bool,
    /// Something of it started and never finished.
    pub open: bool,
    /// A test of it had no method to name it by.
    pub unnamed: bool,
    /// The hook left out something of it.
    pub left_out: bool,
    /// Why the hook kept something of it that the list named.
    pub kept: BTreeSet<String>,
}

/// What a JVM's events say of one test, over every invocation of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestEvents {
    /// An invocation started.
    pub started: bool,
    /// An invocation started and never finished.
    pub open: bool,
    /// An invocation finished, or was skipped.
    pub finished: bool,
    /// An invocation failed, or its container did.
    pub failed: bool,
    /// An invocation was skipped or aborted (an assumption).
    pub skipped: bool,
    /// The hook left it out.
    pub left_out: bool,
    pub duration_ms: u64,
}

/// The binary name of a class and the method a JUnit Platform unique id names, for an event whose
/// source says neither: `[class:pkg.C]/[nested-class:Inner]/[method:m(int)]` is `pkg.C$Inner` and
/// `m`.
fn from_uid(uid: &str) -> (Option<String>, Option<String>) {
    let mut class: Option<String> = None;
    let mut method = None;
    for segment in uid.split("]/[") {
        let segment = segment.trim_start_matches('[').trim_end_matches(']');
        let Some((kind, value)) = segment.split_once(':') else {
            continue;
        };
        match kind {
            "class" => class = Some(value.to_string()),
            "nested-class" => {
                if let Some(c) = &mut class {
                    c.push('$');
                    c.push_str(value);
                }
            }
            "method" | "test-template" | "test-factory" => {
                method = Some(value.split('(').next().unwrap_or(value).to_string());
            }
            _ => {}
        }
    }
    (class, method)
}

/// A test's top-level class and id, from its class's binary name and its method's name: whole,
/// or up to its first `[` when `cut` (JUnit 4's Parameterized names an invocation `name[0: x=1]`).
pub fn test_id(class: &str, method: &str, cut: bool) -> (String, String) {
    let (top, nested) = match class.split_once('$') {
        Some((top, nested)) => (top, nested),
        None => (class, ""),
    };
    let method = if cut {
        method.split('[').next().unwrap_or(method)
    } else {
        method
    };
    let id = if nested.is_empty() {
        method.to_string()
    } else {
        format!("{nested}#{method}")
    };
    (top.to_string(), id)
}

/// Reads one event file's bytes.
pub fn read_events(bytes: &[u8]) -> JvmEvents {
    let mut out = JvmEvents::default();
    // Each invocation by its unique id: (top class, test id when a test, still open).
    let mut uids: BTreeMap<String, (String, Option<String>, bool)> = BTreeMap::new();
    let text = String::from_utf8_lossy(bytes);
    for line in text.split('\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            out.torn = true;
            break;
        };
        let field = |name: &str| event.get(name).and_then(serde_json::Value::as_str);
        let is_test = event.get("test").and_then(serde_json::Value::as_bool) == Some(true);
        // JUnit Vintage names a Parameterized invocation `name[…]`: one test, `name`.
        let vintage = field("uid").is_some_and(|u| u.starts_with("[engine:junit-vintage]"));
        let (mut class, mut method) = (
            field("class").map(str::to_string),
            field("method").map(str::to_string),
        );
        if class.is_none()
            && let Some(uid) = field("uid")
        {
            (class, method) = from_uid(uid);
        }
        match field("e").unwrap_or("") {
            "jvm" => {
                out.dir = field("dir").map(str::to_string);
                out.basedir = field("basedir").map(str::to_string);
            }
            "plan-started" | "plan-finished" => out.listened = true,
            "leave-out-unread" => out.list_unread = Some(field("error").unwrap_or("").to_string()),
            kind @ ("left-out" | "not-left-out") => {
                let Some(class) = class else { continue };
                if kind == "left-out" {
                    out.left_out.push(LeftOutEvent {
                        class: class.clone(),
                        method: method.clone(),
                        entry: field("entry").map(str::to_string),
                    });
                }
                let (top, id) = test_id(&class, method.as_deref().unwrap_or(""), vintage);
                let entry = out.classes.entry(top).or_default();
                if kind == "left-out" {
                    entry.left_out = true;
                    if method.is_some() {
                        entry.tests.entry(id).or_default().left_out = true;
                    }
                } else {
                    entry.kept.insert(field("reason").unwrap_or("").to_string());
                }
            }
            kind @ ("started" | "skipped" | "finished") => {
                out.listened = true;
                let failed = kind == "finished" && field("status") == Some("FAILED");
                let Some(class) = class else {
                    // An engine's own container, or a test with no class: nothing ties it to a
                    // class, so a failure of it, or any test, is no class's.
                    if is_test || failed {
                        out.unattributed += 1;
                    }
                    continue;
                };
                if let Some(m) = &method
                    && (kind == "started" || kind == "skipped")
                {
                    out.seen.insert(format!("{class}#{m}"));
                }
                let (top, id) = test_id(&class, method.as_deref().unwrap_or(""), vintage);
                let uid = field("uid").unwrap_or("").to_string();
                let entry = out.classes.entry(top.clone()).or_default();
                let own_class = class == top && method.is_none();
                if is_test && method.is_none() {
                    entry.unnamed = true;
                }
                let test = (is_test || method.is_some()).then(|| id.clone());
                match kind {
                    "started" => {
                        uids.insert(uid, (top, test.clone(), true));
                        if let Some(id) = &test {
                            entry.tests.entry(id.clone()).or_default().started = true;
                        } else if own_class {
                            entry.started = true;
                        }
                    }
                    "skipped" => {
                        if let Some(id) = &test {
                            let t = entry.tests.entry(id.clone()).or_default();
                            t.finished = true;
                            t.skipped = true;
                        } else if own_class {
                            entry.skipped = true;
                        }
                    }
                    _ => {
                        if let Some(open) = uids.get_mut(&uid) {
                            open.2 = false;
                        }
                        let ms = event
                            .get("ms")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        if let Some(id) = &test {
                            let t = entry.tests.entry(id.clone()).or_default();
                            if is_test {
                                t.finished = true;
                                t.duration_ms = t.duration_ms.saturating_add(ms);
                                if field("status") == Some("ABORTED") {
                                    t.skipped = true;
                                }
                            }
                            t.failed |= failed;
                        } else {
                            entry.failed |= failed;
                            if own_class {
                                entry.finished = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (top, test, open) in uids.into_values() {
        if !open {
            continue;
        }
        let entry = out.classes.entry(top).or_default();
        entry.open = true;
        if let Some(id) = test {
            entry.tests.entry(id).or_default().open = true;
        }
    }
    out
}

/// \[C-32\] Why the hook's events say it left out what `listed`, the lines of its list, didn't
/// list: a test it told no entry for, or one whose entry, which the list must hold, doesn't name
/// it exactly (a class entry its class or a class it is nested in; a method entry its class and
/// its method, or for JUnit 4's Parameterized `method[…]`). `None` when every test it left out
/// was listed.
pub fn left_out_unlisted<'a>(
    events: impl IntoIterator<Item = &'a JvmEvents>,
    listed: &BTreeSet<String>,
) -> Option<String> {
    for jvm in events {
        for out in &jvm.left_out {
            let what = match &out.method {
                Some(m) => format!("{}#{m}", out.class),
                None => out.class.clone(),
            };
            let Some(entry) = out.entry.as_ref().filter(|e| listed.contains(*e)) else {
                return Some(format!(
                    "the test-JVM hook left out {what}, which no entry of its list names"
                ));
            };
            let names = match entry.split_once('#') {
                None => {
                    out.class == *entry
                        || out
                            .class
                            .strip_prefix(entry.as_str())
                            .is_some_and(|rest| rest.starts_with('$'))
                }
                Some((class, method)) => {
                    out.class == class
                        && out.method.as_deref().is_some_and(|m| {
                            m == method
                                || m.strip_prefix(method).is_some_and(|rest| {
                                    rest.starts_with('[') && rest.ends_with(']')
                                })
                        })
                }
            };
            if !names {
                return Some(format!(
                    "the test-JVM hook left out {what} for the entry {entry}, which names another test"
                ));
            }
        }
    }
    None
}

/// Every event file in `dir`, read, by file name: `Err` when the directory is there and can't be
/// read, or a file is larger than [`MOST_EVENT_BYTES`]. No directory is no file.
pub fn read_events_dir(dir: &Path) -> Result<BTreeMap<String, JvmEvents>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(format!("can't read {}: {err}", dir.display())),
    };
    let mut out = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("can't read {}: {err}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size > MOST_EVENT_BYTES {
            return Err(format!("{} is too large to read", path.display()));
        }
        let bytes =
            std::fs::read(&path).map_err(|err| format!("can't read {}: {err}", path.display()))?;
        out.insert(name, read_events(&bytes));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names above are the hook's, as its sources spell them, and Gradle's script passes the
    /// two properties as written.
    #[test]
    fn the_names_are_those_the_hook_and_gradles_script_use() {
        let leave_out = include_str!("../../jvm/test-jvm-hook/dev/hushrun/testjvm/LeaveOut.java");
        let events = include_str!("../../jvm/test-jvm-hook/dev/hushrun/testjvm/Events.java");
        let names = include_str!("../../jvm/test-jvm-hook/dev/hushrun/testjvm/Names.java");
        let gradle = include_str!("hushrun_gradle.gradle");
        for (source, property, variable, child) in [
            (
                leave_out,
                LEAVE_OUT_PROPERTY,
                LEAVE_OUT_VARIABLE,
                LEAVE_OUT_FILE,
            ),
            (events, EVENTS_PROPERTY, EVENTS_VARIABLE, EVENTS_DIR),
        ] {
            assert!(
                source.contains(&format!("PROPERTY = \"{property}\"")),
                "{property}"
            );
            assert!(
                source.contains(&format!("VARIABLE = \"{variable}\"")),
                "{variable}"
            );
            let child = child.strip_prefix("test-jvm/").unwrap();
            assert!(
                source.contains(&format!("Names.of(PROPERTY, VARIABLE, \"{child}\")")),
                "{child}"
            );
            assert!(gradle.contains(&format!("\"-D{property}=\"")), "{property}");
        }
        assert!(names.contains("new File(v.trim(), \"test-jvm\")"));
    }

    // covers: C-32, C-39
    #[test]
    fn a_test_the_hook_left_out_is_one_its_list_named_exactly() {
        let events = |lines: &str| {
            read_events(
                format!("{{\"e\":\"jvm\",\"pid\":\"1\",\"dir\":\"/w\",\"basedir\":null}}\n{lines}")
                    .as_bytes(),
            )
        };
        let listed: BTreeSet<String> = ["ex.Spec#parses [a]", "p.Param#small", "p.Outer"]
            .map(String::from)
            .into();
        let ok = events(concat!(
            r#"{"e":"left-out","by":"junit-platform","uid":"[engine:spock]/[spec:ex.Spec]/[feature:$spock_feature_0_0]","class":"ex.Spec","method":"parses [a]","entry":"ex.Spec#parses [a]"}"#,
            "\n",
            r#"{"e":"left-out","by":"junit-platform","uid":"[engine:junit-vintage]/[runner:p.Param]/[test:small[0: x=1](p.Param)]","class":"p.Param","method":"small[0: x=1]","entry":"p.Param#small"}"#,
            "\n",
            r#"{"e":"left-out","by":"testng","uid":null,"class":"p.Outer$Inner","method":"m","entry":"p.Outer"}"#,
            "\n",
        ));
        assert_eq!(left_out_unlisted([&ok], &listed), None);
        // A Vintage invocation is one test, `small`, as its id says; a Spock feature is its name.
        assert!(ok.classes["p.Param"].tests.contains_key("small"));
        assert!(ok.classes["ex.Spec"].tests.contains_key("parses [a]"));
        for (line, says) in [
            // A feature that differs after a bracket, which the list didn't name.
            (
                r#"{"e":"left-out","by":"junit-platform","uid":"u","class":"ex.Spec","method":"parses [b]","entry":"ex.Spec#parses [a]"}"#,
                "names another test",
            ),
            (
                r#"{"e":"left-out","by":"testng","uid":null,"class":"p.Sub","method":"small","entry":"p.Param#small"}"#,
                "names another test",
            ),
            (
                r#"{"e":"left-out","by":"testng","uid":null,"class":"p.OuterX","method":"m","entry":"p.Outer"}"#,
                "names another test",
            ),
            (
                r#"{"e":"left-out","by":"testng","uid":null,"class":"p.Other","method":"m","entry":"p.Other#m"}"#,
                "no entry of its list",
            ),
            (
                r#"{"e":"left-out","by":"testng","uid":null,"class":"p.Other","method":"m"}"#,
                "no entry of its list",
            ),
        ] {
            let why = left_out_unlisted([&events(&format!("{line}\n"))], &listed)
                .unwrap_or_else(|| panic!("{line}"));
            assert!(why.contains(says), "{why}");
        }
    }

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
