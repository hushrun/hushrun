# 0051. A workspace package is a scope

- **Status:** accepted
- **Date:** 2026-11-05
- **Rules:** C-14, C-15, C-22
- **Amends:** [0037](0037-any-language-from-cis-own-files.md): item 4, under which a JavaScript
  workspace at the root kept one scope; [0036](0036-agents-told-before-they-push.md), whose
  `hushrun ready` read the root's package.json alone

## Context

After ADR 0050, 201 of the 823 test jobs in the survey of 100 repositories' CI skip in CI, but only
15 of its 188 JavaScript test jobs do, and npm is init's main path. Three things stopped them:

- **A root script that only fans out.** A monorepo's root `test` is `pnpm -r test` or `turbo run
  test`, which runs each package's own `test`. init read the root's package.json alone: it left the
  root's script, "it only runs other scripts or the package manager", or wrapped the whole of a
  `turbo run` under one key, and never read the packages' scripts.
- **One scope for the repository.** A workspace file, `pnpm-workspace.yaml`, `package.json`'s
  `workspaces`, `lerna.json` or `turbo.json`, kept the repository in one scope (ADR 0037, item 4):
  any change ran every check.
- **CI calls scripts outside init's list** of ten names, such as `test:types` or `test-unit`. This
  ADR doesn't change that list.

A package manager already knows a workspace's graph: which directories are its packages, and which
of them each depends on. The scope a package's checks need is that graph, with what its files
name by relative path, which ADR 0037 reads already.

## Decision

1. **The packages** are the directories the root lists, each holding a `package.json` git tracks,
   outside `node_modules`: `pnpm-workspace.yaml`'s `packages`; else `package.json`'s `workspaces`,
   as a list or as `{packages: […]}` (npm, Yarn, Bun); else `lerna.json`'s `packages`, or Lerna's
   own default, `packages/*`. A pattern is read as the tool reads it, `*` within a directory, `**`
   across them, `{a,b}` written out and `!` leaving out; pnpm also leaves out what is under
   `test`, `tests`, `bower_components` and `node_modules`, as pnpm does. The root is no package
   here: its checks take the scope `all`. A `pnpm-workspace.yaml` of settings alone, with no
   `packages`, makes no workspace.
2. **What a package depends on** is each package of the workspace it names in `dependencies`,
   `devDependencies`, `peerDependencies` or `optionalDependencies`: with `workspace:`, with a range
   or a tag, which the package beside it may satisfy, with `catalog:`, or as the target of an
   `npm:` alias. Counting a range the registry serves widens the scope only, the safe direction. A
   path, `file:` or `../`, is read as ADR 0037 reads a path. Then:
   - **Turborepo:** a task of one package that needs another's, `"web#test": {"dependsOn":
     ["api#build"]}`, makes `web` depend on `api`, and a task with no package does for every
     package; `globalDependencies` and `$TURBO_ROOT$` inputs join every scope.
   - **tsconfigs:** a package's `extends` that names another package, such as
     `@acme/tsconfig/base.json`, makes it depend on that one; where a root tsconfig's `paths` map
     imports, every scope holds it, a package's whole directory when the target is in one.
3. **A package with checks is a scope**, named after its package's name, else its directory, made a
   name. It holds its directory and those of the packages it depends on, transitively; what its
   files and theirs name by relative path, such as `../../configs/vitest.shared.ts` or a
   tsconfig's `references`; the root's files and CI's, as every scope does (ADR 0037); and what
   item 2 says every package reads. A check outside every package takes its directory's project,
   as before. Any other file that ties projects together still keeps one scope: `nx.json`, whose
   graph follows the imports in the code, `go.work`, Gradle's settings, a Cargo or uv workspace.
4. **What hushrun can't read keeps one scope, and the plan says why** (C-14): a `package.json` that
   isn't JSON, a pattern it can't read or that leaves the repository, two packages of one name, a
   package of the workspace named from a protocol hushrun can't place, such as
   `"@acme/core": "github:acme/core"`, or a dependency `workspace:` names that no package has. The
   packages' scripts are still wrapped then, as the root's are in a repository of one scope.
5. **Each package's scripts** are read and wrapped as the root's are, by the same names and rules
   (C-15), in the package's own `package.json`, with its scope: `"test": "hushrun run --scope
   acme-a -- vitest run"`. Its check is named after the script, and its key holds its directory and
   its scope (C-9), so two packages' `test` never share a pass. The root's `package.json` gets the
   dependency, the backend and the hook's script, as before. Yarn from 2 on gives a package's
   scripts only its own dependencies' programs, so there each wrapped package gets `hushrun` among
   its devDependencies too.
6. **A script that runs each package's script stays as it is**, with its reason, and the plan says
   how it skips now: `package.json "test" runs each package's test, which init wraps there, so it
   skips package by package`. Such a script is `pnpm -r`, `--filter` or `-C`; `npm --workspaces`,
   `-w` or `--prefix`; `yarn workspaces run`, `yarn workspaces foreach` and `yarn workspace`; `bun
   --filter`; `lerna run`; `turbo run`, or `turbo <task>`, `pkg#task` included; and `nx run-many`,
   `nx affected` and `nx run`. Through a tool whose remote cache keeps its results, ADR 0050's step
   aside holds.
7. **A CI job starts** the scripts its lines name, those they start in turn, and the packages'
   scripts that a script that fans out, or a filter, picks, by name, directory, `...` for
   dependencies and dependents, `^` and `!`; a filter on commits, such as `[origin/main]`, picks
   every package. A job that starts a wrapped script gets the ID token and its identity, as any
   job that runs a check does, and the plan lists the script under the job with `package`, its
   package.json, when it is a package's. CI's own line needs no edit: `pnpm test` at the root now
   skips package by package.
8. **`ready` and the hook read the packages' wrapped scripts too** (amends 0036), each run in its
   package's directory, `cd packages/a && pnpm run test`, under the same rules as the root's.
9. **Turborepo passes hushrun's variables on.** Turborepo's strict mode, its default from
   Turborepo 2, gives a task only the variables turbo.json lists and a few of its own, and before
   Turborepo 2.7 none of GitHub's: a package's wrapped script that `turbo` starts in CI would get
   no ID token, nor hushrun's variables, and run every time. When a job starts a wrapped script
   through `turbo`, without `--env-mode=loose`, and the root's `turbo.json` or `turbo.jsonc` has
   no `"envMode": "loose"`, init adds to its `globalPassThroughEnv` what it doesn't pass yet of
   `ACTIONS_ID_TOKEN_REQUEST_*` and `GITHUB_*`, for GitHub's jobs, `GITLAB_CI` and `CI_*`, for
   GitLab's, and `HUSHRUN_*`: after the list's own entries, or as a new list after `globalEnv`,
   else before `tasks`. turbo passes such variables on without making them part of a task's cache
   key. Comments and trailing commas, which Turborepo reads, stay, each comment beside its value
   (C-15), and the list is laid out as Prettier and Biome would within the repository's line
   width, on one line when it fits, so that a format check in CI still passes. A Turborepo 1 file with no list gets none, since a list there makes every task strict.
   A file init can't read or edit is a change by hand, and the job's scripts that `turbo` runs are
   predicted to run, with that reason (C-14).

## Consequences

Measured on 2026-11-05.

- **On the survey's 100 repositories, 221 of 823 test jobs skip in CI,** 20 more than after ADR
  0050, and 3 more run for their reports: jobs that start a package's script through a filter, a
  root script that runs each package's, or a line in the package's directory. 28 of the 188
  JavaScript test jobs skip, from 15, short of the plan's 40: 77 of the rest run a script outside
  init's ten names, which this ADR doesn't change, and 38 a line no package manager starts. Every
  package scope holds the workspace packages the corpus harness's own reading finds it depends on
  (A10, 59 scopes in 12 repositories), and no job that skipped stops (A8). Five of the
  repositories, Astro, cal.com, Prisma, Tailwind CSS and Next.js, run packages' scripts through
  `turbo` in CI: their turbo.json gets hushrun's variables (item 9), without which those jobs
  would run every time.
- **Older pieces stay safe.** An older hushrun runs a package's `hushrun run --scope a -- …` as any
  run with a scope. A server sees nothing new: a package's result is a result of its scope.
- **Limits.**
  - An import of a sibling that its package.json doesn't name, which only hoisting lets resolve,
    isn't seen: its package's checks can skip on a change to that sibling. pnpm's own layout
    refuses such an import by default; npm's and Yarn 1's let it resolve.
  - A package.json outside the workspace's patterns isn't read, though CI runs its script in its
    directory, such as a fixture's.
  - A filter on commits picks every package, and so does a filter hushrun can't read: CI's job
    then lists more of the packages' scripts than it runs, which costs nothing.
  - A package's own `turbo.json` and tsconfigs count; a Jest or Vitest config that imports a
    sibling by name counts only when its package.json names it.
  - Nx keeps one scope: its graph follows imports, which hushrun doesn't read.
  - Turborepo's mode is read from turbo.json and the `turbo` command, not from a `TURBO_ENV_MODE`
    CI sets, which makes init's list unneeded, nor from a Turborepo 1 task's own `passThroughEnv`,
    which makes that task strict without it: its scripts then run every time though the plan says
    they skip.

## Alternatives considered

- **One check for the whole fan-out**, `hushrun run -- turbo run test`, as before. Any change in
  any package ran every package's tests, and the root's key couldn't tell them apart.
- **Reading every import.** Costly, and the graph a package manager links is the one its package
  files declare: a package can't import what it doesn't depend on in pnpm.
- **hushrun in every package's devDependencies.** npm, pnpm, Yarn 1 and Bun give a package's scripts
  the root's programs; only Yarn from 2 on needs it, and a lockfile change everywhere for nothing
  costs review.
- **`envMode: "loose"` for Turborepo**, or `--env-mode=loose` on the root's script: it gives
  every task every variable, which changes what the repository's tasks see, and hides a variable
  missing from `env` from Turborepo's own hashing. The list adds only hushrun's.
- **The variables in `globalEnv`**, which every task's cache key holds: the ID token changes on
  every run, and turbo's cache would never hit again.
- **Inferring the scope from the directory at run time.** It would change what an existing
  `hushrun run` without `--scope` does in a repository of several scopes; naming the scope in the
  script keeps every run's key what it was.
