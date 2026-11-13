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
// It changes no outcome: every wrapper calls what it wraps, and an error of its own is written as
// the adapter's, which makes the file `not observed`.

import fs from 'node:fs';
import module from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import childProcess from 'node:child_process';
import workerThreads from 'node:worker_threads';
import { afterAll, expect } from 'vitest';

const SCHEMA = 'hushrun.unit-facts/v0';
const STATE = Symbol.for('hushrun.vitest.worker');

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

function install() {
  const state = {
    reads: new Set(),
    dirs: new Set(),
    notFollowed: [],
    errors: [],
    written: 0,
  };
  const note = (what, fn) => {
    try {
      fn();
    } catch (err) {
      state.errors.push(`${what}: ${err && err.message ? err.message : String(err)}`.slice(0, 500));
    }
  };
  const read = (value) => note('read', () => {
    const p = pathOf(value);
    if (p) state.reads.add(p);
  });
  const listed = (value) => note('list', () => {
    const p = pathOf(value);
    if (p) state.dirs.add(p);
  });
  // A wrapper keeps what callers can see on what it wraps: its `name` and `length`, and its own
  // properties, such as `util.promisify.custom` on `exec` and `execFile` (`{ stdout, stderr }`)
  // or `native` on `fs.realpath`. One that is a function is wrapped with the same record, since
  // Node's calls the original it closed over, and what it does would go unseen; one that is what
  // it wraps (`fs.promises.opendir`'s own `promisify.custom`) is the wrapper.
  const observing = (original, record, made = new Map()) => {
    if (made.has(original)) return made.get(original);
    const wrapped = function (...args) {
      record(args[0], args);
      return original.apply(this, args);
    };
    made.set(original, wrapped);
    for (const key of Reflect.ownKeys(original)) {
      if (key === 'prototype' || key === 'arguments' || key === 'caller') continue;
      const desc = Object.getOwnPropertyDescriptor(original, key);
      if (key !== 'name' && key !== 'length' && typeof desc.value === 'function') {
        desc.value = observing(desc.value, record, made);
      }
      note('wrap', () => Object.defineProperty(wrapped, key, desc));
    }
    return wrapped;
  };
  const wrap = (target, name, record) => {
    const original = target[name];
    if (typeof original !== 'function') return;
    target[name] = observing(original, record);
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
    state.notFollowed.push({ why: 'subprocess', detail: `${what} ${String(command)}`.slice(0, 200) });
  };
  for (const name of ['spawn', 'spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync',
    'fork']) {
    wrap(childProcess, name, spawned(name));
  }
  const Worker = workerThreads.Worker;
  if (typeof Worker === 'function') {
    workerThreads.Worker = class extends Worker {
      constructor(...args) {
        state.notFollowed.push({ why: 'worker', detail: String(args[0]).slice(0, 200) });
        super(...args);
      }
    };
  }
  const dlopen = process.dlopen;
  process.dlopen = function (...args) {
    state.notFollowed.push({ why: 'native', detail: String(args[1]).slice(0, 200) });
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
  state.errors.push(`testPath: ${err && err.message ? err.message : String(err)}`);
}

afterAll(() => {
  if (!dir || typeof file !== 'string') return;
  try {
    const reads = new Set(state.reads);
    for (const id of globalThis.__hushrun_vitest_seen || []) reads.add(String(id));
    const require = module.createRequire(import.meta.url);
    for (const id of Object.keys(require.cache || {})) reads.add(id);
    state.written += 1;
    const name = `vitest-worker-${process.pid}-${workerThreads.threadId}-${state.written}.json`;
    const target = path.join(dir, name);
    const temp = path.join(dir, `.${name}.tmp`);
    fs.writeFileSync(temp, JSON.stringify({
      schema: SCHEMA,
      adapter: 'vitest-worker',
      units: [{
        unit: path.resolve(file),
        reads: [...reads],
        dirs: [...state.dirs],
        notFollowed: state.notFollowed,
      }],
      errors: state.errors,
    }));
    fs.renameSync(temp, target);
  } catch {
    // Nothing written: hushrun then knows nothing of this file's reads, and it is not observed.
  }
});
