# 0013. No long-lived keys: keyless signing, bundles, timestamps, P-256 and KMS keys

- **Status:** accepted; its KMS signers (item 10) superseded by [0022](0022-https-ledgers-only-and-no-kms.md). Keyless signing,
  timestamps and P-256 keys stand
- **Date:** 2026-10-15
- **Rules:** E-1, E-2, E-4, E-8, E-10 (new), E-11 (new), T-1, T-8, T-10 (new), T-12 (new), §10.3

## Context

Signing keys today are PEM files in CI secrets. Any step of a job that reads one can copy it, and
the copy signs as that stage until someone notices. Removing a key from trust drops every event
it signed, `failed` ones included, and a key kept "until date X" proves nothing, because `time`
is whatever the signer wrote (RFC 0001 §3.2).

RFC 0001 §4.2 (accepted) moves M10 up and widens it to "no long-lived, exportable keys":

- CI signs as its workflow identity, through Sigstore: Fulcio certifies an ephemeral key for the
  CI's ID token, for minutes;
- or with a key that never leaves a cloud KMS;
- every signature gets an RFC 3161 timestamp, so every event becomes a Sigstore bundle, and keys
  and identities get validity windows checked against trusted time (ADR 0014);
- identities match the calling repository's numeric id, its ref and its workflow, not only a
  subject;
- ECDSA P-256 joins Ed25519.

The plan named the sigstore-rust crates (§11.4). They can't be used:

- `sigstore` 0.14 depends on `aws-lc-rs` unconditionally;
- the new `sigstore-crypto` 0.15, published two days before this ADR, has `aws-lc-rs` as its
  only backend.

`deny.toml` bans `aws-lc-sys` and `aws-lc-rs` (C code that needs CMake). The control plane's
server verifies events with `hushrun_attest::verify`, and must get all of this without changes
on its side.

## Decision

### Formats

1. **Every event is a Sigstore bundle (E-1)**, whatever signs it.
   - The bundle has version 0.3 (`application/vnd.dev.sigstore.bundle.v0.3+json`) and wraps
     the same DSSE envelope, with exactly one signature.
   - Its verification material is `publicKey.hint`, equal to the signature's key id, or
     `certificate.rawBytes`, the DER leaf certificate. A keyless signature's key id is empty.
   - Timestamps go in `timestampVerificationData.rfc3161Timestamps`, as whole DER
     `TimeStampResp`s, the form other Sigstore clients write.
   - `tlogEntries` is empty (item 9).

   A timestamp needs a place to live, and one format for every signer keeps readers simple.
   Readers accept plain DSSE envelopes too: every event written before M10 is one. Root versions
   and signed trust (T-8) stay plain envelopes: people sign them, with several signatures.

2. **P-256 keys (E-2).**
   - They are written `ecdsa-p256:<standard base64 of the DER SubjectPublicKeyInfo>`, the form
     KMSs return and certificates carry.
   - Only the canonical encoding is accepted (named curve, uncompressed point, 91 bytes), so a
     key has one text and one key id.
   - The key id is `sha256:` of those bytes: the same rule as Ed25519's (the hash of the bytes
     the text encodes), and the same id CT logs use.
   - Signatures are DER `ECDSA-Sig-Value`s over SHA-256. Azure Key Vault's `r || s` is
     converted.
   - Private keys are PKCS#8 PEM, as for Ed25519. `hushrun keys generate --algorithm
     ecdsa-p256` writes one, and OpenSSL's keys are read too.
   - ring signs and verifies them: it is already the TLS provider for every target, so there is
     no new C code.

### Keyless signing

3. **Signing (E-11).**
   - The key is a P-256 key made in memory for one command. It is never written anywhere.
   - hushrun asks GitHub Actions for an ID token with audience `sigstore`, or reads
     `SIGSTORE_ID_TOKEN`.
   - It sends Fulcio's v2 `signingCert` request with the public key and a proof of possession:
     its signature over the token's `email`, else its `sub`, as Sigstore's clients do.
   - It checks that the certificate certifies its own key and verifies against the trust root,
     at the certificate's own start, so the runner's clock doesn't matter. A Fulcio that the
     trust root doesn't cover fails at signing (exit 2), not at every reader.
   - One certificate serves all of a command's events, and a new one is requested when less
     than two minutes are left.

4. **Certificates are verified at the trusted time (E-11).**
   - A certificate authority of the trust root, trusted at that time, issued the certificate.
     Every certificate of the chain was valid then.
   - The leaf is for code signing, with a P-256 key, and has no critical extension hushrun
     doesn't check.
   - When the trust root lists CT logs, as public-good Sigstore's does, one embedded SCT must
     verify. This is what Sigstore's clients do: a certificate that Fulcio didn't publish
     doesn't count.
   - A keyless signature without a timestamp never counts: its certificate lasts ten minutes,
     and nothing else says the signature was made inside them.

### Trusted timestamps

5. **Timestamps (E-10).** Every signature is timestamped over the SHA-256 of the signature
   bytes, as Sigstore's clients do.
   - **Requests.** Authorities are tried in the order `HUSHRUN_TSA` lists them, by default
     public-good Sigstore's. Each request carries a nonce and asks for the authority's
     certificate. A timestamp is kept only when it verifies against the trust root.
   - **Verification.** The token is CMS `SignedData` around `TSTInfo`, and the signer's
     signature covers the signed attributes as written; nothing is re-encoded before checking a
     signature. The signed attributes must carry the content type and message digest, and an
     ESS signing-certificate attribute, when present, must name the signer's certificate by
     its hash. The signer's certificate must be for timestamping and must belong to a
     timestamp authority the trust root trusts at `genTime`.
   - **Trusted time.** A signature's trusted time is the first verifying timestamp's
     `genTime`.
   - **No answer.** When no authority answers, a key's event goes out without a timestamp, with
     a warning. It then counts only while its key may sign (T-11). This is RFC 0001's
     availability rule for KMS signing. A keyless event fails instead (exit 9).

6. **`time` must be within 10 minutes of the trusted time, either way (E-8 step 6).**
   - This is the same clock-skew allowance step 6 already gives `time` against the reader's
     clock.
   - The CLI now sets `time` when it signs, as E-4 defines it, rather than when the command
     started. So a `plan --start` after a long `--wait-timeout`, or an `artifact` event after
     a long copy, still agrees with its timestamp.
   - The timestamp is requested right after signing. The two times differ by the runner's
     clock error plus seconds of network: 10 minutes covers a badly set clock, and anything
     beyond it is a broken clock or a lie.
   - Windows and certificates never use `time`, only the trusted time. So the allowance bounds
     only how far `time` can move an event in E-9's order.
   - Whether trusted times should order events, and replace P-6's 60 seconds, stays RFC 0001's
     open question 3.

### Identities

7. **Identities (T-10) match the issuer, the numeric repository id, the ref and the
   workflow.**
   - `issuer` is compared exactly. `repository_id` is compared with Fulcio's Source Repository
     Identifier, `ref` with Source Repository Ref, `workflow` with the Build Config URI, and
     `signer_workflow` with the Build Signer URI.
   - An identity with `repository_id` must also name `ref`, and `workflow` or
     `signer_workflow`. A pattern such as `refs/heads/*` accepts every branch on purpose.
     Without these fields, any branch or any workflow of the repository could sign as the
     stage.
   - Written as a path, `workflow` must sit between the certificate's own repository URI and
     `@` its own ref. So the workflow that signed is the one at the ref the identity names, in
     the repository the id names.
   - `*` in `ref`, `workflow`, `signer_workflow` and `subject` matches any run of characters.
   - A certificate whose identity matches identities of two principals is rejected. Globs can
     overlap, and a signer must count for one principal only.

   **Why the subject alone is unsafe with reusable workflows.** Fulcio puts the workflow that
   signed into the subject alternative name. That is GitHub's `job_workflow_ref`, the Build
   Signer URI. When repository A's workflow calls a reusable workflow in repository B, the job
   that signs runs B's workflow, so the certificate's subject is
   `https://github.com/B/.github/workflows/shared.yml@refs/heads/main`. That is the subject
   every repository calling the same workflow gets. An identity `{issuer, subject: <that URI>}`
   would accept events signed by any repository on GitHub that calls B's workflow, including
   one an attacker creates for that purpose.

   What tells the callers apart is in other extensions:
   - the Source Repository Identifier, the caller's numeric id;
   - the Source Repository Ref;
   - the Build Config URI, the caller's top-level workflow.

   So hushrun never matches a certificate that names a source repository with an identity that
   names none. Subject-only identities stay, for people's certificates, which name no
   repository. The id is numeric because a name can be freed and registered again by someone
   else, and the id can't.

### Configuration and availability

8. **Signers and the trust root come from the environment (T-12), never from the tree.**
   - `HUSHRUN_SIGNING` chooses `key`, `kms` or `sigstore`. `HUSHRUN_SIGNING_KMS` names a KMS key
     version, with cosign's URI schemes (`gcpkms://`, `awskms://`, `azurekms://`) and a
     required version.
   - `HUSHRUN_FULCIO_URL` and `HUSHRUN_TSA` name the services.
   - The trust root is, in order:
     - the file `HUSHRUN_SIGSTORE_TRUSTED_ROOT` names;
     - the copy `hushrun sigstore refresh` cached;
     - the public-good snapshot compiled into the binary.
   - The snapshot is the `trusted_root.json` that Sigstore's TUF repository published as
     targets version 14. The TUF root (version 15) and that `targets.json` are vendored with
     it, and a test checks that root signs the targets, which pin the snapshot.
   - **The refresh** is a TUF update from that root:
     - root rotations, each signed by the thresholds of both versions;
     - the timestamp, snapshot and targets roles, their versions and expiry;
     - the target's length and hashes;
     - no `targets.json` older than the cached one or the snapshot.
   - **Readers verify the cache again, offline**, from the initial root to the cached targets
     and trust root, and ignore it when anything fails. The cache directory may be shared
     through CI caches, and a poisoned cache must not be able to add a certificate authority.
     Readers don't check TUF expiry: reading must work offline, and freshness is what a
     refresh is for.
   - `HUSHRUN_SIGSTORE_TUF_URL` and `HUSHRUN_SIGSTORE_TUF_ROOT` name another repository, such
     as a private Sigstore's or GitHub's.

9. **No transparency log entries.**
   - RFC 3161 timestamps give the trusted time.
   - Public-good Fulcio already publishes every certificate to its CT log, and readers require
     its SCT.
   - A Rekor entry would add a second public record of the same identity and another service
     on the write path, for no property M10 needs.
   - M18 brings hushrun's own log.

10. **KMS signers.**
    - **GCP** (`asymmetricSign` with the digest): tokens come from the GCS ledgers' chain, with
      the cloud-platform scope for an impersonated service account. `hushrun-ledger` now
      exposes its Google and AWS chains for this.
    - **AWS** (`Sign`, `MessageType: DIGEST`): keys come from the S3 ledgers' chain. Requests
      are signed with SigV4 in-house, tested against AWS's published vector.
    - **Azure Key Vault**: tokens are for the vault's scope, which the storage chain can't
      give. They come from a client secret, a federated token file, GitHub's ID token federated
      with an app registration, the Azure CLI, or managed identity.
    - Only P-256 keys are used. Every signature a KMS returns is verified with the key's public
      key before hushrun uses it. `hushrun keys public --kms <uri>` prints the key for
      `trust.yaml`. `hushrun trust sign` signs with a KMS key too.

11. **An unavailable or refusing signer exits 9.** Exit 6 stays the ledger. Exit 9 covers the
    ID token endpoint, Fulcio, every timestamp authority for keyless signing, the KMS, and the
    TUF repository during a refresh.

    **No automatic fallback to a KMS key.** RFC 0001 asked this ADR to decide whether a
    principal may list a KMS key to fall back on.
    - It may: a principal can list identities and keys together, so moving a pipeline from
      keyless to KMS signing needs no change of trust.
    - hushrun doesn't fall back by itself. A pipeline configured to sign keylessly that quietly
      signs with a KMS key during a Sigstore outage changes what its events claim, and uses a
      longer-lived credential than the one it was meant to.
    - The recipe chooses, through `HUSHRUN_SIGNING`, and a failed signature fails the step.

12. **Verification stays in the libraries.**
    - `hushrun_attest::verify(bytes, trust, expected, now)` keeps its signature and its
      `Rejection` variants. Certificate, timestamp, identity, window and inventory failures are
      step 2, and a `time` far from its timestamp is step 6.
    - It verifies with the public-good trust root compiled in. `verify_with` takes another
      root.
    - `Verified` gains `signed_by` (a key, a retired key, or an identity) and `trusted_time`.
      Its `keyid`, for a keyless signature, is `sha256:` of the certificate.
    - The network code (Fulcio, timestamp authorities, TUF, KMSs) sits behind the `sigstore`
      and `kms` features, so a server that only verifies builds none of it.

13. **Dependencies.**
    - `x509-cert` and `cms` are RustCrypto's pure-Rust X.509 and CMS types, on the `der` 0.7
      that the Ed25519 PKCS#8 support already uses. They add `der_derive` and `flagset`.
    - ring verifies P-256, P-384, RSA and Ed25519 certificate and timestamp signatures, and signs
      P-256.
    - `p256` is used only by the testkit's fake KMSs, which must sign digests, as KMSs do. ring
      signs whole messages only.
    - Typed decoding finds the fields, but signatures are checked over the exact bytes read raw
      (`der_raw`), so no re-encoding can make a signature verify.

14. **GitHub's Sigstore instance (RFC 0001 open question 1).**
    - GitHub's `actions/attest` signs private and internal repositories' attestations with
      GitHub's own Fulcio and timestamp authority. Storing them needs GitHub Enterprise Cloud.
    - GitHub documents that action, not other clients, and hushrun doesn't depend on it.
    - An organization that confirms it may use it points `HUSHRUN_FULCIO_URL`, `HUSHRUN_TSA`
      and the trust root at it.
    - KMS stays the default for private repositories (RFC 0001 §5.4): public-good Fulcio's CT
      log publishes every certificate, and with it the repository's and the workflow's names.

15. **Outputs.**
    - `verify` and `ledger show` say how each event was signed and when it was timestamped.
      `hushrun.verify/v1` gains `signer` and `trustedTime`.
    - `doctor` replaces its "signing key" check with "signer". For a keyless job, it reads the
      ID token's claims to show the identity Fulcio would certify. It adds "sigstore trust
      root" and "timestamps".
    - New schemas: `hushrun.sigstore.refresh/v1` and `hushrun.ledger.inventory/v1`.

## Consequences

- **Readers before writers.** A reader older than M10 can't read a bundle, so it ignores
  events that M10 writers sign. Upgrade every reader (plan jobs, the index) before any writer,
  as RFC 0001 Appendix A says.
- **Default timestamps.** Key signers now ask Sigstore's timestamp authority for every event:
  one request each, and a warning when it doesn't answer. `HUSHRUN_TSA=none` opts out, and
  then rotation can't tell old events from new ones (ADR 0014).
- **Offline verification.** It works from the binary's snapshot or a refreshed cache. A new
  Fulcio intermediate needs a refresh or a newer binary before its certificates verify.
- **Public identities.** Keyless signing on public-good Sigstore publishes repository and
  workflow names. The guide says so, and recommends KMS for private repositories.
- **The control plane's server** gets bundles, keyless identities against the public-good
  root, P-256 keys, windows and inline inventories through `verify` as it is. A private
  Sigstore needs `verify_with`. Inventory files need the server to read them and call
  `Trust::attach_inventory` (ADR 0014).

## Alternatives considered

- **The sigstore-rust crates.** They need aws-lc, which `deny.toml` bans, and the newest are
  days old.
- **Bundles only for keyless signatures.** Key-signed events then couldn't carry timestamps,
  and rotation would stay meaningless (§3.2).
- **Timestamps in a separate object beside each event.** Two objects that must agree, and an
  append-only ledger can't attach the second to the first.
- **Rekor instead of timestamp authorities.** A public record of every signing identity,
  another online dependency for writes, and Rekor's key as the source of time. Timestamp
  authorities are the customer's choice (RFC 0001 §4.2).
- **A raw point for P-256 keys.** KMSs and certificates speak SubjectPublicKeyInfo. One
  canonical DER form keeps key ids stable.
- **Matching identities by subject globs only**, as the plan had it. Unsafe with reusable
  workflows (item 7).
- **Falling back from keyless to a KMS key automatically.** Rejected in item 11.
- **Checking TUF expiry when reading the cache.** Verification would stop working offline a
  week after each refresh.
