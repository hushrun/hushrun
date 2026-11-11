# 0068. A check's test files, observed

- **Status:** accepted
- **Date:** 2026-11-10
- **Rules:** C-5, C-10, C-30, C-31, C-32, C-33; relies on C-9, C-25, D-4 to D-6, ADR 0044, 0048, 0060,
  0061, 0067

## Context

`hushrun run NAME -- COMMAND` skips a check when the scope's exact files already passed it. One
changed file runs the whole check again: every test of it. The next step reuses, inside a check that
missed, the test files whose own inputs didn't change. Before anything is reused, hushrun has to show
that it can tell what each test file read, decide each one's state the same way every time, and say
what reuse would have done, on real runs, without changing any of them.

That is this decision: `tests: observe`, for pytest, where a plugin inside the runner observes what
each test file read, and for Rust (`cargo nextest`, `cargo test`), where the compiler's dep-info says
what each test binary was built from. Reuse itself (`tests: reuse`) is a later decision; this one
leaves it its seams.

## Decision

1. **Configuration** (C-5). `checks.<name>.tests` is `off` (the default), `observe` or `reuse`, or
   `{mode, ignore, always}` with globs from the repository root. `off` is as good as absent. It isn't
   part of the check's definition (E-6), so turning it on keeps every key and every piece of
   evidence. In this version `reuse` observes, and says so in one line.
2. **Off changes nothing.** With `tests` absent or `off`, COMMAND gets no new variable and hushrun
   writes no new file. With results off (C-11) or with `--rev` or `--digest`, which name files other
   than the ones COMMAND reads, nothing is observed either; the latter says so.
3. **One adapter per COMMAND.** A COMMAND that is `cargo [+toolchain] nextest run …` or `cargo
   [+toolchain] test …` (not `--doc`, not `--no-run`) gets the Rust adapter; any other gets the pytest
   adapter, which does nothing in a COMMAND that never starts pytest. Where loading the plugin could
   stop pytest (tox and nox choose which variables reach their environments; a Python started with
   `-I` or `-E` ignores `PYTHONPATH` while pytest still reads `PYTEST_ADDOPTS`), nothing is loaded,
   and the run says why.
4. **The contract is a directory** (C-32). hushrun makes `<cache dir>/units/runs/<ULID>/`, writes
   `hushrun.json` there (the repository's root, and the units an adapter may deselect, none in this
   version), names it in `HUSHRUN_UNITS`, and removes it however the run ends; a hushrun that was
   killed leaves one that the next run sweeps a day later. Adapters write facts there, one file per
   process, each whole (a temporary file, then a rename). hushrun decides every state from them, with
   one function (ADR 0060): a failure is a failure; a test file that didn't run whole is `not run`;
   one with something the adapter couldn't follow is `not observed`; one that passed counts only
   when COMMAND passed or the adapter reports each failure by test file, as both of these do.
5. **pytest's unit rule is its `python_files`, read before any test runs** (C-30): from the first of
   `pytest.ini`, `pyproject.toml` (`[tool.pytest.ini_options]`), `tox.ini` (`[pytest]`) and
   `setup.cfg` (`[tool:pytest]`) from COMMAND's directory up to the root, else `test_*.py` and
   `*_test.py`. The plugin reports the value pytest used; when the two differ, or a pattern names a
   directory, no test file is observed.
6. **The pytest adapter** is embedded in the binary, written to `<cache dir>/adapters/` when it isn't
   there, and loaded with `PYTHONPATH` and `PYTEST_ADDOPTS=-p hushrun_pytest`, each keeping what
   COMMAND's environment had. Its first file parses on any Python, and below 3.8 it does nothing. It
   follows imports to the end through a module graph, sees opens through the audit hook (PEP 578),
   makes an opened file an edge of every repository module on the call stack, treats a repository
   module that code outside the repository loaded first as read by every later test file, and adds
   each directory's `conftest.py` and `__init__.py`. Every hook is guarded: an error of its own makes
   its process's test files `not observed`, and never reaches pytest.
7. **What a test file selected and finished decides whether it ran whole.** The plugin counts each
   test file's tests after every deselection (`-k`, `-m`, other plugins), and the tests that finished.
   With pytest-xdist every worker collects every file and writes its own facts; a test file's
   selected tests are the most any process saw, its finished tests the sum of all of them, so a
   worker that died leaves its test files `not run`.
8. **A process started outside any test file** counts against every test file only when the
   repository's own code started it (a `conftest.py` that generates files as it loads). One that
   code outside the repository started alone, such as pytest-xdist's workers asking `uname` where
   they run, is no test file's. A process started by a test file makes that test file `not observed`,
   as forks and the repository's native code do.
9. **pytest's own code is known by the directories its packages are loaded from**, never by a
   directory's name in a path: a repository or a virtual environment with a `py/` directory isn't
   pytest's, and a module code there loads first stays ambient.
10. **The Rust adapter is a target runner** (C-32). hushrun sets `CARGO_TARGET_<TRIPLE>_RUNNER` for
    the host (as `rustc -vV` says it, with COMMAND's toolchain), for each `--target` and for
    `CARGO_BUILD_TARGET` to `hushrun __hushrun-test-runner <TRIPLE> <N> <runner>…`, chaining to the
    runner the environment had set there. When a Cargo configuration file sets a runner, or COMMAND
    does, or hushrun's own path holds a space, it sets nothing and says why: chaining to those safely
    needs Cargo's configuration read as Cargo reads it. The runner starts the binary with the
    environment it would have had (its runner variable as the environment had it, no
    `HUSHRUN_UNITS`), passes stdio and the exit status through, passes SIGINT, SIGTERM, SIGHUP and
    SIGQUIT on, and ends by the test's signal when the test did. A `--list`'s output goes on byte for
    byte as it comes, and hushrun keeps a copy. Each start writes one file.
11. **The build is COMMAND's, asked of Cargo after it** (C-30). hushrun runs `cargo metadata
    --no-deps` and `cargo test --no-run --message-format=json --locked` with COMMAND's own build
    options. Every artifact must be fresh: the first that isn't means this isn't COMMAND's build, Cargo
    is stopped at once, and no test binary is observed; so is a build that takes over 120 s to say
    what it built. A test binary's unit is its target's root file. It read what its own dep-info names,
    what the dep-info of every workspace library it may link names (its package's path dependencies
    of any kind, then theirs that aren't dev-dependencies, closed: more than it links, never less),
    and, for an integration test, a bench or an example, what every target of its package was built
    from (the package rule). The sources are the `.rs` files some dep-info of this build names; every
    other file is in the rest set, whatever its extension. A binary whose own dep-info, or a target's
    it reads, can't be found is `not observed`.
12. **A failing run still records what ran whole** (C-32). This is the laptop's case: one test
    fails, it is fixed, and every other test file has already passed on the files it read. With
    nextest, which lists a binary's tests and starts it once per test, a binary ran whole when
    COMMAND passed, or when each test its list names (ignored ones aside) has a start that exited 0
    and COMMAND says nothing of ignored tests, which can run tests the list doesn't count. nextest
    stops at the first failure by default; a binary it listed and stopped before is `not run`. With
    `cargo test`, which starts a binary once, a binary ran whole when its start ended. A binary with a
    start that failed failed. Doctests start no binary and are no test file: they stay COMMAND's.
13. **What `observe` says.** For each test file, hushrun computes the key the plan will, from what
    the map says it read last time (for Rust, from this build's own lists), finds the newest result
    under it that counts here, and counts the test files `tests: reuse` would have reused: one line
    before the run's last, ``with `tests: reuse`, 12 of 340 test files would have run; 328 already
    passed on the files they read``. When it would have run every test file, it names
    one file of the rest set that changed since the map was written, or a source that is new or gone.
    A test file that would have been reused and failed is a disagreement: a line says so, and the
    check's audit file keeps it.
14. **Unit results stay on this machine** (C-33). One object per run, its key, its result and its
    units' `{key, result, durationMs}`, in `<cache dir>/results/unit-runs/v0/<run key>/`, and an entry
    per unit key in `<cache dir>/results/units/v0/<unit key>/` that names it; recorded only when the
    check's own result was, and swept with this machine's results 30 days after their run (ADR 0067).
    Whether a unit's pass counts here is `RunResult::counts_in` of the run it went with: the rule a
    check's pass is held to (C-10, C-25). A store behind a trait, `UnitResults`, is the seam a server
    takes later.
15. **`units` in a run's result is local in this version** (C-10). `{total, ran, reused}` goes in
    the result this machine keeps, and is left out of what a server is sent: nothing about test files
    goes to a server before the batch protocol does, and a server may check a result's fields.
16. **The map and the audit** are one file each per check, `<cache dir>/units/<check id>.map.json` and
    `.audit.json`, where the check id hashes the run key's fields but `digest`, `external` and
    `counts_from`. Each is read, changed and written whole while a lock file beside it is held. The
    map keeps, for each test file, the units and sources it read last time it ran here, and the rest
    and sources sets of that run; it is never trusted. Neither is written when files changed while
    COMMAND ran.

## Consequences

- A check can say how much of its test time reuse would save, on its own runs, before reuse exists.
- Two adapters, one of each kind, sit behind one contract and one deciding function: the next
  runner adds a reader of facts, not a new set of rules.
- `HUSHRUN_UNITS`, `hushrun.units/v0` and `hushrun.unit-facts/v0` are contracts between hushrun and
  its adapters; the target runner's command line is hushrun's own and may change with it.
- The pytest plugin's module name, `hushrun_pytest`, is taken in every observed pytest.
- An integration test of a Rust package reads its whole package, and a crate that links every other
  makes every other's change run it: as observed, reuse is modest on workspaces of that shape.
- What a Rust test opens at run time is in the rest set: on a repository whose tests read
  `testdata/`, nearly every change runs everything until a later version narrows it with evidence.

## Alternatives considered

- **An extra build before COMMAND, to know the binaries:** it doubles the work when it differs, and
  the build COMMAND makes is the one whose dep-info counts.
- **A wrapper for rustc (`RUSTC_WORKSPACE_WRAPPER`) to see each crate's `--extern`s:** exact, but it
  changes Cargo's fingerprints, so it rebuilds the workspace, and sees nothing of a build that was
  already fresh.
- **Every `deps/*.d` of a crate, stale builds included:** more reads is safe for a key, but a stale
  dep-info that names a `.rs` file no build reads any more would put a data file in the sources set,
  the hole milestone 1 found.
- **nextest's JUnit for pass and fail:** it needs a profile in the repository's configuration or an
  argument, and the target runner already sees every start.
- **Recording nothing when COMMAND fails:** sound, and it loses the case reuse is most for.
- **Counting every process started outside a test file against every test file:** pytest-xdist's
  workers start some on every macOS run, which would leave nothing observed under xdist.
- **Sending `units` to the server now:** additive for the server read here, but nothing about test
  files goes to a server until the batch protocol decides what does.
