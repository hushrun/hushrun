# F07: coverage turned on in pyproject.toml, read by Codecov

**Pattern.** `addopts = "--cov=src --cov-report=xml"` in `pyproject.toml`, a plain `pytest`, and
`codecov/codecov-action` uploading `coverage.xml`.

**Mirrors.** Projects that turn coverage on in their configuration, so the CI line holds no flag.

**Expected after Phase 1.** init wraps `pytest`. The line writes no flag, but its job reads reports,
so the plan predicts `ci: runs` with `ciWhy: reports`, naming `codecov/codecov-action`: what the
job reads decides, whatever the line says.
