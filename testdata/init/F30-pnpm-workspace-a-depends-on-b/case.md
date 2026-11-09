# F30: a pnpm workspace where `a` depends on `b`

**Pattern.** `pnpm-workspace.yaml` lists `packages/*`. `@acme/a` depends on `@acme/b` with
`workspace:*`, and each has a `test` script. CI runs `pnpm -r test` itself.

**Mirrors.** pnpm monorepos, which name a sibling with `workspace:`.

**Expected after Phase 5.** Each package is a scope: `a`'s holds `packages/a` and `packages/b`,
`b`'s only `packages/b`, and both hold the root's files and CI's. Each package's `test` is wrapped
with its scope, `hushrun run --scope acme-a -- vitest run`, and the job lists both, each one
predicted to skip. The root's `test`, `pnpm -r test`, stays as it is.
