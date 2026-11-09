#!/usr/bin/env node
// Builds hushrun's npm packages from release binaries: hushrun, whose `hushrun` command runs
// the binary, and one package per release target that holds it, @hushrun/cli-<os>-<cpu> (npm
// installs only the one for its platform). Node 18 or later, and no dependencies: it runs tar,
// unzip and npm.
//
//   node npm/build.mjs --from DIR [--out DIR]
//       All seven packages, from cargo-dist's archives in DIR: hushrun-<target>.tar.xz (.zip for
//       Windows), each next to its .sha256 file, as `gh release download` saves a release's.
//   node npm/build.mjs --local --binary PATH [--out DIR]
//       hushrun alone, with PATH, a hushrun binary for this machine, in vendor/: one tarball
//       that installs without a registry, for tests.
//
// The version is the Cargo workspace's. `npm pack` writes each package's tarball to --out
// (npm/dist/release, or npm/dist/local with --local), and the build prints the tarballs one per
// line, in the order to publish them: the platform packages first, hushrun last.

import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

const NPM_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.dirname(NPM_DIR);

/** Each platform package's Rust target (dist-workspace.toml), by `<os>-<cpu>` as Node names them. */
export const TARGETS = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-arm64': 'aarch64-unknown-linux-musl',
  'linux-x64': 'x86_64-unknown-linux-musl',
  'win32-arm64': 'aarch64-pc-windows-msvc',
  'win32-x64': 'x86_64-pc-windows-msvc',
};

const OS_NAMES = { darwin: 'macOS', linux: 'Linux', win32: 'Windows' };

/**
 * A platform package's name, such as @hushrun/cli-darwin-arm64. Scoped: npm's spam check refuses
 * new unscoped packages named like platform builds (README, "The packages").
 */
const platformName = (platform) => `@hushrun/cli-${platform}`;

const USAGE = `usage: node npm/build.mjs --from DIR [--out DIR]
       node npm/build.mjs --local --binary PATH [--out DIR]`;

/** The version in Cargo.toml's [workspace.package]. */
export function cargoVersion(root = ROOT) {
  const file = path.join(root, 'Cargo.toml');
  let section = '';
  for (const line of fs.readFileSync(file, 'utf8').split(/\r?\n/)) {
    const header = line.match(/^\s*\[([^\]]+)\]/);
    if (header) {
      section = header[1].trim();
      continue;
    }
    const version = line.match(/^\s*version\s*=\s*"([^"]+)"/);
    if (section === 'workspace.package' && version) {
      if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/.test(version[1])) {
        throw new Error(`${file}: ${version[1]} is not a release version such as 0.9.0`);
      }
      return version[1];
    }
  }
  throw new Error(`${file} has no version in [workspace.package]`);
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

function write(file, data, mode) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, data);
  fs.chmodSync(file, mode);
}

function copy(from, to, mode) {
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(from, to);
  fs.chmodSync(to, mode);
}

/** What every package carries besides its own files: hushrun's license and the notices. */
export const LEGAL = ['LICENSE.md', 'THIRD-PARTY-NOTICES.md'];

/** LEGAL, from the repository's root into a package. */
function copyLegal(dir) {
  for (const name of LEGAL) {
    const file = path.join(ROOT, name);
    if (!fs.existsSync(file)) {
      throw new Error(`${name} is missing from the repository's root: every package carries it`);
    }
    copy(file, path.join(dir, name), 0o644);
  }
}

/** hushrun's package.json, with its platform packages checked against TARGETS. */
export function mainManifest() {
  const pkg = readJson(path.join(NPM_DIR, 'hushrun', 'package.json'));
  const listed = Object.keys(pkg.optionalDependencies || {}).sort();
  const known = Object.keys(TARGETS).map(platformName).sort();
  if (listed.join() !== known.join()) {
    throw new Error(
      `npm/hushrun/package.json's optionalDependencies (${listed.join(', ')}) aren't the ` +
        `platforms build.mjs knows (${known.join(', ')})`,
    );
  }
  return pkg;
}

/** Stages hushrun in `dir`: with its platform packages, or with `binary` in vendor/. */
function stageMain(dir, version, binary) {
  const pkg = mainManifest();
  pkg.version = version;
  if (binary) {
    // The binary is in the package: no platform package to fetch from a registry.
    delete pkg.optionalDependencies;
    const exe = process.platform === 'win32' ? 'hushrun.exe' : 'hushrun';
    copy(binary, path.join(dir, 'vendor', exe), 0o755);
  } else {
    for (const name of Object.keys(pkg.optionalDependencies)) {
      pkg.optionalDependencies[name] = version;
    }
  }
  write(path.join(dir, 'package.json'), `${JSON.stringify(pkg, null, 2)}\n`, 0o644);
  copy(path.join(NPM_DIR, 'hushrun', 'README.md'), path.join(dir, 'README.md'), 0o644);
  copy(
    path.join(NPM_DIR, 'hushrun', 'bin', 'hushrun.js'),
    path.join(dir, 'bin', 'hushrun.js'),
    0o755,
  );
  copyLegal(dir);
  return pkg;
}

/** Stages the platform package for `platform` (such as darwin-arm64) in `dir`, with `binary`. */
function stagePlatform(dir, platform, version, binary) {
  const [osName, cpu] = platform.split('-');
  const fill = (text) =>
    text
      .replaceAll('{os}', osName)
      .replaceAll('{cpu}', cpu)
      .replaceAll('{platform}', `${OS_NAMES[osName]} on ${cpu}`);
  const pkg = JSON.parse(
    fill(fs.readFileSync(path.join(NPM_DIR, 'platform', 'package.json'), 'utf8')),
  );
  pkg.version = version;
  write(path.join(dir, 'package.json'), `${JSON.stringify(pkg, null, 2)}\n`, 0o644);
  const readme = fill(fs.readFileSync(path.join(NPM_DIR, 'platform', 'README.md'), 'utf8'));
  write(path.join(dir, 'README.md'), readme.replaceAll('{name}', pkg.name), 0o644);
  copy(binary, path.join(dir, 'bin', osName === 'win32' ? 'hushrun.exe' : 'hushrun'), 0o755);
  copyLegal(dir);
  return pkg;
}

/** Checks `archive` against the .sha256 file next to it (`<hex>  <name>`, as cargo-dist writes). */
function verifyChecksum(archive) {
  const sums = `${archive}.sha256`;
  if (!fs.existsSync(sums)) {
    throw new Error(`${path.basename(sums)} is missing: the release has one next to each archive`);
  }
  const [expected = '', name] = fs.readFileSync(sums, 'utf8').trim().split(/\s+/);
  if (name !== undefined && name.replace(/^\*/, '') !== path.basename(archive)) {
    throw new Error(`${path.basename(sums)} is for ${name}, not ${path.basename(archive)}`);
  }
  const actual = createHash('sha256').update(fs.readFileSync(archive)).digest('hex');
  if (actual !== expected.toLowerCase()) {
    throw new Error(
      `${path.basename(archive)}: its SHA-256 is ${actual}, and ${path.basename(sums)} says ${expected}`,
    );
  }
}

function unpack(archive, into) {
  fs.mkdirSync(into, { recursive: true });
  const quiet = { stdio: ['ignore', 'ignore', 'inherit'] };
  if (archive.endsWith('.zip')) {
    // unzip where there is one, as on Linux; else tar, which reads zip on macOS and Windows.
    const unzip = spawnSync('unzip', ['-q', archive, '-d', into], quiet);
    if (!unzip.error) {
      if (unzip.status !== 0) {
        throw new Error(`unzip ${path.basename(archive)} failed`);
      }
      return;
    }
  }
  execFileSync('tar', ['-xf', archive, '-C', into], quiet);
}

function filesNamed(dir, name) {
  const found = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      found.push(...filesNamed(file, name));
    } else if (entry.isFile() && entry.name === name) {
      found.push(file);
    }
  }
  return found;
}

/** The binary in the archive for `target` in `from`, checked and unpacked under `scratch`. */
function binaryFromArchive(from, target, exe, scratch) {
  const names = ['tar.xz', 'tar.gz', 'zip'].map((ext) => `hushrun-${target}.${ext}`);
  const archive = names.map((name) => path.join(from, name)).find((file) => fs.existsSync(file));
  if (!archive) {
    throw new Error(`${from} has no archive for ${target} (${names.join(', ')})`);
  }
  verifyChecksum(archive);
  const into = path.join(scratch, target);
  unpack(archive, into);
  const found = filesNamed(into, exe);
  if (found.length !== 1) {
    throw new Error(`${path.basename(archive)} holds ${found.length} files named ${exe}, not one`);
  }
  return { archive, binary: found[0] };
}

/** `binary --version` must say this version: the package and its binary are one release. */
function checkBinaryVersion(binary, version) {
  const result = spawnSync(binary, ['--version'], { encoding: 'utf8' });
  if (result.error) {
    throw new Error(`can't run ${binary}: ${result.error.message}`);
  }
  const said = (result.stdout || '').trim();
  if (said !== `hushrun ${version}`) {
    throw new Error(`${binary} --version says "${said}", and the Cargo workspace is at ${version}`);
  }
}

/** Packs the package in `dir` into `out` with `npm pack`: the tarball's path and size. */
function pack(dir, out) {
  const windows = process.platform === 'win32';
  const args = ['pack', dir, '--pack-destination', out, '--json'];
  // npm is a .cmd script on Windows, which only a shell runs.
  const json = execFileSync(
    windows ? 'npm.cmd' : 'npm',
    windows ? args.map((a) => `"${a}"`) : args,
    {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'inherit'],
      shell: windows,
    },
  );
  const [info] = JSON.parse(json);
  return { file: path.join(out, info.filename), size: info.size };
}

function size(bytes) {
  return bytes < 1e6 ? `${(bytes / 1e3).toFixed(1)} kB` : `${(bytes / 1e6).toFixed(1)} MB`;
}

/** Builds the packages: the tarballs, in the order to publish them. */
export function build({ from, local, binary, out }) {
  const version = cargoVersion();
  fs.mkdirSync(out, { recursive: true });
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'hushrun-npm-'));
  const tarballs = [];
  try {
    if (local) {
      checkBinaryVersion(binary, version);
      const pkg = stageMain(path.join(scratch, 'hushrun'), version, binary);
      const packed = pack(path.join(scratch, 'hushrun'), out);
      console.error(`${pkg.name}@${version}: vendor/ holds ${binary}, ${size(packed.size)}`);
      tarballs.push(packed.file);
    } else {
      for (const [platform, target] of Object.entries(TARGETS)) {
        const exe = platform.startsWith('win32-') ? 'hushrun.exe' : 'hushrun';
        const found = binaryFromArchive(from, target, exe, path.join(scratch, 'unpacked'));
        const dir = path.join(scratch, platform);
        const pkg = stagePlatform(dir, platform, version, found.binary);
        const packed = pack(dir, out);
        const archive = path.basename(found.archive);
        console.error(
          `${pkg.name}@${version}: bin/${exe} from ${archive} (checksum verified), ${size(packed.size)}`,
        );
        tarballs.push(packed.file);
      }
      const pkg = stageMain(path.join(scratch, 'hushrun'), version);
      const packed = pack(path.join(scratch, 'hushrun'), out);
      console.error(
        `${pkg.name}@${version}: the six platform packages as optional dependencies, ${size(packed.size)}`,
      );
      tarballs.push(packed.file);
    }
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
  return tarballs;
}

function main(argv) {
  let options;
  try {
    ({ values: options } = parseArgs({
      args: argv,
      options: {
        from: { type: 'string' },
        local: { type: 'boolean' },
        binary: { type: 'string' },
        out: { type: 'string' },
        help: { type: 'boolean', short: 'h' },
      },
    }));
  } catch (err) {
    console.error(`${err.message}\n${USAGE}`);
    return 2;
  }
  if (options.help) {
    console.log(USAGE);
    return 0;
  }
  if (options.local ? !options.binary || options.from : !options.from || options.binary) {
    console.error(USAGE);
    return 2;
  }
  const out = path.resolve(
    options.out || path.join(NPM_DIR, 'dist', options.local ? 'local' : 'release'),
  );
  try {
    const tarballs = build({
      from: options.from && path.resolve(options.from),
      local: options.local,
      binary: options.binary && path.resolve(options.binary),
      out,
    });
    for (const tarball of tarballs) {
      console.log(tarball);
    }
    return 0;
  } catch (err) {
    console.error(`build.mjs: ${err.message}`);
    return 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  process.exitCode = main(process.argv.slice(2));
}
