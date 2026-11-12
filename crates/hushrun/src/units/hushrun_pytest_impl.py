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

Tests one by one (ADR 0077): when `hushrun.json`'s `unit` is `test`, it reports each test of each
test file, its id being its node id after `<path>::`: whether it finished, failed or was deselected,
and how long it took. It deselects, besides the test files `reuse` lists, the tests `reuseTests`
lists, by their file and id, under the same rules. What it couldn't follow inside one test's window
(its setup, body and teardown) is that test's alone; outside every window (collecting, importing, a
fixture of module, class or session scope, a teardown that may end one) it is the test file's.
Its file is then `hushrun.unit-facts/v1`. It decides nothing of tests either.

Watching a test (ADR 0080): when `reads` is `test` too, and on Python 3.12 or later, it reports
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
import importlib
import inspect
import importlib.util
import json
import os
import sys
import threading
import time
import uuid

__all__ = [
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

SCHEMA = "hushrun.unit-facts/v0"
SCHEMA_V1 = "hushrun.unit-facts/v1"
RUN_SCHEMA = "hushrun.units/v0"
ENV_DIR = "HUSHRUN_UNITS"

THIS_FILE = os.path.abspath(__file__)
if THIS_FILE.endswith((".pyc", ".pyo")):
    THIS_FILE = THIS_FILE[:-1]
SHIM_FILE = os.path.join(os.path.dirname(THIS_FILE), "hushrun_pytest.py")
IMPORTLIB_DIR = os.path.dirname(os.path.abspath(importlib.__file__)) + os.sep

SPAWN_EVENTS = {
    "subprocess.Popen": "subprocess",
    "os.system": "subprocess",
    "os.exec": "subprocess",
    "os.posix_spawn": "subprocess",
    "os.spawn": "subprocess",
    "os.startfile": "subprocess",
    "os.fork": "fork",
    "os.forkpty": "fork",
    "pty.spawn": "subprocess",
}
# Importers that only look a module up and keep no state from it.
TRANSIENT_IMPORTERS = ("unittest/mock.py", "pkgutil.py", "inspect.py", "pydoc.py")
# The directories pytest's own code is in, as pytest has them loaded: never a name in a path, which
# a repository or a virtual environment can hold too (a `py/` directory).
PYTEST_DIRS = ()
NATIVE_SUFFIXES = (".so", ".pyd", ".dylib", ".dll")
PYTEST_OUTSIDE = "loaded by pytest outside any test file"
MOST_ERRORS = 20
CLAIM_FILE = "claim"
NO_TESTS_COLLECTED = 5
# [C-32] This start of pytest, which a pytest-xdist worker takes from its controller instead.
START = uuid.uuid4().hex


def _own(fn):
    return fn == THIS_FILE or fn == SHIM_FILE


class Observer(object):
    def __init__(self, root, directory):
        self.root = os.path.realpath(root)
        self.directory = directory
        self.root_prefix = self.root.rstrip(os.sep) + os.sep
        self.third_party_prefixes = tuple(
            os.path.realpath(p).rstrip(os.sep) + os.sep
            for p in {sys.prefix, sys.base_prefix, sys.exec_prefix}
            if p and not self.root.startswith(os.path.realpath(p).rstrip(os.sep) + os.sep)
        )
        self.lock = threading.RLock()
        self.active = None            # the unit whose phase is running, repository-relative
        self.seq = 0                  # increases at every phase start
        self.unit_last_seq = {}       # unit -> seq of its latest phase
        self.edges = {}               # repo file -> set(repo files): opened from a frame of it
        self.name_edges = {}          # repo file -> set(module names) it asked for
        self.roots = {}               # unit -> set(repo files)
        self.root_names = {}          # unit -> set(module names)
        self.ambient = []             # [seq, file or None, module name or None, why]
        self.mod_files = {}           # module name -> set(repo files) it was loaded from
        self.not_followed = {}        # unit -> {reason: set(details)}
        self.session_not_followed = {}
        self.units = {}               # unit -> facts being gathered
        self.collect_started = {}
        self.collection_finished = False
        self.errors = []
        self.done = False
        self.reuse = frozenset()      # test files hushrun.json lets this run deselect
        self.python_files = None      # pytest's python_files as hushrun read it
        self.claimed = False          # this process claimed the directory
        self.claimant = None          # the pid of the process that did
        self.removed = 0              # tests this process deselected
        self.deselected = {}          # unit -> tests of it this process deselected
        self._rel_cache = {}
        self._import_seen = set()
        self._scanned = 0
        self.unit_roots = {}          # unit -> the files it read directly, as results() found them
        self.paths = []               # [ADR 0080] the file's table of paths
        self.path_index = {}
        self.by_test = False          # hushrun.json's unit is test: tests are reported one by one
        self.reuse_tests = {}         # unit -> frozenset(ids) hushrun.json lets this run deselect
        self.tests = {}               # unit -> {id: facts of the test being gathered}
        self.test_order = {}          # unit -> [ids, in the order they started or were deselected]
        self.active_test = None       # the id of the test whose protocol is running
        self.shared_depth = 0         # >0 while a fixture of a wider scope than a test sets up
        self.shared_teardown = False  # a teardown that may end fixtures of a wider scope
        self.test_not_followed = {}   # (unit, id) -> {reason: set(details)}
        self.watching = False         # [ADR 0080] each test's code, opens and listings are watched
        self.watch_reason = None      # why tests aren't watched, when reads is test
        self.clock = 0                # ticks at each test and shared setup's start and end
        self.test_ran = {}            # (unit, id) -> set(files a function of which started)
        self.test_opened = {}         # (unit, id) -> set(files opened)
        self.test_dirs = {}           # (unit, id) -> set(directories listed)
        self.test_window = {}         # (unit, id) -> [first tick, last tick, last seq, fixtures]
        self.loaded_ran = {}          # unit -> set(files a function of which started as it loaded)
        self.loaded_opened = {}       # unit -> set(files opened as it loaded)
        self.setups = []              # shared setups: {argname, ran, opened, start, end}
        self.setup_stack = []         # the shared setups setting up now, innermost last
        self.code_files = {}          # code filename -> repository file or None
        self.test_names = {}          # (unit, id) -> module names loaded for it by a helper
        self.skipped_ids = set()      # node ids of tests whose body didn't run: skipped
        self.torn_ids = set()         # node ids of tests whose teardown report was logged
        self.session_ran = set()      # [ADR 0080] files a function of which started outside every
        self.session_opened = set()   # window, and files opened there: every test reads them
        self.shared_finishing = 0     # >0 while a fixture of a wider scope than a test is ended

    # --- paths -------------------------------------------------------------------------

    def rel(self, path):
        """Repository-relative path of a file, or None when it is not the repository's own."""
        # A relative path names a file from the directory the process is in now: keyed by the
        # absolute path, a later chdir finds the file it names there, never the one cached.
        if not os.path.isabs(path):
            try:
                path = os.path.join(os.getcwd(), path)
            except Exception:
                return None
        cached = self._rel_cache.get(path)
        if cached is not None or path in self._rel_cache:
            return cached
        result = None
        try:
            real = os.path.realpath(path)
            if real.startswith(self.root_prefix) and not real.startswith(self.third_party_prefixes):
                relative = real[len(self.root_prefix):]
                parts = relative.split(os.sep)
                if not ({".git", "__pycache__", "site-packages", "dist-packages"} & set(parts)) \
                        and not relative.endswith((".pyc", ".pyo")) and not _own(real):
                    result = "/".join(parts)
        except Exception:
            result = None
        self._rel_cache[path] = result
        return result

    def module_file(self, module):
        f = getattr(module, "__file__", None)
        if not f:
            spec = getattr(module, "__spec__", None)
            f = getattr(spec, "origin", None) if spec is not None else None
        if not f or not isinstance(f, str) or f in ("built-in", "frozen"):
            return None
        if f.endswith((".pyc", ".pyo")):
            source = f[:-1]
            if os.path.exists(source):
                f = source
        return self.rel(f)

    def note_module(self, name):
        module = sys.modules.get(name)
        if module is None:
            return
        f = self.module_file(module)
        if f:
            self.mod_files.setdefault(name, set()).add(f)

    def scan_modules(self):
        # Files for modules that were loaded without passing through a wrapped call.
        modules = list(sys.modules.items())
        if len(modules) == self._scanned:
            return
        self._scanned = len(modules)
        for name, module in modules:
            if name not in self.mod_files and module is not None:
                f = self.module_file(module)
                if f:
                    self.mod_files[name] = {f}

    # --- frames ------------------------------------------------------------------------

    @staticmethod
    def requester(frame):
        """The first frame that is neither this adapter's, importlib's nor a frozen one."""
        while frame is not None:
            fn = frame.f_code.co_filename
            if fn.startswith("<") or _own(fn) or fn.startswith(IMPORTLIB_DIR):
                frame = frame.f_back
                continue
            return frame
        return None

    def repo_frames(self, frame):
        seen = []
        while frame is not None:
            fn = frame.f_code.co_filename
            if not _own(fn) and not fn.startswith("<"):
                r = self.rel(fn)
                if r and r not in seen:
                    seen.append(r)
            frame = frame.f_back
        return seen

    # --- attribution -------------------------------------------------------------------

    def add_root(self, unit, f=None, name=None):
        if f:
            self.roots.setdefault(unit, set()).add(f)
        if name:
            self.root_names.setdefault(unit, set()).add(name)

    def make_ambient(self, f=None, name=None, why=""):
        self.ambient.append([self.seq, f, name, why])

    def on_import(self, name, globals_, fromlist, level, frame):
        """A successful `import` statement (or __import__ call)."""
        importer_file = None
        package = None
        if globals_:
            importer_file = globals_.get("__file__")
            package = globals_.get("__package__")
            if package is None and level:
                modname = globals_.get("__name__", "")
                package = modname if "__path__" in globals_ else modname.rpartition(".")[0]
        importer = self.rel(importer_file) if isinstance(importer_file, str) else None
        if importer_file is None:
            req = self.requester(frame)
            importer_file = req.f_code.co_filename if req is not None else None
            importer = self.rel(importer_file) if importer_file else None
        key = (importer_file, name, level, tuple(fromlist) if fromlist else ())
        if importer and key in self._import_seen:
            return
        if level:
            try:
                absname = importlib.util.resolve_name("." * level + name, package) if name \
                    else importlib.util.resolve_name("." * level, package)
            except Exception:
                return
        else:
            absname = name
        names = []
        parts = absname.split(".")
        for i in range(1, len(parts) + 1):
            names.append(".".join(parts[:i]))
        if fromlist:
            module = sys.modules.get(absname)
            items = list(fromlist)
            if "*" in items and module is not None:
                items = list(getattr(module, "__all__", []) or [])
            for item in items:
                if isinstance(item, str) and (absname + "." + item) in sys.modules:
                    names.append(absname + "." + item)
        for n in names:
            if n not in self.mod_files:
                self.note_module(n)
        # [ADR 0080] A module imported while a test or a shared setup runs is read by it.
        if self.watching and self.active and (self.active_test is not None or self.setup_stack):
            self.touched("ran", self.resolve(names))
        if importer:
            self.name_edges.setdefault(importer, set()).update(names)
            self._import_seen.add(key)
            return
        # Asked for by code outside the repository: the running test file asked for it, and so did
        # the nearest repository frame on the stack, if any. Only repository modules matter here.
        names = [n for n in names if self.mod_files.get(n)]
        if not names:
            return
        if self.active:
            for n in names:
                self.add_root(self.active, name=n)
        frames = self.repo_frames(frame)
        if frames:
            self.name_edges.setdefault(frames[0], set()).update(names)

    def on_first_load(self, fullname, frame):
        """A module is about to be loaded for the first time (it is not in sys.modules)."""
        req = self.requester(frame)
        req_file = req.f_code.co_filename if req is not None else ""
        req_rel = self.rel(req_file) if req_file else None
        if req_rel:
            self.name_edges.setdefault(req_rel, set()).add(fullname)
            return
        if req_file.startswith(PYTEST_DIRS):
            # pytest collecting a test file, a conftest or a plugin named in pytest_plugins.
            if self.active:
                self.add_root(self.active, name=fullname)
            else:
                self.make_ambient(name=fullname, why=PYTEST_OUTSIDE)
            return
        frames = self.repo_frames(req)
        if frames:
            self.name_edges.setdefault(frames[0], set()).add(fullname)
        if req_file.replace(os.sep, "/").endswith(TRANSIENT_IMPORTERS):
            if self.active:
                self.add_root(self.active, name=fullname)
                # [ADR 0080] Loaded for the test running now, through a helper outside the
                # repository that keeps nothing: the test read it.
                if self.watching and self.active_test is not None:
                    self.test_names.setdefault((self.active, self.active_test), set()).add(fullname)
            return
        # Code outside the repository loaded it and may keep its state: every later test file may
        # use it.
        self.make_ambient(name=fullname, why="first loaded by " + os.path.basename(req_file or "?"))

    def on_open(self, args, frame):
        path = args[0] if args else None
        if isinstance(path, int) or path is None:
            return
        if isinstance(path, bytes):
            path = os.fsdecode(path)
        elif not isinstance(path, str):
            try:
                path = os.fspath(path)
            except TypeError:
                return
            if isinstance(path, bytes):
                path = os.fsdecode(path)
        r = self.rel(path)
        if r is None:
            return
        mode = args[1] if len(args) > 1 else None
        flags = args[2] if len(args) > 2 else 0
        if isinstance(mode, str):
            reading = "r" in mode or "+" in mode
        else:
            reading = (flags or 0) & 3 != os.O_WRONLY
        try:
            if os.path.isdir(os.path.join(self.root, r)):
                return
        except Exception:
            return
        if not reading:
            return
        owner, owner_frame = self.owner(frame)
        if owner in ("import", "pytest"):
            # The import system or pytest reading a test file, a conftest or a module: imports
            # are the module graph's, and pytest's own reads belong to the file it collects.
            if self.active:
                self.add_root(self.active, f=r)
            return
        if owner == "repo":
            for fr in self.repo_frames(owner_frame):
                if fr != r:
                    self.edges.setdefault(fr, set()).add(r)
        if self.active:
            self.add_root(self.active, f=r)
            self.touched("opened", {r})
        else:
            # [ADR 0080] Opened while no window is open: every test of the session reads it.
            self.touched("opened", {r})
            if owner != "repo":
                self.make_ambient(f=r, why="opened outside any test file by code outside the repository")

    def owner(self, frame):
        """Who an open belongs to: the first frame, from the open outwards, that is the import
        system's, pytest's or the repository's ("other" when only code outside it is there)."""
        while frame is not None:
            fn = frame.f_code.co_filename
            if fn.startswith("<frozen importlib"):
                return "import", frame
            if not _own(fn) and not fn.startswith("<"):
                if self.rel(fn):
                    return "repo", frame
                if fn.startswith(PYTEST_DIRS):
                    return "pytest", frame
            frame = frame.f_back
        return "other", None

    def on_spawn(self, event, args, frame):
        reason = SPAWN_EVENTS[event]
        detail = event
        try:
            if event == "subprocess.Popen":
                argv = args[1] if len(args) > 1 else None
                exe = args[0]
                if exe is None and argv:
                    exe = argv if isinstance(argv, (str, bytes)) else argv[0]
                detail = os.path.basename(os.fsdecode(exe)) if exe else event
            elif event == "os.system":
                detail = str(args[0]).split()[0] if args and args[0] else event
            elif event in ("os.exec", "os.posix_spawn", "os.spawn"):
                path = args[1] if event == "os.spawn" else args[0]
                detail = os.path.basename(os.fsdecode(path))
        except Exception:
            pass
        if self.active:
            target = self.not_followed_target()
        elif self.repo_frames(frame):
            # Outside any test file, by the repository's own code (a conftest.py that generates
            # files as it loads): it may change what every test file reads.
            target = self.session_not_followed
        else:
            # Outside any test file, by code outside the repository alone: the runner's own, such
            # as pytest-xdist's workers asking `uname` where they run. No test file asked for it.
            return
        target.setdefault(reason, set()).add(detail)

    def on_dlopen(self, name):
        if isinstance(name, bytes):
            name = os.fsdecode(name)
        r = self.rel(name) if isinstance(name, str) and os.path.sep in name else None
        if r:
            target = self.not_followed_target() if self.active else self.session_not_followed
            target.setdefault("native", set()).add(r)

    def not_followed_target(self):
        """Where something not followed while a unit is active goes: the running test's own, with
        tests one by one and inside its window alone; else the unit's, for all its tests."""
        if self.by_test and self.active_test is not None and not self.shared_depth \
                and not self.shared_teardown and not self.shared_finishing:
            return self.test_not_followed.setdefault((self.active, self.active_test), {})
        return self.not_followed.setdefault(self.active, {})

    # --- watching a test [ADR 0080] -------------------------------------------------------

    def code_file(self, filename):
        f = self.code_files.get(filename, False)
        if f is False:
            f = self.rel(filename) if filename and not filename.startswith("<") else None
            self.code_files[filename] = f
        return f

    def touched(self, kind, files):
        """Files a function of which started (`ran`) or that were opened (`opened`), for the shared
        setup setting up now, the test running now, or the test file loading now."""
        if not self.watching or not files:
            return
        if self.setup_stack:
            self.setup_stack[-1][kind].update(files)
        elif self.active_test is not None and self.active:
            key = (self.active, self.active_test)
            target = self.test_ran if kind == "ran" else self.test_opened
            target.setdefault(key, set()).update(files)
        elif self.active:
            target = self.loaded_ran if kind == "ran" else self.loaded_opened
            target.setdefault(self.active, set()).update(files)
        else:
            # No window is open: a session hook (pytest_configure, pytest_sessionstart,
            # pytest_collection_modifyitems, pytest_collection_finish) of a conftest.py or a
            # plugin of the repository. What runs then is read by every test of the session.
            (self.session_ran if kind == "ran" else self.session_opened).update(files)

    def listed(self, path):
        if not self.watching or self.active_test is None or not self.active or self.setup_stack:
            return
        r = self.rel(path) if path is not None else None
        if r is None and path is not None:
            try:
                real = os.path.realpath(path if os.path.isabs(path) else os.path.join(os.getcwd(), path))
            except Exception:
                return
            if real.rstrip(os.sep) == self.root:
                r = ""
        if r is not None:
            self.test_dirs.setdefault((self.active, self.active_test), set()).add(r)

    def tick(self):
        self.clock += 1
        return self.clock

    def test_entry(self, unit, test_id):
        tests = self.tests.setdefault(unit, {})
        if test_id not in tests:
            tests[test_id] = {"finished": False, "failed": False, "deselected": False,
                              "duration": 0.0, "skipped": False}
            self.test_order.setdefault(unit, []).append(test_id)
        return tests[test_id]

    # --- phases ------------------------------------------------------------------------

    def start(self, unit):
        self.rewatch()
        self.seq += 1
        self.active = unit
        self.unit_last_seq[unit] = self.seq
        self.scan_modules()

    def stop(self):
        if self.active:
            self.unit_last_seq[self.active] = self.seq
        self.active = None
        # What runs outside every window from here is the session's: seen anew.
        self.rewatch()

    def rewatch(self):
        """[ADR 0080] A window starts or ends: functions are watched anew, so that each one is seen
        in every window that calls it. Only while no other tool uses sys.monitoring: restarting
        events re-arms every tool's, and one that appeared stops the watching for the rest of the
        process, said once."""
        if not self.watching:
            return
        other = _other_tool()
        if other is not None:
            self.stop_watching("%s started using sys.monitoring during the run" % other)
            return
        _restart_watch()

    def stop_watching(self, why):
        """[ADR 0080] Stops watching for the rest of the process and releases the tool id: this
        process watched no test then, and each reads its test file's list."""
        _stop_watching()
        self.watching = False
        if self.watch_reason is None:
            self.watch_reason = "%s, which watching a test would disturb" % why

    def unit_entry(self, unit):
        return self.units.setdefault(unit, {"collected": 0, "selected": 0, "finished": 0,
                                             "failed": False, "duration": 0.0, "skipped": 0})

    # --- the end -----------------------------------------------------------------------

    def static_roots(self, unit):
        found = []
        parts = unit.split("/")
        for i in range(len(parts)):
            d = parts[:i]
            for name in ("conftest.py", "__init__.py"):
                candidate = "/".join(d + [name])
                if candidate != unit and os.path.isfile(os.path.join(self.root, *candidate.split("/"))):
                    found.append(candidate)
        return found

    def resolve(self, names):
        out = set()
        for n in names:
            out.update(self.mod_files.get(n, ()))
        return out

    @staticmethod
    def closure(roots, file_edges):
        seen = set()
        pending = list(roots)
        while pending:
            f = pending.pop()
            if f in seen:
                continue
            seen.add(f)
            pending.extend(file_edges.get(f, ()))
        return seen

    def edge_list(self):
        """[ADR 0072] The direct edges seen, for `hushrun why` to show a path: a file opened
        another, a file imported another, or a test file read another while it ran through no edge
        of its own (pytest collecting it, a conftest.py above it, a module code outside the
        repository loaded). Explanations only: hushrun keys nothing by them."""
        out = set()
        for f, targets in self.edges.items():
            for t in targets:
                if t != f:
                    out.add((f, t, "open"))
        for f, names in self.name_edges.items():
            for t in self.resolve(names):
                if t != f:
                    out.add((f, t, "import"))
        direct = {(f, t) for f, t, _ in out}
        for unit, roots in self.unit_roots.items():
            for r in roots:
                if r != unit and (unit, r) not in direct:
                    out.add((unit, r, "read"))
        return [list(e) for e in sorted(out)]

    def results(self):
        self.scan_modules()
        file_edges = {}
        for f, targets in self.edges.items():
            file_edges.setdefault(f, set()).update(targets)
        for f, names in self.name_edges.items():
            file_edges.setdefault(f, set()).update(self.resolve(names))
        native_files = {f for files in self.mod_files.values() for f in files
                        if f.endswith(NATIVE_SUFFIXES)}
        out = []
        units = set(self.units) | set(self.roots) | set(self.root_names)
        units.discard(None)
        for unit in sorted(units):
            last = self.unit_last_seq.get(unit, 0)
            roots = {unit}
            roots.update(self.static_roots(unit))
            roots.update(self.roots.get(unit, ()))
            roots.update(self.resolve(self.root_names.get(unit, ())))
            for seq, f, name, why in self.ambient:
                if seq <= last:
                    if f:
                        roots.add(f)
                    if name:
                        files = self.mod_files.get(name, ())
                        if why == PYTEST_OUTSIDE:
                            # conftest.py and __init__.py files that pytest loads while collecting a
                            # directory apply to the test files below them (static_roots), not all.
                            files = [x for x in files
                                     if x.rsplit("/", 1)[-1] not in ("conftest.py", "__init__.py")]
                        roots.update(files)
            self.unit_roots[unit] = roots
            reads = self.closure(roots, file_edges)
            info = self.units.get(unit) or self.unit_entry(unit)
            not_followed = [{"why": k, "detail": d}
                            for k, v in sorted(self.not_followed.get(unit, {}).items())
                            for d in sorted(v)]
            for f in sorted(reads & native_files):
                not_followed.append({"why": "native", "detail": f})
            selected = info["selected"] if self.collection_finished else info["collected"]
            entry = {
                "unit": unit,
                "selected": selected,
                "finished": info["finished"],
                "failed": bool(info["failed"]),
                "deselected": unit in self.deselected,
                "durationMs": int(round(info["duration"] * 1000)),
                "skipped": info.get("skipped", 0),
                "reads": sorted(reads),
                "notFollowed": not_followed,
            }
            if self.by_test:
                entry["tests"] = self.test_results(unit)
            if self.watching:
                self.watch_results(unit, entry)
            out.append(entry)
        return out

    def index(self, path):
        at = self.path_index.get(path)
        if at is None:
            at = len(self.paths)
            self.paths.append(path)
            self.path_index[path] = at
        return at

    def indices(self, paths):
        return sorted(self.index(p) for p in paths)

    def watch_results(self, unit, entry):
        """[ADR 0080] What each test of `unit` touched, and what its loading, its shared setups and
        the runner did for all of them, by index into the file's `paths`."""
        ambient = self.ambient
        order = self.test_order.get(unit, [])
        for test in entry["tests"]:
            key = (unit, test["id"])
            window = self.test_window.get(key)
            if window is None:
                continue
            ran = set(self.test_ran.get(key, ()))
            ran.update(self.resolve(self.test_names.get(key, ())))
            opened = set(self.test_opened.get(key, ()))
            # What ran, or was opened, while no window was open: every test's.
            ran.update(self.session_ran)
            opened.update(self.session_opened)
            last_seq = window[2]
            # A module code outside the repository loaded is read by the test running then and
            # every test after it in the process (RFC 0003 §3.3).
            for seq, f, name, why in ambient:
                if seq > last_seq:
                    continue
                if f:
                    opened.add(f)
                if name:
                    files = self.mod_files.get(name, ())
                    if why == PYTEST_OUTSIDE:
                        # conftest.py and __init__.py files pytest loads while collecting are
                        # what every test below them reads (`everyTest`), not every test's.
                        files = [x for x in files
                                 if x.rsplit("/", 1)[-1] not in ("conftest.py", "__init__.py")]
                    ran.update(files)
            test["ran"] = self.indices(ran)
            test["opened"] = self.indices(opened)
            test["dirs"] = self.indices(self.test_dirs.get(key, ()))
        entry["loaded"] = {"ran": self.indices(self.loaded_ran.get(unit, ())),
                           "opened": self.indices(self.loaded_opened.get(unit, ()))}
        entry["everyTest"] = self.indices(self.static_roots(unit))
        shared = []
        for setup in self.setups:
            served = []
            for at, test_id in enumerate(order):
                window = self.test_window.get((unit, test_id))
                if window is None:
                    continue
                # A setup that starts inside a test's window serves that test; one it was served,
                # by name (requested, or asked for at run time), serves it while pytest kept it.
                inside = window[0] <= setup["start"] <= window[1]
                kept = setup["argname"] in window[3] and setup["start"] <= window[1] \
                    and (setup["end"] is None or setup["end"] >= window[0])
                if inside or kept:
                    served.append(at)
            if served:
                shared.append({"tests": served, "ran": self.indices(setup["ran"]),
                               "opened": self.indices(setup["opened"])})
        entry["shared"] = shared

    def watch_tables(self):
        """[ADR 0080] Each file's direct imports and the files each module opened, by index."""
        imports = {}
        for f, names in self.name_edges.items():
            targets = {t for t in self.resolve(names) if t != f}
            if targets:
                imports[str(self.index(f))] = self.indices(targets)
        opens = {}
        for f, targets in self.edges.items():
            targets = {t for t in targets if t != f}
            if targets:
                opens[str(self.index(f))] = self.indices(targets)
        return imports, opens

    def test_results(self, unit):
        """[ADR 0077] Each test of `unit` this process ran or deselected, in that order."""
        out = []
        tests = self.tests.get(unit, {})
        for test_id in self.test_order.get(unit, []):
            facts = tests[test_id]
            out.append({
                "id": test_id,
                "finished": bool(facts["finished"]),
                "failed": bool(facts["failed"]),
                "deselected": bool(facts["deselected"]),
                "skipped": bool(facts.get("skipped")),
                "durationMs": int(round(facts["duration"] * 1000)),
                "notFollowed": [{"why": k, "detail": d}
                                for k, v in sorted(self.test_not_followed.get((unit, test_id), {}).items())
                                for d in sorted(v)],
            })
        return out


OBSERVER = None
_ORIG_IMPORT = builtins.__import__
_LOCAL = threading.local()


def _wrapped_import(name, globals=None, locals=None, fromlist=(), level=0):
    obs = OBSERVER
    if obs is None or obs.done or getattr(_LOCAL, "busy", False):
        return _ORIG_IMPORT(name, globals, locals, fromlist, level)
    result = _ORIG_IMPORT(name, globals, locals, fromlist, level)
    _guard(obs.on_import, name, globals, fromlist, level, sys._getframe(1))
    return result


def _guard(fn, *args):
    obs = OBSERVER
    if obs is None or getattr(_LOCAL, "busy", False):
        return
    _LOCAL.busy = True
    try:
        with obs.lock:
            fn(*args)
    except Exception as err:  # the adapter never breaks the run it watches
        if len(obs.errors) < MOST_ERRORS:
            obs.errors.append("%s: %r" % (getattr(fn, "__name__", "?"), err))
    finally:
        _LOCAL.busy = False


class _FirstLoadFinder(object):
    """Sees every first load, whatever asked for it; finds nothing itself."""

    def find_spec(self, fullname, path=None, target=None):
        obs = OBSERVER
        if obs is not None and not obs.done and not getattr(_LOCAL, "busy", False):
            _guard(obs.on_first_load, fullname, sys._getframe(1))
        return None

    def invalidate_caches(self):
        pass


def _audit(event, args):
    obs = OBSERVER
    if obs is None or obs.done:
        return
    if event == "open":
        if not getattr(_LOCAL, "busy", False):
            _guard(obs.on_open, args, sys._getframe(1))
    elif event in SPAWN_EVENTS:
        _guard(obs.on_spawn, event, args, sys._getframe(1))
    elif event == "ctypes.dlopen" and args:
        _guard(obs.on_dlopen, args[0])
    elif event in ("os.listdir", "os.scandir") and obs.watching:
        path = args[0] if args else None
        if isinstance(path, bytes):
            path = os.fsdecode(path)
        if path is None or isinstance(path, str):
            _guard(obs.listed, path if path is not None else ".")


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


def _claim(directory):
    """Claims the run's directory for this process when no other pytest did: (claimed, the pid of
    the process that holds it, or None when that can't be read)."""
    path = os.path.join(directory, CLAIM_FILE)
    try:
        fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except OSError:
        try:
            with open(path, encoding="utf-8") as f:
                return False, int(f.read().strip())
        except Exception:
            return False, None
    try:
        os.write(fd, str(os.getpid()).encode("ascii"))
    finally:
        os.close(fd)
    return True, os.getpid()


WATCH_TOOL = None
CO_OPTIMIZED = getattr(inspect, "CO_OPTIMIZED", 1)


def _restart_watch():
    try:
        sys.monitoring.restart_events()
    except Exception:
        pass


def _on_py_start(code, offset):
    """[ADR 0080] A function started: its file, once a test or a shared setup, is touched. A
    module's top-level code or a class body (not optimized code) is no function."""
    obs = OBSERVER
    try:
        if obs is None or obs.done or not (code.co_flags & CO_OPTIMIZED):
            return sys.monitoring.DISABLE
        f = obs.code_file(code.co_filename)
        if f is not None and not getattr(_LOCAL, "busy", False):
            obs.touched("ran", (f,))
    except Exception as err:
        if obs is not None and len(obs.errors) < MOST_ERRORS:
            obs.errors.append("watch: %r" % (err,))
    return sys.monitoring.DISABLE


_ORIG_USE_TOOL_ID = None


def _other_tool():
    """[ADR 0080] The name of a tool other than hushrun's that holds a sys.monitoring tool id (a
    debugger, coverage.py's sysmon core, a profiler, a repository's own), or None."""
    monitoring = getattr(sys, "monitoring", None)
    if monitoring is None:
        return None
    for tool in range(6):
        if tool == WATCH_TOOL:
            continue
        try:
            name = monitoring.get_tool(tool)
        except Exception:
            name = None
        if name is not None:
            return repr(name)
    return None


def _use_tool_id(tool_id, name):
    """[ADR 0080] sys.monitoring.use_tool_id, which another tool calls: hushrun gives way first, and
    watches no more in this process, so that the tool gets its id and its events stay its own."""
    obs = OBSERVER
    if WATCH_TOOL is not None and obs is not None:
        _call(lambda o: o.stop_watching("%r started using sys.monitoring during the run" % (name,)))
    return _ORIG_USE_TOOL_ID(tool_id, name)


def _start_watching(observer):
    """[ADR 0080] Watches each test's functions with sys.monitoring, Python 3.12 or later, while no
    other tool uses it: why not, or None."""
    global WATCH_TOOL, _ORIG_USE_TOOL_ID
    monitoring = getattr(sys, "monitoring", None)
    if monitoring is None:
        return "Python %d.%d has no sys.monitoring, which watching a test needs (3.12 or later)" \
            % sys.version_info[:2]
    other = _other_tool()
    if other is not None:
        # Restarting events at each test re-arms every tool's: coverage.py's sysmon core, a
        # debugger.
        return "%s uses sys.monitoring, which watching a test would disturb" % other
    for tool in (4, 3):
        if monitoring.get_tool(tool) is None:
            monitoring.use_tool_id(tool, "hushrun")
            WATCH_TOOL = tool
            break
    else:
        return "no sys.monitoring tool id was free"
    monitoring.register_callback(WATCH_TOOL, monitoring.events.PY_START, _on_py_start)
    monitoring.set_events(WATCH_TOOL, monitoring.events.PY_START)
    if _ORIG_USE_TOOL_ID is None:
        _ORIG_USE_TOOL_ID = monitoring.use_tool_id
        monitoring.use_tool_id = _use_tool_id
    observer.watching = True
    return None


def _stop_watching():
    """Releases hushrun's tool id: events off, its callback gone, the id free for another tool."""
    global WATCH_TOOL
    if WATCH_TOOL is None:
        return
    try:
        sys.monitoring.set_events(WATCH_TOOL, 0)
        sys.monitoring.register_callback(WATCH_TOOL, sys.monitoring.events.PY_START, None)
        sys.monitoring.free_tool_id(WATCH_TOOL)
    except Exception:
        pass
    WATCH_TOOL = None
    if _ORIG_USE_TOOL_ID is not None:
        try:
            sys.monitoring.use_tool_id = _ORIG_USE_TOOL_ID
        except Exception:
            pass


def _shared_finishing(obs, step):
    obs.shared_finishing = max(0, obs.shared_finishing + step)


def _wrap_fixture_finish():
    """[ADR 0077] A fixture of a wider scope than one test ends where pytest says, which for a
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
        finally:
            if wide:
                _call(_shared_finishing, -1)

    finish._hushrun = True
    FixtureDef.finish = finish


def _install():
    global OBSERVER, PYTEST_DIRS
    directory = os.environ.get(ENV_DIR)
    if OBSERVER is not None or not directory or not hasattr(sys, "addaudithook"):
        return
    with open(os.path.join(directory, "hushrun.json"), encoding="utf-8") as f:
        run = json.load(f)
    if run.get("schema") != RUN_SCHEMA or not run.get("root"):
        return
    import _pytest  # noqa: F401  (loaded already: pytest is importing this plugin)
    PYTEST_DIRS = _pytest_dirs()
    observer = Observer(run["root"], directory)
    reuse = run.get("reuse") or []
    if isinstance(reuse, list) and all(isinstance(u, str) for u in reuse):
        observer.reuse = frozenset(reuse)
    python_files = run.get("pythonFiles")
    if isinstance(python_files, list) and all(isinstance(p, str) for p in python_files):
        observer.python_files = list(python_files)
    # [ADR 0077] Tests one by one, and the tests the plan lets this run deselect.
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
    OBSERVER = observer
    if observer.by_test:
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
    OBSERVER = None

import pytest  # noqa: E402


def _node_path(node):
    p = getattr(node, "path", None)
    if p is None:
        p = getattr(node, "fspath", None)
    return str(p) if p is not None else None


def _unit_of(node):
    p = _node_path(node)
    if not p or OBSERVER is None:
        return None
    return OBSERVER.rel(p)


def _test_id(nodeid):
    """[ADR 0077] A test's id: its node id after its file's path and `::`; None when it has none."""
    if not isinstance(nodeid, str) or "::" not in nodeid:
        return None
    return nodeid.split("::", 1)[1] or None


def _call(fn, *args):
    """Runs one hook's work with the observer's lock, never raising into pytest."""
    obs = OBSERVER
    if obs is None or obs.done:
        return
    try:
        with obs.lock:
            fn(obs, *args)
    except Exception as err:
        if len(obs.errors) < MOST_ERRORS:
            obs.errors.append("%s: %r" % (fn.__name__, err))


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
    """[ADR 0080] The fixtures a test was served, as its request has them once it ran: those it
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
            # [ADR 0077] A test the plan lists one by one: its file runs, without it.
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
    if OBSERVER is not None and not OBSERVER.done:
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
        _call(_protocol_end, unit, test_id, getattr(item, "nodeid", None), cut)


def _shared_setup(obs, fixturedef, step):
    """[ADR 0077] A fixture of a wider scope than one test sets up: what happens meanwhile serves
    every test that requests it, and is the test file's. [ADR 0080] What it runs and opens is a
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
    """[ADR 0080] pytest no longer keeps a shared fixture's value: tests after this don't read it."""
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
    """[ADR 0077] A teardown that may end fixtures of a wider scope: the last test of its file or
    class, or of the session. What happens meanwhile is the test file's."""
    obs.shared_teardown = nextitem is None or getattr(nextitem, "parent", None) is not \
        getattr(item, "parent", None)


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
    if OBSERVER is not None and not OBSERVER.done:
        _call(lambda obs: obs.active and _served(obs, obs.active, obs.active_test,
                                                 _request_fixtures(item)))
    yield


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session, exitstatus):
    obs = OBSERVER
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
    except Exception:
        obs.done = True
