# Changelog

All notable changes to the hushrun CLI, whose releases read this file. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the CLI follows [Semantic Versioning](https://semver.org/). Contract changes (JSON outputs, exit
codes, environment variables, event formats, the ledger layout) are listed first in each release.

## [Unreleased]

A test as the unit ([ADR 0077](docs/adr/0077-a-test-as-the-unit.md)): with `tests.unit: test`, a
check reuses each test of a test file on its own, beside its test files, for pytest, Vitest,
Jest, Go, Gradle, Maven, `swift test` and CTest. New, and off unless a check asks.

Build blocks ([ADR 0076](docs/adr/0076-compiled-crates-are-reused-between-worktrees.md)): a crate
the compiler already built from the same inputs is reused, in this worktree or another on the same
machine, through a rustc wrapper that `hushrun run` sets. New, and off until a person turns it on.

### Added

- Contract: **`checks.<name>.tests.unit`** (`file`, the default, or `test`) and **`tests.reads`**
  (`file`, the default, or `test`, which needs `unit: test`). With `unit: test`, a test file whose
  own key didn't pass has its tests reused one by one, each under a key of its own
  (`hushrun.unit-key/v1`); a test that starts a process costs only itself; and an audited test
  that fails runs once more alone. pytest, Vitest, Jest, Go, Gradle, Maven, `swift test` and CTest
  know tests; Cargo reuses by test binary and says so once. With `reads: test`, pytest on Python
  3.12 or later, Vitest and Jest watch each test, and a test is keyed by what it read alone
  ([ADR 0078](docs/adr/0078-what-a-test-read.md),
  [ADR 0080](docs/adr/0080-pytest-watches-a-test.md)); below 3.12, and for the other adapters, it
  runs as `reads: file`, and says so once.
- Contract: **`units.tests`** in a run's result, `{total, ran, reused}`, for a run with
  `unit: test`. A run sends its results failures first, up to 50,000 and 8 MiB; an older server's
  5,000 gets the first 5,000 once more, said once, and one that refuses a `savedMs` with no test
  file reused gets the run without it. `HUSHRUN_UNITS`'s `hushrun.json` gains `unit`, `reads` and
  `reuseTests`, and an adapter's file may be `hushrun.unit-facts/v1`, with each unit's tests.
- Contract: an adapter's facts may send each unit's `everyTest`, the file's `opens` and
  `reexports` (followed to the end on a test's own list), `settings.watch`, and each test's
  `neverLeftOut`.
- Hints hold tests ([ADR 0079](docs/adr/0079-hints-with-tests.md)): `hushrun.unit-hints/v1`
  carries a check's tests, their ids sealed with XChaCha20-Poly1305 under a key the server never
  has, so a machine with an empty cache reuses single tests on its first run. A hint with no tests
  is version 0, as before.
- `hushrun why NAME UNIT::TEST` explains one test; its JSON gains `test`. `hushrun ready` counts
  tests where tests are the unit.
- **Vitest** ([ADR 0081](docs/adr/0081-vitest-tests-one-by-one.md)): with `unit: test`, each test,
  named by its suites' names and its own, is reused on its own, and with `reads: test` watched
  through V8's coverage.
- **Jest** ([ADR 0082](docs/adr/0082-jest.md)): Jest 29 and 30 are observed through
  `NODE_OPTIONS`, test files and, with `unit: test`, each test.
- **Go** ([ADR 0083](docs/adr/0083-go-tests-one-by-one.md)): with `unit: test`, a package's tests
  are listed, left out with `-test.skip` and read from the binary's own output; with COMMAND's own
  `-run` or `-skip`, by package.
- **Gradle** ([ADR 0084](docs/adr/0084-gradle.md)): test classes and, with `unit: test`, their
  methods, for Java, Kotlin and Android's local unit tests, on Gradle 8.14 and 9, after one line
  in the settings file that `hushrun init` proposes.
- **Maven** ([ADR 0085](docs/adr/0085-maven.md)): `mvn` and `./mvnw` reuse test classes and, with
  `unit: test`, single tests, through `MAVEN_OPTS`, with no file of the repository changed.
- **`swift test`** ([ADR 0086](docs/adr/0086-swift-test.md)): test targets and, with `unit: test`,
  XCTest and Swift Testing tests, through hushrun's `swift` on COMMAND's `PATH`.
- **CTest** ([ADR 0087](docs/adr/0087-ctest.md)): C and C++ test programs built with CMake and,
  with `unit: test`, each CTest test, through hushrun's `ctest` on COMMAND's `PATH`.
- Contract: **`hushrun build on|observe|off|status|clean`**. `on`, `observe` and `off` save the
  mode for this user: `observe` compiles everything, stores blocks and says what would have been
  reused, and `on` reuses. `status`, also `hushrun build` alone, shows the mode, the store's
  folder, its size and its blocks by age and by repository. `clean` empties the store and clears
  suspended keys. `--format json` gives each as an object.
- Contract: **`HUSHRUN_BUILD`** (`off`, `observe` or `on`) sets the mode for one command and leaves
  the saved one; **`HUSHRUN_BUILD_KEEP_DAYS`**, at least 1, how long an unused block is kept;
  **`HUSHRUN_BUILD_DIR`**, an absolute folder, the store instead of `<cache directory>/build`. A
  value that can't be used turns reuse off for the run, and `hushrun run` says so in one line.
  **`HUSHRUN_BUILD_SESSION`** is set by `hushrun run` for the wrapper, not for people.
- Contract: **`hushrun run` sets `RUSTC_WRAPPER`** for the command it runs, in both forms, when
  the mode is not `off`: to a link named `hushrun-rustc` that points at the hushrun binary. It
  doesn't when `RUSTC_WRAPPER` is set already, in CI, on Windows, for an isolated check, or with
  no cache directory. A `cargo` outside `hushrun run` is untouched, and cargo's configuration is
  never edited. With `tests: reuse`, the build made before the command and a test file's second
  run compile through it too, in the same session, and the run says once what its compiles came
  to.
- Contract: **`build.json`**, beside the credentials file, holds the mode, `keep_days` (15),
  `never_store` (package names), `env` (more variable names for the key), `min_free_gb` (10) and
  `programs`, written whole under a lock. A file `hushrun run` can't read means it reuses nothing
  and says why; `hushrun build status` exits 2.
- Contract: **`programs` in `build.json`** is `movable`, the default, or `tied`; any other value
  makes the file invalid. `movable`: on macOS a program that links with debug info gets a debug
  bundle, `<program>.dSYM` beside it in `target/`, and keeps no debug map of its own, so it can be
  reused in another checkout and its backtraces keep `file:line`. That costs a `dsymutil` run per
  program and about twice the bytes stored on a first build. `tied`: a program is compiled as
  asked and reused only in the target folder that built it. **`hushrun build status`** shows the
  setting, as `programs` in its text and in its JSON.
- Contract: **`SOURCE_DATE_EPOCH` is part of every compile's key**, since compilers and build tools
  take "now" from it when it is set. hushrun never sets it.
- Contract: **the `build` folder in the cache directory** is the store: `blocks/`, the hints and
  suspensions under `steps/`, `tmp/`, `trash/`, the runs' `sessions/`, the output folders the
  wrapper has written records in under `out-dirs/`, the wrapper's link under `bin/` and the
  sweep's `swept` stamp. One store serves every repository on the machine. In each output folder,
  `.hushrun/` holds the wrapper's records beside the outputs, `units/` among them, and goes with
  `cargo clean`.
- Contract: after its command, `hushrun run` writes one line of what the compiles came to:
  `build: 331 of 372 crates reused, 41 built, saving about 2m 40s`, `build: 41 crates built, none
  reused`, or in `observe`, `build: observing: 2 of 3 crates would have been reused, 1 differed
  from what a reuse would have given`. Then at most three lines for the crates it made cargo
  rebuild, such as `build: app is rebuilt: a file was added to migrations, which cargo doesn't
  watch` or `build: 4 more are rebuilt for changes cargo doesn't watch`; then at most five notes,
  such as a key suspended, and once `build: nothing was stored because the disk has less than 10
  GB free`.
- **`hushrun run` rebuilds a crate that changed in ways cargo can't see.** Before its command,
  with reuse on or in `observe`, it removes the outputs of a crate it has seen compiled in the
  target folders here when a file was added to a folder the crate reads, a file a known macro
  reads changed, a file appeared where a module could also have been, or a file's bytes changed
  under a date older than the last build. Cargo then builds it. A new migration for sqlx's
  `migrate!` used to leave the old library in place; now the run says `build: app is rebuilt: a
  file was added to migrations, which cargo doesn't watch` and the library has it. Nothing is
  asked of the person.
- Inside `hushrun run` with reuse on, debug info names workspace files from the workspace root,
  so a debugger shows source from there, and the first build in an output folder is not
  incremental. Rebuilds after an edit stay incremental.
- `hushrun run` deletes blocks unused for 15 days, at most once a day. There is no size limit.
  Nothing leaves the machine.
- Tested on macOS and on arm64 Linux in a container: 0 differences between a block and a fresh
  compile in 5,564 compiles on Linux. There, proc macros, build-script binaries and programs move
  between checkouts with none of the macOS steps, since ELF has no install name or debug map and
  the debug info linked into each output names workspace files from the workspace root. Where the
  file system can't clone, as on ext4, a reuse is a copy: three checkouts took 38,997 MB of disk
  with reuse on against 35,164 MB without, while a further checkout built in about 23 s against
  74 to 87 s. Not tested: x86-64, other file systems, other linkers, musl.

### Changed

- Contract: this machine's store of what test files read is now
  `<cache directory>/units/<id>.v2.sqlite`, with each test file's tests. When only the older
  `<id>.v1.sqlite` is there, a run imports it once and leaves it as it was, for an older hushrun.
  A CI cache step should name `units/*.sqlite`: one that names `units/*.v1.sqlite` restores a file
  no run updates, and a run that imported one in CI says so. The step `hushrun init` proposes now
  names `units/*.sqlite`.
- A skipped test in a run whose command failed records nothing, and its test file records no
  pass: a suite setup that failed, or a runner that stopped, can skip what it hides. In a run that
  passed, a skip is a pass, as before. An adapter's facts may say `skipped`; pytest does.
- A test file that failed while none of its tests failed, such as a failing `afterAll`, records
  no pass for any of its tests: the next run runs them, and what failed runs again.
- When an adapter finds after the command that its runner left out something the plan didn't
  list, `hushrun run` says so in one line and runs the command once more with nothing left out,
  going by that run alone; if that can't be done, or it happens again, nothing is recorded and the
  run fails, saying why. An adapter's facts may say `leftOutUnlisted`.

### Fixed

- Two starts of a test runner in one command, such as two pytests or `ctest -L a; ctest -L b`,
  no longer add up to a test file that ran whole: a test file's tests are counted within each
  start, and one of whose tests never ran records no pass. An adapter's facts may say `start`;
  pytest sends it. A test told by id finished only when every process that ran it finished it.
- A pytest test that `pytest.exit` or an interrupt cut short no longer counts as finished: it
  records nothing, and its test file, which didn't run whole, records no pass. Before, with
  `tests: reuse`, the file could be recorded as passed when that test was its last.
- A pytest fixture of a wider scope whose teardown fails keeps every test it served from a pass,
  in every file it served: pytest reports the error on whichever test ran last, and the test that
  caused it no longer keeps its pass.
- A pytest test file skipped whole as it loads (`pytest.skip(allow_module_level=True)`,
  `pytest.importorskip`) counts as skipped, so in a run whose command failed it records no pass.
- A run whose adapter knows it ran a stale build, such as `ctest` on programs built before the
  last change, is no longer recorded as the check's pass: it says it passed and isn't recorded,
  and the next run runs it. Contract: an adapter's facts may say `staleBuild`.
- A start of a test runner that dies before it says what it saw, such as a second pytest that
  crashes, keeps every test file and test of the run from a pass. Contract: an adapter's process
  may make `<anything>.started` in `HUSHRUN_UNITS` as it begins and remove it once its facts are
  written; pytest's does.
- This machine's record of what test files read is set aside and made anew only when it is damaged
  or of another build: a read-only file, a full disk or an I/O error no longer replaces a healthy
  one, and two runs never set aside each other's.
- A pytest that a test of the run starts and stops no longer keeps every pass of the check out for
  good: only a start of the runner by COMMAND says it began.
- `hushrun why` says why a test or a test file kept no result the last time it ran here, as the
  run decided it: a skip in a run that failed (such as a Go test that skips because another
  failed), a failure no test explains, a start that died, a stale build, a test its adapter never
  leaves out. Before, it said nothing had changed and no pass counted. A stale build in a run that
  failed no longer records the passes of test files and tests either.
- A run's results of test files and tests fit a server's 8 MiB body, cut from the last as past
  50,000, and only a 413 in an older server's words sends the first 5,000 again: a server's own
  413 is said as its refusal.
- With `reads: test`, pytest's adapter follows what Python modules re-export: a package's
  `__init__.py` that binds names from another module, `from X import *`, and a module's `__all__`.
- pytest's adapter takes a SQLite database a test connects to as read, as it does a file it opens.
- A pytest fixture of a wider scope whose end fails keeps the pass of a test that asked for it at
  run time (`request.getfixturevalue`), and no longer of a test served another fixture of its name.
- pytest's adapter no longer takes a relative path opened after a `chdir` for the file of that
  name it saw before: a test that read `sub/data.py` as `data.py` from `sub/` now has it on its
  list, and runs when it changes.
- With `reads: test`, pytest's adapter takes a test to have read what a fixture it asked for at
  run time (`request.getfixturevalue`) read as it set up, and what a shared setup that started
  inside its window read; and what ran while no test's window was open, such as a
  `pytest_configure` of a `conftest.py`, is every test's.
- A process a pytest fixture of a wider scope starts as it ends, when it switches its parameter
  inside the next test's setup, is its test file's, not that one test's.
- pytest's adapter watches a test only while no other tool uses `sys.monitoring` (a debugger,
  coverage.py's sysmon core, a repository's own tool): with one there it watches nothing, and when
  one appears it stops, gives back its tool id and says why once; each test then reads its test
  file's list.
- `reads: test`'s known misses name a third: a value an earlier test computed and a cache kept,
  such as a `functools.cache` wrapper, which a later test reads without its code running. The
  audit's second run catches it, as it does the other two.
- A `ctest`, `swift test` or wrapper COMMAND that CTest's or `swift test`'s adapter now takes,
  which pytest's adapter took before, stays pytest's where this machine's store says pytest
  reported its test files. Where it doesn't, such as on a CI runner whose cache doesn't hold the
  check, CTest or `swift test` observes it, and what pytest ran under it is no longer observed.
  A run so kept forgets pytest only when its command passed while pytest reported nothing: one that
  failed before pytest started, such as `ctest` without its build directory, keeps it.
- Jest's adapter fails every test under a failing `afterAll`, which jest-circus doesn't, so a file
  with another failing test no longer keeps those tests' passes and leaves them out next time,
  their `afterAll` with them. After an uncaught exception or an unhandled rejection, which Jest 29
  charges to whatever test is running, no test of its file passes.
- With `reads: test`, Jest's adapter keeps a file opened with every repository file on the stack,
  the test file included: a cache one module above the one that read it, or in the test file,
  reaches each later test that reads it.
- With `reads: test`, a test that reads through a chain of barrels (`export * from`, a CommonJS
  `module.exports = require(…)`) lists every barrel of it, for Jest and Vitest: their adapters send
  `reexports`.
- With `unit: test`, the Go runner passes a test binary's output on until every program the test
  left running lets it go, as `go test` does, instead of stopping 5 seconds after the binary
  exited; such a package's tests still record no pass.
- Jest, Vitest and Go say which tests were skipped (`test.skip`, `describe.skip`, `test.todo`,
  `test.skipIf`, `ctx.skip()`, and Go's `t.Skip()` under `-v` or `-json`) and which start of the
  runner wrote each file, so a skip in a run that failed records nothing. A Jest test file that
  two projects ran ran whole only when each of them finished it.
- Jest and Vitest started more than once in one COMMAND, as a script's `jest a && jest b` does,
  no longer keep the first start's passes when the second fails: the second start leaves a
  `.started` file, no test file or test of that run records a pass, and the run says why once. A
  `.started` file whose content is a JSON object with a `why` gives the reason the run says.
- With `unit: test`, a Go package that failed without `-v` or `-json` keeps no pass of its other
  tests: Go names only what failed, so a test that skipped because another failed looked passed.
  The run says so once, and `hushrun why` says it for such a test; add `-v` to keep them.
- `hushrun why` gives a test that kept no result the reason its run recorded with it in this
  machine's store, rather than one guessed from COMMAND: a Go test that a panic or a timeout left
  unrun is told so, not to add `-v`, and a `-v` that comes only from `GOFLAGS` is no longer asked
  for. An adapter's facts may say a test's `notKept`; the Go runner sends it.
- Go with `unit: test`: a package whose test leaves a program holding its output past `go test`'s
  own wait now fails with `Test I/O incomplete`, as without hushrun. The runner hands output still
  open after the binary exits to a forwarder and exits, rather than passing it on to its end
  itself, which made `go test` print `ok`.
- Maven and Gradle no longer record as passed a test TestNG skipped because another class's suite
  setup failed: a skipped test, and a class skipped whole, is told skipped, and passes only in a
  run where nothing failed.
- A change to an annotation processor of the build, or to any project a module or project names
  only to its build (`annotationProcessorPaths`, plugin dependencies, `annotationProcessor`,
  `kapt`, `ksp`, `compileOnly`), now runs the tests of the code it generates, under Maven and
  Gradle.
- Gradle: a class whose test JVM died (a crash, `System.exit`, a kill) records no pass. The
  adapter now reads the test-JVM hook's events and the task's failure.
- The test-JVM hook matches a listed test by its whole name: Spock features, parametrized display
  names and Kotest names that differ after a `[` are no longer all left out for one entry; and
  TestNG's interceptor leaves out an inherited test by the subclass it runs in. A test the hook
  left out that its list didn't name makes the run start COMMAND once more with nothing left out.
- Gradle leaves out no JUnit 4 test of a class in the default package by a filter pattern, which
  also left out the test of that name in every class of the same simple name.
- Gradle: a file of a JVM kind that no project of the build compiles, such as a tool beside a
  project's sources that a script builds and publishes to `mavenLocal()`, is now the rest, so a
  change to it runs every test class: a project is read as its source directories, not its whole
  directory. Maven read modules so already.
- Gradle: a test that ends its JVM with `System.exit(0)` under JUnit 4 or TestNG, which Gradle
  calls a success, no longer has its class recorded as passed: the test-JVM hook now also goes on
  each test JVM as a Java agent that says how the JVM ended.
- Maven: a test that reads another module's test classes through a third module's compile-scope
  test-jar dependency now runs when those test classes change.
- Maven: a module another module uses only through a plugin's configuration, such as the
  dependency plugin's `copy` of its jar, is now read by that module's tests.
- Gradle: a test-classpath jar from `mavenLocal()` is no longer dropped silently: where a project
  of the build published it, the class reads that project; otherwise the class is not observed,
  and says why.
- A Maven run of TestNG no longer counts each test twice in its summary: TestNG's own copies of
  Surefire's report, below `surefire-reports/junitreports/`, aren't read beside it.
- A `gradlew` script that starts Gradle again after a failure no longer has its second start's
  pass replace the first start's failure: each start keeps its own facts.
- `swift test --skip-build` is no longer observed: it runs an earlier build's products, whose
  passes hushrun recorded for the files as they were.
- `swift test`: a target built with a build-tool plugin of another package reads its package
  whole; a console `passed` line counts only after the test's `started` line; a list after the
  run that fails, where tests were left out, runs the command again with nothing left out;
  XCTSkip and Swift Testing's disabled tests are told as skipped (under `--parallel`, in a run
  that failed, an XCTest pass may be one); each hushrun `swift` is a start of its own; and the
  plan's `swift package describe` gets a command's own `env NAME=value` assignments.
- hushrun's `swift` no longer waits for output a program `swift test` left running holds open: it
  returns with `swift test`, a forwarder of its own passes on what that program prints later, and
  that run records no pass.
- `swift test`: an executable target is read by every test target, which may start it; hushrun's
  `swift` stops waiting a second after swift ends whatever output keeps arriving, and keeps at most
  16 MiB of it.
- CTest: a header a program of the project writes (a generator built and run by the build) now
  makes each test program that includes it read what the generator is built from, on Ninja and
  Makefiles, so a change to a header the generator includes runs those programs.
- CTest: a program or file a test names in its arguments or its `ENVIRONMENT`
  (`add_test(NAME x COMMAND driver $<TARGET_FILE:server>)`) is read by its test program; a
  directory of a build named there makes it read every source and not up to date.
- CTest: each `ctest` of a command is a start of its own, so two of them never add up to a test
  program that ran whole.
- CTest: a test preset's `execution.repeat` of `until-pass` no longer records a pass that came
  after a failure, its `filter.include.index` leaves nothing out, and its `execution.showOnly`
  is not observed.
- CTest: hushrun's own listings no longer overwrite a test preset's `outputLogFile` or
  `outputJUnitFile`.
- CTest: an in-source build (`cmake -B .`) is said not to be observed, rather than silently
  reusing nothing.
- CTest: a test whose program isn't built here is marked so whichever test of its unit comes
  first; skipped and disabled tests are told as skipped, so in a run that failed they record no
  pass; and the plan's listing and walk get a command's own `env NAME=value` assignments.
- CTest: a run in which a test of a program older than what it is built from ran (a source
  changed, nothing built) no longer records the check's pass: it says once that the pass isn't
  recorded, naming the program, what changed, and to build before ctest, and the next run on the
  same code runs again.
- CTest: a custom command's `DEPFILE`, the programs of the fixtures and tests a test depends on, a
  program a test program waits for or has compiled in, and a program a test names by a relative
  path are now read; on Makefiles a source newer than its object makes the program stale; an
  in-source build says so in the run; and for `ctest` as the command the project's discovery runs
  once less, the plan's listing being taken.
- CTest and `swift test`: a file of a source kind that the build's graph gives to no test program
  or test target (a target no test depends on, `cases/*.c` a test reads at run time, a
  `Fixtures/` tree) is in the rest set, so a change to it runs everything. A program whose build
  couldn't be read no longer makes every C and C++ file a source.
- On Windows, hushrun's `ctest` and `swift` end with the real program's exit code whole, where
  one past 255 used to end as 1.
- On Windows, hushrun's test runner ends with a Cargo or Go test binary's exit code whole, as
  its `ctest` and `swift` do, where one past 255 used to end as 1.
- hushrun's `ctest` and `swift` end on a signal sent after the real program ended, rather than
  ignoring it.
- The real `ctest` or `swift` hushrun starts is the one a shell would: an empty `PATH` entry is
  the current directory, a relative one is from it, and a link to one of hushrun's own programs
  is passed over.

## [0.13.0] - 2026-11-12

Test by test ([ADR 0068](docs/adr/0068-test-files-observed.md) to
[ADR 0074](docs/adr/0074-test-files-on-the-dashboard-and-milestone-4s-loose-ends.md)): inside a
check that has to run, the test files that already passed on the files they read aren't run again.
It is off unless a check asks for it, with `tests: observe` (every test file runs, and hushrun says
what reuse would have done) or `tests: reuse`. Observed for pytest, Cargo (`cargo nextest`, `cargo
test`), `go test` and Vitest; audited as it goes; and explained, one test file at a time, by
`hushrun why`.

### Added

- Contract: **`checks.<name>.tests`** in `hushrun.yaml`: `off` (the default, as if absent),
  `observe` or `reuse`, or `{mode, ignore, always}`. Not part of the check's definition: turning it
  on keeps every key and every piece of evidence, and with it off nothing changes at all.
- Contract: **`observe`**: every test file runs as before; hushrun records what each read and how it
  ended, and says before its last line ``with `tests: reuse`, 12 of 340 test files would have run;
  328 already passed on the files they read``, or which changed file would have run them all. A
  test file that would have been reused and failed is said, and kept in the audit. Nothing changes
  COMMAND's outcome or exit code.
- Contract: **`reuse`**: before COMMAND, `hushrun: unit: 12 of 340 test files run; 328 already
  passed on the files they read, and 7 more that did run as a check`, or why every test file runs;
  the last line adds `; reusing 328 test files saved about 3m 05s`. A run that passed with reused
  test files is a pass, and the same files skip the whole check next time. COMMAND stays byte for
  byte as written.
- Contract: **audit runs**: 2% of the test files that could be reused, at least one, run anyway. One
  that fails runs once more: passing then, it may be flaky; failing again, the check's reuse is
  suspended for the repository until a person runs **`hushrun tests resume NAME`**, which a coding
  agent can't.
- Contract: **the adapters**, each keeping COMMAND as written: pytest through a plugin hushrun loads
  with `PYTHONPATH` and `PYTEST_ADDOPTS` (xdist included); Cargo through a target runner chained to
  one the environment sets, and the build's dep-info; `go test` through `-exec` added to `GOFLAGS`,
  `go list -deps -test` and Go's own test log, with Go's test cache off for the check; Vitest 4
  through one line in the repository's Vitest config,
  `plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())]`,
  which `hushrun init` proposes under "Change by hand" and never writes. A test file whose reads an
  adapter can't follow (a process it started, a fork, native code, a `TestMain`, a worker thread, a
  plugin hushrun doesn't know, …) is `not observed` and always runs. Any other COMMAND gets the
  pytest adapter, which does nothing where pytest never starts, and says nothing of test files
  until pytest has reported them.
- Contract: **`hushrun why NAME UNIT [-- COMMAND]`**: what a run of the check would do with one test
  file, for the files on disk now, and why, in one sentence: `tests/test_cart.py runs: it read
  shop/pricing.py, which changed`, with the imports between them when the adapter saw them; or
  that it would be reused (and that an audit may still run it), was never seen run here, wasn't
  observed, is named by `tests.always`, failed last time, is suspended, or isn't a test file of the
  check. It says when the answer is as far as this machine knows, writes nothing, and has
  `--format json` (`hushrun.why/v1`).
- Contract: **`hushrun ready`** and the pre-push hook add, for a check with `tests: reuse` it would
  run, `unit: CI would run 12 of 340 test files; 328 already passed on the files they read` (with
  results on this machine, what a run here would), or that its reuse is suspended: for pytest and
  Vitest, whose plans read files, with one read-only batch lookup.
- Contract: **`units`** in a run's result, wherever the result goes, a server included:
  `{total, ran, reused}`, `ranAnyway` when nothing could be reused (`force`, `reports`,
  `suspended`, …), and `savedMs`, the time the reused test files took when they last ran.
  **`hushrun-results/v0`** gains `POST {base}/v0/runs/{key}` (a run with its test files' results,
  by key, never a name or a path), `POST {base}/v0/units/lookup` and `POST {base}/v0/units/reuse`.
  A server without them records the run alone, and nothing is reused, said once. A suspension that
  can't reach the server waits on this machine, and nothing is reused until it gets there.
- Contract: **`hushrun repository stats`** counts, beside what skips saved, the test files this
  machine's runs ran and reused and the time reuse saved (`testFiles` in JSON).
- Contract: **`HUSHRUN_UNITS`**, the directory where an adapter writes what it saw
  (`hushrun.units/v0`, `hushrun.unit-facts/v0`): what to deselect, pytest's `pythonFiles`, the
  reused binaries, Vitest's `testFiles`; per process, each test file's outcome, reads, listed
  directories, and the direct edges between files it saw.
- This machine's store of what test files read: one SQLite file per repository in the cache
  directory, `units/<id>.v1.sqlite`, holding each check's map, audit and edges, written in one
  transaction per run and forgotten 30 days after a run last wrote it. One that can't be read costs
  one full run and one line, and is made again. A cache step that keeps it between CI runs saves
  `<cache dir>/units/*.v1.sqlite`. SQLite is built into hushrun (`rusqlite`, bundled).
- The unit key's vectors, `testdata/digest/unit-keys.json`, and `tools/refdigest.py unit-keys`.
- **A machine with an empty cache reuses what a warm one would**
  ([ADR 0075](docs/adr/0075-what-other-machines-saw-test-files-read.md)): a CI runner or an agent's
  sandbox asks the server for what other machines saw its test files read, keys each test file by
  this machine's list and theirs, newest first, three at most, and reuses it when some list's key
  has a pass that counts here and none has a fail. What a run saw goes back to the server once it
  is recorded, in `observe` too, and not again while it is the same. Every file path in it is a
  keyed hash under a key the server never has, derived from the names of the check's sources.
  Lists are kept per system (Linux, macOS, Windows), and a check whose `counts_from` names `os`
  reads those systems'. A test file reused on another machine's list is kept in this machine's
  store as `shared` until it runs here, and `hushrun why` says `another machine saw it read …`.
  Never in local mode, for Cargo, or for a run that recorded nothing. Contract:
  **`hushrun-results/v0`** gains `PUT {base}/v0/units/hints/{sha256}` and `POST
  {base}/v0/units/hints/lookup`; a server without them changes nothing, said once,
  `hushrun: unit: acme doesn't keep what other machines saw its test files read yet, so this
  machine goes by what it saw itself`. One too large to send isn't sent, said once. A check whose
  `counts_from` names this machine's system among others reads this system's lists only, and a
  list borrowed from another system keys nothing once no rule reads it, and is never sent as this
  system's. With a backend that keeps none, `hushrun init` says under "Change by hand" the cache
  step a CI job needs to keep what its test files read between runs, for GitHub Actions and
  GitLab, and edits no CI file for it.

### Changed

- In CI where a later step reads a check's reports, no test file is reused, as no check is
  skipped; a summary of a run with reused test files leaves its coverage out.

## [0.12.0] - 2026-11-11

Results stay on this machine until someone shares them ([ADR 0067](docs/adr/0067-results-stay-on-this-machine-until-someone-shares-them.md)):
hushrun works with no account, and `ready` and the pre-push hook go by what passed here.

### Added

- Contract: **`hushrun init --local`** sets a repository up for this machine only: it wraps the
  checks, writes the agent skill and AGENTS.md, installs the hook and the package, and signs in to
  nothing, names no backend, gives CI's jobs no ID token and makes no machine key. `--dry-run`
  shows that plan. A plain `hushrun init` later signs in and adds what sharing needs, wrapping
  nothing twice.
- Contract: **`hushrun.ready/v2` has `results`**, `shared`, `local` or `off`, and
  **`hushrun.whoami/v1` has `signed_out`**, the backend a repository names that nobody here is
  signed in to. Both are optional.
- At a terminal with no login, `hushrun init` asks first whether to share results with CI and
  teammates, which signs in, or keep them on this machine. `--yes`, `--dry-run` and `--org` share,
  as before.
- `hushrun run` forgets this machine's results and skips 30 days after their run, sweeping its
  cache directory at most once a day.

### Changed

- Contract: **in CI, results never stay on the runner.** With no server named, results are off,
  and `hushrun run` says so in one line: a cached directory of unsigned results shared between
  jobs could be forged. A repository whose files name a backend is unaffected.
- **A repository's backend with no credential here keeps results on this machine.** With no
  login, no token and no ID token, outside CI, a run asks the server nothing, skips what passed
  here, and still says `sign in to share results with <org>: npx hushrun login` at a terminal.
- `hushrun ready` and the pre-push hook work with results on this machine: each check passed here,
  failed here or has no pass here yet, an agent's push waits for them, and a note says that CI
  still runs every check and what shares results with it.
- `hushrun repository stats` counts this machine's results where they stay here and no repository
  URL is set, instead of failing with `no repository URL`.
- The block `hushrun init` writes in AGENTS.md holds with or without shared results: hushrun skips
  a check "on this machine, and in CI once the repository shares its results", where it said "in
  CI". init replaces an older block where it is.
- **The skill is revision 2.** It says that a pass skips here, and in CI once the repository
  shares its results, has a section on results that stay on this machine, tells a repository in
  another language by its `hushrun.yaml` or by CI's `hushrun run` lines, and leaves setting up
  with no account to `hushrun guide`. `hushrun ready` notes a repository's copy of revision 1 as
  older, and `hushrun skill update` or `hushrun init` updates it, to commit.

### Fixed

- A coverage report whose counts don't fit together no longer wraps them, or stops a debug build:
  the statements of a Go coverprofile and a Cobertura report's branches stop at the largest count
  there is. A fuzz run in CI found it.

## [0.11.1] - 2026-11-10

### Fixed

- The skill's marketplace bundle has Cursor's manifest, `.cursor-plugin/plugin.json`, and its
  README gives the privacy policy and a support contact, which Kiro's registry asks for.
- A fix or feature under `recipes/` now counts toward the next release, since the binary embeds
  the skill and the guide, and the marketplace bundle is built from them.

## [0.11.0] - 2026-11-09

Coding agents learn what is out of date ([ADR 0065](docs/adr/0065-agents-learn-what-is-out-of-date.md)):
hushrun itself, and the copy of its skill they read.

### Added

- Contract: **`hushrun guide`**, and `hushrun.guide/v1` with `--format json`: what a coding agent
  needs to know about this hushrun beyond its skill, from the binary, then a newer release the
  daily check learned of and each copy of the skill: `current`, `older`, `newer`, `edited` or
  `absent`. It asks nobody, and changes nothing.
- Contract: **`hushrun skill update`**, which writes this hushrun's skill into the repository, as
  init does, or with `--global` into `~/.claude/skills/hushrun` and `~/.agents/skills/hushrun`. It
  leaves a copy edited by hand, or written by a newer hushrun, unless `--force`.
- Contract: **the skill's revision**, `metadata.hushrun-skill` in its frontmatter, which goes up
  only when its text changes.
- `hushrun ready` notes when the repository's skill is older than this hushrun's, or a newer
  hushrun wrote it, and `hushrun doctor` checks each copy.

### Changed

- Contract: **the newer-release notice reaches coding agents**, without a terminal, and in a
  sandbox that sets `CI` with an agent in it: `hushrun: hushrun <newest> is available, and this
  is <this>: when you report back, tell the person, …`. Once a day, as for people.
- The skill holds what every hushrun does the same way, and points to `hushrun guide` for the
  rest. Its frontmatter no longer says that init wrote it.
- `hushrun init` no longer takes a copy of the skill that a newer hushrun wrote back to its own.
- The `hushrun` npm package carries the skill, at `skills/hushrun/SKILL.md`, where tools that link
  skills from installed packages look.
- Each release writes the skill's bundle for marketplaces to github.com/hushrun/skills
  ([ADR 0066](docs/adr/0066-the-skill-goes-to-marketplaces-from-a-repository-the-release-writes.md)):
  skills.sh, Claude Code, Codex, GitHub Copilot, Gemini CLI and `gh skill` install from there.

## [0.10.1] - 2026-11-09

### Security

- Update gix to 0.90.0 for [GHSA-q54r-jf2p-rfr2](https://github.com/advisories/GHSA-q54r-jf2p-rfr2).
  hushrun only reads git objects and never reached the affected checkout, index, merge or
  tree-editing code.

## [0.10.0] - 2026-11-09

### Added

- Open-source the hushrun CLI under the MIT license.

## [0.9.0] - 2026-11-08

hushrun 0.9.0 has no stages ([ADR 0062](docs/adr/0062-no-stages.md)): a repository has one
pipeline, and nothing moves a change from one environment to the next. A breaking release, and
hushrun's server ships first. `hushrun run NAME -- COMMAND`, `ready`, the pre-push hook, `init`
and sign-in work as before.

### Removed

- Contract: **stages.** `--stage` on every command; `stages:` in hushrun.yaml, with `after`,
  `upstream`, `upstream_grace`, `ci.github.workflow` and `ci.gitlab.ref`; `<check>@<stage>`
  requirements; and checks of kind `deployment`. A check that ran against a stage's deployment
  now runs in the deploy step, or is attested before `mark passed`.
- Contract: **approvals.** `hushrun approve` and checks of kind `approval`. Approval events older
  releases wrote stay in the repository's ledger, and count for nothing.
- Contract: **the plan mode `wait`**, with upstream runs and the CI API calls that looked for
  them. A plan says `full`, `refresh`, `promote`, `current` or `blocked`.
- Contract: **copying between stages.** An artifact has one `location`; `artifact resolve`
  deploys the build the plan reuses, by digest, and copies nothing. `artifact copy` stays.
- **The per-stage CI-only lists**, an org's and a repository's. The repository's one switch,
  "CI reuses only CI's results", now also makes its deploys count only its CI's events. hushrun's
  server merged the lists into each repository's periods, so nothing became less strict.
- `adopt`'s two workflows (`deploy-staging.yml`, `deploy-production.yml`), and the `stage` input
  of the reusable workflow `hushrun-deploy.yml`.

### Changed

- Contract: **`deploy:` in hushrun.yaml** holds what a stage's own settings held: `branch`,
  `require`, `lease`, `dedupe`, `allow_force` and `on_repository_error`. Without `require`, a
  deploy requires every check with a `run`, and `run --all`, `ready`, `plan` and `status` read
  that one list. `counts_from` and a scope's `external` are refused in a file with `deploy:`,
  where they were refused in a file with stages.
- Contract: **an older hushrun.yaml keeps skipping.** `stages`, `checks.<name>.kind` and
  `artifacts.<name>.locations` are read past, and each command names them once, with their line:
  ``warning: hushrun.yaml:14:1: `stages` is ignored since hushrun 0.9.0, which has no stages:
  delete it; a stage's `branch`, `require` and `lease` go under `deploy` ``. `run`, `ready` and
  the hook go on as before, and such a file no longer loses its `backend:` or `repository_url`.
  `plan`, and `artifact resolve` when it plans, refuse it (exit 2) until those keys are gone: a
  deploy that ignored `stages:` would skip the gates its author wrote. `hushrun init` removes each
  check's `kind`, and names `stages` and `locations` for you to move.
- Contract: **`--stage` and `hushrun approve` stop and say so.** Both are hidden, run nothing and
  exit 2, with `hushrun 0.9 has no stages` or `hushrun 0.9 has no approvals`, until 0.10.
- Contract: **outputs that named a stage move to `/v2`**: `hushrun.run`, `hushrun.plan`,
  `hushrun.status`, `hushrun.ready`, `hushrun.policy.explain`, `hushrun.verify`,
  `hushrun.repository.stats`, `hushrun.artifact.resolve` and `hushrun.adopt`. The plan file is
  `hushrun.plan/v2`, so every job of a run stays on one release.
- Contract: **events name no stage.** A deployment is indexed at `v1/deployed/<scope>/_/`. The
  first 0.9 plan doesn't see a deployment 0.8 recorded at a stage, and deploys once more. The
  signers answer's `ci_only` is one list of periods; an older server's per-stage list reads as
  the union of its periods.
- **`hushrun run CHECK…`**, with no command after `--`, runs hushrun.yaml's checks, `--isolated`
  included, and records their evidence: the named ones, `--all`, or the plan's checks to run.
- **`init --pipeline` takes `--branch` and `--image`**, and writes one `deploy` block. **`adopt`
  takes `--branch` and `--image [ARTIFACT=]REPOSITORY`**, and writes one workflow,
  `.github/workflows/hushrun.yml`, a name that leaves a team's own `deploy.yml` alone. `guard
  --branch` defaults to `deploy.branch`.
- `plan` no longer asks GitHub or GitLab about runs: the caller workflow needs no `actions: read`,
  and `GITHUB_TOKEN` goes unread. `HUSHRUN_GITLAB_TOKEN` serves only `doctor`'s checks of the
  protected branch and of who may set pipeline variables.
- `status` shows one decision per scope; `repository ls`, `signers` and `stats`, `policy
  explain` and `verify` drop their stage column.
- **`repository gc` keeps the newest deployment of every segment the deployed index lists**, a
  stage's from 0.8 or `_`, and the events of its digest.
- A pass a machine or an agent signs keeps its signature and key id on hushrun's server, so a
  check's history shows who signed it after the machine is gone.
- The deploy docs moved from hushrun.com/docs/promotion to /docs/deploy, with redirects.

## [0.8.1] - 2026-11-08

### Changed

- Contract: **`hushrun run --stage` without `--isolated` runs only on the content its evidence
  is for.** It exits 2 before anything runs, and records nothing, when a scope's file at the
  revision (`HEAD`, or `--rev`) is changed, deleted or of another kind in the working tree, or when
  a `--digest` isn't the scope's digest here. Files the revision doesn't have are left alone, so a
  job's own files (credentials an action writes, say) don't count. `--worktree` records for the
  files as they are. A plan file is carried out only where each of its scopes still digests to the
  plan's digest. `hushrun run NAME --rev X -- COMMAND` keeps taking your word that the command
  tests X (ADR 0060).
- Contract: **in CI, results never send a person's saved login.** A personal `HUSHRUN_TOKEN` in CI
  signs only with the key the job names, `HUSHRUN_MACHINE_KEY` or `HUSHRUN_MACHINE_KEY_FILE`;
  without one the result isn't sent, and the run says what to set. The ID token is unchanged.
- Contract: **one CI detector decides everywhere.** It now also knows Jenkins (`JENKINS_URL` and
  `BUILD_ID`), Azure Pipelines (`TF_BUILD`), TeamCity, CodeBuild, Travis CI, Bitbucket Pipelines,
  Drone, AppVeyor and Semaphore, which earlier needed `CI=true`. `HUSHRUN_PROVIDER` now also decides
  whether a run is a fork's, which job asks and as whom.
- **`npm install` and `hushrun init` keep the pre-push hook's mode**, `--refuse` or `--warn`. Only
  `hushrun hook install` changes it. init leaves out a hook that `hushrun hook uninstall` removed
  or `hushrun.hook` turns off (ADR 0061).
- **`hushrun ready` and the pre-push hook register no machine.** The run they ask for does.
- When package.json and hushrun.yaml name different backends, hushrun says so once a command;
  package.json still decides.

### Fixed

- A failure here outranks an earlier pass that nothing recorded: `ready` and the hook ask for the
  check again, where they said "passed, not recorded" and let an agent push.
- `plan` keeps a queued upstream run pending until an event that counts at the stage names it, and
  numbers attempts by the events that count there. `mark passed` judges P-6's 60 s rule by the time
  it signs with, not the command's start. `on_repository_error: full` still blocks a run on the
  wrong branch.
- The deployed index is read past entries that don't count at the stage, so promotion's dedupe
  and GC see the deploy that does, and GC keeps the entry the planner reads. `mark deployed`
  completes a deploy whose index write failed instead of writing a second pair.
- A sign-in waits out a busy or unreachable host and ends when its code expires, `slow_down`
  included. `hushrun login` keeps the previous backend when the new org refuses access. init signs
  in again when the backend no longer takes the saved login, where it went on as a member.
- The credentials file is changed under a lock, the Sigstore trust root and `monitor --state` are
  written whole, and `monitor` keeps a remembered checkpoint it can't check instead of starting
  over.
- A Windows update that can't put `hushrun.exe` back says where the old one is. The newer-release
  notice tells an npm install to update the package.
- The reports search says it stopped short when a reader names `node_modules`, `.venv`, `venv`,
  `__pycache__` or `.git`.
- The log client never moves a command's newest checkpoint back, and checks answers that came
  before the log key in the order they came.

## [0.8.0] - 2026-11-06

### Added

- Contract: **a `# hushrun: leave` comment keeps `hushrun init` off a line of CI's files**, with
  the reason after it if you like: `# hushrun: leave, public-good Sigstore changes while the code
  doesn't`. Above a step, or at the end of its `run:` line, it leaves the step's commands; above a
  GitLab script item, that item's; and a comment line right above one command of a `run: |` block,
  that command. init lists each under what it leaves, with your reason, wraps and suggests none of
  them, and gives their job nothing for them. Every init after that leaves them too, where before
  init applied its whole plan or nothing. An older hushrun reads it as any comment.
- Contract: **a pull request from a fork skips what its repository's CI passed**, where the
  backend lets it: in GitHub Actions with no credential, which is what GitHub gives a fork's job,
  a lookup asks as the repository's CI, `?as=ci&repository=github.com/<GITHUB_REPOSITORY>`, with
  no token, on github.com only. hushrun's hosted backend answers it for a public repository of its
  open-source program, from what that repository's own CI recorded, so a pull request that changes
  one package skips the checks of every other. Such a run records nothing: it sends no `POST`, a
  skip's either, and ends with `hushrun: results aren't shared with acme: GitHub gives a pull
  request from a fork no ID token, so it records nothing`; a job of the repository itself without
  `id-token: write` is told to add it. A backend that refuses the lookup gets what it got before,
  and the checks run. Outside GitHub Actions, nothing changes: a laptop signs in.
- Contract: **`hushrun init --dry-run --format json`** (or `HUSHRUN_FORMAT=json`) prints init's plan
  as one JSON document, `hushrun.init-plan/v1`, in place of its text, for scripts, reviews and
  agents. It lists the scopes, package.json's scripts, each CI job that runs a check with each
  command init wraps, leaves or finds through hushrun already and the steps that read reports, the
  files init would write as unified diffs that `git apply` takes, and its notes. For each wrapped
  command and script, `ci` says what `hushrun run` will decide in that job once the job has a pass:
  `skips`, or `runs` with the reason, in the run's own words when they can be known before it runs,
  such as a job with several variants. The dry run still signs in to nothing, writes nothing,
  installs nothing and sends nothing, and the text of `hushrun init --dry-run` is as it was. Without
  `--dry-run`, `--format json` changes nothing.
- Contract: **`HUSHRUN_REPORTS_READ`**, a variable for a CI job that reads the reports its checks
  write: `true` makes every check of the job run on a pass, on any CI, since a skip writes no
  report. `false`, or unset, adds nothing. A value hushrun can't read is ignored, with a line. An
  older hushrun doesn't read it, and skips.
- Contract: **`checks.<name>.reports`** in `hushrun.yaml`: `ci: skip` says the repository's
  readers handle a skip, so CI skips the check on a pass as before, and `files` lists report files
  the check writes, up to 32 globs. Neither changes the check's key. A hushrun from before refuses
  a `hushrun.yaml` with it, as it does any key it doesn't know.
- Contract: **a run's result says why it ran although a pass would have skipped it**,
  `ranAnyway`: `variants`, `reusable`, `reports`, `rule`, `force`, `inside-run` or `cant-tell`,
  only when a pass of its key was there to skip on. With `--force`, and in a job whose variants or
  calls hushrun can't tell apart, a run now asks for that pass first, one lookup. The dashboard
  counts these runs, so you can see what could have skipped and why. A result without it reads
  as before, and a newer word reads as one more reason.
- **A run whose verdict isn't the one that decided it says it may be flaky**:
  `hushrun: unit failed in 12s (exit 1); it passed on this exact code before, so it may be flaky`,
  or `; it failed on this exact code before, so it may be flaky` after a pass. On GitHub the step
  summary says the same. Only a result that counts in the job decides: another job's pass says
  nothing of flakiness.
- **`hushrun doctor` names a merge queue's run**: in a `merge_group` run on GitHub, the pull
  request it tests and the branch it merges into. A queue's run of a pull request's tree skips on
  that pull request's pass in the same job, as before; the GitHub guide has a "Merge queues"
  section.
- Contract: **a run's result keeps the summary of its test and coverage reports**, `reports`:
  counts, and each report file's path and format, at most 4 KiB, never a test's name, a message or
  a file's contents. A skip's is the summary of the pass it stood in for. `hushrun ready --format
  json` gives a passed check's as `reports`. `filesComplete` says it names every report file the
  run wrote, as far as the job's readers read; a run that wrote none records just that. An older
  hushrun reads these results as before.
- Contract: **`HUSHRUN_VARIANT`, `HUSHRUN_WORKFLOW` and `HUSHRUN_INPUTS`**, set in a GitHub Actions
  job's own `env`, as `hushrun init` writes them: `${{ toJSON(matrix) }}` says which variant of a
  job with several runs, a called workflow's own path says which file its job is in, and
  `${{ toJSON(inputs) }}` which call it is. A run trusts each only when the workflow's file sets it
  to that very value. An older hushrun doesn't read them, and runs every variant as before.
- Contract: **a result's `variant`**, `{"key", "label"}`, beside `job`, for a run in a job that
  says which of its variants it is: the key tells the variant apart, and the label shows the
  matrix's values for people, never a call's inputs. In CI, the lookup asks the server for the
  newest result that counts in this job and variant (`?job=`, `&variant=`). A server from before
  ignores the query; hushrun then decides on the newest result as before, and skips less. An older
  hushrun reads these results as before.
- Contract: **a command of init's plan says the composite action it is in**, `action`, the action's
  file, when it isn't the job's own; its `line` is then that file's. A plan without one is read as
  before.
- Contract: **a script of a job in init's plan says its package**, `package`, its package.json from
  the root, when it is a workspace package's: the plan lists the packages' scripts each job starts,
  and package.json's scripts list theirs, each by its package.json, as `package` always said. A
  plan without one is read as before.
- **Each variant of a job skips on its own pass.** `hushrun init` writes
  `HUSHRUN_VARIANT: ${{ toJSON(matrix) }}` in a job with several variants, and wraps its lines and
  scripts, so that CI runs each variant until that variant passes on this code, for a matrix an
  expression builds too. A pass from a laptop or an agent still never skips a variant. On GitLab,
  each `parallel` instance and `parallel:matrix` variant counts its own passes, with nothing to
  add.
- **Reusable workflows skip in CI.** init reads a workflow of this repository that a running one
  calls with `uses: ./.github/workflows/<file>`, wraps its checks, writes `HUSHRUN_WORKFLOW` and,
  when it takes inputs, `HUSHRUN_INPUTS` in its jobs, and gives the calling job `id-token: write`.
  Each call then skips on its own pass, and the inputs show nowhere.
- **A line whose words CI fills in skips in CI.** init wraps `pytest ${{ matrix.args }}`,
  `${{ github.sha }}`, or `$PKGS` set in the job's `env`, when nothing in them may hold a secret,
  and each value has passes of its own. A line that sets variables first, `GOARCH=386 go test
  ./...`, goes through `env`, `hushrun run <name> -- env GOARCH=386 go test ./...`, in a bash step,
  so that each value of the variable has its own passes too. A secret, `github.token`, a variable
  set from either or that an earlier step writes from a command's output, or `$(…)`, leaves the
  line, and the plan says why. `hushrun ready` keeps such a check as `ci`, since only the run can
  tell its key.
- **A workspace package is a scope.** In a JavaScript workspace, whose packages
  `pnpm-workspace.yaml`, `package.json`'s `workspaces` (npm, Yarn and Bun) or `lerna.json` list,
  `hushrun init` reads each package's scripts and wraps them in its own package.json, with its
  scope: `"test": "hushrun run --scope acme-web -- vitest run"`. A package's scope holds its
  directory and those of the packages it depends on, by its dependencies, a `turbo.json` task that
  needs another package's, or a tsconfig's `references` and `extends`, so a change runs only the
  checks of the packages it reaches. The root's script that runs each package's, such as `pnpm -r
  test`, `turbo run test` or `npm test --workspaces`, stays as it is and skips package by package,
  and a CI job that runs it, or picks a package with `--filter`, gets the ID token it needs. With
  Yarn from 2 on, each wrapped package gets `hushrun` in its devDependencies, since Yarn gives a
  package's scripts only its own dependencies' programs. Turborepo 2 gives the scripts it runs
  only the variables turbo.json lists, so when CI runs a package's script through `turbo`, init
  adds `ACTIONS_ID_TOKEN_REQUEST_*`, `GITHUB_*` and `HUSHRUN_*` (GitLab's `GITLAB_CI` and `CI_*`)
  to turbo.json's `globalPassThroughEnv`, after any entries it has, keeping its comments; turbo
  passes them on without making them part of its cache keys. Where init can't read the graph, such
  as a package of the workspace named from `github:`, one scope holds the repository, and the plan
  says why. `hushrun ready` lists the packages' checks too, each run in its package's directory.
- **Checks in composite actions skip in CI.** init reads the composite actions in `.github/` that
  a job uses with `uses: ./…`, and those they use, wraps their checks in the action's file, once,
  and gives every job that runs them, a nightly's too, hushrun's setup, its ID token and which
  variant it is. Each job, and each variant, skips on its own pass. A line one of those jobs can't
  take, as when it gives the line a secret or init can't edit it, stays as it is for all, and the
  plan says where. `hushrun ready` reads the actions too.
- **`hushrun doctor`** warns of a job that sets one of those variables to anything else, since no
  pass skips it there.
- Contract: **GitHub outputs on every run**: `hushrun-result` (`pass`, `fail` or `skip`),
  `hushrun-skipped` (`true` or `false`), and with a summary `hushrun-tests`,
  `hushrun-tests-failed` and `hushrun-coverage-lines`. In a step that runs several checks, the last
  run's values win.
- **A run says what its test and coverage reports say**, and a skip what its pass's said:
  `hushrun: unit passed in 1m 21s: 1,204 tests, 14 skipped, 87.3% of lines covered; it won't run
  again on this code`, and `hushrun: unit skipped: this exact code passed in CI 3 hours ago (saves
  1m 21s): 1,204 tests, 14 skipped, 87.3% of lines covered`. hushrun reads JUnit XML and TRX, and
  LCOV, Cobertura, JaCoCo, Clover, OpenCover, Go's coverprofile, coverage.py's JSON, istanbul's
  summary and SimpleCov, telling each by its content, in the files the command line names,
  `reports.files`, the files the CI job's readers read, and the places tools write to by default,
  modified since the run started. No report file leaves the machine. On GitHub Actions a skip also
  writes a step summary with its pass's tests and coverage, and `hushrun ready` shows a passed
  check's summary after who recorded it.
- Contract: **`hushrun init` suggests the test scripts it could also wrap**, and applies none. In
  a job named for its tests, a line such as `./scripts/test.sh`, `bash scripts/ci/test.sh` or
  `./runtest` is listed under `Could also wrap`, with the line to use:
  `.github/workflows/ci.yml, job test: ./scripts/test.sh → hushrun run test -- ./scripts/test.sh`.
  A script can deploy or write reports, which a skip wouldn't do, so a person decides. The plan as
  JSON lists them in `suggestions`, each with its file, job, line, check, the line to use
  (`after`), and whether its job has hushrun (`setup`).
- **`hushrun init` knows more test runners in CI's files:** `python -m unittest`, Gradle's `build`,
  Maven's `install`, `sbt test`, `bundle exec rake` with no task, `ctest`, `meson test`,
  `dart test`, `flutter test` and `deno test`, and a Maven or Gradle task after a `clean`, such as
  `mvn clean install`. A build with another task after it, such as `./gradlew build publish`,
  stays as it is, since that task could publish. So does `ctest` when it only lists its tests or
  runs a dashboard. init also reads past a launcher's own options, as `uv run --no-sync pytest` and
  `npx --yes vitest run` have.
- **`hushrun init` wraps a `make`, `just` or `task` target by what its recipe runs.** A target
  such as `make test-ci` is a check when its recipe, its prerequisites or the targets it runs,
  three levels down, run a check init knows, such as `pytest` or `go test`, and nothing that
  changes something outside the machine, such as `docker push`, `git push` or `npm publish`.
  init names the check after the target. A target that runs a check and also does such a thing
  stays as it is, and init says what it does: `its recipe pushes an image (docker push)`. So does
  one that runs a check beside a part init can't read, with `its recipe can't be read`. A target
  whose recipe runs no check says nothing, as before. A target whose checks are all linters, such
  as one that runs `golangci-lint`, skips in CI beside a Codecov upload, as the linter itself does.

- **`hushrun init` wraps rustdoc and your own `cargo xtask` checks.** `cargo doc` and `cargo
  rustdoc` are the check `rustdoc`. `cargo xtask <task>` is a check named after its task when the
  task is `test`, `lint` or another task runner's name, or runs with `--check`, such as `cargo xtask
  codegen --check`; any other task can release or publish, so init lists it under `Could also wrap`
  with the line to use, and changes nothing for it.

### Changed

- **A pull request from a fork says why it shares no results**: `hushrun: results aren't shared
  with acme: GitHub gives a pull request from a fork no ID token, so its checks run, and record
  nothing`, where it told the contributor to sign in or give the job `id-token: write`, which a
  fork's job can't do. Its checks run, as before, unless its backend answers it without a token
  (above).
- **`turbo run`, `lerna run` and `nx run-many` at a workspace's root aren't one check any more**
  when they run packages' scripts init wraps: each package's script is wrapped instead, and the
  root's stays as it is, so a change runs only what it reaches. Where init wraps none of those
  scripts, the root's is wrapped as before.
- Contract: **a run's result hides the secrets hushrun can tell in its command.** In the result's
  `command`, a word is `[redacted]` when it is the value of a variable whose name holds `TOKEN`,
  `SECRET`, `PASSWORD`, `PASSWD`, `PASS`, `KEY`, `CREDENTIAL`, `AUTH`, `PRIVATE` or `CERT`, in any
  case, and that value has 8 bytes or more. So is a word shaped like a token: `ghp_…` and GitHub's
  others, `glpat-…`, `xoxb-…` and Slack's others, `AKIA…`, `sk-…`, or a PEM block. In a word with an
  `=`, only what follows it is redacted: `DEPLOY_TOKEN=[redacted]`. The run's key still comes from
  the command as given, so what skips doesn't change: only what the dashboard shows does. A secret
  that no variable names and no shape shows still goes as written, so keep secrets out of commands.
- **Two CI jobs that run one check both skip on a re-run.** Each finds its own pass, which the
  other's no longer hides.
- In a job with several variants, a run that can't skip says what the job lacks:
  `hushrun: test running: job test of .github/workflows/ci.yml has several variants, and its
  HUSHRUN_VARIANT isn't ${{ toJSON(matrix) }}, so no pass skips it`, in place of the advice to name
  each variant. A name per variant still works, and with both, the variable decides. A pass that
  doesn't count says where it came from: another variant of this job, outside CI, or another job.
- **`hushrun ready`** says `CI runs each variant until that variant passes on this code` for a
  check in a job that says which variant runs, and what to set otherwise; it and **`hushrun doctor`**
  read the workflows a running one calls, as init does.
- init's last line says that each variant of a job with several skips only on its own pass, and the
  plan as JSON says `variantIdentity: env` for a job that sets `HUSHRUN_VARIANT`, and `named` for
  GitLab's instances. A command in a workflow file that doesn't read as YAML stays as it is, saying
  why.
- A result the server refuses as invalid (400 `invalid`) ends its run's line with `; not
  recorded: <what the server said>`, as the other refusals do, and the exit code is the command's.
- **In CI, a check runs when a later step of its job reads its reports**, and says why:
  `hushrun: unit running: a later step reads its reports (codecov/codecov-action), and a skip
  writes none`. On GitHub Actions, hushrun reads the workflow file it runs in and finds, in the
  job, a known reader (Codecov, Coveralls, test reporters, SonarSource, SARIF uploads and others),
  an artifact upload of a report, an action whose input names a report file, a command that reads
  reports such as `coverage xml` or `cargo llvm-cov report`, or a local composite action with one.
  A step that runs only on failure doesn't count, and a linter still skips beside readers of
  coverage and test results only. The check records its pass as before, and laptops and agents
  still skip it.
- **`hushrun init` wraps CI's lines that write reports**, such as `pytest --cov` or
  `go test -coverprofile=c.out`, which it used to leave: a laptop or an agent skips them on a pass,
  and CI runs them every time when a later step of their job reads the reports. init says which,
  `CI runs test every time: a later step reads its reports (codecov/codecov-action). Laptops and
  agents skip it on a pass.`, and what Codecov's own file means for them: the uploads it waits for
  (`after_n_builds`), and the flags that would need `carryforward: true` for CI to skip. On GitLab,
  where nothing reads `.gitlab-ci.yml` at run time, init writes `HUSHRUN_REPORTS_READ: "true"` into
  the jobs it changes whose artifacts keep reports, a `dotenv` report aside, since it passes
  variables to later jobs, and says to add it by hand where a job's variables come through `<<` or
  an anchor. A package.json script that writes reports, run in a CircleCI, Azure Pipelines,
  Buildkite, Jenkins or AppVeyor file that reads them, stays as it is, since a run there can't see
  the step.
- **`hushrun ready`** says `ci` for a check CI runs every time for its reports, with why, unless it
  failed here; **`hushrun doctor`** lists them in a new check, `reports`, as `ok`.
- **A check whose pass wrote nothing a later step reads skips in CI after its first pass in the
  job.** In a job with two test checks and an upload of one's coverage, the other skips once it
  passed there. It needs a step that reads only the paths it names: Codecov's actions look for
  coverage of their own besides `files`, so a check beside them runs unless the step sets
  `disable_search: true`. The pass must be this job's, recorded by this version of hushrun, with
  the workflow in the check's scope.
- init's notes for a Codecov upload say how to let CI skip its check: `reports.ci: skip` for it in
  `hushrun.yaml`, and `if: steps.<id>.outputs.hushrun-skipped != 'true'` on the upload, when its
  flag carries forward and `after_n_builds` doesn't count it. init never writes it.
- **`hushrun init` steps aside where a remote build cache already reuses results.** Turborepo, Nx,
  Gradle, Bazel and Pants with a remote cache set up keep their own results across machines, so
  init leaves what runs through them as it is, CI's lines and package.json's scripts alike, and
  says why: `Turborepo reuses its own results across machines (remote cache), so init leaves it to
  Turborepo`. It tells a remote cache by `TURBO_TOKEN` or `TURBO_TEAM` in CI's files or
  `remoteCache` in `turbo.json`; Nx Cloud in `nx.json` or `NX_CLOUD_ACCESS_TOKEN`; a remote
  `buildCache` in Gradle's settings, or the Develocity plugin, with caching on; `--remote_cache`,
  `--remote_executor` or `--bes_backend` in `.bazelrc`; and `remote_cache_read` in `pants.toml`.
  Without one, nothing changes. The plan as JSON says `ciWhy: stepped-aside` for each, a left
  script included. A repository can still wrap such a line by hand.
- `hushrun init` leaves a Maven phase whose tests are turned off, such as
  `mvn verify -DskipTests`, as it is: it builds, and tests nothing.

- **`hushrun ready` and the hook judge a job behind a changes filter as any job.** A job whose
  `if:` reads only other jobs' outputs and results, such as `if: needs.changes.outputs.api ==
  'true'`, runs when its area changed, so its checks are listed as `missing` or `passed` where they
  used to be `ci`, `its job has a condition`. A check with no pass yet is listed even on a push that
  doesn't change its area, such as right after hushrun is set up, and the hook asks an agent to run
  it once. Any other condition stays with CI, and a called workflow now takes the condition of the
  job that calls it.
- **A cargo line carries the flags CI sets for it.** When a job's, a step's or the workflow's `env`
  sets `RUSTFLAGS` or `RUSTDOCFLAGS`, init wraps the line with them, `hushrun run rustdoc -- env
  'RUSTDOCFLAGS=-D warnings' cargo doc --no-deps`, so they are part of what a pass is for: rustdoc
  without `-D warnings` passes on a broken link, and such a pass must never skip CI's run. A line
  init can't carry them into stays as it is, and the plan says why, such as on Windows or in a
  GitLab job. Lines already through hushrun are left as they are.

### Fixed

- **A package.json script in a repository whose CI's lines init gives scopes** now names its
  scope, `--scope all`, which holds the whole repository. It ran under the one scope of the
  project CI's lines were in, and could skip on a change elsewhere.
- `hushrun init` reads `npm test --prefix=<dir>` and `-w <package>` after the script's name as
  another package's script, as npm does, not the root's.
- **A line that sets a variable with a quoted value**, such as `RUSTFLAGS="-D warnings" cargo test`,
  was wrapped with the variable after `--`, where hushrun would have started it as a program, so
  the step failed. It goes through `env` now, keeping its quotes.
- `hushrun init` reads more of the flags that write a report a later step may read: pytest-cov's
  `--cov`, `--cov=src` and `--cov-report=xml`; RSpec's JUnit formatter (`--format
  RspecJunitFormatter`, `--require rspec_junit_formatter`); .NET's coverage collectors and coverlet
  (`--collect:"XPlat Code Coverage"`, `/p:CollectCoverage=true`); and flags about reports, such as
  `--make-reports` and `--reporter=junit`. A flag that turns a report off, such as `--no-coverage`
  or `--coverage=false`, and a reporter that only prints, such as `--reporter=dot`, no longer count.
- `hushrun init` reads `make -C build test`, `make -j 4 test` and `make -f ci.mk lint` as the
  checks they are: an option's value no longer hides the target.
- **`hushrun init` splits a repository whose project builds with a crate of another Cargo
  workspace.** A crate that inherits from its workspace (`edition.workspace = true`, `serde = {
  workspace = true }`) used to keep the whole repository in one scope, `<crate>/Cargo.toml inherits
  from a workspace`. Now the project's scope gets the workspace's `Cargo.toml` and the crates the
  crate inherits by path, but not their dev-dependencies, which it never builds. A project that is
  itself a member of a workspace now holds all of the workspace, whose lockfile it builds with.
- **`hushrun init` and `hushrun ready` read a folded `run: >` block as YAML gives it to the shell**:
  a command folded over lines is one command, wrapped once, in front of its first line, and a folded
  `hushrun run` is read as through hushrun already, where init used to propose a second one inside
  it. A comment after a block, indented no further than its key, is no longer read as part of it.

## [0.7.0] - 2026-11-03

### Added

- `hushrun repository usage` shows the free checks of an open source repository, which are in none
  of its other numbers, so a repository whose checks are all free no longer looks as if nothing was
  recorded: `free       github.com/acme/oss: 1,204 recorded for free, outside the plan`. Outside a
  repository, the line has the org's total. It appears only when the number is above zero, and
  `none of them` stays on the `repository` line, because it's still true. With `--format json`,
  `usage.free` lists each repository's free checks, which is additive to
  `hushrun.repository.usage/v1`. A server from before the field doesn't send it, and then the text
  and the JSON are what they were.

### Changed

- Contract: **the plan mode `skip` is now `current`**: the stage already runs this code, so there
  is nothing to deploy. `skip` read like the skip of a check that already passed, which `hushrun run`
  still records as `skip`. The `mode` output, `HUSHRUN_MODE`, `hushrun.plan/v1` and `hushrun status`
  say `current`. `--plan` and `HUSHRUN_PLAN_JSON` still read a plan that says `skip`, from 0.6.0 or
  earlier. The reusable workflow and the recipes deploy on `promote`, `refresh` or `full`, which
  don't change, so they need no edit. **A script of your own that matches `skip` must match
  `current`.**
- `hushrun plan` no longer names the scope `default`, which read as "by default", when it is the
  plan's only one: its text starts `promote: …` instead of `default promote: …`. On GitHub Actions
  its step summary starts with what the run does, such as `**Nothing to deploy.** This stage already
  runs this code.`, then a `| Mode | Reason |` table, and its annotation's title is
  `hushrun plan: production` instead of `hushrun plan: default on production`. A plan with named
  scopes, or several, is as it was.
- A plan's reason says what to do next. `wait`: the stage plans again by itself when the upstream
  finishes. `current`: to deploy it again, re-run it or start it by hand. A failed upstream: push a
  fix, or force a full run (`--force-full`). The wrong branch: the branch to run it from. A missing
  approval: `needs approval of release`, with the command a member runs,
  `hushrun approve release --stage production`, or the dashboard. A missing deployment check, such as
  `e2e@staging`: only `staging` records it, after it deploys this code, so re-run `staging`. Exit
  codes and annotations don't change: `blocked` still fails the job, with exit code 3.
- `hushrun init`'s plan says what it does with the repository: `adds github.com/acme/shop to your
  org's dashboard, unless it is there already`, and `hushrun.yaml` gets `repository
  github.com/acme/shop, its URL on hushrun, and the key that verifies hushrun's log`.
- The skill and the `AGENTS.md` block that `hushrun init` writes tell coding agents never to run
  `hushrun approve`, which refuses CI, `HUSHRUN_AGENT` and agent tokens but not an agent using a
  person's sign-in; to ask which organization when init lists several, then run
  `npx hushrun init --yes --org <slug>`; to force a run with the script's name and exact command,
  such as `npx hushrun run --force test -- vitest run`, so that it counts; to leave a check this
  machine can't run to the person, who can push or install the hook with `--warn`; and to report
  which checks hushrun skipped and which `hushrun ready` left to CI. Run `hushrun init` again to
  update them.

## [0.6.0] - 2026-11-02

### Added

- `counts_from` in `hushrun.yaml` says what a check needs of the machine a run happens on: `os`,
  `arch`, `go`, `rust`, `node`, `xcode` and `env`, at the top level, in a scope or for a check, the
  nearest winning whole, and `anywhere` for no rule. A run that doesn't meet it keeps its exit code
  and records nothing, so nothing skips on it, and costs no quota. It says so before a long run and
  in its last line: `hushrun: go-test passed in 6s; it won't count: needs go 1.25, this is go1.24.3`.
  One that does ends `hushrun: go-test passed in 6s on linux, go1.25.14; it counts`. A pass from a
  machine that met it skips on any machine, and skipping measures nothing. Without a rule, nothing
  is measured and a run prints what it did. A run measures what `PATH` finds, so a command that picks
  its own toolchain doesn't count: `cargo +nightly`, a tool given by its path, or `PATH` set in the
  command. What a machine says is its own word: this stops mistakes, not people. See
  [Where a check counts](https://hushrun.com/docs/where-a-check-counts).
  **A `hushrun.yaml` with `counts_from` fails to parse before this release, as other new keys did:
  hushrun 0.5.0 runs every check without skipping and records nothing, and its `hushrun ready` exits
  2. So does a check with no `kind`. Update every machine and CI job that reads the file first.**
- `hushrun init` writes `counts_from: {os: macos}` for an Apple project, and shows it in its plan: at
  the top of `hushrun.yaml` for a repository of one scope, in the scope for a project of several.
  Only when `xcodebuild` or `swift` goes through hushrun in a job with one `macos-*` runner, and
  every other check there is one of those or `swiftlint`, which earns no rule alone.
  Never for a Node job on `macos-latest`, a check that also runs on Linux or Windows, a `runs-on`
  holding an expression, a job with `uses:`, a local action off a Mac, GitLab, a file with stages, or
  a repository already set up, and it never changes a `counts_from` a file has, `anywhere` included.
  When nothing else writes `hushrun.yaml`, it writes one for the rule, with `version` and
  `repository`. A project with a `Package.swift` or only an `.xcodeproj` keeps a monorepo in one
  scope, so it gets no rule unless `hushrun.yaml` declares its scopes first.
- `hushrun init` reads `xcodebuild` as iOS projects write it: its action anywhere among its options,
  `test-without-building`, a `|` into `xcpretty` or `xcbeautify` that writes no report, and, in any
  language, a command over several lines that end in `\`. `-resultBundlePath`, `-enableCodeCoverage`
  and `--xunit-output` stay as they are, like any report a later step may read, and so does
  `fastlane scan`, which writes reports to `fastlane/test_output`, and says so.
- When `hushrun init` can wrap none of the checks it found in CI's files, it lists each with its
  reason, instead of saying it knows none.
- `hushrun ready --strict` exits 0 only when at least one check is known and every check passed.
  Plain `ready` doesn't change: it is true when nothing is `missing`.
- `hushrun ready` notes a rule in `checks.<name>` that nothing runs, to catch a typo:
  `hushrun.yaml has a rule for go-tset, and nothing runs it`.
- `HUSHRUN_AGENT`: a sandbox now needs one variable instead of three. One string holds the backend,
  the agent token and its key, on one line that a shell and a secrets form take as it is. `hushrun
  token create` shows it at a terminal, as the first line of stderr, and the dashboard's Agents page
  shows it. **It needs this release or later:** `hushrun init` pins `^0.5.0`, which never reaches
  0.6, so a repository set up on 0.5 moves with `npm install --save-dev hushrun@latest`, and an older
  hushrun ignores the variable and shares nothing.
- The string overrides a bare `CI=true`, which is what sandboxes set. GitHub Actions, GitLab CI,
  Buildkite, CircleCI and `HUSHRUN_PROVIDER` beat it, so a job a secret reaches stays CI. Its backend
  counts as named by the environment, so a repository with no `backend:` still shares results, and
  its token goes there and to no other server. CI outside GitHub keeps `HUSHRUN_TOKEN` and
  `HUSHRUN_MACHINE_KEY`, and sandboxes set up with them keep working.
- With `HUSHRUN_AGENT` set, `hushrun whoami` and `hushrun doctor` say it named the backend, is the
  credential and holds the key, and `doctor` tries the key. It wins over `HUSHRUN_TOKEN`,
  `HUSHRUN_MACHINE_KEY`, `HUSHRUN_MACHINE_KEY_FILE` and a different `HUSHRUN_BACKEND`, and says so in
  one line. A string hushrun can't read says what's wrong, without printing any of it, and the check
  still runs.
- `hushrun approve`, `hushrun login`, `hushrun token` and the pre-push hook treat `HUSHRUN_AGENT` as
  an agent, even a string they can't read, and even beside `HUSHRUN_INVOKED_BY=human:…`. No login
  speaks for it, so `hushrun init` stops and says so, and with a string that can't be read nothing
  is recorded. An isolated run never passes it into a container.
- A `HUSHRUN_AGENT` string in any other `HUSHRUN_*` variable, in quotes or without its prefix, is
  dropped with a line. One on a command line stops the command with exit 2, and one in a URL or in
  `hushrun.yaml` is refused. hushrun never sends or prints one. Naming the thing is fine: this looks
  for a real string, not for the words `hushrun_agent_`.
- A rule's `env` in `counts_from` can't name `HUSHRUN_AGENT`, `HUSHRUN_TOKEN` or
  `HUSHRUN_MACHINE_KEY`, whose values are secrets it would compare to.

### Changed

- Contract: `hushrun.ready/v1` has `recordedBy` on each check: who recorded a `passed` check's result,
  as the server says.
- Contract: a run result has an optional `environment`, on every result from now on: the `platform`,
  and the `tools` a rule made hushrun measure.
- Contract: `checks.<name>.kind` in `hushrun.yaml` is optional, `source` by default, with the same
  definition digest.
- `hushrun ready`'s `ci` now means nothing here changes it, and CI or another machine has to run it.
  `hushrun ready` and the pre-push hook start no tool, for any repository, and they no longer start
  Docker or Podman to see if it is there: they look for it. They read a rule's `os`, `arch` and `env`,
  and a check with a tool in its rule stays `missing` until a run says otherwise. A run that doesn't
  count, pass or fail, leaves its reason, and `ready` then says `ci`: `a run here didn't count: needs
  go 1.25, this is go1.24.3`. That stands for this code on this machine while no result is recorded
  for it, so a machine fixed later costs one more run. A recorded failure outranks all of it: the
  check stays `missing`, and when a run here wouldn't count, `ready` gives no command and says what
  clears it.
- Contract: `--format json` of `hushrun token create` gains `hushrun_agent`, how scripts get the
  string. Stdout without it stays the token alone.
- Contract: **`hushrun token create` prints the agent's key on stderr only when stderr is a terminal,
  outside CI, and the token there only when stdout is a terminal too.** A piped stderr is often a log,
  and a job's always is. A script that read the key from stderr must use `--format json`, which gives
  the token, the key and the string; a token made without seeing its key is no use, and stderr says
  to revoke it.
- Contract: `named_by` in `hushrun.whoami/v1` and `hushrun.repository.usage/v1` can be `HUSHRUN_AGENT`.
- Contract: a new environment variable, `HUSHRUN_AGENT`. `hushrun doctor` has a new check, `agent`,
  when an agent is set.
- Contract: a command line that holds a `HUSHRUN_AGENT` string exits 2, and a run with one in its
  command doesn't run it.
- Contract: a run result has an optional `job`, the CI job that ran it and the system it ran on,
  such as `test@linux/amd64`.
- A CI pass counts in its own job only: one of the same name on the same system. On GitHub Actions
  the name is the job's id, whichever workflow file holds it, and on GitLab CI its `CI_JOB_NAME`. So
  a `staging.yml` and a `production.yml` with one job id on one system keep sharing a pass, even if
  they install different tool versions, and a `test` job on macOS no longer reuses a `test` pass from
  Linux. A pass from a laptop or an agent still skips them. When two jobs run one check, the newest
  result can hide a job's own pass, and both run. A CI pass from before this release runs once more.
- A reusable workflow's jobs no longer skip in CI: hushrun can't tell how many variants they have.

### Fixed

- `git push` no longer starts a `node_modules/.bin/hushrun` that git tracks: the hook's lines then use
  the `hushrun` on `PATH`. **A clone keeps the lines it has until `hushrun hook install`, which a
  package.json's `prepare` runs at each install. Elsewhere, run it once.**
- A pass no longer skips a run it can't stand in for. The run key holds no environment, so in a
  matrix job the first variant to pass skipped the rest, and a check that fails on Node 25 printed
  `skipped` and exited 0 because it passed on Node 16. In a job with several variants, or one
  hushrun can't read, `hushrun run` now runs the check every time, records it, and says why in one
  line. A name that differs per variant, such as `hushrun run test-${{ strategy.job-index }} --`,
  keeps skipping. On GitLab CI, a job with `parallel` runs every time. Outside CI nothing changes.
  **If your repository gets hushrun from npm, update it: your lockfile pins 0.5.0, which keeps
  skipping wrongly until it moves.**
- `hushrun init` keeps wrapping a package.json script that a job with several variants runs: CI runs
  each variant and records it, and laptops and agents skip.
- `hushrun ready` says `ci` for such a check, unless it failed on this code, which stays `missing`,
  and no longer says CI skips what another CI job passed. It follows a job through the scripts it
  starts, as `hushrun doctor` does. `doctor` has a `variants` check that says CI runs such a check
  every time and how to name each variant.
- A reusable workflow's job no longer skips when a plain job of the caller has its id. A call to
  another repository can't be read, so that case still reads as the plain job.
- `hushrun init`'s last line said CI skips any check wherever it passed, even when nothing went
  through hushrun. It now says what a job skips, and only when something does.

## [0.5.0] - 2026-11-02

### Added

- `hushrun init` asks which organization a repository is for when you belong to several, instead of
  taking the one you signed in to last. Enter takes the one named like the repository's owner, else
  the last one. `--org <slug>` says it. With `--yes` or without a terminal, init takes the org named
  like the owner, or exits 2 and lists your organizations. Exit code 2 is new here for `init --yes`.
  A repository that already names its organization keeps it, and nothing asks.
- `hushrun init --org <slug>` moves a repository that records to another of your organizations: its
  backend, its URL and log key, and connecting it there. The old organization keeps what it recorded.

### Changed

- Contract: the credentials file's login has an optional `machine_org`.

### Fixed

- A machine is registered in every organization it writes to. It used to register in one only, so
  results for a person's other organizations weren't recorded (`machine_unknown`).
- In a Bun repository, CI now skips tests that already passed. `hushrun init` left `bun test` in CI
  as it was and printed `workflows none run these checks`, so CI ran every test on every push. It
  now wraps `bun test`, with or without a `test` script, and any command at the root that a wrapped
  script runs word for word. Such a command takes the script's name, so that a pass of `bun run test`
  skips it in CI. In a repository already set up, run `hushrun init` again; it changes nothing else
  in CI's files.
- `hushrun ready` and the pre-push hook no longer say CI will skip a check that CI runs outside
  hushrun: they name the command and the job, and say what to do.
- `hushrun init` says which command goes through hushrun in a Bun repository: `bun test` is Bun's
  own runner, which never reads package.json, so it can't skip.
- A package.json script whose tool was never installed, such as a `lint` with no ESLint in the
  repository, no longer looks like a failure. `hushrun init` says the tool isn't installed and
  shows the skip with the next script. `hushrun ready` leaves the check with CI, so the pre-push
  hook no longer refuses a push for a command that can't pass here. Running the script still
  exits 127.

## [0.4.1] - 2026-11-01

### Changed

- The npm packages are published through npm's trusted publishing, with no token (ADR 0039). npm
  ends publishing with tokens in January 2027. Nothing changes for anyone installing them.

## [0.4.0] - 2026-11-01

### Added

- **Any language** (ADR 0037). Contract changes first:
  - **`hushrun.yaml`: `backend`** (C-3, C-11) names where a repository's checks share results, for
    a repository with no package.json, with exactly the token rules of package.json's. `whoami` and
    `repository usage` give `named_by: "hushrun.yaml"`.
  - **`hushrun.yaml`: `scopes.<name>.external`** (C-4) declares what a scope's checks build against
    outside the repository: a git checkout's top, relative to the root, with `include` and
    `exclude`. It doesn't go with stages (C-8).
  - **The run key** (C-9) and **`hushrun.run-result/v0`** (C-10) hold `external`, only when a scope
    declares it: every key that exists stays as it is.
  - **`hushrun-results/v0`** (C-12): a lookup may ask `?as=ci&repository=<id>`, what that
    repository's CI would find, which `hushrun ready` does. A server may ignore it.
  - **`hushrun init` finds checks in CI's own files**, GitHub's workflows and GitLab's pipeline, and
    wraps them in place, with what the job needs: hushrun's install and an ID token. One table
    recognizes Rust, Go, Python, Java, Kotlin, Ruby, PHP, .NET, Elixir, Swift, Terraform, and `make`,
    `just` and `task` targets. What can't skip safely stays, and the plan says why.
  - **A repository of projects gets a scope each** in `hushrun.yaml`: the root's files, without its
    prose, CI's files, and what the project's manifests, links and `../` paths name; what they name
    outside the repository becomes `external`. A workspace, or a manifest init can't read, keeps one
    scope, and the plan says why. A path built at run time can't be seen, and the plan says that too.

### Changed

- **`hushrun ready` and the pre-push hook read CI's files too**, so a repository in any language has
  checks, and look each run up by its scope, its directory and its `external` inputs: a repository
  of several scopes gets real states, and the hook can refuse a push there.
- **`hushrun ready` leaves more with CI**, so that doing what it says always ends: a job with
  services or a container, one that needs a secret, a condition, another operating system, a key CI
  fills in, and everything when the repository's CI reuses only CI's results. A workflow CI wouldn't
  start for the push says nothing.
- **`hushrun init` without package.json** names the backend in `hushrun.yaml`, and says that each
  clone needs `hushrun hook install`.

### Fixed

- **A `git gc` running in the background no longer reads as a missing object.** `git commit` can
  leave one running, and for the moment it moved objects into a pack, a check run or `hushrun ready`
  could fail with `E_MISSING_OBJECTS` on an object that was there. A read gix can't do now goes to
  git, and only what git can't read either fails (D-9).
- **On Linux, an edit that kept a file's size in the second it was committed could go unseen.** The
  worktree's digest came from a copy of git's index, and the copy's newer date told git to trust
  the file's size and time. `hushrun run` could then skip a check on code that had changed, and
  `hushrun ready --worktree` could say a repository was ready when it wasn't. The copy now keeps
  the index's date, so git reads such a file's bytes (D-10). macOS was not affected.

## [0.3.0] - 2026-11-01

### Added

- **Agents find out before they push** (ADR 0036). Contract changes first:
  - **Exit code 10**, from `hushrun ready` and the pre-push hook: a check that can run here hasn't
    passed on this code.
  - **`hushrun.ready/v1`**, the output of `hushrun ready --format json`.
  - **`hushrun ready`** says what CI would still run on this exact code, and the commands that run
    it here first, from `hushrun.yaml`'s stages and package.json's `hushrun run` scripts. What can't
    change from here stays with CI and doesn't count against the code, so doing what it says ends.
  - **`hushrun hook install`** adds a pre-push hook that refuses a coding agent's push until then,
    and warns a person; `--refuse` and `--warn` make it everyone or no one. It runs no checks, and
    only a known gap stops a push: a crash, a timeout or a server that doesn't answer never does.
    `hushrun hook uninstall` takes it out.
  - **`hushrun init`** writes a skill for Claude Code, Codex, OpenCode and Cursor, a block in
    `AGENTS.md`, the hook, and a script in `package.json` that puts the hook in every clone
    (`prepare`, or `postinstall` for Yarn from 2 on). A repository set up before gets them by
    running init again.

### Changed

- **npm: the platform packages are scoped**, `@hushrun/cli-<os>-<cpu>` instead of
  `hushrun-<os>-<cpu>`: npm's spam check refused a new unscoped `hushrun-win32-arm64`. `hushrun`
  keeps its name, so `npx hushrun` and devDependencies are unchanged. 0.2.0, the first version on
  npm, has these names.
- **npm and GitHub say what hushrun does:** the package's description and keywords, and links from
  both READMEs to the docs, the guides and the comparisons.

## [0.2.0] - 2026-10-29

### Changed

- **CI installs the newest hushrun release by default** (ADR 0035), and pinning one is the
  opt-in; it was the other way round. A new release reaches every pipeline that pins none at its
  next run: fixes without an edit, and a bad release as fast. Contract changes first:
  - The setup action's `version` is optional, `latest` by default: it installs the newest release,
    from that release's tag, and says which in a new output, `version`. `sha256` needs a pinned
    `version`.
  - The reusable workflow's `version` is optional too. Its plan job installs the caller's
    version, else the newest release, and outputs it; every later job of the run installs the
    same one.
  - `install-hushrun.sh` installs the newest release without `HUSHRUN_VERSION`, or with
    `latest`, and takes `HUSHRUN_INSTALLER_GITHUB_BASE_URL` for GitHub's address. A release its
    copy doesn't list is checked against the `.sha256` published with it, read again over TLS on
    every run, instead of refused; over plain HTTP it is still refused. `HUSHRUN_SHA256` and
    `HUSHRUN_DOWNLOAD_URL` need `HUSHRUN_VERSION`.
  - The newest release's image is also `ghcr.io/hushrun/hushrun:latest`. Publishing an older
    release again never moves it, and a pre-release never gets it.
  - `hushrun adopt` writes no `version:`, only a comment that shows how to pin one, and
    `--hushrun-version` pins. In `hushrun.adopt/v1`, `version` is null unless pinned.
  - The GitLab recipe runs `ghcr.io/hushrun/hushrun:latest`, and its plan job passes the
    release it ran to later jobs as `HUSHRUN_VERSION`, in its dotenv report. The GitHub recipes
    pin no version.

### Added

- **`hushrun update`, and a notice when a newer release is out** (ADR 0035). Contract changes
  first:
  - `hushrun update` replaces a hushrun that the shell or PowerShell installer installed, when
    the installer's receipt names it, with the newest release: the archive for this platform,
    checked against the release's `.sha256`, unpacked with the system's `tar`, and put in place
    with a rename (on Windows, the running binary moves aside as `hushrun.exe.old` first); the
    receipt then says the new version. It exits 0 when it updated or nothing is newer; 2 for an
    install it doesn't manage, such as npm's, CI's or one built from source, with how to update
    that one; and 1 when it can't tell the newest release or the download fails its check,
    leaving hushrun as it was (C-20).
  - At a terminal, at most once a day, hushrun says ``hushrun <version> is available: run
    `hushrun update` `` on stderr after a command. It asks GitHub in the background while the
    command runs, at most once a day, and waits at most 200 ms after it; it never asks or says
    anything in CI, with `--format` other than text, or with `-q`. `HUSHRUN_NO_UPDATE_CHECK`
    turns it off. It keeps `update-check.json` in hushrun's cache directory (C-21).
  - `HUSHRUN_INSTALLER_GITHUB_BASE_URL`, the installers' own variable, points both at a mirror,
    or at a test's server.

## [0.1.0] - 2026-10-29

### Changed

- **Results from people and agents are always signed, and every agent token comes with its key**
  (ADR 0028 amendment 1). hushrun cloud took a person's or an agent's result without
  `Hushrun-Signature` for one release; nothing was deployed with that, so it now refuses one. And
  a fleet's sandboxes no longer register a machine each: an agent has the one key its token came
  with. Contract changes first:
  - `hushrun token create --agent NAME` always makes the agent's key, a P-256 key, and sends its
    public half with the request that makes the token: `machine: {"key", "proof"}`, the proof the
    key's signature over `hushrun-agent-machine/v1\n<org>\n<agent>\n<key id>\n` (C-12). The
    server registers it as the token's one machine, `agent:NAME`. hushrun prints the token on
    stdout, and the key once on stderr, after the variables to set, as `HUSHRUN_MACHINE_KEY`;
    `hushrun.token/v1` always has `machine_key` (`pem`, `public_key`, `keyid`) and the server's
    `machine`. `--with-key` is gone, and `hushrun init` suggests
    `hushrun token create --agent gitlab-ci --expires 365d`.
  - `hushrun run` with a personal or an agent token signs every result (C-18). An agent signs only
    with the key the environment names, `HUSHRUN_MACHINE_KEY` or `HUSHRUN_MACHINE_KEY_FILE`, never
    a key file a person's sign-in left on the machine, and never registers a machine
    (`POST {base}/v0/machines`). Without a key, hushrun doesn't send the result; with a server's
    401 `machine_required` or `machine_unknown`, it isn't recorded either. Either way the run ends
    with `hushrun: <NAME> passed in <time>; not recorded:` and what to do:
    ``this machine isn't registered: run `hushrun login` `` with a person's token,
    ``set `HUSHRUN_MACHINE_KEY` to the key `hushrun token create --agent` printed`` with an agent
    token (C-14). The command still runs, and hushrun keeps its exit code.
  - A person's machine registration the server refuses, such as 409 `machine_limit`, leaves the
    result unrecorded with the server's message; before, the result went unsigned.
  - `hushrun login` ends with the server's message, and exit 1, when the server won't register
    the machine: 409 `machine_limit` (20 live machines in the org; the message names the
    dashboard's machines page) or 409 `machine_revoked` (the machine was revoked for good), to
    which hushrun adds that removing the key file makes this machine a new key (C-13).
  - hushrun cloud revokes a person's machine unused for 90 days; `hushrun login` on it brings it
    back (C-18).
  - `hushrun whoami` shows, for an agent token, the key the environment names, the one its results
    are signed with, or `machine   none: set HUSHRUN_MACHINE_KEY …` when there is none (C-17).

- **The product is now hushrun** (it was promote, whose domain belongs to someone else). Nothing
  was published under the old name, so none of this keeps the old names working. Contract changes
  first:
  - The binary and the crates (`hushrun`, `hushrun-*`), and the npm package `hushrun` with platform
    packages `hushrun-<os>-<cpu>`: `npx hushrun init`.
  - `hushrun.yaml` (was `promote.yaml`), the `HUSHRUN_*` variables (were `PROMOTE_*`) and the
    package.json key `"hushrun": {"backend": …}`.
  - The hosted backend is `https://api.hushrun.com`, and the
    dashboard `https://app.hushrun.com`.
  - On the wire: the `Hushrun-Signature` and `X-Hushrun-*` headers, the token prefixes `hra_`, `hru_`
    and `hrl_`, the event producer `"tool": "hushrun"`, the `hushrun.*/v1` schema ids, the
    `application/vnd.hushrun.*` media types and every signing label. The source digest's label is
    `hushrun-source-v1`, so every source digest and run key differs from before.
  - `hushrun adopt` writes `uses: hushrun/hushrun/action/setup`, and the image is
    `ghcr.io/hushrun/hushrun`.
  - The plan mode `promote` keeps its name: it says what a stage does with the digest.
- **hushrun is closed source** (ADR 0034): free to use under `LICENSE.md`, a proprietary
  license, instead of Apache-2.0. Nothing was released under Apache-2.0. Contract changes first:
  - Releases, the setup action and the reusable workflow stay at `github.com/hushrun/hushrun`,
    now a public repository without source: installers, `npx hushrun`, `uses:` lines, GitLab's
    `install-hushrun.sh` and the image keep their URLs.
  - Each release archive holds the binary, a README, `LICENSE.md` and `THIRD-PARTY-NOTICES.md`,
    which lists the open-source crates the binary is built with and their license texts.
  - The npm packages say `"license": "SEE LICENSE IN LICENSE.md"`, carry both files, and are
    published without provenance. The image's `org.opencontainers.image.licenses` is
    `LicenseRef-hushrun`, and it holds both files in `/usr/share/licenses/hushrun/`.
  - No crate is published to crates.io.

### Added

- **When the org's plan records no more checks, nothing fails** (ADR 0032). hushrun cloud's
  plans include a number of recorded checks per period; past it, its server answers a new
  `check` event, or a `pass` or `fail` result, with 402 `quota_exceeded`, and takes everything
  else. Contract changes first:
  - A 402 whose `error.code` is `quota_exceeded` is a new ledger error, `Quota`, with the
    server's message and, when the answer has them, `upgrade_url`, `plan`, `used`, `limit` and
    `period_end` (L-3, L-10). It isn't retried. Any other 402 is still `Invalid`.
  - `run --stage` and `attest` don't record a check event refused so: they say once per command
    `warning: check results aren't recorded: <the server's message>; upgrade: <upgrade_url>` (a
    warning annotation on GitHub Actions), record the rest, and keep their exit codes, `run
    --stage` 0 when its checks passed and 8 when one failed, `attest` 0 (L-5). `hushrun.run/v1`
    gives such a check `event`, `custody` and `environment` null and a `detail` ending in `; not recorded`;
    `attest` prints `not recorded check: <name>` and leaves it out of `hushrun.attest/v1`'s
    `events`. Every other failure exits 6, as before.
  - `hushrun run NAME -- COMMAND` keeps COMMAND's exit code when its record is refused so, and
    ends with `hushrun: <NAME> passed in <time>; not recorded: <the server's message>; upgrade:
    <upgrade_url>`, or `failed in <time> (exit <code>)` (C-14).
  - New command `hushrun repository usage`: the org's plan, this period's recorded checks against
    what the plan includes and its limit, the overage, this repository's share and the dashboard,
    from `GET {base}/v0/usage` (C-12, C-19) on the backend `hushrun run` records to, with the
    credential it sends there. Results on this machine are metered nowhere, and a server that
    answers 404 or 405 doesn't report usage; it says so and exits 0, and exits 6 when the server
    can't be reached, refuses the credential or answers anything else. JSON: new schema
    `docs/schemas/repository-usage.v1.schema.json`.
  - New command `hushrun repository stats [--since 30d] [--local]`, for sharing volume without
    telemetry (L-14): events by kind, distinct digests and check events by stage, from one
    listing of the repository's events and no event read; with `--local`, this machine's `hushrun
    run` results by verdict, by check, the distinct run keys and the time skips saved, sending
    nothing. JSON: new schema `docs/schemas/repository-stats.v1.schema.json`.

### Changed

- **Approvals count only from people** (ADR 0031 amendment 3). An approval check took an
  approval from any signer but the CI, so an agent token with its machine key (`hushrun token
  create --agent NAME --with-key`) could approve a deploy. P-3 now counts only approvals a
  member's machine signed (`user:<email>`, the browser key the dashboard registers included), at
  every stage, CI-only or not; everything else an agent signs counts as before. Contract changes
  first:
  - `plan`, `run` and `status` don't count an `approval` event signed by an agent's machine
    (`agent:<name>`): the requirement stays `missing` until a person approves, and hushrun cloud's
    approvals page agrees.
  - `hushrun approve` exits 2 and records nothing, and with `--out` writes nothing, when it would
    approve as an agent: with an agent token in `HUSHRUN_TOKEN`, before it reads or signs anything
    (`HUSHRUN_TOKEN is an agent token, and approvals count only from people: …`), and when the
    server names its signing key, a key file or `HUSHRUN_MACHINE_KEY`, an agent's machine (`key
    sha256:… signs as agent:<name>, and approvals count only from people, never from an agent:
    …`).
  - `hushrun verify`'s `notCountingAt` (`hushrun.verify/v1`) names an approval's own stage when no
    person signed it, the CI's included, and the text says `doesn't count at <stage>: approvals
    count only from people`.
  - `hushrun policy explain` says `approvals from people only, on members' machines` at a CI-only
    stage, where it said they came from members' machines and agents.

- **Membership is trust** (ADR 0031). `hushrun.yaml` no longer says who may sign. An event counts
  when a registered machine (C-18) of a current member of the repository's org, or of one of its
  agent tokens whose scope covers the repository, signed it, or the repository's own CI, keylessly,
  with a certificate for the repository's numeric id the server knows (T-15). Signers are
  `ci`, `user:<email>` or `agent:<name>`; a machine claims custody `isolated` at most, and only
  `ci` claims `ci`. Logging out, removing a member, revoking an agent token or a machine stops
  what it signs from counting; signatures timestamped before that keep counting. Contract
  changes first:
  - New ledger request (L-13): `GET {repository_url}/signers` answers `{"signers":
    <hushrun.signers/v1>, "statement": <signed note>}`, the CI identities and every machine with
    its owner and revocation time, and the log key's signed note over the document's hash.
    hushrun checks it against `hushrun.yaml`'s `transparency_log.key`, and refuses an answer that
    doesn't verify (exit 6) or names another repository (exit 2); in CI, a server whose CI is
    another repository id exits 2.
  - `transparency_log` is a top-level key of `hushrun.yaml` (T-9), read with the rest of the file.
    The witnesses the server submits to are the server's (`GET {base}`'s `log.witnesses`);
    `hushrun.yaml`'s witnesses are what readers require, and `doctor` compares the two (T-13).
  - A stage can be CI-only (T-16): its checks, artifacts and own and upstream lifecycle events
    count only from the CI. Approvals count from any member's machine, never from the CI. The
    rule is set on the server, by the org's admins, for the org or one repository (ADR 0031
    amendment 1), and the signers answer carries it (`ci_only`, L-13): `accept` in
    `hushrun.yaml` is an unknown key. [ADR 0031 amendment 2] The answer carries each stage's
    whole history, `ci_only: [{"stage", "periods": [{"from", "until"?}]}]`, the org's and the
    repository's periods merged: a non-CI event doesn't count at a stage when its trusted time
    falls in any of its periods, or, without a trusted time, while the stage is CI-only now. So
    one timestamped before the rule began keeps counting, and one made during an earlier period
    doesn't count again when the rule is lifted and set again. `verify` says where an event
    doesn't count (`notCountingAt`), at stages CI-only now or before.
  - `hushrun policy explain --stage T` asks the server's signers answer whether `T` is CI-only,
    checked against the pinned log key as `plan` and `verify` check it, or reads a saved one with
    `--signers FILE`: it says "CI only since …" with the earlier periods, and each requirement
    counts from the CI only, or from members' machines, agents and the CI. Without a URL, or
    when the server doesn't answer, it says in one line that CI-only stages weren't checked and
    exits 0. JSON (new schema `docs/schemas/policy-explain.v1.schema.json`): `ciOnly` is
    `{"checked": true, "from": "server" | "file", "source", "statementChecked", "now", "since",
    "periods"}` or `{"checked": false, "reason"}`, and a requirement's `accepts` is `people`,
    `ci` or `signers`.
  - New commands: `hushrun repository signers [--out FILE]` prints or saves the server's signed
    answer, and `hushrun verify --signers FILE` verifies against a saved one, offline. `verify`'s
    JSON gives each result's `signer` and its `signature` (`kind` `machine` or `identity`);
    `approve --format json` gives `signer` and `signedWith`; `plan --explain-json` records the
    signers answer it used.
  - GC statements are signed by a machine the answer marks `gc`: an org admin's or owner's (L-9).
  - `npx hushrun init` writes `hushrun.yaml` with `repository`, `repository_url` and the
    server's log key, and nothing else: no identities, no ruleset, no consent screen, no `glab`.
    It ends with "Commit and push". `init --pipeline` writes a starter `hushrun.yaml` whose
    says nothing about CI-only stages, and no keys; `--keys-dir` is gone. `adopt` writes the
    workflows with `signing: sigstore` and no trust.
  - `hushrun.yaml` is the evaluated revision's (T-5) for the repository's URL too: in CI without
    `HUSHRUN_REPOSITORY_URL` or a base, the URL it names gets the job's ID token, never a bearer
    token (T-4). Nothing reads the default branch.
- **init asks to join the repository's org for someone who isn't in it.** When `hushrun.backend`
  names an org the person isn't a member of (a fresh sign-in approved for another org, or a saved
  login the org refuses), init reads their request to join (results-v0 §3,
  `GET {base}/v0/access-requests`): one waiting, filed on the approval page or by an earlier run,
  is reported, not filed again; otherwise init asks `Ask the admins of <org> to let <email> in?
  [Y/n]` (yes with `--yes`; without a terminal it files nothing and says how) and files it with the
  repository's id. It says the request was sent, or is still waiting, and where an admin approves
  it (`<dashboard>/<org>/-/members`), writes nothing the org would refuse, as before, and ends with
  "Once an admin of <org> approves your request, run npx hushrun init again". Someone invited
  is told to accept the invitation; a request declined lately, and a backend without requests to
  join, fall back to asking an admin for an invitation.
- **A sign-in approved for no organization no longer says the server doesn't register machines.**
  It says the machine isn't registered yet, since a machine is registered in an org; `hushrun run`
  registers it on its first write to one (`POST {base}/v0/machines`).

- **A first-time user's `npx hushrun init` ends with the repository added, in one run.** The
  approval page now lets someone without an org join or create one while approving, so the
  sign-in comes back with an org and init carries on. A sign-in approved for no org (a server
  from before that page) no longer says `named no org: run hushrun login …/o/<org>`: it says to
  create or join an organization at the dashboard's `/new` (the sign-in's verification URL
  without `/device`), then run init again; `hushrun login` says the same. A repository whose
  `hushrun.backend` is an org the person isn't in, signed in to an org of their own, says so and
  to ask an admin of that org for an invitation, and init sets up the files without the refusals
  of connecting the repository and asking for its ledger.
- **`no repository URL` says when hushrun.yaml isn't committed yet.** hushrun reads `hushrun.yaml`
  as committed, so right after `hushrun init` a command such as `hushrun repository ls` found no
  `repository_url`; when the working tree's `hushrun.yaml` names one, the message says to commit it
  or pass `--config hushrun.yaml`.

- **The settings people type say "repository", not "ledger"** (ADR 0029, ADR 0030).
  - hushrun.yaml's `ledger:` is `repository_url:` (`repository:` already holds the repository's
    id), `--ledger` is `--repository-url`, `adopt --ledger-base` is `--repository-base`, and
    `witness serve --ledger-url` is `--log-url`.
  - `HUSHRUN_LEDGER` is `HUSHRUN_REPOSITORY_URL`, matching the flag and key; `HUSHRUN_LEDGER_BASE`,
    `HUSHRUN_LEDGER_TOKEN` and `HUSHRUN_LEDGER_TOKEN_COMMAND` are `HUSHRUN_REPOSITORY_BASE`,
    `HUSHRUN_REPOSITORY_TOKEN` and `HUSHRUN_REPOSITORY_TOKEN_COMMAND`. Token commands get the URL
    as `HUSHRUN_REPOSITORY_URL`.
  - The reusable workflow takes `repository-url` and `repository-base`, from the GitHub variables
    `HUSHRUN_REPOSITORY_URL` and `HUSHRUN_REPOSITORY_BASE`.
  - `ledger_log:` is `transparency_log:`, and a stage's `on_ledger_error` is
    `on_repository_error`.
  - `hushrun ledger ls | show | gc | monitor` is `hushrun repository …`, and its JSON
    outputs are `hushrun.repository.ls/v1`, `.show/v1`, `.gc/v1` and
    `.monitor/v1` (schemas `repository-*.v1.schema.json`; the monitor's state file is
    `hushrun.repository.monitor.state/v1`). `doctor`'s checks `ledger`, `ledger protocol`,
    `ledger url`, `ledger log` and `ledger witnesses` are `server`, `server protocol`,
    `repository url`, `transparency log` and `witnesses`. Messages say "server" or "repository
    URL" where they said "ledger".
  - Nothing was released with the old names, so none of them is kept as an alias: the old keys,
    flags and subcommand are unknown, and the old variables are ignored.
- **What hushrun prints and writes cites nothing users can't open.** `--help`, messages, and the
  files `adopt` and `init --pipeline` write drop spec rules, ADRs, milestones and repository paths
  such as "(T-5)" or `docs/guides/github.md`; a guide is named by its page on hushrun.com/docs.
  So do the recipes people copy (`recipes/`) and the reusable workflow's input descriptions.

### Removed

- **Witnesses** (ADR 0033). Nothing was released with them. Contract changes first:
  - `transparency_log` names `origin` and `key` only: `witnesses`, `quorum`, `map_quorum` and
    `max_age` are unknown keys (exit 2), and the `hushrun.v1` schema matches.
  - `hushrun witness` (`init`, `serve`, `--verify-map`, `--log-url`) is an unknown subcommand;
    `hushrun.witness/v1`, `hushrun.witness.state/v1` and `hushrun.witness.evidence/v1` are gone.
  - Commands read no cosignatures and no longer warn that `hushrun.yaml` names no witnesses, and
    they don't check a listing's proof: they check what L-11 says (receipts, signed listings,
    one history), as they did without witnesses. The client no longer reads `GET {base}`'s
    `log.witnesses` or asks `log/leaves` for `size=`.
  - `hushrun repository monitor` drops `witnesses` and `unwitnessed_checkpoint` from
    `hushrun.repository.monitor/v1`, and no longer exits 6 when witnesses fall behind; it still
    checks every map leaf (`map_mismatch`). `doctor` drops its `witnesses` and `server witnesses`
    checks.
  - Gone with them: `hushrun-tlog`'s `cosignature`, `witness` and `replica` (and the compact
    range's `push` and `nodes` and the map builder's live keys, which only the replica read), the
    testkit's witnesses and its leaves that don't hash to the checkpoint, the cosignatures in
    `tools/reftlog.py` and `testdata/tlog/vectors.json`, and the witness guide. ADRs 0020 and 0021
    are superseded in part; the map, listing proofs and the monitor's map checks stay (L-12).
- **Trust in `hushrun.yaml`, and everything that maintained it** (ADR 0031). Nothing was
  released with it. `trust:` is an unknown key, and with it principals, `accept` maps,
  `min_custody`, `locations`, `constraints`, `max_attempts`, `audit` (P-17), `protected_paths`
  (P-16), validity windows and retired keys with their inventories, and `gc` principals. Gone
  too: `--trust`, `--trust-from-tree`, `HUSHRUN_TRUST` and `HUSHRUN_TRUST_REF` (ignored), the
  default-branch fetch (T-14), `hushrun trust`, `hushrun org` and `org.yaml`, `doctor --org`,
  `adopt --org` and `--repository-id`, `doctor --compiled`, `repository inventory`, init's
  GitLab identities, ruleset and Sigstore consent, events' `trustDigest` and `trustRevision`,
  plans' `protectedDigest` and audits (`reason: audit`, `audited`), `status`'s broken
  constraints, and the schemas `trust.v1`, `org.v1`, `org-plan.v1`, `org-apply.v2`,
  `doctor-org.v1` and `repository-inventory.v1`.
- **The fallback to signed trust, and the migration from it** (ADR 0026 amendment 2). Nothing
  was released with a root or signed trust. Removed: hushrun.yaml's `ledger_root` (now
  an unknown key), `adopt --ledger-root`, init's move of signed trust into hushrun.yaml,
  `--trust` with a repository URL (exit 2) and `--trust` naming a URL, `doctor`'s `trust
  migration`, `signed trust` and `trust root` checks, `repository gc`'s root-key signer, the
  monitor's root attribution and its `root_rewritten` finding, and the `root.v1` and
  `signed-trust.v1` schemas. The hidden `trust root`, `trust sign`, `repository put-root` and
  `repository put-trust` stubs are gone too: they are unknown subcommands.
  The client's configuration object API (`get_config`, `put_config`, `create_config`, and
  `GET`/`PUT {base}/config/{name}`) goes with them: it held only roots, signed trust and
  inventories, and hushrun cloud no longer serves it.

### Fixed

- **A timeout stops the whole check** (§10.4). At a check's `timeout`, `hushrun run` killed only
  the shell its `run` goes through, and what that shell had started went on running past the
  timeout, holding the job's output open; with dash, Debian's and Ubuntu's `sh` (GitHub's Linux
  runners among them), that was the command itself. hushrun now kills every process under the
  check, on Linux and macOS, and the check fails as before. The check stays in hushrun's process
  group, so a terminal's Ctrl-C and prompts reach it as they did.
- **A bad environment variable never stops a command** (§10.1, C-14). clap read
  `HUSHRUN_CONFIG`, `HUSHRUN_FORMAT`, `HUSHRUN_NO_CI_OUTPUT`, `HUSHRUN_GIT_READER` and `plan`'s
  `HUSHRUN_FORCE_FULL` by their flags' rules, so an empty value, or a boolean written `1` or
  `yes`, made a command exit 2 before anything ran, any command for the first four: a CI job's
  `HUSHRUN_NO_CI_OUTPUT: ${{ vars.X }}` with `X` unset or `1` stopped `hushrun run` from running
  its check. hushrun now reads its variables itself, once the command line is parsed. Contract
  changes first:
  - An empty variable is unset. A boolean is `true` or `false`, `1` or `0`, `yes` or `no`, `on`
    or `off`, in any case. Any other value is ignored with one line on stderr, such as `hushrun:
    HUSHRUN_FORMAT is "xml", not text, json, env or dotenv, so it's ignored` (`-q` silences it),
    and the command goes on with the default. A flag is as strict as before: `--format xml`
    exits 2.
  - The same holds for `HUSHRUN_PROVIDER` (one hushrun doesn't know leaves the provider to
    detection) and `HUSHRUN_RUN_ATTEMPT` or `GITHUB_RUN_ATTEMPT` (one that isn't a positive whole
    number leaves the attempt unknown), which made every command but `hushrun run` without a
    stage exit 2; and for `HUSHRUN_LOG`, which, empty, hid every warning.
  - `--config-ref` wins over `HUSHRUN_CONFIG`, as a flag does, where the two together exited 2.
  - A variable that isn't UTF-8, whatever its name, is ignored: `hushrun run` panicked on one
    after its command ran (exit 101), instead of exiting with the command's code.
- **A script's name names its check** (C-9). Without NAME, `hushrun run -- COMMAND` took the
  package.json script's name (`npm_lifecycle_event`) only when it already was a valid name, so
  `test:unit`, `format:check` or `Lint` silently named their checks after the program, such as
  `vitest`, while `hushrun init` says each check's name is its script's. Contract change: a
  script's name is made a name as a program's is (ASCII lowercase, what comes before its first
  letter dropped, each run of other characters one `-`, 63 characters at most), so `test:unit`
  names `test-unit` and `format:check` names `format-check`. The run keys of such scripts' checks
  change with their names: their first run after the upgrade runs. `npx` still names nothing,
  and a script that leaves no name, such as `123`, still leaves it to the program.
- **The GitLab setup `hushrun init --pipeline` prints works** (L-10, T-12). It says to store an
  agent token and its machine key as the project's variables, but in CI hushrun sent
  `HUSHRUN_TOKEN` to no repository URL that only `hushrun.yaml` named, and signed with the machine
  key only with `HUSHRUN_SIGNING=machine`. Contract changes first:
  - In CI too, an agent token (`hra_…`) in `HUSHRUN_TOKEN` goes to the repository's URL when it is
    on hushrun's host (`HUSHRUN_HOST`, by default `https://api.hushrun.com`), whoever names
    the URL, as it already did outside CI: the server there takes it for its org only. A GitHub
    Actions job's ID token still comes first, and no other token, such as a `HUSHRUN_TOKEN` that
    isn't an agent's, follows a URL that only `hushrun.yaml` names.
  - Without `HUSHRUN_SIGNING` or a key file, only a GitHub Actions job with an ID token signs
    keylessly by default, as the repository's CI. Otherwise a machine key signs
    (`HUSHRUN_MACHINE_KEY`, `HUSHRUN_MACHINE_KEY_FILE` or the one beside the credentials file),
    in CI too, such as an agent's in a GitLab pipeline, whose events count as `agent:<name>`'s.
    In CI only a key the environment names counts: never one a person's sign-in left on a
    runner.
    `SIGSTORE_ID_TOKEN` signs keylessly without a machine key, or with `HUSHRUN_SIGNING=sigstore`.
    A CI job with no signer is told how to get one.
  - init's GitLab steps make the token for a year (`hushrun token create --agent gitlab-ci
    --with-key --expires 365d`; a token lasts 30 days by default), store it as `HUSHRUN_TOKEN`
    and the key as `HUSHRUN_MACHINE_KEY_FILE`, a protected File variable, since GitLab can't mask
    a key, keep `HUSHRUN_GITLAB_TOKEN` (`read_api`) for pipelines' liveness, and say that a
    GitLab project can't use CI-only stages, since its pipeline signs as an agent. The steps
    name no spec rules, and are numbered without the gap GitHub's had.
  - The recipe, `recipes/gitlab/.gitlab-ci.yml`, and the GitLab guide take that one setup:
    `HUSHRUN_REPOSITORY_URL`, `HUSHRUN_REPOSITORY_TOKEN` and the per-stage keys mapped to
    `HUSHRUN_SIGNING_KEY_FILE` are gone from them.
  - `doctor` checks the signer of a job with `HUSHRUN_MACHINE_KEY` or
    `HUSHRUN_MACHINE_KEY_FILE`, and a job that can't sign keylessly is no longer warned on every
    command that the server doesn't know the repository's id.

### Added

- **init's plan names the repository's ledger before it exists** (C-12, C-15, ADR 0026 item 19).
  Contract change first: `POST {base}/v0/ledgers` takes `dry_run`, which makes nothing and
  answers `"dry_run": true` and `"exists"`, and `default_branch`, the repository's default
  branch, which every answer echoes. Once signed in, init asks with `dry_run`, and the plan says
  `finds <id>'s ledger, <name>: <page>`, `makes <id>'s ledger, <name>, for promoting its builds:
  <URL>`, or the backend's refusal; applying sends the default branch with the request.
- **Machines registered at sign-in** (C-18, ADR 0028). Contract changes first:
  - New environment variables: `HUSHRUN_MACHINE_KEY`, the machine key's PEM, for sandboxes that
    keep no files, and `HUSHRUN_MACHINE_KEY_FILE`, another path for it. `HUSHRUN_SIGNING`
    takes `machine` (T-12).
  - The device flow (C-13) sends `"machine": {"key", "name"}` with `POST {origin}/cli/login` and
    a `proof` with `POST {origin}/cli/login/token`, the key's signature over
    `hushrun-machine/v1\n<device_code>\n<keyid>\n`. The credentials file keeps the registered
    key id as `machine`, and login says `Registered this machine, <name> (<key id>), for its
    writes to <origin>.` A server that answers no machine signs people in as before.
  - An optional request of `hushrun-results/v0`: `POST {base}/v0/machines` `{"key", "name",
    "proof"}`, the proof over the hex SHA-256 of the token, for a login from before this release
    and for an agent token, once per token, remembered in the cache directory.
  - A result recorded with a personal or agent token carries `Hushrun-Signature: <keyid>
    <base64>`, over `hushrun-result/v1\n<key>\n<hex SHA-256 of the body>\n`. A server without
    machines gets unsigned results, and servers accept unsigned ones for this release only.
  - `hushrun.whoami/v1` gains `machine`, the key id and where the key is; `hushrun.token/v1`
    gains `machine_key` (`pem`, `public_key`, `keyid`) from `create --with-key`.
  - The key is ECDSA P-256, made the first time the machine signs in, at `machine-key.pem` beside
    the credentials file (0600, written through a rename). hushrun refuses one inside the
    repository it runs in.
- **A person's machine signs with its own key** (T-12, ADR 0028 item 5): outside CI, without
  `HUSHRUN_SIGNING` or a key file, events and approvals are signed with the key this machine's
  sign-in registered, so nobody's email goes to Sigstore's public log. Outside CI, a ledger on the
  origin you signed in to gets that login's token, and an agent token goes to ledgers on hushrun's
  host (L-10). The server writes such an event only when a registered machine of the token's
  owner signed it, and it counts while the machine does (T-15).
- **`hushrun token create --with-key`** (C-17, C-18): also makes the agent's machine key,
  registers it with the new token as `agent:<name>`, and shows it once: on stderr, to set as
  `HUSHRUN_MACHINE_KEY`, with its public key, or as `machine_key` in JSON. Every sandbox of the
  agent signs as one machine, which counts while the token does.
- **`hushrun whoami`** (C-17, ADR 0027): the backend `hushrun run` uses here and what named it,
  the credential that goes there, or why `HUSHRUN_TOKEN` is withheld, and who the backend says
  it is (`GET {base}/v0`), with an agent token's agent, repository, expiry and limit. It exits 1
  when the backend refuses the credential and 6 when it can't be reached; `--format json`
  prints `hushrun.whoami/v1`. `hushrun doctor` shows the same as its `results` check, before it
  reads hushrun.yaml, so it says something useful in a repository that only uses `hushrun run`.
- **`hushrun token create|list|revoke`** (C-17, ADR 0027): an org admin's agent tokens, through
  the org's backend (`{base}/v0/agent-tokens`, optional in `hushrun-results/v0`) with their
  login, never `HUSHRUN_TOKEN`. `create --agent NAME` is for the repository hushrun runs in
  unless `--repository` or `--org-wide` says otherwise, lasts `--expires` (whole hours, 1h to
  365d, 30d by default) and records at most `--rate` results a minute (the server's default
  otherwise). It prints the token alone on stdout, and on stderr what it is and the sandbox's
  environment. `list` shows the tokens that aren't revoked; `revoke` takes an id, a prefix, the
  token, or an agent's name that only one token has. `--format json` prints `hushrun.token/v1`.
- **Agent tokens in `HUSHRUN_TOKEN`** (C-10, C-12, C-13, ADR 0027). Contract changes first:
  - An agent token (`hra_…`) in `HUSHRUN_TOKEN` also goes to the repository's or the login's
    backend when that backend is on hushrun's host, `HUSHRUN_HOST` or
    `https://api.hushrun.com`, so a sandbox sets one variable. Other tokens keep ADR 0023's
    rule, and the line that says a token is withheld now names hushrun's host.
  - `hushrun.run-result/v0` gains an optional `invokedBy`, from `HUSHRUN_INVOKED_BY`
    (`agent:<id>` or `human:<id>`): a label that tells apart the runs of a fleet sharing a token.
    A value of another form is left out, with a line.
  - Optional parts of `hushrun-results/v0`: `GET {base}/v0`'s `token`, which describes an agent
    token, and 429 on a write from a token over its limit.
- **`hushrun init` sets up `hushrun run`** (spec C-15, ADR 0024). Contract changes first:
  - New flags: `--yes`/`-y`, `--no-browser`, `--dry-run`, `--run`, `--no-run`. Without a
    terminal, init applies nothing without `--yes`, prints its plan and exits 2.
  - `HUSHRUN_NPM_PACKAGE` names the package init adds to devDependencies (default
    `hushrun@^<version>`; an exact version gets the caret package managers save).
  - It finds the checks in the root package.json and the GitHub Actions jobs that run them,
    signs in with the org hint, shows every change and asks once. It wraps the checks as
    `hushrun run -- …`, names the org's backend in package.json, installs hushrun with the
    repository's package manager, gives those jobs `id-token: write`, connects the repository,
    and runs one check twice to show a skip. Edits keep each file's other bytes.
- **`hushrun init` gets the repository its ledger** (C-12, C-15, ADR 0025). Contract changes
  first:
  - An optional request of `hushrun-results/v0`: `POST {base}/v0/ledgers` `{"repository",
    "github_repository_id"?}`, with a personal token, answers the org's ledger for the
    repository, 201 when the server made it and 200 when the org had it.
  - init's plan shows the org's changes, the repository connected and its ledger, under the
    org's name, and asks `Apply?` when only those change; a repository already set up signs in
    and gets its ledger.
  - After connecting the repository, init asks for its ledger, with GitHub's id for it from
    `gh api repos/<owner>/<name> --jq .id` when `gh` can tell, read while the sign-in waits.
    It prints the ledger and its page; a member who may not make one, a backend without the
    request, or an error is a warning, and never fails init.
- **What CI repeats** (C-16): with `gh` signed in, `hushrun init` prints how often the last 30
  days of workflow runs re-tested a tree the same workflow had already passed, such as "Last 30
  days, CI ran 96 times on code it had already passed: about 11 h."
- **`hushrun run [NAME] -- COMMAND`**: NAME is optional (C-9). Without it, the check is named
  after the package.json script that runs it (`npm_lifecycle_event`, not `npx`), else after
  COMMAND's program. So `"test": "hushrun run -- vitest run"` names its check `test`.
- **Optional parts of `hushrun-results/v0`** (C-12): `GET {base}/v0` may name the org's
  `dashboard`; `POST {base}/v0/repositories` `{"repository"}`, with a personal token, lets a
  repository's CI record results with its own ID token: no secret, no GitHub App.
- **hushrun on npm** (`npm/`): `hushrun`, whose `hushrun` command runs the binary of one of six
  platform packages (`hushrun-darwin-arm64`, `-darwin-x64`, `-linux-arm64`, `-linux-x64`,
  `-win32-arm64`, `-win32-x64`), optional dependencies npm installs only on their platform.
  Arguments, stdio, the exit code and signals pass through. It sets `HUSHRUN_NPM_PACKAGE` to
  `hushrun@<version>` unless set, and `HUSHRUN_BINARY` names a binary where it has none.
  `npm/build.mjs` builds the seven packages from a release's archives, checked against their
  checksums, or with `--local` one tarball for tests without a registry; the `npm` workflow
  publishes them after a release, with provenance.

- The run guide's **Agents in sandboxes**: copy-paste setup for Claude Code cloud and for a
  container, fleets (one token, a label each, the limit), CI on other providers, a backend on
  another host, and what each refusal means. The agents guide points there for results and
  keeps promotion's evidence as it was. Spec C-10 to C-14 and the new C-17 describe it all.

### Changed

- **People read "repository", not "ledger"**. What hushrun prints (init, plan, doctor, verify,
  the configuration errors, `--help`, and the HTTP client's errors) calls the hosted record of a
  repository by the repository, its URL, or hushrun's server, as the dashboard does. The word
  stays in what is a contract: the `ledger` command, `--ledger`, `HUSHRUN_LEDGER*`, `ledger:`,
  `ledger_root` and `ledger_log` in hushrun.yaml, JSON keys and schemas, and `doctor`'s check
  names; so do the operator commands about the server itself (`monitor`, `witness`). The header
  comment of a hushrun.yaml that init writes now reads "where this repository's events go".

- **In CI, a job with an ID token signs keylessly by default** (T-12): without
  `HUSHRUN_SIGNING` or a key file, `sigstore` is the signer.
- The guides (GitHub, GitLab, repository access, agents, run, witness) and the README describe
  membership as trust and machines registered at sign-in; ADRs 0028 and 0031 record the contract
  as built. The org guide is gone.

- **`hushrun run` says why it didn't record in one line** (C-11, C-14). A run that sent no token
  and was refused for it says nothing about it until the command ends, doesn't try to record,
  and then prints one line: at a terminal the sign-in hint alone, elsewhere `hushrun: results
  aren't shared with <org>: <the server's reason>`. Until now a teammate without a login saw two
  long 401 lines, for the lookup and for the record, next to the hint. A credential the lookup
  is refused (401, 403) is said once, `<check> running: <org> refused HUSHRUN_TOKEN: …`, and the
  record isn't tried; a record refused for another repository (403) or for a token over its
  limit (429) ends the run with `<check> passed in 41s; not recorded: <reason>`.
- **Where `hushrun run` sends results** (C-11, ADR 0023). Contract changes first:
  - A repository can name its backend in its root package.json, `"hushrun": {"backend":
    "<URL>"}`. It comes after `HUSHRUN_BACKEND` and before the backend `hushrun login` saved, so
    CI, teammates and agents need no variable.
  - `HUSHRUN_TOKEN` goes only to the backend `HUSHRUN_BACKEND` names, no longer to the
    repository's or the login's; hushrun says when it withholds it.
  - When the repository's backend refuses a request without a token, the command runs as before,
    and at a terminal hushrun ends with `hushrun: sign in to share results with <org>: npx
    hushrun login`.
- **`hushrun login [URL]`** (C-13): with no URL, `HUSHRUN_BACKEND`, repository backend or earlier
  login, it signs in to the hosted backend, `https://api.hushrun.com` (`HUSHRUN_HOST`
  overrides it), instead of exiting 2, and says which backend it signs in to. An org URL sends
  its org as a hint and stays the backend; otherwise the org the sign-in was approved for
  becomes it. A server that names no org leaves results on this machine, with a warning.
  `hushrun logout` without a URL signs out of the backend `hushrun login` would sign in to.
- **`hushrun init` without flags no longer writes `hushrun.yaml` and keys**: that init is now
  `hushrun init --pipeline`, implied by `--force`, `--keys-dir`, `--repository`,
  `--staging-branch`, `--production-branch`, `--staging-image` and `--production-image`, so
  existing invocations still work. A command that needs `hushrun.yaml` and finds none now says
  `hushrun init --pipeline` writes a starter one.
- The GitHub guide sends someone who wants to skip checks to `npx hushrun init` first, and
  its ledger step takes the admin token from the ledger's Connect page: hushrun cloud's
  Promotion setup, which it named, is gone. ADR 0025's item 7 and RFC 0002 no longer describe
  that guide as current either.

### Removed


- **Only `https://` ledgers** (spec L-1, ADR 0022). Contract changes first:
  - `gs://`, `s3://`, `az://`, `file://`, `mem://` and `oci://` ledger URLs fail to parse with
    "only https:// ledgers are supported (the hosted ledger)", wherever a ledger URL is read:
    `hushrun.yaml`'s `ledger`, `HUSHRUN_LEDGER`, `HUSHRUN_LEDGER_BASE`, `--ledger`,
    `HUSHRUN_BACKEND` and `hushrun login`. `http://` stays for loopback hosts. The `hushrun.v1`
    schema matches.
  - HTTP ledgers' rules now apply everywhere: `mark deployed` needs the `started` of
    `plan --start` (L-11), and GC on a ledger with a log signs each deletion (L-9).
  - `HUSHRUN_LEDGER_TOKEN_COMMAND` is for `https://` ledgers only.
  - `hushrun run`'s results stay in the machine's cache unless `HUSHRUN_BACKEND` or
    `hushrun login` names an `https://` server; a shared directory is no longer a backend.
  - The reusable workflow loses its `azure-storage-account` input.
  - Gone with them: the object-store backend and its credentials, the `it` feature of
    `hushrun-ledger` and its conformance runs on emulators and on GCS, the Terraform modules in
    `deploy/terraform/`, `tools/azurite-container.py`, and the `object-stores` and `terraform` CI
    jobs.
- **KMS signers** (spec T-12, E-2, ADR 0022): `HUSHRUN_SIGNING=kms`, `HUSHRUN_SIGNING_KMS`
  (`gcpkms://`, `awskms://`, `azurekms://`), `hushrun keys public --kms`, and the reusable
  workflow's `signing-kms` input. Events are signed with a key file, Ed25519 or
  P-256, or keylessly through Sigstore. `object_store` leaves the build, with the GCP and AWS
  credential chains it provided.

### Changed

- **GitLab jobs record their runs as `gitlab`** (§11.2, E-4, M9), not `generic`. The run is
  `CI_PIPELINE_ID`, with `CI_PIPELINE_URL`, `CI_JOB_NAME` and, new in events, `jobId`
  (`CI_JOB_ID`); `HUSHRUN_RUN_ID` no longer applies on GitLab, and the attempt comes from the plan
  or `HUSHRUN_RUN_ATTEMPT`. Attempts recorded as `generic` before the upgrade stay separate runs.
  The repository comes from `CI_PROJECT_URL`, so `HUSHRUN_LEDGER_BASE` now works on GitLab.
- A run is never its own pending upstream run (P-7), on GitHub too: a plan whose upstream stage
  runs in the same pipeline or workflow no longer waits for itself.
- `--dotenv` and `--format dotenv` refuse output of 5 KiB or more (GitLab wants less than its
  5 KiB limit) and keys that aren't ASCII letters, digits and `_`.
- **M8: local and agent evidence** (spec E-12, T-5, ADR 0018). Contract changes
  first:
  - `hushrun.run/v1` adds `isolated`, and per check `custody` and `environment`. Both are
    additive.
  - **Pull-request pipelines must read the base branch's `hushrun.yaml`** (T-5): on GitHub's
    `pull_request` and `pull_request_target` events and GitLab's merge request pipelines, commands
    that read or write the ledger exit 2 without `--config-ref <base branch>`.
  - `HUSHRUN_CUSTODY=isolated` on anything but `run --isolated` records custody `local`, with a
    warning (E-4).
  - New environment variables: `HUSHRUN_CONTAINER_RUNTIME`, `HUSHRUN_RESTRICTED_NETWORK`,
    `HUSHRUN_EXPORT_DIR`, `HUSHRUN_INVOKED_BY`, `HUSHRUN_OIDC_ISSUER`, `HUSHRUN_OIDC_CLIENT_ID`;
    `BROWSER` is honored for signing in.

- **Events are Sigstore bundles** (spec E-1, M10). Every event is written as a bundle
  (`application/vnd.dev.sigstore.bundle.v0.3+json`) around the same DSSE envelope, with one
  signature, the key's id or the signing certificate, and an RFC 3161 timestamp when an
  authority answers. Readers accept plain envelopes, which every older event is, and bundles.
  Hushrun's readers older than this release can't read bundles: upgrade every job that reads the
  ledger, and the index, before any job that writes.
- An event's `time` is when it was signed (E-4), not when the command started.
- **Exit code 9**: the signer is unavailable or refused (the ID token endpoint, Fulcio, every
  timestamp authority for keyless signing, the KMS, or the TUF repository during a refresh).
- **Key signers ask for timestamps by default**, from Sigstore's timestamp authority; when none
  answers, the event goes out without one, with a warning. `HUSHRUN_TSA=none` opts out.
- `hushrun verify` (`hushrun.verify/v1`) gains each event's `signature` (`machine` or
  `identity`, with the certificate's identity) and `trustedTime`; its text and `ledger show` say how each
  event was signed and when it was timestamped.
- `hushrun doctor`'s `signing key` check is now `signer`, for keys, KMS keys and keyless
  identities, and it adds `sigstore trust root` and `timestamps`.
- `hushrun keys public` prints a KMS key's public key (`--kms`) or the configured signer's;
  `hushrun.keys/v1`'s `publicKey` may be `ecdsa-p256:…`.
- Exit codes and configuration (T-4): with `HUSHRUN_LEDGER_BASE` in CI, `hushrun.yaml`'s
  `repository` must be the CI provider's exactly, and its `ledger` the derived one, or commands
  exit 2 (a mismatched repository was a warning).
- The recipes in `recipes/github/` are now short callers of the reusable workflow.
- The deployed-index key parser refuses a sign in the 13-digit timestamp (L-2).
- `hushrun doctor`'s `ledger log` check compares `ledger_log.origin` with the origin the ledger
  says its log has, which stays the same when hushrun reaches the ledger by another URL, such as
  a base-derived one (T-4, T-9).

### Added

- **`hushrun run NAME -- COMMAND` without `--stage`** runs COMMAND unless this exact code
  already passed it, and remembers the result (spec §7.6, C-9 to C-14; `docs/guides/run.md` shows
  how to add it). It needs no `hushrun.yaml`, keys or ledger, and
  works the same on a laptop, in CI and in an agent's sandbox, so `"test": "hushrun run unit --
  vitest run"` in `package.json` skips the same way everywhere. The code is the repository's
  files as they are now, ignored files left out; the key also covers NAME, the command and the
  directory it runs in, but not the machine. Results go to `HUSHRUN_BACKEND`, else to the
  backend `hushrun login` signed in to, else to `<cache dir>/results`; an empty `HUSHRUN_BACKEND`
  turns them off. hushrun runs the command whenever it can't tell (outside a repository, an
  unreadable or unreachable backend), records nothing when files change during the run, and exits
  with the command's code (127 when it can't be found). `--stage` is now optional; with it, `run`
  is unchanged.
- **Results backends** (C-11, C-12): a directory (`file:///abs/path`), or a server that speaks
  `hushrun-results/v0` at an org's URL, such as `https://host/o/acme`: `GET {base}/v0/results/{key}`
  answers the newest result for a run key, `POST` records one, and `GET {base}/v0` says who the
  caller is. Requests carry `HUSHRUN_TOKEN`, else the token `hushrun login` saved for the host,
  else in GitHub Actions an ID token whose audience is the base URL; a server gets 10 seconds a run.
- **Skips are recorded too**, as results whose `result` is `skip`, with `savedMs`: how long the
  run they stood in for took. They never answer a lookup; a directory keeps them apart, under
  `skips/v0/`. Every result also names its `repository` (`host/owner/name`), from the CI provider,
  `hushrun.yaml`, or the `origin` remote, so the dashboard can tell repositories apart (C-10; the
  record's schema is `docs/schemas/run-result.v0.schema.json`).
- **`hushrun login [URL]` and `hushrun logout`.** Login asks the host for a code, opens the page
  that approves it (`--no-browser` only prints it), and keeps the personal token it collects in
  `<config dir>/hushrun/credentials.json` (or `HUSHRUN_CREDENTIALS`), readable by its owner only.
  Logout revokes the token on the host and forgets it (C-13).
- `hushrun_ledger::HttpLedger::json_request`: a JSON request to another of hushrun's endpoints on
  the same host, sent like ledger requests (same origin only, the token, retries).
- **GitLab CI** (M9, ADR 0019): run identity and triggers from `CI_PIPELINE_SOURCE`; liveness of
  staging's pipelines and their pending pipelines with `HUSHRUN_GITLAB_TOKEN` (a project access
  token with `read_api`), mapped onto P-6 (`waiting_for_callback` included); `ci.gitlab.ref` (C-7)
  for an upstream whose pipelines run on another ref than its branch.
- Keyless signing on GitLab (T-10, T-12): the job's `id_tokens` entry `SIGSTORE_ID_TOKEN` with
  `aud: sigstore`, checked for its audience and expiry; identities match GitLab's Build Config
  URI (`…//.gitlab-ci.yml@ref`) with `workflow: .gitlab-ci.yml`.
- `hushrun doctor` on GitLab: the API, protected stage branches, who may set pipeline variables
  (T-4), the identity Fulcio would certify for the job, and `.gitlab-ci.yml` in every scope.
  `hushrun init` writes GitLab's next steps for GitLab projects.
- `recipes/gitlab/.gitlab-ci.yml`, `recipes/gitlab/install-hushrun.sh` (an exact version,
  checked against a pinned checksum), the container image `ghcr.io/hushrun/hushrun:<version>`
  (`deploy/image/`), and the `publish` workflow that builds it and attaches the pinned script to
  each release; `cargo xtask installer-pins` writes the pins; `cargo xtask e2e --gitlab`;
  [docs/guides/gitlab.md](docs/guides/gitlab.md).
- `hushrun run --isolated` (E-12): each check runs in its pinned image and platform with Docker
  or Podman, on an export of exactly the digested files, with no network unless it declares
  `open` or `restricted`, only its `env` variables, never a secret, and never the signing key.
  Its evidence records the environment and claims custody `isolated` (or `sandbox` on an
  operator's ephemeral runner). The reusable workflow's `isolated` input runs CI's checks the
  same way. A check that exits 125 is told from the runtime's own failure by the container's
  state; Ctrl-C, SIGTERM or SIGHUP removes the container and the export and records nothing;
  a signing key file among the exported files refuses the run. Adds the signal-hook crate
  (pure Rust over libc) for the signals.
- `--worktree` on `plan`, `run`, `attest`, `mark`, `guard`, `status`, `approve` and
  `artifact resolve` (D-10).
- `--config-ref <ref>` reads `hushrun.yaml` from a git ref (T-5).
- `hushrun approve <check> --stage T [--comment C] [--out FILE]` (`hushrun.approve/v1`): a
  person's signed approval (P-3), with their key, a KMS key, or as themselves: keyless signing
  on a person's machine signs them in with their browser (OAuth with PKCE at Sigstore's
  issuer). It shows what is approved, and records nothing that wouldn't count (T-2); `--out`
  writes the signed event for the dashboard to relay.
- [The agent guide](docs/guides/agents.md) and the `AGENTS.md` snippet in `recipes/agents/`.
- The testkit's fake container runtime and fake OIDC issuer; `crates/hushrun/tests/isolated_docker.rs`
  runs isolated checks with a real runtime behind `it`.

- Keyless signing with Sigstore (spec E-11, T-12, M10): `HUSHRUN_SIGNING=sigstore` signs with an
  ephemeral P-256 key that Fulcio (`HUSHRUN_FULCIO_URL`, public-good by default) certifies for
  GitHub Actions' ID token (`id-token: write`) or `SIGSTORE_ID_TOKEN`. Certificates are checked
  against the Sigstore trust root, with their SCTs.
- RFC 3161 timestamps on every signature (E-10), from the authorities `HUSHRUN_TSA` lists, tried
  in order; `time` must be within 10 minutes of the trusted time.
- ECDSA P-256 keys (E-2): `ecdsa-p256:<base64 of the SubjectPublicKeyInfo>`, key ids over those
  bytes; `hushrun keys generate --algorithm ecdsa-p256`; PKCS#8 keys OpenSSL writes.
- KMS signers (E-2, T-12): `HUSHRUN_SIGNING_KMS` with `gcpkms://`, `awskms:///` or
  `azurekms://` P-256 keys, credentials from the ledgers' chains (and Entra for Key Vault).
- `hushrun sigstore refresh` (`hushrun.sigstore.refresh/v1`): a TUF update of the Sigstore trust
  root into `$HUSHRUN_CACHE_DIR/sigstore/`, which readers verify again offline.
  `HUSHRUN_SIGSTORE_TRUSTED_ROOT`, `HUSHRUN_SIGSTORE_TUF_URL` and `HUSHRUN_SIGSTORE_TUF_ROOT`
  name other instances. The public-good trust root is vendored, with the TUF metadata that pins
  it.
- `hushrun_attest::verify_with`, and `Verified::signed_by` and `trusted_time`, for the control
  plane's server; `hushrun_ledger::credentials` exposes the GCP and AWS credential chains.
- The testkit's fake Sigstore (ID tokens, Fulcio, a timestamp authority) and fake KMS; the
  Python reference reads and writes bundles.
- The GitHub guide's keyless signing. ADR 0013.
- The hosted ledger's transparency log, H1 (spec L-11, ADR 0017, RFC 0001 §4.7). Contract
  changes first:
  - Events gain an optional `checkpoint` (E-4): the newest checkpoint of an HTTP ledger's log
    that the writer verified. `started` gains an optional `listings` (E-5): the signed listings
    the plan decided from. Readers that don't know them ignore them (E-8 step 3).
  - `hushrun.yaml` gains `transparency_log: {origin, key}` (T-9): the public half of the
    server's log key, an Ed25519 key that must not be a machine's.
  - With `transparency_log`, every create on an `https://` ledger must come with a receipt that proves
    it is in the log, every listing with the ledger's signed statement, and every checkpoint a
    command meets, its own or one inside an event, must be one history with the others;
    otherwise exit 6, which for two histories names both signed checkpoints. A listed key the
    ledger won't serve, or a rejected event's bytes the log doesn't hold, exit 6 too. Without
    it, hushrun warns once per command and checks nothing (the migration).
  - `plan --start` records its listings in its `started` events, and on an `https://` ledger
    `mark deployed` needs that `started` (exit 2 otherwise), as the recipes already do.
  - `hushrun ledger monitor [--state FILE]` (`hushrun.ledger.monitor/v1`,
    `docs/schemas/ledger-monitor.v1.schema.json`): replays an https ledger's log and checks every
    checkpoint and recorded listing against it; exit 5 when the ledger contradicts its log, with
    its signed statement as proof.
  - `repository gc`: on an https ledger that keeps a log, each deletion carries a signed GC
    statement (L-9), which the tombstone's leaf records.
  - `doctor` checks the log (`transparency log`): that `hushrun.yaml` names its key, and that the
    latest checkpoint verifies; it prints the `transparency_log` lines to add.
- The hosted ledger's transparency log, H2 (spec L-12, T-13, ADR 0020, RFC 0001 §4.7): an
  incomplete listing is refused before the decision, and a second history needs witnesses to
  collude. Contract changes first:
  - Logs gain a leaf kind, `map`, at the end of every batch: the root of the prefix map, which
    commits to the sorted keys of every `v1/events/<scope>/<digest>/` and
    `v1/deployed/<scope>/<stage>/`. Monitors from before this release stop at the first one
    (exit 6): upgrade them.
  - `transparency_log` gains `witnesses` (C2SP cosigner keys and their URLs), `quorum`, and
    `max_age` (T-13): the witnesses readers require. The server holds those it submits
    checkpoints to.
  - With witnesses in `hushrun.yaml`, every checkpoint a command reads at needs recent cosignatures by a
    quorum of them, and every listing of a map prefix a proof against the map its checkpoint
    commits to; otherwise exit 6. Without them, H1's checks and a warning once per command.
  - `hushrun ledger monitor` checks every map leaf (`map_mismatch`, an error), and reports the
    latest checkpoint's cosignatures (`witnesses`; `unwitnessed_checkpoint` and
    `unmapped_checkpoint`, warnings); `checked.maps` counts the maps. With witnesses in `hushrun.yaml`,
    a latest checkpoint without a recent quorum makes it exit 6 and keep its `--state`, after
    checking everything else.
  - `hushrun witness init` and `hushrun witness serve` (`hushrun.witness/v1`): a minimal C2SP
    witness a team runs itself, with its state in `hushrun.witness.state/v1`.
  - `doctor` adds `ledger witnesses`, and warns when `quorum` is half the witnesses or fewer.
  - A listing read at an older checkpoint than one the command already verified is refused even
    when that checkpoint is one the command saw before (L-11).
  - `attest --check` alone now configures the log on a ledger that keeps one, so its receipts
    are checked and its events carry a checkpoint (L-11, E-4); before, they weren't.
- Witnesses that verify the map (spec L-12, T-13, ADR 0021): a ledger that commits to a map
  leaving events out is refused before the decision too, where the monitor only finds it
  afterwards. Contract changes first:
  - `hushrun witness serve --verify-map [--ledger-url URL…]` reads each log's leaves from its
    ledger, with a reader token, keeps a replica of the log beside `--state` (the state file's
    `maps`, and a journal of keys in `<state>.maps/`), and cosigns a checkpoint only when the
    map it commits to follows from them. It answers 422 to a map that doesn't, and to two trees
    of one size, with evidence (`hushrun.witness.evidence/v1`) it prints and keeps, and it
    cosigns nothing more of that log; 503 when it can't check yet. Once it checked a log's
    maps, it refuses to serve that log without `--verify-map`.
  - `log/leaves` takes `size=`: the leaves of that tree, which may be the newest signed
    checkpoint before its witnesses had their turn (L-11).
  - Witnesses take `verify_map: true`, and `transparency_log` takes `map_quorum`: how
    many of the counted cosignatures must be by such witnesses (1 by default when one is
    named). Readers then need both counts (exit 6 otherwise); the monitor's `witnesses` gains
    `mapQuorum` and `mapFresh`, and `doctor` counts them. Hushrun versions before this one
    refuse both keys.
  - `HUSHRUN_LEDGER_TOKEN_COMMAND` now runs with `HUSHRUN_LEDGER_URL` set to the ledger's URL,
    for HTTP ledgers (L-10), so one command can print each ledger's token.
- `hushrun-tlog` gains `replica`, a witness's replica of a log, and the testkit's fake ledger
  witnesses that verify maps, `log/leaves?size=`, and leaves that don't hash to the checkpoint.
- `hushrun-tlog` gains the prefix map (a compacted sparse Merkle tree), range proofs of key trees,
  C2SP `tlog-cosignature` and `tlog-witness`; `tools/reftlog.py` computes their vectors from the
  definitions. The testkit's fake ledger keeps maps, has witnesses that are honest or collude, and
  can commit to a map that lies.
- `hushrun-tlog`, a new crate on ring only: RFC 6962 trees and proofs, C2SP signed notes and
  checkpoints, the signed-listing statement, and the monitor's replay. Its vectors come from
  `tools/reftlog.py`, an independent implementation, and match what Go's `sumdb/note` signs.
- `recipes/github/monitor.yml`: run the monitor every hour from your own CI.
- The testkit's fake HTTP ledger keeps a log, and can lie: hide keys from listings, show a token
  another history, or send receipts that prove nothing.
- `HUSHRUN_LEDGER_BASE` (T-4): one organization variable; each repository's ledger is
  `<base>/<repository id>`.
- `hushrun adopt` (`hushrun.adopt/v1`): a starter `hushrun.yaml` from the repository's language,
  Dockerfiles and deploy workflows, shaped by `--org`, and one workflow per stage that calls the
  reusable workflow; idempotent, with `--dry-run`.
- The reusable workflow `.github/workflows/hushrun-deploy.yml` (M14): the recipes' jobs, with
  build and deploy commands as inputs and cloud credentials through workload identity
  federation; `signing: sigstore` signs keylessly, as the calling workflow, and `signing: kms`
  with `signing-kms`.
- Terraform modules for ledgers on GCS, S3 and Azure in `deploy/terraform/` (L-6), with
  examples; CI validates them.
- ADR 0016, invariant I-9, and a CI job for Terraform.
- `https://` ledgers (spec L-1, L-10): the `hushrun-ledger-http/1` protocol, for the hosted
  ledger of RFC 0001 §4.7. Tokens come from `HUSHRUN_LEDGER_TOKEN` (new),
  `HUSHRUN_LEDGER_TOKEN_COMMAND`, or a GitHub Actions ID token whose audience is the ledger's
  URL; they are never logged, never sent to another host, and sent only to a ledger named in
  `HUSHRUN_LEDGER` or `--ledger`, never to one only `hushrun.yaml` names. `http://` works on
  loopback hosts only. Requests retry and time out as for cloud ledgers (L-4), honoring
  `Retry-After`.
- `hushrun doctor` checks an HTTP ledger's protocol, repository and URL (`ledger protocol` and
  `ledger url`), and its clock.
- The testkit's fake HTTP ledger; the ledger conformance suite, now in L-2's key shapes, runs on
  it too.
- Ledger layout: `plan --start` uploads the `hushrun.yaml` it evaluated to
  `v1/policies/<sha256 hex>.yaml` (L-2), named by the events' `policyDigest`, for the index.
  Readers that decide ignore it, its failures are warnings, and `ledger gc` keeps it.
- The GitHub Actions provider (M6): run identity and trigger, `$GITHUB_OUTPUT` with heredocs,
  step summaries and annotations, liveness of upstream runs and their queued runs through the
  REST API, matched to the digest by tree or by fetching the commit.
- `hushrun run` (JSON `hushrun.run/v1`): the plan's checks, or the stage's checks without valid
  evidence; retries, timeouts, commands after `--`, pinned images; exit 8 on failure (M6).
- `hushrun init` (`hushrun.init/v1`) and `hushrun doctor` (`hushrun.doctor/v1`) (M6).
- `action/setup`, recipes for staging, production and monorepos in `recipes/github/`, the
  GitHub guide, and `cargo xtask e2e` (M6).
- `hushrun artifact resolve` (A-1), with JSON output `hushrun.artifact.resolve/v1`: the
  reference to deploy for the plan's decision; copies by digest to the stage's location,
  records the copy (`origin: copied`), and falls back to a build with a permission hint (A-3)
  (M5).
- `hushrun artifact copy` and `hushrun artifact digest` (`hushrun.artifact.copy/v1`,
  `hushrun.artifact.digest/v1`) (M5).
- The OCI copy (A-2): OCI and Docker manifests and indexes, cross-repository mounts with a
  token scoped to both repositories, streamed and verified blobs, and digest checks after every
  push (M5).
- Registry credentials from the Docker configuration, with a timeout for credential helpers;
  `attest --artifact` and `mark --artifact` pin tags through the registry (M5).
- Cloud ledgers (M4): `gs://`, `s3://` and `az://` through `object_store`, with 10 s request
  timeouts, retries with jittered backoff, a 60 s budget for `plan`'s requests, and TLS on
  rustls with ring only.
- GCS workload identity federation (`external_account` credentials), AWS profiles with static
  keys, and `HUSHRUN_LEDGER_TOKEN_COMMAND` for any other bearer token (M4).
- `hushrun ledger gc --older-than <duration> [--dry-run]`, JSON output `hushrun.ledger.gc/v1`
  (M4).
- The ledger conformance suite runs on file, mem, an in-memory object store, and, in CI, on S3
  and Azure emulators and a GCS emulator; nightly on a real GCS bucket (M4).
- `docs/guides/ledger-access.md`: append-only access for CI on GCS, S3 and Azure (L-6) (M4).
- The planner (spec §8): `promote`, `refresh`, `full`, `wait`, `skip` and `blocked` from verified
  evidence, run attempts, upstream state, and reusable artifacts; a pure function, tested by a
  scenarios and property tests of I-1…I-8 (M3).
- `hushrun plan` with `--start`, `--lease`, `--wait-timeout`, upstream grace, `--out` and
  `--explain-json`; JSON output `hushrun.plan/v1` and the env keys of §10.2; exit 3 when a scope
  is blocked (M3).
- The plan as the single source: `--plan FILE` or `HUSHRUN_PLAN_JSON` for `attest`, `mark` and
  `guard`, with the revision check (exit 2), the run identity and `forced` taken from the plan
  (M3).
- `hushrun guard` (stale-revision guard, exit 4 with `--on-stale fail`) and `hushrun status`
  (`hushrun.status/v1`) (M3).
- Event format `event/v1`: DSSE envelopes (Ed25519) around in-toto Statements whose subject is
  the scope's source digest; the ledger layout `v1/events/…` and `v1/deployed/…` (M2).
- `hushrun keys`, `attest`, `mark`, `verify`, `ledger ls|show` and
  `policy validate|explain`, with JSON outputs `hushrun.keys/v1`, `hushrun.attest/v1`,
  `hushrun.mark/v1`, `hushrun.verify/v1` (M2).
- Full `hushrun.yaml` validation with `file:line:column` errors, a JSON Schema, and the `file://` ledger (M2).
- `hushrun digest`: source digest v1 of each scope, from a git revision (gix or the git CLI),
  the worktree as it would be committed, or a plain directory; `--explain`, `--manifest-out`,
  and `text`, `json` (`hushrun.digest/v1`), `env` and `dotenv` output (M1).
- Content-addressed object and stat caches under `HUSHRUN_CACHE_DIR` (M1).
- `tools/refdigest.py`, an independent reference implementation, and golden vectors for digests
  and scope patterns (M1).
- Workspace, CI on Linux, macOS and Windows, and release builds for six targets (M0).
