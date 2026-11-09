// bin/hushrun.js: which binary it runs (the platform package, else vendor/, else HUSHRUN_BINARY),
// and that arguments, stdin, the environment, the exit code and signals pass through. The fake
// binaries are Node scripts, so the tests that run them need a POSIX system.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const SOURCE = path.join(path.dirname(path.dirname(fileURLToPath(import.meta.url))), 'hushrun');
const require = createRequire(import.meta.url);
const VERSION = '1.2.3';
const HOST = `@hushrun/cli-${process.platform}-${process.arch}`;
const posix =
  process.platform === 'win32' ? 'the fake binaries are scripts, which need POSIX' : false;

/** A stand-in for hushrun: says which copy it is and what it got, then does what FAKE says. */
function fakeBinary(file, which) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(
    file,
    `#!${process.execPath}
const which = ${JSON.stringify(which)};
const [action, arg] = (process.env.FAKE || '').split(':');
const report = (extra) =>
  console.log(JSON.stringify({ which, argv: process.argv.slice(2), npmPackage: process.env.HUSHRUN_NPM_PACKAGE ?? null, ...extra }));
if (action === 'exit') {
  report();
  process.exit(Number(arg));
} else if (action === 'kill') {
  process.kill(process.pid, arg);
  setTimeout(() => {}, 5000);
} else if (action === 'stdin') {
  report({ stdin: require('node:fs').readFileSync(0, 'utf8') });
} else if (action === 'wait') {
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    process.on(signal, () => {
      console.log('got ' + signal);
      if (arg === 'die') {
        process.removeAllListeners(signal);
        process.kill(process.pid, signal);
      } else {
        process.exit(42);
      }
    });
  }
  console.log('ready');
  // A signal that never comes fails the test, rather than leaving this waiting.
  setTimeout(() => process.exit(99), 10000);
} else {
  report();
}
`,
  );
  fs.chmodSync(file, 0o755);
}

/**
 * node_modules/hushrun (this source, at VERSION) in a new directory, with a fake binary in the
 * platform package for `platform`/`arch` and in vendor/ (unless left out), and one elsewhere for
 * HUSHRUN_BINARY.
 */
function layout(
  t,
  { platformPackage = true, vendor = true, platform = process.platform, arch = process.arch } = {},
) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'hushrun-shim-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const main = path.join(root, 'node_modules', 'hushrun');
  const pkg = JSON.parse(fs.readFileSync(path.join(SOURCE, 'package.json'), 'utf8'));
  pkg.version = VERSION;
  for (const name of Object.keys(pkg.optionalDependencies)) {
    pkg.optionalDependencies[name] = VERSION;
  }
  fs.mkdirSync(path.join(main, 'bin'), { recursive: true });
  fs.writeFileSync(path.join(main, 'package.json'), JSON.stringify(pkg, null, 2));
  fs.copyFileSync(path.join(SOURCE, 'bin', 'hushrun.js'), path.join(main, 'bin', 'hushrun.js'));
  const name = `@hushrun/cli-${platform}-${arch}`;
  const exe = platform === 'win32' ? 'hushrun.exe' : 'hushrun';
  const files = {
    root,
    shim: path.join(main, 'bin', 'hushrun.js'),
    platform: path.join(root, 'node_modules', name, 'bin', exe),
    vendor: path.join(main, 'vendor', exe),
    env: path.join(root, 'elsewhere', exe),
  };
  if (platformPackage) {
    fakeBinary(files.platform, 'platform');
    fs.writeFileSync(
      path.join(root, 'node_modules', name, 'package.json'),
      JSON.stringify({ name, version: VERSION }),
    );
  }
  if (vendor) {
    fakeBinary(files.vendor, 'vendor');
  }
  fakeBinary(files.env, 'env');
  return files;
}

/** This environment without what would change the shim's choices, plus `vars`. */
function environment(vars = {}) {
  const env = { ...process.env };
  for (const name of [
    'HUSHRUN_BINARY',
    'HUSHRUN_NPM_PACKAGE',
    'FAKE',
    'NODE_TEST_CONTEXT',
    'NODE_OPTIONS',
  ]) {
    delete env[name];
  }
  return { ...env, ...vars };
}

function runShim(files, { args = [], vars, input, cwd } = {}) {
  return spawnSync(process.execPath, [files.shim, ...args], {
    cwd: cwd || files.root,
    env: environment(vars),
    input,
    encoding: 'utf8',
    timeout: 20000,
  });
}

function report(result) {
  assert.equal(result.stderr, '');
  return JSON.parse(result.stdout);
}

test('finds the platform package first, then vendor/, then HUSHRUN_BINARY', (t) => {
  // Resolution only: on every OS, and for platforms other than this one.
  for (const [platform, arch] of [
    [process.platform, process.arch],
    ['linux', 'arm64'],
    ['win32', 'x64'],
  ]) {
    const name = `@hushrun/cli-${platform}-${arch}`;
    const on = { platform, arch };

    const all = layout(t, on);
    const env = { HUSHRUN_BINARY: all.env };
    assert.deepEqual(require(all.shim).findBinary({ ...on, env }), {
      file: all.platform,
      from: name,
    });

    const noPackage = layout(t, { ...on, platformPackage: false });
    assert.deepEqual(
      require(noPackage.shim).findBinary({ ...on, env: { HUSHRUN_BINARY: noPackage.env } }),
      {
        file: noPackage.vendor,
        from: 'vendor',
      },
    );

    const neither = layout(t, { ...on, platformPackage: false, vendor: false });
    assert.deepEqual(
      require(neither.shim).findBinary({ ...on, env: { HUSHRUN_BINARY: neither.env } }),
      {
        file: neither.env,
        from: 'HUSHRUN_BINARY',
      },
    );

    const none = layout(t, { ...on, platformPackage: false, vendor: false });
    const missing = require(none.shim).findBinary({ ...on, env: {} });
    assert.deepEqual(Object.keys(missing), ['missing']);
    assert.match(
      missing.missing,
      new RegExp(`^hushrun: ${name.replaceAll('.', '\\.')}, which holds the hushrun binary`),
    );
  }
});

test('runs the binary it finds', { skip: posix }, (t) => {
  const vars = (files) => ({ HUSHRUN_BINARY: files.env });
  const all = layout(t);
  assert.equal(report(runShim(all, { vars: vars(all) })).which, 'platform');
  const noPackage = layout(t, { platformPackage: false });
  assert.equal(report(runShim(noPackage, { vars: vars(noPackage) })).which, 'vendor');
  const neither = layout(t, { platformPackage: false, vendor: false });
  assert.equal(report(runShim(neither, { vars: vars(neither) })).which, 'env');
  // A relative HUSHRUN_BINARY is relative to the working directory, never a PATH lookup.
  assert.equal(
    report(runShim(neither, { vars: { HUSHRUN_BINARY: path.join('elsewhere', 'hushrun') } })).which,
    'env',
  );
});

test('without a binary, says so in one line and exits 1', { skip: posix }, (t) => {
  const result = runShim(layout(t, { platformPackage: false, vendor: false }));
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr.split('\n').length, 2, 'one line, and its newline');
  assert.match(
    result.stderr,
    new RegExp(
      `^hushrun: ${HOST.replaceAll('.', '\\.')}, which holds the hushrun binary, isn't installed`,
    ),
  );
  assert.match(result.stderr, /hushrun-installer\.sh \| sh\n$/);
});

test("says why it can't run HUSHRUN_BINARY", { skip: posix }, (t) => {
  const files = layout(t, { platformPackage: false, vendor: false });
  const absent = runShim(files, { vars: { HUSHRUN_BINARY: path.join(files.root, 'absent') } });
  assert.equal(absent.status, 1);
  assert.equal(
    absent.stderr,
    `hushrun: can't run ${path.join(files.root, 'absent')}: no such file\n`,
  );

  fs.chmodSync(files.env, 0o644);
  const notExecutable = runShim(files, { vars: { HUSHRUN_BINARY: files.env } });
  assert.equal(notExecutable.status, 1);
  assert.match(notExecutable.stderr, /permission denied \(is it executable\?\)\n$/);

  // node_modules/.bin/hushrun is this script: running it would start it again, and again.
  const bin = path.join(files.root, 'node_modules', '.bin');
  fs.mkdirSync(bin);
  fs.symlinkSync(path.join('..', 'hushrun', 'bin', 'hushrun.js'), path.join(bin, 'hushrun'));
  const itself = runShim(files, { vars: { HUSHRUN_BINARY: path.join(bin, 'hushrun') } });
  assert.equal(itself.status, 1);
  assert.match(itself.stderr, /^hushrun: HUSHRUN_BINARY names this npm command/);
});

test('passes the arguments as they are', { skip: posix }, (t) => {
  const args = [
    'run',
    '--',
    'node',
    '-e',
    'console.log("a b")',
    '',
    '  ',
    'ü ñ',
    '$HOME',
    '*',
    "it's",
    '--version',
    '-h',
  ];
  assert.deepEqual(report(runShim(layout(t), { args })).argv, args);
  assert.deepEqual(report(runShim(layout(t))).argv, []);
});

test('sets HUSHRUN_NPM_PACKAGE to this package unless it is set', { skip: posix }, (t) => {
  const files = layout(t);
  assert.equal(report(runShim(files)).npmPackage, `hushrun@${VERSION}`);
  const tarball = 'file:../hushrun-1.2.3.tgz';
  assert.equal(
    report(runShim(files, { vars: { HUSHRUN_NPM_PACKAGE: tarball } })).npmPackage,
    tarball,
  );
  assert.equal(report(runShim(files, { vars: { HUSHRUN_NPM_PACKAGE: '' } })).npmPackage, '');
});

test('passes stdin through', { skip: posix }, (t) => {
  const input = 'Apply? y\nsecond line\n';
  assert.equal(report(runShim(layout(t), { vars: { FAKE: 'stdin' }, input })).stdin, input);
});

test("exits with the binary's exit code", { skip: posix }, (t) => {
  const files = layout(t);
  for (const code of [0, 1, 2, 8, 130, 255]) {
    const result = runShim(files, { vars: { FAKE: `exit:${code}` } });
    assert.equal(result.status, code);
    assert.equal(report(result).which, 'platform');
  }
});

test('ends by the signal that ended the binary', { skip: posix }, (t) => {
  const files = layout(t);
  for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP', 'SIGKILL']) {
    const result = runShim(files, { vars: { FAKE: `kill:${signal}` } });
    assert.equal(result.signal, signal);
    assert.equal(result.status, null);
  }
  // Node ignores SIGPIPE, so the shim can't end by it: it exits 128 + 13, as shells report it.
  fs.writeFileSync(files.platform, '#!/bin/sh\nkill -PIPE $$\n');
  const pipe = runShim(files);
  assert.equal(pipe.signal, null);
  assert.equal(pipe.status, 141);
});

/** The shim, started with FAKE=wait, once its binary is ready for a signal. */
async function waiting(t, mode) {
  const files = layout(t);
  const child = spawn(process.execPath, [files.shim], {
    cwd: files.root,
    env: environment({ FAKE: mode }),
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  t.after(() => child.exitCode === null && child.signalCode === null && child.kill('SIGKILL'));
  let stdout = '';
  child.stdout.setEncoding('utf8');
  await new Promise((resolve, reject) => {
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
      if (stdout.includes('ready\n')) {
        resolve();
      }
    });
    child.on('exit', () =>
      reject(new Error(`the shim ended before its binary was ready: ${stdout}`)),
    );
  });
  return { child, output: () => stdout };
}

test(
  'relays signals sent to it alone to the binary',
  { skip: posix, timeout: 30000 },
  async (t) => {
    for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) {
      const { child, output } = await waiting(t, 'wait');
      child.kill(signal);
      const [code, ended] = await once(child, 'close');
      assert.equal(output(), `ready\ngot ${signal}\n`);
      assert.deepEqual([code, ended], [42, null]);
    }
    // A binary that ends by the relayed signal: the shim ends by it too.
    const { child, output } = await waiting(t, 'wait:die');
    child.kill('SIGTERM');
    const [code, ended] = await once(child, 'close');
    assert.equal(output(), 'ready\ngot SIGTERM\n');
    assert.deepEqual([code, ended], [null, 'SIGTERM']);
  },
);
