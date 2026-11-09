# 0028. Every sign-in registers the machine

- **Status:** accepted; amended by [0031](0031-membership-is-trust.md) on 2026-10-23: a
  registered machine of a current member or agent counts by itself, with no `trust:` entry, and
  `hushrun trust add --this-machine` is gone; amended on 2026-10-28 (amendment 1, below): a
  person's or an agent's result is refused unsigned, an agent token comes with its one key, and
  people's machines are revoked after 90 days unused and capped at 20 per org
- **Date:** 2026-10-21
- **Rules:** C-18 (new), C-11, C-13, L-6, L-10, E-2, E-8, T-1, T-2, T-12; ADR 0026, which it
  completes; ADR 0027, whose agent tokens it gives machines; ADR 0018 item 18, which it amends
  for approvals
- **Amends:** [0023](0023-a-repositorys-backend-and-the-hosted-default.md) item 2 (what a
  personal token may write); [0018](0018-local-and-agent-evidence.md) item 18 (how people sign);
  RFC 0002 §3.8's "Identity" (D6), which had an admin approve each laptop key as a trust change
  signed in the browser

## Context

ADR 0026 moves trust into the repository: the `trust:` section of `hushrun.yaml` on the default
branch, so that `npx hushrun init` leaves a ledger that works once its commit merges, with no
root, signature, upload or pin. That settles who signs for CI. It leaves the people and the
agents:

- A laptop records results with a bearer token (`hru_`, C-13), and nothing proves which machine
  sent them. A token pasted into a log or a chat records results as its owner from anywhere.
- To write ledger events (`hushrun run --stage`, `approve`), a laptop or an agent needs a ledger
  API token (`hrl_`) that an admin made and someone pasted into `HUSHRUN_LEDGER_TOKEN`, plus a
  key file made with `hushrun keys generate` whose public half someone put into trust, or the
  keyless browser flow that publishes their email in Sigstore's log (ADR 0018 item 18).
- Agents (ADR 0027) get tokens that record results only, and its item 10 leaves room for keys.

The direction: writes to the ledger come only from signed-in machines, every sign-in
authenticates the machine it runs on, and init's first sign-in does it for the first time.

One thing the root did must survive. It kept the server from deciding who counts (RFC 0001 §4.7):
a hosted ledger that served the list of signers could add one. If readers counted every machine
the server says is registered, the server would decide again. So the decision separates two
questions: who may write, which the server decides, and whose writes count for a promotion,
which the repository decides.

## Decision

### The key

1. **Each machine has one key, made the first time it signs in (C-18).**
   - It is an ECDSA P-256 key (E-2), made by hushrun and kept beside the credentials file:
     `<config dir>/hushrun/machine-key.pem`, PKCS#8 PEM, readable by its owner only (0600 on
     Unix), written through a temporary file and a rename like `credentials.json` (C-13).
     `HUSHRUN_MACHINE_KEY_FILE` names another path; `HUSHRUN_MACHINE_KEY` holds the PEM itself,
     for sandboxes that keep no files.
   - P-256 rather than Ed25519, because the Secure Enclave, TPMs and Windows Hello hold only
     P-256 keys: a later release can move the key into them without changing its type, its key
     id or anything a reader sees.
   - One key per machine, reused by every sign-in on it, whatever the origin or the org, and made
     again only when the file is missing. Its public half isn't secret; "this machine" is what a
     reader names when it counts it (item 6).
   - It is never in a repository. hushrun refuses a key path inside the repository it runs in,
     and an isolated run already refuses a key file among its exported entries (ADR 0018
     item 4).

### Registration

2. **Every sign-in registers the key, with proof that the machine holds it.**
   - `hushrun login`, init's sign-in (C-15 step 3) and `hushrun login` again on the same machine
     all send it. `POST {origin}/cli/login` gains `"machine": {"key": "ecdsa-p256:…", "name":
     "<host name>"}`, and the approval page shows the name and the key id beside the client, so
     the person approves "ana-mbp, sha256:3f2a…".
   - `POST {origin}/cli/login/token` gains `"proof"`: standard base64 of the machine key's
     signature over `hushrun-machine/v1\n<device_code>\n<keyid>\n`. The device code never
     leaves the CLI that started the flow, so a relayed sign-in can't register a key its
     relayer holds, and a server can't be handed someone else's public key as yours. Without a
     proof that verifies, the server answers 400 `invalid_proof` and issues no token. The
     answer gains `"machine": {"id", "keyid"}`, and hushrun keeps the key id with the login.
   - A token from before this release, and an agent token (ADR 0027) on its first result from
     a machine, register with `POST {base}/v0/machines` `{"key", "name", "proof"}`, the proof
     over `hushrun-machine/v1\n<hex of SHA-256 of the token>\n<keyid>\n`. 201 registers, 200
     the same key again for that token. hushrun remembers the answer in its cache directory, so
     a sandbox registers once. `hushrun token create --with-key` registers the agent's key with
     the new token as it makes it (item 10).
   - **Machines are per org.** A person's machine belongs to them in the org the sign-in was
     approved for, bound to the CLI token it issued; an agent's machine belongs to its token,
     in the token's org. A person who works in two orgs signs in for each.
   - A server without machines ignores `machine`, and answers 404 or 405 to `/v0/machines`:
     hushrun signs in as before, says that the server doesn't register machines, and sends its
     results unsigned there.
   - The server writes `machine.registered` to the org's audit log, with the name, key id and
     client.

3. **A machine counts at the server while its sign-in does.**
   - `hushrun logout` revokes the token, and the machine with it. Signing in again registers it
     again.
   - Removing a member from an org revokes their machines in that org. Their token keeps working
     in their other orgs, where their machines are registered on their own. Until now removing a
     member only deleted the membership, and each request re-checked it.
   - Revoking an agent token revokes the machines it registered.
   - The dashboard lists a person's machines on their account (name, key id, org, client,
     registered, last used) and revokes one, which revokes its token. An org's admins see the
     org's machines and revoke any. Each revocation is `machine.revoked` in the audit log.

### What a machine writes

4. **The backend accepts a write from a person or an agent only when a registered machine
   signed it.** Beside CI's identities (ADR 0026 item 8), which don't change:
   - **Results** (C-12). `POST {base}/v0/results/{key}` from a personal or agent token carries
     `Hushrun-Signature: <keyid> <base64 signature>`, by the machine key, over
     `hushrun-result/v1\n<key hex>\n<SHA-256 of the body, hex>\n`. The server checks that the key
     is a registered, unrevoked machine of the token's owner in that org, and records which
     machine sent the result; a key it doesn't know is 401 `machine_unknown`, a signature that
     doesn't verify 401 `invalid_signature`, and nothing is recorded. For one release it still
     takes an unsigned result, and the dashboard marks it unsigned; then it refuses it with 401
     `machine_required`. An ID token never signs.
   - **Ledger objects** (L-10). `PUT {ledger}/objects/v1/…` takes a personal token of a member of
     the ledger's org, or an agent token of that org whose scope covers the ledger's repository,
     when the object is an event (an envelope or a bundle) signed by a key that is a registered,
     unrevoked machine of that token's owner in that org. The server checks the signature with
     `hushrun_attest::verify::signed_by_one_of`, the same library readers use. Such a caller is
     a writer for objects under `v1/` only (L-6): no configuration, no deletion.
   - **Ledger reads.** A personal token of a member is a reader of the org's ledgers, and an
     agent token a reader of the ledgers its scope covers, so `hushrun status` and `plan` on a
     laptop or in a sandbox need no ledger token.
   - **Ledger API tokens** (`hrl_`) stay, for CI without ID tokens the backend accepts and for
     monitors, as L-6 has them. People no longer need them.

5. **hushrun signs with the machine key by default on a person's machine (amends T-12 and ADR
   0018 item 18).** Without `HUSHRUN_SIGNING`, outside CI, on a machine that has its key and no
   `HUSHRUN_SIGNING_KEY` or `HUSHRUN_SIGNING_KEY_FILE`, events are signed with the machine key
   (`HUSHRUN_SIGNING=machine` names it), and `hushrun approve` too. The keyless browser flow
   stays, with `HUSHRUN_SIGNING=sigstore`, for people who want their email as the identity. In
   CI a job with an ID token signs keylessly by default (ADR 0026 item 7).

   The token travels with requests as C-13 says: to its own origin only. Outside CI, when the
   environment names no ledger token, a ledger gets the login saved for its origin, or an agent
   token when the ledger is on hushrun's host (ADR 0027 item 6), even when only `hushrun.yaml`
   names it, since such a token is useless anywhere else; `HUSHRUN_LEDGER_TOKEN` keeps T-4's
   rule.

### What a machine's writes count for

6. **A machine counts for a promotion only where the repository's trust names its key.** This is
   the safeguard, and it needs no new reader code:
   - Readers that decide (`plan`, `guard`, `approve`'s check, `mark`, `status`) count an event
     only when trust accepts the principal whose key signed it (E-8, T-2). A machine key is a
     key: it counts where a reviewed commit lists it under a principal's `keys`, with the
     custody that principal may claim (`local`, or `isolated` for runs `run --isolated` makes).
   - So the server's registry can't add a signer. A server that accepted a write from a key it
     made up, or from a machine it was told to revoke, stores an event that no reader counts,
     unless trust names that key; and a key trust names was put there by a reviewed merge.
   - Production-like stages stay CI-only by default (ADR 0026): init writes no laptop, agent or
     approver principal, and registering a machine changes no trust.
   - `hushrun trust add <principal> --this-machine` prints, and with `--write` edits into
     `hushrun.yaml`, the lines that list this machine's key under a principal, for a pull
     request. The dashboard's machines list offers the same lines, and with the GitHub App opens
     the pull request.

7. **What the server's registry decides, and what it doesn't.**

   | The server could | What it changes |
   |---|---|
   | register a machine for a person it made up, or a key nobody holds | who may write; nothing a reader counts |
   | accept a write it should have refused (a revoked machine, an unregistered key) | an event readers ignore, unless trust names the key |
   | refuse, delay or withhold writes and reads | as ADR 0026 item 8 says, with the log and witnesses |
   | answer a result lookup with a pass nobody recorded | one skipped run of a check (C-10), which ADR 0023 item 2 already trusts the server with |

   Revocations go the safe way: honoring one only takes trust away, so a later release may have
   readers drop events a key signed after the server says it was revoked, by trusted time,
   without giving the server any power it lacks.

8. **Removing a person from trust is still a commit.** Revoking their machine at the server stops
   its writes at once; events it signed before keep counting, as a retired key's do (T-11),
   which is right, since they were made while the person was trusted. When a revoked machine's
   key is in trust, the dashboard says so and offers the lines that remove it, and `doctor`
   warns.

### init and agents

9. **init registers the first machine.** Its sign-in is the first, so the person running init has
   a registered machine before init shows its plan. It writes no principal for it (item 6).

10. **An agent's machine comes with its token, when it should count.** An agent token (ADR 0027)
    registers the key of each machine it runs on with its first request (item 2), so its results
    and, signed, its writes are attributed and revocable. A sandbox that keeps no files makes a
    new key each time; those machines write, and count for nothing in trust. When an agent's
    evidence should count at a stage, `hushrun token create --agent <name> --with-key` also
    makes one key for the agent, registers it with the token as `agent:<name>`, and prints it
    once, to set as `HUSHRUN_MACHINE_KEY`, so every sandbox of that agent signs as one machine
    that a commit can name (ADR 0018's agent principal).

## Consequences

**Gained:**
- No `hrl_` token or key file to make, paste and rotate for a person or an agent: signing in is
  the whole setup, on every machine.
- Every result and every event from outside CI says which machine made it, and each machine can be
  revoked on its own, from the dashboard.
- A leaked bearer token alone writes nothing: the writes need the machine key, which never leaves
  the machine, and later may not be able to.
- People approve and record stage evidence without publishing their email in Sigstore's log (D6).
- The server still can't add a signer that a promotion counts: the repository names keys, as it
  names CI's identities.

**Lost, or not done:**
- **A commit per machine that should count for a promotion.** A team that wants every member's
  laptop to count at staging lists each laptop's key. That is a decision about people, which ADR
  0026 already leaves to a reviewed commit; checks without stages (C-10) need no trust, and
  laptops' passes already skip CI there.
- **Results stay the server's word.** A machine signature says which machine sent a result, but a
  reader of results (C-10) asks the server, as ADR 0023 item 2 accepts.
- **A machine is as safe as its account and disk.** The key sits next to the token in the user's
  config directory; malware running as the user takes both. Hardware-held keys are the later
  answer.
- **One more request at sign-in for old tokens and agents** (`/v0/machines`), once per machine and
  token.

**Contracts:** C-18; `machine` and `proof` in the device flow; `POST {base}/v0/machines`;
`Hushrun-Signature` on results; personal and agent tokens as ledger writers under L-10;
`HUSHRUN_MACHINE_KEY`, `HUSHRUN_MACHINE_KEY_FILE`, `HUSHRUN_SIGNING=machine`;
`hushrun trust add --this-machine`; `hushrun token create --with-key`;
`hushrun_attest::verify::signed_by_one_of`. Servers without machines keep working with old and
new CLIs for one release.

## Alternatives considered

- **Readers count every machine the server registered, with registrations in the ledger's
  transparency log (ADR 0017) as the public check.** The server would write each registration and
  revocation into each ledger's log, and trust would say "machines of members count at staging".
  It saves the commit per machine, at a price:
  - The log detects, it doesn't prevent: the server can register a machine for a member it made
    up, and only a monitor someone runs, which compares the registrations with who should be
    there, notices it, after the deploy.
  - Without witnesses, which init doesn't add (ADR 0026, decision 5), the server can show a
    deploy's reader a history with the registration and the monitor one without; detection then
    waits for the two histories to meet.
  - Membership itself isn't in the log, so "a member" stays the server's word.
  - Revocation becomes proving that something is absent, which needs H2's map proofs.
  - Readers, the monitor and the ledger need new objects and checks.

  Trust naming the key prevents instead of detecting, with no new reader code. The log route can
  come later, for teams that want every member's machine to count without commits, and then it
  should require witnesses.
- **An admin approves each machine as a trust change, signed in the browser** (RFC 0002 §3.8).
  It needed the root, which ADR 0026 removes; a commit is the reviewed change now.
- **Sigstore with the person's email** for laptops. Their email goes into a public log (D6), and
  each signature needs a browser sign-in.
- **The token as the signature.** A bearer token proves nothing about where a request came from,
  and the server sees it on every request, so it could sign as its owner.
- **One key per origin or per org.** It would keep a machine's sign-ins to two servers apart, but
  a reader names a machine, not a sign-in; one key keeps "this machine" one thing, and its
  public half reveals nothing.
- **Ed25519 keys.** Simpler, and what `hushrun keys generate` makes by default; but no platform key
  store holds them, so moving to hardware later would change every machine's key and key id.
- **Deriving the agent's key from its token.** One secret instead of two, but the server sees the
  token, so it would have to see only a derived half, and ADR 0027's token format was already
  settled.

## Amendment 1: no unsigned results, one key per agent, people's machines tidied (2026-10-28)

Item 4 let hushrun cloud take a person's or an agent's result without `Hushrun-Signature` for
one release, marked unsigned in the dashboard. Nothing was deployed with it, so that release
never came, and until it did a bearer token alone recorded results as its owner from anywhere,
the gap item 4 set out to close. Items 2 and 10 had an agent token register the key of each
machine it runs on, with its first request. A fleet in thousands of sandboxes, each making a new
key because it keeps no files, would register thousands of machines: rows that count as the
agent's signers (ADR 0031), stay in every signers answer, and never sign again. And only a person
ever retired a person's machine, so a laptop sold or reinstalled stayed a signer of every org it
had signed in to.

The decision: a person's or an agent's result is signed, always; an agent has one key, made
with its token; and the server retires people's machines that go unused.

1. **Unsigned results are refused** (amends item 4). `POST {base}/v0/results/{key}` with a
   personal token (`hru_`) or an agent token (`hra_`) and no `Hushrun-Signature` gets 401
   `machine_required`, before the server reads, meters or stores the body: nothing is recorded.
   There is no grace period, since nothing was deployed. CI's ID tokens are unchanged: they never
   sign, the header is ignored for them, and the dashboard's "unsigned" marking goes, since a
   result without a machine is now CI's.

2. **hushrun signs, or doesn't send** (C-18). With a personal or an agent token, `hushrun run`
   always signs its results. A person signs with this machine's key; an agent only with the key
   its environment names, `HUSHRUN_MACHINE_KEY` or `HUSHRUN_MACHINE_KEY_FILE`, never a person's
   key file left on the machine. Without a key, hushrun doesn't send the result, and the run's
   last line says it wasn't recorded and what to do:
   - a person's token: `` this machine isn't registered: run `hushrun login` ``;
   - an agent token: `` set `HUSHRUN_MACHINE_KEY` to the key `hushrun token create --agent` printed ``.

   A server's 401 `machine_required` or `machine_unknown` gets the same line. The command still
   runs, and hushrun exits with its code: results aren't evidence (ADR 0023 item 2).

3. **An agent token comes with its one key** (amends items 2 and 10). Whoever makes the token
   makes the key, where the token is shown, and registers its public half in the same request:
   `hushrun token create --agent <name>` makes a P-256 key, as `--with-key` did, which is gone;
   the dashboard's Agents page makes an Ed25519 key in the browser, with WebCrypto. Both carry
   `machine: {"key", "proof"}`, the proof the key's signature over
   `hushrun-agent-machine/v1\n<org>\n<agent>\n<key id>\n`, and the server makes the token and its
   machine, named `agent:<name>`, in one transaction. The private key is shown once, beside the
   token, to set as `HUSHRUN_MACHINE_KEY`; the server never sees it, and the dashboard keeps no
   copy. So:
   - a sandbox never registers a key of its own: `POST {base}/v0/machines` refuses an agent token
     with 403, and a result an agent token sends signed by any other key gets 401
     `machine_unknown`, with no machine made;
   - every sandbox of an agent signs as the one machine its token came with, which item 10 asked
     for when an agent's evidence should count, and which is now the only way;
   - the database holds at most one live machine per agent token. Revoking the token revokes it,
     as before.

4. **People's unused machines are revoked** (amends item 3). A scheduled job of the server revokes
   a person's machine whose last use is more than 90 days old: a sign-in that registered it
   again, a signed result, a signed ledger write, or an approval in the dashboard signed with it;
   a machine never used counts from its registration. The reason is `unused`, by the system, in
   the audit log. Agents' machines aren't swept: their tokens expire within a year.

5. **A revocation says why, and only two reasons come back.** The server keeps each machine's
   reason (`revoked_reason`): `logout`, `dashboard` (its owner on their account page, or an admin
   on the org's), `token_revoked`, `agent_token_revoked`, `member_removed`, `member_left` or
   `unused`. Signing in again on the same machine, as the same person with the same key, brings
   back one revoked as `unused`, and, as item 3 always said, one its owner signed out of with
   `hushrun logout`, so that `hushrun logout` and then `hushrun login` keep working. Any other
   stays revoked, and the sign-in gets 409 `machine_revoked`: someone decided against that
   machine, or against its owner's place in the org, and a sign-in mustn't undo it. Such a
   machine signs in again with a new key, which hushrun makes once its key file is removed.

   A machine that comes back is live as if it had never been revoked: the signers answer (ADR
   0031 item 6) lists it without a revocation, so what it signed while revoked counts too. That
   is harmless for `unused` and `logout`, which say nothing against the machine, and it is why no
   other reason comes back.

6. **At most 20 live machines per person per org.** Registering, or bringing back, a 21st gets
   409 `machine_limit`, which names the dashboard's machines page, where the person revokes one
   they no longer use; `hushrun login` shows the message. A browser's key counts like a
   terminal's. The cap bounds what one account adds to an org's signers; with item 4, it rarely
   binds.

Consequences:

- A leaked personal or agent token records nothing by itself, with no release in between.
- A fleet adds one machine to the org's signers, however many sandboxes it runs. A sandbox takes
  two variables, `HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY`, both from one
  `hushrun token create --agent` or the Agents page, and both shown once.
- A laptop unused for 90 days gets the line of item 2 on its next run, and records again after
  `hushrun login`. A person removed from an org and invited back, or whose machine was revoked
  from the dashboard, signs in on that machine with a new key, and then signs in again in their
  other orgs, since one key serves them all (item 1).
- A database from before keeps each agent token's earliest live machine, and the migration
  revokes the others (`extra_agent_machine`); reasons of earlier revocations come from the audit
  log. Nothing was deployed, so this touches test data only.

Alternatives considered:

- **Keep sandboxes registering their keys, and sweep agents' machines too.** The rows would still
  churn through every signers answer, and an agent's evidence would count from keys no one can
  name.
- **The server makes the agent's key and returns it.** The server would hold, if only for a
  moment, a private key whose signatures count, which hushrun cloud never does (RFC 0001's CP-3).
- **A grace period for unsigned results.** Nothing was deployed, so nobody needs one.
- **Every revoked machine comes back at sign-in, as before.** An admin's revocation would last
  until the machine's next `hushrun login`.
- **No cap.** One account could add any number of machines to every org's signers.
