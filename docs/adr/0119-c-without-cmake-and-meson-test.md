# 0119. C and C++ without CMake: Make, autotools, Meson and Ninja reach the launcher; `meson test` as units

- **Status:** proposed; built
- **Date:** 2026-11-24
- **Rules:** B-230 to B-238 (draft), C-102 to C-105; B-50 to B-62 and C-49 read for them

## Context

[ADR 0097](0097-c-and-c++-build-blocks.md) reuses C and C++ objects through CMake's compiler
launcher, and [ADR 0090](0090-ctest.md) reuses CTest's test programs. Both stop at CMake. An
audit on Linux (GCC 13.3, GNU Make 4.3, autoconf 2.71 with automake 1.16, Meson 1.3.2, Ninja
1.11.1, CMake 3.28.3), on small projects of each kind in two worktrees, found:

| build | objects reused in a second worktree, before | after |
|---|---|---|
| CMake + Ninja | all | all (unchanged) |
| plain Makefile (`$(CC)` from make's default) | none: no launcher | all 3 |
| autotools (`./configure && make`) | none | all 3; configure's checks stored none |
| Meson (`meson setup && ninja`) | none | all 2 |
| handwritten `build.ninja` | none | all 3 |
| CMake configured from a Makefile | all, CMake's own way | all; its cache names `/usr/bin/cc` |

and that `meson test` was only ever a whole check: `hushrun init` proposed it, nothing reused
its tests. ADR 0097 refused `CC`/`CXX` and `PATH` shims for COMMAND as a whole, for reasons that
still hold: CMake and Meson write down the absolute path of the compiler they find, the Rust `cc`
crate reruns every build script when `CC` changes, and a cached path into a run's session fails
once the run is gone. It left Meson and Make to a later phase (its owner decisions 6 and 7).

What each build system takes from the environment decides the way in:

- **GNU Make** reads `MAKEFILES`, makefiles read before any other, in every make, recursive ones
  included, and runs its commands with the `PATH` a makefile exports (measured on 4.3, both for
  commands it runs through the shell and those it starts directly). Make compares times, never
  commands.
- **autoconf** writes the compiler it found by name (`CC = gcc`) into the Makefile, and a clone's
  fresh dates make `make` run automake, autoconf and `configure` again before building, so their
  checks (`conftest.c`) go through whatever `PATH` make's commands have.
- **Meson** reads `CC` and `CXX` once, when it sets a build folder up, splits them as a shell
  does, and keeps them for the folder's life (`--reconfigure` too). Set to
  `/bin/sh -c '<ADR 0097's script>' /usr/bin/cc`, Meson identifies GCC through it, writes the
  same indirection CMake caches into `build.ninja` (its `$` escaped), and `ninja -n` has no work
  after toggling `HUSHRUN_CC` (measured). Without `CC`, Meson writes down `cc` by name.
- **Ninja** reads nothing, and keys each output by its command's text.

## Decision

1. **Compiler programs on `PATH` for make's commands alone** (B-230 to B-232). The run writes, in
   its session, `cc`, `c++`, `gcc`, `g++`, `clang`, `clang++`: POSIX `sh` scripts that find the next
   compiler of their name on `PATH` past every folder of such scripts and start the launcher with
   that compiler's path, or the compiler itself when the run's launcher is gone. `MAKEFILES` names
   a makefile that exports the folder first on `PATH`, once, and not in a CMake build folder.
   COMMAND's own `PATH` doesn't change: the Rust linker, CMake run from a script, Bazel and every
   other tool COMMAND starts see the compilers the person has. Refused: `CC`/`CXX` for COMMAND (ADR
   0097's reasons); `make CC=…` through `MAKEFLAGS`, which overrides the Makefile's own `CC` and
   loses which compiler it named; a makefile that sets `CC` itself, which an autotools Makefile's
   `CC = gcc`, read after it, replaces.
2. **What writes down a compiler's path never sees the scripts** (B-233). The folder also holds
   `cmake` and `meson`, which start the real ones with `PATH` without it, so a build folder
   configured from make's commands names `/usr/bin/cc` (tested). The rustc wrapper's linker lookup
   (B-18) passes the folder over. A tool started by its path, or one that runs `which gcc` into a
   file, is a hole this leaves, documented.
3. **A handwritten `build.ninja`** (B-235): a COMMAND that is `ninja …` by name, on a build file in
   a folder neither CMake nor Meson set up and that doesn't say it is theirs, gets the folder first
   on its `PATH`. Nothing goes into `build.ninja`. A top-level `ninja` program was refused: CMake
   would find it on `PATH` at its first configure and cache it as `CMAKE_MAKE_PROGRAM`.
4. **Meson through `CC` and `CXX` at `meson setup` alone** (B-234, B-236, B-237). In a repository
   whose checkout has a `meson.build`, hushrun's own `meson` (ADR 0080's program on `PATH`) is first
   on COMMAND's `PATH`, and gives `meson setup` the indirection in front of Meson's own first
   default, or of the one-word compiler the person set. Only `meson setup` gets the variables,
   which answers ADR 0097's owner decision 6 without its objection. A build folder set up before is
   named after the run, as CMake's are. Meson's `compile_commands.json` names `/bin/sh` first
   (CMake leaves its launcher out): a cost a person sees in clangd, written in the guide.
5. **Configure checks are not stored** (B-238): autoconf's `conftest` and Meson's compiles under
   `meson-private` pass through as `configure-probe`, as CMake's do.
6. **`meson test` as units** (C-102 to C-105). The same shape as CTest's adapter, sharing its
   build-folder walk: a unit is a test program (`tests/t_add (debug)`), a test a Meson test by its
   listed name; hushrun's `meson` lists with COMMAND's words, leaves tests out by naming the tests
   kept (with none kept, `--no-suite` for each suite, which runs none and passes), lists again to
   check, and reads outcomes from Meson's own `testlog.json`. Unlike CTest, `meson test` builds
   before it runs: the walk that decides what may be left out is made before the real `meson`
   starts (a program its build would make again is stale, and runs), and the one that is recorded
   after it ends, so a changed program's pass is recorded in the run that built it.

## Consequences

- Make, autotools, Meson and handwritten Ninja builds reuse objects between worktrees and, through
  the relay, between machines, with no setting but build reuse being on; every rule of B-51 to
  B-62 holds for their compiles, since the launcher gets the compiler by its path as from CMake.
- A run's make commands see one more folder on `PATH`, and each compile one more `sh` (too little
  to measure on Debian's dash). A `ccache` named as `CC` in a Makefile finds the script as its
  compiler, whose date is new each run: ccache misses, and the launcher behind it serves.
- A Meson build folder set up inside a run keeps the indirection for good, as a CMake cache does.
- Meson test programs reuse as CTest's do; a change to `meson.build` runs every test (rest set).
- Not verified here: macOS (GNU Make 3.81, Apple clang, Homebrew's Meson), BSD make (which reads no
  `MAKEFILES`: no reuse, no harm), Meson's subprojects' `<project>:<name>` listing, and the walk on
  Meson's unity and precompiled-header builds. CI runs the tests on macOS.

## Alternatives

- **`CC`/`CXX` for COMMAND**: refused in ADR 0097, refused again.
- **`PATH` shims for COMMAND as a whole**: CMake and Meson would cache them; so would a CMake
  configure finding a top-level `ninja`.
- **A Meson native file** (`--native-file`) naming the indirection: Meson keeps the file's path and
  reads it again at `--reconfigure`; a file of the session's would be gone.
- **Rewriting `build.ninja` or Makefiles**: Ninja would rebuild everything on every toggle.
- **`meson test` keyed by the test, not its program**: a Meson test is a program and its arguments,
  and what it read is its program's; as for CTest, the program is the unit and the tests are told
  one by one (`unit: test`).
