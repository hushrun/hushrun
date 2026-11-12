# 0083. `go test`'s tests one by one

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-45, C-35; relies on C-32, C-34, C-39, ADR 0070, 0077, RFC 0004

## Context

ADR 0070 reuses a Go package's tests as one unit. ADR 0077 adds the test as a second, finer unit
(`tests.unit: test`), with pytest as the adapter that knows tests, and leaves Go saying once that
it reuses by package. RFC 0004 §4.4 asks Go to know tests: by name, left out by hushrun's runner
with `-test.skip`, their outcomes read from the binary's own output, with no flag that changes
what the binary or `go test` prints. This decision is that adapter. What each test read stays its
package's: RFC 0004 §6.

## Decision

1. **A test** (C-45) is a top-level `Test…`, `Example…` or `Fuzz…` function of a package, by its
   name: `cart::TestTotal`. A subtest belongs to its top-level test, and a fuzz target's seeds to
   it. Benchmarks aren't tests here, and an example with no output comment isn't run, so it isn't
   listed.
2. **Listing.** With `unit: test`, hushrun's runner, which starts every test binary (ADR 0070),
   first starts it with `-test.list=.` and reads what it prints: tests, benchmarks, fuzz targets
   and examples. The start has the run's environment, its standard input closed, its standard
   error discarded, `GOCOVERDIR` in the run's own directory, and 60 seconds. A list any line of
   which isn't a Go name (a package's `init` printed something), a binary that fails, or one that
   takes longer: its tests aren't told, and the package runs as a package. Listing runs the
   package's `init` once more, as `go test -list` does; it would run a `TestMain` once more too,
   so a binary that has one (its table of function names holds `<package>.TestMain`, even when it
   is stripped) isn't listed: such a package is `not observed` anyway (ADR 0070). A package the
   plan reuses whole isn't started at all, as before: its tests are those this machine's store
   has for it, each left out with it.
3. **Selection.** For the tests `hushrun.json`'s `reuseTests` lists that the binary listed, the
   runner adds `-test.skip=^(TestA|TestB)$` before COMMAND's own arguments, where Go's `flag` is
   sure to read it: one element, so Go matches only top-level names, anchored at both ends, each
   name quoted as `regexp.QuoteMeta` does, though Go names hold no metacharacter. A test of the
   plan's that the binary no longer has isn't named. A pattern longer than 64 KiB leaves nothing
   out. `-test.skip` is Go 1.20's: the listing asks for it with `-test.skip=^$`, and a binary that
   refuses it is listed again without; its tests then run, are told, and the runner says once a
   run, on the binary's standard error, since it knows no check's name: `hushrun: go test's tests
   are reused by package: this Go can't leave out one test (Go 1.20 and later can)`. A test proves
   the pattern against a real binary: names that are prefixes of others, a subtest named as a
   top-level test, an example, a fuzz target, a name outside ASCII.
4. **Outcomes, from the binary's own output and its end.** The runner passes what the binary
   prints on byte for byte, through a pipe of its own: one for standard output and error when the
   runner's are one file, as `go test` makes them, so that their order is kept. It reads each line
   as it goes by. Then:
   - a test is **failed** when any `--- FAIL: ` names it, anywhere in a line (after a test's output
     with no newline), by its top-level name (up to a subtest's `/`, `-cpu`'s `-4` or a
     duplicate's `#01`). A test that prints `--- FAIL: TestX` itself, or an example's `got:` that
     holds one, makes `TestX` failed: reading too many failures only runs a test again;
   - a test **passed** only when the binary's run **ended normally** and the test was listed, not
     left out by hushrun, and named by no `--- FAIL: `. A run ended normally when Go's test main
     returned from running the tests (below); the output was read to its end, every line shorter
     than 1 MiB, within 5 seconds of the binary's exit (Amendment 1: `go test` itself waits longer,
     and the runner passes the output on to its end); its last line but `coverage: …` lines is Go's summary,
     marked with `\x16` under `-json` (`-test.v=test2json`, where Go marks its own lines) and
     possibly after a test's output with no newline otherwise; and the summary is `PASS` with
     exit 0 and `-test.paniconexit0` (so `os.Exit(0)` in a test is a panic), or `FAIL` with exit
     1, a `--- FAIL: ` seen, every name a `--- FAIL: ` gave one the binary listed, and no
     `-failfast`;
   - **Go's test main returned.** The runner adds `-test.memprofile=<a file of the run's
     directory>`. `M.Run` writes that profile only in the end it defers, after its summary, and
     prints nothing for it. A test that prints `FAIL` and exits 1, `os.Exit` anywhere, a panic, a
     timeout or a signal never gets there. Without it, a test that printed Go's failure and summary
     and exited would make every test after it a pass it never had. COMMAND's own `-memprofile`
     would take its place: its tests aren't told;
   - a binary that ended any other way records the failures it named and no pass: every other test
     of it didn't finish, and is `not run`.

   What was seen with Go 1.27, without `-v`: a pass prints `PASS` alone; a failure
   `--- FAIL: TestMid (0.00s)`, its log indented, then `FAIL`, exit 1; a parallel subtest's
   `t.Fatal` both its parent's line and its own, indented; a failing example `--- FAIL:
   Example_e`, `got:`, `want:`, at the start of their lines; a fuzz seed `--- FAIL: FuzzF`; a
   panic `--- FAIL: TestMid` then `panic: … [recovered, repanicked]` and a stack, exit 2;
   `os.Exit(0)` the same, with `panic: unexpected call to os.Exit(0) during test`; `os.Exit(1)`
   nothing, exit 1; a timeout `panic: test timed out after 2s`, `running tests:` and the test,
   exit 2, with no `--- FAIL`; `-failfast` stops starting tests after the first failure and still
   prints `FAIL`; `-count=2` names a failing test twice; `-shuffle` prints `-test.shuffle <seed>`
   first; output with no newline glues Go's next line to it (`no newline--- FAIL: TestMid`), and
   with `-json` Go's own line carries `\x16` after it; `-cover` prints `coverage: …` after the
   summary; `-test.count=0` runs nothing and prints `PASS`. `t.Skip` prints nothing without `-v`,
   and a skipped test passed, as a test file of skipped tests does.
5. **COMMAND's own `-run` or `-skip`: by package, outright.** Which tests ran then is Go's
   matching: a pattern split at unbracketed slashes into elements matched against subtests, an
   alternation at the top level treated apart, a top-level test that runs only some subtests, and
   RE2's syntax, which Rust's regex crate reads differently in places. hushrun doesn't reproduce
   it. A COMMAND with `-run` or `-skip`, or `-test.run` or `-test.skip` after `-args`, gets
   `unit: file` and the run says once `hushrun: <NAME>: its tests are reused by package: go test
   -run's tests can't be reused one by one yet` (or `-skip's`). A selection COMMAND doesn't show
   (in `GOFLAGS`), and `-test.list`, `-test.fuzz` or a `-test.count` that isn't a positive number
   in a binary's arguments: the runner tells none of its tests, and the package runs as a package.
6. **Durations.** A test's time is the one Go printed for it in its own `--- PASS`, `--- FAIL` or
   `--- SKIP` line, to 10 ms, summed over `-count`; with neither `-v` nor `-json`, Go times only
   the tests that failed, and a test that passed has 0: hushrun doesn't share a binary's time out
   among its tests. So "saved about …" counts only time that was measured, and a run that reuses
   Go tests timed at 0 says `; 12 tests reused`.
7. **A package's own facts.** A package one of whose tests hushrun left out didn't run whole: its
   facts say so, so that it records no pass of its own whatever reads them (C-39's two tiers do,
   and so does a run past 50,000 tests, which reads by test file). Everything ADR 0070 says stays:
   which COMMANDs are offered, Go's test cache off under `-exec`, the test log, and what makes a
   package `not observed` (a `TestMain`, the repository's native code, a program started, no log),
   which is so for every test of it, since Go's log is the package's. `unit: file` is unchanged:
   the runner lists nothing, adds nothing and pipes nothing.

## Consequences

- A Go package with one failing test runs that test, and one more as a check, the next time; the
  others are reused one by one until something they read changes.
- With `unit: test`, a test binary's output goes through a pipe of hushrun's: a test that asks
  whether its output is a terminal sees a pipe, as it does under `go test` testing several
  packages.
- Each package hushrun runs test by test starts its binary twice: once to list, once to test.
- A test that prints Go's words, or a binary whose `init` prints, costs reuse, never a wrong one.
- With Go's default output, reuse of Go tests saves time hushrun can't measure, and says so by
  count.

## Alternatives considered

- **Adding `-test.v=test2json` and reading Go's events:** every test's start and end, with times,
  but COMMAND's output would change.
- **Reading the test main `go list` writes for the list:** no second start, but `go list` asked
  after COMMAND can name a build other than the one that ran, and a test it names that didn't run
  would pass.
- **Passes by subtraction from the summary alone:** a test that prints `FAIL` and exits as a
  failed run would read as a normal end.
- **Reproducing `-run` and `-skip`:** possible for plain top-level names, but a subtest pattern
  runs part of a top-level test, and proving a regex engine against RE2 for every pattern isn't
  certain. COMMAND is in every key, so a check with `-run` still reuses by package.
- **Listing a package reused whole:** it would start a binary whose package the plan reuses, and
  run its `init`, only to count its tests.

## Amendment 1: a test binary's output is passed on to its end (2026-11-12)

§4 read the binary's output for 5 seconds after it exited, "what `go test` itself waits by default
for a program the test left holding it", and the runner then exited. A reviewer showed the premise
wrong: with a test that started `sh -c "sleep 12; echo late"` on its standard output, Go 1.27's `go
test` printed `late` and waited the full 12 s, with hushrun's `unit: file` too, but with `unit:
test` it printed nothing late and stopped after 5 s, since the output went through the runner's
pipe and the runner had exited.

The decision: the runner decides the tests as before (no test of the start passes when the output
was still open 5 seconds after the binary exited: a program holding it may print anything into it),
writes its facts, and then passes the output on until every program lets it go, as `go test` reads
it without hushrun.

## Amendment 2: skips and starts (2026-11-12)

A test whose own `--- SKIP` line, or a subtest's, the output holds is told `skipped`, so that in a
run whose COMMAND failed it records no pass (ADR 0077 Amendment 2). Go prints those lines with `-v`
and `-json` only; without them a skip can't be told from a pass, which Amendment 3 decides. Each
runner's facts name the start
of `go test` that started it, by that process (`start`), so that one `go test` testing several
packages is one start.

## Amendment 3: a package that failed without `-v` keeps no pass of its other tests (2026-11-12)

Without `-v` or `-json`, Go prints a line only for a test that failed: a test that passed and one
that skipped, perhaps because another failed (`if !ready { t.Skip() }` after the setup test
failed), look alike. Amendment 2 left such a skip recorded as a pass in a run that failed, which
the next run would leave out.

The decision, in the one function that turns a binary's output and end into its tests' facts: when
the binary's run failed (it ended normally on `FAIL`) and its arguments don't make Go name each
test's outcome (`-test.v`, true or `test2json`), every test it didn't name failed is told as not
finished. The core then records nothing for them (`not run`), without saying they failed; telling
them `skipped` would claim what wasn't seen. The tests it named failed are recorded
failed, and the package's own fail is recorded as before. A binary that passed is unchanged: every
listed test hushrun didn't leave out passed, a skip included. The runner's facts say why
(`settings.note`), and the run says it once, `hushrun: <NAME>: a package that failed keeps its
other tests' results only when go test names each one: add -v to keep them`; each such test's
facts carry the same sentence (`notKept`), which this machine's store keeps with its `not-run`
row, and `hushrun why NAME 'pkg::TestX'` says it from there. A start that didn't end normally
gives each test it left without a result its own reason the same way: the binary ended before
Go's test main returned (a panic, a timeout, `os.Exit` or a signal), it failed under `-failfast`,
or its output couldn't be read to Go's summary. The reason is decided once, where the test's
state is, and never derived again from COMMAND's flags: a `-v` that only `GOFLAGS` gives makes Go
name each test, and a panic leaves tests unrun that `-v` wouldn't keep.

The trade, plainly: without `-v`, Go's tests one by one save only two things. In a package that
passed, a test that did something hushrun couldn't follow costs only itself; in a package that
failed, nothing: every test of it runs again, as it would by package. With `-v` or `-json` each
test's outcome is seen, and a failed package runs again only the tests that failed or read a
changed file.

Considered and not done: adding `-test.v` to the binary's arguments and filtering its lines back
out. It changes what COMMAND prints (Go's non-verbose output holds a test's log only when it
fails, and in other places than `-v`'s), and hushrun never changes what COMMAND prints.

## Amendment 4: output held open is handed on, and `go test` decides (2026-11-12)

Amendment 1 said the runner passing the output on to its end reads it "as `go test` reads it
without hushrun". That holds only for a program that lets the output go within `go test`'s own
wait. `go test` waits for a binary's output only so long after the binary exits (Go 1.27:
`WaitDelay`, the larger of 5 s and a tenth of `-timeout`, none with `-bench` or `-fuzz`), then
prints `*** Test I/O incomplete … after exiting.` and fails the package. Under hushrun `go test`
waits on the runner; a runner that exited only at the output's end turned that failure into `ok`,
and the check recorded a pass (a reviewer: plain `FAIL … exit 1` against `ok … unit passed`). The
runner that exited after 5 s before Amendment 1 turned it into `ok` too.

The decision: the runner reads the binary's output as hushrun's `swift` does (ADR 0086 Amendment
3), with the one function both use: once the binary exited, until the output ends or has been quiet
for 250 ms (at most 1 s). Output still open then: no test of the start passes (the reason as
before), the runner writes its facts, hands the open pipe to a forwarder it starts and doesn't wait
for (hushrun under a hidden subcommand, the pipe as its standard input, the runner's standard
output, `go test`'s pipe, as its own), and exits with the binary's code. `go test` then sees what it
sees without hushrun, the binary gone and its output still open, and its own wait decides: late
output inside it arrives and the package is `ok`; past it, `go test` prints its line and fails the
package, on its own timing. The runner needs no knowledge of the delay.
