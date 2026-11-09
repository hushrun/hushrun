# 5. Spec: events and attestations

> Normative. Copied from the implementation plan (2026-10-13) in M0; this file is now the source of truth.
> MUST, SHOULD and MAY follow RFC 2119. Rules tagged `[Mn]` belong to milestone n.

**E-1 Envelope.** Every event is a DSSE envelope (JSON) with these fields:

- `payloadType`: `application/vnd.in-toto+json`;
- `payload`: standard padded base64 of the in-toto Statement bytes;
- `signatures`: `[{"keyid", "sig"}]`.

The signature covers the PAE:

```
PAE(type, body) = "DSSEv1" SP LEN(type) SP type SP LEN(body) SP body
```

`LEN` is the byte length in ASCII decimal with no leading zeros, and `SP` is a single space.

[M10] Every event hushrun writes from M10 on is a Sigstore bundle (`mediaType` `application/vnd.dev.sigstore.bundle.v0.3+json`) that wraps the same DSSE envelope, whatever signs it, so that every signature can carry a timestamp (E-10):

- the envelope has exactly one signature;
- `verificationMaterial` carries either `publicKey.hint`, equal to the signature's `keyid` (a key, E-2), or `certificate.rawBytes`, the DER signing certificate (keyless, E-11), and then the signature's `keyid` is empty;
- `verificationMaterial.timestampVerificationData.rfc3161Timestamps` carries RFC 3161 timestamps of the signature (E-10), when an authority answered;
- `verificationMaterial.tlogEntries` is empty: hushrun writes no transparency log entries.

The object is compact JSON. Readers accept plain DSSE envelopes, which every event written before M10 is, and bundles of version 0.3, which may also be written `application/vnd.dev.sigstore.bundle+json;version=0.3`.

**E-2 Keys.** Keys are Ed25519 or, [M10], ECDSA P-256, the curve hardware keys, browsers and Sigstore's certificates use:

- Ed25519 public keys are written `ed25519:<standard base64 of the 32 raw bytes>`. Signatures are 64 bytes (RFC 8032) and verify strictly: no small-order keys, no malleable signatures.
- [M10] P-256 public keys are written `ecdsa-p256:<standard base64 of the DER SubjectPublicKeyInfo>`: `id-ecPublicKey` with the named curve `prime256v1` and the uncompressed point, 91 bytes. Any other encoding of the same key is refused, so a key has one text. Signatures are DER `ECDSA-Sig-Value`s over the SHA-256 of the message.
- `keyid` is `sha256:<hex SHA-256 of the bytes the text encodes>`: the 32 raw bytes, or the SubjectPublicKeyInfo.
- Private keys are PKCS#8 PEM (`-----BEGIN PRIVATE KEY-----`), Ed25519 or P-256.
- [ADR 0022] KMS keys (GCP Cloud KMS, AWS KMS, Azure Key Vault) are no longer signers: a key is a key file, or a keyless certificate (E-11).

**E-3 Statement.**

```json
{
  "_type": "https://in-toto.io/Statement/v1",
  "subject": [{"name": "<repository>#<scope>", "digest": {"sha256": "<source digest hex>"}}],
  "predicateType": "https://github.com/hushrun/hushrun/spec/event/v1",
  "predicate": { }
}
```

There is exactly one subject. Its digest is the source digest, which is the SHA-256 of the manifest (D-5), so the subject is the manifest document itself.

**E-4 Common predicate fields.** All are required unless marked optional.

| Field | Meaning |
|---|---|
| `kind` | One of `started`, `check`, `artifact`, `deployed`, `passed`, `failed`, `superseded`, `approval`. |
| `repository` | Canonical repository id, such as `github.com/acme/shop` (C-2). |
| `scope` | Scope name. |
| `stage` | Stage the event belongs to, if any. Optional on every kind: an event without one belongs to the repository's one pipeline, and is stored under `_` (L-2). On `check` events with `checkKind: source` it is informational. |
| `canonicalization` | `hushrun-source-v1`. |
| `source` | `{vcs, revision, ref?, tree?, objectFormat?}`. Informational; never used for matching. |
| `run` | `{provider, id, attempt, url?, trigger, job?, jobId?}` (§11.2 defines each provider's values). `provider` is `github`, `gitlab`, `buildkite`, `circleci`, `local` or `generic`. `trigger` is `push`, `upstream`, `manual` or `schedule`. [M9] On GitLab, `id` is `CI_PIPELINE_ID`, `url` `CI_PIPELINE_URL`, `job` `CI_JOB_NAME` and `jobId` `CI_JOB_ID`; GitLab counts no attempts, so `attempt` comes from the plan (§10.4). |
| `producer` | `{tool: "hushrun", version, custody, invokedBy?: {type: "human" or "agent", id}}`. [M8] `custody` is what the command claims: `HUSHRUN_CUSTODY` when set, else `ci` in CI (§11.2), else `isolated` for a check `hushrun run --isolated` ran (E-12) and `local` for anything else. Only an isolated run claims `isolated`: `HUSHRUN_CUSTODY=isolated` anywhere else records `local`, with a warning. `invokedBy` is informational, from `HUSHRUN_INVOKED_BY` (`agent:<id>` or `human:<id>`). |
| `time` | RFC 3339 UTC with milliseconds: when the event was signed. |
| `policyDigest` | `sha256:` of the `hushrun.yaml` bytes used. Informational. |
| `forced` | Optional; `true` on events from a `--force-full` run. |
| `checkpoint` | [M18] Optional. The newest checkpoint of an HTTP ledger's transparency log that the writer verified before signing, as the C2SP signed note the ledger sent (L-11). A reader that checks the log asks the ledger for a consistency proof to its own checkpoint. |

**E-5 Kind-specific object.** Each event carries one object under a key equal to its `kind`.

| Kind | Object |
|---|---|
| `started` | `{leaseUntil, listings?}`. Readers cap `leaseUntil` at `time` plus the stage's `lease` (C-7). [M18] `listings` are the signed listings the plan decided from, `[{statement, keys?}]`: each page's statement, the ledger's C2SP signed note (L-11), and its keys unless the page had more than 1000 (or all of them would take more than 512 KiB). `plan --start` records them, possibly none, in every `started` it writes to an HTTP ledger, and only there. |
| `check` | `{name, checkKind, definitionDigest, environment?, environmentDigest?, result, attempts, durationMs, startedAt, finishedAt, exitCode?, command?, reports?}`. `checkKind` is `source` or `deployment`; `result` is `pass` or `fail`; `command` is informational (§10.4, `run`); each report is `{name, digest: {sha256}}`. |
| `artifact` | `{name, type, ref, digest: {sha256}, origin, copiedFrom?}`. `type` is `oci` or `file`; `origin` is `built` or `copied`. `ref` MUST be pinned by digest (`…@sha256:…` for OCI), and that digest MUST equal `digest.sha256`. |
| `deployed` | `{artifacts: [{name, ref, digest: {sha256}}], target?: {name, detail?}}`. |
| `passed` | `{artifacts: [{name, ref, digest: {sha256}}]}` |
| `failed` | `{reason, checks?: [name]}` |
| `superseded` | `{reason, byRevision?}`, where `reason` is `stale` or `cancelled` |
| `approval` | `{check, comment?}` |

**E-6 Definition digest.** It is `sha256:` over the RFC 8785 (JCS) canonical JSON of the check's definition object, `{name, kind, run?, shell?, image?, platform?, env?, network?, version?}`, taken from `hushrun.yaml`:

- fields absent from the policy are absent from the object, but for `kind`: [ADR 0045] a check that doesn't write it is `source`, and has the digest it has with `kind: source`;
- `env` is the sorted list of variable names;
- `max_age`, `timeout`, `retries` and [ADR 0045] `counts_from` are not part of the definition.

Changing a definition invalidates its evidence.

**E-7 Environment.** `{image?: "<ref>@sha256:…", platform?: "linux/amd64", network?: "none" | "restricted" | "open", runner?: "<free text>"}`. `environmentDigest` is `sha256:` of its JCS form. [M8] An isolated run (E-12) records the pinned image, the platform it ran on (the pinned one, else the image's), the network it had, and `runner` naming the container runtime and its version.

**E-8 Verification.** An event is accepted only if every step below holds, in order. Otherwise it is ignored: the reason is logged, and `hushrun verify` shows it.

1. The object is at most 1 MiB and parses as a DSSE envelope or, [M10], a bundle of version 0.3 (E-1), with the in-toto payload type. A bundle's envelope has exactly one signature, and a key hint equals that signature's key id.
2. At least one signature counts:
   - [ADR 0031] a signature by the key of a machine among the repository's signers (T-15, L-13), when it verifies and the machine wasn't revoked at the signature's trusted time (E-10) or, without a trusted timestamp, isn't revoked now;
   - [M10] in a bundle with a certificate, the signature, when it verifies with the certificate's key, the certificate verifies at the trusted time (E-11), and its identity is the repository's CI (T-15). A keyless signature without a trusted timestamp never counts.
3. The payload is a Statement v1 with predicate type `…/event/v1`. Unknown fields are ignored; an unknown major version is rejected.
4. The subject matches:
   - `subject[0].name` equals `<repository>#<scope>`, and `predicate.repository`, `scope` and `canonicalization` also match.
   - For reads under `v1/events/<scope>/<digest>/`, `subject[0].digest.sha256` equals that digest.
   - For reads of the deployed index (L-8), there is no queried digest. The event's own subject digest is used, and `scope`, `stage` and `kind: deployed` must match the index path: an event without a stage only under `_`, and one with a stage only under that stage.
5. `kind` and `stage` match the storage key (§6), `_` standing for no stage. A storage path is never trusted on its own.
6. Timestamps are plausible:
   - [M10] with a trusted timestamp (E-10), `time` is within 10 minutes of the trusted time, either way (the same clock-skew allowance);
   - `time` is at most 10 minutes in the future (clock-skew allowance);
   - `check.finishedAt` is no later than `time` plus 1 minute.
7. Kind-specific constraints hold (E-5): for example, an artifact ref's digest equals `digest.sha256`. No kind needs a stage: since 0.9, a lifecycle event without one is accepted.
8. Effective custody is the lower of `producer.custody` and the signer's maximum custody: `ci` for the repository's CI, `isolated` for a machine (T-15).

Verification runs before the planner, with the same `now`. The planner sees only accepted events, and decides which of them count toward which requirement (§8.2).

[M18] On a ledger whose transparency log the reader checks (L-11), reading also checks the log: an accepted event's `checkpoint` (E-4) must be one history with the reader's, a rejected event's bytes must be the ones the log holds, and a listed key the ledger doesn't serve is the ledger's failure, never a missing event. Any of them ends the command with exit 6.

**E-9 Ordering.** Events are ordered by `time`, then by storage key. ULIDs in keys guarantee uniqueness, not authority.

**E-10 Trusted timestamps [M10].** Every signature hushrun makes gets an RFC 3161 timestamp, so that verification can tell when a signature existed without trusting its signer's clock.

- **Requests.** The timestamp is over the SHA-256 of the signature bytes. hushrun asks the timestamp authorities that `HUSHRUN_TSA` names (T-12) in order, with a random nonce and asking for the authority's certificate, and the bundle carries the whole DER `TimeStampResp` of the first authority whose timestamp verifies (E-1). When none does, a key's event goes out without a timestamp, with a warning, and counts only while its key may sign (T-11); a keyless signature fails (E-11).
- **Verification.** A timestamp verifies when:
  - the response's status is granted (0 or 1), and its token is CMS `SignedData` around `TSTInfo` version 1;
  - the message imprint is the hash of the signature (SHA-256, SHA-384 or SHA-512);
  - the token has one signer, whose signed attributes carry `TSTInfo`'s content type and message digest and, when present, an ESS signing-certificate attribute that names the signer's certificate by its hash;
  - the signer's signature over the signed attributes, as written, verifies with its certificate's key;
  - that certificate may timestamp (extended key usage `timeStamping`), was valid at `genTime`, and belongs to a timestamp authority of the Sigstore trust root (T-12): it is the authority's own certificate, or one of the authority's certificates issued it; and the trust root trusts the authority at `genTime`.
- **Trusted time.** A signature's trusted time is the `genTime` of its bundle's first timestamp that verifies. Timestamps that don't verify are ignored, as if absent.
- **Uses.** `time` agrees with the trusted time (E-8 step 6); keyless certificates are checked at it (E-11); a revoked machine's signatures are checked against it only (T-15).

**E-11 Signing certificates [M10].** Keyless signing has no long-lived key: an ephemeral P-256 key signs, under a certificate that lasts minutes.

- **Signing.** The key is made in memory for one command and never written. Fulcio certifies it for the CI's ID token (audience `sigstore`, T-12), with a proof of possession: the key's signature over the token's `email` claim, else its `sub`. hushrun checks that the certificate certifies its key and verifies against the Sigstore trust root, and asks for a new one when less than two minutes are left. Every keyless signature needs a trusted timestamp (E-10).
- **Verification at the trusted time `t`.**
  - A certificate authority of the Sigstore trust root (T-12), trusted at `t`, issued the certificate: the certificate names the authority's first certificate as its issuer, and that certificate's signature over it verifies; each certificate of the authority's chain was issued by the next, and the first is a CA that may sign certificates.
  - Every certificate, the signing certificate and the authority's, was valid at `t`.
  - The signing certificate is no CA; its key usage allows digital signatures and its extended key usage code signing; it has no critical extension other than key usage, extended key usage, subject alternative name and basic constraints; its key is P-256 and verifies the signature.
  - When the trust root lists CT logs, one of the certificate's embedded SCTs (RFC 6962) verifies with one of them, over the certificate without its SCT list and its issuer's key hash, and the log was trusted when it logged the certificate.
- **Identity.** A certificate names: the OIDC issuer (extension `1.3.6.1.4.1.57264.1.8`, else the older `.1.1`); the subject, the first URI, email address or Fulcio username of the subject alternative name; and, when present, Fulcio's Source Repository Identifier (`.1.15`), Source Repository URI (`.1.12`), Source Repository Ref (`.1.14`), Build Config URI (`.1.18`) and Build Signer URI (`.1.9`). Principals list identities (T-10).
- A keyless signature's `keyid` in the envelope is empty; readers name it by `sha256:` of the certificate's DER.

**E-12 Isolated runs [M8].** `hushrun run --isolated` runs each check in a container, so that the evidence it signs is about exactly the content it names, run as CI would run it:

1. **The export.** The entries of the scope's digest (D-7), read through the VCS's own readers, never an export tool that applies attributes, are written to a new directory: regular files with mode 0644 or 0755 (D-3), symbolic links with their targets as stored, and a gitlink as an empty directory. Nothing on the way is followed, so nothing lands outside it. Every path segment must be one file name on the host (on Windows, not a drive, stream, device name, or a name ending in a dot or space), and the directory must then hold exactly those paths, or the run fails before the check starts (a case-insensitive filesystem folds paths that differ only in case). The entries must digest to the subject's digest: evidence is never recorded for content other than what ran. Each attempt (C-5 `retries`) starts from a new export.
2. **The container.** The check runs in its pinned `image`, which it must have (C-5), for its `platform`, with Docker or Podman (`HUSHRUN_CONTAINER_RUNTIME`, else `docker`, else `podman`): the export mounted at `/src`, its working directory, with no new privileges, its `run` through its `shell` (default `sh`) or the command after `--` as the entrypoint. A missing image is pulled first. The runtime's own failures record nothing: a failed pull, or exit code 125 from a container whose check never started, as its state says (a check that exits 125 itself fails, recorded). The container is removed afterwards. A stop signal (SIGINT, SIGTERM, SIGHUP) while the check runs removes the container and the export, records nothing, and ends hushrun as the signal asked; what a killed run left is removed by the next isolated run after a day.
3. **The network** is the check's `network` (C-5): `none` by default, the runtime's default network for `open`, and for `restricted` the network `HUSHRUN_RESTRICTED_NETWORK` names, whose egress the operator restricts; without it, a `restricted` check doesn't run (exit 2), since hushrun can't restrict a network by itself.
4. **The environment** holds only the variables the check's `env` names that are set, passed by name so their values never reach a command line, and never a secret: a check whose `env` names hushrun's or CI's credentials, or any name with `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `PASSPHRASE`, `CREDENTIAL`, `PRIVATE`, `AUTH`, `ACCESS_KEY`, `APIKEY`, `DATABASE_URL` or `_KEY_` in it, ending in `_KEY`, `_PAT`, `_DSN` or `_PWD`, or a credential-bearing URL such as `REDIS_URL`, doesn't run isolated (exit 2). The signing key stays with hushrun, outside the container: a key file (`HUSHRUN_SIGNING_KEY_FILE`) among the exported entries refuses the run (exit 2).
5. **The evidence** records the environment (E-7) and custody `isolated`, or `sandbox` when the operator's ephemeral runner holds the key and says so with `HUSHRUN_CUSTODY=sandbox`, or `ci` in CI (E-4).

Example of a decoded `check` payload:

```json
{
  "_type": "https://in-toto.io/Statement/v1",
  "subject": [{"name": "github.com/acme/shop#default",
               "digest": {"sha256": "5f0c…e1"}}],
  "predicateType": "https://github.com/hushrun/hushrun/spec/event/v1",
  "predicate": {
    "kind": "check",
    "repository": "github.com/acme/shop",
    "scope": "default",
    "stage": "staging",
    "canonicalization": "hushrun-source-v1",
    "source": {"vcs": "git", "revision": "1248b05…", "ref": "refs/heads/develop", "tree": "eb812ed…", "objectFormat": "sha1"},
    "run": {"provider": "github", "id": "38480001234", "attempt": 1, "trigger": "push", "job": "test",
            "url": "https://github.com/acme/shop/actions/runs/38480001234"},
    "producer": {"tool": "hushrun", "version": "0.4.0", "custody": "ci"},
    "time": "2026-10-13T15:04:05.123Z",
    "policyDigest": "sha256:9a1b…",
    "check": {"name": "unit", "checkKind": "source", "definitionDigest": "sha256:03bd…",
              "result": "pass", "attempts": 1, "durationMs": 95410, "exitCode": 0,
              "startedAt": "2026-10-13T15:02:30.000Z", "finishedAt": "2026-10-13T15:04:05.000Z"}
  }
}
```
