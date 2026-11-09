#!/usr/bin/env node
// The `hushrun` command of the hushrun package. It runs the hushrun binary for this platform
// with the same arguments, stdio and environment, and ends as the binary ended: with its exit
// code, or by the same signal.
//
// The binary comes from, in order: the platform package (@hushrun/cli-<os>-<cpu>, an optional
// dependency that npm installs only on its own platform), vendor/ in this package (a local build,
// npm/build.mjs --local), and the file HUSHRUN_BINARY names.
'use strict';

const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const pkg = require('../package.json');

const RELEASES = 'https://github.com/hushrun/hushrun/releases/latest/download';

/** The platform package for an OS and CPU as Node names them, such as @hushrun/cli-darwin-arm64. */
function platformPackage(platform, arch) {
  return `@hushrun/cli-${platform}-${arch}`;
}

/** The binary's file name, in a platform package's bin/ or in vendor/. */
function binaryName(platform) {
  return platform === 'win32' ? 'hushrun.exe' : 'hushrun';
}

/** The binary to run, as `{ file, from }`, or `{ missing }`: the line to print when there is none. */
function findBinary({ env = process.env, platform = process.platform, arch = process.arch } = {}) {
  const name = platformPackage(platform, arch);
  const exe = binaryName(platform);
  try {
    return { file: require.resolve(`${name}/bin/${exe}`), from: name };
  } catch {
    // Not installed: npm leaves out the other platforms' packages, and every optional
    // dependency with --omit=optional.
  }
  const vendored = path.join(__dirname, '..', 'vendor', exe);
  if (fs.existsSync(vendored)) {
    return { file: vendored, from: 'vendor' };
  }
  if (env.HUSHRUN_BINARY) {
    return { file: path.resolve(env.HUSHRUN_BINARY), from: 'HUSHRUN_BINARY' };
  }
  return { missing: missingLine(platform, arch) };
}

/** What to say when there is no binary: why, and the installer to use instead. */
function missingLine(platform, arch) {
  const install =
    platform === 'win32'
      ? `powershell -ExecutionPolicy Bypass -c "irm ${RELEASES}/hushrun-installer.ps1 | iex"`
      : `curl --proto '=https' --tlsv1.2 -LsSf ${RELEASES}/hushrun-installer.sh | sh`;
  const name = platformPackage(platform, arch);
  if (Object.hasOwn(pkg.optionalDependencies || {}, name)) {
    return (
      `hushrun: ${name}, which holds the hushrun binary, isn't installed (were optional ` +
      `dependencies left out?); reinstall ${pkg.name}, or install hushrun with: ${install}`
    );
  }
  return (
    `hushrun: ${pkg.name} has no hushrun binary for ${platform}-${arch}; set HUSHRUN_BINARY ` +
    `to one, or install hushrun with: ${install}`
  );
}

/** Whether `file` is this script, as node_modules/.bin/hushrun is: running it would loop. */
function isThisScript(file) {
  try {
    return fs.realpathSync(file) === fs.realpathSync(__filename);
  } catch {
    return false;
  }
}

/** Runs `file` with `args`, and ends this process as it ended. */
function run(file, args) {
  // Signals sent to this process alone, such as a CI runner's or a process manager's, go on to
  // the binary. A terminal's Ctrl+C reaches both anyway. On Windows, Ctrl+C reaches every process
  // on the console and killing the binary would end it at once, so this process only waits.
  const windows = process.platform === 'win32';
  const signals = windows ? ['SIGINT', 'SIGBREAK'] : ['SIGINT', 'SIGTERM', 'SIGHUP', 'SIGQUIT'];
  let child;
  const relay = (signal) => {
    if (!windows && child && child.exitCode === null && child.signalCode === null) {
      child.kill(signal);
    }
  };
  const release = () => signals.forEach((signal) => process.removeListener(signal, relay));
  signals.forEach((signal) => process.on(signal, relay));

  const cannotRun = (err) => {
    release();
    const why =
      err.code === 'ENOENT'
        ? 'no such file'
        : err.code === 'EACCES'
          ? 'permission denied (is it executable?)'
          : err.message;
    process.stderr.write(`hushrun: can't run ${file}: ${why}\n`);
    process.exitCode = 1;
  };
  try {
    child = spawn(file, args, { stdio: 'inherit' });
  } catch (err) {
    cannotRun(err);
    return;
  }
  child.on('error', (err) => {
    // After a start, an error is a signal that couldn't be relayed: the exit still comes.
    if (child.pid === undefined) {
      cannotRun(err);
    }
  });
  child.on('exit', (code, signal) => {
    release();
    if (signal) {
      // The same signal, now acting as by default, so that whatever waits for this process (a
      // shell, npm, a CI runner) sees the binary's ending.
      process.kill(process.pid, signal);
      // A signal Node ignores, such as SIGPIPE: exit as shells report one.
      setTimeout(() => process.exit(128 + (os.constants.signals[signal] || 0)), 1000);
      return;
    }
    process.exitCode = code;
  });
}

function main() {
  const found = findBinary();
  if (found.missing) {
    process.stderr.write(`${found.missing}\n`);
    process.exitCode = 1;
    return;
  }
  if (found.from === 'HUSHRUN_BINARY' && isThisScript(found.file)) {
    process.stderr.write(
      `hushrun: HUSHRUN_BINARY names this npm command (${found.file}); name a hushrun binary\n`,
    );
    process.exitCode = 1;
    return;
  }
  // The package `hushrun init` adds to devDependencies: this one, unless already set (a test
  // can name a tarball).
  if (process.env.HUSHRUN_NPM_PACKAGE === undefined) {
    process.env.HUSHRUN_NPM_PACKAGE = `${pkg.name}@${pkg.version}`;
  }
  run(found.file, process.argv.slice(2));
}

if (require.main === module) {
  main();
}

module.exports = { platformPackage, binaryName, findBinary, missingLine };
