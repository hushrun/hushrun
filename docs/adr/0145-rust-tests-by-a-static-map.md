# 0145. Rust tests one by one, by a static map of what each reaches

- **Status:** proposed; built; amended (amendment 1, 2026-11-27)
- **Date:** 2026-11-25
- **Rules:** C-146 to C-152 added; C-39 amended; relies on ADR 0068, 0069, 0080, 0095,
  0096
- **Amended by:** [0162](0162-rust-tests-listed-before-the-run.md): the plan lists each test
  binary's tests, so a machine with no store of its own, a CI runner, reuses them one by one

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
  of `crates/hushrun/src` runs about 7% of the binary's unit tests instead of all of them (52 of
  751 for `units/gradle.rs`, the 19 that fall back included), and one to `units/mod.rs` about
  21%. The integration tests in `crates/hushrun/tests/` start the binary, so they all still run.

## Consequences

- A change in one module of a crate runs the tests that reach it, not every test of every binary
  that links the crate. On this repository, the unit tests of `hushrun-core` reach 7 of its
  37 source files on average. Of the 732 unit tests of the `hushrun` binary that narrow (of 751),
  three quarters reach 20 or fewer of the 396 files the binary is built from, and the widest
  reaches 73.
- The map was checked at run time: with a `panic!` at the head of every function of seven files
  (`units/gradle.rs`, `units/cargo.rs`, `units/mod.rs`, `units/store.rs`, `results.rs`,
  `hushrun-core`'s `glob.rs` and `names.rs`), in two builds, each of the 824 unit tests the map
  said reach none of that build's files passed, and 213 of those that reach them failed.
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
- **Lists a repository extends.** `HYGIENIC` (procedural macro crates whose output names only
  absolute paths and its input) and `OWN_METHODS` (crates whose `macro_rules!` bodies call methods
  only on their own types) are hushrun's own trust assumptions, in code and in C-150. A
  repository's macro that isn't on them counts the whole of its module, which is sound. Letting a
  repository add crates to them is left for later.
- **Receivers' types.** A call to a trait's default method on a concrete type resolves to the
  trait, so it counts every `impl` that overrides the method. rust-analyzer's index says no
  receiver's type; one that did would let such a call count the one `impl`.

## Alternatives considered

- **Coverage or runtime tracing:** ruled out by the owner.
- **Watching each test's reads at run time:** a compiled test reads no source.
- **Rust's `-Zunpretty=expanded`:** it would expand every macro, but it needs a nightly
  toolchain, and its output still has to be resolved.
- **A name-based map alone:** sound and fast, but every common name (`new`, `run`) reaches every
  item of that name, and it was ruled out for this round.

## Amendment 1 (2026-11-27): files read at compile time

A test that reads a source as text was reused after that source changed. A repository's test
did `include_str!("../src/api/mod.rs")` and checked the routes it lists. Its map reached nothing
of `api/mod.rs`, which held no item the test called, so the file left the test's list. A change
to it that broke the test kept the test's key, and the test was reused.

`include_str!`, `include_bytes!` and `include!` read a file when the crate compiles. rustc's
dep-info names it, so it was in the binary's reads, and in the files a test's list may leave out.
The map now counts it as reached, two ways (C-147):

1. **The macro.** An item whose own bytes hold one of the three reaches the whole file it names.
   The input is a string literal, resolved from the folder of the item's file, as rustc does. The
   macro is found by its name and its `!` wherever it stands, nested in another macro's input or
   in a `macro_rules!` body, where tree-sitter sees only tokens. An input that names no source
   changes nothing: one whose last string literals make a name that doesn't end in `.rs`
   (`concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/x.json")`), whose file already stays in
   every list, and one under `OUT_DIR`, which a build script made and its output keys. Any other
   input makes the item fall back. It can name any file, the map can't tell which, and a skip
   counts as a pass.
2. **The dep-info.** Each `.rs` file a crate's dep-info names that isn't one of that crate's
   modules stays in the list of every test of a binary built from that crate. The macro rule
   doesn't see every read: a macro from outside the workspace can include a file its input
   names. A file that isn't a module of the crate that read it was read as text or included,
   whoever read it. It stays whole, for every test of the binary, which is sound and coarse. A
   crate whose root the map lacks keeps every file its dep-info names. A file that is a module of
   the crate that includes it is the first rule's alone.

What this costs this repository is nothing: none of its tests includes a `.rs` file.
