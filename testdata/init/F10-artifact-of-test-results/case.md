# F10: an artifact of test results that must be there

**Pattern.** `pytest --junitxml=test-results/junit.xml`, then `actions/upload-artifact` of
`test-results/` with `if-no-files-found: error`.

**Mirrors.** Discourse and Electron, whose upload fails the job when no file is found.

**Expected after Phase 1.** init wraps the line, which it used to leave for its `--junitxml`, and
predicts `ci: runs` with `ciWhy: reports`, naming `actions/upload-artifact`.
