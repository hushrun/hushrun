# Rust tests one by one, measured

[ADR 0145](../adr/0145-rust-tests-by-a-static-map.md) reuses each Rust test on its own under
`tests.unit: test` and `cargo nextest run`. This page records what it did on this repository's own
`test` and `test-macos` checks, on a Mac and in CI, with hushrun 0.20.1, the first release with the
map.

In short: on a machine that ran the tests before, a change to one module ran the tests that reach
it, and no test the map left out would have failed. Most of the test time is integration tests that
start the `hushrun` binary, which always run, so a change in `crates/hushrun/src/` saved little time.
In CI no test was reused one by one with 0.20.1: a fresh runner's plan knew no Rust test (see
[In CI](#in-ci)). [ADR 0162](../adr/0162-rust-tests-listed-before-the-run.md) has the plan list
each test binary's tests, and a fresh store then runs the same tests a warm machine runs (see
[A fresh machine](#a-fresh-machine)); CI shows it once that is released. The runs also found a
test reused wrongly, which [is fixed](#a-wrong-reuse-found-and-fixed).

## On a Mac

An Apple silicon Mac, Rust 1.94.0 with its rust-analyzer, cargo-nextest 0.9.152, hushrun 0.20.1
built from `main`. The runs were in a clone of the repository whose `hushrun.yaml` names no
backend, so results stayed on the machine (ADR 0067), with a cache folder of their own
(`HUSHRUN_CACHE_DIR`), as `test-macos`, which counts from macOS. Tests that need a tool the machine
doesn't name (`HUSHRUN_TEST_*`) skipped themselves, as they do on any machine without them.

**Three crates** (`cargo nextest run --locked -p hushrun-graph -p hushrun-digest -p hushrun-tlog`:
5 test binaries, 103 tests):

| Run | What hushrun said | Tests run |
| --- | --- | --- |
| First | `all 5 test binaries run: none had passed here yet on the files it read` | 103 |
| Same files | `skipped: this exact code passed 52s ago` | 0 |
| A comment in `fn bit` of `hushrun-tlog/src/map.rs` | `23 of 103 tests run, in 3 of 5 test binaries; 80 already passed on the files they read, and 2 more that already passed run anyway, as a check` | 23 |

The 21 tests that ran besides the audit's 2 are the map's and the replay's, which call `bit`;
those of `b64`, `checkpoint`, `note` and the other modules didn't run. rust-analyzer indexed the
workspace in 34 seconds, and the map took half a second more.

A `panic!` at the head of the same `bit`, after a pass on the files without it, ran 23 tests, of
which 4 failed. Plain `cargo nextest run` of the same three crates, with no hushrun, failed the same
4 and no other: no test the map left out would have failed.

**The whole workspace** (`cargo nextest run --workspace --locked --no-tests=pass`: 148 test
binaries, 3,533 tests):

| Run | What hushrun said | Tests run | nextest | The check |
| --- | --- | --- | --- | --- |
| First | `all 148 test binaries run: none had passed here yet on the files it read` | 3,533 | 392 s | 6 min 32 s |
| A comment in `fn method_of` of `crates/hushrun/src/units/gradle.rs` | `111 of 148 test binaries run; 16 already passed on the files they read, and so did 2,049 tests of the others, and 1 more test binary and 48 more tests that already passed run anyway, as a check` | 1,188 | 332 s | 5 min 45 s |
| Instead, a comment in `fn parse_class` of `crates/hushrun-core/src/glob.rs` | `142 of 148 test binaries run; 2 already passed on the files they read, and so did 1,538 tests of the others, and 32 more tests that already passed run anyway, as a check` | 1,978 | 449 s | 8 min 8 s |

The index took 57 to 89 seconds on each new tree; the map of every test, 6 to 8 seconds more. The
machine was also doing other work, so the times of the last two rows are rough.

After the change to `gradle.rs`, 66 of the binary's 782 unit tests ran, the audit's included, and
1,122 of the 1,338 tests in `crates/hushrun/tests/`: those that start the `hushrun` binary. After
the change to `glob.rs`, which nearly every crate links, 57 of `hushrun-core`'s 187 tests ran, 138
of the binary's 782 and 268 of `hushrun-build`'s 669 (143 of those for the reason below).

So a change in `crates/hushrun/src/` ran a third of the tests, and the check took 12% less time.
The tests that still run are the ones that take the time: the integration tests that start the
binary are 3,299 of the 3,897 seconds the tests take one after another, 85%.

## What still runs, and why

On the whole workspace, 1,372 of the 3,533 tests are taken to have read what their test binary
read (C-152), each for the first item that can start a binary of the workspace (C-151):

| Tests | Where | Why |
| --- | --- | --- |
| 1,085 | `crates/hushrun/tests/` | they start the `hushrun` binary (`env!("CARGO_BIN_EXE_hushrun")`, `cargo_bin`), which reads all of `crates/hushrun/src/` and every library it links. By design: no test of the binary can be narrowed by what its source reaches. |
| 143 | `hushrun-build`'s unit tests | `invocation.rs`'s `CARGO_KEYED_PREFIXES` and `read_env` hold the string `"CARGO_BIN_EXE_"`: they key a compile by the variables Cargo sets, and start nothing, but C-151 counts any string that starts with it. |
| 81 | `hushrun-build/tests/{cargo,cc,rustc,process}.rs` | they find the wrapper examples beside `current_exe()` and start them: binaries of the workspace, rightly. |
| 31 | `hushrun-testkit`'s `workspace_root`, and the tests that call it | it looks above `current_exe()` for the root when `CARGO_MANIFEST_DIR` is unset, and starts nothing. |
| 21 | `hushrun-graph`'s tests | `link_node`, the map's own detector, holds `starts_with("CARGO_BIN_EXE_")`. |
| 11 | a few unit tests of `hushrun` and `hushrun-build` | they start their own test program again (`current_exe()`), rightly. |

Any change in a crate the binary links, `crates/hushrun/src/` included, runs the 1,085 again. A
change in a crate the binary doesn't link (`xtask`, the testkit's own tests) or in one test file
runs only what reaches it.

## In CI

The pull request's runs, on GitHub's `ubuntu-latest` and `macos-latest` runners, with hushrun
0.20.1 from the newest release. Each pushed a commit that changes one comment, then was dropped.
On `main`, the 0.20.1 tree's own run had used 0.19.1, which reused whole binaries: `all 148 test
binaries run`, 34 min 51 s on Linux and 41 min 26 s on macOS.

**Run A**, a comment in `crates/hushrun-core/src/lib.rs`, which nearly every binary links. No
test had a key of its own yet, since 0.19.1 recorded none, so this run only records them:

| Job | What hushrun said | Tests run | The check |
| --- | --- | --- | --- |
| `test` | `146 of 148 test binaries run; 2 already passed on the files they read, and 1 more test binary that already passed runs anyway, as a check` | 3,458 | 36 min 13 s |
| `test-macos` | the same | 3,469 | 50 min 58 s |

No line said rust-analyzer was missing, and none that a test fell back. After the tests, the
map's index of the new tree took 2 min 12 s on Linux and 3 min 41 s on macOS, between nextest's
summary and hushrun's last line: a fresh runner keeps no index.

**Run B**, a comment in `fn method_of` of `crates/hushrun/src/units/gradle.rs`, on run A's keys:

| Job | What hushrun said | Tests run | nextest | The check |
| --- | --- | --- | --- | --- |
| `test` | `121 of 148 test binaries run; 27 already passed on the files they read, and 1 more test binary that already passed runs anyway, as a check` | 2,118 | 1,552 s (run A: 2,172 s) | 25 min 57 s (run A: 36 min 13 s) |
| `test-macos` | the same | 2,120 | 3,003 s (run A: 3,054 s) | 50 min 7 s (run A: 50 min 58 s) |

**No test was reused one by one in CI.** The 1,340 tests left out on Linux (1,349 on macOS) are
those of the 27 test binaries reused whole: the crates `gradle.rs` isn't built into, such as
`hushrun-build`, `hushrun-core` and `xtask`. All 782 of the `hushrun` binary's unit tests ran on
both systems, where the same change on a Mac ran 66. The plan line says so: it names no `tests of
the others`. The 29% the Linux check saved is binary reuse alone. On macOS it saved 51 seconds:
the binaries reused whole hold fast tests (577 of the 9,161 seconds the tests take one after
another), and every slow one ran.

Why: a plan keys a test it knows, and the Cargo adapter knows a test from this machine's store
alone, the row its last run here wrote. A fresh runner has no store. Cargo shares no hints either
(its keys come from the build, so it sends none), so no other machine's run names a test to it.
Each test of a binary that changed is then keyed by its binary's list, and runs. Runs A and B
recorded each test's key on the server, so the passes are there to find; nothing asks for them.
A plan that lists each test binary's tests (it builds them before COMMAND) knows them on any
machine: [ADR 0162](../adr/0162-rust-tests-listed-before-the-run.md) does, measured below. CI
installs the newest release, so these CI numbers stay 0.20.1's until a release has it; then CI's
run B would run about the 1,221 tests of the next section, not 2,120.

## A fresh machine

The fix of [ADR 0162](../adr/0162-rust-tests-listed-before-the-run.md), on the same Mac and clone
as above, as `test-macos` on the whole workspace, steps that stand in for a CI runner:

1. Run 1 with hushrun 0.20.1, with a new cache folder: `all 148 test binaries run`, 3,533 tests,
   which records each test's key in the results (local mode, ADR 0067).
2. Two copies of that cache folder, each without its `units/` folder: no store of the check's
   tests and no rust-analyzer index, as on a fresh runner. The results stay, as on the server.
3. A comment in `fn method_of` of `crates/hushrun/src/units/gradle.rs`, the change of run B, and
   the test binaries built once, so that neither run below pays the compile.
4. Run 2 with 0.20.1 on one copy, and with the fix on the other.

| Run 2 | What hushrun said | Tests run | `hushrun` unit tests | `crates/hushrun/tests/` | nextest | The check |
| --- | --- | --- | --- | --- | --- | --- |
| 0.20.1 | `121 of 148 test binaries run; 27 already passed on the files they read, and 1 more test binary that already passed runs anyway, as a check` | 2,120 | 782 of 782 | 1,338 of 1,338 | 336 s | 6 min 18 s |
| The fix | `1,221 of 3,533 tests run, in 108 of 148 test binaries; 2,312 already passed on the files they read, and 48 more that already passed run anyway, as a check` | 1,221 | 66 of 782 | 1,121 of 1,338 | 317 s | 5 min 18 s |

With 0.20.1, the fresh store ran what CI's run B ran: the 2,120 tests of the 121 binaries whose
keys changed, every unit test of the `hushrun` binary among them. With the fix it ran what a warm
machine runs after the same change ([On a Mac](#on-a-mac): 66 of the 782, and 1,122 of the
1,338): 66 and 1,121, and 34 tests of other crates, the audit's runs among them. Its last line counts tests, `2,312 tests reused`, where 0.20.1's said `27 test binaries
reused` (neither gives a time: a store with no whole run of the check has nothing to cap it by,
C-33).

Listing the 148 test binaries' tests (two starts each, side by side) took 0.3 seconds and found
the same 3,533 tests nextest ran. The fresh store's rust-analyzer index took 47 seconds and the
map 5 seconds more, both before COMMAND now; 0.20.1 pays the index after COMMAND instead, to
record each test's key. Both checks include it.

## A comment in a web server ran three tests in four

Measured on 2026-11-28 with hushrun 0.21.0 on another workspace: a web server, one package of 96
source files, with 972 tests in 63 test binaries. 176 are unit tests of its library and 796 are
integration tests, nearly all of which build the whole application (its configuration and its
router) and send it requests in the test's own process. None starts a binary, so the map narrows
all 972.

After a run that passed, two lines of comment were added at the end of one module, a file whose
functions request handlers call and one of whose constants the configuration reads:

| What ran | Tests run | Reused |
| --- | --- | --- |
| The whole suite, nothing recorded yet | 972 | 0 |
| After the comment | 746 | 226 |

The map is right, and it is why they ran. This machine's store keeps each test's list:

| Tests | Their lists hold the changed file | Source files in a list |
| --- | --- | --- |
| 176 unit tests of the library | 21 | 5 on average, 93 at most |
| 796 integration tests | 737 | 88 of the 96 on average; 737 hold 80 or more |

A test that builds the application's router can reach every handler the router names, and through
them nearly every module. The 59 integration tests whose lists don't hold the file (tests of migrations among them) could be
reused, as could the unit tests that reach nothing of it: 8 of the library's 176 ran, the audit's
among them. `hushrun why` says the same of each:
`… runs: it read src/<module>.rs, which changed`.

So for a server tested through its router, a change to any module the router reaches runs every
such test. The map narrows unit tests and tests of one module's functions. Finer keys (by the
items a test reaches, ADR 0145's follow-up) wouldn't help here either: the handlers are reached.

### Why a comment still changes a test's key

A test's key holds the bytes of each file it reaches (C-146), so a comment changes it. A key that
ignored comments was considered and not built, since no form of it is both sound and worth its
cost:

- **A comment that moves a line can fail a test.** `line!()`, `column!()`, a panic's location and
  `Location::caller()` in a `#[track_caller]` function give the line to the running code, and
  error types and log records carry it. A test that compares such a text
  (`assert_eq!(format!("failed at line {}", here()), "failed at line 8")`, `here` returning
  `Location::caller().line()`) passes, and fails once a comment line is added above it: tried
  with rustc 1.94. The map can't show that no code a test reaches does this, in a dependency
  included. A skip counts as a pass, so a key must change when a line moves.
- **A doc comment is code.** `///` and `//!` are `#[doc]` attributes, which macros read and
  rustdoc runs.
- **A procedural macro can read comments.** `proc_macro::Span::source_text` gives a macro the
  source behind its input, comments included, so a comment inside a macro's input, or inside an
  item an attribute or a derive is given, can change what it expands to.
- **A file read as text** (`include_str!`, `include_bytes!`, `include!`) is its bytes
  ([amendment 1](../adr/0145-rust-tests-by-a-static-map.md#amendment-1-2026-11-27-files-read-at-compile-time)).

What is left is a comment that moves no token, outside every macro's input and every item with an
attribute macro or a derive: text changed inside a comment, a comment added at the end of a line
or of the file. That would have kept the keys in the measurement above, whose comment was at the
end of the file, but not after the commoner edit, a comment line added above a function. It needs
a second hash of each source through the keys, this machine's store and `hushrun why`. It is left
as a proposal.

## A wrong reuse, found and fixed

A test that reads a source as text, `include_str!("../src/api/mod.rs")`, was reused after a
change to that file broke it: the map reached nothing of the file, which held no item the test
called. [ADR 0145 amendment 1](../adr/0145-rust-tests-by-a-static-map.md#amendment-1-2026-11-27-files-read-at-compile-time)
counts what `include_str!`, `include_bytes!` and `include!` read as reached, and keeps every
`.rs` file a crate's dep-info names that is none of its modules in every test of its binaries.
In a two-file reproduction, 0.20.1 reused the test and passed while plain nextest failed it; the
fixed build ran it and failed. On this repository nothing changes: no test includes a `.rs`
file, and no crate's dep-info names one that isn't its module.

## Known follow-ups

- **In CI, no Rust test is reused one by one** with 0.20.1 (above). Fixed by
  [ADR 0162](../adr/0162-rust-tests-listed-before-the-run.md), [measured](#a-fresh-machine) on a
  fresh store; CI shows it once a release has it.
- **A test binary with no tests hid the run's count of tests.** Four of this workspace's binaries
  have no tests without the `it` feature. When one was reused whole, its row named no test, so the
  run took its tests to be known nowhere (C-39) and sent no count of tests: after the change to
  `gradle.rs`, the last line said `reusing 48 test binaries saved about 6m 32s` while 2,049 tests
  were reused. Fixed by ADR 0162: the plan's list tells a binary with no tests from one whose tests
  aren't known.
- **An audited test of a binary that passed whole didn't run.** When the audit drew a test of a
  binary the first tier could reuse whole, the plan took the binary out of `reuse` but left it in
  the list the target runner skips, so the test never ran though the line counted it. Fixed by
  ADR 0162.
- **`saved about` can be more than the run took.** It adds up the reused tests' recorded
  durations, which ran side by side, capped at the last whole run: `saved about 6m 32s` on a run
  that took 5 min 45 s, 47 seconds less than the run before.

## Proposals

- **Keys that ignore a comment that moves no token.** See
  [above](#why-a-comment-still-changes-a-tests-key): sound only for a comment outside every
  macro's input whose change leaves every token on its line and column, which leaves out a comment
  line added or removed anywhere but at the end of a file.
- **C-151's string rule.** A string that starts with `CARGO_BIN_EXE_` counts as a lookup of a
  binary even where it is a prefix compared against (`starts_with`, `strip_prefix`) or kept in a
  list of prefixes. Leaving those out, or a marker for them, would narrow the 143 tests of
  `hushrun-build` and the 21 of `hushrun-graph` above. A third-party library that spawns a path it
  is handed is what the rule guards against, so the change needs its own decision.
- **`workspace_root` without `current_exe()`.** The testkit looks above the running executable only
  when `CARGO_MANIFEST_DIR` is unset, which `cargo test` and `cargo nextest run` always set.
  Dropping that fallback would narrow the 31 tests that reach it.
