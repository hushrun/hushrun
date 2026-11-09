# Adopting hushrun on GitHub Actions

This guide sets up a repository that deploys from `main` so that each deploy runs only what these
exact files haven't passed yet, and deploys an image already built and checked for them by digest
instead of rebuilding it. Plan on about 30 minutes. The examples use hushrun's hosted server, and
Google Cloud for the images (Artifact Registry, Cloud Run); other clouds follow the same steps.

A repository has one pipeline (ADR 0062): no stages, and nothing moves a change from one
environment to the next. A re-run or a revert deploys the build it already has, and a push of
files the repository already runs deploys nothing.

Nobody maintains a list of who may sign. The events that count are those signed by your org's
members and agents, from machines they registered when they signed in, and by this
repository's own CI, which signs keylessly with GitHub's ID token (spec T-15). hushrun's server
says who those are; `hushrun.yaml` pins the server's log key, so it can't change its answer
silently.

To skip checks this exact code already passed, which takes one command, start with
`npx hushrun init` ([run.md](run.md)) instead. It also gets the repository's URL on hushrun, which step 3 uses.

## What you need

- **Admin rights on the repository.**
- **An org on hushrun**, and the repository's URL there: `https://<host>/l/<org>/<repository>`,
  one per repository (spec L-1). There is nothing to set up in your cloud for it.
- **The registry your images already use.**
- **A build that bakes in nothing a later deploy should change** (A-7). A reused image keeps the
  values it was built with. Next.js `NEXT_PUBLIC_*`, Vite `VITE_*` and similar values break
  that; `hushrun doctor` looks for them.

## 1. Install hushrun and run `init` (3 minutes)

Download a release for your machine from the repository's Releases page, or use the shell
installer it lists; `hushrun update` later replaces what the installer installed with the newest
release. Then, in the repository:

```bash
hushrun init --pipeline --image us-central1-docker.pkg.dev/my-project/app/api
```

`init --pipeline` writes a starter `hushrun.yaml`: one `unit` check, the image at its `location`,
and a `deploy` block that deploys from `main` (`--branch` names another) and requires `unit`.
To have deploys count checks, artifacts and lifecycle events only when this repository's CI
signed them, so that nothing a laptop or an agent records decides a deploy, turn on **CI reuses
only CI's results** on the repository's settings page in the dashboard (T-16). It isn't in
`hushrun.yaml`, so no pull request can drop it. `hushrun policy explain` shows whether the
server makes the repository CI-only, since when, and whose events count for each required check.
There are no keys to make or keep.

## 2. Edit `hushrun.yaml` (5 minutes)

- **`checks`**: the checks a deploy requires, with the command `hushrun run` executes. Add
  `max_age` to checks that should be re-run after a while.
- **`scopes`**: leave them out for one deployable. For a monorepo, one scope per service, each
  covering everything that determines its artifact *and its deployment*: build files, the
  Dockerfile, the deploy workflow (D-7).
- **`digest.exclude`**: files that never affect what ships, such as `**/*.md`. A commit that
  changes only those deploys nothing new.
- **`deploy`**: the `branch` it deploys from, and in `require` the checks a deploy needs; without
  `require`, it needs every check with a `run` (C-7).

A `hushrun.yaml` from hushrun 0.8 or earlier still works with `hushrun run`, `ready` and the
hook: `stages`, `kind` and `locations` are ignored, with a warning that names their line. `plan`
refuses it until they are gone (C-1): move the stage's `branch`, `require` and `lease` to
`deploy`, and give each artifact one `location`.

Commit it.

## 3. Get the repository's URL (5 minutes)

The repository's URL is `https://<host>/l/<org>/<repository>`: CI appends to it, and never
overwrites or deletes anything ([ledger-access.md](ledger-access.md)). `npx hushrun init`,
run by an org admin in the repository, makes it, prints its page, and writes it as
`repository_url:` in `hushrun.yaml`, with the server's log key as `transparency_log` (spec T-9).
With `gh` signed in, it also sends GitHub's id for the repository, so that the server names
this repository's GitHub Actions as its CI (T-15).

There is no root to create, and nothing to sign, upload or merge before events count
(ADR 0031). Who counts is the org's members and agents, and this repository's CI, as the server
says. Protecting `main` is still good practice, since a push there runs the deploy, but hushrun
doesn't depend on it.

## 4. Configure GitHub (2 minutes)

Every job that runs hushrun needs `permissions: id-token: write`: GitHub's ID token signs events
keylessly as the repository's CI, and authenticates the jobs to hushrun's server. No secret is
needed.

In CI, hushrun reads the repository's URL from `hushrun.yaml` (T-4), so no variable is needed.
To pin it in CI's settings, set `HUSHRUN_REPOSITORY_URL` in **Settings → Secrets and variables →
Actions → Variables** to `https://<host>/l/<org>/<repository>`; it must match.

## 5. Add the workflow (5 minutes)

`hushrun adopt` writes it: `.github/workflows/hushrun.yml`, one short workflow that calls the
reusable workflow [`hushrun-deploy.yml`](../../.github/workflows/hushrun-deploy.yml), with your
build and deploy commands to fill in. `--branch` names the branch it deploys from, and
`--image [ARTIFACT=]REPOSITORY` an image's repository. Or copy
[`recipes/github/hushrun.yml`](../../recipes/github/hushrun.yml); for a monorepo,
[`recipes/github/monorepo.yml`](../../recipes/github/monorepo.yml), which calls it once per
scope. Then set:

- the commit of `hushrun-deploy.yml` (a release's). Each run installs the newest hushrun release:
  the plan job installs it, and every later job of the run installs the same one. To install one
  release until you change it, set `version`, such as `version: 0.9.0` (ADR 0035);
- `build`, which pushes the image to `$REF`, and `deploy`, which deploys `$REF`, pinned by
  digest; `setup` runs before both, for example to log in to the registry;
- the cloud credentials for your registry and your deploy, through workload identity federation:
  `gcp-workload-identity-provider` and `gcp-service-account`, `aws-role-to-assume` and
  `aws-region`, or the `azure-*` inputs. No job needs a cloud secret, and hushrun's server needs none:
  its jobs present GitHub's ID token;
- `signing: sigstore`, so jobs sign keylessly as the repository's CI, and `deploy-environment`,
  your usual deploy environment.

The reusable workflow holds the recipe's structure: the plan job decides once, the test job
runs exactly the plan's checks, the deploy job guards, resolves, builds and attests only when
the plan says so, deploys, records `deployed` and then `passed`, and the report jobs record
`superseded` or `failed` when a run is cancelled or fails. Every later job carries out the plan
through `HUSHRUN_PLAN_JSON`. A fix to these jobs reaches your repository as a new commit of
`hushrun-deploy.yml`. Writing your own workflow instead? Start from `hushrun-deploy.yml`'s jobs,
and never check out `ref: main`: every job must see the revision the plan was made for, and
`guard` catches a branch that moved on. A check that ran against a deployment, such as end-to-end
tests on the environment, runs in your `deploy` step, or is attested before `mark passed`.

## 6. Check and go (3 minutes)

```bash
hushrun doctor --repository-url https://<host>/l/<org>/<repository>
```

`doctor` checks the configuration, the server and its log key, who counts as a signer (the
org's machines and the repository's CI), this machine's signer and the timestamp authorities, registry access, the deploy
workflow's scope and baked configuration.

Push to `main`. The first run plans `full`: it runs the checks, builds, deploys, and records each
step. Re-run it, or revert to files an earlier run passed: it plans `promote`, and deploys the
image it already built and checked, by digest, with no build and no checks to repeat.
A push of files the repository already runs plans `current`, and deploys nothing. The step
summary of each plan job shows the decision and why.

## Day to day

- **A plan says `blocked`**: the reason says what to do, such as "this repository deploys only
  from main; this run is on fix-login, so run it from main". A blocked plan fails the workflow on
  purpose.
- **Forcing a full build**: run the workflow by hand with `force_full`. Forced runs rebuild and
  re-run every required check; with `allow_force: false` in `deploy`, the plan refuses.
- **`refresh`**: evidence expired (`max_age`) or a check's definition changed. Only those checks
  run, and the image is still reused.
- **A newer commit landed while deploying**: `guard` records `superseded` and the remaining
  deploy steps skip. The newer run deploys instead.
- **Re-running failed jobs**: a re-run is a new attempt of the same run; hushrun records it as
  such.
- **Old events**: `hushrun repository gc --older-than 180d --dry-run`, as an org admin.
- **The org's plan is used up**: new check events aren't recorded; `run` and `attest` say so
  once, with where to upgrade, record everything else, and exit as they would, so deploys go on.
  `hushrun repository usage` shows where the org stands (ADR 0032).
- **Pull requests**: a job on `pull_request` reads `hushrun.yaml` from the branch it merges into,
  never from the change (T-5): fetch it and pass `--config-ref origin/$GITHUB_BASE_REF`, or
  hushrun refuses to plan or record.
- **Pull requests from forks** never record: GitHub gives a fork's job no ID token. With no
  credential in GitHub Actions, hushrun asks as the repository's CI (ADR 0052), and hushrun's
  hosted service answers for a public repository approved in its open-source program, from what
  the repository's own CI recorded: a pull request that changes one package skips the others'
  checks. Anywhere else, a fork's checks run. The run's last line says which, and running it again
  runs what it ran. Most of the savings come from `main`, merge queues and maintainers' own
  branches.
- **Matrices and reusable workflows**: in a job with several variants, init writes
  `HUSHRUN_VARIANT: ${{ toJSON(matrix) }}` in its `env`, so each variant skips only on its own
  pass. A workflow of this repository that another calls with `uses: ./.github/workflows/<file>`
  gets `HUSHRUN_WORKFLOW: <its own path>` in each job that runs a check, since GitHub names the
  caller's file in `GITHUB_WORKFLOW_REF`, and `HUSHRUN_INPUTS: ${{ toJSON(inputs) }}` when it takes
  inputs, so each call skips on its own pass; the inputs show nowhere. The calling job gets
  `id-token: write`, since a called workflow has no more permissions than its caller gives it. A
  workflow another repository holds can't be read, and its jobs run every time
  ([In any language](run.md#in-any-language)).
- **Reports**: in a job where a later step reads a check's reports, such as
  `codecov/codecov-action` or an artifact upload of `test-results/`, CI runs the check every time
  so that the step finds them, and says why; laptops and agents still skip it on a pass. hushrun
  reads this from the workflow file the job runs. `reports.ci: skip` for the check in
  `hushrun.yaml` lets CI skip when your readers handle a skip
  ([Let CI skip a check whose reports are read](run.md#let-ci-skip-a-check-whose-reports-are-read)).
- **Step summary and outputs**: a skip writes a step summary with when and where its pass ran,
  linked to that run when it is this repository's, what the skip saved, and the pass's tests and
  coverage. Every `hushrun run` writes the step outputs `hushrun-result` (`pass`, `fail` or
  `skip`), `hushrun-skipped` (`true` or `false`), `hushrun-tests`, `hushrun-tests-failed` and
  `hushrun-coverage-lines`; give the step an `id:` to read them, as
  `steps.<id>.outputs.hushrun-skipped`. In a step that runs several checks, the last run's values
  win. `--no-ci-output` writes neither
  ([What a pass's reports said](run.md#what-a-passs-reports-said)).
- **People and agents**: a teammate's evidence counts from their first `hushrun login`, and stops
  counting when they leave the org or log out; events they signed before keep counting. Agents
  validate before pushing with `hushrun run --isolated`; see [the agent guide](agents.md). Set the
  reusable workflow's `isolated: true` so CI runs checks in the same pinned images.

## Merge queues

A check's key comes from the files it reads, not from the branch or the event, so a merge queue
needs nothing of hushrun but the event in the workflow:

```yaml
on:
  pull_request:
  merge_group:
  push:
    branches: [main]
```

- **The queue's run skips** when the pull request's run passed on the same files: when nothing
  merged into `main` since, the queue tests the same tree. A pass counts in its own job, by the
  job's `id` and the system it runs on, so the queue's job must have the pull request's job's,
  as it does when one workflow has all three events.
- **When `main` moved**, the queue's tree holds the other changes too, so a check whose scope
  they touch runs, and one whose scope they don't still skips.
- **The push to `main`** that merging makes holds the queue's tree, and skips on the same pass.
  A skip records no pass of its own.
- **Its runs say what they test.** In a `merge_group` run, `hushrun doctor` names the pull
  request and the branch it merges into, from the queue's ref,
  `gh-readonly-queue/<base>/pr-<n>-<sha>`. That ref is the run's branch, so `deploy.branch`
  never takes a queue's run for its base branch's. A queue's run reads `hushrun.yaml`
  from the commit it tests, as a push does, not from the base branch as a pull request does.

## Keyless signing

Jobs sign keylessly: each asks GitHub for an ID token, Fulcio certifies a key that lives only in
that job for ten minutes, and a timestamp authority timestamps every signature. Nothing is
secret. The certificate names the repository's numeric id, and the server names GitHub Actions
with that id as the repository's CI, so the events count as `ci` (spec T-12, T-15; ADR 0013).
Any workflow of the repository signs as its CI, except a `pull_request_target` run, which runs
the base branch's workflow for a change that hasn't merged. In CI, hushrun checks that the
server's CI is this job's repository (`GITHUB_REPOSITORY_ID`), and exits 2 otherwise.

Public-good Sigstore publishes every certificate it issues, and with it the repository's and
the workflow's names. For a private repository that must not appear there, use a private
Sigstore instance that `HUSHRUN_FULCIO_URL`, `HUSHRUN_TSA` and `HUSHRUN_SIGSTORE_TRUSTED_ROOT`
name, or sign with an agent's machine key ([agents.md](agents.md)); events signed that way are
an agent's, not the CI's, so a CI-only repository doesn't count them.

Give `id-token: write` to every job that runs `hushrun` and writes: `plan --start`, `run`,
`attest`, `guard`, `artifact resolve`, `mark` and the report jobs. `HUSHRUN_SIGNING: sigstore`
is the default for a job with an ID token and no key. `hushrun doctor` in a job reads the ID
token and says whether it signs as the repository's CI, and whether the timestamp authority
answers.

When Fulcio or the timestamp authority is unreachable, a signing step exits 9 and records
nothing; re-run the job. Reading needs neither: verification is offline, against the Sigstore
trust root in the binary, or the newer one `hushrun sigstore refresh` caches.

## More on the server

The repository's server (spec L-1, L-10) needs no cloud credentials. Every job that runs
hushrun asks GitHub for an ID token, as the reusable workflow's jobs do:

```yaml
jobs:
  plan:
    runs-on: ubuntu-latest
    # id-token: write lets hushrun sign keylessly, and request an ID token whose audience is
    # the repository's URL.
    permissions: { contents: read, id-token: write }
    steps:
      - uses: actions/checkout@v4
      - uses: hushrun/hushrun/action/setup@main        # pin to a release commit
        # The newest hushrun release; `with: { version: 0.2.0 }` installs that one until you
        # change it, and `sha256` checks it against the checksum you give.
      - id: plan
        run: hushrun plan --start
```

With the writer role, CI appends events and reads everything, but can't delete anything (L-6).
Org admins use an API token from the repository's settings, in `HUSHRUN_REPOSITORY_TOKEN`, for
monitors and scripts. Never commit a token: hushrun reads them only from the environment.

`hushrun doctor` checks that the server speaks hushrun's protocol, holds this repository, and
calls itself by the URL hushrun uses, which is the ID tokens' audience, and summarizes who
counts: `hushrun repository signers` prints the server's signed answer, and `--out FILE` saves
it for `hushrun verify --signers FILE`, offline.

### The transparency log

The hosted server appends every write to an append-only Merkle log and signs its checkpoints,
and its answers about who counts, with a log key (spec L-11, L-13). `hushrun.yaml` names that
key at the top level, so the server can't vouch for itself (T-9); `npx hushrun init` writes
it when the server tells it, and `hushrun doctor` shows the lines otherwise:

```yaml
transparency_log:
  origin: <host>/l/<org>/<repository>
  key: "ed25519:…"
```

From then on every command checks what the server answers: each write's receipt, each listing's
signed statement, the signers answer, and that every checkpoint it meets, its own and those
inside events, is one history; anything else exits 6. `plan --start` records the listings its
decision read in its `started` event, and `hushrun mark deployed` needs that event, as the
recipes already do. Then monitor the log from your own CI with
[`recipes/github/monitor.yml`](../../recipes/github/monitor.yml): `hushrun repository monitor`
replays the log every hour and exits 5, with the server's own signed statement as proof, if any
plan was shown a listing that left out an event the log held, or if the server committed to a
map of the log that leaves one out.

Until `hushrun.yaml` names the key, hushrun checks nothing and warns on every command. GC signs
each deletion with an org admin's or owner's signed-in machine: `hushrun repository gc
--older-than 180d`, run by an admin after `hushrun login`.
