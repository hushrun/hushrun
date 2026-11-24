# 0106. TypeScript build blocks through a hook in the compiler's Node process

- **Status:** proposed; local reuse and sharing through the relay are built
- **Date:** 2026-11-24
- **Rules:** B-100 to B-109 (draft), C-50 to C-52; B-13, B-14, B-16, B-22, B-25 to B-34 read for
  TypeScript

## Context

hushrun skips a `tsc` check whose inputs passed already, but any change in a checkout runs the
whole compile again, and `tsc` is whole-program: a type check of a mid-sized monorepo is tens of
seconds to minutes, every time a new worktree, branch or CI job starts from a clean tree. ADR 0097
set TypeScript aside for C and C++ because there is "no per-unit compiler call to wrap". There is a
per-*call* one: each `tsc` (or `tsc --build`, one process over every project reference) is a
deterministic function of what it reads, and what it writes is a handful of text files.

What a `tsc` reads isn't knowable from its arguments: module resolution probes hundreds of paths
that don't exist (`index.ts`, `index.d.ts`, `package.json` up every folder to `/`, `node_modules/@types`),
`include` globs list folders, `extends` chains and `references` add configs, and pnpm's links are
resolved. A key from a list of declared inputs (ccache's direct mode for C) would serve stale
outputs the first time a `package.json` with `"type": "module"` appears where one was looked for.

TypeScript 4 to 6 compile inside the Node process `bin/tsc` starts, through a small file-system
layer (`ts.sys`) over Node's `fs`: `readFileSync`, `statSync` with `throwIfNoEntry: false`,
`readdirSync` with file types, `realpathSync.native`, and `openSync`/`writeSync`/`closeSync`,
`mkdirSync`, `utimesSync`, `unlinkSync` (checked in TypeScript 6.0.3's `_tsc.js`). Everything the
compiler knows of the disk passes there. TypeScript 7, `latest` on npm since this autumn, is native
Go code that `bin/tsc` `execve`s: none of that passes through Node.

## Decision

1. **Entry: a `--require` hook in `NODE_OPTIONS`, inert outside a compiler** (B-100). `hushrun run`,
   in a checkout with a `package.json`, adds `--require <hook>` to COMMAND's `NODE_OPTIONS` and sets
   `HUSHRUN_TSC` to a link `hushrun-tsc` beside `hushrun-rustc`. The hook is written into the
   store's folder named by its content, so a process that outlives the run still starts. In any
   Node process whose main file isn't a compiler of its table it costs one regular expression; the
   table's one row is TypeScript's `bin/tsc` or `lib/tsc.js` of a `typescript` package of major
   version 4 to 6. `npm run`, `npx`, pnpm's shell shims and `node node_modules/typescript/bin/tsc`
   all reach it. Refused:
   - A `PATH` shim named `tsc`: `npm run` and `npx` put `node_modules/.bin` first, so it never runs.
   - Running `tsc` ourselves with `--listFiles --traceResolution` and parsing its output: it would
     change what the compile prints, and misses `readdir`, configs and links.
   - `strace`/`ptrace`: Linux only, slow, and needs a capability CI containers often lack.
2. **The key: the compiler's own reads, through `fs`, as it made them** (B-102, B-103). The first
   key is what was asked (the compiler's file by content, arguments, folder, platform, case, TTY,
   colour variables). The second is what was read: every file by the SHA-256 of the bytes the
   compiler got, hashed in the hook as it got them, so a file changed afterwards keys another
   block; every path looked at by its kind or absence; every folder listed by its entries; every
   link by where it led. What the compile wrote earlier in the process (`--build` reading one
   project's `.d.ts` for the next) is its output, not its input, and is left out. Paths are kept
   from the checkout's root. The compiler's walk up for `package.json` and `node_modules` above the
   checkout is kept as names absent below *every* folder above the root, which holds at any depth,
   so a laptop at `/Users/me/src/app` and CI at `/home/runner/work/app/app` share. Anything else
   outside the checkout ties the block to the checkout's folder.
3. **What a block holds and how it is put back** (B-104, B-105). Every file written, standard output
   and error with the root replaced by `{root}`, and the exit status (0, 1, 2: errors are as
   reusable as success). A hit writes each file whole and dated now, then the hook prints the
   messages and exits with the status before the compiler loads; a plain `tsc -b` afterwards finds
   everything up to date. Not stored: reads through a descriptor, asynchronous calls, removing,
   renaming or re-dating a file, writes outside the checkout, and **`--build` over existing
   `.tsbuildinfo` files**, where `tsc` decides by file dates (output newer than every input) that
   no key holds. A clean build is what a new worktree and a CI job run, and is the one that matters.
4. **`--noEmit` is a block of messages** (B-107): a type check reused with its errors and status.
5. **Sharing as C and C++ do** (B-108): kind `tsc/1`, class `library`, manifest per check and
   Node platform, hints asked only when the manifest names the first key, at most eight second
   keys fetched, `Shared::may_reuse` and the audit's pick as every kind's, a downloaded hint
   followed only when every path it names is under `{root}` or a name above it, a downloaded
   block's messages filtered to SGR colour, a failing one reported `rejected` and suspended. The
   server needs nothing new: kinds are strings.
6. **Never in the way** (B-106). Any failure answers "run as asked"; the store step runs from the
   process's `exit` handler and can't change its status; the compiler's output passes through as
   written.
7. **Built for more compilers than `tsc`.** The hook's table is the only thing that names
   TypeScript: a row gives the file the process runs, the package, and the file whose bytes are the
   identity. vue-tsc and svelte-check run the same `ts.sys` over Node's `fs` and need a row each
   (and a look at what of theirs they load besides), not new machinery. Nothing here was tried with
   them.

## Consequences

- Measured on this machine (Linux, Node 22, TypeScript 6.0.3): a two-file project, 1.1 s cold,
  0.39 s for a whole `hushrun run` whose compile was reused (most of it hushrun's own start); a
  two-project `tsc -b`, reused in a second checkout and in a second machine at another path through
  the fake backend, byte for byte. The hook records about 70 reads, 13 looks, 2 listings for the
  small project; a monorepo's thousands are hashed at a hit (lookups hash in Rust, the miss's
  hashing is the hook's, in Node's native SHA-256).
- `NODE_OPTIONS` in COMMAND carries a `--require` of a file under hushrun's cache. A script that
  replaces `NODE_OPTIONS` drops it (and Jest's adapter's); the guide says to keep what it had.
- TypeScript 7 gets nothing until hushrun can see what native `tsc` reads; the run says nothing
  about it (it is not a Node `tsc` the table names).
- `--build` over existing build state isn't stored: a worktree that builds again and again only
  reuses its first build. TypeScript's own incrementality covers that case.
- A `node_modules` linked in from outside the checkout, or a global `tsc`, ties every block to the
  checkout: it reuses there, never elsewhere.
- Test reuse (C-50 to C-52): the audit that went with this found no stale reuse in TypeScript
  tests with Vitest 4.1 (vite-tsconfig-paths and Vite's own tsconfig paths) and Jest 30 (ts-jest
  with and without `isolatedModules`, @swc/jest, babel-jest). Every `tsconfig*.json` is a rest file,
  so a change to one runs every test file; erased imports are no read. One needless re-run was
  fixed (ts-jest listing `include` read every file for the first test file of each process), Jest's
  `NODE_OPTIONS` now goes after the build's hook, and `node --test`, `tsx --test` and Mocha, which
  have no adapter, say so.

## Alternatives

- **Per-project blocks inside `tsc --build`**, by hooking the builder's program per project:
  finer, and it would reuse the unchanged projects of an incremental build; it needs TypeScript's
  internals, which change between releases. Left for later, if whole-call blocks prove too coarse.
- **Keying `--build` by dates as well**: dates never match between checkouts, so it would reuse
  nothing that the clean-build rule doesn't.
- **A Language Service plugin**: runs only in editors and tsserver, not in `tsc`.

## Not verified

- macOS (case-insensitive file systems: the compiler's case probe is in the first key, and the
  table's paths are compared as given), and Windows (nothing is set there).
- vue-tsc, svelte-check, Yarn Plug'n'Play (its `fs` patch sits under the hook; untried), and a real
  monorepo's size and hit cost.
- Sharing against the real backend: only the fake build protocol.
