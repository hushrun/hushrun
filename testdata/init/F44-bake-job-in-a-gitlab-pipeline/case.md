# F44: the bake job, appended to a GitLab pipeline

**Pattern.** A pipeline with a build stage and a test stage for a crate.

**Expected (B-69).** With `--bake`, init appends a `hushrun-bake` job in the `.post` stage, on pushes
to the default branch and pipelines started from the web, with an ID token for the backend and one
for Sigstore, baking into `$CI_REGISTRY_IMAGE`; and it vendors `ci/install-hushrun.sh`, which the
job installs hushrun with.
