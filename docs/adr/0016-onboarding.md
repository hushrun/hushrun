# 0016. Onboarding: the reusable workflow, `adopt`, `doctor --org` and the Terraform modules

- **Status:** accepted; item 8, the Terraform modules, superseded by
  [0022](0022-https-ledgers-only-and-no-kms.md); item 2's install by exact version superseded by
  [0035](0035-updates-and-the-newest-release-by-default.md): the workflow installs the newest
  release unless its caller pins one
- **Superseded in part by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-15
- **Rules:** L-6, T-4, T-7, T-8, O-1, O-5, O-7, D-7

## Context

RFC 0001 §4.4 (accepted) wants adoption to take one reviewed pull request per repository, with
no secrets and no admin rights on it, after a one-time organization setup (M14): Terraform
modules for the ledger, a reusable workflow that holds the recipes' structure, `hushrun adopt`
to write a repository's files, and `hushrun doctor --org` to find drift. Its open question 10
asks whether the reusable workflow should take build and deploy commands as inputs, or
composite actions should leave teams their own workflows.

## Decision

1. **The reusable workflow takes commands (open question 10).** `hushrun-deploy.yml`
   (`on: workflow_call`) holds the recipes' jobs: plan; test, which runs exactly the plan's
   checks; deploy (guard, resolve, build and attest when the plan builds, deploy, mark
   deployed, the deployment checks other stages require, mark passed); and the report jobs. The
   team gives `build` and `deploy`, which see the image as `$REF`. That puts the structure, the
   part that must be right for hushrun to be safe, in one versioned place: a fix ships as a new
   commit, and `doctor --org` finds who hasn't taken it. Teams that need their own workflow copy
   its jobs.
2. **Pinning the workflow pins everything it runs.** A reusable workflow can reference its own
   repository's composite actions only by a ref written in the file, which can't be its own
   commit, so a caller pinned to commit X would still run whatever `@main` holds. So the workflow
   installs hushrun itself, by exact version, checked against the release's SHA-256, in a step
   shared by its jobs through a YAML anchor (which GitHub Actions accepts since September 2025,
   and actionlint follows), and pins every action by commit. No new composite action helps
   under that constraint; `action/setup` stays for hand-written workflows.
3. **No secrets.** Cloud credentials come through workload identity federation (the `gcp-*`,
   `aws-*` and `azure-*` inputs). Until M10, the signing key is `HUSHRUN_SIGNING_KEY`, from the
   `signing-environment` or passed by the caller. The ledger comes from settings: callers pass
   `vars.HUSHRUN_LEDGER_BASE` and `vars.HUSHRUN_LEDGER`, which resolve in the caller's context,
   and the workflow falls back to its own `vars` (T-4).
4. **`adopt` writes what it can know, and fails loudly on what it can't.** It detects the
   language (the `unit` check's command and the workflow's toolchain), the Dockerfiles (one image
   at the root, or one scope and image per directory, each scope covering the deploy workflows,
   D-7) and the workflows that already deploy (their branches decide the stages' when nothing
   else does; their deploy commands are quoted as comments). The `build` command builds the
   Dockerfile; `deploy` is a placeholder that fails, since a deploy command copied from another
   workflow refers to that workflow's context: a starter that deploys nothing fails the first
   run instead of passing it.
5. **`adopt --org` meets the organization from the start.** The repository's entry gives the
   stages and branches, the registries give the image locations, the base gives the ledger, and
   the compiled trust's constraints shape the stages: required checks are added, an org-owned
   check without `run`, and `upstream`, `allow_force`, `on_ledger_error`, `after` and `branch`
   as required. `adopt` then checks its own `hushrun.yaml` against that trust (T-2, T-6, T-7),
   and refuses to write one that wouldn't plan.
6. **`adopt` is idempotent.** A file adopt would write identically is `unchanged`; one that
   differs is `kept`, since the team may have edited it, unless `--force`; `--dry-run` prints
   instead of writing. Its output is deterministic, so a golden copy can be linted in CI.
7. **`doctor --org` reports drift; it changes nothing.** Per repository of `org.yaml`, from the
   ledger: trust missing, unsigned or unverifiable, behind what `org.yaml` compiles to, or behind
   the compiled serial waiting in the compiled directory (O-7). With `GITHUB_TOKEN` (contents:
   read on the repositories), on each stage's branch: a `hushrun.yaml` that names another
   repository or ledger, disagrees with the compiled trust, or breaks a constraint, with its
   line; workflows on another commit of the reusable workflow, another hushrun version, or none
   that runs hushrun; and the repository's spelling on GitHub. Drift that breaks or weakens a
   repository's checks is an error (exit 2), so a scheduled job can alert on it; an old
   workflow version, a branch not yet adopted, or trust expiring soon is a warning.
8. **Terraform binds each cloud as tightly as it allows (L-6).** GCS: one workload identity
   pool admitting only the organization's repositories, bucket access per repository (by
   numeric id when given) conditioned to its `v1/` for creates, and signing keys bound to the
   stage's branch through a `repository@ref` attribute. S3 and Azure: a role or identity per
   repository and stage, assumable only from that stage's branch or its `hushrun-<stage>`
   environment. The org root's `root/1.json` goes into every repository's prefix once, under
   the store's lock: GCS object retention in locked mode and S3 Object Lock in compliance mode
   until a date, Azure's locked version-level immutability for a period. Azure stays weaker, as
   [ledger-access.md](../guides/ledger-access.md) says: RBAC can't say create-only, so CI's role
   has no delete and every version is kept. KMS keys (M10) and index read access are optional.
   Provider ranges start where the arguments the modules use appeared, and the examples validate
   at both ends.

## Consequences

- A repository adopts in one pull request: `adopt --org` writes three files, an admin adds the
  entry it prints to `org.yaml`, and `org plan`, `sign` and `apply` give it trust.
- The reusable workflow is now a contract: its inputs and outputs are versioned with the
  repository, and a breaking change needs a new major workflow, since callers pin commits.
- Anchors keep the workflow short, at the price of tools that don't expand them: actionlint does;
  some security scanners may not.
- `doctor --org` needs a token that reads every repository's contents; GitHub Actions'
  `GITHUB_TOKEN` reads only its own repository, so a scheduled job needs an App or a
  fine-grained token.

## Alternatives considered

- **Composite actions per step**, for teams to compose. They can't be pinned to the reusable
  workflow's commit, and composing them right is what the recipes' structure was for.
- **`adopt` copying the existing deploy command.** It refers to the old workflow's variables
  and image tags, and would deploy the wrong thing, or nothing, while looking configured.
- **`doctor --org` failing on every warning.** A repository not adopted yet on one branch isn't
  broken; failing on it would train people to ignore the job.
- **One identity per repository on S3 and Azure**, like GCS's bucket access. Branch binding is
  what an environment secret with a branch policy gave; per-stage roles keep it without
  secrets.
