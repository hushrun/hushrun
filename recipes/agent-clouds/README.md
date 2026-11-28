# Agent clouds

What to put in each hosted agent platform so a session skips what this exact code already passed
(ADR 0140 to 0143). Replace `acme` with your organization and `shop` with the repository's name.
`hushrun init --agent-clouds <list>` writes the hooks, Cursor's install script and Copilot's setup
steps for you.

| File | Platform | What it does |
|---|---|---|
| `claude-code-settings.json` | Claude Code on the web | `.claude/settings.json`: when a cloud session starts, `hushrun hook session-start` pulls the baked store and the package store and fetches the newest results (its setup script has no secrets), then tells Claude what this code already passed; when a session stops, sends waiting results |
| `cursor.Dockerfile` | Cursor cloud agents | `.cursor/Dockerfile`: the hushrun binary; the baked store is pulled by the Build, not kept in a layer |
| `cursor-hooks.json` | Cursor cloud agents | `.cursor/hooks.json`: sends waiting results when an agent stops |
| `cursor-hushrun-install.sh` | Cursor cloud agents | `.cursor/hushrun-install.sh`: while Cursor builds the environment, installs hushrun if it is missing, pulls the baked store into `/hushrun` and fetches the newest results; always exits 0 |
| `cursor-environment.json` | Cursor cloud agents | `.cursor/environment.json`: `install` runs that script, after the install command already there |
| `copilot-setup-steps.yml` | GitHub Copilot cloud agent | `.github/workflows/copilot-setup-steps.yml`: a session from the job's ID token, and the baked store |
| `copilot-hooks.json` | GitHub Copilot cloud agent | `.github/hooks/hushrun.json`: sends waiting results when the session ends |
| `codex-setup.sh` | Codex cloud | The environment's setup script: a session, results and compiled code fetched while there is a network |
| `jules-setup.sh` | Jules | The repository's setup script |
| `devin-setup.sh` | Devin | The machine's setup, before it is saved |
| `antigravity-env-setup.sh` | Google Antigravity Remote Agents | The remote project's Environment setup script, pasted in its settings: installs hushrun if it is missing, pulls the baked store and the package store (as root, through `sudo -n`, or into folders under `$HOME` that `~/.profile` names), runs the install command through `hushrun packages exec`, puts `hushrun packages env`'s variables in the profile, and fetches the newest results; always exits 0 |

`hushrun hook session-start` does nothing outside a hosted agent's sandbox and always exits 0. It
stops after 45 seconds (`--budget`), and the hook's `timeout` of 60 leaves `npx` time to start it.
A step still running then goes on in the background. Its steps' output is in
`session-start.log` in hushrun's cache directory. Other platforms' start hooks can call it too.
A repository with the older hook, which ran `hushrun build pull`, gets the new one from `hushrun
init --agent-clouds claude-code`, in the old one's place.

Its steps are `hushrun build pull` (compiled code, into `/hushrun`), `hushrun packages pull` (the
package store, into `/hushrun-packages`, before anything installs) and `hushrun fetch`. CI
publishes the package store with `hushrun packages bake --oci REPOSITORY … -- COMMAND` on the
default branch (ADR 0163). The backend's registry doesn't take package stores yet, so publish to a
registry of your own (`--oci ghcr.io/acme/shop`) and set `HUSHRUN_PACKAGES_IMAGE=ghcr.io/acme/shop`
in the environment's settings. Add `HUSHRUN_PACKAGES_REGISTRY_TOKEN` there too if that registry
needs a token; `HUSHRUN_REGISTRY_TOKEN` stays the one for hushrun's registry. A sandbox that
doesn't run as root sets `HUSHRUN_PACKAGES_LOWER` to a folder it may write.

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

Antigravity keeps a remote project's setup script in its settings, so `hushrun init` can't write
it: paste `antigravity-env-setup.sh` there whole, and edit its install command (`npm ci`) and
`HUSHRUN_PACKAGES_IMAGE`. Until hushrun's registry takes package stores, the store comes from your
own registry: [`recipes/github/packages-bake.yml`](../github/packages-bake.yml) publishes it on
each push to the default branch (`hushrun packages bake --oci ghcr.io/acme/shop -- npm ci`, with
the job's `GITHUB_TOKEN` and `packages: write`). When `--oci hushrun` works, the recipe names
`hushrun` instead of the image and the setup script drops its `HUSHRUN_PACKAGES_IMAGE` line: one
line each. A private package needs `HUSHRUN_PACKAGES_REGISTRY_TOKEN` in the project's settings.
What the probe ([docs/validation/antigravity-remote.md](../../docs/validation/antigravity-remote.md))
hasn't answered yet is marked `[probe]` in the script: whether setup runs once or per
conversation (the profile block is replaced, never added twice), whether it has root (`AGENT_USER`
names the agent's user when setup runs as root and the agent doesn't), whether the agent's shell
reads `~/.profile` or `~/.bashrc` (both get the block), and where the workspace is. Hosts to allow
there: hushrun's three above, `github.com`, `objects.githubusercontent.com` and `release-assets.githubusercontent.com` for the installer,
and the package store's registry (`ghcr.io` and `pkg-containers.githubusercontent.com` for GHCR)
until hushrun's registry takes package stores.
