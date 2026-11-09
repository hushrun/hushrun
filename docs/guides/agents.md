# Agents and local runs

Coding agents push often, and each push makes CI run checks the agent already ran. With
hushrun, an agent runs the checks before it pushes, in the image CI runs them in, on exactly the
files it is about to commit, and signs what happened. CI's plan then counts that evidence and
skips those checks. People validate locally the same way, with their own identity.

To let agents skip the checks this exact code already passed, with `hushrun run -- COMMAND` and
no `hushrun.yaml`, a sandbox needs one variable, `HUSHRUN_AGENT`: see
[Agents in sandboxes](run.md#agents-in-sandboxes). This guide is about deploys: evidence that
CI's plan counts toward a deploy.

This guide assumes hushrun already deploys the repository ([the GitHub guide](github.md)).
Setting up agents takes about 15 minutes: pin images, give each agent a token, which comes with
its machine key, and run `npx hushrun init`, which writes what they read (step 4).

## How it works

```bash
hushrun run --all --isolated --worktree
```

- `--worktree` digests the files as `git add -A && git commit` would record them now (D-10):
  the evidence is about the commit the agent is about to make.
- `--all` runs every check a deploy requires (`deploy.require`, else every check with a `run`)
  that has no valid evidence yet.
- `--isolated` runs each check in its pinned image with Docker or Podman, on an export of
  exactly the digested files (E-12): no network unless the check declares one, only the
  variables its `env` names, never a secret, and never the signing key, which stays with
  hushrun.
- The evidence claims custody `isolated`, and records the image, platform and network it ran
  with (E-7). CI's plan counts it unless the repository is CI-only, since the agent is one of
  the org's signers (P-1, T-15, T-16).

Then the agent commits exactly what it validated, with no other change, and pushes. If the
files differ from what it ran, the digest differs and CI runs the checks itself.

## 1. Pin the images CI runs checks in

An isolated run needs the check's `image`, pinned by digest, and its `platform`: evidence counts
only from the same image and platform (E-7), so pin the image your CI job already uses:

```yaml
checks:
  unit:
    run: go test ./...
    image: golang@sha256:<digest of the image CI uses>   # GitHub: the job's `container:`
    platform: linux/amd64
    env: [GOFLAGS]          # the only variables the check sees; secrets never go in
    # network: none         # the default; `open` for checks that download, `restricted` for
    #                       # a network you restrict (HUSHRUN_RESTRICTED_NETWORK)
```

CI must run the check in the same image. With the reusable workflow, set `isolated: true`: its
test job then runs the plan's checks with `hushrun run --isolated` too, in the pinned images.
Otherwise run the job in that image (`container:` on GitHub) and pass it to `hushrun run` with
`--environment-image`.

A check whose `env` names a secret (`NPM_TOKEN`, `AWS_SECRET_ACCESS_KEY`, `DATABASE_URL`,
anything with `TOKEN`, `SECRET`, `PASSWORD`, `AUTH`, `APIKEY` or `_KEY` in it, and more: E-12)
never runs isolated: keep it in CI. The list is a safety net; name in `env` only what the check
needs.

## 2. Give each agent a token with a machine key

An agent signs with a machine key ([ADR 0028](../adr/0028-machines-registered-at-sign-in.md)):
the server takes its writes because the key is registered with its agent token, and its
evidence counts because the agent is one of the org's signers ([ADR 0031](../adr/0031-membership-is-trust.md),
spec T-15). Nothing goes in `hushrun.yaml`. Every agent token comes with its key, made where the
token is made and registered with it, so every sandbox of the agent signs as one machine:

```bash
hushrun token create --agent claude-code   # at a terminal, shows one string, once
```

The dashboard's **Agents** page makes the same, the key in your browser. Set `HUSHRUN_AGENT` to
the string, which holds the backend, the token and the key; `--format json` gives it to a script as
`hushrun_agent`. The server records the agent's results,
and writes its events, only signed by that key. A key in the repository would be part of what
`--worktree` digests and the agent commits: hushrun refuses a key file there, and an isolated run
refuses to export one.

The string is for sandboxes. A pipeline keeps the token and the key as variables of their own, `HUSHRUN_TOKEN` and `HUSHRUN_MACHINE_KEY` or
`HUSHRUN_MACHINE_KEY_FILE`, which `hushrun token create` shows at a terminal too, and `--format json`
gives a script. Without the key, such a job records nothing, and says to set `HUSHRUN_MACHINE_KEY`.

Revoking the token (`hushrun token revoke claude-code`, or the dashboard) stops what the agent
signs from counting; evidence it signed before keeps counting, when timestamped. A repository
whose deploys should never count an agent's evidence is made CI-only in the dashboard (T-16):
**CI reuses only CI's results**, on its settings page. It isn't in `hushrun.yaml`, so no pull
request, an agent's included, can drop it.

People's laptops work the same way: a teammate's machine, registered at `hushrun login`, counts
like an agent's, with custody `local` at most from a laptop, and a CI-only repository keeps
both out of its deploys.

## 3. Give the agent its environment

The agent's machine or sandbox needs:

- `HUSHRUN_AGENT` (step 2): its token reads the repositories its scope covers and records results,
  and the server takes an event from it only when its key signed it (L-6, L-10). hushrun signs with
  that key when nothing else is set;
- Docker or Podman. `HUSHRUN_CONTAINER_RUNTIME` picks one; exports go under hushrun's cache
  directory, which Docker Desktop, Colima and Podman machines share with their VMs
  (`HUSHRUN_EXPORT_DIR` moves them);
- optionally `HUSHRUN_INVOKED_BY=agent:<name>`, which events record, and run results too;
- the same agent token serves the results of `hushrun run NAME -- COMMAND`
  ([Agents in sandboxes](run.md#agents-in-sandboxes)); its results carry the machine's
  signature too.

`hushrun doctor` checks the key, the server, and whether the server counts the machine as a
signer.

## 4. Tell the agents

`npx hushrun init` writes three things into the repository, so every clone and sandbox has them:

- a skill, in `.claude/skills/hushrun/` for Claude Code and `.agents/skills/hushrun/` for Codex;
  OpenCode and Cursor read both;
- a block in `AGENTS.md`;
- a pre-push hook, with a script in package.json that installs it wherever a package manager
  installs the dependencies: `prepare`, or `postinstall` for Yarn from 2 on. git doesn't clone
  hooks: without package.json, the first `hushrun ready` or `hushrun run` in a clone installs it,
  a sandbox included, where hushrun is installed. A clone where nobody has run hushrun has none.

[`recipes/agents/`](../../recipes/agents/) holds their text. All of it comes down to one command:

```bash
hushrun ready
```

It lists what CI would still run on this exact code, and the commands that run it here:

```text
a0b1c2d isn't ready: CI would run 1 check again.
  missing unit  no pass yet for this exact code
Run these here, then push:
  hushrun run --isolated unit
```

It reads the checks from `hushrun.yaml`'s `deploy`, `package.json`'s `hushrun run` scripts and CI's
own files, in any language, and looks each up by the key `hushrun run` would use: its scope, its
directory and what its scope builds against outside the repository. A `hushrun.yaml` without
`deploy` adds no checks of its own.

A check it can do nothing about here is `ci`, and doesn't count against the code: CI or another
machine has to run it. `hushrun ready --strict` exits 0 only when every check passed. A CI-only
repository's, one that needs a secret, one pinned to an image on a machine with no Docker or Podman; a
job with services or a container, or a condition; one that runs on another operating system; one
whose key CI fills in; one whose tool isn't installed here; and every check, when the repository's CI reuses only CI's results, since a
pass here wouldn't change what it runs. A workflow CI wouldn't start for this push says nothing.
A condition that reads only other jobs' outputs and results, such as a changes filter's `if:
needs.changes.outputs.api == 'true'`, doesn't count: such a job runs when its area changed, which
is when this code has no pass yet, so its checks are listed as any job's. A check with no pass yet
is listed even when its area didn't change on this push, such as right after hushrun is set up,
and the hook asks an agent to run it once.
`--format json` gives the same as `hushrun.ready/v2`, and it exits 10 when a check is left to run.

## 5. What happens on a push

The hook tells the agent the same thing, so an agent that read nothing finds out from the push:

```text
hushrun: push refused: CI would run unit again on a0b1c2d
  unit: no pass yet for this exact code
Run these here, then push again:
  hushrun run --isolated unit
```

By default it refuses a coding agent's push and warns a person's, which goes on. To change that for
everyone, `hushrun hook install --refuse` or `--warn`.

The hook isn't a gate. It runs no checks, `git push --no-verify` skips it, an agent that sets
none of the [variables hushrun knows](../spec/07-configuration.md#78-before-a-push-adr-0036) counts
as a person (`HUSHRUN_INVOKED_BY=agent:<name>` names it), and a push that doesn't use git never
reaches it. CI runs whatever has no pass. So it stops a push only for a check it knows hasn't
passed, and that a command can pass here: a crash, a server that doesn't answer or 15 seconds
without an answer let the push through.

With husky, the hook goes in `.husky/pre-push`, beside your lines. With lefthook, or a pre-push
hook of your own, hushrun prints the lines to add.

## Pull requests

A pull request's pipeline reads `hushrun.yaml` from the branch it merges into, which was
reviewed, never from the change (T-5):

```bash
git fetch --depth=1 origin "$GITHUB_BASE_REF"
hushrun run --all --config-ref "origin/$GITHUB_BASE_REF"
```

hushrun refuses to plan or record in a pull request's pipeline without it. Its jobs sign as the
repository's CI, and what they record counts toward a deploy of the same files, so deploy only
from a protected branch (`deploy.branch`).

## When something is refused

| Message | What to do |
|---|---|
| `pins no image, so it can't run isolated` | Pin the check's `image` (step 1) |
| `never passes secrets in` | Keep that check in CI, or drop the variable from its `env` |
| `declares network: restricted` | Set `HUSHRUN_RESTRICTED_NETWORK` to a network whose egress you restrict |
| `collides with another digested path` | Two paths differ only in case: set `HUSHRUN_EXPORT_DIR` to a case-sensitive directory |
| `the signing key HUSHRUN_SIGNING_KEY_FILE is …, in the content this run exports` | Move the key outside the repository, and drop it from the commit if it was ever added |
| `no valid signature by a machine of a member or an agent of the org` | Sign in with `hushrun login`, or give the agent the string its token came in, as `HUSHRUN_AGENT` (step 2) |
| `here digests to …, not …` | The checkout isn't the content the plan or `--digest` names; use `--worktree` for uncommitted work |
