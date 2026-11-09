# F04: the test command sits in a local composite action

**Pattern.** The job's last step is `uses: ./.github/actions/run-tests`, whose `action.yml` runs
`python -m pytest`.

**Mirrors.** pandas' `.github/actions/run-tests`: 62 of the survey's test jobs keep their command
in a composite action.

**Expected today (Phase 0).** init reads no `action.yml`, so it sees no check: `jobs: []`.

**Expected after Phase 4.** The action's line is wrapped in `action.yml`, and the job that uses it
gets the setup step.
