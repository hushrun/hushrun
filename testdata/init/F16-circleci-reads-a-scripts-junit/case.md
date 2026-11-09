# F16: CircleCI stores the test results a script writes

**Pattern.** `.circleci/config.yml` runs `npm test` and `store_test_results`, and package.json's
`test` writes JUnit through `jest-junit`.

**Mirrors.** Material UI and Nest, whose CircleCI jobs store test results.

**Expected after Phase 1.** init doesn't edit CircleCI's file, and a run there can't see the step
that reads what the script writes, so init leaves `test`, saying why.
