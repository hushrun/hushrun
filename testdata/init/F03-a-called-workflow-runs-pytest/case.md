# F03: a caller and the reusable workflow that runs pytest

**Pattern.** `ci.yml` runs on pushes and pull requests and calls
`uses: ./.github/workflows/test.yml`; `test.yml` starts only on `workflow_call`, and runs
`python -m pytest`.

**Mirrors.** uv and Pydantic: 113 of the survey's test jobs sit in reusable workflows.

**Expected today (Phase 0).** init reads no file that only `workflow_call` starts, so it wraps
nothing, and lists no job: `jobs: []`, with the note that it found no check it knows.

**Expected after Phase 3.** The called file's job is read, gets `HUSHRUN_WORKFLOW` naming its own
path, and its `calledBy` names `ci.yml`'s job `test`.
