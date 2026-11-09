# 0021. Witnesses that verify the map

- **Status:** accepted; superseded in part by [0033](0033-no-witnesses.md): witnesses that verify the map are gone, and the token command's URL (item 6) stands
- **Date:** 2026-10-16
- **Rules:** L-12, T-13, L-11, L-10, L-4, CP-2, CP-3

## Context

ADR 0020 made the hosted ledger commit, in every checkpoint, to a map of each listing prefix's
keys, and had witnesses cosign checkpoints, so that a listing that leaves a key out is refused
before the decision. One gap stayed after the fact ("What stays after the fact"): readers can't
check that the map follows from the log, which needs the whole log. A ledger can commit to a map
leaf whose map leaves a key out, prove listings against it, and deceive every plan until a
monitor's next pass finds the map leaf (`map_mismatch`). The lie is one public, witnessed act,
but it is found afterwards, and a C2SP witness, which checks consistency only, cosigns it.

ADR 0020 left the fix for later: a witness that replays the log and checks the map before it
cosigns. Then its cosignature also means "this checkpoint's map follows from its leaves", and
readers whose trust counts such cosignatures refuse the lying map before any plan reads it. It
needs what C2SP witnesses don't have: read access to the ledger, a state as large as the map,
and a way to read the leaves of a checkpoint the ledger is still waiting on its witnesses for.

## Decision

**An opt-in mode of `hushrun witness serve`** (L-12): with `--verify-map`, the witness keeps a
replica of each log it cosigns, and cosigns a checkpoint only when, on top of C2SP's checks:

1. **Its leaves are the checkpoint's.** The witness replays the leaves from its replica's size to
   the checkpoint's, and they must hash to the checkpoint's root.
2. **Its last leaf is a map leaf** (any tree but the empty one): the map readers prove listings
   against.
3. **That map follows from the log.** The root the map leaf records is the map of every leaf
   before it, as `MapBuilder` computes it from the live keys of every map prefix.

It checks the map of each checkpoint it is asked to cosign, not every map leaf between two of
them: a reader accepts only checkpoints that witnesses which verify maps cosigned, and each of
those checked the map of that checkpoint. In steady state the ledger submits every batch's
checkpoint, so that is every map leaf; while catching up it is the last one, which keeps a
replay linear in the log instead of rebuilding a busy prefix's key tree at every batch. The
monitor still checks every map leaf (L-12), and the rest of what L-11 checks.

**How it reads the leaves.** From the ledger, as a monitor does, with a reader token:

4. **`log/leaves?size=`** (L-11): the leaves of the tree of `size` leaves, which may be the newest
   signed checkpoint even before its witnesses had their turn, which is exactly the one the
   ledger submits. The ledger answers it at once, without waiting for its own sequencing, since
   the batch's writers are waiting for the witness. Without `size`, `log/leaves` reads as far as
   the checkpoint readers read at, as before.
5. **The ledger** is `https://<origin>` (T-9: the origin is the ledger's own URL without its
   scheme), or `--ledger URL` for the log whose origin it is, such as `http://` on a loopback
   host.
6. **The token** is the CLI's (L-10): `HUSHRUN_LEDGER_TOKEN` when the witness verifies one log;
   `HUSHRUN_LEDGER_TOKEN_COMMAND` otherwise, which now runs with `HUSHRUN_LEDGER_URL` set to the
   ledger's URL, as `HUSHRUN_ORG_TOKEN_COMMAND` does (O-7), so one command can print each
   ledger's token: the hosted ledger's tokens belong to one ledger each. A static token with
   several logs is refused, since it would go to ledgers it doesn't belong to.

**What it keeps** (next to `--state`), so that a restart loses nothing it answered for:

7. **In the state file**, per log, atomically with the C2SP record (`hushrun.witness.state/v1`,
   additive): the replica's size, the tree's compact range there (one hash per bit of the size),
   the signed checkpoint it was checked against, the root the tree's last leaf records when it
   is a map leaf, the map's root, how many keys are live, and the journal's generation and
   length.
8. **A journal per log**: every key under a map prefix that goes live (`+key`) or goes (`-key`),
   in log order. Records are appended, then synced, then the state file records the new length,
   so a crash leaves a tail that the next start truncates. On start the journal is replayed
   into memory, and the map it gives must have the recorded root, or the replica is damaged: it
   is dropped and the log replayed again from the ledger, never trusted. When the journal holds
   more records than twice its live keys (and at least 100,000), it is rewritten as the live
   keys, in a new generation that the state file then names.
9. **In memory**: the replica, the live keys of every map prefix. Progress that a checkpoint
   hasn't verified yet stays in memory between requests and on disk past the recorded length,
   so a replay interrupted by the network resumes; leaves that turn out not to hash to the
   checkpoint send the replica back to what the state file records.

**What it answers** (C2SP's statuses keep their meaning):

10. **200** with its cosignature when the map follows.
11. **422** when the map doesn't follow, or when the ledger signed two trees of one size: that is
    evidence, `hushrun.witness.evidence/v1`, with the signed checkpoint, the map leaf's index,
    the root it records and the root the leaves give. The witness prints it, keeps it in its
    state, and cosigns nothing more of that log, refreshes included: every later tree holds the
    lie. Also 422, without evidence, for a checkpoint whose last leaf isn't a map leaf, or one
    older than a tree the witness already replayed: it can't vouch for those, and the ledger
    submits newer ones.
12. **503** when it can't check now: the leaves are unreachable or refused, they don't hash to
    the checkpoint, or another request is replaying that log (answered at once, so submissions
    don't pile up). The replay of a long catch-up goes on after the ledger stops waiting, and
    records what it cosigned; the ledger's next submission meets C2SP's 409 and continues from
    there.

**Trust says which witnesses verify** (T-13):

13. **`verify_map: true`** on a witness: it runs with `--verify-map`. Its cosignature looks like
    any other, so trust is the only place that says it means more. A witness that once verified
    maps refuses to start without `--verify-map`, so that turning it off is a decision made in
    trust first.
14. **`map_quorum`**: how many of the counted cosignatures must be by witnesses with
    `verify_map`: 0 to the smaller of `quorum` and their number, 1 by default when one is named.
    `quorum` still counts every witness: it protects one history, which needs any two quorums to
    share an honest witness; `map_quorum` protects the map, which needs one honest witness that
    verifies it. They are different witnesses on purpose: a public witness network checks
    consistency for anyone, but only a witness the customer gave read access can check maps.
15. **Readers** (L-12): every checkpoint a command reads at needs `quorum` recent cosignatures, of
    which `map_quorum` by witnesses that verify the map; otherwise exit 6, naming them. The
    ledger refreshes cosignatures when either count of recent ones runs short, so a verifying
    witness that missed a submission is asked again within seconds. `doctor` and the monitor
    report both counts.

What this buys: a lying map is refused before the decision unless `map_quorum` witnesses that
verify maps collude with the ledger. The monitor's `map_mismatch` stays, for intermediate map
leaves and for readers without such witnesses.

**Performance** for big logs, measured on a laptop (release build, one core, a million live
keys of realistic shape, a quarter of them deployed markers in four stages):

- **Steady state:** per checkpoint, one `log/leaves` request per 1000 leaves of the batch, a hash
  per leaf, the key trees of the prefixes the batch touched rebuilt, a journal append and sync,
  and the state file rewritten (a few kilobytes per log): the work the ledger did for the batch.
  A batch that touches a prefix of 50,000 keys takes about 8 ms; of 200,000, about 32 ms, since a
  key tree in byte order is rebuilt whole when a key goes in at its front, as deployed markers
  do.
- **Catching up** (a new witness, or one back from downtime): every leaf since the replica's
  size, 1000 per request, replayed at about a million leaves a second, then one rebuild of each
  touched prefix's key tree (under a second for a million keys). The ledger's answers bound it:
  at 20 to 50 ms a request, 20,000 to 50,000 leaves a second. Rebuilding at every map leaf
  instead would cost the 8 ms above for each of the log's batches: hours for a big log.
  Submissions meanwhile get 503 at once; witnesses with `verify_map` should join trust once
  they have cosigned.
- **Memory and disk:** about 375 bytes of memory and 116 bytes of journal per live key under a
  map prefix (a million keys: under 400 MB and 120 MB). A restart reads the journal and rebuilds
  the map in under 2 s per million keys.

## Consequences

- **The gap closes for readers that count verifying witnesses.** Trust without `verify_map`
  keeps ADR 0020's behaviour exactly.
- **Availability.** Verifying witnesses join the deploy path like the others (CP-2): with fewer
  than `map_quorum` of them reachable, caught up and recent for longer than `max_age`, plans and
  writes exit 6. Name more verifying witnesses than `map_quorum` to tolerate one being down.
- **Read access.** A verifying witness holds a reader token of each ledger it verifies. The
  leaves carry keys and hashes only, but a reader token also reads objects, so a witness run by
  someone else sees the ledger's events. A role limited to `log/` would fit it better: not
  added here.
- **A witness that loses its replica** replays the log again, which takes as long as the log is
  big; it answers 503 meanwhile. Unverified progress of a catch-up is lost if the process stops
  before a checkpoint verifies it.
- **Formats are contracts:** `log/leaves?size=`, trust's `verify_map` and `map_quorum`,
  `HUSHRUN_LEDGER_URL` in the token command's environment, the state file's `map` records, the
  journal's lines, and `hushrun.witness.evidence/v1`. A hushrun from before this refuses
  `verify_map` and `map_quorum` (C-1): upgrade readers first, then name verifying witnesses.

## Alternatives considered

- **The ledger pushes the leaves with each checkpoint.** No token, and no request back to a
  ledger that is waiting. But C2SP's `add-checkpoint` has a fixed body of at most 64 KiB, so it
  would need an endpoint of our own on every verifying witness, a batch's leaves can be many
  megabytes, and a new witness must read the whole log anyway. Rejected for reading, which also
  serves catching up.
- **Checking every map leaf, as the monitor does.** It would also find a lie in a checkpoint the
  witness was never asked about, but a catch-up would rebuild the key trees of the busiest
  prefixes at every batch, quadratic in the deploys of a stage. Nothing a reader accepts depends
  on those map leaves; the monitor checks them.
- **Cosigning consistency anyway after a lie, or after `--verify-map` is turned off.** A
  cosignature doesn't say what was checked, so readers whose trust names the witness as
  verifying would count it. Rejected: a witness that caught its log lying, or that verified
  maps, keeps meaning what trust says it means.
- **`quorum` counting only verifying witnesses.** Simpler, but every witness would need read
  access to the ledger, which rules out public witness networks, and one history needs more
  witnesses than one true map does. Rejected for two counts.
- **A snapshot of the keys rewritten at every checkpoint.** Simple, but each batch would write
  every live key. Rejected for the journal, rewritten only when mostly superseded.
- **An embedded database for the replica.** It would bring storage code the CLI doesn't have,
  for a set of strings and a few hashes. Rejected for the journal.
- **Anchoring a long catch-up's progress** with consistency proofs to the ledger's latest
  witnessed checkpoint, so a stopped witness resumes it. Possible later; a first catch-up is a
  one-off, and it is safe to start over.
