use super::*;

use hushrun_core::names::{CheckName, ScopeName};
use hushrun_core::time::Time;
use hushrun_core::units::Tree;

const HASH_A: &str = "f 1111111111111111111111111111111111111111111111111111111111111111";
const HASH_B: &str = "f 2222222222222222222222222222222222222222222222222222222222222222";

fn base(check: &str) -> KeyBase {
    KeyBase {
        check: CheckName::new(check).unwrap(),
        command: vec!["pytest".into(), "-q".into()],
        cwd: String::new(),
        scope: ScopeName::new("default").unwrap(),
        external: None,
        counts_from: None,
    }
}

/// A tree of these files, each with a hash of its own.
fn tree(paths: &[&str]) -> Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut sorted: Vec<&str> = paths.to_vec();
    sorted.sort();
    for (n, path) in sorted.iter().enumerate() {
        bytes.extend_from_slice(format!("f {:064x} {path}", n + 1).as_bytes());
        bytes.push(0);
    }
    Tree::from_manifest(&bytes).unwrap()
}

fn mapped(reads: &[(&str, &str)]) -> Mapped {
    Mapped {
        reads: reads
            .iter()
            .map(|(p, e)| (p.to_string(), e.to_string()))
            .collect(),
        ..Mapped::default()
    }
}

struct Run<'a> {
    /// Every unit the run's adapter told, run, reused or not run.
    known: BTreeSet<String>,
    check: &'a str,
    seen: BTreeMap<String, Mapped>,
    tree: Tree,
    edges: BTreeSet<Edge>,
    disagreements: Vec<Disagreement>,
    now_ms: i64,
}

fn run<'a>(check: &'a str, seen: &[(&str, Mapped)], tree_paths: &[&str]) -> Run<'a> {
    Run {
        known: seen.iter().map(|(u, _)| u.to_string()).collect(),
        check,
        seen: seen
            .iter()
            .map(|(u, m)| (u.to_string(), m.clone()))
            .collect(),
        tree: tree(tree_paths),
        edges: BTreeSet::new(),
        disagreements: Vec::new(),
        now_ms: 1_800_000_000_000,
    }
}

fn id(check: &str) -> Digest {
    super::super::store::check_id(&base(check), Some("example.com/acme/shop"))
}

fn write_run(path: &Path, r: &Run<'_>) -> Result<(), Unread> {
    let rest = BTreeMap::from([("uv.lock".to_string(), HASH_A.to_string())]);
    let sources = BTreeSet::from(["app/a.py".to_string(), "app/b.py".to_string()]);
    let b = base(r.check);
    let listed = BTreeSet::from(["tests/test_a.py".to_string()]);
    write(
        path,
        &Write {
            check_id: id(r.check),
            base: &b,
            adapter: "pytest",
            seen: &r.seen,
            tree: &r.tree,
            rest: &rest,
            sources: &sources,
            unit_files: Some(&listed),
            known: &r.known,
            whole_ms: None,
            ran_nothing: None,
            edges: &r.edges,
            disagreements: &r.disagreements,
            hints: &[],
            now_ms: r.now_ms,
        },
    )
}

// covers: C-36, C-33
#[test]
fn a_run_writes_its_map_and_the_next_reads_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("units/store.v1.sqlite");
    // Nothing yet: an empty map, no store made by reading, and nothing to say.
    let none = load(&path, &id("unit"));
    assert!(!none.map.reported() && none.unread.is_none());
    assert!(!path.exists(), "reading made a store");

    let mut a = mapped(&[("tests/test_a.py", HASH_A), ("app/a.py", HASH_B)]);
    a.not_followed.insert(NotFollowed {
        why: "subprocess".into(),
        detail: "git".into(),
    });
    a.dirs.insert("testdata".into());
    let mut r = run(
        "unit",
        &[("tests/test_a.py", a.clone())],
        &["tests/test_a.py", "app/a.py"],
    );
    r.edges.insert((
        "tests/test_a.py".into(),
        "app/a.py".into(),
        EdgeKind::Import,
    ));
    r.disagreements.push(Disagreement {
        unit: "tests/test_a.py".into(),
        key: Digest::of(b"k"),
        run_key: Digest::of(b"r"),
        at: Time::parse("2026-11-11T12:00:00Z").unwrap(),
    });
    write_run(&path, &r).unwrap();
    let loaded = load(&path, &id("unit"));
    assert!(loaded.unread.is_none());
    let map = loaded.map;
    assert!(map.reported());
    assert_eq!(map.units["tests/test_a.py"], a);
    assert_eq!(map.rest["uv.lock"], HASH_A);
    assert_eq!(map.sources.len(), 2);
    assert!(map.unit_files.contains("tests/test_a.py"));
    assert_eq!(edges(&path, &id("unit")).unwrap(), r.edges);
    assert_eq!(disagreements(&path, &id("unit")).unwrap(), r.disagreements);
    // Another check of the repository is another map.
    assert!(!load(&path, &id("lint")).map.reported());
    let named = checks_named(&path, "unit").unwrap();
    assert_eq!(named.len(), 1);
    assert_eq!(named[0].command, ["pytest", "-q"]);
}

// covers: C-36, C-33
#[test]
fn a_run_replaces_what_it_saw_and_keeps_the_rest_while_its_file_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v1.sqlite");
    let all = ["tests/test_a.py", "tests/test_b.py", "tests/test_c.py"];
    let first = run(
        "unit",
        &[
            ("tests/test_a.py", mapped(&[("app/a.py", HASH_A)])),
            ("tests/test_b.py", mapped(&[("app/b.py", HASH_A)])),
            ("tests/test_c.py", mapped(&[])),
        ],
        &all,
    );
    write_run(&path, &first).unwrap();
    // test_a ran again on a new app/a.py; test_b was reused; test_c is gone.
    let mut second = run(
        "unit",
        &[("tests/test_a.py", mapped(&[("app/a.py", HASH_B)]))],
        &["tests/test_a.py", "tests/test_b.py"],
    );
    second.now_ms += 1000;
    write_run(&path, &second).unwrap();
    let map = load(&path, &id("unit")).map;
    assert_eq!(
        map.units.keys().map(String::as_str).collect::<Vec<_>>(),
        ["tests/test_a.py", "tests/test_b.py"]
    );
    assert_eq!(map.units["tests/test_a.py"].reads["app/a.py"], HASH_B);
    assert_eq!(map.units["tests/test_b.py"].reads["app/b.py"], HASH_A);
}

// covers: C-36
#[test]
fn edges_from_what_ran_are_this_runs_and_the_others_stay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v1.sqlite");
    let files = [
        "tests/test_a.py",
        "tests/test_b.py",
        "app/a.py",
        "app/b.py",
        "app/c.py",
    ];
    let mut first = run(
        "unit",
        &[
            ("tests/test_a.py", mapped(&[("app/a.py", HASH_A)])),
            ("tests/test_b.py", mapped(&[("app/b.py", HASH_A)])),
        ],
        &files,
    );
    first.edges = BTreeSet::from([
        (
            "tests/test_a.py".into(),
            "app/a.py".into(),
            EdgeKind::Import,
        ),
        ("app/a.py".into(), "app/c.py".into(), EdgeKind::Import),
        (
            "tests/test_b.py".into(),
            "app/b.py".into(),
            EdgeKind::Import,
        ),
    ]);
    write_run(&path, &first).unwrap();
    // test_a ran again, and app/a.py no longer imports app/c.py; test_b didn't run.
    let mut second = run(
        "unit",
        &[("tests/test_a.py", mapped(&[("app/a.py", HASH_B)]))],
        &files,
    );
    second.edges = BTreeSet::from([(
        "tests/test_a.py".into(),
        "app/a.py".into(),
        EdgeKind::Import,
    )]);
    write_run(&path, &second).unwrap();
    assert_eq!(
        edges(&path, &id("unit")).unwrap(),
        BTreeSet::from([
            (
                "tests/test_a.py".into(),
                "app/a.py".into(),
                EdgeKind::Import
            ),
            (
                "tests/test_b.py".into(),
                "app/b.py".into(),
                EdgeKind::Import
            ),
        ])
    );
}

// covers: C-36
#[test]
fn hushrun_processes_writing_at_once_keep_each_others_checks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v1.sqlite");
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let path = path.clone();
            std::thread::spawn(move || {
                let check = format!("check{n}");
                let unit = format!("tests/test_{n}.py");
                let r = Run {
                    known: BTreeSet::from([unit.clone()]),
                    check: Box::leak(check.into_boxed_str()),
                    seen: BTreeMap::from([(unit.clone(), mapped(&[("app/a.py", HASH_A)]))]),
                    tree: tree(&[&unit, "app/a.py"]),
                    edges: BTreeSet::new(),
                    disagreements: Vec::new(),
                    now_ms: 1_800_000_000_000,
                };
                write_run(&path, &r).unwrap();
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    for n in 0..8 {
        let map = load(&path, &id(&format!("check{n}"))).map;
        assert!(
            map.units.contains_key(&format!("tests/test_{n}.py")),
            "check{n}"
        );
    }
}

// covers: C-36
#[test]
fn a_store_that_cant_be_read_reads_as_no_map_and_the_next_write_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v1.sqlite");
    let r = run(
        "unit",
        &[("tests/test_a.py", mapped(&[]))],
        &["tests/test_a.py"],
    );
    // Not a database at all.
    std::fs::write(&path, b"this is not SQLite, it is a partial download").unwrap();
    let loaded = load(&path, &id("unit"));
    assert!(!loaded.map.reported());
    assert!(
        matches!(&loaded.unread, Some(Unread::Broken(_))),
        "{:?}",
        loaded.unread
    );
    assert!(loaded.unread.unwrap().why().contains("can't be read"));
    assert!(checks_named(&path, "unit").is_err());
    write_run(&path, &r).unwrap();
    assert!(load(&path, &id("unit")).map.reported());

    // A database of another schema, or no store of hushrun's.
    for (app, version) in [(APPLICATION_ID, SCHEMA + 1), (0, 1), (7, 0)] {
        remove(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE other (x); PRAGMA application_id = {app}; PRAGMA user_version = {version};"
        ))
        .unwrap();
        drop(conn);
        let loaded = load(&path, &id("unit"));
        assert!(
            !loaded.map.reported() && loaded.unread.is_some(),
            "{app} {version}"
        );
        write_run(&path, &r).unwrap();
        assert!(load(&path, &id("unit")).map.reported(), "{app} {version}");
    }

    // A store whose pages were damaged.
    let bytes = std::fs::read(&path).unwrap();
    let mut damaged = bytes.clone();
    for byte in damaged.iter_mut().skip(100).take(4000) {
        *byte = 0xa5;
    }
    remove(&path);
    std::fs::write(&path, &damaged).unwrap();
    let loaded = load(&path, &id("unit"));
    assert!(loaded.unread.is_some() && !loaded.map.reported());
    write_run(&path, &r).unwrap();
    assert!(load(&path, &id("unit")).map.reported());
}

// covers: C-36
#[test]
fn a_store_another_hushrun_holds_is_busy_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v1.sqlite");
    let r = run(
        "unit",
        &[("tests/test_a.py", mapped(&[]))],
        &["tests/test_a.py"],
    );
    write_run(&path, &r).unwrap();
    let mut holder = Connection::open(&path).unwrap();
    let held = holder
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let b = base("lint");
    let w = Write {
        check_id: id("lint"),
        base: &b,
        adapter: "pytest",
        seen: &r.seen,
        tree: &r.tree,
        rest: &BTreeMap::new(),
        sources: &BTreeSet::new(),
        unit_files: None,
        known: &r.known,
        whole_ms: None,
        ran_nothing: None,
        edges: &BTreeSet::new(),
        disagreements: &[],
        hints: &[],
        now_ms: r.now_ms,
    };
    assert_eq!(
        write_waiting(&path, &w, Duration::from_millis(50)),
        Err(Unread::Busy)
    );
    // A reader isn't held up by a writer, in WAL.
    assert!(load(&path, &id("unit")).map.reported());
    drop(held);
    write_waiting(&path, &w, Duration::from_millis(50)).unwrap();
    assert!(load(&path, &id("lint")).map.reported());
}

// covers: C-36, C-11
#[test]
fn a_check_no_run_wrote_for_30_days_is_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v1.sqlite");
    let old = run(
        "old",
        &[("tests/test_a.py", mapped(&[("app/a.py", HASH_A)]))],
        &["tests/test_a.py", "app/a.py"],
    );
    write_run(&path, &old).unwrap();
    let mut new = run(
        "new",
        &[("tests/test_a.py", mapped(&[]))],
        &["tests/test_a.py"],
    );
    new.now_ms = old.now_ms + 29 * 86_400_000;
    write_run(&path, &new).unwrap();
    assert!(load(&path, &id("old")).map.reported(), "29 days");
    new.now_ms = old.now_ms + 31 * 86_400_000;
    write_run(&path, &new).unwrap();
    assert!(!load(&path, &id("old")).map.reported(), "31 days");
    // What only it needed went with it.
    let conn = Connection::open(&path).unwrap();
    let versions: i64 = conn
        .query_row(
            "SELECT count(*) FROM version v JOIN path p ON p.id = v.path_ref WHERE p.path = 'app/a.py'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(versions, 0);
}

// covers: C-36, ADR 0061
#[test]
fn reading_a_store_leaves_no_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v1.sqlite");
    let r = run(
        "unit",
        &[("tests/test_a.py", mapped(&[]))],
        &["tests/test_a.py"],
    );
    write_run(&path, &r).unwrap();
    let listing = || {
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = listing();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert!(load(&path, &id("unit")).map.reported());
    checks_named(&path, "unit").unwrap();
    edges(&path, &id("unit")).unwrap();
    assert_eq!(listing(), before);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
}

// covers: C-36
#[test]
fn impossible_rows_cant_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v1.sqlite");
    let r = run(
        "unit",
        &[("tests/test_a.py", mapped(&[]))],
        &["tests/test_a.py"],
    );
    write_run(&path, &r).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    conn.execute_batch("INSERT INTO path (path) VALUES ('other.py');")
        .unwrap();
    // What may be written: a shared row with its system.
    conn.execute_batch(
        "INSERT INTO unit (check_ref, path_ref, ran_at, origin, shared_os)
         VALUES (1, (SELECT id FROM path WHERE path = 'other.py'), 0, 'shared', 'linux');
         DELETE FROM unit WHERE path_ref = (SELECT id FROM path WHERE path = 'other.py');",
    )
    .unwrap();
    for bad in [
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES ('x', 'a', '[]', '', 'default', 'pytest', 0)",
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES (printf('%064d', 1), 'a', '\"pytest\"', '', 'default', 'pytest', 0)",
        // [ADR 0077] An adapter's word is 1 to 32 of a-z, 0-9 and -, whichever adapter it is.
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES (printf('%064d', 1), 'a', '[]', '', 'default', 'Jest', 0)",
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES (printf('%064d', 1), 'a', '[]', '', 'default', '', 0)",
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES (printf('%064d', 1), 'a', '[]', '', 'default', 'swift test', 0)",
        // [ADR 0077] A test has an id, a state a run records, and no negative duration.
        "INSERT INTO test (unit_ref, test_id, state, duration_ms) VALUES (1, '', 'passed', 0)",
        "INSERT INTO test (unit_ref, test_id, state, duration_ms) VALUES (1, 'test_a', 'reused', 0)",
        "INSERT INTO test (unit_ref, test_id, state, duration_ms) VALUES (1, 'test_a', 'passed', -1)",
        "INSERT INTO test (unit_ref, test_id, state, duration_ms) VALUES (999, 'test_a', 'passed', 0)",
        "INSERT INTO test (unit_ref, test_id, state, duration_ms, list_ref) VALUES (1, 'test_a', 'passed', 0, 999)",
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES (printf('%064d', 1), 'a', '[]', '', 'default', 'pytest', 'yesterday')",
        "INSERT INTO version (path_ref, entry) VALUES (1, 'not a hash')",
        "INSERT INTO edge (check_ref, from_ref, to_ref, kind) VALUES (1, 1, 1, 'calls')",
        "INSERT INTO unit (check_ref, path_ref, ran_at) VALUES (999, 1, 0)",
        // [C-38] A shared row names the system that saw it; a row seen here names none.
        "INSERT INTO unit (check_ref, path_ref, ran_at, origin) VALUES (1, (SELECT id FROM path WHERE path = 'other.py'), 0, 'shared')",
        "INSERT INTO unit (check_ref, path_ref, ran_at, origin, shared_os) VALUES (1, (SELECT id FROM path WHERE path = 'other.py'), 0, 'seen here', 'linux')",
        "INSERT INTO unit (check_ref, path_ref, ran_at, origin, shared_os) VALUES (1, (SELECT id FROM path WHERE path = 'other.py'), 0, 'shared', 'beos')",
        "INSERT INTO hint_known (check_ref, address, os, content_id, how, at) VALUES (1, printf('%064d', 1), 'linux', printf('%064d', 1), 'guessed', 0)",
    ] {
        assert!(conn.execute_batch(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_store_of_a_repository_is_one_file_and_the_sweep_leaves_it() {
    let cache = Path::new("/cache");
    let a = path(cache, Some("example.com/Acme/Shop"));
    assert_eq!(a, path(cache, Some("example.com/acme/shop")));
    assert_ne!(a, path(cache, Some("example.com/acme/web")));
    assert_ne!(a, path(cache, None));
    let name = a.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with(".v2.sqlite"));
    assert!(is_store_file(&name) && is_store_file(&format!("{name}-wal")));
    assert!(!is_store_file("x.map.json") && !is_store_file("x.v1.sqlite"));
}

// covers: C-36, C-39
#[test]
fn a_test_files_tests_are_kept_with_it_and_replaced_with_it() {
    use super::super::store::TestRow;
    use hushrun_core::units::State;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.v2.sqlite");
    let row = |state: State, ms: u64, why: Option<&str>| TestRow {
        state,
        duration_ms: ms,
        not_followed: why
            .map(|w| NotFollowed {
                why: w.into(),
                detail: "git".into(),
            })
            .into_iter()
            .collect(),
        not_kept: None,
        list: None,
    };
    let mut with_tests = mapped(&[("tests/test_a.py", HASH_A)]);
    with_tests.tests = BTreeMap::from([
        ("test_one".to_string(), row(State::Passed, 4, None)),
        (
            "TestA::test_two[x y]".to_string(),
            row(State::Failed, 9, None),
        ),
        (
            "test_spawns".to_string(),
            row(State::NotObserved, 1, Some("subprocess")),
        ),
    ]);
    let r = run(
        "unit",
        &[("tests/test_a.py", with_tests.clone())],
        &["tests/test_a.py"],
    );
    write_run(&path, &r).unwrap();
    let map = load(&path, &id("unit")).map;
    assert_eq!(map.units["tests/test_a.py"].tests, with_tests.tests);
    // A run that saw the test file again replaces its tests with what it saw; \[C-37\] a test it
    // decided `not-run` keeps why, as its adapter said it.
    let mut fewer = mapped(&[("tests/test_a.py", HASH_A)]);
    // \[C-37\] And the test file keeps why it kept no result of its own.
    fewer.not_kept = Some("one of its tests was skipped in a run that failed".into());
    fewer.tests = BTreeMap::from([(
        "test_one".to_string(),
        TestRow {
            not_kept: Some("its binary ended early".into()),
            ..row(State::NotRun, 0, None)
        },
    )]);
    let r = run(
        "unit",
        &[("tests/test_a.py", fewer.clone())],
        &["tests/test_a.py"],
    );
    write_run(&path, &r).unwrap();
    let map = load(&path, &id("unit")).map;
    assert_eq!(map.units["tests/test_a.py"].tests, fewer.tests);
    assert_eq!(map.units["tests/test_a.py"].not_kept, fewer.not_kept);
    // \[C-37\] Why it kept nothing belongs to a `not-run` test, or a `passed` one that keeps no
    // pass: the store refuses it on another.
    {
        let conn = Connection::open(&path).unwrap();
        assert!(
            conn.execute(
                "UPDATE test SET state = 'failed' WHERE not_kept IS NOT NULL",
                []
            )
            .is_err()
        );
        assert!(
            conn.execute("UPDATE test SET not_kept = '' WHERE state = 'not-run'", [])
                .is_err()
        );
    }
    // A test file that left the scope takes its tests with it.
    let r = run("unit", &[], &["tests/other.py"]);
    write_run(&path, &r).unwrap();
    let conn = Connection::open(&path).unwrap();
    let left: i64 = conn
        .query_row("SELECT count(*) FROM test", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

/// The tables of schema 1, as an older hushrun (0.13.0) made them.
const SCHEMA_1_TABLES: &str = "
CREATE TABLE path (
    id   INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE
) STRICT;

-- A file's kind and content hash, as a source manifest writes them (D-4): one row per version
-- of a file some row needs.
CREATE TABLE version (
    id       INTEGER PRIMARY KEY,
    path_ref INTEGER NOT NULL REFERENCES path (id),
    entry    TEXT NOT NULL CHECK (entry GLOB '? [0-9a-f]*' AND length(entry) = 66),
    UNIQUE (path_ref, entry)
) STRICT;

-- One row per check id (C-33), with what it is, for `hushrun why`.
CREATE TABLE checks (
    id         INTEGER PRIMARY KEY,
    check_id   TEXT NOT NULL UNIQUE CHECK (length(check_id) = 64),
    name       TEXT NOT NULL CHECK (name <> ''),
    command    TEXT NOT NULL CHECK (json_valid(command) AND json_type(command) = 'array'),
    cwd        TEXT NOT NULL,
    scope      TEXT NOT NULL CHECK (scope <> ''),
    adapter    TEXT NOT NULL CHECK (adapter IN ('pytest', 'cargo', 'go', 'vitest')),
    -- When a run last wrote it, in milliseconds since 1970, by this machine's clock.
    written_at INTEGER NOT NULL
) STRICT;

-- The check's last run: its rest set, its sources, and Vitest's list of test files.
CREATE TABLE check_rest (
    check_ref   INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    version_ref INTEGER NOT NULL REFERENCES version (id),
    PRIMARY KEY (check_ref, version_ref)
) STRICT, WITHOUT ROWID;
CREATE TABLE check_source (
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    path_ref  INTEGER NOT NULL REFERENCES path (id),
    PRIMARY KEY (check_ref, path_ref)
) STRICT, WITHOUT ROWID;
CREATE TABLE check_listed (
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    path_ref  INTEGER NOT NULL REFERENCES path (id),
    PRIMARY KEY (check_ref, path_ref)
) STRICT, WITHOUT ROWID;

-- Each test file of a check, as it last ran here, or [C-38] as another machine saw it read the
-- files of a pass that reused it here.
CREATE TABLE unit (
    id        INTEGER PRIMARY KEY,
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    path_ref  INTEGER NOT NULL REFERENCES path (id),
    ran_at    INTEGER NOT NULL,
    origin    TEXT NOT NULL CHECK (origin IN ('seen here', 'shared')),
    -- The system whose machine saw a shared list: one for `shared`, none for `seen here`.
    shared_os TEXT CHECK (shared_os IN ('linux', 'macos', 'windows')),
    UNIQUE (check_ref, path_ref),
    CHECK ((origin = 'seen here') = (shared_os IS NULL))
) STRICT;
-- What it read that its key holds, each file as it was then.
CREATE TABLE unit_read (
    unit_ref    INTEGER NOT NULL REFERENCES unit (id) ON DELETE CASCADE,
    version_ref INTEGER NOT NULL REFERENCES version (id),
    PRIMARY KEY (unit_ref, version_ref)
) STRICT, WITHOUT ROWID;
CREATE INDEX unit_read_by_version ON unit_read (version_ref);
-- What the adapter couldn't follow while it ran, by its word: any makes it `not observed`.
CREATE TABLE unit_not_followed (
    unit_ref INTEGER NOT NULL REFERENCES unit (id) ON DELETE CASCADE,
    why      TEXT NOT NULL CHECK (why <> ''),
    detail   TEXT NOT NULL,
    PRIMARY KEY (unit_ref, why, detail)
) STRICT, WITHOUT ROWID;
-- [ADR 0070] Directories it listed: a plan adds every file below each.
CREATE TABLE unit_dir (
    unit_ref INTEGER NOT NULL REFERENCES unit (id) ON DELETE CASCADE,
    path_ref INTEGER NOT NULL REFERENCES path (id),
    PRIMARY KEY (unit_ref, path_ref)
) STRICT, WITHOUT ROWID;

-- Direct edges an adapter saw in a check's runs, for `hushrun why` to show a path. Never read
-- for a key.
CREATE TABLE edge (
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    from_ref  INTEGER NOT NULL REFERENCES path (id),
    to_ref    INTEGER NOT NULL REFERENCES path (id),
    kind      TEXT NOT NULL CHECK (kind IN ('import', 'open', 'read')),
    PRIMARY KEY (check_ref, from_ref, to_ref, kind)
) STRICT, WITHOUT ROWID;
CREATE INDEX edge_to ON edge (to_ref);

-- [C-38] Each hint this machine sent or received for a check, by its address and system: one it
-- already knows isn't sent again. The newest 200 of a check.
CREATE TABLE hint_known (
    check_ref  INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    address    TEXT NOT NULL CHECK (length(address) = 64),
    os         TEXT NOT NULL CHECK (os IN ('linux', 'macos', 'windows')),
    content_id TEXT NOT NULL CHECK (length(content_id) = 64),
    how        TEXT NOT NULL CHECK (how IN ('sent', 'received')),
    at         INTEGER NOT NULL,
    PRIMARY KEY (check_ref, address, os, content_id)
) STRICT, WITHOUT ROWID;

-- [C-34] Each test file that would have been reused and failed: the newest 200 of a check.
CREATE TABLE disagreement (
    id        INTEGER PRIMARY KEY,
    check_ref INTEGER NOT NULL REFERENCES checks (id) ON DELETE CASCADE,
    unit      TEXT NOT NULL,
    key       TEXT NOT NULL CHECK (length(key) = 64),
    run_key   TEXT NOT NULL CHECK (length(run_key) = 64),
    at        TEXT NOT NULL
) STRICT;
";

/// A store of schema 1 at `path`, as an older hushrun wrote it, with rows of every table: a check
/// of pytest's, a test file that read two files, one that wasn't followed, the rest set, edges, a
/// hint known and a disagreement.
fn schema_1_store(path: &Path, check_id: &Digest) {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(SCHEMA_1_TABLES).unwrap();
    conn.execute_batch(&format!(
        "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = 1;
         INSERT INTO path (id, path) VALUES (1, 'tests/test_a.py'), (2, 'app.py'),
             (3, 'tests/test_b.py'), (4, 'data'), (5, 'uv.lock');
         INSERT INTO version (id, path_ref, entry) VALUES (1, 1, '{HASH_A}'), (2, 2, '{HASH_B}'),
             (3, 5, '{HASH_A}');
         INSERT INTO checks (id, check_id, name, command, cwd, scope, adapter, written_at)
             VALUES (1, '{}', 'unit', '[\"pytest\",\"-q\"]', '', 'default', 'pytest', 1);
         INSERT INTO check_rest VALUES (1, 3);
         INSERT INTO check_source VALUES (1, 2);
         INSERT INTO unit (id, check_ref, path_ref, ran_at, origin, shared_os)
             VALUES (1, 1, 1, 1, 'seen here', NULL), (2, 1, 3, 1, 'shared', 'linux');
         INSERT INTO unit_read VALUES (1, 1), (1, 2);
         INSERT INTO unit_not_followed VALUES (2, 'subprocess', 'git');
         INSERT INTO unit_dir VALUES (1, 4);
         INSERT INTO edge VALUES (1, 1, 2, 'import');
         INSERT INTO hint_known VALUES (1, '{}', 'linux', '{}', 'sent', 1);
         INSERT INTO disagreement (check_ref, unit, key, run_key, at)
             VALUES (1, 'tests/test_a.py', '{}', '{}', '2026-11-01T00:00:00Z');",
        check_id.hex(),
        "a".repeat(64),
        "b".repeat(64),
        "c".repeat(64),
        "d".repeat(64),
    ))
    .unwrap();
}

// covers: C-36
#[test]
fn a_store_of_schema_1_is_imported_whole_once_and_left_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = super::path(dir.path(), Some("example.com/acme/shop"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let old = older(&path).unwrap();
    assert!(old.to_string_lossy().ends_with(".v1.sqlite"));
    let check_id = id("unit");
    schema_1_store(&old, &check_id);
    let before = std::fs::read(&old).unwrap();
    assert_eq!(
        upgrade(&path).as_deref(),
        old.file_name().and_then(|n| n.to_str())
    );
    let map = load(&path, &check_id);
    assert!(map.unread.is_none(), "{:?}", map.unread);
    let map = map.map;
    assert!(map.reported());
    assert_eq!(
        map.units["tests/test_a.py"]
            .reads
            .keys()
            .collect::<Vec<_>>(),
        ["app.py", "tests/test_a.py"]
    );
    assert_eq!(
        map.units["tests/test_a.py"].dirs.iter().collect::<Vec<_>>(),
        ["data"]
    );
    assert_eq!(map.units["tests/test_b.py"].not_followed.len(), 1);
    assert_eq!(
        map.units["tests/test_b.py"].origin,
        Origin::Shared(hushrun_core::hints::Os::Linux)
    );
    assert_eq!(adapter_of(&path, &check_id).as_deref(), Some("pytest"));
    assert_eq!(edges(&path, &check_id).unwrap().len(), 1);
    assert_eq!(disagreements(&path, &check_id).unwrap().len(), 1);
    // Its tables are this schema's: a check of an adapter schema 1 didn't know is kept, and a run
    // writes into it.
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE checks SET adapter = 'ctest' WHERE check_id = ?1",
            [check_id.hex()],
        )
        .unwrap();
    assert_eq!(adapter_of(&path, &check_id).as_deref(), Some("ctest"));
    let r = run(
        "other",
        &[("tests/test_a.py", mapped(&[("app.py", HASH_B)]))],
        &["app.py", "tests/test_a.py"],
    );
    write_run(&path, &r).unwrap();
    assert!(load(&path, &id("other")).map.reported());
    // Once: with a store of this schema there, nothing is imported again; the older file stays.
    assert_eq!(upgrade(&path), None);
    assert_eq!(std::fs::read(&old).unwrap(), before);
}

// covers: C-36
#[test]
fn a_file_that_isnt_a_store_of_schema_1_is_never_imported() {
    let dir = tempfile::tempdir().unwrap();
    let path = super::path(dir.path(), None);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let old = older(&path).unwrap();
    std::fs::write(&old, b"not a database").unwrap();
    assert_eq!(upgrade(&path), None);
    assert!(!path.exists());
    // Another application's database: nothing either, and nothing left behind.
    std::fs::remove_file(&old).unwrap();
    Connection::open(&old)
        .unwrap()
        .execute_batch("CREATE TABLE t (x); PRAGMA user_version = 1;")
        .unwrap();
    assert_eq!(upgrade(&path), None);
    assert!(!path.exists());
    let left: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(left.iter().all(|n| !n.starts_with(".import-")), "{left:?}");
}

// covers: C-36
#[test]
fn a_store_that_cant_be_read_is_set_aside_and_made_anew_by_the_next_write() {
    // Schema 2 as a build before its test table's `not_kept` column made it: every read and every
    // write of it fails, for ever, unless the next write sets it aside and starts again.
    let dir = tempfile::tempdir().unwrap();
    let path = super::path(dir.path(), Some("example.com/acme/shop"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let column = "    -- [C-37] Why a test the run decided `not-run`, or `passed` with no pass kept, kept no result.\n    not_kept    TEXT CHECK (not_kept IS NULL OR (state IN ('not-run', 'passed') AND not_kept <> '')),\n";
    assert!(TABLES.contains(column));
    let old = TABLES.replace(column, "");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(&old).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = {SCHEMA};"
        ))
        .unwrap();
    }
    let r = run(
        "unit",
        &[("tests/test_a.py", mapped(&[("app.py", HASH_B)]))],
        &["app.py", "tests/test_a.py"],
    );
    write_run(&path, &r).unwrap();
    let loaded = load(&path, &id("unit"));
    assert!(loaded.unread.is_none(), "{:?}", loaded.unread);
    assert!(loaded.map.reported());
    // Set aside, not deleted: one kept, beside it.
    let mut aside = path.as_os_str().to_os_string();
    aside.push(".unread");
    assert!(std::path::Path::new(&aside).is_file());
}

// covers: C-36, C-37
#[test]
fn a_unit_that_isnt_a_file_keeps_its_row_through_a_run_that_reused_it() {
    // A CTest program, named from the build directory, which no file of the scope is: a run that
    // reused it saw it and wrote nothing of it, and its row stays for `hushrun why`. A run that
    // no longer knows it forgets it.
    let dir = tempfile::tempdir().unwrap();
    let path = super::path(dir.path(), Some("example.com/acme/shop"));
    let first = run(
        "unit",
        &[("test_5", mapped(&[("src/five.c", HASH_A)]))],
        &["src/five.c"],
    );
    write_run(&path, &first).unwrap();
    let mut reused = run("unit", &[], &["src/five.c"]);
    reused.known.insert("test_5".to_string());
    write_run(&path, &reused).unwrap();
    assert!(load(&path, &id("unit")).map.units.contains_key("test_5"));
    let gone = run("unit", &[], &["src/five.c"]);
    write_run(&path, &gone).unwrap();
    assert!(!load(&path, &id("unit")).map.units.contains_key("test_5"));
}

/// `path` with `.unread` after it.
fn aside_of(path: &Path) -> PathBuf {
    let mut aside = path.as_os_str().to_os_string();
    aside.push(".unread");
    PathBuf::from(aside)
}

// covers: C-36
#[cfg(unix)]
#[test]
fn a_store_that_cant_be_written_for_now_is_left_as_it_is() {
    use std::os::unix::fs::PermissionsExt;
    // A healthy store, read-only for one run (a full disk, an I/O error, a permission are the
    // same): the run reads it as nothing and writes nothing, and the store is there after.
    let dir = tempfile::tempdir().unwrap();
    let path = super::path(dir.path(), Some("example.com/acme/shop"));
    let first = run(
        "unit",
        &[("tests/test_a.py", mapped(&[("app.py", HASH_A)]))],
        &["app.py", "tests/test_a.py"],
    );
    write_run(&path, &first).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    // As root, permissions hold nothing back: nothing to see.
    let held = std::fs::OpenOptions::new().write(true).open(&path).is_err();
    let other = run(
        "other",
        &[("tests/test_b.py", mapped(&[("app.py", HASH_A)]))],
        &["app.py", "tests/test_b.py"],
    );
    let wrote = write_run(&path, &other);
    if path.exists() {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    if !held {
        return;
    }
    assert!(wrote.is_err());
    assert!(!aside_of(&path).exists());
    let loaded = load(&path, &id("unit"));
    assert!(loaded.unread.is_none(), "{:?}", loaded.unread);
    assert!(loaded.map.reported());
}

// covers: C-36
#[test]
fn only_damage_a_retry_cant_cure_sets_a_store_aside() {
    use rusqlite::ffi;
    let error = |code| rusqlite::Error::SqliteFailure(ffi::Error::new(code), None);
    for code in [ffi::SQLITE_NOTADB, ffi::SQLITE_CORRUPT] {
        assert!(matches!(unread(&error(code)), Unread::Broken(_)), "{code}");
    }
    for code in [
        ffi::SQLITE_READONLY,
        ffi::SQLITE_FULL,
        ffi::SQLITE_IOERR,
        ffi::SQLITE_PERM,
        ffi::SQLITE_CANTOPEN,
        ffi::SQLITE_CONSTRAINT,
    ] {
        assert!(matches!(unread(&error(code)), Unread::Trouble(_)), "{code}");
    }
    for code in [ffi::SQLITE_BUSY, ffi::SQLITE_LOCKED] {
        assert_eq!(unread(&error(code)), Unread::Busy, "{code}");
    }
}

// covers: C-36
#[test]
fn two_runs_that_find_a_store_damaged_set_aside_only_the_damaged_one() {
    // Both fail on the damaged store; the first sets it aside and makes a new one, which it writes
    // to; only then does the second decide. It must set aside nothing of the first's work.
    let dir = tempfile::tempdir().unwrap();
    let path = super::path(dir.path(), Some("example.com/acme/shop"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let damaged = b"this is not SQLite, it is a partial download".to_vec();
    std::fs::write(&path, &damaged).unwrap();
    let both_failed = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (first_done, wait_first) = std::sync::mpsc::channel::<()>();
    let wait_first = std::sync::Mutex::new(Some(wait_first));
    let write_as = |check: &'static str, file: &'static str| {
        let r = run(
            check,
            &[(file, mapped(&[("app.py", HASH_A)]))],
            &["app.py", file],
        );
        write_run(&path, &r)
    };
    std::thread::scope(|scope| {
        let barrier = both_failed.clone();
        let first = scope.spawn(move || {
            BEFORE_SET_ASIDE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    barrier.wait();
                }))
            });
            let wrote = write_as("unit", "tests/test_a.py");
            first_done.send(()).unwrap();
            wrote
        });
        let barrier = both_failed.clone();
        let waiting = wait_first.lock().unwrap().take().unwrap();
        let second = scope.spawn(move || {
            BEFORE_SET_ASIDE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    barrier.wait();
                    waiting.recv().unwrap();
                }))
            });
            write_as("other", "tests/test_b.py")
        });
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
    });
    // The damaged bytes are what was set aside, and the new store holds both runs' work.
    assert!(
        std::fs::read(aside_of(&path)).unwrap() == damaged,
        "the new store was set aside"
    );
    assert!(load(&path, &id("unit")).map.reported());
    assert!(load(&path, &id("other")).map.reported());
}

// covers: C-22, C-32
#[test]
fn readers_that_write_nothing_read_the_adapter_of_a_schema_1_store_without_importing() {
    // `hushrun ready` and `hushrun why` never import: they read the word from the older file, so
    // they choose the adapter the run, which imports first, chooses.
    let dir = tempfile::tempdir().unwrap();
    let path = super::path(dir.path(), Some("example.com/acme/shop"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let old = older(&path).unwrap();
    let check_id = id("unit");
    schema_1_store(&old, &check_id);
    let before = std::fs::read(&old).unwrap();
    let listing = || -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let listed = listing();
    assert_eq!(adapter_of(&path, &check_id).as_deref(), Some("pytest"));
    assert_eq!(adapter_of(&path, &id("other")), None);
    assert!(!path.exists());
    assert_eq!(std::fs::read(&old).unwrap(), before);
    // Not a file more beside it: no `-shm`, no `-wal` (C-36, ADR 0061).
    assert_eq!(listing(), listed);
}
