<!-- hushrun:start (hushrun init writes this block; run it again to update it) -->
## Before you push: hushrun

This repository's checks run through hushrun, which skips a check in CI when this exact code
already passed it. Commit, then run `hushrun ready` (`npx hushrun ready` where hushrun is a dev
dependency). Run the commands it prints, fix what fails, and push once it says the code is ready.

The pre-push hook refuses a coding agent's push until then, and `hushrun ready` installs it in a
clone that has none. Don't use `--no-verify` unless a person asks you to. If a check can't run on
this machine, tell the person what it needs. When you report back, say which checks hushrun skipped
and which `hushrun ready` left to CI. More:
https://hushrun.com/docs/for-agents.md
<!-- hushrun:end -->
