//! \[ADR 0068\] `tests: observe` end to end (C-5, C-30 to C-33, C-36, C-37): what only the CLI
//! does around pytest's test files. Every other test of `observe` is in-process, in `units.rs`.

#[path = "../common/observe.rs"]
mod fixtures;

use super::cli::Cli as _;
use super::python::{PYTEST_CLEARED, has_pytest, python, tools};
use super::repo::Repo;
use super::store;

use fixtures::*;

/// A repository of `files`, committed, whose COMMAND never inherits pytest's options, a
/// `PYTHONPATH` or Cargo's target directory.
fn repo_of(files: &[(&str, &str)]) -> Repo {
    let owned: Vec<(String, String)> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
    Repo::new(&owned).clear(PYTEST_CLEARED)
}

/// `hushrun run unit -- python -m pytest -q -p no:cacheprovider`.
fn pytest_args(python: &str) -> Vec<&str> {
    vec![
        "run",
        "unit",
        "--",
        python,
        "-m",
        "pytest",
        "-q",
        "-p",
        "no:cacheprovider",
    ]
}

/// The run directories a stopped or finished run left behind.
fn run_dirs(r: &Repo) -> usize {
    std::fs::read_dir(r.cache.join("units/runs")).map_or(0, |d| d.count())
}

// covers: C-5, C-32
#[cfg(unix)]
#[test]
fn with_tests_off_command_gets_nothing_new() {
    let print_env = ["run", "unit", "--", "sh", "-c", "env | sort"];
    for yaml in [
        None,
        Some("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    run: x\n"),
        Some("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: off\n"),
    ] {
        let mut files = vec![("src/a.txt", "a\n")];
        if let Some(yaml) = yaml {
            files.push(("hushrun.yaml", yaml));
        }
        let r = repo_of(&files);
        let ran = r.run(
            &print_env,
            &[("PYTHONPATH", "/kept"), ("PYTEST_ADDOPTS", "-q")],
        );
        assert_eq!(ran.code, 0, "{}", ran.stderr);
        let names: Vec<&str> = ran
            .stdout
            .lines()
            .filter_map(|l| l.split_once('=').map(|(n, _)| n))
            .collect();
        for line in ran.stdout.lines() {
            let runner = line.starts_with("CARGO_TARGET_") && line.contains("_RUNNER=");
            assert!(!line.starts_with("HUSHRUN_UNITS=") && !runner, "{line}");
            // \[ADR 0070, 0071\] Nor what loads the Go and Vitest adapters.
            assert!(
                !line.starts_with("HUSHRUN_VITEST=")
                    && !line.starts_with("HUSHRUN_GOFLAGS=")
                    && !line.starts_with("GOFLAGS="),
                "{line}"
            );
        }
        // What the environment said, as it said it.
        assert!(
            ran.stdout.contains("\nPYTHONPATH=/kept\n"),
            "{}",
            ran.stdout
        );
        assert!(
            ran.stdout.contains("\nPYTEST_ADDOPTS=-q\n"),
            "{}",
            ran.stdout
        );
        assert!(names.contains(&"HUSHRUN_INSIDE_RUN"));
        // No file of hushrun's for test files.
        assert!(!r.cache.join("units").exists());
        assert!(!r.cache.join("adapters").exists());
        assert!(!r.cache.join("results/units").exists());
        assert!(!ran.stderr.contains("test file"), "{}", ran.stderr);
    }
    // With `tests: observe`, what COMMAND gets is in-process: units.rs.
}

// covers: C-32
#[cfg(unix)]
#[test]
fn a_run_a_signal_stops_leaves_no_directory() {
    let yaml = "version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {mode: reuse}\n";
    let r = repo_of(&[("src/a.txt", "a\n"), ("hushrun.yaml", yaml)]);
    let ran = r.run(&["run", "unit", "--", "sh", "-c", "kill -TERM $$"], &[]);
    assert_eq!(ran.code, 128 + 15, "{}", ran.stderr);
    // \[ADR 0069\] Reuse is in this version: with no test file, it says nothing of them.
    assert!(!ran.stderr.contains("test file"), "{}", ran.stderr);
    assert_eq!(run_dirs(&r), 0);
}

// covers: C-36
#[test]
fn a_store_an_older_hushrun_kept_is_imported_once_and_left_as_it_was() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("lib.py", "X = 1\n"),
        ("extra.py", "Y = 1\n"),
        (
            "test_a.py",
            "import lib\n\ndef test_a():\n    assert lib.X == 1\n",
        ),
        ("test_b.py", "def test_b():\n    assert True\n"),
    ]);
    let python = python();
    let args = pytest_args(&python);
    let first = r.run(&args, &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    // What an older hushrun left: the same rows in `<id>.v1.sqlite`, of schema 1, and no store of
    // schema 2.
    let stores = store::stores(&r.cache);
    assert_eq!(stores.len(), 1);
    let v2 = stores[0].clone();
    let v1 = v2.with_file_name(
        v2.file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".v2.sqlite", ".v1.sqlite"),
    );
    {
        let conn = rusqlite::Connection::open(&v2).unwrap();
        conn.execute_batch(
            "DROP TABLE test_not_followed; DROP TABLE test; DROP TABLE test_list_dir;
             DROP TABLE test_list_read; DROP TABLE test_list; PRAGMA user_version = 1;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
    }
    std::fs::rename(&v2, &v1).unwrap();
    let v1_bytes = std::fs::read(&v1).unwrap();
    // A change no test file read: the check runs, and what reuse would have done reads the map
    // the older store held.
    r.write("extra.py", "Y = 2\n");
    let second = r.run(&args, &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        hushrun_testkit::lines::without_saving(second.line("hushrun: unit: with `tests: reuse`")),
        "hushrun: unit: with `tests: reuse`, 0 of 2 test files would have run; 2 already passed on the files they read"
    );
    // The older file stays as it was, for an older hushrun; this one's store is of schema 2.
    assert_eq!(std::fs::read(&v1).unwrap(), v1_bytes);
    assert!(v2.is_file());
    assert!(!second.stderr.contains("cache step"), "{}", second.stderr);
}

// covers: C-32, C-37
#[test]
fn hushrun_why_says_a_test_of_a_file_pytest_exit_cut_short_didnt_finish() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&cut_short());
    let python = python();
    let ran = r.run(&pytest_args(&python), &[]);
    assert_eq!(ran.code, 3, "{}\n{}", ran.stdout, ran.stderr);
    // \[C-37\] `hushrun why` says a test of it didn't finish, not that a start of its runner
    // didn't run it whole.
    let why = r.run(&["why", "unit", "test_k.py"], &[]);
    assert!(
        why.stdout.contains("test_k.py would run with `tests: reuse`: not every test of it finished the last time it ran"),
        "{}\n{}",
        why.stdout,
        why.stderr
    );
}

// covers: C-32, C-37
#[cfg(unix)]
#[test]
fn hushrun_why_says_a_start_of_the_runner_ended_before_it_said_what_it_saw() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    let r = repo_of(&runner_dies());
    let line = runner_dies_line(&python());
    let ran = r.run(&["run", "unit", "--", "sh", "-c", &line], &[]);
    assert_ne!(ran.code, 0, "{}\n{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains(
            "hushrun: unit: no test file or test records a pass: a start of its runner ended before it said what it saw"
        ),
        "{}",
        ran.stderr
    );
    // \[C-37\] `hushrun why` says what the run decided, not that it never ran here.
    let why = r.run(&["why", "unit", "test_x.py"], &[]);
    assert!(
        why.stdout
            .contains("a start of its runner ended before it said what it saw"),
        "{}\n{}",
        why.stdout,
        why.stderr
    );
}

// covers: C-32
#[cfg(unix)]
#[test]
fn a_command_pytest_no_longer_reports_for_is_chosen_afresh_next_time() {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return;
    }
    // `make test` ran pytest, and the store says so; then the Makefile runs ctest. The stored word
    // keeps CTest away only while pytest reports: a run in which it reported nothing clears it.
    let python = python();
    let makefile = format!("test:\n\t{python} -m pytest -q -p no:cacheprovider\n");
    let r = repo_of(&[
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        ("test_a.py", "def test_a():\n    pass\n"),
        ("Makefile", &makefile),
    ]);
    let first = r.run(&["run", "unit", "--", "make", "test"], &[]);
    assert_eq!(first.code, 0, "{}\n{}", first.stdout, first.stderr);
    let checks = || -> i64 {
        let store = store::stores(&r.cache);
        rusqlite::Connection::open(&store[0])
            .unwrap()
            .query_row("SELECT count(*) FROM checks", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(checks(), 1);
    let silent_runs = || -> i64 {
        let store = store::stores(&r.cache);
        rusqlite::Connection::open(&store[0])
            .unwrap()
            .query_row("SELECT silent_runs FROM checks", [], |row| row.get(0))
            .unwrap()
    };
    // A run that fails before pytest starts (no build directory) proves nothing of who reports:
    // the word stays.
    r.write("Makefile", "test:\n\tctest --test-dir build\n");
    let failed = r.run(&["run", "unit", "--", "make", "test"], &[]);
    assert_ne!(failed.code, 0, "{}\n{}", failed.stdout, failed.stderr);
    assert_eq!(checks(), 1, "{}", failed.stderr);
    // A ctest whose build directory is empty finds no test and exits 0: pytest said nothing this
    // time, and the word stays; the next run that reaches pytest is pytest's, as before.
    let tools = r.root.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let fake = tools.join("ctest");
    std::fs::write(&fake, "#!/bin/sh\necho 'No tests were found!!!'\nexit 0\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let empty = r.run(&["run", "unit", "--", "make", "test"], &[("PATH", &path)]);
    assert_eq!(empty.code, 0, "{}\n{}", empty.stdout, empty.stderr);
    assert_eq!(checks(), 1, "{}", empty.stderr);
    assert!(
        empty
            .stderr
            .contains("hushrun: unit: test files weren't observed: pytest reported none this time"),
        "{}",
        empty.stderr
    );
    assert!(!empty.stderr.contains("afresh"), "{}", empty.stderr);
    r.write(
        "Makefile",
        &format!("test:\n\tctest --test-dir build || {python} -m pytest -q -p no:cacheprovider\n"),
    );
    let pytest = r.run(&["run", "unit", "--", "make", "test"], &[]);
    assert_eq!(pytest.code, 0, "{}\n{}", pytest.stdout, pytest.stderr);
    assert_eq!((checks(), silent_runs()), (1, 0), "{}", pytest.stderr);
    // The Makefile now runs ctest, and passes, with pytest silent: twice in a row, and the word
    // goes on the second.
    r.write(
        "Makefile",
        "test:\n\tctest --test-dir build\n# now ctest's\n",
    );
    let once = r.run(&["run", "unit", "--", "make", "test"], &[("PATH", &path)]);
    assert_eq!(once.code, 0, "{}\n{}", once.stdout, once.stderr);
    assert_eq!((checks(), silent_runs()), (1, 1), "{}", once.stderr);
    r.write(
        "Makefile",
        "test:\n\tctest --test-dir build\n# now ctest's, again\n",
    );
    let twice = r.run(&["run", "unit", "--", "make", "test"], &[("PATH", &path)]);
    assert_eq!(twice.code, 0, "{}\n{}", twice.stdout, twice.stderr);
    assert_eq!(checks(), 0, "{}", twice.stderr);
    assert!(
        twice
            .stderr
            .contains("so the next run chooses how to observe them afresh"),
        "{}",
        twice.stderr
    );
}

/// The first 10 characters of a ULID: its milliseconds.
fn ulid_time(ms: i64) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    (0..10)
        .rev()
        .map(|i| ALPHABET[((ms >> (5 * i)) & 31) as usize] as char)
        .collect()
}

// covers: C-33 (ADR 0067: unit results are swept as this machine's results are)
#[test]
fn this_machine_forgets_unit_results_after_30_days() {
    let r = repo_of(&[("src/a.txt", "a\n")]);
    let old = ulid_time(
        "2026-01-01T00:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond(),
    );
    let results = r.cache.join("results");
    let entry = results.join(format!(
        "units/v0/{}/{old}0000000000000000.json",
        "a".repeat(64)
    ));
    let run = results.join(format!(
        "unit-runs/v0/{}/{old}0000000000000000.json",
        "b".repeat(64)
    ));
    let map = r.cache.join("units/stale.map.json");
    let left = r.cache.join("units/runs/01OLD");
    for file in [&entry, &run, &map] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "{}").unwrap();
    }
    std::fs::create_dir_all(&left).unwrap();
    // The files' own times are now: the map and a stopped run's directory go by them, so they stay.
    let ran = r.run(
        &[
            "run",
            "--now",
            "2026-02-01T00:00:00Z",
            "unit",
            "--",
            "git",
            "--version",
        ],
        &[],
    );
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(
        !entry.exists() && !run.exists(),
        "unit results older than 30 days go"
    );
    assert!(map.exists() && left.exists());
}
