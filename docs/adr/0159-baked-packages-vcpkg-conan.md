# 0159. Baked packages for C and C++: vcpkg's asset cache, and no Conan part

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-10 gains the `vcpkg` part's paragraph (`docs/spec/11-baked-packages.md`); K-6 and
  K-7 read

## Context

A C or C++ sandbox that uses vcpkg downloads every port's sources before it builds a thing, and
vcpkg downloads its own CMake and Ninja when the machine's are older than it wants. ADR 0149 gave
hushrun a package store, one part per package manager, and the readers for Gradle and Maven. This
is the part for vcpkg, and the reason there is none for Conan, the two package managers the plan
names for C and C++.

## Decision

1. **A `vcpkg` part is vcpkg's asset cache**, `parts/vcpkg/assets/<sha512>`: each file named by
   the SHA-512 its portfile names. A bake keeps no other file. vcpkg reads it in place as one more
   asset source, `X_VCPKG_ASSET_SOURCES=x-azurl,file://<store>/parts/vcpkg/assets,,read`, after
   the person's words. Nothing is copied by hushrun: vcpkg copies the one asset it needs into its
   downloads folder, as it would after a download.
2. **The part is verified by its lockfile.** vcpkg checks every file it takes from an asset source
   against the SHA-512 the portfile names, and downloads from the file's own source when the file
   differs or is missing. The portfiles at the checkout's baseline, or in the overlay ports it
   names, are the lockfile. A store need not be signed for a run to read the part, in CI too.
3. **The fetch is the check's own command**, as for Gradle and Maven, and the one to name is
   `vcpkg install --only-downloads`. The bake gives it `X_VCPKG_ASSET_SOURCES` with its own folder
   as a `readwrite` source, and `VCPKG_DOWNLOADS` and `VCPKG_BINARY_SOURCES=clear`, so that a
   machine's downloads folder or binary cache can't keep a source out of the part. `--harvest
   vcpkg=FOLDER` takes an asset cache vcpkg filled the same way. A part says, through a new
   method with a default (`Part::fetch_env`), what its fetch needs; Gradle and Maven need none.
4. **A person's own asset source leaves the part alone**, with a line. vcpkg reads at most one
   asset source and refuses to start with two. Words that name none (`clear`, `x-block-origin`)
   are kept, before the part's.
5. **No `conan` part.** See below.

## What was confirmed by running the tool

On 2026-11-26, in a Linux container, with vcpkg 2025-10-16 (tag 2025.10.17) and a port of the
fixture's own whose source is a file served from a `file://` URL:

- `vcpkg install --only-downloads` with a `readwrite` asset source at an empty folder stores each
  source there under its SHA-512, and vcpkg's own CMake, Ninja and patchelf too when it downloads
  them (with `VCPKG_FORCE_SYSTEM_BINARIES=1` it downloads none).
- Download mode starts no process of a port: `vcpkg_execute_required_process` and
  `execute_process` refuse to run, so a portfile's `touch` of a canary file never happens.
- That folder, made read-only, as a `read` source with `x-block-origin` and an empty downloads
  folder: the install passes, every file "Asset cache hit", and the folder is unchanged.
- One byte of a file changed: vcpkg says the download "had an unexpected hash", and downloads the
  file from its origin; with `x-block-origin`, the install fails. A missing file is downloaded
  from its origin.
- Two `x-azurl` sources: "a maximum of one asset read url can be specified", and vcpkg stops.
- Every `cc/1` key is the same with and without the store (`tests/build_cc.rs`): an object built
  in a run that read the store is reused by a run without it.

Not confirmed: the oldest vcpkg with these sources (they date from 2021), macOS and Windows, and a
port from the built-in registry at a baseline, since this container's network refuses GitHub's
source archives. The fixture's port goes through the same `vcpkg_download_distfile` every port
does.

## Conan, dropped

The plan's Conan part was a seed: the bake runs `conan install` and `conan cache save`, and a run
restores the one archive with `conan cache restore`, the part verified by a signature. Conan 2.32's
restore can't meet the rules a seed keeps:

- **It extracts the archive with tar's filter turned off** (`the_tar.extraction_filter =
  (lambda member, _: member)`, `conan/api/subapi/cache.py`, then `extractall`). An archive whose
  entry is `../../escaped.txt` wrote that file beside the Conan home here. hushrun's rows would
  name one file, the archive; what lands on disk is whatever the archive says, unchecked by row.
- **It writes over what is there**: a package already in the cache is removed and replaced, and
  its metadata replaced whole (the code's own comment: "destructive"). A seed never writes over a
  file that is there.
- **The cache's database is Conan's own**, so hushrun can't unpack the files itself and leave the
  registering to Conan.

A signature says who baked the archive; it doesn't make a restore that writes outside the cache
safe. Conan Center is also out of this container's reach, so no downloaded binary could be tested.
The way back is a reader of kind B: a local Conan remote served from the part, which Conan
downloads from and checks itself, as the plan keeps a loopback registry for a tool whose seed
fails.

## Considered and dropped

- **`VCPKG_DOWNLOADS` at the part.** vcpkg writes into its downloads folder (a `.part` file, the
  extracted tools), and names files there by file name, which says nothing of their bytes.
- **Appending to an asset source the person named.** vcpkg refuses two.
- **A `vcpkg-built` part** (binary archives through `VCPKG_BINARY_SOURCES`) is Phase 4's.

## Consequences

- `hushrun run`, `hushrun packages exec` and `env` give vcpkg the variable; `X_VCPKG_ASSET_SOURCES`
  enters no key, and vcpkg builds from its own folders, so no compile's key moves.
- A bake that names only `vcpkg install --only-downloads` gets every source the manifest names,
  for this machine's triplet. vcpkg reports success even when a download fails, so a source it
  couldn't reach is simply not in the part, and a run downloads it.
- A store whose parts are all verified by a lockfile says so when it is unsigned, instead of
  saying that only the person's say-so reads it.
