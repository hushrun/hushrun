# 0092. Programs are tied unless the run shares

- **Status:** accepted
- **Date:** 2026-11-16
- **Rules:** B-9, B-21

## Context

[ADR 0076](0076-compiled-crates-are-reused-between-worktrees.md) (decisions 10 and 11) made a
macOS program movable by default: every program that links with debug info is compiled with
`-C split-debuginfo=packed` and `-C strip=debuginfo`, so rustc runs `dsymutil` and writes a debug
bundle beside it, and the program's block can be reused in another folder.
[ADR 0091](0091-linked-outputs-between-machines.md) then let such a block leave the machine.
`programs: tied` in `build.json` was the way to keep programs as cargo builds them.

A profile of a cache miss on 2026-11-16 (Apple M5, this repository at `1baefb6`, hushrun 0.16.0,
`cargo test --workspace --no-run`, an empty store, a shim in front of the compiler timing every
call) showed where a first build's extra time goes:

| First build, one worktree | wall | CPU |
|---|---:|---:|
| no cache | 49.9 s | 386 s |
| hushrun, programs movable | 76.3 s | 543 s |
| hushrun, programs tied | 52.4 s | 382 s |

Summed over the compiler calls of each kind, against no cache, the 82 test programs took
134.7 s more when movable and 19.0 s less when tied, and the 3 compiles of the `hushrun` binary
23.7 s more against 1.5 s. `dsymutil`, once per program, is 24 s of the 26 s of extra wall time
and all of the 157 s of extra CPU. Everything else the wrapper does costs about 2.5 s.

Three worktrees one after the other, each a first build of its own target folder with the store
the one before left:

| Three worktrees | same commit | nearby commits |
|---|---:|---:|
| no cache | 154 s, 14.7 GB | 150 s, 14.6 GB |
| programs movable | 128 s, 7.8 GB | 171 s, 13.4 GB |
| programs tied | 109 s, 6.3 GB | 130 s, 6.8 GB |

Tied reuses fewer compiles, since each worktree links its own test programs (364 to 367 of 465
reused at one commit), but a link without `dsymutil` is cheap and the store holds no bundles: a
first build stores 2.7 GB against 5.1 GB. Tied was faster and smaller than movable in every case
measured, and on nearby commits movable was slower than no cache at all.

What the bundle buys is a program block that can be used somewhere else than the target folder
that built it. On one machine that never paid for itself above. Between machines it is the only
way a macOS program travels (B-24). Linux never runs `dsymutil` and is not affected.

One more fact shaped the file's format. Every hushrun up to 0.16 wrote `"programs": "movable"`
into `build.json` on every save of any setting, and no command sets `programs`: a `movable` found
in a file says nothing about what its owner chose.

## Decision

1. **A run's programs are tied unless the run shares** (B-9). When nobody chose, a program is
   movable, with its debug bundle, in a run that shares or would: the run has a relay, or its
   backend was asked and couldn't be reached, the same test that drops the debug map from proc
   macros and build scripts, so no key turns with the network. In a run that shares nothing it
   is tied: compiled as asked, nothing added, reused only in the target folder that built it.
   Set to `tied`, programs are tied in every run.
2. **One function decides**, from the setting and whether the run shares
   (`ProgramsSetting::in_a_run`). The wrapper asks it through the run's session; nothing else
   compares the setting itself. In CI and in a sandbox no person's settings are read (B-25, B-31)
   and the same function runs with the setting not chosen.
3. **The setting has two values in memory, not chosen and `tied`**, and no way to hold "a bundle
   in a run that shares nothing". In `build.json` and in a run's `session.json` only `tied` is
   written. A written `movable` reads as not chosen, like a missing field (B-21).
4. **The file's version stays 1 and gets no new value.** An older hushrun on the same machine
   reads the same `build.json`, and refuses a version or a value it doesn't know. It reads a file
   with no `programs` as its own default.
5. **`hushrun build status` says the rule in words**: `follow sharing: …` or `tied: …`; its JSON
   has `"programs": "follow"` or `"tied"`.

## Alternatives considered

- **Keep movable and make `dsymutil` cheaper** (less debug info for test programs, more threads).
  It changes what people debug with, or still pays per program for a reuse that didn't pay back.
- **Make the bundle later**: link as cargo would, and run `dsymutil` when a block is first reused
  elsewhere or uploaded. The program's UUID and bytes are those of the link, so the stored
  program would have to be linked for a bundle anyway; not tried.
- **A new value in the file** (`auto`, `follow`). Older versions refuse the file, which turns
  reuse off for them with an error.
- **Treat a written `movable` as a choice.** Then every person whose file was ever saved would
  keep paying for bundles they never asked for.

## Consequences

- A person who shares nothing gets first builds about as fast as with no cache on macOS, and a
  store about half the size. Programs are linked again in each worktree's target folder.
- A run that shares still pays `dsymutil` once per program: that is what lets the block travel.
- Nobody can ask for bundles in a run that shares nothing. A hand-written `"programs":
  "movable"` now means "follow sharing".
- A tied program's block is reused only in the target folder that built it (B-24's `tied`).
- A machine that turns sharing on or off builds its programs once more: movable and tied
  programs never share a key (B-5), as before.
- An older hushrun on the same machine reads a file this one saved as its own default, movable.
- Movable blocks already in a store are found again only by runs that share; the others age out
  (B-14).
