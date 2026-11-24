// [ADR 0108] Vue's single-file components, for hushrun's Vitest plugin (hushrun_vitest.mjs), in
// Vitest's own process.
//
// @vitejs/plugin-vue (`vite:vue`) makes each `.vue` file's modules from that file and from the
// files its blocks name with `src=`, which are modules of their own, so the worker sees them run.
// One read is not: to compile `defineProps<Props>()`, `defineEmits<E>()` and the other macros
// typed with an imported type, @vue/compiler-sfc reads the file that declares the type, and no
// code of that file ever runs (a type has none). What the compile made from it (the props, their
// runtime types, the emits) is still the component's behaviour.
//
// So this wraps the compiler the plugin holds (`api.options.compiler`): each `compileScript` says,
// in its result's `deps`, the files its types were resolved from, followed to the end, and this
// keeps them by component. The plugin then appends them to the component's module, as it appends
// re-exports, and a test that reads the component reads them too.
//
// A `vite:vue` this can't wrap (no `api`, no compiler, no `compileScript`) is a plugin hushrun
// doesn't know, and its project is `not observed`.

import path from 'node:path';

const WRAPPED = Symbol.for('hushrun.vitest.vue.compiler');

// Wraps every `vite:vue` of a resolved config's plugins; returns the names of those it couldn't.
export function wrap(plugins, state) {
  const unwrapped = [];
  for (const plugin of plugins || []) {
    if (!plugin || plugin.name !== 'vite:vue') continue;
    const api = plugin.api;
    const options = api && api.options;
    const compiler = options && options.compiler;
    if (!compiler || typeof compiler.compileScript !== 'function') {
      unwrapped.push('vite:vue (a compiler hushrun could not wrap)');
      continue;
    }
    if (compiler[WRAPPED]) continue;
    const compileScript = compiler.compileScript;
    const wrapped = {
      ...compiler,
      [WRAPPED]: true,
      compileScript(descriptor, opts) {
        const result = compileScript.call(this, descriptor, opts);
        keep(state, descriptor, result);
        return result;
      },
    };
    api.options = { ...options, compiler: wrapped };
    if (api.options.compiler !== wrapped) unwrapped.push('vite:vue (a compiler hushrun could not wrap)');
  }
  return unwrapped;
}

function keep(state, descriptor, result) {
  try {
    const file = descriptor && typeof descriptor.filename === 'string' ? path.resolve(descriptor.filename) : null;
    if (!file) return;
    const deps = (state.vueDeps ??= new Map());
    const set = deps.get(file) || new Set();
    for (const dep of (result && result.deps) || []) {
      if (typeof dep === 'string' && path.isAbsolute(dep)) set.add(path.resolve(dep));
    }
    set.delete(file);
    deps.set(file, set);
  } catch (err) {
    state.errors.push(`vue: ${err && err.message ? err.message : String(err)}`.slice(0, 500));
  }
}

// The files a component's compile read besides itself, for a module id of it: `[]` for one this
// process compiled from itself alone, or for an id that is no component.
export function compiledFrom(state, id) {
  const file = String(id).split('?')[0];
  if (!file.endsWith('.vue') || !state.vueDeps) return [];
  return [...(state.vueDeps.get(path.resolve(file)) || [])].sort();
}
