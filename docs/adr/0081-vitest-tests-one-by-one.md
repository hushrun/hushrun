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
   ` > `, the file excluded: `cart > in euros > rounds`. The k-th test (k from 2) of a file whose
   joined name the file already had is `<name> [k]`, in the order the file declares them (Vitest's
   shuffle reorders runs, not declarations). Tests whose ids collide (the same id from different
   suites, `describe('a > b')` and `describe('a')` › `describe('b')`, or a suffixed id that still
   equals another) are told as one entry per id: failed if any of them failed, finished only if all
   did, never with a list of their own, and `neverLeftOut`, so hushrun records no pass of theirs and
   no plan lists them (C-39); they run every time. RFC 0004's Jest adapter takes the same rule. Each
   case of `test.each` and `describe.each` is a test of its own, named as Vitest formats it. A
   formatted value that differs between machines (a path, a date, a random number, an object's
   address) makes an id that differs too: on another machine it is a new test, and runs, as a
   renamed test does.
2. **Leaving out tests**, in the worker, when the runner starts the file's own suite: after
   collection and after Vitest's filters (`.only`, `-t`, a line, tags), before the file's first
   hook. Each test `reuseTests` lists that Vitest would run gets mode `skip` and a skipped result,
   and every suite all of whose tests are then skipped gets mode `skip`, which Vitest reads after
   that point: its `beforeAll` and `afterAll` don't run. A test the file skips, a todo, a test a
   filter left out and one whose id another shares are never touched. The plugin hands the list to
   the workers in `vitest-tests` in the run's directory, written whole in the sequencer before any
   worker starts, and only where it may leave out test files (ADR 0071 §6: Vitest's list is the
   plan's, coverage is off, Vitest isn't watching files); without that file no test is left out or
   told. Vitest then prints the left-out tests as skipped (`Tests  3 passed | 1 skipped (4)`),
   counts a suite skipped whole in its total only, since it reports no result for its tests, and
   prints a file all of whose tests are left out as a skipped file. It exits 0 in each case,
   everything left out included: the files were collected, so its "no test files" doesn't apply. A
   file all of whose tests are listed is still imported; hushrun may list it whole instead (C-32).
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
   listed come from the `fs` wrapping ADR 0071 has, by window, and each open is also kept with
   every module of the repository on its stack (`opens`), so that a file a module read once and
   kept reaches each test that runs that module's code (RFC 0003 §3.3). Each module's direct
   imports come from the worker's own module runner (`evaluatedModules`), dynamic imports
   included. Vitest's config files and the project's setup files are `everyTest`: what Vitest
   loads for every test of the file (RFC 0004 §3.2 rule 6). A listing in a suite's hook or while
   the file loaded is each test's it serves. With `reads: test` and Vitest's coverage on, the main
   process's facts say why no test was watched (`settings.watch`), and the run says it once.
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
8. **The facts.** A worker that tells tests writes `hushrun.unit-facts/v1`: its unit, named by its
   path from the root, with `tests`, `loaded`, `everyTest` and `shared`; the file's `paths` from the
   repository root, `imports` and `opens`. The main process's file is ADR 0071's, with
   `settings.watch` when it says why no test was watched. hushrun merges a test file's workers (a
   file in two projects runs each test in each): a test finished only where every worker that told
   it finished it, failed where any failed it, was left out where every one left it out, and what it
   read is known only where every one said it.
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
  the test. A value two imports away, RFC 0004 §3.2's first miss and C-40's, is caught with
  Vitest: the module that built it from another read it through that one's getter while the file
  loaded, so every test of the file has it (a test holds it), but not through a chain of `export *`
  barrels, which Amendment 1 follows by `reexports`. The second miss stays: a registry
  that a module imported only for that filled at import, read through no function of the filler.
  A value a module copied before Vite evaluated it (a native module, a dependency in
  `node_modules`) is outside what is watched.
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

## Amendment 1: re-exports are followed, and what a value two imports away still misses (2026-11-12)

The Consequences said a value two imports away is caught with Vitest. A reviewer showed it isn't
for a chain of barrels: the test imports `rate` from `src/index.js`, which has `export * from
'./sub/index.js'`, which has `export * from './impl.js'`. Vite runs `export *` through getters of
its own runtime, below `node_modules`, so no function of either barrel runs when the test reads
`rate`; the test's list held the outer barrel (the test file imports it) and `impl.js` (its
function ran), not the middle one, and pointing the middle one at another file left the test
reusable, and the check passed.

The decision: the plugin reads each module Vite makes, outside `node_modules`, for `export * from`
and `export { x } from` (its syntax tree, and its text, since a re-export read where there is none
only widens a list), resolves each through Vite, and appends one statement that hands the result to
the worker; the worker sends it as the facts' `reexports`, which a test's list follows to the end
(C-40). A specifier that doesn't resolve is text that only looks like a re-export.

What is caught, then: a value read through a getter of the module that exported it (a named
export, an `export { x } from`, which Vite compiles to a getter in the barrel), a value a module
built from another while the file loaded (every test of the file has the module that read it), and
a chain of re-exports, however long. What isn't: a registry filled at import (the second miss), a
value two imports away that a module outside the repository or Vite's evaluation copied, and a value
copied into a module through something that is neither an import, a getter nor a re-export, read by
no function of the way. Each stands under audit runs, as RFC 0004 §3.2 has it.

## Amendment 2: skips and starts (2026-11-12)

Each test the file skips (`test.skip`, `describe.skip`, `test.todo`, `test.skipIf`) and each that
skipped itself (`ctx.skip()`) is told `skipped`, and the main process's file counts each test file's
skipped tests (`skipped`), so that in a run whose COMMAND failed none of them, and no file with one,
records a pass (ADR 0077 Amendment 2). The plugin names the start of Vitest that claimed the run
(`start`, in its file and, through `HUSHRUN_VITEST_START`, in each worker's); only that start writes,
so a second Vitest in one COMMAND adds nothing. Its own merge keeps C-32's rules: a test file ran
whole only when it did in every project that ran it, and a test finished only where every worker
that told it, and didn't leave it out, finished it. When Vitest says its run ended more than once,
the main process's file adds each end to the earlier ones instead of replacing them.

## Amendment 3: a second start in one COMMAND (2026-11-12)

Only the start of Vitest that claims the run writes facts, so with a script such as
`vitest a && vitest b` the second start went unobserved, and the store kept passes
of the first for tests the second failed. A start that finds the run claimed, and that no start
above it named (`HUSHRUN_VITEST_START`, which a Vitest one of the run's tests starts inherits, and adds
nothing), now leaves a `.started` file in the run's directory and never removes it (C-32). Its
content says why, `{"why": "Vitest started more than once in its command, and hushrun observes
only the first start: what another start ran can't be told"}`, which the core says once in place
of its own sentence: the contract now reads a `.started` file's `why`, when its content is a JSON
object with one. No unit and no test of that run records a pass; a fail records as usual, and the
second start's own failure is COMMAND's. A COMMAND with one start is unchanged.

## Amendment 4: a start that runs no test file, and an error no test owns (2026-11-12)

A reviewer ran `vitest list`, `vitest related` with nothing related and a `vitest run` that found no
test file before the run of the tests in one script: each claimed the run when its config loaded,
so the start that ran the tests was taken for a second start, and no pass of the script was ever
kept. Now a start whose reporter never heard a run end with a test file run or left out gives the
claim up as it ends (the claim file reads `released:<pid>`); a start after it takes the run and
removes the main process's file that one wrote, and with none, that file stands. A start that finds
the run claimed says it is a second start only when a run with test files starts (the reporter's
`onTestRunStart`): `vitest list` sorts its files and starts no run.

An error no test owns (an unhandled rejection, a timer that threw after its test) makes Vitest fail
the run, and its main process's facts keep no pass of it (`failuresByUnit: false`). `hushrun why`
then had nothing true to say of the tests. The main process now keeps each such error with the test
file Vitest says it came from (`settings.unhandled`), and every test of the run that didn't fail is
told not finished with a `notKept` sentence naming them, which the core keeps with the test's row
(C-37). A run that records nothing stores its rows only once the core does so.

The same sentence is the run's reason too: the adapter gives it to the core as a facts file's
top-level `notKept` would, so that `hushrun why` says it of each test file of the run, the files
that passed included, rather than the core's "its run failed, and its adapter doesn't report each
failure by test file". Each test that didn't fail keeps it as its own `notKept`.


## Amendment 5: the config line a project type-checks, and what real projects measured (2026-11-13)

The line ADR 0071 has init propose, `plugins: [process.env.HUSHRUN_VITEST &&
import(process.env.HUSHRUN_VITEST).then((m) => m.default())]`, broke svgo's own test command:
svgo (Vitest 4.1.10) type-checks its `vitest.config.js` (`typecheck.enabled`), the expression is
`"" | Promise<any> | undefined`, `""` is no `PluginOption`, and plain Vitest, with no hushrun
anywhere, failed with every test passed. Init now proposes `plugins: [process.env.HUSHRUN_VITEST ?
import(process.env.HUSHRUN_VITEST).then((m) => m.default()) : undefined]`, which types as a plugin
option under `strict` in `defineConfig`, an object of `ViteUserConfig` and a project's plugins
(tested; the old line fails each, and a plain object without a type takes both). Vitest 4 has no
`vitest.workspace` file; its `projects` is tested. The adapter still looks for `HUSHRUN_VITEST`, so
a config with the old line loads it and isn't told it is missing.

On svgo, with a check's `tests` on, init proposed the line, which would add nothing: `typecheck`
makes every test file `not observed`. Where a config's text turns on `typecheck`, sets `isolate:
false` or runs tests in a browser, init now proposes no line and says so in one. Without `tests` on
in `hushrun.yaml`, init says nothing of test files: they stay off until a check asks (ADR 0068).

What the first runs on real repositories measured (2026-11-13, with Vitest: eemeli/yaml at 528ef30
and svg/svgo at 31194fb; beside pytest's pallets/click and encode/httpx and Jest's moment/luxon and
jest-community/jest-extended), with no wrong reuse in 270 mutations and replays of real commits:

- In `observe`, `reads: file` cost 10 to 46% of wall time across the six projects, and `reads: test`
  81 to 453%. yaml: 3.1 s plain, about 17 s with each test watched (+453% wall, +177% CPU); its
  `reads: file` +15%. svgo: `reads: test` +94%.
- On mutations, `reads: test` ran only about 2 to 10% fewer tests than `reads: file` on these
  projects, because their tests import the package through a barrel or `__init__` that re-exports
  everything: yaml's source mutations reran a mean of 3,272 of 3,404 tests with `reads: test` and
  3,337 with `reads: file`, and nearly every replayed commit reran everything in both.
- Most Vitest 4 projects tried can't be observed at all: zod, svgo and pinia turn on `typecheck`,
  valibot sets `isolate: false`.

That is the cost and the benefit today.
