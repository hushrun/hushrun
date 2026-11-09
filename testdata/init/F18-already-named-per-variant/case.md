# F18: a check named per variant already

**Pattern.** A matrix job's line goes through hushrun already, named per variant as ADR 0044
advised: `hushrun run test-${{ strategy.job-index }} -- go test ./...`.

**Mirrors.** Repositories set up by hushrun 0.7.0 or earlier that followed its advice for matrices.

**Expected after Phase 3.** init leaves the line as it is and writes `HUSHRUN_VARIANT` in the job's
`env`: with both a name and the variable, the variable decides, and a variant's values don't move
when the matrix is reordered the way an index does. `variantIdentity: env`. Only the run can tell
the line's key, since CI fills in its name, so the plan predicts nothing for it.
