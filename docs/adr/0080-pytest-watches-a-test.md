# 0080. pytest watches a test

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-42; relies on C-32, C-39, C-40, ADR 0068, 0077, 0078, RFC 0004

## Context

`tests.reads: test` (ADR 0078) needs an adapter to report what each test touched. For pytest that is
the code each test ran, the files it opened and the directories it listed, and what its fixtures did
for it. Python 3.12's `sys.monitoring` (PEP 669) gives a cheap event when a function starts; Python's
audit hook (PEP 578) already gives the plugin each open (ADR 0068).

## Decision

1. **Functions called** (`ran`). With `unit: test` and `reads: test`, on Python 3.12 or later, the
   plugin registers for `PY_START` under a free tool id. Its callback records the file of each
   function that starts between a test's setup and its teardown, and returns `DISABLE`, so each
   code object costs one event per window: the plugin restarts events at each test's start and at
   each shared fixture's setup and teardown. Module top level and class bodies don't count (their
   code objects aren't functions, `CO_OPTIMIZED` unset): they run at import, which every test of
   the file needs, and are reported once per unit as `everyTest`.
2. **Opens and listings** come from the audit hook, as before, now routed to the test whose window
   is open, to the fixture being set up, or to the unit being collected.
3. **Imports** come from the module graph: each module's direct imports (`imports`), and the files
   each module's own code opened (`opens`). A module imported lazily inside a test is that test's.
4. **Fixtures.** What a fixture's setup ran and opened goes to every test that requested it,
   directly or through another fixture, while pytest kept its value (`shared`: the setup's files,
   the tests' ids it served, and the window it was kept for). A function-scoped fixture is inside
   its test's window already.
5. **RFC 0003 §3.3's two rules, at a test's grain**: a test read the files its code imports
   directly, and what code outside the repository loaded on its behalf (a plugin pytest loaded,
   `-p app.ext`) is every test's.
6. **Below 3.12** there is no `sys.monitoring`: the plugin says why in `settings.watch` (`Python
   3.11 has no sys.monitoring, which watching a test needs (3.12 or later)`), every test reads its
   test file's list, and the run says so once (ADR 0078).
7. **xdist.** Each worker watches the tests it runs and writes its own facts; the controller
   watches nothing. Facts merge across processes as before: a test's lists are known only when
   every process that reported it said them.
8. **The cost of watching**, measured on Python 3.14.10 on an Apple M-series laptop (ten cores, under
   other load): a suite of 300 small tests in 20 files, each calling into two of ten modules, one
   fixture shared by a third of them; 15 interleaved rounds of three runs each, CPU time (user and
   system) of the pytest process, medians: without the plugin 0.532 s, with the plugin and
   `reads: file` 0.633 s, with `reads: test` (watching) 0.717 s. Watching costs **0.084 s of CPU per
   300 tests, about 0.28 ms a test, 13% over the plugin without it**. Wall-clock times varied more
   than that and aren't used.

## Consequences

- With `reads: test`, a change to a module only some tests call runs those tests.
- A value reached two imports away whose code never ran in the test, or a registry another module
  filled at import, isn't on its list: the known misses, which the audit's second run catches.
- Python 3.11 and older keep `reads: file` behaviour, said once a run.

## Alternatives considered

- **`sys.settrace` or `sys.setprofile` below 3.12:** an event for every call and line, many times
  the cost measured here, and it fights coverage tools for the same hook.
- **`LINE` events:** a file per line executed adds nothing a function start doesn't, at far more
  events.
- **Counting module top level per test:** it runs once, at the first import, so the first test of a
  file would read every module and the others none.

## Amendment 1: what a test was served, and what ran outside every window (2026-11-12)

Reviewers confirmed wrong reuses in item 4 and in what item 1 left out.

- A fixture asked for at run time (`request.getfixturevalue`, which pytest-lazy-fixture and
  parametrized fixture names use) isn't in `item.fixturenames`, so its setup's reads never reached
  the test that asked for it: with a session fixture set up by an earlier test, the later one read
  its value and none of its files. Now a test serves as requested every fixture its request
  resolved by the time it ended, those asked for at run time included, and a shared setup that
  starts inside a test's window always serves that test.
- Functions that session-level hooks call outside every test's window (`pytest_configure`,
  `pytest_sessionstart`, `pytest_collection_modifyitems`, `pytest_collection_finish` in a
  `conftest.py` or a repository plugin) landed on no test's list, while a value they kept reached a
  test. Now what runs, or is opened, while no window is open is read by every test of the
  process, and events are restarted when a window ends too, so a function seen in a window is
  seen again outside it.
- The plugin took a tool id for the session and called `restart_events()` at every window, which
  re-arms the events of every tool (coverage.py's sysmon core, a debugger), and a repository whose
  own tests take the same tool id failed. Now it watches only while no other tool holds a tool
  id: checked as it starts and at each window's start and end, and at another tool's
  `use_tool_id`, which it lets through after giving way. When another appears it stops for the
  rest of the process, releases its id, reports no test of the process as watched (each reads its
  test file's list), and says why once in `settings.watch`.

## Amendment 2: what the second review ran (2026-11-12)

A second review ran the plugin on pytest 8.4.2 and 9.2 and confirmed more. Each is fixed with a
test that failed first.

- **A test cut short.** The protocol's wrapper counted a test finished after its `yield` whatever
  came up through it, so a test a session fixture's `pytest.exit` or a `KeyboardInterrupt` cut
  short was recorded as passed, and with `unit: file` a file whose last test it was ran whole.
  Now a test finished only once its teardown's report was logged and no exception came up through
  its protocol.
