# F02: a Node and OS matrix running a wrapped script

**Pattern.** A job with a matrix over operating systems and Node versions runs `npm test`, and
package.json's `test` is `vitest run`.

**Mirrors.** Many of the survey's JavaScript repositories: 263 of its 823 test jobs have several
variants.

**Expected today (Phase 0).** init wraps the script (`hushrun run -- vitest run`). The job has
several variants and no identity, so CI runs the check in every variant: `ci: runs` with
`ciWhy: variants`, in the run's own words. Laptops and agents still skip it on a pass.

**Expected after Phase 3.** `HUSHRUN_VARIANT: ${{ toJSON(matrix) }}` in the job's `env`,
`variantIdentity: env`, and `ci: skips`, each variant on its own pass.
