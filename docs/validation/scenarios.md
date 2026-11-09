# Planner scenarios

A scenario is one planner decision written down: a policy, the events recorded, the run being
planned, and the decision expected. The runner
(`crates/hushrun-testkit/tests/scenarios.rs`) builds the planner's input from each file under
`testdata/scenarios/` and calls `hushrun_core::plan::plan` directly. Scenarios test decisions,
not signatures: every event counts as verified, signed by the signer it names.

```sh
cargo test -p hushrun-testkit --test scenarios
HUSHRUN_SCENARIOS_VERBOSE=1 cargo test -p hushrun-testkit --test scenarios -- --nocapture
```

The second form prints every decision. A failure lists each way the decision differs from
`expect`, for every failing file, in one run. The same runner also checks that
`hushrun_core::plan::check_status`, which `hushrun run` skips by and the server's index shows,
gives every required check the status and deciding event the planner gives it.

[ADR 0062] A repository has one pipeline: scenarios plan a scope of it, with no stage, no
upstream and no approval. Events can still carry the stage hushrun 0.8 recorded them at, to
model a repository's history.

## Directories

| Directory | Holds |
|---|---|
| `pipeline/` | One pipeline's everyday decisions: a first push, a re-run, a revert, a failure, a cancelled or still-running attempt, an image that isn't reusable, the wrong branch. Most began as cases of ci-promote's `plan_test.sh`, and their `description` names the case |
| `differences/` | The intended differences from ci-promote that remain (spec §8.7): N1, N3, N3b, N24, N43, N60 |
| `rules/` | At least one scenario per planner rule, named `p<NN>-<what>.yaml`, and `t16-<what>.yaml` for a CI-only repository (T-16) |
| `history/` | How a 0.9 plan reads what hushrun 0.8 recorded at a stage: check events count, attempts and deployments don't, approvals are inert |

Every rule P-1, P-4 … P-6, P-8 … P-10 and P-13 … P-15 is named by at least one scenario's
`covers:`, and so is T-16; `cargo xtask trace` counts scenarios like any other test. P-2, P-3,
P-7, P-11 and P-12 went with stages (ADR 0062), and so did the scenarios that existed only for
them: upstream states, `wait` and pending runs, deployment checks, approvals and waivers.

### ci-promote cases

`plan_test.sh` has 35 cases, which hushrun 0.8 kept in `ci-promote/`, numbered as there, planning
production after staging. With one pipeline, the cases about staging's verdict are gone: 03
(a newer staging run queued: `wait`), 23 and 27 (an upstream-triggered run, the same as a push),
25, 26 and 30 (a staging failure blocking production). The rest are in `pipeline/` under names
that say what they decide. Cases that test how ci-promote read the GitHub API, which hushrun
doesn't do, are covered elsewhere:

| # | ci-promote case | hushrun | Covered by |
|---|---|---|---|
| 07 | Can't read the jobs of a failed run → `blocked` | Events say whether a run failed or was stale; nothing reads jobs. A failed run that recorded nothing is failed by liveness, and gives nothing to reuse | `rules/p06-liveness-failed.yaml` |
| 16 | The API fails listing runs → `blocked`, not `full` | The ledger replaces the API: an unreadable ledger exits 6 with no decision (L-5) | `crates/hushrun/tests/plan.rs` |
| 18 | The head commit is unreadable → `blocked` | No digest, no plan: exit 2, nothing recorded | `crates/hushrun/tests/plan.rs` |
| 19 | A failed run's commit is gone (404) → ignored → `full` | Nothing to look up: the failure belongs to another digest | `pipeline/only-other-content-ran.yaml` |
| 20 | Looking up a staging run's commit fails (500) → `blocked` | Pending runs went with upstream stages (ADR 0062): a plan decides from the ledger's events for its digest, and matches no run to it | — |
| 33 | A custom staging branch and workflow are queried | The branch a repository deploys from is policy (`deploy.branch`) | `pipeline/a-custom-branch.yaml` |
| 34 | A custom stale-step name → superseded → `full` | `guard` records `superseded` (reason `stale`) itself; there is no step name to match | `pipeline/stale-before-the-build-only-builds.yaml`, `differences/N3-stale-after-build.yaml` |
| 35 | A custom deployed context → `skip` | The deployed index replaces commit statuses | `pipeline/already-deployed-is-current.yaml` |

## Format

```yaml
name: "an earlier attempt's tests failed this digest -> full, running them again"
covers: [P-4, P-6, P-15]
description: "A failure doesn't block the pipeline: the failed check runs again, with the build"
policy: acme-api.yaml          # testdata/policies/<file>, or policy_yaml: |
now: "16:00"
context: {branch: main, trigger: push}
events:
  - {kind: started, run: p1, at: "15:00", by: ci}
  - {kind: check, run: p1, at: "15:01", by: ci, check: unit, result: fail}
  - {kind: failed, run: p1, at: "15:02", by: ci, reason: unit failed}
expect:
  mode: full
  checks: [toolchain, unit]
  requirements: {toolchain: missing, unit: failed}
```

Unknown keys are errors, at every level: a file from hushrun 0.8 with `stage:`, `pending:`,
`waived:` or `upstream:` doesn't load.

### Top level

| Key | Meaning |
|---|---|
| `name` | Required. Shown when the scenario fails. |
| `covers` | Rule IDs the scenario covers (see [README](README.md)). |
| `description` | Free text; a case that began in ci-promote names it. |
| `policy` / `policy_yaml` | A fixture name under `testdata/policies/`, or the policy inline. `policy_yaml` wins. |
| `scope` | The scope `X` [default: `default`]. |
| `now` | The planner's clock. |
| `context` | The run being planned, below. |
| `events` | Events for the digest, below. |
| `deployed` | Entries of the deployed index, `v1/deployed/<scope>/_/`, below. |
| `liveness` | The CI provider's answers about attempts without a terminal event (P-6). |
| `ci_only` | `true` when the signers answer says the repository is CI-only (T-16, L-13), with one open period from 00:00 of the scenario's day. Scenario events carry no trusted time, so then only `ci` events count. |
| `expect` | The decision, below. |

**Times.** `HH:MM` or `HH:MM:SS` means that time on 2026-10-13 UTC. A full RFC 3339
timestamp is accepted too.

**References.** A reference ending in `@` and one hex digit expands to `@sha256:` and 64 copies
of that digit: `…/back/go@a` is `…/back/go@sha256:aaaa…`. Write full digests when the digit
shorthand can't express the case.

**Digests.** Scenarios plan for one digest, called `this`. Events and deployments can name
`other`, a second digest, to model other content.

**Policies.** `acme-api.yaml` is one scope, two checks and an image, deployed from `main`.
`deploy.yaml` has every `deploy` setting: two scopes with an artifact each, a check pinned to an
image and platform, a check with no `run` that another workflow attests, `deploy.require`, a
1 h lease, `dedupe: [push]` and `allow_force: false`.

### `context`

| Key | Default | Meaning |
|---|---|---|
| `branch` | none | The branch the run is on. Without one, P-8 doesn't apply. |
| `trigger` | `push` | `push`, `upstream` (another workflow's completion started it), `manual` or `schedule`. |
| `attempt` | `1` | The run attempt. |
| `force` | `false` | `--force-full` (P-9). |
| `run` | `this-run` | The run id. The provider is always `generic`. |

### `events`

Each entry is one verified event. Common keys:

| Key | Default | Meaning |
|---|---|---|
| `kind` | required | `started`, `check`, `artifact`, `deployed`, `passed`, `failed`, `superseded`, or `approval` (hushrun 0.8 history only). |
| `stage` | none | The stage hushrun 0.8 or earlier recorded the event at, for history. Since 0.9 no event has one. |
| `run`, `attempt` | required, `1` | The run that recorded it. Attempts group by (`run`, `attempt`). |
| `at` | required | The event's `time`. |
| `by` | required | Who signed it: `ci` or a name starting `ci-` is the repository's CI (signer `ci`); `agent-<name>` is the agent `agent:<name>`; any other name is the member `user:<name>@example.com` (T-15). |
| `custody` | `ci` | The custody the producer claimed. The effective custody is capped by the signer's: `ci` for the CI, `isolated` for a machine (T-15). |
| `digest` | `this` | `this` or `other`. |
| `scope` | the scenario's | The event's scope. |
| `trigger` | `push` | What started the run that recorded it. |
| `url` | none | The run's URL. |

Keys by kind:

| Kind | Keys |
|---|---|
| `started` | `lease`: a duration such as `30m` [default: `deploy.lease`, else 3 h]. `leaseUntil` is `at` plus it. |
| `check` | `check` (required), `result` (`pass` or `fail`, default `pass`), `finished` (a time; default `at`), `attempts` (default 1), `definition: stale` (a definition digest that isn't the policy's), `image` and `platform` (the recorded environment), `check_kind` (`source` [default], or `deployment` for a 0.8 deployment check). |
| `artifact` | `artifact` (required), `ref` (required). |
| `deployed`, `passed` | `artifacts`: artifact name to reference. |
| `failed`, `superseded` | `reason` [default: `failed`, `cancelled`]. A superseded reason is `stale` or `cancelled`. |
| `approval` | `check` (required). |

### `deployed`

| Key | Default | Meaning |
|---|---|---|
| `digest` | `other` | `this` or `other`. |
| `at`, `by` | required | The entry's time and signer. |
| `run` | `deploy-<index>` | The run that deployed it. |
| `stage` | none | The stage a 0.8 entry was recorded under, `v1/deployed/<scope>/<stage>/`, which doesn't count (P-10). |

### `liveness`

```yaml
liveness:
  - {run: p2, state: no-verdict}       # in-flight, no-verdict, superseded, failed
```

It takes `attempt` [default: 1].

### `expect`

Only `mode` is required; every other key is checked only when present.

| Key | Checks |
|---|---|
| `mode` | The mode, exactly: `promote`, `refresh`, `full`, `current` or `blocked`. |
| `reason` | The reason contains this text. |
| `build` | `true` or `false`. |
| `artifacts` | Artifact name to reference, exactly (the `@x` shorthand works). `{}` means none. |
| `checks` | The checks to run, in order. |
| `requirements` | Required check to status: `satisfied`, `expired`, `failed` or `missing`. |

## Adding a scenario

1. Pick the directory: a new rule case goes in `rules/`, named after the rule it pins down; a
   case about what 0.8 recorded goes in `history/`.
2. Start from a similar file. Keep `events` in the order they happened; the runner doesn't need
   it (I-3 says order never matters), but readers do.
3. Name the rules it covers in `covers:`.
4. Run the scenarios. A new scenario that passes on the first run deserves a second look: break
   the expectation once and check the runner reports it.
