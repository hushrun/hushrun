//! \[ADR 0107\] Node's test runner: which COMMANDs, how it loads, and what its facts come to.

use super::*;

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

fn tree_of(paths: &[&str]) -> Tree {
    let mut bytes = format!("{}\n", hushrun_core::CANONICALIZATION).into_bytes();
    let mut paths = paths.to_vec();
    paths.sort();
    for path in paths {
        bytes.extend(format!("f {} {path}\0", "0".repeat(64)).as_bytes());
    }
    Tree::from_manifest(&bytes).unwrap()
}

fn facts(value: serde_json::Value) -> FactsFile {
    serde_json::from_value(value).unwrap()
}

// covers: C-54
#[test]
fn node_with_test_among_its_own_options_is_the_runner() {
    for command in [
        "node --test",
        "nodejs --test",
        "node --test test/",
        "node --test test/**/*.test.js",
        "node --test-reporter spec --test",
        "node --test-reporter=dot --test --test-concurrency 2",
        "node --import tsx --test",
        "node -r ./setup.js --test",
        "node --test --test-shard=1/2",
        "node --test --test-isolation=process",
        "/usr/local/bin/node --test",
    ] {
        assert!(
            matches!(recognize(&words(command)), Some(Ok(_))),
            "{command}"
        );
    }
    assert!(
        recognize(&words("node --test --experimental-test-coverage"))
            .unwrap()
            .unwrap()
            .coverage
    );
    for refused in [
        "node --test --watch",
        "node --watch --test",
        "node --test --test-isolation=none",
        "node --test --experimental-test-isolation=none",
        "node --test --test-isolation none",
    ] {
        assert!(
            matches!(recognize(&words(refused)), Some(Err(_))),
            "{refused}"
        );
    }
    for other in [
        "node",
        "node script.js --test",
        "node -e 1",
        "node --test-reporter --test",
        "npm test",
        "deno test",
        "bun test",
        "jest",
    ] {
        assert!(recognize(&words(other)).is_none(), "{other}");
    }
}

// covers: C-54
#[test]
fn it_loads_by_node_options_keeping_what_they_had() {
    let dir = Path::new("/c/adapters/node-test-0");
    let vars = variables(dir, Some("--max-old-space-size=4096".into())).unwrap();
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0].0, "NODE_OPTIONS");
    assert_eq!(
        vars[0].1,
        OsString::from(
            "--max-old-space-size=4096 --require /c/adapters/node-test-0/hushrun-node-test.cjs"
        )
    );
    let spaced = require(Path::new("/my cache/a.cjs"), None).unwrap();
    assert_eq!(spaced, OsString::from("--require \"/my cache/a.cjs\""));
    assert!(require(Path::new("/a\"b/a.cjs"), None).is_err());
    let cache = tempfile::tempdir().unwrap();
    let installed = install(cache.path()).unwrap();
    let text = std::fs::read_to_string(installed.join("hushrun-node-test.cjs")).unwrap();
    assert_eq!(text, PRELOAD);
    assert_eq!(install(cache.path()).unwrap(), installed);
}

// covers: C-54, C-55
#[test]
fn the_main_process_and_each_test_files_process_come_to_one_unit_each() {
    let tree = tree_of(&[
        "lib/a.js",
        "lib/b.mjs",
        "test/a.test.js",
        "test/b.test.js",
        "test/c.test.mjs",
        "test/d.test.js",
        "fixtures/x.json",
    ]);
    assert!(observe(&[], &tree).unwrap_err().contains("wrote nothing"));
    let main = facts(serde_json::json!({
        "schema": "hushrun.unit-facts/v1", "adapter": MAIN, "failuresByUnit": true, "start": "s",
        "settings": {"testFiles": ["test/a.test.js", "test/b.test.js", "test/c.test.mjs", "test/d.test.js"],
                     "refused": null, "unowned": ["test/d.test.js"]},
        "units": [
            {"unit": "test/a.test.js", "selected": 1, "finished": 1},
            {"unit": "test/b.test.js", "selected": 1, "finished": 1, "failed": true},
            {"unit": "test/c.test.mjs", "deselected": true},
            {"unit": "test/d.test.js", "selected": 1, "finished": 1, "failed": true},
        ],
    }));
    let child = |unit: &str, reads: serde_json::Value, failed: bool| {
        facts(serde_json::json!({
            "schema": "hushrun.unit-facts/v1", "adapter": ADAPTER, "failuresByUnit": true,
            "start": "s",
            "units": [{"unit": unit, "selected": 1, "finished": 1, "failed": failed,
                       "reads": reads, "dirs": ["fixtures"],
                       "tests": [{"id": "t", "finished": true, "failed": failed}]}],
        }))
    };
    let a = child(
        "test/a.test.js",
        serde_json::json!(["lib/a.js", "lib/gone.js"]),
        false,
    );
    let d = child("test/d.test.js", serde_json::json!([]), false);
    // A child's facts without the main process's: the runner stopped before its main wrote.
    assert!(observe(std::slice::from_ref(&a), &tree).is_err());
    assert!(observe(&[main.clone(), main.clone()], &tree).is_err());
    let mut broken = main.clone();
    broken
        .errors
        .push("this Node, v20.20.2, has no fs.globSync".into());
    assert!(observe(&[broken], &tree).unwrap_err().contains("v20.20.2"));
    let seen = observe(&[main, a, d], &tree).unwrap();
    assert_eq!(seen.unit_files.len(), 4);
    let a = &seen.units["test/a.test.js"];
    assert!(a.facts.complete && !a.facts.failed && a.facts.not_followed.is_empty());
    // What it read, a path no longer in the scope left out, and below the directory it listed.
    assert_eq!(
        a.reads.iter().map(String::as_str).collect::<Vec<_>>(),
        ["fixtures/x.json", "lib/a.js", "test/a.test.js"]
    );
    // The runner ran it and its process never said what it read: not observed, not complete.
    let b = &seen.units["test/b.test.js"];
    assert!(!b.facts.complete && b.facts.failed);
    assert!(b.facts.not_followed.iter().any(|n| n.why == "unseen"));
    assert!(seen.units["test/c.test.mjs"].facts.deselected);
    // [C-55] Its process failed outside its tests: its tests keep nothing, with why.
    let d = &seen.units["test/d.test.js"];
    assert!(d.facts.failed && !d.facts.complete);
    let t = &d.tests.as_ref().unwrap()["t"];
    assert!(!t.finished && !t.failed);
    assert_eq!(t.not_kept.as_deref(), Some(UNOWNED));
}
