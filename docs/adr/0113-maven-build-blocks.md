# 0113. Maven's modules through the build cache extension's remote cache

- **Status:** proposed; built (sharing through the relay, people's machines and CI writing their
  own scopes)
- **Date:** 2026-11-24
- **Rules:** B-170 to B-177 (draft); B-14, B-22, B-24, B-26 to B-32, B-34 read for Maven

## Context

Build blocks reuse compiled Rust, Go, C and C++, Gradle's tasks and Xcode's compiles between
checkouts and machines ([ADR 0078](0078-sharing-build-blocks-through-a-backend.md),
[ADR 0104](0104-gradle-build-blocks.md)). A Java build that runs on Maven had nothing: its tests are
reused test class by test class ([ADR 0088](0088-maven.md)), but each CI job and each new worktree
compiles and packages every module of the reactor again.

Maven has no build cache of its own. Apache's **build cache extension**
(`org.apache.maven.extensions:maven-build-cache-extension`, loaded from `.mvn/extensions.xml`)
is the one there is: it keys each module of a reactor by a **checksum** of its inputs (the
effective POM, every source and resource file by content and path relative to the module, the
checksums of the reactor modules it depends on), keeps entries in a local cache, and can ask a
**remote** cache over HTTP. Rewriting Maven's mojo execution to cache it ourselves (a core
extension of hushrun's that skips `compile` and `jar`) would be a second build cache with the
extension's work to do again and its subtleties to learn again; serving the extension's remote is
the same move as Gradle's.

A spike with Maven 3.9.11 and the extension 1.2.0 on JDK 21, against a loopback server of 30
lines, found what decides the rest:

- On a module, the extension `GET`s `{url}/v1.1/<groupId>/<artifactId>/<checksum>/buildinfo.xml`;
  on a hit, each file that file names (`<artifact>/<fileName>`, each
  `<attachedArtifacts>/<attachedArtifact>/<fileName>`), then restores the module and skips every
  mojo it recorded, `surefire:test` among them. After it built a module it `PUT`s each file, then
  `buildinfo.xml`, then, once per build, `build-cache-report.xml` under a build's id. Requests go
  through Maven Resolver's HTTP transport: `Expect: 100-continue`, `Content-Length`, kept
  connections, no credential unless `settings.xml` has a server for it.
- A second checkout at another path, with an empty local repository and local cache, restored
  both modules of a two-module reactor from the remote: the checksum is relocatable.
- `-Dmaven.build.cache.remote.url`, `…remote.enabled` and `…remote.save.enabled` given in
  `MAVEN_OPTS` (system properties) take effect; they win over the extension's configuration file.
  `-o` leaves the remote on.
- Maven prints the remote's URL at `INFO` for each file it saves or downloads.
- A module built twice makes a `buildinfo.xml` that differs in `buildTime` and `buildServer`,
  and a jar and the extension's zips of `attachedOutputs` that differ in their entries' dates
  (unless the build sets `project.build.outputTimestamp`, and the zips not even then).
- A `pom` module has no entry.
- An entry built with `-DskipTests` is restored into a later `mvn verify` of the same inputs, which
  then skips `surefire:test`: the extension's checksum doesn't hold the property, and
  `buildinfo.xml` records nothing of it, unless the build configures the extension's
  reconciliation of plugin parameters.

Two things set this kind apart from Gradle's. An entry is several files that come in several
requests, so the cache must hold them until the entry is whole. And the URL is all hushrun can
give Maven: no token in a header without editing `settings.xml`.

## Decision

- **hushrun is the extension's remote, for a run that shares** (B-170), in a build that loads the
  extension: its `.mvn/extensions.xml` (or its root POM) names `maven-build-cache-extension`. The
  build's own switch decides, as `org.gradle.caching` does for Gradle: hushrun loads nothing into
  Maven for it, and a build without the extension is told in one line. A build that names a remote
  of its own (in `.mvn/maven-build-cache-config.xml`, `.mvn/maven.config`, `.mvn/jvm.config`,
  `MAVEN_OPTS` or `MAVEN_ARGS`) keeps it, and the run says so.
- **The address rides on `MAVEN_OPTS`.** It is the one variable both the `mvn` scripts and the
  Maven Wrapper's launcher pass on (`MAVEN_ARGS` is the scripts' alone). COMMAND gets the person's
  `MAVEN_OPTS` and the three properties after it; where the Maven test adapter also sets
  `MAVEN_OPTS`, its value gains them, so COMMAND gets one.
- **A token in the path, for the run alone.** The listener is a thread of the `hushrun run`
  process on `127.0.0.1`, at a port the system picks, and every path starts with 64 hex characters
  made for the run: Maven prints the URL, so a token kept from run to run would sit in every CI log;
  one made for a run is worth nothing once the run ends. Gradle's is kept because Gradle's
  configuration cache records it; Maven has no such cache.
- **A block is a module's entry** (B-171, B-172): its key is the SHA-256 of
  `hushrun.maven-key/v1`, the layout, the group, the artifact and the checksum, and its first key
  is itself (`maven/1`). It holds `buildinfo.xml` in a canonical form (`buildTime` and
  `buildServer` fixed, every other byte the extension's) and each file it names as Maven sent it.
  `buildinfo.xml` is read as untrusted bytes with quick-xml: no DTD, one `build` root, the layout
  `v1.1`, the checksum, group and artifact the path's, plain file names named once with a size,
  64 at most. A stored block is served only when its files are exactly those, of those sizes and
  digests, with its `buildinfo.xml` canonical.
- **Pieces until whole** (B-175): the files of a module are held in memory, 512 MiB at most for
  all modules at once, until its `buildinfo.xml` and every file it names have come, in any order;
  then the whole is checked and kept. A file of another size than `buildinfo.xml` says, a
  `buildinfo.xml` of another module, a body too large: nothing is kept. The module's cost is the
  time from this run's missed `GET` of its `buildinfo.xml` to its entry being whole: what Maven took
  to build it here.
- **Files after `buildinfo.xml`** (B-174): a file is served only from a block whose
  `buildinfo.xml` this run served, so the vouch (B-34) asked for `buildinfo.xml` covers what
  follows, and nothing is served for a module the extension didn't first ask about.
- **Only a build that ran its tests is kept** (B-175): in a run whose Maven skips tests
  (`skipTests` or `maven.test.skip` in COMMAND, `MAVEN_OPTS`, `MAVEN_ARGS`, `.mvn/maven.config` or
  `.mvn/jvm.config`), no module's entry is kept, and the run says so once. Shared, such an entry
  would let another machine's `mvn verify`, and so a check hushrun records as passed, skip tests that
  never ran. Entries are still served to such a run: restoring a tested module into a build that
  skips tests loses nothing.
- **What may leave** (B-173): an entry whose `buildinfo.xml` or files hold the checkout's folder,
  Maven's local repository, the store or the home folder stays with the extension's own cache.
- **The rest is Gradle's** (B-174 to B-177): fetched only for a module the check's `maven/1`
  manifest names, under one platform, `jvm`; never audited or compared, since a jar keeps dates;
  not baked; the summary counts modules apart.

## Consequences

- A Java reactor's CI shares what main built with every branch from its first job, and a laptop
  that runs `hushrun run build -- mvn verify` takes what CI built, with no cache server of the
  team's own, once the build loads the extension.
- A restored module's tests don't run: that is the extension's decision, made on the module's
  inputs. hushrun's test adapter records no outcome for them in that run, and claims none (no rule
  of ADR 0088 changes). Where the entry came from hushrun, the module's tests ran and passed where
  it was made, or were left out by hushrun's own test reuse because they had passed on the same
  files: the extension saves only a build that passed, and hushrun keeps none that skipped tests.
  A profile or a POM that skips tests where the run can't see it is the build's to reconcile with
  the extension's own configuration.
- What can be reused is what the extension can: a module whose plugins read absolute paths into
  its effective POM keys differently on each machine; the extension's own `input` and `exclude`
  configuration is the remedy.
- The trust is the scopes', not the bytes', as for Gradle: an entry passes every check hushrun can
  make and may still hold outputs its inputs didn't make. The checksum is the extension's, 64 bits
  by default (`XX`); hushrun's key holds it under the module's coordinates, so a collision needs
  the same module, and the scopes decide who writes.
- The URL, token included, is in Maven's output for the run; it opens nothing once the run ends.
- Not built: Maven blocks in baked stores and `hushrun build fetch`; the extension's layouts other
  than `v1.1`; checking `fileHash` (the extension's own hash of a file, `XX` by default), since a
  forged entry can give any hash, and the store's SHA-256 already holds each file to what was kept.

## Alternatives

- **A core extension of hushrun's that caches mojo executions itself**: a second build cache to
  build and keep right, for a gain over the extension's of nothing a person can see.
- **Turning the extension on for every Maven build** (`-Dmaven.ext.class.path` with the extension
  and its dependencies): a build that never chose it would start skipping tests and mojos no one
  checked restore correctly.
- **A token in `settings.xml`**: a file of the person's to edit, or a `-s` that replaces theirs.
- **`MAVEN_ARGS`**: the Maven Wrapper's launcher doesn't read it.
- **A token kept in the store, as Gradle's**: printed by Maven in every log, for every later run.
