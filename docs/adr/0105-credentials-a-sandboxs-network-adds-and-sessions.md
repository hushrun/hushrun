# 0105. Credentials a sandbox's network adds, and sessions

- **Status:** proposed; built
- **Date:** 2026-11-23
- **Rules:** C-51 and C-52 new; C-13, C-17, C-18 and C-50 read
- **Amends:** [0046](0046-one-variable-for-a-sandbox.md), for sandboxes that hold no key

## Context

ADR 0046 gave a sandbox one variable, `HUSHRUN_AGENT`, which holds the agent token and its private
key. The key signs each result in the sandbox (ADR 0028), so the string must be in the sandbox's
environment. Hosted agent platforms increasingly keep secrets out of the sandbox:

- Claude Code on the web and others add a credential to outgoing requests at their network's proxy:
  the sandbox never sees it, so it can't sign with it.
- Codex cloud gives secrets to the setup script only; its newer form puts a placeholder in the
  variable, which its network replaces in requests.
- Cursor's cloud agents mint short OIDC ID tokens over a socket (`CURSOR_AGENT_SOCKET`,
  `POST /v1/tokens/oidc`; `iss` `https://api.cursor.com`, keys at `https://api.cursor.com/keys`),
  naming the team, the repository and the agent's run.
- GitHub's Copilot cloud agent runs its setup steps in a GitHub Actions job, whose ID token proves
  the repository, but the agent's results must never count as that repository's CI.

None of these can hold a key, and none should need an org admin to copy a secret by hand.

## Decision

1. **Bearer agent tokens.** An agent token is made `machine` (as before: a key comes with it and
   signs its results) or `bearer` (`hushrun token create --bearer`, or the dashboard's "Bearer
   token, no key"): no key exists, the token alone records, and the server keeps who sent it, as
   `agent:<name>`. Off by default. The server knows which a token is, so a machine token's results
   without a signature are still refused (`machine_required`).
2. **`HUSHRUN_AUTH`.** `proxy`: requests to the backend carry no token, the platform's network adds
   it, and a 401 means the network wasn't set up, which the run says, naming the host. `bearer`:
   `HUSHRUN_TOKEN` is a bearer agent token, sent with no key. A value hushrun doesn't know is
   ignored (§10.1). Neither signs.
3. **A placeholder is sent as it is.** In a hosted platform's sandbox (C-50), a `HUSHRUN_TOKEN` that
   has none of hushrun's shapes (`hru_`, `hra_`, `hrs_`, `hrl_`) is taken for a placeholder the
   network replaces: it goes where an agent token goes (hushrun's host, or the backend
   `HUSHRUN_BACKEND` names), with no key. Outside such a sandbox it is a token like any other, and
   a repository's file never gets it (C-13).
4. **Session tokens.** `POST {base}/v0/sessions` swaps an agent token, a Cursor cloud agent's ID
   token or a GitHub Actions ID token for a session token (`hrs_`): one repository's results, read
   and recorded, for 4 hours by default and 12 at most, never past its agent token, as
   `agent:<name>` (`agent:cursor`, `agent:copilot` for a job GitHub runs for Copilot, else
   `agent:github-actions`). A session reaches neither the shared store nor the registry, and never
   records as CI. It ends when its agent token is revoked or expires, when its Cursor link is
   removed or lapses, or when its repository is disconnected: the server reads that, with its own
   clock, at every request, and also revokes the sessions in the transaction that revokes a token or
   removes a link.
5. **`hushrun login --session`** makes one with what the environment holds, in that order, and
   writes it whole to `sessions.json` beside the credentials file (a temporary file and a rename,
   mode 0600, under a lock), by origin. It prints no token. Runs outside CI read it when nothing
   else is set for that origin; with an agent token or `HUSHRUN_AGENT` in the environment, those
   win. It is for agents: it never registers a machine, so `HUSHRUN_AGENT` doesn't refuse it.
6. **Cursor.** An org's owner or admin links a Cursor `team_id` on the dashboard, audited and
   revocable; a team links to one org at a time, and the link holds only while the person it rests
   on is an owner or admin of the org. `repo_url` must name a repository the org connected. In a
   Cursor cloud agent's sandbox, a run with no session gets one by itself, minting Cursor's ID token
   for the backend's origin and caching it until it expires, and saves the session.
7. **Copilot.** Its setup steps run `hushrun login --session` with the job's ID token (`permissions:
   id-token: write`). In the agent's own job, hushrun knows (C-50), the job's ID token is set aside
   from everything but that command (ADR 0104 item 5).

## Consequences

- A sandbox can share results with no key in it, and, on Cursor and Copilot, with no secret copied
  at all. What it records is the agent's word, as any agent's is, and a CI-only repository (T-16)
  never reuses it.
- A bearer token is a weaker credential than a machine token: whoever holds it records as the agent
  until it is revoked or expires. That is why it is off by default and why a session, one
  repository's for hours, is the form to hand a sandbox where one can be minted.
- Cursor's token format, signing algorithm and socket protocol are taken from the plan's research
  and not tried against Cursor: the server takes RS256 only, and the CLI reads the answer's
  `token`, `id_token`, `idToken` or `value`. Copilot's job is told by `copilot` in its
  `job_workflow_ref` or `actor`, also unconfirmed. Both need verifying on the platforms.
- Sessions don't use the shared store of build blocks yet: a sandbox that wants compiled code still
  needs an agent token or a baked store.

## Alternatives considered

- **The server signs results with a key of its own.** Results are an agent's word, kept with who
  sent them; the server signs nothing (RFC 0001), and a signature it made itself would prove nothing
  more than the record does.
- **A key in the session.** The point is that nothing secret and lasting is in the sandbox.
- **Cursor's or GitHub's ID token on every request.** Each request would verify a JWT against a
  key set, and a 5-minute token would expire mid-run; a session is one check, and lasts the work.
- **A placeholder recognised by its shape.** Platforms don't promise one; any token that isn't
  hushrun's, in a hosted sandbox, is what such a platform hands over.
- **Treating Copilot's ID token as CI's.** The job isn't the repository's CI, and its results must
  never write CI scopes.
