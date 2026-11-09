# 0065. Agents learn what is out of date: a thin skill, `hushrun guide`, and a notice they see

- **Status:** accepted
- **Date:** 2026-11-09
- **Rules:** C-21, C-22, C-24 (amended); C-28 and C-29 (new); C-23's test of who is an agent
- **Amends:** [0035](0035-updates-and-the-newest-release-by-default.md) item 3, whose notice was
  "only for people"; [0036](0036-agents-told-before-they-push.md) item 8, whose skill held every
  rule, and [0061](0061-local-state-changes-only-when-asked.md)'s notice, which kept quiet in a
  sandbox with an agent in it

## Context

hushrun's skill reaches agents through the repository: `hushrun init` writes it to
`.claude/skills/hushrun/SKILL.md` and `.agents/skills/hushrun/SKILL.md` (ADR 0036). The skill is
also going to skills marketplaces, such as skills.sh, where a person installs it once in their
home, and where it is updated only when they run that marketplace's own command.

Three things go out of date, and nothing told an agent about any of them:

- **hushrun itself.** The newer-release notice (C-21) needs stderr on a terminal. A coding
  agent's shell has none, so the notice was for people only, by design (ADR 0035). ADR 0061 also
  kept it out of sandboxes that set `CI`, with an agent in them.
- **The repository's copy of the skill.** A repository pins hushrun as a dev dependency. When
  the dependency moves, the skill stays as the older hushrun wrote it until someone runs init
  again. Worse, an older hushrun's init took a newer copy back to its own text.
- **A copy from a marketplace.** Which copy an agent reads isn't hushrun's choice: Claude Code
  prefers a person's copy over the repository's, Copilot CLI the repository's, and Codex and VS
  Code load both. A March copy in a person's home can override the repository's, for a hushrun
  from June.

Other CLIs that ship a skill answer the same way (Vercel's `vercel setup skills`, Prisma, Neon,
PlanetScale's `agent-guide`): the binary holds the rules for its own version, the skill points
to it, and the CLI says when a copy is stale, where an agent can see it.

## Decision

1. **The skill holds only what every hushrun does the same way** (C-24): how to tell that a
   repository uses hushrun, the loop of `hushrun ready`, the hook, what to do when hushrun says
   something is out of date, setting up with `npx hushrun init --yes`, and what never to do. It
   says to run `hushrun guide` once per session for the rest, and what to do when `guide` is
   unknown, an older hushrun's case. Its frontmatter holds only the Agent Skills spec's fields:
   `name`, `description`, `compatibility` and `metadata`. It no longer says that init wrote it,
   which a copy from a marketplace would contradict.

2. **`hushrun guide`** (C-28) prints the rest from the binary: what each state means, the hook,
   what each of hushrun's lines means, partial runs, `--force`, Bun, setting up with several
   organizations or in a sandbox, and how hushrun and the skill are kept current. Then what is
   true here: this hushrun's version and skill revision, a newer release the notice learned of,
   and each copy of the skill with its state. `--format json` gives `hushrun.guide/v1`. It asks
   nobody and changes nothing. `recipes/agents/GUIDE.md` is the one place its text lives.

3. **A skill revision.** The skill says `metadata.hushrun-skill`, a whole number that goes up
   only when its text changes, never with each release, so that a release doesn't rewrite every
   repository's copy. A test keeps the hash of each revision's text, and fails when the text
   changes without a new revision. The nine texts hushrun wrote before revisions, 0.3.0 to
   0.10.1, are revision 0: their hashes are listed, and their texts kept in
   `testdata/skills/before-revisions`.

4. **One function says what a copy is**, `skill::state_of`: `current`, `older`, `newer`, `edited`
   or `absent`. A copy whose revision is this hushrun's and whose text is that revision's is
   current. A lower revision with that revision's text, or a text from before revisions, is
   older. A higher revision is newer, whatever its text. Anything else is edited. CRLF reads as
   LF, as git may check a file out on Windows. `ready`, `doctor`, `guide`, `init` and `skill
   update` all ask it, and none decides on its own.

5. **Nothing takes a copy back to an older revision.** init leaves a newer copy (C-24), and
   `hushrun skill update` (C-29) leaves a newer copy and an edited one unless `--force`. A newer
   copy means this hushrun is the one to update, and `ready` and `guide` say so.

6. **`ready` notes the repository's copies** (C-22): an older copy, with the command that updates
   it, and a newer one, with how to update hushrun. The hook doesn't print them: a push isn't
   the place, and `ready` is what agents run before one. `ready.v2` has no new field: these are
   notes.

7. **`hushrun skill update`** writes this hushrun's skill where agents read it: the repository's
   two copies, or with `--global` the person's own, `~/.claude/skills/hushrun/SKILL.md` and
   `~/.agents/skills/hushrun/SKILL.md`. Each through a temporary file and a rename. A copy that
   is a link, as skills managers make them, keeps leading where it did: the file it leads to is
   written.

8. **The notice reaches agents** (C-21). When the environment says a coding agent runs hushrun,
   by C-23's test, now in one module that the hook and the notice share, the notice needs no
   terminal, and a sandbox that says `CI` with an agent in it doesn't silence it. It says, in
   words for the agent: ``hushrun: hushrun <newest> is available, and this is <this>: when you
   report back, tell the person, who updates it with `hushrun update` ``, or the package manager's
   words for an npm install. Everything else is as before: never in CI, never with `-q`, `--format
   json` or `HUSHRUN_NO_UPDATE_CHECK`, once a day, from one record, so an agent and a person on
   one machine are told once between them.

9. **An agent doesn't update hushrun itself.** The skill says to finish the task, then tell the
   person what hushrun said, with its command. An npm package is the repository's, so updating
   it is a change to the code; a binary is the person's; and an update in the middle of a task
   changes the tool the agent is using.

10. **Tests stay off the network.** `.cargo/config.toml` sets `HUSHRUN_NO_UPDATE_CHECK=1` for what
    cargo runs, so a test that inherits an agent's environment never asks GitHub; the notice's
    own tests clear their environment. The test kit clears the variables coding agents set, so a
    test sees a person whichever agent runs the suite.

## Consequences

- Every copy of the skill, whichever an agent reads, is right for whichever hushrun is installed:
  the rules that change come from the binary. It costs an agent one command per session.
- An agent learns of a newer release, an older copy of the skill, and a copy from a newer hushrun,
  and tells the person. Nothing is updated without them.
- A repository set up before 0.11.0 has a copy from before revisions: `ready` says it is older,
  and `hushrun skill update` or `hushrun init` replaces it.
- **Contracts:** `hushrun guide` and `hushrun.guide/v1`; `hushrun skill update`, its `--global`
  and `--force`, and its exit codes; `metadata.hushrun-skill` in the skill; `ready`'s new notes;
  the agent's notice line.
- A change to `recipes/agents/SKILL.md` needs a new revision, and its hash: the test says so.
- Skills marketplaces carry the same `SKILL.md`, and update it on their own schedule. Since the
  skill points to the binary, a stale copy there is still right, only shorter.

## Alternatives considered

- **Keeping the whole skill, versioned with each release.** Every release would rewrite every
  repository's copy, and a person's copy from a marketplace would still override the repository's
  in Claude Code, with rules for another hushrun.
- **The release's version in the skill.** It changes when nothing in the text does, so every
  release would make every copy stale. A revision changes with the text only.
- **Comparing copies by their text alone.** It tells a copy apart from this hushrun's, but not
  older from newer, so an older hushrun would take a newer copy back.
- **`ready` or `run` rewriting a stale copy themselves.** It changes committed files as a side
  effect of a check (ADR 0061: local state changes only when asked), and an agent would push the
  change without anyone asking for it.
- **Letting agents update hushrun.** See item 9.
- **Telling agents through the hook.** A push is the wrong moment, and the hook prints only what
  bears on the push.
