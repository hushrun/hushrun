# 0087. CTest's test programs, and each CTest test

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** C-49; relies on C-22, C-30, C-31, C-32, C-33, C-34, C-38, C-39, ADR 0068, 0069, 0070,
  0077, RFC 0004

## Context

RFC 0004 asks for C and C++ among the languages a check reuses test by test. CMake projects run
their tests with CTest, which takes nothing from the environment that would let hushrun inside, and
whose tests are programs the build makes: what a test read is what its program is built from, as
for Rust's test binaries and Go's packages (ADR 0069, 0070). ADR 0077 gave adapters a way in through
`PATH`: a program of hushrun's named as the runner. This decision is CTest's adapter, the first to
use it, for both units, as a spike proved it on CTest 3.22.1 and 4.4.5, on macOS and Linux.

## Decision

1. **A unit is a test program, a test is a CTest test** (C-30, C-39, C-49). A test program is the
   program a CTest test starts: its command's first word, real, named by its path from its build
   directory (the nearest directory above it with a `CMakeCache.txt`), without a multi-config
   generator's configuration folder or `.exe`. The configuration it was built in is part of the
   name, `tests/t_add (Release)`, so that a Debug build and a Release build of the same files never
   share a result. In what people read a test program is a test file, as a Go package is.
2. **A test's id is its CTest name**, verbatim (Catch2's and doctest's included), but for a test
   `gtest_discover_tests` made: its CTest name changes with the CMake version and the standard
   library (`Container/0.StartsEmpty` is `Container/std::vector<int,std::__1::allocator<int>>.StartsEmpty`
   on 3.22, `Container.StartsEmpty<std::vector<int,std::__1::allocator<int>>>` on 4.4, and spelt
   otherwise with libstdc++), so its id is its `--gtest_filter`, which none of them changes. Names
   aren't unique: an id met twice in a unit gets `#2`, and a duplicated name is left out only when
   every copy is, since CTest leaves out by name.
3. **Reads are what the build directory says the program is built from.** With Ninja, the build
   graph walked from the program with `ninja -t query` (Ninja 1.10 has no `-t inputs`), and each
   object's sources and headers from `ninja -t deps`. With Makefiles, every
   `CMakeFiles/<target>.dir/build.make`'s rules and each object's compiler depfile, a precompiled
   header's included (GCC records what the header read only there). Both gave the same files in the
   spike, and the tests check both. A file of the build directory no rule makes was written when CMake
   configured: the program reads every file configuring read. Sources fetched into the build
   directory (`FetchContent`, `_deps/`) aren't the repository's: their identity is their
   declaration, in a CMake file, in the rest set. With neither readable (another generator, a
   program the graph doesn't name, an object with no record of its headers), the program reads every
   C and C++ file of the scope. **Sources** are the files of a C, C++, Objective-C or CUDA kind a
   program read; CMake files, presets, toolchain files and data are in the rest set, so a change to
   one runs everything.
4. **A stale program ran old code.** A program whose build has work to do (`ninja -n`), or that is
   older than a file it read, is `not observed`: it records nothing, and none of its tests is left
   out.
5. **Loading, with COMMAND byte for byte as written.** hushrun's `ctest`, first on COMMAND's `PATH`,
   reaches every `ctest` started by name: typed as COMMAND, or started by a shell line, a script or a
   Makefile. It finds the real one on `PATH` as COMMAND had it, starts it with that `PATH`, passes
   stdio, the exit status and the signals that stop a process through, and ends by the signal that
   ended it (ran: Ctrl-C to the group, the real `ctest` dies of SIGINT and so does hushrun's; SIGTERM
   to hushrun's alone is passed on). A `ctest` a test starts gets the real one. With no plan, what a
   person sees and the exit code are the real `ctest`'s, byte for byte but for timings (ran, 3.22 and
   4.4). It changes nothing for what isn't a test run (`-S`, `--build-and-test`, `-N`,
   `--show-only`, `--print-labels`, help, `--version`, `-T` other than `Test`).
6. **Selection, checked twice.** The tests left out are those `hushrun.json` lists. In the spike's
   order: `--exclude-from-file` on 3.29 and later (exact names); else one `-E`, COMMAND's own (or its
   test preset's, which a command line's would replace) joined to each name anchored and escaped as
   CMake reads it, with no group (3.22 takes nine); else `-R` with the names kept; else nothing. A
   regular expression CMake can't compile (past 65,535 bytes, or a tenth group on 3.22) is dropped
   silently and every test runs; a filter can match a test nobody meant to leave out. So hushrun's
   `ctest` lists again with the final arguments, and requires exactly the tests expected, by name
   and command; otherwise it tries the next way, and with none runs COMMAND's `ctest` unchanged. A
   fixture's setup or cleanup a kept test needs is never left out (ran: `-E` removes a setup for
   real, and its tests run without it). `--rerun-failed` and `-I` pick tests by their position
   after filtering: those runs are only observed. Everything left out adds `--no-tests=ignore`, so
   that a pass stays a pass.
7. **Outcomes from CTest's own JUnit file** (`--output-junit`, CMake 3.21 and later; before it,
   nothing is observed and the run says so once): COMMAND's or its preset's when it names one, else
   a fresh one of hushrun's. Nothing ever reads as a pass from absence: the file must exist, have
   changed in this run, parse and hold as many test cases as it says; a test absent from it didn't
   run; a duplicated name whose counts differ didn't finish, and failed if one of its cases did;
   `ctest` ended by a signal records nothing. A skip (`SKIP_RETURN_CODE`,
   `SKIP_REGULAR_EXPRESSION`) passed, as a skipped test does elsewhere; another `notrun` and
   `disabled` didn't. A pass under `--repeat until-pass` or `after-timeout` may follow a failure,
   which the file doesn't keep: that test is `not observed`. A `DISABLED` test isn't one of its
   unit's selected tests, so it doesn't stop its program from passing whole.
8. **No binding to bytes; a check where `ctest` runs instead.** Cargo's plan builds first, so it
   can bind a reused binary to its bytes (ADR 0069). CTest's plan can't: COMMAND may be a shell line
   that builds and then tests, and the bytes don't exist yet. What stands in: the plan writes the
   lists each unit's key was made from to a file of the adapter's own in the run's directory, and
   before it leaves out any test of a unit, hushrun's `ctest` checks again, after any build COMMAND
   made, that each of the unit's programs is built here, up to date, not modified after the plan,
   and built from exactly the C and C++ files of the scope its key was made from. So a key that
   passed held every file the code came from, whatever the build directory's configuration did in
   between. For `ctest …` itself the plan lists and walks before COMMAND, as Go's asks `go list`, and
   reuses with no map at all.
9. **Which COMMANDs.** `ctest …` by name; a wrapper (a shell's `-c` line, a `#!` script, a
   non-CMake Makefile) whose own text starts `ctest` by name and names no other runner: no pytest
   word, and no `swift test` (C-32's one rule, shared with the `swift test` adapter). pytest keeps
   any other wrapper, as before: CTest projects whose tests are pytest are rare, and losing
   pytest's reuse would cost those that aren't. Refused, said once: `ctest` by its path, which `PATH` can't stand in for; and
   `cmake --build … --target test`, `make test` and `ninja test` in a CMake build directory, which
   start CTest by its full path; `ctest --test-dir build` can be observed. A multi-config build
   lists its tests only with `-C`: without it nothing is listed, and the run says so.
10. **GoogleTest inside one CTest test isn't selectable**, and stays one test. GoogleTest reads
    `GTEST_FILTER` from the environment, so the adapter could leave out cases that way and read
    `GTEST_OUTPUT`'s XML; but the variables reach every GoogleTest program the run starts,
    discovered ones too, where the command line's own filter wins; programs with one name overwrite
    each other's XML; a filter that matches nothing passes with no test, so a whole program left out
    looks like a pass to CTest; the filter shares the environment's space; and the one CTest status
    would mix skipped and run cases. Not built.
11. **Tests one by one** (C-39): with `unit: test`, the adapter reports each test and leaves out the
    tests `reuseTests` lists; a program whose key didn't pass has its tests that passed reused one by
    one, and the one that failed runs alone. A test can't be watched (`reads: test` runs as
    `reads: file`): each test read what its program read (RFC 0004 §6).

## Consequences

- C and C++ repositories built with CMake reuse test programs, and CTest tests one by one, on the
  same rules as Rust's test binaries and Go's packages.
- A change to any CMake file, preset or data file runs everything: they are in the rest set.
- The compiler, its flags and options set in the build directory's cache aren't in a key, as
  `RUSTFLAGS` aren't in Cargo's (C-9 is the check's own key's limit). The configuration's name is,
  in the unit's name; a reconfigured build that changes which files a program is built from is
  caught by the check in decision 8, and one that rebuilds during COMMAND by its "not modified after
  the plan".
- Two build directories of one configuration that one COMMAND tests share their units' names.
- Files a test opens at run time aren't followed, as Rust's aren't; those in the repository are in
  the rest set.

## Alternatives considered

- **One `-E` for everything:** it doesn't scale (about 1,400 forty-character names), and a regular
  expression that doesn't compile is dropped silently.
- **CMake's File API for what a program read:** it names targets and sources, not headers, nor
  header-only libraries (INTERFACE targets were absent from 3.22's reply), and has a reply only when
  a query was there before configuring.
- **`-T Test` and `Test.xml` before 3.21:** it changes what a person sees and writes `Testing/<tag>/`.
- **Binding a reused program to its bytes in the plan:** the plan can't know them before COMMAND
  builds; decision 8's check, made when `ctest` runs, holds to what was built.
- **A unit per build directory:** RFC 0004 names a test program by its program; the configuration
  in the name separates the builds that matter.

## Amendment 1: what the walk follows (2026-11-12)

Decision 4 read a test program from its build directory's records: each object's headers, and a
header a build rule writes followed one step. Reviewers showed three ways a program read more than
its key held, each a test left out while it failed on the code as it was:

- **A generated header's generator.** A project builds `gen`, runs it to write `gen.h`, and a test
  program includes `gen.h`. On Ninja the walk asked `ninja -t query` for the rule's inputs and
  never read the records of `gen`'s own objects; on Makefiles it took only the repository files the
  rule named. A header `gen.c` includes was missing, so a change to it reused the program although
  the build wrote another `gen.h`.
- **A program a test names.** `add_test(NAME x COMMAND driver $<TARGET_FILE:server>)`: the unit is
  `driver`, and `server`, which it starts, was never walked.
- **An in-source build.** With `cmake -B .` every file the build named was taken for a file of the
  build: each program read nothing, nothing was reused, and nothing said why.

The decision:

- A file a build rule writes is followed to the end on both generators: the rule's inputs, the
  program it runs, and the headers of every object reached that way, from `ninja -t deps` or the
  object's depfile. The staleness check on Makefiles compares those files too; Ninja's `-n` already
  knew them.
- Each argument of a test's command (and its value after `=`), and each value of its `ENVIRONMENT`
  and `ENVIRONMENT_MODIFICATION`, that is an absolute path that exists adds to what its program
  read: a program a build rule makes adds what it is built from, and must not have been built after
  the plan either; a file of the repository adds itself; a directory of the repository adds every
  source; a directory of a build adds every source and makes the program not up to date, since
  which of its programs runs, and whether it was built again, can't be told. A file of a build
  directory no rule makes is data the test reads or writes, and adds nothing. A program a test
  starts by any other way (a path it builds itself, a file it reads) isn't seen, as files a test
  opens aren't (Consequences).
- An in-source build (a build directory that holds the repository) isn't read: each of its
  programs is not followed as `in-source build`, with a line that says a build directory of its own
  can be read.

## Amendment 2: one start per `ctest`, and presets (2026-11-12)

Decision 7 counted every `ctest` of a COMMAND as one start, and ADR 0077 Amendment 1 showed two
starts adding up to a program that ran whole. Decision 3 read only the command line for what a
test preset can say too: a preset's `execution.repeat` of `until-pass` recorded a pass that came
after a failure, its `filter.include.index` picks tests by their position, and its
`execution.showOnly` runs no test. And hushrun's own listings wrote the user's files: with
`--preset`, CTest writes the preset's `outputLogFile`, and on 4.4 its `outputJUnitFile`, even for
`--show-only` (ran).

The decision:

- Each hushrun `ctest` names a start of its own in its facts file.
- A preset's `until-pass` or `after-timeout` leaves each pass not followed, as `--repeat` does (a
  command line's `--repeat` replaces it); its `filter.include.index` makes hushrun observe and leave
  nothing out, as `-I` does; its `showOnly` makes the run not observed. Every other field that
  chooses tests is honoured by the listings, which take `--preset` as the run does, and the second
  listing checks what a mechanism keeps.
- A listing gets a JUnit file of its own in a temporary directory, which replaces the preset's. A
  command line's `-O` doesn't stop CTest writing the preset's log (ran: 4.4 writes both; 3.22
  writes the preset's only when `-O` is given), so the preset's log is kept before each listing
  and put back after it, bytes and time.

## Amendment 3: skips, each test's program, and COMMAND's assignments (2026-11-12)

- CTest's skips (`SKIP_RETURN_CODE`, `SKIP_REGULAR_EXPRESSION`) and disabled tests are told as
  skipped (ADR 0077 Amendment 2), and counted for a program told without its tests: in a run that
  failed they record no pass. They used to read as passed.
- `not built here` is each test's own: a unit whose name two programs share (one built here, one
  found on `PATH`, when the build names no configuration) used to miss it when the built one came
  first.
- The plan's listing and walk start `ctest` and `ninja` with COMMAND's own `env NAME=value`
  assignments (ADR 0077 Amendment 7), so that `env CTEST_CONFIGURATION_TYPE=Release ctest` lists
  what its `ctest` will.
- The real `ctest` is the one a shell would start: an empty `PATH` entry is the current directory
  and a relative one is from it, and a link into a directory of hushrun's programs is passed over.

## Amendment 4: a source no program is built from is in the rest set (2026-11-12)

Decision 5 made a file of a source kind a source when some program read it; with a program whose
build couldn't be read, "every source" counted too, so one such program (a test that names the
repository's directory, another generator) made every file of a source kind a source. A test that
reads such a file at run time (a compiler's test suite feeding `cases/*.c` to its program, a header
a script copies) then didn't run when it changed: the file was on the "every source" program's list
only.

The decision, the rule: **a file of a source kind that a build's graph gives to no program of the
check is in the rest set, so a change to it runs everything; a file the graph gives to a program is
on that program's list only.** Sources are what the walks found; a program that reads every source
still reads them all, but makes none of them a source. The plan's file lists, for its check before
leaving out, the C and C++ files of the sources and units sets only. Cost: a repository with many
sources no program is built from reuses less, since each of them is in every key; `tests.ignore`
is how a person says a tree isn't read by any test.

## Amendment 5: output after `ctest` returns (2026-11-12)

hushrun's `ctest` starts the real one with its own standard output and error, and copies neither:
it returns when `ctest` does, with its code, and what a program a test left running prints later
reaches the same output directly, as without hushrun. Nothing is held, unlike hushrun's `swift`,
which copies standard output to read it (ADR 0086 Amendment 3).

## Amendment 6: what a second review found the walk missed (2026-11-12)

A second review ran these, each a test left out while real `ctest` failed after a change:

- **A custom command's `DEPFILE`.** A generator that reads a header only its depfile names. On
  Ninja, an edge whose `build.ninja` stanza sets a `depfile` is asked for Ninja's record of its
  output (`deps = gcc`), as an object is; on Makefiles, the depfile `DependInfo.cmake` names for a
  custom output is read. No record or no depfile: the program reads every source.
- **Fixtures and `DEPENDS`.** A test's program reads the walks of the programs of every fixture
  setup and cleanup it requires (through the fixtures those require) and of each test it
  `DEPENDS` on, with what they name; a setup not built here makes it read every source.
- **A program started by a path compiled in, or waited for.** On Ninja the walk follows a program's
  order-only inputs through phony edges to the executables, modules and shared libraries it waits
  for (`add_dependencies`), and reads the absolute paths in its objects' `DEFINES` and `FLAGS`; on
  Makefiles, the targets its own waits for in `CMakeFiles/Makefile2` and the paths in its
  `flags.make`. Each program found is walked.
- **A relative path in a test's command** is taken from its `WORKING_DIRECTORY`, or, since the
  listing doesn't give a test's own directory, from its program's directory and its build
  directory.
- **Staleness on Makefiles** compares each object (and a custom command's output) with the files
  its depfile names, as make does, and the program with its objects.
- **An in-source build** says so once in the run, as Amendment 1 promised: when every program of
  the run is of one, its test files weren't observed, with why.
- **Listings.** For `ctest …` as COMMAND the plan keeps its listing in the run's directory, and
  hushrun's `ctest` takes it when its `ctest`, words and directory are the same. A listing runs the
  project's discovery scripts (`TEST_INCLUDE_FILES`, how `gtest_discover_tests`' `PRE_TEST` mode
  runs the user's program): they now run once more than without hushrun, to know which program
  each test runs, and once more again when tests are left out, for the listing that checks what
  is left out (and once more for each way of leaving out that fails that check).

hushrun's `ctest` also ends on a signal sent after the real `ctest` ended, as any program does: the
forwarding of signals no longer keeps them once the real one is gone.

## Amendment 7: a run on a stale program records no pass (2026-11-12)

Item 4 kept a stale program's own unit from a pass, but the check's pass was still recorded, so
the next run on the same code skipped whole and never ran the program as built. A program known to
be older than what it is built from (`ninja -n` has work, or on Makefiles a file it read is newer)
whose test ran in a start of `ctest` now makes that start's facts say `staleBuild` (C-32):
`<program> is older than what it is built from (<what changed>): build before ctest`. A pass of
the run then records nothing, the check's included, and says so once; a failure records as usual.
A program whose being up to date can't be told (a header record that isn't `VALID` while `ninja -n`
has no work, `ninja` not answering, a directory of a build a test names) stays `not observed` only:
it proves no old code ran, and treating it as stale would keep such a check from ever recording.
Ninja's `-n` is now asked even when a record wasn't `VALID`, so its answer decides. A stale
program none of whose tests ran in the start says nothing.

## Amendment 8: one rule for a stale program, by the files' own times (2026-11-12)

A third review found the two ways of deciding Amendment 7 used disagree with the build, both
ways. `ninja -n` has work whenever a target runs on every build (`add_custom_target(stamp ALL …)`
writing a header a test includes), so every `cmake --build && ctest` said "build before ctest" and
recorded nothing; a header such a target wrote in the same second as the object made make 3.81's
comparison call it old too; and a directory of a build a test names (`t_run ${CMAKE_BINARY_DIR}/bin`,
starting `bin/helper`) was only "can't be told", so a pass on an old helper was recorded.

One rule now decides for both generators: **a program is old when a file of the repository one of
its objects was built from is newer than that object, or one of its objects (or a library it
links) is newer than it.** A file a rule writes is never the newer side itself: it stands for the
repository's files its rule reads, to the end (a generated header for its script and template, a
generator's output for the generator's sources). Times compare strictly, at the coarser of the
two times' own resolutions, read from the times (no fraction and an even second is FAT's two
seconds, no fraction one second, whole hundredths exFAT's), so a file system that rounds a later
write down never makes a change. A program reading a file CMake wrote when it configured is also
old when a file configuring read is newer than `CMakeFiles/cmake.check_cache`, which every
configure writes again. A program a test program waits for or names is judged by the same rule on
its own, not by its time against the test program's (it is rebuilt alone when only its sources
change). For a directory of a build a test names, every program the build makes there is judged.
`ninja -n` is no longer asked.

The person's line names the file and the object or program: `t_util ran old code: src/util.h
changed after CMakeFiles/t_util.dir/tests/t_util.c.o was made; build before ctest`. When the
generator's make compares whole seconds (GNU Make before 4) and the file changed in the second
its object was made, a build won't help, and the line says so: `…, in the same second: make
compares whole seconds, and won't make it again; touch tests/t_misc.c, then build before ctest`.
A program whose build can't be read is old when any source is newer than it, and the line says
that whether it is built from that source can't be told.

What the rule can't see stays unseen, and the program counts as up to date: a compile command
changed without a rebuild (a flag in `CMakeLists.txt` the build hasn't configured yet, unless the
program reads a file configuring wrote); a file changed with its time put back (`touch -r`, a
checkout tool that keeps times, an archive extracted with its times), which ninja and make miss
too; and a change in the same tick of a coarse file system's clock. One case it gets wrong the
other way: a rule that leaves its output untouched when it would write the same bytes (`copy_if_different`,
Ninja's `restat`) leaves its inputs newer than the object, which the rule takes for old until the
object is touched. A record that isn't `VALID` still makes the program one whose being up to date
can't be told (`not observed`), which doesn't keep the run's pass.

## Amendment 9: a fixture not built here adds what its command names (2026-11-12)

Amendment 6 made a test read what its fixtures' and dependencies' programs are built from, and a
helper whose program isn't built here (`add_test(NAME tmp_setup COMMAND ${CMAKE_COMMAND} -E
make_directory …)`) read every source, so every test that required `tmp` ran on any change. Such a
helper now adds only what its command names: its program when that is a file of the repository
(a script it runs), and what its arguments and environment name, as a test's own command does;
`cmake -E`, `sh -c` or a tool of the system adds nothing. A script that starts a built program
without naming it is missed, as a test program's own start of one is.

The same review's project showed a second loss: CTest's listing puts in a cleanup's `DEPENDS`
every test that requires its fixture (to run the cleanup last), so following the cleanup's
`DEPENDS` made each such test read the others' programs. Those aren't followed: they run beside
the test, not for it. In the reviewer's project (21 programs, 5 requiring a `cmake -E` fixture),
a change to a leaf library ran 6 programs again rather than 10, and a header two include 4 rather
than 9, as before Amendment 6.

## Amendment 10: the kept listing is taken once, and an unlisted failure (2026-11-12)

Amendment 6 kept the plan's listing for hushrun's `ctest`, but hushrun's `ctest` wrote one too, for
every start, so a later identical start took the first one's: in `ctest --test-dir build;
<discovery adds case_bad>; ctest --test-dir build` the second start's listing lacked `case_bad`,
and the program that failed it kept its pass of the first. Only the plan writes the listing, only
for COMMAND `ctest …` itself, and hushrun's `ctest` reads it once and removes it: a later start,
and every start under a wrapper, lists for itself.

A failed case in CTest's JUnit file that names no test of the listing (a discovery script added it
after the listing ran) is a failure no listed test explains (C-39), of a program that can't be
told: no unit of that start keeps a pass, said as the adapter's error.
