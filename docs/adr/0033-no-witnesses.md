# 0033. No witnesses

- **Status:** accepted
- **Date:** 2026-10-27
- **Rules:** T-13 (removed); L-12, L-11, L-10, T-9, C-1, CP-2
- **Supersedes:** [0020](0020-proven-listings-and-witnesses.md)'s witnesses (items 5 to 8, the
  monitor's and `doctor`'s witness checks, and the migration's warning);
  [0021](0021-witnesses-that-verify-the-map.md) but its item 6 (the token command's URL);
  [0031](0031-membership-is-trust.md) item 12 (the server holds the witnesses it submits to)

## Context

ADR 0020 gave the hosted ledger's transparency log two things for H2: a prefix map that every
checkpoint commits to, with a proof on every listing of a map prefix, and witnesses that cosign
checkpoints (C2SP `tlog-cosignature` and `tlog-witness`), which `hushrun.yaml` named with a
quorum and a freshness bound (T-13). ADR 0021 added witnesses that also check the map before they
cosign, and ADR 0031 put the list the server submits to on the server, beside the one readers
require.

Witnesses were what let a plan refuse a short listing before it decides: a reader needed a recent
quorum of cosignatures on every checkpoint it read at, and checked every listing's proof against
the map. Without witnesses, readers kept L-11's checks and ignored proofs and cosignatures; that
was every repository `init` set up, since it never adds witnesses (ADR 0026, decision 5).

What it took to turn them on, and what they cost once on:

- **Setup.** Someone runs each witness where the server reaches it, an org admin sets them on
  the server, and a commit names them in `hushrun.yaml` with a quorum, which `doctor` compares.
- **Availability.** Witnesses join the deploy path (CP-2): with fewer than the quorum reachable
  and recent for longer than `max_age`, every plan and every write exits 6.
- **Surface.** A command with a server, a state file, a replica and a journal; four keys in
  `hushrun.yaml`; JSON formats for the state and the evidence; `log/leaves?size=`; an admin API;
  outbound requests from the server to URLs customers choose, with their own guard
  (`HUSHRUN_WITNESS_PRIVATE`); tables for witnesses, policies and cosignatures, and a refresh on
  reads; and reports in the monitor, `doctor` and the index.

The maintainers decided to remove them (2026-10-27).

## Decision

1. **Witnesses are gone**, from the CLI and from hushrun cloud. Nothing was released with them.
   - `hushrun witness` (`init`, `serve`, `--verify-map`, `--log-url`) is an unknown subcommand.
     Its formats (`hushrun.witness/v1`, `hushrun.witness.state/v1`,
     `hushrun.witness.evidence/v1`) and its journal go with it.
   - `transparency_log` names `origin` and `key` only (T-9): `witnesses`, `quorum`, `map_quorum`
     and `max_age` are unknown keys (C-1). T-13 keeps its number, marked removed.
   - Readers read no cosignatures: a checkpoint is checked against the log key and joined to the
     command's history, as L-11 says.
   - `GET {base}` no longer states `log.witnesses`, and `log/leaves` no longer takes `size=`,
     which only witnesses that check the map read.
   - `hushrun repository monitor` loses `witnesses`, `unwitnessed_checkpoint` and its exit 6 for a
     quorum that isn't recent; `doctor` loses its `witnesses` and `server witnesses` checks. Exit
     codes keep their numbers.
   - `hushrun-tlog` loses `cosignature`, `witness` and `replica`, and the testkit's fake ledger
     its witnesses.
   - hushrun cloud no longer submits checkpoints or keeps and refreshes cosignatures, and loses
     `PUT …/witnesses` and `HUSHRUN_WITNESS_PRIVATE`. A new migration drops its witness tables and
     `tlog_checkpoints.witnessed_at`: readers read at the latest checkpoint, and a write answers
     once a signed checkpoint holds its leaf.
2. **The transparency log stays whole**: the append-only log, signed checkpoints, receipts,
   signed listings, the signers answer's statement (L-13), the log key `hushrun.yaml` pins (T-9),
   one history per command and inside events (L-11), and the monitor.
3. **The map and listing proofs stay** (L-12): every batch still ends with a map leaf, a listing
   of a map prefix still carries its proof, and the monitor still checks every map leaf
   (`map_mismatch`) and warns of a latest checkpoint without one (`unmapped_checkpoint`).
4. **Readers check what L-11 says, and not listing proofs**, as ADR 0020's migration had readers
   without witnesses do. A proof shows only that a page agrees with the map the ledger committed
   to in the history it showed this reader. A ledger that means to deceive a plan commits to a map
   that leaves the event out, or shows the reader a history of its own, and either is found after
   the fact, by the monitor or when the histories meet, as a short listing is under L-11.

## Consequences

- **A short listing is found after the fact**, for every repository, as it already was for every
  repository without witnesses: by the monitor, with the ledger's own signed statement
  (`omitted_keys`), map leaf (`map_mismatch`) or checkpoints (`fork`) as proof. The monitor that
  counts runs from the customer's own CI (`recipes/github/monitor.yml`).
- **Two histories need no collusion**: a ledger can show a reader a history of its own until a
  checkpoint of it meets one of the other, in an event or in the monitor's `--state`.
- **A stale view has no bound**: a ledger can serve an older checkpoint of the same history to a
  command that never saw a newer one.
- **Fewer parts on the deploy path**: no third party that plans and writes wait for, and no
  requests from the server to URLs a customer chose.
- **The map keeps its cost** (a leaf per batch, a proof per listing of a map prefix) with no
  reader checking proofs. It changes no reader, and it keeps the way open for readers to check
  proofs later without changing the log.
- **History stays**: ADRs 0020 and 0021 keep their text, superseded in part, and hushrun cloud
  keeps its audit entries (`ledger.witnesses_changed`) while it drops what the witnesses held.

## Alternatives considered

- **Keep witnesses, off by default.** They were off by default; the command, the keys, the
  server's submissions and their tests stayed for every repository all the same.
- **Have readers check listing proofs anyway.** A plan would refuse a page its ledger's own map
  contradicts, so a ledger that hides an event would also have to commit to a map that lies, or
  fork, which a monitor finds whether or not the deceived plan recorded its listing. But it is
  still found afterwards, and every plan would fail whenever the server can't make a proof (it
  leaves the proof out rather than fail the listing). Left for a later decision: the map and the
  proofs stay, so it would be a change to readers only.
- **Drop the map and the proofs too.** They cost little and no reader depends on them; dropping
  them would change the log's leaves, which every monitor parses.
- **Keep `log/leaves?size=`.** Only witnesses that check the map read a tree past the latest
  checkpoint, while the ledger waited for them.
