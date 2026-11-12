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
    `reads: test` with `reads: file`, and says so once. In this version that is pytest too.
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
- Results grow with tests where test files don't pass whole. More than 5,000 in a run records none,
  as before; sending failures first and the server's new limit are a later decision.

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
