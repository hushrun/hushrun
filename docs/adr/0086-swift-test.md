# 0086. `swift test`'s test targets and tests

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-48; relies on C-30, C-31, C-32, C-34, C-36, C-38, C-39, ADR 0060, 0070, 0077

## Context

RFC 0004 adds a test as a unit beside the test file (ADR 0077) and new adapters, one of them for
`swift test`: SwiftPM's tests, in XCTest and in Swift Testing. SwiftPM takes nothing from the
environment that would let hushrun inside it, so hushrun gets in the way ADR 0077 built for it: a
program named `swift` first on COMMAND's `PATH` that is hushrun. A spike on Swift 6.4.2, on macOS
and on Linux, found what can be relied on and what can't; this decision is what was built from it.

## Decision

1. **A unit is a test target** (C-30, C-48), named by its directory from the repository root, as
   Go's package is (ADR 0070). In what people read it is a `test target`. Its files are its
   sources: the files of a kind SwiftPM compiles below its directory (Swift, C, C++, Objective-C,
   their headers and assembly).
2. **A test** is what `swift test list` prints after the target: XCTest's `Class/method`, Swift
   Testing's `Suite/Nested/func(labels:)` or `func(labels:)` outside a suite, a raw identifier with
   its backticks. A parametrized test is one test: `--skip` can't leave out one of its cases. Two
   `private` tests of one name in two files get `/File.swift:line:column` after their ids, which
   moves when a line does: such a test is never left out, nor reported one by one. The adapter
   leaves it out of its `--skip` patterns and out of the tests it reports (it sends no
   `neverLeftOut`): it runs whenever its target runs, counts in no total of tests, and keeps no
   result of its own; a failure of it fails its target, whose other tests then keep no pass, as
   for any failure no reported test explains (C-39).
3. **Reads, before the run.** `swift package --package-path P --scratch-path <hushrun's own>
   describe --type json`, followed into every dependency of type `fileSystem`, gives each target's
   directory, its targets and its products; `dump-package` gives the settings `describe` leaves
   out. Neither resolves, fetches or builds, and with the scratch path neither writes into the
   package (`show-dependencies` resolves and writes `Package.resolved`: never asked). A test target
   read every file below its directory and below the directory of each target it depends on, in
   its package and in local packages, and each header search path. **Sources** are the files of a
   compiled kind below a target's directory, a binary or system library target aside. Everything
   else is in the rest set: every `Package.swift` and `Package@swift-*.swift`, `Package.resolved`,
   resources, module maps, a binary target's files, a file below no target's directory.
4. **What can't be followed**, each decided:
   - *A source a target lists outside its directory* (`sources: ["../x.swift"]`): `describe` drops
     it, and so does the build (ran: the test that used it didn't compile). Nothing to follow.
   - *Remote packages* (`sourceControl`, `registry`): pinned by `Package.resolved`, in the rest set.
     A test target that uses a remote product while the root package has no `Package.resolved` in
     the scope is `not observed` (`unpinned`).
   - *Remote binary targets*: pinned by URL and checksum in the manifest, in the rest set. A local
     one's files are in the rest set.
   - *Macros*: a macro target is a target: its sources are followed; `swift-syntax` is remote,
     pinned as above.
   - *Build-tool plugins and their tools*: the plugin target and its tool are targets, followed; a
     plugin may read any file of its package, so a target built with one reads its package whole.
   - *`unsafeFlags`* that may name a file or a directory (a `/`, `@`, `-I`, `-F`, `-L`,
     `-isystem`, `-include`, a module map, a VFS overlay, a plugin path, …): the target is `not
     observed`. Other flags (`-warnings-as-errors`) change nothing.
   - *A header search path* is followed: the directory is read.
   - *A local package outside the scope*, or a directory hushrun reads with no file in the scope:
     `not observed` (`outside-scope`).
   - *System library headers, the toolchain, the environment, a file a test opens by its path at
     run time*: not followed, as no adapter follows the machine. A test that reads another
     target's sources at run time is the audit's case (C-34).
5. **Loading.** `swift`, and `swift-test` where COMMAND's `PATH` has one (Linux), are hushrun's
   programs on `PATH` (ADR 0077). They catch `swift test` typed, or started by a script, a
   Makefile, `sh -c` or `env`. `xcrun swift test` and a path to `swift` go round them: COMMAND in
   either form is refused, said once, and a script that does it writes nothing, which the run says
   once (`swift test wrote nothing for hushrun: no swift test reached hushrun's swift on PATH`). Every other subcommand, and
   `swift test list`, `last`, `--help`, `--version`, `--list-tests`, `--show-codecov-path`,
   `--debugger`, passes through untouched. The real `swift` gets `PATH` as COMMAND had it, so a
   `swift test` that a test starts never reaches hushrun: that is the child's mark. The adapter is
   chosen for `swift test …`, `swift-test …`, and, where a `Package.swift` is, a wrapper (a
   shell's `-c` line, a `#!` script, `make` with its Makefile) whose own text starts `swift test`
   or `swift-test` and names no other runner (C-32's one rule, shared with CTest's adapter). Any
   other COMMAND beside a `Package.swift` keeps the pytest adapter, as before this adapter: a
   wrapper that runs pytest loses nothing.
6. **What the person sees** is what `swift test` printed, and its exit code. Standard output goes
   through a pseudo-terminal in raw mode, with the window's size, when the person's is a terminal
   (Swift Testing colors only on one), else a pipe, and is copied as it goes by; standard error is
   not touched; a signal that stops the real `swift` stops hushrun's the same way. The flags
   hushrun adds show only in SwiftPM's own error after a Swift Testing crash, which prints the
   helper's command line, and `--skip`'s warning when everything was left out.
7. **Nothing is listed before the run.** A listing first builds, and the real run then prints
   neither the compiler's warnings nor `Fetching` lines. After the run, `swift test list
   --skip-build` with COMMAND's options names the tests, and every test the list holds is reported.
   A plan's tests come from this machine's store, as for every adapter (C-39).
8. **Leaving out.** For each target `reuse` lists, `--skip '^Target\.'`; for the tests
   `reuseTests` lists, one `--skip` per class or suite, `^Target\.Suite/(?:a\(\)|b\(\))(?:/|$)`,
   every regular expression metacharacter escaped; added to COMMAND's own `--skip` and `--filter`.
   XCTest is matched against `Target.Class/method`, Swift Testing against the id and its
   location, so `(?:/|$)` ends both; an unanchored pattern would leave out `testTotalWithTax` with
   `testTotal`. A class with more tests gets more values, each within 64 KiB, and none is added
   when they would take more than 512 KiB of command line and environment: then nothing is left
   out, said once. Everything left out exits 0 with SwiftPM's `No matching test cases were run`.
9. **When nothing is left out.** Two SwiftPM faults make any `--skip` wrong: on macOS, a nested
   XCTest class (`enum Outer { class InnerTests }`) silently doesn't run under any `--skip` or
   `--filter`, and under `--parallel` its xunit says it passed; on Linux, the XCTest tests that
   remain go to the runner as one argument, which must stay under 128 KiB. Both are known from the
   previous run's list, which the store keeps as its list of what the runner listed (C-36), each
   `<unit>::<id>`: a plan whose list has a nested XCTest class (macOS), or XCTest ids that come to
   more than 96 KiB on Linux (512 KiB elsewhere, room for tests added since), leaves nothing out,
   said once. And after a run that left tests out, this run's list is checked again: a nested
   class in a target that ran (macOS), or a run that failed with no XCTest test started and XCTest
   ids past the limit (Linux), runs `swift test` again with nothing left out, with a line, and that
   run is the one reported. That check is the guard: it already catches a class added since the
   list, and a plan with no list at all (a fresh machine whose hints carry the targets' lists)
   leaves out as it plans and relies on it; the list kept only saves the second run. The list
   stays where it is (the store's list of what the runner listed) rather than in the store's
   tests, which hold ids only for `unit: test`, while the faults matter for `unit: file` too.
10. **Outcomes** (C-48), never a pass from absence, never an id the list after the run doesn't
    hold:
    - *Swift Testing:* the event stream, `--event-stream-output-path` to a fresh path of hushrun's
      own (`--event-stream-version 0`), never the xunit file. A test passed with a start, an end
      and no issue that is an error and not known; a skip is a skipped test, which passes; a start
      without an end didn't finish, and is a crash when the run failed; never started is not run.
      After a crash on macOS with an xunit path given, SwiftPM writes no event stream at all: its
      tests are not run.
    - *XCTest, serial:* SwiftPM writes no xunit file, so the console is the record, read from the
      copy of standard output: `Test Case '-[Target.Class method]' passed (0.001 seconds).`, a
      nested class by its mangled name, demangled; on Linux `Test Case 'Class.method' …`, with no
      target, which names a test only when one target has it.
    - *XCTest, `--parallel`:* the xunit file, added only then. An XCTSkip reads as passed, which
      is what hushrun records for a skipped test anyway; a nested class that never ran reads as
      passed on macOS, so under any `--skip` or `--filter` its tests count for nothing.
    - A user's own path is read only when the run wrote it. A failed run that no test explains, a
      build that failed or a Swift Testing process that ended with nothing in flight, keeps no
      pass.
11. **Which COMMANDs.** `swift test` with its options, `--filter`, `--skip`, `--parallel`, `-c`,
    `--test-product`, `--disable-xctest` and the rest. Refused, said once: `--enable-code-coverage`
    (a listing after the run could add to the coverage data), `--repeat-until` and
    `--maximum-repetitions` (a test runs more than once), a Swift older than 6.0. Proven on 6.4.2.
    **Windows** isn't claimed: the adapter refuses there, and nothing is observed.

## `xcodebuild test`, not built

It would take the same `PATH` program, named `xcodebuild` (catching scripts and fastlane, not
`xcrun xcodebuild`); a list from `-enumerate-tests` with the command's scheme, destination and test
plan, which builds first; leaving out with `-skip-testing:Target/Class/method`, literal identifiers
in a response file, so no escaping and no argument limit; outcomes from a result bundle hushrun
names unless the command names one, read with `xcresulttool`; and reads from the Xcode project's
own graph, not `describe`. None of it is built.

## Consequences

- A Swift package reuses test targets on what SwiftPM says they are built from, and with
  `unit: test` its tests one by one.
- A package with a nested XCTest class leaves nothing out on macOS, until the class moves to the
  top level.
- `swift test` takes, besides, a `swift --version` and a list after the run, and a `describe`
  before it when something is left out.
- What a test opens at run time isn't followed: the audit is the net.

## Alternatives considered

- **Listing first:** it changes what the person sees (above).
- **The xunit file for everything:** serial XCTest writes none, and after a crash it is missing or
  short.
- **One `--skip` per test:** the matchers are quadratic; grouped patterns took 8 s where one per
  test took 22 s and more, for 4,000 of 8,000 tests.
- **Reading the manifest as text** for unsafe flags and paths: `dump-package` gives them as SwiftPM
  reads them.

## Amendment 1: what makes a pass of `swift test` (2026-11-12)

Reviewers found five ways a test target or a test was left out, or recorded as passed, when it
should have run:

- **`--skip-build`** runs the test products an earlier build left. `swift build --build-tests`, a
  change, then `swift test --skip-build` recorded the old products' passes for the files as they
  were, and the next run reused a target that fails on them. CTest's adapter has its `stale
  program`; SwiftPM says nothing of whether its products are up to date.
- **A build-tool plugin of another package** shows in `describe` only as a product dependency, so a
  target built with it didn't read its package whole, as decision 4 says a plugin's target does.
- **A console pass with no start**: a `Test Case '…' passed` line counted for a test the console
  never said started, as a test that prints XCTest's words, or a line after a crash, can print.
- **A list after the run that fails** after something was left out left that run's result standing,
  although what SwiftPM ran of what was left out couldn't be checked.
- **Skips** read as passes, in a run that failed too.

The decision:

- `--skip-build` makes the run not observed, with a line saying why: nothing is left out and
  nothing recorded. A plain `swift test` builds first, whatever `--build-path` or `--scratch-path`
  names, so it stays observed; `--skip-update` builds as usual.
- A target whose `dump-package` names any plugin usage, or that depends on a product `describe`
  says is a plugin, makes what depends on it read its package whole.
- A pass or a skip counts only as the end of a start the console said, once; a failure counts
  whatever came before it.
- When the list after the run fails and the run left something out, the facts file says so as left
  out but not listed (ADR 0077 Amendment 4): COMMAND starts once more with nothing left out.
- XCTSkip, and Swift Testing's disabled tests and conditions, are told as skipped (ADR 0077
  Amendment 2). Under `--parallel` the xunit file reads an XCTSkip as a pass, which can't be told
  apart: in a run that failed, each XCTest pass read there is told as possibly skipped, so none
  records a pass; in a run that passed nothing changes.
- Output a program `swift test` started still holds 5 seconds after it ended is no longer waited
  for: hushrun's `swift` stops passing it on then, as hushrun's target runner does for a test
  binary (ADR 0083), and records no pass of that run (its failures stand). It used to wait for the
  end of output, for ever if a program left running kept it open.
- Each hushrun `swift` names a start of its own in its facts file, and the plan's `describe` and
  `dump-package` get COMMAND's own `env NAME=value` assignments (ADR 0077 Amendment 7): a manifest
  can read the environment, so `env WITH_EXTRA=1 swift test` may build a target with a dependency
  only it names.
- The real `swift` is the one a shell would start: an empty `PATH` entry is the current directory
  and a relative one is from it, and a link into a directory of hushrun's programs is passed over.

## Amendment 2: a source no test target reads is in the rest set (2026-11-12)

Decision 4 made every file of a compiled kind below any target's directory a source. A target no
test target depends on (a library nothing here uses, a plugin nobody uses here) was then on no list
and in no key: a change to it ran nothing, though a test may build or read it.

The decision, the rule: **a file of a source kind that the graph gives to no test target of the
check is in the rest set, so a change to it runs every test target; a file the graph gives to a
test target is on that target's list only.** Sources are the files of a compiled kind below a
target's directory that a test target reads. A test can start an executable target (or a macro's,
a separate executable) from the build's products without depending on it, so the graph gives the
files of every executable and macro target to every test target of the package: a change to one
runs them all; corrected after a review found an integration test target that launches the
package's tool reused while only the tool's own test target, which depends on it, ran. A library
target stays its users'. A file below no target's directory (`Fixtures/`,
`Examples/`, a nested package this one doesn't depend on) was in the rest set already, and stays
there even when a test target reads its package whole for a plugin. A non-Swift file SwiftPM
doesn't compile (`.metal`, a resource) is in the rest set as before; one it does (`.c`, `.m`)
follows the rule. Cost: a package with many targets no test reads reuses less, since each of their
files is in every key; `tests.ignore` is how a person says a tree isn't read by any test.

## Amendment 3: output a program left running prints later is passed on (2026-11-12)

Amendment 1 stopped waiting for held output after 5 seconds and dropped what came after, which
changed what COMMAND printed: without hushrun, `swift test` returns when it exits, and a program a
test left running goes on printing to the terminal; nothing waits and nothing is lost.

The decision: once the real `swift` ended, hushrun's `swift` reads its output without blocking
until it ends, or until it has been quiet for 250 ms (at most 1 s, for output that never goes
quiet). A quarter second only has to tell "still held open" from "closing now": what a program's
last writers flush as they exit comes within milliseconds. Output still open then is handed, with
what the pipe already holds, to a forwarder hushrun starts and doesn't wait for: hushrun again under
a hidden subcommand, with the pipe's read end (or the pseudo-terminal's) as its standard input and
hushrun's standard output as its own, copying to the end. No `fork` in a threaded process. hushrun's
`swift` then exits with the real one's code. The run is still `held`: what it printed can't be
read whole, so no pass of it is recorded, its failures stand, and the facts say why. Standard error
was never copied, and isn't touched. Should the forwarder not start, hushrun's `swift` copies the
rest itself, to its end.

hushrun's target runner for `go test` (ADR 0083) differs on purpose: `go test` itself waits for a
test binary's output to end (up to its `WaitDelay`), so there the runner forwards to the end within
the same process, as the command without hushrun would. hushrun's `ctest` never copies output: the
real `ctest` gets hushrun's standard output itself, so a program a test left running prints there
directly (ADR 0087).

## Amendment 4: a cap on what is kept, and signals after the end (2026-11-12)

A second review left `yes` running from a test: hushrun's `swift` was still alive 10 s after swift
ended, with no forwarder, then spent over 90 s and 401 MB reading what it had kept. The copying
loop went on reading while output kept arriving without asking whether swift had ended, and kept
every byte. Now every pass of the loop asks, the second after swift's end is counted whatever
arrives, and the rest goes to the forwarder; at most 16 MiB is kept to read, what comes after is
still passed on whole, and such a run records no pass, saying why.

A signal sent once swift had ended (while hushrun's `swift` listed or read) was stored and never
acted on: signal-hook can't put a default handler back. Each signal now also gets a default
action that runs once forwarding stops, so it ends hushrun's `swift` as it would any program.

## Amendment 5: a loud suite, and saying why output wasn't read (2026-11-12)

A third review printed more than 16 MiB from a test: Amendment 4's cap kept the first 16 MiB of
bytes, so the `Test Case` lines that came after were never read, nothing of the tests was
recorded, and nothing said so. hushrun now keeps, as output goes by, only the lines it reads
(XCTest's `Test Case '…'`), whole, at most 16 MiB of them; everything is still passed on byte for
byte. A suite that prints much besides is recorded. Past 16 MiB of those lines, or with output a
program swift started still held after swift ended, each test of the run with no failure seen is
`not observed`, with why (`output`), so `hushrun why` says the reason of each test, and the run
says it once, through its facts' `settings.note`, with the size: `hushrun: unit: swift test
printed 17.0 MiB of `Test Case` lines, more than the 16 MiB hushrun reads: none of its tests is
recorded`. A line longer than 64 KiB is kept up to there.

## Amendment 6: what stays unseen (2026-11-12)

Two limits a third review found are kept, and said here rather than fixed:

- **`swift test --skip-build` records the check's pass.** Amendment 1 made such a run not
  observed, so none of its targets' or tests' passes is recorded, but the check's own pass still
  is, on the files as they are, though the products it ran may be older than them. Whether those
  products are up to date can't be told from outside SwiftPM's build; a plain `swift test` builds
  first, and is what a check should run.
- **A file changed with its time put back** (`touch -r`, a tool that restores times, an archive
  extracted with its times) isn't built again by SwiftPM, whose build goes by times, and the run
  tests what was built before. hushrun keys a target by the files' contents, so the next run's key
  is the new one, but nothing tells it the products are old.
