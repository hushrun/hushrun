# 0029. The settings that name a repository's URL say "repository"

- **Status:** accepted; amended by ADR 0030 (2026-10-22): `HUSHRUN_REPOSITORY` is
  `HUSHRUN_REPOSITORY_URL`, and decisions 2 (`init`'s rename), 3 and 4 (the old names) are
  withdrawn: nothing was released, so no alias is kept
- **Date:** 2026-10-22
- **Rules:** C-3, T-4, L-10, L-12, §11.3; ADR 0016 (the reusable workflow's inputs), ADR 0021
  (the token command's `HUSHRUN_LEDGER_URL`)
- **Amends:** the names in C-3, T-4 and §11.3; nothing they mean

## Context

People read "repository" for what hushrun keeps per repository: its events, results and log on
the server. The CLI's and the cloud's messages already say so. The settings people type still said
`ledger`: hushrun.yaml's `ledger:`, `--ledger`, `HUSHRUN_LEDGER` and its family. Those are the
names people meet first, in CI settings and in the files `init` and `adopt` write, and they are
the last place the internal word shows.

Two names were taken:

- hushrun.yaml's `repository:` is the repository's id, such as `github.com/acme/shop` (C-2).
- `--repository` is the repository's id on `adopt`, `token create` and `org`, and a global flag
  can't share a name with a subcommand's.

## Decision

1. **New names.**

   | Old | New |
   | --- | --- |
   | hushrun.yaml `ledger:` | `repository_url:` |
   | `--ledger` (global) | `--repository-url` |
   | `adopt --ledger-base` | `--repository-base` |
   | `witness serve --ledger-url` | `--log-url` (the global `--repository-url` holds the name; it pairs with `--log`) |
   | `HUSHRUN_LEDGER` | `HUSHRUN_REPOSITORY` |
   | `HUSHRUN_LEDGER_BASE` | `HUSHRUN_REPOSITORY_BASE` |
   | `HUSHRUN_LEDGER_TOKEN` | `HUSHRUN_REPOSITORY_TOKEN` |
   | `HUSHRUN_LEDGER_TOKEN_COMMAND` | `HUSHRUN_REPOSITORY_TOKEN_COMMAND` |
   | `HUSHRUN_LEDGER_URL` (set for a token command) | `HUSHRUN_REPOSITORY_URL` |
   | reusable workflow inputs `ledger`, `ledger-base` | `repository-url`, `repository-base` |
   | GitHub variables `HUSHRUN_LEDGER`, `HUSHRUN_LEDGER_BASE` | `HUSHRUN_REPOSITORY`, `HUSHRUN_REPOSITORY_BASE` |

   The URL key and flag say `_url` because the bare word is the id. The environment variables
   keep the shorter family that was asked for; their values are checked as URLs, so one set to
   an id fails with a message.

2. **Everything hushrun writes uses the new names only**: `init`, `adopt`, `org apply`, the
   recipes, the reusable workflow's callers, the cloud's adoption pull requests, templates and
   pages, and the docs. When `init` adds a section to an existing hushrun.yaml, it also renames an
   old `ledger:` line.

3. **The old names work for one release.** They are hidden from `--help` and the schema marks the
   key deprecated. Each one in use prints one line on stderr naming the new one, once per
   process; `-q` silences it. An old and a new name both given:
   - with the same value (URLs compared normalized): the new one, and a line saying to remove the
     old;
   - with different values: exit 2 (for hushrun.yaml, an error at the old key's line).

   A token command gets both `HUSHRUN_REPOSITORY_URL` and `HUSHRUN_LEDGER_URL`.

4. **The reusable workflow** takes the new inputs and passes the old inputs and variables through
   under the old environment names, so a caller written before this release, or an organization
   whose variables still have the old names, keeps working, and hushrun itself prints the hint
   and catches a conflict.

5. **Unchanged:** internal identifiers and Rust types, the HTTP API's paths and JSON fields, the
   database, the `hushrun ledger` admin subcommand, JSON output keys, doctor check names,
   `org.yaml`'s `ledger_base`, `ledger` and `ledger_log`, hushrun.yaml's `ledger_log`,
   `on_ledger_error` and the deprecated `ledger_root`.

## Consequences

- People see one word, in what they type and what hushrun writes.
- An organization should rename its GitHub variables (or GitLab CI/CD variables) within the
  release; until then every run prints the hint.
- The next release removes the old names: `ledger:` becomes an unknown key, the old flags
  unknown arguments, and the old variables are ignored. `doctor` could warn before then.

## Amendment (ADR 0030, 2026-10-22)

- **Final names.** `HUSHRUN_REPOSITORY` and the GitHub variable of that name are
  `HUSHRUN_REPOSITORY_URL`, like `--repository-url` and `repository_url:`; a token command gets
  the URL under that same name, and no longer `HUSHRUN_LEDGER_URL`. The rest of decision 1
  stands. ADR 0030 also renames what item 5 left: org.yaml's `ledger_base:` (`repository_base:`),
  an entry's `ledger:` (`repository_url:`), `ledger_log:` (`transparency_log:`) and
  `on_ledger_error` (`on_repository_error`).
- **No aliases.** Nothing was released with the old names, so decisions 3 and 4 are withdrawn
  before a release: the old keys are unknown, the old flags unknown arguments, the old variables
  ignored, and the reusable workflow passes nothing through. `init` no longer renames a
  `ledger:` line (decision 2's last sentence), and the schema has no deprecated `ledger` key.
