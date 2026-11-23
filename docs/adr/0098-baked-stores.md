# 0098. Baked stores: compiled code on a sandbox's own disk

- **Status:** proposed; built for Rust (`rustc/1`), Go (`go/1`) and C and C++ (`cc/1`) blocks
- **Date:** 2026-11-21
- **Rules:** B-63, B-64; B-12, B-14, B-28, B-32, B-39, B-42, B-46, B-60, B-61 changed; B-24, B-26,
  B-29 to B-31, B-34, B-36, B-37, B-56, B-59 read

## Context

A sandbox or a container starts cold. [ADR 0079](0079-sharing-build-blocks-in-ci-and-sandboxes.md)
gave it `hushrun build fetch`, which downloads in a setup phase what the newest manifest names
and keeps it for 24 hours, and the relay (B-26), which fetches during a build what a wrapper asks
for and gives up when a download wouldn't beat the compile. Both download at boot or during the
first build. The systems that start sandboxes all know how to start one from something already
on disk: an image layer (`COPY --from` in a Dockerfile), an archive a live snapshot unpacks, a
volume mounted read-only. What they need from hushrun is a folder of compiled code, and a rule
for how long a run may trust it.

Three things in the code stood in the way:

1. **The 24-hour rule of a fetch** (B-36). An image can be days old. The rule exists because a
   backend can withdraw a block (B-34) and a sandbox that can reach no server must stop reusing
   it within a day. A baked store needs a rule that doesn't expire on a clock, and still stops
   trusting a block once the backend has moved on.
2. **The store is written everywhere.** Every reuse dates a block's `used` file (B-14), the sweep
   renames blocks into `trash/`, publishes rename folders into `blocks/`. A shared volume or an
   image layer mounted read-only can't hold the store.
3. **This machine's store keeps no manifest.** A run that shares sends its manifest with `close`
   (B-30) and keeps nothing; a machine with no backend has nothing that says which blocks a
   check's last build used, so a bake from it would have to guess.

The packs (B-28) already have the property a published folder needs: sealed with each block's
key, so whoever reads the folder without the same sources, flags and compiler reads ciphertext.

## Decision

1. **`hushrun build bake --out DIR`** writes sealed packs, sealed hints and `baked.json`, and
   nothing else (B-63). The packs are the source's bytes, unchanged, so a block has one form
   everywhere it travels. One function (`baked::check_contents`) refuses anything else in the
   folder (a file, a folder, a link, a pack that doesn't begin as one, a pack `baked.json`
   doesn't name) before the folder, written beside `--out`, is renamed into place; `--out` must
   be new, empty, or a baked store, so bake never deletes anything else.
2. **One store, three kinds.** A store has one part per kind and platform: `rustc/1` for the
   Rust compiler's target triple, `go/1` for `GOOS_GOARCH`, `cc/1` per target triple a run
   compiled C or C++ for. Each part has its own manifest identity, and the store's `manifest` is
   the digest of its parts'. Each kind looks in this machine's store, then the baked store, then
   the relay, decided by the same `baked::state`.
3. **Only what may leave any machine goes in from a machine's store**: portable crates (B-24), Go
   archives (which the store keeps only when portable, B-42) and objects with no reason to stay
   (B-56). Never a block bound to the folders of the person who baked it: B-24 lets bound blocks
   leave only from a CI job, because a person's folders carry their user name, and an image is
   pushed to a registry. No folder is written in plain text: a part's `binds` names each folder
   its blocks may be bound to by role and the SHA-256 of the role and the folder, normalised.
4. **Trust is the run's evidence, never the file's word.** `trust` in `baked.json` is a claim a
   bake makes, and the reader refuses combinations no bake writes (`trusted` from a machine's
   store, `trusted` with a row outside the trusted scope, the one definition
   `blocks::is_trusted_scope`). But a folder anyone could write says `trusted` as easily as a
   bake, so a run trusts a block only on evidence of its own (`baked::Evidence`): the server's
   word, when the run's relay talks to the backend the store was baked from, for the store's
   repository, which must be the run's (a store of another repository is its own state); or the
   person's say-so, `HUSHRUN_BUILD_LOWER_UNTRUSTED=1`, outside CI only. As the run's check begins,
   before COMMAND starts, the relay waits for `open` and looks every pack up without fetching,
   within 5 seconds (the wait a CI job gives `open`); a block the backend serves in the trusted
   scope with the row's digest of outputs is verified, the list goes in the session's folder, and
   only verified blocks are opened, each with the server's word in its record. CI never reuses a
   block no server vouched for, so nothing built on such a block goes up to `protected/`. A
   signature over `baked.json` and the packs, tied to the org's CI identity, is the other way to
   prove trust, offline; it is the seam `Evidence` leaves, and hushrun checks none yet.
5. **The newest manifest is the source's.** From this machine's store: every run of a check with a
   session now leaves its manifests in the store, one file per repository, check and Rust target
   triple, with a part per kind and platform and the run's folders as digests, saved under the
   file's lock and written whole under a name no other process uses, so the newest is the one
   file there. Cargo calls the
   compiler only for what changed in a warm target folder, so the file keeps the last one's
   pairs, each first key a run compiled again given its new block in place, its new pairs after,
   and a pair whose block the store dropped left out: a laptop's manifest names its whole build.
   From a backend: what `open` answers, downloaded as B-36 does. A team's own bucket is no
   source: hushrun has no bucket protocol for blocks, and this decision doesn't invent one.
6. **"Newer" is "another identity named by the same source for the same repository, check and
   kind and platform"**, never a date. A manifest's identity is a digest of its kind and pairs, so a
   backend's and a machine's manifest of the same run have the same one. A source keeps one
   newest manifest per slot, so whatever else it names there replaced the store's.
7. **Replaced is learned before anything is reused, and never forgotten** (B-63). The same check
   as a run begins asks `open` for each part's manifest of a store baked for a check; a newer one
   marks the store replaced in the run's own store before COMMAND starts, so a fresh sandbox's or
   a CI job's empty store learns it on its first run. A mark is never swept: a store with another
   manifest is another store, and a replaced store turning current again with time was a bug. A
   store baked for no check is never marked: the repository's newest manifest of any check would
   replace it at every other run. No relay, no answer: the store stays as it was. A replaced
   store's blocks stand as downloads (B-34), whose server's word the relay asks for and records.
8. **Only the store's whole identity can make it unusable.** A store of another repository, with
   no part for this machine's platforms (a target triple compared in a tag's words, so GNU and
   musl builds of hushrun read one store), or with no evidence, is ignored with one line. A part
   of another platform is not read; a folder a part binds that differs here is one line: a bound
   block's key holds its folders, so it only misses. The run's folders come from the run (its
   checkout, `CARGO_TARGET_DIR`, cargo and rustup homes, `HOME`, and Go's `GOROOT`, `GOPATH`,
   `GOMODCACHE`), normalised.
9. **The store underneath is read-only** (B-64). `HUSHRUN_BUILD_LOWER` names it. The wrapper, the
   Go cache program and the launcher look in the upper store, then the baked store, then the
   relay; a block found underneath is opened with its key and published into the upper, which
   every later reuse, record check (B-6, B-15, B-41, B-59), sweep and floor (B-14, B-22) is
   about. Nothing writes, dates or removes anything underneath.
10. **Go's cache program runs in every session of a Go module**, not only in one that shares
    (B-39), and keeps a portable archive in the store in every run (B-42), so a laptop with no
    backend has Go packages to bake and a sandbox reads a baked store's through the program. It
    goes up only from a run with a relay, as before.
11. **A bake is crash-safe.** It writes a new folder named for its process and exchanges it with
    the old store in one step (Linux `renameat2`, macOS `renamex_np`), else renames the old one
    aside first; the next bake puts back a store a dead bake left aside and removes its other
    leftovers. A store missing blocks its manifests named is written as partial, said with the
    count, and the bake exits 7.
12. **A pack's reader sets the run bits** of a program's, a build script's or a proc macro's linked
   output wherever it has a read bit, as rustc writes it under the umask (B-28). A pack keeps no file mode; before this, a build script opened from a pack (a
   download, B-30, or a baked store) was stored without it, and cargo failed with "failed to run
   custom build command … Permission denied". The baked ripgrep below found it; the relay's
   downloads had the same fault. And since a block an earlier hushrun stored that way keeps its
   mode, a reuse also gives a linked output of a program, build script or proc macro its run
   bits wherever it is readable (B-12), whatever mode the store kept.
13. **An older hushrun can't mistake a baked block for its own.** A block opened from a baked
    store gets, beside `baked`, an entry of vouched for whose host is `hushrun:baked-store`: an
    older hushrun sharing the cache folder ignores the field it doesn't know and reads a download
    whose server's word is stale, never a block built here.
14. **The wrapper's crate still holds no zstd and no cipher** ([ADR
   0078](0078-sharing-build-blocks-through-a-backend.md)): the binary installs what opens a pack
   (`baked::Unseal`) before the wrapper runs, the same code that opens a download.

## Alternatives considered

- **Bake the store's own folders** (`blocks/…`, unpacked), and let the wrapper clone straight from
  a read-only lower store. Faster to read (no zstd, no cipher, no copy into the upper), but the
  bytes of every block would sit in plain text in an image anyone with a pull token can read,
  and a block would have a second format. Rejected: packs stay sealed.
- **A relay that serves the baked store**, as a saved fetch is served (B-36). It would reuse the
  relay's opening, but it makes `Shared` a relay, which on macOS changes proc macros' and build
  scripts' keys (B-9) between a run with a baked store and one without, and a run with no backend
  would need a relay with nothing to talk to. Rejected: a store underneath is a store, not
  sharing.
- **Keep the 24-hour rule and stretch it** (a week). Still a clock that a snapshot outlives, and
  no better at withdrawals than knowing the backend moved on.
- **Compare manifests by date.** The backend's manifests carry none, and two machines' clocks
  would decide. Identity plus "the source names one newest per slot" needs neither.
- **Ignore a store one of whose blocks is bound to other folders** (the first version). One
  bound block at another checkout threw away every portable one; bound blocks are keyed by their
  folders, so they only miss. Replaced by decision 8.
- **Trust what `baked.json` says when it is consistent** (the first version). A review showed a
  laptop's store edited to say `trusted` was reused unasked in CI, and dependents built on it
  went up to the trusted scope. Consistency checks stay as cheap refusals; trust is evidence.
- **Learn "replaced" from the run after** (the first version). An empty CI or sandbox store, the
  main use, would never have seen the mark before reusing; the check now happens as the run
  begins, bounded by the same 5 seconds a CI job waits for `open`.
- **Compare hushrun's own build triple.** Released Linux binaries are musl and `cargo install`
  builds GNU; the compiler's triple in a tag's words reads one store for both.
- **A key in `build.json` naming the lower store.** Older hushruns refuse a field they don't
  know, which turns reuse off for them; an image sets a variable as easily as a file.

## Consequences

- A container built `FROM` an image with the baked folder, with `HUSHRUN_BUILD_LOWER` set, reuses
  what CI (or the person) compiled with no download at boot; the measurement below is one such
  container.
- Every run of a check with build reuse on now writes one small file at its end
  (`manifests/<digest>.json`), swept after `keep_days`.
- An older hushrun on the same machine reads a block opened from a baked store as a download
  whose server's word is stale (decision 13).
- A store read with the server's word costs a run, as it begins, a wait for `open` and one lookup
  per batch of packs the backend's limits allow, at most 5 seconds; a backend that doesn't answer in time leaves the store
  untrusted for that run (it compiles), never a hang.
- A Go module's every run now starts Go's cache program and keeps its portable archives in the
  store, shared or not; Go's own cache still serves first.
- A store baked from a backend names the digests of the baking machine's folders for each part,
  since the rows don't say which blocks are bound; a run at other folders gets one line and
  misses those blocks.
- A small sandbox disk under the free-space floor (B-22) can read the store and use nothing in
  it: opening a pack writes into the upper.

## Measurement

ripgrep 14.1.1 in Linux containers (Docker Desktop on an Apple M-series Mac: 10 CPUs and 8 GB
for Docker, `linux-arm64`, Rust 1.94.0), `hushrun run build -- cargo build --offline` with this
branch's CLI, `HUSHRUN_BACKEND=` (no backend), `HUSHRUN_BUILD=on`, cargo's defaults (debug info,
incremental for workspace members, every CPU). Every case builds the same tree with the same
one-file change, a line added at the top of `crates/searcher/src/lib.rs` (so `grep-searcher`,
`grep-printer`, `grep` and `rg` compile again), at the same paths (`/work/ripgrep`, the cargo home
`/cargo`, the store under `/home/runner`), each in a fresh container from a plain image:

- **a, warm laptop**: a full build of the unchanged tree first, then the change: the target
  folder and the store both warm.
- **a2, warm store**: the same, with `target/` removed before the change.
- **b, baked**: an image that only adds the folder `hushrun build bake --check build` wrote from
  a's store after its first build (38 packs, 34 MB, root-owned and not writable by the build's
  user), with `HUSHRUN_BUILD_LOWER` and `HUSHRUN_BUILD_LOWER_UNTRUSTED=1`; empty store, empty
  `target/`. **b2**: the same folder as a read-only bind mount (`:ro`) instead of a layer.
  **b3**: b with `HUSHRUN_BUILD_AUDIT=none`.
- **c, cold**: empty store, empty `target/`, through hushrun; **c0**: plain `cargo build`.

Eight rounds, the cases interleaved, the host loaded by other work (load average 8 to 14 on 10
cores), so medians:

| Case | wall, median (min–max) | CPU (user+sys), median | crates reused / built |
|---|---:|---:|---|
| a, warm laptop | 1.69 s (1.52–1.96) | 2.94 s | 0 / 4 |
| a2, warm store, empty target | 1.62 s (1.38–2.74) | 2.97 s | 33–35 / 4–6 |
| **b, baked, image layer** | **2.39 s (2.09–4.75)** | 4.73 s | 30–33 / 6–9 |
| b2, baked, read-only bind mount | 3.13 s (2.18–3.81) | 6.36 s | 28–33 / 6–11 |
| b3, baked, audit off | 2.20 s (1.87–3.63) | 4.06 s | 35 / 4 |
| c, cold through hushrun | 3.70 s (3.19–5.78) | 14.84 s | 0 / 39 |
| c0, cold, plain cargo | 3.89 s (3.78–7.26) | 16.23 s | — |

A baked store took a one-file change from 3.7 s cold to 2.4 s (1.4 times the warm laptop's
1.7 s), and its CPU from 14.8 s to 4.7 s. What b adds over a2 is opening 30-odd packs into the
store and the first-reuse audit: with the audit off (b3) it reuses exactly what the warm store
does, and costs 0.6 s of wall time and 1.1 s of CPU more, the packs' zstd, cipher and copy. The
audit (one first reuse of a download in ten, B-34) compiled 2 to 5 crates more per run, which
on a project whose crates compile in under a second each is most of the difference. The bind
mount (b2) goes through Docker Desktop's file sharing from macOS and is not what a Linux host's
read-only mount costs. ripgrep is small: its cold build is under 4 s on this machine, so these
are ratios, not savings anyone waits for; a large workspace is still to be measured.

### Go and C and C++

The same method, a newer build of this branch (release, Linux arm64), Go 1.25.14 and GCC 12.2
with CMake 3.25.1 and Ninja 1.11.1 in one image, every case in a fresh container at the same
paths, no backend, the cases interleaved and their order rotated each round, the host loaded by
other work (load average shown below), so medians. **a** builds the unchanged tree first (so Go's
own cache, or the build folder, and the store are warm) and then the change; **b** reads the
folder `hushrun build bake --check build` wrote from a's store after its first build, as an image
layer, root-owned and read-only, with `HUSHRUN_BUILD_LOWER_UNTRUSTED=1`, and starts with an empty
store, Go cache and build folder; **b3** is b with `HUSHRUN_BUILD_AUDIT=none`; **c** is cold
through hushrun, **c0** cold without it.

**caddy 2.10.2** (`go build -o /tmp/caddy ./cmd/caddy`, `CGO_ENABLED=0`, the module cache in the
image): 870 packages; a line added at the top of `caddy.go`, which most packages import, so 40
packages compile again. The bake held 831 packages (125 MB); the 39 left are assembly outside the
standard library, which stays on the machine (B-42). Eight rounds, host load 8 to 14:

| Case | wall, median (min–max) | CPU (user+sys), median | packages reused / built |
|---|---:|---:|---|
| a, warm laptop | 4.26 s (2.87–9.23) | 13.0 s | 0 / 40 |
| **b, baked, image layer** | **7.38 s (5.84–11.12)** | 25.3 s | 703–728 / 142–167 |
| b3, baked, audit off | 7.24 s (5.04–9.34) | 17.0 s | 791 / 79 |
| c, cold through hushrun | 18.14 s (14.12–23.23) | 125.6 s | 0 / 870 |
| c0, cold, plain `go` | 17.34 s (11.48–20.28) | 127.1 s | — |

**abseil-cpp 20250512.1 with its tests** (CMake, Ninja, Release, C++17, googletest 1.17.0 inside
the checkout, `-j4`: more jobs ran out of Docker's 8 GB on the test units): 395 objects; a line
added at the top of `absl/strings/str_cat.cc`, so one compiles again. The bake held 393 objects
(18 MB) and 393 hints. Five rounds, host load 5 to 15:

| Case | wall, median (min–max) | CPU (user+sys), median | objects reused / built |
|---|---:|---:|---|
| a, warm laptop | 2.76 s (2.47–3.39) | 8.8 s | 0 / 1 |
| **b, baked, image layer** | **58.16 s (27.10–62.50)** | 109.4 s | 344–359 / 36–51 |
| b3, baked, audit off | 5.90 s (5.08–8.61) | 18.8 s | 393 / 2 |
| c, cold through hushrun | 282.36 s (226.84–303.05) | 1117.9 s | 1 / 394 |
| c0, cold, plain CMake | 279.14 s (239.92–386.11) | 1110.6 s | — |

With the audit off, a baked store takes both changes close to the warm laptop: caddy from 18.1 s
cold to 7.2 s (warm 4.3 s), the 79 compiles being the 40 packages the change reaches and the 39
that stay on the machine; abseil from 282 s cold to 5.9 s (warm 2.8 s), 48 times faster, with a
sixtieth of the CPU. The audit is most of what b adds: a baked block's first reuse here is
audited as a download's (one in ten, B-34), and a sandbox started from an image reuses every
block for the first time, every time. On abseil it compiled 34 to 49 of the heaviest test units
again in each run (none differed), 58 s; on caddy about 80 packages, 0.1 s of wall and 8 s of
CPU. Reading through hushrun cost nothing measurable cold (c against c0).

## Amendment: what two reviews found, and the owner's decisions

Two independent reviews of this decision's code, and decisions the owner took after them, change
it as follows (B-63, B-64, B-16, B-39):

- **A run opens exactly what it verified.** A store that named one pack twice let the server's
  check see the first row and the wrapper open the second, with the server's word on it (proved
  by a test). The reader now refuses a pack named twice; every piece of evidence names the SHA-256
  of the `baked.json` bytes it was given, and the state reads the file once; the server vouches
  for each row's outputs, pack SHA-256 and size (not its class, cost or link environment, which the key decides nothing by) in the repository's own trusted scope, the
  `protected/…` scope `open` names (the scope encodes the repository's id, so a name prefix said
  nothing); and the rows a run may open go once into a sorted index in its session folder, which
  every wrapper reads instead of `baked.json` (also sparing a large C++ store a parse per miss). A
  block opened from a store records it by that digest, so a block a person accepted unverified can
  never pass as one a later signature covers. Swapping rows on a writable folder after the check
  opens nothing of theirs.
- **Evidence is per block.** A lookup that fails, or the 5 seconds running out, keeps the
  evidence already obtained for other blocks; no answer from `open` is still none. A backend that
  vouches per manifest would make the check one request; that is the server's to add.
- **Folder digests are gone** (decision 3). They only printed notes, put up to five false ones in
  any sandbox whose home differs from CI's, and a home folder's digest gave its user name away to
  a dictionary of five names. No part binds anything; a bound block is a miss where its folders
  differ, said or not.
- **The say-so is decided on the run's authority**, not on CI detection alone: a run with a relay
  whose server lets it write the repository's trusted scope, or hasn't said, accepts no say-so, in
  or out of CI (`HUSHRUN_PROVIDER=local` changes nothing).
- **Every check, by default** (owner's decision). A bake holds every check's saved manifest for
  the repository and platform (from a backend, every check hushrun.yaml names), each pack once;
  the store's identity covers them all, it is replaced when any check's manifest is, and `--check`
  only narrows. So init's job needs no check. A backend has no list of a repository's checks; a
  check only CI's commands name is reached through the repository's newest build, or by name.
- **No first-reuse audit for a verified block** (owner's decision: "skip the first-reuse audit
  when trust is verified"). A block opened from a current (not replaced) store whose evidence is
  this run's, the server's word for its row (outputs, pack SHA-256 and size; class, cost and link
  environment aren't vouched) in the trusted scope given as the run began, or a signature by a
  signer the repository names that passes `not_before` where the run has no backend to ask, is
  never audited at its first reuse here; its later reuses are one in fifty, as any. "Verified"
  is scoped so (`Evidence::current`, after review 5): a signature is a statement made at baking
  time, and the backend is what learns afterwards that a block was withdrawn or the store
  replaced. Where a backend is configured but timed out, or isn't the store's source (a store baked
  from a laptop and signed by a key, or another org's), the run can't hear a withdrawal, so a
  signed store there keeps the one in ten; where no backend exists, nothing newer can be known
  and the signature is the whole story. A replaced store keeps it too. The cost accepted with the
  owner's choice: sandboxes started from an image become most of the reuse of trusted blocks and
  send no audit reports for those first reuses, so B-16's feed for protected blocks comes from
  later reuses (one in fifty), from laptops and from CI; `HUSHRUN_BUILD_AUDIT=every` restores it
  anywhere, and overrides the exemption. A first build of this change sampled those first reuses one in fifty instead; the owner
  chose none. The one in ten took abseil from 5.9 s to 58 s on every sandbox start and found
  nothing; those blocks are what CI on the default branch built, and the server or the signer
  vouches for exactly their bytes. A store the person accepted unverified keeps the one in ten,
  and `HUSHRUN_BUILD_AUDIT=every` audits every first reuse still. One function decides
  (`Current::reuse_pick`, applied by `Current::audits_reuse`), which the wrapper, the Go cache
  program and the launcher all call. What is given up: a verified block built with an input
  its key misses is no longer caught on a sandbox's first build; it is caught where the block is
  reused again (one in fifty) or where it was built.
- **Go's cache program only where a run shares or reads a baked store** (owner's decision,
  replacing decision 10). A Go user who does neither sees 0.18.0 exactly. A bake from a laptop's
  store therefore has Go packages only from runs that shared or read a baked store, and says so;
  from a backend it has the shared ones. The program is asked for once per run.
- **status and bake say "server decides"** for a backend's store, rather than "untrusted", when
  they ask no backend: the same function, told nobody asked.
- **A partial bake exits 11** when others start from it (an image, `--from backend --trusted`),
  and 0 with a line for a store for this machine, where swept blocks make it routine; 7 stays a
  registry's.
- **One bake of a folder at a time**, by a lock beside it held for the whole bake, replaces the
  process ids (decision 11): containers sharing a volume see other numbers, and one could remove
  another's live folder. Under the lock every leftover is a dead bake's. The folder is synced to
  the disk before it takes `--out`'s name.
- **Saved manifests** keep no folder digest, share one slot between a GNU and a musl hushrun (the
  platform in a tag's words), set an unreadable file aside and say so instead of starting over,
  wait 30 seconds at most for the slot's lock, and drop a pair whose block is unused for the kept
  days (15 by default, B-14): the sweep would remove the block then anyway, and a crate the build
  no longer uses leaves the next bake.
- **A short cargo home no longer keeps registry crates home** (B-10). With `CARGO_HOME=/cargo`,
  the output search found `/cargo` inside the stand-in `/hushrun/cargo`, so 35 of ripgrep's 38
  crates were "cargo-home-in-output". Week 1's bake didn't leave non-portable blocks out, so its
  measurement with `/cargo` baked 38; 0.18.0 had the same false positive and kept those crates off
  the shared store. A folder's text found only inside a stand-in is now the stand-in.

## Amendment: what the first real bake found (ADR 0103)

[ADR 0103](0103-what-the-first-real-bake-found.md): a manifest's identity is its distinct pairs
sorted (`hushrun.block-manifest/v2`), never the order cargo's parallel build ended its compiles in,
and `baked.json`'s rows are sorted, so the same build bakes the same identity and bytes; a linked
output's hint names rustc's temporary folder `rustcXXXXXX`, as E does; and a `--from store` bake
in a CI job with no backend, whose runs kept nothing, says so and what works there.

## Open decisions

- The variable names `HUSHRUN_BUILD_LOWER` and `HUSHRUN_BUILD_LOWER_UNTRUSTED`, the command name
  `bake`, and the words `trusted`/`untrusted` in `baked.json` are public once released.
- Whether a replaced store should rather be ignored outright.
- Whether a backend should vouch per manifest rather than per block, and list a repository's
  checks (both server-side).
- Whether a laptop's bake should keep Go packages without a baked store (a mark set by its first
  bake would let runs keep them from then on).
- Whether the first-reuse audit should also spare a plain download from the trusted scope.
- A bucket as a source, and the backend saying in its rows which blocks are bound.
