// The bundle skills marketplaces read (ADR 0066): run with `node --test recipes/agents/*.test.mjs`.

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { cargoVersion } from '../../npm/build.mjs';
import {
  AGENT_PLUGIN_KEYS,
  AGENT_PLUGIN_SCHEMA,
  NAME,
  REPOSITORY,
  files,
  skillRevision,
} from './bundle.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.dirname(path.dirname(HERE));
const VERSION = cargoVersion(ROOT);
const SKILL = fs.readFileSync(path.join(HERE, 'SKILL.md'), 'utf8');

function scratch(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'hushrun-bundle-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function bundle(...args) {
  return spawnSync(process.execPath, [path.join(HERE, 'bundle.mjs'), ...args], {
    encoding: 'utf8',
  });
}

const read = (dir, relative) => fs.readFileSync(path.join(dir, relative), 'utf8');
const json = (dir, relative) => JSON.parse(read(dir, relative));

test('the bundle carries the skill as hushrun wrote it, and every manifest', (t) => {
  const out = path.join(scratch(t), 'out');
  const result = bundle('--out', out);
  assert.equal(result.status, 0, result.stderr);
  const listed = result.stdout.trim().split('\n').sort();
  assert.deepEqual(listed, Object.keys(files({ version: VERSION })).sort());
  // The skill, byte for byte: its revision tells a copy older or current (ADR 0065).
  assert.equal(read(out, 'skills/hushrun/SKILL.md'), SKILL);
  assert.equal(read(out, 'LICENSE'), read(ROOT, 'LICENSE.md'));
  assert.deepEqual(
    fs.readFileSync(path.join(out, 'assets/icon.png')),
    fs.readFileSync(path.join(HERE, 'icon.png')),
  );
});

test("every manifest says the CLI's version, and the marketplaces none of their own", () => {
  const bundled = files({ version: VERSION });
  const parse = (relative) => JSON.parse(bundled[relative]);
  for (const relative of ['plugin.json', '.claude-plugin/plugin.json', 'gemini-extension.json']) {
    assert.equal(parse(relative).version, VERSION, relative);
    assert.equal(parse(relative).name, NAME, relative);
  }
  // Claude Code takes plugin.json's version over an entry's, and validate reports both.
  for (const relative of ['.claude-plugin/marketplace.json', '.agents/plugins/marketplace.json']) {
    const marketplace = parse(relative);
    assert.equal(marketplace.name, NAME, relative);
    assert.equal(marketplace.plugins.length, 1, relative);
    assert.equal(marketplace.plugins[0].name, NAME, relative);
    assert.equal(marketplace.plugins[0].version, undefined, relative);
  }
  assert.equal(parse('.claude-plugin/marketplace.json').plugins[0].source, './');
  // Codex asks every entry for its policy and category.
  const codex = parse('.agents/plugins/marketplace.json').plugins[0];
  assert.deepEqual(codex.source, { source: 'local', path: './' });
  assert.equal(codex.policy.installation, 'AVAILABLE');
  assert.ok(codex.category);
});

test("the Agent Plugins manifest has only its schema's keys", () => {
  const manifest = JSON.parse(files({ version: VERSION })['plugin.json']);
  assert.equal(manifest.$schema, AGENT_PLUGIN_SCHEMA);
  for (const key of Object.keys(manifest)) {
    assert.ok(AGENT_PLUGIN_KEYS.includes(key), key);
  }
  assert.match(manifest.name, /^(?!.*(?:--|\.\.))[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/);
  assert.deepEqual(Object.keys(manifest.author).sort(), ['email', 'name', 'url']);
});

test("Claude's manifest passes what Anthropic's directory checks first", () => {
  const bundled = files({ version: VERSION });
  const manifest = JSON.parse(bundled['.claude-plugin/plugin.json']);
  // Not presented as Anthropic's own, and kebab-case.
  assert.doesNotMatch(manifest.name, /^(claude|anthropic|anthropics|cc-plugin)(-|$)/);
  assert.match(manifest.name, /^[a-z0-9]+(-[a-z0-9]+)*$/);
  assert.equal(manifest.license, 'MIT');
  for (const key of ['homepage', 'documentationUrl', 'supportUrl', 'privacyPolicyUrl', 'termsOfServiceUrl']) {
    assert.match(manifest[key], /^https:\/\//, key);
  }
  assert.equal(manifest.icon, './assets/icon.png');
  assert.ok(bundled['assets/icon.png'].length < 2 * 1024 * 1024);
  // A README of at least 40 words, and no credential in any file.
  const words = bundled['README.md'].split(/\s+/).filter(Boolean).length;
  assert.ok(words >= 40, `${words} words`);
  for (const [relative, text] of Object.entries(bundled)) {
    if (typeof text !== 'string') continue;
    assert.doesNotMatch(text, /\b(hra|hru)_[A-Za-z0-9]{8,}|hushrun_agent_[A-Za-z0-9]{8,}/, relative);
  }
});

test('the README says how to install and update it, from where it is published', () => {
  const readme = files({ version: VERSION })['README.md'];
  assert.match(readme, new RegExp(`npx skills add ${REPOSITORY}`));
  assert.match(readme, /hushrun skill update --global/);
  assert.match(readme, new RegExp(`This is hushrun ${VERSION.replace(/\./g, '\\.')}, skill revision ${skillRevision(SKILL)}\\.`));
  assert.doesNotMatch(readme, /\$\{|undefined/);
  const elsewhere = files({ version: VERSION, repository: 'acme/agent-skills' })['README.md'];
  assert.match(elsewhere, /npx skills add acme\/agent-skills/);
});

test('it writes only into an empty directory, and the same bytes every time', (t) => {
  const dir = scratch(t);
  const once = path.join(dir, 'once');
  const twice = path.join(dir, 'twice');
  assert.equal(bundle('--out', once).status, 0);
  assert.equal(bundle('--out', twice).status, 0);
  for (const relative of Object.keys(files({ version: VERSION }))) {
    assert.deepEqual(fs.readFileSync(path.join(once, relative)), fs.readFileSync(path.join(twice, relative)), relative);
  }
  const again = bundle('--out', once);
  assert.equal(again.status, 1);
  assert.match(again.stderr, /isn't empty/);
});

test('usage errors exit 2', () => {
  assert.equal(bundle().status, 2);
  assert.equal(bundle('--nope').status, 2);
  const bad = bundle('--out', path.join(os.tmpdir(), 'unused'), '--repository', 'no slash');
  assert.equal(bad.status, 1);
  assert.match(bad.stderr, /isn't OWNER\/NAME/);
});

test('a skill without a revision is refused', () => {
  assert.throws(() => skillRevision('---\nname: hushrun\n---\n'), /no hushrun-skill revision/);
  assert.equal(skillRevision(SKILL.replace(/\n/g, '\r\n')), skillRevision(SKILL));
});
