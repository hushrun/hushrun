# 0056. Readers derive promotion state as the planner does

- **Status:** accepted
- **Superseded in part by:** [0062](0062-no-stages.md)
- **Date:** 2026-11-07
- **Rules:** P-5, P-6, P-7, P-8, P-10, §8.1, L-2, L-5, L-8, L-9, T-16, E-4, plan §10.4 (run identity)

## Context

An audit of hushrun's implicit state machines (2026-11-06) found the same promotion facts
derived in several places, each a little differently:

- The approvals page of hushrun's server took "staging passed this digest" from staging's newest
  *passed* attempt, while the planner's P-7 takes the newest attempt that isn't superseded: after
  a re-run that failed, the page offered a digest the planner then blocked. The server also
  counted staging's events by staging's CI-only rule, where P-5 counts an upstream's events by
  the target stage's (T-16), and took what a stage runs from deployed events in `v1/events` too,
  which the planner never reads (L-8). The server now calls the planner's own P-5 … P-7
  (`hushrun_core::plan::upstream`), and P-6's terminal state, lease and 60 s rule, instead of
  copying them; a test pins `upstream` to `plan` across every scenario (api-v1 §6.4 says so).
- `plan` treated a queued upstream run as having recorded something once *any* verified event
  named its id, while P-5 groups only lifecycle events from signers that count for the target.
  An event a signer that doesn't count wrote under a CI run's id took the run off the pending
  list, and P-7 never saw it either: the upstream read `never` instead of `in-flight`. Likewise
  `plan`'s "one more than the highest attempt already recorded" counted every event of the run.
- `mark passed` skipped a `passed` P-6 would ignore by comparing the `failed`'s time with when
  the command started, while the event's `time` is when it is signed (E-4), and P-6 compares
  that: a `passed` signed 60 s or more after the failure was dropped though it would have
  counted. The 60 s rule was a second constant in the CLI.
- With `on_repository_error: full`, an unreadable repository gave every scope `full` without
  the planner, so P-8's branch rule, which nothing overrides and which needs no events, didn't
  apply: a push to another branch built and deployed the stage.
- Reading the deployed index stopped at the first page with an entry that *verifies*, and the
  planner then kept only those that count for the stage (T-16). If the newest 20 were deploys
  that don't count at a CI-only stage (a laptop's, untimestamped, from before it became
  CI-only), the planner found no deployment, so P-10 didn't fire, and GC found none to keep, so
  it could delete the entry the planner should have read. GC also broke same-millisecond ties
  the other way from the planner.
- A `deployed` event is two writes. When the second, to the deployed index, failed, the event was
  in `v1/events` only: hushrun's server counted it as what the stage runs and the planner didn't,
  and running `mark deployed` again wrote a second pair, leaving the half-written one behind.

## Decision

1. **What a run has recorded is what makes its attempts (P-5).** A pending upstream run (§8.1)
   is one whose current attempt has no lifecycle event at the upstream that counts for the target
   stage (T-16); on GitLab, no attempt of the pipeline has one. The attempt `plan` takes when
   nothing names one (plan §10.4, item 4) is one more than the highest attempt among the run's
   lifecycle events at the stage that count there. Both use the predicate P-5 groups attempts by,
   `hushrun_core::plan::records_attempt`.
2. **A writer judges a `passed` by the time it is signed with.** `mark passed` (and anything
   that records a terminal event through the same path) doesn't record a `passed` that is less
   than 60 s after a `failed` of its attempt, measured from the `time` it is signed with (E-4),
   with P-6's own rule (`hushrun_core::plan::failure_wins`). When the attempt has a `failed`, it
   signs the event once to learn that time, and again after any implicit `started`, so the
   `passed` still comes after it.
3. **`on_repository_error: full` keeps the branch rule (L-5, P-8).** When the repository can't
   be read and the stage opts in, a run on a branch the stage doesn't deploy from is `blocked`
   with P-8's reason, exit 3, as it is when the repository answers; on its branch, or when the
   branch is unknown, it is `full` as before. Both use `hushrun_core::plan::wrong_branch`.
   Validation still allows `on_repository_error: full` with `branch`: the rule needs no
   evidence.
4. **The deployed index is read until an entry counts (L-8, L-9).** "Trusted" in L-8 means
   it verifies and counts for the stage (T-16): reading goes on past entries that verify but
   don't count, up to the 200 entries L-8 allows. GC keeps the entry the planner reads, chosen by
   the planner's own `newest_deployment` (so with its tie-break, the smaller key), and the
   newest entry that verifies whoever signed it, with the same tie-break.
5. **A half-written deploy is completed, not repeated (L-2).** `mark deployed` for an attempt
   that already recorded the same deployment (the same artifacts and target) in `v1/events`, and
   whose deployed-index entry is missing, writes that entry with the event's own bytes, at the
   key the event's time and ULID give, and records no new event. With both copies there, it
   records a new deployment as before. What a stage runs is the deployed index's alone, on the
   server too (api-v1 §4, §6.4).
6. **hushrun's server reads with the planner's code.** Which digests wait for an approval
   (api-v1 §6.4) uses `hushrun_core::plan::upstream` over the index's events, with the target
   stage's CI-only rule and no liveness answers or pending runs, and what a stage runs is the
   newest entry of its deployed index that counts for it. The index's attempt rows take P-6 from
   `hushrun_core::plan::terminal` and `lease_end`. A testkit test pins `upstream` to `plan`
   across every scenario, and new scenarios hold the planner to the cases above.
