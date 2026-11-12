//! \[ADR 0087\] What `ctest --output-junit` (CMake 3.21 and later) says each test came to, read so
//! that nothing absent, unreadable or ambiguous ever reads as a pass.
//!
//! - The file must exist, parse, and hold as many `<testcase>` as its `<testsuite>`'s `tests`
//!   says; otherwise nothing of it is read.
//! - `status="run"` passed; `fail` failed; `notrun` with a skip's message
//!   (`SKIP_RETURN_CODE=…`, `SKIP_REGULAR_EXPRESSION_MATCHED`) was skipped, which passed;
//!   `notrun` with any other (a missing program, a fixture that failed) didn't run; `disabled`
//!   didn't run.
//! - Cases are matched to the tests that ran by name, in order: a test whose name has no case
//!   didn't run (ran: `--stop-on-failure` leaves later tests out of the file); a name with as many
//!   cases as tests is matched one to one; any other count can't be told apart.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// What one test came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    /// Skipped as the test asked (`SKIP_RETURN_CODE`, `SKIP_REGULAR_EXPRESSION`): a pass, as a
    /// skipped test is elsewhere in hushrun.
    Skipped,
    /// It didn't run: a missing program, a failed fixture, disabled, or absent from the file.
    NotRun,
    /// Its name has as many cases as tests nowhere: which is which can't be told. `failed` when
    /// any case of the name failed, which then counts as each test's failure.
    Unknown {
        failed: bool,
    },
}

/// One `<testcase>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub name: String,
    pub outcome: Outcome,
    pub duration_ms: u64,
}

/// The cases of a JUnit file ctest wrote, in order, or why none can be read.
pub fn read(bytes: &[u8]) -> Result<Vec<Case>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "it isn't UTF-8")?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut declared: Option<u64> = None;
    let mut cases: Vec<Case> = Vec::new();
    let mut open: Option<(String, String, u64)> = None;
    let mut message: Option<String> = None;
    let mut suites = 0;
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("it doesn't parse: {err}"))?;
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "testsuite" => {
                suites += 1;
                let tests = attribute(&e, "tests")?
                    .and_then(|t| t.parse::<u64>().ok())
                    .ok_or("its testsuite says no number of tests")?;
                declared = Some(declared.unwrap_or(0) + tests);
            }
            Event::Start(e) if e.local_name().as_ref() == "testcase" => {
                open = Some(case_of(&e)?);
                message = None;
            }
            Event::Empty(e) if e.local_name().as_ref() == "testcase" => {
                let (name, status, ms) = case_of(&e)?;
                cases.push(finish(name, &status, None, ms));
            }
            Event::Start(e) | Event::Empty(e)
                if open.is_some()
                    && matches!(e.local_name().as_ref(), "skipped" | "failure" | "error") =>
            {
                message = Some(attribute(&e, "message")?.unwrap_or_default());
            }
            Event::End(e) if e.local_name().as_ref() == "testcase" => {
                let (name, status, ms) = open.take().ok_or("a testcase closes twice")?;
                cases.push(finish(name, &status, message.take(), ms));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if open.is_some() {
        return Err("a testcase doesn't close".into());
    }
    match declared {
        None => Err("it has no testsuite".into()),
        Some(n) if n != cases.len() as u64 => Err(format!(
            "it says {n} tests and holds {} in {suites} testsuite(s)",
            cases.len()
        )),
        Some(_) => Ok(cases),
    }
}

fn attribute(e: &BytesStart<'_>, name: &str) -> Result<Option<String>, String> {
    for attr in e.attributes() {
        let attr = attr.map_err(|err| format!("an attribute doesn't parse: {err}"))?;
        if attr.key.local_name().as_ref() == name {
            let value = quick_xml::escape::unescape(&attr.value)
                .map_err(|err| format!("an attribute doesn't parse: {err}"))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn case_of(e: &BytesStart<'_>) -> Result<(String, String, u64), String> {
    let name = attribute(e, "name")?.ok_or("a testcase has no name")?;
    let status = attribute(e, "status")?.unwrap_or_default();
    let seconds: f64 = attribute(e, "time")?
        .and_then(|t| t.parse().ok())
        .unwrap_or(0.0);
    let ms = (seconds.max(0.0) * 1000.0).round();
    Ok((name, status, if ms.is_finite() { ms as u64 } else { 0 }))
}

fn finish(name: String, status: &str, message: Option<String>, duration_ms: u64) -> Case {
    let outcome = match status {
        "run" => Outcome::Passed,
        "fail" => Outcome::Failed,
        "notrun"
            if message.as_deref().is_some_and(|m| {
                m.starts_with("SKIP_RETURN_CODE") || m.starts_with("SKIP_REGULAR_EXPRESSION")
            }) =>
        {
            Outcome::Skipped
        }
        _ => Outcome::NotRun,
    };
    Case {
        name,
        outcome,
        duration_ms,
    }
}

/// What each of `names`, the tests that ran in order (some names more than once), came to by
/// `cases`.
pub fn matched(names: &[&str], cases: &[Case]) -> Vec<(Outcome, u64)> {
    let mut by_name: std::collections::BTreeMap<&str, Vec<&Case>> = Default::default();
    for case in cases {
        by_name.entry(&case.name).or_default().push(case);
    }
    let mut wanted: std::collections::BTreeMap<&str, usize> = Default::default();
    for name in names {
        *wanted.entry(name).or_default() += 1;
    }
    let mut taken: std::collections::BTreeMap<&str, usize> = Default::default();
    names
        .iter()
        .map(|name| {
            let found = by_name.get(name).map(Vec::as_slice).unwrap_or_default();
            if found.is_empty() {
                return (Outcome::NotRun, 0);
            }
            if found.len() != wanted[name] {
                let failed = found.iter().any(|c| c.outcome == Outcome::Failed);
                return (Outcome::Unknown { failed }, 0);
            }
            let n = taken.entry(name).or_default();
            let case = found[*n];
            *n += 1;
            (case.outcome, case.duration_ms)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What CTest 4.4 wrote for the spike's sample, trimmed (runs/A_c_plain_44).
    const WRITTEN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="Linux" tests="7" failures="2" disabled="1" skipped="2" hostname="" time="1" timestamp="2026-11-12T10:00:00">
	<testcase name="add" classname="add" time="0.01" status="run">
		<properties><property name="cmake_labels" value=""/></properties>
		<system-out>ok</system-out>
	</testcase>
	<testcase name="fail" classname="fail" time="0.02" status="fail">
		<failure message="Failed"/>
		<system-out>status=&quot;run&quot; printed by the test</system-out>
	</testcase>
	<testcase name="skipme" classname="skipme" time="0" status="notrun">
		<skipped message="SKIP_RETURN_CODE=77"/>
	</testcase>
	<testcase name="missing_exe" classname="missing_exe" time="0" status="notrun">
		<skipped message="Unable to find executable"/>
	</testcase>
	<testcase name="disabled_one" classname="disabled_one" time="0" status="disabled"/>
	<testcase name="weird name+(1)[x].*?|$^ \end &lt;&amp;&gt;" classname="w" time="1.5" status="run"/>
	<testcase name="crash" classname="crash" time="0" status="fail"><failure message="SEGFAULT"/></testcase>
</testsuite>
"#;

    // covers: C-49
    #[test]
    fn the_reader_reads_each_status_and_never_a_pass_from_absence() {
        let cases = read(WRITTEN.as_bytes()).unwrap();
        let got: Vec<(&str, Outcome)> =
            cases.iter().map(|c| (c.name.as_str(), c.outcome)).collect();
        assert_eq!(
            got,
            [
                ("add", Outcome::Passed),
                ("fail", Outcome::Failed),
                ("skipme", Outcome::Skipped),
                ("missing_exe", Outcome::NotRun),
                ("disabled_one", Outcome::NotRun),
                ("weird name+(1)[x].*?|$^ \\end <&>", Outcome::Passed),
                ("crash", Outcome::Failed),
            ]
        );
        assert_eq!(cases[5].duration_ms, 1500);
        // Fewer cases than it says, no testsuite, half a file, not XML: nothing is read.
        let short = WRITTEN.replace(r#"tests="7""#, r#"tests="8""#);
        assert!(read(short.as_bytes()).is_err());
        assert!(read(b"<?xml version=\"1.0\"?><nothing/>").is_err());
        assert!(read(&WRITTEN.as_bytes()[..400]).is_err());
        assert!(read(b"stale").is_err());
        // An empty run writes tests="0", which reads as no case.
        assert!(
            read(b"<testsuite tests=\"0\"></testsuite>")
                .unwrap()
                .is_empty()
        );
    }

    // covers: C-49
    #[test]
    fn cases_match_tests_by_name_and_duplicates_that_dont_add_up_are_unknown() {
        let case = |name: &str, outcome| Case {
            name: name.into(),
            outcome,
            duration_ms: 5,
        };
        let cases = vec![
            case("a", Outcome::Passed),
            case("dup", Outcome::Passed),
            case("dup", Outcome::Failed),
            case("half", Outcome::Failed),
        ];
        let got = matched(&["a", "dup", "dup", "gone", "half", "half"], &cases);
        assert_eq!(
            got.iter().map(|(o, _)| *o).collect::<Vec<_>>(),
            [
                Outcome::Passed,
                Outcome::Passed,
                Outcome::Failed,
                Outcome::NotRun,
                Outcome::Unknown { failed: true },
                Outcome::Unknown { failed: true },
            ]
        );
    }
}
