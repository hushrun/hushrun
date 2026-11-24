# 0107. Node's test runner and Mocha

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-54, C-55, C-56, C-57; relies on C-30, C-31, C-32, C-39, ADR 0068, 0069, 0071, 0080,
  0081, 0085, RFC 0004

## Context

JavaScript had two observed runners, Vitest (ADR 0084) and Jest (ADR 0085). An audit on Node 22.20
and 24.11 found the two other runners JavaScript projects use most fell to the default adapter,
which does nothing for them: Node's own test runner (`node --test`), which needs no dependency and
which this repository's own npm packages use, and Mocha. `hushrun init` already knew
`node --test` as a test check; neither had a units adapter.

The audit also looked for compiled code. Plain JavaScript compiles nothing a later step reads:
Node compiles each module as it loads it, and V8's code cache is per machine and keyed by V8's
version. Transpilers and bundlers (Babel, esbuild, SWC, tsc's emit) turn files into files; where a
project runs one as a step, that step is a check whose outputs hushrun's reports (ADR 0052) and
the whole-check skip already cover, and none of the three keeps a content-addressed cache an
outside process could serve the way Gradle's and Xcode's are served (ADR 0104, 0105). esbuild and
SWC finish a large project in well under a second, which leaves no time a cache would buy back.
No build-block kind is added; B-110 to B-119 stay unused.

## Decision

1. **Node's test runner, loaded through `NODE_OPTIONS`.** hushrun writes its file to
   `<cache dir>/adapters/node-test-<hash>/` and adds ` --require <file>` to `NODE_OPTIONS`, as for
   Jest. COMMAND is `node [options] --test [patterns]`, `--test` among Node's own options. Not
   offered, said once: `--watch`, `--watch-path`, `--test-isolation=none` (and
   `--experimental-test-isolation=none`), where every test file runs in one process. It needs
   `fs.globSync` and `module.registerHooks`, Node 22.15 or later; an older Node writes why.
2. **The main process lists, and chooses, by `process.argv`.** The runner reads its patterns from
   `process.argv` after every preload, and lists files with its own glob. The adapter lists them
   the same way (the patterns, or the runner's default, `node_modules` left out, sorted) and, when
   that list is `hushrun.json`'s `testFiles`, coverage is off, no `--test-shard` is asked and no
   kept name has a glob character, replaces the patterns with the files `reuse` doesn't list; a
   pattern that matches nothing when none is left. A shard would split a shorter list otherwise:
   it leaves no file out, and says so. The main process hears the runner's own event stream (the
   `emit` of its `TestsStream`, found on the prototype chain) for how each test file's process
   ended, and claims the run's directory (`node-test-claim`); a second start says so with a
   `.started` file, as Jest's (ADR 0085 Amendment 4).
3. **One process is one test file.** The runner gives each test file a process of its own
   (`NODE_TEST_CONTEXT=child-v8`), so what that process read from its start to its exit is the
   test file's: every module Node compiled (`Module.prototype._compile`) or loaded (the module
   hooks' `load`), every file opened, read or looked at through `fs`, every directory listed.
   `HUSHRUN_NODE_TEST_IN` marks every process below it, which writes nothing.
4. **Tests, from the test file's own events**, which the runner reports in declaration order:
   `test:start` before a test's subtests, `test:pass` or `test:fail` after them. A test's id is
   Jest's rule (its suites' and parent tests' names and its own, joined with ` > `, ` [k]` for
   the k-th with an already-seen name). A test that has subtests, and tests whose ids collide,
   are never left out: a subtest runs only when its parent's body does.
5. **Leaving tests out, by `skip`.** `node:test`'s `test`, `it`, `describe`, `suite` and their
   `skip`, `only` and `todo` are wrapped, with each test's context's `test`, and each registration
   computes the id its events will have, through an `AsyncLocalStorage` of the suites above it;
   one `reuseTests` lists gets `skip: 'hushrun: passed before on the files it read'`, which its
   events carry back. `require('node:test')` gets the wrapped function through `Module._load`;
   `import test from 'node:test'`, whose default export no property can change, an ES module of
   the same exports, through the module hooks' `resolve`. A test left out that the plan didn't
   list (a registration whose id differed from its events') sends `leftOutUnlisted`, and the run
   goes again with nothing left out (C-32).
6. **What a test file's process failed outside its tests.** A hook of a suite fails every test
   under it; the file's own top-level hook, every test of the file. An uncaught exception or
   unhandled rejection in the process, a failure of the file's root that isn't a hook's, an exit
   code no failing test explains, and a test file the main process heard fail for a reason other
   than its subtests (a late timer's error, a crash, an exit before its tests) keep no test of the
   file: each sends `notKept`.
7. **Mocha, loaded the same way**, for `mocha`, `_mocha`, a path to either, `npx`, `pnpm [exec]`,
   `yarn [exec]`, `npm exec [--]` and `node [options] …/mocha/bin/mocha.js`, an installed Mocha
   10 or 11. Its class is found by its shape (`loadFiles`, `run`, `Runner`, `Suite`, read through
   descriptors, so that no other module's getter runs). Not offered: `--watch`, `--parallel`,
   `--dry-run`, and what runs no spec file.
8. **Mocha runs every spec file in one process.** So a spec file is never left out by not loading
   it: each loads as it would without hushrun, its top level and its root hooks included, and at
   the runner's start, before any hook, the tests `reuse` and `reuseTests` list are taken out of
   their suites, which Mocha then neither runs nor reports, nor runs the hooks of a suite left
   with none. Leaving out needs the spec files Mocha listed to be `testFiles`.
9. **What a Mocha spec file read** is what was read in its windows (while it loaded, and while
   its tests and their own hooks ran: Mocha says a test began before its `beforeEach` hooks, so
   after each hook the window is the running test's again), and everything the modules it loaded
   load, to the end, as Node's module graph has it: each module's `children`, which Node keeps for
   cached requires too, and the module hooks' resolutions for ES modules. A module that read a
   file once and kept it reaches every spec file that loads it, by every repository frame on the
   stack of the open (ADR 0085 Amendment 2). Every spec file reads what was read before the first
   loaded (a config, `--require`), outside every window, and in a root hook. A directory listed
   outside every window counts only when repository code listed it: Mocha lists `test/` as it
   collects spec files.
10. **A spec file that defines a root hook isn't a unit.** Its hooks run for every test of every
    spec file: it is a source every spec file reads, and is never left out; its own tests always
    run. `--file`'s files are the same. `.mocharc.*` of a code kind is read by every spec file.
11. **Mocha's outcomes** come from its runner's events (`pass`, `fail`, `pending`, `test end`). A
    hook that fails fails every test under its suite, a root hook's every test. An uncaught error,
    which Mocha charges to whichever test runs, and a failure outside any test keep no test of the
    run.
12. **`reads: test`** runs as `reads: file` for both (C-39's "can't watch one"): a test reads its
    file's list.

### The known misses

- **Node's runner:** none beyond C-31's of every runner: what isn't a file of the scope.
- **Mocha:** one spec file's test depending on another's having run (a registry a test fills, a
  global another file's test sets) is as without hushrun only when both run: leaving the first
  out changes what the second sees. What one spec file's top level does stays, since every file
  loads. This is pytest's case too, and an audit (C-34) is what catches it.

## Costs

30 test files of 10 tests each, each file requiring 2 of 10 modules, Node 22.20.0, Linux x64 on
4 CPUs, each setting alternated over 11 rounds after a warm-up, medians of wall time and of the
CPU time of the runner and its processes:

| Runner | No adapter (wall / CPU) | Adapter, `unit: test` (wall / CPU) |
|---|---|---|
| `node --test` | 0.70 s / 2.02 s | 0.81 s / 2.29 s (CPU ×1.13) |
| Mocha 11.7.5 | 0.17 s / 0.19 s | 0.19 s / 0.23 s (CPU ×1.21) |

That is 0.9 ms of CPU a test (9 ms a test file, most of it the preload's own load in each
process) for Node's runner, and 0.13 ms a test for Mocha, whose stack walk at each repository
`fs` call is its main cost.

## Consequences

- A project that runs `node --test` or Mocha reuses test files and tests with no file changed.
- Node's runner's internals aren't wrapped: the adapter goes by its public event stream, its
  `process.argv` contract and `node:test`'s exports. A release that reorders its events, or reads
  its patterns before preloads run, observes nothing or leaves out nothing, and says so: a test
  left out that the plan didn't list reruns the whole command.
- Mocha's deselected spec files still load: on a suite whose files take long to import, reuse
  saves the tests' time, not the loading.
- Node 20 isn't offered: it lacks `module.registerHooks`, without which ES modules can't be
  followed, and it reached its end of life in April 2026.

## Alternatives considered

- **A reporter through `--test-reporter`:** a reporter given in `NODE_OPTIONS` replaces the
  default one unless every other is given too, and changes the output; the stream's own `emit`
  changes nothing.
- **Leaving a Node test file out in its own process** (a stub for `process.argv[1]`): the runner
  would still report the file, as a passing test of its own.
- **Leaving Mocha spec files out unloaded:** a root hook it defined, or what its top level does,
  would vanish from every other spec file's run.
- **Mocha's `--parallel`:** each worker runs one spec file, which would fit Node's runner's model;
  left for later, said once.
- **AVA:** its workers are processes or threads per test file, as Node's runner's; not done here.
