// [ADR 0071] hushrun's setup file, which its Vitest plugin puts first in every project's
// setupFiles: in a worker, for the one test file it runs (hushrun observes only projects that
// isolate each test file), it records what the file read and what it couldn't follow, and writes
// that, whole, to the run's directory when the file's tests end.
//
// - Modules: every module Vite made runs one statement the plugin appended, which pushes its id
//   to `globalThis.__hushrun_vitest_seen`, from the first module of the worker on; CommonJS a test
//   requires is in `require.cache`.
// - Files: Node's `fs` and `fs/promises`, wrapped, record what is opened, read, looked at and
//   listed, for code that calls them after this file ran.
// - What it can't follow: a child process, a worker thread or a native addon.
//
// [ADR 0081] With tests one by one (`unit: test`, which the plugin hands over in `vitest-tests`
// once Vitest's own list of test files is the one hushrun planned from), it also:
//
// - names each test by its suites' names and its own, joined with ` > `, ` [2]`, ` [3]` after a
//   name the file already had, in the order the file declares them;
// - leaves out, after the file is collected and before any hook of theirs, the tests hushrun
//   lists, and a suite all of whose tests are left out, so that its `beforeAll` doesn't run;
// - says how each test ended, and what it couldn't follow inside its own window;
// - with `reads: test`, watches each test: the files a function of which was called between its
//   start and its end (V8's precise coverage, through `node:inspector`, a module's top-level code
//   apart), and the files it opened and listed; what a suite's `beforeAll` and `afterAll` did, for
//   the tests of the suite; and what loading the file did, for every test.
//
// It changes no outcome: every wrapper calls what it wraps, and an error of its own is written as
// the adapter's, which makes the file `not observed`.

import fs from 'node:fs';
import module from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import childProcess from 'node:child_process';
import workerThreads from 'node:worker_threads';
import { afterAll, expect, TestRunner } from 'vitest';

const SCHEMA = 'hushrun.unit-facts/v0';
const SCHEMA_V1 = 'hushrun.unit-facts/v1';
const STATE = Symbol.for('hushrun.vitest.worker');
// [ADR 0081] Where the plugin hands over tests one by one (no `.json`: it isn't a facts file).
const TESTS = 'vitest-tests';
// Vitest's module evaluator wraps each module in `'use strict';async (…)=>{{`: the function at
// this offset, like the script's own at 0, is the module's top-level code.
const WRAPPER_OFFSET = "'use strict';".length;

function pathOf(value) {
  try {
    if (typeof value === 'string') {
      return path.resolve(value.startsWith('file:') ? fileURLToPath(value) : value);
    }
    if (value instanceof URL) return fileURLToPath(value);
    if (Buffer.isBuffer(value)) return path.resolve(value.toString());
  } catch {
    // A value that names no path, such as a file descriptor.
  }
  return null;
}

// A module id's file, when it names one: an absolute path or a `file:` URL, without its query.
function idPath(id) {
  const text = String(id).split('?')[0];
  try {
    if (text.startsWith('file:')) return fileURLToPath(text);
  } catch {
    return null;
  }
  return path.isAbsolute(text) ? text : null;
}

// This file, which is hushrun's and no test's.
const SELF = fileURLToPath(import.meta.url);

function message(err) {
  return err && err.message ? err.message : String(err);
}

function install() {
  const state = {
    reads: new Set(),
    dirs: new Set(),
    notFollowed: [],
    unattributed: [],
    errors: [],
    written: 0,
    // [ADR 0081] The test file being run, when tests are told one by one.
    file: null,
  };
  const note = (what, fn) => {
    try {
      fn();
    } catch (err) {
      state.errors.push(`${what}: ${message(err)}`.slice(0, 500));
    }
  };
  // [ADR 0081] What is happening now, for the test, suite or loading it is part of.
  const bucket = () => (state.file && state.file.current) || null;
  const read = (value) => note('read', () => {
    const p = pathOf(value);
    if (p) {
      state.reads.add(p);
      bucket()?.opened.add(p);
      if (state.file && state.file.watch) keptBy(state.file, p);
    }
  });
  const listed = (value) => note('list', () => {
    const p = pathOf(value);
    if (p) {
      state.dirs.add(p);
      bucket()?.dirs.add(p);
    }
  });
  // Every one is the file's (ADR 0071); with tests one by one, also the test's, suite's or
  // loading's it happened in, else the file's alone.
  const unfollowed = (entry) => {
    state.notFollowed.push(entry);
    const now = bucket();
    if (now) now.notFollowed.push(entry);
    else state.unattributed.push(entry);
  };
  const wrap = (target, name, record) => {
    const original = target[name];
    if (typeof original !== 'function') return;
    const wrapped = function (...args) {
      record(args[0], args);
      return original.apply(this, args);
    };
    Object.defineProperty(wrapped, 'name', { value: original.name });
    target[name] = wrapped;
  };
  for (const target of [fs, fs.promises]) {
    for (const name of ['readFile', 'readFileSync', 'open', 'openSync', 'createReadStream',
      'stat', 'statSync', 'lstat', 'lstatSync', 'access', 'accessSync', 'existsSync',
      'realpath', 'realpathSync', 'copyFile', 'copyFileSync', 'cp', 'cpSync']) {
      wrap(target, name, read);
    }
    for (const name of ['readdir', 'readdirSync', 'opendir', 'opendirSync']) {
      wrap(target, name, listed);
    }
  }
  const spawned = (what) => (command) => {
    unfollowed({ why: 'subprocess', detail: `${what} ${String(command)}`.slice(0, 200) });
  };
  for (const name of ['spawn', 'spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync',
    'fork']) {
    wrap(childProcess, name, spawned(name));
  }
  const Worker = workerThreads.Worker;
  if (typeof Worker === 'function') {
    workerThreads.Worker = class extends Worker {
      constructor(...args) {
        unfollowed({ why: 'worker', detail: String(args[0]).slice(0, 200) });
        super(...args);
      }
    };
  }
  const dlopen = process.dlopen;
  process.dlopen = function (...args) {
    unfollowed({ why: 'native', detail: String(args[1]).slice(0, 200) });
    return dlopen.apply(this, args);
  };
  module.syncBuiltinESMExports();
  return state;
}

const state = (globalThis[STATE] ??= install());
const dir = process.env.HUSHRUN_UNITS;
let file = null;
try {
  file = expect.getState().testPath;
} catch (err) {
  state.errors.push(`testPath: ${message(err)}`);
}

// The facts of a test file as ADR 0071 has them: what it read, all of it, and what it couldn't
// follow, all of it.
function wholeFile() {
  const reads = new Set(state.reads);
  for (const id of globalThis.__hushrun_vitest_seen || []) reads.add(String(id));
  const require = module.createRequire(import.meta.url);
  for (const id of Object.keys(require.cache || {})) reads.add(id);
  return {
    unit: path.resolve(file),
    reads: [...reads],
    dirs: [...state.dirs],
    notFollowed: [...state.notFollowed],
  };
}

function writeFacts(value) {
  state.written += 1;
  const name = `vitest-worker-${process.pid}-${workerThreads.threadId}-${state.written}.json`;
  const target = path.join(dir, name);
  const temp = path.join(dir, `.${name}.tmp`);
  fs.writeFileSync(temp, JSON.stringify(value));
  fs.renameSync(temp, target);
}

afterAll(() => {
  if (!dir || typeof file !== 'string') return;
  // [ADR 0081] Tests one by one are written when the run's files end, after every hook.
  if (state.file && state.file.live) return;
  try {
    writeFacts({ schema: SCHEMA, adapter: 'vitest-worker', units: [wholeFile()], errors: state.errors });
  } catch {
    // Nothing written: hushrun then knows nothing of this file's reads, and it is not observed.
  }
});

// ---------------------------------------------------------------------------------------------
// [ADR 0081] Tests one by one.

function readTests() {
  if (!dir) return null;
  try {
    const told = JSON.parse(fs.readFileSync(path.join(dir, TESTS), 'utf8'));
    return told && told.unit === 'test' ? told : null;
  } catch {
    return null;
  }
}

function newBucket() {
  return { ran: new Set(), opened: new Set(), dirs: new Set(), notFollowed: [] };
}

function startFile(told) {
  const root = told.root;
  const unit = path.resolve(file);
  const testFile = TestRunner.getCurrentSuite().file;
  const declared = new WeakMap();
  // The modes the file declares, before Vitest's filters (`.only`, `-t`, a line) change them: a
  // test the file skips passed, as a file of skipped tests does; one a filter left out isn't told.
  if (testFile && Array.isArray(testFile.tasks)) {
    const push = testFile.tasks.push;
    const remember = (task) => {
      if (!task || typeof task !== 'object') return;
      declared.set(task, task.mode);
      if (Array.isArray(task.tasks)) task.tasks.forEach(remember);
    };
    Object.defineProperty(testFile.tasks, 'push', {
      configurable: true,
      enumerable: false,
      writable: true,
      value(...items) {
        try {
          items.forEach(remember);
        } catch {
          // Without them no skipped test is told, which only runs more.
        }
        return push.apply(this, items);
      },
    });
  }
  state.file = {
    unit,
    root,
    testFile,
    declared,
    watch: told.watch === true,
    reuse: new Set(Array.isArray(told.reuseTests?.[relativeTo(root, unit)])
      ? told.reuseTests[relativeTo(root, unit)] : []),
    // What Vitest loads for every test of the file: its config files and this project's setup
    // files, as files whose code ran.
    every: [...(told.configFiles || []),
      ...[].concat(globalThis.__vitest_worker__?.config?.setupFiles || [])]
      .filter((p) => typeof p === 'string' && p !== SELF),
    live: false,
    concurrent: false,
    loaded: newBucket(),
    after: newBucket(),
    shared: new Map(),
    tests: new Map(),
    stack: [],
    // The tests whose window is open: more than one at once is two tests running together.
    open: new Set(),
    lastWasTestEnd: false,
    current: null,
    kinds: new Map(),
    ids: new Map(),
    finished: new Set(),
    // Each module whose code was on the stack when a file was opened, with the files: a file a
    // module read once and kept reaches every test that runs the module's code.
    kept: new Map(),
    shared_ids: new Set(),
  };
  state.file.current = state.file.loaded;
  // Without V8's coverage no test says what it ran, and each gets its file's list.
  if (state.file.watch) {
    try {
      state.file.watch = coverage() !== null;
    } catch {
      state.file.watch = false;
    }
  }
  hooks();
}

// The paths from the repository root that `p` names, `/`-separated; null outside it.
const real = new Map();
function relativeTo(root, p) {
  if (typeof p !== 'string' || !root) return null;
  let resolved = real.get(p);
  if (resolved === undefined) {
    try {
      resolved = fs.realpathSync.native(p);
    } catch {
      resolved = p;
    }
    real.set(p, resolved);
  }
  const rel = path.relative(root, resolved);
  if (!rel || rel.startsWith('..') || path.isAbsolute(rel)) return null;
  const parts = rel.split(path.sep);
  return parts.includes('node_modules') ? null : parts.join('/');
}

// ---- V8's precise coverage, one session for this worker.

function coverage() {
  if (state.inspector !== undefined) return state.inspector;
  state.inspector = null;
  const inspector = module.createRequire(import.meta.url)('node:inspector');
  const session = new inspector.Session();
  session.connect();
  let failed = null;
  session.post('Profiler.enable', (err) => { failed ||= err; });
  session.post('Profiler.startPreciseCoverage', { callCount: true, detailed: false }, (err) => {
    failed ||= err;
  });
  if (failed) throw failed;
  state.inspector = session;
  return session;
}

const scripts = new Map();
// The files a function of which was called since the last take, a module's top-level code apart.
function take() {
  const session = state.inspector;
  if (!session) return [];
  let result = null;
  let error = null;
  session.post('Profiler.takePreciseCoverage', (err, out) => {
    error = err;
    result = out;
  });
  if (error) throw error;
  if (!result) throw new Error("V8's coverage didn't answer at once");
  const evaluated = globalThis.__vitest_worker__?.evaluatedModules;
  const files = [];
  for (const script of result.result) {
    let known = scripts.get(script.url);
    if (known === undefined) {
      known = null;
      if (script.url && !script.url.startsWith('node:')) {
        const p = idPath(script.url);
        if (p && p !== SELF && !/[\\/]node_modules[\\/]/.test(p)) {
          const lookup = (id) => Boolean(evaluated && typeof evaluated.getModuleById === 'function'
            && evaluated.getModuleById(id));
          known = { path: p, byVitest: lookup(script.url) || lookup(p) };
        }
      }
      scripts.set(script.url, known);
    }
    if (!known) continue;
    for (const fn of script.functions) {
      const range = fn.ranges[0];
      if (!range || range.count === 0) continue;
      if (range.startOffset === 0) continue;
      if (known.byVitest && fn.functionName === '' && range.startOffset === WRAPPER_OFFSET) continue;
      files.push(known.path);
      break;
    }
  }
  return files;
}

// What ran since the last boundary goes to what is happening now. A take that fails stops
// watching: no test of the file then says what it ran, and each gets its file's list.
function flush(F) {
  if (!F.watch) return;
  try {
    for (const p of take()) F.current.ran.add(p);
  } catch {
    F.watch = false;
  }
}

// ---- Vitest's runner, at a test's and a suite's boundaries.

function hooks() {
  if (state.hooked) return;
  state.hooked = true;
  const proto = TestRunner.prototype;
  const guard = (what, fn) => {
    try {
      fn();
    } catch (err) {
      state.errors.push(`${what}: ${message(err)}`.slice(0, 500));
    }
  };
  const mine = (task) => {
    const F = state.file;
    return F && task && task.file && path.resolve(task.file.filepath) === F.unit ? F : null;
  };
  const onBeforeRunSuite = proto.onBeforeRunSuite;
  proto.onBeforeRunSuite = async function (suite, ...rest) {
    let before = null;
    guard('suite start', () => {
      const F = mine(suite);
      if (!F) return;
      settle(F);
      flush(F);
      if (suite === suite.file) {
        F.live = true;
        own(this);
        name(F);
        choose(F);
      } else if (F.stack[F.stack.length - 1] !== (suite.suite || suite.file) || F.open.size) {
        F.concurrent = true;
      }
      before = F.current;
      F.stack.push(suite);
      F.current = sharedOf(F, suite);
      F.lastWasTestEnd = false;
    });
    const out = await onBeforeRunSuite?.call(this, suite, ...rest);
    guard('suite start', () => {
      const F = mine(suite);
      // A suite that won't run (skipped, or a file that failed to load) has no end to wait for.
      const runs = (suite.mode === 'run' || suite.mode === 'queued')
        && !(suite.result && suite.result.state === 'fail');
      if (!F || before === null || runs) return;
      const at = F.stack.lastIndexOf(suite);
      if (at >= 0) F.stack.splice(at, 1);
      F.current = before;
    });
    return out;
  };
  const onAfterRunSuite = proto.onAfterRunSuite;
  proto.onAfterRunSuite = async function (suite, ...rest) {
    const out = await onAfterRunSuite?.call(this, suite, ...rest);
    guard('suite end', () => {
      const F = mine(suite);
      if (!F) return;
      settle(F);
      flush(F);
      if (F.stack[F.stack.length - 1] !== suite || F.open.size) F.concurrent = true;
      const at = F.stack.lastIndexOf(suite);
      if (at >= 0) F.stack.splice(at, 1);
      const top = F.stack[F.stack.length - 1];
      F.current = top ? sharedOf(F, top) : F.after;
      F.lastWasTestEnd = false;
    });
    return out;
  };
  const onBeforeRunTask = proto.onBeforeRunTask;
  proto.onBeforeRunTask = async function (test, ...rest) {
    const out = await onBeforeRunTask?.call(this, test, ...rest);
    guard('test start', () => {
      const F = mine(test);
      if (!F) return;
      settle(F);
      if (test.mode !== 'run' && test.mode !== 'queued') return;
      // Between one test's end and the next one's start only Vitest's code runs: one take less.
      if (!F.lastWasTestEnd) flush(F);
      if (F.open.size || F.stack[F.stack.length - 1] !== (test.suite || test.file)
        || test.concurrent) {
        F.concurrent = true;
      }
      F.open.add(test);
      F.current = testOf(F, test);
      F.lastWasTestEnd = false;
    });
    return out;
  };
}

// The runner's own methods Vitest wrapped when it made it: a test's end, after its hooks, its
// fixtures and its retries, and the end of the run's files, when the facts are written.
function own(runner) {
  if (runner.__hushrun_tests) return;
  Object.defineProperty(runner, '__hushrun_tests', { value: true });
  const onAfterRunTask = runner.onAfterRunTask;
  runner.onAfterRunTask = async function (test, ...rest) {
    try {
      const F = state.file;
      if (F && F.open.has(test)) {
        flush(F);
        F.finished.add(test);
        F.open.delete(test);
        const top = F.stack[F.stack.length - 1];
        F.current = top ? sharedOf(F, top) : F.after;
        F.lastWasTestEnd = true;
      }
    } catch (err) {
      state.errors.push(`test end: ${message(err)}`.slice(0, 500));
    }
    return onAfterRunTask?.call(this, test, ...rest);
  };
  const onAfterRunFiles = runner.onAfterRunFiles;
  runner.onAfterRunFiles = async function (...args) {
    try {
      const F = state.file;
      if (F && F.live && !F.written) {
        F.written = true;
        settle(F);
        flush(F);
        writeFacts(testsFacts(F));
      }
    } catch {
      // Nothing written: hushrun knows nothing of this file, and it is not observed.
    }
    return onAfterRunFiles?.call(this, ...args);
  };
}

// A test that skipped itself (`ctx.skip()`) ends with no `onAfterRunTask`: its window closes at
// the next boundary, with what ran since.
function settle(F) {
  for (const test of F.open) {
    if (!test.result || test.result.state === 'run') continue;
    flush(F);
    F.finished.add(test);
    F.open.delete(test);
    const top = F.stack[F.stack.length - 1];
    F.current = top ? sharedOf(F, top) : F.after;
  }
}

// The repository's modules on the stack of an open, each of which may keep what it read.
function keptBy(F, opened) {
  const prepare = Error.prepareStackTrace;
  const limit = Error.stackTraceLimit;
  let sites = [];
  try {
    Error.stackTraceLimit = 64;
    Error.prepareStackTrace = (_, callSites) => callSites;
    const holder = {};
    Error.captureStackTrace(holder, keptBy);
    sites = holder.stack || [];
  } finally {
    Error.prepareStackTrace = prepare;
    Error.stackTraceLimit = limit;
  }
  for (const site of sites) {
    const name = site && typeof site.getFileName === 'function' ? site.getFileName() : null;
    const module = name ? idPath(name) : null;
    if (!module || module === SELF || /[\\/]node_modules[\\/]/.test(module)) continue;
    if (!F.kept.has(module)) F.kept.set(module, new Set());
    F.kept.get(module).add(opened);
  }
}

function sharedOf(F, suite) {
  let b = F.shared.get(suite);
  if (!b) {
    b = newBucket();
    F.shared.set(suite, b);
  }
  return b;
}

function testOf(F, test) {
  let b = F.tests.get(test);
  if (!b) {
    b = newBucket();
    F.tests.set(test, b);
  }
  return b;
}

function testsOf(task, out = []) {
  for (const child of task.tasks || []) {
    if (child.type === 'test') out.push(child);
    else if (child.type === 'suite') testsOf(child, out);
  }
  return out;
}

// [ADR 0081] Each test's id: its suites' names and its own, joined with ` > `; the k-th test of
// the file with a name it already had gets ` [k]`. Names that join to one id from different
// suites, or an id that is still another's, make those tests ambiguous: never left out, never told.
function name(F) {
  const all = [];
  const walk = (task, names) => {
    for (const child of task.tasks || []) {
      const here = [...names, String(child.name)];
      if (child.type === 'test') all.push({ test: child, names: here });
      else if (child.type === 'suite') walk(child, here);
    }
  };
  walk(F.testFile || {}, []);
  const groups = new Map();
  for (const entry of all) {
    const base = entry.names.join(' > ');
    if (!groups.has(base)) groups.set(base, []);
    groups.get(base).push(entry);
  }
  const ids = new Map();
  for (const [base, entries] of groups) {
    const shape = JSON.stringify(entries[0].names);
    const mixed = entries.some((e) => JSON.stringify(e.names) !== shape);
    entries.forEach((entry, k) => {
      // Different suites that join to one name share it: they can't be told apart.
      ids.set(entry.test, mixed || k === 0 ? base : `${base} [${k + 1}]`);
    });
  }
  const count = new Map();
  for (const id of ids.values()) count.set(id, (count.get(id) || 0) + 1);
  F.ids = ids;
  // An id more than one test has: told once, never left out (C-43).
  F.shared_ids = new Set([...count].filter(([, n]) => n > 1).map(([id]) => id));
}

function declaredSkip(F, test) {
  for (let task = test; task; task = task.suite) {
    const mode = F.declared.get(task);
    if (mode === 'skip' || mode === 'todo') return true;
  }
  return false;
}

// After Vitest's own filters, before the file's first hook: what each test is, and the listed
// tests left out, with every suite all of whose tests are left out.
function choose(F) {
  const tests = testsOf(F.testFile || {});
  let changed = false;
  for (const test of tests) {
    if (test.concurrent) F.concurrent = true;
    const id = F.ids.get(test);
    if (test.mode === 'run' || test.mode === 'queued') {
      if (F.reuse.has(id) && !F.shared_ids.has(id)) {
        // Skipped, as Vitest's own report then says: its mode doesn't reach the main process.
        test.mode = 'skip';
        test.result = { state: 'skip' };
        F.kinds.set(test, 'deselected');
        changed = true;
      } else {
        F.kinds.set(test, 'planned');
      }
    } else if (declaredSkip(F, test)) {
      F.kinds.set(test, 'skipped');
    } else {
      F.kinds.set(test, 'filtered');
    }
  }
  if (!changed) return;
  const skipEmpty = (suite) => {
    for (const child of suite.tasks || []) if (child.type === 'suite') skipEmpty(child);
    const running = (t) => t.mode === 'run' || t.mode === 'queued';
    if (running(suite) && (suite.tasks || []).length && !suite.tasks.some(running)) {
      suite.mode = 'skip';
    }
  };
  skipEmpty(F.testFile);
}

// Whether a suite above `test` failed in a hook of its own: its `afterAll`, a cleanup, a snapshot
// check. A test that finished under it failed with it; one that didn't finish, didn't run.
function suiteFailed(test) {
  for (let s = test.suite || test.file; s; s = s === s.file ? null : (s.suite || s.file)) {
    if (s.result && Array.isArray(s.result.errors) && s.result.errors.length) return true;
  }
  return false;
}

function testsFacts(F) {
  const paths = [];
  const index = new Map();
  const at = (p) => {
    const rel = relativeTo(F.root, p);
    if (rel === null) return null;
    let i = index.get(rel);
    if (i === undefined) {
      i = paths.length;
      paths.push(rel);
      index.set(rel, i);
    }
    return i;
  };
  const many = (items) => {
    const out = new Set();
    for (const p of items) {
      const i = at(p);
      if (i !== null) out.add(i);
    }
    return [...out].sort((a, b) => a - b);
  };
  const whole = wholeFile();
  // What wasn't followed outside any test's window: the file's, every test's.
  const notFollowed = [...state.unattributed, ...F.loaded.notFollowed, ...F.after.notFollowed];
  const watched = F.watch && !F.concurrent;
  const served = (test) => {
    const out = [];
    for (let s = test.suite || test.file; s; s = s === s.file ? null : (s.suite || s.file)) {
      const b = F.shared.get(s);
      if (b) out.push(b);
    }
    return out;
  };
  const told = [];
  const order = new Map();
  const byId = new Map();
  for (const test of testsOf(F.testFile || {})) {
    let kind = F.kinds.get(test);
    if (kind === 'deselected' && (F.finished.has(test) || F.tests.has(test))) kind = 'planned';
    if (kind === undefined || kind === 'filtered') continue;
    const id = F.ids.get(test);
    const own = F.tests.get(test) || newBucket();
    const shared = served(test);
    const entry = {
      id,
      finished: false,
      failed: false,
      deselected: false,
      durationMs: 0,
      notFollowed: [],
    };
    if (kind === 'deselected') {
      entry.deselected = true;
    } else {
      if (kind === 'skipped') {
        entry.finished = true;
      } else {
        const state = test.result && test.result.state;
        entry.finished = F.finished.has(test)
          && (state === 'pass' || state === 'fail' || state === 'skip');
        entry.failed = state === 'fail' || (entry.finished && suiteFailed(test));
        entry.durationMs = Math.round((test.result && test.result.duration) || 0);
      }
      const unfollowed = [...own.notFollowed];
      for (const b of shared) unfollowed.push(...b.notFollowed);
      if (F.concurrent) notFollowed.push(...unfollowed);
      else entry.notFollowed = unfollowed;
      if (watched) {
        entry.ran = many(own.ran);
        entry.opened = many(own.opened);
        const dirs = [...own.dirs, ...F.loaded.dirs, ...F.after.dirs];
        for (const b of shared) dirs.push(...b.dirs);
        entry.dirs = many(dirs);
      }
    }
    // Tests that share an id are told once: failed if any failed, finished only if all did, with
    // no list of their own, and never left out (C-43).
    if (F.shared_ids.has(id)) {
      delete entry.ran;
      delete entry.opened;
      delete entry.dirs;
      entry.neverLeftOut = true;
      const at = byId.get(id);
      if (at !== undefined) {
        const had = told[at];
        had.finished = had.finished && entry.finished;
        had.failed = had.failed || entry.failed;
        had.durationMs += entry.durationMs;
        had.notFollowed.push(...entry.notFollowed);
        order.set(test, at);
        continue;
      }
      byId.set(id, told.length);
    }
    order.set(test, told.length);
    told.push(entry);
  }
  // A suite's own setup and teardown, for the tests it serves that are told.
  const shared = [];
  if (watched) {
    for (const [suite, b] of F.shared) {
      if (!b.ran.size && !b.opened.size) continue;
      const tests = [...new Set(testsOf(suite).filter((t) => order.has(t)).map((t) => order.get(t)))];
      if (tests.length) shared.push({ tests, ran: many(b.ran), opened: many(b.opened) });
    }
  } else if (F.concurrent) {
    for (const b of F.shared.values()) notFollowed.push(...b.notFollowed);
  }
  if (F.concurrent) {
    for (const b of F.tests.values()) notFollowed.push(...b.notFollowed);
  }
  // The unit by its path from the root, as every reader of tests one by one names it.
  const unit = {
    ...whole,
    unit: relativeTo(F.root, whole.unit) ?? whole.unit,
    notFollowed: dedupe(notFollowed),
    tests: told,
  };
  const facts = {
    schema: SCHEMA_V1,
    adapter: 'vitest-worker',
    units: [unit],
    errors: state.errors,
    paths,
    imports: {},
  };
  if (watched) {
    unit.loaded = {
      ran: many([...F.loaded.ran, ...F.after.ran]),
      opened: many([...F.loaded.opened, ...F.after.opened]),
    };
    unit.everyTest = many(F.every);
    unit.shared = shared;
    facts.imports = imports(at, many);
    // [ADR 0081, amended] What each module re-exports from, as the plugin read it: a test that
    // reads a barrel reads what it re-exports, to the end.
    facts.reexports = {};
    for (const [id, targets] of Object.entries(globalThis.__hushrun_vitest_reexports || {})) {
      const p = idPath(id);
      const from = p && p !== SELF ? at(p) : null;
      if (from === null || !Array.isArray(targets)) continue;
      const to = many(targets.map(idPath).filter(Boolean)).filter((i) => i !== from);
      if (to.length) facts.reexports[String(from)] = to;
    }
    facts.opens = {};
    for (const [module, files] of F.kept) {
      const from = at(module);
      if (from === null) continue;
      const to = many(files).filter((i) => i !== from);
      if (to.length) facts.opens[String(from)] = to;
    }
  }
  return facts;
}

function dedupe(items) {
  const seen = new Set();
  return items.filter((item) => {
    const key = `${item.why}\0${item.detail}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

// Each module's direct imports, as this worker's module runner evaluated them.
function imports(at, many) {
  const out = {};
  const evaluated = globalThis.__vitest_worker__?.evaluatedModules;
  const nodes = evaluated && evaluated.idToModuleMap;
  if (!nodes) return out;
  for (const [id, node] of nodes) {
    const from = idPath(id);
    const i = from && from !== SELF ? at(from) : null;
    if (i === null) continue;
    const to = many([...(node.imports || [])]
      .map(idPath)
      .filter((p) => p && p !== SELF));
    if (to.length) out[String(i)] = [...new Set([...(out[String(i)] || []), ...to])].sort((a, b) => a - b);
  }
  return out;
}

// Last, once every declaration above is made.
const told = typeof file === 'string' ? readTests() : null;
if (told) {
  try {
    startFile(told);
  } catch (err) {
    state.errors.push(`tests: ${message(err)}`.slice(0, 500));
    state.file = null;
  }
}
