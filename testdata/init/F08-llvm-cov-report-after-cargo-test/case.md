# F08: cargo llvm-cov around cargo test, then an upload

**Pattern.** `cargo llvm-cov show-env` sets up coverage, `cargo test` runs the tests, `cargo llvm-cov
report --lcov` writes the report, and `codecov/codecov-action` uploads it.

**Mirrors.** polars' `test-coverage.yml` (jobs `coverage-rust` and `coverage-python`), two of the
survey's six report jobs.

**Expected after Phase 1.** init wraps `cargo test --workspace`, and predicts `ci: runs` with
`ciWhy: reports`: the first step that reads its reports is the `run:` step of `cargo llvm-cov
report`, which a skip would leave nothing to read.
