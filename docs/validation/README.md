# Validation

How hushrun's behavior is checked: the parts the repository enforces mechanically.

## Traceability (`cargo xtask trace`)

Every normative rule in `docs/spec/` has an ID, such as `D-3` or `P-11`. A test covers a rule by
naming it after a `covers:` marker:

```rust
// covers: D-4, D-5
#[test]
fn manifest_bytes_match_the_spec() { … }
```

```yaml
# testdata/scenarios/rules/p06-failed-after-passed.yaml
covers: [P-6, P-13]
```

`cargo xtask trace` lists every rule with the tests that cover it, and fails when:

- a rule of a started milestone has no covering test;
- a test names a rule that doesn't exist.

A rule is a bold run in a spec file that starts with its ID. A removed rule keeps its number and
is named in plain text, as P-2, P-3, P-7, P-11 and P-12 are since ADR 0062: a test that still
names it fails the trace as an unknown rule, so its `covers:` goes with the behavior it tested.

A rule belongs to the milestone in its `[Mn]` tag, or else to the milestone of its family
(`xtask/src/trace.rs`, `base_milestone`). [`milestone`](milestone) names the current milestone;
bump it when a milestone starts.

## Layers

| Layer | Where | Runs |
|---|---|---|
| Unit tests | `crates/*/src` (`#[cfg(test)]`) | every PR, on Linux, macOS and Windows |
| Golden vectors | `testdata/digest/`, `tools/refdigest.py` | every PR, against every adapter and the Python reference |
| Planner scenarios | `testdata/scenarios/` ([format and catalog](scenarios.md)) | every PR |
| Property tests | `crates/hushrun-testkit/tests/properties.rs` | every PR (512 cases), nightly (10,000 cases) |
| CLI tests | `crates/hushrun/tests/` | every PR |
| Golden init fixtures | `testdata/init/`, `crates/hushrun/tests/init_fixtures.rs` | every PR: init's JSON plan for each pattern (`HUSHRUN_UPDATE_GOLDEN=1` rewrites them), and A9, its prediction run against the run's decision |
| Corpus | `cargo xtask corpus`, `tools/corpus/` ([README](../../tools/corpus/README.md)) | nightly, and before a CI-reach phase's pull request: init on 100 real repositories, its assertions A1 to A10, and the delta against the baseline; it needs the network once |
| Integration tests | behind the `it` cargo feature, such as `crates/hushrun-artifact/tests/registries.rs` | their own CI jobs (registries, public-good Sigstore, Docker); they may use the network |
| Fuzzing | `fuzz/` | every PR (60 s per target), nightly (15 min) |
| Mutation testing | `cargo mutants` on the planner | nightly |

## Mutation testing

The planner must kill at least 90% of its mutants (M3), and every survivor is reviewed. Today
every viable mutant is killed. A new survivor gets a test that kills it, or, when no input can
tell it apart from the original, the code is simplified until the mutant doesn't exist.

```sh
PROPTEST_CASES=256 cargo mutants -p hushrun-core -f 'crates/hushrun-core/src/plan/*.rs' \
  --test-package hushrun-core,hushrun-testkit --timeout 90
```

`--test-package` matters: most of the planner's tests are the scenarios and properties in
`hushrun-testkit`.

## HTTP ledgers

The ledger contract runs on `HttpLedger` in every test run, against the testkit's fake HTTP
ledger on the loopback (`hushrun_testkit::fake_ledger`), which implements L-10: roles, key
shapes, pages, batch-gets, and injected faults (statuses with `Retry-After`, dropped
connections, delays, redirects). Every CLI test's `World` has the same fake as its ledger. The
fake answers the repository's signers (L-13), signed with its log key: the world's members'
machines, which tests register, revoke or forget, and the CI identity a test sets (T-15).

[M18] The fake keeps a transparency log as the hosted ledger does (L-11): receipts, checkpoints
signed with a fixed log key, signed listings and the `log/` endpoints. It can also lie: leave
keys out of listings while signing for them (`hide_from_listings`), show a token another
history (`fork_for`), or send receipts that prove nothing. `World::new` writes its log key in
`hushrun.yaml` (T-9); `World::without_log_key` is a repository that names none yet.
`crates/hushrun/tests/transparency.rs` runs H1's two guarantees end to end: a ledger that hides a
failed attempt from the listing a plan reads sways that plan, and `hushrun repository monitor` then
reports it with the ledger's signed statement; a reader shown another history exits 6 naming both
checkpoints. `hushrun-tlog`'s vectors come from `tools/reftlog.py`, an independent
implementation of RFC 6962 and C2SP signed notes in Python's standard library
(`python3 tools/reftlog.py selftest`, then `vectors > testdata/tlog/vectors.json`).

## Keyless signing and timestamps (M10)

Every test run signs and verifies without the network:

- `hushrun_attest::testing` mints a throwaway Sigstore: a certificate authority that issues
  Fulcio-style certificates with SCTs from a CT log, a timestamp authority, the
  `trusted_root.json` that trusts them, and a TUF repository that serves it.
- The testkit serves that on the loopback: GitHub's ID token endpoint, Fulcio and the timestamp
  authority (`hushrun_testkit::sigstore`).
- `crates/hushrun/tests/keyless.rs` runs the pipeline end to end with them: keyless signing,
  certificates of another repository, a timestamp authority that is down (exit 9), offline
  verification from a refreshed cache with the services gone, P-256 key files, and revoked
  machines counted by trusted time (T-15).

Real Sigstore checks interoperability:

- The vendored public-good trust root is verified against its TUF metadata.
- A certificate public-good Fulcio issued (`testdata/sigstore/`) is verified, SCT included.
- `cargo test -p hushrun-attest --features it --test sigstore` asks Sigstore's timestamp
  authority for a timestamp, refreshes the trust root from its TUF repository, and, in a GitHub
  Actions job with `id-token: write`, gets a certificate from Fulcio.

## Local and agent evidence (M8)

- **Planner.** Scenarios decide with agents' and laptops' evidence (`rules/p01-agent-evidence-counts`,
  `rules/p05-a-members-attempt-counts`) and with the repository CI-only (T-16, the scenarios'
  `ci_only: true`, as the signers answer says), where only `ci`'s checks, artifacts, lifecycle
  events and deployments count (`rules/t16-*`, `rules/p10-deployed-by-a-laptop-doesnt-count`). A
  property test checks that, on random worlds, a CI-only repository decides every check, artifact
  and deployment on `ci`'s events only. `keyless.rs` checks the answer end to end: a non-CI event
  timestamped before the rule began counts, an untimestamped one doesn't, and a saved answer
  carries the rule to `verify --signers`; and [ADR 0031 amendment 2] after the rule was lifted
  and set again, one timestamped in the earlier period doesn't count, and one outside it does.
- **Isolated runs.** `hushrun_testkit::container::FakeRuntime` is a Docker-compatible CLI that
  records its calls and runs each check on the host, in the export, with only the variables
  hushrun passed. `crates/hushrun/tests/isolated.rs` runs E-12 end to end with it: the export,
  networks, variables and secrets, custody and environment, the runtime's failures, timeouts and
  retries, an agent's evidence making CI skip a check, and none of it counting while the server
  names the repository CI-only. It also covers a
  check's own exit 125 (recorded, told from the runtime's by `container inspect`), SIGTERM,
  SIGINT and SIGHUP during a check (the container and export removed, nothing recorded, hushrun
  ended by the signal), exports a killed run left, and a signing key file inside the export.
  Unit tests check every
  exported path segment on POSIX and on Windows rules. With a real runtime,
  `cargo test -p hushrun --features it --test isolated_docker` checks that an isolated check has
  no route out by default and one with `network: open`, sees exactly the digested files and
  modes, and only its variables, that a check's exit 125 and a missing shell are recorded as
  failures, and that no container is left; the `isolated` CI job runs it with Docker.
- **Pull requests.** `crates/hushrun/tests/pull_requests.rs` covers `--config-ref`, T-5 on
  GitHub and GitLab, and `--worktree` on every command that digests. Approvals went with stages
  (ADR 0062): `history/an-approval-is-inert` shows a 0.8 approval event counting for nothing.

## Onboarding

`crates/hushrun/tests/adopt.rs` adopts sample repositories; what it writes for one is golden
under `testdata/adopt/`, which the `actionlint` CI job lints with the recipes and the reusable
workflow. `crates/hushrun/tests/workflows.rs` pins the reusable workflow's inputs, its jobs and
their order, and that every action it runs is pinned by commit.

## Registries

`cargo test -p hushrun-artifact --features it --test registries` and
`cargo test -p hushrun --features it --test artifact_registries` copy images between the two
registry hosts named in `HUSHRUN_IT_REGISTRIES`, over plain HTTP. The `registries` CI job
runs CNCF Distribution and zot. The ignored test `seed_a_large_layer` pushes a 2 GB layer for
the nightly memory check of `artifact copy`.

## GitLab CI (M9)

`crates/hushrun/tests/gitlab.rs` runs hushrun as GitLab jobs against
`hushrun_testkit::fake_gitlab`, a fake of GitLab's REST API (projects, pipelines by id and by ref,
protected branches, token checks, injected faults): run identity and dotenv reports, retried
plan jobs, liveness (P-6), API failures, `doctor`'s GitLab checks, and keyless signing with GitLab's ID tokens through the
testkit's Sigstore, whose fake Fulcio maps GitLab's claims as Fulcio's `gitlab-pipeline` template
does. The same file pins the recipe's structure. `xtask/src/installer.rs` runs the real install
script against a fake release on the loopback, on Linux and macOS.

## End to end on GitHub

`cargo xtask e2e --repo OWNER/NAME` drives a sandbox repository set up as
[the GitHub guide](../guides/github.md) describes: it pushes commits, waits for the staging and
production workflows, and checks each plan's decision in the run log. `--dry-run` prints the
steps. The recipes are linted by the `actionlint` CI job. [M8] With `--agent-key FILE` it also
runs E10: an agent validates a change with `hushrun run --all --stage staging --isolated
--worktree` before pushing it, and staging's plan must run none of its checks; the key is an
agent's machine key, made with its token by `hushrun token create --agent NAME`, as
[the agent guide](../guides/agents.md) sets up.

[G12] `--scenario merge-queue` runs GitHub's merge queue end to end, and only when named: a pull
request passes, the queue's run of the same tree skips, and so does the push to `main` that
merging makes. The sandbox must be an organization's repository whose `main` requires a merge
queue in a ruleset, with a `ci.yml` that runs `hushrun run unit -- ./test.sh` on `pull_request`,
`merge_group` and pushes to `main` ([the GitHub guide's merge queues](../guides/github.md#merge-queues)).

## End to end on GitLab

`cargo xtask e2e --gitlab GROUP/PROJECT` runs the same scenarios on a GitLab.com project set up as
[the GitLab guide's sandbox section](../guides/gitlab.md#a-sandbox-project) describes, with
`glab`: it waits for each push pipeline at the pushed commit and reads the plan job's trace.

