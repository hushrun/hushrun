# 0043. A clone gets the hook from what runs in it, and init puts hushrun on PATH

- **Status:** accepted; amended by [0061](0061-local-state-changes-only-when-asked.md) on 2026-11-07:
  `hushrun init` isn't someone asking for the hook, so it leaves an uninstall as it is (item 5)
- **Date:** 2026-11-01
- **Rules:** C-15, C-20, C-23, C-24
- **Amends:** [0036](0036-agents-told-before-they-push.md), whose only way into a clone was a
  package manager's script; [0037](0037-any-language-from-cis-own-files.md) item 11, which left
  every clone without package.json to run `hushrun hook install` by hand

## Context

We ran `npx hushrun init` on a real Go backend. It worked, and its last lines gave a person two more
jobs:

```text
Install hushrun where you push from, so that the hook runs:
  curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh | sh
git doesn't clone hooks, and this repository has no package.json to install one: each clone, an agent's sandbox included, runs hushrun hook install once.
```

Both were real, and the first was a trap. The hook was written, and `command -v hushrun` found
nothing: npx's copy is on `PATH` only while npx runs, so on the machine that had just run init the
hook did nothing, with no word about it. In JavaScript the package manager covers all of this, and
a repository in any other language has no such step.

Nothing runs when git clones, and that won't change. But the people and agents the hook is for do
run hushrun in a clone, and hushrun can act then.

## Decision

1. **`hushrun ready` and `hushrun run` install a missing hook** (C-23), and no other command does,
   `hushrun hook pre-push` least of all. They do it only when all of these hold: this isn't CI; the
   repository is set up for hushrun, by `hushrun.yaml` or a package.json that names it; there is no
   `.husky` or lefthook file; git's configuration sets neither `core.hooksPath` nor `hushrun.hook`;
   and nothing has the hook's name.

2. **It only ever adds a file where there is none.** So a hook of someone else's stays, and so does
   hushrun's own with `--refuse` or `--warn`. A hook the repository tracks is left alone because a
   change to it is a change to the code, which would change the digest of what is being checked.
   `core.hooksPath` set means the same thing, or means a choice this has no business second-guessing.

3. **The file appears whole.** `setup::write` renames first and marks the file executable after, so
   a crash between the two leaves a hook that git won't run and that nothing would ever repair,
   and it replaces what is there. `hushrun run` starts in parallel from wrapped scripts, so the
   hook is written beside its final name, made executable, and moved there without replacing
   anything. One run wins and says so; the others say nothing.

4. **It can't fail or slow a command.** Every error goes to the debug log, so a read-only `.git` or
   a sandbox behaves as if nothing was tried. In a clone with the hook, the work is one stat. git
   starts only when there is no hook, in a repository set up for hushrun. One line goes to stderr
   when it installs, and nothing to stdout, so `ready --format json` is unchanged.

5. **An uninstall holds.** `hushrun hook uninstall` sets `hushrun.hook` to `off` in the clone's git
   configuration: it stays in that clone, and it isn't part of the code. Only someone asking for
   the hook clears it, with `hushrun hook install` or `hushrun init`. A package manager's `prepare`
   isn't someone asking, so it now leaves such a clone alone, which is a change: an uninstall in a
   package.json repository used to last until the next `npm install`.

6. **init installs hushrun when npx brought it** (C-24), through its plan, so that it is seen first
   and `--dry-run` writes nothing. That needs all of these: no package.json, whose dependency the
   hook would run; hushrun in a `node_modules` directory, so a binary someone built or copied isn't
   replaced by a download they didn't ask for; none on `PATH`; not CI; not Windows; and no
   installer's receipt naming a binary that is still there.

7. **The release is downloaded, not copied from npx.** `hushrun update` touches only a binary an
   installer's receipt names, so a copy of `current_exe` without one is stranded, and with one is a
   receipt vouching for a file nobody checked. So init uses update's path: the release's archive,
   its published SHA-256, `tar`, and `--version`. Then a copy made by init and a copy made by the
   installer are the same bytes. It leaves a receipt with `install_layout` `flat`, which update
   already reads, and `provider.source` `hushrun init`, so it can be told apart.

8. **Only directories that are the person's own, already on `PATH`.** In order: `$CARGO_HOME/bin`,
   `~/.cargo/bin`, `$XDG_BIN_HOME`, `~/.local/bin`, `~/bin`, `$GOBIN`, `$GOPATH/bin`, `~/go/bin`.
   The installer's directory comes first, so that a later run of the installer lands on the same
   file. It is a list and not "whatever on `PATH` is writable" because a version manager's
   directory is writable too, and changes under the file. Nothing edits a shell's startup file:
   that is the installer's to offer. A directory on `PATH` that isn't there yet is made.

9. **A failed install is never a failed init.** No directory fits, or the download or a check
   fails: init finishes everything else, warns, and prints the installer's command, as it did
   before.

## Rejected alternatives

- **A global `core.hooksPath` or `init.templateDir`.** Each reaches every future clone with no
  step at all, and changes git's behaviour for every repository on the machine, which is more than
  a check-skipping tool should take on. The first also turns off hooks in `.git/hooks` everywhere.
- **The hook falling back to `npx hushrun`.** It needs Node in every push of a repository that may
  have nothing to do with it, and the network whenever npx's cache was cleared, inside the 15
  seconds a push may wait.
- **A file in `.git` for the opt-out.** A stat is cheaper than starting git, but the case is rare,
  and git's configuration can be read and changed with the tools a person already has.
- **`hushrun doctor` reporting the hook, and nothing else.** It leaves a person with a step, and
  agents never run it.
- **Installing into any writable directory on `PATH`.** See item 8.

## Consequences

- In a repository without package.json, `AGENTS.md`, the skill and init no longer ask anyone to run
  `hushrun hook install`. The first `hushrun ready` does it, which is what those files tell an agent
  to run anyway.
- **What this doesn't do:** a clone where nobody has run hushrun has no hook, and neither does a
  machine without hushrun. The hook still can't reach someone who ignores hushrun altogether; CI
  still runs whatever has no pass.
- A hook written under `npx hushrun ready`, on a machine with no hushrun on `PATH`, does nothing
  until hushrun is installed. Nothing goes wrong, and the line it prints doesn't say so.
- Someone who ran `hushrun hook uninstall` on a version before this has no `hushrun.hook` set, and
  gets the hook back once. A second uninstall holds.
- A linked worktree's `.git` is a file, so there the first stat finds nothing, and each run starts
  git twice. That costs a few milliseconds beside a run that starts git anyway.
- `core.hooksPath` pointing somewhere stale is left as it is, like any other value of it.
- A receipt now has two writers. Should dist's format change, `install_of`'s tests and these would
  disagree, which is the point of them.
- On Windows nothing changes but the command init prints, which is now PowerShell's. No test runs
  there, so init doesn't offer to install.
