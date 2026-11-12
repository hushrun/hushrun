# 0081. Vitest's tests, one by one

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-43, C-35; relies on C-32, C-39, ADR 0071, 0077, RFC 0004

## Context

ADR 0077 made a single test a unit beside its test file, with pytest as the adapter that knows
tests. RFC 0004 §4.2 names Vitest next: its id is its suites' names and its own, it is left out in
the worker after collection, and with `reads: test` it is watched through V8's precise coverage.
This decision is that adapter's half: the setup file and plugin of ADR 0071, extended. It was built
against Vitest 4.1.12 on Node 20, after reading Vitest's runner and running each claim below.

## Decision

1. **A test's id** is the names of its suites and its own, as Vitest formats them, joined with
   ` > `: `cart > in euros > rounds`. The k-th test of a file whose joined name the file already
   had is `<name> [k]`, counting in the order the file declares them (Vitest's shuffle reorders
   runs, not declarations), the suffix RFC 0004's Jest adapter takes too. A name that holds ` > `
   can make different suites join to one id (`describe('a > b')` and `describe('a')` ›
   `describe('b')`): those tests, and any id still another's after the suffix, are ambiguous; they
   are never told and never left out, so they run every time. Each case of `test.each` and
   `describe.each` is a test of its own, named as Vitest formats it. A formatted value that differs
   between machines (a path, a date, a random number, an object's address) makes an id that differs
   too: on another machine it is a new test, and runs, as a renamed test does.
2. **Leaving out tests**, in the worker, when the runner starts the file's own suite: after
   collection and after Vitest's filters (`.only`, `-t`, a line, tags), before the file's first
   hook. Each test `reuseTests` lists that Vitest would run gets mode `skip` and a skipped result,
   and every suite all of whose tests are then skipped gets mode `skip`, which Vitest reads after
   that point: its `beforeAll` and `afterAll` don't run. A test the file skips, a todo, a test a
   filter left out and an ambiguous one are never touched. The plugin hands the list to the workers
   in `vitest-tests` in the run's directory, written whole in the sequencer before any worker
   starts, and only where it may leave out test files (ADR 0071 §6: Vitest's list is the plan's,
   coverage is off, Vitest isn't watching files); without that file no test is left out or told.
   Vitest then prints the left-out tests as skipped (`Tests  3 passed | 1 skipped (4)`), counts a
   suite skipped whole in its total only, since it reports no result for its tests, and prints a
   file all of whose tests are left out as a skipped file. It exits 0 in each case, everything left
   out included: the files were collected, so its "no test files" doesn't apply. A file all of whose
   tests are listed is still imported; hushrun may list it whole instead (C-32).
3. **How each test ended**, written by the worker when the run's files end, after every hook:
   `finished` when its window closed (the runner's end of the test, after `beforeEach`,
   `afterEach`, `aroundEach`, fixtures and retries, or, for a test that skipped itself, the next
   boundary) with a result; `failed` when Vitest failed it, or when a suite above it failed in a
   hook of its own after it finished (`afterAll`, a cleanup, obsolete snapshots), so that a test
   whose teardown fails never reads as a pass. A failing `beforeAll` fails the file, by the main
   process as ADR 0071 has it, and the tests under it didn't finish. A file that can't be collected
   tells no test. Vitest's own verdict stands for retries (a pass on a retry is a pass, its time
   every try's) and for `test.fails`. A test the file skips (`test.skip`, `skipIf`, a skipped suite)
   and a todo finished and passed, with nothing run; one that called `ctx.skip()` finished. A test
   `.only`, `-t`, a line or a tag filter left out isn't told: it isn't a test that passed. A test
   Vitest would have run that a bail or a cancel stopped didn't finish.
4. **Watching a test** (`reads: test`). The setup file starts V8's precise coverage (call counts,
   by function) through `node:inspector` in the worker, before the test file is imported, and
   takes it, synchronously, at each boundary: the file's start (what ran while it was imported and
   collected: `loaded`), each suite's start and end (its `beforeAll` and `afterAll`: `shared`, for
   the tests under it), and each test's end (`ran`). The take at a test's start is left out when
   another test just ended: only Vitest's code runs between. A file counts when a function of it was
   called, but not the script's own function nor the function Vitest's module evaluator wraps each
   module in: that is a module's top-level code. Vite evaluates imports as live bindings read
   through getters, which are functions of the exporting module: a test that reads a constant from
   `pricing.ts` ran `pricing.ts`, and a module that built a value from another while it was
   imported ran that one while the file loaded, for every test. Files opened and directories
   listed come from the `fs` wrapping ADR 0071 has, by window. Each module's direct imports come
   from the worker's own module runner (`evaluatedModules`), dynamic imports included. Vitest's
   config files and the project's setup files are in `loaded` as files whose code ran (RFC 0004
   §3.2 rule 6). A listing in a suite's hook or while the file loaded is each test's it serves.
   Paths outside the repository and below `node_modules` are left out. hushrun computes each
   test's list from these; the adapter decides nothing.
5. **A test not watched alone.** A file with a concurrent test (`test.concurrent`,
   `describe.concurrent`, `sequence.concurrent`), or in which two tests' windows, or a suite and a
   test, were seen open at once, can't be watched test by test: none of its tests sends `ran`,
   `opened` or `dirs`, so each gets its file's list (RFC 0004 §3.2), and what wasn't followed
   inside any of them is the file's.
6. **`not observed`, per test.** A child process, a worker thread or a native addon inside a test's
   window is that test's alone; inside a suite's hooks, each test's that suite serves; while the
   file loaded, or after its suite ended, the file's, every test's. Every limit of ADR 0071 stays.
   The runner's boundaries are Vitest 4's `TestRunner` methods (`onBeforeRunSuite`,
   `onAfterRunSuite`, `onBeforeRunTask`, and the instance's `onAfterRunTask` and `onAfterRunFiles`,
   which Vitest wraps when it makes the runner); a runner of the repository's is already
   `not observed`. Where the boundaries never come (another copy of Vitest's runner), the file is
   written as ADR 0071 has it, with no tests: its check reuses by test file. The declared modes come
   from the file's task list as Vitest builds it; without them no skipped test is told.
7. **Vitest's coverage** (`--coverage`, the config, any provider) leaves out no test and watches
   none: the v8 provider shares V8's counters, and a partial run's coverage would read as a drop
   (ADR 0071 §6).
8. **The facts.** A worker that tells tests writes `hushrun.unit-facts/v1`: its unit's `tests`,
   `loaded` and `shared`, the file's `paths` from the repository root and `imports`; the main
   process's file is ADR 0071's. hushrun merges a test file's workers (a file in two projects runs
   each test in each): a test finished only where every worker that told it finished it, failed
   where any failed it, was left out where every one left it out, and what it read is known only
   where every one said it.
9. **The cost**, measured on Node 20.20.2 and Vitest 4.1.12, forks pool, 30 test files of 10
   trivial tests each importing two of ten modules, interleaved rounds on a heavily loaded machine
   (load average 65 and 129), CPU time of Vitest and its workers, medians of 7 and 9 rounds: no
   adapter 4.20 s and 4.41 s; ADR 0071's test files 4.60 s and 4.74 s; tests one by one, not
   watched, 4.60 s and 4.81 s; watched, 5.20 s and 5.54 s. Watching costs 0.6 to 0.7 s of CPU for
   300 tests, 2 to 2.5 ms a test, most of it each worker's coverage session and its takes; telling
   tests without watching costs nothing measurable.

## Consequences

- A Vitest check with `unit: test` runs again only the tests that failed or read a changed file,
  after the one config line ADR 0071 needs.
- A suite left out whole shows in Vitest's summary as tests neither passed nor skipped: Vitest
  sends no result for the tests of a suite it skips.
- Reading through a getter counts: a constant read from another module makes that module run for
  the test, which narrows `reads: test`'s first miss (RFC 0004 §3.2) to values copied before the
  test file's modules were evaluated, and to registries filled through no import.
- The boundaries and the module wrapper's offset are Vitest 4's: another major version is
  `not observed` (ADR 0071 §9), and a minor version that moves them reports no tests, which only
  runs more.

## Alternatives considered

- **A runner of hushrun's (`test.runner`):** documented, but it would replace a repository's own,
  and a second copy of Vitest's runner class would break the run instead of reporting nothing.
- **Outcomes from the main process's reporter:** ids would be computed in two processes, and the
  main process sees neither the modes a file declared nor what hushrun left out.
- **Vite's module graph for imports:** the server's static graph, shared by every test file; the
  worker's has what it evaluated, dynamic imports included. It still serves `hushrun why`.
- **A take at every test's start:** one more take a test, for Vitest's own code.
- **Leaving out tests through `-t` or test ids:** that is COMMAND's own filter, and a test it
  leaves out isn't one hushrun reused.
