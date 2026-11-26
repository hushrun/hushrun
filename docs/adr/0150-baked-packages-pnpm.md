# 0150. pnpm and npm in the package store

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-18 and K-19 new (`docs/spec/11-baked-packages.md`); K-6, K-7 and K-10 read

## Context

ADR 0149 made the package store and its first two parts, Gradle's and Maven's, both read in place.
A sandbox's setup script also runs `pnpm install --frozen-lockfile` before any check, and every
recipe in `recipes/agent-clouds/` begins with an install. The plan for this stage said pnpm's part
would be verified by the lockfile, seeded into pnpm's store, or read in place when nothing is
missing.

## Decision

1. **The part is pnpm's own store**, `store/v<N>/files` and `store/v<N>/index`, taken from a
   `pnpm fetch` the bake runs with `npm_config_store_dir` at a folder of its own, or harvested.
2. **A signature verifies it, not the lockfile.** `pnpm-lock.yaml`'s `integrity` is the SHA-512 of
   a package's tarball. pnpm's store keeps the tarball's files, each named by its own hash, and an
   index per package; it never keeps the tarball. An index could list any file under its
   package's name and pnpm would link it in, and no lockfile hash says otherwise. So the part is
   classed as Gradle's and Maven's are. Keeping tarballs instead, so that the lockfile could check
   them, would mean writing pnpm's index format ourselves: an internal format, which pnpm changes
   with its store's version.
3. **A seed, never in place.** pnpm writes into its store as it installs (new packages, `tmp/`,
   `projects/`), and `node_modules/.modules.yaml` records the store it came from: a read-only
   store a later install must change would fail it. The seed copies, never links: pnpm hard links
   from its store into `node_modules`, so a linked seed would leave a file of `node_modules` and a
   file of the part as one, and an edit to one would change the other.
4. **Only `packages exec` and `restore` seed.** By the time `hushrun run` runs a check, the setup
   script has installed. A run gives COMMAND nothing for the part: no variable, so no check's key,
   no test adapter's `NODE_OPTIONS` and no `tsc/1` key can change. Tests hold it: everything
   COMMAND sees in a run is the same with and without the store, and a Node test runner check, a
   Vitest check and a `tsc -b` compile made with the store are reused without it.
5. **Where it seeds**: `npm_config_store_dir` (or `NPM_CONFIG_STORE_DIR`) when set, else what
   `pnpm store path` says in the checkout, which reads the project's `.npmrc`, the user's and the
   platform's default. One start of pnpm, only in `exec` and `restore`.

6. **npm's part is its cache's tarballs, verified by the lockfile.** `_cacache/content-v2/sha512/…`
   names each tarball by its own SHA-512, which is `package-lock.json`'s `integrity` of it. So the
   part needs no signature: the seed copies a tarball only when a lockfile in the checkout names
   its hash now and its bytes hash to it. npm reads a tarball from its cache by that hash, with no
   index entry (`index-v5/` is left out: it holds the registry's URLs and request times) and no
   request. The same seed as pnpm's, the same reasons not to read in place: npm writes into its
   cache as it installs.

## Consequences

- An install from an empty store or cache with no registry to reach works: measured with pnpm
  10.28.0 (`downloaded 0`) and npm 10.9 (`npm ci`). A file whose bytes changed since the bake is left out with a line, and pnpm then
  downloads its package, or fails where it can't.
- A bake's `pnpm fetch` leaves the checkout's `node_modules` pointing at the bake's own store,
  which the bake removes; a bake job should not install in that checkout after it.
- Not here: anything an install's scripts make (`postinstall`, native builds) and `node_modules`
  as a tree. Both are built entries, behind the plan's decision D4.
