# CI determinism: tests whose outcome changes with load

- **Status:** findings, and the product change of [ADR 0144](../adr/0144-no-time-limit-decides-what-a-build-reuses.md) built, 2026-11-25. Branch `claude/multi-lang-test-compile-ci-fil292-ci-determinism`.
- **Rules touched:** B-2, B-23, B-26, B-29, B-31, B-34, B-40, B-45, B-60, B-63, B-75, B-80 (ADR 0144); C-48 (proposed).

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

### The fix: the product, not the tests (ADR 0144)

A first prototype made the tests deterministic with a debug-only switch
(`HUSHRUN_BUILD_SETTLE`). That left every client (a team's CI, a person's laptop, an agent's
sandbox) exposed to the same load-dependent outcomes, so it was dropped. Instead, the product
itself no longer lets time decide what a build reuses:

- **G5, G6 are fixed.** The manifest is taken and its prefetch marked under one lock, with a drop
  guard; the C/C++ prefetch counter gets the same guard. The upload loop is marked running when
  it is started (`Relay::start_uploads`).
- **G1–G4 and every bound like them are gone.**
  - The end of a command waits for every upload the run built, on every machine, unless a stop
    signal came. Then `close` names them all. On a terminal it prints `uploading N blocks…`. A
    relay resting after failures, or stopped, ends the wait, since no new request goes.
  - Downloads are waited for to their end, and the rate meter and speed samples are removed.
  - Manifest and prefetch waits have no bound.
  - A rustc question asked while `open` is on the wire waits for it.
  - Vouches, CI's `open`, the baked-store check, the sandbox's B-23 check and
    `hushrun build fetch` run to their end.
- **The only timer left is silence.** The relay heartbeats every second, and every client waits
  on 30 s of silence, whatever the compile cost:
  - the rustc wrapper, its notices, the Go cache program (its watchdog counts heartbeats as life),
    Gradle's cache (which sends `102 Processing` to Gradle), the C/C++ and tsc launchers, and
    Xcode;
  - every backend and object-store request, with a PUT failing when its body stops moving.

  A frozen relay or a dead backend still ends a wait, so a slow one never does.

The tests needed no switch: the two-machine helpers dropped their `&& sleep 1`/`&& sleep 2`, and
the 15 s `HUSHRUN_CC_MANIFEST_WAIT_MS` overrides are gone, with the variable itself. Tests that
asserted the old bounds were rewritten to assert the new rules by outcome and order, never by
elapsed time. Examples: "the end waits for a slow PUT and closes with it", "a download is waited
for to its end whatever its cost says", "a CI job waits for `open` until it answers", "a request
with a held token never waits for the mint: every request carried token 1".

### Verification

On a 4-CPU container:

| Run | Result |
|---|---|
| `hushrun` unit tests (relay, uploads, sessions, bakes, everything) | 723 passed |
| `hushrun-build` unit tests, 3 runs in a row | 652 passed each |
| `hushrun-build` integration tests (cargo, cc, rustc, loader, process) | 75 passed |
| End to end: build_go 18 (+1 ignored), build_cc 11 (+1), build_tsc 4, build_gradle 2, build_share 71, two_machines 24, reuse 32, build 13, build_bake 6, build_bake_deps 6, build_bake_go_cc 3, build_bake_image 11, build_cc_make 6 | all pass |
| main's waits (measured with the first prototype's switch off, which kept them), 8 busy loops on 4 CPUs (load ≈ 16), 12 iterations of the Go two-machine tests | 12 of 12 iterations failed (`23 reused, 1 built; 1 block to upload waits for the next run`) |
| This branch, same load, same 12 iterations of the Go tests, then build_cc's whole suite each time | 12 of 12 passed (Go 3/3, cc 11/11 each) |
| This branch, same load, build_share's whole suite 3 times | 3 of 3 runs passed, 71/71 each |
| An 8 s stall injected on A's first upload over 1.5 MB (not committed), the original Go test | main's waits: 3 of 3 failed as CI did; this branch: 3 of 3 passed (13 s each: the end waits the stall out) |

| Gradle 8.14 for real, its socket timeout set to 1.5 s, B's downloads held 3 s | passes; with the cache's `102 Processing` heartbeats switched off (not committed) it fails: "Read timed out … The remote build cache was disabled during the build" |
| Maven 3.9.11 for real, B's downloads held 3 s | both modules from the shared store |

`every_package_is_reused_on_the_other_machine_however_slowly_the_backend_answers` is the
regression guard. It holds A's PUTs, B's `open` and B's downloads past every old bound and
expects every package reused, so a time-bounded wait that comes back fails it on any machine.

### Residual

- **B-31 forbids minting an ID token once a command's end has begun.** A CI drain that outlives
  the token held fails its remaining uploads, and says so.
- **Job-level budgets stay:** a bake's whole build budget, and `PUBLISH_WITHIN` for a baked
  image.
- **Xcode's own cache client** keeps whatever timeouts it has.
- **tsc's e2e tests** skip here (no TypeScript installed). CI runs them.

## Inventory

Effort: S is under half a day, M is 1–2 days, L is more. "Shared helper" rows affect many tests.

### Two-machine build tests: done by ADR 0144

| Test / helper | Cause | Class | Fix | Effort |
|---|---|---|---|---|
| build_go.rs `Machine::run`, `gitlab_go_run` (`&& sleep 1`, 15 call sites) | G1–G6 | b | **Done**: product fix, no sleep | – |
| build_go.rs `every_package_of_a_pure_go_command…` | G1–G6 | b | **Done** | – |
| build_cc.rs `Machine::run` (`&& sleep 1`, 18 sites), `HUSHRUN_CC_MANIFEST_WAIT_MS=15000` | G1, G3, cc manifest wait | b | **Done**: no sleep, no override (the variable is gone) | – |
| build_tsc.rs `run` (`&& sleep 1`, `MANIFEST_WAIT_MS=15000`) | same | b | **Done** | – |
| build_share.rs `Machine::build`, `ci_build`, `greet` and about 24 inline `&& sleep 2` | G1 (CI drain bounded by cost/4) | b | **Done**: sleeps removed (the SIGTERM test's `sleep 20` stays, it is the command being stopped) | – |
| build_share.rs `a_merge_whose_ci_skipped_is_built_by_the_bake_before_it_bakes`, `a_gitlab_bake_that_builds_keeps_its_store` | G1 (bounded CI drain) | b | **Done** by the product fix; under load in the table above | – |
| build_gradle.rs, build_maven.rs | G2–G4 for `gradle/1` and `maven/1`, and the clients' own socket timeouts | b | **Done**: event waits, `102 Processing` heartbeats; a test each holds downloads past the timeout | – |
| build_bake_*.rs | fetches raced G2–G4 | b | **Done** | – |

### Tests of a bound: replace wall-clock asserts with an injected clock

These assert `elapsed < N` or rely on "X happens within a sleep". The right shape is a
`Clock` the code under test reads, with `Instant::now()` in a release build and a manual clock in
tests. Then "the watchdog answers after the budget" is asserted by advancing the clock and
waiting on the answer. Nothing sleeps.

| Test | Cause | Class | Fix | Effort |
|---|---|---|---|---|
| hushrun-build go/program/tests.rs `a_handler_that_hangs_or_panics_still_gets_one_answer` (PR #53: order plus 3 s; failed on main at 3.006 s) | the watchdog's budget against a panic's backtrace | a | **Done**: the watchdog and `close` read a `Clock` (the wall clock, or a test's manual one). `slow-get` waits until the watchdog answered its request, `hang-get` until the program is done. The test moves the clock only after the handlers answered, and asserts the order: before it moves, only the panics are answered. | – |
| same file: `after_one_relay_call_times_out…`, `a_fetch_that_outlasts_its_wait…` | elapsed-time asserts | a | **Done**: they assert what the relay heard (one request; a heartbeating relay asked twice) | – |
| same file: `a_store_hit_the_watchdog_answered_first…` (slept 900 ms, so it passed vacuously when slow) | same | a | **Done**: the slow handler goes on once the watchdog answered, and `close` waits for it; no sleep | – |
| same file: `Steps` (fed the next input after 10 s whatever the output) | a deadline that changed the order under load | a | **Done**: each step waits for its lines; five minutes with none fails as a hang | – |
| hushrun units/program.rs `output_that_never_stops_is_handed_over…` (`took < MOST_AFTER*4`), `a_program_that_keeps_output_open_past_its_end…` (`< MOST_AFTER*2`) | `run_copying_stdout`'s held decision is `QUIET`/`MOST_AFTER` on `Instant` | a, **and b** | Pass a clock into the read loop. Assert `held`, `cut` and the bytes, never elapsed time. Product: what a program prints after `QUIET` is forwarded but **not kept**, so a slow child's last result lines can miss the parser (e2e_go `what_a_program…left_running…` relies on it). Decide in C-48 whether "kept" may depend on QUIET. | M |
| build_relay/tests.rs: the bound tests (`a_server_that_sleeps_on_open…`, `a_block_whose_stated_cost_is_absurd…`, `a_ci_job_s_end_sends…`, `only_a_job_writing…`, `a_stalled_begin_or_commit…`, `a_stale_vouch…`, `a_slow_id_token_endpoint…`, `a_ci_job_waits_for_open…`, `on_a_slow_link…`, the two panicking runs) | wall-clock asserts on the relay's old budgets | a | **Done**: rewritten to the new rules, asserting outcomes and order | – |
| build_relay/tests.rs: what is left with ms asserts (`a_cold_build_s_questions_wait…` 48 asks <100 ms each, `a_cc_open_that_answers_after_its_check_ended…`, `three_failures_rest…` <200 ms) | wall-clock asserts | a | The relay takes `tokio::time` everywhere already. Run these tests under `#[tokio::test(start_paused = true)]` or on a runtime with paused time. Tokio's auto-advance then makes every `sleep` and `timeout` instant and exact, and the bounds become exact equalities. Needs the relay's runtime to be built in the test, and `Instant::now()` replaced by `tokio::time::Instant::now()` in the relay. | L (many tests, mechanical) |
| build_go.rs `a_frozen_relay_costs_a_run_one_wait…` (SIGSTOP, `date +%s`, SIGHUP at 51 s on macOS) | freezes the whole hushrun process (the relay is a thread), so `sh` and `go` under it are also its children and session | a | Freezing isn't needed to test "one wait per run". A fake whose `open` **never answers** (`fake.hang(route::OPEN)`) gives the same relay state with no signals, and `RELAY_DOWN` (the file the first timeout writes) is the event to assert on. Assert that the second and third go commands asked the relay nothing (the fake's `seen()`), not seconds between stamps. | M |
| build_cc.rs `a_frozen_relay_costs_a_build_one_wait…` | the cc twin, also `sleep 1500 ms` before SIGSTOP | a | same | M |
| units_swift.rs:1252, units_ctest.rs:2884 `…left_running_keeps_printing…` (<2 s against `sleep 2; echo late`) | the late line is timed | a | The fake prints its late line when a FIFO the test opens after hushrun returns is written: ordering by event | S |
| e2e_go.rs `output_held_past_go_tests_own_wait…` (8 s against go test's 5 s WaitDelay) | Go's own timer | a | Inherent to Go's `WaitDelay`. Keep it, or own the 3 s margin explicitly. Low frequency. | – |
| jest/node/vitest "late timer" tests (a 5–20 ms timer against a 150 ms test) | the error can land in a neighbouring test | a | The fixture's late callback fires on a promise the next test resolves, not on a timer | S each |
| run.rs `failures_are_recorded_retried_and_exit_8` (`timeout: 1s` of `sleep 5`, <4 s), isolated.rs, login.rs, update.rs, notice.rs, setup/repeats.rs, measure.rs, oidc.rs, ledger http tests | asserts the product's own timeout fired in time | a | Only an upper bound can flake. Keep the lower bounds, and turn upper bounds into "the child was killed" or "the request was abandoned" (state), not time | S each |
| units.rs:1213 `rust_test_binaries_are_observed…` (a failing test sleeps 3 s so the others finish first) | scheduling order | a | Order the binaries with a FIFO barrier the failing one waits on | S |
| units_dotnet.rs VSTest console summaries joined on one line (PR #54) | parsing an interleaved console | a/b | The adapter already writes and reads TRX (`units/dotnet/trx.rs`). Take counts from TRX and use the console only for the person. That removes the class rather than the case. | M |
| units_ctest pinned to CMake 3.22 behaviour | the runner image's CMake changes under the test | c | Pin CMake in CI (like Maven and Gradle, by checksum), or assert on the version found and branch explicitly. Don't let `ubuntu-latest` choose. | S |
| hushrun-build wrapper.rs `a_file_written_just_before_the_compile_is_not_taken…` (a 5 ms sleep assumed to cross a file-clock tick) | the kernel's tick and scheduling | a | **Done**: probes until the file clock moved past the file's date | – |
| other mtime ticks (`sleep 20–30 ms`, 1100 ms for Ninja and Mix) | coarse file clocks | a | Loop-until-mtime-changed helpers already exist (units_ctest). Use them everywhere, and set mtimes explicitly with `filetime` where the test only needs "older". | S |

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

1. **Manifest amendments** (`hushrun flush` for build blocks): would let a person's command exit
   before its uploads finish without its manifest falling short. This needs a protocol change and
   the backend's support (hushrun/cloud). Worth doing on top of ADR 0144, not instead of it.
2. **C-48 "held" output**: decide whether what a test program prints after `QUIET` counts for
   results (today it is forwarded, not parsed).
3. **B-31's no-mint-at-the-end rule** against a long CI drain: decide whether the end may renew
   the job's ID token.

## Recommended order

1. **First PR (this branch):** ADR 0144 with its spec rules, product change, tests and CHANGELOG.
   It needs one macOS run.
2. CI: drop `|| echo`, add caches for jars, m2, gradle-home and node, and add the jars mirror
   (one PR, no Rust).
3. macOS: skip tool installs when checks will skip, then the split or merge-queue decision.
4. Injected clocks: `run_copying_stdout`, then the relay's remaining ms-bound unit tests on
   paused tokio time. (The Go cache program's watchdog is done.)
5. Frozen-relay tests become "a backend that never answers" (no SIGSTOP).
6. .NET counts from TRX; pin CMake; the other mtime tests.
7. Product ADRs: manifest amendments; C-48; B-31's end-of-run mint.
