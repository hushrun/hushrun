# F34: a script that names a shared config outside its package

**Pattern.** `packages/a`'s `test` is `vitest run --config ../../configs/vitest.shared.ts`: a file
in a directory that is no package. `packages/b` reads nothing outside itself. CI runs `pnpm -r
test`.

**Mirrors.** Monorepos that share one Vitest or Jest config.

**Expected after Phase 5.** `a`'s scope holds `configs/vitest.shared.ts`, so a change to it runs
`a`'s test; `b`'s doesn't. Each `test` is wrapped with its scope.
