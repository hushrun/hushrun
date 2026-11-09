# F22: a calling job's matrix feeds the called file's inputs

**Pattern.** `ci.yml`'s job `test` has a matrix over Python versions and calls
`./.github/workflows/test.yml` with `python-version: ${{ matrix.python }}`; `test.yml` runs
`python -m pytest`.

**Mirrors.** Pydantic, whose test matrix calls a reusable workflow.

**Expected after Phase 3.** The called job has no matrix of its own: each call is told apart by its
inputs, which `HUSHRUN_INPUTS` carries, so the calling job's matrix needs nothing of its own.
`HUSHRUN_WORKFLOW` and `HUSHRUN_INPUTS` in the called job's `env`, the permission in the calling
job, and `ci: skips`, each call on its own pass.
