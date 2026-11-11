---
name: hushrun
description: Use before every git push in a repository that uses hushrun, when hushrun's pre-push hook refuses a push, when a line starts with "hushrun:", and when asked to set hushrun up. hushrun skips a check that this exact code already passed, here and, once the repository shares its results, in CI, so running checks here first means CI doesn't run them again.
compatibility: hushrun 0.11.0 or newer for `hushrun guide`; everything else here works with any hushrun.
metadata:
  hushrun-skill: "2"
---

# hushrun

hushrun remembers which checks passed on which files. A pass here skips the same check here, on
any branch or commit with the same files, and in CI too once the repository shares its results.

This skill holds what every hushrun does the same way. **Run `hushrun guide` once per session.**
It prints the rest for the hushrun installed here, such as what each of its lines means, and says
whether hushrun and this skill are current. If `guide` is an unknown command, this hushrun is
older than 0.11.0: follow this skill, and tell the person, who updates it.

In these commands, `hushrun` is `npx hushrun` where hushrun is a dev dependency (it is in
`package.json`'s `devDependencies`).

## Is hushrun here?

A repository uses hushrun when its `package.json` has scripts that start with `hushrun run`, or a
`"hushrun"` key that names a backend; in another language, when it has a `hushrun.yaml`, or its
CI's files run checks as `hushrun run …`.

## Before you push

1. Commit your work.
2. Run `hushrun ready`. It lists the checks this exact code hasn't passed yet, which CI would
   still run, and the commands that run them here, each in full, with the directory each runs in.
3. Run those commands as printed, each on its own, so that each is the check CI looks for. If a
   check fails, fix the code, not the check.
4. Run `hushrun ready` again until it says the code is ready, then push.

`hushrun ready --format json` gives the same answer as data: `next` holds the commands to run, and
exit code 10 means they are still to run. A check in state `ci` is one nothing here changes: CI or
another machine runs it, and `why` says what it needs.

## Where results stay on this machine

Results are shared only where hushrun has a backend and a sign-in or a token for it. Anywhere else
they stay on this machine: a check that passed here skips here, and CI runs every check.
`hushrun ready` says when that is so. The steps above are the same. Sharing is the person's to
decide: tell them what `hushrun ready` said, and don't sign in or run `hushrun init` for it unless
they ask.

## When a push is refused

hushrun's pre-push hook refuses a coding agent's push while a check that can run here hasn't
passed on the code being pushed. It prints the same list and the same commands as `hushrun
ready`: run them, then push again. Don't push with `--no-verify` unless the person asks you to.
If a check can't run on this machine, tell the person what it needs, and don't work around it.

## When something is out of date

hushrun says so in a line that starts with `hushrun:`, or in `hushrun ready`'s notes:

- a newer release of hushrun is out;
- this repository's copy of this skill is older than this hushrun's, and `hushrun skill update`
  updates it;
- a newer hushrun wrote this repository's copy, so this hushrun is the one to update.

Finish the task first. Then tell the person what hushrun said, with the command it gave. Don't
update hushrun, or rewrite this skill, in the middle of a task unless the person asks: the package
is the repository's, the binary is theirs, and an update changes the tool you are using.

## Reporting back

Say which checks hushrun skipped because this code passed them before, which `hushrun ready` left
to CI (`ci`), whether it said results stay on this machine, and anything hushrun said is out of
date.

## Setting hushrun up

When a person asks you to add hushrun to a repository, run `npx hushrun init --yes` at its root.
It prints a sign-in link and a code: show both to the person and wait, since only they can approve
it. Then show them the diff. `hushrun guide` covers the rest, such as setting up with no account,
a person in several organizations, and sandboxes with no browser.

## Never

- Commit a token (`hra_…`, `hru_…`), a `HUSHRUN_AGENT` string (`hushrun_agent_…`) or a machine key,
  or print one in a log or a pull request.
- Remove `hushrun run --` from a script to make a check run.
- Edit the `"hushrun"` key in `package.json`, `hushrun.yaml`, a `HUSHRUN_*` variable, or
  `id-token: write` or a `hushrun run` in a CI file, unless the person asks.
- Run `hushrun login`, `hushrun logout` or `hushrun token` without being asked.

More: https://hushrun.com/docs/for-agents.md
