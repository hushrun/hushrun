# 0036. Agents are told before they push: `hushrun ready`, a pre-push hook and a skill

- **Status:** accepted; amended by [0051](0051-a-workspace-package-is-a-scope.md) on 2026-11-05:
  `ready` reads a JavaScript workspace's packages' scripts too
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-31
- **Rules:** C-22, C-23 and C-24 (new); C-14, C-15; §10.3
- **Amends:** [0018](0018-local-and-agent-evidence.md), whose only way to reach an agent was a
  snippet for `AGENTS.md`; [RFC 0002](../rfc/0002-checks-first.md) §3.8, which proposed
  `hushrun hook install` as a hook that runs what the code still needs

## Context

An agent that validates before it pushes saves CI's run. Nothing made it do so. A snippet for
`AGENTS.md`, which someone had to find and paste, was all there was: an agent that ignored it
pushed, CI ran everything, and nothing broke, so nobody noticed the saving that didn't happen.
`hushrun init` left nothing in the repository for an agent to read.

Tools that gate `git push` at a proxy have the opposite problem. They reach every agent, and tell
it what to do next, but they sit in the path of every push and must be the only route to the
host. hushrun's design is that a missing pass costs a run, never a stop (C-14, ADR 0032), and that
is worth keeping.

So an agent needs its next step from the push itself, without hushrun becoming something a push
depends on.

## Decision

1. **`hushrun ready` says what's left** (C-22): what CI would still run on this exact code, and
   the command that runs each check here, as text or `hushrun.ready/v1`. It exits 10 when a check
   is left to run, a new exit code, and 0 otherwise. It looks at the source checks of the stage the
   branch goes to, as `run --all` does, and at package.json's `hushrun run` scripts.

2. **A check is `missing` only if a command here can pass it.** What can't change from here is
   `ci`, and doesn't count against the code: a stage that takes CI's evidence only, a check
   without a `run` or with a secret, a pinned image with no Docker or Podman, a machine whose
   signature wouldn't count, results that stay on this machine, or a server that doesn't answer.
   So doing what `ready` says always ends, and it never asks for a run that would change nothing.

3. **A pass that can't be recorded leaves a mark on the machine** (C-22). The org's plan may refuse
   a record, and then no lookup can see the pass, so `ready` would ask for the same run for ever.
   That would break ADR 0032, under which a plan's limit stops nothing. The mark, an empty file
   under the cache directory named for the run's key, makes such a check `ci`.

4. **The hook refuses an agent and warns a person, by default** (C-23). A refused push is how an
   agent finds out: it needs no instructions, and it reads the next step from the push's output.
   A person's push goes on with the same list. `--refuse` and `--warn` make it everyone or no one.
   hushrun tells an agent by what the environment says: `HUSHRUN_INVOKED_BY`, an agent token, then
   the variables coding agents set. An agent it doesn't recognize is treated as a person, which
   costs what it costs today.

5. **The hook runs no checks.** RFC 0002 proposed a hook that runs them. A long check inside
   `git push` holds up a person, and is killed by an agent's tool timeout, with nothing recorded.
   Refusing returns at once, and the agent runs the command on its own, with its own timeout.

6. **The hook judges the commit it is sent, not the worktree.** git says which commits a push
   sends, on stdin. Evidence is about content, so a pass from before the commit counts, and an
   unrelated edit in the worktree doesn't. The branch is the remote's, so `HEAD:develop` is
   judged as `develop`.

7. **Only a known gap stops a push.** In the hook, a crash, a timeout of 15 seconds, a server that
   doesn't answer and a `hushrun.yaml` that doesn't parse all let it through. The lines it installs
   stop a push on exit code 10 alone, so a crash, a signal, or an older hushrun that doesn't know
   the command can't, and they stop it with `exit 1`, so that lines of a team's after them can't
   undo a refusal in a shell without `-e`. It isn't a gate, and it says so: `git push --no-verify` skips it, and pushes
   that don't use git, or come from a CI job, never reach it. CI runs whatever has no pass.

8. **`hushrun init` sets agents up** (C-24), so that nobody has to find a snippet. It writes a skill
   to `.claude/skills/hushrun/SKILL.md` and `.agents/skills/hushrun/SKILL.md`, adds a block to
   `AGENTS.md`, and installs the hook. Those two paths reach Claude Code, Codex, OpenCode and Cursor,
   each by its own documentation of 2026-10-31. Every file is in the repository, so every clone and
   sandbox has it. `recipes/agents/` is the one source of their text, built into the binary.

9. **A script puts the hook in each clone.** git doesn't clone hooks, and an agent's sandbox is
   usually a fresh clone. npm (`install` and `ci`), pnpm, Bun and Yarn 1 run `prepare` after an
   install, so a sandbox that runs `npm ci` has the hook; Yarn from 2 on doesn't, and runs
   `postinstall`, so init writes that one there. All of them were tried on 2026-10-31, each with a
   fresh repository. `|| exit 0` keeps an install from failing where hushrun isn't there, such as
   with `--omit=dev`. A repository without package.json installs it with `hushrun hook install`,
   which the skill says to do. \[ADR 0043\] There, the first `hushrun ready` or `hushrun run` now
   does.

10. **A hook of someone else's stays theirs.** `.husky/pre-push` is shell lines a team keeps, so
    hushrun adds to it, and goes where the file says. A hook of git's that isn't hushrun's may be any
    language, and lefthook's YAML has comments to keep, so for those hushrun prints what to add.

## Consequences

- An agent that does what a refused push says skips CI's run, with no instructions at all, in a
  repository `hushrun init` set up.
- An agent can still skip it: `--no-verify`, or a push that doesn't use git, or an agent that sets
  none of the variables. The skill says not to, and CI still runs everything. This closes the
  common case, not every case, and the docs say so.
- A person sees a few more lines on a push that CI will repeat, and none otherwise but one that
  says the code is ready.
- A check that an agent can't run here, say one that needs a service, refuses its push until a
  person says otherwise. A team can install with `--warn`.
- Repositories that `hushrun init` set up before this get all of it by running init again.

## Rejected alternatives

- **A gate on the server, or a git proxy.** It would really stop what hushrun can only discourage,
  and every push would then depend on hushrun. Its value here is lower: a push without evidence
  is safe, and costs only a CI run.
- **Refusing everyone by default.** A person with a reason to push gets in the way of a tool that
  saves them nothing they asked for. People can choose it with `--refuse`.
- **One copy of the skill.** Claude Code reads only `.claude/skills`, and Codex only
  `.agents/skills`. A symbolic link breaks where git has `core.symlinks` off.
- **Reading the failure from the backend for a quota refusal.** It needs a new answer from every
  server, and the mark needs none.
