# 0062. No stages

- **Status:** accepted
- **Date:** 2026-11-08
- **Rules:** C-1, C-6, C-7 (now `deploy`), C-8, E-6, E-8, P-2, P-3, P-7, P-11, P-12 (removed), P-8,
  P-9, P-13, P-15, T-16, §7, §8, §10.4
- **Supersedes in part:** [0006](0006-m3-clarifications.md) (upstreams, `wait`),
  [0008](0008-m5-oci-promotion.md) (copying between stages), [0009](0009-m6-github-actions.md) and
  [0019](0019-gitlab.md) (one caller per stage, liveness of upstream runs),
  [0016](0016-onboarding.md) (`adopt --stage`),
  [0018](0018-local-and-agent-evidence.md) (approvals), [0031](0031-membership-is-trust.md)
  amendments 1-3 (CI-only stages, approvals), [0056](0056-readers-derive-promotion-state-as-the-planner-does.md)
  (state per stage)
- **Amends:** [0005](0005-m2-clarifications.md), [0024](0024-init-sets-up-hushrun-run.md),
  [0030](0030-final-setting-names-without-aliases.md), [0032](0032-quota-refusals-fail-open.md),
  [0036](0036-agents-told-before-they-push.md), [0037](0037-any-language-from-cis-own-files.md),
  [0044](0044-one-key-for-runs-that-differ.md), [0045](0045-a-check-says-where-it-counts.md),
  [0054](0054-checks-on-depot-ci-and-behind-mise-tasks.md), [0060](0060-each-state-of-a-run-decided-once.md)

## Context

hushrun began as a promotion pipeline. `hushrun.yaml` declared *stages* (staging, production),
each with a branch, the checks it requires and the stage it comes after. The planner decided, per
stage, whether a change could be *promoted*: production reused staging's evidence (`e2e@staging`),
waited while staging was still running, was blocked until a person approved, and copied staging's
image to production's registry by digest.

The product people adopt is something else. `hushrun run NAME -- CMD` skips a check when these exact
files already passed it, on a laptop, in an agent's sandbox or in CI, and nothing there has a
stage. The repositories using hushrun have no `stages:` and no `--stage` in CI. hushrun's pitch is one
idea, don't test again what you or your agent already validated, and stages made it read as a
deployment tool.

On 2026-11-08 the maintainers decided to remove the stage part, moving a change from one stage to the next,
and only that: the event ledger, its transparency log, Sigstore signing, isolated container runs,
the planner, `mark`, `attest`, `guard`, `status`, artifacts and the dashboard's history stay,
without `--stage`. Approvals go with stages: an approval only ever gated the move into a stage.

## Decision

1. **A repository has one implicit pipeline.** No event carries a stage. The events key's stage
   segment is `_`, which the layout has always allowed; the deployed index becomes
   `v1/deployed/<scope>/_/…`. E-8 no longer requires a stage on lifecycle and approval events.
2. **Configuration.** A former stage's own settings move to one optional block:

   ```yaml
   deploy:
     branch: main          # P-8: the branch a deploy runs from
     require: [unit, e2e]  # default: every check with `run`
     lease: 30m
     dedupe: [push, upstream]
     allow_force: true
     on_repository_error: fail
   artifacts:
     image: { type: oci, location: ghcr.io/acme/api }   # one location, was `locations: {stage: repo}`
   ```

   `Policy::required()` is the one definition of the checks a commit needs: `deploy.require`, else
   every check with `run`. `run --all`, `ready`, `plan`, `status` and the server's index call it.
   Gone with no replacement: `stages`, `after`, `upstream`, `upstream_grace`, `ci.github.workflow`,
   `ci.gitlab.ref`, `check@stage` requirements, `kind: deployment` and `kind: approval`. A check
   that ran against an earlier stage's deployment now runs in the deploy step, or is attested
   before `mark passed`.
3. **Old files keep working where that's safe.** C-1 still refuses unknown keys. `stages`,
   `checks.<name>.kind` and `artifacts.<name>.locations` are *retired*: parsed, ignored, and named
   once per command with their line, "ignored since hushrun 0.9.0", and what to delete or move to
   `deploy:`. `run`, `ready` and the pre-push hook keep skipping, and a file with retired keys no
   longer loses its `backend:` or `repository_url`. `plan` refuses such a file (exit 2): a deploy
   that ignored `stages:` would skip the gates its author wrote. `hushrun init` removes each
   check's `kind` (a line of its own or a flow map's entry), and names `stages` and `locations`
   without touching them: only a person can say which of a stage's gates `deploy:` keeps. E-6 keeps `"kind": "source"` in every definition object, so evidence recorded
   before 0.9 keeps its definition digest.
4. **Commands.** `run CHECK…` (no command after `--`) runs hushrun.yaml's checks, `--isolated`
   included, and records a ledger `check` event; `run NAME -- CMD` is unchanged. `plan` keeps
   `full`, `refresh`, `promote`, `current` and `blocked`; `wait` goes with upstreams. `promote` keeps
   its wire value and now means every required check is satisfied and every artifact is reusable
   from an earlier passed attempt of this digest. `approve` is removed. `guard`, `status`,
   `mark`, `attest`, `artifact`, `repository`, `policy explain`, `verify`, `ready`, `init
   --pipeline` and `adopt` lose `--stage` and per-stage output; `init --pipeline` and `adopt` take
   `--branch` and `--image`. Outputs whose schema required a `stage` move to `/v2`. A hidden global
   `--stage` and a hidden `approve` exit 2 with "hushrun 0.9 has no stages", until 0.10.
5. **CI-only is one switch per repository.** The per-stage CI-only lists (an org's and a
   repository's) merge into the existing per-repository "CI reuses only CI's results". Its history
   is stage-less periods, which the signers answer carries; readers also accept the old per-stage
   shape as the union of its periods. T-16: while a repository is CI-only, checks, artifacts and
   lifecycle events count only from CI, except events whose trusted time falls outside every
   period. The server's migration fills the periods with the union of the old ones, so nothing
   becomes less strict. C-8's refusal of `counts_from` and scope `external` moves from "a file with
   stages" to "a file with `deploy:`": ledger evidence still can't honour those rules.
6. **The server accepts old writers.** CLIs up to 0.8 keep writing stage-bearing events to the
   ledger. They are stored as they are (the ledger is append-only and signed), the index ignores
   the stage, and approval events are inert history. The index tables are a cache and are rebuilt
   without stage columns.
7. **Stage-free results keep their signature.** Every result a machine or agent token writes keeps
   its key id, signature and the hash of the signed body, so a check's history shows who signed a
   pass after the machine is gone. CI results are authenticated by the job's ID token and carry
   none.

The rules P-2, P-3, P-7, P-11 and P-12 are removed from the spec, marked as P-16 and P-17 were.
C-7 keeps its number and defines `deploy`, which holds what a stage's settings held. P-9 loses its approval and deployment-check waivers; P-13 matches the one `location`; P-15
becomes "else full".

## Rejected alternatives

- **Delete the pipeline with the stages.** Every ledger write came from a stage command, so the
  ledger, the log and the planner could have gone too. The maintainers kept them: one pipeline still
  skips checks that already have evidence, reuses a build of the same digest and doesn't redeploy
  what is running.
- **Keep one fixed internal stage.** It would have changed almost no code, but every reader would
  still carry a stage that means nothing, and the stage columns would stay.
- **Rename `promote`.** The mode's value is matched by the server's database, the stats, the
  dashboard and every workflow's `["promote","refresh","full"]` filter. The behaviour is the same
  minus upstreams.
- **Keep an org-wide CI-only switch.** With no stages it would be a second switch meaning "CI
  trusts only CI" beside the repository's, and the two would disagree.
- **Refuse old files everywhere.** CI installs the latest release, so a refusal would stop every
  check in a repository whose file still says `stages:`, for a key `run` never read.

## Consequences

- hushrun 0.9.0 is a breaking release (`feat(cli)!`). The server ships first: an 0.8 server
  refuses `_` deployed keys and stage-less lifecycle events.
- The first 0.9 `plan` doesn't see deployments 0.8 recorded under a stage, and redeploys once.
- A pipeline that ran 0.8 and 0.9 in different jobs of one run breaks; the plan file is
  `hushrun.plan/v2`, so each run stays on one release.
- Docs move from `/docs/promotion/*` to `/docs/deploy/*`, with redirects.
- The reusable workflow `hushrun-deploy.yml` loses its `stage` input; old pinned copies install the
  newest release and meet the hidden `--stage`.
