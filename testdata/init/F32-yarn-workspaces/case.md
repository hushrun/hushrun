# F32: Yarn workspaces

**Pattern.** Yarn 4, with `"workspaces": ["packages/*"]`. `@acme/app` depends on `@acme/ui` with
`workspace:^`. CI runs `yarn workspaces foreach -A run test`.

**Mirrors.** Yarn 2 and later, whose packages' scripts find only the programs of their own
dependencies.

**Expected after Phase 5.** Both packages are found, and `app`'s scope holds `ui`'s. Each `test`
is wrapped with its scope, and each package gets `hushrun` in its devDependencies: Yarn doesn't
give a package's scripts the root's programs.
