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

It decides nothing: hushrun decides each test file's state from these facts. It selects nothing
either, in this version. It never changes pytest's outcome: every hook is guarded, and any error of
its own is reported, which makes hushrun record nothing for the test files this process saw.

Output: one file per process in $HUSHRUN_UNITS, written whole when the session ends (a temporary
file, then a rename). With pytest-xdist each worker writes its own, and the controller one with no
test files. Standard library only; Python 3.8 or later.
"""

import builtins
import importlib
import importlib.util
import json
import os
import sys
import threading
import time

__all__ = [
    "pytest_collectstart",
    "pytest_itemcollected",
    "pytest_collectreport",
    "pytest_collection_finish",
    "pytest_runtest_protocol",
    "pytest_runtest_logreport",
    "pytest_sessionfinish",
]

SCHEMA = "hushrun.unit-facts/v0"
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
        self._rel_cache = {}
        self._import_seen = set()
        self._scanned = 0

    # --- paths -------------------------------------------------------------------------

    def rel(self, path):
        """Repository-relative path of a file, or None when it is not the repository's own."""
        cached = self._rel_cache.get(path)
        if cached is not None or path in self._rel_cache:
            return cached
        result = None
        try:
            real = os.path.realpath(path if os.path.isabs(path) else os.path.join(os.getcwd(), path))
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
        elif owner != "repo":
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

    def on_spawn(self, event, args):
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
        target = self.not_followed.setdefault(self.active, {}) if self.active \
            else self.session_not_followed
        target.setdefault(reason, set()).add(detail)

    def on_dlopen(self, name):
        if isinstance(name, bytes):
            name = os.fsdecode(name)
        r = self.rel(name) if isinstance(name, str) and os.path.sep in name else None
        if r:
            target = self.not_followed.setdefault(self.active, {}) if self.active \
                else self.session_not_followed
            target.setdefault("native", set()).add(r)

    # --- phases ------------------------------------------------------------------------

    def start(self, unit):
        self.seq += 1
        self.active = unit
        self.unit_last_seq[unit] = self.seq
        self.scan_modules()

    def stop(self):
        if self.active:
            self.unit_last_seq[self.active] = self.seq
        self.active = None

    def unit_entry(self, unit):
        return self.units.setdefault(unit, {"collected": 0, "selected": 0, "finished": 0,
                                             "failed": False, "duration": 0.0})

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
            reads = self.closure(roots, file_edges)
            info = self.units.get(unit) or self.unit_entry(unit)
            not_followed = [{"why": k, "detail": d}
                            for k, v in sorted(self.not_followed.get(unit, {}).items())
                            for d in sorted(v)]
            for f in sorted(reads & native_files):
                not_followed.append({"why": "native", "detail": f})
            selected = info["selected"] if self.collection_finished else info["collected"]
            out.append({
                "unit": unit,
                "selected": selected,
                "finished": info["finished"],
                "failed": bool(info["failed"]),
                "durationMs": int(round(info["duration"] * 1000)),
                "reads": sorted(reads),
                "notFollowed": not_followed,
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
        _guard(obs.on_spawn, event, args)
    elif event == "ctypes.dlopen" and args:
        _guard(obs.on_dlopen, args[0])


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
    # Repository modules loaded before the adapter (a .pth file, sitecustomize) are ambient.
    for name, module in list(sys.modules.items()):
        if module is not None and observer.module_file(module):
            observer.note_module(name)
            observer.make_ambient(name=name, why="loaded before the adapter")
    OBSERVER = observer
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
    entry = obs.unit_entry(unit)
    entry["duration"] += getattr(report, "duration", 0.0) or 0.0
    if report.failed:
        # A failure, or an error in setup or teardown; pytest reports a strict xpass failed too.
        entry["failed"] = True


def _protocol_start(obs, unit):
    obs.unit_entry(unit)
    obs.start(unit)


def _protocol_end(obs, unit):
    obs.unit_entry(unit)["finished"] += 1
    obs.stop()


def pytest_collectstart(collector):
    _call(_collectstart, collector)


def pytest_itemcollected(item):
    _call(_itemcollected, item)


def pytest_collectreport(report):
    _call(_collectreport, report)


def pytest_collection_finish(session):
    _call(_collection_finish, session)


def pytest_runtest_logreport(report):
    _call(_logreport, report)


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_protocol(item, nextitem):
    unit = None
    if OBSERVER is not None and not OBSERVER.done:
        try:
            unit = _unit_of(item)
        except Exception:
            unit = None
    if unit:
        _call(_protocol_start, unit)
    yield
    if unit:
        _call(_protocol_end, unit)


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session, exitstatus):
    obs = OBSERVER
    if obs is None or obs.done:
        return
    try:
        with obs.lock:
            obs.stop()
            config = session.config
            worker = getattr(config, "workerinput", None)
            is_controller = bool(getattr(config.option, "numprocesses", None)) and not worker
            try:
                units = [] if is_controller else obs.results()
            except Exception as err:
                obs.errors.append("results: %r" % (err,))
                units = []
            obs.done = True
            try:
                python_files = list(config.getini("python_files"))
            except Exception:
                python_files = None
            facts = {
                "schema": SCHEMA,
                "adapter": "pytest",
                "pid": os.getpid(),
                "worker": (worker or {}).get("workerid") if worker else None,
                "controller": is_controller,
                "failuresByUnit": True,
                "settings": {"pythonFiles": python_files, "python": sys.version.split()[0],
                             "pytest": pytest.__version__},
                "notFollowed": [] if is_controller else
                [{"why": k, "detail": d} for k, v in sorted(obs.session_not_followed.items())
                 for d in sorted(v)],
                "units": units,
                "errors": obs.errors,
            }
            name = "pytest-%d%s.json" % (os.getpid(), ("-" + facts["worker"]) if facts["worker"] else "")
            tmp = os.path.join(obs.directory, "." + name + ".tmp")
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(facts, f, sort_keys=True)
            os.replace(tmp, os.path.join(obs.directory, name))
    except Exception:
        obs.done = True
