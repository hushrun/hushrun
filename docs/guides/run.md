# Skipping checks this code already passed

`hushrun run -- COMMAND` runs COMMAND unless this exact code already passed it. Put it in front
of a check once, and the check skips the same way on your laptop, in CI and in an agent's
sandbox. `npx hushrun init` puts it there for you.

## In a minute

In the repository:

```bash
npx hushrun init
```

It works in any language. init finds the checks among `package.json`'s scripts (`test`, `lint`,
`typecheck` and the like) and in CI's own files, signs you in through your browser, and shows what
it will change before it changes anything:

```text
hushrun init will change:
  package.json
    "lint": "eslint ." → "hushrun run -- eslint ."
    "test": "vitest" → "hushrun run -- vitest run" (vitest alone watches on a terminal)
    "hushrun": {"backend": "https://api.hushrun.com/o/acme"}, where CI, teammates and agents share results
    devDependencies: "hushrun": "^0.1.0"
  .github/workflows/ci.yml
    job test: adds id-token: write to its permissions
  acme
    connects github.com/acme/shop, so that its CI records results with its own ID token
    adds github.com/acme/shop to acme's dashboard, unless it is there already

Apply? [Y/n]
```

In a repository with no `package.json` at its root, in Rust, Go, Python, Java, Kotlin, Ruby, PHP,
.NET, Elixir or Swift, or with `make`, `just` or `task`, the checks are the ones CI runs
([below](#in-any-language)).

Each check's name is its script's, made a name where it has to be: `test:unit`'s check is
`test-unit`, and `format:check`'s `format-check`. Once you agree, init makes exactly these
edits, keeping the rest of each file as it was, installs hushrun with your package manager, lets
the repository's CI record its results, makes sure your org has the repository's ledger for when
you deploy with hushrun (an org admin's init makes it; anyone's finds it), and runs a check twice:

```text
$ npm run lint
hushrun: lint running: no pass yet for this exact code
  … eslint's output …
hushrun: lint passed in 6s; it won't run again on this code
$ npm run lint
hushrun: lint skipped: this exact code passed just now (saves 6s)
```

Then commit and push: CI records its first results. From then on a job skips what this exact code
already passed on a laptop, in an agent's sandbox or in that job; each variant of a job with several
skips only on its own pass. A coding agent runs `npx hushrun init --yes`, which
applies without asking; `--dry-run` shows the changes and makes none.

init also sets up coding agents: a skill, a block in `AGENTS.md`, and a pre-push hook that says
what CI would run again ([the agents guide](agents.md#4-tell-the-agents)). `npx hushrun ready` says
the same whenever you ask.

### In any language

CI's workflows and GitLab's pipeline already list your checks, so `npx hushrun init` reads them,
whatever the language, and needs no second list. It wraps the commands it knows, where they are:

```text
hushrun init will change:
  .github/workflows/ci.yml
    job test: cargo test --locked → hushrun run test -- cargo test --locked
    job test: installs hushrun first
    job test: adds id-token: write to its permissions
  hushrun.yaml
    a new file: repository github.com/acme/shop, its URL on hushrun, and the key that verifies hushrun's log
    backend: https://api.hushrun.com/o/acme, where CI, teammates and agents share results
```

`backend:` in `hushrun.yaml` does what `"hushrun": {"backend": …}` does in `package.json`, so there
is no variable to set. Every other byte of the file stays as it was.

A command stays as it is, and the plan says why, when it can't skip safely: one whose answer changes
with more than the code, such as `cargo deny`; one that writes a report a later step may read. Two
jobs of one command become two checks, since a pass on one system says nothing about another.

To keep init off a line it would wrap, say a test against a service whose answer changes while
your code doesn't, put a comment above its step, with why:

```yaml
      # hushrun: leave, public-good Sigstore changes while the code doesn't
      - run: cargo test --locked --features it --test sigstore
```

At the end of the `run:` line works too (`- run: cargo test # hushrun: leave`), and so does a
comment line right above one command of a `run: |` block. init lists the line under what it leaves,
with your reason, and gives its job nothing for it; the next init leaves it too.

A line whose words CI or the shell fills in is wrapped when each is safe to record, such as
`pytest ${{ matrix.args }}`, `${{ github.sha }}`, or `go test $PKGS` with `PKGS` set in the job's
`env`. Its key is the words as they ran, so each value has passes of its own. A word that may hold a
secret leaves the line, and the plan says why: `${{ secrets.X }}`, `github.token`, a variable set
from either, one an earlier step writes from a command's output, or one nothing in the file sets.
So does `$(…)`. A line that sets variables first, `GOARCH=386 go test ./...`, becomes `hushrun run
test -- env GOARCH=386 go test ./...`, so that `GOARCH=386` and `GOARCH=amd64` never share a pass, in
a bash step: on Linux and macOS, or with `shell: bash`. `hushrun ready` lists such a check as `ci`,
`its command takes values only CI knows`, since only the run can tell its key.

A cargo line whose job sets `RUSTFLAGS` or `RUSTDOCFLAGS` gets them in the line too, `hushrun run
rustdoc -- env 'RUSTDOCFLAGS=-D warnings' cargo doc --no-deps`: rustdoc without `-D warnings` passes
on a broken link, and such a pass must never skip CI's run. Another variable your own tests read,
such as one that says which servers to test against, isn't something init can know: put it in the
line yourself, `env NAME=value` after the `--`, so that it is part of what a pass is for. A command
folded over lines in a `run: >` block is one command, as YAML gives it to the shell, and is wrapped
in front of its first line.

init also reads the composite actions in `.github/` that a job uses with `uses: ./…`, and those they
use. It wraps a check in the action's file, once, and every job that runs it gets hushrun's setup
above its step that uses the action, its ID token and which variant it is, a nightly's too. GitHub
names the calling job in the action's steps, so each job, and each variant, skips on its own pass.
When one of those jobs gives the line a secret, or init can't edit one of them, the line stays as
it is for all, and the plan says where. [ADR 0050](../adr/0050-init-reads-what-ci-runs.md) gives
the rules and their limits.

A `make`, `just` or `task` target other than `test`, `lint` and the like, such as `make test-ci`,
is wrapped when its recipe runs a check init knows, and nothing that pushes, publishes or deploys:
init reads the recipe, three levels down, and runs nothing. A task of your own, `cargo xtask lint`,
or one that runs with `--check`, such as `cargo xtask codegen --check`, is wrapped too, named after
the task. A test script, such as `./scripts/test.sh`, or any other `cargo xtask` task, can do
anything, so init only lists it under `Could also wrap`, with the line to use. And where Turborepo, Nx, Gradle, Bazel or Pants already reuse their results across machines
through a remote cache, init leaves what runs through them to the tool, and says so.

In CI, a pass counts in its own job, one name on one system whichever workflow's file holds it, and
each job finds its own: two jobs that run one check both skip on a re-run. In a job with several
variants, a matrix or GitLab's `parallel`, each variant counts only its own passes. init wraps its
lines and scripts like any other, and writes which variant each run is in the job's `env`:

```yaml
jobs:
  test:
    strategy:
      matrix:
        node: [20, 22]
    runs-on: ubuntu-latest
    env:
      HUSHRUN_VARIANT: ${{ toJSON(matrix) }}
```

GitHub fills it in with the values of the variant that runs, so CI runs each variant until that
variant passes on this code, for a matrix an expression builds too. GitLab names each instance
apart already, so it needs nothing. A pass from a laptop or an agent never skips a variant, since it
ran none, and still skips every job without variants. A job without the variable, whatever an older
init wrapped, doesn't skip: the check runs, is recorded, and one line says why, and
`npx hushrun init` adds it. If init can't edit the job's `env`, written on one line or shared
through an anchor, its notes give the line to add. `hushrun ready` lists such a check as `ci`, and
`hushrun doctor` warns of a job that sets the variable to anything else. C-10 and C-14 give the
rules and their limits; [ADR 0049](../adr/0049-each-variant-is-its-own-job.md) says why.

A check that only means something in one place can say so. `counts_from` in `hushrun.yaml` says what
it needs, such as `counts_from: {os: linux, go: "1.25"}`, and a run that doesn't meet that keeps its
exit code and records nothing: `hushrun: go-test passed in 6s; it won't count: needs go 1.25, this
is go1.24.3`. A pass from a machine that met it skips anywhere. `hushrun ready` and the hook start no
tool, so they read `os`, `arch` and `env`, and learn the rest from that run, whose reason they then
show as `ci`, unless the check failed on this code, which still holds a push. It is about where a run happened, not whose evidence counts, and what a machine says
is its own word: it stops mistakes, not people. `npx hushrun init` writes one rule itself,
`counts_from: {os: macos}`, when the checks of a repository or a scope run in `macos-*` jobs with
Xcode or Swift, and shows it in its plan. C-15, C-25 and
[ADR 0045](../adr/0045-a-check-says-where-it-counts.md) give the rules and the limits.

Without `package.json`, git doesn't clone the pre-push hook and no package manager installs it, so
the first `hushrun ready` or `hushrun run` in a clone does, where hushrun is installed. When
`npx hushrun init` runs on a machine with no hushrun on `PATH`, its plan offers to install a release
in a directory of yours that is on `PATH`, so that the hook can run. `hushrun hook uninstall` keeps
the hook out of that clone until `hushrun hook install`.

### A repository of several projects

A change to one project shouldn't run another's checks. When CI runs checks in subdirectories, init
gives each project a scope in `hushrun.yaml`, and passes it to `hushrun run`:

```yaml
scopes:
  back:
    include:
      - "back/**"
      - "web/config.json"
      - "*"
      - ".github/workflows/**"
    exclude:
      - "*.md"
      - "LICENSE*"
      - "NOTICE*"
    external:
      - path: ../cli
  ui:
    include:
      - "ui/**"
      - "*"
      - ".github/workflows/**"
    exclude:
      - "*.md"
      - "LICENSE*"
      - "NOTICE*"
```

```yaml
- run: hushrun run --scope back test -- cargo test --locked
  working-directory: back
```

A check skips until its scope's files change, so a scope has to hold everything the check reads.
init gives each one:

- **The root's files:** `*` is every file at the root, dotfiles included, and nothing below it, since
  a project's tools read configs there. `*.md`, `LICENSE*` and `NOTICE*` stay out, also at the root
  only: a changelog shouldn't run every check, and a project's own docs still count.
- **What its manifests name:** in its `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod` or
  Terraform files. A crate that inherits from its Cargo workspace (`edition.workspace = true`,
  `serde = { workspace = true }`) brings that workspace's `Cargo.toml` and the crates it inherits by
  path, but not its dev-dependencies, which a project that depends on it never builds. A project
  that is itself a member of a workspace holds all of the workspace, whose lockfile it builds with.
- **What its files name:** any path that starts with `../`, quoted or not, in a text file git
  tracks, that leads to something outside the project, as `web/config.json` above does for
  a test that opens it. `cat ../lib/gen.txt`, `context: ../lib` and `make -C ../lib` count; without
  quotes, a comment doesn't. A wrong guess only widens a scope.
- **Where its links lead:** a link git tracks counts as a path to what it points at. An absolute
  link, or one to nothing git tracks, keeps one scope.
- **CI's files**, and `.cargo/**`.

Where it can't be sure, a workspace it can't read or a manifest it can't read, it keeps one scope
and says why: any file's change then runs every check, which costs a run and never a wrong skip.

init can't see a path built at run time, such as `path.join(__dirname, "..", "..", "landing")`. If a
check reads one, add it to the scope's `include`.
You can always write the scopes yourself. With several scopes, `hushrun run` needs `--scope`: without
it, it runs the command and records nothing.

### Monorepos

In a JavaScript workspace, each package is a project. init reads the packages the root lists, in
`pnpm-workspace.yaml`, `package.json`'s `workspaces` (npm, Yarn and Bun) or `lerna.json`, and wraps
each package's scripts in its own `package.json`, with its scope:

```json
{
  "name": "@acme/web",
  "scripts": {
    "test": "hushrun run --scope acme-web -- vitest run"
  },
  "dependencies": {
    "@acme/ui": "workspace:*"
  }
}
```

A package's scope holds its directory and those of the packages it depends on, as its
`dependencies`, `devDependencies`, `peerDependencies` and `optionalDependencies` name them, and
theirs in turn, with what the rest of this section says every scope holds. A change to `@acme/ui`
runs `@acme/web`'s tests; a change to `@acme/web` leaves `@acme/ui`'s skipped. A `turbo.json` task
that needs another package's, such as `"web#test": {"dependsOn": ["ui#build"]}`, counts too, and so
do a tsconfig's `references` and an `extends` that names a package.

The root's `test`, such as `pnpm -r test`, `turbo run test` or `npm test --workspaces`, stays as it
is: it runs each package's `test`, which skips package by package, so CI's `pnpm test` needs no
change. A job that runs it gets the ID token it needs to record. With Yarn from 2 on, each package
init wraps also gets `hushrun` in its `devDependencies`, since Yarn gives a package's scripts only
its own dependencies' programs.

Turborepo 2 gives the scripts it runs only the variables `turbo.json` lists, so when CI runs a
package's script through `turbo`, init lists hushrun's, and CI's, in `globalPassThroughEnv`:

```json
{
  "globalPassThroughEnv": [
    "ACTIONS_ID_TOKEN_REQUEST_*",
    "GITHUB_*",
    "HUSHRUN_*"
  ]
}
```

turbo passes them on without making them part of its cache keys. Without them, the script would
get no ID token in CI and run every time. A list there already keeps its entries and gets these
after them, and comments stay where they are.

init keeps one scope, and says why, when it can't read the graph: a `package.json` that isn't JSON,
or a package of the workspace named from somewhere it can't place, such as `github:acme/core`.
`nx.json` keeps one scope too, since Nx follows the imports in the code. A package that imports a
sibling its `package.json` doesn't name, which npm and Yarn 1 let resolve, can skip on a change to
that sibling: name it among its dependencies, as pnpm requires anyway.

### Code outside the repository

A check can build against something beside the repository, such as a sibling checkout its
manifest points at. Nothing in the repository's own files changes when that does, so a scope
declares it, as `external` above: `path` is the top of a git checkout, relative to the repository's
root, and `include` and `exclude` choose its files, all of them by default. Its files as they are
now, uncommitted changes included, are part of what counts as the same code, so a change there runs
the check again. When the path is missing, or isn't the top of a checkout, hushrun can't tell which
code this is: it runs the command, says why, and records nothing. `external` doesn't go with `deploy`,
whose evidence covers the repository's own files.

### By hand

Install hushrun:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh | sh
```

On Windows, use the release's PowerShell installer. Later, `hushrun update` replaces what the
installer installed with the newest release; at a terminal, hushrun says once a day when one is
out (set `HUSHRUN_NO_UPDATE_CHECK=1` to turn that off). Then put `hushrun run` in front of the
check, here in `package.json`:

```json
"scripts": {
  "test": "hushrun run unit -- vitest run"
}
```

`unit` names the check, and everything after `--` is its command; without a name, the check
takes the script's. Run it twice:

```text
$ npm test
hushrun: unit running: no pass yet for this exact code
  … vitest's output …
hushrun: unit passed in 41s; it won't run again on this code
$ npm test
hushrun: unit skipped: this exact code passed just now (saves 41s)
```

That's all. There is no config file, key or account to set up, and hushrun exits with the
command's exit code, or 0 when it skips.

## What counts as the same code

- **The repository's files as they are now:** committed or not, untracked ones included, ignored
  ones left out, or those of the check's scope ([above](#a-repository-of-several-projects)), and
  what the scope declares outside the repository. Change a file and the check runs again; committing
  changes nothing, and going back to files that passed skips.
- **The name, the exact command, and the directory you run it from.** `vitest run` and
  `vitest run src/cart.test.ts` are two checks, so a partial run never stands in for a full one.
  Without a name, `hushrun run -- vitest run` takes the name of the `package.json` script that
  runs it, `test` for `npm test`, else the program's, `vitest`. A name is lowercase letters,
  digits, `_` and `-`, so a script's is made one: `npm run test:unit` names its check
  `test-unit`, and `Lint` names it `lint`. `hushrun run unit -- vitest run` names it yourself.
- **Not the machine, the branch, the commit or the CI provider.** A pass on your laptop skips the
  same check in CI, and the other way round. Environment variables and the OS aren't part of it
  either: when a check depends on them, give each variant its own name, such as `unit-node22`.

### Shell tests with Bats

With `tests: reuse` (or `{mode: reuse, unit: test, reads: test}` for one test at a time), a Bats
check skips the `.bats` files, or the tests, that already passed on the files they read. hushrun
sees what each test read through bash itself: the helpers it `load`ed, the libraries it
`source`d, the scripts it ran and what those sourced. A test it reuses shows in bats' own output
as a skip, so the count and the exit code stay bats':

```text
$ hushrun run unit -- bats test
hushrun: unit: 3 of 4 tests run, in 2 of 2 test files; 1 already passed on the files it read
1..4
ok 1 says hello
ok 2 plain
ok 3 adds # skip reused by hushrun
ok 4 adds zero
```

- **bats-core 1.10 or later**, started as `bats`, by a path (`./test/bats/bin/bats`), or through
  `npx`, `pnpm`, `yarn` or `bun`. A shell script or Makefile that runs bats isn't observed: run
  bats itself after `--`.
- **A file bash doesn't run runs every test when it changes**: a `#!/bin/sh` script, a Python
  helper, a fixture. bash-run files are `.bash` and `.sh` files whose `#!` line names bash (or
  that have none and aren't executable) and scripts like `bin/tool` with a bash `#!` line.
- **What hushrun can't see**: a file read as data (`cat`, `grep`, `shellcheck`), a program that
  isn't bash, and a process started without the environment (`env -i`, `sudo`). Put a test file
  that depends on those in `tests.always`, so it always runs.

## Where results go

hushrun takes the first of these:

1. **`HUSHRUN_BACKEND`**, when it is set. Empty (`HUSHRUN_BACKEND=`), it turns results off: the
   command always runs, and nothing is recorded.
2. **The repository's backend**, which `package.json` names:

   ```json
   "hushrun": {
     "backend": "https://api.hushrun.com/o/acme"
   }
   ```

   or, in a repository with no `package.json`, `hushrun.yaml`:

   ```yaml
   backend: https://api.hushrun.com/o/acme
   ```

   `hushrun init` adds it. Committed with the code, it tells CI, your teammates and their agents
   where results go, with no variable to set. Your org's URL is also on its Checks page in the
   dashboard, under **Connect**.
3. **The backend you signed in to** with `hushrun login`, for repositories that name none.
4. **This machine**, with no setup: hushrun's cache directory, such as
   `~/.cache/hushrun/results` on Linux or `~/Library/Caches/hushrun/results` on macOS.

A result keeps its command, which the dashboard shows. A word hushrun can tell is a secret shows as
`[redacted]`: the value of a variable whose name says it is one, such as `DEPLOY_TOKEN`, or a word
shaped like a token, such as `ghp_…`. The key is the command as given, so what skips doesn't change.
A secret no variable names still goes as written, so keep secrets out of commands.

On a laptop, a backend needs your login. Until you sign in, hushrun runs every check, and at a
terminal it ends with:

```text
hushrun: sign in to share results with acme: npx hushrun login
```

`npx hushrun login` (or `hushrun login`) opens a page where you approve the code your
terminal shows, for one of your orgs, then keeps a token only you can read. In a repository that
names a backend, it signs in there; elsewhere, to hushrun's hosted backend, where you pick the
org. `hushrun login https://hushrun.example/o/acme` signs in to an org on another server.
`hushrun logout` revokes the token.

Signing in also registers this machine
([ADR 0028](../adr/0028-machines-registered-at-sign-in.md)): hushrun makes a key the first time,
kept beside the token and never in a repository, and the server binds it to you and the org.
Your results and events carry the machine's signature, and the server refuses one from a
machine it didn't register. Without the key, or once the machine is revoked, a run ends with
``hushrun: test passed in 41s; not recorded: this machine isn't registered: run `hushrun login` ``.
The dashboard lists your machines, each revocable, and `hushrun logout` revokes this one with
the token. hushrun cloud revokes a machine you haven't used in 90 days, and `hushrun login` on it
brings it back; you have at most 20 machines in an org, and past that the sign-in says which
page to revoke one on. `hushrun whoami` shows the key. For deploys, your
machine's events count while you are a member of the org, unless the dashboard makes the repository CI-only
([ADR 0031](../adr/0031-membership-is-trust.md)).

## In CI: GitHub Actions

CI needs no secret, and no variable when `package.json` names the backend. Give the job an ID
token:

```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@v4
      - run: npm ci
      - run: npm test
```

`npm ci` installs hushrun with the other devDependencies. hushrun asks GitHub for an ID token
whose audience is the backend's URL, and the backend checks which repository the job runs for.
hushrun cloud accepts a repository its org connected: `hushrun init` connects yours, with your
login, or a hosted ledger of the org is linked to it. The first run it accepts from a connected repository pins that repository's GitHub id, so a
repository deleted and created again under the same name is refused, with a message that says
why. Pull requests from forks get no ID token: their checks run, unrecorded.

Without `package.json`, a step installs hushrun, and `hushrun.yaml` names the backend:

```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write
    steps:
      - uses: actions/checkout@v5
      - uses: hushrun/hushrun/action/setup@v0.4.0
      - run: hushrun run unit -- cargo test
```

On other CI, store an agent token as the job's `HUSHRUN_TOKEN` secret, and the key it comes with
as `HUSHRUN_MACHINE_KEY`, or as a file `HUSHRUN_MACHINE_KEY_FILE` names where the CI can't mask a
PEM block: `hushrun token create --agent gitlab-ci --expires 365d`, at a terminal, shows both, and
`--format json` gives them to a script. `HUSHRUN_AGENT` is for sandboxes ([below](#agents-in-sandboxes)): where CI sets only `CI=true`, hushrun can't tell it
from one, so the job's passes would count everywhere, and it would skip on any pass. When `package.json` names a backend on hushrun's hosted backend, that's all;
for a backend elsewhere, set `HUSHRUN_BACKEND` too. Any other token in `HUSHRUN_TOKEN` goes only to the backend
`HUSHRUN_BACKEND` names, never to one that a file in the repository names: a change to that file
can't send your CI's secret anywhere.

## What a pass's reports said

After the command exits, pass or fail, `hushrun run` reads the test and coverage reports it wrote
and keeps their summary with the result: counts, and each file's path and format. A skip shows the
summary of the pass it stood in for.

```
hushrun: unit passed in 1m 21s: 1,204 tests, 14 skipped, 87.3% of lines covered; it won't run again on this code
hushrun: unit skipped: this exact code passed in CI 3 hours ago (saves 1m 21s): 1,204 tests, 14 skipped, 87.3% of lines covered
```

`hushrun ready` shows a passed check's after who recorded it, and the dashboard shows each run's.
Failures and errors count as failed; Go's coverage counts statements.

hushrun reads JUnit XML and TRX for test results, and LCOV, Cobertura, JaCoCo, Clover, OpenCover,
Go's coverprofile, coverage.py's JSON report, istanbul's `coverage-summary.json` and SimpleCov's
`.resultset.json` for coverage. It tells each by its content, never by its name, and takes
coverage from one format only, so coverage written in two formats counts once. It looks at:

- the files the command line names, such as `--junitxml=junit.xml`, `--cov-report=xml:cov.xml`,
  `-coverprofile=c.out` or `--coverageDirectory coverage`;
- the files `hushrun.yaml` lists for the check, for a path no tool would use:

  ```yaml
  # hushrun.yaml
  checks:
    unit:
      reports:
        files: [out/results/*.xml]   # from the directory the check runs in
  ```

- on GitHub Actions, the files a later step of the job reads;
- the places tools write to by default, such as `junit.xml`, `coverage/lcov.info`,
  `target/surefire-reports/*.xml` or `**/TEST-*.xml`, from where the check ran and from the
  repository's root.

Only files the run wrote count: regular files inside the repository, modified since it started.
hushrun keeps no file, no test's name and no message: to see which test failed, run the check
again with `--force`, or open the report. Reading a report never changes a run's result or its
exit code; a report it can't read is no summary.

## Reports a later step reads

A skip writes no report. So in a CI job where a later step reads what a check writes, such as a
coverage upload to Codecov, a test reporter, or an artifact upload of `test-results/`, a pass
doesn't skip the check: it runs, records its pass, and says why.

```
hushrun: unit running: a later step reads its reports (codecov/codecov-action), and a skip writes none
```

On a laptop, and for an agent, nothing reads those files, so the same check skips on a pass, as
before. `hushrun init` wraps such lines in CI's files, and its plan says which checks CI runs every
time, and why.

On GitHub Actions, hushrun finds those steps in the workflow file the job runs:

- a known uploader or reporter: Codecov, Coveralls, coverage comments, test reporters such as
  `dorny/test-reporter`, SonarSource's scanners, CodeQL's SARIF upload;
- an artifact upload whose path names a report, such as `coverage/` or `junit.xml`, but not
  `dist/`;
- an action it doesn't know, with an input that names a report file;
- a command that reads reports, such as `codecovcli upload-process`, `coverage xml` or
  `cargo llvm-cov report`;
- a local composite action with such a step.

A step that runs only on `failure()` doesn't count, since it never runs after a skip, which
succeeds. A linter, a formatter or a type checker still skips beside steps that read only
coverage or test results: its skip leaves them as they were.

A check whose pass wrote none of what those steps read skips after its first pass in the job. Its
pass recorded every report file it wrote ([above](#what-a-passs-reports-said)), so in a job with
two test checks and an upload of one's coverage, the other skips:

```yaml
      - run: npx hushrun run unit -- pytest --junitxml=junit.xml
      - run: npx hushrun run cov -- pytest tests/slow --cov --cov-report=xml
      - uses: codecov/codecov-action@v5
        with:
          files: coverage.xml
          disable_search: true
```

That needs a step that reads only the paths it names. Codecov's actions also upload the coverage
they find themselves, unless `disable_search: true` says not to; a step that names no path, runs a
command, or reads SARIF can read anything, so the checks beside it keep running. The pass must be
the same job's, recorded by the same version of hushrun, with the workflow file in the check's
scope, so that changing the job's steps changes what counts as the same code. A pass names at most
16 report files, so a check that writes more, such as Surefire's file for each test class, keeps
running beside such a step.

When your readers handle a skip, such as an upload that only runs when the check ran, let CI skip
anyway ([below](#let-ci-skip-a-check-whose-reports-are-read)).

Where hushrun doesn't read CI's file at run time, on GitLab and other CI, set
`HUSHRUN_REPORTS_READ=true` in each job that reads its checks' reports: every check of that job
then runs on a pass. `hushrun init` writes it into the GitLab jobs it changes whose artifacts keep
reports, and says to add it by hand where a job's variables come through `<<` or an anchor.
`hushrun ready` says `ci` for these checks, with why, and `hushrun doctor` lists them.

## Let CI skip a check whose reports are read

A check whose reports a later step reads runs every time in CI (above). When that step can do
without them on a skip, let CI skip the check: set `reports.ci: skip` for it, and gate the step on
the output every run writes on GitHub Actions, `hushrun-skipped`, with an `id:` on the step that
runs hushrun.

```yaml
# hushrun.yaml
checks:
  unit:
    reports:
      ci: skip
```

```yaml
# .github/workflows/ci.yml
      - id: unit
        run: npx hushrun run unit -- pytest --cov --cov-report=xml
      - uses: codecov/codecov-action@v5
        if: steps.unit.outputs.hushrun-skipped != 'true'
        with:
          files: coverage.xml
          flags: unit
```

- **Codecov** needs the check's flag to carry forward, so that a commit with no upload keeps the
  last coverage, and an `after_n_builds` that doesn't count this upload. `hushrun init` says what
  your `codecov.yml` sets of both.

  ```yaml
  # codecov.yml
  flag_management:
    individual_flags:
      - name: unit
        carryforward: true
  ```

- **`dorny/test-reporter`** fails on no results by default: gate it with the same `if:`, or set
  `fail-on-empty: false`.
- **`actions/upload-artifact`** with `if-no-files-found: error` fails on a skip: gate it, or set
  `if-no-files-found: ignore`.
- **`EnricoMi/publish-unit-test-result-action`**: gate it with the same `if:`.

A step that runs whatever happened, such as `if: always()`, keeps that and adds the output:
`if: always() && steps.unit.outputs.hushrun-skipped != 'true'`. Don't send an empty Codecov upload
to fill the gap: it reports coverage nobody measured.

## Agents in sandboxes

Agents that work on the repository in sandboxes, such as Claude Code cloud sessions, `claude -p`
in a container, or an E2B or Docker sandbox, have no browser to sign in with. An org admin gives
them an agent token instead. It records and reads results of one repository, as
`agent:<name>`, until it expires (in 30 days by default) or someone revokes it, and nothing
else: no ledger, no other repository, no other org.

Create one on the org's **Agents** page in the dashboard, or in the repository, signed in with
`hushrun login`:

```bash
hushrun token create --agent fleet-a
```

At a terminal, it shows one string, once, as its first line on stderr: a single line that holds the
org's URL, the token and the agent's key. In CI, or with stderr piped, it doesn't, nor the key, since
a log could keep them: `--format json` gives a script the string as `hushrun_agent`. Stdout stays the
token alone. The backend keeps only the token's hash and the key's public half. Every token comes with its key,
and the backend records the agent's results only signed by it. One string can serve a whole fleet.
Give each sandbox one variable:

```bash
HUSHRUN_AGENT=hushrun_agent_v1_…
```

That's all, whether or not the repository names a backend: `npm test` in the sandbox skips what any
agent, teammate or CI job already passed on the same files, and records the rest. In the sandbox,
`hushrun whoami` shows the agent, its repository and when the token expires.

The string needs hushrun 0.6.0 or later. An older one ignores it, and shares nothing. `hushrun init`
pins the version it was run with, such as `^0.5.0`, and a caret on 0.x stays in that minor, so a
repository set up before 0.6.0 stays there, however often `npm ci` runs, and running init again
doesn't move it. Move it once, and commit `package.json` and the lockfile:

```bash
npm install --save-dev hushrun@latest   # or pnpm add -D, yarn add -D, bun add -d
```

The string holds a private key, so treat it as a secret. hushrun never sends it, and never prints
it after `token create`. It only ever goes in `HUSHRUN_AGENT`. In any other of hushrun's variables
it is dropped, with a line, and a command line that holds one stops before anything runs. An older
hushrun has neither guard, and would send it from `HUSHRUN_TOKEN`.

It means several things at once:

- **This is a sandbox.** hushrun treats the run as local even where the sandbox sets `CI=true`. A
  named CI's own variable, such as `GITHUB_ACTIONS` or `GITLAB_CI`, beats it, so a job a secret
  reaches stays that CI, with its guards. An agent's step inside such a job that should count as a
  sandbox sets `HUSHRUN_PROVIDER=local` too.
- **Its backend counts as one the environment named,** so it wins over `package.json` and
  `hushrun.yaml`, and the token goes to it. That token goes to no other server.
- **It wins over what is beside it.** `HUSHRUN_TOKEN`, `HUSHRUN_MACHINE_KEY`,
  `HUSHRUN_MACHINE_KEY_FILE` and a different `HUSHRUN_BACKEND` are ignored, and one line says so.
  An empty `HUSHRUN_BACKEND` still turns results off.
- **An agent runs here.** `hushrun login`, `hushrun token` and an `init` that
  needs a login refuse, and a push is an agent's, even with `HUSHRUN_INVOKED_BY=human:…`, whether the
  string can be read or not. No login on the machine speaks for it, and when the string can't be
  read, nothing is recorded.

Commands inherit it, as they inherit anything in the environment, and a check's own `hushrun run`
needs it to record. An isolated run never passes it in. A string that can't be read costs one line
that says what's wrong, and the check still runs.

To build one by hand, base64url-encode, without padding, the JSON
`{"backend": "https://api.hushrun.com/o/acme", "key": "<the PEM block>", "token": "hra_…"}`,
and put `hushrun_agent_v1_` in front.

### Set up with two variables

A sandbox that was set up before `HUSHRUN_AGENT` keeps working:

```bash
HUSHRUN_TOKEN=hra_…                  # the agent token
HUSHRUN_MACHINE_KEY="-----BEGIN …"   # the agent's key: the whole PEM block, as one value
HUSHRUN_PROVIDER=local               # where the sandbox sets CI=true, so that hushrun doesn't take it for CI
```

A sandbox that keeps the key in a file outside the repository sets `HUSHRUN_MACHINE_KEY_FILE` to
its path instead. hushrun signs an agent's results only with a key one of these names, never with
a key file a person's sign-in left on the machine. `hushrun token create` shows both at a terminal,
and `--format json` gives them to a script. Without the key a sandbox records nothing.

### Claude Code cloud

In the environment's settings:

- the environment variable:

  ```bash
  HUSHRUN_AGENT=hushrun_agent_v1_…
  ```

- network access that reaches `api.hushrun.com`, `registry.hushrun.com` and
  `*.r2.cloudflarestorage.com`, as allowed domains or with full access, and the npm registry;
- a setup script that installs the dependencies, which installs hushrun with them: `npm ci`.

Each session's `npm test` then goes through `hushrun run`, and skips what another session, a
teammate or CI already passed on the same files.

### Hosted agent platforms

hushrun knows the sandboxes vendors host for their agents by a variable only that platform sets,
such as `CLAUDE_CODE_REMOTE` on Claude Code on the web or `CURSOR_AGENT_SOCKET` on Cursor's cloud
agents. There a run is local, even beside `CI=true` or, for GitHub's Copilot cloud agent, inside a
GitHub Actions job. With no credential set, results stay in the sandbox and skip checks for the rest
of the session; with one, they are shared. An agent's own variable, such as `CLAUDECODE`, makes a
sandbox only beside a bare `CI=true`: an agent's step in a named CI's job stays CI.

In the sandbox, `hushrun doctor` names the platform, where the credential comes from, whether each
host hushrun needs answers, and the exact entry that platform's allowlist needs. It never prints a
value. `sh scripts/agent-cloud-probe.sh`, in this repository, shows what a new platform's sandbox
looks like, with variable names only.

### A container

Any sandbox takes the same variable: `docker run -e`, an E2B sandbox's environment, a
Kubernetes job's `env`. With an image that has git, Node.js and Claude Code:

```bash
docker run --rm \
  -e HUSHRUN_AGENT \
  -e HUSHRUN_INVOKED_BY=agent:worker-1 \
  -e ANTHROPIC_API_KEY \
  agent-image sh -c '
    git clone https://github.com/acme/shop && cd shop && npm ci &&
    claude -p "Fix the flaky cart test. Run npm test before you commit."'
```

### Fleets

- **One token, a label each.** `HUSHRUN_INVOKED_BY=agent:<id>` labels each run, so that Checks
  tells the members of a fleet apart; the token still decides who recorded:

  ```bash
  for i in $(seq 1 20); do
    docker run -d -e HUSHRUN_AGENT \
      -e HUSHRUN_INVOKED_BY=agent:worker-$i agent-image …
  done
  ```

- **A limit per token.** A token records at most so many results a minute, 300 on hushrun cloud
  unless `hushrun token create --rate N` sets another, so that a runaway fleet can't flood the
  org. Past it, checks still run, and lookups still skip; hushrun ends the run with `hushrun:
  test passed in 41s; not recorded: …`.
- **One machine per token.** However many sandboxes use a token, they sign with its one key, so
  the org's machines list shows one machine for it, not one per sandbox.
- **Rotation.** Tokens expire, 365 days at most. Create the next one under the same agent name,
  switch the variable, then `hushrun token revoke` the old one.

`npx hushrun ready` tells an agent what's left before it pushes, and the pre-push hook that
`hushrun init` installs refuses its push until then. A sandbox gets the hook with its dependencies,
through the script init adds to `package.json`; without `package.json`, from its first
`hushrun ready` or `hushrun run`.

A pass an agent records skips the check in CI, as a pass on a laptop does: results only decide
whether this exact code runs a check again. A deploy's evidence is another matter, with its own
setup ([the agents guide](agents.md)).

### Another host

`HUSHRUN_AGENT` names its own backend, on any host, so this is for tokens in `HUSHRUN_TOKEN`. Such a
token follows the repository's backend only on hushrun's host, `https://api.hushrun.com` or the one
`HUSHRUN_HOST` names, so that a change to a file in the repository can't send it anywhere else. For a
backend on another host, set `HUSHRUN_BACKEND` to the org's URL in the sandbox too (`hushrun token
create` prints it), or `HUSHRUN_HOST` to that host.

### When a sandbox's results aren't recorded

hushrun says why in one line, and runs the check anyway:

| Message | What to do |
|---|---|
| `refused HUSHRUN_AGENT: agent token hra_… (agent:fleet-a) expired on …` | Create another under the same name, and update the variable |
| `HUSHRUN_AGENT is damaged: …` | The string was cut short or wrapped: copy it again, on one line. Nothing else about it is shown, since it holds a key |
| `HUSHRUN_AGENT comes from a newer hushrun than this one` | Update hushrun |
| `HUSHRUN_AGENT wins over …` | Remove the variables it names: they're ignored |
| `HUSHRUN_TOKEN holds a HUSHRUN_AGENT string, which is never sent` | Set it as `HUSHRUN_AGENT`, and unset the variable the line names |
| `HUSHRUN_AGENT holds an agent token alone …` | Stdout is the token: set the whole string, from a terminal or `--format json` |
| ``HUSHRUN_AGENT's key can't sign for its token: make a new string …`` | Make a new one with `hushrun token create --agent`: no variable replaces the key inside it |
| `results aren't shared with acme: no token: …`, with `HUSHRUN_AGENT` set | This hushrun is older than 0.6.0, and ignores it: move the repository (above) |
| `… was revoked on … by …` | The same, if the sandbox should still record |
| `not recorded: … records results for github.com/acme/shop only, and this result is for …` | The token is in another repository's sandbox: use that repository's token, or one from `--org-wide` |
| `not recorded: agent token hra_… (agent:fleet-a) recorded 300 results in the last minute, its limit` | The fleet writes faster than its token allows: slow it, or create its token with a higher `--rate` |
| `running without skipping: … the external input ../cli …` | What the scope builds against isn't there, or isn't the top of a git checkout: check it out where `hushrun.yaml` says |
| `not recorded: …; upgrade: https://…` | The org has recorded every check its plan includes this period: an admin upgrades at that page, or the next period starts over. `hushrun repository usage` shows where it stands |
| ``not recorded: set `HUSHRUN_MACHINE_KEY` to the key `hushrun token create --agent` printed`` | The sandbox has no key, or one that isn't its token's: set `HUSHRUN_MACHINE_KEY` to the key shown with the token, or make a new token if nobody kept it |
| `HUSHRUN_TOKEN isn't sent to …` | The backend isn't on hushrun's host: use `HUSHRUN_AGENT`, or set `HUSHRUN_BACKEND` or `HUSHRUN_HOST` (above) |

## Tests with Node's test runner and Mocha

With `tests: {mode: reuse}` on a check, `node --test` and Mocha reuse test files, and with
`unit: test` single tests, that already passed on the files they read, as Vitest and Jest do. No
file changes: hushrun adds its adapter to `NODE_OPTIONS` (keep what a script's `NODE_OPTIONS=` had,
`NODE_OPTIONS="$NODE_OPTIONS …"`, or the adapter is dropped and the run says so).

```yaml
checks:
  unit:
    tests: {mode: reuse, unit: test}
```

```text
$ hushrun run unit -- node --test
hushrun: unit: 3 of 41 tests run, in 2 of 12 test files; 38 already passed on the files they read
```

- **Node's test runner** needs Node 22.15 or later. Each test file's process is watched from its
  start to its exit, so what a test file read is exact; a left-out test shows as skipped with
  `hushrun: passed before on the files it read`. `--watch`, `--test-isolation=none` and coverage
  run everything; `--test-shard` leaves single tests out but no whole test file.
- **Mocha** 10 or 11, on Node 22.15 or later. Every spec file still loads, so its root hooks and
  what its top level does are as without hushrun; the tests that passed are taken out before the
  run starts, and a suite left with none runs no hook. A spec file that defines a root hook is
  read by every spec file and always runs. `--parallel` and `--watch` run as written.
- What a test reads is its test file's list (`reads: test` runs as `reads: file`).

## Packages a sandbox starts with

A sandbox that starts from an image with compiled code still downloads its dependencies: a
Gradle or Maven build fills its cache from nothing. A **package store** holds those downloads,
one part per package manager in the tool's own layout, and each tool reads its part in place
beside its own cache: Gradle through `GRADLE_RO_DEP_CACHE`, Maven as
`-Dmaven.repo.local.tail=` in `MAVEN_OPTS`. Nothing is copied, and what the store lacks is
downloaded as usual.

```sh
# Bake: run the check's own build with the tools' caches at the bake's own folders.
hushrun packages bake --out /hushrun-packages -- ./gradlew test
# Or take a cache the job already filled.
hushrun packages bake --out /hushrun-packages --harvest maven=$HOME/.m2/repository
# Read: hushrun run does it; outside a run, exec or env.
hushrun packages exec -- mvn -B verify
eval "$(hushrun packages env)"
```

A run reads `/hushrun-packages`, or the folder `HUSHRUN_PACKAGES_LOWER` names. A store nothing
signed is read only with `HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1`, and never in CI. A run that may
write what others trust hashes every file first; elsewhere a run checks names and sizes, and
`hushrun packages verify` hashes every file. `HUSHRUN_PACKAGES=off` turns it off.

Hex's packages are seeded instead: before the command, a run puts each tarball `mix.lock` pins
into Hex's own cache (`HEX_HOME`, else `~/.hex`), checked against the lockfile, and `mix deps.get`
unpacks `deps/` from there; `mix deps.compile` runs as usual. Hex still fetches each package's
registry record, so its repository must be reachable.

```sh
hushrun packages bake --out /hushrun-packages -- mix deps.get
# In the sandbox's setup, before mix deps.get:
hushrun packages restore
```

## Compiled code

A check that runs can still skip compiling what this machine already compiled from the same
inputs, in any worktree or clone: `hushrun build on` turns that on for the builds `hushrun run`
runs. Rust crates go through cargo's `RUSTC_WRAPPER`, and the C and C++ objects of a CMake build
through CMake's compiler launcher. Go's own build cache already does this on one machine, so hushrun
adds Go only where an org shares compiled code through its backend, and Gradle's tasks (Kotlin,
Java, Android) the same way, as the build's remote build cache. On your machine it is off until
you turn it on, once, for every repository; `hushrun build` shows the mode, the store and its size,
and `hushrun build observe` compiles everything as usual and says what would have been reused.

```text
$ hushrun build on
$ hushrun run build -- cargo build      # in a second worktree
hushrun: build: 32 of 35 crates reused, 3 built, saving about 17s
```

- **Only inside `hushrun run`.** A plain `cargo build` or `cmake --build` neither reads nor fills
  the store, and turning reuse on doesn't make cargo rebuild what it built already. A
  `RUSTC_WRAPPER` you set, such as sccache's, stays, and then no Rust crate is reused. A CMake build
  folder gets the launcher when it is configured inside `hushrun run`.
- **When it saves time.** Each new worktree or clone, with its own target folder, reuses what the
  others compiled, and so does a build after `cargo clean`, or of a file whose date changed but
  whose bytes didn't, which cargo would compile again. A program, a binary or a test binary, is
  reused only in the target folder that built it.
- **When it saves little.** One checkout whose target folder stays already compiles only what
  changed. Worktrees that share one `CARGO_TARGET_DIR` already reuse most crates through cargo
  itself, but cargo judges freshness by file dates, so one worktree can be handed another's build.
- **Disk.** The store, `build` under hushrun's cache folder, is a second place compiled code
  lives. Files go in and out by cloning where the file system can clone, as APFS does, and then the
  store and the target folders share their bytes: on a Mac, a second worktree that reused 32 of 35
  crates took 10 MB more disk, where plain cargo took 147 MB. Where files are copied instead, the
  store saves disk only when you delete target folders you would otherwise keep. `du` and
  `hushrun build status` count a cloned file in full in each place. A block not used for 15 days is
  deleted, and nothing is stored while the disk has less than 10 GB free.

- **Gradle, and so Android.** Where the org shares, `hushrun run` is the remote build cache of a
  Gradle build: a branch's first CI job takes from main's what main's CI already ran, and a laptop
  takes what CI ran, with no cache server of your own. It needs the line `hushrun init` proposes in
  the settings file (`if (System.getenv("HUSHRUN_GRADLE")) apply from:
  System.getenv("HUSHRUN_GRADLE")`, or `System.getenv("HUSHRUN_GRADLE")?.let { apply(from = it) }`
  in `settings.gradle.kts`) and the build's own `org.gradle.caching=true` in `gradle.properties`;
  the run says which is missing. Gradle's local cache stays Gradle's, so a plain `./gradlew` is as
  warm as before; a remote the build sets up itself, such as Develocity's, stays, and hushrun steps
  aside. `--offline` turns Gradle's remote cache off, and hushrun's with it. Tasks that Gradle
  can't cache, or whose keys hold absolute paths, run on every machine as before (the Android
  Cache Fix plugin covers the Android plugin's known ones), and an entry that holds your checkout's
  folder or your home folder never leaves the machine. The summary counts tasks apart:

  ```text
  $ hushrun run build -- ./gradlew assembleDebug
  hushrun: build: gradle: 214 tasks: 198 from hushrun's cache (198 from the shared store), 16 run and stored, saving about 3m 40s
  ```

  Gradle's outputs needn't be the same from one run of a task to the next, so hushrun can't
  compare an entry with a fresh run as it does compiled code: what a machine reads is only as
  trustworthy as the machines that write what it reads. Trusted copies come only from CI on the
  branches hushrun.yaml's backend lists; a person's entries reach that person, or the team where
  the org pools them.

- **Maven.** Where the org shares, `hushrun run` is the remote cache of Apache's build cache
  extension, so a Maven reactor's modules built on one machine are restored on another: a
  branch's first CI job takes from main's what main's CI built, and a laptop takes what CI built.
  The build chooses the extension itself, in `.mvn/extensions.xml`:

  ```xml
  <extensions>
    <extension>
      <groupId>org.apache.maven.extensions</groupId>
      <artifactId>maven-build-cache-extension</artifactId>
      <version>1.2.0</version>
    </extension>
  </extensions>
  ```

  hushrun gives it the remote through `MAVEN_OPTS`, with a token made for the run, which Maven
  prints with each file it saves or downloads and which opens nothing once the run ends. The
  extension's local cache stays its own; a remote the build names itself stays, and hushrun steps
  aside; the run says which applies. A restored module skips its mojos, tests included, as the
  extension decides; so a run whose Maven skips tests (`-DskipTests`, `-Dmaven.test.skip`) shares
  none of the modules it builds, which another build would restore without ever running their
  tests. An entry that holds your checkout's folder, Maven's local repository or your home folder
  never leaves the machine. The summary counts modules apart:

  ```text
  $ hushrun run build -- mvn -B verify
  hushrun: build: maven: 12 modules: 11 from hushrun's cache (11 from the shared store), 1 built and stored, saving about 1m 50s
  ```

  As for Gradle, a jar keeps its files' dates, so hushrun can't compare an entry with a fresh build:
  trusted copies come only from CI on the branches hushrun.yaml's backend lists.

- **Baked into an image.** `hushrun build bake --out DIR` writes the crates, Go packages and C and
  C++ objects the last run of each check here used (`--check NAME` narrows it) into a folder of
  sealed packs, for an image or a read-only volume a sandbox starts from. A run there reads it
  underneath its own store when `HUSHRUN_BUILD_LOWER` names the folder, and never writes to it.
  Baked from your machine it holds only what may leave any machine and is untrusted, for you
  alone: read only with `HUSHRUN_BUILD_LOWER_UNTRUSTED=1`, never in CI, and never in a run whose
  backend lets it write the repository's trusted blocks. Go packages are in it only from runs that
  shared or read a baked store. `--from backend` bakes what the org's CI compiled, and a run whose
  backend vouches for each block as it begins reuses them, in CI too. A store missing blocks says
  so and exits 0; one published for others exits 11. Bake on the platform the sandbox has.

- **Published as an image.** `hushrun build bake --oci ghcr.io/acme/app` publishes the store as a
  layered, signed image, and a Dockerfile takes it with
  `COPY --from=ghcr.io/acme/app:hushrun-linux-x64-latest /hushrun /hushrun`: a run finds a store at
  `/hushrun` (owned by root or you, and writable by nobody else) with no variable. The `-latest`
  tag moves only from CI on the default branch; elsewhere a bake publishes its fixed tag, which
  never moves, and with `--branch-tag` its branch's tag. A code change uploads only the image's
  last layer when the image it builds on is signed by a signer hushrun.yaml names and the backend
  still vouches for what it keeps. A run trusts the image when hushrun.yaml names its signer under
  `build: baked: signers:` (and `build: baked: not_before` retires older ones); `hushrun init
  --bake` adds the CI job that bakes after the default branch's build, and `hushrun build manifest
  --json` says which image is newest. Keyless signing, as the CI job's own identity, is the setup
  to use. A signing key (`--signing-key FILE`, `HUSHRUN_SIGNING_KEY_FILE`) is read before the bake
  builds anything, but its file stays readable to that build, whose build scripts and tests run as
  the same user: a key in a job that builds can leak, and a leaked key stays good until you take it
  out of `build: baked: signers`.

- **Over time.** An image's workspace layer is written again once a week (`build: baked:
  compact_every`). A bake within the hour of the image its tag names publishes nothing unless 50
  blocks changed (`rebake_every`, `rebake_blocks`; `--force` publishes anyway): sandboxes started in
  between fetch the difference, and the job's daily run publishes whatever the limit, so a quiet
  day after a held bake still catches up. Long-lived branches listed under `build: baked: branches:` get
  their own tags from the job `hushrun init --bake` writes, each signed as that branch's workflow;
  a run on the default branch trusts one only if hushrun.yaml names that branch's identity.

- **After a merge CI skipped.** When CI on the default branch skips the checks a pull request
  passed, it compiles nothing; the bake job then builds the merged code itself before it bakes, so
  the image follows main: each check as CI's own workflow runs it (the command after `hushrun run
  NAME --`, its folder and its job's variables, read from CI's file at each bake), reusing what CI
  already built, at most 30 minutes; only the checks whose newest build is behind are built (a
  check that compiles nothing counts as built at the commit its run named). A build that fails,
  doesn't finish uploading or has a block the backend refuses leaves the backend's newest build as
  it was, and the next bake tries again. The build gets none of the job's tokens. `--no-build`
  turns it off. The job copies the build job's setup actions only when they are pinned to a commit
  (`uses: owner/action@<40-hex commit>`), and a local action (`uses: ./…`) only when every action
  its `action.yml` uses is; `hushrun init --bake` says which to pin otherwise.

- **Blocks bound to CI's folders.** A crate whose output holds CI's home or cargo home (C `-sys`
  crates, and what depends on them) is baked too, for sandboxes at the same folders: on GitHub's
  runner `HOME=/home/runner`, `CARGO_HOME=/home/runner/.cargo` and the checkout at
  `/home/runner/work/<name>/<name>` (the image's `baked.json` names them under `paths`). Elsewhere
  they compile, and the run says which folders they need.

- **Dependencies first, for a new repository.** Before CI's first build is baked, `hushrun build
  bake --deps-only --oci ghcr.io/acme/app -- cargo test --locked` (the check's own cargo command,
  for its profile and features; a `cargo test` compiles its tests and runs none) compiles the
  third-party crates with your own crates as empty stubs, and publishes an image of them; the first full bake replaces it, and it
  never replaces a full one. In a Go module it builds the third-party packages, with no command,
  or with the check's `go test` flags (`-- go test -race -tags x ./...`, which isn't run).

- **What it saves, measured.** Add `--report -- cargo build` to a bake to time that command on this
  machine cold and from the store just baked: both times, and how many compiles the store served.

- **As an archive, for a sandbox snapshotted while it runs.** `hushrun build bake --tar store.tar`
  writes the same store, signed the same way, as one tar whose files are under `hushrun/`
  (`store.tar.zst` compresses it; it saves little, since the packs are compressed already, and `-`
  writes it to stdout). Put it where the sandbox can download it, then in the snapshot step:

  ```sh
  curl -fsSL https://files.example.com/acme/store.tar | tar -x -C /   # the store lands at /hushrun
  # ...then take the snapshot
  ```

  Extract it as root or as the user who runs the builds: both are read at `/hushrun` with no
  variable. A non-root user needs `/hushrun` made and given to it first (`sudo install -d -o
  "$USER" /hushrun`). A folder its group may write isn't read there, and the line says the fix
  (`chmod -R g-w /hushrun`). Extracted elsewhere (`tar -x -C /opt`), name it:
  `HUSHRUN_BUILD_LOWER=/opt/hushrun`. To refresh, boot the last snapshot and run `hushrun build
  fetch` (it downloads the backend's newest blocks into the sandbox's own store, kept for a day),
  or replace `/hushrun` with a newer archive (good until a newer manifest replaces it), then
  snapshot again. A stale snapshot is slower, never wrong: what it lacks is fetched or compiled.

- **Pulled in a setup step or a hook, for a sandbox whose image you can't change.** `hushrun build
  pull` downloads the newest baked image of the repository from the backend's registry, checks it
  as a run would, and puts its store at `/hushrun` (or `HUSHRUN_BUILD_LOWER`). When the store
  there is the one the image holds already, it downloads nothing. A sandbox's setup never fails for
  it:

  ```sh
  hushrun build pull -q || true
  ```

  A template that many sandboxes start from should be right or not be built, so its build pulls
  with `--require-signed` and no `|| true`, and pins what it pulled with `--resolve`, which prints
  `<image>@sha256:…` and downloads nothing:

  ```sh
  hushrun build pull --require-signed --format json > pulled.json
  ```

  The pull reads the registry with a pull token in `HUSHRUN_REGISTRY_TOKEN`, an agent token, or a
  person's sign-in. A session (`hushrun login --session`) reads results, not the registry: a
  sandbox that holds only a session needs a pull token for the pull, and the line says so.
- **As a disk image, for a volume mounted read-only.** `hushrun build bake --erofs store.erofs`
  writes the store as a read-only EROFS image whose root is the store, made without root or
  `mkfs.erofs`. Write it to the volume once (or attach it as the volume's disk), mount it
  read-only, and point runs at it: mounted at `/hushrun` it is read as it is
  (`mount -t erofs -o ro store.erofs /hushrun`); anywhere else, `HUSHRUN_BUILD_LOWER=/mnt/store`.
  Runs write only their own store. Linux reads EROFS from 5.4; where nothing can mount it,
  `fsck.erofs --extract=DIR store.erofs` unpacks it, or use the tar.

  The same store gives the same archive and disk image byte for byte, on any machine; two bakes of
  it differ only in when they were baked and the signature (an unsigned bake with
  `SOURCE_DATE_EPOCH` set gives the same bytes twice). Bake on the platform the sandbox has.

- **In hosted mode** the image lives at `registry.hushrun.com/<org>/<name>`, built from the blocks
  the org's backend already holds, so no block is stored twice: the bake job publishes there with
  `hushrun build bake --from backend --trusted --oci hushrun` (or `--oci
  registry.hushrun.com/<org>/<name>`), and only CI on the default branch can. `docker login
  registry.hushrun.com -u x -p <pull token>`, then
  `COPY --from=registry.hushrun.com/<org>/<name>:hushrun-linux-x64-latest /hushrun /hushrun`. A
  pull token is read-only, for one repository; an owner or admin makes it, and it stops working
  when its maker leaves the org or loses the role. A backend of your own serves its registry on its
  own host.

### C and C++

The objects of a CMake build come from the store when their inputs match (ADR 0097). Some
compiles always run as asked, and the summary's `compiled as asked` line counts each by its reason:

- **C++20 modules.** CMake 3.28 and later scan every C++20 source. With clang, a source that
  imports no module is stored and reused as any other; a module and the sources that import one
  compile every time (`modules`). With GCC every scanned compile does, since GCC writes a second
  file beside each object.
- **Precompiled headers** (`target_precompile_headers`): the header and the sources that use it
  compile every time (`pch`).
- **clang 15 to 18** compile through the launcher as GCC does: without the include graph newer
  clangs write, so a quoted include from another folder is keyed by the source's folder.

Tests a CTest run starts are reused one by one (ADR 0090). So are the tests of a GoogleTest or
Catch2 3 program you start yourself, by its path, with `tests: {mode: reuse, unit: test}`:

```text
$ hushrun run unit -- build/tests/t_calc
hushrun: unit: 1 of 40 tests run, in 1 of 1 test program; 39 already passed on the files they read, and 1 more that already passed runs anyway, as a check
```

hushrun leaves tests out through the framework's own variables (`GTEST_FILTER`, Catch2's
`TESTBRIDGE_TEST_ONLY`) and reads what each test came to from its XML, so the command stays as you
wrote it; GoogleTest or Catch2 says it is filtering. A `--gtest_filter` of your own, or sharding,
keeps every test running and records each. doctest and Catch2 2 read nothing from the
environment: run them through `ctest` with `doctest_discover_tests` or `catch_discover_tests`.

### C and C++ without CMake

Make, autotools, Meson and a handwritten `build.ninja` get the same launcher as CMake, with nothing
to set ([ADR 0119](../adr/0119-c-without-cmake-and-meson-test.md)):

```text
$ hushrun run build -- make -j8                          # in a second worktree
hushrun: build: c/c++: 41 objects: 40 reused, 1 built
```

- **Make and autotools.** Inside `hushrun run`, GNU Make's commands find hushrun's `cc`, `gcc`,
  `clang` (and their C++ names) first on `PATH`, through `MAKEFILES`; each starts the next compiler
  of its name through the launcher. A Makefile that names its compiler by a path, or sets `PATH`
  itself, compiles as before. `./configure` writes the compiler's name, not a path, so a plain
  `make` afterwards is the compiler's own, and has nothing to do. BSD make reads no `MAKEFILES` and
  gets nothing.
- **Meson.** In a repository with a `meson.build`, a `meson setup` inside `hushrun run` names its
  compilers through the same one-line indirection CMake keeps in its cache: outside a run it is the
  compiler. A build folder set up before keeps its compilers, and the run says so: set it up again
  inside `hushrun run` with `meson setup --wipe <folder>`. Its `compile_commands.json` then starts
  with `/bin/sh`, which some editors' language servers can't follow.
- **Ninja.** `hushrun run … -- ninja` on a `build.ninja` you wrote gets the compilers it names on
  `PATH`; `build.ninja` isn't touched, so `ninja -n` has no work after.
- **Configure checks** (autoconf's `conftest.c`, Meson's in `meson-private`) compile as asked and
  are never stored.
- **`meson test`.** With `tests: reuse`, `hushrun run test -- meson test -C build` reuses each test
  program whose files didn't change, as with CTest: hushrun's `meson` on `PATH` names the tests that
  still need to run, after checking that their programs are built from what the plan keyed.

### Python

Pure Python compiles nothing to reuse, and a C extension a package builds as it installs
(setuptools, meson-python) doesn't go through hushrun's C and C++ launcher: setuptools takes its
compiler only from `CC`, which hushrun never sets because CMake, Meson and the Rust `cc` crate read
it too; meson-python starts its own Meson, not the `meson setup` on COMMAND's `PATH` that
hushrun gives the launcher (ADR 0119); and pip's isolated builds compile against headers in a new
temporary folder each time. pip's wheel cache keeps what it built already
([ADR 0110](../adr/0110-unittest.md)). Python's tests are reused one by one under `tests: reuse`,
with pytest, unittest (`python -m unittest`) and Django's `manage.py test`.

### Rust: clippy, checks and doctests

`cargo check` and `cargo clippy` go through the same store as `cargo build`: every target's
check is kept, the programs', tests' and benches' with `--all-targets` too, and a workspace
member's check under clippy is kept apart from rustc's, with its lints, which a reuse prints again.
A `clippy.toml` added where clippy would find it first, or another clippy, compiles the crate again.

```text
$ hushrun run clippy -- cargo clippy --workspace --all-targets -- -D warnings   # a new worktree
hushrun: build: 4 of 9 crates reused, 5 built
```

With `tests: reuse`, `cargo test --doc` runs only the doctests of the libraries whose files
changed: a library's doctests are a unit under its root file (`crates/core/src/lib.rs`), and one
that already passed on what it read isn't handed to rustdoc at all, so nothing of it compiles. What
they read is what the library's unit tests read, its dev-dependencies' sources included, and
`RUSTDOCFLAGS`. Doctests inside a plain `cargo test` still run every time; run them as a check of
their own, as a project that uses nextest already does.

```text
$ hushrun run doctest -- cargo test --workspace --doc --locked
hushrun: doctest: 2 of 5 doctest crates run; 3 already passed on the files they read, and 1 more that already passed runs anyway, as a check
```

hushrun says why and runs the command as it was when it can't tell what a command ran: a
`RUSTDOC` or `build.rustdoc` of your own, `cargo nextest run --archive-file` (its binaries were
built elsewhere), and `--partition count:M/N`, which a binary left out would shift between jobs.
`--partition hash:M/N` gives each test its job by name, and works.

### Rust's tests one by one

With `tests: {mode: reuse, unit: test}` and `cargo nextest run`, hushrun reuses each Rust test on
its own ([ADR 0145](../adr/0145-rust-tests-by-a-static-map.md)), not only whole test binaries. A
test is left out when nothing it can reach changed. What it can reach comes from a static map of
the workspace's sources, made with rust-analyzer's index: the functions it calls, the types it
uses and their `impl` blocks, every implementation of a trait method it calls, and what its macros
expand to. No coverage and no tracing are used. Where the map can't be sure (a procedural macro of
your own, a test that starts one of your binaries), the test keeps its test binary's reads, and the
run says why once.

```yaml
checks:
  test:
    tests: {mode: reuse, unit: test}
```

```text
$ hushrun run test -- cargo nextest run --workspace
hushrun: test: 3 of 3 test binaries run; 0 already passed on the files they read, and so did 5 tests of the others, and 1 more test that already passed runs anyway, as a check
```

Try `mode: observe` first: every test runs, and the last line says how many tests reuse would have
left out. rust-analyzer is optional (`rustup component add rust-analyzer`). Without it, every test
keeps its binary's reads and the run says so. Its index takes a few minutes on a large workspace.
hushrun keeps it in its cache, so a run on files already indexed doesn't start rust-analyzer again.
Plain `cargo test` starts each binary once for all its tests, so it keeps reusing whole test
binaries.

A test that starts a program from a variable (`Command::new(path)`) keeps its binary's reads, since
that program could be one of your binaries. A helper that only ever starts other programs, such as
the real test runner of an adapter test, can say so with a marker. A wrong marker hides a failure,
so the helper must check at run time that the program isn't one of your binaries, and panic when it
is:

```rust
#[doc(alias = "hushrun::starts_no_workspace_binary")]
pub fn command(program: &Path) -> Command {
    // Never a binary this workspace builds: panic rather than let a test pass on stale reads.
    assert!(!is_workspace_binary(program), "{} is a binary of this workspace", program.display());
    Command::new(program)
}
```

The marker covers that function's own `Command::new`, not `env!("CARGO_BIN_EXE_…")`,
`current_exe` or `cargo_bin` inside it. `hushrun why` names the marked functions a test went
through.

### Xcode

Xcode apps (iOS, iPadOS, macOS) reuse compiles through Xcode's own compilation cache, which hushrun
serves. Inside `hushrun run`, an `xcodebuild` in a checkout that holds an Xcode project, workspace
or Swift package gets it with nothing to set up: hushrun starts the cache's service beside the
command and gives Xcode an xcconfig file that turns caching on and maps the checkout's folder, so a
second worktree, a new branch's clone or another machine computes the same keys. Swift, C, C++,
Objective-C and Clang modules are cached; linking, asset catalogs and code signing aren't.

```text
$ hushrun run build -- xcodebuild -scheme App build      # in a second worktree
hushrun: build: xcode: 412 compiles asked: 405 from this Mac's cache, 0 reused, 7 compiled
```

- **Xcode 27.** Older versions crash on cached, prefix-mapped diagnostics, and get nothing; a run
  with an Xcode hushrun doesn't know yet says so in one line. `HUSHRUN_BUILD_XCODE=0` turns it off
  for a run.
- **On this Mac,** what any checkout compiled is kept in the store's `xcode` folder, so a new
  branch's worktree doesn't start from an empty DerivedData.
- **Between machines and in CI,** an org that shares compiled code shares Xcode's compiles too:
  a new branch's CI job reuses what `main`'s CI compiled with the same Xcode build. A compile
  whose output holds your home folder or your checkout's path stays on your Mac.
- **The Xcode app.** `hushrun build xcode on` makes the app's own builds use the same cache: a
  LaunchAgent serves it and sets `XCODE_XCCONFIG_FILE` for the apps you open, so quit and reopen
  Xcode once. `hushrun build xcode status` says whether it is on; `off` undoes it. The app reuses
  what is on this Mac and downloads nothing: a `hushrun run … xcodebuild` is what downloads.
- **Your own xcconfig.** An `XCODE_XCCONFIG_FILE` you set is included first; settings you pass on
  `xcodebuild`'s command line still win.

This is new and its rules are a draft ([ADR 0105](../adr/0105-xcode-compilation-cache.md)): the
ADR lists what a first build on a real Mac should confirm.

### TypeScript

`tsc` reuses what it wrote and printed before, in another worktree, a new branch's clone or CI.
Inside `hushrun run`, in a checkout with a `package.json`, hushrun adds a small hook to
`NODE_OPTIONS`; in a Node process that runs TypeScript's `tsc`, the hook looks for a block before
the compiler loads, and on a miss records what the compiler read, looked at, listed and wrote. A
plain `tsc`, `tsc -p tsconfig.build.json`, `tsc --build` over project references and `tsc
--noEmit` all work, through `npm run`, `npx` or a script:

```text
$ hushrun run build -- npm run build      # "build": "tsc -b", in a second worktree
hushrun: build: typescript: 1 compile: 1 reused, 0 built, saving about 14s
```

- **Keyed by what it read.** The compiler's own file, its arguments and folder, and every file of
  the program by its bytes: sources, declarations under `node_modules`, `lib.*.d.ts`, each
  `tsconfig` of an `extends` chain, each `package.json` it looked at. A file appearing where the
  compiler looked for one and found nothing (a `package.json` with `"type": "module"`, a
  `paths` target) and a new file in a folder an `include` lists both make it compile again.
- **Type checks too.** `tsc --noEmit` prints the same errors and ends with the same status
  without starting, when nothing it read changed.
- **Put in place as files dated now.** A plain `tsc --build` afterwards finds every project up to
  date.
- **`--build` stores only a build from scratch.** Over earlier `.tsbuildinfo` files the compiler
  decides by file dates, which no key holds, so such a build runs as usual and isn't stored: the
  first build of a fresh worktree or CI job is the one shared.
- **Between machines and in CI,** an org that shares compiled code shares these blocks too, by
  Node's platform. A compile whose output names your checkout's path (an absolute `sourceRoot`),
  or that read a file outside the checkout (a `tsc` installed globally, a `node_modules` linked in
  from elsewhere), stays on this machine.
- **TypeScript 4 to 6.** TypeScript 7's `tsc` is native code whose reads hushrun can't see from
  Node yet: it runs as usual. `--watch`, `--extendedDiagnostics`, `--generateTrace` and `tsc -b
  --verbose` run as usual too, and the run's summary says why.
- **A script that sets `NODE_OPTIONS` without what it had** (`NODE_OPTIONS=--max-old-space-size=8192
  tsc`) drops the hook: keep it (`NODE_OPTIONS="$NODE_OPTIONS --max-old-space-size=8192"`).

This is new and its rules are a draft ([ADR 0106](../adr/0106-typescript-build-blocks.md)).

### Elixir

`mix compile` isn't reused or shared: a module's code holds its checkout's absolute path
(`__DIR__`, `__ENV__.file`, what templates read through them), and what a compile read of the
environment (`System.get_env/1` in a module body) isn't known, so another checkout's or machine's
`_build` could run with the wrong files or values ([ADR 0122](../adr/0122-exunit.md)). `mix test`'s
test files are reused (C-114 to C-117).

### PHP

PHP compiles nothing ahead of a run, so there is nothing of it to reuse here. Its tests are reused
instead: with `tests: reuse` (and `unit: test` for each test on its own), a check that runs
PHPUnit 10, 11 or 12 or Pest 3 or 4 (`vendor/bin/phpunit`, `vendor/bin/pest`, `php
vendor/bin/phpunit`, or a `composer test` whose script runs one of them) leaves out of PHPUnit's
suite the test files and tests that already passed on the files they read, with nothing to set
up: hushrun loads its adapter through `PHP_INI_SCAN_DIR` and runs your own bootstrap after it.

- **What a test file read** is what PHP had loaded or opened by the end of its last test, since a
  class loaded once leaves no trace in the next test that uses it. The first test files of a run
  read least; each run that reuses some loads less before the rest, so they read less next time.
- **Keep PHPUnit's result cache in `.gitignore`** (`.phpunit.result.cache`, `.phpunit.cache/`), or
  set `cacheResult="false"`: PHPUnit rewrites it on every run, and a run that changed a file of the
  repository records nothing.
- **Not yet:** Pest's `--parallel`, paratest, `php artisan test`, a PHP started with `-n`, and a
  test PHPUnit runs in a process of its own, which always runs. With coverage on, everything runs.

The rules are [C-90 to C-93](../spec/07-configuration.md) and
[ADR 0116](../adr/0116-phpunit-and-pest.md).

`hushrun guide` says the rest, such as how to read each line, and the rules are
[spec §10](../spec/10-build-blocks.md).

### Svelte

Svelte's tests reuse as any Vitest project's, after the one config line `hushrun init` proposes:
components and runes modules (`.svelte`, `.svelte.js`, `.svelte.ts`) are read through
vite-plugin-svelte's compile, so changing a component runs only the test files, or with
`unit: test` the tests, that rendered it. What a preprocessor reads for a component (a
`<style lang="scss">`'s imports) counts too, and the Svelte config, with what it imports, is read by
every test, as the Vite config is. In a SvelteKit app, `$app/environment` and `$env/*` no longer
make a test file run every time; its env entry (`src/env.*`) and `.env` files are read by every
test. Component tests in jsdom or happy-dom load hushrun's setup file, whatever the framework.

Svelte's compile isn't shared as compiled code: it is a small part of `vite build`, and Vite has
no compile cache for hushrun to serve, so a `vite build` or `svelte-check` check skips or runs
whole ([ADR 0109](../adr/0109-svelte.md)).

### Vue

Vue's compiles get no build blocks: `vue-tsc --noEmit` is a check that a whole run already skips
on code that passed, and `vite build` compiles each component inside one bundler process, with no
per-file compile or cache protocol for hushrun to stand in for. What hushrun does for a Vue project
is its tests: with `tests` on, a component (`.vue`) is a source like a `.ts` file, under Vitest
(`@vitejs/plugin-vue`) and Jest (`@vue/vue3-jest`), so a change to one component runs only the
test files that mounted it.

```text
$ hushrun run unit -- vitest run        # after editing src/Badge.vue
hushrun: unit: 1 of 40 test files run; 39 already passed on the files they read
```

- **What a component is made of** counts as read with it: its `src=` blocks, and, under Vitest,
  the files the types of `defineProps<Props>()` and the other macros came from, whose code never
  runs. A type change that breaks a component runs its tests.
- **`jsdom` and `happy-dom`** environments work: hushrun lets Vite serve its setup file.
- **Rest.** A CSS, HTML or image file a component names, and `tsconfig.json`, run every test file
  when they change. Under Jest, a run whose cache is cold with ts-jest or `lang="ts"` components
  reads everything below the directories `tsconfig.json` includes, so its test files reuse only
  after a run with Jest's cache warm.
- **Not followed.** Nuxt's `@nuxt/test-utils` environment makes modules from no file: its test
  files always run. `@vitejs/plugin-vue-jsx` with `resolveType` reads types hushrun doesn't see.

The rules are C-58 to C-61 ([ADR 0108](../adr/0108-vue-components.md)).

### Kotlin

Kotlin's tests and builds go through Gradle, with the same line in the settings file. Beside
`test`, `jvmTest` and Android's `testDebugUnitTest`, hushrun follows Kotlin Multiplatform's own
test tasks, `jsNodeTest`, `jsBrowserTest`, `wasmJsNodeTest` and native ones such as
`linuxX64Test`: a test class is the file that declares it, so a `commonTest` class is one test
file for every platform it runs on, and a class or test that already passed on the files it read
is left out on all of them, through Gradle's own test filter. Kotest's specs are test files too,
left out whole; their tests, which have no method to name, always run when their spec does.

```text
$ hushrun run unit -- ./gradlew allTests
hushrun: unit: 3 of 12 tests run, in 2 of 5 test classes; 9 already passed on the files they read
```

For compiled code, Kotlin's compile tasks, KSP's and kapt's annotation processing are cacheable and
relocatable, and are shared like any other Gradle task. kapt's stub generation writes its
sources' absolute paths into its output, so it stays on the machine, and the summary names it:

```text
hushrun: build: gradle: 1 task run here stays on this machine (1: its outputs hold the checkout's folder): :app:kaptGenerateStubsDebugKotlin
```

KSP has no such step. A Kotlin/JS or native task leaves out at most 250 tests one by one, and no
class of the default package.

### .NET

`dotnet build`'s compiles aren't reused: since .NET 8 the SDK writes the commit into every
assembly, so no compile could be reused across commits ([ADR 0118](../adr/0118-dotnet-test.md),
decision 11). hushrun reuses .NET's tests, not its assemblies. With
`tests: reuse`, `hushrun run unit -- dotnet test` leaves out each test project that already passed
on the files it and the projects it references compile, unbuilt, and with `unit: test` the tests
that passed of one that didn't ([ADR 0118](../adr/0118-dotnet-test.md)). A project on
Microsoft.Testing.Platform runs every time, and the run says so. Add `TestResults/` to `.gitignore`
when COMMAND writes TRX files there: a run that leaves new files in the checkout isn't recorded.

### Ruby's tests

With `tests: reuse` on a check that runs RSpec or Minitest, a spec or test file that already passed
on the files it read doesn't run again:

```yaml
checks:
  test:
    tests: reuse
```

```text
$ hushrun run test -- bundle exec rspec
hushrun: test: 2 of 40 test files run; 38 already passed on the files they read
Run options: exclude {:hushrun_reused=>"already passed on the files it read"}
```

`rspec`, `bin/rails test`, `rake test`, `rake spec` and `ruby -Itest …`, alone or after
`bundle exec`, load hushrun's adapter through `RUBYLIB` and `RUBYOPT`; no file of the repository
changes, and a script that sets `RUBYOPT` should add to what it had (`RUBYOPT="$RUBYOPT -W0"`).
A test file read the repository's code that ran for it, what that code requires, `require_relative`s
and opens, and what spec_helper or test_helper loaded. In a Rails app, what Zeitwerk, an
initializer or a fixture loaded while one test file ran is read by every later one, since the app
keeps it; with `config.eager_load` on in the test environment (Rails' default when `CI` is set)
every file of `app/` is, and only changes to test files skip anything. `tests: {mode: reuse, unit:
test}` reuses each RSpec example and Minitest test on its own. Ruby 3.1 or later; Spring and
`rspec --only-failures` aren't observed, and say so.

## When hushrun runs anyway

Whenever hushrun can't tell whether this code passed, it runs the command and says why:
outside a git repository, with results turned off, without the login a backend needs, with an
`external` input it can't read, without `--scope` among several scopes, or when the backend can't be
reached within 10 seconds or refuses the token. At worst, a doubt costs a
run; it never skips a check it shouldn't. Without a login, at a terminal, it says only how to
sign in.

It records nothing when files change while the command runs, since the result would be for
neither version: list what the check writes, such as `coverage/`, in `.gitignore`.
`hushrun run --force -- vitest run` runs it even after a pass.

When a pass of this code would have skipped a run but for one rule, as `--force`, a later step
that reads the check's reports, or a job that doesn't say which of its variants it is, the run's
result says which, `ranAnyway`, and the dashboard's "Could have skipped" counts those runs by
reason, with what lets them skip. A run whose verdict isn't the one of the result that decided
it here says it may be flaky on its last line, and on GitHub in the step summary:
`hushrun: test failed in 12s (exit 1); it passed on this exact code before, so it may be flaky`.

When the org's plan has recorded all the checks it includes this period, the check still runs
and exits as it would, and hushrun ends with `hushrun: test passed in 41s; not recorded: <the
server's message>; upgrade: <where to upgrade>`. Skips and lookups are free, so earlier passes
still skip; a check that wasn't recorded runs again next time. `hushrun repository usage` shows
where the org stands, and this repository's share ([ADR 0032](../adr/0032-quota-refusals-fail-open.md)).

The rules, with the run key and the record each run leaves, are
[spec §7.6](../spec/07-configuration.md#76-hushrun-run-with-a-command) (C-9 to C-14) and
[`run-result.v0.schema.json`](../schemas/run-result.v0.schema.json), and `hushrun token` and
`hushrun whoami` are C-17. Why a repository may name its own backend, and how its CI connects
without a secret, is [ADR 0023](../adr/0023-a-repositorys-backend-and-the-hosted-default.md);
agent tokens are [ADR 0027](../adr/0027-agent-tokens.md). `hushrun ready` and the hook are C-22 and
C-23, and [ADR 0036](../adr/0036-agents-told-before-they-push.md) says why they work as they do.
