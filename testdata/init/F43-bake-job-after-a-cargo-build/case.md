# F43: the bake job, after a cargo build on GitHub

**Pattern.** One workflow builds and tests a crate on pushes to main and on pull requests.

**Expected (B-69).** With `--bake`, init writes `.github/workflows/hushrun-bake.yml`, a workflow of
its own that runs after `CI` passes on main and by hand: it bakes the backend's trusted build into
`ghcr.io/acme/shop`, with `packages: write` and `id-token: write`, and the plan says the Dockerfile
line and the `build: baked: signers:` lines for hushrun.yaml.
