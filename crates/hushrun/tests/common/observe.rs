//! `tests: observe`'s fixtures for pytest, shared by the in-process tests (`units.rs`) and the
//! end-to-end ones (`e2e_pytest/observe.rs`).

#![allow(dead_code)]

/// hushrun.yaml of a check `unit` in `observe`.
pub const YAML_OBSERVE: &str =
    "version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: observe\n";

/// A test file whose last test asks for a fixture that ends the session: two of two tests started,
/// one finished, and another test file.
pub fn cut_short() -> Vec<(&'static str, &'static str)> {
    vec![
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "conftest.py",
            "import pytest\n\n@pytest.fixture\ndef db():\n    pytest.exit('database not reachable', returncode=3)\n",
        ),
        (
            "test_k.py",
            "def test_1():\n    pass\n\ndef test_db(db):\n    pass\n",
        ),
        ("test_z.py", "def test_z():\n    pass\n"),
    ]
}

/// A test file whose second test kills its pytest while `CRASH` is set.
pub fn runner_dies() -> Vec<(&'static str, &'static str)> {
    vec![
        ("hushrun.yaml", YAML_OBSERVE),
        (".gitignore", "__pycache__/\n.pytest_cache/\n"),
        (
            "test_x.py",
            "import os, signal\n\ndef test_1():\n    pass\n\ndef test_2():\n    if os.environ.get('CRASH'):\n        os.kill(os.getpid(), signal.SIGKILL)\n",
        ),
    ]
}

/// COMMAND, a shell line, that starts pytest twice on `test_x.py`, the second time with `CRASH`.
pub fn runner_dies_line(python: &str) -> String {
    let pytest = format!("{python} -m pytest -q -p no:cacheprovider test_x.py");
    format!("{pytest}; CRASH=1 {pytest}")
}
