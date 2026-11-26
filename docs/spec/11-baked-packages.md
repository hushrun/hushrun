# 11. Spec: baked packages

> Normative. MUST, SHOULD and MAY follow RFC 2119.
>
> A baked store (B-63) gives a sandbox its compiled code. It doesn't give it its dependencies: the
> sandbox still runs `npm ci`, `cargo fetch` or a Gradle build that downloads half of Maven
> Central. A **package store** holds those downloads: one **part** per package manager, in that
> tool's own cache layout, a list of every file with its SHA-256, and optionally a signature. A
> run, or a setup script through `hushrun packages exec`, points each tool at its part. A tool that
> reads a second folder natively reads it in place; nothing is copied. The store is slower when it
> is stale, never wrong: a package it lacks is downloaded as it is today, and hushrun never sets a
> tool's offline switch.

A **package** is a file a package manager downloads because a lockfile or a build file names it:
a `.crate`, a module zip, a tarball, a wheel, a jar, a `.nupkg`. A **built entry** is a file that
exists only because code ran during an install. The word is "packages" because "deps" already
means compiled third-party code (B-65, B-72).

**K-1 The package store.** A package store is a folder that holds these and nothing else:

| Path | What |
|---|---|
| `packages.json` | schema `hushrun.baked-packages/v1` (K-2) |
| `packages.sigstore.json` | optional: its signature, a Sigstore bundle |
| `rows/<part>.jsonl` | one line per file of the part (K-3), sorted by path as bytes |
| `parts/<part>/…` | the part's files, in the package manager's own cache layout |

A part's name is its package manager's (`gradle`, `maven`, `go`, `cargo`, `pnpm`, `npm`, …):
lowercase ASCII letters and digits in words joined by `-`; a built part's ends in `-built`. One
function (`hushrun_packages::store::check_contents`) walks the folder and refuses any other file or
folder, a rows file or a part's folder of a part `packages.json` doesn't name, a part it names
with no rows file or no folder, a file under `parts/` no row names, a folder no row is under, a row
whose file is missing, a file whose size isn't its row's (and when it checks bytes, whose SHA-256
isn't), a file that may run whose row is `file` or the reverse, a link that isn't a row of kind
`link` with that target, and a special file. It never follows a link. A package store is never
inside a baked store's folder, and a baked store never inside it: a run, and every released
hushrun, refuses a baked store that holds any other file (B-63), so the package store is a sibling
folder, `/hushrun-packages` beside `/hushrun`. *Rests on:* every file a tool reads from a part
being named by a row, so that what a signature covers (K-7) is every byte a tool reads. *Breaks
if:* a tool read a file of its cache that the bake left out and a later writer added: the check
refuses that store whole.

**K-2 `packages.json`.** A JSON object with exactly these fields: `schema`
(`hushrun.baked-packages/v1`), `repository` (as a run names it, or null), `platform` (the baking
machine's, in a tag's words, as `baked.json`'s), `identity`, `baked_ms` (milliseconds since the
epoch; `SOURCE_DATE_EPOCH` when it is set), and `parts`, one object per part, each part once,
sorted by name, with exactly: `part`, `class` (`download` or `built`), `tool` (the tool's version
line, or `harvested`), `runtime` (null for a download part; what a built part's entries are bound
to), `inputs`, `manifests` (the manifest set's paths from the checkout's root, sorted), `identity`,
`rows` (`sha256`, `bytes` and `count` of its rows file), `bytes` (of its files) and `verified_by`
(`lockfile` or `signature`).

The three digests are each the SHA-256 of a list of UTF-8 fields, every field followed by one NUL
byte (no field holds one), with digests written as 64 lowercase hex characters:

- A part's **identity**: `hushrun.packages-part/v1`, its name, its class, its runtime (empty when
  null), then each row's path, kind and SHA-256, sorted by path as bytes.
- The store's **identity**: `hushrun.baked-packages-id/v1`, then each part's name and identity,
  sorted by name as bytes. So the same files give the same identity on any machine.
- A part's **inputs**: `hushrun.packages-inputs/v1`, its name, its tool's version line, the
  platform, then each file of its manifest set (K-4) as its path and the SHA-256 of its bytes,
  sorted by path as bytes. It is shown, and it lets a bake skip; it is never evidence.

`testdata/packages/vectors.json` holds vectors of all three, which `tools/refpackages.py`, written
from this text, checks. The file is written as two-space JSON and a newline, so the same store is
the same bytes. A reader refuses another schema (naming it as a version it doesn't read), a field
it doesn't know at any level, no schema, a part named twice, a part whose name isn't a part's, a
class, runtime and `verified_by` no bake writes together (a download with a runtime, a built part
whose name doesn't end in `-built`, with no runtime, or verified by a lockfile), a manifest that is
no path in a checkout, a store identity that isn't its parts', a part whose rows file isn't the
bytes `rows.sha256` names, a part identity that isn't its rows', and a part's `bytes` that aren't
its rows'. *Rests on:* SHA-256, and paths holding no NUL. *Breaks if:* two bakes wrote rows in
another order for the same files: the sort is by bytes, so they can't.

**K-3 A row.** One JSON object a line with exactly: `path` (relative to the part's folder,
`/`-separated, no empty, `.` or `..` component, no backslash, no control character), `kind`
(`file`, `exe` for a file its owner may run, `link`), `bytes` (a file's size; a link's, its
target's length), `sha256` (of the file's bytes; of a link's target text), and optionally `target`
(a link's, and only a link's: relative, and never above the part's folder at any step), `package`
(the package the tool gave the file, as the tool names it) and `locked` (the lockfile's own hash of
that package as the lockfile writes it, when the lockfile has one and the file is the artifact that
hash is of). A rows file ends each row with a newline, holds each path once, sorted by path as
bytes. *Rests on:* a package manager's cache needing no hard link, device or mode but "may run".
*Breaks if:* a tool's cache held a hard link it relies on: the bake writes two files instead.

**K-4 The manifest set.** A part's manifest set is the lockfiles of that package manager in the
checkout and, where it has none or the lockfile is optional, the build files that name
dependencies. For `gradle`: `settings.gradle`, `settings.gradle.kts`, every `build.gradle` and
`build.gradle.kts`, `gradle/libs.versions.toml`, `gradle/wrapper/gradle-wrapper.properties`,
`gradle.lockfile` and `gradle/verification-metadata.xml`. For `maven`: every `pom.xml`,
`.mvn/maven.config`, `.mvn/extensions.xml` and `.mvn/wrapper/maven-wrapper.properties`. Only files
git tracks count. *Rests on:* those files deciding what the tool downloads. *Breaks if:* a build
resolved by something outside them (an init script, a property of the machine): the part's
`inputs` would stay the same while what it downloads changed, and a bake would skip.

**K-5 What may be baked.** A **download**: a file the package manager fetched from a
registry, an index or a pinned commit, unchanged. Never a snapshot or a changing version
(`-SNAPSHOT`, a Gradle changing module), never a path, workspace or `file:` dependency, never
anything from the checkout, never a credential or a configuration file of the tool, never a lock
or a file the tool writes about its own use of the cache. A **built entry**, only in a built part.
Nothing else: a file the bake can't class is left out and counted in what the bake says. A
`gradle` part leaves out `*.lock`, `gc.properties` and every path with a `-SNAPSHOT` component; a
`maven` part leaves out `_remote.repositories`, `*.lastUpdated`, `resolver-status.properties`,
`*.lock`, `*.part`, every dot-folder (`.locks`, `.cache`) and every path with a `-SNAPSHOT`
component. *Rests
on:* a released version's bytes never changing at its registry. *Breaks if:* a registry served
other bytes for a version: the store keeps the bytes it had, and a tool that checks a hash refuses
them.

**K-6 Baking.** `hushrun packages bake --out DIR | --tar FILE [--only PART]... [--harvest
PART=FOLDER]... [--no-build] [--force] [--max-bytes N] [-- COMMAND]` writes the package store of the repository it runs in. With
`COMMAND` (the check's own build, such as `./gradlew test` or `mvn -B verify`) it runs COMMAND
the way a bake runs anything of the repository's (B-63's `in_bake`: hushrun's, CI's and signing
credentials withheld, no result recorded, no baked store or package store read, nothing it
started outliving it), with Gradle's user home (`GRADLE_USER_HOME`) and Maven's local repository
(`-Dmaven.repo.local=` joined to `MAVEN_OPTS`) at folders of the bake's own, so that what the part
holds is this build's downloads and nothing a machine had before; a registry credential the job
set for the tool stays. `--harvest PART=FOLDER` runs nothing for that part and takes a cache the
job already filled (a Gradle user home, a Maven local repository); a part harvested says
`harvested` as its tool. Either way a part is taken only when the tool's cache holds files, and
only the files K-5 keeps, each copied as `0644`, or `0755` when its owner may run it; a link is
kept only when it stays inside the part. The bake then writes the rows and `packages.json`,
checks the new folder by names and bytes (K-1), writes it through to the disk, and puts it in
place in one step under the folder's lock, as B-63 does (one bake of a folder at a time;
`.<name>.bake.lock` beside it). `baked_ms` is `SOURCE_DATE_EPOCH` when it is set, so the same
files bake to the same bytes. A store larger than `--max-bytes` (4 GiB unless given) fails the
bake, naming its two largest parts and `--only`. A bake with nothing to take fails and says what
it takes. It signs as a bake of compiled code signs (B-67): `--signing-key FILE`, the signing
variables, a CI job's ID token, or unsigned with a line (`--no-sign`: unsigned). The statement's
predicate type is `https://hushrun.com/attestations/baked-packages/v1` and its one subject is
`packages.json` by the SHA-256 of its bytes; `packages.json` names every rows file, which names
every file, so the statement covers every byte a tool reads. The signature,
`packages.sigstore.json`, is written into the new folder before it is put in place.

Without `COMMAND`, or with `--no-build` (which takes no `COMMAND`), a part whose build files the
checkout holds and no `--harvest` names is left out, with a line. **Nothing to bake:** when
`--out` already holds a store with every part this bake would take, each from the same tool line
and with the `inputs` the checkout has now, the bake changes nothing, says so and exits 0;
`--force` bakes anyway. **`--tar FILE`** writes the store as one ustar archive whose every entry is
under `hushrun-packages/`, so that `tar -x -C /` puts it where a run reads one: entries sorted by
path as bytes, each folder above a file once, folders `0755`, files `0644`, or `0755` for a row of
kind `exe`, a row of kind `link` as a link (`typeflag` `2`), owner and time 0, so the same store is
the same bytes on any machine; a name too long for a ustar header fails the bake. With `--tar`
alone, the store is baked into a folder beside the archive and removed after. *Rests on:* each tool downloading into the cache folder the bake names. *Breaks
if:* a build downloaded through another cache (an init script naming another Gradle home): those
files aren't in the part, and the tool downloads them in a run, as it does today.

**K-7 Trust and evidence.** What `packages.json` says is a claim. A run reads a part on evidence:

| Part | Evidence | Who may read it |
|---|---|---|
| `verified_by: lockfile` | none needed: hushrun checks each file against its row's `locked` hash and the lockfile in the checkout before a tool gets it, or the tool checks it itself against the lockfile | any run |
| `verified_by: signature`, class `download` | a signature by a signer `hushrun.yaml` names under `build: baked: signers:`, over these very `packages.json` bytes, passing `not_before` (B-67's check) | any run |
| the same, no such signature | the person's say-so, `HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1` | only a run that writes nothing others trust: not CI, not a run whose backend lets it write the trusted scope (B-63's one definition) |

`packages.json` names every rows file by its SHA-256 and every rows file names every file by its
own, so a signature over `packages.json` covers every byte a tool reads. Evidence for other
`packages.json` bytes is none. A store whose parts all need a signature, with no evidence, is
untrusted and read not at all; a store with parts of both kinds and no evidence is read for its
`lockfile` parts alone, with one line. `hushrun.yaml` gains no key: the signers are the baked
store's. *Rests on:* B-67's signers, and a signer signing only stores it baked. *Breaks if:* a
signer's job baked what a pull request wrote: the same holds of compiled code, and B-67 says what
a signing job may run.

**K-8 Checking bytes.** Before a tool reads a part in place, the run checks the part's folder by
names, kinds and sizes against its rows (K-1). A run that may write what others trust also hashes
every file of each part it reads, once per process, before any tool reads it, and leaves the part
out on the first difference, with a line. No time limit ends the hash. `hushrun packages verify`
hashes every file of every part, anywhere. *Rests on:* a file that changed size or kind being
found by its name and size, and a file changed in place being found only by its bytes. *Breaks
if:* a person's machine or a sandbox held a store someone changed in place with the same size: the
run reads it. A sandbox's store is as trustworthy as its image; the docs say so, and `hushrun
packages verify` finds it.

**K-11 Where a run finds the store.** `HUSHRUN_PACKAGES_LOWER` names it (set and empty: none; it
must be an absolute path). Unset, `/hushrun-packages` does when it holds a `packages.json`, and
only when it, its `packages.json`, `rows/`, `parts/` and every folder under `parts/` belong to root
or the run's user, none is a link, and neither its group nor anyone else may write them; a
read-only mount needs neither. A named folder is the person's choice and is read whoever owns it.
`HUSHRUN_PACKAGES=off` turns every reader off. The store is never written, dated or swept by a
run. *Rests on:* a folder only root or the run's user may write being theirs. *Breaks if:* another
process of the same user wrote it: it could as well have written the tool's own cache.

**K-12 Its state** is one function's answer (`hushrun_packages::state::state`), which a run and
every `hushrun packages` command call: **absent** (none named, or readers off: no line, no
variable); **invalid** (it can't be read or holds what a store never does: ignored, one line);
**wrong repository** (`packages.json` names a repository and the run another); **wrong platform**;
**untrusted** (K-7); **behind** (a part's `inputs` isn't the checkout's now: it is still read,
since what it holds is still what the lockfile hashes or the signer signed, and the tool downloads
what it lacks, one line per part); **current**. *Rests on:* a part's `inputs` being computed the
same way by the bake and the run. *Breaks if:* a run read another tool version than the bake did:
the part is called behind, and is still read.

**K-10 Readers.** A part reaches its tool in one of three ways: **A, in place** (the tool reads a
second, read-only folder beside its own cache; hushrun sets one variable or one property; nothing
is copied), **B, as a registry** (the tool downloads from the part and checks each package against
the lockfile itself), or **C, a seed** (hushrun copies or links each file the lockfile still names,
checked against its `locked` hash, into the tool's own cache). `gradle` is A:
`GRADLE_RO_DEP_CACHE=<store>/parts/gradle`, a folder that holds `modules-2` (Gradle 6.2 or later).
`maven` is A: `-Dmaven.repo.local.tail=<store>/parts/maven/repository` in `MAVEN_OPTS`, never
`MAVEN_ARGS`, which the Maven Wrapper's launcher doesn't read; a Maven older than 3.9 ignores the
property and downloads as it does today. A variable the person or the job already set is kept and
the part is left alone, with a line (`packages: <part>: left alone: <VARIABLE> is set already`);
for `maven`, a `maven.repo.local.tail` already named in `MAVEN_OPTS`, `MAVEN_ARGS`,
`.mvn/maven.config` or `.mvn/jvm.config`. A folder whose path holds white space, a quote or a
backslash can't ride in `MAVEN_OPTS`, which is split into words: the `maven` part is left alone,
with a line. Where a reader's variable is one another part of hushrun builds in the same run
(`MAVEN_OPTS`, which the `maven/1` cache and the Maven test adapter write), there is one value,
built once: the person's words, then the build cache's, then the reader's. A reader never names a
variable an adapter or a build kind owns (`NODE_OPTIONS`, `PATH`, `PYTHONPATH`, `RUBYOPT`,
`RUBYLIB`, `PHP_INI_SCAN_DIR`, `ERL_LIBS`, `ELIXIR_ERL_OPTIONS`, `BASH_ENV`, `MAKEFILES`,
`VSTestTargets`, `GOFLAGS`, `RUSTC_WRAPPER`, `RUSTDOC`, `CARGO_HOME`, `GRADLE_USER_HOME`), and
hushrun sets no offline switch. `hushrun run` gives COMMAND the readers' variables whether build
reuse is on or off (packages follow `HUSHRUN_PACKAGES`, not `hushrun build`), and lines for what
it ignored; `hushrun packages exec -- COMMAND` gives them to a command outside a run, records
nothing and exits with COMMAND's code; `hushrun packages env` prints them as `export` lines. A
variable a reader sets never enters a key: not a check's, not a
block's. *Rests on:* Gradle and Maven reading the second folder only for what their own cache
lacks, and writing nothing to it (confirmed with Gradle 8.14.3 and Maven 3.9.11, a read-only part,
an empty cache, offline). *Breaks if:* a tool wrote into the folder it reads: a read-only mount
refuses it, and a run's check by names finds a file no row names.

`pip` is B: `PIP_FIND_LINKS=<store>/parts/pip`, a flat folder of the wheels and source archives
a requirements file with hashes names. Its manifest set (K-4) is every `requirements*.txt`,
`*-requirements.txt` and `.txt` file in a `requirements` folder; its part keeps a file only when
one of them names its SHA-256 (`--hash=sha256:`), which is the row's `locked`. A bake fetches it
with pip's own download (K-30), `python3 -m pip download --no-deps --require-hashes` once per
requirements file that has hashes; one that has none fetches nothing. pip checks every file
against the requirements file's hashes, so the part is `verified_by: lockfile`. The part is left
alone, with a line, when `PIP_FIND_LINKS` is set, when a pip configuration file pip reads names
`find-links` (the variable would replace it), and when the folder's path holds white space, which
pip splits the variable on. *Rests on:* pip installing from the part when its index can't be
reached (confirmed with pip 26.2.2, a read-only part, an empty cache, no network). *Breaks if:*
the index can be reached: pip prefers the index's copy of a file whose hash equals the part's,
and downloads it, so the part saves nothing there; it is never wrong.

`poetry` is C, a seed (K-31) into Poetry's cache: `POETRY_CACHE_DIR`, else the platform's
(`$XDG_CACHE_HOME/pypoetry` or `~/.cache/pypoetry`, `~/Library/Caches/pypoetry` on macOS,
`%LOCALAPPDATA%\pypoetry\Cache` on Windows). Its part is `artifacts/` of a Poetry cache, each file
at the folder Poetry names by the file's URL and hash; its manifest set is every `poetry.lock`;
its part keeps a file only when a `poetry.lock` names that file's name with its SHA-256, so a
wheel Poetry built from a source archive in that folder is left out. A bake fetches it with
COMMAND, `POETRY_CACHE_DIR` at a folder of the bake's own. Poetry checks each file of its cache
against `poetry.lock` before it installs it. The part is left alone, with a line, when
`POETRY_CACHE_DIR` isn't an absolute path, and when the project's `poetry.toml` or the person's
Poetry `config.toml` names `cache-dir`, which hushrun doesn't read. *Rests on:* Poetry installing
a file its cache holds at that folder without downloading it (confirmed with Poetry 2.5.2: no
file downloaded, and a seeded file changed in place refused). *Breaks if:* Poetry changed how it
names that folder: the seeded files sit where Poetry never looks, and it downloads as today.

`uv` has no part. uv reads no second cache; its cache holds unpacked wheels in a layout of its
own, not the files it downloaded; it takes no file from `UV_FIND_LINKS` that `uv.lock` names at
an index; and an index named by `UV_DEFAULT_INDEX` or `UV_INDEX_URL` rewrites `uv.lock`
(confirmed with uv 0.13.10).

**K-30 A part the tool's own download fetches.** Where a tool has a command that downloads what
a part's manifest set names and runs nothing else of the repository's (pip's `download`), a bake
runs it for that part, with or without COMMAND, into a folder of the bake's own, the way it runs
COMMAND (K-6), and takes the part from that folder; `--no-build` runs it not, and a part
`--harvest` names isn't fetched. The part's `tool` is `fetched by` and the command's program
and subcommand, such as `python3 -m pip download`. A manifest set the command can't fetch from leaves the part out, with
a line that says why. *Rests on:* the command downloading for the machine the bake runs on, as
the store's `platform` says. *Breaks if:* a requirements file named packages for another
platform only: the command downloads none, and the part is left out.

**K-31 A seed.** A part read by a seed (K-10, C) is copied into the tool's cache before COMMAND
starts, in `hushrun run`, `hushrun packages exec` and `env`, and by `hushrun packages restore`.
Each row but a link is copied only when the lockfile in the checkout now names its file, by
name where the lockfile gives one and by SHA-256, with the row's `locked`; and only when the
store's file hashes to the row's SHA-256, which the seed checks as it copies. A row the
lockfile names no more is left out and counted; a file whose bytes differ is left out, with a
line naming it. A file already at its place in the cache is left as it is, unhashed: the tool
checks its own cache against the lockfile. A copy is written beside its place and renamed into
it, as `0644`. The seed writes nowhere else, and never into the store. *Rests on:* the tool
checking each file of its cache against the lockfile before it uses it. *Breaks if:* a file was
put at its place in the cache by something else with other bytes: the seed leaves it, and the
tool refuses it as it would today.
