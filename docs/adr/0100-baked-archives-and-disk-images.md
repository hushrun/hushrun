# 0100. Baked archives and disk images: the store as one tar, and as a read-only EROFS image

- **Status:** proposed; built
- **Date:** 2026-11-22
- **Rules:** B-71; B-63, B-65 and B-67 changed; B-28, B-64 read

## Context

[ADR 0099](0099-baked-images.md) published a baked store as an OCI image, which a Dockerfile
takes with one line. Two other ways of starting a warm sandbox take no image:

1. **A live snapshot.** Some providers snapshot a running sandbox: start one, put files on its
   disk, snapshot, and start later sandboxes from the snapshot. What the step needs is one file
   it can download and unpack, `curl … | tar -x -C /`, with no registry client and no docker.
2. **A shared volume.** Others mount a volume read-only into many sandboxes (a network disk, a
   drive the platform keeps). What it needs is a file system image written once and mounted, with
   no per-sandbox copy.

Both must carry exactly what the image carries, so a run trusts the store on the same evidence
(ADR 0099's signature, the server's word), and both should be the same bytes for the same store,
so whoever keeps them can tell an unchanged store from a new one, and a cache or a CDN keeps one
copy.

## Decision

1. **`--tar FILE` is B-65's layer bytes of the whole store under `hushrun/`.** One function
   already writes a deterministic ustar of a store's files (`hushrun_core::blocks::image::chunks`):
   sorted, every folder once, `0755`/`0644`, owner 0, time 0, no extended header. The archive is
   that function over every file of the folder, so `tar -x -C /` puts the store at `/hushrun`,
   where a run reads it with no variable (ADR 0099), and any tar reads it (GNU, BSD, busybox).
   `-` writes it to stdout for a pipe; what bake says then goes to stderr.
2. **Uncompressed by default; `.zst` when the name asks.** The packs are zstd streams sealed with
   ChaCha20-Poly1305, and the hints are sealed too: ciphertext doesn't compress. What zstd can
   take is the tar headers, the padding to 512 bytes and `baked.json`, which the measurement
   below puts at a fraction of a percent of ripgrep's store. A name ending in `.zst` gets zstd
   level 3 on one thread (the same bytes for the same tar with one zstd version), for whoever
   wants it anyway; the default doesn't pay a compressor's time, or tie the file's digest to a
   compressor's version, for nothing.
3. **`--erofs FILE` is written by hushrun, not `mkfs.erofs`.** EROFS is Linux's read-only file
   system, mountable since 5.4, which is what container runtimes and volume platforms use for
   read-only images. Three ways to make one were weighed:
   - **Shell out to `mkfs.erofs`** (erofs-utils) with deterministic flags (`-T0`, a fixed `-U`,
     `--all-root`, `-x-1`). It needs erofs-utils installed (Debian has it; a laptop or a CI image
     usually doesn't; macOS has no package hushrun can count on), and its output follows its
     version: the layout heuristics changed between 1.5 and 1.8, so "the same store gives the same
     bytes" would hold only on machines with the same erofs-utils. Its reading of the source folder
     follows the folder's own order unless told otherwise.
   - **A Rust crate.** None is in the lockfile. The EROFS writer known to us (composefs-rs's) comes
     inside a crate made for composefs images, with their layout and dependencies, and wasn't
     fetched or checked against `deny.toml` here (no network install for it); the others read.
   - **Write the subset ourselves**, as the tar is written: uncompressed, no extended attributes,
     flat files and folders, which is all a store holds. About 400 lines with no dependency and no
     IO (`hushrun_core::blocks::erofs`), a pure function of the files and their bytes in chunks
     (metadata, file ranges, zeros) that streams the files without holding them, on any OS, with no
     root and no loop device, and the same bytes everywhere by construction.

   We chose the last. Its layout is the one `mkfs.erofs` 1.8 writes for such a folder (compact
   inodes, breadth first, folders' entries sorted, tails inline beside their inodes, `.` and
   `..` in every folder), documented byte by byte in the module, minus the superblock checksum
   (an optional feature; leaving it out keeps the writer free of CRC32C) and with times all 0. It
   was checked against the kernel and the tools that read EROFS (below), and the unit tests read
   every image back with a reader written apart from the writer.
4. **The disk image's root is the store.** Mounted at `/hushrun` it is read with no variable:
   mounted read-only, nobody can write it, so the default folder's owner and group-write checks
   (B-65) don't apply to a folder on a read-only file system (a user-namespaced container, such
   as rootless podman, Kubernetes' `hostUsers: false` or a remapped daemon, sees a root-owned
   mount as owned by nobody, 65534, and no `chown` can change a read-only mount; amended after
   review, see below); mounted
   anywhere else, `HUSHRUN_BUILD_LOWER` names the mount point, which a run reads as B-64 says,
   never writing it. Its UUID is the first 16 bytes of the SHA-256 of `baked.json` (a version 8
   UUID), so it follows the store and is never random.
5. **One signature, made once, before anything is made of the folder.** A bake that publishes
   (an image, an archive, a disk image) now decides its signer before it bakes and signs the
   folder once; the image, the tar and the disk image each carry that `baked.sigstore.json`.
   Before, the image's publish signed the folder itself, and its fixed-tag shortcut (the store is
   published already) signed nothing, which an archive written after it would have carried
   unsigned. Deciding the signer first also stops a `workflow_run` job that may not sign before it
   downloads and bakes anything.
6. **Written whole.** A file goes to a name no other process uses beside it, is synced, then
   renamed: whatever reads `FILE` sees a whole archive or none, and a failure leaves no temporary
   file. Stdout is written as it goes; a consumer of a pipe sees the end of the stream.
7. **Two bakes of the same store give the same bytes when nothing is signed.** The archive and
   the image are pure functions of the folder, but `baked.json` holds `baked_ms`, the time of the
   bake, and a signature holds a timestamp and (for ECDSA) random bytes. A bake that signs nothing
   takes `baked_ms` from `SOURCE_DATE_EPOCH` when it is set, the reproducible-builds convention, so
   two such bakes of one store are byte for byte the same. A signed store keeps the clock's time:
   `build: baked: not_before` compares a signed store's `baked_ms` (B-67), and a CI job that sets
   `SOURCE_DATE_EPOCH` to a commit's time for other reasons would otherwise sign stores that look
   older than they are. The fixed tag (B-66) and `hushrun build manifest` remain the identity a
   watcher should compare; the archive's digest is the identity of its bytes.
8. **Who extracts it doesn't matter; who may write it after does.** A tar extracted by root is
   root's and one extracted by the user who runs builds is theirs, and both pass `/hushrun`'s
   check. The archive's modes are `0644` and `0755`, and a umask only takes bits away (GNU tar
   applies it for a user and keeps the archive's modes for root), so an extract is never group- or
   world-writable; a `/hushrun` made beforehand with a group-writable mode is set to the archive's
   `0755` by GNU tar as its owner extracts over it (measured below). A store someone made writable
   after is refused, as before, with a line that now says the fix (`chmod -R g-w /hushrun`).

## Alternatives considered

- **gzip as the default.** Universal, but it would compress ciphertext for nothing, and gzip's
  output varies with its implementation and level as zstd's does.
- **The OCI image's layers as the archive** (concatenate them, or point a step at the registry's
  blobs). A layer is only part of the store, and a snapshot step would need a registry client
  and the image's manifest to know which blobs to fetch; the tar is one file.
- **squashfs.** Older and mountable almost everywhere, but `mksquashfs`'s output varies with its
  version and compressor, and its format is larger to write by hand (compressed metadata blocks,
  fragment tables). EROFS is what read-only container images and volume platforms moved to, and
  its uncompressed layout is simple.
- **ext4 read-only.** Needs `mkfs.ext4 -d` and its journal and timestamps tamed; far more format
  to write, for no reader that EROFS lacks.
- **`mkfs.erofs --tar=f` from hushrun's tar.** Determinism of input order solved, but still a
  tool to install and a version to match.
- **Store the archive's digest as the change signal.** It changes with every signature; the
  store's manifest (B-63) doesn't.

## Consequences

- A live-snapshot step needs `curl` and `tar` and nothing of hushrun's; a volume needs a kernel
  with EROFS (Linux 5.4 or later, built with it, as Docker Desktop's and the major distributions'
  are) or `fsck.erofs --extract`.
- The archive, the disk image and the image's layers hold the same files; a team keeping all
  three keeps the store three times, as it chose.
- A bake that publishes and can't sign now says so before it bakes, and a `workflow_run` job
  refused a keyless signature stops before it downloads anything.
- `SOURCE_DATE_EPOCH` set in a bake that signs nothing changes `baked.json`'s time to it.

## Measurement

ripgrep 14.1.1 in Linux containers (Docker Desktop on an Apple M-series Mac, `linux-arm64`, Linux
7.0 with EROFS built in, Rust 1.94.0), a release build of this change, no backend. A container
from `rust:1.94.0-bookworm` with ripgrep's source and crates (the same image as ADR 0099's, no
image layer of a store) built ripgrep through hushrun and baked it, signed with a key the
checkout's hushrun.yaml names: `hushrun build bake --check build --tar store.tar --erofs
store.erofs --signing-key key.pem` took 1.05 s for 38 packs and 38 hints.

**Sizes.**

| What | Bytes | Against the files |
|---|---:|---:|
| the store's 78 files (38 packs, 38 hints, `baked.json`, `baked.sigstore.json`) | 34,810,773 | |
| `store.tar` | 34,891,264 | +0.23 % |
| `store.tar.zst` (level 3, hushrun's) | 34,835,355 | +0.07 % (0.16 % under the tar) |
| the tar through `zstd -19` / `gzip -9` (for comparison) | 34,810,311 / 34,815,505 | 0.23 % / 0.22 % under the tar |
| `store.erofs` | 34,869,248 | +0.17 % |

zstd takes 56 KB off 34.9 MB: the tar headers and padding. The packs are ciphertext. Hence an
uncompressed default.

**Reproducibility.** Two bakes of the same store, each unsigned with `SOURCE_DATE_EPOCH` set,
gave the same tar (SHA-256 `547f8f4d…`) and the same disk image (`1afa1e58…`). Two signed bakes
differed only in `baked.json` (its `baked_ms`) and `baked.sigstore.json`; every pack and hint was
the same bytes at the same place. The layouts' pinned digests in the unit tests pass on macOS and
on Linux, so the same files give the same bytes on both.

**The disk image** passed `fsck.erofs` (erofs-utils 1.8.6); `fsck.erofs --extract` and a
read-only mount by the kernel (`mount -t erofs -o ro`, file-backed, in a privileged container)
both gave a tree equal to the baked folder (`diff -r`), every file root's, folders `0755`, files
`0644`. A test image with 800 files (folders of several blocks, tails that fit beside their inode
and tails that don't, an empty file) did the same.

**A fresh container, no image layer.** The same one-file change ADR 0098 and 0099 measured (a line
atop `crates/searcher/src/lib.rs`), each case in a fresh container from the plain image, eight
rounds, the cases interleaved and their order rotated, host load 4.8 to 8.3, cargo's defaults
(incremental for workspace members), the default audit:

| Case | wall, median (min–max) | CPU, median | crates reused / built |
|---|---:|---:|---|
| a, warm laptop | 1.64 s (1.55–1.91) | 2.81 s | 0 / 4 |
| **tr, tar extracted to `/` by root** | **2.15 s (2.00–2.50)** | 3.76 s | 35 / 4 |
| **tu, tar extracted to `/` by the build's user** | **2.12 s (2.00–2.48)** | 3.80 s | 35 / 4 |
| **em, disk image mounted read-only at `/mnt/vol`** | **2.09 s (2.01–2.48)** | 3.79 s | 35 / 4 |
| c, cold through hushrun | 3.67 s (3.55–4.28) | 14.57 s | 0 / 39 |

1.3 times the warm laptop in all three, against 1.6 in ADR 0099's image measurement, which then
audited a signed block's first reuse one time in ten (the owner's decision since: never, where no
backend could say otherwise); the store and the 35 crates it gives are the same. The four
compiled are the ones the change reaches. Extracting the 34.9 MB tar took 0.04 to 0.06 s (from
the page cache). In **tr** `/hushrun` was `root 0755` and `baked.json` `root 0644`; in **tu**
the user extracted with umask 002 into a `/hushrun` made beforehand `0775` and given to it, and
GNU tar 1.35 left both `runner 0755` and `runner 0644`; with umask 077 everything was `0700`/`0600`,
the user's, and read. In **em** the run read the mount with `HUSHRUN_BUILD_LOWER=/mnt/vol`;
mounted at `/hushrun` it needs no variable (root, `0755`). A store made group-writable after
(`chmod g+w /hushrun/packs`) was ignored with "its group may write /hushrun/packs: `chmod -R g-w
/hushrun` fixes it"; one owned by another user and one world-writable said theirs.

## Amendment: a read-only mount is read whoever owns it (review 7)

Decision 4 said a mounted image is read because it is "owned by root and writable by nobody".
In a user-namespaced container root's files show as owned by nobody (65534), so the check refused
the mount, and its advice (`chown -R`) can't apply to a read-only file system. A folder, file or
hint folder of `/hushrun` on a file system mounted read-only (`statvfs`'s `ST_RDONLY`) now skips
the owner and group-write checks (nobody can write it); a link is refused as before, and a
writable folder keeps every check. The line for a folder owned by another user names
`HUSHRUN_BUILD_LOWER=<folder>`, which reads it anyway.

## Open decisions

- Uncompressed tar as the default, and `.zst` chosen by the file's name rather than a flag.
- `SOURCE_DATE_EPOCH` for unsigned bakes only; the clock for signed ones.
- The EROFS writer as hushrun's own code rather than `mkfs.erofs`, and no superblock checksum.
- `--tar -` and `--erofs -` both allowed (one at a time).
- Whether a bake should also publish the archive somewhere (a bucket, the backend), rather than
  leave a file for the team to host: today hushrun writes the file and the team serves it.
