# 0078. Sharing build blocks through a backend

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** B-5, B-14, B-15, B-16, B-21, B-22, B-24, B-25, B-26, B-27, B-28, B-29, B-30, B-32,
  B-33, B-34, B-35

## Context

[ADR 0077](0077-portable-build-blocks.md) made blocks portable: equal keys where the bytes are
equal, and one answer to "may this block leave the machine". What moves them between machines is
an org's backend, which keeps a shared store behind every machine's own store and serves it to
the callers its rules allow. The backend's side is its own; this record is the machine's side for
people's machines. CI and agents' sandboxes have their own rules, in
[ADR 0079](0079-sharing-build-blocks-in-ci-and-sandboxes.md).

The rules were attacked on paper before any of this was built, and the attack changed most of
them. The points that shaped the design: an upload of "everything the server lacks" would have
sent blocks a machine only downloaded back up under another name, undoing a withdrawal; a download
that finished after its compile left would have been reused later without ever being compared,
even while observing; one mode for local and shared reuse would have turned local reuse off for
a week of observing; a random salt would have made every retry a different pack the server
refuses; a fetch with no bound could cost several times a crate's compile on a slow link; and an
input the compiler isn't told about is in a block's bytes and not in its key.

## Decision

- **Two modes, one function.** B-1's mode stays reuse on this machine. Sharing has its own,
  `observe` or `on`, the org's, which a person may lower here (`hushrun build share`,
  `HUSHRUN_BUILD_SHARE`) and never raise; never above the local mode. One function of who runs,
  the two settings and what the org said decides both, and every caller asks it again (B-25,
  B-33).
- **A relay in the `hushrun run` process** talks to the backend for the wrappers, over a Unix
  socket in the session's private folder; the wrapper keeps no network code. Cargo never waits
  for the network: `open` goes in the background, and until it answers every request is a miss
  (B-26).
- **Never slower than building.** A fetch a wrapper waits on is started only when it is expected
  to arrive within half the crate's cost, and given up as soon as it no longer is; the cost is
  capped, scaled to this machine, and taken from this machine's own compile when it knows one;
  three failures rest the relay (B-26).
- **Sealed with the block's own key, the salt from the bytes** (B-27, B-28): the backend and the
  object store can't open a block; a retry sends the same pack; two contents under one key never
  share a secret. The pack's record leaves out what tells a reader which repository built it and
  when. Its reader checks the pack's hash, the seal, each file, the key, the first key and the
  digest of outputs, in that order, and computes that digest itself.
- **Where a block came from** is recorded beside it (B-32). Only a block this run built goes up,
  in the org and as the credential that built it; a downloaded block, and one from before the
  record, never do. A download is published only to a wrapper still waiting, never while
  observing, and it is reused only while a server the run belongs to vouched for it within a day
  (B-34). A difference or a rejection of a downloaded block is reported to the scope it came from,
  and suspends its first key for the kept days.
- **The outbox is a table of states**, each a place on disk, each move a rename or a file written
  whole, every answer of the backend placed in a row (B-29). A dead run's claims go back at the
  next relay's start and in the sweep. On a person's machine uploads end with the command: those
  in flight get 5 seconds, and a block whose upload three commands cut off is dropped.
- **Prefetch from the last manifest** (B-30), skipping what the store holds, so a fresh machine
  downloads before cargo asks and a warm one downloads nothing.
- **An input the compiler isn't told about**: a repository names such variables under `build:
  env:` (B-35); a block whose output holds the value, 8 bytes or more, of a variable outside its key that
  no CI system set stays here (B-24 `unkeyed-value`); a block compiled for `target-cpu=native` stays here (B-24
  `native-cpu`), since rustc gives the code generator more of the CPU than any key can say.
- **`zstd`**, a C library built with `cc` as `ring` and `mimalloc` are, compresses packs, in the
  binary and in `hushrun-core`'s `pack` feature only, never in the wrapper's crate.

## Consequences

- A person's org can turn sharing on, and a machine that observes keeps all its local reuse while
  it uploads and compares. `close` counts comparisons, those of another machine's blocks and those
  that differed, so a week of observing can be judged by more than an absence of differences.
- Compiled code leaves the machine, sealed, opt-in per org; the security page says what sealing
  doesn't cover: a registry crate's key is public, and a value read behind the compiler's back is
  in the bytes.
- Short runs on a slow uplink may never send a large block; another machine's copy is the one
  that matters.
- Uploads hold a pack over 64 MiB in a file and read it whole for its PUT; a 2 GiB pack costs that
  much memory while it goes.
- Only `hushrun run NAME -- COMMAND` shares: a check of hushrun.yaml run by `hushrun run CHECK`
  does no results lookup, so it never learns that the org shares.
