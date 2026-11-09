# 0004. Git entry modes: canonicalize as git does on checkout

- **Status:** accepted
- **Date:** 2026-10-14
- **Rules:** D-3, D-9, D-13 (`docs/spec/04-source-digest.md`)

## Context

D-3 makes a regular file kind `x` when it has "any execute bit", and `f` otherwise. A
filesystem has nine permission bits, so "any" is natural there. A git tree stores a mode per
entry, and git itself only distinguishes `100644` from `100755`: when it reads a tree, it
canonicalizes a blob's mode with `(mode & 0100) ? 0755 : 0644`. Repositories created by old git
versions or other tools can still contain modes such as `100664` or `100775` (`git fsck`
reports them as `badFilemode`).

If the git adapter applied "any execute bit" to raw tree modes, a tree entry `100654` would be
`x` in revision mode, while `git checkout` writes the file as `0644`, so dir mode on that
checkout would see `f`. D-13 requires those to match.

## Decision

The git adapter canonicalizes a blob's mode exactly as git does before applying D-3: only the
owner execute bit counts. The dir adapter keeps "any execute bit", which it can't get wrong on a
clean export, because git writes only `0644` and `0755`. Both object readers and
`tools/refdigest.py` follow the same rule, and the spec states it under D-3.

## Consequences

- Revision mode, worktree mode and a checkout agree, whatever modes old tools left in a tree.
- A tree whose only change is a legacy mode spelling (`100664` → `100644`) keeps its digest,
  as git itself treats both as the same file.

## Alternatives considered

- **Any execute bit on raw tree modes.** Breaks D-13 for legacy modes.
- **Reject non-canonical modes.** Correct but unhelpful: git accepts them, and the user can't
  fix history.
