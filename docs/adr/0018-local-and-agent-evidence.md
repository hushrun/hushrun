# 0018. Local and agent evidence: isolated runs, protected paths, audits, approvals

- **Status:** accepted
- **Superseded in part by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-16
- **Rules:** E-12 (new), E-4, E-7, C-5, T-5, P-16, P-17, P-1, P-3, T-2, T-10, D-10

## Context

M8 delivers the original idea (plan §16): validate locally, and have CI trust it. Agents push
far more often than people, and each push makes CI test content an agent already tested. The
plan gives the pieces: `run --isolated` (plan §11.6), local custody end to end, protected paths
(P-16), audit sampling (P-17), `approve`, `--config-ref` for pull requests (T-5), and the agent
guide (§15.5). RFC 0001 (accepted) moved M10 before M8, so that approvals are signed by people's
own identities from the start rather than by personal keys first.

The specs left some of it open. §11.6 says isolated runs pass "only the variables named in
`env`" and "never secrets", but `env` lives in `hushrun.yaml`, which the content under
evaluation can change. `restricted` networks have no definition. P-16 doesn't say which entries
its patterns see, or whether approvals count while the protected paths differ. P-17's `u` hashes
"`digest ‖ check ‖ run id`" without an encoding. T-5 says pull-request pipelines MUST use
`--config-ref` without saying who enforces it. And a person can sign keylessly only with an ID
token, which nothing on their laptop gave them.

Where the plan is ambiguous, it asks for the behavior that runs more checks or deploys less
(§0.1). Each decision below does.

## Decision

### Isolated runs (E-12)

1. **The export is the digest, nothing else.** `run --isolated` writes the entries of the
   scope's digest to a new directory through the VCS's own readers (the git CLI's one
   `cat-file --batch` per command, or gix), never `git archive`, which applies attributes.
   Modes are 0644 and 0755 (D-3), links keep their targets, a gitlink becomes an empty
   directory. It checks two things before any check runs:
   - the entries digest to the subject's digest: with `--digest` or `--plan`, the checkout
     must hold that content, or the run fails (exit 2) rather than record evidence about other
     bytes;
   - the directory lists exactly those paths afterwards. On a case-insensitive filesystem,
     macOS's by default, `README` and `readme` are one file; the run fails naming them, instead
     of a check passing against a tree that never existed;
   - every path segment is one file name on the host. On Windows that also rules out what
     Windows reads as something else: `\`, a drive or stream (`:`), a device such as `NUL` or
     `COM1` with any extension, and a final dot or space, which it strips. A Git tree can hold
     any of them, and `C:` alone would replace the export's path.

   Each retry starts from a new export, because the last attempt may have changed it. Exports
   go under `<cache dir>/exports` (`HUSHRUN_EXPORT_DIR` overrides), which Docker Desktop, Colima
   and Podman machines share with their VMs, unlike macOS's `/var/folders`. An export a killed
   run left is removed by the next isolated run once it is a day old (its name's ULID dates it).

2. **Docker or Podman, through their CLI.** `HUSHRUN_CONTAINER_RUNTIME` names one, else
   `docker`, else `podman`, found on `PATH`. The run is
   `run --name … --pull never --init --label com.hushrun.check=<check> --network …
   [--platform …] --security-opt no-new-privileges --volume <export>:/src[:z] --workdir /src
   --env NAME… --entrypoint <shell> <image> -c <run>`, and the container is removed afterwards.
   - The entrypoint is replaced, as GitHub's `container:` jobs replace it, so images that set
     one still run the check's command. `--init` runs the check under an init process, as CI's
     runners don't make it PID 1: signals reach it, and what it leaves is reaped.
   - A missing image is pulled first, for the pinned platform. A failed pull is the runtime's
     failure: nothing is recorded, and hushrun exits 1. Evidence is about content; a daemon
     that is down says nothing about content.
   - The CLI's exit code 125 means the runtime failed, or the check itself exited 125. Not
     `--rm`, so the container's state (`container inspect`: started, exit code, error) can tell
     them apart: a check that started and exited 125 fails, recorded; otherwise nothing is
     recorded. Trusting the CLI alone would let a failing check's 125 erase its failure, and an
     audit's (P-17).
   - A stop signal (SIGINT, SIGTERM or SIGHUP) while a check runs removes the container and the
     export, records nothing, then ends hushrun as the signal asked. Outside a container the
     signals act as by default. Stopped containers a killed run left are removed by the next
     isolated run once they are a day old, by their label (`container prune`).
   - The container runs as the image's user, root for most, as in CI, so the results match
     CI's. A rootful runtime leaves what it wrote owned by root on the host, so the same image
     empties the export before hushrun deletes it.
   - `:z` relabels the mount for Podman on SELinux hosts; Docker Desktop doesn't need it.
   - A timeout removes the container by name: stopping the CLI wouldn't stop it.
   - The signal handling is signal-hook's flags (pure Rust over libc): idle, a signal runs its
     default action; while a check runs, it only sets a flag the wait loop polls.

   No library: the bollard crate would bring a second HTTP stack, and Podman's socket isn't
   always on. The CLI is what CI uses, and what an operator can swap.

3. **Networks.** `none` is the default (`--network none`). `open` is the runtime's default
   network. `restricted` has no portable meaning: hushrun doesn't run a proxy or write firewall
   rules. A check that declares it runs only on the network the operator names in
   `HUSHRUN_RESTRICTED_NETWORK` (a Docker or Podman network whose egress they restrict), and
   without one it doesn't run (exit 2), instead of quietly getting `open`.

4. **Secrets never go in, whatever `env` says.** `hushrun.yaml` is content under evaluation: a
   change could add `GITHUB_TOKEN` to a check's `env` and hand it to the code it tests. So a
   check whose `env` names one of hushrun's or CI's credentials, or a name that reads like a
   secret (`TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `PASSPHRASE`, `CREDENTIAL`, `PRIVATE`,
   `AUTH`, `ACCESS_KEY`, `APIKEY`, `DATABASE_URL` or `_KEY_` in it, or `_KEY`, `_PAT`, `_DSN` or
   `_PWD` at its end, and URLs that carry credentials such as `REDIS_URL`), doesn't run
   isolated: exit 2, before anything runs. It is a heuristic, not a guarantee: a check's `env`
   should name only what it needs. Dropping the variable instead would run a different check
   than the definition says. The others pass by name (`--env NAME`), so values reach the runtime
   through its environment and never a command line or a log. The signing key stays in
   hushrun's process; the container never sees it, its socket or the user's home. A key file
   (`HUSHRUN_SIGNING_KEY_FILE`) among the exported entries refuses the run (exit 2): with
   `--worktree`, an agent's key generated into the repository would otherwise reach the check,
   and the commit.

5. **Custody claims are honest (E-4).** `isolated` is what `run --isolated` claims by default.
   An operator whose ephemeral runner holds the key sets `HUSHRUN_CUSTODY=sandbox`, and CI stays
   `ci`. `HUSHRUN_CUSTODY=isolated` on anything else records `local`, with a warning: the
   principal's cap is the enforcement, but hushrun's own tools shouldn't make a claim they
   didn't earn. `HUSHRUN_INVOKED_BY=agent:<id>` fills `producer.invokedBy`, which is
   informational.

### Local custody and `max_attempts` (P-1)

6. The planner rules came with M3; M8 adds the end-to-end behavior. `hushrun run` names the
   signer's principal and, when it has `max_attempts`, says before running that its attempts
   at this check and digest are spent, and after recording that its evidence no longer counts.
   It still runs the check: the agent learns whether it passes. CLI tests cover an accepted
   agent making CI skip a check, a laptop's local evidence counting where accepted and not under
   `min_custody: isolated`, and an unknown key counting nowhere.

### Protected paths (P-16)

7. **The whole snapshot.** The patterns match every entry of the snapshot, not one scope's and
   not after `digest.exclude`: `.github/**` is usually outside every scope, and that is why it
   needs protecting. The digest is the manifest's (D-4, D-5), so `sha256sum` checks it.
8. **The plan carries it.** `hushrun.plan/v1` gains `protectedDigest`, and `deployed` in each
   decision the one the stage last recorded. `mark deployed` records the plan's, so a deploy
   records the protected paths the plan decided with, and computes it from the checkout only
   without a plan.
9. **Unknown is changed.** A deployment from before the trust protected any paths, a stage that
   never deployed, and a run whose protected digest is unknown (a reader without the content,
   such as the index) all count only `ci` evidence. The first CI deploy then records the digest.
10. **Approvals still count.** P-16 ignores check evidence below `ci`. Approvals come from
    people (T-2) and never have custody `ci`; ignoring them would block every stage that
    requires one the moment a workflow changed, with no way out: forcing never waives an
    approval (P-9), and only a deploy would record the new digest. A person approving the change
    is exactly what a protected path asks for.
11. **Reasons say so.** When evidence below `ci` would satisfy a requirement but for P-16, the
    plan's reason ends with why: the paths changed since the stage last deployed, it never
    deployed, its newest deployment recorded none, or this run's are unknown.

### Audit sampling (P-17)

12. **`u` is `SHA-256(D ‖ C ‖ 0x00 ‖ R)`**, the digest's 32 bytes, the check's name, a zero
    byte and the run's id in UTF-8; its first 8 bytes, big-endian, over 2⁶⁴. The zero byte
    keeps `unit1` in run `2` apart from `unit` in run `12`. A known answer pins it.
13. **"Relies only on evidence below `ci`"** means no candidate of `ci` custody satisfies the
    requirement on its own: a newer agent pass over an older, still valid CI pass isn't
    audited; an agent pass after a CI failure is.
14. **An audit is a `refresh`, not a `promote`**: the check is in `checks_to_run` with reason
    `audit` and `audited` (event, principal, custody), so the pipeline runs it before deploying.
    `status` shows no audits: they are drawn per run, and status is no run.
15. **A failed audit** is recorded like any failure, so the newest evidence is CI's and the
    next plan has the check `failed`; `hushrun run` prints an alert naming the principal and
    the event, annotates it as an error on GitHub, and exits 8. A test checks the rate over
    1,000 simulated plans at six rates, within four and a half standard deviations, and exactly
    at 0 and 1.

### Approvals (P-3)

16. **`hushrun approve <check> --stage T`** records an `approval` event (E-5) for an approval
    check `T` requires, about the checkout's digest or `--digest`. Before signing it prints what
    is approved, what `T` runs now, and `git log <deployed>..<approved>` to see the change.
    With `--digest` for content the checkout doesn't hold, the event names no revision rather
    than a wrong one.
17. **It records only what counts.** The signed event is verified as readers verify it (E-8),
    and its principal must be accepted for the approval at that stage (T-2), or nothing is
    written (exit 2), naming the principals that are. A pipeline never approves: `approve`
    refuses to run in CI.
18. **People sign as themselves.** Any signer works: a key file, a KMS key, or keyless. On a
    person's machine, keyless signing (`HUSHRUN_SIGNING=sigstore`) gets their ID token by
    signing them in with their browser: OAuth 2.0's authorization code flow with PKCE
    (RFC 7636) and a loopback redirect (RFC 8252), at public-good Sigstore's issuer
    (`https://oauth2.sigstore.dev/auth`, client `sigstore`) or `HUSHRUN_OIDC_ISSUER` and
    `HUSHRUN_OIDC_CLIENT_ID`, as Sigstore's other clients do. hushrun opens `$BROWSER` or the
    system's opener and prints the URL; the token's nonce must be this sign-in's. Fulcio
    certifies their email, which trust lists as a subject identity (T-10), and every keyless
    signature is timestamped (E-11).
19. **`--out FILE`** writes the signed event instead of appending it, for the dashboard to relay
    to a hosted ledger, create-only (RFC 0001 §4.6, CP-5). The file is the object as the ledger
    stores it.

### Pull requests (T-5)

20. **hushrun enforces it.** In a pull request's pipeline (GitHub's `pull_request` and
    `pull_request_target`, GitLab's merge request pipelines), every command that reads or
    writes the ledger exits 2 without `--config-ref`, or with a ref that isn't the branch the
    pull request merges into (`origin/main`, `main`): `--config-ref HEAD` is the change itself.
    A pull request's job runs the pull request's workflow, so this guards honest pipelines, not
    a hostile one; pull requests get a principal of their own, accepted for little (the agent
    guide says so). `policy validate` still reads the pull request's own file.
21. `--config-ref` reads the file from the object database (`git cat-file`), so the ref must be
    fetched: shallow checkouts fetch the base branch first, and the error says how.

### `--worktree` (D-10)

22. `plan`, `run`, `attest`, `mark`, `guard`, `status`, `approve` and, without a plan,
    `artifact resolve` take `--worktree`, which excludes `--rev` and `--digest`. The revision recorded is still `HEAD`'s, with the tree the
    worktree would commit (E-4 `source` is informational).

## Consequences

- An agent with a key of a principal accepted at staging, and a container runtime, validates
  before it pushes, and CI skips what it validated: the plan's `checks_to_run` shrinks, the
  stage refreshes or promotes. The agent guide and the `AGENTS.md` snippet say how to set it up.
- Isolated runs need the image pinned: a check without `image` can't run isolated, by design.
  Teams pin the image their CI jobs already run in (`container:` on GitHub), and the evidence
  then counts in CI too (E-7).
- Protected paths make agent evidence wait for one CI deploy after every change to them.
  Audits cost CI the audit rate's share of reruns. Both are the price of trusting evidence from
  outside CI, and both are off until trust sets them.
- `restricted` networks need an operator's network. hushrun never pretends to restrict.
- The dashboard relays approvals (RFC 0001 §4.6): the server prepares an unsigned statement,
  the approver's browser signs it with their own key, or uploads what `hushrun approve --out`
  wrote, and the server appends it, create-only; it signs nothing (CP-5).

## Alternatives considered

- **`git archive` for exports.** It applies `export-ignore` and `export-subst` attributes, so the
  container would see other bytes than the digest names.
- **Dropping secret-looking variables instead of refusing.** The check would run without a
  variable its definition names, and could pass where CI fails.
- **A built-in egress proxy for `restricted`.** A per-platform firewall or proxy that hushrun
  maintains, for one network mode. An operator's network is simpler and honest about what it
  restricts.
- **Ignoring approvals under P-16 too.** It deadlocks stages that require approvals (item 10).
- **Hashing the digest as text in `u`.** Either works; the raw bytes avoid two spellings, and
  the separator removes ambiguity at the boundary between name and run id.
- **`SIGSTORE_ID_TOKEN` only, for people.** No identity provider Fulcio trusts gives a person a
  token for `sigstore` from a command line; the browser flow is how Sigstore's clients do it.
- **Leaving T-5 to the recipes.** A recipe that forgets the flag would evaluate a change against
  its own rules, silently; the CLI knows when it runs in a pull request.
