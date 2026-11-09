# F15: a GitLab job that keeps a JUnit report

**Pattern.** `.gitlab-ci.yml`'s job `test` runs `pytest --junitxml=report.xml`, and keeps
`artifacts:reports:junit`.

**Mirrors.** GitLab projects whose merge requests show test results.

**Expected after Phase 1.** Nothing reads `.gitlab-ci.yml` at run time, so init writes
`HUSHRUN_REPORTS_READ: "true"` into the job's `variables`, wraps the line, and predicts `ci: runs`
with `ciWhy: reports`, in the variable's words.
