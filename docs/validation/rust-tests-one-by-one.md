# Rust tests one by one, measured

[ADR 0145](../adr/0145-rust-tests-by-a-static-map.md) reuses each Rust test on its own under
`tests.unit: test` and `cargo nextest run`. This page records what it did on this repository's own
`test` and `test-macos` checks, on a Mac and in CI, with hushrun 0.20.1, the first release with the
map.

In short: on a machine that ran the tests before, a change to one module ran the tests that reach
it, and no test the map left out would have failed. Most of the test time is integration tests that
start the `hushrun` binary, which always run, so a change in `crates/hushrun/src/` saved little time.
In CI no test was reused one by one: a fresh runner's plan knows no Rust test (see [In CI](#in-ci)).
The runs also found a test reused wrongly, which [is fixed](#a-wrong-reuse-found-and-fixed).

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
A plan that listed each test binary's tests (it builds them before COMMAND) would know them on
any machine. That fix needs a release before CI can show it, since CI installs the newest one.

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

- **In CI, no Rust test is reused one by one** (above): on a fresh runner the plan knows no test of
  a Cargo test binary. Listing each built test binary's tests at the plan (`--list`), and keying
  the tests the adapter maps whether or not a row or a hint names them, would fix it.

- **A test binary with no tests hides the run's count of tests** (on a Mac; in CI there was no
  count of tests to hide). Four of this workspace's binaries
  have no tests without the `it` feature. When one is reused whole, its row names no test, so the
  run takes its tests to be known nowhere (C-39) and sends no count of tests: after the change to
  `gradle.rs`, the last line said `reusing 48 test binaries saved about 6m 32s` while 2,049 tests
  were reused. A row can't tell "no tests" from "tests not known"; it would need to.
- **`saved about` can be more than the run took.** It adds up the reused tests' recorded
  durations, which ran side by side, capped at the last whole run: `saved about 6m 32s` on a run
  that took 5 min 45 s, 47 seconds less than the run before.

## Proposals

- **C-151's string rule.** A string that starts with `CARGO_BIN_EXE_` counts as a lookup of a
  binary even where it is a prefix compared against (`starts_with`, `strip_prefix`) or kept in a
  list of prefixes. Leaving those out, or a marker for them, would narrow the 143 tests of
  `hushrun-build` and the 21 of `hushrun-graph` above. A third-party library that spawns a path it
  is handed is what the rule guards against, so the change needs its own decision.
- **`workspace_root` without `current_exe()`.** The testkit looks above the running executable only
  when `CARGO_MANIFEST_DIR` is unset, which `cargo test` and `cargo nextest run` always set.
  Dropping that fallback would narrow the 31 tests that reach it.
