# 0049. Each variant is its own job

- **Status:** accepted
- **Date:** 2026-11-05
- **Rules:** C-10, C-12, C-14, C-15, C-22, §10.1
- **Amends:** [0044](0044-one-key-for-runs-that-differ.md): items 1, 3, 4, 7 and 8, and its
  Consequences

## Context

ADR 0044 made a CI pass belong to its job, and stopped a job with several variants from skipping,
since one key and one `job` stood for all of them. That ended a wrong skip, and it cost most of
what CI could skip. In a survey of 100 repositories' CI (2026-11-03), 823 jobs run tests:

- 263 of them (32%), in 72 repositories, have several variants: a matrix, or GitLab's `parallel`.
  None skips in CI.
- 113 (14%) sit in reusable workflows. GitHub names the caller's file in `GITHUB_WORKFLOW_REF` and
  the called file's job in `GITHUB_JOB`, so hushrun can't find the job, and none skips.
- When two jobs run one check, the lookup answers the newest result for the key, whatever job
  recorded it, so each job's pass hides the other's and neither skips. ADR 0044 listed this.

0044's way round the first was a name per variant, `hushrun run test-${{ strategy.job-index }} --`.
Few repositories do it, and an index is a position: reorder the matrix, and index 3 is another
variant.

A step can see its variant after all. GitHub fills in `${{ toJSON(matrix) }}` with the values of
the variant that runs, and `${{ toJSON(inputs) }}` with the inputs a called workflow got. What it
can't do is put them in the key, which holds no environment so that a laptop's pass stands in for
CI's (C-9).

## Decision

1. **A lookup asks for the newest result that counts in this job** (amends 0044 item 2's "Lookup
   and protocol don't change"). In CI, the CLI asks
   `GET {base}/v0/results/{key}?job=<job>[&variant=<variant key>]`, and a server answers the newest
   pass or fail among the results that count there, with `"forJob": true`. A result counts when it
   has this `job` and this variant (none, when none is asked); or comes from outside CI, provider
   `local`, when no variant is asked; or from a generic CI that names no job, when no variant is
   asked. The agent token's repository, `ciOnly` and `?as=ci` combine with it as before. So two
   jobs that run one check each find their own pass. A server from before this ignores the query
   and answers the newest result for the key; the CLI checks it again with the same rule, and
   decides as 0.7.0 did: correctly, with fewer skips. Outside CI nothing changes.
2. **A job of several variants says which one each run is** (amends 0044 items 1 and 3). init writes
   `HUSHRUN_VARIANT: ${{ toJSON(matrix) }}` in the job's `env`. The key still holds no environment,
   since that is what lets a laptop's pass skip a plain job. The variant goes beside `job`, as the
   result's `variant`: `key`, the `sha256:` of the value's JCS form, so that key order and spaces
   don't matter, and `label`, the values in key order, at most 100 characters, for people to read.
   A run in such a job skips only when `HUSHRUN_VARIANT` is set, is a JSON object (or `null`) of at
   most 64 KiB, and the job's own `env` in the workflow file sets it to `${{ toJSON(matrix) }}`,
   whatever its spaces and case. Then GitHub filled in this variant's values, and nothing else did.
   Anything else runs, saying why. It works for a matrix an expression builds too, since the value
   is the variant as it ran. A name per variant still counts (0044 item 5); with both, the variable
   decides.
3. **In a job with variants, no pass from outside CI counts.** A laptop or an agent's sandbox ran no
   variant, so its pass can't say which one passed. This keeps what 0044 did there: no pass from
   outside CI ever skipped a variant. Outside CI, a pass still skips on a laptop or in a sandbox.
4. **A called workflow's jobs name their own file** (amends 0044 item 4). init writes
   `HUSHRUN_WORKFLOW: <the file's own path>` in each job of a file that a running workflow of this
   repository calls with `uses: ./…`, and `HUSHRUN_INPUTS: ${{ toJSON(inputs) }}` when the file
   takes inputs. With `HUSHRUN_WORKFLOW` set, the run reads that file, under `.github/workflows/` in
   this checkout, finds `jobs.<GITHUB_JOB>`, and goes on only when that job sets `HUSHRUN_WORKFLOW`
   to that very path and the file starts on `workflow_call`. The variant's key is then over
   `{"inputs": …, "matrix": …}`, each `null` when absent, and its label holds only the matrix's
   values: inputs show nowhere, not in a label, a record or a line. GitHub passes secrets through
   `secrets:`, never `inputs`, so inputs hold none, and they are hashed anyway. A value the job's
   `env` sets counts whatever the file says of its variants, since it is what GitHub filled in, and
   a variable the run has that the file's job doesn't set means that file isn't the one running: a
   caller's checkout can hold one at the same path when another repository's workflow is called,
   so it runs. GitHub doesn't pass a caller's workflow-level `env` to a called workflow, so the
   value comes from the called file. A file another repository calls can't be read, and runs, as
   before. 0044's rule for a call with this job's id stays for runs without `HUSHRUN_WORKFLOW`.
5. **On GitLab, each instance is its own variant** (amends 0044 item 3). `CI_JOB_NAME` already
   differs per `parallel` instance and `parallel:matrix` variant, so a CI pass counts for its own
   instance. The run still takes a variant, from that name, so that no pass from outside CI counts
   there (item 3).
6. **init writes it all in the same pass as the wraps** (amends 0044 item 7). A CI file's own line in
   a job with variants is wrapped now, since each variant says which it is. A job gets the variables
   when it runs a check through hushrun: a line init wraps, a wrapped package.json script, or a line
   through hushrun already. A value that is right stays; a value set otherwise stays too, and init's
   notes say what it should be. An `env` init can't edit as lines (in flow style, an anchor, an
   alias, or one that comes through `<<`) gets the line to add, in the notes. A job that calls a
   file whose jobs need an ID token gets the permission too, since a called workflow has no more
   than its caller gives it. The plan's JSON says `variantIdentity: env` for a job whose own `env`
   says which run it is, a called workflow's that names its file included, and `named` for GitLab's
   instances. A file that doesn't read as YAML gets nothing, and its commands are left, saying why.
7. **`ready` and `doctor` say so** (amends 0044 item 8). A laptop can't know a variant's values, so
   `ready` keeps a check in a job that tells its variants apart as `ci`: `CI runs each variant until
   that variant passes on this code`, or `each call` for a called workflow that takes inputs. A job
   that doesn't tell them apart says what to set, and to run `hushrun init`. `doctor`'s `variants`
   check says `ok` for each, and warns of a job that sets `HUSHRUN_VARIANT`, `HUSHRUN_WORKFLOW` or
   `HUSHRUN_INPUTS` to something else, since no pass skips it there. Both read the files a running
   workflow calls, as init does.

## Consequences

Measured on 2026-11-05.

- **On the survey's 100 repositories, 133 of 823 test jobs skip in CI,** 42 more than before: 31
  matrix jobs whose lines init left, 5 matrix jobs' scripts that ran every time, and 6 jobs of
  called workflows init didn't read. No job that skipped stops (the corpus harness, A8).
- **A check run by two jobs skips in both on a re-run.** The lookup is one condition more, and a
  key's results are still walked newest first, stopping at the first that counts. `EXPLAIN` on a
  key with 20,001 results over four jobs: 0.02 to 0.06 ms when the job's pass is among the newest,
  4 to 18 ms when nothing counts, as for a job that never ran. An index on the job didn't help,
  since passes from outside CI count in every job, so there is none, and no migration.
- **A matrix job skips per variant, and a reusable workflow per call,** once init has written its
  variables: a repository set up before this gets them from `hushrun init` again.
- **A pass from outside CI still never skips a variant.** In a matrix job, CI runs each variant
  until that variant passes on this code. A laptop and an agent skip as before.
- **Older pieces stay safe.** An older CLI in such a job ignores the variables and runs every
  variant, as 0044 says; its passes have no `variant`, so a newer CLI's variant doesn't take them.
  A server from before this answers the newest result for the key, and the CLI runs when it isn't
  one that counts.
- **The dashboard shows a run's variant by its label,** never its key (api-v1 `CheckRun.variant`).
  A call with inputs and no matrix has an empty label, and shows none.
- **Limits.**
  - Two calls with the same inputs are one variant, and share a pass, which is what they are.
  - A call whose inputs change on every run, such as a time, never skips.
  - A job whose file sets the variable some other way runs every time, and `doctor` warns.
  - A variable set for the whole workflow, not in the job's `env`, isn't the job's own, and runs.
  - `HUSHRUN_VARIANT` larger than 64 KiB runs every time. A label longer than 100 characters is
    cut, with `…`; the key is over the whole value.
  - A file a step starts still isn't read, so 0044's limit on names written in a script stands for
    runs without the variable.
  - `hushrun ready` still asks without a job, since a laptop doesn't know a CI job's platform: a
    check two jobs run says `ci` when its newest pass is the other job's, though CI now skips
    both. It errs toward CI, never toward a skip.

## Alternatives considered

- **`strategy.job-index` as the variant.** It is a position: reorder the matrix, and one index is
  another variant, with the old one's pass.
- **Trusting `HUSHRUN_VARIANT` without reading the file.** A constant set for the whole workflow, or
  in a step, would make every variant one, and the first to pass would skip the rest: the wrong skip
  0044 ended.
- **The variant in the key.** The key is what C-9 fixes for every machine, and what `ready` and the
  dashboard find a check by. Splitting it per variant would change every key in such a job, which a
  contract that only grows can't, and a lookup filter does the same additively.
- **Asking without the variant and choosing in the CLI.** The newest result for the key would still
  hide a variant's own older pass, the very problem of two jobs.
- **Only a name per variant, as 0044 advised.** Few repositories follow it, and it moves with the
  matrix.
- **A variant from outside CI counting where it matches.** A laptop has no matrix, and its values
  would be whatever a person typed.
