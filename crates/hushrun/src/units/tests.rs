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
            adapter: db::AdapterWord::Pytest,
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
