# F33: Bun workspaces

**Pattern.** Bun, with `"workspaces": ["apps/*", "libs/*"]` and a `bun.lock`. `api` depends on
`shared` with `workspace:*`. CI runs `bun run --filter '*' test`.

**Mirrors.** Bun monorepos.

**Expected after Phase 5.** Both packages are found across the two patterns, and `api`'s scope
holds `shared`'s. Each `test` is wrapped with its scope, and the job lists both.
