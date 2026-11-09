# Adopting hushrun on GitLab CI

This guide sets up a GitLab project that deploys from `main` so that each deploy runs only what
these exact files haven't passed yet, and deploys an image already built and checked for them by
digest instead of rebuilding it. A project has one pipeline (ADR 0062): no stages, and nothing
moves a change from one environment to the next. Plan on about 30 minutes. The examples use hushrun's hosted server, and
Google Cloud for the images (Artifact Registry, Cloud Run); other clouds follow the same steps.

## What you need

- **The Maintainer role on the project**, with `main` protected (*Settings → Repository →
  Protected branches*). Protected variables reach only protected branches, so the agent's token
  and machine key below reach only the deploy branch's pipelines.
- **GitLab 17.0 or later**, on GitLab.com or self-managed: ID tokens with `ci_config_ref_uri`, and
  `after_script` when a job is cancelled.
- **An org on hushrun**, the project's URL there (spec L-1), and the registry your images
  already use.
- **A build that bakes in nothing a later deploy should change** (A-7). A reused image keeps the
  values it was built with. Next.js `NEXT_PUBLIC_*`, Vite `VITE_*` and similar values break
  that; `hushrun doctor` looks for them.
- **Linux runners.** The install script covers Linux and macOS; the container image is
  `linux/amd64` and `linux/arm64`.

## 1. Install hushrun and run `init` (3 minutes)

Download a release for your machine from the repository's Releases page. Then, in the project:

```bash
hushrun init --pipeline --image us-central1-docker.pkg.dev/my-project/app/api
```

`npx hushrun init` (without `--pipeline`) sets up `hushrun run` and, once the project
has a URL on hushrun, writes `hushrun.yaml` naming it and the server's log key. It sets no GitLab
protection: protect the default branch yourself.

`hushrun init --pipeline` recognizes a GitLab project by its `.gitlab-ci.yml` or its host. It
writes a starter `hushrun.yaml`, one `unit` check, the image at its `location` and a `deploy`
block from `main` (`--branch` names another), and no keys: who counts is the org's members and
agents, and the project's CI when the server verifies its ID tokens (spec T-15, ADR 0031). It
prints the steps below in short.

## 2. Edit `hushrun.yaml` (5 minutes)

- **`checks`**: the checks a deploy requires, with the command `hushrun run` executes.
  Add `max_age` to checks that should be re-run after a while.
- **`scopes`**: leave them out for one deployable. For a monorepo, one scope per service, each
  covering everything that determines its artifact *and its deployment*: build files, the
  Dockerfile, `.gitlab-ci.yml` and anything it includes (D-7).
- **`digest.exclude`**: files that never affect what ships, such as `**/*.md`. A commit that
  changes only those deploys nothing new.
- **`deploy`**: the `branch` it deploys from, and in `require` the checks a deploy needs; without
  `require`, it needs every check with a `run` (C-7).

A `hushrun.yaml` from hushrun 0.8 or earlier still works with `hushrun run`, `ready` and the
hook: `stages`, `kind` and `locations` are ignored, with a warning that names their line. `plan`
refuses it until they are gone (C-1).

Commit it.

## 3. Get the project's URL, and the pipeline's agent (8 minutes)

The project's URL is `https://<host>/l/<org>/<repository>`: CI appends to it, and never
overwrites or deletes anything ([ledger-access.md](ledger-access.md)). Get it with
`npx hushrun init`, as [the GitHub guide's step 3](github.md#3-get-the-repositorys-url-5-minutes)
describes; it writes it in `hushrun.yaml` as `repository_url:`, with the log key. Commit it:
that is where the pipeline reads it.

The hosted server verifies GitHub Actions' ID tokens but not GitLab's yet, so GitLab pipelines
sign and write as one of the org's agents. An org admin creates its token, which comes with a
machine key registered with it:

```bash
hushrun token create --agent gitlab-ci --expires 365d
```

At a terminal, it prints the token (`hra_…`) and the machine key, once (step 4); in a script,
`--format json` gives them, since hushrun keeps a key out of a pipe that could be a log. A token lasts 30 days unless
`--expires` says otherwise, 365 days at most. With both in the job, hushrun needs nothing else:

- **The token** goes with every request to the project's URL that `hushrun.yaml` names, since
  that URL is on hushrun's host, which takes an agent token for its org only. No other token
  follows a URL that only a file in the repository names. On a server of your own, set
  `HUSHRUN_HOST` to its origin in the project's variables, so that the token goes there too.
- **The machine key** signs every event, since the job has no keyless identity the server
  counts. Events the pipelines sign with it count as the agent's (`agent:gitlab-ci`), until an
  admin revokes the token; those signed before keep counting.

### Cloud access

hushrun's server needs no cloud credentials. Your registry and your deploy do: give CI those roles
through workload identity federation with GitLab's ID tokens, with no long-lived cloud key. On
Google Cloud:

1. Create a workload identity pool with an OIDC provider for issuer `https://gitlab.com`. Map
   `google.subject=assertion.sub`, `attribute.project_id=assertion.project_id` and
   `attribute.ref=assertion.ref`, and set the condition
   `assertion.project_id == "PROJECT_ID" && assertion.ref_protected == "true"`.
2. Let the pool's principals impersonate the service account that holds the registry and deploy
   roles.
3. In the recipe's `.hushrun` template, ask for an ID token and write the credential file that
   Google's tools read. GitLab's `extends` replaces a `before_script` rather than merging it,
   which is why the recipe keeps every hushrun job's steps in that one template:

   ```yaml
   .hushrun:
     id_tokens:
       GCP_ID_TOKEN:
         aud: https://iam.googleapis.com/projects/PROJECT_NUMBER/locations/global/workloadIdentityPools/gitlab/providers/gitlab
     variables:
       GOOGLE_APPLICATION_CREDENTIALS: $CI_BUILDS_DIR/gcp-credentials.json
     before_script:
       - printf '%s' "$GCP_ID_TOKEN" > "$CI_BUILDS_DIR/gcp-id-token.jwt"
       - |
         cat > "$GOOGLE_APPLICATION_CREDENTIALS" <<EOF
         {"type": "external_account",
          "audience": "//iam.googleapis.com/projects/PROJECT_NUMBER/locations/global/workloadIdentityPools/gitlab/providers/gitlab",
          "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
          "token_url": "https://sts.googleapis.com/v1/token",
          "credential_source": {"file": "$CI_BUILDS_DIR/gcp-id-token.jwt"},
          "service_account_impersonation_url": "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/deployer@PROJECT.iam.gserviceaccount.com:generateAccessToken"}
         EOF
       - |
         have=$(hushrun --version 2>/dev/null || true)
         if [ -z "$have" ] || { [ -n "${HUSHRUN_VERSION:-}" ] && [ "$have" != "hushrun $HUSHRUN_VERSION" ]; }; then
           bin=$(sh ci/install-hushrun.sh)
           export PATH="$(dirname "$bin"):$PATH"
         fi
   ```

On AWS, declare an ID token with `aud: sts.amazonaws.com`, write it to a file, and set
`AWS_ROLE_ARN` and `AWS_WEB_IDENTITY_TOKEN_FILE`. On Azure, use `aud: api://AzureADTokenExchange`
with `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` and `AZURE_FEDERATED_TOKEN_FILE`. Bind each cloud's
access to the project's id and a protected ref, as the condition above does.

## 4. Configure GitLab (6 minutes)

In *Settings → CI/CD → Variables*:

| Variable | Type | Flags | Value |
|---|---|---|---|
| `HUSHRUN_TOKEN` | Variable | Masked, protected | the agent's token (`hra_…`) |
| `HUSHRUN_MACHINE_KEY_FILE` | File | Protected | the agent's machine key (PEM) |
| `HUSHRUN_GITLAB_TOKEN` | Variable | Masked | a project access token with `read_api` |

- **The key is a File-type variable**, because a PEM key can't be masked: GitLab writes it to a
  file beside the project's directory, outside the repository, and `HUSHRUN_MACHINE_KEY_FILE`
  holds its path. Every job of the pipeline signs as the agent with it; the recipe sets nothing
  for signing.
- **`HUSHRUN_GITLAB_TOKEN`** lets `hushrun doctor` read whether `main` is protected and who may
  set pipeline variables. Create it in *Settings → Access tokens* with the `read_api` scope;
  reading who may set pipeline variables takes the Maintainer role. A pipeline that recorded no
  outcome counts as running until its lease ends (`deploy.lease`, P-6).
- **No `HUSHRUN_REPOSITORY_URL`** is needed: the pipeline reads the URL from `hushrun.yaml`. Set
  it only to pin the URL in the project's settings too; it must then be the one `hushrun.yaml`
  names.
- **Pipeline variables** that someone sets when running a pipeline outrank project variables,
  so whoever may set them could point `HUSHRUN_REPOSITORY_URL` or `HUSHRUN_HOST` elsewhere, or
  change how jobs sign. Set *Minimum role to use pipeline variables* to *No one allowed*, or to
  *Maintainer* if you force full builds (below). `hushrun doctor` reports the setting when its
  token may read it.

## 5. Add the pipeline (5 minutes)

Copy [`recipes/gitlab/.gitlab-ci.yml`](../../recipes/gitlab/.gitlab-ci.yml) to your project's
root, and `install-hushrun.sh`, attached to each release, to `ci/install-hushrun.sh`. Read the
script before you commit it: it installs a release for the runner's OS and architecture, and
checks the archive against the checksum it pins for that release, or for a release it doesn't
list, such as a newer one, the checksum published with it. Then set:

- the `test` job's image, the toolchain your checks need;
- the `deploy` job's image and its scripts: `./build.sh "$HUSHRUN_REF"` pushes the image to
  `$HUSHRUN_REF`, and `./deploy.sh "$HUSHRUN_REF"` deploys it, pinned by digest;
- your cloud's credential step in `.hushrun`, as in step 3.

Each pipeline runs the newest hushrun release: the plan job runs the image's `latest`, and
passes its version on in its dotenv report, so every later job installs the same release, even
when a newer one comes out meanwhile (ADR 0035). To install one release until you change it,
set `HUSHRUN_VERSION`, such as `"0.9.0"`, and `HUSHRUN_IMAGE` to that release, pinned by the
digest its release notes give: `ghcr.io/hushrun/hushrun:0.9.0@sha256:…`.

The recipe holds the structure of plan §15.4:

- the plan job decides once and passes the plan on as `plan.json`, with its outputs in a dotenv
  report (`HUSHRUN_MODE`, `HUSHRUN_CHECKS`, …, and the release it ran, `HUSHRUN_VERSION`);
- the test job runs exactly the plan's checks;
- the deploy job guards, resolves, builds and attests only when the plan says so, deploys, and
  records `deployed` and `passed`; `resource_group: deploy` keeps to one deploy at a time. A
  check that ran against a deployment, such as end-to-end tests on the environment, runs in the
  deploy script, or is attested before `mark passed`;
- `mark-failed` records `failed` when a job failed or timed out, and a job that GitLab cancels
  records `superseded` from its `after_script`.

Every job carries out the plan: never check out another ref, and never pass `--rev`. Each
command whose output a job evaluates goes through a variable
(`out=$(hushrun guard … --format env); eval "$out"`), because `eval "$(hushrun guard …)"`
succeeds when hushrun fails.

Jobs that only run hushrun use the container image `ghcr.io/hushrun/hushrun` (Alpine, git, CA
certificates and the static binary): `latest`, the newest release, or the version you pin. Other
jobs install the same release with the script. The image and the release archives are public;
for runners that can't reach GitHub, mirror them where the runners can read them, pin
`HUSHRUN_VERSION`, and point `HUSHRUN_IMAGE` and `HUSHRUN_DOWNLOAD_URL` there
([deploy/image](../../deploy/image/README.md)).

## 6. Check and go (3 minutes)

From a job, or locally with `HUSHRUN_GITLAB_TOKEN` and `CI_PROJECT_PATH` set:

```bash
hushrun doctor
```

`doctor` checks the configuration, the server and its log key, who counts as a signer, this
job's signer (in a job, that the server lists the agent's machine key for `agent:gitlab-ci`) and
the timestamp authorities, registry access, that `.gitlab-ci.yml` is inside every scope, baked
configuration, and on GitLab the API, whether the deploy branch is protected, and who may set
pipeline variables.

Push to `main`. The first pipeline plans `full`: it runs the checks, builds, deploys, and records
each step. Run a new pipeline on the same files, or revert to files an earlier pipeline passed:
it plans `promote`, and deploys the image it already built and checked, by digest, with no build
and no checks to repeat. A push of files the project already runs plans `current`, and deploys
nothing. The plan job's log shows the decision and why.

## Day to day

- **A plan says `blocked`**: the reason says what to do, such as "this repository deploys only
  from main; this run is on fix-login, so run it from main". A blocked plan fails the pipeline on
  purpose.
- **Forcing a full build**: with *Minimum role to use pipeline variables* set to *Maintainer*, a
  Maintainer runs the pipeline for `main` with `HUSHRUN_FORCE_FULL=true`. Forced runs rebuild
  and re-run every required check; with `allow_force: false` in `deploy`, the plan refuses.
- **Retrying jobs**: GitLab keeps the pipeline's id. A retried plan job starts a new attempt; a
  retried later job continues its plan's attempt, and a `passed` it records after the first
  try's `failed` wins (P-6).
- **A newer commit landed while deploying**: `guard` records `superseded` and the deploy job
  stops. The newer pipeline deploys instead.
- **The agent's token expires**, a year after `--expires 365d`: before then, create the next one
  under the same agent, `hushrun token create --agent gitlab-ci --expires 365d`, which comes with
  a new key, replace `HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY_FILE`, and revoke the old token with
  `hushrun token revoke`. Events signed with the old key keep counting, when timestamped.
- **Old events**: `hushrun repository gc --older-than 180d --dry-run`, as an org admin.
- **The org's plan is used up**: new check events aren't recorded; `run` and `attest` say so
  once, with where to upgrade, record everything else, and exit as they would, so deploys go on.
  `hushrun repository usage` shows where the org stands (ADR 0032).
- **Reports**: nothing reads `.gitlab-ci.yml` at run time, so a job whose `artifacts:reports`, or
  artifacts of reports, need what its checks write says so in its variables,
  `HUSHRUN_REPORTS_READ: "true"`: every check of that job then runs on a pass, so the reports are
  there, and laptops and agents still skip. `hushrun init` writes it into the jobs it changes, and
  says to add it by hand where a job's variables come through `<<` or an anchor; add it to a job
  init doesn't change, or set it as a CI/CD variable of the jobs that need it
  ([Reports a later step reads](run.md#reports-a-later-step-reads)).

## Keyless signing

GitLab's ID tokens can sign keylessly: Fulcio certifies a key that lives only in the job, for
the project's id and ref, and a timestamp authority timestamps every signature. Such events count
as the project's CI once its server lists GitLab's issuer with the project's id (spec T-15);
the hosted server doesn't yet, so until it does, sign with the agent's machine key above.
Public-good Sigstore publishes every certificate, with the project's path in it; a self-managed
GitLab needs a private Sigstore instance that trusts its issuer, which `HUSHRUN_FULCIO_URL`,
`HUSHRUN_TSA` and `HUSHRUN_SIGSTORE_TRUSTED_ROOT` name. Set `HUSHRUN_SIGNING: sigstore`, as
below: with the agent's machine key in the job too, hushrun signs with the key unless told
otherwise, since only GitHub Actions is a repository's keyless CI by default.

```yaml
.hushrun:
  id_tokens:
    SIGSTORE_ID_TOKEN:
      aud: sigstore
  variables:
    HUSHRUN_SIGNING: sigstore
```

hushrun refuses a token whose audience isn't `sigstore`, or that expired, before sending it
anywhere (exit 2 and 9). A merge request pipeline signs for its source project and branch, so a
fork's never counts. `hushrun doctor` in a job says whether it signs as the project's CI.

## A sandbox project

`cargo xtask e2e --gitlab GROUP/PROJECT` drives a GitLab.com project through hushrun's lifecycle
(VALIDATION.md §9) and checks each plan's decision in its job log. It needs `git` and
`glab auth login`. Set the project up once:

1. **Create a project** on GitLab.com, such as `<you>/hushrun-sandbox`, public if you use keyless
   signing. Protect `main`.
2. **Commit an app that builds an image in seconds**:
   - `test.sh` with `#!/bin/sh` and `exit 0`, executable;
   - `Dockerfile` with `FROM alpine:3.24` and `COPY app.txt /app.txt`, and an `app.txt`;
   - `hushrun.yaml`:

     ```yaml
     version: 1
     repository: gitlab.com/<you>/hushrun-sandbox
     digest: { exclude: ["**/*.md"] }
     checks:
       unit: { run: ./test.sh }
     artifacts:
       image:
         type: oci
         location: registry.gitlab.com/<you>/hushrun-sandbox/app
     deploy: { branch: main, require: [unit] }
     ```
3. **Take the recipe**, with `ci/install-hushrun.sh`. Give the test job `image: alpine:3.24`,
   and make the deploy job build with the project's own registry:

   ```yaml
   deploy:
     image: docker:27
     services: [docker:27-dind]
     variables:
       DOCKER_TLS_CERTDIR: /certs
     script:
       # First: the build pushes the image with these credentials.
       - echo "$CI_REGISTRY_PASSWORD" | docker login -u "$CI_REGISTRY_USER" --password-stdin "$CI_REGISTRY"
       # … then the recipe's deploy script, unchanged.
   ```

   `./build.sh` runs `docker build -t "$1" .` and `docker push "$1"`, and `./deploy.sh` prints
   what it would deploy.
4. **Get the project's URL and the agent** as in step 3, and the variables of step 4.

Then run `cargo xtask e2e --gitlab <you>/hushrun-sandbox`, or `--dry-run` first to see the steps.

## Limits

- **Child pipelines** record runs under their own pipeline id. Run hushrun's jobs in the parent
  pipeline.
- **Merge request pipelines** have no branch for P-8, as pull requests on GitHub, and their
  certificates name their source branch: they are for checks, not deploys.
- **Windows runners**: install a release with its PowerShell installer; the install script is
  for Linux and macOS.
- **`hushrun adopt`** writes GitHub workflows only; start a GitLab project from the recipe.
- **A GitLab project can't be CI-only.** A CI-only repository counts only the CI's events, and
  the hosted server doesn't verify GitLab's ID tokens yet: the pipeline signs as an agent, whose
  events it then doesn't count. Don't turn on **CI reuses only CI's results** for a GitLab
  project in the dashboard.
