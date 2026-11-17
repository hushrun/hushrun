# 0093. Go build blocks through Go's cache program

- **Status:** accepted
- **Date:** 2026-11-16
- **Rules:** B-39 to B-47; B-25, B-26, B-29, B-34 read for Go

## Context

Build blocks shared compiled Rust between machines ([ADR 0078](0078-sharing-build-blocks-through-a-backend.md),
[ADR 0091](0091-linked-outputs-between-machines.md)). Go has a build cache of its own, keyed by
its own action id, and from Go 1.24 lets an external program serve it (`GOCACHEPROG`): the `go`
command starts the program once per command and asks it to `get` and `put` every entry, over JSON
lines. A design built on that (the build blocks plan's §3.16, Phase 7) showed a second machine
building a module of 753 packages in 6.8 s from the first machine's archives, against 69 s cold.

Its review found what makes that unsafe as first designed. Go checks only the size of what a
cache program returns: one byte changed was linked into another program that ran. Go keys each
dependent by the content id an import's file *claims*, never by its bytes, so a forged archive
made honest machines store wrong dependents under honest keys, where they outlived the forged
block. Assembly that includes a header from outside its package gets one action id for other
bytes. A compile's printed output is replayed only from Go's own cache. And a design that kept
hushrun's own store for Go's local entries made the first run inside `hushrun run` cold, and a
later plain `go` rebuild everything.

## Decision

- **The local side stays Go's own cache.** The program reads and writes the person's `GOCACHE` in
  Go's own format, so plain `go` and `hushrun run go` share one cache, `go clean -cache` works, the
  first run inside `hushrun run` is as warm as plain Go, and an id Go's cache holds is never
  audited by a miss. Only package archives that may travel are added, as blocks in hushrun's
  store, beside it (decision 40 of the plan).
- **Only package archives of pure Go leave the machine**, judged from their bytes: `__.PKGDEF` and
  Go objects only, assembly only if the standard library's, a build ID naming the action asked,
  a content id that is its bytes', and a compile that printed nothing. Nothing is shared from a
  `go` command run with `-toolexec`, `-overlay`, another compiler, or compiler or assembler flags
  that name files. cgo, test results, the package index, cgo's generated files and vet's facts
  stay Go's.
- **The key is Go's action id and hushrun's identity of the toolchain**, the hash of every tool in
  its tool folder and its headers, because Go names a release toolchain by its version line alone.
- **One relay, one store, one outbox, two kinds.** Go blocks are kind `go/1` through the same
  relay, records, vouches, uploads and withdrawals as `rustc/1`. A check with a Go command asks
  `open` and sends `close` a second time, kind `go/1`, for its own manifest, and a Go fetch is
  looked up only when that manifest names it: Go asks about every entry its cache misses, and a
  server round trip for each would make a cold build slower than plain `go`. The server needs
  nothing new.
- **The program never hangs `go` and never kills it**: one answer per request by a
  compare-and-set, a watchdog and `catch_unwind` that answer a miss, every relay call bounded and
  none after one failed, nothing on stdout but protocol lines, nothing on stderr from a panic.
- **One definition for whether a download may be reused** (`Shared::may_reuse`), which the `rustc`
  wrapper now calls too, and one for the audit's pick (`audit::sample`).

## Consequences

- A Go module whose `go` is 1.24 to 1.27 shares its pure-Go packages wherever the run shares
  (B-25), with no setting; a newer Go gets no program until hushrun names it.
- Without `-trimpath`, Go's ids hold a package's folder: a laptop and CI share the standard
  library and whatever lies at the same path, and CI jobs of one repository share fully.
  `GOFLAGS=-trimpath` widens it, and changes the file names in stack traces.
- Above a cgo package, sharing is limited to machines with the same C toolchain and headers (243
  of caddy's 771 packages on Linux); `CGO_ENABLED=0` widens it.
- In a Go module, a `RUSTC_WRAPPER` set already (sccache) leaves only hushrun's own Rust wrapper
  out: the session begins for Go's cache program, and Go shares as it would without it.
  `hushrun build fetch` saves no Go blocks for a sandbox.
- Go's own cache is evidence of nothing: a Go cache hit with other bytes than a block's is
  counted, never reported or suspended (a `-toolexec` build may have put it there). Only the
  audit's miss on purpose reports `differs`; a downloaded archive that fails its checks is
  reported `rejected`, suspended, then removed.
- Go doesn't trim its cache while a program serves it, so the program trims it at `close` as
  Go would (`trim.txt`, once a day, five days and an hour).
- A relay that fails or times out once is down for the rest of the run (a file in the session's
  folder), so a frozen relay costs a run one wait instead of one per `go` command; a fetch that
  is only slow on a relay that still answers is a miss and leaves it on.
- A `go` whose version the probe didn't see keeps Go's own cache and leaves hushrun's store out,
  so a `GOTOOLCHAIN` switch inside COMMAND is as warm as plain Go and never served another
  toolchain's archives.
- The relay's tallies (bytes, uploads, comparisons) go with the `rustc/1` close; the `go/1` close
  carries the Go command's manifest and counts.

## Alternatives

- **hushrun's own store for every Go entry** (the design): cold first run, plain `go` warm only
  from its own cache, `go clean -cache` ineffective. Refused by the review.
- **A lookup for every miss**: correct, and slower than plain `go` wherever the server is far.
- **Writing downloads into `GOCACHE`**: they would escape a withdrawal, and `GOCACHE` holds no
  toolchain identity.
