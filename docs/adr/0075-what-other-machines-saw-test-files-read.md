# 0075. What other machines saw a check's test files read

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** C-12, C-15, C-22, C-33, C-34, C-35, C-36, C-37, C-38; relies on C-9, C-10, C-25, C-30,
  C-31, ADR 0045, 0046, 0061, 0067, 0068, 0069, 0070, 0071, 0072, 0073, 0074

## Context

Since ADR 0069 a run sends each test file's result to the server under its key, and any machine
can ask for it. But a machine can compute a key only when it knows which files the test file read,
and that list, the map, lives in one machine's store (ADR 0072). A CI runner or a coding agent's
sandbox starts with an empty cache, so it has no map: it runs every pytest and Vitest test file,
and every Go package whose tests opened a file their key holds beyond what the package is built
from. That is measured in
`crates/hushrun/tests/two_machines.rs`. Cargo needs no map: its keys come from the build.

RFC 0003 (D3) chose CI's own cache to carry the map, and the server only if that wasn't enough.
It isn't: a sandbox has no cache between runs, GitHub scopes a cache by branch so a pull request
never reads a sibling's, and a cache is one more step in every workflow. The owner asked on
2026-11-11 for a fresh machine to reuse what a warm one would, from the backend. The plan's
section 5 records the decisions this ADR builds on (S1 to S6).

A map is a hint: a wrong list makes a key no run recorded, which runs the test file (RFC 0003
§3.4). So a list another machine sends can't cause a wrong reuse either, and needs no signature.
What it must not do is say what a private repository holds: no file path goes to a server (RFC
0003 principle 7). Phase 1 made the hint's bytes (C-38's key, tokens, address and content) and
phase 2 the server's two requests (results-v0, Hints). This decision is the CLI's part: asking,
using and sending.

## Decision

1. **The server keeps the map too, as hints, and CI's cache still can** (S1, the owner's,
   2026-11-11: both). A hint is what one machine's store says a check's test files read, every path
   a 16-byte keyed hash under a key derived from the names of the check's sources, which the
   server never has (C-38, S2: derived). A store CI's cache carries is read first, as before.
2. **When a run asks.** Only where hints can change something: a run whose check's own key
   missed, with `tests` `observe` or `reuse`, whose results go to a server (never this machine's
   directory, ADR 0067), whose adapter isn't Cargo's, on Linux, macOS or Windows. `hushrun ready`
   and `hushrun why` ask as a run would, read-only, and note nothing (ADR 0061). One `POST
   {base}/v0/units/hints/lookup` per system whose hints it reads.
3. **Whose.** This machine's own system's (S3, the owner's: its own OS's). A test file that reads
   `_darwin.py` on a Mac and `_linux.py` on Linux has two true lists; keying it on Linux by the
   Mac's would let a change to `_linux.py` leave the key as it was. So a machine never reads one
   pool of several: with no rule, and with a `counts_from` (C-25) that names its own system, among
   others or alone, it reads its own system's. Only where the rule names `os` and this machine's
   system isn't among them does it read the systems it names, one lookup each: only those leave
   results for the check, and a pass under a key skips on any machine (ADR 0045). A machine always
   writes under its own system.
4. **Where.** A hint's address and key, for pytest and Go, come from the sources set, the files
   `shape` hashes (C-31). For Vitest, whose list of test files is in the hint (ADR 0071), from
   every file of a source kind with no test file named: a new test file then gives a new address,
   and everything runs once, as Vitest's adapter does here. The plan then classifies with the map's
   list of test files, else the newest hint's.
5. **One function gives the lists**, and the plan, `hushrun ready`, `observe`'s "would have run"
   and `hushrun why` all call it (`units::shared::lists`): this machine's own entry when it has one
   (one `not observed` runs, whatever the hints say; a `shared` one only when its system is one
   this run reads, item 7), or for Go with none, what its test binary is
   built from now; then each hint's list, newest first; the same list once, and 3 at most. Then, as
   before, a key for each list from the files on disk now, and one batch lookup, in pages of 5,000
   keys.
6. **One rule decides** (`units::shared::choose`): a test file may be reused when some list's key
   has a newest result that counts here and is a pass, **and no list's key has one that is a
   fail**. Two runs that saw it read different files and disagree is a reason to run. The audit
   share, `tests.always`, `not observed`, suspension and the 5,000 test files stay as ADR 0069 has
   them.
7. **Kept here, marked with its system.** A test file reused on a hint's list is kept in this
   machine's store as that list, `shared`, in a column whose `CHECK` takes `seen here` or `shared`,
   with the system the hint was asked for in a second column a `CHECK` ties to the first: one for
   `shared`, none for `seen here` (C-36). The next run of it here replaces it with what it saw. The
   system is part of the state because the check id holds no `counts_from`: a Mac whose rule named
   `linux` kept Linux's list, and without its system that list would be the Mac's first once the
   rule went, its key could hit, and the Mac would send it as macOS's. So a `shared` entry counts
   for a key only while its system is one the run reads, and a hint this machine writes holds a
   reused test file's list only when the list was seen here or came from its own system. So this machine's next hint carries it on, and `hushrun
   why` says which it was: `it passed on the files another machine saw it read`, with `another
   machine saw it read <files>`. The store never shipped in a release, so its schema changed in
   place.
8. **Sending.** After a run kept its map (only when its files didn't change while COMMAND ran) and
   a server recorded its result with its test files' results, in `observe` too, which is what lets
   CI say what reuse would have done before anyone turns it on: the hint, from the store, of every
   test file the run executed and fully observed and every one it reused, with the list whose key
   hit, under this machine's system. One `PUT {base}/v0/units/hints/{sha256}`, and none when this
   machine already sent or received a hint with its content id for the address and system. Never
   in local mode, never without a credential, never for a run that recorded nothing, such as one
   that doesn't meet `counts_from`.
9. **The record of what was sent.** The store notes the content id of each hint it sent or
   received, by address and system, the newest 200 of a check: one a run read is noted with the
   run's map, one sent once the server kept it. A `PUT` that fails notes nothing, so the next run of
   the check builds the hint again and sends it, from that note and nothing else: no outbox. A hint
   the writer calls too large isn't sent, said once, `hushrun: <NAME>: what its test files read
   wasn't shared with other machines: it is too large (<n> <what>, more than <most>)`. Sending never
   fails a run: what goes wrong is logged for debug, as the store's errors are.
10. **What converges where.** A server that doesn't answer for hints: this machine's map alone, as
    before. One from before hints (404 or 405): the same, nothing more is asked of it that run, and
    after the run recorded, once, `hushrun: <NAME>: <org> doesn't keep what other machines saw its
    test files read yet, so this machine goes by what it saw itself`, not when it keeps no test
    files' results either, which ADR 0069 already says once. A hint that can't be read is left out,
    and the others used. A wrong, stale or hostile hint, or one written under another key, makes
    keys nothing matches, or tokens no file matches, and those test files run.
11. **What doesn't change** (the plan's §2.7): the key; whose pass counts, by the lookup's filter
    and the CLI's own check; audit runs and suspension; no reuse where a later step reads reports
    (C-26); nothing leaves the machine in local mode; Cargo builds first.
12. **One definition** of the 5,000 test files a run records, looks up and a hint holds:
    `hushrun_core::units::MOST_UNITS`, which `hints` re-exports.
13. **CI's cache step, where the server keeps no hints** (S1's other half, the plan's §2.8). When
    a check has `tests` on and init talks to the repository's backend, signed in, it asks the hint
    lookup once; a 404 or 405 adds to init's `Change by hand` the cache step a CI job needs: for
    GitHub Actions, `actions/cache` of `~/.cache/hushrun/units/*.v1.sqlite` (the macOS and
    Windows paths beside it) with a key of `runner.os` and the commit; for GitLab, whose cache holds paths below
    the project only, `HUSHRUN_CACHE_DIR: .hushrun-cache` and a cache of
    `.hushrun-cache/units/*.v1.sqlite` keyed by `CI_RUNNER_EXECUTABLE_ARCH`. The key holds the runner's system, so item 3 holds for a
    cache too. Only the store travels, as RFC 0003 §3.11 has it: never the outbox of suspensions
    or the local reuse state beside it in `units/`, since a cache is restored by its key's prefix
    and an old one would bring back a suspension a person had since resumed. The one file is
    enough, since every write to the store, the note of a hint sent included, checkpoints its WAL
    into it. init edits no CI file for it, as it proposes Vitest's config line and writes none
    (ADR 0071). Nothing where the server keeps hints, which carry the same lists to more machines;
    nothing with `--local` or no backend, where CI keeps no results (ADR 0067 item 3), so the step
    would spare nothing; and `--dry-run`, which sends nothing, says nothing of it.

## Consequences

- A CI runner or a sandbox with an empty cache reuses what a machine with the writer's store copied
  in would, for pytest, Vitest and Go: the parity tests of `two_machines.rs` hold it, against the
  test kit's fake results server, on Linux with every runner installed.
- A run with a server that keeps hints sends at most one request more before COMMAND, per system it
  reads, and at most one after, only when what its test files read changed.
- The server learns how many test files a check has, how many files each read, and which read the
  same file, and no path: the two-machine tests search every request for hints for each fixture
  path, in the clear, inflated and in base64. Who could guess the key is in the plan's §2.2.
- A Mac's hint doesn't spare Linux CI its first full run after a source file is added or removed.
  `observe` can measure what one pool would add, and the rule widens only on that evidence.
- A member can write hints that match nothing: they cost that repository runs until newer hints
  replace them, 3 per address at most. The same member could record failing results today.
- Pull requests from forks reuse nothing, as before (S4, the owner's: out for now).
- A repository whose backend keeps no hints gets, from `hushrun init`, the cache step its CI jobs
  need (item 13), and nothing in its CI files changes unless a person adds it.

## Alternatives considered

- **CI's cache alone** (RFC 0003 D3 as written): a sandbox has none, a pull request reads only its
  base branch's caches, and it is one more step in every workflow. It stays for a backend without
  hints, and the owner chose both.
- **One pool for all systems:** a Mac's list keys a test file on Linux, and a change to a file only
  Linux reads leaves the key as it was, so a Mac's pass is reused where the local design never
  would. Kept per system, and widened only if `observe` shows it costs much.
- **Plain paths, for repositories that say so:** simpler and debuggable, but a setting to get wrong,
  and a private repository's layout on a server the moment it is.
- **A random line in `hushrun.yaml` as the key:** closes the cases §2.2 lists where someone knows
  the check's whole list of sources, but needs setup in every repository, and is one more secret to
  lose. It can come later: it only changes the key, so hints are written again under it.
