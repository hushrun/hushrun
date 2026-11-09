---
name: hushrun
description: Use before every git push in a repository that uses hushrun, when hushrun's pre-push hook refuses a push, when a line starts with "hushrun:", and when asked to set hushrun up. hushrun skips CI checks that this exact code already passed, so running them here first means CI doesn't run them again.
metadata:
  written-by: hushrun init
---

# hushrun

hushrun remembers which checks passed on which files. A pass here skips the same check in CI, on
any branch or commit with the same files. `hushrun init` wrote this file; run it again to update it.

In these commands, `hushrun` is `npx hushrun` where hushrun is a dev dependency (it is in
`package.json`'s `devDependencies`). `hushrun ready` prints each command in full, with the directory
it runs in: run it as printed, so that it is the check CI looks for.

## Before you push

1. Commit your work.
2. Run `hushrun ready`. It lists what CI would still run on this exact code, and the commands that
   run it here.
3. Run those commands, each on its own. If a check fails, fix the code, not the check.
4. Run `hushrun ready` again until it says the code is ready, then push.

`hushrun ready --format json` gives the same answer as data: `next` holds the commands to run, and
exit code 10 means they are still to run.

Each check is one of three:

- `passed`: it passed on this code, and CI skips it. `recordedBy` says who recorded it.
- `missing`: it hasn't, and the command it prints makes it pass here. With no command, it failed on
  this code and a run here wouldn't count: `why` says what clears it, a change to the code or a pass
  on a machine that meets the rule.
- `ci`: nothing here changes it, and CI or another machine has to run it. `why` says what it needs,
  such as `needs linux, this is macos`. `a run here didn't count: needs go 1.25, this is go1.24.3`
  means a run on this machine told it that, for this code, and that no result is recorded for it:
  running it again won't change anything. If the person expects it to pass here, tell them what it
  says. Once this machine is fixed, one run counts, and clears it.

`ready` says the code is ready when nothing is `missing`, so a `ci` check hasn't run. To know that
every check passed, use `hushrun ready --strict`, which exits 0 only then.

## When a push is refused

hushrun's pre-push hook refuses a coding agent's push while a check that can run here hasn't passed
on the code being pushed. It prints the same list and the same commands as `hushrun ready`. Run
them, then push again. The hook never runs checks itself, so a long check can't time out your push.

Don't push with `--no-verify` unless the person asks you to: CI would then run those checks again.

If a check can't run on this machine, for example because it needs a service this machine doesn't
have, tell the person what it needs, and don't work around it. They can push it themselves, or
install the hook with `hushrun hook install --warn`, which only warns. Leave both to them.

## Reporting back

Say which checks hushrun skipped because this code passed them before, and which `hushrun ready`
left to CI (`ci`).

## Reading its lines

- `hushrun: test skipped: this exact code passed …` is a pass, with exit code 0.
- `hushrun: test failed in 2s (exit 1)` is a real failure, with the check's own output above it.
- `… files changed while it ran, so it wasn't recorded …`: the check writes files that aren't
  ignored. Tell the person, and suggest adding them to `.gitignore`.
- `hushrun: test passed in 6s on linux, go1.25.14; it counts`: this machine meets what
  `counts_from` in `hushrun.yaml` asks of it, and the pass is recorded.
- `hushrun: test passed in 6s; it won't count: needs go 1.25, this is go1.24.3`: the command really
  did pass, and nothing was recorded, so CI runs it, and `hushrun ready` stops asking. The same line
  comes first, as `running: this run won't count: …`, before a long run: you can stop it there.
  Don't edit `counts_from`, and don't change the command, to get past it: it says where this
  repository's checks count. Tell the person what the line says.
- `hushrun: installed the pre-push hook in …`: this clone had none. There is nothing to do.
- `hushrun: sign in to share results with acme: npx hushrun login`: nobody has signed in here. Checks
  still run. Tell the person, who signs in once.
- `hushrun: results aren't shared with acme: …`: no credential reached the backend, as in a sandbox
  without `HUSHRUN_AGENT`. Checks still run. Tell the person, and don't try to sign in.
- `… not recorded: …`: the check ran, and its result isn't shared. `hushrun ready` stops asking for
  it, and CI runs it. Tell the person what the line says.

## Partial runs

`vitest run src/cart.test.ts` is a different check from `vitest run`. Use partial runs while you
work, then run the full check, the way `hushrun ready` names it.

## Bun

`bun test` is Bun's own runner. It never reads package.json, so it never goes through hushrun and
can't skip. Where the `test` script wraps it, run `bun run test`, the way `hushrun ready` names it.

## Setting hushrun up

When a person asks you to add hushrun to a repository, run `npx hushrun init --yes` at its root. It
prints a sign-in link and a code: show both to the person and wait, since only they can approve it.
Then show them the diff.

If they belong to several organizations and none is named like the repository's owner, init lists
them, changes nothing and exits with status 2. Ask which one, then run
`npx hushrun init --yes --org <slug>`.

git doesn't clone hooks. A package manager's install adds this one, and so does the first
`hushrun ready` or `hushrun run` in a clone, where hushrun is installed. After a person's
`hushrun hook uninstall`, neither does: only `hushrun hook install` brings it back, so leave that to
them.

In a sandbox with no browser, a person gives you one variable, `HUSHRUN_AGENT`. Some sandboxes have
`HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY` instead. Without any, checks still run but share nothing:
say so instead of trying to sign in, which `HUSHRUN_AGENT` refuses anyway.

## Never

- Commit a token (`hra_…`, `hru_…`), a `HUSHRUN_AGENT` string (`hushrun_agent_…`) or a machine key,
  or print one in a log or a pull request.
- Remove `hushrun run --` from a script to make a check run. `hushrun run --force NAME -- COMMAND`
  runs it once and records the result. Give the script's name and its exact command, so the result
  counts for the same check: `npx hushrun run --force test -- vitest run`.
- Edit the `"hushrun"` key in `package.json`, `hushrun.yaml`, a `HUSHRUN_*` variable, or
  `id-token: write` or a `hushrun run` in a CI file, unless the person asks.
- Run `hushrun login`, `hushrun logout` or `hushrun token` without being asked.
- Run `hushrun approve`. Approvals are a person's.

More: https://hushrun.com/docs/for-agents.md
