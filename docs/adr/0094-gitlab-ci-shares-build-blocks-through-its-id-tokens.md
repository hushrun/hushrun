# 0094. GitLab CI shares build blocks through its ID tokens

- **Status:** accepted
- **Date:** 2026-11-16
- **Rules:** B-25, B-26, B-31, B-48, B-49

## Context

[ADR 0079](0079-sharing-build-blocks-in-ci-and-sandboxes.md) trusts a CI job by its own ID
token, and only GitHub Actions had one: anywhere else in CI an agent token read the org's blocks
and uploaded none. In GitLab CI that was a hole. The backend can't tell an agent token used in a
GitLab job from one used in a sandbox, so a merge request's job holding the pipeline's agent token
read what the org shares, with whatever scope that token had. The plan for gitlab.com (step 4b,
§3.17 of `build-blocks-across-machines.md` in the cloud repository) and its server are built: the
backend verifies gitlab.com's ID tokens, decides a trusted ref from eleven conditions, and asks a
job to prove it is the job, since a GitLab token can't be renewed or revoked and works until the
job's timeout, also after the job ended. This is the CLI's side of that contract (build-v0 §2.1).

GitLab gives a job no folder of its own, as `RUNNER_TEMP` is on GitHub. The CLI's fallback store
for CI, `hushrun-build-<process id>` under the system's temporary folder, removed a folder it found
at its path as "a dead run's". Process ids repeat across containers, and a shell runner or a
Docker volume shares `/tmp` between jobs, so a trusted job could remove a merge request job's live
store and then build into a folder that job was still writing (the design review's finding 6).

## Decision

**The token.** In GitLab CI the credential for build blocks is the job's ID token, read from
`HUSHRUN_ID_TOKEN` alone, which the job declares with
`id_tokens: {HUSHRUN_ID_TOKEN: {aud: <the org's URL>}}`, one string. One function reads it and
decides one of three things: absent, unusable (not gitlab.com's issuer exactly, another audience
or an array of them, no `job_id` or `jti`, expired), or usable. An unusable token is sent nowhere,
and the run says why in one line whatever the org does. A usable one goes to no server but its
audience: a repository's files can name any backend, and a GitLab token handed to another server
could write at hushrun's for the rest of the job's timeout. It is the bearer of every build
request, never renewed. On the results lookup it rides beside the agent token that is the
lookup's bearer, in `Hushrun-Id-Token`, so whether the org shares is answered by CI's rule for the
job's project, and a job of an org that doesn't share makes no build request. A job with no token
to look results up with lets `open` decide, as a run of several scopes does.

**Only the ID token counts.** In GitLab CI a person's or an agent's token reads nothing of the
shared store and uploads nothing. The backend can enforce this only for a request that says it
comes from GitLab CI, so the CLI's relay asks with the job's ID token or not at all.

**The job's key.** The job's first run that shares makes an Ed25519 key and keeps it in one file
under the system's temporary folder, named by the token's `job_id` and a digest of its `jti`,
written whole at mode 0600 and hard-linked into place, so of two runs starting at once one wins
and the other reads its key. Every later run reads the same file, because a second key withdraws
the job on the backend. A file that isn't a plain file of this user's alone, or isn't this job's,
is never read as the key nor replaced: that run shares nothing. Every build request, `open` first,
carries the public half and the key's signature over `hushrun_core::blocks::job_message`, added in
the one function every build request goes through. The message, the header names and the new
words of `open` live in `hushrun_core::blocks`, the wire types the backend pins, so the two sides
share one definition.

**Close after every run.** The backend serves what a GitLab job wrote as trusted only once that
job closed a run with its key. Every `hushrun run` whose `open` answered sends `close`, whatever
COMMAND did. No `begin` starts with less than 2 minutes left of the token: it couldn't be
committed.

**A store of the run's own.** Without a job folder, a CI run's store is
`hushrun-build-<CI_JOB_ID or process id>-<16 random hex>`, made exclusively at mode 0700, its lock
held for the run's life, and removed when the run's session ends. A folder already at that path is
never adopted or removed. A run removes another run's store only when its lock is free and was
made over an hour before: a run that crashed. This holds for every CI without a job folder, not
only GitLab's.

**What is not built.** A self-managed GitLab: its issuer and keys need rules of their own. Results
through GitLab's ID tokens: results keep their agent token in this step. `hushrun init` writes no
`.gitlab-ci.yml` (the site's page has it), so the `id_tokens:` block is in the guide and the site's
docs, not in a file init writes.

## Consequences

- A GitLab job's build reads and writes as its project's CI, and the backend decides what it may
  write from the token's claims. A merge request from a fork reads trusted blocks and writes
  nothing.
- A GitLab job that held an agent token and read the org's blocks no longer does: sharing is off
  there until the job declares `HUSHRUN_ID_TOKEN`, and the run says how in one line.
- A `target/` restored from GitLab's cache is keyed by content, since the run's store starts empty
  and confirms none of the records the cache brought (B-6, B-31, fixed with
  [ADR 0079](0079-sharing-build-blocks-in-ci-and-sandboxes.md)'s release). A job that caches
  `target/` gains nothing from it for sharing.
- Two steps of one job that see different temporary folders make two keys and withdraw their own
  job. GitLab runs a job's script in one shell, so this takes a `TMPDIR` changed between steps.
- A key file stays in a shell runner's `/tmp` until its token has been expired for an hour and
  another run starts there.

## What only a real gitlab.com project can settle

Whether an ordinary job's token carries the claims the backend requires, and their exact form
(`ci_config_ref_uri`'s case, `job_source`); the token's lifetime in each kind of job; whether
`/tmp` and `TMPDIR` stay the same across a job's script lines on each executor; whether a hosted
runner restores one job's cache into another's. The tests here run against the CLI's own fake of
the contract.
