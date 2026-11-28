# 0165. A session start in Antigravity's format, read from stdin, once per commit; and `hook stop`

- **Status:** proposed; built (the CLI). `hushrun init` doesn't write Antigravity's hooks yet.
- **Date:** 2026-11-28
- **Rules:** C-168 changed; C-169 new; relies on C-140, C-143 and B-240
- **Amends:** [0161](0161-a-session-start-that-warms-and-tells.md) (as amended by
  [0163](0163-a-package-store-pulled-into-a-sandbox.md)) and
  [0143](0143-a-baked-store-pulled-into-a-fixed-sandbox.md)

## Context

Google Antigravity's Remote Agent runs hooks that `.agents/hooks.json` names, by event:
`SessionStart` when a conversation starts, and `Stop` each time the agent's loop ends a turn. The
same JSON reaches each hook on stdin: `conversationId`, `workspacePaths`, `transcriptPath`,
`lastUserInput` (the person's prompt), and on `Stop`, `fullyIdle`. A hook's default timeout is
30 seconds.

`hushrun hook session-start` (ADR 0161) was built for Claude Code on the web, and had four gaps
there:

- **Context reaches Antigravity's model only as JSON on stdout:** `{"injectSteps":
  [{"systemMessage": {"systemMessage": "…"}}]}`. Plain text, which Claude Code adds to the context,
  goes nowhere. A `Stop` hook's stdout is parsed too, so `hushrun flush -q`'s lines would be read
  as a broken answer.
- **It never read stdin.** A hook started from a folder outside the checkout found no repository,
  and a payload larger than a pipe holds could block the writer.
- **Every conversation runs the hook.** A subagent is a conversation of its own, and so is a
  resumed one. Each would pull again, and two at once would pull into the same folder.
- **`Stop` fires every turn.** A flush at each one sends nothing new most of the time.

## Decision

1. **`--format antigravity`.** `hushrun hook session-start --format antigravity` prints
   `{"injectSteps":[{"systemMessage":{"systemMessage":"<the block>","metadata":{"source":"hushrun"}}}]}`
   on one line, serialized with `serde_json`, so a check's name can't break it. The block is
   ADR 0161's lines, unchanged. The watchdog's reduced block goes through the same formatter.
   Where the command has nothing to say (outside a hosted sandbox, outside a repository, or after
   a panic) it prints `{}`. It is a value of the global `--format`, hidden from its list. No other
   command takes it (a usage error, exit 2), and `HUSHRUN_FORMAT` can't set it.
2. **The format isn't chosen from the platform.** A person who runs the command by hand in a
   sandbox C-140 calls Antigravity's still gets text. Plain text stays the default, so Claude
   Code's hook is unchanged.
3. **Stdin is read in both formats,** at most 1 MiB, for at most 0.5 s, and not at all from a
   terminal. A payload past the cap is dropped whole. What follows is read and thrown away in the
   background, so the writer never waits on a full pipe. Only two fields are kept: the workspace
   (`workspacePaths[0]`, else Claude Code's `cwd`) and `fullyIdle`. The payload is never logged,
   stored or printed: it holds the person's prompt. When the folder the hook starts in isn't a
   repository and `-C` names none, the command moves into the workspace's folder when that is
   one, and the steps, its children, inherit it.
4. **The budget stays 45 s.** Antigravity's default timeout is 30 s, so the hook init writes for
   it sets `"timeout": 60`, as Claude Code's does (C-145).
5. **A stamp: one set of steps per commit, for 10 minutes.** After the steps,
   `<cache dir>/session-start.stamp` records the repository's root, HEAD, HEAD's tree, each step's
   outcome (`none published yet` included), what `ready` said, and the time. A start in the same
   root on the same HEAD and tree, within 10 minutes, skips the steps and prints the block the
   stamp holds, without hashing the code again. A stamp with a step left running doesn't count:
   the step may have ended since, and the pulls are no-ops once the destination holds what's
   published. Ten minutes covers a subagent's start and a quick resume. Past it, the results on
   the backend may have moved.
6. **A lock, held by a step left running.** The steps run under an exclusive lock on
   `<cache dir>/session-start.lock`. Each step gets the lock's file as its stdin, so a step the
   budget cuts off holds the lock in its own process group until it exits, after the hook ends.
   The lock belongs to the open file, and the step's copy keeps it open. The command never unlocks
   by hand, since that would free it for the step too. A start that can't take the lock starts no
   step. It says the steps of the start that holds it, from that start's stamp when one left a
   step running, or else each step as `still running in the background`. Then it says what
   `ready` knows, as usual. The command's own copy goes once the steps end, so a start soon after
   isn't held for the length of `ready`'s evaluation. On Windows a step doesn't keep the lock
   after the command ends.
7. **Steps already done by a setup script cost little.** Both pulls do nothing when the
   destination holds the published identity. The steps inherit the hook's environment as it is,
   `HUSHRUN_BUILD_LOWER` and `HUSHRUN_PACKAGES_LOWER` included, so they look where the setup
   script put the stores. `fetch`'s kept answers (C-144) stand in for lookups the backend doesn't
   answer.
8. **`hushrun hook stop` (C-169)**, hidden, like `hook pre-push`. It reads stdin the same way. It
   flushes what waits in the outbox (C-143) only when `fullyIdle` is true or absent, so a turn
   that isn't the last sends nothing. Its own lines go to stderr. On stdout it prints `{}` with
   `--format antigravity` and nothing otherwise. It never prints a `decision`, since `continue`
   would keep the agent's loop going. It always exits 0. Claude Code's `Stop` hook (`hushrun
   flush -q`) is unchanged.

### What init writes for Antigravity (later)

Init doesn't write `.agents/hooks.json` yet. When it does, its hooks run:

- `SessionStart`: `hushrun hook session-start --format antigravity || echo '{}'`, with
  `"timeout": 60`;
- `Stop`: `hushrun hook stop --format antigravity || echo '{}'`, with `"timeout": 30`.

The `|| echo '{}'` only matters when hushrun can't start: the commands themselves always exit 0.
They need no shell gate. Outside a hosted sandbox the session start prints `{}`. The stop hook
flushes wherever it runs, as `hushrun flush` does.

## Consequences

- An Antigravity Remote Agent conversation starts with the stores pulled and the model told what
  passed, once C-140 finds its sandbox (`HUSHRUN_SANDBOX=antigravity-remote` until a marker is
  confirmed).
- A subagent's or a resumed conversation's start costs a file read when it comes within 10
  minutes on the same commit. The block it prints may miss a check that passed in between.
- A step left running keeps the lock until it ends, so a start during a long pull says it's still
  running and starts nothing. That is what it would have found anyway.
- The cache directory holds two more small files. Neither holds a credential or the payload.
- A `--format` value that one pair of commands takes is a special case in `main`, in exchange for
  one flag everywhere.

## Alternatives considered

- **A generic `--format json`** with hushrun's own schema. Antigravity reads only its own shape,
  so a hook would need a converter in shell. A later platform with its own shape gets a value of
  its own.
- **Choosing JSON when C-140 says Antigravity.** A person running the command by hand would get
  JSON, and a probe or a setup script would get a format it didn't ask for.
- **A separate `--hook-format` flag.** Two flags would name formats. A value of `--format` is
  what a person tries first.
- **A lock file held only by the hook.** A step it left running would go unseen, and the next
  start would pull into the same folder at the same time.
- **Keeping `flush -q` as Antigravity's stop hook.** Its stdout lines would be parsed as the
  hook's answer, and it would flush every turn.
