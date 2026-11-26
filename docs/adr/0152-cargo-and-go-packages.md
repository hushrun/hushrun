# 0152. Cargo and Go packages: a seed and a registry

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-4, K-5, K-6 and K-10 changed (`docs/spec/11-baked-packages.md`); K-3, K-7 and K-18's seed read

## Context

ADR 0149 made the package store and its first two parts, Gradle and Maven, both read in place
(mechanism A) and both verified by a signature. A sandbox that runs `cargo build` or `go test`
still downloads every crate and module its lockfile names. Neither tool reads a second, read-only
cache folder, so neither can be read in place. Both tools have a lockfile that pins each package's
hash, so neither part needs a signature.

## Decision

1. **`cargo` is a seed (mechanism C), the seed ADR 0150 made for pnpm and npm.** The part holds
   the `.crate` files of a cargo home's `registry/cache/<registry>/`, at the same paths. The seed
   copies each one the run's cargo home lacks into it, and only when its SHA-256 is the
   `checksum` a tracked `Cargo.lock` gives that name and version from a registry. Cargo unpacks it
   as it unpacks a download, into the same `registry/src` path, so no compile sees a difference.
   Unlike pnpm and npm, whose install runs before a check, cargo fetches inside the check's own
   build, so **`hushrun run` seeds the `cargo` part before COMMAND starts** (a part says so with
   `seeds_in_a_run`). After the first seed, a stamp makes later runs cost one `stat`.
   `hushrun packages exec`, `env` and `restore` seed as they do for npm. hushrun sets no variable
   and never names `CARGO_HOME`. Git dependencies are left out: a checkout is no registry's file
   and has no checksum.
2. **`go` is a registry (mechanism B).** The part holds `cache/download` of a module cache:
   each module's `.info`, `.mod` and `.zip`, nothing of Go's own notes. The reader sets one
   variable, `GOPROXY=file://<store>/parts/go/download,` followed by the `GOPROXY` Go would use
   without it, as `go env` says. That value covers Go's environment file and its own default, so
   a module the part lacks comes from where it came before. Go checks each module against
   `go.sum`, or against the checksum database when `go.sum` lacks it. `GOFLAGS` is never read or
   set.
3. **Both are verified by the lockfile.** Each row carries the lockfile's hash (`locked`):
   `Cargo.lock`'s checksum, and `go.sum`'s `h1:` for a `.zip` and for a `.mod`. Any run, CI
   included, reads an unsigned store's `cargo` and `go` parts. A store that also holds a part
   needing a signature is read for these alone when nothing verifies it, as K-7 says. A bake
   with no signature says so, and says that every run reads it.
4. **Where the reader can't rely on that check, it leaves the part alone.** With `GOSUMDB=off`,
   `GOPRIVATE` or `GONOSUMDB`, Go takes a module `go.sum` lacks without checking it (`go mod
   download`, `go get`), so it would take a store's bytes on their word. A `GOPROXY` the person
   already set is kept, as every reader keeps a variable already set.
5. **The bake's fetch** runs the check's own build with `CARGO_HOME` and `GOMODCACHE` at folders
   of the bake's own, beside Gradle's and Maven's. `--harvest cargo=FOLDER` takes a cargo home,
   and `--harvest go=FOLDER` takes a module cache (`go env GOMODCACHE`).

## What was confirmed by running the tool

On 2026-11-26, in a Linux container:

- **Cargo 1.94.0.** A cargo home that holds only `registry/cache/index.crates.io-…/*.crate`
  (no index, no `registry/src`) builds online without downloading a crate ("Downloading crates"
  never appears), and cargo unpacks them itself. Offline it fails without the index entries,
  which the part doesn't hold. **Cargo checks no checksum of a `.crate` already in its cache.** A
  crate repacked with one line added to a source file, under the same name, was unpacked and
  compiled. So the seed checks each file against `Cargo.lock` before copying it. A build whose
  crates were seeded and the same build whose crates were downloaded make the same blocks: with
  build reuse on, the second run of `cargo build`, `cargo check` and `cargo clippy` reused 3 of 3
  crates.
- **Go 1.24.7 and 1.27.4.** A part holding a module's `.mod` and `.zip`, made read-only and named
  first in `GOPROXY` before `off`, builds `go test` from an empty module cache with no network,
  and the part gains nothing. With `off` alone the build fails. A part that lacks a module falls
  through `,` to the next proxy. A zip whose contents changed is refused against `go.sum`
  ("SECURITY ERROR"), and against the checksum database when `go.sum` lacks it. With
  `GONOSUMDB` naming the module and `go.sum` lacking it, `go mod download` took the changed zip.
  That is why decision 4 exists. With `go.sum` lacking a module, `go test` refuses to build
  (`missing go.sum entry`). A zip with bytes appended after its end is accepted, since `h1:`
  hashes the files a zip holds and not the zip's bytes, and those files are the same.
- **Cargo's home at a folder of the bake's own** under rustup's `cargo` proxy: `cargo fetch`
  downloads into it, and the part holds what it downloaded, each row with its checksum.

Not confirmed here: Windows and macOS; a registry other than crates.io; the oldest Cargo whose
`registry/cache` folder name matches a newer one's. The folder's hash changed in Cargo 1.85, and a
cargo that names the folder differently than the bake's cargo finds nothing in the part and
downloads, as today.

## Considered and dropped

- **Pointing cargo at the part as a local registry** (`[source] replace-with`, a
  `local-registry`): that needs a configuration file in the checkout or the cargo home, and it
  replaces crates.io outright, so a stale store would fail a build that passes today.
- **Seeding the index too, for `--offline`:** the index cache is cargo's own note, in a format
  that changes between versions. hushrun sets no offline switch, and online cargo fetches the few
  index entries it needs.
- **Copying Go's modules into `GOMODCACHE`:** Go reads a module straight from the extracted tree
  without checking it again. A registry keeps Go's own check on every module.
- **Prepending to a `GOPROXY` the person set:** every reader leaves a variable the person set
  alone. Go's environment file isn't a variable, and the reader keeps its proxy after the part.

## Consequences

- A sandbox's `cargo build` or `go test` downloads only what its store lacks.
- A Cargo or Go store needs no signature to be read in CI.
- A run with a `go` part runs `go env` once as it begins. A run with a `cargo` part stats its
  stamp once it has seeded, and hashes only the crates it copies.
