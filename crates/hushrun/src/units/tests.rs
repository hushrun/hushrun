use std::cell::{Cell, RefCell};

use super::*;
use hushrun_core::names::{CheckName, ScopeName};
use store::{Lookup, Resumed, ReuseState};

/// Results on a server that keeps what it is sent, or refuses it.
struct Hints {
    refuse: Cell<bool>,
    sent: RefCell<Vec<Vec<u8>>>,
}

impl UnitResults for Hints {
    fn lookup(&self, _: &[Digest], _: &dyn Fn(&RunResult) -> bool) -> Result<Lookup, String> {
        Ok(Lookup::default())
    }
    fn suspend(&self, _: &str, _: &Digest, _: &Digest, _: &str) -> Result<ReuseState, String> {
        unreachable!()
    }
    fn resume(&self, _: i64, _: &str) -> Result<Resumed, String> {
        unreachable!()
    }
    fn put_hint(&self, _: &store::HintPlace, bytes: &[u8]) -> Result<(), String> {
        self.sent.borrow_mut().push(bytes.to_vec());
        if self.refuse.get() {
            Err("refused".into())
        } else {
            Ok(())
        }
    }
}

fn tree(paths: &[String]) -> Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort();
    for (n, path) in sorted.iter().enumerate() {
        bytes.extend_from_slice(format!("f {:064x} {path}", n + 1).as_bytes());
        bytes.push(0);
    }
    Tree::from_manifest(&bytes).unwrap()
}

/// What a run of `units` test files that each passed and read `app.py` observed, kept in a store
/// in `dir`.
fn observed(dir: &Path, units: usize) -> (Observed, Tree) {
    let names: Vec<String> = (0..units).map(|n| format!("tests/test_{n}.py")).collect();
    let mut paths = names.clone();
    paths.push("app.py".into());
    let tree = tree(&paths);
    let base = KeyBase {
        check: CheckName::new("unit").unwrap(),
        command: vec!["pytest".into()],
        cwd: String::new(),
        scope: ScopeName::new("default").unwrap(),
        external: None,
        counts_from: None,
    };
    let check_id = store::check_id(&base, None);
    let mut o = Observed {
        share: Some(shared::Place::new(&check_id, &["app.py".to_string()])),
        store: Some(StoreAt {
            path: dir.join("store.v1.sqlite"),
            check_id,
            base,
            adapter: "pytest",
            hints: adapter::Hints::Sources,
        }),
        ..Observed::default()
    };
    for name in &names {
        o.units.insert(name.clone(), (State::Passed, None));
        let entry = |path: &str| {
            let (kind, hash) = tree.entry(path).unwrap();
            (path.to_string(), format!("{kind} {hash}"))
        };
        o.mapped.insert(
            name.clone(),
            Mapped {
                reads: [entry(name), entry("app.py")].into_iter().collect(),
                ..Mapped::default()
            },
        );
    }
    (o, tree)
}

// covers: C-38
#[test]
fn a_hint_is_sent_once_and_again_after_a_send_that_failed() {
    let dir = tempfile::tempdir().unwrap();
    let (o, tree) = observed(dir.path(), 3);
    let now = Time::parse("2026-11-11T12:00:00Z").unwrap();
    o.keep(&Digest::of(b"run"), &tree, now);
    let results = Hints {
        refuse: Cell::new(true),
        sent: RefCell::new(Vec::new()),
    };
    assert_eq!(o.share(&results, now), None);
    assert_eq!(results.sent.borrow().len(), 1);
    // Refused: nothing noted, so the next run sends it again.
    results.refuse.set(false);
    assert_eq!(o.share(&results, now), None);
    assert_eq!(results.sent.borrow().len(), 2);
    let sent = results.sent.borrow()[1].clone();
    assert_eq!(results.sent.borrow()[0], sent);
    // Kept: the same hint isn't sent again.
    assert_eq!(o.share(&results, now), None);
    assert_eq!(results.sent.borrow().len(), 2);
    // The note of it is in the store's one file, which is all a CI cache keeps (C-15).
    let wal = dir.path().join("store.v1.sqlite-wal");
    assert!(
        std::fs::metadata(&wal).map_or(true, |m| m.len() == 0),
        "{} holds what the file doesn't",
        wal.display()
    );
    // It is what the store says, under the check's key, with no path in the clear.
    let place = o.share.as_ref().unwrap();
    let read = hushrun_core::hints::read(&sent, &place.key, tree.paths()).unwrap();
    assert_eq!(read.hint.units.len(), 3);
    assert!(read.hint.units["tests/test_0.py"].reads.contains("app.py"));
    let content = hushrun_core::hints::inflate(&sent).unwrap();
    assert!(!content.windows(6).any(|w| w == b"app.py"));
}

// covers: C-38
#[test]
fn a_hint_too_large_to_send_isnt_sent_and_is_said_once() {
    let dir = tempfile::tempdir().unwrap();
    let (o, tree) = observed(dir.path(), MOST_UNITS + 1);
    let now = Time::parse("2026-11-11T12:00:00Z").unwrap();
    o.keep(&Digest::of(b"run"), &tree, now);
    let results = Hints {
        refuse: Cell::new(false),
        sent: RefCell::new(Vec::new()),
    };
    assert_eq!(
        o.share(&results, now).as_deref(),
        Some(
            "unit: what its test files read wasn't shared with other machines: it is too large (5001 test files, more than 5000)"
        )
    );
    assert!(results.sent.borrow().is_empty());
}

/// \[C-41\] What a run's hint holds of tests: three test files, `test_0` passed whole,
/// `test_1` with a test that failed, `test_2` partly reused, each with a test that passed (with a
/// list of its own with `reads: test`), one not followed and one not run.
fn sent_tests(
    reads: Reads,
) -> BTreeMap<String, BTreeMap<String, Option<hushrun_core::hints::Unit>>> {
    let dir = tempfile::tempdir().unwrap();
    let (mut o, tree) = observed(dir.path(), 3);
    o.by_test = true;
    o.reads = reads;
    o.units
        .insert("tests/test_1.py".into(), (State::Failed, None));
    o.units
        .insert("tests/test_2.py".into(), (State::NotRun, None));
    o.partly.insert("tests/test_2.py".into());
    for (n, mapped) in o.mapped.values_mut().enumerate() {
        let list = (reads == Reads::Test).then(|| store::TestListRow {
            reads: mapped.reads.clone(),
            dirs: BTreeSet::new(),
        });
        let row = |state| store::TestRow {
            state,
            duration_ms: 1,
            not_followed: BTreeSet::new(),
            list: list.clone(),
        };
        mapped.tests.insert("test_ok".into(), row(State::Passed));
        mapped.tests.insert(
            "test_other".into(),
            row(if n == 1 { State::Failed } else { State::Passed }),
        );
        mapped
            .tests
            .insert("test_child".into(), row(State::NotObserved));
        mapped
            .tests
            .insert("test_skipped".into(), row(State::NotRun));
    }
    let now = Time::parse("2026-11-11T12:00:00Z").unwrap();
    o.keep(&Digest::of(b"run"), &tree, now);
    let results = Hints {
        refuse: Cell::new(false),
        sent: RefCell::new(Vec::new()),
    };
    assert_eq!(o.share(&results, now), None);
    let sent = results.sent.borrow();
    assert_eq!(sent.len(), 1);
    let place = o.share.as_ref().unwrap();
    let read = hushrun_core::hints::read(&sent[0], &place.key, tree.paths()).unwrap();
    // A test file partly reused is this machine's to send, as one that ran.
    assert_eq!(read.hint.units.len(), 3, "{read:?}");
    read.hint.tests
}

// covers: C-41
#[test]
fn a_hint_holds_the_tests_that_have_keys_and_a_test_file_partly_reused() {
    let ids = |tests: &BTreeMap<String, Option<hushrun_core::hints::Unit>>| {
        tests.keys().cloned().collect::<Vec<_>>()
    };
    // `reads: file`: ids alone, of the test files that didn't pass whole.
    let tests = sent_tests(Reads::File);
    assert_eq!(
        tests.keys().collect::<Vec<_>>(),
        ["tests/test_1.py", "tests/test_2.py"]
    );
    for of_file in tests.values() {
        assert_eq!(ids(of_file), ["test_ok", "test_other"]);
        assert!(of_file.values().all(Option::is_none));
    }
    // `reads: test`: every test file's, with each test's own list.
    let tests = sent_tests(Reads::Test);
    assert_eq!(tests.len(), 3);
    let list = tests["tests/test_0.py"]["test_ok"].as_ref().unwrap();
    assert_eq!(
        list.reads,
        ["app.py", "tests/test_0.py"]
            .map(str::to_string)
            .into_iter()
            .collect()
    );
}

// covers: C-38, C-36
#[test]
fn a_test_file_reused_on_another_systems_list_isnt_in_this_machines_hint() {
    let dir = tempfile::tempdir().unwrap();
    let (mut o, tree) = observed(dir.path(), 3);
    let this = hushrun_core::hints::Os::this().unwrap();
    let other = if this == hushrun_core::hints::Os::Linux {
        hushrun_core::hints::Os::Macos
    } else {
        hushrun_core::hints::Os::Linux
    };
    // tests/test_0.py was reused on a list a machine of another system saw, kept here as such;
    // tests/test_1.py on one of this system's.
    o.units
        .insert("tests/test_0.py".into(), (State::Reused, None));
    o.units
        .insert("tests/test_1.py".into(), (State::Reused, None));
    o.mapped.get_mut("tests/test_0.py").unwrap().origin = Origin::Shared(other);
    o.mapped.get_mut("tests/test_1.py").unwrap().origin = Origin::Shared(this);
    let now = Time::parse("2026-11-11T12:00:00Z").unwrap();
    o.keep(&Digest::of(b"run"), &tree, now);
    let results = Hints {
        refuse: Cell::new(false),
        sent: RefCell::new(Vec::new()),
    };
    assert_eq!(o.share(&results, now), None);
    let sent = results.sent.borrow();
    assert_eq!(sent.len(), 1);
    let place = o.share.as_ref().unwrap();
    let read = hushrun_core::hints::read(&sent[0], &place.key, tree.paths()).unwrap();
    let units: Vec<&str> = read.hint.units.keys().map(String::as_str).collect();
    assert_eq!(units, ["tests/test_1.py", "tests/test_2.py"]);
}

// covers: C-39
#[test]
fn an_adapter_that_cant_do_what_the_check_asks_reuses_by_test_file_and_says_so_once() {
    let words = |text: &str| text.split(' ').map(str::to_string).collect::<Vec<_>>();
    let tests = |unit: Unit, reads: Reads| Tests {
        mode: Mode::Reuse,
        unit,
        reads,
        ..Tests::default()
    };
    let cargo = cargo::recognize(&words("cargo nextest run")).unwrap();
    let go = gotest::recognize(&words("go test ./...")).unwrap().unwrap();
    let vitest = vitest::recognize(&words("npx vitest run"))
        .unwrap()
        .unwrap();
    // Test files, as asked: nothing to say, whatever the adapter.
    for adapter in [&cargo as &dyn Adapter, &go, &vitest, &pytest::Pytest] {
        let (unit, reads, notes) = what_it_reuses(&tests(Unit::File, Reads::File), adapter);
        assert_eq!((unit, reads), (Unit::File, Reads::File));
        assert!(notes.is_empty(), "{notes:?}");
    }
    // Tests, of an adapter that knows none: by test file, said once, in the runner's words.
    let (unit, _, notes) = what_it_reuses(&tests(Unit::Test, Reads::File), &cargo);
    assert_eq!(unit, Unit::File);
    assert_eq!(
        notes,
        ["its tests are reused by test binary: cargo's tests can't be reused one by one yet"]
    );
    // \[ADR 0083\] Go knows tests, unless COMMAND chooses its own.
    let (unit, reads, notes) = what_it_reuses(&tests(Unit::Test, Reads::Test), &go);
    assert_eq!((unit, reads), (Unit::Test, Reads::File));
    assert_eq!(
        notes,
        [
            "each test is taken to have read what its package read: go test can't be watched test by test yet"
        ]
    );
    let run = gotest::recognize(&words("go test -run TestX ./..."))
        .unwrap()
        .unwrap();
    let (unit, _, notes) = what_it_reuses(&tests(Unit::Test, Reads::File), &run);
    assert_eq!(unit, Unit::File);
    assert_eq!(
        notes,
        ["its tests are reused by package: go test -run's tests can't be reused one by one yet"]
    );
    let (_, _, notes) = what_it_reuses(&tests(Unit::Test, Reads::File), &vitest);
    assert_eq!(
        notes,
        ["its tests are reused by test file: Vitest's tests can't be reused one by one yet"]
    );
    // pytest knows tests, and \[ADR 0080\] watches each one: it says itself where it can't.
    let (unit, reads, notes) = what_it_reuses(&tests(Unit::Test, Reads::File), &pytest::Pytest);
    assert_eq!((unit, reads), (Unit::Test, Reads::File));
    assert!(notes.is_empty());
    let (unit, reads, notes) = what_it_reuses(&tests(Unit::Test, Reads::Test), &pytest::Pytest);
    assert_eq!((unit, reads), (Unit::Test, Reads::Test));
    assert!(notes.is_empty());
}

/// What `decide` needs of a run of one pytest test file, `tests/test_a.py`, in `dir`, on `tree`.
fn decided(
    dir: &Path,
    tree: &Tree,
    seen: Seen,
    map: &store::Map,
    plan: &Plan,
) -> (Observed, Keyer<'static>) {
    decided_by(dir, tree, seen, map, plan, Unit::Test)
}

/// [`decided`] with tests decided one by one or not (`unit`), as a run past the limit isn't.
fn decided_by(
    dir: &Path,
    tree: &Tree,
    seen: Seen,
    map: &store::Map,
    plan: &Plan,
    unit: Unit,
) -> (Observed, Keyer<'static>) {
    let base = KeyBase {
        check: CheckName::new("unit").unwrap(),
        command: vec!["pytest".into()],
        cwd: String::new(),
        scope: ScopeName::new("default").unwrap(),
        external: None,
        counts_from: None,
    };
    let results = Hints {
        refuse: Cell::new(false),
        sent: RefCell::new(Vec::new()),
    };
    let sets = units::classify(
        tree.paths(),
        &|p| p.starts_with("tests/"),
        units::Sources::Kinds(&[".py"]),
        &[],
    );
    let counts = |_: &RunResult| true;
    let end = Finish {
        base: base.clone(),
        repository: None,
        tree,
        command_passed: true,
        counts: &counts,
        store: &results,
        root: dir,
        cwd: dir,
        mode: Mode::Reuse,
    };
    let read = Read {
        seen: BTreeMap::from([("tests/test_a.py".to_string(), seen)]),
        failures_by_unit: true,
        sets: sets.clone(),
        ..Read::default()
    };
    let at = StoreAt {
        path: dir.join("store.v2.sqlite"),
        check_id: store::check_id(&base, None),
        base: base.clone(),
        adapter: "pytest",
        hints: adapter::Hints::Sources,
    };
    let watches = facts::Watches::default();
    let mode = Modes {
        unit,
        reads: Reads::File,
        watches: &watches,
        unwatched: None,
        shared: &Shared::default(),
    };
    let observed = decide(
        &end,
        &Tests::default(),
        &mode,
        read,
        map,
        at,
        How::Plan(plan),
    );
    // The one keyer, for the files as they are, to say which keys a record must have.
    let tree: &'static Tree = Box::leak(Box::new(tree.clone()));
    let sets: &'static units::Sets = Box::leak(Box::new(sets));
    let base: &'static KeyBase = Box::leak(Box::new(base));
    (observed, Keyer::new(tree, sets, base))
}

// covers: C-39, C-34
#[test]
fn a_test_that_wasnt_followed_keeps_its_test_file_from_a_pass_when_tests_go_by_file() {
    // A test of the file started a process: its adapter says so in that test's facts. Whether
    // tests are decided one by one or, past the limit, by test file, the file is not observed,
    // records no pass, and its row keeps what wasn't followed where the next plan sees it: it
    // runs, whatever that process read.
    for unit in [Unit::Test, Unit::File] {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<String> = ["app.py", "tests/test_a.py"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let tree = tree(&paths);
        let spawned = units::NotFollowed {
            why: "subprocess".into(),
            detail: "python".into(),
        };
        let tests = BTreeMap::from([
            (
                "test_spawns".to_string(),
                facts::MergedTest {
                    finished: true,
                    not_followed: BTreeSet::from([spawned.clone()]),
                    ..facts::MergedTest::default()
                },
            ),
            (
                "test_ok".to_string(),
                facts::MergedTest {
                    finished: true,
                    ..facts::MergedTest::default()
                },
            ),
        ]);
        let seen = Seen {
            facts: Facts {
                complete: true,
                ..Facts::default()
            },
            reads: paths.iter().cloned().collect(),
            duration_ms: 3,
            tests: Some(tests),
        };
        let (o, _) = decided_by(
            dir.path(),
            &tree,
            seen,
            &store::Map::default(),
            &Plan::default(),
            unit,
        );
        assert_eq!(
            o.units["tests/test_a.py"],
            (State::NotObserved, None),
            "{unit:?}"
        );
        assert!(
            o.records()
                .iter()
                .all(|r| r.result != "pass" || unit == Unit::Test),
            "{unit:?}: {:?}",
            o.records()
        );
        let mapped = &o.mapped["tests/test_a.py"];
        let stopped = mapped.not_followed.contains(&spawned)
            || mapped
                .tests
                .get("test_spawns")
                .is_some_and(|row| row.state == State::NotObserved);
        assert!(stopped, "{unit:?}: {mapped:?}");
        // The next plan: no list to key the file by, or not reused whole.
        let mut map = store::Map::default();
        map.units.insert("tests/test_a.py".into(), mapped.clone());
        let lists = shared::lists(
            "tests/test_a.py",
            map.units.get("tests/test_a.py"),
            &Shared::default(),
            &tree,
            None,
        );
        assert!(
            lists.is_none() || !whole_reusable(Unit::Test, &map, "tests/test_a.py"),
            "{unit:?}"
        );
        assert!(
            lists.is_none() || unit == Unit::Test,
            "{unit:?}: by test file, the file itself isn't keyed"
        );
    }
}

// covers: C-39, C-34
#[test]
fn a_run_whose_plan_left_tests_out_never_records_a_pass_of_their_test_file() {
    // The plan listed test_one from a store that knew fewer tests than the limit; the run then
    // reported more, past it. Its adapter left test_one out: the file didn't run whole, so it
    // records no pass of its own on any path, and its tests are decided one by one.
    for unit in [Unit::Test, Unit::File] {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<String> = ["app.py", "tests/test_a.py"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let tree = tree(&paths);
        let tests = BTreeMap::from([
            (
                "test_one".to_string(),
                facts::MergedTest {
                    deselected: true,
                    ..facts::MergedTest::default()
                },
            ),
            (
                "test_two".to_string(),
                facts::MergedTest {
                    finished: true,
                    ..facts::MergedTest::default()
                },
            ),
        ]);
        let seen = Seen {
            facts: Facts {
                complete: true,
                ..Facts::default()
            },
            reads: paths.iter().cloned().collect(),
            duration_ms: 3,
            tests: Some(tests),
        };
        let mut plan = Plan::default();
        plan.tests.passed.insert(
            "tests/test_a.py::test_one".into(),
            TestPassed {
                unit: "tests/test_a.py".into(),
                id: "test_one".into(),
                key: Digest::of(b"its pass"),
                found: Found {
                    passed: true,
                    duration_ms: 7,
                },
                list: shared::TestList {
                    list: built_list(&BTreeSet::new()),
                    own: false,
                },
            },
        );
        let (o, keyer) = decided_by(dir.path(), &tree, seen, &store::Map::default(), &plan, unit);
        assert_eq!(o.units["tests/test_a.py"].0, State::NotRun, "{unit:?}");
        let reads: BTreeSet<String> = paths.iter().cloned().collect();
        let file_key = keyer.key("tests/test_a.py", &reads).unwrap();
        assert!(
            o.records().iter().all(|r| r.key != file_key),
            "{unit:?}: {:?}",
            o.records()
        );
        // test_two's own pass is recorded: the run counted by test.
        let two = keyer
            .test_key("tests/test_a.py", "test_two", &reads)
            .unwrap();
        assert!(
            o.records()
                .iter()
                .any(|r| r.key == two && r.result == "pass"),
            "{unit:?}"
        );
        assert_eq!(
            o.tests["tests/test_a.py"]["test_one"],
            State::Reused,
            "{unit:?}"
        );
    }
}

// covers: C-39
#[test]
fn a_test_its_adapter_never_leaves_out_records_no_pass_of_its_own() {
    // test_twin shares its id with another test, so its adapter can't leave it out; test_bad
    // failed, so the file records each test on its own: test_ok's pass and test_bad's fail, and
    // nothing for test_twin, which no plan can then list.
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<String> = ["app.py", "tests/test_a.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let tree = tree(&paths);
    let test = |failed: bool, never_left_out: bool| facts::MergedTest {
        finished: true,
        failed,
        never_left_out,
        ..facts::MergedTest::default()
    };
    let tests = BTreeMap::from([
        ("test_ok".to_string(), test(false, false)),
        ("test_twin".to_string(), test(false, true)),
        ("test_bad".to_string(), test(true, false)),
    ]);
    let seen = Seen {
        facts: Facts {
            complete: true,
            failed: true,
            ..Facts::default()
        },
        reads: paths.iter().cloned().collect(),
        duration_ms: 3,
        tests: Some(tests),
    };
    let (o, keyer) = decided(
        dir.path(),
        &tree,
        seen,
        &store::Map::default(),
        &Plan::default(),
    );
    let reads: BTreeSet<String> = paths.iter().cloned().collect();
    let key = |id: &str| keyer.test_key("tests/test_a.py", id, &reads).unwrap();
    let recorded: BTreeMap<Digest, String> = o
        .records()
        .iter()
        .map(|r| (r.key, r.result.clone()))
        .collect();
    assert_eq!(
        recorded.get(&key("test_ok")).map(String::as_str),
        Some("pass")
    );
    assert_eq!(
        recorded.get(&key("test_bad")).map(String::as_str),
        Some("fail")
    );
    assert_eq!(recorded.get(&key("test_twin")), None, "{recorded:?}");
    // Its row stays, as it ran.
    assert_eq!(
        o.mapped["tests/test_a.py"].tests["test_twin"].state,
        State::Passed
    );
}

// covers: C-39
#[test]
fn a_test_file_partly_reused_records_its_tests_and_no_pass_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<String> = ["app.py", "lib.py", "tests/test_a.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let tree = tree(&paths);
    let entry = |path: &str| {
        let (kind, hash) = tree.entry(path).unwrap();
        (path.to_string(), format!("{kind} {hash}"))
    };
    // The last run here: test_one imported lib.py and passed; test_two failed.
    let row = |state| store::TestRow {
        state,
        duration_ms: 7,
        not_followed: BTreeSet::new(),
        list: None,
    };
    let mut map = store::Map {
        reported: true,
        ..store::Map::default()
    };
    map.units.insert(
        "tests/test_a.py".into(),
        Mapped {
            reads: [entry("app.py"), entry("lib.py"), entry("tests/test_a.py")]
                .into_iter()
                .collect(),
            tests: BTreeMap::from([
                ("test_one".to_string(), row(State::Passed)),
                ("test_two".to_string(), row(State::Failed)),
            ]),
            ..Mapped::default()
        },
    );
    // This run: the plan listed test_one, which the adapter deselected; test_two ran and passed,
    // and nothing imported lib.py.
    let tests = BTreeMap::from([
        (
            "test_one".to_string(),
            facts::MergedTest {
                deselected: true,
                ..facts::MergedTest::default()
            },
        ),
        (
            "test_two".to_string(),
            facts::MergedTest {
                finished: true,
                duration_ms: 3,
                ..facts::MergedTest::default()
            },
        ),
    ]);
    let seen = Seen {
        facts: Facts {
            complete: true,
            ..Facts::default()
        },
        reads: ["app.py", "tests/test_a.py"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        duration_ms: 3,
        tests: Some(tests),
    };
    let mut plan = Plan::default();
    plan.tests.passed.insert(
        "tests/test_a.py::test_one".into(),
        TestPassed {
            unit: "tests/test_a.py".into(),
            id: "test_one".into(),
            key: Digest::of(b"its pass"),
            found: Found {
                passed: true,
                duration_ms: 7,
            },
            list: shared::TestList {
                list: built_list(&BTreeSet::new()),
                own: false,
            },
        },
    );
    let (o, keyer) = decided(dir.path(), &tree, seen, &map, &plan);
    // Not run whole: no pass of its own (reuse never chains, and this run didn't execute it all).
    assert_eq!(o.units["tests/test_a.py"].0, State::NotRun);
    assert_eq!(
        o.tests["tests/test_a.py"],
        BTreeMap::from([
            ("test_one".to_string(), State::Reused),
            ("test_two".to_string(), State::Passed),
        ])
    );
    // test_two records under its own key, over what the test file read now and what it read
    // when test_one last ran: what only a reused test read is still the file's.
    let union: BTreeSet<String> = ["app.py", "lib.py", "tests/test_a.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let key = keyer
        .test_key("tests/test_a.py", "test_two", &union)
        .unwrap();
    assert_eq!(
        o.records(),
        [UnitRecord {
            key,
            result: "pass".into(),
            duration_ms: 3,
        }]
    );
    let kept = &o.mapped["tests/test_a.py"];
    assert!(kept.reads.contains_key("lib.py"), "{kept:?}");
    // test_one keeps the row its reuse stands on; test_two has this run's.
    assert_eq!(kept.tests["test_one"], row(State::Passed));
    assert_eq!(kept.tests["test_two"].state, State::Passed);
    let counts = o.counts.as_ref().unwrap();
    assert_eq!((counts.total, counts.ran, counts.reused), (1, 1, 0));
    assert_eq!(
        counts.tests,
        Some(TestCounts {
            total: 2,
            ran: 1,
            reused: 1
        })
    );
    assert_eq!(counts.saved_ms, Some(7));

    // The same, with test_one deselected by something other than the plan: it didn't run, and
    // the file ran whole of what was asked, and passed: its own pass, and no test's.
    let (o, keyer) = decided(
        dir.path(),
        &tree,
        Seen {
            facts: Facts {
                complete: true,
                ..Facts::default()
            },
            reads: union.clone(),
            duration_ms: 3,
            tests: Some(BTreeMap::from([(
                "test_two".to_string(),
                facts::MergedTest {
                    finished: true,
                    ..facts::MergedTest::default()
                },
            )])),
        },
        &map,
        &Plan::default(),
    );
    assert_eq!(o.units["tests/test_a.py"].0, State::Passed);
    assert_eq!(o.records().len(), 1);
    assert_eq!(
        o.records()[0].key,
        keyer.key("tests/test_a.py", &union).unwrap()
    );
}

// covers: C-39
#[test]
fn what_one_test_didnt_follow_is_its_own_and_its_file_doesnt_pass_whole() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<String> = ["app.py", "tests/test_a.py"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let tree = tree(&paths);
    let spawned = units::NotFollowed {
        why: "subprocess".into(),
        detail: "git".into(),
    };
    let seen = Seen {
        facts: Facts {
            complete: true,
            ..Facts::default()
        },
        reads: paths.iter().cloned().collect(),
        duration_ms: 3,
        tests: Some(BTreeMap::from([
            (
                "test_spawns".to_string(),
                facts::MergedTest {
                    finished: true,
                    not_followed: BTreeSet::from([spawned.clone()]),
                    ..facts::MergedTest::default()
                },
            ),
            (
                "test_plain".to_string(),
                facts::MergedTest {
                    finished: true,
                    ..facts::MergedTest::default()
                },
            ),
        ])),
    };
    let (o, keyer) = decided(
        dir.path(),
        &tree,
        seen.clone(),
        &store::Map::default(),
        &Plan::default(),
    );
    assert_eq!(o.units["tests/test_a.py"].0, State::NotObserved);
    assert_eq!(
        o.tests["tests/test_a.py"]["test_spawns"],
        State::NotObserved
    );
    assert_eq!(o.tests["tests/test_a.py"]["test_plain"], State::Passed);
    let reads: BTreeSet<String> = paths.iter().cloned().collect();
    assert_eq!(
        o.records().iter().map(|r| r.key).collect::<Vec<_>>(),
        [keyer
            .test_key("tests/test_a.py", "test_plain", &reads)
            .unwrap()]
    );
    // The store keeps the file keyable, and the test that wasn't followed as such.
    let kept = &o.mapped["tests/test_a.py"];
    assert!(kept.not_followed.is_empty());
    assert_eq!(kept.tests["test_spawns"].state, State::NotObserved);
    // Outside every window, it is every test's: none records.
    let mut whole = seen;
    whole.facts.not_followed.insert(spawned);
    let (o, _) = decided(
        dir.path(),
        &tree,
        whole,
        &store::Map::default(),
        &Plan::default(),
    );
    assert!(o.records().is_empty());
    assert_eq!(o.tests["tests/test_a.py"]["test_plain"], State::NotObserved);
    assert!(!o.mapped["tests/test_a.py"].not_followed.is_empty());
}

// covers: C-39
#[test]
fn past_50000_tests_a_check_reuses_by_test_file() {
    let paths = vec!["tests/test_a.py".to_string(), "tests/test_b.py".to_string()];
    let tree = tree(&paths);
    let sets = units::classify(
        tree.paths(),
        &|p| p.starts_with("tests/"),
        units::Sources::Kinds(&[".py"]),
        &[],
    );
    let base = KeyBase {
        check: CheckName::new("unit").unwrap(),
        command: vec!["pytest".into()],
        cwd: String::new(),
        scope: ScopeName::new("default").unwrap(),
        external: None,
        counts_from: None,
    };
    let keyer = Keyer::new(&tree, &sets, &base);
    let row = store::TestRow {
        state: State::Passed,
        duration_ms: 1,
        not_followed: BTreeSet::new(),
        list: None,
    };
    let mut map = store::Map::default();
    for (unit, n) in [("tests/test_a.py", 25_000), ("tests/test_b.py", 25_001)] {
        map.units.insert(
            unit.into(),
            Mapped {
                tests: (0..n).map(|i| (format!("t{i}"), row.clone())).collect(),
                ..Mapped::default()
            },
        );
    }
    let keyed: Keyed = paths.iter().map(|p| (p.clone(), Vec::new())).collect();
    let results = Hints {
        refuse: Cell::new(false),
        sent: RefCell::new(Vec::new()),
    };
    let passed = BTreeMap::new();
    let plan = |map: &store::Map| {
        plan_tests(&TestsAt {
            keyer: &keyer,
            reads: Reads::File,
            tree: &tree,
            keyed: &keyed,
            map,
            shared: &Shared::default(),
            passed: &passed,
            store: &results,
            counts: &|_| true,
            started: "2026-11-11T12:00:00Z",
        })
        .map(|_| ())
        .map_err(|TooMany(n)| n)
    };
    assert_eq!(plan(&map), Err(50_001));
    // At the limit, it plans.
    map.units
        .get_mut("tests/test_b.py")
        .unwrap()
        .tests
        .remove("t0");
    assert_eq!(plan(&map), Ok(()));
}
