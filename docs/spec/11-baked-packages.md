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
`.mvn/maven.config`, `.mvn/extensions.xml` and `.mvn/wrapper/maven-wrapper.properties`. For
`composer`: every `composer.lock`. Only files git tracks count. *Rests on:* those files deciding what the tool downloads. *Breaks if:* a build
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
component; a `composer` part keeps only `files/<vendor>/<name>/<40 hex>.<type>`, Composer's archives,
and leaves out the rest of its cache (`repo/`, `vcs/`, `.htaccess`). *Rests
on:* a released version's bytes never changing at its registry. *Breaks if:* a registry served
other bytes for a version: the store keeps the bytes it had, and a tool that checks a hash refuses
them.

**K-6 Baking.** `hushrun packages bake --out DIR | --tar FILE [--only PART]... [--harvest
PART=FOLDER]... [--no-build] [--force] [--max-bytes N] [-- COMMAND]` writes the package store of the repository it runs in. With
`COMMAND` (the check's own build, such as `./gradlew test` or `mvn -B verify`) it runs COMMAND
the way a bake runs anything of the repository's (B-63's `in_bake`: hushrun's, CI's and signing
credentials withheld, no result recorded, no baked store or package store read, nothing it
started outliving it), with Gradle's user home (`GRADLE_USER_HOME`), Maven's local repository
(`-Dmaven.repo.local=` joined to `MAVEN_OPTS`) and Composer's cache (`COMPOSER_CACHE_DIR`) at
folders of the bake's own, so that what the part
holds is this build's downloads and nothing a machine had before; a registry credential the job
set for the tool stays. `--harvest PART=FOLDER` runs nothing for that part and takes a cache the
job already filled (a Gradle user home, a Maven local repository, a Composer cache); a part harvested says
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

`vcpkg` is A: vcpkg's asset cache, read as one more asset source. Its manifest set is every
`vcpkg.json` and `vcpkg-configuration.json`. The part is `parts/vcpkg/assets/<sha512>`: each file
named by the SHA-512 a portfile (or vcpkg's own list of the tools it downloads) names, as 128
lowercase hex characters; a bake keeps no other file. A bake's fetch (`vcpkg install
--only-downloads` is the one to name) gets `X_VCPKG_ASSET_SOURCES=x-azurl,file://<folder>/assets,,readwrite`
after the person's words, and `VCPKG_DOWNLOADS` at a folder of the bake's own and
`VCPKG_BINARY_SOURCES=clear`, so that vcpkg downloads every source this build names and stores
each there; `--harvest vcpkg=FOLDER` takes an asset cache vcpkg filled the same way. A run gives
vcpkg `X_VCPKG_ASSET_SOURCES=x-azurl,file://<store>/parts/vcpkg/assets,,read`, after the person's
words when they name no asset source (`clear`, `x-block-origin`). vcpkg reads at most one asset
source and refuses to start with two, so a value that names one leaves the part alone, with a line
(`packages: vcpkg: left alone: X_VCPKG_ASSET_SOURCES names an asset source already, and vcpkg
reads only one`), and so does a store whose path holds white space, a quote, a backslash or one of
`` ,;`%#? ``; a bake's fetch then isn't given the variable either, with a line. vcpkg checks each
file it takes from an asset source against the SHA-512 its portfile names, and downloads from the
file's own source when it differs or is missing, so the part is `verified_by: lockfile`: the
portfiles at the checkout's baseline, or in the overlay ports it names, are its lockfile, and a
run reads the part with no signature. vcpkg copies an asset into its own downloads folder and
builds from its own tree, so no compile reads a file of the part where it is, no `cc/1` key holds
the store's path, and the store may be anywhere. *Rests on:* vcpkg's asset sources (`x-azurl`
with a `file://` URL, `read`, `x-block-origin`) and download mode starting no process of a port
(confirmed with vcpkg 2025-10-16 on Linux: a read-only part read from an empty downloads folder
with the origin gone; a file changed in place refused for its hash and downloaded from its
origin). *Breaks if:* a vcpkg stopped checking an asset's SHA-512 or took one by another name: the
part would then need a signature, as `maven`'s does. A file a portfile downloads with no SHA-512
(`SKIP_SHA512`) never goes through an asset source, and is downloaded every time.

`composer` is C. The part holds the archives of Composer's files cache, `parts/composer/files/
<vendor>/<name>/<40 hex>.<type>`, where the hex is the SHA-1 of the archive's URL, as Composer
names an archive it downloads. A bake's fetch is `composer install --download-only --no-scripts
--no-plugins`, which writes no `vendor/` and runs no code of a package; `--harvest
composer=FOLDER` takes a cache folder a job already filled (the one `COMPOSER_CACHE_DIR` names,
which holds `files/`). Each row carries its package (`vendor/name`) and no `locked`: a lock's
`dist.shasum` is empty for most packages, Packagist's among them, so the part is verified by a
signature (K-7). Only `hushrun packages exec` and `hushrun packages restore` seed; `hushrun run`
gives COMMAND nothing for it, and never names `PHP_INI_SCAN_DIR` or any variable of Composer's.
The seed copies into the files cache `composer config cache-files-dir` names in the checkout only
an archive a `composer.lock` there names now, by its package's name and the SHA-1 of its
`dist.url`, and, where that package's `shasum` isn't empty, only when the archive's SHA-1 is it;
one no lock names is left, with a count, and one whose bytes aren't its row's or its `shasum` is
left with a line. `composer install` then extracts each package from the cache and writes
`vendor/` itself, with the project's own scripts and plugins as the person's command says, the
same `vendor/` an install from the network writes. *Rests on:* Composer reading an archive from
its files cache by its name, with no request, and checking it against a `shasum` only when the
lock has one (confirmed with Composer 2.8.12 on PHP 8.3, an empty cache and a registry nobody
answers). Composer has no second, read-only cache beside its own: `COMPOSER_CACHE_READ_ONLY` with
`COMPOSER_CACHE_DIR` would read the part, but in place of the person's cache, not beside it, so
the part is a seed. *Breaks if:* Composer named its archives otherwise (the seed fills names it
doesn't read, and it downloads as it does today), or a dist URL Composer rewrites before it
downloads, such as a mirror's (that archive is left out, and downloaded).

**K-18 The `pnpm` part.** The part holds pnpm's content-addressed store as `store-dir` names it:
`parts/pnpm/store/v<N>/files/…` (each file named by the SHA-512 of its bytes) and
`store/v<N>/index/…` (one index a package, which lists its files), and nothing else of the store:
not `tmp/`, not `projects/` (links to the checkouts that installed from it), not a lock. Its
manifest set is every tracked `pnpm-lock.yaml`. A bake's fetch (`hushrun packages bake --out DIR
-- pnpm fetch`) runs with `npm_config_store_dir` at a folder of the bake's own; `--harvest
pnpm=FOLDER` takes a `store-dir` a job already filled (the folder `pnpm store path` names is its
`v<N>` folder). A row of an index carries its package as pnpm writes it (`ms@2.1.3`, a scope's
`/` as `+`) and no `locked`: the lockfile's `integrity` hashes a package's tarball, which the
store doesn't keep, so nothing in `pnpm-lock.yaml` hashes what the part holds, and the part is
verified by a signature (K-7). Its reader is C, a seed, and only `hushrun packages exec` and
`hushrun packages restore` seed: `hushrun run` gives COMMAND nothing for it, since by the time a
check runs the install is done. The seed copies each file of the part into the store pnpm uses
(`npm_config_store_dir` or `NPM_CONFIG_STORE_DIR` when set, else the parent of the folder
`pnpm store path` names in the checkout), never over a file that is there, each file hashed as it
is copied and left out, with a line, when its bytes aren't its row's; pnpm then downloads what it
was of. A file is written beside its place and renamed, so pnpm never sees half of one. A seed
that left nothing out writes `.hushrun-packages-<part identity>` in that store, and the next seed
of the same part looks at nothing else. The seed never sets `NODE_OPTIONS` or any other variable,
and never uses pnpm's global virtual store: `node_modules` is pnpm's own, with links inside the
checkout, as an install from the network makes it. *Rests on:* pnpm installing from its store,
with a frozen lockfile, every package whose index and files are there, and checking each file
against its index as it imports it (confirmed with pnpm 10.28.0, store `v10`, an empty store and
a registry nobody answers). *Breaks if:* pnpm changed its store's layout (a new `v<N>`: the part
seeds a folder that pnpm doesn't read, and pnpm downloads as it does today), or a lockfile
changed since the bake (the packages it adds are downloaded).

**K-19 The `npm` part.** The part holds the tarballs of npm's cache and nothing else of it:
`parts/npm/_cacache/content-v2/sha512/<2 hex>/<2 hex>/<124 hex>`, each file's path the SHA-512
of its bytes. Not `index-v5/` (it names the registry's URLs and the times of the bake's
requests), not `_logs/`, not another hash's folder. Its manifest set is every tracked
`package-lock.json` and `npm-shrinkwrap.json`. A bake's fetch (`hushrun packages bake --out DIR --
npm ci --ignore-scripts`) runs with `npm_config_cache` at a folder of the bake's own;
`--harvest npm=FOLDER` takes a cache folder a job already filled (the one `npm config get cache`
names). Each row carries `locked`, `sha512-<base64>` of its path, as `package-lock.json` writes
the `integrity` of that tarball. The part is verified by the lockfile (K-7): a run reads it with
no signature and no say-so. Its reader is C, a seed, and only `hushrun packages exec` and
`hushrun packages restore` seed; `hushrun run` gives COMMAND nothing for it. The seed copies into
the `_cacache` of the cache npm uses (`npm_config_cache` or `NPM_CONFIG_CACHE` when set, else what
`npm config get cache` names in the checkout) only a tarball whose SHA-512 an `integrity` of a
lockfile in the checkout names now (any version of the lockfile: `packages` and nested
`dependencies`), and only when the SHA-512 of its bytes is that hash. A tarball no lockfile names
now is left, with a count; one whose bytes aren't its row's or its name's is left with a line, and
npm downloads it or fails without a registry. A seed that left anything out writes no stamp, so
the next lockfile can name what it left. *Rests on:* npm reading a tarball from its cache by the
lockfile's `integrity`, with no index entry and no request, when the lockfile has `integrity` and
`resolved` (confirmed with npm 10.9 and `npm ci`, an empty cache and a registry nobody answers).
*Breaks if:* npm stopped reading its cache by hash (a seed then saves nothing, and npm downloads
as it does today), or a lockfile held only `sha1` hashes (nothing is seeded).

**K-30 The `pip` part.** The part is a flat folder, `parts/pip/<file>`, of the wheels and source
archives a requirements file with hashes names, and pip reads it as a registry (K-10, B):
`PIP_FIND_LINKS=<store>/parts/pip`. Its manifest set is every tracked `requirements*.txt`,
`*-requirements.txt` and `.txt` file in a `requirements` folder. The part keeps a file only when
one of them names its SHA-256 (`--hash=sha256:`), and that hash, as `sha256:<hex>`, is the row's
`locked`. A bake fetches the part with pip's own download (K-32), `python3 -m pip download
--no-deps --require-hashes` once per requirements file that has hashes; one that has none fetches
nothing, with a line. `--harvest pip=FOLDER` takes a folder of such files (`pip download -d`).
pip checks every file against the requirements file's hashes, so the part is `verified_by:
lockfile`. The part is left alone, with a line, when `PIP_FIND_LINKS` is set, when a pip
configuration file pip reads names `find-links` (the variable would replace it), and when the
folder's path holds white space, which pip splits the variable on. *Rests on:* pip installing
from the part when its index can't be reached (confirmed with pip 26.2.2, a read-only part, an
empty cache, no network). *Breaks if:* the index can be reached: pip prefers the index's copy of
a file whose hash equals the part's, and downloads it, so the part saves nothing there; it is
never wrong.

**K-31 The `poetry` part.** The part is `artifacts/` of a Poetry cache: `parts/poetry/artifacts/
<2 hex>/<2 hex>/<2 hex>/<58 hex>/<file>`, each wheel or source archive unchanged in the folder
Poetry names by the file's URL and hash. Its manifest set is every tracked `poetry.lock`. The part
keeps a file only when a `poetry.lock` names that file's name with its SHA-256, which is the row's
`locked`; a wheel Poetry built from a source archive in that folder is no download and is left
out. A bake fetches the part with COMMAND (`hushrun packages bake --out DIR -- poetry install`),
`POETRY_CACHE_DIR` at a folder of the bake's own; `--harvest poetry=FOLDER` takes a Poetry cache
folder a job already filled. The part is verified by the lockfile (K-7). Its reader is C, a seed,
as the `pnpm` part's is (K-18): `hushrun packages exec`, `env` and `restore` seed, `hushrun run`
gives COMMAND nothing. The seed copies into `artifacts/` of Poetry's cache (`POETRY_CACHE_DIR`,
else `$XDG_CACHE_HOME/pypoetry` or `~/.cache/pypoetry`, `~/Library/Caches/pypoetry` on macOS,
`%LOCALAPPDATA%\pypoetry\Cache` on Windows) only a file a `poetry.lock` in the checkout names now
by its name and the SHA-256 of its bytes. Poetry checks each file of its cache against
`poetry.lock` before it installs it. The part is left alone, with a line, when `POETRY_CACHE_DIR`
isn't an absolute path, and when the project's `poetry.toml` or the person's Poetry
`config.toml` names `cache-dir`, which hushrun doesn't read. *Rests on:* Poetry installing a file
its cache holds at that folder without downloading it (confirmed with Poetry 2.5.2: no file
downloaded, and a seeded file changed in place refused). *Breaks if:* Poetry changed how it names
that folder: the seeded files sit where Poetry never looks, and it downloads as today; or the
index can't be reached: Poetry asks it for each file's URL, and fails as today.

**K-32 A part the tool's own download fetches.** Where a tool has a command that downloads what a
part's manifest set names and runs nothing else of the repository's (pip's `download`), a bake
runs it for that part, with or without COMMAND, into a folder of the bake's own, the way it runs
COMMAND (K-6), and takes the part from that folder; `--no-build` runs it not, and a part
`--harvest` names isn't fetched. The part's `tool` is `fetched by` and the command's program and
subcommand, such as `python3 -m pip download`. A manifest set the command can't fetch from leaves
the part out, with a line that says why. *Rests on:* the command downloading for the machine the
bake runs on, as the store's `platform` says. *Breaks if:* a requirements file named packages for
another platform only: the command downloads none, and the part is left out.

`uv` has no part. uv reads no second cache; its cache holds unpacked wheels in a layout of its
own, not the files it downloaded; it takes no file from `UV_FIND_LINKS` that `uv.lock` names at an
index; and an index named by `UV_DEFAULT_INDEX` or `UV_INDEX_URL` rewrites `uv.lock` (confirmed
with uv 0.13.10).
