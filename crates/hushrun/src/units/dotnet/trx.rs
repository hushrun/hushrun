//! \[ADR 0118\] What a VSTest TRX file (`--logger trx`) says each test came to, read so that nothing
//! absent, unreadable or ambiguous ever reads as a pass.
//!
//! - The file must parse, hold its `<ResultSummary>`, and as many top-level `<UnitTestResult>` as
//!   its `<Counters total>` says; otherwise nothing of it is read. A data row's result nested in
//!   `<InnerResults>` (MSTest's) isn't counted: its parent's outcome holds it.
//! - A result names its test by `testId`, which `<TestDefinitions>` maps to a `<TestMethod>`'s
//!   `className` and `name`: the test's id is `<className>.<name>`, its fully qualified name for
//!   xUnit, NUnit and MSTest alike (NUnit's `name` holds a test case's arguments).
//! - `Passed` passed; `Failed`, `Error`, `Timeout` and `Aborted` failed; `NotExecuted` was skipped,
//!   which passed; any other outcome (`Inconclusive`, `NotRunnable`, `Pending`, …) didn't finish.

use std::collections::{BTreeMap, BTreeSet};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// What one result came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    /// Its body didn't run (`NotExecuted`): a skip, which passes, as elsewhere in hushrun.
    Skipped,
    /// It didn't finish, or its outcome is one hushrun doesn't take for a pass or a fail.
    NotRun,
}

/// One top-level result: the test's id, what it came to, and how long it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Result {
    pub id: String,
    pub outcome: Outcome,
    pub duration_ms: u64,
}

/// What a TRX file says: each top-level result, in order, and the run's own outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trx {
    pub results: Vec<Result>,
    /// `<ResultSummary outcome>`: `Completed`, `Failed`, `Aborted`, `Error`, …
    pub summary: String,
    /// The assemblies its tests are in, as its `<TestMethod codeBase>` name them.
    pub code_bases: BTreeSet<String>,
}

/// The results of a TRX file, or why none can be read.
pub fn read(bytes: &[u8]) -> std::result::Result<Trx, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "it isn't UTF-8")?;
    // A BOM, which VSTest may write.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    // Each result's testId, outcome and duration, in order.
    let mut raw: Vec<(String, String, u64)> = Vec::new();
    let mut methods: BTreeMap<String, String> = BTreeMap::new();
    let mut code_bases: BTreeSet<String> = BTreeSet::new();
    let mut unit_test: Option<String> = None;
    let mut nested = 0usize;
    let mut summary: Option<String> = None;
    let mut total: Option<u64> = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("it doesn't parse: {err}"))?;
        match event {
            Event::Start(e) if e.local_name().as_ref() == "UnitTestResult" => {
                if nested == 0 {
                    raw.push(result_of(&e)?);
                }
                nested += 1;
            }
            Event::Empty(e) if e.local_name().as_ref() == "UnitTestResult" => {
                if nested == 0 {
                    raw.push(result_of(&e)?);
                }
            }
            Event::End(e) if e.local_name().as_ref() == "UnitTestResult" => {
                nested = nested.checked_sub(1).ok_or("a result closes twice")?;
            }
            Event::Start(e) if e.local_name().as_ref() == "UnitTest" => {
                unit_test = Some(attribute(&e, "id")?.ok_or("a test definition has no id")?);
            }
            Event::End(e) if e.local_name().as_ref() == "UnitTest" => {
                unit_test = None;
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "TestMethod" => {
                let Some(id) = &unit_test else {
                    return Err("a test method is outside a test definition".into());
                };
                let class = attribute(&e, "className")?.ok_or("a test method has no className")?;
                let name = attribute(&e, "name")?.ok_or("a test method has no name")?;
                if let Some(code_base) = attribute(&e, "codeBase")? {
                    code_bases.insert(code_base);
                }
                if methods
                    .insert(id.clone(), format!("{class}.{name}"))
                    .is_some()
                {
                    return Err(format!("test {id} is defined twice"));
                }
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "ResultSummary" => {
                summary = Some(attribute(&e, "outcome")?.unwrap_or_default());
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Counters" => {
                total = Some(
                    attribute(&e, "total")?
                        .and_then(|t| t.parse().ok())
                        .ok_or("its counters say no total")?,
                );
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let summary = summary.ok_or("it has no result summary")?;
    let total = total.ok_or("it has no counters")?;
    if total != raw.len() as u64 {
        return Err(format!("it says {total} results and holds {}", raw.len()));
    }
    let results = raw
        .into_iter()
        .map(|(test, outcome, duration_ms)| {
            let id = methods
                .get(&test)
                .cloned()
                .ok_or_else(|| format!("result {test} names no test definition"))?;
            Ok(Result {
                id,
                outcome: match outcome.as_str() {
                    "Passed" => Outcome::Passed,
                    "Failed" | "Error" | "Timeout" | "Aborted" => Outcome::Failed,
                    "NotExecuted" => Outcome::Skipped,
                    _ => Outcome::NotRun,
                },
                duration_ms,
            })
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    Ok(Trx {
        results,
        summary,
        code_bases,
    })
}

fn attribute(e: &BytesStart<'_>, name: &str) -> std::result::Result<Option<String>, String> {
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

fn result_of(e: &BytesStart<'_>) -> std::result::Result<(String, String, u64), String> {
    let test = attribute(e, "testId")?.ok_or("a result has no testId")?;
    let outcome = attribute(e, "outcome")?.unwrap_or_default();
    let duration = attribute(e, "duration")?
        .as_deref()
        .and_then(duration_ms)
        .unwrap_or(0);
    Ok((test, outcome, duration))
}

/// `hh:mm:ss.fffffff` in milliseconds.
fn duration_ms(text: &str) -> Option<u64> {
    let mut parts = text.split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let ms = ((hours * 60.0 + minutes) * 60.0 + seconds) * 1000.0;
    (ms.is_finite() && ms >= 0.0).then(|| ms.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trx(results: &str, definitions: &str, total: usize) -> String {
        format!(
            "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<TestRun xmlns=\"http://microsoft.com/schemas/VisualStudio/TeamTest/2010\"><Results>{results}</Results><TestDefinitions>{definitions}</TestDefinitions><ResultSummary outcome=\"Completed\"><Counters total=\"{total}\" executed=\"{total}\" /></ResultSummary></TestRun>"
        )
    }

    fn def(id: &str, class: &str, name: &str) -> String {
        format!(
            "<UnitTest name=\"{name}\" id=\"{id}\"><TestMethod codeBase=\"x.dll\" className=\"{class}\" name=\"{name}\" /></UnitTest>"
        )
    }

    // covers: C-98
    #[test]
    fn each_result_is_named_by_its_class_and_method_and_nothing_unreadable_passes() {
        let text = trx(
            "<UnitTestResult testId=\"a\" outcome=\"Passed\" duration=\"00:00:01.5000000\" />\
             <UnitTestResult testId=\"b\" outcome=\"Failed\" duration=\"00:00:00.0010000\"><Output /></UnitTestResult>\
             <UnitTestResult testId=\"c\" outcome=\"NotExecuted\" />\
             <UnitTestResult testId=\"d\" outcome=\"Inconclusive\" />\
             <UnitTestResult testId=\"e\" outcome=\"Passed\"><InnerResults><UnitTestResult testId=\"e\" outcome=\"Passed\" /></InnerResults></UnitTestResult>",
            &[
                def("a", "Calc.AddTests", "Adds"),
                def("b", "Calc.AddTests", "Zero(1)"),
                def("c", "Calc.AddTests", "Later"),
                def("d", "Calc.AddTests", "Maybe"),
                def("e", "Calc.RowTests", "Rows"),
            ]
            .concat(),
            5,
        );
        let read = read(text.as_bytes()).unwrap();
        assert_eq!(read.summary, "Completed");
        assert_eq!(read.code_bases, BTreeSet::from(["x.dll".to_string()]));
        let got: Vec<(&str, Outcome, u64)> = read
            .results
            .iter()
            .map(|r| (r.id.as_str(), r.outcome, r.duration_ms))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Calc.AddTests.Adds", Outcome::Passed, 1500),
                ("Calc.AddTests.Zero(1)", Outcome::Failed, 1),
                ("Calc.AddTests.Later", Outcome::Skipped, 0),
                ("Calc.AddTests.Maybe", Outcome::NotRun, 0),
                ("Calc.RowTests.Rows", Outcome::Passed, 0),
            ]
        );
        // A count that doesn't match, a result with no definition, a cut file: nothing read.
        let short = trx(
            "<UnitTestResult testId=\"a\" outcome=\"Passed\" />",
            &def("a", "C", "M"),
            2,
        );
        assert!(super::read(short.as_bytes()).is_err());
        let orphan = trx(
            "<UnitTestResult testId=\"z\" outcome=\"Passed\" />",
            &def("a", "C", "M"),
            1,
        );
        assert!(super::read(orphan.as_bytes()).is_err());
        let cut = &text[..text.len() / 2];
        assert!(super::read(cut.as_bytes()).is_err());
    }
}
