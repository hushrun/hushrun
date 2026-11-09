# F21: one called file, called twice with different inputs

**Pattern.** `ci.yml` calls `./.github/workflows/test.yml` from two jobs, with
`python-version: "3.12"` and `"3.13"`; `test.yml` runs `python -m pytest`.

**Mirrors.** uv, which calls one test workflow per Python version.

**Expected after Phase 3.** The called job gets `HUSHRUN_WORKFLOW` naming its own file and
`HUSHRUN_INPUTS: ${{ toJSON(inputs) }}`, so each call skips only on its own pass; both calling jobs
get the ID token permission; `calledBy` names both. The inputs show nowhere.
