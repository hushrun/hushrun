# 0147. Flaky tests, from evidence

- **Status:** proposed; being built (C-160 to C-163 built; C-164 on are the next step)
- **Date:** 2026-11-25
- **Rules:** C-160 to C-169 reserved, C-160 to C-163 added; C-32, C-33, C-34 amended; relies on
  ADR 0069, 0074, 0080, 0087, 0088, 0142, 0145

## Context

A unit's key commits to every file it read (C-31). When the same key holds a pass and a fail, the
test passed and failed on the same files: that is evidence it is flaky, not a guess from retries.

hushrun already gathers that evidence and throws most of it away:

- A unit that had a pass and fails runs once more (C-34). When it passes then, the run says
  `it may be flaky`. That line reaches the terminal and nothing else: the server gets the first
  run's `fail` and no word of the second.
- Gradle, Maven, Cargo under nextest and `meson test` see every try of a test the runner retried
  and record a fail when any try failed (ADR 0087 item 9, ADR 0088, C-146, C-105). That the tries
  disagreed is lost.
- A server keeps each unit result under its key, but can't tell a test's key from its test file's
  (a failed test fails its file too), so counting keys would count each flaky test twice.
- A run's record doesn't say how many units its audit ran, so the one unbiased sample hushrun
  draws (2% of what could be reused) can't be read as a rate.

The server (the cloud repository's plan `flaky-tests-from-evidence`, Phase 1) writes one row, a
flake, when a fail is proven to have a pass on the same files in the same place, and answers the
request that records the run with what the organization has seen of the units that just failed.
This ADR is the CLI's half: what it sends, and (the next step) what it says with the answer.

## Decision

1. **A unit result says what it is** (C-160). `level` is `test` on a test's result and `file` on
   a test file's, unless a test of that file records a fail of its own in the same run. So each
   failure is said once, by the smallest unit that holds it, and a server that writes a flake only
   for a fail with `level` counts one flaky test once. A test its adapter never leaves out records
   no pass of its own (C-39), so its file speaks for it (a Kotest spec's tests). A pass may carry
   `level` too; it changes nothing on the server, which looks a pass up by key.
2. **The second run is said** (C-161). A unit whose second run passed sends `"again": "pass"` on
   its `fail`. The fail stays the fail: the next run still runs it.
3. **A runner's own retry is said** (C-162). An adapter that sees every try and records a result
   sets `retried` in a test's facts when the tries didn't all end the same, and the result sends
   `"retried": true`. Cargo under nextest, Gradle, Maven and `meson test` do; their result stays a
   fail. Bats and CTest record nothing for a retried test, and `swift test`, `xcodebuild test` and
   test programs started directly don't offer the runner's flag, so none of them sends it.
   Vitest, Jest, Mocha, pytest and RSpec are left as they are until someone reads what their
   retries leave in the facts.
4. **The audit is counted** (C-163). `units.audited` in a run's result counts what the plan's
   audit ran though it had passed: tests, where the run counts tests.
5. **`id` is reserved.** A unit result may carry `id`, an identity that survives edits. This
   version sends none: making one that the server can't compute from a guessed test name needs a
   key that the repository holds, which is a later decision.
6. **Names stay on the machine.** Every new field is a word, a flag, a count or a hash. No test
   name or path is sent.
7. **Old and new.** A server from before these fields ignores them: a unit result's fields are
   read without refusing unknown ones, and a run's result is kept as sent. This machine's own
   files (`hushrun.unit-run/v0`) leave each field out when absent, so an older hushrun reads them.

## Consequences

- With a server that writes flakes, a CI run's failed test can be named, later, as having failed
  and passed on the same files, by hushrun's second run, by the runner's retry, or by two runs in
  the same place. Nothing about which tests run changes, and no verdict does.
- hushrun under-counts flakes: a test that passed isn't run again while its files stay the same.
  `units.audited` is what lets a page say how often audited tests flaked, without that bias.
- A body grows by at most about 40 bytes for a result with every field; the 8 MiB bound
  (RFC 0004 §3.8) still sizes each result by its JSON.

## Alternatives

- **The server infers the level** from which keys of a run failed together. It can't: a test's key
  and its file's are both hashes, and a file can fail with none of its tests (a collection error).
- **Send names**, so the dashboard names flaky tests. No: names stay on the machine (C-33). A person
  names them in a clone; that command is a later step.
- **A plain hash of a test's name as `id`.** The server could confirm a guessed name with it.
- **Count a retried pass as a pass**, as Vitest does today. Nine adapters refuse to; this ADR keeps
  each adapter's rule and only says that the tries disagreed.
