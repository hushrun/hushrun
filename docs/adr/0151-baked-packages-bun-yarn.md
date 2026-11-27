# 0151. Bun and Yarn in the package store

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-20 new, and the `bun` and `yarn` parts' paragraphs under K-10
  (`docs/spec/11-baked-packages.md`); K-6, K-7 and K-10 read

## Context

ADR 0149 made the package store, with Gradle's and Maven's parts read in place. The pnpm and npm
job (ADR 0150) adds the first seed: hushrun copies a part's files into the tool's own cache before
an install. Bun and Yarn are next. The plan said Bun's part would hold tarballs checked against
`bun.lock` if Bun takes a cache folder it didn't write, and Bun's unpacked cache, read on a
signature, if not; that Yarn 4's part would be seeded and Yarn 1's read in place, as an offline
mirror.

## What was confirmed by running the tools

On 2026-11-26, in a Linux container, with no route to any registry (every proxy variable at a
closed port) and, where it matters, as a user who may not write the part:

- **Bun 1.4.4 takes a folder it didn't write.** Its cache keeps each package unpacked as
  `<name>@<version>@@@1/`, plus `<name>/<version>@@@1` links with absolute targets and `*.npm`
  metadata files. A cache made of nothing but those folders, each unpacked by hand from the
  registry's tarball (which is byte for byte what Bun writes), installs `bun install
  --frozen-lockfile` with no request, and leaves `node_modules` as an install from the network
  does, link for link, under both linkers (`hoisted` and `isolated`). Bun never checks a cached
  folder: whatever is there is used.
- **Bun can't read a part in place.** Pointed at a read-only cache that lacks one package the
  lockfile names, Bun fails at once: `cannot write files to cache directory: EACCES`. A stale store
  would fail an install that passes today. As root it writes into the store instead.
- **Yarn 4.12.0**: each zip of its cache is the file whose SHA-512 is `yarn.lock`'s `checksum`
  after the cache key (`10c0/<hex>`). A global folder holding only copied zips (no `index/`, no
  `metadata/`) installs `yarn install --immutable` with no request, and the same `.pnp.cjs` as from
  the network. A zip with one byte changed fails the install ("doesn't match the expected
  checksum"). `yarn config get cacheFolder` names the folder Yarn really uses, global cache
  included. Yarn 4 refuses `YARN_YARN_OFFLINE_MIRROR` outright ("Unrecognized or legacy
  configuration settings").
- **Yarn 3.8.7** writes `__metadata` `version: 6`, whose `checksum` is not the SHA-512 of the
  cached zip.
- **Yarn 1.22.22** reads `YARN_YARN_OFFLINE_MIRROR`, doesn't make the folder, and writes into the
  mirror every tarball it downloads. Each tarball is the file whose SHA-512 is `yarn.lock`'s
  `integrity`. A read-only mirror that lacks one package fails the install (`EACCES … open
  '<mirror>/is-odd-3.0.1.tgz'`). A writable mirror seeded with all but one installs, downloads the
  one into the mirror, and leaves `yarn.lock` as it was; a tarball with one byte changed fails the
  install ("Integrity check failed"). With `node_modules` up to date it downloads nothing at all.

Not confirmed here: Bun 1.2's `lockfileVersion` 1 (the reader takes it from Bun's own docs; a
check that would have installed an older Bun was refused in this session); Bun and Yarn on macOS
and Windows; a registry other than npm's.

## Decision

1. **Bun's part holds tarballs, verified by `bun.lock`.** Bun takes a folder it didn't write, so
   the plan's preferred design holds: the part is `tarballs/<name>@<version>.tgz`, each row's
   `locked` the lockfile's `integrity`, and an unsigned store is read.
2. **A bake downloads Bun's tarballs by the lockfile, with no tool run (K-20).** The plan had the
   bake run `bun install --ignore-scripts` with Bun's cache at a folder of the bake's own, but that
   cache never holds a tarball, so nothing a bake could take from it is what `bun.lock` hashes.
   hushrun reads `bun.lock` and downloads each package from npm's registry instead, eight at a
   time, with no credential, each checked against its integrity before it is written. No script
   can run, so the plan's canary (a trusted dependency's install script) can't fire. A package from
   another registry is left out and counted: a private registry's credential isn't hushrun's to
   send, and Bun names such a package's cache folder by its host.
3. **The seed unpacks (K-20).** For each tarball the checkout's `bun.lock` names now, by name,
   version and SHA-512, the seed unpacks it beside its place in Bun's cache and renames it to
   `<name>@<version>@@@1/`. It copies, never links, as the plan asked. The ustar reader takes
   regular files and folders only, and refuses a link, a device or a path that climbs out.
   Unpacking needs gzip: `flate2`, over the `zlib-rs` the binary already builds, is the one new
   crate.
4. **Yarn's part is Yarn's own files, verified by `yarn.lock`.** Yarn 4: the cache's zips. Yarn 1:
   an offline mirror's tarballs, from a bake whose fetch gets a mirror and a cache of its own.
   The bake gives the fetch only the variables of the Yarn that wrote the checkout's `yarn.lock`.
   A Yarn 2 or 3 lockfile is one hushrun doesn't read: the part is left out with a line, never
   guessed.
5. **Yarn 1 is a seed too, not the plan's in-place mirror (the plan's fallback).** A read-only
   mirror fails a stale install, so the seed copies the tarballs into `hushrun-offline-mirror`
   beside Yarn's cache folder, and `packages exec` and `env` give the install
   `YARN_YARN_OFFLINE_MIRROR` naming it. That needed one addition to the seed: a part may name
   the variables an install needs to read what was seeded (`Part::seed_env`). A mirror the person
   already names is theirs, and the part is left alone.
6. **The seed is the pnpm and npm job's.** ADR 0150's branch wasn't merged when this started, so
   `seed.rs`, `Reader::Seed`, `Admit` and its wiring into `exec`, `env` and `restore` are carried
   here as that branch has them, and the two merge cleanly in either order. Added on top: a part
   may seed its own way (`Part::seed`, for Bun's unpacking), `Part::seed_env`, `Part::locked_by`
   (a row's `locked` from the lockfile, where the path alone doesn't say it),
   `Part::lockfile_unread` and `Part::downloads`.

## Considered and dropped

- **Bun's unpacked cache, read on a signature.** The plan's second choice: right only if Bun
  refused a folder it didn't write, which it doesn't.
- **The bake running Bun and harvesting its cache.** Bun keeps no tarball; repacking a folder gives
  other bytes than the registry's.
- **Seeding a Yarn 1 cache directly.** Its entries are unpacked folders with a `.yarn-metadata.json`
  Yarn writes itself; writing that format would mean copying an internal format Yarn may change.
- **Yarn 1 read in place.** Above: a stale store would fail the install.

## Consequences

- An install from an empty cache with no registry to reach works for Bun, Yarn 4 and Yarn 1, and
  `hushrun run` gives a check nothing for either part: no key changes (tests compare everything
  COMMAND sees with and without the store).
- A Bun bake needs npm's registry, as a Yarn bake needs whatever registry the install uses.
- `hushrun packages env` in a Yarn 1 repository exports `YARN_YARN_OFFLINE_MIRROR`; a Yarn 4
  started from that shell would refuse it. It is printed only where the checkout's `yarn.lock` is
  Yarn 1's.
- Yarn 1 writes every later download into hushrun's mirror too, beside its cache; `yarn cache
  clean` doesn't remove it.
- A bake that runs after an install that found `node_modules` up to date gets nothing from Yarn 1,
  and says so.
