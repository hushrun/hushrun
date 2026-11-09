# F28: a test script in a job named for its tests

**Pattern.** Job `test` runs `./scripts/setup-db.sh`, then `./scripts/test.sh`; job `server`,
named `Server tests`, runs `./runtest --verbose`; job `build` runs `bash scripts/ci/test.sh`.

**Mirrors.** VS Code's `./scripts/test.sh` and Redis's `./runtest`: the survey found test scripts
like these among the 219 commands init doesn't recognize.

**Expected after Phase 4.** init wraps none of them, since a script can deploy or write reports,
and lists two in `suggestions`, each with the line to use: `hushrun run test -- ./scripts/test.sh`
in job `test`, and `hushrun run runtest -- ./runtest --verbose` in job `server`, whose name holds
"tests". Neither job installs hushrun, so `setup` is `none`. `./scripts/setup-db.sh` isn't one,
since its name doesn't say it tests, and job `build`'s line isn't one, since neither its id nor a
name says it runs tests. `jobs` stays empty: init wraps and leaves nothing it knows. The text lists
the two under "Could also wrap".
