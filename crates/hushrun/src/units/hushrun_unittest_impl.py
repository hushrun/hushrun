"""hushrun's unittest adapter: for each test module unittest runs, and each of its tests, what it read
(ADR 0110, C-66 to C-69). It drives the observer pytest's adapter drives (`hushrun_observe`), with
unittest's own points instead of pytest's hooks; what "read" means, and what it can't follow, are
the same (ADR 0068, 0083).

Where it starts: the `sitecustomize.py` hushrun puts first on PYTHONPATH loads it, and it starts
only in a process whose own command line runs the tests (`-m unittest`, a `manage.py`,
`django-admin` or `-m django` with `test`, `coverage run` of either) and that no other process of
this run started: a Python a test starts inherits `HUSHRUN_UNITTEST_SESSION`, and is the test's
doing, which isn't followed (`subprocess`). A launcher written in Python (poetry, pipenv) has
another command line, and the Python it starts for the tests is the one.

Units (C-66): a unit is a test module's file. unittest's loader imports it, which the observer's
finder sees: from then until the next one, what loads is that unit's, as pytest's collection of a
file is. A test's unit is its class's module's file; its id is `test.id()` after that module's name
and a dot (`CartTest.test_total`). A module that failed to import, or that raised SkipTest as it
loaded, is a test of unittest's own (`unittest.loader`), which is a fail or a skip of its unit and
no test of it. A test no repository module owns (a doctest, a FunctionTestCase) is no unit's: it
runs every time, and when it fails no pass of the run is kept (`failuresByUnit`).

Windows: a test's window is from `startTest` to `stopTest` of the result, inside `TestCase.run`.
What runs between the suite's fixtures and the test (Django's `_pre_setup`) and after it (Django's
`_post_teardown`) is its unit's, which every test of the unit reads. `setUpModule` and
`setUpClass` are shared setups (ADR 0083, item 4): what they ran and opened is read by every test
of their module or class that ran while unittest kept it. `tearDownClass`, `tearDownModule` and
their cleanups are the unit's, and a failure there keeps no pass for any test the fixture served.

Selecting (C-67): the first top-level suite run in a process claims the run's directory, and only
then, and only when every `discover` it saw used the pattern hushrun read (`pythonFiles`), it
removes from the suite the tests of the units `reuse` lists and the tests `reuseTests` lists, and
reports them deselected. When every test was removed, unittest's "no tests ran" (exit 5, Python
3.12 or later) becomes 0, and only then.

Output: one file per process in $HUSHRUN_UNITS, `unittest-<pid>.json`, written whole at the end of
each top-level suite run. Standard library only.
"""

import builtins
import json
import os
import sys
import threading
import time

import hushrun_observe as core
from hushrun_observe import (
    ENV_DIR,
    MOST_ERRORS,
    NO_TESTS_COLLECTED,
    RUN_SCHEMA,
    SCHEMA,
    SCHEMA_V1,
    START,
    Observer,
    _FirstLoadFinder,
    _audit,
    _claim,
    _start_watching,
    _stop_watching,
    _wrapped_import,
)

WORD = "unittest"
SESSION_VAR = "HUSHRUN_UNITTEST_SESSION"
DEFAULT_PATTERN = "test*.py"
LOADER = "unittest.loader"
DJANGO_PROGRAMS = ("manage.py", "django-admin", "django-admin.py")
_MISSING = object()
# What the current thread's TestCase.run is running: a startTest of another test came from elsewhere.
_CASE = threading.local()


def runs_tests(argv):
    """[C-66] Whether a process's own command line runs unittest's tests, as hushrun's recognizer
    reads COMMAND: `-m unittest`; Django's `test`, through `manage.py`, `django-admin` or
    `-m django`; `coverage run` of either is the same Python."""
    words = list(argv or ())
    for i, word in enumerate(words):
        if word == "-m" and i + 1 < len(words):
            module = words[i + 1]
            if module in ("unittest", "unittest.__main__"):
                return True
            if module == "django":
                return "test" in words[i + 2:]
        if word in ("-munittest", "-munittest.__main__"):
            return True
        if word == "-mdjango":
            return "test" in words[i + 1:]
        if os.path.basename(word) in DJANGO_PROGRAMS:
            return "test" in words[i + 1:]
    return False


def _script(argv):
    """[ADR 0110] The script a Python's own command line runs (`manage.py`), when it runs one."""
    words = list(argv or ())[1:]
    at = 0
    while at < len(words):
        word = words[at]
        if word in ("-m", "-c") or word.startswith(("-m", "-c")) and not word.startswith("--"):
            return None
        if word == "-" or not word.startswith("-"):
            return word
        if word == "--check-hash-based-pycs" or (word[1:2] in ("W", "X") and len(word) == 2):
            at += 1
        at += 1
    return None


def _unittest_dirs():
    import unittest

    dirs = set()
    f = os.path.abspath(unittest.__file__)
    for d in {os.path.dirname(f), os.path.dirname(os.path.realpath(f))}:
        dirs.add(d.rstrip(os.sep) + os.sep)
    return tuple(sorted(dirs))


def _call(fn, *args):
    """Runs one point's work with the observer's lock, in the process that started it, never
    raising into unittest. A forked child (a test's, Django's --parallel worker) observes nothing:
    its parent says what it couldn't follow."""
    obs = core.OBSERVER
    if obs is None or obs.done or obs.pid != os.getpid():
        return None
    try:
        with obs.lock:
            return fn(obs, *args)
    except Exception as err:
        if len(obs.errors) < MOST_ERRORS:
            obs.errors.append("%s: %r" % (getattr(fn, "__name__", "?"), err))
        return None


# --- what a test is ------------------------------------------------------------------------------


def _module_unit(obs, name):
    """The unit of the module `name`: its file, as loaded or as the loader was loading it."""
    found = obs.loading_files.get(name)
    if found:
        return found
    module = sys.modules.get(name) if name else None
    return obs.module_file(module) if module is not None else None


def _identify(obs, test):
    """(unit, id, unittest's own): a test's unit and its id there. A test of unittest's loader
    stands for a module that didn't load (it failed, or raised SkipTest): its unit's, with no id."""
    cls = type(test)
    if cls.__module__ == LOADER:
        name = getattr(test, "_testMethodName", None)
        unit = None
        if isinstance(name, str):
            parts = name.split(".")
            for end in range(len(parts), 0, -1):
                unit = _module_unit(obs, ".".join(parts[:end]))
                if unit:
                    break
        return unit, None, True
    if cls.__module__ == "doctest":
        return None, None, False
    unit = _module_unit(obs, cls.__module__)
    if unit is None:
        return None, None, False
    try:
        full = test.id()
    except Exception:
        return None, None, False
    prefix = cls.__module__ + "."
    test_id = full[len(prefix):] if full.startswith(prefix) else full
    return unit, test_id or None, False


def _keys(test):
    """[ADR 0083] The shared setups a test is served: its module's and its class's."""
    cls = type(test)
    return frozenset({"module " + cls.__module__,
                      "class %s.%s" % (cls.__module__, cls.__qualname__)})


def _not_followed(obs, unit, test_id, why, detail):
    if obs.by_test and test_id is not None:
        target = obs.test_not_followed.setdefault((unit, test_id), {})
    else:
        target = obs.not_followed.setdefault(unit, {})
    target.setdefault(why, set()).add(detail)


# --- loading -------------------------------------------------------------------------------------


def _first_load(fullname, path, frame):
    """[C-66] A first load unittest's loader asked for: a test module about to be imported, whose
    file is the unit from here; a package is none, and ends the one before."""
    obs = core.OBSERVER
    if obs is None or obs.pid != os.getpid():
        return
    req = obs.requester(frame)
    if req is None or not req.f_code.co_filename.startswith(core.RUNNER_DIRS):
        return
    import importlib.machinery

    spec = importlib.machinery.PathFinder.find_spec(fullname, path)
    origin = getattr(spec, "origin", None) if spec is not None else None
    if not isinstance(origin, str) or spec.submodule_search_locations is not None:
        obs.stop()
        return
    unit = obs.rel(origin)
    if unit is None or not unit.endswith(".py"):
        obs.stop()
        return
    obs.loading_files[fullname] = unit
    obs.unit_entry(unit)
    obs.start(unit)


def _discovered(obs, pattern):
    obs.patterns.add(pattern if isinstance(pattern, str) else DEFAULT_PATTERN)


def _loaded(obs):
    obs.stop()


# --- the suite -----------------------------------------------------------------------------------


def _tests_of(suite, out, opaque):
    """Every test of `suite`, depth first; suites that aren't unittest's are opaque."""
    import unittest

    for test in getattr(suite, "_tests", ()):
        if isinstance(test, unittest.TestSuite):
            _tests_of(test, out, opaque)
        elif test is None:
            continue
        else:
            try:
                iter(test)
            except TypeError:
                out.append(test)
            else:
                opaque.append(test)


def _may_deselect(obs):
    if not (obs.reuse or obs.reuse_tests) or obs.python_files is None or not obs.claimed:
        return False
    return all(p == obs.python_files[0] for p in obs.patterns) and len(obs.python_files) == 1


def _deselect(obs, suite):
    """[C-67] Removes the tests hushrun.json lists from `suite` and every suite in it."""
    import unittest

    removed_units = {}
    for inner in [suite] + _suites_in(suite):
        tests = getattr(inner, "_tests", None)
        if not isinstance(tests, list):
            continue
        keep = []
        for test in tests:
            if test is None or isinstance(test, unittest.TestSuite):
                keep.append(test)
                continue
            unit, test_id, own = _identify(obs, test)
            gone = unit is not None and (
                unit in obs.reuse
                or (test_id is not None and not own and test_id in obs.reuse_tests.get(unit, ())))
            if not gone:
                keep.append(test)
                continue
            obs.removed += 1
            if unit in obs.reuse:
                removed_units[unit] = removed_units.get(unit, 0) + 1
            if obs.by_test and test_id is not None and not own:
                obs.test_entry(unit, test_id)["deselected"] = True
        inner._tests = keep
    for unit, n in removed_units.items():
        obs.unit_entry(unit)
        obs.deselected[unit] = obs.deselected.get(unit, 0) + n


def _suites_in(suite):
    import unittest

    out = []
    for test in getattr(suite, "_tests", ()):
        if isinstance(test, unittest.TestSuite):
            out.append(test)
            out.extend(_suites_in(test))
    return out


def _top_start(obs, suite, result):
    obs.stop()
    obs.runs += 1
    if not obs.claim_tried:
        obs.claim_tried = True
        obs.claimed, obs.claimant = _claim(obs.directory)
    if obs.started_file is None:
        path = os.path.join(obs.directory, "unittest-%d.started" % os.getpid())
        with open(path, "w", encoding="utf-8") as f:
            f.write(str(os.getpid()))
        obs.started_file = path
    if _may_deselect(obs):
        _deselect(obs, suite)
    tests, opaque = [], []
    _tests_of(suite, tests, opaque)
    if opaque:
        # A suite unittest doesn't know holds tests hushrun can't count: none of this run's
        # failures is told by unit, and a test file whose tests it holds may not have run whole.
        obs.opaque = True
    for test in tests:
        unit, test_id, own = _identify(obs, test)
        if unit is None:
            continue
        obs.unit_entry(unit)["selected"] += 1
    obs.collection_finished = True
    _instrument(obs, result)


def _top_end(obs, result, raised):
    _restore(obs, result)
    obs.active_test = None
    obs.stop()
    if raised:
        # [C-32] Cut short (an exception up through the suite, an interrupt): no failure of it is
        # told by unit, so a run that failed keeps no pass.
        obs.unowned_failure = True
    _write(obs)


# --- the result ----------------------------------------------------------------------------------


def _instrument(obs, result):
    """Wraps the result's own methods on the result itself, whatever its class (unittest's,
    Django's, a runner's), and restores them as the run ends."""
    if id(result) in obs.instrumented:
        return
    saved = {}
    for name, after in _RESULT_POINTS.items():
        original = getattr(result, name, None)
        if original is None:
            continue
        saved[name] = result.__dict__.get(name, _MISSING)
        setattr(result, name, _wrapped(original, after))
    obs.instrumented[id(result)] = saved


def _wrapped(original, after):
    def method(*args, **kwargs):
        try:
            return original(*args, **kwargs)
        finally:
            _call(after, *args)
    return method


def _restore(obs, result):
    saved = obs.instrumented.pop(id(result), None) or {}
    for name, had in saved.items():
        try:
            if had is _MISSING:
                delattr(result, name)
            else:
                setattr(result, name, had)
        except Exception:
            pass


def _start_test(obs, test, *_):
    unit, test_id, own = _identify(obs, test)
    obs.active_test = None
    if unit is None:
        obs.stop()
        return
    obs.unit_entry(unit)
    obs.start(unit)
    obs.shared_depth = 0
    obs.shared_teardown = False
    obs.setup_stack = []
    obs.test_started[id(test)] = time.perf_counter()
    here = getattr(_CASE, "test", None) is test
    if own:
        return
    obs.active_test = test_id
    if obs.by_test and test_id is not None:
        obs.test_entry(unit, test_id)
        if obs.watching:
            tick = obs.tick()
            obs.test_window[(unit, test_id)] = [tick, tick, obs.seq, _keys(test)]
    if not here:
        # Reported to this result by another process (Django's --parallel replays its workers'
        # results): what it read was never seen here.
        _not_followed(obs, unit, test_id, "subprocess", "a test that ran in another process")


def _stop_test(obs, test, *_):
    unit, test_id, own = _identify(obs, test)
    t0 = obs.test_started.pop(id(test), None)
    if unit is None:
        obs.stop()
        return
    duration = time.perf_counter() - t0 if t0 is not None else 0.0
    obs.unit_entry(unit)["duration"] += duration
    if obs.by_test and test_id is not None and not own:
        obs.test_entry(unit, test_id)["duration"] += duration
        window = obs.test_window.get((unit, test_id))
        if window is not None:
            window[1] = obs.tick()
            window[2] = obs.seq
    obs.stopped.add(id(test))
    # Back to the unit: what follows the test before the suite goes on (Django's _post_teardown)
    # is every test's of it.
    obs.active_test = None
    obs.start(unit)


def _outcome(obs, test, kind):
    if type(test).__name__ == "_ErrorHolder":
        _fixture_outcome(obs, kind)
        return
    unit, test_id, own = _identify(obs, test)
    if unit is None:
        if kind == "failed":
            obs.unowned_failure = True
        return
    entry = obs.unit_entry(unit)
    if kind == "failed":
        entry["failed"] = True
    elif kind == "skipped" and id(test) not in obs.skips:
        obs.skips.add(id(test))
        entry["skipped"] = entry.get("skipped", 0) + 1
    if obs.by_test and test_id is not None and not own:
        facts = obs.test_entry(unit, test_id)
        if kind == "failed":
            facts["failed"] = True
        elif kind == "skipped":
            facts["skipped"] = True


def _add_error(obs, test, *_):
    _outcome(obs, test, "failed")


def _add_skip(obs, test, *_):
    _outcome(obs, test, "skipped")


def _add_sub_test(obs, test, subtest=None, err=None, *_):
    if err is not None:
        _outcome(obs, test, "failed")


_RESULT_POINTS = {
    "startTest": _start_test,
    "stopTest": _stop_test,
    "addError": _add_error,
    "addFailure": _add_error,
    "addUnexpectedSuccess": _add_error,
    "addSkip": _add_skip,
    "addSubTest": _add_sub_test,
}


def _case_ran(obs, test, ok):
    """[C-32] A test finished once its TestCase.run returned, its stopTest reported, with nothing
    coming up through it."""
    stopped = id(test) in obs.stopped
    obs.stopped.discard(id(test))
    if not (ok and stopped):
        return
    unit, test_id, own = _identify(obs, test)
    if unit is None:
        return
    obs.unit_entry(unit)["finished"] += 1
    if own:
        return
    node = "%s::%s" % (unit, test_id)
    obs.finished_ids[node] = (unit, test_id if obs.by_test else None)
    obs.served[node] = (_keys(test), frozenset())
    if obs.by_test and test_id is not None:
        obs.test_entry(unit, test_id)["finished"] = True


# --- shared setups -------------------------------------------------------------------------------


def _fixture_begin(obs, key, unit, phase):
    obs.fixture_stack.append((key, unit, phase, obs.setup_stack, obs.active, obs.active_test))
    obs.active_test = None
    obs.setup_stack = []
    if unit:
        obs.unit_entry(unit)
        obs.start(unit)
    else:
        obs.stop()
    if phase == "setup":
        obs.shared_depth += 1
        if obs.watching:
            setup = {"argname": key, "ran": set(), "opened": set(), "start": obs.tick(),
                     "end": None, "def": key}
            obs.setups.append(setup)
            obs.setup_stack = [setup]
    else:
        obs.shared_teardown = True


def _fixture_end(obs):
    if not obs.fixture_stack:
        return
    key, unit, phase, setup_stack, active, active_test = obs.fixture_stack.pop()
    if phase == "setup":
        obs.shared_depth = max(0, obs.shared_depth - 1)
    else:
        obs.shared_teardown = any(f[2] == "teardown" for f in obs.fixture_stack)
        # unittest keeps it no more: tests after this don't read it.
        for setup in reversed(obs.setups):
            if setup["argname"] == key and setup["end"] is None:
                setup["end"] = obs.tick()
                break
    obs.setup_stack = setup_stack
    if active:
        obs.start(active)
    else:
        obs.stop()
    obs.active_test = active_test


def _fixture_outcome(obs, kind):
    """A setUpModule, setUpClass, tearDownClass or tearDownModule (or a cleanup of theirs) failed or
    raised SkipTest: unittest reports it on no test (an `_ErrorHolder`)."""
    if not obs.fixture_stack:
        if kind == "failed":
            obs.unowned_failure = True
        return
    key, unit, phase = obs.fixture_stack[-1][:3]
    if unit is None:
        if kind == "failed":
            obs.unowned_failure = True
        return
    entry = obs.unit_entry(unit)
    if kind == "skipped":
        entry["skipped"] = entry.get("skipped", 0) + 1
        return
    entry["failed"] = True
    if phase == "teardown":
        # [C-32] Its end failed: no test it served keeps a pass.
        obs.failed_wide.append((key, None))


def _pre_test(obs, test):
    """[C-66] After the suite's fixtures, before the test: what runs from here to its startTest
    (Django's _pre_setup) is its unit's."""
    unit, _, _ = _identify(obs, test)
    obs.active_test = None
    if unit:
        obs.unit_entry(unit)
        obs.start(unit)
    else:
        obs.stop()


def _module_of(obs, name):
    return _module_unit(obs, name) if name else None


def _patch():
    import unittest
    import unittest.case
    import unittest.loader
    import unittest.main
    import unittest.suite

    plain = unittest.suite.TestSuite
    loader = unittest.loader.TestLoader

    original_run = plain.run

    def run(self, result, debug=False):
        top = getattr(result, "_testRunEntered", False) is False
        if top:
            _call(_top_start, self, result)
        raised = True
        try:
            outcome = original_run(self, result, debug)
            raised = False
            return outcome
        finally:
            if top:
                _call(_top_end, result, raised)

    plain.run = run

    original_class_setup = plain._handleClassSetUp

    def _handleClassSetUp(self, test, result):
        current = test.__class__
        previous = getattr(result, "_previousTestClass", None)
        quiet = current == previous or getattr(result, "_moduleSetUpFailed", False) \
            or getattr(current, "__unittest_skip__", False)
        if not quiet:
            _call(_fixture_begin, "class %s.%s" % (current.__module__, current.__qualname__),
                  _call(lambda o: _module_of(o, current.__module__)), "setup")
        try:
            return original_class_setup(self, test, result)
        finally:
            if not quiet:
                _call(_fixture_end)
            _call(_pre_test, test)

    plain._handleClassSetUp = _handleClassSetUp

    original_module_fixture = plain._handleModuleFixture

    def _handleModuleFixture(self, test, result):
        current = test.__class__.__module__
        try:
            previous = self._get_previous_module(result)
        except Exception:
            previous = None
        quiet = current == previous
        if not quiet:
            _call(_fixture_begin, "module " + current,
                  _call(lambda o: _module_of(o, current)), "setup")
        try:
            return original_module_fixture(self, test, result)
        finally:
            if not quiet:
                _call(_fixture_end)

    plain._handleModuleFixture = _handleModuleFixture

    original_module_teardown = plain._handleModuleTearDown

    def _handleModuleTearDown(self, result):
        try:
            previous = self._get_previous_module(result)
        except Exception:
            previous = None
        if previous:
            _call(_fixture_begin, "module " + previous,
                  _call(lambda o: _module_of(o, previous)), "teardown")
        try:
            return original_module_teardown(self, result)
        finally:
            if previous:
                _call(_fixture_end)

    plain._handleModuleTearDown = _handleModuleTearDown

    original_class_teardown = plain._tearDownPreviousClass

    def _tearDownPreviousClass(self, test, result):
        previous = getattr(result, "_previousTestClass", None)
        current = test.__class__ if test is not None else None
        quiet = previous is None or current == previous
        if not quiet:
            _call(_fixture_begin, "class %s.%s" % (previous.__module__, previous.__qualname__),
                  _call(lambda o: _module_of(o, previous.__module__)), "teardown")
        try:
            return original_class_teardown(self, test, result)
        finally:
            if not quiet:
                _call(_fixture_end)

    plain._tearDownPreviousClass = _tearDownPreviousClass

    original_case_run = unittest.case.TestCase.run

    def case_run(self, result=None):
        had = getattr(_CASE, "test", None)
        _CASE.test = self
        ok = False
        try:
            outcome = original_case_run(self, result)
            ok = True
            return outcome
        finally:
            _CASE.test = had
            _call(_case_ran, self, ok)

    unittest.case.TestCase.run = case_run

    original_discover = loader.discover

    def discover(self, start_dir, pattern=DEFAULT_PATTERN, top_level_dir=None, *args, **kwargs):
        _call(_discovered, pattern)
        try:
            return original_discover(self, start_dir, pattern, top_level_dir, *args, **kwargs)
        finally:
            _call(_loaded)

    loader.discover = discover

    for name in ("loadTestsFromName", "loadTestsFromNames"):
        original = getattr(loader, name)

        def by_name(self, *args, _original=original, **kwargs):
            try:
                return _original(self, *args, **kwargs)
            finally:
                _call(_loaded)

        setattr(loader, name, by_name)

    # `unittest.main` is TestProgram itself, as the package exports it: its module is this one.
    program = sys.modules["unittest.main"].TestProgram
    original_run_tests = program.runTests

    def runTests(self, *args, **kwargs):
        try:
            return original_run_tests(self, *args, **kwargs)
        except SystemExit as stop:
            # [C-67] unittest says no test ran (exit 5) only because this process removed every
            # test that was left: those already passed on the files they read, so the run passed.
            # Any failure or error gives unittest another status first.
            if stop.code == NO_TESTS_COLLECTED and _call(_removed_all, self):
                raise SystemExit(0)
            raise

    program.runTests = runTests


def _removed_all(obs, program):
    result = getattr(program, "result", None)
    return obs.claimed and obs.removed > 0 and result is not None and result.wasSuccessful()


# --- the end -------------------------------------------------------------------------------------


def _write(obs):
    try:
        units = obs.results()
    except Exception as err:
        obs.errors.append("results: %r" % (err,))
        units = []
    try:
        edges = obs.edge_list()
    except Exception:
        edges = []
    settings = {"pythonFiles": sorted(obs.patterns), "python": sys.version.split()[0]}
    if obs.watch_reason:
        settings["watch"] = obs.watch_reason
    facts = {
        "schema": SCHEMA_V1 if obs.by_test else SCHEMA,
        "adapter": WORD,
        "start": START,
        "pid": os.getpid(),
        "worker": None,
        "controller": False,
        "failuresByUnit": not (obs.unowned_failure or obs.opaque),
        "settings": settings,
        "notFollowed": [{"why": k, "detail": d}
                        for k, v in sorted(obs.session_not_followed.items()) for d in sorted(v)],
        "units": units,
        "edges": edges,
        "errors": obs.errors,
        "removedItems": obs.removed,
        "parent": os.getppid(),
    }
    if obs.watching:
        try:
            imports, opens = obs.watch_tables()
            facts["imports"] = imports
            facts["opens"] = opens
            facts["reexports"] = obs.reexport_table()
            facts["paths"] = obs.paths
        except Exception as err:
            obs.errors.append("watch tables: %r" % (err,))
            for unit in units:
                unit.pop("loaded", None)
            facts["errors"] = obs.errors
    name = "unittest-%d.json" % os.getpid()
    tmp = os.path.join(obs.directory, "." + name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(facts, f, sort_keys=True)
    os.replace(tmp, os.path.join(obs.directory, name))
    # [C-32] Its facts are written whole: this start said what it saw.
    if obs.started_file:
        try:
            os.remove(obs.started_file)
        except OSError:
            pass
        obs.started_file = None


def _forked():
    """A forked child (a test's, a worker of Django's --parallel) observes nothing and writes
    nothing: its parent says what it couldn't follow."""
    obs = core.OBSERVER
    if obs is not None:
        obs.started_file = None
        obs.done = True


def start():
    """Starts the adapter in this process, when its command line runs the tests and no process of
    this run started it."""
    directory = os.environ.get(ENV_DIR)
    if core.OBSERVER is not None or not directory or not hasattr(sys, "addaudithook"):
        return
    if os.environ.get(SESSION_VAR) == directory:
        return
    if not runs_tests(getattr(sys, "orig_argv", None)):
        return
    with open(os.path.join(directory, "hushrun.json"), encoding="utf-8") as f:
        run = json.load(f)
    if run.get("schema") != RUN_SCHEMA or not run.get("root"):
        return
    # [C-66] Every process this one starts is a test's doing (or Django's --parallel workers'),
    # never another start of the runner by COMMAND.
    os.environ[SESSION_VAR] = directory
    core.RUNNER_DIRS = _unittest_dirs()
    core.RUNNER_OPENS = False
    core.RUNNER_OUTSIDE = "loaded by unittest outside any test file"
    # [ADR 0110] A script of the repository's that starts the runner (Django's manage.py) is under
    # every frame of the run: it is read by every test (it is opened as Python starts), and owns
    # no open or import of what it runs.
    script = _script(getattr(sys, "orig_argv", None))
    if script and script != "-":
        core.RUNNER_FILES = frozenset({os.path.abspath(script), os.path.realpath(script), script})
    observer = Observer(run["root"], directory)
    observer.static_names = ("__init__.py",)
    observer.wide_not_kept = ("the end of its %s failed (its tearDownClass or tearDownModule, or a "
                              "cleanup of theirs), which unittest reports on no test, so it keeps "
                              "no pass")
    observer.pid = os.getpid()
    observer.patterns = set()
    observer.loading_files = {}
    observer.claim_tried = False
    observer.runs = 0
    observer.opaque = False
    observer.unowned_failure = False
    observer.instrumented = {}
    observer.test_started = {}
    observer.stopped = set()
    observer.skips = set()
    observer.fixture_stack = []
    reuse = run.get("reuse") or []
    if isinstance(reuse, list) and all(isinstance(u, str) for u in reuse):
        observer.reuse = frozenset(reuse)
    python_files = run.get("pythonFiles")
    if isinstance(python_files, list) and all(isinstance(p, str) for p in python_files):
        observer.python_files = list(python_files)
    observer.by_test = run.get("unit") == "test"
    wants_watch = observer.by_test and run.get("reads") == "test"
    reuse_tests = run.get("reuseTests") or {}
    if observer.by_test and isinstance(reuse_tests, dict):
        observer.reuse_tests = {
            unit: frozenset(ids) for unit, ids in reuse_tests.items()
            if isinstance(unit, str) and isinstance(ids, list) and all(isinstance(i, str) for i in ids)
        }
    # Repository modules loaded before the adapter (a .pth file, the sitecustomize it hid) are
    # ambient.
    for name, module in list(sys.modules.items()):
        if module is not None and observer.module_file(module):
            observer.note_module(name)
            observer.make_ambient(name=name, why="loaded before the adapter")
    _patch()
    core.OBSERVER = observer
    core.FIRST_LOAD_HOOK = _first_load
    if wants_watch:
        try:
            observer.watch_reason = _start_watching(observer)
        except Exception as err:
            observer.watch_reason = "sys.monitoring couldn't be used (%r)" % (err,)
            observer.watching = False
    try:
        os.register_at_fork(after_in_child=_forked)
    except Exception:
        pass
    sys.meta_path.insert(0, _FirstLoadFinder())
    builtins.__import__ = _wrapped_import
    sys.addaudithook(_audit)
