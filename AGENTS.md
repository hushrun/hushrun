<!-- hushrun:start (hushrun init writes this block; run it again to update it) -->
## Before you push: hushrun

This repository's checks run through hushrun, which skips a check when this exact code already
passed it: on this machine, and in CI once the repository shares its results. Commit, then run
`hushrun ready` (`npx hushrun ready` where hushrun is a dev dependency). Run the commands it
prints, fix what fails, and push once it says the code is ready.

The pre-push hook refuses a coding agent's push until then, and `hushrun ready` installs it in a
clone that has none. Don't use `--no-verify` unless a person asks you to. If a check can't run on
this machine, tell the person what it needs. When you report back, say which checks hushrun skipped
and which `hushrun ready` said CI still runs. Before you finish, run `hushrun flush`: it sends any
result that waited on this machine while the backend was out of reach. More:
https://hushrun.com/docs/for-agents.md
<!-- hushrun:end -->

## Why CI runs every test, and how to keep yours from it

CI's `test` and `test-macos` reuse whole Rust test binaries: one that already passed on the files
it read doesn't run again. Every integration test in `crates/hushrun/tests/` starts the `hushrun`
binary, so it reads all of `crates/hushrun/src/`: a pull request that changes any file there runs
all 108 test binaries on every push, about 45 minutes on macOS. That is reuse working, not broken.
Before you push such a change:

- Push once, when `hushrun ready` says the code is ready. Each push cancels the running CI, and a
  cancelled run keeps nothing, so a follow-up push for a typo or the CHANGELOG starts it over.
- Don't merge `main` in unless GitHub says the branch conflicts: CI already tests the merge.
- Put docs, ADR and spec changes that stand alone in their own pull request: alone, they run about
  3 binaries.
- When you report back, quote hushrun's line before the tests, such as `3 of 107 test binaries
  run` or `all 108 test binaries run`.

The table of what runs for which change: [CONTRIBUTING.md](CONTRIBUTING.md#which-tests-ci-runs-again).
