# Agent clouds

What to put in each hosted agent platform so a session skips what this exact code already passed
(ADR 0140 to 0143). Replace `acme` with your organization and `shop` with the repository's name.
`hushrun init --agent-clouds <list>` writes the hooks, Cursor's install script and Copilot's setup
steps for you.

| File | Platform | What it does |
|---|---|---|
| `claude-code-settings.json` | Claude Code on the web | `.claude/settings.json`: when a cloud session starts, `hushrun hook session-start` pulls the baked store and fetches the newest results (its setup script has no secrets), then tells Claude what this code already passed; when a session stops, sends waiting results |
| `cursor.Dockerfile` | Cursor cloud agents | `.cursor/Dockerfile`: the hushrun binary; the baked store is pulled by the Build, not kept in a layer |
| `cursor-hooks.json` | Cursor cloud agents | `.cursor/hooks.json`: sends waiting results when an agent stops |
| `cursor-hushrun-install.sh` | Cursor cloud agents | `.cursor/hushrun-install.sh`: while Cursor builds the environment, installs hushrun if it is missing, pulls the baked store into `/hushrun` and fetches the newest results; always exits 0 |
| `cursor-environment.json` | Cursor cloud agents | `.cursor/environment.json`: `install` runs that script, after the install command already there |
| `copilot-setup-steps.yml` | GitHub Copilot cloud agent | `.github/workflows/copilot-setup-steps.yml`: a session from the job's ID token, and the baked store |
| `copilot-hooks.json` | GitHub Copilot cloud agent | `.github/hooks/hushrun.json`: sends waiting results when the session ends |
| `codex-setup.sh` | Codex cloud | The environment's setup script: a session, results and compiled code fetched while there is a network |
| `jules-setup.sh` | Jules | The repository's setup script |
| `devin-setup.sh` | Devin | The machine's setup, before it is saved |

`hushrun hook session-start` does nothing outside a hosted agent's sandbox and always exits 0. It
stops after 45 seconds (`--budget`), and the hook's `timeout` of 60 leaves `npx` time to start it.
A step still running then goes on in the background. Its steps' output is in
`session-start.log` in hushrun's cache directory. Other platforms' start hooks can call it too.
A repository with the older hook, which ran `hushrun build pull`, gets the new one from `hushrun
init --agent-clouds claude-code`, in the old one's place.

Cursor runs `install` on every Build and snapshots the disk; a new agent boots that disk, so it
starts with the baked store and the results fetched for the Build's commit. The pull swaps
`/hushrun` into place by renames in `/`, so the script runs it as root: directly, or through
`sudo -n`, which never asks for a password. Without root or passwordless sudo the pull fails, the
Build log says why, and fetch still runs. For a private registry, set `HUSHRUN_REGISTRY_TOKEN` as a
Cursor team or environment secret (Builds don't read a person's own secrets): the pull reads it,
and no file stores it. A public repository needs no token.

A saved environment reads a committed `.cursor/environment.json` only when its environment.json
location points at that file, and environments start with that location set to none. A cloud
agent start that creates a saved environment for a single repository that has none yet points it
at a valid committed file. A personal or team environment that ignores the file doesn't run the
script until someone points it there. Builds read the default branch, so the files work once they
are merged there.

An older `.cursor/Dockerfile` with `RUN hushrun build pull` keeps a store from the day the
Dockerfile last changed. `hushrun init` doesn't edit it: remove that line yourself.

A dev container gets hushrun from its feature, `ghcr.io/hushrun/features/hushrun:1` (`features/`).

The hosts to allow, where a platform limits the network: `api.hushrun.com`,
`registry.hushrun.com` and `*.r2.cloudflarestorage.com`. `hushrun doctor` checks them.
