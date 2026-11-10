#!/usr/bin/env node
// Builds the bundle that skills marketplaces read (ADR 0066): hushrun's skill, as
// `skills/hushrun/SKILL.md`, with each tool's manifest at the root, all at the CLI's version. The
// release writes it to the repository marketplaces install from, github.com/hushrun/skills, so
// that nothing there is edited by hand: recipes/agents/ is the one place its text lives.
//
//   node recipes/agents/bundle.mjs --out DIR [--repository OWNER/NAME]
//
// DIR must be empty or not exist yet. The version is the Cargo workspace's. Node 18 or later, and
// no dependencies.
//
// Who reads what:
//   skills/hushrun/SKILL.md          skills.sh (`npx skills add`), `gh skill`, every plugin below
//   plugin.json                      Agent Plugins 1.0: GitHub Copilot, VS Code, Codex, Cursor,
//                                    Kiro, Antigravity
//   .claude-plugin/plugin.json       Claude Code, and Anthropic's directory
//   .claude-plugin/marketplace.json  Claude Code, Copilot CLI, Factory, Devin, Junie
//   .agents/plugins/marketplace.json Codex, which asks for policy and category
//   .cursor-plugin/plugin.json       Cursor's marketplace, whose plugins have their manifest here
//   gemini-extension.json            Gemini CLI, and its gallery
//   assets/icon.png                  Anthropic's directory: the listing's icon, read once, at the
//                                    first submission (hushrun's mark, from the landing's logo)
//   README.md, LICENSE               people, and the directories' checks: Kiro's asks the README
//                                    for a privacy policy and a support contact

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

import { cargoVersion } from '../../npm/build.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.dirname(path.dirname(HERE));

/** The plugin's name everywhere: a lowercase word that every tool's rules take. */
export const NAME = 'hushrun';
/** Where marketplaces install it from, unless --repository says otherwise. */
export const REPOSITORY = 'hushrun/skills';

const DESCRIPTION =
  "Tells coding agents what to run before they push in a repository that uses hushrun, which skips CI checks this exact code already passed, and how to set hushrun up.";
const AUTHOR = { name: 'hushrun', email: 'support@hushrun.com', url: 'https://hushrun.com' };
const HOMEPAGE = 'https://hushrun.com/docs/for-agents';
const SOURCE = 'https://github.com/hushrun/hushrun';
const SUPPORT = `${SOURCE}/issues`;
const PRIVACY = 'https://hushrun.com/privacy';
const TERMS = 'https://hushrun.com/terms';
const LICENSE = 'MIT';
const KEYWORDS = ['ci', 'testing', 'pre-push', 'github-actions', 'gitlab-ci', 'coding-agents', 'skills'];

/** The keys an Agent Plugins 1.0 manifest may have: its schema allows no others. */
export const AGENT_PLUGIN_KEYS = [
  '$schema',
  'name',
  'version',
  'description',
  'author',
  'homepage',
  'repository',
  'license',
  'keywords',
  'extensions',
];
export const AGENT_PLUGIN_SCHEMA = 'https://agent-plugins.org/schemas/1.0.0/plugin.schema.json';

const USAGE = 'usage: node recipes/agents/bundle.mjs --out DIR [--repository OWNER/NAME]';

/** The skill's revision, as its frontmatter says it (metadata.hushrun-skill). */
export function skillRevision(skill) {
  const front = skill.replace(/\r\n/g, '\n').match(/^---\n([\s\S]*?)\n---/);
  const revision = front && front[1].match(/^\s+hushrun-skill:\s*"?(\d+)"?\s*$/m);
  if (!revision) {
    throw new Error('recipes/agents/SKILL.md says no hushrun-skill revision');
  }
  return Number(revision[1]);
}

const json = (value) => `${JSON.stringify(value, null, 2)}\n`;

/** The bundle's files, by path, for `version` and `repository`: text, or bytes for the icon. */
export function files({ version, repository = REPOSITORY, root = ROOT }) {
  if (!/^[A-Za-z0-9-]+\/[A-Za-z0-9._-]+$/.test(repository)) {
    throw new Error(`--repository ${repository} isn't OWNER/NAME`);
  }
  const skill = fs.readFileSync(path.join(root, 'recipes', 'agents', 'SKILL.md'), 'utf8');
  const revision = skillRevision(skill);
  const license = fs.readFileSync(path.join(root, 'LICENSE.md'), 'utf8');
  const icon = fs.readFileSync(path.join(root, 'recipes', 'agents', 'icon.png'));
  const common = {
    name: NAME,
    version,
    description: DESCRIPTION,
    author: AUTHOR,
    homepage: HOMEPAGE,
    repository: `https://github.com/${repository}`,
    license: LICENSE,
    keywords: KEYWORDS,
  };
  return {
    'skills/hushrun/SKILL.md': skill,
    'plugin.json': json({ $schema: AGENT_PLUGIN_SCHEMA, ...common }),
    // The version is here only: Claude Code reads plugin.json's before a marketplace entry's,
    // and refuses neither, but `claude plugin validate` reports the two when both are set.
    '.claude-plugin/plugin.json': json({
      ...common,
      displayName: 'hushrun',
      icon: './assets/icon.png',
      documentationUrl: HOMEPAGE,
      supportUrl: SUPPORT,
      privacyPolicyUrl: PRIVACY,
      termsOfServiceUrl: TERMS,
    }),
    '.claude-plugin/marketplace.json': json({
      name: NAME,
      description: "hushrun's skill for coding agents, from hushrun's own releases.",
      owner: { name: AUTHOR.name, email: AUTHOR.email },
      plugins: [{ name: NAME, source: './', description: DESCRIPTION }],
    }),
    '.agents/plugins/marketplace.json': json({
      name: NAME,
      interface: { displayName: 'hushrun' },
      plugins: [
        {
          name: NAME,
          source: { source: 'local', path: './' },
          policy: { installation: 'AVAILABLE', authentication: 'ON_INSTALL' },
          category: 'Developer Tools',
        },
      ],
    }),
    // Cursor's own format: its author takes a name and an email, and its logo a path.
    '.cursor-plugin/plugin.json': json({
      name: NAME,
      version,
      description: DESCRIPTION,
      author: { name: AUTHOR.name, email: AUTHOR.email },
      homepage: HOMEPAGE,
      repository: `https://github.com/${repository}`,
      license: LICENSE,
      keywords: KEYWORDS,
      logo: 'assets/icon.png',
    }),
    'gemini-extension.json': json({ name: NAME, version, description: DESCRIPTION }),
    'assets/icon.png': icon,
    'README.md': readme({ version, revision, repository }),
    LICENSE: license,
  };
}

function readme({ version, revision, repository }) {
  return `# hushrun's skill for coding agents

hushrun skips CI checks that this exact code already passed. This skill tells a coding agent what
to run before it pushes in a repository that uses hushrun, and how to set hushrun up. The rules
that change from one hushrun to the next come from the binary: the skill has the agent run
\`hushrun guide\`, which also says whether hushrun and the skill are current.

hushrun's release writes this repository from [hushrun/hushrun](${SOURCE}), where the text lives
in \`recipes/agents/\`. Don't edit it here: a change belongs there.

## Install

| Where | How |
|---|---|
| Any agent, through skills.sh | \`npx skills add ${repository}\` |
| Claude Code | \`/plugin marketplace add ${repository}\`, then \`/plugin install hushrun@hushrun\` |
| Codex | \`codex plugin marketplace add ${repository}\` |
| GitHub Copilot CLI | \`copilot plugin marketplace add ${repository}\` |
| Gemini CLI | \`gemini extensions install https://github.com/${repository}\` |
| GitHub CLI | \`gh skill install ${repository} hushrun\` |
| One repository | \`npx hushrun init\` writes the skill into it, for every clone |
| Your home, from hushrun | \`hushrun skill update --global\` |

## Support and privacy

- Support: [${SUPPORT}](${SUPPORT}), or ${AUTHOR.email}
- Privacy policy: [${PRIVACY}](${PRIVACY})
- Terms: [${TERMS}](${TERMS})

The skill sends nothing itself. The hushrun CLI it runs hashes files on the machine and never sends
them; once a person signs in, it records check results with hushrun's service, and about once a day
it asks GitHub whether a newer release is out (\`HUSHRUN_NO_UPDATE_CHECK=1\` turns that off).

## Updates

Each hushrun release updates this repository, tagged as the release is, and each tool updates
its copy with its own command: \`npx skills update\`, \`claude plugin update hushrun@hushrun\`,
\`codex plugin marketplace upgrade\`, \`copilot plugin update hushrun\`, \`gemini extensions update
hushrun\` or \`gh skill update\`. A copy that is out of date still works: hushrun says so, in
\`hushrun guide\` and \`hushrun ready\`.

This is hushrun ${version}, skill revision ${revision}. MIT licensed: see LICENSE.
`;
}

/** Writes the bundle into `out`, which must be empty or not exist yet. The paths written. */
export function write({ out, version, repository }) {
  if (fs.existsSync(out) && fs.readdirSync(out).length > 0) {
    throw new Error(`${out} isn't empty`);
  }
  const written = [];
  for (const [relative, text] of Object.entries(files({ version, repository }))) {
    const file = path.join(out, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
    written.push(relative);
  }
  return written;
}

export function main(argv) {
  let options;
  try {
    ({ values: options } = parseArgs({
      args: argv,
      options: { out: { type: 'string' }, repository: { type: 'string' } },
    }));
  } catch (err) {
    console.error(`bundle.mjs: ${err.message}\n${USAGE}`);
    return 2;
  }
  if (!options.out) {
    console.error(USAGE);
    return 2;
  }
  try {
    const written = write({
      out: path.resolve(options.out),
      version: cargoVersion(ROOT),
      repository: options.repository || REPOSITORY,
    });
    for (const file of written) {
      console.log(file);
    }
    return 0;
  } catch (err) {
    console.error(`bundle.mjs: ${err.message}`);
    return 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  process.exitCode = main(process.argv.slice(2));
}
