//! The XML formats: JUnit and TRX for test results, Cobertura, JaCoCo, Clover and OpenCover for
//! coverage. Read as a stream with quick-xml, which reads no DTD and expands no entity: a
//! DOCTYPE is skipped whole, and no text or attribute is ever unescaped, since only numbers and
//! names are read.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::{Counts, Deadline, Error, Format, Ratio, Read, Tests};

/// How many events go by between looks at the clock.
const EVENTS_PER_LOOK: u32 = 4096;

/// A document being read.
struct Walk<'a> {
    reader: Reader<&'a [u8]>,
    deadline: Deadline,
    events: u32,
}

impl<'a> Walk<'a> {
    fn new(text: &'a str, deadline: Deadline) -> Walk<'a> {
        let mut reader = Reader::from_str(text);
        let config = reader.config_mut();
        config.trim_text_start = true;
        config.trim_text_end = true;
        config.check_end_names = true;
        config.expand_empty_elements = false;
        Walk {
            reader,
            deadline,
            events: 0,
        }
    }

    fn next(&mut self) -> Result<Event<'a>, Error> {
        self.events = self.events.wrapping_add(1);
        if self.events.is_multiple_of(EVENTS_PER_LOOK) {
            self.deadline.check()?;
        }
        self.reader
            .read_event()
            .map_err(|e| Error::Malformed(e.to_string()))
    }

    /// The root element, and whether it is empty (`<root/>`).
    fn root(&mut self) -> Result<(BytesStart<'a>, bool), Error> {
        loop {
            match self.next()? {
                Event::Start(e) => return Ok((e, false)),
                Event::Empty(e) => return Ok((e, true)),
                Event::Eof => return Err(Error::NotAReport),
                // The declaration, comments, processing instructions, the DOCTYPE.
                _ => {}
            }
        }
    }

    /// Every tag below the root, with its depth (the root's children are at 2), until the root
    /// closes.
    fn each(&mut self, mut on: impl FnMut(Tag<'_, 'a>) -> Result<(), Error>) -> Result<(), Error> {
        let mut depth = 1u32;
        loop {
            match self.next()? {
                Event::Start(e) => {
                    depth += 1;
                    on(Tag::Open(&e, depth, true))?;
                }
                Event::Empty(e) => on(Tag::Open(&e, depth + 1, false))?,
                Event::End(_) => {
                    on(Tag::Close(depth))?;
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Event::Eof => {
                    return Err(Error::Malformed("it ends before its root closes".into()));
                }
                _ => {}
            }
        }
    }
}

/// A tag below the root: an element that opens, at its depth, and whether it is a start tag
/// (`<a>`, not `<a/>`); or the depth of an element that closes.
enum Tag<'e, 'a> {
    Open(&'e BytesStart<'a>, u32, bool),
    Close(u32),
}

/// What a document in one of these formats says.
pub(crate) fn read(text: &str, deadline: Deadline) -> Result<(Format, Read), Error> {
    let mut walk = Walk::new(text, deadline);
    let (root, empty) = walk.root()?;
    let format = format_of(&root).ok_or(Error::NotAReport)?;
    let read = match format {
        Format::Junit => Read::Tests(junit(&mut walk, &root, empty)?),
        Format::Trx => Read::Tests(trx(&mut walk, empty)?),
        Format::Cobertura => Read::Coverage(cobertura(&mut walk, &root, empty)?),
        Format::Jacoco => Read::Coverage(jacoco(&mut walk, empty)?),
        Format::Clover => Read::Coverage(clover(&mut walk, empty)?),
        Format::Opencover => Read::Coverage(opencover(&mut walk, empty)?),
        _ => return Err(Error::NotAReport),
    };
    Ok((format, read))
}

/// The format a document's first bytes are in: its root element says.
pub(crate) fn sniff(text: &str) -> Option<Format> {
    let mut walk = Walk::new(text, Deadline::none());
    let (root, _) = walk.root().ok()?;
    format_of(&root)
}

fn format_of(root: &BytesStart<'_>) -> Option<Format> {
    match root.local_name().as_ref() {
        "testsuites" | "testsuite" => Some(Format::Junit),
        "TestRun" => Some(Format::Trx),
        // Cobertura's root says its line rate; Clover's holds a project.
        "coverage" if attr(root, "line-rate").is_some() => Some(Format::Cobertura),
        "coverage" => Some(Format::Clover),
        "report" => Some(Format::Jacoco),
        "CoverageSession" => Some(Format::Opencover),
        _ => None,
    }
}

/// The value of `element`'s attribute `name`, as written: never unescaped.
fn attr<'e>(element: &'e BytesStart<'_>, name: &str) -> Option<std::borrow::Cow<'e, str>> {
    element
        .attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .map(|a| a.value)
}

/// A count in an attribute: a whole number, or `None`.
fn number(element: &BytesStart<'_>, name: &str) -> Option<u64> {
    attr(element, name)?.trim().parse().ok()
}

/// A count an attribute must hold.
fn needed(element: &BytesStart<'_>, name: &str) -> Result<u64, Error> {
    number(element, name).ok_or_else(|| {
        Error::Malformed(format!(
            "{} has no count in {name}",
            element.local_name().as_ref()
        ))
    })
}

/// A duration in seconds, as JUnit writes it, in milliseconds. Some writers group thousands
/// with commas, and some use a comma for the decimal point.
fn seconds(element: &BytesStart<'_>, name: &str) -> Option<u64> {
    let value = attr(element, name)?;
    let value = value.trim();
    let value = if value.contains('.') {
        value.replace(',', "")
    } else {
        value.replace(',', ".")
    };
    let seconds: f64 = value.parse().ok()?;
    let ms = (seconds * 1000.0).round();
    // At most 10^11 ms, about three years.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (ms.is_finite() && (0.0..=1e11).contains(&ms)).then_some(ms as u64)
}

/// JUnit: every `<testcase>`, at any depth, counts once, by its children `<failure>`,
/// `<error>` and `<skipped>`; the summary attributes are often wrong, so they are never read.
/// Surefire's reruns (`<flakyFailure>`, `<rerunFailure>`, …) aren't outcomes. The time is the
/// root's, else the sum of the suites just below it.
fn junit(walk: &mut Walk<'_>, root: &BytesStart<'_>, empty: bool) -> Result<Tests, Error> {
    #[derive(Clone, Copy, Default)]
    struct Case {
        depth: u32,
        failure: bool,
        error: bool,
        skipped: bool,
    }
    fn count(tests: &mut Tests, case: Case) {
        tests.total += 1;
        if case.failure {
            tests.failed += 1;
        } else if case.error {
            tests.errors += 1;
        } else if case.skipped {
            tests.skipped += 1;
        }
    }
    let mut tests = Tests::default();
    let mut suites: Option<u64> = None;
    let mut suite_without_time = false;
    let mut case: Option<Case> = None;
    if !empty {
        walk.each(|tag| {
            match tag {
                Tag::Open(e, depth, start) => match e.local_name().as_ref() {
                    "testsuite" if depth == 2 => match seconds(e, "time") {
                        Some(ms) => suites = Some(suites.unwrap_or(0).saturating_add(ms)),
                        None => suite_without_time = true,
                    },
                    "testcase" if case.is_none() => {
                        let this = Case {
                            depth,
                            ..Case::default()
                        };
                        if start {
                            case = Some(this);
                        } else {
                            count(&mut tests, this);
                        }
                    }
                    outcome @ ("failure" | "error" | "skipped") => {
                        if let Some(open) = case.as_mut().filter(|c| c.depth + 1 == depth) {
                            match outcome {
                                "failure" => open.failure = true,
                                "error" => open.error = true,
                                _ => open.skipped = true,
                            }
                        }
                    }
                    _ => {}
                },
                Tag::Close(depth) => {
                    if let Some(open) = case.filter(|c| c.depth == depth) {
                        count(&mut tests, open);
                        case = None;
                    }
                }
            }
            Ok(())
        })?;
    }
    tests.duration_ms = seconds(root, "time").or(if suite_without_time { None } else { suites });
    Ok(tests)
}

/// TRX: `ResultSummary/Counters`. Timeouts and aborted tests count as failed, tests that didn't
/// run as skipped. The time is `Times`' start to finish.
fn trx(walk: &mut Walk<'_>, empty: bool) -> Result<Tests, Error> {
    if empty {
        return Err(Error::Malformed(
            "a TRX file without a ResultSummary".into(),
        ));
    }
    let mut tests: Option<Tests> = None;
    let mut times: Option<(String, String)> = None;
    let mut in_summary = false;
    walk.each(|tag| {
        match tag {
            Tag::Open(e, depth, start) => match (e.local_name().as_ref(), depth) {
                ("Times", 2) => {
                    if let (Some(from), Some(to)) = (attr(e, "start"), attr(e, "finish")) {
                        times = Some((from.into_owned(), to.into_owned()));
                    }
                }
                ("ResultSummary", 2) => in_summary = start,
                ("Counters", 3) if in_summary => {
                    let some = |name| number(e, name).unwrap_or(0);
                    tests = Some(Tests {
                        total: needed(e, "total")?,
                        failed: some("failed")
                            .saturating_add(some("timeout"))
                            .saturating_add(some("aborted")),
                        errors: some("error"),
                        skipped: some("notExecuted"),
                        duration_ms: None,
                    });
                }
                _ => {}
            },
            Tag::Close(2) => in_summary = false,
            Tag::Close(_) => {}
        }
        Ok(())
    })?;
    let mut tests = tests.ok_or_else(|| Error::Malformed("a TRX file without Counters".into()))?;
    tests.duration_ms = times.and_then(|(from, to)| {
        let from: jiff::Timestamp = from.trim().parse().ok()?;
        let to: jiff::Timestamp = to.trim().parse().ok()?;
        let ms = to.as_millisecond().checked_sub(from.as_millisecond())?;
        u64::try_from(ms).ok()
    });
    Ok(tests)
}

/// Cobertura: the root's `lines-covered`/`lines-valid` and `branches-covered`/`branches-valid`;
/// else each `<line>`, once per file and number, since a class lists its lines again under its
/// methods. A line with `hits` above 0 is covered, and a branch line's `condition-coverage`,
/// `50% (1/2)`, gives its branches.
fn cobertura(walk: &mut Walk<'_>, root: &BytesStart<'_>, empty: bool) -> Result<Counts, Error> {
    // Each line's hits and branches, by file and number.
    let mut seen: HashMap<(u32, u64), (u64, u64, u64)> = HashMap::new();
    let mut files: HashMap<String, u32> = HashMap::new();
    let mut file: Option<u32> = None;
    if !empty {
        walk.each(|tag| {
            let Tag::Open(e, ..) = tag else {
                return Ok(());
            };
            match e.local_name().as_ref() {
                "class" => {
                    let name = attr(e, "filename")
                        .map(|n| n.into_owned())
                        .unwrap_or_default();
                    let next = u32::try_from(files.len()).unwrap_or(u32::MAX);
                    file = Some(*files.entry(name).or_insert(next));
                }
                "line" => {
                    let (Some(file), Some(at)) = (file, number(e, "number")) else {
                        return Ok(());
                    };
                    let hits = number(e, "hits").unwrap_or(0);
                    let (covered, total) = attr(e, "condition-coverage")
                        .and_then(|c| branches_of(&c))
                        .unwrap_or((0, 0));
                    let entry = seen.entry((file, at)).or_insert((0, 0, 0));
                    entry.0 = entry.0.max(hits);
                    entry.1 = entry.1.max(covered);
                    entry.2 = entry.2.max(total);
                }
                _ => {}
            }
            Ok(())
        })?;
    }
    let attrs = |covered: &str, valid: &str| match (number(root, covered), number(root, valid)) {
        (Some(covered), Some(valid)) if valid > 0 => Some((covered, valid)),
        _ => None,
    };
    let lines = attrs("lines-covered", "lines-valid").unwrap_or_else(|| {
        let total = seen.len() as u64;
        let covered = seen.values().filter(|(hits, ..)| *hits > 0).count() as u64;
        (covered, total)
    });
    let branches = attrs("branches-covered", "branches-valid").unwrap_or_else(|| {
        let total: u64 = seen.values().map(|(_, _, total)| total).sum();
        let covered: u64 = seen.values().map(|(_, covered, _)| covered).sum();
        (covered, total)
    });
    Ok(Counts {
        lines: Ratio::some(lines.0, lines.1),
        statements: None,
        branches: Ratio::some(branches.0, branches.1),
    })
}

/// `50% (1/2)`: 1 of 2 branches covered.
fn branches_of(text: &str) -> Option<(u64, u64)> {
    let inside = text.split_once('(')?.1.split_once(')')?.0;
    let (covered, total) = inside.split_once('/')?;
    Some((covered.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// JaCoCo: the report's own `<counter>`s, its direct children; counters below repeat parts of
/// the totals.
fn jacoco(walk: &mut Walk<'_>, empty: bool) -> Result<Counts, Error> {
    let mut counts = Counts::default();
    let mut found = false;
    if !empty {
        walk.each(|tag| {
            let Tag::Open(e, 2, _) = tag else {
                return Ok(());
            };
            if e.local_name().as_ref() != "counter" {
                return Ok(());
            }
            let ratio = || -> Result<Option<Ratio>, Error> {
                let missed = needed(e, "missed")?;
                let covered = needed(e, "covered")?;
                Ok(Ratio::some(covered, covered.saturating_add(missed)))
            };
            match attr(e, "type").as_deref() {
                Some("LINE") => {
                    counts.lines = ratio()?;
                    found = true;
                }
                Some("BRANCH") => counts.branches = ratio()?,
                _ => {}
            }
            Ok(())
        })?;
    }
    if !found {
        return Err(Error::NotAReport);
    }
    Ok(counts)
}

/// Clover: the project's own `<metrics>`: statements, and conditionals for branches.
fn clover(walk: &mut Walk<'_>, empty: bool) -> Result<Counts, Error> {
    let mut counts: Option<Counts> = None;
    let mut in_project = false;
    if !empty {
        walk.each(|tag| {
            match tag {
                Tag::Open(e, depth, start) => match (e.local_name().as_ref(), depth) {
                    ("project", 2) => in_project = start,
                    ("metrics", 3) if in_project => {
                        let statements = needed(e, "statements")?;
                        let covered = needed(e, "coveredstatements")?;
                        let branches =
                            match (number(e, "conditionals"), number(e, "coveredconditionals")) {
                                (Some(total), Some(covered)) => Ratio::some(covered, total),
                                _ => None,
                            };
                        counts = Some(Counts {
                            lines: Ratio::some(covered, statements),
                            statements: None,
                            branches,
                        });
                    }
                    _ => {}
                },
                Tag::Close(2) => in_project = false,
                Tag::Close(_) => {}
            }
            Ok(())
        })?;
    }
    counts.ok_or(Error::NotAReport)
}

/// OpenCover: the session's own `<Summary>`: sequence points for lines, branch points for
/// branches.
fn opencover(walk: &mut Walk<'_>, empty: bool) -> Result<Counts, Error> {
    let mut counts: Option<Counts> = None;
    if !empty {
        walk.each(|tag| {
            let Tag::Open(e, 2, _) = tag else {
                return Ok(());
            };
            if e.local_name().as_ref() != "Summary" {
                return Ok(());
            }
            let points = needed(e, "numSequencePoints")?;
            let visited = needed(e, "visitedSequencePoints")?;
            let branches = match (
                number(e, "numBranchPoints"),
                number(e, "visitedBranchPoints"),
            ) {
                (Some(total), Some(covered)) => Ratio::some(covered, total),
                _ => None,
            };
            counts = Some(Counts {
                lines: Ratio::some(visited, points),
                statements: None,
                branches,
            });
            Ok(())
        })?;
    }
    counts.ok_or(Error::NotAReport)
}
