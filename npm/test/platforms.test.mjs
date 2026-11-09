// The platform mapping: Node's (os, cpu) → platform package → release target, and the lists
// that must agree on it (hushrun's optional dependencies, build.mjs, dist-workspace.toml).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { TARGETS, mainManifest } from '../build.mjs';

const NPM_DIR = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const require = createRequire(import.meta.url);
const shim = require('../hushrun/bin/hushrun.js');
const manifest = require('../hushrun/package.json');

const SIX = [
  ['darwin', 'arm64', '@hushrun/cli-darwin-arm64', 'aarch64-apple-darwin'],
  ['darwin', 'x64', '@hushrun/cli-darwin-x64', 'x86_64-apple-darwin'],
  ['linux', 'arm64', '@hushrun/cli-linux-arm64', 'aarch64-unknown-linux-musl'],
  ['linux', 'x64', '@hushrun/cli-linux-x64', 'x86_64-unknown-linux-musl'],
  ['win32', 'arm64', '@hushrun/cli-win32-arm64', 'aarch64-pc-windows-msvc'],
  ['win32', 'x64', '@hushrun/cli-win32-x64', 'x86_64-pc-windows-msvc'],
];

/** The `targets` array of dist-workspace.toml: beside npm/ in the public repository, a level up
 * in the maintainers', where the CLI is in cli/. */
function distTargets() {
  const here = path.join(NPM_DIR, '..', 'dist-workspace.toml');
  const file = fs.existsSync(here) ? here : path.join(NPM_DIR, '..', '..', 'dist-workspace.toml');
  const toml = fs.readFileSync(file, 'utf8');
  const list = toml.match(/^targets\s*=\s*\[([^\]]*)\]/m);
  assert.ok(list, 'dist-workspace.toml has a targets array');
  return [...list[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

test('each release platform has its package and its target', () => {
  for (const [platform, arch, name, target] of SIX) {
    assert.equal(shim.platformPackage(platform, arch), name);
    assert.equal(TARGETS[`${platform}-${arch}`], target);
  }
  assert.equal(shim.binaryName('win32'), 'hushrun.exe');
  assert.equal(shim.binaryName('darwin'), 'hushrun');
  assert.equal(shim.binaryName('linux'), 'hushrun');
});

test("hushrun's optional dependencies are the six platform packages", () => {
  assert.deepEqual(
    Object.keys(manifest.optionalDependencies).sort(),
    SIX.map(([, , name]) => name).sort(),
  );
  // The same version as the package: a placeholder here, which the build replaces in both.
  for (const version of Object.values(manifest.optionalDependencies)) {
    assert.equal(version, manifest.version);
  }
  assert.doesNotThrow(() => mainManifest());
});

test("build.mjs builds exactly dist-workspace.toml's targets", () => {
  assert.deepEqual(Object.values(TARGETS).sort(), distTargets().sort());
  assert.deepEqual(Object.keys(TARGETS).sort(), SIX.map(([p, a]) => `${p}-${a}`).sort());
});

test('the main package runs bin/hushrun.js as hushrun, on Node 18 or later', () => {
  assert.equal(manifest.name, 'hushrun');
  assert.deepEqual(manifest.bin, { hushrun: 'bin/hushrun.js' });
  assert.equal(manifest.engines.node, '>=18');
  assert.ok(manifest.files.includes('bin/') && manifest.files.includes('vendor/'));
});

test('every package is MIT, and carries LICENSE.md with the third-party notices', () => {
  assert.equal(manifest.license, 'MIT');
  assert.ok(manifest.files.includes('LICENSE.md'));
  assert.ok(manifest.files.includes('THIRD-PARTY-NOTICES.md'));
  // npm links the public repository, which has no npm/ directory.
  assert.equal(manifest.repository.directory, undefined);
});

test('platform packages declare os and cpu, and ship bin/ with the license and notices', () => {
  const template = fs.readFileSync(path.join(NPM_DIR, 'platform', 'package.json'), 'utf8');
  const pkg = JSON.parse(
    template
      .replaceAll('{os}', 'linux')
      .replaceAll('{cpu}', 'x64')
      .replaceAll('{platform}', 'Linux on x64'),
  );
  assert.equal(pkg.name, '@hushrun/cli-linux-x64');
  // Scoped, so private unless it says otherwise.
  assert.deepEqual(pkg.publishConfig, { access: 'public' });
  assert.deepEqual(pkg.os, ['linux']);
  assert.deepEqual(pkg.cpu, ['x64']);
  assert.deepEqual(pkg.files, ['bin/', 'LICENSE.md', 'THIRD-PARTY-NOTICES.md']);
  assert.equal(pkg.bin, undefined);
  assert.equal(pkg.license, manifest.license);
  assert.deepEqual(pkg.repository, manifest.repository);
});

test('a platform without a package gets one line, with the installer', () => {
  for (const [platform, arch] of [
    ['freebsd', 'x64'],
    ['linux', 'ia32'],
    ['linux', 'arm'],
    ['android', 'arm64'],
  ]) {
    const line = shim.missingLine(platform, arch);
    assert.doesNotMatch(line, /\n/);
    assert.match(line, new RegExp(`no hushrun binary for ${platform}-${arch}`));
    assert.match(line, /HUSHRUN_BINARY/);
    assert.match(
      line,
      /curl --proto '=https' --tlsv1\.2 -LsSf https:\/\/\S+\/hushrun-installer\.sh \| sh$/,
    );
  }
  assert.match(
    shim.missingLine('win32', 'ia32'),
    /irm https:\/\/\S+\/hushrun-installer\.ps1 \| iex"$/,
  );
});

test("a supported platform whose package isn't installed names the package", () => {
  for (const [platform, arch, name] of SIX) {
    const line = shim.missingLine(platform, arch);
    assert.doesNotMatch(line, /\n/);
    assert.match(
      line,
      new RegExp(
        `^hushrun: ${name.replaceAll('.', '\\.')}, which holds the hushrun binary, isn't installed`,
      ),
    );
    assert.match(
      line,
      platform === 'win32' ? /hushrun-installer\.ps1/ : /hushrun-installer\.sh \| sh$/,
    );
  }
});
