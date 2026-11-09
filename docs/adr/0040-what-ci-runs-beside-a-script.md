# 0040. What CI runs beside a script is that script's check

- **Status:** accepted
- **Date:** 2026-11-01
- **Rules:** C-15, C-22, C-23
- **Amends:** [0037](0037-any-language-from-cis-own-files.md), which left every JavaScript command
  in CI's files to package.json

## Context

We set hushrun up on a real Bun backend, and what it promised didn't happen:

- init wrapped `"test": "bun test"`, and left `run: bun test` in both of CI's workflows. It printed
  `workflows none run these checks`, so CI ran every test on every push.
- `hushrun ready` said the code was ready, the hook said `CI skips test`, and the block init wrote
  in `AGENTS.md` promised the same. None of it was true.
- Typing `bun test` ran everything again, with no line from hushrun. It is Bun's own runner, which
  never reads package.json, so only `bun run test` reaches the wrapped script.

`bun test` is how Bun's documentation runs tests, so this is the normal Bun repository, not an edge
case. ADR 0037 let package.json own JavaScript because CI's `npm test` starts the wrapped script,
and wrapping it again would run hushrun twice. That holds for a package manager's script. It doesn't
hold for a command CI runs itself.

## Decision

1. **`bun test` is a row of the table** that counts whatever the root holds: no script stands in
   for it. So a repository with no `test` script gets it wrapped too.
2. **A command at the root that a wrapped script runs, word for word, is that script's check,**
   whatever the tool. init wraps it in place, under the script's name, so that the script and CI
   have one key (C-9) and a pass of either skips the other.
3. **It keeps that name however many jobs run it.** Two jobs of one command normally take their
   job's name, since they may run on two systems (ADR 0037 item 3). `npm test` in two jobs already
   shares a key, so this adds no new risk, and a job's name here would lose the shared key, which is
   the point. Any other command that wants the name takes its job's in front.
4. **A repository already set up gets these from a second init,** and nothing else of CI's files:
   what its people left out of hushrun stays their choice. Only GitHub's workflows, since a GitLab
   job also needs an install script and a token, which are a person's to set up.
5. **`hushrun ready` says so.** While CI runs such a command itself, a note says which and what to
   do, and the hook prints it in place of `CI skips`.
6. **People are told what to type.** init's last lines name the command that goes through hushrun,
   and the skill and the docs say why `bun test` doesn't.

## Alternatives

- **A Bun preload, in `bunfig.toml`, that sends a bare `bun test` through hushrun.** It would fix
  what people type. But in a preload, `process.argv` is Bun and one test file, whatever the command
  line held: `bun test` and `bun test -t cart` look the same (Bun 1.3.11). A filtered run could then
  stand in for the whole suite, the one mistake hushrun must not make.
- **Rewriting CI's `bun test` to `bun run test`.** It needs no install step, but it is a new kind of
  edit, and it needs a script that a repository may not have. Wrapping in place is the edit init
  already makes.
- **Wrapping every JavaScript tool in CI.** `npx vitest run` in CI and `vitest run` in a script have
  different keys, so this would add hushrun to a job without sharing a single pass.

## Consequences

- `bun test` typed at a prompt still runs everything. The hook and `hushrun ready` name the command
  that doesn't, once it matters.
- Sharing takes the same words: `bun test --bail` in CI and `bun test` in a script stay two checks,
  and `hushrun ready` lists both.
- A repository set up before this release needs `hushrun init` run again.
- A pass on a laptop can now skip a Bun repository's tests in the job a deploy needs. Where that
  isn't wanted, the CI-only setting (C-12) keeps skips to what CI recorded.
