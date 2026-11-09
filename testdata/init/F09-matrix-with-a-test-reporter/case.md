# F09: a test reporter in a matrix job

**Pattern.** A Node matrix runs `npm test`, a wrapped script that writes JUnit, and
`dorny/test-reporter` reads `junit.xml`.

**Mirrors.** gemini-cli's CI.

**Expected after Phase 1.** init wraps the script. The job has several variants, which comes first:
the plan predicts `ci: runs` with `ciWhy: variants`, and lists `dorny/test-reporter` among the job's
`readers`, which keep CI running it once Phase 3 tells the variants apart.
