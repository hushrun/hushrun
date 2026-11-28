# 0143. A baked store pulled into a sandbox whose image is fixed, and recipes per platform

- **Status:** proposed; built
- **Date:** 2026-11-23
- **Rules:** B-240 and C-145 new; B-63 to B-67 and B-70 read
- **Amended by:** [0148](0148-a-pull-that-says-what-it-pulled.md), [0161](0161-a-session-start-that-warms-and-tells.md), [0164](0164-a-cursor-build-that-starts-warm.md), [0165](0165-a-session-start-in-antigravitys-format.md)

## Context

A baked store (ADR 0098 to 0103) reaches a run through an image: `COPY --from=<image> /hushrun
/hushrun` in the sandbox's Dockerfile. Most hosted agent platforms don't build the sandbox from the
team's Dockerfile, or build it from an image the team can't change: Claude Code on the web, Codex
cloud, Copilot's cloud agent, Jules and Devin each start from their own image and run a setup step.
There, a store can only arrive by being downloaded. And each platform puts its setup somewhere else.

## Decision

1. **`hushrun build pull`** downloads the repository's newest baked image from the backend's
   registry (`<registry host>/<org>/<name>:hushrun-<platform>-latest`, B-70), or one `--image`
   names, with a pull token (`HUSHRUN_REGISTRY_TOKEN`), the backend's own credential here when it
   goes to that registry, or nothing for a public repository. It unpacks only what is under
   `hushrun/` (regular files and folders, no `..`, no absolute path) into a folder beside the
   destination, checks it as `hushrun build bake` checks a folder (B-63, names and bytes) and, when
   `hushrun.yaml` names signers of baked stores, its signature (B-67), then swaps it into place:
   `--to`, `HUSHRUN_BUILD_LOWER` or `/hushrun`. A store that doesn't check out replaces nothing.
   Every run still decides what it trusts in a store as it begins, as for any baked store.
2. **Recipes per platform** in `recipes/agent-clouds/`: Claude Code's hooks (a `SessionStart` hook,
   gated on `CLAUDE_CODE_REMOTE`, pulls the store, since its setup script has no environment
   variables or network secrets), Cursor's Dockerfile with a build secret, Copilot's setup steps,
   and setup scripts for Codex cloud, Jules and Devin.
3. **A Dev Container Feature,** `features/src/hushrun`, to publish as
   `ghcr.io/hushrun/features/hushrun`: it installs a release with its own installer, and its
   `onCreateCommand` runs `hushrun fetch` (and `hushrun build pull` when asked). It holds no secret
   and recommends the container's own.
4. **`hushrun init` names the agent clouds the team uses** (C-145: `--agent-clouds` only; a person at
   a terminal who named none is told how) and writes only those files.
   It reads no answer for it: init's other questions read the terminal, and scripted answers to them
   must keep working.

## Consequences

- A fixed-image sandbox starts with CI's compiled code, at the cost of one download per session.
  A layer is read into memory whole, at most 8 GiB each, which a large store may feel.
- A store pulled by a run that writes nothing trusted can be read unverified only as B-63 already
  allows (`HUSHRUN_BUILD_LOWER_UNTRUSTED=1`); `build pull` doesn't widen what a run trusts.
- The feature is built here and published by the release process, separately: nothing publishes it
  yet.
- The recipes aren't tried on each platform yet: Devin's and Jules' setup were read from their
  docs, and none of the platforms' hooks has run hushrun there.

## Alternatives considered

- **A store per platform image** built by the bake. The platforms' images change on their own
  schedule, and a team can't publish to them.
- **Pulling with `docker`.** Most sandboxes have no Docker daemon.
- **Trusting what the pull checked.** A run reads `baked.json` itself, once, as it begins; what a
  pull checked stands for nothing a moment later.
