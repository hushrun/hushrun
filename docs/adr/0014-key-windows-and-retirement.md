# 0014. Validity windows and retired keys

- **Status:** accepted;
  superseded by [0031](0031-membership-is-trust.md) on 2026-10-23
- **Date:** 2026-10-15
- **Rules:** T-1, T-11 (new), E-8, E-10, L-9

## Context

Before M10, rotation and revocation had no clean meaning (RFC 0001 §3.2):

- Removing a key from `trust.yaml` makes every event it signed unverifiable, old `failed`
  events included. Under the default `upstream: not-failed`, a digest that staging failed can
  then stop being blocked.
- Keeping the old key "until date X" doesn't work either. `time` is whatever the signer wrote
  (E-4), so a stolen key signs events dated before X.

M10 gives every signature an RFC 3161 timestamp from an authority the signer doesn't control
(E-10, [ADR 0013](0013-keyless-signing.md)). Validity windows can now be checked against a time
the signer can't choose.

Events signed before M10 carry no timestamp. RFC 0001 §4.2 says the keys that signed them retire with an inventory instead: admins
list in trust the events each retired key signed, and the CLI accepts a retired key's events
only from that list.

## Decision

1. **Windows.**
   - Keys and identities may have `not_before` and `not_after`: RFC 3339 times, both
     inclusive, the second later than the first. A key becomes a mapping
     `{key, not_before?, not_after?}`; a plain key, as before, may sign at any time.
   - A timestamped signature counts only when its trusted time is in the window of the key or
     identity that made it.
   - A key's signature without a timestamp counts only while the key's window contains the
     reader's now.
   - A keyless signature always has a timestamp (E-11).
   - The signer's `time` never decides.

   Rotation then means what RFC 0001 asks:
   - the old key gets a `not_after`, and its timestamped events keep counting for as long as
     they live;
   - a stolen old key can't sign anything that counts: a timestamp it gets is from now, after
     `not_after`, and an event without one counts only while the key may sign.

2. **Untimestamped events of a windowed key stop counting at `not_after`.** hushrun can't tell
   when such an event was signed, so it reads that as the key being invalid now (RFC 0001
   §4.2). This is the price of `HUSHRUN_TSA=none`, and of an authority outage, for key signers.
   `doctor` warns about both.

3. **Retired keys.**
   - `retired: {events: [sha256:…]}` lists the events in the trust policy itself.
   - `retired: {inventory: <file>, digest: sha256:…}` names a file next to the trust policy and
     pins it by digest.
   - A retired key's signature counts only for a stored object whose SHA-256 the inventory
     lists, whatever its timestamps. The deployed index holds the same bytes as `events/`, so
     one entry covers both copies.
   - The digest of the whole object, rather than its key or its payload, names exactly the bytes
     that were inventoried. A storage path is never trusted (E-8), and an inventory of payloads
     would accept the same statement signed again.
   - A retired key has no window.

4. **Inventory files** hold one `sha256:<hex>` per line.
   - Their names are relative paths of plain segments, outside the names the ledger uses for
     itself: `trust.yaml`, `trust.dsse.json`, `root/…`, `v1/…`. The guide suggests
     `inventory/<principal>.txt`.
   - Signed trust (T-8) pins them by digest, so the root's signatures cover them too.
   - Readers read them where they read the trust policy: next to the `--trust` file or URL,
     else in the ledger.
   - **A missing or changed file is a configuration error (exit 2).** Dropping an inventory
     would drop the retired key's events, `failed` ones included, which is exactly the failure
     this ADR exists to prevent.

5. **Commands.**
   - `hushrun ledger inventory --key K [--out FILE]` lists the stored events whose signatures
     by `K` verify, sorted, and prints the digest to pin and the YAML to paste (JSON
     `hushrun.ledger.inventory/v1`).
   - `hushrun ledger put-trust` uploads each inventory the trust names from next to the file it
     uploads, checked against its pinned digest, before the trust itself, so readers never see
     trust that names a missing inventory. An inventory the ledger already holds with the right
     digest stays.
   - File ledgers now create the directories of nested configuration names. The ledger
     conformance suite checks such names on every backend.

6. **GC ends retirement.** Once `ledger gc` has deleted a retired key's events, nothing depends
   on its inventory, and admins remove the key and its file from trust (RFC 0001 §4.2).

7. **The library.**
   - `Trust::inventory_files` lists what a reader must fetch, and `Trust::attach_inventory`
     checks a file's digest and attaches it.
   - `verify` rejects a retired key's events whose inventory wasn't attached, with a reason
     that says so. Inline inventories need nothing.

## Consequences

- **Moving a ledger off keys:**
  1. `hushrun ledger inventory --key <each old key> --out inventory/<name>.txt`;
  2. new signers in trust: identities, or KMS keys;
  3. each old key as `retired` with its inventory;
  4. sign the trust, and `ledger put-trust`.

  The old keys' secrets can then be deleted: a copy of them signs nothing that counts.
- **Size.** An inventory is about 72 bytes per event. Inline in a trust policy, a 1 MiB
  `trust.yaml` (C-8) holds roughly 14,000 events; larger inventories go in files.
- **The control plane's server** must read inventory files from the ledger and attach them
  before verifying events of retired keys. Until it does, those events show as rejected with
  "the inventory … wasn't read". Inline inventories work unchanged. An `https://` ledger must
  serve and accept configuration names with directories (`config/inventory/…`), which L-10
  allows.

## Alternatives considered

- **Retiring a key by date, without timestamps.** A stolen key backdates (RFC 0001 §3.2).
- **Removing old keys.** Their `failed` events disappear with them.
- **Inventories of storage keys.** Paths aren't authenticated; bytes are.
- **Signing inventories separately.** The trust policy that pins them is already signed (T-8),
  and a digest is all it needs.
- **Accepting a retired key's events when the inventory can't be read.** Fails open, and could
  unblock a digest staging failed.
