# 0030. The last "ledger" settings are renamed, and no old name is kept

- **Status:** accepted; amended by [0031](0031-membership-is-trust.md) on 2026-10-23:
  `org.yaml` and `hushrun repository inventory`, whose names it settled, are gone
- **Amended by:** [0062](0062-no-stages.md)
- **Date:** 2026-10-22
- **Rules:** C-3, C-7, T-4, T-7, T-9, T-13, L-5, L-10, O-1, O-4, §11.3; ADR 0029
- **Amends:** ADR 0029 (its environment variable's name, and its decisions 2 to 4 on old names)

## Context

ADR 0029 renamed the settings that name a repository's URL, and kept the old names as hidden,
warning aliases for one release. Three things were left:

- `HUSHRUN_REPOSITORY` holds a URL, but reads like `GITHUB_REPOSITORY`, which is `owner/name`,
  and doesn't match `--repository-url` and `repository_url:`.
- Other keys people type still said `ledger`: org.yaml's `ledger_base:` and a repository entry's
  `ledger:` and `ledger_log:`, trust's `ledger_log:`, a stage's `on_ledger_error`, and the cloud
  admin's `hushrun-cloud reindex --ledger`.
- Nothing has been released with any of these names, so the aliases protect no one, and cost
  code, tests, a deprecated schema entry and an `init` rewrite.

## Decision

1. **Final names.**

   | Before | Now |
   | --- | --- |
   | `HUSHRUN_REPOSITORY` (ADR 0029; `HUSHRUN_LEDGER` before it) | `HUSHRUN_REPOSITORY_URL` |
   | GitHub variable `HUSHRUN_REPOSITORY` | `HUSHRUN_REPOSITORY_URL` |
   | `HUSHRUN_REPOSITORY_URL` and `HUSHRUN_LEDGER_URL`, set for a token command | `HUSHRUN_REPOSITORY_URL` |
   | org.yaml `ledger_base:` | `repository_base:` |
   | org.yaml repository entry `ledger:` | `repository_url:` |
   | `ledger_log:` (a `trust:` section, trust.yaml, an org.yaml entry) | `transparency_log:` |
   | a stage's `on_ledger_error` (hushrun.yaml, and trust's `constraints`) | `on_repository_error` |
   | `hushrun-cloud reindex --ledger <name>` | `--repository <name>` |

   Unchanged from ADR 0029: `repository_url:`, `--repository-url`, `--repository-base`,
   `witness serve --log-url`, `HUSHRUN_REPOSITORY_BASE`, `HUSHRUN_REPOSITORY_TOKEN`,
   `HUSHRUN_REPOSITORY_TOKEN_COMMAND`, and the reusable workflow's `repository-url` and
   `repository-base` inputs.

   - The variable that holds the URL now says `_URL`, like the flag and the key. A token command
     is given that same URL under the same name, which is why one name serves both.
   - `repository_base:` and `repository_url:` in org.yaml match `HUSHRUN_REPOSITORY_BASE` and
     hushrun.yaml's `repository_url:`.
   - `transparency_log:` says what the block is: the transparency log's origin, key, witnesses
     and quorum (T-9, T-13), as the docs, ADR 0017 and `witness serve --log` call it. It is not a
     URL, so it takes no `_url`, and `log:` alone would read as logging.
   - `on_repository_error` keeps the word ADR 0029 chose for what hushrun keeps per repository on
     the server, which is what a stage fails or runs `full` without (L-5).

2. **No aliases.** Neither ADR 0029's old names nor these get one. The old keys are unknown
   keys, which every file hushrun parses already refuses with `file:line:column` (C-8, O-1); the
   old flags are unknown arguments (exit 2); the old variables are ignored like any other
   unknown variable. Removed with them: the warning hints and conflict checks, hushrun.yaml's
   deprecated `ledger` key and its schema entry, `init`'s rename of an old `ledger:` line, and
   the reusable workflow's passthrough of the `ledger` and `ledger-base` inputs and the
   `HUSHRUN_LEDGER*` variables. ADR 0029's decisions 2 (the rename by `init`), 3 and 4 no longer
   hold, and its consequence "the next release removes the old names" happened before a release.

3. **Unchanged:** internal identifiers and Rust types, the HTTP API's paths and JSON fields, the
   database, JSON output keys, doctor check names, and the `hushrun ledger` admin subcommand.
   hushrun.yaml's deprecated `ledger_root` (ADR 0026 item 13) keeps its name: it is read only
   by the migration from signed trust, and goes with that migration.

4. **Messages name trust by its source.** `policy validate`, `doctor` and the other messages
   that named `trust.yaml` whatever trust they read now say `hushrun.yaml's trust: section` when
   it came from the section, or the trust file's name.

## Consequences

- People type one word, and one spelling per setting, in CI settings, org.yaml and hushrun.yaml.
- A setup written against an unreleased build with the old names stops working: unknown keys
  fail to parse, so those show at once; an old variable is ignored, so a CI job reports that it
  has no repository URL, naming `HUSHRUN_REPOSITORY_URL`.
- The `hushrun ledger` subcommand is the last place the word shows to people. It is an admin
  tool (`gc`, `monitor`, `inventory`) that acts on the server's store, and its JSON schemas are
  named for it; renaming it to `hushrun repository` is open.

## Amendment 1: `hushrun repository` (2026-10-22)

The open rename was decided: `hushrun ledger` is `hushrun repository`, with no alias, for
the reasons of decision 2. Decision 3 changes accordingly.

- `hushrun repository ls | show | inventory | gc | monitor`; `hushrun ledger` is an unknown
  subcommand (exit 2).
- Their JSON outputs are `hushrun.repository.ls/v1`, `.show/v1`, `.inventory/v1`, `.gc/v1` and
  `.monitor/v1`, with the schemas `repository-gc`, `repository-inventory` and
  `repository-monitor`; the monitor's state file is `hushrun.repository.monitor.state/v1`.
- `doctor`'s checks are `server` (was `ledger`), `server protocol`, `repository url`,
  `transparency log` and `witnesses` (were `ledger protocol`, `ledger url`, `ledger log` and
  `ledger witnesses`); `doctor --org`'s `ledger` check is `server`.
- Messages, help and the recipes say "server" (what keeps the events and the log) or "repository
  URL" where they said "ledger".
- Still unchanged: internal identifiers, crate names, the HTTP API's paths and JSON fields, the
  database, and the JSON output keys (`ledger` in `plan`, `adopt`, `org apply`, `doctor --org`
  and these outputs).
