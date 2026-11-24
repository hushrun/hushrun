# 0109. Svelte

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-62, C-63, C-64, C-65; relies on C-30, C-31, C-35, C-40, C-43, ADR 0071, 0084

## Context

Svelte's tests run in Vitest: Svelte 5 components (`.svelte`) and runes modules (`.svelte.js`,
`.svelte.ts`), compiled by `@sveltejs/vite-plugin-svelte`, rendered with `@testing-library/svelte`
in jsdom, in a plain Vite project or a SvelteKit app. An audit on a small real project
(Vitest 4.1.12, Svelte 5.59.1, vite-plugin-svelte 7.3.2 and 6.2.4, SvelteKit 3.4.0 and 2.70.4,
jsdom 31 and 26, on Node 22) found that hushrun reused none of it:

1. **The run broke.** In a worker whose environment isn't Node (jsdom, happy-dom), Vitest asks
   Vite for each module through `/@fs/`, which Vite serves only from `server.fs.allow`: the
   workspace, or the list a plugin sets (SvelteKit's). hushrun's setup file is in the cache
   directory, outside it, so every test file failed with `Cannot find module
   '/@fs/…/hushrun-vitest-setup.mjs'`, with no hushrun in the config but the line ADR 0071 has
   init propose. This is every component test of every framework, not Svelte's alone.
2. **Nothing was observed.** vite-plugin-svelte's plugins (`vite-plugin-svelte:compile`, …) are
   plugins ADR 0071 §5 doesn't know, so every test file of a project that has them is
   `not observed`, always runs, and says so.
3. **`.svelte` was in the rest set** (ADR 0071 §4): a change to any component ran every test
   file.
4. **What the compile reads beyond the component isn't seen.** vite-plugin-svelte runs the Svelte
   config's preprocessors in Vite's own process, not the worker, and a preprocessor may read other
   files (a `<style lang="scss">`'s imports, a markup preprocessor that includes a file), which it
   says in its result's `dependencies`. The Svelte config itself (`svelte.config.js`), which
   vite-plugin-svelte or SvelteKit 2 loads, is no Vite config dependency.
5. **SvelteKit.** Its plugins make no module from other files in a Vitest run but these:
   SvelteKit 2's `$env/*` and `__sveltekit/*` are virtual modules (`\0virtual:…`), which ADR 0071
   counts as `not observed`, so any test file that reaches `$app/environment` always ran; SvelteKit
   3 writes them as files below its `outDir` (`.svelte-kit/generated`), which git ignores, from an
   env entry (`src/env.*`) read in Vite's process.

What already worked once the run loaded: a compiled component's module id is its `.svelte` file,
so the worker's appended statement records it, and V8's coverage names the compiled functions by
it, so a test that rendered a component ran it (C-40). A runes module is a `.js` or `.ts` file.

## Decision

1. **Vite serves the setup file.** The plugin adds its own directory to the resolved
   `server.fs.allow`, which Vite reads at each request: the workspace stays allowed, and a list a
   project or plugin set is kept, with that one directory added. ADR 0108 (Vue) found the same
   failure and makes the same change, in the same words; this decision holds it for SvelteKit's
   own `fs.allow` list too.
2. **`.svelte` is a source** (C-30): a change to one runs the test files, and with `reads: test`
   the tests, that ran it. A kind a Svelte config adds through `extensions` (mdsvex's `.svx`) stays
   in the rest set.
3. **vite-plugin-svelte's plugins are known**, by name (3 to 7, every one its `svelte()` returns),
   with `@testing-library/svelte`'s `svelteTesting()` (`vite-plugin-svelte-testing-library`, config
   alone): each makes a module from that module's own file. The inspector, when on, makes its
   modules from none, which are virtual and stay `not observed`.
4. **What a preprocessor read is read by whoever runs the component.** After vite-plugin-svelte
   resolves, the plugin wraps each preprocessor group's `markup`, `script` and `style` in place
   (vite-plugin-svelte keeps the group objects it was given), keeping the `dependencies` each
   result names, by the file preprocessed. When the component's module is transformed, it appends
   them to the module's record: the test file read them (`reads: file`), and, as a barrel's
   re-exports are (ADR 0084 Amendment 1), each test that reads the component reads them. A
   property the function carries (`vitePreprocess`'s `__resolvedConfig`) stays where it was and is
   copied onto the wrapper. A preprocessor that reads a file and names none in `dependencies` is
   outside what is watched, as a file any Vite plugin reads in Vite's own process is.
5. **The Svelte config is a config file** (every test file reads it, C-35): the file
   vite-plugin-svelte says it loaded, and, with SvelteKit, any `svelte.config.{js,mjs,ts,mts}` in
   the root, which SvelteKit 2 loads itself; with each file of the repository either imports by a
   relative path, read from its text and followed to the end (one read where there is none only
   widens every list).
6. **SvelteKit's plugins are known** (2 and 3, by name), and what it makes its env modules from
   is a config file: the env entry (`src/env.*`, with what it imports) and the `.env` files of its
   env directory, which a tracked one is anyway as the rest set. SvelteKit 2's virtual modules made
   from those and its config (`$env/static/*`, `$env/dynamic/*`, `__sveltekit/env…`,
   `__sveltekit/server`, `__sveltekit/environment`, `__sveltekit/paths`) aren't recorded as
   virtual; any other virtual module (`$service-worker`, another plugin's) still is. The rest of
   `.svelte-kit/generated` (`server.js`, the manifest, route nodes) is made from the Vite config,
   `app.html` and `error.html` (rest), which hooks and routes exist (the sources' shape, C-31) and
   files a test that needs them imports itself.
7. **No build blocks.** `svelte-check` writes nothing a later build reads, and its check already
   reuses whole. The Svelte compile inside `vite build` is a pure function of the preprocessed
   source, the file name, the compile options and Svelte's version, so a block keyed by those
   would be sound; but Vite has no compile cache protocol for hushrun to serve, as Gradle's remote
   build cache or Xcode's compilation cache are, and the compile is a small part of a build: 400
   components compiled in 0.55 to 0.8 s of a 1.7 s `vite build` on this machine, about 1.4 ms a
   component, while `vite build` itself already runs only when the check didn't pass on this code.
   B-130 to B-139 stay unused.

## Consequences

- A Svelte or SvelteKit project with ADR 0071's line reuses its test files and tests as a plain
  TypeScript one does, and a component's change runs only what rendered it.
- A component test in jsdom or happy-dom with any framework loads hushrun's setup file.
- Every test file reads the Svelte config and what it imports, and with SvelteKit the env entry: a
  change to one runs every test file, as a change to the Vite config does.
- A value of hushrun's own environment that SvelteKit's env modules or Vite's `import.meta.env`
  bake in isn't keyed, as before.
- Not verified: SvelteKit 3 and jsdom 31 need Node 22, and CI's Node is 20.20.2, so CI tests
  SvelteKit 2 with jsdom 26 and vite-plugin-svelte 6; SvelteKit 3 and vite-plugin-svelte 7 were
  tried here, on Node 22, by hand. Svelte with Jest (`svelte-jester`) is unchanged: Jest's adapter
  keeps `.svelte` in the rest set.

## Follow-ups

- `svelte-check --incremental` keeps svelte2tsx's output and a TypeScript build info file below
  `.svelte-check` (or `.svelte-kit`); once the TypeScript build blocks land, that build info may be
  theirs to share, and svelte2tsx's output a block of its own.
- The cloud UI and backend need nothing: no block kind is added.

## Alternatives considered

- **Treating every `.svelte-kit` file as made from the whole of `src`:** sound, but every test
  that reaches `$app/*` would read every file of `src`; what each generated file is made from is
  narrower, and is said in §6.
- **Leaving `server.fs.allow` alone and writing the setup file into the workspace:** a file in a
  checkout that git would see, and that a plugin's own `fs.allow` (SvelteKit's) still refuses.
- **Reading a preprocessor's reads in Vite's process with the `fs` wrapper:** Vite's process
  serves every test file at once, so a read can't be told to one component; `dependencies` is
  what vite-plugin-svelte itself watches by.
