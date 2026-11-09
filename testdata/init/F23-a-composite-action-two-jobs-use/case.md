# F23: a composite action two jobs use

**Pattern.** `.github/actions/run-tests/action.yml` runs `pytest -m "${{ inputs.pattern }}"` in a
`bash -el {0}` shell; job `pytest`, a matrix over two platforms, uses it with its default, and job
`minimum-versions` with a pattern of its own.

**Mirrors.** pandas, whose test jobs all run `.github/actions/run-tests`: 62 of the survey's test
jobs keep their command in a composite action.

**Expected after Phase 4.** The action's line is wrapped once, in `action.yml`, and shows under
both jobs with its `action`. Each job gets hushrun's setup above the step that uses the action and
its ID token; the matrix job says its variant in its `env`. Each job, and each variant, skips on
its own pass: GitHub names the calling job in the action's step.
