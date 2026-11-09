# F17: a matrix an earlier job builds

**Pattern.** A `setup` job writes the matrix as an output, and `test` reads it with
`matrix: ${{ fromJSON(needs.setup.outputs.matrix) }}`, then runs `python -m pytest`.

**Mirrors.** The survey's 43 matrices built by an expression, which no file says the values of.

**Expected after Phase 3.** init wraps the line and writes `HUSHRUN_VARIANT: ${{ toJSON(matrix) }}`
in `test`'s `env`: the value is the variant as it ran, so it works whatever built the matrix.
`variants: several`, `variantIdentity: env`, and `ci: skips`, each variant on its own pass.
