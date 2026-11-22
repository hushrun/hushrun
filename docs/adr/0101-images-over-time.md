# 0101. Images over time: compaction by age, the re-bake limit, branch bakes, dependencies first, and the measured win

- **Status:** proposed; built
- **Date:** 2026-11-22
- **Rules:** B-72 to B-74; B-65, B-66 and B-69 changed; B-26, B-63, B-67, B-70 read

## Context

[ADR 0099](0099-baked-images.md) publishes a baked store as a layered image after each build of
the default branch. Living with such images over weeks raises five questions it left open:

1. **The workspace layer only grows.** It keeps blocks no longer used until half its bytes are
   stale or the recent layer outgrows its bound. A slow trickle of changes never reaches either,
   and every sandbox pulls the dead weight.
2. **Main moving fast.** A repository merging every few minutes would publish an image per merge,
   and every consumer that watches the tag would rebuild a snapshot per merge, for a handful of
   changed blocks the relay could fetch anyway.
3. **Long-lived branches.** A release or refactoring branch others work on wants its own warm
   image, without its blocks ever passing for the default branch's.
4. **A new repository.** Its first image waits for CI's first full build, tests and all, then the
   bake. The dependencies, most of a cold build in most repositories, could come first.
5. **Showing the win.** The plan asks `init` to say how much faster a sandbox starts. `init` runs
   before CI exists, and a number nobody measured is worse than none.

## Decision

1. **The workspace layer is written again once a week** (`build: baked: compact_every`, `7d`;
   `0s` turns the age off), beside the size rules. The image's label records `compacted_ms`, the
   baking time of the bake that wrote the layer, and each bake that keeps the layer carries it;
   a bake compares it with its own time (`baked.json`'s `baked_ms`) as it runs. No cron, no state
   file: the time lives in the image being built on, which a bake reads only once it counts (a
   signer hushrun.yaml names signed it, B-65), so the time is as trustworthy as the layer. An
   unrecorded time, or one more than ten minutes past the bake's clock, counts as old (review 9: a
   day's slack kept a layer a forged or wrong clock dated ahead for a day longer).
2. **A bake waits while the image its tag names is young and little changed** (B-73): younger than
   `rebake_every` (`1h`) by that image's signed `baked_ms`, with fewer than `rebake_blocks` (50) of
   this store's packs absent from it. It publishes nothing, moves no tag, exits 0 and says why;
   `--force` publishes. Over an image that doesn't count there is nothing to wait on. One decision
   (`Rebake::holds`), made before the image is assembled, for any registry and for a backend's.
   Archives and folders are written whatever the limit: they are files the person asked for.
3. **Branches are baked by a run on the branch** (B-69). `build: baked: branches:` lists them; on
   GitHub the bake workflow's `workflow_run` lists them too, and a second job, whose only right is
   `actions: write`, starts the workflow by hand on the branch whose build passed for a push. That
   run bakes `--from backend --branch-tag` and signs keylessly as the workflow **on that branch**.
   On GitLab the branch's own pipeline bakes. The moving tag never moves from a branch (B-66), and
   the signer init prints stays the default branch's: a run on the default branch trusts a
   branch's image only if hushrun.yaml names that branch's identity, which init says and never
   writes.
4. **`--deps-only` stubs the repository's own crates in a real build** (B-72): the check's cargo
   command runs in a store and target folder of the bake's own, and the wrapper replaces the one
   source of each path package inside the checkout with an empty file before anything else, then
   runs it untouched by the store. Cargo's graph, features and flags are the real build's, so every
   third-party crate compiles with the key the real build computes (the tests check the keys
   against a real build's on another machine, and a fresh machine reusing every one of them). A Go
   module's third-party packages (`go list -deps -test ./...`, neither standard nor main module)
   are built with `go build` under Go's cache program. C and C++ have no separate dependencies to
   build. The image's label says `deps_only`; it takes its fixed tag (its manifest names the
   dependencies alone, so the tag can't name a full store with blocks missing, the reason a partial
   store takes none), moves its tags only over nothing or another dependencies-only image, checked
   before it builds, and a full bake replaces it.
5. **`--report` measures, on the machine it runs on** (B-74): COMMAND cold in a fresh store and
   target folder, then with the store just baked underneath a fresh store, as a sandbox from the
   image runs it. With `--deps-only` the cold side is the dependencies-only build itself. Both
   wall-clock times, their ratio and the count of compiles the store served are printed; nothing
   is estimated. `init` can't measure, so its plan names `--deps-only` and `--report`.

## Alternatives considered

- **Compaction by a cron job, or a local state file of the last compaction.** A CI job starts
  empty; a schedule is one more trigger to secure and pay for. The image being built on is the one
  place every bake reads.
- **Compaction time from the registry's own dates.** Unsigned and registry-specific.
- **A re-bake limit by the bake's own clock alone** (a stamp file, a CI cache). The same objection;
  and the signed time is what B-67's `not_before` already compares.
- **Counting changed bytes instead of blocks.** A block is what a sandbox compiles when it lacks
  it; one large crate changed is one compile. Bytes would also need the previous image's sizes per
  pack, which the label has; left for the owner (below).
- **A branch bake by `workflow_run` alone, checking out the branch's commit.** Its certificate
  would name the default branch's workflow file and ref: a branch's blocks signed as the default
  branch's, trusted by every run on it. `workflow_run_signs` already refuses it (exit 9).
- **Branch bakes on `push` to the branch.** They would race the branch's build, whose blocks the
  bake reads from the backend.
- **`cargo chef`-style skeletons** (a copy of the manifests with stub sources in another folder).
  It changes the workspace's path, loses path dependencies outside it and vendored sources, and
  writes stubs into a checkout. Stubbing in the wrapper keeps every path and every input of a
  third-party compile as it is.
- **`cargo build -p <dependency>` for each third-party crate.** Features are unified over the
  packages selected, so a crate built alone gets other features than in the real build, and other
  keys.
- **Stubbing by `CARGO_PRIMARY_PACKAGE`.** Cargo sets it only for the packages a command selects:
  in a workspace with a root package, the other members would compile for real.
- **A deps-only store with no fixed tag, as a partial store.** It has every block its manifest
  names; withholding the tag would hide it from a watcher that reads fixed tags, for no safety.
- **`init` printing an estimate** from the blocks' recorded compile times. Their sum is CPU time
  in one place and wall time nowhere; a sandbox's time depends on its cores, its disk and its
  network.

## Consequences

- An image's workspace layer is at most a week old; a weekly compaction uploads that layer once.
- Main merging faster than hourly publishes at most one image an hour unless 50 blocks changed;
  sandboxes started in between fetch the difference through the relay.
- A branch bake costs a second, very short job on GitHub, and `actions: write` for it.
- A new repository's first image costs one dependencies-only build in a job of its own, which the
  team writes with the build's own setup steps (init prints how); the first full bake replaces it.
- A dependencies-only build compiles the repository's own crates as empty stubs in a target folder
  of its own, so it never leaves stub outputs where a real build would find them.

## Measurement

nushell 0.113.0 (686 crates, 42 of them its own) in Linux containers (Docker Desktop on an Apple
M-series Mac, 10 CPUs and 8 GB, `linux-arm64`, Rust 1.94.0, `cargo build -j6`, dev profile), a
release build of this branch, `registry:3` on a Docker network, no backend, each case in a fresh
container. Other work shared the host: load is given with each.

**The plan's "done when": a fresh sandbox's first build after a one-file change.** A line atop
`crates/nu-cli/src/lib.rs`. The warm laptop is a container that built the tree (incremental, its
own store); the sandbox is the base image plus one `COPY --from` of the image a CI-style build
(`CARGO_INCREMENTAL=0`) baked, read on the say-so. Four rounds, interleaved and rotated, load 3.8
to 16, medians:

| Case | wall (min–max) | CPU | reused / built |
|---|---:|---:|---|
| warm laptop | 14.4 s (13.6–29.6) | 16.4 s | 0 / 3 |
| sandbox, `CARGO_INCREMENTAL=0`, audit off | **36.4 s** (34.9–39.2) | 66.3 s | 671 / 15 |
| sandbox, `CARGO_INCREMENTAL=0`, audit one in ten | 38.7 s (38.1–43.2) | 90.4 s | 599–614 / 72–87 |
| sandbox, cargo's defaults (incremental) | 47.1 s (43.7–53.6) | 132.8 s | 556–579 / 107–130 |
| cold, CI-style, in the bake's own build | 67.8 s | 284 s | 0 / 686 |

**2.5 times the warm laptop: the 1.5x does not hold for nushell** (ripgrep was 1.4x, ADR 0098).
Of the 15 compiles left, 2 are the change (nu-cli and its dependent nu-lsp) plus the `nu` program;
the store lacks the other 12 because their blocks may not leave the machine that built them: the
C-building `-sys` crates (ring, blake3, libsqlite3-sys, libgit2-sys, lmdb-master-sys), uucore and
a build script hold the cargo home and home folder in their outputs, and **nu-command, the
largest crate, follows them** (its externs are such crates); three more were tied in the laptop's
non-sharing run. The rest is opening 671 packs (1.25 GB) into the sandbox's store. A sandbox at
the same paths as its baker could reuse those blocks; a baked store has no way to carry blocks
bound to paths yet (ADR 0098's open item).

**Dependencies first.** `--deps-only -- cargo build -j6` (42 own crates stubbed): the build took
58.4 s (649 compiles, 647 kept), the whole bake with the push and `--report` 88.6 s, and published
639 blocks, a 533.9 MB deps layer (the full image: 673 blocks, 627.8 MB). The full path is CI's
cold build, 67.8 s, then the bake, 15.1 s: for nushell, whose cold debug build is mostly its
dependencies and fast on 6 cores, the dependencies-only image lands no sooner by the clock; it
spares running the tests, and the wait for CI's pipeline. `--report` measured the dependencies'
build cold (58.4 s) against the store (18.1 s, 572 of 649 compiles served, the rest audited one in
ten on the say-so). A sandbox from the dependencies-only image building all of nushell, three
interleaved rounds against a cold one (load 12 to 18): **54.7 s** (42.2–56.2) against **101.6 s**
(98.1–102.3), 639 of 686 compiles from the store.

**Refresh against scratch.** Yesterday = 0.113.0, today = four merges later (bd4b478f). A
container from yesterday's image, today's checkout, build and bake: 56.2 s and 68.8 s for the
build (643 of 686 reused, 43 built), 20.1 s and 12.1 s for the bake; today's store has 30 packs
yesterday's lacks, 90.3 MB, which is what `hushrun build fetch` would download (fetch itself not
measured: no backend). From scratch: 110.7 s and 88.1 s cold, 12.7 s and 13.3 s to bake a 629 MB
store. Load 9.7 to 28.

**A new repository, install to first warm sandbox** (local, no backend): `init --dry-run --bake`
writes the job, but follows nushell's first workflow ("Security audit"), since init wraps no
cargo check of nushell's CI; the dependencies-only bake and push, 88.6 s; the sandbox image
(`FROM` + `COPY --from`), 9.5 s; its first full build, 54.7 s: about two and a half minutes, the
first image waiting on no CI run.

## Amendment: the bake builds what a skipped merge left unbuilt (B-75, the owner's decision)

The end-to-end run against a real backend showed the gap: after a merge, main's CI skips the
checks the pull request passed for this exact code, compiles nothing, and the bake republishes the
previous store; `-latest` goes stale while main moves. The owner chose that **the bake job
compiles what's missing**: when the default branch's newest manifest on the backend isn't of the
commit checked out (missing, no commit, or another), the bake runs the checks hushrun.yaml names
through hushrun as CI on the default branch, compiling only what no trusted store holds, then
bakes from the backend. The CI checks themselves still skip.

- **Detected from `open`.** `open`'s answer gains `manifestCommit`, the commit the manifest's run
  named in its `close`; one function compares it with `HEAD` (`bake_build::behind`). A backend
  that doesn't send it is treated as behind: a bake builds, reusing everything, rather than
  publish a store it can't date.
- **In-process, through the bake's relay.** The build's session shares through the same relay the
  bake reads with, in the bake's own store (so uploads read the outbox the wrappers write), with
  a fresh, non-incremental target folder, so the merged code's blocks are compiled, stored and
  uploaded as a trusted CI job's are. No results lookup and no result: the pull request's pass is
  neither consulted nor recorded as the job's.
- **Bounded.** 30 minutes for all checks, said in the log as it starts; past it the running check
  is stopped with its process group and the bake goes on.
- **`--no-build`** opts out.
- **The job has the toolchain.** init copies the build job's `uses:` steps (GitHub) or its image
  and `before_script` (GitLab) into the bake job.
- **The bake's relay downloads what it already holds.** Its own build just compiled and uploaded
  the merged blocks into the same store, and a prefetch skips what the store holds; a bake bakes
  from what arrives, so its relay now downloads every block and hint the manifests name.

Alternatives: a child `hushrun run NAME --force` would look up and record results (the one thing
to avoid); building in the bake's folder's own `target/` would pass incremental workspace crates
through and store none of them.

Open for the owner: 30 minutes; building on a missing `manifestCommit`; copying only `uses:` steps;
running every check hushrun.yaml names (tests included) rather than a build command.

## Open decisions

- The defaults: a week for compaction, an hour and 50 blocks for the re-bake limit, and counting
  blocks rather than bytes.
- Whether `init` writes the dependencies-only job itself (it needs the build's setup steps, which
  init doesn't know), and whether it would run on the push that adds the bake workflow.
- `actions: write` for the job that starts a branch's bake, and per-ref concurrency groups.
- `deps_only` in the label rather than in `baked.json`, so `hushrun build status` doesn't say a store
  is dependencies only.
- `--report` accepting the store it just baked in CI (outside the person's say-so rules), since
  nothing the measurement builds leaves its own store.
