# F31: npm `workspaces` as an object

**Pattern.** The root's package.json says `"workspaces": {"packages": ["packages/*"]}`, the form
Yarn 1 brought for `nohoist`, which npm and Bun read too. `web` depends on `core` with a range
`core`'s version satisfies. CI runs `npm test --workspaces`.

**Mirrors.** Repositories that moved from Yarn 1 to npm and kept the object.

**Expected after Phase 5.** Both packages are found. `web`'s scope holds `core`'s directory: a
range a workspace package satisfies links it as `workspace:` does. Each `test` is wrapped with its
scope, and the job lists both.
