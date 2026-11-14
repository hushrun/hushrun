# 0091. Linked outputs between machines

- **Status:** accepted
- **Date:** 2026-11-14
- **Rules:** B-6, B-9, B-10, B-16, B-18, B-24, B-29, B-30, B-34, B-37, B-38

## Context

[ADR 0078](0078-sharing-build-blocks-through-a-backend.md) and
[ADR 0079](0079-sharing-build-blocks-in-ci-and-sandboxes.md) share libraries. A library is
written by rustc alone, and its key holds everything rustc reads. A program, a proc macro and a
build-script binary are written by a linker that rustc runs, from files rustc never names: the
C driver, the `ld` it finds, the start files, the C library, the SDK. Those blocks stayed on the
machine that built them (the reason `linked`), and so did every block that holds one of the
machine's folders. On a cold machine that left every proc macro and every build script to
compile before the first downloaded library could be of use.

Experiments before any code said what holds. Two machines whose link environment is the same
by content link the same bytes, whatever their users, homes, checkouts and toolchain paths: two
containers from one image did; Debian 12 against 13 did not. On macOS the same holds only after
the link stops recording where the toolchain is installed. And the rules as they stood gave one
key to different bytes on a single machine: another `ld` earlier on `PATH`, a system library
replaced, a variable Apple's linker reads by itself.

## Decision

**What is shared**

- **A linked output whose link environment is known**, on the same terms as a library. The
  environment, E, is the SHA-256 of an ordered list of what decides a link's bytes outside
  rustc's command line: the C driver's dry run of the link, word for word; each program that
  runs and each library it loads; the start files; the C runtime as `ld` finds it; the
  toolchain's own linker tools; the SDK. It takes the place of the driver's and the linker's
  size and date in the key (B-18, B-37).
- **Nothing about E is remembered but each file's digest.** The dry run and every search run
  again at each link and each lookup. A remembered answer is right until a file appears earlier
  in a search, and then every remembered stat still matches while another file answers.
- **One rule says which `ld` runs** (B-18), and E takes its answer; it searches for none
  itself. Two searches drifted once already: gcc 15's driver names an `ld` its `collect2` never
  runs. Where that `ld` is rustc's own `gcc-ld` program, the `rust-lld` it runs is in E by
  content.
- **A library a crate asks for with `-l` is keyed by content wherever it is found, and stays in
  the name a dependent uses for a proc macro.** Only E is left out of that name, so libraries
  keep sharing between machines whose linkers differ.
- **On macOS a link no longer records where the toolchain is** (B-9): in a run that shares,
  proc macros and build scripts are linked without a debug map; a program sees the toolchain
  through a link beside its outputs, one per toolchain. A panic inside a build script or a
  proc macro keeps its message, its location and its frames' names, and loses each frame's
  file and line. A run that shares nothing keeps the map, as before: nothing is bought by
  dropping it there. The two are keyed apart.
- **A block bound to folders, where those folders are the same** (B-24): a block that holds a
  folder of its checkout or its machine, or reads a library from a system folder, has that
  folder in its keys, so another machine computes them only where the folder is the same text. A CI job offers such a block for
  upload, and only a CI job, known by its credential (the job's own ID token) and never by the
  environment: every job of a repository has the same paths, and they name
  nobody. This is what shares CI's programs, test binaries and libraries that bundle C code.

**What stays on the machine**

- A link whose environment can't be told with certainty: a driver that is neither gcc nor clang
  by its own dry run; a `-fuse-ld` other than `bfd`, `gold` and `lld` (so `mold`); an `ld`
  sysroot other than `/`; a `-specs` or `--config` file; a linker script that does more than
  name files; a loader variable set for the link; a link that does link-time optimization;
  anything on Windows. Each is stored and reused where it was built, as before.
- A block whose outputs can't be searched (`unsearchable`): it may hold a folder nobody saw.
- A program on macOS whose target folder lies outside its workspace is tied to that folder: its
  UUID is a hash over names that hold the folder's path.
- A linked output compiled with rustc's own run paths (`-C rpath`) is tied to its output
  folder, its dependency folders and the toolchain's folder: rustc writes the way between them
  into it, relative to itself, and that differs with a checkout's depth while no folder's name
  is in the bytes. Its first key holds those folders (B-9), so it is reused where they are the
  same and travels only as a bound block.
- A person's blocks that hold a folder: the folder's text holds their user name.
- Everything B-24 kept before: a read outside the roots, a value no key holds, this machine's
  CPU, an extern only this machine names.

**When two machines disagree** (B-34). A library that differs under one key means one build had
other inputs than the key names: the key is suspended and the trusted copy served. A linked
output that differs means E missed something on one of the two machines, and what this
machine's linker makes is what it would make every time. The machine keeps its own block, in
the downloaded one's place, and keeps that first key to itself for `keep_days`: nothing of it
is asked of the shared store or sent to it. The report says the link differs, in a word of its
own, and no suspension is made here.

**Before a downloaded program runs** (B-38). Cargo runs a build script itself, and one that
can't start fails the build, and every build after it, with no error rustc prints. A downloaded
build script or program is checked first by reading its headers, with nothing run: its CPU,
that the file is whole, that its loader is one of the system's own files, that each library
it needs is on this machine. The bytes came from another machine, so the loader they name is
never run to ask. One that can't start is a miss and is compiled here.

**What the relay fetches unasked** (B-30): proc macros and build scripts first, then libraries;
programs only when a compile asks.

**Every class is audited byte for byte** (B-16), a program's debug bundle file by file: a link
is as deterministic as rustc, so a difference means a key that names less than what was read.

## Alternatives considered

- **A version text for the linker** (`ld -v`, the SDK's version). It misses what the linker
  loads and which `ld` the driver picks, and an image whose `ld` was rebuilt keeps its text.
- **Remembering E by the stats of the files it found.** Cheaper by a dry run per link; wrong as
  soon as another file answers a search (above).
- **Leaving all of a link's inputs out of a dependent's name for a proc macro.** A library that
  expands a macro linked to a system library then kept its key while its bytes changed.
- **Path-mapping flags for C code**, to make the libraries that bundle C code the same on two
  machines. It changes people's C flags, makes `cargo` and `hushrun run cargo` rebuild each
  other's C code, and two gcc versions differ anyway. Sharing blocks bound to equal folders
  recovers them in CI, where it matters.
- **Suspending a linked output that differs, as a library is.** It made the machine compile
  that crate on every build for the kept days, and threw away a block its own linker makes for
  bytes it never would.
- **Asking the program's own loader to list what it needs** (`ld.so --list`, as `ldd` does).
  The loader's path is read from the downloaded file: a crafted file names any program, and the
  check ran it. Reading the headers finds the same missing library and missing symbol version
  and runs nothing.
- **No check before a downloaded build script runs**, since E is meant to make a failure
  impossible. The failure, if E is ever incomplete, needs `cargo clean`: not a state a build
  may be left in.

## Consequences

- Measured on this repository's own build (368 stored blocks on Linux arm64): two machines of
  one image with other users, homes, cargo homes, checkouts and toolchain places shared 306
  blocks before, all of them libraries, and share 361 now: every proc macro and 35 of 36
  build scripts as well. Two jobs with the same paths share all 368, the seven bound ones
  among them (the libraries that bundle C code, the programs). Every block two machines keyed
  alike had the same bytes.
- Every linked output's key changes once, and on macOS every proc macro, build script and
  program is linked differently than before: one cold build.
- E costs a dry run and a few questions at each link and each lookup (a few milliseconds with
  gcc, 12 to 19 ms through macOS's `cc` shim), and the first link of a run hashes the linker
  programs once.
- E changes when a linker's own library is updated, with the linked bytes unchanged: a runner
  image update costs a rebuild of linked outputs. Lost reuse, never wrong bytes.
- Not covered, and so not shared: `mold` and other linkers, release programs built with
  `-C lto`, split debug files on Linux, Windows.
- The audit is the check on E's completeness. A linked output that differs between two
  machines is kept by each and reported as a link that differs; a pattern of those names a
  hole in E.
