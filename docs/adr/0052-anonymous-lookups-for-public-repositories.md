# 0052. Anonymous lookups for public repositories

- **Status:** accepted, and built
- **Date:** 2026-11-05, accepted the same day with each recommendation below
- **Rules:** C-10, C-11, C-13, C-14; results-v0 §2 and §3 for the server; api-v1 §12.5
- **Amends:** results-v0 §2 (a fourth caller, for one request), and
  [0049](0049-each-variant-is-its-own-job.md)'s lookup, which it asks as is

This ADR is decision D5 of the CI reach plan (Phase 7, 7.1), which
said to decide on the ADR before any code. It was accepted with the recommendation of each
of the decisions below. [0053](0053-fork-results-through-the-base-repository.md), its other half,
stays a design only.

## Context

A pull request from a fork runs in the base repository's workflow, with the fork's code, and
GitHub gives its job no ID token and no secret, whatever the workflow's `permissions` say. So a
fork's `hushrun run` has no credential: its lookup gets 401 and it runs every check, and it can't
record what it ran (results-v0 §2). In the survey of 100 repositories' CI, 54% of merged pull
requests came from forks. For an open source project, that is most of its CI.

Most of a fork's pull request is code that already passed. Phase 5 gives each workspace package a
scope, so a pull request that touches one package has the same keys as `main` for every other
package's checks, and `main`'s CI recorded passes under them. A fork's run only needs to read
those passes; it never needs to write.

Reading is safe in a way writing isn't. A fork controls its job: its workflow file and its code
run there, so it can always skip its own checks, by editing them out. A lookup lets it skip only
what an existing pass under the same key stands for; it adds no way to make a check pass. The
questions are what an answer tells someone who isn't the org: the plan lists enumeration,
privacy, abuse, and repositories that go private.

## Decision

1. **Who may ask.** `GET {base}/v0/results/{key}` with no `Authorization`, with
   `?as=ci&repository=<id>`, where `<id>` is `github.com/<owner>/<name>`, answers when the org has
   that repository in the open-source program, approved (api-v1 §12.5), and the server last found
   it public at most an hour ago (item 4). Anything else stays 401, as today, with today's
   message. No other request takes no credential.
2. **What it answers.** The newest pass or fail that counts in the asked job and variant, as ADR
   0049's `?job=&variant=` reads them, among results that repository's own CI recorded: an ID
   token whose `repository_id` is the repository's pinned GitHub id. The server keeps that id with
   each result an ID token records, from now on, so a rename keeps answering and a result's
   `repository`, which is the client's word, never decides. Results recorded before have no id,
   and don't answer.
   - `recordedBy` is `ci`: no subject, which holds the ref, and no email.
   - The record is as posted, without `invokedBy`. `reports` stays.
   - `ciOnly` is moot: only CI's results answer.
3. **Limits.** 300 anonymous lookups a minute per client address, and 3,000 a minute per
   repository, each per instance of the server, then 429 `rate_limited` with `Retry-After: 60`,
   which the CLI doesn't wait for: the check runs. An IPv6 address counts by its /64, and a limiter
   keeps at most 50,000 addresses a minute, refusing new ones past that, so that addresses sprayed
   at it can't grow its memory. The address is the client's as the server's front reports
   it: the build reads it from the load balancer's `X-Forwarded-For` without trusting the part a client
   can write. A lookup is one index read, and anonymous lookups are free, as
   every lookup is.
4. **Public now, not once.** The program reads the repository from GitHub's API when it applies
   and from every ID token afterwards (`repository_visibility`), so a check stops being free on the
   next run of a repository that went private. A lookup has no token, so the server keeps
   `public_checked_at` with the repository: an ID token of the repository's CI sets it, public or
   not, and a lookup more than an hour after it reads the repository from GitHub's API again, with
   the OAuth app's quota, before it answers. Not public, gone, answered with another id than the
   pinned one, or GitHub unreachable: 401, and the repository answers nothing until a check says
   public again. When GitHub doesn't answer, the visibility is unknown, and the next lookup asks
   again five minutes later, so an outage costs one call per repository every five minutes, not one
   per lookup. Each ID token also brings the program's name for the repository up to date, as
   applying again under a new name does, so a fork of a renamed repository, which asks under the
   new name, is answered after the repository's next CI run.
5. **The CLI.** In GitHub Actions with no credential, as in a fork's pull request
   (`ACTIONS_ID_TOKEN_REQUEST_URL` unset, no `HUSHRUN_TOKEN`, `HUSHRUN_AGENT` or login), the lookup
   goes with `as=ci&repository=` of the run's repository, `GITHUB_REPOSITORY` (the base
   repository's, in a fork's pull request), and `job` and `variant` as any run's. A pass that
   counts skips, and says where it ran, as any skip does. It records nothing: there is no
   credential to record with, and it tries no `POST`. A 401 or a 429 means what it means today: the
   check runs, and the run ends with the line that says why, which already tells a fork's pull
   request that GitHub gives it no ID token. A run whose lookup was answered ends with `results
   aren't shared with acme: GitHub gives a pull request from a fork no ID token, so it records
   nothing` instead, or, in a job of the repository itself without `id-token: write`, with
   `` no token: give the job `permissions: id-token: write` ``. Outside GitHub Actions, and on a
   GitHub other than github.com, hushrun never asks anonymously: a laptop signs in.
6. **Contracts only grow.** results-v0 §2 gets a fourth caller, "no credential", for this one
   request; §3 says what it answers. An older CLI asks as it does today and gets the same 401 from
   any repository the program hasn't approved. An older server answers 401 to everything, and a
   new CLI runs, as now.

## Consequences

- **What forks gain.** A fork's pull request skips the checks whose scope it didn't change,
  while the branch is up to date with its base: in a workspace, every package it didn't touch
  (Phase 5); in a repository of one scope, nothing, since any change runs everything. Re-runs of
  the fork's own checks still run: it records nothing to skip on. The plan's measure is the
  survey's 54%: none of those pull requests skips today.
- **Enumeration.** A key is the SHA-256 of what the check reads, its command and its directory
  (C-9): asking needs the files, so an answer says only whether CI passed on files the asker has.
  For a public repository with public CI logs, that is already public. Nothing lists keys.
- **Privacy.** No email, no agent's name, no machine, no `invokedBy`, and only CI's own results:
  a member's laptop and an agent never show to someone outside the org.
- **Going private.** Answers stop within an hour of the change, or at the next CI run, whichever
  comes first. What they answered in that hour was CI's results for code that was public then.
- **Limits.**
  - GitHub's hosted runners share addresses, so one busy address can hit 300 a minute with
    others' lookups. A 429 costs runs, never a failure, and the per-repository cap is what
    protects the database.
  - Only the open-source program's repositories.
  - GitLab has no anonymous path: its fork pipelines use agent tokens, which forks don't get.
  - A pass from a member's laptop doesn't answer, though the repository's own CI would skip on it
    unless "CI reuses only CI's results" is on.

## Decisions

Each was decided as recommended, on 2026-11-05.

1. **Build anonymous lookups (D5).** Recommendation: yes, as items 1 to 6 say. Reading can't make
   a check pass, and a fork could always skip its own checks.
2. **Which repositories.** (a) the open-source program's approved repositories, which the server
   already verified are public, not forks and licensed, and a person read while the limit is set;
   or (b) any repository on github.com an admin turns it on for, once the server finds it public.
   Recommendation: (a) now. (b) needs a setting, and a reason to want it outside the program.
3. **Whose results answer.** (a) the repository's own CI's only, with `recordedBy` `ci`; or (b) the
   plan's: any result for the repository, with `recordedBy` reduced to `ci`, `person` or `agent`.
   Recommendation: (a). A laptop's pass rarely has a fork's tree, (b) would show members' activity
   to anyone with the files, and (a) answers from the one identity a client can't choose.
4. **How fresh.** An hour, read again on demand, and every ID token, or the plan's daily check.
   Recommendation: an hour. It costs one GitHub call per repository per hour of lookups.
5. **The limits.** 300 a minute per address and 3,000 per repository. Recommendation: yes, and
   measure the 429s from GitHub's runners in the first weeks.

## Alternatives considered

- **A read token per repository, written in the workflow.** A fork's job sees no secret, so it
  would be in the file, readable by anyone: an anonymous lookup with a secret to rotate.
- **`pull_request_target`, which runs the base's workflow with the base's ID token.** Checking out
  and running the fork's code there is the known way to leak a repository's secrets and tokens; no
  guide of hushrun's should ask for it.
- **Recording forks' results** through the base repository: [0053](0053-fork-results-through-the-base-repository.md).
  It adds skips on re-runs and on `main`, and it is where the risk is: forgery.
- **Answering from a result's `repository`.** It is the client's word: any member, or an agent
  token for the whole org, could make a public repository's anonymous answers say pass.
- **No limit.** Lookups aren't limited for callers with a credential, who are the org's. An
  anonymous caller is anyone.
