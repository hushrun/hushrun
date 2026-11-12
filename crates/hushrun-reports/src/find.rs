//! Which files a run wrote reports to (C-27): the files its command line names, those
//! hushrun.yaml lists for the check, those its CI job's readers read, and the places tools write
//! to by default; each a regular file inside the repository, modified since the run started.
//!
//! Every source becomes a pattern from the repository's root, and one walk from the root finds
//! them all, entering only directories some pattern can reach. The walk never follows a link,
//! never enters `.git`, `node_modules`, `.venv`, `venv` or `__pycache__`, enters no hidden
//! directory through `**`, and gives up past 200,000 entries or a second: then nothing is found.
//! It says when it fell short of where a reader's pattern goes: a directory deeper than `**`
//! goes, one it couldn't read, a link, or one of those it never enters that the pattern names.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use hushrun_core::glob::Pattern;

use crate::Found;

/// What a search needs to know of the run.
#[derive(Debug, Clone, Copy)]
pub struct Query<'a> {
    /// The repository's top directory.
    pub root: &'a Path,
    /// The directory the command ran in.
    pub cwd: &'a Path,
    /// When the command started.
    pub started: SystemTime,
    /// The command, as it ran.
    pub command: &'a [String],
    /// hushrun.yaml's `checks.<name>.reports.files`: globs from `cwd`.
    pub declared: &'a [String],
    /// The paths the CI job's readers read, from the root.
    pub readers: &'a [String],
}

/// The most directory entries a search reads.
const MOST_ENTRIES: usize = 200_000;
/// The longest a search takes.
const MOST_TIME: Duration = Duration::from_secs(1);
/// How many levels `**` goes down.
const DEEPEST: usize = 6;
/// What a file's time may be off by: a report is modified at or after the start, less this.
const CLOCK: Duration = Duration::from_secs(2);
/// Directories never entered.
const SKIPPED: &[&str] = &[".git", "node_modules", ".venv", "venv", "__pycache__"];

/// Where tools write test results and coverage when told nothing, from the directory the command
/// ran in and from the repository's root.
const DEFAULT_PLACES: &[&str] = &[
    "junit.xml",
    "junit-*.xml",
    "*junit*.xml",
    "test-results.xml",
    "rspec.xml",
    "report.xml",
    "test-results/**/*.xml",
    "test-reports/**/*.xml",
    "build/test-results/**/*.xml",
    "target/surefire-reports/*.xml",
    "target/failsafe-reports/*.xml",
    "target/nextest/*/junit.xml",
    "TestResults/**/*.trx",
    "**/TEST-*.xml",
    "coverage.xml",
    "cobertura.xml",
    "**/cobertura-coverage.xml",
    "lcov.info",
    "coverage/lcov.info",
    "coverage.lcov",
    "coverage/coverage-summary.json",
    "coverage.json",
    "coverage/clover.xml",
    "clover.xml",
    "coverage.out",
    "cover.out",
    "coverage.txt",
    "target/site/jacoco/jacoco.xml",
    "build/reports/jacoco/**/*.xml",
    "build/reports/kover/**/*.xml",
    "coverage/.resultset.json",
    "TestResults/**/coverage.cobertura.xml",
    "TestResults/**/coverage.opencover.xml",
];

/// What a search found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    /// The report files the run may have written, each once, by path from the root.
    pub files: Vec<Found>,
    /// It left unread where a pattern of the job's readers goes on: a directory deeper than `**`
    /// goes or one it couldn't read, or a link, which it never follows. A file the run wrote
    /// there, which a reader reads, isn't in `files`.
    pub cut_short: bool,
}

/// The report files the run `q` describes may have written; `None` when the search went past
/// its bounds, so that a summary is whole or absent.
pub fn find(q: &Query<'_>) -> Option<Search> {
    let cwd = rel_of(q.root, q.cwd).unwrap_or_default();
    let mut sources: Vec<String> = Vec::new();
    let from_cwd = |path: &str| join(&cwd, path);
    for path in command_paths(q.command, |name| nextest_junit(q, name)) {
        sources.extend(from_cwd(&path));
    }
    for path in q.declared {
        sources.extend(from_cwd(path));
    }
    let mut read_by_readers: Vec<String> = Vec::new();
    for path in q.readers {
        let path = plain(path.trim());
        if path.is_empty() || path.starts_with('!') {
            continue;
        }
        let path = path.trim_start_matches('/');
        read_by_readers.extend(join("", path));
        // A reader's path may be a directory: what is below it.
        read_by_readers.extend(join("", &format!("{}/**/*", path.trim_end_matches('/'))));
    }
    sources.extend(read_by_readers.iter().cloned());
    for place in DEFAULT_PLACES {
        sources.extend(from_cwd(place));
        if !cwd.is_empty() {
            sources.extend(join("", place));
        }
    }
    sources.sort();
    sources.dedup();
    let globs: Vec<Glob> = sources.iter().filter_map(|s| Glob::new(s)).collect();
    let readers: Vec<Glob> = read_by_readers
        .iter()
        .filter_map(|s| Glob::new(s))
        .collect();
    let mut budget = Budget {
        entries: 0,
        until: Instant::now() + MOST_TIME,
    };
    let mut found: Vec<Found> = Vec::new();
    let cut_short = walk(q.root, &globs, &readers, &mut budget, &mut found)?;
    let since = q
        .started
        .checked_sub(CLOCK)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    found.retain(|f| {
        std::fs::symlink_metadata(&f.path).is_ok_and(|meta| {
            meta.file_type().is_file() && meta.modified().is_ok_and(|at| at >= since)
        })
    });
    found.sort_by(|a, b| a.rel.cmp(&b.rel));
    found.dedup_by(|a, b| a.rel == b.rel);
    drop_testng_copies(&mut found);
    Some(Search {
        files: found,
        cut_short,
    })
}

/// Surefire's TestNG provider writes each test once in its own report, `TEST-TestSuite.xml`, and
/// TestNG's reporter writes them again, a class a file, below `junitreports/` beside it. A report
/// there is dropped only when it is such a copy: every case it holds, by class and name, is in a
/// `TEST-TestSuite.xml` found beside its directory. A report of another tool there, or one that
/// can't be read or holds a case of its own, stays: no test counts twice, and none is lost.
fn drop_testng_copies(found: &mut Vec<Found>) {
    /// The most bytes of a report this reads to tell a copy: past it, the report stays.
    const MOST_BYTES: u64 = 64 * 1024 * 1024;
    fn cases(path: &Path) -> Option<std::collections::BTreeSet<(String, String)>> {
        if std::fs::metadata(path).ok()?.len() > MOST_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        crate::xml::junit_cases(std::str::from_utf8(&bytes).ok()?)
    }
    let suites: std::collections::BTreeMap<String, PathBuf> = found
        .iter()
        .filter_map(|f| {
            let (dir, name) = f.rel.rsplit_once('/').unwrap_or(("", f.rel.as_str()));
            (name == "TEST-TestSuite.xml").then(|| (dir.to_string(), f.path.clone()))
        })
        .collect();
    let mut read: std::collections::BTreeMap<
        String,
        Option<std::collections::BTreeSet<(String, String)>>,
    > = std::collections::BTreeMap::new();
    found.retain(|f| {
        let Some((dir, _)) = f.rel.rsplit_once('/') else {
            return true;
        };
        let parent = match dir.rsplit_once('/') {
            Some((parent, "junitreports")) => parent,
            None if dir == "junitreports" => "",
            _ => return true,
        };
        let Some(suite) = suites.get(parent) else {
            return true;
        };
        let held = read
            .entry(parent.to_string())
            .or_insert_with(|| cases(suite));
        match (held, cases(&f.path)) {
            (Some(held), Some(copy)) => copy.is_empty() || !copy.is_subset(held),
            _ => true,
        }
    });
}

/// A path as CI writes it, with what CI fills in made a wildcard: `test_results_${{ matrix.crate
/// }}.json` is `test_results_*.json`. One that starts with an expression and a `/`, such as
/// `${{ github.workspace }}/coverage.xml`, is from the root.
fn plain(path: &str) -> String {
    let mut plain = String::new();
    let mut rest = path;
    while let Some(start) = rest.find("${{") {
        plain.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("}}") else {
            return String::new();
        };
        rest = &rest[start + end + 2..];
        if plain.is_empty() && rest.starts_with('/') {
            continue;
        }
        if !plain.ends_with('*') {
            plain.push('*');
        }
    }
    plain.push_str(rest);
    plain
}

/// `path` from `base`, `/`-separated, or `None` when it isn't below it.
fn rel_of(base: &Path, path: &Path) -> Option<String> {
    let joined = |rel: &Path| -> Option<String> {
        let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
        Some(parts?.join("/"))
    };
    if let Ok(rel) = path.strip_prefix(base) {
        return joined(rel);
    }
    // One may name the other through a link, as macOS's /tmp does.
    let base = std::fs::canonicalize(base).ok()?;
    let path = std::fs::canonicalize(path).ok()?;
    joined(path.strip_prefix(&base).ok()?)
}

/// `path`, from the directory `base` (from the root, `/`-separated), as a pattern from the root;
/// `None` when it leaves the repository or is absolute.
fn join(base: &str, path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    if path.is_empty() || path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// One segment of a pattern.
enum Segment {
    /// `**`: up to [`DEEPEST`] directories.
    Any,
    /// A pattern of one name, and its text.
    One(Pattern, String),
}

/// A pattern from the root, and how deep a match of it can be.
struct Glob {
    whole: Pattern,
    segments: Vec<Segment>,
    deepest: usize,
}

impl Glob {
    fn new(source: &str) -> Option<Glob> {
        let whole = Pattern::new(source).ok()?;
        let mut segments = Vec::new();
        let mut deepest = 0usize;
        for part in source.split('/') {
            if part == "**" {
                segments.push(Segment::Any);
                deepest += DEEPEST;
            } else {
                segments.push(Segment::One(Pattern::new(part).ok()?, part.to_string()));
                deepest += 1;
            }
        }
        Some(Glob {
            whole,
            segments,
            deepest,
        })
    }

    /// Whether a file can match below the directory `dir`, by its segments from the root.
    fn reaches(&self, dir: &[String]) -> bool {
        // A file below it is at least one level deeper.
        dir.len() < self.deepest && self.reaches_at_any_depth(dir)
    }

    /// Whether a file could match below `dir` if `**` went down without end.
    fn too_deep_at(&self, dir: &[String]) -> bool {
        dir.len() >= self.deepest && self.reaches_at_any_depth(dir)
    }

    fn reaches_at_any_depth(&self, dir: &[String]) -> bool {
        for (i, name) in dir.iter().enumerate() {
            match self.segments.get(i) {
                Some(Segment::Any) => return dir[i..].iter().all(|name| !name.starts_with('.')),
                Some(Segment::One(pattern, _)) if i + 1 < self.segments.len() => {
                    if !pattern.is_match(name) {
                        return false;
                    }
                }
                // The last segment names the file: a directory can't hold more.
                _ => return false,
            }
        }
        true
    }

    /// Whether a file can match below the directory `dir` because the pattern names each of its
    /// directories, word for word: no `**`, `*` or class stands for one.
    fn names(&self, dir: &[String]) -> bool {
        dir.len() < self.segments.len()
            && dir
                .iter()
                .zip(&self.segments)
                .all(|(name, segment)| matches!(segment, Segment::One(_, text) if text == name))
    }

    fn matches(&self, path: &str, depth: usize) -> bool {
        depth <= self.deepest && self.whole.is_match(path)
    }
}

/// What a search may still spend.
struct Budget {
    entries: usize,
    until: Instant,
}

impl Budget {
    /// One more entry read: `None` past the bounds.
    fn spend(&mut self) -> Option<()> {
        self.entries += 1;
        if self.entries > MOST_ENTRIES {
            tracing::debug!(
                "looking for reports read more than {MOST_ENTRIES} entries: no summary"
            );
            return None;
        }
        if self.entries.is_multiple_of(256) && Instant::now() > self.until {
            tracing::debug!("looking for reports took more than {MOST_TIME:?}: no summary");
            return None;
        }
        Some(())
    }
}

/// The files below `root` that some glob matches, as `found`; and whether it left unread what one
/// of `readers`, the globs of the job's readers, matches or reaches: a directory too deep, one it
/// couldn't read, or a link.
fn walk(
    root: &Path,
    globs: &[Glob],
    readers: &[Glob],
    budget: &mut Budget,
    found: &mut Vec<Found>,
) -> Option<bool> {
    let mut cut_short = false;
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(root.to_path_buf(), Vec::new())];
    while let Some((dir, segments)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            cut_short = cut_short || readers.iter().any(|glob| glob.reaches(&segments));
            continue;
        };
        for entry in entries.flatten() {
            budget.spend()?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // The entry itself, never what a link points to.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let mut these = segments.clone();
            these.push(name);
            if kind.is_dir() {
                if SKIPPED.contains(&these[these.len() - 1].as_str()) {
                    // No pattern goes into it through a wildcard, but a reader's path that names
                    // it reads where the search doesn't go (C-27).
                    cut_short = cut_short || readers.iter().any(|glob| glob.names(&these));
                    continue;
                }
                cut_short = cut_short || readers.iter().any(|glob| glob.too_deep_at(&these));
                if globs.iter().any(|glob| glob.reaches(&these)) {
                    stack.push((entry.path(), these));
                }
            } else if kind.is_file() {
                let rel = these.join("/");
                if globs.iter().any(|glob| glob.matches(&rel, these.len())) {
                    found.push(Found {
                        path: entry.path(),
                        rel,
                    });
                }
            } else if kind.is_symlink() {
                // A link is never followed: what a reader reads through one is out of sight, the
                // link's file or what is below its directory.
                let rel = these.join("/");
                let to_a_dir = std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir());
                cut_short = cut_short
                    || readers.iter().any(|glob| {
                        glob.matches(&rel, these.len()) || (to_a_dir && glob.reaches(&these))
                    });
            }
        }
    }
    Some(cut_short)
}

/// The value of the option `names` at `words[i]`: `--name=value`, or `--name value`.
fn value<'a>(words: &'a [String], i: usize, names: &[&str]) -> Option<&'a str> {
    let word = words[i].as_str();
    names.iter().find_map(|name| {
        if word == *name {
            words.get(i + 1).map(String::as_str)
        } else {
            word.strip_prefix(name)?.strip_prefix('=')
        }
    })
}

/// The report files and directories `words`, a command, names (from where it ran): a directory
/// comes back as itself and as what is below it. `nextest` gives the JUnit file a nextest
/// profile writes, when its configuration has one.
fn command_paths(words: &[String], nextest: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let dir = |paths: &mut Vec<String>, d: &str| {
        let d = d.trim_end_matches('/');
        paths.push(d.to_string());
        paths.push(format!("{d}/**/*"));
    };
    let has = |flag: &str| words.iter().any(|w| w == flag);
    let junit_reporter =
        (0..words.len()).any(|i| value(words, i, &["--reporter"]) == Some("junit"));
    let llvm_cov = has("--lcov") || has("--cobertura");
    let mut results_directory: Option<&str> = None;
    let mut rspec_junit = false;
    for i in 0..words.len() {
        // pytest
        if let Some(p) = value(words, i, &["--junitxml", "--junit-xml"]) {
            paths.push(p.to_string());
        }
        // pytest-cov
        if let Some(report) = value(words, i, &["--cov-report"]) {
            match report.split_once(':') {
                Some(("xml" | "lcov" | "json", p)) if !p.is_empty() => paths.push(p.to_string()),
                _ => {
                    let kind = report.split(':').next().unwrap_or_default();
                    if let Some(default) = match kind {
                        "xml" => Some("coverage.xml"),
                        "lcov" => Some("coverage.lcov"),
                        "json" => Some("coverage.json"),
                        _ => None,
                    } {
                        paths.push(default.to_string());
                    }
                }
            }
        }
        // go
        if let Some(p) = value(words, i, &["-coverprofile", "--coverprofile"]) {
            paths.push(p.to_string());
        }
        // jest, vitest
        if let Some(d) = value(
            words,
            i,
            &["--coverageDirectory", "--coverage.reportsDirectory"],
        ) {
            dir(&mut paths, d);
        }
        if let Some(p) = value(words, i, &["--outputFile.junit"]) {
            paths.push(p.to_string());
        }
        if junit_reporter && let Some(p) = value(words, i, &["--outputFile"]) {
            paths.push(p.to_string());
        }
        // phpunit
        if let Some(p) = value(
            words,
            i,
            &["--log-junit", "--coverage-clover", "--coverage-cobertura"],
        ) {
            paths.push(p.to_string());
        }
        // cargo llvm-cov
        if llvm_cov && let Some(p) = value(words, i, &["--output-path"]) {
            paths.push(p.to_string());
        }
        // dotnet
        if let Some(d) = value(words, i, &["--results-directory"]) {
            results_directory = Some(d);
            dir(&mut paths, d);
        }
        // rspec: `--out` writes what the formatter before it formats.
        if let Some(format) = value(words, i, &["--format", "-f"]) {
            rspec_junit = format.to_ascii_lowercase().contains("junit");
        }
        if rspec_junit && let Some(p) = value(words, i, &["--out", "-o"]) {
            paths.push(p.to_string());
        }
    }
    // dotnet's TRX logger names its file in the results directory.
    for i in 0..words.len() {
        if let Some(logger) = value(words, i, &["--logger", "-l"])
            && let Some(name) = logger
                .split(';')
                .find_map(|part| part.trim().strip_prefix("LogFileName="))
        {
            let base = results_directory
                .unwrap_or("TestResults")
                .trim_end_matches('/');
            paths.push(format!("{base}/{name}"));
        }
    }
    // nextest's profile: `--profile X`, `-P X`, else `default`.
    if words.iter().any(|w| w == "nextest") {
        let profile = words
            .iter()
            .enumerate()
            .find_map(|(i, _)| value(words, i, &["--profile", "-P"]))
            .unwrap_or("default");
        paths.extend(nextest(profile));
    }
    paths
}

/// The JUnit file nextest's `profile` writes, from the command's directory, when
/// `.config/nextest.toml` there or at the root sets `[profile.<profile>.junit] path`.
fn nextest_junit(q: &Query<'_>, profile: &str) -> Option<String> {
    let (config, base) = [q.cwd, q.root].iter().find_map(|dir| {
        let text = std::fs::read_to_string(dir.join(".config/nextest.toml")).ok()?;
        Some((text, *dir))
    })?;
    let section = format!("[profile.{profile}.junit]");
    let mut inside = false;
    let path = config.lines().map(str::trim).find_map(|line| {
        if line.starts_with('[') {
            inside = line == section;
            return None;
        }
        let (key, value) = line.split_once('=')?;
        (inside && key.trim() == "path").then(|| value.trim().trim_matches(['"', '\'']).to_string())
    })?;
    let target = format!("target/nextest/{profile}/{path}");
    // From the command's directory, as the other paths are.
    if base == q.cwd {
        Some(target)
    } else {
        let back = rel_of(q.root, q.cwd)?;
        let ups = back
            .split('/')
            .filter(|p| !p.is_empty())
            .map(|_| "..")
            .collect::<Vec<_>>();
        Some(if ups.is_empty() {
            target
        } else {
            format!("{}/{target}", ups.join("/"))
        })
    }
}
