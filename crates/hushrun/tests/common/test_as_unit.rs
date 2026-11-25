//! The fixtures of the tests of a test as the unit, shared by the in-process ones
//! (`test_as_unit.rs`) and the end-to-end ones (`e2e_pytest/test_as_unit.rs`): the shop, the
//! runners that write pytest's facts themselves, and a test file of 6,000 tests.

#![allow(dead_code)]

use serde_json::{Value, json};

/// The repository the fixtures' `hushrun.yaml` names.
pub const REPOSITORY: &str = "example.com/acme/shop";

/// Owned files, from borrowed ones.
pub fn files(files: &[(&str, &str)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect()
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: {REPOSITORY}\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A cart with six tests, one of which fails while `FAIL` is set; a test file whose one test starts
/// a process while three don't; and three small test files of one test each.
pub fn shop(tests: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (
            ".gitignore".into(),
            "__pycache__/\n.pytest_cache/\n".into(),
        ),
        (
            "pytest.ini".into(),
            "[pytest]\ntestpaths = tests\npythonpath = .\n".into(),
        ),
        ("requirements.txt".into(), "nothing==1.0\n".into()),
        ("app/__init__.py".into(), String::new()),
        ("app/cart.py".into(), "def total(xs):\n    return sum(xs)\n".into()),
        ("app/greet.py".into(), "def hello():\n    return 'hi'\n".into()),
        (
            "tests/test_cart.py".into(),
            "import os\nimport pytest\nfrom app.cart import total\n\n\
             def test_one():\n    assert total([1]) == 1\n\n\
             def test_two():\n    assert total([1, 1]) == 2\n\n\
             def test_flips():\n    assert not os.environ.get('FAIL')\n\n\
             class TestMore:\n    def test_three(self):\n        assert total([3]) == 3\n\n\
             @pytest.mark.parametrize('n', [4, 5])\ndef test_n(n):\n    assert total([n]) == n\n"
                .into(),
        ),
        (
            "tests/test_child.py".into(),
            "import subprocess, sys\nfrom app.cart import total\n\n\
             def test_spawns():\n    assert subprocess.run([sys.executable, '-c', 'pass']).returncode == 0\n\n\
             def test_a():\n    assert total([1]) == 1\n\n\
             def test_b():\n    assert total([2]) == 2\n\n\
             def test_c():\n    assert total([3]) == 3\n"
                .into(),
        ),
    ];
    for n in 1..=3 {
        files.push((
            format!("tests/test_greet_{n}.py"),
            "from app.greet import hello\n\ndef test_hello():\n    assert hello() == 'hi'\n".into(),
        ));
    }
    files
}

/// What `hushrun why` says of a test skipped in a run that failed (C-37).
pub const SKIPPED_IN_A_FAILED_RUN: &str = "it was skipped in a run that failed, and a skip there proves nothing: what failed may have skipped what it hides";

/// A runner that writes an adapter's facts itself, as pytest's plugin would: each test file
/// of `FAKE_FILES` ran whole, and in the starts `FAKE_LEFT_OUT` names (by their number across
/// every run, from 1) it says it left out what the plan didn't list. Each start keeps the
/// `hushrun.json` it saw in `FAKE_STATE`, and exits with `FAKE_EXIT_<n>`, else 0.
pub const FAKE_RUNNER: &str = r#"import json, os, sys
d = os.environ["HUSHRUN_UNITS"]
with open(os.path.join(d, "hushrun.json")) as f:
    run = json.load(f)
state = os.environ["FAKE_STATE"]
n = len(os.listdir(state)) + 1
with open(os.path.join(state, str(n)), "w") as f:
    json.dump(run, f)
units = [{"unit": u, "selected": 1, "finished": 1, "reads": [u]}
         for u in os.environ["FAKE_FILES"].split()]
facts = {"schema": "hushrun.unit-facts/v0", "adapter": "pytest", "failuresByUnit": True,
         "settings": {"pythonFiles": ["test_*.py", "*_test.py"]}, "units": units}
if str(n) in os.environ.get("FAKE_LEFT_OUT", "").split():
    facts["leftOutUnlisted"] = "2 features of test_a.py nobody listed"
if os.environ.get("FAKE_STALE"):
    facts["staleBuild"] = os.environ["FAKE_STALE"]
with open(os.path.join(d, "fake-%d.json" % n), "w") as f:
    json.dump(facts, f)
sys.exit(int(os.environ.get("FAKE_EXIT_%d" % n, "0")))
"#;

/// A runner that writes pytest's facts itself with tests as the unit, and fails: `test_a.py`
/// failed while each of its tests passed, as a failure no test explains would say (a failing
/// `teardown_module`, a crash between tests); with `FAKE_TWO_FAILS`, its `test_two` failed; with
/// `FAKE_STALE`, it ran on a stale build.
pub const UNEXPLAINED_RUNNER: &str = r#"import json, os, sys
d = os.environ["HUSHRUN_UNITS"]
two = bool(os.environ.get("FAKE_TWO_FAILS"))
tests = [{"id": "test_one", "finished": True}, {"id": "test_two", "finished": True, "failed": two}]
units = [{"unit": "test_a.py", "selected": 2, "finished": 2, "failed": True,
          "reads": ["test_a.py"], "tests": tests}]
facts = {"schema": "hushrun.unit-facts/v1", "adapter": "pytest", "failuresByUnit": True,
         "settings": {"pythonFiles": ["test_*.py", "*_test.py"]}, "units": units}
if os.environ.get("FAKE_STALE"):
    facts["staleBuild"] = os.environ["FAKE_STALE"]
with open(os.path.join(d, "fake.json"), "w") as f:
    json.dump(facts, f)
sys.exit(1)
"#;

/// A test file of 6,000 tests, one of which fails while `FAIL` is set, and one more test file.
pub fn many(tests: &str) -> Vec<(String, String)> {
    vec![
        ("hushrun.yaml".into(), yaml(tests)),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\n".into()),
        (
            "tests/test_many.py".into(),
            "import os\nimport pytest\n\n@pytest.mark.parametrize('n', range(5999))\ndef test_n(n):\n    pass\n\ndef test_flips():\n    assert not os.environ.get('FAIL')\n".into(),
        ),
        ("tests/test_other.py".into(), "def test_other():\n    pass\n".into()),
    ]
}

/// A runner that writes pytest's facts itself with tests as the unit: `FAKE_UNITS` as its units,
/// a test file the run's `hushrun.json` lists in `reuse`, and a test it lists in `reuseTests`, left
/// out; `failuresByUnit` unless `FAKE_BY_UNIT` is `0`; `staleBuild`, `notKept` and
/// `leftOutUnlisted` from `FAKE_STALE`, `FAKE_NOT_KEPT` and `FAKE_LEFT_OUT`; a `.started` file
/// left with `FAKE_STARTED` as its content; and `FAKE_EXIT` as its exit code.
pub const RULES_RUNNER: &str = r#"import json, os, sys
d = os.environ["HUSHRUN_UNITS"]
with open(os.path.join(d, "hushrun.json")) as f:
    run = json.load(f)
n = len([x for x in os.listdir(d) if x.startswith("fake-")]) + 1
units = json.loads(os.environ["FAKE_UNITS"])
whole = run.get("reuse") or []
listed = run.get("reuseTests") or {}
for u in units:
    if u["unit"] in whole:
        u.update(deselected=True, finished=0, failed=False)
    for t in u.get("tests", []):
        if u["unit"] in whole or t["id"] in listed.get(u["unit"], []):
            t.update(finished=False, deselected=True, failed=False)
facts = {"schema": "hushrun.unit-facts/v1", "adapter": "pytest",
         "failuresByUnit": os.environ.get("FAKE_BY_UNIT", "1") == "1",
         "settings": {"pythonFiles": ["test_*.py", "*_test.py"]}, "units": units}
for env, key in [("FAKE_STALE", "staleBuild"), ("FAKE_NOT_KEPT", "notKept"),
                 ("FAKE_LEFT_OUT", "leftOutUnlisted")]:
    if os.environ.get(env):
        facts[key] = os.environ[env]
with open(os.path.join(d, "fake-%d.json" % n), "w") as f:
    json.dump(facts, f)
if os.environ.get("FAKE_STARTED"):
    with open(os.path.join(d, "fake-%d.started" % n), "w") as f:
        f.write(os.environ["FAKE_STARTED"])
if os.environ.get("FAKE_SLEEP"):
    import time
    time.sleep(float(os.environ["FAKE_SLEEP"]))
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
"#;

/// `test_a.py`'s facts: `ran` its tests (`t1`, `t2`, and `t3` when it fails), and `extra` over it.
pub fn rules_units(tests: Value, extra: Value) -> String {
    let n = tests.as_array().map_or(0, Vec::len);
    let mut unit = json!({"unit": "test_a.py", "selected": n, "finished": n, "reads": ["test_a.py"], "tests": tests});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        unit[k] = v;
    }
    json!([unit, {"unit": "test_b.py", "selected": 1, "finished": 1, "reads": ["test_b.py"],
                  "tests": [{"id": "tb", "finished": true}]}])
    .to_string()
}

/// [`shop`] with tests skipped while `SKIP` is set, in four ways, and a test file skipped whole as
/// it loads.
pub fn skips() -> Vec<(String, String)> {
    let mut files = shop("{mode: reuse, unit: test}");
    files.push((
        "tests/test_skips.py".into(),
        "import os\nimport pytest\n\n\
         @pytest.fixture\ndef ready():\n    if os.environ.get('SKIP'):\n        pytest.skip('not ready')\n\n\
         @pytest.mark.skipif(bool(os.environ.get('SKIP')), reason='skipped')\ndef test_skipped():\n    assert True\n\n\
         def test_in_setup(ready):\n    assert True\n\n\
         @pytest.mark.xfail(bool(os.environ.get('SKIP')), run=False, reason='not run')\ndef test_not_run():\n    assert True\n\n\
         def test_ran():\n    assert True\n"
            .into(),
    ));
    files.push((
        "tests/test_skipmod.py".into(),
        "import os\nimport pytest\n\nif os.environ.get('SKIP'):\n    pytest.skip('not here', allow_module_level=True)\n\ndef test_m():\n    assert True\n"
            .into(),
    ));
    files
}

/// An autouse module fixture that checks at its end that no test leaked, and a test that leaks
/// while `BUG` is set.
pub fn leak() -> Vec<(String, String)> {
    vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\npythonpath = .\n".into()),
        (
            "tests/test_leak.py".into(),
            "import os, pytest\nLEAKS = []\n\n\
             @pytest.fixture(scope='module', autouse=True)\ndef no_leaks():\n    yield\n    assert not LEAKS, 'a test left a connection open'\n\n\
             def test_opens():\n    if os.environ.get('BUG'):\n        LEAKS.append('conn')\n\n\
             @pytest.mark.parametrize('i', range(10))\ndef test_more(i):\n    assert True\n\n\
             def test_last():\n    assert True\n"
                .into(),
        ),
        (
            "tests/test_other.py".into(),
            "def test_other():\n    assert True\n".into(),
        ),
    ]
}

/// A session fixture a test asks for at run time, which says at the session's end that the test
/// leaked while `BUG` is set.
pub fn session_leak() -> Vec<(String, String)> {
    vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\npythonpath = .\n".into()),
        (
            "tests/conftest.py".into(),
            "import pytest\nOPEN = []\n\n\
             @pytest.fixture(scope='session')\ndef db():\n    yield OPEN\n    assert not OPEN, 'a connection was left open'\n"
                .into(),
        ),
        (
            "tests/test_l.py".into(),
            "import os\n\ndef test_leaky(request):\n    db = request.getfixturevalue('db')\n    if os.environ.get('BUG'):\n        db.append('c')\n"
                .into(),
        ),
        (
            "tests/test_m.py".into(),
            "import pytest\n\n@pytest.mark.parametrize('i', range(5))\ndef test_many(i, db):\n    assert True\n"
                .into(),
        ),
    ]
}

/// One test file of two tests: one fails while `FAIL` is set, the other while `HIDDEN` is, which
/// no file says, as a missed dependency would.
pub fn two_tests() -> Vec<(String, String)> {
    vec![
        (
            "hushrun.yaml".to_string(),
            yaml("{mode: reuse, unit: test}"),
        ),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        (
            "tests/test_two.py".into(),
            "import os\n\ndef test_ok():\n    assert not os.environ.get('HIDDEN')\n\n\
             def test_flips():\n    assert not os.environ.get('FAIL')\n"
                .into(),
        ),
    ]
}

/// A test file of 100 tests, and one of one test.
pub fn big_and_small() -> Vec<(String, String)> {
    vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\n".into()),
        (
            "tests/test_big.py".into(),
            "import pytest\n\n@pytest.mark.parametrize('n', range(100))\ndef test_n(n):\n    assert n >= 0\n".into(),
        ),
        ("tests/test_small.py".into(), "def test_s():\n    pass\n".into()),
    ]
}

/// A test file that fails while `BROKEN` is set, which no key holds, and another.
pub fn target_and_changed() -> Vec<(String, String)> {
    vec![
        ("hushrun.yaml".into(), yaml("{mode: reuse, unit: test}")),
        (".gitignore".into(), "__pycache__/\n.pytest_cache/\n".into()),
        ("pytest.ini".into(), "[pytest]\ntestpaths = tests\n".into()),
        (
            "tests/test_target.py".into(),
            "import os\n\ndef test_target():\n    assert not os.environ.get('BROKEN')\n".into(),
        ),
        (
            "tests/test_changed.py".into(),
            "def test_c():\n    pass\n".into(),
        ),
    ]
}
