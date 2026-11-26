# 0146. Baked packages: a package store beside the baked store

- **Status:** proposed; being built
- **Date:** 2026-11-25
- **Rules:** K-1 to K-8 and K-10 to K-12 new (`docs/spec/11-baked-packages.md`); B-63, B-65 and B-67 read

## Context

A warm sandbox starts with its compiled code on disk: `hushrun build bake` writes a baked store, an
image carries it at `/hushrun` or `hushrun build pull` downloads it there, and a run reads it under
its own store. It doesn't start with its dependencies. The sandbox still runs `npm ci`, `cargo
fetch`, `go mod download`, or a Gradle or Maven build that downloads what the build files name. For
hushrun's own tests, filling Maven's local repository and Gradle's user home from nothing takes
about 400 seconds each (CI run 45681483946). CI keeps both in Actions' cache, and a hit costs
seconds; a sandbox, a laptop and a provider's template have no such cache, and an agent's session
on this repository can't run the Maven and Gradle adapter tests at all, since their fixtures need a
filled repository.

## Decision

1. **A package store**, a folder of its own: `packages.json` (`hushrun.baked-packages/v1`), one
   rows file per part listing every file with its SHA-256, the parts' files in each package
   manager's own cache layout, and optionally a signature. One part per package manager. A part is
   read in place where the tool reads a second, read-only folder natively (Gradle's
   `GRADLE_RO_DEP_CACHE`, Maven's `maven.repo.local.tail`), and nothing is copied.
2. **A sibling, never inside the baked store.** A run refuses a baked store that holds any other
   file or folder, and so does every hushrun already released (`baked::check_contents`, called by
   `baked::state` by names). `hushrun build pull` unpacks only `hushrun/`. A package store inside
   `/hushrun` would switch off compiled-code reuse for every older hushrun. So it is
   `/hushrun-packages`, with its own file, its own signature and, later, its own image.
3. **The word is "packages"**: `hushrun packages …`, `/hushrun-packages`,
   `HUSHRUN_PACKAGES_LOWER`. "deps" already means compiled third-party code (the image's `deps`
   layer, `bake --deps-only`).
4. **A rule family of its own, `K-`, in a new spec file**, so that this work collides with no
   other effort's rule numbers in `10-build-blocks.md`.
5. **Identity from rows, not from the tool.** A part's identity is the digest of its rows' paths,
   kinds and SHA-256s; the store's, of its parts'. The same files give the same identity anywhere.
   A part's `inputs` (its tool's version and its manifest set's bytes) is shown and lets a bake
   skip; it is never evidence, since nothing proves the files came from those inputs.
6. **Never a wrong package.** A store that is stale names fewer packages than the lockfile; the
   tool downloads the rest. hushrun never sets a tool's offline switch.

Taken without asking, as the plan recommended and the owner hasn't yet answered (plan D1 to D3):
the word, the folder, a sibling store, and the rule family in a new file.

## What was confirmed by running the tool

On 2026-11-25, in a Linux container (`tools/startup/confirm/`):

- **Maven 3.9.11.** A repository filled by `mvn test` with `-Dmaven.repo.local=` in `MAVEN_OPTS`
  (439 files for one JUnit test), made read-only, is read as `-Dmaven.repo.local.tail=` **in
  `MAVEN_OPTS`** by an empty `maven.repo.local`, offline, by `mvn` and by `./mvnw`: the build
  passes and the empty repository gains nothing. Without the tail the same offline build fails.
  Maven checks no checksum of a file it reads from a local repository, so a `maven` part is
  verified by a signature, not by Maven.
- **Gradle 8.14.3.** `caches/modules-2` of a user home filled online, less `*.lock` and
  `gc.properties`, made read-only and named by `GRADLE_RO_DEP_CACHE`, is read by an empty user
  home offline: the build passes and the home's `modules-2` gains nothing. One more dependency,
  online, is downloaded into the writable home, and the part is unchanged.

Not confirmed here: that Apache's build cache extension computes the same module keys with and
without a tail; the oldest Gradle (6.2) and Maven (3.9.0) that read these; how either tool behaves
on another platform.

## Considered and dropped

- **New files in the baked store** and **new fields in `baked.json`:** every released hushrun
  refuses both, and would stop reusing compiled code.
- **A `hushrun.yaml` key** for where packages come from: every released hushrun refuses an unknown
  key under `build:`.
- **Setting a tool's offline switch** where a store is present: a stale store would fail an
  install that today passes.
- **`MAVEN_ARGS` for Maven's tail:** the Maven Wrapper's launcher doesn't read it (ADR 0113).
- **A local registry hushrun serves** to every tool: it checks every byte as it serves it, but
  it is a process for every install, and the tools that read a second folder need none.

## Consequences

- `hushrun packages bake` runs the check's own build with the tools' caches at its own folders,
  or takes a cache the job filled (`--harvest`), since no Gradle or Maven command resolves
  everything a build will ask for without running it. This release signs nothing: a store it
  bakes is read with `HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1`, outside CI. Signing, an image and a
  pull come later.
- `hushrun run` gives COMMAND the readers' variables whether build reuse is on or off. Where the
  Maven test adapter or the `maven/1` cache builds `MAVEN_OPTS`, the tail joins that one value.
- A program that lists a sandbox's folders sees one more, `/hushrun-packages`.
