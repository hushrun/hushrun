# F14: a linter and the tests, in one job with a Codecov upload

**Pattern.** `ruff check .` and `pytest --cov=src --cov-report=xml`, then `codecov/codecov-action`.

**Mirrors.** Many repositories that lint and test in one job.

**Expected after Phase 1.** init wraps both. The linter predicts `ci: skips`: Codecov reads only
coverage, which its skip leaves as it was. The tests predict `ci: runs` with `ciWhy: reports`.
