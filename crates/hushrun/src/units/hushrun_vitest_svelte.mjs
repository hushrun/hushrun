// [ADR 0109] What hushrun's Vitest plugin knows of Svelte: vite-plugin-svelte's plugins and
// SvelteKit's, and what each makes a module from beyond the module's own file.
//
// - vite-plugin-svelte compiles each `.svelte` file, and each runes module (`.svelte.js`,
//   `.svelte.ts`), from that file alone, but for its preprocessors (`<style lang="scss">`, a
//   markup preprocessor), which may read other files and say so in their `dependencies`: each is
//   wrapped to keep those, and the plugin hands them to the worker beside the module, as a read of
//   whoever runs it.
// - The Svelte config it loads (`svelte.config.js` and the files it imports by a relative path)
//   is a config file: every test file reads it, as Vite's own config.
// - SvelteKit's plugins make no module from other files in a Vitest run; the env modules it
//   generates below its `outDir` (`.svelte-kit`) come from files that are config files here
//   (`envFiles`, below).
//
// Every function here is called under the plugin's guard: an error of its own makes the run's test
// files `not observed`.

import fs from 'node:fs';
import path from 'node:path';

// vite-plugin-svelte's plugins (v3 to v7), SvelteKit's (v2 and v3) and Testing Library's, by name. SvelteKit 2's
// `vite-plugin-sveltekit-virtual-modules` makes `$env/static/*` and `__sveltekit/*` from no one
// file: those modules are virtual, which the adapter already counts as `not observed`.
const KNOWN = new Set([
  'vite-plugin-svelte',
  'vite-plugin-svelte:config',
  'vite-plugin-svelte:setup-optimizer',
  'vite-plugin-svelte:load-compiled-css',
  'vite-plugin-svelte:load-custom',
  'vite-plugin-svelte:preprocess',
  'vite-plugin-svelte:compile',
  'vite-plugin-svelte:compile-module',
  'vite-plugin-svelte:hot-update',
  'vite-plugin-svelte:optimize',
  'vite-plugin-svelte:optimize-module',
  'vite-plugin-svelte-module',
  'vite-plugin-svelte-inspector',
  'vite-plugin-sveltekit-resolve-root',
  'vite-plugin-sveltekit-setup',
  'vite-plugin-sveltekit-env-vars',
  'vite-plugin-sveltekit-guard',
  'vite-plugin-sveltekit-remote',
  'vite-plugin-sveltekit-remote-guard',
  'vite-plugin-sveltekit-service-worker',
  'vite-plugin-sveltekit-service-worker-env',
  'vite-plugin-sveltekit-compile',
  'vite-plugin-sveltekit-adapter',
  'vite-plugin-sveltekit-build',
  'vite-plugin-sveltekit-virtual-modules',
  // @testing-library/svelte's `svelteTesting()`: config alone (Svelte's browser build, its cleanup
  // as a setup file, itself inlined).
  'vite-plugin-svelte-testing-library',
]);

export function known(name) {
  return KNOWN.has(name);
}

// What preprocessing read, by the file preprocessed: `dependencies` of each preprocessor's result.
const read = new Map();
const wrapped = new WeakSet();

function keep(filename, result) {
  if (!filename || !result || !Array.isArray(result.dependencies)) return;
  const deps = read.get(filename) ?? new Set();
  for (const dep of result.dependencies) {
    if (typeof dep === 'string' && dep) deps.add(path.resolve(dep));
  }
  read.set(filename, deps);
}

// Wraps one preprocessor group's `markup`, `script` and `style` in place: vite-plugin-svelte keeps
// the group objects it was given, so a wrapper set after it resolved is the one it calls. A
// property the function carries (vitePreprocess's `__resolvedConfig`) stays on the function the
// wrapper calls, and is copied onto the wrapper.
function wrapGroup(group) {
  if (!group || typeof group !== 'object' || wrapped.has(group)) return;
  wrapped.add(group);
  for (const kind of ['markup', 'script', 'style']) {
    const fn = group[kind];
    if (typeof fn !== 'function') continue;
    const wrapper = async function (input, ...rest) {
      const result = await fn.call(this, input, ...rest);
      keep(input && input.filename, result);
      return result;
    };
    for (const key of Reflect.ownKeys(fn)) {
      if (key === 'length' || key === 'name' || key === 'prototype') continue;
      try {
        Object.defineProperty(wrapper, key, Object.getOwnPropertyDescriptor(fn, key));
      } catch {
        // A property it can't carry: the original keeps it.
      }
    }
    group[kind] = wrapper;
  }
}

// The relative imports of a config file, by its text: one read where there is none only widens
// what every test file reads. Followed to the end, inside `root`.
const SPECIFIER = /(?:\bfrom\s*|\bimport\s*\(\s*|\brequire\s*\(\s*|\bimport\s+)(['"])(\.{1,2}\/[^'"]+)\1/g;
const EXTENSIONS = ['', '.js', '.mjs', '.cjs', '.ts', '.mts', '.cts', '/index.js', '/index.ts'];
function configClosure(file, root) {
  const out = new Set();
  const queue = [file];
  while (queue.length) {
    const next = queue.pop();
    if (out.has(next)) continue;
    out.add(next);
    let text;
    try {
      text = fs.readFileSync(next, 'utf8');
    } catch {
      continue;
    }
    for (const m of text.matchAll(SPECIFIER)) {
      const base = path.resolve(path.dirname(next), m[2]);
      for (const ext of EXTENSIONS) {
        const candidate = base + ext;
        if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
          if (!path.relative(root, candidate).startsWith('..')) queue.push(candidate);
          break;
        }
      }
    }
  }
  return [...out];
}

const SVELTE_CONFIGS = ['svelte.config.js', 'svelte.config.mjs', 'svelte.config.ts', 'svelte.config.mts'];
const state = { kit: false };

// What a resolved project's Svelte plugins add to the run: its config files, every test file's
// reads. Wraps the project's preprocessors.
export function resolved(config) {
  const plugins = config.plugins || [];
  const configFiles = [];
  const vps = plugins.find((p) => p && p.name === 'vite-plugin-svelte:config');
  const options = vps && vps.api && vps.api.options;
  if (options) {
    if (typeof options.configFile === 'string' && options.configFile) {
      configFiles.push(...configClosure(path.resolve(options.configFile), config.root));
    }
    for (const group of [].concat(options.preprocess || [])) wrapGroup(group);
  }
  const kit = plugins.find((p) => p && p.name === 'vite-plugin-sveltekit-setup');
  if (kit) {
    state.kit = true;
    // SvelteKit 2 loads `svelte.config.js` itself, and hands vite-plugin-svelte its options
    // inline: the file is read wherever it is.
    for (const name of SVELTE_CONFIGS) {
      const file = path.join(config.root, name);
      if (fs.existsSync(file)) configFiles.push(...configClosure(file, config.root));
    }
    for (const file of envFiles(config.root, kit.api && kit.api.options)) {
      configFiles.push(...configClosure(file, config.root));
    }
  }
  return configFiles;
}

// [ADR 0109] What SvelteKit generates its env modules from (`$app/env`, `$env/*`, below its
// `outDir`, `.svelte-kit`), when its config resolves: the env entry (`src/env.*`, with what it
// imports) and the `.env` files of its env directory. A test reads the generated modules through
// SvelteKit's runtime, below node_modules, which a test's list doesn't follow: each is a config
// file instead, which every test file reads. Its config is the Vite config, which every test file
// reads already, or SvelteKit 2's `svelte.config.js`, which vite-plugin-svelte loaded.
function envFiles(root, options) {
  const kit = (options && options.kit) || options || {};
  const files = kit.files || {};
  const src = path.resolve(root, typeof files.src === 'string' ? files.src : 'src');
  const envDir = path.resolve(root, kit.env && typeof kit.env.dir === 'string' ? kit.env.dir : '.');
  const out = [];
  for (const [dir, wanted] of [
    [src, (name) => /^env\.[a-z]+$/.test(name)],
    [envDir, (name) => name === '.env' || name.startsWith('.env.')],
  ]) {
    try {
      for (const name of fs.readdirSync(dir)) if (wanted(name)) out.push(path.join(dir, name));
    } catch {
      // No such directory: nothing of it.
    }
  }
  return out;
}

// [ADR 0109] SvelteKit 2's virtual modules a test reaches through `$app/environment`, `$env/*`
// and `$app/env/*`: made from its config, the env entry and the `.env` files, which are config
// files with SvelteKit (`envFiles`), and from the variables of hushrun's own environment, as
// Vite's `import.meta.env` is. Any other virtual module (`$service-worker`, a plugin's) stays one
// no file makes, and its test file `not observed`.
const KIT_VIRTUAL = /^\0virtual:(?:env\/(?:static|dynamic)\/(?:public|private)|__sveltekit\/(?:env|env\/public\/(?:client|server)|env\/private|server|environment|paths))$/;
export function fromConfig(id) {
  return state.kit && KIT_VIRTUAL.test(String(id));
}

// What a module of `id` read beyond its own file: its preprocessors' dependencies.
export function dependencies(id) {
  const file = String(id).split('?')[0];
  const deps = read.get(file);
  return deps ? [...deps].filter((d) => d !== file) : [];
}
