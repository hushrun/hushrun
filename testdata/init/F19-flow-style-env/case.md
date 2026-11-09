# F19: a matrix job whose env is on one line

**Pattern.** A matrix job writes its `env` in flow style, `env: {CGO_ENABLED: "0"}`, and runs
`go test ./...`.

**Mirrors.** The edit fallback: init edits lines, and a one-line map has no line of its own for a
key.

**Expected after Phase 3.** init wraps the line but doesn't edit the `env`. Its notes give the line
to add: `its env is written on one line: add HUSHRUN_VARIANT: ${{ toJSON(matrix) }} to it`. Until
then CI runs the check in every variant: `variantIdentity: none`, `ci: runs` with
`ciWhy: variants`, in the run's own words.
