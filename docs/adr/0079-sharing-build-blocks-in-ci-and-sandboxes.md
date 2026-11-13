# 0079. Sharing build blocks in CI and sandboxes

- **Status:** accepted
- **Date:** 2026-11-13
- **Rules:** B-2, B-25, B-26, B-28, B-29, B-30, B-31, B-34, B-36

## Context

[ADR 0078](0078-sharing-build-blocks-through-a-backend.md) shared blocks between people's
machines and left CI unchanged. A CI job is where most compiles happen and where a shared store
pays most: every job starts cold. It differs from a laptop in three ways that shaped the rules: its
disk and everything on it go when the job ends; it has no person's login, only what the CI
system gives it (in GitHub Actions an ID token, elsewhere at most a secret); and a job that runs
on a branch nobody reviewed must not write what other jobs trust. An agent's sandbox differs
again: its setup phase has the network and a token, and its later phases may have neither.

## Decision

- **CI is unchanged unless the org shares.** The wrapper is set in CI only when the results
  lookup carried `Hushrun-Build: on`, the job can be asked as itself, and `open` answers within
  5 seconds; cargo starts after. Without an answer, one line; an org that doesn't share costs
  nothing and says nothing (B-2).
- **The job's own store.** Under `RUNNER_TEMP`, named by the repository, run, attempt, job and
  the job's request token, with a 2 GB floor and no person's settings; nothing outlives the job,
  another job's store found there is removed unread, and only markers of this job and store go
  up (B-31).
- **The job's ID token, always, in GitHub Actions.** Beside an agent token too: what the
  backend trusts a job by are the token's claims. Renewed in the background within a minute of
  its expiry and sent until it expires, minted again after a 401, waited for only within a
  request's own deadline, and never minted once the command has ended. Anywhere else in CI an
  agent token reads and never uploads (B-31).
- **The commit on every request**, lookups included, so a backend that decides trust by the
  commit decides it for every request (B-31); an older backend ignores the field. A job whose
  tracked files differ from its commit names none.
- **The end of a job waits for its uploads**, longer for a job that writes trusted blocks, never
  more than a quarter of what the waiting blocks cost to compile, for the whole call: one
  deadline, `close` within it (B-29, B-31).
- **`hushrun build fetch` for sandboxes.** The setup phase saves sealed packs, hints and the
  policy for 24 hours, no token; later phases run a relay from it that sends nothing (B-36).
- **From the review of the relay as built:** a question about a stale vouch never waits for
  `open`, and the run's stale vouches are asked in one lookup (B-34); the prefetch starts at the
  first wrapper that misses (B-30); a pack can't unpack past 64 times its size or the disk's
  floor (B-28); a downloaded block that differs is suspended and replaced in one step (B-32);
  and every variable no CI system set is searched for from 8 bytes (B-24).

## Consequences

- A repository's first sharing job compiles everything and uploads within its wait; the next
  job downloads. A job cut off mid-upload leaves nothing behind: its store goes with it.
- `hushrun init` writes nothing new for GitHub: a job that already has `id-token: write` for
  results shares as soon as the org turns sharing on.
- A sandbox's fetch can be up to a day stale; within that day a withdrawn block may be reused
  there, as on any machine that can reach no server (B-34).
- Values under 8 bytes that a build reads without saying remain the documented limit, caught
  only by the audit and `observe` (B-24).
