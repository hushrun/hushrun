# 0077. A test as the unit: the core, and pytest

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** C-5, C-10, C-32, C-36, C-39; relies on C-30, C-31, C-33, C-34, C-38, ADR 0068, 0069,
  0072, 0075, RFC 0004

## Context

Test by test (ADR 0068, 0069) reuses each test file that already passed on the files it read. One
failing test runs its whole file again, and one test that starts a program costs its file's other
tests their reuse. RFC 0004 adds the test as a second, finer unit, and a second setting for what a
test is taken to have read. This decision is its core: the settings, a test's key, its states, two
tiers in the plan and in the record, the contract and the store that carry tests, the counts and
the lines, the limit, one interface for every adapter, and pytest as the adapter that knows tests.
What a test read with `reads: test`, hints with tests, and `hushrun why` and `hushrun ready` for
tests are later decisions; this one leaves them their seams.

## Decision

1. **Two settings** (C-5). `checks.<name>.tests.unit` is `file`, the default, or `test`;
   `tests.reads` is `file`, the default, or `test`, which needs `unit: test`, and `hushrun.yaml`'s
   validation says what to write otherwise. The short form, `tests: reuse`, means test files, as it
   did. Neither is part of the check's definition (E-6).
2. **A test** (C-39) is one named test of a test file: for pytest, its node id after `<path>::`,
   parameters included. Its name, everywhere people read it, is its unit, `::` and its id.
3. **A test's key** is the unit key's fields, computed the same way, under
   `hushrun.unit-key/v1`, with `test`, by one function beside the unit key's
   (`hushrun_core::units::test_key`). A test file's key is unchanged (`v0`), so every result
   recorded before counts as it did. With `reads: file`, a test read what its test file read.
4. **States, by the one function** (`hushrun_core::units::state`), from a test's own facts: it
   finished, it failed, it was deselected, and what wasn't followed inside its window and outside
   every window of its file. Something not followed inside one test's setup, body or teardown is
   that test's alone; at import, in a fixture of a wider scope, or in a teardown that may end one,
   it is the file's, and every test's. A test file some of whose tests were reused one by one
   didn't run whole: `not run`, so it records no pass of its own (RFC 0003's principle 2).
5. **Two tiers** (RFC 0004 §3.3). A plan asks each test file's own key first; a pass reuses all of
   it, as before. For each test file whose key isn't a pass, and none of whose tests was left
   unfollowed the last time it ran here, it keys each test this machine saw run by the same lists
   with its id, and the one rule (`units::shared::choose`) reuses a test whose newest result that
   counts is a pass and no list's is a fail. A run records a test file that passed whole as before,
   and nothing more; one that didn't run or pass whole records each test that ran under its own
   key, and its own fail when one failed. With `reads: file`, a test file some of whose tests were
   reused is keyed by what it read now and what it read when it last ran here: its reused tests
   didn't run, and what only they read is still its own. A larger list only makes keys finer.
6. **Audits** (C-34). Of the tests that could be reused one by one, 2%, at least one, run anyway,
   chosen by each test's key and the run's start. An audited test, or one listed that ran all the
   same, that fails runs once more after COMMAND, alone: its file runs with every other test of it
   listed in `reuseTests`, every other file in `reuse`. A second failure is a disagreement, and
   suspends the check's reuse, test files and tests alike.
7. **The contract** (C-32). `hushrun.json` gains `unit`, `reads` and `reuseTests`, the tests an
   adapter may deselect, by unit and id; its schema stays `hushrun.units/v0`, since an adapter that
   reads none of them deselects whole files only, which runs more. An adapter's file may be
   `hushrun.unit-facts/v1`: each unit gains `tests` (`id`, `finished`, `failed`, `deselected`,
   `durationMs`, and for `reads: test` `ran`, `opened` and `dirs`, indexes into the file's `paths`
   table, and `notFollowed`), `loaded` and `shared`; the file gains `paths` and `imports`. All of it
   is parsed and merged across an adapter's processes now: a test is deselected only when every
   process that reported it deselected it. A `tests` that doesn't read leaves the file unread, as
   a `v0` file that doesn't read is: its units record nothing.
8. **The store is schema 2** (C-36): `<cache dir>/units/<id>.v2.sqlite`, with each test file's
   tests (id, last state, duration, what wasn't followed inside it), and a table of lists that
   tests share, for `reads: test`, empty until then. A test reused keeps the row it stands on. An
   adapter is named by its word, any of `a-z`, `0-9` and `-`, so a new adapter needs no schema. A
   `v1` file is left for an older hushrun; init's cache step names `*.v2.sqlite`.
9. **Counts and lines** (C-10, RFC 0004 §1.2). `units` keeps counting test files, one reused when
   none of its tests ran; a run with `unit: test` adds `units.tests`, `{total, ran, reused}`. The
   line before COMMAND is `unit: 31 of 2,140 tests run, in 4 of 340 test files; 2,109 already
   passed on the files they read`, `observe`'s says what reusing tests would have done, and the
   last says what reusing tests saved. Coverage from a run that reused tests isn't kept (C-27).
10. **Where an adapter can't** (RFC 0004 §1.2). One that knows no test keeps working with
    `unit: test`: it reuses by test file and says so once, `its tests are reused by test binary:
    cargo's tests can't be reused one by one yet`. One that can't watch a test answers
    `reads: test` with `reads: file`, and says so once. pytest watches a test (ADR 0080) while no
    other tool uses `sys.monitoring`.
11. **The limit** (C-39). A check with more than 50,000 tests reuses by test file for that run, and
    says so.
12. **One interface for adapters.** The plan, the reading of what an adapter wrote, each unit's
    state and the record are one code for every adapter, which implements one trait
    (`units::adapter::Adapter`) in its own module and adds one function to one list
    (`units::adapter::RECOGNIZED`). A runner that takes nothing from the environment can be reached
    through a program on COMMAND's `PATH` that is hushrun in a hidden mode
    (`units::program`): it finds the real program after its own directory, changes its arguments as
    the adapter says, and passes the rest through, as the target runner does.
13. **pytest** reports each test with `unit: test`: its node id, whether it finished, failed or
    was deselected, its time, and what it couldn't follow inside its window. It deselects the tests
    `reuseTests` lists after collection and every other deselection, under ADR 0069's rules: one
    pytest of a run may, an xdist worker of that one too, only with the `python_files` hushrun read,
    and exit 5 becomes 0 only when what it deselected left nothing. With `unit: file` its file is
    exactly as before.

## Consequences

- With `reads: file`, a test reuses what its file would have, and more: the tests of a file that
  passed when one of them failed, and the tests of a file one of whose tests started a program.
  A change to a file the test file read runs every test of it, as before.
- Tests of a file that only pass together now run apart. `unit: file`, or `always` for that file,
  is the answer; the audit's second run catches a test that fails alone, and suspends reuse.
- A partly reused test file's list only grows until it runs whole, which makes its keys finer and
  never wrong.
- A test whose id changes (a renamed parameter) is a new test, and runs.
- Results grow with tests where test files don't pass whole. A run sends at most 50,000, failures
  first, in at most 8 MiB, and an older server keeps the first 5,000 (ADR 0078, item 9).

## Alternatives considered

- **Recording each test under its own key with `reads: file` when its file passed whole:** each key
  would say what the file's says, and multiply the results by the tests. Two tiers keep the keys
  near the number of test files.
- **A sixth state for a test file partly reused:** `not run`, "it didn't run whole", is what it is,
  and records nothing; the counts say it ran.
- **Keeping a partly reused file's list to what this run saw:** its reused tests' keys would stop
  matching the next run, and the file would never settle.
- **Bumping `hushrun.json`'s schema:** an adapter that doesn't read the new fields only runs more;
  a new schema would make every older plugin read nothing.
- **A fixed list of adapters in the store's CHECK:** every new adapter would need a schema.

## Amendment 1: completeness within one start of the runner (2026-11-12)

Item 7 merged every process of an adapter alike: a unit's `selected` the most any process said,
its `finished` the sum, which pytest-xdist needs (each worker selects every test and runs a share).
Reviewers showed it wrong for two separate starts in one COMMAND, `ctest -L L1; ctest -L L2
--stop-on-failure` or two pytests: one of two, then one of two, summed to a unit that ran whole one
of whose tests never ran, and its pass was recorded. And a test told by id finished when any
process finished it, so a start that stopped inside it was outweighed by one that finished it.

The decision: a facts file may say `start`, one id for every process of one start of the runner
(pytest's controller and its xdist workers, one `ctest`, one Jest and its workers). The sum is
taken within a start; a unit ran whole only when it did in every start that selected any of it,
and failed when it failed in any. Files that say none are one start, as before, so an adapter that
doesn't send it is merged as it was. A test finished only when every process that reported it,
and didn't deselect it, finished it: an xdist worker reports only the tests it ran or deselected,
so xdist still merges as it did. pytest sends `start`, its workers taking xdist's `testrunuid`.

## Amendment 2: a skip in a run that failed proves nothing (2026-11-12)

Item 4 decided a test that finished as `ran and passed`, a skipped one included. Reviewers showed
TestNG skipping every test after a suite setup failed: those classes were recorded as passed, the
next run left them out, and the setup's failure was hidden; and Gradle calling the test in flight
skipped when its JVM died. A skip says the body didn't run, and in a run that failed that can be
the failure's doing.

The decision: a test's facts may say `skipped` (it finished without its body running: skipped,
disabled, an assumption not met), and a unit's, for a unit told without its tests, how many of its
finished tests were. In a run whose COMMAND failed, the one state function makes such a test `not
run`, and a test file with any skipped test records no pass of its own; its failure, if it failed,
is recorded as before. In a run that passed, nothing changes. An adapter that doesn't say
`skipped` is read as before. pytest says it for a skip or skipif mark, a skip raised in a fixture's
setup or in the test, and an xfail that isn't run; an xfail that ran, and failed as expected, ran.

## Amendment 3: a failure no test explains keeps no test's pass (2026-11-12)

Item 4 decided each test from its own facts alone. A reviewer ran this on Jest: a failing
`afterAll` failed the test file while each of its tests passed; the tests were recorded one by
one, the next run left them out, the `afterAll` never ran, and the check passed.

The decision: a test file that failed while none of its tests failed records no pass for any of
its tests in that run: the one state function makes them `not run`. Its own `fail` is recorded as
before, so its next run asks its tests again and runs them. A failure one of its tests explains
leaves the others' passes as they were.

## Amendment 4: a run that left out what no plan listed runs again (2026-11-12)

RFC 0003's principle 5 says every failure of the machinery ends in "ran everything". A reviewer
showed the shared JVM hook leaving out Spock features nobody listed while the check passed: the
plan was right, the machinery wasn't, and what it left out never ran. Some adapters can tell only
after COMMAND, from what the runner reported, that this happened.

The decision: an adapter says so from its `read` (`Read::left_out`, with why in words people read),
or a facts file does (`leftOutUnlisted`). hushrun then says one line, starts COMMAND once more in a
directory of its own whose `hushrun.json` lists nothing, with no plan, and decides everything (the
exit code, every unit's state, the check's result) from that start alone; the first start's
results are dropped. When the second start can't be set up, or says the same again, nothing is
recorded for the check or any unit, and hushrun fails, saying why: what ran can't be told from what
a broken run left out. `swift test`'s own run again, inside its program, stays as it is; this is
for adapters that learn it only after COMMAND.

## Amendment 5: a wider fixture's end is its test file's wherever it happens (2026-11-12)

Item 4 says something not followed in a teardown that may end a fixture of a wider scope is the
test file's. pytest ends a parametrized fixture of a wider scope that switches its value inside
the next test's setup, not in a teardown, and a reviewer confirmed in the facts a process started
there charged to that one test alone. Now what isn't followed while a fixture of a wider scope
ends is the test file's, wherever pytest ends it.

## Amendment 6: a test not followed guards its file for either unit (2026-11-12)

Item 5 kept a test that wasn't followed in its own row, and the file's row only what was outside
every test; the plan didn't reuse such a file whole, but only with `unit: test`. A check switched
back to `unit: file` then reused the file whole on an older pass of its own key, which the test
that started a process never earned again. The guard now holds for either unit, in the one
predicate the plan, `observe`, `hushrun ready` and `hushrun why` ask; the file's next run, whole,
replaces the row.

## Amendment 7: the tools an adapter starts get COMMAND's own assignments (2026-11-12)

The registry reads a COMMAND that starts `env NAME=value …` by what follows the assignments, but
the tools adapters start themselves (Cargo's plan build, `go env` and `go list`, `swift package
describe`, CTest's listing) didn't get them: `env GOARCH=386 go test` would key a package on the
files `go list` names for this machine's target. Now every such tool is built through one helper
(`units::command::tool`), which applies COMMAND's own assignments (`StartAt`, `PlanAt` and `ReadAt`
carry them as `assigned`), and an adapter reads COMMAND's environment with them first. And a
program's name is compared in lower case on Windows, whose file system ignores case.

## Amendment 8: a check pytest observed stays pytest's (2026-11-12)

Before CTest's and `swift test`'s adapters, any COMMAND no adapter recognized got pytest's, so
`hushrun run x -- ctest` in a project whose CTest tests are pytest was observed by pytest; now CTest
claims it and observes nothing of what pytest ran. The decision: when this machine's store says
pytest last reported the check's test files, a COMMAND CTest's or `swift test`'s adapter would
claim stays pytest's (`units::adapter::choose_kept`), for the run, `hushrun ready` and `hushrun
why` alike. Hints aren't asked: they are read only once an adapter is chosen, for its sources and
its word, and asking one before would mean a request to the server for every such run. So on a
machine whose store doesn't hold the check, such as a fresh CI runner, CTest or `swift test` takes
the COMMAND, which the CHANGELOG says.

## Amendment 9: a store of schema 1 is imported (2026-11-12)

Item 8 left a `v1` file for an older hushrun and read it never, and init's cache step named
`*.v2.sqlite`. CI workflows that cache `units/*.v1.sqlite`, the step init proposed before, would
restore a file no run updates, and a person upgrading would see reuse stop with no word why.

The decision: when there is no `v2` file and a `v1` file is hushrun's store of schema 1, a run with
`tests` on makes the `v2` file from it before anything reads the store. A copy of the file and
additive statements wouldn't be sound: schema 1's `checks.adapter` has a `CHECK` that takes four
words, which SQLite can't alter, so a store copied whole would refuse every newer adapter's check.
Instead a new file of schema 2 is made, every row of schema 1's tables copied into it from a copy
of the `v1` file and its WAL, in one transaction, whole or not at all, with the references checked;
it takes the store's name only when no other hushrun made one meanwhile. The `v1` file is left as
it is, for an older hushrun. A run in CI that imported says once what to change in the cache step,
which now names the stores without their schema (`units/*.sqlite`), so the next schema needs no
edit. `hushrun ready` and `hushrun why`, which write nothing, never import.

## Amendment 10: what the second review ran (2026-11-12)

A second review ran the core on pytest 8.4.2 and 9.2. Each item is fixed with a test that failed
first; the plugin's are in ADR 0080's Amendment 2.

- **A start that died.** A start of the runner that died before writing its facts was invisible:
  `pytest tests; CRASH=1 pytest tests` recorded the file passed whole from the first start in a
  run that exited 139. Now a process may make `<anything>.started` in the run's directory as it
  begins and remove it once its facts are written; one left after COMMAND means no unit and no
  test of the run records a pass (the one state function gets COMMAND as failed and no failure
  told by unit, later a fact of its own, below), fails still record, and the run says so once. No rule says a failed COMMAND with
  no failed unit keeps nothing: a package that fails to build beside fifty that passed keeps the
  fifty.
- **Left out in `observe`.** Amendment 4's second start ran in `observe` too, where nothing was
  listed: it would be the same run, and a second start that failed turned a passing COMMAND into a
  failure. Now only a start whose plan listed something runs again; any other records no unit,
  says why once, and keeps COMMAND's exit code.
- **A stale build.** A `ctest --test-dir build` with no build after a source change ran the old
  programs and passed, and that was recorded as the check's pass on this code: the next run was
  skipped. An adapter that knows says so (`Read::stale_build`, or `staleBuild` in its facts): a run
  that passed then records nothing at all and says why once; a failed one records its fail.
- **A store that can't be read.** The line said the next run starts such a store again, but only a
  schema mismatch was replaced: a schema-2 file made before the test table's `not_kept` column
  failed every read and write for ever, said at debug only. Now a store whose tables aren't the
  ones this build makes can't be read either, and the one place that writes sets aside (as
  `<file>.unread`, one kept) a store it can't open or write for anything but busy, and makes it
  anew, once. The missing column isn't added in place: one rule covers every shape a store can be
  in, and costs one run's map.
- **A stored word that sticks.** Amendment 8's word kept pytest's adapter for a `make test` whose
  Makefile had moved from pytest to `ctest`: pytest never loaded, nothing was observed for ever,
  and a machine with a fresh cache decided otherwise. Now a run kept pytest's by the word in which
  pytest wrote no facts forgets the check in the store's write after the run's record, and the
  next run, `hushrun ready` and `hushrun why` choose afresh.
- **`ready` and `why` before an import.** They write nothing, so they never import a schema-1
  store, and read no adapter word: they could choose CTest where the run, which imports first,
  chooses pytest. They now read the word from the schema-1 file, read only.
- **Why a test kept nothing.** `hushrun why` gave the wrong reason for a test the core left
  without a result: a Go `TestNeeds` that skipped because `TestBase` failed (with `GOFLAGS=-v`)
  was told nothing changed and no pass counts. Only an adapter's `notKept` was kept. Now the one
  state function returns, with each `not run`, the rule that decided it (`NotKept`: left out by
  something else, unfinished, tests reused one by one, a start that died, a skip in a failed run,
  a failure no test explains, a stale build, a failed run without failures by unit), and with a
  pass a test never left out keeps none of. The start that died, the stale build and
  `neverLeftOut` are facts that function reads, not a COMMAND narrowed to failed or a check after
  it. The sentence goes into the test's row, the adapter's words winning for a test that didn't
  finish, and into the test file's own row, a new column; `why` says it as reason `not-kept`, for
  a test first and for a test file where nothing changed and no pass counts. A test file told
  without its tests and decided `not run` still keeps no row, as before: `why` says it never ran
  here. A stale build in a run that failed now keeps every pass out too: before, a test file or
  test whose failures the adapter reports by unit recorded its pass.
- **Corrections.** Item 10 said pytest can't watch a test "in this version": it does, by ADR 0080,
  while no other tool uses `sys.monitoring`. The Consequences said more than 5,000 results in a run
  record none: a run sends 50,000 at most, failures first, and an older server keeps the first
  5,000 (ADR 0078, item 9 and Amendment 4).

## Amendment 11: what the third review ran (2026-11-12)

A third review ran what Amendment 10 added, on pytest 8.4.2 and 9.2. Each item is fixed with a
test that failed first; the plugin's are in ADR 0080's Amendment 3.

- **A pytest a test starts.** A passing test that started a pytest of its own and stopped it left
  that pytest's `.started` file, since it inherits the run's variables: every run of the check
  then kept no pass, for good. A pytest session now sets `HUSHRUN_PYTEST_SESSION` to the run's
  directory in its own environment, which only its descendants inherit; a pytest that finds it
  set to the run's directory makes no file. Jest's and Vitest's second-start files already tell
  a start inside a test by a variable the first start sets (`HUSHRUN_JEST_RUN`,
  `HUSHRUN_VITEST_START`); their adapters load only in their own COMMANDs (Jest through
  `NODE_OPTIONS`, which a pytest COMMAND doesn't set), and CTest, `swift test`, Go, Cargo, Maven
  and Gradle make no such file, so no other child of a test leaves one.
- **A word lost to a run that failed.** A `ctest --test-dir build` whose tests run pytest, kept
  pytest's by the store's word, lost the word to one run with the build directory missing: ctest
  failed before pytest started, and CTest took the check from then on, with one unit instead of
  pytest's test files. Only a run whose COMMAND passed while pytest wrote nothing forgets the word.
- **A store set aside for trouble that passes.** Amendment 10 set aside a store any write failed
  on for anything but busy: a store read-only for one run was renamed and made anew, losing every
  check's lists and words of the repository, and a full disk would do the same. In a race of two
  runs on a damaged store, the second could set aside the store the first had just made. Now only
  what a retry can't cure sets a store aside (another schema or tables, or damage SQLite reports:
  not a database, corrupt); read-only, full, I/O and permission errors read as no map for that run
  and change nothing on disk. The rename happens under the lock beside the store, and only while
  its path names the file that failed.
- **A reader that left files.** `ready` and `why` opened the schema-1 file read-only to read its
  adapter word, which left `-shm` and `-wal` beside it. They now read it from a copy outside the
  cache, as the import does, and the directory is as it was.
- **`why` after a run that kept nothing.** A test file or test the run decided `not run` kept a
  row only when some test of it ran, so after a start that died, a module skipped as it loads in
  a failed run, a second start of Jest, or a Vitest run with an error no test owns, `hushrun why`
  said "this machine has never seen it run" of what had just run. Every unit and test the run saw
  and decided `not run`, by any rule, now keeps a row with what it read and why, a run that
  records nothing at all (a stale build that passed, what was left out twice) included; a unit
  that didn't run whole keeps the row it had, since what it read this time is less than all it
  reads. Left out twice is a rule of the one function now (`left_out_again`), so its tests say it.
  The reasons take an adapter's words where it gives some: a test's `notKept` (pytest's plugin
  says which wider fixture's end kept a test, where before `why` said it didn't finish), a
  `.started` file's `why`, and a facts file's own `notKept` for a failed run whose failures
  aren't told by unit.
- **A unit that isn't a file.** After a run that reused a CTest program, `hushrun why unit
  test_5` said it isn't a file of the scope: a write forgot every row whose name no file or
  directory of the scope holds when the run didn't run it, though the run had reused it. The
  store now keeps such a row while the run's adapter tells the unit, run, reused or not run; and
  `why` splits `UNIT::TEST` where the unit is one the store knows, so a Gradle or Maven class of
  no file (`<project directory>::<class>`) is found whole, with its tests after it.

## Amendment 12: what the confirmation pass ran (2026-11-12)

- **A test file's reason, through its test.** After a session fixture's end failed, `why` of the
  test said the fixture's end, but `why` of its test file said it "didn't run whole in every start
  of its runner", which points at a start. A test file kept out because not every test of it
  finished, or one was skipped in a run that failed, now says it through the first such test and
  that test's own reason; with no test told, it says not every test of it finished, without a
  start.
- **A word lost to one silent run.** A `ctest --test-dir build` whose tests run pytest, with an
  empty build directory, found no test and exited 0: pytest reported nothing, the word went, and
  CTest kept the check for good. A passing run in which pytest stays silent is now counted with
  the word (`silent_runs`, set back to 0 by every run that writes the check, in the statement
  that writes the word); only the second in a row forgets the word, and the first says pytest
  reported none this time, promising nothing.

## Amendment 13: each adapter's word in the run's counts (2026-11-13)

The first runs on real repositories read `2 of 22 tests run, in 2 of 22 test files` on fmt, whose
units are test programs, and `all 1 test file would have run` of FTXUI's one program and its 412
tests. The run's lines that count units (the plan's line, `observe`'s line and the last line's
`; <n> … reused`) now say them in the adapter's word, the one C-39 already gives it: `test file`
(pytest, Vitest, Jest), `test binary` (Cargo), `package` (Go), `test class` (Gradle, Maven), `test
target` (`swift test`), `test program` (CTest), made plural by English's rule (`test binaries`,
`test classes`). Lines that name the kind in general (`test files aren't observed`) are unchanged.

## Amendment 14: what the first real runs said (2026-11-13)

The feature ran on real repositories (click, httpx, werkzeug, yaml, svgo, luxon, jest-extended,
fmt, FTXUI, two Swift packages): no wrong reuse, but several lines said something untrue.

- **"and 1 more that did runs as a check"** read as a mistake. The audit's clause is now `, and 1
  more that already passed runs anyway, as a check` (`run anyway` for more), and `all <N> run; 1
  that already passed on the files it read runs anyway, as a check`; the counts are as before.
- **`ready` in each adapter's word.** `hushrun ready` said "test files" for every adapter while a
  run's lines said "test programs" or "test classes". Its line is now worded by the one function
  beside the run's (`previewed_line`), with the adapter's word.
