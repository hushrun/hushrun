# 0097. C and C++ build blocks through CMake's compiler launcher

- **Status:** proposed, a design and a spike; nothing is built
- **Date:** 2026-11-18
- **Rules:** B-50 to B-62 (draft); B-13, B-14, B-15, B-22, B-25 to B-34 read for C and C++

## Context

Build blocks reuse compiled Rust ([ADR 0076](0076-compiled-crates-are-reused-between-worktrees.md) and after) and pure-Go package
archives ([ADR 0093](0093-go-build-blocks.md)) between checkouts and machines. CTest already
reuses C and C++ *tests* ([ADR 0090](0090-ctest.md)), so a C or C++ check whose code changed still
compiles every translation unit again before CTest can leave anything out. A C or C++ build runs
one compiler process per translation unit, each reading hundreds of headers, and CMake, the build
system most C and C++ repositories that hushrun meets use, has a standard hook for starting the
compiler through another program (`CMAKE_<LANG>_COMPILER_LAUNCHER`), which ccache and sccache use.
Those two make the hook proven and the territory known; they also show the traps: ccache's fast
mode trusts a list of headers and misses headers that appear later, and both rewrite or tie paths
in ways a person sees.

The other candidates were weaker. TypeScript's cost is whole-program type checking, with no
per-unit compiler call to wrap, and hushrun already skips `tsc` as a check. Swift and Kotlin
compile per module inside their own drivers (Swift's own caching, Gradle's build cache); Zig
caches per compilation inside one process. C and C++ is the one where each unit is a separate,
expensive process behind a standard hook.

A spike (a launcher of about 900 lines, outside the repository) built abseil-cpp with its tests
(433 translation units of C++ with GoogleTest, 791 build steps) with CMake and Ninja, on macOS
(Apple clang 21, CMake 4.4.5, Ninja 1.13) and on Linux (GCC 12.2, CMake 3.25.1, Ninja 1.11), cold,
warm in another checkout path, and on a "second machine" (a fresh container at another path, the
store carried as a tar), beside ccache 4.14.3. The machine running it was shared and heavily
loaded (load average 300 to 500 on 10 cores), so CPU seconds are the steadier measure and wall
times are indicative. What it found decides most of what follows:

| abseil + tests, Release | wall | CPU (user+sys) | outcome |
|---|---|---|---|
| macOS, no launcher, cold (twice) | 167 s, 192 s | not recorded | |
| macOS, spike, cold, empty store | 130 s | 766 s | 431 stored |
| macOS, spike, another checkout, no prefix map | 150 s | 753 s | 140 reused, 292 tied to the first path |
| macOS, spike, another checkout, prefix map added | **11.9 s** | **52.8 s** | 433 reused |
| macOS, spike in preprocessor mode, cold | 207 s | 932 s | 431 stored |
| macOS, spike in preprocessor mode, another checkout | 23.3 s | 99.1 s | 433 reused |
| macOS, spike with places and probes, cold | 260 s | 841 s | 431 stored |
| macOS, spike with places and probes, another checkout | 16.7 s | 50.0 s | 433 reused |
| macOS, the same, every reuse compiled again and compared | 199 s | 766 s | 433 the same bytes |
| macOS, ccache (`base_dir`), cold | 158 s | 858 s | |
| macOS, ccache (`base_dir`), another checkout | 6.2 s | 31.0 s | 437 direct hits |

| abseil + tests, Release, Linux in Docker, `-j4` | wall | CPU | outcome |
|---|---|---|---|
| Linux GCC 12, no launcher, cold (`-j6`) | 330 s | 1941 s | |
| Linux, spike, cold, empty store (under the Mac's load) | 605 s | 2396 s | 431 stored |
| Linux, spike, a second container at another path, store from a tar | **15.2 s** | **55.9 s** | 433 reused |
| Linux, the same, every reuse compiled again and compared | 572 s | 2215 s | 433 the same bytes |
| Linux, ccache (`base_dir`), cold | 484 s | 1926 s | |
| Linux, ccache (`base_dir`), the second container | 17.4 s | 62.4 s | 438 hits |

- **Paths decide what travels, and debug info is not the main reason.** Without debug info, 292
  of 433 objects (34 of 165 library objects, 258 of 268 test objects) still hold the checkout's
  path, through `__FILE__` in abseil's logging and in GoogleTest's `TEST` macros; with GCC 12 on
  Linux, 127 of 433 are byte-identical between two paths as compiled. Two checkouts on one
  laptop therefore share a third of the build as compiled, and everything once the compile
  maps the checkout's folder to `.` (`-ffile-prefix-map`): with it, all 433 objects compiled
  again in the second checkout were byte-identical to the first's, and abseil's 359 tests passed
  in the second checkout built wholly from the first's objects. In a Debug build of
  abseil's 156 library units on macOS, objects byte-identical between the two paths: 1 as
  compiled, 117 with `-fdebug-prefix-map` (the other 39 hold `__FILE__`), 1 with
  `-fmacro-prefix-map` alone, 156 with `-ffile-prefix-map`, which is both.
- **ccache's fast mode serves stale objects, measured.** ccache 4.14.3 in its default direct mode,
  on a file that includes `"h.h"` with `-Ifirst -Isecond`: after `first/h.h` is created it still
  served the object built from `second/h.h`; after a file that `__has_include("opt.h")` asked
  about appeared, it served the object built without it. The spike, recording each place a header
  could have been found before the one it was and each name a `__has_include` asked about, built
  both again and reused the earlier object once the files were gone. On abseil that is about 300
  places and probes a unit beside its 888 reads, and costs 3 ms a hit (79 ms against 76 ms).
  Preprocessor mode is correct
  by construction for both, and costs a preprocessor run on every hit and every miss: 335 ms a hit
  against 76 ms in direct mode, 409 ms more a miss (22% more CPU cold).
- **A hit costs hashing what the unit read.** A unit of abseil's tests reads 888 files on average,
  80% of them under the SDK (headers and libc++), 4% under the toolchain. Hashing them all at every
  hit is most of the spike's 76 ms. ccache's 31 s against the spike's 53 s for the same warm build
  is most likely its inode cache (on by default), which answers a header's digest from its stat
  without opening the file. ccache also names the compiler by its size and date by default
  (`compiler_check = mtime`), which is fast and says nothing across machines.
- **The hook can sit in CMake's cache without depending on hushrun.** CMake caches a launcher
  read from the environment (`CMAKE_CXX_COMPILER_LAUNCHER:STRING=…`), on the first configure and on
  any later configure of a build folder whose cache has none (CMake 4.4.5 and 3.25.1). A launcher
  of `/bin/sh -c 'exec ${HUSHRUN_CC:+"$HUSHRUN_CC"} "$0" "$@"'` names nothing of hushrun's: with
  `HUSHRUN_CC` unset the shell replaces itself with the compiler, same arguments, same stdio, same
  status; Ninja's command text is the same in and out of a run, so `ninja -n` says "no work to do"
  both ways and nothing rebuilds when a person moves between plain `ninja` and `hushrun run`;
  CMake leaves launchers out of `compile_commands.json`, so clangd sees the compiler. The cost is
  one more shell a compile: 6.9 ms on macOS, where `/bin/sh` is bash (2.3 ms with `/bin/dash`),
  and too little to measure on Debian, where it is dash.

## Decision

1. **Entry: CMake's compiler launcher, as an indirection that is inert outside a run** (B-50).
   Wherever `hushrun run` sets `RUSTC_WRAPPER` (B-2), it also sets `CMAKE_C_COMPILER_LAUNCHER` and
   `CMAKE_CXX_COMPILER_LAUNCHER` for COMMAND to `/bin/sh;-c;exec ${HUSHRUN_CC:+"$HUSHRUN_CC"} "$0"
   "$@"`, each only when the
   person's environment has none, and `HUSHRUN_CC` to a link `hushrun-cc` beside `hushrun-rustc`
   that points at the hushrun binary. What CMake caches is the indirection: no session path, no
   file of hushrun's, nothing that breaks if hushrun is removed. A launcher already in a build
   folder's cache (ccache, sccache, distcc) wins, and hushrun's never runs there. A build folder
   configured before hushrun is picked up at its next configure inside a run, CMake's own re-run
   after a `CMakeLists.txt` edit included; until then it compiles as before and the run says so
   once. Refused:
   - `CC`/`CXX`: CMake, Meson and autotools resolve and cache the compiler they name, CMake runs
     its compiler identification through it, and the Rust `cc` crate prints
     `rerun-if-env-changed=CC`, so every toggle would rerun every build script.
   - `PATH` shims named `cc`, `c++`, `gcc`, `g++`, `clang`, `clang++` (ADR 0080's way in for
     `ctest`): CMake and Meson cache the absolute path they find, which would be the session's
     folder or a file of hushrun's in the person's cache, and a later plain build would fail.
   - A launcher naming `hushrun-cc` directly: cached, so a later plain build fails once the link
     or hushrun is gone.
   - Rewriting or wrapping `build.ninja`: Ninja keys each output by its command's hash, so every
     move between a run and a plain build would rebuild everything.
   Meson (no launcher variable; it uses ccache or sccache only when it finds them at setup), plain
   Makefiles and autotools, and the Rust `cc` crate get nothing in this decision (phases 3 and 4).

2. **One source to one object, or exactly as asked** (B-51). The launcher handles a call with
   `-c`, one source of C or C++ (by extension, or `-x c`/`-x c++` before it), one `-o`, and any
   of `-MD`, `-MMD`, `-MF`, `-MT`, `-MQ`, `-MP`. Everything else is passed through untouched, and
   these are named: a link or anything without `-c`; `-E`, `-S`, `-M`, `-MM`; several sources;
   `@file`; precompiled headers made or used (`-x c-header`, `-x c++-header`, `-include-pch`, an
   `-include` whose `.gch` or `.pch` exists, a read file that is one); modules (`-fmodules`,
   `-fmodule-*`, `-fcxx-modules`, `.cppm`, `.ixx`, `@…modmap`); profiles read or written and
   coverage (`-fprofile-*`, `-fauto-profile`, `--coverage`, `-ftest-coverage`, `-fprofile-arcs`,
   `-fcoverage-mapping`); a second output (`-gsplit-dwarf`, `-save-temps`, `-ftime-trace`,
   `-fstack-usage`, `-fdump-*`, `-MJ`, `-fcrash-diagnostics-dir`); flags that load code or read
   files the depfile doesn't list (`-fplugin`, `-Xclang`, `-mllvm`, `--config`, `-B`); GCC's
   `-flto` without `-frandom-seed` (two compiles of one file differ, measured on GCC 12);
   Objective-C, Objective-C++, assembly and CUDA; any `DEPENDENCIES_OUTPUT`,
   `SUNPRO_DEPENDENCIES` or `CC_PRINT_*` the person set; a compiler that is itself a launcher
   (`ccache`, `sccache`, `distcc`, `icecc`, `buildcache`, by name or link
   target); MSVC and `clang-cl`; Windows. Every other flag is keyed by its text: the compiler is
   the authority on what it means, and the audit is the net for one that reads a file unseen.
   `-march=native`, `-mcpu=native` and `-mtune=native` are keyed by what the driver resolves them
   to and are never portable, as B-5 keys rustc's `native`.

3. **The key: hushrun's two keys, read directly, with the places a header could have been**
   (B-52 to B-54). Not ccache's direct mode, which trusts the list of headers; not preprocessor
   mode, which is 4.4 times slower a hit and 22% slower cold.
   - The **first key** is what was asked: the compiler's identity, the arguments in order with
     roots and bases replaced and `-o`, `-MF`, `-MT`, `-MQ` left out, the compile's folder from its
     root, the variables a compiler reads without an argument saying so, and what the launcher
     adds (prefix maps).
   - The **second key** is what was read: each file of the compiler's full dependency list, by its
     path from a root or base and its SHA-256; each **place**, a path where a header would have
     been found first had it existed (every folder searched before the one that held it, the
     including file's folder among them), as absent; and each **probe**, a literal name a read
     file asks `__has_include` or `__has_include_next` about, in every folder searched, as present
     or absent. The including file is known from clang's own record of the include graph, which
     it writes to a file when asked (`CC_PRINT_HEADERS_FORMAT=json`,
     `CC_PRINT_HEADERS_FILTERING=direct-per-file`, `CC_PRINT_HEADERS_FILE`; Apple clang 21 writes
     it, Debian's clang 14 doesn't); for GCC and an older clang the source's own folder stands for
     every including file's, which leaves one case to the audit.
   - **Not stored**: a read file holding `__TIME__`, `__DATE__` or `__TIMESTAMP__` (unless
     `SOURCE_DATE_EPOCH`, keyed, gives the first two a value); a probe whose argument isn't a
     literal; a read outside every root, base and system folder; a read file changed while
     compiling (B-11).
   - The **compiler's identity** is its content: the binary the call reaches (Apple's
     `/usr/bin/cc` and its siblings are `xcrun` trampolines, resolved with `xcrun -f` under the
     call's `DEVELOPER_DIR`, `SDKROOT` and `TOOLCHAINS`), for GCC also `cc1`, `cc1plus` and `as` as
     `-print-prog-name` names them, the driver's account of a compile with no flags (`-###`:
     target, resource folder, sysroot, implicit flags, configuration files), and the SDK's
     `SDKSettings.json`. Like Go's (B-43), it is memoised by path and stat. Unlike Go's, it does
     not hash the headers of the toolchain and the SDK up front: they are read files like any
     other, named from their **bases** (`{sdk}`, `{toolchain}`, the system include folders), and
     their digests come from a run memo that answers by stat, as ccache's inode cache does, which
     should bring a macOS hit from 76 ms towards ccache's (phase 2 measures it).

4. **Portability: the launcher maps the checkout's folders, in a run that shares and in one that
   doesn't** (B-55, B-56). It adds `-ffile-prefix-map=<root>=.` for each root in its given and
   resolved forms, longest first, as the `rustc` wrapper adds `--remap-path-prefix` (B-9); the
   addition is in the first key. `__FILE__`, assertion messages and debug info then name files
   from the repository's root (`./absl/log/log.cc`), and a debugger finds sources from the
   repository's root or with a source map. Without it, two worktrees on one laptop share a third
   of abseil and none of a debug build; ccache's own answer, `base_dir`, rewrites the compiler's
   arguments to relative paths and changes `__FILE__` the same way. A block whose bytes still hold
   a root, the home folder or a base's value is tied (B-24's rules, read for C). A person who
   wants absolute paths sets `build: cc: maps: off` (owner decision 4), and their blocks are tied
   to the checkout.

5. **A restored object looks freshly built** (B-57). The object is written under a temporary name
   in its folder and renamed into place, dated now. The depfile is written from the hint that
   made the key, with this checkout's roots, in the syntax the compiler writes (the target `-MT` or
   `-MQ` names, spaces escaped, `-MP`'s phony targets, `-MMD`'s system headers left out), never
   from the downloaded block. Messages are replayed with this checkout's roots. Ninja and Make
   then see an output newer than its inputs (measured: `ninja -n` has no work after a warm build,
   `ninja -t deps` names the second checkout's headers), and CTest's stale-program rule (ADR 0090,
   Amendment 8) compares an object dated now with its sources and a program linked after it.

6. **Trust as for Go, and nothing in the build checks an object** (B-59, B-60). Ninja, Make and
   the linker accept any file with the right name and format. A forged object is code that runs
   in every test program and binary linked from it, on a laptop and in CI. So every check is
   hushrun's: the downloaded object's SHA-256 and size are its record's; its format is the one
   the compile makes (Mach-O `MH_OBJECT` or ELF `ET_REL`, of the call's target architecture, LLVM
   bitcode for a clang `-flto`); `Shared::may_reuse` decides from its record (B-34); `audit::sample`
   misses on purpose one time in fifty, one in ten on a download's first reuse, and a difference
   suspends the key, reports `differs` and keeps the compile. The depfile is written from the
   files this machine hashed, so a forged hint can name a key no block has or one a forged block
   has, and nothing else. Messages from a downloaded block lose every control character but SGR
   colour. Format
   checks catch garbage, not malice: the audit is the only check of meaning, as for Rust.

7. **Every failure runs the compiler unchanged** (B-58). Before the launcher writes anything, any
   failure `exec`s the compiler with the arguments as given, so stdout, stderr and the exit
   status are the compiler's own. After it starts a compile of its own, the compiler's stderr is
   passed on line by line as it comes and its status returned, a signal re-raised. A restore
   that fails part way compiles as asked over what it wrote. A panic, a store that can't be
   written, a memo that can't be read, a relay that doesn't answer: each ends in the compiler.

8. **One relay, one store, one outbox, a third kind** (B-60). C and C++ blocks are kind `cc/1`,
   class `library`, through the same relay, records, vouches, outbox and withdrawals as `rustc/1`
   and `go/1`, with a manifest of their own per check (`open` and `close` of kind `cc/1`, the
   platform the target triple the driver names), and a fetch looked up only when the manifest
   names it. The server needs nothing new.

9. **State has one home each** (B-13, B-14, B-57). A block is absent, written under `tmp/`, or
   published by one rename; a hint is written whole under its first key; an outbox marker keeps
   B-29's states; the run memo's entries are written whole and answer only while their stat holds.
   The launcher entry in a person's CMake cache is not hushrun's state: it is inert text, and
   `cmake -U CMAKE_CXX_COMPILER_LAUNCHER` removes it.

## Consequences

- A CMake build of C and C++ inside `hushrun run`, with Ninja or Makefiles, reuses its objects
  between worktrees from the first phase, and between machines through the relay from the
  second, with no setting but build reuse being on.
- `__FILE__` and debug info name files from the repository's root inside a run. Programs that
  print `__FILE__` print a relative path; a debugger needs a source map or to start from the
  repository's root. This is the visible cost of reuse between paths, as for ccache's `base_dir`.
- A build folder configured inside a run keeps a one-line launcher in its cache. It costs a
  shell a compile outside runs too (about 7 ms on macOS, nothing measurable on Debian).
- A laptop and CI share C and C++ objects only between the same compiler, SDK or system headers
  and target. A macOS laptop and Linux CI share nothing; CI jobs of one image share fully, and a
  Linux laptop shares with CI only when its compiler and system headers are CI's by content.
- Precompiled headers, modules, coverage and profile builds compile as before until a later
  phase decides them. Objective-C, CUDA and MSVC compile as before.
- A hit is a few tens of milliseconds of hashing (30 ms on Linux, 79 ms on macOS before the run
  memo); a cold build pays one hashing pass after each compile (47 to 141 ms a unit in the spike,
  under 3% of a unit's compile).

## Alternatives

- **Preprocessor mode** (sccache; ccache's fallback): correct by construction for headers that
  appear later and for `__has_include`, and 4.4 times slower a hit; refused for direct reads with
  places and probes, which the spike showed catch the cases ccache's direct mode misses.
- **ccache's direct mode as it is**: fastest, and serves stale objects in two measured cases.
- **Hashing the SDK and toolchain headers once into the identity**, as Go's identity holds
  `pkg/include`: thousands of files no compile reads, hashed per SDK; refused for a run memo of
  the files actually read.
- **Leaving paths as they are and tying blocks**: no visible change, a third of abseil reused
  between worktrees, nothing in a debug build; offered as the setting `maps: off`.
- **Delegating to ccache or sccache** behind hushrun's relay: their stores can't be vouched for,
  withdrawn or audited by hushrun's rules, and their keys hold no compiler identity by content
  across machines.

## Build plan

Each phase is one builder's work and ends green on macOS and Linux.

1. **Local reuse between worktrees.** `hushrun-cc` (name check like `hushrun-rustc`), B-50's
   environment, B-51's parser with a table test per refused flag, the identity and its memo
   (B-52), both keys with places and probes (B-53, B-54), the clang include graph, prefix maps
   (B-55), place and portability (B-56), restore (B-57), fallback (B-58), the audit's local
   comparison, the run's lines (B-61), without a session (B-62). Tests: abseil-like fixture at two
   paths; shadowing header and `__has_include`; a panic and a store that can't be written leave
   the compiler's stdout, stderr and status byte for byte; CTest's stale-program rule after a
   restore; a plain `ninja` after a run has no work.
2. **The run memo for bases**, so a hit costs about what ccache's does: digests of files under
   the SDK, toolchain and system folders answered by stat across the run's processes.
3. **Sharing through the relay**: kind `cc/1` (B-60), format checks of a downloaded object, the
   manifest per check and platform, prefetch, vouches, reports and withdrawals, `audit::sample`
   and `Shared::may_reuse` called as Go calls them; a Linux→Linux test at two paths through the
   fake backend.
4. **Wider entry**: response files (read, keyed by content), Objective-C and Objective-C++,
   GCC's include graph, Meson (owner decision 6), the Rust `cc` crate (owner decision 7).
5. **Precompiled headers and coverage**: a PCH as a block keyed by its own reads, and its users
   keyed by the PCH's key, not its bytes; `-ftest-coverage` with its `.gcno` as a second output;
   profiles read (`-fprofile-use`) keyed by content.

## Decisions for the owner

1. **The kind's name.** Recommended `cc/1`: one kind for C and C++, clang and GCC, as `CC` and
   the `cc` crate name them; the compiler's identity is in every key, so nothing is gained by
   one kind per compiler, and `c++` is not a kind `is_kind` reads.
2. **A line in the person's CMake cache.** Recommended yes: a build folder configured inside a
   run keeps the inert launcher for good. It is the one way found to reach CMake builds with no
   setting while a plain build stays the compiler's, byte for byte. The alternative is to set it
   only in CI, where build folders are thrown away, and have a person opt a folder in
   (`hushrun build cc on <folder>`, which would write the cache entry itself).
3. **The variable's name, frozen in caches.** Recommended `HUSHRUN_CC`. Once a cache holds the
   indirection, renaming the variable leaves those folders without reuse until reconfigured.
4. **Prefix maps by default.** Recommended on, mapping each root to `.`, as the `rustc` wrapper
   maps the workspace: without them a third of a release build and almost none of a debug build
   travel between two worktrees of one laptop. The cost is visible: `__FILE__` and debug info
   name files from the repository's root. `build: cc: maps: off` in `hushrun.yaml` (or the
   person's settings) keeps absolute paths and ties every block that holds one.
5. **GCC's including file.** Recommended: accept the source's folder standing in for it, with the
   audit as the net, and measure before doing more. The exact graph would need `-H`, whose lines
   GCC writes to stderr among the diagnostics and the wrapper would have to take out.
6. **Meson.** Recommended: not until someone asks. Meson has no launcher variable; the same
   indirection through `CC` and `CXX` at `meson setup` would work, but those variables reach every
   other tool COMMAND starts.
7. **The Rust `cc` crate** (ring, zstd-sys, mimalloc in this repository). Recommended: a spike in
   phase 4 before deciding. The way in that looks safe is `PATH` shims named `cc` and `c++` that
   act only under a cargo build script (the crate leaves `PATH` out of `rerun-if-env-changed`, so
   no build script reruns); the risk is a build script that runs CMake (the `cmake` crate), which
   would cache the shim's path in its `OUT_DIR`.
8. **Precompiled headers.** Recommended: pass through until phase 5, and build it only if a real
   repository asks. Keying a PCH's users by the PCH's key, not its bytes, is new machinery.
9. **The local store's size.** Objects are stored as compiled: 137 MB for abseil with its tests,
   where ccache keeps 48 MB compressed. Recommended: keep B-13's layout (a reuse clones, which a
   compressed store can't) and B-14's expiry, and revisit with numbers from real repositories.
10. **The shell.** Recommended `/bin/sh` everywhere: on macOS it is bash (7 ms a compile) and
    `/bin/dash` would cost 2 ms, but a cache that names `/bin/dash` breaks on Linux systems
    without it, and CMake build folders do get copied between machines.
