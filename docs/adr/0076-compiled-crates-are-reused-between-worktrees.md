# 0076. Compiled crates are reused between worktrees

- **Status:** accepted
- **Date:** 2026-11-11
- **Rules:** B-1 to B-23

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
   key is what was asked: the compiler's identity, which covers its files and its standard library,
   so a toolchain updated under the same name is a new one; the arguments, with roots replaced and
   the folder-only and message-format ones left out; the variables cargo sets for the crate's
   compile and those its build script set; the variables rustc, the C driver and the linker read
   without listing them, such as `MACOSX_DEPLOYMENT_TARGET` and `SDKROOT`; `SOURCE_DATE_EPOCH`,
   which compilers and build tools take "now" from when it is set; and what the wrapper adds to the
   compile besides the remaps, so that a program compiled `movable` and one compiled `tied` never
   share a key (decision 11). It holds no machine, user, branch, commit or time. The second key
   adds what the compile read, as rustc's own dep-info lists it: each file by SHA-256 and the
   places a module could have been; each `env-dep`; each dependency by the hash of its file; each
   native library named with `-l`, in every folder rustc searches for it; the names in every folder
   a read file sits in, so a new migration changes the key; every file of the build script's
   output; what a local build script declared; and for a linked output, every library and file the
   linker was handed. The list of files to hash before a lookup comes from earlier compiles of the
   same first key, and is a hint that is never trusted: the wrapper hashes the files as they are
   now, a file that is gone is hashed as absent, and a block exists under a key only because a
   compile read exactly those bytes. A wrong or old list gives a key nobody recorded, which is a
   build. A compile during which a read file changed, by size, dates, inode or device, is `built,
   not stored`.

   Linux dates files from a clock that moves once a tick (1 ms at `HZ=1000`) and lags the real
   one, and two rules follow from it. The compile's start is the date the file system gives an
   empty file the wrapper creates in the output folder just before the compile, so it is read on
   the same clock as the files (B-11); with the real start less 20 ms, as first built, a build
   script's output and the libraries a link takes, written a few milliseconds before the compile,
   counted as changed during it. And the memo of the compiler's identity is kept only when every
   folder and file it is trusted by is dated before the memo, since a folder changed twice in one
   tick keeps its date (B-17) (`m6-linux.md`).

   Two of those parts are narrower than first built. A native search folder (`-L native=`) is
   keyed, by its names and its static libraries, for a library only: a library can bundle a static
   library found there with nothing reporting it. A linked output has none in its key, since cargo
   hands it every dependency's folders whether it links anything from them or not, and its link
   line names every library it takes. Keyed for every compile, the folder of `libmimalloc-sys`,
   whose library changed in every build (decision 14), made every program of a package that depends
   on it miss, though two of them link it. And a macro that reads the first `.env` found walking up
   (sqlx, `dotenvy_macro`, `dotenv_codegen`) is keyed by the file it finds above the roots, by
   SHA-256 or none, not by each place it looked: every place above the workspace lay outside every
   root, which tied the server's library and every program to its folder.
5. **Paths are recorded from their roots, and a block moves only with proof** (B-8, B-9, B-10). A
   root is the workspace, the package, the build script's output, the output folder, a dependency
   folder, a native search folder, or a location cargo hands the compile: the target folder
   `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` names when it is absolute, `CARGO_TARGET_TMPDIR`,
   and each folder a `CARGO_BIN_EXE_<name>` value is in. Before those locations were roots, an
   integration test got another key in every checkout whose target folder lay outside it. A path
   under a root is recorded as the root's name and the rest, in its given and its
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
   The one library that differed, `hushrun-testkit`, read `env!("CARGO_MANIFEST_DIR")`: it depended
   on its path, the second compile tied it, and with it every test binary that links it. It now
   finds the workspace when the tests run, from the running process's `CARGO_MANIFEST_DIR` or by
   walking up from the executable, so it bakes no folder in.
6. **No block comes from an incremental compile, and an edit keeps incremental compilation**
   (B-9). An incremental compile gives a different crate hash, and storing one could carry an
   incremental-compilation bug into every worktree. So the wrapper leaves `-C incremental` out of
   a first build in an output folder, compiles it clean and stores it. A compile that asks for
   `-C incremental`, finds no block, and already has its dep-info file in the output folder is a
   rebuild after an edit, and is passed through exactly as asked. Leaving incremental out of every
   compile was measured first, on a one-line edit and `cargo test --no-run` on a warm target: on
   the CLI, 9.0 to 11.7 s with incremental became 17.3 to 33.5 s without (`m0-cli.md`); on the
   server, 9.8 to 14.5 s became 40.2 to 77.9 s (`m0-back.md`). A cold build without incremental was
   no slower: on the CLI, a mean of 253.5 s of user CPU against 275.7 s with it (`m0-cli.md`). The
   owner kept this rule on 2026-11-11: "Keep it".
7. **A reused block is new files in the target** (B-12). Each file is cloned, or copied where the
   file system can't clone, under a temporary name in the output folder and renamed into place,
   writable, all with one modification time. A program's debug bundle is cloned whole into a
   temporary folder and renamed into place as one folder, after a bundle already there is moved
   aside and removed, so no bundle is ever there in part. The dep-info file and the recorded
   compiler messages are written with this folder's paths, the notices cargo waits on last. A step
   that fails before the first message is written ends in rustc running. On a file system that
   can't clone, such as ext4 on Linux, each reused file is a full copy of the store's, which costs
   disk and time (see Consequences).
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
   nothing and only stops new blocks. The plan proposed it, and the owner accepted it on
   2026-11-11: "the free space floor is fine".
10. **Programs are in scope, and on macOS they move with a debug bundle** (B-6, B-9, B-10, B-18),
    by the owner's decisions: test binaries, `hushrun` and `hushrun-cloud`, not only libraries. A
    program's key includes every library the linker was handed (`--print=link-args`), the
    libraries `-l` names and the files linker options name, the linker, the `dsymutil` rustc runs
    and the SDK; where that list can't be read, or names a file that can't be found, it is `built,
    not stored`.

    As first built, a debug program on macOS named its object files by absolute path in its debug
    map, so it was tied to its target folder. Three ways were weighed: leave it tied; give it
    `-oso_prefix` alone; or give it a debug bundle. `-oso_prefix` alone made programs identical
    across paths, but lost `file:line` for the workspace's own frames whenever the working
    directory was not the target folder, which includes every `cargo test`, and no setting gives
    them back. With a bundle and the prefix at the workspace, programs and their bundles were
    identical across paths, and backtraces kept their lines from every working directory tried,
    with the checkout that built them gone (`m3-programs-oso-prefix.md`). On 2026-11-11 the owner
    chose "the third option": movable, with a debug bundle per program.

    So on Apple targets a program that links with debug info gets `-C split-debuginfo=packed`, and
    rustc runs `dsymutil` and writes `<program>.dSYM` beside it with the DWARF of every object it
    was linked from; `-C strip=debuginfo`, so the program keeps no debug map naming those objects
    by their paths; and `-oso_prefix` for the workspace's resolved form, so the UUID the linker
    computes before the strip holds no path when the target folder is inside the workspace. The
    bundle's `Contents/Resources/Relocations`, which names the program by its absolute path and
    which no debugger reads, is removed. The bundle is part of the block: every file under it is
    stored, searched for the roots, and put back as one folder.

    A program still ties when it bakes a folder in, such as a test that holds
    `env!("CARGO_BIN_EXE_hushrun")` or a path made from `env!("CARGO_MANIFEST_DIR")`: B-10's search
    finds it. Checking out one commit at a new path rebuilt 12 of the CLI's 70 programs, every one
    tied for that reason (`m4-own-sde-inside-on.md`), and 2 of the server's 26,
    `no_stages_migration` and `billing` (`m4-back-inside-on.md`). Such a program is reused when the
    same folder comes back to the same inputs, such as a branch switched back.

    On Apple targets a proc macro gets an install name that doesn't hold its folder
    (`-install_name @rpath/<file>`), and proc macros and build-script binaries get `-oso_prefix`
    for their dependency folder: nobody debugs those, and without it each holds the folder it was
    linked in. Those two made all 20 and 14 proc macros identical in M0, where without them all
    differed (`m0-cli.md`, `m0-back.md`). They are reused anywhere unless B-10 finds a root in them.

    On Linux none of these steps is needed, and proc macros, build-script binaries and programs
    move all the same. ELF has no install name and no debug map. The dev profile's DWARF is linked
    into the output itself, and with the workspace remap its paths are relative. The GNU build ID
    is a hash of the output's own bytes, so it holds no path. The wrapper adds only the two remaps
    there, and `programs: tied` changes nothing. In `observe` on arm64 Linux, all 20 proc macros,
    33 build scripts and 58 movable programs found the block another checkout built and matched it
    byte for byte; the other 12 programs tie for the reason above (`m6-linux.md`). On Linux rustc
    also hands the linker a search folder inside its temporary folder, whose name is new at each
    compile; it is not a link input (B-18), or every linked output would have a key of its own.
11. **A setting keeps programs tied, for whoever wants that** (B-21). `programs` in `build.json`
    is `movable`, the default, or `tied`, the behavior first built: none of the three additions,
    the program stored with the debug objects it names and reused only in the target folder that
    built it. Any other value makes the file invalid. The session carries what the file said when
    the run began, so every compile of a run sees one value. A program set to `tied` whose flags
    would allow a bundle has the bundle an earlier compile left removed first, since that bundle
    matches no program compiled now and would keep it from being stored. A program whose own flags
    say `-C split-debuginfo=off` or a `-C strip` other than `none`, one without debug info, and
    one for another target are left as they are under either value.

    What `movable` costs is a `dsymutil` run per program and about twice the bytes on a first
    build. What it gains is the programs of every further checkout. Measured:

    | | CLI | Server |
    |---|---|---|
    | First build, CPU, programs tied | 259.3 s | 291.2 s |
    | First build, CPU, programs movable | 362.4 s and 371.5 s | 507.5 s and 488.6 s |
    | First build, bytes stored, tied | 1.97 GB | 3.30 GB |
    | First build, bytes stored, movable | 3.90 GB | 6.96 GB |
    | Further checkout, programs tied | 18.0 s and 17.8 s wall, 118.3 s and 114.0 s CPU, 344 and 337 of 422 reused | 37.5 s and 30.1 s wall, 148.3 s and 151.7 s CPU, 313 and 315 of 355 reused |
    | Further checkout, programs movable | 19.4 s and 19.8 s wall, 73.4 s and 72.4 s CPU, 404 and 402 of 429 reused | 40.6 s and 30.2 s wall, 66.2 s and 60.3 s CPU, 342 and 342 of 355 reused |

    The first-build rows are `observe` runs from an empty store at the same commit
    (`m1-*-observe.md` tied, `m4-*-observe.md` movable). The further-checkout rows are three
    checkouts of one commit on a separate volume (`m5-disk-and-times.md`): the tied ones were
    measured on an earlier commit of the branch with the target outside the checkout, the CLI at
    another commit. Other sessions were compiling the whole time, so wall times track the load and
    CPU is the steadier figure.
12. **Every failure of the machinery ends in rustc running as asked** (B-15, B-16, B-19). An error
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
13. **`hushrun run` rebuilds what changed in ways cargo can't see** (B-23). Cargo decides whether
    to call rustc by dates and by the files rustc listed last time, so four changes never reached
    the wrapper, though its keys cover them: a file added to a folder a crate reads (sqlx's
    `migrate!` includes each migration but lists the folder unseen), a file a known macro reads, a
    file where a module could also have been, and bytes changed under a date older than the last
    build (an archive extracted with its own dates). The owner asked for a fix "transparently to
    users, without adding random flags no one will remember". So inside `hushrun run`, with the
    mode `observe` or `on`, before COMMAND starts, hushrun checks the crates it has seen compiled
    in the target folders of the folder it runs in, and removes the outputs of any whose inputs
    changed in one of those ways. Cargo finds them missing and calls rustc, and the wrapper answers
    by content as usual. Nothing is asked of the person.

    Every compile the wrapper sees through to a successful end, a reuse among them, leaves a
    record of its unit in its output folder's `.hushrun/units/`: its outputs by inode, device and
    size, and what to watch outside the shared folders, with what was seen. The store keeps one
    file per output folder with a record, under `out-dirs/`, naming every working directory its
    compiles ran in. The check takes the lock cargo holds on each target folder and skips one a
    build holds; leaves to cargo what cargo sees itself; removes only files a record names, never
    a source file; keeps the dep-info, so a crate cargo builds incrementally stays incremental;
    and stops at a two-second deadline, leaving the rest for the next run. A crate from a registry
    or a git source has no record: what its build script wrote is cargo's to watch.

    Its limits, as B-23 states them: only crates the wrapper saw compiled are checked, and only in
    the output folders registered for the folder `hushrun run` runs in, so a script that builds
    into a folder elsewhere is not covered, and neither is a crate last built by plain cargo. A
    macro that reads a file the table of known macros doesn't name, and a file the link line names,
    are not watched. A name added to a watched folder that a fresh compile would also see, such as
    an editor's swap file beside a source, rebuilds the crate, as its key says.
14. **This repository pins the date its C code is compiled with.** `libmimalloc-sys` 0.1.49 writes
    the C compiler's `__DATE__` and `__TIME__` into its static library
    (`c_src/mimalloc/v2/src/options.c:222`), so that library differed in every build, and nothing
    that links it could be reused from another build. Of the 393 packages in the released lockfile
    it is the only one that does. On 2026-11-11 the owner agreed: "yes add it, run the full test
    suite with it". The CLI's `.cargo/config.toml` sets `SOURCE_DATE_EPOCH = "315532800"`
    (1980-01-01, which every tool accepts); a value set in the environment wins. With it the
    library is byte-identical across builds (`m0-cli.md`), and the gate's whole suite passed:
    1,705 tests (`m5-disk-and-times.md`). The effect, on six checkouts of one commit with the
    variable set in the environment against without it: at a path no build used before, 10.3 to
    12.8 s wall and 402 to 405 of 429 compiles reused, against 31.3 to 38.3 s and 359 to 365; at a
    path built before, 5.3 to 6.5 s against 25.6 to 27.2 s (`m4-own-sde-inside-on.md`,
    `m4-own-inside-on.md`). The wrapper never sets the variable, since the C compile runs in the
    build script, which never goes through `RUSTC_WRAPPER`; it keys it (decision 4). The server
    needs nothing: no dependency of its writes the time.
15. **Each piece of state has one home.**

    | Thing | States | Home | Moved by |
    |---|---|---|---|
    | Mode | `off`, `observe`, `on` | `build.json` beside the credentials, written whole under a lock; a variable changes one command's view | `hushrun build on\|observe\|off` |
    | What programs do | `movable`, `tied` | `programs` in `build.json`; the session carries one value for the run | The person, in the file |
    | One compile in a run | `reused`, `built and stored`, `built, not stored`, `passed through` | A line in the run's log, which the summary reads | One function in the wrapper |
    | A block | writing, stored, expired, gone | The folder it is in: `tmp/`, `blocks/`, `trash/` | Publishing and expiring are renames |
    | Last use | A time | The block's `used` file, from the command's clock | A reuse or a creation sets it, the sweep reads it |
    | A first key | `ok`, `suspended` | A file beside its hints | A failed load or audit sets it, `hushrun build clean` clears it |
    | A unit's record | there, gone | `.hushrun/units/` in the output folder, written whole | One function: removed before rustc runs or a reuse, written after one that ends well; the check removes it with the outputs it names |
    | An output folder known to the check | registered, dropped | `out-dirs/` in the store, written at most once per run per folder | A compile's record registers it; the sweep drops a folder that is gone or unseen for `keep_days` |

## Consequences

- **What a person sees.** `hushrun build on`, `observe` and `off` set the mode, `hushrun build
  status` shows it with the store's folder, how long blocks are kept, `programs`, the store's size
  and its blocks by age and by repository, and `hushrun build clean` empties the store. After each
  command `hushrun run` writes one line after its own prefix, such as `build: 331 of 372 crates
  reused, 41 built, saving about 2m 40s`, `build: 41 crates built, none reused`, or, in
  `observe`, `build: observing: 2 of 3 crates would have been reused, 1 differed from what a reuse
  would have given`. Then, for each crate the check before the command removed, at most three
  lines, such as `build: app is rebuilt: a file was added to migrations, which cargo doesn't
  watch`; then at most five notes, such as a suspended key, and the free-space line once.
- **On macOS each program has a `.dSYM` beside it in `target/`,** and the program holds no debug
  map: its debug info is in the bundle. Cargo links `target/debug/<bin>.dSYM` to the hashed
  program's bundle, so the copy cargo makes finds its lines too. With `programs` set to `tied`,
  programs are as cargo builds them.
- **Debug info names workspace files from the workspace root.** Compiler messages are unchanged,
  since cargo already passes relative paths. A backtrace frame reads `./crates/…` where it read an
  absolute path, and keeps its `file:line` from any working directory, in a program reused in
  another checkout with the checkout that built it deleted. A debugger shows source when its
  working directory is the workspace root, or with a source map from `.` to it; with the bundle,
  breakpoints by file and line resolve from the workspace root and from `/`
  (`m0-cli.md`, `m3-programs-oso-prefix.md`).
- **The first build in an output folder is not incremental.** Rebuilds after an edit are. A cold
  target without incremental state was 2.3 GB against 3.4 GB (`m0-cli.md`).
- **Measured on the final build** (`m5-disk-and-times.md`, `m5-*-on.md`, `m5-*-none.md`): three
  or four checkouts of one commit, each with its own target inside it, built with `cargo test
  --no-run` (`--workspace` on the CLI). Other sessions were compiling heavily: the 1-minute load
  was 54 to 460 during the timed runs, so wall times track the load and CPU is the steadier
  figure.

  | | CLI | Server |
  |---|---|---|
  | No wrapper, one build | 94.4 s and 77.7 s wall, 363.9 s and 357.9 s CPU | 134.5 s and 139.2 s wall, 403.9 s and 376.3 s CPU |
  | Reuse on, the first build | 126.8 s wall, 470.1 s CPU, 4.13 GB stored | 140.7 s wall, 539.0 s CPU, 6.98 GB stored |
  | Reuse on, each further checkout | 21.3 s, 19.0 s and 35.1 s wall; 76.1 s, 72.7 s and 83.5 s CPU; 403 to 406 of 429 compiles reused; 0.57 GB stored | 22.3 s, 19.3 s and 16.0 s wall; 58.3 s, 54.6 s and 50.6 s CPU; 338 to 344 of 355 reused; 0.48 GB stored |
  | Real disk for three checkouts, no wrapper | 11,230 MB | 14,373 MB |
  | Real disk for three checkouts, reuse on | 5,649 MB; the store alone 5,031 MB apparent | 7,920 MB; the store alone 7,579 MB apparent |
  | A block against a fresh compile | 0 differences in 1,716 compiles | 0 differences in 1,420 compiles |

  Real disk is `df` on a separate volume, where a reused file is a clone of the store's and is
  counted once.
- **Tested on arm64 Linux, in a container** (`m6-linux.md`): Debian 13 in Docker on the
  maintainer's Mac, kernel 6.12, GNU ld 2.45 through `cc`, the target on ext4, and the CLI's
  `cargo test --workspace --no-run`, 428 compiles a commit. A block against a fresh compile: 0
  differences in 5,564 compiles over 13 commit builds, and 0 in 8,132 counting the runs before the
  last two fixes. A further checkout of the same commit, its target beside it, reused 402 to 407
  of 428 compiles. Linux needed four fixes, each with a test that failed there first: three unit
  tests that assumed macOS's file dates, the compiler's memo (B-17), the compile's start (B-11)
  and rustc's temporary search folder (B-18), all described above.
- **On ext4 a reuse is a copy, so it saves time but not disk.** Three checkouts, each with its
  target inside it, took 38,997 MB of real disk with reuse on (27,529 MB of checkouts and an
  11,367 MB store, apparent) against 35,164 MB without: 11% more, where APFS took 50% less. Linux
  outputs are larger too, since each test binary keeps its DWARF: the first build stored 9.16 GB,
  against 4.13 GB on macOS. Time still dropped: a further checkout took 22.8 to 23.6 s wall and
  58.8 to 61.2 s CPU, against 74.2 to 87.1 s wall and 284.9 to 324.7 s CPU with no wrapper. A
  reused program takes a median of about 0.5 s to copy and hash, up to 6.7 s for the largest test
  binaries.
- **Not tested on Linux:** x86-64; other file systems (btrfs or XFS with reflinks, tmpfs,
  overlayfs, NFS); other linkers (`lld`, `mold`); musl and cross targets; a distribution's rustc
  without rustup; `hushrun run` by hand, beyond its tests.
- **What it does not do.**
  - It doesn't shrink a target. Blocks make one cheap to delete; cleaning targets is later.
  - Build scripts still run in each target, such as ring's C compile.
  - What neither rustc nor cargo can see, this can't either: a database a macro queries, a file a
    macro reads without listing it. Known macros are handled by name, a package can be put on the
    never-store list, and the audit is the backstop. Inside one target folder cargo still decides
    what to compile; the check before a run (decision 13) makes it ask for the four changes cargo
    misses, within that rule's limits, and any other compile cargo doesn't ask for never reaches
    the wrapper.
  - A build script that writes the time into its output, such as `libmimalloc-sys`'s C code,
    gives its crate a new key each time it runs, unless `SOURCE_DATE_EPOCH` is set. This
    repository sets it (decision 14); another project with such a dependency has to set it
    itself, since the wrapper can't (`m0-cli.md`).
  - Nothing changes on Windows or in CI: the wrapper isn't set there.
  - Cargo's new build layout (1.100) is not handled; both repositories pin 1.94.
- **The store grows, and only expiry shrinks it.** On the final build the first build stored 4.13
  GB on the CLI and 6.98 GB on the server, and each further checkout of the same commit 0.57 GB and
  0.48 GB: 5.83 GB and 8.42 GB after four (`m5-*-on.md`). A new commit of the server stored 0.48
  GB (`m4-back-inside-on.md`). Replaying the CLI's released history stored 2.10 to 2.74 GB per
  commit, most of it test binaries that the test library of those commits tied to their folder
  (`m4-cli-inside-on.md`). The free-space floor stops new blocks below 10 GB; the 15-day expiry is
  the only thing that deletes.
- **Sharing between machines is later.** Nothing leaves the machine. Keys hold no machine, user or
  time, and each block records the repository that built it, so later rules can decide whose
  blocks count; registry crates' paths under the cargo home stay absolute, which M0 did not test
  across machines (`m0-cli.md`).
- **Open, for the owner:**
  - "no size limit", against the store's measured growth above;
  - the gate sharing the worktrees' store, with `check-local.sh` setting `HUSHRUN_BUILD_DIR`. The
    gate's export, which gave every file the commit's date, now extracts with `tar -x -m`, fixed
    in the owner's script on his word, "fix what you found";
  - whether B-18's Linux change bumps the second key's schema. It changes which inputs a Linux
    program's, proc macro's or build script's key holds, and none on macOS; the schema was left at
    `hushrun.build-block/v4` so that macOS stores don't rebuild for nothing (`m6-linux.md`);
  - pushing and releasing. Nothing is released.

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
- **Programs tied, or movable by `-oso_prefix` alone.** Tied, every further checkout links every
  program again. `-oso_prefix` alone loses the workspace's own `file:line` in backtraces under
  `cargo test`, with no setting to bring it back (`m3-programs-oso-prefix.md`). `tied` stays as a
  setting (decision 11).
- **A flag or a build script line for what cargo can't see.** The owner asked for it to work
  without flags a person has to remember. The plan's line for the server, `rerun-if-changed` on
  its migrations folder, would cover one of the four cases in one repository.
