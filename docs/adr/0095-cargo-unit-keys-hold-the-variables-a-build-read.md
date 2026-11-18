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
then with it set was reused the second time, and so was one whose build script set another value.

Build blocks already key the same lines (B-6 `env`: "each `env-dep` line of the dep-info, name and
value with roots replaced, or unset"), with one replacement of roots (B-8) so that keys name no
machine's folder.

## Decision

**What is keyed.** A Cargo test binary's key holds one more member, `env`: the SHA-256 of the
variables the same dep-info files name that its `reads` come from (C-31's closure). Each
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
build.

**The same on every machine.** Some values are folders of the machine: `CARGO_MANIFEST_DIR`,
`CARGO_MANIFEST_PATH`, `OUT_DIR`, `CARGO_TARGET_TMPDIR`, `CARGO_BIN_EXE_*`, and anything a build
script derives from the checkout, the cargo home or the home folder. In each value the
repository's root, Cargo's target folder (`cargo metadata`'s `target_directory`), the cargo home
(`CARGO_HOME` when absolute, else `$HOME/.cargo`) and the home folder (`HOME`), each in its given form and its
form with symlinks resolved, are replaced by B-8's placeholders (`{hushrun:workspace}`,
`{hushrun:target-dir}`, `{hushrun:cargo-home}`, `{hushrun:home}`), the longest folder first, where
it starts and ends a path. Those of COMMAND's `env NAME=value` assignments win over hushrun's
environment, as for every tool the adapter starts. This is build blocks' replacement, not a
second one: `Roots::of_checkout` in `hushrun_build::roots` builds the same `Roots` from these four
folders, and the home folder is a root name (`home`) that only these roots have, ranked below
every other, so a compile's roots and keys are as they were. A value under none of them is keyed
as it is, and then the key is per machine, correctly. Where the repository's root isn't a `/`
path (Windows), no value is replaced.

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

- A wrong reuse is gone: a binary built with another value of a variable it read, from the
  environment or from a build script, runs. The same value keys the same binary again, so nothing
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
- Testdata gains a Cargo vector with `env` (set, unset and root-replaced values), which
  `tools/refdigest.py unit-keys` recomputes on every OS.

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
- **Storing nothing, and having `hushrun why` start no build.** `why` would then compute keys no
  run recorded and say every binary runs.
