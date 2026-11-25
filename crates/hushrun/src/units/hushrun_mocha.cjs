'use strict';
// [ADR 0107] hushrun's Mocha adapter, loaded into every Node process of COMMAND with
//
//   NODE_OPTIONS="<what it had> --require <this file>"
//
// which hushrun sets. Without HUSHRUN_UNITS and its hushrun.json (C-32) it does nothing at all;
// in a process where no Mocha loads, it writes nothing. It looks at what each CommonJS load
// returns, by shape, for Mocha's own class (its `loadFiles`, `loadFilesAsync`, `Runner` and
// `Suite`), never by path or version.
//
// Mocha runs every spec file in one process, one after another, and loads them all before any
// test runs. So a spec file is never left out by not loading it: each loads as it would without
// hushrun, root hooks and what its top level does included, and at the runner's start, before
// any hook, the tests hushrun.json's `reuse` and `reuseTests` list are taken out of their suites,
// which Mocha then neither runs nor reports, nor runs the hooks of a suite left with none.
//
// What a spec file read is what was read while it loaded and while its tests and their hooks ran,
// and everything the modules it loaded load, to the end, as Node's module graph has it (each
// module's `children`, the module hooks' resolutions for ES modules), with what any of those
// modules' code opened, by every repository frame of the stack of the open; and, for every spec
// file, what was read before the first one loaded (`--require`, a config), outside every window,
// in a root hook, and the files that define a root hook or that `--file` names.
//
// It decides nothing: it writes what it saw, whole (a temporary name starting with `.`, then a
// rename), as `hushrun.unit-facts/v1`, `mocha-<pid>.json`. Every hook is guarded: an error of
// its own goes in the file's `errors`, which makes what it saw `not observed`, and never reaches
// Mocha.

const KEY = Symbol.for('hushrun.mocha');
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
  const { fileURLToPath } = require('url');
  const SCHEMA = 'hushrun.unit-facts/v1';
  const RUN_SCHEMA = 'hushrun.units/v0';
  const dir = process.env.HUSHRUN_UNITS;
  const env = process.env;
  // A Mocha a test starts: nothing.
  if (env.HUSHRUN_MOCHA_IN) return;
  const own = {
    readFileSync: fs.readFileSync,
    writeFileSync: fs.writeFileSync,
    renameSync: fs.renameSync,
    openSync: fs.openSync,
    closeSync: fs.closeSync,
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
    if (!r || r.startsWith('..') || path.isAbsolute(r)) r = path.relative(root, realOrSelf(abs));
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

  // ---------------------------------------------------------------- where a read goes
  // The window: a spec file loading, a test or a hook of it running (its file), or anything else
  // (`every`), which every spec file reads.
  const bucket = () => ({ reads: new Set(), dirs: new Set(), notFollowed: [] });
  const every = bucket();
  const byFile = new Map();
  let window = null; // a spec file, from the root, or null for `every`
  const into = () => {
    if (!window) return every;
    if (!byFile.has(window)) byFile.set(window, bucket());
    return byFile.get(window);
  };
  // What each repository module's code opened, by every repository frame of the open's stack: a
  // spec file that runs a module reads what the module read once and kept.
  const opens = new Map();
  const FRAME = /(?:\(|at )((?:file:\/\/)?(?:\/|[A-Za-z]:\\)[^():]+):\d+:\d+\)?$/;
  function openedBy(r) {
    const stack = new Error().stack || '';
    for (const line of stack.split('\n')) {
      const m = FRAME.exec(line.trim());
      if (!m || m[1] === __filename) continue;
      const frame = rel(m[1]);
      if (!frame || frame === r) continue;
      if (!opens.has(frame)) opens.set(frame, new Set());
      opens.get(frame).add(r);
    }
  }
  let observing = false; // from Mocha's first load of spec files to its runner's end
  const opened = (p) => {
    const r = rel(p);
    if (!r) return;
    into().reads.add(r);
    if (observing) openedBy(r);
  };
  // Whether the repository's own code is on the stack: Mocha listing directories as it collects
  // spec files is no spec file's read.
  const byRepository = () => (new Error().stack || '').split('\n').some((line) => {
    const m = FRAME.exec(line.trim());
    return m && m[1] !== __filename && rel(m[1]) !== null;
  });
  const listed = (p) => {
    const r = rel(p);
    if (r && (window || byRepository())) into().dirs.add(r);
  };
  const compiled = (p) => {
    const r = rel(p);
    if (r) into().reads.add(r);
  };
  const unfollowed = (why, detail) => into().notFollowed.push({ why, detail: String(detail).slice(0, 200) });

  // ---------------------------------------------------------------- fs, processes, addons
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
  function observingFn(original, record) {
    // What a process read before Mocha loaded (its config, `--require`) is every spec file's.
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
        desc.value = desc.value === original ? wrapped : observingFn(desc.value, record);
      }
      return desc;
    });
  }
  function wrap(target, name, record) {
    const original = target[name];
    if (typeof original !== 'function' || WRAPPED.has(original)) return;
    target[name] = observingFn(original, record);
  }
  for (const target of [fs, fs.promises]) {
    for (const name of ['readFile', 'readFileSync', 'open', 'openSync', 'createReadStream', 'copyFile',
      'copyFileSync', 'cp', 'cpSync', 'stat', 'statSync', 'lstat', 'lstatSync', 'access', 'accessSync',
      'existsSync', 'exists', 'realpath', 'realpathSync', 'readlink', 'readlinkSync']) wrap(target, name, opened);
    for (const name of ['readdir', 'readdirSync', 'opendir', 'opendirSync']) wrap(target, name, listed);
    for (const name of ['glob', 'globSync']) wrap(target, name, () => unfollowed('glob', name));
  }
  const cp = require('child_process');
  for (const name of ['spawn', 'spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync', 'fork']) {
    wrap(cp, name, (cmd) => {
      if (observing) unfollowed('subprocess', `${name} ${String(cmd)}`);
    });
  }
  const wt = require('worker_threads');
  if (typeof wt.Worker === 'function') {
    const OriginalWorker = wt.Worker;
    wt.Worker = carry(class Worker extends OriginalWorker {
      constructor(...args) {
        if (observing) unfollowed('worker', String(args[0]));
        super(...args);
      }
    }, OriginalWorker);
  }
  const dlopen = process.dlopen;
  process.dlopen = carry(function (...args) {
    if (observing) unfollowed('native', String(args[1]));
    return dlopen.apply(this, args);
  }, dlopen);
  const compile = Module.prototype._compile;
  Module.prototype._compile = carry(function hushrunCompile(content, filename) {
    guard('compile', () => compiled(filename));
    return compile.apply(this, arguments);
  }, compile);
  // ES modules: what loads, and what each imports.
  const esmEdges = new Set();
  if (typeof Module.registerHooks === 'function') {
    guard('module hooks', () => Module.registerHooks({
      resolve(specifier, context, nextResolve) {
        const out = nextResolve(specifier, context);
        guard('edge', () => {
          const from = context.parentURL && rel(context.parentURL);
          const to = out && typeof out.url === 'string' && out.url.startsWith('file:') ? rel(out.url) : null;
          if (from && to && from !== to) esmEdges.add(`${from}\0${to}`);
        });
        return out;
      },
      load(url, context, nextLoad) {
        if (url.startsWith('file:')) guard('load', () => compiled(url));
        return nextLoad(url, context);
      },
    }));
  } else {
    S.noHooks = `this Node, ${process.version}, has no module.registerHooks, which hushrun sees ES modules load with (Node 22.15 or later)`;
  }
  // Mocha's own class, by its shape, as it loads: own properties read by their descriptors only,
  // so that no getter of another module's runs.
  const own_ = (o, k) => {
    try {
      const d = o && Object.getOwnPropertyDescriptor(o, k);
      return d && 'value' in d ? d.value : undefined;
    } catch {
      return undefined;
    }
  };
  const isMocha = (f) => {
    const proto = own_(f, 'prototype');
    return typeof own_(proto, 'loadFiles') === 'function' && typeof own_(proto, 'run') === 'function'
      && typeof own_(f, 'Runner') === 'function' && typeof own_(f, 'Suite') === 'function';
  };
  // Mocha's own class, by its shape, as it loads.
  const load = Module._load;
  Module._load = carry(function hushrunLoad() {
    const out = load.apply(this, arguments);
    if (!S.mocha && typeof out === 'function' && isMocha(out)) guard('patch Mocha', () => patchMocha(out));
    return out;
  }, load);
  try {
    Module.syncBuiltinESMExports();
  } catch (err) {
    note('syncBuiltinESMExports', err);
  }

  // ---------------------------------------------------------------- Mocha
  function patchMocha(Mocha) {
    S.mocha = Mocha;
    // [C-32] One start of Mocha writes for the run: one inside a test, or a second in COMMAND,
    // says so once, and keeps every pass of the run out.
    let claimed = true;
    try {
      own.closeSync(own.openSync(path.join(dir, 'mocha-claim'), 'wx'));
    } catch {
      claimed = false;
    }
    env.HUSHRUN_MOCHA_IN = String(process.pid);
    const M = {
      claimed,
      testFiles: null, // from the root, as Mocha collected them, `--file`'s left out
      always: new Set(), // `--file`'s, and every file that defines a root hook
      refused: null,
      mayLeaveOut: false,
      records: new Map(), // test object -> record
      order: [],
      leftOut: new Set(),
      unsure: null,
      rootFailed: false,
      ended: false,
      written: false,
    };
    S.M = M;
    const loading = (original) => carry(function (...args) {
      guard('list spec files', () => listFiles(this));
      if (M.claimed) {
        guard('windows', () => {
          const suite = this.suite;
          suite.on('pre-require', (context, file) => {
            window = rel(file);
          });
          suite.on('post-require', () => {
            window = null;
          });
        });
        observing = true;
      }
      return original.apply(this, args);
    }, original);
    for (const name of ['loadFiles', 'loadFilesAsync']) {
      const original = Mocha.prototype[name];
      if (typeof original === 'function') Mocha.prototype[name] = loading(original);
    }
    // A root hook a spec file defines runs for every test of every spec file: that file is read
    // by all of them, and never left out.
    for (const name of ['beforeAll', 'beforeEach', 'afterAll', 'afterEach']) {
      const original = Mocha.Suite.prototype[name];
      if (typeof original !== 'function') continue;
      Mocha.Suite.prototype[name] = carry(function (...args) {
        if (this.root && window) M.always.add(window);
        return original.apply(this, args);
      }, original);
    }
    const runRunner = Mocha.Runner.prototype.run;
    Mocha.Runner.prototype.run = carry(function (...args) {
      if (M.claimed && !M.runner) {
        M.runner = this;
        guard('runner', () => startRunner(this));
      }
      return runRunner.apply(this, args);
    }, runRunner);
    process.on('exit', () => guard('write', () => writeFacts()));
  }

  function listFiles(mocha) {
    const M = S.M;
    if (M.testFiles) return;
    const options = mocha.options || {};
    if (!M.claimed) {
      guard('second start', () => writeWhole(`mocha-second-${process.pid}.started`, {
        why: 'Mocha started more than once in its command, and hushrun observes only the first start: what another start ran can\'t be told',
      }));
      M.testFiles = [];
      return;
    }
    if (options.parallel) {
      M.refused = 'Mocha runs spec files in worker processes (--parallel), which hushrun\'s adapter doesn\'t observe';
      S.errors.push(M.refused);
    }
    if (options.dryRun) S.errors.push('mocha --dry-run reports tests it doesn\'t run');
    if (S.noHooks) S.errors.push(S.noHooks);
    const fileOpt = new Set((Array.isArray(options.file) ? options.file : options.file ? [options.file] : [])
      .map((f) => rel(path.resolve(f))).filter(Boolean));
    for (const f of fileOpt) M.always.add(f);
    const files = (mocha.files || []).map((f) => ({ f, r: rel(path.resolve(f)) }));
    M.listed = [...new Set(files.filter((x) => x.r && !fileOpt.has(x.r)).map((x) => x.r))].sort();
    M.testFiles = M.listed;
    M.outside = files.some((x) => !x.r);
  }

  // [ADR 0107] Once every spec file loaded: its units are the spec files that define no root hook
  // (those are read by every spec file and never left out), and the plan may leave tests out only
  // when that list is hushrun.json's `testFiles`.
  function decide() {
    const M = S.M;
    M.testFiles = M.listed.filter((f) => !M.always.has(f));
    const sameList = Array.isArray(run.testFiles)
      && JSON.stringify([...run.testFiles].sort()) === JSON.stringify(M.testFiles);
    if (M.refused) {
      // already said
    } else if (!Array.isArray(run.testFiles)) {
      M.refused = null;
    } else if (!sameList) {
      M.refused = 'Mocha lists other spec files than the plan does';
    } else if (M.outside) {
      M.refused = 'a spec file Mocha lists is outside the repository';
    } else {
      M.mayLeaveOut = true;
    }
  }

  // A test's names, from its outermost suite, the root excluded.
  function namesOf(runnable) {
    const names = [];
    for (let s = runnable; s && !s.root; s = s.parent) names.unshift(String(s.title));
    return names;
  }
  function fileOf(runnable) {
    for (let s = runnable; s; s = s.parent) {
      if (s.file) return rel(s.file);
    }
    return null;
  }
  function under(test, suite) {
    for (let s = test.parent; s; s = s.parent) if (s === suite) return true;
    return false;
  }

  function startRunner(runner) {
    const M = S.M;
    window = null;
    // Every test, in declaration order, with its id: the k-th test (k from 2) of a file with an
    // already-seen joined name gets ` [k]`.
    const counts = new Map();
    const walk = (suite) => {
      for (const hook of [].concat(suite._beforeAll || [], suite._beforeEach || [], suite._afterEach || [], suite._afterAll || [])) {
        if (suite.root) {
          const f = hook.file ? rel(hook.file) : null;
          if (f) M.always.add(f);
        }
      }
      for (const test of suite.tests || []) {
        const file = fileOf(test);
        const joined = namesOf(test).join(' > ');
        const key = `${file}\0${joined}`;
        const k = (counts.get(key) || 0) + 1;
        counts.set(key, k);
        const r = { test, file, id: k > 1 ? `${joined} [${k}]` : joined, status: 'pending', durationMs: 0, codeSkip: !!test.pending };
        M.records.set(test, r);
        M.order.push(r);
      }
      for (const child of suite.suites || []) walk(child);
    };
    walk(runner.suite);
    if (M.listed) decide();
    const ids = new Map();
    for (const r of M.order) {
      const key = `${r.file}\0${r.id}`;
      ids.set(key, (ids.get(key) || 0) + 1);
    }
    for (const r of M.order) r.ambiguous = ids.get(`${r.file}\0${r.id}`) > 1;
    // Leaving out: a whole spec file `reuse` lists, and each test `reuseTests` lists, but one
    // whose id collides, out of its suite before any hook runs.
    if (M.mayLeaveOut) {
      const files = new Set(Array.isArray(run.reuse) ? run.reuse : []);
      const tests = run.unit === 'test' && run.reuseTests ? run.reuseTests : {};
      const remove = new Set();
      for (const r of M.order) {
        if (!r.file || M.always.has(r.file)) continue;
        const listedTests = Array.isArray(tests[r.file]) ? tests[r.file] : [];
        if (files.has(r.file) || (!r.ambiguous && listedTests.includes(r.id))) {
          remove.add(r.test);
          r.status = 'left-out';
        }
      }
      if (remove.size > 0) {
        const prune = (suite) => {
          suite.tests = (suite.tests || []).filter((t) => !remove.has(t));
          for (const child of suite.suites || []) prune(child);
        };
        prune(runner.suite);
        for (const r of M.order) if (r.status === 'left-out' && r.file) M.leftOut.add(r.file);
      }
    }
    // The outcomes, from the runner's events.
    runner.on('hook', (hook) => {
      const parent = hook.parent;
      window = parent && parent.root ? null : fileOf(hook);
    });
    // Mocha says a test began before its `beforeEach` hooks run: after a hook, the window is the
    // running test's again.
    let current = null;
    runner.on('hook end', () => {
      window = current ? fileOf(current) : null;
    });
    runner.on('test', (test) => {
      current = test;
      window = fileOf(test);
      const r = M.records.get(test);
      if (r) r.status = 'started';
    });
    runner.on('test end', (test) => {
      current = null;
      window = null;
      const r = M.records.get(test);
      if (r && typeof test.duration === 'number') r.durationMs = Math.round(test.duration);
    });
    runner.on('pass', (test) => {
      const r = M.records.get(test);
      if (r && r.status !== 'failed') r.status = 'passed';
    });
    runner.on('pending', (test) => {
      const r = M.records.get(test);
      if (r) r.status = 'skipped';
    });
    runner.on('fail', (runnable, err) => {
      if (err && err.uncaught && !M.unsure) {
        const first = String(err.message || err).split('\n')[0];
        M.unsure = `Mocha reported an uncaught error${first.length <= 120 ? ` (${first})` : ''}, which it may charge to a test that didn't raise it, so no test keeps a result of that run`;
      }
      const r = M.records.get(runnable);
      if (r) {
        r.status = 'failed';
        return;
      }
      if (runnable && runnable.type === 'hook' && runnable.parent) {
        // A hook failed: every test under its suite failed, a root hook's being every test's.
        if (runnable.parent.root) M.rootFailed = true;
        for (const t of M.order) if (under(t.test, runnable.parent)) t.status = 'failed';
        return;
      }
      if (!M.unsure) M.unsure = 'Mocha reported a failure outside any test, so no test keeps a result of that run';
    });
    runner.on('end', () => {
      M.ended = true;
      window = null;
      observing = false;
      guard('write', () => writeFacts());
    });
  }

  // Every file `start` reaches through the module graph and what its modules opened.
  function closure(start, imports) {
    const out = new Set(start);
    const queue = [...start];
    while (queue.length > 0) {
      const f = queue.pop();
      for (const next of [...(imports.get(f) || []), ...(opens.get(f) || [])]) {
        if (!out.has(next)) {
          out.add(next);
          queue.push(next);
        }
      }
    }
    return out;
  }
  // Each repository module's direct imports: its `children`, as Node keeps them for every module
  // it required, cached ones included, and the module hooks' resolutions.
  function moduleGraph() {
    const imports = new Map();
    const add = (from, to) => {
      if (!from || !to || from === to) return;
      if (!imports.has(from)) imports.set(from, new Set());
      imports.get(from).add(to);
    };
    const seen = new Set();
    const visit = (m) => {
      if (!m || seen.has(m)) return;
      seen.add(m);
      const from = rel(m.filename);
      for (const child of m.children || []) {
        add(from, rel(child.filename));
        visit(child);
      }
    };
    for (const m of Object.values(require.cache)) visit(m);
    for (const edge of esmEdges) {
      const [from, to] = edge.split('\0');
      add(from, to);
    }
    return imports;
  }

  function writeFacts() {
    const M = S.M;
    if (!M || M.written || !M.claimed || !M.testFiles) return;
    M.written = true;
    const imports = moduleGraph();
    // Mocha looks at every spec file as it collects them, outside every window: that is no spec
    // file's read of another's code. A root hook's file is read by all.
    const units = new Set(M.testFiles);
    const shared = closure([...every.reads].filter((f) => !units.has(f)).concat([...M.always]), imports);
    for (const f of units) shared.delete(f);
    const sharedDirs = new Set(every.dirs);
    const byUnit = new Map();
    for (const r of M.order) {
      if (!r.file || M.always.has(r.file)) continue;
      if (!byUnit.has(r.file)) byUnit.set(r.file, []);
      byUnit.get(r.file).push(r);
    }
    for (const f of M.testFiles) if (!byUnit.has(f)) byUnit.set(f, []);
    const byTest = run.unit === 'test';
    const out = [];
    const unitNf = (b) => dedupe([...every.notFollowed, ...(b ? b.notFollowed : [])]);
    const ran = (r) => r.status === 'passed' || r.status === 'failed';
    for (const [file, records] of byUnit) {
      if (!M.testFiles.includes(file)) continue;
      const b = byFile.get(file);
      const deselected = records.length > 0 && records.every((r) => r.status === 'left-out');
      if (M.rootFailed) for (const r of records) if (r.status !== 'left-out') r.status = 'failed';
      let selected = 0;
      let finished = 0;
      for (const r of records) {
        if (ran(r)) finished += 1;
        if (ran(r) || r.status === 'pending' || r.status === 'started') selected += 1;
      }
      if (M.unsure || !M.ended) finished = 0;
      const reads = closure([file, ...(b ? b.reads : [])], imports);
      for (const s of shared) reads.add(s);
      const unit = {
        unit: file,
        selected: deselected ? 0 : selected,
        finished: deselected ? 0 : finished,
        skipped: records.filter((r) => r.status === 'skipped').length,
        failed: records.some((r) => r.status === 'failed'),
        deselected,
        durationMs: records.reduce((n, r) => n + r.durationMs, 0),
        reads: [...reads].sort(),
        dirs: [...new Set([...(b ? b.dirs : []), ...sharedDirs])].sort(),
        notFollowed: unitNf(b),
      };
      if (byTest) {
        const groups = new Map();
        for (const r of records) {
          if (!groups.has(r.id)) groups.set(r.id, []);
          groups.get(r.id).push(r);
        }
        const done = (r) => !M.unsure && M.ended && (ran(r) || r.status === 'skipped');
        unit.tests = [...groups.values()].map((rs) => {
          const r = rs[0];
          const out = {
            id: r.id,
            finished: rs.every(done),
            failed: rs.some((x) => x.status === 'failed'),
            deselected: rs.every((x) => x.status === 'left-out'),
            durationMs: rs.reduce((n, x) => n + x.durationMs, 0),
            notFollowed: [],
          };
          if (rs.some((x) => x.status === 'skipped')) out.skipped = true;
          if (rs.length > 1) out.neverLeftOut = true;
          if (M.unsure) out.notKept = M.unsure;
          return out;
        });
      }
      out.push(unit);
    }
    const notFollowed = [];
    writeWhole(`mocha-${process.pid}.json`, {
      schema: SCHEMA,
      adapter: 'mocha',
      start: `mocha-${process.pid}-${S.started}`,
      failuresByUnit: M.ended,
      settings: {
        version: process.version,
        testFiles: M.testFiles,
        always: [...M.always].sort(),
        refused: M.refused,
        complete: M.ended,
      },
      notFollowed,
      units: out,
      errors: S.errors,
      edges: [...imports].flatMap(([from, tos]) => [...tos].map((to) => [from, to, 'import'])),
      ...(M.notKept ? { notKept: M.notKept } : {}),
    });
  }
}
