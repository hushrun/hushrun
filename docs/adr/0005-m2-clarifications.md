# 0005. Events, ledger and configuration: choices made in M2

- **Status:** accepted
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-14
- **Rules:** E-5, L-3, C-5, C-6, C-8, T-2, T-5, §10.4, §11.2, §12.4

## Context

M2 implements events, signing, the file ledger and configuration. A few places in the plan
left a choice open. Following plan §0.1, each choice below runs more checks or deploys less.

## Decisions

1. **The `Ledger` trait is synchronous.** It has `get_many` for the bounded parallel reads of
   §12.4; network backends (M4) run their own async runtime behind it. Commands then need no
   async code, and the file and mem backends stay simple. L-3's contract is unchanged.
2. **Artifacts in `deployed` and `passed` events** are `{name, ref, digest: {sha256}}`, the
   same digest set as `artifact` events. E-5 now says so.
3. **`default`, `artifact` and `events` can't name a check**, because `trust.yaml`'s `accept`
   uses them as keys (T-2). C-8 now lists it.
4. **Approval checks have no `run`**: people approve them (`hushrun approve`, M8). C-5 now says
   so.
5. **`mark deployed` and `mark passed` need every artifact of the scope**, not just one, because
   every stage deploys every artifact of its scope (C-6). They exit 2 otherwise.
6. **`policy validate` reads the working tree's `hushrun.yaml`** by default (`--rev` and
   `--config` choose another). It is an authoring tool that never touches the ledger (T-4).
   Every other command reads the policy from the revision it evaluates (T-5), never from
   uncommitted edits.
7. **`keys generate` without `--out`** writes the PEM to stdout and the public key and key id to
   stderr, so `hushrun keys generate > key.pem` works; `--format json` needs `--out`, so no
   private key ever appears in JSON.
8. **Local runs get a new run id per command** (§11.2), unless `HUSHRUN_RUN_ID` groups several
   commands into one run.
9. **A `file://` ledger whose directory doesn't exist is unavailable** (exit 6), not empty, so a
   mistyped path can't look like a ledger with no failures (L-5).
10. **Until their milestones, GitHub, GitLab, Buildkite and CircleCI are detected but identify
    runs like the generic provider** (`HUSHRUN_RUN_ID`, `provider: generic`). T-4's CI rules
    apply as soon as any provider is detected.
11. **`trust.yaml` needs at least one principal with at least one key**; keyless `identities`
    are refused until M10.

## Consequences

- Each choice is enforced by a test, and the spec text names the ones that are normative.
- Moving the ledger trait to async later would be an internal refactor: no contract depends on
  it.
