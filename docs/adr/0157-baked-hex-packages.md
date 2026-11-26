# 0157. Baked packages for Hex: a seed into Hex's cache

- **Status:** proposed; being built
- **Date:** 2026-11-26
- **Rules:** K-10 (the `hex` part's paragraph) and K-50 (a seed) new
  (`docs/spec/11-baked-packages.md`); K-1 to K-8, K-11, K-12, K-18 and K-19 read

## Context

A package store (ADR 0149) gives a sandbox the downloads of its package managers, so an install
needs no registry for what the store holds. An Elixir project's dependencies come from Hex:
`mix deps.get` downloads each package's tarball into Hex's cache, `$HEX_HOME/packages/hexpm/`,
checks it against `mix.lock` and unpacks it into `deps/`; `mix deps.compile` then builds it. In a
fresh sandbox both run from nothing. The compile is what an Elixir sandbox waits for, and this ADR
doesn't take it away: it takes away the downloads.

Hex reads no second, read-only cache (no mechanism A), and a mirror (`HEX_MIRROR_URL`) replaces
Hex's repository rather than coming before it, so a store that lacks a package would fail an
install that passes today (no mechanism B). That leaves a seed.

## Decision

1. **A `hex` part**, read by a seed (K-10's mechanism C). It holds `packages/hexpm/*.tar` and
   nothing else of Hex's cache: not `cache.bin` (the registry cache, in Hex's own format, which
   K-5 never bakes), not another repository's folder.
2. **Verified by the lockfile.** A row's `locked` is `mix.lock`'s outer checksum, the SHA-256 of
   the tarball. A run puts a tarball into the run's Hex cache only when its bytes are that hash,
   and Hex checks it again itself. An unsigned store is read by any run, CI included.
3. **The smallest `mix.lock` reader that serves**: one entry a line, as Mix writes it, and of an
   entry only a `:hex` tuple's eight fields. Any other form gives no hashes, and the part is left
   out with a line, never guessed.
4. **The bake's fetch leaves the checkout alone.** `mix deps.get` runs with `HEX_HOME`,
   `MIX_DEPS_PATH` and `MIX_BUILD_PATH` at folders of the bake's own, discarded after it. So
   `deps/` is discarded, the checkout's `deps/` and `_build` are as they were, and no `_build` is
   ever baked (ADR 0122: a copied `_build` holds another checkout's paths).
5. **The seed pnpm and npm have** (ADR 0150, `seed.rs`), written down once as K-50: never over a
   file that is there; each file hashed as it is put, checked against its row and the lockfile,
   and renamed in; a stamp of the part (`.hushrun-packages-<part identity>`) once nothing was left
   out, so the next seed costs one `stat`. Only `hushrun packages exec`, `env` and `restore` seed:
   `hushrun run` gives COMMAND nothing, since by then the install is done.
6. **Links, for a part that allows them.** `Part::links` (false unless a part says otherwise) lets
   the seed put a hard link to the store's file, where the file system allows one, and a copy
   where it doesn't; the bytes checked are read from the link. Hex replaces a cached file rather
   than writing into it, so `hex` links; pnpm and npm still copy.
7. **Nothing is set.** hushrun never names `HEX_HOME`, a variable of Mix's, or `ERL_LIBS` (the
   ExUnit adapter's). The seed finds the cache Hex would use as Hex 2.x does: `HEX_HOME`; else,
   with `MIX_XDG` `1` or `true`, `hex` under `XDG_CACHE_HOME` or `~/.cache`; else `~/.hex`.

The part fits the trait #74 merged (`Reader::Seed`, `Part::seed_into`, `Part::admit`) and adds
two methods besides `links`: `Part::locks`, the lockfiles a part reads, or an error for one in a
form it doesn't read, which leaves the part out of a bake and a seed; and `Part::pinned`, a row's
`locked` from those lockfiles, for a part whose files' paths don't say their hash (npm's do, so
`Part::locked` serves it). #76's cargo part can take the same three.

## What was confirmed by running the tool

On 2026-11-26, in a Linux container, with Hex 2.7.0, Elixir 1.18.4 and Erlang/OTP 27.3.4 (CI's),
against hex.pm and against a repository of the tests' own (`mix hex.build`, `mix hex.registry
build`, served on 127.0.0.1, logging every path):

- **A seeded tarball is never downloaded.** With `HEX_HOME` holding only
  `packages/hexpm/<name>-<version>.tar`, `mix deps.get` asks for the packages' registry records
  (`/packages/<name>`) and no tarball, and unpacks `deps/`. With an empty `HEX_HOME` it asks for
  both.
- **The outer checksum is the tarball's SHA-256**, and Hex checks a cached tarball against it:
  one byte changed, same size, Hex says the checksum differs, downloads the package again and
  writes a new file. It writes a new inode: a hard link to the store's file is left as it was.
- **No registry, no install.** With the network off, a seeded `HEX_HOME` fails ("Unknown package
  … in lockfile"): Hex needs each package's signed registry record, which it keeps only in
  `cache.bin`. With `HEX_OFFLINE=1` (an offline switch, which hushrun never sets) and no
  `cache.bin`, it fails too. So the part saves every tarball download, and a sandbox still needs
  Hex's repository for the records.
- **`mix deps.get` runs no dependency's code.** A package whose `mix.exs` and compile each write a
  file: neither is written by `mix deps.get`, both by `mix deps.compile`.
- **Where Hex keeps packages** (`mix hex.config cache_home`): `HEX_HOME`; `MIX_XDG=1` or `true`
  gives `$XDG_CACHE_HOME/hex` or `~/.cache/hex`; other values of `MIX_XDG` don't count;
  `HEX_CACHE_HOME` isn't read.
- **`MIX_DEPS_PATH`** moves `deps/` out of the checkout, an umbrella's included; a plain `mix
  deps.get` still makes `_build`, hence `MIX_BUILD_PATH` too.

Not confirmed: Hex 1.x and Elixir before 1.14; macOS and Windows; a private organization's
packages (left out: they are cached under `packages/hexpm:<org>/`).

## Considered and dropped

- **A mirror** (`HEX_MIRROR_URL` at the part, mechanism B): Hex takes everything from a mirror,
  so a package the store lacks would fail, and the registry records would have to be baked.
- **Baking `cache.bin`** so an install works offline: it is Hex's own file about its use of the
  cache, and it is read only with `HEX_OFFLINE`, an offline switch.
- **A seed of its own** (this branch's first version, before #74 merged): the same rules as
  `seed.rs`, twice.
- **Linking for every part:** pnpm hard-links its store's files into `node_modules`, where a
  person's edit would reach the store's file; a part links only when its tool never writes into
  its cache.
- **Baking `deps/` or `_build`**: `deps/` is the checkout's, and `_build` holds paths.

## Consequences

- An Elixir sandbox with a store runs `hushrun packages restore` (or `hushrun packages exec --
  mix deps.get`) in its setup, and `mix deps.get` downloads registry records only. `hushrun run`
  seeds nothing.
- `mix deps.compile` still runs every time.
- The ExUnit adapter's keys don't move: a seeded install and a network install give the same
  reads and the same reuse (`units_exunit.rs`).
