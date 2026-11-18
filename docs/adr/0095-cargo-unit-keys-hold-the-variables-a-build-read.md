# 0095. A Cargo test binary's key holds the variables its build read

- **Status:** accepted
- **Date:** 2026-11-17
- **Rules:** C-31, C-33, C-36, C-37, B-6, B-8

## Context

For a Cargo check with `tests` on, a test binary's unit key (C-31) hashes what the binary read
according to rustc's dep-info: `reads` (the binary's own dep-info, every workspace library it may
link, and for an integration test, a bench or an example every target of its package), `rest`
and `shape`. The reader of those files skipped every line that starts with `#`, so it dropped
rustc's `# env-dep:NAME=value` and `# env-dep:NAME` lines. Those name the variables the crate read
at compile time with `env!` and `option_env!`: ones a person or CI exports, ones a build script
sets with `cargo:rustc-env`, and Cargo's own (`CARGO_PKG_VERSION`, `CARGO_MANIFEST_DIR`, `OUT_DIR`,
`CARGO_BIN_EXE_*`, …).

Cargo rebuilds a binary when one of them changes, but its key stayed the same, so a pass recorded
with one value was reused for a binary built with another: a wrong reuse. The whole-repository
`rest` set hides the common case (a version bump also changes `Cargo.toml`), but nothing covers
a variable that comes from the environment, or one a build script derives from it, and a planned
narrowing of `rest` (per-unit `rest`, decision D2 of the CI-runs plan) would expose more. A test
written against the old code showed it: a binary built with `HUSHRUN_FIXTURE_FLAVOR` unset and
then with it set was reused the second time, and so was one whose build script set another value
with `cargo:rustc-env`. A review found the same for a crate outside the workspace (a registry, git
or out-of-tree path crate) that reads a variable: its dep-info was never read at all.

Build blocks already key the same lines (B-6 `env`: "each `env-dep` line of the dep-info, name and
value with roots replaced, or unset"), with one replacement of roots (B-8) so that keys name no
machine's folder.

## Decision

**What is keyed.** A Cargo test binary's key holds one more member, `env`: the SHA-256 of the
variables the same dep-info files name that its `reads` come from (C-31's closure), and those of
every library outside the workspace it may be built against. Those are the packages `cargo
metadata`'s dependency graph holds for the binary's package: its own dependencies of any kind,
then theirs other than dev-dependencies, closed, resolved with COMMAND's `--features`,
`--all-features` and `--no-default-features`. When `cargo metadata` can't give the graph, every
library outside the workspace that the build compiled counts for every binary, which keys more,
never less. Their files are no file of the scope, so only their variables are added, not their
`reads`. The graph costs `cargo metadata` without `--no-deps`, twice a run (the plan and the
reading after COMMAND): on hushrun's own workspace, 412 packages, about 0.13 s warm against
0.02 s, and 2 MB of JSON. Each
`# env-dep:` line is one entry: its name, `=` and its value, or its name alone when the variable
was unset when read, unescaped as rustc escapes it (`\\`, `\n`, `\r`), split at the first `=`. The
digest is of each entry's UTF-8 bytes followed by one NUL byte, sorted by bytes, each once; no
entry gives the SHA-256 of nothing, and every Cargo key holds `env`, that one included. A line
with an escape rustc never writes is one entry as written, so it still tells builds apart. The
entries come from one reader of `env-dep` lines, `hushrun_build::depinfo::env_dep`, which build
blocks' parser calls too, and from one function that reads a build, `read_build` in the Cargo
adapter, which the plan (before COMMAND) and the reading after COMMAND both call. `observe`'s line
keys from the run's build. `hushrun why`, which starts no tool, keys a binary by the `env` its
last build here held, which this machine's store keeps beside the binary's reads (`unit_env`).
`hushrun ready` says nothing of a Cargo check's test files, as before: they are known only from a
build. A Cargo list without `env`, in a row an older hushrun wrote or a hint, keys nothing, since
a key without `env` is an older hushrun's and its passes stand on another build. `why` then says
the binary runs because its last build here isn't known, and a run that reuses the binary on its
build's key gives the row that build's `env`. A dep-info that can't be read, or a library of the
binary's that has none, makes the binary `not observed`, never one that read nothing.

**What isn't keyed.** Only what rustc tracks: variables read with `env!` and `option_env!`, those
a build script sets with `cargo:rustc-env` among them. A build script that turns a variable into
`cargo:rustc-cfg`, into link arguments, or into a file it generates under `OUT_DIR` changes the
binary with the same key; that gap was there before this decision and is not closed by it. Nor
is a variable a proc macro reads with `std::env` while it expands, which neither rustc nor Cargo
tracks.

**The same on every machine.** Some values are folders of the machine: `CARGO_MANIFEST_DIR`,
`CARGO_MANIFEST_PATH`, `OUT_DIR`, `CARGO_TARGET_TMPDIR`, `CARGO_BIN_EXE_*`, `CARGO` (the
toolchain's cargo), and anything a build script derives from the checkout, the cargo home or the
home folder. In each value these folders, each in its given form and its form with symlinks
resolved, are replaced by B-8's placeholders, the longest folder first, where it starts and ends a
path: the repository's root (`{hushrun:workspace}`), Cargo's target folder (`cargo metadata`'s
`target_directory`, `{hushrun:target-dir}`), its build folder (`build_directory`,
`{hushrun:build-dir}`), the toolchain (the sysroot of COMMAND's `rustc`, `{hushrun:toolchain}`),
the rustup home (`RUSTUP_HOME`, else `$HOME/.rustup`, `{hushrun:rustup-home}`), the cargo home
(`CARGO_HOME`, else `$HOME/.cargo`, `{hushrun:cargo-home}`) and the home folder (`HOME`,
`{hushrun:home}`). Those of COMMAND's `env NAME=value` assignments win over hushrun's
environment, as for every tool the adapter starts; an empty variable is unset; a `CARGO_HOME` or
`RUSTUP_HOME` that isn't absolute is from where COMMAND runs, as Cargo reads it; a `HOME` that is
empty, not absolute or `/` is no folder and gives no cargo or rustup home. A folder two of them
share is named by the first of `target-dir`, `build-dir`, `workspace`, `toolchain`,
`rustup-home`, `cargo-home`, `home`: the home folder below every other, so `CARGO_HOME=$HOME`
names `{hushrun:cargo-home}`. This is build blocks' replacement, not a second one:
`Roots::of_checkout` in `hushrun_build::roots` builds the same `Roots` from these folders, the
cargo home and the toolchain as B-8's bases, and `build-dir`, `rustup-home` and `home` are root
names only these roots have, so a compile's roots and keys are as they were. A value under none of
them is keyed as it is, and then the key is per machine, correctly. Where the repository's root
isn't a `/` path (Windows), no value is replaced.

**The key's form.** The member is `"env": "sha256:<hex>"`, beside `reads`, `rest` and `shape`.
Only Cargo's keys carry it: pytest's, Vitest's, Jest's and every other adapter's are byte for byte
what they were, and so are a test's keys (C-39), since Cargo doesn't reuse tests one by one. The
schema string stays `hushrun.unit-key/v0`: an older CLI computes another key for a Cargo binary
than this one and misses, which costs one full run of each Cargo check after an upgrade or a
downgrade, as a new `env` value does.

**This machine's store.** Schema 2 gains one table, `unit_env`, one row for a test file whose key
held an `env`. A store of schema 2 made before it, which has every other table as this build
makes it and lacks this one, is this schema's store: opened to read, its test files hold no
`env`; opened to write, it gets the table in one transaction under the lock beside it, every row
kept. Any other difference in its tables sets it aside, as before.

## Consequences

- A wrong reuse is gone: a binary built with another value of a variable it, or a crate it is
  built against, read with `env!` or `option_env!` (one a build script set with `cargo:rustc-env`
  included) runs. What a build script derives any other way is not covered (above). The same value keys the same binary again, so nothing
  runs that didn't need to, and two checkouts at different paths, with different cargo homes and
  homes, compute the same keys.
- The first run of each Cargo check after the release reuses nothing on any machine, CI
  included: every Cargo key is new. Results recorded before stay, and keep counting for older
  CLIs.
- An older CLI (0.17 and before) checks a store's tables exactly, so it sets aside a store this
  build gave the `unit_env` table, and this build then makes the table again in the store the
  older one made anew. Alternating an older and a newer CLI in one repository on one machine
  costs the map, for every adapter, each time the older writes. One CLI version per repository,
  as CI's latest install and `hushrun update` give, costs nothing.
- `hushrun why` keys a Cargo binary by its last build here: a variable exported since that build
  is not seen until a run builds again. Its explanations name files, so when only a variable
  changed it can say the binary runs without saying which variable did.
- What a variable means isn't keyed: a value naming a file keys the name, not the file's bytes,
  as for a compile. A variable two crates of one closure both read with two values is two
  entries; the entries say which values were read, not by which crate, so two crates that swap
  their values between two builds give one key. That takes two crates of one closure reading one
  variable, with values that trade places and nothing else changing, such as two build scripts
  that each set it from another variable of the environment, exported the other way round. We
  know of no real case; keying entries by crate (below) would close it.
- Testdata gains Cargo vectors with `env` (set, unset and root-replaced values, folders two roots
  share, a build folder of its own), which `tools/refdigest.py unit-keys` recomputes on every OS
  from C-31's text.

## Alternatives considered

- **A new key schema, `hushrun.unit-key/v1`, for every adapter.** It would change every
  pytest, Vitest and Jest key and cost every check one full run, for a member they don't have.
- **Keying the entries by crate** (`{"<target root>": {"NAME": value}}`). It closes the swap case
  above, at the cost of a nested form, a name for crates outside the repository, and a second
  shape for the same lines build blocks key flat. Not chosen while no real case of the swap is
  known; it is the owner's call.
- **Keying the variables' current values from hushrun's environment** instead of the dep-info.
  It misses what a build script sets, and keys variables the build never read.
- **Replacing values with a second replacement in the CLI.** Two replacements drift; B-8's is
  tested for reversibility and collisions already.
- **A schema 3 store, a new file imported from schema 2.** It would leave an older CLI's file
  untouched, but costs an import of every table and a second file per repository in every CI
  cache, for one table an in-place addition gives.
- **Every library outside the workspace for every binary**, without the dependency graph. It
  needs no `cargo metadata` with dependencies, but a variable one crate reads would rerun every
  binary of the workspace. It is the fallback when the graph can't be had.
- **Storing nothing, and having `hushrun why` start no build.** `why` would then compute keys no
  run recorded and say every binary runs.
