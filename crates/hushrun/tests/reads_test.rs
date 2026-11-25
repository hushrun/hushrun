//! \[ADR 0081, 0083\] What a test read, with `tests.reads: test` (C-40, C-42): fixtures that pin
//! the heuristic. Each changes one file so that one test, `test_target`, fails, and asks `observe`
//! whether that test would have been reused: never with `reads: file`, whose list is its test
//! file's; not with `reads: test` where the rule follows the change; and, for the three shapes
//! `reads: test` is known to miss (RFC 0004 §3.2, C-40), yes, which a test names as a known miss.
//! One more shows an audit run catching such a miss and suspending reuse.
//!
//! In-process (CONTRIBUTING.md, "Which tests CI runs again"): each test runs a check as `hushrun
//! run unit -- python -m pytest …` would, through pytest's adapter's own type, `pytest::Pytest`,
//! and the code every adapter shares, and never starts the `hushrun` binary (`common/pytest.rs`).
//! pytest runs as COMMAND, for real. Nothing here needs the CLI, so `e2e_pytest/reads_test.rs`
//! holds no test.
//!
//! They need Python 3.12 or later with pytest, to watch a test, and say so and pass otherwise,
//! unless `HUSHRUN_TEST_TOOLS=required`, which CI sets.

#[path = "common/reads_test.rs"]
mod fixtures;
#[path = "common/in_process.rs"]
mod in_process;
#[path = "common/pytest.rs"]
mod pytest;
#[path = "common/python.rs"]
mod python;
#[path = "common/repo.rs"]
mod repo;
#[path = "common/store.rs"]
mod store;

use serde_json::json;

use fixtures::*;
use in_process::{Ran, Repo};
use python::{has_pytest, python, tools, watches};

/// Whether these tests can run here: pytest, on a Python that watches a test. Below 3.12 there is
/// nothing to watch with, which isn't a missing tool: `test_as_unit`'s observe test holds what a
/// run says then (C-42), and these skip.
fn can_watch() -> bool {
    if !tools("Python 3.8+ with pytest and pytest-xdist", has_pytest()) {
        return false;
    }
    if !watches() {
        eprintln!("note: skipping: this Python has no sys.monitoring, so pytest watches no test");
        return false;
    }
    true
}

fn repo_of(files: &[(&str, &str)], tests: &str) -> Repo {
    pytest::repo(&files_of(files, tests))
}

/// `hushrun run unit -- python -m pytest -q -p no:cacheprovider`, in-process.
fn run_pytest(r: &Repo, python: &str) -> Ran {
    pytest::run(r, &pytest::pytest_command(python, &[]), &[])
}

/// Whether `observe`, with `reads`, would have reused `test_target` of `tests/test_a.py` after
/// `change` made it fail: what its disagreement line says.
fn would_reuse(files: &[(&str, &str)], change: (&str, &str), reads: &str) -> bool {
    let r = repo_of(
        files,
        &format!("{{mode: observe, unit: test, reads: {reads}}}"),
    );
    let python = python();
    let first = run_pytest(&r, &python);
    assert_eq!(first.code, 0, "{}", first.said());
    assert!(
        !first.said().contains("each test is taken to have read"),
        "{}",
        first.said()
    );
    r.write(change.0, change.1);
    let second = run_pytest(&r, &python);
    assert_eq!(
        second.code,
        1,
        "the change makes test_target fail:\n{}",
        second.said()
    );
    assert!(second.stdout.contains("test_target"), "{}", second.stdout);
    second.lines.iter().any(|l| l ==
        "unit: tests/test_a.py::test_target failed, though it passed before on the same files it read: with `tests: reuse` it would have been reused",
    )
}

/// `reads: test` follows the change: neither setting would reuse the test.
fn caught(files: &[(&str, &str)], change: (&str, &str)) {
    if !can_watch() {
        return;
    }
    assert!(!would_reuse(files, change, "file"), "reads: file");
    assert!(!would_reuse(files, change, "test"), "reads: test");
}

/// A known miss of `reads: test` (RFC 0004 §3.2): `reads: file` runs the test, `reads: test`
/// would reuse it. Whoever closes the miss changes the second assertion.
fn known_miss(files: &[(&str, &str)], change: (&str, &str)) {
    if !can_watch() {
        return;
    }
    assert!(!would_reuse(files, change, "file"), "reads: file");
    assert!(
        would_reuse(files, change, "test"),
        "reads: test: a known miss"
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_function_called() {
    caught(
        &[
            ("app/calc.py", "def f():\n    return 1\n"),
            (
                "app/facade.py",
                "from app import calc\n\ndef g():\n    return calc.f()\n",
            ),
            (
                "tests/test_a.py",
                "from app import facade\n\ndef test_target():\n    assert facade.g() == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/calc.py", "def f():\n    return 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_constant_the_code_that_ran_imports_directly() {
    caught(
        &[
            ("app/consts.py", "K = 1\n"),
            (
                "app/facade.py",
                "from app import consts\n\ndef g():\n    return consts.K\n",
            ),
            (
                "tests/test_a.py",
                "from app import facade\n\ndef test_target():\n    assert facade.g() == 1\n",
            ),
        ],
        ("app/consts.py", "K = 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_constant_the_test_file_imports_directly() {
    caught(
        &[
            ("app/consts.py", "K = 1\n"),
            (
                "tests/test_a.py",
                "from app.consts import K\n\ndef test_target():\n    assert K == 1\n",
            ),
        ],
        ("app/consts.py", "K = 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_file_opened() {
    caught(
        &[
            ("app/data.py", "1\n"),
            (
                "tests/test_a.py",
                "import os\n\ndef test_target():\n    with open(os.path.join('app', 'data.py')) as f:\n        assert f.read().strip() == '1'\n",
            ),
        ],
        ("app/data.py", "2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_file_a_module_read_once_and_kept() {
    // test_first opens the file through prices.py, which keeps what it read; test_target, after
    // it, gets the kept value and opens nothing.
    caught(
        &[
            ("app/data.py", "5\n"),
            (
                "app/prices.py",
                "import os\n_kept = None\n\ndef price():\n    global _kept\n    if _kept is None:\n        with open(os.path.join(os.path.dirname(__file__), 'data.py')) as f:\n            _kept = f.read().strip()\n    return _kept\n",
            ),
            (
                "tests/test_a.py",
                "from app import prices\n\ndef test_first():\n    assert prices.price()\n\ndef test_target():\n    assert prices.price() == '5'\n",
            ),
        ],
        ("app/data.py", "6\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_what_a_shared_fixture_read() {
    caught(
        &[
            ("app/helper.py", "def make():\n    return 1\n"),
            (
                "app/fx.py",
                "from app import helper\n\ndef make():\n    return helper.make()\n",
            ),
            (
                "tests/conftest.py",
                "import pytest\nfrom app import fx\n\n@pytest.fixture(scope='module')\ndef made():\n    return fx.make()\n",
            ),
            (
                "tests/test_a.py",
                "def test_target(made):\n    assert made == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/helper.py", "def make():\n    return 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_module_loaded_lazily_inside_a_test() {
    caught(
        &[
            ("app/lazy.py", "K = 1\n"),
            (
                "tests/test_a.py",
                "def test_target():\n    from app import lazy\n    assert lazy.K == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/lazy.py", "K = 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_module_code_outside_the_repository_loaded() {
    // pytest loads app/ext.py as a plugin, before any test; test_target reads it from
    // sys.modules, and nothing of the repository imports it.
    caught(
        &[
            (
                "pytest.ini",
                "[pytest]\ntestpaths = tests\npythonpath = .\naddopts = -p app.ext\n",
            ),
            ("app/ext.py", "VALUE = 1\n"),
            (
                "tests/test_a.py",
                "import sys\n\ndef test_target():\n    assert sys.modules['app.ext'].VALUE == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/ext.py", "VALUE = 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_fixture_a_test_asked_for_at_run_time() {
    // test_first requests the session fixture and sets it up; test_target asks for it with
    // `request.getfixturevalue`, as pytest-lazy-fixture does, and gets the value pytest kept.
    caught(
        &[
            ("app/helper.py", "def make():\n    return 1\n"),
            (
                "app/fx.py",
                "from app import helper\n\ndef make():\n    return helper.make()\n",
            ),
            (
                "tests/conftest.py",
                "import pytest\nfrom app import fx\n\n@pytest.fixture(scope='session')\ndef made():\n    return fx.make()\n",
            ),
            (
                "tests/test_a.py",
                "def test_first(made):\n    assert made\n\ndef test_target(request):\n    assert request.getfixturevalue('made') == 1\n",
            ),
        ],
        ("app/helper.py", "def make():\n    return 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_fixture_set_up_inside_a_tests_window() {
    // The first to ask for the session fixture asks at run time: its setup starts inside
    // test_target's window, and serves it.
    caught(
        &[
            ("app/helper.py", "def make():\n    return 1\n"),
            (
                "app/fx.py",
                "from app import helper\n\ndef make():\n    return helper.make()\n",
            ),
            (
                "tests/conftest.py",
                "import pytest\nfrom app import fx\n\n@pytest.fixture(scope='session')\ndef made():\n    return fx.make()\n",
            ),
            (
                "tests/test_a.py",
                "def test_target(request):\n    assert request.getfixturevalue('made') == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/helper.py", "def make():\n    return 2\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_what_a_session_hook_ran_outside_every_test() {
    // conftest.py's pytest_configure, before any test, calls into app/setup.py, which keeps what
    // app/calc.py computes; test_target reads the kept value, and runs neither.
    caught(
        &[
            ("app/state.py", "VALUE = None\n"),
            ("app/calc.py", "def value():\n    return 1\n"),
            (
                "app/setup.py",
                "from app import calc, state\n\ndef configure():\n    state.VALUE = calc.value()\n",
            ),
            (
                "tests/conftest.py",
                "from app import setup\n\ndef pytest_configure(config):\n    setup.configure()\n",
            ),
            (
                "tests/test_a.py",
                "from app import state\n\ndef test_target():\n    assert state.VALUE == 1\n\ndef test_plain():\n    assert True\n",
            ),
        ],
        ("app/calc.py", "def value():\n    return 2\n"),
    );
}

/// A package that re-exports: tax.py takes RATE from `app.config`, whose `__init__.py` binds it
/// from `.defaults`; no function of either runs. `head` is the re-exporting module's text.
fn reexported(head: (&'static str, &'static str)) -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/config/defaults.py", "RATE = 0.2\nCURRENCY = 'EUR'\n"),
        head,
        (
            "app/tax.py",
            "from app.config import RATE\n\ndef tax(amount):\n    return round(amount * RATE, 2)\n",
        ),
        (
            "tests/test_a.py",
            "from app.tax import tax\n\ndef test_target():\n    assert tax(10) == 2.0\n\ndef test_plain():\n    assert True\n",
        ),
    ]
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_what_a_packages_init_reexports() {
    caught(
        &reexported((
            "app/config/__init__.py",
            "from .defaults import RATE, CURRENCY\n",
        )),
        ("app/config/defaults.py", "RATE = 0.3\nCURRENCY = 'EUR'\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_a_star_import() {
    caught(
        &reexported((
            "app/config/__init__.py",
            "from app.config.defaults import *\n",
        )),
        ("app/config/defaults.py", "RATE = 0.3\nCURRENCY = 'EUR'\n"),
    );
}

// covers: C-40, C-42
#[test]
fn reads_test_follows_what_a_modules_all_reexports() {
    // A plain module, not a package, whose `__all__` names what it took from another.
    let mut files = reexported(("app/config/__init__.py", ""));
    files.push((
        "app/config/public.py",
        "from app.config.defaults import RATE\n__all__ = ['RATE']\n",
    ));
    files.retain(|(p, _)| *p != "app/tax.py");
    files.push((
        "app/tax.py",
        "from app.config.public import RATE\n\ndef tax(amount):\n    return round(amount * RATE, 2)\n",
    ));
    caught(
        &files,
        ("app/config/defaults.py", "RATE = 0.3\nCURRENCY = 'EUR'\n"),
    );
}

// covers: C-40
#[test]
fn a_known_miss_of_reads_test_a_constant_a_plain_module_passes_on() {
    // A plain module, neither a package's `__init__.py` nor one whose `__all__` names it, takes
    // RATE from defaults.py, and tax.py takes it from there: no re-export C-40 follows, and no
    // function of the way runs. Still the first known miss, two imports away.
    let mut files = reexported(("app/config/__init__.py", ""));
    files.push((
        "app/config/middle.py",
        "from app.config.defaults import RATE\n",
    ));
    files.retain(|(p, _)| *p != "app/tax.py");
    files.push((
        "app/tax.py",
        "from app.config.middle import RATE\n\ndef tax(amount):\n    return round(amount * RATE, 2)\n",
    ));
    known_miss(
        &files,
        ("app/config/defaults.py", "RATE = 0.3\nCURRENCY = 'EUR'\n"),
    );
}

/// Two imports away: cart.py ran and imports settings.py, which built TAX from defaults.py, with
/// no function of either called.
fn two_imports_away() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/defaults.py", "RATE = 3\n"),
        (
            "app/settings.py",
            "from app.defaults import RATE\nTAX = RATE * 2\n",
        ),
        (
            "app/cart.py",
            "from app.settings import TAX\n\ndef total(xs):\n    return sum(xs) + TAX\n",
        ),
        (
            "tests/test_a.py",
            "from app.cart import total\n\ndef test_target():\n    assert total([1]) == 7\n",
        ),
    ]
}

// covers: C-40
#[test]
fn a_known_miss_of_reads_test_a_value_two_imports_away() {
    known_miss(&two_imports_away(), ("app/defaults.py", "RATE = 4\n"));
}

// covers: C-40
#[test]
fn a_known_miss_of_reads_test_a_registry_another_file_filled_at_import() {
    known_miss(
        &a_registry(),
        (
            "app/plugins.py",
            "from app.registry import HANDLERS\nHANDLERS['x'] = 42\n",
        ),
    );
}

/// A value test_first computed through rates.py, which a `functools.cache` wrapper (C code, so no
/// function of the way starts again) keeps: test_target, after it, takes it from the cache, and
/// runs nothing of rates.py. Which test misses it depends on the order they run in.
fn a_value_a_cache_kept() -> Vec<(&'static str, &'static str)> {
    vec![
        ("app/rates.py", "def rate():\n    return 1\n"),
        (
            "app/heavy.py",
            "import functools\nfrom app import rates\n\n@functools.cache\ndef value():\n    return rates.rate()\n",
        ),
        (
            "tests/test_a.py",
            "from app import heavy\n\ndef test_first():\n    assert heavy.value()\n\ndef test_target():\n    assert heavy.value() == 1\n",
        ),
    ]
}

// covers: C-40
#[test]
fn a_known_miss_of_reads_test_a_value_an_earlier_test_computed_and_a_cache_kept() {
    known_miss(
        &a_value_a_cache_kept(),
        ("app/rates.py", "def rate():\n    return 2\n"),
    );
}

// covers: C-40, C-34
#[test]
fn an_audit_catches_a_known_miss_of_reads_test_and_suspends_reuse() {
    if !can_watch() {
        return;
    }
    // test_target is the one test the plan could leave out (test_other.py changes too, so it
    // runs): the audit, 2% of one test and at least one, runs it as a check whatever its draw; it
    // fails, runs once more alone, fails again, and the check's reuse is suspended.
    let r = repo_of(&a_registry(), "{mode: reuse, unit: test, reads: test}");
    let python = python();
    let first = run_pytest(&r, &python);
    assert_eq!(first.code, 0, "{}", first.said());
    r.write(
        "app/plugins.py",
        "from app.registry import HANDLERS\nHANDLERS['x'] = 42\n",
    );
    r.write(
        "tests/test_other.py",
        "def test_other():\n    assert True  # changed\n",
    );
    let caught = run_pytest(&r, &python);
    assert_eq!(caught.code, 1, "{}", caught.said());
    // The second run disagreed, which suspends reuse; the CLI's line that says so is end to end
    // (`e2e_pytest/reads_test.rs`).
    assert_eq!(r.disagreements(), ["tests/test_a.py::test_target"]);
}

// covers: C-40, C-42, C-36
#[test]
fn with_reads_test_a_test_is_reused_on_its_own_list_and_its_file_records_each_test() {
    if !can_watch() {
        return;
    }
    // One test of a file runs app/a.py's code through app/wa.py, sixty others app/b.py's through
    // app/wb.py; the test file imports both wrappers, which every test reads: a change to a.py
    // runs only the first test, and two of the sixty, chosen by their keys, as a check.
    let files = [
        ("app/a.py", "def f():\n    return 1\n"),
        ("app/b.py", "def f():\n    return 1\n"),
        (
            "app/wa.py",
            "from app import a\n\ndef f():\n    return a.f()\n",
        ),
        (
            "app/wb.py",
            "from app import b\n\ndef f():\n    return b.f()\n",
        ),
        (
            "tests/test_a.py",
            "import pytest\nfrom app import wa, wb\n\ndef test_one():\n    assert wa.f() == 1\n\n@pytest.mark.parametrize('n', range(60))\ndef test_two(n):\n    assert wb.f() == 1\n",
        ),
    ];
    let r = repo_of(&files, "{mode: reuse, unit: test, reads: test}");
    let python = python();
    let first = run_pytest(&r, &python);
    assert_eq!(first.code, 0, "{}", first.said());
    // A test file that passed whole records its own pass and each test's.
    let map = r.map();
    assert_eq!(
        map["units"]["tests/test_a.py"]["tests"]
            .as_object()
            .unwrap()
            .len(),
        61
    );
    assert_eq!(r.unit_results().values().sum::<usize>(), 2 + 61 + 1);
    r.write("app/a.py", "def f():\n    return 1  # changed\n");
    let second = run_pytest(&r, &python);
    assert_eq!(second.code, 0, "{}", second.said());
    assert!(
        second.lines.iter().any(|l| hushrun_testkit::lines::drawn(l)
            == hushrun_testkit::lines::drawn("unit: 3 of 62 tests run, in 1 of 2 test files; 59 already passed on the files they read, and 2 more that already passed run anyway, as a check")),
        "{}",
        second.said()
    );
    assert_eq!(
        second.units["tests"],
        json!({"total": 62, "ran": 3, "reused": 59})
    );
    // With `reads: file` the same change runs every test of the file.
    r.write(
        "hushrun.yaml",
        &yaml("{mode: reuse, unit: test, reads: file}"),
    );
    r.write("app/a.py", "def f():\n    return 1  # changed again\n");
    let whole = run_pytest(&r, &python);
    assert_eq!(whole.code, 0, "{}", whole.said());
    assert!(
        !whole.said().contains("58 already passed"),
        "{}",
        whole.said()
    );
}
