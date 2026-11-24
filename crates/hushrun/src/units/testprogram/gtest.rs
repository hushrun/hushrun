//! \[ADR 0120\] GoogleTest, for a test program COMMAND starts itself: what its arguments and
//! environment let hushrun do, its listing, the filter that leaves tests out, and its XML.
//!
//! GoogleTest takes `GTEST_FILTER` and `GTEST_OUTPUT` from the environment when its command line
//! names neither, so hushrun leaves tests out and reads what each came to without changing a
//! word of COMMAND. A test's id is its `--gtest_filter`, `Suite.Name`, as for a test CTest's
//! `gtest_discover_tests` made (ADR 0090).

use super::{Case, Outcome};

/// What COMMAND's own arguments let the adapter do: `Err` why it can't observe the program at
/// all, `Ok(Some)` why it observes it and leaves nothing out.
pub fn args(args: &[String]) -> Result<Option<String>, String> {
    let mut observe = None;
    for arg in args {
        let Some(flag) = flag(arg) else { continue };
        match flag.as_str() {
            "gtest_output" => {
                return Err(format!(
                    "COMMAND names its own {arg}, which takes the place of the file hushrun reads what each test came to from"
                ));
            }
            "gtest_list_tests" => {
                return Err("COMMAND lists the program's tests and runs none".into());
            }
            "gtest_repeat" => {
                return Err(format!(
                    "COMMAND runs each test more than once ({arg}), and a pass may follow a failure"
                ));
            }
            "gtest_fail_fast" => {
                return Err(format!(
                    "with {arg} GoogleTest writes the tests after a failure as skipped although they never ran"
                ));
            }
            "gtest_flagfile" => {
                return Err(format!(
                    "COMMAND reads GoogleTest's flags from a file ({arg}), which may name any of them"
                ));
            }
            "gtest_filter" => {
                observe = Some(format!(
                    "COMMAND names its own {arg}, which wins over the GTEST_FILTER hushrun would leave tests out with"
                ));
            }
            _ => {}
        }
    }
    Ok(observe)
}

/// A GoogleTest flag's name as GoogleTest reads it: after `--`, `-` or `/`, before `=`, with
/// `-` read as `_`.
fn flag(arg: &str) -> Option<String> {
    let rest = arg
        .strip_prefix("--")
        .or_else(|| arg.strip_prefix('-'))
        .or_else(|| arg.strip_prefix('/'))?;
    let name = rest.split('=').next().unwrap_or(rest).replace('-', "_");
    name.starts_with("gtest_").then_some(name)
}

/// What COMMAND's environment lets the adapter do, as [`args`] says it.
pub fn env(get: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Result<Option<String>, String> {
    for name in [
        "GTEST_OUTPUT",
        "GTEST_REPEAT",
        "GTEST_FAIL_FAST",
        "GTEST_FLAGFILE",
        "TESTBRIDGE_TEST_RUNNER_FAIL_FAST",
    ] {
        if get(name).is_some() {
            return Err(format!(
                "{name} is set, which changes what GoogleTest runs or where it writes what each test came to"
            ));
        }
    }
    for name in ["GTEST_FILTER", "TESTBRIDGE_TEST_ONLY"] {
        if get(name).is_some() {
            return Ok(Some(format!(
                "{name} is set, which the GTEST_FILTER hushrun would leave tests out with takes the place of"
            )));
        }
    }
    for name in ["GTEST_TOTAL_SHARDS", "GTEST_SHARD_INDEX"] {
        if get(name).is_some() {
            return Ok(Some(format!(
                "{name} is set: the program runs one shard of its tests, and leaving any out would move the others between shards"
            )));
        }
    }
    Ok(None)
}

/// The ids `--gtest_list_tests` printed, in order: `Suite.Name`, without the comments typed and
/// value-parameterized tests carry. Lines a `main` of the project's own printed besides are
/// passed over.
pub fn listing(stdout: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut suite: Option<String> = None;
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        let bare = line.split("  # ").next().unwrap_or(line);
        if let Some(name) = bare.strip_prefix("  ") {
            if let Some(suite) = &suite
                && !name.is_empty()
                && !name.contains(char::is_whitespace)
            {
                out.push(format!("{suite}{name}"));
            }
        } else if bare.ends_with('.') && !bare.contains(char::is_whitespace) {
            suite = Some(bare.to_string());
        } else {
            suite = None;
        }
    }
    out
}

/// The `GTEST_FILTER` that leaves out `ids`: `-*` when every test is left out.
pub fn filter(ids: &[&str], whole: bool) -> String {
    if whole {
        "-*".to_string()
    } else {
        format!("-{}", ids.join(":"))
    }
}

/// Whether an id can be named in a filter exactly: none of the filter's own characters.
pub fn nameable(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('-') && !id.contains([':', '*', '?'])
}

/// What GoogleTest's XML file says each test came to, by id, in order. A `<testcase>` with a
/// `<failure>` or `<error>` failed; `result="skipped"` was skipped; `status="notrun"` is a
/// disabled test, which didn't run; any other `status="run"` passed. A failure outside every
/// test (an environment's `SetUp`) is a case with an empty id that failed.
pub fn cases(bytes: &[u8]) -> Result<Vec<Case>, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let text = std::str::from_utf8(bytes).map_err(|_| "it isn't UTF-8")?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut cases = Vec::new();
    let mut open: Option<Case> = None;
    let mut suites = 0;
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("it doesn't parse: {err}"))?;
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "testsuites" => {
                suites += 1;
            }
            Event::Start(e) if e.local_name().as_ref() == "testcase" => {
                open = Some(case(&e)?);
            }
            Event::Empty(e) if e.local_name().as_ref() == "testcase" => {
                cases.push(case(&e)?);
            }
            Event::Start(e) | Event::Empty(e)
                if matches!(e.local_name().as_ref(), "failure" | "error") =>
            {
                if let Some(case) = &mut open {
                    case.outcome = Outcome::Failed;
                }
            }
            Event::End(e) if e.local_name().as_ref() == "testcase" => {
                cases.extend(open.take());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if suites == 0 {
        return Err("it holds no <testsuites>".into());
    }
    Ok(cases)
}

fn case(e: &quick_xml::events::BytesStart<'_>) -> Result<Case, String> {
    let get = |name: &str| super::attribute(e, name);
    let suite = get("classname")?.unwrap_or_default();
    let name = get("name")?.unwrap_or_default();
    let status = get("status")?.unwrap_or_default();
    let result = get("result")?.unwrap_or_default();
    let outcome = if status == "notrun" {
        Outcome::Disabled
    } else if result == "skipped" {
        Outcome::Skipped
    } else {
        Outcome::Passed
    };
    let id = if suite.is_empty() && name.is_empty() {
        String::new()
    } else {
        format!("{suite}.{name}")
    };
    Ok(Case {
        id,
        outcome,
        duration_ms: super::millis(get("time")?.as_deref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    // covers: C-106
    #[test]
    fn its_own_flags_say_what_can_be_done() {
        assert_eq!(args(&words("--gtest_shuffle --gtest_color=no")), Ok(None));
        assert!(matches!(args(&words("--gtest_filter=Add.*")), Ok(Some(_))));
        assert!(matches!(args(&words("-gtest_filter=Add.*")), Ok(Some(_))));
        assert!(matches!(args(&words("--gtest-filter=Add.*")), Ok(Some(_))));
        for refused in [
            "--gtest_output=xml:x.xml",
            "/gtest_output=json",
            "--gtest_list_tests",
            "--gtest_repeat=3",
            "--gtest_fail_fast",
            "--gtest_flagfile=f",
        ] {
            assert!(args(&words(refused)).is_err(), "{refused}");
        }
        let none = |_: &str| None;
        assert_eq!(env(&none), Ok(None));
        let sharded = |n: &str| (n == "GTEST_TOTAL_SHARDS").then(|| "4".into());
        assert!(matches!(env(&sharded), Ok(Some(_))));
        let output = |n: &str| (n == "GTEST_OUTPUT").then(|| "xml".into());
        assert!(env(&output).is_err());
    }

    // covers: C-106
    #[test]
    fn the_listing_names_each_test_by_its_filter() {
        let printed = "Running main() from ./googletest/src/gtest_main.cc\nAdd.\n  One\n  DISABLED_Three\nTy/0.  # TypeParam = int\n  Works\nEv/P.\n  Even/0  # GetParam() = 2\n";
        assert_eq!(
            listing(printed),
            ["Add.One", "Add.DISABLED_Three", "Ty/0.Works", "Ev/P.Even/0"]
        );
        assert_eq!(
            filter(&["Add.One", "Ev/P.Even/0"], false),
            "-Add.One:Ev/P.Even/0"
        );
        assert_eq!(filter(&[], true), "-*");
    }

    // covers: C-106
    #[test]
    fn its_xml_says_what_each_test_came_to() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites tests="4" name="AllTests">
  <testsuite name="Add" tests="3">
    <testcase name="One" status="run" result="completed" time="0.002" classname="Add" />
    <testcase name="Two" status="run" result="completed" time="0." classname="Add">
      <failure message="x" type=""><![CDATA[g.cpp:4]]></failure>
    </testcase>
    <testcase name="DISABLED_Three" status="notrun" result="suppressed" time="0." classname="Add" />
    <testcase name="Skip" status="run" result="skipped" time="0." classname="Sk">
      <skipped message="no"/>
    </testcase>
  </testsuite>
</testsuites>"#;
        let got = cases(xml).unwrap();
        let outcomes: Vec<(&str, Outcome)> =
            got.iter().map(|c| (c.id.as_str(), c.outcome)).collect();
        assert_eq!(
            outcomes,
            [
                ("Add.One", Outcome::Passed),
                ("Add.Two", Outcome::Failed),
                ("Add.DISABLED_Three", Outcome::Disabled),
                ("Sk.Skip", Outcome::Skipped),
            ]
        );
        assert_eq!(got[0].duration_ms, 2);
        assert!(cases(b"<testsuites><testcase").is_err());
        assert!(cases(b"<other/>").is_err());
    }
}
