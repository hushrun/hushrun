# 0020. Proven listings and witnesses: the hosted ledger's log, H2

- **Status:** accepted; superseded in part by [0033](0033-no-witnesses.md): its witnesses (items 5 to 8) are gone, and the map and listing proofs stand
- **Date:** 2026-10-16
- **Rules:** L-12 (new), T-13 (new), L-11, T-9, L-2, L-8, E-4, E-5, P-7, CP-1, CP-2, CP-3

## Context

RFC 0001 §4.7 makes the hosted ledger an append-only Merkle log in two phases. ADR 0017 built
H1: every create is proven to be in the log, every listing comes with the ledger's signed
statement, and a monitor replays the log and finds, after the fact and with the ledger's own
signature as proof, every deploy decided from a listing that left keys out. Two gaps remain,
and the RFC closes them in H2:

- **The decision still happens.** H1 finds a short listing at the monitor's next pass, after
  production deployed what it shouldn't have. Decisions that write no `started` (`skip`, `wait`,
  `blocked`) aren't recorded at all.
- **One ledger, two histories.** H1 catches a split view only when a reader meets a checkpoint
  of the other history inside an event. A ledger that keeps the two audiences apart, or shows a
  reader an older tree, isn't caught.

H2, this ADR: "every checkpoint also commits to a map from each listing prefix to a Merkle tree
of its sorted keys, so a whole listing, or a first page of one, can be proven"; "monitors check,
at every checkpoint, that the map follows from the log"; and "independent witnesses cosign
checkpoints (C2SP `tlog-witness`), and the signed trust names the witnesses and how many must
cosign". The control plane rules hold: nothing new goes into the planner (CP-1), the ledger is
still the only part on the deploy path (CP-2), and witnesses are trusted because signed trust
names them, never because the ledger does (CP-3).

## Decision

**The map is committed inside the tree** (L-12), all in `hushrun-tlog`:

1. **A map leaf ends every batch.** Its entry is `hushrun-tlog-entry/v1`, `map`, and the map's
   root in hex: the map of every leaf before it. So a checkpoint's last leaf commits to the map
   at that checkpoint, every map the ledger ever committed to is in its log, and witnesses,
   which cosign trees, cosign maps with them. The empty log has no map leaf, and an empty map.
2. **Map prefixes are the planner's listings**: `v1/events/<scope>/<digest>/` and
   `v1/deployed/<scope>/<stage>/`, a key's first four segments. Each one's live keys, in byte
   order, are an RFC 6962 tree with leaf hashes `SHA-256(0x00 || key)`. A whole listing is
   proven by its keys; a page by a range proof: the complete subtrees before and after it, plus
   the key just before it, which must be at most `after` (nothing skipped), and the tree's end
   when the page says nothing follows (nothing cut).
3. **The map is a compacted sparse Merkle tree** over `SHA-256(prefix)`: an empty subtree is 32
   zero bytes, a subtree of one entry is that entry's hash, any other is RFC 6962's node hash.
   An entry is `hushrun-map-entry/v1`, the prefix, its key count and its key tree's root. The
   shape depends only on the entries, so the ledger, monitors and the reference compute the
   same root; an update costs a path, about log₂ of the number of prefixes; and a proof is that
   path, which shows a prefix's entry or that it has none (an empty subtree, or another entry
   where it would be).
4. **A listing proof** is the map's root, the map leaf's inclusion proof in the checkpoint's
   tree, the map's path to the prefix, and the page's range proof. `HttpLedger` checks it before
   it returns keys; the `Ledger` trait doesn't change.

**Witnesses** (L-12, T-13):

5. **C2SP formats, unchanged.** Witnesses cosign as `tlog-cosignature` says (timestamped
   Ed25519, signature type `0x04`), and the ledger submits each checkpoint with `tlog-witness`'s
   `add-checkpoint`: the size the witness last cosigned, the consistency proof from it, and the
   checkpoint. Any C2SP witness works; `hushrun witness serve` is one a team runs itself.
6. **Signed trust names them** (T-13): each witness's C2SP key and the URL the ledger posts to,
   `quorum`, and `max_age` (5 minutes by default). A witness can't hold the log key or a
   principal's key.
7. **Readers need a recent quorum.** Every checkpoint a command reads at (receipts, listings,
   the latest) needs cosignatures by `quorum` witnesses, each at most `max_age` old and at most a
   minute ahead of the reader's clock; otherwise exit 6. Checkpoints inside events keep L-11's
   check (a consistency proof): they are old by nature.
8. **Writes wait for their witnesses.** The ledger answers a write once its checkpoint has been
   submitted, so a writer's receipt is cosigned; readers read at the latest checkpoint whose
   witnesses had their turn; and the ledger submits the latest checkpoint again when its
   cosignatures pass half of `max_age`, at most every two seconds. A witness that fails is
   recorded; readers count the others.

What this buys, precisely:

- **An incomplete listing is refused before the decision.** A page that leaves out a key the
  committed map holds doesn't verify, whatever the ledger signs. To make a reader accept it, the
  ledger must commit to a map that leaves the key out: a map leaf in its own log, cosigned with
  the tree, that every monitor checks at every map leaf (`map_mismatch`, exit 5). Unlike H1's
  per-request statements, that lie is one global commitment, the same for every reader, found
  whether or not the deceived plan wrote anything. A ledger can't keep monitors before it
  either: a monitor needs a recent quorum on the checkpoint it replays to, as plans do, or it
  exits 6, so within `max_age` it is shown a tree that holds the lie.
- **Two histories need the witnesses to collude.** An honest witness cosigns one history, so two
  checkpoints that a quorum each cosigned share a witness that must have lied when any two
  quorums intersect (`quorum` above half the witnesses).
- **A stale view is bounded by `max_age`.** An honest witness never cosigns a tree older than one
  it cosigned, and a writer's create returns only with its witnesses' cosignatures, so a key
  written more than `max_age` before a plan reads is in its listing unless witnesses collude.

**Monitors** check that every map leaf records the map of the leaves before it, and report the
latest checkpoint's cosignatures (`unwitnessed_checkpoint`). While the witnesses are behind they
still check the log (`Log::latest_signed`), but exit 6 and keep their `--state`: a view without a
recent quorum may not be the ledger's current one. The index runs the same checks on its own
database. `doctor` warns when `quorum` is half the witnesses or fewer, since two quorums then
need not share a witness.

**Migration**, as T-9 did it: without witnesses in signed trust, readers
keep H1's checks, ignore proofs and cosignatures, and warn once per command; `doctor` says how to
add them. Map leaves are written for every hosted ledger regardless, so a ledger can turn
witnesses on with a trust upload. A hushrun from before T-13 refuses `witnesses` (C-1), and its
monitor stops at the first map leaf (an unknown kind, exit 6): upgrade readers and monitors
first.

## Consequences

- **What stays after the fact.** Readers can't check that the map follows from the log: that
  needs the whole log. A ledger that commits to a lying map deceives readers until a monitor's
  next pass. It is now a single, public, witnessed act, provable at any later time, but the
  monitor that counts must still run: hourly from the customer's CI (`recipes/github`), or on a
  witness's host.
- **What witnesses can do together.** A quorum of colluding witnesses lets the ledger show a
  reader another history, or an older one. A forked view is still caught by a monitor that sees
  the listing it recorded (H1); an older view is consistent with the log and, beyond clocks,
  isn't. Choose witnesses run by parties with different interests: the customer's own, a
  partner's, a public witness network.
- **Availability.** Witnesses join the deploy path (CP-2): with fewer than `quorum` reachable for
  longer than `max_age`, plans exit 6. `quorum` below the number of witnesses tolerates some of
  them being down.
- **Costs.** Each batch adds one leaf and, with witnesses, a round trip to each (in parallel,
  5 s at most), inside L-4's 10 s. A listing of a map prefix carries a proof of about
  log₂(prefixes) + 2·log₂(keys) hashes. The server keeps the map's history in rows, one per
  change of a prefix; the newest map stays in memory.
- **Formats are contracts:** the map entry and map leaf texts, the proof's JSON
  (ledger-http-v1 §11), trust's `witnesses`, `quorum` and `max_age`, the monitor's new kinds, and
  `hushrun witness`'s state file (`hushrun.witness.state/v1`).

## Alternatives considered

- **The map root as a checkpoint extension line.** C2SP checkpoints allow them and cosignatures
  cover them, but witnesses make "no statement about them": a witness would cosign the same tree
  with two different maps, one per reader. Rejected for a leaf, which the tree's root covers.
- **A sorted list of prefixes under an RFC 6962 tree.** It would reuse one hash scheme for
  everything, but inserting a prefix shifts every leaf after it, so each batch would rehash the
  whole map, and a monitor would rehash it at every map leaf. Rejected for the sparse tree.
- **Witnesses that also check the map.** Their cosignature would then mean the map follows from
  the log, and a lying map would be refused before the decision too. But C2SP witnesses check
  consistency only; one that checks maps needs read access to the ledger and a state as large as
  the map. Left for later: `hushrun witness serve` could grow a `--verify-map` mode.
- **Readers asking witnesses directly for their latest checkpoint** (C2SP's monitoring
  endpoint, which `hushrun witness serve` has). Exact freshness, but every plan would depend on
  reaching every witness from CI. Rejected for timestamps and `max_age`, which C2SP's
  cosignatures carry for this.
- **Requiring witnesses on every `https://` ledger at once.** Every hosted pipeline would stop on
  the upgrade until its admins re-signed trust. Rejected for a warning, as T-9 did.
- **C2SP tiles instead of proof endpoints**, as ADR 0017 noted. Still possible: the map and the
  witnesses don't depend on how proofs travel.
