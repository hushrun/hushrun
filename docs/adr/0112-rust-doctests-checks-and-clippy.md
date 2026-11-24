# 0112. Rust: doctests as units, checks and clippy as blocks

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-74 to C-77, B-160, B-161; amends C-32's "Which adapter" (`--doc`); relies on
  C-30, C-31, C-32, B-4 to B-6, B-12, B-17, ADR 0068, 0069, 0076, 0080, 0095, 0096

## Context

Rust has had test units since ADR 0068 (a test binary, through Cargo's target runner, `cargo test`
and `cargo nextest run`) and build blocks since ADR 0076 (`RUSTC_WRAPPER`). An audit of what a
Rust project's CI runs, on a small workspace with a build script, a proc macro, path dependencies,
dev-dependencies and doctests, found five gaps:

- **Doctests always ran.** C-32 gave `cargo test --doc` no adapter, and in `cargo test` the
  doctests ran after the binaries every time. Doctests are often the slowest tests of a library,
  since rustdoc compiles each one (each block, before edition 2024's merged doctests) on its own,
  and a project that uses nextest runs them as a check of their own (`cargo test --doc`), as this
  repository's CI does: every one of them compiled and ran on every push.
- **`cargo check --all-targets` and `cargo clippy --all-targets` stored no program, test or proc
  macro.** The wrapper classed a compile by its crate type, so a test target's or a program's
  check was a linked output, and with no link line to read it was `built, not stored`
  (`link-inputs`): on the sample, 3 of 9 compiles of a second checkout's check were reused, where
  all but the changed crate's could be.
- **`cargo clippy` reused nothing of the workspace.** Cargo runs each member's compile as
  `clippy-driver rustc …`, and B-4 passed every call through a driver: a lint check, CI's most
  common job after the tests, compiled every member again in each checkout.
- **`cargo nextest run --archive-file` rebuilt the workspace.** The adapter took it, and its plan
  asked Cargo for this checkout's build, which compiles everything where the archive was meant to
  spare it, for binaries that aren't the archive's.
- **`cargo nextest run --partition count:M/N` could leave a test to nobody.** A partition by count
  gives each job the tests at its places in the whole list; a binary hushrun leaves out lists no
  test, so every test after it moves, and two jobs that left out different binaries could each
  think a test the other's.

Checked and found right: build scripts and proc macros (keyed and moved as B-6, B-24 say; under
`cargo test`, their outputs in `scripts`), `--release` (another profile is other arguments, other
keys; the sample's second checkout reused 4 of 5), a workspace of many members (the units and the
blocks are per crate), and `cargo nextest run --partition hash:M/N` (a test's job is its name's).

## Decision

1. **`cargo test --doc` gets the Cargo adapter; a unit is a library's doctests** (C-74), named by
   its root file, the unit rule being the libraries with `doctest: true`. `cargo test` without
   `--doc` keeps its doctests outside every unit, as before, since its library's unit test binary
   already has that name.
2. **hushrun stands in for rustdoc** (C-74, C-76) as its target runner stands in for a test
   binary: `RUSTDOC` names one of ADR 0080's programs, which starts the toolchain's rustdoc, the
   one Cargo would have, and passes everything through. A library `hushrun.json` lists by the
   file Cargo hands rustdoc for it (its `.rlib`) and that file's bytes isn't handed to rustdoc:
   nothing is compiled or run for it. The toolchain, profile, features and dependencies are in
   that file's bytes, as they are in a test binary's.
3. **The build is COMMAND's own, with a rustdoc that runs nothing** (C-75): Cargo refuses `--doc`
   with `--no-run`, and `cargo test --no-run` builds test binaries `--doc` never builds, so the
   plan and the read after COMMAND ask for `cargo test --doc --message-format=json` with
   `RUSTDOC=true`. Its artifacts are exactly what COMMAND builds, fresh after it.
4. **What doctests read is what a unit test binary of the library reads** (C-75): its dep-info,
   its package's path dependencies of any kind (a doctest may use a dev-dependency) and theirs,
   and their build scripts; plus the variables Cargo takes rustdoc's flags from and the bytes of
   Cargo's home's configuration, which a library's file doesn't show. `Cargo.lock` and the
   repository's Cargo configuration are in the rest set, which every key holds.
5. **A metadata-only compile is a library's check** (B-160), whatever its crate type.
6. **Clippy's driver is read** (B-161) for metadata-only compiles: the driver starts the compile
   as Cargo had it, its bytes join the compiler's identity, its `CLIPPY_` variables are keyed, and
   the configuration it would look for is searched for upward as a macro's `.env` is. Its lints
   are replayed with the block, as rustc's warnings are. Its linked outputs (a member's build
   script or proc macro) pass through as before.
7. **nextest's archives and partitions by count are refused, with why** (C-77): said once,
   COMMAND runs as it was.

## Consequences

- A push that changes one crate of a workspace runs only the doctests that read it, and its
  `cargo clippy --all-targets` and `cargo check --all-targets` in a new checkout compile only that
  crate's targets and its dependents; on the sample, a second checkout's clippy reused every
  member compile but the changed crate's, with its lints said again.
- A rustdoc the person set (`RUSTDOC`, `build.rustdoc`) keeps doctests unobserved, as a runner
  they set keeps test binaries unobserved. Windows has no `true` to build with, so doctests aren't
  observed there yet.
- A clippy block is never a rustc block: the driver is in the key, so `cargo check` and `cargo
  clippy` of one crate keep two blocks.
- The cloud's dashboard needs nothing new: doctest units are Cargo units under their library's
  file, and clippy and check blocks are `rustc/1` blocks of class `library`.

## Not done

- Doctests inside a `cargo test` without `--doc`: they would need a second unit under the
  library's name, or a unit holding both, which C-30's one-name-per-unit doesn't allow yet.
- Tests one by one for doctests (`unit: test`): a doctest's name is its file and line, which any
  edit above it moves.
- Combining a binary's tests across nextest's hash partitions, so that a binary spread over jobs
  records a pass.
- `cargo clippy --fix` and `cargo fix`, which run Cargo itself as the compiler, pass through as
  before.
