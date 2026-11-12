# 0078. What a test read: one function

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-40; amends C-12, C-22, C-32, C-36, C-37, C-39; relies on C-31, C-34, C-38, ADR 0077,
  RFC 0004

## Context

ADR 0077 made the test a unit, and took each test to have read what its test file read. RFC 0004
§3.2 adds `tests.reads: test`: each test is keyed by what it touched, so a change to a file only
some tests read runs those tests and no others. What a test read is a derived state, and every
reader needs the same answer: the plan that keys it, the record that keys its result, `observe`
saying what reuse would have done, `hushrun ready` and `hushrun why`. Two copies of the rule would
disagree on a key, and a key one reader computes that another doesn't is a pass nobody finds, or a
reuse nobody earned.

## Decision

1. **One function** (`hushrun_core::units::test_list`, C-40) gives a test's list from what its
   adapter reported, for both values of `tests.reads`, and every reader calls it: the record
   through it; the plan, `observe`, `ready` and `why` through `units::shared::test_lists`, which
   reads the list the record kept (this machine's store, or a hint, C-41).
2. **The rules** (RFC 0004 §3.2). With `reads: file`, or when anything below is missing, a test
   read its test file's list. With `reads: test`, its **code** is its test file, the files whose
   functions were called while it ran (`ran`), the files every test of the file reads as code
   (`everyTest`: module top level and class bodies run at collection, which every test needs),
   what its file loaded at collection (`loaded.ran`), and what each fixture it requested, directly
   or through another, ran while pytest kept that fixture's value (`shared`). Its list is that
   code, the files that code imports directly (`imports`) and opens (`opens`), the files it opened
   while it ran, what its file opened at collection and its fixtures opened, and the directories it
   listed, with every file now below them. A file imported two hops away whose code never ran
   isn't on the list: that is the known miss the audit is there for (RFC 0003 §3.3's two rules at a
   test's grain).
3. **Facts carry it** (C-32), additions to `hushrun.unit-facts/v1` that older adapters needn't
   send: per test `ran`, `opened`, `dirs`; per unit `loaded`, `everyTest`, `shared`; per file
   `paths`, `imports` and `opens`; and `settings.watch`, a sentence saying why the adapter watched
   no test. An adapter whose `KnowsTests.reads` is false, or that leaves any of these out, gets
   `reads: file` for that run, said once: `<check>: each test is taken to have read what its test
   file read: <why>`.
4. **Keys and tiers** (C-39). A test's key, `hushrun.unit-key/v1`, digests its own list. With
   `reads: test`, a test file that ran whole and passed records its own pass (`v0`) and each test's
   (`v1`): the next run may reuse the file whole, or each test on what it alone read.
5. **The store keeps lists as sets** (C-36): each test's row points at its list (`list_ref`), one
   row per distinct list, which tests share; a list no test points at is pruned.
6. **Decided once.** Whether a run counts by test or by test file is decided once: a run whose plan
   listed tests to leave out counts by test whatever it reported, and one past the limit counts by
   test file. On either path, what a test's adapter couldn't follow is its test file's too: the file
   records no pass of its own, and with its tests not kept one by one its row keeps what wasn't
   followed, so the next plan doesn't key it at all.
7. **A test its adapter never leaves out** (`neverLeftOut` in its facts, such as one whose id
   another test of its file shares) records no pass of its own: no plan finds one to list it by,
   and it runs, as it would. Its fail still counts.
8. **Readers.** `hushrun why NAME UNIT::TEST` (C-37) explains one test: its test file's reasons
   first, then the second tier's; a test file one of whose tests wasn't followed the last time it
   ran here isn't reused whole, and says so. `hushrun ready` (C-22) counts tests where tests are the
   unit, as the run's line does. `observe`'s "couldn't follow" counts tests where its line does.
9. **A run's results go failures first** (C-12, RFC 0004 §3.8): every failure, then test files'
   passes, then tests' passes, at most 50,000 and 8 MiB. An older server's limit (5,000, 1 MiB) is
   met by sending the first 5,000 once more and saying how many weren't kept; an older server that
   refuses a `savedMs` with no test file reused gets the run again without it.

## Consequences

- With `reads: test`, a change to a module only some tests call runs those tests; the rest of the
  file is reused one by one.
- A value two imports away, or a registry another module filled at import, can be missed: the test
  that read it is reused on a list without that file. The audit's second run catches it and
  suspends reuse, as RFC 0003 §3.3 does for test files.
- Results grow by one per test where a file passes whole with `reads: test`; failures are never
  the ones a server's limit cuts.

## Alternatives considered

- **Each reader with its own copy of the list:** the record and the plan would drift, and a key
  only one of them computes is a reuse nobody earned or a pass nobody finds.
- **Fields on the adapter's `Seen` for what a test read:** five adapters written against the 0a
  interface would stop compiling. The facts carry them, read once for every adapter.
- **Keying `reads: test` tests only when their file didn't pass whole:** the next run, with a
  change only some tests read, would find no test's pass to reuse.

## Amendment 1: re-exports are followed to the end (2026-11-12)

Item 2's rules take a test to have read the files its code imports directly, one step. A reviewer
showed Vitest missing `export * from` chains: `index.ts` re-exports `api.ts`, which re-exports
`impl.ts`; the test calls `run` through `index.ts`, no function of `api.ts` is ever called (it has
none of its own), and `impl.ts` was on no test's list. A change to it reused the test.

The decision: a facts file may send `reexports`, shaped like `imports`, the files each file
re-exports from. For every file on a test's own list, the files it re-exports from are on the list
too, followed to the end through re-exports only, in the one function (`test_list`). An adapter
that doesn't send it loses nothing it had. Vitest's adapter sends it; with `reads: file` the list
is the test file's, as before.
