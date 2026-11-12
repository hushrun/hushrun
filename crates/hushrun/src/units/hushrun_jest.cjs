'use strict';
// [ADR 0082] hushrun's Jest adapter, loaded into every Node process of COMMAND with
//
//   NODE_OPTIONS="<what it had> --require <this file>"
//
// which hushrun sets, or, where a script replaces NODE_OPTIONS, with one line at the top of the
// Jest config that `hushrun init` proposes:
//
//   process.env.HUSHRUN_JEST && require(process.env.HUSHRUN_JEST);
//
// Without HUSHRUN_UNITS and its hushrun.json (C-32) it does nothing at all. With them, it wraps
// Node's `fs`, `child_process`, `worker_threads` and `process.dlopen` (they record only while a
// test file runs), and looks at what each CommonJS load returns, by shape, to find Jest's own
// parts:
//
// - main process: jest-config's `readConfigs` (it claims the run's directory, adds a reporter and a
//   passWithNoTests that is true only when hushrun left out every test file), @jest/core's
//   `SearchSource` (every project's test files), the test sequencer's `sort` (after Jest's filters
//   and --shard, it leaves out the test files hushrun.json lists, when Jest's own list of test
//   files is hushrun.json's `testFiles`);
// - each test file, in the worker that runs it: jest-runtime's `Runtime` (the file's session, from
//   its first require to its teardown, and the direct imports it resolves), Node's `vm` (every
//   module Jest compiles for it), the test environment's `handleTestEvent` (jest-circus's events:
//   each test, its outcome, and leaving out the tests `reuseTests` lists at `run_start`), and,
//   with `reads: test`, V8's precise coverage between the events (which files' functions ran).
//
// It decides nothing: it writes what it saw, whole (a temporary name starting with `.`, then a
// rename), as `hushrun.unit-facts/v1`, `jest-main-<pid>.json` for the process that claimed the
// run and `jest-<pid>-<thread>-<n>.json` for each test file a process ran. Every hook is guarded:
// an error of its own goes in the file's `errors`, which makes what it saw `not observed`, and
// never reaches Jest. A shape it needs that isn't there observes nothing, and says so.

const KEY = Symbol.for('hushrun.jest');
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
  const REPORTER = path.join(__dirname, 'hushrun-jest-reporter.cjs');
  const dir = process.env.HUSHRUN_UNITS;
  // Our own reads and writes go through these, captured before anything is wrapped.
  const own = {
    readFileSync: fs.readFileSync,
    writeFileSync: fs.writeFileSync,
    renameSync: fs.renameSync,
    openSync: fs.openSync,
    writeSync: fs.writeSync,
    closeSync: fs.closeSync,
    existsSync: fs.existsSync,
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
  Object.assign(S, {
    dir,
    run,
    root,
    // [C-39] Report each test; and with `reads: test`, watch what each ran and opened.
    byTest: run.unit === 'test',
    watchAsked: run.unit === 'test' && run.reads === 'test',
    errors: [],
    session: null,
    written: 0,
    userRequire: 0,
    // Repository files Node itself compiled in this process: Jest's config, a transformer, an
    // environment, a resolver of the repository's. Code every later test file of the process runs.
    processFiles: new Set(),
    // Repository files read outside any test file's session, between test files: Jest reading the
    // next test file and setting up its environment. They go to the next test file of the process.
    pending: { opened: new Set(), dirs: new Set(), notFollowed: [] },
    // Whether reads outside a session are kept for the next: in a worker of the run from its start,
    // in the main process once the run starts.
    buffering: !!process.env.HUSHRUN_JEST_RUN,
    // [ADR 0082] ts-jest checks types with a language service: what a test file read can't be
    // told from what it ran.
    typeAware: false,
    tsJest: false,
    tsCompilerSeen: false,
  });

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
  const rootSlash = root.endsWith(path.sep) ? root : root + path.sep;
  const rawCache = new Map();
  const absCache = new Map();
  function rel(p) {
    if (typeof p === 'string') {
      const hit = rawCache.get(p);
      if (hit !== undefined) return hit;
      const out = relSlow(p);
      if (rawCache.size < 200000) rawCache.set(p, out);
      return out;
    }
    return relSlow(p);
  }
  function relSlow(p) {
    if (p == null) return null;
    if (typeof p === 'string' && p.startsWith(rootSlash) && !p.includes('node_modules')
      && !p.includes(`${path.sep}.`) && !p.includes(`${path.sep}${path.sep}`)) {
      return path.sep === '/' ? p.slice(rootSlash.length) : p.slice(rootSlash.length).split(path.sep).join('/');
    }
    let abs;
    try {
      if (typeof p === 'string') {
        if (p.startsWith('node:')) return null;
        abs = p.startsWith('file:') ? fileURLToPath(p) : path.resolve(p);
      } else if (p instanceof URL) abs = fileURLToPath(p);
      else if (Buffer.isBuffer(p)) abs = path.resolve(p.toString());
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
    absCache.set(abs, out);
    return out;
  }
  function writeWhole(name, value) {
    const target = path.join(dir, name);
    const temp = path.join(dir, `.${name}.${process.pid}.tmp`);
    own.writeFileSync(temp, JSON.stringify(value));
    own.renameSync(temp, target);
  }
  // Workers write only for the run whose main process claimed the directory (C-32): a Jest a
  // test starts inherits the variables, and its own main process clears this one.
  const runOf = () => process.env.HUSHRUN_JEST_RUN || '';
  // [C-32] This start of Jest, which its main process names when it claims the run: one id for it
  // and every worker of it, whatever its projects or --shard; only one start of a run writes.
  const startOf = () => (process.env.HUSHRUN_JEST_START ? { start: process.env.HUSHRUN_JEST_START } : {});
  S.rel = rel;
  S.note = note;

  // ---------------------------------------------------------------- fs, processes, addons
  // Wrapped when this file loads: graceful-fs, which Jest uses, copies fs's functions when it
  // loads, and a later wrap would miss what goes through it.
  function wrap(target, name, record) {
    const original = target[name];
    if (typeof original !== 'function' || original.__hushrun) return;
    const wrapped = function (...args) {
      if (S.session || S.window || S.buffering) {
        try {
          record(args[0], name);
        } catch (err) {
          note(`record ${name}`, err);
        }
      }
      return original.apply(this, args);
    };
    Object.defineProperty(wrapped, 'name', { value: original.name });
    Object.defineProperty(wrapped, 'length', { value: original.length });
    wrapped.__hushrun = true;
    for (const k of Object.keys(original)) wrapped[k] = original[k]; // realpathSync.native
    target[name] = wrapped;
  }
  // Where a read goes: the open test file's session, the main process's globalSetup window, or
  // the next test file this process runs.
  const sink = () => S.session || (S.window ? S.global : (S.buffering ? S.pending : null));
  const opened = (p) => {
    const r = rel(p);
    const to = r && sink();
    if (to) to.open ? to.open(r) : to.opened.add(r);
  };
  const listed = (p) => {
    const r = rel(p);
    const to = r && sink();
    if (to) to.list ? to.list(r) : to.dirs.add(r);
  };
  const unfollowed = (why, detail) => {
    const to = sink();
    if (!to) return;
    const item = { why, detail: String(detail).slice(0, 200) };
    if (to.notFollowed instanceof Function) to.notFollowed(item);
    else to.notFollowed.push(item);
  };
  for (const target of [fs, fs.promises]) {
    for (const name of ['readFile', 'readFileSync', 'open', 'openSync', 'createReadStream', 'copyFile',
      'copyFileSync', 'cp', 'cpSync', 'stat', 'statSync', 'lstat', 'lstatSync', 'access', 'accessSync',
      'existsSync', 'exists', 'realpath', 'realpathSync']) wrap(target, name, opened);
    for (const name of ['readdir', 'readdirSync', 'opendir', 'opendirSync']) wrap(target, name, listed);
  }
  const cp = require('child_process');
  for (const name of ['spawn', 'spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync', 'fork']) {
    const original = cp[name];
    if (typeof original !== 'function') continue;
    wrap(cp, name, (cmd) => unfollowed('subprocess', `${name} ${String(cmd)}`));
  }
  const wt = require('worker_threads');
  if (typeof wt.Worker === 'function' && !wt.Worker.__hushrun) {
    const OriginalWorker = wt.Worker;
    const Wrapped = class Worker extends OriginalWorker {
      constructor(...args) {
        unfollowed('worker', String(args[0]));
        super(...args);
      }
    };
    Wrapped.__hushrun = true;
    wt.Worker = Wrapped;
  }
  const dlopen = process.dlopen;
  process.dlopen = function (...args) {
    // Only an addon the repository's code required: Jest loads its own (Jest 30's resolver).
    if (S.window || (S.session && S.userRequire > 0)) unfollowed('native', String(args[1]));
    return dlopen.apply(this, args);
  };
  // What Node itself compiles from the repository: code that runs for every later test file.
  const compile = Module.prototype._compile;
  Module.prototype._compile = function hushrunCompile(content, filename) {
    guard('compile', () => {
      if (typeof filename !== 'string') return;
      if (/[\\/]ts-jest[\\/]/.test(filename)) S.tsJest = true;
      const r = rel(filename);
      if (r) {
        S.processFiles.add(r);
        if (S.window && S.global) S.global.opened.add(r);
      }
    });
    return compile.apply(this, arguments);
  };
  installVmHooks();
  try {
    Module.syncBuiltinESMExports();
  } catch (err) {
    note('syncBuiltinESMExports', err);
  }

  // ---------------------------------------------------------------- vm: modules a test file ran
  function installVmHooks() {
    const vm = require('vm');
    const loaded = (filename) => {
      if (!S.session || typeof filename !== 'string') return;
      if (!filename.startsWith('file:') && !path.isAbsolute(filename)) return; // '@jest/globals'
      const r = rel(filename);
      if (r) S.session.load(r);
    };
    if (!vm.Script.__hushrun) {
      const OriginalScript = vm.Script;
      const Wrapped = class Script extends OriginalScript {
        constructor(code, options) {
          super(code, options);
          loaded(typeof options === 'string' ? options : options && options.filename);
        }
      };
      Wrapped.__hushrun = true;
      vm.Script = Wrapped;
    }
    if (!vm.compileFunction.__hushrun) {
      const originalCompileFunction = vm.compileFunction;
      const wrapped = function compileFunction(code, params, options) {
        const fn = originalCompileFunction.apply(this, arguments);
        loaded(options && options.filename);
        return fn;
      };
      wrapped.__hushrun = true;
      vm.compileFunction = wrapped;
    }
    for (const name of ['SourceTextModule', 'SyntheticModule']) {
      const C = vm[name];
      if (typeof C !== 'function' || C.__hushrun) continue;
      const Wrapped = class extends C {
        constructor(...args) {
          super(...args);
          const options = name === 'SourceTextModule' ? args[1] : args[2];
          if (options && options.identifier) loaded(options.identifier);
        }
        link(linker) {
          if (typeof linker !== 'function') return super.link(linker);
          return super.link(async (specifier, referencing, extra) => {
            const m = await linker(specifier, referencing, extra);
            if (S.session) {
              guard('link', () => {
                const abs = (id) => (typeof id === 'string' && (id.startsWith('file:') || path.isAbsolute(id)) ? id : null);
                const from = rel(abs(referencing && referencing.identifier));
                const to = rel(abs(m && m.identifier));
                if (from && to && from !== to) {
                  S.session.edge(from, to);
                  const file = abs(referencing.identifier);
                  const p = file.startsWith('file:') ? fileURLToPath(file) : file;
                  if (S.session.watch && reexportSpecs(p).has(specifier)) S.session.reexport(from, to);
                }
              });
            }
            return m;
          });
        }
      };
      Object.defineProperty(Wrapped, 'name', { value: name });
      Wrapped.__hushrun = true;
      vm[name] = Wrapped;
    }
  }

  // ---------------------------------------------------------------- what ran, by V8's counters
  let inspectorSession = null;
  function coverageOn() {
    if (inspectorSession) return true;
    const inspector = require('inspector');
    const session = new inspector.Session();
    session.connect();
    let failed = null;
    session.post('Profiler.enable', (e) => { failed = failed || e; });
    session.post('Profiler.startPreciseCoverage', { callCount: true, detailed: false }, (e) => { failed = failed || e; });
    if (failed) throw failed;
    inspectorSession = session;
    return true;
  }
  // The repository files a function of which ran since the last take, module top-level apart:
  // that is what an import is (RFC 0004 §3.2, rule 2).
  function takeCalled() {
    if (!inspectorSession) return [];
    let result = null;
    let error = null;
    inspectorSession.post('Profiler.takePreciseCoverage', (e, r) => { error = e; result = r; });
    if (error) throw error;
    if (!result) throw new Error('takePreciseCoverage did not answer at once');
    const files = new Set();
    for (const script of result.result) {
      if (!script.url || script.url.startsWith('node:')) continue;
      const r = rel(script.url);
      if (!r || files.has(r)) continue;
      for (const f of script.functions) {
        const range = f.ranges[0];
        if (!range || range.count === 0) continue;
        // The script or module itself (vm.Script, compileFunction's body, an ES module).
        if (range.startOffset === 0 && f.functionName === '') continue;
        if (f.functionName === 'Object.<anonymous>' && range.startOffset < 200) continue; // Jest 29's wrapper
        files.add(r);
        break;
      }
    }
    return [...files];
  }

  // ---------------------------------------------------------------- why a test isn't watched
  const V8_COVERAGE = "Jest's own V8 coverage is on, and it takes the counters watching a test needs";
  const TYPE_AWARE = 'ts-jest checks types with a language service, which reads every file of code for every test file';
  // [ADR 0082, amended] The repository files whose code is on the stack, nearest first: each may
  // keep what was opened (a cache in a module above the one that read, or in the test file).
  function openers() {
    const limit = Error.stackTraceLimit;
    const prepare = Error.prepareStackTrace;
    let frames = [];
    try {
      Error.stackTraceLimit = 64;
      Error.prepareStackTrace = (_, callSites) => callSites;
      const holder = {};
      Error.captureStackTrace(holder, openers);
      frames = holder.stack || [];
    } catch {
      frames = [];
    } finally {
      Error.prepareStackTrace = prepare;
      Error.stackTraceLimit = limit;
    }
    const out = [];
    for (const frame of Array.isArray(frames) ? frames : []) {
      const name = frame && typeof frame.getFileName === 'function' ? frame.getFileName() : null;
      if (!name || name === __filename) continue;
      const r = rel(name);
      if (r && !out.includes(r)) out.push(r);
    }
    return out;
  }
  // Jest's config files, as the main process that claimed the run found them.
  function everyFromMain() {
    if (S.main) return S.main.every || [];
    try {
      const list = JSON.parse(process.env.HUSHRUN_JEST_EVERY || '[]');
      return Array.isArray(list) ? list.filter((x) => typeof x === 'string') : [];
    } catch {
      return [];
    }
  }

  // ---------------------------------------------------------------- one test file in a worker
  const bucket = (kind, describe) => ({ kind, describe, called: new Set(), opened: new Set(), loaded: new Set(), dirs: new Set(), nf: [] });
  // [RFC 0004 §4.2, §4.3] A test's names: its suites' and its own, from the outermost.
  function namesOf(test) {
    const names = [];
    for (let t = test; t && t.parent; t = t.parent) names.unshift(String(t.name));
    return names;
  }
  function describePath(block) {
    const names = [];
    for (let d = block; d && d.parent; d = d.parent) names.unshift(d.name);
    return names.join(' > ');
  }
  function errMessage(e) {
    const err = Array.isArray(e) ? e[0] : e;
    return String((err && err.message) || err).split('\n')[0].slice(0, 300);
  }
  const under = (test, block) => {
    for (let d = test.parent; d; d = d.parent) if (d === block) return true;
    return false;
  };

  class FileSession {
    constructor(testPath, runtime) {
      this.abs = testPath;
      this.file = rel(testPath);
      this.runtime = runtime;
      this.modules = new Set();
      this.reads = new Set();
      this.dirs = new Set();
      this.edges = new Map();
      // [ADR 0082, amended] Imports that re-export: by the importer's text, or its exports holding
      // the imported module's own objects (a CommonJS barrel); and what each user require returned.
      this.reexportEdges = new Map();
      this.exportsOf = new Map();
      this.nf = [];
      this.tests = new Map(); // circus test object -> record
      this.order = [];
      this.fileFailures = [];
      this.sawEvents = false;
      this.sawRunStart = false;
      this.started = false; // the test file's own module began to load
      this.setup = new Set(); // modules compiled before it: setup files and theirs
      this.leftOut = 0;
      this.concurrent = false;
      this.fileBucket = bucket('file');
      this.current = this.fileBucket;
      this.hooks = [];
      // [ADR 0082, amended] What failed outside every test: the describe blocks whose afterAll
      // failed, the errors jest-circus took as theirs, and errors it couldn't attribute (an
      // uncaught exception, an unhandled rejection: Jest 29 charges one to whatever test runs
      // when it arrives, which may not be the test that caused it).
      this.afterAllFailed = [];
      this.hookErrors = new Set();
      this.uncaught = 0;
      this.rejections = new Set();
      this.circus = null;
      const run = S.run;
      // Leave out tests only when the main process that claimed the run allowed it.
      this.mayDeselect = process.env.HUSHRUN_JEST_DESELECT === '1';
      const listed = run.reuseTests && this.file ? run.reuseTests[this.file] : null;
      this.reuse = new Set(this.mayDeselect && Array.isArray(listed) ? listed : []);
      // Whether to watch each test, as the main process said; else why not, in its words.
      const watch = process.env.HUSHRUN_JEST_WATCH || '';
      this.watch = S.watchAsked && watch === '1';
      this.unwatched = S.watchAsked && !this.watch && watch !== '0' && watch ? watch : null;
      const co = runtime && runtime._coverageOptions;
      if (this.watch && co && co.collectCoverage && co.coverageProvider === 'v8') {
        // Jest's own V8 coverage takes the same counters: watching would empty it.
        this.watch = false;
        this.unwatched = V8_COVERAGE;
      }
      // [C-40] What each module of the repository opened, kept with the module: a file read once
      // and cached reaches every test that runs the module's code.
      this.opens = new Map();
      this.coverage = false;
      if (this.watch) {
        try {
          this.coverage = coverageOn();
          takeCalled(); // what ran before this file isn't its own
        } catch (err) {
          note('coverage', err);
          this.coverage = false;
        }
      }
      this.resolves = canResolve(runtime);
      // What this process read since the last test file ended is this one's: Jest reading it and
      // setting up its environment.
      const p = S.pending;
      for (const r of p.opened) {
        this.reads.add(r);
        this.keep(r, null);
      }
      for (const r of p.dirs) this.list(r);
      this.fileBucket.nf.push(...p.notFollowed);
      S.pending = { opened: new Set(), dirs: new Set(), notFollowed: [] };
    }
    open(r) {
      this.reads.add(r);
      const whos = this.watch ? openers() : null;
      // Inside a require, held until it ends: Jest reading a module to compile it isn't the
      // test opening it.
      const frame = S.frames[S.frames.length - 1];
      if (frame) frame.opened.push([r, whos]);
      else this.keep(r, whos);
    }
    // An open that stands: the window's, and each module's whose code was on the stack, the test
    // file's included: any of them may have kept what it read for a later test that runs its code.
    keep(r, whos) {
      this.current.opened.add(r);
      for (const who of whos || []) {
        if (who === r) continue;
        if (!this.opens.has(who)) this.opens.set(who, new Set());
        this.opens.get(who).add(r);
      }
    }
    list(r) {
      this.dirs.add(r);
      this.current.dirs.add(r);
    }
    load(r) {
      this.modules.add(r);
      this.current.loaded.add(r);
      const frame = S.frames[S.frames.length - 1];
      if (frame) frame.compiled.add(r);
      if (!this.started) {
        if (r === this.file) this.started = true;
        else this.setup.add(r);
      }
    }
    edge(from, to) {
      this.edges.set(`${from}\0${to}`, [from, to]);
    }
    reexport(from, to) {
      this.reexportEdges.set(`${from}\0${to}`, [from, to]);
    }
    // [ADR 0082, amended] An import whose module's own objects reappear in the importer's exports
    // re-exports it (`module.exports = require(…)`, `exports.x = require(…).x`): a wider list is
    // safe, a narrower one isn't. Getters aren't called: that would run code of the repository's.
    reexportsByValue() {
      for (const [from, to] of this.edges.values()) {
        if (this.reexportEdges.has(`${from}\0${to}`)) continue;
        const a = this.exportsOf.get(from);
        const b = this.exportsOf.get(to);
        if (!a || !b || (typeof a !== 'object' && typeof a !== 'function')) continue;
        if (a === b) {
          this.reexport(from, to);
          continue;
        }
        const values = new Set([b]);
        for (const v of ownValues(b)) values.add(v);
        if (ownValues(a).some((v) => values.has(v))) this.reexport(from, to);
      }
    }
    notFollowed(item) {
      this.current.nf.push(item);
    }
    switchTo(next, take = true) {
      if (this.coverage && take) {
        try {
          for (const f of takeCalled()) this.current.called.add(f);
        } catch (err) {
          note('take', err);
          this.coverage = false;
        }
      }
      this.current = next || this.fileBucket;
    }
    record(test) {
      let r = this.tests.get(test);
      if (!r) {
        r = { id: null, status: 'pending', durationMs: 0, attempts: 0, b: bucket('test'), codeSkip: false, mode: null };
        this.tests.set(test, r);
      }
      return r;
    }
    // [RFC 0004 §4.3] Ids in collection order, taken at run_start before --randomize shuffles: the
    // k-th test (k from 2) of a file whose id another had gets " [k]". A test whose id still
    // collides, or whose names hold " > " so that two nestings join alike, is never left out.
    assignIds(rootBlock) {
      const all = [];
      const walk = (block, skipped) => {
        for (const child of block.children) {
          if (child.type === 'describeBlock') walk(child, skipped || child.mode === 'skip');
          else if (child.type === 'test') all.push({ test: child, names: namesOf(child), skipped: skipped || child.mode === 'skip' });
        }
      };
      walk(rootBlock, rootBlock.mode === 'skip');
      const counts = new Map();
      const nestings = new Map();
      for (const t of all) {
        t.base = t.names.join(' > ');
        const n = (counts.get(t.base) || 0) + 1;
        counts.set(t.base, n);
        t.id = n === 1 ? t.base : `${t.base} [${n}]`;
        if (!nestings.has(t.base)) nestings.set(t.base, new Set());
        nestings.get(t.base).add(JSON.stringify(t.names));
      }
      const ids = new Map();
      for (const t of all) ids.set(t.id, (ids.get(t.id) || 0) + 1);
      for (const t of all) {
        const r = this.record(t.test);
        r.id = t.id;
        r.mode = t.test.mode || null;
        r.codeSkip = t.skipped || t.test.mode === 'todo';
        r.ambiguous = ids.get(t.id) > 1 || nestings.get(t.base).size > 1;
        if (t.test.concurrent) {
          this.concurrent = true;
          r.concurrent = true;
        }
        this.order.push(t.test);
      }
      if (!this.reuse.size) return;
      for (const t of all) {
        const r = this.tests.get(t.test);
        if (this.reuse.has(r.id) && !r.ambiguous) {
          if (!t.skipped && t.test.mode !== 'todo') t.test.mode = 'skip';
          r.leftOut = true;
          this.leftOut += 1;
        }
      }
    }
    handle(event, state) {
      this.sawEvents = true;
      if (state && typeof state === 'object') this.circus = state;
      switch (event.name) {
        case 'run_start':
          this.sawRunStart = true;
          this.assignIds(state.rootDescribeBlock);
          this.switchTo(this.fileBucket);
          break;
        case 'hook_start': {
          const t = event.hook.type;
          if (t === 'beforeAll' || t === 'afterAll') {
            const b = bucket(t, event.hook.parent);
            this.hooks.push(b);
            this.switchTo(b);
          }
          break;
        }
        case 'hook_success':
        case 'hook_failure': {
          const t = event.hook.type;
          if (event.name === 'hook_failure' && (t === 'beforeAll' || t === 'afterAll')) {
            // A failing beforeAll or afterAll fails the file.
            this.fileFailures.push({ in: t, describe: describePath(event.hook.parent), message: errMessage(event.error) });
            // jest-circus fails each test under a failing beforeAll, and none under an afterAll:
            // the tests under that block failed with it, a root-level afterAll's being all.
            if (t === 'afterAll') {
              this.afterAllFailed.push(event.hook.parent || event.describeBlock || null);
              this.hookErrors.add(event.error);
            }
          }
          if (t === 'beforeAll' || t === 'afterAll') this.switchTo(this.fileBucket);
          break;
        }
        case 'test_start': {
          const r = this.record(event.test);
          r.attempts += 1;
          // No take here: since the last boundary only Jest's own code ran.
          this.switchTo(r.b, false);
          break;
        }
        case 'test_skip':
        case 'test_todo': {
          const r = this.record(event.test);
          if (r.leftOut) r.status = 'left-out';
          // Skipped by its own code (test.skip, describe.skip, test.todo) it passed, as a test
          // file of skipped tests does; by a focus or --testNamePattern it didn't run.
          else r.status = r.codeSkip || event.name === 'test_todo' ? 'skipped' : 'not-run';
          this.switchTo(this.fileBucket);
          break;
        }
        case 'test_done': {
          const r = this.record(event.test);
          r.status = event.test.errors && event.test.errors.length ? 'failed' : 'passed';
          r.durationMs = event.test.duration == null ? 0 : Math.round(event.test.duration);
          this.switchTo(this.fileBucket);
          break;
        }
        case 'error':
          // An uncaught exception or unhandled rejection: whose it is can't be told. A
          // rejection handled later (Jest 30's `error_handled`) is no error.
          if (event.promise) this.rejections.add(event.promise);
          else this.uncaught += 1;
          break;
        case 'error_handled':
          this.rejections.delete(event.promise);
          break;
        case 'run_finish':
          this.switchTo(this.fileBucket);
          for (const e of state.unhandledErrors || []) this.fileFailures.push({ in: 'unhandled', message: errMessage(e) });
          break;
        default:
      }
    }
    // [ADR 0082, amended] Each test's outcome as the file ended, not as its test_done said: an
    // error jest-circus attached to a test after its end (Jest 30 charges a late one to the test
    // whose code raised it), a failing afterAll above it, and an error no test can be charged with,
    // after which no test of the file is told as finished.
    settle() {
      const unhandled = (this.circus && this.circus.unhandledErrors) || [];
      const unattributed = this.uncaught > 0 || this.rejections.size > 0
        || unhandled.some((e) => !this.hookErrors.has(Array.isArray(e) ? e[0] : e));
      for (const [test, r] of this.tests) {
        if (r.status === 'passed') {
          if ((test.errors && test.errors.length) || this.afterAllFailed.some((b) => b && under(test, b))) r.status = 'failed';
        }
        if (unattributed) r.unsure = true;
      }
      if (unattributed) this.fileFailures.push({ in: 'unattributed', message: 'an error no test can be charged with' });
    }
    finish() {
      if (this.done) return;
      this.done = true;
      this.switchTo(this.fileBucket);
      if (!this.file || !runOf()) return;
      const unitNf = [...this.fileBucket.nf];
      for (const b of this.hooks) unitNf.push(...b.nf);
      if (!this.sawEvents) unitNf.push({ why: 'events', detail: "no jest-circus event reached hushrun's adapter (an environment it didn't wrap)" });
      if (this.sawEvents && !this.sawRunStart) this.fileFailures.push({ in: 'collection', message: 'the test file failed before its tests ran' });
      this.settle();
      // Process-level code: every test file of this process runs it.
      for (const f of S.processFiles) this.reads.add(f);
      const typeAware = S.typeAware || (S.tsJest && !S.tsCompilerSeen);
      // [RFC 0004 §3.2] What each test read is told only when every part is: its window's
      // counters, the file's direct imports, and no type checker reading everything.
      const lists = this.watch && this.coverage && this.resolves && !typeAware;
      // [C-40] Why no test of it was watched, in a sentence people read.
      let watchWhy = null;
      if (S.watchAsked && S.byTest) {
        if (typeAware) watchWhy = TYPE_AWARE;
        else if (this.unwatched) watchWhy = this.unwatched;
        else if (!this.resolves) watchWhy = "this Jest's runtime doesn't say which file a module name resolves to, which watching a test needs (Jest 29, or 30.5 and later)";
        else if (!this.coverage) watchWhy = "V8's precise coverage, which watching a test needs, wasn't available in a Jest process";
        else if (this.concurrent) watchWhy = 'test.concurrent tests run at once, so each of them takes its test file\'s list';
      }
      const records = this.order.map((t) => this.tests.get(t));
      // In a file with concurrent tests, windows overlap: what wasn't followed is the file's. And
      // where tests aren't told, what wasn't followed in one is the file's too.
      if (this.concurrent || !S.byTest) for (const r of records) unitNf.push(...r.b.nf);
      let selected = 0;
      let finished = 0;
      let failed = this.fileFailures.length > 0;
      for (const r of records) {
        const ran = r.status === 'passed' || r.status === 'failed';
        if (ran) finished += 1;
        if (ran || (r.status === 'pending' && !r.codeSkip && !r.leftOut)) selected += 1;
        if (r.status === 'failed') failed = true;
      }
      const reads = new Set([...this.modules, ...this.reads]);
      reads.add(this.file);
      const unit = {
        unit: this.file,
        selected,
        finished,
        // [C-32] Its tests that finished without their body running (test.skip, describe.skip,
        // test.todo): in a run that failed, its file records no pass.
        skipped: records.filter((r) => r.status === 'skipped').length,
        failed,
        deselected: false,
        durationMs: records.reduce((n, r) => n + (r.durationMs || 0), 0),
        reads: [...reads].sort(),
        dirs: [...this.dirs].sort(),
        notFollowed: dedupe(unitNf),
      };
      const table = [];
      const index = new Map();
      const ix = (p) => {
        let i = index.get(p);
        if (i === undefined) {
          i = table.length;
          table.push(p);
          index.set(p, i);
        }
        return i;
      };
      const ixs = (set) => [...new Set([...set].map(ix))].sort((a, b) => a - b);
      if (S.byTest) {
        const position = new Map(records.map((r, i) => [r, i]));
        // What wasn't watched outside every test's window: listed directories go to each test.
        const fileDirs = new Set(this.fileBucket.dirs);
        // [RFC 0004 §4.3] One entry per id: tests whose ids collide are told as one, never left
        // out, failed if any failed and finished only if all did, with no list of their own.
        const groups = new Map();
        for (const r of records) {
          if (!groups.has(r.id)) groups.set(r.id, []);
          groups.get(r.id).push(r);
        }
        const place = new Map([...groups.keys()].map((id, i) => [id, i]));
        const done = (r) => !r.unsure && (r.status === 'passed' || r.status === 'failed' || r.status === 'skipped');
        unit.tests = [...groups.values()].map((rs) => {
          const r = rs[0];
          if (rs.length > 1 || r.ambiguous) {
            return {
              id: r.id,
              finished: rs.every(done),
              failed: rs.some((x) => x.status === 'failed'),
              ...(rs.some((x) => x.status === 'skipped') ? { skipped: true } : {}),
              deselected: false,
              durationMs: rs.reduce((n, x) => n + (x.durationMs || 0), 0),
              notFollowed: this.concurrent ? [] : dedupe(rs.flatMap((x) => x.b.nf)),
              neverLeftOut: true,
            };
          }
          const out = {
            id: r.id,
            finished: done(r),
            failed: r.status === 'failed',
            deselected: r.status === 'left-out',
            durationMs: r.durationMs || 0,
            notFollowed: this.concurrent ? [] : dedupe(r.b.nf),
          };
          if (r.attempts > 1) out.attempts = r.attempts;
          if (r.status === 'skipped') out.skipped = true;
          if (lists && !r.concurrent && (r.status === 'passed' || r.status === 'failed')) {
            const dirs = new Set([...r.b.dirs, ...fileDirs]);
            for (const b of this.hooks) if (under(this.order[position.get(r)], b.describe)) for (const d of b.dirs) dirs.add(d);
            // A module compiled in its window ran for it: its top level, at least.
            out.ran = ixs(new Set([...r.b.called, ...r.b.loaded]));
            out.opened = ixs(r.b.opened);
            out.dirs = ixs(dirs);
          }
          return out;
        });
        if (lists) {
          // What collection called and opened; what the runner loads for every test of the
          // file: its setup files, its process's own code and Jest's config files.
          unit.loaded = { ran: ixs(this.fileBucket.called), opened: ixs(this.fileBucket.opened) };
          unit.everyTest = ixs(new Set([...this.setup, ...S.processFiles, ...everyFromMain()]));
          unit.shared = this.hooks.map((b) => ({
            tests: [...new Set(records
              .filter((r, i) => (r.status === 'passed' || r.status === 'failed') && !r.ambiguous && groups.get(r.id).length === 1 && under(this.order[i], b.describe))
              .map((r) => place.get(r.id)))].sort((a, c) => a - c),
            ran: ixs(new Set([...b.called, ...b.loaded])),
            opened: ixs(b.opened),
          })).filter((x) => x.tests.length);
        }
      }
      const imports = {};
      if (lists) {
        for (const [from, to] of this.edges.values()) {
          const k = String(ix(from));
          (imports[k] = imports[k] || []).push(ix(to));
        }
        for (const k of Object.keys(imports)) imports[k] = [...new Set(imports[k])].sort((a, b) => a - b);
      }
      const opens = {};
      if (lists) for (const [who, set] of this.opens) opens[String(ix(who))] = ixs(set);
      const reexports = {};
      if (lists) {
        guard('reexports', () => this.reexportsByValue());
        for (const [from, to] of this.reexportEdges.values()) {
          const k = String(ix(from));
          (reexports[k] = reexports[k] || []).push(ix(to));
        }
        for (const k of Object.keys(reexports)) reexports[k] = [...new Set(reexports[k])].sort((a, b) => a - b);
      }
      this.exportsOf.clear();
      S.written += 1;
      writeWhole(`jest-${process.pid}-${wt.threadId}-${S.written}.json`, {
        schema: SCHEMA,
        adapter: 'jest',
        ...startOf(),
        failuresByUnit: true,
        settings: {
          typeAware,
          watched: lists,
          ...(watchWhy ? { watch: watchWhy } : {}),
          leftOut: this.leftOut,
          fileFailures: this.fileFailures,
        },
        units: [unit],
        paths: table,
        imports,
        reexports,
        opens,
        edges: [...this.edges.values()].map(([from, to]) => [from, to, 'import']),
        removedItems: this.leftOut,
        errors: S.errors.splice(0),
      });
    }
  }
  function dedupe(items) {
    const seen = new Map();
    for (const i of items) seen.set(`${i.why}\0${i.detail}`, i);
    return [...seen.values()];
  }
  // The objects and functions an exports object holds as plain values, its getters uncalled.
  function ownValues(exp) {
    const out = [];
    let keys = [];
    try {
      keys = Object.getOwnPropertyNames(exp).slice(0, 5000);
    } catch {
      return out;
    }
    for (const k of keys) {
      try {
        const d = Object.getOwnPropertyDescriptor(exp, k);
        const v = d && 'value' in d ? d.value : undefined;
        if (v && (typeof v === 'object' || typeof v === 'function')) out.push(v);
      } catch {
        // A property that can't be looked at says nothing.
      }
    }
    return out;
  }
  // [ADR 0082, amended] The specifiers a repository file re-exports from, by its own text, before
  // any transform: `export * from`, `export { x } from`, and CommonJS's `module.exports =
  // require(…)`, `__exportStar(require(…))`, `...require(…)` and `Object.assign(exports, require(…))`.
  // Text that only looks like one (a string, a comment) only widens a test's list.
  const REEXPORT_TEXT = [
    /export\s*(?:type\s+)?(?:\*(?:\s*as\s+[\w$]+)?|\{[^}]*\})\s*from\s*(['"])([^'"]+)\1/g,
    /module\.exports\s*=\s*require\(\s*(['"])([^'"]+)\1\s*\)/g,
    /__exportStar\(\s*require\(\s*(['"])([^'"]+)\1/g,
    /\.\.\.\s*require\(\s*(['"])([^'"]+)\1/g,
  ];
  const ASSIGN_TEXT = /Object\.assign\(\s*(?:module\.)?exports\s*,([^;]*)/g;
  const REQUIRE_TEXT = /require\(\s*(['"])([^'"]+)\1/g;
  const specsCache = new Map();
  function reexportSpecs(abs) {
    let out = specsCache.get(abs);
    if (out) return out;
    out = new Set();
    let text = '';
    try {
      text = own.readFileSync(abs, 'utf8');
    } catch {
      text = '';
    }
    if (text.length < 4 << 20) {
      for (const re of REEXPORT_TEXT) for (const m of text.matchAll(re)) out.add(m[2]);
      for (const m of text.matchAll(ASSIGN_TEXT)) for (const r of m[1].matchAll(REQUIRE_TEXT)) out.add(r[2]);
    }
    if (specsCache.size < 50000) specsCache.set(abs, out);
    return out;
  }
  S.reexportSpecs = reexportSpecs;

  // ---------------------------------------------------------------- Jest's parts, found by shape
  const ENV_DONE = Symbol.for('hushrun.jest.env');
  const SESSION_KEY = Symbol.for('hushrun.jest.session');
  const SEEN_EVENT = Symbol.for('hushrun.jest.event');
  function patchEnvironment(Env) {
    const proto = Env.prototype;
    if (Object.prototype.hasOwnProperty.call(proto, ENV_DONE)) return;
    proto[ENV_DONE] = true;
    const own = Object.prototype.hasOwnProperty.call(proto, 'handleTestEvent') ? proto.handleTestEvent : null;
    const inherited = own ? null : proto.handleTestEvent;
    proto.handleTestEvent = function hushrunHandleTestEvent(event, state) {
      if (event && typeof event === 'object' && !event[SEEN_EVENT]) {
        try {
          Object.defineProperty(event, SEEN_EVENT, { value: true });
          const session = this[SESSION_KEY] || S.session;
          if (session) session.handle(event, state);
        } catch (err) {
          note(`event ${event && event.name}`, err);
        }
      }
      const next = own || inherited;
      return next ? next.call(this, event, state) : undefined;
    };
  }
  const RT_DONE = Symbol.for('hushrun.jest.runtime');
  function sessionFor(rt) {
    if (rt[SESSION_KEY]) return rt[SESSION_KEY];
    if (S.session && S.session.runtime !== rt && !S.session.done) {
      S.session.nf.push({ why: 'overlap', detail: 'another test file began before it ended' });
      S.session.fileBucket.nf.push({ why: 'overlap', detail: 'another test file began before it ended' });
      S.session.finish();
    }
    const s = new FileSession(rt._testPath, rt);
    rt[SESSION_KEY] = s;
    if (rt._environment) rt._environment[SESSION_KEY] = s;
    S.session = s;
    S.frames = [];
    return s;
  }
  // [ADR 0082] How a runtime resolves a module name, for the direct imports: Jest 30.5's
  // `_resolution.resolveCjs`, Jest 29's `_resolveCjsModule`. Where neither is there, imports
  // can't be told, and no test's list is sent.
  function canResolve(rt) {
    return !!rt && ((rt._resolution && typeof rt._resolution.resolveCjs === 'function') || typeof rt._resolveCjsModule === 'function');
  }
  const edgeCache = new Map();
  function resolveEdge(rt, from, name) {
    const key = `${from}\0${name}`;
    if (edgeCache.has(key)) return edgeCache.get(key);
    let out = null;
    if (typeof name === 'string' && !name.startsWith('node:')) {
      try {
        let r = null;
        if (rt._resolution && typeof rt._resolution.resolveCjs === 'function') r = rt._resolution.resolveCjs(from, name);
        else if (typeof rt._resolveCjsModule === 'function') r = rt._resolveCjsModule(from, name);
        out = typeof r === 'string' && path.isAbsolute(r) ? r : null; // a core module resolves to its name
      } catch {
        out = null;
      }
    }
    if (edgeCache.size < 100000) edgeCache.set(key, out);
    return out;
  }
  // A manual mock that a require of `name` from `from` gets instead of the module.
  function mockEdges(rt, from, name, resolved) {
    const out = [];
    try {
      const r = rt._resolver;
      if (r && typeof r.getMockModule === 'function') {
        const m = r.getMockModule(from, name);
        if (typeof m === 'string') out.push(m);
      }
    } catch {
      // No manual mock by name.
    }
    if (resolved) {
      const beside = path.join(path.dirname(resolved), '__mocks__', path.basename(resolved));
      if (own.existsSync(beside)) out.push(beside);
    }
    return out;
  }
  // A require ended: what it opened goes to the require around it, or to the window, but the
  // modules it compiled, which Jest read to compile them.
  S.frames = [];
  function endFrame(frame) {
    const at = S.frames.lastIndexOf(frame);
    if (at < 0) return;
    S.frames.splice(at);
    const kept = frame.opened.filter(([r]) => !frame.compiled.has(r));
    const parent = S.frames[S.frames.length - 1];
    if (parent) {
      // Jest 29 reads a module in the require around the one that compiles it.
      for (const r of frame.compiled) parent.compiled.add(r);
      parent.opened.push(...kept);
    } else if (S.session) for (const [r, whos] of kept) S.session.keep(r, whos);
  }
  function patchRuntime(Runtime) {
    const proto = Runtime.prototype;
    if (Object.prototype.hasOwnProperty.call(proto, RT_DONE)) return;
    proto[RT_DONE] = true;
    for (const name of ['requireModule', 'requireInternalModule', 'unstable_importModule', 'requireModuleOrMock', 'requireMock']) {
      const original = proto[name];
      if (typeof original !== 'function') continue;
      const user = name === 'requireModuleOrMock' || name === 'unstable_importModule' || name === 'requireMock';
      proto[name] = function (from, moduleName, ...rest) {
        let exported = null;
        if (this._testPath && runOf()) {
          guard('session', () => {
            const s = sessionFor(this);
            if (name !== 'requireInternalModule' && typeof moduleName === 'string') {
              const f = rel(from);
              if (f) {
                const target = resolveEdge(this, from, moduleName);
                const to = rel(target);
                if (to && to !== f) {
                  s.edge(f, to);
                  if (s.watch) {
                    if (reexportSpecs(from).has(moduleName)) s.reexport(f, to);
                    if (name === 'requireModuleOrMock') exported = to;
                  }
                }
                if (name !== 'requireModule') {
                  for (const m of mockEdges(this, from, moduleName, target)) {
                    const mr = rel(m);
                    if (mr && mr !== f) s.edge(f, mr);
                  }
                }
              }
            }
          });
        }
        const frame = S.session ? { opened: [], compiled: new Set() } : null;
        if (frame) S.frames.push(frame);
        if (user) S.userRequire += 1;
        let out;
        try {
          out = original.call(this, from, moduleName, ...rest);
        } catch (err) {
          if (user) S.userRequire -= 1;
          if (frame) endFrame(frame);
          throw err;
        }
        if (frame) endFrame(frame);
        if (exported && S.session && S.session.exportsOf.size < 100000) S.session.exportsOf.set(exported, out);
        if (!user) return out;
        if (out && typeof out.then === 'function' && name === 'unstable_importModule') {
          return out.finally(() => { S.userRequire -= 1; });
        }
        S.userRequire -= 1;
        return out;
      };
    }
    const teardown = proto.teardown;
    if (typeof teardown === 'function') {
      proto.teardown = function (...args) {
        guard('finish', () => {
          const s = this[SESSION_KEY];
          if (s) {
            if (S.session === s) S.session = null;
            s.finish();
          }
        });
        return teardown.apply(this, args);
      };
    }
  }
  // [ADR 0082] ts-jest's compiler: a language service means type checking.
  function patchTsCompiler(C) {
    const proto = C.prototype;
    if (proto.__hushrun) return;
    proto.__hushrun = true;
    S.tsCompilerSeen = true;
    const create = proto._createLanguageService;
    proto._createLanguageService = function (...args) {
      S.typeAware = true;
      return create.apply(this, args);
    };
  }

  // ---------------------------------------------------------------- the main process
  S.main = null;
  function claimMain() {
    if (S.main) return true;
    const file = path.join(dir, 'jest-claim');
    try {
      const fd = own.openSync(file, 'wx');
      own.writeSync(fd, String(process.pid));
      own.closeSync(fd);
    } catch {
      try {
        if (own.readFileSync(file, 'utf8').trim() !== String(process.pid)) return false;
      } catch {
        return false;
      }
    }
    S.main = {
      refused: null,
      notes: [],
      testFiles: null,
      selected: null,
      removed: new Set(),
      results: new Map(),
      allLeftOut: false,
      settings: {},
      written: false,
    };
    // What the globalSetup window reads and starts is every test file's (C-32).
    S.global = { opened: new Set(), dirs: new Set(), notFollowed: [] };
    process.env.HUSHRUN_JEST_RUN = String(process.pid);
    process.env.HUSHRUN_JEST_START = `jest-${process.pid}-${S.started}`;
    process.env.HUSHRUN_JEST_DESELECT = '0';
    process.env.HUSHRUN_JEST_WATCH = '0';
    process.env.HUSHRUN_JEST_EVERY = '[]';
    S.buffering = false;
    process.on('exit', () => guard('exit', () => S.writeMain(null)));
    return true;
  }
  function notClaimed() {
    // A Jest started inside a test file of the run: neither it nor its workers write.
    delete process.env.HUSHRUN_JEST_RUN;
    delete process.env.HUSHRUN_JEST_START;
    delete process.env.HUSHRUN_JEST_EVERY;
    process.env.HUSHRUN_JEST_DESELECT = '0';
    S.buffering = false;
  }
  function patchConfigModule(cfg) {
    if (cfg.readConfigs.__hushrun) return;
    const readConfigs = cfg.readConfigs;
    const wrapped = async function readConfigsHushrun(argv) {
      if (argv && typeof argv.config === 'string') S.argvConfig = argv.config;
      const out = await readConfigs.apply(this, arguments);
      if (!claimMain()) {
        notClaimed();
        return out;
      }
      return guard('readConfigs', () => adjustConfigs(out, argv)) || out;
    };
    wrapped.__hushrun = true;
    try {
      cfg.readConfigs = wrapped;
    } catch (err) {
      note('readConfigs', err);
    }
  }
  function adjustConfigs(out) {
    const g = out.globalConfig;
    const copy = { ...g };
    assess(g, out.configs);
    addToGlobal(copy, g);
    return { ...out, globalConfig: Object.freeze(copy) };
  }
  // Adds hushrun's reporter, and a passWithNoTests that is true only when the user set it or
  // hushrun left out every test file Jest chose.
  function addToGlobal(target, g) {
    const reporters = g.reporters && g.reporters.length ? [...g.reporters] : [['default', {}]];
    reporters.push([REPORTER, {}]);
    target.reporters = reporters;
    const userPass = g.passWithNoTests;
    Object.defineProperty(target, 'passWithNoTests', {
      enumerable: true,
      configurable: true,
      get: () => !!(userPass || (S.main && S.main.allLeftOut)),
    });
  }
  function assess(g, configs) {
    const M = S.main;
    const issues = [];
    const version = String(S.version || '');
    const major = Number(version.split('.')[0]);
    if (major !== 29 && major !== 30) {
      note('version', version ? `this Jest, ${version}, isn't one hushrun's adapter knows (29, 30)` : "hushrun's adapter can't tell which Jest this is");
    }
    if (g.watch || g.watchAll) issues.push('Jest watches files in this run');
    if (g.onlyFailures) issues.push('--onlyFailures chooses test files from an earlier run');
    if (g.onlyChanged || g.lastCommit || g.changedSince) issues.push('--onlyChanged chooses test files from git');
    if (g.listTests) issues.push('--listTests runs no test file');
    if (g.collectCoverage) issues.push("Jest's coverage is on, and a partial run's coverage would read as a drop");
    for (const c of configs) {
      if (!/jest-circus/.test(String(c.testRunner))) {
        // [ADR 0082] Without jest-circus's events, nothing of a test file is told.
        S.global.notFollowed.push({ why: 'runner', detail: String(c.testRunner).slice(0, 200) });
      }
    }
    M.settings = {
      watch: !!(g.watch || g.watchAll),
      shard: g.shard || null,
      coverage: !!g.collectCoverage,
      coverageProvider: g.coverageProvider || null,
      projects: configs.length,
    };
    if (issues.length) M.refused = issues.join('; ');
    const v8 = g.collectCoverage && g.coverageProvider === 'v8';
    const runnerOk = configs.every((c) => /jest-circus/.test(String(c.testRunner)));
    process.env.HUSHRUN_JEST_WATCH = !S.watchAsked || !runnerOk ? '0' : v8 ? V8_COVERAGE : '1';
    S.rootDirs = configs.map((c) => c.rootDir).filter((d) => typeof d === 'string');
  }
  const SEQ_DONE = Symbol.for('hushrun.jest.sequencer');
  function patchSequencer(Seq) {
    const proto = Seq.prototype;
    if (Object.prototype.hasOwnProperty.call(proto, SEQ_DONE)) return;
    proto[SEQ_DONE] = true;
    const sortOf = proto.sort;
    proto.sort = async function (tests) {
      if (S.restoreFreeze) {
        S.restoreFreeze();
        if (S.lateAssess) guard('late assess', S.lateAssess);
        S.lateAssess = null;
      }
      const sorted = await sortOf.call(this, tests);
      if (!S.main || S.main.sorted) return sorted;
      S.main.sorted = true;
      const kept = (await guardAsync('sort', () => deselect(sorted))) || sorted;
      // From here until the reporter hears the run start, the main process runs globalSetup.
      S.window = true;
      return kept;
    };
  }
  async function guardAsync(what, fn) {
    try {
      return await fn();
    } catch (err) {
      note(what, err);
      return undefined;
    }
  }
  // Every test file of every project, by Jest's own matcher, with no filter.
  function allTestFiles() {
    const files = new Set();
    for (const context of S.contexts || []) {
      const ss = new S.SearchSource(context);
      for (const f of context.hasteFS.getAllFiles()) if (ss.isTestFilePath(f)) files.add(f);
    }
    return files;
  }
  function deselect(sorted) {
    const M = S.main;
    if (S.SearchSource && S.contexts && S.contexts.size) {
      M.testFiles = [...allTestFiles()].map((f) => rel(f)).filter(Boolean).sort();
    }
    M.selected = sorted.map((t) => rel(t.path) || t.path);
    const run = S.run;
    const reuse = new Set(Array.isArray(run.reuse) ? run.reuse : []);
    const reuseTests = run.reuseTests && typeof run.reuseTests === 'object' ? Object.keys(run.reuseTests) : [];
    if (!reuse.size && !reuseTests.length) return sorted;
    if (M.refused) return sorted;
    const planned = Array.isArray(run.testFiles) ? [...run.testFiles].sort() : null;
    const mine = M.testFiles;
    if (!planned || !mine || planned.length !== mine.length || planned.some((f, i) => f !== mine[i])) {
      M.refused = "Jest's test files aren't the ones hushrun planned from";
      return sorted;
    }
    process.env.HUSHRUN_JEST_DESELECT = '1';
    const kept = sorted.filter((t) => {
      const r = rel(t.path);
      if (r && reuse.has(r)) {
        M.removed.add(r);
        return false;
      }
      return true;
    });
    if (kept.length === 0 && sorted.length > 0) M.allLeftOut = true;
    return kept;
  }
  // Jest's config files: what the main process compiled and its globalSetup read, Jest's config
  // by its names in each project's root, and --config's file.
  function configFiles() {
    const out = new Set(S.processFiles);
    for (const f of S.global.opened) out.add(f);
    const names = ['jest.config.js', 'jest.config.ts', 'jest.config.mjs', 'jest.config.cjs', 'jest.config.mts', 'jest.config.cts'];
    for (const d of S.rootDirs || []) for (const n of names) if (own.existsSync(path.join(d, n))) out.add(rel(path.join(d, n)));
    if (S.argvConfig && own.existsSync(path.resolve(S.argvConfig))) out.add(rel(path.resolve(S.argvConfig)));
    out.delete(null);
    return [...out].sort();
  }
  S.runStarted = function runStarted() {
    S.window = false;
    guard('config files', () => {
      S.main.every = configFiles();
      process.env.HUSHRUN_JEST_EVERY = JSON.stringify(S.main.every);
    });
    // In band, this process runs the test files from here.
    S.buffering = true;
  };
  S.fileResult = function fileResult(test, result, globalConfig) {
    const M = S.main;
    if (!M) return;
    const file = rel(test.path);
    if (!file) return;
    const cfg = test.context && test.context.config;
    const project = cfg ? (cfg.displayName && cfg.displayName.name) || cfg.id || '' : '';
    const results = result.testResults || [];
    const ran = results.filter((t) => t.status === 'passed' || t.status === 'failed').length;
    // Obsolete snapshots of a file fail the run unless they are being updated.
    const updating = globalConfig && globalConfig.updateSnapshot === 'all';
    const obsolete = !updating && result.snapshot && result.snapshot.unchecked > 0;
    M.results.set(`${project}\0${file}`, {
      unit: file,
      selected: ran,
      finished: ran,
      failed: !!(result.testExecError || result.numFailingTests > 0 || result.failureMessage || obsolete),
      deselected: false,
      durationMs: result.perfStats && result.perfStats.end ? Math.max(0, result.perfStats.end - result.perfStats.start) : 0,
    });
  };
  S.writeMain = function writeMain(results) {
    const M = S.main;
    if (!M || M.written) return;
    M.written = true;
    const units = [...M.results.values()];
    for (const file of M.removed) units.push({ unit: file, selected: 0, finished: 0, failed: false, deselected: true, durationMs: 0 });
    const files = guard('config files', configFiles) || [];
    writeWhole(`jest-main-${process.pid}.json`, {
      schema: SCHEMA,
      adapter: 'jest-main',
      ...startOf(),
      failuresByUnit: !!results && !results.wasInterrupted && !results.runExecError,
      settings: {
        version: String(S.version || ''),
        testFiles: M.testFiles,
        selected: M.selected,
        refused: M.refused,
        configFiles: files,
        dirs: [...S.global.dirs].sort(),
        complete: !!results,
        ...M.settings,
      },
      notFollowed: dedupe(S.global.notFollowed),
      units,
      removedItems: M.removed.size,
      errors: S.errors.splice(0),
    });
  };

  // Module loading: look at what each CommonJS load returns, by shape.
  const seen = new WeakSet();
  const isClass = (v) => typeof v === 'function' && v.prototype;
  function inspect(exp) {
    if (!exp || (typeof exp !== 'object' && typeof exp !== 'function') || seen.has(exp)) return;
    seen.add(exp);
    const candidates = [exp];
    for (const k of ['default', 'TestEnvironment', 'TsCompiler']) {
      const d = Object.getOwnPropertyDescriptor(exp, k);
      if (d && 'value' in d && d.value) candidates.push(d.value);
    }
    for (const c of candidates) {
      if (!isClass(c)) continue;
      const p = c.prototype;
      if (typeof p.getVmContext === 'function' && typeof p.setup === 'function' && typeof p.teardown === 'function') {
        patchEnvironment(c);
      } else if (typeof p.requireModuleOrMock === 'function' && typeof p.teardown === 'function') {
        patchRuntime(c);
      } else if (typeof p.sort === 'function' && typeof p.cacheResults === 'function') {
        patchSequencer(c);
      } else if (typeof p._createLanguageService === 'function') {
        patchTsCompiler(c);
      }
    }
    if (typeof exp === 'object') {
      const rc = Object.getOwnPropertyDescriptor(exp, 'readConfigs');
      if (rc && typeof rc.value === 'function' && typeof exp.normalize === 'function') patchConfigModule(exp);
      const hasCore = Object.getOwnPropertyDescriptor(exp, 'SearchSource') && Object.getOwnPropertyDescriptor(exp, 'runCLI');
      if (hasCore && isClass(exp.SearchSource) && typeof exp.runCLI === 'function') patchCore(exp);
    }
  }
  function patchCore(core) {
    S.SearchSource = core.SearchSource;
    if (typeof core.getVersion === 'function') guard('version', () => { S.version = core.getVersion(); });
    const proto = core.SearchSource.prototype;
    if (proto.__hushrun) return;
    proto.__hushrun = true;
    S.contexts = S.contexts || new Set();
    const getTestPaths = proto.getTestPaths;
    if (typeof getTestPaths === 'function') {
      proto.getTestPaths = function (...args) {
        guard('contexts', () => {
          if (this._context && this._context.hasteFS) S.contexts.add(this._context);
        });
        return getTestPaths.apply(this, args);
      };
    }
  }
  const load = Module._load;
  Module._load = function hushrunLoad() {
    const exp = load.apply(this, arguments);
    try {
      inspect(exp);
    } catch (err) {
      note('inspect', err);
    }
    return exp;
  };

  // The config line's way in: loaded by Jest's config, inside readConfigs, after jest-config and
  // @jest/core loaded. It catches the configurations as Jest freezes them, and passes this file
  // to the workers Jest starts later through NODE_OPTIONS.
  const late = Object.keys(require.cache).some((k) => /[\\/]jest-config[\\/]build[\\/]/.test(k));
  S.late = late;
  if (late) {
    for (const k of Object.keys(require.cache)) {
      if (/[\\/]@jest[\\/]core[\\/]build[\\/]index\.js$/.test(k)) guard('late core', () => inspect(require.cache[k].exports));
    }
    const quoted = /[\s"\\]/.test(__filename) ? `"${__filename.replace(/(["\\])/g, '\\$1')}"` : __filename;
    if (!String(process.env.NODE_OPTIONS || '').includes(__filename)) {
      process.env.NODE_OPTIONS = `${process.env.NODE_OPTIONS || ''} --require ${quoted}`.trim();
    }
    const freeze = Object.freeze;
    const projects = [];
    S.restoreFreeze = () => {
      if (Object.freeze !== freeze) Object.freeze = freeze;
    };
    Object.freeze = function hushrunFreeze(o) {
      if (o && typeof o === 'object' && !Array.isArray(o)) {
        guard('late config', () => {
          if ('testSequencer' in o && 'maxWorkers' in o && 'passWithNoTests' in o && 'reporters' in o) {
            if (claimMain()) {
              const g = { ...o };
              // Project configs are frozen after the global one: assess when tests are sorted.
              S.lateAssess = () => assess(g, projects);
              addToGlobal(o, g);
            } else {
              notClaimed();
            }
          } else if ('testEnvironment' in o && 'testRunner' in o && 'transform' in o && 'rootDir' in o) {
            projects.push(o);
          }
        });
      }
      return freeze.apply(this, arguments);
    };
  }
}
