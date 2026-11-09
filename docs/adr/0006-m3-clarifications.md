# 0006. Planner: choices made in M3

- **Status:** accepted
- **Superseded in part by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-14
- **Rules:** P-5, P-7, P-13, P-15, §8.5, §10.4

## Context

M3 implements the planner (§8), the plan file and `plan`, `guard` and `status` (§10.4). The spec
fixes almost every behavior. A few places left a choice open. Following plan §0.1, each choice
below runs more checks or deploys less, or else it only makes output deterministic.

## Decision

1. **`checks_to_run` and `waived` follow the order of `T`'s `require`.** Scenarios, `HUSHRUN_CHECKS`
   and `actionable_scopes` are then stable across runs and match what the policy author wrote.
   §8.5 now says so. With several scopes, `actionable_scopes` lists them by name, or in the order of the `--scope`
   flags.
2. **An attempt without a `started` event is ordered by its oldest event** (P-5). hushrun always
   writes `started` first, but another tool, or a write that failed halfway, could leave an
   attempt without one. Ordering it anyway keeps it in P-7's walk, where it can still block or
   wait; dropping it could let a failed upstream go unseen.
3. **Until M6, nothing reports pending runs or liveness.** The generic and local providers have
   no API, so an upstream attempt without a terminal event is in flight until its lease ends,
   then `unknown`, which blocks (P-6's "no answer" row). Upstream grace still applies.
4. **`status` decides each stage as a manual first attempt on the stage's branch.** A manual
   trigger isn't deduplicated (P-10), so `status` shows what a deploy would need now, and lists
   the newest deployed digest separately.
5. **`guard` with a plan records `superseded` only for scopes the plan deploys** (`promote`,
   `refresh`, `full`). Scopes that wait, skip or are blocked started no deploy to supersede.
6. **`attest` writes the implicit `started` only when the run's attempt is newer than the
   plan's** (§10.4, case 2). Check evidence alone never creates an attempt (P-5).
7. **An upstream-validated artifact that can't be reused blocks a stage with deployment
   checks.** P-13's exception picks the artifact `U`'s `passed` event names. When that artifact
   is not a candidate (an unaccepted signer, an undeclared location, a failed attempt) the spec
   fell back to the newest candidate, which `U`'s deployment checks never saw. If `T` requires
   a deployment check, the artifact now counts as unavailable, so P-15 blocks. Without
   deployment checks nothing validated the running artifact, and the fallback stays. P-13 now
   says so.
8. **`mark deployed` and `mark passed` take only artifact events of this run, at this stage,
   from principals accepted for (`stage`, `artifact`).** A run id is just a field: another
   principal can write events that claim it, and the same run can deploy several stages.
   Without the filter, either could choose what a `passed` event names, which item 7 and P-13
   trust.

## Consequences

- Each choice is enforced by a scenario or a CLI test.
- Item 3 ends when providers fill the pending-run and liveness inputs (M6). The planner already
  takes both, so nothing in the planner changes.

## Alternatives considered

- **Sorting `checks_to_run` by name.** Deterministic too, but it reorders what the author wrote,
  and pipelines that print the list read worse.
- **Ignoring attempts without `started`.** Simpler, but it can hide a `failed` event, which
  would let a deploy through (P-7). Rejected.
- **Keeping P-13's fallback to the newest candidate** (item 7). It follows the original text,
  but it deploys an artifact that the required deployment checks didn't validate, which P-15
  exists to prevent.
