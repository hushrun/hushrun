# 0017. The hosted ledger's transparency log (H1)

- **Status:** accepted
- **Date:** 2026-10-15
- **Rules:** L-11 (new), T-9 (new), L-9, L-10, E-4, E-5, E-8, L-5, L-7, T-8

## Context

RFC 0001 §4.7 (accepted) makes the hosted ledger an append-only Merkle log in two phases. The
threat is withheld events: storage carries no trust (E-8), so the ledger can't forge an event,
but it can leave one out of a listing. Hiding a whole failed attempt can sway a plan (the
upstream's walk, P-7, then reaches an older attempt or none). With a customer's own bucket the
party who could do that is the customer's admin, whom L-6 limits; with a hosted ledger it is us,
so readers must be able to catch it.

H1, this ADR, makes it impossible to do quietly: every deploy decided from an incomplete listing
is found after the fact, with the ledger's own signed statement as proof, and a ledger that
shows two histories is caught when a reader meets a checkpoint from the other one. H2 (M19:
listing proofs and witnesses) will refuse the incomplete listing before the decision. The
control plane rules hold: the log changes no planner input (CP-1), the ledger stays the only
part on the deploy path (CP-2), and its key is trusted only because signed trust names it
(CP-3). Phase 2 of the hosted product builds it now; the server's side is in the private
monorepo, against the contract `ledger-http-v1` §10.

## Decision

**Formats** (L-11), all in a new crate, `hushrun-tlog`: pure Rust on ring (which TLS already
brings), shared by the CLI, its monitor and the server.

1. **RFC 6962, exactly.** Leaf and node hashes with the `0x00`/`0x01` prefixes, the split at the
   largest power of two, `PATH` and `PROOF`, verified as RFC 9162 §2.1 restates them. The crate's
   vectors come from `tools/reftlog.py`, which computes them from the RFC's recursive
   definitions, and match Certificate Transparency's published ones; property tests compare the
   store-of-complete-nodes algorithms the server uses with a whole tree.
2. **A leaf is the hash of an entry naming the key and the SHA-256 of the bytes**:
   `hushrun-tlog-entry/v1`, the kind (`object`, `config`, `tombstone`), the name, the hex hash,
   and for a tombstone the hash of its GC statement or `gc none`, each line ending in `\n`.
   Hashing the bytes' hash rather than the bytes lets a deletion drop the bytes and keep the
   leaf, and lets monitors replay the log from short records. Names follow L-10's alphabet, so
   the text has one parse.
3. **C2SP formats for what is signed.** Checkpoints are `tlog-checkpoint` notes, signed as
   `signed-note`s with Ed25519; the origin is the ledger's URL without the scheme, and the key is
   named after it, so a checkpoint counts only under its own name. Go's
   `golang.org/x/mod/sumdb/note` signs our test notes byte for byte the same. Off-the-shelf
   tooling can read our checkpoints.
4. **The signed-listing statement** is another signed note by the log key:
   `hushrun-listing/v1`, then `origin`, `size`, `root`, `prefix`, `after`, `next` and
   `keys <count> <sha256 of each key + \n>`, one labeled line each, in that order. It signs the
   hash of the keys, not the keys: the answer carries them, and a fixed-size statement is cheap to
   record. Its first line keeps it apart from checkpoints, which the same key signs.

**The log key in signed trust** (T-9):

5. `trust.yaml` gains `ledger_log: {origin, key}`. Signed trust carries it (T-8), so the root's
   signers, not the ledger, decide which key may sign checkpoints. The key must not be a
   principal's: it signs nothing a planner reads.
6. **Migration: a warning, not a refusal.** Every hosted ledger keeps a log from the day the
   server does, and none of their trusts names its key yet. Refusing would stop every pipeline
   until its admins re-sign trust. So without `ledger_log` the CLI checks nothing, says once per
   command that the ledger keeps a log trust doesn't name, and `doctor` prints the lines to add;
   with it, every check is mandatory, and a ledger that stops serving a log is `Unavailable`.
   A later release may make the key required for `https://` ledgers.

**What the CLI checks** (L-11), in `HttpLedger`, behind a `Log` trait on `Ledger` that other
backends don't implement:

7. **Receipts before success.** A create's 201 (and a retried create's 412) carries the leaf's
   index, a checkpoint and the inclusion proof. The create counts only when they verify; else
   `Unavailable`, exit 6: a write is either provably in the log, or not done.
8. **One history per command.** Every checkpoint the command meets is joined to the newest one
   it verified by a consistency proof, in whichever direction; a listing older than the newest
   is refused. A failure quotes both signed checkpoints, which together prove the fork.
9. **Checkpoints in events** (E-4): every event carries the newest checkpoint its writer
   verified. Readers check each one they read against their own, so a ledger that showed
   staging's writer and production's reader two histories can't produce the proof, and the plan
   exits 6 naming both. A checkpoint the log key didn't sign is skipped with a warning: it
   proves nothing either way.
10. **No quiet holes.** On a checked log a listed key the ledger doesn't serve fails the read,
    and a rejected event's bytes must be proven to be the log's (`log/inclusion?key=`).
    Otherwise a ledger could hide an event by answering 404, or by serving garbage that
    verification then drops.
11. **Recorded listings** (E-5): `plan --start` records, in each `started`, the statements of
    the listings its decision read for that scope, with their keys (up to 1000 keys a page and
    512 KiB in all), so a monitor can name what was left out. A plan that writes no `started`
    deploys nothing, so H1 doesn't need its listings.
12. **Deploys follow `plan --start` on HTTP ledgers.** `mark deployed` refuses an attempt
    without the `started` that `plan --start` wrote (one with `listings`), with exit 2: the
    recipes already do this, and it is what makes every deploy's listings checkable.

**The monitor** (`hushrun ledger monitor`, part of the CLI that customers run):

13. It replays the leaves into a tree, checks the latest checkpoint and the one `--state`
    remembered, then every recorded checkpoint and listing, deletion and anomaly. A listing that
    left out a key the log held is reported with the statement, the recorded keys and the hidden
    keys' leaves. It exits 0 when the ledger agrees with its log, 5 when it doesn't (exit 5 is
    "verification failed", which this is), 6 when it can't read, 2 without a log key in trust.
    Output `hushrun.ledger.monitor/v1`. A full replay each run is simple and fast enough for
    H1's logs; `--state` catches a ledger that rolls back between runs.
14. The index runs the same checks on its own database and shows them; that catches our bugs,
    not us. The monitor that counts is the customer's or an auditor's.

**Deletions** (L-9):

15. A deletion is a tombstone leaf, and an administrator signs each one: a GC statement (DSSE,
    `application/vnd.hushrun.gc+json`, `{ledger, key, time}`) by a key of any version of the
    root, sent in `X-Hushrun-Gc`. One envelope per key keeps each tombstone self-contained and
    the header small. GC refuses to run on a checked log without such a key; a delete without a
    statement is still accepted by the protocol, and every monitor reports it.

**The server** (contract `ledger-http-v1` §10): one log per ledger, sequenced in batches of at
most a second, with every complete node stored so that any proof takes O(log n) lookups, and a
create answering once its leaf is in a signed checkpoint, so L-7 holds. The per-ledger key is
sealed at rest with a server key. Everything is additive to `hushrun-ledger-http/1`.

## Consequences

- Readers that check the log cost a few more requests per command: a consistency proof per new
  checkpoint (usually one or two), and a proof per rejected event. Creates wait for their batch,
  up to about a second, inside L-4's 10 s.
- A ledger can still hide an event from one plan, but every deploy decided that way is found at
  the monitor's next pass, with proof. Decisions that deploy nothing write no `started`, so H1
  doesn't see them; they deploy less, not more.
- `trust.yaml` gains a key that hushrun versions before T-9 refuse (C-1): readers upgrade before
  admins add it.
- `started` events grow by their listings' statements (about 400 bytes each) and keys.
- `hushrun-ledger` depends on `hushrun-tlog`, and so on ring even without the `cloud` feature.
- `ledger gc` on a hosted ledger needs a root key as well as an admin token.
- H2 can build on the same leaves, statements and checkpoints: a map from listing prefixes to
  key trees, committed in each checkpoint, and witnesses' cosignatures (C2SP `tlog-witness`).

## Alternatives considered

- **Hashing the object's bytes into the leaf directly.** A deletion would then need the bytes to
  check its leaf, or would break the tree. Rejected for the bytes' hash.
- **C2SP tiles (`tlog-tiles`) instead of proof endpoints.** Cacheable and static, and what new
  logs adopt; but the hosted ledger already answers dynamic requests with tokens, and H1's
  clients need a handful of proofs. Tiles can come with H2 and witnesses.
- **Signing the listed keys rather than their hash.** The statement would grow with the page,
  and the page already carries the keys; the started event records them when it can.
- **Refusing ledgers whose trust lacks the log key at once.** Every hosted pipeline would stop on
  the server's upgrade day. Rejected for a warning and `doctor`'s lines (item 6).
- **The log key in `hushrun.yaml`, beside `ledger_root`.** The repository would choose who signs
  the ledger's checkpoints; signed trust is reviewed by the root's signers (T-8).
- **The server signing deletions on an admin token's behalf.** It would attribute deletions to
  the very party they guard against. Rejected for admins' own signatures (item 15).
- **One GC statement per run, listing every key.** Smaller in total, but too large for a header
  on big runs, and a tombstone would point at a shared object a ledger could withhold.
