use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use proptest::prelude::*;

use super::*;

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/reports")
}

fn found(base: &Path, rel: &str) -> Found {
    Found {
        path: base.join(rel),
        rel: rel.to_string(),
    }
}

// covers: C-27
#[test]
fn every_vector_gives_its_summary() {
    let text = std::fs::read_to_string(testdata().join("vectors.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();
    assert!(vectors.len() >= 40, "{}", vectors.len());
    for vector in vectors {
        let name = vector["name"].as_str().unwrap();
        let files: Vec<Found> = vector["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| found(&testdata(), f.as_str().unwrap()))
            .collect();
        let got = summarize(&files, &Limits::default());
        let got = serde_json::to_value(&got).unwrap();
        assert_eq!(got, vector["reports"], "{name}");
    }
}

// covers: C-27
#[test]
fn numbers_read_as_people_write_them() {
    assert_eq!(count(0), "0");
    assert_eq!(count(999), "999");
    assert_eq!(count(1204), "1,204");
    assert_eq!(count(1_234_567), "1,234,567");
    let pct = |covered, total| percent(Ratio { covered, total });
    assert_eq!(pct(8123, 9310), "87.3%");
    assert_eq!(pct(1, 2), "50.0%");
    // Half up.
    assert_eq!(pct(1, 8), "12.5%");
    assert_eq!(pct(3, 16), "18.8%");
    // Never all for less than all, nor none for more than none.
    assert_eq!(pct(9996, 10000), "99.9%");
    assert_eq!(pct(4, 10000), "0.1%");
    assert_eq!(pct(10, 10), "100.0%");
    assert_eq!(pct(0, 10), "0.0%");
    assert_eq!(pct(u64::MAX - 1, u64::MAX), "99.9%");
}

// covers: C-27
#[test]
fn a_summary_says_tests_then_coverage() {
    let tests = |total, failed, errors, skipped| Tests {
        total,
        failed,
        errors,
        skipped,
        duration_ms: None,
    };
    let lines = Coverage {
        format: "cobertura".into(),
        lines: Some(Ratio {
            covered: 8123,
            total: 9310,
        }),
        statements: None,
        branches: None,
    };
    let reports = Reports {
        tests: Some(tests(1204, 0, 0, 14)),
        coverage: Some(lines.clone()),
        ..Reports::default()
    };
    assert_eq!(
        reports.summary().unwrap(),
        "1,204 tests, 14 skipped, 87.3% of lines covered"
    );
    // Failures and errors add up.
    let reports = Reports {
        tests: Some(tests(1204, 2, 1, 0)),
        ..Reports::default()
    };
    assert_eq!(reports.summary().unwrap(), "1,204 tests, 3 failed");
    let reports = Reports {
        tests: Some(tests(1, 0, 0, 0)),
        ..Reports::default()
    };
    assert_eq!(reports.summary().unwrap(), "1 test");
    let go = Coverage {
        format: "go".into(),
        lines: None,
        statements: Some(Ratio {
            covered: 3,
            total: 5,
        }),
        branches: None,
    };
    let reports = Reports {
        coverage: Some(go),
        ..Reports::default()
    };
    assert_eq!(reports.summary().unwrap(), "60.0% of statements covered");
    assert_eq!(Reports::default().summary(), None);
}

/// A directory with `files`, each written now.
fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

const JUNIT: &str = r#"<testsuite time="1"><testcase name="a"/><testcase name="b"><failure/></testcase></testsuite>"#;
const LCOV: &str = "SF:a.js\nDA:1,1\nDA:2,0\nend_of_record\n";

// covers: C-27
#[test]
fn a_file_over_its_limit_drops_its_kind_only() {
    let dir = tree(&[
        ("junit.xml", JUNIT),
        ("big/junit.xml", JUNIT),
        ("lcov.info", LCOV),
    ]);
    let files = [
        found(dir.path(), "junit.xml"),
        found(dir.path(), "lcov.info"),
        found(dir.path(), "big/junit.xml"),
    ];
    // The big one is a JUnit file past the limit: no test results, but coverage still.
    std::fs::write(
        dir.path().join("big/junit.xml"),
        format!("{JUNIT}<!--{}-->", "x".repeat(200)),
    )
    .unwrap();
    let limits = Limits {
        file_bytes: 150,
        ..Limits::default()
    };
    let reports = summarize(&files, &limits).unwrap();
    assert_eq!(reports.tests, None);
    assert_eq!(reports.coverage.unwrap().format, "lcov");
    // More of one kind than the total allows drops that kind.
    let limits = Limits {
        total_bytes: 200,
        ..Limits::default()
    };
    let files = [
        found(dir.path(), "junit.xml"),
        found(dir.path(), "lcov.info"),
    ];
    let both = summarize(&files, &limits).unwrap();
    assert!(both.tests.is_some() && both.coverage.is_some());
    let three = [
        found(dir.path(), "junit.xml"),
        found(dir.path(), "junit.xml").with_rel("again.xml", dir.path().join("big/junit.xml")),
        found(dir.path(), "lcov.info"),
    ];
    let reports = summarize(&three, &limits).unwrap();
    assert_eq!(reports.tests, None);
    assert!(reports.coverage.is_some());
    // Too many files: nothing.
    let limits = Limits {
        files: 1,
        ..Limits::default()
    };
    assert_eq!(summarize(&files, &limits), None);
    // Out of time: nothing.
    let limits = Limits {
        time: Duration::ZERO,
        ..Limits::default()
    };
    assert_eq!(summarize(&files, &limits), None);
}

impl Found {
    fn with_rel(mut self, rel: &str, path: PathBuf) -> Found {
        self.rel = rel.to_string();
        self.path = path;
        self
    }
}

// covers: C-27
#[test]
fn a_record_names_sixteen_files_and_counts_the_rest() {
    let mut files: Vec<(String, &str)> = (0..20)
        .map(|i| (format!("r/TEST-{i:02}.xml"), JUNIT))
        .collect();
    files.push((format!("{}/junit.xml", "d".repeat(170)), JUNIT));
    let owned: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), *t)).collect();
    let dir = tree(&owned);
    let found: Vec<Found> = files.iter().map(|(p, _)| found(dir.path(), p)).collect();
    let reports = summarize(&found, &Limits::default()).unwrap();
    assert_eq!(reports.files.len(), MOST_FILES);
    assert_eq!(reports.files_left_out, 5);
    assert_eq!(reports.tests.as_ref().unwrap().total, 42);
    assert!(serde_json::to_vec(&reports).unwrap().len() <= MOST_BYTES);
}

// covers: C-27
#[cfg(unix)]
#[test]
fn a_link_is_never_read() {
    let outside = tree(&[("junit.xml", JUNIT)]);
    let dir = tree(&[]);
    std::os::unix::fs::symlink(
        outside.path().join("junit.xml"),
        dir.path().join("junit.xml"),
    )
    .unwrap();
    assert_eq!(
        summarize(&[found(dir.path(), "junit.xml")], &Limits::default()),
        None
    );
    let q = query(dir.path(), dir.path(), &[]);
    assert_eq!(rels(find(&q)), Vec::<String>::new());
    // Nor a directory that is a link.
    std::os::unix::fs::symlink(outside.path(), dir.path().join("test-results")).unwrap();
    assert_eq!(rels(find(&q)), Vec::<String>::new());
}

fn query<'a>(root: &'a Path, cwd: &'a Path, command: &'a [String]) -> Query<'a> {
    Query {
        root,
        cwd,
        started: SystemTime::now() - Duration::from_secs(60),
        command,
        declared: &[],
        readers: &[],
    }
}

fn rels(found: Option<Search>) -> Vec<String> {
    found.unwrap().files.into_iter().map(|f| f.rel).collect()
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

// covers: C-27
#[test]
fn the_default_places_are_found_from_the_command_and_the_root() {
    let dir = tree(&[
        ("junit.xml", JUNIT),
        ("pkg/coverage/lcov.info", LCOV),
        ("pkg/target/surefire-reports/TEST-a.xml", JUNIT),
        ("deep/a/b/c/d/TEST-x.xml", JUNIT),
        ("deeper/a/b/c/d/e/f/TEST-x.xml", JUNIT),
        ("node_modules/x/junit.xml", JUNIT),
        ("node_modules/a/TEST-x.xml", JUNIT),
        (".tox/py/TEST-x.xml", JUNIT),
        ("notes/report.md", "no"),
    ]);
    let pkg = dir.path().join("pkg");
    let q = query(dir.path(), &pkg, &[]);
    assert_eq!(
        rels(find(&q)),
        [
            "deep/a/b/c/d/TEST-x.xml",
            "junit.xml",
            "pkg/coverage/lcov.info",
            "pkg/target/surefire-reports/TEST-a.xml",
        ]
    );
}

// covers: C-27
#[test]
fn testngs_copies_of_surefires_report_are_found_once() {
    let dir = tree(&[
        ("target/surefire-reports/TEST-TestSuite.xml", JUNIT),
        ("target/surefire-reports/junitreports/TEST-ng.A.xml", JUNIT),
        ("target/surefire-reports/junitreports/TEST-ng.B.xml", JUNIT),
        // A directory of that name with no suite report beside it is read.
        ("other/junitreports/TEST-x.xml", JUNIT),
    ]);
    let q = query(dir.path(), dir.path(), &[]);
    assert_eq!(
        rels(find(&q)),
        [
            "other/junitreports/TEST-x.xml",
            "target/surefire-reports/TEST-TestSuite.xml",
        ]
    );
}

// covers: C-27
#[test]
fn a_report_older_than_the_run_is_not_its() {
    let dir = tree(&[("junit.xml", JUNIT)]);
    let mut q = query(dir.path(), dir.path(), &[]);
    q.started = SystemTime::now() + Duration::from_secs(60);
    assert_eq!(rels(find(&q)), Vec::<String>::new());
    // A clock two seconds off still counts.
    q.started = SystemTime::now() + Duration::from_secs(1);
    assert_eq!(rels(find(&q)), ["junit.xml"]);
}

// covers: C-27
#[test]
fn the_command_line_names_its_reports() {
    // Each case in a tree of its own, whose files no default place names.
    let case = |files: &[(&str, &str)], line: &str| {
        let dir = tree(files);
        let pkg = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        let command = words(line);
        let q = query(dir.path(), &pkg, &command);
        rels(find(&q))
    };
    assert_eq!(
        case(&[("out/r.xml", JUNIT)], "pytest --junitxml=../out/r.xml"),
        ["out/r.xml"]
    );
    assert_eq!(
        case(&[("pkg/out/r.xml", JUNIT)], "pytest --junit-xml out/r.xml"),
        ["pkg/out/r.xml"]
    );
    assert_eq!(
        case(
            &[("pkg/out/cov.xml", "<coverage/>")],
            "pytest --cov=src --cov-report=xml:out/cov.xml"
        ),
        ["pkg/out/cov.xml"]
    );
    assert_eq!(
        case(
            &[("pkg/c.prof", "mode: set\n")],
            "go test -coverprofile c.prof ./..."
        ),
        ["pkg/c.prof"]
    );
    assert_eq!(
        case(
            &[("pkg/c.prof", "mode: set\n")],
            "go test -coverprofile=c.prof ./..."
        ),
        ["pkg/c.prof"]
    );
    assert_eq!(
        case(
            &[("pkg/cov/lcov.info", LCOV)],
            "jest --coverageDirectory cov"
        ),
        ["pkg/cov/lcov.info"]
    );
    assert_eq!(
        case(
            &[("pkg/cov/lcov.info", LCOV)],
            "vitest run --coverage.reportsDirectory=cov"
        ),
        ["pkg/cov/lcov.info"]
    );
    assert_eq!(
        case(
            &[("pkg/out/v.xml", JUNIT)],
            "vitest run --outputFile.junit=out/v.xml"
        ),
        ["pkg/out/v.xml"]
    );
    assert_eq!(
        case(
            &[("pkg/out/v.xml", JUNIT)],
            "vitest run --reporter=junit --outputFile out/v.xml"
        ),
        ["pkg/out/v.xml"]
    );
    // --outputFile with no JUnit reporter names nothing.
    assert_eq!(
        case(
            &[("pkg/out/v.xml", JUNIT)],
            "vitest run --outputFile out/v.xml"
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        case(&[("pkg/out/p.xml", JUNIT)], "phpunit --log-junit out/p.xml"),
        ["pkg/out/p.xml"]
    );
    assert_eq!(
        case(
            &[("pkg/out/c.xml", "<coverage/>")],
            "phpunit --coverage-clover=out/c.xml"
        ),
        ["pkg/out/c.xml"]
    );
    assert_eq!(
        case(
            &[("pkg/results/run.trx", "<TestRun/>")],
            "dotnet test --results-directory results"
        ),
        ["pkg/results/run.trx"]
    );
    assert_eq!(
        case(
            &[("pkg/out/t.trx", "<TestRun/>")],
            "dotnet test --results-directory out --logger trx;LogFileName=t.trx"
        ),
        ["pkg/out/t.trx"]
    );
    assert_eq!(
        case(
            &[("pkg/out/spec.xml", JUNIT)],
            "bundle exec rspec --format RspecJunitFormatter --out out/spec.xml"
        ),
        ["pkg/out/spec.xml"]
    );
    // --out after a formatter that only prints names nothing.
    assert_eq!(
        case(
            &[("pkg/out/spec.xml", JUNIT)],
            "bundle exec rspec --format progress --out out/spec.xml"
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        case(
            &[("pkg/llvm.out", LCOV)],
            "cargo llvm-cov --lcov --output-path llvm.out"
        ),
        ["pkg/llvm.out"]
    );
    assert_eq!(
        case(
            &[("pkg/llvm.out", LCOV)],
            "cargo llvm-cov --output-path llvm.out"
        ),
        Vec::<String>::new()
    );
    let nextest = [
        ("pkg/target/nextest/ci/out.xml", JUNIT),
        (
            "pkg/.config/nextest.toml",
            "[profile.ci.junit]\npath = \"out.xml\"\n",
        ),
    ];
    assert_eq!(
        case(&nextest, "cargo nextest run --profile ci"),
        ["pkg/target/nextest/ci/out.xml"]
    );
    assert_eq!(case(&nextest, "cargo nextest run"), Vec::<String>::new());
}

// covers: C-27
#[test]
fn hushrun_yaml_and_the_readers_name_reports_too() {
    let dir = tree(&[
        ("pkg/out/custom.xml", JUNIT),
        ("artifacts/tests/a.xml", JUNIT),
        ("upload/cov.info", LCOV),
        ("results/run-3.xml", JUNIT),
    ]);
    let pkg = dir.path().join("pkg");
    let declared = ["out/*.xml".to_string()];
    let readers = [
        "./artifacts/tests/".to_string(),
        // What CI fills in: the workspace is the root, and anything else a wildcard.
        "${{ github.workspace }}/upload/cov.info".to_string(),
        "results/run-${{ matrix.n }}.xml".to_string(),
        "!dist/**".to_string(),
    ];
    let q = Query {
        declared: &declared,
        readers: &readers,
        ..query(dir.path(), &pkg, &[])
    };
    assert_eq!(
        rels(find(&q)),
        [
            "artifacts/tests/a.xml",
            "pkg/out/custom.xml",
            "results/run-3.xml",
            "upload/cov.info"
        ]
    );
    // Nothing outside the repository is looked at.
    let out = [
        "../../elsewhere/*.xml".to_string(),
        "/etc/passwd".to_string(),
    ];
    let q = Query {
        declared: &out,
        ..query(dir.path(), &pkg, &[])
    };
    assert_eq!(rels(find(&q)), Vec::<String>::new());
}

proptest! {
    // covers: C-27
    #[test]
    fn a_record_stays_within_its_bound(
        paths in proptest::collection::vec("[a-z\"\\\\/\\x01é]{1,200}", 0..40),
        total in 0u64..2_000_000_000,
    ) {
        let read: Vec<(Found, Format, Read)> = paths
            .iter()
            .map(|p| {
                let found = Found { path: PathBuf::from(p), rel: p.clone() };
                (found, Format::Junit, Read::Tests(Tests { total, ..Tests::default() }))
            })
            .collect();
        let refs: Vec<(&Found, Format, Read)> = read.iter().map(|(f, fmt, r)| (f, *fmt, r.clone())).collect();
        let mut reports = reports_of(&refs);
        prop_assert!(serde_json::to_vec(&reports).unwrap().len() <= MOST_BYTES);
        prop_assert!(reports.files.len() <= MOST_FILES);
        prop_assert!(reports.files.iter().all(|f| f.path.len() <= LONGEST_PATH));
        // As `record` leaves it: complete when nothing was left out, still within the bound.
        reports.files_complete = reports.files_left_out == 0;
        fit(&mut reports);
        prop_assert!(serde_json::to_vec(&reports).unwrap().len() <= MOST_BYTES);
        prop_assert!(!reports.files_complete || reports.files_left_out == 0);
    }

    // covers: C-27
    #[test]
    fn any_bytes_read_without_a_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = read(&bytes);
        let _ = sniff(&bytes);
    }
}

// covers: C-27 (a fuzz run found the first: two blocks whose statements don't fit a u64 together)
#[test]
fn counts_too_big_to_add_read_without_a_panic() {
    let most = u64::MAX;
    let go = format!("mode: set\na.go:1.1,2.2 {most} 1\nb.go:1.1,2.2 {most} 0\n");
    let Ok((Format::Go, Read::Coverage(counts))) = read(go.as_bytes()) else {
        panic!("a coverprofile");
    };
    let statements = counts.statements.expect("statements");
    assert_eq!((statements.covered, statements.total), (most, most));

    // Cobertura with no totals on its root: each line's branches are added up.
    let cobertura = format!(
        r#"<coverage line-rate="1"><packages><package><classes><class filename="a.py"><lines>
<line number="1" hits="1" branch="true" condition-coverage="100% ({most}/{most})"/>
<line number="2" hits="1" branch="true" condition-coverage="100% ({most}/{most})"/>
</lines></class></classes></package></packages></coverage>"#
    );
    let Ok((Format::Cobertura, Read::Coverage(counts))) = read(cobertura.as_bytes()) else {
        panic!("a Cobertura report");
    };
    let branches = counts.branches.expect("branches");
    assert_eq!((branches.covered, branches.total), (most, most));
}

// covers: C-27
#[test]
fn a_reports_record_from_a_newer_hushrun_reads() {
    let json = r#"{"files":[{"path":"a.x","kind":"profiles","format":"pprof"}],"filesLeftOut":2,
        "tests":{"total":3,"failed":1,"errors":0,"skipped":0,"flaky":1},
        "coverage":{"format":"kover","functions":{"covered":1,"total":2}},"mutants":{}}"#;
    let reports: Reports = serde_json::from_str(json).unwrap();
    assert_eq!(reports.files[0].format, "pprof");
    assert_eq!(reports.files_left_out, 2);
    assert_eq!(reports.tests.unwrap().failed, 1);
    assert_eq!(reports.coverage.unwrap().lines, None);
}

// covers: C-27
#[test]
fn counts_past_what_a_record_holds_are_no_summary() {
    let file = Found {
        path: PathBuf::from("r.xml"),
        rel: "r.xml".into(),
    };
    let tests = |total, duration_ms| {
        Read::Tests(Tests {
            total,
            duration_ms: Some(duration_ms),
            ..Tests::default()
        })
    };
    let coverage = |total| {
        Read::Coverage(Counts {
            lines: Ratio::some(1, total),
            ..Counts::default()
        })
    };
    let reports = |read: Vec<(Format, Read)>| {
        let read: Vec<(&Found, Format, Read)> =
            read.into_iter().map(|(f, r)| (&file, f, r)).collect();
        reports_of(&read)
    };
    // Two files' tests together past the bound: none.
    let past = reports(vec![
        (Format::Junit, tests(MOST_COUNT, 1)),
        (Format::Junit, tests(1, 1)),
        (Format::Lcov, coverage(10)),
    ]);
    assert_eq!(past.tests, None);
    assert!(past.coverage.is_some());
    // A duration past the bound is unknown, and the counts stay.
    let long = reports(vec![
        (Format::Junit, tests(3, MOST_DURATION_MS)),
        (Format::Junit, tests(3, 1)),
    ]);
    assert_eq!(long.tests.unwrap().duration_ms, None);
    // Coverage past the bound: none, though the file is still named.
    let over = reports(vec![(Format::Lcov, coverage(MOST_COUNT + 1))]);
    assert_eq!((over.tests, over.coverage), (None, None));
    assert_eq!(over.files.len(), 1);
    assert_eq!(
        reports(vec![(Format::Lcov, coverage(MOST_COUNT))])
            .coverage
            .unwrap()
            .lines
            .unwrap()
            .total,
        MOST_COUNT
    );
}

// covers: C-26, C-27
#[test]
fn a_record_says_when_its_files_are_all_the_run_wrote() {
    let record_of = |files: &[(&str, &str)], readers: &[String]| {
        let dir = tree(files);
        let q = Query {
            readers,
            ..query(dir.path(), dir.path(), &[])
        };
        let search = find(&q).unwrap();
        record(&search, &Limits::default())
    };
    let json = |reports: Option<Reports>| serde_json::to_value(reports).unwrap();
    // Nothing found: the list is complete, and that is all.
    assert_eq!(
        json(record_of(&[], &[])),
        serde_json::json!({"filesComplete": true})
    );
    // Every file found was read.
    let both = record_of(&[("junit.xml", JUNIT), ("lcov.info", LCOV)], &[]).unwrap();
    assert!(both.files_complete);
    assert_eq!(both.files.len(), 2);
    // A file that isn't a report hushrun reads, or a broken one, leaves it incomplete.
    let unread = record_of(&[("junit.xml", JUNIT), ("coverage.xml", "<html/>")], &[]).unwrap();
    assert!(!unread.files_complete);
    assert_eq!(unread.files.len(), 1);
    assert_eq!(record_of(&[("junit.xml", "<testsuite")], &[]), None);
    // A file read that counts nothing is still named, so a reader of it counts.
    let empty = record_of(&[("lcov.info", "TN:\n")], &[]).unwrap();
    assert_eq!(
        (empty.tests.as_ref(), empty.coverage.as_ref()),
        (None, None)
    );
    assert_eq!(empty.files[0].path, "lcov.info");
    assert!(empty.files_complete);
    // Seventeen files: one left out, so not complete.
    let many: Vec<(String, &str)> = (0..17)
        .map(|i| (format!("test-results/TEST-{i:02}.xml"), JUNIT))
        .collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(p, t)| (p.as_str(), *t)).collect();
    let many = record_of(&many, &[]).unwrap();
    assert_eq!(many.files_left_out, 1);
    assert!(!many.files_complete);
}

// covers: C-26, C-27
#[test]
fn a_search_says_when_it_stopped_short_of_a_readers_files() {
    let cut_short = |files: &[(&str, &str)], readers: &[&str]| {
        let dir = tree(files);
        let readers: Vec<String> = readers.iter().map(|r| r.to_string()).collect();
        let q = Query {
            readers: &readers,
            ..query(dir.path(), dir.path(), &[])
        };
        find(&q).unwrap().cut_short
    };
    let deep = "a/b/c/d/e/f/g/h/TEST-x.xml";
    // `**` goes six levels down: below that, a reader's file may be missed.
    assert!(cut_short(&[(deep, JUNIT)], &["**/TEST-*.xml"]));
    assert!(cut_short(
        &[("coverage/a/b/c/d/e/f/g/x.info", LCOV)],
        &["coverage/"]
    ));
    // Within reach, or deep only where no reader reads, it isn't.
    assert!(!cut_short(&[("a/b/TEST-x.xml", JUNIT)], &["**/TEST-*.xml"]));
    assert!(!cut_short(&[(deep, JUNIT)], &["coverage.xml"]));
    assert!(!cut_short(&[(deep, JUNIT)], &[]));
    // A link a reader reads through is out of sight, file or directory.
    #[cfg(unix)]
    {
        let dir = tree(&[("build/junit.xml", JUNIT)]);
        std::os::unix::fs::symlink("build/junit.xml", dir.path().join("junit.xml")).unwrap();
        std::os::unix::fs::symlink("build", dir.path().join("results")).unwrap();
        let search = |readers: &[&str]| {
            let readers: Vec<String> = readers.iter().map(|r| r.to_string()).collect();
            let q = Query {
                readers: &readers,
                ..query(dir.path(), dir.path(), &[])
            };
            find(&q).unwrap().cut_short
        };
        assert!(search(&["junit.xml"]));
        assert!(search(&["results/"]));
        assert!(search(&["**/TEST-*.xml"]));
        assert!(!search(&["coverage.xml"]));
        assert!(!search(&[]));
        // A link to a file no reader's pattern matches hides nothing, wherever `**` goes.
        let dir = tree(&[("AGENTS.md", "notes")]);
        std::os::unix::fs::symlink("AGENTS.md", dir.path().join("CLAUDE.md")).unwrap();
        let readers = ["**/TEST-*.xml".to_string()];
        let q = Query {
            readers: &readers,
            ..query(dir.path(), dir.path(), &[])
        };
        assert!(!find(&q).unwrap().cut_short);
    }
    // A hidden directory isn't where `**` reads.
    assert!(!cut_short(
        &[(".cache/a/b/c/d/e/f/g/TEST-x.xml", JUNIT)],
        &["**/TEST-*.xml"]
    ));
    // Nor is a directory the search never enters: but a reader that names one by name reads
    // where the search didn't go.
    for (file, reader) in [
        ("node_modules/r/junit.xml", "node_modules/r/junit.xml"),
        (".venv/reports/junit.xml", ".venv/reports/"),
        ("web/node_modules/.cache/junit.xml", "web/node_modules"),
    ] {
        assert!(cut_short(&[(file, JUNIT)], &[reader]), "{reader}");
        assert!(!cut_short(&[(file, JUNIT)], &["**/junit.xml"]), "{file}");
    }
    assert!(!cut_short(
        &[("node_modules/r/junit.xml", JUNIT)],
        &["*/r/junit.xml"]
    ));
    // A record of such a search isn't complete.
    let dir = tree(&[(deep, JUNIT), ("junit.xml", JUNIT)]);
    let readers = ["**/TEST-*.xml".to_string()];
    let q = Query {
        readers: &readers,
        ..query(dir.path(), dir.path(), &[])
    };
    let reports = record(&find(&q).unwrap(), &Limits::default()).unwrap();
    assert!(!reports.files_complete);
    assert_eq!(reports.tests.unwrap().total, 2);
}
