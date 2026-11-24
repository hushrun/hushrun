# 0120. C++ test programs started directly, precompiled headers and C++20 modules

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-106 to C-108, B-241 to B-243; relies on C-30 to C-34, C-39, C-49, B-51, B-55,
  ADR 0080, 0090, 0097

## Context

ADR 0090 reuses C and C++ tests that CTest runs, and ADR 0097 C and C++ objects that CMake's
compiler launcher compiles. An audit on a real project (GoogleTest 1.14, Catch2 3.4, doctest
2.4.11, CMake 3.28, Ninja, GCC 13 and 14, clang 18) found four gaps:

1. **A test program started directly** (`build/tests/t_calc`, the most common way to run one
   program while working on it, and how many CI jobs run GoogleTest with `--gtest_shard_index`)
   reached no adapter: COMMAND starts no `ctest`, so pytest's adapter took it and observed nothing.
2. **C++20 compiles were never stored.** CMake 3.28 and later (policy CMP0155) scan every C++20
   source of a Ninja build for modules and give each compile `@<object>.modmap`, a response file,
   which B-51 passes through: in a C++20 project nothing was reused, modules or not.
3. **Every clang 15 to 18 compile failed through the launcher.** Those clangs know
   `CC_PRINT_HEADERS_FILTERING` but not its value `direct-per-file` (B-55's include graph), and
   end every compile given it with `environment variable CC_PRINT_HEADERS_FILTERING has invalid
   value direct-per-file`. Ubuntu 24.04's clang is 18. ADR 0097 had measured Apple clang 21, which
   takes it, and Debian's clang 14, which ignores the variables.
4. **Precompiled headers** (`target_precompile_headers`) and **modules** passed through, as B-51
   says, but clang's precompiled header, which CMake passes as `-Xclang -include-pch`, was counted
   as `loads-code`.

## Decision

1. **A test program started by its path is a unit** (C-106). COMMAND whose first word, after
   `env` assignments, is a path (it holds a `/`) to a file of a CMake build directory (the nearest
   directory above with a `CMakeCache.txt`), and whose bytes say it is a GoogleTest or a Catch2 3
   program, gets the test-program adapter: the unit is the program, named as ADR 0090 names a
   test's program (`tests/t_calc (Release)`), and a test is one of its tests. What the program read
   is ADR 0090's walk of its build directory, with COMMAND's arguments as a test's command's (a
   program or file they name adds what it is built from or itself). **Sources** are every C and
   C++ file the build directory compiles into any of its programs, so that a change to another
   program's file leaves this one's tests reused: one program alone would otherwise put every
   other program's files in the rest set (ADR 0090 Amendment 4), and any change would run it.
2. **COMMAND byte for byte as written; what to leave out goes in the environment.** Neither
   framework can be reached through `PATH`, since COMMAND is the program. Both read what hushrun
   needs from the environment, which hushrun sets for each start of COMMAND, just before it:
   - **GoogleTest** (C-106): `GTEST_OUTPUT=xml:<file>` in the run's directory, and
     `GTEST_FILTER=-<id>:<id>…` (`-*` for a program left out whole). A test's id is its
     `--gtest_filter`, `Suite.Name`, as for a test `gtest_discover_tests` made (ADR 0090).
   - **Catch2 3.3 and later** (C-107): Bazel's test variables, which it reads when `BAZEL_TEST`
     is set: `XML_OUTPUT_FILE` adds a JUnit reporter beside the one COMMAND chose, and
     `TESTBRIDGE_TEST_ONLY` takes the place of the command line's test spec; with one excluded,
     quoted, escaped name per test left out (`~"adds\, small" ~"multiplies \[x\]"`). It also needs
     `TEST_SHARD_INDEX=0`, `TEST_TOTAL_SHARDS=1` and `TEST_SHARD_STATUS_FILE`, or it prints a
     warning for each missing one: one shard of one is the whole program. A test's id is its name.
     A program left out whole runs whole, since Catch2 fails when it runs no test.
   - What a person sees changes by the framework's own line about a filter (`Note: Google Test
     filter = …`, Catch2's `Filters: …`), as when they filter themselves.
3. **Listed twice, checked against the plan.** Before COMMAND starts, hushrun lists the program's
   tests with COMMAND's own arguments (`--gtest_list_tests`; `--list-tests --reporter xml`
   without the reporters and output files COMMAND names), checks the program as hushrun's `ctest`
   checks one before leaving any of its tests out (built here, up to date, not built after the
   plan, built from exactly the files its key held: ADR 0090 decision 8), and lists again with the
   variable that leaves tests out: unless that listing names exactly the tests expected, nothing
   is left out. COMMAND is the program, and nothing builds between the plan and it.
4. **Outcomes from the framework's XML, never from absence** (C-108). The file must exist, be no
   older than the start by more than two seconds and parse. GoogleTest: a case with a `<failure>`
   or `<error>` failed, `result="skipped"` was skipped (a pass, as elsewhere), `status="notrun"`
   is a disabled test, not one of the program's selected tests. Catch2 writes a case per leaf
   section (`test/section`): each is its test's, by the longest listed name it is or starts
   followed by `/`; a test with a failed section failed. A listed test with no case didn't run (a
   Catch2 test that makes no assertion and prints nothing has none: it never records a pass). A
   failure no listed test explains (an environment's `SetUp`) is an adapter error: nothing of the
   start is recorded. A program older than what it is built from ran old code: nothing is
   recorded (C-32's stale build). When tests were left out and GoogleTest didn't run a kept one,
   or COMMAND failed with no test failing (Catch2 fails when every test it ran was skipped), the
   run starts COMMAND once more with nothing left out and goes by that start (C-32).
5. **Observed only, or not at all.** COMMAND's own `--gtest_filter`, a `GTEST_FILTER` or
   `TESTBRIDGE_TEST_ONLY` already set, GoogleTest's sharding variables, and Catch2's own test spec,
   `--input-file` or `--shard-count`/`--shard-index` keep every test: each is recorded, and the
   plan says why nothing is left out (COMMAND is in every unit's key, C-31, so a filtered or sharded
   run's passes never reuse the whole program's). Not observed, said once: COMMAND's own
   `--gtest_output`, `--gtest_list_tests`, `--gtest_repeat`, `--gtest_fail_fast` (GoogleTest then
   writes the tests after a failure as skipped) and `--gtest_flagfile`, and their variables;
   Catch2's `--section` and its listing options; Bazel's variables already set; **doctest**, which
   takes nothing from the environment; and **Catch2 2** or a Catch2 3 that reads no `BAZEL_TEST`,
   each with the advice to run it through `ctest` with its `*_discover_tests`, which ADR 0090
   observes.
6. **An empty module map is no argument** (B-241). A response file holding nothing but white
   space adds no argument: the compile is the one without it, keyed and stored as any other.
   CMake's map of a source that imports and exports no module is empty with clang; one that names
   a module (`-fmodule-file=`, `-x c++-module`) passes through as `modules`, as does any other
   response file as `response-file`. GCC's scanned compiles keep passing through as `modules`:
   CMake gives every one `-fmodules-ts -fmodule-mapper=<map> -fdeps-format=p1689r5`, and with
   `-fdeps-format` and no `-fdeps-file` GCC 14 writes a second file, `<object stem>.ddi`, beside
   the object (ran), which a stored object can't give back.
7. **Precompiled headers stay passed through, counted as such** (B-242). Keying a precompiled
   header's users by what the header read, and not by its bytes (GCC's `.gch` is a memory image,
   different on every build), is new machinery ADR 0097 left open; nothing here needs it to be
   sound. `-Xclang -include-pch` (and any `-Xclang` before a flag B-51 counts as `pch` or
   `modules`) is counted as `pch`.
8. **The include graph only from a clang that takes it** (B-243). The compiler's identity asks
   once, through `/usr/bin/env` with the graph's three variables, whether the driver preprocesses
   `/dev/null` with them; only one that does gets them. The memo's schema moves to
   `hushrun.cc-compiler-memo/3`, so that every compiler is asked again once.

## Consequences

- `build/tests/t_calc` through `hushrun run` with `tests: {mode: reuse, unit: test}` leaves out
  the tests that passed on the files its program is built from. With `unit: file` the one program
  is audited every run (C-34 audits at least one test file), so `unit: test` is what saves time.
- The first run that names the build's other sources (after the adapter is new to a check) runs
  everything once: `<file> is now a source`.
- Two more listings of the program run before COMMAND (its static initializers run each time), and
  the walk reads every program's records in the build directory; on the audit's project, 0.1 s.
- A C++20 build with clang reuses the objects of sources that import no module; with GCC, none.
- Precompiled headers' users and modules' importers compile every time, as before.
- With clang 15 to 18, compiles keep B-54's fallback for the including folder, as GCC's do.

## Alternatives considered

- **Wrapping COMMAND** (`hushrun` starting the program with `--gtest_filter`): COMMAND would no
  longer be byte for byte as written, which every adapter keeps (ADR 0080).
- **`GTEST_FILTER` inside CTest** (ADR 0090 decision 10, not built): the variable reaches every
  GoogleTest program a run starts. Here COMMAND is one program, and what it starts in turn is its
  own (a death test restarts itself with its own `--gtest_filter`, which wins).
- **Catch2's test spec on its own command line** and **doctest's `--dt-*` options**: they are
  COMMAND's words.
- **Treating a GCC compile whose mapper names no module as a plain one**: the `.ddi` beside the
  object says no, and a stored object would leave it missing.
- **Gating the graph on clang's version**: Apple's clang numbers its versions otherwise, and a
  distribution's clang may carry a patch.
