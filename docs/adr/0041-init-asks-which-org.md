# 0041. Init asks which org a repository is for

- **Status:** accepted
- **Date:** 2026-11-01
- **Rules:** C-13, C-15, C-18; results-v0 §5

## Context

`hushrun init` chose an org only through the approval page. That page runs during a sign-in, so it
ran once. From then on, with a login saved, init took the backend the last sign-in left behind and
said nothing. Someone who belongs to several orgs, as anyone with repositories under several GitHub
organizations might, had every repository added to one org, its checks counted against that org's
plan, and no prompt to catch it. The way around it, `hushrun login <org URL>` before each init,
existed but nothing pointed to it.

Fixing that uncovered a second fault. A machine is registered per org (results-v0 §6), and the CLI
treated it as registered once. It skipped registering when the sign-in had registered this key,
whichever org that was for, and it remembered a registration by token alone. So a person's second
org would never get the machine, and its records would be refused with `machine_unknown`.

## Decision

1. **The repository's files decide first.** A repository that names a backend keeps it, and nothing
   asks. A saved login no longer decides for a repository that names none.
2. **`GET {origin}/cli/orgs`, with a personal token,** answers the person's orgs, by slug, with their
   role, and the dashboard's URL. It is a read of what the token can already reach, so no sign-in
   is needed to ask. A token it doesn't take is 401, and init then signs in again. Only a 404 or
   405, a backend from before the list, keeps the last org. Any other failure stops init, because
   taking the last org when the list can't be read is this same bug during an outage.
3. **With a login saved and a repository that names no backend,** init takes the person's only org.
   With several it shows them and asks, by number or slug. Enter takes the org named like the
   repository's owner, else the org saved as the last one. With neither, Enter takes nothing, and
   the end of the input stops init.
4. **`--org <slug>` says it,** for scripts and agents, and for anyone who knows.
5. **With `--yes` or no terminal, init doesn't guess.** The org named like the repository's owner is
   as sure as it gets without asking, because the person belongs to it and it shares the owner's
   name, so init takes it and says so. Otherwise it exits 2 and lists the orgs, before changing
   anything.
6. **`--org` takes one of the person's orgs.** For any other, init exits 2 and changes nothing, so a
   mistyped slug can't reach a repository's files. Asking to join stays what it was: for a
   repository whose files name an org the person isn't in.
7. **`--org` moves a repository that records to another org,** after asking, unless `--yes`. The new
   org comes first: it gives the ledger and connects the repository, and a refusal leaves every
   file as it was, so a member who can't add the repository is told before anything is asked. A
   server that gives no ledger can't take a move, because the files can't lose the old URL without
   a new one.
   Then init rewrites `backend:` in hushrun.yaml and `hushrun.backend` in package.json, and replaces
   the URL and log key the old org gave, which name its ledger. hushrun.yaml goes first: package.json
   names the backend over it, so stopping between the two still reads as a move to run again. Only
   `--org` moves: otherwise repositories of a team would be one mistaken Enter from leaving it.
8. **A machine registers once per token and org.** The login keeps the org its sign-in registered
   the machine in (`machine_org`), which is the only org that needs no registration. Init registers
   it in the org it set the repository up in, once that is agreed, because signed events are
   refused without it, and they can come before any result. The first write of a result still does
   it for any other org. A login from
   before this registers once in each org, and is answered 200 where it already was.
9. **The last org stays the last org.** Choosing one for a repository doesn't change the backend
   `hushrun login` saved, which repositories naming none still use, and which `hushrun login`
   alone changes.

## Consequences

- People with one org see nothing new.
- `npx hushrun init --yes` can now stop with exit 2. It stops only for someone in several orgs, none
  named like the repository's owner, and the message has what to add. An agent should ask the
  person, as its docs say.
- A plan (`--dry-run`, or no terminal without `--yes`) now reads the list when a login exists, within
  10 seconds. It remains no sign-in and changes nothing, and it says when init would ask. A real
  init gives the list as long as its other requests, so a slow start of the server doesn't stop it.
- An outage of the list stops init for everyone with a saved login in a repository that names no
  org, even people with one org. `--org` gets past it.
- A new CLI against a backend from before this gets 404 and keeps the last org: nothing breaks in
  either order of deploying, but the fix needs both. The backend goes first.
- A moved repository's old org keeps its results and its ledger, and history doesn't follow:
  results are per org, so its first run in the new org runs in full. There is no way yet to remove
  a repository from an org.
- Repositories of a team are unaffected by a teammate's choice: the choice is committed, and the
  next person's init reads it.
- `machine_org` is a new optional field of the credentials file. Older CLIs ignore it.

## Alternatives considered

- **Always sign in again, so the approval page chooses.** No new endpoint, but a browser and a
  token per repository, for someone setting up many.
- **Take the last org, and say which.** One line in a long output is how this went unnoticed, and a
  committed URL and a plan's usage are costly to get wrong.
- **Refuse `--yes` with several orgs, always.** Safe, but it stops the common case for no gain: a
  repository under `acme` for someone in the org `acme`.
- **Ask in a repository that already names an org.** Asking on every run would train people to press
  Enter, and it would invite moving a team's repository by accident.
- **Move a repository by editing its files by hand.** `init` doesn't connect a repository already set
  up, so CI's ID tokens would be refused in the new org, and hushrun.yaml would keep the old ledger.
