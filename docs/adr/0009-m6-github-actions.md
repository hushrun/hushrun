# 0009. GitHub Actions: choices made in M6

- **Status:** accepted; item 8's exact version superseded by
  [0035](0035-updates-and-the-newest-release-by-default.md): the setup action installs the newest
  release unless a workflow pins one
- **Superseded in part by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-14
- **Rules:** P-6, P-7, A-7, C-2, D-7, E-6, E-7, T-4, §10.2, §10.4, §11.2, §15

## Context

M6 makes GitHub Actions a native provider and adds `run`, `init`, `doctor`, the setup action,
the recipes, the user guide and an e2e driver. The plan describes these closely, but leaves
some edges open. Following plan §0.1, each choice below runs more checks or deploys less.

## Decision

1. **Run identity comes from GitHub**: `GITHUB_RUN_ID`, `GITHUB_RUN_ATTEMPT` and the run's URL.
   `HUSHRUN_RUN_*` doesn't override them on GitHub, which keeps one run's jobs in one attempt.
   The trigger follows `GITHUB_EVENT_NAME` (§11.2); other events, such as pull requests, say
   nothing, so `plan` treats them as manual. For `workflow_run`, the event payload names the
   upstream run, which the plan's step summary shows.
2. **Runner output.** Outputs are appended to `$GITHUB_OUTPUT`, multi-line values in a heredoc
   whose delimiter is chosen so it can't occur in the value. Step summaries are Markdown tables.
   Annotations go to stderr, keeping stdout for the command's output, and their data is escaped
   so a reason can't inject another workflow command. A blocked plan is an error annotation,
   `wait` a notice, a stale `guard` and `resolve`'s fallback warnings.
3. **The API is optional.** Liveness and pending runs need `GITHUB_TOKEN` or `GH_TOKEN`
   (`actions: read`); `GITHUB_API_URL` selects GitHub Enterprise Server. Without a token, or
   when the API fails, `plan` warns and relies on leases and upstream grace (§10.4). Only the
   upstream stage's attempts without a verdict are asked about, and only those GitHub recorded.
4. **Pending runs need `ci.github.workflow` and the upstream's `branch`.** A queued or running
   run counts until its current attempt records something. It matches this digest when its
   tree equals the checked-out revision's; otherwise hushrun fetches the commit
   (`git fetch --depth=1 origin <sha>`, no credential prompts) and digests it with the current
   scope definitions. A run it can't match is ignored, with a warning.
5. **`run`** sends each check's stdout to hushrun's stderr, so `--format json` stays parseable.
   Without a plan, and with no check names, it behaves like `--all`. A check that pins an image
   is refused, not just warned about, when `--environment-image` is missing or names another
   digest: that evidence could never count (E-7), so the stage would refresh forever. A timeout
   kills the check and records a failure with no exit code. `--isolated` is refused until M8.
6. **`init`** writes keys (mode 0600, in a 0700 directory) and `trust.yaml` outside the
   repository, by default in the user's config directory, and refuses a directory inside the
   repository, comparing resolved paths. It keeps keys that exist, so running it again doesn't
   rotate them. The repository id comes from `origin`, with a lowercase host (C-2).
7. **`doctor`** exits 2 when a check fails and 0 with warnings. Registry access is only a
   warning, because credentials often exist only in CI. Clock skew over 30 s is a warning (P-6
   compares within 60 s). The baked-configuration scan (A-7) reads at most 64 MB of JavaScript,
   TypeScript and `.env*` files. The D-7 check treats a workflow file that mentions `hushrun` as
   a deploy workflow.
8. **Recipes pass expressions to `run:` scripts through `env:`**, never pasted into the script,
   following GitHub's guidance on script injection. The setup action installs an exact version,
   verifies the archive's SHA-256 (the release's checksum file, or an expected hash given as
   input), and fails on anything else.
9. **The e2e driver** (`cargo xtask e2e --repo OWNER/NAME`) needs a sandbox repository set up
   as the guide describes. It pushes commits, waits for the workflows, and reads each plan's
   decision from the run log.

## Consequences

- Everything except the e2e driver is tested locally, against a fake GitHub API on the
  loopback and a real git remote. The driver and the guide's 30-minute adoption still need a
  sandbox repository.
- `run`'s refusal of mismatched images is stricter than `attest`, which records evidence
  others produced and only warns.

## Alternatives considered

- **Letting `HUSHRUN_RUN_ID` override GitHub's run id.** It would let a job split one run into
  several attempts' worth of events; nothing needs that.
- **Failing `plan` when the GitHub API fails.** ci-promote blocked on API errors; hushrun's
  ledger decides instead, and a missed queued run costs duplicate work, never a wrong deploy.
- **Writing keys next to the repository.** One `git add -A` away from a leaked signing key.
