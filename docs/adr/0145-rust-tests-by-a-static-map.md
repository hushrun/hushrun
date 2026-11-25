# 0145. Rust tests one by one, by a static map of what each reaches

- **Status:** proposed; built
- **Date:** 2026-11-25
- **Rules:** C-146 to C-152 added; C-39 amended; relies on ADR 0068, 0069, 0080, 0095,
  0096

## Context

hushrun's Rust adapter (ADR 0068) reuses whole test binaries. A test binary reads its own dep-info,
the dep-info of every workspace library it may link and, for an integration test, of every other
target of its package (the package rule). A change anywhere in a crate runs again every test binary
that links it. In this repository, any change in `crates/hushrun/src` runs all 108 test binaries
again: about 25 minutes on Linux and 45 on macOS.

Other adapters reuse one test at a time by watching what each test reads (pytest, Vitest, Go). A
compiled Rust test reads no source file at run time, so there is nothing to watch. Coverage and
runtime tracing are ruled out: neither hushrun nor any repository that uses it may rely on them.

What is left is to read the source: for each test function, which source it can reach. A skip
counts as a pass, so a map that misses a dependency hides a real failure. The map must never be
narrower than can be shown.

## Decision

1. **A static map, in a crate of its own** (`crates/hushrun-graph`, C-147). tree-sitter's Rust
   grammar splits each `.rs` file into items: functions, `impl` blocks and their functions, traits,
   types, consts, statics, `macro_rules!` definitions and item-level macro invocations. The
   design follows the owner's `cg` (tree-sitter items, rust-analyzer's SCIP index, a walk from tests
   to what they reach). This session couldn't read cg, so the crate was written afresh to that
   design. rust-analyzer's `scip` command resolves every name it can: calls, paths, types, method
   calls, operators and derefs. An edge goes from each item to the item that defines each symbol
   that item's own code refers to.

2. **What the index can't show counts on the safe side** (C-148 to C-151):
   - A trait's method reaches the method of that name in every `impl` of the trait. A type reaches
     the head of every `impl` that names it, and the whole of each `impl` of a trait from outside
     the workspace (`Drop`, `Display`, operators, `From`), which is called with no name.
   - A function's address or a closure is an occurrence like a call. What runs with nothing naming
     it (`#[used]`, `#[no_mangle]`, `#[ctor]`, `#[distributed_slice]`, a macro from outside the
     workspace that registers code at link time) is reached by every test of a binary built from
     its file.
   - A macro reaches its definition. A workspace `macro_rules!` body, which the index doesn't
     resolve, counts every item named by each of its free names, the locals it binds left out. A
     `macro_rules!` of another crate counts its free names, read from that crate's sources. A
     procedural macro of the workspace makes the test fall back. One from another crate counts the
     whole of the module it is invoked in, except for the few crates
     [`HYGIENIC`](../../crates/hushrun-graph/src/graph.rs) names, whose output names nothing but
     absolute paths and its input. An identifier the index leaves unresolved (code `cfg` left out
     under rust-analyzer's configuration, a macro's input it couldn't expand) counts every item of
     that name that the identifier's place in a path can name.
   - A test that can start a binary of the workspace falls back: `env!("CARGO_BIN_EXE_…")`,
     `current_exe`, `cargo_bin`, and `Command::new` of anything but a literal that names no
     workspace binary and holds no path. A function marked
     `#[doc(alias = "hushrun::starts_no_workspace_binary")]` promises its own `Command::new` starts
     none, and must check that at run time: the marker is inert, needs no dependency, sits next to
     what it promises where review sees it, and never covers a binary named outright.

3. **A test's key** (C-146) is made of what its binary's key holds (its files, `env`, `scripts`),
   less the `.rs` sources of its own crate, of the workspace libraries it may link and of its
   package's other targets that the map shows it reaches nothing of. A build script's files, a
   procedural macro's and every file that isn't a `.rs` source stay in every test's key. The map
   is made from the current files at every plan and every reading after COMMAND. It is never the
   list a past run saw: a change outside what a test reached can change what it reaches (a new
   `impl`, a glob import).

4. **Only with `tests.unit: test` and `cargo nextest run`** (C-146). nextest starts each test on
   its own, and the target runner sees each start. It leaves a reused test out of the list it passes
   on, so nextest never starts it. `cargo test` starts a binary once for all its tests. It keeps
   reusing whole binaries, and the run says so once. No configuration key is added.

5. **Missing tools fall back** (C-152). When rust-analyzer is missing, fails, takes over 30
   minutes, or gives an index that lacks one of a binary's sources, every affected test has its
   binary's reads. The run says so once, naming the fix (`rustup component add rust-analyzer`).
   rust-analyzer is optional. The index is kept in `<cache dir>/units/rust-analyzer/` under the
   SHA-256 of rust-analyzer's version, its configuration (COMMAND's features and target) and every
   file of the workspace in the scope. The four newest are kept, a plan and its reading share one,
   and a run on files already indexed starts nothing.

6. **tree-sitter's C code** is a deliberate exception, as ring's, mimalloc's, SQLite's and zstd's
   are: its runtime and its generated Rust grammar are C, built with `cc` for every release
   target. syn would have been pure Rust, but the decomposition follows cg's, and tree-sitter
   parses a file with errors in it without giving up on the rest (a file that doesn't parse whole
   is one the map lacks, C-147).

## What CI pays

- **rust-analyzer** must be installed for the toolchain (`rustup component add rust-analyzer`,
  or `components = ["rust-analyzer"]` in `rust-toolchain.toml`). hushrun installs nothing. Without
  it, a CI job runs as it does today, by test binary, and its log says how to add it.
- **The index** of a tree costs `rust-analyzer scip` once: about 2 minutes 10 seconds and 70 MB on
  this workspace, on top of the build, which rust-analyzer reuses for build scripts and procedural
  macros. It is kept in hushrun's cache directory (`<cache dir>/units/rust-analyzer/`). A job
  whose cache directory doesn't survive it (a fresh runner) pays it on every run whose tree is
  new, which is every push. A rerun of the same commit pays nothing. The plan and the reading after
  COMMAND share one index.
- **The map** is built in about 10 seconds (release build), and each test's walk takes about a
  millisecond.
- What it saves is the test time of every test the change doesn't reach. A change to one adapter
  of `crates/hushrun/src` runs about 40% of the binary's unit tests instead of all of them. The
  integration tests in `crates/hushrun/tests/` start the binary, so they all still run.

## Consequences

- A change in one module of a crate runs the tests that reach it, not every test of every binary
  that links the crate. On this repository, the unit tests of `hushrun-core` reach 7 of its
  37 source files on average. Of the 751 unit tests of the `hushrun` binary, about a quarter reach
  fewer than 25 of its 394 sources. A quarter reach more than 250, mostly through the command
  line's types, whose derives reach most of the binary.
- Binary-spawning integration tests never narrow, by C-151; nor does a test of a binary built from
  a source the index lacks.
- rust-analyzer's index of this workspace takes about 2 minutes 20 seconds and 70 MB, against a
  25-minute test run. The map is built in about 10 seconds. A run whose files were already indexed
  pays only the map.
- The map leans on rust-analyzer's resolution. Where that is wrong rather than missing (a
  reference resolved to the wrong item), the map is wrong too. The audit runs (C-34) are the net,
  as for every adapter.

## Follow-ups

- **A name-based map without rust-analyzer.** Every same-named item would count, which is coarser
  but needs no tool. The owner asked that this round ship nothing that narrows without
  rust-analyzer.
- **Incremental indexing.** `rust-analyzer scip` indexes the whole workspace each time. An index
  per crate, kept by that crate's inputs, would index only what changed.
- **Finer keys.** A test's key is made of whole files. Keying by the items it reaches would let a
  change to a function it doesn't reach, in a file it does, leave it reusable.
- **The command line's types.** Derives on `cli.rs`'s types reach most of the `hushrun` binary.
  A closer reading of clap's derive would narrow the tests that build a command line.

## Alternatives considered

- **Coverage or runtime tracing:** ruled out by the owner.
- **Watching each test's reads at run time:** a compiled test reads no source.
- **Rust's `-Zunpretty=expanded`:** it would expand every macro, but it needs a nightly
  toolchain, and its output still has to be resolved.
- **A name-based map alone:** sound and fast, but every common name (`new`, `run`) reaches every
  item of that name, and it was ruled out for this round.
