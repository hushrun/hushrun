# 0066. The skill goes to marketplaces from a repository the release writes

- **Status:** accepted
- **Date:** 2026-11-09
- **Rules:** C-24 (the skill); relies on [0065](0065-agents-learn-what-is-out-of-date.md)
- **Relies on:** [0035](0035-updates-and-the-newest-release-by-default.md) (the newest release),
  [0039](0039-npm-trusted-publishing.md) (the release environment)

## Context

hushrun's skill reaches agents through repositories that `hushrun init` set up (ADR 0036). An
agent in any other repository can't learn that hushrun exists, or how to set it up. Skills
marketplaces fix that: skills.sh (`npx skills add`), Claude Code's plugin marketplaces and
Anthropic's directory, Codex, GitHub Copilot and VS Code, Gemini CLI, `gh skill`, Cursor, Kiro.

Each reads a repository, and each has its own manifest, all of which assume the plugin is the
repository's root: Gemini CLI wants `gemini-extension.json` there, the Agent Plugins format wants
`plugin.json` there. This repository's root is a Rust workspace, with `Cargo.lock` and test data
that Anthropic's directory holds for a reviewer; and the skills CLI would find init's own copy in
`.agents/skills/hushrun`, from `main`, before a released one.

None of them updates a skill for the person, but for Anthropic's directory, which follows a
branch: each has its own command. Claude Code updates a plugin only when its `version` changes.
The skills CLI follows a branch's content: a skill changed on `main` before a release would reach
its users early. ADR 0065 makes a stale copy safe, since the skill points to the binary, but the
copy should still change only with a release.

## Decision

1. **One bundle, built from `recipes/agents/`** by `recipes/agents/bundle.mjs`, with no
   dependencies: `skills/hushrun/SKILL.md`, byte for byte the skill init writes; `plugin.json`
   (Agent Plugins 1.0, only its schema's keys); `.claude-plugin/plugin.json`, with the listing
   fields Anthropic's directory reads and the icon at `assets/icon.png`;
   `.claude-plugin/marketplace.json`, which Claude Code, Copilot CLI, Factory, Devin and Junie
   read; `.agents/plugins/marketplace.json`, with the policy and category Codex asks for;
   `gemini-extension.json`; a README that says how to install and update it; and `LICENSE`.
   Every manifest says the CLI's version, from `Cargo.toml`, and the marketplaces' entries none,
   so that Claude Code reads one.

2. **A repository of its own, `github.com/hushrun/skills`,** holds the bundle at its root, and
   nothing else. Marketplaces install from it. It is never edited by hand.

3. **The release writes it.** `.github/workflows/skills.yml` runs after the Release workflow, as
   `npm.yml` does, in the `release` environment, which waits for a maintainer. It builds the
   bundle at the release's tag, replaces the repository's files with it, commits, and tags the
   commit as the release is tagged, `v<version>`, with a deploy key, `SKILLS_DEPLOY_KEY`. It never
   publishes a pre-release, a release that is there already, or a release older than the newest
   one there. Without the key it builds and checks the bundle and publishes nothing, so it can go
   in before the repository exists.

4. **The npm package carries the skill too**, at `skills/hushrun/SKILL.md`, where tools that link
   skills from installed packages look, such as the skills CLI's `experimental_sync`. A
   repository's copy then follows its own hushrun's version.

5. **Listings are submitted by a person.** Anthropic's directory, Cursor's marketplace,
   awesome-copilot, ClawHub, Tessl and Kiro each take one submission from an account, and follow
   the repository after it. skills.sh lists a skill after its first install.

## Consequences

- An agent anywhere can find hushrun's skill, and a release reaches every marketplace that
  follows the repository without a hand in it.
- A change to `recipes/agents/` reaches marketplaces at the next release only.
- Claude Code's users get a new release when they update the plugin, since its version changes;
  auto-update is theirs to turn on. Anthropic's directory checks each new commit.
- **Contracts:** the bundle's layout and the repository's tags; `SKILLS_DEPLOY_KEY`; the npm
  package's `skills/`.
- Renaming the plugin breaks every install: `hushrun` is its name everywhere.

## Alternatives considered

- **Publishing from this repository**, from a subdirectory. Gemini CLI and the Agent Plugins
  format want the plugin at the root, the skills CLI would find init's copy from `main` first,
  and every install would clone the Rust workspace.
- **Committing the bundle here and pointing marketplaces at a branch.** It needs a second copy of
  every file in this repository, kept equal by a test, and the branch would still be this
  repository's.
- **A token instead of a deploy key.** A deploy key writes to one repository and nothing else.
- **Publishing on every push to `main`.** Skill users would get text for a hushrun that isn't out.
