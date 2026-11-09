# 0001. Source digest: a flat, sorted manifest of each scope's files

- **Status:** accepted
- **Date:** 2026-10-14
- **Rules:** D-1 … D-13 (`docs/spec/04-source-digest.md`)

## Context

hushrun reuses a validation result wherever the same content appears again: in another
commit, on another branch, in another stage. It needs an identifier for "this content" that:

- is the same for the same files, whatever the commit, branch or VCS;
- covers only a scope's files in a monorepo, chosen by include and exclude globs;
- captures everything that changes what a build or a deploy does: bytes, the execute bit,
  symbolic links and submodule pins;
- anyone can recompute with standard tools, without trusting hushrun.

ci-promote used the git tree hash for this. It works for one repository with one deployable,
but it is git-specific, it depends on the object format (`sha1` or `sha256`), and it can't
express a scope that is not a single directory.

## Decision

A scope's source digest is the SHA-256 of a canonical manifest (D-4):

```
manifest = "hushrun-source-v1\n" || entry_1 || … || entry_n
entry    = kind || " " || content_hash || " " || path || "\0"
```

- `kind` is one of `f`, `x`, `l`, `g` (D-3); `content_hash` is the SHA-256 of the bytes as the
  VCS stores them, with no line-ending, keyword or filter conversion.
- Paths are relative, `/`-separated, NFC-normalized UTF-8, unique, and sorted by their bytes
  (D-2, D-4). The encoding is injective because `kind` and `content_hash` have fixed lengths and
  paths contain no NUL.
- Scope filtering happens before hashing, with one glob grammar on every OS (D-7).
- The digest is `sha256:<hex>` of the manifest bytes, so `hushrun digest --manifest-out m`
  followed by `sha256sum m` checks any digest (D-5).
- The canonicalization name is part of the manifest and of every event. Any change becomes
  `hushrun-source-v2`, and the planner never matches digests across canonicalizations (D-8).
- `tools/refdigest.py` is an independent implementation, written from the spec rather than from
  the Rust code, and the golden vectors run against both.

## Consequences

- Git, a clean directory export and other VCSs produce the same digest for the same content
  (D-13), so evidence moves across branches, commits and tools.
- A scope that leaves out a deployment input can skip a redeploy that was needed. The default
  scope covers everything, and `doctor` warns about deploy workflows outside a scope.
- Hashing is linear in content size. Content-addressed caches (blob id → SHA-256, and a stat
  cache for directories) keep repeated digests fast (plan §12.6).
- `hushrun-source-v1` is frozen once M1 ships.

## Alternatives considered

- **Git tree hashes.** Git-specific, depend on the object format, and can't express glob
  scopes.
- **in-toto `dirHash` (Go's `h1`).** Ignores file modes and symbolic links, so an executable
  bit flip or a retargeted link would keep the digest.
- **A Merkle tree per directory.** Cheaper incremental updates, but scopes cut across
  directories, and the flat manifest takes about ten lines in any language.
