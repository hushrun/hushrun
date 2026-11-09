# 0025. `hushrun init` gets the repository its ledger

- **Status:** accepted
- **Date:** 2026-10-19
- **Rules:** C-12, C-15; L-1, L-10, which it relies on; ADR 0023's trust on first use

## Context

Promoting a repository's builds (§10) records into its hosted ledger,
`https://<host>/l/<org>/<ledger>`, one per repository (L-1). In hushrun cloud a person made it in
the dashboard, with the "New ledger" form on the overview and in the sidebar, or in the
Promotion setup guide. Since ADR 0024, `npx hushrun init` sets a repository up with one
command, but making its ledger stayed a form that only a dashboard session could submit, and
that asked for the GitHub repository by hand.

A hosted ledger takes CI's ID tokens (L-10) from one GitHub repository, which the server checks
by GitHub's numeric id for it (`repository_id`), not by its name: a repository deleted and made
again under the name is another repository. The form took the id from GitHub's public API when
nobody gave one, and that API can't see a private repository: such a ledger refused every ID
token until someone found the id. ADR 0023 learns a connected repository's id from its CI's
first ID token, since the CLI holds no GitHub credentials of its own.

## Decision

1. **init asks the org's backend for the repository's ledger** (C-12, C-15 step 6), after
   connecting it: `POST {base}/v0/ledgers` `{"repository", "github_repository_id"?}`, with the
   login's personal token. The backend answers the org's ledger for the repository: the one it
   has (200) or one it makes (201). It refuses an ID token: CI can't vouch for a repository.
2. **One ledger per repository, however often and by whomever it is asked.** The backend finds
   the ledger by GitHub's id for the repository when it knows one, else by the repository's
   name, ignoring case, and it takes one request at a time for a repository, so two teammates'
   init make one ledger. A repository already set up gets its ledger too: init signs in and
   asks, without a question, since nothing in the repository changes.
3. **Making a ledger takes an org admin; finding one doesn't**, as in the dashboard. A member's
   init finds the ledger an admin's init made; when there is none, it says that an admin's init
   makes it.
4. **The ledger is named after the repository**, as the GitHub App names one: the last segment,
   lowercase, other characters as `-`, `r-` in front unless it starts with a letter, at most 56
   characters; then `-2`, `-3`… while the org has a ledger of that name. The repository is kept
   as init names it, its host lowercase, because `hushrun.yaml`'s `repository` must match the
   ledger's exactly.
5. **GitHub's id for the repository comes from what GitHub says, and is never replaced.** First
   the org's own knowledge: its GitHub App's installations, else its CI's first ID token, which
   ADR 0023 pins for results. Else the id in the request, an admin's word, as the dashboard's form
   takes one: init reads it with `gh api repos/<owner>/<name> --jq .id` while the sign-in waits,
   through `gh`'s own sign-in, as C-16 does, so no token passes through hushrun. A ledger without
   an id takes one, once, from an admin's request; a ledger that still has none takes the id
   that the repository's CI pins with its first ID token to the org's results, in the same
   transaction. An id in the request that contradicts the org's, or a ledger of the repository
   bound to another id, is a 409 that changes nothing. The audit log records `ledger.created` and
   `ledger.linked`, by the person, or by the system for a pin.
6. **The ledger never fails init.** A refusal, a backend without the request (404 or 405) or an
   error is a warning, and the checks are set up all the same.
7. **The dashboard's "New ledger" form goes.** The GitHub App's adoption still makes the ledger
   of a repository it adopts. The Promotion setup guide, which made one too, has since gone from
   the dashboard as well.

## Consequences

- Every repository init sets up has its ledger, so promotion starts from one, and the dashboard
  lists it. An empty ledger is no sign that an org promotes builds: the dashboard's overview
  leads with the org's checks until a ledger records something or its setup begins.
- A private repository's ledger takes its CI's ID tokens as soon as an admin's init runs with
  `gh` signed in, else after its CI's first run with hushrun. A CI write to the ledger before
  that is refused, with ledger-http-v1's message.
- Trust on first use now binds ledgers that nothing else bound: the window ADR 0023 accepts for
  results, another repository taking a connected name before its first CI run, applies to such
  a ledger too. A bound ledger stays bound; binding it to another repository remains the App
  adoption's explicit `rebind_ledger`.
- A backend that speaks `hushrun-results/v0` without ledgers answers 404, and init says that
  promotion needs a ledger from that backend's own settings.

## Alternatives considered

- **Let `POST /api/v1/orgs/{org}/ledgers` take the CLI's token.** The dashboard's API
  authenticates a session with a CSRF header; bearer tokens there would widen every endpoint it
  has for one request. The results protocol's endpoints already take the CLI's token.
- **A command of its own, such as `hushrun ledger create`.** One more step per repository,
  which init exists to remove.
- **Only trust on first use.** The ledger would wait for CI's first run even when the person
  running init can read the id now.
- **Only `gh`.** Without it, which is common, the ledger would never take an id.
- **GitHub's public API in the backend, as the form does.** It can't see a private repository,
  and it would put a call to GitHub in init's path; the pin covers public repositories too.
- **Owner and name on a collision (`acme-shop`).** Clearer for forks, but the App's adoption
  names its ledgers `shop-2`; one rule keeps a repository's ledger name the same whichever way
  it was set up.
