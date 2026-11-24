# 0108. Vue's components

- **Status:** accepted
- **Date:** 2026-11-24
- **Rules:** C-58, C-59, C-60, C-61; relies on C-30, C-31, C-40, C-43, C-44, ADR 0071, 0084, 0085

## Context

A Vue project tests its single-file components (`.vue`) with Vitest, through
`@vitejs/plugin-vue` and `@vue/test-utils` in a `jsdom` or `happy-dom` environment, or with Jest
through `@vue/vue3-jest`. Before this decision, tried on a small project of each (Vitest 4.1.12,
Vue 3.5.46, plugin-vue 6.0.11, test-utils 2.6.0, jsdom 26; Jest 29.7.0, vue3-jest 29.2.6), three
things held:

1. **Vitest in `jsdom` failed every test file.** A browser environment loads setup files through
   Vite's `/@fs/`, which serves only what `server.fs.allow` holds. hushrun's setup file lives in
   `<cache dir>/adapters/`, outside the project, so Vitest said `Cannot find module
   '/@fs/…/hushrun-vitest-setup.mjs'` and the check failed under hushrun while it passed alone.
   This was so for any Vitest project in `jsdom` or `happy-dom`, Vue or not.
2. **A component was in the rest set (C-30).** `.vue` was a source kind of neither adapter, so a
   change to any component was a change to every test file's key: everything ran. Sound, but test
   reuse did nothing for what a Vue project changes most.
3. **Vitest saw a module it couldn't name.** Every component's compiled code imports
   `\0plugin-vue:export-helper`, which plugin-vue makes from no file, so every test file that
   mounted a component was `not observed` ("a module a Vite plugin made") and always ran.

Making `.vue` a source needs every read of a component to be one the adapter sees. What
plugin-vue makes of a component, tried one by one:

- the component's own modules (`A.vue`, `A.vue?vue&type=style&index=0&lang.module.css`) are
  named by its file, the query dropped (ADR 0071);
- a block's `src=` (`<script src="./logic.ts">`, `<template src="./a.html">`, `<style src>`) is a
  module of its own, `./logic.ts?vue&type=script&src=true&lang.ts`, named by that file;
- a `<script setup>`'s imports are imports of the compiled module;
- **a type imported for a macro** is not. For `defineProps<Props>()` with `import type { Props }
  from './types'`, @vue/compiler-sfc reads `types.ts` in Vitest's main process to compile the
  runtime props (their names, `Boolean` casting, required ones), and the import is erased: no
  module of `types.ts` ever runs. Changing `on?: boolean` to `on?: string` there changes what the
  component does with `<Flag on>`, and no test's list held the file.

Under Jest, vue3-jest compiles the component in the worker, under its own file name, and reads a
`src=` file through `fs`, which the adapter wraps (ADR 0085). It resolves no imported type: its
compile has no file system, and `defineProps<Props>()` from another file fails under Jest alone.

## Decision

1. **`.vue` is a source kind** of the Vitest adapter and of the Jest adapter (C-58). A component
   is read by the test files whose worker ran it, and with `reads: test` by the tests that ran its
   code or loaded it (C-40). CSS, HTML and other files a `src=` names stay rest: a change to them
   runs every test file.
2. **plugin-vue's helper is no file.** `\0plugin-vue:export-helper`, the same text from the
   plugin's own code for every component, is told as Vite's own helpers are: nothing (C-58). Any
   other module made from no file still makes its test file `not observed`.
3. **What a component's compile read** (C-59). In `configResolved`, hushrun's Vitest plugin wraps
   the compiler `vite:vue` holds (`api.options.compiler`, Vue's documented way to give the plugin
   a compiler) with one whose `compileScript` calls the original and keeps, by component, the
   files its result's `deps` names: those type resolution read, followed to the end through the
   types' own imports and `extends`, a `.vue` file's own types included. The plugin appends them
   to the component's module, as it appends re-exports (ADR 0084, Amendment 1), and the worker
   adds them to the test file's reads and, with `reads: test`, to the component's re-export edges:
   a test that reads the component reads them, to the end (C-40 rule 5). A `vite:vue` whose
   compiler can't be wrapped is a plugin hushrun doesn't know, and its project is `not observed`.
4. **A browser's environment** (C-60). In `configResolved` the plugin adds its own directory to
   the `server.fs.allow` Vite resolved, which keeps the project's own entries and defaults. It
   changes nothing else Vite serves.
5. **Jest** (C-61). Nothing changes for vue3-jest but the source kind: its compiled component is a
   module the adapter sees, and a `src=` file is read through `fs` on every run, Jest's transform
   cache warm or cold. That cache keys a component by its own text, so Jest alone passes a stale
   component after a change to its `src=` script; hushrun runs that test file, and it fails.
6. **No build blocks.** A Vue project's compiles are `vue-tsc` and `vite build`:
   - `vue-tsc --noEmit` is a type check: it writes nothing a later build reads, and a whole check
     already skips when the same code passed (C-9). Its incremental form, `vue-tsc --build` with
     `.tsbuildinfo` files, is TypeScript's project references: if blocks for `tsc --build` are
     made, `vue-tsc` should share them, as a follow-up of that work, not here.
   - `vite build` compiles each component inside one bundler process, with no compiler process
     per file for a wrapper to stand in for and no cache protocol for hushrun to serve, unlike
     Gradle (ADR 0104) or Xcode (ADR 0105). Its cost is the bundle, not the SFC compile. Its output
     is one result of the whole run.

   B-120 to B-129 are reserved for Vue and unused.

## Consequences

- A change to one component runs the test files that mounted it, and the files its compile read
  are followed: a test whose component's props come from a changed type runs, and fails.
- Vitest checks in `jsdom` and `happy-dom` work under hushrun at all.
- A component a test file imports directly is read by each of its tests, as a module a test file
  imports directly is (C-40 rule 3).
- Known misses. `@vitejs/plugin-vue-jsx` (`vite:vue-jsx`) with its opt-in `resolveType` and an
  `fs` of the user's resolves imported types in `.tsx` files the same way, unseen; a `.tsx` was a
  source already, so this is no new miss, and it stays one. A file type resolution looked for and
  didn't find (a `.d.ts` that would shadow) is no read. A `tsconfig.json` that steers resolution
  is rest, so its change runs everything.
- Nuxt's `@nuxt/test-utils` runs tests in an environment of its own, with modules made from no
  file (`#imports`, its virtual modules): its test files are `not observed` and always run.
- Vue 2 (`@vitejs/plugin-vue2`, `vite:vue2`) is no Vitest plugin hushrun knows by name beyond its
  `vite:` prefix; Vue 2.7's compiler resolves no imported type, so its components are read as
  Vue 3's are. Not tried.

## Alternatives considered

- **Parse the component's imports in hushrun's plugin:** it would follow `import type` but not a
  type's own imports and `extends` the way @vue/compiler-sfc does; the compiler's `deps` is what it
  read.
- **Wrap `fs` in Vitest's main process while a component compiles:** transforms run concurrently,
  so a read can't be told to one component, and compiler-sfc caches a file's types across
  components.
- **Keep `.vue` rest:** sound, and reuse does nothing for a Vue project's components.
- **A rule of its own for compiled-from edges:** C-40 rule 5 already follows a file to what it is
  made of without its code running; a component's types are that.
