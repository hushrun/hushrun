# 0031. Membership is trust

- **Status:** accepted; amended (amendment 1: CI-only stages are set on the server; amendment 2:
  every period a stage was CI-only counts; amendment 3: approvals count only from people)
- **Superseded in part by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-23
- **Rules:** T-15 (new), T-16 (new), L-13 (new); T-4, T-8, T-9, T-12, T-13, C-3, C-7, C-8, C-12,
  C-15, C-18, E-4, E-8, E-10, L-1, L-2, L-6, L-9, L-10, L-12, P-1, P-2, P-3, P-9, P-12, P-13,
  I-9. Removed: T-1, T-2, T-3, T-6, T-7, T-10, T-11, T-14, P-16, P-17, O-1 to O-8
- **Supersedes:** [0026](0026-trust-anchored-in-the-repository.md)'s trust: where it lives
  (items 1 to 10), what init writes about it (items 11 to 16, the ruleset and the consent),
  item 18 and amendment 1 (the organization's trust); [0015](0015-org-policy-and-constraints.md)
  whole; [0014](0014-key-windows-and-retirement.md) (windows and retired keys).
  **Amends:** [0028](0028-machines-registered-at-sign-in.md) (a registered machine counts by
  itself); [0030](0030-final-setting-names-without-aliases.md) (`org.yaml` names and
  `repository inventory`); [0018](0018-local-and-agent-evidence.md) items on protected paths,
  audits and `max_attempts`; [0017](0017-transparency-log.md) and
  [0020](0020-proven-listings-and-witnesses.md) (where the log key and the witnesses are named);
  [0013](0013-keyless-signing.md) (identities in trust).

## Context

ADR 0026 put trust in the repository: a `trust:` section in `hushrun.yaml`, read from the
default branch, listing principals, their keys and identities, and which principal each stage
accepts. ADR 0028 then registered every machine at sign-in, and ADR 0027 gave agents tokens with
machines of their own. That left two lists of the same people: the org's members on the server,
whose machines the server already refuses writes from when they aren't registered, and the
principals in `trust:`, which someone had to keep in step by pull request (`hushrun trust add
--this-machine --pr`, `org apply`). Every new teammate, laptop and agent meant a trust PR before
their evidence counted, and every departure meant another one before it stopped. The trust page,
the trust PRs, the default-branch fetch, the ruleset init asked GitHub for, and the organization
policy compiled into each repository's section all existed to maintain that second list.

There was a deadlock too: the server submits checkpoints to witnesses, but the witnesses were
named only in the default branch's `trust:` section, which the server could read only through a
GitHub App. Without one, a server could not learn its own witnesses.

## Decision

### Who counts

1. **Membership is trust (T-15).** An event counts when it is signed by:
   - a machine registered at sign-in (ADR 0028) of a current member of the repository's org, or
     of one of the org's agent tokens whose scope is the org or this repository (ADR 0027), or a
     browser key the dashboard registers as the approving person's machine; or
   - the repository's own CI, through keyless OIDC: a certificate whose issuer the server lists
     for the repository and whose Source Repository Identifier is the repository's numeric id
     (GitHub Actions with the id GitHub gives the repository; any other CI identity the server
     verifies the same way). A `pull_request_target` certificate is never the CI's.
2. **Signers are labelled**, not named in a file: `ci`, `user:<email>` or `agent:<name>`. The
   highest custody a machine may claim is `isolated`; only `ci` claims `ci`.
3. **Revocation stops the future, not the past.** Logging out, removing a member, revoking an
   agent token or a machine makes the server keep the machine with the time it was revoked. A
   timestamped signature (RFC 3161, E-10) counts when its trusted time is before that;
   an untimestamped one counts only while the machine isn't revoked. So evidence made while a
   machine was trusted keeps counting, and nothing signed after does. This is T-11's window rule,
   with the server setting the window's end.
4. **No principals, accept maps, trust PRs, trust editor or default-branch reads.** `trust:` is
   an unknown key; `--trust`, `--trust-from-tree`, `HUSHRUN_TRUST`, `HUSHRUN_TRUST_REF`,
   `hushrun trust`, `hushrun org`, `org.yaml`, `doctor --org`, `adopt --org` and
   `repository inventory` are gone. Nothing was released, so nothing is kept for compatibility.

### Stages

5. **One constraint stays: `accept: ci` (T-16).** *Amendment 1 moves it to the server.* A stage that says it counts checks, artifacts,
   and its own and upstream lifecycle events only from the repository's CI. Approvals are never
   the CI's, at any stage: they count from any member's machine. (Amendment 3: from people only,
   never an agent's machine.) This replaces `min_custody`,
   per-check `accept` lists, `events@` entries and protected paths, which existed to keep laptop
   evidence out of production; P-16 (protected paths) and P-17 (audits) go with them, as do
   `max_attempts`, `locations` and `constraints`.

### How readers learn the signers

6. **The server answers them (L-13).** `GET {repository_url}/signers`, any role, returns
   `{"signers": <hushrun.signers/v1 document>, "statement": <signed note>}`. The document lists
   the CI identities and every machine, revoked ones included, with owner, registration and
   revocation times, and a `gc` flag for the machines of current admins and owners. The
   statement is a C2SP signed note by the log key over the SHA-256 of the document's JCS form.
7. **hushrun.yaml pins the log key**, at the top level (`transparency_log:`, T-9), read with the
   rest of the file from the evaluated revision (T-5). A reader checks the statement against it,
   so a server can't change who counts without signing it with the key the repository's history
   pins; without a pinned key, it warns and uses the answer unchecked.
8. **Offline verification uses the server's signed answer.** `hushrun repository signers --out
   FILE` saves it, and `hushrun verify --signers FILE` checks events against it with no network,
   after checking the statement against the pinned key.
9. **In CI**, when the provider reports the repository's id, an answer whose CI names another id
   exits 2. The repository's URL comes from the environment, the base, else the evaluated
   `hushrun.yaml`, which gets the job's ID token and never a bearer token (T-4).
10. **GC statements** are signed by a machine the answer marks `gc` (L-9).

### Where the trust is

11. **Trust is placed in the server**, as ADR 0023 item 2 places a repository's results:
    whoever runs the org's server decides who its members are, and so whose evidence counts. A
    compromised server could add a machine and sign events with it. What it can't do silently:
    change an answer without the log key, which signs every answer and every checkpoint, so a
    reader that saved an answer, and a monitor that replays the log, holds proof of what the
    server said; delete or rewrite history (L-11, L-12); or make CI evidence out of a machine's,
    since `ci` is only a certificate for the repository's id. A stage that wants evidence no
    laptop and no server-added machine can make says `accept: ci`. (Amendment 1: is made CI-only
    on the server; the rule travels in the signed answer.)

### Witnesses

12. **The server holds the witnesses it submits to**, set by the org's admins (api-v1, the
    repository page) and stated in `GET {base}` as `log.witnesses`. That removes the deadlock:
    the server needs nothing from the repository to start witnessing. `hushrun.yaml`'s
    `transparency_log.witnesses` stays what readers require (T-13), and `hushrun doctor` warns
    when the two lists differ.

### Init

13. **init writes no trust.** It writes `hushrun.yaml` with `repository`, `repository_url` and
    the log key the server tells the signed-in person, asks GitHub for no ruleset and glab for
    nothing, and needs no consent screen for Sigstore's public log beyond what CI signing
    already implies. The next step is to commit and push.

## Consequences

- A new teammate's or agent's evidence counts from their first sign-in; a departure stops
  counting at removal, with no pull request either way.
- The repository's history no longer records who may sign; the server's signed answers and its
  audit log do. `--explain-json` carries the answer a plan used.
- The cloud loses its trust index, the trust endpoints and PRs, and the
  Trust page; the repository page shows who counts and holds the witnesses. A migration drops
  the trust tables and columns and keeps the transparency log and its witnesses' history.
- An organization that wanted the same rules everywhere gets them from membership and
  `accept: ci`, without compiling anything into each repository.

## Open questions

- ~~Should `accept: ci` also be settable by the server (an org-wide policy), so that a repository
  can't drop it in a pull request?~~ Yes, and only there: amendment 1.
- Should the CI identity be narrowed to a workflow or a branch, as T-10 allowed? Today any
  workflow of the repository is its CI, except `pull_request_target`.
- Should a self-hosted reader be able to require a statement (refuse an unpinned answer)
  instead of warning?

## Alternatives considered

- **Keep `trust:` and generate it from membership.** Still a second list, and still a pull
  request per change.
- **Read the witnesses from the default branch through the GitHub App.** Keeps the deadlock for
  every repository without the App, and every host but GitHub.

## Amendment 1: CI-only stages are set on the server (2026-10-23)

Item 5 kept `accept: ci` in `hushrun.yaml`, read from the evaluated revision (T-5). That puts the
rule in the change it governs: a pull request that drops `accept: ci` from production is
evaluated under its own `hushrun.yaml`, so the first deploy of that change already takes a
laptop's evidence. Reviewing it is the only defence, and the rule exists for the stages where a
review isn't enough. The maintainers decided the rule belongs with membership, on the server, where
the org's admins already decide whose evidence counts.

1. **`accept` is gone from `hushrun.yaml`**: an unknown key (C-7, C-8), with no alias. Nothing was
   released with it. `init --pipeline`, `adopt`, the recipes and the schema don't write it.
2. **The server holds the rule, in two places** (api-v1):
   - **the org**: stage names that are CI-only in every repository of the org (such as
     `production`), set on the Members page, where the org already decides who signs;
   - **a repository**: the stages it marks, set on its page beside "Whose events count".
   A stage is CI-only when the org lists it or the repository marks it; the repository page
   shows the org's as locked. Only admins and owners change either; members read them. Each
   change is audited (`org.settings_changed`, `ledger.settings_changed`).
3. **Readers learn it from the signers answer** (L-13): `ci_only: [{"stage", "since"}]`, signed
   by the log key with the rest of the document, so a server can't drop or add the rule without
   the key hushrun.yaml pins. `since` is when the stage last became CI-only without a break
   (the earliest of the org's and the repository's, while both hold). A saved answer carries it
   to `verify --signers` offline, and `--explain-json` records it as the plan's input.
4. **Earlier events: the revocation rule** (T-15 item 3). An event someone other than the CI
   signed counts at a CI-only stage when its trusted time (E-10) is before `since`, and never
   without a trusted timestamp. Evidence made while the stage took anyone's keeps counting; a
   laptop can't backdate into it, since its own `time` doesn't decide. Lifting the rule counts
   everyone's events again; setting it again starts a new `since`, so a non-CI event
   timestamped during an earlier CI-only period counts again then. Keeping every period would
   close that, at the cost of a history in every answer; an admin who lifts the rule is audited.
   (Amendment 2 keeps every period.)
5. **The server refuses** a member's or an agent's machine-signed write of a non-approval event
   stored at a CI-only stage (`v1/events/…/<kind>.<stage>.json`, `v1/deployed/<scope>/<stage>/`)
   with 403, naming the setting. It is a convenience: readers decide by the answer, a source
   check stored without a stage can't be refused (it may count elsewhere), and the server's
   index, which applies the same rule, recomputes the org's or the repository's states when the
   rule changes.

Consequences: a repository's history no longer shows whether production was CI-only for a
deploy; the signed answer in `--explain-json` and the audit log do. A server that wants to drop
the rule must sign an answer without it, which a saved answer and the log's monitors expose.

## Amendment 2: every period a stage was CI-only counts (2026-10-23)

Amendment 1 item 4 kept one `since` per stage: lifting the rule and setting it again started a
new one, so an event a laptop signed, with a trusted timestamp, during the earlier CI-only period
counted again once the rule was back, which is when the stage is supposed to refuse it. The maintainers
decided to close that: the server keeps each stage's whole history, and the answer carries it.

1. **The server keeps periods, not a `since`** (api-v1). Each time an admin sets a stage CI-only,
   for the org or a repository, a period opens; lifting it closes the period, with who did each.
   A stage set again opens a new period. Periods are never deleted (but with their org or
   repository), and each change stays audited.
2. **The answer carries them** (L-13): `ci_only: [{"stage", "periods": [{"from", "until"?}]}]`,
   one entry per stage that is or was CI-only, sorted. A repository's periods for a stage are
   the union of the org's and its own: overlapping or touching periods merge, so a stage the org
   lists while the repository also marks it is one period from the earlier start to the later
   end. `until` is absent while the stage is CI-only. A reader refuses periods that overlap,
   touch, run backwards, are out of order or are open before the last, as it refuses a stage
   listed twice. Nothing was released with amendment 1's `since`, so it is gone, not kept.
3. **The rule** (T-16). An event someone other than the CI signed doesn't count at a stage:
   - with a trusted time (E-10), when that time falls in one of the stage's periods, from its
     start (included) to its end (excluded). Before the first period, between two, and after the
     last one ended, it counts, as it does at a stage that never was CI-only. So amendment 1's
     semantics hold for events before the first period, and evidence made during any period
     never counts there, whatever happened since;
   - without a trusted time, while the stage is CI-only now. Such an event could have been made
     at any time, now included; while the stage takes anyone's evidence, its signer could sign
     the same event again and it would count, so refusing the old one would protect nothing.
     This is amendment 1's rule for untimestamped events, unchanged.
4. **Everything that applies the rule applies periods**: `plan`, `run`, `mark`, `artifact
   resolve`, `approve`, `status`, `repository gc` and `verify` (whose `notCountingAt` names a
   stage that was CI-only when the event was made, even if it isn't now); the server's refusal of
   a machine-signed write (still only while the stage is CI-only now, since the write is made
   now) and its index, which recomputes on every change as before.

Consequences: answers grow by one period per time an admin set a stage CI-only, which is rare.
The repository page and the Members page show "CI only since …" for the current period and a
line with the earlier ones. A stage lifted for good stays in every answer, closed; its history is
part of what the log key signs.

Alternatives considered: refusing untimestamped events at a stage that ever was CI-only (sound
too, but it makes lifting the rule partial for good, with no gain, by item 3's argument);
bounding an untimestamped event by its machine's registration (a key exists before it is
registered, so it bounds nothing); forgetting periods older than some age (a server could then
let an old event count by waiting, which is what this amendment removes).

## Amendment 3: approvals count only from people (2026-10-27)

Item 1 made the machines of the org's agent tokens signers, and item 5 said approvals count from
any member's machine, but P-3 only kept the CI out: it took an approval from any signer but `ci`.
So an agent token made with `hushrun token create --agent NAME --with-key` (since ADR 0028
amendment 1, every agent token comes with its key, and the flag is gone), signing with its
machine key, could `hushrun approve`, and its approval satisfied the check; `policy explain` even
said approvals came "from members' machines and agents". An approval is a person's decision that a
digest may deploy, and an agent that wrote the change approving it defeats the check. The maintainers
decided that approvals count only from people.

1. **P-3 takes people's approvals only**: events whose signer is `user:<email>`, a machine a member
   registered at sign-in or the browser key the dashboard registers when a member approves there.
   An agent's machine (`agent:<name>`) and the CI (`ci`) never approve, at any stage, CI-only or
   not. Everything else an agent signs counts as before (T-15, T-16).
2. **Every reader agrees.** `plan`, `run`, `status`, `approve`'s check and hushrun cloud's approvals
   page decide by P-3 (`hushrun_core::signers::is_person`); `verify` names an approval's stage in
   `notCountingAt` when no person signed it; `policy explain` says approvals come from people
   only.
3. **`hushrun approve` refuses to sign as an agent**, as it refuses in CI: with an agent token in
   `HUSHRUN_TOKEN`, before it reads or signs anything, and when the server names the signing key
   (a key file, or `HUSHRUN_MACHINE_KEY`) an agent's machine. It exits 2 and records nothing, and
   with `--out` writes nothing.
4. **hushrun cloud refuses an agent token's write of an approval event**
   (`v1/events/…/<ULID>.approval.<stage>.json`) with 403 `denied`, and its dashboard appends only
   an approval a person signed. Like the CI-only refusal, this fails early: readers decide by P-3
   either way, and an agent's approval that reached a ledger still doesn't count.

Consequences: an agent can't stand in for a reviewer, and an approval an agent recorded before
stops counting. What still lets a non-person approve is a person's key in an agent's hands: an
agent that runs with a member's machine key, on their laptop or in a sandbox where they signed
in, signs as that member, and nothing in the signature tells the two apart.
