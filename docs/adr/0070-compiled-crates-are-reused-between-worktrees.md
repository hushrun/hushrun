# 0070. Compiled crates are reused between worktrees

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** B-1 to B-22

## Context

hushrun skips a check that already passed (§7.6). A check that runs still compiles everything it
needs, and on a Rust workspace the build is the larger cost. Every worktree, every run of the
maintainers' gate and every branch switch compiles the same crates again, and each keeps its own
`target/`.

- **The targets are large.** Measured on the maintainer's Mac on 2026-11-10, for the plan this ADR
  follows: the CLI's target 21 GB, the target of hushrun's server (`hushrun-cloud`) 36 GB, the
  gate's and the release's targets about 120 GB together, and the disk 95% full. M0 found the CLI's
  main target at 22 GB, then 31 GB, and the server's at 55 GB of apparent size in 156,260 files
  (`reports/2026-11-11-build-blocks/m0-cli.md`, `m0-back.md`).
- **Most of a cold build is crates nobody changed.** A cold `cargo test --workspace --no-run` of the
  CLI (rustc 1.94.0, dev profile, 448 units) took 66.8 s wall and 553.6 s summed over units:
  312.6 s for 288 registry libraries, 22.8 s for 20 proc macros, 5.6 s to compile and 6.6 s to run
  33 build scripts, 55.0 s for 10 workspace libraries and 150.9 s for 64 programs (`m0-cli.md`).
  The server's `cargo test --no-run` (381 units) took 94.7 s wall and 629.5 s summed: 340.1 s for
  270 registry libraries, 25.1 s for 4 git libraries, 17.9 s for 14 proc macros, 16.5 s for build
  scripts, 34.0 s for the workspace library and 196.0 s for 26 programs (`m0-back.md`).
- **Cargo's own cache is one target folder, and it decides by dates.** Cargo reuses an output only
  in the folder that built it, and calls it fresh when its sources are older than it. Files
  restored with an older date left a stale library in place, and a new migration in a folder the
  sqlx macros read rebuilt nothing, since cargo doesn't track that folder (`m0-back.md`). Files
  that carry a newer date made cargo rebuild every workspace package of an otherwise complete
  target (`m0-cli.md`). A target also never deletes: the plan found the same crate under up to 14
  hashes, about 6 old copies of each test binary and 13 to 46 GB of incremental state in those
  targets.
- **Outputs name the folder they were built in.** Built from two absolute paths with no special
  flags, 651 of 714 `.rmeta` and 300 of 328 `.rlib` files of the CLI were identical, and 0 of 22
  proc macros: `serde`, `serde_core` and `thiserror` include a file of their build script's output
  by absolute path, everything that depends on them carries a hash of it, and a proc macro holds its
  own output path (the plan's review). On the server, 116 of 564 files differed (`m0-back.md`).
- **Nothing existing fits.** Cargo's cache shared between workspaces is a 2026 goal that starts on
  nightly without build scripts or proc macros. sccache stores no proc macro, nothing linked and no
  incremental compile, and its path option changes only the key, so a file it reuses still points
  at another worktree.

## Decision

1. **hushrun stands in for rustc, only inside `hushrun run`** (B-2). When the mode is not `off`,
   both forms of `hushrun run`, `NAME -- COMMAND` and each check of hushrun.yaml that runs here,
   start the command with `RUSTC_WRAPPER` set to a link named `hushrun-rustc` that points at the
   hushrun binary, and `HUSHRUN_BUILD_SESSION` naming the run's folder. The binary knows its role
   by that name before it reads any setting. hushrun never edits cargo's configuration, by the
   owner's decision: a plain `cargo` outside `hushrun run` neither reads nor fills the store. The
   wrapper is not set when `RUSTC_WRAPPER` is set already, since that is the person's choice; in
   CI (C-14's detection), which comes later with sharing between machines; on Windows; for an
   isolated check, whose container has neither the link nor the store; or with no cache directory.
   Cargo's fingerprints don't count the wrapper, so changing the mode never forces a rebuild:
   adding it to a warm target rebuilt 0 units (`m0-cli.md`).
2. **Three modes per user, `off` until the person turns it on** (B-1). `off` changes nothing. In
   `observe` every compile runs as in `off`, blocks are stored, and a compile whose block exists
   is compared with it, so a person sees what would have been reused, and whether anything
   differed, before trusting it. In `on` a compile whose block exists does not run. `hushrun build
   on|observe|off` saves the mode; `HUSHRUN_BUILD` overrides it for one command.
3. **A block is one compiler call's outputs, and what can't be keyed is passed through** (B-3,
   B-4). Each call ends in exactly one outcome, from one function: `reused`, `built and stored`,
   `built, not stored` or `passed through`, the last two with a reason. Libraries (also for `cargo
   check`), proc macros, build-script binaries and programs are handled. clippy's driver, probes
   (`--print`, `-vV`, a source of `-`), `@file` arguments, a `--target` that is a file or not one
   of the compiler's built-in targets, a `--sysroot`, a `-Z` flag outside a short known list, an
   `--extern` with no path, a caller's own path remap, coverage and profile builds, an `--emit`
   kind other than `dep-info`, `metadata` and `link`, `cdylib`, `dylib` and `staticlib`, a compile
   without `--out-dir`, `-C extra-filename` or `--json` with `artifacts`, a package on the person's
   never-store list, and a compiler whose identity can't be established run exactly as asked, and
   nothing is stored. An argument the wrapper doesn't know how to key is a reason to pass through,
   never to guess. Cargo's probes and the compiles a build script runs count nowhere (B-20).
4. **Two keys, and what was read comes from the compiler** (B-5, B-6, B-7, B-11, B-17). The first
   key is what was asked: the compiler's identity, which covers its files and its standard
   library, so a toolchain updated under the same name is a new one; the arguments, with roots
   replaced and the folder-only and message-format ones left out; the variables cargo sets for the
   crate's compile and those its build script set; and the variables rustc, the C driver and the
   linker read without listing them, such as `MACOSX_DEPLOYMENT_TARGET` and `SDKROOT`. It holds no
   machine, user, branch, commit or time. The second key adds what the compile read, as rustc's own
   dep-info lists it: each file by SHA-256 and the places a module could have been; each
   `env-dep`; each dependency by the hash of its file; each native library named with `-l`, in
   every folder rustc searches for it; the names in every folder a read file sits in, so a new
   migration changes the key; every file of the build script's output; what a local build script
   declared; and for a linked output, every library and file the linker was handed. The list of
   files to hash before a lookup comes from earlier compiles of the same first key, and is a hint
   that is never trusted: the wrapper hashes the files as they are now, a file that is gone is
   hashed as absent, and a block exists under a key only because a compile read exactly those
   bytes. A wrong or old list gives a key nobody recorded, which is a build. A compile during which
   a read file changed, by size, dates, inode or device, is `built, not stored`.
5. **Paths are recorded from their roots, and a block moves only with proof** (B-8, B-9, B-10). A
   root is the workspace, the package, the build script's output, the output folder or a dependency
   folder. A path under a root is recorded as the root's name and the rest, in its given and its
   resolved form, and looked up under the current root; a path under another checkout is never
   opened. The wrapper adds `--remap-path-prefix` for the workspace, as `.`, and for the build
   script's output, as a fixed stand-in that names the package. After a compile every output is
   searched for each root as raw bytes. None found: the block may be reused anywhere. Any found, or
   an output that can't be searched: the block is tied to its roots. A compile that read a root
   through a variable is compiled a second time with that variable naming a link to the same
   folder, and is reused anywhere only if the bytes are the same.

   M0 proved the remap before any product code. Without it, on the CLI, `serde`, `serde_core`,
   `serde_json`, `serde_saphyr`, `thiserror` and `hushrun-core` differed between two paths;
   remapping the workspace alone left the same six, remapping the build script's output alone left
   only `hushrun-core`, and both left none. Two builds of one checkout into two target folders
   differ too without the second remap, because the build script's output lives in the target
   (`m0-cli.md`). With the remap, built at two paths of different length, every registry library
   was identical (288 of 288 `.rlib` and `.rmeta` on the CLI, 270 of 270 on the server), and so were
   the server's 4 git libraries and its workspace library, the CLI's 9 of 10 workspace libraries,
   all 20 and 14 proc macros, and all 33 build-script binaries of each (`m0-cli.md`, `m0-back.md`).
   The one library that differed, `hushrun-testkit`, reads `env!("CARGO_MANIFEST_DIR")`: it depends
   on its path, and the second compile is what ties it.
6. **No block comes from an incremental compile, and an edit keeps incremental compilation**
   (B-9). An incremental compile gives a different crate hash, and storing one could carry an
   incremental-compilation bug into every worktree. So the wrapper leaves `-C incremental` out of
   a first build in an output folder, compiles it clean and stores it. A compile that asks for
   `-C incremental`, finds no block, and already has its dep-info file in the output folder is a
   rebuild after an edit, and is passed through exactly as asked. Leaving incremental out of every
   compile was measured first, on a one-line edit and `cargo test --no-run` on a warm target: on
   the CLI, 9.0 to 11.7 s with incremental became 17.3 to 33.5 s without (`m0-cli.md`); on the
   server, 9.8 to 14.5 s became 40.2 to 77.9 s (`m0-back.md`). A cold build without incremental was
   no slower: on the CLI, a mean of 253.5 s of user CPU against 275.7 s with it (`m0-cli.md`).
7. **A reused block is new files in the target** (B-12). Each file is cloned, or copied where the
   file system can't clone, under a temporary name in the output folder and renamed into place,
   writable, all with one modification time. The dep-info file and the recorded compiler messages
   are written with this folder's paths, the notices cargo waits on last. A step that fails before
   the first message is written ends in rustc running.
8. **One store for the machine, not one per repository** (B-13). The store is `<cache
   directory>/build`, or `HUSHRUN_BUILD_DIR`. A key commits to everything a block was built from,
   so a block is as valid in another repository as in the one that built it, and dependencies two
   repositories share at the same version and features compile once. A block is written under
   `tmp/`, synced and published by renaming its folder into `blocks/`: the first writer wins and an
   identical second one is dropped. Nothing rewrites a published block. Each block records the
   repository that built it, for `hushrun build status` and for the sharing rules later. The
   store's root is one parameter, so a store per repository stays a small change.
9. **A block unused for 15 days is deleted, and there is no size limit** (B-14, B-22). A reuse and
   a creation set a block's last use to the command's time. After a run, at most once a day,
   `hushrun run` writes the sweep's stamp, then moves every block unused for more than `keep_days`
   (15 unless set; `HUSHRUN_BUILD_KEEP_DAYS` overrides it) into `trash/` and deletes it there, and
   deletes what is more than a day old under `tmp/` and `trash/`. There is no limit on the store's
   size, by the owner's decision: last use is the only rule. Separately, nothing is stored while the
   store's volume has less than `min_free_gb` free, 10 GB unless set, 0 to turn it off; those
   compiles are `built, not stored`, and the run says so once. The floor is not a limit: it deletes
   nothing and only stops new blocks. The plan proposed it, and the owner has not yet ruled on it.
10. **Programs are in scope** (B-6, B-9, B-18), by the owner's decision: test binaries, `hushrun`
    and `hushrun-cloud`, not only libraries. A program's key includes every library the linker was
    handed (`--print=link-args`), the libraries `-l` names and the files linker options name, the
    linker and the SDK; where that list can't be read, or names a file that can't be found, it is
    `built, not stored`. A program is stored with the debug objects it names, and gets no
    `-oso_prefix`, since its debug info must work; so on macOS a debug program names its target
    folder and is tied to it. It is reused when the same folder comes back to the same inputs,
    such as a branch switched back, or the gate, which builds every commit in one folder.
    On Apple targets a proc macro gets an install name that doesn't hold its folder
    (`-install_name @rpath/<file>`), and proc macros and build-script binaries get `-oso_prefix`
    for their dependency folder: nobody debugs those, and without it each holds the folder it was
    linked in. Those two made all 20 and 14 proc macros identical in M0, where without them all
    differed (`m0-cli.md`, `m0-back.md`).
11. **Every failure of the machinery ends in rustc running as asked** (B-15, B-16, B-19). An error
    computing a key, reading the store or putting files in place runs rustc with nothing stored.
    Settings that can't be read, a store that can't be written, CI and Windows are `passed
    through`. A kill at any point leaves nothing a later build takes for a block. A first key is
    `ok` or `suspended`, and a suspended key is never reused. It is suspended when a dependent fails
    to load a reused block (rustc's E0460, E0461, E0462, E0463, E0464, E0514 and E0786, never an
    error in the dependent's own code), and the reused files are then removed so that cargo builds
    them; or when an audit differs: in `on`, one in fifty of the library compiles that could be
    reused runs anyway and is compared byte for byte with the block, and a difference serves the
    fresh outputs. Programs and proc macros are not compared. `hushrun build clean` clears every
    suspension.
12. **Each piece of state has one home.**

    | Thing | States | Home | Moved by |
    |---|---|---|---|
    | Mode | `off`, `observe`, `on` | `build.json` beside the credentials, written whole under a lock; a variable changes one command's view | `hushrun build on\|observe\|off` |
    | One compile in a run | `reused`, `built and stored`, `built, not stored`, `passed through` | A line in the run's log, which the summary reads | One function in the wrapper |
    | A block | writing, stored, expired, gone | The folder it is in: `tmp/`, `blocks/`, `trash/` | Publishing and expiring are renames |
    | Last use | A time | The block's `used` file, from the command's clock | A reuse or a creation sets it, the sweep reads it |
    | A first key | `ok`, `suspended` | A file beside its hints | A failed load or audit sets it, `hushrun build clean` clears it |

## Consequences

- **What a person sees.** `hushrun build on`, `observe` and `off` set the mode, `hushrun build
  status` shows it with the store's folder, its size and its blocks by age and by repository, and
  `hushrun build clean` empties the store. After each command `hushrun run` writes one line after
  its own prefix, such as `build: 331 of 372 crates reused, 41 built, saving about 2m 40s`,
  `build: 41 crates built, none reused`, or, in `observe`, `build: observing: 2 of 3 crates would
  have been reused, 1 differed from what a reuse would have given`; then at most five notes, such
  as a suspended key, and the free-space line once.
- **Debug info names workspace files from the workspace root.** Compiler messages are unchanged,
  since cargo already passes relative paths. A backtrace frame reads `./crates/…` where it read an
  absolute path. A debugger shows source when its working directory is the workspace root, or with
  a source map from `.` to it; from elsewhere, breakpoints by file and line still resolve
  (`m0-cli.md`).
- **The first build in an output folder is not incremental.** Rebuilds after an edit are. A cold
  target without incremental state was 2.3 GB against 3.4 GB (`m0-cli.md`).
- **What it does not do.**
  - It doesn't shrink a target. Blocks make one cheap to delete; cleaning targets is later.
  - Build scripts still run in each target, such as ring's C compile.
  - What neither rustc nor cargo can see, this can't either: a database a macro queries, a file a
    macro reads without listing it. Known macros are handled by name, a package can be put on the
    never-store list, and the audit is the backstop. Inside one target folder cargo still decides
    what to compile, and a compile cargo doesn't ask for never reaches the wrapper.
  - A build script that writes the time into its output, such as libmimalloc-sys's C code, gives
    its crate a new key each time it runs, unless the person sets `SOURCE_DATE_EPOCH`; the wrapper
    can't set it, since that compile happens in the build script (`m0-cli.md`).
  - Nothing changes on Windows or in CI: the wrapper isn't set there.
  - Cargo's new build layout (1.100) is not handled; both repositories pin 1.94.
- **The store can grow past the free disk.** With programs stored and no limit, one set of test
  binaries with their debug objects is 1 to 5 GB, stored again whenever a library they link
  changes (the plan). The free-space floor stops new blocks below 10 GB; the 15-day expiry is the
  only thing that deletes.
- **Sharing between machines is later.** Nothing leaves the machine. Keys hold no machine, user or
  time, and each block records the repository that built it, so later rules can decide whose
  blocks count; registry
  crates' paths under the cargo home stay absolute, which M0 did not test across machines
  (`m0-cli.md`).
- **Open, for the owner:**
  - the free-space floor (B-22), built at 10 GB, not yet ruled on;
  - the size limit, to revisit with the numbers from replaying each repository's history;
  - programs made movable on macOS with `-oso_prefix`, at the cost of their debug paths;
  - the gate sharing the worktrees' store, with `check-local.sh` setting `HUSHRUN_BUILD_DIR`.

## Alternatives considered

- **One shared target folder.** Cargo decides freshness by dates, so one folder that several
  checkouts write either rebuilds what a newer date touched or keeps an output whose source came
  back with an older date: M0 saw both (`m0-cli.md`, `m0-back.md`). A target keeps everything it
  ever built, and one that outlived its folder's name held outputs nothing reaches: 1,079 of the
  server target's 1,120 dep-info files named an earlier path (the plan).
- **Cloning a target into a new worktree.** On the CLI the clone took 31 to 38 s, cargo kept the
  374 dependency units and rebuilt the 74 workspace units, saving about 150 s of the 275 s of CPU,
  at up to tens of GB of logical space (`m0-cli.md`). On the server the clone took 42 s for 55 GB,
  33 units were rebuilt, and the whole took about 118 s against a 92 to 95 s cold build: it saved
  nothing (`m0-back.md`). It also carries cargo's date hazard, and copies stale outputs with the
  live ones.
- **sccache.** It stores no proc macro, nothing linked and no incremental compile, and a file it
  reuses still names the worktree that built it.
- **Waiting for cargo's own cache.** It starts on nightly, without build scripts or proc macros.
- **A store per repository.** A key already commits to everything a block was built from, so
  splitting the store only compiles shared dependencies once per repository. The root is one
  parameter, so it stays a small change.
- **A size limit.** Declined by the owner: a block is deleted when unused for 15 days, and that is
  the only rule.
