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

- network access that reaches `api.hushrun.com`, as an allowed domain or with full access,
  and the npm registry;
- a setup script that installs the dependencies, which installs hushrun with them: `npm ci`.

Each session's `npm test` then goes through `hushrun run`, and skips what another session, a
teammate or CI already passed on the same files.

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
