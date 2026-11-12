//! \[ADR 0085\] Surefire's and Failsafe's XML reports, read test by test (C-47).
//!
//! A report is read whole or not at all: one that doesn't parse to its end, with every element
//! closed, holds nothing. Its counts are never read (Surefire 3.0 to 3.5 writes `tests="0"` on a
//! suite whose nested classes failed); each `testcase` is, by its `classname`, never by the file's
//! name. XML is read as a stream with quick-xml, which reads no DTD and expands no entity; an
//! attribute's value is unescaped for the five predefined entities and character references only.

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// How one test case of a report ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// A `failure`, `error`, `rerunFailure`, `rerunError`, `flakyFailure` or `flakyError`: a test
    /// that failed once failed, even when a rerun passed.
    Failed,
    /// `skipped`: disabled, ignored or an assumption that didn't hold.
    Skipped,
}

/// One `testcase` of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// Its `classname`, the binary name of its class; empty when it has none.
    pub classname: String,
    /// Its `name`: a method, with a parametrized invocation's text after it; empty for a class's
    /// own failure (Surefire 3.5 and older).
    pub name: String,
    pub time_ms: u64,
    pub outcome: Outcome,
}

/// The most bytes of one report hushrun reads: past it, the report holds nothing.
pub const MOST_REPORT_BYTES: usize = 256 * 1024 * 1024;

/// The test cases of a JUnit XML report as Surefire writes it (`testsuite`, or `testsuites`
/// around them); `Err` when it isn't one, or doesn't read to its end.
pub fn cases(bytes: &[u8]) -> Result<Vec<Case>, String> {
    if bytes.len() > MOST_REPORT_BYTES {
        return Err("too large".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    let mut reader = Reader::from_str(text);
    let config = reader.config_mut();
    config.check_end_names = true;
    config.expand_empty_elements = false;
    let mut depth = 0usize;
    let mut root = None;
    let mut out = Vec::new();
    // The test case being read, and the depth of its element.
    let mut open: Option<(Case, usize)> = None;
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        match event {
            Event::Start(e) => {
                depth += 1;
                element(&e, depth, &mut root, &mut open, &mut out, false)?;
            }
            Event::Empty(e) => {
                element(&e, depth + 1, &mut root, &mut open, &mut out, true)?;
            }
            Event::End(e) => {
                if let Some((_, at)) = &open
                    && *at == depth
                    && e.local_name().as_ref() == "testcase"
                    && let Some((case, _)) = open.take()
                {
                    out.push(case);
                }
                depth = depth.checked_sub(1).ok_or("an element closed twice")?;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0 || open.is_some() {
        return Err("it ends before its elements do".into());
    }
    match root.as_deref() {
        Some("testsuite" | "testsuites") => Ok(out),
        Some(other) => Err(format!("its root element is {other}")),
        None => Err("it has no element".into()),
    }
}

fn element(
    e: &BytesStart<'_>,
    depth: usize,
    root: &mut Option<String>,
    open: &mut Option<(Case, usize)>,
    out: &mut Vec<Case>,
    empty: bool,
) -> Result<(), String> {
    let name = e.local_name().as_ref().to_string();
    if root.is_none() {
        *root = Some(name.clone());
    }
    match name.as_str() {
        "testcase" => {
            if open.is_some() {
                return Err("a testcase inside a testcase".into());
            }
            let mut case = Case {
                classname: String::new(),
                name: String::new(),
                time_ms: 0,
                outcome: Outcome::Passed,
            };
            for attr in e.attributes() {
                let attr = attr.map_err(|e| e.to_string())?;
                let value = attr
                    .normalized_value(XmlVersion::Implicit1_0)
                    .map_err(|e| e.to_string())?;
                match attr.key.local_name().as_ref() {
                    "classname" => case.classname = value.into_owned(),
                    "name" => case.name = value.into_owned(),
                    "time" => {
                        let seconds: f64 = value.trim().replace(',', "").parse().unwrap_or(0.0);
                        if seconds.is_finite() && seconds > 0.0 {
                            case.time_ms = (seconds * 1000.0).round() as u64;
                        }
                    }
                    _ => {}
                }
            }
            if empty {
                out.push(case);
            } else {
                *open = Some((case, depth));
            }
        }
        "failure" | "error" | "rerunFailure" | "rerunError" | "flakyFailure" | "flakyError" => {
            if let Some((case, _)) = open {
                case.outcome = Outcome::Failed;
            }
        }
        "skipped" => {
            if let Some((case, _)) = open
                && case.outcome != Outcome::Failed
            {
                case.outcome = Outcome::Skipped;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_gives_its_cases_by_classname_and_never_its_counts() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="pkg.C" tests="0" failures="0">
  <properties><property name="a" value="b"/></properties>
  <testcase name="passes" classname="pkg.C" time="0.012"/>
  <testcase name="fails" classname="pkg.C" time="0.001"><failure message="x">trace</failure><system-out>o</system-out></testcase>
  <testcase name="param(int)[1]" classname="pkg.C" time="0"/>
  <testcase name="innerFails" classname="pkg.C$Inner"><error/></testcase>
  <testcase name="disabled" classname="pkg.C"><skipped/></testcase>
  <testcase name="flaky" classname="pkg.C"><flakyFailure/></testcase>
  <testcase name="a &quot;b&quot;" classname="pkg.C"/>
</testsuite>"#;
        let cases = cases(xml).unwrap();
        let got: Vec<(&str, &str, Outcome)> = cases
            .iter()
            .map(|c| (c.classname.as_str(), c.name.as_str(), c.outcome))
            .collect();
        assert_eq!(
            got,
            [
                ("pkg.C", "passes", Outcome::Passed),
                ("pkg.C", "fails", Outcome::Failed),
                ("pkg.C", "param(int)[1]", Outcome::Passed),
                ("pkg.C$Inner", "innerFails", Outcome::Failed),
                ("pkg.C", "disabled", Outcome::Skipped),
                ("pkg.C", "flaky", Outcome::Failed),
                ("pkg.C", "a \"b\"", Outcome::Passed),
            ]
        );
        assert_eq!(cases[0].time_ms, 12);
    }

    #[test]
    fn a_report_cut_short_or_foreign_holds_nothing() {
        let whole = br#"<testsuite name="pkg.C"><testcase name="a" classname="pkg.C"/><testcase name="b" classname="pkg.C"><failure/></testcase></testsuite>"#;
        assert_eq!(cases(whole).unwrap().len(), 2);
        for cut in [40, 70, 100, whole.len() - 1] {
            assert!(cases(&whole[..cut]).is_err(), "cut at {cut}");
        }
        assert!(cases(b"<html><testcase name=\"a\"/></html>").is_err());
        assert!(cases(b"").is_err());
        assert!(cases(b"\xff\xfe").is_err());
    }
}
