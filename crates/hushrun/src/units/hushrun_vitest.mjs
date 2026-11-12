// [ADR 0071] hushrun's Vitest adapter: the Vite plugin a repository's Vitest config loads with
//
//   plugins: [process.env.HUSHRUN_VITEST && import(process.env.HUSHRUN_VITEST).then((m) => m.default())]
//
// which `hushrun init` proposes. Without HUSHRUN_VITEST (hushrun sets it only for a check whose
// `tests` aren't off), the line adds nothing. With it, and HUSHRUN_UNITS naming the run's
// directory (C-32), this plugin, in Vitest's own process:
//
// - adds hushrun's setup file first to each project's setupFiles: in each worker, it records what
//   the test file read (the modules Vite made for it, through the line appended below, the files
//   it opened, the directories it listed) and what it couldn't follow (a process, a worker thread,
//   a native addon);
// - appends to every module Vite makes, outside node_modules, one statement that records its id
//   when the module runs, after the module's own code, so that no line moves;
// - wraps the sequencer: after Vitest has chosen the test files (its filters, --shard), it removes
//   those hushrun.json lists, when Vitest's own list of test files is the one hushrun planned from,
//   and only then. When that leaves none, the run passes as Vitest's "no test files" would not:
//   it is the run of those files, which passed;
// - adds a reporter that writes, when the run ends, how each test file ended.
//
// Every hook is guarded: an error of its own is written as the adapter's, which makes the run's
// test files `not observed`, and never reaches Vitest. Only the first Vitest process that starts
// with the run's directory claims it; any other (nested) adds nothing.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCHEMA = 'hushrun.unit-facts/v0';
const RUN_SCHEMA = 'hushrun.units/v0';
const SETUP = path.join(path.dirname(fileURLToPath(import.meta.url)), 'hushrun-vitest-setup.mjs');
const STATE = Symbol.for('hushrun.vitest.main');

// Plugins known to make each module from that module's own file: Vite's and Vitest's own, and
// hushrun's. Any other may make a module from other files, which no test file's reads would hold.
const KNOWN_PREFIXES = ['vite:', 'vitest', 'builtin:'];
const KNOWN_NAMES = new Set(['alias', 'hushrun', 'vite-tsconfig-paths']);

function noop() {
  return { name: 'hushrun' };
}

function writeWhole(file, value) {
  const temp = path.join(path.dirname(file), `.${path.basename(file)}.${process.pid}.tmp`);
  fs.writeFileSync(temp, JSON.stringify(value));
  fs.renameSync(temp, file);
}

function readRun(dir) {
  try {
    const run = JSON.parse(fs.readFileSync(path.join(dir, 'hushrun.json'), 'utf8'));
    return run && run.schema === RUN_SCHEMA ? run : null;
  } catch {
    return null;
  }
}

// The run's directory for this process, once claimed: the first Vitest that starts with it.
function claim(dir) {
  const file = path.join(dir, 'vitest-claim');
  try {
    const fd = fs.openSync(file, 'wx');
    fs.writeSync(fd, String(process.pid));
    fs.closeSync(fd);
    return true;
  } catch {
    try {
      return fs.readFileSync(file, 'utf8').trim() === String(process.pid);
    } catch {
      return false;
    }
  }
}

function realOrSelf(p) {
  try {
    return fs.realpathSync(p);
  } catch {
    return p;
  }
}

function relativeTo(root, file) {
  const rel = path.relative(root, realOrSelf(file));
  if (!rel || rel.startsWith('..') || path.isAbsolute(rel)) return null;
  return rel.split(path.sep).join('/');
}

export default function hushrun() {
  const dir = process.env.HUSHRUN_UNITS;
  if (!dir || !fs.existsSync(dir) || !claim(dir)) return noop();
  const state = (globalThis[STATE] ??= {
    dir,
    errors: [],
    whole: [],
    projects: new Map(),
    configFiles: new Set(),
    removed: new Set(),
    refused: null,
    installed: false,
    testFiles: null,
  });
  const guard = (what, fn) => {
    try {
      return fn();
    } catch (err) {
      state.errors.push(`${what}: ${err && err.message ? err.message : String(err)}`.slice(0, 500));
      return undefined;
    }
  };
  return {
    name: 'hushrun',
    enforce: 'post',
    config(config) {
      guard('config', () => {
        config.test ??= {};
        const setup = config.test.setupFiles == null ? [] : [].concat(config.test.setupFiles);
        if (!setup.includes(SETUP)) config.test.setupFiles = [SETUP, ...setup];
      });
    },
    configResolved(resolved) {
      guard('configResolved', () => {
        for (const file of [resolved.configFile, ...(resolved.configFileDependencies || [])]) {
          if (file) state.configFiles.add(path.resolve(file));
        }
        const unknown = resolved.plugins
          .map((p) => p.name)
          .filter((n) => !KNOWN_NAMES.has(n) && !KNOWN_PREFIXES.some((p) => n.startsWith(p)));
        state.projects.set(resolved.root, { plugins: unknown });
      });
    },
    async transform(code, id) {
      if (id.includes('/node_modules/') || id.includes('\\node_modules\\')) return null;
      // [ADR 0081, amended] The files it re-exports from (`export * from`, `export { x } from`):
      // Vite runs `export *` through getters of its own, so no function of a barrel runs when a
      // test reads through it, and a test's list follows these instead, to the end.
      let reexports = [];
      try {
        reexports = await reexported(this, code, id);
      } catch (err) {
        state.errors.push(`reexports: ${err && err.message ? err.message : String(err)}`.slice(0, 500));
      }
      return guard('transform', () => {
        const told = reexports.length
          ? `;(globalThis.__hushrun_vitest_reexports ||= {})[${JSON.stringify(id)}] = ${JSON.stringify(reexports)};`
          : '';
        return {
          code: `${code}\n;(globalThis.__hushrun_vitest_seen ||= []).push(${JSON.stringify(id)});${told}`,
          map: null,
        };
      });
    },
    configureVitest({ vitest, project }) {
      guard('configureVitest', () => configure(state, vitest, project, guard));
    },
  };
}

// [ADR 0081, amended] The specifiers a module re-exports from, by its syntax tree when it parses
// and by its text always: a re-export read where there is none only widens a test's list.
const REEXPORT = /export\s*(?:type\s+)?(?:\*(?:\s*as\s+[\w$]+)?|\{[^}]*\})\s*from\s*(['"])([^'"]+)\1/g;
async function reexported(context, code, id) {
  const specifiers = new Set();
  for (const m of code.matchAll(REEXPORT)) specifiers.add(m[2]);
  try {
    const ast = context.parse(code);
    for (const node of ast.body || []) {
      if ((node.type === 'ExportAllDeclaration' || node.type === 'ExportNamedDeclaration')
        && node.source && typeof node.source.value === 'string') {
        specifiers.add(node.source.value);
      }
    }
  } catch {
    // Text the parser doesn't take: what its text says stands.
  }
  const out = [];
  for (const specifier of specifiers) {
    // One that doesn't resolve is text that only looks like a re-export (a string, a comment):
    // a real one would fail the module's own import.
    let resolved = null;
    try {
      resolved = await context.resolve(specifier, id);
    } catch {
      resolved = null;
    }
    const file = resolved && typeof resolved.id === 'string' ? resolved.id.split('?')[0] : null;
    if (file && path.isAbsolute(file) && !out.includes(file)) out.push(file);
  }
  return out;
}

// What a project's configuration keeps from being observed, each as `{why, detail}`.
function projectIssues(state, project) {
  const config = project.config;
  const issues = [];
  const pool = config.pool;
  if (pool !== 'forks' && pool !== 'threads') issues.push({ why: 'pool', detail: String(pool) });
  if (config.isolate === false) issues.push({ why: 'isolate', detail: 'isolate: false' });
  if (config.browser && config.browser.enabled) issues.push({ why: 'browser', detail: 'browser' });
  if (config.typecheck && config.typecheck.enabled) {
    issues.push({ why: 'typecheck', detail: 'typecheck' });
  }
  const globalSetup = [].concat(config.globalSetup || []);
  if (globalSetup.length) issues.push({ why: 'global-setup', detail: String(globalSetup[0]) });
  const env = String(config.environment || 'node');
  if (env.startsWith('.') || path.isAbsolute(env)) issues.push({ why: 'environment', detail: env });
  if (config.runner) issues.push({ why: 'runner', detail: String(config.runner) });
  const css = config.css;
  if (css && typeof css === 'object' && [].concat(css.include || []).length) {
    issues.push({ why: 'css', detail: 'css.include' });
  }
  const seen = state.projects.get(config.root) || state.projects.get(project.vite?.config?.root);
  for (const name of seen ? seen.plugins : ['(a project hushrun could not read)']) {
    issues.push({ why: 'plugin', detail: name });
  }
  return issues;
}

function configure(state, vitest, project, guard) {
  state.vitest = vitest;
  state.issues ??= new Map();
  state.issues.set(project.name || '', projectIssues(state, project));
  if (state.installed) return;
  state.installed = true;
  const major = Number(String(vitest.version || '').split('.')[0]);
  if (major !== 4) {
    state.errors.push(`this Vitest, ${vitest.version}, isn't one hushrun's adapter knows (4)`);
  }
  if (vitest.config.watch) {
    state.errors.push('Vitest watches files in this run');
    return;
  }
  const Sequencer = vitest.config.sequence.sequencer;
  class HushrunSequencer extends Sequencer {
    async sort(files) {
      const sorted = await super.sort(files);
      const kept = await guardAsync(state, 'sort', () => deselect(state, vitest, sorted));
      return kept ?? sorted;
    }
  }
  vitest.config.sequence.sequencer = HushrunSequencer;
  vitest.config.reporters.push({
    async onTestRunEnd(modules, errors, reason) {
      if (state.testFiles == null) {
        state.testFiles = await guardAsync(state, 'report', () => allTestFiles(vitest));
      }
      guard('report', () => report(state, vitest, modules, errors, reason));
    },
  });
}

async function guardAsync(state, what, fn) {
  try {
    return await fn();
  } catch (err) {
    state.errors.push(`${what}: ${err && err.message ? err.message : String(err)}`.slice(0, 500));
    return undefined;
  }
}

// Every test file of every project, by Vitest's own include and exclude, with no filter.
async function allTestFiles(vitest) {
  const files = new Set();
  for (const project of vitest.projects) {
    const found = await project.globTestFiles();
    for (const file of found.testFiles) files.add(path.resolve(file));
  }
  return files;
}

async function deselect(state, vitest, sorted) {
  state.testFiles = await allTestFiles(vitest);
  const run = readRun(state.dir);
  if (!run) return sorted;
  const root = realOrSelf(run.root);
  const coverage = Boolean(vitest.config.coverage && vitest.config.coverage.enabled);
  const mine = [...state.testFiles].map((f) => relativeTo(root, f)).filter(Boolean).sort();
  const planned = [...(run.testFiles || [])].sort();
  const same = mine.length === planned.length && mine.every((f, i) => f === planned[i]);
  const refused = coverage
    ? "Vitest's coverage is on, and a partial run's coverage would read as a drop"
    : !same ? "Vitest's test files aren't the ones hushrun planned from" : null;
  // [ADR 0081] Tests one by one, handed to the workers before any starts: the tests they may
  // leave out only where test files may be, and watching each test only without Vitest's own
  // coverage, which shares V8's counters.
  if (run.unit === 'test') {
    const reuseTests = refused === null && run.reuseTests && typeof run.reuseTests === 'object'
      ? run.reuseTests : {};
    if (refused !== null && run.reuseTests && Object.keys(run.reuseTests).length) {
      state.refused = refused;
    }
    if (run.reads === 'test' && coverage) {
      state.watch = "Vitest's coverage is on, and it shares V8's counters with watching a test";
    }
    writeWhole(path.join(state.dir, 'vitest-tests'), {
      unit: 'test',
      watch: run.reads === 'test' && !coverage,
      root,
      reuseTests,
      configFiles: [...state.configFiles],
    });
  }
  if (!Array.isArray(run.reuse) || run.reuse.length === 0) return sorted;
  if (refused !== null) {
    state.refused = refused;
    return sorted;
  }
  const reuse = new Set(run.reuse);
  const kept = [];
  for (const spec of sorted) {
    const unit = relativeTo(root, spec.moduleId);
    if (unit && reuse.has(unit)) {
      state.removed.add(path.resolve(spec.moduleId));
    } else {
      kept.push(spec);
    }
  }
  // Every test file left was reused: what Vitest would say of no test files isn't this run's.
  if (kept.length === 0 && sorted.length > 0) vitest.config.passWithNoTests = true;
  return kept;
}

function report(state, vitest, modules, errors, reason) {
  const units = new Map();
  for (const module of modules) {
    const unit = path.resolve(module.moduleId);
    const tests = [...module.children.allTests()];
    const states = tests.map((t) => t.result().state);
    const moduleState = module.state();
    const done = moduleState === 'passed' || moduleState === 'failed' || moduleState === 'skipped';
    let selected = states.filter((s) => s !== 'skipped').length;
    const finished = states.filter((s) => s === 'passed' || s === 'failed').length;
    if (!done) selected = Math.max(selected, finished + 1);
    const before = units.get(unit) || {
      unit,
      selected: 0,
      finished: 0,
      failed: false,
      deselected: false,
      durationMs: 0,
      reads: [],
      notFollowed: [],
      dirs: [],
    };
    before.selected += selected;
    before.finished += finished;
    before.failed ||= moduleState === 'failed' || !module.ok();
    before.durationMs += Math.round(module.diagnostic().duration || 0);
    const issues = state.issues.get(module.project.name || '') || [];
    before.notFollowed.push(...issues);
    units.set(unit, before);
  }
  for (const unit of state.removed) {
    if (!units.has(unit)) {
      units.set(unit, {
        unit,
        selected: 0,
        finished: 0,
        failed: false,
        deselected: true,
        durationMs: 0,
        reads: [],
        notFollowed: [],
        dirs: [],
      });
    }
  }
  const bail = Number(vitest.config.bail || 0) > 0;
  writeWhole(path.join(state.dir, `vitest-${process.pid}.json`), {
    edges: importEdges(vitest),
    schema: SCHEMA,
    adapter: 'vitest',
    failuresByUnit: errors.length === 0 && reason !== 'interrupted' && !(bail && reason !== 'passed'),
    settings: {
      testFiles: [...(state.testFiles || [])],
      configFiles: [...state.configFiles],
      version: String(vitest.version || ''),
      refused: state.refused,
      // [ADR 0081] Why no test was watched, when `reads: test` asked and none could be.
      ...(state.watch ? { watch: state.watch } : {}),
    },
    notFollowed: state.whole,
    units: [...units.values()],
    errors: state.errors,
    removedItems: state.removed.size,
  });
}

// [ADR 0072] Which module imports which, from each project's Vite module graph, for `hushrun why`
// to show a path: `[from, to, 'import']`, absolute paths, outside node_modules. Explanations only:
// nothing keys by them, so what can't be read here is left out and costs only the path.
function importEdges(vitest) {
  const edges = new Map();
  const own = (file) => typeof file === 'string' && path.isAbsolute(file) && !/[\\/]node_modules[\\/]/.test(file);
  try {
    for (const project of vitest.projects || []) {
      const server = project.vite;
      if (!server) continue;
      const graphs = [];
      for (const environment of Object.values(server.environments || {})) {
        if (environment && environment.moduleGraph) graphs.push(environment.moduleGraph);
      }
      if (graphs.length === 0 && server.moduleGraph) graphs.push(server.moduleGraph);
      for (const graph of graphs) {
        const nodes = graph.idToModuleMap ? graph.idToModuleMap.values() : [];
        for (const node of nodes) {
          if (!own(node.file)) continue;
          for (const imported of node.importedModules || []) {
            if (!own(imported.file) || imported.file === node.file) continue;
            const key = `${node.file}\0${imported.file}`;
            edges.set(key, [node.file, imported.file, 'import']);
          }
        }
      }
    }
  } catch {
    return [...edges.values()];
  }
  return [...edges.values()];
}
