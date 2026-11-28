# Antigravity Remote Agents, probed

[ADR 0161](../adr/0161-a-session-start-that-warms-and-tells.md) has a Claude Code on the web
session start by pulling the compiled code and the newest results, and by telling the agent what
already passed. Google Antigravity's Remote Agents could get the same, through the repository's
`.agents/hooks.json` or the remote project's environment setup script, but the answers that design
needs are not in Antigravity's documentation: which variable marks a remote environment, whether a
repository's hooks load there, how long a hook may take, what a setup script's work leaves for the
agent, and which of hushrun's hosts answer.

This page is the kit that answers them, and the form for the answers. Someone with a Google AI Pro
or Ultra account runs it in real Remote Agent conversations. It does for Antigravity what
`scripts/agent-cloud-probe.sh` did for Claude Code on the web on 2026-11-23, the run
[ADR 0140](../adr/0140-hosted-agents-sandboxes-are-local.md) cites.

**Status: not yet run.** The [results](#results) are empty until someone runs it.

## What the kit does and never does

It prints the names of variables, never a value, and nothing of the prompts: a hook's stdin carries
the user's prompt as `lastUserInput`, and the hooks log the names of stdin's fields, its
`workspacePaths`, `fullyIdle`, and a 10-character hash of `conversationId` that tells conversations
apart, and nothing else from it. It writes only the files below, plus a `PROBE_FROM_SETUP` line in
`~/.profile`, `~/.bashrc` and, as root, `/etc/environment`. It sends only unauthenticated HTTPS
requests, with no credential, to the hosts in the report's reachability section.

| File | Goes | Writes |
| --- | --- | --- |
| [`scripts/agent-cloud-probe.sh`](../../scripts/agent-cloud-probe.sh) | in the test repository, at the same path | the report: with `--mode agent`, `setup` or `hook`, also `/tmp/probe-<mode>.txt` |
| [`scripts/antigravity-probe/hook.sh`](../../scripts/antigravity-probe/hook.sh) | in the test repository, at the same path | `~/.probe-hooks.log`, `~/.probe-bg` |
| [`scripts/antigravity-probe/hooks.json`](../../scripts/antigravity-probe/hooks.json) | copied to `.agents/hooks.json` | a `SessionStart` hook (sleeps 50 s, `timeout: 90`, then injects `PROBE-OK`) and a `Stop` hook |
| `hooks-empty.json`, `hooks-text.json`, `hooks-fail.json` | swapped in for Q3 | a `SessionStart` hook that prints nothing; plain text (`PROBE-TEXT`); JSON (`PROBE-FAIL`) and exits 1 |
| `hooks-slow.json` | swapped in when 90 s was honored | the main hook with a 100 s sleep and `timeout: 150` |
| [`scripts/antigravity-probe/env-setup.sh`](../../scripts/antigravity-probe/env-setup.sh) | pasted into the remote project's settings | `~/.probe-setup`, `/tmp/probe-setup.log`, `/tmp/probe-setup.txt` |

A hook finds `hook.sh` under its cwd, the repository's root, `$HOME`, `/home/antigravity` or
`/workspace`. If it finds none, it logs `hook.sh-not-found` with its cwd, and the `SessionStart`
hook injects `PROBE-NOSCRIPT` instead: the hook loaded, but from somewhere else.

## Before you start

- **Stop's `decision`.** Antigravity's `Stop` hook must print a `decision`, and this kit couldn't
  read which values [hooks.md](https://antigravity.google/docs/hooks.md) allows. `hook.sh` prints
  `{"decision":"allow"}` (its `stop_decision`, and the fallback at the end of `hooks.json`'s `Stop`
  command). If hooks.md names another value for "let the agent stop", change both before you copy
  the kit. If a conversation never ends its turn, or keeps continuing by itself, remove the `Stop`
  group from `.agents/hooks.json` and note it under Q2.
- **Write down the clock.** For each conversation, note the time you send the first message and
  the time the first reply appears. The logs are in UTC.
- **Expect about an hour,** most of it waiting for a conversation to go idle.

## Steps

### 1. A test repository

Create an empty private repository on GitHub, say `antigravity-probe`, then, on your machine:

```sh
git clone https://github.com/hushrun/hushrun /tmp/hushrun
git clone https://github.com/YOU/antigravity-probe && cd antigravity-probe
mkdir -p scripts .agents
cp /tmp/hushrun/scripts/agent-cloud-probe.sh scripts/
cp -R /tmp/hushrun/scripts/antigravity-probe scripts/
cp scripts/antigravity-probe/hooks.json .agents/hooks.json
git add . && git commit -m "hushrun Antigravity probe" && git push
```

### 2. A remote project

In Antigravity, create a remote project and attach the repository. Then, in the project's settings:

- Paste [`env-setup.sh`](../../scripts/antigravity-probe/env-setup.sh) whole as the environment
  setup script, with `PROBE_SETUP_SLEEP=0`.
- **Q10:** read the settings page and write down whether it has environment variables, secrets or
  network settings, what each is called, and whether a secret is offered to setup, to the agent,
  or both. Don't add any yet.

### 3. Conversation A, the first message

Start a conversation and send this as the first message, unchanged:

> Before anything else: did a system message with PROBE-OK, PROBE-TEXT, PROBE-FAIL or
> PROBE-NOSCRIPT in it reach you at the start of this conversation? Quote it whole, or say no.
> Then, from the repository's root, run `sh scripts/agent-cloud-probe.sh --mode agent >/dev/null`
> and show me `/tmp/probe-agent.txt`, `/tmp/probe-setup.txt` and `/tmp/probe-hook.txt` whole, each
> in its own code block (say so if one is missing). Then run
> `echo "shell:${PROBE_FROM_SETUP:-unset} env:${PROBE_FROM_SETUP_ENV:-unset} etc:${PROBE_FROM_SETUP_ETC:-unset}"; bash -lc 'echo "login:${PROBE_FROM_SETUP:-unset}"'; bash -ic 'echo "interactive:${PROBE_FROM_SETUP:-unset}"'`
> and show me its output, and tell me how a command you run is started: which shell, as a login or
> interactive shell, or neither.

Copy the whole reply into your notes.

### 4. Conversation A, a push and a pull request (Q9)

> Install a pre-push hook in this clone with
> `printf '#!/bin/sh\necho PRE-PUSH-FIRED >&2\ndate -u >> "$HOME/.probe-prepush"\n' > .git/hooks/pre-push && chmod +x .git/hooks/pre-push`.
> Then create a branch `probe-pr`, add a file `probe.txt` with one line, commit it, push the
> branch and open a pull request. Tell me exactly how you pushed and opened it: the command or tool,
> and whether it was `git push` in this shell. Show me any output with PRE-PUSH-FIRED in it, and
> then `cat ~/.probe-prepush`. Also show me `git config --show-origin --get-regexp '^(credential|url|http)\.'`
> with every value after the key replaced by `<set>`.

### 5. Conversation A, three minutes later (Q4)

Wait until at least three minutes have passed since the first message, then:

> Run `sh scripts/agent-cloud-probe.sh --mode agent >/dev/null` again and show me only its
> "Antigravity" section.

`~/.probe-bg` should now hold a `bg-done` line: the detached `sleep 120` outlived the hook.

### 6. Conversation A, a subagent (Q4)

> Start a subagent and have it run `echo hello from the subagent`. When it's done, show me
> `cat ~/.probe-hooks.log`.

A `SessionStart` line after the subagent started, with a `conv=` of its own or the same, says the
hook fires for subagents, and whether the subagent counts as its own conversation.

### 7. Conversation B (Q5, Q4)

Start a second conversation in the same project and send step 3's message again.

- **Q5, once or per conversation:** count the `setup-start` lines in `~/.probe-setup` and compare
  their `boot=` with conversation A's. A new `setup-start` line means setup ran again for B. One
  line, with A's time, in a machine with a new `boot=` means a snapshot of setup's work.
- **Q4:** a new `SessionStart main start` line, with a different `conv=`, means the hook fires per
  conversation.

### 8. Conversation A, idle and resumed (Q4)

Leave conversation A untouched until Antigravity shows it as idle or paused (note how long that
took, and what the UI said). Then send:

> Run `sh scripts/agent-cloud-probe.sh --mode agent >/dev/null` and show me its "Antigravity"
> section and its first six lines. Did a system message with PROBE-OK in it reach you again?

- A new `boot=` means a new machine. `~/.probe-*` files still there mean `$HOME` was kept; the
  `/tmp/probe-*.txt` files still there mean `/tmp` was kept too.
- A new `SessionStart` line means the hook fires on resume.
- A `bg-done` line dated after the idle began means a detached process survived the idle.

### 9. Hook output variants (Q3)

For each of `hooks-empty.json`, `hooks-text.json` and `hooks-fail.json`, in that order:

```sh
cp scripts/antigravity-probe/hooks-text.json .agents/hooks.json   # the variant
git commit -am "probe: text variant" && git push
```

Start a new conversation and send:

> Did a system message with PROBE-TEXT, PROBE-FAIL, PROBE-OK or PROBE-NOSCRIPT in it reach you at
> the start of this conversation? Quote it whole, or say no. Then show me
> `tail -n 3 ~/.probe-hooks.log`.

Note whether the conversation started normally, showed an error, or didn't start, and what the
agent quoted.

### 10. Limits (Q2, Q5)

- **A hook's timeout above 90 s:** only if PROBE-OK reached conversation A. Swap in
  `hooks-slow.json` as in step 9 and ask the same question. A `SessionStart slow end` line and
  PROBE-OK quoted mean 150 s was honored. A `start` line with no `end` means the hook was killed
  before 100 s.
- **The setup script's time limit:** set `PROBE_SETUP_SLEEP=900` in the pasted script, start a new
  conversation, and note whether it starts, and after how long. Then ask the agent for
  `cat ~/.probe-setup /tmp/probe-setup.log`. The last `setup-still-running` line is how long setup
  was allowed, give or take 30 s. Set it back to 0 afterwards.
- Put `hooks.json` back in `.agents/hooks.json` when you're done.

### 11. Clean up

Delete the remote project and the test repository. In your own clones nothing changed.

## Reading the answers

| You saw | It means |
| --- | --- |
| PROBE-OK quoted, the first reply 50 s or more after your message | `SessionStart` loads from the repository, runs before the first model call, blocks it, and got at least 50 s |
| PROBE-OK quoted, the first reply sooner than 50 s | the hook doesn't block the first call; its message arrived before the model answered all the same, so check the log's times |
| no PROBE-OK, but `start` and `end` log lines | the hook ran its 50 s and its output didn't reach the model, or not before the reply; ask again in the next message |
| a `start` log line and no `end` | the hook was killed before 50 s: `timeout: 90` isn't honored, and the cap is the gap between the first reply and `start`, or less |
| PROBE-NOSCRIPT, or a `hook.sh-not-found` line | the repository's `hooks.json` loaded, but its cwd is outside the repository: the line has the cwd |
| no log line at all, and `~/.probe-hooks.log` absent | the repository's `.agents/hooks.json` doesn't load remotely (or `sh` isn't how a hook runs) |
| the `ANTIGRAVITY_*` and other markers in `/tmp/probe-agent.txt` | Q1: a marker that reads set in a remote conversation, and which `/tmp/probe-hook.txt` also has, is the candidate for C-140's table; compare with a local Antigravity conversation if you have one |

## Results

Fill in a cell with what you saw, and its step. Paste the three reports, `/tmp/probe-agent.txt`,
`/tmp/probe-setup.txt` and `/tmp/probe-hook.txt`, below the table or in an issue linked from it.
Read them first: the probe prints no value, but an agent's reply might add one.

- **Run by:** not yet run
- **Date:**
- **Plan:** (Pro or Ultra)
- **Antigravity version:**

| # | Question | Where it shows | Result |
| --- | --- | --- | --- |
| Q1 | Which variable marks a **remote** environment? Is `ANTIGRAVITY_AGENT` set? `CI`? | "Platform markers" in `probe-agent.txt` and `probe-hook.txt` (steps 3, 7) | |
| Q2 | Does the repository's `.agents/hooks.json` load remotely? Does `SessionStart` fire before the first model call, and block it? What timeout is honored (90 s? 150 s?)? Does `Stop` with `decision` work? | the agent's answer, the clock, `~/.probe-hooks.log` (steps 3, 10) | |
| Q3 | What a hook's stdout may be: empty; plain text (does it reach the model?); JSON with exit 1 (does the conversation start? does the message arrive?) | step 9 | |
| Q4 | Does `SessionStart` fire for a subagent? A second conversation? A resumed one? Does a detached process outlive the hook, and an idle and resume? | `~/.probe-hooks.log`, `~/.probe-bg` (steps 5 to 8) | |
| Q5 | Setup: once per project (a snapshot) or per conversation? Its variables and secrets? Network? Root (`id -u`, `sudo -n true`)? Time limit? Do `~/.profile`, `~/.bashrc`, `/etc/environment` lines reach the agent's shell, and which shell reads them? | `~/.probe-setup`, `probe-setup.txt`, step 3's `echo` (steps 3, 7, 10) | |
| Q6 | Do `api.hushrun.com`, `registry.hushrun.com`, `*.r2.cloudflarestorage.com`, `ghcr.io`, `pkg-containers.githubusercontent.com`, `registry-1.docker.io`, `us-docker.pkg.dev` answer? A proxy? Its CA in the system store? The same from setup as from the agent? | "Reachability" and "Proxy and trust" in `probe-agent.txt` and `probe-setup.txt` | |
| Q7 | OS, arch, glibc, CPUs, memory, free disk; rustc, go, node, java, `npx`, Docker; `/` and `$HOME` one file system? | "System" and "Toolchains" in `probe-agent.txt` | |
| Q8 | The workspace's path; the hook's cwd against stdin's `workspacePaths`; the names of stdin's fields | `cwd=`, `workspacePaths=` and `keys=` in `~/.probe-hooks.log` | |
| Q9 | A git credential proxy or helper? Does a pre-push hook fire, locally and on the agent's own push? How does the agent open a pull request? | "Git" in `probe-agent.txt`, step 4 | |
| Q10 | Environment variables or a secret store in the project's settings, and who sees them | step 2, "Variable names" in `probe-setup.txt` and `probe-agent.txt` | |
