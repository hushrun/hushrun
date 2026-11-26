# 10. Spec: build blocks

> Normative. MUST, SHOULD and MAY follow RFC 2119.
>
> hushrun skips a check that already passed (§7.6). Build blocks are a finer level under a check
> that runs: a crate the compiler already built from the same inputs is not compiled again, in
> this worktree or in another one on the same machine. For Go (B-39 to B-47), a package archive
> another machine compiled is reused through Go's own cache program, beside Go's own cache. For C
> and C++ (B-50 to B-62, a draft), an object is reused through CMake's compiler launcher; for
> TypeScript (B-100 to B-109, a draft), what `tsc` wrote and printed, through a hook in its Node
> process. Nothing here changes what a check's result
> is. Nothing leaves the machine unless the org shares build blocks through its backend and the
> person lets this machine take part (B-25 to B-35): then a portable block (B-24) goes up sealed
> with its own key, and another machine reuses it only after the same checks as a block of its own.
> A baked store (B-63, B-64) carries the same sealed packs in an image or a read-only volume, read
> underneath a run's own store.

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
is set already (sccache, for one), or on Windows: the other wrapper stays, COMMAND runs as it
would without hushrun's reuse, and where the run's results lookup said the org shares
(`Hushrun-Build: on`) the run says one line, `build reuse is off for this run: another compiler
wrapper is set (RUSTC_WRAPPER)`; elsewhere it says nothing. In CI (C-14's detection) it sets it only when the run's results
lookup carried `Hushrun-Build: on`, the job can be asked as itself (B-31), and the relay's `open`
answers, however long that takes (ADR 0144: a server silent for 30 seconds, twice, has failed):
then in the job's own store (B-31), and only then does cargo start. In CI only `NAME -- COMMAND` sets it (B-25); `CHECK…` never does there. With no
answer to the results lookup and a fetch an earlier step of the job saved
that serves the run (B-36), it sets it too, and the run reads that fetch only. With no answer to
the results lookup or with `open` failed otherwise, COMMAND runs unwrapped and the run says one
line, `build reuse is off for this run: the backend didn't answer`; a job
whose org doesn't share runs exactly as it would without any of this, and says nothing, but for
`open`'s reason `contract`, which it says in B-25's one line. A job with no backend named
(B-25's "CI otherwise"; `HUSHRUN_BACKEND=""`, or nothing names one) keeps no compiled code in
any store, and when the job asked for reuse (`HUSHRUN_BUILD` is `on` or `observe`) the run says
so in one line, `build reuse is off for this run: in CI hushrun keeps compiled code only in the
job's own store, for a backend that shares it, and nothing names one here (hushrun.yaml's
`backend`, or HUSHRUN_BACKEND)`; `hushrun build status` says the same reason on its `ci` line,
and a bake of the job's store refuses with it (B-63); a job that asked nothing hears nothing
(`build_session::ci_without_backend`, the one test and the one text). A binary called by that name is the
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
`DEVELOPER_DIR`, `COMPILER_PATH`, `GCC_EXEC_PREFIX`, and for a linked output every variable whose
name starts with `LD_`, `RC_` or `COLLECT_`, and `LDEMULATION` and `GNUTARGET`), but for a library,
which links nothing, neither those nor `SDKROOT` (rustc reads the SDK only to link; a library's
bytes are the same under two SDKs). The prefixes are what the linkers read from their own
environment without a word on the link line: Apple's `ld` (Xcode 27.2, names found in its binary)
changes every output's UUID with `RC_UUID_SALT` and a dynamic library's header with
`LD_APPLICATION_EXTENSION_SAFE` and `LD_NO_ENCRYPT`; GNU `ld` reads `LD_RUN_PATH`, `LDEMULATION`
and `GNUTARGET`, and `collect2` its `COLLECT_` ones. A prefix, not a list, because a linker update
can add one and nobody reads its binary again; so `LD_LIBRARY_PATH`, `LD_PRELOAD` and `LD_AUDIT`,
which change the libraries the linker programs themselves load, are keyed too, with roots
replaced (on Linux cargo sets `LD_LIBRARY_PATH` for rustc to the target folder's `deps`, and
rustup adds the toolchain's `lib`: both roots or bases, so the text is the same in every checkout).
*Rests on:* rustc writing a library's `.rlib` and `.rmeta` itself, running no C
driver or linker for it, and taking from the SDK nothing but what links (1.94.0 on macOS: the
same bytes with `SDKROOT` unset, set to two SDK versions, and `DEVELOPER_DIR` naming either;
`MACOSX_DEPLOYMENT_TARGET`, which does change a library, stays keyed); and the linkers reading
their own variables under those names (a test links a proc macro with and without
`LD_APPLICATION_EXTENSION_SAFE` and a program with and without `RC_UUID_SALT` on macOS: other bytes,
other keys). *Breaks if:* a rustc read the SDK's version or a file under it for a library, or ran
a linker to write one; or a linker read a variable under another name.
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
- `externs`: each `--extern`, by name and the SHA-256 of the file passed. A proc macro whose block
  may move (`anywhere`) is named by two things together: what it was built from, the key of the
  block that produced it without its place and without E (B-37), the linker, the SDK, `dsymutil`
  and the C runtime's places (`link`); and what a dependent's compiler reads from it, the SHA-256 of the crate
  metadata rustc put in the linked file (its header, length and the metadata, found as the one place
  in the file where they are), which holds the crate hash a dependent is compiled against and which
  a linker places but doesn't write. A macro built elsewhere by another linker is the same input;
  one whose metadata came out different for any reason gives another key, never a dependent that
  fails to load. The digest is taken from the bytes that are stored, in the read that hashes
them, and the block records it; a reuse names the file by that record. *Rests on:* the code a
dependent's rustc loads from the file and runs, and the metadata it reads, following from what
the macro was built from (its sources, dependencies, flags, compiler and the libraries linked
into it, `link`'s libraries) and not from how the linker laid them out. *Breaks if:* a linker
changed the code a macro runs, or a macro's expansion depended on something its compile wasn't
told about (see B-24 on inputs read without telling rustc). A file in which that
metadata can't be found, and a tied proc macro,
  which is another file in each checkout, are named by their SHA-256. **A record in `target/` is
  a note for speed, never an authority.** The record the wrapper leaves beside each output
  (`<out dir>/.hushrun/<file>.json`: its SHA-256, the block it was stored as or reused from,
  what dependents name it by, whether that name is portable) saves dependents a read; its
  SHA-256 answers while the file's size, modification time, inode and device are the recorded
  ones, and only when the record is sealed with the store's seal: a digest of the record with
  32 random bytes the store keeps in `seal` at its root, made once by the first wrapper that
  finds none and never written anywhere else. Any other record (from another store, from before
  seals, or one whose stat was foreseen and planted) has its file hashed first, and is then kept
  and sealed only when the file's bytes are the ones it names. What it says of the block is believed only when the run's own store confirms it: the
  store holds the block the record names, and that block's `block.json` lists a file of this
  name with this size and SHA-256; what dependents name the file by and whether that is portable
  are then taken from the block's description, never from the record. One function decides it
  (`sidecar::vouched`), with one store lookup per proc macro among a compile's externs whose
  record claims a name. Any other proc macro is named by its SHA-256 and is not named portably
  (B-24 `extern`): it still builds, and a dependent is still stored and reused here, by those
  bytes. So a `target/` folder restored from a CI cache, copied from another machine, or left
  by a block since expired gives its dependents keys of their own and nothing built against it
  leaves the machine for that reason; in CI the store starts empty with the job (B-31), so a
  restored `target/` proves nothing there and a job shares only what it compiled itself. A
  compile that stores nothing (low space, a suspended first key, a failed reuse) leaves a record
  naming no block, and its dependents name its proc macros by their bytes too. *Rests on:* the
  store being the run's own (the person's, or the job's in CI), which nobody but the person and
  the code they run writes, and each stored file's SHA-256 recorded when it was published; and
  the seal being secret from whoever wrote the `target/` folder (in CI it is made with the job's
  store). *Breaks if:* someone who can read the store's `seal` writes records into a
  `target/` folder this store's runs read, which is someone who could write the store itself;
- `native`: for each `-l`, every name its file may have (`lib<name>.a`, `.dylib`, `.so`, `.tbd`,
  `.rlib`, `<name>.lib`; `<name>.framework`) in every folder rustc searches for it (`-L native=`,
  `all=` and bare ones; `framework=` ones for a framework), in order, by SHA-256 or as absent. A
  file under `OUT_DIR` is in `generated` already. A name found in none of them reaches only a
  link, where `link` keys every place the linker may take it from (B-18); a library's compile
  reads nothing of it. A `-l` whose file can't be found with certainty
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
  linker may take a library named with `-l` from, in its `-L` and `-F` folders and then in the
  folders the C driver hands `ld` and those `ld` searches by itself (B-18), with each file a
  linker script found there names, and every file an option names for it (a linker script, a
  symbol list), by SHA-256 or as absent; those folders of the driver's and `ld`'s, in order; and
  the link environment's identity E when it is known (B-37), else the C driver, the `ld` it runs
  and the SDK by where they lie and their dates (B-18). The places in the driver's and `ld`'s folders of
  the C runtime the standard library asks for (`-lc`, `-lm`, `-lrt`, `-lpthread`, `-ldl`,
  `-lutil`, `-lgcc_s`, `-lgcc`, `-lgcc_eh`, `-lSystem`), with what their scripts name, and the
  folders themselves are, like E, the linker and the SDK, left out of what a dependent names a
  movable proc macro by; any other library found there, which a crate asked for, is in it. *Rests
  on:* the standard library calling the C runtime for services whose answer, for one input,
  doesn't follow its version. *Breaks if:* a macro's expansion depended on the C library's version
  (a locale, a time zone): B-24's inputs not told;
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
[ADR 0095] A Cargo test binary's unit key (C-31) replaces roots in the values of its `env-dep`
lines the same way, with roots of its own: the repository's root as `workspace`, Cargo's target
folder and its build folder both as `target-dir` (a key is never read back, so two folders may
share a name), the cargo home and the toolchain as bases, the rustup home as `rustup-home` and
the home folder as `home`. `rustup-home` and `home` are roots no compile has. A folder two of
them share is named by the first of `target-dir`, `workspace`, `toolchain`, `rustup-home`,
`cargo-home`, `home`: the home folder below every other. [ADR 0096] The same roots replace those
folders in what a binary's build scripts gave it (C-31's `scripts`: their output's lines and the
paths of the files they generated), and a Go package's unit key replaces the repository's root and
the home folder alone, with those names, in the values of the variables its tests read. No root
or base is added, so a compile's roots and keys are as they were.

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
decides what becomes of a proc macro's and a build-script binary's debug map, which names every
object the link took by its path, the standard library's by where this machine keeps the
toolchain:

- **In a run that shares** (the shared mode is `observe` or `on`: the org shares, someone is
  signed in, and the person didn't turn sharing off; whether or not the backend answered, so
  the key doesn't turn with the network) they are linked **without the map** (`-Wl,-S`). With
  it none could be the same on two machines, so this is what lets them travel. What it costs:
  a panic inside one still prints its message, its location (`build.rs:1:64`) and every
  frame's function name, and no frame has a file and line any more, and a debugger has
  nothing to step through, where the script or macro was compiled with debug info at all
  (cargo's dev profile compiles them without; measured on a build script with `debuginfo=2`:
  14 of 14 frames had a file and line before, none after).
- **In a run that shares nothing** (sharing off for the org or on this machine, nobody signed
  in, a command that never shares) the map **stays**, with the folder it was linked in taken
  off its names (`-oso_prefix <the first dependency folder>/`), as before linked outputs
  travelled: local debugging keeps file and line, two checkouts on one machine still link the
  same bytes, and the block stays on the machine because it names the toolchain.

The two are different compiles: what the wrapper adds is in the first key (B-5), so a block of
one kind is never reused for the other. **To keep the map while the org shares**: turn sharing
off on this machine (`hushrun build share off`, B-33), or name the package under `never_store`
(B-4), which passes all of that package's compiles through untouched.
A program (a `bin` that is
not a build script, or a test) that links with debug info gets a debug bundle instead, in a run
that shares (the same test as above: the shared mode is `observe` or `on`, whether or not the
backend answered) and unless the person set `programs` to `tied` (B-21). In a run that shares
nothing a program is tied: compiled as asked, with nothing added, and reused only in the target
folder that built it. The bundle costs a `dsymutil` run per program (measured on this
repository's first build on macOS: 49.9 s with no cache, 52.4 s with programs tied, 76.3 s
with a bundle each) and pays only where a program's block may leave the machine (B-24). One
function decides which a run's programs are, from the setting and whether the run shares, for
the wrapper and for `hushrun build status`. A program gets its bundle through four additions: `-C split-debuginfo=packed`, so that rustc runs `dsymutil` and writes
`<program>.dSYM` beside it with the DWARF of every object it was linked from;
`-C strip=debuginfo`, so that the program keeps no debug map naming those objects by their
paths; `-oso_prefix` for the workspace, in the form the output folder is written with (as cargo
gave it, or resolved), so that the UUID the linker computes before the strip holds no path of
the checkout when the output folder is inside the workspace; and `--sysroot` naming a link
beside the outputs, `<output folder>/.hushrun/sysroot-<the compiler's identity, 16 hex
digits>`, that leads to the toolchain (to its view without sources where there is one, below),
so that the linker, which names each object by the path it is given, names the standard
library's objects from the output folder too and the UUID is the same wherever the toolchain is
installed. The link is one per toolchain, so two toolchains building into one target folder
never share it; a call that finds none makes it in place, which replaces nothing, so that the
compiles that start at once and all find it missing make it once and a link that is right is
never renamed over (on macOS 26 a readlink racing a rename over its link fails now and then); a
call that finds it leading elsewhere makes it again under a name of its own and renames it into
place, before the compile; paths under it are paths under the toolchain
(B-8, B-11). The name is the wrapper's own and is only ever a link: a folder found in its place
(a target folder copied by a tool that followed the link, something planted) would be the
sysroot rustc reads, so it is removed with what it holds and the link made; a file there is
replaced by the rename; and a link under a temporary name that a process left when it died
between making and renaming it is removed, once ten minutes old, by the next call that makes
the link. Where the output folder lies from the workspace is part of every name the linker
hashes, so it is in the first key: its path from the workspace when it is inside, and when it
is outside (`CARGO_TARGET_DIR` elsewhere, a workspace whose path holds a comma) the folder as
this machine has it, which ties the program to its place (`tied`, B-24): two target folders
build one program each. On every target, a linked output compiled with rustc's own run paths
(`-C rpath`, the last one not switched off) holds the way from its output folder to the
toolchain's and its dependencies' folders, written relative to itself (`$ORIGIN/../../…`,
`@loader_path/../../…`): no root's name is in it, the search of the outputs (B-10) can't see
it, and it differs between two checkouts at another depth. Its first key therefore holds the
output folder, each `-L dependency=` folder (each as given and as it resolves) and the
toolchain's folder as this machine has them, and it is `tied`: reused only where those are the
same text, on this machine or, from CI, on another (B-24's `bound`). Keying the folders was
chosen over keeping such a block out of the store: two checkouts on one machine must not share
it either, and CI jobs, whose paths are the same, still do. A run path the link line gives as
text (`-C link-arg=-Wl,-rpath,…`) is an argument like any other, in the first key as written.
*Rests on:* Apple's `ld` naming an input by the path it was given
(shown: a toolchain copied to another path links the same program, bundle and UUID through the
link, and other ones without it). *Breaks if:* a later `ld` resolved links in the names. Before such a
compile the wrapper removes the bundle an earlier compile left, since `dsymutil` adds to one; it
does so too for a tied program (set to `tied`, or of a run that shares nothing) whose flags
would allow a bundle, since that bundle
matches no program compiled now and would keep it from being stored (B-18).
After it, it removes the bundle's `Contents/Resources/Relocations`, which names the program by
its absolute path and which no debugger reads. A tied program, a program whose own
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
sections): the place is the roots, and the block is reused only where they are the same. One
that can't be searched also never leaves its machine (`unsearchable`, B-24). A
compile that read a root through a variable (an `env-dep` whose value holds a root) is `anywhere`
only if a second compile, with that variable naming a link to the same folder, gives the same
bytes. In the same pass every output is searched for the folders that name the machine: the cargo
home, the rustup home and the home folder, each in its given and resolved forms, as raw bytes
wherever they lie: whatever comes before or after a hit, it counts. A linked output is also
searched for the toolchain's own folder, `<sysroot>/lib/rustlib`, in the toolchain's given and
resolved forms and through its view without sources (B-9): a toolchain outside the rustup home
and the home folder (a distribution's `/usr`, a Nix store path, `/opt`) names the machine's
install as well, and a link can write it (macOS's debug map names the standard library's objects
by it); `toolchain-in-output` (B-24). A library is not searched for it: rustc remaps the
standard library's paths to `/rustc/<commit>/` in what it writes itself. A short folder is therefore
also found in other text (a home `/root` in `/rootfs`, `/roots.rs` or `keyRings/root/`), and
such a library stays on its machine: lost reuse elsewhere, never a block that prints another
machine's folder (`$CARGO_HOME` followed by `_cache` is that folder too). One text is not: a hit
that lies inside a stand-in B-9 writes (`/hushrun/cargo`, a build script output's), as part of the
stand-in's own text, is the stand-in, the same on every machine, and counts for no root, folder,
toolchain or variable's value, in an output or in a record: a cargo home of `/cargo` would
otherwise be found in every registry crate's `/hushrun/cargo/…` paths, and none could leave the
machine. Nor is a hit that starts a longer name in a path running on from a stand-in: the byte after
it is a name's (not `/`, a terminator or the end), and every byte back to an occurrence of the
stand-in, within 4096, is a path's (letters, digits, `/ . _ - + @ ~`), as a checkout of `/app` sits
in `/hushrun/cargo/registry/src/…/approx-0.5.1/…` (`scan::Needles::not_continuing`). A root's own
contents start with `/` and the root alone ends there, so neither is passed over; nor is one in
binary data whose length or terminator precedes it (rmeta's length-prefixed strings), which breaks
the run. A stand-in that is a folder's own text is that folder. Those don't change the
place, which is about this machine's checkouts; they decide whether the block may leave the
machine (B-24).

**B-11 Changed while compiling.** The wrapper records each read file's size, dates, inode and
device around the compile and around its hashing. A compile is stored only if no read file
changed and none carries a date at or after the compile's start, and no symlink on the way to
one (each folder of its path, the path itself, and what each link points at, up to 40 links)
carries such a date either: a link retargeted during the compile to an older file is new. The
files a hint hashed for the lookup must be as they were then, and the files a link read, every
file its link environment hashed (B-37) and the native files (B-6) are checked the same way. Otherwise the outcome is `built, not stored`. The
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
new file the owner can write. The linked output of a program, a build script or a proc macro is
also given a run bit wherever it has a read bit, whatever mode the store holds: a pack keeps no
file mode (B-28), and a block an older hushrun downloaded is stored without them. All get one
modification time, the wrapper's start. A program's
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

**B-13 The store.** `<cache directory>/build`, or `HUSHRUN_BUILD_DIR`. Its root holds `seal`, the
secret that seals the records beside outputs (B-6); `hushrun build clean` keeps it. A block is written under
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
and deletes a suspension with an end once its end has passed (B-15). It deletes the manifests
runs left (B-63) once older than `keep_days`, never a mark that a baked store was replaced (a
store with another manifest is another store, so a mark names one store for good, and a replaced
store must not turn current again with time), and never touches a baked store read underneath
(B-64). The stamp of the sweep is
written before anything is deleted. There is no limit on the store's size.

**B-15 Suspension.** A first key is `ok` or `suspended`. It is suspended when an audit disagrees
(B-16) or when a dependent fails to load a reused block (rustc's errors E0460, E0461, E0462,
E0463, E0464, E0514 and E0786, never an error in the dependent's own code); in the second case
the reused files are removed from the output folder so that cargo builds them, and one line says
so. Only the externs the error names are acted on: by their name, their crate's name or their
file (E0463's "can't find crate for `x` which `y` depends on" names `x` and `y`); a crate that is
not one of the call's `--extern`s, or one no reuse put in place, changes nothing. Which block an
extern came from, and so which first key is suspended and which download is reported `rejected`
(B-34), is what the run's own store confirms of its record (B-6), never the record alone: a
record that came with a restored `target/` could name any block in the store. An extern whose
record says reused but which the store doesn't confirm suspends and reports nothing; its file
and record are removed, so cargo builds it, and the line names it. *Rests on:* the store's
`block.json` listing the SHA-256 of each file it gave out, which a reused file still has.
*Breaks if:* a record came back with the file's stat recorded (B-6). Every call that
runs rustc as asked after its arguments were read (`passed through` for a reason found after
reading them, such as the never-store list, a compiler of unknown identity or an incremental rebuild
(B-9), or `built, not stored` because a key, the store or a reuse failed) runs rustc as a child
when a reuse put at least one of its externs in place, with the same arguments, its stderr passed
on line by line and its stdout untouched, so that this rule runs; so does a call whose package
lies outside the shared folders, for its unit record (B-23). Otherwise rustc replaces the
wrapper, and so does it for a call whose arguments can't be read (B-4). A suspended key is never
reused. `hushrun build clean` clears it.

**B-16 Audit.** In `on`, one in fifty of the compiles that could be reused runs anyway, chosen by
a hash of the key and the run's start, whatever its class. Its outputs are compared byte for byte
with the block's: a library's, and a program's, a proc macro's or a build-script binary's, each
file of a program's debug bundle among them (less `Contents/Resources/Relocations`, which is
never stored, B-9). A difference serves the fresh outputs, suspends the key and is reported; for a linked output found different from a copy that was only downloaded, B-34 says what happens instead. In
`observe` every block found is compared the same way, and a compile whose block's messages don't
serve its form is compared too. *Rests on:* a link being as deterministic as rustc's own output
(the same link repeated, at the same path and at two, gave the same bytes on Linux and on macOS
with B-9's changes), so that a difference means a block built from other inputs than its key
names: a link environment B-37 missed, or bytes that were tampered with. *Breaks if:* a linker
wrote something of its own each time (a time, a random UUID): every audit of such outputs would
differ and suspend its key, which loses reuse and never serves wrong bytes. `HUSHRUN_BUILD_AUDIT`, read once as `hushrun run` begins, audits
every such compile when it is `every` and none when it is `none`; any other value keeps one in
fifty. A block's first reuse here, when it was only downloaded or opened from a baked store, is
picked one time in ten instead (B-34); one opened from the run's baked store, current (not
replaced), whose source is verified for this run (the server's word for its row, outputs, pack
SHA-256 and size, in the repository's trusted scope given as this run began; or a signature by a
signer the repository names that passes `not_before`, where the run has no backend that could say
the store was replaced or a block withdrawn: `baked::Evidence::current`, B-63, B-67) is never
audited at its first reuse here, and one in fifty at each later one, as any reuse is. A signed
store whose backend couldn't be reached, or isn't its source, keeps the one in ten: that audit took a C++ test build
from 6 seconds to 58 on every sandbox start and found nothing, and those blocks are the ones CI on
the default branch built, whose bytes the server or the signer vouches for. A store the person
accepted unverified keeps the one in ten (`Current::reuse_pick` decides which, and
`Current::audits_reuse` applies it with this variable: the one definition the wrapper, the Go cache
program and the launcher pick by). `every` audits those first reuses too. `every` is what a replay that compares two machines asks for. `none` removes the one check
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
B-6's `link` says; the C driver (`cc`, `-C linker`), found as rustc runs it, on the link
command's `PATH`, by where it lies (B-8) and, under a root, its SHA-256, else its size and
modification time; the `ld` that driver runs, the same way and, outside the roots, also its
change time and inode, so that a file put in its place with the same size and date (a package
that keeps dates, a link retargeted by `update-alternatives`) is another `ld`; and the SDK, by
where it lies and the date of its settings file. A driver, `ld` or SDK outside every root ties
the block. When the link's environment is known (B-37), E takes the place of those texts of the
driver, the `ld`, `dsymutil` and the SDK: it holds each of them by content, with what they load,
the start files, the C runtime and the toolchain's own linker tools, so a driver whose date
changed and whose bytes didn't is the same link, and one rebuilt with the same date is another.
Where E is unknown the texts stay, the block is stored and reused on this machine as before, and
it never leaves it (B-24); an `ld` in a folder named `gcc-ld` then also names the `rust-lld` in
the folder above by its size, dates and inode, since that is the linker it runs and no part of
the compiler's identity holds it (B-17). The hint keeps E's question (the driver as rustc named it, the line
without its inputs and output, the variables the line set, roots replaced) beside the one below;
a hint without it is not followed.

*Which `ld` runs* is asked of the driver at every link, and again before every lookup: never
remembered, never taken from a hint. When rustc runs `rust-lld` itself (`-flavor` first, as for
WebAssembly and many embedded targets) that program is the `ld`, asked nothing, and searches no
folder of its own. clang's `--ld-path=`, or a `-fuse-ld=` that holds a `/`, is it.
Otherwise the driver is asked `-print-prog-name=ld`, or `-print-prog-name=ld.<name>` for the
line's `-fuse-ld=<name>` (gcc asked `-fuse-ld=lld -print-prog-name=ld` still says `ld`), with
the line's options that change its answers (`-B`, `-fuse-ld=`, `--ld-path=`, `--sysroot`, the
target, `-m*`, the GCC installation clang takes, the kind of output), the link command's `PATH`
and `LC_ALL=C`. A path in the answer is the `ld`: clang searches `PATH` itself, and Apple's driver
names the selected Xcode's `ld`, so the key follows `xcode-select` and `DEVELOPER_DIR` and not
`PATH`. A driver that links through `collect2` (one lies in its program folders) is the
exception: gcc 15 answers with a `<target>-ld` it found on `PATH`, where `collect2` never looks
for that name. Its path is the `ld` only when it lies in a `-B` folder or a program folder, or
when the driver says it again asked with an empty `PATH` (an `ld` built into gcc); a driver that
can't answer that is one that can't be asked. Otherwise the name asked for is looked up like a
bare one. A bare name is looked up as gcc's `collect2` does: `real-ld`, `collect-ld`, then the name,
each in the `-B` folders and the driver's program folders (`-print-search-dirs`, `programs:`),
then the name on the link command's `PATH`. That `PATH` is rustc's own entries (the toolchain's
`bin`) and then the one rustc inherited, which a lookup takes from its own variables; the hint
keeps the question (the driver's name when rustc gave a bare one, which the lookup finds again
on that `PATH`, rustc's entries and the options, roots replaced), never the answer, and a hint
without it is not followed. A driver that can't be asked or whose answer can't be read (a
`-C linker` that is another linker run directly, as `ld.lld` or `mold`), and an `ld` found
nowhere, are `built, not stored`. A link given a gcc specs file (`-specs=`, `--specs=`,
`--specs`) or a clang configuration file (`--config`, and the folders one is looked for in) is
`built, not stored`: either can change which `ld` runs and what it is handed.

*Where `ld` looks for a library* after rustc's folders (B-6's `link`): first the folders of
`LIBRARY_PATH`, which gcc and clang both hand `ld` (the variable's text is keyed too, B-5). On
Apple's targets then the SDK's `usr/lib` and `usr/local/lib` and `/usr/local/lib` (Apple's `ld`
takes a folder under the SDK when there is one, else the folder itself; each `LIBRARY_PATH`
folder is taken both ways), and for a framework the SDK's `Library/Frameworks` and
`System/Library/Frameworks` and `/Library/Frameworks`. Elsewhere the driver's library folders
(`-print-search-dirs`, `libraries:`, which for clang are the other `-L` folders it hands `ld`),
then `ld`'s own `SEARCH_DIR`s (`ld --verbose`; none for an `ld` without that option, as `lld`),
one that starts with `=` under the sysroot `ld --print-sysroot` gives with the line's
`--sysroot`. Each folder is taken as the kernel finds it (the part that exists resolved on disk,
the rest by name), each once. Each candidate is by SHA-256, through the run's digests, or absent,
as in rustc's folders. A file found there that is a GNU linker script (text that is not an
object, an archive, a Mach-O file or a `.tbd` stub) holding only `GROUP`, `INPUT`, `AS_NEEDED`
and `OUTPUT_FORMAT`, comments allowed (glibc's `libc.so`, gcc's `libgcc_s.so`), has each file it
names keyed the same way: an absolute name as it is, a relative one in the script's folder, then
rustc's working folder, then each search folder, and `-l<name>` as on the line; a script found
among those is followed too, eight deep at most. Any other script, a name under the sysroot
(`=…`, `$SYSROOT…`), and any script while that sysroot is other than `/` or nothing, is `built,
not stored`.

Cost, measured per link on rustc 1.94 (median of 15 links of a proc macro and of a program,
against the same without these questions): on macOS (Xcode 27.2 through `/usr/bin/cc`) about
12 ms more at the link and 12 ms more at the lookup, nearly all of it the one question, which
the shim answers slowly; on Linux arm64 with gcc 14 (Debian 13 in a container) about 5.5 ms
more at the link (four questions, about 1,000 places stat'ed for the standard library's seven
`-l`, the libraries and scripts hashed once per run) and 2.5 ms more at the lookup. A clang
driver answers in about 15 ms a question. *Rests on:* a driver running the `ld` it names for
`-print-prog-name` or, for a bare name, the one `collect2`'s search finds (gcc 14, clang 19 and
Apple's driver checked); `ld` taking a `-l` only from rustc's `-L` folders, the driver's,
`LIBRARY_PATH`'s and its own, which `-print-search-dirs` and `ld --verbose` print (strace of GNU
`ld` 2.44: no other folder opened for a library); and a linker script's names being found as
written above (the GNU `ld` manual, `INPUT`). *Breaks if:* a driver ran another `ld` than it
names (a wrapper script that ignores `-print-prog-name`, which then answers for itself as the
`ld`); an `ld` searched a folder neither shows (one built with a path list `--verbose` doesn't
print); or a script's names were looked for elsewhere.

Where that list can't be read, where a file it names is gone or can't be found, or where a
linker script an option names names more files (`INCLUDE`, `INPUT`, `GROUP`, `SEARCH_DIR`,
`STARTUP`), the outcome is `built, not stored`. Every other argument of the link line that is
the path of an existing regular file (an object a `link-arg` names, `-bundle_loader`'s,
`-dylib_file`'s), and is not an output of the compile, is an input by SHA-256. A `-B` folder
outside the compiler's sysroot is `built, not stored`. Files in rustc's own temporary folder, and
the search folder rustc makes in it (`-L <output folder>/rustcXXXXXX/raw-dylibs` on Linux), are
not inputs: what they hold comes from the compile, and their name is new at each compile. What
rustc prints on stdout goes on to the wrapper's stdout, but the linker command. The `dsymutil`
rustc runs for a debug bundle (B-9), the first on `PATH`, is an input like the driver. A program
is stored with its debug bundle: every file under `<program>.dSYM`, named by its path from the
output folder; a bundle with no DWARF file, or that holds anything but folders and regular
files, is `built, not stored`. A program the wrapper leaves as it is (B-9) is stored with the
debug objects it names.

**B-37 The link environment.** For a linked output, **E** is the identity by content of what,
outside rustc's own command line, decides the link's bytes. One function computes it, from the
link's own command (rustc's `--print=link-args` line) and this machine's files, at the link and
again before every lookup, from the question the hint keeps (the driver as rustc named it, the
line without its inputs and output, the variables the line set, roots replaced), never from an
answer a hint or an earlier link gave. The line the hint keeps names rustc's temporary folder in
the output folder (`{hushrun:out-dir}/rustcXXXXXX`, new at every link: the symbol list a proc
macro's link hands the linker, and on Linux the `raw-dylibs` stubs every link searches) by that
fixed name (`link::fixed_rustc_temp`), as E reads it, so that two builds of the same code leave
the same hint: the folder is gone once rustc ends, and a lookup's dry run never found it under
either name. A hint that named the folder it linked in made a baked store's hints, and the
dependencies layer of its image (B-65), new bytes at every bake. E is the SHA-256 of an **ordered list** of entries, each
(role, name asked for, digest), so which file answered which request is part of it:

1. the C driver's dry run: the line without its inputs (each object and library given by path)
   and its output, with `-o /nonexistent/hushrun-link -###`, the link's `PATH` and the variables
   the line set (`SDKROOT`, `LC_ALL`, …). Its **whole text**, every word, with rustc's temporary
   folder names (`rustcXXXXXX`) and gcc's temporary files (`ccXXXXXX.res`) made fixed and roots
   and bases replaced by their names, also after an option's letters (`-L{…}`), in each line
   as a whole and only then taken apart at spaces, so a root whose path holds a space (a
   checkout under `My Projects/`) is replaced like any other. A command line of the dry run
   (the drivers start one with a space) is then written word by word, each between quotes only
   where it is empty or holds a space, a quote or a backslash: gcc quotes a word by the
   characters in it, so a folder with a space came quoted and the same folder elsewhere did
   not, and clang quotes every word. Other lines keep their own quotes, which don't depend on
   what a path holds. The text holds
   every flag the link passes, not only the files it names, and the loader's path the driver
   adds. What it does **not** carry is a path rustc computes from where a checkout lies:
   `-C rpath` writes the way from the output folder to the toolchain as a relative `RUNPATH`
   (`$ORIGIN/../../…`), and a lookup asks the dry run of the line a hint kept, which is the
   line of the checkout that linked. That is B-9's to key, in the first key;
2. the driver by SHA-256, as found on the link's `PATH`, on every target. A driver is a file
   that decides what the linker is handed, and its dry run of a line without inputs need not
   show what it does when it is given objects: a script of the workspace named with
   `-C linker`, or another machine's `cc` first on `PATH`, is another file and another E. On
   Apple's targets `/usr/bin/cc` is a small program that runs the selected Xcode's clang; it is
   hashed like any driver, and what that clang hands the linker is the dry run's text; the program the dry run's
   last command names (`collect2`, or the `ld` clang runs); the `ld` that really runs, which is
   B-18's answer for this link and nothing E searches for by itself (one rule says which `ld`
   runs: the one `collect2` finds, a gcc 15 driver's own answer believed only as B-18 says);
   when that `ld` lies in a folder named `gcc-ld` (rustc's own, handed to the driver with `-B`
   where rustc links with its lld, x86-64 Linux since 1.90), the `rust-lld` in the folder above,
   which that small program only runs and which writes the bytes; the plugin it loads
   (`-plugin`); and every shared
   library each of those loads, transitively, where the loader would find it: ELF `DT_NEEDED`
   through `DT_RPATH` (when there is no `DT_RUNPATH`), `LD_LIBRARY_PATH`, `DT_RUNPATH`, the
   folders `/etc/ld.so.conf` lists and `/lib64`, `/usr/lib64`, `/lib`, `/usr/lib`, with each
   program's loader (`PT_INTERP`); Mach-O load commands through the image's and the program's
   `LC_RPATH`, `@loader_path` and `@executable_path`, a system library that lies only in macOS's
   shared cache (`/usr/lib/…`, `/System/…`) by its name;
3. every absolute path of an existing file on that command, by SHA-256: the start files
   (`Scrt1.o`, `crti.o`, `crtbeginS.o`, …), the compiler runtime, `lto-wrapper`;
4. each library a `-l` of that command names, when it is the C runtime the standard library asks
   for (`c`, `m`, `rt`, `pthread`, `dl`, `util`, `gcc_s`, `gcc`, `gcc_eh`, `System`) or one the
   driver added, as `ld` finds it: the command's `-L` folders in order (on Apple's targets each
   under the SDK first when it is there), then `ld`'s own (`ld --verbose`'s `SEARCH_DIR`s;
   Apple's: the SDK's `usr/lib` and `usr/local/lib`, and `/usr/local/lib`), `.so` before `.a`
   while the command links dynamically and `.a` alone while it links statically (`-Bstatic`,
   `-Bdynamic`, `--push-state`, `--pop-state`), `.tbd`, `.dylib`, `.so`, `.a` on Apple's; and each
   file a GNU linker script found there names (B-18's reading), eight deep at most. Any other
   `-l` is a crate's, keyed by content in `link` wherever it is found (B-6), and so stays in
   what a dependent names a proc macro by;
5. every file under the toolchain's `<sysroot>/lib/rustlib/<host>/bin/` (`rust-lld`,
   `gcc-ld/*`, `rust-objcopy`), which B-17 doesn't hash and rustc runs (`rust-lld` is the default
   linker on x86-64 Linux, `rust-objcopy` strips on macOS), with what each loads where that is
   found (a tool that doesn't run for this link may need a library only rustc's own variables
   lead to, as Linux's `rust-objcopy` takes `libLLVM` from rustup's `LD_LIBRARY_PATH`: such a
   library is listed as absent, not a reason E is unknown); and the
   `dsymutil` rustc runs when a program gets a debug bundle, first on `PATH`, and on Apple's
   targets, when that is the `/usr/bin` shim, the selected Xcode's beside its `ld`;
6. on Apple's targets the SDK the dry run names (`-syslibroot`): its `SDKSettings.json` and
   `System/Library/CoreServices/SystemVersion.plist` by content, and the `-platform_version`
   triple.

Where a file lies is in E only as far as the dry run's text says it (two installs of one image
have the same text); the places searched and found empty are not in it. A file that appears
earlier in a search (another `ld` earlier on `PATH`, a newer GCC beside the old one) changes
which file answers, so **nothing about E is remembered**: the dry run and every search run again
at each link and each lookup. Only each file's SHA-256 is remembered, for the run, by the path as
found and its stat (size, both dates, inode, device; B-6's run digests), so a link retargeted to
another file is another file. The list is stored with the block (its description's
`link_environment`), never in a key but by its digest.

E is **unknown**, with why, when: the dry run doesn't say it is gcc (`gcc version …`) or clang
(`… clang version …`), judged from its output, never from the driver's name (a wrapper script
named `clang` that runs clang is clang); the dry run exits with an error, prints `error:`, or
names no program; that program is neither `collect2` nor an `ld` hushrun knows (`ld`, `ld.bfd`,
`ld.gold`, `ld.lld`, `ld64.lld`, `ld64`, `ld-classic`); the `ld` B-18 found can't be run, or it
is rustc's `gcc-ld` program and there is no `rust-lld` above it to run; a `-fuse-ld=` on rustc's line or on the
command names anything but `bfd`, `gold` and `lld` (`mold`); `ld --print-sysroot`, with the
command's `--sysroot`, is neither `/` nor empty (an `ld` that can't say: the command's own
`--sysroot`); a linker script does more than name files; a `-l` or a library a program loads is
found nowhere; a library search folder holds a `$` word other than `$ORIGIN`; the link's
environment sets `LD_PRELOAD`, `LD_AUDIT`, any `DYLD_` variable but `DYLD_LIBRARY_PATH` and
`DYLD_FALLBACK_LIBRARY_PATH`, or one of those or `LD_LIBRARY_PATH` with a folder that is neither
a root nor a base (cargo's `deps` and rustup's toolchain `lib` are both) and, for the macOS
fallback, not one of the defaults cargo adds (`$HOME/lib`, `/usr/local/lib`, `/usr/lib`); or the
link itself does link-time optimization: `-C lto` or `-C linker-plugin-lto` on rustc's line, or
a `-flto…` on the link line. The standard library's own bitcode (`.llvmbc` sections inside its
objects) is not that: the linker takes those objects' code. *Rests on:* a link's bytes following
from the files it reads and the programs it runs, which the driver's dry run and the loaders
name; and the dry run printing the command the driver runs (gcc 14, clang 19 and Apple's clang
checked; equal E gave equal bytes on seven pairs of machines and different E different bytes,
Debian 12 against 13 among them). *Breaks if:* a program on the way read a file neither the dry
run nor its loader names (a configuration file of its own, a plugin it searches for itself);
the driver changed what it adds without printing it in its dry run; a library came from the
loader's cache (`ld.so.cache`) where `ld.so.conf`'s folders don't lead, or from a
`glibc-hwcaps` subfolder; macOS's shared cache held another version of a library a linker program
loads and that changed what it writes; or another `ld` ran than B-18 finds (its own *breaks
if*). The linker's version text is not asked: the `ld` it would describe is in E by content.

Cost, measured on rustc 1.94 (median of 11 links of a proc macro and of a program after the
first, release build): on macOS (Xcode 27.2 through `/usr/bin/cc`) about 12 to 19 ms at each
link and the same at each lookup, nearly all of it the dry run through the shim; on Linux arm64
with gcc 14 (Debian 13 in a container) about 3.5 to 6 ms (three questions, about 105 entries).
The first link of a run also hashes what no earlier link of the run did, about 210 MB on macOS
(`rust-lld` 122 MB, `libLTO.dylib` 69 MB) and 330 MB on Linux (`libLLVM` 155 MB, which the
toolchain's tools load): 0.4 to 0.5 s on Linux and about 0.7 s on this Mac, whose SHA-256 is
slow under load. A file of 8 MiB or more is hashed under a lock on its run digest's name, so the
links a cold build starts at once hash it once and the others wait for it and read it.

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
`min_free_gb` and `programs`. `mode` is in the file only once someone chose it (`hushrun build
on|observe|off`, or a file written by hand): a save of another setting keeps the mode the file
had, and writes none where it had none, so that a file never says `off` where nobody chose it
(B-25 reads a written `off` in a sandbox). A file with no `mode` is `off` on a person's
machine. `programs` is not chosen, or `tied`. Not chosen, programs follow sharing: in a run that
shares (B-9) a program is movable and
gets a debug bundle, which costs a `dsymutil` run per program and about twice the bytes
stored on a first build and lets another checkout or machine reuse it; in a run that shares
nothing it is tied, compiled as asked and
reused only in the target folder that built it. Set to `tied`, programs are tied in every run.
Nothing sets a bundle for a run that shares nothing. In the file, only `tied` is written. A
written `movable` is read as not chosen, like a missing field: every hushrun up to 0.16 wrote
`movable` on each save, whoever chose or didn't, so it can't be told from nobody's choice; and
the file's version stays 1 with no new value, since those versions read the same file and
refuse a value or a version they don't know. Any other value makes the file invalid. The
session carries what the file said when `hushrun run` began, written the same way, so every
compile of a run sees one value; in CI and in a sandbox, where no person's settings are read
(B-25, B-31), `programs` is not chosen. `hushrun build status` shows the mode, the store's folder, how long blocks are kept,
`programs`, the share setting and what sharing does here and why (B-33), the store's size and
its blocks by age and by repository; in CI, one more line of what `hushrun run NAME -- COMMAND`
does in the job and why, from B-25's one function and no person's settings (reuse in the job's
own store when the org shares, reading only with an agent token outside GitHub Actions, or
nothing, with the reason: no backend, no ID token). `hushrun build clean` empties the store. `share` (B-33) is
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
  names, and runs to its end (ADR 0144): a check stopped by a deadline would leave a crate whose
  inputs changed unchecked on a loaded machine, and cargo would reuse its stale outputs. A record
  that can't be read is ignored.
- *What it says.* For each crate it removed, one line in the run's summary after the totals, at
  most three: `build: <crate> is rebuilt: <what changed>, which cargo doesn't watch`, the path
  from the folder `hushrun run` runs in. When nothing was removed it says nothing.
- *Limits.* Only crates the wrapper saw compiled are checked, and only in the output folders
  registered for the folder `hushrun run` runs in: a script that builds into a folder elsewhere
  is not covered, and neither is a crate last built by plain cargo. A macro that reads a file the
  table of known macros doesn't name, and a file the link line names (B-18), are not watched. A
  name added to a watched folder that a fresh compile would also see as an input (an editor's
  swap file beside a source) rebuilds the crate, as its key says.

**B-24 Portable.** A block is **portable** when nothing in its keys, its hint, its record or its
bytes names the machine that built it. A portable block may leave that machine; so may, from a
CI job, one that is only bound to folders (below); nothing else ever does.
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

- `link-environment-unknown`: a program, a proc macro or a build-script binary whose link
  environment can't be told by content (B-37): the description and the log say why. A linked
  output whose E is known is portable on the same terms as a library: its key holds E, which two
  machines compute alike only where the files that decide the link's bytes are alike. *Rests
  on:* B-37's list being all that decides those bytes. *Breaks if:* it isn't (B-37's *breaks
  if*); the audit (B-16), which compares every class, is the check on it.
- `toolchain-in-output`: a linked output holds the toolchain's own folder (B-10). Such a block is
  keyed by the toolchain's place (`bound`), as by any machine folder it holds. On macOS B-9's
  changes keep it out of proc macros, build scripts and movable programs; a tied program
  (B-21), or one the wrapper leaves as it is, still names the standard library by it.
- `tied`: its place is its roots (B-10), or it is a movable macOS program whose output folder
  lies outside its workspace, or a linked output with rustc's own run paths (`-C rpath`),
  whose first key holds those folders (B-9).
- `unsearchable`: its outputs can't be searched for folders (B-10): it is `tied` as well, and
  never travels.
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
  differently (B-6): it is tied, what names it holds this machine, or the run's own store
  doesn't confirm its record, so it is named by its bytes; or a library among its
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
  rules, and so is each place a value lies inside one of those folders or the toolchain's (the
  folder macOS's `TMPDIR` names, above a store; a user's name, in the home folder): there it is
  that folder, which its own reason reports, and only a place outside them counts. The values are looked for in the same pass over each output as the roots, and only
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

**A block bound to folders travels where those folders are the same.** Some reasons name
only folders whose values the block's keys hold: `tied` (its place, B-10, or the target folder
of a movable macOS program, B-9), the `…-in-output` and `…-in-record` reasons with
`toolchain-in-output`, and `variable-names-base`, whose folders are in the second key as
`bound`; and `read-in-shared-folder`, whose read is in the second key by its path and its
content (a library a crate asks for with `-l`, found in the system's or the SDK's folder, as
`-liconv` and every framework are on macOS; so is every source read, native and declared
input). The one read the second key holds by content alone is a library the linker is handed
by its path: that path is an argument of rustc's own line, which the first key holds as
written. A block whose reasons are all of those is **bound**: another machine computes its
keys only where every such folder is the same text, and there the block is as right as on the
machine that built it. One function says so (`Portability::bound_only`), and the wrapper tells
the relay of a block it built both answers, portable and bound. A bound block is offered for
upload **only by a CI job, which is a run whose credential is the job's own ID token** (B-29,
B-31): the credential the server trusts a job by, never the environment, which says CI on any
machine where a variable is set. A person's or an agent's token offers none, in CI or out of
it. Every job of a repository has the same paths, which is
where such a block is of use (a program that holds the cargo home through a C library's debug
info, a test binary that bakes `CARGO_MANIFEST_DIR`, a library that bundles C code), and a
CI path names nobody, where a person's folders hold their user name, which everyone the scope
serves could read in the block's bytes. Nothing changes for the machine that looks one up: its
hint names which folders and never their values (B-7), the lookup fills in this machine's, and
a machine with another value finds another key. What never travels, bound or not: a block
whose outputs can't be searched (`unsearchable`, which comes with `tied`: it may hold a folder
nobody saw), and one with any other reason below. A tied test binary reused where its checkout
is the same reads, when it runs, the files of the machine that runs it, as one built there
would. *Rests on:* the search and the proofs finding every folder a block holds, so that
every one is in a key (B-10's *can't see* applies: a folder written in another form is in no
key, and travels with a block marked portable today as well). *Breaks if:* two machines have a
folder of the same text and other content that the keys don't hold: the cargo home's files are
keyed by content where a compile reads them, and a path a program only names is read when it
runs, so this is the case of any tied block on one machine whose folder's content changed.

A library among its externs is not a reason: it is named by its bytes, so a dependency that isn't
portable gives the block another key on another machine, never a wrong block. A proc macro whose
block is not portable only because of its link environment, or because its bytes hold a machine
folder or the toolchain's, is still named alike on two machines (B-6): neither is in that name.
A program that bakes its checkout (`env!("CARGO_MANIFEST_DIR")`, `CARGO_BIN_EXE_<name>`) is
`tied` and stays on its machine, like any block that holds one of its roots.

## Sharing through a backend

A backend may keep a **shared store** of build blocks for an org, behind every machine's own
store. The backend never sees a key, a path or a crate name: it sees a block's **address**, its
**kind** (`rustc/1`, `go/1` for Go's package archives, B-45, or `cc/1` for C and C++ objects, B-60, a draft), sealed bytes it can't open, and the scope a caller's credential puts it in.
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
| CI with an agent token and no ID token, the org shares, outside GitHub Actions and GitLab CI | on, in the job's own store | the org's mode, reading only |
| CI otherwise | off | off |
| An agent token where a person is signed in, the org shares | that person's local mode, `off` until the person chose one | the org's mode, lowered as for the person, by the person's settings |
| An agent token where a person is signed in, otherwise | that person's local mode | off |
| An agent token in a sandbox (no person signed in), the org shares, no `off` written or set there | on | the org's mode |
| An agent token in a sandbox, otherwise | off | off |

"The org shares" is read from the run's results lookup, whose answer carries `Hushrun-Build:
on`; only then is `open` asked, so an org that doesn't share pays no request. Build blocks belong
to the repository, not to a scope: a run that can't tell which code it is (several scopes in
`hushrun.yaml` and no `--scope`) makes no results lookup, and still shares, reading the
repository with no scope chosen; its `open` is the one request that asks whether the org shares,
and an org that doesn't share says nothing there either. Only a run that can't read the
repository at all doesn't share, and says so in one line when it has a credential to share
with.

The table is `hushrun run NAME -- COMMAND`'s, on a person's machine, in a sandbox and in a CI job
(B-2, B-31): that form alone shares. `hushrun run CHECK…` shares nothing, in any row: it starts no
relay, so it neither downloads from nor uploads to the shared store, and asks the backend nothing
about blocks. Outside CI it reuses this machine's own store as B-1's mode says (and so nothing in
a sandbox, where nobody chose a mode), and reads a saved fetch that serves it (B-36) as any run
does; in CI it sets no wrapper at all. So that this is not silent: when a check of such a run
compiled through the wrapper, the run asks the backend once, with one results lookup (the
check's own key, B-26's header on its answer), whether the org shares, and where it does says
one line a run, ``build: only `hushrun run NAME -- COMMAND` uses the org's shared store: this
run's crates were neither downloaded from it nor uploaded to it``. A run whose checks compile
nothing asks nothing, and in CI, or with reuse off here, no wrapper sees a compile, so nothing
is asked or said. *Rests on:* the backend's header and `open` saying what the
org decided, and the machine's settings being its own; `hushrun run CHECK…` handing its session
no backend to share through (`cmd::run` begins each check's session with none, ADR 0078), so it
never asks the backend about blocks. *Breaks if:* a project's CI runs its builds as checks of `hushrun.yaml`: it reuses
nothing there, and its sandboxes reuse only what a fetch saved. A run never shares more than its local mode allows: with local `observe`, sharing
observes too.

A **sandbox**, for this purpose, is a machine where no person is signed in to the run's backend
(the credentials file holds no login for its host) and whose run carries an agent token: both,
as the one function that maps a run's credential to its row says (`build_session::caller`, with
`build_session::signed_in_here`, for the session, the relay and `hushrun build status`). An
agent token's run there whose org shares (its results lookup said so, or, with none made, `open`
says so, which the run waits for as a CI job does) begins a session in the
store with reuse on, and its relay shares as the org's mode says. A written `off` wins there as
it does everywhere: where the machine's settings file says `"mode": "off"`, as `hushrun build
off` writes it, the run has no session, reuses and shares nothing, and says nothing. Nothing
else in the file is read for a sandbox: no file, or one that names no mode (as a save of
another setting leaves it, B-21), leaves reuse on,
`observe` or `on` written there change nothing, and a file that can't be read turns reuse off
and says why in one line, since it can't say whether `off` is written. `HUSHRUN_BUILD` sets
one command's mode over either (`settings::sandbox_mode` is the one definition, for the run and
`hushrun build status`); a run from a saved fetch (B-36) on a machine where no person is signed in to the fetch's
backend is one too, the fetch standing for the token that made it. One whose org doesn't share
runs as it would without any of this, and says nothing. Where a person is signed in, an agent's
run with a token is that person's machine's: it follows the person's build settings, `off` until
the person turns reuse on (B-1), so build reuse never turns on there without someone choosing
it. A settings file, or none, says nothing about what the machine is; it says only whether
reuse is `off` there. *Rests on:* a person's
sign-in being on the machines people work at and not in a sandbox's image. *Breaks if:* a
sandbox's image carries a person's saved sign-in for the backend: its agent runs follow that
person's settings, off by default, and reuse nothing until `HUSHRUN_BUILD=on`; or a person who
never signed in to the backend runs an agent token on their own machine: it is taken for a
sandbox, reuses in the store and shares as one.

When `open` answers that sharing is off although the org shares, it gives one reason
(`hushrun_core::blocks::OffReason`): `plan`, the org's plan includes no shared store; or
`contract`, the org's plan sets its shared storage by contract alone and none is set yet. For
`contract` the run says one line, also where `open` decides whether the org shares (which is
otherwise silent) and in a CI job that then runs unwrapped (B-2), since the org did turn sharing
on: `build: sharing is off for this run: the
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
run with one line. While the relay works on an answer it writes one space every second
(`relay::HEARTBEAT`), which a wrapper's read takes for the answer still coming and JSON reads as
nothing; a wrapper waits on a relay that says nothing at most 30 seconds (`relay::WAIT`), and no
socket, no answer, an answer it can't read and a relay silent that long are a miss, and the
compile goes on as without a shared store (B-19). An answer that panics ends its heartbeats and
its connection: a miss at once.

**No time limit decides what a build reuses** (ADR 0144). `open` is sent as the run starts, and a
question that comes before it answers waits for it: until it answers, or fails, its server silent
for 30 seconds (`Tuning::open`) and once more. A fetch a wrapper waits on goes to the front of
the queue and is waited for to its end, however long the download takes and whatever its
compile cost here or on the machine that built it: what the shared store holds is reused, on a
slow link or a loaded machine alike. Every request to the backend or the object store waits on
its server's **silence**, 30 seconds (`http::SILENCE`): its head, then each piece of its body,
each byte starting it again; a PUT's body goes a piece at a time and fails when it stops moving
for the silence. How long an answer takes, on its own, never makes it a failure. A **failure**
is a request that errored or whose server was silent for the silence: a network error, a 5xx, a lookup or a download that failed
(a download that fails its checks included), and an upload's `begin`, PUT or `commit` that
errored or whose server went silent; a download that arrives ends the row. Three in a row **rest** the relay for
30 seconds, and the run's first rest says so in one line, once: the backend stopped answering
mid-run, and why. While it rests no new request goes to the network (no lookup, hint lookup,
download, `begin`, report; the upload loop claims nothing new), and
every wrapper is answered from what the relay holds: hints the prefetch downloaded, packs
already in `incoming/`, downloads already under way, vouches already known. A 401, 403 or 404 in
the middle of a run stops every request for the rest of the run, keeps the markers (B-29) and
says one line; a server that can't be reached says one line too. A download, a comparison's
download or a PUT that can't connect to the object store's host (a sandbox behind an allow-list
reaches the backend and not the store) says, once per run, which host to allow:
`build: the shared store's host, <host>, can't be reached from here: where this network allows
only some hosts, allow <host> to reuse what other machines built`, the host alone
(`build_relay::store_host`), never the link, its path or its query, which hold a signature; no
error that names a link reaches any line. `hushrun build fetch` says it too. Every failure ends
in rustc running as asked. *Rests on:* what one machine reuses of another's being worth more
than the time a slow link or a loaded machine adds to a build, so that the outcome is the
backend's answers alone; a server that stopped answering going silent, which a working one never
does for 30 seconds; packs already downloaded costing nothing more to open, so a rest that
dropped them would only waste them; and the socket being no boundary inside one OS user, whose
processes can read the machine's key anyway. *Breaks if:* the link is slower than compiling: the
build waits for downloads it could have beaten, by choice.

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
and reported `unreadable`. A pack keeps no file mode: the reader gives the linked output of a
program, a build script or a proc macro a run bit wherever it has a read bit, as rustc writes it
under the umask, so that cargo can start a build script that came in a pack. Before it writes any
file, the reader refuses a record whose files come to more than 64 times the pack, or to more
than the disk can take above the free-space floor
(B-22), by one rule (`Relay::room_for`) for a download and for shared `observe`'s comparison
(B-33), which writes nothing but would spend as much work; that is a compile, not a report (in
`observe`, no comparison), and each file stops at its declared size whatever the stream holds. A pack over 2 GiB is not shared. *Rests on:* the digest of outputs
being computed again from what was unpacked, so that no comparison rests on the uploader's word;
zstd giving the same stream for one version of it, so two versions of hushrun may give two packs
of one block, which costs a wait and nothing else.

**B-29 Uploads and the outbox.** A block gets a marker in the store's `outbox/` only when it was
built in this run, it is portable or, in a run whose credential is a CI job's ID token, bound
to folders (B-24), the policy `open`
returned says the caller writes,
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
| claimed | done, the local block discarded | `begin` refused `differs` for a library: a trusted copy with other bytes exists |
| claimed | done, the local block kept and its first key kept to this machine | `begin` refused `differs` for a program, a proc macro or a build-script binary (B-34) |
| claimed, sent | waiting, not tried for 10 minutes | a network error, a 5xx, 401, 403, 404, an expired link, `busy`, `over_allowance`, `too_many_uploads`, `commit` answering `missing`, `gone` or `differs`, any word this version doesn't know |
| claimed, sent | waiting, one more cut-off | the end of the command cut off its `begin`, PUT or `commit` in flight |
| waiting | done | three cut-offs |
| any | done | the block is gone, or the marker is older than `keep_days` (B-14) |

`begin` is sent a few blocks at a time, just before their PUTs, signed by the machine; PUTs run 4
at a time, 2 while downloads are in flight; a stored block's newest hints go up with it, sealed
with its first key. When the command ends, nothing after it has a deadline unless a stop signal
came (`upload::wait_at_end`, the one place that says; ADR 0144), on a person's machine, in a
sandbox and in CI alike: the upload loop goes on claiming what the run built until none is left
to go now (each one done, or put off by an answer or a server's silence), or the relay rests or
stops; then `close` names what went up. What a run leaves out of its manifest depends on the
backend's answers alone, never on how fast the machine was. On a terminal the run says, while it
waits, `hushrun: build: uploading N blocks before the run ends (Ctrl-C leaves the rest for the
next run)`. Asked to stop (B-49), it starts no PUT, cuts off those in flight and the `begin`
and `commit` requests at once, and gives `close` 2 seconds. The rest waits for the next run, and
the run says how many wait. An upload loop that panics is over when it panics, as one that
returned is (`upload::uploads` marks it so however it ends, and `Relay::start_uploads` marks it
running before it is spawned, so that an end that comes first waits for it): the end of its
command waits for it no longer and sends `close`, and a later check of the same process starts its own loop. The claims it
held stay claimed until its run is dead, then go back at the next relay's start or in the sweep
(B-14), as a crash's do; its packs under `outbox/tmp/<run>/` go with the dead run's folders. A
wrapper whose answering thread panics gets no answer, which is a miss; every later reader of the
relay's state takes a lock a panic poisoned as it is.
`commit` answering `differs` means the object store holds a size or SHA-256 other than the one
`begin` named: the upload arrived broken, and the block here stays and goes again later. Only
`begin` refusing `differs` says a trusted copy with other bytes exists, and only then is a local
block discarded; the run says in one line how many it discarded.
*Rests on:* the record of where a block came from (B-32), which keeps a block from going up under
another name or into another org, and undoing a withdrawal; and packs being the same bytes on
every try (B-27), so a retry after a crash finds its upload under way.

**B-30 Prefetch.** `close` sends the run's **manifest**: the ordered pairs of first-key address
and block address of every compile, reused, downloaded or built, but a block this run meant to
upload and didn't (its marker still waiting in the outbox, or claimed and cut off at the end of
the command, B-29), and a compiled crate that may not leave the machine (neither portable nor,
from a CI job's own ID token, bound to folders alone, B-24: such as one an `extern` keeps here):
no bake or fetch counts on a block the server doesn't hold. Its marker stays,
and the next run in the same store uploads it (a CI job's store goes with the job, so there its
block is simply not named). `open` returns the newest one
for the repository, the check and the platform, and the commit that run's `close` named
(`manifestCommit`, absent when the backend keeps none: B-75 compares it). The prefetch starts when the first wrapper of
the run asks the relay anything: a run whose compiles all find their blocks here (a warm target
with an empty store, or no compile at all) downloads nothing. The relay then drops every pair
whose block or first key the store holds, asks for the rest's hints in one request and looks up the rest's blocks in
one, and downloads by class, 8 at a time, before cargo asks: proc macros and build-script
binaries first, since cargo can start no dependent of a macro and no build script's run before
they are there, and they are small; then the libraries, in the manifest's order. Programs are
the last thing a build needs and the largest: they are fetched only when asked for, and so is
a class this version doesn't know; but a bake's relay (B-63) downloads every block the manifests
name, programs last, since nothing would ask for them and the store holds what its manifest
names. A manifest of a kind a build tool keys itself (`gradle/1`, `maven/1`, whose blocks go up
as class `other`, B-81, B-175) is downloaded whole, in its order. A pair whose first key this
machine keeps to itself (B-34)
is left out. A pack waits sealed in `incoming/<run>/` until a wrapper brings the
key that opens it, and is deleted once opened and published, or when the run ends. A hint is
written where downloaded hints live, apart from the hints compiles here wrote: those are tried
first, and a downloaded hint naming more bytes than twice what this machine's own hints of the
first key name (64 MiB when it has none) is not followed. *Rests on:* a manifest being only
addresses, so a wrong one wastes downloads and can't cause a wrong reuse; and a warm machine
holding what its manifest names, so it downloads nothing.

**B-31 CI and sandboxes.** A CI job's store is the job's own, never the cache directory, and
nothing in it outlives the job. A `target/` folder the job restores from a cache is no evidence there:
its records name blocks this store doesn't hold, so its proc macros are named by their bytes and
nothing built against them goes up (B-6); the job shares what it compiled itself. In GitHub Actions it is `hushrun-build-<run>-<attempt>-<digest>`
under the job's temporary folder (`RUNNER_TEMP`), the digest one function makes
(`build_session::github_job`) of the repository (`GITHUB_REPOSITORY_ID`, else
`GITHUB_REPOSITORY`), the run (`GITHUB_RUN_ID`), its attempt (`GITHUB_RUN_ATTEMPT`), the job
(`GITHUB_JOB`) and the job's own request token (`ACTIONS_ID_TOKEN_REQUEST_TOKEN`), which every
step of one job shares and no other job has: so every call of one job finds the same store, and
no other job can make or name it. At its start a job removes every other `hushrun-build*` entry
in its temporary folder, unread. Elsewhere (GitLab CI, which gives a job no folder of its own,
or with no request token) the store is `hushrun-build-<job>-<random>` under the system's
temporary folder (`TMPDIR`), `<job>` the job's id (`CI_JOB_ID`) else the process's, `<random>`
16 hex characters of fresh randomness, so that two runs never name the same folder, whatever
their process ids (two containers on one shared `/tmp` have the same ones). The run makes it
itself, exclusively (mode 0700), and takes the lock of its `ci-run.lock` at once, which it holds
while it lives. A folder already at that path is never adopted, read or removed: the run has no
store and says so in one line. The store goes when the run's session ends, whatever COMMAND did
(`end_run_store`). A CI command that isn't `hushrun run` (a bake, `hushrun build manifest`,
`hushrun build status`) uses the job's store where GitHub Actions gives one, and otherwise a store
of its own made the same way, removed when the command ends (`build_session::ci_command_store`),
so a GitLab job bakes; `hushrun build fetch`, whose store a later step reads, needs the job's. At its start a run removes the other `hushrun-build-*` folders there of this
user's whose lock file was made over an hour before and whose lock is free, which a run that
crashed or was killed left, and leaves every other one, a live run's included whatever its age. In CI who built a block, and so whose markers go up (B-29, B-32), is the job and its
store: the credential's fingerprint (for an ID token, the job's digest) joined with the store's
folder, so that a marker or a block another run left is never this run's, whatever credential it
was made with. No person's settings file is read, reuse is on unless `HUSHRUN_BUILD` says
otherwise, `HUSHRUN_BUILD_DIR` is not followed, and the free-space floor is 2 GB. In GitHub
Actions every build request carries the job's ID token, whose audience is the backend's URL, also
when an agent token is set beside it. One place decides what a request carries
(`Credential::bearer`): a token with more than a minute left is sent; one with less starts a
mint in the background and is still sent until its real expiry, also when that renewal fails;
a 401 gets a fresh one and one more try. At most one mint is in flight, and it waits on its
endpoint's silence (B-26) whoever waits for it; a request with no token it may send (none yet,
the held one expired, or the one a 401 refused) waits for that mint to end, and a mint that fails
counts, for that request, as a server that can't be reached (B-26). No mint
starts once the end of the command has begun: `close` and the last uploads carry the token held
while it is good, or go unsent. The results lookup whose `Hushrun-Build` answer sets the
wrapper carries the ID token too, before any token the environment holds (C-13), so the answer is
one to the job itself. With no ID token (a pull request from a fork, a job without
`id-token: write`) sharing is off and the run says why in one line, whatever else the
environment holds: an agent token in `HUSHRUN_TOKEN` or `HUSHRUN_AGENT` reads nothing there, and a
results answer obtained with it never sets the wrapper. The same holds in GitLab CI with its own
ID token (B-48). Anywhere else in CI an agent token reads the org's blocks and uploads none,
whatever the policy says. **A person's or an agent's token
used in CI says so:** every build request of such a run, and its results lookup, carries
`Hushrun-Ci: 1` (`hushrun_core::blocks::CI_HEADER`; one test each for the relay's client and the
lookup, `Client::says_ci` and `results::Server::says_ci`: the run is a CI job's and its
credential is a token, never a job's ID token), and the lookup names the run's repository
(`?repository=`, what the CI provider says, else what the run read), so that the backend
answers `Hushrun-Build` by CI's rule for that repository and not by the token's. The backend
then gives it the repository's trusted blocks to read and nothing to write, and `open` answers
`"untrusted": "credential"` with `writes: false`: the run makes no marker (B-29) and says once
`build: this job reads the repository's trusted blocks and writes none: it uses a person's or an
agent's token in CI, not the job's own ID token`. A CLI from before the word reads it as
`unknown`. Outside CI no request carries the header.
Every request, lookups included, names the workspace's commit when one can be read, and in CI
only while the files git tracks are as that commit has them, checked once per run
(`wrap::build_commit`, a `git diff` of the worktree against the commit): a job that changed a
tracked file before building (a version bump, a nightly `cargo update`) names none, which the
backend counts as a commit other than its token's, so it writes no trusted block. A file git
doesn't track, and changes inside a submodule's own checkout, are not compared; a job the
policy calls untrusted says once, in plain words, why it writes nothing to the repository's
trusted blocks. The end of the command waits for every upload the job built (B-29, ADR 0144),
whatever it writes and whatever the blocks cost: the store goes with the job, and what it leaves
unsent is left out of its manifest for every later job. Whether a job writes the repository's
trusted blocks (the scope the policy says it writes is the repository's trusted one,
`protected/…`, and nothing else decides it: `Policy::writes_trusted`; a pull request's job writes
`ci/…` with no reason it isn't trusted, and is not trusted) decides what it writes, never how
long it waits. *Rests on:*
the runner emptying its temporary folder when the job ends; the ID token's claims being what the
backend trusts a job by, so a token copied out of one job can do no more than that job could, for
minutes; the commit being what the backend checks a trusted job's token against, and the
tracked files being as it has them; every block a job built being worth its upload, since every
later job of the repository then downloads it rather than compiling; and the request token
being the job's alone. *Breaks if:*
a job's build reads a file git doesn't track (one an earlier step generated outside
`.gitignore`): the job names its commit while building other code, and the commit check, a guard
against honest mistakes, doesn't see it (keys still commit to every file read, so no wrong bytes
follow); the token endpoint fails for longer than a token lives (5 minutes on GitHub): from the held
token's expiry, every request is a miss and the job compiles as it would without a shared store,
its uploads lost; the end's uploads outlast the token held, since no mint starts once the end has
begun: what is left unsent is said, and left out of the manifest; a self-hosted runner keeps `RUNNER_TEMP` between jobs and the next job can't remove what the last
one left there (a file it lacks permission to delete): that store is never read or uploaded, but
it takes disk; the legs of one matrix job, which share the run, attempt and job name, run one
after another on one runner and the runner gives them the same request token (then a leg reuses
and uploads what an earlier leg of the same run built, in the same scope); or another process can
read a job's environment, which holds the request token.

**B-36 A sandbox's fetch.** `hushrun build fetch [CHECK]...`, run in a sandbox's setup phase with
the network and a credential (an agent token in `HUSHRUN_AGENT`, an agent or a personal token in
`HUSHRUN_TOKEN`, a person's sign-in, or in GitHub Actions the job's ID token; its help names
them, and with none it exits naming them all, `HUSHRUN_AGENT` first), sends `open` for the repository and platform once for each check named,
by its id as `hushrun run` names it (B-26: a digest of the repository and the check's name), or
once naming no check, which the backend answers with the repository's newest manifest for the
platform, whatever its check (`OpenRequest::check`); an `open` naming a check gets that check's
manifest only. It downloads what the manifests name that the store lacks, and saves into the store's `prefetched/` the sealed packs, the sealed hints, the
policy and the rows, with no token and no link, written whole (a new folder renamed in, named for
no process: two containers sharing a store are both process 1, and neither sees the other's
processes). It is good for 24 hours from the time it was fetched, read when it is used; the sweep
removes it after, and the folders a fetch that died left once they are an hour old.
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
run deletes the fetch. A machine where no person is signed in to the fetch's backend is a
sandbox (B-25): reuse is on there unless its settings file says `off`, and sharing is the
org's mode as saved; where one is, that person's settings decide. *Rests on:*
a sandbox's later phases running the same repository and toolchain as its setup, so the keys
they compute are the ones the fetch's manifest names; and the packs staying sealed with each
block's key, so the fetch holds nothing a compile here couldn't make. *Breaks if:* a block the
server withdrew after the fetch is reused within its 24 hours, as B-34 allows any run that can
reach no server; or the org turns sharing off while the backend can't be reached, which a run
there learns only when it can. A network that refuses the backend with an answer of its own (a
proxy's 403) counts as the backend's answer, and the fetch then serves nothing: a cost, not a
wrong block. A sandbox whose later phases have the network and no token gets a 401, and so
nothing from its fetch, for the same reason.

**B-48 GitLab CI, through its ID token.** In a GitLab CI job a credential for build blocks is
the job's ID token, read from `HUSHRUN_ID_TOKEN` alone, which the job declares with
`id_tokens: {HUSHRUN_ID_TOKEN: {aud: <the org's URL>}}`, one string: hushrun asks GitLab for no
other, and gitlab.com only (a self-managed instance's tokens are not taken). A run is a GitLab
job's by one function (`gitlab_job::is_job`): GitLab as the provider (`GITLAB_CI`), or GitLab's
job variables `CI_JOB_ID` and `CI_SERVER_URL` beside `HUSHRUN_ID_TOKEN` or `CI_JOB_TOKEN`,
whatever `HUSHRUN_PROVIDER` says and whether or not `CI` is set (a container a step starts, handed
the job's variables). Such a run is CI for build blocks (B-31). One function reads the variable
(`gitlab_job::read`) and decides one of three things, which nothing else decides; the run keeps
the answer in one place (`results::GitlabCi`, the credential its build requests carry):

| `HUSHRUN_ID_TOKEN` | What the run does |
|---|---|
| unset or empty | shares nothing; where the results lookup says the org shares, says once `build reuse is off for this run: the job can't mint an ID token (declare `id_tokens: {HUSHRUN_ID_TOKEN: {aud: <URL>}}` in the job)` |
| not a JWT, an issuer other than `https://gitlab.com` exactly, an audience that isn't one string equal to the backend's URL as the run names it, compared exactly as the backend compares it (a trailing slash is another audience; an array that holds it is in another service's hands too), no `job_id` or `jti`, expired, or no `CI_JOB_TOKEN` beside it (B-49) | sent nowhere; shares nothing, and says why once, with how to declare it, whatever the org does: the job asked to share by declaring it |
| gitlab.com's, for the backend's URL, not expired, with `CI_JOB_TOKEN` | the bearer of every build request (B-26), signed with the job's key (B-49) |

A usable token goes to no server but the one its audience names: a repository's files can name
any backend, and a token sent elsewhere could be used at hushrun's until the job's timeout. It is
never renewed (GitLab gives a job one): no 401 retry, and the relay sends nothing with it once it
expired. On the run's results lookup it goes in `Hushrun-Id-Token`
(`hushrun_core::blocks::ID_TOKEN_HEADER`) beside the token that is the lookup's bearer, since
results take no GitLab ID token: the answer's `Hushrun-Build` is then CI's for the job's project,
so a job of an org that doesn't share, or of a project nobody confirmed, makes no build request.
A job with no token to look results up with lets `open` say whether the org shares, as B-25's run
of several scopes does (`BuildAccess::open_decides`): one `open`, as silent as ever when the org
doesn't. In a GitLab job a person's or an agent's token reads nothing of the shared store and
uploads nothing: the relay's requests carry the job's ID token or none
(`results::Backend::build_access`), which is the CLI's rule, since the backend can't tell where a
token runs unless the request says so. A container handed an agent token and none of GitLab's
variables is a sandbox to hushrun as to the backend. The store is the run's own (B-31), and a
`target/` folder restored from GitLab's cache is keyed by its content (B-6, B-31). `open`'s
`untrusted` may also be `job`, `token`, `fork`, `source`, `unprotected`, `config` or `user`, each
said in one line (`untrusted_words`), and its `event` is said by gitlab.com's rule, not GitHub's:
on a listed branch a push is trusted, and a schedule, web or API pipeline only where the project's
admin chose **Trust pipelines started by a person**. *Rests on:* the backend verifying the token
(its signature by gitlab.com's keys, its issuer, its audience, its CI claims, its expiry by the
backend's clock) and deciding what the job may write: this rule only decides whether to send it. *Breaks if:* the
backend's URL as the run names it is not the audience the backend checks (a proxy in front of
it, another spelling of the host): every job then says its audience is wrong.

**B-49 A GitLab job proves it is the job.** A GitLab ID token works until the job's timeout, also
after the job ended, so a copied one could write. A GitLab job's build requests are therefore
signed with a key of the job's own, which its first `open` binds the job to on the backend; a
request with another key withdraws the job, and the backend then deletes what the job wrote and
refuses its later requests (its rule). So the key must be the same in every `hushrun run` of the
job, whatever its temporary folder, current folder or container, and a key nobody else can make.
It is **derived, never stored** (`gitlab_job::JobKey::of`): the Ed25519 seed is HKDF-SHA256 with
the salt `hushrun.gitlab-job-key/v1`, the job's `CI_JOB_TOKEN` as input key material and
`<job_id>\n<jti>` of the ID token as info. GitLab gives every step of a job the same
`CI_JOB_TOKEN`, masks it in logs and stops taking it when the job ends; a copied ID token alone
doesn't give the key. A file under the temporary folder (the first design) followed `TMPDIR` and
the container, so a step in a container of its own, or a tool that exported its own `TMPDIR`,
made a second key and withdrew its own job, its first runs' uploads with it. With no
`CI_JOB_TOKEN` (a container that wasn't handed it) the run shares nothing and says why: every
request of a GitLab job, a read included, must carry the job's key, and any other key would
withdraw the job, so there is no reading without it. The backend needs only the public half to
be the same in every request of the job.

Every build request, `open` first, carries `Hushrun-Job-Key`, the public half as a machine's is
written (`ed25519:<base64>`), and `Hushrun-Job-Signature`, standard base64 of its signature over
`hushrun_core::blocks::job_message`: `hushrun-build-job/v1\nPOST\n<path>\n<org>\n<job id>\n<SHA-256
of the body, lowercase hex>\n`, `<path>` `/o/<org>/<route>` (`/o/acme/v0/build/open`), `<job id>`
the token's `job_id`. Both are added in the one place every build request is sent
(`Client::post_bytes`), so no request goes unsigned. What a GitLab job writes as trusted is served
only once the job has closed a run with its key, so every `hushrun run` whose `open` answered
sends `close` at its end: when COMMAND ends, whatever its exit code, and when hushrun is asked to
stop (`build_session::run_command`). GitLab cancels and times out a job with SIGTERM, then kills it
after a grace period: while COMMAND runs, a stop signal (SIGTERM, SIGINT, SIGHUP) goes to COMMAND,
once, instead of ending hushrun; when COMMAND has ended, the run waits for no upload, sends `close`
within 2 seconds (`upload::STOPPED_CLOSE`), and removes its store. The signal goes to COMMAND's
process, not to processes COMMAND started that outlive it. A run killed outright (SIGKILL, a
machine that went) sends no `close` and has uploaded for nothing, which is the safe side; its
store is a dead run's (B-31). No `begin` starts with less than 2 minutes left of the token, since
nothing can be committed after its `exp`: the markers wait, untouched, and the run says once
`build: no more uploads this run: the job's ID token expires in less than 2 minutes`. A refusal
of the job (`job_withdrawn`) stops sharing and is said in plain words, with that the job's later
runs are refused too. *Rests on:* nobody but the job holding both its ID token and its
`CI_JOB_TOKEN`; HKDF-SHA256 and Ed25519. *Breaks if:* something in the job sends both tokens out
(a dependency's build script reading the environment): it can then sign as the job until the
token's timeout, as on GitHub code in the job holds the job's credential.

**B-32 Where a block came from.** Beside each block the store keeps a record, `origin.json`,
written before the block is published and rewritten whole under the block's lock when it
changes: **built here** (the run, and with a backend its host, the org and a fingerprint of the
credential, never the secret); **vouched for** (one entry per host, org and repository whose
server answered for the block, with the scope, the digest of outputs, the server's date and its
word: `served`, or `withdrawn` once it said it no longer serves it); **baked** (the manifest of
the baked store it was opened from, B-64, with an entry of vouched for whose host is
`hushrun:baked-store`, no org and no date, so that the server's word is recorded beside it as for
any download, and so that a hushrun from before baked stores, sharing the cache folder, reads the
block as a download whose server's word is stale, never as one built here); and whether it has been reused here yet. How a block stands for a reuse (any, fresh, stale or withdrawn) is one function
of that record (`Origin::standing`), which the wrapper and the relay both call. A download makes the first entry. A
block with no record, as every block from before this rule, is reused here as before and never
goes up. The relay publishes a download only to answer a wrapper still waiting for it, through
the store's own publish (first writer wins), never in shared `observe`. A block that was only
downloaded may give way to this run's download or compile; a block built here never does. A
compile whose own publish finds a block there compares outputs: a block built here is a second
opinion for every class (B-16), and a difference is treated as B-16 treats one; a block only downloaded
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
network does afterwards, and a run with no relay, which asks nothing, honours it too. **A run
that is not sharing by a decision reuses no block it only downloaded**, whatever its entry's
age: the org doesn't share (the results lookup's answer carried no `Hushrun-Build: on`, or
`open` said so), the person set this machine to `share off` or its local mode says so, nobody
is signed in, or the command is `hushrun run CHECK…`, which never shares (B-25). It is a miss:
rustc runs, and the block that compile builds takes the downloaded one's place and is from then
on built here (B-32), so a key is compiled once because of it. A run with no relay reuses a
download only when it would share and **its backend couldn't be reached**: its results lookup
got no answer of any kind, which the session records before COMMAND starts
(`session::Shared::Unreached`, the one state every wrapper of the run reads, set by
`build_session::unreached`); then an entry of any age counts, for the blocks this machine's own
hints lead to. **No answer of any kind** means no server gave an HTTP response: the connection
couldn't be made, or timed out or was cut before a status line arrived, or the credential the
lookup needs couldn't be had because nothing answered the request for it. Every other outcome
that is not a well-formed answer saying the org shares is a decision that it doesn't, and the
download is compiled once: a redirect hushrun doesn't follow, any 4xx or 5xx, a body that is
cut, empty or not the JSON asked for (a proxy's or a captive portal's page), and a credential
request a server refused. One place says which it was, `HttpLedger::answered_any`, set where a
response's status line arrives. In a run with a relay, a download
is reused without asking a server while its entry for this run's host, org and repository is
under 24 hours old, unless the relay says sharing stopped for the run by a decision (`open`
said the org doesn't share or that the run's sharing is off, or a request was refused with 401,
403 or 404), which the wrapper asks it before each such reuse and the relay answers from its
phase, at once and without the network; a relay that doesn't answer is no decision. On an older
entry the wrapper asks the relay, which waits for `open` and then for the server's answer,
however long they take (ADR 0144: no budget). "Served, with these outputs" lets it be reused.
**The server can't be reached** means `open` failed: only then does an entry of any age count,
and the block is reused. Once `open` has answered, a stale entry is never enough: the wrapper
waits for the server's answer and, without one (a lookup that failed, or a relay resting after
failures, which asks nothing new), it is a miss: rustc runs, and the block that compile
builds takes the downloaded one's place and is from then on built here (B-32). The relay asks
the server, without fetching, about every block in the store only downloaded whose entry for
this run's host, org and repository is stale, as the store's records list them, as soon as
`open` answers (`Relay::ask_vouches`), in as few lookups as the lookup's limit allows; one runs
at a time, and a question about a block it didn't name waits for it to end and then starts the
next. A lookup that panics is over when it panics, as one that returned is (`ask_vouches` marks
it so however it ends): a question about the block it was started for is a miss at once, the
next question about a block no lookup named starts one, and the end of the check waits for it
no longer. At the end of a check it asks again, within the uploads' cut, about every downloaded block
the check reused that no lookup of the run answered for, so that a machine in daily use never
goes stale. Every answer is written into the block's record: served with its outputs renews the
entry, anything else is a "no". On a person's machine a downloaded block's first reuse, of any
class (B-16), is audited one time in ten, which the record notes, then one in fifty as any (B-16). A difference
found by an audit or a comparison of a downloaded block, and a dependent that can't load one
(B-15), suspend its first key for `keep_days`, not until `hushrun build clean`, and are reported
to the scope the record names: `differs`, `rejected`, or `unreadable` for a pack that failed its
checks. **A linked output is the exception to the suspension.** A library is written by rustc
alone, so two that differ under one key mean one was built from other inputs than the key
names. A program, a proc macro or a build-script binary is written by a linker, and two that
differ mean the two machines' link environments differ in something B-37 doesn't hold; what this
machine's linker makes is what it would make every time. So when an audit, a comparison in
shared `observe` or a compile finds such an output different from a copy that was only
downloaded, and when `begin` refuses one this machine built with `differs` (B-29), the machine
**keeps its own and keeps the first key to itself** for `keep_days`: its block takes the
downloaded one's place (or stays, for a refused upload), nothing is suspended, and until the
date no block of that first key is asked of the shared store, fetched from a manifest, reused
from a download or offered for upload, while this machine's own is reused here as always. The
mark is one file beside the first key's hints (`local`, with its end; `hushrun build clean`
clears it, and the sweep removes an ended one), read by the wrapper's lookup and by the relay's
prefetch; the run says so in one line, and the report to the scope is `link_differs`, a word a
server that doesn't know it records nothing for. Each block's description holds its link
environment as a list (B-37), so two that disagree can be laid side by side. *Rests on:* the server answering a lookup only for blocks it still serves to this
caller, so that a withdrawn block stops being reused on laptops within a day without a list of
withdrawals, and a "no" from it being final for that copy; a lookup without fetching counting as
a use, so blocks in daily use don't expire (a stale block the run doesn't reuse, asked about with
the rest, counts as used too); and a slow answer being worth its wait, since a reuse the server
didn't vouch for is never one. *Breaks if:* a server says
"no" by mistake (a lookup answered for the wrong repository): the block compiles here from then
on, a cost and never a wrong reuse; or the results lookup is answered by something that is not the
backend (a proxy's error page): the run takes that for an org that doesn't share, and compiles
its downloads once.

**B-38 A downloaded program is checked before it may start.** Before a build-script binary or a
program that was only downloaded is put in place, each time, its headers are read to tell
whether it can start here. **Nothing is run**: not the program, not the loader it names, no
tool. The file came from another machine, so every path in it is data, never a command. Cargo
runs a build script itself, so no rule that reads rustc's errors sees one fail to start (B-15):
the build fails with "failed to run custom build command", cargo takes the binary for up to
date, and every later build fails the same way until the target folder is cleaned. One function
decides, by the file's own format, and only for a compile whose target is this machine's (a
program for another target is not started here).

- **ELF**, where this system starts ELF. Its class, byte order and CPU (`e_machine`) are this
  machine's. The file is whole: the program-header table, the section-header table, and every
  segment and section that takes bytes of the file lie inside it. Its loader (`PT_INTERP`),
  when it has one, is an absolute path without `..` in one of the system's own folders (`/lib`,
  `/lib64`, `/usr/lib`, `/usr/lib64`, `/libexec`, `/usr/libexec`; Android's `/system/bin` and
  `/apex`), both as written and as it resolves through links, and is a regular file with a run
  bit: never a file of the workspace, of the store, or of any other place a download or a
  checkout can write. Each library it needs (`DT_NEEDED`) is a regular ELF file for this CPU,
  the first of that name in the program's run paths (`DT_RUNPATH`, or `DT_RPATH` when there is
  none; `$ORIGIN` is the folder the program is put in; a folder with any other `$` word is left
  out), then in the folders the system's loader searches (those `/etc/ld.so.conf` lists, then
  `/lib64`, `/usr/lib64`, `/lib`, `/usr/lib`). Each symbol version the program requires of such a
  library (`DT_VERNEED`, weak ones excepted) is one that library defines (`DT_VERDEF`). A
  static program has nothing to find.
- **Mach-O**, where this system starts Mach-O. The file is 64-bit and for this machine's CPU, or
  a universal file whose part for this CPU is; a program for the other CPU only is not started
  (not through Rosetta either). It is whole: its load commands, each segment's bytes and each
  table of `__LINKEDIT` (symbols, strings, signature, fixups) lie inside it. Its dynamic loader
  (`LC_LOAD_DYLINKER`) is `/usr/lib/dyld`. Each library a load command names, weak loads
  excepted, is a Mach-O file for this CPU where dyld looks (`@rpath` through each `LC_RPATH`,
  `@loader_path`, `@executable_path`, all from the folder the program is put in), or an image
  of this system's dyld shared cache, whose list of image paths is read from the cache file
  (`/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/`, `/System/Library/dyld/`): a
  path under `/usr/lib/` is no proof by itself. Where no cache can be read, a library under
  `/usr/lib/` or `/System/` counts when `SDKROOT` names an SDK that has its stub (`.tbd`).
- A file of the other format, or of neither (a script), is not a program this system starts.
  On Windows nothing is checked: no linked output is stored there.

A program that fails any of this is **a miss**, not a broken build: rustc runs, the block that
compile builds takes the downloaded one's place and is from then on built here (B-32), and the
run's log says why. Nothing is suspended or reported: the block is right for the machine that
built it. A proc macro is not checked: rustc loads it, and B-15 acts on its error. *Rests on:*
the kernel and the loader refusing nothing this reading accepts (shown on real programs: one
the system's compiler links passes; cut in half, built for the other CPU, or loading a
`/usr/lib/` library that is not there, it is a miss). *Breaks if:* a program starts and then
fails on something no header names (a library it opens by name while running, a minimum OS
version newer than this system); or needs a variable only cargo sets when it runs it
(`LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH` for a library in a build's own folder), which this
reading never consults, or a loader outside the system's folders (Nix, Guix): such a program
is compiled here once though it would have run, which costs a compile and never a wrong block.

**B-35 `build: env:`.** A repository's `hushrun.yaml` may name variables under a top-level
`build: env:` list; every compile of that repository is keyed by their values (B-5), on every
machine and in both forms of `hushrun run`, and the list is part of no check's definition and no
run's key. It is for a build that reads a variable without telling the compiler (a proc macro
calling `std::env::var`), whose value would otherwise be in a block's bytes and not in its key.
*Rests on:* the project knowing which variables its build reads; a variable it reads without
naming keeps the block on this machine only when it looks like a secret (B-24), and is otherwise
caught only by the audit (B-16).

## Baked stores

A **baked store** is one repository's compiled crates, Go packages and C and C++ objects for one
machine's platforms, written once as a folder that an image, an archive or a read-only volume
carries, so that a sandbox or a container starts with them on its own disk instead of downloading
them at boot (B-36). It holds the blocks' sealed packs (B-28) and sealed hints (B-27) exactly as
they travel to and from a backend, and one file that says what they are. A run reads it underneath
its own store, which stays the only place it writes.

**B-63 Baking, and when a baked store is trusted.** `hushrun build bake --out DIR [--from
store|backend] [--check NAME]... [--trusted]` writes, for the repository it runs in, every
**check**'s compiled code (only those `--check` names, when it names any), and for each check one
**part** per kind and platform this machine builds: `rustc/1` for the Rust compiler's own target
triple (`rustc -vV`'s `host:`), `go/1` for `GOOS_GOARCH` in a Go module, and `cc/1` for each target
triple a run compiled C or C++ for (from a backend, the C compiler's `-dumpmachine`):

- `packs/<address>.pack`: each block a part's newest manifest (below) names that the source holds
  and that may be baked, **once**, whichever checks name it. From this machine's store, only a
  block that may leave any machine: a portable crate (B-24), a Go archive (the store keeps only
  portable ones, B-42) and an object with no reason to stay (B-56); never a block bound to the
  folders of the person or machine that baked it, never one whose description can't be read,
  never one with another reason, and never a download a server withdrew (B-34). From a backend,
  what it serves. The packs are the source's own, unchanged and sealed: whoever reads the folder
  without computing a block's key reads ciphertext.
- `hints/<first-key address>/<SHA-256 of the sealed bytes>`: each hint of the `rustc/1` and
  `cc/1` first keys, sealed with its first key (a Go action id is its own key).
- `baked.json`, schema `hushrun.baked-store/v1`: `trust`, the store's `manifest` (the SHA-256 of
  each check's id and its parts' kinds, platforms and manifest identities, sorted), the `source`,
  the repository, `platform` in a tag's words (`linux-x64`, `linux-arm64`, `macos-arm64`),
  `checks` (each one's id, or none for the repository's newest build of any check, and its `parts`:
  kind, platform and manifest identity), the `toolchains` (`rustc -vV`'s first line and `host:`,
  `go version`), one row per pack (its kind, address, size, SHA-256, digest of outputs, class, the
  compile's cost, from a backend its scope, and the digest of a linked output's link environment,
  B-37, where its description has one), and when it was baked. Nothing in it is a folder of the
  machine that baked it, in plain text or as a digest: a digest of a home folder is a dictionary
  lookup away from its user's name, and no reader decided anything by one.

A reader refuses a file of another schema, with a field it doesn't know, with no `trust`, with a
check named twice, a part of a kind it doesn't know or a platform `is_platform` doesn't read, two
parts of one kind and platform in one check, a row of no part's kind, **a pack named twice** (two
rows of one address could each say something else of it, and a reader that checked one could open
the other), a `manifest` that isn't its checks', or a combination no bake writes: `trusted` with
the source this machine's store, or `trusted` with a row whose scope is not a trusted scope
(`blocks::is_trusted_scope`, `protected/…`; this is the claim's check, the evidence's is the
repository's own scope, below).

Nothing else goes in: one function (`baked::check_contents`) walks the folder and refuses any
other file (but `baked.sigstore.json`, a store's signature, B-67), folder, link or special file, a
pack or a hint that doesn't begin as one, a pack `baked.json` doesn't name, and a row whose pack
isn't there. One bake of a folder at a time: a bake holds the folder's lock, `.<name>.bake.lock`
beside `--out`, from its check of `--out` to the exchange (a second bake of it says another bake
holds it; the lock goes with the process however it ends, and stays as a file). It writes into a
new folder beside `--out` under a name no other process uses (128 random bits, never a process
id), checks it, writes every file and
folder of it through to the disk, and puts it in place (`baked::put_in_place`): exchanged with a
store already there in one step where the file system can (Linux, macOS), so both names always
name a whole store, else the old store renamed aside first. Under the lock, whatever lies beside
`--out` is a dead bake's, whatever its process id says (containers on one volume see other
numbers): first a store renamed aside while `--out` is missing goes back, and every other leftover
is removed (`baked::reconcile`, which asks for the lock by its type). `--out` must not exist, be
empty, or hold a baked store: bake never deletes a folder of anything else; a process other than
a bake that writes into `--out` meanwhile loses what it wrote with the old store, since `--out` is
bake's own. Blocks a manifest named that couldn't be had (no longer in the store, not downloaded in
time) make the store **partial**: it is written and said; a store for this machine exits 0, routine
as blocks get swept, and one others start from (an image, B-65, an archive or a disk image, B-71,
or `--from backend --trusted`) exits **11**, never a registry's 7. A failure past the flags (the disk, the store) exits 1.

**The newest manifest** of each check is the source's, never a clock's:

- **This machine's store** (`--from store`, the default; no network): a run of a check (`hushrun
  run NAME -- COMMAND`, or a check of hushrun.yaml run here) with a session leaves its manifests in
  the store when it ends, `manifests/<digest of the repository, the check's id and the platform in
  a tag's words>.json` (so a GNU and a musl hushrun share one; with no repository, the checkout's
  digest too, which never leaves the machine), schema `hushrun.build-manifest/v2`: per kind and
  platform the pairs `close` sends (B-30), by key. Saving takes the file's lock (waiting 30
  seconds at most, then saying another run held it), reads it, merges and writes it whole, synced,
  under a name no other process uses: cargo calls the compiler only for what changed in a warm
  target folder, so a run alone names part of its build, and the file keeps the last pairs in their
  order, each first key the run compiled given its new block in place, the run's new pairs after,
  and a pair whose block the store no longer holds, or holds unused for longer than the kept days
  (B-14), left out. A file that can't be read is set aside under a name that says so, never
  silently replaced, and the run says so. A bake reads every check's file of the repository and
  platform, or those `--check` names. A store baked from it is **untrusted**: its blocks were built
  or downloaded by a laptop, a sandbox or a job, for whoever baked it, and `--trusted` is refused.
  In a CI job whose commands each get a store of their own (no job folder to share, as in GitLab
  CI, B-31), no run's blocks are in the bake's: a bake that reads that store (`--from store`, the
  default, but with `--deps-only`, which bakes its own build's store, B-72; and `build manifest`'s)
  is refused, exit 2, naming `--from backend`. So is one in a CI job with no backend named
  (`build_session::ci_without_backend`), whose runs kept nothing (B-2, B-25): the refusal says
  why and what works there, `--deps-only -- COMMAND` (B-72), which bakes the third-party crates
  from a build of its own, or, with a backend that shares build blocks, `--from store` or
  `--from backend`. What a bake bakes is one decision (`build::BakeSource`), which the refusal
  and the bake both read.
- **A backend** (`--from backend`, with the credentials `hushrun build fetch` takes): for each
  check `--check` names, else each check hushrun.yaml names and each check CI's files run through
  `hushrun run NAME -- …` for a push to the default branch (read as B-75 reads them), else none (the
  repository's newest build of any check; a backend that lists a repository's checks is the
  server's to add), and each
  part, the manifest `open` answers for the repository, the check, the kind and the platform, and
  what it names that the backend serves, downloaded as B-36 downloads it, programs too (B-30). An empty manifest is no
  part, and a check with none is left out. It claims trust when every pack came from the
  repository's trusted scope, the `protected/…` scope `open` names among those the bake reads;
  otherwise it is untrusted, unless `--trusted`, which leaves every other pack out.
- **A team's own bucket** is no source yet: hushrun has no bucket protocol for blocks.

A manifest's **identity** is the SHA-256 of `hushrun.block-manifest/v2`, a newline, the kind, a
newline and each distinct pair's two addresses, the pairs sorted (`baked::manifest_id`): a
backend's manifest and the one a machine saves for the same run have the same identity, and so do
two builds of the same code whose compiles ended in another order, as cargo's parallel build
makes them. The order is the prefetch's (B-30), never the identity's: hashed in order (`v1`), the
identity, and the fixed tag a store's identity names (B-66), moved with no change to the code.
`baked.json` holds its rows sorted by kind, then address, never in the order compiles ended. A source keeps one newest
manifest per repository, check, kind and platform, so a manifest it names now for a check's part
whose identity isn't the part's, and that names any pair, is **newer**: it replaced the store, and
any one check's newer manifest replaces it. An empty one replaces nothing. The store's identity
(`baked::store_manifest`, the one function `bake` and `build manifest` take it from, B-68) covers
every check it holds, never its rows, so it is known before a bake.

**Trust is the run's evidence, never the file's word.** `trust` in `baked.json` is a claim: a
folder anyone could have written says it as easily as a bake. A run trusts a block of a baked
store only on evidence of its own (`baked::Evidence`), and every piece of evidence names the
SHA-256 of the `baked.json` bytes it was given: the state reads the file once and takes evidence
only for those bytes:

- **The server's word.** A run whose relay talks to the backend the store names as its source, for
  the store's repository, which is the run's, checks the store as its check begins, before
  COMMAND starts and so before anything is reused: it waits for the check's `open` and looks every
  pack up (B-36's lookup, fetching nothing, the lookups of all its kinds 8 at a time so a large
  store's last rows are asked as soon as its first), and asks `open` for each part of each check it
  names, each until it answers or fails (ADR 0144: no time limit). A row the backend serves in the
  repository's trusted scope (the one `protected/…` scope `open` names, `protected/<repository
  id>`, never another repository's; two such scopes are no evidence, since which is the
  repository's would rest on their order) with that row's digest of outputs, SHA-256 and size is
  **verified** (its class, cost and link environment are the store's claim, which the key
  decides nothing by). Evidence is per block: a lookup that fails leaves out only the blocks it
  was for; an `open` that fails, or no trusted scope in it, is no evidence and no replacement. The server's word comes first even when it vouches for
  fewer rows than a signature covers (a signed store older than the backend's kept days reuses
  only what the backend still keeps): the backend is what knows a block was withdrawn. Each opened block's record carries the server's word (B-32). A
  newer manifest marks the store replaced (below) before the run reads it.
- **The person's say-so.** `HUSHRUN_BUILD_LOWER_UNTRUSTED=1` accepts every block of the store as
  it is, in a run that writes nothing others trust (`baked::authority`): not in CI, whose results a
  repository counts as CI's, and not with a relay whose server lets it write the repository's
  trusted scope, or hasn't said (`HUSHRUN_PROVIDER=local` in a job changes neither). The say-so
  is in the run's key (C-9's `baked_accepted`, the SHA-256 of that `baked.json`), so a pass it
  rests on is never a result CI, or any run without it, reuses.
- **A signature** by a signer the repository's hushrun.yaml names, over `baked.json` (which names
  every pack's SHA-256) and the store's hints, checked offline as the run begins, of a store baked
  no earlier than `build: baked: not_before` (B-67).

The rows a run may open (every row under a signature or a say-so, the verified ones under the
server's word, from the bytes the state read) are written once, sorted, into the run's session
folder (`baked-rows`, fixed-width lines read by a binary search); no wrapper, Go cache program or
launcher reads `baked.json` again, so what the run verified is exactly what it opens. A block
reused without asking is reused only while the run's store is current and is the very `baked.json`
its record names (by that SHA-256). CI accepts no say-so: a block it reuses that no server vouched
for could become the repository's trusted blocks through what is built on it. So in CI only
verified blocks, or every block of a store a signer hushrun.yaml names signed (B-67), are opened,
and a dependent compiled on one has the server's word or that signature under it before it goes up
(B-29, B-31). A store from this machine's store, read with no say-so and no signature, or from a
backend that doesn't answer, is untrusted for the run.

**Replaced.** A newer manifest of any of its checks marks the store replaced in this machine's
store, `baked/replaced/<manifest>`, written whole; the sweep keeps marks (B-14). The run that
learns it says so in one line, and decides with it: a fresh sandbox's or a CI job's empty store
learns it on its first run, as the check begins. A check of none is checked for evidence and never
for a newer manifest: the repository's newest of any check would replace it at every other run. A
run with no relay (offline, no backend, `hushrun run CHECK…`, an org that doesn't share) learns
nothing, and the store stays as it was.

**Its state for a run** is one function's answer (`baked::state`), which the run, `hushrun build
status`, the relay (through the run's session) and `hushrun build bake` all call, from the folder
`HUSHRUN_BUILD_LOWER` names, `baked.json`, the run's repository, the platform of each kind here
(the Rust compiler's target triple for `rustc/1` and `cc/1`, `GOOS_GOARCH` for `go/1`), the
evidence above, whether a backend was asked, and the marks in its own store; no clock:

| State | When | A run |
|---|---|---|
| absent | `HUSHRUN_BUILD_LOWER` unset or empty, and nothing at `/hushrun` | reads nothing |
| invalid | not an absolute path; under the run's own store or holding it; `baked.json` missing, unreadable or refused; a name the folder holds that a baked store never holds; at `/hushrun`, owned by another user than root or the run's, a link, or writable by anyone (B-65) | ignores it |
| wrong repository | baked for another repository than the run's | ignores it |
| wrong platform | no part's platform is this run's for its kind: a target triple compared by its tag's words (`x86_64-unknown-linux-gnu` and `…-musl` are both `linux-x64`), `GOOS_GOARCH` exactly | ignores it |
| server decides | only `hushrun build status` and `bake` say it, which ask no backend: a store from a backend, with no signature or say-so here | (a run reads what that backend vouches for, or ignores it with no answer) |
| untrusted | no evidence above | ignores it |
| replaced | a mark says its manifest was replaced | reads it; a block from it stands as a download no server vouched for yet (B-34): reused on a server's word with a relay, or when the backend can't be reached, never in a run that isn't sharing by a decision |
| current | none of the above | reads it: with the server's word, the verified blocks, which stand as fresh downloads; with a say-so or a signature (B-67), every block, reused without asking |

A part of another platform is not read, and says so in one line; the others are. A run that
ignores the store says why in one line, never fails for it, and compiles as it would without it.
`hushrun build status` shows the state in words and, with `--format json`, as `baked`, with the
evidence and the checks. *Rests on:* keys coming from inputs, so that a stale store is slower,
never wrong: a block it lacks is a miss, and a block it holds is reused only under the key a
compile here computed; the source being the one that says which manifest is newest; and a
backend's lookup naming what it serves now. *Breaks if:* a person says so for a folder someone
else wrote: outside CI the say-so is theirs to give; the backend withdraws a block of a store read
on a say-so: its copy is reused until the store is baked again; or, with no backend to ask, an old
store a signer validly signed is put where a run reads it: it is evidence until hushrun.yaml's
`build: baked: not_before` is raised past it (B-67).

**B-64 A read-only store underneath.** `HUSHRUN_BUILD_LOWER` names a baked store (set and empty:
none); unset, `/hushrun` does when it holds a `baked.json` (B-65); a run whose
state is current or replaced (B-63) reads it under its own store, the **upper**, which stays the
only store it writes (B-13). Each kind looks in the upper first, then in the baked store's part
for its platform, and only then in a relay:

- **The wrapper**: its own hints, then the blocks they lead to; then, where the upper has no
  downloaded hint for the first key, each of the baked store's sealed hints of it, opened with the
  first key and written where downloaded hints live (B-30), followed; then a relay (B-26).
- **The Go cache program** (B-39), which a run sets only where it shares or a baked store is
  named: on a get whose action id the upper lacks, the baked store's `go/1` pack of it; then a
  relay.
- **The launcher** (B-60), for a target triple B-59 can check: its own hints, then hints
  downloaded earlier, then the baked store's hints of the first key and the second keys they give,
  each opened from its pack; then a relay.

A second key the upper lacks and the run's rows (B-63) hold is opened from its pack with the key
(B-28's checks against the row: SHA-256, size, outputs) and published into the upper, its record
(B-32) saying `baked` with the SHA-256 of the store's `baked.json`, and with the server's word in
the repository's trusted scope when that is the evidence; then it is reused there as any block found, and counted apart in
the run's summary, `(N from the baked store)`. A replaced store is read only in a run that shares
or would, since elsewhere nothing opened from it could be reused. A first key kept on this machine
(B-34) reads nothing from it.

A block opened from a baked store is never built here: it never goes up (B-29), gives way to a
compile here (B-32), and a difference an audit or a dependent finds is acted on as for a download
(B-15, B-16, B-34; its first reuse here is audited one time in ten, or never when its source is
verified, B-16). It is reused without asking only while the run's store is current on a say-so or a
signature and is the store the record names (`Origin::baked_in`, called through
`Current::may_reuse`, the one definition the wrapper, the Go cache program and the launcher decide
by); otherwise it stands as a download, whose server's word
the relay asks for and records. A copy already in the upper with the same outputs, only downloaded
or opened from an older baked store, stays where it is and names the current store and the
server's word from then on.

The baked store is never written, renamed, dated or removed: a run reads `baked.json` once, as it
begins, and then the session's rows, a hint folder and a pack, and nothing else. The sweep (B-14), `hushrun build clean` and the free-space
floor (B-22) are the upper's alone: housekeeping never touches the store underneath, and the
floor holds for what opening a pack writes into the upper (B-28's room), so with too little free
space a block that only the baked store holds is compiled. So a baked store works on a read-only
file system, a read-only bind mount and a folder whose files nobody may write.
*Rests on:* the packs being sealed with each block's own key, so the folder holds nothing a
compile here couldn't make, and the upper confirming every reuse (B-6, B-15, B-41, B-59) as for
any block. *Breaks if:* the upper's volume has less than the floor free (a small sandbox disk):
the store is read and nothing it holds can be used; a baked store is replaced on disk while a run
reads it: the run opens only the rows it read, and a pack whose bytes aren't its row's is refused
and compiled; or a block bound to the toolchain's place holds a toolchain view without sources (B-17),
which lies under the store that made it: it is reused only where the upper store's own folder is
the same (the same `HOME` or `HUSHRUN_CACHE_DIR`), and elsewhere compiles.


**B-65 A baked store as an image.** `hushrun build bake --oci REPOSITORY` publishes the store it
bakes as an OCI image to a repository of any registry (`--oci-dir DIR`: an OCI image layout in a
folder, no registry), built by hushrun, never by `docker build`. `--out` may then be left out: the
store is written in a folder of this bake's own in this machine's store's temporary folder (a name
no other bake uses, never a process id) and removed however the bake ends; a killed bake's goes
with the store's sweep of old leftovers. The image
holds the baked folder at **`/hushrun`**, and a run with `HUSHRUN_BUILD_LOWER` unset reads a store
at `/hushrun` when it holds a `baked.json` (one `stat` when nothing is there), the folder, the
file, the `packs/` and `hints/` folders and each folder of hints belong to root or the run's user,
none is a link, and no other user may write them, neither anyone nor the group (one on a file
system mounted read-only, `statvfs`'s `ST_RDONLY`, needs no owner and no modes: nobody can write
it, and a user-namespaced container sees a root-owned mount as owned by nobody) (else it is invalid,
B-63: another process of the machine may have put it there; a volume mounted group-writable, as
Kubernetes' `fsGroup` mounts one, is read when `HUSHRUN_BUILD_LOWER` names it, the person's
choice, which this check doesn't apply to), so one line takes it into an image:

    COPY --from=<repository>:hushrun-<platform>-latest /hushrun /hushrun

The image's config names the OS and CPU of the store's `platform` (`linux-x64` is `linux` and
`amd64`, `linux-arm64` `linux` and `arm64`, `macos-…` `darwin`), and nothing else but the layers'
digests and one label, `dev.hushrun.baked-image`, whose value is JSON, schema
`hushrun.baked-image/v1`: the store's `manifest`, its `platform`, each layer's `role` and its
`files` (path, size, SHA-256), the `baked.json` rows of the workspace layer's packs,
`compacted_ms` (when the workspace layer was written: the baking time of the bake that wrote it,
carried by each bake that keeps it) and, for a dependencies-only store (B-72), `deps_only: true`. No
`created`, no history: the same store makes the same config.

**Layers.** Every file of the store is in exactly one layer, at its own path, so layers never
overlap, and there are at most three, bottom first, each named by the descriptor's annotation
`dev.hushrun.layer`:

- **deps**: the packs, and their first keys' hints, of third-party code: a Rust block whose
  package (`CARGO_PKG_NAME`) Cargo.lock names only with a `source` (a registry or git); nothing
  else is placed here (a Go or C block, a block baked from a backend, whose rows don't say), so a
  block hushrun can't place is the repository's own. It changes only when they do: the lockfile,
  the toolchain or flags.
- **workspace**: the repository's own code as of the last **compaction**, reused from the previous
  image by its digest, with any of its packs this bake no longer uses kept in it (and their rows
  carried in the image's `baked.json` after the bake's own, so that every pack the image holds is
  named, B-63). The previous image is the one the tag this bake moves names (else the moving
  tag's), and only when it **counts** (`bake_image::image_counts`, the one check the fixed-tag
  shortcut below makes too): an image signature by a signer hushrun.yaml names (B-67), whose
  statement names this store's repository, platform, source and claim (a laptop's store never
  stands for a backend's trusted one) and a time no earlier than `build: baked: not_before`
  (`bake_image::not_before_holds`, as B-67 says: a keyless signature's timestamp, a key's
  signed `baked_ms`; a key's statement naming none counts only while `not_before` is unset). Its label and
  layer digests are then a trusted bake's, which checked its own previous image the same way. A row is carried only as the backend vouches for it now (outputs, pack SHA-256, size) in the repository's trusted
  scope (B-63's lookup): a withdrawn block is never carried, and a store baked from this machine
  carries none. A bake compacts it, writing it again from its own code, when there is no previous
  image or it can't be read or isn't signed so (said), with `--compact`, when the layer is as
  old as `build: baked: compact_every` or older (a week unless hushrun.yaml says otherwise; `0s`
  turns the age off), by the label's `compacted_ms` against the bake's own time (`baked.json`'s
  `baked_ms`), evaluated as the bake runs, an unrecorded time or one more than ten minutes past
  the bake's clock counting as old, when the previous layer isn't at the destination, when a path it holds has other bytes now, when a pack it keeps unused
  can't be carried, when packs no longer used are half its bytes or more, or when the recent layer,
  which every bake uploads again, would hold more than 64 MiB, or more than 1 MiB and a quarter of
  what the workspace layer holds. A hint it keeps unused costs nothing: no row names a hint.
- **recent**: the repository's own code the workspace layer doesn't hold, its **programs** (a
  pack whose row's class is a linked program: a binary, a test executable; these link everything
  below them, so most merges change them), `baked.json` and `baked.sigstore.json`: always there,
  and all a code change to the repository changes. A program is never put in the workspace layer and
  doesn't count against the recent layer's bound above: it would compact the workspace layer at
  every merge, which every pull would then take again. One a kept workspace layer holds already
  with the same bytes (an image baked before programs had a group of their own) stays there and is
  not put in the recent layer too: no path is in two layers.

An empty deps or workspace layer is left out; one function (`hushrun_artifact::image::plan`)
decides it all from the bake's files and the previous image.

**Layer bytes** are a pure function of the ordered list of the layer's files and their bytes
(`hushrun_core::blocks::image::chunks`), so that a backend can stream the same layer, file by
file, and get the same digest: an uncompressed tar (`application/vnd.oci.image.layer.v1.tar`), the
packs being compressed and sealed already. POSIX ustar: every file, and before them every folder
above one, once, sorted by path as bytes (a folder's path ending in `/`); one 512-byte header each,
with the path in `name`, or split at the first `/` from the left that leaves `name` at most 100
bytes and `prefix` at most 155 (the shortest `prefix` that fits, as GNU tar splits); `mode` `0000644` (a file) or `0000755` (a folder), `uid`, `gid` `0000000`, `size` in 11
octal digits (`00000000000` for a folder), `mtime` `00000000000`, each then NUL; `chksum` the sum of
the header's bytes with that field as 8 spaces, in 6 octal digits, NUL and a space; `typeflag` `0`
or `5`; `magic` `ustar\0`, `version` `00`; `linkname`, `uname`, `gname`, `devmajor` and `devminor`
all NUL. A file's bytes follow its header, padded with NULs to 512; two 512-byte blocks of NULs
end the layer. A file of 8 GiB or more, a path that isn't relative printable ASCII without
empty, `.` or `..` parts, a path given twice, or a file's path that is also a folder above
another file, is refused.

**Publishing** reads the previous image (in a layout, its index's entry of that name; any failure
reading it is no previous image), asks once per blob whether the destination holds it (an error is
an error, never "absent"), uploads each blob it doesn't, so a code change uploads the recent layer
and the config alone, pushes the manifest by its digest, then the fixed tag when it takes one
(B-66), then the signature (B-67), and the moving tags last: a moving tag never points at an image
whose signature isn't there yet. A complete store whose fixed tag names an image that counts
(above) is published already: nothing is uploaded, and the moving tags go to
that image. "Signed" here is "counts", as above: after `not_before` is raised, an image of the
same store from before it is no shortcut, and the bake publishes a new one. Only a 404 or a 405
means a registry has no referrers API. Credentials are docker's (`config.json` and its credential helpers, as A-2's copy reads them), else a CI job's
own: in GitHub Actions `GITHUB_TOKEN` (passed in by the workflow) as `GITHUB_ACTOR` for `ghcr.io`,
in GitLab CI `CI_REGISTRY_USER` and `CI_REGISTRY_PASSWORD` for `CI_REGISTRY`. A bake settles the
registry's credential before it builds anything (ADR 0102, `credentials::settled`): in CI the job's
own first, else the Docker configuration's, read and its helper run then; it is the one used from
then on, so nothing a bake's build could write (a Docker configuration, a credential helper on
`PATH`) is consulted after it, and publishing to a backend's registry (B-70) reads no Docker
configuration at all. A layout's
`index.json` is read, changed and written whole under its lock. *Rests on:* a pack of one block
being the same bytes each time it is packed (B-28), so an unchanged block keeps its layer's
digest. *Breaks if:* two bakes of one repository and platform run at once: the later moving tag
wins, whichever store is newer (a CI job takes one group, B-69); or the registry drops a blob
the workspace layer reuses: the next bake finds it missing and compacts.

**B-66 Tags.** An image of a store is tagged, in its repository, for another program to read:

| Tag | Moves | Names |
|---|---|---|
| `hushrun-<platform>-<first 16 hex of the store's manifest>` | never | the first complete store with that manifest identity (B-63) a bake published there; a partial store takes none; a dependencies-only store (B-72) takes its own, its manifest naming the dependencies alone |
| `hushrun-<platform>-latest` | to each bake of CI on the default branch | the newest store of the default branch |
| `hushrun-<platform>-branch-<name>-latest` | to each bake with `--branch-tag` on the branch | the newest store a bake of the branch published |

The moving tag moves only from CI (not a pull request's run) on a branch the CI provider names the
repository's default branch (`bake_image::moves_latest`); a laptop's, another branch's or a pull
request's bake publishes the fixed tag (and the branch's tag, with `--branch-tag`) and says why the
moving tag stayed. The fixed tag never moves: a bake whose fixed tag names an image already moves
no tag there to another. When that image doesn't count for the store (B-65: not signed by a signer
hushrun.yaml names, another source or claim, from before `not_before`), the bake publishes its own
image without a fixed tag, moves its moving tags, and says so loudly, on stderr whatever the format
and in `--format json`'s `image.warnings`: anyone who can push to the repository can put an image
at a fixed tag first, since `hushrun build manifest` predicts it. A program that needs the store's
own image reads the moving tag, or checks the fixed tag's signature as a run does. An image
whose signatures can't be read at that moment is said in a note.

`<platform>` is `baked.json`'s `platform`: the baking machine's Rust compiler's target in a tag's
words (`linux-x64`, `linux-arm64`, `macos-arm64`), or a target triple with no short words. A store
with several checks and parts (`rustc/1`, `go/1`, `cc/1`, or C and C++ for several targets) has
one tag: the store's manifest is the digest of all its checks' parts, so any one's change is a new
fixed tag, and the platform is the baking machine's, whatever targets a part compiled for.
`<name>` is the branch without `refs/heads/`, each run of characters other than `a-z`, `0-9`, `.`
and `_` (an uppercase letter among them) one `-`, lowercased, `-` and `.` trimmed from both ends; a
name that comes out exactly as given and leaves the tag at most 128 characters is itself; any other
is cut to fit and ends with `--` and the first 16 hex of the SHA-256 of the branch as given
(`hushrun_core::blocks::image::branch_name`). A name kept as given never holds `--`, so it is never
another's cut form, and two cut forms are one only where 64 bits of SHA-256 are: two branches
never share a tag. A program reading tags may rely on that. The moving tag is the change signal: a program
watches it, or the fixed tag `hushrun build manifest` names (B-68). A stale image is slower, never
wrong: keys come from inputs.

**B-67 Signatures.** A bake that publishes signs, with what `--signing-key FILE` (PKCS#8 PEM),
else `HUSHRUN_SIGNING_KEY` or `HUSHRUN_SIGNING_KEY_FILE`, gives (asking no timestamp authority
unless `HUSHRUN_TSA` names one: a key's signature is checked against the key, so a bake signed
with a key contacts no outside service unless configured), else keylessly with an ID token at
hand (a CI job's, GitHub Actions' or `SIGSTORE_ID_TOKEN`, through Fulcio, with an RFC 3161 timestamp,
T-12, E-10) in CI, or outside CI where `HUSHRUN_SIGNING=sigstore` asks for it; else, or with
`--no-sign`, it publishes unsigned and says why. "In CI" is the one answer the bake reads for its
tags and its lines too (`gitlab_job::in_ci`), handed to the one function that decides what signs
(`App::bake_signing`, which gives the reason it doesn't with it): a bake told it runs outside CI
(`HUSHRUN_PROVIDER=local` in a job) signs with no ID token unasked, as it says. A bake that GitHub's
`workflow_run` started signs keylessly only when the event the job was given (`GITHUB_EVENT_PATH`)
shows the run it follows was a successful push of this repository to its default branch, at the
commit the job checked out (`GITHUB_SHA`), else it fails with exit 9
(`bake_image::workflow_run_signs`): its certificate names the default branch's workflow file and
the event `workflow_run`, never what started the run it follows, so a hand-written job without
B-69's condition would otherwise sign after a fork's pull request whose branch is named like the
default branch, or after a tag of that name. A person's machine key
and a browser sign-in never sign an image. Each signature is a Sigstore bundle (v0.3) around a
DSSE envelope of an in-toto statement (`application/vnd.in-toto+json`):

- **The store's**, `baked.sigstore.json` beside `baked.json` in the folder and the image:
  predicate type `https://hushrun.com/attestations/baked-store/v1`, one subject `baked.json` by
  the SHA-256 of its bytes, and a predicate with the store's `repository`, `manifest`, `platform`
  and `hints`, every hint file of the store as `hints/<first key>/<name>`, each named for the
  SHA-256 of its bytes. `baked.json` names every pack's size and SHA-256, so the statement covers
  every byte a run reads.
- **The image's**, a referrer in the registry beside the image, as cosign keeps one: an artifact
  manifest whose `subject` is the image's manifest, `artifactType` and one layer
  `application/vnd.dev.sigstore.bundle.v0.3+json`, an empty config, and annotations
  `dev.sigstore.bundle.content: dsse-envelope` and `dev.sigstore.bundle.predicateType`; with no
  referrers API, the registry keeps it under the tag `sha256-<hex of the image's digest>`, an index
  that lists it (the OCI distribution spec's tag schema). Predicate type
  `https://hushrun.com/attestations/baked-image/v1`, one subject named for the repository with the
  image's manifest digest, and a predicate with the store's `repository`, `manifest`, `platform`,
  the SHA-256 of its `baked.json`, the tags, and what B-65's check of an image reads: `baked_ms`,
  `source` and `trust` as `baked.json` has them. Anyone can verify it with cosign, for a key
  `cosign verify-attestation --new-bundle-format --insecure-ignore-tlog --key cosign.pub --type
  https://hushrun.com/attestations/baked-image/v1 <repository>@<digest>`; keylessly
  `cosign verify-attestation --new-bundle-format --insecure-ignore-tlog --use-signed-timestamps
  --type https://hushrun.com/attestations/baked-image/v1 --certificate-identity-regexp
  '^https://github\.com/<owner>/<repo>/\.github/workflows/' --certificate-oidc-issuer
  https://token.actions.githubusercontent.com <repository>@<digest>` (or `--certificate-identity`
  with the workflow's exact identity), and the store's `baked.sigstore.json` over `baked.json`
  with `cosign verify-blob-attestation --new-bundle-format --insecure-ignore-tlog
  --use-signed-timestamps --type https://hushrun.com/attestations/baked-store/v1 --bundle
  baked.sigstore.json` and the same identity flags, `baked.json` last. The bundle holds an RFC 3161
  timestamp, not a transparency log entry: `--insecure-ignore-tlog` says there is none, and
  `--use-signed-timestamps` checks the certificate at the timestamp's time. Without it cosign
  checks the certificate now, and Fulcio's lasts 10 minutes: `failed to verify leaf
  certificate` after that. A key's signature holds no timestamp unless `HUSHRUN_TSA` names an
  authority, so its command takes no `--use-signed-timestamps`.

**A run's evidence.** As a run begins, before COMMAND starts, with a store named and hushrun.yaml
naming signers under `build: baked: signers:` (each either `issuer` and `subject`, a keyless
certificate's OIDC issuer and subject alternative name compared exactly, or `key`,
`ecdsa-p256:<base64 SPKI>`, `ed25519:<base64>` or a PEM `PUBLIC KEY`), one function
(`bake_image::verify_store`) checks the store's signature: a key's signature over the envelope by a
key named, or a certificate the Sigstore trust root issued (`HUSHRUN_SIGSTORE_TRUSTED_ROOT`, else
the one in the binary; offline), valid at its timestamp, whose issuer and subject are a signer's
and whose run was started by a push, a schedule, a run by hand (`workflow_dispatch`, GitLab's
`web`) or the bake job's `workflow_run` (B-69, whose condition in the default branch's own file
admits only a push to it from this repository), never by anything else (an issue comment,
`pull_request_target`, another workflow's call), and never a certificate naming no event; the
statement the store statement; its subject the SHA-256 of the `baked.json` there, read once; and,
when hushrun.yaml sets `build: baked: not_before` (a time), a signing time no earlier than it
(`bake_image::not_before_holds`, the one rule for stores and images): for a keyless signature, its
timestamp, which an authority of the trust root made and the certificate's validity holds, never
the `baked_ms` the statement claims (a compromised workflow could sign a far-future one); for a
key's, the signed `baked_ms`, which is the key holder's claim. Then the store's evidence is **signed** (`baked::Evidence`),
the run's session lists the hint files the statement covers, and every block is opened and reused
without asking, in CI too: whoever can make that signer sign could write the repository's trusted
blocks. A signature that doesn't verify is no evidence, said in one line (`hushrun build status`
says it too). The server's word, when the store's backend gives it, comes first: a replaced store,
or a block the backend no longer vouches for, is then what the server says, signed or not. While
the run reads the store, the wrapper, the Go cache program and the launcher read the rows the state
read from the signed bytes (B-63), and a hint file only when it is listed and its bytes are its
name; a pack's SHA-256 is checked as it is opened (B-28). A bake checks the image it builds on with
the same signers (B-65): an image signature over that image's manifest, by the same rules.

What `build: baked:` names (its signers and `not_before`) is part of every check's definition (E-6)
and every run's key (C-9) when it names anything: a pull request that adds a signer has results
and evidence of its own, which its base never reuses. *Rests on:* hushrun.yaml being the
repository's: on a pull request it is the change's text, whose blocks reach no trusted scope
(B-29). *Breaks if:* a signer's key leaks: a store it signs is trusted until hushrun.yaml stops
naming it, and `not_before` is no defence (the key signs any `baked_ms`): taking the key out of
the signers is; or, with no backend to ask, an old validly signed store is replayed: it is evidence
until `not_before` is raised past its timestamp (keyless) or its `baked_ms` (a key); a keyless
identity can't be revoked otherwise.

**B-68 `hushrun build manifest`.** Prints the newest manifests of the repository's compiled code
as a bake would read them now, every check's (or those `--check NAME` names), from this machine's
store (the default) or the backend (`--from backend`, asking `open` and downloading nothing), with
the store identity a bake would write, from the same checks and the same function
(`baked::store_manifest`); with `--json` (or `--format json`) as `hushrun.build-manifest/v1`:

| Field | What |
|---|---|
| `schema` | `hushrun.build-manifest/v1` |
| `source` | `store` or `backend` |
| `repository` | the repository, as a run names it, or `null` |
| `manifest` | the store's manifest identity a bake would write, 64 hex (B-63) |
| `short` | its first 16 hex, as the fixed tag holds it |
| `platform` | the tag's platform words (B-66) |
| `checks` | each check's `check` (its id, B-26, or `null` for the repository's newest of any check) and `parts`: each part's `kind`, `platform` (a target triple, or Go's `GOOS_GOARCH`), `manifest` (its identity, 64 hex) and `blocks` (how many pairs it names) |
| `tags` | `fixed` and `latest`, the tags an image of it gets (B-66) |

A field this version doesn't write is never removed from `v1`; a reader ignores fields it
doesn't know. No manifest exits 2.

**B-69 The bake job.** `hushrun init --bake` adds a job that bakes after the default branch's build
and when run by hand, from the backend (`build bake --from backend --trusted`, every check, B-63;
in CI a run shares blocks only through one, B-31), signed keylessly as the job, into the
repository's registry. On GitHub, a workflow of its own, `.github/workflows/hushrun-bake.yml`: on
`workflow_run` of the build's workflow, completed on the default branch, and `workflow_dispatch`;
its job runs by hand only on the default branch, and after the build only when it passed for a
`push` of this repository (`github.event.workflow_run.event == 'push'` and the run's head
repository this one: never a fork's pull request whose branch has the default branch's name), and
daily on a `schedule` (B-73's way out of a held bake; its arm of the `if:` is
`github.event_name == 'schedule'` alone, since a scheduled run's event names no repository to
compare the branch with, and GitHub runs a schedule only on the default branch's latest commit,
whose ref is that branch: hushrun takes the ref's branch as the default branch for that event
alone, `hushrun_ci`); one `concurrency`
group; the checkout with the whole history (`fetch-depth: 0`, since B-75 compares commits);
`contents: read`, `packages: write`, `id-token: write`; `npx --yes hushrun@<range>` pushing to `ghcr.io/<owner>/<repo>` with `GITHUB_TOKEN`, or,
for a repository on hushrun's hosted backend (its URL `https://api.hushrun.com/o/<org>`),
publishing to `registry.hushrun.com/<org>/<name>` (B-70) with no `packages: write` and no token
but the ID token. On
GitLab, a `hushrun-bake` job appended to `.gitlab-ci.yml`: `stage: .post`, on pushes to the default
branch and pipelines started from the web or a schedule on it (the plan says to add a daily
pipeline schedule), the whole history (`GIT_DEPTH: "0"`), one `resource_group`, `id_tokens` for the backend
(`HUSHRUN_ID_TOKEN`, B-48) and Sigstore (`SIGSTORE_ID_TOKEN`), hushrun installed by
`ci/install-hushrun.sh` (vendored when missing), pushing to `$CI_REGISTRY_IMAGE`, or publishing to
`registry.hushrun.com/<org>/<name>` on the hosted backend. The merge of
init's change is itself a push to the default branch, so the first image is baked right after it.
The plan prints the Dockerfile line and the `build: baked: signers:` lines that name the job, for
hushrun.yaml. Without a backend (`--local`) it adds nothing and says why; a file or job already
there is left as it is.

The long-lived branches hushrun.yaml names under `build: baked: branches:` (the default branch
left out) are baked too, each into `hushrun-<platform>-branch-<name>-latest` (`--branch-tag`,
`--from backend` with no `--trusted`: a branch's blocks are outside the trusted scope), never
moving `-latest`, and each signed as the job **on that branch**: on GitHub `workflow_run` lists
them too, and since it always runs the default branch's file, whose keyless signature names the
default branch, a second job (`actions: write` alone) starts the workflow by hand on the branch
whose build passed for a push (`gh workflow run hushrun-bake.yml --ref <branch>`, the branch name
passed through a variable; a branch with no copy of the file fails that step saying to merge the
default branch into it, and the plan says each listed branch needs the file), whose run bakes and
signs as that branch's workflow; the bake job runs by hand on the default branch or a listed
branch, on the schedule, after the build only for the default branch (the trusted bake's step on
the default branch or the schedule, the branch's step never on the schedule), its
`concurrency` group (on the bake job alone, so a dispatching job never waits on or cancels a
bake) the branch's (`github.event.workflow_run.head_branch || github.ref_name`: a run after the
build and one by hand on the same branch share it). A branch name with `*`, `?`, `[`, `]`, `!` or
`+` is refused in hushrun.yaml: GitHub's `workflow_run` filter would read it as a pattern, where
the dispatch and GitLab compare it exactly. On GitLab the branch's own pipeline bakes, its job's ID token naming
the branch, one `resource_group` per branch. A run on the default branch never trusts a branch's
image unless hushrun.yaml names that branch's identity among its signers, which the plan says and
init never writes. hushrun's registry has no branch tags (B-70): the job there bakes the default
branch alone, said. The plan also says how to publish a first image of the dependencies alone
before CI's first build lands (B-72), and how to measure what an image saves (B-74).

The bake job builds what a skipped merge left unbuilt (B-75), so it carries the build's
toolchain: on GitHub, each step of the build job (the one that runs the build check, else the
first that runs commands) that `uses:` an action other than `actions/checkout` is copied after the
checkout (a step that only runs commands is not: the plan lists the job to check), with what CI
fills in that the bake job wouldn't filled in from the build job (`setup::replay::resolve`, the
function B-75 reads CI's files with: the job's primary matrix entry, and the workflow's, the job's
and the step's `env`; each value keeps its type, a plain scalar something was filled into becoming
a single-quoted string), while `secrets.*`, `vars.*`, `runner.*` and `github.*` but its event stay
as written, meaning the same in the bake job. A step's `if:` is an expression with or without
`${{ }}`: its `matrix.KEY`s are written as the primary entry's literals either way, and one that
reads another step's output, `needs`, `inputs`, `strategy`, `job` or the event copies nothing. Each
copied step gets the variables the build job gives every step (the workflow's `env:`, then the
job's, filled in the same way) where it doesn't set them itself. The job signs the image, so only
an action of GitHub's own (`actions/…`, `github/…`), pinned to a full commit (40 hex), a container
pinned by its digest, or an action of this repository (`./…`) whose `action.yml` (or `action.yaml`)
uses only actions this rule copies, and whose container image, if any, is pinned by digest, followed
into this repository's actions up to 4 deep (`setup::bake::pinned_deep`; one init can't read, one
outside the repository or one deeper is refused as one not pinned), is copied (ADR 0102); a step
that isn't, or that takes another step's output, an input, or a matrix only CI makes, copies
nothing: the job is written with `--no-build`, and the plan names each step and how to pin it
(`git ls-remote` prints the commit; init reads nothing over the network). When an action of another
owner pinned to a commit is copied, the plan says it can still use actions of its own by tag,
which init can't read. On GitLab the job takes the build job's `image:` (else
`rust:latest`) and its `before_script:`. The plan also says which folders a sandbox needs for the
blocks bound to CI's (B-76): on GitHub `HOME=/home/runner`, `CARGO_HOME=/home/runner/.cargo` and
the checkout at `/home/runner/work/<name>/<name>`; on GitLab the job's.

**B-70 Publishing to a backend's registry.** A backend may serve a read-only OCI registry built
from the packs it holds: `GET /v2/…`, the distribution spec's pull API and token flow, images named
`<registry host>/<org>/<name>`, `<name>` the repository's own name, the last part of its path,
lowercase, when that is one OCI path component (`hushrun_core::blocks::publish::image_name`). The
registry host is `registry.hushrun.com` for hushrun's hosted backend (`https://api.hushrun.com`)
and the backend's own host for any other (`hosted::registry_host`, the one definition bake and
init read). `hushrun build bake --from backend --trusted --oci hushrun`, or `--oci` with a
repository on that host (whose path must be the one the backend names), publishes there with one
request instead of pushing: `POST v0/build/images` (`hushrun_core::blocks::PublishRequest`) with
the relay's credential and headers (`open`'s, B-49's job signature on GitLab). It reads the
previous image from the moving tag as B-65 does, through the registry's token flow with the same
credential (in CI the job's own ID token, for its own repository's image), plans and signs as B-65
and B-67 do (the image statement names the fixed and the moving tag), and sends the image's
description: each layer's files bottom first, a pack by its block (`kind` and `address` from its
`baked.json` row), any other file by its SHA-256 and its bytes (one a reused layer keeps from the
previous image, which the backend holds, by its SHA-256 alone), the manifest, config and
signature it made, the moving tag's digest it read (`previous`, the compare-and-set's expected
value, `null` for none), and the workspace's commit (`git rev-parse HEAD`). Another registry
host is pushed to as B-65 says, and `registry.hushrun.com` for a repository on another backend is
refused, as are `--from store`, no `--trusted` and `--branch-tag` with the backend's registry (it
has no branch tags), before anything is baked. A partial store is not published there and exits
11: its image would take the fixed tag for good.

The backend computes each layer's digest from the bytes it holds with B-65's function and refuses
a manifest that names another (422 `mismatch`), accepts only packs its repository's trusted scope
serves (422 `missing_blocks`), and only from CI on the default branch (403 `not_publisher`); the
moving tag moves by compare-and-set on `previous`, only to a newer run (409 `stale`, 409
`not_newer`), and the fixed tag never moves: a store published before keeps its image, and the
moving tag goes to it (the answer names that image, and bake says so). The answer names the layers
the backend read: for the image sent, their digests and sizes must be the bake's, in order, or the
bake fails loudly. One loop decides every answer (`Relay::publish_image`): 503 (`busy`,
`unavailable`, `Retry-After` heeded, at most 30 s apart), 409 `duplicate` and a request that got no
answer are sent again as the same bytes, the same image being the same publish there; 409 `stale`
reads the previous image again, plans again from the image the backend names and sends that, once,
and a second `stale` stops with a line (another bake is publishing); 409 `not_newer` publishes
nothing and exits 0 with a line (a newer bake won, which is what the moving tag is for); every other
refusal fails the bake with what the backend said, a 403 saying that only CI on the default branch
publishes there and naming `--oci-dir` and `--oci REPOSITORY` for anywhere else; ten minutes in all.
`--format json` writes `image.hosted`: the image's `image`, its `state` (`published`, `unchanged`,
`not_newer`), `published_before`, `sent_bytes` (the last request's), `requests`, `sent_total_bytes`
(every request's) and `replanned` (a `stale` answer made it plan again, also said in a note). *Rests on:* B-65's layer bytes being a pure
function of the files, on both sides. *Breaks if:* the backend's object store holds bytes other than
a pack's row says: the backend checks every file's SHA-256 when it first reads a layer and refuses
the publish.

**B-71 An archive and a disk image.** `hushrun build bake --tar FILE` writes the store it bakes
as one tar, and `--erofs FILE` as one read-only EROFS disk image, with or instead of `--out`,
`--oci` and `--oci-dir` (with no `--out`, the store is written aside and removed, as B-65 says). Each
is the folder as it is once baked and signed: `baked.json`, `baked.sigstore.json` when the bake
signs, each pack `baked.json` names and each hint, nothing else. A bake that writes either signs as
one that publishes an image does (B-67: `--signing-key`, the signing variables, a CI job's ID
token, or unsigned with `--no-sign` or nothing to sign with, said), and signs the folder once,
before anything is made of it, so a run that reads the extracted or mounted folder has the evidence
a run that reads the image has, for the same bytes.

- **The tar** is B-65's layer bytes of those files under `hushrun/` (`hushrun_core::blocks::image::
  chunks`): every folder above a file once, sorted, ustar, folders `0755` and files `0644`, owner
  and group 0, every time 0. `tar -x -C /` puts the store at `/hushrun`, which a run reads with no
  variable (B-65); `tar -x -C DIR` at `DIR/hushrun`, which `HUSHRUN_BUILD_LOWER` then names. A
  `FILE` whose name ends in `.zst` is the same tar compressed with zstd, at level 3 on one thread;
  `-` writes the tar to stdout, and then what bake says, text or JSON, goes to stderr. Uncompressed
  is the default, since packs and hints are compressed and sealed already (ADR 0100 measured what
  zstd saves).
- **The disk image** is EROFS (Linux's read-only file system, which every kernel since 5.4 can
  mount, and `fsck.erofs` reads) whose root is the store, made by hushrun with no `mkfs.erofs`, no
  root and no loop device (`hushrun_core::blocks::erofs::chunks`, whose documentation gives the
  layout byte by byte): 4096-byte blocks; the superblock at byte 1024 with no checksum and no
  feature; inodes from the root at nid 36, breadth first, each folder's entries by name as bytes,
  compact unless a size needs 64 bits, never across a block; a file's whole blocks after them in
  that order and its tail inline beside its inode where both fit in a block; folders `040755` and
  files `0100644`, owner and group 0, every time 0; and a UUID made of the first 16 bytes of the
  SHA-256 of `baked.json` (an RFC 9562 version 8 UUID), so the same store has the same one.
  Mounted read-only at `/hushrun` it is read with no variable; at any other mount point
  `HUSHRUN_BUILD_LOWER` names it.
- **Both are a pure function of the store's files**: the same folder gives the same bytes on any
  machine. A file is written whole, under a name no other process uses beside it, synced and
  renamed into place, so `FILE` is always a whole archive or none; a failure leaves no temporary
  file. Two bakes of the same store differ only in `baked.json`'s `baked_ms` and in the signature:
  a bake that signs nothing takes `baked_ms` from `SOURCE_DATE_EPOCH` (seconds) when it is set, so
  two such bakes give the same bytes; a signed store's time is always the clock's, since
  `build: baked: not_before` compares it (B-67).
- **Who extracts it.** A tar extracted by root is root's, and one extracted by the user who runs
  the builds is theirs (its modes less that user's umask, which only takes bits away): both are read
  at `/hushrun` (B-65). A folder or file of it that its group or anyone may write, or that another
  user owns, is invalid there, and the line says how to fix it (`chmod -R g-w /hushrun`,
  `chmod -R go-w /hushrun`, or extracting it as root or as that user).
- A partial store exits 11, since others start from the archive (B-63). `--tar` and `--erofs` may
  not both write to stdout, nor name the same file. `--format json` adds `archives`, each with its
  `format` (`tar`, `tar.zst`, `erofs`), `path` (`null` for stdout), `bytes`, `sha256` (of the bytes
  written), `files` (the store's) and `unpacked` (the tar's or image's size before compression).

*Rests on:* B-65's layer bytes and the EROFS layout being pure functions of the files, and the
packs being sealed (B-28), so an archive or a volume is as safe to hand around as an image.
*Breaks if:* another zstd version compresses otherwise: a `.zst` file's bytes change, never the tar
inside; or a kernel has no EROFS (before 5.4, or built without it): `fsck.erofs --extract=DIR`
unpacks the image anywhere, or the tar serves instead.

**B-72 Dependencies only.** `hushrun build bake --deps-only [--check NAME] -- COMMAND` bakes a
store of a repository's third-party code before any build of its own has run, so that its first
image lands sooner: COMMAND (the check's cargo command, such as `cargo test --locked`: the
third-party crates are compiled for its profile, features and target; a `cargo test` runs as
`cargo test --no-run`, compiling its tests and running none, since rustdoc compiles doctests
without the wrapper, against the stubs; the doc tests are left out, `cargo test --doc` compiling
the library's tests instead, `--lib`, whose third-party crates are theirs, since cargo takes no
`--no-run` with `--doc`) runs, as every build of a bake runs (B-75's environment, through
`bake_build::in_bake` and `run_in_bake`: no credential of the job's, no result of its own, and on
Linux nothing it left running outlives it), in a store and a target folder of the bake's own (beside the bake, removed however it ends; never the person's
store, a relay or a baked store), its session (`session::begin_isolated`) naming a **stub**: each
compile of a package the nearest `Cargo.lock` lists only as a path package (no registry or git
source names it) whose `CARGO_MANIFEST_DIR` is inside the checkout has its one `.rs` argument
replaced by an empty source in the session's folder (one with `fn main() {}` for a `bin` crate
that isn't a test; `#![allow(warnings)]` in both), and is run as so changed with nothing looked up,
stored or recorded (`Stub::args`, which the wrapper calls before anything else of a compile). So
cargo's graph, features and flags are the real build's, every third-party crate compiles as the
real build compiles it, with the same keys (keys come from inputs, and no input of a third-party
compile is a stub), and the repository's own crates cost an empty compile. In a Go module, the
third-party packages `go list -deps -test ./...` names (neither the standard library's nor the main
module's) are built with `go build`, Go's cache program keeping them, both with the flags of a Go
COMMAND that change what they compile to (`-race`, `-msan`, `-asan`, `-cover` and its mode and
packages, `-tags`, `-trimpath`, `-gcflags`, `-asmflags`, `-ldflags`, `-mod`, `-buildmode`, `-pgo`;
`GOFLAGS` reaches them from the environment), a Go COMMAND itself never run (it would build and
test the module's own packages); C and C++ have no
dependencies apart from the project's own sources, and a repository with neither Cargo nor Go
refuses `--deps-only`. That store is baked as B-63 bakes this machine's (untrusted, signed as any
bake signs, B-67), with `--out`, `--oci`, `--oci-dir`, `--tar` and `--erofs` as for any bake; COMMAND
is required in a Cargo workspace, `--from backend`, `--trusted` and more than one `--check` are
refused, and so is a workspace with no `Cargo.lock`. It reads no CI command's store, so a GitLab
job, whose commands each have a store of their own, bakes it (B-63). The bake says how long the build took, how many
blocks it kept and how many crates it stubbed (`--format json`: `deps_only`).

A dependencies-only store is complete for its own manifest, which names the dependencies alone,
so it takes its fixed tag (B-66: that tag can never name a full store missing blocks). Its image's
label says `deps_only`, and it has no workspace layer, so its bake says nothing of one being
written again (`bake_image::rewritten`; `compacted` is `null` in `--format json`). It **never sets a full image back**: before building anything, the bake
reads each tag it would move (`-latest` from CI on the default branch, the branch's with
`--branch-tag`), and when one names an image whose label doesn't say `deps_only` it builds and
publishes nothing and exits 0 with a line (`bake_image::deps_only_waits`; an image whose tag can't
be read is an error); and it asks again once its image's layers and fixed tag are up, just before
its moving tags move: a full image that landed meanwhile keeps them, and a note says so (a plain
registry has no compare-and-set, so a full image landing between that read and the tag's write
can still be set back; the next full bake moves it forward). A full bake's image replaces a dependencies-only one as any newer image does
(the earlier one doesn't count for it, B-65: another source and claim, so it compacts, and an
unchanged deps layer is not uploaded again). *Rests on:* the stubbed crates being the repository's
own: no third-party crate depends on a path package. *Breaks if:* a `[patch]` replaces a crate a
third-party crate depends on with a path package inside the checkout: that third-party crate is
compiled against a stub and the build fails, said as COMMAND's failure; or the image is used for
another command than COMMAND's profile and features: its blocks miss, and compile.

**B-73 The re-bake limit.** A bake publishes no image while the image the tag it moves names
(else `-latest`'s) counts (B-65) and is younger than `build: baked: rebake_every` (an hour unless
hushrun.yaml says otherwise; `0s` turns the limit off), by its signed statement's `baked_ms`
against this bake's `baked_ms`, and fewer than `build: baked: rebake_blocks` (50) of this store's
packs are absent from every layer of that image (`Rebake::holds`, the one decision, evaluated as the
bake runs, never stored). It publishes nothing then, moves no tag, exits 0 and says why and that
`--force` publishes anyway (`--format json`: `image.state` `skipped` and `image.skipped`); the
relay fetches what's newer meanwhile (B-26). A fixed tag naming this very store is still taken as
the publish it already is (B-65), and an image with no signed time, one past the bake's clock
(never "recent"), or a dependencies-only image under a full bake (B-72: a full store replaces it
whatever its age) is no reason to wait. A bake started by a schedule (GitHub's `schedule`, GitLab's
`schedule` pipeline) publishes whatever the limit, as `--force` does: the job `init --bake` writes
runs daily for that. On GitHub a scheduled run's event names no repository: its ref, the default
branch's (GitHub runs a schedule there alone), is taken as the default branch for that event alone
(`hushrun_ci`, B-66's `moves_latest`), so the scheduled bake moves `-latest` and builds what's
missing as a push's would. The same holds before publishing into a backend's registry (B-70). Archives
and folders (`--tar`, `--erofs`, `--out`) are written whatever the limit. *Rests on:* a stale image
being slower, never wrong. *Breaks if:* a held bake is followed by no merge: `-latest` stays behind
the default branch (by fewer than `rebake_blocks` blocks) until the next bake that publishes, which
the daily scheduled bake bounds to a day; a job with no schedule waits for the next merge,
indefinitely over a quiet week.

**B-75 A bake builds what a skipped merge left unbuilt.** A merge whose checks CI skipped (the
pull request or an agent passed this exact code, B-29) compiles nothing on the default branch,
so the backend's newest build is of an older commit and a bake would publish the old store under
`-latest`. So a bake `--from backend` in CI on the default branch (`moves_latest`, B-66) first asks
`open` for each check it bakes, downloading nothing, and decides in one function
(`bake_build::behind`), check by check: a check is behind when the backend holds no build of it,
its newest manifest names no commit, or names a commit the one checked out (`git rev-parse HEAD`)
descends from (`git merge-base --is-ancestor`). A check that compiles nothing (a linter, a JS test,
a docs check) closes an empty manifest, which counts as its build of the commit it names like any
other. Only the checks that are behind are built, and the line names each with why. It builds
nothing when every check's newest is of the commit checked out, when one is of a commit
the commit checked out doesn't descend from (a re-run of an older bake or a retried old pipeline,
whose build would set the backend's newest manifest, and with no compare-and-set on a plain
registry the image, back), when the checkout's history doesn't hold that commit (a shallow clone;
the job `init --bake` writes checks out the whole history, B-69), or with no commit checked out;
each is said.

**What it builds is what CI's build job runs.** The checks it bakes (those `--check` names, else
every check hushrun.yaml names and every check CI's files run through `hushrun run NAME --`,
B-63) are run as CI's own files at the commit checked out say, read as `hushrun ready` reads them
(`setup::replay::replays`): for a push to the default branch, the first job on a Linux x64 runner
that runs `hushrun run NAME -- COMMAND` (else the first that does), COMMAND as the line writes it,
in its step's folder, with the variables CI's file gives it (the workflow's, the job's and the
step's `env` on GitHub; the file's and the job's `variables`, and the job's first `parallel:
matrix:` entry, on GitLab). A job whose `runs-on` is an expression that, for its primary entry,
doesn't name a Linux x64 runner is never replayed. A job's matrix stands for its **primary entry**: the first combination
GitHub makes (each key's values in the order written, the first key slowest, less `exclude`, with
what `include` adds) whose `runs-on` is a Linux x64 runner, else the first; `${{ matrix.KEY }}` and
`${{ env.NAME }}` take that entry's and the variables' values (`setup::replay::resolve`). CI's own
variables take the bake job's values, a job of the same repository on the same kind of runner
(`Values::runner`, its environment less what the build never gets, `WITHHELD`): on GitHub
`${{ github.workspace }}`, `${{ github.repository }}`, `${{ github.repository_owner }}`,
`${{ github.server_url }}`, `${{ github.sha }}`, `${{ github.ref }}`, `${{ github.ref_name }}`,
`${{ runner.temp }}`, `${{ runner.tool_cache }}`, `${{ runner.os }}` and `${{ runner.arch }}` from
the matching `GITHUB_*` and `RUNNER_*` variables, and `${{ env.NAME }}` that no level sets from the
job's `NAME`; on GitLab `$NAME` and `${NAME}` in a variable's value as GitLab's runner expands them,
from the variables set before it, else the job's own (its predefined `CI_*` ones), `$$` a `$`, a
variable that says `expand: false` taken as written. A variable whose value only CI knows (a
secret, another expression, a variable the bake job doesn't have, or a credential it withholds) is
left out, never passed as written, with one line naming it; a command that takes such a value isn't
built, said. CI's file is the one definition: init writes the command there and
nothing else (no `run` in hushrun.yaml, which would make the check required, C-10), and an edit to
it is what the next bake builds. A check hushrun.yaml gives a `run` that CI's files don't run is
built from it, at the repository's root.

Each check runs under the shell CI's job runs it with (`setup::replay::Shell`: on GitHub the step's
`shell:`, else the job's `defaults.run.shell`, else the workflow's, else GitHub's own `bash -e {0}`,
`bash` and `sh` being GitHub's templates for them, COMMAND written to the file `{0}` names, a shell
other than bash or sh not built, said; on GitLab bash where the job's image has it, else sh, as its
runner picks; hushrun.yaml's `run` as `hushrun run` runs it) in a session of the bake's own store
that shares through the bake's
relay as a CI job's run does (its job's own ID token, its commit, the check's id), so what the
trusted scope holds is reused, and what the merge changed is compiled and uploaded (B-29, B-31, as
any trusted CI job's blocks). Nothing is forced that CI doesn't set: no `CARGO_INCREMENTAL`, so a
crate CI compiles incrementally is passed through here too (B-4). Each check builds into **the
target folder cargo gives it in CI**, as `cargo metadata` says in its folder with its variables
(`CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `.cargo/config.toml`'s `build.target-dir`, else the
workspace's `target`; without a Cargo project, CI's variables, else `target` at the root or in its
folder), which the job's fresh checkout doesn't have, so what is keyed by the target folder's path
is keyed as CI keys it; checks CI builds into one folder build into one; where one is there
already, a fresh folder in the bake's store for it, said. Each is removed once the checks are
built. Every build of a bake (this one, the dependencies-only build, B-72, and `--report`'s, B-74)
runs through one function (`bake_build::in_bake` and `run_in_bake`): the command gets the bake's
environment less every credential
the bake holds (`bake_build::WITHHELD`: GitHub's ID token request token and URL, `GITHUB_TOKEN`,
`GH_TOKEN`, GitLab's `SIGSTORE_ID_TOKEN`, `HUSHRUN_ID_TOKEN`, job and registry tokens, hushrun's own
token, agent and signing keys), `HUSHRUN_BACKEND` empty and `HUSHRUN_INSIDE_RUN=1`, so a `hushrun
run` inside it (a Makefile's, a script's) looks up and records no result: the pull request's pass
is neither read nor taken as the job's, and the bake itself records none. The bake has the
backend's token before the build starts; the signature's ID token is asked for only after the last
check's command and every process it started are gone (ADR 0102): on Linux the bake is the build's
subreaper, and when a check's command ends whatever it left running is stopped and killed, tree
and all, and reaped.

A check's build **closes a manifest only when it is whole**: its command succeeded and every
block it built went up (`Ending::Bake`: its uploads are waited for as at every end, B-29, since
the bake's store goes with its job). One that failed, was stopped, left blocks unsent, or made a block `begin`
refused for good (`too_large`, `unsupported`, `no_scope`, `suspended`: never on the backend, though
its marker is done) closes nothing, said, a refusal naming each block and why: the backend's newest build stays the last whole one, and the next bake builds again, where a
manifest naming the commit with a part of its blocks would have been taken for the commit's build
by every later bake. All of it within 30 minutes (`BUILD_WITHIN`), said as it starts; past it the
running check is stopped with every process it started (its process group, and each process under
it, a group of its own too) and the bake goes on with what the backend holds then. Each check's
line says how many compiles were reused and built (`--format json`: `built_missing`, its `why` and
`lines`); with no check to build, it bakes what the backend holds and says so. `--no-build` skips
it. The bake's relay then downloads every block and hint the newest manifests name, whether this
store holds them or not (B-63: what is baked is what the backend serves). The checks of CI itself
still skip. *Rests on:* CI's file being what CI's build runs, and the job having its toolchain
(B-69). *Breaks if:* the backend doesn't send `manifestCommit`: every bake builds, which reuses
what the trusted scope holds and costs the build's time; the backend doesn't keep a check's empty
manifest with its commit (where it keeps none of that check with entries): every bake builds that
check; a check needs a service, a secret or a
step that only runs commands the bake job lacks: it fails, closes nothing, the bake says so and
bakes what the backend holds, and every bake tries again (each within its bound); or CI's build
runs on another runner image than the bake job (other folders): blocks bound to them miss, and
compile, in the bake's build.

**B-76 Blocks bound to CI's folders.** A block bound to the folders of the build that made it
(B-24: its keys hold the cargo home, the home folder, the checkout or a system folder's path, such
as a C `-sys` crate's, a build script's, and what depends on them) goes up only from a CI job's own
ID token (B-29), so the repository's trusted scope holds such blocks at CI's folders, which name
nobody. A bake **in CI, from the backend, `--trusted`** carries them as it carries every block the
manifests name, and records in `baked.json` the folders they are bound to, those the build ran
with (`paths`: `home`, `cargo_home`, `rustup_home`, `checkout`; `build::built_paths`: the first
check's variables as CI's file gives its build job, filled in as B-75 fills them, a relative folder
under the check's folder, over the bake job's own environment; `BakedPaths::of`: `HOME`,
`CARGO_HOME` else `$HOME/.cargo`, `RUSTUP_HOME` else `$HOME/.rustup`, and the checkout), which are
CI's runner's actual paths (GitHub's `ubuntu-latest`: `/home/runner`, `/home/runner/.cargo`, `/home/runner/.rustup`,
`/home/runner/work/<name>/<name>`), not a person's. No other bake records any: a store baked from
this machine's store never holds a block bound to its folders (B-63), and a person's paths never go
up. A sandbox reuses those blocks only at the same folders (their keys hold them; at others they
miss and compile, never wrong); a run whose own folders differ reads the store all the same and
says once which folders the bound blocks need (`baked::state`'s notes: `HOME=/home/runner (here
/root)`), and `init --bake` prints them (B-69). *Rests on:* CI's folders being the same in every
job of the runner image the bake job runs on. *Breaks if:* CI's build job runs on another image
than the bake job, or its checks set other folders from one another: the recorded folders are the
first check's, and other bound blocks miss there too.

**B-74 Measuring what a store saves.** `hushrun build bake --report -- COMMAND` times COMMAND on
this machine twice, each in a store and a target folder of its own beside the bake (cargo's
`CARGO_TARGET_DIR`, Go's `GOCACHE`), as every build of a bake runs (B-75's environment, through
`bake_build::in_bake` and `run_in_bake`), removed once measured: **cold**, with nothing to reuse (with
`--deps-only`, the dependencies-only build itself, which had nothing to reuse, unless it built Go
packages COMMAND doesn't), and **warm**, with the baked store just written underneath an empty
store, as a sandbox started from its image reads it (on its signature when hushrun.yaml names the
signer, else as accepted: the bake reads what it made, and nothing the build makes leaves its own
store). It says both wall-clock times, the ratio, and how many of the warm build's compiles came
from the store; with `--deps-only` both builds stub the repository's own crates, and it says that
this is the dependencies' share of a build; a warm build that read nothing says the store's state
(`--format json`: `report`). Nothing is estimated: a number it prints is a build it ran. `hushrun
init` runs before CI, so it can't measure: its plan names `--report`.

## Go

Go keeps its own build cache, keyed by its own **action id** (32 bytes: the hash of everything a
compile was asked, the sources by content, each import's content id, the flags, the version),
and from Go 1.24 lets a program serve it: the `go` command named by `GOCACHEPROG` starts that
program once per command and asks it to `get` and `put` every entry, over JSON lines on stdin
and stdout. hushrun's program keeps the person's own cache as it is and adds, beside it, only
the **package archives of pure Go** another machine may reuse: kind `go/1`, through the same
relay, store, records and outbox as the `rustc` wrapper's blocks (B-13, B-14, B-26 to B-34).
Go checks only the size of what a cache program hands it, and keys a dependent by the content
id an import's file claims: every check below is the only guard there is.

**B-39 The Go cache program.** `hushrun run NAME -- COMMAND` sets `GOCACHEPROG` for COMMAND to
a link named `hushrun-gocache` beside the wrapper's (B-2), pointing at the hushrun binary, when
all of these hold: the run has a session that shares (B-26), or it reads a baked store
(`HUSHRUN_BUILD_LOWER`, or `/hushrun` holding one, B-64, whose state, B-63, the run doesn't ignore:
decided first, with Go's platform from `go env`), whose Go packages are read through the program
and whose bake from this machine reads the archives B-42 keeps (B-63): a run that does neither,
or names a store its state ignores (`/hushrun` in CI with no signer named, an empty folder),
leaves Go as plain go runs it, as before 0.19; the folder COMMAND runs in, or
one above it, holds a `go.mod` or `go.work`; `go env -json GOVERSION GOCACHEPROG GOOS GOARCH`,
asked once in that folder within 10 seconds, names a version from `go1.24` to `go1.27` (any
patch, release candidates and betas) and no `GOCACHEPROG`, which is where Go reads one from
COMMAND's environment, the user's go env file and the toolchain's own `$GOROOT/go.env` (F12); and
the link's path can be given to Go: as it is when it holds no white space and doesn't start with
a quote, else wrapped in `'`, else in `"`, else not at all, with one line. A `GOCACHEPROG` the
person set is left as it is. An isolated check, Windows, a run outside a
Go module whose `RUSTC_WRAPPER` is set already (no session, B-2) and any other Go version get
nothing, and `go` runs as it would without hushrun. In a Go module's folder a `RUSTC_WRAPPER` set
already (sccache, say) leaves only hushrun's own wrapper out: the session still begins, the
person's wrapper passes through as it is, Go's program is decided by the rules above alone, and,
in an org that shares and where the folder is a cargo project's too, the run says once `build:
Rust crates aren't reused in this run: another compiler wrapper is set (RUSTC_WRAPPER)`. A `go`
command inside COMMAND whose version is not the one the probe saw (`GOTOOLCHAIN` switched inside
COMMAND) is B-47's. A binary called by that name is the program and does nothing
else; it decides that from its own name, before reading any setting. *Rests on:* Go 1.24 and
later starting the named program for every `go` command and sending it every get and put of its
build cache, splitting the variable as `quoted.Split` does; stock Go before 1.24 ignoring the
variable; the cache's format and protocol being the same from 1.24 to 1.27 (read in Go 1.27.4's
source, run on 1.25.14 and 1.27.4). *Breaks if:* a Go release changes the protocol or the cache's
format: hushrun names the versions it knows, so a newer Go gets no program until it is added.

**B-40 Answers.** The program writes `{"ID":0,"KnownCommands":["get","put","close"]}` before
reading anything, then answers every request exactly once: each request is `open` until one
compare-and-set makes it `answered`, and its handler's answer, a panic's (every handler runs under
`catch_unwind`) and a watchdog's for a request with no sign of life for 40 seconds (its reading,
then each heartbeat of the relay its handler waits on: ADR 0144) race for that one move, so a
second answer, which Go takes as fatal, is never written. A `get` that fails in any way is a
miss. A `put` answers a file holding exactly the body: in Go's cache, else (a write that failed,
a panic, the watchdog) in a folder of the program's own, removed at `close`; only a body whose
SHA-256 isn't its `OutputID` is answered with an error. The program's stdout carries protocol
lines only: no child it starts inherits it (it would hold Go's `close` open), and a panic prints
nothing on its stderr, which is the person's. `close` waits for the handlers still running while
one has shown life within the watchdog's time, answers, and the process exits. Every call to the
relay waits on its silence alone, 30 seconds (B-26's `relay::WAIT`), whatever the key's compile
cost: a relay working on an answer (a large download) says so every heartbeat and is waited for;
a notice (a block built, a report) is written and not waited for; connecting to the relay's
socket is bounded by the same wait. After one call fails, or meets a relay silent for the whole
wait, the relay is down for the rest of the run, not only of that `go` command: a file in the
session's folder (`go-relay-down`) tells every later `go` command of the run, which asks it
nothing, so a frozen relay costs a run one wait (F10). A panic while the program starts it starts again with no
session (B-47); one while it reads a request answers that request with an error, which Go takes
as a miss for a `get` and leaves its own build output in place of for a `put`, and reading goes
on: the program never exits before `close`. Side effects of serving a block (its use, a
download's first reuse, the log's line) happen only for the answer Go got: a `get` the watchdog
answered first records nothing.
*Rests on:* Go waiting for every answer without a limit, failing the command when the program
exits before `close` or sends an answer it doesn't expect, and building on a miss (read in
`cmd/go/internal/cache/prog.go`; proved by a test whose handlers hang and panic). *Breaks if:* Go
starts the program through a shell, or a Go release treats a miss on a hit's subkey differently,
or an error answer to a `get` as fatal.

**B-41 Go's own cache is the local side.** The program reads and writes `GOCACHE`, the folder the
`go` command exports to it, in Go's own format (`<2 hex>/<action>-a` holding `v1 <action>
<output> <size> <time>`, the body at `<2 hex>/<output>-d`). Every body Go puts is written there,
a temporary file renamed into place, and that file is what Go is handed. A `get` is answered from
there first, as Go's own disk cache would answer it (a well-formed line naming the id, a body of
the size it says), the time it holds included (so `go clean -testcache` still expires a result),
and both files' dates moved to now when an hour old (Go's `markUsed`). At `close` the program
trims the cache as Go's `DiskCache.Trim` does, since Go doesn't while a program serves it: at most
once a day by `trim.txt` (decimal Unix seconds, read again and written under a lock), removing
the `-a` and `-d` entries of the 256 folders unused for five days and an hour.
So plain `go` and `go` inside `hushrun run` share one cache, the first run inside `hushrun run`
is as warm as plain Go, `go clean -cache` clears what the program kept there, and Go's package
index, test results, compiler output, cgo's files and vet's facts, which are keyed by paths,
sizes and dates, stay exactly as Go keeps them and never leave the machine. A body served from
hushrun's store, a download among them, is never written into `GOCACHE`: its keys carry no
toolchain identity, and a withdrawn download would outlive its withdrawal there. *Rests on:* Go's
disk format and trim, the same from Go 1.10 to 1.27 (`cmd/go/internal/cache/cache.go`), and the
`go` command exporting its effective `GOCACHE` to the program. *Breaks if:* Go changes it within the
versions B-39 names.

**B-42 Portable (Go).** A package archive Go puts may become a block, and leave the machine,
only when every one of these holds, else it stays in Go's cache alone and the run's log records
why (`go.local`): the `go` command wasn't altered (`altered`:
`-toolexec`, `-overlay`, `-compiler`, `-gccgoflags`, `-linkshared`, `-buildmode=shared|plugin`,
or `-gcflags`/`-asmflags` naming a file the tool reads, `-I`, `-std`, or a value with `cfg`,
`profile`, `symabis` or `asmhdr` in it, matched as text in the parent process's arguments and in
`GOFLAGS`; arguments that can't be read, or a parent that is not the `go` binary of the `GOROOT`
the environment names, count as altered; read on Linux from `/proc`, on macOS with `lsof` and
`ps`); the toolchain's identity was computed (`toolchain`, B-43); the compiler's output, which Go
puts under `Subkey(action, "stdout")` just before the archive, was empty (`printed`, F4: Go
prints a compile's output again only from its own cache); and the bytes say so
(`archive::portable`): an `ar` archive whose first member is `__.PKGDEF` and every other member a
Go object, `_go_.o` or an object the assembler made for a standard library package (flags
`ObjFlagFromAssembly` and `ObjFlagStd`, [ADR 0111] or before Go 1.27 B-150's mark, read after the object's text header, whose end `\n!\n` is
looked for over the whole member: the runtime's header on macOS runs past 4096 bytes): a cgo object or a `.syso` is `native`, assembly of any
other package `assembly` (F1: its includes are outside Go's id); its build ID, read where Go's
`buildid.ReadFile` reads it, is `<action>/<content>` with `<action>` Go's `HashToString` of the
action id it is put under (`build-id`); and `<content>` is the SHA-256 of the bytes with every
occurrence of the build ID zeroed, as Go's `FindAndHash` computes it (`content-id`, F2). A
portable archive is kept in the store in every run the program serves, so that `hushrun build bake`
finds it (B-63), and goes up only from a run with a relay (B-29). An altered command is also served nothing from
hushrun's store. *Rests on:* Go's id covering every
input of a pure-Go compile but the toolchain's bytes and the alterations above (proved for two
users, module paths, GOPATH, GOMODCACHE and GOROOT, with and without `-trimpath`; and six proved
cases of one id for other bytes, all of them kept here: cgo against another header, `-toolexec`,
a patched toolchain, the package index, test results, cgo-generated sources; and assembly
including a header from outside its package). *Breaks if:* a Go release adds an input of a
pure-Go compile that its id doesn't hold: the audit (B-44) is the net.

**B-43 The key and the toolchain.** hushrun's key for a Go block is the SHA-256 of the JCS form
of `{"action": <Go's action id, hex>, "schema": "hushrun.go-key/v1", "toolchain": <identity,
hex>}`; its first key is itself, and it has no hints. The **identity** is the SHA-256 of the JCS
form of `hushrun.go-toolchain/v1`, `GOVERSION`, and the name (relative to `GOROOT`) and SHA-256
of: `bin/go`, every regular file in `GOTOOLDIR` (`compile`, `asm`, `cgo`, `cover`, `link`, `vet`
and the rest), every file under `pkg/include`, and every `.h` under `src`; `GOROOT`, `GOTOOLDIR`
and `GOVERSION` as the `go` command exports them to the program, so a toolchain `GOTOOLCHAIN`
switched to is the one identified. Each file's digest is remembered for the run (B-6's memo, by
path, size, both dates, inode and device), and so is the list of files found. *Rests on:* a pure
Go compile's output following from Go's id and those files. *Breaks if:* the compiler reads a
file of the toolchain outside them (the standard library's Go sources are in Go's id already).

**B-44 Serving a block (Go).** On a `get` that Go's cache doesn't answer, and only for a command
that wasn't altered and a known toolchain: a block under the key that the store holds, whose key
isn't suspended (B-15), whose description says `go/1` and the action asked, is served only when
`Shared::may_reuse` says so (B-34, the one definition the `rustc` wrapper decides by too), its
file's SHA-256 and size are its record's (else it is removed and compiled), and its bytes pass
B-42's byte checks (else it is compiled; when it was only downloaded it is first reported
`rejected` to the scope its record names, then its key is suspended for `keep_days` as the
`rustc` wrapper's refused downloads are, then it is removed).
Then one time in fifty, one in ten on a downloaded block's first reuse here (`audit::sample`),
every time in local `observe` and with `HUSHRUN_BUILD_AUDIT=every`, never with `none`, and never
twice for one key in a run (a file per key in the session's folder, made with `create_new`), it is
answered as a miss on purpose: what Go then compiles is compared with it, and a difference
suspends the key for `keep_days`, reports `differs` when the block was only downloaded, and puts
the compile in its place. An id Go's own cache holds is never audited by miss: it is compared
for free, when the toolchain is known already, with a block under the same key, and a
difference is only counted (`N in Go's own cache with other bytes than hushrun's blocks (not
reported)`): Go's cache is not evidence, as a `-toolexec` build or a patched toolchain may have
put other bytes under the same id there, so only a miss on purpose suspends or reports. A block served is handed to Go as a file named by its SHA-256 in
the session's folder, a link to the store's file it was read from (the same inode) or a copy of
the bytes checked, never rewritten or removed while the run lives, whatever later replaces the
block (F8). A compile whose block is in the store already is compared with it; one only
downloaded gives way to it. *Rests on:* Go compiling after a miss and putting under the same id,
and pure-Go archives being deterministic (both proved). *Breaks if:* Go stopped putting after a
miss: an audit would then compare nothing, which the run's line shows as no comparison.

**B-45 Sharing Go blocks.** Go blocks are kind `go/1`, class `library`, through the run's relay
(B-26 to B-34) as the `rustc` wrapper's are: an outbox marker for a block built here (B-29, its
`kind` recorded), `begin` and `commit` of one kind at a time, records of where a block came from,
vouches, reports and withdrawals. A check whose COMMAND gets the program asks `open` a second
time, kind `go/1`, for its manifest alone (the policy is the first `open`'s), at the same time
as the first, with the platform `GOOS_GOARCH` that `go env` gave; and `close`s a second time,
kind `go/1`, under a run of its own (the SHA-256 of the check's run and `go/1`), with the Go
command's archives that may leave the machine in the order the program logged them, each pair's
first address its block address, Go's own cache's hits included; an archive kept here (B-42) is
counted and not listed. A Go fetch is looked up only when that manifest names it: Go asks
about every entry its cache misses, most of them local ones no server holds. The first Go fetch
of the check starts the prefetch of what the manifest names that the store lacks (a machine
whose `go` finds everything in its own cache asks nothing and downloads nothing); a fetch that
comes while that lookup runs waits for it to end; [ADR 0111] so does one that comes while the
check's `open` or its `go/1` `open` is still under way (Go asks for its first packages as COMMAND
starts). ADR 0144: no bound on either wait, so that a package the manifest names is reused
whatever the machine's load. The manifest is taken and its prefetch marked under one lock: a
fetch that came between the two would find neither, and miss. *Rests on:*
the server keeping manifests per repository, check, platform and kind, and taking any kind that
`is_kind` reads (no server change). *Breaks if:* a check's manifest names other packages than
its next run asks for: they compile here, a cost and never a wrong block.

**B-46 What a run says (Go).** The Go cache program's lines go into the run's log beside the
wrapper's, and the summary counts them apart: `build: go: N packages: X from Go's own cache, Y
reused (Z from the shared store), W built`, with the time saved and how many differed, and one
line for the compiled archives kept on this machine, by reason (B-42), and, in `(…)`, how many
reused came from the baked store (B-64). Go's own output is unchanged. *Rests on:* nothing.

**B-47 Without a session.** A program that finds no usable session (a `go` command that outlived
its run, `GOCACHEPROG` set by hand to the link) serves Go's own cache only, as Go's disk cache
would, and keeps nothing else. One whose Go version is not the probe's and not one B-39 names
(`GOTOOLCHAIN` switched inside COMMAND) still serves and keeps Go's own cache, whose format is
the same from Go 1.10, and leaves hushrun's store out: nothing is served from it or stored in it.
One with a version B-39 names serves the store under that toolchain's own identity (B-43). One
with no usable `GOCACHE` answers every `get` as a miss, keeps every body in its own folder until
`close`, and fails nothing. *Rests on:* nothing.

**B-150 The standard library's assembly before Go 1.27.** [ADR 0111] An object the assembler made
(`ObjFlagFromAssembly`) is a standard library package's, for B-42, when its own header says
`ObjFlagStd`, or when the archive's `_go_.o`, the compiler's object of the same package, says
`ObjFlagStd` and not `ObjFlagFromAssembly`. The compiler marks its object so under `-std`, which
the `go` command gives the compile of a standard package, and of no other, in every version B-39
names; `-std` in `-gcflags` is B-42's `altered`. Go's assembler takes `-std`, and marks its own
objects, only from Go 1.27: with Go 1.24 to 1.26 `runtime`, `internal/bytealg` and every other
standard package with assembly were `assembly` and never left the machine (8 of the 24 packages
of a small program on amd64). *Rests on:* `cmd/go` adding `-std` to a compile's flags exactly when the package is
standard (`gc.go`, `p.Standard`), and `cmd/compile` setting `ObjFlagStd` from it (read in Go
1.24.7's and 1.27.4's source; the two-machine tests pass on 1.24.7, 1.25.0, 1.26.0 and 1.27.4).
*Breaks if:* a Go release gives `-std` to a package outside the standard library: B-43's identity
hashes only the toolchain's headers, and such a package's assembly could include others.

## C and C++ (draft)

> Draft, from [ADR 0097](../adr/0097-c-and-c++-build-blocks.md) (proposed). Phases 1 to 3 are
> built: local reuse between worktrees, the run memo, and sharing through the relay (B-59, B-60).
> A rule a phase doesn't reach yet passes the compile through.

CMake starts each compile through a **launcher** named by `CMAKE_C_COMPILER_LAUNCHER` and
`CMAKE_CXX_COMPILER_LAUNCHER`: `<launcher> <compiler> <arguments>`, one process per translation
unit. hushrun's launcher reuses an **object**, what one compile of one source wrote, with its
depfile and its messages, as a block of kind `cc/1` in the same store, through the same relay,
records and outbox as the `rustc` wrapper's (B-13, B-14, B-26 to B-34). Ninja, Make and the linker
check nothing about an object but its name and format: every check below is the only guard there
is. A **root** here is the repository's folder as `hushrun run` found it (the nearest folder at
or above the one it runs in that holds `.git`, as the shell named it in `PWD`), in its given and
resolved forms; the compile's folder is named from it. **Bases** are the SDK (`{sdk}`, the folder
`xcrun --show-sdk-path` names for the call, resolved), the toolchain (`{toolchain}`, the folder
above the compiler's `bin`), and the folders the driver searches for system headers.

**B-50 The launcher.** When the mode is not `off`, `hushrun run` sets, in both its forms and
wherever B-2 sets `RUSTC_WRAPPER` (with the session B-2 describes),
`CMAKE_C_COMPILER_LAUNCHER` and `CMAKE_CXX_COMPILER_LAUNCHER` for COMMAND, each only when
COMMAND's environment has no value for it, to the CMake list
`/bin/sh;-c;[ -x "$HUSHRUN_CC" ] || HUSHRUN_CC= && exec ${HUSHRUN_CC:+"$HUSHRUN_CC"} "$0" "$@"`
(no `;` in the script, where CMake would split the list), and `HUSHRUN_CC` to a link named
`hushrun-cc` beside the wrapper's (B-2), pointing at the hushrun binary. Unlike `RUSTC_WRAPPER`,
a `RUSTC_WRAPPER` the person set doesn't stop it: the session begins as for Go (B-39), in a folder
at or under a `CMakeLists.txt` of its checkout, or a CMake build folder. It sets
nothing for an isolated check, on Windows, or for a language whose launcher the person set. CMake reads the variable when
it configures a build folder whose cache holds no launcher for that language, the first configure
and any later one, CMake's own re-run included, and caches the text: that text names no file of
hushrun's and no session, so outside a run, where `HUSHRUN_CC` is unset, or names a link that is
gone (hushrun removed, the run's folder deleted), the shell replaces itself with the compiler, with the same arguments, file descriptors, environment and exit status, and
Ninja and Make see the same command in and out of a run and rebuild nothing for the move. A
launcher already in the cache (ccache, sccache) is CMake's to use, and hushrun's never runs in
that folder. A binary called `hushrun-cc` is the launcher and does nothing else; it decides that
from its own name, before reading any setting. `hushrun run NAME -- COMMAND` says once, in an org that shares or
with the mode `on`, `build: C and C++ objects aren't reused in <folder>: it was configured before
hushrun's launcher; configure it again inside hushrun run` for a build folder COMMAND's words
name (`cmake --build <dir>`, `cmake -B <dir>`, `ninja -C <dir>`, `make -C <dir>`) whose
`CMakeCache.txt`, read after COMMAND, names neither launcher and whose generator is Ninja or
Makefiles; when that cache was written by CMake older than 3.17, which reads no launcher from the
environment, it says `build: C and C++ objects aren't reused in <folder>: CMake <version> reads no
launcher from the environment (3.17 and later do)` instead. For such a folder whose cache names a
launcher and whose generated rules (`CMakeFiles/rules.ninja`, `build.make`) run something before
the indirection (a project's `RULE_LAUNCH_COMPILE`, which CMake puts before
`<LANG>_COMPILER_LAUNCHER`), it says `build: <folder> starts a launcher of its own before
hushrun's (RULE_LAUNCH_COMPILE), which sees /bin/sh as the compiler: …`: a ccache there takes
`/bin/sh` for the compiler and compiles every time, in and out of a run, while hushrun's launcher
still runs behind it.
*Rests on:* CMake 3.17 and later reading the variables from the environment when the cache has
none, at a later configure too (run on 3.25.1 and 4.4.5) and leaving launchers out of `compile_commands.json` (run on
4.4.5); POSIX `sh` expanding `${HUSHRUN_CC:+"$HUSHRUN_CC"}` to nothing when the variable is unset
or empty. *Breaks if:* a CMake release stops reading the variables on a later configure (a
folder configured before the run then never gets the launcher, which the run's line says), or a
build runs on a system with no `/bin/sh` (Windows, where nothing is set).

**B-51 Passed through.** The launcher runs the compiler exactly as asked, storing nothing, unless
the call is one compile of one source to one object: `-c`; one source of C (`.c`) or C++ (`.cc`,
`.cpp`, `.cxx`, `.c++`, `.C`), or one after `-x c` or `-x c++`; one `-o`; and depfile flags only
among `-MD`, `-MMD`, `-MF`, `-MT`, `-MQ`, `-MP`. It passes through, with the reason in the run's
log: no session (B-62); a first argument whose file name, or whose link's target's, is `ccache`,
`sccache`, `distcc`, `icecc` or `buildcache`; `clang-cl` or `cl`; no `-c`, or `-E`, `-S`, `-M`,
`-MM`, `-MG`; several sources; `@file`; `-x` naming any other language, and sources of
Objective-C, Objective-C++, assembly or CUDA; a precompiled header made or used (`-x c-header`,
`-x c++-header`, `-include-pch`, `-emit-pch`, an `-include` of a file beside which a `.gch` or
`.pch` exists, a read file whose name ends in either); modules (`-fmodules`, any `-fmodule-` flag,
`-fcxx-modules`, `-fmodule-file=`, a `.cppm`, `.ixx` or `.mpp` source, a `@` map); profiles and
coverage (any `-fprofile-` flag, `-fauto-profile`, `--coverage`, `-ftest-coverage`,
`-fprofile-arcs`, `-fcoverage-mapping`, `-fprofile-instr-`); a second output (`-gsplit-dwarf` and
`-gsplit-dwarf=`, `-save-temps`, `-ftime-trace`, `-fstack-usage`, `-fcallgraph-info` and
`-fcallgraph-info=`, any `-fdump-` flag, `-MJ`, `-fcrash-diagnostics-dir`, `-fopt-info` naming a
file, GCC's `-fdiagnostics-format=` ending in `-file`, `-fdiagnostics-add-output=`,
`-fdiagnostics-set-output=`, and an assembler listing or depfile: `-Wa,` or `-Xassembler` with
`-a…=file` or `--MD`); flags that load code or read files the
depfile need not list (`-fplugin`, `-fpass-plugin`, `-Xclang`, `-mllvm`, `--config`, `-B`,
`-specs`, `-wrapper`, `-ivfsoverlay`); Xcode's second outputs (`--serialize-diagnostics`,
`-index-store-path`), `-aux-info`, `-fsave-optimization-record`, `-dependency-file`; include
prefixes (`-iprefix`, `-iwithprefix`, `-iwithprefixbefore`, `-iwithsysroot`, `-I-`, a folder
written `=…`); a source on stdin, two `-o`, an `-o -`, a positional that is no source (an object,
a preprocessed, Fortran or LLVM IR input, a word the parser can't place); `-Wp,` with an `-M`
flag; GCC's `-flto` without `-frandom-seed`; `DEPENDENCIES_OUTPUT`,
`SUNPRO_DEPENDENCIES` or any `CC_PRINT_` or `CC_LOG_` variable in the environment; the compiler's
identity can't be established (B-52). A compile in CMake's own configure folders
(`CMakeFiles/CMakeScratch/`, `CMakeFiles/CMakeTmp`) runs as asked and is counted nowhere
(`cmake-probe`). Every other flag is keyed by its text (B-53). *Rests on:*
the named flags being those that make a compile write a second file or read one its depfile need
not list. *Breaks if:* a flag outside the list reads a file the depfile doesn't list: two such
compiles share a key, which only the audit (B-59) finds.

**B-52 The compiler.** The compiler's identity is the SHA-256 of the JCS form of
`hushrun.cc-compiler/v1` and: the SHA-256 of the binary the call reaches, where `/usr/bin/cc`,
`c++`, `clang`, `clang++`, `gcc` and `g++` on macOS are `xcrun`'s trampolines and the binary is
the one `xcrun -f clang` (or `clang++`) names under the call's `DEVELOPER_DIR`, `SDKROOT` and
`TOOLCHAINS`; for GCC, the SHA-256 of each program `-print-prog-name=` names for `cc1`, `cc1plus`
and `as`, found on the call's `PATH`, and of each plugin the next item names; what the driver
prints for `-### -c -x <language> /dev/null -o /dev/null` with the call's target flags (`-target`,
`--target=`, `-arch`, `-m32`, `-m64`, `-isysroot`, `--sysroot`, `-march`, `-mcpu`), with roots and
bases replaced: the target, the resource folder, the sysroot, the implicit flags and the
configuration files it read; the SHA-256 of each file the driver says it read (clang's
`Configuration file:`, GCC's `Reading specs from`), and for clang of each shared library of LLVM
or clang (`libLLVM*`, `libclang-cpp*`) in the folder beside its `bin`, which a distribution's
update changes under an unchanged binary; and on macOS the SHA-256 of the SDK's
`SDKSettings.json`. The probe
also takes the call's `-stdlib=`, `-nostdinc`, `-nostdinc++`, `-nostdlibinc`, `-nobuiltininc`,
`--gcc-toolchain=` and `--gcc-install-dir=`, and the system folders of the search are what the
driver's `-E -v` lists with the same flags. It is kept in a memo named by the compiler's path,
size, dates and inode, the language, the flags of the probe, the values of `DEVELOPER_DIR`,
`SDKROOT`, `TOOLCHAINS`, `PATH`, `CLANG_NO_DEFAULT_CONFIG`, `CCC_OVERRIDE_OPTIONS`,
`GCC_EXEC_PREFIX` and `COMPILER_PATH`, and on macOS the folder `xcode-select` chose
(`/var/db/xcode_select_link`), which a trampoline's stat doesn't follow; the memo answers only
while each program, file and library it names has the stat it holds, and so do clang's binary's
folder (a configuration file made beside it) and GCC's install folder (`-print-search-dirs`'s
`install:`, where a specs file made is read). The SDK and each system folder are kept as the
driver names them, which is how a compile spells what it finds there, and resolved, both `{sdk}`
for the SDK (a Mac whose SDK is the link `MacOSX.sdk`). One process asks the driver under a
lock beside the memo; the others of a cold build read what it wrote. The target triple the driver prints is the
block's platform (B-60). Headers under the bases are not in the identity: they are read files,
hashed like any other (B-54). *Rests on:* a compile's output following from the binaries named,
the flags the driver adds and the files the compile reads. *Breaks if:* a compiler loads code
from a file the identity doesn't hash (a plugin `-###` doesn't name): such compiles are passed
through when the flag that loads it is seen (B-51), and audited otherwise.

**B-53 What was asked (C).** The first key is the SHA-256 of the JCS form of
`hushrun.cc-key/v1`; the compiler's identity; the language; the compile's folder as a path from a
root, or as it is when under none (which ties the block, B-56); the arguments in order, with roots
and bases replaced (B-8's replacement, reversible up to a root's two forms) and without `-o`, `-MF`,
`-MT`, `-MQ` and their values; whether a depfile was asked and which (`-MD` or `-MMD`, and `-MP`);
for `-march=native`, `-mcpu=native` and `-mtune=native`, what the driver resolves them to (`-###`'s
`-target-cpu` and `-target-feature` for clang, `cc1`'s expanded `-march=`, `-mtune=` and feature
flags for GCC) in place of their text, asked once per identity and flags in a run and memoised
in the run's session folder, never in the store, which a network share can give machines with
other processors; the variables a
compile reads without an argument saying so: `CPATH`, `C_INCLUDE_PATH`, `CPLUS_INCLUDE_PATH`,
`OBJC_INCLUDE_PATH`, `SDKROOT`, `DEVELOPER_DIR`, `MACOSX_DEPLOYMENT_TARGET`,
`IPHONEOS_DEPLOYMENT_TARGET`, `TVOS_DEPLOYMENT_TARGET`, `WATCHOS_DEPLOYMENT_TARGET`,
`XROS_DEPLOYMENT_TARGET`, `SOURCE_DATE_EPOCH`, `CCC_OVERRIDE_OPTIONS`, `GCC_EXEC_PREFIX`,
`COMPILER_PATH`, `GCC_COMPARE_DEBUG`, and for the messages `LANG`, `LC_ALL`, `LC_MESSAGES`,
`LC_CTYPE`, `GCC_COLORS`, `GCC_URLS`, `TERM_URLS`, `NO_COLOR`, each with roots and bases
replaced, or unset; the person's extra variables (B-21 `env`) and the repository's `build: env:`
(B-35); and what the launcher adds (B-55), with roots replaced. It holds no machine, user, branch,
commit or time. *Rests on:* GCC's and clang's documented variables being every one they read for a
compile. *Breaks if:* a compiler reads another variable that changes its output: the audit finds
it, as ADR 0095 found Cargo's.

**B-54 What was read (C).** The second key is the SHA-256 of the JCS form of the first key and:

- `reads`: each file of the compile's full dependency list (B-55) but the output, as a path from
  its root or base with its SHA-256, in the list's order. A read outside every root, base and
  system folder (`/usr/include`, `/usr/local/include`, `/opt/homebrew/include` and the folders
  the driver searches) is not stored (`outside-roots`);
- `places`: for each read file found in a folder of the search, each path where it would have
  been found first had it existed, every earlier folder of the search joined with the rest of its
  path, recorded as absent. The search is in the compiler's order: the including file's folder
  (for a forced include given relative, the compile's folder first), `-iquote`, `-I` and `-F`,
  `CPATH`'s, `-isystem` and `-iframework`, `C_INCLUDE_PATH`'s (or `CPLUS_INCLUDE_PATH`'s), the
  driver's system folders, then `-idirafter`; a folder also given as a system one is searched only
  at its system place, as GCC and clang drop its `-I`, and two forms of one folder are one; a
  framework folder's places are its `<name>.framework/Headers/` and `PrivateHeaders/` forms, the
  first before the second. The including file is the one clang's include graph names (B-55); for
  GCC, clang before 15, and every file a system header includes (which the graph leaves out) the
  source's folder stands for every including file's;
- `probes`: each literal name a read file passes to `__has_include` or `__has_include_next`, in
  every folder of the search, recorded as present or absent. A probe whose argument is not a
  string literal or `<…>` is not stored (`computed-probe`).

A place that exists when the compile is stored and isn't a read is left out when it is older than
the compile (the over-count above named a folder the compiler didn't search for that header), and
is not stored (`changed`) when it is dated at or after the compile's start: a header a parallel
build step made where the compiler had already looked. A probe there and dated since, or absent
from a folder changed since (other than the folders the compile writes into), is `changed` too.
A read file holding an assembler directive that reads a file (`.incbin`, `.include "…"`), which no
depfile names, is not stored (`incbin`), nor one calling `__has_embed`, whose answer depends on
whether a file is there and empty (`has-embed`). A `..` after a symbolic link in a read's path is
left to the kernel, which takes it from the link's target.

A read file holding `__TIME__`, `__DATE__` or `__TIMESTAMP__` as a word anywhere (comments
included), looked for in the read that hashes it, or arguments holding one (`-DBUILD=__DATE__`),
is not stored (`time-macro`), unless `SOURCE_DATE_EPOCH` is set (keyed, B-53) and neither holds
`__TIMESTAMP__`. A read whose name ends in `.gch`, `.pch` or `.pcm` is not stored
(`pch`). The definition `#define __has_include(…) …`, an SDK's fallback for a compiler without the
operator, is not a probe. The lists are a hint (B-7) under the first key, written as a table of
folders and the names in each, folders absent the same names in one entry; a lookup hashes the files
as they are now and checks each place and probe, a folder at a time (a missing folder holds none of
its places, and a folder asked about more than three names is listed once, a name that matches
one there ignoring case counting as present), and a block exists under a key only because a
compile read exactly those bytes and saw exactly those places. Places are over-counted, never
under: every folder of the search a read file's path lies in is taken as the one that may have
held it, and every including file clang names counts for quoted and angled includes alike. The
digests of files under the SDK, the toolchain and the system folders come from the run memo
(B-52's bases): one entry per folder in the session's folder, each file's stat, SHA-256 and
probes, answering only while its size, both dates, inode and device hold; files under a root
are hashed every time. B-11 holds for every read, place
and probe. *Rests on:* a compile's output following from the files it read and the places its
search looked, and a `__has_include` being decided by the folders of the search (measured: a
header created where the search looks first, and a file `__has_include` asked about appearing,
each gave a new key and a compile, where ccache 4.14.3's direct mode served the old object).
*Breaks if:* the including file isn't the source and a file appears beside it under a name a later
folder answered: a header added in `include/foo/` shadowing the `gen/config.h` that a quoted
include in `include/foo/a.h` found gets a stale reuse that only the audit finds. That is every
compile by GCC or a clang before 15 (every Linux distribution's GCC build), and with any clang,
every file a system header includes (owner decision 5; the unit test
`without_a_graph_the_source_s_folder_stands_for_every_including_file_s` pins it). Also if a compile
reads a file through something other than an include, an embed, a probe or an assembler directive
it looks for (`#embed` files are in the depfile).

**B-55 What the launcher changes in a compile.** For a compile it doesn't pass through it adds
`-ffile-prefix-map=<root>=.` for each root, in its given and resolved forms, ordered by length so
that the inner one wins (owner decision: off with `build: cc: maps: off`, then nothing is added
and the block is tied to its paths, B-56). It always takes the compiler's full dependency list in
a file of its own (`-MD -MF <the session's folder>/…`, with `-MT` set to the output), replacing
the call's `-MD`, `-MMD`, `-MF`, `-MT`, `-MQ` and `-MP`, and writes the person's depfile itself:
after a compile, the compiler's own text, its file having been asked with the call's `-MT`, `-MQ`
and `-MP`, less for `-MMD` the reads under the driver's own system folders and under no folder a
user include could have found them in (the source's, `-iquote`, `-I`, `-F`, `CPATH`'s): headers
of `-isystem`, `-idirafter` and `C_INCLUDE_PATH` folders stay, since which folder found a header
only the compiler knows, and a depfile naming one file more costs a stat where one naming one
fewer misses a rebuild; after a reuse, as B-57 says, from the same rule. A depfile that can't be
written runs the compiler again as asked, keeping its stderr, and passes on only what it says
beyond the first compile's lines. For clang it sets `CC_PRINT_HEADERS_FORMAT=json`, `CC_PRINT_HEADERS_FILTERING=direct-per-file`
and `CC_PRINT_HEADERS_FILE=<the session's folder>/…` in the compile's environment, which makes
clang write its include graph there and nothing to stderr; not for a compile with `-H` or
`--trace-includes`, whose include tree clang would then write to stderr as the graph's JSON:
that compile has no graph, as GCC's hasn't. A compile of the launcher's own pipes the compiler's
stderr, so a compiler that colours its messages only on a terminal writes none, under Make on a
terminal as under Ninja (which always pipes); `-fdiagnostics-color=always`, keyed, keeps colour. A compile it runs as asked (B-51,
B-58) gets none of this. A clang that writes no graph there (Debian's clang 14 writes none) is
treated as GCC is (B-54). *Rests on:* clang writing the graph to that file (run on Apple clang
21) and GCC's and clang's `-MD` listing every file a compile reads but probes' folders.
*Breaks if:* a clang writes a graph in another form there: it is not read, and the source's
folder stands in.

**B-56 Place and portable (C).** After a compile, the object is searched as raw bytes for each
root in its given and resolved forms, as UTF-8 and as UTF-16 and UTF-32 in each byte order (a root
in `L"…"`, `u"…"` or `U"…"`), for the home folder and for each base's value. None found:
`anywhere`. A block whose `-D` values hold a root is tied whatever the object holds: the code may
turn the value into bytes no search finds. A root found: the block is tied to its roots (its key holds their text) and is reused
only where they are the same. The home folder or a base found: B-24's `…-in-output` reasons, kept
on the machine. A tied block, one passed `-march=native` or its kin (`native-cpu`), one with a
read outside roots and bases, and one whose compile's folder is under no root are never portable.
A portable block may leave the machine (B-25); a block bound only to folders may from CI, as B-24
says. *Rests on:* a path in an object appearing as its bytes (no compression of debug sections:
a compile with `-gz` (other than `-gz=none`) or `--compress-debug-sections` gives a tied block,
its sections not searched).

**B-57 Reuse (C).** The object is cloned (copied where the file system can't clone) under a
temporary name in its folder, one name per output so that the next write replaces what a killed
launcher left, given the mode a file the compiler made would have (`0666` less the umask),
renamed into place and dated by the rename. Then the person's
depfile is written whole, a temporary file renamed into place, from the reads that made the key,
with this checkout's roots and bases, in the syntax the compiler writes: at the path `-MF` named, else where the compiler would have
written it (the output with its suffix replaced by `.d`); the target the call's `-MT` or `-MQ`
named (`-MQ` quoted as the compiler quotes it), else the output; paths as the compiler writes them (relative when under
the compile's folder and given relative, spaces and `#` escaped); for `-MMD` without the files in
system folders; for `-MP` a phony target per header. A downloaded block's depfile, if any, is never
read. Then the recorded stdout and stderr are written with this checkout's roots. If any step
fails before the first message is written, the call compiles as asked (B-58) over what was
written. *Rests on:* Ninja and Make deciding by dates and depfiles: an object dated after its
inputs, with a depfile naming this checkout's files, is up to date (measured: `ninja -n` has no
work after a warm build), and CTest's stale-program rule (ADR 0090, Amendment 8) compares that date
with the sources' and a program linked after it.

**B-58 Every failure runs the compiler.** Until the launcher writes a byte to stdout or stderr or
starts a compile of its own, any failure (an argument it can't read, a store it can't open, a
memo, a hint, a panic) replaces it with the compiler and the arguments as given (`exec`), so
stdout, stderr, the exit status and the signal are the compiler's own. A compile of its own
passes the compiler's stderr on line by line as it comes and its stdout whole, returns its
status, and re-raises the signal that ended it. A restore that fails part way runs the compiler
as asked as a child, with the same passing on. A store that fails after a successful compile is
`built, not stored`. A kill at any point leaves nothing a later build takes for a block.

**B-59 Checks of a C block.** A block is served only when its first key isn't suspended (B-15),
`Shared::may_reuse` says so (B-34), and its object's size and SHA-256 are its record's; a block
only downloaded also only when the compile's target is one whose objects the launcher can check
and its bytes are an object of the compile's kind: ELF `ET_REL` or Mach-O `MH_OBJECT` of the
target's machine, width and byte order, or for clang's `-flto` LLVM bitcode (raw or wrapped).
A block whose bytes fail is removed and the call compiles; one only downloaded is first reported
`rejected` to the scope its record names, and its first key suspended for `keep_days`, so the
compile that follows stores nothing under it. A compile here whose object isn't of the compile's
kind (clang's bitcode without `-flto`, a target the launcher can't check) is stored with the
reason `not-an-object` and stays on the machine (B-56). Then one time in fifty, one in ten on a
downloaded block's first reuse here, which its record notes (`audit::sample`), every time in local
`observe` and with `HUSHRUN_BUILD_AUDIT=every`, never with `none`, the launcher compiles anyway and
compares the object byte for byte, whatever the two keys (a compile tied by this checkout's root
has another key than an honest block it matches): a difference serves the compile and suspends
the first key,
until `hushrun build clean` for a block built here; for one only downloaded, for `keep_days`, with
a report `differs` to the scope it came from, and the compile takes its place (B-32). A compile
that finds a downloaded block of its own key already in the store compares it the same way, and
where the two agree the block is from then on also built here when every file of it is this
compile's; where only the object is, this compile's messages take the downloaded ones' place,
so that no downloaded message is ever served unfiltered. A suspension for `keep_days` never
shortens a longer one already there (one until `hushrun build clean`, or one ending later): the
check and the write are under one lock of the store's. The depfile is written from the
hint's reads, the files this machine hashed for the key, never from anything downloaded.
A block's messages are stored with roots as placeholders and each temporary file the driver
names (GNU as's `/tmp/ccwUJrhb.s: Assembler messages:`, `cc` and six random letters and digits,
in `/tmp` and its kin, macOS's per-user temporary folders or `TMPDIR`) as `{temp}`, which a reuse shows as it is, so one compile's blocks are the same outputs on every
machine. Messages from a downloaded block keep newlines, tabs, carriage returns and SGR colour
sequences (`ESC [ <digits and ;> m`) and lose every other control character before they are
written, the C1 controls (U+0080 to U+009F, as UTF-8 or as a lone byte) among them.
*Rests on:* a compile being deterministic at one path and, with B-55's maps, at two (measured on
abseil: every unit at a second path gave the stored bytes); the compiler's identity holding no
name the driver makes up each time it is asked (GCC's `-###` temporary files, written `{temp}`).
*Breaks if:* a compiler writes a time or random bytes into an object: every audit of it differs,
which loses reuse and never serves wrong bytes.

**B-60 Sharing C blocks.** C blocks are kind `cc/1`, class `library`, through the run's relay
(B-26 to B-34) as Go's are (B-45): records, vouches, reports and withdrawals, and hints sealed
with the first key as `rustc` blocks' (B-30). The launcher follows the hints compiles here wrote
first, then hints downloaded earlier (in a run that shares or would, B-34, and never for a first
key kept on this machine), then a baked store's (B-64), and asks the relay for a first key's
hints only when it has none, in
local mode `on`, for a target triple `is_platform` reads and B-59 can check; a downloaded hint is
followed only within 20,000 reads, 500,000 places and 100,000 probes, and only when every read,
place and probe it names lies under this call's roots, bases or system folders once made
absolute. A second key the hints lead
to that the store lacks is fetched as a `rustc` wrapper's is (B-26), at most eight keys a fetch,
and only a key the manifest pairs with the first key: any other asks the server nothing.
The relay asks `open` a further time, kind `cc/1`, with the platform the compile's target triple,
when the first launcher compiling for that triple asks (at most eight triples a check), and keeps
the answer only while the check that asked is still the relay's, and
answers a launcher's hints and fetch only for a first key that manifest names: any other is a
miss, without the network. A question waits for the check's `open` and that manifest until they
answer or fail (ADR 0144: no bound). The first question about a first key the manifest names
starts the prefetch of what it names that the store lacks (their hints, then their blocks) and
waits for its lookups. A question to the relay waits on its silence alone, 30 seconds (B-26),
whatever the compile last took; one that meets a relay silent that long, or that hung up, marks
the relay down in the session's folder
(a mark the Go cache program's also stands for), and no launcher of the run asks it anything
after. A block stored with no reason to stay (B-56) is told to the relay, which gives it an outbox
marker (B-29) when the policy lets the run write and its record says this run's host, org and
credential built it; one whose only reason is `tied` goes up from a CI job's own ID token alone,
as B-24's bound blocks do; its hints go up after its commit. A block's pack carries neither the
uploader's `rustc_ms` nor its `compile_ms`: a download's compile time is the one the server
states. At the check's end the relay reads the run's log for objects stored with no reason to
stay (or `tied` alone) that no launcher told it of (a launcher that marked the relay down tells
it nothing after), and gives each the marker its launcher would have. A check closes once per target triple
its objects were compiled for, under a run made from the check's and the triple, with a manifest
of its blocks in the order the launcher logged them, a reuse's as a compile's (each line holds
its first key and its triple), leaving out a block that stays on the machine (a reuse's reasons
from the block's record) and CMake's configure checks; a CI job's close, whose credential is its
own ID token, names its `tied` blocks too, since a later job at the same paths fetches only a key
a manifest pairs. *Rests on:* the server
taking any kind `is_kind` reads and keeping manifests per kind (no server change).

**B-61 What a run says (C).** The launcher's lines go into the run's log beside the wrapper's,
each with how long the call took, and the summary counts them apart: `build: c/c++: N objects: X
reused (Y from the shared store, V from the baked store, B-64), W built`, with the time saved and how many compared with their
blocks differed (in `observe`, `build: c/c++: observing: X of N objects would have been reused`),
and one line, `build: c/c++: N objects compiled as asked or not stored (…)`, by reason, the three
most frequent; in a run that shares, one more, `build: c/c++: N objects compiled here stay on this
machine (…)`, by B-56's reason (an object with two reasons counts under each). CMake's configure
checks count nowhere. The compiler's own output is unchanged.

**B-62 Without a session.** A launcher that finds no usable session (a build that outlived its
run, `HUSHRUN_CC` set by hand) replaces itself with the compiler and the arguments as given, and
reads nothing else. *Rests on:* nothing.

### C and C++ without CMake (draft)

[ADR 0119](../adr/0119-c-without-cmake-and-meson-test.md): GNU Make, the Makefiles autotools writes, Meson, and a `build.ninja` no generator
wrote reach the launcher of B-50 to B-62, whose rules hold for every call it gets. Nothing here is
written outside the run's session but by Meson, whose build folder keeps B-50's indirection, and
nothing the run writes is read once it is over.

**B-230 Builds without CMake.** Wherever B-50 gives COMMAND the launcher, it also makes the
compiler programs (B-231) and sets `MAKEFILES` (B-232); in a Meson repository it puts hushrun's
`meson` on `PATH` (B-234); a COMMAND that is `ninja …` itself on a handwritten build gets the
programs on its `PATH` (B-235). The session begins for them as for CMake (B-50) where a
`RUSTC_WRAPPER` the person set would keep it from beginning: in a folder at or under a
`meson.build`, `configure`, `configure.ac`, Makefile (`GNUmakefile`, `makefile`, `Makefile`) or
`build.ninja` of its checkout. Nothing on Windows. COMMAND's own `PATH` is never changed but by
B-234 and B-235: a compiler found on it is the one the person has, for every tool that writes
down where it found one.

**B-231 The compiler programs.** A folder `cc-programs` of the run's session holds `cc`, `c++`,
`gcc`, `g++`, `clang` and `clang++`, each a POSIX `sh` script, the same text in every run, and a
marker file `.hushrun-cc-programs`. A script finds the first program of its name on `PATH` in a
folder without the marker (an empty entry being the current folder), sets `PATH` to `PATH` without
those folders (each other entry as it was, a trailing `:` kept), and replaces itself with
`$HUSHRUN_CC <that program's path> <arguments>` when `HUSHRUN_CC` names a file it can run, else
with the program itself; with none found it says `<name>: not found` and exits 127. The launcher
then gets the compiler by its path, as CMake gives it, and every rule of B-51 to B-62 holds.

**B-232 GNU Make.** `MAKEFILES` names `cc-programs/hushrun.mk` first, then what the person set,
when its path holds no space, `:` or `$`; otherwise the run says once `build: C and C++ objects of
GNU Make builds aren't reused in this run: the run's folder, <folder>, has a space or a `:` in its
path`. GNU Make reads it before any makefile, in every make COMMAND starts, a recursive one
included; it puts the folder first on `PATH` for make's commands (`export PATH`), once however
deep make starts make, and not where make runs in a folder holding a `CMakeCache.txt`, whose rules
name their compilers by path. Under GNU Make 3 (macOS's 3.81), which starts a command line without a shell
itself and finds its program on its own `PATH`, it also sets `SHELL` to `/bin//sh`, the same shell
under another text, so that every line goes through a shell that reads the exported `PATH`. A
Makefile's own `SHELL` replaces it: another shell does the same, and `SHELL = /bin/sh` again (as
`configure` writes it) leaves the lines without metacharacters to make, whose compiles then pass by
the programs under GNU Make 3. A Makefile that sets `CC` or `CXX` to a name reaches the programs as
its default `cc` does; one that names a compiler by its path, or sets `PATH` itself, doesn't. Make
compares times, not commands: a plain `make` after the run has nothing to do (`make -q`).

**B-233 Tools that write down a compiler's path.** The folder also holds `cmake` and `meson`, which
start the first of their name on `PATH` past the marked folders with `PATH` without them: a build
folder they configure from make's commands names the real compiler, never a script of a session.
The link's linker (B-18) is looked for past the marked folders too. A tool started by its path
from make's commands, or another that writes down the compiler it finds (`which gcc` into a
generated file), is not guarded: the file names a script of a session, which is gone after the
run.

**B-234 hushrun's `meson`.** In a repository with a `meson.build` at its checkout's root, the run
puts hushrun's own `meson` (ADR 0080's programs on `PATH`) first on COMMAND's `PATH`. It starts
the real `meson`, the first on `PATH` as COMMAND had it outside folders of hushrun's programs, with
that `PATH`, stdio, the exit status and the signals passed through, and changes nothing but
`meson setup`'s variables (B-236) and, in a run that reuses tests, `meson test` (C-102). A run whose
test adapter puts programs of its own on `PATH` puts its `meson` there instead.

**B-235 Handwritten Ninja.** A COMMAND whose program is `ninja`, by name, whose build file (`-f`,
else `build.ninja`, in `-C`'s folder, the last one, else COMMAND's) exists, is in a folder
holding neither a `CMakeCache.txt` nor a `meson-private`, and whose first 4 KiB say neither
`# CMAKE generated file` nor `autogenerated by the Meson build system`, gets `PATH` with the
compiler programs first. Nothing is written into `build.ninja`: Ninja keys each output by its
command's text, which stays the same in a run and out of one (`ninja -n` has no work after). A
`ninja` started by a shell line or a script gets nothing from this rule (it does from B-232 when
make starts it).

**B-236 `meson setup`.** hushrun's `meson` (B-234), started with `setup` first while `HUSHRUN_CC`
names a file it can run, sets `CC` and `CXX` for the real `meson`, each to
`/bin/sh -c '<B-50's script>' <compiler>`, `<compiler>` an absolute path quoted as a shell splits
it: with the variable unset or empty, the first of Meson's defaults (`cc`, `gcc`, `clang`;
`c++`, `g++`, `clang++`) on `PATH` past folders of hushrun's programs; set to one word that isn't a
launcher (`ccache`, `sccache`, `distcc`, `icecc`, `buildcache`) nor `/bin/sh`, that word's
program; left as it is otherwise, or when no compiler is found. Meson keeps the compilers for the
build folder's life (`--reconfigure` keeps them), so its `build.ninja` starts B-50's indirection,
which is the compiler outside a run, the launcher inside one, and the same text both ways:
`ninja -n` has no work after the run, and a build after it with hushrun gone is the compiler's.
Meson's `compile_commands.json` names `/bin/sh` first. No other tool COMMAND starts gets `CC` or
`CXX`.

**B-237 A Meson build folder set up before.** After COMMAND, in a run whose mode is `on` or that
shares, each folder COMMAND's words name (`meson … -C <dir>`, a word of `meson setup` that isn't an
option, and B-50's `ninja -C`, `make -C`, `cmake --build`) that holds `meson-private`, and whose
`meson-info/intro-compilers.json` names C or C++ compilers none of which starts with `/bin/sh` or a
launcher, gets one line: `build: C and C++ objects aren't reused in <folder>: Meson set it up
before hushrun's launcher and keeps its compilers; set it up again inside hushrun run (meson setup
--wipe <folder>)`.

**B-238 Configure checks.** Like CMake's (B-51's `cmake-probe`), a compile whose source is
`conftest` (`.c`, `.cpp`, …) and whose output's name starts `conftest` (autoconf's checks), or
that runs in, or writes into, a `meson-private` folder (Meson's), passes through as
`configure-probe` and counts in no manifest: their folders and names say nothing that a later
compile could reuse, and storing them would only fill the store.

**B-240 `hushrun build pull`.** [ADR 0143, ADR 0148] It puts the repository's newest baked store where runs read one. The image is `--image`, else `<registry host of the backend's origin>/<org>/<image name of the repository>` (B-70), and the tag `--tag` (a tag, or `sha256:<64 hex>` for the image itself), else `hushrun-<platform>-latest` for `--platform` or this machine's (B-66). The credential, as Basic `hushrun:<token>`: `HUSHRUN_REGISTRY_TOKEN`, else the backend's token here when the image is on that backend's registry, else none (a public repository's image); for an image on a host that isn't hushrun's registry with none of these, docker's own credentials for that host, as `bake --oci` reads them. When the registry refuses and the credential sent was a session, it says `build pull: <image>:<tag>: this sandbox holds a session, which reads results and not the registry: set HUSHRUN_REGISTRY_TOKEN to a pull token an owner or admin makes for this repository`. It reads the manifest and the config, whose label (B-65) names the store's manifest identity and platform. With `--resolve` it prints `<image>@sha256:<64 hex>` on stdout and reads no layer and writes nothing. When the label's manifest is the `manifest` of the destination's `baked.json`, and the destination passes B-63's check by names, it downloads nothing and says `hushrun: <folder> holds <image>:<tag> (<digest>) already: nothing was downloaded`; `--force` pulls anyway. Otherwise it writes each layer to a file beside the destination as it arrives, hashed on the way, checked against its digest and at most 8 GiB, never holding a layer in memory, and unpacks from each file only entries under `hushrun/`, regular files and folders, into a folder beside the destination; an entry outside the folder, a link, a device or a layer that ends inside an entry stops it. That folder must be a baked store (B-63, `baked.json` read, names and bytes checked), and when `hushrun.yaml` names signers of baked stores, its signature must verify (B-67); with `--require-signed` a store is refused also when `hushrun.yaml` names no signer or there is no `hushrun.yaml`: `build pull: hushrun.yaml names no signer of baked stores, and --require-signed was given: <folder> is as it was. hushrun init --bake prints the lines to add`. Only then does it replace the destination, `--to`, else `HUSHRUN_BUILD_LOWER`, else `/hushrun`, by renames: what was there stays when the swap fails, and no layer file is left behind. It says `hushrun: pulled <image>:<tag> (<digest>) into <folder>: <signed by … | why a run won't trust it unasked>`, with the whole digest, `sha256:` and 64 hex. With `--format json` it also prints `hushrun.build-pull/v1` (`docs/schemas/build-pull.v1.schema.json`) on stdout: `image`, `tag`, `digest`, `manifest`, `platform` and `state` (`pulled`, `already_there`, `resolved`), and, but for `resolved`, `to`, `downloaded_bytes`, `signature` (`named` when a signer `hushrun.yaml` names signed it, `not_checked` when it names none) and `signed_by` (the signer, or null); a field is never removed from `v1`. On GitHub Actions, unless `--no-ci-output`, it writes the step outputs `hushrun-image-digest` and `hushrun-manifest`. A tag that names nothing, a store that doesn't check, or a registry that refuses exits 1, and replaces nothing. A run still decides what it trusts as it begins (B-63, B-64).

## Gradle (draft)

> Draft, from [ADR 0104](../adr/0104-gradle-build-blocks.md) (proposed). Built: sharing through
> the relay. Not built: Gradle blocks in baked stores (B-63) and in `hushrun build fetch` (B-36).

Gradle keys each cacheable task by its own **build cache key** (the hash of the task's
implementation, its inputs by content and its paths as the task normalizes them), keeps entries in
a local cache of its own, and on a miss asks a **remote** cache over HTTP: `GET {url}{key}`, and
`PUT {url}{key}` once it has run the task. hushrun is that remote in a run that shares: entries
are blocks of kind `gradle/1` in the same store, through the same relay, records and outbox as the
other kinds (B-13, B-14, B-26 to B-34). Gradle's local cache stays Gradle's. hushrun computes none
of the key, and Gradle doesn't ask a task's outputs to be the same from one execution to the next:
every check below is the only guard there is, and none of them proves an entry's bytes are what
its inputs make.

**B-77 Hushrun's cache for Gradle.** In a run that has a relay (B-26), `hushrun run` serves a
cache to Gradle when the folder COMMAND runs in, or one above it up to the checkout's (the nearest
folder holding `.git`), holds `settings.gradle` or `settings.gradle.kts`, and that file holds the
text `HUSHRUN_GRADLE` (ADR 0087's line); a settings file without it is said once, `build: Gradle's
tasks aren't shared in this run: <file> doesn't load hushrun's script; add `<line>` after any
pluginManagement {} and plugins {} block`, and nothing more is done. Windows and an isolated check
get nothing. The cache listens on `127.0.0.1` at the store's port (`<store>/gradle/endpoint.json`,
mode 0600: a port from 20000 to 31999 and 64 hex characters of token, made once from
`/dev/urandom`), else, when another process holds that port, at one the system picks, with the
line `build: gradle: another run is using this store's Gradle cache port, so this run's Gradle
can't reuse its configuration cache`. It answers `GET`, `HEAD` and `PUT` of `/cache/<key>` with
Basic authentication `hushrun:<token>` (compared in constant time; anything else is 401), a key of
32 to 128 lowercase hex characters (else 404), HTTP/1.1 with `Content-Length` or chunked bodies,
`Expect: 100-continue` and kept connections, 64 at once, idle ones closed after 120 seconds, and a
peer that isn't the loopback refused. COMMAND gets `HUSHRUN_GRADLE_CACHE`
(`http://127.0.0.1:<port>/cache/`) and `HUSHRUN_GRADLE_CACHE_TOKEN`, and `HUSHRUN_GRADLE` naming
`<store>/gradle/cache-<16 hex>.gradle` unless the run's environment sets it; ADR 0087's test script,
which a check that observes tests sets instead, ends with the same text. That script, once the
settings file is evaluated, gives the build an `HttpBuildCache` remote at that URL with push on
and `allowInsecureProtocol`, unless the build set a remote of its own, which stays; any failure
leaves the build as it was. The cache is used only where the build turns Gradle's build cache on;
where neither the user's `gradle.properties` (Gradle's user home) nor the build's says
`org.gradle.caching=true`, the run says `build: gradle: the build cache is off for this build, so
Gradle asks hushrun's cache nothing: set `org.gradle.caching=true` in gradle.properties, or pass
--build-cache`. *Rests on:* Gradle recording each variable a settings script reads as an input of
its configuration cache, so a URL and a token that are the same from run to run keep it; a remote
set in `settingsEvaluated` surviving a reused configuration cache (both seen on Gradle 8.14.3);
`--offline` turning the remote off, which is Gradle's.

**B-78 Keys and the canonical entry.** A block's key is the SHA-256 of `hushrun.gradle-key/v1\n`
followed by Gradle's key, and its first key is itself. Its one file, `entry.tar`, is the entry in
its **canonical form**: a ustar tar of the entry's members in Gradle's order, each with its name,
type (a file or a folder), permission bits and bytes, owner, group and dates 0, a name over 100
bytes in a PAX header (`path`), ended by two zero blocks; `METADATA`'s `key=value` lines sorted,
comments dropped, `buildInvocationId=hushrunhushrunhushrunhushr`, `creationTime=0`,
`executionTime=0`. Its record holds Gradle's key (`gradle_key`), the task (`task`, unescaped), the
Gradle version (`gradle`) and the task's execution time (`rustc_ms`, from `METADATA`). An entry
Gradle sends is refused unless it is one gzip member (no trailing bytes, its CRC-32 and length
right), a tar whose headers' checksums hold, every member a file or a folder (PAX extended and
global headers and GNU long names read past), every name relative, its parts neither empty, `.`
nor `..`, with no backslash or NUL, exactly one `METADATA`, a file, whose `buildCacheKey` is the key
it was sent under, and at most 1 GiB unpacked; a body over 512 MiB is read and dropped. A block is
served only when its record names `gradle/1` and the key asked, its one file's size and SHA-256 are
its record's, and its bytes are read as an entry of that key whose canonical form is those bytes;
Gradle is sent them as a gzip stream of stored deflate blocks. *Rests on:* Gradle reading an
entry's `METADATA` for its origin alone and dating each file it unpacks with the time it writes it
(seen on Gradle 8.14.3).

**B-79 What may leave.** An entry Gradle stores whose members' names or bytes (`METADATA` aside)
hold, as bytes anywhere, the checkout's folder (as given and resolved), Gradle's user home
(`GRADLE_USER_HOME`, else `$HOME/.gradle`), the store's folder or the home folder, tried in that
order, is not stored: it stays in Gradle's own cache, and its line says which (`checkout`,
`gradle-home`, `store`, `home`). A folder of one character is never searched for. *Rests on:* the
same search as B-10's; a value the task read and wrote in another form is in the bytes and not
found, as B-24 says of every kind.

**B-80 A `GET`.** For a key whose block is suspended (B-15): 404. A block in the store is served
as B-78 says once `Current::may_reuse` allows it (B-34), the question to the relay being
`gradle_vouch`. Else, in a run whose mode is `on`, the cache asks the relay `gradle_fetch`, which
answers only while sharing is on and the check's `gradle/1` manifest (B-82) names the key, as a Go
fetch is answered (B-45): the first such question starts the manifest's prefetch, and one asked
while it looks up waits for it to end; then the download is waited for to its end (B-26, ADR
0144). While a `GET` is worked on, the cache writes `HTTP/1.1 102 Processing` each heartbeat:
Gradle's client reads past it, and its socket timeout ends only a cache that stopped answering,
never one whose download is slow, so Gradle never turns its remote cache off for a busy machine. Every other answer is
404, and Gradle runs the task. A download that fails B-78's checks is reported `rejected` to the
scope it came from, its key suspended for the kept days, and removed. A served block's use is dated
(B-14), and a download's first reuse here is recorded (B-32). A relay that doesn't answer once is
asked nothing more by the cache for the rest of the run. A `HEAD` is 200 when the store holds a block of the key that
isn't suspended, else 404, with no body: it downloads nothing and logs nothing.

**B-81 A `PUT`.** Answered 200 whatever becomes of the entry. A canonical entry B-79 doesn't keep
here is stored as a block built by this run (B-32) and told to the relay (`gradle_built`, with the
task's execution time as its cost), which uploads it where the run may write, as for every kind
(B-29): what the server's scopes let the caller write, a CI job's own scope or `protected/<id>`,
a person's machine its own or `team`. Not stored: a key suspended, a store below B-22's floor. A
block there already that was only downloaded gives way to this execution, which is then told to
the relay; one built here stays. Gradle's entries are never audited (B-16) and never compared in
shared `observe` (B-33): a difference is no sign of a forged entry when a task may make other bytes
each time (a test report's dates, a jar's file dates); it is logged (`differed`) and nothing else.
A Gradle block goes up as class `other`, whatever its record says, since the server suspends a key
whose trusted `library` copies differ and deletes the trusted one; `begin`'s `differs` for it
leaves the block built here as it is, its key neither kept here (B-34) nor discarded. No report
about a Gradle block says it differs: the server suspends a key on a trusted job's `differs`
report whatever the class; one that couldn't be read, was missing or was rejected is reported as
for every kind.

**B-82 Manifest and close.** A check whose run serves the cache asks `open` of kind `gradle/1` and
platform `jvm` beside its own `open`, for its manifest alone, as a Go command's does (B-45), and,
when it logged a Gradle line, sends `close` of kind `gradle/1`, platform `jvm`, under a run that is
the SHA-256 of the check's run followed by `gradle/1\n`, with a manifest of its entries' pairs in
the order logged, leaving out those B-79 kept here and those whose upload hasn't gone. Gradle's
blocks are in no saved manifest, and `hushrun build bake` leaves them out. *Rests on:* the server
taking any kind and keeping manifests per kind (no server change); Gradle's keys holding the
operating system wherever a task's outputs depend on it, so one platform serves every machine.

**B-83 What a run says (Gradle).** The cache's lines go into the run's log beside the other
kinds', the task by Gradle's name in `crate_name`, and the summary counts them apart: `build:
gradle: N tasks: X from hushrun's cache (Y from the shared store), Z run and stored`, with `saving
about T` from the served tasks' execution times; and, when some stay, `build: gradle: N tasks run
here stay on this machine (…)`, by B-79's reason. Gradle's own output is unchanged.

**B-210 Which tasks stay (Gradle).** [ADR 0117] B-83's line of entries that stay ends with the
tasks they are of, by Gradle's name, each once, in the order first seen, three at most, then `and
<n> more`: `build: gradle: 1 task run here stays on this machine (1: its outputs hold the
checkout's folder): :kapt:kaptGenerateStubsKotlin`. hushrun changes no byte of an entry to let it
leave.

## Maven (draft)

> Draft, from [ADR 0113](../adr/0113-maven-build-blocks.md) (proposed). Built: sharing through
> the relay. Not built: Maven blocks in baked stores (B-63) and in `hushrun build fetch` (B-36).

Apache's build cache extension keys each module of a Maven reactor by a **checksum** of its inputs,
keeps entries in a local cache of its own, and asks a **remote** cache over HTTP: `GET
{url}/v1.1/<group>/<artifact>/<checksum>/buildinfo.xml`, then each file that names; after it built a
module, `PUT` of each file, then of `buildinfo.xml`. hushrun is that remote in a run that shares:
a module's entry is one block of kind `maven/1` in the same store, through the same relay, records
and outbox as the other kinds (B-13, B-14, B-26 to B-34). The extension's local cache stays its
own. hushrun computes none of the checksum, and a jar keeps its files' dates: every check below is
the only guard there is, and none of them proves an entry's files are what its inputs make.

**B-170 Hushrun's cache for Maven.** In a run that has a relay (B-26), `hushrun run` serves a
cache to Maven when the nearest folder at or above the one COMMAND runs in, up to the checkout's,
that holds `.mvn` (else `pom.xml`) has a `.mvn/extensions.xml` or a `pom.xml` holding the text
`maven-build-cache-extension`; a build without it is said once, `build: Maven's modules aren't
shared in this run: the build doesn't load Apache's build cache extension; add
maven-build-cache-extension to .mvn/extensions.xml`, and nothing more is done. A build whose
`MAVEN_OPTS`, `MAVEN_ARGS`, `.mvn/maven.config` or `.mvn/jvm.config` holds
`maven.build.cache.remote.url`, or whose `.mvn/maven-build-cache-config.xml` holds `<remote` and
`<url>`, keeps its own remote, with the line `build: maven: the build names a remote cache of its
own, which stays: Maven's modules aren't shared through hushrun`. Windows and an isolated check get
nothing. The cache listens on `127.0.0.1` at a port the system picks, for the run alone, and
answers paths whose first part is a token of 64 hex characters made for the run from
`/dev/urandom` (compared in constant time; any other is 401): `GET`, `HEAD` and `PUT`, HTTP/1.1 with
`Content-Length` or chunked bodies, `Expect: 100-continue` and kept connections, 64 at once, idle
ones closed after 120 seconds, a peer that isn't the loopback refused. COMMAND gets `MAVEN_OPTS`:
the run's own, then `-Dmaven.build.cache.remote.enabled=true
-Dmaven.build.cache.remote.url=http://127.0.0.1:<port>/<token>
-Dmaven.build.cache.remote.save.enabled=true`; where the Maven test adapter (ADR 0088) sets
`MAVEN_OPTS`, its value gains those three words it doesn't hold. *Rests on:* the `mvn` scripts and
the Maven Wrapper passing `MAVEN_OPTS` to the JVM, and the extension reading its properties from
system properties over its configuration file; `-o` leaving the remote on (seen with Maven 3.9.11
and the extension 1.2.0).

**B-171 Paths and keys.** A path is the token, then `v1.1`, a group and an artifact (1 to 255 of
letters, digits, `.`, `-` and `_`, neither `.` nor `..`), a checksum (16 to 128 lowercase hex
characters) and a file name (1 to 255 of letters, digits, `.`, `-`, `_` and `+`, neither `.` nor
`..`); empty parts are read past. Any other path is 404, except a `PUT` of
`build-cache-report.xml`, answered 200 and dropped. A block's key is the SHA-256 of
`hushrun.maven-key/v1\nv1.1\n<group>\n<artifact>\n<checksum>`, and its first key is itself.

**B-172 The block.** A block holds `buildinfo.xml` first, in its **canonical form**: the bytes
Maven sent, with the contents of `buildTime` set to `1970-01-01T00:00:00.000` and of `buildServer`
to `hushrun`; then each file it names, in its order, as Maven sent it. Its record holds the group,
artifact and checksum, the module (`group:artifact`), the extension's `hashFunction` (`hash`) and
the module's cost (`rustc_ms`). A `buildinfo.xml` is refused unless it is UTF-8, at most 16 MiB,
parses to its end with every element closed and no DTD, has one `build` root, its
`cacheImplementationVersion` `v1.1`, its `projectsInputInfo/checksum`, `artifact/groupId` and
`artifact/artifactId` those of its path, and each `fileName` of `artifact` and of
`attachedArtifacts/attachedArtifact` plain text, a file name of B-171's form other than
`buildinfo.xml`, named once, with a `fileSize`; at most 64 files. An artifact with no `fileName` is
no file. A block is served only when its record names `maven/1` and the module asked, its first
file is a canonical `buildinfo.xml` of that module, its other files are exactly those it names, of
the sizes it gives, and every file is of the size and SHA-256 its record holds. *Rests on:* the
extension reading `buildTime` and `buildServer` for its report alone (seen with the extension
1.2.0).

**B-173 What may leave.** An entry whose canonical `buildinfo.xml` or files hold, as bytes
anywhere, the checkout's folder (as given and resolved), Maven's local repository (`$HOME/.m2`),
the store's folder or the home folder, tried in that order, is not stored: it stays in the
extension's own cache, and its line says which (`checkout`, `repository`, `store`, `home`). A
folder of one character is never searched for. *Rests on:* the same search as B-10's; a jar's
compressed entries hide what they hold, as B-24 says of every kind.

**B-174 A `GET`.** Of `buildinfo.xml`: for a key whose block is suspended (B-15), 404. A block in
the store is served as B-172 says once `Current::may_reuse` allows it (B-34), the question to the
relay being `maven_vouch`. Else, in a run whose mode is `on`, the cache asks the relay
`maven_fetch`, which answers only while sharing is on and the check's `maven/1` manifest (B-176)
names the key, as a Gradle fetch is answered (B-80), and the download is waited for to its end
(B-26, ADR 0144). While a `GET` is worked on, the cache writes `HTTP/1.1 102 Processing` each
heartbeat, as Gradle's does: the extension's client reads past it, and its socket timeout ends
only a cache that stopped answering. Every other answer is 404, and Maven builds the module. A download that fails B-172's checks is reported `rejected` to the scope it came from, its
key suspended for the kept days, and removed. A served block's use is dated (B-14), and a
download's first reuse here is recorded (B-32). Of another file: served from the block of a module
whose `buildinfo.xml` this run served, when the block holds it with its size and SHA-256, else 404.
A `HEAD` is 200 when the store holds a block of the module that isn't suspended and holds the file,
else 404, with no body: it downloads nothing and logs nothing. Every question to the relay waits
on its silence alone (B-26), whatever the module's cost; a relay silent for the whole wait once is
asked nothing more by the cache for the rest of the run.

**B-175 A `PUT`.** Answered 200 whatever becomes of the file. The files of a module are held in
memory, at most 512 MiB for every module at once (beyond, the module's are dropped and its line says
so), until its `buildinfo.xml` and every file that names have come, in any order; then the entry is
whole: one whose `buildinfo.xml` fails B-172's checks, or a file of another size than it gives, is
not kept, and its line says why. A body over 512 MiB drops the module's files. The module's cost is
the time from this run's first missed `GET` of its `buildinfo.xml` to the entry being whole, else
0. A run whose Maven skips tests keeps no entry: `skipTests` or `maven.test.skip` set (`-Dname`,
`-Dname=true` in any case, `-D name`, `--define`, the last of each winning) in COMMAND's words,
`MAVEN_OPTS`, `MAVEN_ARGS`, `.mvn/maven.config` or `.mvn/jvm.config`, said once: `build: maven:
<place> sets <property>, so the modules this run builds aren't shared: another build would skip
tests that never ran`. A whole entry B-173 doesn't keep here is stored as a block built by this run
(B-32) and told to the relay (`maven_built`, with its cost), which uploads it where the run may
write, as for every kind (B-29). Not stored: a key suspended, a store below B-22's floor. A block
there already that was only downloaded gives way to this build, which is then told to the relay;
one built here stays. Maven's entries are never audited (B-16) and never compared in shared
`observe` (B-33): a jar keeps its files' dates; a difference is logged (`differed`) and nothing
else. A Maven block goes up as class `other`, as Gradle's does (B-81), and `begin`'s `differs` for
it leaves the block built here as it is, and no report about it says it differs. *Rests on:* the extension restoring an entry built with tests skipped into a build that runs
them, and skipping them, unless the build reconciles the property (seen with the extension 1.2.0).

**B-176 Manifest and close.** A check whose run serves the cache asks `open` of kind `maven/1` and
platform `jvm` once the cache serves, for its manifest alone, as Gradle's does (B-82), and, when it
logged a Maven line, sends `close` of kind `maven/1`, platform `jvm`, under a run that is the
SHA-256 of the check's run followed by `maven/1\n`, with a manifest of its entries' pairs in the
order logged, leaving out those B-173 kept here and those whose upload hasn't gone. Maven's blocks
are in no saved manifest, and `hushrun build bake` leaves them out. *Rests on:* the server taking
any kind and keeping manifests per kind (no server change); the extension's checksum holding the
effective POM, profiles activated by the operating system included.

**B-177 What a run says (Maven).** The cache's lines go into the run's log beside the other kinds',
the module as `group:artifact` in `crate_name`, and the summary counts them apart: `build: maven: N
modules: X from hushrun's cache (Y from the shared store), Z built and stored`, with `saving about
T` from the served modules' costs; and, when some stay, `build: maven: N modules built here stay on
this machine (…)`, by B-173's reason. Maven's own output is unchanged.

## Xcode (draft)

> Draft, from [ADR 0105](../adr/0105-xcode-compilation-cache.md) (proposed). Built, and not yet
> run against a real Xcode: the ADR's Mac spike decides what stays.

From Xcode 26, Swift and Clang key each compile by its content and ask a **compilation-cache
service** for its result before compiling, over gRPC on the Unix socket
`COMPILATION_CACHE_REMOTE_SERVICE_PATH` names. hushrun's service keeps what Xcode puts in a cache
of its own on the machine, and shares a **value** (what Xcode put under one key) with every object
it reaches as a block of kind `xcode/1`, through the same store, relay, records and outbox as the
other kinds (B-13, B-14, B-26 to B-34). Xcode checks nothing it is served: every check below is the
only guard there is.

**B-84 The entry.** When the mode is not `off`, `hushrun run` starts the service for COMMAND when
all of these hold: macOS; `HUSHRUN_BUILD_XCODE` isn't `0`, `off`, `false` or `no`; the checkout's
root or the folder COMMAND runs in holds, within four folders (none hidden, none of `node_modules`,
`Pods`, `Carthage`, `DerivedData`, `build`, `target`, `vendor`), a `.xcodeproj`, a `.xcworkspace`
or a `Package.swift`; and `xcodebuild -version`, asked once within 10 seconds under COMMAND's
`DEVELOPER_DIR`, names a major version hushrun knows (27). Another major is said in one line;
every other reason for none is silent. The service listens on `xcode.sock` in the session's folder
(or a private folder under `XDG_RUNTIME_DIR`, `TMPDIR` or `/tmp` when that path is too long), and
COMMAND gets `XCODE_XCCONFIG_FILE` naming `xcode.xcconfig` beside it: `#include?` of the file the
person's `XCODE_XCCONFIG_FILE` named, unless hushrun wrote it; `COMPILATION_CACHE_ENABLE_CACHING`,
`COMPILATION_CACHE_ENABLE_PLUGIN` (`YES`) and `COMPILATION_CACHE_REMOTE_SERVICE_PATH` (the socket);
`SWIFT_ENABLE_PREFIX_MAPPING`, `CLANG_ENABLE_PREFIX_MAPPING`, `SWIFT_ENABLE_PROJECT_PREFIX_MAPPING`,
`CLANG_ENABLE_PROJECT_PREFIX_MAPPING` (`YES`); `SWIFT_OTHER_PREFIX_MAPPINGS` and
`CLANG_OTHER_PREFIX_MAPPINGS` mapping `$(PROJECT_TEMP_DIR)` to `/^derived`, `$(BUILT_PRODUCTS_DIR)`
to `/^built`, `$(OBJROOT)/../..` to `/^dd` and the checkout's root to `/^workspace`; and an empty
`CLANG_MODULES_BUILD_SESSION_FILE`. A root with white space or `=` in its path gets no maps, and
one line says its keys are its own. The service stops when the run's session ends, before its
relay ends the check. *Rests on:* Xcode reading `XCODE_XCCONFIG_FILE` from the environment and
applying it to every target of the build, a package's included; Xcode 27 mapping those prefixes
before it computes a key (x8, verified against Xcode 27). *Breaks if:* Xcode stops reading the
variable, or a new major changes the protocol or the settings: hushrun names the majors it knows.

**B-85 The protocol.** The service answers `compilation_cache_service.cas.v1.CASDBService`
(`Get`, `Put`, `Load`, `Save`) and `compilation_cache_service.keyvalue.v1.KeyValueDB` (`GetValue`,
`PutValue`) as Apple's LLVM defines them, one HTTP/2 stream a call, one uncompressed message each
way, `grpc-status` 0 in the trailers. A method it doesn't have and a compressed message are
`UNIMPLEMENTED`, a message it can't read `INVALID_ARGUMENT`, one over 2 GiB `RESOURCE_EXHAUSTED`:
the plugin takes each as a miss or a failed put, and Xcode compiles. An object a `Get` or `Load`
asked for on disk (`write_to_disk`) is handed as a file of its own when it holds 1 MiB or more,
since gRPC clients refuse messages over 4 MiB by default; a `Put` or `Save` may name a file, read
up to 2 GiB. A `Put` whose reference is no id of 32 bytes, or names an object the cache doesn't
hold, is refused. *Rests on:* the plugin's taking a reply's file, and falling back to a compile on
any error.

**B-86 The cache.** The service keeps every object and value Xcode puts in the store's `xcode/`
folder: an object at `objects/<2 hex>/<id>`, as `hushrun.xcode-object-file/v1`, the number of its
references (8 bytes, big-endian), each reference and its bytes; a value at `values/<2 hex>/<the
SHA-256 of Xcode's key>`, as `hushrun.xcode-value/v1` and its entries sorted by name. An object's
**id** is the SHA-256 of `hushrun.xcode-object/v1\n`, the number of its references (8 bytes,
big-endian), each reference and its bytes. Files are written whole and renamed into place; one
that doesn't read, or an object whose bytes aren't its id, is removed and is a miss. A `GetValue`
is answered from the cache first, when every object the value reaches is there (a value whose
outputs the service can't name, B-87, as it was put), else the value is removed. A file is dated
when used, at most once an hour; the end of a service removes, at most once a day, what nobody
used for the store's kept days, and temporary and handed files a day old.

**B-87 A block.** A value's **roots** are the references of the `CASObject` under its entry
`value`, and any entry of exactly 32 bytes that names an object the cache holds; everything they
reach, all the way down, is its closure. A block of kind `xcode/1` holds `value` (its one encoding)
and `objects/<id hex>` for each object of the closure, its description `key` (Xcode's, hex),
`value` (the SHA-256 of its encoding), `objects`, `platform` and the store's usual fields. Opening
a block reads every file against its record (B-14), every object against the id its name gives,
the value against its description, and that every object the value reaches is in the block or the
cache; only then are its objects kept and its value served. A block that fails is discarded; one
only downloaded is first reported `rejected` and its key suspended for the kept days.

**B-88 The key (Xcode).** The key of the value under Xcode's key `k` is the SHA-256 of the JCS
form of `{"key": <k, hex>, "schema": "hushrun.xcode-key/v1"}`, its own first key, with no hints.
*Rests on:* Xcode's key holding the compiler's version, its arguments and the content of every
input, as LLVM's and Swift's cache keys do. *Breaks if:* a compiler reads an input its key doesn't
hold: the audit (B-91) is the net.

**B-89 What may leave (Xcode).** A value put in a run with a session becomes a block, and may
leave the machine, only when: its roots can be found (`unknown-value`); every object of its closure
is in the cache (`incomplete`); the closure holds at most 256 MiB and 20,000 objects (`large`);
and, outside CI, no object holds the home folder or the checkout's root in its given or resolved
form, as raw bytes (`holds-path`). Else it stays in the cache alone, reused on this machine, and
the log names why. A block is stored as built here by this run (B-32), and told to the relay.

**B-90 Sharing (Xcode).** The platform of an Xcode block is `xcode-` and `xcodebuild -version`'s
build, lower case. A `GetValue` the cache and the store don't answer is asked of the run's baked
store (B-64), then, in a run that shares `on`, of the relay, which it waits on while the relay
works (B-26's silence, 30 seconds, whatever the key's compile cost; ADR 0144). The relay asks the server only for a key the check's `xcode/1`
manifest for the platform names (asked with `open` at the first such question, as B-60's), and the
first key it names starts the prefetch of the rest. A check closes once for each Xcode build its
values were compiled with, under a run made from the check's, the kind and the platform, with the
pairs of its reuses and its blocks in the order logged, leaving out what stays (B-89). *Rests
on:* the server taking any kind `is_kind` reads and keeping manifests per kind and platform (no
server change).

**B-91 The audit and what a run says (Xcode).** A block served from the store or a download is,
one time in fifty, one in ten at a downloaded block's first reuse, every time in `observe` and
with `HUSHRUN_BUILD_AUDIT=every`, never with `none`, never twice for one key in a run, answered as
a miss on purpose; the value Xcode puts next under the key is compared with it by digest, and a
difference suspends the key for the kept days and reports a download `differs`. A value put
under a key whose block is in the store already is compared the same way, and one only downloaded
gives way to it. The run's summary says, when Xcode asked anything: `build: xcode: N compiles
asked: X from this Mac's cache, Y reused (Z from the shared store), W compiled, V kept to share`,
and how many differed.

**B-92 The Xcode app.** `hushrun build xcode on` (macOS) writes `hushrun.xcconfig` in
`xcode-app/` under hushrun's cache directory, with B-84's settings, the socket `xcode-app/xcode.sock`
(or `/tmp/hushrun-xcode-<uid>/xcode.sock` when too long) and `$(WORKSPACE_DIR)` for the checkout;
writes the LaunchAgent `com.hushrun.xcode-cache` running `hushrun build xcode serve` at login, kept
alive; loads it; and sets `XCODE_XCCONFIG_FILE` for the login session with `launchctl setenv`,
which the agent does again at every start. The service reads and writes the store's cache (B-86)
with no session: it asks no backend and keeps no block. `off` unloads and removes the agent,
unsets the variable only when it names hushrun's file, and removes the file. `status` says which
of these hold. *Rests on:* apps opened after `launchctl setenv` getting the variable, and Xcode
reading it for the app's builds. *Breaks if:* it doesn't: the spike says.

## C++20 modules and precompiled headers

**B-241 An empty response file.** [ADR 0120] An `@file` argument whose file, from the compile's
folder, holds nothing but white space adds no argument: B-51 classifies the call without it, and
B-53 keys it so, as the same compile written without it. CMake 3.28 and later give every scanned
C++20 compile `@<object>.modmap`, empty with clang for a source that imports and exports no
module. A `.modmap` that holds anything passes the call through as `modules`; any other response
file, or one that can't be read, as `response-file`. GCC's scanned compiles pass through as
`modules` by their `-fmodules-ts` and `-fmodule-mapper=` (B-51). *Rests on:* a response file's
words being the only thing it adds, and an empty one none. *Breaks if:* a compiler read a response
file's name for anything else.

**B-242 Precompiled headers.** [ADR 0120] A precompiled header made or used passes through as
`pch` (B-51), and so does `-Xclang` before a flag B-51 counts as `pch` or `modules`, as that flag
would: CMake's `target_precompile_headers` with clang passes `-Xclang -include-pch -Xclang
<header>.pch`. Every other `-Xclang` is `loads-code`. Nothing a precompiled header's user or a
module's importer compiles is stored.

**B-243 A clang that takes the include graph.** [ADR 0120] A clang compile gets B-55's
variables only when the compiler's identity says its driver takes them: asked once with the
identity (B-52), as `/usr/bin/env CC_PRINT_HEADERS_FORMAT=json
CC_PRINT_HEADERS_FILTERING=direct-per-file CC_PRINT_HEADERS_FILE=/dev/null <compiler> -x <lang>
-E /dev/null -o /dev/null`, which must exit 0. clang 15 to 18 know the filtering variable but not
`direct-per-file`, and fail every compile given it; they compile with B-54's fallback, as GCC
does. The identity's memo is `hushrun.cc-compiler-memo/3`, so that each compiler is asked again.

**B-244 The bake's step outputs.** [ADR 0148] On a CI runner, a bake that names a registry
(`--oci`, or the hosted registry) says what it published, for a later job to pull or build a
template from by digest. On GitHub Actions, unless `--no-ci-output`, it writes to `$GITHUB_OUTPUT`:
`hushrun-image` (the repository, with no tag), `hushrun-image-digest` (`sha256:…`, empty when
nothing was published), `hushrun-image-tag` (the moving tag it moved, else the fixed tag),
`hushrun-image-published` (`true` when `image.state` is `published`, `false` when the re-bake limit
held it, B-73) and `hushrun-manifest` (the store's manifest identity). When `HUSHRUN_BAKE_ENV` names
a file, it writes the same values there as `KEY=value` lines, the names uppercased with `_`
(`HUSHRUN_IMAGE_DIGEST=…`), for GitLab's `artifacts: reports: dotenv`; a file outside the
project's folder (`CI_PROJECT_DIR`, else the current folder) is refused with a line, and the bake
exits 1. The text and the JSON are as they were. `hushrun init --bake` (B-69) gives the GitHub
bake step `id: bake` (and the branch's `id: bake_branch`) and the job these five as its `outputs`,
and sets `HUSHRUN_BAKE_ENV: hushrun-bake.env` with `artifacts: reports: dotenv: hushrun-bake.env`
on the GitLab job.
## TypeScript (draft)

> Draft, from [ADR 0106](../adr/0106-typescript-build-blocks.md) (proposed). Built: local reuse
> between worktrees and runs, and sharing through the relay. TypeScript 4 to 6 only: TypeScript 7
> and later compile in native code `bin/tsc` starts, whose reads a Node process can't see, and pass
> through.

A **compile** here is one run of the TypeScript compiler's `tsc` in a Node process: a plain
`tsc`, `tsc -p <config>`, `tsc --noEmit` or `tsc --build` over project references, one process
whatever the number of projects. A compile's **block**, kind `tsc/1`, holds every file it wrote,
its standard output and error, and its exit status, in the same store, through the same relay,
records and outbox as the `rustc` wrapper's (B-13, B-14, B-26 to B-34). A **root** is the
repository's folder as B-50 names it, in its given and resolved forms.

**B-100 The hook.** When the mode is not `off`, `hushrun run` begins the session as B-2 says and,
in a folder at or under a `package.json` of its checkout, writes the hook (`hushrun-tsc-<digest>.cjs`,
named by its content) into the store's `tsc/` folder, sets `NODE_OPTIONS` for COMMAND to what
COMMAND's environment had followed by `--require <hook>` (the path in double quotes, backslashes
doubled, when it holds white space; a path with a quote sets nothing and costs a line), and
`HUSHRUN_TSC` to a link named `hushrun-tsc` beside the wrapper's (B-2), pointing at the hushrun
binary. Nothing on Windows or for an isolated check. The hook does nothing in a Node process without
`HUSHRUN_TSC`, in a worker thread, or whose main file, after links, isn't a compiler its table
names: TypeScript's `bin/tsc` or `lib/tsc.js` in a package named `typescript` of major version 4
to 6. Every other Node process pays one test of its main file's name. A test adapter that sets
`NODE_OPTIONS` (C-44's Jest) adds to the hook's, never over it (C-50).

**B-101 Passed through.** The compiler runs as asked, nothing stored, and the run's log says why,
for `--watch` or `-w` (`watch`); `--init`, `--help`, `--all`, `--version`, `--showConfig`
(`prints`); `--diagnostics`, `--extendedDiagnostics` (`timings`); `--generateTrace`,
`--generateCpuProfile` (`trace`); `--build` with `--verbose`, `--dry` or `--clean` (`build-mode`);
a working folder outside every root (`cwd-outside-roots`); a compiler file that can't be read
(`compiler`); a store that can't be read (`store-read`). Options are matched as the compiler
matches them, in any case, after one or two dashes. Every other argument is keyed by its text.

**B-102 The first key.** The SHA-256 of the JCS form of: the kind, the compiler's row and the
version its `package.json` names; the compiler's identity, the SHA-256 of the file its row names
(`lib/_tsc.js`, else `lib/tsc.js`), memoised under the store by path, size, dates, inode and device;
each argument with every root's text replaced by `{root}`; the working folder from the root; Node's
`process.platform`; whether file names keep case, as the compiler decides it (its own file not
found under its name with every letter's case swapped); whether standard output is a terminal (the
compiler's default for `--pretty`); the values of `NO_COLOR`, `FORCE_COLOR` and `NODE_ENV` that are
set; and the person's and the repository's keyed variables (B-5, B-35).

**B-103 The second key.** While the compiler runs, the hook records what it did through Node's
`fs`, Node's own calls inside a call left out: each file read, with the SHA-256 of the very bytes
it was given; each path looked at (`statSync`, `lstatSync`, `existsSync`, `accessSync`) as a file,
a folder, another kind or absent; each folder listed, by its entries' names and kinds; each path
resolved (`realpathSync` and its `native`), by where it led. What the compile wrote earlier in the
same process (one project's declarations that `--build` reads for the next) is left out of all
four. Paths under a root are recorded from it (`{root}/src/a.ts`). A path outside every root
looked at and absent, below a folder above the root (the compiler's walk up for `package.json` and
`node_modules`), is kept as its name below that folder, and holds where that name is absent below
every folder above the root of the checkout that looks it up, whatever its depth. Any other path
outside every root is kept as it is, and ties the block (B-104). The compiler's look at its own
file under swapped case is in the first key, not here. The **hint**, written under the first key,
holds the four lists sorted, and the second key is the SHA-256 of the first key, the hint and the
reads' digests in its order. A lookup takes the hints of the first key, the newest first, and the
first whose every kind, listing, link and absence holds here and whose reads, hashed now, give a
key the store holds, is the block. A hint is never trusted: a wrong one gives a key nothing holds.

**B-104 Not stored.** A compile is stored only when the hook recorded no error of its own
(`hook-error`) and nothing a block can't hold or key, each said by its name: reading through a
file descriptor (`read-fd`), an asynchronous call or a watch (`async-…`), and removing, renaming,
copying, linking, appending to, truncating or re-dating a file (`unlink`, `rename`, …); when it
ended with the compiler's own status 0, 1 or 2 (`status`); when every file it wrote is under a
root and there when it ended (`write-outside-roots`, `output-gone`); when its messages hold no
`{root}` of their own (`placeholder`); for `--build`, when it read no `.tsbuildinfo`
(`build-over-outputs`): the compiler then decides by the dates of outputs it finds, which no key
holds, so only a build that found none is stored; and when the store's volume has the space B-22
asks. A block whose written files hold a root's text, or whose hint names a path outside the
roots, is **tied**: stored under the SHA-256 of its key and the roots, so only a checkout at the same
folder finds it, and it never leaves the machine. Every root's text in the messages is stored as
`{root}`.

**B-105 Restore.** A block found, vouched for (B-108) and not picked by the audit is put in place
before the compiler loads: each file, checked against the size and SHA-256 its record holds and
written under a temporary name in its folder (made when missing) and renamed into place, dated
now; then the hook prints its standard output and error, the roots replaced by this checkout's,
and ends the process with its status. A file that fails its check, or a write that fails, runs the
compiler as asked, which writes over whatever was put in place (`restore`). A plain `tsc --build`
after a restore finds every project up to date, its outputs newer than its sources. In `observe`,
and for a compile the audit picks (B-16: one time in fifty, one in ten at a download's first reuse,
`HUSHRUN_BUILD_AUDIT`), the compiler runs, and what it wrote and printed is compared, file by path
and digest, with the block found: a difference suspends the first key for the kept days and says
so, and a downloaded block that differs is reported `differs`.

**B-106 Fallback.** Nothing the hook or the helper does changes what the compiler does unless a
block is put in place whole: a missing session, a helper that can't be started or answers
anything else, a panic, a store that can't be written, each runs the compiler exactly as asked.
The compiler's standard output and error pass through as it writes them; the hook keeps a copy.
The helper is asked twice: before the compiler loads (`hushrun-tsc lookup`, the call on standard
input, its answer on standard output) and as the process ends (`hushrun-tsc store <plan>`), whose
outcome never changes the process's status.

**B-107 Type checks.** A compile that writes nothing, `tsc --noEmit` among them, is a block of its
messages and status alone: a type check whose program read the same bytes, kinds, listings and
links prints the same errors and ends with the same status without the compiler starting, in any
checkout.

**B-108 Sharing.** In a run that shares (B-25), a block that isn't tied goes up as kind `tsc/1`
under its first key, class `library`, its platform Node's `process.platform`, when the helper tells
the relay it was built; a check closes once for each platform its compiles ran on, with the pairs
of its reuses and blocks, as B-60's. A lookup that finds nothing here asks the relay for the first
key's hints only when the check's `tsc/1` manifest for the platform names it (asked with `open` at
the first such question), waiting on the relay's silence alone (B-26; ADR 0144), and
fetches one of at most eight second keys the downloaded hints lead to. A downloaded hint is followed
only when every path it names is under `{root}` or a name above it, with at most 200,000 paths.
A downloaded block is served only as `Shared::may_reuse` says (B-34), its files' sizes and SHA-256
checked as B-105's, every path under `{root}`, and its messages with every control character but a
newline, a tab and SGR colour taken out; one that fails is reported `rejected`, its first key
suspended for the kept days and the block removed. TypeScript blocks aren't baked (B-63). *Rests
on:* the server taking any kind `is_kind` reads (no server change).

**B-109 What a run says (TypeScript).** The run's summary says, when the hook asked anything:
`build: typescript: N compiles: X reused (Y from the shared store), Z built`, with what reuse saved
from a second on and how many were compared and differed; and `build: typescript: N compiles run as
asked or not stored (<count> <reason>, …)`, the three most frequent reasons first.

## Rust's checks and clippy

[ADR 0112] `cargo check` and `cargo clippy` ask rustc for metadata only, for every target, and
`cargo clippy` runs each workspace member's compile through clippy's driver. These rules extend
B-4 to B-6 for them; every other rule reads as it does for a library.

**B-160 A compile that links nothing.** A compile whose `--emit` has no `link` (with `--emit`
absent, rustc's default is `link`) writes what a library's check writes, `lib<crate name><extra
filename>.rmeta` and the dep-info, whatever its crate type (`lib`, `rlib`, `proc-macro` or
`bin`) and with `--test` too: it is class `library` (B-3's log, B-5's variables, B-6's native
folders, B-24), never a linked output, and gets no `--print=link-args` (B-9). `cargo check
--all-targets` and `cargo clippy --all-targets` ask this of every program, example, test, bench
and proc macro's own crate, which were each `built, not stored` (`link-inputs`) before. A crate
type B-4 passes through (`cdylib`, `dylib`, `staticlib`, or more than one) still does. *Rests on:*
rustc writing no object and running no linker for `--emit=dep-info,metadata` (1.94.0: a program's
and a test's check write the `.rmeta` and the `.d` alone). *Breaks if:* a rustc ran the linker for
a metadata-only compile.

**B-161 Clippy's driver.** A call whose first argument's file name is `clippy-driver` (or
`clippy-driver.exe`) and whose second is rustc (B-4) is read as the rustc call after the driver
when B-160 makes it a library; it passes through as `not-rustc` otherwise, as every call through a
driver did before: a workspace member's build script and proc macro under `cargo clippy`. The
wrapper starts the compile, and each audit, place check and comparison of it, as cargo did:
the driver, then rustc's path, then the arguments. The compiler's identity in the first key is
the SHA-256 of the JCS form of `{"schema": "hushrun.build.driver/1", "compiler": <B-17's
identity>, "driver": {"name": <its file name>, "sha256": <its bytes'>}}`, the driver's bytes read
once per install (a memo in the compiler memos, `driver-<digest>.json`, named by its path, size
and dates); a driver that can't be read passes the call through (`compiler`). The first key's
variables add every variable whose name starts with `CLIPPY_` or `__CLIPPY_` (`CLIPPY_ARGS`, the
lints `cargo clippy -- …` gives; `CLIPPY_CONF_DIR`; `CLIPPY_DISABLE_DOCS_LINKS`), with roots
replaced. The driver lists in the dep-info the configuration it read (`clippy.toml` or
`.clippy.toml`), the package's `Cargo.toml`, and `CLIPPY_ARGS` and `CLIPPY_CONF_DIR` as
`env-dep` lines, but not the places it looked for a configuration and found none: B-6's
`declared` holds each of `.clippy.toml` and `clippy.toml` as an upward search, as a macro's
`.env` is held, from `CLIPPY_CONF_DIR` when it is set and not empty, else from the package
folder; so a configuration that appears where the driver would find it first changes the key.
The diagnostics, clippy's lints among them, are stored and replayed as rustc's are (B-12).
*Rests on:* the driver linking the toolchain's `rustc_driver` (in B-17's identity) and reading
nothing else that changes its output than its arguments, those variables and the files above
(clippy 0.1.94: `clippy_config::lookup_conf_file`, `clippy_driver::track_files`). *Breaks if:* a
clippy read another file or variable without listing it.
