# 0099. Baked images: a store as a layered, signed OCI image, and its tags

- **Status:** proposed; built for every kind a baked store carries
- **Date:** 2026-11-21
- **Rules:** B-65 to B-70; B-63 and B-64 changed; A-2, B-28, B-29, B-31, B-48, C-15, E-10, T-12
  read

## Context

[ADR 0098](0098-baked-stores.md) gave a sandbox a folder of compiled code, `hushrun build bake
--out DIR`, and a rule for how long a run trusts it. What starts sandboxes reads images: a
Dockerfile's `COPY --from`, a provider's image build, a program that watches a tag. Four things
stood between the folder and them:

1. **An image.** `docker build` of the folder works, but it needs a daemon in CI, writes a new
   layer of every byte at each merge, and gives a digest that depends on the build's clock and
   compressor. A merge changes a few crates; their image should change by a few MB.
2. **A signal.** Something has to say which image is newest, so that a consumer refreshes its
   snapshot when, and only when, there is something new.
3. **Trust without a backend.** ADR 0098 trusts a store only on the backend's word or, outside
   CI, the person's. A sandbox started from an image may reach no backend, and the person isn't
   there. A signature over what the run reads, by an identity the repository names, is evidence
   it can check offline (ADR 0098's open decision).
4. **A job.** Someone must bake after each merge; `hushrun init` writes CI's files.

## Decision

1. **The image holds the store at `/hushrun`, and a run reads a store there by default.** With
   `HUSHRUN_BUILD_LOWER` unset and `/hushrun/baked.json` there, `/hushrun` is the baked store
   (set and empty turns it off; set to a folder names that one). So one line is the whole of a
   consumer's change, `COPY --from=<repository>:hushrun-<platform>-latest /hushrun /hushrun`,
   with no `ENV`, whatever user the build runs as. Not under a cache folder: the cache
   directory's `build/` is the run's own store, which a baked store must never be under (B-63's
   invalid state), and a home folder depends on the user.
2. **Built by hushrun, as `crane` and `ko` build images.** The config names only the OS, the CPU,
   the layers' digests and one label; no `created`, no history. Layers are **uncompressed tars**
   whose bytes are a pure function of their files and the files' bytes: sorted entries, every
   folder above a file, fixed modes, owners and times, ustar with no extended headers
   (`hushrun_core::blocks::image`, in the crate a backend builds with, documented byte by byte in
   B-65). Packs are compressed and sealed already, so compression would buy nothing and would tie
   a digest to a compressor's version; and a backend can generate the same layer on the fly from
   its object store, file by file, and get the same digest the CLI pushed.
3. **Three layers by how often their blocks change, never one per block.** *deps*: third-party
   crates, placed by Cargo.lock (a package named only with a `source`), so the layer is a pure
   function of the lockfile, the toolchain and the flags; *workspace*: the repository's own code
   as of the last compaction, reused by digest, keeping blocks a later bake no longer uses (named
   in the image's `baked.json` so B-63's "every pack is named" holds); *recent*: the rest of the
   repository's own code and `baked.json`. A code change rewrites only *recent*; a lockfile change
   also *deps*. *Workspace* is written again (compacted) when there is no previous image, when
   asked, when it is missing at the destination, when a path it holds has other bytes, when half
   its bytes are blocks no longer used, or when *recent* would hold more than 16 MiB and more than
   half of it; then the image is *deps* and *workspace* alone, the fallback the plan names. One
   function decides it (`hushrun_artifact::image::plan`); at most three layers.
4. **The previous image is read from the registry, not a local file.** The moving tag's manifest
   and config, whose label (`dev.hushrun.baked-image`) lists each layer's files and the rows of the
   workspace layer's packs: what a CI job, which starts with nothing, needs to keep a layer and
   name what it holds. In an image layout, the index's entry of that tag. A blob is uploaded only
   when a HEAD says the registry lacks it.
5. **The fixed tag first, the moving tag last.** `hushrun-<platform>-<16 hex of the manifest>`,
   then the signature, then `hushrun-<platform>-latest`: a moving tag never points at an image
   whose signature isn't there. Tags are a contract (B-66), with the branch form defined now. A
   store with several parts has one tag: its manifest is the digest of all of its parts'.
6. **Two signatures, one signer.** The store's, `baked.sigstore.json` in the folder: an in-toto
   statement whose subject is `baked.json` by digest (which names every pack's SHA-256) and
   whose predicate lists the hint files (each named for its bytes' SHA-256), so it covers every
   byte a run reads and stays valid in a folder copied out of the image (it can't name the
   image's digest, being inside it). The image's, a cosign-style referrer: a Sigstore bundle as an
   OCI artifact whose subject is the image, under the referrers API or, for a registry without
   one (registry:2), the tag schema's `sha256-<hex>` index; `cosign verify-attestation` checks it.
   Keylessly with a CI job's ID token (Fulcio and an RFC 3161 timestamp, the existing code), or
   with `--signing-key`; never a person's machine key or a browser sign-in. The envelope's
   signature names no key id, since cosign skips a signature whose key id isn't its own form of
   the key's; the bundle names the key.
7. **A signature is evidence when hushrun.yaml names its signer** (`build: baked: signers:`:
   a keyless certificate's issuer and subject compared exactly, or a key). It is checked as the
   run begins, offline, with the binary's Sigstore trust root (or `HUSHRUN_SIGSTORE_TRUSTED_ROOT`);
   then every block is reused without asking, in CI too, since whoever can make the signer sign
   could write the repository's trusted blocks. While the run reads the store, `baked.json` is
   read only while its bytes are those signed and a hint only when listed and its bytes are its
   name, so a folder changed after the check gets nothing it changed reused.
8. **The bake job reads the backend and runs after the build.** In CI a run shares blocks only
   through a backend (B-31), so a job can't bake its own store; `--from backend --trusted` bakes
   what CI on the default branch wrote. On GitHub a workflow of its own (no edit to a file init
   didn't write), `workflow_run` of the build's workflow on the default branch plus
   `workflow_dispatch`, one `concurrency` group, `packages: write` and `id-token: write`, pushing
   to GHCR with `GITHUB_TOKEN`; on GitLab a `.post` job with the two ID tokens and a
   `resource_group`, pushing to the project's registry. `--bake-now` isn't needed: the merge of
   init's change is a push to the default branch, so the first image is baked right after it.
   `--bake` is opt-in.
9. **`hushrun build manifest --json`** prints what a bake would publish now: the store's manifest,
   its parts and the tags, with a versioned schema (B-68), for a watcher that would rather ask
   than poll a registry.

## Alternatives considered

- **`docker build` or BuildKit.** Needs a daemon, compresses with the daemon's gzip, and stamps
  times; incremental only through its own cache. Rejected for the reasons above.
- **gzip or zstd layers.** Smaller only by the hints and `baked.json`; the digest would depend
  on the compressor, and a backend would have to compress the same way to serve the same digest.
- **One layer per block.** Registries cap layers near 100, and pulls slow down with many.
- **A deps/workspace split only, rewritten each bake.** A code change would rewrite the
  workspace layer. Keeping unused blocks in it until a compaction is what lets a merge upload
  only *recent*; compaction bounds what they cost.
- **Leave the image's store where the plan's sketch put it (`/root/.cache/hushrun`).** That is the
  cache directory, whose `build/` is the run's own store: a baked store there would be written,
  swept and refused. And it ties the image to root.
- **A local state file of the previous image.** A CI job starts empty; the registry has it.
- **Sign only the image's digest.** A run reads a folder, not an image, and can't learn the
  digest of the image it came from; the store's statement covers the bytes the run reads.
- **Classic cosign `.sig`/`.att` tags.** Older and still read by older tools, but they carry a
  timestamp in an annotation and no bundle; the new bundle format is what cosign 3 writes and
  reads, and what hushrun's signing already makes.
- **Trust a signature by any identity of the repository's CI** (the server's `ci` signer, T-15).
  Offline there is no server to name it; hushrun.yaml is the repository's own word, and an exact
  subject names one workflow on one branch.
- **Bake inside the build job, from the job's store.** In CI a run has no store without a backend
  (B-31), and a job's own store would mix in whatever it reused.

## Consequences

- An image is published in a few seconds after a build, and a code change uploads one layer and
  a config, a few hundred KB to a few MB. A lockfile change uploads *deps* again.
- The image's `baked.json` may name packs the newest manifest doesn't (kept in *workspace*): a run
  may open one only when a compile computes its key, so they cost space, never correctness.
- A store whose signer hushrun.yaml names is trusted in CI without a backend; on a pull request
  the hushrun.yaml is the change's own, whose blocks reach no trusted scope (B-29).
- Every run with a store named and signers configured reads `baked.sigstore.json` and lists the
  store's hints as it begins: one signature check and one folder listing.
- Bakes of one repository and platform must not run at once: the later moving tag wins. The job
  init writes takes one group.

## Measurement

ripgrep 14.1.1 in Linux containers (Docker Desktop on an Apple M-series Mac, 10 CPUs and 8 GB,
`linux-arm64`, Rust 1.94.0), a release build of this branch, `registry:2` on a Docker network, no
backend.

**Uploads.** A "CI" container built ripgrep through hushrun (`CARGO_INCREMENTAL=0`, so that the
workspace's crates are blocks), baked with `--oci` and a key, then made a code-only change (a line
atop `crates/printer/src/lib.rs`, 3 crates compiled again) and baked, then a lockfile change
(`cargo update -p memchr`, 2.7.4 to 2.8.3, 14 crates compiled again) and baked. What each bake
uploaded, from its JSON and confirmed by the registry's log (13 blob uploads in all, 6, 3 and 4):

| Bake | deps | workspace | recent | config + signature | uploaded |
|---|---:|---:|---:|---:|---:|
| first (38 blocks, 34.8 MB) | 29,443,072 | 5,430,784 | 28,160 | 23,008 | 34,925,024 B |
| code-only change | kept (HEAD) | kept | **589,312** | 23,408 | **612,720 B** |
| lockfile change | **40,413,696** | kept | 5,027,328 | 29,404 | 45,470,428 B |

The deps layer grew on the lockfile change because the store's saved manifest keeps a crate's old
pairs while the store holds them (ADR 0098's open decision), so the old memchr dependents went in
too.

**A plain image.** `FROM` the build image (rust:1.94-bookworm, ripgrep's source and crates) plus
`COPY --from=localhost:5000/ripgrep:hushrun-linux-arm64-<first bake> /hushrun /hushrun`: the run
found `/hushrun` with no variable, and read it as current, signed by the key hushrun.yaml names.
The same one-file change ADR 0098 measured (a line atop `crates/searcher/src/lib.rs`), each case in
a fresh container, eight rounds, the cases interleaved and their order rotated, host load 3.8 to
8.2, cargo's defaults (incremental for workspace members):

| Case | wall, median (min–max) | CPU, median | crates reused / built |
|---|---:|---:|---|
| a, warm laptop | 1.77 s (1.69–2.39) | 3.19 s | 0 / 4 |
| **b, image with the one line, signed** | **2.80 s (2.46–4.37)** | 6.56 s | 23–29 / 10–16 |
| b3, the same, audit off | 2.58 s (2.41–2.78) | 5.86 s | 29 / 10 |
| c, cold through hushrun | 4.03 s (3.79–4.52) | 15.74 s | 0 / 39 |

1.6 times the warm laptop (1.5 with the audit off), against 1.4 in ADR 0098's image built by
`docker build` with the person's say-so: the store is the same, read on a signature instead.
The ten compiles left with the audit off are the workspace's crates, which cargo builds
incrementally in the dev profile and hushrun passes through (B-4): a warm laptop compiles only the
four the change reaches, a fresh container all of them. Five more rounds with
`CARGO_INCREMENTAL=0` in the container gave 2.45 s against 1.79 s (12 to 13 compiled), so that is
not all of it; why more than the four compile there is open. The image grew by 70 MB as Docker
counts it (the 35 MB store, stored packed and unpacked); the registry holds 34.9 MB of layers.

**Signatures.** cosign 3.0.2, in a container on an internal Docker network with no way out (its
TUF update failed, as it should), with the binary's public-good trust root as `--trusted-root`
and `--insecure-ignore-tlog`: `verify-attestation --key` verified the image's attestation with the
signing key and refused another key; `verify-blob-attestation` verified `baked.json` against
`baked.sigstore.json` and refused a changed file. registry:2 had to be served over TLS for it:
cosign 3.0.2 doesn't carry `--allow-http-registry` to its fetch of the bundle and falls back to
the classic `.att` tag. In the image, a `baked.json` changed after signing was no evidence ("it
was made for another baked.json"), the store was ignored and 39 crates compiled; one byte changed
in a pack left the store trusted and that pack refused by its SHA-256 (37 of 39 reused against
38 unchanged, `CARGO_INCREMENTAL=0`). Keyless signing and its check ran against hushrun's fake
Fulcio and timestamp authority in the tests; nothing contacted Fulcio, Rekor or a TUF mirror.

## Amendment: what a review found, and the owner's decisions

A review of this decision's code (with proof tests) and the owner's decisions after it change it as
follows (B-65 to B-69):

- **A bake builds only on an image it can vouch for** (decisions 3 and 4). A tampered moving tag's
  image got the attacker's rows into the next bake's `baked.json`, which the bake then signed, and
  its bytes into the image through a workspace layer reused by digest and never read (proved). Of
  the three ways out (drop carrying, re-download and re-hash the reused layer, or verify), we chose
  to verify: the previous image counts only when an image signature by a signer hushrun.yaml names
  covers its manifest (so its label and layer digests are a trusted bake's, which checked its own
  previous image the same way), and a row is carried only as the backend vouches for it now,
  whole, in the repository's trusted scope, so a withdrawn block is never carried. A store baked
  from a machine's store carries nothing (no source vouches for a block it no longer names) and
  compacts instead. Dropping carrying altogether would have rewritten the workspace layer on most
  merges, which is what the layering is for; re-hashing would download the workspace layer on
  every bake and still leave the label's rows unproved. Any failure reading the previous image is
  no previous image, said, and the bake compacts.
- **The moving tag moves only from CI on the default branch** (decision 5): the CI provider's
  branch is the repository's default branch, and the run isn't a pull request's. A laptop, another
  branch or a pull request publishes the fixed tag, says why, and moves a branch's tag when asked
  (`--branch-tag`), which is also what such a bake builds on. The job runs by hand only on the
  default branch, after the build only for a push of this repository (a fork's pull request named
  like the default branch triggered it), and GitLab's web runs only on the default branch. Whether
  the server's `open` restricts a bake's reads to the default branch's blocks (the bake sends no
  commit) is for the server's own work.
- **A keyless signature counts only from an allowed trigger**: a push, a schedule, a run by hand
  (`workflow_dispatch`, GitLab's `web`), or `workflow_run`, the bake job's own, whose condition in
  the default branch's file admits only a push. The owner named the first three; the job init
  writes needs the fourth, and the subject already pins the workflow file and its branch. An issue
  comment, `pull_request_target` or another workflow's call runs the named file with others' inputs.
- **The fixed tag never moves.** A complete store whose fixed tag names an image a signer
  hushrun.yaml names signed is published already: nothing is uploaded and the moving tags go to
  that image. A partial store takes no fixed tag (exit 11, ADR 0098's amendment), since its tag
  would otherwise name a store missing blocks forever. `baked_ms` stays in `baked.json`: with the
  fixed tag written once, a second bake's other clock moves nothing.
- **Rollback**: `build: baked: not_before` (a time) makes a signed store baked earlier no
  evidence; where a backend answers, its word comes first, so a replaced store or a withdrawn
  block is what the server says, signed or not. With no backend an old signed store can be
  replayed until `not_before` is raised past it: B-63's and B-67's "breaks if".
- **What hushrun.yaml trusts is in every check's key**: `build: baked:` (signers, `not_before`) is
  part of every check's definition and every run's key when it names anything, so a pull request
  that adds a signer has results of its own, which its base never reuses.
- **`/hushrun` is read only when root or the run's user owns it**, neither it nor its `baked.json`
  is a link, and nobody else may write them; otherwise ignored with one line.
- **The recent layer**, uploaded again by every bake, is folded in past 64 MiB, or past 1 MiB and a
  quarter of the workspace layer (was 16 MiB and half). A hint the workspace layer keeps unused no
  longer forces compaction: only packs carry rows.
- **Contract fixes**: branch tags never collide (case is a change; a cut form ends with `--` and
  16 hex, which no kept name holds), a layer refuses a file that is a folder too wherever it sorts,
  the long-path split is GNU's first slash that fits (as the code did; the text said last), only a
  404 or 405 means "no referrers API", a blob check's error is an error, and the `it` registry
  tests fail rather than pass when no registry is named.
- **The vendored TUF root** (`hushrun-attest/sigstore/root.json`, version 15) expired on
  2026-11-20. Nothing that verifies a signature reads it: a run checks certificates against the
  vendored `trusted_root.json`, whose current authority, timestamp authority and logs have no end
  date, so keyless verification works. It is the starting point of `hushrun sigstore refresh`,
  which TUF lets start from an expired root as long as the mirror serves a newer one; with no newer
  root there the refresh fails. A release should vendor the mirror's current `root.json`,
  `targets.json` and `trusted_root.json` (a network step this work didn't take). The steps, on a
  machine with the network, in one reviewed change:
  1. `HUSHRUN_CACHE_DIR=$(mktemp -d) hushrun sigstore refresh --format json`, and note its
     `rootVersion`, `targetsVersion` and `trustedRoot` (the trusted root's SHA-256).
  2. In `$HUSHRUN_CACHE_DIR/sigstore/tuf-repo-cdn.sigstore.dev/`, the file `current` names the set
     just written; from that set copy `targets.json` and `trusted_root.json` over
     `crates/hushrun-attest/sigstore/targets.json` and `trusted_root.json`, and the highest
     `root/<N>.json` over `crates/hushrun-attest/sigstore/root.json`. (Or fetch the same three
     from `https://tuf-repo-cdn.sigstore.dev/`: `<N>.root.json` for the newest N, then
     `timestamp.json` to `snapshot.json` to `<V>.targets.json`, then
     `targets/<sha256>.trusted_root.json` as `targets.json` names it.)
  3. Write the new versions and the trusted root's SHA-256 into
     `crates/hushrun-attest/sigstore/README.md`.
  4. `cargo test -p hushrun-attest` (a test checks that `root.json` signs `targets.json` and that
     `targets.json` pins `trusted_root.json`), then the workspace's tests: a run's keyless checks
     read `trusted_root.json`.

  Done on 2026-11-22: root version 16 (expiring on 2027-04-22) is vendored, and Sigstore's
  `targets.json` (version 14) and `trusted_root.json` hadn't changed. The steps now live in
  `crates/hushrun-attest/sigstore/README.md`, with a test that the root vendored before signed
  the new one.

## Amendment: publishing into a backend's registry (B-70)

A backend can serve a read-only registry built from the packs it already holds (its pull API is
the distribution spec's; layers are made on the fly with B-65's function). Nobody pushes blobs
there, so a bake publishes by describing the image in one request, `POST v0/build/images`
(`hushrun_core::blocks::PublishRequest`, shared with the backend).

- **How a bake knows it is the backend's registry.** `--oci` keeps one meaning, "this image's
  repository", and gains two forms: a repository on the registry host of the repository's own
  backend (`registry.hushrun.com` for hushrun's hosted backend, the backend's own host otherwise:
  `hosted::registry_host`, one definition for bake and init), detected from the backend
  hushrun.yaml (or `HUSHRUN_BACKEND`) names; and the word `hushrun`, "the backend's registry,
  whatever its host", for a self-hosted backend that names another host for its images or for a
  person who doesn't want to spell the name. Why not a new flag such as `--publish`: the explicit
  repository is what the Dockerfile line names, so a job's command reads as where the image goes,
  and a second flag would need its own conflicts with `--oci` and `--oci-dir`. Why detect at all
  rather than only the word: `init --bake` writes the repository, and a person copying the
  Dockerfile line's name into `--oci` gets the right behaviour instead of a 405 from a registry
  that takes no blob. `registry.hushrun.com` with another backend is refused (it would fail at the
  server anyway), and the backend's registry needs `--from backend --trusted` (it takes only
  blocks its trusted scope serves) and has no branch tags: all refused before anything is baked.
  The word `hushrun` can't name Docker Hub's `library/hushrun`, which nobody may push to.
- **What a bake sends**: each layer's files bottom first, a pack by its block, any other file by
  its bytes (one a kept layer holds from the previous image by its SHA-256 alone: the backend
  holds it), the manifest, config and signature it made, the moving tag's digest it read, and the
  commit. The previous image is read through the registry's token flow with the relay's own
  credential: in CI the job's ID token, which the backend limits to the job's own repository.
- **Answers decided in one loop** (`Relay::publish_image`). The server computes every layer's
  digest itself; the bake compares them with its own and fails loudly on a difference (and says the
  backend's own 422 `mismatch` as loudly). 503, 409 `duplicate` and no answer send the same bytes
  again: the server treats the same image as the same publish, so a retry finishes what a lost
  answer left. 409 `stale` (the moving tag moved since it was read) reads the previous image again
  and plans again from the image the server names, **once**, then stops with a line: a second
  stale means another bake is publishing right now, and that one will land. The protocol allows
  three tries; one is enough to cover a race with a single other bake and keeps a CI job's time
  bounded. 409 `not_newer` publishes nothing and **exits 0** with a line: a newer bake won, which
  is the moving tag doing its job, and a red CI job there would only be noise (`--format json`
  says `"state": "not_newer"` for a program that cares). No new exit code: 0 is "the image the
  moving tag names is current", which is true.
- **A partial store is not published there** (exit 11 with a line). The backend gives an image of a
  store's manifest identity the fixed tag for good, and a later complete bake of the same manifest
  would then be dropped for the partial one. Against other registries a partial store takes no
  fixed tag (B-66), which the backend can't tell from the request.
- **`init --bake`** writes the hosted registry's repository for a repository on hushrun's hosted
  backend (`https://api.hushrun.com/o/<org>`), with no `packages: write` and no registry token,
  and keeps GHCR or the GitLab project's registry otherwise, and when the backend isn't known yet.
  The GitHub workflow keeps its path, `.github/workflows/hushrun-bake.yml`: the backend checks a
  `workflow_run` publish against that file's identity.
- **The backend pins this crate**: the wire types live in `hushrun_core::blocks::publish`, so the
  server reads requests with the CLI's own definition. The server reads them with
  `deny_unknown_fields` today; a member added later needs the server to relax that first.

Open for the owner: the word `hushrun` for `--oci`; whether a partial store should be refused or
published without moving anything; exit 0 for `not_newer`; one stale retry rather than the
protocol's three; `init --bake` keeping GHCR while the backend is unknown.

## Amendment: the recent layer's bound stays absolute (review 5)

Review 5 asked for `RECENT_MOST` (64 MiB) to be relative to the workspace layer, since a large
repository compacts often. It stays absolute beside the relative rule (more than 1 MiB and a quarter
of the workspace layer). Every bake uploads the recent layer whole again, so a recent layer of size
R costs R on every merge until the next compaction, which costs the workspace layer W once. With
recent growing by d a merge, compacting at a bound C costs about W·d/C + C/2 a merge, least near
C = √(2·W·d): it grows with the square root of W, not with W. A quarter of W (the relative rule
alone) would let a 2 GB workspace re-upload up to 500 MB on every merge before compacting, worse
than compacting at 64 MiB; and a merge that changes more than the bound by itself (a core crate
every workspace crate depends on) uploads nearly W either way. A bound that follows √(W·d) needs d,
which one bake doesn't know; the owner can tune the constant (an open decision below).

## Amendment: the bake signs the folder (ADR 0100)

The folder's signature, `baked.sigstore.json` (decision 6), is now made by the bake once, before
it publishes anything of the store, rather than by the image's publish: an archive or a disk
image of the same store (B-71) carries the same file, and the publish's shortcut for a store
whose fixed tag is published already signed nothing. The bake decides its signer before it bakes.

## Amendment: images over time (ADR 0101)

[ADR 0101](0101-images-over-time.md) adds a weekly compaction (the label's `compacted_ms`), the
re-bake limit (B-73), branch bakes in the job init writes (B-69) and dependencies-only images
(B-72), whose label says `deps_only`.

## Open decisions

- `--bake` is opt-in, and init prints, rather than writes, the `build: baked: signers:` lines.
- The tag words (`hushrun-<platform>-…`, `--` in a cut branch name), the label and annotation
  names, the predicate types (`https://hushrun.com/attestations/…`), `/hushrun` as the default
  path, and the `--bake`, `--branch-tag` and `manifest` names are public once released.
- Compaction's thresholds (half stale; 64 MiB, or 1 MiB and a quarter, for *recent*); the weekly
  compaction is ADR 0101's.
- The deps layer places only Rust crates; Go modules and C/C++ objects, and every block of a store
  baked from a backend (whose rows don't say), count as the repository's own until a row says.
- A dev-profile build of the workspace's crates is incremental and never reused from any store.
- The job bakes `linux-x64` only (GitHub's `ubuntu-latest`); an arm image needs an arm runner and
  blocks built on one.
- Moving tags have no compare-and-set in the registry protocol: one group per job is the guard.
- `workflow_run` in the trigger allowlist, beside the owner's three.
