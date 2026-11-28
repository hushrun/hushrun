# Agent clouds

What to put in each hosted agent platform so a session skips what this exact code already passed
(ADR 0140 to 0143). Replace `acme` with your organization and `shop` with the repository's name.
`hushrun init --agent-clouds <list>` writes the hooks and Copilot's setup steps for you.

| File | Platform | What it does |
|---|---|---|
| `claude-code-settings.json` | Claude Code on the web | `.claude/settings.json`: when a cloud session starts, `hushrun hook session-start` pulls the baked store and the package store and fetches the newest results (its setup script has no secrets), then tells Claude what this code already passed; when a session stops, sends waiting results |
| `cursor.Dockerfile` | Cursor cloud agents | `.cursor/Dockerfile`: the baked store in the image, with a build secret |
| `cursor-hooks.json` | Cursor cloud agents | `.cursor/hooks.json`: sends waiting results when an agent stops |
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

Its steps are `hushrun build pull` (compiled code, into `/hushrun`), `hushrun packages pull` (the
package store, into `/hushrun-packages`, before anything installs) and `hushrun fetch`. CI
publishes the package store with `hushrun packages bake --oci REPOSITORY … -- COMMAND` on the
default branch (ADR 0163). The backend's registry doesn't take package stores yet, so publish to a
registry of your own (`--oci ghcr.io/acme/shop`) and set `HUSHRUN_PACKAGES_IMAGE=ghcr.io/acme/shop`
in the environment's settings. Add `HUSHRUN_PACKAGES_REGISTRY_TOKEN` there too if that registry
needs a token; `HUSHRUN_REGISTRY_TOKEN` stays the one for hushrun's registry. A sandbox that
doesn't run as root sets `HUSHRUN_PACKAGES_LOWER` to a folder it may write.

A dev container gets hushrun from its feature, `ghcr.io/hushrun/features/hushrun:1` (`features/`).

The hosts to allow, where a platform limits the network: `api.hushrun.com`,
`registry.hushrun.com` and `*.r2.cloudflarestorage.com`. `hushrun doctor` checks them.
