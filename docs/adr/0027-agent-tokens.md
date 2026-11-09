# 0027. Agent tokens: agents in sandboxes record results without a browser

- **Status:** accepted
- **Date:** 2026-10-20
- **Rules:** C-10, C-11, C-12, C-13, C-14, C-17 (new); ADR 0023 item 2, which it amends for
  agent tokens; ADR 0018, which it leaves as it is

## Context

People launch many agents at once on a repository that `npx hushrun init` set up: Claude
Code cloud sessions, `claude -p` in containers, E2B or Docker sandboxes, CI on providers other
than GitHub. Each one runs `npm test`, and so `hushrun run`, and should skip what another agent,
a teammate or CI already passed. Today a backend takes two kinds of caller (results-v0 §2):

- a member's personal token from `hushrun login`, whose device flow needs a person at a browser
  for each machine, and which acts as that person in all their orgs;
- a GitHub Actions ID token, which exists only in GitHub Actions.

A sandbox has neither. `HUSHRUN_TOKEN` exists for other CI (ADR 0023 item 2), but nothing in
hushrun cloud issues a token that it accepts: a person would have to copy their own `hru_`
token into every sandbox, where it outlives the sandbox, can't be told apart from the person,
and reaches every org they belong to. And `HUSHRUN_TOKEN` goes only to the backend
`HUSHRUN_BACKEND` names, so a sandbox needs a second variable even when the repository's
`package.json` names the backend.

A fleet also writes far faster than a person. The backend limits only its public endpoints, per
address; nothing limits result writes, so one agent in a loop could fill an org's results.

Ledger and promotion writes by agents are out of scope: they are being redesigned around machine
keys registered at each sign-in, with the ledger root removed (ADR 0026, proposed on its own
branch). Agents' stage evidence
keeps ADR 0018's keys and L-6's writer tokens until then.

## Decision

1. **Agent tokens.** An org admin or owner creates one in the dashboard (the org's **Agents**
   page) or with `hushrun token create --agent <name>` (C-17). Each token:
   - is `hra_` and 40 base62 characters, the shape of `hru_` and `hrl_`, so a scanner or a
     person tells the three apart. It is shown once; the server keeps its SHA-256 and its first
     ten characters (`hra_` and 6) to show;
   - belongs to one org, and names an agent: a name as L-2 says (`claude-code`, `fleet-a`,
     `gitlab-ci`). Its results record as `agent:<name>`. Several tokens may share a name, so a
     token can be replaced before the old one is revoked, and one token may serve a whole fleet;
   - is scoped to one repository, `host/owner/name` as results-v0 §3 keeps it, or to the whole
     org when its creator chooses so. The CLI's default is the repository it runs in;
   - expires: 1 hour to 365 days after it is created, 30 days by default. It never renews;
   - is revoked on its own, by any admin, in the dashboard or with `hushrun token revoke`;
   - has a write limit, `results_per_minute`, the server's default (300, `HUSHRUN_AGENT_RESULTS_PER_MINUTE`)
     unless its creator sets another, from 1 to 10,000 (item 5);
   - leaves an audit entry when it is created and when it is revoked: `agent_token.created`,
     `agent_token.revoked`, with its id, name, prefix, repository, expiry and limit.

2. **An agent token records and reads results, nothing else.** At its own org it answers
   `GET {base}/v0`, `GET {base}/v0/results/{key}` and `POST {base}/v0/results/{key}`. Every
   other request refuses it with 403: connecting a repository, a repository's ledger, managing
   tokens, the dashboard's API and the hosted ledger (which takes `hrl_` tokens and ID tokens,
   never `hra_`). At another org it gets 403, which names the token's own.

3. **A repository scope holds on the server.** For a token scoped to a repository:
   - a result is recorded under the token's repository, whatever the record says, so the
     dashboard groups an agent's runs with CI's and people's;
   - a result whose `repository` names another repository is refused, 403, with both names in
     the message: the token is in the wrong sandbox. The same repository counts by its path's
     last segments, ignoring case, on any host, since a sandbox's checkout may reach it through
     a mirror or a git proxy (`127.0.0.1/git/acme/shop` is `github.com/acme/shop`). A record
     that names no repository takes the token's;
   - a lookup answers only results recorded for that repository (on `github.com`, ignoring
     case). An agent never skips on another repository's pass, and learns nothing about the
     org's other repositories.

   A token for the whole org records and reads as a member's personal token does.

4. **Who recorded, and which run.** A result records `agent:<name>`, the token's agent. A run
   may also carry a label, `invokedBy`, a new optional field of `hushrun.run-result/v0` (C-10)
   that hushrun fills from `HUSHRUN_INVOKED_BY` (`agent:<id>` or `human:<id>`, as events take it):
   one token per fleet, and `HUSHRUN_INVOKED_BY=agent:worker-17` in each member, tells the runs
   apart on Checks. The label informs; the token decides who recorded.

5. **Each token's writes are limited.** The server counts the token's results, skips included,
   over the last 60 seconds. Past `results_per_minute`, it answers 429 `rate_limited` with
   `Retry-After: 60` and a message naming the agent, the token's prefix and the limit, and
   records nothing. Counting what the token already recorded keeps the limit right across
   server instances, with no counter to keep. hushrun has already run the check by then: it
   says so in one line and exits with the command's code (C-14); the next run tries again.
   Lookups aren't limited: a skip is the cheap path, and a limited lookup would only make the
   fleet run more. People's tokens and CI's ID tokens aren't limited here.

6. **An agent token follows the repository's backend on hushrun's host** (C-13; amends ADR 0023
   item 2 for `hra_` tokens). `HUSHRUN_TOKEN` still goes to the backend `HUSHRUN_BACKEND`
   names. When `HUSHRUN_BACKEND` isn't set and `HUSHRUN_TOKEN` starts with `hra_`, it also goes
   to the repository's backend, or the one `hushrun login` saved, when that backend is on
   hushrun's host: `HUSHRUN_HOST`, by default `https://api.hushrun.com`, compared by origin.
   So a sandbox on the hosted backend needs one variable, `HUSHRUN_TOKEN`.

   ADR 0023 kept `HUSHRUN_TOKEN` from following a file because a changed file could send a
   bearer secret to any server. Here the host comes from hushrun or from the sandbox's
   environment, never from the repository: a changed `package.json` can only send the token to
   another org's path on hushrun's own host, which refuses it (item 2) and already holds its
   hash. Any other token in `HUSHRUN_TOKEN`, and an agent token for another host, keep ADR
   0023's rule, and hushrun says when it withholds one. A self-hosted server sets
   `HUSHRUN_HOST`, or `HUSHRUN_BACKEND`, in the sandbox's environment.

7. **Refusals a person can act on, in as few lines as possible** (C-14).
   - The server says what is wrong with the token, by its prefix and agent: unknown, expired
     (when), revoked (when), for another org, or scoped to another repository; and for an
     expired or revoked one, how to get another (`hushrun token create --agent <name>`).
   - When a backend refuses the token a lookup sent (401 or 403), hushrun says so once, with the
     server's message, runs the command, and doesn't try to record: the same token would be
     refused again. The run's last line says it wasn't recorded.
   - A run that sent no token and was refused for it (401) prints nothing about it until the
     end: at a terminal, only ADR 0023's one line, `hushrun: sign in to share results with
     <org>: npx hushrun login`; elsewhere, one line with the server's reason. Until now a
     teammate without a login saw two long 401 lines, for the lookup and for the record, next to
     the hint.
   - A record refused with 429 ends the run with one line: the check's result, that it wasn't
     recorded, and the server's message.

8. **`hushrun whoami`, and `hushrun doctor`, show the identity** (C-17). `whoami` prints the
   backend `hushrun run` would use and what named it, which credential goes there
   (`HUSHRUN_TOKEN`, the login's token, a GitHub Actions ID token, or none, and why one is
   withheld), and who the backend says it is, from `GET {base}/v0`. For an agent token,
   `GET {base}/v0` adds `token`: its kind, agent, prefix, repository, expiry and limit, which
   `whoami` shows. `doctor` adds the same as one line, `results`, before it reads
   `hushrun.yaml`, so it works in a repository that only uses `hushrun run`.

9. **No `hushrun login --with-token`, for now.** `HUSHRUN_TOKEN` reaches the repository's backend
   on hushrun's host (item 6), and every place agents run takes environment variables: a Claude
   Code cloud environment's settings, `docker run -e`, an E2B sandbox's `envs`, CI's secrets.
   Saving the token instead would put a second copy of the secret in an image or a volume that
   outlives the sandbox, and a non-interactive login that followed the repository's
   `package.json` would send the token wherever a branch points it, with no person to read
   which file chose the backend. It stays an additive change if a sandbox turns up that can't
   set a variable.

10. **Room for machine keys.** An agent token is a credential for results, and nothing in it
    signs: this ADR adds no signing keys for agents. The token has an id, an org, an agent and a
    scope, so the machine keys that the ledger's redesign registers at each sign-in (ADR 0026,
    proposed) can later attach to a token by its id: a fleet's token could register each
    sandbox's key with its first request, or require one before it records.

## Consequences

- Launching a fleet takes one token and one variable: create it on the Agents page or with
  `hushrun token create --agent fleet-a`, and set `HUSHRUN_TOKEN` in the sandbox's environment.
  With `HUSHRUN_PROVIDER=local` where a sandbox sets `CI=true`, and `HUSHRUN_INVOKED_BY` per
  member, Checks shows each run as `agent:fleet-a`, labeled.
- A leaked token records and reads results of one repository, at most `results_per_minute` a
  minute, until it expires or someone revokes it. Results are not evidence: they decide only
  whether a check of this exact code runs again (ADR 0023 item 2), and a pass recorded by an
  agent skips the check in CI as a laptop's pass does (C-10). Stage evidence is unchanged: it
  needs a principal that trust accepts (ADR 0018), which an agent token is not. The dashboard
  shows who recorded each run.
- CI on other providers uses the same tokens: `hushrun token create --agent gitlab-ci`, stored
  as the job's `HUSHRUN_TOKEN`.
- A fleet that writes faster than its limit loses sharing, not checks: its runs still run, and
  its lookups still skip on what others recorded.
- Expiry is mandatory, so long-lived uses (an always-on CI) rotate a token at least once a year:
  create the next one under the same agent name, switch the secret, revoke the old one.
- `hushrun-results/v0` gains optional parts: `GET {base}/v0`'s `token`, the agent-token requests
  (`{base}/v0/agent-tokens`, for an admin's personal token) and 429 on a write. Other servers
  ignore them; hushrun handles their absence.

## Alternatives considered

- **Personal tokens in sandboxes.** They act as a person in every org they belong to, never
  expire, can't be told apart from the person's own runs, and need a browser per machine.
- **Long-lived personal tokens minted in the dashboard for a person** (GitHub's PAT model). Still
  a person's identity in every org, and a fleet's runs would show as that person.
- **Ledger API tokens (`hrl_`) for results.** They are per ledger and carry ledger roles; a token
  for results would gain ledger access it must not have, and the ledger's model is being
  redesigned (ADR 0026).
- **An exchange: the token buys a short-lived session at sign-in.** Safer against replay, but
  every sandbox would hold the long-lived secret anyway to exchange it, and the server would
  keep a second kind of credential. Machine keys (item 10) address replay better, later.
- **Limits in memory, per server instance**, as the public endpoints' are. Cheap, but the server
  runs several instances, so a fleet's limit would multiply with them. The results table already
  holds what the limit counts.
- **A limit per org instead of per token.** One runaway token would stop every other agent,
  person and CI job of the org from recording. Per token, the runaway is the one that stops.
- **Keep `HUSHRUN_TOKEN` with `HUSHRUN_BACKEND` only.** Safe, but every sandbox would repeat a URL
  that the repository already commits, which is the step ADR 0023 removed for CI.
