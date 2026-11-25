//! The fixtures of `reuse.rs`'s pytest tests, which its in-process tests and their end-to-end
//! ones (`e2e_pytest/reuse.rs`) share.

#![allow(dead_code)]

/// The repository every fixture's hushrun.yaml names.
pub const REPOSITORY: &str = "example.com/acme/shop";

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: {REPOSITORY}\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A shop: two modules each read by one test file, a greeting read by five, a test file that
/// starts a process, and a lockfile in the rest set.
pub fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (
            ".gitignore".into(),
            "__pycache__/\n.pytest_cache/\nflag\nflaky-seen\n".into(),
        ),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("requirements.txt".into(), "nothing==1.0\n".into()),
        ("app/__init__.py".into(), String::new()),
        ("app/pricing.py".into(), "def price(x):\n    return x * 2\n".into()),
        (
            "app/cart.py".into(),
            "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)\n"
                .into(),
        ),
        ("app/greet.py".into(), "def hello():\n    return 'hi'\n".into()),
        (
            "tests/test_cart.py".into(),
            "from app.cart import total\n\ndef test_total():\n    assert total([1, 2]) == 6\n".into(),
        ),
        (
            "tests/test_pricing.py".into(),
            "from app.pricing import price\n\ndef test_price():\n    assert price(3) == 6\n".into(),
        ),
        (
            "tests/test_child.py".into(),
            "import subprocess, sys\n\ndef test_child():\n    assert subprocess.run([sys.executable, '-c', 'pass']).returncode == 0\n".into(),
        ),
    ];
    for n in 1..=5 {
        files.push((
            format!("tests/test_greet_{n}.py"),
            "from app.greet import hello\n\ndef test_hello():\n    assert hello() == 'hi'\n".into(),
        ));
    }
    files
}
/// A test file that fails when a file outside the check's files exists: what no adapter can see,
/// the missed dependency an audit run is for; and another, read by its own module.
pub fn hidden(tests: &str) -> Vec<(String, String)> {
    vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (
            ".gitignore".into(),
            "__pycache__/\n.pytest_cache/\nflag\nseen\n".into(),
        ),
        ("liba.py".into(), "X = 1\n".into()),
        ("libb.py".into(), "Y = 1\n".into()),
        (
            "test_hidden.py".into(),
            "import os, liba\n\ndef test_x():\n    assert not os.path.exists(os.path.join(os.path.dirname(__file__), 'flag'))\n".into(),
        ),
        (
            "test_other.py".into(),
            "import libb\n\ndef test_y():\n    assert libb.Y == 1\n".into(),
        ),
    ]
}
