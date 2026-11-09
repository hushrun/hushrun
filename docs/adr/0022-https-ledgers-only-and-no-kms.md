# 0022. HTTPS ledgers only, and no KMS signers

- **Status:** accepted
- **Date:** 2026-10-18
- **Rules:** L-1, L-3, L-4, L-6, L-7, L-9, L-10, L-11, T-4, T-8, T-12, C-3, C-11, E-2, O-7,
  §11.3

## Context

hushrun reads and writes four kinds of ledger: buckets (`gs://`, `s3://`, `az://`, M4, ADR
0007), directories (`file://`), in-memory ones for tests (`mem://`), and, since M18 (ADR 0011),
ledgers served over HTTP, such as the hosted one. It signs with four kinds of signer: key files,
Sigstore's keyless certificates (ADR 0013), and keys in GCP Cloud KMS, AWS KMS or Azure Key
Vault (ADR 0013, item 10). `oci://` was reserved for M12 and never built.

RFC 0002 puts checks first: setup is three steps in the dashboard, with no cloud project, IAM or
keys. Each kind of ledger and signer beyond that path costs code to secure, test and explain,
and several make the record weaker than the hosted ledger:

- **Buckets** need a cloud project, IAM conditions per repository, a retention lock for the root
  and a credential step in every job. Azure can't say "create but don't overwrite", the
  listings a plan reads are unproven, and only the bucket's owner can monitor it. The
  dashboard's external ledgers read buckets with the server's own credentials, an open
  risk.
- **Directories** keep a second trust model alive: a ledger without a root reads plain
  `trust.yaml` (T-8), and deploys there need no `plan --start` (L-11). Every rule has two cases,
  and the tests mostly exercise the weaker one.
- **KMS keys** need a cloud project, a key per stage, IAM conditions binding each key to a branch,
  and three credential chains in the binary, two of them borrowed from `object_store`. RFC 0002
  (D1) makes keyless signing, and ID tokens as signatures once M24 tests them, the defaults;
  KMS was left for an Advanced area that the checks-first setup never reaches.

## Decision

1. **Ledger URLs are `https://`** (L-1), and `http://` on a loopback host for local development,
   such as a server run locally at `http://localhost:28787`. Every other scheme (`gs://`, `s3://`,
   `az://`, `file://`, `mem://`, `oci://`) fails to parse with one message: "only https://
   ledgers are supported (the hosted ledger)". That holds wherever a ledger URL is read:
   `hushrun.yaml`'s `ledger`, `HUSHRUN_LEDGER`, `HUSHRUN_LEDGER_BASE`, `--ledger`, `org.yaml`'s
   `ledger_base` and `ledger`, `HUSHRUN_BACKEND` and `hushrun login`.
2. **One backend.** The object-store backend goes: `ObjectLedger`, its storage credentials,
   the conformance runs on emulators and the nightly GCS run (ADR 0007, items 2, 3, 5, 9 and
   11), and `hushrun-ledger`'s `it` feature. Its `cloud` feature now means the HTTP backend.
   `MemLedger` is test code. `FileLedger` stays for one job, `hushrun run`'s results on this
   machine, in hushrun's cache directory (§7.6), which no URL or variable names; a backend
   that machines share is an `https://` server (C-11).
3. **Every ledger has a root, and `hushrun.yaml` pins it** (T-8, C-3). Readers take only signed
   trust, `trust.dsse.json`; `hushrun ledger put-trust` refuses a plain `trust.yaml`. Plain
   trust is still read where no ledger is involved: `--trust FILE` and `--trust-from-tree`, for
   `policy validate` and `policy explain`.
4. **Every ledger keeps a transparency log**, so L-11's rules apply to all of them: `mark
   deployed` follows the `started` of `plan --start`, and GC signs each deletion with a root key
   (L-9). The implicit `started` (§10.4) remains for terminal events without a deploy.
5. **`HUSHRUN_LEDGER_TOKEN_COMMAND` is for `https://` ledgers only** (L-10); it no longer
   prints storage tokens (ADR 0007, item 6).
6. **No KMS signers** (T-12, E-2). `gcpkms://`, `awskms://` and `azurekms://` keys,
   `HUSHRUN_SIGNING_KMS`, `HUSHRUN_SIGNING=kms`, `hushrun keys public --kms`, and the reusable
   workflow's `signing-kms` input go, with the cloud credential chains (ADR 0007, items 7 and 8)
   and `object_store`, which leaves the workspace. Events, roots and trust are signed with a key
   file, Ed25519 or ECDSA P-256, or keylessly through Sigstore. P-256 stays: hardware keys,
   browser keys and Fulcio's certificates use it.
7. **The Terraform modules go** (ADR 0016, item 8; M14): they made buckets and their KMS keys.
   An HTTP ledger needs no cloud resources. The reusable workflow loses its
   `azure-storage-account` input, which only `az://` ledgers read.
8. **Access control is the ledger's** (L-6): an HTTP ledger enforces L-10's roles. The per-store
   IAM rules leave the spec, and L-7 is the server's to keep.
9. **The control plane loses external ledgers**: every ledger it
   shows is hosted, and the "Your own bucket" path leaves onboarding.

## Consequences

- There is no ledger without a server. Local experiments use a hosted ledger, or a server on
  the loopback; the CLI's tests use the testkit's fake HTTP ledger, which now backs every
  `World`. `hushrun run NAME -- COMMAND` still needs nothing: its results stay on the machine
  until it signs in to a backend.
- A team on a bucket or a directory moves to an HTTP ledger: it creates one, signs the root and
  the trust, pins `ledger_root`, and sets `HUSHRUN_LEDGER`. Its old events aren't copied, so the
  first plans on the new ledger run `full`.
- A team signing with KMS keys moves to keyless signing, or to key files in its CI's
  environment secrets. Private repositories wait for RFC 0002's D1 (M24) for a default that
  publishes nothing.
- The binary carries no storage or KMS clients, and CI loses the emulator and Terraform jobs.
- Being append-only, consistent and monitored no longer depends on how each customer wrote IAM.
- Bringing any of them back means a second trust model again, or another credential chain,
  and for buckets, a log a bucket can't keep by itself.

## Alternatives considered

- **Keep `file://` for local use.** It keeps the plain-trust path and the deploys without
  `plan --start` alive, for experiments that a ledger on the loopback serves as well.
  `hushrun run` keeps its local results without it.
- **Keep buckets and KMS in an Advanced area** (RFC 0002 §3.7, as first drafted). That keeps
  every cost above for paths the checks-first setup never uses, and each bucket stays weaker
  than the hosted ledger.
- **Keep only GCS, or only GCP KMS.** Less code, but the same IAM setup and credential chains.
- **Drop P-256 with KMS.** Keyless certificates and browser keys sign with it.
