//! Which steps of a CI job read reports a check may write (C-26): one catalog, for init's plan,
//! `hushrun ready`, `hushrun doctor` and `hushrun run`. A skip writes no report, so in a GitHub
//! Actions job where a step reads reports, a pass doesn't skip, and the step finds what it reads.
//! Adding a reader is adding a row, with an example the tests read.
//!
//! A step reads reports when it uses an action of [`ACTIONS`]; uploads an artifact whose path
//! names a report; uses an action the table doesn't know, with an input that names a report file;
//! runs a command of [`COMMANDS`]; or uses a local composite action one of whose steps reads
//! reports, one level deep. A path alone never makes a `run:` step a reader: otherwise
//! `hushrun run unit -- jest --coverage` would be its own. A step that runs only after a failure
//! reads nothing after a skip, which succeeds.

use serde_json::Value;

use super::checks;
use super::scripts;

/// What a reader reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Coverage reports.
    Coverage,
    /// Test results.
    Tests,
    /// Other reports, such as SARIF or a code scanner's.
    Other,
}

impl Kind {
    /// The word init's plan says.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Coverage => "coverage",
            Kind::Tests => "tests",
            Kind::Other => "other",
        }
    }
}

/// A step that reads reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reader {
    /// Its place among its job's steps, from 0.
    pub step: usize,
    /// Its `uses:` without the `@ref`, else its `name:`, else `run: ` and at most 40 characters of
    /// its command, cut after a word, with `…` when cut.
    pub name: String,
    /// What it reads, in order, each once.
    pub kinds: Vec<Kind>,
    /// The paths its inputs name, as written: empty when it reads whatever it finds.
    pub paths: Vec<String>,
    /// It runs only after an earlier step failed (`if: failure()`), so never after a skip.
    pub only_on_failure: bool,
    /// \[ADR 0048\] Its paths are all it reads. Not when it names none, nor when it looks for
    /// reports of its own as well, as Codecov's actions and commands do: then hushrun can't tell
    /// what it reads (C-26).
    pub bounded: bool,
}

impl Reader {
    /// Whether it reads only coverage or test results, which a linter's skip leaves as they were.
    pub fn coverage_or_tests_only(&self) -> bool {
        self.kinds
            .iter()
            .all(|kind| matches!(kind, Kind::Coverage | Kind::Tests))
    }
}

/// An action that reads reports.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Action {
    /// `owner/repo` or `owner/repo/path`, `|` between the ways to write it, matched ignoring case.
    pub uses: &'static str,
    pub kinds: &'static [Kind],
    /// The inputs that hold the paths it reads, as its `action.yml` names them.
    pub inputs: &'static [&'static str],
    /// A step that uses it, which the tests read.
    pub example: &'static str,
}

const C: &[Kind] = &[Kind::Coverage];
const T: &[Kind] = &[Kind::Tests];
const CT: &[Kind] = &[Kind::Coverage, Kind::Tests];
const O: &[Kind] = &[Kind::Other];

/// The actions that read reports, with the inputs that hold paths, each checked against its
/// `action.yml`.
pub const ACTIONS: &[Action] = &[
    // `file` is v4's and earlier's.
    Action {
        uses: "codecov/codecov-action",
        kinds: C,
        inputs: &["files", "file", "directory"],
        example: "{uses: codecov/codecov-action@v5, with: {files: coverage.xml}}",
    },
    Action {
        uses: "codecov/test-results-action",
        kinds: T,
        inputs: &["files", "file", "directory"],
        example: "{uses: codecov/test-results-action@v1, with: {files: junit.xml}}",
    },
    Action {
        uses: "coverallsapp/github-action",
        kinds: C,
        inputs: &["file", "files", "path-to-lcov", "base-path"],
        example: "{uses: coverallsapp/github-action@v2, with: {file: coverage/lcov.info}}",
    },
    // It finds `.coverage` where COVERAGE_PATH says, by default the root.
    Action {
        uses: "py-cov-action/python-coverage-comment-action",
        kinds: C,
        inputs: &["COVERAGE_PATH"],
        example: "{uses: py-cov-action/python-coverage-comment-action@v3, with: {GITHUB_TOKEN: x}}",
    },
    Action {
        uses: "MishaKav/pytest-coverage-comment",
        kinds: CT,
        inputs: &[
            "pytest-coverage-path",
            "pytest-xml-coverage-path",
            "pytest-json-coverage-path",
            "junitxml-path",
        ],
        example: "{uses: MishaKav/pytest-coverage-comment@main, with: {junitxml-path: pytest.xml}}",
    },
    Action {
        uses: "MishaKav/jest-coverage-comment",
        kinds: CT,
        inputs: &["coverage-summary-path", "coverage-path", "junitxml-path"],
        example: "{uses: MishaKav/jest-coverage-comment@main, with: {coverage-summary-path: coverage/coverage-summary.json}}",
    },
    Action {
        uses: "ArtiomTr/jest-coverage-report-action",
        kinds: C,
        inputs: &["coverage-file", "base-coverage-file"],
        example: "{uses: ArtiomTr/jest-coverage-report-action@v2, with: {coverage-file: report.json}}",
    },
    Action {
        uses: "romeovs/lcov-reporter-action",
        kinds: C,
        inputs: &["lcov-file", "lcov-base"],
        example: "{uses: romeovs/lcov-reporter-action@v0.4.0, with: {lcov-file: coverage/lcov.info}}",
    },
    Action {
        uses: "zgosalvez/github-actions-report-lcov",
        kinds: C,
        inputs: &["coverage-files"],
        example: "{uses: zgosalvez/github-actions-report-lcov@v4, with: {coverage-files: coverage/lcov.*.info}}",
    },
    Action {
        uses: "irongut/CodeCoverageSummary",
        kinds: C,
        inputs: &["filename"],
        example: "{uses: irongut/CodeCoverageSummary@v1.3.0, with: {filename: coverage/**/coverage.cobertura.xml}}",
    },
    Action {
        uses: "5monkeys/cobertura-action",
        kinds: C,
        inputs: &["path"],
        example: "{uses: 5monkeys/cobertura-action@master, with: {path: coverage.xml}}",
    },
    Action {
        uses: "danielpalme/ReportGenerator-GitHub-Action",
        kinds: C,
        inputs: &["reports"],
        example: "{uses: danielpalme/ReportGenerator-GitHub-Action@5, with: {reports: coverage.xml}}",
    },
    Action {
        uses: "madrapps/jacoco-report",
        kinds: C,
        inputs: &["paths"],
        example: "{uses: madrapps/jacoco-report@v1.7.2, with: {paths: build/reports/jacoco/test/jacocoTestReport.xml}}",
    },
    Action {
        uses: "insightsengineering/coverage-action",
        kinds: C,
        inputs: &["path"],
        example: "{uses: insightsengineering/coverage-action@v2, with: {path: coverage.xml}}",
    },
    Action {
        uses: "orgoro/coverage",
        kinds: C,
        inputs: &["coverageFile"],
        example: "{uses: orgoro/coverage@v3.2, with: {coverageFile: coverage.xml}}",
    },
    Action {
        uses: "paambaati/codeclimate-action",
        kinds: C,
        inputs: &["coverageLocations"],
        example: "{uses: paambaati/codeclimate-action@v9, with: {coverageLocations: 'coverage/lcov.info:lcov'}}",
    },
    Action {
        uses: "qltysh/qlty-action/coverage",
        kinds: C,
        inputs: &["files"],
        example: "{uses: qltysh/qlty-action/coverage@v2, with: {files: coverage/lcov.info}}",
    },
    // A scanner reads what its own properties name.
    Action {
        uses: "SonarSource/sonarqube-scan-action|SonarSource/sonarcloud-github-action",
        kinds: O,
        inputs: &[],
        example: "{uses: SonarSource/sonarqube-scan-action@v5}",
    },
    Action {
        uses: "dorny/test-reporter",
        kinds: T,
        inputs: &["path"],
        example: "{uses: dorny/test-reporter@v2, with: {name: tests, path: 'reports/*.xml', reporter: java-junit}}",
    },
    Action {
        uses: "EnricoMi/publish-unit-test-result-action|EnricoMi/publish-unit-test-result-action/linux|EnricoMi/publish-unit-test-result-action/macos|EnricoMi/publish-unit-test-result-action/windows|EnricoMi/publish-unit-test-result-action/composite",
        kinds: T,
        inputs: &[
            "files",
            "junit_files",
            "nunit_files",
            "xunit_files",
            "trx_files",
        ],
        example: "{uses: EnricoMi/publish-unit-test-result-action/linux@v2, with: {files: 'test-results/**/*.xml'}}",
    },
    Action {
        uses: "mikepenz/action-junit-report",
        kinds: T,
        inputs: &["report_paths"],
        example: "{uses: mikepenz/action-junit-report@v5, with: {report_paths: '**/build/test-results/test/TEST-*.xml'}}",
    },
    Action {
        uses: "test-summary/action",
        kinds: T,
        inputs: &["paths"],
        example: "{uses: test-summary/action@v2, with: {paths: 'test/results/**/TEST-*.xml'}}",
    },
    Action {
        uses: "scacap/action-surefire-report",
        kinds: T,
        inputs: &["report_paths"],
        example: "{uses: scacap/action-surefire-report@v1}",
    },
    Action {
        uses: "ctrf-io/github-test-reporter",
        kinds: T,
        inputs: &["report-path"],
        example: "{uses: ctrf-io/github-test-reporter@v1, with: {report-path: './ctrf/*.json'}}",
    },
    Action {
        uses: "trunk-io/analytics-uploader",
        kinds: T,
        inputs: &[
            "junit-paths",
            "xcresult-path",
            "swift-test-xunit-paths",
            "bazel-bep-path",
        ],
        example: "{uses: trunk-io/analytics-uploader@v1, with: {junit-paths: 'test-results/*.xml'}}",
    },
    Action {
        uses: "datadog/junit-upload-github-action",
        kinds: T,
        inputs: &["files"],
        example: "{uses: datadog/junit-upload-github-action@v2, with: {files: ./reports}}",
    },
    Action {
        uses: "pmeier/pytest-results-action",
        kinds: T,
        inputs: &["path"],
        example: "{uses: pmeier/pytest-results-action@main, with: {path: test-results.xml}}",
    },
    Action {
        uses: "phoenix-actions/test-reporting",
        kinds: T,
        inputs: &["path"],
        example: "{uses: phoenix-actions/test-reporting@v15, with: {name: tests, path: 'reports/*.xml', reporter: java-junit}}",
    },
    Action {
        uses: "github/codeql-action/upload-sarif",
        kinds: O,
        inputs: &["sarif_file"],
        example: "{uses: github/codeql-action/upload-sarif@v3, with: {sarif_file: results.sarif}}",
    },
];

/// The uploads whose path decides: an artifact that holds a report reads it.
const UPLOADS: &str = "actions/upload-artifact|actions/upload-pages-artifact";

/// A command of a `run:` step that reads reports.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Command {
    /// The program, `|` between the ways to write it.
    pub program: &'static str,
    /// What follows it in order, options aside, `|` between alternatives.
    pub words: &'static [&'static str],
    /// A word it must also have, `|` between alternatives; empty for none.
    pub has: &'static str,
    pub kind: Kind,
    /// A line that runs it, which the tests read.
    pub example: &'static str,
}

const fn command(
    program: &'static str,
    words: &'static [&'static str],
    has: &'static str,
    kind: Kind,
    example: &'static str,
) -> Command {
    Command {
        program,
        words,
        has,
        kind,
        example,
    }
}

/// The commands that read reports. Codecov's old script, `bash <(curl -s https://codecov.io/bash)`,
/// is known by its address.
pub const COMMANDS: &[Command] = &[
    command(
        "codecov|./codecov",
        &[],
        "",
        Kind::Coverage,
        "./codecov -t token",
    ),
    command(
        "codecovcli",
        &["do-upload|upload-process|upload-coverage"],
        "",
        Kind::Coverage,
        "codecovcli upload-process -f coverage.xml",
    ),
    command(
        "codecovcli",
        &[],
        "-f|--file",
        Kind::Coverage,
        "codecovcli --verbose -f coverage.xml",
    ),
    command(
        "coveralls",
        &[],
        "",
        Kind::Coverage,
        "coveralls --service=github",
    ),
    command(
        "coverage",
        &["combine|report|xml|html|json|lcov"],
        "",
        Kind::Coverage,
        "python -m coverage xml",
    ),
    command(
        "genhtml",
        &[],
        "",
        Kind::Coverage,
        "genhtml lcov.info -o out",
    ),
    command("lcov", &[], "", Kind::Coverage, "lcov --list coverage.info"),
    command(
        "llvm-cov",
        &["report|show|export"],
        "",
        Kind::Coverage,
        "xcrun llvm-cov export -format=lcov",
    ),
    command(
        "cargo",
        &["llvm-cov", "report"],
        "",
        Kind::Coverage,
        "cargo llvm-cov report --lcov --output-path lcov.info",
    ),
    command(
        "go",
        &["tool", "cover"],
        "",
        Kind::Coverage,
        "go tool cover -func=coverage.out",
    ),
    command("gcovr", &[], "", Kind::Coverage, "gcovr --xml coverage.xml"),
    command(
        "nyc",
        &["report"],
        "",
        Kind::Coverage,
        "npx nyc report --reporter=lcov",
    ),
    command("c8", &["report"], "", Kind::Coverage, "npx c8 report"),
    command(
        "reportgenerator",
        &[],
        "",
        Kind::Coverage,
        "reportgenerator -reports:coverage.xml -targetdir:out",
    ),
    command(
        "datadog-ci",
        &["junit", "upload"],
        "",
        Kind::Tests,
        "npx datadog-ci junit upload --service app reports/",
    ),
    command(
        "trunk|./trunk",
        &["flakytests", "upload"],
        "",
        Kind::Tests,
        "./trunk flakytests upload --junit-paths 'out/*.xml'",
    ),
    command(
        "junit2html",
        &[],
        "",
        Kind::Tests,
        "junit2html junit.xml out.html",
    ),
    command(
        "allure",
        &["generate"],
        "",
        Kind::Tests,
        "allure generate allure-results",
    ),
    command("sonar-scanner", &[], "", Kind::Other, "sonar-scanner"),
    command(
        "mvn|./mvnw|mvnw",
        &[],
        "sonar:sonar",
        Kind::Other,
        "./mvnw -B verify sonar:sonar",
    ),
    command(
        "gradle|./gradlew|gradlew",
        &[],
        "sonar|sonarqube",
        Kind::Other,
        "./gradlew sonar",
    ),
];

/// What start a program another reads with: a project's launchers, and Apple's.
const LAUNCHERS: &[&[&str]] = &[&["xcrun"]];

/// What a path holds when it names a report, matched ignoring case.
const TEST_TOKENS: &[&str] = &[
    "junit",
    "test-results",
    "test_results",
    "testresults",
    "test-reports",
    "surefire-reports",
    "failsafe-reports",
    "nextest",
    ".trx",
    "results.xml",
    "report.xml",
    "allure-results",
];
const COVERAGE_TOKENS: &[&str] = &[
    "coverage",
    "lcov",
    "cobertura",
    "jacoco",
    "clover",
    "kover",
    "htmlcov",
    ".coverage",
    "coverprofile",
    "cover.out",
];
const OTHER_TOKENS: &[&str] = &[".sarif", "reports/"];
/// Surefire's and Gradle's test results, `TEST-*.xml`: in capitals, since `test-plan` or
/// `test-utils` is no report.
const SUREFIRE: &str = "TEST-";

/// How a report file's name ends.
const EXTENSIONS: &[&str] = &[
    ".xml",
    ".info",
    ".lcov",
    ".json",
    ".out",
    ".trx",
    ".sarif",
    ".xcresult",
];

/// The steps of `job`, a workflow's `jobs.<id>`, that read reports. `read` reads a file of the
/// repository by its path from the root, `/` separated: the run reads its checkout, and init's
/// plan the files as init would leave them.
pub fn readers(job: &Value, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Vec<Reader> {
    let steps = job.get("steps").and_then(Value::as_array);
    steps
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(at, step)| reader(at, step, read, true))
        .collect()
}

/// What `step`, the job's `at`th, reads, if it reads reports. `deeper` says a local composite
/// action is read too: a step of one is read with it set to false.
fn reader(
    at: usize,
    step: &Value,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    deeper: bool,
) -> Option<Reader> {
    let found = match (text(step, "uses"), text(step, "run")) {
        (Some(uses), _) => action_reads(uses, step, read, deeper),
        (None, Some(run)) => run_reads(run),
        (None, None) => None,
    }?;
    let Found {
        kinds,
        paths,
        only_on_failure: inner_only_on_failure,
        bounded,
    } = found;
    let uses = text(step, "uses").map(|uses| uses.split('@').next().unwrap_or(uses));
    let name = uses
        .map(str::to_string)
        .or_else(|| text(step, "name").map(str::to_string))
        .unwrap_or_else(|| {
            let run = text(step, "run").unwrap_or_default();
            let first = run
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            format!("run: {}", shortened(first.trim(), 40))
        });
    Some(Reader {
        step: at,
        name,
        kinds: once_each(kinds),
        bounded: bounded && !paths.is_empty(),
        paths,
        only_on_failure: inner_only_on_failure || text(step, "if").is_some_and(only_on_failure),
    })
}

/// What a reader found: its kinds, its paths, whether all it holds runs only on failure, and
/// whether its paths are all it reads.
struct Found {
    kinds: Vec<Kind>,
    paths: Vec<String>,
    only_on_failure: bool,
    bounded: bool,
}

/// \[ADR 0048\] The actions that look for reports of their own besides the files they're given,
/// unless `disable_search` is `true`, or only in `directory`.
const SEARCHING: &[&str] = &["codecov/codecov-action", "codecov/test-results-action"];

fn action_reads(
    uses: &str,
    step: &Value,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    deeper: bool,
) -> Option<Found> {
    let action = uses.split('@').next().unwrap_or(uses).trim();
    let with = step.get("with").and_then(Value::as_object);
    let values = |input: &str| -> Vec<String> {
        with.and_then(|with| with.get(input))
            .and_then(Value::as_str)
            .map(paths_of)
            .unwrap_or_default()
    };
    let named = |names: &str| {
        names
            .split('|')
            .any(|name| name.eq_ignore_ascii_case(action))
    };
    if let Some(row) = ACTIONS.iter().find(|row| named(row.uses)) {
        let paths: Vec<String> = row.inputs.iter().flat_map(|input| values(input)).collect();
        let searches = SEARCHING.iter().any(|uses| named(uses));
        let input = |name: &str| with.and_then(|with| with.get(name));
        let bounded = !searches
            || input("disable_search").is_some_and(|v| v == true || v.as_str() == Some("true"))
            || input("directory")
                .and_then(Value::as_str)
                .is_some_and(|d| !d.trim().is_empty());
        return Some(Found {
            kinds: row.kinds.to_vec(),
            paths,
            only_on_failure: false,
            bounded,
        });
    }
    if named(UPLOADS) {
        let mut kinds = Vec::new();
        let mut paths = Vec::new();
        for path in values("path") {
            if let Some(found) = report_like(&path, true) {
                kinds.extend(found);
                paths.push(path);
            }
        }
        return (!kinds.is_empty()).then_some(Found {
            kinds,
            paths,
            only_on_failure: false,
            bounded: true,
        });
    }
    if let Some(dir) = action.strip_prefix("./") {
        return deeper.then(|| composite_reads(dir, read)).flatten();
    }
    // An action the table doesn't know reads a report when an input names one, and may read
    // more than it names.
    let mut kinds = Vec::new();
    let mut paths = Vec::new();
    for value in with.into_iter().flat_map(|with| with.values()) {
        for path in value.as_str().map(paths_of).unwrap_or_default() {
            if let Some(found) = report_like(&path, false) {
                kinds.extend(found);
                paths.push(path);
            }
        }
    }
    (!kinds.is_empty()).then_some(Found {
        kinds,
        paths,
        only_on_failure: false,
        bounded: false,
    })
}

/// What the local composite action in `dir` reads, through its own steps: they run where the step
/// that uses it does.
fn composite_reads(dir: &str, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Option<Found> {
    let dir = dir.trim_end_matches('/');
    if dir.split('/').any(|part| part == "..") || dir.starts_with('/') {
        return None;
    }
    let (path, bytes) = ["action.yml", "action.yaml"].iter().find_map(|name| {
        let path = if dir.is_empty() {
            (*name).to_string()
        } else {
            format!("{dir}/{name}")
        };
        read(&path).map(|bytes| (path, bytes))
    })?;
    let action: Value = hushrun_core::yaml::parse(&path, &bytes).ok()?;
    let runs = action.get("runs")?;
    if !text(runs, "using").is_some_and(|using| using.eq_ignore_ascii_case("composite")) {
        return None;
    }
    let inner: Vec<Reader> = runs
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(at, step)| reader(at, step, read, false))
        .collect();
    if inner.is_empty() {
        return None;
    }
    let only_on_failure = inner.iter().all(|reader| reader.only_on_failure);
    let bounded = inner.iter().all(|reader| reader.bounded);
    Some(Found {
        kinds: inner
            .iter()
            .flat_map(|reader| reader.kinds.clone())
            .collect(),
        paths: inner.into_iter().flat_map(|reader| reader.paths).collect(),
        only_on_failure,
        bounded,
    })
}

/// \[ADR 0047\] What a GitLab job's own `artifacts` keep of the reports a check writes, which
/// GitLab reads once the job is done: `artifacts:reports:<kind>` for the first report it declares,
/// but `dotenv`, which passes variables to later jobs rather than what a check found, else
/// `artifacts:paths` when one of its paths names a report. Nothing reads `.gitlab-ci.yml` at run
/// time, so init says it in the job's variables (C-26).
pub fn gitlab_reads(job: &Value) -> Option<String> {
    let artifacts = job.get("artifacts")?;
    if let Some(kind) = artifacts
        .get("reports")
        .and_then(Value::as_object)
        .and_then(|reports| reports.keys().find(|kind| kind.as_str() != "dotenv"))
    {
        return Some(format!("artifacts:reports:{kind}"));
    }
    let paths = artifacts.get("paths").and_then(Value::as_array)?;
    paths
        .iter()
        .filter_map(Value::as_str)
        .any(|path| report_like(path, true).is_some())
        .then(|| "artifacts:paths".to_string())
}

/// What a `run:` step's commands read.
fn run_reads(run: &str) -> Option<Found> {
    let mut kinds = Vec::new();
    if run.contains("codecov.io/bash") {
        kinds.push(Kind::Coverage);
    }
    for line in logical_lines(run) {
        let mut here = Some(String::new());
        for found in scripts::commands(&line, &mut here) {
            let words: Vec<&str> = found
                .words
                .iter()
                .map(String::as_str)
                .skip_while(|word| scripts::assignment(word))
                .collect();
            kinds.extend(command_reads(&words));
        }
    }
    (!kinds.is_empty()).then_some(Found {
        kinds,
        paths: Vec::new(),
        only_on_failure: false,
        bounded: false,
    })
}

/// The kind of report `words`, one command, read, if they read one.
pub fn command_reads(words: &[&str]) -> Option<Kind> {
    let mut words = checks::without_launchers(words);
    while let Some(launcher) = LAUNCHERS
        .iter()
        .find(|launcher| words.len() > launcher.len() && words.starts_with(launcher))
    {
        words = checks::without_launchers(&words[launcher.len()..]);
    }
    let (program, rest) = words.split_first()?;
    COMMANDS.iter().find_map(|row| {
        if !row.program.split('|').any(|name| name == *program) {
            return None;
        }
        if !row.has.is_empty()
            && !rest
                .iter()
                .any(|word| row.has.split('|').any(|has| has == *word))
        {
            return None;
        }
        let mut plain = rest.iter().filter(|word| !word.starts_with('-'));
        let in_order = row.words.iter().all(|wanted| {
            plain
                .next()
                .is_some_and(|word| wanted.split('|').any(|alternative| alternative == *word))
        });
        in_order.then_some(row.kind)
    })
}

/// A step's shell lines, with those a `\` continues joined.
fn logical_lines(run: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in run.lines() {
        match line.trim_end().strip_suffix('\\') {
            Some(start) => {
                current.push_str(start);
                current.push(' ');
            }
            None => {
                current.push_str(line);
                lines.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.trim().is_empty() {
        lines.push(current);
    }
    lines
}

/// The paths a value lists: one a line, or several split by commas, as these actions take them;
/// exclusions (`!…`) and comments aside.
fn paths_of(value: &str) -> Vec<String> {
    value
        .lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|path| !path.is_empty() && !path.starts_with('!') && !path.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// `text` without what CI fills in (`${{ … }}`): that names nothing, and what is around it still
/// can, as `target/test_results_${{ matrix.crate }}.json` does.
fn without_expressions(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${{") {
        plain.push_str(&rest[..start]);
        rest = rest[start..]
            .find("}}")
            .map_or("", |end| &rest[start + end + 2..]);
    }
    plain.push_str(rest);
    plain
}

/// The kinds of report `path` names, when it names one: it holds a token of one, and ends in a
/// report's extension, or in `/`, or, with `directories`, has no extension, as a directory.
fn report_like(path: &str, directories: bool) -> Option<Vec<Kind>> {
    let plain = without_expressions(path);
    let lower = plain.to_ascii_lowercase();
    let mut kinds = Vec::new();
    for (kind, tokens) in [
        (Kind::Tests, TEST_TOKENS),
        (Kind::Coverage, COVERAGE_TOKENS),
        (Kind::Other, OTHER_TOKENS),
    ] {
        let surefire = kind == Kind::Tests && plain.contains(SUREFIRE);
        if surefire || tokens.iter().any(|token| lower.contains(token)) {
            kinds.push(kind);
        }
    }
    let last = lower.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    // `.coverage` is a file without an extension, as a directory is.
    let no_extension = !last.trim_start_matches('.').contains('.');
    let ends = EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
        || lower.ends_with('/')
        || (directories && no_extension);
    (!kinds.is_empty() && ends).then_some(kinds)
}

/// Whether an `if:` says the step runs only when an earlier one failed: exactly `failure()`,
/// spaces and `${{ }}` aside.
fn only_on_failure(condition: &str) -> bool {
    let plain: String = condition.chars().filter(|c| !c.is_whitespace()).collect();
    let plain = plain
        .strip_prefix("${{")
        .and_then(|rest| rest.strip_suffix("}}"))
        .unwrap_or(&plain);
    plain == "failure()"
}

/// `text`, or its first words that fit in `most` characters, with `…`: a word cut in two reads
/// like a mistake.
fn shortened(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let cut: String = text.chars().take(most).collect();
    let words = match cut.rfind(' ') {
        Some(at) if at > 0 => cut[..at].trim_end(),
        _ => cut.as_str(),
    };
    format!("{words}…")
}

/// `kinds` in order, each once.
fn once_each(mut kinds: Vec<Kind>) -> Vec<Kind> {
    kinds.sort();
    kinds.dedup();
    kinds
}

/// The text of `value`'s `key`, when it is text.
fn text<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A job of `steps`, each a YAML flow mapping.
    fn job(steps: &[&str]) -> Value {
        let yaml = format!(
            "steps:\n{}",
            steps
                .iter()
                .map(|step| format!("  - {step}\n"))
                .collect::<String>()
        );
        hushrun_core::yaml::parse("job.yml", yaml.as_bytes()).expect("YAML")
    }

    fn none(_: &str) -> Option<Vec<u8>> {
        None
    }

    fn read_of(files: &[(&str, &str)]) -> impl Fn(&str) -> Option<Vec<u8>> {
        let files: BTreeMap<String, Vec<u8>> = files
            .iter()
            .map(|(path, text)| ((*path).to_string(), text.as_bytes().to_vec()))
            .collect();
        move |path: &str| files.get(path).cloned()
    }

    // covers: C-26
    #[test]
    fn every_action_reads_its_own_example() {
        for row in ACTIONS {
            let found = readers(&job(&["{uses: actions/checkout@v5}", row.example]), &none);
            assert_eq!(found.len(), 1, "{}: {found:?}", row.example);
            let reader = &found[0];
            assert_eq!(reader.step, 1, "{}", row.example);
            assert_eq!(
                reader.kinds,
                once_each(row.kinds.to_vec()),
                "{}",
                row.example
            );
            assert!(
                row.uses
                    .split('|')
                    .any(|uses| uses.eq_ignore_ascii_case(&reader.name)),
                "{}: {}",
                row.example,
                reader.name
            );
            assert!(!reader.only_on_failure);
            // The paths it names come from the inputs the row lists.
            let step = &job(&[row.example])["steps"][0];
            for path in &reader.paths {
                assert!(
                    row.inputs.iter().any(|input| step["with"][*input]
                        .as_str()
                        .is_some_and(|value| value.contains(path.as_str()))),
                    "{}: {path}",
                    row.example
                );
            }
        }
    }

    // covers: C-26
    #[test]
    fn every_command_reads_its_own_example() {
        for row in COMMANDS {
            let found = readers(&job(&[&format!("{{run: \"{}\"}}", row.example)]), &none);
            assert_eq!(found.len(), 1, "{}: {found:?}", row.example);
            assert_eq!(found[0].kinds, vec![row.kind], "{}", row.example);
            assert_eq!(
                found[0].name,
                format!("run: {}", shortened(row.example, 40)),
                "{}",
                row.example
            );
            assert!(found[0].paths.is_empty());
        }
    }

    // covers: C-26
    #[test]
    fn an_action_matches_by_name_whatever_its_ref_and_case() {
        for uses in [
            "codecov/codecov-action@v5",
            "codecov/codecov-action@671740ac38dd9b0130fbe1cec585b89eea48d3de",
            "Codecov/Codecov-Action@main",
        ] {
            let found = readers(&job(&[&format!("{{uses: {uses}}}")]), &none);
            assert_eq!(found.len(), 1, "{uses}");
            assert_eq!(found[0].kinds, vec![Kind::Coverage]);
        }
        let found = readers(
            &job(&[
                "{name: Upload, uses: codecov/codecov-action@v4, with: {file: ./coverage.xml, flags: unit}}",
            ]),
            &none,
        );
        assert_eq!(found[0].name, "codecov/codecov-action");
        assert_eq!(found[0].paths, ["./coverage.xml"]);
        // Several paths, one a line or split by commas.
        let found = readers(
            &job(&["{uses: codecov/codecov-action@v5, with: {files: './a.xml,./b.xml'}}"]),
            &none,
        );
        assert_eq!(found[0].paths, ["./a.xml", "./b.xml"]);
    }

    // covers: C-26
    #[test]
    fn an_artifact_reads_a_report_when_its_path_names_one() {
        let kinds = |path: &str| {
            readers(
                &job(&[&format!(
                    "{{uses: actions/upload-artifact@v4, with: {{name: out, path: '{path}'}}}}"
                )]),
                &none,
            )
            .first()
            .map(|reader| reader.kinds.clone())
        };
        assert_eq!(kinds("dist/"), None);
        assert_eq!(kinds("build/app.tar.gz"), None);
        assert_eq!(kinds("requirements-coverage.txt"), None);
        assert_eq!(kinds("coverage/"), Some(vec![Kind::Coverage]));
        assert_eq!(kinds("coverage"), Some(vec![Kind::Coverage]));
        assert_eq!(kinds(".coverage"), Some(vec![Kind::Coverage]));
        assert_eq!(kinds("htmlcov"), Some(vec![Kind::Coverage]));
        assert_eq!(kinds("test-results/**/*.xml"), Some(vec![Kind::Tests]));
        assert_eq!(
            kinds("target/nextest/ci/junit.xml"),
            Some(vec![Kind::Tests])
        );
        assert_eq!(kinds("**/TEST-*.xml"), Some(vec![Kind::Tests]));
        assert_eq!(kinds("results.sarif"), Some(vec![Kind::Other]));
        assert_eq!(kinds("reports/"), Some(vec![Kind::Other]));
        assert_eq!(kinds("reports/tests_new_models"), Some(vec![Kind::Other]));
        // What CI fills in names nothing, and what is around it still can.
        assert_eq!(
            kinds("target/test_results_${{ matrix.test_crate }}.json"),
            Some(vec![Kind::Tests])
        );
        assert_eq!(
            kinds("./webapp/channels/coverage/shard-${{ matrix.shard }}"),
            Some(vec![Kind::Coverage])
        );
        assert_eq!(kinds("${{ env.REPORTS }}"), None);
        // `TEST-` is Surefire's, in capitals: a test plan is no report.
        assert_eq!(kinds(".build/test-plan"), None);
        assert_eq!(kinds("packages/test-utils/dist"), None);
        // One path a line; exclusions and what CI fills in say nothing.
        let found = readers(
            &job(&[
                "{uses: actions/upload-pages-artifact@v3, with: {path: \"dist/\\n!coverage/tmp\\ncoverage/lcov.info\\n${{ env.OUT }}\"}}",
            ]),
            &none,
        );
        assert_eq!(found[0].paths, ["coverage/lcov.info"]);
        assert_eq!(found[0].name, "actions/upload-pages-artifact");
    }

    // covers: C-26
    #[test]
    fn an_unknown_action_reads_a_report_file_its_inputs_name() {
        let found = |with: &str| {
            readers(
                &job(&[&format!("{{uses: acme/publish@v1, with: {with}}}")]),
                &none,
            )
        };
        let reader = &found("{report: build/test-results/junit.xml}")[0];
        assert_eq!(reader.name, "acme/publish");
        assert_eq!(reader.kinds, [Kind::Tests]);
        assert_eq!(reader.paths, ["build/test-results/junit.xml"]);
        assert_eq!(found("{dir: coverage/}")[0].kinds, [Kind::Coverage]);
        // A name that holds a token, without a report's ending, or a directory without its `/`.
        assert!(found("{file: requirements-coverage.txt}").is_empty());
        assert!(found("{dir: coverage}").is_empty());
        assert!(found("{token: x, path: dist/app.json}").is_empty());
        assert!(readers(&job(&["{uses: acme/publish@v1}"]), &none).is_empty());
    }

    // covers: C-26
    #[test]
    fn a_run_step_reads_reports_only_through_a_command_that_does() {
        let reads = |run: &str| readers(&job(&[&format!("{{run: \"{run}\"}}")]), &none);
        for run in [
            "codecovcli upload-process -f coverage.xml",
            "cargo llvm-cov report --lcov --output-path lcov.info",
            "bash <(curl -s https://codecov.io/bash)",
            "python -m coverage combine && python -m coverage xml",
            "uv run coverage report -m",
            "set -e\\ncd web\\nnpx nyc report",
            "go tool cover \\\\\\n  -func=coverage.out",
        ] {
            assert_eq!(reads(run).len(), 1, "{run}");
        }
        // A path alone, or a command that writes reports, reads none.
        for run in [
            "pytest --cov --cov-report=xml",
            "hushrun run unit -- jest --coverage",
            "cat coverage.xml",
            "cargo llvm-cov --lcov --output-path lcov.info",
            "cargo llvm-cov show-env --export-prefix",
            "codecovcli create-commit",
            "mvn -B verify",
        ] {
            assert!(reads(run).is_empty(), "{run}");
        }
        let found = readers(&job(&["{name: Coverage report, run: coverage xml}"]), &none);
        assert_eq!(found[0].name, "Coverage report");
        // A long command, cut after a word.
        let found = readers(
            &job(&["{run: \"codecovcli upload-process -f coverage.xml --flag unit\"}"]),
            &none,
        );
        assert_eq!(found[0].name, "run: codecovcli upload-process -f…");
        assert_eq!(
            shortened(&"a".repeat(50), 40),
            format!("{}…", "a".repeat(40))
        );
    }

    // covers: C-26
    #[test]
    fn a_step_that_runs_only_on_failure_is_marked() {
        for condition in [
            "failure()",
            "${{ failure() }}",
            " failure( ) ",
            "${{failure()}}",
        ] {
            let step = format!("{{uses: codecov/codecov-action@v5, if: \"{condition}\"}}");
            let found = readers(&job(&[&step]), &none);
            assert!(found[0].only_on_failure, "{condition}");
        }
        for condition in [
            "always()",
            "success()",
            "failure() && github.ref == 'refs/heads/main'",
            "!cancelled()",
        ] {
            let step = format!("{{uses: codecov/codecov-action@v5, if: \"{condition}\"}}");
            let found = readers(&job(&[&step]), &none);
            assert!(!found[0].only_on_failure, "{condition}");
        }
    }

    // covers: C-26
    #[test]
    fn a_reader_is_bounded_when_its_paths_are_all_it_reads() {
        let bounded = |step: &str| {
            let found = readers(&job(&[step]), &none);
            assert_eq!(found.len(), 1, "{step}");
            found[0].bounded
        };
        assert!(bounded(
            "{uses: dorny/test-reporter@v2, with: {name: t, path: junit.xml, reporter: java-junit}}"
        ));
        assert!(bounded(
            "{uses: actions/upload-artifact@v4, with: {name: c, path: coverage/}}"
        ));
        // Codecov looks for coverage of its own, unless told not to or where.
        assert!(!bounded(
            "{uses: codecov/codecov-action@v5, with: {files: coverage.xml}}"
        ));
        assert!(bounded(
            "{uses: codecov/codecov-action@v5, with: {files: coverage.xml, disable_search: true}}"
        ));
        assert!(bounded(
            "{uses: codecov/codecov-action@v5, with: {directory: coverage}}"
        ));
        assert!(!bounded("{uses: codecov/codecov-action@v5}"));
        // It names nothing, is an action the catalog doesn't know, or a command.
        assert!(!bounded(
            "{uses: py-cov-action/python-coverage-comment-action@v3, with: {GITHUB_TOKEN: x}}"
        ));
        assert!(!bounded(
            "{uses: acme/report@v1, with: {report: test-results/junit.xml}}"
        ));
        assert!(!bounded("{run: coverage xml}"));
        // A local composite action is bounded when its steps are.
        let read = read_of(&[(
            ".github/actions/tests/action.yml",
            "runs:\n  using: composite\n  steps:\n    - uses: dorny/test-reporter@v2\n      with:\n        path: junit.xml\n",
        )]);
        let found = readers(&job(&["{uses: ./.github/actions/tests}"]), &read);
        assert!(found[0].bounded);
    }

    // covers: C-26
    #[test]
    fn a_local_composite_action_reads_what_its_steps_read_one_level_deep() {
        let read = read_of(&[
            (
                ".github/actions/upload/action.yml",
                "name: upload\nruns:\n  using: composite\n  steps:\n    - uses: codecov/codecov-action@v5\n      with:\n        files: coverage.xml\n",
            ),
            (
                ".github/actions/tests/action.yaml",
                "runs:\n  using: composite\n  steps:\n    - run: pytest\n      shell: bash\n",
            ),
            (
                ".github/actions/outer/action.yml",
                "runs:\n  using: composite\n  steps:\n    - uses: ./.github/actions/upload\n",
            ),
            (
                ".github/actions/node/action.yml",
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            (
                ".github/actions/on-failure/action.yml",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/upload-artifact@v4\n      if: failure()\n      with:\n        path: test-results/\n",
            ),
        ]);
        let found = readers(&job(&["{uses: ./.github/actions/upload}"]), &read);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "./.github/actions/upload");
        assert_eq!(found[0].kinds, [Kind::Coverage]);
        assert_eq!(found[0].paths, ["coverage.xml"]);
        // One that reads nothing, one level deeper, not composite, missing, or outside.
        for uses in [
            "./.github/actions/tests",
            "./.github/actions/outer",
            "./.github/actions/node",
            "./.github/actions/missing",
            "./../elsewhere",
        ] {
            assert!(
                readers(&job(&[&format!("{{uses: {uses}}}")]), &read).is_empty(),
                "{uses}"
            );
        }
        // What runs only on failure inside it runs only on failure.
        let found = readers(&job(&["{uses: ./.github/actions/on-failure}"]), &read);
        assert!(found[0].only_on_failure);
    }

    // covers: C-26
    #[test]
    fn a_reader_of_coverage_or_tests_only_is_told_from_one_of_others() {
        let found = readers(
            &job(&[
                "{uses: codecov/codecov-action@v5}",
                "{uses: dorny/test-reporter@v2}",
                "{uses: github/codeql-action/upload-sarif@v3}",
            ]),
            &none,
        );
        let only: Vec<bool> = found.iter().map(Reader::coverage_or_tests_only).collect();
        assert_eq!(only, [true, true, false]);
        assert_eq!(found[2].step, 2);
    }

    // covers: C-26
    #[test]
    fn a_gitlab_job_reads_reports_its_artifacts_keep() {
        let reads = |job: &str| {
            let job: Value = hushrun_core::yaml::parse("job.yml", job.as_bytes()).unwrap();
            gitlab_reads(&job)
        };
        assert_eq!(
            reads("script: [pytest]\nartifacts:\n  reports:\n    junit: report.xml\n").as_deref(),
            Some("artifacts:reports:junit")
        );
        assert_eq!(
            reads("artifacts:\n  reports:\n    coverage_report:\n      coverage_format: cobertura\n      path: coverage.xml\n")
                .as_deref(),
            Some("artifacts:reports:coverage_report")
        );
        assert_eq!(
            reads("artifacts:\n  paths: [dist/, coverage/]\n").as_deref(),
            Some("artifacts:paths")
        );
        assert_eq!(reads("artifacts:\n  paths: [dist/]\n"), None);
        assert_eq!(reads("script: [pytest]\n"), None);
        // A dotenv report passes variables to later jobs, not what a check found.
        assert_eq!(
            reads("artifacts:\n  reports:\n    dotenv: build.env\n"),
            None
        );
        assert_eq!(
            reads("artifacts:\n  reports:\n    dotenv: build.env\n    junit: report.xml\n")
                .as_deref(),
            Some("artifacts:reports:junit")
        );
        assert_eq!(
            reads("artifacts:\n  reports:\n    dotenv: build.env\n  paths: [coverage/]\n")
                .as_deref(),
            Some("artifacts:paths")
        );
    }

    // covers: C-26
    #[test]
    fn a_job_without_steps_reads_nothing() {
        let called: Value =
            hushrun_core::yaml::parse("job.yml", b"uses: ./.github/workflows/ci.yml\n").unwrap();
        assert!(readers(&called, &none).is_empty());
        assert!(
            readers(
                &job(&["{uses: actions/checkout@v5}", "{run: npm test}"]),
                &none
            )
            .is_empty()
        );
    }
}
