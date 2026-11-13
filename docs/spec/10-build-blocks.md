# 10. Spec: build blocks

> Normative. MUST, SHOULD and MAY follow RFC 2119.
>
> hushrun skips a check that already passed (§7.6). Build blocks are a finer level under a check
> that runs: a crate the compiler already built from the same inputs is not compiled again, in
> this worktree or in another one on the same machine. Nothing here changes what a check's result
> is. Nothing leaves the machine unless the org shares build blocks through its backend and the
> person lets this machine take part (B-25 to B-35): then a portable block (B-24) goes up sealed
> with its own key, and another machine reuses it only after the same checks as a block of its own.

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
`hushrun build on|observe|off` sets it; `HUSHRUN_BUILD` overrides it for one command. Only `off`,
`observe` and `on` are a mode there: `true` and `false`, which `hushrun plan --format env` and
`hushrun artifact resolve` print under the same name for a shell to evaluate, count as unset,
silently; any other value turns reuse off for the run, in one line. *Rests on:* no mode being
named `true` or `false`. *Breaks if:* a person means one of those as a mode: nothing says it was
ignored.

**B-2 The wrapper.** When the mode is not `off`, `hushrun run` sets `RUSTC_WRAPPER` to a link named
`hushrun-rustc` that points at the hushrun binary, in both its forms: `hushrun run NAME -- COMMAND`
sets it for COMMAND, and `hushrun run CHECK…` sets it for each hushrun.yaml check it runs on this
machine, with one session per check that all of the check's attempts share. In `NAME -- COMMAND`
one session serves every start of COMMAND in the run (a test file that runs once more, C-34, starts
it again) and the build `tests: reuse` makes before COMMAND, which is COMMAND's build made first;
it ends after the last start. It MUST NOT set it for an isolated check: that check runs in a
container, where the link and the store don't exist. It MUST NOT set it either when `RUSTC_WRAPPER`
is set already, or on Windows. In CI (C-14's detection) it sets it only when the run's results
lookup carried `Hushrun-Build: on`, the job can be asked as itself (B-31), and the relay's `open`
answers within 5 seconds of its start: then in the job's own store (B-31), and only then does
cargo start. With no answer to the results lookup and a fetch an earlier step of the job saved
that serves the run (B-36), it sets it too, and the run reads that fetch only. With no answer to
the results lookup or to `open` in time otherwise, COMMAND runs unwrapped and the run says one
line, `build reuse is off for this run: the backend didn't answer in time`; a job
whose org doesn't share runs exactly as it would without any of this, and says nothing. A binary called by that name is the
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
`DEVELOPER_DIR`, `COMPILER_PATH`, `GCC_EXEC_PREFIX`), but for a library, which links nothing,
neither those nor `SDKROOT` (rustc reads the SDK only to link; a library's bytes are the same
under two SDKs). *Rests on:* rustc writing a library's `.rlib` and `.rmeta` itself, running no C
driver or linker for it, and taking from the SDK nothing but what links (1.94.0 on macOS: the
same bytes with `SDKROOT` unset, set to two SDK versions, and `DEVELOPER_DIR` naming either;
`MACOSX_DEPLOYMENT_TARGET`, which does change a library, stays keyed). *Breaks if:* a rustc
read the SDK's version or a file under it for a library, or ran a linker to write one.
`SOURCE_DATE_EPOCH`, which compilers and build tools take "now" from when it is set (the
wrapper never sets it), is keyed for every compile, a library's too: rustc doesn't read it, but
a proc macro runs inside rustc and may, and a library's command line doesn't show every macro it
expands (a derive re-exported by another library comes through `-L dependency=`); the person's
extra
variables; and what the
wrapper adds to the compile besides the remaps (B-9), with roots replaced, so that a program
compiled `movable` and one compiled `tied` (B-21) never share a key. No other
`CARGO_` variable is keyed: cargo's own configuration (`CARGO_TARGET_DIR`, `CARGO_HOME`,
`CARGO_TERM_*`, `CARGO_PROFILE_*`, tokens) reaches rustc only through arguments already keyed;
the target folder `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` names is a root, so outputs are
searched for it (B-10). `CARGO`, `CARGO_MAKEFLAGS`, `CARGO_PRIMARY_PACKAGE`, `CARGO_SBOM_PATH` and
`CARGO_RUSTC_CURRENT_DIR` are not keyed. It holds no machine, user, branch, commit or time.

The repository's `build: env:` (B-35) adds its variables to the person's extra variables, for
every compile of the repository. `-C target-cpu=native`, in any spelling (`-C`, `-C` joined,
`--codegen`), whether written on the command line, through `RUSTFLAGS` or cargo's configuration
(which reach rustc as arguments), means the CPU of the machine that compiles. Where it is the
last `-C target-cpu` of a call, for every class of block, libraries included, the first key holds
what the compiler reports for this host in place of the flag's text: the CPU rustc names for
`native` (`rustc --print target-cpus`: "currently `apple-m4`") and the target features `rustc
--print cfg -C target-cpu=native` lists, with the call's `--target`. The compiler is asked once
per compiler identity and target, and the answer kept in a memo beside the compiler's (B-17),
not once per compile. A compiler that can't be asked, or whose answer can't be read, leaves the
flag keyed by its text. Either way the block is never portable (`native-cpu`, B-24): rustc hands
the code generator every feature of the host CPU, more than `--print cfg` shows, so two machines
that agree on the CPU's name and every printed feature can still compile different code, and no
key can say what `native` meant. A `-C llvm-args` whose words give `native` as a value
(`-mcpu=native`) is keyed by its text and not portable the same way. Nothing else in rustc's
flags means "this machine": `-C target-feature` takes named features only (`+native` is an
unknown feature rustc warns about and passes on); `-Z tune-cpu` is a `-Z` flag that passes
through (B-4); and `-C link-arg=-march=native` or `-mcpu=native` reaches only a linked output,
which is never portable (B-24). *Rests on:* a machine having one CPU, so that what the compiler
reports for it tells one machine's `native` from another's within one store, which is all the
key has to do once such a block can't leave the machine; it keeps a store carried to another
machine with its disk from being reused there. *Breaks if:* the machine's CPU changed under one
store (a virtual machine moved to another host) while the memo stays, which the memo's name, the
compiler's identity, doesn't see; or a compile reached `native` through something else than its
arguments (a target specification file is already passed through, B-4).

**B-6 What was read.** The second key is the SHA-256 of the JCS form of the first key and:

- `reads`: each file rustc's dep-info lists, as a path from its root with its SHA-256, and each
  place a module of a read `.rs` file could also have been, as absent;
- `env`: each `env-dep` line of the dep-info, name and value with roots replaced, or unset;
- `externs`: each `--extern`, by name and the SHA-256 of the file passed. A proc macro whose
  block may move (`anywhere`) is named by two things together: what it was built from, the key
  of the block that produced it without its place and without the linker, the SDK and
  `dsymutil`; and what a dependent's compiler reads from it, the SHA-256 of the crate metadata
  rustc put in the linked file (its header, length and the metadata, found as the one place in
  the file where they are), which holds the crate hash a dependent is compiled against and which
  a linker places but doesn't write. A macro built elsewhere by another linker is the same input;
  one whose metadata came out different for any reason gives another key, never a dependent
  that fails to load. The digest is taken from the bytes that are stored, in the read that hashes
them, and the block records it; a reuse names the file by that record. *Rests on:* the code a
dependent's rustc loads from the file and runs, and the metadata it reads, following from what
the macro was built from (its sources, dependencies, flags, compiler and the libraries linked
into it, `link`'s libraries) and not from how the linker laid them out. *Breaks if:* a linker
changed the code a macro runs, or a macro's expansion depended on something its compile wasn't
told about (see B-24 on inputs read without telling rustc). A file in which that
metadata can't be found, and a tied proc macro,
  which is another file in each checkout, are named by their SHA-256;
- `native`: for each `-l`, every name its file may have (`lib<name>.a`, `.dylib`, `.so`, `.tbd`,
  `.rlib`, `<name>.lib`; `<name>.framework`) in every folder rustc searches for it (`-L native=`,
  `all=` and bare ones; `framework=` ones for a framework), in order, by SHA-256 or as absent. A
  file under `OUT_DIR` is in `generated` already. A name found in none of them is a system
  library, covered by the SDK and the linker. A `-l` whose file can't be found with certainty
  (`+verbatim`, a rename or `:file`, a kind this crate doesn't know) is `built, not stored`;
- `native folders`: for a library, every `-L native=`, `all=` and bare folder, whether or not a
  `-l` names anything in it (a `#[link(kind = "static")]` in the source bundles a static library
  found there into the `.rlib`, and nothing reports it): the names of its entries, and by SHA-256
  each static library directly in it, that is each `*.a` and `*.lib` and each other file that is
  an archive (starts with `!<arch>` or `!<thin>`), since `+verbatim` bundles one under any name.
  A library whose `.rlib` holds nothing but rustc's own members bundled no static library, so
  the bytes of those static libraries are not in it: its key names the folders by their entries
  alone, and its hint says so, so that the lookup names them the same way. It holds only its own
  when both hold: every member is the one `lib.rmeta`, which holds rustc's metadata header, or
  an object rustc names `<crate name><extra filename>.<codegen unit>.rcgu.o`, the prefix taken
  from the call's `--crate-name` and `-C extra-filename`; and no member is, byte for byte, a
  member of an archive directly in a folder rustc searched for native libraries (every
  `-L native=`, `-L all=` and bare `-L`), whatever the archive's file name. Only members of a
  size the other side has are hashed, so a large archive beside a library costs its headers. An
  archive or `.rlib` that can't be read counts as bundled. A `-l static` on the command line
  with `+verbatim`, or a name that is a path, is never stored (its file can't be found with
  certainty). *Covers:* accident, which is what it is for: a crate that bundles a static
  library from a folder it was handed, under any file name and any member names, is keyed by
  that library's bytes. *Can't see:* a build script that sets out to fool it. A script can name
  its object `lib.rmeta` or with the dependent's prefix (read from
  `target/<profile>/.fingerprint/`, which cargo makes before build scripts run); those are
  caught by their bytes. But a `#[link(name = "/elsewhere/x.archive", kind = "static",
  modifiers = "+verbatim")]` in source takes an archive from outside every searched folder
  (rustc 1.94.0 does, with an absolute or `..` path), and with forged member names it would pass.
  Such a script already runs with the person's rights and could write the store itself; the
  audit (B-16) is what would catch it. *Rests on:* rustc copying a bundled archive's members
  into the `.rlib` byte for byte, as 1.94.0 does with `-l static`, `+verbatim`,
  `+whole-archive`, incremental, LTO and `-C embed-bitcode`, for each kind of folder (a test
  builds real `.rlib`s that bundle, including objects named as rustc's own and an archive with
  no static library's name, and fails if one comes out as bundling nothing); a static library
  in the sysroot's target folder, which rustc also searches, is in the compiler's identity
  (B-17). *Breaks if:* a rustc rewrote a bundled member while copying it. A compile that
  writes no `.rlib` keys the static libraries' bytes. These folders are usually other packages' `OUT_DIR`s: no
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
  each place under a root or a base as a read where it lies (and each place under a shared folder,
  when the search starts in one), and, from the first folder above them all, one entry: the first
  regular file of that name found walking on up, by SHA-256, or none. A shared folder above a root
  or a base (`/opt` above a cargo home in `/opt/tools/cargo`) is in that entry, as the home folder
  above `~/.cargo` is. It is computed from the call's own folders before and after the compile, holds no
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

Two more folders are **bases**: the cargo home (`CARGO_HOME`, else `~/.cargo`, as `hushrun run`
found it) and the toolchain (the compiler's sysroot, B-17, and its view without sources, B-9).
A path under a base is recorded, like one under a root, as the base's name and the rest
(`{hushrun:cargo-home}/registry/src/…`, `{hushrun:toolchain}/lib/rustlib/…`) in arguments,
keyed variables, reads and hints, so that keys and hints name no machine's folder. Unlike a
root, a base is the same for every checkout of one person: it never ties a block to a
checkout, and B-10 doesn't search for it to decide a place. A block whose bytes or records hold
a base's value is keyed by that value (`bound`, B-24), since two people on one store, or one who
moves the base, have another. *Rests on:* a path under a base meaning the same file in any
install of the same packages (the registry's unpacked sources, the toolchain's files, each read
by content), every compile's paths under the cargo home remapped (B-9), and B-10 finding a base's
value wherever a block holds it. *Breaks if:* a block held a base's value where neither the
search nor the proof of a variable sees it (B-24): such a block is keyed as if the base had one
value, may be marked portable, and is reused on another machine or by another person with the
first machine's value; only the audit (B-16) finds it. Where a
root and a base hold the same path, the root names it.

**B-9 What the wrapper changes in a compile.** For a compile it doesn't pass through, the wrapper
adds `--remap-path-prefix` for the cargo home, as `/hushrun/cargo`, for the workspace, as `.`,
and for the build script's output, as a fixed stand-in that names the package; each in its given
and its resolved form, ordered by length so that of two folders that hold a path the inner one
wins (as B-8 places it), with the scope left whole. A registry or git crate's paths then name no
machine. It leaves `-C incremental` out. On Apple targets it also gives
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
info and a program for any other target are left as they are. When the toolchain holds the
sources of the standard library or of the compiler (`lib/rustlib/src`, rustup's `rust-src`;
`lib/rustlib/rustc-src`, `rustc-dev`), every compile also gets `--sysroot` naming the toolchain's
view without them (B-17): rustc maps the standard library's `/rustc/<commit>/` paths to those
folders when they are there and reads macro text from them, which changes crate hashes,
metadata and debug info, so a machine with them and one without would compile one key two ways.
Paths under the view are paths under the toolchain (B-8, B-11). On such a machine this is
visible to the person, and only for the compiles the wrapper handles (a compile it passes
through, such as an incremental rebuild of a workspace crate, is as without the wrapper): an
error or warning that points into the standard library names `/rustc/<commit>/…` and shows no
source line (and its column may differ by one), as on a machine without the sources; and debug
info names the standard library's files as `/rustc/<commit>/…`, so a debugger needs a source map
to show them. Panic locations and what programs do are the same. Nothing else changes.

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
bytes. In the same pass every output is searched for the folders that name the machine: the cargo
home, the rustup home and the home folder, each in its given and resolved forms, as raw bytes
wherever they lie: whatever comes before or after a hit, it counts. A short folder is therefore
also found in other text (a home `/root` in `/rootfs`, `/roots.rs` or `keyRings/root/`), and
such a library stays on its machine: lost reuse elsewhere, never a block that prints another
machine's folder (`$CARGO_HOME` followed by `_cache` is that folder too). Those don't change the
place, which is about this machine's checkouts; they decide whether the block may leave the
machine (B-24).

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
written during it; where that file can't be made, it is 20 ms before the real start. A path
under the toolchain's view without sources (B-9) is judged by the toolchain's file it leads to:
the view's links are new whenever a run makes them, and the compiler's identity holds the
toolchain by content (B-17).

**B-12 Reuse.** Each file of the block is cloned (copied where the file system can't clone) under
a temporary name that doesn't begin with `lib`, in the output folder, and renamed into place: a
new file the owner can write. All get one modification time, the wrapper's start. A program's
debug bundle is cloned whole into a temporary folder in the output folder, and that folder is
renamed into place after a bundle already there is moved aside and removed, so that no bundle is
ever there in part. The bundle goes first, then the other files, then the dep-info file, which
is written with this folder's paths. Then the recorded messages are written to stderr with
this folder's paths, the notices that an output is ready last. If any step fails before the first
message is written, the call compiles as any compile the wrapper handles does (B-9's changes,
the remaps among them) and stores nothing: as asked, without the remaps, its outputs would hold
this machine's folders and every dependent would get a key no other machine computes. Messages are kept per form (`--error-format`, `--json`, colour);
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
more than a day old under `tmp/` and `trash/`. By the same rule it deletes the compiler memos
written longer ago than `keep_days` (one of an older form is never read again; one in use is
computed again) and each toolchain's view without sources (B-17) whose marker no run dated for
as long, by renaming it into `trash/`; a run dates the marker of each view it uses once. A
view without its marker, and what a maker of one left (a temporary or moved-aside folder, a
lock), go once they are more than a day old. It also puts back the outbox's claims of dead runs
(B-29), removes the folders of dead runs (`incoming/<run>/`, `outbox/tmp/<run>/`,
`relays/<run>/`, B-26), drops a marker whose block is gone or that is older than `keep_days`,
and deletes a suspension with an end once its end has passed (B-15). The stamp of the sweep is
written before anything is deleted. There is no limit on the store's size.

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
proc macros are not compared. `HUSHRUN_BUILD_AUDIT`, read once as `hushrun run` begins, audits
every such compile when it is `every` and none when it is `none`; any other value keeps one in
fifty. `every` is what a replay that compares two machines asks for. `none` removes the one check
that catches two compiles with one key and different bytes, for whoever sets it, CI or not: it is
for tests that expect a reuse, and a run that has it ends with a line saying the audit is off.
The key log records each run's setting.

**B-17 The compiler.** The compiler's identity is its content, so that one toolchain installed
on two machines, under any folder or name, is one compiler: the output of `rustc -vV`; the
SHA-256 of the compiler's own binary (under rustup the toolchain's `bin/rustc`, never the proxy);
the SHA-256 of each compiler library in the toolchain's `lib/` (`librustc_driver-*`, `libLLVM*`),
by name; and the SHA-256 of each file under `<sysroot>/lib/rustlib/<triple>/lib/`, by its path
there, for the host and for the call's `--target`. Whether the toolchain holds sources is not
part of it: no compile sees them (B-9). The view without them is a folder of the store, under
the compiler memos, whose `lib/rustlib/<target>` is a link to the toolchain's for each target
folder it has (each folder of `lib/rustlib/` that holds a `lib/`, but the sources), and nothing
else: that is all rustc reads from a sysroot it is given. It is named by the toolchain's path
and those target folders, so a target installed later gives a new one and nothing else in the
toolchain's folder renames it, and it is made once per store for each such toolchain: as
`hushrun run` begins, for the compiler cargo will call there (`RUSTC`, else `rustc` on `PATH`,
asked `--print sysroot` in the run's folder), so that the wrappers only read it; else by the
first wrapper that needs it. It is whole or absent: a maker fills a folder whose name only it uses, writes a marker
last and renames it into place, removing nothing it did not make. Each call checks the view it
finds with a few `stat`s (the marker, and each target's `lib/` through its link); one that is not
whole is moved aside and made again under a lock on its name. One that can't be made is a
compiler of unknown identity. *Rests on:* a compiler's behaviour following from those bytes and
from no file outside them, and rustc reading from a sysroot nothing but `lib/rustlib/<target>`
and the sources (strace of real builds on rustup's layout and Debian's `/usr`). *Breaks if:* a
toolchain loaded code or data from a file the identity doesn't hash (a plugin elsewhere, a
codegen backend outside `lib/`), or a rustc read another part of the sysroot, which the view
would then lack. The compiler's built-in targets are the output of
`rustc --print target-list`. The content, `-vV` and the target list are kept per compiler and
target in a memo named by the compiler's path, size and dates, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`
and the stamps of the `lib/` folders, and computed again when a standard library folder, a `lib/`
folder or a compiler library changes. A memo is kept only when every one of those folders and
files, and the compiler's own files, is dated before it: a file system may date from a clock that
moves once a tick (Linux), so a folder changed twice in one tick keeps its dates. A rustup proxy
with no `RUSTUP_TOOLCHAIN` is passed through.

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
sysroot is `built, not stored`. Files in rustc's own temporary folder, and the search folder
rustc makes in it (`-L <output folder>/rustcXXXXXX/raw-dylibs` on Linux), are not inputs: what
they hold comes from the compile, and their name is new at each compile. What rustc
prints on stdout goes on to the wrapper's stdout, but the linker command. The `dsymutil` rustc
runs for a debug bundle (B-9), the first on `PATH`, is an input like the linker. A program is
stored with its debug bundle: every file under `<program>.dSYM`, named by its path from the
output folder; a bundle with no DWARF file, or that holds anything but folders and regular
files, is `built, not stored`. A program the wrapper leaves as it is (B-9) is stored with the
debug objects it names.

**B-19 Every failure runs rustc.** An error computing a key or reading the store MUST end in
rustc running as asked with nothing stored; an error putting a block's files in place ends in a
compile with the wrapper's changes and nothing stored (B-12). A call that runs rustc as asked
after its arguments were read first leaves a mark for its unit in its output folder
(`.hushrun/as-asked/<crate><extra filename>`), before rustc starts and so before any output a
dependent could start from; a compile or a reuse of the unit removes it first. A dependent of
a library so marked is not portable (B-24 `extern`). A settings file that can't be
read, a store that can't be written, CI and Windows are `passed through`. A kill at any point
leaves nothing that a later build takes for a block. Where a file is expected, the wrapper opens
only a regular file: a named pipe, a socket or a device is never opened, the compile that reads
one is `built, not stored`, and a hint that names one is skipped.

**B-20 What a run says.** Each compile appends one line to the run's log with its outcome, its
crate and its package. After COMMAND exits, `hushrun run` writes one line: how many crates were
reused and built, and the time reuse saved; in `observe`, how many would have been reused and
how many differed, and in shared `observe` (B-33) how many blocks were compared with the shared
store's copies and how many of those differed. A call passed through for no `--crate-name`, a probe, no `-C extra-filename`,
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
`programs`, the share setting and what sharing does here and why (B-33), the store's size and
its blocks by age and by repository. `hushrun build clean` empties the store. `share` (B-33) is
`on` (unless set), `observe` or `off`; `hushrun build share on|observe|off` saves it, and
`HUSHRUN_BUILD_SHARE` sets it for one command.

**B-22 Free space.** Nothing is stored while the store's volume has less than `min_free_gb` free
(10 unless set; 0 turns the rule off). The outcome is `built, not stored`, and the run's line
says so once. The same floor holds for what sharing writes: no download starts while free space,
less the pack and four times its size for what it unpacks to, would fall below it (B-30), and no
block is packed for upload below it (B-29).

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

**B-24 Portable.** A block is **portable**, and is the only kind that may ever leave the machine
that built it, when nothing in its keys, its hint, its record or its bytes names that machine.
One function decides it, from the compile's facts, and records the answer with every reason
that holds in the block's description and in the run's log. On its own machine a block is
reused whatever the answer, with one condition: a block that holds one of the machine's folders
(the `…-in-output`, `…-in-record` and `variable-names-base` reasons below) has those folders'
values in its second key, as `bound`: each of the cargo home, the rustup home, the home folder
and, for a variable that names it, the toolchain, with its forms on the machine that built it.
Its hint names which folders, never their values, and the lookup fills in this machine's. So
two people with other homes on one store, or one person who moves `CARGO_HOME` or a toolchain,
find another key: each compiles once and stores a block of their own beside the other's, and
neither is refused. *Rests on:* the search (B-10) and the proof of a variable that names a base
finding every folder a block holds. What they see: a folder's text anywhere in an output or a
record, as raw bytes; and the effect on the bytes of a variable the compile reported reading
(an `env-dep`) whose value names a base. What they can't see: a value read without telling
rustc (a proc macro calling `std::env::var("CARGO_HOME")`, or reading a file) and written in
another form (hashed, reversed, split across bytes). Such a block holds a machine's folder with
no reason found: it is marked portable, holds no `bound`, and is reused on another machine or
by another person on a shared store with the first one's value. This is the limit of every
input a compile isn't told about, not of portability alone; the audit (B-16) is the only thing
that catches it, by compiling a reused block again and comparing. The reasons:

- `linked`: a program, a proc macro or a build-script binary. Its linker, C runtime and SDK have
  no content identity yet.
- `tied`: its place is its roots (B-10).
- `read-outside`: a read lies outside every root, base and shared folder.
- `read-in-shared-folder`: a read lies in a shared folder that is not a base (a system folder,
  the rustup home outside the toolchain), whose path is this machine's.
- `cargo-home-in-output`, `rustup-home-in-output`, `home-in-output`: an output holds that folder
  (B-10).
- `cargo-home-in-record`, `rustup-home-in-record`, `home-in-record`: a key, the hint or what the
  block records beside its files (its dep-info, its messages) holds that folder.
- `variable-names-base`: a variable the compile read names a base, and a second compile with it
  naming another folder didn't give the same bytes, or couldn't be made.
- `extern`: a proc macro among its externs is one a dependent on another machine would name
  differently (B-6): it is tied, or what names it holds this machine; or a library among its
  externs was compiled as asked (B-19), without the remaps, so its bytes hold this machine's
  folders and a dependent elsewhere names other bytes. Such a block is no use to another
  machine. *Can't see:* a library built by plain cargo outside `hushrun run`, which leaves no
  mark; its dependents' keys are this machine's alone and may go up, which costs uploads, never
  a wrong block.
- `unkeyed-value`: an output holds, as it is, the value of an environment variable that is in
  neither key (not among the first key's variables, and not named by the dep-info), with a
  value of 8 bytes or more: a name that reads like a secret (E-12's one definition), or any other
  name, less three kinds. A CI system's own: the exact names that GitHub Actions, GitLab,
  Buildkite, CircleCI, Travis, Jenkins, Azure Pipelines, Bitbucket, Drone, TeamCity, AppVeyor or
  CodeBuild documents setting itself (`GITHUB_SHA`, `RUNNER_OS`, `CI_COMMIT_SHA`,
  `BUILD_SOURCEVERSION`, …, listed in `portable::SET_BY_CI`), and only while that system is
  detected (`GITHUB_ACTIONS`, `GITLAB_CI`, `TF_BUILD`, `JENKINS_URL`, … set and not empty); never
  a prefix, so `BUILD_ENV`, `CI_REGION` or `AGENT_POOL`, a project's own settings, are searched in
  CI too, and so is a laptop's `BUILD_NUMBER`. A build tool's (`CARGO`, `RUSTC`, `RUSTUP_`,
  `RUST_`, `OUT_DIR`, `HUSHRUN_`, by prefix). The shell's: `_`, `PWD` and `OLDPWD`, which name
  where a command started. A name that reads like a secret (`GITHUB_TOKEN`, `CI_JOB_TOKEN`) is
  searched whatever its system. A value that holds a root or a folder of the machine is left to those
  rules. The values are looked for in the same pass over each output as the roots, and only
  the variables' names are recorded, in the description and the run's log, which counts the
  blocks held back and their variables; a value is never written anywhere. *Rests on:* a value
  a compile reads without telling rustc (a proc macro calling `std::env::var`) reaching the
  bytes as it is: then anyone who could open the block could read it, and another machine with
  another value would reuse this one's bytes. *Breaks if:* the value is transformed on the way
  (hashed, encoded, split), or is shorter than 8 bytes: a value under 8 bytes (a short region
  or environment name, `prod`, `eu-1`) turns up in outputs by chance too often to be searched,
  is the documented limit of this rule, and only the audit (B-16) and shared `observe` (B-33)
  catch a block built with another such value. CI's own variables are left out because their values are
  the same for every job of a repository and turn up by chance (`https://github.com` is in the
  doc comments of nearly half of the libraries on crates.io); a project whose build reads one
  without telling names it in `build: env:` (B-35). It also breaks if a project gives its
  own setting a name that a detected CI system documents as its own: that value travels.
- `native-cpu`: it was compiled for this machine's CPU, `-C target-cpu=native` or `-mcpu=native`
  in `-C llvm-args` (B-5), whether or not the compiler said what that is here. *Rests on:* rustc
  handing the code generator every feature of the host CPU, which is more than its printed
  words (`--print target-cpus`, `--print cfg`) show; so no key can say what `native` meant, and
  a machine that agrees with them could still be given instructions it can't run. *Breaks if:*
  a compile reaches the host's CPU through something other than its arguments.

A library among its externs is not a reason: it is named by its bytes, so a dependency that isn't
portable gives the block another key on another machine, never a wrong block. A proc macro whose
block is not portable only because it is `linked` is still named alike on two machines (B-6).

## Sharing through a backend

A backend may keep a **shared store** of build blocks for an org, behind every machine's own
store. The backend never sees a key, a path or a crate name: it sees a block's **address**, its
**kind** (`rustc/1`), sealed bytes it can't open, and the scope a caller's credential puts it in.
The protocol is the backend's (`hushrun_core::blocks` holds its types); these rules are the
machine's side.

**B-25 Where a run's blocks may go.** A run's blocks stay on the machine (`local`), go nowhere
(`off`), or are also shared (`shared`). One function decides it, from who runs (a person, a CI
job, an agent token), B-1's mode, the person's share setting (B-33) and what the org said; every
caller asks it again from the same facts and nobody stores the answer:

| Who | Local reuse | Sharing |
|---|---|---|
| A person, local mode `off` | off | off |
| A person signed in, the org shares, own setting not `off` | the local mode | the lowest of the org's mode, the own setting and the local mode |
| A person otherwise | the local mode | off |
| CI with an ID token, the org shares | on, in the job's own store | the org's mode |
| CI with an agent token and no ID token, the org shares | on, in the job's own store | the org's mode, reading only |
| CI otherwise | off | off |
| An agent token, the org shares | the machine's local mode, or on in a sandbox | the org's mode, lowered on a person's machine as for the person |
| An agent token otherwise | the machine's local mode, or off in a sandbox | off |

"The org shares" is read from the run's results lookup, whose answer carries `Hushrun-Build:
on`; only then is `open` asked, so an org that doesn't share pays no request. Build blocks belong
to the repository, not to a scope: a run that can't tell which code it is (several scopes in
`hushrun.yaml` and no `--scope`) makes no results lookup, and still shares, reading the
repository with no scope chosen; its `open` is the one request that asks whether the org shares,
and an org that doesn't share says nothing there either. Only a run that can't read the
repository at all doesn't share, and says so in one line when it has a credential to share
with. In this version a
CI job still passes through (B-2), and only `hushrun run NAME -- COMMAND` shares. *Rests on:*
the backend's header and `open` saying what the org decided, and the machine's settings being
its own. A run never shares more than its local mode allows: with local `observe`, sharing
observes too.

A **sandbox** is a machine with no person's settings file (B-21), as the one function that maps
a run's credential to its row says (`build_session::caller`, for the session, the relay and
`hushrun build status`). An agent token's run there whose org shares (its results lookup said so,
or, with none made, `open` says so within 5 seconds, which the run waits for as a CI job does)
begins a session in the store with reuse on unless `HUSHRUN_BUILD` says otherwise, and its relay
shares as the org's mode says; a run from a saved fetch (B-36) is one too. One whose org doesn't
share runs as it would without any of this, and says nothing. *Rests on:* `build.json` being
written only by `hushrun build`, which a person runs and a sandbox's image doesn't. *Breaks if:* a
person who never ran `hushrun build` runs an agent token on their own machine: it is taken for a
sandbox, reuses in the store and uploads as one.

When `open` answers that sharing is off although the org shares, it gives one reason
(`hushrun_core::blocks::OffReason`): `plan`, the org's plan includes no shared store; or
`contract`, the org's plan sets its shared storage by contract alone and none is set yet. For
`contract` the run says one line, also where `open` decides whether the org shares (which is
otherwise silent), since the org did turn sharing on: `build: sharing is off for this run: the
org's plan sets its shared storage by contract, and none is set yet: get in touch with hushrun to
set it`; it never says to upgrade, since no plan gives that storage. A reason this version doesn't
know reads as unknown, and the run says what it says for an org that doesn't share. *Rests on:*
the backend naming `contract` only for a plan whose storage no plan change gives.

**B-26 The relay, and its deadlines.** The part of the `hushrun run` process that talks to the
shared store for the wrappers is the **relay**, one per process; it sends `open` and `close`
once for each check, since a manifest belongs to a check. The wrappers reach it over a Unix
socket whose path the session names, one request per connection, one line of JSON each way; the
wrapper has no network code. The socket lives in the session's folder, made mode 0700; when that
path is longer than 100 bytes (macOS refuses one over 103), in a new folder of a random name,
mode 0700, under `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp`, each tried when the one before
isn't set, isn't a folder, or would give a path too long, so that a long `$TMPDIR` costs nothing. Before binding, the folder MUST
be a real folder, not a link, owned by this user, with mode 0700; before sending a key, a
wrapper MUST find the socket owned by its own user. A check that fails turns sharing off for the
run with one line. A wrapper waits at most 30 seconds; no socket, no answer, an answer it can't
read and a late answer are a miss, and the compile goes on as without a shared store (B-19).

Cargo never waits for the network on a person's machine: `open` is sent as the run starts, gets
10 seconds and one retry, and until it answers every request is a miss. A fetch a wrapper waits
on has a **cost**: the smallest of what its compile cost on the machine that built it (scaled by
this machine's speed against the uploaders', learned from this run's own compiles whose cost the
server also knew, and taken as equal until it has three), what this machine's own last compile of
the same first key took, and 60 seconds. The **rate** is the link's throughput over all
downloads together, partial and abandoned ones included; with no rate yet, the first fetch a
wrapper asks for runs to its end as the measurement, its wrapper waiting no longer than half the
cost, and every other request is a miss until a rate exists, unless its download is under way
already. One estimate (`State::to_finish`) decides a start and a give-up: a download's own bytes
still to come over the rate, and, while it waits in the queue, those of the downloads running
ahead of it and a round trip. A fetch whose estimate isn't under half the cost is not started
and not waited for; one that is goes to the front of the queue, and it is given up as soon as
elapsed time and the estimate pass half the cost. A miss and a give-up are no failure: nothing
errored, and a parallel build's first small crates give up by the dozen. A **failure** is a
request that errored or timed out: a network error, a 5xx, a lookup or a download that failed
(a download that fails its checks included), and an upload's `begin`, PUT or `commit` that
errored or timed out; a download that arrives ends the row. Three in a row **rest** the relay for
30 seconds, and the run's first rest says so in one line, once: the backend stopped answering
mid-run, and why. While it rests no new request goes to the network (no lookup, hint lookup,
download, `begin`, report; the upload loop claims nothing new), and
every wrapper is answered from what the relay holds: hints the prefetch downloaded, packs
already in `incoming/`, downloads already under way, vouches already known. A 401, 403 or 404 in
the middle of a run stops every request for the rest of the run, keeps the markers (B-29) and
says one line; a server that can't be reached says one line too. Every failure ends in rustc
running as asked. *Rests on:* a wrapper being worth at most half its compile in waiting, so that
the worst a wrong guess adds to one crate is half its compile time; packs already downloaded
costing nothing more to open, so a rest that dropped them would only waste them; and the socket
being no boundary inside one OS user, whose processes can read the machine's key anyway.
*Breaks if:* the link's rate falls after a fetch started: the fetch is given up within half its
cost, and its bytes so far are spent for nothing.

**B-27 Addresses and sealing.** A block's **address** is SHA-256 of `hushrun.block-address/v1`,
a newline, the kind, a newline and the key's 32 bytes; a first key's address is the same of the
first key. A block is sealed with a secret from its own key: HKDF-SHA-256 of the key with a salt
and `hushrun.block-pack/v1`, the salt being HMAC-SHA-256, keyed with the key, of
`hushrun.block-pack-salt/v1` and a newline followed by the SHA-256 of exactly the bytes handed to
the cipher (the compressed stream). ChaCha20-Poly1305 seals it in chunks of 1 MiB, each chunk's
nonce a counter and a last-chunk flag, the pack's header bound to every chunk as associated
data. A hint is sealed the same way with its first key, under `hushrun.block-hint/v1`. *Rests
on:* whoever can open a block being whoever can compute its key, which takes the same sources,
flags and compiler; the salt coming from the bytes, so that the same block packs to the same
pack wherever it is packed and a retry is the same request, and two contents under one key never
share a secret and a counter. It doesn't cover an input the compiler isn't told about, whose
value is in the bytes and not in the key (B-24 `unkeyed-value`, B-35); for a registry crate
anyone can compute the key.

**B-28 The pack.** One block is one object, `hushrun.block-pack/v1`: `HRBP`, the version 1, the
codec 1 (zstd), the cipher 1 and the 32-byte salt; then chunks, each a 4-byte big-endian length
and its ciphertext with its tag. Inside is a zstd stream, made on one thread at level 3, of a
4-byte length, the block's record as canonical JSON, and each file's bytes in the record's order.
The record names the key, the first key, the description and each file's name, size and
SHA-256; it leaves out the repository, the date built and the compile time, which the reader
fills in for its own machine when it publishes. A pack is packed in two passes, held in memory
under 64 MiB and in a file of the run's `outbox/tmp/<run>/` above. The packer checks each file's
size and SHA-256 against the store's record while it reads. The reader checks, in this order, the
pack's size and SHA-256 against the server's row, the cipher's tags (a pack cut short, with no
last chunk, with data after it, or with a chunk over 1 MiB fails), each file's size and SHA-256
against the record, that the record names the key and the first key it was asked for, and that
the **digest of outputs** (SHA-256 of the canonical JSON of the files sorted by name, each its
name, size and SHA-256, and nothing else) is the row's. A pack that fails any check is dropped
and reported `unreadable`. Before it writes any file, the reader refuses a record whose files
come to more than 64 times the pack, or to more than the disk can take above the free-space floor
(B-22), by one rule (`Relay::room_for`) for a download and for shared `observe`'s comparison
(B-33), which writes nothing but would spend as much work; that is a compile, not a report (in
`observe`, no comparison), and each file stops at its declared size whatever the stream holds. A pack over 2 GiB is not shared. *Rests on:* the digest of outputs
being computed again from what was unpacked, so that no comparison rests on the uploader's word;
zstd giving the same stream for one version of it, so two versions of hushrun may give two packs
of one block, which costs a wait and nothing else.

**B-29 Uploads and the outbox.** A block gets a marker in the store's `outbox/` only when it was
built in this run, it is portable (B-24), the policy `open` returned says the caller writes,
sharing is `observe` or `on`, and the store's record says this run's host, org and credential
built it (B-32). A marker an earlier run left goes up only from a run with the same host, org and
credential. A downloaded block, and one with no record, never goes up. An upload's state is where
its marker is: `outbox/<key>.json` (waiting), `outbox/claimed/<run>/<key>.json` (claimed, and
sent once its PUT succeeded, known to the relay that holds the claim), no marker (done). Every
move is a file written whole or a rename:

| From | To | When |
|---|---|---|
| none | waiting | the relay accepts a block (above) |
| waiting | claimed | a relay takes it: a rename, which only one wins |
| claimed | waiting | the run that claimed it is dead: at any relay's start and in B-14's sweep |
| claimed | sent | packed, `begin` answered `put`, and the PUT succeeded or found the object there |
| sent | done | `commit` answered `stored` |
| claimed | done | `begin` answered `have`, or refused `too_large`, `unsupported`, `no_scope` or `suspended`; the block is gone, or no longer matches its record |
| claimed | done, the local block discarded | `begin` refused `differs`: a trusted copy with other bytes exists |
| claimed, sent | waiting, not tried for 10 minutes | a network error, a 5xx, 401, 403, 404, an expired link, `busy`, `over_allowance`, `too_many_uploads`, `commit` answering `missing`, `gone` or `differs`, any word this version doesn't know |
| claimed, sent | waiting, one more cut-off | the end of the command cut off its `begin`, PUT or `commit` in flight |
| waiting | done | three cut-offs |
| any | done | the block is gone, or the marker is older than `keep_days` (B-14) |

`begin` is sent a few blocks at a time, just before their PUTs, signed by the machine; PUTs run 4
at a time, 2 while downloads are in flight; a stored block's newest hints go up with it, sealed
with its first key. When the command ends, no new PUT starts, and everything after the command
has one deadline (`upload::wait_at_end`, the one place that says): on a person's machine 5
seconds for the uploads in flight and 5 for `close`, 10 in all; in CI what B-31 says, `close`
within it. Uploads (the drain in CI, the PUTs, and the `begin` and `commit` requests in flight)
are cut off when `close`'s 5 seconds, or half the deadline if that is less, are all that is
left, and `close` goes within what is left; a request still waited on at the deadline, whatever
its own timeout, is abandoned. The rest waits for the next run, and the run says how many wait.
`commit` answering `differs` means the object store holds a size or SHA-256 other than the one
`begin` named: the upload arrived broken, and the block here stays and goes again later. Only
`begin` refusing `differs` says a trusted copy with other bytes exists, and only then is a local
block discarded; the run says in one line how many it discarded.
*Rests on:* the record of where a block came from (B-32), which keeps a block from going up under
another name or into another org, and undoing a withdrawal; and packs being the same bytes on
every try (B-27), so a retry after a crash finds its upload under way.

**B-30 Prefetch.** `close` sends the run's **manifest**: the ordered pairs of first-key address
and block address of every compile, reused, downloaded or built. `open` returns the newest one
for the repository, the check and the platform. The prefetch starts when the first wrapper of
the run asks the relay anything: a run whose compiles all find their blocks here (a warm target
with an empty store, or no compile at all) downloads nothing. The relay then drops every pair
whose block or first key the store holds, asks for the rest's hints in one request and looks up the rest's blocks in
one, and downloads the libraries, 8 at a time and in order, before cargo asks; programs are
fetched only when asked for. A pack waits sealed in `incoming/<run>/` until a wrapper brings the
key that opens it, and is deleted once opened and published, or when the run ends. A hint is
written where downloaded hints live, apart from the hints compiles here wrote: those are tried
first, and a downloaded hint naming more bytes than twice what this machine's own hints of the
first key name (64 MiB when it has none) is not followed. *Rests on:* a manifest being only
addresses, so a wrong one wastes downloads and can't cause a wrong reuse; and a warm machine
holding what its manifest names, so it downloads nothing.

**B-31 CI and sandboxes.** A CI job's store is the job's own, never the cache directory, and
nothing in it outlives the job. In GitHub Actions it is `hushrun-build-<run>-<attempt>-<digest>`
under the job's temporary folder (`RUNNER_TEMP`), the digest one function makes
(`build_session::github_job`) of the repository (`GITHUB_REPOSITORY_ID`, else
`GITHUB_REPOSITORY`), the run (`GITHUB_RUN_ID`), its attempt (`GITHUB_RUN_ATTEMPT`), the job
(`GITHUB_JOB`) and the job's own request token (`ACTIONS_ID_TOKEN_REQUEST_TOKEN`), which every
step of one job shares and no other job has: so every call of one job finds the same store, and
no other job can make or name it. At its start a job removes every other `hushrun-build*` entry
in its temporary folder, unread. Elsewhere (or with no request token) the store is
`hushrun-build-<process id>` under the system's temporary folder, made by the run itself: one
found there at the start, left by a process that died with the same id, is removed first, and
when it can't be, the run has no store and says so in one line; it is removed when the run's
session ends. In CI who built a block, and so whose markers go up (B-29, B-32), is the job and its
store: the credential's fingerprint (for an ID token, the job's digest) joined with the store's
folder, so that a marker or a block another run left is never this run's, whatever credential it
was made with. No person's settings file is read, reuse is on unless `HUSHRUN_BUILD` says
otherwise, `HUSHRUN_BUILD_DIR` is not followed, and the free-space floor is 2 GB. In GitHub
Actions every build request carries the job's ID token, whose audience is the backend's URL, also
when an agent token is set beside it. One place decides what a request carries
(`Credential::bearer`): a token with more than a minute left is sent; one with less starts a
mint in the background and is still sent until its real expiry, also when that renewal fails;
a 401 gets a fresh one and one more try. At most one mint is in flight, and it gets 5 seconds
whoever waits for it; a request with no token it may send (none yet, the held one expired, or
the one a 401 refused) waits for that mint only within its own deadline, and a mint that fails
or comes later counts, for that request, as a server that can't be reached (B-26). No mint
starts once the end of the command has begun: `close` and the last uploads carry the token held
while it is good, or go unsent. The results lookup whose `Hushrun-Build` answer sets the
wrapper carries the ID token too, before any token the environment holds (C-13), so the answer is
one to the job itself. With no ID token (a pull request from a fork, a job without
`id-token: write`) sharing is off and the run says why in one line, whatever else the
environment holds: an agent token in `HUSHRUN_TOKEN` or `HUSHRUN_AGENT` reads nothing there, and a
results answer obtained with it never sets the wrapper. Anywhere else in CI an agent token reads
the org's blocks and uploads none, whatever the policy says.
Every request, lookups included, names the workspace's commit when one can be read, and in CI
only while the files git tracks are as that commit has them, checked once per run
(`wrap::build_commit`, a `git diff` of the worktree against the commit): a job that changed a
tracked file before building (a version bump, a nightly `cargo update`) names none, which the
backend counts as a commit other than its token's, so it writes no trusted block. A file git
doesn't track, and changes inside a submodule's own checkout, are not compared; a job the
policy calls untrusted says once, in plain words, why it writes nothing to the repository's
trusted blocks. The end of the command waits for uploads as one function of its context says
(`upload::wait_at_end`): a job writing trusted blocks (the scope the policy says it writes is the
repository's trusted one, `protected/…`, and nothing else decides it: `Policy::writes_trusted`; a
pull request's job writes `ci/…` with no reason it isn't trusted, and is not trusted) up to 2
minutes, any other up to 20 seconds,
never longer than a quarter of what the compiles of the blocks still to go took, and both for the
whole `hushrun run` call; meanwhile uploads go on, since the store goes with the job. That is one
deadline for everything after the command, `close` included, which keeps its 5 seconds (B-29):
never less than those, so a job with nothing to upload still sends its manifest. *Rests on:*
the runner emptying its temporary folder when the job ends; the ID token's claims being what the
backend trusts a job by, so a token copied out of one job can do no more than that job could, for
minutes; the commit being what the backend checks a trusted job's token against, and the
tracked files being as it has them; and a quarter of
a block's compile being worth waiting for once, since the next job of the repository then
downloads it rather than compiling; and the request token being the job's alone. *Breaks if:*
a job's build reads a file git doesn't track (one an earlier step generated outside
`.gitignore`): the job names its commit while building other code, and the commit check, a guard
against honest mistakes, doesn't see it (keys still commit to every file read, so no wrong bytes
follow); the token endpoint fails for longer than a token lives (5 minutes on GitHub): from the held
token's expiry, every request is a miss and the job compiles as it would without a shared store,
its uploads lost; a self-hosted runner keeps `RUNNER_TEMP` between jobs and the next job can't remove what the last
one left there (a file it lacks permission to delete): that store is never read or uploaded, but
it takes disk; the legs of one matrix job, which share the run, attempt and job name, run one
after another on one runner and the runner gives them the same request token (then a leg reuses
and uploads what an earlier leg of the same run built, in the same scope); or another process can
read a job's environment, which holds the request token.

**B-36 A sandbox's fetch.** `hushrun build fetch [CHECK]...`, run in a sandbox's setup phase with
the network and a token (an agent's: `HUSHRUN_AGENT`, or an `hra_` token in `HUSHRUN_TOKEN`; with
none it exits naming `HUSHRUN_AGENT`), sends `open` for the repository and platform once for each check named,
by its id as `hushrun run` names it (B-26: a digest of the repository and the check's name), or
once naming no check, which the backend answers with the repository's newest manifest for the
platform, whatever its check (`OpenRequest::check`); an `open` naming a check gets that check's
manifest only. It downloads what the manifests name that the store lacks, and saves into the store's `prefetched/` the sealed packs, the sealed hints, the
policy and the rows, with no token and no link, written whole (a new folder renamed in). It is
good for 24 hours from the time it was fetched, read when it is used; the sweep removes it after.
A fetch serves only a later run of the same host, org and repository (the run's backend, as the
`HUSHRUN_BACKEND` variable, the repository's files or a login name it, and its repository, as
B-26 names it), and only while that backend answered no request of the run: an answer always wins
over a fetch, that the org doesn't share, a 401 or a 403 included, and a run that names no backend
or no repository, or a fetch that names none, is no match (`saved::Saved::serves`, the one
definition). Such a run whose store holds a fresh fetch runs a relay from it: the policy as
saved, each pack linked into the run's `incoming/` and opened when a wrapper brings its key,
the hints as saved; it uploads nothing, reports nothing and sends no request, and so leaves no
outbox marker for what it builds and says nothing about uploads (as any run that uploads nothing,
such as an agent token's in CI). In CI the fetch is
saved in the job's own store (B-31), which only a GitHub Actions job with an ID token has, so that
a later step of the same job finds it; elsewhere in CI `hushrun build fetch` refuses, since each
run's store there is its own. A step of that job whose backend answered nothing, and whose fetch
serves it as above, gets the wrapper and reads the fetch only, in place of the one line B-2 says;
this is where a coding agent that runs on a GitHub Actions runner behind a firewall is served. No
run deletes the fetch. A machine with no person's
settings file is a sandbox: reuse is on there, and sharing is the org's mode as saved. *Rests on:*
a sandbox's later phases running the same repository and toolchain as its setup, so the keys
they compute are the ones the fetch's manifest names; and the packs staying sealed with each
block's key, so the fetch holds nothing a compile here couldn't make. *Breaks if:* a block the
server withdrew after the fetch is reused within its 24 hours, as B-34 allows any run that can
reach no server; or the org turns sharing off while the backend can't be reached, which a run
there learns only when it can. A network that refuses the backend with an answer of its own (a
proxy's 403) counts as the backend's answer, and the fetch then serves nothing: a cost, not a
wrong block. A sandbox whose later phases have the network and no token gets a 401, and so
nothing from its fetch, for the same reason.

**B-32 Where a block came from.** Beside each block the store keeps a record, `origin.json`,
written before the block is published and rewritten whole under the block's lock when it
changes: **built here** (the run, and with a backend its host, the org and a fingerprint of the
credential, never the secret); **vouched for** (one entry per host, org and repository whose
server answered for the block, with the scope, the digest of outputs, the server's date and its
word: `served`, or `withdrawn` once it said it no longer serves it); and whether it has been
reused here yet. How a block stands for a reuse (any, fresh, stale or withdrawn) is one function
of that record (`Origin::standing`), which the wrapper and the relay both call. A download makes the first entry. A
block with no record, as every block from before this rule, is reused here as before and never
goes up. The relay publishes a download only to answer a wrapper still waiting for it, through
the store's own publish (first writer wins), never in shared `observe`. A block that was only
downloaded may give way to this run's download or compile; a block built here never does. A
compile whose own publish finds a block there compares outputs: a block built here is a second
opinion for a library, and a difference is treated as B-16 treats one; a block only downloaded
is replaced where the two differ, its first key suspended as B-34 says, and the difference
reported. The replacement is one step under the block's lock (the two folders exchange names), so
no other publish lands between the old block leaving and the new one arriving; where the
filesystem can't exchange, the old one stays, and the suspension makes that a compile. Either way it is from then on also
built here. A download of a block already there, only downloaded, with the same outputs (two
runs on one store that fetched it at once) leaves the block's folder where it is, since a reuse
may be copying from it, and adds this run's vouch to its record. *Rests on:* one store serving every org, credential and agent of one OS user, so that
only the record tells a block one run may send from one it may not.

**B-33 The shared mode, apart from the local one.** B-1's mode is reuse on this machine. Sharing
has its own mode, `observe` or `on`, the org's, set by its admin and returned by `open`; a person
may lower it for the machine (`hushrun build share on|observe|off`, `HUSHRUN_BUILD_SHARE` for
one command) and never raise it. In shared `observe` a machine keeps all its local reuse, uploads
what it builds, never waits for or publishes a download, and compares: after a compile stores a
library, the relay downloads the shared store's copy in the background, opens it within the room
a download gets (B-28's 64 times the pack and the disk's floor), and compares its files with the
local block's. A difference suspends the first key here for `keep_days` and is
reported `differs` to the scope the copy came from. `close` counts the comparisons, those of
blocks another machine wrote, and those that differed; comparisons not finished when the command
ends are dropped. Each compared block is downloaded once, by its comparison: there is no
prefetch (B-30) and no downloaded hint in shared `observe`, since neither leads anywhere but to a
download no wrapper may reuse. The run's summary says how many blocks were compared and how many
differed (B-20). In `on` a download is reused. *Rests on:* one week of observing being free of
cost to local reuse, and a comparison being of another machine's bytes, which only `close`'s
counts can show.

**B-34 A downloaded block is reused only while a server the run belongs to vouches for it.** A
block built here is reused as any. A block only downloaded is never reused as a download again
once any server said it no longer serves it: its record keeps that "no" (B-32), whatever the
network does afterwards, and a run with no relay, which asks nothing, honours it too. Otherwise
it is reused without asking while its entry for this run's host, org and repository is under 24
hours old; else the wrapper asks the relay, which answers within the block's **budget**, half
what its compile took (B-26). "Served, with these outputs" lets it be reused. **The server can't
be reached** means `open` failed, or hasn't answered within that budget: only then does an entry
of any age count, and the block is reused. Once `open` has answered, a stale entry is never
enough: the wrapper waits for the server's answer within its budget and, without one (a lookup
slower than that, or one that failed), it is a miss: rustc runs, and the block that compile
builds takes the downloaded one's place and is from then on built here (B-32). The relay asks
the server, without fetching, about every block in the store only downloaded whose entry for
this run's host, org and repository is stale, as the store's records list them, as soon as
`open` answers (`Relay::ask_vouches`), in as few lookups as the lookup's limit allows; one runs
at a time, and a question about a block it didn't name waits for it to end and then starts the
next. At the end of a check it asks again, within the uploads' cut, about every downloaded block
the check reused that no lookup of the run answered for, so that a machine in daily use never
goes stale. Every answer is written into the block's record: served with its outputs renews the
entry, anything else is a "no". On a person's machine a downloaded library's first reuse is
audited one time in ten, which the record notes, then one in fifty as any (B-16). A difference
found by an audit or a comparison of a downloaded block, and a dependent that can't load one
(B-15), suspend its first key for `keep_days`, not until `hushrun build clean`, and are reported
to the scope the record names: `differs`, `rejected`, or `unreadable` for a pack that failed its
checks. *Rests on:* the server answering a lookup only for blocks it still serves to this
caller, so that a withdrawn block stops being reused on laptops within a day without a list of
withdrawals, and a "no" from it being final for that copy; a lookup without fetching counting as
a use, so blocks in daily use don't expire (a stale block the run doesn't reuse, asked about with
the rest, counts as used too); and cargo never waiting on the network longer than the compile it
may save. *Breaks if:* `open` takes longer than a block's budget: that block is reused as on a
machine that can't reach the server, until a run whose `open` answers in time; or a server says
"no" by mistake (a lookup answered for the wrong repository): the block compiles here from then
on, a cost and never a wrong reuse.

**B-35 `build: env:`.** A repository's `hushrun.yaml` may name variables under a top-level
`build: env:` list; every compile of that repository is keyed by their values (B-5), on every
machine and in both forms of `hushrun run`, and the list is part of no check's definition and no
run's key. It is for a build that reads a variable without telling the compiler (a proc macro
calling `std::env::var`), whose value would otherwise be in a block's bytes and not in its key.
*Rests on:* the project knowing which variables its build reads; a variable it reads without
naming keeps the block on this machine only when it looks like a secret (B-24), and is otherwise
caught only by the audit (B-16).
