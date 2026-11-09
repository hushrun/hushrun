# F37: Turborepo's strict mode keeps hushrun's variables from a package's script

**Pattern.** A pnpm workspace on Turborepo 2. The root's `test` is `turbo run test`, CI runs
`pnpm test`, and turbo.json lists no variables to pass on. `@acme/a` depends on `@acme/b`.

**Mirrors.** Turborepo monorepos, such as Astro's and cal.com's: Turborepo 2's strict mode gives a
task only the variables turbo.json lists and a few of its own, so a package's wrapped script gets
neither CI's ID token nor hushrun's variables, and before Turborepo 2.7 none of GitHub's.

**Expected after Phase 5.** Each package's `test` is wrapped with its scope, and the root's stays
as it is, since it runs each package's. turbo.json gets `globalPassThroughEnv` before its `tasks`:
`ACTIONS_ID_TOKEN_REQUEST_*`, `GITHUB_*` and `HUSHRUN_*`, which turbo passes on without making them
part of a task's cache key. The job lists both packages' scripts, each predicted to skip.
