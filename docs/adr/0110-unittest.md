# 0110. unittest and Django's `manage.py test`, and why Python's C extensions get no build blocks

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-66 to C-69; relies on C-30, C-31, C-32, C-33, C-34, C-39, C-40, C-42, ADR 0068,
  0069, 0080, 0081, 0083
- **Build blocks:** none (B-140 to B-149 reserved, unused); see "Compiled code" below

## Context

pytest's adapter (ADR 0068, 0083) reuses Python's tests by test file and by test, but only when
pytest runs them. The standard library's runner, `python -m unittest`, and Django's
`manage.py test`, which drives it, got pytest's adapter as any COMMAND nobody else takes: it loads
nothing outside pytest, so such a check was reused whole or not at all. Measured on a project of
two test modules under `tests: mode: reuse`: a change to a module only one of them imports ran
both, every time. `hushrun init` already proposes `python -m unittest discover -s tests` as a
check (ADR 0050), and Django projects run `manage.py test` in CI more than any other command.

unittest has no plugins and no hook a runner calls: its loader imports test modules, its suites
call fixtures and tests, its result is told what happened. Three ways in were weighed:

- **Rewriting COMMAND** (`python -m hushrun_unittest …`, `program::Program` on `PATH`): COMMAND
  is a unit's key (C-31) and the person's; a Python found on `PATH` is only one way in
  (`.venv/bin/python`, `uv run`).
- **`usercustomize`**: off in a virtual environment (`ENABLE_USER_SITE` is false there).
- **`sitecustomize` first on `PYTHONPATH`**: Python imports it as it starts, from the first
  directory on `sys.path` that has one, before any of COMMAND's code; it hides the one a
  distribution or a virtual environment installs. This container's two Pythons both ship one
  (Ubuntu's apport hook in 3.12; this image's own zlib finder in 3.13), so hiding it can't mean
  losing it.

## Decision

1. **The adapter is observed, its way in a `sitecustomize`, and COMMAND is kept byte for byte**
   (C-66). hushrun writes `sitecustomize.py`, the adapter and the observer to
   `<cache dir>/adapters/unittest-<hash>/` and puts that directory first on `PYTHONPATH`. The
   `sitecustomize` runs the one it hides first (the next on `sys.path`, as `sitecustomize`, its
   error said as `site.py` says one), then starts the adapter. A Python started with `-I`, `-E` or
   `-S` doesn't import it, and the run says so once; so does COMMAND's own `env PYTHONPATH=…`,
   which replaces hushrun's.
2. **One observer for both Python adapters.** The module graph, the audit hook, `sys.monitoring`
   and the facts' tables move out of pytest's plugin into `hushrun_observe.py`, which pytest's
   adapter installs too; the plugin keeps pytest's hooks. What a runner says differs (its own
   directories, whether its opens are reads, the files every unit below a directory reads, why a
   wider fixture's end keeps no pass), and those are the observer's settings. pytest's tests pass
   unchanged on it.
3. **Which process.** A process whose own command line (`sys.orig_argv`, 3.10 or later) runs the
   tests (`-m unittest`, a `manage.py` or `django-admin` with `test`, `-m django test`, `coverage
   run` of either), whose environment doesn't name this run's directory in
   `HUSHRUN_UNITTEST_SESSION`. It sets that in its own environment: a Python a test starts is the
   test's doing, as pytest's `HUSHRUN_PYTEST_SESSION` makes it (ADR 0083, Amendment 3). Asking
   the command line rather than taking the first Python keeps a launcher written in Python
   (poetry, pipenv) from taking the session from the Python it starts.
4. **The unit is a test module's file**, by the pattern discovery uses, read from COMMAND
   (`discover -p`, Django's `--pattern`, `test*.py`), with unittest's module-name rule. A
   process reports every pattern its `discover` calls used; another than hushrun's follows no
   unit. A test's id is `test.id()` after its module's name: `CartTest.test_total`.
5. **unittest's own points, wrapped where they are, never copied.** The observer's finder sees the
   loader's first load of a test module, which starts its unit, as pytest's collection of a file
   does. `TestSuite.run`, when no run has entered its result, is the top-level run: it claims the
   run's directory, removes what may be removed, counts what each unit holds, and wraps the
   result's own methods on the result itself, whatever its class, restoring them as it ends.
   `startTest` and `stopTest` bound a test's window; `TestCase.run` returning says it finished and
   ran in this process. `_handleModuleFixture`, `_handleClassSetUp`, `_tearDownPreviousClass` and
   `_handleModuleTearDown` bound `setUpModule` and `setUpClass` as shared setups and their
   teardowns as the unit's. These four are private to unittest (there since 3.4, and read here in
   3.12's and 3.13's sources); each wrapper calls the original whatever happens, so a later
   unittest that drops one loses the shared setups' precision, not the tests.
6. **What runs between the fixtures and a test is its unit's** (C-68): Django's `_pre_setup` loads
   a `TransactionTestCase`'s fixtures there, and `_post_teardown` runs after `stopTest`. Every test
   of the unit reads it.
7. **Selection** (C-67): the claiming process removes tests from the suite before it runs, from
   every `TestSuite` in it, only when every `discover` used hushrun's pattern; "no tests ran" (5)
   becomes 0 only when it removed every test that was left and nothing failed.
8. **Outcomes** (C-69): fixtures' errors, which unittest reports on no test, fail their unit, and a
   teardown's keeps no pass for any test the fixture served (ADR 0083, Amendment 2's rule for
   pytest's wider fixtures). A module that didn't import is a fail of its unit, one that raised
   `SkipTest` a skip. A failure no repository module owns (a doctest), an error up through the
   suite, or a suite of a class unittest doesn't know keeps no pass of a run that failed.
9. **Django.** `manage.py test` is DiscoverRunner over unittest, and works through all of this.
   Its script is under every frame of the run: without a rule, every file opened anywhere became an
   edge of `manage.py`, which every test reads, and every test module read everything (measured:
   the blog's module read the shop's fixture, template and pricing). `manage.py`'s frames own no
   open and no import. Its in-memory test database, `file:memorydb_default?mode=memory`, was a
   read of a file named `memorydb_default`: a `mode=memory` URI is no file, for pytest-django too.
   Django's `--parallel` runs the tests in worker processes, which the suite override hides; the
   run says so and nothing is observed.

## Verified

- On Python 3.12.3 (with Ubuntu's `sitecustomize`) and 3.13.18 (with this image's), unittest and
  Django 6.1.3: a change to a module two imports away that only one test calls runs that test and
  one audited; a change to a Django app's own module runs that app's test module and one audited
  of the two others; a hidden `sitecustomize` still runs; a Python a test starts writes nothing.
  `crates/hushrun/tests/units_unittest.rs` holds these.
- pytest's tests (`units`, `test_as_unit`, `reads_test`, `reuse`, `why`) pass on the shared
  observer.
- **The cost**, measured on Python 3.13.18 in a four-core Linux container: 300 small tests in 20
  modules, each calling into two of ten modules, a `setUpClass` in a third of them; 15 rounds,
  CPU time of the unittest process, medians: without the adapter 0.080 s, with it and
  `reads: file` 0.109 s, with `reads: test` 0.181 s. Watching costs about 0.24 ms a test over
  `reads: file`, near pytest's 0.28 ms (ADR 0083, item 8, another machine).

## Compiled code

Pure Python compiles nothing hushrun could reuse: bytecode is the interpreter's own cache. C
extensions built as a package installs (setuptools, meson-python) do compile, and the question was
whether they can go through the C and C++ launcher (ADR 0097, `hushrun-cc`) soundly. They can't,
cleanly, and no build block kind is added:

- **No inert way in.** setuptools takes its compiler only from `CC`, `CXX` and `LDSHARED`, which
  B-50 refuses to set for any build: CMake, Meson and autotools cache what `CC` names, and the Rust
  `cc` crate reruns every build script when it changes. meson-python builds with Meson, which has
  no launcher variable (ADR 0097 left it to a later phase). A Python-level hook (a
  `sitecustomize` wrapping `distutils`' `spawn`) is lost where it matters most: pip's isolated
  build replaces `PYTHONPATH` with its own directory and its own `sitecustomize`
  (`pip/_internal/build_env.py`).
- **Nothing an isolated build compiles would be stored.** pip builds against build dependencies
  installed in a fresh `/tmp/pip-build-env-*` folder each time (numpy's and pybind11's headers):
  reads outside every root, base and system folder, which ADR 0097 stores nothing for.
- **What remains reuses already.** pip's wheel cache keeps what an sdist built; an editable or
  `--no-build-isolation` build reruns setuptools' own up-to-date check.

The cloud needs nothing for this ADR: it adds no block kind and no result kind.

## Consequences

- `python -m unittest` and `manage.py test` checks are reused by test module and, with `unit:
  test`, by test, as pytest's are.
- A change to a file of the rest set (a Django template, a fixture, any file that isn't `.py`) runs
  every test module, as it does for pytest.
- Python 3.8 and 3.9, which have no `sys.orig_argv`, run every test module.
- Django's `--parallel`, and a custom runner that doesn't run unittest's suites, run everything.

## Alternatives considered

- **pytest running the unittest tests** (pytest collects `TestCase`s): changes COMMAND, and the
  runner's behaviour (pytest-django's database setup is not Django's).
- **Copying `TestSuite.run`** to hook the call of each test: its body would have to track every
  CPython's; wrapping the four fixture methods and the result needs none of it.
- **A window from `TestCase.__call__`**: Django overrides it to wrap the base one in
  `_pre_setup` and `_post_teardown`, so a base wrapper sits inside them; `startTest` and
  `stopTest`, plus the rule in item 6, cover both.
- **Following Django's parallel workers**: with `fork` they inherit the observer, with `spawn`
  and `forkserver` (3.14's default on Linux) they don't, and their results reach the parent
  replayed; half-followed is worse than said.
