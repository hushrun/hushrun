use std::cell::RefCell;

use super::*;
use crate::units::store::{Resumed, ReuseState};

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

fn tree(paths: &[&str]) -> Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut sorted: Vec<&str> = paths.to_vec();
    sorted.sort();
    for (n, path) in sorted.iter().enumerate() {
        bytes.extend_from_slice(format!("f {:064x} {path}", n + 1).as_bytes());
        bytes.push(0);
    }
    Tree::from_manifest(&bytes).expect("a manifest")
}

fn this() -> Os {
    Os::this().expect("a system")
}

/// A hint of this machine's system.
fn hint(units: &[(&str, &[&str])]) -> (Os, hints::Hint) {
    let hint = hints::Hint {
        adapter: "pytest".into(),
        units: units
            .iter()
            .map(|(unit, reads)| {
                (
                    unit.to_string(),
                    hints::Unit {
                        reads: set(reads),
                        dirs: BTreeSet::new(),
                    },
                )
            })
            .collect(),
        unit_files: BTreeSet::new(),
    };
    (this(), hint)
}

fn mapped(reads: &[&str], origin: Origin) -> Mapped {
    Mapped {
        reads: reads
            .iter()
            .map(|r| (r.to_string(), format!("f {}", "0".repeat(64))))
            .collect(),
        origin,
        ..Mapped::default()
    }
}

fn found(passed: bool) -> Found {
    Found {
        passed,
        duration_ms: 1,
    }
}

// covers: C-38
#[test]
fn a_test_files_lists_are_its_own_then_the_hints_newest_first_each_once_three_at_most() {
    let t = tree(&["t.py", "a.py", "b.py", "c.py", "d.py"]);
    let shared = Shared {
        hints: vec![
            hint(&[("t.py", &["t.py", "b.py"])]),
            hint(&[("t.py", &["t.py", "a.py"])]),
            hint(&[("t.py", &["t.py", "c.py"])]),
            hint(&[("t.py", &["t.py", "d.py"])]),
        ],
        received: Vec::new(),
        systems: vec![this()],
    };
    let own = mapped(&["t.py", "a.py"], Origin::SeenHere);
    let lists = lists("t.py", Some(&own), &shared, &t, None).expect("lists");
    let files: Vec<BTreeSet<String>> = lists.iter().map(|l| l.files.clone()).collect();
    assert_eq!(
        files,
        vec![
            set(&["t.py", "a.py"]),
            set(&["t.py", "b.py"]),
            set(&["t.py", "c.py"])
        ]
    );
    assert_eq!(lists[0].from, From::Here(Origin::SeenHere));
    assert!(!lists[0].seen_elsewhere());
    assert_eq!(lists[1].from, From::Hint(this()));
    // No entry here: the hints alone; none either: nothing to key it by.
    let lists_alone = super::lists("t.py", None, &shared, &t, None).expect("lists");
    assert_eq!(lists_alone[0].files, set(&["t.py", "b.py"]));
    assert_eq!(
        super::lists("other.py", None, &shared, &t, None),
        Some(Vec::new())
    );
    // A row another machine's list made is that machine's word.
    let kept = mapped(&["t.py", "b.py"], Origin::Shared(this()));
    assert!(super::lists("t.py", Some(&kept), &shared, &t, None).unwrap()[0].seen_elsewhere());
}

// covers: C-38, C-34
#[test]
fn an_entry_here_that_wasnt_followed_runs_whatever_the_hints_say() {
    let t = tree(&["t.py", "a.py"]);
    let shared = Shared {
        hints: vec![hint(&[("t.py", &["t.py", "a.py"])])],
        received: Vec::new(),
        systems: vec![this()],
    };
    let mut own = mapped(&["t.py"], Origin::SeenHere);
    own.not_followed.insert(units::NotFollowed {
        why: "subprocess".into(),
        detail: String::new(),
    });
    assert_eq!(lists("t.py", Some(&own), &shared, &t, None), None);
}

// covers: C-38, C-35
#[test]
fn for_go_every_list_holds_what_the_build_is_made_of_which_alone_stands_for_no_entry() {
    let t = tree(&[
        "data/data.go",
        "data/data_test.go",
        "pricing/pricing.go",
        "d/x.txt",
    ]);
    let compiled = set(&["data/data.go", "data/data_test.go"]);
    let mut seen = hint(&[("data", &["pricing/pricing.go"])]);
    seen.1.units.get_mut("data").unwrap().dirs = set(&["d"]);
    let shared = Shared {
        hints: vec![seen],
        received: Vec::new(),
        systems: vec![this()],
    };
    let lists = lists("data", None, &shared, &t, Some(&compiled)).expect("lists");
    assert_eq!(lists.len(), 2);
    assert_eq!(lists[0].from, From::Build);
    assert_eq!(lists[0].files, compiled);
    assert_eq!(
        lists[1].files,
        set(&[
            "data/data.go",
            "data/data_test.go",
            "pricing/pricing.go",
            "d/x.txt"
        ])
    );
}

// covers: C-38
#[test]
fn a_pass_reuses_only_when_no_list_has_a_fail() {
    let (a, b, c) = (Digest::of(b"a"), Digest::of(b"b"), Digest::of(b"c"));
    let mut results = BTreeMap::new();
    results.insert(a, found(true));
    assert_eq!(
        choose(&[None, Some(c), Some(a)], &results),
        Choice::Passed(2, a)
    );
    assert_eq!(choose(&[Some(c)], &results), Choice::Nothing);
    assert_eq!(choose(&[], &results), Choice::Nothing);
    results.insert(b, found(false));
    assert_eq!(choose(&[Some(a), Some(b)], &results), Choice::Failed);
    assert_eq!(choose(&[Some(b), Some(a)], &results), Choice::Failed);
}

/// A store that answers every key with a pass and counts its lookups.
struct Counting {
    pages: RefCell<Vec<usize>>,
    suspended_on: usize,
}

impl UnitResults for Counting {
    fn lookup(&self, keys: &[Digest], _: &dyn Fn(&RunResult) -> bool) -> Result<Lookup, String> {
        self.pages.borrow_mut().push(keys.len());
        let mut reuse = ReuseState::default();
        if self.pages.borrow().len() == self.suspended_on {
            reuse.state = crate::units::store::Reuse::Suspended;
        }
        Ok(Lookup {
            found: keys.iter().map(|k| (*k, found(true))).collect(),
            reuse,
        })
    }
    fn suspend(&self, _: &str, _: &Digest, _: &Digest, _: &str) -> Result<ReuseState, String> {
        unreachable!()
    }
    fn resume(&self, _: i64, _: &str) -> Result<Resumed, String> {
        unreachable!()
    }
}

// covers: C-38, C-34
#[test]
fn the_lookup_goes_in_pages_of_5000_keys_each_key_once() {
    let store = Counting {
        pages: RefCell::new(Vec::new()),
        suspended_on: 3,
    };
    let keys: Vec<Digest> = (0..12_000u32)
        .map(|n| Digest::of(&n.to_be_bytes()))
        .chain([Digest::of(&0u32.to_be_bytes())])
        .collect();
    let found = lookup(&store, keys, &|_| true).expect("found");
    assert_eq!(*store.pages.borrow(), vec![5_000, 5_000, 2_000]);
    assert_eq!(found.found.len(), 12_000);
    // A page that says the check is suspended says so for all.
    assert!(!found.reuse.allows_reuse());
    let store = Counting {
        pages: RefCell::new(Vec::new()),
        suspended_on: 0,
    };
    assert!(lookup(&store, [], &|_| true).unwrap().reuse.allows_reuse());
    assert_eq!(*store.pages.borrow(), vec![0]);
}

// covers: C-38, C-25
#[test]
fn a_machine_reads_its_own_systems_hints_or_those_counts_from_names() {
    let base = |counts_from: Option<serde_json::Value>| KeyBase {
        check: "unit".parse().unwrap(),
        command: vec!["pytest".into()],
        cwd: String::new(),
        scope: "default".parse().unwrap(),
        external: None,
        counts_from,
    };
    let this = Os::this().expect("a system");
    assert_eq!(systems(&base(None)), vec![this]);
    assert_eq!(
        systems(&base(Some(serde_json::json!({"go": ["1.25"]})))),
        vec![this]
    );
    // A rule that names this system and another: this system's own lists only (S3).
    let other = if this == Os::Linux {
        Os::Macos
    } else {
        Os::Linux
    };
    let both = serde_json::json!({"os": [this.word(), other.word()]});
    assert_eq!(systems(&base(Some(both))), vec![this]);
    // A rule that names only other systems: theirs.
    let others: Vec<Os> = [Os::Linux, Os::Macos, Os::Windows]
        .into_iter()
        .filter(|os| *os != this)
        .collect();
    let words: Vec<&str> = others.iter().map(|os| os.word()).collect();
    assert_eq!(
        systems(&base(Some(serde_json::json!({ "os": words })))),
        others
    );
}

// covers: C-38, C-36
#[test]
fn another_systems_list_kept_here_keys_nothing_once_no_rule_names_that_system() {
    let t = tree(&["t.py", "a.py", "b.py"]);
    let other = if this() == Os::Linux {
        Os::Macos
    } else {
        Os::Linux
    };
    let kept = mapped(&["t.py", "a.py"], Origin::Shared(other));
    // A rule named that system: its list is this machine's first.
    let reading_it = Shared {
        systems: vec![other],
        ..Shared::default()
    };
    let lists_then = lists("t.py", Some(&kept), &reading_it, &t, None).expect("lists");
    assert_eq!(lists_then.len(), 1);
    assert_eq!(lists_then[0].from, From::Here(Origin::Shared(other)));
    // The rule is gone: this machine reads its own system's, and the entry is as if it weren't.
    let own_system = Shared {
        hints: vec![hint(&[("t.py", &["t.py", "b.py"])])],
        received: Vec::new(),
        systems: vec![this()],
    };
    let lists_now = lists("t.py", Some(&kept), &own_system, &t, None).expect("lists");
    assert_eq!(lists_now.len(), 1, "{lists_now:?}");
    assert_eq!(lists_now[0].files, set(&["t.py", "b.py"]));
    assert_eq!(lists_now[0].from, From::Hint(this()));
    // This system's own shared list still counts.
    let ours = mapped(&["t.py", "a.py"], Origin::Shared(this()));
    let lists_ours = lists("t.py", Some(&ours), &own_system, &t, None).expect("lists");
    assert_eq!(lists_ours[0].from, From::Here(Origin::Shared(this())));
}
