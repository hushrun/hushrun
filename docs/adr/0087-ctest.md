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
9. **Which COMMANDs.** `ctest …` by name; a shell's `-c` line, a script or a non-CMake Makefile
   COMMAND names whose text starts `ctest` by name and no pytest (pytest keeps those, as today:
   CTest projects whose tests are pytest are rare, and losing pytest's reuse would cost those that
   aren't). Refused, said once: `ctest` by its path, which `PATH` can't stand in for; and
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
