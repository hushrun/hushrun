# 0026. Trust anchored in the repository, so that `init` secures the ledger

- **Status:** accepted; its decisions resolved by the maintainers on 2026-10-20 (below), and completed
  by [0028](0028-machines-registered-at-sign-in.md), which registers a machine at every sign-in;
  item 18 amended on 2026-10-21: `org apply` keeps what a team adds (below, and O-8); items 12
  and 13 and decision 7 amended on 2026-10-22: the fallback and the migration are gone (below);
  its trust (items 1 to 16, 18 and amendment 1) superseded on 2026-10-23 by
  [0031](0031-membership-is-trust.md): membership is trust, and `hushrun.yaml` has no `trust:`
- **Date:** 2026-10-20
- **Rules:** T-14 (new), T-4, T-5, T-8 (retired), T-2, T-9, T-10, T-11, T-12, T-13, C-3, C-7,
  C-12, C-15, E-4, L-2, L-3, L-6, L-9, L-10, O-1, O-5, O-7; CP-3 and CP-5 (RFC 0001 §2.3)
- **Supersedes:** [0012](0012-signed-trust.md) whole. **Amends:**
  [0013](0013-keyless-signing.md) item 7 (the trigger) and item 1's root and trust envelopes;
  [0014](0014-key-windows-and-retirement.md) item 4 (where inventories live);
  [0015](0015-org-policy-and-constraints.md) items 6, 10, 11 and 13 (the org root, signing,
  the log key); [0016](0016-onboarding.md) item 7; [0017](0017-transparency-log.md) items 5, 6
  and 15; [0020](0020-proven-listings-and-witnesses.md) item 6 and its migration;
  [0021](0021-witnesses-that-verify-the-map.md) item 13; [0022](0022-https-ledgers-only-and-no-kms.md)
  item 3; [0024](0024-init-sets-up-hushrun-run.md) item 1 (the wizard writes `hushrun.yaml`);
  [0025](0025-init-gets-the-repositorys-ledger.md)'s consequences. RFC 0001 §4.3's signed trust
  and §4.7's pin; RFC 0002 §3.4 (D2) and §3.6's "Sign". The specs change with the code that
  implements each item; the other ADRs keep their text, with this one named in their status.

## Context

Since ADR 0025, `npx hushrun init` gets the repository its ledger, and the dashboard shows it
as "not secured · no root". It stays unusable until people do six things in two places: make a
browser key, write `root/1.json` with it, sign `trust.yaml`, upload both (`ledger put-root`,
`put-trust`), and commit the pin, `ledger_root`, to `hushrun.yaml` (T-8, C-3). init can do none
of it, by design: ADR 0012 has people sign the root with their own keys, so that the server
never serves the trust that verifies it (RFC 0001 §4.7), and CP-5 lets no service sign it. The
owner's goal is that init does all the work it can, and that a ledger init made is ready for
promotion.

The root protects the list of who may write. It doesn't protect the writers, and since keyless
signing (ADR 0013) the writers are the repository's:

- A pipeline's identity is `{issuer, repository_id, ref, workflow}` (T-10). Fulcio certifies it
  for whatever code runs as that workflow on that ref. So whoever can change
  `.github/workflows/deploy-staging.yml` on `develop` signs as staging, whatever the root says.
- The code host is already trusted completely: GitHub issues the ID tokens that Fulcio turns into
  identities, and runs the workflows.
- So the weakest gate in front of every keyless principal is the stage branch's protection. The
  root puts a stronger-looking gate, people's keys and a threshold, in front of a list whose
  entries are only as safe as that branch.

What the root still buys is worth naming:

- **A threshold of named people** (two of three) for changing trust, independent of the host's
  admins, who can bypass branch protection.
- **Trust for key principals** on hosts whose ID tokens Fulcio doesn't certify, where the list of
  public keys is the whole defense.
- **A ledger that can't vouch for its own trust** (RFC 0001 §4.7). Trust in the repository keeps
  this: the server holds none of it.

T-4's principle stays: trust never comes from the content under evaluation. A protected default
branch isn't that content. T-5 already treats it as reviewed, which is why a pull request's
pipeline reads its rules with `--config-ref <base branch>`.

## Decision

### Where trust lives

1. **Trust is the `trust:` section of `hushrun.yaml` on the default branch (T-14, new; retires
   T-8).**
   - The section has `trust.yaml`'s keys (§7.2) except `version` and `repository`, which come
     from `hushrun.yaml`. `trust.yaml`, `trust.dsse.json`, root versions and the pin go.
   - **The tip, never the revision.** Readers take the section from the tip of the default
     branch when the command runs, never from the revision under evaluation, unless that
     revision is the tip. A change counts once it is merged; a pull request, a pushed branch or
     an unmerged commit never adds its own signer.
   - **Which repository and branch.** In CI they come from the provider, never from the tree:
     on GitHub, `GITHUB_SERVER_URL`/`GITHUB_REPOSITORY` and the event's
     `repository.default_branch`, else `git ls-remote --symref <url> HEAD`; on GitLab,
     `CI_PROJECT_URL` and `CI_DEFAULT_BRANCH`. `HUSHRUN_TRUST_REF`, from the CI's settings, names
     another branch. Locally, the remote whose URL is `hushrun.yaml`'s `repository`, else
     `origin`, and its default branch as `refs/remotes/<remote>/HEAD` names it, else as the
     remote answers; a laptop decides no deploy, so a fork's `origin` only misleads its own
     `status`. A checkout with no remote has no default branch to read: a command that reads a
     ledger exits 2 and says so (`--trust-from-tree` is for experiments), except through the
     migration's fallback (item 13).
   - **hushrun fetches it.** `git fetch --no-tags --depth=1 <remote> refs/heads/<branch>:refs/hushrun/trust`,
     with the checkout's own credentials, then `git cat-file`, as `--config-ref` reads (D-9). A
     fetch that fails exits 6, like a ledger that can't be read (L-5); a default branch without
     the section exits 2, naming the commit it read.
   - **The id must match.** When the CI provider reports the repository's numeric id
     (`GITHUB_REPOSITORY_ID`, `CI_PROJECT_ID`), every identity's `repository_id` must equal it, or
     the command exits 2: trust copied from another repository fails loudly.
   - **Policy keeps T-5.** Checks, stages and artifacts still come from the revision under
     evaluation, or from `--config-ref` in a pull request. Since trust now comes from another
     revision, `accept`, `min_custody`, `locations` and `constraints` entries for a stage that the
     evaluated `hushrun.yaml` doesn't have are skipped with a warning, instead of T-2's error.
   - **Inventories** (T-11) are files beside `hushrun.yaml` on the default branch, still pinned by
     digest.
   - **Elsewhere.** `--trust FILE` (plain YAML) stays for commands that read no ledger
     (`policy validate`, `policy explain`) and for `verify` on a stored event. `--trust-from-tree`
     stays for local experiments, and exits 2 in CI for any command that reads or writes a
     ledger.

2. **In CI, the ledger comes from the default branch too (amends T-4).** Without
   `HUSHRUN_LEDGER` or `HUSHRUN_LEDGER_BASE`, a command uses the default branch's
   `hushrun.yaml` `ledger`. T-4 kept the ledger out of the tree so that a change couldn't point
   a plan at a ledger that replays events selectively; the default branch's file can't be
   changed by an unreviewed change, which is the same bar as trust. Tokens stay put:
   - the job's ID token, whose audience is that ledger's URL (L-10), so no other server accepts
     it, as ADR 0023 argued for results;
   - `HUSHRUN_LEDGER_TOKEN` and `HUSHRUN_LEDGER_TOKEN_COMMAND`, bearer secrets, still go only to
     a ledger `HUSHRUN_LEDGER` or `--ledger` names.

   When `HUSHRUN_LEDGER` is set, it must equal the default branch's `ledger`. CI then needs no
   variable for promotion, and RFC 0002's D2 (asking the hosted service) isn't needed.

### What init writes

3. **init derives trust from what it already reads.**

   | Field | From |
   |---|---|
   | `repository_id` | `gh api repos/<owner>/<name> --jq .id` (ADR 0025 item 5); else the `github_repository_id` the ledger answer names (the App's, or CI's pin); else GitHub's public API for a public repository; else none (item 19, what is left) |
   | the default branch | `git ls-remote --symref origin HEAD` |
   | principal `ci`, custody `ci` | one identity per workflow init found running a wrapped check (C-15 step 1): `ref: refs/heads/<default>`, that `workflow` |
   | principals `ci-<stage>` | per stage: `ref: refs/heads/<stage branch>` and the stage's workflow, from the stages `--pipeline` writes, or the deploy workflows `adopt` detects (ADR 0016 item 4): their `on.push.branches` and file |
   | `accept` | as O-5 compiles it: `default` lists the stage's pipeline, its upstreams' and `ci`; `events@<stage>` only that stage's pipeline |
   | `ledger_log` | `GET {ledger}` once: the origin and log key the ledger states (T-9) |

   GitLab gets `{issuer: https://gitlab.com, repository_id: <project id from glab>, ref,
   workflow: .gitlab-ci.yml}`. On hosts whose ID tokens Fulcio doesn't certify, `--pipeline`
   generates key pairs as it does now, writes the public keys into the section, and says where
   the private ones go. init writes no laptop, agent or approver principal, and no witnesses:
   those are people's choices (item 19), and machines registered at sign-in count only once a
   commit names them ([ADR 0028](0028-machines-registered-at-sign-in.md) item 6).

   **A private repository's names in a public log.** Public-good Sigstore's CT log publishes
   every certificate, and with it the repository's and the workflows' names. For a private
   repository (`gh api` says `private`), init's plan says so and asks for consent before it
   writes keyless identities; `--yes` consents, since the plan it applies shows the line. Without
   consent, init writes no `ci` identity and says how to sign with a key file instead
   (`hushrun keys generate`, the public key under the principal's `keys`, the private one in a CI
   secret), until RFC 0002's D1 gives a private default (M24). Few people can read a private
   repository, so what the consent is about is only the names in Sigstore's log.

   Taking `ledger_log` from the ledger is trust on first use, which ADR 0015 item 13 refused.
   Here the key lands in a commit that people review, and only a commit changes it afterwards:
   the server can't swap logs or histories later, which is what T-9 is for.

4. **init writes `hushrun.yaml` once it has a ledger, in every repository it sets up (amends
   C-7, ADR 0024 item 1)**, checks-only ones included, so that no ledger sits without trust. A
   `hushrun.yaml` without stages is valid: it names `repository`, `ledger` and `trust`, and
   `plan` exits 2 until it has a stage. The wizard writes those keys. To an existing
   `hushrun.yaml` it adds the section, edited as lines like workflows (ADR 0024 item 3); a
   section that differs from what init derives is `kept`, and the plan shows the difference.
   Without a ledger (a member's init before an admin's, ADR 0025 item 3) it writes no
   `hushrun.yaml`, and says so.

5. **init protects the branches, when the person may, by default.** With `gh` signed in as a
   repository admin, the plan includes one more change, applied with the rest unless the person
   answers no: a ruleset, `hushrun`, on the default branch and each stage branch, requiring a
   pull request with one approval, dismissing stale approvals, blocking force pushes and
   deletions, with no bypass actors. Two cases change it:
   - **One writer.** GitHub never lets an author approve their own pull request, so in a
     repository where the person running init is the only one with write access, a required
     approval and no bypass would leave them unable to merge. There the ruleset requires a pull
     request with no approval, and still blocks force pushes and deletions; the plan says so,
     and says to raise it to one when a second person joins (`doctor` warns once there are two).
   - **No rulesets on the plan.** A private repository on GitHub Free can't have rulesets
     (`gh api` answers 403 or 422). init says so, applies the rest, and `doctor` keeps warning.

   Without admin rights it says what to ask an admin for, and `doctor` keeps saying it.

6. **The ledger is secured when init's commit lands on the default branch.** No key, signature,
   upload or pin. The reviewed merge is the signature.

### Signatures

7. **CI signs keylessly, and readers check the certificate against trust (T-10, T-12).**
   - In CI, a job with an ID token and no key signs with `sigstore` by default (amends T-12).
   - The claims stay ADR 0013's: issuer, Source Repository Identifier, Ref, Build Config URI and
     Build Signer URI.
   - **The trigger (amends T-10).** A certificate whose Build Trigger (`1.3.6.1.4.1.57264.1.20`)
     is `pull_request_target` matches only an identity whose new `trigger` field names it. That
     event runs the base branch's workflow with the base branch's ref, and is how a fork's code
     usually ends up running as the base. init writes no `trigger`.
   - **Environments can't be checked yet.** The proposed direction had the certificate's
     environment checked against trust; Fulcio's certificates carry none. GitHub's ID token
     does (`environment`), so T-10 gains `environment` with ID-token signatures (RFC 0002 D1,
     M24). Until then the stage's branch protection is the control, and init writes a deploy
     job's environment as a comment beside its identity.

8. **The server stores (L-6, L-10).** CI's ID tokens write under `v1/` for the repository id the
   ledger pinned (ADR 0025); people and agents write under `v1/` from a machine registered at
   sign-in, whose key signed the event ([ADR 0028](0028-machines-registered-at-sign-in.md)
   item 4); members read; the admin role keeps only deletions (L-9). No configuration write
   remains, since trust and roots aren't on the server. So the server decides who may write,
   but can't add a signer that readers count or forge an event (E-8): every signer a reader
   counts is a key or an identity the default branch's trust names. It can refuse, delay and
   withhold. What withholding gets is unchanged, and the proposed "fails closed" is only partly
   true:
   - with witnesses in trust (T-13), a listing that leaves out a key is refused before the
     decision (H2);
   - without them, the default init writes, a deploy decided from a short listing is found by
     the monitor afterwards (H1). Hiding a whole upstream attempt still lets an older passing
     attempt promote; hiding every attempt makes production plan `full` and rebuild and retest
     (P-15).

9. **GC statements are signed by a principal trust names under `gc` (amends L-9, ADR 0017 item
   15),** instead of a root key: a key, or the identity of a scheduled workflow on the default
   branch. Without `gc`, GC refuses to run on a checked log, as it does without a root key now.

10. **Readers without a checkout get a copy, for display (E-4, L-2).** The index and the
    dashboard can't run `git fetch` on a private repository without the App. So `plan --start`
    also uploads the default branch's `hushrun.yaml`, create-only, to `v1/policies/<hex>.yaml`
    (L-2), and events gain `trustDigest` and `trustRevision`, optional and informational. The
    index takes the trust of the newest event whose certificate names the ledger's pinned
    `repository_id`, the default branch's ref (from the App, or `default_branch` in init's
    `POST /v0/ledgers`, an admin's word like `github_repository_id`) and a trigger other than
    `pull_request_target`; with the App, it reads the branch directly. Readers that decide never
    read this copy (CP-1).

### A new developer who clones the repository

11. What stops each thing they could try:

    | They | What stops it |
    |---|---|
    | sign in to the server (`hushrun login`) | Their sign-in registers their machine (ADR 0028): it reads the org's ledgers, records results (C-12), and writes events their machine key signed. None of those events counts for a promotion: readers count a machine key only where the default branch's trust lists it (E-8), and a reviewed commit puts it there |
    | sign an event on their laptop | With their machine key, it matches only keys trust lists. Keyless, Fulcio certifies their email; it matches only subject identities that trust lists for approvers (T-10). Neither is a `ci` principal |
    | push a branch with a changed deploy workflow | The certificate's ref is their branch's; no identity names it. **Every stage branch must be protected**, not only the default one: on an unprotected `develop`, anyone who can push signs as staging. init's ruleset covers stage branches, and `doctor` warns about each identity whose ref isn't protected |
    | open a pull request that changes trust | Readers take trust from the default branch's tip (item 1), and the pull request's own pipeline reads its rules from the base (T-5). A fork's pull request gets no ID token; a `pull_request_target` run matches no identity (item 7). The change counts after a reviewed merge, by people the ruleset or CODEOWNERS name |
    | dispatch a workflow on the default branch | It runs the reviewed workflow, which signs only what it already would |
    | approve a deploy | `approve` writes nothing unless their identity is under an approval principal for that stage (ADR 0018 item 17) |
    | run checks on their laptop (ADR 0018) | `hushrun run --stage` evidence counts only where trust accepts its principal, under T-3's `min_custody`, P-16's protected paths and P-17's audits. init writes no laptop or agent principal, so laptop evidence counts nowhere until a reviewed commit adds one. Results without a stage (C-10) aren't ledger events: no plan reads them, and they decide only whether a check runs again (ADR 0023 item 2) |

### The root chain

12. **The root chain goes, after one release of migration; it doesn't stay opt-in.** Hosts
    without usable ID tokens sign with key files whose public keys trust lists, and repository
    trust serves them as it serves identities: the root never signed events, it guarded the
    list. An opt-in root would keep two trust models, the cost ADR 0022 removed `file://` to
    avoid, for hosts whose branches can't be protected, and those can't protect their deploy
    pipeline either.
    - **This release** removes what makes or changes a root or signed trust for one repository:
      `trust root init`, `trust sign`, `ledger put-root` and `ledger put-trust`. Nothing should
      start a root now, and a ledger whose trust must change migrates instead (item 13). It
      keeps what reads them: the fallback, init's migration, and `ledger gc`'s root keys for a
      ledger with a root until `gc` principals exist (item 9).
    - **Organizations** keep `org plan`, `org sign` and `org apply` for this release, with a
      warning, so that their repositories' signed trust can still change until `org apply`
      opens pull requests (item 18).
    - **The next release** removes the fallback, the organization commands' signing, `root.v1`
      and `signed-trust.v1`, and the server's root and trust objects become read-only history.

### Migration

13. **init migrates a ledger with a root.** In a repository whose ledger has signed trust, the
    plan shows: the signed trust, verified against the current pin, written as the `trust:`
    section; inventory files copied beside `hushrun.yaml`; `ledger_root` removed. For one
    release, readers whose default branch has no section fall back to the ledger's signed trust
    and the pin, with a warning. The fallback isn't a downgrade: that trust is still the
    admins'. Root objects stay in the ledger, create-only and unread. After that release a
    `ledger_root` is ignored with a warning, never an error (C-1), so old files keep parsing.
    A ledger with no root and a default branch with no
    section exits 2, naming the commit it read and saying to merge the commit
    `npx hushrun init` made.

14. **hushrun cloud.**
    - The Connect page's "Secure this ledger" goes. The ledger's header shows where trust comes
      from (`hushrun.yaml on main @ 1a2b3c4`, from item 10 or the App), or "no trust on main
      yet: merge the commit `npx hushrun init` made", and warns when a stage branch is
      unprotected. "not secured · no root" goes.
    - The root editor and the trust change flow (collecting root signatures) go. The trust editor prepares the section's diff, opens a pull request through
      the App when installed, and otherwise shows the lines to commit. Trust history is the
      default branch's commits. Browser keys stay, for approvals and laptop principals that a
      commit adds.
    - The App's adoption templates write the `trust:` section
      from the repository id, stage branches and workflow files the App writes, and drop the
      `ledger_root` comment and the "Secure the ledger" step. Their branch protection note stays,
      worded as what it now is: without it, anyone who can push to the branch signs as its
      stage. `drift.rs` and `doctor --org` compare the default branch's section.
    - **CP-3 and CP-5 change.** The App opens pull requests, so it has contents: write. On a
      protected default branch, without bypass, it can only propose trust; on an unprotected one
      it could push a writer, as could anyone with push access. CP-3 becomes "the control plane
      holds no key or identity that trust accepts, and can't change trust on a protected
      default branch"; CP-5 becomes "trust changes are reviewed merges".

15. **`doctor`** replaces "root" and "signed trust" with: "trust" (repository, branch, commit,
    principals); "branch protection", per identity's ref, from the rules API
    (`GET /repos/{o}/{r}/rules/branches/{b}`, readable with contents: read), with classic
    protection reported as unknown without admin rights; "ledger log key", trust's against what
    `GET {ledger}` states; and a warning when the evaluated revision's section differs from the
    default branch's, so that nobody thinks an unmerged edit is in force.

16. **`verify` and `ledger show`** read trust as item 1 says, and print the commit they read.
    `verify --trust FILE` still verifies a stored event offline.

17. **Logs and witnesses** (ADR 0017, 0020, 0021). `ledger_log` and its witnesses are part of
    the section. init adds no witnesses: H1, found after the fact, is the default. Every new ledger's trust names its log key from init on, so ADR 0017 item 6's
    warnings apply only to migrated ledgers. Adding witnesses, `quorum` or `map_quorum`, or
    dropping `verify_map` (ADR 0021 item 13), is a commit. `ledger monitor` reads the key from
    the default branch, or `--trust FILE`. `hushrun witness serve --verify-map` doesn't change.

18. **Organization policy (ADR 0015) loses its root.** `org plan` still compiles `org.yaml`.
    `org apply` opens one pull request per repository that changes its section, through the App
    or `gh`, merged under the repository's review rules; `org.yaml`'s `root` goes, and with it
    `org sign`. A team can drop an org constraint with a reviewed merge; `doctor --org` and
    `drift.rs` report every repository whose default branch differs from what `org.yaml`
    compiles to. *Amended on 2026-10-21 (Amendment 1, below):* the pull request merges what
    `org.yaml` compiles into the section and keeps what the team added itself, and "differs"
    means differs from that merge.

### What `init` does, end to end (replaces C-15's steps)

19. **init's steps**, replacing C-15's:

    1. **Look**, as now, plus: the default branch; the workflows that deploy, their branches and
       environments; an existing `hushrun.yaml`, its `trust:` and `ledger_root`.
    2. **In the background**, while the sign-in waits: what CI repeats (C-16); GitHub's id for
       the repository; through `gh`, the rules on the default and stage branches, and whether
       the person is a repository admin.
    3. **Sign in** (C-13), which registers this machine (ADR 0028 item 9).
    4. **Ask for the ledger first**, without creating it: `POST /v0/ledgers` with `dry_run`, so
       the plan can show its URL and whether it would be created. Then `GET {ledger}` for its
       log key.
    5. **Show one plan**, and ask once: package.json's wraps, backend and devDependency; each
       job's `id-token: write`; `hushrun.yaml` with its `trust:` section (item 3), or the
       migration (item 13); for a private repository, the line about Sigstore's public log
       that the answer consents to; the ruleset (item 5), when the person is an admin; in the
       org, connecting the repository and its ledger, with `default_branch`.
    6. **Apply** the org's changes first: connect, then the ledger. Then the files, package.json
       first, and `hushrun.yaml` only with a ledger. Then the ruleset. Then install the package
       (ADR 0024 item 6).
    7. **Show a skip**, as now.
    8. **Next step:** `Commit, open a pull request and merge it: CI records results, and the
       ledger is secured once this commit is on <default branch>.`

    **What is left to people, and why:**
    - **Commit, review and merge.** On purpose: the reviewed merge is what makes trust count.
      init never pushes to the default branch.
    - **Protecting the branches**, when the person running init isn't a repository admin.
    - **Making the ledger**, when they aren't an org admin (ADR 0025 item 3): a member's init
      writes no `hushrun.yaml`, and an admin's init finishes it.
    - **The repository id**, for a private repository with no App and no `gh` sign-in: init
      writes no identity, says so, and running init again after CI's first run, which pins the
      id at the backend, fills it in.
    - **Deploy commands**, for promotion: `--pipeline` and `adopt` write a placeholder that fails
      (ADR 0016 item 4).
    - **Approvers, laptop and agent principals, witnesses and `gc`**: decisions about people and
      other parties that init can't make.
    - **Consent to public names**, for a private repository on public-good Sigstore, until RFC
      0002's D1 gives a private default.

## Consequences

**Gained:**
- init leaves a ledger that works once a pull request merges: no keys, signatures, uploads,
  pins, CI variables or expiry renewals.
- One trust model. The server holds no trust, so it can't put back an older signed envelope, the
  replay ADR 0012's consequences accepted until expiry. Revoking is a merge, in force at the next
  command.
- Trust is reviewed where the code is, in the same tools, and CODEOWNERS can name
  `hushrun.yaml`.

**Lost, stated plainly:**
- **The threshold of named people.** Required reviews (one approval, or two, with code owners)
  replace two of three root keys. Repository admins and org owners can bypass branch protection
  unless a ruleset has no bypass actors; the root's signers were independent of them.
- **Rotating without a commit.** Keyless identities need no rotation; a key principal's rotation
  is a reviewed commit.
- **Anyone who can push to the default branch unreviewed can change trust.** They could already
  change the deploy workflow, and sign as production through it.
- **Key principals on hosts without usable ID tokens** now depend on the host's branch
  protection, not on offline admin keys. Their private keys sat in CI secrets that the same
  pushers could read anyway.
- **The log key is trusted on first use**, at init, then pinned by review.
- **Availability.** Every command that reads trust fetches one ref: a host that is down stops
  plans (exit 6). CI on that host is down too.
- **A trust change lands on the default branch first.** A stage added on `develop` gets its
  principal when main does. Gitflow repositories feel this.

**Contracts:** T-14, the `trust:` section, `trigger` in identities, `gc`, `trustDigest` and
`trustRevision`, `HUSHRUN_TRUST_REF`, `dry_run` and `default_branch` on `POST /v0/ledgers`.
Readers before T-14 refuse `trust:` without `--trust-from-tree`, so readers upgrade before
repositories migrate.

## Decisions

The maintainers resolved the eight questions this ADR left open, on 2026-10-20:

1. **A `trust:` section in `hushrun.yaml`**, not a file of its own. CODEOWNERS that name
   `hushrun.yaml` gate the team's checks too; a separate file stays an additive change if a
   team asks to split them.
2. **The ruleset is in init's plan, applied by default** with the rest, for a repository admin
   (item 5), with no required approval where they are the only writer.
3. **Every repository with a ledger gets `hushrun.yaml`**, checks-only ones included (item 4).
4. **Organization policy reaches repositories as one pull request each** (item 18), not by
   delegation: trust is never served by the ledger again.
5. **No witnesses by default.** H1 finds a short listing after the fact; a team adds witnesses
   with a commit.
6. **Private repositories before M24: consent in init's plan to public Sigstore** (item 3).
   Access to a private repository is narrow; the concern is only that its names appear in
   Sigstore's public log.
7. **One release of migration**, with the fallback to signed trust (item 13).
8. **One required approval** in the ruleset (item 5).

## Amendment 1: `org apply` keeps what a team adds (2026-10-21)

Item 18 had `org apply` write what `org.yaml` compiles as the whole section. A section holds more
than that once a team uses it: the principal of a laptop that `hushrun trust add --this-machine`
lists (ADR 0028 item 6), an accept entry for it, `gc`, the `ledger_log` init wrote when
`org.yaml` names none. Each of them showed up as a removal in every organization pull request,
so a team had to reject the organization's changes to keep its own. The maintainers decided that
team-added entries are kept and merged, and that the organization's constraints still win.

1. **Whose an entry is.** The section's `org` block (O-6) names the `org.yaml` that compiled it,
   by revision and digest. `org apply` compiles that version again from the policy repository's
   history: what it compiled for the repository is the organization's, and what else the section
   holds is the team's. Entries are principals, accept entries (`accept.<stage>.<key>`), each
   stage's `locations` and `min_custody`, each constraint and protected path, and `audit`,
   `ledger_log` and `gc`. A section without the block came from no `org.yaml`, so it is all the
   team's.
2. **The merge.** `org.yaml`'s entries are written as it compiles them now, so a team's change to
   one goes, and an entry it compiled before and doesn't now goes. The team's entries stay with
   their lines and comments, and a team's principal in one of `org.yaml`'s accept entries stays
   in it.
3. **The organization's constraints win.** A team's principal goes from accept entries at a stage
   whose `min_custody` its custody is below (T-3; approval checks aside), from accept entries
   `org.yaml` enforces (O-3), and from `events@` entries, which take lifecycle events from the
   stage's pipeline only (O-5). A key or identity of one of `org.yaml`'s principals, or its log or
   witness keys, goes from a team's principal (T-1, T-9, T-13), and a team constraint that
   contradicts one of `org.yaml`'s goes (T-7). The pull request's body and the command's output
   say what stays and what goes, and why.
4. **When the previous version can't be read** (a shallow clone of the policy repository, a block
   edited by hand), an entry `org.yaml` doesn't compile now could be one it dropped or one the team
   added. `org apply` refuses that repository, naming the entries, rather than keep a principal
   the organization removed.
5. **`doctor --org` and `org plan`** report the same merge: whether the default branch's section
   is current, and the team's entries kept and removed. `drift.rs` compares no organization trust
   (the App has no `org.yaml`), and the App's own trust pull requests change only what a person
   asked for (a key, or the section the trust editor prepared), so neither changes.

Alternatives considered: marking `org.yaml`'s lines with a comment, which every line of the
section would carry and a team's copy would spread; a list of the organization's entries in the
`org` block, which readers from before it would refuse (C-1); and taking what `org.yaml` compiles
now as the organization's, which would keep, as the team's, a principal the organization had
just removed.


## Amendment 2: no fallback to signed trust, and no migration (2026-10-22)

Items 12 and 13 kept, for one release, what read the root and signed trust: readers whose
default branch had no `trust:` section fell back to the ledger's signed trust, verified against
`hushrun.yaml`'s `ledger_root` pin; init moved that trust into `hushrun.yaml`; `ledger gc` and
the monitor kept the root's keys; and organizations kept `org sign` and `org apply --signed`
(decision 7). Nothing was ever released with a root or signed trust, so no repository has any
to migrate, and the release that keeps them protects no one while costing a second trust model
in every reader, the server and the dashboard. The maintainers decided to remove them now, before a
release, as ADR 0030 did for the old names.

- **Readers** take trust only from the default branch's section (T-14). Without one, or without
  a default branch to read, a command that reads a ledger exits 2, naming the commit and saying
  to merge init's commit; `--trust` with a ledger exits 2 too, since no file replaces the
  section, and names a file only (no URL into a ledger).
- **Gone from the CLI:** hushrun.yaml's `ledger_root` (an unknown key), `adopt --ledger-root`,
  the root and signed-trust reading and its warnings, init's migration, `doctor`'s `trust
  migration`, `signed trust` and `trust root` checks, `repository gc`'s root signer and the
  monitor's root attribution and `root_rewritten` finding, the hidden `trust root`, `trust sign`,
  `repository put-root` and `put-trust` stubs (now unknown subcommands), `org sign`,
  `org apply --signed` and `--roots`, org.yaml's `root` and `signed_trust`, `doctor --org`'s root
  and signed-trust checks, and the `root.v1`, `signed-trust.v1`, `org-sign.v1` and
  `org-apply.v1` schemas.
- **Configuration objects go with them.** The server's `GET`/`PUT {base}/config/{name}` and the
  client's `get_config`/`put_config`/`create_config` served only root versions, signed trust,
  `trust.yaml` and inventories; inventories are read beside the default branch's
  `hushrun.yaml`. `config` leaves stay in the log format, for logs from before.
- **Gone from hushrun cloud:** the index's fallback to signed trust (trust is the default
  branch's copy: the App's read, or the one keyless default-branch events name), the API's
  root, pin, serial and signed-trust fields, the Trust page's signed-trust sections, and the
  tables that held signed trust. The transparency
  log, its checkpoints and witnesses are separate from the root and stay.
- Item 12's "the next release removes …" and item 13 no longer hold, and neither does decision 7;
  `gc` principals (item 9) are the only signers of GC statements.

## Alternatives considered

- **Keep the root, and have init make it** with the signed-in person's browser or passkey key,
  as RFC 0002 §3.6's "Sign" does. A threshold of one person's key, kept in one browser, still
  protecting only the list; a lost key strands the ledger. A root the server makes would be the
  server vouching for itself.
- **Keep the root as an opt-in** for hosts without ID tokens. Two trust models and two paths in
  every rule, for a case repository trust with keys already covers (item 12).
- **Trust from the revision under evaluation,** as T-5 reads policy. A pushed branch or a pull
  request would add its own signer.
- **Trust through the host's API** instead of `git fetch`. A token per provider, where the
  checkout's credentials already reach the repository.
- **Trust published to the ledger by a default-branch run,** and read from there by every reader.
  The ledger would serve trust again, and withholding a newer copy would undo a revocation. It
  stays the index's display copy only (item 10).
- **Checking environments now, through the ledger server's writer check.** The server isn't
  trusted, so it could only add defense in depth; readers wait for ID-token signatures (item 7).
- **Requiring witnesses on every new ledger.** init can't make independent witnesses appear, and
  a ledger whose witnesses are its operator's proves less than it seems to.
