# 0002. Ledger: signed events in an append-only object store

- **Status:** accepted; its bucket and directory backends removed by
  [0022](0022-https-ledgers-only-and-no-kms.md): a ledger is served over HTTP
- **Date:** 2026-10-14
- **Rules:** E-1 … E-9 (`docs/spec/05-events.md`), L-1 … L-9 (`docs/spec/06-ledger.md`)

## Context

ci-promote reads its record from GitHub: workflow runs, job conclusions, commit statuses and
uploaded artifacts. That ties it to one CI provider, artifacts expire after 30 days, the scan
covers only the last 30 runs, and nothing in the record is signed: anyone who can upload an
artifact in the staging workflow can describe any image.

hushrun needs a record that works with any CI, keeps evidence as long as it is useful, and
carries its own proof of who produced it.

## Decision

- Every fact is a **signed event**: a DSSE envelope around an in-toto Statement whose subject is
  the scope's source digest (E-1 … E-5). Events are Ed25519-signed in v1; keyless Sigstore
  bundles come in M10.
- Events live in a **ledger**: a bucket prefix or a directory that holds one repository
  (L-1). The layout is keyed by scope and digest, so one LIST returns everything the planner
  needs for a digest, plus a deployed index per (scope, stage) whose keys sort newest first
  (L-2).
- **Storage carries no trust.** Every event is verified when read, against a trust policy that
  never comes from the content being evaluated (E-8, T-4). The storage key is checked against
  the signed payload, never trusted on its own.
- **Append-only, with no locks.** Keys are unique by construction (a ULID per event), objects
  are created once and never modified, and the stores we support give strong read-after-write
  and list-after-write consistency (L-3, L-7).
- **CI can only create and read** under `v1/`. Deleting a `failed` event could unblock a digest,
  so deletion and `trust.yaml` are admin-only (L-6).
- **Fail closed.** A plan that can't read the ledger exits 6 without a decision (L-5).

## Consequences

- Any CI provider can produce and consume evidence; the provider is only a hint for liveness
  and pending runs.
- Verification cost grows with the number of events per digest. Plans read one LIST and a
  bounded number of parallel GETs; GC removes old events (L-9).
- The ledger layout, the event format and the key formats are public contracts, versioned
  under `v1/`.
- Backends share one contract and one conformance suite: file and mem in M2, GCS, S3 and Azure
  Blob in M4, and optionally OCI in M12.

## Alternatives considered

- **GitHub runs, statuses and artifacts** (ci-promote today). Provider-specific, unsigned, and
  artifacts expire.
- **Git notes** (the "Local CI Attestation Format v0.1" draft). Writing needs push access to
  the repository, notes are easy to rewrite, and fetching them in every pipeline is slow.
- **A hosted verification API.** A non-goal for v1 (plan §2.2); the `Ledger` trait leaves room
  for one.
- **A database.** Needs an operator and credentials management beyond what CI already has for
  object storage.
