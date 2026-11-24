"""hushrun's pytest adapter: for each test file pytest collects, what it read (ADR 0068, C-32).

"Read" is the spec's meaning: a test file read a file when it imported it, directly or through any
chain of imports, or opened it, while pytest collected it, in its fixtures or in its tests. It never
means "executed a line of it".

How it observes:

- A module graph. Every `import` statement goes through `builtins.__import__`, which this adapter
  wraps, so a request for a module that is already loaded is seen too. Each request adds an edge
  from the importing file to the imported module (and its parent packages, and the submodules a
  `from x import y` names). A finder at the front of `sys.meta_path` sees every first load, whatever
  asked for it, and who asked. Edges are kept once, whoever triggered them, and followed to the end
  from every test file when the session ends, so a module another test file loaded first is still on
  this one's list.
- File opens, through Python's audit hook (PEP 578). An open during a test file's collection or
  tests is that test file's. Every repository file on the call stack of an open also gets an edge
  to the opened file, so a value a module read once and cached reaches every test file that imports
  that module.
- conftest.py and package __init__.py files of every directory from the root to a test file are
  that test file's.
- A repository module that code outside the repository loaded first (a framework's settings, an
  entry point, a plugin registry) can hold state that later test files use without importing it.
  Such a module is ambient: it is read by the test file that was running and by every test file
  that runs after it, and by all of them when none was running.

What it can't see, it reports instead of hiding, for the test file running then, or for every test
file when none was: `subprocess` (anything a child process reads), `fork` (a forked child observes
nothing) and `native` (a repository extension module or a ctypes library, whose own reads are
invisible).

It decides nothing: hushrun decides each test file's state from these facts.

How it selects (ADR 0069): `hushrun.json`'s `reuse` lists the test files that already passed on the
files they read. After collection and every other deselection (-k, -m, other plugins), the process
that may deselect removes their tests, tells pytest they were deselected, and reports exactly which
test files it deselected. Only one pytest may: the first to start with this directory (it claims
it), or a pytest-xdist worker that this one started. A nested pytest, or a second pytest of the same
COMMAND, deselects nothing. Neither does one whose `python_files` isn't what hushrun read. When
everything left was deselected by it, pytest's "no tests collected" (exit 5) would turn a pass into
a fail, so that process's exit status becomes 0, and only then: any failure or error gives pytest
another status first.

Tests one by one (ADR 0080): when `hushrun.json`'s `unit` is `test`, it reports each test of each
test file, its id being its node id after `<path>::`: whether it finished, failed or was deselected,
and how long it took. It deselects, besides the test files `reuse` lists, the tests `reuseTests`
lists, by their file and id, under the same rules. What it couldn't follow inside one test's window
(its setup, body and teardown) is that test's alone; outside every window (collecting, importing, a
fixture of module, class or session scope, a teardown that may end one) it is the test file's.
Its file is then `hushrun.unit-facts/v1`. It decides nothing of tests either.

Watching a test (ADR 0083): when `reads` is `test` too, and on Python 3.12 or later, it reports
what each test touched, and hushrun makes each test's list from it (C-40). Through
`sys.monitoring`, every file a function of which started while the test's setup, body or teardown
ran: a module's top-level code or a class body running is no function, since that is what an
import is. Through the audit hook, the files it opened and the directories it listed. A module it
imported while it ran is read too. What a fixture of a wider scope than one test ran or opened as
it set up is a shared setup, read by every test that requests the fixture, directly or through
another, while pytest keeps its value. What ran or was opened while a test file was collected is
its `loaded`; its `conftest.py` and `__init__.py` files are what every test of it reads
(`everyTest`). Each file's direct imports (`imports`) and the files each module opened from its own
code (`opens`) come from the module graph it keeps already. A module code outside the repository
loaded is read by the test running then and every test after it in the process. Below 3.12 it
watches nothing, and says why (`settings.watch`).

It never changes pytest's outcome on its own error: every hook is guarded, any error of its own is
reported, which makes hushrun record nothing for the test files this process saw, and an error
while selecting deselects nothing.

Output: one file per process in $HUSHRUN_UNITS, written whole when the session ends (a temporary
file, then a rename). With pytest-xdist each worker writes its own, and the controller one with no
test files. Standard library only; Python 3.8 or later.
"""

import builtins
import json
import os
import sys
import time

import hushrun_observe as core  # noqa: E402
from hushrun_observe import (  # noqa: E402
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
    _call,
    _claim,
    _start_watching,
    _stop_watching,
    _wrapped_import,
)

__all__ = [
    "pytest_sessionstart",
    "pytest_collectstart",
    "pytest_itemcollected",
    "pytest_collectreport",
    "pytest_collection_modifyitems",
    "pytest_collection_finish",
    "pytest_runtest_protocol",
    "pytest_runtest_logreport",
    "pytest_fixture_setup",
    "pytest_fixture_post_finalizer",
    "pytest_runtest_teardown",
    "pytest_sessionfinish",
]


def _pytest_dirs():
    dirs = set()
    for name in ("_pytest", "pluggy", "py"):
        module = sys.modules.get(name)
        f = getattr(module, "__file__", None)
        # Only a package's own directory: pytest 7.2 and later ship `py` as one file, py.py, in
        # site-packages, whose directory is every other library's too.
        if not f or not hasattr(module, "__path__"):
            continue
        for d in {os.path.dirname(os.path.abspath(f)), os.path.dirname(os.path.realpath(f))}:
            dirs.add(d.rstrip(os.sep) + os.sep)
    return tuple(sorted(dirs))


def _shared_finishing(obs, step):
    obs.shared_finishing = max(0, obs.shared_finishing + step)


def _wrap_fixture_finish():
    """[ADR 0080] A fixture of a wider scope than one test ends where pytest says, which for a
    parametrized one switching its value is inside the next test's setup: what isn't followed
    while it ends is the test file's, as a teardown that may end one is, never that test's."""
    try:
        from _pytest.fixtures import FixtureDef
    except Exception:
        return
    original = FixtureDef.finish
    if getattr(original, "_hushrun", False):
        return

    def finish(self, *args, **kwargs):
        wide = getattr(self, "scope", "function") != "function"
        if wide:
            _call(_shared_finishing, 1)
        try:
            return original(self, *args, **kwargs)
        except BaseException:
            # [C-32] Its end failed: what it served didn't pass.
            if wide:
                _call(_wide_failed, self)
            raise
        finally:
            if wide:
                _call(_shared_finishing, -1)

    finish._hushrun = True
    FixtureDef.finish = finish


def _install():
    directory = os.environ.get(ENV_DIR)
    if core.OBSERVER is not None or not directory or not hasattr(sys, "addaudithook"):
        return
    with open(os.path.join(directory, "hushrun.json"), encoding="utf-8") as f:
        run = json.load(f)
    if run.get("schema") != RUN_SCHEMA or not run.get("root"):
        return
    import _pytest  # noqa: F401  (loaded already: pytest is importing this plugin)
    core.RUNNER_DIRS = _pytest_dirs()
    observer = Observer(run["root"], directory)
    reuse = run.get("reuse") or []
    if isinstance(reuse, list) and all(isinstance(u, str) for u in reuse):
        observer.reuse = frozenset(reuse)
    python_files = run.get("pythonFiles")
    if isinstance(python_files, list) and all(isinstance(p, str) for p in python_files):
        observer.python_files = list(python_files)
    # [ADR 0080] Tests one by one, and the tests the plan lets this run deselect.
    observer.by_test = run.get("unit") == "test"
    wants_watch = observer.by_test and run.get("reads") == "test"
    reuse_tests = run.get("reuseTests") or {}
    if observer.by_test and isinstance(reuse_tests, dict):
        observer.reuse_tests = {
            unit: frozenset(ids) for unit, ids in reuse_tests.items()
            if isinstance(unit, str) and isinstance(ids, list) and all(isinstance(i, str) for i in ids)
        }
    observer.claimed, observer.claimant = _claim(directory)
    # Repository modules loaded before the adapter (a .pth file, sitecustomize) are ambient.
    for name, module in list(sys.modules.items()):
        if module is not None and observer.module_file(module):
            observer.note_module(name)
            observer.make_ambient(name=name, why="loaded before the adapter")
    core.OBSERVER = observer
    _wrap_fixture_finish()
    if wants_watch:
        try:
            observer.watch_reason = _start_watching(observer)
        except Exception as err:
            observer.watch_reason = "sys.monitoring couldn't be used (%r)" % (err,)
            observer.watching = False
    sys.meta_path.insert(0, _FirstLoadFinder())
    builtins.__import__ = _wrapped_import
    sys.addaudithook(_audit)


try:
    _install()
except Exception:
    core.OBSERVER = None

import pytest  # noqa: E402


def _node_path(node):
    p = getattr(node, "path", None)
    if p is None:
        p = getattr(node, "fspath", None)
    return str(p) if p is not None else None


def _unit_of(node):
    p = _node_path(node)
    if not p or core.OBSERVER is None:
        return None
    return core.OBSERVER.rel(p)


def _test_id(nodeid):
    """[ADR 0080] A test's id: its node id after its file's path and `::`; None when it has none."""
    if not isinstance(nodeid, str) or "::" not in nodeid:
        return None
    return nodeid.split("::", 1)[1] or None


def _collectstart(obs, collector):
    if isinstance(collector, pytest.Session) or \
            (hasattr(pytest, "Package") and isinstance(collector, pytest.Package)):
        obs.stop()
        return
    p = _node_path(collector)
    unit = obs.rel(p) if p and os.path.isfile(p) else None
    if unit and isinstance(collector, pytest.File):
        obs.collect_started[collector.nodeid] = (unit, time.perf_counter())
        obs.unit_entry(unit)
        obs.start(unit)
    elif unit:
        obs.start(unit)
    else:
        obs.stop()


def _itemcollected(obs, item):
    unit = _unit_of(item)
    if unit:
        obs.unit_entry(unit)["collected"] += 1


def _collectreport(obs, report):
    started = obs.collect_started.pop(report.nodeid, None)
    if started:
        unit, t0 = started
        entry = obs.unit_entry(unit)
        entry["duration"] += time.perf_counter() - t0
        if report.failed:
            entry["failed"] = True
        # [C-32] Skipped whole as it loaded (`pytest.skip(allow_module_level=True)`,
        # `pytest.importorskip`): a skip of its unit, which in a run that failed proves nothing.
        if report.skipped:
            entry["skipped"] = entry.get("skipped", 0) + 1
    if report.failed and not started:
        unit = obs.rel(str(report.fspath)) if getattr(report, "fspath", None) else None
        if unit and os.path.isfile(os.path.join(obs.root, unit)):
            obs.unit_entry(unit)["failed"] = True
    obs.stop()


def _collection_finish(obs, session):
    # What will run, after every deselection (-k, -m, other plugins): a test file whose selected
    # tests all finished ran whole.
    for item in getattr(session, "items", ()):
        unit = _unit_of(item)
        if unit:
            obs.unit_entry(unit)["selected"] += 1
    obs.collection_finished = True


def _logreport(obs, report):
    unit = obs.active
    if not unit:
        return
    if report.when == "teardown":
        obs.torn_ids.add(report.nodeid)
    entry = obs.unit_entry(unit)
    duration = getattr(report, "duration", 0.0) or 0.0
    entry["duration"] += duration
    if report.failed:
        # A failure, or an error in setup or teardown; pytest reports a strict xpass failed too.
        entry["failed"] = True
    # [C-32] Its body didn't run: a skip or skipif mark, a skip raised in setup or in the test, an
    # xfail that isn't run. An xfail that ran, and failed as expected, ran.
    skipped = report.skipped and not (report.when == "call" and hasattr(report, "wasxfail"))
    if skipped and report.nodeid not in obs.skipped_ids:
        obs.skipped_ids.add(report.nodeid)
        entry["skipped"] = entry.get("skipped", 0) + 1
    if obs.by_test and obs.active_test is not None:
        test = obs.test_entry(unit, obs.active_test)
        test["duration"] += duration
        if report.failed:
            test["failed"] = True
        if skipped:
            test["skipped"] = True


def _protocol_start(obs, unit, test_id, fixtures):
    obs.unit_entry(unit)
    obs.start(unit)
    obs.active_test = test_id
    obs.shared_depth = 0
    obs.shared_teardown = False
    obs.setup_stack = []
    if obs.by_test and test_id is not None:
        obs.test_entry(unit, test_id)
        if obs.watching:
            tick = obs.tick()
            obs.test_window[(unit, test_id)] = [tick, tick, obs.seq, frozenset(fixtures)]


def _served(obs, unit, test_id, fixtures):
    """[ADR 0083] The fixtures a test was served, as its request has them once it ran: those it
    requested and those it asked for at run time (`request.getfixturevalue`)."""
    window = obs.test_window.get((unit, test_id))
    if window is not None and fixtures:
        window[3] = window[3] | frozenset(fixtures)


def _protocol_end(obs, unit, test_id, nodeid, cut):
    # [C-32] A test finished only once its teardown's report was logged, and no `pytest.exit` or
    # interrupt came up through its protocol: one cut short didn't finish, and its file didn't run
    # whole.
    finished = not cut and nodeid in obs.torn_ids
    if finished:
        obs.unit_entry(unit)["finished"] += 1
        obs.finished_ids[nodeid] = (unit, test_id)
    if obs.by_test and test_id is not None:
        if finished:
            obs.test_entry(unit, test_id)["finished"] = True
        else:
            obs.test_entry(unit, test_id)
        window = obs.test_window.get((unit, test_id))
        if window is not None:
            window[1] = obs.tick()
            window[2] = obs.seq
    obs.active_test = None
    obs.shared_depth = 0
    obs.shared_teardown = False
    obs.stop()


def _may_deselect(obs, config):
    """Whether this process may deselect what hushrun.json lists: the one that claimed the run's
    directory, or a pytest-xdist worker it started; and only when pytest's python_files is the one
    hushrun read, so that the plan's test files are pytest's."""
    if not (obs.reuse or obs.reuse_tests) or obs.python_files is None:
        return False
    worker = getattr(config, "workerinput", None)
    if worker is None:
        allowed = obs.claimed
    else:
        allowed = obs.claimant is not None and os.getppid() == obs.claimant
    return allowed and list(config.getini("python_files")) == obs.python_files


def _modifyitems(obs, config, items):
    if not _may_deselect(obs, config):
        return
    keep, gone, by_unit, tests_gone = [], [], {}, []
    for item in items:
        unit = _unit_of(item)
        test_id = _test_id(getattr(item, "nodeid", None)) if obs.by_test else None
        if unit is not None and unit in obs.reuse:
            gone.append(item)
            by_unit[unit] = by_unit.get(unit, 0) + 1
            if test_id is not None:
                tests_gone.append((unit, test_id))
        elif unit is not None and test_id is not None and test_id in obs.reuse_tests.get(unit, ()):
            # [ADR 0080] A test the plan lists one by one: its file runs, without it.
            gone.append(item)
            tests_gone.append((unit, test_id))
        else:
            keep.append(item)
    if not gone:
        return
    # pytest is told first: if that fails, nothing was deselected.
    config.hook.pytest_deselected(items=gone)
    items[:] = keep
    obs.removed += len(gone)
    for unit, n in by_unit.items():
        obs.deselected[unit] = obs.deselected.get(unit, 0) + n
    for unit, test_id in tests_gone:
        obs.test_entry(unit, test_id)["deselected"] = True


def _workers_removed(obs):
    """What the pytest-xdist workers of this process deselected, from the files they wrote as
    their sessions ended, which is before this one's."""
    removed = 0
    for name in os.listdir(obs.directory):
        if not name.endswith(".json") or name.startswith(".") or name == "hushrun.json":
            continue
        try:
            with open(os.path.join(obs.directory, name), encoding="utf-8") as f:
                facts = json.load(f)
        except Exception:
            continue
        if facts.get("adapter") == "pytest" and facts.get("parent") == os.getpid():
            removed += int(facts.get("removedItems") or 0)
    return removed


SESSION_VAR = "HUSHRUN_PYTEST_SESSION"


def _session_started(obs, session):
    """[C-32] The main process (an xdist controller, not a worker, whose death its tests already
    say) says it began: a `.started` file it removes once its facts are written. One left after
    COMMAND means a start died before it said what it saw.

    A pytest a test of this run started (its own process environment names this run's directory
    in `HUSHRUN_PYTEST_SESSION`, which a session of the run sets in its own, so that only its
    descendants have it) is the test's doing, which isn't followed, never a start of the runner by
    COMMAND: it makes none. A second pytest COMMAND starts (`pytest a; pytest b`) doesn't inherit
    it, and makes its own."""
    if getattr(session.config, "workerinput", None) is not None:
        return
    if os.environ.get(SESSION_VAR) == obs.directory:
        return
    os.environ[SESSION_VAR] = obs.directory
    path = os.path.join(obs.directory, "pytest-%d.started" % os.getpid())
    with open(path, "w", encoding="utf-8") as f:
        f.write(str(os.getpid()))
    obs.started_file = path


@pytest.hookimpl(tryfirst=True)
def pytest_sessionstart(session):
    _call(_session_started, session)


def pytest_collectstart(collector):
    _call(_collectstart, collector)


def pytest_itemcollected(item):
    _call(_itemcollected, item)


def pytest_collectreport(report):
    _call(_collectreport, report)


@pytest.hookimpl(trylast=True)
def pytest_collection_modifyitems(session, config, items):
    _call(_modifyitems, config, items)


def pytest_collection_finish(session):
    _call(_collection_finish, session)


def pytest_runtest_logreport(report):
    _call(_logreport, report)


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_protocol(item, nextitem):
    unit = None
    test_id = None
    if core.OBSERVER is not None and not core.OBSERVER.done:
        try:
            unit = _unit_of(item)
            test_id = _test_id(getattr(item, "nodeid", None))
        except Exception:
            unit = None
    fixtures = ()
    try:
        fixtures = tuple(getattr(item, "fixturenames", ()) or ())
    except Exception:
        fixtures = ()
    if unit:
        _call(_protocol_start, unit, test_id, fixtures)
    outcome = yield
    if unit:
        try:
            cut = getattr(outcome, "excinfo", None) is not None
        except Exception:
            cut = True
        _call(_served, unit, test_id, _request_fixtures(item))
        # What it was served by name, for a test whose teardown never ran (its request is gone by
        # now, so its definitions are the teardown's to note).
        _call(_note_served, getattr(item, "nodeid", None), fixtures, ())
        _call(_protocol_end, unit, test_id, getattr(item, "nodeid", None), cut)


def _shared_setup(obs, fixturedef, step):
    """[ADR 0080] A fixture of a wider scope than one test sets up: what happens meanwhile serves
    every test that requests it, and is the test file's. [ADR 0083] What it runs and opens is a
    shared setup's."""
    if getattr(fixturedef, "scope", "function") == "function":
        return
    obs.shared_depth = max(0, obs.shared_depth + step)
    if not obs.watching:
        return
    # Each start and end of a shared setup watches anew, so that one function is seen by every
    # setup and test that calls it, not only the first.
    obs.rewatch()
    if not obs.watching:
        return
    if step > 0:
        setup = {"argname": getattr(fixturedef, "argname", None), "ran": set(), "opened": set(),
                 "start": obs.tick(), "end": None, "def": id(fixturedef)}
        obs.setups.append(setup)
        obs.setup_stack.append(setup)
    elif obs.setup_stack:
        obs.setup_stack.pop()


def _shared_finalized(obs, fixturedef):
    """[ADR 0083] pytest no longer keeps a shared fixture's value: tests after this don't read it."""
    if not obs.watching or getattr(fixturedef, "scope", "function") == "function":
        return
    for setup in reversed(obs.setups):
        if setup["def"] == id(fixturedef) and setup["end"] is None:
            setup["end"] = obs.tick()
            break


def pytest_fixture_post_finalizer(fixturedef, request):
    _call(_shared_finalized, fixturedef)


@pytest.hookimpl(hookwrapper=True)
def pytest_fixture_setup(fixturedef, request):
    _call(_shared_setup, fixturedef, 1)
    try:
        yield
    finally:
        _call(_shared_setup, fixturedef, -1)


def _teardown_shares(obs, item, nextitem):
    """[ADR 0080] A teardown that may end fixtures of a wider scope: the last test of its file or
    class, or of the session. What happens meanwhile is the test file's."""
    obs.shared_teardown = nextitem is None or getattr(nextitem, "parent", None) is not \
        getattr(item, "parent", None)


def _note_served(obs, nodeid, names, def_ids):
    """[C-32] The fixtures a test was served, by name and by definition: what a wider fixture whose
    end fails takes the pass of. Noted while its request is alive (its teardown, before any fixture
    ends: pytest drops the request once the protocol ends), and added to."""
    had_names, had_defs = obs.served.get(nodeid, (frozenset(), frozenset()))
    obs.served[nodeid] = (had_names | frozenset(names), had_defs | frozenset(def_ids))


def _request_fixture_defs(item):
    """The ids of the fixture definitions `item`'s request resolved; none when pytest doesn't say,
    and then fixtures are matched by name."""
    try:
        return tuple(id(d) for d in item._request._fixture_defs.values())
    except Exception:
        return ()


def _wide_failed(obs, fixturedef):
    obs.failed_wide.append((getattr(fixturedef, "argname", None), id(fixturedef)))


def _request_fixtures(item):
    """The names of every fixture `item`'s request resolved, a fixture asked for at run time
    included; none when pytest doesn't say."""
    try:
        return tuple(item._request.fixturenames)
    except Exception:
        return ()


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_teardown(item, nextitem):
    _call(_teardown_shares, item, nextitem)
    if core.OBSERVER is not None and not core.OBSERVER.done:
        _call(lambda obs: obs.active and _served(obs, obs.active, obs.active_test,
                                                 _request_fixtures(item)))
        # [C-32] What it was served, by definition, while its request still says: before any of
        # its fixtures ends, a wider one whose end may fail included.
        _call(_note_served, getattr(item, "nodeid", None), _request_fixtures(item),
              _request_fixture_defs(item))
    yield


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session, exitstatus):
    obs = core.OBSERVER
    if obs is None or obs.done:
        return
    try:
        with obs.lock:
            obs.stop()
            _stop_watching()
            config = session.config
            worker = getattr(config, "workerinput", None)
            is_controller = bool(getattr(config.option, "numprocesses", None)) and not worker
            try:
                units = [] if is_controller else obs.results()
            except Exception as err:
                obs.errors.append("results: %r" % (err,))
                units = []
            obs.done = True
            # pytest says no test ran only because this process, or its workers, deselected every
            # test that was left: the tests hushrun.json listed already passed on the files they
            # read, so the session passed. Any failure or error makes pytest's status another.
            try:
                if worker is None and obs.claimed and int(session.exitstatus) == NO_TESTS_COLLECTED:
                    if obs.removed + _workers_removed(obs) > 0:
                        session.exitstatus = 0
            except Exception as err:
                obs.errors.append("exitstatus: %r" % (err,))
            try:
                python_files = list(config.getini("python_files"))
            except Exception:
                python_files = None
            # [ADR 0072] What explains a path, never what decides: a failure here costs only the
            # path `hushrun why` would show.
            try:
                edges = [] if is_controller else obs.edge_list()
            except Exception:
                edges = []
            settings = {"pythonFiles": python_files, "python": sys.version.split()[0],
                        "pytest": pytest.__version__}
            if obs.watch_reason:
                settings["watch"] = obs.watch_reason
            # [C-32] One start: a worker's is its controller's (xdist's testrunuid, which every
            # worker of one start has), so tests are counted within it and never across starts.
            if worker:
                start = "xdist-%s" % (worker.get("testrunuid") or "parent-%d" % os.getppid())
            else:
                start = START
            facts = {
                "schema": SCHEMA_V1 if obs.by_test else SCHEMA,
                "adapter": "pytest",
                "start": start,
                "pid": os.getpid(),
                "worker": (worker or {}).get("workerid") if worker else None,
                "controller": is_controller,
                "failuresByUnit": True,
                "settings": settings,
                "notFollowed": [] if is_controller else
                [{"why": k, "detail": d} for k, v in sorted(obs.session_not_followed.items())
                 for d in sorted(v)],
                "units": units,
                "edges": edges,
                "errors": obs.errors,
                "removedItems": obs.removed,
                "parent": os.getppid(),
            }
            if obs.watching and not is_controller:
                try:
                    imports, opens = obs.watch_tables()
                    facts["imports"] = imports
                    facts["opens"] = opens
                    facts["reexports"] = obs.reexport_table()
                    facts["paths"] = obs.paths
                except Exception as err:
                    # Without its tables no test of this process can be told on its own.
                    obs.errors.append("watch tables: %r" % (err,))
                    for unit in units:
                        unit.pop("loaded", None)
                    facts["errors"] = obs.errors
            name = "pytest-%d%s.json" % (os.getpid(), ("-" + facts["worker"]) if facts["worker"] else "")
            tmp = os.path.join(obs.directory, "." + name + ".tmp")
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(facts, f, sort_keys=True)
            os.replace(tmp, os.path.join(obs.directory, name))
            # [C-32] Its facts are written whole: this start ended.
            if obs.started_file:
                try:
                    os.remove(obs.started_file)
                except OSError:
                    pass
    except Exception:
        obs.done = True
