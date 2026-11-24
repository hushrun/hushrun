"""hushrun's observer of a Python test run, which its pytest and unittest adapters share (ADR
0068, 0083, 0110; C-32, C-42, C-66).

It keeps the module graph (every `import` through `builtins.__import__`, every first load through a
finder at the front of `sys.meta_path`), what was opened through the audit hook (PEP 578), what it
couldn't follow, and, on Python 3.12 or later, the files of the functions that started in each
window through `sys.monitoring` (PEP 669). A runner's own module drives it: it says when a unit
(a test file) and a test start and stop, which shared setups serve which tests, and writes the
facts. Standard library only; Python 3.8 or later. It decides nothing.
"""

import ast
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

SCHEMA = "hushrun.unit-facts/v0"
SCHEMA_V1 = "hushrun.unit-facts/v1"
RUN_SCHEMA = "hushrun.units/v0"
ENV_DIR = "HUSHRUN_UNITS"

THIS_FILE = os.path.abspath(__file__)
if THIS_FILE.endswith((".pyc", ".pyo")):
    THIS_FILE = THIS_FILE[:-1]
# The adapter's directory, which hushrun writes and holds only the adapter's own files: none of them
# is a repository's, nor code a test runs.
OWN_DIRS = frozenset({os.path.dirname(THIS_FILE), os.path.dirname(os.path.realpath(THIS_FILE))})
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
# The directories the runner's own code is in, as the runner has them loaded, which its module sets
# (pytest's, unittest's): never a name in a path, which a repository or a virtual environment can
# hold too (a `py/` directory).
RUNNER_DIRS = ()
# Whether a file the runner's own code opens is the unit's alone and no test's (pytest reading a
# test file or a conftest.py as it collects): else it is read as any library's open is.
RUNNER_OPENS = True
NATIVE_SUFFIXES = (".so", ".pyd", ".dylib", ".dll")
RUNNER_OUTSIDE = "loaded by pytest outside any test file"
# [ADR 0110] The code files, as their frames name them, that start the runner from the repository:
# Django's `manage.py`, under every frame of the run, whose frames own no open and no import.
RUNNER_FILES = frozenset()
# [ADR 0110] Called before a first load is attributed, with the module's name, the finder's `path`
# and the frame that asked: how a runner whose units it learns only as it imports them (unittest's
# loader) starts one.
FIRST_LOAD_HOOK = None
MOST_ERRORS = 20
CLAIM_FILE = "claim"
NO_TESTS_COLLECTED = 5
# [C-32] This start of the runner, which a pytest-xdist worker takes from its controller instead.
START = uuid.uuid4().hex


def _own(fn):
    return os.path.dirname(fn) in OWN_DIRS


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
        self.paths = []               # [ADR 0083] the file's table of paths
        self.path_index = {}
        self.by_test = False          # hushrun.json's unit is test: tests are reported one by one
        self.reuse_tests = {}         # unit -> frozenset(ids) hushrun.json lets this run deselect
        self.tests = {}               # unit -> {id: facts of the test being gathered}
        self.test_order = {}          # unit -> [ids, in the order they started or were deselected]
        self.active_test = None       # the id of the test whose protocol is running
        self.shared_depth = 0         # >0 while a fixture of a wider scope than a test sets up
        self.shared_teardown = False  # a teardown that may end fixtures of a wider scope
        self.test_not_followed = {}   # (unit, id) -> {reason: set(details)}
        self.watching = False         # [ADR 0083] each test's code, opens and listings are watched
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
        self._reexports = {}          # [C-40] repo file -> files it re-exports from
        self.torn_ids = set()         # node ids of tests whose teardown report was logged
        self.started_file = None      # [C-32] this start's `.started` file, removed at its end
        self.finished_ids = {}        # node id -> (unit, test id) of each test that finished
        self.served = {}              # node id -> (fixture names, fixture def ids) it was served
        self.failed_wide = []         # (name, def id) of each wider fixture whose end raised
        self.session_ran = set()      # [ADR 0083] files a function of which started outside every
        self.session_opened = set()   # window, and files opened there: every test reads them
        self.shared_finishing = 0     # >0 while a fixture of a wider scope than a test is ended
        # The files every unit below a directory reads: pytest's conftest.py and package
        # __init__.py; unittest's __init__.py alone.
        self.static_names = ("conftest.py", "__init__.py")
        # [C-37] Why a test a wider fixture served keeps no pass, when that fixture's end failed.
        self.wide_not_kept = ("a fixture of a wider scope it was served (%s) failed as it ended, and "
                              "pytest reports that on whichever test ran last, so it keeps no pass")

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
            if not _own(fn) and not fn.startswith("<") and fn not in RUNNER_FILES:
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
        # [ADR 0083] A module imported while a test or a shared setup runs is read by it.
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
        transient = req_file.replace(os.sep, "/").endswith(TRANSIENT_IMPORTERS)
        if req_file.startswith(RUNNER_DIRS) and not transient:
            # The runner collecting a test file, a conftest or a plugin named in pytest_plugins;
            # unittest's loader importing a test module.
            if self.active:
                self.add_root(self.active, name=fullname)
            else:
                self.make_ambient(name=fullname, why=RUNNER_OUTSIDE)
            return
        frames = self.repo_frames(req)
        if frames:
            self.name_edges.setdefault(frames[0], set()).add(fullname)
        if transient:
            if self.active:
                self.add_root(self.active, name=fullname)
                # [ADR 0083] Loaded for the test running now, through a helper outside the
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
        if owner in ("import", "runner"):
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
            # [ADR 0083] Opened while no window is open: every test of the session reads it.
            self.touched("opened", {r})
            if owner != "repo":
                self.make_ambient(f=r, why="opened outside any test file by code outside the repository")

    def owner(self, frame):
        """Who an open belongs to: the first frame, from the open outwards, that is the import
        system's, the runner's or the repository's ("other" when only code outside it is there)."""
        while frame is not None:
            fn = frame.f_code.co_filename
            if fn.startswith("<frozen importlib"):
                return "import", frame
            if not _own(fn) and not fn.startswith("<") and fn not in RUNNER_FILES:
                if self.rel(fn):
                    return "repo", frame
                if RUNNER_OPENS and fn.startswith(RUNNER_DIRS):
                    return "runner", frame
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

    # --- watching a test [ADR 0083] -------------------------------------------------------

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
        """[ADR 0083] A window starts or ends: functions are watched anew, so that each one is seen
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
        """[ADR 0083] Stops watching for the rest of the process and releases the tool id: this
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
            for name in self.static_names:
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

    def unserve_failed(self):
        """[C-32] A wider fixture whose end raised failed everything it served: no test it served
        finished (a session fixture's reaches tests in other files), so none records a pass, and
        their test files didn't run whole. A test is matched by the fixture it was served, by its
        definition where pytest says, else by its name."""
        for name, def_id in self.failed_wide:
            for nodeid, (names, def_ids) in self.served.items():
                hit = def_id in def_ids if def_ids else name in names
                if not hit or nodeid not in self.finished_ids:
                    continue
                unit, test_id = self.finished_ids.pop(nodeid)
                entry = self.unit_entry(unit)
                entry["finished"] = max(0, entry["finished"] - 1)
                if test_id is not None and test_id in self.tests.get(unit, {}):
                    self.tests[unit][test_id]["finished"] = False
                    # [C-37] Why it keeps no result, for `hushrun why`: not that it didn't finish.
                    self.tests[unit][test_id]["notKept"] = self.wide_not_kept % name

    def results(self):
        self.scan_modules()
        self.unserve_failed()
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
                        if why == RUNNER_OUTSIDE:
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
        """[ADR 0083] What each test of `unit` touched, and what its loading, its shared setups and
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
                    if why == RUNNER_OUTSIDE:
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
        """[ADR 0083] Each file's direct imports and the files each module opened, by index."""
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

    def reexport_table(self):
        """[C-40] The files each repository module re-exports from, by index: a package's
        `__init__.py` that binds names at its top level with `from X import …`, a module with
        `from X import *`, and a module whose `__all__` names what it bound with `from X import`,
        each read from the module's source (once a module), since a value has no origin."""
        modules = {}
        for name, files in self.mod_files.items():
            for f in files:
                if f.endswith(".py"):
                    modules.setdefault(f, set()).add(name)
        out = {}
        for f, names in modules.items():
            targets = {t for t in self.reexports_of(f, min(names)) if t != f}
            if targets:
                out[str(self.index(f))] = self.indices(targets)
        return out

    def reexports_of(self, f, modname):
        """[C-40] What the module at `f`, imported as `modname`, re-exports from, as files."""
        cached = self._reexports.get(f)
        if cached is not None:
            return cached
        found = set()
        self._reexports[f] = found
        try:
            with open(os.path.join(self.root, *f.split("/")), "rb") as source:
                tree = ast.parse(source.read(), filename=f)
        except Exception:
            return found
        package_init = f.rsplit("/", 1)[-1] == "__init__.py"
        package = modname if package_init else modname.rpartition(".")[0]

        def files_of(name):
            return set(self.mod_files.get(name, ())) if name else set()

        def top_level(body):
            for node in body:
                yield node
                if isinstance(node, (ast.If, ast.Try)):
                    for part in (node.body, node.orelse, getattr(node, "finalbody", []),
                                 *[h.body for h in getattr(node, "handlers", [])]):
                        yield from top_level(part)

        bound = {}
        public = []
        for node in top_level(tree.body):
            if isinstance(node, ast.ImportFrom):
                try:
                    source_name = importlib.util.resolve_name(
                        "." * node.level + (node.module or ""), package) \
                        if node.level else node.module
                except Exception:
                    continue
                for alias in node.names:
                    if alias.name == "*":
                        found |= files_of(source_name)
                        continue
                    origin = files_of(source_name) | files_of(source_name + "." + alias.name)
                    bound[alias.asname or alias.name] = origin
                    if package_init:
                        found |= origin
            elif isinstance(node, (ast.Assign, ast.AugAssign, ast.AnnAssign)):
                targets = node.targets if isinstance(node, ast.Assign) else [node.target]
                if any(isinstance(t, ast.Name) and t.id == "__all__" for t in targets) \
                        and isinstance(node.value, (ast.List, ast.Tuple)):
                    public.extend(e.value for e in node.value.elts
                                  if isinstance(e, ast.Constant) and isinstance(e.value, str))
        for name in public:
            found |= bound.get(name, set())
        return found

    def test_results(self, unit):
        """[ADR 0080] Each test of `unit` this process ran or deselected, in that order."""
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
                **({"notKept": facts["notKept"]} if facts.get("notKept") else {}),
                "notFollowed": [{"why": k, "detail": d}
                                for k, v in sorted(self.test_not_followed.get((unit, test_id), {}).items())
                                for d in sorted(v)],
            })
        return out


OBSERVER = None
_ORIG_IMPORT = builtins.__import__
_LOCAL = threading.local()


def _caller():
    """The frame that called the hook calling this, or None where C called it with no Python frame
    to call from: an import or an open as the interpreter starts, before any of its code runs."""
    try:
        return sys._getframe(2)
    except ValueError:
        return None


def _wrapped_import(name, globals=None, locals=None, fromlist=(), level=0):
    obs = OBSERVER
    if obs is None or obs.done or getattr(_LOCAL, "busy", False):
        return _ORIG_IMPORT(name, globals, locals, fromlist, level)
    result = _ORIG_IMPORT(name, globals, locals, fromlist, level)
    _guard(obs.on_import, name, globals, fromlist, level, _caller())
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
            if FIRST_LOAD_HOOK is not None:
                _guard(FIRST_LOAD_HOOK, fullname, path, _caller())
            _guard(obs.on_first_load, fullname, _caller())
        return None

    def invalidate_caches(self):
        pass


def _audit(event, args):
    obs = OBSERVER
    if obs is None or obs.done:
        return
    if event == "open":
        if not getattr(_LOCAL, "busy", False):
            _guard(obs.on_open, args, _caller())
    elif event in SPAWN_EVENTS:
        _guard(obs.on_spawn, event, args, _caller())
    elif event == "ctypes.dlopen" and args:
        _guard(obs.on_dlopen, args[0])
    elif event == "sqlite3.connect" and args and not getattr(_LOCAL, "busy", False):
        # [C-32] sqlite3 opens its file in C, where no `open` event is raised: its path is read.
        # `:memory:` and an empty name are no file; a `file:` URI names its path before `?`.
        path = _sqlite_path(args[0])
        if path is not None:
            _guard(obs.on_open, (path, "r"), _caller())
    elif event in ("os.listdir", "os.scandir") and obs.watching:
        path = args[0] if args else None
        if isinstance(path, bytes):
            path = os.fsdecode(path)
        if path is None or isinstance(path, str):
            _guard(obs.listed, path if path is not None else ".")


def _sqlite_path(database):
    """The file a `sqlite3.connect` names, or None for an in-memory database."""
    try:
        if isinstance(database, bytes):
            database = os.fsdecode(database)
        elif not isinstance(database, str):
            database = os.fspath(database)
            if isinstance(database, bytes):
                database = os.fsdecode(database)
    except TypeError:
        return None
    if database.startswith("file:"):
        # [ADR 0110] `mode=memory` names a shared in-memory database (Django's test database,
        # pytest-django's), never a file, whatever its name.
        query = database.split("#", 1)[0].partition("?")[2]
        if "mode=memory" in query.split("&"):
            return None
        database = database[len("file:"):].split("?", 1)[0].split("#", 1)[0]
        if database.startswith("//"):
            database = database[2:]
            database = database[database.find("/"):] if "/" in database else ""
    if database in ("", ":memory:"):
        return None
    return database


def _claim(directory):
    """Claims the run's directory for this process when no other process of the runner did: (claimed, the pid of
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
    """[ADR 0083] A function started: its file, once a test or a shared setup, is touched. A
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
    """[ADR 0083] The name of a tool other than hushrun's that holds a sys.monitoring tool id (a
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
    """[ADR 0083] sys.monitoring.use_tool_id, which another tool calls: hushrun gives way first, and
    watches no more in this process, so that the tool gets its id and its events stay its own."""
    obs = OBSERVER
    if WATCH_TOOL is not None and obs is not None:
        _call(lambda o: o.stop_watching("%r started using sys.monitoring during the run" % (name,)))
    return _ORIG_USE_TOOL_ID(tool_id, name)


def _start_watching(observer):
    """[ADR 0083] Watches each test's functions with sys.monitoring, Python 3.12 or later, while no
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


def _call(fn, *args):
    """Runs one hook's work with the observer's lock, never raising into the runner."""
    obs = OBSERVER
    if obs is None or obs.done:
        return
    try:
        with obs.lock:
            fn(obs, *args)
    except Exception as err:
        if len(obs.errors) < MOST_ERRORS:
            obs.errors.append("%s: %r" % (fn.__name__, err))
