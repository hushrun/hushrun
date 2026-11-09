# F01: a coverage flag init doesn't see, beside a Codecov upload

**Pattern.** `uv run pytest --cov=src --cov-report=xml`, then `codecov/codecov-action` uploading
the `coverage.xml` that line writes.

**Mirrors.** n8n's `ci-python.yml` (job `checks`) and `test-evals-python.yml`, in the survey of 100
repositories.

**Expected after step 1.1.** init leaves the pytest line (`status: left`): `--cov=src` writes a
report, which a skip wouldn't, and the upload after it would send nothing.

**Expected after step 1.5.** init wraps the line again, and predicts `ci: runs` with
`ciWhy: reports`, naming `codecov/codecov-action`: CI runs it every time, and laptops and agents skip
it on a pass.
