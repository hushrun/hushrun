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
    check: &'a str,
    seen: BTreeMap<String, Mapped>,
    tree: Tree,
    edges: BTreeSet<Edge>,
    disagreements: Vec<Disagreement>,
    now_ms: i64,
}

fn run<'a>(check: &'a str, seen: &[(&str, Mapped)], tree_paths: &[&str]) -> Run<'a> {
    Run {
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
            adapter: AdapterWord::Pytest,
            seen: &r.seen,
            tree: &r.tree,
            rest: &rest,
            sources: &sources,
            unit_files: Some(&listed),
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
        adapter: AdapterWord::Pytest,
        seen: &r.seen,
        tree: &r.tree,
        rest: &BTreeMap::new(),
        sources: &BTreeSet::new(),
        unit_files: None,
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
    for bad in [
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES ('x', 'a', '[]', '', 'default', 'pytest', 0)",
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES (printf('%064d', 1), 'a', '\"pytest\"', '', 'default', 'pytest', 0)",
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES (printf('%064d', 1), 'a', '[]', '', 'default', 'jest', 0)",
        "INSERT INTO checks (check_id, name, command, cwd, scope, adapter, written_at) VALUES (printf('%064d', 1), 'a', '[]', '', 'default', 'pytest', 'yesterday')",
        "INSERT INTO version (path_ref, entry) VALUES (1, 'not a hash')",
        "INSERT INTO edge (check_ref, from_ref, to_ref, kind) VALUES (1, 1, 1, 'calls')",
        "INSERT INTO unit (check_ref, path_ref, ran_at) VALUES (999, 1, 0)",
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
    assert!(name.ends_with(".v1.sqlite"));
    assert!(is_store_file(&name) && is_store_file(&format!("{name}-wal")));
    assert!(!is_store_file("x.map.json") && !is_store_file("x.v2.sqlite"));
}
