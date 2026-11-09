# 0019. GitLab CI: choices made in M9

- **Status:** accepted; superseded in part by [0035](0035-updates-and-the-newest-release-by-default.md):
  the install script and the recipe install the newest release unless a pipeline pins one, and
  the newest release's image is also `latest` (items 16 to 18)
- **Superseded in part by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-16
- **Rules:** C-7, E-4, E-11, P-6, P-7, T-4, T-10, T-12, §10.2, §10.4, §11.2, §14, §15.4

## Context

M9 makes GitLab CI the second native provider: run identity, triggers, dotenv outputs, liveness
and pending pipelines (§11.2), the recipe (§15.4), the install script and the container image
(§14), and a sandbox project. RFC 0001 §5.1 adds GitLab's ID tokens for keyless signing, and
identities in `trust.yaml` that GitLab's certificates can match (T-10).

GitLab differs from GitHub Actions in ways the plan leaves open:

- **No attempts.** Retrying a job keeps its pipeline id, and GitLab counts no attempts.
- **No runner files.** Jobs pass values through dotenv reports and artifacts, and nothing
  collects a step summary or annotations.
- **One pipeline definition.** A project has one `.gitlab-ci.yml`. Fulcio names it in a Build
  Config URI with `//` between the project and the file:
  `https://gitlab.com/acme/back//.gitlab-ci.yml@refs/heads/main`.
- **ID tokens are declared, not requested.** `id_tokens` puts one JWT per declared audience in a
  variable when the job starts, and it lasts the job's timeout.
- **Pipeline variables outrank project variables.** Whoever may run a pipeline with variables
  can override `HUSHRUN_LEDGER`, unless the project restricts them.
- **The pipeline statuses** are those §11.2 lists, plus `waiting_for_callback`, which GitLab
  added after the plan was written.

Following plan §0.1, each choice below runs more checks or deploys less.

## Decision

### Runs

1. **Run identity comes from GitLab** (E-4 [M9]).
   - The run is `CI_PIPELINE_ID`, its URL `CI_PIPELINE_URL`, its job `CI_JOB_NAME`, and
     `jobId` is `CI_JOB_ID`.
   - The branch is `CI_COMMIT_BRANCH`. Tag and merge request pipelines have none, so P-8 treats
     their branch as unknown, as it does on GitHub; `guard` still checks before deploying.
   - The revision is `CI_COMMIT_SHA`. The repository is `CI_PROJECT_URL` without its scheme,
     with a lowercase host (C-2): the id `init` derives from the remote.
   - `HUSHRUN_RUN_ID` doesn't override the pipeline id, as on GitHub (ADR 0009).
2. **Attempts come from the plan.** There is no native attempt, so the order is
   `HUSHRUN_RUN_ATTEMPT`, then the plan's attempt, then (for `plan`) one more than the highest
   attempt this pipeline recorded at the stage (§10.4).
   - A retried plan job starts a new attempt.
   - A retried later job continues its plan's attempt. A `passed` it records more than 60 s
     after the `failed` that `mark-failed` recorded wins (P-6): a retried job succeeded.
   - No command writes an implicit `started` for a newer native attempt, because GitLab has
     none.
3. **Triggers** follow `CI_PIPELINE_SOURCE` (§11.2):
   - `push` is push;
   - `pipeline`, `parent_pipeline` and `trigger` are upstream;
   - `web` and `api` are manual;
   - `schedule` is schedule.

   Other sources, such as `merge_request_event`, say nothing, and `plan` treats them as manual.

### Outputs

4. **No runner files on GitLab.** Jobs pass values as dotenv reports (`--dotenv FILE` or
   `--format dotenv`), and the plan as a file artifact (`plan --out`). The job log shows each
   command's text output. GitLab has no step summaries or annotations, so hushrun writes none
   there.
5. **dotenv follows GitLab's parser**:
   - one `KEY=value` per line, never a blank line, a comment or quotes;
   - keys of ASCII letters, digits and `_`, which every key hushrun writes has;
   - single-line values;
   - at most 20 variables, fewer than 5 KiB, and never `json`.

   A key outside that alphabet, or a file of 5 KiB or more (GitLab wants it strictly smaller),
   is refused. Values are written as they are: GitLab strips leading and trailing spaces.
   Dotenv variables outrank job and default variables, so `HUSHRUN_MODE` and the rest reach
   later jobs, but never project or pipeline variables.

### The API

6. **The API is optional and needs `HUSHRUN_GITLAB_TOKEN`.**
   - The token is a project access token with `read_api`; the Reporter role is enough.
     `CI_JOB_TOKEN` can't read pipelines.
   - The API is `CI_API_V4_URL` (GitLab's own variable, so self-managed instances work), and the
     project is `CI_PROJECT_ID`.
   - The token goes in `Authorization: Bearer`, which reqwest never forwards to another origin,
     and the client follows no redirects. Nothing logs it.
   - Without the token, or when the API fails, `plan` warns and relies on leases and upstream
     grace (§10.4), as on GitHub.
7. **Liveness** (P-6 [M9]) is `GET /projects/:id/pipelines/:pid`. Only attempts that GitLab
   recorded are asked about. The pipeline's status answers for every attempt of it, since GitLab
   has no attempts. The statuses map as §11.2 says, with one addition:
   - `created`, `waiting_for_resource`, `preparing`, `waiting_for_callback`, `pending`,
     `running`, `scheduled` and `manual` are in flight;
   - `success` is no verdict;
   - `canceled`, `canceling` and `skipped` are superseded;
   - `failed`, and any status not listed, is failed.

   `waiting_for_callback` is new since the plan. A pipeline waiting for an external callback
   hasn't ended, so it is listed as in flight rather than left to the catch-all. `manual`
   means a manual job holds the pipeline; it stays in flight until someone runs it or cancels
   the pipeline.
8. **Pending pipelines** (P-7) are listed when `HUSHRUN_GITLAB_TOKEN` is set and the upstream
   stage `U` has a ref: its `ci.gitlab.ref` (C-7 [M9]), else its `branch`.
   - The request is `GET /projects/:id/pipelines?ref=<ref>&order_by=id&sort=desc&per_page=50`,
     keeping the in-flight statuses.
   - A pipeline counts as pending until it has recorded anything at `U`, in any attempt: there
     is no attempt number to compare. A pipeline retried after its verdict therefore isn't
     pending again until its retried plan job records `started`. A plan in that window sees the
     old verdict, and staging's completion plans production again anyway.
   - A pipeline at this checkout's commit has its digests. Any other is matched as on GitHub:
     `git fetch --depth=1 origin <sha>`, then the digest. GitLab's API gives no tree.
9. **A run is never its own upstream**, on GitHub too. The current run is left out of the
   pending runs. When `U` and `T` run in one pipeline (or one GitHub workflow), the run would
   otherwise wait for itself.
10. **`ci.gitlab.ref`** (C-7 [M9]) is a branch or tag name, not a `refs/` path. It is the ref
    whose pipelines are `U`'s runs, for an upstream stage with no `branch` or whose pipelines run
    on another ref.

### Keyless signing and identities

11. **ID tokens come from `id_tokens`** (T-12 [M9]).
    - A job declares `SIGSTORE_ID_TOKEN: {aud: sigstore}`. That is the variable hushrun already
      reads (M10), and the one cosign and GitLab's own Sigstore guide use.
    - The plan's default variable `HUSHRUN_ID_TOKEN` is not adopted. One variable per audience
      keeps a token meant for one service from reaching another.
    - On GitLab, before sending the token to Fulcio, hushrun checks that its `aud` includes
      `sigstore` and that it hasn't expired. Each error names the `id_tokens` entry to add.
    - Fulcio's proof of possession signs `sub`, since GitLab's tokens have no `email`.
    - `doctor` reads the token's claims and shows the identity Fulcio would certify, mapped as
      Fulcio's `gitlab-pipeline` template does:
      - issuer `iss`;
      - subject, Build Config URI and Build Signer URI `https://<ci_config_ref_uri>`;
      - Source Repository URI `<iss>/<project_path>`;
      - Source Repository Ref `refs/heads/<ref>` or `refs/tags/<ref>`;
      - Source Repository Identifier `project_id`.
12. **GitLab identities in trust** (T-10 [M9]) name:
    - `issuer: https://gitlab.com`, or the instance's URL with a private Fulcio;
    - the project's numeric id as `repository_id`;
    - `ref: refs/heads/<branch>`;
    - `workflow: .gitlab-ci.yml`.

    The two forms differ only in the separator. A path-form `workflow` matches
    `<repository>/<path>@<ref>` on GitHub, and `<repository>//<path>@<ref>` too, GitLab's form.

    A path-form `workflow` that starts with `/` is now an error, because no repository path
    does. Before, `/x` matched GitLab's form by accident and nothing else.
13. **What binds a GitLab stage is the protected branch.**
    - Merge request pipelines sign for their source project and source branch: GitLab documents
      `project_id` and `ref` that way. A fork's merge request never carries this project's id,
      and a branch of this project names only itself.
    - Every pipeline of a project runs the one `.gitlab-ci.yml`, so `workflow` adds little.
    - Who may push or merge to `develop` decides who can make a pipeline sign as staging, and
      the guide makes the stage branches protected.

### Configuration

14. **Pipeline variables** (T-4 [M9]) outrank project variables. A person allowed to set them
    could point `HUSHRUN_LEDGER` at another ledger. The guide restricts them (*Minimum role to
    use pipeline variables*: no one, or Maintainer with protected stage branches). `doctor`
    reports the setting when the token can read it. The recipe's trigger job forwards no
    variables, so it runs under the strictest setting, and staging's `HUSHRUN_STAGE` never
    reaches production.
15. **Keys on GitLab** live in File-type protected variables, as the plan's recipe has them.
    Every protected branch's pipelines can read protected variables, so a `develop` pipeline
    can read production's key. That is the weakness GitHub has without environments. Keyless
    signing (public projects on gitlab.com) and KMS keys behind workload identity bound to the
    ref (private projects) bind each stage to its branch. The guide leads with them. The
    recipe keeps keys for the first run, with signing set in one place.

### Distribution

16. **The recipe** (`recipes/gitlab/.gitlab-ci.yml`) is §15.4, finalized:
    - Jobs that only run hushrun use the container image. Jobs on a toolchain image install the
      binary with the vendored script.
    - Every job that runs hushrun has an `after_script` that records `superseded` (reason
      `cancelled`) when GitLab cancels it mid-script. GitLab runs `after_script` on cancel with
      `CI_JOB_STATUS=canceled`, so this is the counterpart of GitHub's report-cancelled job.
    - Timeouts are failures, which `mark-failed` records.
    - The trigger job sets `inherit: variables: false`.
17. **`ci/install-hushrun.sh`** (`recipes/gitlab/install-hushrun.sh`, vendored) is POSIX `sh`:
    - It installs the exact `HUSHRUN_VERSION` for `uname`'s Linux or macOS target, with curl or
      wget, from `HUSHRUN_DOWNLOAD_URL` (default: the GitHub release).
    - It checks the archive against `HUSHRUN_SHA256` when set, else against the checksum pinned
      in the script for that version and target. It refuses an unpinned version without
      `HUSHRUN_SHA256`.
    - It installs into `HUSHRUN_INSTALL_DIR`: by default `$CI_PROJECT_DIR/.hushrun/bin` in CI,
      else `$HOME/.local/bin`. It keeps a binary already there when its checksum matches, so
      `after_script` can call it again cheaply. Then it prints the path.
    - `cargo xtask installer-pins` writes the pins from a release's `.sha256` files. The
      `publish` workflow attaches the pinned script to each release. Adopters vendor that copy
      after reading it.
18. **The container image** is `ghcr.io/hushrun/hushrun:<version>`, built from
    `deploy/image/Dockerfile`:
    - Alpine, pinned by digest, with `git`, CA certificates and the release's static musl
      binaries. Those are the archives' bytes, checked against their checksums before the build.
    - `linux/amd64` and `linux/arm64`.
    - No entrypoint, because GitLab runs job scripts with the image's shell.
    - `safe.directory=*`, because the runner may check out the repository as another user.
    - Tagged with the exact version only, never `latest` or `0`.
    - `.github/workflows/publish.yml` builds and pushes it after the Release workflow, with
      BuildKit provenance and an SBOM.
    - While the repository is private, its image is private too. Adopters mirror it to a
      registry their runners can read, for example with `hushrun artifact copy`.

### Tests and diagnosis

19. **Test harness.**
    - `hushrun_testkit::fake_gitlab` serves pipelines by project, the list endpoint with its
      filters, and token checks, records each request, and can answer with any status.
    - CLI tests drive `plan`, `run`, `mark` and `guard` as GitLab jobs against it.
    - Keyless signing with GitLab's claims runs through the testkit's Sigstore. Its fake Fulcio
      now maps GitLab's claims as Fulcio does.
    - `cargo xtask e2e --gitlab GROUP/PROJECT` drives a GitLab.com sandbox project with `glab`,
      set up as `docs/guides/gitlab.md` describes.
20. **`doctor` on GitLab**:
    - checks that `.gitlab-ci.yml` (and `.gitlab/**`) is inside every scope (D-7);
    - reads the project through the API, so a broken token shows before a plan needs it;
    - checks that each stage's branch is protected;
    - reports who may set pipeline variables.

    The last three need `HUSHRUN_GITLAB_TOKEN`. Each is skipped, with a note, when the token's
    role can't read that setting.

## Consequences

- Everything except the e2e driver is tested locally: against a fake GitLab API, a real git
  remote, and the testkit's Sigstore. The driver and a timed adoption need the sandbox project.
- A GitLab plan sees a pipeline's own verdict until a retry writes again, while GitHub tells
  attempts apart. That can cost a re-plan, never a wrong deploy.
- `waiting_for_callback` is in flight. A status GitLab adds later is failed until hushrun lists
  it, which can block a plan that should have waited.
- Hosted ledgers keep their GitHub-only ID tokens (L-10). A GitLab job authenticates to one with
  `HUSHRUN_LEDGER_TOKEN` until the server accepts GitLab's issuer and the CLI sends a token
  for the ledger's audience.
- `hushrun adopt` still writes GitHub workflows only.

## Alternatives considered

- **`HUSHRUN_ID_TOKEN` as the plan named it.** It is a second name for what
  `SIGSTORE_ID_TOKEN` already carries. With one variable for every audience, Fulcio and a
  ledger would receive each other's tokens.
- **Trusting `CI_JOB_TOKEN` for the API.** It can't list pipelines, and fine-grained job token
  permissions aren't available on every instance.
- **Counting a pipeline's jobs as attempts** (a retried job as attempt 2). Jobs aren't
  attempts: a pipeline's jobs share one plan, and one retried job would split the pipeline's
  events into several attempts.
- **Filtering pending pipelines by source**, for example push only. Web, API, schedule and
  trigger pipelines run staging too. A pipeline that never records anything only delays a plan
  while it runs.
- **Reading the Build Trigger extension** and refusing merge request pipelines. GitLab already
  names their source project and branch, which `repository_id` and `ref` check.
- **A `:latest` or `:0` image tag.** Recipes pin an exact version (§14); a moving tag changes
  what CI runs without a commit.
