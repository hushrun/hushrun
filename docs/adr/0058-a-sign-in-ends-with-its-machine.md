# 0058. A sign-in ends with its machine, or after 90 days unused

- **Status:** accepted
- **Date:** 2026-11-07
- **Rules:** C-13, C-18
- **Amends:** [0028](0028-machines-registered-at-sign-in.md) amendment 1, under which the
  sweep of unused machines left personal tokens as they were

## Context

A personal token (`hru_…`, C-13) has no expiry. Only `hushrun logout` with that same token
revoked it, and revoking a machine on the dashboard's account page revoked the one token the
machine was bound to: the sign-in that registered it last. The 2026-11-06 audit of hushrun's
implicit state machines found three ways a token stayed live and unseen:

- **A newer sign-in on the same machine.** `hushrun login` again on a machine moves its machine
  to the new token and overwrites the old one in `credentials.json`, but never revokes it.
  Revoking the machine then left the older token live.
- **A sign-in approved for no org.** It registers no machine, so no machine names it, and the
  dashboard had nothing to revoke it with.
- **A sign-in nobody uses.** The sweep revoked machines unused for 90 days, never tokens, and a
  token kept working for reads, for connecting repositories and for registering a machine.

## Decision

1. **A token keeps the machine key its sign-in proved** (C-18): the sign-in's `machine` and
   `proof`, or, for a token whose sign-in had none, the first key it registers with
   `POST {base}/v0/machines`.
2. **Revoking one's machine signs out every sign-in on it.** Revoking a machine on the account
   page revokes every live token of the person that proved its key, and each one's other
   machines (`token_revoked`), not only the token it was bound to. An admin revoking a person's
   machine on the org's page still leaves their sign-ins as they are: they work in the person's
   other orgs.
3. **The sweep of unused machines revokes unused tokens too.** A token with no use in 90 days
   (its `last_used_at`, else when it was given) is revoked, with the machines it registered, as
   `unused`. `hushrun login` signs in again and brings those machines back, as it brings back a
   machine swept on its own.

## Consequences

- Someone who doesn't use hushrun for 90 days runs `hushrun login` again; until then the server
  answers 401 and hushrun says to (C-14), as it does for a revoked token.
- A token a newer sign-in replaced on a machine, or one approved for no org, ends with that
  machine's revocation or 90 days after its last use, whichever comes first. A list of one's
  sign-ins to revoke each by itself is left for later.
- Tokens given before this release keep the key of a machine still bound to them; an older one,
  whose machine a newer sign-in took, has none and ends with the sweep.
