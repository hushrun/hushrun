# 0077. Portable build blocks

- **Status:** accepted
- **Date:** 2026-11-12
- **Rules:** B-6, B-8, B-9, B-10, B-11, B-14, B-16, B-17, B-24

## Context

[ADR 0076](0076-compiled-crates-are-reused-between-worktrees.md) reuses a crate compiled from the
same inputs in every worktree on one machine. Its keys named the machine: the compiler by its
path, size and dates, the cargo home and the toolchain by their absolute paths, a proc macro by a
key that held the linker. So two machines with the same toolchain and sources never shared a key,
and nothing said whether a block's bytes held a folder of the machine that built it. Sharing
blocks between machines (a laptop and CI, two CI jobs, an agent's sandbox) needs both: keys that
are equal where the bytes are, and one answer to "may this block leave the machine".

A spike of that, tested on two Linux machines (two distributions, two users, two homes, two
linkers, each with its own toolchain install) and on two macOS setups, found every library
byte-identical under equal keys, with one exception that broke builds: when only one machine has
rustup's `rust-src`, rustc reads the text of the standard library's macros from it, and
`format_args!` places spans by that text, so `unreachable!("…")`, `unimplemented!("…")`, `dbg!`,
`writeln!(f)` and `println!()` give a crate another crate hash. A library reused from one machine
then failed to load against the proc macro built on the other (E0463), and some libraries
differed under equal keys with no path in them.

## Decision

- **The compiler by content** (B-17): `rustc -vV`, and the SHA-256 of the compiler, its libraries
  and the standard library's files. No path, date or variable. A memo keyed by path and dates
  keeps it from being computed more than once per install.
- **Two named bases** (B-8): the cargo home and the toolchain are recorded by name in keys and
  hints, like roots, but never tie a block. Every compile gets `--remap-path-prefix` for the
  cargo home, to `/hushrun/cargo` (B-9).
- **The machine's folders are searched for** (B-10): every output is searched for the cargo home,
  the rustup home and the home folder, in the pass that already searches for the roots.
- **One function decides whether a block may leave** (B-24), with named reasons, recorded in the
  block and in every log line. Linked outputs (programs, proc macros, build-script binaries) never
  may yet: their link environment has no content identity.
- **A proc macro is named by what it was built from and by its metadata** (B-6): its key body
  without its place, linker, SDK and `dsymutil`, and the SHA-256 of the crate metadata rustc put
  in the linked file, which holds the crate hash a dependent is compiled against and which a
  linker places without writing. A macro that came out different for any reason gives its
  dependents another key, never a library that fails to load.
- **No compile sees the toolchain's sources** (B-9, B-17): where the toolchain holds
  `lib/rustlib/src` or `lib/rustlib/rustc-src`, every compile the wrapper handles gets
  `--sysroot` naming a view of the toolchain that holds only its target folders, as links. It is
  made whole or not at all, checked each time it is found, made as `hushrun run` begins, and
  swept like unused blocks (B-14). A link through it is judged by the toolchain's file (B-11).
- **What reaches only some compiles stays out of the others' keys** (B-5, B-6): a library's key
  holds neither the linker's variables nor the SDK, nor `SOURCE_DATE_EPOCH` unless it expands a
  proc macro (rustc never reads it; a macro runs inside rustc and may), nor the static libraries
  in its native folders when its `.rlib` bundled none of them.
- **A machine's folder is found where it ends a name** (B-10): a home `/root` is not found in
  `/rootfs` or `/roots.rs`. The byte before is not looked at, since a flag (`-I/root/include`) or
  the length crate metadata writes before a string can stand there.
- **The audit can be set for a run** (B-16): `HUSHRUN_BUILD_AUDIT=every` for replays that compare
  machines, `none` for tests; a run with `none` says so.

## Consequences

- Every local key changes once; blocks stored before are not reused and expire.
- A person with `rust-src` sees, for the compiles the wrapper handles, diagnostics that point
  into the standard library as `/rustc/<commit>/…` with no source line, and debug info that names
  its files that way. Rerunning a failed compile as asked would bring the notes back at the cost of
  a second compile on every failure and of holding diagnostics until rustc exits; that is left to
  the owner.
- Nothing leaves a machine yet. This is what a later server and relay need: the blocks that may
  leave, and keys that agree between machines.
- Lost on purpose, for now: the C-bundling crates whose objects hold absolute paths (hushrun
  would have to change the C compiler's flags, and only machines with the same C compiler would
  gain), and every linked output between machines with other linkers.
