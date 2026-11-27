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
`composer`: every `composer.lock`. For
`nuget`: every `*.csproj`, `*.fsproj`, `*.vbproj`, `packages.lock.json`, `packages.config`,
`Directory.Packages.props`, `Directory.Build.props`, `Directory.Build.targets`, `global.json` and
NuGet.Config (its name in any case), and `.config/dotnet-tools.json`. For
`swiftpm`: every `Package.resolved`, `Package.swift` and `Package@swift-*.swift`. For `cargo`: every `Cargo.lock` and `Cargo.toml`. For `go`:
every `go.mod`, `go.sum`, `go.work` and `go.work.sum`. Only files git tracks count. *Rests on:*
those files deciding what the tool downloads. *Breaks if:* a build
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
and leaves out the rest of its cache (`repo/`, `vcs/`, `.htaccess`); a `nuget` part keeps only
files under a package's `<id>/<version>/` folder (a version starts with a digit), the
`.nupkg.metadata` NuGet needs to see the package included, and leaves out `*.lock` and every top
folder whose name starts with a dot (`.tools`). A `cargo` part keeps only
`registry/cache/<registry>/<name>-<version>.crate` (a git dependency's checkout is no registry's
file and has no checksum); a `go` part keeps only `download/<module>/@v/<version>.info`, `.mod`
and `.zip`, leaving out `list`, `*.lock`, `*.ziphash`, `*.partial`, temporary files and the
checksum database's `sumdb/`. A `swiftpm` part keeps the
bare clones under `repositories/` of SwiftPM's cache and leaves out every file beside them
(SwiftPM's locks), `*.lock`, `FETCH_HEAD`, `ORIG_HEAD`, `gc.log`, `gc.pid`, `tmp_*`, a clone's
`hooks/` and `logs/`, and `objects/info/alternates`. *Rests
on:* a released version's bytes never changing at its registry. *Breaks if:* a registry served
other bytes for a version: the store keeps the bytes it had, and a tool that checks a hash refuses
them.

**K-6 Baking.** `hushrun packages bake --out DIR | --tar FILE [--only PART]... [--harvest
PART=FOLDER]... [--no-build] [--force] [--max-bytes N] [-- COMMAND]` writes the package store of the repository it runs in. With
`COMMAND` (the check's own build, such as `./gradlew test` or `mvn -B verify`) it runs COMMAND
the way a bake runs anything of the repository's (B-63's `in_bake`: hushrun's, CI's and signing
credentials withheld, no result recorded, no baked store or package store read, nothing it
started outliving it), with Gradle's user home (`GRADLE_USER_HOME`), Maven's local repository
(`-Dmaven.repo.local=` joined to `MAVEN_OPTS`), Composer's cache (`COMPOSER_CACHE_DIR`), NuGet's
global packages folder (`NUGET_PACKAGES`), the cargo home (`CARGO_HOME`) and Go's module cache
(`GOMODCACHE`) at folders of the bake's own, so that what the part holds is this build's
downloads and nothing a machine had before; a registry credential the job set for the tool
stays. `--harvest PART=FOLDER` runs nothing for that part and takes a cache the job already
filled (a Gradle user home, a Maven local repository, a Composer cache, a NuGet global packages
folder, a cargo home, a Go module cache, as `go env GOMODCACHE` names it); a part harvested says
`harvested` as its tool. A part whose tool has a download-only command of its own is fetched by
it, with or without `COMMAND`, unless `--no-build` or a `--harvest` names the part: for
`swiftpm`, `swift package resolve --force-resolved-versions` in each package whose
`Package.resolved` is beside its `Package.swift`, with `--cache-path` and `--scratch-path` at
folders of the bake's own, run as `COMMAND` is, so that nothing of the checkout is written; its
tool is `fetched by swift package resolve` (K-32). SwiftPM runs no install script; resolving builds
and runs each package's manifest, as `swift build` does. Either way a part is taken only when the tool's cache holds files, and
only the files K-5 keeps, each copied as `0644`, or `0755` when its owner may run it; a link is
kept only when it stays inside the part. The bake then writes the rows and `packages.json`,
checks the new folder by names and bytes (K-1), writes it through to the disk, and puts it in
place in one step under the folder's lock, as B-63 does (one bake of a folder at a time;
`.<name>.bake.lock` beside it). `baked_ms` is `SOURCE_DATE_EPOCH` when it is set, so the same
files bake to the same bytes. A store larger than `--max-bytes` (4 GiB unless given) fails the
bake, naming its two largest parts and `--only`. A bake with nothing to take fails and says what
it takes. A part whose manifest set the checkout holds and whose cache the fetch left empty is
left out with a line that says so: an install that finds everything in place downloads nothing. It signs as a bake of compiled code signs (B-67): `--signing-key FILE`, the signing
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
property and downloads as it does today. `nuget` is A:
`NUGET_FALLBACK_PACKAGES=<store>/parts/nuget/packages`, a folder that holds `<id>/<version>/`. The
variable replaces every fallback folder a NuGet.Config names, so where the person set it already,
their folders are kept, first, and the part's joins after a `;`; otherwise the folders the
NuGet.Configs name (the checkout's root and each folder above it, the user's `~/.nuget/NuGet` and
`~/.nuget/config`, the machine's) are carried first, merged as NuGet merges them: the closest
config's in its order, a key a closer config named skipped, a relative folder from its config's
folder, nothing past a `<clear />`. A list hushrun can't read exactly (a `%VARIABLE%`, an entity,
an item other than `add` and `clear`) and a folder whose path holds a `;` leave the part alone,
with a line. NuGet hashes no file it reads from a fallback folder, and a lockfile's
`contentHash` is of the `.nupkg`, not of the extracted files NuGet reads, so a `nuget` part is
verified by a signature, with or without `packages.lock.json`. A variable the person or the job already set is kept and
the part is left alone, with a line (`packages: <part>: left alone: <VARIABLE> is set already`);
for `maven`, a `maven.repo.local.tail` already named in `MAVEN_OPTS`, `MAVEN_ARGS`,
`.mvn/maven.config` or `.mvn/jvm.config`. A folder whose path holds white space, a quote or a
backslash can't ride in `MAVEN_OPTS`, which is split into words: the `maven` part is left alone,
with a line.

`cargo` is C: it seeds as K-18 says of a seed (each file hashed as it is copied and checked
against its row, never over a file that is there, written beside and renamed, a stamp when it left
nothing out), into the `registry/cache` of the run's cargo home (`CARGO_HOME`, else `~/.cargo`), at
the same paths, and only a `.crate` whose SHA-256 is the `checksum` a `Cargo.lock` of the checkout
gives that name and version from a registry. Cargo unpacks it as it unpacks a download; it checks
no checksum of a file already in its cache (confirmed with Cargo 1.94), so a file whose bytes
aren't the ones a lockfile pins is never copied, with a line, and one no lockfile pins stays in
the store. Cargo fetches inside the check's own build, so `hushrun run` seeds the `cargo` part
before COMMAND starts too, as `hushrun packages exec`, `env` and `restore` do. Nothing is set:
hushrun never names `CARGO_HOME`. The part's registry folders are the baking cargo's
(`index.crates.io-<hash>`); a cargo that names its folder otherwise finds nothing there and
downloads, as today. `go` is B: `GOPROXY=file://<store>/parts/go/download,` then the `GOPROXY` Go would
use without it (`go env GOPROXY`, run with `GOTOOLCHAIN=local` outside any module, so that its
environment file and its own default count), so that a module the part lacks comes from where it
came before. Go checks each module it takes from the part against `go.sum`, or the checksum
database when `go.sum` lacks it, as it checks a download. Where Go would take a module `go.sum`
lacks unchecked (`GOSUMDB=off`, `GOPRIVATE` or `GONOSUMDB`, as `go env` says), or a store's
folder holds a character a `file://` address can't, the `go` part is left alone, with a line.
`GOPROXY` is the one variable it sets; `GOFLAGS` is never read or set.

`swiftpm` is C, and verified by a signature (K-7): only `hushrun packages exec`, `env` and
`restore` seed, and `hushrun run` gives COMMAND nothing for it. The seed copies each file of a
clone a `Package.resolved` of the checkout pins (by its repository's name, the last component of
the pin's location less `.git`; a local or registry package is no clone of SwiftPM's) into
`repositories/` of SwiftPM's cache: `Library/Caches/org.swift.swiftpm` under
`CFFIXED_USER_HOME`, else under `HOME`, on macOS (Foundation reads the home folder from
`CFFIXED_USER_HOME` and the user's account, never from `HOME`);
`$XDG_CACHE_HOME/org.swift.swiftpm`, else `~/.cache/org.swift.swiftpm`, on Linux; elsewhere the
part is left alone, with a line. It copies as pnpm's seed does (K-18): never over a file that is
there, each file hashed as it is copied and left out, with a line, when its bytes aren't its
row's, written beside its place and renamed, and a stamp when nothing was left out. A store
keeps no empty folder, so after a seed that copied anything, each clone in that folder gains the
folders git reads a repository by (`refs/heads`, `refs/tags`, `objects/info`, `objects/pack`). A clone no
lockfile pins now is left, with a count. A `Package.resolved` at the checkout's root of a version
other than 1, 2 or 3 leaves the part alone, with a line; nothing of it is guessed. Nothing is set:
no variable, no option. *Rests on:* SwiftPM fetching nothing for a pin whose revision its cached
clone holds, and copying that clone into the package's `.build` (its `ifNeeded` update strategy;
confirmed by CI's Swift on Linux and macOS, offline). *Breaks if:* a command named another cache
with `--cache-path`, turned it off with `--disable-dependency-cache`, a Swift read another
folder than these, or on macOS `HOME` named another folder than the user's account and
`CFFIXED_USER_HOME` was unset: nothing is there, and SwiftPM downloads as it does today; or a file of a clone
changed in a store nobody hashed whole: the seed leaves that file out, the clone lacks an object,
and SwiftPM fails on it rather than download (in a run that may write what others trust, K-8 has
left the whole part out first). Git checks no object against its name when it clones from a
folder, so a lockfile's revision proves nothing of a clone's files: a signature does.

Where a reader's variable is one another part of hushrun builds in the same run
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
an empty cache, offline); on Go serving a `file://` proxy read-only and checking it against
`go.sum` (confirmed with Go 1.24.7 and 1.27.4); on Cargo unpacking a `.crate` from its cache as
it unpacks a download (confirmed with Cargo 1.94). *Breaks if:* a tool wrote into the folder it reads: a read-only mount
refuses it, and a run's check by names finds a file no row names. For `nuget`, *rests on:*
NuGet reading a fallback folder only for packages its global packages folder lacks, writing
nothing to it, and seeing a package only where its `.nupkg.metadata` is (confirmed with the .NET
8 SDK 8.0.134, an empty global packages folder and no source; the part's bytes were the same
after restore and build). *Breaks if:* a project's own NuGet.Config, below the checkout's root,
named fallback folders: the variable replaces them, and NuGet downloads what they held, as it
would without them. `obj/project.assets.json` names every
folder NuGet read, the part's too, and is in no key: it is never tracked, and the dotnet adapter's
keys hold only tracked files a test project compiles (ADR 0118).

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

**The `bun` part.** The part holds `parts/bun/tarballs/<name>@<version>.tgz` (a scoped name's
scope as a folder), each the tarball of a package `bun.lock` names from npm's registry, which a
bake downloads (K-20), and nothing of Bun's own cache, which keeps packages unpacked. Its manifest
set is every tracked `bun.lock` and `bun.lockb`. `bun.lock` is read as JSON that allows a comma
before a closing bracket; its `lockfileVersion` must be 1 or 2. Another version, or a `bun.lockb`
alone (Bun's binary lockfile, which names no hashes), leaves the part out with a line. An entry of
its `packages` is a package when it is `["<name>@<version>", "<registry>", {…}, "sha512-…"]` and
the version holds no `:`: a workspace's packages and `file:`, `link:`, `git` and `github` entries
are not. One whose registry field isn't empty came from another registry, and is left out and
counted. Each row carries `package` (`<name>@<version>`) and `locked`, the entry's `integrity`. The
part is verified by the lockfile (K-7). Its reader is C, and only `hushrun packages exec` and
`hushrun packages restore` seed; `hushrun run` gives COMMAND nothing for it. The seed's folder is
`BUN_INSTALL_CACHE_DIR` when set, else `bunfig.toml`'s `[install.cache]` `dir` when it is absolute
(a relative one leaves the part alone, with a line), else `$BUN_INSTALL/install/cache`, else
`~/.bun/install/cache`; it unpacks (K-20) each tarball whose name, version and SHA-512 a
`bun.lock` of the checkout names now into `<name>@<version>@@@1/`, the folder Bun names a package
of the default registry by. Never A: Bun has one cache, links `node_modules` files out of it, and
fails an install that must add a package to a cache it can't write. *Rests on:* Bun installing,
with a frozen lockfile and no request, every package whose folder its cache holds, without checking
the folder, and leaving `node_modules` as an install from the network does, link for link, under
both its linkers (`hoisted` and `isolated`) (confirmed with Bun 1.4.4, an empty cache seeded from
tarballs, as a user who may not write the part, and no route to a registry). *Breaks if:* Bun named
its cache's folders otherwise (another suffix than `@@@1`): it doesn't see what was seeded and
downloads as it does today. Bun never checks a cached folder against the lockfile, so the seed's
check is the only one.

**The `yarn` part.** The part's files are of the Yarn that wrote the checkout's root `yarn.lock`.
For Yarn 4 (`__metadata:` with `version: 8`), `parts/yarn/berry/<file>.zip`: the zips of Yarn's
cache, each the file whose SHA-512 is a `checksum` (`<cacheKey>/<hex>`). For Yarn 1 (`# yarn
lockfile v1`), `parts/yarn/v1/<file>.tgz`: an offline mirror's tarballs, each the file whose
SHA-512 is an `integrity`. A Yarn 2 or 3 lockfile (`version` 4 or 6, whose `checksum` isn't the
zip's SHA-512) leaves the part out with a line. Its manifest set is every tracked `yarn.lock`. A
bake's fetch (`hushrun packages bake --out DIR -- yarn install …`) gives Yarn 4
`YARN_GLOBAL_FOLDER` at a folder of the bake's own and `YARN_ENABLE_GLOBAL_CACHE=true`, and Yarn 1
`YARN_YARN_OFFLINE_MIRROR` at a folder of the bake's own, made first, and `YARN_CACHE_FOLDER` at
another, so that every package is downloaded; never the other Yarn's variables, since Yarn 4
refuses Yarn 1's. `--harvest yarn=FOLDER` takes a Yarn 4 global folder (its `cache/`), a folder of
zips, or a Yarn 1 mirror. Only the `.zip` and `.tgz` files directly in that folder are kept. Each
row carries `locked`, the hash as `yarn.lock` writes it. The part is verified by the lockfile
(K-7). Its reader is C, and only `hushrun packages exec` and `hushrun packages restore` seed. For
Yarn 4 the seed's folder is what `yarn config get cacheFolder` names in the checkout (Yarn's global
cache, or the project's). For Yarn 1 it is `hushrun-offline-mirror` beside the folder `yarn cache
dir` names, and `hushrun packages exec` and `env` give the install `YARN_YARN_OFFLINE_MIRROR`
naming it (`restore` says it); a mirror the person names already (`YARN_YARN_OFFLINE_MIRROR`, or
`yarn-offline-mirror` in the checkout's or the home folder's `.yarnrc`) leaves the part alone,
with a line. Either way the seed copies only a file whose SHA-512 a `yarn.lock` of the checkout
names now. Never A: Yarn 1 writes every package it downloads into its mirror and fails an install
when it can't, and Yarn 4 fails an install over a cached zip whose bytes aren't its checksum.
*Rests on:* Yarn installing, with no request, from a cache folder of copied zips alone (Yarn 4,
the same `.pnp.cjs` as from the network) or from a mirror of copied tarballs (Yarn 1, the same
`node_modules`) (confirmed with Yarn 4.12.0 and 1.22.22, an empty cache and no route to a
registry). *Breaks if:* Yarn 4 named its zips otherwise (another `cacheKey` changes the checksums
too, so nothing is seeded and Yarn downloads), or a Yarn 1 bake found `node_modules` up to date:
it downloads nothing, and the bake says so (K-6).

**K-20 Downloaded by the lockfile, unpacked by the seed.** A part whose tool keeps no file a
lockfile hashes (Bun's cache holds packages unpacked) holds instead each package's tarball as the
registry serves it, which the lockfile's `integrity` hashes. A bake downloads those itself, with
no tool run, with or without `-- COMMAND` and `--no-build`: each package of the manifest set's
lockfiles that names its registry and an integrity, eight at a time, with no credential, each
request ending on an answer, a failure, or 30 seconds in which no byte came. A tarball whose
SHA-512 isn't the lockfile's, or that the registry doesn't serve, is left out, and the bake says
how many and the first; a part none of whose packages came is left out. The part's `tool` is `the
registry, by the lockfile`. A seed that unpacks checks each tarball as a copy is checked (its row's
SHA-256, then the lockfile in the checkout), unpacks it into a folder beside its place in the
tool's cache and renames that into place, never over a folder that is there: a gzip'd ustar
archive, with the first component of every path dropped (npm's `package/`), pax and GNU long names
read, regular files and folders only, a file `0755` when the archive lets anyone run it and
`0644` otherwise. A link, a device, an absolute path or a path with a `..` component refuses that
tarball, which is left out with a line, and the tool downloads it. The seed's stamp is written only
when it left nothing out. *Rests on:* a package's files in the tool's cache being its tarball's,
and nothing more (confirmed for Bun 1.4.4: a folder unpacked from the registry's tarball is the
folder Bun writes, byte for byte). *Breaks if:* a tool's unpacking changed a package's files (a
rename, a file of its own): the seeded folder would then differ from what the tool writes, which
a test that installs from a seed and compares the tree with an install from the network finds.

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
`hex` is C. Its folder holds `packages/hexpm/<name>-<version>.tar`, the tarballs of Hex's own
repository as Hex caches them, and nothing else of the cache: not `cache.bin` (Hex's registry
cache, in Hex's own format), not a file Hex is writing, not another repository's folder (an
organization's). Its manifest set is every `mix.lock` and `mix.exs`. A bake's fetch, such as `mix
deps.get`, runs with `HEX_HOME` at a folder of the bake's own and with `MIX_DEPS_PATH` and
`MIX_BUILD_PATH` at two more, discarded after it, so that the checkout's `deps` and `_build` are
left as they were and no `_build` is ever baked (a copied `_build` holds another checkout's paths);
`mix deps.get` runs no dependency's code, neither its `mix.exs` nor its compile. It is verified by
the lockfile: a row's `locked` is the outer checksum `mix.lock` gives that package from `hexpm`,
the eighth field of its `:hex` entry and the SHA-256 of the tarball. `mix.lock` is read one entry a
line, as Mix writes it; a lockfile in another form, or a `:hex` entry of other than eight fields
(the lockfile of a Hex older than 0.21, which has no outer checksum), gives no hashes, and the bake
leaves the part out and a seed leaves it alone, each with a line. Only `hushrun packages exec`,
`env` and `restore` seed it (K-50), as they seed `pnpm` and `npm`; `hushrun run` gives COMMAND
nothing for it, since by the time a check runs the install is done. The seed goes into
`packages/hexpm` of the cache Hex uses: `HEX_HOME`; else, when `MIX_XDG` is `1` or `true`, `hex`
under `XDG_CACHE_HOME` or `~/.cache`; else `~/.hex`. A relative or empty folder leaves the part
alone, with a line. A tarball is put only when `mix.lock` pins its SHA-256, and as a link to the
store's file where the file system allows one. Nothing is set: hushrun never names `HEX_HOME`, a
variable of Mix's or `ERL_LIBS`. `mix deps.get` then unpacks `deps/` from the cache, and `mix
deps.compile` runs as it does without the store, every time. Hex checks each cached tarball
against `mix.lock`'s outer checksum itself and downloads the package again when they differ. Hex
still fetches each package's registry record (signed, a few KB), so a seeded install needs Hex's
repository reachable; with no network it fails as it does without the store. *Rests on:* `mix
deps.get` taking a cached tarball whose SHA-256 is the lockfile's outer checksum without
downloading it, and replacing a cached file rather than writing into it (confirmed with Hex 2.7.0,
Elixir 1.18.4 and Erlang/OTP 27.3.4, against a repository that refused every tarball). *Breaks
if:* a Hex version kept its tarballs in another folder or under other names: the seed puts them
where Hex doesn't look, and Hex downloads as it does today; `mix.lock` took a newer form: the part
is left out, with a line; Hex wrote into a cached file in place: a linked file would change the
store's, which a run's check by bytes and `hushrun packages verify` find.

**K-50 A seed.** A part read by a seed (K-10's mechanism C) puts each of its files into the tool's
own cache, at the same path below it, when `hushrun packages exec`, `env` or `restore` runs. A
file the cache holds already is never written over. Each other file is read and its SHA-256
checked against its row and then against what the part's lockfile pins; a file whose bytes aren't
its row's or the lockfile's is never put, with one line, and the tool downloads its package; a
file no lockfile names now stays in the store. A file is written beside its place and renamed in,
so the tool never sees half of one: a copy, `0644` (`0755` for a row of kind `exe`), or, for a
part whose tool replaces a file of its cache and never writes into one (`hex`), a hard link to
the store's file where the file system allows one, whose bytes are the ones checked. A seed that
left nothing out writes `.hushrun-packages-<part identity>` in the folder it seeds, and the next
seed of the same part reads that stamp and nothing else: one `stat`. *Rests on:* SHA-256, and a
linking part's tool replacing a cached file rather than writing into it. *Breaks if:* that tool
wrote into a cached file in place: through a link it would change the store's file, which a run's
check by bytes and `hushrun packages verify` find; a person removed a seeded file after the
stamp: the tool downloads it, as it does today.

**K-45 The `bundler` part.** The part holds the `.gem` files of a gem cache and nothing else of
it: `parts/bundler/cache/<name>-<version>[-<platform>].gem`. Not a git source's clone
(`cache/bundler/git`), an installed gem, a built extension or a spec. Its manifest set is every
tracked `Gemfile`, `Gemfile.lock`, `gems.rb`, `gems.locked` and `*.gemspec`. A row's `package` is
`<name> (<version>)`, and its `locked` is the lockfile's `sha256=<hex>` from `CHECKSUMS`. The part
is verified by the lockfile (K-7) only when every lockfile of its manifest set has a `CHECKSUMS`
section, and it then holds only the files they hash; otherwise by a signature. A bake fetches the
part with Bundler's own download (K-32): `bundle cache --no-install --gemfile <Gemfile>
--cache-path <part>/cache` for each lockfile of its manifest set, which downloads each gem and
installs none, so no gem's install script runs, and writes nothing into the checkout; with no
`bundle` on `PATH` the part is left out, with a line. `--harvest bundler=FOLDER` takes a gem home
(`cache/`) or a `BUNDLE_PATH` (`ruby/<version>/cache/`) instead. The download and `COMMAND` get
`BUNDLE_PATH` at a folder of the bake's own that the part never takes from, `BUNDLE_FROZEN=true`
and `BUNDLE_GLOBAL_GEM_CACHE=false`. Its reader is C, a seed, and only `hushrun packages exec`
and `hushrun packages restore` seed; `hushrun run` gives COMMAND nothing for it. The seed copies into
`<Bundler.bundle_path>/cache`, where Bundler looks for a gem before it downloads one, the folder
the `ruby` on `PATH` names when asked from the folder of the Gemfile Bundler reads
(`BUNDLE_GEMFILE`'s, or the nearest `Gemfile` or `gems.rb` up to the checkout's root) with
`RUBYOPT` unset. So `bundle install` installs each gem where it would after a download, and a gem
loads from the same path with and without the store. It copies only a gem the `GEM` section of a
lockfile in the checkout names now, and when that lockfile's `CHECKSUMS` hashes it, only when the
SHA-256 of its bytes is that hash; a gem named with no hash only for a part a signature verifies.
A seed sets no variable: not `GEM_HOME`, `GEM_PATH`, `BUNDLE_PATH`, `BUNDLE_CACHE_PATH`,
`RUBYOPT`, `RUBYLIB` or Bootsnap's. The part is left alone, with a line, when no Gemfile is found,
there is no `ruby`, or Bundler's global gem cache is on. *Rests on:* Bundler taking
`<path>/cache/<file>` as the download, and checking it against `CHECKSUMS` itself (confirmed with
Bundler 4.0.18 and Ruby 3.3.6). *Breaks if:* a `.bundle/config` in the checkout names Bundler's
path or app cache during the bake: it outranks the bake's variables, the gems go there, and the
part is left out; or the global gem cache is on, which the question to `ruby` sees: the part is
left alone.
