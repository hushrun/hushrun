# 0096. Unit keys hold what build scripts gave a binary, and what Go tests read of the environment

- **Status:** accepted
- **Date:** 2026-11-18
- **Rules:** C-30, C-31, C-33, C-35, C-36, C-37, C-39
- **Amends:** ADR 0095 ("What isn't keyed": build scripts' `rustc-cfg`, link arguments and generated
  files are keyed now)

## Context

ADR 0095 put in a Cargo test binary's key the variables rustc tracks (`env!`, `option_env!`, and
`cargo:rustc-env`). A review of it found more wrong reuse of the same kind, and two more turned up
beside it.

1. **Build scripts (review finding B2).** A build script that turns a variable into
   `cargo:rustc-cfg`, into link arguments (`rustc-link-lib`, `rustc-link-search`,
   `rustc-link-arg`, …) or into a file it writes under `OUT_DIR` that the crate `include!`s builds
   another binary from the same files of the repository. rustc's dep-info names none of it but the
   generated file's path, which `reads` skips because it is outside the repository. The key stayed
   the same, so a pass recorded before was reused. Tests against the old code showed it for a cfg,
   a generated file and a link search path.
2. **Build scripts' dep-info was never found.** `depinfo_of` looked for a `.d` file in `deps/`
   or by an executable. A build script's compile is neither: Cargo names
   `target/<profile>/build/<pkg>-<hash>/build-script-build`, a copy of the compile beside it, and
   rustc wrote `build_script_build-<hash>.d` in the same folder (checked with Cargo 1.94 and
   `--message-format=json`). So `build.rs` was named by no dep-info: it fell in the rest set,
   where any change to it reran every binary of the workspace. And an integration test, whose
   package rule (C-31) reads every target of its package, met a target without dep-info and was
   `not observed`: recorded never, reused never.
3. **Go's `getenv` lines.** Go's test log (`-test.testlogfile`) names each variable the tests
   read with `os.Getenv` and `os.LookupEnv`. `gotest::read_log` skipped those lines, so a package
   was reused after a variable its tests read changed. Go's own test cache keys them
   (`cmd/go/internal/test`, `computeTestInputsID`, Go 1.27.4): a `getenv NAME` line hashes `NAME`
   with its value, or its absence, in the `go` command's environment (`hashGetenv`: a 0 byte for
   unset, a 1 byte and the value for set, so set to nothing is a value), and `GODEBUG` always,
   since the runtime reads it without logging it.

## Decision

### Cargo: `scripts`, a member of its own

A Cargo test binary's key holds one more member, `scripts`: the SHA-256 of what the build scripts
of its closure gave its build. The closure is ADR 0095's for `env`: the binary's package, the
workspace libraries it may link, and the crates outside the workspace its package's dependency
graph holds (every one the build compiled when there is no graph). Each entry is one of:

- **a build script's run** of one of those packages, from Cargo's `build-script-executed`
  message (its package id and `OUT_DIR`): `output `, its `OUT_DIR`, then for each line of the
  `output` file Cargo saved beside `OUT_DIR` that reaches the compiler or the linker, in the order
  the script printed them, a line feed and the line as `KEY=VALUE` without `cargo:` or `cargo::`.
  Those lines are `rustc-cfg`, `rustc-flags`, `rustc-link-lib`, `rustc-link-search`,
  `rustc-link-arg` and its `-bins`, `-bin`, `-tests`, `-examples`, `-benches` and `-cdylib` forms,
  and `rustc-cdylib-link-arg`;
- **a file the build made that a compile read**: a file the dep-info of the closure names
  (its `reads`' files and those of the crates outside the workspace) under Cargo's target folder or
  its build folder, `file `, the SHA-256 of its bytes as `sha256:<hex>`, a space and its path.

In each, the checkout's folders are replaced exactly as ADR 0095 replaces them in `env`
(`Roots::of_checkout`, `to_placeholders`): the same roots, the same function. The entries are
digested as `env`'s are (UTF-8, a NUL after each, sorted, each once), so none gives the SHA-256 of
nothing. Every Cargo key holds `scripts`, that one too.

The output is read by build blocks' own reader of it (`hushrun_build::script`, B-5, B-6), which
finds the file in both of Cargo's layouts (`output`, `stdout`) and trims lines as Cargo does; it
gains the list of these lines (`to_the_compile`), which build blocks don't read: a compile's key
holds the compile's own arguments, which carry them. `read_build` stays the one function that reads
a build, which the plan (before COMMAND) and `observe` (after it) both call; `hushrun why` keys a
binary by what its last build here held, which the store keeps beside `env` (below).

**Why these lines, and not the whole file.** The file holds lines that don't change the binary
and do change between machines or runs:

- `rerun-if-changed` and `rerun-if-env-changed` say when Cargo reruns the script, not what it
  built; their paths may be absolute and so name the machine.
- `warning` (and `error`, which fails the build) may print anything, a time or a folder included.
- `metadata` (and the old `cargo:KEY=VALUE` for any other key) reaches only the build scripts of
  dependents, as `DEP_<links>_<KEY>`: what those print is keyed in their own entries.
- `rustc-env` is keyed by rustc's `env-dep` lines exactly where a crate reads it (ADR 0095), and
  keying it here too would rerun binaries for a value no crate read.
- `rustc-check-cfg` changes which cfgs warn, not what is built.

Keying the whole file would make CI and a laptop miss for no reason, so the key holds the lines
that reach the compile, and nothing else. With the new `cargo::` syntax an unknown key is an error
Cargo refuses, and with the old one it is metadata, so this list is every line that reaches the
compile as of Cargo 1.94. Their order is kept: a link order is part of what is linked.

**Why a member of its own, not entries in `env`.** `env`'s entries are a variable's name and
value; any string can be a name, so a script entry could, in principle, be spelled like one.
A member of its own can't collide with `env`, keeps `unit_env` meaning what ADR 0095 says it
means, and lets a store that knows `env` but not `scripts` (ADR 0095's build) be told apart:
a Cargo list that lacks either keys nothing, and `hushrun why` says the binary's last build here
isn't known, as for a row without `env`.

**A generated file's path.** It is under the target or build folder, `{hushrun:target-dir}` with
roots replaced, and holds Cargo's hash of the script's run (`build/<package>-<hash>/out/…`). That
hash is the same for two checkouts at different paths, which ADR 0095's `OUT_DIR` already relies
on, and it changes when the build does, which is a correct miss. A file that can't be read makes
the binary `not observed` (`unreadable-generated-file`), and so does an output that can't be read
(`unreadable-build-output`).

### Cargo: build scripts' dep-info, and what they watch

`depinfo_of` finds a build script's dep-info beside the copy Cargo names:
`build/<package>-<hash>/build_script_build-<hash>.d` (the file's name with `-` as `_`, and the
folder's hash). Then:

- `build.rs` and the modules it includes are named by a dep-info, so they are sources (C-30), and
  each binary whose closure holds the script reads them: the binaries of its package and of every
  package that may link its library. Editing a build script reruns those, not the workspace.
- What a build script of the workspace watches with `rerun-if-changed` (from its package's folder
  when relative; a folder, every file below it) is read by the same binaries. A file of the rest
  set stays keyed by `rest` as before; this keeps the map honest for a narrower `rest`.
- An integration test of a package with a build script is observed, recorded and reused.

### Go: `env`, with the variables' values now

A Go package's key holds `env`, ADR 0095's member with the same entries and digest: each name a
`getenv` line of its tests' log names, and `GODEBUG` always, each with its value in COMMAND's
environment (its own `env NAME=value` assignments over hushrun's), or alone when unset; set to
nothing is a value. This is Go's rule, `computeTestInputsID`'s, with Go's process environment
being COMMAND's.

**Where the names live.** As for files, the map holds the list and the key takes what is current:
this machine's store keeps the names a package's tests read when it last ran here, one row each in
a new table, `unit_getenv`, and every key, the plan's before COMMAND, the record's after it and
`hushrun why`'s, takes their values from the environment it is computed in, by one function
(`gotest::Getenv::outside`). The store never keeps a value. A package whose tests were left out
one by one (C-39) keys by the names this run's log gave and those its last run here kept, as it
does for the files they opened.

**Every Go key holds `env`.** A list whose names aren't known, the build's alone or a hint's (a hint
holds no names), or a row an older hushrun wrote, is keyed by none: `GODEBUG`'s entry alone. That
is safe: a key recorded by this hushrun holds every name its run read, so a list that misses one
gives another key, never the same; and a key an older hushrun recorded holds no `env`, so no list
of this one matches it. A Go test's key (C-39) holds its package's `env` too, since Go reuses tests
one by one and what a test read is its package's (`reads: false`); every other adapter's test keys
are as they were. The cost is one full run of each Go check after the upgrade, as for Cargo.

**Roots replaced, unlike Go.** Go keys each value as it is; its cache is one machine's. hushrun's
keys are shared between machines, so each value has the checkout's root (`{hushrun:workspace}`) and
the home folder (`HOME`, when absolute and not `/`, `{hushrun:home}`) replaced, by ADR 0095's
`Roots::of_checkout` and `to_placeholders`. A `HOME` of `/home/ci` and one of `/home/dev`, or a
`PWD` in each checkout, then give one key, as two machines with the same layout should. On one
machine this changes nothing but one case: `HOME` itself moved to another folder reads as the same
`{hushrun:home}`, which ADR 0095 accepted for Cargo's roots. Go's toolchain (`GOROOT`), module
cache and build cache aren't roots: they need `go env`, and `hushrun why` starts no tool; under the
home folder by default, they are replaced as part of it. `TMPDIR` isn't a root either (below).

## Consequences

- The wrong reuses are gone: a binary whose build script gave it another cfg, another link
  argument or another generated file runs, and so does a Go package, or a Go test, whose tests read
  another value of a variable, or a variable newly set, unset or set to nothing. The same values
  key the same binary or package again.
- Every Cargo key and every Go key is new: the first run of each Cargo and Go check after the
  release reuses nothing, here and in CI. pytest's, Vitest's, Jest's and every other adapter's
  keys, and their tests' keys, are byte for byte what they were. The schema string stays
  `hushrun.unit-key/v0`.
- A variable a Go package's tests read changing reruns all of the package's tests: Go's log is the
  package's, as its files are. A test fixture that made a test fail through a variable now reruns
  its package; the Go adapter's tests that simulated a missed dependency with one now use a file
  outside the repository, which no key holds.
- **A fresh machine reuses no Go package whose tests read a variable.** Hints (C-38) hold no names,
  so a machine with no store of its own keys such a package by none, and misses. That includes
  every package that calls `t.TempDir`, which reads `GOTMPDIR` and `TMPDIR` (seen in a Go 1.27.4
  test log). A CI job that restores this machine's store (`units/*.sqlite`) keeps reusing them,
  run after run. Hints that carry names would close it (below).
- **`TMPDIR` is keyed as it is.** On macOS it is a per-user folder (`/var/folders/…/T/`), so two
  people's Macs never share a key for a package that calls `t.TempDir`; one Mac, and Linux runners
  that leave `TMPDIR` unset, do.
- `hushrun why` keys a Go package with the values in its own environment and COMMAND's
  assignments, as a run would; when no file changed it says that a variable its tests read may
  have another value now, naming them, since the store keeps no value to compare with.
- `build.rs` is a source now, so the sources' shape changes for every Cargo repository with a build
  script, which is one more reason every Cargo key is new; after that, editing a build script
  reruns only the binaries that read it.
- This machine's store gains two tables, `unit_scripts` (beside `unit_env`, which each row needs:
  it references it, so a `scripts` without an `env` can't be written) and `unit_getenv`, added in
  place as `unit_env` is (C-36). A store with any of the three missing reads without what they
  hold and gets them on its next write, every row kept. An older CLI sets aside a store that has
  them, as it does one with `unit_env`.
- `cargo test --no-run --message-format=json` already gave `build-script-executed` messages; the
  plan and `observe` read them and one `output` file per build script run, and the generated files'
  bytes, which costs reading what the build wrote, no tool more.
- Testdata gains vectors: two Cargo binaries with `scripts` (a cfg, link arguments, an outside
  crate under the cargo home, a generated file, lines left out, a build folder of its own), and a Go
  package with and without variables and one of its tests, which `tools/refdigest.py unit-keys`
  recomputes from C-31's text: the output's lines, the files' text, the log and the values.

## Alternatives considered

- **Key the whole `output` file.** Simple, and it misses between machines on warnings and absolute
  `rerun-if-changed` paths, and between runs on a warning with a time in it, for no change in the
  binary.
- **Use the `build-script-executed` message's fields** (`cfgs`, `linked_libs`, `linked_paths`,
  `env`) instead of the file. They say most of the same, but not `rustc-link-arg*` or
  `rustc-flags`, and they would be a second reader of what build blocks already read from the
  file; the message is used for what it alone says, which run is which package's and where its
  `OUT_DIR` is.
- **Fold `scripts` into `env`.** One member and one table less, at the cost of entries that could,
  in principle, collide with a variable's, and of a store that couldn't tell a row of ADR 0095's
  build from one of this.
- **Key Go's values raw, as Go does.** Every key would name the checkout and the home folder, so CI
  and a laptop would never share one for a package that reads `HOME` or `PWD`.
- **Make `TMPDIR` a root, or leave it out.** A root would let two Macs share keys, but a
  variable's value named by a placeholder no build block has; leaving it out would be a guess that
  no test's result depends on it. Both are the owner's call; keyed as it is, it is correct.
- **Names in hints** (a sealed list per package, as tests are, C-41). It would let a fresh machine
  reuse packages that read variables. It changes the hint format (a `v2`), which a server stores
  and older CLIs read, so it is its own decision.
- **Keep Go's digest of the last values in the store**, as Cargo's `env` is. It would key the
  values of the last run, not the ones a run now has: a run with another value would be reused.
