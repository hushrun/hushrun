//! `reads_test.rs`'s fixtures, shared by its in-process tests and its end-to-end ones
//! (`e2e_pytest/reads_test.rs`): a small pytest project, and the registry `reads: test` is known
//! to miss.

#![allow(dead_code)]

const REPOSITORY: &str = "example.com/acme/shop";

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: {REPOSITORY}\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// The project's files: `files` over a pytest project whose one other test file every change
/// runs, in a check whose `tests` are `tests`.
pub fn files_of(files: &[(&str, &str)], tests: &str) -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("app/__init__.py".into(), String::new()),
        // A test file every change runs, so that the check's own key misses.
        (
            "tests/test_other.py".into(),
            "def test_other():\n    assert True\n".into(),
        ),
    ];
    for (path, text) in files {
        all.retain(|(p, _)| p != path);
        all.push((path.to_string(), text.to_string()));
    }
    all
}

/// A registry plugins.py fills as app/__init__.py imports it: the test file imports the registry
/// and nothing of plugins.py, whose code runs at import only. An `import` statement binds no name
/// it re-exports (a `from app import plugins` in a package's `__init__.py` would, C-40).
pub fn a_registry() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/__init__.py", "import app.plugins\n"),
        ("app/registry.py", "HANDLERS = {}\n"),
        (
            "app/plugins.py",
            "from app.registry import HANDLERS\nHANDLERS['x'] = 41\n",
        ),
        (
            "tests/test_a.py",
            "from app.registry import HANDLERS\n\ndef test_target():\n    assert HANDLERS['x'] == 41\n",
        ),
    ]
}
