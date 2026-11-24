'use strict';
// [ADR 0107] hushrun's adapter for Node's own test runner (`node --test`), loaded into every Node
// process of COMMAND with
//
//   NODE_OPTIONS="<what it had> --require <this file>"
//
// which hushrun sets. Without HUSHRUN_UNITS and its hushrun.json (C-32) it does nothing at all.
// With them it does something in two kinds of process only, and nothing in any other:
//
// - the runner's main process (`node --test`, no NODE_TEST_CONTEXT): it claims the run's
//   directory, lists the test files as the runner will (the same glob, `node_modules` left out,
//   sorted), and, when that list is hushrun.json's `testFiles`, coverage is off and no shard is
//   asked (a shorter list shards otherwise), replaces the runner's patterns in `process.argv`,
//   which the runner reads after every preload, with the test files hushrun.json's `reuse`
//   doesn't list. It hears the runner's own events (its TestsStream) for how each test file's
//   process ended, and writes `node-test-main-<pid>.json`.
// - each test file's process (NODE_TEST_CONTEXT=child-v8, which the runner gives it, and the
//   claim the main process left in its environment): one process runs one test file, so what it
//   read from its start to its exit is that test file's: every module Node compiled (CommonJS)
//   or loaded (ES modules, through `module.registerHooks`), every file opened, read or looked at
//   through `fs`, every directory listed. Its tests come from its own TestsStream's events,
//   which are in declaration order; with `unit: test` it leaves out, by `skip`, the tests
//   hushrun.json's `reuseTests` lists, through `node:test`'s own functions, wrapped. It writes
//   `node-test-<pid>.json`.
//
// A process a test file starts inherits HUSHRUN_NODE_TEST_IN and does nothing. It decides
// nothing: it writes what it saw, whole (a temporary name starting with `.`, then a rename), as
// `hushrun.unit-facts/v1`. Every hook is guarded: an error of its own goes in the file's
// `errors`, which makes what it saw `not observed`, and never reaches the runner.

const KEY = Symbol.for('hushrun.node-test');
if (!globalThis[KEY] && process.env.HUSHRUN_UNITS) {
  globalThis[KEY] = { started: Date.now() };
  try {
    install(globalThis[KEY]);
  } catch (err) {
    // Never break the process this was loaded into.
  }
}

function install(S) {
  const fs = require('fs');
  const path = require('path');
  const Module = require('module');
  const { fileURLToPath, pathToFileURL } = require('url');
  const SCHEMA = 'hushrun.unit-facts/v1';
  const RUN_SCHEMA = 'hushrun.units/v0';
  // The reason a left-out test is skipped with, which its events carry back: how this file tells
  // a test it left out from one the test's own code skipped.
  const LEFT_OUT = 'hushrun: passed before on the files it read';
  const dir = process.env.HUSHRUN_UNITS;
  const env = process.env;
  // A process below a test file (a script a test starts, a `node --test` inside a test): nothing.
  if (env.HUSHRUN_NODE_TEST_IN) return;
  const isChild = env.NODE_TEST_CONTEXT === 'child-v8';
  const isMain = !env.NODE_TEST_CONTEXT && process.execArgv.includes('--test');
  if (!isChild && !isMain) return;
  // A test file's process belongs to the start whose main process claimed the run.
  if (isChild && !env.HUSHRUN_NODE_TEST_RUN) return;
  const own = {
    readFileSync: fs.readFileSync,
    writeFileSync: fs.writeFileSync,
    renameSync: fs.renameSync,
    openSync: fs.openSync,
    closeSync: fs.closeSync,
    statSync: fs.statSync,
    realpathSync: fs.realpathSync.native || fs.realpathSync,
  };
  let run = null;
  try {
    run = JSON.parse(own.readFileSync(path.join(dir, 'hushrun.json'), 'utf8'));
  } catch {
    return;
  }
  if (!run || run.schema !== RUN_SCHEMA || typeof run.root !== 'string') return;
  const realOrSelf = (p) => {
    try {
      return own.realpathSync(p);
    } catch {
      return path.resolve(p);
    }
  };
  const root = realOrSelf(run.root);
  S.errors = [];
  function note(what, err) {
    S.errors.push(`${what}: ${err && err.message ? err.message : String(err)}`.slice(0, 400));
  }
  function guard(what, fn) {
    try {
      return fn();
    } catch (err) {
      note(what, err);
      return undefined;
    }
  }
  // A repository file, relative to the root, '/'-separated; null for anything else.
  const absCache = new Map();
  function rel(p) {
    if (p == null) return null;
    let abs;
    try {
      if (typeof p === 'string') {
        if (p.startsWith('node:')) return null;
        abs = p.startsWith('file:') ? fileURLToPath(p) : path.resolve(p);
      } else if (p instanceof URL) {
        if (p.protocol !== 'file:') return null;
        abs = fileURLToPath(p);
      } else if (Buffer.isBuffer(p)) abs = path.resolve(p.toString());
      else return null;
    } catch {
      return null;
    }
    if (absCache.has(abs)) return absCache.get(abs);
    let r = path.relative(root, abs);
    if (!r || r.startsWith('..') || path.isAbsolute(r)) {
      // A path through a symlink (macOS /tmp -> /private/tmp) may still be inside.
      r = path.relative(root, realOrSelf(abs));
    }
    let out = null;
    if (r && !r.startsWith('..') && !path.isAbsolute(r) && !/(^|[\\/])node_modules([\\/]|$)/.test(r)) {
      out = r.split(path.sep).join('/');
    }
    if (absCache.size < 200000) absCache.set(abs, out);
    return out;
  }
  function writeWhole(name, value) {
    const target = path.join(dir, name);
    const temp = path.join(dir, `.${name}.${process.pid}.tmp`);
    own.writeFileSync(temp, JSON.stringify(value));
    own.renameSync(temp, target);
  }
  const dedupe = (items) => {
    const seen = new Map();
    for (const item of items) seen.set(`${item.why}\0${item.detail}`, item);
    return [...seen.values()];
  };

  // Every event of the runner's TestsStream in this process, to `listener`. The stream is the
  // runner's own, made before any preload can see it: its `emit` is found on the prototype chain,
  // so that its own events, and nothing else's, reach the listener.
  function hearEvents(listener) {
    const EventEmitter = require('events');
    const emit = EventEmitter.prototype.emit;
    if (emit.__hushrunNodeTest) return;
    const wrapped = { emit(type, data) {
      if (typeof type === 'string' && type.startsWith('test:') && this && this.constructor
        && this.constructor.name === 'TestsStream') {
        try {
          listener(type, data || {});
        } catch (err) {
          note(`event ${type}`, err);
        }
      }
      return emit.apply(this, arguments);
    } }.emit;
    Object.defineProperty(wrapped, 'length', { value: emit.length });
    Object.defineProperty(wrapped, 'name', { value: 'emit' });
    Object.defineProperty(wrapped, '__hushrunNodeTest', { value: true });
    EventEmitter.prototype.emit = wrapped;
  }

  if (isMain) main();
  else child();

  // ---------------------------------------------------------------- the runner's main process
  function main() {
    // [C-32] Another start of the runner in this run, below a test file's process or beside the
    // one that claimed it: it says so once, and keeps every pass of the run out.
    let claimed = true;
    try {
      own.closeSync(own.openSync(path.join(dir, 'node-test-claim'), 'wx'));
    } catch {
      claimed = false;
    }
    if (!claimed) {
      guard('second start', () => writeWhole(`node-test-second-${process.pid}.started`, {
        why: "Node's test runner started more than once in its command, and hushrun observes only the first start: what another start ran can't be told",
      }));
      env.HUSHRUN_NODE_TEST_IN = String(process.pid);
      return;
    }
    const start = `node-test-${process.pid}-${S.started}`;
    env.HUSHRUN_NODE_TEST_RUN = String(process.pid);
    env.HUSHRUN_NODE_TEST_START = start;
    env.HUSHRUN_NODE_TEST_DESELECT = '0';
    const M = {
      testFiles: [],
      removed: [],
      refused: null,
      byName: new Map(), // the name the runner gives a test file's own test -> its unit
      ended: new Map(), // unit -> { passed, failureType, durationMs }
      summary: false,
    };
    S.main = M;
    const args = process.execArgv;
    const options = `${args.join(' ')} ${env.NODE_OPTIONS || ''}`;
    const has = (flag) => args.some((a) => a === flag || a.startsWith(`${flag}=`));
    const notObserved = (() => {
      if (/--(experimental-)?test-isolation[= ]none/.test(options)) {
        return "Node's test runner runs every test file in one process (--test-isolation=none), so what each read can't be told";
      }
      if (has('--watch') || has('--watch-path')) return "Node's test runner reruns as files change (--watch)";
      if (typeof fs.globSync !== 'function') {
        return `this Node, ${process.version}, has no fs.globSync, which hushrun lists test files with as Node's test runner does (Node 22 or later)`;
      }
      if (typeof Module.registerHooks !== 'function') {
        return `this Node, ${process.version}, has no module.registerHooks, which hushrun sees each test file's ES modules with (Node 22.15 or later)`;
      }
      return null;
    })();
    const coverage = has('--experimental-test-coverage') || has('--test-coverage');
    const shard = has('--test-shard');
    // The test files, as the runner lists them: its patterns, or its default one, through the
    // same glob, `node_modules` left out, sorted.
    const cwd = process.cwd();
    const patterns = process.argv.slice(1);
    let listed = null;
    if (!notObserved) {
      listed = guard('list test files', () => {
        let globs = patterns;
        if (globs.length === 0) {
          const kinds = ['js', 'mjs', 'cjs'];
          if (process.features && process.features.typescript) kinds.push('ts', 'mts', 'cts');
          globs = [`**/{test,test/**/*,test-*,*[._-]test}.{${kinds.join(',')}}`];
        }
        const quiet = process.emitWarning;
        // fs.glob is experimental in Node 22, and says so once: not in the runner's output.
        process.emitWarning = function (warning, ...rest) {
          const text = String(warning && warning.message ? warning.message : warning);
          if (/glob/i.test(text)) return undefined;
          return quiet.call(this, warning, ...rest);
        };
        try {
          return fs.globSync(globs, { cwd, exclude: (name) => name === 'node_modules' }).sort();
        } finally {
          process.emitWarning = quiet;
        }
      });
    }
    if (listed) {
      for (const name of listed) {
        const unit = rel(path.resolve(cwd, name));
        if (unit) {
          M.testFiles.push(unit);
          M.byName.set(name, unit);
        }
      }
      M.testFiles.sort();
    }
    // Which test files the plan lets this run leave out.
    const reuse = new Set(Array.isArray(run.reuse) ? run.reuse : []);
    const sameList = Array.isArray(run.testFiles)
      && JSON.stringify([...run.testFiles].sort()) === JSON.stringify(M.testFiles);
    let mayLeaveOut = false;
    if (notObserved || !listed) M.refused = notObserved || "hushrun couldn't list the test files as Node's test runner does";
    else if (coverage) M.refused = "Node's test coverage is on, and a partial run's coverage would read as a drop";
    else if (!Array.isArray(run.testFiles)) M.refused = null;
    else if (!sameList) M.refused = "Node's test runner lists other test files than the plan does";
    else if (listed.length !== M.testFiles.length) M.refused = 'a test file the runner lists is outside the repository';
    else mayLeaveOut = true;
    if (mayLeaveOut && reuse.size > 0) {
      const kept = listed.filter((name) => !reuse.has(M.byName.get(name)));
      const removed = listed.filter((name) => reuse.has(M.byName.get(name)));
      if (removed.length > 0 && shard) {
        M.refused = "the runner shards its list (--test-shard), and a shorter list would shard otherwise";
        mayLeaveOut = false;
      } else if (removed.length > 0 && kept.some((name) => /[*?[\]{}()!+@\\]/.test(name))) {
        M.refused = "a test file's name has a glob character, which the runner's patterns can't name alone";
        mayLeaveOut = false;
      } else if (removed.length > 0) {
        // The runner reads its patterns from `process.argv` after every preload. None kept: a
        // pattern that matches nothing, which the runner takes as no test file.
        const none = `__hushrun_left_out_every_test_file__/*.${process.pid}`;
        process.argv.splice(1, process.argv.length - 1, ...(kept.length > 0 ? kept : [none]));
        M.removed = removed.map((name) => M.byName.get(name));
      }
    }
    env.HUSHRUN_NODE_TEST_DESELECT = mayLeaveOut && !coverage ? '1' : '0';
    if (notObserved) {
      S.errors.push(notObserved);
    } else {
      hearEvents((type, data) => {
        if (type === 'test:summary' && data.file === undefined) M.summary = true;
        if (type !== 'test:complete' || data.nesting !== 0) return;
        // A test file's own test, which the runner names by the path it was given.
        const unit = M.byName.get(data.name);
        if (!unit || !data.file || path.resolve(cwd, data.name) !== data.file) return;
        const details = data.details || {};
        const error = details.error;
        M.ended.set(unit, {
          passed: !!details.passed,
          failureType: error && (error.failureType || (error.cause && error.cause.failureType)) || null,
          durationMs: Math.round(details.duration_ms || 0),
        });
      });
    }
    process.on('exit', () => guard('write', () => {
      const units = [];
      const unowned = [];
      for (const [unit, end] of M.ended) {
        units.push({
          unit,
          selected: 1,
          finished: 1,
          failed: !end.passed,
          deselected: false,
          durationMs: end.durationMs,
        });
        // A test file's process failed for a reason none of its tests owns: it exited with an
        // error after its tests (a late uncaught exception), or stopped before they ran.
        if (!end.passed && end.failureType !== 'subtestsFailed') unowned.push(unit);
      }
      for (const unit of M.removed) {
        units.push({ unit, selected: 0, finished: 0, failed: false, deselected: true, durationMs: 0 });
      }
      writeWhole(`node-test-main-${process.pid}.json`, {
        schema: SCHEMA,
        adapter: 'node-test-main',
        start,
        failuresByUnit: M.summary,
        settings: {
          version: process.version,
          testFiles: M.testFiles,
          refused: M.refused,
          unowned,
          complete: M.summary,
        },
        notFollowed: [],
        units,
        errors: S.errors,
      });
    }));
  }

  // ---------------------------------------------------------------- one test file's process
  function child() {
    env.HUSHRUN_NODE_TEST_IN = String(process.pid);
    const file = process.argv[1] ? path.resolve(process.argv[1]) : null;
    const unit = file && rel(file);
    if (!unit) return;
    const F = {
      reads: new Set([unit]),
      dirs: new Set(),
      notFollowed: [],
      edges: new Set(),
    };
    const opened = (p) => {
      const r = rel(p);
      if (r) F.reads.add(r);
    };
    const listed = (p) => {
      const r = rel(p);
      if (r) F.dirs.add(r);
    };
    const unfollowed = (why, detail) => F.notFollowed.push({ why, detail: String(detail).slice(0, 200) });

    // What a wrapper carries of what it wraps: its `name`, `length` and own properties, so that
    // callers see what they would without hushrun. `util.promisify.custom` is wrapped too.
    const PROMISIFIED = require('util').promisify.custom;
    const WRAPPED = new WeakSet();
    function carry(wrapped, original, each = (key, desc) => desc) {
      WRAPPED.add(wrapped);
      for (const key of Reflect.ownKeys(original)) {
        if (key === 'prototype' || key === 'arguments' || key === 'caller') continue;
        try {
          Object.defineProperty(wrapped, key, each(key, Object.getOwnPropertyDescriptor(original, key)));
        } catch (err) {
          note(`carry ${String(key)}`, err);
        }
      }
      return wrapped;
    }
    function observing(original, record) {
      const call = (self, args) => {
        try {
          record(args[0]);
        } catch (err) {
          note('record', err);
        }
        return original.apply(self, args);
      };
      const wrapped = Object.prototype.hasOwnProperty.call(original, 'prototype')
        ? function (...args) { return call(this, args); }
        : { w(...args) { return call(this, args); } }.w;
      return carry(wrapped, original, (key, desc) => {
        if (key === PROMISIFIED && typeof desc.value === 'function') {
          desc.value = desc.value === original ? wrapped : observing(desc.value, record);
        }
        return desc;
      });
    }
    function wrap(target, name, record) {
      const original = target[name];
      if (typeof original !== 'function' || WRAPPED.has(original)) return;
      target[name] = observing(original, record);
    }
    for (const target of [fs, fs.promises]) {
      for (const name of ['readFile', 'readFileSync', 'open', 'openSync', 'createReadStream', 'copyFile',
        'copyFileSync', 'cp', 'cpSync', 'stat', 'statSync', 'lstat', 'lstatSync', 'access', 'accessSync',
        'existsSync', 'exists', 'realpath', 'realpathSync', 'readlink', 'readlinkSync']) wrap(target, name, opened);
      for (const name of ['readdir', 'readdirSync', 'opendir', 'opendirSync', 'glob', 'globSync']) {
        wrap(target, name, name.startsWith('glob') ? () => unfollowed('glob', name) : listed);
      }
    }
    const cp = require('child_process');
    for (const name of ['spawn', 'spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync', 'fork']) {
      wrap(cp, name, (cmd) => unfollowed('subprocess', `${name} ${String(cmd)}`));
    }
    const wt = require('worker_threads');
    if (typeof wt.Worker === 'function') {
      const OriginalWorker = wt.Worker;
      wt.Worker = carry(class Worker extends OriginalWorker {
        constructor(...args) {
          unfollowed('worker', String(args[0]));
          super(...args);
        }
      }, OriginalWorker);
    }
    const dlopen = process.dlopen;
    process.dlopen = carry(function (...args) {
      unfollowed('native', String(args[1]));
      return dlopen.apply(this, args);
    }, dlopen);
    // What Node compiles: every CommonJS module, and, through the module hooks, every ES module
    // and what each imports.
    const compile = Module.prototype._compile;
    Module.prototype._compile = carry(function hushrunCompile(content, filename) {
      guard('compile', () => opened(filename));
      return compile.apply(this, arguments);
    }, compile);
    try {
      Module.syncBuiltinESMExports();
    } catch (err) {
      note('syncBuiltinESMExports', err);
    }

    // ------------------------------------------------ leaving tests out, through node:test's API
    const byTest = run.unit === 'test';
    const toLeaveOut = new Set(byTest && env.HUSHRUN_NODE_TEST_DESELECT === '1'
      && run.reuseTests && Array.isArray(run.reuseTests[unit]) ? run.reuseTests[unit] : []);
    const leftOut = new Set();
    const SHIM = 'hushrun-node-test-api.mjs';
    let api = null;
    if (toLeaveOut.size > 0) api = guard('wrap node:test', () => wrapApi(require('node:test')));
    if (typeof Module.registerHooks === 'function') {
      guard('module hooks', () => Module.registerHooks({
        resolve(specifier, context, nextResolve) {
          // `import test from 'node:test'` takes the default export, which no property of the
          // builtin's can change: an ES module of the same exports, wrapped, stands in for it.
          if (api && (specifier === 'node:test') && !(context.conditions || []).includes('require')
            && !(context.parentURL || '').endsWith(SHIM)) {
            return { url: `${pathToFileURL(path.join(__dirname, SHIM)).href}`, format: 'module', shortCircuit: true };
          }
          const out = nextResolve(specifier, context);
          guard('edge', () => {
            const from = context.parentURL && rel(context.parentURL);
            const to = out && out.url && out.url.startsWith('file:') ? rel(out.url) : null;
            if (from && to && from !== to) F.edges.add(`${from}\0${to}`);
          });
          return out;
        },
        load(url, context, nextLoad) {
          if (api && url.endsWith(SHIM)) {
            const names = Object.keys(api).filter((k) => /^[A-Za-z_$][\w$]*$/.test(k) && k !== 'default');
            const source = `const api = globalThis[Symbol.for('hushrun.node-test')].api;\nexport default api;\n`
              + names.map((k) => `export const ${k} = api[${JSON.stringify(k)}];`).join('\n');
            return { format: 'module', source, shortCircuit: true };
          }
          if (url.startsWith('file:')) guard('load', () => opened(url));
          return nextLoad(url, context);
        },
      }));
    } else {
      unfollowed('esm', `this Node, ${process.version}, has no module.registerHooks`);
    }
    S.api = api;
    // A CommonJS `require('node:test')` gets the wrapped API too: the function itself is what
    // `const test = require('node:test')` calls.
    if (api) {
      const load = Module._load;
      Module._load = carry(function hushrunLoad(request) {
        if (request === 'node:test') return api;
        return load.apply(this, arguments);
      }, load);
    }

    // node:test's functions, each of whose registrations is given the id its events will have,
    // and `skip` when `reuseTests` lists it.
    function wrapApi(orig) {
      const { AsyncLocalStorage } = require('async_hooks');
      const scope = new AsyncLocalStorage();
      const counts = new Map();
      // The arguments as Node's test() takes them: ([name][, options][, fn]).
      function parse(args) {
        let [name, options, fn] = args;
        if (typeof name === 'function') {
          fn = name;
          name = undefined;
          options = undefined;
        } else if (name !== null && typeof name === 'object') {
          fn = options;
          options = name;
          name = undefined;
        } else if (typeof options === 'function') {
          fn = options;
          options = undefined;
        }
        if (typeof name !== 'string' || name === '') name = (fn && fn.name) || '<anonymous>';
        return { name, options: options == null ? {} : options, fn };
      }
      function idOf(name) {
        const names = [...(scope.getStore() || []), name];
        const joined = names.join(' > ');
        const k = (counts.get(joined) || 0) + 1;
        counts.set(joined, k);
        return { names, id: k > 1 ? `${joined} [${k}]` : joined };
      }
      function inScope(fn, names) {
        if (typeof fn !== 'function') return fn;
        const wrapped = function (...args) {
          if (args[0] && typeof args[0] === 'object' && typeof args[0].test === 'function') patchContext(args[0]);
          return scope.run(names, () => fn.apply(this, args));
        };
        Object.defineProperty(wrapped, 'length', { value: fn.length });
        Object.defineProperty(wrapped, 'name', { value: fn.name });
        return wrapped;
      }
      function registering(original, isTest) {
        const wrapped = function (...args) {
          const { name, options, fn } = parse(args);
          const { names, id } = idOf(name);
          let opts = options;
          if (isTest && toLeaveOut.has(id) && !options.todo) {
            opts = { ...options, skip: LEFT_OUT };
            leftOut.add(id);
          }
          return original.call(this, name, opts, inScope(fn, names));
        };
        Object.defineProperty(wrapped, 'length', { value: original.length });
        Object.defineProperty(wrapped, 'name', { value: original.name });
        return wrapped;
      }
      const contexts = new WeakSet();
      function patchContext(t) {
        const proto = Object.getPrototypeOf(t);
        if (!proto || contexts.has(proto) || typeof proto.test !== 'function') return;
        contexts.add(proto);
        proto.test = registering(proto.test, true);
      }
      const withVariants = (original, isTest) => {
        const wrapped = registering(original, isTest);
        for (const variant of ['skip', 'only', 'todo']) {
          if (typeof original[variant] === 'function') wrapped[variant] = registering(original[variant], isTest);
        }
        return wrapped;
      };
      const api = withVariants(orig, true);
      for (const key of Reflect.ownKeys(orig)) {
        if (key === 'length' || key === 'name' || key === 'prototype' || key === 'skip' || key === 'only' || key === 'todo') continue;
        try {
          Object.defineProperty(api, key, Object.getOwnPropertyDescriptor(orig, key));
        } catch (err) {
          note(`api ${String(key)}`, err);
        }
      }
      for (const key of ['test', 'it']) if (typeof orig[key] === 'function') api[key] = withVariants(orig[key], true);
      for (const key of ['describe', 'suite']) if (typeof orig[key] === 'function') api[key] = withVariants(orig[key], false);
      // `const { test } = require('node:test')` and `import { test } from 'node:test'`.
      for (const key of ['test', 'it', 'describe', 'suite']) {
        if (typeof api[key] === 'function') {
          try {
            orig[key] = api[key];
          } catch (err) {
            note(`api ${key}`, err);
          }
        }
      }
      Module.syncBuiltinESMExports();
      return api;
    }

    // ------------------------------------------------ the tests, from this process's events
    // Reported in declaration order (`test:start` before a test's subtests, `test:pass` or
    // `test:fail` after them), which is how each test's suites are told.
    const T = {
      stack: [], // the open tests and suites, by nesting: { names, record }
      counts: new Map(),
      records: [],
      enqueued: 0,
      reportedStarts: 0,
      rootFailure: null,
      uncaught: null,
    };
    hearEvents((type, data) => {
      if (S.listen) S.listen();
      if (type === 'test:enqueue') {
        T.enqueued += 1;
      } else if (type === 'test:start') {
        T.reportedStarts += 1;
        const nesting = data.nesting | 0;
        T.stack.length = Math.min(T.stack.length, nesting);
        const above = T.stack[nesting - 1];
        const names = [...(above ? above.names : []), String(data.name)];
        const joined = names.join(' > ');
        const k = (T.counts.get(joined) || 0) + 1;
        T.counts.set(joined, k);
        const record = {
          id: k > 1 ? `${joined} [${k}]` : joined,
          names,
          status: 'pending',
          isSuite: false,
          hasChildren: false,
          durationMs: 0,
          parent: above ? above.record : null,
        };
        if (above) above.record.hasChildren = true;
        T.stack[nesting] = { names, record };
        T.records.push(record);
      } else if (type === 'test:pass' || type === 'test:fail') {
        const nesting = data.nesting | 0;
        const details = data.details || {};
        const open = T.stack[nesting];
        if (!open || open.names[open.names.length - 1] !== String(data.name)) {
          // The root's own failure: a hook at the file's top level, named by the file.
          if (type === 'test:fail' && nesting === 0 && data.name === file) {
            const error = details.error || {};
            T.rootFailure = error.failureType || 'unknown';
          } else note('events', `${type} ${String(data.name)} without its start`);
          return;
        }
        const r = open.record;
        r.isSuite = details.type === 'suite';
        r.durationMs = Math.round(details.duration_ms || 0);
        const error = details.error || {};
        if (data.todo !== undefined && data.todo !== false) r.status = 'skipped';
        else if (data.skip === LEFT_OUT) r.status = 'left-out';
        else if (data.skip !== undefined && data.skip !== false) r.status = 'skipped';
        else if (type === 'test:pass') r.status = 'passed';
        else if (error.failureType === 'cancelledByParent') r.status = 'cancelled';
        else r.status = 'failed';
        r.failureType = type === 'test:fail' ? error.failureType || null : null;
      }
    });
    for (const event of ['uncaughtException', 'unhandledRejection']) {
      process.on(event, (err) => {
        if (!T.uncaught) T.uncaught = String(err && err.message ? err.message : err).split('\n')[0];
      });
    }

    // Written at exit by the listener added after the runner's own (at its first event), which
    // reports what never ended before the process goes; one added now writes when no event came.
    let late = false;
    const listen = () => {
      if (late) return;
      late = true;
      process.on('exit', write);
    };
    process.on('exit', (code) => {
      if (!late) write(code);
    });
    S.listen = listen;
    function write(code) {
      guard('write', () => writeFacts(code));
    }
    function writeFacts(code) {
      const records = T.records.filter((r) => !r.isSuite);
      // [ADR 0107] A suite whose own hook failed fails every test under it; a hook of the file's
      // own (its root) every test of the file.
      const underFailed = (r) => {
        for (let p = r.parent; p; p = p.parent) {
          if (p.isSuite && p.status === 'failed' && p.failureType !== 'subtestsFailed') return true;
        }
        return false;
      };
      for (const r of records) {
        if (T.rootFailure === 'hookFailed' || underFailed(r)) r.status = 'failed';
      }
      // An error no test owns: no test of the file keeps a result of this run.
      const failedTests = records.some((r) => r.status === 'failed');
      let unsure = null;
      if (T.uncaught) unsure = `Node's test runner reported an error it couldn't charge to a test${T.uncaught.length <= 120 ? ` (${T.uncaught})` : ''}, so no test of its file keeps a result of that run`;
      else if (T.rootFailure && T.rootFailure !== 'hookFailed') unsure = "the test file's process failed outside its tests, so no test of its file keeps a result of that run";
      else if ((code || process.exitCode) && !failedTests) unsure = "the test file's process exited with an error no test owns, so no test of its file keeps a result of that run";
      // [C-39] A leftover the plan didn't list: the run goes again with nothing left out.
      const unlisted = records.filter((r) => r.status === 'left-out' && !toLeaveOut.has(r.id));
      const ran = (r) => r.status === 'passed' || r.status === 'failed';
      let selected = 0;
      let finished = 0;
      for (const r of records) {
        if (ran(r)) finished += 1;
        if (ran(r) || r.status === 'pending' || r.status === 'cancelled') selected += 1;
      }
      // Tests the runner was given and never reported (a process that stopped early).
      selected += Math.max(0, T.enqueued - T.reportedStarts);
      if (unsure) finished = 0;
      const out = {
        unit,
        selected,
        finished,
        skipped: records.filter((r) => r.status === 'skipped').length,
        failed: failedTests || !!T.rootFailure || !!unsure,
        deselected: false,
        durationMs: Date.now() - S.started,
        reads: [...F.reads].sort(),
        dirs: [...F.dirs].sort(),
        notFollowed: dedupe(F.notFollowed),
      };
      if (byTest) {
        const groups = new Map();
        for (const r of records) {
          if (!groups.has(r.id)) groups.set(r.id, []);
          groups.get(r.id).push(r);
        }
        const done = (r) => !unsure && (ran(r) || r.status === 'skipped');
        out.tests = [...groups.values()].map((rs) => {
          const r = rs[0];
          // [C-39] Ids that collide are told as one, never left out; and so is a test with
          // subtests, whose subtests run only when its body does.
          if (rs.length > 1 || r.hasChildren) {
            return {
              id: r.id,
              finished: rs.every(done),
              failed: rs.some((x) => x.status === 'failed'),
              ...(rs.some((x) => x.status === 'skipped') ? { skipped: true } : {}),
              ...(unsure ? { notKept: unsure } : {}),
              deselected: false,
              durationMs: rs.reduce((n, x) => n + x.durationMs, 0),
              notFollowed: [],
              neverLeftOut: true,
            };
          }
          const test = {
            id: r.id,
            finished: done(r),
            failed: r.status === 'failed',
            deselected: r.status === 'left-out',
            durationMs: r.durationMs,
            notFollowed: [],
          };
          if (r.status === 'skipped') test.skipped = true;
          if (unsure) test.notKept = unsure;
          return test;
        });
      }
      const table = [];
      const index = new Map();
      const ix = (p) => {
        if (!index.has(p)) {
          index.set(p, table.length);
          table.push(p);
        }
        return index.get(p);
      };
      const imports = {};
      for (const edge of F.edges) {
        const [from, to] = edge.split('\0');
        const key = String(ix(from));
        (imports[key] = imports[key] || []).push(ix(to));
      }
      writeWhole(`node-test-${process.pid}.json`, {
        schema: SCHEMA,
        adapter: 'node-test',
        start: env.HUSHRUN_NODE_TEST_START,
        failuresByUnit: true,
        settings: { version: process.version },
        notFollowed: [],
        units: [out],
        errors: S.errors,
        edges: [...F.edges].map((e) => [...e.split('\0'), 'import']),
        paths: table,
        imports,
        ...(unlisted.length > 0 ? { leftOutUnlisted: `Node's test runner left out ${unlisted[0].id}, which the plan didn't list` } : {}),
      });
    }
  }
}
