# 0024. `hushrun init` sets up `hushrun run`

- **Status:** accepted
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-19
- **Rules:** C-15, C-16; C-9, C-11, C-12 and C-13, which it relies on; §10.4

## Context

The product is `hushrun run -- COMMAND`: skip a check this exact code already passed, on a
laptop, in CI and in an agent's sandbox. Getting a repository there took about 12 steps in five
places: install the binary; wrap each check in package.json; find the org's URL in the
dashboard; `hushrun login` with it; set `HUSHRUN_BACKEND` in each workflow; give each job
`id-token: write`; connect the repository's CI in the dashboard; install hushrun in CI; then
commit, push and check that results arrive. The goal is a new user set up in under three
minutes, with one command people copy, `npx hushrun init`, and the same command for agents
with `--yes`.

`hushrun init` wrote a promotion pipeline's starter files (§10.4, M6): `hushrun.yaml`, two
signing keys and `trust.yaml`. That is a later step, for teams that deploy with hushrun, and its
flags already say which init they mean.

## Decision

1. **`init` is the setup wizard; the pipeline's init moves behind `--pipeline`.** Its own flags
   (`--force`, `--keys-dir`, `--repository`, `--staging-branch`, `--production-branch`,
   `--staging-image`, `--production-image`) imply `--pipeline`, so `hushrun init --staging-image
   …`, as the GitHub and GitLab guides run it, is unchanged. The wizard's flags (`--yes`,
   `--no-browser`, `--dry-run`, `--run`, `--no-run`) don't go with them. The wizard needs no
   `hushrun.yaml`, so it runs before hushrun loads one, like `login`.
2. **One plan, one question.** Init looks (the checks, the package manager, the workflows that
   run the checks), signs in, then shows every change, as old → new lines, and asks once. With
   `--yes` it doesn't ask; without a terminal it needs `--yes`, and otherwise prints the plan
   and exits 2, so that an agent that forgets the flag sees what would change. `--dry-run`
   signs in to nothing and contacts nothing.
3. **Edits keep each file's bytes.** package.json is read by a small JSON reader in the crate
   that keeps each value's span: an edit replaces a string, or inserts a member laid out like the
   members around it (one per line at their indentation, spaces or tabs, or on one line), with
   the file's line endings; everything else, its final newline or the lack of one included,
   stays. A dependency goes in order among sorted ones, `devDependencies` after
   `dependencies`, `hushrun` last. Before writing, serde_json must read the result as the
   original with exactly those changes, or init exits 1 and writes nothing. Workflows are
   edited as lines, with the indentation each file uses; nothing is parsed and written back, so
   comments and quoting elsewhere stay.
4. **Wrapping only what init reads for sure.** A simple command gets `hushrun run -- ` in front:
   C-9 names its check after the script. A chain of `&&` wraps each command except the package
   manager's own, which run other scripts (wrapped on their own) or install packages; tool runs
   (`npx`, `npm exec`, `pnpm dlx`, `bun x`) and `bun test` are wrapped. A bare `vitest`, which
   watches on a terminal, becomes `vitest run`. Scripts that already run hushrun, that watch,
   serve or write files, or that use the shell beyond `&&` (`||`, `;`, pipes, redirections,
   substitutions, subshells, builtins, a leading `VAR=value`) stay as they are, with the reason:
   wrapping them would change what they do, or key a run on a command hushrun doesn't see.
5. **A job that runs a wrapped check gets an ID token, as narrowly as its file allows.**
   `id-token: write` goes into the job's own `permissions:`; else into the workflow's, which the
   job inherits; else into a new block on the job with `contents: read`, since a job's first
   `permissions:` drops every permission it doesn't list, and the plan says so. `write-all`
   needs nothing. `read-all` can't take one more permission, so the plan says what to write
   instead. A command counts when it runs the script at the repository root: not after a `cd`
   elsewhere, under another `working-directory`, or with an option that picks another package.
6. **The package manager installs hushrun.** Init adds `HUSHRUN_NPM_PACKAGE` (which the npm
   package sets to its own version), else `hushrun@^<version>`, to `devDependencies`
   itself, so that the plan shows it and a failed install leaves it declared; then it runs
   `npm install --save-dev`, `pnpm add --save-dev` (`-w` at a `pnpm-workspace.yaml` root),
   `yarn add --dev` (`-W` for Yarn 1 at a `workspaces` root) or `bun add --dev`, so that the
   lockfile and `node_modules/.bin/hushrun` match. CI's own install step then installs hushrun
   too: no workflow needs a step for it. A failed install exits 1, after the repository is
   connected, so that running the install again finishes the setup.
7. **The repository names its backend; CI needs no variable and no secret.** package.json gets
   `"hushrun": {"backend": "<org URL>"}` (C-11), and init connects the repository
   (`POST {base}/v0/repositories`, C-12) so that the backend takes CI's ID token for it.
8. **What CI repeats, while the sign-in waits (C-16).** With `gh` signed in, init lists the
   repository's workflow runs of the last 30 days, the newest 1,000 at most, within 10 s, groups
   them by workflow and commit tree, and counts the completed runs that started after a pass on
   the same tree. It prints one line, or nothing: it never fails init, and it uses `gh`'s
   sign-in, so no GitHub token passes through hushrun.
9. **A skip, to finish.** Init runs the first wrapped lint, typecheck or test check twice
   through the package manager, streaming its output, unless `--no-run`, or `--yes` without
   `--run`: an agent runs the checks itself.

## Consequences

- Setup is one command and one answer. The repository carries its backend and permissions, so
  teammates, CI and agents need nothing more; a teammate running init in a set-up repository
  only signs in.
- `hushrun init` without flags no longer writes `hushrun.yaml`. A script that relied on that
  passes `--pipeline`. The wizard's printed lines (the estimate, the connection, the next step)
  are what the dashboard's start page and the docs quote.
- The JSON reader and the workflow editor are code to keep: their tests pin exact bytes for
  indentation styles, CRLF, a missing final newline, escapes, and each permissions case.
- A package manager writes package.json again when it adds the package: npm keeps the file's
  indentation and line endings but sorts dependencies and lays out arrays one item per line.
  Init puts the dependency where npm would, so a file npm last wrote changes no further; a file
  laid out by hand may.
- Workflows init can't read (a job in flow style, a run built by expressions) stay as they are;
  their runs record nothing until someone adds `id-token: write` (C-14 runs them anyway).
- A new `permissions:` block narrows a job's token to `contents: read`: a job that pushed,
  commented or published needs its permission listed again. The plan says so before anything
  changes.
- Wrapping each command of a chain can skip a setup command (`next typegen`) on code that
  passed it; a later command of the chain that failed on that code then runs, in a fresh
  checkout, without the setup's output. It costs a run of that command, never a skip of a
  failed check.

## Alternatives considered

- **Reading package.json with serde_json and writing it back** (with `preserve_order`): it
  loses the file's layout, such as arrays on one line, and the diff a person reviews grows
  with it. A format-preserving JSON crate would be a dependency for one file.
- **A YAML library for workflows** (serde-saphyr is already a dependency): writing back drops
  comments and quoting. Line edits keep them.
- **`HUSHRUN_BACKEND` in each workflow.** It needs an edit per job and does nothing for
  laptops and agents; package.json is read by all three (C-11).
- **A question per change.** Slower, and the changes only work together; one plan is easier to
  review, and `--dry-run` shows it without applying.
- **Wrapping a whole chain as one check** (`hushrun run -- sh -c "…"`): not portable to
  Windows, where npm runs scripts with cmd.exe, and the check's key would hide which command
  failed.
- **The plain install (`npm install`, `pnpm install`, …) after init's own edit.** It would
  leave package.json exactly as init wrote it, where an add command writes the file again
  (Consequences). The add commands stay: they are what each package manager documents for
  adding a tool, they record the range it resolved, and init's sorted insertion leaves npm's
  rewrite nothing more to change on a file npm last wrote.
- **Calling GitHub's API with a token for the estimate.** hushrun would handle a GitHub token
  for a number it shows once; `gh` already holds the person's sign-in, and without it init
  says nothing.
- **Naming the wizard `hushrun setup`.** `init` is what people expect to run first, and the
  pipeline's init is the later step that takes flags anyway.

## Note, 2026-10-22: a first-time user's sign-in

Init's sign-in (C-15 step 3) assumed the person approving it belongs to an org. A brand-new
person whose first sign-in made no org of their own (someone invited, since an invitation waiting
keeps `first_org` from making one), or who isn't in the org a repository names, approved the
sign-in for no org, and init stopped with `named no org: run hushrun login …/o/<org>`, useless to
someone with none. The approval page (`/device`) now lets them accept an invitation, pick one of
their orgs, or create one, in the same step as approving, so the sign-in comes back with an org
and init adds the repository in the same run. The device flow's contract doesn't change: the
approval names an org the person belongs to, as before (results-v0 §5, api-v1 §8). What init
says when an approval still names none, or names an org other than the repository's, is in C-15
step 3.

## Note, 2026-10-23: asking to join the repository's org

Someone who runs init in a repository of an org they aren't in used to be told to ask one of its
admins for an invitation, out of band. The server now keeps requests to join (cloud api-v1 §6.5,
results-v0 §3): init reads the person's request and, unless one waits already (the approval page
offers the same request), asks once and files it, with the repository's id, so an admin sees who
asks and for what on the org's Members page. Nothing else changes for that person: init still
writes nothing the org would refuse, and the next run, once approved, adds the repository. Filing
from init as well as from the approval page is safe because the server keeps one open request per
person and org and answers a second ask with the open one; init reads first only so as not to ask
the person a question they already answered on the page.
