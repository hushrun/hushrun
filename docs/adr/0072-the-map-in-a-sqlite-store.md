# 0072. The map in a SQLite store, one per repository

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** C-33, C-36; relies on C-11, C-30, C-31, C-34, ADR 0044, 0060, 0061, 0067, 0068, 0069,
  0070, 0071

## Context

ADR 0068 kept a check's map and its audit as one JSON file each, `<cache dir>/units/<check
id>.map.json` and `.audit.json`, read, changed and written whole under a lock. RFC 0003 (§3.10,
decision D4) chose one file per check until `hushrun why` needed queries, and SQLite then. `hushrun
why` (ADR 0073) needs more than a map file held:

- what each file a test file read was when it last ran here, so that "it read X, which changed" can
  name X: the map held paths only;
- the direct edges an adapter saw (a test file imports a module, a module opens a file), to show a
  path from the test file to what changed, across every unit of a check;
- the checks of a repository by their name, with their command, cwd and scope, since `hushrun why
  unit tests/test_cart.py` names no command.

The map is a hint: it chooses which files a key hashes, and a wrong one makes a key no run
recorded, which runs the test file (RFC 0003 §3.4). Nothing about where it lives may change that.

## Decision

1. **What moves, and what stays a file.** The map, the audit's disagreements and the edges go into
   one SQLite file per repository, `<cache dir>/units/<id>.v1.sqlite`, the id the SHA-256 of the
   JCS form of `{"schema": "hushrun.unit-store-id/v1", "repository"}` (the repository as the run
   names it, C-10, lowercase, `""` for none). They explain and hint; none decides. What decides
   stays a file, as before:
   - **unit results**, under `results/` beside this machine's run results, swept with them (ADR
     0067, which rejected SQLite for results: the ledger's directory creates each object
     atomically, and one store for results and another for everything else would be two answers
     to one question);
   - **the local reuse state**, `units/reuse/<id>.json`, and **the outbox**, `units/outbox/`. A store
     that can't be read is replaced (item 6); if a suspension lived there, replacing it would turn
     reuse back on, which is a wrong reuse waiting to happen. As a file that doesn't read, the reuse
     state reads as suspended, and the outbox only ever adds suspensions.

   The map files go: nothing writes or reads them, and the 30-day sweep removes those an
   unreleased hushrun left. No hushrun was released with them, so nothing migrates from them.
2. **The schema** (C-36), `STRICT` tables with foreign keys and `CHECK`s, so that a row of another
   type or an impossible combination isn't written: `path`; `version` (a file's kind and content
   hash, as a source manifest writes them, D-4); `checks` (one row per check id, with its name,
   command as a JSON array, cwd, scope, adapter, and when a run last wrote it); `check_rest`,
   `check_source`, `check_listed` (the check's last run's rest set, sources, and Vitest's list);
   `unit` (each test file that ran here), `unit_read` (what its key holds, each file as it was
   then), `unit_not_followed`, `unit_dir`; `edge` (`import`, `open` or `read`, per check); and
   `disagreement` (the newest 200 of a check). It departs from RFC 0003's Appendix C sketch in what
   `hushrun why` showed it needs: file versions per test file, edges per check rather than global
   (two checks' adapters see different graphs), and no `observed_at` per edge.
3. **The schema's version** is in the file's name (`v1`) and in `PRAGMA user_version`, with
   `PRAGMA application_id` hushrun's (`hush`). A hushrun with another schema keeps another file, so
   two versions on one machine never fight over one. A file whose ids aren't these is not a store
   this hushrun can read: it reads as no map (item 6).
4. **Rows are keyed by the check id** ADR 0068 and 0070 defined (the run key's fields but `digest`,
   `external` and `counts_from`, with the repository), as the map files were. It already holds the
   repository, which also chooses the file: one function makes each, and a check's rows can't land
   in another repository's file.
5. **Writes.** A run that recorded writes once, in one `BEGIN IMMEDIATE` transaction, after a busy
   wait of at most 5 s: each test file that ran replaces its rows; one the map has whose file left
   the scope goes (for a Go package, named by its directory, when no file is below it); the check's
   sets are this run's; the edges from every file a test file that ran read are this run's, since a
   module's imports are seen whole when it loads, and the others stay; disagreements join the
   audit. Then it checkpoints the WAL into the file when no reader holds it. Creating the file
   takes a lock file beside it, so two hushrun making it at once make it once. WAL lets readers go
   on while one writes. A write that can't happen (busy, a disk that
   refuses) is dropped, logged, and costs the next run its reuse, nothing else.
6. **What can't be read is never trusted.** A store missing, busy past the wait, broken (SQLite
   says it isn't a database, or damaged), or of another schema reads as an empty map: every test
   file runs, and the run says it once, `hushrun: <NAME>: every test file runs: this machine's
   record of what its test files read can't be read (<why>), so the next run here starts it again`
   (in `observe`, as what `tests: reuse` would have done). The run that writes next removes it and
   its WAL files and makes it again. Nothing about a store ever fails a run.
7. **Readers that must write nothing** (`hushrun why`, `hushrun ready`, ADR 0061) open it read-write
   without creating it and `PRAGMA query_only`: a read-only connection would leave WAL's files
   behind, and this one removes them as it closes. A store being made right now (no ids yet) reads
   as none.
8. **Pruning** (ADR 0067). Each write deletes the checks no run wrote for 30 days, by the run's
   clock, with all their rows, and the versions and paths no row needs any more. The cache's sweep
   leaves this schema's files alone and removes any other file of `units/` no run touched for 30
   days, a store of another schema included.
9. **CI's own cache** (RFC 0003 D3). `hushrun init` writes no cache step for the map today, and
   still doesn't: a job that wants the map between runs caches `<cache dir>/units/*.v1.sqlite`
   after hushrun exits, its last write checkpointed into that one file (item 5); a `-wal` left by
   runs at once holds only what a later run would write again. Before,
   it was `*.map.json`. A missing store costs one full run, as a missing map did.
10. **The dependency** is `rusqlite` 0.41.1 with `bundled` (MIT), which builds SQLite's
    amalgamation (public domain) with the C compiler `cc` finds, as `ring` already does on every
    release target in `dist-workspace.toml` (macOS on both architectures, Linux musl on both,
    Windows MSVC on both): no system library, and the same SQLite everywhere. Default features are
    off (no statement cache, no wasm). `cargo deny` passes (MIT; SQLite's own notice is its
    dedication to the public domain), and `cargo xtask notices` lists rusqlite and libsqlite3-sys.
    It is the third C code in the binary after ring and mimalloc, which `deny.toml`'s note asked an
    ADR for: this is it. On a Mac (aarch64, release profile, fat LTO, symbols stripped) the binary
    went from 16,433,680 to 18,295,408 bytes (+1.86 MB, 11%), most of it SQLite with its JSON
    functions, which the schema's `CHECK`s use; the rest is `hushrun why`. The release targets
    were not built here: ring's C builds on all of them, and SQLite's amalgamation needs nothing
    more than a C compiler for the target.

## Consequences

- `hushrun why` can say which file changed and the path to it, from this machine alone.
- One file per repository holds every check's map: two repositories on one machine keep apart as
  before, and a check's rows are replaced in one transaction instead of a file rewritten under a lock.
- The binary has SQLite in it, and builds need a C compiler on every target, as they already did.
- A damaged store costs one full run and one line; a store of another hushrun version costs that
  version nothing.

## Alternatives considered

- **Keep the map files, and a SQLite file for what `why` needs:** two copies of what a test file
  read, written by two code paths that can disagree. One must be derived from the other, and the
  store can hold the map whole.
- **The reuse state and the outbox in the store too:** one file fewer, but a store that can't be read
  is replaced, and a replaced suspension is reuse turned back on.
- **One store per machine, rows keyed by repository:** a damaged file would cost every repository
  its reuse, and a repository's rows would be in a file whose name says nothing of it.
- **A pure-Rust embedded database (redb, sled) or a graph database:** the RFC's research (§3.10)
  found nothing that needs a graph engine, and SQLite's format, tools and durability are known; a
  person can open the store with the `sqlite3` shell to see what hushrun saw.
- **`rusqlite` with the system SQLite:** versions differ by platform and Windows has none.
