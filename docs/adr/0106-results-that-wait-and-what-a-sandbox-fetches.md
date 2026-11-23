# 0106. Results that wait, what a sandbox fetches, and hooks at a session's end

- **Status:** proposed; built
- **Date:** 2026-11-23
- **Rules:** C-53, C-54 and C-55 new; C-11, C-24 and B-36 read

## Context

A hosted sandbox loses its network: Codex cloud turns it off once setup ends, and any sandbox can
lose its proxy for a while. Until now, a result whose server couldn't be reached was dropped, and
while a backend was named, lookups had nothing to fall back on, so a check the agent had just passed
ran again. `hushrun build fetch` saved compiled Rust crates for such a sandbox, and nothing else.
And nothing ran when an agent's session ended, so what it passed in its last minutes could stay in
a sandbox that was about to go.

## Decision

1. **An outbox.** A result whose request didn't reach its server, or that the server failed (5xx,
   408), is kept in `<cache dir>/outbox/<key>.json`, written whole under `outbox.lock`. Its one
   state, decided by one function, is `pending`, `sent` (2xx), `refused` (any other 4xx but 429),
   or `expired` (7 days after it was written, by this machine's clock when read). Settled states
   stay. A newer result of the same key replaces one still pending: the server answers the newest.
2. **Sent by the next run.** `hushrun run` and `hushrun ready` send what waits for their backend
   before anything else, and so does `hushrun flush`. Only to the server an entry was for, and only
   with the same kind of credential that wrote it. One flush at a time; a server out of reach is
   tried once a flush. Sending again is safe: the server keeps one result per caller, key and body.
3. **Read while the server is out of reach.** A lookup that can't reach its server, or that the
   server fails, answers from the outbox (pending or sent), then from what `hushrun fetch` saved:
   a check this machine passed skips, with no network.
4. **Never in CI.** A runner's cache is shared with no one, and whoever can write a cache CI
   restores could have a job send it with the job's credential (ADR 0067).
5. **`hushrun fetch`** looks up every check of the worktree as `hushrun ready` does, keeps each
   answer, sealed by its SHA-256 over the server, the key and the result, for 24 hours, then
   fetches compiled code as `hushrun build fetch` does. `build fetch` stays as it was.
6. **Hooks at a session's end.** `hushrun init --agent-clouds <list>` (asked at a terminal, else the
   clouds the repository already has files for) writes, for each cloud named: Claude Code's `Stop`
   hook in `.claude/settings.json`, with a `SessionStart` hook that pulls the baked store in Claude
   Code on the web; Cursor's `stop` in `.cursor/hooks.json`; Copilot's `sessionEnd` in
   `.github/hooks/hushrun.json` and its setup steps; and a dev container with hushrun's feature.
   Each hook runs `hushrun flush`. JSON files keep what they have. The AGENTS.md block says to run
   `hushrun flush` before finishing.

## Consequences

- A sandbox that loses its network keeps skipping what it passed, and its results reach the backend
  when the network comes back, or when the session ends with a network. A session whose network
  never comes back loses them: that is accepted, and the docs say so.
- What `hushrun fetch` keeps lets a check skip on a pass from CI or another machine with no network,
  for 24 hours; a withdrawal in that time isn't seen, as for `build fetch`.
- The server dedupes a result sent again by its whole body. Two genuine runs with byte-identical
  results (the same start time and duration) would count once; the CLI writes the start to the
  millisecond, so that doesn't happen in practice.
- `hushrun fetch` doesn't fetch test files' results or hints, and compiled code only where `build
  fetch` does (Rust): Go packages and C and C++ objects compile in such a sandbox as before.

## Alternatives considered

- **One file per result** (`<key>-<run>.json`). The server answers the newest result of a key; older
  ones still pending are worth nothing to a lookup, and only cost requests.
- **Idempotency keys in a header.** The body already identifies a run; a header would need every
  server to keep keys.
- **Flushing in the pre-push hook.** A push isn't the end of a session, and the hook must stay fast.
