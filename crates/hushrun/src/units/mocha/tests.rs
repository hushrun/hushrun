//! \[ADR 0107\] Mocha: which COMMANDs, and what its facts come to.

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

// covers: C-56
#[test]
fn one_run_of_mocha_is_recognized_however_it_starts() {
    for command in [
        "mocha",
        "mocha --recursive test/",
        "_mocha",
        "npx mocha",
        "npx --no-install mocha",
        "pnpm mocha",
        "pnpm exec mocha",
        "yarn mocha",
        "npm exec -- mocha",
        "node_modules/.bin/mocha --reporter dot",
        "node node_modules/mocha/bin/mocha.js",
        "node --import tsx node_modules/mocha/bin/mocha.js",
        "mocha --parallel=false",
    ] {
        assert!(
            matches!(recognize(&words(command)), Some(Ok(_))),
            "{command}"
        );
    }
    for refused in [
        "mocha --watch",
        "mocha -w",
        "mocha --parallel",
        "mocha -p",
        "mocha --dry-run",
        "mocha --list-reporters",
        "mocha --version",
        "npx mocha init",
    ] {
        assert!(
            matches!(recognize(&words(refused)), Some(Err(_))),
            "{refused}"
        );
    }
    for other in [
        "npm test",
        "nyc mocha",
        "node script.js",
        "jest",
        "vitest run",
    ] {
        assert!(recognize(&words(other)).is_none(), "{other}");
    }
}

// covers: C-56
#[test]
fn an_installed_mocha_hushrun_does_not_know_is_said() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("node_modules/mocha");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("package.json"), r#"{"version": "9.2.2"}"#).unwrap();
    let why = recognized(&words("npx mocha"), dir.path())
        .unwrap()
        .unwrap_err();
    assert!(why.contains("9.2.2"), "{why}");
    std::fs::write(package.join("package.json"), r#"{"version": "11.8.0"}"#).unwrap();
    assert!(recognized(&words("npx mocha"), dir.path()).unwrap().is_ok());
}

// covers: C-56, C-57
#[test]
fn its_one_file_comes_to_one_unit_for_each_spec_file() {
    let tree = tree_of(&[
        ".mocharc.cjs",
        "lib/a.js",
        "test/a.spec.js",
        "test/b.spec.js",
        "test/hooks.spec.js",
    ]);
    assert!(observe(&[], &tree).unwrap_err().contains("wrote nothing"));
    let file: FactsFile = serde_json::from_value(serde_json::json!({
        "schema": "hushrun.unit-facts/v1", "adapter": ADAPTER, "failuresByUnit": true,
        "settings": {"testFiles": ["test/a.spec.js", "test/b.spec.js"],
                     "always": ["test/hooks.spec.js"], "refused": null},
        "units": [
            {"unit": "test/a.spec.js", "selected": 2, "finished": 2,
             "reads": ["lib/a.js", "test/hooks.spec.js"], "dirs": [],
             "tests": [{"id": "a > one", "finished": true}, {"id": "a > two", "finished": true}]},
            {"unit": "test/b.spec.js", "deselected": true, "reads": ["test/hooks.spec.js"],
             "tests": [{"id": "b > one", "deselected": true}]},
        ],
    }))
    .unwrap();
    assert!(observe(&[file.clone(), file.clone()], &tree).is_err());
    let seen = observe(std::slice::from_ref(&file), &tree).unwrap();
    // A spec file that defines a root hook isn't a unit: it is a source every spec file reads.
    assert_eq!(
        seen.unit_files
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["test/a.spec.js", "test/b.spec.js"]
    );
    let a = &seen.units["test/a.spec.js"];
    assert!(a.facts.complete && !a.facts.failed);
    // Mocha's config of a code kind is read by every spec file.
    assert_eq!(
        a.reads.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            ".mocharc.cjs",
            "lib/a.js",
            "test/a.spec.js",
            "test/hooks.spec.js"
        ]
    );
    assert_eq!(a.tests.as_ref().unwrap().len(), 2);
    assert!(seen.units["test/b.spec.js"].facts.deselected);
    let mut broken = file;
    broken
        .errors
        .push("Mocha runs spec files in worker processes".into());
    assert!(observe(&[broken], &tree).unwrap_err().contains("worker"));
}
