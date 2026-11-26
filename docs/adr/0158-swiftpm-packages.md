# 0158. SwiftPM's packages: clones seeded into its cache

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-4, K-5, K-6 and K-10 amended (`docs/spec/11-baked-packages.md`); K-7 and K-8 read

## Context

A package store (ADR 0149) holds one part per package manager. A Swift package's dependencies are
git repositories: SwiftPM clones each into its shared cache (`repositories/<name>-<hash>`, a bare
clone), copies that into the package's `.build`, and checks out the revision `Package.resolved`
pins. A sandbox starts with an empty cache and clones every dependency from the network.

SwiftPM reads no second cache, so the part can't be read in place (mechanism A), as Gradle's and
Maven's are. It serves no registry for git dependencies (B). It is a seed (C), as the plan's row
for `swiftpm` says.

## Decision

1. **The part is `repositories/` of SwiftPM's cache**, each repository a bare clone, less
   SwiftPM's locks beside them and git's own notes in them (`FETCH_HEAD`, `ORIG_HEAD`, `gc.*`,
   `tmp_*`, `hooks/`, `logs/`, `objects/info/alternates`). The manifest set is every
   `Package.resolved`, `Package.swift` and `Package@swift-*.swift`.
2. **The bake runs SwiftPM's own resolve**: `swift package resolve --force-resolved-versions`
   in each package whose `Package.resolved` is beside its `Package.swift`, with `--cache-path`
   and `--scratch-path` at the bake's own folders, as `COMMAND` runs (B-63's `in_bake`). It runs
   with or without `COMMAND`, so no build of the check is needed. `--force-resolved-versions`
   resolves exactly the lockfile and never rewrites it, and the scratch folder keeps `.build` out
   of the checkout. `--harvest swiftpm=FOLDER` takes a cache the job already filled.
3. **The seed is main's (ADR 0150):** `hushrun packages exec`, `env` and `restore` copy each
   file of a clone a `Package.resolved` of the checkout pins into `repositories/` of SwiftPM's
   cache, never over a file that is there, each hashed as it is copied, written beside its place
   and renamed, with a stamp once nothing was left out. On macOS the cache is
   `~/Library/Caches/org.swift.swiftpm`; on Linux it is `$XDG_CACHE_HOME/org.swift.swiftpm`, else
   `~/.cache/org.swift.swiftpm`. The part is the same on both. `hushrun run` seeds nothing, as for
   pnpm and npm, though SwiftPM resolves inside `swift build` and `swift test` themselves: a
   sandbox seeds with `hushrun packages restore` in its setup, before its checks.
4. **A pin names a clone by its repository's name.** That name is the last component of the
   pin's location, less `.git`; SwiftPM's folder is that name, `-`, and 8 hex characters of a
   SHA-256 of its canonical location. Two pins whose locations end alike would both seed, which
   costs a clone SwiftPM won't use and nothing else. Reproducing SwiftPM's canonical URL to match
   the hash exactly would break silently whenever SwiftPM changes it.
5. **Verified by a signature, not the lockfile.** This is the plan's error (see below).
6. **`Package.resolved` versions 1, 2 and 3** are read: each remote source-control pin's location
   and revision. Another version gives nothing, and the part is left alone with a line. Nothing
   is guessed.

## What the plan got wrong

The plan's row said "lockfile: git checks a revision's content". It doesn't, for a clone from a
folder. Run in this container with git 2.43: replace a loose object of a bare clone with other
bytes under the same name, still valid zlib and the same size, then clone from that folder. The
checkout at the pinned commit holds the forged file. Only a clone over a transport
(`--no-local`, through `index-pack`) or `git fsck` notices. SwiftPM copies its cached clone
locally. So a store someone changed would be checked out at the pinned revision, and the lockfile
proves nothing about a clone's files. The part is `verified_by: signature`, like Maven's: an
unsigned store's `swiftpm` part is read only with `HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1`, outside
CI. The seed's own hash of every file also catches a changed byte where K-8 doesn't hash (a
sandbox, a laptop), which is more than the plan's tamper case asked for.

This branch first seeded each clone whole: written into a folder beside its place and renamed
in, objects hard-linked, a clone with any changed file not put in place. ADR 0150's seed merged
first, file by file, and this part now uses it. The cost: in a store nobody hashed whole (a
sandbox, a laptop), a clone one file of which changed is copied less that file, and SwiftPM then
fails on the missing object instead of downloading the clone. In CI, K-8 leaves the whole part
out first. A seed that groups a clone's files would close it, and is left for later.

## What was confirmed, and where

- **From SwiftPM's source** (main, read 2026-11-26): `--cache-path` names the shared cache. The
  default is `<caches folder>/org.swift.swiftpm`, where the caches folder is Foundation's.
  `--disable-dependency-cache` turns the cache off. A pin with a revision is fetched only if the
  cached clone lacks it (`RepositoryUpdateStrategy.ifNeeded(revision:)`, `fetchRequired`).
  Local repositories are never cached (`cacheLocalPackages` is `false`). `Package.resolved`
  versions 1 to 3 are the ones SwiftPM reads.
- **In this container** (Linux, no Swift: download.swift.org is blocked, and running a toolchain
  unpacked from Swift's image was refused): git's behaviour above. Also everything the stand-in
  `swift` of `packages_swiftpm.rs` drives: the bake through `--cache-path`, what a bake keeps,
  the seed, the stamp, the signed and unsigned reads, and each tamper case.
- **By CI alone**, through `swiftpm_resolves_offline_from_a_seeded_cache_and_no_units_key_moves`
  with the runner's Swift on Linux (`test`) and macOS (`test-macos`):
  - the bake's `swift package resolve` with these options;
  - the default cache folder on each system;
  - that SwiftPM resolves offline from a seeded cache while the same run without the store fails;
  - that a `swift test` run with the store and one without have the same units key;
  - that a lockfile changed by one package makes the part `behind` and still resolves online.

  macOS's cache folder in particular was checked only there.
- **Not confirmed:** `xcodebuild`'s use of the same cache. Xcode resolves packages into its own
  `SourcePackages` and, by default, through the shared cache too. A seed sets no variable and
  writes nothing in the checkout, so no `xcode/1` key and no unit key of `xcodebuild test` can
  move; but whether Xcode reads the seeded clones is not tested here.

## Considered and dropped

- **Naming the cache with `--cache-path`** to point SwiftPM at the store in place: SwiftPM writes
  into its cache (locks, fetches), and it is an option of COMMAND, which a reader never rewrites.
- **Seeding every clone the part holds:** a clone no pin names is one SwiftPM never reads.
- **The registry's downloads and binary artifacts** (`registry/`, `artifacts/`): not in the
  plan's row, and no test fixture of hushrun uses them.

## Consequences

- A sandbox whose image carries a signed store resolves a Swift package's pinned dependencies
  with no network.
- `part::Part` gains `download`, the tool's own download-only commands a bake runs with or
  without `COMMAND`.
