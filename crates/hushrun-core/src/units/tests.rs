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
        let key = unit_key(
            &key_base,
            vector["unit"].as_str().unwrap(),
            &reads,
            &rest,
            &shape,
        )
        .unwrap();
        let expect = &vector["expect"];
        assert_eq!(reads.to_string(), expect["reads"], "{name}: reads");
        assert_eq!(rest.to_string(), expect["rest"], "{name}: rest");
        assert_eq!(shape.to_string(), expect["shape"], "{name}: shape");
        assert_eq!(key.to_string(), expect["key"], "{name}: key");
    }
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
