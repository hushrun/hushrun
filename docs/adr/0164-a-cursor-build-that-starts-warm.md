# 0164. A Cursor Build that starts warm

- **Status:** proposed; built
- **Date:** 2026-11-28
- **Rules:** C-145 changed; relies on B-64, B-65, B-240, C-140, C-142 and C-144
- **Amends:** [0143](0143-a-baked-store-pulled-into-a-fixed-sandbox.md)

## Context

ADR 0143 put Cursor's pull in the Dockerfile: `RUN --mount=type=secret,… hushrun build pull`. A
layer runs again only when the Dockerfile changes, so the store in the image was the one from the
day the Dockerfile last changed, while CI kept baking newer ones.

Cursor prepares a cloud agent's disk in a Build. It clones the environment's repositories at their
default branch, runs `install` from `.cursor/environment.json` from the project root, and snapshots
the disk. It runs `install` on every Build, possibly on the disk an earlier Build prepared. A
Build that fails doesn't replace the active one. The snapshot keeps disk state only: a variable
exported during `install` doesn't reach the agent. Builds read team and environment secrets, not a
person's own. Cursor's docs say install scripts can mint an OIDC token on `CURSOR_AGENT_SOCKET`.

## Decision

1. **The pull moves to `install`.** `hushrun init --agent-clouds cursor` writes
   `.cursor/hushrun-install.sh` and points `install` at it. The command runs on every Build, on
   the cloned repository, and the snapshot is what a new agent boots. The script also installs
   hushrun when it is missing, then runs `hushrun build pull`, then `hushrun fetch`.
2. **The store is `/hushrun`.** `install` can't leave `HUSHRUN_BUILD_LOWER` for the agent, and a
   run with it unset reads `/hushrun` (B-64, B-65). The pull unpacks beside its destination and
   swaps it in by renames in `/` (B-240), so it runs as root: directly, or through `sudo -n`,
   which never asks for a password. A tree the pull makes as root, folders `755` and files `644`,
   is what B-65's owner and mode rules accept. The script never makes it writable by others. A
   store someone named with `HUSHRUN_BUILD_LOWER` is theirs, and is pulled as the user.
3. **Sudo keeps three variables.** `--preserve-env=HUSHRUN_REGISTRY_TOKEN,HUSHRUN_BACKEND,HUSHRUN_HOST`:
   the pull token, and what says which backend and registry are meant. Everything else is reset,
   so the agent's own credentials, sessions and Cursor's socket don't reach a process running as
   root. The binary is named by its path, so sudo's `secure_path` doesn't choose it. `fetch` runs
   as the user, so its results land in that user's cache directory.
4. **The registry token stays `HUSHRUN_REGISTRY_TOKEN`,** a Cursor team or environment secret,
   stored in no file. A Cursor OIDC token and the `hrs_` session it mints read results and don't
   pull (ADR 0141). Pull reads that token before any other credential, so the order of pull and
   fetch doesn't decide what pull sends. Without it, a pull through sudo sends nothing, which a
   public repository's image needs.
5. **Init doesn't invent the dependency install.** The command already in `install` stays first,
   joined with `&&`, so a failing `pnpm install` still fails the Build. An `install` that already
   names the script, or that isn't a string, stays as it is.
6. **The script exits 0,** so a missing image, a refused registry or no sudo doesn't replace the
   last good Build with a failed one.
7. **The script is replaced only when hushrun wrote it.** Init writes it when it is missing, and
   replaces it when its bytes are a script an earlier hushrun wrote, from a list next to the
   current text (empty for now). Anything else is a hand edit, and stays.

## Consequences

- A new Cursor agent boots with the newest baked store and with results fetched for the Build's
  commit, kept 24 hours. It looks up the rest on its first run.
- The pull needs root or passwordless sudo during `install`. Where neither is there, the pull says
  why it failed in the Build log, and fetch still runs.
- A saved environment reads `.cursor/environment.json` only when its environment.json location
  points at it. Environments start with none, so a team that made one by hand points it there.
- The session fetch gets is saved on the snapshot. An agent booted from it uses that session while
  it is good, at most 12 hours (C-142), then gets its own over the socket. The server checks a
  session at every request.
- The script installs hushrun only when it is missing. A disk an earlier Build prepared keeps the
  hushrun it has.
- A Dockerfile a repository already has, with its own `RUN hushrun build pull`, stays as it is.
  Init doesn't edit it.
- Nothing here has run in a real Cursor Build yet. Which user runs `install`, whether the socket
  is there, and whether a Build's OIDC token makes a session are for that check to confirm.

## Alternatives considered

- **Keep the pull in the Dockerfile.** See the context: the layer goes stale.
- **Pull to a folder of the user's and set `HUSHRUN_BUILD_LOWER`.** The variable doesn't survive
  the snapshot. Writing it into a shell profile reaches only the shells that read that file.
- **`sudo -E`.** It would hand every variable of the Build to root, the agent's credentials too.
- **A world-writable `/hushrun` made in the Dockerfile.** B-65 ignores a store others may write.
- **Run the whole script as root.** Fetch would then keep its results in root's cache directory,
  which the agent doesn't read when it runs as another user.
