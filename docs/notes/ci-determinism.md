# CI determinism: tests whose outcome changes with load

- **Status:** findings and a prototype, 2026-11-24. Branch `claude/multi-lang-test-compile-ci-fil292-ci-determinism`.
- **Rules touched:** B-26, B-29, B-31, B-45, B-60, B-82 (test switch, two product fixes); C-48 (proposed).

While about 17 PRs were merged into main one at a time, timing-dependent tests failed on PRs
that didn't touch them. Each failure cost a 45–75 minute CI cycle. The bar here is that **load
can't change a test's outcome**. A timeout, a longer bound, a sleep or a retry doesn't meet it. A
test passes when the product's answer is right, and a slow machine only makes it pass later.

## The principle

A time bound in the product is **policy**: how long waiting is worth it on this machine. Examples
are a second for Go's manifest, half a package's compile cost for a download, and 5 s of uploads
at the end. Policy has two kinds of test, and they mustn't be mixed:

1. **Tests of the policy.** These test the function that computes the bound, with numbers in and
   numbers out (`upload::wait_at_end`'s unit test does this). The time comes in as a value.
   Wall-clock time isn't measured.
2. **Tests of an outcome**, such as "B reuses what A built", "the summary says X" or "the parser
   finds 12 tests". Here the bound has to go. Each wait ends on the **event** it waits for (an
   answer, a failure, a stop, a completion), never on a timer. Then the outcome depends only on
   what the fake backend or tool answered.

Almost every flake below is a type-2 test that silently depends on a type-1 bound.

## Classes

- **(a) The test races the product.** The test asserts on elapsed time, or expects something to
  happen within a sleep.
- **(b) The product's own timing can change a result under load.** A deliberate bound such as a
  give-up, a cut-off or a "held" decision changes what the product does, or a real race exists
  between two locks.
- **(c) CI environment and network.** Rate-limited registries, failures swallowed with `|| echo`,
  and runner starvation.

## What the Go flake actually was (item 1)

`every_package_of_a_pure_go_command_is_reused_on_the_other_machine_runtime_too` fails when B
builds any package. I found **five** separate ways for that to happen. Each one only needs the
machine to be slow:

| # | Where | What happens under load | Class |
|---|---|---|---|
| G1 | `end_check_as` → `upload::wait_at_end` (person's machine) | `ending` is set as the command ends. No marker not yet claimed goes up, and those in flight get 5 s. Whatever is left is missing from A's `close` manifest, so B builds it correctly. The test's `&& sleep 1` was the only thing giving the loop time. | b (policy) |
| G2 | `named_fetch`, `GO_PREFETCH_WAIT` (PR #34) | B's first Go fetch waits at most 1 s, or half the package's local cost, for `open` and the manifest. A slower `open` means a miss and a build. | b (policy) |
| G3 | `wait_for`, `budget = cost / 2` | A download is abandoned once it passes half of what the package took to compile. For runtime packages that is milliseconds, so any stall makes it a miss. | b (policy) |
| G4 | `wait_for`, "no rate yet" | Until the first download has measured the link, every other queued fetch answers a miss. | b (policy) |
| G5 | `named_fetch`, `Taken` then `go_prefetching = true` under **two** locks | A fetch arriving between the two locks sees the manifest taken and no prefetch under way. It misses even with infinite time. | **b, a real race** |
| G6 | `Relay::uploads` set `uploading = true` inside the spawned task | If the end comes before the runtime first polls the loop, it finds "no loop" and waits for nothing. | **b, a real race** |

The two-machine test of `Machine::run_uploaded` (re-run A until nothing waits to upload) only
covers G1, and it is a retry loop. G2 to G6 would still flake it.

### The fix, prototyped on this branch

**Product fixes (G5, G6).** These are real bugs, independent of tests:

- `named_fetch` now sets `GoManifest::Taken` and `named_prefetching(kind) = true` in **one**
  critical section. A drop guard clears the flag however the prefetch ends, a panic included. The
  `cc/1` path already did this (`cc_prefetching += 1` under the same lock as `take()`).
- `Relay::start_uploads` marks the loop as running **before** spawning it. `uploads()` is no
  longer callable on its own.

**`Tuning::settle`, the test switch for G1 to G4.** It is set by `HUSHRUN_BUILD_SETTLE=1` and read
only in a debug build, like `HUSHRUN_CC_MANIFEST_WAIT_MS`. A release build is always `false`.
When it is set:

- `named_fetch` and `platform_names` (Go, Gradle, C/C++) have no `until`. They wait until `open`
  or the manifest answered or failed, and until the prefetch is no longer under way.
- `wait_for` waits for every download until it arrives, is dropped, or the relay stops. The
  budget and rate checks are skipped.
- `end_check_as` runs `upload::drain` on **any** machine with no cut and no deadline, until no
  marker of this run waits to go or is claimed (one the backend put off with a date doesn't count).
  The upload loop then stops, and `close` gets its normal `tuning.close`. A stop signal (B-49)
  still wins.

Each of these conditions is one the relay reaches without a timer, so the outcome depends only on
the backend's answers. Request-level network timeouts (`open`/`lookup` at 10 s) remain. See
"residual" below.

`hushrun_testkit::env::SETTLE` names the switch for every two-machine test.
`build_go.rs`'s `Machine::hushrun` sets it, and `&& sleep 1` is gone from `Machine::run` and
`gitlab_go_run`. `a_frozen_relay_costs_a_run_one_wait_not_one_per_go_command` measures the bound
itself, so it removes the variable.

**Why a test switch and not a product change for G1.** On a person's machine, holding the
terminal until every upload is done is wrong: the next run sends them. In CI, the bound (a quarter
of the compile cost, at most 2 minutes) is a cost judgement that B-31 states. The product is
correct either way, because a manifest names only what went up. The test asked a different
question. See "Product follow-ups" for where the end-of-run semantics might still change.

### Verification

On a 4-CPU container, `cargo test -p hushrun --test build_go`:

| Run | Result |
|---|---|
| The whole suite, settled, no `sleep 1` | 17 passed, 1 ignored (57 s) |
| New test `every_package_is_reused_on_the_other_machine_however_slowly_the_backend_answers`: A's PUTs held 1 s each, B's `open` 1.5 s (> `GO_PREFETCH_WAIT`), B's downloads 300 ms (> half a runtime package's compile) | passes (11 s) |
| The same two tests with the switch off (`SETTLE=0`), otherwise the same code | **both fail**: `24 packages: 24 built` and `23 reused, 1 built; 1 block to upload waits for the next run`. That is exactly CI's message. |
| 8 busy loops on 4 CPUs (load average about 16), 12 iterations of `every_package…`, `…however_slowly…` and `a_go_package_one_machine…`, **unsettled** | 12 of 12 iterations failed |
| The same load, **settled** | 12 of 12 iterations passed (3 of 3 tests each) |

The new test is the regression guard. It injects a delay past each bound at the racy points, so
if a later change brings back a time-bounded wait into an outcome path, the test fails on every
machine, not just a loaded one.

### Residual

- Per-request timeouts (`tuning.open` and `tuning.lookup` at 10 s, `close` at 5 s) still exist
  in settled mode. A 10 s stall of a localhost fake is far beyond what we have seen, but it is
  still a bound. The clean fix is for `settle` to drop them too. That needs `Option<Duration>` in
  `http::Client::post`, because `Instant + Duration::MAX` panics. The effort is small.
- `platform_names` now clears `cc_prefetching` with a drop guard, as G5 does. Without it, a
  panicking cc prefetch would hang a settled question instead of ending it.

## Inventory

Effort: S is under half a day, M is 1–2 days, L is more. "Shared helper" rows affect many tests.

### Two-machine build tests: one mechanism, `SETTLE`

| Test / helper | Cause | Class | Fix | Effort |
|---|---|---|---|---|
| build_go.rs `Machine::run`, `gitlab_go_run` (`&& sleep 1`, 15 call sites) | G1–G6 | b | **Done**: `SETTLE`, no sleep | – |
| build_go.rs `every_package_of_a_pure_go_command…` | G1–G6 | b | **Done** | – |
| build_cc.rs `Machine::run` (`&& sleep 1`, 18 sites), `HUSHRUN_CC_MANIFEST_WAIT_MS=15000` | G1, G3, cc manifest wait | b | Set `SETTLE`, drop the sleep and the 15 s override. The relay half is already in this branch. | S |
| build_tsc.rs `run` (`&& sleep 1`, `MANIFEST_WAIT_MS=15000`) | same | b | same | S |
| build_share.rs `Machine::build`, `ci_build`, `greet` and about 24 inline `&& sleep 2` | G1 (CI drain is bounded by cost/4) | b | `SETTLE`. Keep the sleeps only in tests that measure the window itself. | M (many tests, some test the window) |
| build_share.rs `a_merge_whose_ci_skipped_is_built_by_the_bake_before_it_bakes` | probably G1 (bounded CI drain) | b | `SETTLE`. Confirm on a loop. | S |
| build_gradle.rs (no override at all) | G2–G4 for `gradle/1` (`named_fetch` is shared) | b | `SETTLE` | S |
| build_bake_*.rs | bake already drains to `within`, but fetches still race G2–G4 | b | `SETTLE` for outcome tests | S |

### Tests of a bound: replace wall-clock asserts with an injected clock

These assert `elapsed < N` or rely on "X happens within a sleep". The right shape is a
`Clock` the code under test reads, with `Instant::now()` in a release build and a manual clock in
tests. Then "the watchdog answers after the budget" is asserted by advancing the clock and
waiting on the answer. Nothing sleeps.

| Test | Cause | Class | Fix | Effort |
|---|---|---|---|---|
| hushrun-build go/program/tests.rs `a_handler_that_hangs_or_panics_still_gets_one_answer` (PR #53: order plus 3 s) | the watchdog polls `slot.since.elapsed()` every 50 ms | a | The watchdog reads a `Clock`. `slow-get` blocks until its slot was answered by the watchdog (an event) instead of sleeping `budget*2`. `hang-get` blocks until `close`. The test advances the clock past the budget and receives the answer. | M |
| same file: `after_one_relay_call_times_out…` (<550 ms against 600), `a_store_hit_the_watchdog_answered_first…` (sleeps 900 ms, so it passes vacuously when slow), `a_fetch_that_outlasts_its_wait…` | same | a | same clock | (same) |
| hushrun units/program.rs `output_that_never_stops_is_handed_over…` (`took < MOST_AFTER*4`), `a_program_that_keeps_output_open_past_its_end…` (`< MOST_AFTER*2`) | `run_copying_stdout`'s held decision is `QUIET`/`MOST_AFTER` on `Instant` | a, **and b** | Pass a clock into the read loop. Assert `held`, `cut` and the bytes, never elapsed time. Product: what a program prints after `QUIET` is forwarded but **not kept**, so a slow child's last result lines can miss the parser (units_go `what_a_program…left_running…` relies on it). Decide in C-48 whether "kept" may depend on QUIET. | M |
| build_relay/tests.rs, about 17 tests with ms bounds (`a_server_that_sleeps_on_open…` <2 s, `a_block_whose_stated_cost_is_absurd…` <600 ms with a 600 ms ceiling and zero headroom, `a_cold_build_s_questions_wait…` 48 asks <100 ms each, `a_ci_job_s_end_sends…` in [900 ms, 3 s), …) | wall-clock asserts on the relay's waits | a | The relay takes `tokio::time` everywhere already. Run these tests under `#[tokio::test(start_paused = true)]` or on a runtime with paused time. Tokio's auto-advance then makes every `sleep` and `timeout` instant and exact, and the bounds become exact equalities. Needs the relay's runtime to be built in the test, and `Instant::now()` replaced by `tokio::time::Instant::now()` in the relay. | L (many tests, mechanical) |
| build_go.rs `a_frozen_relay_costs_a_run_one_wait…` (SIGSTOP, `date +%s`, SIGHUP at 51 s on macOS) | freezes the whole hushrun process (the relay is a thread), so `sh` and `go` under it are also its children and session | a | Freezing isn't needed to test "one wait per run". A fake whose `open` **never answers** (`fake.hang(route::OPEN)`) gives the same relay state with no signals, and `RELAY_DOWN` (the file the first timeout writes) is the event to assert on. Assert that the second and third go commands asked the relay nothing (the fake's `seen()`), not seconds between stamps. | M |
| build_cc.rs `a_frozen_relay_costs_a_build_one_wait…` | the cc twin, also `sleep 1500 ms` before SIGSTOP | a | same | M |
| units_swift.rs:1252, units_ctest.rs:2884 `…left_running_keeps_printing…` (<2 s against `sleep 2; echo late`) | the late line is timed | a | The fake prints its late line when a FIFO the test opens after hushrun returns is written: ordering by event | S |
| units_go.rs `output_held_past_go_tests_own_wait…` (8 s against go test's 5 s WaitDelay) | Go's own timer | a | Inherent to Go's `WaitDelay`. Keep it, or own the 3 s margin explicitly. Low frequency. | – |
| jest/node/vitest "late timer" tests (a 5–20 ms timer against a 150 ms test) | the error can land in a neighbouring test | a | The fixture's late callback fires on a promise the next test resolves, not on a timer | S each |
| run.rs `failures_are_recorded_retried_and_exit_8` (`timeout: 1s` of `sleep 5`, <4 s), isolated.rs, login.rs, update.rs, notice.rs, setup/repeats.rs, measure.rs, oidc.rs, ledger http tests | asserts the product's own timeout fired in time | a | Only an upper bound can flake. Keep the lower bounds, and turn upper bounds into "the child was killed" or "the request was abandoned" (state), not time | S each |
| units.rs:1213 `rust_test_binaries_are_observed…` (a failing test sleeps 3 s so the others finish first) | scheduling order | a | Order the binaries with a FIFO barrier the failing one waits on | S |
| units_dotnet.rs VSTest console summaries joined on one line (PR #54) | parsing an interleaved console | a/b | The adapter already writes and reads TRX (`units/dotnet/trx.rs`). Take counts from TRX and use the console only for the person. That removes the class rather than the case. | M |
| units_ctest pinned to CMake 3.22 behaviour | the runner image's CMake changes under the test | c | Pin CMake in CI (like Maven and Gradle, by checksum), or assert on the version found and branch explicitly. Don't let `ubuntu-latest` choose. | S |
| mtime ticks (`sleep 20–30 ms`, 1100 ms for Ninja and Mix) | coarse file clocks | a | Loop-until-mtime-changed helpers already exist (units_ctest). Use them everywhere, and set mtimes explicitly with `filetime` where the test only needs "older". | S |

### CI environment and network

| Item | Cause | Class | Fix | Effort |
|---|---|---|---|---|
| `JVM jars` job, `cargo xtask jars --check`: Maven Central 429 | 11 API jars fetched by curl from repo1 on every cold runner | c | The jars are pinned by SHA-256, so any source is safe. (1) `actions/cache` of `target/xtask-jars/` keyed on `hashFiles('xtask/src/jars.rs')`, so a hit never touches the network. (2) `jars.rs` tries a list of mirrors (`repo1.maven.org`, `maven-central.storage-download.googleapis.com/maven2`) on a 429 or 5xx. The checksum is the contract, so this is a second source, not a retry of the same request. | S |
| Linux and macOS "fill Gradle's user home" / "fill Maven's repository": `… \|\| echo` | a failed fill is swallowed, so dozens of offline tests fail later with misleading messages | c | Remove `\|\| echo`, so a fill failure fails **that step**, once, saying "network". Cache the filled `gradle-home` and `m2` with `actions/cache` keyed on the fixtures' build files and the pinned versions, so most runs never fill. Fill only on a miss, on `main`, and save only from `main`. | S |
| Node 22/24 tarballs (`curl --retry 3`) | the same shape | c | `actions/cache` keyed on the versions | S |
| macOS runner starvation (queued, then "cancelled" with runner_id 0 after about 15 min) | one `macos` job runs clippy, nextest (whole workspace), doctest, refs and npm. A macOS runner is the scarce resource, and `concurrency: ci-${{ github.ref }}` cancels in-progress runs per ref but not across 17 PR refs. | c | See "macOS" below | M |

### macOS

1. **Run what changed.** `test-macos` already goes through `hushrun run`, so on unchanged
   inputs it skips. That is also why `main`'s macOS sometimes "passes" by reusing a test binary.
   The rest of the job (installs of pytest, Django, Go, Node, Jest, Vue, Mocha, Ruby and tsc,
   several minutes) runs even when the check will skip. Run `hushrun ready --json` first and
   skip the tool installs when every macOS check is already passed (or reused) for this tree.
2. **Split the job.** Use `clippy-macos` + `doctest-macos` (cheap, no tools) and `test-macos`
   (tools), both `needs:` nothing. They hold two runners for a shorter time instead of one runner
   for 45–75 min. If runners are the constraint rather than wall time, keep one job and do (1).
3. **Queue, don't cancel.** During the merge train, put macOS behind a `merge_group` event (a merge
   queue) instead of on every PR push, so 17 PRs make one macOS run per merge rather than one per
   push. Linux keeps running per PR.
4. **Never mistake reuse for a pass.** A reused `test-macos` says what it reused. The merge
   orchestrator should treat "skipped: passed on <commit>" as a pass only when that commit is an
   ancestor of the merge, which is what hushrun's key already guarantees for the same inputs.

## Product follow-ups (each needs an ADR)

1. **End-of-run uploads that complete later can't help Go.** A manifest names only what went up
   by `close`, and Go and Gradle fetch only by manifest (B-45, B-82). So blocks sent later by the
   next run, or by `hushrun flush` (which today sends only results, ADR 0142), help rustc's
   key-addressed lookups but never Go. A `close` *amendment*, the same run naming blocks it sent
   afterwards, would make the person's-machine bound cost only latency, never reuse. This is the
   product change that removes G1 instead of settling it.
2. **`hushrun flush` should drain the build outbox too** (with the amendment above), so a sandbox's
   last command can wait on completion explicitly. That is the "completion signal" a CI step or an
   agent can use without a time budget.
3. **C-48 "held" output**: decide whether what a test program prints after `QUIET` counts for
   results (today it is forwarded, not parsed).

## Recommended order

1. **First PR (this branch, ready to go):** G5 and G6 product fixes; `Tuning::settle` +
   `testkit::env::SETTLE`; build_go.rs on it without `sleep 1`; the delayed-backend regression
   test; CHANGELOG. This fixes the flake that cost the most cycles. Linux-only verification
   so far: it needs one macOS run.
2. `SETTLE` in build_cc.rs, build_tsc.rs, build_gradle.rs and build_bake*.rs, dropping their
   sleeps and 15 s overrides (one PR, mechanical). Then build_share.rs.
3. CI: drop `|| echo`, add caches for jars, m2, gradle-home and node, and add the jars mirror
   (one PR, no Rust).
4. macOS: skip tool installs when checks will skip, then the split or merge-queue decision.
5. Injected clocks: Go cache-program watchdog, `run_copying_stdout`, then the relay's unit tests
   on paused tokio time.
6. Frozen-relay tests become "a backend that never answers" (no SIGSTOP).
7. .NET counts from TRX; pin CMake.
8. Product ADR: `close` amendments and build-outbox `flush`.
