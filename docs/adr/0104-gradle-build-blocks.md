# 0104. Gradle's tasks through Gradle's own remote build cache

- **Status:** proposed; built (sharing through the relay, people's machines and CI writing their
  own scopes)
- **Date:** 2026-11-24
- **Rules:** B-77 to B-83 (draft); B-14, B-22, B-24, B-26 to B-32, B-34 read for Gradle

## Context

Build blocks reuse compiled Rust, pure-Go package archives and C and C++ objects between
checkouts and machines ([ADR 0078](0078-sharing-build-blocks-through-a-backend.md),
[ADR 0093](0093-go-build-blocks.md), [ADR 0097](0097-c-and-c++-build-blocks.md)). An Android app,
or any Kotlin or Java build, compiles through Gradle, and its CI jobs start from nothing: a branch
made this morning compiles every module, generates every `R` class and dexes everything again,
though main's CI did the same work an hour before. The tests of such a build are already reused
test class by test class ([ADR 0087](0087-gradle.md)), so what a changed branch pays for is the
build before its tests.

Gradle has the hook already. Each cacheable task has a **build cache key** (the hash of the task's
implementation, its inputs by content and their paths as the task normalizes them), Gradle keeps
entries in a local cache of its own, and on a miss it asks a **remote** cache over HTTP: `GET
{url}{key}`, and `PUT` after it runs the task. The cloud plan's §3.13 named it: "the relay serves
that HTTP on localhost for the build". The server needs no change: it stores sealed bytes under an
address, for any kind.

A spike with Gradle 8.14.3 on JDK 21, against a loopback server of 30 lines, found what decides
the rest:

- Gradle sends `GET` and `PUT /cache/<32 hex>` (an MD5 today) with Basic authentication, a
  `Content-Length` and keep-alive. An entry is a gzip stream of one tar: `METADATA` first
  (`buildCacheKey`, `identity` (the task's path), `gradleVersion`, `executionTime`,
  `creationTime`, `buildInvocationId`), then the outputs as `tree-<property>/…`, each member
  behind a PAX header with a sub-second date, long names given in it.
- A second checkout at another path with an empty Gradle user home took `compileJava` from the
  remote (`FROM-CACHE`): Gradle's keys are relocatable for the tasks that declare their paths so.
- **The configuration cache records every variable a settings script reads.** A port that changed
  from run to run made Gradle configure the build again every time.
- `gradle.startParameter.buildCacheEnabled = true` from a settings script works for the first run
  and not once the configuration cache is reused, since settings scripts don't run then. A
  remote set in `settingsEvaluated` does survive it.
- Two executions of one task with the same outputs made entries that differ only in `METADATA`'s
  three changing values and the tar's dates. Gradle read back an entry with those fixed (`0`, and a
  constant invocation id) and its dates zeroed, and dated every file it unpacked with the time it
  wrote it.
- `--offline` turns the remote build cache off.

Two things set this kind apart from Rust's, Go's and C's. hushrun computes none of the key: it is
Gradle's, from inputs hushrun never sees, so nothing can check an entry against its key beyond
`METADATA` naming it. And Gradle doesn't ask a task's outputs to be the same from one execution to
the next: test reports carry dates and host names, a `Jar` keeps file dates by default.

## Decision

- **hushrun is the build's remote cache, for a run that shares** (B-77). The listener is a thread
  of the `hushrun run` process, on the loopback, at a port and with a token the store keeps in
  `gradle/endpoint.json` (0600) from its first run, so the configuration cache holds. A second
  run of the same store at the same time gets a port of the system's choosing, and says that its
  Gradle configures again. It reaches the relay over the session's socket, as every adapter does.
- **Loading is ADR 0087's settings line**, unchanged. `HUSHRUN_GRADLE` names a script that sets
  the remote in `settingsEvaluated` unless the build set one itself (a Develocity cache stays the
  build's, as ADR 0050 already leaves it); the test adapter's script ends with the same text. No
  line, and the run says the line to add.
- **The build's own switch decides whether the cache is used.** hushrun doesn't turn Gradle's
  build cache on: a build that never turned it on hasn't been checked for tasks that cache wrongly.
  A build whose `gradle.properties` don't say `org.gradle.caching=true` is told so in one line.
- **The local side stays Gradle's own cache**, as Go's does: plain `./gradlew` and `hushrun run`
  are as warm as each other, and Gradle asks hushrun only about a task its own cache misses.
- **Keys and the canonical entry** (B-78). The block's key is the SHA-256 of
  `hushrun.gradle-key/v1\n` and Gradle's key, and is its own first key (`gradle/1`). The block
  holds one file, `entry.tar`: the entry in a canonical form (the same members in the same order,
  each with its name, type, mode and bytes, long names in PAX, `METADATA` sorted with its three
  changing values fixed), so its digest of outputs, which the server compares between uploads and
  the reader checks, is the same wherever a task made the same outputs. It is served as a gzip
  stream of stored deflate blocks: nothing to compress over the loopback, and the same bytes
  everywhere. Every check reads the bytes as untrusted: one gzip member with its CRC and length, a
  tar whose headers' checksums hold, names that are relative and plain, files and folders only, one
  `METADATA` naming the key asked, 1 GiB unpacked at most; a stored block must be canonical.
- **What may leave** (B-79): an entry whose member names or bytes hold the checkout's folder,
  Gradle's user home, the store or the home folder stays with Gradle's own cache, and the run says
  how many and why.
- **A fetch** (B-80) is asked only for a key the check's `gradle/1` manifest names, as Go's are,
  within half what the task took where it ran, and a download is served only while a server the
  run belongs to vouches for it (B-34). A download that fails its checks is reported `rejected`,
  its key suspended for the kept days, and removed.
- **Gradle's entries are never audited or compared** (B-81): a difference says nothing of a
  forged entry when the same task may make other bytes each time. A block there already that was
  only downloaded gives way to this execution, which then goes up; one built here stays. In shared
  `observe` entries go up and downloads aren't served, as for every kind.
- **Who writes** is the server's scopes, as for every kind: a trusted CI job of a listed branch
  writes `protected/<id>`, which every branch's jobs and every member's machines read first; a
  branch's CI writes its own `ci/<id>/<ref>`; a person's machine writes `person/<id>` or `team` as
  the org pools them. A new branch's first job reads main's blocks from its first task.
- **A manifest of its own** (B-82): the check asks `open` and sends `close` of kind `gradle/1`
  beside its own, under one platform, `jvm`, for every machine: Gradle's keys hold what a task
  depends on, the operating system included where a task declares it.
- **The summary counts Gradle apart** (B-83): `build: gradle: N tasks: X from hushrun's cache (Y
  from the shared store), Z run and stored, saving about T`, and a line for those that stay.

## Consequences

- An Android app's CI shares what main built with every branch from its first job, and a laptop
  that runs `hushrun run build -- ./gradlew assembleDebug` takes what CI ran, without a remote
  cache server of the team's own. A build with Develocity or another remote keeps its own.
- What can be reused is what Gradle can: tasks that aren't cacheable, or whose keys hold absolute
  paths, run on each machine as before. Android's own tasks are mostly cacheable and relocatable;
  where some aren't, the Android Cache Fix plugin is the known remedy, and the run's counts show
  what came from hushrun.
- The trust is the scopes', not the bytes'. An entry passes every check hushrun can make and may
  still hold outputs its inputs didn't make: a writer whose scope another machine reads can poison
  that machine's build. That is why a trusted copy is only CI's on a listed branch, and why a
  person's blocks reach only that person or, where the org pools them, its team. The security page
  says so.
- The listener is reachable by any process of the machine that has the token; the token is in the
  store, readable by its user alone, and in COMMAND's environment, as every session's secrets are.
- A `PUT` is held in memory, up to 512 MiB; a larger entry stays in Gradle's cache. An entry is
  unpacked in memory, up to 1 GiB.
- `--offline` turns hushrun's cache off with Gradle's own remote; the docs say so.
- Not built: Gradle blocks in baked stores (`hushrun build bake` leaves them out), `hushrun build
  fetch` for sandboxes, Maven's build cache extension. The server keeps entries' disagreements
  (`differs` at `begin`) for every kind; for Gradle they are expected wherever a task's outputs
  vary, and only discard the second copy.

## Alternatives

- **hushrun's store for Gradle's local entries too**: a cold first run inside `hushrun run`, and a
  plain `./gradlew` warm only from its own cache. Refused as Go's was.
- **The entry as Gradle sent it**: its digest of outputs would change with every execution, so the
  server would refuse every second upload of a key as `differs`, and a vouch could never match.
- **Turning the build cache on for the build** (`GRADLE_OPTS=-Dorg.gradle.caching=true`, which the
  spike showed works with the configuration cache): a build that never turned it on would start
  reusing tasks no one checked cache correctly.
- **A port per run**: Gradle configures every build again.
- **Comparing entries as other kinds are compared**: every test task and every jar with dates
  would be reported as differing, and its key suspended.
