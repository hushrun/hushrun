# Agent clouds

What to put in each hosted agent platform so a session skips what this exact code already passed
(ADR 0104 to 0107). Replace `acme` with your organization and `shop` with the repository's name.
`hushrun init --agent-clouds <list>` writes the hooks and Copilot's setup steps for you.

| File | Platform | What it does |
|---|---|---|
| `claude-code-settings.json` | Claude Code on the web | `.claude/settings.json`: pulls the baked store when a cloud session starts (its setup script has no secrets), and sends waiting results when a session stops |
| `cursor.Dockerfile` | Cursor cloud agents | `.cursor/Dockerfile`: the baked store in the image, with a build secret |
| `cursor-hooks.json` | Cursor cloud agents | `.cursor/hooks.json`: sends waiting results when an agent stops |
| `copilot-setup-steps.yml` | GitHub Copilot cloud agent | `.github/workflows/copilot-setup-steps.yml`: a session from the job's ID token, and the baked store |
| `copilot-hooks.json` | GitHub Copilot cloud agent | `.github/hooks/hushrun.json`: sends waiting results when the session ends |
| `codex-setup.sh` | Codex cloud | The environment's setup script: a session, results and compiled code fetched while there is a network |
| `jules-setup.sh` | Jules | The repository's setup script |
| `devin-setup.sh` | Devin | The machine's setup, before it is saved |

A dev container gets hushrun from its feature, `ghcr.io/hushrun/features/hushrun:1` (`features/`).

The hosts to allow, where a platform limits the network: `api.hushrun.com`,
`registry.hushrun.com` and `*.r2.cloudflarestorage.com`. `hushrun doctor` checks them.
