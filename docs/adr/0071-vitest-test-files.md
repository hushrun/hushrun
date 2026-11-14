# 0071. Vitest's test files

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** C-15, C-22, C-30, C-32, C-34, C-35; relies on C-14, C-31, ADR 0044, 0060, 0068, 0069,
  0070

## Context

RFC 0003 §3.9 names Vitest as the JavaScript runner to observe: the most common one behind
JavaScript's test scripts, which isolates test files by default. This decision is its adapter, the
second observed one. It was built against Vitest 4.1.12 (Vite 8) on Node 20, after a spike of
Vitest's own source.

## Decision

1. **Loading needs one line in the repository's Vitest config**, which `hushrun init` proposes in
   its "Change by hand" list and never writes:
   `plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())]`
   (ADR 0084 Amendment 5: now a conditional, which a config that type-checks itself accepts).
   Vitest takes no plugin, reporter or setup file from the environment (none of the variables it
   reads does that), and a preload through `NODE_OPTIONS` would have to patch Vitest's internals.
   Without `HUSHRUN_VITEST`, which hushrun sets only for a check whose `tests` aren't off, the line
   adds nothing; it loads from ES module and CommonJS configs alike, since Vite takes a promise in
   `plugins`. A config without the line (hushrun looks for `HUSHRUN_VITEST` in the config Vitest
   would load) loads nothing, and the run says so once with the line.
2. **The plugin is hushrun's, written to `<cache dir>/adapters/vitest-<hash>/`** with its setup
   file, whole, like pytest's. In Vitest's process it claims the run's directory (the first Vitest
   that starts with it; a nested one adds nothing), puts the setup file first in each project's
   `setupFiles`, appends to every module Vite makes outside `node_modules` one statement that
   records its id when it runs (after its own code, so no line moves and no source map changes),
   wraps the sequencer, and adds a reporter. Every hook is guarded: its own error makes the run's
   test files `not observed`.
3. **The unit rule is Vitest's own list** (C-30): the test files its `include` and `exclude` name,
   every project's (`TestProject.globTestFiles`), as the adapter listed them in the run that wrote
   the check's map. hushrun can't evaluate a TypeScript config, nor match Vitest's globs exactly,
   so the plan classifies by the map's list, and a file no run listed yet is a source, which runs
   everything once. The adapter confirms: it deselects nothing when Vitest's list isn't the plan's.
   The record classifies by this run's list.
4. **Reads** (C-31, C-35): the modules the worker ran for the test file, through the appended
   statement (static and dynamic imports, `import.meta.glob`, setup files, `vi.mock`'s modules and
   `__mocks__` files, CommonJS `require` through `require.cache`); the files it opened, read, looked
   at and copied through Node's `fs` and `fs/promises`, wrapped in the worker; every file below a
   directory it listed; and the config files, the config and every file it imports
   (`configFileDependencies`), which every test file reads. **Sources** are `.js`, `.mjs`, `.cjs`,
   `.jsx`, `.ts`, `.mts`, `.cts`, `.tsx`. Every other kind is rest: `node_modules`, lockfiles,
   `package.json`, `tsconfig*.json`, `.env` files, snapshots, `.vue` and `.svelte`, which plugins
   compile.
5. **`not observed`**: a child process, a worker thread, a native addon (the wrappers see each); a
   module some plugin made from no one file (`\0…`, `virtual:…`, but Vite's own helpers); a test
   file the setup file didn't see run; and, for a project, what runs outside the test file's
   worker or shares one: a pool other than `forks` and `threads`, `isolate: false`, browser mode,
   typecheck, a `globalSetup`, an environment or runner of the repository's, CSS processing, and
   a Vite plugin hushrun doesn't know (names other than Vite's `vite:` and `builtin:`, Vitest's,
   `alias` and `vite-tsconfig-paths`), since it may make a module from other files.
6. **Selection** (C-32): in the sequencer, after Vitest's own filters (file arguments, `--project`,
   `--changed`) and `--shard`, before any worker imports a test file. The plugin removes the listed
   test files, and when that leaves none of a list that had some, sets `passWithNoTests` for this
   run: Vitest's "no test files" would turn a pass into a fail. A filter that matched nothing stays
   Vitest's own failure, from before the sequencer. Unlike pytest's plugin it doesn't import reused
   files: Vitest gives the exact list after its filters, so an empty one is known without them.
   Coverage on (`--coverage`, or the config) reuses nothing: a partial run's coverage would read
   as a drop and could fail a threshold.
7. **Each file's end, from the main process**: the reporter's `onTestRunEnd` says, per test file,
   the tests chosen (not skipped), those that finished, whether it failed, and what the plugin
   deselected. Failures are reported by file unless the run was interrupted, had errors outside
   any file, or `bail` stopped it.
8. **Which COMMANDs**: one run of Vitest, `vitest run` or `vitest --run`, started as `vitest`,
   `npx [--no-install] vitest`, `pnpm [exec] vitest`, `yarn exec vitest`, `bunx vitest`, `bun x
   vitest`, `npm exec [--] vitest` or a path to it. Not offered, said once: `vitest` without `run`
   (it watches with a terminal attached), `watch`, `bench`, `list`, `--watch`, `--typecheck`,
   `--browser`, `--ui`. `npm test` hides which program runs: the default adapter is chosen, and
   does nothing for Vitest.
9. **Versions**: the plugin's hooks are Vite's (`config`, `configResolved`, `transform`) and
   Vitest's documented ones (`configureVitest`, the sequencer, a reporter's `onTestRunEnd`,
   `TestModule`); it adds its reporter and sequencer to `vitest.config` in `configureVitest`, which
   Vitest 4 runs before it makes its reporters and pool. Another major version of Vitest is `not
   observed`, and says so.

## Consequences

- A Vitest repository observes and reuses test files after one reviewed line in its config.
- Projects with a plugin of their own are never reused until hushrun knows it; most React and Vue
  projects' plugins are Vite's own (`vite:react-babel`, `vite:vue`).
- What a test opens through `fs` before hushrun's setup file runs (an environment's own module)
  isn't seen; an environment of the repository's is `not observed` for it.
- Node 22 is needed for Vitest 5, which this adapter doesn't know yet.

## Alternatives considered

- **Jest:** a reporter and a sequencer need config entries too, and its module registry is its
  own; Vitest was the RFC's choice and holds.
- **Vite's module graph per test file:** shared by all test files and missing imports with
  variables; the appended statement sees what ran.
- **`experimental.importDurations`:** collected only at collection, not for imports during
  tests, and experimental.
- **A preload through `NODE_OPTIONS`:** no documented hook into Vitest's workers or sequencer.
