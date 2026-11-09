# F35: a tsconfig that references a sibling, and extends a package's

**Pattern.** `packages/app/tsconfig.json` has `"references": [{"path": "../lib"}]` and
`"extends": "@acme/tsconfig/base.json"`, with comments, as tsconfigs may. Its package.json names
neither `lib` nor `@acme/tsconfig`. CI runs `pnpm --filter @acme/app test`.

**Mirrors.** TypeScript monorepos with project references and a shared tsconfig package.

**Expected after Phase 5.** `app`'s scope holds `packages/lib`, which its tsconfig references, and
`packages/tsconfig`, the package it extends. Its `test` is wrapped with its scope, and the job
lists it alone: the filter picks one package.
