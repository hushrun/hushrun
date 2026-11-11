# 10. Spec: build blocks

> Normative. MUST, SHOULD and MAY follow RFC 2119.
>
> hushrun skips a check that already passed (§7.6). Build blocks are a finer level under a check
> that runs: a crate the compiler already built from the same inputs is not compiled again, in
> this worktree or in another one on the same machine. Nothing here changes what a check's result
> is, and nothing leaves the machine.

A **block** is what one compiler call produced: one crate built one way. The **store** holds
blocks for every repository on the machine. A **root** is a folder a compile's paths start from:
the workspace (rustc's working directory), the package (`CARGO_MANIFEST_DIR`), the build script's
output (`OUT_DIR`), the output folder (`--out-dir`), each dependency search folder
(`-L dependency=`), each native search folder (`-L native=`, `-L all=` or a bare `-L`, named by
its place among the `-L` flags) that is not under a shared folder, and the locations cargo or the
person hand a compile: the target folder `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` names when
it is absolute, `CARGO_TARGET_TMPDIR`, and each folder a `CARGO_BIN_EXE_<name>` value is in (named
by its place in the order of the variables' names). None of them is derived from another root.

**B-1 Mode.** Reuse has one mode per user: `off`, `observe` or `on`. It is `off` until the person
turns it on. In `observe` every compile runs as in `off`, blocks are stored, and a compile whose
block already exists is compared with it. In `on` a compile whose block exists is not run.
`hushrun build on|observe|off` sets it; `HUSHRUN_BUILD` overrides it for one command.

**B-2 The wrapper.** When the mode is not `off`, `hushrun run` sets `RUSTC_WRAPPER` to a link named
`hushrun-rustc` that points at the hushrun binary, in both its forms: `hushrun run NAME -- COMMAND`
sets it for COMMAND, and `hushrun run CHECK…` sets it for each hushrun.yaml check it runs on this
machine, with one session per check that all of the check's attempts share. In `NAME -- COMMAND`
one session serves every start of COMMAND in the run (a test file that runs once more, C-34, starts
it again) and the build `tests: reuse` makes before COMMAND, which is COMMAND's build made first;
it ends after the last start. It MUST NOT set it for an isolated check: that check runs in a
container, where the link and the store don't exist. It MUST NOT set it either when `RUSTC_WRAPPER`
is set already, in CI (C-14's detection), or on Windows. A binary called by that name is the
wrapper and does nothing else; it MUST decide that from its own name, before reading any setting. A
build outside `hushrun run` neither reads nor fills the store. Cargo's own fingerprints don't count
the wrapper, so changing the mode never forces a rebuild.

**B-3 Outcomes.** Every compiler call ends in exactly one outcome, decided by one function from
facts: `reused`, `built and stored`, `built, not stored` (with the reason) or `passed through`
(with the reason). The wrapper's exit code is rustc's, or 0 for `reused`.

**B-4 Passed through.** The wrapper MUST run rustc exactly as asked, storing nothing, when any of
these holds: the first argument is not rustc (clippy's driver); no `--crate-name`; `--print`,
`-vV` or a source of `-`; an `@file` argument; a `--target` that names a file or is not one of
the compiler's built-in targets (B-17); a `--sysroot`; a `-Z` flag other than
`unstable-options`, `threads`, `share-generics`, `macro-backtrace` and `debuginfo-compression`;
an `--extern` with no path, other than `proc_macro`; a `--remap-path-prefix`,
`-Z remap-cwd-prefix` or `-Z remap-path-scope` of the caller's; `-C instrument-coverage` or
`-C profile-generate|use`;
an `--emit` kind other than `dep-info`, `metadata` and `link`; more than one `--crate-type`; a
crate type of `cdylib`, `dylib` or `staticlib`; no `--out-dir` or no `-C extra-filename`; no
`--json` with `artifacts`; the package is on the person's never-store list; the compiler's
identity can't be established (B-17).

**B-5 What was asked.** The first key is the SHA-256 of the JCS form of: a schema name; the
compiler's identity (B-17); the arguments in order, with roots replaced by placeholders (B-8) and
without `--out-dir`, `-L dependency=`, `-C incremental=`, `--error-format`, `--json`, `--color`,
`--diagnostic-width`, and with each `--extern` reduced to its name and options; the variables
cargo sets for a crate's compile, with roots replaced: those whose name starts with
`CARGO_PKG_`, `CARGO_MANIFEST_` or `CARGO_BIN_EXE_`, and `CARGO_CRATE_NAME`, `CARGO_BIN_NAME` and
`CARGO_TARGET_TMPDIR`; the variables the build script set with `cargo:rustc-env`; the variables
rustc reads without an `env-dep` line (`RUSTC_BOOTSTRAP`, `MACOSX_DEPLOYMENT_TARGET`,
`IPHONEOS_DEPLOYMENT_TARGET`, `TVOS_DEPLOYMENT_TARGET`, `WATCHOS_DEPLOYMENT_TARGET`,
`XROS_DEPLOYMENT_TARGET`, `SDKROOT`, `RUSTC_FORCE_RUSTC_VERSION`, `LLVM_OVERRIDE_PRODUCER`,
`RUST_TARGET_PATH`) and those the C driver and the linker read (`LIBRARY_PATH`, `LD_RUN_PATH`,
`DEVELOPER_DIR`, `COMPILER_PATH`, `GCC_EXEC_PREFIX`); `SOURCE_DATE_EPOCH`, which compilers and
build tools take "now" from when it is set (the wrapper never sets it); the person's extra
variables; and what the
wrapper adds to the compile besides the remaps (B-9), with roots replaced, so that a program
compiled `movable` and one compiled `tied` (B-21) never share a key. No other
`CARGO_` variable is keyed: cargo's own configuration (`CARGO_TARGET_DIR`, `CARGO_HOME`,
`CARGO_TERM_*`, `CARGO_PROFILE_*`, tokens) reaches rustc only through arguments already keyed;
the target folder `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` names is a root, so outputs are
searched for it (B-10). `CARGO`, `CARGO_MAKEFLAGS`, `CARGO_PRIMARY_PACKAGE`, `CARGO_SBOM_PATH` and
`CARGO_RUSTC_CURRENT_DIR` are not keyed. It holds no machine, user, branch, commit or time.

**B-6 What was read.** The second key is the SHA-256 of the JCS form of the first key and:

- `reads`: each file rustc's dep-info lists, as a path from its root with its SHA-256, and each
  place a module of a read `.rs` file could also have been, as absent;
- `env`: each `env-dep` line of the dep-info, name and value with roots replaced, or unset;
- `externs`: each `--extern`, by name and the SHA-256 of the file passed. A proc macro whose
  block may move (`anywhere`) is named by the key of the block that produced it, without its
  place; a tied one is another file in each checkout, and is named by its SHA-256;
- `native`: for each `-l`, every name its file may have (`lib<name>.a`, `.dylib`, `.so`, `.tbd`,
  `.rlib`, `<name>.lib`; `<name>.framework`) in every folder rustc searches for it (`-L native=`,
  `all=` and bare ones; `framework=` ones for a framework), in order, by SHA-256 or as absent. A
  file under `OUT_DIR` is in `generated` already. A name found in none of them is a system
  library, covered by the SDK and the linker. A `-l` whose file can't be found with certainty
  (`+verbatim`, a rename or `:file`, a kind this crate doesn't know) is `built, not stored`;
- `native folders`: for a library, every `-L native=`, `all=` and bare folder, whether or not a
  `-l` names anything in it (a `#[link(kind = "static")]` in the source bundles a static library
  found there into the `.rlib`, and nothing reports it): the names of its entries, and each `*.a`
  and `*.lib` directly in it by SHA-256. These folders are usually other packages' `OUT_DIR`s: no
  record is written there, and digests are remembered for the run under its session's folder, by
  path and stat. A linked output (a program, a proc macro, a build-script binary) has none:
  cargo hands it every dependency's folders whether it links anything from them or not, and its
  link line names every library it takes, which `link` keys;
- `shape`: the names of the entries of every folder that directly holds a read file;
- `generated`: every file under `OUT_DIR`, by path and SHA-256;
- `declared`: for a package that is not from a registry or a git source, what its build script
  declared with `rerun-if-changed` (folders in full) and `rerun-if-env-changed`; and the inputs of
  the macros known to read files the compiler doesn't list. A macro that reads the first file of
  a name found walking up the folders (`.env` for sqlx, `dotenvy_macro` and `dotenv_codegen`) has
  each place under a root or a shared folder as a read where it lies, and, from the first folder
  above them all, one entry: the first regular file of that name found walking on up, by SHA-256,
  or none. It is computed from the call's own folders before and after the compile, holds no
  folder above the roots, and so doesn't tie the block (B-8); B-11 applies to each place it
  looked at;
- `link`: for a linked output, every library the linker was handed, by SHA-256; every place the
  linker may take a library named with `-l` from in its `-L` and `-F` folders, and every file an
  option names for it (a linker script, a symbol list), by SHA-256 or as absent; the linker and
  the SDK (B-18);
- `place`: `anywhere`, or the roots themselves (B-10).

**B-7 Hints.** To compute the second key before compiling, the wrapper needs the list of what to
hash. It comes from the lists earlier compiles of the same first key recorded. A list is a hint
and MUST NOT be trusted: the wrapper hashes the files as they are now, and a block exists under a
key only because a compile read exactly those bytes. A file the list names that is gone is
hashed as absent, so the key differs and the compile runs. A list that names a root this call
doesn't have is skipped, and so is the list of a tied block (B-10) recorded with other roots
than this call's: it carries a digest of them and is skipped before anything it names is
opened. The result of a compile is always recorded under the key computed from
what that compile read, never from a hint.

**B-8 Roots.** A path that lies under a root, in the root's given form or its resolved form, is
recorded as the root's name and the rest, and looked up under the current root. A path recorded
under another checkout MUST NOT be opened. Replacing roots in arguments and values MUST be
reversible up to a root's two forms: inputs that differ in anything but which form of a root they
use never give one text. A read at an absolute path outside every
root, the cargo home, the toolchain and the system folders ties the block to its roots. A path
with a `..` is resolved on disk up to its last `..`, and the parts after it are kept as written,
so a symlink at its end is hashed through whatever it points at now; when that resolved part is
not what removing the `..` gives (a linked folder before it), the read can't be placed and the
compile is `built, not stored`.

**B-9 What the wrapper changes in a compile.** For a compile it doesn't pass through, the wrapper
adds `--remap-path-prefix` for the workspace, as `.`, and for the build script's output, as a
fixed stand-in that names the package; each in its given and its resolved form, the workspace
first, with the scope left whole. It leaves `-C incremental` out. On Apple targets it also gives
a proc macro an install name that doesn't hold its folder (`-install_name @rpath/<file>`), and
gives proc macros and build-script binaries `-oso_prefix` for their dependency folder: nobody
debugs those, and without it each holds the folder it was linked in. A program (a `bin` that is
not a build script, or a test) that links with debug info gets a debug bundle instead, unless the
person set `programs` to `tied` (B-21), through three additions: `-C split-debuginfo=packed`, so that rustc runs `dsymutil` and writes
`<program>.dSYM` beside it with the DWARF of every object it was linked from;
`-C strip=debuginfo`, so that the program keeps no debug map naming those objects by their
paths; and `-oso_prefix` for the workspace's resolved form, so that the UUID the linker computes
before the strip holds no path when the target folder is inside the workspace. Before such a
compile the wrapper removes the bundle an earlier compile left, since `dsymutil` adds to one; it
does so too for a program set to `tied` whose flags would allow a bundle, since that bundle
matches no program compiled now and would keep it from being stored (B-18).
After it, it removes the bundle's `Contents/Resources/Relocations`, which names the program by
its absolute path and which no debugger reads. A program set to `tied`, a program whose own
flags say `-C split-debuginfo=off` or a `-C strip` other than `none`, a program without debug
info and a program for any other target are left as they are. It changes nothing else.

No block comes from an incremental compile, and an edit never loses incremental compilation: a
compile that asks for `-C incremental`, finds no block, and already has its dep-info file in the
output folder (so this is a rebuild, not a first build there) is `passed through`, exactly as
asked. A first build there is compiled without `-C incremental` and stored.

**B-10 Place.** After a compile, every output is searched for each root as raw bytes, in its
given and resolved forms, each file of a program's debug bundle among them. None found: the block's place is `anywhere`. Any found, or an output
that can't be searched (`-C lto`, `-C linker-plugin-lto`, `-C embed-bitcode=yes`, compressed debug
sections): the place is the roots, and the block is reused only where they are the same. A
compile that read a root through a variable (an `env-dep` whose value holds a root) is `anywhere`
only if a second compile, with that variable naming a link to the same folder, gives the same
bytes.

**B-11 Changed while compiling.** The wrapper records each read file's size, dates, inode and
device around the compile and around its hashing. A compile is stored only if no read file
changed and none carries a date at or after the compile's start, and no symlink on the way to
one (each folder of its path, the path itself, and what each link points at, up to 40 links)
carries such a date either: a link retargeted during the compile to an older file is new. The
files a hint hashed for the lookup must be as they were then, and the files a link read and the
native files (B-6) are checked the same way. Otherwise the outcome is `built, not stored`. The
start is taken as file dates count it: on Linux, which dates files from a clock that moves once a
tick and lags the real one, it is the date the file system gives a file the wrapper creates in
the output folder just before the compile (for a reuse, as the call begins), so that a file
written a moment before the compile, such as a build script's output, is not taken for one
written during it; where that file can't be made, it is 20 ms before the real start.

**B-12 Reuse.** Each file of the block is cloned (copied where the file system can't clone) under
a temporary name that doesn't begin with `lib`, in the output folder, and renamed into place: a
new file the owner can write. All get one modification time, the wrapper's start. A program's
debug bundle is cloned whole into a temporary folder in the output folder, and that folder is
renamed into place after a bundle already there is moved aside and removed, so that no bundle is
ever there in part. The bundle goes first, then the other files, then the dep-info file, which
is written with this folder's paths. Then the recorded messages are written to stderr with
this folder's paths, the notices that an output is ready last. If any step fails before the first
message is written, rustc runs. Messages are kept per form (`--error-format`, `--json`, colour);
a block whose compile wrote only notices serves every form, and otherwise a missing form is a
build.

**B-13 The store.** `<cache directory>/build`, or `HUSHRUN_BUILD_DIR`. A block is written under
`tmp/`, synced, and published by renaming its folder to `blocks/<two hex digits>/<key>`: the first
writer wins and
a second, identical block is dropped. Nothing rewrites a published block. Each block records the
repository that built it.

**B-14 Expiry.** A reuse sets the block's last use to the command's time; so does its creation.
After a run, at most once a day, `hushrun run` deletes every block whose last use is more than
`keep_days` old (15 unless set; `HUSHRUN_BUILD_KEEP_DAYS` overrides it), by renaming it into
`trash/` and deleting it there, deletes the hints not refreshed for as long, and deletes what is
more than a day old under `tmp/` and `trash/`. The stamp of the sweep is written before anything
is deleted. There is no limit on the
store's size.

**B-15 Suspension.** A first key is `ok` or `suspended`. It is suspended when an audit disagrees
(B-16) or when a dependent fails to load a reused block (rustc's errors E0460, E0461, E0462,
E0463, E0464, E0514 and E0786, never an error in the dependent's own code); in the second case
the reused files are removed from the output folder so that cargo builds them, and one line says
so. Only the externs the error names are acted on: by their name, their crate's name or their
file (E0463's "can't find crate for `x` which `y` depends on" names `x` and `y`); a crate that is
not one of the call's `--extern`s, or one no reuse put in place, changes nothing. Every call that
runs rustc as asked after its arguments were read (`passed through` for a reason found after
reading them, such as the never-store list, a compiler of unknown identity or an incremental rebuild
(B-9), or `built, not stored` because a key, the store or a reuse failed) runs rustc as a child
when a reuse put at least one of its externs in place, with the same arguments, its stderr passed
on line by line and its stdout untouched, so that this rule runs; so does a call whose package
lies outside the shared folders, for its unit record (B-23). Otherwise rustc replaces the
wrapper, and so does it for a call whose arguments can't be read (B-4). A suspended key is never
reused. `hushrun build clean` clears it.

**B-16 Audit.** In `on`, one in fifty of the library compiles that could be reused runs anyway,
chosen by a hash of the key and the run's start. Its outputs are compared byte for byte with the
block's. A difference serves the fresh outputs, suspends the key and is reported. Programs and
proc macros are not compared.

**B-17 The compiler.** The compiler's identity is: the path given, resolved; its size and
modification time; `RUSTUP_HOME` and `RUSTUP_TOOLCHAIN`; under rustup, the size and modification
time of the toolchain's own `rustc` and of its `librustc_driver`; the output of `rustc -vV`; and
the standard library: the names, sizes and modification times of the files in
`<sysroot>/lib/rustlib/<triple>/lib/` for the host and for the call's `--target`. The compiler's
built-in targets are the output of `rustc --print target-list`. `-vV`, the target list, the
standard library's listing and which `librustc_driver` files the toolchain's `lib/` holds are kept
per compiler and target, and computed again when a standard library folder, a `lib/` folder or
one of those files changes. They are kept only when every one of those folders and files, and
the compiler's own files, is dated before what keeps them: a file system may date from a clock
that moves once a tick (Linux), so a folder changed twice in one tick keeps its dates. A rustup
proxy with no `RUSTUP_TOOLCHAIN` is passed through.

**B-18 Programs.** A program's inputs include every library the linker was handed, which rustc
reports with `--print=link-args`; the libraries named with `-l` and the files options name, as
B-6's `link` says; the linker, and the linker `-fuse-ld` names (found as `ld.<name>` or `<name>`
in the `-B` folders, then on `PATH`), each by where it lies (B-8) and, under a root, its SHA-256,
else its size and modification time; and the SDK, by where it lies and the date of its settings
file. A linker or SDK outside every root ties the block. Where that list can't be read, where a file it names is gone or can't be found, or where a
linker script names more files (`INCLUDE`, `INPUT`, `GROUP`, `SEARCH_DIR`, `STARTUP`), the
outcome is `built, not stored`. Every other argument of the link line that is the path of an
existing regular file (an object a `link-arg` names, `-bundle_loader`'s, `-dylib_file`'s), and is
not an output of the compile, is an input by SHA-256. A `-B` folder outside the compiler's
sysroot is `built, not stored`. Files in rustc's own temporary folder are not inputs. What rustc
prints on stdout goes on to the wrapper's stdout, but the linker command. The `dsymutil` rustc
runs for a debug bundle (B-9), the first on `PATH`, is an input like the linker. A program is
stored with its debug bundle: every file under `<program>.dSYM`, named by its path from the
output folder; a bundle with no DWARF file, or that holds anything but folders and regular
files, is `built, not stored`. A program the wrapper leaves as it is (B-9) is stored with the
debug objects it names.

**B-19 Every failure runs rustc.** An error computing a key, reading the store, or putting files
in place MUST end in rustc running as asked with nothing stored. A settings file that can't be
read, a store that can't be written, CI and Windows are `passed through`. A kill at any point
leaves nothing that a later build takes for a block. Where a file is expected, the wrapper opens
only a regular file: a named pipe, a socket or a device is never opened, the compile that reads
one is `built, not stored`, and a hint that names one is skipped.

**B-20 What a run says.** Each compile appends one line to the run's log with its outcome, its
crate and its package. After COMMAND exits, `hushrun run` writes one line: how many crates were
reused and built, and the time reuse saved; in `observe`, how many would have been reused and
how many differed. A call passed through for no `--crate-name`, a probe, no `-C extra-filename`,
no `--out-dir`, an `-o` or no `--json` with `artifacts` is not one of the build's crates and
counts nowhere: those are cargo's probes and the ones a build script runs.

**B-21 Settings.** One file per user, `build.json` beside the credentials, written whole under a
lock: `mode`, `keep_days`, `never_store` (package names), `env` (extra variable names),
`min_free_gb` and `programs`. `programs` is `movable` (unless set) or `tied`: a movable program
gets a debug bundle (B-9), which costs a `dsymutil` run per program and about twice the bytes
stored on a first build and lets a later checkout reuse it; a tied one is compiled as asked and
reused only in the target folder that built it. Any other value makes the file invalid. The
session carries what the file said when `hushrun run` began, so every compile of a run sees one
value. `hushrun build status` shows the mode, the store's folder, how long blocks are kept,
`programs`, the store's size and its blocks by age and by repository. `hushrun build clean`
empties the store.

**B-22 Free space.** Nothing is stored while the store's volume has less than `min_free_gb` free
(10 unless set; 0 turns the rule off). The outcome is `built, not stored`, and the run's line
says so once.

**B-23 What cargo can't see.** Cargo decides whether to call rustc by dates and by the files
rustc listed last time, so some changes never reach the wrapper: a file added to a folder a macro
lists (sqlx's `migrate!` puts each migration in an `include_str!` but lists the folder unseen), a
file a known macro reads (B-6 `declared`), a module that appears where one could have been, a
file whose bytes changed under a date older than the last build (an archive extracted with its
own dates). The keys cover all four; this rule makes them count. Inside `hushrun run`, with the
mode `observe` or `on`, before COMMAND starts, hushrun removes the outputs of every crate whose
inputs changed in one of those ways, so that cargo finds them missing and calls rustc, and the
wrapper answers by content as usual. Nothing is asked of the person.

- *The record.* Every compile the wrapper sees through to a successful end, a reuse among them,
  leaves a record of its unit in its output folder's `.hushrun/units/`, named by the crate and
  its `-C extra-filename`, so that it dies with `cargo clean`: the unit's outputs (what `link`
  and `metadata` produced) by inode, device and size; and what to watch, with what was seen: each
  file the dep-info lists, each place a module of a listed `.rs` file could also have been, the
  folder that holds each listed file, the inputs of the known macros (folders walked in full),
  for a library each native folder and the static libraries in it, and each place a `-l` may be
  found; for a file its size, both dates, inode, device and, when the compile hashed it, its
  SHA-256; for a place, absent; for a folder, its entry names. Nothing under a shared folder
  (B-8) or in the unit's own output folder is watched; a crate with nothing to watch has no
  record, and neither has one whose package lies in a shared folder (a registry or git crate:
  what its build script wrote is cargo's to watch). It is written whole, under a temporary name that begins with a dot, after the outputs
  are in place. A watched file or folder dated at or after the start of the compile (or the
  reuse) means it changed meanwhile: no record is written then.
- *One record or none.* The record is removed before rustc runs and before a reuse puts files in
  place, and written after. A call passed through after its arguments were read runs rustc as a
  child, exactly as asked with its stderr passed on line by line, whenever its package lies
  outside the shared folders, and writes the record from the dep-info rustc left, with stats and
  no digests. A call whose arguments the wrapper doesn't read (a parser's pass-through, no
  session) removes the record of the unit its `--crate-name`, `-C extra-filename` and `--out-dir`
  name before rustc runs. A failed compile leaves none.
- *The registry.* The store keeps one file per output folder the wrapper wrote a record in,
  `out-dirs/<digest of its path>.json`: the folder, every working directory its compiles ran in
  (workspaces may share a target folder), all resolved, and when a run last saw it, written at
  most once per run per folder. The sweep
  (B-14) drops an entry whose folder is gone or that no run saw for `keep_days`.
- *The check.* It takes the registered output folders with a working directory that is the folder
  `hushrun run` runs in, inside it or above it, and for each takes the lock cargo holds on its
  target folder (`.cargo-lock` in the profile folder), skipping a folder a build holds or one
  with no such lock. For each record there: an output that is gone or isn't the file the record
  names means the record no longer describes the unit (plain cargo built it since) and only the
  record is removed. A listed file gone or dated at or after the record's start is one cargo sees
  itself: nothing is done. Otherwise a watched file whose stat differs is changed unless a digest
  on record says its bytes are the same; a place that was absent and isn't, or the reverse, is
  changed; a folder whose stat differs is changed if its names differ. For a changed unit, its
  outputs that are still the files the record names are removed with their records (B-6), then
  the unit record; the dep-info stays, so the rebuild of a crate cargo builds incrementally stays
  incremental (B-9), and a program's debug bundle stays for the next compile to replace (B-9). It
  reads no file but to settle a differing stat, never touches a source file or a file no record
  names, and stops at a deadline (two seconds), leaving the rest for the next run. A record that
  can't be read is ignored.
- *What it says.* For each crate it removed, one line in the run's summary after the totals, at
  most three: `build: <crate> is rebuilt: <what changed>, which cargo doesn't watch`, the path
  from the folder `hushrun run` runs in. When nothing was removed it says nothing.
- *Limits.* Only crates the wrapper saw compiled are checked, and only in the output folders
  registered for the folder `hushrun run` runs in: a script that builds into a folder elsewhere
  is not covered, and neither is a crate last built by plain cargo. A macro that reads a file the
  table of known macros doesn't name, and a file the link line names (B-18), are not watched. A
  name added to a watched folder that a fresh compile would also see as an input (an editor's
  swap file beside a source) rebuilds the crate, as its key says.
