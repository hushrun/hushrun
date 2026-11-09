# Repository access control

A repository's events on hushrun must be append-only for CI (spec L-6). A pipeline that could
delete or overwrite an event could remove a `failed` verdict and unblock a digest that never
passed. hushrun verifies every event's signature, but a signature can't prove that an event is
*missing*. Only the server's own rules can.

A repository's URL is an `https://` URL (spec L-1, L-10), such as the hosted server's. Buckets
and directories are no longer supported ([ADR 0022](../adr/0022-https-ledgers-only-and-no-kms.md)).

What each identity may do:

| Identity | `v1/` (events and the deployed index) | Configuration | Delete |
|---|---|---|---|
| CI | create and read | read | never |
| A person or an agent, from a registered machine | create what the machine signed, and read | read | never |
| Org admins and owners | read | read | `repository gc` only (L-9) |

## Roles and tokens

The server enforces these rules itself, with a role per token: `reader` reads, `writer` also
appends under `v1/`, and `admin` also deletes. hushrun writes every event with
a conditional create (`If-None-Match: *`), so a well-behaved client never overwrites anything;
the roles make sure a misbehaving one can't either.

- **CI.** A GitHub Actions job with `permissions: id-token: write` sends an ID token whose
  audience is the repository's URL, and the server gives it the writer role for the repository it
  names: a pipeline can append events but never delete one. Elsewhere, such as on GitLab, the
  pipeline is one of the org's agents, with a token and the machine key it comes with, made by
  `hushrun token create --agent gitlab-ci --expires 365d`: the token in `HUSHRUN_TOKEN`, a
  masked, protected variable, and the key in `HUSHRUN_MACHINE_KEY_FILE`, a protected File
  variable (a PEM key can't be masked). The token goes to the repository's URL on hushrun's host,
  and hushrun signs with the key by default, so its events count as the agent's and append like
  any agent's ([the GitLab guide](gitlab.md)).
- **People and agents** ([ADR 0028](../adr/0028-machines-registered-at-sign-in.md)).
  `hushrun login` registers the machine's key, and an agent token comes with one key, registered
  when an admin makes the token. A member's login reads the org's repositories, an agent token
  reads those its scope covers, and either appends an event only when a registered machine of
  theirs signed it. hushrun signs with the machine's key by default, unless a GitHub Actions
  job's ID token signs, and outside CI it sends the login to repositories on its own origin.
  Revoking a sign-in, a member or an agent token revokes their machines.
- **Org admins and owners** run `repository gc` from their signed-in machines, and keep any
  admin token out of CI.
- Tokens from the environment go only to a URL that `HUSHRUN_REPOSITORY_URL` or `--repository-url`
  names (spec T-4); in CI without either, the URL `hushrun.yaml` names gets only the job's ID
  token. Two tokens go to their own host only, whoever names the URL: an agent token
  (`HUSHRUN_TOKEN`, `hra_…`) to a URL on hushrun's host, in CI too, since the server there takes
  it for its org only; and a login to its own origin, outside CI. No other token follows a URL
  that only `hushrun.yaml` names.

## Who counts

Membership is trust ([ADR 0031](../adr/0031-membership-is-trust.md)). An event counts when it
is signed by a registered machine of a current member or agent of the org, or by the
repository's own CI, keylessly (spec E-8, T-15). The server answers who those are
(`GET {repository_url}/signers`, L-13), and signs its answer with the log key `hushrun.yaml`
pins, so it can't change it silently. Removing a member, logging out or revoking an agent token
stops what they sign from counting; what they signed before keeps counting. A repository whose
deploys should count only CI's evidence is made CI-only in the dashboard, and the same signed
answer lists the periods it was (T-16): `hushrun policy explain` shows it, or `--signers FILE`
from a saved answer, offline.

## The transparency log

The hosted server also keeps a transparency log (spec L-11): every write is a leaf of an
append-only Merkle log, and the server signs its checkpoints, listings and signers answers with
a log key. Name that key in `hushrun.yaml` (`transparency_log`, T-9; `npx hushrun init`
writes it when the server tells it, `hushrun doctor` prints the lines), and hushrun
checks every write's receipt, every listing and every checkpoint, so that withholding an event
from a plan is found by the next run of `hushrun repository monitor`, and showing two histories
fails the plan that meets both. A deploy follows `plan --start`, which records the listings the
plan decided from, for monitors to check. Every checkpoint also commits to a map of each
listing's keys, and the monitor checks that every map follows from the log (spec L-12).

## Garbage collection

`hushrun repository gc --older-than 180d` deletes old events (L-9), which CI never may. Run it
as an org admin or owner, first with `--dry-run`. On a repository that keeps a log, it signs each
deletion with the admin's signed-in machine, which the server's signers answer marks for GC, so
that a deletion is never the server's own doing; monitors report any deletion without such a
signature.
