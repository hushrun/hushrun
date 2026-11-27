# 0162. Rust tests listed before the run, so a fresh machine reuses them one by one

- **Status:** proposed; built
- **Date:** 2026-11-27
- **Rules:** C-146 and C-39 amended; relies on ADR 0145, 0080, 0082
- **Amends:** [0145](0145-rust-tests-by-a-static-map.md)

## Context

ADR 0145 reuses each Rust test on its own under `cargo nextest run`: a test's key is made of the
files the static map says it reaches. But a plan keyed only the tests it knew of, and the Cargo
adapter knew a test from one place: the row this machine's store wrote the last time the test ran
here. Cargo shares no hints (C-38: its keys come from the build), so no other machine's run could
name a test to it either.

A CI runner starts with no store. Its plan knew no Rust test, so every test of a binary whose own
key missed ran, even though the map could key each one and the results of earlier runs held a pass
for most of those keys. On this repository's pull request, after a one-line change in
`crates/hushrun/src/units/gradle.rs`, CI ran all 782 unit tests of the `hushrun` binary, where a
Mac that had run them before ran 66 ([the measurements](../validation/rust-tests-one-by-one.md#in-ci)).
The only saving came from whole binaries reused.

The same runs showed two smaller faults on the way:

- **An audit that never ran.** With `unit: test`, a binary the first tier would reuse whole is
  taken back when the audit draws one of its tests (C-39), so that test runs. The plan took it out
  of `reuse`, but not out of `binaries`, the list Cargo's target runner reads. The runner still
  skipped the whole binary, and the drawn test never ran, though the line said it did.
- **A binary with no tests hid the count of tests.** A binary reused whole with no row naming a
  test counted as one whose tests nobody knows, so the run sent no count of tests and its last line
  said `reusing 27 test binaries` while hundreds of tests were reused one by one besides.

## Decision

1. **The plan lists each test binary's tests** (C-146). After the plan's build, which already
   makes every test binary before COMMAND (ADR 0069), hushrun starts each binary as nextest does
   to list it: `<binary> --list --format terse`, in its package's folder, with
   `CARGO_MANIFEST_DIR` set to it, COMMAND's assignments, and the binary's own folder on the
   dynamic library path. Each `<name>: test` line is a test, and `<name>` is the id its start with
   `--exact <name>` gets, which is what a run records (C-146), so the keys match. Unless COMMAND
   says something of ignored tests, a second start with `--ignored` gives the tests nextest leaves
   out by default, and those are dropped. The binaries are listed side by side.
2. **The plan keys every test it listed** (C-39), besides those this machine's store has and
   those hints hold. A test the map narrows is keyed by its own list; one it can't, by its
   binary's, as before. One lookup asks for all of them, and the results another machine recorded
   answer it, as they answer a binary's key.
3. **A listing that fails changes nothing.** A binary that can't be started, exits with an error,
   or takes over a minute to list is left with the tests this machine's store knows, as before.
   The plan says once `the tests of <n> of <m> test binaries couldn't be listed, so only those this
   machine ran are reused one by one: <unit>: <why>`.
4. **A binary the second tier takes back from whole reuse isn't skipped whole** (C-34). The plan
   lists in `binaries` only what stays in `reuse` after the audit, so the drawn test runs.
5. **A binary that listed no test has none.** It counts as known with zero tests: the run counts
   tests, a binary reused whole counts the tests it listed as reused, and the audit draws nothing
   of a binary with nothing to run. The plan's line and the last line then count tests, as on a
   machine that ran them all before.

### Why listing, not hints

Cargo could take part in hints (C-38, C-41), as pytest and Gradle do: each run would send each
test's id, and a fresh runner would read them. That needs a round trip to the server, works only
where the results are shared, and names the tests as some earlier run saw them. Listing needs no
server, works in local mode too, costs a fraction of a second, and names the tests as they are in
this build. It also matches what nextest does a moment later, with the same binaries. The keys
already come from the build and the map, so hints would add nothing but the names. Listing is the
smaller change, and it keeps C-38's rule that Cargo sends and reads no hints.

### Soundness

Knowing more tests can only add tests to what the plan asks about. A test is reused only when its
key has a pass and no fail, by the same rule (C-38), and the key is made the same way whether the
id came from a row or a list. Two cases need a word:

- **A row's guard.** A test that wasn't followed the last time it ran here has no lists (C-39), so
  it runs whatever passes there are. A fresh machine has no such row. For Cargo this changes
  nothing: a test binary's build facts are what can leave it unfollowed, and a binary with any is
  neither keyed nor listed. A pass recorded under a test's key was recorded by a run that followed
  it. This is the trust a fresh machine already gives a whole binary's pass.
- **nextest's filters.** The plan doesn't evaluate `-E`, a name filter, `--partition hash:` or a
  profile's default filter. A test they leave out may be listed and keyed: if it passed before, it
  is listed in `reuseTests` and nextest wouldn't start it anyway; if not, the plan counts it among
  those that run. Only the counts in the lines can be too high; nothing is reused that shouldn't be.

## What it costs

Listing every test binary of this workspace (148 binaries, two starts each) took LISTING_TIME on
an Apple silicon Mac. The static map was already made at the plan on a machine with rows; on a
fresh machine it is now made at the plan rather than only after COMMAND, from the same index,
which the plan and the reading after COMMAND share (C-152). So the index costs no more than before.

## Consequences

- A CI runner reuses Rust tests one by one from the results alone. On this repository, after the
  same change to `units/gradle.rs`, a fresh store ran FIXED_HUSHRUN of the binary's 782 unit
  tests, against all 782 with 0.20.1 ([the measurements](../validation/rust-tests-one-by-one.md#a-fresh-machine)).
- CI installs the newest release, so the fix shows in CI only once it is released.
- An audit of a test of a binary that passed whole runs that test.
- The run's lines count tests on a fresh machine, and when a binary with no tests is reused whole.

## Alternatives considered

- **Hints for Cargo** (above): a server round trip and names from the past, for what listing gives
  from this build.
- **`cargo nextest list --message-format json`** with COMMAND's arguments: it would apply nextest's
  own filters, but `run` and `list` take different options, so COMMAND would have to be rewritten
  option by option, and a second Cargo invocation could rebuild what the plan's build made with
  the build blocks' wrapper. Listing the binaries the plan already built is what nextest itself
  does after its build.
- **Keying every test of the map, unlisted.** The map knows test functions by their paths, but not
  every test a binary holds (a macro-made test, a custom harness's), and a test it names that
  doesn't exist would be asked for nothing. The binary's own list is the truth.
