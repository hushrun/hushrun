# 0161. A session start that warms the sandbox and tells the agent what passed

- **Status:** proposed; built
- **Date:** 2026-11-27
- **Rules:** C-168 new; C-145 changed; relies on B-240, C-140, C-144, C-22 and C-143
- **Amends:** [0143](0143-a-baked-store-pulled-into-a-fixed-sandbox.md)
- **Amended by:** [0163](0163-a-package-store-pulled-into-a-sandbox.md) (`packages pull` between
  the two steps)

## Context

`hushrun init --agent-clouds claude-code` wrote a `SessionStart` hook into `.claude/settings.json`
that ran `[ "$CLAUDE_CODE_REMOTE" != true ] || hushrun build pull -q || true` (ADR 0143). It had
three gaps:

- **Only the compiled code arrived.** The newest results (`hushrun fetch`, C-144), which let a
  session skip what CI already passed while the backend is out of reach, waited for the agent's
  first `hushrun run` or `hushrun ready`.
- **Claude learned nothing at the start.** Claude Code adds a `SessionStart` hook's stdout to the
  model's context. The hook printed nothing there, so the agent found out what this code already
  passed only once it ran `hushrun ready`, if it ran it.
- **Every change needed init again.** The hook's text was written into each repository. A better
  session start, such as a future `packages pull`, would have needed every repository to run init
  again and commit the result.

## Decision

1. **One command, `hushrun hook session-start`.** The hook calls this command and nothing else, so
   later improvements ship in the binary. It lives under `hook`, beside `hook pre-push`: both are
   commands a hook runs, not commands a person types. Unlike `pre-push`, it isn't hidden, since
   other platforms' start hooks and setup steps can call it too.
2. **It acts only in a hosted agent's sandbox,** as C-140's table decides: `hushrun_ci::sandbox`,
   limited to platform rows (Claude Code on the web, Cursor's and Copilot's cloud agents, Codex
   cloud), with `HUSHRUN_SANDBOX` and `HUSHRUN_PROVIDER` honored as everywhere else. A coding agent
   on someone's laptop, or in a CI job, gets nothing from it. It doesn't read `CLAUDE_CODE_REMOTE`
   itself. Elsewhere it prints nothing and exits 0.
3. **It never fails a session.** It always exits 0, catches a panic, and has a budget, `--budget
   SECONDS`, default 45 s. Claude Code gives a command hook 60 s unless the hook sets `timeout`,
   so init sets `timeout: 60` (C-145). That leaves room for `npx` to start hushrun. Of the budget,
   a third, at most 15 s, is kept for the context. The steps get the rest.
4. **The steps, in order:** `hushrun build pull` (B-240), then `hushrun fetch` (C-144). A
   `hushrun packages pull` goes between them once it exists. Each step runs as a child process of
   the same hushrun binary, with its output in `<cache dir>/session-start.log` and never on
   stdout. `fetch` is skipped without a credential for the backend, by the same selection `hushrun
   run` makes (C-11, C-13): `HUSHRUN_AGENT`, `HUSHRUN_AUTH=proxy`, a session, or any token that
   goes there. `build pull` runs without one, since a public repository's registry needs none. A
   step that fails or can't reach the network is a word in the context, and the next step still
   runs.
5. **A step the budget cuts off goes on in the background.** It is detached in its own process
   group, so the hook's end doesn't stop it, and the steps after it are skipped. The pull swaps a
   store into place only once it checks out (B-240), so a pull that finishes after the session's
   first command does no harm. Killing it would have meant a large store never arrives: each
   session would cut it off at the same point.
6. **Then the context, on stdout, in plain text,** from `hushrun ready`'s own evaluation of HEAD
   (C-22). It is at most 10 lines: what the steps did; how many checks this exact code already
   passed, and whether CI skips them; which checks are left to run here; which are left to CI;
   whether results stay on this machine (no credential, or a lookup the backend refused or didn't
   answer: `hushrun doctor`); and the instruction to run what `hushrun ready` lists before pushing.
   It names steps and checks only: never a token, an agent string or another value of a variable.
   If `ready` doesn't answer before the budget ends, a watchdog prints a reduced block that says
   so and ends the command.
7. **Init replaces the old hook.** When `.claude/settings.json` holds the exact `build pull`
   command an earlier init wrote, init puts the new hook in its place, with `timeout`. Any other
   copy of the old hook is taken out, along with an entry that is left with no hook. Every other
   entry and field stays.

### Plain stdout, not JSON

Claude Code takes either plain stdout or JSON with
`hookSpecificOutput.additionalContext` from a `SessionStart` hook. This uses plain text:

- **It works on more platforms.** Another platform's start hook, a setup script, or a person
  running the command by hand reads the same lines. JSON would make the command one platform's.
- **It needs no escaping.** A check's name with quotes or newlines can't break the format the
  agent gets.
- **It costs nothing.** For a `SessionStart` hook, both land in the same place in the model's
  context.

If a platform needs JSON later, a `--format json` can produce it. The text stays as the default.

## Consequences

- A Claude Code on the web session starts with CI's compiled code and the newest results, and
  the agent knows from its first turn what to run before pushing.
- Repositories that ran an earlier init keep the old hook until they run `hushrun init
  --agent-clouds claude-code` again. The old hook still works.
- `ready` hashes the tree in the hook. On a large repository that can take longer than the time
  kept for it. The block then says so, and the agent runs `hushrun ready` itself.
- A pull still running in the background competes with the session's first build for the
  network.
- A process-wide flag in the results client records a lookup the backend refused (401, 403,
  407), failed (5xx, 408) or didn't answer, for the line that points at `hushrun doctor`. It
  can't tell a proxy's refusal from the backend's. `hushrun doctor` can.

## Alternatives considered

- **More shell in the hook** (`build pull; fetch; ready`). Every change would need init again, it
  couldn't share a budget, and `ready`'s output is meant for a person, not 10 lines of context.
- **Running `hushrun ready --format json` as a child.** That would install the pre-push hook and
  add the skill's notes, which are side effects a session start shouldn't have. It would also
  hash the tree in a process the budget can only kill.
- **Killing a step at the deadline.** See decision 5.
- **A hard-coded `CLAUDE_CODE_REMOTE` check in the command.** Other platforms' start hooks
  couldn't reuse it. The shell gate stays in the hook only so that `npx` isn't started on a
  laptop.
