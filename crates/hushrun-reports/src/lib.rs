//! \[ADR 0048\] Summaries of the test and coverage reports a run wrote (C-27): counts, and each
//! file's path and format. Never a test's name, a message, or a file's contents.
//!
//! The crate does no I/O but reading: [`find`] walks the directories a run may have written to,
//! and [`summarize`] reads the files it found. Formats are told by their content, never by their
//! names (Appendix B of the plan): JUnit XML and TRX for test results; LCOV, Cobertura, JaCoCo,
//! Clover, OpenCover, Go's coverprofile, coverage.py's JSON, istanbul's summary and SimpleCov for
//! coverage. XML is read as a stream, with no DTD and no entity expansion.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

mod find;
mod json;
mod lines;
mod text;
mod xml;

pub use find::{Query, Search, find};
pub use lines::{count, percent};

/// A report file a run may have written: its path, and its path from the repository root,
/// `/`-separated, as the record names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub rel: String,
}

/// What a summary may cost. A file over `file_bytes` drops its kind's summary, as do more than
/// `total_bytes` of one kind's files and a summary that takes longer than `time`: a summary is
/// whole or absent, never a partial count. More than `files` files gives no summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub file_bytes: u64,
    pub total_bytes: u64,
    pub files: usize,
    pub time: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            file_bytes: 64 << 20,
            total_bytes: 256 << 20,
            files: 5_000,
            time: Duration::from_secs(3),
        }
    }
}

/// The most files a record names: the others are counted in `filesLeftOut`.
pub const MOST_FILES: usize = 16;
/// The longest path a record names, in bytes.
pub const LONGEST_PATH: usize = 160;
/// The most bytes a record's `reports` takes, serialized.
pub const MOST_BYTES: usize = 4096;
/// The highest count a record holds: a kind whose counts go past it has no summary.
pub const MOST_COUNT: u64 = 1_000_000_000;
/// The longest duration a record holds, in milliseconds, about three years.
pub const MOST_DURATION_MS: u64 = 100_000_000_000;

/// What a run's reports say (C-27), as a result records it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reports {
    /// The report files read, by path from the repository root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<File>,
    /// The report files read that `files` leaves out, past [`MOST_FILES`] or [`LONGEST_PATH`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub files_left_out: u64,
    /// `files` names every file the search found, each read, and the search went everywhere the
    /// job's readers read: so a reader that reads none of them doesn't read what the run wrote
    /// (C-26). False, and left out, otherwise.
    #[serde(default, skip_serializing_if = "is_false")]
    pub files_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<Tests>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Coverage>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// A report file. `kind` and `format` are words, so that a record a newer hushrun wrote reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    pub path: String,
    /// `tests` or `coverage`.
    pub kind: String,
    /// One of [`Format`]'s words, such as `junit` or `cobertura`.
    pub format: String,
}

/// The tests a run's test results count. `failed` and `errors` are apart, as JUnit keeps them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tests {
    pub total: u64,
    #[serde(default)]
    pub failed: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub skipped: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// What a run's coverage covers, from one format. Go's counts statements, the others lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statements: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branches: Option<Ratio>,
}

/// So many covered of so many.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ratio {
    pub covered: u64,
    pub total: u64,
}

impl Ratio {
    fn add(this: Option<Ratio>, that: Option<Ratio>) -> Option<Ratio> {
        match (this, that) {
            (Some(a), Some(b)) => Some(Ratio {
                covered: a.covered.saturating_add(b.covered),
                total: a.total.saturating_add(b.total),
            }),
            (a, b) => a.or(b),
        }
    }

    /// The ratio, when it counts something: a total of 0 says nothing.
    fn some(covered: u64, total: u64) -> Option<Ratio> {
        (total > 0).then_some(Ratio {
            covered: covered.min(total),
            total,
        })
    }
}

/// A format hushrun reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Format {
    Junit,
    Trx,
    Lcov,
    Cobertura,
    Jacoco,
    Clover,
    Opencover,
    Go,
    Coveragepy,
    Istanbul,
    Simplecov,
}

/// Coverage comes from the first of these formats a run wrote, so that coverage written twice
/// in two formats isn't counted twice.
const COVERAGE_ORDER: [Format; 9] = [
    Format::Lcov,
    Format::Cobertura,
    Format::Jacoco,
    Format::Clover,
    Format::Opencover,
    Format::Go,
    Format::Coveragepy,
    Format::Istanbul,
    Format::Simplecov,
];

impl Format {
    /// Its word in a record.
    pub fn word(self) -> &'static str {
        match self {
            Format::Junit => "junit",
            Format::Trx => "trx",
            Format::Lcov => "lcov",
            Format::Cobertura => "cobertura",
            Format::Jacoco => "jacoco",
            Format::Clover => "clover",
            Format::Opencover => "opencover",
            Format::Go => "go",
            Format::Coveragepy => "coveragepy",
            Format::Istanbul => "istanbul",
            Format::Simplecov => "simplecov",
        }
    }

    /// Whether it holds test results, not coverage.
    pub fn is_tests(self) -> bool {
        matches!(self, Format::Junit | Format::Trx)
    }

    fn kind(self) -> &'static str {
        if self.is_tests() { "tests" } else { "coverage" }
    }
}

/// What one file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Tests(Tests),
    Coverage(Counts),
}

/// One coverage file's counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub lines: Option<Ratio>,
    pub statements: Option<Ratio>,
    pub branches: Option<Ratio>,
}

impl Counts {
    fn add(self, other: Counts) -> Counts {
        Counts {
            lines: Ratio::add(self.lines, other.lines),
            statements: Ratio::add(self.statements, other.statements),
            branches: Ratio::add(self.branches, other.branches),
        }
    }

    fn is_empty(&self) -> bool {
        self.lines.is_none() && self.statements.is_none() && self.branches.is_none()
    }
}

/// Why a file says nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// It is in no format hushrun reads.
    NotAReport,
    /// It is in one, but broken.
    Malformed(String),
    /// The summary ran out of time.
    TimedOut,
}

/// When a summary must be done.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deadline(Option<Instant>);

impl Deadline {
    pub(crate) fn none() -> Deadline {
        Deadline(None)
    }

    pub(crate) fn check(self) -> Result<(), Error> {
        match self.0 {
            Some(at) if Instant::now() > at => Err(Error::TimedOut),
            _ => Ok(()),
        }
    }
}

/// What `bytes`, one file, says, in whichever format it is: the entry the fuzz target uses.
pub fn read(bytes: &[u8]) -> Result<(Format, Read), Error> {
    read_by(bytes, Deadline::none())
}

fn read_by(bytes: &[u8], deadline: Deadline) -> Result<(Format, Read), Error> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace());
    match start.map(|at| bytes[at]) {
        Some(b'<') => xml::read(text_of(bytes)?, deadline),
        Some(b'{') => json::read(bytes, deadline),
        Some(_) => text::read(text_of(bytes)?, deadline),
        None => Err(Error::NotAReport),
    }
}

fn text_of(bytes: &[u8]) -> Result<&str, Error> {
    std::str::from_utf8(bytes).map_err(|e| Error::Malformed(format!("not UTF-8: {e}")))
}

/// The format `prefix`, a file's first bytes, is in, without reading the rest: for a file too
/// big to read, whose kind's summary it drops.
pub fn sniff(prefix: &[u8]) -> Option<Format> {
    let prefix = prefix.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(prefix);
    let start = prefix.iter().position(|b| !b.is_ascii_whitespace())?;
    // A prefix may end inside a character: what is before it is enough.
    let text = match std::str::from_utf8(&prefix[start..]) {
        Ok(text) => text,
        Err(e) => std::str::from_utf8(&prefix[start..start + e.valid_up_to()]).ok()?,
    };
    match text.as_bytes().first() {
        Some(b'<') => xml::sniff(text),
        Some(b'{') => json::sniff(text),
        _ => text::sniff(text),
    }
}

/// How many bytes [`sniff`] reads.
const SNIFF_BYTES: usize = 64 << 10;

/// The summary of `files`, the reports a run wrote, within `limits`; `None` when they hold no
/// test results and no coverage. Test results are summed over every file; coverage comes from
/// the first format in `COVERAGE_ORDER` that a file holds, summed over that format's files.
/// A file that is broken, or in no format hushrun reads, says nothing.
pub fn summarize(files: &[Found], limits: &Limits) -> Option<Reports> {
    let (reports, _) = summary(files, limits)?;
    (reports.tests.is_some() || reports.coverage.is_some()).then_some(reports)
}

/// \[ADR 0048\] What a run's result keeps of the reports `search` found (C-27): their summary,
/// naming every file read, and `filesComplete` when it names every file found and the search
/// went everywhere the job's readers read. With nothing found, that is all it says. `None` when it
/// has nothing to say: past `limits.files`, or with nothing read and the list not complete.
pub fn record(search: &Search, limits: &Limits) -> Option<Reports> {
    let (mut reports, every_file_read) = summary(&search.files, limits)?;
    reports.files_complete = every_file_read && reports.files_left_out == 0 && !search.cut_short;
    fit(&mut reports);
    let says = reports.tests.is_some()
        || reports.coverage.is_some()
        || !reports.files.is_empty()
        || reports.files_left_out > 0
        || reports.files_complete;
    says.then_some(reports)
}

/// The summary of `files` within `limits`, naming every file read, and whether every file was
/// read; `None` past `limits.files`.
fn summary(files: &[Found], limits: &Limits) -> Option<(Reports, bool)> {
    if files.len() > limits.files {
        tracing::debug!(
            "{} report files, more than {}: no summary",
            files.len(),
            limits.files
        );
        return None;
    }
    let started = Instant::now();
    let deadline = Deadline(started.checked_add(limits.time));
    // A file found twice is read once.
    let mut seen = std::collections::BTreeSet::new();
    let files: Vec<&Found> = files.iter().filter(|f| seen.insert(&f.path)).collect();
    let found = files.len();
    // Each file's format first, from its first bytes, so that each kind is read whole or not
    // at all.
    let mut sniffed: Vec<(&Found, u64, Format)> = Vec::new();
    let mut too_big: Vec<bool> = vec![false; 2];
    for found in files {
        // A regular file, never a link, and read only by the path it was found by.
        let Ok(meta) = std::fs::symlink_metadata(&found.path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let Some(format) = sniff_file(found) else {
            continue;
        };
        if meta.len() > limits.file_bytes {
            tracing::debug!(
                "{} is over {} bytes: no summary of its kind",
                found.rel,
                limits.file_bytes
            );
            too_big[usize::from(!format.is_tests())] = true;
            continue;
        }
        sniffed.push((found, meta.len(), format));
    }
    let mut read: Vec<(&Found, Format, Read)> = Vec::new();
    for (tests, kind) in [(true, 0), (false, 1)] {
        if too_big[kind] {
            continue;
        }
        let ours: Vec<&(&Found, u64, Format)> = sniffed
            .iter()
            .filter(|(.., format)| format.is_tests() == tests)
            .collect();
        let bytes: u64 = ours.iter().map(|(_, size, _)| size).sum();
        if bytes > limits.total_bytes {
            tracing::debug!(
                "{bytes} bytes of reports, more than {}: no summary of their kind",
                limits.total_bytes
            );
            continue;
        }
        let mut these = Vec::new();
        let mut whole = true;
        for (found, ..) in ours {
            if deadline.check().is_err() {
                whole = false;
                break;
            }
            let Ok(bytes) = read_at_most(found, limits.file_bytes) else {
                continue;
            };
            match read_by(&bytes, deadline) {
                Ok((format, said)) => these.push((*found, format, said)),
                Err(Error::TimedOut) => {
                    tracing::debug!(
                        "reading reports took more than {:?}: no summary of their kind",
                        limits.time
                    );
                    whole = false;
                    break;
                }
                Err(err) => tracing::debug!("{} says nothing: {err:?}", found.rel),
            }
        }
        if !whole {
            // Out of time: nothing later is read either.
            break;
        }
        read.extend(these);
    }
    let every_file_read = read.len() == found;
    Some((reports_of(&read), every_file_read))
}

/// The first bytes of the file `found`, for [`sniff`].
fn sniff_file(found: &Found) -> Option<Format> {
    use std::io::Read as _;
    let mut prefix = Vec::with_capacity(SNIFF_BYTES);
    std::fs::File::open(&found.path)
        .ok()?
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut prefix)
        .ok()?;
    sniff(&prefix)
}

/// The file `found`, unless it grew past `most` bytes since it was measured.
fn read_at_most(found: &Found, most: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(&found.path)?
        .take(most.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > most {
        return Err(std::io::Error::other("it grew past the limit"));
    }
    Ok(bytes)
}

/// The summary of what each file said, naming every file read.
fn reports_of(read: &[(&Found, Format, Read)]) -> Reports {
    let mut tests: Option<Tests> = None;
    let mut durations = true;
    for (_, _, said) in read {
        if let Read::Tests(these) = said {
            durations &= these.duration_ms.is_some();
            let sum = tests.get_or_insert_with(Tests::default);
            sum.total = sum.total.saturating_add(these.total);
            sum.failed = sum.failed.saturating_add(these.failed);
            sum.errors = sum.errors.saturating_add(these.errors);
            sum.skipped = sum.skipped.saturating_add(these.skipped);
            sum.duration_ms = Some(
                sum.duration_ms
                    .unwrap_or(0)
                    .saturating_add(these.duration_ms.unwrap_or(0)),
            );
        }
    }
    if let Some(tests) = tests.as_mut()
        && (!durations || tests.duration_ms.is_some_and(|ms| ms > MOST_DURATION_MS))
    {
        // A file that didn't say how long its tests took leaves the sum unknown.
        tests.duration_ms = None;
    }
    // Counts past what a record holds are no summary, never a partial one.
    let tests = tests.filter(|t| {
        [t.total, t.failed, t.errors, t.skipped]
            .iter()
            .all(|n| *n <= MOST_COUNT)
    });
    let coverage = COVERAGE_ORDER.iter().find_map(|wanted| {
        let counts = read
            .iter()
            .filter(|(_, format, _)| format == wanted)
            .filter_map(|(.., said)| match said {
                Read::Coverage(counts) => Some(*counts),
                Read::Tests(_) => None,
            })
            .fold(None::<Counts>, |sum, counts| {
                Some(sum.map_or(counts, |sum| sum.add(counts)))
            })?;
        (!counts.is_empty()).then_some(Coverage {
            format: wanted.word().to_string(),
            lines: counts.lines,
            statements: counts.statements,
            branches: counts.branches,
        })
    });
    let coverage = coverage.filter(|c| {
        [c.lines, c.statements, c.branches]
            .iter()
            .flatten()
            .all(|r| r.total <= MOST_COUNT)
    });
    let mut named: Vec<File> = read
        .iter()
        .map(|(found, format, _)| File {
            path: found.rel.clone(),
            kind: format.kind().to_string(),
            format: format.word().to_string(),
        })
        .collect();
    named.sort_by(|a, b| a.path.cmp(&b.path));
    named.dedup_by(|a, b| a.path == b.path);
    let mut reports = Reports {
        files: Vec::new(),
        files_left_out: 0,
        files_complete: false,
        tests,
        coverage,
    };
    for file in named {
        if reports.files.len() < MOST_FILES && file.path.len() <= LONGEST_PATH {
            reports.files.push(file);
        } else {
            reports.files_left_out += 1;
        }
    }
    fit(&mut reports);
    reports
}

/// `reports` within [`MOST_BYTES`]: escapes can make a path longer in JSON than in bytes, so
/// files go, last first, into `filesLeftOut`, and the list is no longer complete.
fn fit(reports: &mut Reports) {
    while serde_json::to_vec(&*reports).map_or(usize::MAX, |json| json.len()) > MOST_BYTES {
        if reports.files.pop().is_none() {
            break;
        }
        reports.files_left_out += 1;
        reports.files_complete = false;
    }
}

#[cfg(test)]
mod tests;
