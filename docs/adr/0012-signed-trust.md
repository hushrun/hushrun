# 0012. Signed trust and the root chain

- **Status:** accepted; since [0022](0022-https-ledgers-only-and-no-kms.md) every ledger has a
  root, so readers never take plain `trust.yaml` from a ledger
- **Date:** 2026-10-15
- **Rules:** T-8 (new), T-4, C-3, L-2, L-3, L-6, L-10, E-1, E-2

## Context

Today `trust.yaml` is as safe as the bucket's IAM: whoever can write it decides who signs.
RFC 0001 §4.3 (accepted) adds a root, signed by people, that trust must be signed by; §4.7
requires it for a hosted ledger, which would otherwise serve the very trust that verifies it, and
pins the root's first version in `hushrun.yaml`. The object formats were agreed with the
server's implementation (its contract's §7); this ADR fixes the details. The control plane's
server stores these objects and shows them, so verification lives in a library it can call.

## Decision

1. **Formats.** A root version, `root/<n>.json`, is a DSSE envelope (E-1) of payload type
   `application/vnd.hushrun.root+json` around
   `{"_type": "hushrun.root/v1", "version", "threshold", "keys", "expires"}`. Signed trust,
   `trust.dsse.json`, is one of `application/vnd.hushrun.trust+json` around
   `{"_type": "hushrun.trust/v1", "serial", "expires", "trust"}`, where `trust` is the
   `trust.yaml` text. Payloads are compact JSON. hushrun writes `expires` with milliseconds, as
   events write `time`; readers accept any RFC 3339 time and ignore fields they don't know.
   Keys are `ed25519:<base64>` and key ids `sha256:<hex>` (E-2); P-256 and KMS keys come with
   M10. `docs/schemas/root.v1.schema.json` and `signed-trust.v1.schema.json` describe the
   payloads.
2. **Signatures count once per key.** A signature counts when its key id names a key of the
   version in question and it verifies over the envelope's PAE. Two signatures by one key count
   once. A version lists each key once, and its threshold is between 1 and its number of keys.
3. **The chain rotates like TUF's root.** Version 1 carries `threshold` of its own signatures.
   Version n > 1 carries `threshold(n−1)` signatures by version n−1's keys and `threshold(n)`
   by its own; a key in both versions counts toward both. Each version's payload must name its
   own number. Readers fetch `root/1.json`, `root/2.json`, … until one is missing, and refuse
   the whole chain when any version fails, so that a broken newer version can't be skipped in
   favor of an older one.
4. **Only the newest version must be unexpired.** Older versions may have expired; a rotation
   replaced them. Checking every version would break a chain for good once any old version
   expired.
5. **Trust counts only by the newest root.** Signatures by an older version's keys don't
   count, so a rotation retires keys. The serial must be at least 1, and `expires` must be in the
   future, compared with the command's `now`.
6. **Serials only go up, at upload.** Readers keep no state across commands (§12.6), so they
   can't compare serials; `ledger put-trust` refuses a serial that isn't above the ledger's
   current one, and the index can flag one that goes down (RFC 0001 §4.3). Only a current
   envelope that a threshold of some root version's keys signed, expired or not, counts
   (`signed::signed_serial`): junk with a huge serial must not block every later upload.
7. **Readers.** A ledger with `root/1.json` takes signed trust only: `trust.dsse.json`, or the
   envelope that `--trust` names, verified against the ledger's chain. `trust.yaml` is ignored,
   a plain `--trust` file is refused, and so is `--trust-from-tree`: otherwise a CI variable, or
   the tree, could bypass the root. Commands that read no ledger (`policy validate --trust`)
   read what they are given. A refusal exits 2, like a missing `trust.yaml`; a ledger that can't
   be read exits 6 (L-5).
8. **An `https://` ledger needs a root, and the pin.** `hushrun.yaml`'s `ledger_root` names the
   digest of `root/1.json`'s bytes (C-3); readers refuse a chain that doesn't start there, and
   say which digest the ledger has, to pin once whoever created the root confirms it. A pin on
   any other ledger is honored the same way; requiring it there is RFC 0001's open question 12.
9. **Verification is a library.** `hushrun_attest::signed::verify_roots` and `verify_trust`
   take bytes and the time, and touch no storage. The CLI reads the objects through the
   `Ledger` trait; the server can verify what it stores with the same functions.
10. **Commands.**
    - `hushrun trust root init --key … --threshold K --expires D [--root-version N] --out FILE`
      writes an unsigned version, by default version 1 with threshold 1, valid for 365 days. It
      never overwrites a file.
    - `hushrun trust sign FILE [--key PEM]` adds one signature, in place, and changes nothing
      when that key already signed. Version 1 refuses a key it doesn't list; a later version
      warns, because the key may be one of the previous version's. Given a `trust.yaml`, it
      first wraps the text in a new envelope (`--serial`, required; `--expires`, 90 days by
      default) written beside it as `trust.dsse.json`, never over an existing file.
    - `hushrun ledger put-root FILE` uploads version n once versions 1 … n−1 are in the
      ledger and the chain verifies with it, create-only (`create_config`, L-3). Uploading the
      same bytes again changes nothing. For version 1 it prints the pin.
    - `hushrun ledger put-trust FILE` takes a signed envelope, verifies it against the ledger's
      chain, checks its `trust.yaml` against `hushrun.yaml`, and refuses a lower serial. A plain
      `trust.yaml` is refused on a ledger with a root, and on an `https://` ledger.
11. **`doctor`** reports the serial, the signers and the root version, and warns 14 days
    before signed trust expires and 30 days before the newest root does.

## Consequences

- Rotating the root makes trust signed by the old keys stop counting at once. Sign the new
  serial with the new keys first, then upload the root and the trust one right after the other;
  `put-root` says so.
- Readers keep no state, so with write access to a bucket someone could put back an older signed
  envelope, until it expires, or delete newer root versions, which matters only if an older
  root's keys leaked. Retention locks on every root version, object versioning, and the index's
  alerts are the answer; a hosted ledger keeps roots create-only and never deletes them.
- A ledger without a root costs one more request per command, the `GET` that finds no
  `root/1.json`.
- Root and trust signers hold Ed25519 keys until M10 brings KMS and hardware keys.

## Alternatives considered

- **A detached signature beside `trust.yaml`.** Two objects that must agree, and a reader that
  finds one without the other. An envelope keeps the text and its signatures together, in the
  format events already use.
- **Accepting trust signed by any version of the chain.** Retired keys could keep signing it.
- **Checking every root version's expiry.** An expired old version would break the chain
  forever, and rotation exists to replace expiring versions.
- **Remembering the newest root and serial seen**, as TUF clients do. hushrun keeps no cache
  across commands (§12.6), and a cache in CI would be per runner; the index watches instead.
- **The root in a CI variable.** On GitHub, a repository variable overrides an organization's
  variable of the same name, and reaches the CLI only through the workflow's own `env:`
  (RFC 0001 §4.3).
