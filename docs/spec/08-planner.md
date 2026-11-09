# 8. Spec: planner

> Normative. Copied from the implementation plan (2026-10-13) in M0; this file is now the source of truth.
> MUST, SHOULD and MAY follow RFC 2119. Rules tagged `[Mn]` belong to milestone n.

The planner is a pure function in `hushrun-core`, with no IO, no clock and no environment access. Everything it needs is passed in, so any decision can be replayed from the JSON dump that `plan --explain-json` writes.

[ADR 0062] A repository has one pipeline. The planner decides what one run of it does with the digest `D` of a scope `X`. Nothing moves a change from one stage to the next, so there is no target stage, no upstream stage and no approval. P-2, P-3, P-7, P-11 and P-12 are removed with them; every other rule keeps its number.

### 8.1 Inputs

- the policy, and with it the required checks: `Policy::required()`, which is `deploy.require` when the file lists it, else every check with `run`, in name order (C-7). `run --all`, `ready`, `plan`, `status` and the server's index use this one definition;
- the scope `X` and the digest `D`;
- the run context: `branch?`, `trigger` (`push`, `upstream`, `manual` or `schedule`), `attempt`, `force_full`, the run identity and `now`. `upstream` is a run that another workflow's completion started, such as GitHub's `workflow_run`;
- the verified events for (`X`, `D`), each with its signer (`ci`, `user:<email>` or `agent:<name>`, T-15), effective custody (E-8) and trusted time when it has one (E-10);
- [ADR 0031 amendment 2, ADR 0062] the periods the repository was CI-only, as the signers answer says (T-16, L-13). An answer that still lists periods per stage, as servers did before ADR 0062, is read as their union;
- the verified entries of the deployed index for `X` (L-8);
- liveness answers for the pipeline's run attempts that have no terminal event (optional).

### 8.2 Evidence

**P-1 Checks.** For a required check `C`, the candidates are the `check` events that meet all of these:

- `name` is `C`, `checkKind` is `source`, and `definitionDigest` equals `def(C)` (E-6);
- the environment matches, when `C` pins one (E-7);
- [ADR 0031, ADR 0062] it counts (T-16): the repository isn't CI-only, or its signer is `ci`, or its trusted time falls outside every period the repository was CI-only, or it has none and the repository isn't CI-only now.

[ADR 0062] Evidence is about the files, whoever recorded it and wherever: a check event hushrun 0.8 or earlier recorded at a stage counts like one with no stage. A check event those releases recorded for a deployment check (`checkKind: deployment`) never counts, and an `approval` event is never a candidate.

[ADR 0062] P-2 (deployment checks, `C@U`) and P-3 (approvals) are removed. A check that ran against a deployment now runs in the deploy step, or is attested before `mark passed`; an approval only ever gated the move into a stage.

**P-4 Status.** The newest candidate decides, ordered as in E-9:

- `satisfied`: it passed and is within `max_age`, with age measured from the earlier of `finishedAt` and `time`;
- `expired`: it passed but is older than `max_age`;
- `failed`: it failed;
- `missing`: there is no candidate.

### 8.3 Run attempts

**P-5 Run attempts.** The pipeline's lifecycle events for `D` (`started`, `deployed`, `passed`, `failed` and `superseded`) that count (T-16) are grouped into run attempts by (`provider`, `id`, `attempt`). Every attempt has a `started` event, because commands write one first when it's missing (§10.4). Attempts are ordered by their oldest `started` event (E-9); an attempt without one, which only another tool could write, is ordered by its oldest event. P-13 uses their states (P-6). [ADR 0062] A lifecycle event hushrun 0.8 or earlier recorded at a stage belongs to that stage's pipeline, not this one, and is in no attempt.

**P-6 Attempt state.** The terminal events are `passed`, `failed` and `superseded`.

*An attempt with terminal events:* process them in E-9 order, starting with no state:

- `failed` sets the state to failed, whatever it was. A later job broke, or a real failure happened next to a superseded run.
- `passed` sets it to passed, unless the state is failed and this `passed` is less than 60 s after that `failed`. Within 60 s, signer clock skew could hide the true order, so the failure wins.
- `superseded` sets it to superseded only when there is no state yet, because a cancellation must not hide a verdict.

So `failed` after `passed` counts (a later job broke), and so does `passed` well after `failed` (a retried job succeeded). The stale guard doesn't fail its job by default (`--on-stale skip`, §10.4), so no cleanup `failed` follows a stale `superseded`.

*An attempt without terminal events* gets its state from liveness. GitHub's values map like this, and other providers map theirs the same way (§11.2):

| Liveness answer | State |
|---|---|
| Status `queued`, `in_progress`, `waiting`, `requested` or `pending` | In flight |
| Completed, conclusion `success` | No verdict: it passed in CI but recorded no outcome |
| Completed, conclusion `cancelled`, `skipped` or `stale` | Superseded |
| Completed, any other conclusion: `failure`, `timed_out`, `startup_failure`, `action_required`, `neutral`, or a value this table doesn't list | Failed |
| No answer | In flight until `leaseUntil` (capped at `time` plus `deploy.lease`, E-5), then `unknown` |

[M9] GitLab's pipeline statuses: `created`, `waiting_for_resource`, `preparing`, `waiting_for_callback`, `pending`, `running`, `scheduled` and `manual` are in flight; `success` is no verdict; `canceled`, `canceling` and `skipped` are superseded; `failed`, and any status not listed, is failed. GitLab counts no attempts, so a pipeline's status answers for every attempt recorded in it.

[ADR 0062] Only `passed`, and `superseded` with reason `stale`, make an attempt's artifacts reusable (P-13), and only terminal events give those states. No state that liveness or the lease gives changes a decision; they say what an attempt is doing to the people who read it.

[ADR 0062] P-7 (upstream state) is removed with upstream stages, and so is the mode it led to, `wait`. A run no longer waits for another run of the same digest: until that run's attempt passes, it has nothing to reuse.

### 8.4 Decision procedure

Rules apply in order; the first one that returns a mode decides.

**P-8 Wrong branch.** If `deploy.branch` is `B`, the run's branch is known, and it differs from `B`: `blocked` ("this repository deploys only from `B`; this run is on …, so run it from `B`"). `force_full` does not override this. If the branch is unknown (local runs), warn and continue; `guard` still checks before deploying. Without `deploy.branch`, a run on any branch continues.

**P-9 Force.** If `force_full` is set:

- `blocked` when `deploy.allow_force` is false;
- otherwise `full`, with `build = true` and `checks_to_run` set to every required check, whatever its evidence.

[ADR 0062] Forcing waives nothing: there are no deployment checks or approvals to waive. Events from a forced run carry `forced: true`. Later jobs learn that the run is forced from the plan (§10.4).

**P-10 Dedupe.** `current` (nothing to deploy) when all three hold:

- the trigger is in `deploy.dedupe` (default: `push` and `upstream`);
- the attempt is 1;
- the newest trusted `deployed` event of the deployed index for `X` (L-8) has digest `D`. Trusted means it verifies and its signer counts (T-16).

[ADR 0062] The pipeline's deployed index is `v1/deployed/<scope>/_/`. An entry hushrun 0.8 or earlier recorded under a stage was that stage's and doesn't count, so the first plan of a digest those releases deployed deploys it once more.

[ADR 0062] P-11 (upstream) and P-12 (requirements other stages record: deployment checks and approvals) are removed.

**P-13 Artifacts.** For each artifact declared for scope `X`, the candidates are the artifact events for `D` that meet all of these:

- they count (T-16);
- they were recorded by an attempt of the pipeline (P-5) whose state (P-6) is `passed`, or `superseded` with reason `stale`. A stale-superseded attempt passed its checks and built, then lost to a newer revision before deploying. Artifacts from failed, unknown, cancelled, no-verdict or still-running attempts are never reused, nor are those of an attempt hushrun 0.8 recorded at a stage;
- the ref is pinned by digest, and that digest equals `digest.sha256`;
- [ADR 0062] the ref's repository is the artifact's one `location` (C-6).

The newest candidate wins. A scope with no declared artifacts passes this rule.

**P-14 Promote or refresh.** If every artifact of `X` has a candidate: `promote` when every required check is satisfied. Otherwise `refresh`, with `checks_to_run` set to the required checks that are missing, expired or failed.

**P-15 Full.** Otherwise: `full`, with `build = true` and `checks_to_run` as in P-14. [ADR 0062] A rebuild is always allowed: hushrun 0.8 blocked here when the stage required a deployment check, since a rebuilt artifact was never seen by it.

[ADR 0031] P-16 (protected paths) and P-17 (audit sampling) are removed: they kept laptop evidence in check against a trust policy that named who may sign. With membership as trust (T-15), a repository that wants only CI's evidence is made CI-only on the server (T-16).

### 8.5 Output

For each scope, the decision has:

- `mode`, and a `reason`: one sentence a person can act on, naming runs and URLs;
- `digest` and `build`;
- `artifacts`: name → pinned ref, with the event key that justified it;
- `checks_to_run`, each with why it runs (`missing`, `expired`, `failed` or `forced`), in the order of `Policy::required()`;
- the requirement table: every required check, its status, and the event that decided it;
- the newest deployed digest.

[ADR 0062] `waived`, `deployment_checks` and the upstream state are gone. Schema: `hushrun.plan/v2` (§10.2).

### 8.6 Invariants

These are property-tested (VALIDATION.md §3).

- **I-1** `promote` and `refresh` deploy only artifacts from events for `D` that count, pinned by digest at the artifact's location, and recorded by an attempt of the pipeline that passed or was superseded as stale. `promote` also requires every required check to be satisfied.
- [ADR 0062] I-2 (no deploy after an upstream's failure) is removed: there is no upstream.
- **I-3** The order in which events are listed never changes the decision.
- **I-4** Adding events that fail verification, or events for another digest, scope or repository, never changes the decision. The one exception is the deployed index for `X`: a newer deployment of another digest can turn `current` into a deploy (P-10).
- **I-5** `current` happens only on an automatic first attempt.
- **I-6** A forced run never returns `promote`, `refresh` or `current`.
- **I-7** With every other input fixed, including the set of accepted events, advancing `now` never turns another mode into `promote`.
- [ADR 0062] I-8 (no approval is waived) is removed: there are no approvals.
- **I-9** `checks_to_run` lists only checks that `Policy::required()` names.

[ADR 0062] Two more properties pin how the planner reads what hushrun 0.8 recorded: taking away every lifecycle event and deployed index entry recorded at a stage, and every approval, never changes a decision (P-5, P-10); and the stage a check or artifact event was recorded at, or none, never does (P-1, P-13).

### 8.7 Mapping from ci-promote

ci-promote is the shell pipeline hushrun replaced first; its production plan decided from staging's runs. [ADR 0062] hushrun has no stages, so the mapping is now from ci-promote's plan to the plan of one pipeline, which decides from its own earlier attempts of the same digest.

| ci-promote | hushrun |
|---|---|
| `promote` | `promote`, or `refresh` when checks have `max_age` |
| `skip`: staging is queued or still running the tree | `full`: an attempt that hasn't passed gives nothing to reuse (hushrun 0.8 waited, with `wait`) |
| `skip`: the commit is already deployed | `current`, matched by digest instead of commit |
| `full` | `full` |
| `blocked`: staging failed the tree | `full`: the checks that failed run again, and the build. `blocked` is only the wrong branch (P-8) and a refused force (P-9) |
| GitHub API error → `blocked` | Ledger error → exit 6 (L-5) |
| Cancelled or stale staging run → look further back | Every attempt that passed or went stale counts, and the newest artifact among them wins (P-13) |

Intended differences. Shadow mode (§17) counted these as explained:

| Situation | ci-promote | hushrun | Scenario (VALIDATION.md §5.3) |
|---|---|---|---|
| A new commit with the same files as the deployed one | Deploys again | `current`: same digest | N24 |
| An attempt superseded by the stale guard after it built the image | `full` | `promote` or `refresh` with that image | N3 |
| A passed attempt whose attestation artifact expired (after 30 days) | `full` | `promote`: the artifact event is still in the ledger | N60 |
| An attempt a manual run made | Ignored (only push runs count) | Counts | N43 |
| A check with `max_age` | Not applicable | `refresh` | N1 |

[ADR 0062] N15 (a staging attempt with no recorded outcome and no liveness blocked production) and N52 (a staging failure older than ci-promote's window still blocked) went with upstream stages: such an attempt now gives nothing to reuse, and the plan is `full` (`rules/p06-lease-ran-out.yaml`, `pipeline/an-earlier-failure-runs-again.yaml`).
