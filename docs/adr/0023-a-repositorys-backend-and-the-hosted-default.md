# 0023. A repository's backend, and the hosted default

- **Status:** accepted
- **Date:** 2026-10-19
- **Rules:** C-9, C-11, C-12, C-13, C-14, T-4, L-10

## Context

`hushrun run NAME -- COMMAND` shares results through a backend (C-11), and the goal now is that a
new user gets there in under three minutes, with one command, `npx hushrun init`. Until now
each place that runs a check had to name the backend itself: `HUSHRUN_BACKEND` in each CI job,
`hushrun login <URL>` on each laptop, typed by a teammate who first has to find the org's URL.
CI's ID token counted only for a repository the org's GitHub App reaches or a hosted ledger is
linked to, both an org admin's steps. And every wrapped check repeated a name its
`package.json` script already has: `"test": "hushrun run unit -- vitest run"`.

Ledgers forbid the obvious shortcut. T-4 keeps the ledger out of the content under evaluation:
in CI, `HUSHRUN_LEDGER` comes from the CI's settings, and a token, an ID token included, goes
only to a ledger that `HUSHRUN_LEDGER` or `--ledger` names (L-10). Events decide deployments, so
a change that pointed them at another ledger could choose what a plan sees.

## Decision

1. **A repository names its backend** (C-11): `"hushrun": {"backend": "<URL>"}` in the
   `package.json` at the repository's root, committed with the code. It comes after
   `HUSHRUN_BACKEND` and before the backend `hushrun login` saved. A teammate who clones, an
   agent in a sandbox and CI all find it, with no variable.
2. **Results may follow the code, unlike evidence.** A result only decides whether a check of
   this exact code runs again; it gates nothing else. Whoever can change `package.json` can
   already change what `npm test` runs: a change that pointed results at a server that answers
   "pass" gains nothing over a change to the script, and both show in the same diff. What a
   file in the repository must not move is a credential, so the tokens follow rules it can't
   change (C-13):
   - **ID tokens name their audience.** The ID token a GitHub Actions job sends has the
     backend's URL as its audience, so a backend that a file names gets a token no other
     backend accepts. Unlike L-10's ledgers, it may go to a backend the repository names.
   - **Logins are per origin.** A personal token goes only to the origin that issued it, so a
     laptop's login never reaches a server that a file names elsewhere.
   - **`HUSHRUN_TOKEN` goes only to `HUSHRUN_BACKEND`,** as ledger tokens go only where
     `HUSHRUN_LEDGER` says. It is a bearer secret any server could replay, so it follows the
     CI's own settings and never a file. The backend can still come from the file, unlike
     T-4's ledger: a job with `HUSHRUN_TOKEN` and no `HUSHRUN_BACKEND` sends the token nowhere,
     and hushrun says so.
3. **Without a login, a laptop runs, and says how to sign in** (C-11, C-14). A repository's
   backend refuses requests without a token; hushrun runs the check unrecorded, as C-14 says,
   and at a terminal ends with one line, once a run: `hushrun: sign in to share results with
   <org>: npx hushrun login`. A script or a CI job, which has no one to sign in, never
   sees it.
4. **The hosted backend is the default** (C-13). `hushrun login` with nothing else to go on (no
   URL, `HUSHRUN_BACKEND`, repository backend or earlier login) signs in to
   `https://api.hushrun.com`, or `HUSHRUN_HOST`. Without URL, it says which of these it
   signs in to. The device flow sends the org it expects as a hint (`/o/<org>` in the URL it
   signs in to), the person picks the org on the approval page, and the token answer names it
   with its URL (`org`, `backend`), which becomes the backend. When hushrun signs in to an
   org's URL, that URL stays the backend, whatever the answer names. A server that names no
   org, or names its URL on another origin, where the token is no use, leaves results on the
   machine, and hushrun says so.
5. **A repository's CI connects without the App** (C-12). `POST {base}/v0/repositories`
   `{"repository": "github.com/acme/shop"}`, with a member's login, lets that repository's CI
   record results at the org with its own ID token: no secret, no GitHub App, no admin. An ID
   token can't connect a repository; `hushrun init` connects the one it sets up. hushrun cloud
   keeps the id lowercase and pins the GitHub repository's numeric id (`repository_id`) from the
   first ID token it accepts for it: trust on first use. A repository deleted and created
   again under the same name is then refused, with a message that says why. A server without
   the request answers 404 or 405, and hushrun points at the server's settings.
6. **A check is named after its script** (C-9). Without NAME, the check is
   `npm_lifecycle_event`, the script npm, pnpm, yarn and bun run (`test` for `npm test`), when
   it is a valid name other than `npx`, which `npx` and `npm exec` set; else COMMAND's program,
   without its directory and extension, made a valid name (`vitest`). The name stays in the key,
   so `hushrun run -- vitest run` in the `test` script and `hushrun run test -- vitest run`
   share results.

## Consequences

- Setting a repository up is one commit: `hushrun init` wraps the scripts, names the backend,
  adds the devDependency and connects the repository. CI needs `id-token: write` and nothing
  else; a teammate runs `npx hushrun login` once.
- `package.json` now decides where results go. A repository without one, in Go or Rust, still
  sets `HUSHRUN_BACKEND`; reading another manifest is a later, additive change.
- A hostile repository can make a laptop send it run results (check names, commands, digests),
  and run its own sign-in page through `hushrun login`, where a relayed device code could ask a
  person to approve a sign-in that isn't theirs (RFC 8628 §5.4). hushrun names the file that
  chose the backend before it signs in. Running that repository's checks runs its code anyway,
  which could take more than results.
- Trust on first use leaves a window: if another repository takes a connected name before the
  first CI run, its token is the one pinned. Connecting is a member's act, by name, and the
  dashboard lists each connected repository with when CI first recorded from it.
- `HUSHRUN_TOKEN` no longer goes to the backend `hushrun login` saved: a job that relied on that
  sets `HUSHRUN_BACKEND` too.
- Two wrapped commands in one script share the script's name: `"test": "hushrun run -- vitest
  run && hushrun run -- playwright test"` records two `test` checks. Their keys differ, so their
  results never mix, but the dashboard shows both as `test`; naming each keeps them apart.

## Alternatives considered

- **Keep the backend out of the code, as T-4 keeps the ledger.** Every CI job and every
  teammate would set a variable or type the org's URL: the steps that make setup slow, for a
  protection that results don't need (item 2).
- **A file of hushrun's own, such as `.hushrun.json`.** One more file in every repository, for
  one setting that `package.json` can carry next to the scripts it wraps. `hushrun.yaml` is the
  pipeline's, and its `ledger` follows T-4.
- **Send `HUSHRUN_TOKEN` to whichever backend applies, as before.** A pull request could then
  name a server and receive the CI's secret.
- **Connect CI only through the GitHub App.** Installing it needs an org admin, which a new user
  often isn't. The App stays, for orgs that want every repository.
- **Match a repository by name alone, or pin its id when it is connected.** By name alone, a
  repository created again under the name is accepted. Pinning at connect time needs GitHub's
  API with the person's GitHub credentials, which the CLI doesn't hold; the first ID token
  carries the id, signed by GitHub.
- **Always ask for NAME.** In `package.json` the script's name is the right one, and a second
  name to keep in step is one of the steps the start is meant to lose.
