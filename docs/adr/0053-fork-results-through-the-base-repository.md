# 0053. Fork results through the base repository

- **Status:** proposed, a design only
- **Date:** 2026-11-05
- **Rules:** C-9, C-10, C-12, C-17; RFC 0002 §3.2's same-tree rule (T-13); results-v0 §2 and §3
- **Would amend, once accepted:** results-v0 §3 (`POST` of a fork's result), and C-10 (when one
  counts)

This ADR designs, and nothing in it is to be built yet: it is decision D6 of
the CI reach plan (Phase 7, 7.2), "design only for now". It names the
questions that must have answers before anyone builds it. [0052](0052-anonymous-lookups-for-public-repositories.md)
lets a fork's pull request read passes; this is about recording them.

## Context

A fork's job can't record: GitHub gives it no ID token and no secret (results-v0 §2). So with
0052 a fork's pull request skips what `main` already passed, but never what it passed itself: a
re-run runs everything again, and after merging, `main` runs what the pull request just ran on
the same tree.

The base repository can record for it. A `workflow_run` workflow runs when the pull request's CI
completes, from the default branch's file, with the base repository's permissions, an ID token
included, and never with the fork's code. Netty, Spark, Django and FastAPI already publish their
test reports this way, as the plan's survey found: the pull request's run uploads an artifact, and
the `workflow_run` job publishes it.

What a fork's run uploads, it chose. Its code runs in the job that writes the file, as the same
user, before and after hushrun, so it can write any record, or put another `hushrun` on `PATH`.
That is the whole problem.

## Design (proposed)

1. **The fork's job writes its results to a file.** With `HUSHRUN_RESULTS_FILE` set and no
   credential, `hushrun run` writes what it would have recorded, each `hushrun.run-result/v0` with
   its key, to that file, at most 64 KiB a record and 256 records, and a later step uploads it:
   `actions/upload-artifact`, named `hushrun-results`, kept a day. It records nothing itself.
2. **The base repository records them.** A workflow on `workflow_run` of the CI workflow,
   `types: [completed]`, with `actions: read` and `id-token: write`, downloads that run's artifact
   and runs `hushrun record --from hushrun-results.json --run <run id>`. That command runs nothing
   from the artifact: it reads JSON, bounded, and checks the run with GitHub's API, never with the
   artifact's word: the run's event is `pull_request`, its head repository is a fork of this one,
   its head commit, and the pull request's merge commit at the time, whose tree it reads with
   `GET /repos/{owner}/{repo}/git/commits/{sha}`.
3. **A fork's result says so.** Each record is posted with the base repository's ID token and
   `"fork": {"pullRequest": <n>, "head": "<sha>", "tree": "<the merge commit's tree>"}`. The
   server keeps it apart from the repository's own CI results: the token is the base's, and it
   would otherwise look like `main`'s.
4. **When it counts (C-10).** A fork's pass never answers a lookup by itself. It counts:
   - for a run whose whole checkout has the result's `tree`: RFC 0002 §3.2's rule, whole trees,
     even for a check with a scope, so that a pass can't stand for files it didn't run on;
   - only on the branch the pull request merges into, and on its merge queue's runs, which test
     that tree.
   A fork's fail answers as any fail does: it makes the next run run.
5. **What a run says.** A skip on a fork's pass says so:
   `hushrun: test skipped: this exact code passed in a pull request from a fork, 2 hours ago`.

## Open questions

These are the plan's three, and none has an answer that this design can give alone.

1. **Forgery before merge.** A fork can upload a pass for its own merge tree without running
   anything, or with tests it broke. Item 4's rule makes that pass count on `main` exactly when
   the trees are equal: right after the pull request merges with nothing in between, the moment
   `main`'s CI exists to catch what it broke. A maintainer reviewed the code, not its results.
   What could stop it:
   - (a) a fork's passes count only for re-runs of the same pull request's tree, never on the
     base branch: safe, since the fork could skip its own checks anyway, and it saves re-runs only;
   - (b) they count on the base branch only after a maintainer approved the run (GitHub's approval
     for outside contributors): it adds a person, but not one who checks results;
   - (c) the base repository records only a fork's fails: they can only make runs run, so they
     can't be forged into a skip; they save nothing, and show flaky checks in the dashboard;
   - (d) not at all.
2. **The same-tree proof.** The tree has to come from GitHub, not from the artifact: a pull
   request's run uses the merge commit GitHub made then (`GITHUB_SHA`, `refs/pull/<n>/merge`), and
   the `workflow_run` event names the head commit, not that one, and for a fork's run its
   `pull_requests` list is empty, so even the pull request is found by its head commit. Which
   calls prove the merge commit the run checked out, when the base moved while it ran? And the run on `main` has to compare its
   own commit's tree, one API call or a lookup in its clone, and run when neither works.
3. **Quotas.** The base repository's ID token records them, so they count against its org's plan,
   or are free in the open-source program. A fork can open many pull requests and push often,
   each run uploading up to 256 results. A cap per pull request? Results free only in the program?
   Fails only, as (c) has it?

And three more the design raises:

4. **`hushrun record` in a privileged job.** It holds an ID token while it reads a file a fork
   wrote. It must parse strictly and run nothing; a bug there is a way into the base repository's
   token.
5. **Who it was.** `recordedBy` would be the base's `workflow_run` subject. The dashboard should
   say "a pull request from a fork", and the person behind it isn't known.
6. **GitLab.** A fork's merge request pipeline runs in the fork's project, or in the parent's with
   a maintainer's approval; this design is GitHub's only.

## Consequences, if built

- With (a): re-runs of a fork's pull request skip what that pull request already passed on the
  same tree, and nothing on `main` changes. With (b) or a later answer to question 1, `main`
  would skip right after a merge too, which is where most of a fork's cost repeats.
- Two workflow files, a new command and an artifact for each repository that takes it: more setup
  than anything else hushrun asks of a repository, and init would have to write the second file.

## Decisions

1. **Keep this a design (D6).** Recommendation: yes, until 0052 is built and measured: if forks
   still run most of what they could skip, the cost is in re-runs and merges, and this is the way
   to it.
2. **When it is built, which answer to forgery.** Recommendation: (a), re-runs only, first. It
   saves the least, and it is the only option in which a fork can't make `main` skip.

## Alternatives considered

- **`pull_request_target`.** It runs the base's workflow with the base's token on the fork's
  event, and running the fork's code there hands the fork that token: the known way to leak a
  repository's secrets.
- **A secret in the fork's job.** GitHub gives a fork's job none, by design.
- **Signing in the fork's job with a key the workflow holds.** The workflow file is in the
  repository, so the key would be public, and anyone could record as the fork.
- **Recording nothing, and reading only (0052).** It is the recommended first step, and this
  design is what would come after it.
