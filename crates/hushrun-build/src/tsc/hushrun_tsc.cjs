'use strict';
// [ADR 0106] hushrun's TypeScript compiler hook (B-100 to B-109), loaded into every Node process
// of COMMAND with
//
//   NODE_OPTIONS="<what it had> --require <this file>"
//
// which `hushrun run` sets beside HUSHRUN_TSC, the helper `hushrun-tsc`. In a process that isn't
// a compiler this table names, or without HUSHRUN_TSC, it does nothing at all.
//
// In a compiler's process it asks the helper, before the compiler loads, whether a block holds
// what this call would write (`hushrun-tsc lookup`). When one does, the helper has put its files
// in place: this file writes the compiler's messages and ends the process with its status, and
// the compiler never starts. Otherwise it records, while the compiler runs, every file it reads,
// looks at, lists and resolves through Node's `fs`, every file it writes, and what it prints,
// and hands that to the helper as the process ends (`hushrun-tsc store`), which keys and stores
// it. It decides nothing itself, and every hook is guarded: an error of its own is recorded
// (the call is then not stored) and never reaches the compiler, whose output, files and status
// are its own.
//
// A compiler is named by the file the process runs and the file whose bytes are its identity:
// vue-tsc or svelte-check can be added here as rows, and the helper keys them the same way.


function start() {
  const main = process.argv[1];
  if (typeof main !== 'string') return;
  // The cheap test first: every other Node process pays only this.
  if (!/(?:^|[\\/])(?:tsc|tsc\.js)$/.test(main)) return;
  try {
    if (!require('worker_threads').isMainThread) return;
  } catch {
    return;
  }
  const fs = require('fs');
  const path = require('path');
  const childProcess = require('child_process');
  const own = {
    readFileSync: fs.readFileSync,
    writeFileSync: fs.writeFileSync,
    writeSync: fs.writeSync,
    statSync: fs.statSync,
    realpathSync: fs.realpathSync,
    renameSync: fs.renameSync,
    spawnSync: childProcess.spawnSync,
  };
  let real;
  try {
    real = own.realpathSync(main);
  } catch {
    return;
  }
  const program = PROGRAMS.map((p) => p(real, own, path)).find(Boolean);
  if (!program) return;
  const helper = process.env.HUSHRUN_TSC;
  const call = {
    schema: 'hushrun.tsc-call/v1',
    program: program.name,
    package: program.package,
    version: program.version,
    compiler: program.compiler,
    args: process.argv.slice(2),
    cwd: process.cwd(),
    tty: !!(process.stdout && process.stdout.isTTY),
    platform: process.platform,
    env: pick(process.env, ['NO_COLOR', 'FORCE_COLOR', 'NODE_ENV']),
    pid: process.pid,
  };
  let answer;
  try {
    const asked = own.spawnSync(helper, ['lookup'], {
      input: JSON.stringify(call),
      stdio: ['pipe', 'pipe', 'ignore'],
      maxBuffer: 1 << 30,
      windowsHide: true,
    });
    if (asked.status !== 0 || !asked.stdout) return;
    answer = JSON.parse(asked.stdout.toString('utf8'));
  } catch {
    return;
  }
  if (!answer || typeof answer !== 'object') return;
  if (answer.do === 'reused') {
    // B-105: the helper put the files in place; the messages are the compiler's as it wrote
    // them, and the status its own.
    try {
      if (answer.stdout) own.writeSync(1, answer.stdout);
      if (answer.stderr) own.writeSync(2, answer.stderr);
    } catch {
      // A closed stdout is the compiler's problem too: the status still stands.
    }
    process.exit(typeof answer.code === 'number' ? answer.code : 0);
  }
  if (answer.do === 'record' && typeof answer.record === 'string' && typeof answer.plan === 'string') {
    record(answer, { fs, path, own, helper });
  }
}

// [B-101] The compilers this hook knows, by the file the process runs (after links). Each row
// returns what the helper keys the call by, or nothing for a file that isn't its compiler. A
// compiler that doesn't run in this process (TypeScript 7 and later, which is native code
// started by `bin/tsc`) is not a row: its reads can't be seen here.
const PROGRAMS = [
  function typescript(real, own, path) {
    const m = /^(.*[\\/]typescript)[\\/](?:bin[\\/]tsc|lib[\\/]tsc\.js)$/.exec(real);
    if (!m) return null;
    const dir = m[1];
    let manifest;
    try {
      manifest = JSON.parse(own.readFileSync(path.join(dir, 'package.json'), 'utf8'));
    } catch {
      return null;
    }
    if (!manifest || manifest.name !== 'typescript' || typeof manifest.version !== 'string') return null;
    const major = parseInt(manifest.version, 10);
    if (!(major >= 4 && major < 7)) return null;
    // TypeScript 5.7 and later load `lib/_tsc.js` from a small `lib/tsc.js`; before, `lib/tsc.js`
    // is the compiler.
    let compiler = path.join(dir, 'lib', '_tsc.js');
    try {
      own.statSync(compiler);
    } catch {
      compiler = path.join(dir, 'lib', 'tsc.js');
    }
    return { name: 'tsc', package: dir, version: manifest.version, compiler };
  },
];

function pick(env, names) {
  const out = {};
  for (const name of names) if (typeof env[name] === 'string') out[name] = env[name];
  return out;
}

// [B-103] What the compiler did through `fs`, recorded while it runs and handed to the helper
// as the process ends.
function record(answer, { fs, path, own, helper }) {
  const R = {
    schema: 'hushrun.tsc-record/v1',
    reads: new Map(), // path -> [sha256, size]
    looks: new Map(), // path -> 'f' | 'd' | 'o' | '-'
    lists: new Map(), // path -> [[name, kind]...] | null
    links: new Map(), // path -> resolved | null
    writes: [], // paths, in the order first written
    written: new Set(),
    other: [], // what makes the call unstorable, in words
    errors: [],
    stdout: [],
    stderr: [],
  };
  const abs = (p) => {
    if (typeof p === 'string') return path.resolve(p);
    if (p instanceof URL) return path.resolve(require('url').fileURLToPath(p));
    if (Buffer.isBuffer(p)) return path.resolve(p.toString('utf8'));
    return null;
  };
  const kindOf = (st) => (!st ? '-' : st.isFile() ? 'f' : st.isDirectory() ? 'd' : 'o');
  const direntKind = (d) => (d.isFile() ? 'f' : d.isDirectory() ? 'd' : d.isSymbolicLink() ? 'l' : 'o');
  let depth = 0;
  // A hook's own reads go through `fs` too: at depth, so they record nothing.
  const guard = (fn) => {
    depth++;
    try {
      fn();
    } catch (err) {
      R.errors.push(String(err && err.message ? err.message : err).slice(0, 200));
    } finally {
      depth--;
    }
  };
  const wrap = (name, after) => {
    const orig = fs[name];
    if (typeof orig !== 'function') return;
    const wrapped = function (...args) {
      // A call `fs` makes of itself (readFileSync opens the file) is the outer call's.
      if (depth > 0) return orig.apply(this, args);
      let result;
      let failed = null;
      depth++;
      try {
        result = orig.apply(this, args);
      } catch (err) {
        failed = err;
      } finally {
        depth--;
      }
      guard(() => after(args, result, failed));
      if (failed) throw failed;
      return result;
    };
    for (const k of Object.keys(orig)) wrapped[k] = orig[k];
    Object.defineProperty(wrapped, 'name', { value: orig.name });
    fs[name] = wrapped;
  };
  // What the compile itself wrote earlier in this run (`tsc --build` reads one project's
  // declarations for the next) is its own output, not what it was given: left out of what it
  // read, looked at, listed and resolved.
  const own_output = (a) => R.written.has(a);
  const look = (p, st) => {
    const a = abs(p);
    if (a && !R.looks.has(a) && !own_output(a)) R.looks.set(a, kindOf(st));
  };
  // A read is keyed by the bytes the compiler got, hashed as it got them: a file that changes
  // afterwards keys another block, never this one.
  const crypto = require('crypto');
  const read = (p, result) => {
    const a = abs(p);
    if (!a || R.reads.has(a) || own_output(a)) return;
    let bytes = result;
    if (typeof bytes === 'string') bytes = own.readFileSync(a);
    if (!Buffer.isBuffer(bytes)) return R.other.push('read-kind');
    const digest = crypto.createHash('sha256').update(bytes).digest('hex');
    R.reads.set(a, [digest, bytes.length]);
  };
  const wrote = (p) => {
    const a = abs(p);
    if (!a) return;
    if (!R.written.has(a)) {
      R.written.add(a);
      R.writes.push(a);
    }
  };
  wrap('readFileSync', (args, result, failed) => {
    if (typeof args[0] === 'number') return R.other.push('read-fd');
    if (failed) return look(args[0], null);
    read(args[0], result);
  });
  wrap('statSync', (args, result, failed) => look(args[0], failed ? null : result));
  wrap('lstatSync', (args, result, failed) => {
    const a = abs(args[0]);
    if (!a) return;
    if (!failed && result && result.isSymbolicLink()) {
      if (!R.links.has(a) && !own_output(a)) R.links.set(a, linkOf(a));
      return;
    }
    look(a, failed ? null : result);
  });
  wrap('existsSync', (args, result) => {
    const a = abs(args[0]);
    if (a && !R.looks.has(a) && !own_output(a)) R.looks.set(a, result ? kindOf(own.statSync(a, { throwIfNoEntry: false })) : '-');
  });
  wrap('accessSync', (args, result, failed) => {
    const a = abs(args[0]);
    if (a && !R.looks.has(a) && !own_output(a)) R.looks.set(a, failed ? '-' : kindOf(own.statSync(a, { throwIfNoEntry: false })));
  });
  wrap('readdirSync', (args, result, failed) => {
    const a = abs(args[0]);
    if (!a || R.lists.has(a)) return;
    if (failed || !Array.isArray(result)) return R.lists.set(a, null);
    const entries = result.map((e) =>
      typeof e === 'string' || Buffer.isBuffer(e)
        ? [String(e), kindOf(own.statSync(path.join(a, String(e)), { throwIfNoEntry: false }))]
        : [e.name, direntKind(e)],
    );
    R.lists.set(a, entries.filter((e) => !own_output(path.join(a, e[0]))));
  });
  const linkOf = (a) => {
    try {
      return own.realpathSync.native ? own.realpathSync.native(a) : own.realpathSync(a);
    } catch {
      return null;
    }
  };
  const resolved = (args, result, failed) => {
    const a = abs(args[0]);
    if (a && !R.links.has(a) && !own_output(a)) R.links.set(a, failed ? null : String(result));
  };
  {
    const native = fs.realpathSync.native;
    wrap('realpathSync', resolved);
    if (typeof native === 'function') {
      const wrappedNative = function (...args) {
        if (depth > 0) return native.apply(this, args);
        let result;
        let failed = null;
        depth++;
        try {
          result = native.apply(this, args);
        } catch (err) {
          failed = err;
        } finally {
          depth--;
        }
        guard(() => resolved(args, result, failed));
        if (failed) throw failed;
        return result;
      };
      fs.realpathSync.native = wrappedNative;
    }
  }
  const fds = new Map();
  wrap('openSync', (args, result, failed) => {
    if (failed) return;
    const flags = args[1] === undefined ? 'r' : args[1];
    if (flags === 'r' || flags === 'rs' || flags === 0) {
      R.other.push('read-fd');
      return;
    }
    const a = abs(args[0]);
    if (a) {
      fds.set(result, a);
      wrote(a);
    }
  });
  wrap('writeFileSync', (args, result, failed) => {
    if (typeof args[0] === 'number') {
      if (!fds.has(args[0]) && args[0] > 2) R.other.push('write-fd');
      return;
    }
    if (!failed) wrote(args[0]);
  });
  wrap('mkdirSync', () => {});
  for (const name of ['unlinkSync', 'rmSync', 'rmdirSync', 'renameSync', 'copyFileSync', 'cpSync', 'utimesSync',
    'futimesSync', 'symlinkSync', 'linkSync', 'appendFileSync', 'truncateSync', 'chmodSync']) {
    wrap(name, (args, result, failed) => {
      if (!failed) R.other.push(name.replace(/Sync$/, ''));
    });
  }
  for (const name of ['readFile', 'stat', 'lstat', 'readdir', 'open', 'writeFile', 'access', 'exists', 'realpath',
    'opendir', 'opendirSync', 'createReadStream', 'createWriteStream', 'watch', 'watchFile']) {
    const orig = fs[name];
    if (typeof orig !== 'function') continue;
    const wrapped = function (...args) {
      R.other.push('async-' + name);
      return orig.apply(this, args);
    };
    for (const k of Object.keys(orig)) wrapped[k] = orig[k];
    fs[name] = wrapped;
  }
  // What the compiler prints, as it prints it: passed on unchanged.
  for (const [stream, into] of [[process.stdout, R.stdout], [process.stderr, R.stderr]]) {
    const write = stream.write;
    stream.write = function (chunk, encoding, cb) {
      guard(() => {
        if (typeof chunk === 'string') into.push(chunk);
        else if (chunk) into.push(Buffer.from(chunk).toString('utf8'));
      });
      return write.apply(this, arguments);
    };
  }
  process.on('exit', (code) => {
    try {
      const out = {
        schema: R.schema,
        code,
        reads: [...R.reads].map(([p, s]) => [p, ...s]),
        looks: [...R.looks],
        lists: [...R.lists],
        links: [...R.links],
        writes: R.writes,
        other: [...new Set(R.other)],
        errors: R.errors,
        stdout: R.stdout.join(''),
        stderr: R.stderr.join(''),
      };
      const temp = path.join(path.dirname(answer.record), '.' + path.basename(answer.record) + '.tmp');
      own.writeFileSync(temp, JSON.stringify(out));
      own.renameSync(temp, answer.record);
      own.spawnSync(helper, ['store', answer.plan], { stdio: 'ignore', windowsHide: true });
    } catch {
      // Not stored: the compiler's status stands.
    }
  });
}

// Last, so that everything above is defined when it runs.
const KEY = Symbol.for('hushrun.tsc');
if (!globalThis[KEY] && process.env.HUSHRUN_TSC) {
  globalThis[KEY] = true;
  try {
    start();
  } catch (err) {
    // Never break the process this was loaded into.
  }
}
