use std::collections::BTreeSet;

use serde_json::Value;

use super::*;
use crate::glob::Pattern;

fn globs(patterns: &[&str]) -> Vec<Pattern> {
    patterns.iter().map(|p| Pattern::new(p).unwrap()).collect()
}

// covers: C-30
#[test]
fn every_file_of_the_scope_is_in_exactly_one_set() {
    let scope = [
        "app/settings.py",
        "conftest.py",
        "docs/adr/0001.md",
        "requirements.txt",
        "tests/test_a.py",
        "tests/data.txt",
        "native/ext.rs",
    ];
    let is_unit = |path: &str| path.starts_with("tests/test_") && path.ends_with(".py");
    let sets = classify(
        scope,
        &is_unit,
        Sources::Kinds(&[".py"]),
        &globs(&["docs/**"]),
    );
    assert_eq!(sets.of("tests/test_a.py"), Some(Set::Units));
    assert_eq!(sets.of("app/settings.py"), Some(Set::Sources));
    assert_eq!(sets.of("conftest.py"), Some(Set::Sources));
    assert_eq!(sets.of("requirements.txt"), Some(Set::Rest));
    assert_eq!(sets.of("tests/data.txt"), Some(Set::Rest));
    // A native source isn't a kind pytest observes: a change to it runs everything.
    assert_eq!(sets.of("native/ext.rs"), Some(Set::Rest));
    assert_eq!(sets.of("docs/adr/0001.md"), Some(Set::Ignored));
    assert_eq!(sets.of("elsewhere.py"), None);
    let all: usize = [Set::Units, Set::Sources, Set::Rest, Set::Ignored]
        .iter()
        .map(|set| sets.files(*set).count())
        .sum();
    assert_eq!(all, scope.len());
    // `ignore` wins over the unit rule.
    let sets = classify(
        ["tests/test_a.py"],
        &is_unit,
        Sources::Kinds(&[".py"]),
        &globs(&["tests/**"]),
    );
    assert_eq!(sets.of("tests/test_a.py"), Some(Set::Ignored));
}

// covers: C-30
#[test]
fn a_compiler_given_adapters_sources_are_the_rs_files_a_build_names() {
    let named: BTreeSet<String> = [
        "crates/core/src/lib.rs",
        "crates/core/src/util.rs",
        "crates/core/greeting.txt",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let units = ["crates/core/src/lib.rs"];
    let sets = classify(
        [
            "crates/core/src/lib.rs",
            "crates/core/src/util.rs",
            "crates/core/greeting.txt",
            "testdata/fixture/main.rs",
            "Cargo.lock",
        ],
        &|path| units.contains(&path),
        Sources::Named(&named, &[".rs"]),
        &[],
    );
    assert_eq!(sets.of("crates/core/src/lib.rs"), Some(Set::Units));
    assert_eq!(sets.of("crates/core/src/util.rs"), Some(Set::Sources));
    // An include_str! target: a build names it, and a test may open it too.
    assert_eq!(sets.of("crates/core/greeting.txt"), Some(Set::Rest));
    // A .rs file no build names is data some test may open (M1's hole): the rest set.
    assert_eq!(sets.of("testdata/fixture/main.rs"), Some(Set::Rest));
    assert_eq!(sets.of("Cargo.lock"), Some(Set::Rest));
    // What a key commits to of what a unit read.
    let keyed = sets.keyed([
        "crates/core/src/lib.rs",
        "crates/core/src/util.rs",
        "crates/core/greeting.txt",
        "outside/scope.rs",
    ]);
    assert_eq!(
        keyed.into_iter().collect::<Vec<_>>(),
        ["crates/core/src/lib.rs", "crates/core/src/util.rs"]
    );
}

fn tree(files: &[(&str, &str, &str)]) -> Tree {
    let mut entries: Vec<(&str, &str, &str)> = files.to_vec();
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut bytes = format!("{}\n", crate::CANONICALIZATION).into_bytes();
    for (path, kind, text) in entries {
        bytes.extend_from_slice(
            format!("{kind} {} {path}", Digest::of(text.as_bytes()).hex()).as_bytes(),
        );
        bytes.push(0);
    }
    Tree::from_manifest(&bytes).expect("a manifest")
}

/// The vectors `tools/refdigest.py unit-keys` checks independently.
// covers: C-31
#[test]
fn unit_keys_match_the_golden_vectors() {
    let doc: Value =
        serde_json::from_str(include_str!("../../../../testdata/digest/unit-keys.json")).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();
    assert!(vectors.len() >= 4);
    for vector in vectors {
        let name = vector["name"].as_str().unwrap();
        let strings = |field: &str| -> Vec<String> {
            vector[field]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        let files: Vec<(String, String, String)> = vector["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_string(),
                    f["kind"].as_str().unwrap().to_string(),
                    f["text"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str, &str)> = files
            .iter()
            .map(|(p, k, t)| (p.as_str(), k.as_str(), t.as_str()))
            .collect();
        let tree = tree(&borrowed);
        let units = strings("units");
        let sources = strings("sources");
        let sets = classify(
            tree.paths().collect::<Vec<_>>(),
            &|path| units.iter().any(|u| u == path),
            Sources::Named(&sources.iter().cloned().collect(), &[""]),
            &[],
        );
        let rest: Vec<&str> = sets.files(Set::Rest).collect();
        assert_eq!(
            rest,
            strings("rest")
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            "{name}"
        );
        let reads_list = strings("reads");
        let reads = tree.digest(sets.keyed(reads_list.iter().map(String::as_str)));
        let rest = tree.digest(rest);
        let shape = shape(sets.files(Set::Sources));
        let base = &vector["base"];
        let key_base = KeyBase {
            check: CheckName::new(base["check"].as_str().unwrap()).unwrap(),
            command: base["command"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect(),
            cwd: base["cwd"].as_str().unwrap().to_string(),
            scope: ScopeName::new(base["scope"].as_str().unwrap()).unwrap(),
            external: base.get("external").cloned(),
            counts_from: base.get("counts_from").cloned(),
        };
        let unit = vector["unit"].as_str().unwrap();
        // \[ADR 0077\] A vector that names a test is a test's key (C-39); the others, a unit's.
        let key = match vector.get("test").and_then(Value::as_str) {
            Some(test) => test_key(&key_base, unit, test, &reads, &rest, &shape).unwrap(),
            None => unit_key(&key_base, unit, &reads, &rest, &shape).unwrap(),
        };
        let expect = &vector["expect"];
        assert_eq!(reads.to_string(), expect["reads"], "{name}: reads");
        assert_eq!(rest.to_string(), expect["rest"], "{name}: rest");
        assert_eq!(shape.to_string(), expect["shape"], "{name}: shape");
        assert_eq!(key.to_string(), expect["key"], "{name}: key");
    }
}

// covers: C-39
#[test]
fn a_tests_key_is_its_unit_keys_fields_and_its_id_and_never_a_units_key() {
    let base = KeyBase {
        check: CheckName::new("unit").unwrap(),
        command: vec!["pytest".into()],
        cwd: String::new(),
        scope: ScopeName::new("default").unwrap(),
        external: None,
        counts_from: None,
    };
    let (reads, rest, shape) = (Digest::of(b"r"), Digest::of(b"s"), Digest::of(b"t"));
    let file = unit_key(&base, "tests/test_a.py", &reads, &rest, &shape).unwrap();
    let a = test_key(&base, "tests/test_a.py", "test_x", &reads, &rest, &shape).unwrap();
    let b = test_key(&base, "tests/test_a.py", "test_y", &reads, &rest, &shape).unwrap();
    // A test's key is never its file's, nor another test's of the same file and files.
    assert_ne!(a, file);
    assert_ne!(a, b);
    // An id that looks like a path doesn't make another unit's test the same key.
    let c = test_key(&base, "tests/test_a.py::x", "y", &reads, &rest, &shape).unwrap();
    let d = test_key(&base, "tests/test_a.py", "x::y", &reads, &rest, &shape).unwrap();
    assert_ne!(c, d);
    // Every byte a key holds of what it read moves it.
    let other = test_key(&base, "tests/test_a.py", "test_x", &shape, &rest, &shape).unwrap();
    assert_ne!(a, other);
    assert_eq!(
        test_name("tests/test_a.py", "TestA::test_x[1]"),
        "tests/test_a.py::TestA::test_x[1]"
    );
    assert_eq!(MOST_TESTS, 50_000);
}

// covers: C-31
#[test]
fn shape_is_the_sorted_paths_each_with_a_nul_and_an_empty_rest_is_the_header() {
    // Sorted by bytes, deduplicated, each followed by NUL.
    assert_eq!(shape(["b.py", "a.py", "b.py"]), Digest::of(b"a.py\0b.py\0"));
    assert_eq!(shape([]), Digest::of(b""));
    let empty = Tree::default();
    assert_eq!(empty.digest([]), Digest::of(b"hushrun-source-v1\n"));
    // A path the tree doesn't hold is left out of a digest, not an error.
    let t = tree(&[("a.py", "f", "x\n")]);
    assert_eq!(t.digest(["a.py", "gone.py"]), t.digest(["a.py"]));
    assert_ne!(t.digest(["a.py"]), t.digest([]));
    // Contents change reads, never shape.
    let t2 = tree(&[("a.py", "f", "y\n")]);
    assert_ne!(t.digest(["a.py"]), t2.digest(["a.py"]));
    assert!(Tree::from_manifest(b"not a manifest").is_none());
}

fn facts(complete: bool, failed: bool, not_followed: bool) -> Facts {
    let mut facts = Facts {
        complete,
        failed,
        ..Facts::default()
    };
    if not_followed {
        facts.not_followed.insert(NotFollowed {
            why: "subprocess".into(),
            detail: "python".into(),
        });
    }
    facts
}

const PASSED: RunFacts = RunFacts {
    command_passed: true,
    failures_by_unit: true,
    planned: false,
};

// covers: C-32
#[test]
fn a_units_state_is_decided_once_from_its_facts() {
    assert_eq!(state(&facts(true, false, false), &PASSED), State::Passed);
    assert_eq!(state(&facts(true, true, false), &PASSED), State::Failed);
    // A failure is a failure, even when the runner stopped inside it or something wasn't followed.
    assert_eq!(state(&facts(false, true, true), &PASSED), State::Failed);
    assert_eq!(
        state(&facts(true, false, true), &PASSED),
        State::NotObserved
    );
    assert_eq!(state(&facts(false, false, false), &PASSED), State::NotRun);
    assert_eq!(state(&facts(false, false, true), &PASSED), State::NotRun);
    // COMMAND failed: a pass holds only when the adapter reports each failure by unit.
    let failed_run = RunFacts {
        command_passed: false,
        failures_by_unit: false,
        planned: false,
    };
    assert_eq!(
        state(&facts(true, false, false), &failed_run),
        State::NotRun
    );
    let attributed = RunFacts {
        failures_by_unit: true,
        ..failed_run
    };
    assert_eq!(
        state(&facts(true, false, false), &attributed),
        State::Passed
    );
    // Deselected: reused when the plan listed it, else nothing ran.
    let deselected = Facts {
        deselected: true,
        ..Facts::default()
    };
    assert_eq!(state(&deselected, &PASSED), State::NotRun);
    let planned = RunFacts {
        planned: true,
        ..PASSED
    };
    assert_eq!(state(&deselected, &planned), State::Reused);
    // What each records.
    assert_eq!(State::Passed.recorded(), Some(true));
    assert_eq!(State::Failed.recorded(), Some(false));
    for nothing in [State::Reused, State::NotObserved, State::NotRun] {
        assert_eq!(nothing.recorded(), None, "{nothing:?}");
    }
}

// covers: C-39
#[test]
fn a_tests_state_comes_from_its_own_facts_and_a_file_partly_reused_didnt_pass_whole() {
    // A test: the same function, from what happened inside its own window.
    assert_eq!(state(&facts(true, false, false), &PASSED), State::Passed);
    assert_eq!(
        state(&facts(true, false, true), &PASSED),
        State::NotObserved
    );
    assert_eq!(state(&facts(false, false, false), &PASSED), State::NotRun);
    // A test file some of whose tests were reused one by one: not run whole, so no pass of its
    // own; a failure of one of its tests is still its failure.
    let partly = Facts {
        tests_reused: true,
        ..facts(true, false, false)
    };
    assert_eq!(state(&partly, &PASSED), State::NotRun);
    assert_eq!(State::NotRun.recorded(), None);
    let partly_failed = Facts {
        tests_reused: true,
        ..facts(true, true, false)
    };
    assert_eq!(state(&partly_failed, &PASSED), State::Failed);
    let words = |unit: Unit, reads: Reads| (unit.word(), reads.word());
    assert_eq!(words(Unit::default(), Reads::default()), ("file", "file"));
    assert_eq!(Unit::from_word("test"), Some(Unit::Test));
    assert_eq!(Reads::from_word("test"), Some(Reads::Test));
    assert_eq!(Unit::from_word("case"), None);
}

// covers: C-32, C-39
#[test]
fn a_skip_in_a_run_whose_command_failed_proves_nothing() {
    let skipped = Facts {
        skipped: true,
        ..facts(true, false, false)
    };
    // COMMAND passed: a skip is a pass, as before.
    assert_eq!(state(&skipped, &PASSED), State::Passed);
    // COMMAND failed, though the adapter reports each failure by unit: not run, nothing recorded.
    let failed_run = RunFacts {
        command_passed: false,
        ..PASSED
    };
    assert_eq!(state(&skipped, &failed_run), State::NotRun);
    assert_eq!(
        state(&facts(true, false, false), &failed_run),
        State::Passed
    );
    // A failure is still a failure.
    let skipped_failed = Facts {
        failed: true,
        ..skipped.clone()
    };
    assert_eq!(state(&skipped_failed, &failed_run), State::Failed);
    // A test of a test file that failed with no test failing: not run, whatever COMMAND did.
    let unexplained = Facts {
        unexplained: true,
        ..facts(true, false, false)
    };
    assert_eq!(state(&unexplained, &PASSED), State::NotRun);
    assert_eq!(state(&unexplained, &failed_run), State::NotRun);
}

// covers: C-5
#[test]
fn modes_read_as_written() {
    for mode in [Mode::Off, Mode::Observe, Mode::Reuse] {
        assert_eq!(Mode::from_word(mode.word()), Some(mode));
    }
    assert_eq!(Mode::from_word("on"), None);
    let tests = Tests {
        mode: Mode::Observe,
        ignore: Vec::new(),
        always: globs(&["xtask/**"]),
        ..Tests::default()
    };
    assert!(tests.is_on());
    assert!(tests.always_runs("xtask/src/main.rs"));
    assert!(!tests.always_runs("crates/a/src/lib.rs"));
    assert!(!Tests::default().is_on());
}

// covers: C-32
#[test]
fn a_failure_outranks_a_deselection_the_plan_listed() {
    // One process (an xdist worker that may deselect) reports the unit deselected; another, which
    // may not, ran it and saw it fail. Reused would hide the failure: a failure is a failure.
    let both = Facts {
        deselected: true,
        failed: true,
        complete: true,
        ..Facts::default()
    };
    let planned = RunFacts {
        planned: true,
        ..PASSED
    };
    assert_eq!(state(&both, &planned), State::Failed);
    assert_eq!(state(&both, &PASSED), State::Failed);
}

// covers: C-34
#[test]
fn audits_take_two_percent_at_least_one_chosen_by_key_and_start() {
    assert_eq!(audit_share(0), 0);
    assert_eq!(audit_share(1), 1);
    assert_eq!(audit_share(50), 1);
    assert_eq!(audit_share(51), 2);
    assert_eq!(audit_share(328), 7);
    let keys: Vec<(String, Digest)> = (0..120)
        .map(|n| {
            (
                format!("tests/test_{n}.py"),
                Digest::of(format!("{n}").as_bytes()),
            )
        })
        .collect();
    let pick = |started: &str| audited(keys.iter().map(|(u, k)| (u.as_str(), k)), started);
    let one = pick("2026-11-10T12:00:00.000Z");
    assert_eq!(one.len(), 3);
    // The same run picks the same; another start picks others.
    assert_eq!(one, pick("2026-11-10T12:00:00.000Z"));
    assert_ne!(one, pick("2026-11-10T12:00:00.001Z"));
    assert!(audited([], "x").is_empty());
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

// covers: C-40
#[test]
fn a_tests_list_is_what_ran_what_that_imports_directly_and_what_it_opened() {
    let file = set(&[
        "tests/test_a.py",
        "app/cart.py",
        "app/settings.py",
        "app/defaults.py",
    ]);
    let none = BTreeSet::new();
    let imports = BTreeMap::from([
        (
            "tests/test_a.py".to_string(),
            set(&["app/cart.py", "app/greet.py"]),
        ),
        ("app/cart.py".to_string(), set(&["app/settings.py"])),
        ("app/settings.py".to_string(), set(&["app/defaults.py"])),
        ("app/fixtures.py".to_string(), set(&["app/db.py"])),
    ]);
    let opens = BTreeMap::from([("app/prices.py".to_string(), set(&["prices.json"]))]);
    let (loaded_ran, loaded_opened) = (set(&["app/registry.py"]), set(&["schema.json"]));
    let every = set(&["conftest.py"]);
    let shared = vec![(
        set(&["test_x"]),
        set(&["app/fixtures.py"]),
        set(&["fixture.json"]),
    )];
    let no_reexports = BTreeMap::new();
    let watch = FileWatch {
        unit: "tests/test_a.py",
        file: &file,
        file_dirs: &none,
        imports: &imports,
        reexports: &no_reexports,
        opens: &opens,
        loaded: Some((&loaded_ran, &loaded_opened)),
        every: &every,
        shared: &shared,
    };
    let (ran, opened, dirs) = (
        set(&["app/cart.py", "app/prices.py"]),
        set(&["data.txt"]),
        set(&["tests/data"]),
    );
    let test = |id| TestWatch {
        id,
        ran: Some(&ran),
        opened: Some(&opened),
        dirs: Some(&dirs),
    };
    let list = test_list(Reads::Test, &watch, &test("test_x"));
    assert!(list.own);
    assert_eq!(
        list.files,
        set(&[
            // 1. its own file, and 3. what that imports directly
            "tests/test_a.py",
            "app/greet.py",
            // 2. what ran, 3. what that imports directly, and 4. what it opened and kept
            "app/cart.py",
            "app/settings.py",
            "app/prices.py",
            "prices.json",
            // 4. what the test opened
            "data.txt",
            // what ran and was opened while its file was loaded
            "app/registry.py",
            "schema.json",
            // what the runner loads for every test
            "conftest.py",
            // the shared setup that served it: what it ran, imports and opened
            "app/fixtures.py",
            "app/db.py",
            "fixture.json",
        ])
    );
    assert_eq!(list.dirs, dirs);
    // Two imports away with no function of the way called: a known miss of `reads: test`.
    assert!(!list.files.contains("app/defaults.py"));
    // A shared setup that didn't serve it isn't its.
    let other = test_list(Reads::Test, &watch, &test("test_y"));
    assert!(!other.files.contains("app/fixtures.py"));
    // `reads: file`, a test not watched alone, or a file whose loading wasn't said: its file's.
    let whole = TestList {
        files: file.clone(),
        dirs: none.clone(),
        own: false,
    };
    assert_eq!(test_list(Reads::File, &watch, &test("test_x")), whole);
    let unwatched = TestWatch {
        ran: None,
        ..test("test_x")
    };
    assert_eq!(test_list(Reads::Test, &watch, &unwatched), whole);
    let no_loaded = FileWatch {
        loaded: None,
        ..watch
    };
    assert_eq!(test_list(Reads::Test, &no_loaded, &test("test_x")), whole);
}

// covers: C-40
#[test]
fn a_tests_list_follows_reexports_to_the_end() {
    // The test calls a function of index.ts, which re-exports everything of api.ts, which
    // re-exports impl.ts's `run`: no function of api.ts is called, and impl.ts isn't imported
    // directly by anything that ran. Its list holds both, and never what impl.ts merely imports.
    let file = set(&["test/a.test.ts", "src/index.ts", "src/api.ts", "src/impl.ts"]);
    let none = BTreeSet::new();
    let imports = BTreeMap::from([
        ("test/a.test.ts".to_string(), set(&["src/index.ts"])),
        ("src/index.ts".to_string(), set(&["src/api.ts"])),
        ("src/api.ts".to_string(), set(&["src/impl.ts"])),
        ("src/impl.ts".to_string(), set(&["src/deep.ts"])),
    ]);
    let reexports = BTreeMap::from([
        ("src/index.ts".to_string(), set(&["src/api.ts"])),
        ("src/api.ts".to_string(), set(&["src/impl.ts"])),
        // A cycle ends.
        ("src/impl.ts".to_string(), set(&["src/index.ts"])),
    ]);
    let opens = BTreeMap::new();
    let (loaded_ran, loaded_opened) = (BTreeSet::new(), BTreeSet::new());
    let watch = FileWatch {
        unit: "test/a.test.ts",
        file: &file,
        file_dirs: &none,
        imports: &imports,
        reexports: &reexports,
        opens: &opens,
        loaded: Some((&loaded_ran, &loaded_opened)),
        every: &none,
        shared: &[],
    };
    let ran = set(&["src/index.ts"]);
    let list = test_list(
        Reads::Test,
        &watch,
        &TestWatch {
            id: "a",
            ran: Some(&ran),
            opened: Some(&none),
            dirs: Some(&none),
        },
    );
    assert!(list.own);
    assert_eq!(
        list.files,
        set(&["test/a.test.ts", "src/index.ts", "src/api.ts", "src/impl.ts"])
    );
}
