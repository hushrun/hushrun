// npm/build.mjs: the seven packages from cargo-dist's archives, and the local build. It runs tar
// and npm pack, as the release workflow does on Linux.
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { LEGAL, SKILL, TARGETS, cargoVersion } from '../build.mjs';

const NPM_DIR = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const BUILD = path.join(NPM_DIR, 'build.mjs');
const VERSION = cargoVersion();
const posix = process.platform === 'win32' ? 'the build runs on macOS and Linux' : false;
const hasZip = !spawnSync('zip', ['-v'], { stdio: 'ignore' }).error;

function scratch(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'hushrun-build-')));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function runBuild(args) {
  return spawnSync(process.execPath, [BUILD, ...args], {
    encoding: 'utf8',
    env: { ...process.env, npm_config_update_notifier: 'false' },
    timeout: 120000,
  });
}

/** The package inside a tarball, unpacked under `into`. */
function unpacked(tarball, into) {
  const dir = path.join(into, path.basename(tarball, '.tgz'));
  fs.mkdirSync(dir, { recursive: true });
  execFileSync('tar', ['-xzf', tarball, '-C', dir]);
  return path.join(dir, 'package');
}

const mode = (file) => fs.statSync(file).mode & 0o777;
const json = (file) => JSON.parse(fs.readFileSync(file, 'utf8'));

/** The package in `pkg` carries hushrun's license and the third-party notices, as they are. */
function assertLegal(pkg) {
  for (const name of LEGAL) {
    assert.equal(
      fs.readFileSync(path.join(pkg, name), 'utf8'),
      fs.readFileSync(path.join(NPM_DIR, '..', name), 'utf8'),
      `${name} in ${pkg}`,
    );
  }
  assert.equal(json(path.join(pkg, 'package.json')).license, 'MIT');
}

/** A hushrun binary for this machine that says it is `version`. */
function fakeBinary(dir, version) {
  // Not hushrun-<version>: that is where unpacked() puts the tarball hushrun-<version>.tgz.
  const file = path.join(dir, `fake-hushrun-${version}`);
  fs.writeFileSync(file, `#!/bin/sh\necho "hushrun ${version}"\n`);
  fs.chmodSync(file, 0o755);
  return file;
}

/** cargo-dist's archive for `target` in `dir`, with its .sha256 file: zip for Windows, if it can. */
function releaseArchive(dir, target) {
  const windows = target.includes('windows');
  const exe = windows ? 'hushrun.exe' : 'hushrun';
  const name = `hushrun-${target}`;
  const staging = path.join(dir, 'staging', name);
  fs.mkdirSync(staging, { recursive: true });
  fs.writeFileSync(path.join(staging, exe), `binary for ${target}\n`);
  fs.chmodSync(path.join(staging, exe), 0o755);
  fs.writeFileSync(path.join(staging, 'README.md'), '# hushrun\n');
  let archive;
  if (windows && hasZip) {
    // Windows' zip has its files at the top; the tar.xz archives have them in a directory.
    archive = path.join(dir, `${name}.zip`);
    execFileSync('zip', ['-q', '-r', archive, '.'], { cwd: staging });
  } else {
    archive = path.join(dir, `${name}.tar.xz`);
    execFileSync('tar', ['-cJf', archive, '-C', path.dirname(staging), name]);
  }
  const hex = createHash('sha256').update(fs.readFileSync(archive)).digest('hex');
  fs.writeFileSync(`${archive}.sha256`, `${hex}  ${path.basename(archive)}\n`);
  return archive;
}

function releaseArchives(dir) {
  const archives = path.join(dir, 'archives');
  fs.mkdirSync(archives);
  return {
    dir: archives,
    files: Object.values(TARGETS).map((target) => releaseArchive(archives, target)),
  };
}

test(
  '--local: hushrun alone, with the binary in vendor/',
  { skip: posix, timeout: 120000 },
  (t) => {
    const dir = scratch(t);
    const binary = fakeBinary(dir, VERSION);
    const out = path.join(dir, 'out');
    const result = runBuild(['--local', '--binary', binary, '--out', out]);
    assert.equal(result.status, 0, result.stderr);
    const tarball = path.join(out, `hushrun-${VERSION}.tgz`);
    assert.equal(result.stdout, `${tarball}\n`);

    const pkg = unpacked(tarball, dir);
    const manifest = json(path.join(pkg, 'package.json'));
    assert.equal(manifest.name, 'hushrun');
    assert.equal(manifest.version, VERSION);
    assert.deepEqual(manifest.bin, { hushrun: 'bin/hushrun.js' });
    assert.equal(manifest.optionalDependencies, undefined, 'nothing to fetch from a registry');
    assert.deepEqual(fs.readdirSync(path.join(pkg, 'vendor')), ['hushrun']);
    assert.equal(
      fs.readFileSync(path.join(pkg, 'vendor', 'hushrun'), 'utf8'),
      fs.readFileSync(binary, 'utf8'),
    );
    assert.equal(mode(path.join(pkg, 'vendor', 'hushrun')), 0o755);
    assert.equal(mode(path.join(pkg, 'bin', 'hushrun.js')), 0o755);
    assert.equal(
      fs.readFileSync(path.join(pkg, 'bin', 'hushrun.js'), 'utf8'),
      fs.readFileSync(path.join(NPM_DIR, 'hushrun', 'bin', 'hushrun.js'), 'utf8'),
    );
    assert.ok(fs.existsSync(path.join(pkg, 'README.md')));
    // [ADR 0066] This release's skill, where tools that link skills from packages look.
    assert.equal(
      fs.readFileSync(path.join(pkg, 'skills', 'hushrun', 'SKILL.md'), 'utf8'),
      fs.readFileSync(SKILL, 'utf8'),
    );
    assert.ok(manifest.files.includes('skills/'));
    assertLegal(pkg);
  },
);

test('--local refuses a binary of another version', { skip: posix, timeout: 60000 }, (t) => {
  const dir = scratch(t);
  const result = runBuild([
    '--local',
    '--binary',
    fakeBinary(dir, '0.0.1'),
    '--out',
    path.join(dir, 'out'),
  ]);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /--version says "hushrun 0\.0\.1", and the Cargo workspace is at /);
  assert.equal(result.stdout, '');
});

test(
  "--from: seven packages from the release's archives",
  { skip: posix, timeout: 120000 },
  (t) => {
    const dir = scratch(t);
    const archives = releaseArchives(dir);
    const out = path.join(dir, 'out');
    const result = runBuild(['--from', archives.dir, '--out', out]);
    assert.equal(result.status, 0, result.stderr);

    // Platform packages first, then hushrun, which depends on them. npm pack names a scoped
    // package's tarball without the @: @hushrun/cli-darwin-arm64 is hushrun-cli-darwin-arm64-*.
    const platforms = Object.keys(TARGETS);
    assert.deepEqual(
      result.stdout.trimEnd().split('\n'),
      [
        ...platforms.map((p) => `hushrun-cli-${p}-${VERSION}.tgz`),
        `hushrun-${VERSION}.tgz`,
      ].map((f) => path.join(out, f)),
    );

    for (const [platform, target] of Object.entries(TARGETS)) {
      const [osName, cpu] = platform.split('-');
      const pkg = unpacked(path.join(out, `hushrun-cli-${platform}-${VERSION}.tgz`), dir);
      const manifest = json(path.join(pkg, 'package.json'));
      assert.equal(manifest.name, `@hushrun/cli-${platform}`);
      // A scoped package is private unless it says otherwise.
      assert.deepEqual(manifest.publishConfig, { access: 'public' });
      assert.equal(manifest.version, VERSION);
      assert.deepEqual(manifest.os, [osName]);
      assert.deepEqual(manifest.cpu, [cpu]);
      assert.deepEqual(manifest.files, ['bin/', ...LEGAL]);
      assert.equal(manifest.preferUnplugged, true);
      assert.doesNotMatch(JSON.stringify(manifest), /\{(os|cpu|platform)\}/);
      const exe = osName === 'win32' ? 'hushrun.exe' : 'hushrun';
      assert.deepEqual(fs.readdirSync(path.join(pkg, 'bin')), [exe]);
      assert.equal(fs.readFileSync(path.join(pkg, 'bin', exe), 'utf8'), `binary for ${target}\n`);
      assert.equal(mode(path.join(pkg, 'bin', exe)), 0o755);
      assert.match(
        fs.readFileSync(path.join(pkg, 'README.md'), 'utf8'),
        new RegExp(`^# @hushrun/cli-${platform}\n`),
      );
      assertLegal(pkg);
    }

    const main = unpacked(path.join(out, `hushrun-${VERSION}.tgz`), dir);
    const manifest = json(path.join(main, 'package.json'));
    assert.equal(manifest.version, VERSION);
    assert.deepEqual(
      manifest.optionalDependencies,
      Object.fromEntries(platforms.map((p) => [`@hushrun/cli-${p}`, VERSION])),
    );
    assert.ok(!fs.existsSync(path.join(main, 'vendor')));
    assert.deepEqual(
      fs.readdirSync(main).sort(),
      ['README.md', 'bin', 'package.json', 'skills', ...LEGAL].sort(),
    );
    assert.equal(
      fs.readFileSync(path.join(main, 'skills', 'hushrun', 'SKILL.md'), 'utf8'),
      fs.readFileSync(SKILL, 'utf8'),
    );
    assertLegal(main);
  },
);

test(
  '--from refuses an archive without its checksum, or with another',
  { skip: posix, timeout: 60000 },
  (t) => {
    const dir = scratch(t);
    const archives = releaseArchives(dir);
    const [first] = archives.files;
    const out = path.join(dir, 'out');

    fs.writeFileSync(`${first}.sha256`, `${'0'.repeat(64)}  ${path.basename(first)}\n`);
    const wrong = runBuild(['--from', archives.dir, '--out', out]);
    assert.equal(wrong.status, 1);
    assert.match(
      wrong.stderr,
      new RegExp(
        `${path.basename(first).replaceAll('.', '\\.')}: its SHA-256 is [0-9a-f]{64}, and`,
      ),
    );

    fs.rmSync(`${first}.sha256`);
    const missing = runBuild(['--from', archives.dir, '--out', out]);
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, /\.sha256 is missing/);

    fs.rmSync(first);
    const noArchive = runBuild(['--from', archives.dir, '--out', out]);
    assert.equal(noArchive.status, 1);
    assert.match(noArchive.stderr, /has no archive for aarch64-apple-darwin/);
  },
);

test('usage errors exit 2', { skip: posix }, () => {
  for (const args of [
    [],
    ['--local'],
    ['--from', 'x', '--binary', 'y'],
    ['--local', '--binary', 'y', '--from', 'x'],
    ['--frm', 'x'],
  ]) {
    const result = runBuild(args);
    assert.equal(result.status, 2, `${args.join(' ')}: ${result.stderr}`);
    assert.match(result.stderr, /usage: node npm\/build\.mjs --from DIR/);
  }
});
