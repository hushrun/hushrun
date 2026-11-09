# 0015. Organization policy, constraints and the ledger base

- **Status:** accepted;
  superseded by [0031](0031-membership-is-trust.md) on 2026-10-23, except the repository base
  (`HUSHRUN_REPOSITORY_BASE`, T-4), which stays
- **Date:** 2026-10-15
- **Rules:** O-1 … O-7 (new), T-7 (new), T-4, P-9, P-12, I-9 (new), T-2, T-6, T-8, C-2, L-10

## Context

RFC 0001 §4.3 (accepted) adds organization policy (M13): an admin-owned `org.yaml` that says what
an organization wants everywhere, compiled into each repository's `trust.yaml` and signed by the
org root (T-8, [ADR 0012](0012-signed-trust.md)); constraints in trust that bind each team's
`hushrun.yaml` (T-7); and `HUSHRUN_LEDGER_BASE`, one variable that gives every repository its
ledger. The RFC sketches `org.yaml` and leaves open question 4, whether a broken constraint is a
configuration error or a `blocked` plan. This ADR fixes the formats and the behavior. Following
plan §0.1, where the RFC left a choice, it takes the one that deploys less or checks more.

## Decision

1. **A broken constraint is a configuration error: exit 2 (open question 4).** A constraint is
   a property of the pair (`hushrun.yaml`, trust), not of a digest's evidence: no re-run, wait
   or force changes it, only a reviewed change to `hushrun.yaml` or to `org.yaml`. That is what
   exit 2 means everywhere else, T-6's locations and T-2's approval entries included, and
   `blocked` means what evidence decided. So `plan --stage T` exits 2 before reading events
   when `hushrun.yaml` breaks one of `T`'s constraints, and records nothing. The objection to
   exit 2, that the reason would hide in a log, is answered without a decision: on GitHub each
   broken constraint annotates its line of `hushrun.yaml`, which the pull request shows, and the
   step summary lists them. `policy validate --trust` checks every stage, so a pull request
   learns before it merges.
2. **Constraints are per stage, and only `plan` enforces them.** A broken production
   constraint doesn't stop staging: an organization that adds one doesn't freeze every
   repository's staging deploys. `run`, `attest` and `mark` record facts and never refuse to:
   refusing a `failed` would hide a failure. `doctor`, `status`, `policy explain` and
   `ledger put-trust` report broken constraints; an upload never fails on one, since trust may
   tighten what `hushrun.yaml` must say.
3. **Constraints say only what tightens.** `upstream: passed`, `allow_force: false` and
   `on_ledger_error: fail` are their only values; the defaults constrain nothing and are
   refused. `require` lists requirements as `hushrun.yaml` writes them. `after` is a chain,
   nearest first, that the stage's chain must start with, so one constraint can say
   "production after canary after staging". Two constraints no `hushrun.yaml` can meet at once
   are an error in trust. Each carries an optional `from`, where it was written, which the
   compiler fills with `org.yaml`'s line: errors name it, as RFC 0001 asks.
4. **Older readers refuse constraints.** `trust.yaml` rejects unknown keys (C-1), so a CLI
   before M13 refuses trust with `constraints` instead of skipping them: the safe direction.
   Pipelines upgrade before their organization adds constraints.
5. **Org-owned checks: evidence a stage can't produce blocks (P-12, I-9).** A source check is
   produced *elsewhere* at `T` when no principal accepted for it at `T` is accepted for
   `events@T`, the lifecycle events `T`'s own pipeline signs; the planner can tell that from
   trust alone. Before, such a check went into `checks_to_run`: the pipeline ran it, recorded
   evidence that didn't count, and deployed, since the deploy job doesn't plan again. Now it
   blocks, like a deployment or approval check, naming who may attest it; forcing never waives
   it; `hushrun run` without names skips it; `policy explain` says `producedBy: elsewhere`. This
   is what makes an org-owned check hold: the organization requires `sast` and accepts it only
   from its security team's principal, so a team's `run: "true"` changes nothing. The same rule
   catches a hand-written trust that accepts a check at production only from staging.
6. **`org.yaml` is compiled, not inherited** (RFC 0001 Appendix B). `hushrun org plan` writes
   one `trust.yaml` per repository; nothing merges at run time, and `policy explain` shows a
   file. The compiler is pure and deterministic (`hushrun_core::org`), so the index and the
   admin surface compile the same bytes.
7. **Templates name a stage's pipeline.** A template whose name has `{stage}` is one principal
   per stage: the stage's pipeline. Each stage accepts, by `default`, its own pipeline and its
   upstreams', so evidence and artifacts are reused along the path, and `events@<stage>` only
   from that stage's pipeline, for every stage of the chain (T-2 needs the direct upstream's,
   and P-13 reads every upstream's attempts). Every stage of an entry gets an `accept` entry, so
   T-2's built-in fallback, every `ci` principal, never applies there: a security team's
   principal counts only where it is named. A per-stage template lists no keys, which every
   stage's principal would share (T-1); entries give keys per compiled principal, or the
   template names identities, which need none (T-10): a pipeline's names its repository by
   `{repository_id}`, its `ref` and its `workflow`. A template's identities are checked as T-10
   says, with each placeholder standing for a value of its kind, so a mistake is an error at
   its line rather than in some repository's compiled file.
8. **Enforced and default settings.** A stage's `enforced` settings apply to every repository;
   its `defaults` apply until a repository replaces them, through `registries` for locations or
   an `exception`, which needs a `reason`. Nothing loosens an enforced setting, in `defaults`
   as in an entry: a lower `min_custody`, a location outside the enforced prefixes, an enforced
   `accept` key given other principals, a lower audit rate, or a contradicting constraint is an
   error. Constraints only add, so enforced and default constraints both apply, each with its
   own `from`. `approvers` are `accept` entries for approval checks whose principals can't have
   custody `ci` (T-2), checked when `org.yaml` loads. Compiled files list their repository's
   exceptions, with reasons and lines, in comments, so that signers review them.
9. **Provenance lives in trust, apart from it.** An `org` block records the organization,
   `org.yaml`'s path, the last commit that changed it (not `HEAD`: committing the compiled files
   must not change them), and its digest; with uncommitted changes there is no revision.
   Comparisons leave the block out, so a commit that changes nothing for a repository asks
   nobody to sign its trust again.
10. **Plan, sign, apply.** `org plan` writes, per repository that changed, an envelope one
    serial above the ledger's (counting only envelopes a root version signed, ADR 0012 item 6)
    and valid for `signed_trust.valid_for`; unchanged trust is signed again within
    `renew_within` of expiry. A re-plan keeps an envelope with the same trust and serial, and
    its signatures. `org sign` checks everything before it signs anything: `org.yaml` at the
    planned digest and revision, and each envelope and the `trust.yaml` beside it exactly what
    `org.yaml` compiles to, so a signer signs what was reviewed. `org apply` uploads what a
    threshold of the org root signed once it verifies against the ledger's chain with a higher
    serial; with `--roots` it first uploads missing root versions, create-only. A ledger whose
    newest root isn't `org.yaml`'s is reported, not changed.
11. **Credentials for many ledgers.** Hosted ledger tokens are per ledger. `hushrun org` runs
    `HUSHRUN_ORG_TOKEN_COMMAND` once per `https://` ledger, with `HUSHRUN_LEDGER_URL` set, so a
    command can look up each ledger's admin token; or sends `HUSHRUN_ORG_TOKEN`, an
    organization-wide token, to ledgers on `ledger_base`'s origin only. Cloud ledgers keep their
    ambient credentials. Org commands send no GitHub OIDC token: the policy repository's
    identity is not the repositories'.
12. **`HUSHRUN_LEDGER_BASE` (amends T-4).** In CI, without `HUSHRUN_LEDGER`, the ledger is
    `<base>/<repository id>`, the id from the CI provider, never from the tree. `hushrun.yaml`'s
    `repository` must be that id exactly (before, a mismatch was a warning) and its `ledger`
    that URL, or commands exit 2: a changed base can't redirect a repository silently.
    `HUSHRUN_LEDGER` wins when set, with a warning when the base gives another URL, so a
    repository can keep its own ledger; generic CI, which reports no repository, needs it.
    Exactness includes case: GitHub reports `Acme/Back` as it is spelled, and ledger paths keep
    case. The derived URL counts as named outside the repository, so it gets the environment's
    tokens (L-10).
13. **A hosted ledger's log key is written in `org.yaml`, never fetched (T-9).** Each entry may
    name `ledger_log`, the origin and key of its ledger's transparency log, as the ledger's
    administrators publish them; the compiled trust carries it as it is. T-9 has trust name the
    key so that the ledger can't vouch for itself, so `org plan` doesn't fill it in from what
    the ledger says. It compares the two, and warns when the ledger keeps a log the entry
    doesn't name, when they differ, or when applying would drop a key the ledger's trust names
    now. The origin is the one the ledger's checkpoints name, which a base-derived URL doesn't
    change.

## Consequences

- An organization adds a constraint, and repositories that don't meet it stop planning that
  stage, with an annotation on the line to change. `doctor --org` lists them before the trust
  is applied, and `policy validate --trust` in pull requests catches the rest.
- An `org.yaml` entry and the repository's `hushrun.yaml` must agree on stages: trust that names
  a stage `hushrun.yaml` lacks is an error, as it was for `accept` (T-2). `doctor --org` reports
  it per branch.
- Root signers sign once per repository per change, in one pass each; unchanged repositories
  ask nothing. Expiry still asks for renewals, which `org plan` prepares; who signs them
  routinely stays RFC 0001's open question 5.
- The compiled directory, with `plan.json`, can be committed to the policy repository and
  reviewed in a pull request: signatures are added to its files.
- A stage that `hushrun.yaml` has and `org.yaml` doesn't list falls back to T-2's built-in
  acceptance. The organization governs the stages it lists.

## Alternatives considered

- **A broken constraint as `blocked`, exit 3.** It would put a configuration problem among
  decisions that evidence changes, leave `run`, `attest` and the index treating it as one, and
  still need annotations to point at the line.
- **Constraints checked by every command, for every stage.** One broken production constraint
  would stop staging, and refusing to record a failure deploys more later.
- **Org-owned checks marked in trust**, say `external: [sast]`. A second statement that could
  disagree with `accept`; the acceptance already says who produces a check.
- **Running org-owned checks as `refresh` with a warning.** The deploy that follows would carry
  no valid evidence for them.
- **Provenance in the envelope's payload.** Readers would ignore it, but the per-constraint
  lines, which errors need, belong with the constraints, in the text signers review.
- **`HEAD` as the revision.** Committing the compiled files changes `HEAD`, and with it every
  compiled file: no signature would survive its own commit.
- **One admin token for every ledger.** The hosted ledger's tokens are scoped to one ledger; a
  command per ledger fits that, and a vault.
