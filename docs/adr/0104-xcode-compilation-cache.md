# 0104. Xcode's compilation cache, served by hushrun

- **Status:** proposed; built (the service, local reuse between checkouts and the Xcode app,
  blocks through the relay, baked stores), not yet run against a real Xcode: the Mac spike below
  decides what stays
- **Date:** 2026-11-24
- **Rules:** B-77 to B-85 (draft); B-13, B-14, B-15, B-24, B-26 to B-34, B-63, B-64 read for
  Xcode

## Context

iOS, iPadOS and macOS apps build with Xcode, and a new branch's CI job, a new worktree or a fresh
clone starts from an empty DerivedData: every Swift module and every Objective-C file compiles
again. hushrun reuses compiled Rust, Go and C and C++ between checkouts and machines, and none of
it reaches an Xcode build: [ADR 0097](0097-c-and-c++-build-blocks.md) passes Objective-C, Clang
modules and precompiled headers through, which is most of what Xcode asks Clang for, and Swift
compiles a module at a time inside its own driver, with no per-file process to stand in front of.

Since Xcode 26, Xcode keys each Swift and Clang compile by its content and can ask a
**compilation-cache service** for its result before compiling it: `COMPILATION_CACHE_ENABLE_CACHING`,
`COMPILATION_CACHE_ENABLE_PLUGIN` and `COMPILATION_CACHE_REMOTE_SERVICE_PATH` name a Unix socket,
and the toolchain's plugin speaks gRPC to it, as defined in Apple's fork of LLVM
(`llvm/lib/RemoteCachingService/RemoteCacheProto/compilation_caching_{cas,kv}.proto`): a key-value
store (`GetValue`, `PutValue`) from Xcode's key of a compile to a value, and an object store
(`Get`, `Put`, `Load`, `Save`) of bytes with ordered references, named by ids the service gives.
Xcode computes the keys; the service only keeps and serves. Open-source services of the same
protocol exist (Tuist's, Bitrise's, x8 with an S3 bucket behind it, CASBuildCache), which shows
the protocol is stable enough to build on and found its traps first:

- **Keys hold paths unless Xcode maps them.** x8's author found that only the prefix-mapping
  settings (`SWIFT_ENABLE_PREFIX_MAPPING`, `CLANG_ENABLE_PREFIX_MAPPING`, Xcode 27's
  `*_ENABLE_PROJECT_PREFIX_MAPPING`, and `*_OTHER_PREFIX_MAPPINGS` for the checkout, DerivedData
  and the products' folders) give two checkouts the same keys; and that
  `CLANG_MODULES_BUILD_SESSION_FILE` must be empty, since Swift keys that file's own path without
  mapping it.
- **Xcode 26 crashes on cached, prefix-mapped batch diagnostics**; Swift 6.4
  (swiftlang/swift#90700), in Xcode 27, fixes it. So prefix mapping, and so sharing, needs Xcode 27.
- **A value names its outputs** as a `CASObject` under the entry `value`, whose references are the
  compile's output objects and a schema marker (read from a real cache bucket by x8).
- **Settings attached to a project don't reach its Swift packages' targets** in the Xcode app;
  settings given to `xcodebuild` do.
- **A gRPC client refuses a message over 4 MiB by default**: the protocol's `write_to_disk` lets
  the service hand a large object as a file instead.

## Decision

1. **hushrun serves the protocol; Xcode computes the keys** (B-78). hushrun does not wrap `swiftc`
   or `clang` for Xcode: the service answers every call, over HTTP/2 on a Unix socket, with a
   protobuf codec written by hand (std only in `hushrun-build`; the HTTP/2 server is `h2`, already
   in the binary through reqwest). An id is the SHA-256 of `hushrun.xcode-object/v1`, the number of
   references, each reference and the bytes, so every machine names the same object the same way,
   and a value's bytes are a digest of everything below it.
2. **The local side is the service's own cache** (B-79), in the store's `xcode/` folder: every
   object and value Xcode puts, kept whatever leaves the machine, dated on use, trimmed after the
   store's kept days. This is what a new worktree or branch on the same Mac reuses, with or
   without a backend: DerivedData is per checkout, the cache is per machine.
3. **A block is one value and everything it reaches** (B-80), kind `xcode/1`, under the SHA-256 of
   `hushrun.xcode-key/v1` and Xcode's key (B-81): one download brings a compile's value and its
   outputs together, and the object lookups that follow are answered from the cache. Opening one
   checks every object against its id and that every object the value reaches is there, before
   anything is kept; a block that fails is refused whole, and a downloaded one is reported and its
   key suspended, as Go's are.
4. **What may leave** (B-82): a value whose outputs the service can name, all of them in the cache,
   under 256 MiB and 20,000 objects, with no output holding the person's home folder or the
   checkout's (on a person's machine; a CI job's folders name no person). Anything else stays in
   the cache and is still reused on this machine.
5. **Downloads follow a manifest per Xcode build** (B-83). Xcode asks for every compile's key, and
   most are ones no server holds: the relay asks the server only for a key the check's `xcode/1`
   manifest for the platform `xcode-<build>` names, and the first such key starts the prefetch of
   the rest, as a C or C++ first key does (B-60). The relay's manifest state is now one table by
   kind and platform for both.
6. **The audit is the same** (B-84): one block in fifty, one in ten at a downloaded block's first
   reuse, every one in `observe`, is answered as a miss on purpose and the value Xcode puts next is
   compared with it; a difference suspends the key and reports a download.
7. **Entry: `XCODE_XCCONFIG_FILE`** (B-77). `hushrun run` sets it for COMMAND, when the checkout
   holds an Xcode project, workspace or Swift package and `xcodebuild -version` names Xcode 27, to
   a file in the session's folder with the caching settings, the socket, and the maps with the
   checkout's own folder; a file the person set is included first (`#include?`). Xcode applies it
   to every target of a build, packages included, which project settings don't reach. Nothing is
   written into the project, and outside a run nothing changes.
8. **The Xcode app** (B-85). `hushrun build xcode on` installs a LaunchAgent that runs `hushrun
   build xcode serve` at login, on a socket at a fixed place, and sets `XCODE_XCCONFIG_FILE` for the
   login session (`launchctl setenv`), with the workspace's folder (`$(WORKSPACE_DIR)`) in the
   maps. It reads and writes the same cache as a run's, and asks no backend: a run downloads, the
   app reuses what is on the Mac. `off` undoes it and leaves a file the person set alone.

## The Mac spike

Nothing here has run against a real Xcode yet (it was built on Linux, tested with an HTTP/2 client
of the protocol). Before the status moves to accepted, on Xcode 27, measure and record:

1. An app with Swift packages and an Objective-C target, `hushrun run build -- xcodebuild build`
   cold, then in a second worktree: hits, CPU and wall time, and the run's `build: xcode:` line.
2. The same between two Macs through the backend (`hushrun build share on`), and in CI.
3. Whether the KV value's `value` entry is a `CASObject` naming every output, as x8 read it; if
   not, `unknown-value` keeps every value local, and the roots must be read another way.
4. Whether any object holds a path after prefix mapping (`holds-path` counts in the log), and
   whether a closure reaches inputs as well as outputs (block sizes).
5. Whether `XCODE_XCCONFIG_FILE` set by `launchctl setenv` reaches the Xcode app's builds, its
   package targets included, and whether `$(WORKSPACE_DIR)` maps the checkout for a project
   without a workspace.
6. Whether the plugin moves a file handed with `write_to_disk` across volumes (the cache's folder
   and DerivedData).
7. The audit's comparisons: whether Swift and Clang give the same value for the same key on two
   machines (the Merkle ids make that a byte comparison).

## Consequences

- An Xcode build in `hushrun run` reuses what another checkout, branch or machine compiled, from
  the first run, once the org shares; a new branch's CI job starts from what `main`'s trusted CI
  compiled.
- Xcode 26 gets nothing (no safe prefix mapping), nor does a newer Xcode until its major version is
  added to `probe::KNOWN_MAJORS` after a spike.
- Linking, asset catalogs, storyboards and code signing aren't cached: Xcode's cache is the
  compiles only.
- The cache holds every object Xcode puts (whether that includes the inputs keys are made from is
  the spike's question 4): it grows like DerivedData's own `CompilationCache.noindex`, and is
  trimmed like the store.
- A value the service can't read the outputs of is reused on its machine only.
- The Xcode app's service doesn't download: on a fresh Mac, the first `hushrun run … xcodebuild`
  fills the cache the app then reads.
