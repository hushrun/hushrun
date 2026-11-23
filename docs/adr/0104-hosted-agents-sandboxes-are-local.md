# 0104. A hosted agent's sandbox is local, whatever CI it sets

- **Status:** proposed; built
- **Date:** 2026-11-23
- **Rules:** C-50 new; C-11, C-17, C-23 and §11.2's detection read
- **Amends:** [0046](0046-one-variable-for-a-sandbox.md) item 3, for a platform's own marker

## Context

Coding agents now run in sandboxes their vendors host: Claude Code on the web, Codex cloud,
Cursor's cloud agents, GitHub's Copilot cloud agent, Jules, Devin. hushrun reuses a pass there only
when it knows the sandbox isn't CI, and it didn't:

- `detect` read none of the agents' markers that the hook's test of who is an agent (C-23) already
  knew, in a list of its own. A sandbox that exports `CI=true` ran as generic CI: results off with
  no backend (ADR 0067), and no reuse within the session either.
- GitHub's Copilot cloud agent runs inside a GitHub Actions job. hushrun took it for a CI job of the
  repository, one that isn't in the repository's workflows, so ADR 0044's guard skipped nothing,
  and the job's ID token would have made its results CI's.
- ADR 0046 item 3 lets `HUSHRUN_AGENT` override bare `CI=true` and nothing else, because a secret
  reaches a named CI's job easily. That reasoning holds for a secret. It doesn't for a variable
  only a vendor's own platform sets.
- Nothing said, from inside a sandbox, which hosts hushrun needs, whether they answer, and what to
  put in that platform's allowlist.

We ran `scripts/agent-cloud-probe.sh` in a Claude Code on the web session (2026-11-23):
`CLAUDE_CODE_REMOTE=true`, `CLAUDECODE` and `IS_SANDBOX` set, `CI` unset, linux x86_64, glibc
2.39, rustc 1.94, go 1.24, node 22 preinstalled, a git proxy (`GIT_CONFIG_*` from the
environment), a pre-push hook that fires on a local push, and an HTTPS proxy whose CA is in the
system store. With the environment's default network, hushrun's hosts were refused by the proxy
(403), so a team must allow them.

## Decision

1. **One table of agents and platforms** (`hushrun_ci::platforms`), read by `detect` and by C-23's
   test of who is an agent, so the two never disagree. Each row has an id, a name, the agent it
   runs, its markers, its kind, how hushrun knows the marker, and where that platform lets a team
   allow hosts.
2. **Two kinds of row.** A *platform* row is a vendor's hosted sandbox, whose marker only that
   platform sets: `CLAUDE_CODE_REMOTE` (Claude Code on the web, probed), `CURSOR_AGENT_SOCKET`
   (Cursor's cloud agents, documented with Cursor's OIDC tokens), `COPILOT_AGENT_SESSION_ID`
   (Copilot's cloud agent) and `CODEX_PROXY_CERT` (Codex cloud). An *agent* row is a coding agent
   that also runs on people's machines and in CI jobs: `CLAUDECODE`, `CODEX_SANDBOX`, `CURSOR_AGENT`
   and the rest C-23 already read.
3. **A new outcome, a sandbox.** `detection` answers `Ci(provider)`, `Local`, or `Sandbox(row)`,
   which is local: runs record `local`, local mode works (C-11), and reuse within the session is
   free, with no account. `detect` answers the provider, as before.
4. **Order.** An explicit `HUSHRUN_PROVIDER` first, a person's decision. Then a platform's marker,
   which beats `CI=true` and a named CI's variable alike (this amends ADR 0046 item 3 for platform
   markers only). Then a named CI. Then bare `CI=true`: a sandbox beside an agent row's marker, local
   beside `HUSHRUN_AGENT` (ADR 0046), else generic CI. Else a person's machine. `HUSHRUN_AGENT` alone
   still doesn't override a named CI.
5. **A sandbox in GitHub Actions never gets CI's ID token.** `Env` sets
   `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN` aside in a platform's sandbox,
   so no result, ledger write or build request carries it as CI's credential. Only
   `hushrun login --session` reads them (ADR 0105), to get a session as an agent.
6. **`hushrun doctor`** says, in a sandbox or with `--hosts`: the platform and its marker; where the
   credential comes from, by name; whether each host answers over TLS that verifies with the
   system's store (the backend, its registry, and for hushrun's own backend
   `*.r2.cloudflarestorage.com`); the exact entry for that platform's allowlist; and what will be
   shared. It never prints a value.
7. **Unconfirmed markers say so.** `COPILOT_AGENT_SESSION_ID` was reported by a third party and
   isn't in GitHub's documentation; `CODEX_PROXY_CERT` likewise for Codex cloud. Their rows are
   marked unconfirmed, doctor warns in their sandbox, and the fix for a wrong guess is
   `HUSHRUN_PROVIDER`. Jules and Devin have no row: nothing documents a marker, and neither sets
   `CI`, so they are local already.

## Consequences

- A hosted sandbox reuses passes within its session with no setup, and shares them once a credential
  is set (ADR 0105). Results of a sandbox count in CI jobs without variants as any agent's pass does
  (ADR 0044): no new guard.
- A workflow that sets a platform's marker by hand makes its job a sandbox. Only someone who writes
  the workflow can, and they could set `HUSHRUN_PROVIDER=local` as well.
- The table is a contract with the docs' per-platform pages: a row's id names its page.
- A vendor that renames its marker breaks detection until a release follows; the sandbox runs as
  before this ADR meanwhile, which costs reuse, never a wrong skip.

## Alternatives considered

- **Any agent marker beats a named CI.** An agent's step in a GitHub Actions job (Claude Code's
  action sets `CLAUDECODE`) would lose ADR 0044's guards. Only platform markers do.
- **`IS_SANDBOX` as a marker.** It is set in the session we probed, but it is a common name with no
  owner; anyone's container may set it.
- **A `Provider::Sandbox`.** Events and the server read the provider; a new value would need both to
  change, and a sandbox records as `local` already.
- **Treating Copilot's job as CI and adding its workflow to the repository's.** The job isn't the
  repository's CI, and its results must never write CI scopes.
