# hushrun guide

What a coding agent needs to know about this hushrun, beyond its skill. `hushrun guide` prints it
from the hushrun installed here, so it always says what that hushrun does. Its last section says
what is true on this machine.

## Each check is one of three

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

## The pre-push hook

The hook never runs checks itself, so a long check can't time out your push. It refuses a coding
agent's push and warns a person. If a check can't run on this machine, for example because it
needs a service this machine doesn't have, the person can push it themselves, or install the hook
with `hushrun hook install --warn`, which only warns. Leave both to them.

git doesn't clone hooks. A package manager's install adds this one, and so does the first
`hushrun ready` or `hushrun run` in a clone, where hushrun is installed. After a person's
`hushrun hook uninstall`, neither does: only `hushrun hook install` brings it back, so leave that to
them.

## Reading hushrun's lines

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
- `hushrun: hushrun 0.12.0 is available, and this is 0.11.0: …`: a newer release is out. Once the
  task is done, tell the person, with the command the line gives.

## Partial runs

`vitest run src/cart.test.ts` is a different check from `vitest run`. Use partial runs while you
work, then run the full check, the way `hushrun ready` names it.

## Running a check again

To see a skipped check's output, run it again with `--force`, which runs it once and records the
result. Give the script's name and its exact command, so that the result counts for the same
check: `npx hushrun run --force test -- vitest run`.

## Bun

`bun test` is Bun's own runner. It never reads package.json, so it never goes through hushrun and
can't skip. Where the `test` script wraps it, run `bun run test`, the way `hushrun ready` names it.

## Setting hushrun up

`npx hushrun init --yes` applies init's changes without asking. Without `--yes`, and without a
terminal, init prints its plan and exits with status 2. `npx hushrun init --dry-run` shows the plan
and changes nothing.

If the person belongs to several organizations and none is named like the repository's owner,
init lists them, changes nothing and exits with status 2. Ask which one, then run
`npx hushrun init --yes --org <slug>`.

In a sandbox with no browser, a person gives you one variable, `HUSHRUN_AGENT`. Some sandboxes have
`HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY` instead. Without any, checks still run but share nothing:
say so instead of trying to sign in, which `HUSHRUN_AGENT` refuses anyway.

## Keeping hushrun and its skill current

- **hushrun.** At most once a day, hushrun says when a newer release is out. Where a package
  manager installed it, the package is updated with it, such as `npm install hushrun@latest`;
  else `hushrun update` updates it. `HUSHRUN_NO_UPDATE_CHECK=1` turns the check off.
- **This repository's skill.** `hushrun init` writes it to `.claude/skills/hushrun/SKILL.md` and
  `.agents/skills/hushrun/SKILL.md`, and `hushrun skill update` updates both, then they are
  committed. Each copy says its revision, `hushrun-skill` in its frontmatter. A copy edited by
  hand, or one a newer hushrun wrote, is left as it is, unless the person asks for `--force`.
- **Your own copy.** `hushrun skill update --global` writes `~/.claude/skills/hushrun/SKILL.md`
  and `~/.agents/skills/hushrun/SKILL.md`, for repositories that have none. A copy from a skills
  marketplace is updated by that marketplace's own command.

`hushrun guide --format json` gives this as data: the guide, this hushrun's version and skill
revision, `update` when a newer release is known, and each copy of the skill with its state:
`current`, `older`, `newer`, `edited` or `absent`.
